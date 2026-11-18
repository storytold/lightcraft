//! Settings ▸ Lens profiles: the folder setting (saved, applied at once), downloading lensfun's database from a local
//! server (good, 404, truncated, not an archive), removing it, and what Develop ▸ Optics is told about a photo.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use std::sync::PoisonError;

use serde_json::{Value, json};

use crate::Session;
use crate::lens_download::tests::{DB, archive, gzip};
use crate::lens_profiles::TEST_LOCK;

/// The test's folder in Temp, removed when the test ends.
fn scratch(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lens-db-{name}"))
}

/// A local server answering every request with `status` and `body`.
fn serve(status: &'static str, body: Vec<u8>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/lensfun.tar.gz", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let _ = s.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
            let _ = s.write_all(&body);
        }
    });
    url
}

/// A session whose settings live in `dir`, downloading from `url`.
fn session(dir: &std::path::Path, url: &str) -> Session {
    let mut s = Session::new();
    s.lens_db = crate::lens_download::LensSettings::load(Some(dir.to_path_buf()));
    s.lens_db.url = url.to_string();
    s
}

/// A look-up the app makes by itself (after the database changed) gives the photo its lens data, but adds no undo
/// step: the user did nothing.
#[test]
fn a_lens_look_up_is_not_an_undo_step() {
    let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    if skip_for_env() {
        return;
    }
    let before = crate::lens_profiles::configured_dir();
    crate::lens_profiles::set_dir(None);
    let dir = scratch("undo");
    let m = lcc_meta::Metadata {
        make: Some("Testcam".into()),
        model: Some("TC-1".into()),
        lens_model: Some("18-55mm F3.5-5.6".into()),
        focal_length: Some(18.0),
        f_number: Some(3.5),
        ..Default::default()
    };
    let (w, h) = (64usize, 48usize);
    let data: Vec<u16> = (0..w * h).map(|i| 600 + (i % 977) as u16).collect();
    let file = dir.join("a.dng");
    std::fs::write(&file, crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, m)).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    assert!(s.catalog.photo(id).unwrap().embedded_lens.is_none());
    let db = scratch("undo-db");
    std::fs::write(db.join("lenses.xml"), DB).unwrap();
    s.lens_db.set_dir(Some(db.to_path_buf())).unwrap();
    let undo = s.undo.len();
    let changed = s.execute("lens.db.refresh", &json!({"ids": [id.0]})).unwrap();
    crate::lens_profiles::set_dir(before);
    assert_eq!(changed["changed"], json!([id.0]));
    assert!(s.catalog.photo(id).unwrap().embedded_lens.is_some(), "the profile was applied");
    assert_eq!(s.undo.len(), undo, "no undo step for a look-up");
}

fn skip_for_env() -> bool {
    lcc_brand::env_os(crate::lens_profiles::ENV).is_some_and(|v| !v.is_empty())
}

#[test]
fn folder_setting_is_saved_used_at_once_and_reloaded() {
    let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    if skip_for_env() {
        return;
    }
    let before = crate::lens_profiles::configured_dir();
    let settings = scratch("settings");
    let db = scratch("folder");
    std::fs::write(db.join("lenses.xml"), DB).unwrap();
    crate::lens_profiles::set_dir(None);
    let mut s = session(&settings, "http://127.0.0.1:9/none");
    assert_eq!(s.execute("lens.db.status", &json!({})).unwrap()["lenses"], 0);
    // not a folder, a folder without profiles: errors that say what to choose
    assert!(s.execute("lens.db.setDir", &json!({"dir": settings.join("nope").to_string_lossy()})).is_err());
    let empty = scratch("empty");
    let e = s.execute("lens.db.setDir", &json!({"dir": empty.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("no lens profiles"), "{e}");
    assert!(s.execute("lens.db.setDir", &json!({})).is_err());
    let st = s.execute("lens.db.setDir", &json!({"dir": db.to_string_lossy()})).unwrap();
    assert_eq!(st["lenses"], 1);
    assert_eq!(st["origin"], "folder");
    assert_eq!(st["dir"], db.to_string_lossy().as_ref());
    assert!(st["updated"].as_str().is_some_and(|d| d.len() == 10), "{st}");
    // remembered: a new session (a restart) uses it
    crate::lens_profiles::set_dir(None);
    let s2 = session(&settings, "");
    assert_eq!(crate::lens_profiles::configured_dir(), Some(db.to_path_buf()));
    assert_eq!(s2.lens_db.status()["lenses"], 1);
    // cleared with null, and remembered so
    s.execute("lens.db.setDir", &json!({"dir": null})).unwrap();
    assert_eq!(s.execute("lens.db.status", &json!({})).unwrap()["lenses"], 0);
    let _ = session(&settings, "");
    assert_eq!(crate::lens_profiles::configured_dir(), None);
    crate::lens_profiles::set_dir(before);
}

#[test]
fn download_installs_lensfun_and_failures_are_messages() {
    let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    if skip_for_env() {
        return;
    }
    let before = crate::lens_profiles::configured_dir();
    crate::lens_profiles::set_dir(None);
    let settings = scratch("download");
    let fail = |status: &'static str, body: Vec<u8>| -> String {
        let mut s = session(&settings, &serve(status, body));
        s.execute("lens.db.download", &json!({})).unwrap();
        match s.lens_db.wait() {
            Some(crate::lens_download::State::Failed(e)) => e,
            other => panic!("{status}: {other:?}"),
        }
    };
    let e = fail("404 Not Found", b"missing".to_vec());
    assert!(e.contains("could not download the lens database"), "{e}");
    let e = fail("200 OK", b"<html>a captive portal</html>".to_vec());
    assert!(e.contains("not a gzip archive"), "{e}");
    let good = archive();
    let e = fail("200 OK", good[..good.len() / 2].to_vec());
    assert!(e.contains("damaged") || e.contains("incomplete"), "{e}");
    let e = fail("200 OK", gzip(&crate::lens_download::tests::entry("x/data/db/a.xml", b"<lensdatabase/>", b'0')));
    assert!(e.contains("no lens profiles"), "{e}");
    // nothing was put in place by a failure
    assert_eq!(crate::lens_profiles::configured_dir(), None);
    assert!(!settings.join("lensfun").join("db").exists());

    let mut s = session(&settings, &serve("200 OK", good));
    let st = s.execute("lens.db.download", &json!({})).unwrap();
    assert_eq!(st["download"]["state"], "running");
    assert!(s.execute("lens.db.download", &json!({})).is_err() || s.lens_db.wait().is_some());
    assert_eq!(s.lens_db.wait(), Some(crate::lens_download::State::Done { lenses: 1 }));
    let st = s.execute("lens.db.status", &json!({})).unwrap();
    assert_eq!(st["lenses"], 1, "{st}");
    assert_eq!(st["downloaded"], true);
    assert_eq!(st["download"]["state"], "done");
    let target = settings.join("lensfun").join("db");
    assert!(target.join("slr-testcam.xml").is_file());
    // nothing outside the folder, nothing but the database
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    assert!(!settings.join("evil.xml").exists() && !settings.join("lensfun").join("evil.xml").exists());
    // remembered for the next start
    crate::lens_profiles::set_dir(None);
    let mut s = session(&settings, "");
    assert_eq!(s.lens_db.status()["lenses"], 1);
    // Remove deletes a downloaded database
    let st = s.execute("lens.db.remove", &json!({})).unwrap();
    assert_eq!(st["deleted"], true);
    assert_eq!(st["lenses"], 0);
    assert!(!settings.join("lensfun").exists());
    crate::lens_profiles::set_dir(before);
}

#[test]
fn a_download_without_a_settings_folder_is_refused() {
    let mut s = Session::new();
    let e = s.execute("lens.db.download", &json!({})).unwrap_err().to_string();
    assert!(e.contains("settings folder"), "{e}");
}

#[test]
fn optics_is_told_why_there_is_no_correction() {
    let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    if skip_for_env() {
        return;
    }
    use crate::lens_profiles::Match;
    let before = crate::lens_profiles::configured_dir();
    crate::lens_profiles::set_dir(None);
    let mut s = Session::new();
    let raw = |lens: &str| {
        let mut p = lcc_catalog::Photo::new(lcc_catalog::PhotoId(1), lcc_catalog::Source::Demo { scene: 0 }, "a.nef", "NEF", 10, 10, "now");
        p.kind = lcc_catalog::MediaKind::Raw;
        p.meta.lens = lens.into();
        p
    };
    assert_eq!(s.lens_match(&raw("18-55mm F3.5-5.6")), Some(Match::NoDatabase));
    let mut jpeg = raw("x");
    jpeg.kind = lcc_catalog::MediaKind::Image;
    let db = scratch("optics");
    std::fs::write(db.join("lenses.xml"), DB).unwrap();
    s.lens_db.set_dir(Some(db.to_path_buf())).unwrap();
    assert_eq!(s.lens_match(&jpeg), Some(Match::NotRaw));
    assert_eq!(s.lens_match(&raw("")), Some(Match::NoLensName));
    assert_eq!(s.lens_match(&raw("70-200mm F4")), Some(Match::NotFound("70-200mm F4".into())));
    assert!(matches!(s.lens_match(&raw("18-55mm F3.5-5.6")), Some(Match::Found(_))));
    let mut own = raw("70-200mm F4");
    own.embedded_lens = Some(Box::default());
    assert_eq!(s.lens_match(&own), None);
    // the command says the same about a catalogued photo
    let id = s.catalog.alloc_photo_id();
    let mut p = raw("70-200mm F4");
    p.id = id;
    s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let v: Value = s.execute("lens.db.photo", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(v, json!({"state": "notFound", "lens": "70-200mm F4"}));
    // a change makes every photo's lens data stale until it is looked up again
    assert!(s.lens_profiles_stale(id));
    s.execute("lens.db.refresh", &json!({"ids": [id.0]})).unwrap();
    assert!(!s.lens_profiles_stale(id));
    crate::lens_profiles::reload();
    assert!(s.lens_profiles_stale(id));
    crate::lens_profiles::set_dir(before);
}

/// `lens.db.photo` reports a given lens as `chosen` (over a name match, never over the file's own correction) and
/// `lens.db.list` lists the database for a picker; hostile parameters are errors, never panics.
#[test]
fn a_chosen_lens_and_the_lens_list() {
    let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    if skip_for_env() {
        return;
    }
    let before = crate::lens_profiles::configured_dir();
    crate::lens_profiles::set_dir(None);
    let mut s = Session::new();
    let none = s.execute("lens.db.list", &json!({})).unwrap();
    assert_eq!(none["database"], json!(false));
    let db = scratch("chosen");
    std::fs::write(db.join("lenses.xml"), DB).unwrap();
    s.lens_db.set_dir(Some(db.to_path_buf())).unwrap();
    let add = |s: &mut Session, lens: &str, own: bool| {
        let id = s.catalog.alloc_photo_id();
        let mut p = lcc_catalog::Photo::new(id, lcc_catalog::Source::Demo { scene: 0 }, "a.nef", "NEF", 10, 10, "now");
        p.kind = lcc_catalog::MediaKind::Raw;
        p.meta.lens = lens.into();
        if own {
            p.embedded_lens = Some(Default::default());
            s.lens_from_file.insert(id, true);
        }
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id.0
    };
    let nameless = add(&mut s, "", false);
    let named = add(&mut s, "18-55mm F3.5-5.6", false);
    let own = add(&mut s, "", true);
    let chosen = json!({"model": "Testcam 18-55mm f/3.5-5.6", "focal": 18});
    let photo = |s: &mut Session, id: u64, lens: Value| s.execute("lens.db.photo", &json!({"ids": [id], "lens": lens}));
    assert_eq!(photo(&mut s, nameless, Value::Null).unwrap()["state"], "noLensName");
    assert_eq!(photo(&mut s, nameless, chosen.clone()).unwrap(), json!({"state": "chosen", "lens": "Testcam 18-55mm f/3.5-5.6"}));
    // no focal length from the caller or the photo: no correction could apply, which the status says
    assert_eq!(photo(&mut s, nameless, json!("Testcam 18-55mm f/3.5-5.6")).unwrap()["state"], "noFocal");
    assert_eq!(photo(&mut s, named, Value::Null).unwrap()["state"], "found");
    assert_eq!(photo(&mut s, named, chosen.clone()).unwrap()["state"], "chosen");
    assert_eq!(photo(&mut s, named, json!({"model": "Nonesuch 1000mm"})).unwrap(), json!({"state": "notFound", "lens": "Nonesuch 1000mm"}));
    assert_eq!(photo(&mut s, own, chosen).unwrap()["state"], "embedded");
    for bad in [
        json!(7),
        json!({}),
        json!({"model": ""}),
        json!({"model": "x", "focal": -1}),
        json!({"model": "x", "focal": "wide"}),
        json!({"model": "x", "aperture": 1e308}),
        json!({"model": "x".repeat(10_000)}),
    ] {
        assert!(photo(&mut s, nameless, bad.clone()).is_err(), "{bad}");
    }
    let v = s.execute("lens.db.list", &json!({"mount": "T", "crop": 1.5, "query": "18-55"})).unwrap();
    assert_eq!(
        (v["total"].clone(), v["lenses"][0]["model"].clone(), v["more"].clone()),
        (json!(1), json!("Testcam 18-55mm f/3.5-5.6"), json!(false))
    );
    assert_eq!(s.execute("lens.db.list", &json!({"mount": "Other"})).unwrap()["total"], json!(0));
    assert_eq!(s.execute("lens.db.list", &json!({"limit": 1_000_000_000_000u64, "offset": u64::MAX})).unwrap()["lenses"], json!([]));
    for bad in [
        json!({"crop": -1}),
        json!({"crop": "aps"}),
        json!({"limit": -5}),
        json!({"limit": 1.5}),
        json!({"mount": 3}),
        json!({"query": "q".repeat(10_000)}),
    ] {
        assert!(s.execute("lens.db.list", &bad).is_err(), "{bad}");
    }
    crate::lens_profiles::set_dir(before);
}

/// A lens picked from the database (`lens.choose`, issue #307) corrects a raw whose metadata names a lens the
/// database lacks (a manual lens with no focal length): it needs a focal length, applies as one undo step, is
/// reported as `chosen`, and `null` goes back to the automatic choice.
#[test]
fn a_picked_lens_corrects_a_manual_lens() {
    let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    if skip_for_env() {
        return;
    }
    let before = crate::lens_profiles::configured_dir();
    crate::lens_profiles::set_dir(None);
    let dir = scratch("choose");
    let m = lcc_meta::Metadata {
        make: Some("Testcam".into()),
        model: Some("TC-1".into()),
        lens_model: Some("Oldglass 50mm".into()),
        ..Default::default()
    };
    let (w, h) = (64usize, 48usize);
    let data: Vec<u16> = (0..w * h).map(|i| 600 + (i % 977) as u16).collect();
    let file = dir.join("a.dng");
    std::fs::write(&file, crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, m)).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let db = scratch("choose-db");
    std::fs::write(db.join("lenses.xml"), DB).unwrap();
    s.lens_db.set_dir(Some(db.to_path_buf())).unwrap();
    let lens = json!({"maker": "Testcam", "model": "Testcam 18-55mm f/3.5-5.6"});

    let st = s.execute("lens.db.photo", &json!({})).unwrap();
    assert_eq!(st["state"], json!("notFound"), "{st}");
    // no focal length anywhere: nothing applies yet
    let st = s.execute("lens.choose", &json!({"lens": lens})).unwrap();
    assert_eq!(st["state"], json!("noFocal"), "{st}");
    assert!(s.catalog.photo(id).unwrap().embedded_lens.is_none());
    let undo = s.undo.len();
    let mut with_focal = lens.clone();
    with_focal["focal"] = json!(18.0);
    let st = s.execute("lens.choose", &json!({"lens": with_focal})).unwrap();
    assert_eq!(st, json!({"state": "chosen", "lens": "Testcam 18-55mm f/3.5-5.6"}));
    let p = s.catalog.photo(id).unwrap();
    assert!(p.embedded_lens.is_some(), "the picked profile corrects the photo");
    assert!(p.develop.optics.lens_profile, "picking a lens turns lens corrections on");
    assert_eq!(s.undo.len(), undo + 1, "one undo step");
    // kept in the catalog, not written to the XMP sidecar
    let xmp = crate::sidecar::sidecar_packet(p, &s.catalog);
    assert!(!xmp.contains("Testcam 18-55mm"), "the picked lens is not written to XMP");

    // back to the automatic choice
    let st = s.execute("lens.choose", &json!({"lens": null})).unwrap();
    assert_eq!(st["state"], json!("notFound"), "{st}");
    assert!(s.catalog.photo(id).unwrap().develop.optics.lens.is_none());
    assert!(s.catalog.photo(id).unwrap().embedded_lens.is_none(), "the picked profile no longer applies");
    crate::lens_profiles::set_dir(before);
}

/// Hostile `lens.choose` params are errors, never panics.
#[test]
fn lens_choose_refuses_hostile_params() {
    let mut s = Session::new();
    assert!(s.execute("lens.choose", &json!({"lens": "Some lens"})).is_err(), "no active photo");
    let id = s.catalog.alloc_photo_id();
    let mut p = lcc_catalog::Photo::new(id, lcc_catalog::Source::Demo { scene: 0 }, "A.NEF", "NEF", 30, 20, "2026-01-01T10:00:00");
    p.kind = lcc_catalog::MediaKind::Raw;
    s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let long = "x".repeat(100_000);
    for bad in [
        json!({}),
        json!({"lens": 5}),
        json!({"lens": []}),
        json!({"lens": ""}),
        json!({"lens": {"model": ""}}),
        json!({"lens": {"maker": "A"}}),
        json!({"lens": {"model": 3}}),
        json!({"lens": {"model": long}}),
        json!({"lens": {"model": "L", "maker": long}}),
        json!({"lens": {"model": "L", "focal": -1}}),
        json!({"lens": {"model": "L", "focal": 1e300}}),
        json!({"lens": {"model": "L", "focal": "fifty"}}),
    ] {
        assert!(s.execute("lens.choose", &bad).is_err(), "{bad} was accepted");
    }
    assert!(s.catalog.photo(id).unwrap().develop.optics.lens.is_none(), "nothing was stored");
}
