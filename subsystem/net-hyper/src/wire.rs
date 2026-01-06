//! HTTP wire helpers for the Hyper driver.

use hyper::{
    Body, Request, Response, StatusCode, Uri, Version,
    header::{CONTENT_LENGTH, HOST, TRANSFER_ENCODING},
    http::{HeaderName, HeaderValue, Method},
};
use selium_abi::NetProtocol;

use crate::driver::HyperError;

const MAX_HEADERS: usize = 64;
const HTTP1_VERSION: &str = "HTTP/1.1";
const HTTP2_VERSION: &str = "HTTP/2";
const HTTP2_VERSION_ALT: &str = "HTTP/2.0";

pub(crate) async fn format_request_bytes(
    mut request: Request<Body>,
    protocol: NetProtocol,
) -> Result<Vec<u8>, HyperError> {
    let body = hyper::body::to_bytes(request.body_mut())
        .await
        .map_err(HyperError::Hyper)?;
    let mut buf = Vec::with_capacity(body.len() + 256);
    let version = version_label(protocol)?;

    let path = request
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    buf.extend_from_slice(request.method().as_str().as_bytes());
    buf.extend_from_slice(b" ");
    buf.extend_from_slice(path.as_bytes());
    buf.extend_from_slice(b" ");
    buf.extend_from_slice(version.as_bytes());
    buf.extend_from_slice(b"\r\n");

    let mut has_content_length = false;
    for (name, value) in request.headers().iter() {
        if name == TRANSFER_ENCODING {
            continue;
        }
        if name == CONTENT_LENGTH {
            has_content_length = true;
        }
        buf.extend_from_slice(name.as_str().as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(value.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    if !has_content_length {
        let len = body.len().to_string();
        buf.extend_from_slice(CONTENT_LENGTH.as_str().as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(len.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    buf.extend_from_slice(b"\r\n");
    buf.extend_from_slice(&body);
    Ok(buf)
}

pub(crate) async fn format_response_bytes(
    response: Response<Body>,
    protocol: NetProtocol,
) -> Result<Vec<u8>, HyperError> {
    let (parts, body) = response.into_parts();
    let body = hyper::body::to_bytes(body)
        .await
        .map_err(HyperError::Hyper)?;
    let mut buf = Vec::with_capacity(body.len() + 256);
    let version = version_label(protocol)?;

    let status = parts.status;
    let reason = status.canonical_reason().unwrap_or("");

    buf.extend_from_slice(version.as_bytes());
    buf.extend_from_slice(b" ");
    buf.extend_from_slice(status.as_str().as_bytes());
    if !reason.is_empty() {
        buf.extend_from_slice(b" ");
        buf.extend_from_slice(reason.as_bytes());
    }
    buf.extend_from_slice(b"\r\n");

    let mut has_content_length = false;
    for (name, value) in parts.headers.iter() {
        if name == TRANSFER_ENCODING {
            continue;
        }
        if name == CONTENT_LENGTH {
            has_content_length = true;
        }
        buf.extend_from_slice(name.as_str().as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(value.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    if !has_content_length {
        let len = body.len().to_string();
        buf.extend_from_slice(CONTENT_LENGTH.as_str().as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(len.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    buf.extend_from_slice(b"\r\n");
    buf.extend_from_slice(&body);
    Ok(buf)
}

pub(crate) fn parse_request(
    protocol: NetProtocol,
    domain: &str,
    port: u16,
    bytes: &[u8],
) -> Result<Request<Body>, HyperError> {
    let (head, body) = split_message(bytes)?;
    let header_text = std::str::from_utf8(head)
        .map_err(|_| HyperError::HttpParse("invalid header encoding".to_string()))?;
    let mut lines = header_text.split("\r\n");
    let start_line = lines.next().ok_or(HyperError::HttpIncomplete)?;
    let (method, path, version) = parse_request_line(start_line)?;
    ensure_http_version(protocol, version)?;

    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).map_err(HyperError::InvalidMethod)?)
        .version(protocol_version(protocol)?)
        .uri(build_request_uri(protocol, domain, port, path)?);

    let mut content_length = None;
    let mut host_header = None;
    let mut header_count = 0;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        header_count += 1;
        if header_count > MAX_HEADERS {
            return Err(HyperError::HttpParse("too many headers".to_string()));
        }

        let (name_raw, value_raw) = line
            .split_once(':')
            .ok_or_else(|| HyperError::HttpParse("malformed header".to_string()))?;
        let name = HeaderName::from_bytes(name_raw.trim().as_bytes())
            .map_err(HyperError::InvalidHeaderName)?;
        let value =
            HeaderValue::from_str(value_raw.trim()).map_err(HyperError::InvalidHeaderValue)?;

        if name == TRANSFER_ENCODING && is_chunked(value_raw.as_bytes())? {
            return Err(HyperError::TransferEncoding);
        }
        if name == CONTENT_LENGTH {
            let len = parse_content_length(value_raw.as_bytes())?;
            if let Some(existing) = content_length {
                if existing != len {
                    return Err(HyperError::ContentLengthMismatch {
                        expected: existing,
                        actual: len,
                    });
                }
            } else {
                content_length = Some(len);
            }
        }
        if name == HOST {
            host_header = Some(value_raw.trim().to_string());
        }

        builder = builder.header(name, value);
    }

    if let Some(host) = host_header.as_deref() {
        if !host_matches(domain, host) {
            return Err(HyperError::HostMismatch);
        }
    }

    if host_header.is_none() {
        let host = host_header_value(domain, port, protocol)?;
        builder = builder.header(HOST, host);
    }

    let body = if let Some(len) = content_length {
        if body.len() < len {
            return Err(HyperError::HttpIncomplete);
        }
        if body.len() > len {
            return Err(HyperError::ContentLengthMismatch {
                expected: len,
                actual: body.len(),
            });
        }
        body
    } else {
        body
    };

    builder
        .body(Body::from(body.to_vec()))
        .map_err(HyperError::Http)
}

pub(crate) fn parse_response(
    protocol: NetProtocol,
    bytes: &[u8],
) -> Result<Response<Body>, HyperError> {
    let (head, body) = split_message(bytes)?;
    let header_text = std::str::from_utf8(head)
        .map_err(|_| HyperError::HttpParse("invalid header encoding".to_string()))?;
    let mut lines = header_text.split("\r\n");
    let start_line = lines.next().ok_or(HyperError::HttpIncomplete)?;
    let (version, status) = parse_response_line(start_line)?;
    ensure_http_version(protocol, version)?;

    let mut builder = Response::builder()
        .status(status)
        .version(protocol_version(protocol)?);
    let mut content_length = None;
    let mut header_count = 0;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        header_count += 1;
        if header_count > MAX_HEADERS {
            return Err(HyperError::HttpParse("too many headers".to_string()));
        }

        let (name_raw, value_raw) = line
            .split_once(':')
            .ok_or_else(|| HyperError::HttpParse("malformed header".to_string()))?;
        let name = HeaderName::from_bytes(name_raw.trim().as_bytes())
            .map_err(HyperError::InvalidHeaderName)?;
        let value =
            HeaderValue::from_str(value_raw.trim()).map_err(HyperError::InvalidHeaderValue)?;

        if name == TRANSFER_ENCODING && is_chunked(value_raw.as_bytes())? {
            return Err(HyperError::TransferEncoding);
        }
        if name == CONTENT_LENGTH {
            let len = parse_content_length(value_raw.as_bytes())?;
            if let Some(existing) = content_length {
                if existing != len {
                    return Err(HyperError::ContentLengthMismatch {
                        expected: existing,
                        actual: len,
                    });
                }
            } else {
                content_length = Some(len);
            }
        }

        builder = builder.header(name, value);
    }

    let body = if let Some(len) = content_length {
        if body.len() < len {
            return Err(HyperError::HttpIncomplete);
        }
        if body.len() > len {
            return Err(HyperError::ContentLengthMismatch {
                expected: len,
                actual: body.len(),
            });
        }
        body
    } else {
        body
    };

    builder
        .body(Body::from(body.to_vec()))
        .map_err(HyperError::Http)
}

pub(crate) fn error_response(
    protocol: NetProtocol,
    status: StatusCode,
    message: &str,
) -> Response<Body> {
    let mut response = Response::new(Body::from(message.to_string()));
    *response.status_mut() = status;
    *response.version_mut() = protocol_version(protocol).unwrap_or(Version::HTTP_2);
    match HeaderValue::from_str(&message.len().to_string()) {
        Ok(value) => {
            response.headers_mut().insert(CONTENT_LENGTH, value);
        }
        Err(err) => {
            tracing::debug!(err = %err, "failed to set content-length on error response");
        }
    }
    response
}

pub(crate) fn host_matches(domain: &str, host: &str) -> bool {
    let host = if let Some(stripped) = host.strip_prefix('[') {
        stripped
            .split_once(']')
            .map(|(left, _)| left)
            .unwrap_or(stripped)
    } else {
        host.split_once(':').map(|(left, _)| left).unwrap_or(host)
    };
    host.eq_ignore_ascii_case(domain)
}

fn split_message(bytes: &[u8]) -> Result<(&[u8], &[u8]), HyperError> {
    let mut index = None;
    for (offset, window) in bytes.windows(4).enumerate() {
        if window == b"\r\n\r\n" {
            index = Some(offset);
            break;
        }
    }

    let index = index.ok_or(HyperError::HttpIncomplete)?;
    let head = bytes.get(..index).ok_or(HyperError::HttpIncomplete)?;
    let body = bytes.get(index + 4..).ok_or(HyperError::HttpIncomplete)?;
    Ok((head, body))
}

fn parse_request_line(line: &str) -> Result<(&str, &str, &str), HyperError> {
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or(HyperError::HttpIncomplete)?;
    let path = parts.next().ok_or(HyperError::HttpIncomplete)?;
    let version = parts.next().ok_or(HyperError::HttpIncomplete)?;
    if parts.next().is_some() {
        return Err(HyperError::HttpParse("invalid request line".to_string()));
    }
    Ok((method, path, version))
}

fn parse_response_line(line: &str) -> Result<(&str, StatusCode), HyperError> {
    let mut parts = line.split_whitespace();
    let version = parts.next().ok_or(HyperError::HttpIncomplete)?;
    let code = parts.next().ok_or(HyperError::HttpIncomplete)?;
    let code = code.parse::<u16>().map_err(|_| HyperError::InvalidStatus)?;
    let status = StatusCode::from_u16(code).map_err(|_| HyperError::InvalidStatus)?;
    Ok((version, status))
}

fn ensure_http_version(protocol: NetProtocol, version: &str) -> Result<(), HyperError> {
    match protocol {
        NetProtocol::Http => {
            if version.eq_ignore_ascii_case(HTTP1_VERSION) {
                Ok(())
            } else {
                Err(HyperError::HttpParse(
                    "unsupported HTTP version".to_string(),
                ))
            }
        }
        NetProtocol::Https => {
            if version.eq_ignore_ascii_case(HTTP2_VERSION)
                || version.eq_ignore_ascii_case(HTTP2_VERSION_ALT)
            {
                Ok(())
            } else {
                Err(HyperError::HttpParse(
                    "unsupported HTTP version".to_string(),
                ))
            }
        }
        _ => Err(HyperError::UnsupportedProtocol { protocol }),
    }
}

fn protocol_version(protocol: NetProtocol) -> Result<Version, HyperError> {
    match protocol {
        NetProtocol::Http => Ok(Version::HTTP_11),
        NetProtocol::Https => Ok(Version::HTTP_2),
        _ => Err(HyperError::UnsupportedProtocol { protocol }),
    }
}

fn version_label(protocol: NetProtocol) -> Result<&'static str, HyperError> {
    match protocol {
        NetProtocol::Http => Ok(HTTP1_VERSION),
        NetProtocol::Https => Ok(HTTP2_VERSION),
        _ => Err(HyperError::UnsupportedProtocol { protocol }),
    }
}

fn normalise_path(raw: &str) -> Result<String, HyperError> {
    let trimmed = if raw.is_empty() { "/" } else { raw };
    if let Ok(uri) = trimmed.parse::<Uri>() {
        if uri.scheme().is_some() {
            if let Some(path) = uri.path_and_query() {
                return Ok(path.as_str().to_string());
            }
            return Ok("/".to_string());
        }
    }

    if trimmed.starts_with('/') {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("/{trimmed}"))
    }
}

fn build_request_uri(
    protocol: NetProtocol,
    domain: &str,
    port: u16,
    path: &str,
) -> Result<Uri, HyperError> {
    let scheme = match protocol {
        NetProtocol::Http => "http",
        NetProtocol::Https => "https",
        _ => return Err(HyperError::UnsupportedProtocol { protocol }),
    };
    let authority = authority_string(domain, port, protocol);
    let path = normalise_path(path)?;
    let uri = format!("{scheme}://{authority}{path}");
    uri.parse::<Uri>().map_err(HyperError::InvalidUri)
}

fn parse_content_length(value: &[u8]) -> Result<usize, HyperError> {
    let text = parse_header_string(value)?;
    text.parse::<usize>()
        .map_err(|_| HyperError::HttpParse("invalid content-length".to_string()))
}

fn parse_header_string(value: &[u8]) -> Result<String, HyperError> {
    std::str::from_utf8(value)
        .map(|s| s.trim().to_string())
        .map_err(|_| HyperError::HttpParse("invalid header encoding".to_string()))
}

fn is_chunked(value: &[u8]) -> Result<bool, HyperError> {
    let text = parse_header_string(value)?;
    Ok(text
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("chunked")))
}

fn host_header_value(
    domain: &str,
    port: u16,
    protocol: NetProtocol,
) -> Result<HeaderValue, HyperError> {
    let host = authority_string(domain, port, protocol);
    HeaderValue::from_str(&host).map_err(HyperError::InvalidHeaderValue)
}

fn authority_string(domain: &str, port: u16, protocol: NetProtocol) -> String {
    let default_port = match protocol {
        NetProtocol::Http => 80,
        NetProtocol::Https => 443,
        _ => port,
    };
    let base = if domain.contains(':') && !domain.starts_with('[') {
        format!("[{domain}]")
    } else {
        domain.to_string()
    };
    if port == default_port {
        base
    } else {
        format!("{base}:{port}")
    }
}
