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

    /// Whether a place names download has been started in this session (running, or ended).
    #[cfg(test)]
    pub(crate) fn download_started(&self) -> bool {
        let sh = self.lock();
        sh.running || sh.last.is_some()
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
    // the place list holds towns of 15 000 people or more: a photo in a smaller one finds the nearest
    // larger town, which may be another municipality; past this many metres City is left empty (#2242)
    let max_distance = match p.get("maxDistance") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_f64().filter(|m| m.is_finite() && *m > 0.0).ok_or_else(|| bad(C, "`maxDistance` must be a positive number of metres"))?),
    };
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
        let Some(mut place) = names.reverse(*at) else { continue };
        // too far from the town found: State and Country still hold, City (and below) doesn't
        let too_far = max_distance.is_some_and(|m| place.distance > m);
        let nearest_city = too_far.then(|| std::mem::take(&mut place.city));
        if too_far {
            place.sublocation.clear();
        }
        let Some(ph) = s.catalog.photo(*id) else { continue };
        let mut meta = ph.meta.clone();
        fill(&mut meta, &place, overwrite);
        if meta != ph.meta {
            ops.push(Op::SetMeta { id: *id, meta: Box::new(meta) });
        }
        let mut entry = json!({"id": id.0, "place": place});
        if let Some(city) = nearest_city {
            entry["nearestCity"] = json!(city);
        }
        found.push(entry);
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
            "metadata.reverseGeocode",
            "Fill Location from GPS",
            ["Photo"],
            None,
            "{ids?, overwrite?: bool (also replace place fields that already have text; default false: only empty ones are filled), dryRun?, dataset?: folder, maxDistance?: metres (past this distance from the town found, City is left empty and the town is given as nearestCity)} → {looked, found, changed, photos: [{id, place: {sublocation, city, state, country, iso, distance}, nearestCity?}], attribution} — fills City / State / Country (and Sublocation when known) from GPS with the offline GeoNames place names (download them first); one undo step. The place names list towns of 15 000 people or more (GeoNames cities15000): a photo taken in a smaller town gets the nearest larger one, up to 50 km away (`distance` says how far); `maxDistance` keeps such a town out of City",
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

    /// Issue #2242: the place names hold only towns of 15 000 people or more, so a photo in a
    /// village finds the nearest larger town. `maxDistance` leaves City empty past that distance
    /// (State and Country still fill) and says which town it was.
    #[test]
    fn max_distance_leaves_a_far_town_out_of_city() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let dir = dataset("maxdist");
        // about 5 km north of Paris (like Portofino and Rapallo in the issue), and right in Paris
        set_gps(&mut s, ids[0], Some((48.90, 2.35)), "");
        set_gps(&mut s, ids[1], Some((48.855, 2.35)), "");
        let r = s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0, ids[1].0], "dataset": dir, "maxDistance": 2000})).unwrap();
        let m0 = s.catalog.photo(ids[0]).unwrap().meta.clone();
        assert_eq!((m0.city.as_str(), m0.state.as_str(), m0.country.as_str()), ("", "Île-de-France", "France"));
        assert_eq!(s.catalog.photo(ids[1]).unwrap().meta.city, "Paris", "close enough");
        let far = r["photos"].as_array().unwrap().iter().find(|p| p["id"] == json!(ids[0].0)).unwrap();
        assert_eq!((far["place"]["city"].as_str(), far["nearestCity"].as_str()), (Some(""), Some("Paris")), "{far}");
        assert!(far["place"]["distance"].as_f64().unwrap() > 2000.0);
        // without it, the nearest town fills City as before
        s.undo_step().unwrap();
        s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0], "dataset": dir})).unwrap();
        assert_eq!(s.catalog.photo(ids[0]).unwrap().meta.city, "Paris");
        // hostile values are refused, not taken as "no limit"
        for bad in [json!(-1), json!(0), json!("far"), json!(true)] {
            let e = s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0], "dataset": dir, "maxDistance": bad})).unwrap_err();
            assert!(e.to_string().contains("maxDistance"), "{e}");
        }
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
