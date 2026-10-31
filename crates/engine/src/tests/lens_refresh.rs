//! Lens profiles stay per lens. Presets, Copy / Paste and Sync carry "Enable Lens Corrections" and
//! its amounts, never a profile: each photo keeps the one found for its own lens. And photos imported before the lens
//! database was there get their profile when it arrives (`cmd::lens_refresh`).

use std::sync::Arc;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_develop::{EmbeddedLens, EmbeddedWarp, LensSource};
use serde_json::json;

use crate::Session;

fn lens(k: f64) -> EmbeddedLens {
    EmbeddedLens {
        warp: Some(EmbeddedWarp { planes: [[1.0, k, 0.0, 0.0, 0.0, 0.0]; 3], radius: 0.6, ..Default::default() }),
        source: LensSource::Database,
        ..Default::default()
    }
}

fn lens_of(s: &Session, id: PhotoId) -> Option<EmbeddedLens> {
    s.catalog.photo(id).and_then(|p| p.embedded_lens)
}

/// Two demo photos "taken with different lenses".
fn two_lenses() -> (Session, PhotoId, PhotoId) {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible().iter().copied().take(2).collect();
    let (a, b) = (ids[0], ids[1]);
    s.catalog.apply(Op::SetEmbeddedLens { id: a, lens: Some(Box::new(lens(-0.05))) }).unwrap();
    s.catalog.apply(Op::SetEmbeddedLens { id: b, lens: Some(Box::new(lens(0.03))) }).unwrap();
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"optics": {"lens_profile": true}}, "label": "Lens Corrections"})).unwrap();
    s.execute("develop.set", &json!({"values": {"optics.profileDistortion": 60}})).unwrap();
    (s, a, b)
}

fn carried(s: &Session, id: PhotoId) {
    let d = s.develop_of(id).unwrap();
    assert!(d.optics.lens_profile, "the switch travels");
    assert_eq!(d.optics.profile_distortion, 60.0, "the amount travels");
}

#[test]
fn copy_paste_carries_the_switch_and_amounts_never_the_profile() {
    let (mut s, a, b) = two_lenses();
    s.execute("develop.copy", &json!({})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    carried(&s, b);
    assert_eq!(lens_of(&s, b), Some(lens(0.03)), "b keeps its own lens's profile");
    assert_eq!(lens_of(&s, a), Some(lens(-0.05)));
}

#[test]
fn sync_and_presets_keep_each_photos_own_profile() {
    let (mut s, a, b) = two_lenses();
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": a.0})).unwrap();
    s.execute("develop.sync", &json!({})).unwrap();
    carried(&s, b);
    assert_eq!(lens_of(&s, b), Some(lens(0.03)));
    // a preset made on a's settings, applied to b
    let pid = s.execute("preset.create", &json!({"name": "Optics", "groups": ["optics"]})).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"optics": {"lens_profile": false}}, "label": "Lens Corrections"})).unwrap();
    s.execute("preset.apply", &json!({"id": pid})).unwrap();
    carried(&s, b);
    assert_eq!(lens_of(&s, b), Some(lens(0.03)), "a preset never brings another lens's profile");
    assert_eq!(lens_of(&s, a), Some(lens(-0.05)));
}

/// A batch of results gives each probed photo's profile to that photo and to its own virtual copies, never to the
/// copies of a photo that wasn't probed.
#[test]
fn each_result_reaches_its_photos_virtual_copies_and_no_others() {
    let mut s = Session::new();
    for p in [raw(1, "50mm F1.8", false), raw(2, "50mm F1.8", false), raw(3, "50mm F1.8", false)] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    for (id, of) in [(11, 1), (12, 1), (21, 2), (31, 3)] {
        let mut copy = raw(id, "50mm F1.8", false);
        copy.copy_of = Some(PhotoId(of));
        s.catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    }
    let probed = [1, 2].map(|id| {
        let m = crate::cmd::lens_refresh::Missing { id: PhotoId(id), path: format!("/photos/{id}.cr3") };
        (m, Ok(crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() }))
    });
    let mut found = crate::cmd::lens_refresh::apply(&mut s, probed.into()).unwrap();
    found.sort();
    assert_eq!(found, [1, 2, 11, 12, 21].map(PhotoId), "each probed photo and its own copies");
    for id in [1, 2, 11, 12, 21] {
        assert!(s.catalog.photo(PhotoId(id)).unwrap().embedded_lens.is_some(), "photo {id} gets the profile");
    }
    for id in [3, 31] {
        assert!(s.catalog.photo(PhotoId(id)).unwrap().embedded_lens.is_none(), "photo {id} wasn't probed: unchanged");
    }
}

/// A raw in a library folder, as an older import left it: no lens data.
fn raw(id: u64, lens_name: &str, edited: bool) -> Photo {
    let mut p =
        Photo::new(PhotoId(id), Source::File { path: format!("/photos/{id}.cr3") }, &format!("{id}.cr3"), "CR3", 600, 400, "2026-10-01T00:00:00");
    p.kind = MediaKind::Raw;
    p.meta.camera = "Testcam TC-1".into();
    p.meta.lens = lens_name.into();
    if edited {
        let mut d = (*p.develop).clone();
        d.light.exposure = 0.5;
        p.develop = Arc::new(d);
        p.edited = Some("2026-10-02T00:00:00".into());
    }
    p
}

const DB: &str = r#"<lensdatabase>
  <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1</cropfactor></camera>
  <lens><maker>Testcam</maker><model>Testcam 50mm f/1.8</model><mount>T</mount><cropfactor>1</cropfactor>
    <calibration><distortion model="poly3" focal="50" k1="-0.01" /></calibration></lens></lensdatabase>"#;

/// When the database arrives, the library's raws whose lens it knows get their profile; untouched photos get the
/// correction switched on as import would, edited ones keep their settings; lenses it doesn't know are not probed.
#[test]
fn photos_get_their_profile_when_the_database_arrives() {
    let mut s = Session::new();
    for p in [raw(1, "50mm F1.8", false), raw(2, "50mm F1.8", true), raw(3, "300mm F4", false), raw(4, "", false)] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let db = lightcraft_lensdb::LensDb::parse(DB).unwrap();
    let listed = crate::cmd::lens_refresh::missing(&s.catalog, &db, None, false);
    assert_eq!(listed.iter().map(|m| m.id.0).collect::<Vec<_>>(), [1, 2], "only lenses the database knows");
    assert_eq!(crate::cmd::lens_refresh::missing(&s.catalog, &db, Some(&[PhotoId(2)]), false).len(), 1, "or only the photos asked for");
    // what the files give when read again
    let results = listed.into_iter().map(|m| (m, Ok(crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() }))).collect();
    let found = crate::cmd::lens_refresh::apply(&mut s, results).unwrap();
    assert_eq!(found, [PhotoId(1), PhotoId(2)]);
    let p = |id: u64| s.catalog.photo(PhotoId(id)).unwrap().clone();
    assert!(p(1).embedded_lens.is_some() && p(1).develop.optics.lens_profile, "untouched: on, as import would");
    assert!(p(2).embedded_lens.is_some() && !p(2).develop.optics.lens_profile, "edited: its settings stay");
    assert!(p(3).embedded_lens.is_none());
    assert!(s.undo.is_empty(), "bookkeeping, not an undo step");
    // the Optics panel's words
    assert_eq!(crate::cmd::lens_refresh::no_profile_reason(&p(3)).as_deref(), Some("No lens profile found for 300mm F4."));
    assert_eq!(crate::cmd::lens_refresh::no_profile_reason(&p(4)).as_deref(), Some("This file names no lens."));
    assert_eq!(crate::cmd::lens_refresh::no_profile_reason(&p(1)), None);
    // a result read from a file the photo no longer has (moved meanwhile) is dropped, for its virtual copies too
    let mut copy = raw(5, "300mm F4", false);
    copy.copy_of = Some(PhotoId(3));
    s.catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    let stale = crate::cmd::lens_refresh::Missing { id: PhotoId(3), path: "/elsewhere/3.cr3".into() };
    let ok = crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() };
    assert!(crate::cmd::lens_refresh::apply(&mut s, vec![(stale, Ok(ok))]).unwrap().is_empty());
    assert!(s.catalog.photo(PhotoId(3)).unwrap().embedded_lens.is_none(), "a stale probe changes nothing");
    assert!(s.catalog.photo(PhotoId(5)).unwrap().embedded_lens.is_none(), "nor the copies");
    // without file access the command says so instead of clearing anything
    assert!(s.execute("library.refreshLensProfiles", &json!({})).is_err());
    assert!(s.execute("library.refreshLensProfiles", &json!({"ids": "x"})).is_err());
}

fn with_lens(mut p: Photo, l: Option<EmbeddedLens>, on: bool) -> Photo {
    p.embedded_lens = l;
    let mut d = (*p.develop).clone();
    d.optics.lens_profile = on;
    p.develop = Arc::new(d);
    p
}

/// When the database's files were replaced, the photos whose profile came from the old database are read again
/// (they get the new profile, or lose one the database no longer gives), and so are the raws that named no lens when
/// they were imported. A profile from the file itself is left alone, and none of these are read at an ordinary start.
#[test]
fn a_replaced_database_looks_up_database_profiles_and_unnamed_photos_again() {
    let mut s = Session::new();
    let file_lens = EmbeddedLens { source: LensSource::File, ..lens(-0.04) };
    for p in [
        with_lens(raw(1, "50mm F1.8", false), Some(lens(-0.01)), true),
        with_lens(raw(2, "50mm F1.8", false), Some(file_lens), true),
        raw(3, "", false),
        with_lens(raw(4, "300mm F4", false), Some(lens(-0.02)), true),
    ] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let db = lightcraft_lensdb::LensDb::parse(DB).unwrap();
    let ids = |v: Vec<crate::cmd::lens_refresh::Missing>| v.iter().map(|m| m.id.0).collect::<Vec<_>>();
    assert!(crate::cmd::lens_refresh::missing(&s.catalog, &db, None, false).is_empty(), "an ordinary start reads none of them");
    let listed = crate::cmd::lens_refresh::missing(&s.catalog, &db, None, true);
    assert_eq!(ids(listed.clone()), [1, 3, 4], "database profiles and unnamed raws, never the file's own correction");
    // what the files give with the new database: 1 another profile, 3 its lens (now read) and a profile, 4 nothing
    let gives = |id: u64| match id {
        1 => crate::media::ProbeInfo { embedded_lens: Some(lens(-0.03)), ..Default::default() },
        3 => {
            let mut info = crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() };
            info.meta.lens = "50mm F1.8".into();
            info
        }
        _ => crate::media::ProbeInfo::default(),
    };
    let results = listed.into_iter().map(|m| (m.clone(), Ok(gives(m.id.0)))).collect();
    let found = crate::cmd::lens_refresh::apply(&mut s, results).unwrap();
    assert_eq!(found, [PhotoId(1), PhotoId(3)]);
    assert_eq!(lens_of(&s, PhotoId(1)), Some(lens(-0.03)), "the new database's profile");
    assert_eq!(lens_of(&s, PhotoId(2)), Some(file_lens), "the file's own correction stays");
    assert_eq!(lens_of(&s, PhotoId(3)), Some(lens(-0.01)));
    assert_eq!(s.catalog.photo(PhotoId(3)).unwrap().meta.lens, "50mm F1.8", "the lens's name, read now");
    assert_eq!(lens_of(&s, PhotoId(4)), None, "a profile the database no longer gives is gone");
    assert!(s.undo.is_empty(), "bookkeeping, not an undo step");
}

/// Every render of a photo uses the one database profile the catalog holds for it: a fresh decode that looked the lens
/// up in a newer database doesn't override it (a cached preview and a fresh render agree), while the file's own
/// correction, part of the file, still comes from the decode.
#[test]
fn a_decode_never_overrides_the_photos_database_profile() {
    use crate::media::DecodedSource;
    let img = Arc::new(lightcraft_raster::Rgb32f::new(4, 4));
    let decoded = |l: EmbeddedLens| DecodedSource::new(img.clone(), Some(lightcraft_pipeline::SourceInfo { lens: Some(l), ..Default::default() }));
    let header = |l: Option<EmbeddedLens>| lightcraft_pipeline::SourceInfo { lens: l, ..Default::default() };
    assert_eq!(decoded(lens(-0.05)).info_or(header(Some(lens(-0.01)))).lens, Some(lens(-0.01)), "the catalog's profile");
    assert_eq!(decoded(lens(-0.05)).info_or(header(None)).lens, None, "none until the photo is given one");
    let file_lens = EmbeddedLens { source: LensSource::File, ..lens(-0.04) };
    assert_eq!(decoded(file_lens).info_or(header(None)).lens, Some(file_lens), "the file's own correction");
}

/// When a photo's database profile changes (a replaced database looked up again), its render key and its thumbnail
/// and variant cache keys change, so no cached render made with the old profile is shown; with the correction off
/// the profile changes nothing, and the keys stay.
#[test]
fn a_new_database_profile_changes_the_render_and_cache_keys() {
    let mut s = Session::new();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(with_lens(raw(1, "50mm F1.8", false), Some(lens(-0.01)), true)) }).unwrap();
    let id = PhotoId(1);
    let keys = |s: &mut Session| {
        let render = s.render_job(id, 800, 600, false, true).unwrap().key;
        let thumb = s.thumb_job(id, 256).unwrap().cache.unwrap().1;
        let p = s.catalog.photo(id).unwrap().clone();
        (render, thumb, Session::variant_key(&p, &p.develop, 128))
    };
    let before = keys(&mut s);
    s.catalog.apply(Op::SetEmbeddedLens { id, lens: Some(Box::new(lens(-0.03))) }).unwrap();
    let after = keys(&mut s);
    assert_ne!(before.0, after.0, "render key");
    assert_ne!(before.1, after.1, "thumbnail cache key");
    assert_ne!(before.2, after.2, "variant cache key");
    // correction off: the profile isn't used, so changing it leaves every key (and the cached thumbnails) as they are
    let off = with_lens((**s.catalog.photo(id).unwrap()).clone(), Some(lens(-0.03)), false);
    s.catalog.apply(Op::SetDevelop { id, settings: off.develop.clone(), label: "Lens Corrections".into(), edited: None }).unwrap();
    let off_before = keys(&mut s);
    s.catalog.apply(Op::SetEmbeddedLens { id, lens: Some(Box::new(lens(-0.05))) }).unwrap();
    assert_eq!(off_before, keys(&mut s));
}

/// `lens.downloadDatabase` selects the folder it downloads into, so the session reads the database once it is in
/// place (a headless session would otherwise download it and go on without it); and the app's own folder is read
/// only when it is whole (its marker, written last, is there).
#[test]
fn the_downloaded_database_is_selected_and_read_only_when_whole() {
    use crate::{lens_profiles, lensfun_download};
    if std::env::var_os(lens_profiles::ENV).is_some_and(|v| !v.is_empty()) {
        lightcraft_testkit::skip!(Input, "LIGHTCRAFT_LENSFUN_DIR names the folder in use");
        return;
    }
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("lc-lens-select-{}-{unique}", std::process::id()));
    let dir = root.join("lensfun");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lenses.xml"), DB).unwrap();
    let before = lens_profiles::host_dir();
    let mut s = Session::new();
    // nothing listens on port 1: the download fails at once, without the network
    crate::cmd::lens_refresh::start_download(&mut s, &dir, vec!["http://127.0.0.1:1/db".into()], lensfun_download::FILES).unwrap();
    assert_eq!(lens_profiles::dir().as_deref(), Some(dir.as_path()), "the session reads the folder it downloads into");
    assert!(lens_profiles::database().is_none(), "no marker: a download that never finished is not read");
    std::fs::write(dir.join(lensfun_download::MARKER), "whole").unwrap();
    lens_profiles::reload();
    assert_eq!(lens_profiles::database().map(|db| db.lenses.len()), Some(1), "whole: read");
    let t0 = std::time::Instant::now();
    while s.lensfun.running() && t0.elapsed() < std::time::Duration::from_secs(60) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    lens_profiles::use_dir(before);
    let _ = std::fs::remove_dir_all(&root);
}
