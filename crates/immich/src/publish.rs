//! IMM-PUBLISH: Immich as a publish service. A published collection is an Immich album (found by
//! name, created when missing). Per service, what is sent:
//!
//! - `rendered`: the render made with the service's export settings (Immich shows the edit);
//! - `original`: the original file (and the photo's XMP as Immich's sidecar);
//! - `both`: both, stacked in Immich with the render on top.
//!
//! Before an upload the SHA-1 is checked with the server (`bulk-upload-check`), so Immich never
//! stores a duplicate: an asset already there is just put into the album. An original that is
//! already linked to an Immich asset (IMM-LINK, external libraries: shared originals) is never
//! uploaded. Re-publishing uploads the new render, moves album membership and sends the old render
//! to Immich's trash. Removing a photo from the collection takes it out of the album; deleting its
//! asset is a separate option (`deleteRemoved`, off), and always goes to the trash.
//!
//! Remote ids: `r:<render id>` and/or `o:<original id>`, joined by `+`.
//!
//! Sources: own design; Immich's public API documentation (plan/immich.md → Publish).

use std::collections::HashMap;
use std::path::PathBuf;

use dac_catalog::PhotoId;
use dac_publish::{Capabilities, PublishError, PublishService, Published, Upload};
use serde::{Deserialize, Serialize};

use crate::ImmichError;
use crate::client::{Client, NewAsset, UploadData};
use crate::types::UploadCheck;

pub const KIND: &str = "immich";

/// What a service sends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendMode {
    #[default]
    Rendered,
    Original,
    Both,
}

/// An Immich publish service's settings (`ServiceConfig::settings`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PublishSettings {
    /// The connected account (`<url>#<user id>`).
    pub account: String,
    pub send: SendMode,
    /// Removing a photo from the collection also sends its asset to Immich's trash.
    pub delete_removed: bool,
}

/// What the service needs to know about a photo besides the render.
#[derive(Clone, Debug, Default)]
pub struct PhotoInfo {
    /// ISO 8601 capture (or import) time.
    pub created: String,
    pub favorite: bool,
    /// The original file (modes `original` / `both`).
    pub original: Option<PathBuf>,
    pub original_name: String,
    /// An Immich asset the original is already linked to (never uploaded again).
    pub linked: Option<String>,
    /// The photo's XMP, sent as the original's sidecar.
    pub xmp: Option<Vec<u8>>,
}

pub struct ImmichPublish {
    client: Client,
    settings: PublishSettings,
    album_name: String,
    album: Option<String>,
    photos: HashMap<PhotoId, PhotoInfo>,
    device_id: String,
}

fn err(e: ImmichError) -> PublishError {
    match e {
        ImmichError::Io(m) => PublishError::Io(m),
        e => PublishError::Service(e.to_string()),
    }
}

fn mime_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "tif" | "tiff" => "image/tiff",
        "avif" => "image/avif",
        "jxl" => "image/jxl",
        "webp" => "image/webp",
        "heic" | "heif" => "image/heif",
        _ => "application/octet-stream",
    }
}

/// `(render, original)` ids of a remote id.
pub fn parse_remote(id: &str) -> (Option<&str>, Option<&str>) {
    let (mut r, mut o) = (None, None);
    for part in id.split('+') {
        if let Some(x) = part.strip_prefix("r:") {
            r = Some(x);
        } else if let Some(x) = part.strip_prefix("o:") {
            o = Some(x);
        }
    }
    (r, o)
}

fn remote_id(render: Option<&str>, original: Option<&str>) -> String {
    let mut v = Vec::new();
    if let Some(r) = render {
        v.push(format!("r:{r}"));
    }
    if let Some(o) = original {
        v.push(format!("o:{o}"));
    }
    v.join("+")
}

impl ImmichPublish {
    /// `album_name`: the collection's name on the server; `device_id`: the app's id as a device.
    pub fn new(client: Client, settings: PublishSettings, album_name: &str, device_id: &str, photos: HashMap<PhotoId, PhotoInfo>) -> ImmichPublish {
        ImmichPublish { client, settings, album_name: album_name.to_string(), album: None, photos, device_id: device_id.to_string() }
    }

    fn album(&mut self) -> Result<String, PublishError> {
        if let Some(a) = &self.album {
            return Ok(a.clone());
        }
        let id = match self.client.albums().map_err(err)?.into_iter().find(|a| a.album_name == self.album_name) {
            Some(a) => a.id,
            None => self.client.create_album(&self.album_name).map_err(err)?.id,
        };
        self.album = Some(id.clone());
        Ok(id)
    }

    /// The id of an asset with these bytes' SHA-1 already on the server.
    fn existing(&self, sha1_b64: &str) -> Result<Option<String>, PublishError> {
        let r = self.client.upload_check(&[UploadCheck { id: "x".into(), checksum: sha1_b64.into() }]).map_err(err)?;
        Ok(r.into_iter().find(|x| x.action == "reject").and_then(|x| x.asset_id))
    }

    fn send(
        &self,
        data: UploadData<'_>,
        sha1: String,
        name: &str,
        device_asset_id: &str,
        info: &PhotoInfo,
        sidecar: Option<&[u8]>,
    ) -> Result<String, PublishError> {
        if let Some(id) = self.existing(&sha1)? {
            return Ok(id);
        }
        let up = NewAsset {
            data,
            file_name: name,
            mime: mime_of(name),
            device_asset_id,
            device_id: &self.device_id,
            created: &info.created,
            favorite: info.favorite,
            sidecar,
        };
        self.client.upload(&up).map(|(id, _)| id).map_err(err)
    }
}

impl PublishService for ImmichPublish {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { comments: false, likes: true }
    }

    fn publish(&mut self, up: &Upload<'_>) -> Result<Published, PublishError> {
        let info =
            self.photos.get(&up.photo).cloned().unwrap_or_else(|| PhotoInfo { created: "1970-01-01T00:00:00.000Z".into(), ..Default::default() });
        let album = self.album()?;
        let mode = self.settings.send;
        let render = if mode != SendMode::Original {
            let sha = dac_hash::sha1_bytes(up.bytes).to_base64();
            Some(self.send(UploadData::Bytes(up.bytes), sha, up.file_name, &format!("{}-render", up.photo.0), &info, None)?)
        } else {
            None
        };
        let original = if mode != SendMode::Rendered {
            match (&info.linked, &info.original) {
                (Some(id), _) => Some(id.clone()),
                (None, Some(path)) => {
                    let sha = dac_hash::sha1_file(path).map_err(|e| PublishError::Io(format!("{}: {e}", path.display())))?.to_base64();
                    let name = if info.original_name.is_empty() { up.file_name.to_string() } else { info.original_name.clone() };
                    Some(self.send(UploadData::File(path), sha, &name, &format!("{}-original", up.photo.0), &info, info.xmp.as_deref())?)
                }
                (None, None) => return Err(PublishError::Io("the original file is not available".into())),
            }
        } else {
            None
        };
        let ids: Vec<String> = render.iter().chain(original.iter()).cloned().collect();
        self.client.album_add(&album, &ids).map_err(err)?;
        if let (Some(r), Some(o)) = (&render, &original)
            && r != o
        {
            // stacked: the render on top; an already-stacked pair is not an error worth failing for
            if let Err(e) = self.client.stack(&[r.clone(), o.clone()]) {
                log::info!("Immich stack: {e}");
            }
        }
        // re-publish: the old render leaves the album and goes to the trash
        if let Some(prev) = up.previous {
            let (pr, po) = parse_remote(prev);
            let mut out_of_album = Vec::new();
            if let Some(pr) = pr.filter(|x| Some(*x) != render.as_deref() && Some(*x) != original.as_deref()) {
                out_of_album.push(pr.to_string());
                if let Err(e) = self.client.trash_assets(&[pr.to_string()]) {
                    log::info!("Immich: trashing the old render: {e}");
                }
            }
            if let Some(po) = po.filter(|x| Some(*x) != original.as_deref() && Some(*x) != render.as_deref()) {
                out_of_album.push(po.to_string());
            }
            if !out_of_album.is_empty() {
                self.client.album_remove(&album, &out_of_album).map_err(err)?;
            }
        }
        Ok(Published { remote_id: remote_id(render.as_deref(), original.as_deref()) })
    }

    fn remove(&mut self, id: &str) -> Result<(), PublishError> {
        let album = self.album()?;
        let (r, o) = parse_remote(id);
        let ids: Vec<String> = r.iter().chain(o.iter()).map(|x| x.to_string()).collect();
        if ids.is_empty() {
            return Ok(());
        }
        match self.client.album_remove(&album, &ids) {
            Ok(()) | Err(ImmichError::NotFound(_)) => {}
            Err(e) => return Err(err(e)),
        }
        if self.settings.delete_removed
            && let Some(r) = r
        {
            // only renders: an original may be the one in the user's own library
            let mine = vec![r.to_string()];
            match self.client.trash_assets(&mine) {
                Ok(()) | Err(ImmichError::NotFound(_)) => {}
                Err(e) => return Err(err(e)),
            }
        }
        Ok(())
    }
}
