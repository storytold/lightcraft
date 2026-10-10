//! The interface every publish service implements (the built-in ones, Immich and plugin services).

use dac_catalog::PhotoId;
use serde::Serialize;

use crate::ServiceConfig;
use crate::config::CollectionConfig;

/// Why publishing failed, in words a user can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishError {
    /// The service's settings are missing or wrong (no folder, unknown kind, …).
    Config(String),
    /// Reading or writing a file failed.
    Io(String),
    /// The service refused or failed (network, permission, …).
    Service(String),
    /// The service can't do this.
    Unsupported(String),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Config(m) | PublishError::Io(m) | PublishError::Service(m) | PublishError::Unsupported(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for PublishError {}

/// What a service can do besides publishing and removing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Remote comments ([`PublishService::comments`]).
    pub comments: bool,
    /// Remote likes / favourites.
    pub likes: bool,
}

/// A remote comment on a published photo.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comment {
    pub author: String,
    pub text: String,
    /// ISO 8601, when known.
    pub date: Option<String>,
}

/// One rendered photo to send.
pub struct Upload<'a> {
    pub photo: PhotoId,
    /// The file name the export settings gave it.
    pub file_name: &'a str,
    pub bytes: &'a [u8],
    /// Files that go with it, named like it with another extension (an XMP sidecar).
    pub sidecars: &'a [(&'static str, Vec<u8>)],
    /// The remote id of its previous publication in this collection (a re-publish replaces it).
    pub previous: Option<&'a str>,
}

/// Where a photo was published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub remote_id: String,
}

/// A publish service, opened for one collection.
pub trait PublishService: Send {
    fn kind(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    /// Send (or replace, when `up.previous` is set) one photo.
    fn publish(&mut self, up: &Upload<'_>) -> Result<Published, PublishError>;
    /// Take a published photo off the service. Already gone is not an error.
    fn remove(&mut self, remote_id: &str) -> Result<(), PublishError>;
    /// Remote comments on a published photo (none for services without them).
    fn comments(&mut self, _remote_id: &str) -> Result<Vec<Comment>, PublishError> {
        Ok(Vec::new())
    }
}

/// Open a built-in service for one of its collections.
pub fn open_service(svc: &ServiceConfig, coll: &CollectionConfig) -> Result<Box<dyn PublishService>, PublishError> {
    match svc.kind.as_str() {
        crate::KIND_HARD_DRIVE => Ok(Box::new(crate::hard_drive::HardDrive::open(svc, coll)?)),
        other => Err(PublishError::Unsupported(format!("this build can't publish to `{other}` services (known: {})", crate::KINDS.join(", ")))),
    }
}
