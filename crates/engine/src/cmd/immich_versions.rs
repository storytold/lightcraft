//! RT.C1 / RT.C2: Edit In versions of Immich-linked photos go to Immich, stacked with the photo
//! they were made from, the edit as the stack's primary (Lightroom's "stacked with original",
//! mirrored where Immich users look).
//!
//! `immich.uploadVersion {id}` (the desktop app calls it after the editor saved a version):
//! - the version itself not linked yet, a photo in its catalog stack linked to an Immich asset:
//!   upload the version's file, link the photo to the new asset, and stack it on Immich with the
//!   new asset as primary (`POST /stacks` with the source's stack primary, which merges that stack
//!   in, else with the source asset);
//! - the version already linked (it was saved again): Immich has no supported way to replace an
//!   asset's original (v3.3.1: no `PUT /assets/{id}/original`), so a new asset is uploaded and made
//!   the primary; then **Keep both** (default: the previous upload stays in the stack) or
//!   **Replace** (the previous upload goes to Immich's trash after its albums are copied to the
//!   new asset). `resave` picks one; without it the setting decides (`immich.versionSettings`),
//!   and when that is `ask` the command answers `{ask: true}` for the app to ask the user.
//!
//! The upload runs on a worker thread holding an activity row; the result arrives through
//! `remote.pump`. Rating, tags and description follow through the normal sync (the version is now
//! linked). Errors are reported, never fatal.
//!
//! Sources: own design; Immich v3.3.1 OpenAPI specification (stacks, assets, albums).

use std::path::PathBuf;

use lcc_catalog::{PhotoId, RemoteIdentity, Source, SyncState as LinkState};
use lcc_immich::link::SERVICE;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cmd::{CommandSpec, always, bad, cmd, str_param};
use crate::remote::Msg;
use crate::{Result, Session};

/// The device id Immich records for uploaded versions.
const DEVICE: &str = "desktop-edit";

/// What to do when a version that is already on Immich is saved again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resave {
    /// Ask every time (the default until the user ticks "remember my choice").
    #[default]
    Ask,
    KeepBoth,
    Replace,
}

impl Resave {
    fn parse(s: &str) -> Option<Resave> {
        match s {
            "ask" => Some(Resave::Ask),
            "keepBoth" => Some(Resave::KeepBoth),
            "replace" => Some(Resave::Replace),
            _ => None,
        }
    }
}

/// `<library>/Immich/versions.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VersionSettings {
    pub resave: Resave,
}

fn settings_path(s: &Session) -> Option<PathBuf> {
    s.immich_dir().map(|d| d.join("versions.json"))
}

fn load_settings(s: &Session) -> VersionSettings {
    settings_path(s).and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn version_settings(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.versionSettings";
    let mut st = load_settings(s);
    if let Some(r) = str_param(p, "resave") {
        st.resave = Resave::parse(r).ok_or_else(|| bad(C, format!("resave `{r}` (ask, keepBoth, replace)")))?;
        let path = settings_path(s).ok_or_else(|| bad(C, "no library folder to keep the setting in"))?;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| bad(C, format!("{}: {e}", d.display())))?;
        }
        let bytes = serde_json::to_vec_pretty(&st).map_err(|e| bad(C, e.to_string()))?;
        lcc_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| bad(C, format!("{}: {e}", path.display())))?;
    }
    serde_json::to_value(st).map_err(|e| bad(C, e.to_string()))
}

/// What a finished upload brings back.
pub(crate) struct VersionDone {
    photo: PhotoId,
    account: String,
    result: std::result::Result<Uploaded, String>,
}

struct Uploaded {
    asset: String,
    duplicate: bool,
    stacked: bool,
    trashed: Option<String>,
    warnings: Vec<String>,
}

/// The confirmed Immich link of `photo` (account, asset).
fn link_of(s: &Session, photo: PhotoId) -> Option<(String, String)> {
    s.catalog
        .remote_of(photo)
        .find(|r| r.service == SERVICE && r.sync_state != LinkState::Probable)
        .map(|r| (r.account_id.clone(), r.remote_id.clone()))
}

/// The photo a version came from on Immich: the nearest linked photo below it in its stack, else
/// above it.
fn linked_source(s: &Session, photo: PhotoId) -> Option<(String, String)> {
    let st = s.catalog.stack_of(photo)?;
    let i = st.photos.iter().position(|p| *p == photo)?;
    let below = st.photos.iter().skip(i + 1);
    let above = st.photos.iter().take(i).rev();
    below.chain(above).find_map(|p| link_of(s, *p))
}

fn mime_of(name: &str) -> &'static str {
    match std::path::Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("tif" | "tiff") => "image/tiff",
        Some("psd" | "psb") => "image/vnd.adobe.photoshop",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

/// Plain-data form of a request, for the worker.
struct Job {
    photo: PhotoId,
    path: PathBuf,
    file_name: String,
    created: String,
    favorite: bool,
    /// The asset to stack with (the source, or the previous upload of this version).
    anchor: String,
    /// The previous upload of this version to trash (Replace).
    replace: Option<String>,
}

fn upload_version(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "immich.uploadVersion";
    let id = p.get("id").and_then(Value::as_u64).map(PhotoId).or_else(|| s.active()).ok_or_else(|| bad(C, "no photo"))?;
    let ph = s.catalog.photo(id).cloned().ok_or_else(|| bad(C, format!("no photo {}", id.0)))?;
    let Source::File { path } = &ph.source else { return Ok(json!({"skipped": "a generated photo has no file to upload"})) };
    let own = link_of(s, id);
    let (account, anchor, previous) = match (&own, linked_source(s, id)) {
        (Some((acc, asset)), _) => (acc.clone(), asset.clone(), Some(asset.clone())),
        (None, Some((acc, asset))) => (acc, asset, None),
        (None, None) => return Ok(json!({"skipped": "neither this photo nor its stack is on Immich"})),
    };
    let replace = match previous {
        None => None,
        Some(prev) => {
            let choice = match str_param(p, "resave") {
                Some(r) => Resave::parse(r).ok_or_else(|| bad(C, format!("resave `{r}` (keepBoth, replace)")))?,
                None => load_settings(s).resave,
            };
            match choice {
                Resave::Ask => return Ok(json!({"ask": true, "photo": id.0, "asset": prev})),
                Resave::KeepBoth => None,
                Resave::Replace => Some(prev),
            }
        }
    };
    let (_, client) = s.immich_client(&account).map_err(|e| bad(C, e.to_string()))?;
    let created = ph.captured.clone().unwrap_or_else(|| ph.imported.clone());
    let created = if created.len() == 19 { format!("{created}.000Z") } else { created };
    let job = Job {
        photo: id,
        path: PathBuf::from(path),
        file_name: ph.file_name.clone(),
        created,
        favorite: ph.flag == lcc_catalog::Flag::Pick,
        anchor,
        replace,
    };
    let task = s.activity.start("immich", &format!("Uploading {} to Immich", ph.file_name), crate::activity::Cancel::No);
    let tx = s.remote.sender();
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(true);
    let run = move || {
        let result = work(&client, &job);
        drop(task);
        VersionDone { photo: job.photo, account, result }
    };
    if background {
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Version(Box::new(run())));
        });
        return Ok(json!({"started": true}));
    }
    let done = run();
    Ok(finish(s, done))
}

fn work(c: &lcc_immich::Client, job: &Job) -> std::result::Result<Uploaded, String> {
    let device_asset_id =
        format!("{}-{}", job.photo.0, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    let up = lcc_immich::client::NewAsset {
        data: lcc_immich::client::UploadData::File(&job.path),
        file_name: &job.file_name,
        mime: mime_of(&job.file_name),
        device_asset_id: &device_asset_id,
        device_id: DEVICE,
        created: &job.created,
        favorite: job.favorite,
        sidecar: None,
    };
    let (asset, duplicate) = c.upload(&up).map_err(|e| format!("upload {}: {e}", job.file_name))?;
    let mut warnings = Vec::new();
    if duplicate && asset == job.anchor {
        // saved without a change: nothing to do
        return Ok(Uploaded { asset, duplicate, stacked: false, trashed: None, warnings });
    }
    // the edit on top: with the anchor's stack primary (Immich merges that stack in) or the anchor
    let top = match c.asset_stack(&job.anchor) {
        Ok(Some(st)) => st.primary_asset_id,
        Ok(None) => job.anchor.clone(),
        Err(e) => {
            warnings.push(format!("stack lookup: {e}"));
            job.anchor.clone()
        }
    };
    let stacked = match c.stack(&[asset.clone(), top]) {
        Ok(()) => true,
        Err(e) => {
            warnings.push(format!("stack: {e}"));
            false
        }
    };
    let mut trashed = None;
    if let Some(prev) = &job.replace
        && *prev != asset
    {
        match c.albums_of(prev) {
            Ok(albums) => {
                for a in albums {
                    if let Err(e) = c.album_add(&a.id, std::slice::from_ref(&asset)) {
                        warnings.push(format!("album {}: {e}", a.album_name));
                    }
                }
            }
            Err(e) => warnings.push(format!("albums: {e}")),
        }
        match c.trash_assets(std::slice::from_ref(prev)) {
            Ok(()) => trashed = Some(prev.clone()),
            Err(e) => warnings.push(format!("trash the previous upload: {e}")),
        }
    }
    Ok(Uploaded { asset, duplicate, stacked, trashed, warnings })
}

/// Take a finished upload in: link the version to its new asset. → the outcome
pub(crate) fn finish(s: &mut Session, d: VersionDone) -> Value {
    match d.result {
        Ok(u) => {
            let op = lcc_catalog::Catalog::link_remote_op(RemoteIdentity {
                photo_id: d.photo,
                service: SERVICE.to_string(),
                account_id: d.account.clone(),
                remote_id: u.asset.clone(),
                remote_checksum: None,
                remote_updated_at: None,
                last_synced_at: Some(lcc_catalog::rules::now()),
                sync_state: LinkState::Synced,
            });
            let mut warnings = u.warnings;
            if s.catalog.photo(d.photo).is_some()
                && let Err(e) = s.commit("Link Version to Immich", op)
            {
                warnings.push(format!("link: {e}"));
            }
            for w in &warnings {
                log::warn!("immich version {}: {w}", d.photo.0);
            }
            let v = json!({"photo": d.photo.0, "asset": u.asset, "duplicate": u.duplicate, "stacked": u.stacked, "trashed": u.trashed, "warnings": warnings});
            s.remote.versions.push(v.clone());
            v
        }
        Err(e) => {
            log::warn!("immich version {}: {e}", d.photo.0);
            let v = json!({"photo": d.photo.0, "error": e});
            s.remote.versions.push(v.clone());
            v
        }
    }
}

/// The finished uploads since the last call (the app shows them as toasts).
fn version_results(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"results": std::mem::take(&mut s.remote.versions)}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "immich.uploadVersion",
            "Upload Edit Version to Immich",
            [],
            None,
            "{id?, resave?: keepBoth|replace, background?: true} — upload an Edit In version whose stack holds an Immich-linked photo, link it, and stack it on Immich as the primary; a version already on Immich (saved again) is uploaded as a new asset: keepBoth keeps the previous one in the stack, replace trashes it (albums carried over); without resave the setting decides, `ask` → {ask: true, photo, asset} → {started} | the outcome {photo, asset, duplicate, stacked, trashed, warnings} | {skipped}",
            always,
            upload_version
        ),
        cmd!(
            "immich.versionSettings",
            "Immich Version Settings",
            [],
            None,
            "{resave?: ask|keepBoth|replace} — what saving an Edit In version that is already on Immich again does (kept in the library's Immich folder) → {resave}",
            always,
            version_settings
        ),
        cmd!(query "immich.versionResults", "Immich Version Results", [], None, "{} — finished version uploads since the last call → {results: [{photo, asset?, stacked?, trashed?, warnings?, error?}]}", always, version_results),
    ]
}
