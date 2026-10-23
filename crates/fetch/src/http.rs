//! A small HTTP/1.1 GET client for model downloads, in pure Rust: `std::net` sockets and, for
//! `https://`, rustls with the RustCrypto provider (`rustls-rustcrypto`; no `ring` or
//! `aws-lc-rs`, so nothing is compiled from C or assembly) and the Mozilla root certificates
//! (`webpki-roots`).
//!
//! Only what a large-file download needs: one request per connection (`Connection: close`),
//! `Range` requests for resuming, redirects (followed by the caller), `Content-Length`,
//! chunked and read-to-close bodies. Every read waits at most `stall` for data and checks the
//! cancel flag at least twice a second, so a dead server or a cancelled download never hangs
//! the downloading thread. No proxies (see docs/ai-masks.md).

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// The URL can't be used (scheme, host, characters).
    BadUrl(String),
    /// Name resolution or connecting failed (or took longer than the connect timeout).
    Connect(String),
    /// No data arrived for the stall timeout.
    Stalled,
    /// The cancel flag was raised.
    Cancelled,
    /// TLS failed (certificate, handshake).
    Tls(String),
    /// The server's response could not be understood.
    Protocol(String),
    /// Any other I/O error.
    Io(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::BadUrl(e) => write!(f, "unusable URL: {e}"),
            HttpError::Connect(e) => write!(f, "could not connect: {e}"),
            HttpError::Stalled => write!(f, "the server stopped sending data"),
            HttpError::Cancelled => write!(f, "cancelled"),
            HttpError::Tls(e) => write!(f, "secure connection failed: {e}"),
            HttpError::Protocol(e) => write!(f, "unexpected response: {e}"),
            HttpError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// A parsed `http://` or `https://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

/// Remove `.` and `..` segments from the path part of `path` (RFC 3986 section 5.2.4); the query is kept as is.
fn remove_dot_segments(path: &str) -> String {
    let (p, query) = match path.find('?') {
        Some(i) => (path.get(..i).unwrap_or_default(), path.get(i..).unwrap_or_default()),
        None => (path, ""),
    };
    let mut out: Vec<&str> = Vec::new();
    let mut segments = p.split('/').skip(1).peekable();
    let mut trailing_slash = false;
    while let Some(seg) = segments.next() {
        let last = segments.peek().is_none();
        match seg {
            "." => trailing_slash = last,
            ".." => {
                out.pop();
                trailing_slash = last;
            }
            _ => {
                out.push(seg);
                trailing_slash = false;
            }
        }
    }
    let mut result = format!("/{}", out.join("/"));
    if trailing_slash && !result.ends_with('/') {
        result.push('/');
    }
    result.push_str(query);
    result
}

/// `s` without the leading `prefix`, matched ASCII case-insensitively (RFC 3986 §3.1).
fn strip_scheme<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| s.get(prefix.len()..).unwrap_or_default())
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, HttpError> {
        let bad = |why: &str| HttpError::BadUrl(why.to_string());
        if s.len() > 16 * 1024 {
            return Err(bad("URL exceeds 16 KiB"));
        }
        let s = s.trim();
        if s.chars().any(|c| c.is_control() || c == ' ') {
            return Err(bad("contains spaces or control characters"));
        }
        let (tls, rest) = if let Some(r) = strip_scheme(s, "https://") {
            (true, r)
        } else if let Some(r) = strip_scheme(s, "http://") {
            (false, r)
        } else {
            return Err(bad("only http:// and https:// are supported"));
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (rest.get(..i).unwrap_or_default(), rest.get(i..).unwrap_or_default()),
            None => (rest, ""),
        };
        let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
        let path = remove_dot_segments(&path);
        if authority.contains('@') {
            return Err(bad("user names and passwords in URLs are not supported"));
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let end = v6.find(']').ok_or_else(|| bad("unterminated IPv6 address"))?;
            let host = v6.get(..end).unwrap_or_default();
            let after = v6.get(end + 1..).unwrap_or_default();
            if !after.is_empty() && !after.starts_with(':') {
                return Err(bad("unexpected text after IPv6 address"));
            }
            (host.to_string(), after.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (authority.to_string(), None),
            }
        };
        let port = match port {
            Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| bad("bad port"))?,
            None if tls => 443,
            None => 80,
        };
        if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '_')) {
            return Err(bad("bad host name"));
        }
        Ok(Url { tls, host: host.to_ascii_lowercase(), port, path })
    }

    /// `Location` of a redirect, resolved against this URL.
    pub fn join(&self, location: &str) -> Result<Url, HttpError> {
        let l = location.trim();
        if strip_scheme(l, "http://").is_some() || strip_scheme(l, "https://").is_some() {
            let url = Url::parse(l)?;
            if self.tls && !url.tls {
                return Err(HttpError::BadUrl("redirect from https to http refused".into()));
            }
            return Ok(url);
        }
        // another scheme (`ftp:`, `file:`, `data:`…)
        if let Some((scheme, _)) = l.split_once(':')
            && !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return Err(HttpError::BadUrl(format!("redirect to an unsupported location `{scheme}:`")));
        }
        let scheme = if self.tls { "https" } else { "http" };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let base = format!("{scheme}://{host}:{}", self.port);
        if let Some(rest) = l.strip_prefix("//") {
            return Url::parse(&format!("{scheme}://{rest}"));
        }
        if l.starts_with('/') {
            return Url::parse(&format!("{base}{l}"));
        }
        // empty or fragment-only: the same document
        if l.is_empty() || l.starts_with('#') {
            return Ok(self.clone());
        }
        // relative to the current directory
        let dir = self.path.split('?').next().unwrap_or("/");
        // a query-only reference keeps the whole path, file name included (RFC 3986 §5.2.2)
        if l.starts_with('?') {
            return Url::parse(&format!("{base}{dir}{l}"));
        }
        let dir = dir.rsplit_once('/').map_or("", |(d, _)| d);
        Url::parse(&format!("{base}{dir}/{l}"))
    }

    fn host_header(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let default = if self.tls { 443 } else { 80 };
        if self.port == default { host } else { format!("{host}:{}", self.port) }
    }

    /// The URL without its query (for messages: a query may hold a signed token).
    pub fn display(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}{}", self.host_header(), self.path.split('?').next().unwrap_or("/"))
    }
}

/// Timeouts and the cancel flag for one request.
pub struct Limits<'a> {
    pub connect: Duration,
    pub stall: Duration,
    pub cancel: &'a AtomicBool,
}

/// How often a blocked read wakes up to check the cancel flag.
const TICK: Duration = Duration::from_millis(250);
/// Longest status line + headers accepted.
const MAX_HEAD: usize = 64 * 1024;

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::ClientConnection>, TcpStream),
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, HttpError> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| HttpError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Resolve a host name within `deadline`, checking the cancel flag while it runs. The system resolver blocks with no
/// timeout of its own, so it runs on a helper thread (which ends when the resolver returns); a lookup that takes too
/// long is given up on, and Cancel works during it (issue #1532).
fn lookup(
    host: &str,
    resolve: impl FnOnce() -> std::io::Result<Vec<SocketAddr>> + Send + 'static,
    deadline: Instant,
    limits: &Limits,
) -> Result<Vec<SocketAddr>, HttpError> {
    if limits.cancel.load(Ordering::Relaxed) {
        return Err(HttpError::Cancelled);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("lc-fetch-lookup".into())
        .spawn(move || {
            let _ = tx.send(resolve());
        })
        .map_err(|e| HttpError::Connect(format!("{host}: {e}")))?;
    loop {
        if limits.cancel.load(Ordering::Relaxed) {
            return Err(HttpError::Cancelled);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(HttpError::Connect(format!("{host}: the name lookup timed out")));
        }
        match rx.recv_timeout(left.min(TICK)) {
            Ok(r) => return r.map_err(|e| HttpError::Connect(format!("{host}: {e}"))),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(HttpError::Connect(format!("{host}: the name lookup failed"))),
        }
    }
}

/// Connect to the first of `addrs` that answers, all of them together within `deadline` (each used to get the whole
/// connect timeout of its own, issue #1532).
fn connect_any(host: &str, addrs: &[SocketAddr], deadline: Instant, limits: &Limits) -> Result<TcpStream, HttpError> {
    let mut last = format!("{host}: no address");
    for a in addrs {
        if limits.cancel.load(Ordering::Relaxed) {
            return Err(HttpError::Cancelled);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(HttpError::Connect(format!("{host}: connecting timed out")));
        }
        match TcpStream::connect_timeout(a, left) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("{host}: {e}"),
        }
    }
    Err(HttpError::Connect(last))
}

impl Stream {
    fn connect(url: &Url, limits: &Limits) -> Result<Stream, HttpError> {
        // the lookup and the connection attempts share one connect timeout
        let deadline = Instant::now() + limits.connect;
        let (host, port) = (url.host.clone(), url.port);
        let addrs = lookup(&url.host, move || (host.as_str(), port).to_socket_addrs().map(Iterator::collect), deadline, limits)?;
        let sock = connect_any(&url.host, &addrs, deadline, limits)?;
        sock.set_read_timeout(Some(TICK)).map_err(|e| HttpError::Io(e.to_string()))?;
        sock.set_write_timeout(Some(TICK)).map_err(|e| HttpError::Io(e.to_string()))?;
        let _ = sock.set_nodelay(true);
        if !url.tls {
            return Ok(Stream::Plain(sock));
        }
        let name = rustls::pki_types::ServerName::try_from(url.host.clone()).map_err(|e| HttpError::Tls(format!("{}: {e}", url.host)))?;
        let conn = rustls::ClientConnection::new(tls_config()?, name).map_err(|e| HttpError::Tls(e.to_string()))?;
        let mut s = Stream::Tls(Box::new(conn), sock);
        s.handshake(limits)?;
        Ok(s)
    }

    /// Wait for `f` to make progress, retrying timeouts until `stall` passes without any.
    fn retry<T>(limits: &Limits, mut f: impl FnMut() -> std::io::Result<T>) -> Result<T, HttpError> {
        let start = Instant::now();
        loop {
            if limits.cancel.load(Ordering::Relaxed) {
                return Err(HttpError::Cancelled);
            }
            match f() {
                Ok(v) => return Ok(v),
                Err(e) if is_timeout(&e) => {
                    if start.elapsed() >= limits.stall {
                        return Err(HttpError::Stalled);
                    }
                }
                Err(e) if e.kind() == ErrorKind::InvalidData => return Err(HttpError::Tls(e.to_string())),
                Err(e) => return Err(HttpError::Io(e.to_string())),
            }
        }
    }

    fn handshake(&mut self, limits: &Limits) -> Result<(), HttpError> {
        let Stream::Tls(conn, sock) = self else { return Ok(()) };
        while conn.is_handshaking() {
            Self::retry(limits, || conn.complete_io(sock))?;
        }
        Ok(())
    }

    fn write_all(&mut self, data: &[u8], limits: &Limits) -> Result<(), HttpError> {
        match self {
            Stream::Plain(sock) => {
                let mut rest = data;
                while !rest.is_empty() {
                    let n = Self::retry(limits, || sock.write(rest))?;
                    if n == 0 {
                        return Err(HttpError::Io("connection closed while sending the request".into()));
                    }
                    rest = rest.get(n..).unwrap_or_default();
                }
                Ok(())
            }
            Stream::Tls(conn, sock) => {
                // buffered by rustls, then sent as TLS records
                conn.writer().write_all(data).map_err(|e| HttpError::Tls(e.to_string()))?;
                while conn.wants_write() {
                    Self::retry(limits, || conn.write_tls(sock))?;
                }
                Ok(())
            }
        }
    }

    /// Read some bytes (0 at the end of the stream), waiting at most `stall` for them.
    fn read(&mut self, buf: &mut [u8], limits: &Limits) -> Result<usize, HttpError> {
        match self {
            Stream::Plain(sock) => Self::retry(limits, || sock.read(buf)),
            Stream::Tls(conn, sock) => {
                let start = Instant::now();
                loop {
                    if limits.cancel.load(Ordering::Relaxed) {
                        return Err(HttpError::Cancelled);
                    }
                    match conn.reader().read(buf) {
                        Ok(n) => return Ok(n),
                        // closed without close_notify: the caller checks the length it got
                        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(0),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                        Err(e) => return Err(HttpError::Tls(e.to_string())),
                    }
                    // no plaintext buffered: more TLS records from the socket
                    match conn.read_tls(sock) {
                        Ok(0) => return Ok(0),
                        Ok(_) => {
                            conn.process_new_packets().map_err(|e| HttpError::Tls(e.to_string()))?;
                            while conn.wants_write() {
                                Self::retry(limits, || conn.write_tls(sock))?;
                            }
                        }
                        Err(e) if is_timeout(&e) => {
                            if start.elapsed() >= limits.stall {
                                return Err(HttpError::Stalled);
                            }
                        }
                        Err(e) => return Err(HttpError::Io(e.to_string())),
                    }
                }
            }
        }
    }
}

/// The connection with a read buffer (headers and chunk sizes are read line by line).
struct Conn {
    stream: Stream,
    buf: Vec<u8>,
    pos: usize,
}

impl Conn {
    fn fill(&mut self, limits: &Limits) -> Result<usize, HttpError> {
        if self.pos >= self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        let mut tmp = [0u8; 64 * 1024];
        let n = self.stream.read(&mut tmp, limits)?;
        self.buf.extend_from_slice(tmp.get(..n).unwrap_or_default());
        Ok(n)
    }

    /// One line without its CRLF; at most `max` bytes.
    fn line(&mut self, max: usize, limits: &Limits) -> Result<String, HttpError> {
        loop {
            let avail = self.buf.get(self.pos..).unwrap_or_default();
            if let Some(i) = avail.iter().position(|b| *b == b'\n') {
                let line = avail.get(..i).unwrap_or_default();
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let s = String::from_utf8_lossy(line).into_owned();
                self.pos = self.pos.saturating_add(i + 1);
                return Ok(s);
            }
            if avail.len() > max {
                return Err(HttpError::Protocol("header line too long".into()));
            }
            // keep the partial line, read more after it
            if self.pos > 0 {
                self.buf.drain(..self.pos.min(self.buf.len()));
                self.pos = 0;
            }
            let mut tmp = [0u8; 16 * 1024];
            let n = self.stream.read(&mut tmp, limits)?;
            if n == 0 {
                return Err(HttpError::Protocol("connection closed in the middle of the response".into()));
            }
            self.buf.extend_from_slice(tmp.get(..n).unwrap_or_default());
        }
    }

    /// Up to `out.len()` body bytes (0 at the end of the stream).
    fn read(&mut self, out: &mut [u8], limits: &Limits) -> Result<usize, HttpError> {
        if self.pos >= self.buf.len() && self.fill(limits)? == 0 {
            return Ok(0);
        }
        let avail = self.buf.get(self.pos..).unwrap_or_default();
        let n = avail.len().min(out.len());
        if let (Some(dst), Some(src)) = (out.get_mut(..n), avail.get(..n)) {
            dst.copy_from_slice(src);
        }
        self.pos = self.pos.saturating_add(n);
        Ok(n)
    }
}

/// A response's status, headers and body.
pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    conn: Conn,
    body: BodyMode,
}

enum BodyMode {
    Length(u64),
    Chunked { left: u64, done: bool },
    Close,
    Done,
}

impl Response {
    /// The first header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The body's length when the server announced it.
    pub fn content_length(&self) -> Option<u64> {
        match self.body {
            BodyMode::Length(n) => Some(n),
            _ => None,
        }
    }

    /// Read body bytes into `out` (0 at the end of the body).
    pub fn read(&mut self, out: &mut [u8], limits: &Limits) -> Result<usize, HttpError> {
        match &mut self.body {
            BodyMode::Done => Ok(0),
            BodyMode::Length(left) => {
                if *left == 0 {
                    self.body = BodyMode::Done;
                    return Ok(0);
                }
                let want = usize::try_from(*left).unwrap_or(usize::MAX).min(out.len());
                let n = self.conn.read(out.get_mut(..want).unwrap_or_default(), limits)?;
                if n == 0 {
                    return Err(HttpError::Protocol("the connection closed before the end of the file".into()));
                }
                *left = left.saturating_sub(n as u64);
                Ok(n)
            }
            BodyMode::Close => {
                let n = self.conn.read(out, limits)?;
                if n == 0 {
                    self.body = BodyMode::Done;
                }
                Ok(n)
            }
            BodyMode::Chunked { left, done } => {
                if *done {
                    return Ok(0);
                }
                if *left == 0 {
                    let line = self.conn.line(1024, limits)?;
                    let hex = line.split(';').next().unwrap_or_default().trim();
                    let size = u64::from_str_radix(hex, 16).map_err(|_| HttpError::Protocol(format!("bad chunk size `{hex}`")))?;
                    if size == 0 {
                        // trailers, up to the empty line
                        let mut n = 0;
                        while !self.conn.line(MAX_HEAD, limits)?.is_empty() {
                            n += 1;
                            if n > 100 {
                                return Err(HttpError::Protocol("too many trailers".into()));
                            }
                        }
                        *done = true;
                        return Ok(0);
                    }
                    *left = size;
                }
                let want = usize::try_from(*left).unwrap_or(usize::MAX).min(out.len());
                let n = self.conn.read(out.get_mut(..want).unwrap_or_default(), limits)?;
                if n == 0 {
                    return Err(HttpError::Protocol("the connection closed in the middle of a chunk".into()));
                }
                *left = left.saturating_sub(n as u64);
                if *left == 0 && !self.conn.line(16, limits)?.is_empty() {
                    return Err(HttpError::Protocol("missing CRLF after a chunk".into()));
                }
                Ok(n)
            }
        }
    }
}

/// Send `GET url` with extra `headers` and read the status line and headers.
pub fn get(url: &Url, headers: &[(&str, String)], limits: &Limits) -> Result<Response, HttpError> {
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: LightCraft/{}\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        url.path,
        url.host_header(),
        env!("CARGO_PKG_VERSION")
    );
    for (k, v) in headers {
        if v.chars().any(|c| c == '\r' || c == '\n') || k.chars().any(|c| c == '\r' || c == '\n' || c == ':') {
            return Err(HttpError::BadUrl("header with a line break".into()));
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    let mut stream = Stream::connect(url, limits)?;
    stream.write_all(req.as_bytes(), limits)?;
    let mut conn = Conn { stream, buf: Vec::new(), pos: 0 };
    let status_line = conn.line(MAX_HEAD, limits)?;
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(HttpError::Protocol(format!("not an HTTP/1.x response: {}", status_line.chars().take(60).collect::<String>())));
    }
    let status = parts.next().and_then(|s| s.parse::<u16>().ok()).ok_or_else(|| HttpError::Protocol("bad status line".into()))?;
    let mut headers = Vec::new();
    let mut total = status_line.len();
    loop {
        let line = conn.line(MAX_HEAD, limits)?;
        if line.is_empty() {
            break;
        }
        total = total.saturating_add(line.len());
        if total > MAX_HEAD || headers.len() > 200 {
            return Err(HttpError::Protocol("response headers too large".into()));
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let find = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
    let body = if status == 204 || status == 304 || (100..200).contains(&status) {
        BodyMode::Done
    } else if find("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        BodyMode::Chunked { left: 0, done: false }
    } else if let Some(n) = find("content-length") {
        BodyMode::Length(n.trim().parse::<u64>().map_err(|_| HttpError::Protocol(format!("bad Content-Length `{n}`")))?)
    } else {
        BodyMode::Close
    };
    Ok(Response { status, headers, conn, body })
}

#[cfg(test)]
mod tests_connect {
    use super::*;

    fn limits(cancel: &AtomicBool) -> Limits<'_> {
        Limits { connect: Duration::from_millis(300), stall: Duration::from_secs(1), cancel }
    }

    /// A name lookup that hangs is given up on at the connect timeout, and Cancel stops it at once (issue #1532).
    #[test]
    fn a_hung_name_lookup_times_out_and_can_be_cancelled() {
        let cancel = AtomicBool::new(false);
        let slow = || {
            std::thread::sleep(Duration::from_secs(10));
            Ok(Vec::new())
        };
        let t = Instant::now();
        let r = lookup("slow.example", slow, Instant::now() + Duration::from_millis(300), &limits(&cancel));
        assert!(matches!(&r, Err(HttpError::Connect(m)) if m.contains("timed out")), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        // Cancel while the lookup is waiting on the resolver, not before it starts
        let t = Instant::now();
        let r = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                cancel.store(true, Ordering::Relaxed);
            });
            lookup("slow.example", slow, Instant::now() + Duration::from_secs(30), &limits(&cancel))
        });
        assert!(matches!(r, Err(HttpError::Cancelled)), "{r:?}");
        assert!(t.elapsed() >= Duration::from_millis(100) && t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        // and a lookup asked for after Cancel doesn't start
        assert!(matches!(lookup("slow.example", slow, Instant::now() + Duration::from_secs(30), &limits(&cancel)), Err(HttpError::Cancelled)));
        // a lookup that answers is used
        let cancel = AtomicBool::new(false);
        let local: SocketAddr = "127.0.0.1:9".parse().unwrap();
        assert_eq!(lookup("local", move || Ok(vec![local]), Instant::now() + Duration::from_secs(5), &limits(&cancel)).unwrap(), [local]);
    }

    /// Several addresses that don't answer share one deadline instead of each getting the full timeout.
    #[test]
    fn addresses_share_one_connect_deadline() {
        let cancel = AtomicBool::new(false);
        // non-routable addresses: each either hangs until the deadline or fails at once, never longer
        let addrs: Vec<SocketAddr> = (1..=5).map(|i| format!("10.255.255.{i}:81").parse().unwrap()).collect();
        let t = Instant::now();
        let r = connect_any("blackhole", &addrs, Instant::now() + Duration::from_millis(400), &limits(&cancel));
        assert!(r.is_err());
        assert!(t.elapsed() < Duration::from_millis(1500), "{:?}", t.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_only_or_empty_location_keeps_file_name_and_query() {
        let base = Url::parse("https://a.example/x/y/file?q").unwrap();
        for l in ["#top", "", "  "] {
            let u = base.join(l).unwrap();
            assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (true, "a.example", 443, "/x/y/file?q"), "{l:?}");
        }
    }

    #[test]
    fn rejects_oversized_and_control_character_urls() {
        assert!(Url::parse(&format!("https://example.com/{}", "x".repeat(16 * 1024))).is_err());
        assert!(Url::parse("https://example.com/x?secret=abc\r\nInjected: 1").is_err());
    }

    #[test]
    fn join_query_only_keeps_file_name() {
        let u = Url::parse("https://a.example/x/y/file?q=1").unwrap();
        assert_eq!(u.join("?page=2").unwrap().path, "/x/y/file?page=2");
    }

    #[test]
    fn join_removes_dot_segments() {
        let base = Url::parse("https://a.example/x/y/file").unwrap();
        for (loc, want) in
            [("../z", "/x/z"), ("./z", "/x/y/z"), ("../../../z", "/z"), ("/a/./b/../c", "/a/c"), ("../z?q=1", "/x/z?q=1"), ("..", "/x/")]
        {
            assert_eq!(base.join(loc).unwrap().path, want, "{loc}");
        }
    }

    #[test]
    fn url_scheme_is_matched_case_insensitively() {
        let u = Url::parse("HTTPS://A.example/z").unwrap();
        assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (true, "a.example", 443, "/z"));
        assert!(Url::parse("Http://a.example/").is_ok());
        let j = Url::parse("https://a.example/start").unwrap().join("HTTPS://cdn.example/file").unwrap();
        assert_eq!((j.tls, j.host.as_str(), j.path.as_str()), (true, "cdn.example", "/file"));
    }

    #[test]
    fn parses_urls() {
        let u = Url::parse("https://cdn.example.com/models/sam3/model.safetensors?sig=1#x").unwrap();
        assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (true, "cdn.example.com", 443, "/models/sam3/model.safetensors?sig=1"));
        assert_eq!(u.display(), "https://cdn.example.com/models/sam3/model.safetensors");
        let u = Url::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!((u.tls, u.port, u.path.as_str()), (false, 8080, "/"));
        let u = Url::parse("http://[::1]:9/a").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 9));
        for bad in ["ftp://x/y", "https://", "https://a b/", "https://u:p@h/", "http://h:0/", "http://h:99999/", "https://h/\r\nX: y", "file:///etc"]
        {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_text_after_a_bracketed_ipv6_host() {
        for bad in ["http://[::1]junk/x", "https://[::1]junk/x", "http://[::1]8080/x", "http://[::1]8080"] {
            assert!(matches!(Url::parse(bad), Err(HttpError::BadUrl(_))), "{bad}");
        }
        let u = Url::parse("http://[::1]:8080/x").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 8080));
        let u = Url::parse("http://[::1]/x").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 80));
    }

    #[test]
    fn resolves_redirects() {
        let u = Url::parse("https://a.example/x/y/file?q").unwrap();
        assert_eq!(u.join("https://b.example/z").unwrap().host, "b.example");
        assert_eq!(u.join("/root").unwrap().path, "/root");
        assert_eq!(u.join("other").unwrap().path, "/x/y/other");
        assert_eq!(u.join("//c.example/p").unwrap().host, "c.example");
        assert!(u.join("ftp://nope").is_err());
        // no downgrade from https to http, however the redirect is written
        assert!(u.join("http://a.example/z").is_err());
        let plain = Url::parse("http://127.0.0.1:8080/a").unwrap();
        assert!(!plain.join("http://127.0.0.1:9/b").unwrap().tls);
        assert!(plain.join("https://b.example/z").unwrap().tls);
    }

    #[test]
    fn the_tls_config_builds_with_the_pure_rust_provider() {
        assert!(tls_config().is_ok());
    }
}
