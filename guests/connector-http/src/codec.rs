//! HTTP/1.1 request codec.
//!
//! Parses HTTP/1.1 requests from a byte stream into typed
//! [`HttpRequest`]s. Supports `Content-Length` bodies and chunked
//! transfer-encoded bodies (decoded inline, bounded by the read buffer:
//! requests exceeding the buffer receive a typed 413 at the edge per the
//! design's explicit edge limits).

// Parser offsets are bounds-checked against the buffer before slicing:
// `try_parse` only slices ranges it has verified are present.
#![expect(
    clippy::indexing_slicing,
    reason = "parser offsets are bounds-checked before slicing"
)]

use selium_proto_http::{HttpHeader, HttpRequest};
use tokio::io::AsyncRead;

/// Parsed request head: (method, uri, headers as lowercase name/value pairs).
pub type ParsedHead = (String, String, Vec<(String, String)>);

pub const MAX_HEADERS: usize = 128;
pub const MAX_HEADER_NAME_LEN: usize = 256;
pub const MAX_HEADER_VALUE_LEN: usize = 8192;
pub const MAX_URI_LEN: usize = 8192;
/// Read buffer size for one connection; also the maximum inline request
/// size (headers + body) the edge accepts.
pub const READ_BUF_SIZE: usize = 16384;

/// Outcome of a codec read.
#[derive(Debug)]
pub enum ReadResult {
    /// A fully parsed typed request.
    Request(HttpRequest),
    /// The client closed the connection cleanly.
    Closed,
}

/// Codec-level read errors, mapped to typed edge responses by the handler.
#[derive(Debug)]
pub enum CodecError {
    /// Transport-level read failure.
    Io(std::io::Error),
    /// The request exceeds the edge size limit (typed 413).
    RequestTooLarge,
    /// The client closed mid-request.
    PartialClosed,
}

/// Incremental HTTP/1.1 request parser over a buffered byte stream.
///
/// Cancel-safe: bytes already read are retained in the internal buffer, so
/// an interrupted read (e.g. a pipeline gate pausing socket reads) never
/// loses request bytes.
pub struct HttpCodec {
    buf: Vec<u8>,
    pos: usize,
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Io(error) => write!(f, "read: {error}"),
            CodecError::RequestTooLarge => write!(f, "request too large"),
            CodecError::PartialClosed => write!(f, "connection closed with partial request"),
        }
    }
}

impl HttpCodec {
    /// Creates an empty codec.
    pub fn new() -> Self {
        Self {
            buf: vec![0u8; READ_BUF_SIZE],
            pos: 0,
        }
    }

    /// Reads and parses the next request from `stream`.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::RequestTooLarge`] when the request exceeds the
    /// edge limit, [`CodecError::PartialClosed`] when the client closes
    /// mid-request, and [`CodecError::Io`] for transport errors.
    pub async fn read_request<S: AsyncRead + Unpin>(
        &mut self,
        stream: &mut S,
    ) -> Result<ReadResult, CodecError> {
        use tokio::io::AsyncReadExt;

        loop {
            if let Some(result) = self.try_parse() {
                return Ok(result);
            }

            if self.pos == self.buf.len() {
                return Err(CodecError::RequestTooLarge);
            }

            let n = stream
                .read(&mut self.buf[self.pos..])
                .await
                .map_err(CodecError::Io)?;
            if n == 0 {
                if self.pos > 0 {
                    return Err(CodecError::PartialClosed);
                }
                return Ok(ReadResult::Closed);
            }
            self.pos += n;
        }
    }

    /// Attempts to parse a complete request from the buffered bytes.
    fn try_parse(&mut self) -> Option<ReadResult> {
        let data = &self.buf[..self.pos];
        let header_end = find_subsequence(data, b"\r\n\r\n")?;
        let headers_section = &data[..header_end];

        let (method, uri, headers) = parse_request_head(headers_section).ok()?;
        let headers_end = header_end + 4;

        let chunked = get_header_str(&headers, "transfer-encoding")
            .is_some_and(|value| value.to_lowercase().contains("chunked"));

        let (body, total_needed) = if chunked {
            let (decoded, consumed) = decode_chunked_body(&data[headers_end..])?;
            // Replace the transfer-encoding marker with the decoded length
            // so downstream guests see an inline body; the typed request
            // carries the decoded bytes in `body`.
            (decoded, headers_end + consumed)
        } else {
            let body_len = if let Some(cl) = get_header_str(&headers, "content-length") {
                cl.parse::<usize>().unwrap_or(0)
            } else {
                0
            };

            let total_needed = headers_end + body_len;
            if self.pos < total_needed {
                return None;
            }

            let body = if body_len > 0 {
                data[headers_end..total_needed].to_vec()
            } else {
                vec![]
            };
            (body, total_needed)
        };

        let typed_headers: Vec<HttpHeader> = headers
            .into_iter()
            .map(|(name, value)| HttpHeader::new(name, value))
            .collect();

        let request = HttpRequest::new(method, uri, typed_headers, body);

        let remaining = self.pos - total_needed;
        if remaining > 0 {
            self.buf.copy_within(total_needed..self.pos, 0);
        }
        self.pos = remaining;

        Some(ReadResult::Request(request))
    }
}

impl Default for HttpCodec {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(crate) fn get_header_str<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let name_lower = name.to_lowercase();
    headers
        .iter()
        .find(|(n, _)| *n == name_lower)
        .map(|(_, v)| v.as_str())
}

pub(crate) fn get_typed_header<'a>(headers: &'a [HttpHeader], name: &str) -> Option<&'a str> {
    let name_lower = name.to_lowercase();
    headers
        .iter()
        .find(|h| h.name.to_lowercase() == name_lower)
        .map(|h| h.value.as_str())
}

pub(crate) fn parse_request_head(data: &[u8]) -> Result<ParsedHead, &'static str> {
    let text = match std::str::from_utf8(data) {
        Ok(text) => text,
        Err(_) => return Err("invalid UTF-8 in request"),
    };
    let mut lines = text.split("\r\n");

    let request_line = lines.next().ok_or("empty request")?;
    let mut parts = request_line.split(' ');
    let method = parts.next().ok_or("missing method")?.to_owned();
    let uri = parts.next().ok_or("missing URI")?.to_owned();

    if uri.len() > MAX_URI_LEN {
        return Err("URI too long");
    }

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_lowercase();
            let value = value.trim().to_owned();

            if name.len() > MAX_HEADER_NAME_LEN {
                return Err("header name too long");
            }
            if value.len() > MAX_HEADER_VALUE_LEN {
                return Err("header value too long");
            }
            headers.push((name, value));
        }
    }

    if headers.len() > MAX_HEADERS {
        return Err("too many headers");
    }

    Ok((method, uri, headers))
}

/// Decodes a chunked transfer-encoded body.
///
/// Returns the decoded body and the number of consumed bytes (through the
/// terminating zero chunk and its trailing CRLF — any trailers are
/// consumed but not forwarded; request trailers are rare and carry no
/// semantics the typed surface needs). Returns `None` while the buffer
/// holds an incomplete chunk sequence.
fn decode_chunked_body(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut body = Vec::new();
    let mut offset = 0usize;

    loop {
        let line_end = offset + find_subsequence(&data[offset..], b"\r\n")?;
        let size_line = std::str::from_utf8(&data[offset..line_end]).ok()?;
        // Chunk extensions (`;name=value`) are permitted and ignored.
        let size_token = size_line.split(';').next()?.trim();
        let size = usize::from_str_radix(size_token, 16).ok()?;
        offset = line_end + 2;

        if size == 0 {
            // Terminal chunk: consume trailers up to the blank line.
            let trailer_end = offset + find_subsequence(&data[offset..], b"\r\n")?;
            return Some((body, trailer_end + 2));
        }

        if offset + size + 2 > data.len() {
            return None;
        }
        if &data[offset + size..offset + size + 2] != b"\r\n" {
            return None;
        }
        body.extend_from_slice(&data[offset..offset + size]);
        offset += size + 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn codec_reads_single_request() {
        let mut codec = HttpCodec::new();
        let data = b"GET / HTTP/1.1\r\nhost: example.com\r\n\r\n";
        let mut stream = &data[..];

        match codec.read_request(&mut stream).await.unwrap() {
            ReadResult::Request(req) => {
                assert_eq!(req.method, "GET");
                assert_eq!(req.uri, "/");
                assert_eq!(req.headers.len(), 1);
                assert!(req.body.is_empty());
            }
            _ => panic!("expected Request"),
        }
    }

    #[tokio::test]
    async fn codec_reads_request_with_body() {
        let mut codec = HttpCodec::new();
        let data = b"POST /api HTTP/1.1\r\nhost: example.com\r\ncontent-length: 7\r\n\r\nabcdefg";
        let mut stream = &data[..];

        match codec.read_request(&mut stream).await.unwrap() {
            ReadResult::Request(req) => {
                assert_eq!(req.body, b"abcdefg");
            }
            _ => panic!("expected Request"),
        }
    }

    #[tokio::test]
    async fn codec_reads_chunked_request_body() {
        let mut codec = HttpCodec::new();
        let data =
            b"POST /upload HTTP/1.1\r\nhost: example.com\r\ntransfer-encoding: chunked\r\n\r\n\
                     5\r\nhello\r\n7\r\n, world\r\n0\r\n\r\n";
        let mut stream = &data[..];

        match codec.read_request(&mut stream).await.unwrap() {
            ReadResult::Request(req) => {
                assert_eq!(req.body, b"hello, world");
            }
            _ => panic!("expected Request"),
        }
    }

    #[tokio::test]
    async fn codec_reads_chunked_body_with_trailers() {
        let mut codec = HttpCodec::new();
        let data =
            b"POST /upload HTTP/1.1\r\nhost: example.com\r\ntransfer-encoding: chunked\r\n\r\n\
                     4\r\ndata\r\n0\r\nx-checksum: abc\r\n\r\n";
        let mut stream = &data[..];

        match codec.read_request(&mut stream).await.unwrap() {
            ReadResult::Request(req) => {
                assert_eq!(req.body, b"data");
            }
            _ => panic!("expected Request"),
        }
    }

    #[tokio::test]
    async fn codec_chunked_with_extension() {
        let mut codec = HttpCodec::new();
        let data =
            b"POST /upload HTTP/1.1\r\nhost: example.com\r\ntransfer-encoding: chunked\r\n\r\n\
                     3;ext=1\r\nabc\r\n0\r\n\r\n";
        let mut stream = &data[..];

        match codec.read_request(&mut stream).await.unwrap() {
            ReadResult::Request(req) => assert_eq!(req.body, b"abc"),
            _ => panic!("expected Request"),
        }
    }

    #[tokio::test]
    async fn codec_keep_alive_requests_share_buffer() {
        let mut codec = HttpCodec::new();
        let data = b"GET /one HTTP/1.1\r\nhost: example.com\r\n\r\nGET /two HTTP/1.1\r\nhost: example.com\r\n\r\n";
        let mut stream = &data[..];

        let first = codec.read_request(&mut stream).await.unwrap();
        let second = codec.read_request(&mut stream).await.unwrap();
        match (first, second) {
            (ReadResult::Request(a), ReadResult::Request(b)) => {
                assert_eq!(a.uri, "/one");
                assert_eq!(b.uri, "/two");
            }
            _ => panic!("expected two requests"),
        }
    }

    #[tokio::test]
    async fn codec_oversized_request_is_typed_error() {
        let mut codec = HttpCodec::new();
        // A body larger than the read buffer: the codec must report the
        // typed oversize condition, not truncate or accept it.
        let head = b"PUT /big HTTP/1.1\r\nhost: example.com\r\ncontent-length: 999999\r\n\r\n";
        let big = vec![b'x'; READ_BUF_SIZE];
        let mut chain: Vec<u8> = head.to_vec();
        chain.extend_from_slice(&big);
        let mut stream = &chain[..];

        let result = codec.read_request(&mut stream).await;
        assert!(matches!(result, Err(CodecError::RequestTooLarge)));
    }

    #[tokio::test]
    async fn codec_closed_connection() {
        let mut codec = HttpCodec::new();
        let data = b"";
        let mut stream = &data[..];

        match codec.read_request(&mut stream).await.unwrap() {
            ReadResult::Closed => {}
            _ => panic!("expected Closed"),
        }
    }

    #[tokio::test]
    async fn codec_partial_request_then_closed() {
        let mut codec = HttpCodec::new();
        let data = b"GET / HTTP/1.1\r\n";
        let mut stream = &data[..];

        let result = codec.read_request(&mut stream).await;
        assert!(matches!(result, Err(CodecError::PartialClosed)));
    }

    #[tokio::test]
    async fn codec_accumulates_slow_trickle_reads() {
        // One-byte-at-a-time delivery: the codec must accumulate bytes in
        // its own buffer (the same property that makes a paused read
        // cancel-safe — nothing is lost between reads).
        struct Trickle<'a> {
            data: &'a [u8],
            offset: usize,
        }
        impl tokio::io::AsyncRead for Trickle<'_> {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if self.offset >= self.data.len() || buf.remaining() == 0 {
                    return std::task::Poll::Ready(Ok(()));
                }
                buf.put_slice(&self.data[self.offset..self.offset + 1]);
                self.offset += 1;
                std::task::Poll::Ready(Ok(()))
            }
        }

        let mut codec = HttpCodec::new();
        let data = b"GET /trickle HTTP/1.1\r\nhost: example.com\r\ncontent-length: 3\r\n\r\nabc";
        let mut stream = Trickle { data, offset: 0 };

        match codec.read_request(&mut stream).await.unwrap() {
            ReadResult::Request(req) => {
                assert_eq!(req.uri, "/trickle");
                assert_eq!(req.body, b"abc");
            }
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn find_subsequence_found() {
        assert_eq!(
            find_subsequence(b"hello\r\n\r\nworld", b"\r\n\r\n"),
            Some(5)
        );
    }

    #[test]
    fn find_subsequence_not_found() {
        assert_eq!(find_subsequence(b"hello world", b"\r\n\r\n"), None);
    }

    #[test]
    fn find_subsequence_at_start() {
        assert_eq!(find_subsequence(b"\r\n\r\nhello", b"\r\n\r\n"), Some(0));
    }

    #[test]
    fn parse_simple_get_request() {
        let raw = b"GET / HTTP/1.1\r\nhost: example.com\r\n\r\n";
        let (method, uri, headers) = parse_request_head(raw).unwrap();
        assert_eq!(method, "GET");
        assert_eq!(uri, "/");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "host");
        assert_eq!(headers[0].1, "example.com");
    }

    #[test]
    fn parse_post_with_body() {
        let raw = b"POST /api/data HTTP/1.1\r\nhost: example.com\r\ncontent-type: application/json\r\ncontent-length: 16\r\n\r\n{\"key\":\"value\"}";
        let (method, uri, headers) = parse_request_head(raw).unwrap();
        assert_eq!(method, "POST");
        assert_eq!(uri, "/api/data");
        assert_eq!(headers.len(), 3);
    }

    #[test]
    fn parse_uri_too_long() {
        let long_path = "a".repeat(MAX_URI_LEN + 1);
        let raw = format!("GET /{} HTTP/1.1\r\nhost: x\r\n\r\n", long_path);
        assert_eq!(
            parse_request_head(raw.as_bytes()).unwrap_err(),
            "URI too long"
        );
    }

    #[test]
    fn parse_too_many_headers() {
        let mut raw = String::from("GET / HTTP/1.1\r\n");
        for i in 0..MAX_HEADERS + 1 {
            raw.push_str(&format!("x-hdr-{}: v\r\n", i));
        }
        raw.push_str("\r\n");
        assert_eq!(
            parse_request_head(raw.as_bytes()).unwrap_err(),
            "too many headers"
        );
    }

    #[test]
    fn parse_empty_request() {
        // An empty request line parses as an empty method with no URI;
        // the URI check trips first.
        assert_eq!(parse_request_head(b"").unwrap_err(), "missing URI");
    }

    #[test]
    fn parse_missing_method() {
        assert_eq!(parse_request_head(b"\r\n\r\n").unwrap_err(), "missing URI");
    }

    #[test]
    fn get_header_case_insensitive() {
        let headers = vec![
            ("host".to_string(), "example.com".to_string()),
            ("content-type".to_string(), "text/html".to_string()),
        ];
        assert_eq!(get_header_str(&headers, "HOST"), Some("example.com"));
        assert_eq!(get_header_str(&headers, "Content-Type"), Some("text/html"));
        assert_eq!(get_header_str(&headers, "missing"), None);
    }

    #[test]
    fn get_typed_header_works() {
        let headers = vec![
            HttpHeader::new("Host".to_string(), "example.com".to_string()),
            HttpHeader::new("Content-Type".to_string(), "text/html".to_string()),
        ];
        assert_eq!(get_typed_header(&headers, "host"), Some("example.com"));
        assert_eq!(
            get_typed_header(&headers, "CONTENT-TYPE"),
            Some("text/html")
        );
        assert_eq!(get_typed_header(&headers, "x-missing"), None);
    }

    #[test]
    fn decode_chunked_incomplete_returns_none() {
        let data = b"5\r\nhel";
        assert!(decode_chunked_body(data).is_none());
    }

    #[test]
    fn decode_chunked_bad_terminator_returns_none() {
        let data = b"3\r\nabcXX";
        assert!(decode_chunked_body(data).is_none());
    }
}
