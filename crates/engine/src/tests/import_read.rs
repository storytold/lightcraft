//! What an import reads from the photos themselves (`library.import` → `read`, and the library's
//! default, `library.preferences` → `import.read`): keywords, ratings and labels, descriptions,
//! location and develop settings can each be left in the files. Everything is read unless
//! chosen otherwise.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-import-read-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A photo that says a lot about itself: in the file, keywords, a title, a caption, a creator,
/// a copyright, a place by name and by GPS; in the sidecar beside it, a rating, a label, other
/// keywords, another title, a city and an exposure.
fn a_photo_with_metadata(dir: &Path, name: &str, seed: u8) -> PathBuf {
    let embedded = lightcraft_meta::Metadata {
        keywords: vec!["harbour".into(), "boats".into()],
        title: Some("In the file".into()),
        caption: Some("Morning light".into()),
        artist: Some("A. Photographer".into()),
        copyright: Some("© A. Photographer".into()),
        city: Some("Bergen".into()),
        country: Some("Norway".into()),
        gps: Some(lightcraft_meta::Gps { latitude: 60.39, longitude: 5.32, altitude: None }),
        make: Some("Procedural".into()),
        model: Some("Camera 1".into()),
        capture_time: lightcraft_meta::DateTime::parse_iso("2026-01-14T05:58:48"),
        ..Default::default()
    };
    let xmp = lightcraft_meta::write_xmp(&embedded, None);
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let meta = lightcraft_codecs::EncodeMeta { xmp: Some(&xmp), ..Default::default() };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &meta).unwrap();
    let path = dir.join(format!("{name}.png"));
    std::fs::write(&path, bytes).unwrap();
    std::fs::write(
        dir.join(format!("{name}.xmp")),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"
 xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
 xmp:Rating="4" xmp:Label="Red" photoshop:City="Oslo" crs:Exposure2012="0.5">
 <dc:subject><rdf:Bag><rdf:li>fjord</rdf:li></rdf:Bag></dc:subject>
 <dc:title><rdf:Alt><rdf:li xml:lang="x-default">In the sidecar</rdf:li></rdf:Alt></dc:title>
</rdf:Description></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    path
}

/// What the one imported photo is in the library, by the things a file can say about it.
fn seen(s: &Session) -> Value {
    let p = s.catalog.photos().next().expect("one photo");
    json!({
        "keywords": p.meta.keywords,
        "rating": p.rating,
        "label": p.label.is_some(),
        "title": p.meta.title,
        "caption": p.meta.caption,
        "creator": p.meta.creator,
        "copyright": p.meta.copyright,
        "city": p.meta.city,
        "country": p.meta.country,
        "gps": p.meta.gps.is_some(),
        "edited": *p.develop != p.camera_defaults(),
        "camera": p.meta.camera,
        "captured": p.captured,
    })
}

fn import(s: &mut Session, dir: &Path, read: Option<Value>) -> Value {
    let mut p = json!({"paths": [dir.to_string_lossy()]});
    if let Some(read) = read {
        p["read"] = read;
    }
    let r = s.execute("library.import", &p).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    r
}

/// Given a photo that carries keywords, a rating, a label, a title and more, a place and
/// develop settings, when it is imported without saying otherwise, then all of it is read, as
/// it always was: the sidecar's word over the file's.
#[test]
fn everything_a_photo_says_is_read_unless_chosen_otherwise() {
    let dir = temp_dir("all");
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &dir, None);
    assert_eq!(r["sidecars"], 1, "{r}");
    assert_eq!(
        seen(&s),
        json!({
            "keywords": ["fjord"], "rating": 4, "label": true, "title": "In the sidecar", "caption": "Morning light",
            "creator": "A. Photographer", "copyright": "© A. Photographer", "city": "Oslo", "country": "Norway", "gps": true,
            "edited": true, "camera": "Procedural Camera 1", "captured": "2026-01-14T05:58:48",
        })
    );
    assert_eq!(
        s.execute("library.preferences", &json!({})).unwrap()["import"]["read"],
        json!({"keywords": true, "ratings": true, "descriptions": true, "location": true, "develop": true})
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Given the same photo, when it is imported with each kind of thing left in the file in
/// turn, then that kind is not in the library (neither from the file nor from its sidecar),
/// everything else is, and what the camera recorded (which camera, when) always is.
#[test]
fn each_kind_of_thing_can_be_left_in_the_file() {
    let all = json!({
        "keywords": ["fjord"], "rating": 4, "label": true, "title": "In the sidecar", "caption": "Morning light",
        "creator": "A. Photographer", "copyright": "© A. Photographer", "city": "Oslo", "country": "Norway", "gps": true,
        "edited": true, "camera": "Procedural Camera 1", "captured": "2026-01-14T05:58:48",
    });
    let left_out: [(&str, Value); 5] = [
        ("keywords", json!({"keywords": []})),
        ("ratings", json!({"rating": 0, "label": false})),
        ("descriptions", json!({"title": "", "caption": "", "creator": "", "copyright": ""})),
        ("location", json!({"city": "", "country": "", "gps": false})),
        ("develop", json!({"edited": false})),
    ];
    for (kind, without) in &left_out {
        let dir = temp_dir(kind);
        a_photo_with_metadata(&dir, "one", 1);
        let mut s = Session::new().with_fs();
        import(&mut s, &dir, Some(json!({*kind: false})));
        let mut expected = all.clone();
        for (k, v) in without.as_object().unwrap() {
            expected[k] = v.clone();
        }
        assert_eq!(seen(&s), expected, "without {kind}");
        // the file and its sidecar are as they were: nothing is taken out of them
        assert!(std::fs::read_to_string(dir.join("one.xmp")).unwrap().contains("fjord"));
        let _ = std::fs::remove_dir_all(dir);
    }
    // and all of it at once: a photo as the camera made it
    let dir = temp_dir("none");
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, Some(json!({"keywords": false, "ratings": false, "descriptions": false, "location": false, "develop": false})));
    assert_eq!(
        seen(&s),
        json!({
            "keywords": [], "rating": 0, "label": false, "title": "", "caption": "", "creator": "", "copyright": "", "city": "",
            "country": "", "gps": false, "edited": false, "camera": "Procedural Camera 1", "captured": "2026-01-14T05:58:48",
        })
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// What the import itself gives the photos still applies when the files' own is left out:
/// the keywords typed for this import. The library's default creator and copyright are for
/// photos that have none: a photo whose own were left in the file is not given the library's
/// in their place (it is somebody's, though the library does not say whose).
#[test]
fn what_the_import_adds_still_applies_but_defaults_do_not_stand_in_for_what_was_not_read() {
    let dir = temp_dir("adds");
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    s.execute("library.preferences", &json!({"import": {"creator": "Me", "copyright": "© Me"}})).unwrap();
    let r = s
        .execute(
            "library.import",
            &json!({"paths": [dir.to_string_lossy()], "keywords": ["2026 trip"], "read": {"keywords": false, "descriptions": false}}),
        )
        .unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    let got = seen(&s);
    assert_eq!((got["keywords"].clone(), got["creator"].clone(), got["copyright"].clone()), (json!(["2026 trip"]), json!(""), json!("")));
    assert_eq!((got["rating"].clone(), got["city"].clone()), (json!(4), json!("Oslo")), "the rest was read");
    // (read, the photo's own win as ever; and a photo with none gets the library's)
    s.execute("edit.undo", &json!({})).unwrap();
    import(&mut s, &dir, None);
    assert_eq!(seen(&s)["creator"], json!("A. Photographer"));
    let bare = temp_dir("adds-bare");
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[9, 9, 9, 255]; 64] };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(bare.join("bare.png"), bytes).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("library.import", &json!({"paths": [bare.to_string_lossy()], "read": {"descriptions": false}})).unwrap();
    assert_eq!((seen(&s)["creator"].clone(), seen(&s)["copyright"].clone()), (json!("Me"), json!("© Me")));
    for d in [dir, bare] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// The library's own choice (`library.preferences` → `import.read`) is what an import goes by
/// when it does not say: it is saved with the library, a part of it can be changed alone, and
/// an import that says something else for once goes by that for the parts it names.
#[test]
fn the_library_says_what_its_imports_read_when_they_do_not() {
    let (lib, dir, again) = (temp_dir("lib"), temp_dir("default"), temp_dir("default-again"));
    a_photo_with_metadata(&dir, "one", 1);
    a_photo_with_metadata(&again, "two", 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("library.preferences", &json!({"import": {"read": {"keywords": false, "ratings": false}}})).unwrap();
    assert_eq!(r["import"]["read"], json!({"keywords": false, "ratings": false, "descriptions": true, "location": true, "develop": true}));
    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.execute("library.preferences", &json!({})).unwrap()["import"]["read"]["keywords"], json!(false), "saved with the library");

    import(&mut s, &dir, None);
    let got = seen(&s);
    assert_eq!((got["keywords"].clone(), got["rating"].clone(), got["title"].clone()), (json!([]), json!(0), json!("In the sidecar")));
    // for once: keywords after all, and no titles; ratings as the library says
    s.execute("edit.undo", &json!({})).unwrap();
    import(&mut s, &again, Some(json!({"keywords": true, "descriptions": false})));
    let got = seen(&s);
    assert_eq!((got["keywords"].clone(), got["rating"].clone(), got["title"].clone()), (json!(["fjord"]), json!(0), json!("")));
    // changing one part leaves the others
    let r = s.execute("library.preferences", &json!({"import": {"read": {"keywords": true}}})).unwrap();
    assert_eq!(r["import"]["read"], json!({"keywords": true, "ratings": false, "descriptions": true, "location": true, "develop": true}));
    drop(s);
    for d in [lib, dir, again] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// What is not a choice of what to read is refused, and says what the choices are.
#[test]
fn a_choice_that_is_none_is_refused() {
    let dir = temp_dir("bad");
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    for bad in [json!({"keyword": false}), json!({"keywords": "no"}), json!("nothing"), json!([false])] {
        let e = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()], "read": bad})).unwrap_err().to_string();
        assert!(e.contains("read") && e.contains("keywords"), "{bad}: {e}");
        let e = s.execute("library.preferences", &json!({"import": {"read": bad}})).unwrap_err().to_string();
        assert!(e.contains("read") && e.contains("keywords"), "{bad}: {e}");
    }
    assert_eq!(s.catalog.len(), 0, "nothing was imported");
    let _ = std::fs::remove_dir_all(dir);
}

/// What the photo's sidecar says, read back from the file as it is now.
fn in_the_sidecar(dir: &Path) -> (String, lightcraft_meta::Metadata) {
    let text = std::fs::read_to_string(dir.join("one.xmp")).unwrap();
    let meta = lightcraft_meta::parse_xmp(&text).unwrap().metadata;
    (text, meta)
}

/// Given a photo imported with its keywords, rating, label, title and place left in the file,
/// in a library that writes its changes into sidecars, when the photo is edited (so that its
/// sidecar is written), then the sidecar still has all of that: the library, which has nothing
/// to say of those kinds, takes nothing out. The same when the sidecar is saved by hand.
#[test]
fn a_sidecar_written_later_keeps_what_was_left_in_it() {
    let dir = temp_dir("kept");
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    import(&mut s, &dir, Some(json!({"keywords": false, "ratings": false, "descriptions": false, "location": false, "develop": false})));
    let before = std::fs::read_to_string(dir.join("one.xmp")).unwrap();
    let id = s.catalog.photos().next().unwrap().id.0;
    s.execute("library.select", &json!({"ids": [id], "active": id})).unwrap();

    s.execute("develop.set", &json!({"control": "light.contrast", "value": 20})).unwrap();
    let (text, meta) = in_the_sidecar(&dir);
    assert_ne!(text, before, "the sidecar was written: the library's develop settings are in it");
    assert_eq!(meta.keywords, ["fjord"]);
    assert_eq!((meta.rating, meta.label.as_deref()), (Some(4), Some("Red")));
    assert_eq!((meta.title.as_deref(), meta.city.as_deref()), (Some("In the sidecar"), Some("Oslo")));
    assert!(text.contains("crs:Exposure2012"), "another editor's develop settings stay, as ever");

    s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    let (_, meta) = in_the_sidecar(&dir);
    assert_eq!((meta.keywords.clone(), meta.rating, meta.title.as_deref()), (vec!["fjord".to_string()], Some(4), Some("In the sidecar")));
    let _ = std::fs::remove_dir_all(dir);
}

/// What a photo is given in the library of a kind that was left in its file stays in the
/// library: the sidecar keeps the file's own (its rating, its keywords, its title), whatever is
/// done here, also when that is undone again. A kind that was read is written as ever.
#[test]
fn what_was_left_in_the_file_is_not_written_back_whatever_the_photo_is_given_here() {
    let dir = temp_dir("given");
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute(
        "library.import",
        &json!({"paths": [dir.to_string_lossy()], "keywords": ["2026 trip"], "read": {"keywords": false, "ratings": false, "descriptions": false}}),
    )
    .unwrap();
    let id = s.catalog.photos().next().unwrap().id.0;
    s.execute("library.select", &json!({"ids": [id], "active": id})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    s.execute("photo.flag", &json!({"flag": "pick"})).unwrap();
    s.execute("photo.setMeta", &json!({"title": "Given here", "city": "Bergen"})).unwrap();
    let the_files = |dir: &Path| {
        let (text, meta) = in_the_sidecar(dir);
        assert_eq!(meta.keywords, ["fjord"], "the file's own keywords, and not the library's");
        assert_eq!((meta.rating, meta.label.as_deref(), meta.title.as_deref()), (Some(4), Some("Red"), Some("In the sidecar")));
        assert!(!text.contains("lc:flag"), "nor its flag");
        meta
    };
    assert_eq!(the_files(&dir).city.as_deref(), Some("Bergen"), "the place was read, and is the library's to write");
    // taken back again: the file's own are as they were, not what the library had for a while
    for _ in 0..3 {
        s.execute("edit.undo", &json!({})).unwrap();
    }
    assert_eq!(the_files(&dir).city.as_deref(), Some("Oslo"));
    // the library has what it was given: nothing of it is lost by not being written
    assert_eq!(seen(&s)["keywords"], json!(["2026 trip"]));
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a folder that is synchronized after a sidecar changed in it, then the changed sidecar
/// is read for what the library has of the photo, and what was left in the file at import
/// stays there: a rating changed by another program is no news to a library that left ratings
/// in the files, and a place changed there is read without the rating coming along.
#[test]
fn synchronizing_a_folder_reads_a_changed_sidecar_without_what_was_left() {
    let dir = temp_dir("sync");
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, Some(json!({"ratings": false, "keywords": false})));
    let p = s.catalog.photos().next().unwrap().clone();
    assert!(p.left_in_file.ratings && p.left_in_file.keywords && !p.left_in_file.location);
    let sidecar = dir.join("one.xmp");
    let touched = |text: String, days: u64| {
        std::fs::write(&sidecar, text).unwrap();
        let ahead = std::time::SystemTime::now() + std::time::Duration::from_secs(3600 * 24 * days);
        std::fs::File::options().write(true).open(&sidecar).unwrap().set_modified(ahead).unwrap();
    };
    let sync = |s: &mut Session| s.execute("folder.synchronize", &json!({"path": dir.to_string_lossy(), "readMetadata": true})).unwrap();
    // another program rates it five, some time after the import: no news to this library
    touched(std::fs::read_to_string(&sidecar).unwrap().replace("xmp:Rating=\"4\"", "xmp:Rating=\"5\""), 400);
    let r = sync(&mut s);
    assert_eq!(r["read"], json!(0), "{r}");
    assert_eq!(seen(&s)["rating"], json!(0));
    // and then moves it to Tromsø: that is read, and the rating does not come along
    touched(std::fs::read_to_string(&sidecar).unwrap().replace("Oslo", "Tromsø"), 800);
    let r = sync(&mut s);
    assert_eq!(r["read"], json!(1), "{r}");
    let got = seen(&s);
    assert_eq!((got["city"].clone(), got["rating"].clone(), got["keywords"].clone()), (json!("Tromsø"), json!(0), json!([])));
    let _ = std::fs::remove_dir_all(dir);
}

/// A photo imported with everything read is written as it always was: what the library has is
/// what its sidecar says, and a keyword taken off here is taken out of the sidecar.
#[test]
fn a_photo_that_was_read_whole_is_written_as_ever() {
    let dir = temp_dir("whole");
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    import(&mut s, &dir, None);
    let id = s.catalog.photos().next().unwrap().id.0;
    assert!(s.catalog.photos().next().unwrap().left_in_file.is_nothing());
    s.execute("library.select", &json!({"ids": [id], "active": id})).unwrap();
    s.execute("photo.rate", &json!({"rating": 0})).unwrap();
    s.execute("photo.setMeta", &json!({"keywords": []})).unwrap();
    let (_, meta) = in_the_sidecar(&dir);
    assert_eq!((meta.keywords.len(), meta.rating), (0, Some(0)), "the library's word is the sidecar's");
    let _ = std::fs::remove_dir_all(dir);
}

fn copy_import(s: &mut Session, src: &Path, dest: &Path, read: Value) -> Value {
    let p = json!({"paths": [src.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy(), "organize": "flat", "read": read});
    s.execute("library.import", &p).unwrap()
}

/// Given a Copy import that leaves the keywords unread, and a photo whose keywords are only in the sidecar beside
/// it, when it runs, then the sidecar is copied beside the copy (the copy is all that is left of the photo once the
/// source is gone), and undoing the import leaves the photo copy and the sidecar copy where they are.
#[test]
fn a_copy_brings_the_sidecar_when_it_holds_what_was_left_unread() {
    let (src, dest) = (temp_dir("copy-src"), temp_dir("copy-dest"));
    a_photo_with_metadata(&src, "one", 1);
    let mut s = Session::new().with_fs();
    let r = copy_import(&mut s, &src, &dest, json!({"keywords": false}));
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    let copied = std::fs::read_to_string(dest.join("one.xmp")).expect("the sidecar went with the copy");
    assert_eq!(copied, std::fs::read_to_string(src.join("one.xmp")).unwrap());
    assert!(copied.contains("fjord"));
    assert!(src.join("one.xmp").exists(), "the source's own is untouched");
    s.undo_step().unwrap();
    assert_eq!(s.catalog.photos().count(), 0);
    assert!(dest.join("one.png").exists() && dest.join("one.xmp").exists(), "undo leaves the files, as it does for any copy");
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dest);
}

/// A sidecar named for the whole file (`one.png.XMP`, in whatever letter case the disk has it) is copied under the
/// copy's whole name, not as a stem-named `one.xmp` beside it. (The mistake this guards against showed only on a disk
/// that ignores letter case, as on macOS and Windows.)
#[test]
fn a_sidecar_named_for_the_whole_file_keeps_that_style_beside_the_copy() {
    let (src, dest) = (temp_dir("copy-full-src"), temp_dir("copy-full-dest"));
    a_photo_with_metadata(&src, "one", 1);
    std::fs::rename(src.join("one.xmp"), src.join("one.png.XMP")).unwrap();
    let mut s = Session::new().with_fs();
    let r = copy_import(&mut s, &src, &dest, json!({"keywords": false}));
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    let names: Vec<String> = std::fs::read_dir(&dest).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_lowercase()).collect();
    assert!(names.contains(&"one.png.xmp".to_string()), "the sidecar keeps the whole-name style: {names:?}");
    assert!(!names.contains(&"one.xmp".to_string()), "and no stem-named sidecar appears: {names:?}");
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dest);
}

/// An import that reads everything, or a photo with no sidecar beside it, copies the photo alone, as ever.
#[test]
fn a_copy_leaves_the_sidecar_when_nothing_in_it_was_left_unread() {
    for (read, sidecar) in [(json!({}), true), (json!({"keywords": false}), false)] {
        let (src, dest) = (temp_dir("copy-none-src"), temp_dir("copy-none-dest"));
        a_photo_with_metadata(&src, "one", 1);
        if !sidecar {
            std::fs::remove_file(src.join("one.xmp")).unwrap();
        }
        let mut s = Session::new().with_fs();
        let r = copy_import(&mut s, &src, &dest, read);
        assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
        assert!(dest.join("one.png").exists() && !dest.join("one.xmp").exists());
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(dest);
    }
}

/// A sidecar that can't be put beside the copy (a different one is there already) fails the file: nothing of it is
/// imported, no photo copy is left, and the report says so.
#[test]
fn a_sidecar_that_cannot_be_copied_fails_the_file_and_leaves_no_copy() {
    let (src, dest) = (temp_dir("copy-fail-src"), temp_dir("copy-fail-dest"));
    a_photo_with_metadata(&src, "one", 1);
    std::fs::write(dest.join("one.png"), b"another photo").unwrap();
    std::fs::write(dest.join("one-1.xmp"), b"another sidecar").unwrap();
    let mut s = Session::new().with_fs();
    let r = copy_import(&mut s, &src, &dest, json!({"keywords": false}));
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(0), "{r}");
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(1), "{r}");
    assert!(!dest.join("one-1.png").exists(), "no half-copied photo");
    assert_eq!(s.catalog.photos().count(), 0);
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dest);
}

// ---- each dialog remembers how its Read row was last used ----

fn prefs(s: &mut Session, p: Value) -> Value {
    s.execute("library.preferences", &p).unwrap()["import"].clone()
}

/// Given a library that reads everything, when the Synchronize Folder dialog was last used with
/// Keywords unticked, then that is remembered with the library for that dialog alone: the
/// Import review and the library's own choice are as they were, and so are imports that have no
/// dialog.
#[test]
fn a_dialog_remembers_how_its_read_row_was_last_used() {
    use crate::import::{ReadDialog, ReadFromPhotos};
    let (lib, dir) = (temp_dir("last-lib"), temp_dir("last"));
    a_photo_with_metadata(&dir, "one", 1);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(prefs(&mut s, json!({})).get("readLast").is_none(), "nothing is remembered at first");

    let r = prefs(&mut s, json!({"import": {"readLast": {"synchronize": {"keywords": false}}}}));
    assert_eq!(r["readLast"], json!({"synchronize": {"keywords": false, "ratings": true, "descriptions": true, "location": true, "develop": true}}));
    assert_eq!(r["read"]["keywords"], json!(true), "the library's choice is its own");
    assert!(!s.import_defaults.read_in(ReadDialog::Synchronize).keywords);
    assert_eq!(s.import_defaults.read_in(ReadDialog::Import), ReadFromPhotos::ALL, "the other dialog is as it was");

    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(!s.import_defaults.read_in(ReadDialog::Synchronize).keywords, "kept between sessions");
    // an import that has no dialog (dropped files, Auto Import, an agent's) goes by the library
    import(&mut s, &dir, None);
    assert_eq!(seen(&s)["keywords"], json!(["fjord"]));
    drop(s);
    for d in [lib, dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// A dialog used as the library has it follows the library; so do all once the library's
/// choice changes; and `null` forgets.
#[test]
fn what_is_remembered_is_only_a_departure_from_the_librarys_choice() {
    use crate::import::ReadDialog;
    let mut s = Session::new().with_fs();
    // back to what the library reads: nothing to remember
    prefs(&mut s, json!({"import": {"readLast": {"import": {"ratings": false}}}}));
    let r = prefs(&mut s, json!({"import": {"readLast": {"import": {"ratings": true}}}}));
    assert!(r.get("readLast").is_none(), "{r}");

    // one part named: the others as the dialog would open
    prefs(&mut s, json!({"import": {"readLast": {"import": {"ratings": false}, "synchronize": {"location": false}}}}));
    let r = prefs(&mut s, json!({"import": {"readLast": {"import": {"develop": false}}}}));
    assert_eq!(r["readLast"]["import"], json!({"keywords": true, "ratings": false, "descriptions": true, "location": true, "develop": false}));
    assert_eq!(r["readLast"]["synchronize"]["location"], json!(false), "the other dialog's is untouched");

    let r = prefs(&mut s, json!({"import": {"readLast": {"synchronize": null}}}));
    assert!(r["readLast"].get("synchronize").is_none() && r["readLast"].get("import").is_some(), "{r}");

    // saying the library's choice again changes nothing
    let r = prefs(&mut s, json!({"import": {"read": {"keywords": true}}}));
    assert!(r["readLast"].get("import").is_some(), "{r}");
    // a new choice for the library: the dialogs follow it again
    let r = prefs(&mut s, json!({"import": {"read": {"keywords": false}}}));
    assert!(r.get("readLast").is_none(), "{r}");
    assert!(!s.import_defaults.read_in(ReadDialog::Import).keywords && s.import_defaults.read_in(ReadDialog::Import).ratings);
}

#[test]
fn what_is_not_a_dialog_or_a_choice_is_refused() {
    let mut s = Session::new().with_fs();
    for (p, why) in [
        (json!({"import": {"readLast": 5}}), "readLast"),
        (json!({"import": {"readLast": {"export": {"keywords": false}}}}), "import and synchronize"),
        (json!({"import": {"readLast": {"import": {"keyword": false}}}}), "keywords"),
        (json!({"import": {"readLast": {"import": {"keywords": "no"}}}}), "true or false"),
        (json!({"import": {"readLast": {"synchronize": []}}}), "read"),
    ] {
        let e = s.execute("library.preferences", &p).unwrap_err().to_string();
        assert!(e.contains(why), "{p}: {e}");
        assert!(s.import_defaults.read_last.is_empty(), "{p}: nothing was remembered");
    }
}

/// Given a folder of the library that got a new photo, when it is synchronized with keywords
/// and ratings not read, then the new photo comes in without them and they stay in its files;
/// synchronized without saying, it is read as the library says.
#[test]
fn synchronizing_reads_the_new_photos_as_chosen_for_it() {
    for (read, keywords, rating) in [(Some(json!({"keywords": false, "ratings": false})), json!([]), 0), (None, json!(["fjord"]), 4)] {
        let dir = temp_dir("sync-read");
        a_photo_with_metadata(&dir, "one", 1);
        let mut s = Session::new().with_fs();
        import(&mut s, &dir, None);
        a_photo_with_metadata(&dir, "two", 2);
        let mut p = json!({"path": dir.to_string_lossy()});
        if let Some(read) = &read {
            p["read"] = read.clone();
        }
        let r = s.execute("folder.synchronize", &p).unwrap();
        assert_eq!(r["imported"], json!(1), "{r}");
        let two = s.catalog.photos().find(|p| p.file_name == "two.png").expect("the new photo");
        assert_eq!((json!(two.meta.keywords), two.rating), (keywords, rating), "{read:?}");
        assert_eq!(two.meta.title, "In the sidecar", "what was not unticked is read");
        assert_eq!((two.left_in_file.keywords, two.left_in_file.ratings), (read.is_some(), read.is_some()), "and the photo remembers what was left");
        let one = s.catalog.photos().find(|p| p.file_name == "one.png").expect("the photo that was there");
        assert_eq!(one.rating, 4, "photos already in the library are as they were");
        let _ = std::fs::remove_dir_all(dir);
    }
    // a choice that is none is refused
    let mut s = Session::new().with_fs();
    let e = s.execute("folder.synchronize", &json!({"path": "/nowhere", "read": {"stars": false}})).unwrap_err().to_string();
    assert!(e.contains("keywords"), "{e}");
}
