//! The publish services of a library, kept in `publish.json` in its folder.

use std::path::Path;

use dac_catalog::AlbumId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::PublishError;

/// One published collection: a catalog album, and where its files go on the service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionConfig {
    pub album: AlbumId,
    /// The collection's folder (Hard Drive) or remote name, fixed when it was created: renaming the
    /// album doesn't move what was published.
    pub folder: String,
}

/// One publish service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceConfig {
    /// Stable id (`svc-1`, …).
    pub id: String,
    /// [`crate::KINDS`].
    pub kind: String,
    pub name: String,
    /// The kind's own settings (Hard Drive: `{dir}`).
    #[serde(default)]
    pub settings: Value,
    /// `app.export` params the photos are rendered with.
    #[serde(default)]
    pub export: Value,
    /// The collection set that holds the published collections' albums.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<AlbumId>,
    #[serde(default)]
    pub collections: Vec<CollectionConfig>,
}

impl ServiceConfig {
    pub fn collection(&self, album: AlbumId) -> Option<&CollectionConfig> {
        self.collections.iter().find(|c| c.album == album)
    }
}

/// Every publish service of a library.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PublishConfig {
    pub services: Vec<ServiceConfig>,
    next_id: u64,
}

/// The most services and collections kept (a damaged or hostile file can't make the app loop
/// over millions).
pub const MAX_SERVICES: usize = 256;
pub const MAX_COLLECTIONS: usize = 10_000;

impl PublishConfig {
    /// Read `publish.json` from the library folder `dir`; none yet = no services. A damaged file is
    /// an error (never silently replaced: it holds where things were published).
    pub fn load(dir: &Path) -> Result<PublishConfig, PublishError> {
        let path = dir.join(crate::FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PublishConfig::default()),
            Err(e) => return Err(PublishError::Io(format!("{}: {e}", path.display()))),
        };
        let mut c: PublishConfig = serde_json::from_slice(&bytes).map_err(|e| PublishError::Config(format!("{} is damaged: {e}", path.display())))?;
        c.services.truncate(MAX_SERVICES);
        for s in &mut c.services {
            s.collections.truncate(MAX_COLLECTIONS);
        }
        Ok(c)
    }

    /// Write `publish.json` atomically.
    pub fn save(&self, dir: &Path) -> Result<(), PublishError> {
        let body = serde_json::to_vec_pretty(self).map_err(|e| PublishError::Config(e.to_string()))?;
        let path = dir.join(crate::FILE);
        dac_catalog::safe_file::write_atomic(&path, &body).map_err(|e| PublishError::Io(format!("{}: {e}", path.display())))
    }

    /// A new service id.
    pub fn alloc_id(&mut self) -> String {
        loop {
            self.next_id = self.next_id.saturating_add(1);
            let id = format!("svc-{}", self.next_id);
            if self.service(&id).is_none() {
                return id;
            }
        }
    }

    pub fn service(&self, id: &str) -> Option<&ServiceConfig> {
        self.services.iter().find(|s| s.id == id)
    }
    pub fn service_mut(&mut self, id: &str) -> Option<&mut ServiceConfig> {
        self.services.iter_mut().find(|s| s.id == id)
    }
    /// The service a published collection belongs to.
    pub fn service_of(&self, album: AlbumId) -> Option<&ServiceConfig> {
        self.services.iter().find(|s| s.collection(album).is_some())
    }
}
