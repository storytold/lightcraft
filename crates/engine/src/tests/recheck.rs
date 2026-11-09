//! Issue #662: preview-only raws are rechecked when a library opens in another app version.

use std::path::PathBuf;

use lightcraft_catalog::{APP_VERSION, Op, PhotoId};
use serde_json::json;

use crate::Session;
use crate::cmd::recheck;
use crate::files::tests::cr3_with_preview;

/// A version other than this build's: the flag was set by an older (or newer) LightCraft.
const OTHER: &str = "0.5.0";

struct Lib {
    s: Session,
    dir: PathBuf,
    lib: PathBuf,
    file: PathBuf,
    id: PhotoId,
}

impl Drop for Lib {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A library with one preview-only CR3, its flag stamped by [`OTHER`] as a library saved by
/// another version would be.
fn library(name: &str) -> Lib {
    let dir = std::env::temp_dir().join(format!("lc-recheck-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (lib, files) = (dir.join("lib"), dir.join("files"));
    std::fs::create_dir_all(&files).unwrap();
    let file = files.join("IMG_0001.cr3");
    std::fs::write(&file, cr3_with_preview(96, 64)).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    assert!(p.preview_only.is_some() && !p.preview_only_stale(), "just imported: decided by this version");
    s.apply_system(Op::SetContent {
        id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: p.content_hash.clone(),
        preview_only: p.preview_only.clone(),
        preview_checked: OTHER.into(),
    })
    .unwrap();
    assert!(s.catalog.photo(id).unwrap().preview_only_stale());
    Lib { s, dir, lib, file, id }
}

fn decodable_now(x: &Lib) {
    std::fs::write(&x.file, crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default())).unwrap();
}

/// The acceptance test: a stale flag on a file that decodes now is cleared without a manual
/// Reload, adds no undo step, leaves develop settings and history alone, and survives a reopen.
#[test]
fn stale_flag_on_a_file_that_decodes_now_is_cleared() {
    let mut x = library("decodes");
    x.s.execute("develop.set", &json!({"ids": [x.id.0], "control": "light.exposure", "value": 0.5})).unwrap();
    let before = x.s.catalog.photo(x.id).unwrap().clone();
    let undo = x.s.undo.len();
    decodable_now(&x);
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [x.id.0], "cleared": [x.id.0]}));
    let p = x.s.catalog.photo(x.id).unwrap().clone();
    assert_eq!(p.preview_only, None);
    assert_eq!(p.preview_checked, "", "the stamp goes with the flag");
    assert!(p.develops_raw());
    assert_eq!(x.s.undo.len(), undo, "not an undo step");
    assert_eq!(*p.develop, *before.develop);
    assert_eq!(p.history.len(), before.history.len());
    // journaled: a reopen keeps it
    x.s.persist().unwrap();
    x.s = Session::new().with_fs();
    x.s.open_library(&x.lib, false).unwrap();
    assert_eq!(x.s.catalog.photo(x.id).unwrap().preview_only, None);
}

/// A file that still doesn't decode is stamped by this version (with the decoder's current
/// reason), so the next open doesn't probe it again.
#[test]
fn still_undecodable_file_gets_a_fresh_stamp_and_isnt_probed_again() {
    let mut x = library("still");
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [x.id.0], "cleared": []}));
    let p = x.s.catalog.photo(x.id).unwrap();
    assert!(p.preview_only.is_some());
    assert_eq!(p.preview_checked, APP_VERSION);
    assert_eq!(x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap()["checked"], 0);
    // `all` ignores stamps (testing a decoder build of the same version)
    assert_eq!(x.s.execute("library.recheckPreviewOnly", &json!({"all": true})).unwrap()["checked"], 1);
}

/// A file that can't be read (unplugged drive) changes nothing and is tried again next time.
#[test]
fn missing_file_keeps_its_flag_and_stamp_and_is_retried() {
    let mut x = library("missing");
    std::fs::remove_file(&x.file).unwrap();
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [], "cleared": []}));
    let p = x.s.catalog.photo(x.id).unwrap();
    assert!(p.preview_only.is_some());
    assert_eq!(p.preview_checked, OTHER);
    // back again, and decodable now
    decodable_now(&x);
    assert_eq!(x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap()["cleared"], json!([x.id.0]));
}

/// A photo that changed between the probe and applying it (here: relinked) is left alone.
#[test]
fn photo_changed_while_probing_is_skipped() {
    let mut x = library("changed");
    let listed = recheck::stale(&x.s.catalog, false);
    assert_eq!(listed.iter().map(|i| i.id).collect::<Vec<_>>(), vec![x.id]);
    let probed = recheck::probe(&recheck::file_probe(&x.s).unwrap(), listed);
    decodable_now(&x);
    let other = x.dir.join("files").join("IMG_0001-moved.cr3");
    std::fs::write(&other, cr3_with_preview(96, 64)).unwrap();
    x.s.execute("photo.relink", &json!({"id": x.id.0, "path": other.to_string_lossy()})).unwrap();
    let relinked = x.s.catalog.photo(x.id).unwrap().clone();
    assert_eq!(recheck::apply(&mut x.s, probed).unwrap(), recheck::Rechecked::default());
    assert_eq!(*x.s.catalog.photo(x.id).unwrap().clone(), *relinked);
}

/// Virtual copies share the file: they follow the photo they were copied from.
#[test]
fn virtual_copies_follow_their_photo() {
    let mut x = library("copies");
    let r = x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap();
    let copy = PhotoId(r["ids"][0].as_u64().unwrap());
    assert!(x.s.catalog.photo(copy).unwrap().preview_only.is_some());
    assert_eq!(recheck::stale(&x.s.catalog, true).len(), 1, "copies aren't probed on their own");
    decodable_now(&x);
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r["cleared"], json!([x.id.0, copy.0]));
    assert!(x.s.catalog.photo(copy).unwrap().develops_raw());
}

/// Reload and automatic recheck fill newly readable GPS on same-file copies without
/// overwriting their own metadata or importing GPS from a different source file.
#[test]
fn reload_and_recheck_fill_missing_copy_source_metadata() {
    for command in ["photo.reload", "library.recheckPreviewOnly"] {
        let mut x = library(command);
        let copies: Vec<_> = (0..3)
            .map(|_| {
                let r = x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap();
                PhotoId(r["ids"][0].as_u64().unwrap())
            })
            .collect();
        let mut edited = x.s.catalog.photo(copies[1]).unwrap().meta.clone();
        edited.gps = Some((10.0, 20.0));
        edited.caption = "Copy-specific caption".into();
        x.s.catalog.apply(Op::SetMeta { id: copies[1], meta: Box::new(edited.clone()) }).unwrap();
        x.s.catalog
            .apply(Op::Relink {
                id: copies[2],
                file_name: "other.cr3".into(),
                source: lightcraft_catalog::Source::File { path: "other.cr3".into() },
                format: None,
            })
            .unwrap();
        let relinked = x.s.catalog.photo(copies[2]).unwrap().clone();
        let before = x.s.catalog.photo(copies[0]).unwrap().clone();
        let meta =
            lightcraft_meta::Metadata { gps: Some(lightcraft_meta::Gps { latitude: 45.0, longitude: 7.0, altitude: None }), ..Default::default() };
        std::fs::write(&x.file, crate::tests_xmp::synthetic_dng_with(None, meta)).unwrap();
        x.s.execute(command, &json!({"ids": [x.id.0]})).unwrap();
        for id in [x.id, copies[0]] {
            let p = x.s.catalog.photo(id).unwrap();
            assert!(p.develops_raw());
            assert_eq!(p.meta.gps, Some((45.0, 7.0)), "{command}: {id:?}");
        }
        let copy = x.s.catalog.photo(copies[0]).unwrap();
        assert_eq!(copy.develop, before.develop);
        assert_eq!(copy.history, before.history);
        let kept = &x.s.catalog.photo(copies[1]).unwrap().meta;
        assert_eq!(kept.gps, edited.gps);
        assert_eq!(kept.caption, edited.caption);
        assert_eq!(x.s.catalog.photo(copies[2]).unwrap(), &relinked, "{command}: every fact of a relinked copy stays its own");
    }
}

/// Decoder upgrades keep original identity but require sensor-derived offline proxies.
#[test]
fn orf_recheck_and_reload_rebuild_incompatible_smart_previews() {
    use crate::media::{ProbeInfo, SourceLevel};
    use lightcraft_catalog::MediaKind;
    use lightcraft_pipeline::SourceInfo;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    for command in ["library.recheckPreviewOnly", "photo.reload"] {
        let mut x = library(&format!("smart-{command}"));
        let sensor = Arc::new(AtomicBool::new(false));
        let upgraded = sensor.clone();
        x.s.media.file_probe = Some(Arc::new(move |_| {
            Ok(ProbeInfo {
                width: 48,
                height: 32,
                format: "ORF".into(),
                kind: MediaKind::Raw,
                file_size: 1234,
                content_hash: Some("unchanged".into()),
                preview_only: (!upgraded.load(Ordering::SeqCst)).then(|| "compressed ORF".into()),
                ..Default::default()
            })
        }));
        let upgraded = sensor.clone();
        x.s.media.file_loader = Some(Arc::new(move |path, _, _| {
            std::fs::read(path).map_err(|e| e.to_string())?;
            let raw = upgraded.load(Ordering::SeqCst);
            Ok((
                lightcraft_raster::Rgb32f::filled(48, 32, if raw { [0.2, 0.4, 0.6] } else { [0.8; 3] }),
                SourceInfo { raw, relative_wb: raw, ..Default::default() },
            ))
        }));
        let file = x.dir.join("photo.orf");
        std::fs::write(&file, b"synthetic original").unwrap();
        let r = x.s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        let id = PhotoId(r["imported"][0].as_u64().unwrap());
        let r = x.s.execute("photo.virtualCopy", &json!({"ids": [id.0]})).unwrap();
        let copy = PhotoId(r["ids"][0].as_u64().unwrap());
        let before = x.s.catalog.photo(id).unwrap().clone();
        let dir = x.s.media.smart_dir.clone().unwrap();
        let old_path = crate::smart::sharded_file_path(&dir, &before);
        let legacy = dir.join(crate::smart::file_name(&before));
        assert_eq!(x.s.execute("library.smartPreviews", &json!({"ids": [id.0]})).unwrap()["built"], 1);
        let old = std::fs::read(&old_path).unwrap();
        std::fs::write(&legacy, &old).unwrap();
        // Keep the pre-upgrade filename for preview-only ORFs and all other formats.
        let old_name = |p: &lightcraft_catalog::Photo| {
            let h = lightcraft_preview::Hasher128::new().str(&crate::media::content_key(p)).finish();
            format!("{:032x}.lcsp", h.0)
        };
        assert_eq!(crate::smart::file_name(&before), old_name(&before));
        for format in ["DNG", "CR3", "JPEG"] {
            let mut other = (*before).clone();
            other.format = format.into();
            other.preview_only = None;
            assert_eq!(crate::smart::file_name(&other), old_name(&other));
        }

        sensor.store(true, Ordering::SeqCst);
        x.s.execute(command, &json!({"ids": [id.0], "all": true})).unwrap();
        let after = x.s.catalog.photo(id).unwrap().clone();
        let path = crate::smart::sharded_file_path(&dir, &after);
        assert_eq!(crate::media::content_key(&before), crate::media::content_key(&after));
        assert_ne!(path, old_path, "JPEG pixels must not stand in for sensor pixels");
        assert_eq!(path, crate::smart::sharded_file_path(&dir, x.s.catalog.photo(copy).unwrap()));
        // Neither old sharded nor flat proxies may be read or copied into the new identity.
        std::fs::remove_file(&file).unwrap();
        assert!(x.s.media.source_ref(&after, SourceLevel::Preview).load_source().is_err());
        assert_eq!(x.s.execute("library.smartPreviews", &json!({"ids": [id.0]})).unwrap()["built"], 0);
        assert!(!path.exists());
        std::fs::write(&file, b"synthetic original").unwrap();
        let r = x.s.execute("library.smartPreviews", &json!({"ids": [id.0, copy.0]})).unwrap();
        assert!(r["failed"].as_array().unwrap().is_empty(), "{r}");
        assert_ne!(std::fs::read(&path).unwrap(), old);
        std::fs::remove_file(&file).unwrap();
        for id in [id, copy] {
            let p = x.s.catalog.photo(id).unwrap();
            let source = x.s.media.source_ref(p, SourceLevel::Preview).load_source().unwrap();
            assert!(source.from_proxy && source.info.as_ref().unwrap().raw);
            assert!((source.image.data[0][0] - 0.2).abs() < 0.02);
        }
        if command == "photo.reload" {
            x.s.execute("edit.undo", &json!({})).unwrap();
            assert_eq!(crate::smart::sharded_file_path(&dir, x.s.catalog.photo(id).unwrap()), old_path);
            std::fs::remove_file(&old_path).unwrap();
            std::fs::remove_file(&legacy).unwrap();
            std::fs::write(&file, b"synthetic original").unwrap();
            let r = x.s.execute("library.smartPreviews", &json!({"ids": [id.0]})).unwrap();
            assert_eq!(r["built"], 0, "Undo cannot label sensor pixels as preview-only: {r}");
            assert!(r["failed"][0][1].as_str().unwrap().contains("Reload"), "{r}");
            x.s.execute("edit.redo", &json!({})).unwrap();
            assert_eq!(crate::smart::sharded_file_path(&dir, x.s.catalog.photo(id).unwrap()), path);
        }
    }
}

/// A synthetic ORF catalog entry whose current loader can read sensor data.
fn orf_library(name: &str, sensor: bool) -> Lib {
    let mut x = library(name);
    decodable_now(&x);
    if sensor {
        x.s.execute("photo.reload", &json!({"id": x.id.0})).unwrap();
    }
    let mut p = (**x.s.catalog.photo(x.id).unwrap()).clone();
    p.format = "ORF".into();
    x.s.catalog.apply(Op::RemovePhoto { id: x.id }).unwrap();
    x.s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    x
}

fn old_smart_path(dir: &std::path::Path, p: &lightcraft_catalog::Photo, flat: bool) -> PathBuf {
    let hash = lightcraft_preview::Hasher128::new().str(&crate::media::content_key(p)).finish();
    let name = format!("{:032x}.lcsp", hash.0);
    if flat { dir.join(name) } else { dir.join(&name[..2]).join(name) }
}

fn write_proxy(path: &std::path::Path, info: Option<&lightcraft_pipeline::SourceInfo>, stale: bool) -> Vec<u8> {
    let mut bytes = crate::smart::encode(&lightcraft_raster::Rgb32f::filled(48, 32, [0.2, 0.4, 0.6]), info).unwrap();
    if stale {
        let start = b"LCSP1\n".len();
        let end = start + bytes[start..].iter().position(|b| *b == b'\n').unwrap();
        let mut header: serde_json::Value = serde_json::from_slice(&bytes[start..end]).unwrap();
        header["enc"] = json!(0);
        bytes.splice(start..end, header.to_string().bytes());
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

#[test]
fn orf_smart_build_requires_source_facts() {
    let mut x = orf_library("smart-missing-facts", true);
    x.s.media.insert(x.id, crate::media::SourceLevel::Preview, std::sync::Arc::new(lightcraft_raster::Rgb32f::filled(48, 32, [0.7; 3])));
    let r = x.s.execute("library.smartPreviews", &json!({"id": x.id.0})).unwrap();
    assert_eq!(r["built"], 0, "unknown pixels must not be labelled sensor-derived: {r}");
    assert!(r["failed"][0][1].as_str().unwrap().contains("Reload"), "{r}");
    assert!(!crate::smart::file_path(x.s.media.smart_dir.as_ref().unwrap(), x.s.catalog.photo(x.id).unwrap()).exists());
}

#[test]
fn orf_stale_interpretation_mismatch_stays_actionable() {
    let mut x = orf_library("smart-stale-mismatch", false);
    let dir = x.s.media.smart_dir.clone().unwrap();
    let path = crate::smart::sharded_file_path(&dir, x.s.catalog.photo(x.id).unwrap());
    let bytes = write_proxy(&path, Some(&Default::default()), true);
    let r = x.s.execute("library.smartPreviews", &json!({"id": x.id.0})).unwrap();
    assert_eq!((r["built"].clone(), r["staleKept"].clone()), (json!(0), json!(0)), "{r}");
    assert!(r["failed"][0][1].as_str().unwrap().contains("Reload"), "{r}");
    crate::cmd::previews::refresh_stale_smart_previews(&mut x.s).unwrap().join().unwrap();
    assert!(!crate::smart::look_checked(&dir), "a mismatch must not complete the folder refresh");
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "the old proxy is retained");
    x.s.execute("library.smartPreviews", &json!({"id": x.id.0, "background": true})).unwrap();
    let state = x.s.preview_build.as_ref().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while state.running() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!state.running(), "the background build finished");
    let progress = x.s.execute("library.previewProgress", &json!({})).unwrap();
    assert_eq!(progress["failed"], 1);
    assert!(progress["error"].as_str().unwrap().contains("Reload"), "{progress}");
}

#[test]
fn orf_offline_legacy_sensor_proxies_remain_usable() {
    for flat in [false, true] {
        for raw in [None, Some(false), Some(true)] {
            let mut x = orf_library(&format!("smart-compatible-{flat}-{raw:?}"), true);
            let dir = x.s.media.smart_dir.clone().unwrap();
            let p = x.s.catalog.photo(x.id).unwrap().clone();
            let old = old_smart_path(&dir, &p, flat);
            let info = raw.map(|raw| lightcraft_pipeline::SourceInfo { raw, relative_wb: true, ..Default::default() });
            let bytes = write_proxy(&old, info.as_ref(), true);
            std::fs::remove_file(&x.file).unwrap();
            for known_offline in [false, true] {
                // Exercise both original-load fallback and the known-offline Smart source.
                let original = x.file.to_string_lossy().into_owned();
                x.s.media
                    .availability
                    .set_probe(std::sync::Arc::new(move |path| if path == original { !known_offline } else { std::path::Path::new(path).exists() }));
                let source = x.s.media.source_ref(&p, crate::media::SourceLevel::Preview).load_source();
                assert_eq!(source.is_ok(), raw == Some(true), "flat={flat} raw={raw:?} offline={known_offline}");
                if let Ok(source) = source {
                    assert!(source.from_proxy && source.info.unwrap().raw);
                    assert!((source.image.data[0][0] - 0.2).abs() < 0.02);
                }
            }
            assert_eq!(crate::cmd::previews::has_smart_preview(&x.s, x.id), raw == Some(true));
            let r = x.s.execute("library.smartPreviews", &json!({"id": x.id.0})).unwrap();
            assert_eq!(r["built"], usize::from(raw == Some(true)), "{r}");
            if raw == Some(true) {
                assert_eq!(r["staleKept"], 1, "an offline legacy proxy must keep its old encoding stamp");
            }
            assert_eq!(std::fs::read(&old).unwrap(), bytes);
            if raw == Some(true) {
                // A hostile facts length cannot establish compatibility or size an allocation.
                let start = b"LCSP1\n".len();
                let end = start + bytes[start..].iter().position(|b| *b == b'\n').unwrap();
                let head: serde_json::Value = serde_json::from_slice(&bytes[start..end]).unwrap();
                let tail = bytes.len() - head["tail"].as_u64().unwrap() as usize;
                let mut broken = bytes.clone();
                broken[tail..tail + 4].copy_from_slice(&u32::MAX.to_le_bytes());
                std::fs::write(&old, broken).unwrap();
                assert!(x.s.media.source_ref(&p, crate::media::SourceLevel::Preview).load_source().is_err());
            }
        }
    }
}

#[test]
fn orf_discard_cleans_obsolete_proxies_unless_another_photo_uses_them() {
    for shared in [None, Some(false), Some(true)] {
        let mut x = orf_library(&format!("smart-discard-{shared:?}"), true);
        let dir = x.s.media.smart_dir.clone().unwrap();
        let p = x.s.catalog.photo(x.id).unwrap().clone();
        if let Some(preview_only) = shared {
            let r = x.s.execute("photo.virtualCopy", &json!({"id": x.id.0})).unwrap();
            let copy = PhotoId(r["ids"][0].as_u64().unwrap());
            let mut other = (**x.s.catalog.photo(copy).unwrap()).clone();
            other.preview_only = preview_only.then(|| "not rechecked yet".into());
            x.s.catalog.apply(Op::RemovePhoto { id: copy }).unwrap();
            x.s.catalog.apply(Op::AddPhoto { photo: Box::new(other) }).unwrap();
        }
        let paths = [
            crate::smart::sharded_file_path(&dir, &p),
            dir.join(crate::smart::file_name(&p)),
            old_smart_path(&dir, &p, false),
            old_smart_path(&dir, &p, true),
        ];
        for path in &paths {
            write_proxy(path, Some(&Default::default()), false);
        }
        x.s.execute("library.smartPreviews", &json!({"ids": [x.id.0], "discard": true})).unwrap();
        assert!(!paths[0].exists() && !paths[1].exists(), "both current layouts must be removed");
        assert_eq!(paths[2].exists(), shared.is_some());
        assert_eq!(paths[3].exists(), shared.is_some());
    }
}

/// Without file access every probe would "succeed" knowing nothing and clear every flag: refused.
#[test]
fn session_without_file_access_refuses() {
    let mut x = library("nofs");
    x.s.media.file_probe = None;
    let e = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap_err().to_string();
    assert!(e.contains("no file access"), "{e}");
    assert!(x.s.catalog.photo(x.id).unwrap().preview_only.is_some());
}
