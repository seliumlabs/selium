use anyhow::Result;
use futures::StreamExt;
use serde::Deserialize;
use tracing::{debug, error};

use selium_userland::{
    entrypoint,
    net::{Connection, HttpListener},
};

const HEADER_END: &[u8] = b"\r\n\r\n";
const HTTP_VERSION: &str = "HTTP/1.1";
const JSON_CONTENT_TYPE: &str = "application/json";
const PASSWORD_EXPECTED: &str = "It's an illusion, Michael!";
const PASSWORD_EXPECTED_ALT: &str = "Its an illusion, Michael!";
const STATUS_OK_BODY: &str = r#"{"status":true}"#;
const STATUS_FAIL_BODY: &str = r#"{"status":false}"#;
const STATUS_BAD_REQUEST_BODY: &str = r#"{"error":"invalid request"}"#;

#[derive(Deserialize)]
struct Request {
    password: String,
}

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
    response.extend_from_slice(JSON_CONTENT_TYPE.as_bytes());
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
    let headers = std::str::from_utf8(&payload[..header_end])
        .map_err(|_| "invalid header encoding")?;
    let content_length = parse_content_length(headers)?;
    let body_start = header_end + HEADER_END.len();
    let body = payload
        .get(body_start..)
        .ok_or("request body missing")?;
    match content_length {
        Some(expected) => body
            .get(..expected)
            .ok_or("request body incomplete"),
        None => Ok(body),
    }
}

fn response_for_payload(payload: &[u8]) -> Vec<u8> {
    let body = match body_from_request(payload) {
        Ok(body) if !body.is_empty() => body,
        _ => {
            return build_response(400, "Bad Request", STATUS_BAD_REQUEST_BODY);
        }
    };

    let request: Request = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => {
            return build_response(400, "Bad Request", STATUS_BAD_REQUEST_BODY);
        }
    };

    let matched = request.password == PASSWORD_EXPECTED || request.password == PASSWORD_EXPECTED_ALT;
    let response_body = if matched {
        STATUS_OK_BODY
    } else {
        STATUS_FAIL_BODY
    };

    build_response(200, "OK", response_body)
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

#[entrypoint]
async fn http_server(domain: &str, port: u16) -> Result<()> {
    let listener = HttpListener::bind(domain, port).await?;
    listener
        .incoming()
        .for_each(|result| async move {
            match result {
                Ok(conn) => handle_connection(conn).await,
                Err(err) => {
                    error!(error = ?err, "failed to accept connection");
                }
            }
        })
        .await;

    Ok(())
}
