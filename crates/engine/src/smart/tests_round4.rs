//! Faults at the durable location commit, with different valid proxies in both folders.

use super::*;
use crate::{Session, library::LibraryStores};
use lightcraft_catalog::store::{FsStore, Store};
use lightcraft_preview::{Hash128, PreviewCache};
use serde_json::json;
use std::{
    io,
    sync::Mutex,
    time::{Duration, Instant},
};

static FAIL_SPAWN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

pub(crate) fn fail_cleanup_spawn(to: &Path) -> bool {
    let mut paths = FAIL_SPAWN.lock().unwrap();
    let Some(index) = paths.iter().position(|p| p == to) else { return false };
    paths.swap_remove(index);
    true
}

struct ExitAfterPreferences(FsStore);

impl Store for ExitAfterPreferences {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.0.read(name)
    }
    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.0.write_atomic(name, bytes)?;
        if name == "prefs.json" {
            // FsStore has completed the real durable write. No Rust destructors run.
            std::process::exit(81);
        }
        Ok(())
    }
    fn append(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.0.append(name, bytes)
    }
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        self.0.truncate(name, len)
    }
    fn describe(&self) -> String {
        self.0.describe()
    }
}

fn fixture(tag: &str) -> (PathBuf, Session) {
    let root = super::tests::temp(tag);
    let mut image = Rgb32f::new(96, 64);
    for (i, p) in image.data.iter_mut().enumerate() {
        let x = (i % 96) as f32 / 96.0;
        let y = (i / 96) as f32 / 64.0;
        *p = [x * 2.2 - 0.02, y * 0.7 + 0.01, (x + y) * 0.6];
    }
    let path = root.join("original.tif");
    let encoded = lightcraft_codecs::EncodeImage::new(96, 64, 3, lightcraft_codecs::Samples::F32(image.data.as_flattened()));
    std::fs::write(&path, lightcraft_codecs::encode_tiff(&encoded, lightcraft_codecs::TiffCompression::Deflate, &Default::default()).unwrap())
        .unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    s.execute("library.smartPreviews", &json!({"format": "compact"})).unwrap();
    let photo = s.catalog.photos().next().unwrap();
    let id = photo.id;
    let dest = sharded_file_path(&root.join("destination"), photo);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let (decoded, info) = s.media.file_loader.as_ref().unwrap()(path.to_str().unwrap(), 2560).unwrap();
    std::fs::write(dest, encode_format(&decoded, Some(&info), Format::Lossless).unwrap()).unwrap();
    std::fs::rename(&path, path.with_extension("offline")).unwrap();
    let job = s.thumb_job(id, 128).unwrap();
    let key = job.cache.as_ref().unwrap().1;
    let result = job.run();
    s.accept(&result);
    s.media.rendered.put(key, Arc::new(result.rendered.unwrap().image));
    let old = s.media.rendered.disk().unwrap().get(key).unwrap();
    std::fs::write(root.join("old-thumbnail.rgba"), old.as_bytes()).unwrap();
    (root, s)
}

use crate::cmd::previews::wait_for_opening_refresh;

fn assert_current_thumbnail(s: &mut Session, root: &Path) {
    let to = root.join("destination");
    assert_eq!(s.media.smart_dir.as_ref(), Some(&to));
    assert!(!root.join("original.tif").exists());
    let photo = s.catalog.photos().next().unwrap().clone();
    let path = file_path(&to, &photo);
    assert_eq!(header(&path).unwrap().0, Format::Lossless);
    let mut fresh = s.thumb_job(photo.id, 128).unwrap();
    fresh.source = crate::media::SourceRef::Loaded(Box::new(load(&path).unwrap()));
    // A separate memory cache keeps the reference on the thumbnail CPU path.
    fresh.cache = Some((Arc::new(PreviewCache::memory(1_000_000)), Hash128(0)));
    let reference = fresh.run().rendered.unwrap().image;
    let encoded_reference = lightcraft_preview::decode_jpeg(&lightcraft_preview::encode_jpeg(&reference).unwrap()).unwrap();
    let job = s.thumb_job(photo.id, 128).unwrap();
    let (cache, key) = job.cache.as_ref().unwrap();
    let hit = cache.get(*key).is_some();
    let actual = job.run().rendered.unwrap().image;
    let old = std::fs::read(root.join("old-thumbnail.rgba")).unwrap();
    assert_ne!(old, encoded_reference.as_bytes(), "fixture must distinguish Compact and Lossless with identical cache encoding");
    // A cache miss returns the uncompressed render, a disk hit returns JPEG-decoded pixels.
    let correct = actual.data == reference.data || actual.data == encoded_reference.data;
    let different = actual.data.iter().zip(&encoded_reference.data).filter(|(a, b)| a != b).count();
    assert!(
        correct,
        "new folder served stale Compact thumbnail: cache_hit={hit}, same_as_old={}, differing_pixels={different}",
        actual.as_bytes() == old
    );
}

#[test]
#[ignore = "child process for the durable-preference crash regression"]
fn durable_preferences_crash_child() {
    let root = PathBuf::from(std::env::var_os("LC_SMART_COMMIT_CRASH_ROOT").unwrap());
    let dir = root.join("library");
    let mut s = Session::new().with_fs();
    s.open_library_in(
        LibraryStores {
            dir: dir.clone(),
            catalog: Box::new(FsStore::open(&dir).unwrap()),
            files: Box::new(ExitAfterPreferences(FsStore::open(&dir).unwrap())),
            on_disk: true,
        },
        false,
    )
    .unwrap();
    wait_for_opening_refresh(&s);
    s.execute("library.smartPreviewsLocation", &json!({"path": root.join("destination"), "existing": "move", "background": true})).unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        s.persist_if_dirty();
        assert!(Instant::now() < until, "durable preference checkpoint not reached");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn regression709_round4_exit_after_durable_preferences_reopens_lossless_thumbnail() {
    let (root, mut s) = fixture("round4-commit-crash");
    s.close_library().unwrap();
    drop(s);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "smart::tests_round4::durable_preferences_crash_child", "--ignored"])
        .env("LC_SMART_COMMIT_CRASH_ROOT", &root)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(81));
    for _ in 0..2 {
        let mut s = Session::new().with_fs();
        s.open_library(root.join("library"), false).unwrap();
        assert_current_thumbnail(&mut s, &root);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn regression709_round4_cleanup_spawn_failure_still_invalidates() {
    let (root, mut s) = fixture("round4-spawn-failure");
    let to = root.join("destination");
    let from = s.media.smart_dir.clone().unwrap();
    FAIL_SPAWN.lock().unwrap().push(to.clone());
    s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move", "background": true})).unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    while s.smart_migration.is_some() {
        s.persist_if_dirty();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(s.smart_migration_result.take().unwrap().unwrap_err().contains("injected cleanup spawn failure"));
    assert_eq!(stats(&from).0, 1, "failed cleanup retains the redundant source");
    assert!(s.activity.list().is_empty());
    assert_current_thumbnail(&mut s, &root);
    s.open_library(root.join("library"), false).unwrap();
    assert_current_thumbnail(&mut s, &root);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_preferences_roll_back_the_cache_namespace() {
    let (root, mut s) = fixture("round4-prefs-rollback");
    let old_cache = s.media.rendered.clone();
    let old_folder = s.media.smart_dir.clone();
    let generation = s.smart_previews_cache_generation;
    let prefs = root.join("library/prefs.json");
    if prefs.is_file() {
        std::fs::remove_file(&prefs).unwrap();
    }
    std::fs::create_dir(&prefs).unwrap();
    assert!(s.execute("library.smartPreviewsLocation", &json!({"path": root.join("destination"), "existing": "move"})).is_err());
    assert_eq!(s.smart_previews_cache_generation, generation);
    assert_eq!(s.media.smart_dir, old_folder);
    assert!(Arc::ptr_eq(&s.media.rendered, &old_cache));
    assert_eq!(stats(old_folder.as_ref().unwrap()).0, 1);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(unix)]
fn namespace_switch_does_not_wait_for_an_old_thumbnail_read() {
    use std::{io::Write, sync::mpsc};
    let (root, mut s) = fixture("round4-blocked-thumbnail");
    s.open_library(root.join("library"), false).unwrap();
    wait_for_opening_refresh(&s);
    let id = s.catalog.photos().next().unwrap().id;
    let job = s.thumb_job(id, 128).unwrap();
    let (old_cache, key) = job.cache.clone().unwrap();
    let hex = key.to_string();
    let fifo = root.join("library/thumbs").join(&hex[..2]).join(format!("{hex}.{}", lightcraft_preview::disk::EXT));
    let jpeg = std::fs::read(&fifo).unwrap();
    std::fs::remove_file(&fifo).unwrap();
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let reader = std::thread::spawn(move || job.run());
    // Opening the writer pairs with the old cache's blocking read, while its generation
    // lock is held. Keep the pipe open until after the folder command has returned.
    let mut writer = std::fs::OpenOptions::new().write(true).open(&fifo).unwrap();
    let (done, response) = mpsc::channel();
    let to = root.join("destination");
    let command = std::thread::spawn(move || {
        let result = s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move"}));
        done.send((s, result)).unwrap();
    });
    let ready = response.recv_timeout(Duration::from_secs(1)).ok();
    let returned = ready.is_some();
    writer.write_all(&jpeg).unwrap();
    std::fs::remove_file(&fifo).unwrap();
    drop(writer);
    let late = reader.join().unwrap();
    command.join().unwrap();
    let (mut s, result) = ready.unwrap_or_else(|| response.recv_timeout(Duration::from_secs(10)).unwrap());
    result.unwrap();
    assert!(returned, "folder commit waited for the old thumbnail's I/O");
    assert!(!Arc::ptr_eq(&old_cache, &s.media.rendered));
    s.accept(&late);
    // A writer already running can still finish in its old namespace. It cannot refill the
    // current one, including after another reopen or a cache-budget preference change.
    old_cache.put(key, Arc::new(late.rendered.unwrap().image));
    assert_current_thumbnail(&mut s, &root);
    s.set_cache_mb(20).unwrap();
    s.open_library(root.join("library"), false).unwrap();
    assert_current_thumbnail(&mut s, &root);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
