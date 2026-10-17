//! IMM-SHARELINK: share a set of rendered photos through Immich: upload them, gather them in a new
//! album and create an Immich shared link for that album (`POST /shared-links`, type `ALBUM`) with
//! an optional expiry, password, download and metadata switches. Written from Immich's public API
//! documentation.

use serde::{Deserialize, Serialize};

use crate::ImmichError;
use crate::client::{Client, NewAsset, UploadData};

/// Options of a shared link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ShareOptions {
    /// ISO 8601 UTC expiry (`2026-12-31T00:00:00.000Z`); `None` never expires.
    pub expires_at: Option<String>,
    /// Password viewers must type; `None` or empty for none.
    pub password: Option<String>,
    pub allow_download: bool,
    pub show_metadata: bool,
    pub description: Option<String>,
}

impl Default for ShareOptions {
    fn default() -> Self {
        ShareOptions { expires_at: None, password: None, allow_download: true, show_metadata: true, description: None }
    }
}

/// A created shared link.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SharedLink {
    pub id: String,
    pub key: String,
    pub expires_at: Option<String>,
}

/// The public URL of a shared link on server `base`.
pub fn share_url(base: &str, key: &str) -> String {
    format!("{}/share/{key}", base.trim_end_matches('/'))
}

/// The JSON body of `POST /shared-links` for `album`.
pub fn link_body(album: &str, o: &ShareOptions) -> serde_json::Value {
    let mut b = serde_json::json!({
        "type": "ALBUM",
        "albumId": album,
        "allowDownload": o.allow_download,
        "showMetadata": o.show_metadata,
        "allowUpload": false,
    });
    if let Some(e) = o.expires_at.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        b["expiresAt"] = serde_json::json!(e);
    }
    if let Some(p) = o.password.as_deref().filter(|p| !p.is_empty()) {
        b["password"] = serde_json::json!(p);
    }
    if let Some(d) = o.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        b["description"] = serde_json::json!(d);
    }
    b
}

/// One photo to share: the JPEG bytes and what Immich records about it.
pub struct ShareItem {
    pub file_name: String,
    pub bytes: Vec<u8>,
    /// ISO 8601 capture time.
    pub created: String,
}

/// What a share produced.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shared {
    pub album_id: String,
    pub link_id: String,
    pub url: String,
    pub uploaded: usize,
    pub duplicates: usize,
}

/// Upload `items` (device id `device`), add them to a new album `album_name`, share it.
/// `progress(done, total)` returning false cancels before the next upload.
pub fn share_photos(
    client: &Client,
    album_name: &str,
    device: &str,
    items: &[ShareItem],
    o: &ShareOptions,
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<Shared, ImmichError> {
    if items.is_empty() {
        return Err(ImmichError::Protocol("nothing to share".into()));
    }
    let mut ids = Vec::with_capacity(items.len());
    let mut duplicates = 0;
    for (k, it) in items.iter().enumerate() {
        if !progress(k, items.len()) {
            return Err(ImmichError::Protocol("share cancelled".into()));
        }
        let device_asset_id = format!("share-{album_name}-{k}-{}", it.file_name);
        let (id, dup) = client.upload(&NewAsset {
            data: UploadData::Bytes(&it.bytes),
            file_name: &it.file_name,
            mime: "image/jpeg",
            device_asset_id: &device_asset_id,
            device_id: device,
            created: &it.created,
            favorite: false,
            sidecar: None,
        })?;
        duplicates += usize::from(dup);
        ids.push(id);
    }
    let name = if album_name.trim().is_empty() { "Shared photos" } else { album_name.trim() };
    let album = client.create_album(name)?;
    client.album_add(&album.id, &ids)?;
    let link = client.create_shared_link(&album.id, o)?;
    progress(items.len(), items.len());
    Ok(Shared { album_id: album.id, link_id: link.id, url: share_url(client.base(), &link.key), uploaded: ids.len(), duplicates })
}
