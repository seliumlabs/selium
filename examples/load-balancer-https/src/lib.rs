use anyhow::{Context as AnyhowContext, Result};
use futures::{StreamExt, TryStreamExt};
use selium_atlas::{Atlas, Uri};
use selium_switchboard::{Fanout, Subscriber, Switchboard, SwitchboardError};
use selium_userland::{
    Context, entrypoint,
    net::{Connection, HttpsListener, TlsServerBundle, TlsServerConfig},
};
use tracing::{debug, error};

/// Max number of simultaneous HTTPS requests.
const CONCURRENT_REQUESTS: usize = 50;
/// Internal URI that HTTPS requests are published on.
const LB_URI: &str = "sel://example.org/web/prod/api";
const TLS_CERT_PEM: &[u8] = include_bytes!("../server-cert.pem");
const TLS_KEY_PEM: &[u8] = include_bytes!("../server-key.pem");

#[entrypoint]
async fn load_balancer(ctx: Context, domain: &str, port: u16) -> Result<()> {
    let switchboard = ctx.require::<Switchboard>().await;
    let atlas = ctx.require::<Atlas>().await;

    let lb: Fanout<Connection> = Fanout::create(&switchboard).await?;
    atlas
        .insert(Uri::parse(LB_URI).unwrap(), lb.endpoint_id() as u64)
        .await?;

    let tls_bundle = TlsServerBundle {
        cert_chain_pem: TLS_CERT_PEM.to_vec(),
        private_key_pem: TLS_KEY_PEM.to_vec(),
        client_ca_pem: None,
        alpn: Some(vec!["http/1.1".to_string()]),
        require_client_auth: false,
    };
    let tls_config = TlsServerConfig::register(tls_bundle)
        .await
        .context("failed to register TLS config")?;
    let listener = HttpsListener::bind_with_tls(domain, port, &tls_config).await?;
    listener
        .incoming()
        .and_then(|mut conn| async move {
            conn.prepare_for_transfer().await?;
            Ok(conn)
        })
        .map_err(SwitchboardError::Driver)
        .forward(lb)
        .await?;

    Ok(())
}

#[entrypoint]
async fn conn_handler(ctx: Context) -> Result<()> {
    let switchboard = ctx.require::<Switchboard>().await;
    let atlas = ctx.require::<Atlas>().await;

    let requests: Subscriber<Connection> = Subscriber::create(&switchboard).await?;
    let lb = atlas
        .get(&Uri::parse(LB_URI).unwrap())
        .await?
        .context("load balancer endpoint not found")?;
    requests.connect(&switchboard, lb as u32).await?;

    requests
        .for_each_concurrent(CONCURRENT_REQUESTS, |result| async move {
            match result {
                Ok(conn) => handle_connection(conn).await,
                Err(err) => {
                    error!(error = ?err, "failed to receive connection");
                }
            }
        })
        .await;

    Ok(())
}

// Note: Protocol-level logic is temporary

const HEADER_END: &[u8] = b"\r\n\r\n";
const HTTP_VERSION: &str = "HTTP/1.1";
const CONTENT_TYPE: &str = "text/plain; charset=utf-8";
const STATUS_OK_BODY: &str = "ok";
const STATUS_BAD_REQUEST_BODY: &str = "bad request";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestReadState {
    Incomplete,
    Complete,
    Invalid,
}

fn build_response(status_code: u16, reason: &str, body: &str) -> Vec<u8> {
    let mut response = Vec::with_capacity(body.len() + 128);
    response.extend_from_slice(HTTP_VERSION.as_bytes());
    response.extend_from_slice(b" ");
    response.extend_from_slice(status_code.to_string().as_bytes());
    response.extend_from_slice(b" ");
    response.extend_from_slice(reason.as_bytes());
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(b"Content-Type: ");
    response.extend_from_slice(CONTENT_TYPE.as_bytes());
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(b"Content-Length: ");
    response.extend_from_slice(body.len().to_string().as_bytes());
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(b"Connection: close\r\n");
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(body.as_bytes());
    response
}

fn header_end_index(payload: &[u8]) -> Option<usize> {
    payload
        .windows(HEADER_END.len())
        .position(|window| window == HEADER_END)
}

fn parse_content_length(headers: &str) -> Result<Option<usize>, &'static str> {
    for line in headers.split("\r\n") {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let value = value
                .trim()
                .parse::<usize>()
                .map_err(|_| "invalid content-length")?;
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn request_read_state(payload: &[u8]) -> RequestReadState {
    let Some(header_end) = header_end_index(payload) else {
        return RequestReadState::Incomplete;
    };
    let headers = match std::str::from_utf8(&payload[..header_end]) {
        Ok(headers) => headers,
        Err(_) => return RequestReadState::Invalid,
    };
    let content_length = match parse_content_length(headers) {
        Ok(value) => value,
        Err(_) => return RequestReadState::Invalid,
    };
    let body_len = payload.len().saturating_sub(header_end + HEADER_END.len());
    match content_length {
        Some(expected) if body_len >= expected => RequestReadState::Complete,
        Some(_) => RequestReadState::Incomplete,
        None => RequestReadState::Complete,
    }
}

fn body_from_request(payload: &[u8]) -> Result<&[u8], &'static str> {
    let header_end = header_end_index(payload).ok_or("request missing header terminator")?;
    let headers =
        std::str::from_utf8(&payload[..header_end]).map_err(|_| "invalid header encoding")?;
    let content_length = parse_content_length(headers)?;
    let body_start = header_end + HEADER_END.len();
    let body = payload.get(body_start..).ok_or("request body missing")?;
    match content_length {
        Some(expected) => body.get(..expected).ok_or("request body incomplete"),
        None => Ok(body),
    }
}

fn response_for_payload(payload: &[u8]) -> Vec<u8> {
    match body_from_request(payload) {
        Ok(body) if !body.is_empty() => build_response(200, "OK", &String::from_utf8_lossy(body)),
        Ok(_) => build_response(200, "OK", STATUS_OK_BODY),
        Err(_) => build_response(400, "Bad Request", STATUS_BAD_REQUEST_BODY),
    }
}

async fn handle_connection(mut conn: Connection) {
    let mut request_bytes = Vec::new();
    let mut response_override = None;

    loop {
        match conn.recv().await {
            Ok(Some(frame)) => {
                debug!(len = frame.payload.len(), "received raw frame");
                request_bytes.extend_from_slice(&frame.payload);
            }
            Ok(None) => break,
            Err(err) => {
                error!(error = ?err, "connection handler error");
                return;
            }
        }

        match request_read_state(&request_bytes) {
            RequestReadState::Incomplete => {}
            RequestReadState::Complete => break,
            RequestReadState::Invalid => {
                response_override =
                    Some(build_response(400, "Bad Request", STATUS_BAD_REQUEST_BODY));
                break;
            }
        }
    }

    let response = response_override.unwrap_or_else(|| response_for_payload(&request_bytes));
    if let Err(err) = conn.send(response).await {
        error!(error = ?err, "failed to send response");
    }
}
