//! Server-side HTTP helpers for the Hyper driver.

use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use hyper::{
    Response, StatusCode,
    body::Incoming,
    server::conn::{http1, http2},
    service::service_fn,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ServerConfig;
use selium_abi::NetProtocol;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tracing::warn;

use crate::{
    driver::{HyperBody, HyperError, HyperStream, InboundState, PendingRequest},
    wire::{error_response, format_request_bytes, host_matches, parse_response},
};

pub(crate) fn read_inbound(
    state: &InboundState,
    len: usize,
) -> Result<selium_abi::IoFrame, HyperError> {
    let mut guard = state.request.lock().map_err(|_| HyperError::Lock)?;
    if guard.is_empty() {
        return Ok(selium_abi::IoFrame {
            writer_id: 0,
            payload: Vec::new(),
        });
    }

    let take = len.min(guard.len());
    let payload: Vec<u8> = guard.drain(..take).collect();
    Ok(selium_abi::IoFrame {
        writer_id: 0,
        payload,
    })
}

pub(crate) fn write_inbound(state: &InboundState, bytes: &[u8]) -> Result<(), HyperError> {
    let mut guard = state.response.lock().map_err(|_| HyperError::Lock)?;
    guard.extend_from_slice(bytes);
    Ok(())
}

pub(crate) async fn run_listener(
    listener: TcpListener,
    protocol: NetProtocol,
    domain: String,
    server_config: Arc<ServerConfig>,
    sender: mpsc::Sender<PendingRequest>,
) {
    loop {
        let (stream, remote_addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                warn!(err = %err, "HTTP listener accept failed");
                continue;
            }
        };

        let sender = sender.clone();
        let domain = domain.clone();
        let server_config = Arc::clone(&server_config);
        tokio::spawn(async move {
            if let Err(err) =
                serve_connection(stream, remote_addr, protocol, domain, server_config, sender).await
            {
                warn!(err = %err, "HTTP connection handler failed");
            }
        });
    }
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    remote_addr: SocketAddr,
    protocol: NetProtocol,
    domain: String,
    server_config: Arc<ServerConfig>,
    sender: mpsc::Sender<PendingRequest>,
) -> Result<(), HyperError> {
    let io: HyperStream = match protocol {
        NetProtocol::Http => Box::new(stream),
        NetProtocol::Https => {
            let acceptor = TlsAcceptor::from(server_config);
            let tls_stream = acceptor.accept(stream).await.map_err(HyperError::Tls)?;
            Box::new(tls_stream)
        }
        _ => return Err(HyperError::UnsupportedProtocol { protocol }),
    };

    let service = service_fn(move |req| {
        let sender = sender.clone();
        let domain = domain.clone();
        async move {
            let response =
                match handle_incoming_request(protocol, req, &domain, &sender, remote_addr).await {
                    Ok(response) => response,
                    Err(err) => {
                        warn!(err = %err, "HTTP request handling failed");
                        error_response(
                            protocol,
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "request failed",
                        )
                    }
                };
            Ok::<_, Infallible>(response)
        }
    });

    match protocol {
        NetProtocol::Http => {
            let io = TokioIo::new(io);
            http1::Builder::new()
                .serve_connection(io, service)
                .await
                .map_err(HyperError::Hyper)
        }
        NetProtocol::Https => {
            let io = TokioIo::new(io);
            http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await
                .map_err(HyperError::Hyper)
        }
        _ => return Err(HyperError::UnsupportedProtocol { protocol }),
    }
}

async fn handle_incoming_request(
    protocol: NetProtocol,
    request: hyper::Request<Incoming>,
    domain: &str,
    sender: &mpsc::Sender<PendingRequest>,
    remote_addr: SocketAddr,
) -> Result<Response<HyperBody>, HyperError> {
    if !domain.is_empty() {
        let host = request
            .headers()
            .get(hyper::header::HOST)
            .and_then(|value| value.to_str().ok());
        match host {
            Some(host) if host_matches(domain, host) => {}
            Some(_) => {
                return Ok(error_response(
                    protocol,
                    StatusCode::MISDIRECTED_REQUEST,
                    "host mismatch",
                ));
            }
            None => {
                return Ok(error_response(
                    protocol,
                    StatusCode::BAD_REQUEST,
                    "missing host",
                ));
            }
        }
    }

    let request_bytes = format_request_bytes(request, protocol).await?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    sender
        .send(PendingRequest {
            request_bytes,
            responder: tx,
            remote_addr: remote_addr.to_string(),
        })
        .await
        .map_err(|_| HyperError::ListenerClosed)?;

    let response_bytes = rx.await.map_err(|_| HyperError::ResponseChannelClosed)?;
    match parse_response(protocol, &response_bytes) {
        Ok(response) => Ok(response),
        Err(err) => {
            warn!(err = %err, "invalid HTTP response from guest");
            Ok(error_response(
                protocol,
                StatusCode::BAD_GATEWAY,
                "invalid response",
            ))
        }
    }
}
