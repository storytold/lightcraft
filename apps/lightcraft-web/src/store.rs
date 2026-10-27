//! Imported originals on the web. There is no filesystem: the bytes of every photo picked or
//! dropped in the browser are stored in browser storage as `originals/<content hash>`, and the
//! catalog refers to them by a synthetic path `web/<content hash>/<file name>` (so the decoders
//! still see the file's extension).
//!
//! The engine reads files synchronously ([`FileLoader`]/[`FileProbe`]), so the main thread keeps
//! the bytes it needs in a bounded memory cache ([`Originals`]). A miss (e.g. after a reload)
//! queues an asynchronous load from storage ([`Originals::take_wanted`]) and fails for now; the
//! host prefetches the active photo's original so commands that need its pixels (auto tone,
//! export) find it. Render workers read originals from storage themselves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lightcraft_engine::files::{load_bytes, probe_bytes};
use lightcraft_engine::media::{FileLoader, FileProbe};

/// Prefix of the catalog paths of files kept in browser storage.
pub const PATH_PREFIX: &str = "web/";

/// Validate a browser File/Blob size before reading or copying its whole ArrayBuffer.
pub fn check_original_size(size: f64) -> Result<(), String> {
    if !size.is_finite() || !(0.0..=9_007_199_254_740_991.0).contains(&size) || size.fract() != 0.0 {
        return Err("invalid original file size".into());
    }
    lightcraft_engine::files::check_buffered_original_size(size as u64)
}

/// Copy a browser buffer into WASM, reporting allocation failure instead of aborting the app.
#[cfg(target_arch = "wasm32")]
pub fn copy_browser_bytes(bytes: &js_sys::Uint8Array) -> Result<Vec<u8>, String> {
    check_original_size(f64::from(bytes.length()))?;
    let len = bytes.length() as usize;
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|e| format!("can't allocate original buffer: {e}"))?;
    out.resize(len, 0);
    bytes.copy_to(&mut out);
    Ok(out)
}

/// Catalog path for a stored original.
pub fn original_path(hash: &str, name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or("photo");
    format!("{PATH_PREFIX}{hash}/{name}")
}

/// The content hash in a path made by [`original_path`].
pub fn hash_of_path(path: &str) -> Option<&str> {
    let (hash, _) = path.strip_prefix(PATH_PREFIX)?.split_once('/')?;
    (!hash.is_empty()).then_some(hash)
}

/// Storage key of an original.
pub fn storage_key(hash: &str) -> String {
    format!("originals/{hash}")
}

/// Hash of a file's bytes (the same as the catalog's `content_hash`).
pub fn content_hash(bytes: &[u8]) -> String {
    lightcraft_preview::hash_bytes(bytes).to_string()
}

#[derive(Default)]
struct Inner {
    /// hash → (bytes, last use).
    bytes: HashMap<String, (Arc<Vec<u8>>, u64)>,
    total: usize,
    clock: u64,
    /// Catalog paths stored since the last [`Originals::take_pending`] (to import on the next frame).
    pending: Vec<String>,
    /// Hashes to load from storage.
    wanted: Vec<String>,
    /// Hashes being loaded.
    loading: std::collections::HashSet<String>,
    failed: std::collections::HashSet<String>,
    selection: (Vec<lightcraft_engine::catalog::PhotoId>, Option<lightcraft_engine::catalog::PhotoId>),
    /// Bounded metadata for newly imported videos; their originals remain browser Blobs.
    probes: HashMap<String, lightcraft_engine::media::ProbeInfo>,
}

/// Original bytes in memory, shared between the engine hooks and the host (cheap to clone).
#[derive(Clone, Default)]
pub struct Originals(Arc<Mutex<Inner>>, lightcraft_engine::activity::Activity);

impl Originals {
    /// Share the session's activity stack with asynchronous browser imports.
    pub fn new(activity: lightcraft_engine::activity::Activity) -> Self {
        Self(Arc::default(), activity)
    }

    /// Keep a queued or running import visible until its browser I/O finishes.
    pub fn import_activity(&self, name: &str) -> lightcraft_engine::activity::TaskGuard {
        let guard = self.1.start("import", "Importing", lightcraft_engine::activity::Cancel::No);
        guard.detail(name);
        guard
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keep `bytes` (hash `hash`) in memory, evicting the least recently used others if needed.
    pub fn insert(&self, hash: &str, bytes: Arc<Vec<u8>>) -> Result<(), String> {
        lightcraft_engine::files::check_buffered_original_size(bytes.len() as u64)?;
        let cache_budget = lightcraft_engine::memory::cache_share(lightcraft_engine::memory::budget());
        let mut g = self.lock();
        g.clock += 1;
        let clock = g.clock;
        g.loading.remove(hash);
        g.failed.remove(hash);
        if let Some(old) = g.bytes.insert(hash.to_string(), (bytes.clone(), clock)) {
            g.total -= old.0.len();
        }
        g.total += bytes.len();
        while g.total > cache_budget && g.bytes.len() > 1 {
            let Some(oldest) = g.bytes.iter().filter(|(h, _)| *h != hash).min_by_key(|(_, v)| v.1).map(|(h, _)| h.clone()) else { break };
            if let Some(v) = g.bytes.remove(&oldest) {
                g.total -= v.0.len();
            }
        }
        Ok(())
    }

    /// A file was stored (bytes in storage and in memory): queue its path for import.
    pub fn added(&self, name: &str, hash: &str, bytes: Arc<Vec<u8>>) -> Result<String, String> {
        self.insert(hash, bytes)?;
        let path = original_path(hash, name);
        self.lock().pending.push(path.clone());
        Ok(path)
    }

    /// Queue a video whose full original is held by browser storage, not the byte cache.
    pub fn added_video(&self, name: &str, hash: &str, probe: lightcraft_engine::media::ProbeInfo) -> String {
        let path = original_path(hash, name);
        let mut g = self.lock();
        g.failed.remove(hash);
        g.probes.insert(hash.to_string(), probe);
        g.pending.push(path.clone());
        path
    }

    /// Bytes for a catalog path, if in memory. A miss for a stored original queues its load.
    pub fn get(&self, path: &str) -> Option<Arc<Vec<u8>>> {
        let hash = hash_of_path(path)?;
        let mut g = self.lock();
        g.clock += 1;
        let clock = g.clock;
        if let Some(v) = g.bytes.get_mut(hash) {
            v.1 = clock;
            return Some(v.0.clone());
        }
        if !g.failed.contains(hash) && !g.loading.contains(hash) && !g.wanted.iter().any(|w| w == hash) {
            g.wanted.push(hash.to_string());
        }
        None
    }

    pub fn contains(&self, path: &str) -> bool {
        hash_of_path(path).is_some_and(|h| self.lock().bytes.contains_key(h))
    }

    /// Ask for the original behind `path` to be loaded into memory (no-op if it's there).
    pub fn prefetch(&self, path: &str) {
        let _ = self.get(path);
    }

    /// Hashes to load from storage (they count as loading until [`Originals::insert`] or
    /// [`Originals::load_failed`]).
    pub fn take_wanted(&self) -> Vec<String> {
        let mut g = self.lock();
        let w = std::mem::take(&mut g.wanted);
        g.loading.extend(w.iter().cloned());
        w
    }

    /// Finish a Blob load whose bytes stay outside the WASM cache.
    pub fn load_finished(&self, hash: &str) {
        self.lock().loading.remove(hash);
    }

    /// Suppress automatic retries until the selection changes or a retry is requested.
    pub fn load_failed(&self, hash: &str) {
        let mut g = self.lock();
        g.loading.remove(hash);
        g.failed.insert(hash.to_string());
    }

    /// Whether this original's most recent storage load failed.
    pub fn has_failed(&self, path: &str) -> bool {
        hash_of_path(path).is_some_and(|hash| self.lock().failed.contains(hash))
    }

    /// A new selection gives previously failed originals another attempt.
    pub fn sync_selection(&self, ids: &[lightcraft_engine::catalog::PhotoId], active: Option<lightcraft_engine::catalog::PhotoId>) {
        let mut g = self.lock();
        if g.selection.0 != ids || g.selection.1 != active {
            g.failed.clear();
            g.selection = (ids.to_vec(), active);
        }
    }

    /// Paths added since the last call.
    pub fn take_pending(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().pending)
    }

    /// Release metadata staged for the just-completed import command.
    pub fn finish_import(&self) {
        self.lock().probes.clear();
    }

    /// (originals in memory, bytes).
    pub fn usage(&self) -> (usize, usize) {
        let g = self.lock();
        (g.bytes.len(), g.total)
    }

    /// Engine hooks that decode/probe from memory.
    pub fn hooks(&self) -> (FileLoader, FileProbe) {
        let s = self.clone();
        let loader: FileLoader = Arc::new(move |path: &str, max_edge: usize| {
            let bytes = s.get(path).ok_or_else(|| format!("{path}: the original is still loading from browser storage"))?;
            load_bytes(&bytes, max_edge)
        });
        let s = self.clone();
        let probe: FileProbe = Arc::new(move |path: &str| {
            if let Some(hash) = hash_of_path(path) {
                let mut g = s.lock();
                // Explicit imports/reloads may retry; ordinary render reads must not.
                g.failed.remove(hash);
                if let Some(probe) = g.probes.get(hash).cloned() {
                    return Ok(probe);
                }
            }
            let bytes = s.get(path).ok_or_else(|| format!("{path}: not loaded"))?;
            probe_bytes(path, &bytes)
        });
        (loader, probe)
    }

    /// Install the hooks into a session.
    pub fn install(&self, session: &mut lightcraft_engine::Session) {
        let (l, p) = self.hooks();
        session.media.file_loader = Some(l);
        session.media.file_probe = Some(p);
        let s = self.clone();
        session.media.preview_loader =
            Some(Arc::new(move |path: &str, max_edge: usize| lightcraft_engine::files::embedded_preview_srgb(&s.get(path)?, max_edge)));
    }
}

/// File name part of an export path (the browser download name).
pub fn download_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or("export.png")
}

/// MIME type for a download, by extension.
pub fn mime_for(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("tif" | "tiff") => "image/tiff",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("jxl") => "image/jxl",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
pub(crate) fn png_bytes(w: usize, h: usize) -> Vec<u8> {
    let bytes: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7) as u8, 128, 200, 255]).collect();
    let img = lightcraft_raster::Rgba8::from_bytes(w, h, &bytes).unwrap();
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths() {
        let p = original_path("abc123", "C:\\x\\a.png");
        assert_eq!(p, "web/abc123/a.png");
        assert_eq!(hash_of_path(&p), Some("abc123"));
        assert_eq!(hash_of_path("mem/1/a.png"), None);
        assert_eq!(storage_key("abc"), "originals/abc");
    }

    #[test]
    fn miss_queues_a_load_once() {
        let s = Originals::default();
        assert!(s.get("web/h1/a.png").is_none());
        assert!(s.get("web/h1/a.png").is_none());
        assert_eq!(s.take_wanted(), vec!["h1".to_string()]);
        assert!(s.get("web/h1/a.png").is_none(), "still loading");
        assert!(s.take_wanted().is_empty());
        s.insert("h1", Arc::from(vec![1u8, 2])).unwrap();
        assert_eq!(&*s.get("web/h1/b.jpg").unwrap(), &[1, 2]);
    }

    #[test]
    fn failed_originals_wait_for_selection_change_or_explicit_retry() {
        use lightcraft_engine::catalog::PhotoId;
        let s = Originals::default();
        s.sync_selection(&[PhotoId(1)], Some(PhotoId(1)));
        s.prefetch("web/h1/a.png");
        assert_eq!(s.take_wanted(), ["h1"]);
        s.load_failed("h1");
        for _ in 0..30 {
            s.sync_selection(&[PhotoId(1)], Some(PhotoId(1)));
            s.prefetch("web/h1/a.png");
            assert!(s.get("web/h1/a.png").is_none());
            assert!(s.take_wanted().is_empty());
        }
        assert!(s.has_failed("web/h1/a.png"));
        s.sync_selection(&[PhotoId(1)], Some(PhotoId(2)));
        s.prefetch("web/h1/a.png");
        assert_eq!(s.take_wanted(), ["h1"], "changing the active photo permits a retry");
        s.load_failed("h1");
        let (_, probe) = s.hooks();
        assert!(probe("web/h1/a.png").is_err(), "an explicit reload queues the missing original");
        s.prefetch("web/h1/a.png");
        assert_eq!(s.take_wanted(), ["h1"]);
        s.load_failed("h1");
        s.insert("h1", Arc::new(vec![1])).unwrap();
        assert!(!s.has_failed("web/h1/a.png"), "successful replacement clears the failure");
    }

    #[test]
    fn a_successful_blob_load_does_not_record_a_failure() {
        let s = Originals::default();
        s.prefetch("web/video/clip.mp4");
        assert_eq!(s.take_wanted(), ["video"]);
        s.load_finished("video");
        assert!(!s.has_failed("web/video/clip.mp4"));
        assert!(s.lock().loading.is_empty());
    }

    #[test]
    fn caching_an_original_moves_its_buffer_without_another_full_copy() {
        let s = Originals::default();
        let bytes = vec![1u8, 2, 3];
        let address = bytes.as_ptr();
        s.added("a.png", "h1", Arc::from(bytes)).unwrap();
        let cached = s.get("web/h1/a.png").unwrap();
        assert_eq!(cached.as_ptr(), address);
    }

    #[test]
    fn import_and_render_from_memory() {
        let store = Originals::default();
        let mut session = lightcraft_engine::Session::new();
        store.install(&mut session);
        let bytes = png_bytes(40, 30);
        let hash = content_hash(&bytes);
        let path = store.added("tiny.png", &hash, bytes.into()).unwrap();
        assert_eq!(store.take_pending(), vec![path.clone()]);
        let r = session.execute("library.import", &json!({"paths": [path]})).unwrap();
        let id = lightcraft_engine::catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        let p = session.catalog.photo(id).unwrap();
        assert_eq!((p.width, p.height), (40, 30));
        assert_eq!(p.file_name, "tiny.png");
        assert_eq!(p.content_hash.as_deref(), Some(hash.as_str()), "storage key = catalog content hash");
        let out = session.render_now(id, 20, 20).unwrap();
        assert_eq!(out.image.width, 20);
    }

    #[test]
    fn download_names() {
        assert_eq!(download_name("dir/x-lightcraft.png"), "x-lightcraft.png");
        assert_eq!(download_name("y.png"), "y.png");
        assert_eq!(mime_for("a.JPG"), "image/jpeg");
        assert_eq!(mime_for("a"), "application/octet-stream");
        assert_eq!(download_name(""), "export.png");
    }

    #[test]
    fn original_sizes_are_not_limited_by_the_application_cache_budget() {
        let budget = lightcraft_engine::memory::budget() as f64;
        for size in [0.0, 257.0 * 1048576.0, budget / 2.0 + 1.0, budget + 1.0] {
            assert!(check_original_size(size).is_ok(), "{size}");
        }
        for size in [isize::MAX as f64 + 4096.0, f64::MAX, f64::INFINITY, f64::NAN, -1.0, 1.5, 9_007_199_254_740_992.0] {
            assert!(check_original_size(size).is_err(), "{size}");
        }
    }

    #[test]
    fn video_import_uses_staged_metadata_without_buffering_the_original() {
        let originals = Originals::default();
        let mut session = lightcraft_engine::Session::new();
        originals.install(&mut session);
        let mut probe = probe_bytes("tiny.png", &png_bytes(40, 30)).unwrap();
        probe.kind = lightcraft_engine::catalog::MediaKind::Video;
        probe.format = "MP4".into();
        probe.file_size = 5 << 30;
        probe.duration = Some(60.0);
        probe.content_hash = Some("large-video".into());
        let path = originals.added_video("large.mp4", "large-video", probe);
        assert_eq!(originals.take_pending(), vec![path.clone()]);
        let report = session.execute("library.import", &json!({"paths": [path.clone()]})).unwrap();
        let id = lightcraft_engine::catalog::PhotoId(report["imported"][0].as_u64().unwrap());
        let photo = session.catalog.photo(id).unwrap();
        assert!(photo.is_video());
        assert_eq!(photo.file_size, 5 << 30);
        assert_eq!(photo.duration, Some(60.0));
        assert_eq!(originals.usage(), (0, 0), "the clip does not enter the whole-buffer cache");
        originals.finish_import();
        assert!(originals.lock().probes.is_empty());
        assert!(originals.hooks().1(&path).is_err(), "staged metadata is not retained after import");
    }

    #[test]
    fn browser_import_holds_the_sessions_activity_row() {
        let session = lightcraft_engine::Session::new();
        let originals = Originals::new(session.activity.clone());
        let guard = originals.import_activity("clip.mp4");
        let rows = session.activity.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "import");
        assert!(!rows[0].cancellable);
        drop(guard);
        assert!(session.activity.list().is_empty());
    }
}
