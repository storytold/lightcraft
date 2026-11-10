//! Issue #3921: Nikon High Efficiency NEFs developed from a DNG that Adobe DNG Converter wrote into LightCraft's cache.
//!
//! Adobe's converter isn't needed here: stand-in programs (shell scripts, so these run on Unix) do what the command
//! line asks, writing a DNG made by our own writer, or fail, hang or write something else. The photo is a raw this
//! build can't decode, named as a NEF and marked with the High Efficiency reason as the decoder would. The test
//! against the real converter and a real HE NEF runs only when both are there (`LIGHTCRAFT_HE_NEF` names the file).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lightcraft_catalog::{Op, PhotoId};
use serde_json::json;

use crate::Session;
use crate::dng_converter as dc;

const HE: &str = "Nikon High Efficiency NEF (test)";

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-dngconv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A program standing in for the converter, named `name`, running `body` (sh). The arguments are
/// `-c -p0 -d <dir> -o <name> <input>`: `$4` is the output folder, `$6` the file name, `$7` the input.
#[cfg(unix)]
fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// A stand-in that checks the argument layout and copies `dng` to the output.
#[cfg(unix)]
fn copying(dir: &Path, dng: &Path) -> PathBuf {
    let body = format!(
        "[ \"$1\" = -c ] && [ \"$2\" = -p0 ] && [ \"$3\" = -d ] && [ \"$5\" = -o ] && [ -f \"$7\" ] || exit 2\ncp '{}' \"$4/$6\"",
        dng.display()
    );
    stand_in(dir, "convert.sh", &body)
}

/// One NEF in a fresh library, shown from its preview with the High Efficiency reason, and the DNG a stand-in copies.
#[cfg(unix)]
struct Setup {
    s: Session,
    dir: PathBuf,
    cache: PathBuf,
    nef: PathBuf,
    dng: PathBuf,
    id: PhotoId,
    original: Vec<u8>,
}

#[cfg(unix)]
impl Drop for Setup {
    fn drop(&mut self) {
        dc::set_cache_dir(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(unix)]
fn setup(tag: &str) -> Setup {
    let dir = temp_dir(tag);
    let cache = dir.join("cache");
    dc::set_cache_dir(Some(cache.clone()));
    let dng = dir.join("made.dng");
    std::fs::write(&dng, super::xmp::synthetic_dng_with(None, Default::default())).unwrap();
    let nef = dir.join("DSC_0001.NEF");
    let original = super::xmp::turned_preview_only_raw("");
    std::fs::write(&nef, &original).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [nef.to_string_lossy()]})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().unwrap());
    // as the decoder names a High Efficiency NEF
    let p = s.catalog.photo(id).unwrap().clone();
    assert!(p.preview_only.is_some() && p.content_hash.is_some());
    s.apply_system(Op::SetContent {
        id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: p.content_hash.clone(),
        preview_only: Some(HE.into()),
        preview_checked: lightcraft_catalog::APP_VERSION.into(),
    })
    .unwrap();
    Setup { s, dir, cache, nef, dng, id, original }
}

#[cfg(unix)]
fn hash_of(x: &Setup) -> String {
    x.s.catalog.photo(x.id).unwrap().content_hash.clone().unwrap()
}

#[cfg(unix)]
#[test]
fn a_converted_dng_develops_the_photo_and_the_nef_stays_its_file() {
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut x = setup("develop");
    let before = x.s.catalog.photo(x.id).unwrap().clone();
    let conv = copying(&x.dir, &x.dng);
    dc::set_settings(dc::Settings { path: Some(conv), ..Default::default() });

    let r = x.s.execute("photo.convertHighEfficiency", &json!({"ids": [x.id.0], "wait": true})).unwrap();
    dc::set_settings(dc::Settings::default());
    assert_eq!(r["converted"], json!([x.id.0]), "{r}");
    assert_eq!(r["failed"], json!([]));
    assert_eq!(r["reloaded"], json!([x.id.0]));
    // the DNG is in the cache, not next to the original, and the original is unchanged
    assert_eq!(std::fs::read_dir(&x.cache).unwrap().count(), 1, "only the DNG, no temporary folder");
    let beside: Vec<String> = std::fs::read_dir(&x.dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert!(!beside.iter().any(|n| n.ends_with(".dng") && n != "made.dng"), "no DNG next to the original: {beside:?}");
    assert_eq!(std::fs::read(&x.nef).unwrap(), x.original, "the original is never written");
    // the photo develops from the DNG, and the catalog says so
    let after = x.s.catalog.photo(x.id).unwrap().clone();
    assert!(after.converted_dng(), "{:?} {:?}", after.preview_only, after.preview_checked);
    assert_eq!((after.width, after.height), (32, 24), "the DNG's size");
    assert_eq!(after.format, "NEF");
    assert_eq!(after.content_hash, before.content_hash, "the NEF's hash");
    assert!(after.as_shot_wb.is_some(), "the DNG's as-shot white");
    assert!(dc::photo_dng(&after).is_some_and(|d| d.is_file()));
    assert!(x.s.render_now(x.id, 32, 24).is_ok());

    // undoing the reload puts it back on its embedded preview: the DNG is no longer read for it
    x.s.execute("edit.undo", &json!({})).unwrap();
    let undone = x.s.catalog.photo(x.id).unwrap().clone();
    assert!(!undone.converted_dng() && undone.preview_only.is_some());
    assert_eq!(dc::photo_dng(&undone), None);
    x.s.execute("edit.redo", &json!({})).unwrap();
    assert!(x.s.catalog.photo(x.id).unwrap().converted_dng());

    // a smart preview made while the photo developed from its DNG
    let smart = x.dir.join("smart");
    x.s.media.smart_dir = Some(smart.clone());
    let proxy = crate::smart::sharded_file_path(&smart, x.s.catalog.photo(x.id).unwrap());
    std::fs::create_dir_all(proxy.parent().unwrap()).unwrap();
    std::fs::write(&proxy, b"proxy").unwrap();
    // deleting the converted DNGs (the app reloads itself, in the background) …
    let r = x.s.execute("library.clearDngCache", &json!({"reload": false})).unwrap();
    assert_eq!(r["deletedFiles"], 1);
    assert!(!proxy.exists(), "the smart preview made from the deleted DNG is removed, also without a reload");
    assert_eq!(r["converted"], json!([x.id.0]));
    assert_eq!(r["reloaded"], json!([]));
    // … then Reload puts the photo back on its embedded preview
    x.s.execute("photo.reload", &json!({"ids": [x.id.0], "wait": true})).unwrap();
    let back = x.s.catalog.photo(x.id).unwrap().clone();
    assert!(back.preview_only.is_some() && !back.converted_dng());
}

/// Deleting the converted DNGs keeps the smart preview of a photo whose NEF is offline: nothing else could develop it.
#[cfg(unix)]
#[test]
fn an_offline_photo_keeps_its_smart_preview_when_the_dngs_are_deleted() {
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut x = setup("offline");
    let conv = copying(&x.dir, &x.dng);
    dc::set_settings(dc::Settings { path: Some(conv), ..Default::default() });
    let r = x.s.execute("photo.convertHighEfficiency", &json!({"ids": [x.id.0], "wait": true})).unwrap();
    dc::set_settings(dc::Settings::default());
    assert_eq!(r["converted"], json!([x.id.0]), "{r}");
    let smart = x.dir.join("smart");
    x.s.media.smart_dir = Some(smart.clone());
    let proxy = crate::smart::sharded_file_path(&smart, x.s.catalog.photo(x.id).unwrap());
    std::fs::create_dir_all(proxy.parent().unwrap()).unwrap();
    std::fs::write(&proxy, b"proxy").unwrap();
    // the NEF's drive is not connected
    std::fs::rename(&x.nef, x.dir.join("away.bin")).unwrap();
    let r = x.s.execute("library.clearDngCache", &json!({"reload": false})).unwrap();
    assert_eq!(r["deletedFiles"], 1);
    assert!(proxy.exists(), "the offline photo keeps its smart preview");
}

#[cfg(unix)]
#[test]
fn a_failed_conversion_leaves_nothing_and_says_why() {
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let x = setup("fail");
    let hash = hash_of(&x);
    let no = AtomicBool::new(false);
    let failing = stand_in(&x.dir, "fail.sh", "exit 3");
    let e = dc::convert(&failing, &x.nef, &hash, dc::DEFAULT_TIMEOUT, &no).unwrap_err();
    assert!(e.contains("failed"), "{e}");
    // something that isn't a DNG
    let text = stand_in(&x.dir, "text.sh", "echo hello > \"$4/$6\"");
    let e = dc::convert(&text, &x.nef, &hash, dc::DEFAULT_TIMEOUT, &no).unwrap_err();
    assert!(e.contains("not a DNG"), "{e}");
    // nothing written at all
    let silent = stand_in(&x.dir, "silent.sh", "exit 0");
    let e = dc::convert(&silent, &x.nef, &hash, dc::DEFAULT_TIMEOUT, &no).unwrap_err();
    assert!(e.contains("wrote no DNG"), "{e}");
    // the NEF changes while it is converted: its DNG isn't cached under the hash it had before
    let changing = stand_in(&x.dir, "changing.sh", &format!("printf more >> \"$7\"\ncp '{}' \"$4/$6\"", x.dng.display()));
    let e = dc::convert(&changing, &x.nef, &hash, dc::DEFAULT_TIMEOUT, &no).unwrap_err();
    assert!(e.contains("changed while it was converted"), "{e}");
    assert_eq!(std::fs::read_dir(&x.cache).unwrap().count(), 0, "no DNG and no temporary folder left behind");
    assert_eq!(dc::dng_path(&x.nef, &hash).map(|d| d.is_file()), Some(false));
}

#[cfg(unix)]
#[test]
fn a_hanging_converter_is_stopped_by_the_timeout_and_by_cancel() {
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let x = setup("hang");
    let hash = hash_of(&x);
    let hanging = stand_in(&x.dir, "hang.sh", "exec sleep 30");
    let t = std::time::Instant::now();
    let e = dc::convert(&hanging, &x.nef, &hash, Duration::from_millis(300), &AtomicBool::new(false)).unwrap_err();
    assert!(e.contains("longer than"), "{e}");
    assert!(t.elapsed() < Duration::from_secs(10), "stopped, not waited for");
    let t = std::time::Instant::now();
    let e = dc::convert(&hanging, &x.nef, &hash, dc::DEFAULT_TIMEOUT, &AtomicBool::new(true)).unwrap_err();
    assert!(e.contains("stopped"), "{e}");
    assert!(t.elapsed() < Duration::from_secs(10));
    assert_eq!(std::fs::read_dir(&x.cache).unwrap().count(), 0, "no temporary folder left behind");
}

#[cfg(unix)]
#[test]
fn several_files_convert_in_parallel_and_each_gets_its_own_result() {
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let x = setup("parallel");
    let conv = copying(&x.dir, &x.dng);
    // more files than workers, one of them not a NEF
    let mut jobs = Vec::new();
    for i in 0..dc::PARALLEL + 2 {
        let p = x.dir.join(format!("DSC_{i:04}.NEF"));
        std::fs::write(&p, format!("file {i}")).unwrap();
        jobs.push(dc::Job { path: p, content_hash: format!("hash{i}") });
    }
    jobs.push(dc::Job { path: x.dir.join("x.jpg"), content_hash: "h".into() });
    let done = std::sync::atomic::AtomicUsize::new(0);
    let results = dc::convert_all(&conv, &jobs, dc::DEFAULT_TIMEOUT, &AtomicBool::new(false), &|n| {
        done.store(n, std::sync::atomic::Ordering::Relaxed);
    });
    assert_eq!(results.len(), jobs.len());
    assert!(results[..dc::PARALLEL + 2].iter().all(Result::is_ok), "{results:?}");
    assert!(results.last().is_some_and(Result::is_err), "not a NEF");
    assert_eq!(done.load(std::sync::atomic::Ordering::Relaxed), jobs.len());
    assert_eq!(dc::cache_usage().0, dc::PARALLEL + 2);
    // a stopped batch starts nothing
    let stopped = dc::convert_all(&conv, &jobs, dc::DEFAULT_TIMEOUT, &AtomicBool::new(true), &|_| {});
    assert!(stopped.iter().all(|r| r.as_ref().is_err_and(|e| e.contains("stopped"))), "{stopped:?}");
}

#[cfg(unix)]
#[test]
fn a_changed_nef_needs_a_new_dng() {
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut x = setup("changed");
    let conv = copying(&x.dir, &x.dng);
    dc::set_settings(dc::Settings { path: Some(conv), ..Default::default() });
    x.s.execute("photo.convertHighEfficiency", &json!({"ids": [x.id.0], "wait": true})).unwrap();
    dc::set_settings(dc::Settings::default());
    assert!(x.s.catalog.photo(x.id).unwrap().converted_dng());
    // the file changes on disk (still undecodable): Reload finds no DNG for its new content
    let mut changed = x.original.clone();
    changed.extend_from_slice(b"more");
    std::fs::write(&x.nef, &changed).unwrap();
    x.s.execute("photo.reload", &json!({"ids": [x.id.0], "wait": true})).unwrap();
    let p = x.s.catalog.photo(x.id).unwrap().clone();
    assert!(!p.converted_dng() && p.preview_only.is_some(), "shown from its preview until converted again");
}

#[cfg(unix)]
#[test]
fn clearing_the_cache_keeps_a_conversion_that_is_running() {
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let x = setup("clear-running");
    let hash = hash_of(&x);
    // a slow converter on a worker thread; the cache is cleared while it runs
    let slow = stand_in(&x.dir, "slow.sh", &format!("sleep 1\ncp '{}' \"$4/$6\"", x.dng.display()));
    let nef = x.nef.clone();
    let worker = std::thread::spawn(move || dc::convert(&slow, &nef, &hash, dc::DEFAULT_TIMEOUT, &AtomicBool::new(false)));
    std::thread::sleep(Duration::from_millis(300));
    dc::clear_cache();
    let r = worker.join().unwrap();
    assert!(r.as_ref().is_ok_and(|d| d.is_file()), "{r:?}");
}

#[test]
fn the_settings_command_checks_and_reports() {
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let saved = dc::settings();
    let mut s = Session::new();
    let r = s.execute("app.dngConverter", &json!({"auto": true, "path": "/nowhere/Adobe DNG Converter", "timeoutSeconds": 30})).unwrap();
    assert_eq!(r["auto"], true);
    assert_eq!(r["path"], "/nowhere/Adobe DNG Converter");
    assert_eq!(r["timeoutSeconds"], 30);
    assert_eq!(r["found"], serde_json::Value::Null, "a set path that is missing finds nothing");
    assert!(s.execute("app.dngConverter", &json!({"timeoutSeconds": 1})).is_err());
    assert!(s.execute("app.dngConverter", &json!({"auto": "yes"})).is_err());
    assert!(s.execute("app.dngConverter", &json!({"path": 5})).is_err());
    let r = s.execute("app.dngConverter", &json!({"path": null, "auto": false})).unwrap();
    assert_eq!(r["path"], serde_json::Value::Null);
    assert_eq!(r["auto"], false);
    dc::set_settings(saved);
}

/// A photo that isn't a High Efficiency NEF shown from its preview is refused with the reason.
#[test]
fn only_high_efficiency_nefs_are_converted() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let e = s.execute("photo.convertHighEfficiency", &json!({"ids": [id.0], "wait": true})).unwrap_err().to_string();
    assert!(e.contains("High Efficiency"), "{e}");
}

/// The real thing: Adobe DNG Converter on a real HE NEF (`LIGHTCRAFT_HE_NEF`). Skipped unless both are there.
#[test]
fn adobe_dng_converter_converts_a_real_high_efficiency_nef() {
    let Some(nef) = std::env::var_os("LIGHTCRAFT_HE_NEF").map(PathBuf::from).filter(|p| p.is_file()) else {
        lightcraft_testkit::skip!(Input, "LIGHTCRAFT_HE_NEF names no High Efficiency NEF");
        return;
    };
    let bytes = std::fs::read(&nef).unwrap();
    let Err(lightcraft_raw::RawError::Unsupported(why)) = lightcraft_raw::probe_info(&bytes) else { panic!("not an undecodable NEF") };
    assert!(lightcraft_raw::is_nef_high_efficiency(&why), "{why}");
    // the app offers it for conversion
    let info = crate::files::probe_bytes(&nef.to_string_lossy(), &bytes).unwrap();
    assert!(info.preview_only.as_deref().is_some_and(lightcraft_raw::is_nef_high_efficiency));
    let hash = info.content_hash.clone().unwrap();
    let _serial = dc::TEST_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(converter) = dc::converter() else {
        lightcraft_testkit::skip!(System, "Adobe DNG Converter is not installed");
        return;
    };
    let dir = temp_dir("real");
    dc::set_cache_dir(Some(dir.clone()));
    let dng = dc::convert(&converter, &nef, &hash, dc::DEFAULT_TIMEOUT, &AtomicBool::new(false));
    dc::set_cache_dir(None);
    let dng = dng.unwrap();
    let raw = lightcraft_raw::decode(&std::fs::read(&dng).unwrap()).unwrap();
    assert_eq!(raw.format, lightcraft_raw::RawFormat::Dng);
    assert!(raw.width > 4000, "{}", raw.width);
    let _ = std::fs::remove_dir_all(&dir);
}
