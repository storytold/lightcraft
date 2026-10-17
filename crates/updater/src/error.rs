//! The one error type: everything a check, download or install can fail with, worded for a dialog
//! (no panics, no placeholders).

/// Why an update check, download or install did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// The machine's platform has no release artifact (nothing to update with).
    Unsupported(String),
    /// The release feed could not be read at all.
    Net(String),
    /// The server answered a status we can't use (`404`, `403` rate-limited…).
    Http { code: u16, what: String },
    /// The feed is not the JSON we expect.
    Parse(String),
    /// A feed, file or checksum list that is not `https://` (what, and where).
    Insecure(String),
    /// The newest release has no file for this platform.
    NoAssetFor { version: String, target: String },
    /// The download did not match its expected size or SHA-256.
    Verify(String),
    /// Writing the downloaded file failed (permissions, disk space).
    Io(String),
    /// Installing the downloaded file failed.
    Install(String),
    /// The user cancelled.
    Cancelled,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Unsupported(why) => write!(f, "{why}"),
            UpdateError::Net(why) => write!(f, "could not reach the release server: {why}"),
            UpdateError::Http { code, what } => write!(f, "the release server answered {code} ({what})"),
            UpdateError::Parse(why) => write!(f, "the release information could not be read: {why}"),
            UpdateError::Insecure(what) => write!(f, "refusing {what}: https is required"),
            UpdateError::NoAssetFor { version, target } => {
                write!(f, "release {version} has no download for this system ({target}); get it from the releases page instead")
            }
            UpdateError::Verify(why) => write!(f, "the downloaded update did not check out: {why}"),
            UpdateError::Io(why) => write!(f, "{why}"),
            UpdateError::Install(why) => write!(f, "the update could not be installed: {why}"),
            UpdateError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<lcc_net::NetError> for UpdateError {
    fn from(e: lcc_net::NetError) -> Self {
        match e {
            lcc_net::NetError::Status { code, reason } => UpdateError::Http { code, what: reason },
            other => UpdateError::Net(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_actionable() {
        let e = UpdateError::NoAssetFor { version: "1.2.3".into(), target: "linux-riscv64".into() };
        let text = e.to_string();
        assert!(text.contains("1.2.3") && text.contains("linux-riscv64") && text.contains("releases page"), "{text}");
        assert!(UpdateError::Http { code: 403, what: "rate limited".into() }.to_string().contains("403"));
        assert!(UpdateError::Net("dns".into()).to_string().contains("dns"));
        assert_eq!(UpdateError::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn a_status_maps_to_http() {
        let e: UpdateError = lcc_net::NetError::Status { code: 404, reason: "Not Found".into() }.into();
        assert_eq!(e, UpdateError::Http { code: 404, what: "Not Found".into() });
        let e: UpdateError = lcc_net::NetError::Cancelled.into();
        assert!(matches!(e, UpdateError::Net(_)));
    }
}
