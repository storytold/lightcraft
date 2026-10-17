//! HTTP/1.1 wire format: the response head and body framing (Content-Length, chunked, or read
//! until close). Everything read from the server is bounded.

use std::io::{BufRead, Read};

use crate::NetError;

/// Largest response head (status line plus headers).
const MAX_HEAD: usize = 64 * 1024;
/// Most header lines.
const MAX_HEADERS: usize = 256;
/// Longest chunk-size or trailer line.
const MAX_LINE: usize = 8 * 1024;

/// A response's status line and headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    pub reason: String,
    /// Header names as sent; look them up with [`Head::header`].
    pub headers: Vec<(String, String)>,
}

impl Head {
    /// First value of a header (case-insensitive name).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Read one line ending in LF (CR stripped), at most `max` bytes, one byte at a time (so nothing
/// past the line is consumed: a proxy tunnel's TLS bytes follow its head).
fn read_line(r: &mut impl Read, max: usize) -> Result<String, NetError> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        let n = r.read(&mut b)?;
        if n == 0 {
            return Err(NetError::Protocol("connection closed in the middle of a line".into()));
        }
        if b[0] == b'\n' {
            break;
        }
        if line.len() >= max {
            return Err(NetError::Protocol("line too long".into()));
        }
        line.push(b[0]);
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Read a status line and headers. `1xx` interim responses are skipped.
pub fn read_head(r: &mut impl Read) -> Result<Head, NetError> {
    loop {
        let mut used = 0usize;
        let status_line = read_line(r, MAX_HEAD)?;
        used += status_line.len();
        let mut parts = status_line.splitn(3, ' ');
        let version = parts.next().unwrap_or_default();
        if !version.starts_with("HTTP/1.") {
            return Err(NetError::Protocol(format!("not an HTTP/1.x response: {:?}", status_line.chars().take(40).collect::<String>())));
        }
        let status: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|s| (100..1000).contains(s))
            .ok_or_else(|| NetError::Protocol("bad status code".into()))?;
        let reason = parts.next().unwrap_or_default().to_string();
        let mut headers = Vec::new();
        loop {
            let line = read_line(r, MAX_HEAD)?;
            used += line.len() + 2;
            if used > MAX_HEAD {
                return Err(NetError::Protocol("response head larger than 64 KiB".into()));
            }
            if line.is_empty() {
                break;
            }
            if headers.len() >= MAX_HEADERS {
                return Err(NetError::Protocol("too many headers".into()));
            }
            let Some((k, v)) = line.split_once(':') else {
                return Err(NetError::Protocol("malformed header line".into()));
            };
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        if (100..200).contains(&status) && status != 101 {
            continue;
        }
        return Ok(Head { status, reason, headers });
    }
}

/// How the body is delimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    Empty,
    Length(u64),
    Chunked,
    Close,
}

impl Framing {
    pub(crate) fn of(head: &Head, request_was_head: bool) -> Result<Framing, NetError> {
        if request_was_head || head.status == 204 || head.status == 304 || (100..200).contains(&head.status) {
            return Ok(Framing::Empty);
        }
        if head.header("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked")) {
            return Ok(Framing::Chunked);
        }
        match head.header("content-length") {
            Some(v) => v.trim().parse().map(Framing::Length).map_err(|_| NetError::Protocol("bad Content-Length".into())),
            None => Ok(Framing::Close),
        }
    }
}

/// Reads exactly the body, per its framing.
pub(crate) struct BodyReader<R> {
    inner: R,
    state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Bytes left (Content-Length, or of the current chunk).
    Left {
        left: u64,
        chunked: bool,
    },
    /// Before a chunk-size line (`first`: no CRLF of a previous chunk to consume).
    ChunkStart {
        first: bool,
    },
    UntilClose,
    Done,
}

impl<R: BufRead> BodyReader<R> {
    pub(crate) fn new(inner: R, framing: Framing) -> Self {
        let state = match framing {
            Framing::Empty | Framing::Length(0) => State::Done,
            Framing::Length(n) => State::Left { left: n, chunked: false },
            Framing::Chunked => State::ChunkStart { first: true },
            Framing::Close => State::UntilClose,
        };
        BodyReader { inner, state }
    }

    fn next_chunk(&mut self, first: bool) -> Result<(), NetError> {
        if !first {
            let crlf = read_line(&mut self.inner, 2)?;
            if !crlf.is_empty() {
                return Err(NetError::Protocol("missing CRLF after chunk".into()));
            }
        }
        let line = read_line(&mut self.inner, MAX_LINE)?;
        let size = line.split(';').next().unwrap_or_default().trim();
        let size = u64::from_str_radix(size, 16).map_err(|_| NetError::Protocol("bad chunk size".into()))?;
        if size == 0 {
            // trailers, then the empty line
            let mut used = 0usize;
            loop {
                let t = read_line(&mut self.inner, MAX_LINE)?;
                if t.is_empty() {
                    break;
                }
                used += t.len();
                if used > MAX_HEAD {
                    return Err(NetError::Protocol("trailers too large".into()));
                }
            }
            self.state = State::Done;
        } else {
            self.state = State::Left { left: size, chunked: true };
        }
        Ok(())
    }
}

impl<R: BufRead> Read for BodyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.state {
                State::Done => return Ok(0),
                State::UntilClose => return self.inner.read(buf),
                State::ChunkStart { first } => self.next_chunk(first)?,
                State::Left { left, chunked } => {
                    let want = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
                    let n = self.inner.read(buf.get_mut(..want).unwrap_or_default())?;
                    if n == 0 {
                        return Err(NetError::Protocol("connection closed before the end of the body".into()).into());
                    }
                    let left = left.saturating_sub(n as u64);
                    self.state = match (left, chunked) {
                        (0, true) => State::ChunkStart { first: false },
                        (0, false) => State::Done,
                        (left, chunked) => State::Left { left, chunked },
                    };
                    return Ok(n);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn body(raw: &[u8], framing: Framing) -> Result<Vec<u8>, NetError> {
        let mut out = Vec::new();
        BodyReader::new(Cursor::new(raw.to_vec()), framing).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn heads_and_bodies() {
        let mut r = Cursor::new(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 3\r\nX-A:  b \r\n\r\nabc".to_vec());
        let h = read_head(&mut r).unwrap();
        assert_eq!((h.status, h.reason.as_str(), h.header("x-a")), (200, "OK", Some("b")));
        assert_eq!(Framing::of(&h, false).unwrap(), Framing::Length(3));
        assert_eq!(body(b"4;x=y\r\nWiki\r\n5\r\npedia\r\n0\r\nT: v\r\n\r\n", Framing::Chunked).unwrap(), b"Wikipedia");
        assert_eq!(body(b"rest", Framing::Close).unwrap(), b"rest");
    }

    #[test]
    fn hostile_responses_are_errors() {
        for raw in [&b"SSH-2.0\r\n\r\n"[..], b"HTTP/1.1 abc OK\r\n\r\n", b"HTTP/1.1 200 OK\r\nnocolon\r\n\r\n", b"HTTP/1.1 200"] {
            assert!(read_head(&mut Cursor::new(raw.to_vec())).is_err());
        }
        let huge = format!("HTTP/1.1 200 OK\r\n{}\r\n", "X: y\r\n".repeat(20_000));
        assert!(read_head(&mut Cursor::new(huge.into_bytes())).is_err());
        assert!(body(b"zz\r\n", Framing::Chunked).is_err());
        assert!(body(b"ffffffffffffffffffff\r\n", Framing::Chunked).is_err());
        assert!(body(b"3\r\nabcX", Framing::Chunked).is_err());
        assert!(body(b"ab", Framing::Length(5)).is_err());
    }
}
