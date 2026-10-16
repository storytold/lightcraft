use super::*;
use crate::{Session, media};
use serde_json::json;
use std::sync::{Mutex, atomic::Ordering, mpsc};
use std::time::{Duration, Instant};

type Hook = Arc<dyn Fn(usize) + Send + Sync>;
static HOOKS: Mutex<Vec<(PathBuf, Hook)>> = Mutex::new(Vec::new());

pub(super) fn checkpoint(dir: &Path, index: usize) {
    let hook = HOOKS.lock().unwrap().iter().find(|(path, _)| path == dir).map(|(_, hook)| hook.clone());
    if let Some(hook) = hook {
        hook(index);
    }
}

fn hook(dir: &Path, f: impl Fn(usize) + Send + Sync + 'static) {
    HOOKS.lock().unwrap().push((dir.to_path_buf(), Arc::new(f)));
}

fn fixture(tag: &str) -> (PathBuf, Session, PathBuf, PathBuf) {
    let root = super::tests::temp(tag);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let from = s.media.smart_dir.clone().unwrap();
    std::fs::create_dir_all(&from).unwrap();
    let bytes = encode_format(&Rgb32f::new(16, 12), None, Format::Lossless).unwrap();
    for i in 0..3 {
        std::fs::write(from.join(format!("{i}.lcsp")), &bytes).unwrap();
    }
    let to = root.join("destination");
    (root, s, from, to)
}

fn all_sources(from: &Path) -> bool {
    (0..3).all(|i| is_usable(&from.join(format!("{i}.lcsp"))))
}

#[test]
fn regression709_round3_reopen_conversion_invalidates_current_sources_and_thumbnail() {
    let root = super::tests::temp("round3-reopen");
    let path = root.join("original.tif");
    let mut image = Rgb32f::new(96, 64);
    for (i, p) in image.data.iter_mut().enumerate() {
        let x = (i % 96) as f32 / 96.0;
        let y = (i / 96) as f32 / 64.0;
        *p = [x * 2.2 - 0.02, y * 0.7 + 0.01, (x + y) * 0.6];
    }
    let encoded = lightcraft_codecs::EncodeImage::new(96, 64, 3, lightcraft_codecs::Samples::F32(image.data.as_flattened()));
    std::fs::write(&path, lightcraft_codecs::encode_tiff(&encoded, lightcraft_codecs::TiffCompression::Deflate, &Default::default()).unwrap())
        .unwrap();
    let library = root.join("library");
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.smartPreviews", &json!({"format": "compact"})).unwrap();
    let proxy = file_path(s.media.smart_dir.as_ref().unwrap(), s.catalog.photo(id).unwrap());
    let compact = load(&proxy).unwrap().image;
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let blocked = Mutex::new(blocked);
    let original = s.media.file_loader.clone().unwrap();
    s.media.file_loader = Some(Arc::new(move |p, n| {
        let decoded = original(p, n)?;
        entered.send(()).unwrap();
        blocked.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
        Ok(decoded)
    }));
    s.execute("library.smartPreviews", &json!({"format": "lossless", "background": true})).unwrap();
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    std::fs::rename(&path, path.with_extension("offline")).unwrap();
    s.open_library(&library, false).unwrap();
    s.media.file_loader = Session::new().with_fs().media.file_loader;
    let result = s.thumb_job(id, 128).unwrap().run();
    s.accept(&result);
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !s.preview_build.as_ref().unwrap().finished.load(Ordering::Acquire) {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    let persisted = load(&proxy).unwrap();
    assert_ne!(compact.data, persisted.image.data);
    let actual = s.thumb_job(id, 128).unwrap().run().rendered.unwrap().image;
    let source = s.media.source_ref(s.catalog.photo(id).unwrap(), media::SourceLevel::Thumb).load_source().unwrap();
    let mut fresh = s.thumb_job(id, 128).unwrap();
    fresh.source = media::SourceRef::Loaded(Box::new(persisted.clone()));
    // Keep both comparisons on the thumbnail CPU path. Removing cache would use the GPU.
    fresh.cache = Some((Arc::new(lightcraft_preview::PreviewCache::memory(1_000_000)), lightcraft_preview::Hash128(0)));
    let expected = fresh.run().rendered.unwrap().image;
    let different = actual.data.iter().zip(&expected.data).filter(|(a, b)| a != b).count();
    assert_eq!(different, 0, "reopened thumbnail kept Compact pixels");
    assert_eq!(source.image.data, persisted.image.data, "reopened decoded source kept Compact pixels");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn regression709_round3_migration_returns_while_io_is_blocked() {
    let (root, mut s, from, to) = fixture("round3-responsive");
    let activity = s.activity.clone();
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let blocked = Mutex::new(blocked);
    hook(&from, move |index| {
        if index == 0 {
            entered.send(()).unwrap();
            blocked.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
        }
    });
    let (returned, response) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let r = s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move", "background": true}));
        returned.send((s, r)).unwrap();
    });
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    let ready = response.recv_timeout(Duration::from_millis(500)).ok();
    let rows = activity.list();
    let returned_promptly = ready.is_some();
    if let Some(row) = rows.first() {
        activity.cancel(row.id).unwrap();
    }
    release.send(()).unwrap();
    worker.join().unwrap();
    let (mut s, result) = ready.unwrap_or_else(|| response.recv_timeout(Duration::from_secs(10)).unwrap());
    result.unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !s.activity.list().is_empty() {
        s.persist_if_dirty();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(returned_promptly, "location command blocked on proxy I/O");
    assert!(rows.iter().any(|row| row.kind == "smartMigration" && row.cancellable), "no cancellable activity row");
    assert_eq!(s.media.smart_dir.as_ref(), Some(&from));
    assert!(all_sources(&from));
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn regression709_round3_cancel_keeps_the_whole_old_folder() {
    let (root, mut s, from, to) = fixture("round3-cancel");
    let activity = s.activity.clone();
    hook(&from, move |index| {
        if index == 1 {
            for row in activity.list() {
                activity.cancel(row.id).unwrap();
            }
        }
    });
    s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move"})).unwrap();
    assert!(all_sources(&from), "cancellation removed a source before committing the new location");
    assert_eq!(s.media.smart_dir.as_ref(), Some(&from));
    s.open_library(root.join("library"), false).unwrap();
    assert_eq!(s.media.smart_dir.as_ref(), Some(&from));
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "child process for the crash regression"]
fn migration_crash_child() {
    let root = PathBuf::from(std::env::var_os("LC_SMART_CRASH_FIXTURE").unwrap());
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    let from = s.media.smart_dir.clone().unwrap();
    let crash_at: usize = std::env::var("LC_SMART_CRASH_AT").unwrap_or_else(|_| "1".into()).parse().unwrap();
    hook(&from, move |index| {
        if index == crash_at {
            std::process::exit(79);
        }
    });
    s.execute("library.smartPreviewsLocation", &json!({"path": root.join("destination"), "existing": "move"})).unwrap();
    panic!("crash checkpoint was not reached");
}

#[test]
fn regression709_round3_crash_during_copy_can_reopen_and_retry() {
    let (root, s, from, to) = fixture("round3-crash");
    drop(s);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "smart::tests_round3::migration_crash_child", "--ignored"])
        .env("LC_SMART_CRASH_FIXTURE", &root)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(79));
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    assert_eq!(s.media.smart_dir.as_ref(), Some(&from));
    assert!(all_sources(&from), "crash left the active folder split across destinations");
    let result = s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move"})).unwrap();
    assert_eq!(result["handled"], 3);
    assert!(all_sources(&to));
    assert_eq!(stats(&from).0, 0);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn migration_crash_during_cleanup_reopens_the_complete_new_folder() {
    let (root, s, from, to) = fixture("round3-cleanup-crash");
    drop(s);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "smart::tests_round3::migration_crash_child", "--ignored"])
        .env("LC_SMART_CRASH_FIXTURE", &root)
        .env("LC_SMART_CRASH_AT", "4")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(79));
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    assert_eq!(s.media.smart_dir.as_ref(), Some(&to));
    assert!(all_sources(&to));
    assert_eq!(stats(&from).0, 2, "only redundant sources were removed");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn migration_failed_preferences_keep_every_source_and_old_location() {
    let (root, mut s, from, to) = fixture("round3-prefs-failure");
    // A directory in place of prefs.json makes the atomic rename fail after all copies verify.
    let prefs = root.join("library/prefs.json");
    let _ = std::fs::remove_file(&prefs);
    std::fs::create_dir(&prefs).unwrap();
    assert!(s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move"})).is_err());
    assert_eq!(s.media.smart_dir.as_ref(), Some(&from));
    assert!(s.smart_previews_dir.is_none());
    assert!(all_sources(&from));
    assert!(all_sources(&to));
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn migration_reopen_cancels_copy_without_joining_the_worker() {
    let (root, mut s, from, to) = fixture("round3-reopen-move");
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let blocked = Mutex::new(blocked);
    hook(&from, move |index| {
        if index == 0 {
            entered.send(()).unwrap();
            blocked.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
        }
    });
    s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move", "background": true})).unwrap();
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    s.open_library(root.join("library"), false).unwrap();
    assert!(s.activity.list().iter().any(|t| t.kind == "smartMigration" && t.cancelling));
    assert_eq!(s.media.smart_dir.as_ref(), Some(&from));
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !s.activity.list().is_empty() {
        s.persist_if_dirty();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(all_sources(&from));
    assert_eq!(stats(&to).0, 0);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn proxy_invalidation_follows_library_across_session_replacement() {
    let root = super::tests::temp("round3-cache-identity");
    let (a, b) = (root.join("a"), root.join("b"));
    let mut first = media::MediaCache::default();
    first.attach_disk_cache(&a, 1_000_000);
    let changes = first.invalidation();
    let key = lightcraft_preview::Hash128(123);
    first.rendered.put(key, Arc::new(Rgba8::new(4, 3)));
    first.attach_disk_cache(&b, 1_000_000);
    let unrelated = first.rendered.clone();
    let generation = unrelated.generation();
    changes.clear();
    assert_eq!(unrelated.generation(), generation, "another library must not be invalidated");
    drop(first);
    let mut reopened = media::MediaCache::default();
    reopened.attach_disk_cache(&a, 1_000_000);
    assert!(reopened.rendered.get(key).is_none(), "inactive library retained disk thumbnails");
    reopened.rendered.put(key, Arc::new(Rgba8::new(4, 3)));
    let generation = reopened.rendered.generation();
    changes.clear();
    assert_eq!(reopened.rendered.generation(), generation + 1);
    assert!(reopened.rendered.get(key).is_none());
    let late_job_cache = reopened.rendered.clone();
    drop(reopened);
    let mut replacement = media::MediaCache::default();
    replacement.attach_disk_cache(&a, 1_000_000);
    assert_eq!(late_job_cache.generation(), u64::MAX, "old-session render jobs must retire");
    late_job_cache.put_at(1, key, Arc::new(Rgba8::new(4, 3)));
    assert!(replacement.rendered.get(key).is_none());
    changes.clear();
    assert_eq!(replacement.rendered.generation(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cache_replacement_rejects_a_result_from_the_previous_cache_object() {
    let root = super::tests::temp("round3-late-result");
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    s.media.clear_sources();
    let result = s.thumb_job(id, 128).unwrap().run();
    assert!(result.loaded.is_some());
    s.media.attach_disk_cache(&root, 1_000_000);
    // New caches can have the same numeric generation. Identity must also match.
    assert_eq!(result.cache_generation, s.media.rendered.generation());
    s.accept(&result);
    assert!(!s.media.has_source(id, media::SourceLevel::Thumb));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn migration_background_commits_after_all_copies_and_keeps_concurrent_preferences() {
    let (root, mut s, from, to) = fixture("round3-background-success");
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let blocked = Mutex::new(blocked);
    hook(&from, move |index| {
        if index == 1 {
            entered.send(()).unwrap();
            blocked.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
        }
    });
    let response = s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move", "background": true})).unwrap();
    assert_eq!(response["running"], true);
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(stats(&to).0, 1);
    assert!(all_sources(&from));
    assert_eq!(s.media.smart_dir.as_ref(), Some(&from));
    assert!(s.execute("library.smartPreviews", &json!({})).is_err());
    s.execute("library.smartPreviewsFormat", &json!({"format": "lossless"})).unwrap();
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while s.smart_migration.is_some() {
        s.persist_if_dirty();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    let result = s.smart_migration_result.take().unwrap().unwrap();
    assert_eq!(result["handled"], 3);
    assert_eq!(result["failed"], json!([]));
    assert_eq!(s.media.smart_dir.as_ref(), Some(&to));
    assert!(all_sources(&to));
    assert_eq!(stats(&from).0, 0);
    s.open_library(root.join("library"), false).unwrap();
    assert_eq!(s.media.smart_dir.as_ref(), Some(&to));
    assert_eq!(s.smart_previews_format, Format::Lossless);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn closing_the_last_cache_owner_retires_pending_disk_writers() {
    let media = media::MediaCache::default();
    let late_writer = media.rendered.clone();
    let generation = late_writer.generation();
    drop(media);
    assert_eq!(late_writer.generation(), u64::MAX);
    let key = lightcraft_preview::Hash128(123);
    late_writer.put_at(generation, key, Arc::new(Rgba8::new(4, 3)));
    assert!(late_writer.get(key).is_none());
}
