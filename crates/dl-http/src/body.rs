//! Incremental decoder for `Transfer-Encoding: chunked` (RFC 9112 section 7.1).

use crate::Error;
use crate::headers::{Headers, parse_field_line};
use crate::message::{MAX_HEAD_SIZE, MAX_HEADERS};

/// Longest chunk-size line (size plus extensions) accepted, in bytes.
const MAX_CHUNK_LINE: usize = 4096;

fn bad(reason: &'static str) -> Error {
    Error::InvalidResponse { reason }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Size,
    Data(u64),
    DataCr,
    DataLf,
    Trailer,
    Done,
}

/// Decodes a chunked body fed in arbitrary pieces.
///
/// Lines must end in CRLF. Chunk extensions are validated and ignored; trailer fields are
/// collected (at most [`MAX_HEADERS`](crate::MAX_HEADERS) fields and
/// [`MAX_HEAD_SIZE`](crate::MAX_HEAD_SIZE) bytes). Chunk sizes that do not fit in `u64`
/// are rejected. The decoder does not limit the body size; the caller checks the output
/// length.
///
/// ```
/// let mut dec = dl_http::ChunkedDecoder::new();
/// let mut body = Vec::new();
/// let input = b"4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\nX-Sum: 9\r\n\r\nNEXT";
/// let used = dec.decode(&input[..10], &mut body)?;
/// let used = used + dec.decode(&input[used..], &mut body)?;
/// assert!(dec.is_done());
/// assert_eq!(body, b"Wikipedia");
/// assert_eq!(dec.trailers().get("x-sum"), Some("9"));
/// assert_eq!(&input[used..], b"NEXT");
/// # Ok::<(), dl_http::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct ChunkedDecoder {
    state: State,
    line: Vec<u8>,
    trailers: Headers,
    trailer_bytes: usize,
}

impl Default for ChunkedDecoder {
    fn default() -> ChunkedDecoder {
        ChunkedDecoder::new()
    }
}

impl ChunkedDecoder {
    /// A decoder at the start of a body.
    pub fn new() -> ChunkedDecoder {
        ChunkedDecoder {
            state: State::Size,
            line: Vec::new(),
            trailers: Headers::new(),
            trailer_bytes: 0,
        }
    }

    /// Whether the final chunk and the trailer section were read.
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Trailer fields received so far.
    pub fn trailers(&self) -> &Headers {
        &self.trailers
    }

    /// Consumes the decoder and returns the trailer fields.
    pub fn into_trailers(self) -> Headers {
        self.trailers
    }

    /// Decodes as much of `input` as possible, appending body bytes to `out`.
    ///
    /// Returns the number of bytes consumed. That is all of `input` unless the body ends
    /// inside it; bytes after the end belong to the next message.
    ///
    /// # Errors
    /// [`Error::InvalidResponse`] for malformed sizes, extensions, line endings or
    /// trailers, and when the limits above are exceeded. The decoder must not be used
    /// after an error.
    pub fn decode(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<usize, Error> {
        let mut i = 0;
        while i < input.len() {
            match self.state {
                State::Done => break,
                State::Data(remaining) => {
                    let avail = input.len() - i;
                    let take = usize::try_from(remaining).map_or(avail, |r| r.min(avail));
                    out.extend_from_slice(&input[i..i + take]);
                    i += take;
                    // `take <= remaining`, so the subtraction cannot underflow.
                    let left = remaining - take as u64;
                    self.state = if left == 0 {
                        State::DataCr
                    } else {
                        State::Data(left)
                    };
                }
                State::DataCr => {
                    if input[i] != b'\r' {
                        return Err(bad("missing CRLF after chunk data"));
                    }
                    i += 1;
                    self.state = State::DataLf;
                }
                State::DataLf => {
                    if input[i] != b'\n' {
                        return Err(bad("missing CRLF after chunk data"));
                    }
                    i += 1;
                    self.state = State::Size;
                }
                State::Size | State::Trailer => {
                    let limit = if self.state == State::Size {
                        MAX_CHUNK_LINE
                    } else {
                        MAX_HEAD_SIZE.saturating_sub(self.trailer_bytes)
                    };
                    let rest = &input[i..];
                    let Some(lf) = rest.iter().position(|&b| b == b'\n') else {
                        if self.line.len() + rest.len() > limit {
                            return Err(bad(if self.state == State::Size {
                                "chunk size line is too long"
                            } else {
                                "trailer section is too large"
                            }));
                        }
                        self.line.extend_from_slice(rest);
                        i = input.len();
                        break;
                    };
                    if self.line.len() + lf + 1 > limit {
                        return Err(bad(if self.state == State::Size {
                            "chunk size line is too long"
                        } else {
                            "trailer section is too large"
                        }));
                    }
                    self.line.extend_from_slice(&rest[..lf]);
                    i += lf + 1;
                    let line = std::mem::take(&mut self.line);
                    let line = line
                        .strip_suffix(b"\r")
                        .ok_or(bad("chunked line must end in CRLF"))?;
                    if line.contains(&b'\r') {
                        return Err(bad("bare CR in chunked body"));
                    }
                    if self.state == State::Size {
                        let size = parse_chunk_size(line)?;
                        self.state = if size == 0 {
                            State::Trailer
                        } else {
                            State::Data(size)
                        };
                    } else {
                        self.trailer_bytes += line.len() + 2;
                        if line.is_empty() {
                            self.state = State::Done;
                            break;
                        }
                        if self.trailers.len() == MAX_HEADERS {
                            return Err(bad("too many trailer fields"));
                        }
                        let (name, value) = parse_field_line(line).map_err(bad)?;
                        self.trailers.push_validated(name, value);
                    }
                }
            }
        }
        Ok(i)
    }
}

/// Parses `chunk-size [ BWS ; chunk-ext ]` without the CRLF.
fn parse_chunk_size(line: &[u8]) -> Result<u64, Error> {
    let digits = line.iter().take_while(|b| b.is_ascii_hexdigit()).count();
    if digits == 0 {
        return Err(bad("invalid chunk size"));
    }
    let mut size: u64 = 0;
    for &b in &line[..digits] {
        // `b` is a hex digit, so `to_digit` succeeds.
        let d = u64::from(char::from(b).to_digit(16).unwrap_or(0));
        size = size
            .checked_mul(16)
            .and_then(|s| s.checked_add(d))
            .ok_or(bad("chunk size is too large"))?;
    }
    let rest = &line[digits..];
    let ext_start = rest
        .iter()
        .position(|&b| b != b' ' && b != b'\t')
        .unwrap_or(rest.len());
    let ext = &rest[ext_start..];
    if !ext.is_empty() {
        if ext[0] != b';' {
            return Err(bad("invalid chunk size"));
        }
        if !ext
            .iter()
            .all(|&b| b == b'\t' || (0x20..=0x7e).contains(&b) || b >= 0x80)
        {
            return Err(bad("invalid byte in chunk extension"));
        }
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes `input` split into pieces of `step` bytes.
    fn decode_in_steps(input: &[u8], step: usize) -> Result<(Vec<u8>, Headers, usize), Error> {
        let mut dec = ChunkedDecoder::new();
        let mut out = Vec::new();
        let mut used = 0;
        for piece in input.chunks(step) {
            let n = dec.decode(piece, &mut out)?;
            used += n;
            if dec.is_done() {
                break;
            }
            assert_eq!(n, piece.len());
        }
        if !dec.is_done() {
            return Err(bad("incomplete"));
        }
        Ok((out, dec.into_trailers(), used))
    }

    fn err(input: &[u8]) -> &'static str {
        match decode_in_steps(input, input.len().max(1)) {
            Err(Error::InvalidResponse { reason }) => reason,
            other => panic!("{:?}: {other:?}", String::from_utf8_lossy(input)),
        }
    }

    #[test]
    fn decodes_any_split() {
        let input = b"4\r\nWiki\r\n5\r\npedia\r\nE\r\n in\r\n\r\nchunks.\r\n0\r\n\r\n";
        for step in 1..=input.len() {
            let (body, trailers, used) = decode_in_steps(input, step).unwrap();
            assert_eq!(body, b"Wikipedia in\r\n\r\nchunks.", "step {step}");
            assert!(trailers.is_empty());
            assert_eq!(used, input.len());
        }
    }

    #[test]
    fn extensions_and_trailers() {
        let input = b"a;name=value;quoted=\"x y\"\r\n0123456789\r\n3 ; ext\r\nabc\r\n0;last\r\nExpires: never\r\nX-Checksum:  42 \r\n\r\ntail";
        for step in 1..=input.len() {
            let (body, trailers, used) = decode_in_steps(input, step).unwrap();
            assert_eq!(body, b"0123456789abc");
            assert_eq!(trailers.get("expires"), Some("never"));
            assert_eq!(trailers.get("x-checksum"), Some("42"));
            assert_eq!(&input[used..], b"tail");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_chunk_size(b"0").unwrap(), 0);
        assert_eq!(parse_chunk_size(b"00000000000000000000001").unwrap(), 1);
        assert_eq!(parse_chunk_size(b"fF").unwrap(), 255);
        assert_eq!(parse_chunk_size(b"ffffffffffffffff").unwrap(), u64::MAX);
        assert_eq!(parse_chunk_size(b"10\t;x").unwrap(), 16);
        assert_eq!(parse_chunk_size(b"10  ").unwrap(), 16);
        for bad in [
            &b""[..],
            b" 5",
            b"-5",
            b"+5",
            b"0x5",
            b"5x",
            b"5 5",
            b"g",
            b"5;\x00",
            b"5;\x7f",
            b"10000000000000000",
        ] {
            assert!(parse_chunk_size(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            match parse_chunk_size(b"1ffffffffffffffff") {
                Err(Error::InvalidResponse { reason }) => reason,
                _ => "",
            },
            "chunk size is too large"
        );
    }

    #[test]
    fn huge_chunk_streams_without_allocating() {
        // A declared size near u64::MAX is accepted and simply streams data; the client
        // enforces its own body limit on the output.
        let mut dec = ChunkedDecoder::new();
        let mut out = Vec::new();
        assert_eq!(
            dec.decode(b"ffffffffffffffff\r\nabc", &mut out).unwrap(),
            21
        );
        assert_eq!(out, b"abc");
        assert!(!dec.is_done());
    }

    #[test]
    fn malformed() {
        assert_eq!(err(b"zz\r\n"), "invalid chunk size");
        assert_eq!(err(b"\r\n"), "invalid chunk size");
        assert_eq!(err(b"3\nabc\r\n0\r\n\r\n"), "chunked line must end in CRLF");
        assert_eq!(
            err(b"3\r\nabcX\r\n0\r\n\r\n"),
            "missing CRLF after chunk data"
        );
        assert_eq!(
            err(b"3\r\nabc\rX0\r\n\r\n"),
            "missing CRLF after chunk data"
        );
        assert_eq!(err(b"3\r\nabc\n0\r\n\r\n"), "missing CRLF after chunk data");
        assert_eq!(err(b"3\r\r\nabc\r\n0\r\n\r\n"), "bare CR in chunked body");
        assert_eq!(err(b"0\r\nX: 1\n\r\n"), "chunked line must end in CRLF");
        assert_eq!(
            err(b"0\r\n folded\r\n\r\n"),
            "obsolete line folding is not allowed"
        );
        assert_eq!(err(b"0\r\nNoColon\r\n\r\n"), "header line without a colon");
        assert_eq!(err(b"0\r\nX: \x01\r\n\r\n"), "invalid byte in header value");
        assert_eq!(err(b"10000000000000000\r\n"), "chunk size is too large");

        let mut long = b"1;".to_vec();
        long.resize(MAX_CHUNK_LINE + 10, b'a');
        assert_eq!(err(&long), "chunk size line is too long");
        long.extend_from_slice(b"\r\n");
        assert_eq!(err(&long), "chunk size line is too long");
        // Split across many reads, the line limit still holds.
        assert!(decode_in_steps(&long, 7).is_err());

        let mut many = b"0\r\n".to_vec();
        for i in 0..=MAX_HEADERS {
            many.extend_from_slice(format!("T{i}: v\r\n").as_bytes());
        }
        many.extend_from_slice(b"\r\n");
        assert_eq!(err(&many), "too many trailer fields");

        let mut big = b"0\r\nX: ".to_vec();
        big.resize(MAX_HEAD_SIZE + 10, b'a');
        assert_eq!(err(&big), "trailer section is too large");
    }
}
