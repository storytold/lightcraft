//! The camera roll as a collection: `Camera Roll - {Device Name}` (`docs/sync.md` ▸ Camera roll).
//!
//! `sync.cameraRoll {on, folder?}` makes this device's camera-roll collection (kind
//! [`CollectionKind::CameraRoll`], owned by the device, shared like any other collection) and
//! watches `folder`: about every 30 s a worker takes the folder's stamp
//! ([`crate::folder_watch::stamp`]); when it changed, the folder is imported in place (the
//! ordinary import, which skips what the catalog has) and the collection is made to hold every
//! photo of the catalog whose file is in the folder. On Android the folder is DCIM/Camera, which
//! the app learns from the camera-roll listing the platform gives it (`sync.cameraRollSeen`).
//! The collection follows the device's name.
//!
//! Photo identity does the rest of the dedupe: the roll's photos are keyed by the SHA-1 of the
//! original, the same as the Immich app's upload of them, so whichever of the app's own upload,
//! the Immich app's upload, a peer's copy or a later import comes first, it is one photo.

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use lcc_catalog::{Album, AlbumId, Op, PhotoId, Source};
use lcc_sync::model::{CollectionKind, Grant};
use serde_json::{Value, json};

use super::{Lib, now_ms, touched};
use crate::{EngineError, Result, Session};

/// How often the folder's stamp is taken.
const EVERY: Duration = Duration::from_secs(30);

/// The worker taking the folder's stamp, and the last stamp taken.
#[derive(Default)]
pub(crate) struct RollWatch {
    pub at: Option<Instant>,
    pub rx: Option<Receiver<Option<crate::folder_watch::Stamp>>>,
    pub stamp: Option<crate::folder_watch::Stamp>,
}

/// The shared id of this device's camera roll.
pub(crate) fn roll_id(device: &str) -> String {
    format!("roll:{device}")
}

/// The collection's name on a device called `device_name`.
pub(crate) fn roll_name(device_name: &str) -> String {
    format!("Camera Roll - {device_name}")
}

fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

/// `sync.cameraRoll {on, folder?}`.
pub(crate) fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let on = p.get("on").and_then(Value::as_bool).ok_or_else(|| other("missing `on` (true or false)"))?;
    let folder = p.get("folder").and_then(Value::as_str).map(str::trim).filter(|f| !f.is_empty()).map(str::to_string);
    if folder.as_deref().is_some_and(|f| f.len() > 4096 || f.chars().any(char::is_control)) {
        return Err(other("that folder name can't be used"));
    }
    super::ensure_lib(s);
    let name = s.device_sync.device_name();
    let Some(mut lib) = s.device_sync.lib.take() else { return Err(other("only a catalogue on disk syncs")) };
    let out = (|| -> Result<Value> {
        let me = lib.replica.device.clone();
        if !lib.replica.group.as_ref().is_some_and(|g| g.has(&me)) {
            return Err(other(lcc_sync::replica::Refused::NoGroup));
        }
        if !on {
            lib.replica.camera_roll = None;
            return Ok(json!({"on": false}));
        }
        let folder = match folder {
            Some(f) => {
                if !Path::new(&f).is_dir() {
                    return Err(other(format!("`{f}` isn't a folder")));
                }
                f
            }
            // the platform tells where its camera roll is (Android: `sync.cameraRollSeen`)
            None if cfg!(target_os = "android") => lib.replica.camera_roll.clone().unwrap_or_default(),
            None => return Err(other("choose the folder your camera roll is in (`folder`)")),
        };
        ensure_collection(s, &mut lib, &name);
        lib.replica.camera_roll = Some(folder.clone());
        lib.roll.stamp = None;
        lib.roll.at = None;
        Ok(json!({"on": true, "folder": folder, "collection": roll_id(&me)}))
    })();
    touched(&mut lib);
    lib.revision = None;
    s.device_sync.lib = Some(lib);
    let v = out?;
    if v["on"] == true {
        scan_now(s);
    }
    Ok(v)
}

/// `sync.cameraRollSeen {paths}`: the platform listed its camera roll; on a device whose camera
/// roll syncs without a folder of its own, the folder they are in is the roll.
pub(crate) fn seen(s: &mut Session, p: &Value) -> Result<Value> {
    let paths: Vec<&str> =
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).take(100_000).collect()).unwrap_or_default();
    let parent = paths.iter().filter_map(|x| Path::new(x).parent()).map(|d| d.to_string_lossy().to_string()).find(|d| !d.is_empty());
    let Some(lib) = &mut s.device_sync.lib else { return Ok(json!({"folder": null})) };
    if let (Some(d), Some(cur)) = (parent, lib.replica.camera_roll.as_mut())
        && cur.is_empty()
    {
        *cur = d;
        lib.roll.stamp = None;
        lib.dirty = true;
    }
    Ok(json!({"folder": lib.replica.camera_roll}))
}

/// The camera-roll collection and its catalog collection, made when missing.
fn ensure_collection(s: &mut Session, lib: &mut Lib, device_name: &str) {
    let id = roll_id(&lib.replica.device);
    let mapped = lib.replica.albums.get(&id).copied().filter(|a| s.catalog.album(AlbumId(*a)).is_some());
    let album = match mapped {
        Some(a) => a,
        None => {
            let aid = s.catalog.alloc_album_id();
            if s.apply_system(Op::AddAlbum { album: Album::new(aid, roll_name(device_name)) }).is_err() {
                return;
            }
            lib.replica.asked.insert(aid.0);
            aid.0
        }
    };
    if lib.replica.collection(&id).is_none() {
        lib.replica.add_collection(&id, CollectionKind::CameraRoll, &roll_name(device_name), None, Grant::None, Some(album), now_ms());
    } else {
        lib.replica.albums.insert(id, album);
    }
}

/// The device was renamed: its camera roll follows.
pub(crate) fn renamed(s: &mut Session, device_name: &str) {
    let Some(lib) = &s.device_sync.lib else { return };
    let id = roll_id(&lib.replica.device);
    let Some(a) = lib.replica.albums.get(&id).copied() else { return };
    let name = roll_name(device_name);
    if s.catalog.album(AlbumId(a)).is_some_and(|x| x.name != name) {
        let _ = s.apply_system(Op::RenameAlbum { id: AlbumId(a), name });
    }
}

/// Every sync tick: the folder's stamp on a worker; when it changed, the folder is imported.
pub(crate) fn tick(s: &mut Session) {
    let Some(lib) = &mut s.device_sync.lib else { return };
    let Some(folder) = lib.replica.camera_roll.clone().filter(|f| !f.is_empty()) else { return };
    if let Some(rx) = &lib.roll.rx {
        match rx.try_recv() {
            Ok(stamp) => {
                lib.roll.rx = None;
                if stamp.is_some() && stamp != lib.roll.stamp {
                    lib.roll.stamp = stamp;
                    import_and_file(s, &folder);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => lib.roll.rx = None,
        }
        return;
    }
    if lib.roll.at.is_some_and(|t| t.elapsed() < EVERY) {
        return;
    }
    lib.roll.at = Some(Instant::now());
    let (tx, rx) = channel();
    lib.roll.rx = Some(rx);
    let dir = folder.clone();
    if std::thread::Builder::new()
        .name("camera-roll".into())
        .spawn(move || {
            let _ = tx.send(crate::folder_watch::stamp(&dir));
        })
        .is_err()
    {
        lib.roll.rx = None;
    }
}

/// `sync.cameraRollScan`: import the folder and file the collection now.
pub(crate) fn scan_now(s: &mut Session) -> Value {
    let folder = s.device_sync.lib.as_ref().and_then(|l| l.replica.camera_roll.clone()).filter(|f| !f.is_empty());
    match folder {
        Some(f) => {
            let n = import_and_file(s, &f);
            json!({"folder": f, "photos": n})
        }
        None => json!({"folder": null, "photos": 0}),
    }
}

/// Imports what is new in the folder (in place), then makes the collection hold every photo of
/// the catalog whose file is in it. → how many it holds.
fn import_and_file(s: &mut Session, folder: &str) -> usize {
    if let Err(e) = s.execute("library.import", &json!({"paths": [folder], "mode": "add"})) {
        log::info!("camera roll: {e}");
    }
    let name = s.device_sync.device_name();
    let Some(mut lib) = s.device_sync.lib.take() else { return 0 };
    ensure_collection(s, &mut lib, &name);
    let id = roll_id(&lib.replica.device);
    let mut n = 0;
    if let Some(a) = lib.replica.albums.get(&id).copied() {
        let mut photos: Vec<PhotoId> = s
            .catalog
            .photos()
            .filter(|p| !p.deleted && p.copy_of.is_none() && matches!(&p.source, Source::File { path } if crate::folder_watch::within(path, folder)))
            .map(|p| p.id)
            .collect();
        photos.sort();
        n = photos.len();
        let differs = s.catalog.album(AlbumId(a)).is_some_and(|al| {
            let mut cur = al.photos.clone();
            cur.sort();
            cur != photos
        });
        if differs {
            let _ = s.apply_system(Op::SetAlbumPhotos { id: AlbumId(a), photos });
        }
    }
    touched(&mut lib);
    s.device_sync.lib = Some(lib);
    n
}
