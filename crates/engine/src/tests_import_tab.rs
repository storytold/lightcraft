//! The Import tab's engine half (OneSlips LR-IMP-WINDOW, LR-IMP-NODELETE): importing a browsed
//! folder's picks, selection or all with a preset stack, records following copied files, and
//! browsed photos' files never deleted, renamed or moved.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-import-tab-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(p: &Path, seed: u8) {
    let img = lcc_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, seed, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::write(p, b).unwrap();
}

fn browsed(tag: &str, n: u8) -> (Session, PathBuf) {
    let dir = temp_dir(tag);
    for i in 0..n {
        png(&dir.join(format!("p{i}.png")), i.wrapping_mul(40));
    }
    let mut s = Session::new().with_fs();
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    (s, dir)
}

fn id_of(s: &Session, name: &str) -> lcc_catalog::PhotoId {
    s.catalog.photos().find(|p| p.file_name == name).map(|p| p.id).unwrap_or_else(|| panic!("no {name}"))
}

fn recipe_preset(s: &mut Session, name: &str, control: &str, value: f64) -> String {
    let r = s.execute("preset.saveRecipe", &json!({"name": name, "recipe": [{"id": control, "value": value}]})).unwrap();
    r["id"].as_str().unwrap().to_string()
}

#[test]
fn import_targets_count_picks_selection_and_all() {
    let (mut s, dir) = browsed("targets", 3);
    let a = id_of(&s, "p0.png");
    s.execute("photo.flag", &json!({"ids": [a.0], "flag": "pick"})).unwrap();
    let t = s.execute("import.targets", &json!({"what": "picks"})).unwrap();
    assert_eq!((t["picks"].as_u64(), t["all"].as_u64()), (Some(1), Some(3)), "{t}");
    assert_eq!(t["paths"].as_array().map(Vec::len), Some(1), "{t}");
    assert!(s.execute("import.targets", &json!({"what": "nope"})).is_err());
    // nothing to take outside a browsed folder
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert!(s.execute("import.run", &json!({"what": "all"})).is_err(), "only a browsed folder imports from the tab");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn importing_picks_in_place_applies_the_preset_stack_and_keeps_culling() {
    let (mut s, dir) = browsed("stack", 3);
    let a = id_of(&s, "p0.png");
    let b = id_of(&s, "p1.png");
    let p1 = recipe_preset(&mut s, "Bright", "light.exposure", 0.5);
    let p2 = recipe_preset(&mut s, "Contrast", "light.contrast", 20.0);
    s.execute("photo.flag", &json!({"ids": [a.0], "flag": "pick"})).unwrap();
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 4})).unwrap();
    let before = s.catalog.photos().count();
    let r = s.execute("import.run", &json!({"what": "picks", "presets": [p1, p2], "keywords": ["trip"]})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    let pa = s.catalog.photo(a).unwrap();
    assert!(!pa.local, "the pick joined the library");
    assert_eq!(s.catalog.photos().count(), before, "as itself, not a second record");
    assert_eq!(pa.rating, 4, "its culling went along");
    assert!((pa.develop.light.exposure - 0.5).abs() < 1e-6 && (pa.develop.light.contrast - 20.0).abs() < 1e-6, "both presets, in order");
    assert!(pa.history.iter().filter(|h| h.label.starts_with("Preset: ")).count() >= 2, "one History step per preset");
    assert!(pa.meta.keywords.iter().any(|k| k == "trip"));
    assert!(s.catalog.photo(b).unwrap().local, "an unpicked photo stays browsed");
    assert!(s.execute("import.run", &json!({"what": "picks"})).is_err(), "no picks left to import");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_copied_browsed_photo_follows_its_file_and_the_original_stays() {
    let (mut s, dir) = browsed("copy", 2);
    let dest = temp_dir("copy-dest");
    let a = id_of(&s, "p0.png");
    s.execute("photo.flag", &json!({"ids": [a.0], "flag": "pick"})).unwrap();
    s.execute("photo.label", &json!({"ids": [a.0], "label": "red"})).unwrap();
    let before = s.catalog.photos().count();
    let r = s.execute("import.run", &json!({"what": "picks", "mode": "copy", "destination": dest.to_string_lossy(), "organize": "flat"})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    let p = s.catalog.photo(a).unwrap();
    assert!(!p.local);
    let lcc_catalog::Source::File { path } = &p.source else { panic!("a file") };
    assert!(Path::new(path).starts_with(&dest), "the record follows the copy: {path}");
    assert_eq!(p.flag, lcc_catalog::Flag::Pick, "with its culling");
    assert!(p.label.is_some());
    assert_eq!(s.catalog.photos().count(), before, "no second record");
    assert!(dir.join("p0.png").is_file(), "a copy leaves the browsed file where it was");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn browsed_photos_are_never_deleted_renamed_or_moved() {
    let (mut s, dir) = browsed("nodelete", 2);
    let a = id_of(&s, "p0.png");
    s.execute("photo.flag", &json!({"ids": [a.0], "flag": "reject"})).unwrap();
    assert!(s.execute("photo.delete", &json!({"ids": [a.0]})).is_err());
    assert!(s.execute("photo.deletePermanently", &json!({"ids": [a.0]})).is_err());
    assert!(s.execute("photo.deleteFromDisk", &json!({"ids": [a.0], "confirm": true})).is_err());
    assert!(s.execute("photo.rename", &json!({"ids": [a.0], "template": "x-{seq}"})).is_err());
    let r = s.execute("library.deleteRejected", &json!({"files": "recycle", "confirm": true})).unwrap();
    assert_eq!(r["deleted"], 0, "browsed rejects are left alone: {r}");
    let p = s.catalog.photo(a).unwrap();
    assert!(p.local && !p.deleted, "still browsed");
    assert!(dir.join("p0.png").is_file() && dir.join("p1.png").is_file(), "every file is where it was");
    // forgetting the folder never touches files either
    let _ = s.execute("library.forgetLocal", &json!({"days": 0}));
    assert!(dir.join("p0.png").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_presets_are_kept_and_removed_by_name() {
    let mut s = Session::new();
    assert!(s.execute("import.savePreset", &json!({"name": " ", "settings": {}})).is_err());
    assert!(s.execute("import.savePreset", &json!({"name": "Trip", "settings": 3})).is_err());
    s.execute("import.savePreset", &json!({"name": "Trip", "settings": {"mode": "copy"}})).unwrap();
    s.execute("import.savePreset", &json!({"name": "trip", "settings": {"mode": "add"}})).unwrap();
    let v = s.execute("import.presets", &json!({})).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(1), "one name, replaced: {v}");
    assert_eq!(v[0]["settings"]["mode"], "add");
    s.execute("import.deletePreset", &json!({"name": "TRIP"})).unwrap();
    assert!(s.execute("import.deletePreset", &json!({"name": "Trip"})).is_err());
}

#[test]
fn finishing_starts_at_the_photo_on_screen_and_notes_a_missing_model() {
    let (mut s, dir) = browsed("finish", 5);
    let ids: Vec<u64> = s.visible_cloned().iter().map(|i| i.0).collect();
    assert_eq!(ids.len(), 5);
    s.execute("library.select", &json!({"ids": [ids[3]], "active": ids[3]})).unwrap();
    let order = s.screen_outward_order(&ids);
    assert_eq!(order.first(), Some(&ids[3]), "the photo on screen first: {order:?}");
    let near: Vec<u64> = order.iter().skip(1).take(2).copied().collect();
    assert!(near.contains(&ids[2]) && near.contains(&ids[4]), "then its neighbours both ways: {order:?}");
    assert!(s.execute("library.finishImport", &json!({"ids": [ids[0]], "previews": "huge"})).is_err());
    let r = s.execute("library.finishImport", &json!({"ids": [ids[0]], "previews": "none"})).unwrap();
    assert_eq!(r["running"], false);
    let _ = std::fs::remove_dir_all(&dir);

    // Finishing also gets the photos' Smooth Skin (and AI masks) going; a model that is not there is
    // a note in the answer, not a failed finish.
    let (mut s, dir) = browsed("finish-ai", 2);
    s.segmenter.models_root = Some(dir.join("no-models-here"));
    let ids: Vec<u64> = s.visible_cloned().iter().map(|i| i.0).collect();
    let id = lcc_catalog::PhotoId(ids[0]);
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.skin = lcc_develop::SkinSmoothing::SUBTLE;
    s.set_develop(id, d, "Smooth Skin").unwrap();
    let r = s.execute("library.finishImport", &json!({"ids": ids, "previews": "none"})).unwrap();
    assert_eq!(r["running"], false, "{r}");
    assert_eq!(r["skin"], 0, "{r}");
    assert_eq!(r["aiMasks"], 0, "{r}");
    if crate::segment::Segmenter::AVAILABLE {
        assert!(r["notes"].as_array().is_some_and(|n| n.len() == 1), "one note for the missing model, not one per photo: {r}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn kept_plan(s: &mut Session) -> Option<crate::finishing::FinishPlan> {
    let bytes = s.library.as_mut()?.files.read(crate::finishing::FILE).ok()??;
    serde_json::from_slice(&bytes).ok()
}

fn wait_for(s: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !done(s) {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// An import quit part-way through finishing (its previews not all built) is kept in the library
/// and taken up when the library opens again; once done it is forgotten (ported from OneSlips'
/// LR-IMP-WINDOW). A damaged plan file is left alone, never a failed open.
#[test]
fn an_unfinished_import_is_taken_up_after_a_restart() {
    let dir = temp_dir("finish-restart");
    let (lib, photos) = (dir.join("lib"), dir.join("photos"));
    std::fs::create_dir_all(&photos).unwrap();
    for i in 0..3u8 {
        png(&photos.join(format!("p{i}.png")), i.wrapping_mul(60));
    }
    let ids: Vec<u64> = {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("library.import", &json!({"paths": [photos.to_string_lossy()]})).unwrap();
        let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().filter_map(serde_json::Value::as_u64).collect();
        assert_eq!(ids.len(), 3, "{r}");
        s.execute("library.finishImport", &json!({"ids": ids, "previews": "standard"})).unwrap();
        let plan = kept_plan(&mut s).expect("the finish is kept in the library");
        assert_eq!((plan.ids.len(), plan.previews.as_str()), (3, "standard"), "{plan:?}");
        // the app quits before its frame loop saw the finish end
        wait_for(&mut s, "the preview build", |s| s.preview_build.as_ref().is_some_and(|b| b.finished.load(std::sync::atomic::Ordering::Relaxed)));
        s.close_library().unwrap();
        ids
    };
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let mut kept = s.finish_pending.as_ref().map(|p| p.ids.clone()).expect("taken up after the restart");
        kept.sort_unstable();
        let mut want = ids.clone();
        want.sort_unstable();
        assert_eq!(kept, want);
        // taken up: its preview build starts again (which may be done at once: the previews built
        // before the quit are there, and a build skips them)
        let _at_work = s.finishing_tick();
        assert!(s.finish_pending.is_none(), "the finish is taken up again");
        assert!(s.preview_build.is_some(), "its previews are being built");
        wait_for(&mut s, "the finish", |s| !s.finishing_tick());
        assert!(kept_plan(&mut s).is_some_and(|p| p.ids.is_empty()), "done: forgotten");
        s.close_library().unwrap();
    }
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        assert!(s.finish_pending.is_none(), "nothing left to take up");
        s.library.as_mut().unwrap().files.write_atomic(crate::finishing::FILE, b"{not json").unwrap();
        s.close_library().unwrap();
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(s.finish_pending.is_none() && !s.finishing_tick(), "a damaged plan is no plan");
    assert_eq!(s.library.as_mut().unwrap().files.read(crate::finishing::FILE).unwrap().as_deref(), Some(&b"{not json"[..]), "and left alone");
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
