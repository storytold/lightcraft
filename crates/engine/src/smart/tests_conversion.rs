//! A format conversion that ends with the process, while the disk thumbnail cache holds
//! thumbnails rendered from the proxies it replaces.

use super::*;
use crate::{Session, media};
use lightcraft_catalog::{PhotoId, Source};
use lightcraft_preview::{Hash128, PreviewCache};
use serde_json::json;
use std::sync::atomic::Ordering;

const EXIT_CODE: i32 = 82;

fn originals(root: &Path) -> [PathBuf; 2] {
    [root.join("first.tif"), root.join("second.tif")]
}

fn set_online(root: &Path, online: bool) {
    for path in originals(root) {
        let offline = path.with_extension("offline");
        let (from, to) = if online { (offline, path) } else { (path, offline) };
        if from.exists() {
            std::fs::rename(from, to).unwrap();
        }
    }
}

fn photo_id(s: &Session, name: &str) -> PhotoId {
    s.catalog.photos().find(|p| matches!(&p.source, Source::File { path } if path.ends_with(name))).unwrap().id
}

fn ids(s: &Session) -> [u64; 2] {
    [photo_id(s, "first.tif").0, photo_id(s, "second.tif").0]
}

fn write_original(path: &Path, scale: f32) {
    let mut image = Rgb32f::new(96, 64);
    for (i, p) in image.data.iter_mut().enumerate() {
        let x = (i % 96) as f32 / 96.0;
        let y = (i / 96) as f32 / 64.0;
        *p = [x * 2.2 * scale - 0.02, y * 0.7 + 0.01, (x + y) * 0.6 * scale];
    }
    let encoded = lightcraft_codecs::EncodeImage::new(96, 64, 3, lightcraft_codecs::Samples::F32(image.data.as_flattened()));
    let bytes = lightcraft_codecs::encode_tiff(&encoded, lightcraft_codecs::TiffCompression::Deflate, &Default::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn write_originals(dir: &Path) {
    for (path, scale) in originals(dir).iter().zip([1.0f32, 0.55]) {
        write_original(path, scale);
    }
}

/// Two photos with Compact proxies. The first one's thumbnail, rendered from its proxy while
/// the original was offline, is in the disk cache. Both originals are online again.
fn fixture(tag: &str) -> PathBuf {
    let root = super::tests::temp(tag);
    write_originals(&root);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    s.execute("library.import", &json!({"paths": originals(&root)})).unwrap();
    assert_eq!(s.catalog.photos().count(), 2);
    s.execute("library.smartPreviews", &json!({"format": "compact", "ids": ids(&s)})).unwrap();
    set_online(&root, false);
    s.media.clear_sources();
    let job = s.thumb_job(photo_id(&s, "first.tif"), 128).unwrap();
    let key = job.cache.as_ref().unwrap().1;
    let thumbnail = job.run().rendered.unwrap().image;
    s.media.rendered.put(key, Arc::new(thumbnail));
    let saved = s.media.rendered.disk().unwrap().get(key).unwrap();
    std::fs::write(root.join("old-thumbnail.rgba"), saved.as_bytes()).unwrap();
    set_online(&root, true);
    s.close_library().unwrap();
    root
}

/// The first photo's proxy is Lossless, and its thumbnail matches a render of that proxy.
fn assert_fresh_thumbnail(s: &mut Session, root: &Path) {
    let id = photo_id(s, "first.tif");
    let photo = s.catalog.photo(id).unwrap().clone();
    let path = file_path(s.media.smart_dir.as_ref().unwrap(), &photo);
    assert_eq!(header(&path).unwrap().0, Format::Lossless, "the first proxy was converted before the exit");
    let mut fresh = s.thumb_job(id, 128).unwrap();
    fresh.source = media::SourceRef::Loaded(Box::new(load(&path).unwrap()));
    // A separate memory cache keeps the reference on the thumbnail CPU path.
    fresh.cache = Some((Arc::new(PreviewCache::memory(1_000_000)), Hash128(0)));
    let reference = fresh.run().rendered.unwrap().image;
    let encoded = lightcraft_preview::decode_jpeg(&lightcraft_preview::encode_jpeg(&reference).unwrap()).unwrap();
    let old = std::fs::read(root.join("old-thumbnail.rgba")).unwrap();
    assert_ne!(old, encoded.as_bytes(), "fixture must tell Compact and Lossless apart under the cache encoding");
    let job = s.thumb_job(id, 128).unwrap();
    let hit = job.cache.as_ref().is_some_and(|(cache, key)| cache.get(*key).is_some());
    let actual = job.run().rendered.unwrap().image;
    // A cache miss returns the uncompressed render, a disk hit returns JPEG-decoded pixels.
    let correct = actual.data == reference.data || actual.data == encoded.data;
    let differing = actual.data.iter().zip(&encoded.data).filter(|(a, b)| a != b).count();
    assert!(
        correct,
        "stale Compact thumbnail after the interrupted conversion: cache_hit={hit}, same_as_old={}, differing_pixels={differing}",
        actual.as_bytes() == old
    );
}

/// Photo `name`'s thumbnail matches a render of its current proxy (originals offline).
fn assert_thumbnail_matches_proxy(s: &mut Session, name: &str) {
    let id = photo_id(s, name);
    let photo = s.catalog.photo(id).unwrap().clone();
    let path = file_path(s.media.smart_dir.as_ref().unwrap(), &photo);
    let mut fresh = s.thumb_job(id, 128).unwrap();
    fresh.source = media::SourceRef::Loaded(Box::new(load(&path).unwrap()));
    fresh.cache = Some((Arc::new(PreviewCache::memory(1_000_000)), Hash128(0)));
    let reference = fresh.run().rendered.unwrap().image;
    let encoded = lightcraft_preview::decode_jpeg(&lightcraft_preview::encode_jpeg(&reference).unwrap()).unwrap();
    let job = s.thumb_job(id, 128).unwrap();
    let hit = job.cache.as_ref().is_some_and(|(cache, key)| cache.get(*key).is_some());
    let actual = job.run().rendered.unwrap().image;
    let differing = actual.data.iter().zip(&encoded.data).filter(|(a, b)| a != b).count();
    assert!(
        actual.data == reference.data || actual.data == encoded.data,
        "{name}: thumbnail of a replaced proxy: cache_hit={hit}, differing_pixels={differing}"
    );
}

/// Worker markers and unexpected files at the top of the thumbnail folder.
fn loose_files(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join("library/thumbs")) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.file_name().is_none_or(|name| name != ".lightcraft-thumbs" && name != lightcraft_preview::disk::TAG))
        .collect()
}

/// `name`'s proxy in `s`'s smart previews folder.
fn proxy_of(s: &Session, name: &str) -> PathBuf {
    file_path(s.media.smart_dir.as_ref().unwrap(), s.catalog.photo(photo_id(s, name)).unwrap())
}

/// Waits (up to 10 s) until `proxy` is written as Lossless. A build runs several photos at once
/// (issue #1492), so a later photo's original can be read before an earlier one's proxy is written;
/// a hook that stands for "after that proxy converted" waits for it here.
fn wait_converted(proxy: &Path) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !matches!(header(proxy), Some((Format::Lossless, _))) {
        assert!(std::time::Instant::now() < until, "{} was never converted", proxy.display());
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
#[ignore = "child process for the interrupted conversion regression"]
fn interrupted_conversion_child() {
    let root = PathBuf::from(std::env::var_os("LC_SMART_CONVERSION_ROOT").unwrap());
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let loader = s.media.file_loader.clone().unwrap();
    let first = proxy_of(&s, "first.tif");
    s.media.file_loader = Some(Arc::new(move |path, edge| {
        if path.ends_with("second.tif") {
            // The first photo's Lossless proxy is durable; the build's final clear has not run.
            wait_converted(&first);
            std::process::exit(EXIT_CODE);
        }
        loader(path, edge)
    }));
    let ids = ids(&s);
    s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).unwrap();
    panic!("the build finished without reading the second original");
}

#[test]
fn regression709_interrupted_conversion_reopens_without_stale_thumbnails() {
    let root = fixture("conversion-exit");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "smart::tests_conversion::interrupted_conversion_child", "--ignored"])
        .env("LC_SMART_CONVERSION_ROOT", &root)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(EXIT_CODE));
    set_online(&root, false);
    for _ in 0..2 {
        let mut s = Session::new().with_fs();
        s.open_library(root.join("library"), false).unwrap();
        assert_fresh_thumbnail(&mut s, &root);
        assert!(loose_files(&root).is_empty(), "{:?}", loose_files(&root));
    }
    // A retry with the originals still offline changes no proxy. It must not bring the old
    // thumbnail back, and a completed build leaves nothing behind in the thumbnail folder.
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let ids = ids(&s);
    let r = s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).unwrap();
    assert_eq!((r["built"].as_u64(), r["staleKept"].as_u64()), (Some(2), Some(1)), "{r}");
    assert_fresh_thumbnail(&mut s, &root);
    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    assert_fresh_thumbnail(&mut s, &root);
    assert!(loose_files(&root).is_empty(), "{:?}", loose_files(&root));
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn conversion_is_refused_when_the_thumbnail_folder_cannot_record_it() {
    let root = fixture("conversion-unrecorded");
    let thumbs = root.join("library/thumbs");
    std::fs::rename(&thumbs, root.join("thumbs-aside")).unwrap();
    std::fs::write(&thumbs, b"not a folder").unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let ids = ids(&s);
    let error = s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).unwrap_err().to_string();
    assert!(error.contains("thumbnail"), "{error}");
    for id in ids {
        let photo = s.catalog.photo(PhotoId(id)).unwrap();
        assert_eq!(header(&file_path(s.media.smart_dir.as_ref().unwrap(), photo)).unwrap().0, Format::Compact, "no proxy changed");
    }
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn reopening_during_a_running_build_keeps_its_marker() {
    use std::sync::{Mutex, mpsc};
    use std::time::{Duration, Instant};
    let root = fixture("conversion-running");
    let library = root.join("library");
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    let generation = s.smart_previews_cache_generation;
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel::<()>();
    let blocked = Mutex::new(blocked);
    let loader = s.media.file_loader.clone().unwrap();
    let first = proxy_of(&s, "first.tif");
    s.media.file_loader = Some(Arc::new(move |path, edge| {
        if path.ends_with("second.tif") {
            wait_converted(&first);
            entered.send(()).unwrap();
            blocked.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
        }
        loader(path, edge)
    }));
    let ids = ids(&s);
    s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids, "background": true})).unwrap();
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(loose_files(&root).len(), 1, "the build records its change before the first proxy");
    s.open_library(&library, false).unwrap();
    s.media.file_loader = Session::new().with_fs().media.file_loader;
    assert_eq!(s.smart_previews_cache_generation, generation, "a running build is not an interrupted one");
    assert_eq!(loose_files(&root).len(), 1, "opening keeps the running build's marker");
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !s.preview_build.as_ref().unwrap().finished.load(Ordering::Acquire) {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(loose_files(&root).is_empty(), "the finished build cleared the cache, then removed its marker");
    set_online(&root, false);
    s.open_library(&library, false).unwrap();
    assert_eq!(s.smart_previews_cache_generation, generation);
    assert_fresh_thumbnail(&mut s, &root);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

/// Blocks the build in the second original's load (after the first proxy converted) until released.
#[allow(clippy::type_complexity)]
fn block_second(s: &mut Session) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    use std::sync::{Mutex, mpsc};
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel::<()>();
    let blocked = Mutex::new(blocked);
    let loader = s.media.file_loader.clone().unwrap();
    let first = proxy_of(s, "first.tif");
    s.media.file_loader = Some(Arc::new(move |path, edge| {
        if path.ends_with("second.tif") {
            wait_converted(&first);
            entered.send(()).unwrap();
            blocked.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        }
        loader(path, edge)
    }));
    (waiting, release)
}

fn wait_for_build(s: &Session) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !s.preview_build.as_ref().unwrap().finished.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn reopening_through_another_path_spelling_keeps_the_running_build() {
    let root = fixture("conversion-spelling");
    std::fs::create_dir(root.join("aside")).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let generation = s.smart_previews_cache_generation;
    let (waiting, release) = block_second(&mut s);
    let ids = ids(&s);
    s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids, "background": true})).unwrap();
    waiting.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(loose_files(&root).len(), 1, "the build records its change before the first proxy");
    // The same library, reached through another spelling of its path.
    s.open_library(root.join("aside/../library"), false).unwrap();
    s.media.file_loader = Session::new().with_fs().media.file_loader;
    assert_eq!(loose_files(&root).len(), 1, "opening keeps the running build's marker");
    assert_eq!(s.smart_previews_cache_generation, generation, "a running build is not an interrupted one");
    // The grid shows the second photo from its Compact proxy while its original is offline.
    let second = root.join("second.tif");
    std::fs::rename(&second, second.with_extension("offline")).unwrap();
    s.media.clear_sources();
    let job = s.thumb_job(photo_id(&s, "second.tif"), 128).unwrap();
    let key = job.cache.as_ref().unwrap().1;
    let shown = job.run().rendered.unwrap().image;
    s.media.rendered.put(key, Arc::new(shown));
    std::fs::rename(second.with_extension("offline"), &second).unwrap();
    release.send(()).unwrap();
    wait_for_build(&s);
    assert!(loose_files(&root).is_empty());
    set_online(&root, false);
    s.media.clear_sources();
    // The finished build cleared the cache this session uses.
    assert_thumbnail_matches_proxy(&mut s, "second.tif");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

/// Fails every `prefs.json` save (an I/O error, or a file that could not be read at opening).
struct FailPrefs(lightcraft_catalog::store::FsStore);

impl lightcraft_catalog::store::Store for FailPrefs {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.0.read(name)
    }
    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        if name == "prefs.json" {
            return Err(std::io::Error::other("injected prefs.json failure"));
        }
        self.0.write_atomic(name, bytes)
    }
    fn append(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.0.append(name, bytes)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.0.truncate(name, len)
    }
    fn describe(&self) -> String {
        self.0.describe()
    }
}

#[test]
#[ignore = "child process for the failed preferences save regression"]
fn failed_preferences_child() {
    use lightcraft_catalog::store::FsStore;
    let root = PathBuf::from(std::env::var_os("LC_SMART_PREFS_ROOT").unwrap());
    let dir = root.join("library");
    let mut s = Session::new().with_fs();
    let stores = crate::library::LibraryStores {
        dir: dir.clone(),
        catalog: Box::new(FsStore::open(&dir).unwrap()),
        files: Box::new(FailPrefs(FsStore::open(&dir).unwrap())),
        on_disk: true,
    };
    s.open_library_in(stores, false).unwrap();
    // This session could not save the namespace it took. The grid shows the first photo from
    // its Compact proxy while the original is offline.
    set_online(&root, false);
    s.media.clear_sources();
    let job = s.thumb_job(photo_id(&s, "first.tif"), 128).unwrap();
    let key = job.cache.as_ref().unwrap().1;
    let shown = job.run().rendered.unwrap().image;
    s.media.rendered.put(key, Arc::new(shown));
    set_online(&root, true);
    // A conversion replaces the first proxy, then the process ends while reading the second original.
    let loader = s.media.file_loader.clone().unwrap();
    let first = proxy_of(&s, "first.tif");
    s.media.file_loader = Some(Arc::new(move |path, edge| {
        if path.ends_with("second.tif") {
            wait_converted(&first);
            std::process::exit(EXIT_CODE);
        }
        loader(path, edge)
    }));
    let ids = ids(&s);
    s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).unwrap();
    panic!("the build finished without reading the second original");
}

#[test]
fn regression709_failed_preferences_save_at_opening_still_isolates_the_next_interrupted_build() {
    let root = fixture("conversion-prefs");
    // A run that ended before its cache clear left its marker.
    std::fs::write(root.join("library/thumbs/smart-previews-changing-1-2-3"), b"").unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "smart::tests_conversion::failed_preferences_child", "--ignored"])
        .env("LC_SMART_PREFS_ROOT", &root)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(EXIT_CODE));
    set_online(&root, false);
    for _ in 0..2 {
        let mut s = Session::new().with_fs();
        s.open_library(root.join("library"), false).unwrap();
        assert_fresh_thumbnail(&mut s, &root);
        assert!(loose_files(&root).is_empty(), "{:?}", loose_files(&root));
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// A small disk image (macOS), detached and deleted when dropped.
#[cfg(target_os = "macos")]
struct Volume {
    base: PathBuf,
    mount: PathBuf,
}

#[cfg(target_os = "macos")]
impl Volume {
    /// `None` when this machine cannot create or attach disk images (the test is then skipped).
    fn new(tag: &str, filesystem: &str) -> Option<Volume> {
        use std::process::Command;
        let base = super::tests::temp(tag);
        let (image, mount) = (base.join("volume.dmg"), base.join("mnt"));
        std::fs::create_dir(&mount).unwrap();
        let created = Command::new("hdiutil")
            .args(["create", "-quiet", "-size", "40m", "-fs", filesystem, "-volname", "lcfull", "-type", "UDIF"])
            .arg(&image)
            .status()
            .is_ok_and(|s| s.success());
        let attached = created
            && Command::new("hdiutil")
                .args(["attach", "-quiet", "-nobrowse", "-noverify", "-noautoopen", "-mountpoint"])
                .arg(&mount)
                .arg(&image)
                .status()
                .is_ok_and(|s| s.success());
        if !attached {
            eprintln!("skipped: hdiutil could not create and attach a {filesystem} disk image");
            let _ = std::fs::remove_dir_all(&base);
            return None;
        }
        Some(Volume { base, mount })
    }

    /// Write until the volume refuses even one byte, as when something else fills the drive.
    /// APFS can still have room in a folder's own records, so `folder` (where the library is
    /// about to write) also gets empty files until it refuses one.
    fn fill(&self, folder: &Path) {
        use std::io::Write;
        let mut filler = std::fs::OpenOptions::new().create(true).append(true).open(self.mount.join("filler")).unwrap();
        for size in [1 << 20, 4096, 512, 1] {
            let chunk = vec![0u8; size];
            while filler.write_all(&chunk).is_ok() {}
        }
        let _ = filler.sync_all();
        let full = (0..200_000).any(|i| std::fs::File::create(folder.join(format!("filler-{i}"))).and_then(|f| f.sync_all()).is_err());
        assert!(full, "the volume never filled up");
    }
}

#[cfg(target_os = "macos")]
impl Drop for Volume {
    fn drop(&mut self) {
        let _ = std::process::Command::new("hdiutil").args(["detach", "-quiet", "-force"]).arg(&self.mount).status();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The library drive is full. Discard Smart Previews, the in-app way to get space back, still
/// removes the proxies, and a Lossless conversion into a proxy folder on another drive with room
/// (what the storage dialog suggests) still converts, without serving thumbnails of the old proxies.
#[test]
#[cfg(target_os = "macos")]
fn regression709_full_library_drive_still_discards_and_converts_elsewhere() {
    for filesystem in ["APFS", "HFS+"] {
        let tag = filesystem.trim_end_matches('+').to_lowercase();
        let Some(volume) = Volume::new(&format!("full-volume-{tag}"), filesystem) else { continue };
        let elsewhere = super::tests::temp(&format!("full-volume-elsewhere-{tag}"));
        write_originals(&elsewhere);
        // Library A keeps its Lossless proxies in the library, on the drive that fills up.
        let mut a = Session::new().with_fs();
        a.open_library(volume.mount.join("a"), false).unwrap();
        a.execute("library.import", &json!({"paths": originals(&elsewhere)})).unwrap();
        let a_ids = ids(&a);
        a.execute("library.smartPreviews", &json!({"format": "lossless", "ids": a_ids})).unwrap();
        // Library B keeps Compact proxies on another drive; its thumbnails, rendered from them
        // while the originals were offline, are in its thumbnail cache on the full drive.
        let mut b = Session::new().with_fs();
        b.open_library(volume.mount.join("b"), false).unwrap();
        b.execute("library.import", &json!({"paths": originals(&elsewhere)})).unwrap();
        std::fs::create_dir(elsewhere.join("proxies")).unwrap();
        b.execute("library.smartPreviewsLocation", &json!({"path": elsewhere.join("proxies"), "existing": "move"})).unwrap();
        let b_ids = ids(&b);
        b.execute("library.smartPreviews", &json!({"format": "compact", "ids": b_ids})).unwrap();
        set_online(&elsewhere, false);
        for s in [&mut a, &mut b] {
            s.media.clear_sources();
            for name in ["first.tif", "second.tif"] {
                let job = s.thumb_job(photo_id(s, name), 128).unwrap();
                let key = job.cache.as_ref().unwrap().1;
                let shown = job.run().rendered.unwrap().image;
                s.media.rendered.put(key, Arc::new(shown));
            }
        }
        set_online(&elsewhere, true);
        volume.fill(&volume.mount.join("b/thumbs"));
        let converted = b.execute("library.smartPreviews", &json!({"format": "lossless", "ids": b_ids}));
        volume.fill(&volume.mount.join("a/thumbs"));
        let discarded = a.execute("library.smartPreviews", &json!({"discard": true, "ids": a_ids}));
        assert!(converted.is_ok() && discarded.is_ok(), "{filesystem}: conversion {converted:?}, discard {discarded:?}");
        assert_eq!(discarded.unwrap()["removed"], json!(2), "{filesystem}");
        assert_eq!(stats(a.media.smart_dir.as_ref().unwrap()).0, 0, "{filesystem}: the discard freed the proxies");
        for id in b_ids {
            let photo = b.catalog.photo(PhotoId(id)).unwrap();
            assert_eq!(header(&file_path(b.media.smart_dir.as_ref().unwrap(), photo)).unwrap().0, Format::Lossless, "{filesystem}");
        }
        set_online(&elsewhere, false);
        b.media.clear_sources();
        for name in ["first.tif", "second.tif"] {
            assert_thumbnail_matches_proxy(&mut b, name);
        }
        drop((a, b));
        drop(volume);
        std::fs::remove_dir_all(elsewhere).unwrap();
    }
}

static FAIL_MARKERS: std::sync::Mutex<Vec<(PathBuf, std::io::ErrorKind, usize)>> = std::sync::Mutex::new(Vec::new());

/// The next `count` markers in the library's thumbnail folder fail with `kind`.
fn fail_markers(root: &Path, kind: std::io::ErrorKind, count: usize) {
    let thumbs = crate::media::thumbnail_folder(&root.join("library/thumbs"));
    FAIL_MARKERS.lock().unwrap().push((thumbs, kind, count));
}

/// Test seam in marker creation (an empty file still needs space on APFS and HFS+).
pub(crate) fn injected_marker_failure(dir: &Path) -> Option<std::io::Error> {
    let mut failures = FAIL_MARKERS.lock().unwrap();
    let (_, kind, left) = failures.iter_mut().find(|(thumbs, _, left)| thumbs == dir && *left > 0)?;
    *left -= 1;
    Some(std::io::Error::from(*kind))
}

/// Thumbnail files in the cache's shard folders.
fn cached_thumbnails(root: &Path) -> usize {
    let Ok(shards) = std::fs::read_dir(root.join("library/thumbs")) else { return 0 };
    shards.flatten().filter(|e| e.path().is_dir()).map(|e| std::fs::read_dir(e.path()).map_or(0, |files| files.count())).sum()
}

#[test]
fn a_full_thumbnail_drive_is_cleared_before_the_first_replacement_then_recorded() {
    let root = fixture("conversion-full-once");
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    assert!(cached_thumbnails(&root) > 0);
    fail_markers(&root, std::io::ErrorKind::StorageFull, 1);
    let (waiting, release) = block_second(&mut s);
    let ids = ids(&s);
    s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids, "background": true})).unwrap();
    waiting.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(cached_thumbnails(&root), 0, "the cache was cleared before the first proxy was replaced");
    assert_eq!(loose_files(&root).len(), 1, "the clear made room for the marker");
    release.send(()).unwrap();
    wait_for_build(&s);
    assert!(loose_files(&root).is_empty());
    drop(s);
    set_online(&root, false);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    assert_fresh_thumbnail(&mut s, &root);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_thumbnail_drive_that_stays_full_still_converts() {
    let root = fixture("conversion-stays-full");
    fail_markers(&root, std::io::ErrorKind::StorageFull, 2);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let ids = ids(&s);
    let r = s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).unwrap();
    assert_eq!((r["built"].as_u64(), r["refreshed"].as_u64()), (Some(2), Some(2)), "{r}");
    assert!(loose_files(&root).is_empty());
    set_online(&root, false);
    s.media.clear_sources();
    assert_fresh_thumbnail(&mut s, &root);
    drop(s);
    for _ in 0..2 {
        let mut s = Session::new().with_fs();
        s.open_library(root.join("library"), false).unwrap();
        assert_fresh_thumbnail(&mut s, &root);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_discard_goes_ahead_without_a_marker_but_a_first_build_does_not() {
    let root = fixture("discard-without-marker");
    // A folder that cannot hold a marker, where nothing can be cleared either.
    let thumbs = root.join("library/thumbs");
    std::fs::remove_dir_all(&thumbs).unwrap();
    std::fs::write(&thumbs, b"not a folder").unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let ids = ids(&s);
    let r = s.execute("library.smartPreviews", &json!({"discard": true, "ids": ids})).unwrap();
    assert_eq!(r["removed"], json!(2), "{r}");
    assert_eq!(stats(s.media.smart_dir.as_ref().unwrap()).0, 0);
    // A first build replaces the picture the old thumbnails show: like a conversion, it stops.
    let build = s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids}));
    assert!(build.as_ref().is_err_and(|e| e.to_string().contains("thumbnail")), "{build:?}");
    assert_eq!(stats(s.media.smart_dir.as_ref().unwrap()).0, 0, "no proxy was written");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_conversion_refused_for_a_permission_error_keeps_the_thumbnail_cache() {
    let root = fixture("conversion-permission");
    fail_markers(&root, std::io::ErrorKind::PermissionDenied, 2);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let cached = cached_thumbnails(&root);
    assert!(cached > 0);
    let ids = ids(&s);
    assert!(s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).is_err());
    assert_eq!(cached_thumbnails(&root), cached, "clearing frees no permission");
    for id in ids {
        let photo = s.catalog.photo(PhotoId(id)).unwrap();
        assert_eq!(header(&file_path(s.media.smart_dir.as_ref().unwrap(), photo)).unwrap().0, Format::Compact);
    }
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

/// Issue #1492: with one lane per photo, a marker refused once refuses the whole run. Another lane's
/// retry would succeed, and must not convert its proxy while the run reports the failure.
#[test]
fn a_marker_refused_once_converts_no_proxy_on_any_lane() {
    let root = fixture("conversion-refused-once");
    fail_markers(&root, std::io::ErrorKind::PermissionDenied, 1);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let ids = ids(&s);
    assert!(s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).is_err());
    for id in ids {
        let photo = s.catalog.photo(PhotoId(id)).unwrap();
        assert_eq!(header(&file_path(s.media.smart_dir.as_ref().unwrap(), photo)).unwrap().0, Format::Compact);
    }
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

/// Child modes for the interrupted discard, then interrupted first build, regression.
#[test]
#[ignore = "child process for the interrupted discard and build regression"]
fn interrupted_discard_then_build_child() {
    let root = PathBuf::from(std::env::var_os("LC_SMART_CHAIN_ROOT").unwrap());
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let ids = ids(&s);
    if std::env::var("LC_SMART_CHAIN_MODE").unwrap() == "build" {
        // A first build writes the first proxy, then the process ends while reading the second original.
        let loader = s.media.file_loader.clone().unwrap();
        let first = proxy_of(&s, "first.tif");
        s.media.file_loader = Some(Arc::new(move |path, edge| {
            if path.ends_with("second.tif") {
                wait_converted(&first);
                std::process::exit(EXIT_CODE);
            }
            loader(path, edge)
        }));
        s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).unwrap();
        panic!("the build finished without reading the second original");
    }
    // A thumbnail read held open on a FIFO keeps the discard's final clear waiting, so the
    // process ends after the proxies are removed and before that clear, as with Quit Anyway.
    set_online(&root, false);
    s.media.clear_sources();
    let mut job = s.thumb_job(photo_id(&s, "second.tif"), 128).unwrap();
    let key = job.cache.as_ref().unwrap().1;
    // (rendered aside: no deferred disk write of this key may land after the FIFO is made)
    job.cache = Some((Arc::new(PreviewCache::memory(1_000_000)), key));
    let shown = job.run().rendered.unwrap().image;
    s.media.rendered.put(key, Arc::new(shown));
    set_online(&root, true);
    let hex = key.to_string();
    let fifo = root.join("library/thumbs").join(&hex[..2]).join(format!("{hex}.{}", lightcraft_preview::disk::EXT));
    std::fs::remove_file(&fifo).unwrap();
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    // a new cache object has an empty memory cache, so the read goes to the FIFO
    s.open_library(root.join("library"), false).unwrap();
    let reader = s.thumb_job(photo_id(&s, "second.tif"), 128).unwrap();
    std::thread::spawn(move || reader.run());
    // opening the writer waits for the reader, which then holds the cache's generation lock
    let _writer = std::fs::OpenOptions::new().write(true).open(&fifo).unwrap();
    let proxies: Vec<PathBuf> = ids.iter().map(|id| file_path(s.media.smart_dir.as_ref().unwrap(), s.catalog.photo(PhotoId(*id)).unwrap())).collect();
    std::thread::spawn(move || {
        loop {
            if proxies.iter().all(|p| !p.exists()) {
                std::process::exit(EXIT_CODE);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    });
    let _ = s.execute("library.smartPreviews", &json!({"discard": true, "ids": ids}));
    panic!("the discard's final clear ran");
}

/// Each run is safe alone: an interrupted discard leaves thumbnails that still show the photo.
/// But a first build after it replaces that picture, and if it is interrupted too, both runs
/// must have left a marker.
#[test]
#[cfg(unix)]
fn regression709_an_interrupted_discard_then_an_interrupted_first_build() {
    let root = fixture("discard-then-build");
    for mode in ["discard", "build"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "smart::tests_conversion::interrupted_discard_then_build_child", "--ignored"])
            .env("LC_SMART_CHAIN_ROOT", &root)
            .env("LC_SMART_CHAIN_MODE", mode)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(EXIT_CODE), "{mode}");
    }
    set_online(&root, false);
    for _ in 0..2 {
        let mut s = Session::new().with_fs();
        s.open_library(root.join("library"), false).unwrap();
        assert_fresh_thumbnail(&mut s, &root);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "child process for the full-drive fallback regression"]
fn full_drive_fallback_child() {
    use std::sync::{Mutex, mpsc};
    let root = PathBuf::from(std::env::var_os("LC_SMART_FALLBACK_ROOT").unwrap());
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    // The thumbnail folder's drive is full: no marker can be created, then space comes back.
    fail_markers(&root, std::io::ErrorKind::StorageFull, 2);
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel::<()>();
    let blocked = Mutex::new(blocked);
    let loader = s.media.file_loader.clone().unwrap();
    // The build runs several photos at once (issue #1492): each hook waits until the photos ahead
    // of it are done, the clear after each replacement included.
    let build: Arc<Mutex<Option<Arc<crate::cmd::previews::PreviewBuild>>>> = Arc::default();
    let done = {
        let build = build.clone();
        move |n: usize| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while build.lock().unwrap().as_ref().is_none_or(|b| b.done.load(Ordering::SeqCst) < n) {
                assert!(std::time::Instant::now() < until, "the photos ahead never finished");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    };
    s.media.file_loader = Some(Arc::new(move |path, edge| {
        if path.ends_with("third.tif") {
            done(2);
            std::process::exit(EXIT_CODE);
        }
        if path.ends_with("second.tif") {
            done(1);
            entered.send(()).unwrap();
            blocked.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        }
        loader(path, edge)
    }));
    let ids = [photo_id(&s, "first.tif").0, photo_id(&s, "second.tif").0, photo_id(&s, "third.tif").0];
    s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids, "background": true})).unwrap();
    *build.lock().unwrap() = s.preview_build.clone();
    waiting.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    // The first proxy is replaced. The grid shows the second photo from its old proxy, as when
    // its drive still reads as offline, and that thumbnail is written to disk.
    let second = root.join("second.tif");
    std::fs::rename(&second, second.with_extension("offline")).unwrap();
    s.media.clear_sources();
    let job = s.thumb_job(photo_id(&s, "second.tif"), 128).unwrap();
    let key = job.cache.as_ref().unwrap().1;
    let shown = job.run().rendered.unwrap().image;
    s.media.rendered.put(key, Arc::new(shown));
    std::fs::rename(second.with_extension("offline"), &second).unwrap();
    // The second proxy is replaced, then the process ends while reading the third original.
    release.send(()).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(30));
    panic!("the build did not reach the third original");
}

#[test]
fn regression709_a_run_without_a_marker_keeps_no_thumbnail_of_a_proxy_it_replaced() {
    let root = fixture("fallback-window");
    let third = root.join("third.tif");
    write_original(&third, 0.8);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    s.execute("library.import", &json!({"paths": [third]})).unwrap();
    let id = photo_id(&s, "third.tif").0;
    s.execute("library.smartPreviews", &json!({"format": "compact", "ids": [id]})).unwrap();
    s.close_library().unwrap();
    drop(s);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "smart::tests_conversion::full_drive_fallback_child", "--ignored"])
        .env("LC_SMART_FALLBACK_ROOT", &root)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(EXIT_CODE));
    set_online(&root, false);
    std::fs::rename(&third, third.with_extension("offline")).unwrap();
    for _ in 0..2 {
        let mut s = Session::new().with_fs();
        s.open_library(root.join("library"), false).unwrap();
        let photo = s.catalog.photo(photo_id(&s, "second.tif")).unwrap().clone();
        assert_eq!(header(&file_path(s.media.smart_dir.as_ref().unwrap(), &photo)).unwrap().0, Format::Lossless);
        assert_thumbnail_matches_proxy(&mut s, "second.tif");
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Without a marker, only a write that replaced a usable proxy needs the clear after it: before a
/// first build or a repair, no thumbnail can show the old picture. Each clear resets every
/// texture and decoded source in the app.
#[test]
fn a_first_build_without_a_marker_does_not_reset_the_cache_per_proxy() {
    let root = fixture("first-build-full");
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let dir = s.media.smart_dir.clone().unwrap();
    drop(s);
    std::fs::remove_dir_all(&dir).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let ids = ids(&s);
    fail_markers(&root, std::io::ErrorKind::StorageFull, 2);
    let before = s.media.rendered.generation();
    let r = s.execute("library.smartPreviews", &json!({"format": "lossless", "ids": ids})).unwrap();
    assert_eq!(r["built"], json!(2), "{r}");
    assert_eq!(s.media.rendered.generation() - before, 2, "one clear to free space and one at the end, not one per proxy");
    // A conversion without a marker still clears after each proxy it replaces.
    fail_markers(&root, std::io::ErrorKind::StorageFull, 2);
    let before = s.media.rendered.generation();
    let r = s.execute("library.smartPreviews", &json!({"format": "compact", "ids": ids})).unwrap();
    assert_eq!(r["refreshed"], json!(2), "{r}");
    assert_eq!(s.media.rendered.generation() - before, 4);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
