//! Why a call to an Immich server failed, in words a user can act on. Messages never contain the
//! API key, request bodies or query strings.

use dac_net::NetError;

use crate::types::{MIN_VERSION, ServerVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImmichError {
    /// The URL is not usable.
    BadUrl(String),
    /// The server can't be reached (offline, wrong host or port, timed out).
    Offline(String),
    /// The server's certificate is not trusted: show the fingerprint and ask.
    Untrusted {
        host: String,
        fingerprint: String,
    },
    /// TLS failed for another reason.
    Tls(String),
    /// The API key was refused (401).
    BadKey,
    /// The key lacks a permission (403).
    Forbidden(String),
    /// Not found (404).
    NotFound(String),
    /// The server is older than [`MIN_VERSION`].
    TooOld(ServerVersion),
    /// The server failed (5xx).
    Server(u16),
    /// Another HTTP status.
    Status(u16, String),
    /// The answer could not be understood (not an Immich server, or an incompatible one).
    Protocol(String),
    Cancelled,
    /// A local file could not be read or written.
    Io(String),
    /// No API key stored for the account.
    NoKey,
}

impl ImmichError {
    /// Worth trying again later without changing anything (the retry button / the next pass).
    pub fn retryable(&self) -> bool {
        matches!(self, ImmichError::Offline(_) | ImmichError::Server(_))
    }

    /// A short machine-readable kind, for the control channel and MCP.
    pub fn kind(&self) -> &'static str {
        match self {
            ImmichError::BadUrl(_) => "badUrl",
            ImmichError::Offline(_) => "offline",
            ImmichError::Untrusted { .. } => "untrustedCertificate",
            ImmichError::Tls(_) => "tls",
            ImmichError::BadKey => "badKey",
            ImmichError::Forbidden(_) => "forbidden",
            ImmichError::NotFound(_) => "notFound",
            ImmichError::TooOld(_) => "tooOld",
            ImmichError::Server(_) => "server",
            ImmichError::Status(..) => "status",
            ImmichError::Protocol(_) => "protocol",
            ImmichError::Cancelled => "cancelled",
            ImmichError::Io(_) => "io",
            ImmichError::NoKey => "noKey",
        }
    }
}

impl std::fmt::Display for ImmichError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImmichError::BadUrl(e) => write!(f, "not a usable server address: {e}"),
            ImmichError::Offline(e) => {
                write!(f, "the Immich server can't be reached ({e}); check the address and that the server is running, then retry")
            }
            ImmichError::Untrusted { host, fingerprint } => write!(
                f,
                "the certificate of {host} is not trusted (SHA-256 fingerprint {fingerprint}); confirm it only if it matches your server's certificate"
            ),
            ImmichError::Tls(e) => write!(f, "secure connection failed: {e}"),
            ImmichError::BadKey => {
                write!(f, "the server refused the API key; create a new key in Immich (Account Settings → API Keys) and enter it again")
            }
            ImmichError::Forbidden(what) => write!(f, "the API key lacks the permission for this ({what}); give the key more permissions in Immich"),
            ImmichError::NotFound(what) => write!(f, "not found on the server: {what}"),
            ImmichError::TooOld(v) => {
                write!(f, "this Immich server runs version {v}; the app needs {MIN_VERSION} or later, please upgrade the server")
            }
            ImmichError::Server(code) => write!(f, "the Immich server had an error (HTTP {code}); try again later"),
            ImmichError::Status(code, reason) => write!(f, "the Immich server answered {code} {reason}"),
            ImmichError::Protocol(e) => write!(f, "unexpected answer from the server (is this an Immich server?): {e}"),
            ImmichError::Cancelled => write!(f, "cancelled"),
            ImmichError::Io(e) => write!(f, "{e}"),
            ImmichError::NoKey => write!(f, "no API key is stored for this account; connect it again"),
        }
    }
}

impl std::error::Error for ImmichError {}

impl From<NetError> for ImmichError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::BadUrl(m) => ImmichError::BadUrl(m),
            NetError::Connect(m) => ImmichError::Offline(m),
            NetError::Stalled => ImmichError::Offline("the server stopped responding".into()),
            NetError::Cancelled => ImmichError::Cancelled,
            NetError::Tls(m) => ImmichError::Tls(m),
            NetError::UntrustedCertificate { host, fingerprint } => ImmichError::Untrusted { host, fingerprint },
            NetError::Status { code, reason } => status(code, &reason),
            NetError::Io(m) => ImmichError::Io(m),
            other => ImmichError::Protocol(other.to_string()),
        }
    }
}

/// The error for an HTTP status (`what`: the endpoint, for 403/404).
pub(crate) fn status(code: u16, what: &str) -> ImmichError {
    match code {
        401 => ImmichError::BadKey,
        403 => ImmichError::Forbidden(what.to_string()),
        404 => ImmichError::NotFound(what.to_string()),
        500..=599 => ImmichError::Server(code),
        _ => ImmichError::Status(code, what.to_string()),
    }
}
