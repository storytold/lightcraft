//! A small HTTP/1.1 GET client for model downloads, in pure Rust: `std::net` sockets and, for
//! `https://`, rustls with the RustCrypto provider (`rustls-rustcrypto`; no `ring` or
//! `aws-lc-rs`, so nothing is compiled from C or assembly) and the Mozilla root certificates
//! (`webpki-roots`).
//!
//! Only what a large-file download needs: one request per connection (`Connection: close`),
//! `Range` requests for resuming, redirects (followed by the caller), `Content-Length`,
//! chunked and read-to-close bodies. Sockets are non-blocking: every read or write waits at most
//! `stall` for progress and checks the cancel flag every few milliseconds, so a dead server or a
//! cancelled download never hangs the downloading thread. No proxies (see docs/ai-masks.md).

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
    /// The server ended the secure connection with a fatal alert (it could not read what was sent, or refused it):
    /// it did not act on the request.
    TlsAlert(String),
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
            HttpError::TlsAlert(a) => write!(f, "the server closed the secure connection ({a})"),
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
            Some(p) if p.bytes().all(|b| b.is_ascii_digit()) => p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| bad("bad port"))?,
            Some(_) => return Err(bad("bad port")),
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

/// Longest pause between two looks at a socket that had nothing to read or no room to write (the cancel flag is
/// checked at each look).
const MAX_WAIT: Duration = Duration::from_millis(16);
/// Longest status line + headers accepted.
const MAX_HEAD: usize = 64 * 1024;

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::ClientConnection>, TcpStream),
}

/// A rustls error as an [`HttpError`]: an alert the server sent is [`HttpError::TlsAlert`].
fn tls_error(e: &rustls::Error) -> HttpError {
    match e {
        rustls::Error::AlertReceived(a) => HttpError::TlsAlert(format!("{a:?}")),
        e => HttpError::Tls(e.to_string()),
    }
}

/// An I/O error from rustls (it wraps its own errors as `InvalidData`).
fn io_tls_error(e: &std::io::Error) -> HttpError {
    match e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        Some(t) => tls_error(t),
        None => HttpError::Tls(e.to_string()),
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

/// Pauses between looks at a non-blocking socket: 1 ms, doubling up to [`MAX_WAIT`].
struct Backoff(Duration);

impl Backoff {
    fn new() -> Backoff {
        Backoff(Duration::from_millis(1))
    }

    fn pause(&mut self) {
        std::thread::sleep(self.0);
        self.0 = (self.0 * 2).min(MAX_WAIT);
    }
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
        match rx.recv_timeout(left.min(Duration::from_millis(250))) {
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
    fn connect(url: &Url, limits: &Limits, tls: Option<Arc<rustls::ClientConfig>>) -> Result<Stream, HttpError> {
        // the lookup and the connection attempts share one connect timeout
        let deadline = Instant::now() + limits.connect;
        let (host, port) = (url.host.clone(), url.port);
        let addrs = lookup(&url.host, move || (host.as_str(), port).to_socket_addrs().map(Iterator::collect), deadline, limits)?;
        let sock = connect_any(&url.host, &addrs, deadline, limits)?;
        // Non-blocking, not socket timeouts: on Windows a send that times out may already have sent part of its
        // data ("the socket state is indeterminate"), and sending it again corrupted the TLS stream: the server
        // answered "received fatal alert: BadRecordMac" (note 5). A non-blocking send that can't go out says
        // WouldBlock having sent nothing, so waiting and trying again is exact.
        sock.set_nonblocking(true).map_err(|e| HttpError::Io(e.to_string()))?;
        let _ = sock.set_nodelay(true);
        if !url.tls {
            return Ok(Stream::Plain(sock));
        }
        let name = rustls::pki_types::ServerName::try_from(url.host.clone()).map_err(|e| HttpError::Tls(format!("{}: {e}", url.host)))?;
        let config = match tls {
            Some(c) => c,
            None => tls_config()?,
        };
        let conn = rustls::ClientConnection::new(config, name).map_err(|e| HttpError::Tls(e.to_string()))?;
        let mut s = Stream::Tls(Box::new(conn), sock);
        s.handshake(limits)?;
        Ok(s)
    }

    /// Wait for `f` to make progress, retrying "not now" until `stall` passes without any.
    fn retry<T>(limits: &Limits, mut f: impl FnMut() -> std::io::Result<T>) -> Result<T, HttpError> {
        let start = Instant::now();
        let mut backoff = Backoff::new();
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
                    backoff.pause();
                }
                Err(e) if e.kind() == ErrorKind::InvalidData => return Err(io_tls_error(&e)),
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
                // rustls takes plaintext only up to its buffer limit (64 KiB by default): hand it what
                // it accepts, send that as TLS records, repeat. (`writer().write_all` of a larger
                // body failed with "failed to write whole buffer": the generative-fill upload.)
                let mut rest = data;
                while !rest.is_empty() {
                    let n = conn.writer().write(rest).map_err(|e| HttpError::Tls(e.to_string()))?;
                    rest = rest.get(n..).unwrap_or_default();
                    if n == 0 && !conn.wants_write() {
                        return Err(HttpError::Io("the secure connection accepted no more data".into()));
                    }
                    while conn.wants_write() {
                        let sent = Self::retry(limits, || conn.write_tls(sock))?;
                        if sent == 0 {
                            return Err(HttpError::Io("connection closed while sending the request".into()));
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// After a failed send: the alert the server sent before it closed, if one is waiting (it says why).
    fn alert(&mut self) -> Option<HttpError> {
        let Stream::Tls(conn, sock) = self else { return None };
        for _ in 0..8 {
            match conn.read_tls(sock) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {
                    if let Err(e) = conn.process_new_packets() {
                        return Some(tls_error(&e)).filter(|e| matches!(e, HttpError::TlsAlert(_)));
                    }
                }
            }
        }
        None
    }

    /// Read some bytes (0 at the end of the stream), waiting at most `stall` for them.
    fn read(&mut self, buf: &mut [u8], limits: &Limits) -> Result<usize, HttpError> {
        match self {
            Stream::Plain(sock) => Self::retry(limits, || sock.read(buf)),
            Stream::Tls(conn, sock) => {
                let start = Instant::now();
                let mut backoff = Backoff::new();
                loop {
                    if limits.cancel.load(Ordering::Relaxed) {
                        return Err(HttpError::Cancelled);
                    }
                    match conn.reader().read(buf) {
                        Ok(n) => return Ok(n),
                        // closed without close_notify: an attacker on the path can cut a TLS stream at will, so this is
                        // never a clean end (a body whose length is known is complete before any further read)
                        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Err(HttpError::Tls(TRUNCATED.into())),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                        Err(e) => return Err(io_tls_error(&e)),
                    }
                    // no plaintext buffered: more TLS records from the socket
                    match conn.read_tls(sock) {
                        Ok(0) => return Err(HttpError::Tls(TRUNCATED.into())),
                        Ok(_) => {
                            conn.process_new_packets().map_err(|e| tls_error(&e))?;
                            while conn.wants_write() {
                                Self::retry(limits, || conn.write_tls(sock))?;
                            }
                        }
                        Err(e) if is_timeout(&e) => {
                            if start.elapsed() >= limits.stall {
                                return Err(HttpError::Stalled);
                            }
                            backoff.pause();
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

    /// The whole body, at most `max` bytes (a longer one is an error, not a truncated body).
    pub fn read_to_end(&mut self, max: usize, limits: &Limits) -> Result<Vec<u8>, HttpError> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = self.read(&mut buf, limits)?;
            if n == 0 {
                return Ok(out);
            }
            if out.len().saturating_add(n) > max {
                return Err(HttpError::Protocol(format!("response over {} MB", max >> 20)));
            }
            out.extend_from_slice(buf.get(..n).unwrap_or_default());
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
                    let size = if hex.bytes().all(|b| b.is_ascii_hexdigit()) { u64::from_str_radix(hex, 16).ok() } else { None }
                        .ok_or_else(|| HttpError::Protocol(format!("bad chunk size `{hex}`")))?;
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
    request("GET", url, headers, None, limits)
}

/// Largest request body [`request`] sends (a generative-fill upload is a few MB).
pub const MAX_BODY: usize = 64 * 1024 * 1024;

/// Send `method url` with extra `headers` and, for `Some(body)`, that body (with its
/// `Content-Length`; the caller names its `Content-Type` in `headers`), then read the status
/// line and headers. Header values never appear in errors (they may hold an API key).
pub fn request(method: &str, url: &Url, headers: &[(&str, String)], body: Option<&[u8]>, limits: &Limits) -> Result<Response, HttpError> {
    request_with(method, url, headers, body, limits, None).map_err(|(e, _)| e)
}

const TRUNCATED: &str = "the connection closed without a TLS close_notify: the response may be truncated";

/// How far a failed request got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Before anything was sent (the URL, the name lookup, connecting, the TLS handshake).
    Connect,
    /// While the request was being sent.
    Send,
    /// After it was sent, before the status line and headers arrived.
    Receive,
}

/// [`request`], saying how far a failure got (a caller decides from it whether sending again is safe).
pub fn request_staged(
    method: &str,
    url: &Url,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
    limits: &Limits,
) -> Result<Response, (HttpError, Stage)> {
    request_with(method, url, headers, body, limits, None)
}

/// Whether a request that failed with `e` at `stage` certainly did not reach the server's application, so
/// sending it again cannot do anything twice (a paid generation): it never left this machine, it broke off
/// while being sent, or the server's TLS layer rejected it with an alert before any response.
/// A request whose sending failed never reached the server's application either: it acts on a whole request,
/// and the connection broke before all of it went out.
pub fn not_delivered(e: &HttpError, stage: Stage) -> bool {
    match e {
        HttpError::Cancelled | HttpError::BadUrl(_) => false,
        HttpError::TlsAlert(_) => true,
        _ => matches!(stage, Stage::Connect | Stage::Send),
    }
}

/// Whether `e` is a network failure worth one more try on a new connection (not a refusal or a cancel).
pub fn transient(e: &HttpError) -> bool {
    matches!(e, HttpError::Connect(_) | HttpError::Stalled | HttpError::Tls(_) | HttpError::TlsAlert(_) | HttpError::Io(_) | HttpError::Protocol(_))
}

/// [`request`] with its own TLS configuration (tests: a local server's test certificate).
fn request_with(
    method: &str,
    url: &Url,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
    limits: &Limits,
    tls: Option<Arc<rustls::ClientConfig>>,
) -> Result<Response, (HttpError, Stage)> {
    let at = |stage: Stage| move |e: HttpError| (e, stage);
    if method.is_empty() || !method.chars().all(|c| c.is_ascii_uppercase()) {
        return Err((HttpError::BadUrl("bad request method".into()), Stage::Connect));
    }
    // `path` and `host` are public fields: nothing that could end the request line or a header may reach the wire
    let plain = |s: &str| !s.bytes().any(|b| b <= 0x20 || b == 0x7f);
    if !url.path.starts_with('/') || !plain(&url.path) || !plain(&url.host) {
        return Err((HttpError::BadUrl("a space or control character in the host or path".into()), Stage::Connect));
    }
    if body.is_some_and(|b| b.len() > MAX_BODY) {
        return Err((HttpError::BadUrl(format!("request body over {} MB", MAX_BODY >> 20)), Stage::Connect));
    }
    let mut req = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}/{}\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        url.path,
        url.host_header(),
        lcc_brand::BINARY,
        env!("CARGO_PKG_VERSION")
    );
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept")) {
        req.push_str("Accept: */*\r\n");
    }
    for (k, v) in headers {
        if v.chars().any(|c| c == '\r' || c == '\n') || k.chars().any(|c| c == '\r' || c == '\n' || c == ':') {
            return Err((HttpError::BadUrl("header with a line break".into()), Stage::Connect));
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    let mut stream = Stream::connect(url, limits, tls).map_err(at(Stage::Connect))?;
    let mut sent = stream.write_all(req.as_bytes(), limits);
    if let (Ok(()), Some(b)) = (&sent, body) {
        for chunk in b.chunks(256 * 1024) {
            sent = stream.write_all(chunk, limits);
            if sent.is_err() {
                break;
            }
        }
    }
    if let Err(e) = sent {
        // a server that could not read what arrived says so with an alert before it closes
        return Err((if e == HttpError::Cancelled { e } else { stream.alert().unwrap_or(e) }, Stage::Send));
    }
    read_head(Conn { stream, buf: Vec::new(), pos: 0 }, limits).map_err(at(Stage::Receive))
}

/// The status line and headers of the reply on `conn` (interim 1xx replies skipped).
fn read_head(mut conn: Conn, limits: &Limits) -> Result<Response, HttpError> {
    let (status, headers) = loop {
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
        if (100..200).contains(&status) && status != 101 {
            continue;
        }
        break (status, headers);
    };
    let find = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
    let body = if status == 204 || status == 304 || (100..200).contains(&status) {
        BodyMode::Done
    } else if find("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        BodyMode::Chunked { left: 0, done: false }
    } else if let Some(n) = find("content-length") {
        // RFC 9110: `1*DIGIT`; `u64::from_str` alone would also take a leading '+'
        let v = n.trim();
        let len = if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) { v.parse::<u64>().ok() } else { None };
        BodyMode::Length(len.ok_or_else(|| HttpError::Protocol(format!("bad Content-Length `{n}`")))?)
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

    /// A `Url` built by hand cannot put a line break or space into the request line or the Host header.
    #[test]
    fn a_path_or_host_with_a_line_break_is_refused_before_connecting() {
        let cancel = AtomicBool::new(false);
        for (host, path) in [("example.com", "/a b"), ("example.com", "/a\r\nX: y"), ("example.com", "a"), ("ex\r\nample", "/")] {
            let url = Url { tls: false, host: host.into(), port: 9, path: path.into() };
            assert!(matches!(get(&url, &[], &limits(&cancel)), Err(HttpError::BadUrl(_))), "{host:?} {path:?}");
        }
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

    /// Content-Length is `1*DIGIT`: a sign, hex prefix, trailing junk or empty value is refused, not read as a length.
    #[test]
    fn signed_or_malformed_content_length_is_refused() {
        for value in ["+3", "-3", "3x", "", "0x10", "+0"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {value}\r\n\r\nabc").as_bytes());
            });
            let cancel = AtomicBool::new(false);
            let url = Url::parse(&format!("http://127.0.0.1:{port}/f")).unwrap();
            match get(&url, &[], &limits(&cancel)) {
                Err(HttpError::Protocol(m)) => assert!(m.contains("bad Content-Length"), "{value:?}: {m}"),
                Err(e) => panic!("{value:?}: unexpected error {e:?}"),
                Ok(r) => panic!("{value:?}: malformed Content-Length accepted (status {})", r.status),
            }
            server.join().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse_and_hostile_ones_are_refused() {
        // parses urls
        {
            let u = Url::parse("https://cdn.example.com/models/sam3/model.safetensors?sig=1#x").unwrap();
            assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (true, "cdn.example.com", 443, "/models/sam3/model.safetensors?sig=1"));
            assert_eq!(u.display(), "https://cdn.example.com/models/sam3/model.safetensors");
            let u = Url::parse("http://127.0.0.1:8080").unwrap();
            assert_eq!((u.tls, u.port, u.path.as_str()), (false, 8080, "/"));
            let u = Url::parse("http://[::1]:9/a").unwrap();
            assert_eq!((u.host.as_str(), u.port), ("::1", 9));
            for bad in
                ["ftp://x/y", "https://", "https://a b/", "https://u:p@h/", "http://h:0/", "http://h:99999/", "https://h/\r\nX: y", "file:///etc"]
            {
                assert!(Url::parse(bad).is_err(), "{bad}");
            }
        }
        // url scheme is matched case insensitively
        {
            let u = Url::parse("HTTPS://A.example/z").unwrap();
            assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (true, "a.example", 443, "/z"));
            assert!(Url::parse("Http://a.example/").is_ok());
            let j = Url::parse("https://a.example/start").unwrap().join("HTTPS://cdn.example/file").unwrap();
            assert_eq!((j.tls, j.host.as_str(), j.path.as_str()), (true, "cdn.example", "/file"));
        }
        // rejects oversized and control character urls
        {
            assert!(Url::parse(&format!("https://example.com/{}", "x".repeat(16 * 1024))).is_err());
            assert!(Url::parse("https://example.com/x?secret=abc\r\nInjected: 1").is_err());
        }
        // rejects signed ports
        {
            for bad in ["http://example.com:+80/x", "http://example.com:-80/x", "http://[::1]:+80/x"] {
                assert!(Url::parse(bad).is_err(), "{bad}");
            }
            assert!(Url::parse("http://a.test/").unwrap().join("http://example.com:+80/x").is_err());
            assert_eq!(Url::parse("http://example.com:80/x").unwrap().port, 80);
        }
        // rejects text after a bracketed ipv6 host
        {
            for bad in ["http://[::1]junk/x", "https://[::1]junk/x", "http://[::1]8080/x", "http://[::1]8080"] {
                assert!(matches!(Url::parse(bad), Err(HttpError::BadUrl(_))), "{bad}");
            }
            let u = Url::parse("http://[::1]:8080/x").unwrap();
            assert_eq!((u.host.as_str(), u.port), ("::1", 8080));
            let u = Url::parse("http://[::1]/x").unwrap();
            assert_eq!((u.host.as_str(), u.port), ("::1", 80));
        }
    }

    #[test]
    fn redirects_resolve_against_the_current_url() {
        // resolves redirects
        {
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
        // join removes dot segments
        {
            let base = Url::parse("https://a.example/x/y/file").unwrap();
            for (loc, want) in
                [("../z", "/x/z"), ("./z", "/x/y/z"), ("../../../z", "/z"), ("/a/./b/../c", "/a/c"), ("../z?q=1", "/x/z?q=1"), ("..", "/x/")]
            {
                assert_eq!(base.join(loc).unwrap().path, want, "{loc}");
            }
        }
        // join query only keeps file name
        {
            let u = Url::parse("https://a.example/x/y/file?q=1").unwrap();
            assert_eq!(u.join("?page=2").unwrap().path, "/x/y/file?page=2");
        }
        // fragment only or empty location keeps file name and query
        {
            let base = Url::parse("https://a.example/x/y/file?q").unwrap();
            for l in ["#top", "", "  "] {
                let u = base.join(l).unwrap();
                assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (true, "a.example", 443, "/x/y/file?q"), "{l:?}");
            }
        }
    }

    /// A POST reaches a local server with its headers and whole body, and the reply is read back.
    #[test]
    fn posts_a_body_and_reads_the_reply() {
        use std::io::{BufRead, BufReader};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}/v1/edit", l.local_addr().unwrap())).unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            let (mut len, mut auth) = (0usize, String::new());
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap();
                }
                if k.eq_ignore_ascii_case("authorization") {
                    auth = v.trim().to_string();
                }
            }
            let mut body = vec![0u8; len];
            std::io::Read::read_exact(&mut r, &mut body).unwrap();
            let reply = format!("{{\"method\":\"{}\",\"len\":{len},\"auth\":\"{auth}\"}}", first.split(' ').next().unwrap());
            let mut s = s;
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).unwrap();
        });
        let cancel = AtomicBool::new(false);
        let limits = Limits { connect: Duration::from_secs(5), stall: Duration::from_secs(5), cancel: &cancel };
        let body = vec![7u8; 700_000];
        let mut r = request("POST", &url, &[("Authorization", "Bearer k".into()), ("Content-Type", "application/json".into())], Some(&body), &limits)
            .unwrap();
        assert_eq!(r.status, 200);
        let got = String::from_utf8(r.read_to_end(1 << 20, &limits).unwrap()).unwrap();
        assert_eq!(got, "{\"method\":\"POST\",\"len\":700000,\"auth\":\"Bearer k\"}");
        server.join().unwrap();
        assert!(request("post", &url, &[], None, &limits).is_err());
    }

    /// A test CA and a `localhost` certificate it signed (P-256, valid until 2126), for a local TLS
    /// server. Test-only keys, generated for this test.
    const TEST_CA: &str = "-----BEGIN CERTIFICATE-----
MIIBfzCCASagAwIBAgIUZDE/pE57rmhd0acTEfrqEISjuU4wCgYIKoZIzj0EAwIw
HTEbMBkGA1UEAwwSTGlnaHRDcmFmdCB0ZXN0IENBMCAXDTI2MTAxNTE1NDM1OFoY
DzIxMjYwOTIxMTU0MzU4WjAdMRswGQYDVQQDDBJMaWdodENyYWZ0IHRlc3QgQ0Ew
WTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASqLIAm3w+FWLuqWtaHSChon1F4wgWM
vk9LWH6M/Nv8NIGDxQs3+4Wyc+A0ecQMBD+yDUi4DYE81P6JzmL4teSyo0IwQDAP
BgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwIBBjAdBgNVHQ4EFgQUnYLuIFBi
SxKnhbV9TVwVQTuK6PUwCgYIKoZIzj0EAwIDRwAwRAIgVGQ//6kzxXKKjW19kq7J
CS9ECb+Ga7YSgTbfkL1iEb0CIErq01/VeRxRH7b8KDd1dIo7fYKWqxHjHy9tbD+I
Jqfr
-----END CERTIFICATE-----";
    const TEST_LEAF: &str = "-----BEGIN CERTIFICATE-----
MIIBuzCCAWKgAwIBAgIUbzPZ8rsInlnhpceWrVi3Y9nh9X8wCgYIKoZIzj0EAwIw
HTEbMBkGA1UEAwwSTGlnaHRDcmFmdCB0ZXN0IENBMCAXDTI2MTAxNTE1NDM1OFoY
DzIxMjYwOTIxMTU0MzU4WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjO
PQIBBggqhkjOPQMBBwNCAATfwLLDsrJ+pxUuCMLz7sTkEYam3ozL8HKpaUfs1IO9
3F/lJ7ksXCTdQsQ0QBmX6YThT4t3MerVJemDhM6GhmAjo4GGMIGDMAkGA1UdEwQC
MAAwCwYDVR0PBAQDAgeAMBMGA1UdJQQMMAoGCCsGAQUFBwMBMBQGA1UdEQQNMAuC
CWxvY2FsaG9zdDAdBgNVHQ4EFgQUk6UoMz3BYK3wFQ1ce8E3i25A0LQwHwYDVR0j
BBgwFoAUnYLuIFBiSxKnhbV9TVwVQTuK6PUwCgYIKoZIzj0EAwIDRwAwRAIgT3Qr
YnGdEvkqa1BtPbIjpHVOCFZqRoMyH2Px8e7wsE0CIE21rOdrZA0F67jx3GiWZoZq
9s21ti7TrXHqy7sPWIl+
-----END CERTIFICATE-----";
    /// The leaf's PKCS#8 key, base64 (no PEM armour: it is a throwaway test key).
    const TEST_LEAF_KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgL88PP3vSOLZwBjDA\
sQ6l5HDDBbhGkYuLCq/Hs2w/8suhRANCAATfwLLDsrJ+pxUuCMLz7sTkEYam3ozL\
8HKpaUfs1IO93F/lJ7ksXCTdQsQ0QBmX6YThT4t3MerVJemDhM6GhmAj";

    fn b64(s: &str) -> Vec<u8> {
        let val = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        };
        let digits: Vec<u8> = s.bytes().filter(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/').map(val).collect();
        let mut out = Vec::new();
        for q in digits.chunks(4) {
            let n = q.iter().enumerate().fold(0u32, |a, (i, d)| a | (u32::from(*d) << (18 - 6 * i)));
            out.extend(n.to_be_bytes().iter().skip(1).take(q.len() - 1));
        }
        out
    }

    fn pem_der(pem: &str) -> Vec<u8> {
        b64(&pem.lines().filter(|l| !l.starts_with("-----")).collect::<String>())
    }

    /// A body far over rustls's 64 KiB plaintext buffer goes out whole over TLS (it failed with
    /// "failed to write whole buffer": the first real generative-fill upload).
    #[test]
    fn posts_a_large_body_over_tls() {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let provider = Arc::new(rustls_rustcrypto::provider());
        let server_cfg = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(pem_der(TEST_LEAF)), CertificateDer::from(pem_der(TEST_CA))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(b64(TEST_LEAF_KEY))),
            )
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(pem_der(TEST_CA))).unwrap();
        let client_cfg = Arc::new(
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let (sock, _) = l.accept().unwrap();
            let conn = rustls::ServerConnection::new(Arc::new(server_cfg)).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, sock);
            let mut r = BufReader::new(&mut tls);
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            let sum: u64 = body.iter().enumerate().map(|(i, b)| (i as u64 % 251) * u64::from(*b)).sum();
            let reply = format!("{{\"len\":{len},\"sum\":{sum}}}");
            write!(tls, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            tls.flush().unwrap();
            tls.conn.send_close_notify();
            let _ = tls.flush();
        });
        let cancel = AtomicBool::new(false);
        let limits = Limits { connect: Duration::from_secs(5), stall: Duration::from_secs(20), cancel: &cancel };
        // 6 MB, about what a 1536 × 1024 crop and its mask come to as PNGs
        let body: Vec<u8> = (0..6_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        let sum: u64 = body.iter().enumerate().map(|(i, b)| (i as u64 % 251) * u64::from(*b)).sum();
        let url = Url::parse(&format!("https://localhost:{port}/v1/images/edits")).unwrap();
        let mut r =
            request_with("POST", &url, &[("Content-Type", "application/octet-stream".into())], Some(&body), &limits, Some(client_cfg)).ok().unwrap();
        assert_eq!(r.status, 200);
        let got = String::from_utf8(r.read_to_end(1 << 20, &limits).unwrap()).unwrap();
        assert_eq!(got, format!("{{\"len\":6000000,\"sum\":{sum}}}"));
        server.join().unwrap();
    }

    /// The test server's and client's TLS configurations (the client trusts only the test CA).
    fn test_tls() -> (rustls::ServerConfig, Arc<rustls::ClientConfig>) {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let provider = Arc::new(rustls_rustcrypto::provider());
        let server_cfg = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(pem_der(TEST_LEAF)), CertificateDer::from(pem_der(TEST_CA))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(b64(TEST_LEAF_KEY))),
            )
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(pem_der(TEST_CA))).unwrap();
        let client_cfg = Arc::new(
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        (server_cfg, client_cfg)
    }

    /// A server that reads the upload slowly (a slow uplink: the client's sends wait longer than one cancel tick)
    /// still gets every byte intact. On Windows a socket send that times out may already have sent part of its
    /// data ("the socket state is indeterminate"), so sending it again corrupted the TLS record stream and the
    /// server answered "received fatal alert: BadRecordMac" (note 5, the second generative-fill request).
    #[test]
    fn a_slow_reading_server_gets_the_whole_upload_over_tls() {
        let (server_cfg, client_cfg) = test_tls();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || -> Result<String, String> {
            let (mut sock, _) = l.accept().unwrap();
            let mut conn = rustls::ServerConnection::new(Arc::new(server_cfg)).unwrap();
            let mut plain = Vec::new();
            let mut buf = [0u8; 4096];
            let mut want: Option<usize> = None;
            let mut reads = 0u32;
            let end = |p: &[u8]| p.windows(4).position(|w| w == b"\r\n\r\n");
            loop {
                if conn.wants_write() {
                    conn.write_tls(&mut sock).map_err(|e| e.to_string())?;
                    continue;
                }
                let n = conn.read_tls(&mut sock).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Err("client closed".into());
                }
                // read in bites with pauses: the client's send buffer fills and its sends wait
                reads += 1;
                if reads.is_multiple_of(48) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                conn.process_new_packets().map_err(|e| format!("server: {e}"))?;
                while let Ok(k) = conn.reader().read(&mut buf) {
                    if k == 0 {
                        break;
                    }
                    plain.extend_from_slice(&buf[..k]);
                }
                if want.is_none()
                    && let Some(i) = end(&plain)
                {
                    let head = String::from_utf8_lossy(&plain[..i]).to_string();
                    let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().trim().parse().unwrap();
                    want = Some(i + 4 + len);
                }
                if want.is_some_and(|w| plain.len() >= w) {
                    break;
                }
            }
            let body = &plain[end(&plain).unwrap() + 4..];
            let sum: u64 = body.iter().enumerate().map(|(i, b)| (i as u64 % 251) * u64::from(*b)).sum();
            let reply = format!("{{\"len\":{},\"sum\":{sum}}}", body.len());
            let mut tls = rustls::StreamOwned::new(conn, sock);
            write!(tls, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            tls.flush().unwrap();
            Ok(reply)
        });
        let cancel = AtomicBool::new(false);
        let limits = Limits { connect: Duration::from_secs(5), stall: Duration::from_secs(30), cancel: &cancel };
        let body: Vec<u8> = (0..12_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        let sum: u64 = body.iter().enumerate().map(|(i, b)| (i as u64 % 251) * u64::from(*b)).sum();
        let url = Url::parse(&format!("https://localhost:{port}/v1/flux-tools/erase-v1")).unwrap();
        let r =
            request_with("POST", &url, &[("Content-Type", "application/json".into())], Some(&body), &limits, Some(client_cfg)).map_err(|(e, _)| e);
        let served = server.join().unwrap();
        assert_eq!(served, Ok(format!("{{\"len\":12000000,\"sum\":{sum}}}")));
        let mut r = r.unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(String::from_utf8(r.read_to_end(1 << 20, &limits).unwrap()).unwrap(), served.unwrap());
    }

    /// A server whose TLS layer can't read what arrived answers with a fatal alert: that is reported as the
    /// server closing the secure connection (BadRecordMac), and as a request it never acted on, so sending it
    /// again is safe. A failure after the request went out is not.
    #[test]
    fn a_server_alert_means_the_request_was_not_delivered() {
        let (server_cfg, client_cfg) = test_tls();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = l.accept().unwrap();
            let mut conn = rustls::ServerConnection::new(Arc::new(server_cfg)).unwrap();
            let mut seen = 0usize;
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                while conn.wants_write() {
                    conn.write_tls(&mut sock).unwrap();
                }
                let n = sock.read(&mut buf).unwrap();
                if n == 0 {
                    return;
                }
                // a byte damaged on the way, well after the handshake
                if !conn.is_handshaking() && seen + n > 40_000 && seen <= 40_000 {
                    let i = 40_000 - seen;
                    buf[i.min(n - 1)] ^= 0x5a;
                }
                seen += n;
                conn.read_tls(&mut &buf[..n]).unwrap();
                if conn.process_new_packets().is_err() {
                    // the alert goes out, then the server stops sending and drains what still arrives
                    let _ = conn.write_tls(&mut sock);
                    let _ = sock.shutdown(std::net::Shutdown::Write);
                    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    while sock.read(&mut buf).is_ok_and(|n| n > 0) {}
                    return;
                }
                let mut sink = [0u8; 16 * 1024];
                while let Ok(k) = conn.reader().read(&mut sink) {
                    if k == 0 {
                        break;
                    }
                }
            }
        });
        let cancel = AtomicBool::new(false);
        let limits = Limits { connect: Duration::from_secs(5), stall: Duration::from_secs(10), cancel: &cancel };
        let url = Url::parse(&format!("https://localhost:{port}/v1/flux-tools/erase-v1")).unwrap();
        let body = vec![b'x'; 400_000];
        let Err((e, stage)) = request_with("POST", &url, &[], Some(&body), &limits, Some(client_cfg)) else { panic!("the server refused it") };
        server.join().unwrap();
        assert!(matches!(&e, HttpError::TlsAlert(a) if a.contains("BadRecordMac")), "{e:?} at {stage:?}");
        assert_eq!(e.to_string(), "the server closed the secure connection (BadRecordMac)");
        assert!(not_delivered(&e, stage) && transient(&e));
        // a connection that broke after the request was sent may have been acted on
        assert!(!not_delivered(&HttpError::Io("connection reset".into()), Stage::Receive));
        assert!(not_delivered(&HttpError::Io("connection reset".into()), Stage::Send));
        assert!(not_delivered(&HttpError::Connect("refused".into()), Stage::Connect));
        assert!(!transient(&HttpError::Cancelled) && !not_delivered(&HttpError::Cancelled, Stage::Connect));
    }

    #[test]
    fn the_tls_config_builds_with_the_pure_rust_provider() {
        assert!(tls_config().is_ok());
    }
}
