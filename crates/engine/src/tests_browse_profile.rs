//! Browsed (Local) raws render with their camera body's profile, worked out at render time
//! (OneSlips, LR-PROF-BODY): nothing stored, no sidecar, not edited; a body default set after the
//! folder was browsed shows at once; a photo given its own profile keeps it. Bodies come from the
//! serial number, the file's when the catalog has none, never from a renamed file's number.

use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::Session;
use crate::profile_bodies::{ImportProfile, body_from, file_body};

const BODY: &str = "Canon EOS R5 C|653499300171";

fn browsed(s: &mut Session, path: &str, serial: &str) -> PhotoId {
    let id = s.catalog.alloc_photo_id();
    let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy().to_string();
    let mut p = Photo::new(id, Source::File { path: path.into() }, &name, "CR3", 100, 100, "2026-01-01");
    p.kind = MediaKind::Raw;
    p.meta.camera = "Canon EOS R5 C".into();
    p.meta.camera_model = "Canon EOS R5 C".into();
    p.meta.camera_serial = serial.into();
    p.local = true;
    p.set_local_baseline();
    s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn rendered_camera(s: &mut Session, id: PhotoId) -> String {
    s.render_job(id, 64, 64, false, true).unwrap().settings.profile.camera.clone()
}

#[test]
fn a_browsed_raw_renders_with_its_body_profile_without_being_edited() {
    let dir = std::env::temp_dir().join(format!("cf-browse-profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("IMG_0001.CR3");
    let mut s = Session::new();
    s.body_profiles.insert(BODY.into(), "lc".into());
    let id = browsed(&mut s, &path.to_string_lossy(), "653499300171");
    let before = s.catalog.photo(id).unwrap().clone();
    assert_eq!(rendered_camera(&mut s, id), "lc", "the body's profile");
    for job in [s.thumb_job(id, 256).unwrap(), s.loupe_job(id, 64, 64, true).unwrap()] {
        assert_eq!(job.settings.profile.camera, "lc");
    }
    assert_eq!(s.export_job(id, 64, 64, Default::default(), Default::default()).unwrap().settings.profile.camera, "lc");
    // nothing stored: the record, its edit state and its Local baseline are as browsed
    let after = s.catalog.photo(id).unwrap();
    assert_eq!(after.develop.profile.camera, "");
    assert!(after.edited.is_none() && !after.is_edited() && after.untouched_local());
    assert!(std::sync::Arc::ptr_eq(&before, after), "the catalog record is untouched");
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "no sidecar written");
    // the same settings every time, so render caches keep working
    let (a, b) = (s.render_job(id, 64, 64, false, true).unwrap(), s.render_job(id, 64, 64, false, true).unwrap());
    assert!(std::sync::Arc::ptr_eq(&a.settings, &b.settings) && a.key == b.key);
    // an imported photo keeps what it has
    let mut p = (**s.catalog.photo(id).unwrap()).clone();
    let lib = s.catalog.alloc_photo_id();
    (p.id, p.local, p.source) = (lib, false, Source::File { path: dir.join("IMG_0009.CR3").to_string_lossy().into() });
    s.commit("Import", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    assert_eq!(rendered_camera(&mut s, lib), "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_body_default_set_after_browsing_applies_to_photos_already_browsed() {
    let mut s = Session::new();
    let id = browsed(&mut s, "/nonexistent/IMG_0002.CR3", "653499300171");
    let other = browsed(&mut s, "/nonexistent/IMG_0003.CR3", "653499300174");
    assert_eq!(rendered_camera(&mut s, id), "", "no default yet: the camera's");
    let first = s.render_job(id, 64, 64, false, true).unwrap().key;
    s.execute("profile.setBodyDefault", &json!({"id": "lc", "photo": id.0})).unwrap();
    assert_eq!(s.body_profiles.get(BODY).map(String::as_str), Some("lc"));
    let job = s.render_job(id, 64, 64, false, true).unwrap();
    assert_eq!(job.settings.profile.camera, "lc");
    assert_ne!(job.key, first, "it renders again");
    assert_eq!(rendered_camera(&mut s, other), "", "the other body keeps the camera default");
    assert!(s.catalog.photo(id).unwrap().edited.is_none());
    // cleared again: back to the camera's default
    s.execute("profile.setBodyDefault", &json!({"id": "", "body": BODY})).unwrap();
    assert_eq!(rendered_camera(&mut s, id), "");
}

#[test]
fn the_import_panel_choice_previews_on_browsed_photos_as_the_import_applies_it() {
    let mut s = Session::new();
    s.body_profiles.insert(BODY.into(), "lc".into());
    let id = browsed(&mut s, "/nonexistent/IMG_0005.CR3", "653499300171");
    // Camera default chosen for this body in the Import panel
    s.import_profile_choice = ImportProfile::PerBody([(BODY.to_string(), String::new())].into());
    assert_eq!(rendered_camera(&mut s, id), "");
    s.import_profile_choice = ImportProfile::PerBody(Default::default());
    assert_eq!(rendered_camera(&mut s, id), "lc");
    // added to the library as shown, the record now holds it
    s.execute("photo.addToLibrary", &json!({"ids": [id.0]})).unwrap();
    let p = s.catalog.photo(id).unwrap();
    assert!(!p.local);
    assert_eq!(p.develop.profile.camera, "lc");
    assert_eq!(rendered_camera(&mut s, id), "lc");

    // a browsed photo given its own profile keeps it: the user picks the camera's default for
    // this photo: an edit, kept over the body's
    let own = browsed(&mut s, "/nonexistent/IMG_0004.CR3", "653499300171");
    let mut d = (*s.develop_of(own).unwrap()).clone();
    d.profile.camera = String::new();
    d.light.exposure = 0.5;
    s.set_develop(own, d, "Profile").unwrap();
    assert!(s.catalog.photo(own).unwrap().edited.is_some());
    assert_eq!(rendered_camera(&mut s, own), "", "its own profile");

    // A browsed raw shown with its body's profile is still as shot (`import::looks_as_shot`):
    // culling may show its camera JPEG, and the photo as rendered agrees with the record.
    let shot = browsed(&mut s, "/nowhere/IMG_0002.CR3", "653499300171");
    let settings = std::sync::Arc::new(s.catalog.photo(shot).unwrap().camera_defaults());
    s.commit("As shot", Op::SetDevelop { id: shot, settings, label: "As shot".into(), edited: None }).unwrap();
    let shown = s.rendered_photo_of(shot).unwrap();
    assert_eq!(shown.develop.profile.camera, "lc", "rendered with the body's profile");
    assert!(crate::import::looks_as_shot(s.catalog.photo(shot).unwrap()), "the record is as shot");
    assert!(crate::import::looks_as_shot(&shown), "and so is the photo as rendered");
}

#[test]
fn file_name_bodies_are_camera_names_never_a_renamed_files_number() {
    for (name, body) in [
        ("_SA94454.ARW", Some("SA")),
        ("IMG_0001.CR3", Some("IM")),
        ("_74_0338.CR3", Some("74")),
        ("_71_2012.CR3", Some("71")),
        ("10-B+B-0042.CR3", None),
        ("11-B+B-0043.CR3", None),
        ("1-B+B-0001.CR3", None),
        ("20_ceremony.CR3", None),
        ("07 vows.CR3", None),
        ("A", None),
        ("", None),
        ("__", None),
        ("éé0001.CR3", None),
    ] {
        assert_eq!(file_body(name).as_deref(), body, "{name}");
    }
    assert_eq!(body_from("653499300171", "10-B+B-0042.CR3").as_deref(), Some("653499300171"), "a serial wins");
    assert_eq!(body_from(" ", "10-B+B-0042.CR3"), None);
}

#[test]
fn a_raw_catalogued_without_a_serial_is_known_by_its_files() {
    use lcc_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t};
    let dir = std::env::temp_dir().join(format!("cf-browse-serial-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("10-B+B-0042.CR3");
    let ifd = IfdBuilder::new()
        .with(t::MAKE, Value::Ascii("Canon".into()))
        .with(t::MODEL, Value::Ascii("Canon EOS R5 C".into()))
        .with(t::CAMERA_SERIAL_NUMBER, Value::Ascii("653499300171".into()));
    std::fs::write(&path, TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap()).unwrap();
    let mut s = Session::new();
    s.body_profiles.insert(BODY.into(), "lc".into());
    // browsed before the app kept serials: none in the catalog, and the name names no body
    let id = browsed(&mut s, &path.to_string_lossy(), "");
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!(crate::profile_bodies::key_of(&p), None);
    assert_eq!(s.body_key(&p, true).as_deref(), Some(BODY), "the file's serial");
    assert_eq!(rendered_camera(&mut s, id), "lc");
    let bodies = s.execute("profiles.bodies", &json!({})).unwrap();
    assert!(bodies["bodies"].as_array().unwrap().iter().any(|b| b["body"] == BODY && b["photos"] == 1), "{bodies}");
    // a render reads it in the background: a fresh session gets it once the read lands
    let mut fresh = Session::new();
    fresh.body_profiles.insert(BODY.into(), "lc".into());
    let id2 = browsed(&mut fresh, &path.to_string_lossy(), "");
    let p2 = fresh.catalog.photo(id2).unwrap().clone();
    let mut known = fresh.body_key(&p2, false);
    for _ in 0..200 {
        if known.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        known = fresh.body_key(&p2, false);
    }
    assert_eq!(known.as_deref(), Some(BODY));
    // imported: the serial is kept in the catalog from now on
    let opts = crate::import::ImportOptions { mode: crate::import::ImportMode::Add, ..Default::default() };
    let r = crate::import::import_with(&mut s, &[path.to_string_lossy().to_string()], &opts).unwrap();
    assert_eq!(r.imported, [id.0], "{r:?}");
    let p = s.catalog.photo(id).unwrap();
    assert!(!p.local);
    assert_eq!((p.meta.camera_serial.as_str(), p.develop.profile.camera.as_str()), ("653499300171", "lc"));
    // a file without one, or none at all: no body, no panic
    std::fs::write(dir.join("x.CR3"), b"not a raw").unwrap();
    assert_eq!(crate::profile_bodies::read_serial(&dir.join("x.CR3").to_string_lossy()), None);
    assert_eq!(crate::profile_bodies::read_serial(&dir.join("gone.CR3").to_string_lossy()), None);
    let _ = std::fs::remove_dir_all(&dir);
}
