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
    /// The request framing is invalid (bad or conflicting length, bad chunked
    /// encoding, unparsable head).
    MalformedRequest,
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
            CodecError::MalformedRequest => write!(f, "malformed request framing"),
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
            if let Some(result) = self.try_parse()? {
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
    fn try_parse(&mut self) -> Result<Option<ReadResult>, CodecError> {
        let data = &self.buf[..self.pos];
        let Some(header_end) = find_subsequence(data, b"\r\n\r\n") else {
            return Ok(None);
        };
        let headers_section = &data[..header_end];

        let (method, uri, headers) =
            parse_request_head(headers_section).map_err(|_error| CodecError::MalformedRequest)?;
        let headers_end = header_end + 4;

        let chunked = parse_transfer_encoding(&headers)?;
        let content_lengths: Vec<&str> = headers
            .iter()
            .filter(|(name, _)| name == "content-length")
            .map(|(_, value)| value.as_str())
            .collect();

        // Conflicting framing (any TE plus CL, or several differing CLs) is
        // rejected rather than resolved by a default.
        if chunked && !content_lengths.is_empty() {
            return Err(CodecError::MalformedRequest);
        }

        let (body, total_needed) = if chunked {
            match decode_chunked_body(&data[headers_end..]) {
                Ok(Some((decoded, consumed))) => (decoded, headers_end + consumed),
                Ok(None) => return Ok(None),
                Err(_) => return Err(CodecError::MalformedRequest),
            }
        } else {
            let body_len = match content_lengths.as_slice() {
                [] => 0,
                [length] => parse_content_length(length)?,
                [first, rest @ ..] => {
                    let first = parse_content_length(first)?;
                    if rest
                        .iter()
                        .any(|other| parse_content_length(other).ok() != Some(first))
                    {
                        return Err(CodecError::MalformedRequest);
                    }
                    first
                }
            };

            let body_end = headers_end
                .checked_add(body_len)
                .ok_or(CodecError::MalformedRequest)?;
            if body_len > READ_BUF_SIZE.saturating_sub(headers_end) {
                return Err(CodecError::RequestTooLarge);
            }
            if self.pos < body_end {
                return Ok(None);
            }

            let body = if body_len > 0 {
                data[headers_end..body_end].to_vec()
            } else {
                vec![]
            };
            (body, body_end)
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

        Ok(Some(ReadResult::Request(request)))
    }
}

/// Parses every `Transfer-Encoding` field as an HTTP token list. This edge
/// implements only a single, final `chunked` coding; substring matches,
/// repeated chunked codings, non-final chunked, and unsupported chains are all
/// malformed framing rather than requests with an assumed body length.
fn parse_transfer_encoding(headers: &[(String, String)]) -> Result<bool, CodecError> {
    let mut codings = Vec::new();
    for (_, value) in headers
        .iter()
        .filter(|(name, _)| name == "transfer-encoding")
    {
        for coding in value.split(',') {
            let coding = coding.trim();
            if coding.is_empty() || !coding.bytes().all(is_http_token_byte) {
                return Err(CodecError::MalformedRequest);
            }
            codings.push(coding.to_ascii_lowercase());
        }
    }

    match codings.as_slice() {
        [] => Ok(false),
        [coding] if coding == "chunked" => Ok(true),
        _ => Err(CodecError::MalformedRequest),
    }
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Parses a `Content-Length` value strictly: digits only, no sign, no
/// whitespace, and representable in `usize`.
fn parse_content_length(raw: &str) -> Result<usize, CodecError> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CodecError::MalformedRequest);
    }
    raw.parse::<usize>()
        .map_err(|_error| CodecError::MalformedRequest)
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
/// Returns `Ok(None)` while the buffer holds an incomplete chunk sequence,
/// `Err(())` for a malformed sequence, and the decoded body plus the number of
/// consumed bytes (through the terminating zero chunk's complete trailer
/// section) on success. All trailer fields are consumed so the following
/// request boundary is preserved.
fn decode_chunked_body(data: &[u8]) -> Result<Option<(Vec<u8>, usize)>, ()> {
    let mut body = Vec::new();
    let mut offset = 0usize;

    loop {
        let Some(line_len) = find_subsequence(&data[offset..], b"\r\n") else {
            return Ok(None);
        };
        let line_end = offset + line_len;
        let size_line = std::str::from_utf8(&data[offset..line_end]).map_err(|_error| ())?;
        // Chunk extensions (`;name=value`) are permitted and ignored.
        let size_token = size_line.split(';').next().unwrap_or_default().trim();
        if size_token.is_empty() || !size_token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(());
        }
        let size = usize::from_str_radix(size_token, 16).map_err(|_error| ())?;
        offset = line_end + 2;

        if size == 0 {
            // Consume every trailer field up to and including the blank line.
            loop {
                let Some(trailer_len) = find_subsequence(&data[offset..], b"\r\n") else {
                    return Ok(None);
                };
                let trailer_end = offset + trailer_len;
                let line = &data[offset..trailer_end];
                offset = trailer_end + 2;
                if line.is_empty() {
                    return Ok(Some((body, offset)));
                }
            }
        }

        let Some(chunk_end) = offset.checked_add(size) else {
            return Err(());
        };
        let Some(terminator_end) = chunk_end.checked_add(2) else {
            return Err(());
        };
        if terminator_end > data.len() {
            return Ok(None);
        }
        if &data[chunk_end..terminator_end] != b"\r\n" {
            return Err(());
        }
        body.extend_from_slice(&data[offset..chunk_end]);
        offset = terminator_end;
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

    // scenario: http-connector/malformed-http-framing-is-rejected/invalid-content-length
    #[tokio::test]
    async fn malformed_and_conflicting_lengths_are_rejected() {
        for raw in [
            b"POST / HTTP/1.1\r\nhost: x\r\ncontent-length: nope\r\n\r\n".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ncontent-length: -1\r\n\r\n".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ncontent-length: 3\r\ncontent-length: 4\r\n\r\nabcd".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ncontent-length: 3\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n".as_slice(),
        ] {
            let mut codec = HttpCodec::new();
            let mut stream = raw;
            assert!(
                matches!(
                    codec.read_request(&mut stream).await,
                    Err(CodecError::MalformedRequest)
                ),
                "invalid framing must be rejected: {:?}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[tokio::test]
    async fn transfer_encoding_requires_one_final_chunked_token() {
        for raw in [
            b"POST / HTTP/1.1\r\nhost: x\r\ntransfer-encoding: xchunked\r\n\r\n".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked, gzip\r\n\r\n".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ntransfer-encoding: gzip, chunked\r\n\r\n".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked, chunked\r\n\r\n".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked\r\ntransfer-encoding: chunked\r\n\r\n".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked,\r\n\r\n".as_slice(),
            b"POST / HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chu nked\r\n\r\n".as_slice(),
        ] {
            let mut codec = HttpCodec::new();
            let mut stream = raw;
            assert!(
                matches!(
                    codec.read_request(&mut stream).await,
                    Err(CodecError::MalformedRequest)
                ),
                "invalid transfer coding must be rejected: {:?}",
                String::from_utf8_lossy(raw)
            );
        }

        let raw = b"POST / HTTP/1.1\r\nhost: x\r\ntransfer-encoding: Chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        let mut codec = HttpCodec::new();
        let mut stream = &raw[..];
        match codec
            .read_request(&mut stream)
            .await
            .expect("valid chunked")
        {
            ReadResult::Request(request) => assert_eq!(request.body, b"abc"),
            other => panic!("expected request, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_content_length_is_rejected_before_buffering() {
        let raw = format!(
            "POST / HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n",
            READ_BUF_SIZE + 1
        );
        let mut codec = HttpCodec::new();
        let mut stream = raw.as_bytes();
        assert!(matches!(
            codec.read_request(&mut stream).await,
            Err(CodecError::RequestTooLarge)
        ));
    }

    // scenario: http-connector/malformed-http-framing-is-rejected/chunked-trailers-terminate-correctly
    #[tokio::test]
    async fn multiple_trailers_preserve_the_following_request_boundary() {
        let data = b"POST /first HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\nx-a: 1\r\nx-b: 2\r\n\r\nGET /second HTTP/1.1\r\nhost: x\r\n\r\n";
        let mut codec = HttpCodec::new();
        let mut stream = &data[..];
        match codec
            .read_request(&mut stream)
            .await
            .expect("first request")
        {
            ReadResult::Request(request) => {
                assert_eq!(request.uri, "/first");
                assert_eq!(request.body, b"abc");
            }
            other => panic!("expected first request, got {other:?}"),
        }
        match codec
            .read_request(&mut stream)
            .await
            .expect("second request")
        {
            ReadResult::Request(request) => assert_eq!(request.uri, "/second"),
            other => panic!("expected second request, got {other:?}"),
        }
    }

    #[test]
    fn decode_chunked_incomplete_returns_none() {
        let data = b"5\r\nhel";
        assert!(matches!(decode_chunked_body(data), Ok(None)));
    }

    #[test]
    fn decode_chunked_bad_terminator_is_malformed() {
        let data = b"3\r\nabcXX";
        assert!(matches!(decode_chunked_body(data), Err(())));
    }
}
