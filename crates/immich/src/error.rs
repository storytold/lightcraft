use crate::http::HttpError;

/// Why an Immich call failed. Every variant is something the user can act on, and none of them
/// carries the API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The server could not be reached, or the connection, TLS or HTTP framing failed.
    /// A certificate the built-in root set does not honour arrives here as well, saying so.
    Transport(String),
    /// The key is missing, wrong, or not allowed to do this (401/403).
    Unauthorized,
    /// No such asset, album or endpoint on this server (404).
    NotFound(String),
    /// The server answered with an error. `status` is 0 when the reply could not be read as JSON.
    Api { status: u16, message: String },
    /// A configured cap stopped the transfer, announced or measured while it ran.
    Limit(String),
    /// The user cancelled.
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "{e}"),
            Error::Unauthorized => write!(f, "the server rejected the API key (create one in Immich: Settings > API keys)"),
            Error::NotFound(what) => write!(f, "not found on the server: {what}"),
            Error::Api { status, message } => write!(f, "the server said {status}: {message}"),
            Error::Limit(what) => write!(f, "{what}"),
            Error::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for Error {}

impl From<HttpError> for Error {
    fn from(e: HttpError) -> Error {
        match e {
            HttpError::Cancelled => Error::Cancelled,
            HttpError::Tls(why) => Error::Transport(format!(
                "{why}: the built-in root certificates do not trust this server's certificate \
                 (self-signed or internally issued). Serve it over HTTPS with a certificate the \
                 system trusts, or reach it over http:// on a network you trust."
            )),
            other => Error::Transport(other.to_string()),
        }
    }
}
