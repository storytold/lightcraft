//! One connection: TCP (with connect and stall timeouts and a cancel flag), optionally through an
//! HTTP proxy, optionally wrapped in TLS.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::{NetError, Url};

/// How often a blocked read or write wakes up to look at the cancel flag.
const POLL: Duration = Duration::from_millis(250);

/// A TCP stream whose reads and writes only return data or a real error: they wait through
/// socket timeouts, fail with [`NetError::Stalled`] after `stall` without progress and with
/// [`NetError::Cancelled`] once the flag is raised.
pub(crate) struct TimedTcp {
    tcp: TcpStream,
    stall: Duration,
    cancel: Option<Arc<AtomicBool>>,
}

impl TimedTcp {
    fn check_cancel(&self) -> std::io::Result<()> {
        match &self.cancel {
            Some(c) if c.load(Ordering::Relaxed) => Err(NetError::Cancelled.into()),
            _ => Ok(()),
        }
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

impl Read for TimedTcp {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let start = Instant::now();
        loop {
            self.check_cancel()?;
            match self.tcp.read(buf) {
                Err(e) if is_timeout(&e) => {
                    if start.elapsed() >= self.stall {
                        return Err(NetError::Stalled.into());
                    }
                }
                other => return other,
            }
        }
    }
}

impl Write for TimedTcp {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let start = Instant::now();
        loop {
            self.check_cancel()?;
            match self.tcp.write(buf) {
                Err(e) if is_timeout(&e) => {
                    if start.elapsed() >= self.stall {
                        return Err(NetError::Stalled.into());
                    }
                }
                other => return other,
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.tcp.flush()
    }
}

/// A plain or TLS connection.
pub(crate) enum Conn {
    Plain(TimedTcp),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TimedTcp>>),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(buf),
            Conn::Tls(s) => match s.read(buf) {
                // servers that close without close_notify: treat as end of stream (the body
                // framing, Content-Length or chunked, still catches truncation)
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => Ok(0),
                other => other,
            },
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(buf),
            Conn::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

pub(crate) struct ConnectOptions<'a> {
    pub connect_timeout: Duration,
    pub stall: Duration,
    pub cancel: Option<Arc<AtomicBool>>,
    pub proxy: Option<&'a Url>,
    pub tls: Option<Arc<rustls::ClientConfig>>,
}

fn tcp_connect(host: &str, port: u16, timeout: Duration, cancel: &Option<Arc<AtomicBool>>) -> Result<TcpStream, NetError> {
    let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|e| NetError::Connect(format!("{host}: {e}")))?.collect();
    let mut last = format!("{host}: no address");
    for addr in addrs {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(NetError::Cancelled);
        }
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("{host} ({addr}): {e}"),
        }
    }
    Err(NetError::Connect(last))
}

/// Open a connection to `url`'s origin. Through a proxy, `http://` targets talk to the proxy
/// directly (the request line then carries the absolute URL) and `https://` targets are tunnelled
/// with `CONNECT`.
pub(crate) fn connect(url: &Url, opts: ConnectOptions<'_>) -> Result<Conn, NetError> {
    let (host, port) = match opts.proxy {
        Some(p) => (p.host.as_str(), p.port),
        None => (url.host.as_str(), url.port),
    };
    let tcp = tcp_connect(host, port, opts.connect_timeout, &opts.cancel)?;
    let _ = tcp.set_nodelay(true);
    tcp.set_read_timeout(Some(POLL)).map_err(|e| NetError::Io(e.to_string()))?;
    tcp.set_write_timeout(Some(POLL)).map_err(|e| NetError::Io(e.to_string()))?;
    let mut timed = TimedTcp { tcp, stall: opts.stall, cancel: opts.cancel };
    if opts.proxy.is_some() && url.tls {
        tunnel(&mut timed, url)?;
    }
    let Some(config) = opts.tls.filter(|_| url.tls) else {
        return Ok(Conn::Plain(timed));
    };
    let name = rustls::pki_types::ServerName::try_from(url.host.clone()).map_err(|e| NetError::Tls(format!("{}: {e}", url.host)))?;
    let mut conn = rustls::ClientConnection::new(config, name).map_err(|e| NetError::Tls(e.to_string()))?;
    // finish the handshake now so certificate errors surface as such
    while conn.is_handshaking() {
        conn.complete_io(&mut timed).map_err(tls_error)?;
    }
    Ok(Conn::Tls(Box::new(rustls::StreamOwned::new(conn, timed))))
}

/// Map a handshake I/O error, unwrapping our own errors (untrusted certificate, stall, cancel).
fn tls_error(e: std::io::Error) -> NetError {
    if let Some(inner) = e.get_ref() {
        if let Some(n) = inner.downcast_ref::<NetError>() {
            return n.clone();
        }
        if let Some(rustls::Error::InvalidCertificate(rustls::CertificateError::Other(o))) = inner.downcast_ref::<rustls::Error>()
            && let Some(n) = o.0.downcast_ref::<NetError>()
        {
            return n.clone();
        }
        if let Some(rustls::Error::Other(o)) = inner.downcast_ref::<rustls::Error>()
            && let Some(n) = o.0.downcast_ref::<NetError>()
        {
            return n.clone();
        }
    }
    NetError::Tls(e.to_string())
}

/// `CONNECT host:port` through the proxy; any 2xx opens the tunnel.
fn tunnel(s: &mut TimedTcp, url: &Url) -> Result<(), NetError> {
    let target = if url.host.contains(':') { format!("[{}]:{}", url.host, url.port) } else { format!("{}:{}", url.host, url.port) };
    let req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nUser-Agent: {}\r\n\r\n", crate::user_agent());
    s.write_all(req.as_bytes())?;
    let head = crate::http::read_head(s)?;
    if !(200..300).contains(&head.status) {
        return Err(NetError::Connect(format!("proxy refused the tunnel: {} {}", head.status, head.reason)));
    }
    Ok(())
}
