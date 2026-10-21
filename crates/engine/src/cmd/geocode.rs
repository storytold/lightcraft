//! Reverse geocoding (issue #582): fill City / State / Country from a photo's GPS position, offline, with the
//! GeoNames place names ([`lightcraft_geocode`]). The place names (CC-BY 4.0) are not shipped with the app: the user
//! downloads them once (`metadata.downloadPlaceNames`, ~3 MB from download.geonames.org) into the settings folder;
//! after that every lookup is local and no position ever leaves the machine.
//!
//! Sources: GeoNames dump readme (prose); own design.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use serde_json::{Value, json};

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_geocode::{GEONAMES_ATTRIBUTION, GEONAMES_FILES, GeoNames, LatLon};

use super::{CommandSpec, always, bad, bool_or, cmd, has_selection, str_param};
use crate::activity::Cancel;
use crate::{Result, Session};

/// Where the place names live: `dataset` (a folder) or `<settings>/geonames`.
fn dataset_dir(p: &Value) -> Option<PathBuf> {
    str_param(p, "dataset").map(PathBuf::from).or_else(|| crate::config::config_dir().map(|d| d.join("geonames")))
}

/// How a place-names download ended.
#[derive(Clone, Debug, PartialEq)]
pub enum DownloadOutcome {
    /// Done: this many places.
    Done(usize),
    /// Failed, in words the user can act on.
    Failed(String),
    Cancelled,
}

#[derive(Default)]
struct DownloadShared {
    running: bool,
    last: Option<DownloadOutcome>,
    /// The last outcome, until the UI has shown it ([`PlaceNames::take_outcome`]).
    unreported: Option<DownloadOutcome>,
}

/// The session's place names: the parsed gazetteer (kept while its file is unchanged) and the download state.
#[derive(Default)]
pub struct PlaceNames {
    shared: Arc<Mutex<DownloadShared>>,
    /// (cities file, its size, its modification time, the parsed names).
    cache: Option<(PathBuf, u64, Option<SystemTime>, Arc<GeoNames>)>,
    pub(crate) fill_job: Option<FillJob>,
}

/// A snapshot of the visible photos; apply against fresh metadata only when the whole lookup finishes.
pub(crate) struct FillJob {
    epoch: u64,
    todo: Vec<(PhotoId, LatLon)>,
    cursor: usize,
    found: Vec<(PhotoId, LatLon, lightcraft_geocode::Place)>,
    names: Arc<GeoNames>,
    task: crate::activity::TaskGuard,
}

fn start_fill(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "metadata.fillLocations";
    if s.place_names.fill_job.is_some() {
        return Err(bad(C, "location filling is already running"));
    }
    let mut ids = if p.get("ids").is_some() { s.targets(p)? } else { s.visible_cloned() };
    ids.sort_unstable();
    ids.dedup();
    let todo: Vec<_> = ids
        .into_iter()
        .filter_map(|id| {
            let ph = s.catalog.photo(id)?;
            if ph.deleted || (!ph.meta.city.trim().is_empty() && !ph.meta.state.trim().is_empty() && !ph.meta.country.trim().is_empty()) {
                return None;
            }
            let (lat, lon) = ph.meta.gps?;
            let at = LatLon::new(lat, lon);
            at.is_valid().then_some((id, at))
        })
        .collect();
    if todo.is_empty() {
        return Ok(json!({"started":false,"total":0}));
    }
    let dir = dataset_dir(p).ok_or_else(|| bad(C, "no settings folder: give `dataset`"))?;
    let names = s.place_names.load(&dir).map_err(|e| bad(C, format!("{e}: download the place names first (Photo → Download Place Names)")))?;
    let total = todo.len();
    let task = s.activity.start("geocode", "Filling locations", Cancel::Yes);
    task.progress(0, total as u64);
    s.place_names.fill_job = Some(FillJob { epoch: s.library_generation(), todo, cursor: 0, found: Vec::new(), names, task });
    Ok(json!({"started":true,"total":total}))
}

fn pump_fill(s: &mut Session, _: &Value) -> Result<Value> {
    let Some(mut job) = s.place_names.fill_job.take() else {
        return Ok(json!({"active":false}));
    };
    if job.epoch != s.library_generation() || job.task.is_cancelled() {
        return Ok(json!({"active":false,"cancelled":true}));
    }
    let total = job.todo.len();
    let end = job.cursor.saturating_add(256).min(total);
    for &(id, at) in job.todo.get(job.cursor..end).unwrap_or_default() {
        if let Some(place) = job.names.reverse(at) {
            job.found.push((id, at, place));
        }
    }
    job.cursor = end;
    job.task.progress(end as u64, total as u64);
    if end < total {
        s.place_names.fill_job = Some(job);
        return Ok(json!({"active":true,"done":end,"total":total}));
    }
    let mut ops = Vec::new();
    for (id, at, place) in job.found {
        let Some(ph) = s.catalog.photo(id) else { continue };
        if ph.deleted || ph.meta.gps != Some((at.lat, at.lon)) {
            continue;
        }
        let mut meta = ph.meta.clone();
        fill(&mut meta, &place, false);
        if meta != ph.meta {
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
        }
    }
    let changed = ops.len();
    if changed > 0 {
        s.commit("Fill locations in view", Op::Batch { ops })?;
    }
    Ok(json!({"active":false,"complete":true,"changed":changed,"total":total}))
}

impl PlaceNames {
    fn lock(&self) -> std::sync::MutexGuard<'_, DownloadShared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A download that ended since the last call (the UI shows it once: an error stays readable as a toast).
    pub fn take_outcome(&self) -> Option<DownloadOutcome> {
        self.lock().unreported.take()
    }

    pub fn downloading(&self) -> bool {
        self.lock().running
    }

    /// The gazetteer in `dir`, parsed once and re-read only when the cities file's size or time changes.
    fn load(&mut self, dir: &Path) -> std::result::Result<Arc<GeoNames>, lightcraft_geocode::GeocodeError> {
        let file = dir.join(GEONAMES_FILES[0]);
        let meta = std::fs::metadata(&file).ok();
        let (len, mtime) = (meta.as_ref().map_or(0, |m| m.len()), meta.as_ref().and_then(|m| m.modified().ok()));
        if let Some((f, l, t, g)) = &self.cache
            && meta.is_some()
            && *f == file
            && *l == len
            && *t == mtime
        {
            return Ok(g.clone());
        }
        let g = Arc::new(GeoNames::load(dir)?);
        self.cache = Some((file, len, mtime, g.clone()));
        Ok(g)
    }
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let dir = dataset_dir(p);
    let size = dir.as_ref().and_then(|d| std::fs::metadata(d.join(GEONAMES_FILES[0])).ok()).map(|m| m.len());
    let sh = s.place_names.lock();
    let (last, error) = match &sh.last {
        None => (Value::Null, Value::Null),
        Some(DownloadOutcome::Done(n)) => (json!({"places": n}), Value::Null),
        Some(DownloadOutcome::Failed(e)) => (json!("failed"), json!(e)),
        Some(DownloadOutcome::Cancelled) => (json!("cancelled"), Value::Null),
    };
    Ok(json!({
        "downloaded": size.is_some(), "bytes": size, "path": dir, "downloading": sh.running, "lastDownload": last, "lastError": error,
        "attribution": GEONAMES_ATTRIBUTION, "source": lightcraft_geocode::GEONAMES_BASE,
    }))
}

/// Which place fields to write: a field is filled when it is empty, or always with `overwrite`.
fn fill(meta: &mut lightcraft_catalog::Meta, place: &lightcraft_geocode::Place, overwrite: bool) {
    for (field, value) in [
        (&mut meta.location, &place.sublocation),
        (&mut meta.city, &place.city),
        (&mut meta.state, &place.state),
        (&mut meta.country, &place.country),
    ] {
        if !value.is_empty() && (overwrite || field.trim().is_empty()) {
            *field = value.clone();
        }
    }
}

fn reverse(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "metadata.reverseGeocode";
    let overwrite = bool_or(p, "overwrite", false);
    let ids = s.targets(p)?;
    let todo: Vec<(PhotoId, LatLon)> = ids
        .iter()
        .filter_map(|id| {
            let (la, lo) = s.catalog.photo(*id)?.meta.gps?;
            let at = LatLon::new(la, lo);
            at.is_valid().then_some((*id, at))
        })
        .collect();
    if todo.is_empty() {
        return Err(bad(C, "none of these photos has a GPS position"));
    }
    let dir = dataset_dir(p).ok_or_else(|| bad(C, "no settings folder: give `dataset`"))?;
    let names = s.place_names.load(&dir).map_err(|e| bad(C, format!("{e}: download the place names first (Photo ▸ Download Place Names)")))?;
    // a large selection shows in the activity stack and can be stopped
    let task = (todo.len() > 1).then(|| s.activity.start("geocode", "Looking up places", Cancel::Yes));
    let mut ops = Vec::new();
    let mut found = Vec::new();
    for (n, (id, at)) in todo.iter().enumerate() {
        if let Some(t) = &task {
            if t.cancel_flag().is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed)) {
                return Err(bad(C, "cancelled"));
            }
            t.progress(n as u64, todo.len() as u64);
        }
        let Some(place) = names.reverse(*at) else { continue };
        let Some(ph) = s.catalog.photo(*id) else { continue };
        let mut meta = ph.meta.clone();
        fill(&mut meta, &place, overwrite);
        if meta != ph.meta {
            ops.push(Op::SetMeta { id: *id, meta: Box::new(meta) });
        }
        found.push(json!({"id": id.0, "place": place}));
    }
    drop(task);
    let changed = ops.len();
    if !bool_or(p, "dryRun", false) && changed > 0 {
        s.commit("Reverse Geocode", Op::Batch { ops })?;
    }
    Ok(json!({"looked": todo.len(), "found": found.len(), "changed": changed, "photos": found, "attribution": GEONAMES_ATTRIBUTION}))
}

#[cfg(not(target_arch = "wasm32"))]
mod download {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use lightcraft_geocode::{GEONAMES_BASE, GEONAMES_ZIP};

    use super::*;
    use crate::activity::Unit;

    const MAX_ZIP: u64 = 64 * 1024 * 1024;
    const MAX_TEXT: usize = 256 * 1024 * 1024;

    fn get(name: &str, base: &str, dir: &Path, cancel: &AtomicBool, task: &crate::activity::TaskHandle) -> std::result::Result<(), String> {
        let _ = std::fs::remove_file(dir.join(name));
        let file = lightcraft_fetch::FileSpec { name, size: None, sha256: None, max: MAX_ZIP };
        lightcraft_fetch::download_url(&file, &format!("{base}{name}"), dir, &lightcraft_fetch::Options::default(), cancel, &mut |pr| {
            task.progress(pr.done, pr.total.max(pr.done))
        })
        .map_err(|e| e.to_string())
    }

    /// Fetch and unpack the place names into `dir`; the new files replace the old ones only when complete.
    pub(super) fn run(base: &str, dir: &Path, cancel: &AtomicBool, task: &crate::activity::TaskHandle) -> std::result::Result<usize, String> {
        let staging = dir.join(".download");
        std::fs::create_dir_all(&staging).map_err(|e| format!("could not create {}: {e}", staging.display()))?;
        get(GEONAMES_ZIP, base, &staging, cancel, task)?;
        let zip = std::fs::read(staging.join(GEONAMES_ZIP)).map_err(|e| e.to_string())?;
        let cities = lightcraft_geocode::unzip::extract(&zip, GEONAMES_FILES[0], MAX_TEXT).map_err(|e| e.to_string())?;
        let places = GeoNames::parse(&String::from_utf8_lossy(&cities), None, None).len();
        if places == 0 {
            return Err("the downloaded place list is empty".into());
        }
        let _ = std::fs::remove_file(staging.join(GEONAMES_ZIP));
        let write = |name: &str, data: &[u8]| std::fs::write(dir.join(name), data).map_err(|e| format!("could not save {name}: {e}"));
        for name in &GEONAMES_FILES[1..] {
            // optional: without them states stay blank and countries are ISO codes
            match get(name, base, &staging, cancel, task).and_then(|()| std::fs::read(staging.join(name)).map_err(|e| e.to_string())) {
                Ok(b) => write(name, &b)?,
                Err(e) => log::warn!("place names: {name}: {e}"),
            }
            let _ = std::fs::remove_file(staging.join(name));
        }
        write(GEONAMES_FILES[0], &cities)?;
        let _ = std::fs::remove_dir(&staging);
        Ok(places)
    }

    /// A download base: https, or plain http to this machine's loopback address only (a test
    /// server), with nothing but a port or path after the host, so `http://127.0.0.1.example.com/`
    /// is refused.
    pub(super) fn base_allowed(base: &str) -> bool {
        base.starts_with("https://")
            || base.strip_prefix("http://127.0.0.1").is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('/'))
    }

    /// `metadata.downloadPlaceNames`: starts a background download (shown in the activity stack, cancellable).
    pub(super) fn start(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "metadata.downloadPlaceNames";
        let dir = dataset_dir(p).ok_or_else(|| bad(C, "no settings folder: give `dataset`"))?;
        let base = str_param(p, "base").unwrap_or(GEONAMES_BASE).to_string();
        if !base_allowed(&base) {
            return Err(bad(C, "`base` must be an https:// address"));
        }
        let shared = s.place_names.shared.clone();
        {
            let mut sh = s.place_names.lock();
            if sh.running {
                return Err(bad(C, "the place names are already downloading"));
            }
            sh.running = true;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let task = s.activity.start("download", "Downloading place names", Cancel::Flag(cancel.clone()));
        task.set_unit(Unit::Bytes);
        task.detail("GeoNames");
        let handle = task.handle();
        let thread_dir = dir.clone();
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new().name("place-names-download".into()).spawn(move || {
            // the activity row goes when the thread ends, however it ends
            let _task = task;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&base, &thread_dir, &cancel, &handle)));
            let outcome = match outcome {
                Ok(Ok(n)) => DownloadOutcome::Done(n),
                _ if cancel.load(std::sync::atomic::Ordering::Relaxed) => DownloadOutcome::Cancelled,
                Ok(Err(e)) => DownloadOutcome::Failed(format!("Could not download the place names: {e}. Check the connection and try again.")),
                Err(_) => DownloadOutcome::Failed("The place names download stopped unexpectedly. Try again.".into()),
            };
            if let DownloadOutcome::Failed(e) = &outcome {
                log::warn!("{e}");
            }
            let mut sh = thread_shared.lock().unwrap_or_else(PoisonError::into_inner);
            sh.running = false;
            sh.last = Some(outcome.clone());
            sh.unreported = Some(outcome);
        });
        if let Err(e) = spawned {
            shared.lock().unwrap_or_else(PoisonError::into_inner).running = false;
            return Err(bad(C, format!("could not start the download: {e}")));
        }
        Ok(json!({"started": true, "path": dir, "attribution": GEONAMES_ATTRIBUTION}))
    }
}

pub fn specs() -> Vec<CommandSpec> {
    #[allow(unused_mut)]
    let mut v = vec![
        cmd!(
            "metadata.fillLocations",
            "Fill Locations in Current View",
            ["Photo"],
            None,
            "{ids?, dataset?: folder} — fill missing City / State / Country for a snapshot of the current view (folder, subfolders and filters included); lookup advances with metadata.pumpLocations, cancellable, one undo step; saves in catalog and follows existing XMP preferences",
            always,
            start_fill
        ),
        cmd!(query "metadata.pumpLocations", "Advance Location Filling", [], None,
            "{} → {active, done?, total?, complete?, changed?, cancelled?} — advance a bounded batch of offline lookups; current metadata is preserved", always, pump_fill),
        cmd!(
            "metadata.reverseGeocode",
            "Fill Location from GPS",
            ["Photo"],
            None,
            "{ids?, overwrite?: bool (also replace place fields that already have text; default false: only empty ones are filled), dryRun?, dataset?: folder} → {looked, found, changed, photos: [{id, place: {sublocation, city, state, country, iso, distance}}], attribution} — fills City / State / Country (and Sublocation when known) from GPS with the offline GeoNames place names (download them first); one undo step",
            has_selection,
            reverse
        ),
        cmd!(query "metadata.placeNamesStatus", "Place Names Status", [], None, "{dataset?: folder} → {downloaded, bytes, path, attribution, source}", always, status),
    ];
    #[cfg(not(target_arch = "wasm32"))]
    v.push(cmd!(
        "metadata.downloadPlaceNames",
        "Download Place Names (GeoNames)",
        ["Photo"],
        None,
        "{dataset?: folder} → {started, path, attribution} — downloads the GeoNames cities list (CC-BY 4.0, ~3 MB, from download.geonames.org) in the background for Fill Location from GPS; no photo data is sent",
        always,
        download::start
    ));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const CITIES: &str = "2988507\tParis\tParis\t\t48.85341\t2.3488\tP\tPPLC\tFR\t\t11\t75\t\t\t2138551\t\t42\tEurope/Paris\t2024-01-01\n\
                          2643743\tLondon\tLondon\t\t51.50853\t-0.12574\tP\tPPLC\tGB\t\tENG\t\t\t\t8961989\t\t25\tEurope/London\t2024-01-01\n";

    fn dataset(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightcraft-geocode-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(GEONAMES_FILES[0]), CITIES).unwrap();
        std::fs::write(dir.join(GEONAMES_FILES[1]), "FR.11\tÎle-de-France\nGB.ENG\tEngland\n").unwrap();
        std::fs::write(dir.join(GEONAMES_FILES[2]), "#c\nFR\tFRA\t250\tFR\tFrance\nGB\tGBR\t826\tUK\tUnited Kingdom\n").unwrap();
        dir
    }

    fn set_gps(s: &mut Session, id: PhotoId, gps: Option<(f64, f64)>, city: &str) {
        let mut meta = s.catalog.photo(id).unwrap().meta.clone();
        meta.gps = gps;
        meta.city = city.into();
        meta.state.clear();
        meta.country.clear();
        meta.location.clear();
        s.commit("test", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }

    fn view_photo(s: &mut Session, id: u64, path: &str, gps: Option<(f64, f64)>) {
        let mut photo =
            lightcraft_catalog::Photo::new(PhotoId(id), lightcraft_catalog::Source::File { path: path.into() }, path, "JPEG", 10, 10, "2026-10-19");
        photo.local = true;
        photo.meta.gps = gps;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }

    #[test]
    fn view_fill_respects_local_folder_filters_and_saves_on_reopen() {
        let dir = dataset("view-fill");
        let library = dir.join("library");
        let mut s = Session::new();
        s.open_library(&library, false).unwrap();
        for (id, path, gps) in [
            (1, "/photos/a.jpg", Some((48.86, 2.34))),
            (2, "/photos/child/b.jpg", Some((51.5, -0.1))),
            (3, "/outside/c.jpg", Some((48.86, 2.34))),
            (4, "/photos/no-gps.jpg", None),
        ] {
            let mut photo = lightcraft_catalog::Photo::new(
                PhotoId(id),
                lightcraft_catalog::Source::File { path: path.into() },
                path,
                "JPEG",
                10,
                10,
                "2026-10-19",
            );
            photo.local = true;
            photo.meta.gps = gps;
            s.commit("fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        s.source = crate::LibrarySource::Folder;
        s.browse = Some(crate::Browse { path: "/photos".into(), subfolders: false });
        assert_eq!(s.execute("metadata.fillLocations", &json!({"dataset":dir})).unwrap()["total"], 1);
        let done = s.execute("metadata.pumpLocations", &json!({})).unwrap();
        assert_eq!(done["changed"], 1);
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().meta.city, "Paris");
        assert!(s.catalog.photo(PhotoId(2)).unwrap().meta.city.is_empty());
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.catalog.photo(PhotoId(1)).unwrap().meta.city.is_empty());
        s.browse.as_mut().unwrap().subfolders = true;
        s.filter.text = "b.jpg".into();
        assert_eq!(s.execute("metadata.fillLocations", &json!({"dataset":dir})).unwrap()["total"], 1);
        s.execute("metadata.pumpLocations", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(PhotoId(2)).unwrap().meta.city, "London");
        assert!(s.catalog.photo(PhotoId(3)).unwrap().meta.city.is_empty());
        s.close_library().unwrap();
        drop(s);
        let mut reopened = Session::new();
        reopened.open_library(&library, false).unwrap();
        assert_eq!(reopened.catalog.photo(PhotoId(2)).unwrap().meta.city, "London");
        assert_eq!(reopened.catalog.photo(PhotoId(2)).unwrap().meta.country, "United Kingdom");
    }

    #[test]
    fn cancelling_view_fill_is_atomic_and_late_edits_are_kept() {
        let dir = dataset("cancel-view-fill");
        let mut s = Session::new();
        for id in 1..=300 {
            view_photo(&mut s, id, &format!("/photos/{id}.jpg"), Some((48.86, 2.34)));
        }
        s.source = crate::LibrarySource::Folder;
        s.browse = Some(crate::Browse { path: "/photos".into(), subfolders: true });
        s.execute("metadata.fillLocations", &json!({"dataset":dir})).unwrap();
        assert_eq!(s.execute("metadata.pumpLocations", &json!({})).unwrap()["done"], 256);
        assert!(s.catalog.photo(PhotoId(1)).unwrap().meta.city.is_empty());
        let id = s.activity.list()[0].id;
        s.activity.cancel(id).unwrap();
        assert_eq!(s.execute("metadata.pumpLocations", &json!({})).unwrap()["cancelled"], true);
        assert!(s.catalog.photos().all(|p| p.meta.city.is_empty()));
        s.execute("metadata.fillLocations", &json!({"dataset":dir})).unwrap();
        s.execute("metadata.pumpLocations", &json!({})).unwrap();
        set_gps(&mut s, PhotoId(1), Some((48.86, 2.34)), "My city");
        set_gps(&mut s, PhotoId(2), Some((51.5, -0.1)), "");
        s.execute("metadata.pumpLocations", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().meta.city, "My city");
        assert!(s.catalog.photo(PhotoId(2)).unwrap().meta.city.is_empty(), "Changed GPS must not receive a stale lookup");
        assert_eq!(s.catalog.photo(PhotoId(3)).unwrap().meta.city, "Paris");
        assert!(s.activity.list().is_empty());
    }

    #[test]
    fn manual_fill_persists_and_all_metadata_shows_saved_catalog_values() {
        let dir = dataset("saved-values");
        let library = dir.join("library");
        let mut s = Session::new();
        s.open_library(&library, false).unwrap();
        crate::demo::load(&mut s);
        let id = s.visible_cloned()[0];
        set_gps(&mut s, id, Some((48.86, 2.34)), "");
        s.execute("metadata.reverseGeocode", &json!({"ids":[id.0],"dataset":dir})).unwrap();
        s.close_library().unwrap();
        drop(s);
        let mut s = Session::new();
        s.open_library(&library, false).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().meta.city, "Paris");
        let rows = s.execute("photo.allMetadata", &json!({"id":id.0})).unwrap();
        assert!(rows["catalog"].as_array().unwrap().iter().any(|r| r["name"] == "City" && r["value"] == "Paris"));
    }

    #[test]
    fn fills_empty_fields_keeps_typed_ones_and_undoes() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let dir = dataset("fill");
        set_gps(&mut s, ids[0], Some((48.86, 2.34)), "");
        set_gps(&mut s, ids[1], Some((51.5, -0.1)), "Westminster");
        set_gps(&mut s, ids[2], Some((f64::NAN, 0.0)), "");
        set_gps(&mut s, ids[3], Some((0.0, 0.0)), "");
        let r = s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0, ids[1].0, ids[2].0, ids[3].0], "dataset": dir})).unwrap();
        assert_eq!((r["looked"].as_u64(), r["found"].as_u64(), r["changed"].as_u64()), (Some(3), Some(2), Some(2)), "{r}");
        let m0 = s.catalog.photo(ids[0]).unwrap().meta.clone();
        assert_eq!((m0.city.as_str(), m0.state.as_str(), m0.country.as_str()), ("Paris", "Île-de-France", "France"));
        let m1 = s.catalog.photo(ids[1]).unwrap().meta.clone();
        assert_eq!((m1.city.as_str(), m1.state.as_str(), m1.country.as_str()), ("Westminster", "England", "United Kingdom"));
        assert!(s.activity.list().is_empty(), "the activity row ends with the command");
        // one undo step
        s.undo_step().unwrap();
        assert!(s.catalog.photo(ids[0]).unwrap().meta.city.is_empty());
        assert_eq!(s.catalog.photo(ids[1]).unwrap().meta.city, "Westminster");
        // overwrite replaces typed text
        s.execute("metadata.reverseGeocode", &json!({"ids": [ids[1].0], "dataset": dir, "overwrite": true})).unwrap();
        assert_eq!(s.catalog.photo(ids[1]).unwrap().meta.city, "London");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explains_missing_data_and_gps() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        set_gps(&mut s, ids[0], None, "");
        let e = s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0]})).unwrap_err();
        assert!(e.to_string().contains("GPS"), "{e}");
        set_gps(&mut s, ids[0], Some((48.86, 2.34)), "");
        let missing = std::env::temp_dir().join(format!("lightcraft-geocode-none-{}", std::process::id()));
        let e = s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0], "dataset": missing})).unwrap_err();
        assert!(e.to_string().contains("download"), "{e}");
        let st = s.execute("metadata.placeNamesStatus", &json!({"dataset": missing})).unwrap();
        assert_eq!(st["downloaded"], false);
    }

    #[test]
    fn the_gazetteer_is_cached_until_its_file_changes() {
        let dir = dataset("cache");
        let mut names = PlaceNames::default();
        let a = names.load(&dir).unwrap();
        assert!(Arc::ptr_eq(&a, &names.load(&dir).unwrap()), "unchanged file: no re-read");
        // a different size invalidates it (a new download)
        std::fs::write(dir.join(GEONAMES_FILES[0]), "1\tRome\tRome\t\t41.9\t12.5\tP\tP\tIT\t\t07\n").unwrap();
        let b = names.load(&dir).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(b.reverse(LatLon::new(41.9, 12.5)).unwrap().city, "Rome");
        std::fs::remove_file(dir.join(GEONAMES_FILES[0])).unwrap();
        assert!(names.load(&dir).is_err(), "a deleted file is not served from the cache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A local HTTP server; the first path segment picks the behaviour: `good` (the synthetic dump), `garbage` (a zip
    /// that isn't one), `short` (the body ends before its Content-Length), anything else 404.
    #[cfg(not(target_arch = "wasm32"))]
    fn serve() -> String {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let zip = lightcraft_geocode::unzip::make(GEONAMES_FILES[0], CITIES.as_bytes());
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let (status, body, len): (&str, Vec<u8>, Option<usize>) = match (path.split('/').nth(1).unwrap_or(""), path.ends_with(".zip")) {
                    ("good", true) => ("200 OK", zip.clone(), None),
                    ("good", false) if path.contains("countryInfo") => ("200 OK", b"FR\tFRA\t250\tFR\tFrance\n".to_vec(), None),
                    ("garbage", true) => ("200 OK", b"<html>not a zip</html>".to_vec(), None),
                    ("short", true) => ("200 OK", zip[..zip.len() / 2].to_vec(), Some(zip.len())),
                    _ => ("404 Not Found", b"nope".to_vec(), None),
                };
                let n = len.unwrap_or(body.len());
                let _ = c.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n").as_bytes());
                let _ = c.write_all(&body);
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn download(s: &mut Session, base: &str, dir: &Path) -> DownloadOutcome {
        s.execute("metadata.downloadPlaceNames", &json!({"dataset": dir, "base": base})).unwrap();
        assert!(s.place_names.downloading() || s.place_names.lock().unreported.is_some());
        let started = std::time::Instant::now();
        loop {
            if let Some(o) = s.place_names.take_outcome() {
                // the activity row ends with the thread
                while !s.activity.list().is_empty() && started.elapsed().as_secs() < 30 {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(s.activity.list().is_empty());
                return o;
            }
            assert!(started.elapsed().as_secs() < 60, "the download never ended");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn download_end_to_end_and_its_failures() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let base = serve();
        let dir = std::env::temp_dir().join(format!("lightcraft-geocode-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(s.execute("metadata.downloadPlaceNames", &json!({"dataset": dir, "base": "http://example.com/"})).is_err());

        for bad in ["missing", "garbage", "short"] {
            let o = download(&mut s, &format!("{base}/{bad}/"), &dir);
            let DownloadOutcome::Failed(e) = o else { panic!("{bad}: {o:?}") };
            assert!(e.contains("try again"), "{bad}: {e}");
            let st = s.execute("metadata.placeNamesStatus", &json!({"dataset": dir})).unwrap();
            assert_eq!((st["downloaded"].as_bool(), st["lastDownload"].as_str()), (Some(false), Some("failed")), "{bad}: {st}");
            assert_eq!(st["lastError"].as_str(), Some(e.as_str()));
            assert!(s.place_names.take_outcome().is_none(), "reported once");
        }

        assert_eq!(download(&mut s, &format!("{base}/good/"), &dir), DownloadOutcome::Done(2));
        let st = s.execute("metadata.placeNamesStatus", &json!({"dataset": dir})).unwrap();
        assert_eq!((st["downloaded"].as_bool(), st["lastError"].is_null(), st["lastDownload"]["places"].as_u64()), (Some(true), true, Some(2)));
        set_gps(&mut s, ids[0], Some((48.86, 2.34)), "");
        s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0], "dataset": dir})).unwrap();
        let m = s.catalog.photo(ids[0]).unwrap().meta.clone();
        // admin1 was a 404 (optional): the state stays blank, the rest is filled
        assert_eq!((m.city.as_str(), m.state.as_str(), m.country.as_str()), ("Paris", "", "France"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_base_is_https_or_loopback_only() {
        assert!(download::base_allowed("https://download.geonames.org/export/dump/"));
        assert!(download::base_allowed("http://127.0.0.1:8080"));
        assert!(download::base_allowed("http://127.0.0.1/dump"));
        assert!(!download::base_allowed("http://127.0.0.1.example.com/"));
        assert!(!download::base_allowed("http://127.0.0.1@example.com/"));
        assert!(!download::base_allowed("http://download.geonames.org/"));
    }
}
