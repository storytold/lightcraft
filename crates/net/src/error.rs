//! The one error type of the crate.

/// Why a request failed. Messages never contain header values, bodies or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// The URL can't be used (scheme, host, characters).
    BadUrl(String),
    /// A header name or value can't be sent (line breaks, colons in names).
    BadHeader(String),
    /// Name resolution or connecting failed, or took longer than the connect timeout.
    Connect(String),
    /// No data moved for the stall timeout.
    Stalled,
    /// The cancel flag was raised.
    Cancelled,
    /// TLS failed (handshake, protocol).
    Tls(String),
    /// The server's certificate is not trusted and nobody accepted it (no trust-on-first-use hook,
    /// or the hook declined). Carries the SHA-256 fingerprint so the UI can show it.
    UntrustedCertificate { host: String, fingerprint: String },
    /// The server's response could not be understood.
    Protocol(String),
    /// More redirects than allowed, or a redirect that a non-replayable body can't follow.
    Redirect(String),
    /// The response (or a file) is larger than the allowed size.
    TooLarge(u64),
    /// The server answered with an error status ([`crate::Response::error_for_status`]).
    Status { code: u16, reason: String },
    /// A JSON body could not be written or read.
    Json(String),
    /// A downloaded file does not match its expected size or SHA-256.
    Verify(String),
    /// Any other I/O error (local files included).
    Io(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::BadUrl(e) => write!(f, "unusable URL: {e}"),
            NetError::BadHeader(e) => write!(f, "unusable header: {e}"),
            NetError::Connect(e) => write!(f, "could not connect: {e}"),
            NetError::Stalled => write!(f, "the server stopped responding"),
            NetError::Cancelled => write!(f, "cancelled"),
            NetError::Tls(e) => write!(f, "secure connection failed: {e}"),
            NetError::UntrustedCertificate { host, fingerprint } => {
                write!(f, "the certificate of {host} is not trusted (SHA-256 fingerprint {fingerprint})")
            }
            NetError::Protocol(e) => write!(f, "unexpected response: {e}"),
            NetError::Redirect(e) => write!(f, "redirect not followed: {e}"),
            NetError::TooLarge(n) => write!(f, "larger than the allowed {n} bytes"),
            NetError::Status { code, reason } => write!(f, "server answered {code} {reason}"),
            NetError::Json(e) => write!(f, "JSON: {e}"),
            NetError::Verify(e) => write!(f, "verification failed: {e}"),
            NetError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        // the stream wrappers carry our own errors through io::Error (rustls, flate2)
        if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<NetError>()) {
            return inner.clone();
        }
        NetError::Io(e.to_string())
    }
}

impl From<NetError> for std::io::Error {
    fn from(e: NetError) -> Self {
        // never `Interrupted`: std retries those, which would swallow a cancel
        let kind = if e == NetError::Stalled { std::io::ErrorKind::TimedOut } else { std::io::ErrorKind::Other };
        std::io::Error::new(kind, e)
    }
}
