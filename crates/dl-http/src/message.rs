//! Requests, responses, the response head parser and body framing rules (sans I/O).

use crate::headers::{Headers, is_field_byte, parse_field_line, wire_text};
use crate::{Error, Url};
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// Largest response head (status line plus header fields) accepted, in bytes.
pub const MAX_HEAD_SIZE: usize = 64 * 1024;
/// Largest number of header fields accepted in a response head or trailer section.
pub const MAX_HEADERS: usize = 100;
/// Longest status line accepted, in bytes.
const MAX_STATUS_LINE: usize = 8 * 1024;
/// Empty lines tolerated before a status line.
const MAX_LEADING_EMPTY_LINES: usize = 4;

fn bad(reason: &'static str) -> Error {
    Error::InvalidResponse { reason }
}

/// An HTTP request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`
    Get,
    /// `HEAD`
    Head,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `PATCH`
    Patch,
    /// `DELETE`
    Delete,
    /// `OPTIONS`
    Options,
    /// `TRACE`
    Trace,
}

impl Method {
    /// The method name in upper case.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
        }
    }

    /// Whether repeating the request has the same effect as sending it once
    /// (RFC 9110 section 9.2.2): every method except `POST` and `PATCH`.
    pub fn is_idempotent(self) -> bool {
        !matches!(self, Method::Post | Method::Patch)
    }

    /// Methods whose requests always carry `Content-Length`, even for an empty body.
    fn expects_body(self) -> bool {
        matches!(self, Method::Post | Method::Put | Method::Patch)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Method {
    type Err = Error;

    /// Parses an upper-case method name (method names are case-sensitive).
    fn from_str(s: &str) -> Result<Method, Error> {
        Ok(match s {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "OPTIONS" => Method::Options,
            "TRACE" => Method::Trace,
            _ => {
                return Err(Error::InvalidHeader {
                    reason: "unsupported method",
                });
            }
        })
    }
}

/// A three-digit response status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StatusCode(u16);

impl StatusCode {
    /// 200 OK.
    pub const OK: StatusCode = StatusCode(200);
    /// 204 No Content.
    pub const NO_CONTENT: StatusCode = StatusCode(204);
    /// 304 Not Modified.
    pub const NOT_MODIFIED: StatusCode = StatusCode(304);
    /// 400 Bad Request.
    pub const BAD_REQUEST: StatusCode = StatusCode(400);
    /// 401 Unauthorized.
    pub const UNAUTHORIZED: StatusCode = StatusCode(401);
    /// 404 Not Found.
    pub const NOT_FOUND: StatusCode = StatusCode(404);
    /// 500 Internal Server Error.
    pub const INTERNAL_SERVER_ERROR: StatusCode = StatusCode(500);

    /// A status code in `100..=999`.
    pub fn new(code: u16) -> Option<StatusCode> {
        (100..=999).contains(&code).then_some(StatusCode(code))
    }

    /// The numeric value.
    pub fn as_u16(self) -> u16 {
        self.0
    }

    /// 1xx.
    pub fn is_informational(self) -> bool {
        self.0 < 200
    }

    /// 2xx.
    pub fn is_success(self) -> bool {
        (200..300).contains(&self.0)
    }

    /// 3xx.
    pub fn is_redirection(self) -> bool {
        (300..400).contains(&self.0)
    }

    /// 4xx.
    pub fn is_client_error(self) -> bool {
        (400..500).contains(&self.0)
    }

    /// 5xx and above.
    pub fn is_server_error(self) -> bool {
        self.0 >= 500
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// HTTP protocol version of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Version {
    /// HTTP/1.0
    Http10,
    /// HTTP/1.1
    Http11,
}

/// An HTTP request.
///
/// The client adds `Host` (unless set), `Content-Length` and, through
/// [`ClientOptions`](crate::ClientOptions), `User-Agent`. `Content-Length` and
/// `Transfer-Encoding` fields in [`Request::headers`] are ignored: the body is always sent
/// with a `Content-Length` computed from [`Request::body`].
#[derive(Debug, Clone)]
pub struct Request {
    /// The method.
    pub method: Method,
    /// The target URL.
    pub url: Url,
    /// Header fields, sent in order.
    pub headers: Headers,
    /// The body (empty for none).
    pub body: Vec<u8>,
    /// Overrides [`ClientOptions::timeout`](crate::ClientOptions::timeout) for this request.
    pub timeout: Option<Duration>,
}

impl Request {
    /// A request without headers or body.
    pub fn new(method: Method, url: Url) -> Request {
        Request {
            method,
            url,
            headers: Headers::new(),
            body: Vec::new(),
            timeout: None,
        }
    }

    /// A `GET` request.
    pub fn get(url: Url) -> Request {
        Request::new(Method::Get, url)
    }

    /// A `POST` request with a body.
    pub fn post(url: Url, body: impl Into<Vec<u8>>) -> Request {
        Request::new(Method::Post, url).with_body(body)
    }

    /// Appends a header field.
    ///
    /// # Errors
    /// [`Error::InvalidHeader`] for an invalid name or a value with control characters.
    pub fn with_header(mut self, name: &str, value: &str) -> Result<Request, Error> {
        self.headers.append(name, value)?;
        Ok(self)
    }

    /// Sets the body.
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Request {
        self.body = body.into();
        self
    }

    /// Sets a per-request timeout covering connect, send and the whole response.
    pub fn with_timeout(mut self, timeout: Duration) -> Request {
        self.timeout = Some(timeout);
        self
    }

    /// Serialises the request in origin form (`GET /path HTTP/1.1`), head and body.
    ///
    /// ```
    /// let url = dl_http::Url::parse("http://example.com:8080/a?b=1")?;
    /// let req = dl_http::Request::post(url, b"{}".to_vec())
    ///     .with_header("Content-Type", "application/json")?;
    /// assert_eq!(
    ///     req.to_bytes(),
    ///     b"POST /a?b=1 HTTP/1.1\r\nHost: example.com:8080\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}"
    /// );
    /// # Ok::<(), dl_http::Error>(())
    /// ```
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256 + self.body.len());
        self.write_head(&mut out, false, &[]);
        out.extend_from_slice(&self.body);
        out
    }

    /// Writes the request head. `absolute` selects the absolute form used with HTTP
    /// proxies. `extra` fields are added unless the request already has a field with the
    /// same name; their values must be validated by the caller.
    pub(crate) fn write_head(&self, out: &mut Vec<u8>, absolute: bool, extra: &[(&str, &str)]) {
        out.extend_from_slice(self.method.as_str().as_bytes());
        out.push(b' ');
        if absolute {
            out.extend_from_slice(self.url.scheme().as_str().as_bytes());
            out.extend_from_slice(b"://");
            out.extend_from_slice(self.url.authority().as_bytes());
        }
        out.extend_from_slice(self.url.request_target().as_bytes());
        out.extend_from_slice(b" HTTP/1.1\r\nHost: ");
        match self.headers.get("host") {
            Some(h) => out.extend_from_slice(h.as_bytes()),
            None => out.extend_from_slice(self.url.authority().as_bytes()),
        }
        out.extend_from_slice(b"\r\n");
        for (name, value) in self.headers.iter() {
            if ["host", "content-length", "transfer-encoding"]
                .iter()
                .any(|s| name.eq_ignore_ascii_case(s))
            {
                continue;
            }
            push_field(out, name, value);
        }
        for (name, value) in extra {
            if !self.headers.contains(name) {
                push_field(out, name, value);
            }
        }
        if !self.body.is_empty() || self.method.expects_body() {
            push_field(out, "Content-Length", &self.body.len().to_string());
        }
        out.extend_from_slice(b"\r\n");
    }
}

fn push_field(out: &mut Vec<u8>, name: &str, value: &str) {
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(b"\r\n");
}

/// An HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    /// Protocol version of the status line.
    pub version: Version,
    /// Status code.
    pub status: StatusCode,
    /// Reason phrase (may be empty).
    pub reason: String,
    /// Header fields in received order.
    pub headers: Headers,
    /// The complete body.
    pub body: Vec<u8>,
    /// Trailer fields of a chunked body.
    pub trailers: Headers,
}

impl Response {
    /// The body as UTF-8 text.
    ///
    /// # Errors
    /// [`Error::InvalidResponse`] if the body is not valid UTF-8.
    pub fn text(&self) -> Result<&str, Error> {
        std::str::from_utf8(&self.body).map_err(|_| bad("body is not valid UTF-8"))
    }

    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// Splits `buf` into lines ending in LF (an optional CR before it is removed).
/// Returns `(line, index after LF)`.
fn next_line(buf: &[u8], from: usize) -> Option<(&[u8], usize)> {
    let rel = buf[from..].iter().position(|&b| b == b'\n')?;
    let end = from + rel;
    let line = &buf[from..end];
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    Some((line, end + 1))
}

fn skip_leading_empty_lines(buf: &[u8]) -> Result<usize, Error> {
    let mut pos = 0;
    for _ in 0..=MAX_LEADING_EMPTY_LINES {
        let rest = &buf[pos..];
        if rest.starts_with(b"\r\n") {
            pos += 2;
        } else if rest.starts_with(b"\n") {
            pos += 1;
        } else {
            return Ok(pos);
        }
    }
    Err(bad("too many empty lines before the status line"))
}

/// Index just past the empty line that ends a head, searching LF positions from `from`.
pub(crate) fn find_head_end(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < buf.len() {
        let rel = buf[i..].iter().position(|&b| b == b'\n')?;
        let lf = i + rel;
        match (buf.get(lf + 1), buf.get(lf + 2)) {
            (Some(&b'\n'), _) => return Some(lf + 2),
            (Some(&b'\r'), Some(&b'\n')) => return Some(lf + 3),
            _ => i = lf + 1,
        }
    }
    None
}

fn parse_status_line(line: &[u8]) -> Result<(Version, StatusCode, String), Error> {
    if line.len() > MAX_STATUS_LINE {
        return Err(bad("status line is too long"));
    }
    let version = if line.starts_with(b"HTTP/1.1") {
        Version::Http11
    } else if line.starts_with(b"HTTP/1.0") {
        Version::Http10
    } else if line.starts_with(b"HTTP/") {
        return Err(bad("unsupported HTTP version"));
    } else {
        return Err(bad("invalid status line"));
    };
    let rest = &line[8..];
    let code = match rest {
        [b' ', a, b, c, tail @ ..]
            if a.is_ascii_digit()
                && b.is_ascii_digit()
                && c.is_ascii_digit()
                && matches!(tail.first(), None | Some(&b' ')) =>
        {
            u16::from(a - b'0') * 100 + u16::from(b - b'0') * 10 + u16::from(c - b'0')
        }
        _ => return Err(bad("invalid status code")),
    };
    let status = StatusCode::new(code).ok_or(bad("invalid status code"))?;
    let reason = rest.get(5..).unwrap_or(&[]);
    if !reason.iter().all(|&b| is_field_byte(b)) {
        return Err(bad("invalid byte in reason phrase"));
    }
    Ok((version, status, wire_text(reason)))
}

/// Rejects input that cannot be the start of an HTTP/1.x response.
pub(crate) fn check_head_prefix(buf: &[u8]) -> Result<(), Error> {
    let start = skip_leading_empty_lines(buf)?;
    let rest = &buf[start..];
    let n = rest.len().min(5);
    if rest[..n] != b"HTTP/"[..n] {
        return Err(bad("invalid status line"));
    }
    Ok(())
}

/// Parses a response head (status line and header fields).
///
/// Returns `Ok(None)` while the head is incomplete, or the response (with an empty body)
/// and the number of bytes the head used. Line endings may be CRLF or LF; a CR elsewhere,
/// obsolete line folding, whitespace before a colon, invalid bytes, more than
/// [`MAX_HEADERS`] fields or a head larger than [`MAX_HEAD_SIZE`] are errors.
///
/// ```
/// let input = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";
/// let (resp, used) = dl_http::parse_response_head(input)?.unwrap();
/// assert_eq!(resp.status.as_u16(), 200);
/// assert_eq!(resp.headers.get("content-length"), Some("2"));
/// assert_eq!(&input[used..], b"hi");
/// assert!(dl_http::parse_response_head(b"HTTP/1.1 200 OK\r\n")?.is_none());
/// # Ok::<(), dl_http::Error>(())
/// ```
pub fn parse_response_head(buf: &[u8]) -> Result<Option<(Response, usize)>, Error> {
    check_head_prefix(buf)?;
    let start = skip_leading_empty_lines(buf)?;
    let Some(end) = find_head_end(buf, start) else {
        if buf.len() > MAX_HEAD_SIZE {
            return Err(bad("response head is too large"));
        }
        if let Some((line, _)) = next_line(buf, start) {
            parse_status_line(line)?;
        } else if buf.len() - start > MAX_STATUS_LINE {
            return Err(bad("status line is too long"));
        }
        return Ok(None);
    };
    if end > MAX_HEAD_SIZE {
        return Err(bad("response head is too large"));
    }
    let head = &buf[..end];
    let (status_line, mut pos) = next_line(head, start).ok_or(bad("invalid status line"))?;
    if status_line.contains(&b'\r') {
        return Err(bad("bare CR in status line"));
    }
    let (version, status, reason) = parse_status_line(status_line)?;
    let mut headers = Headers::new();
    loop {
        let (line, next) = next_line(head, pos).ok_or(bad("truncated head"))?;
        pos = next;
        if line.is_empty() {
            break;
        }
        if line.contains(&b'\r') {
            return Err(bad("bare CR in header line"));
        }
        if headers.len() == MAX_HEADERS {
            return Err(bad("too many header fields"));
        }
        let (name, value) = parse_field_line(line).map_err(bad)?;
        headers.push_validated(name, value);
    }
    Ok(Some((
        Response {
            version,
            status,
            reason,
            headers,
            body: Vec::new(),
            trailers: Headers::new(),
        },
        pos,
    )))
}

/// How the body of a response is delimited (RFC 9112 section 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFraming {
    /// No body (`HEAD`, 1xx, 204, 304).
    Empty,
    /// Exactly this many bytes.
    Length(u64),
    /// `Transfer-Encoding: chunked`.
    Chunked,
    /// Everything until the server closes the connection.
    Close,
}

/// Decides how the body of `response` to a `request_method` request is delimited.
///
/// # Errors
/// [`Error::InvalidResponse`] when both `Transfer-Encoding` and `Content-Length` are
/// present (a request smuggling vector), for transfer codings other than a single
/// `chunked`, and for `Content-Length` values that are not one non-negative integer.
///
/// ```
/// use dl_http::{BodyFraming, Method, parse_response_head, response_framing};
/// let (r, _) = parse_response_head(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")?.unwrap();
/// assert_eq!(response_framing(Method::Get, &r)?, BodyFraming::Chunked);
/// assert_eq!(response_framing(Method::Head, &r)?, BodyFraming::Empty);
/// # Ok::<(), dl_http::Error>(())
/// ```
pub fn response_framing(request_method: Method, response: &Response) -> Result<BodyFraming, Error> {
    let code = response.status.as_u16();
    if request_method == Method::Head
        || response.status.is_informational()
        || code == 204
        || code == 304
    {
        return Ok(BodyFraming::Empty);
    }
    let headers = &response.headers;
    if headers.contains("transfer-encoding") {
        if headers.contains("content-length") {
            return Err(bad("both Transfer-Encoding and Content-Length are present"));
        }
        let mut codings = headers
            .get_all("transfer-encoding")
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .filter(|c| !c.is_empty());
        return match (codings.next(), codings.next()) {
            (Some(c), None) if c.eq_ignore_ascii_case("chunked") => Ok(BodyFraming::Chunked),
            _ => Err(bad("unsupported transfer coding")),
        };
    }
    let mut length: Option<u64> = None;
    for v in headers.get_all("content-length").flat_map(|v| v.split(',')) {
        let v = v.trim();
        if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("invalid Content-Length"));
        }
        let n: u64 = v.parse().map_err(|_| bad("Content-Length is too large"))?;
        match length {
            Some(prev) if prev != n => return Err(bad("conflicting Content-Length values")),
            _ => length = Some(n),
        }
    }
    Ok(match length {
        Some(n) => BodyFraming::Length(n),
        None => BodyFraming::Close,
    })
}

/// Whether the connection may carry another request after this response.
#[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), allow(dead_code))]
pub(crate) fn keep_alive(response: &Response) -> bool {
    response.version == Version::Http11 && !response.headers.has_token("connection", "close")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(s: &[u8]) -> Response {
        match parse_response_head(s) {
            Ok(Some((r, used))) => {
                assert_eq!(used, s.len());
                r
            }
            other => panic!("{:?}: {other:?}", String::from_utf8_lossy(s)),
        }
    }

    fn head_err(s: &[u8]) -> &'static str {
        match parse_response_head(s) {
            Err(Error::InvalidResponse { reason }) => reason,
            other => panic!(
                "{:?}: expected error, got {other:?}",
                String::from_utf8_lossy(s)
            ),
        }
    }

    fn framing(s: &[u8]) -> Result<BodyFraming, Error> {
        response_framing(Method::Get, &head(s))
    }

    #[test]
    fn parses_heads() {
        let r =
            head(b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nX-A: 1\r\nx-a: 2\r\n\r\n");
        assert_eq!(r.version, Version::Http11);
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        assert_eq!(r.reason, "Not Found");
        assert_eq!(r.headers.get_all("X-A").collect::<Vec<_>>(), ["1", "2"]);
        let names: Vec<&str> = r.headers.iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["Content-Type", "X-A", "x-a"]);

        let r = head(b"HTTP/1.0 200\r\n\r\n");
        assert_eq!(r.version, Version::Http10);
        assert_eq!(r.reason, "");
        let r = head(b"HTTP/1.1 200 \r\n\r\n");
        assert_eq!(r.reason, "");
        let r = head(b"HTTP/1.1 599 Odd \t Reason \xe9\r\n\r\n");
        assert_eq!(r.status.as_u16(), 599);
        assert_eq!(r.reason, "Odd \t Reason é");
        // Bare LF line endings are accepted (RFC 9112 section 2.2).
        let r = head(b"HTTP/1.1 204 No Content\nA: b\n\n");
        assert_eq!(r.headers.get("a"), Some("b"));
        // A few empty lines before the status line are skipped.
        let r = head(b"\r\n\r\nHTTP/1.1 200 OK\r\n\r\n");
        assert_eq!(r.status, StatusCode::OK);
    }

    #[test]
    fn incomplete_heads() {
        for partial in [
            &b""[..],
            b"H",
            b"HTTP/1.1",
            b"HTTP/1.1 200 OK",
            b"HTTP/1.1 200 OK\r\n",
            b"HTTP/1.1 200 OK\r\nA: b\r\n",
            b"HTTP/1.1 200 OK\r\nA: b\r\n\r",
            b"\r\n",
        ] {
            assert!(
                parse_response_head(partial).unwrap().is_none(),
                "{partial:?}"
            );
        }
        // Leftover bytes after the head are not consumed.
        let (_, used) = parse_response_head(b"HTTP/1.1 200 OK\r\n\r\nbody")
            .unwrap()
            .unwrap();
        assert_eq!(used, 19);
    }

    #[test]
    fn rejects_bad_heads() {
        assert_eq!(head_err(b"SSH-2.0-OpenSSH\r\n"), "invalid status line");
        assert_eq!(head_err(b"\x16\x03\x01"), "invalid status line");
        assert_eq!(head_err(b"HTTP/2 200\r\n\r\n"), "unsupported HTTP version");
        assert_eq!(head_err(b"HTTP/1.1 20 OK\r\n\r\n"), "invalid status code");
        assert_eq!(head_err(b"HTTP/1.1 2000 OK\r\n\r\n"), "invalid status code");
        assert_eq!(head_err(b"HTTP/1.1 099 OK\r\n\r\n"), "invalid status code");
        assert_eq!(head_err(b"HTTP/1.1  200 OK\r\n\r\n"), "invalid status code");
        assert_eq!(head_err(b"HTTP/1.1 200OK\r\n\r\n"), "invalid status code");
        assert_eq!(
            head_err(b"HTTP/1.1 200 O\x00K\r\n\r\n"),
            "invalid byte in reason phrase"
        );
        assert_eq!(
            head_err(b"HTTP/1.1 200 OK\r\n folded\r\n\r\n"),
            "obsolete line folding is not allowed"
        );
        assert_eq!(
            head_err(b"HTTP/1.1 200 OK\r\nA: 1\r\n\tfolded\r\n\r\n"),
            "obsolete line folding is not allowed"
        );
        assert_eq!(
            head_err(b"HTTP/1.1 200 OK\r\nA: 1\rB: 2\r\n\r\n"),
            "bare CR in header line"
        );
        assert_eq!(
            head_err(b"HTTP/1.1 200 \rOK\r\n\r\n"),
            "bare CR in status line"
        );
        assert_eq!(
            head_err(b"HTTP/1.1 200 OK\r\nA : 1\r\n\r\n"),
            "invalid header name"
        );
        assert_eq!(
            head_err(b"HTTP/1.1 200 OK\r\nNoColon\r\n\r\n"),
            "header line without a colon"
        );
        assert_eq!(
            head_err(b"HTTP/1.1 200 OK\r\nA: \x01\r\n\r\n"),
            "invalid byte in header value"
        );
        assert_eq!(
            head_err(b"HTTP/1.1 200 OK\r\nA\x80: 1\r\n\r\n"),
            "invalid header name"
        );
        assert_eq!(
            head_err(b"\r\n\r\n\r\n\r\n\r\n\r\nHTTP/1.1 200 OK\r\n\r\n"),
            "too many empty lines before the status line"
        );
        // The status line is checked as soon as it is complete.
        assert_eq!(head_err(b"HTTP/1.1 abc\r\n"), "invalid status code");
    }

    #[test]
    fn head_limits() {
        let mut ok = b"HTTP/1.1 200 OK\r\n".to_vec();
        for i in 0..MAX_HEADERS {
            ok.extend_from_slice(format!("X-{i}: v\r\n").as_bytes());
        }
        let mut too_many = ok.clone();
        ok.extend_from_slice(b"\r\n");
        assert_eq!(head(&ok).headers.len(), MAX_HEADERS);
        too_many.extend_from_slice(b"X-last: v\r\n\r\n");
        assert_eq!(head_err(&too_many), "too many header fields");

        let mut big = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
        big.resize(MAX_HEAD_SIZE + 1, b'a');
        assert_eq!(head_err(&big), "response head is too large");
        big.extend_from_slice(b"\r\n\r\n");
        assert_eq!(head_err(&big), "response head is too large");

        let mut fits = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
        fits.resize(MAX_HEAD_SIZE - 4, b'a');
        fits.extend_from_slice(b"\r\n\r\n");
        assert_eq!(
            head(&fits).headers.get("x").map(str::len),
            Some(MAX_HEAD_SIZE - 4 - 20)
        );

        let mut long_status = b"HTTP/1.1 200 ".to_vec();
        long_status.resize(MAX_STATUS_LINE + 1, b'a');
        assert_eq!(head_err(&long_status), "status line is too long");
        long_status.extend_from_slice(b"\r\n\r\n");
        assert_eq!(head_err(&long_status), "status line is too long");
    }

    #[test]
    fn framing_rules() {
        assert_eq!(
            framing(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n").unwrap(),
            BodyFraming::Length(10)
        );
        assert_eq!(
            framing(b"HTTP/1.1 200 OK\r\nContent-Length: 007\r\n\r\n").unwrap(),
            BodyFraming::Length(7)
        );
        assert_eq!(
            framing(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\n").unwrap(),
            BodyFraming::Length(5)
        );
        assert_eq!(
            framing(b"HTTP/1.1 200 OK\r\nContent-Length: 5, 5\r\n\r\n").unwrap(),
            BodyFraming::Length(5)
        );
        assert_eq!(
            framing(b"HTTP/1.1 200 OK\r\n\r\n").unwrap(),
            BodyFraming::Close
        );
        assert_eq!(
            framing(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n").unwrap(),
            BodyFraming::Chunked
        );
        assert_eq!(
            framing(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: , chunked\r\n\r\n").unwrap(),
            BodyFraming::Chunked
        );
        assert_eq!(
            framing(b"HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n").unwrap(),
            BodyFraming::Empty
        );
        assert_eq!(
            framing(b"HTTP/1.1 304 Not Modified\r\nContent-Length: 5\r\n\r\n").unwrap(),
            BodyFraming::Empty
        );
        assert_eq!(
            framing(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap(),
            BodyFraming::Empty
        );
        let r = head(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n");
        assert_eq!(
            response_framing(Method::Head, &r).unwrap(),
            BodyFraming::Empty
        );
    }

    #[test]
    fn framing_rejects_smuggling() {
        let bad = [
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 5\r\n\r\n"[..],
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5, 6\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: +5\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 0x10\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 1 2\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length:\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5,\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: identity\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: xchunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding:\r\n\r\n",
        ];
        for b in bad {
            assert!(
                matches!(framing(b), Err(Error::InvalidResponse { .. })),
                "{}",
                String::from_utf8_lossy(b)
            );
        }
        // Whitespace before the colon is rejected by the head parser itself.
        assert!(
            parse_response_head(b"HTTP/1.1 200 OK\r\nTransfer-Encoding : chunked\r\n\r\n").is_err()
        );
    }

    #[test]
    fn keep_alive_rules() {
        assert!(keep_alive(&head(b"HTTP/1.1 200 OK\r\n\r\n")));
        assert!(!keep_alive(&head(
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n"
        )));
        assert!(!keep_alive(&head(
            b"HTTP/1.1 200 OK\r\nConnection: Upgrade, Close\r\n\r\n"
        )));
        assert!(!keep_alive(&head(b"HTTP/1.0 200 OK\r\n\r\n")));
    }

    #[test]
    fn serialises_requests() {
        let url = Url::parse("http://example.com/p?q=1").unwrap();
        let req = Request::get(url.clone())
            .with_header("Authorization", "Bearer t")
            .unwrap()
            .with_header("Content-Length", "99")
            .unwrap()
            .with_header("Transfer-Encoding", "chunked")
            .unwrap();
        assert_eq!(
            req.to_bytes(),
            b"GET /p?q=1 HTTP/1.1\r\nHost: example.com\r\nAuthorization: Bearer t\r\n\r\n"
        );
        let mut out = Vec::new();
        req.write_head(
            &mut out,
            true,
            &[("User-Agent", "ua/1"), ("authorization", "ignored")],
        );
        assert_eq!(
            out,
            b"GET http://example.com/p?q=1 HTTP/1.1\r\nHost: example.com\r\nAuthorization: Bearer t\r\nUser-Agent: ua/1\r\n\r\n"
        );

        let req = Request::new(Method::Post, url.clone());
        assert_eq!(
            req.to_bytes(),
            b"POST /p?q=1 HTTP/1.1\r\nHost: example.com\r\nContent-Length: 0\r\n\r\n"
        );
        let req = Request::new(Method::Delete, url)
            .with_header("host", "virtual")
            .unwrap();
        assert_eq!(
            req.to_bytes(),
            b"DELETE /p?q=1 HTTP/1.1\r\nHost: virtual\r\n\r\n"
        );

        // Userinfo never reaches the request line.
        let url = Url::parse("http://u:p@[::1]:8080/").unwrap();
        let mut out = Vec::new();
        Request::get(url).write_head(&mut out, true, &[]);
        assert_eq!(
            out,
            b"GET http://[::1]:8080/ HTTP/1.1\r\nHost: [::1]:8080\r\n\r\n"
        );
    }

    #[test]
    fn methods_and_status() {
        for m in [
            Method::Get,
            Method::Head,
            Method::Post,
            Method::Put,
            Method::Patch,
            Method::Delete,
            Method::Options,
            Method::Trace,
        ] {
            assert_eq!(m.as_str().parse::<Method>().unwrap(), m);
        }
        assert!("get".parse::<Method>().is_err());
        assert!(Method::Get.is_idempotent() && Method::Put.is_idempotent());
        assert!(!Method::Post.is_idempotent() && !Method::Patch.is_idempotent());
        assert!(StatusCode::new(99).is_none() && StatusCode::new(1000).is_none());
        let s = StatusCode::new(302).unwrap();
        assert!(s.is_redirection() && !s.is_success());
        assert!(StatusCode::OK.is_success());
        assert!(StatusCode::NOT_FOUND.is_client_error());
        assert!(StatusCode::INTERNAL_SERVER_ERROR.is_server_error());
        assert_eq!(StatusCode::OK.to_string(), "200");
    }

    #[test]
    fn text_checks_utf8() {
        let mut r = head(b"HTTP/1.1 200 OK\r\n\r\n");
        r.body = "xin chào".as_bytes().to_vec();
        assert_eq!(r.text().unwrap(), "xin chào");
        r.body = vec![0xff, 0xfe];
        assert!(r.text().is_err());
    }
}
