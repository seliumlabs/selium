//! HTTP/1.1 response serialization for the edge.
//!
//! Unary responses are written with `Content-Length`; streamed responses
//! are written with chunked transfer encoding — head first, then chunks
//! as they arrive from the serving guest, then trailers and the
//! terminating zero chunk. The edge never buffers a whole streamed body.

use selium_proto_http::{HttpHeader, HttpResponse, HttpTrailer};
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Typed 502 response for session/forwarding failures.
pub fn bad_gateway_response() -> HttpResponse {
    HttpResponse::new(
        502,
        vec![HttpHeader::new(
            "content-type".to_string(),
            "text/plain".to_string(),
        )],
        b"Bad Gateway".to_vec(),
    )
}

/// Typed 500 response for transport-level failures.
pub fn internal_error_response() -> HttpResponse {
    HttpResponse::new(
        500,
        vec![HttpHeader::new(
            "content-type".to_string(),
            "text/plain".to_string(),
        )],
        b"Internal Server Error".to_vec(),
    )
}

/// Typed 404-equivalent response for unmatched routes.
pub fn not_found_response() -> HttpResponse {
    HttpResponse::new(
        404,
        vec![HttpHeader::new(
            "content-type".to_string(),
            "text/plain".to_string(),
        )],
        b"Not Found".to_vec(),
    )
}

/// Typed 413 response for requests exceeding the edge size limit.
pub fn payload_too_large_response() -> HttpResponse {
    HttpResponse::new(
        413,
        vec![HttpHeader::new(
            "content-type".to_string(),
            "text/plain".to_string(),
        )],
        b"Payload Too Large".to_vec(),
    )
}

pub fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// Writes one chunk of a chunked response body.
pub async fn write_chunk<S: AsyncWrite + Unpin>(
    stream: &mut S,
    data: &[u8],
) -> std::io::Result<()> {
    let size_line = format!("{:x}\r\n", data.len());
    stream.write_all(size_line.as_bytes()).await?;
    stream.write_all(data).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    Ok(())
}

/// Writes a complete (unary) HTTP/1.1 response.
pub async fn write_response<S: AsyncWrite + Unpin>(
    stream: &mut S,
    response: &HttpResponse,
) -> std::io::Result<()> {
    let status_text = status_reason(response.status);
    let status_line = format!("HTTP/1.1 {} {}\r\n", response.status, status_text);
    stream.write_all(status_line.as_bytes()).await?;

    for header in &response.headers {
        let line = format!("{}: {}\r\n", header.name, header.value);
        stream.write_all(line.as_bytes()).await?;
    }

    if !response.body.is_empty() {
        let cl = format!("Content-Length: {}\r\n", response.body.len());
        stream.write_all(cl.as_bytes()).await?;
    }

    stream.write_all(b"\r\n").await?;

    if !response.body.is_empty() {
        stream.write_all(&response.body).await?;
    }

    stream.flush().await?;
    Ok(())
}

/// Terminates a chunked response: the zero chunk, any trailers, and the
/// final blank line.
pub async fn write_stream_end<S: AsyncWrite + Unpin>(
    stream: &mut S,
    trailers: &[HttpTrailer],
) -> std::io::Result<()> {
    stream.write_all(b"0\r\n").await?;
    for trailer in trailers {
        let line = format!("{}: {}\r\n", trailer.name, trailer.value);
        stream.write_all(line.as_bytes()).await?;
    }
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    Ok(())
}

/// Writes the head of a streamed response: status line and headers, with
/// `Transfer-Encoding: chunked` replacing any body-length framing.
pub async fn write_stream_head<S: AsyncWrite + Unpin>(
    stream: &mut S,
    head: &HttpResponse,
) -> std::io::Result<()> {
    let status_text = status_reason(head.status);
    let status_line = format!("HTTP/1.1 {} {}\r\n", head.status, status_text);
    stream.write_all(status_line.as_bytes()).await?;

    for header in &head.headers {
        // Body-length and pre-existing transfer framings are replaced by
        // chunked encoding on the wire.
        let name = header.name.to_lowercase();
        if name == "content-length" || name == "transfer-encoding" {
            continue;
        }
        let line = format!("{}: {}\r\n", header.name, header.value);
        stream.write_all(line.as_bytes()).await?;
    }
    stream.write_all(b"Transfer-Encoding: chunked\r\n").await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_response_200() {
        let resp = HttpResponse::new(
            200,
            vec![HttpHeader::new(
                "content-type".to_string(),
                "text/plain".to_string(),
            )],
            b"hello".to_vec(),
        );
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).await.unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("HTTP/1.1 200 OK"));
        assert!(out.contains("content-type: text/plain"));
        assert!(out.contains("Content-Length: 5"));
        assert!(out.ends_with("hello"));
    }

    #[tokio::test]
    async fn write_response_no_body() {
        let resp = HttpResponse::new(204, vec![], vec![]);
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).await.unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("HTTP/1.1 204 No Content"));
        assert!(!out.contains("Content-Length"));
    }

    #[tokio::test]
    async fn write_404_response() {
        let mut buf = Vec::new();
        write_response(&mut buf, &not_found_response())
            .await
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("HTTP/1.1 404 Not Found"));
        assert!(out.ends_with("Not Found"));
    }

    #[tokio::test]
    async fn write_500_response() {
        let mut buf = Vec::new();
        write_response(&mut buf, &internal_error_response())
            .await
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("HTTP/1.1 500 Internal Server Error"));
    }

    #[tokio::test]
    async fn write_413_response() {
        let mut buf = Vec::new();
        write_response(&mut buf, &payload_too_large_response())
            .await
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("HTTP/1.1 413 Payload Too Large"));
    }

    #[tokio::test]
    async fn stream_head_uses_chunked_encoding() {
        let head = HttpResponse::new(
            200,
            vec![
                HttpHeader::new("content-type".to_string(), "text/event-stream".to_string()),
                // Must be replaced, not duplicated, by chunked framing.
                HttpHeader::new("content-length".to_string(), "999".to_string()),
            ],
            vec![],
        );
        let mut buf = Vec::new();
        write_stream_head(&mut buf, &head).await.unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("HTTP/1.1 200 OK"));
        assert!(out.contains("content-type: text/event-stream"));
        assert!(out.contains("Transfer-Encoding: chunked"));
        assert!(!out.contains("Content-Length"));
        assert!(out.contains("\r\n\r\n"));
    }

    #[tokio::test]
    async fn chunk_and_end_encoding() {
        let mut buf = Vec::new();
        write_chunk(&mut buf, b"hello").await.unwrap();
        write_chunk(&mut buf, b"world!").await.unwrap();
        write_stream_end(
            &mut buf,
            &[HttpTrailer::new(
                "x-checksum".to_string(),
                "abc".to_string(),
            )],
        )
        .await
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(
            out,
            "5\r\nhello\r\n6\r\nworld!\r\n0\r\nx-checksum: abc\r\n\r\n"
        );
    }

    #[test]
    fn status_reason_known() {
        assert_eq!(status_reason(200), "OK");
        assert_eq!(status_reason(404), "Not Found");
        assert_eq!(status_reason(500), "Internal Server Error");
    }

    #[test]
    fn status_reason_unknown() {
        assert_eq!(status_reason(999), "Unknown");
    }
}
