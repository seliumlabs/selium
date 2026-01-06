//! Client-side HTTP helpers for the Hyper driver.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use selium_abi::{IoFrame, NetProtocol};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::{
    driver::{HyperError, HyperStream, OutboundSender, OutboundState},
    tls::server_name,
    wire::{format_response_bytes, parse_request},
};

pub(crate) async fn connect_stream(
    protocol: NetProtocol,
    domain: &str,
    port: u16,
    client_config: Arc<rustls::ClientConfig>,
) -> Result<HyperStream, HyperError> {
    let trimmed = domain.trim_start_matches('[').trim_end_matches(']');
    let stream = match trimmed.parse::<IpAddr>() {
        Ok(ip) => TcpStream::connect(SocketAddr::new(ip, port))
            .await
            .map_err(HyperError::Connect)?,
        Err(_) => TcpStream::connect((trimmed, port))
            .await
            .map_err(HyperError::Connect)?,
    };
    match protocol {
        NetProtocol::Http => Ok(Box::new(stream)),
        NetProtocol::Https => {
            let server_name = server_name(domain)?;
            let connector = TlsConnector::from(client_config);
            let tls_stream = connector
                .connect(server_name, stream)
                .await
                .map_err(HyperError::Tls)?;
            Ok(Box::new(tls_stream))
        }
        _ => Err(HyperError::UnsupportedProtocol { protocol }),
    }
}

pub(crate) async fn read_outbound(
    state: &OutboundState,
    len: usize,
) -> Result<IoFrame, HyperError> {
    loop {
        {
            let mut guard = state.response.lock().await;
            if !guard.is_empty() {
                let take = len.min(guard.len());
                let payload: Vec<u8> = guard.drain(..take).collect();
                return Ok(IoFrame {
                    writer_id: 0,
                    payload,
                });
            }

            if state.closed.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(IoFrame {
                    writer_id: 0,
                    payload: Vec::new(),
                });
            }
        }

        state.response_notify.notified().await;
    }
}

pub(crate) async fn write_outbound(state: &OutboundState, bytes: &[u8]) -> Result<(), HyperError> {
    if bytes.is_empty() {
        return Ok(());
    }

    let request = parse_request(state.protocol, &state.domain, state.port, bytes)?;
    let response = {
        let mut guard = state.sender.lock().await;
        send_request(&mut guard, request).await?
    };

    let response_bytes = format_response_bytes(response, state.protocol).await?;
    {
        let mut guard = state.response.lock().await;
        guard.extend(response_bytes);
    }
    state.response_notify.notify_waiters();
    Ok(())
}

async fn send_request(
    sender: &mut OutboundSender,
    request: hyper::Request<hyper::Body>,
) -> Result<hyper::Response<hyper::Body>, HyperError> {
    match sender {
        OutboundSender::Http1(sender) => sender
            .send_request(request)
            .await
            .map_err(HyperError::Hyper),
        OutboundSender::Http2(sender) => sender
            .send_request(request)
            .await
            .map_err(HyperError::Hyper),
    }
}
