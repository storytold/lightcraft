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
//! The album is remembered by id (a small state file per collection), so renaming the collection
//! (or moving it into another collection set: sets become name prefixes, `Set / Collection`)
//! renames the Immich album on the next run instead of creating a new one.
//!
//! Uploads are retried: a failed upload (server offline, 5xx, dropped connection) is tried again
//! with backoff, and each attempt first asks the server by checksum whether the bytes already
//! arrived, so a retry never stores a duplicate. A run cut short resumes where it stopped: photos
//! already sent are published, and the rest are found by checksum on the next run.
//!
//! Remote ids: `r:<render id>` and/or `o:<original id>`, joined by `+`.
//!
//! Sources: own design; Immich's public API documentation (plan/immich.md → Publish).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use lcc_catalog::PhotoId;
use lcc_publish::{Capabilities, PublishError, PublishService, Published, Upload};
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
    /// Where the album's Immich id is remembered (rename instead of a new album).
    album_state: Option<PathBuf>,
    upload_retries: u32,
    upload_backoff: Duration,
}

/// Upload attempts after the first, and the first wait (doubling).
pub const UPLOAD_RETRIES: u32 = 3;
pub const UPLOAD_BACKOFF: Duration = Duration::from_secs(2);

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
        ImmichPublish {
            client,
            settings,
            album_name: album_name.to_string(),
            album: None,
            photos,
            device_id: device_id.to_string(),
            album_state: None,
            upload_retries: UPLOAD_RETRIES,
            upload_backoff: UPLOAD_BACKOFF,
        }
    }

    /// Remember the album's id in `path` (one file per published collection).
    pub fn with_album_state(mut self, path: PathBuf) -> ImmichPublish {
        self.album_state = Some(path);
        self
    }

    /// Upload retries and the first wait between them.
    pub fn with_upload_retry(mut self, retries: u32, backoff: Duration) -> ImmichPublish {
        self.upload_retries = retries.min(10);
        self.upload_backoff = backoff;
        self
    }

    fn remembered(&self) -> Option<String> {
        let b = std::fs::read(self.album_state.as_ref()?).ok()?;
        let id = String::from_utf8(b).ok()?.trim().to_string();
        (!id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(id)
    }

    fn remember(&self, id: &str) {
        let Some(p) = &self.album_state else { return };
        let r = p.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|_| std::fs::write(p, id));
        if let Err(e) = r {
            // not fatal: the album is found by name next time
            log::warn!("Immich publish: {}: {e}", p.display());
        }
    }

    /// The collection's album: the remembered one (renamed when the collection's name changed),
    /// else the album of that name, else a new one.
    fn album(&mut self) -> Result<String, PublishError> {
        if let Some(a) = &self.album {
            return Ok(a.clone());
        }
        let albums = self.client.albums().map_err(err)?;
        let known = self.remembered().and_then(|id| albums.iter().find(|a| a.id == id));
        let id = match known {
            Some(a) => {
                if a.album_name != self.album_name {
                    self.client.rename_album(&a.id, &self.album_name).map_err(err)?;
                }
                a.id.clone()
            }
            None => match albums.iter().find(|a| a.album_name == self.album_name) {
                Some(a) => a.id.clone(),
                None => self.client.create_album(&self.album_name).map_err(err)?.id,
            },
        };
        self.remember(&id);
        self.album = Some(id.clone());
        Ok(id)
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
        let mut attempt = 0;
        loop {
            match self.try_send(data, &sha1, name, device_asset_id, info, sidecar) {
                Err(e) if e.retryable() && attempt < self.upload_retries => {
                    let wait = self.upload_backoff.saturating_mul(1 << attempt.min(8));
                    log::info!("Immich upload of {name}: {e}; retrying in {wait:?}");
                    std::thread::sleep(wait);
                    attempt += 1;
                }
                r => return r.map_err(err),
            }
        }
    }

    /// One attempt: the checksum check first, so an upload that reached the server before the
    /// connection dropped is found instead of sent again.
    fn try_send(
        &self,
        data: UploadData<'_>,
        sha1: &str,
        name: &str,
        device_asset_id: &str,
        info: &PhotoInfo,
        sidecar: Option<&[u8]>,
    ) -> Result<String, ImmichError> {
        let r = self.client.upload_check(&[UploadCheck { id: "x".into(), checksum: sha1.into() }])?;
        if let Some(id) = r.into_iter().find(|x| x.action == "reject").and_then(|x| x.asset_id) {
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
        self.client.upload(&up).map(|(id, _)| id)
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
            let sha = lcc_hash::sha1_bytes(up.bytes).to_base64();
            Some(self.send(UploadData::Bytes(up.bytes), sha, up.file_name, &format!("{}-render", up.photo.0), &info, None)?)
        } else {
            None
        };
        let original = if mode != SendMode::Rendered {
            match (&info.linked, &info.original) {
                (Some(id), _) => Some(id.clone()),
                (None, Some(path)) => {
                    let sha = lcc_hash::sha1_file(path).map_err(|e| PublishError::Io(format!("{}: {e}", path.display())))?.to_base64();
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
