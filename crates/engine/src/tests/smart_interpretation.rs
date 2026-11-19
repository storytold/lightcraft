//! Proxy pixels must keep the interpretation that decoded them, including across Undo and offline use.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId};
use lightcraft_pipeline::SourceInfo;
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};

use crate::Session;
use crate::media::SourceLevel;

struct Lib {
    s: Session,
    dir: PathBuf,
    file: PathBuf,
    id: PhotoId,
}

impl Lib {
    fn new(name: &str, sensor: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-smart-interpretation-{name}-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("sensor.dng");
        std::fs::write(&file, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(dir.join("library"), false).unwrap();
        let r = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        let id = PhotoId(r["imported"][0].as_u64().unwrap());
        let mut x = Self { s, dir, file, id };
        if !sensor {
            x.set_preview(true, false);
        }
        x
    }

    fn photo(&self) -> Arc<Photo> {
        self.s.catalog.photo(self.id).unwrap().clone()
    }

    fn smart_dir(&self) -> PathBuf {
        self.s.media.smart_dir.clone().unwrap()
    }

    fn set_preview(&mut self, preview: bool, undo: bool) {
        let p = self.photo();
        let flag = preview.then(|| "previous decoder".into());
        let op = Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: p.content_hash.clone(),
            preview_checked: p.preview_stamp(&flag),
            preview_only: flag,
        };
        if undo {
            self.s.commit("Source interpretation", op).unwrap();
        } else {
            self.s.apply_system(op).unwrap();
            self.s.media.forget(p.id);
        }
    }

    fn build(&mut self) -> Value {
        self.s.execute("library.smartPreviews", &json!({"id": self.id.0})).unwrap()
    }

    fn status(&mut self) -> bool {
        self.s.execute("photo.smartPreview", &json!({"id": self.id.0})).unwrap()["smartPreview"].as_bool().unwrap()
    }

    fn offline_source(&mut self) -> Result<crate::media::DecodedSource, String> {
        let photo = self.photo();
        self.s.media.forget(self.id);
        self.s.media.source_ref(&photo, SourceLevel::Preview).load_source()
    }

    fn finish(self) {
        drop(self.s);
        std::fs::remove_dir_all(self.dir).unwrap();
    }
}

fn old_path(dir: &Path, photo: &Photo, flat: bool) -> PathBuf {
    let hash = lightcraft_preview::Hasher128::new().str(&crate::media::content_key(photo)).finish();
    let name = format!("{:032x}.lcsp", hash.0);
    if flat { dir.join(name) } else { dir.join(&name[..2]).join(name) }
}

fn write_proxy(path: &Path, info: Option<&SourceInfo>, stale: bool) -> Vec<u8> {
    let mut bytes = crate::smart::encode(&Rgb32f::filled(48, 32, [0.2, 0.4, 0.6]), info).unwrap();
    if stale {
        let (start, end, mut header) = header(&bytes);
        header["enc"] = json!(0);
        bytes.splice(start..end, header.to_string().bytes());
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn header(bytes: &[u8]) -> (usize, usize, Value) {
    let start = b"LCSP1\n".len();
    let end = start + bytes[start..].iter().position(|b| *b == b'\n').unwrap();
    (start, end, serde_json::from_slice(&bytes[start..end]).unwrap())
}

fn wait_build(s: &Session) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.preview_build.as_ref().is_some_and(|state| state.running()) {
        assert!(Instant::now() < deadline, "the proxy build finished");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn raw_smart_build_requires_source_facts() {
    let mut x = Lib::new("missing-facts", true);
    x.s.media.insert(x.id, SourceLevel::Preview, Arc::new(Rgb32f::filled(48, 32, [0.7; 3])));
    let r = x.build();
    assert_eq!(r["built"], 0, "unknown pixels must not be labelled sensor-derived: {r}");
    assert!(r["failed"][0][1].as_str().unwrap().contains("Reload"), "{r}");
    assert!(!x.status());
    x.finish();
}

#[test]
fn stale_interpretation_mismatch_does_not_complete_the_refresh() {
    let mut x = Lib::new("stale-mismatch", false);
    let dir = x.smart_dir();
    let path = crate::smart::sharded_file_path(&dir, &x.photo());
    let bytes = write_proxy(&path, Some(&Default::default()), true);
    let r = x.build();
    assert_eq!((r["built"].clone(), r["staleKept"].clone()), (json!(0), json!(0)), "{r}");
    assert!(r["failed"][0][1].as_str().unwrap().contains("Reload"), "{r}");
    let loader = x.s.media.file_loader.clone().unwrap();
    let activity = x.s.activity.clone();
    x.s.media.file_loader = Some(Arc::new(move |path, edge, options| {
        assert!(activity.list().iter().any(|row| row.kind == "smartPreviews"), "the opening refresh holds an activity row");
        loader(path, edge, options)
    }));
    crate::cmd::previews::refresh_stale_smart_previews(&mut x.s).unwrap().join().unwrap();
    assert!(!crate::smart::look_checked(&dir), "a mismatch must not complete the opening refresh");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    x.s.execute("library.smartPreviews", &json!({"id": x.id.0, "background": true})).unwrap();
    wait_build(&x.s);
    let progress = x.s.execute("library.previewProgress", &json!({})).unwrap();
    assert_eq!(progress["failed"], 1);
    assert!(progress["error"].as_str().unwrap().contains("Reload"), "{progress}");
    x.finish();
}

#[test]
fn undo_during_a_build_cannot_overwrite_another_interpretation() {
    let mut x = Lib::new("undo-worker", false);
    let preview = crate::smart::sharded_file_path(&x.smart_dir(), &x.photo());
    let bytes = write_proxy(&preview, Some(&Default::default()), true);
    x.set_preview(false, true);
    let sensor = crate::smart::sharded_file_path(&x.smart_dir(), &x.photo());
    let (entered, receive_entered) = mpsc::channel();
    let (release, receive_release) = mpsc::channel();
    let receive_release = Mutex::new(receive_release);
    x.s.media.file_loader = Some(Arc::new(move |_, _, _| {
        entered.send(()).unwrap();
        receive_release.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
        Ok((Rgb32f::filled(48, 32, [0.7; 3]), SourceInfo { raw: true, ..Default::default() }))
    }));
    x.s.execute("library.smartPreviews", &json!({"id": x.id.0, "background": true})).unwrap();
    receive_entered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(x.s.activity.list().iter().any(|row| row.kind == "smartPreviews"));
    x.s.execute("edit.undo", &json!({})).unwrap();
    assert!(x.photo().preview_only.is_some());
    release.send(()).unwrap();
    wait_build(&x.s);
    assert_eq!(std::fs::read(&preview).unwrap(), bytes, "the old job only writes its captured sensor identity");
    assert!(crate::smart::load(&sensor).unwrap().info.unwrap().raw);
    std::fs::remove_file(&x.file).unwrap();
    assert!(!x.offline_source().unwrap().info.unwrap().raw);
    x.s.execute("edit.redo", &json!({})).unwrap();
    assert!(x.offline_source().unwrap().info.unwrap().raw);
    x.finish();
}

#[test]
fn relink_ignores_wrong_destination_pixels_and_preserves_undo_files() {
    let mut x = Lib::new("relink-files", false);
    let old = crate::smart::sharded_file_path(&x.smart_dir(), &x.photo());
    let old_bytes = write_proxy(&old, Some(&Default::default()), false);
    let target = x.dir.join("destination.dng");
    std::fs::write(&target, crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata { iso: Some(200), ..Default::default() })).unwrap();
    let r = x.s.execute("library.import", &json!({"paths": [target.to_string_lossy()]})).unwrap();
    let destination = PhotoId(r["imported"][0].as_u64().unwrap());
    assert_ne!(
        crate::media::content_key(&x.photo()),
        crate::media::content_key(x.s.catalog.photo(destination).unwrap()),
        "Relink changes content identities"
    );
    let legacy = old_path(&x.smart_dir(), x.s.catalog.photo(destination).unwrap(), true);
    let legacy_bytes = write_proxy(&legacy, Some(&Default::default()), false);
    x.s.execute("photo.relink", &json!({"id": x.id.0, "path": target.to_string_lossy()})).unwrap();
    assert!(x.photo().develops_raw());
    assert!(!x.status(), "the destination's preview pixels cannot stand in for its sensor data");
    assert_eq!(x.build()["built"], 1);
    let sensor = crate::smart::sharded_file_path(&x.smart_dir(), &x.photo());
    let sensor_bytes = std::fs::read(&sensor).unwrap();
    assert!(crate::smart::load(&sensor).unwrap().info.unwrap().raw);
    assert_eq!(std::fs::read(&old).unwrap(), old_bytes);
    assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
    std::fs::remove_file(&x.file).unwrap();
    std::fs::remove_file(&target).unwrap();
    // A later invalid op rolls the raw→preview edit back without deleting offline-only pixels.
    let before = x.s.catalog.to_snapshot();
    assert!(
        x.s.commit(
            "Invalid interpretation edit",
            Op::Batch {
                ops: vec![
                    Op::SetContent {
                        id: x.id,
                        width: 9,
                        height: 9,
                        file_size: 1,
                        content_hash: Some("changed".into()),
                        preview_only: Some("changed interpretation".into()),
                        preview_checked: "".into()
                    },
                    Op::RemovePhoto { id: PhotoId(u64::MAX) },
                ]
            }
        )
        .is_err()
    );
    assert_eq!(x.s.catalog.to_snapshot(), before);
    assert_eq!(std::fs::read(&old).unwrap(), old_bytes);
    assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
    assert_eq!(std::fs::read(&sensor).unwrap(), sensor_bytes);
    assert!(x.offline_source().unwrap().info.unwrap().raw);
    x.s.execute("edit.undo", &json!({})).unwrap();
    assert!(!x.offline_source().unwrap().info.unwrap().raw);
    x.s.execute("edit.redo", &json!({})).unwrap();
    assert!(x.offline_source().unwrap().info.unwrap().raw);
    x.finish();
}

#[test]
fn rebuilding_preserves_content_only_legacy_bytes() {
    for flat in [false, true] {
        let mut x = Lib::new(&format!("rebuild-{flat}"), true);
        let legacy = old_path(&x.smart_dir(), &x.photo(), flat);
        let bytes = write_proxy(&legacy, Some(&SourceInfo { raw: true, ..Default::default() }), true);
        assert_eq!(x.build()["built"], 1);
        assert_eq!(std::fs::read(&legacy).unwrap(), bytes, "a rebuild cannot replace another interpretation's pixels");
        let current = crate::smart::sharded_file_path(&x.smart_dir(), &x.photo());
        assert!(crate::smart::load(&current).unwrap().info.unwrap().raw);
        assert!(!crate::smart::is_stale(&current));
        x.finish();
    }
}

#[test]
fn build_repairs_or_rejects_undecodable_candidates() {
    for offline in [false, true] {
        for stale in [false, true] {
            for fallback in [false, true] {
                let mut x = Lib::new(&format!("corrupt-{offline}-{stale}-{fallback}"), true);
                let photo = x.photo();
                let dir = x.smart_dir();
                let current = crate::smart::sharded_file_path(&dir, &photo);
                let info = SourceInfo { raw: true, ..Default::default() };
                let mut bytes = write_proxy(&current, Some(&info), stale);
                let (_, end, head) = header(&bytes);
                let tail = bytes.len() - head["tail"].as_u64().unwrap() as usize;
                bytes[end + 3..tail - 2].fill(0); // SOI/EOI remain valid; actual JPEG decoding must reject it.
                std::fs::write(&current, bytes).unwrap();
                assert!(crate::smart::is_valid(&current));
                assert!(crate::smart::load(&current).is_err());
                if fallback {
                    write_proxy(&old_path(&dir, &photo, true), Some(&info), stale);
                }
                if offline {
                    std::fs::remove_file(&x.file).unwrap();
                }
                let r = x.build();
                let usable = !offline || fallback;
                assert_eq!(r["built"], usize::from(usable), "{offline}/{stale}/{fallback}: {r}");
                assert_eq!(r["failed"].as_array().unwrap().is_empty(), usable, "{r}");
                assert_eq!(r["staleKept"], usize::from(offline && stale && fallback), "{r}");
                assert_eq!(x.status(), usable);
                assert_eq!(x.s.media.smart_availability.exists(&dir, &photo), usable);
                assert_eq!(x.offline_source().is_ok(), usable);
                x.finish();
            }
        }
    }
}

#[test]
fn damaged_candidates_fall_back_in_every_layout() {
    for decode_failure in [false, true] {
        for good_at in 1..4 {
            let mut x = Lib::new(&format!("fallback-{decode_failure}-{good_at}"), true);
            let photo = x.photo();
            let dir = x.smart_dir();
            let paths = crate::smart::PreviewPath::new(&dir, &photo).candidates();
            let info = SourceInfo { raw: true, ..Default::default() };
            for path in &paths[..good_at] {
                let mut bytes = write_proxy(path, Some(&info), false);
                if decode_failure {
                    let (_, end, head) = header(&bytes);
                    let tail = bytes.len() - head["tail"].as_u64().unwrap() as usize;
                    bytes[end + 3..tail - 2].fill(0);
                } else {
                    bytes.truncate(10);
                }
                std::fs::write(path, bytes).unwrap();
            }
            write_proxy(&paths[good_at], Some(&info), false);
            std::fs::remove_file(&x.file).unwrap();
            assert!(x.status());
            assert!(x.s.media.smart_availability.exists(&dir, &photo));
            let source = x.offline_source().unwrap();
            assert!(source.from_proxy && source.info.unwrap().raw);
            assert!((source.image.data[0][0] - 0.2).abs() < 0.02);
            x.finish();
        }
    }
}

#[test]
fn malformed_declared_facts_cannot_pass_status_build_or_loading() {
    let cases = [
        (json!([]), vec![0]),
        (json!([{"kind": 255, "w": 1, "h": 1, "len": 1}]), vec![0]),
        (json!([{"kind": 0, "w": 0, "h": 1, "len": 0}]), vec![]),
        (json!([{"kind": 0, "w": 2, "h": 2, "len": 1}]), vec![0]),
        (json!([{"kind": 0, "w": usize::MAX, "h": 2, "len": 1}]), vec![0]),
        (json!([{"kind": 0, "w": 2, "h": 2, "len": 4}]), vec![0; 3]),
    ];
    for (i, (mattes, planes)) in cases.into_iter().enumerate() {
        let mut x = Lib::new(&format!("malformed-tail-{i}"), true);
        let photo = x.photo();
        let path = old_path(&x.smart_dir(), &photo, i % 2 == 0);
        let bytes = write_proxy(&path, Some(&SourceInfo { raw: true, ..Default::default() }), false);
        let (_, end, mut head) = header(&bytes);
        let tail = bytes.len() - head["tail"].as_u64().unwrap() as usize;
        let mut facts: Value = serde_json::from_slice(&bytes[tail + 4..]).unwrap();
        facts["mattes"] = mattes;
        let facts = serde_json::to_vec(&facts).unwrap();
        head["tail"] = json!(4 + facts.len() + planes.len());
        let mut broken = format!("LCSP1\n{head}\n").into_bytes();
        broken.extend_from_slice(&bytes[end + 1..tail]);
        broken.extend_from_slice(&(facts.len() as u32).to_le_bytes());
        broken.extend(facts);
        broken.extend(planes);
        std::fs::write(&path, &broken).unwrap();
        assert!(crate::smart::decode(&broken).is_err(), "malformed tail{i}");
        assert!(crate::smart::is_valid(&path), "JPEG framing alone cannot validate the tail");
        std::fs::remove_file(&x.file).unwrap();
        assert!(!x.status(), "malformed tail{i}");
        assert!(!x.s.media.smart_availability.exists(&x.smart_dir(), &photo));
        assert!(x.offline_source().is_err());
        assert_eq!(x.build()["built"], 0);
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        x.finish();
    }
}

#[test]
fn discard_preserves_every_unselected_consumer() {
    for preview_copy in [false, true] {
        for legacy in [false, true] {
            let mut x = Lib::new(&format!("discard-{preview_copy}-{legacy}"), true);
            let r = x.s.execute("photo.virtualCopy", &json!({"id": x.id.0})).unwrap();
            let copy = PhotoId(r["ids"][0].as_u64().unwrap());
            let parent = x.id;
            x.id = copy;
            x.set_preview(preview_copy, false);
            x.id = parent;
            let photo = x.photo();
            let path = if legacy { old_path(&x.smart_dir(), &photo, true) } else { crate::smart::sharded_file_path(&x.smart_dir(), &photo) };
            let bytes = write_proxy(&path, Some(&SourceInfo { raw: true, ..Default::default() }), false);
            std::fs::remove_file(&x.file).unwrap();
            x.s.execute("library.smartPreviews", &json!({"id": copy.0, "discard": true})).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "an unselected sensor consumer still needs these pixels");
            assert!(x.offline_source().unwrap().info.unwrap().raw);
            x.s.execute("library.smartPreviews", &json!({"ids": [parent.0, copy.0], "discard": true})).unwrap();
            assert!(!path.exists(), "selecting every consumer permits explicit deletion");
            x.finish();
        }
    }
}

#[test]
fn proxy_operations_invalidate_fresh_background_availability() {
    for background in [false, true] {
        let mut x = Lib::new(&format!("cached-availability-{background}"), true);
        let photo = x.photo();
        let dir = x.smart_dir();
        let (wake, receive) = mpsc::channel();
        x.s.media.smart_availability.run_in_background(Arc::new(move || {
            let _ = wake.send(());
        }));
        let availability = x.s.media.smart_availability.clone();
        assert!(!availability.exists(&dir, &photo));
        for _ in 0..4 {
            receive.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(!availability.exists(&dir, &photo));
        for discard in [false, true] {
            x.s.execute("library.smartPreviews", &json!({"id": x.id.0, "background": background, "discard": discard})).unwrap();
            wait_build(&x.s);
            let deadline = Instant::now() + Duration::from_secs(5);
            while availability.exists(&dir, &photo) == discard {
                let remaining = deadline.checked_duration_since(Instant::now()).expect("availability changed after write/discard");
                receive.recv_timeout(remaining).unwrap();
            }
        }
        x.finish();
    }
}

#[test]
fn an_unremovable_old_candidate_does_not_block_an_interpretation_edit() {
    let mut x = Lib::new("unremovable-candidate", true);
    let dir = x.smart_dir();
    let photo = x.photo();
    let sensor = crate::smart::sharded_file_path(&dir, &photo);
    let sensor_bytes = write_proxy(&sensor, Some(&SourceInfo { raw: true, ..Default::default() }), false);
    let flat = dir.join(crate::smart::file_name(&photo));
    std::fs::create_dir_all(&flat).unwrap();
    assert!(std::fs::remove_file(&flat).is_err(), "this candidate cannot be invalidated as a file");
    let mut preview = (*photo).clone();
    preview.preview_only = Some("previous decoder".into());
    let preview = crate::smart::sharded_file_path(&dir, &preview);
    let preview_bytes = write_proxy(&preview, Some(&Default::default()), false);
    std::fs::remove_file(&x.file).unwrap();
    x.set_preview(true, true);
    assert!(x.photo().preview_only.is_some(), "classification edits do not depend on deleting proxy files");
    assert!(!x.offline_source().unwrap().info.unwrap().raw);
    x.s.execute("edit.undo", &json!({})).unwrap();
    assert!(x.offline_source().unwrap().info.unwrap().raw);
    assert_eq!(std::fs::read(&sensor).unwrap(), sensor_bytes);
    assert_eq!(std::fs::read(&preview).unwrap(), preview_bytes);
    assert!(flat.is_dir());
    x.finish();
}

#[test]
fn orf_decoder_upgrades_keep_edits_and_require_explicit_same_version_rechecking() {
    use crate::media::ProbeInfo;
    use std::sync::atomic::{AtomicBool, Ordering};
    for (case, command, auto) in [
        ("reload", "photo.reload", false),
        ("manual-recheck", "library.recheckPreviewOnly", false),
        ("stale-version", "library.recheckPreviewOnly", true),
    ] {
        let mut x = Lib::new(&format!("orf-upgrade-{case}"), false);
        let file = x.dir.join("source.orf");
        std::fs::rename(&x.file, &file).unwrap();
        x.file = file;
        x.s.catalog
            .apply(Op::Relink {
                id: x.id,
                file_name: "source.orf".into(),
                source: lightcraft_catalog::Source::File { path: x.file.to_string_lossy().into() },
                format: Some("ORF".into()),
                kind: None,
            })
            .unwrap();
        let original = std::fs::read(&x.file).unwrap();
        let photo = x.photo();
        let (width, height, size, hash) = (photo.width, photo.height, photo.file_size, photo.content_hash.clone());
        let supported = Arc::new(AtomicBool::new(false));
        let current = supported.clone();
        x.s.media.file_probe = Some(Arc::new(move |path| {
            std::fs::read(path).map_err(|e| e.to_string())?;
            Ok(ProbeInfo {
                width,
                height,
                file_size: size,
                content_hash: hash.clone(),
                format: "ORF".into(),
                kind: lightcraft_catalog::MediaKind::Raw,
                preview_only: (!current.load(Ordering::SeqCst)).then(|| "previous decoder".into()),
                ..Default::default()
            })
        }));
        let current = supported.clone();
        x.s.media.file_loader = Some(Arc::new(move |path, _, _| {
            std::fs::read(path).map_err(|e| e.to_string())?;
            let raw = current.load(Ordering::SeqCst);
            Ok((Rgb32f::filled(width as usize, height as usize, if raw { [0.2; 3] } else { [0.7; 3] }), SourceInfo { raw, ..Default::default() }))
        }));
        x.s.execute("develop.set", &json!({"id": x.id.0, "control": "light.exposure", "value": 0.4})).unwrap();
        let r = x.s.execute("photo.virtualCopy", &json!({"id": x.id.0})).unwrap();
        let copy = PhotoId(r["ids"][0].as_u64().unwrap());
        if auto {
            let p = x.photo();
            x.s.apply_system(Op::SetContent {
                id: p.id,
                width: p.width,
                height: p.height,
                file_size: p.file_size,
                content_hash: p.content_hash.clone(),
                preview_only: p.preview_only.clone(),
                preview_checked: "0.5.0".into(),
            })
            .unwrap();
        }
        let before = x.photo();
        let dir = x.smart_dir();
        assert_eq!(x.build()["built"], 1);
        let preview = crate::smart::sharded_file_path(&dir, &before);
        let preview_bytes = std::fs::read(&preview).unwrap();
        let legacy = old_path(&dir, &before, true);
        std::fs::write(&legacy, &preview_bytes).unwrap();
        supported.store(true, Ordering::SeqCst);
        if !auto {
            assert_eq!(x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap()["checked"], 0, "APP_VERSION alone controls automatic rechecks");
            assert!(x.photo().preview_only.is_some());
        }
        let params = if auto { json!({}) } else { json!({"ids": [x.id.0], "all": true}) };
        x.s.execute(command, &params).unwrap();
        let after = x.photo();
        assert!(after.develops_raw());
        assert_eq!(after.develop, before.develop);
        assert_eq!(after.meta, before.meta);
        assert_eq!(crate::media::content_key(&after), crate::media::content_key(&before));
        let sensor = crate::smart::sharded_file_path(&dir, &after);
        assert_ne!(sensor, preview);
        assert_eq!(sensor, crate::smart::sharded_file_path(&dir, x.s.catalog.photo(copy).unwrap()));
        std::fs::remove_file(&x.file).unwrap();
        assert!(x.offline_source().is_err(), "preview pixels cannot satisfy the new sensor interpretation");
        assert_eq!(x.build()["built"], 0);
        std::fs::write(&x.file, &original).unwrap();
        assert_eq!(x.s.execute("library.smartPreviews", &json!({"ids": [x.id.0, copy.0]})).unwrap()["built"], 2);
        std::fs::remove_file(&x.file).unwrap();
        assert!(x.offline_source().unwrap().info.unwrap().raw);
        assert_eq!(std::fs::read(&preview).unwrap(), preview_bytes);
        assert_eq!(std::fs::read(&legacy).unwrap(), preview_bytes);
        if command == "photo.reload" {
            x.s.execute("edit.undo", &json!({})).unwrap();
            assert_eq!(crate::smart::sharded_file_path(&dir, &x.photo()), preview);
            assert!(!x.offline_source().unwrap().info.unwrap().raw, "Undo retains its preview identity");
            x.s.execute("edit.redo", &json!({})).unwrap();
            assert!(x.offline_source().unwrap().info.unwrap().raw);
        }
        x.finish();
    }
}
