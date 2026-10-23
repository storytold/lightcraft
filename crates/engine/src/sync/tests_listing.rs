//! Synchronize Folder answers "is the photo's file there" and "has it a sidecar" from the
//! listing of the folder its scan has just made, instead of asking the disk about every photo.
//!
//! On a network share each question is a round trip (about 5 ms a file on a hard disk over SMB,
//! measured), and a photo without a sidecar took five of them; the listing has every name.
//! The listing answers only what it can be sure of on any file system: where a name could match
//! another way (case, letters outside ASCII, a link), the disk is asked as before.
use super::*;
use crate::walk::EntryKind::{File, Folder, Other};

fn listing(folders: &[(&str, &[(&str, crate::walk::EntryKind)], bool)]) -> Listing {
    let mut l = Listing::default();
    for (dir, names, complete) in folders {
        l.note(Path::new(dir), names.iter().map(|(n, k)| (std::ffi::OsString::from(n), *k)).collect(), *complete);
    }
    l
}

/// Given the listing of a folder, when it is asked about a name, then it says "there" for a
/// name it lists, "not there" for one nothing in the folder could be taken for, and nothing
/// (ask the disk) wherever a file system could see it otherwise.
#[test]
fn the_listing_answers_only_what_it_is_sure_of() {
    let l = listing(&[
        (
            "/pics/trip",
            &[("IMG_1.CR3", File), ("IMG_1.xmp", File), ("img_2.jpg", File), ("sub", Folder), ("link.jpg", Other), (".hidden.xmp", File)],
            true,
        ),
        ("/pics/cut", &[("IMG_1.CR3", File)], false),
        ("/pics/names", &[("IMG_1.CR3", File), ("Größe.JPG", File)], true),
        // (the capital sharp s, which a file system that folds case takes for `SS`)
        ("/pics/sharp", &[("IMG_1.CR3", File), ("STRA\u{1e9e}E.jpg", File)], true),
        ("/pics/devices", &[("aux.jpg", File), ("COM\u{b9}.jpg", File), ("IMG_1.CR3", File)], true),
    ]);
    let (there, gone, ask) = (Some(true), Some(false), None);
    for (path, exists, is_file) in [
        ("/pics/trip/IMG_1.CR3", there, there),
        ("/pics/trip/IMG_1.xmp", there, there),
        ("/pics/trip/.hidden.xmp", there, there),
        ("/pics/trip/sub", there, gone),
        ("/pics/trip/IMG_9.CR3", gone, gone),
        ("/pics/trip/IMG_1.XMP", ask, ask),
        ("/pics/trip/IMG_2.JPG", ask, ask),
        ("/pics/trip/link.jpg", ask, ask),
        ("/pics/trip/SUB", ask, ask),
        // a folder the walk did not list to its end, or not at all
        ("/pics/cut/IMG_1.CR3", there, there),
        ("/pics/cut/IMG_2.CR3", ask, ask),
        ("/pics/other/IMG_1.CR3", ask, ask),
        // a folder that holds a name outside ASCII: what it lists is there, and nothing is surely not there (file
        // systems hold such names equal in more ways than one)
        ("/pics/names/IMG_1.CR3", there, there),
        ("/pics/names/Größe.JPG", there, there),
        ("/pics/names/IMG_2.CR3", ask, ask),
        ("/pics/names/größe.JPG", ask, ask),
        ("/pics/trip/Größe.JPG", ask, ask),
        ("/pics/sharp/strasse.jpg", ask, ask),
        ("/pics/sharp/STRASSE.xmp", ask, ask),
        ("/pics/sharp/IMG_2.CR3", ask, ask),
        // names Windows resolves another way: a short name, a stream, dots or spaces at the end, a device
        ("/pics/trip/IMG_10~1.CR3", ask, ask),
        ("/pics/trip/IMG_1.CR3::$DATA", ask, ask),
        ("/pics/trip/IMG_1.CR3.", ask, ask),
        ("/pics/trip/IMG_1.CR3 ", ask, ask),
        ("/pics/devices/aux.jpg", ask, ask),
        ("/pics/devices/NUL", ask, ask),
        ("/pics/devices/nul.xmp", ask, ask),
        ("/pics/devices/COM1 .jpg", ask, ask),
        ("/pics/devices/COM\u{b9}.jpg", ask, ask),
        ("/pics/trip/con", ask, ask),
        ("/pics/trip/PRN.txt", ask, ask),
        ("/pics/trip/com2.jpg", ask, ask),
        ("/pics/trip/LPT9.xmp", ask, ask),
        ("/pics/trip/lpt0", ask, ask),
        ("/pics/trip/nul.tar.gz", ask, ask),
        ("/pics/trip/CONIN$", ask, ask),
        ("/pics/trip/com10.jpg", gone, gone),
        ("/pics/trip/computer.jpg", gone, gone),
        ("/pics/devices/IMG_1.CR3", there, there),
        ("/pics/devices/auxiliary.jpg", ask, ask),
        ("/pics/trip/auxiliary.jpg", gone, gone),
        // a path that does not end in its file's name is the disk's to resolve
        ("/pics/trip/IMG_1.CR3/", ask, ask),
        ("/pics/trip/IMG_1.CR3/.", ask, ask),
        ("/pics/trip/", ask, ask),
        ("", ask, ask),
    ] {
        assert_eq!((l.exists(Path::new(path)), l.is_file(Path::new(path))), (exists, is_file), "{path:?}");
    }
    // a name that is no text: nothing in its folder is surely not there
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut l = Listing::default();
        l.note(
            Path::new("/pics/raw"),
            vec![(std::ffi::OsString::from("IMG_1.CR3"), File), (std::ffi::OsString::from_vec(vec![0x66, 0xff, 0x6f]), File)],
            true,
        );
        assert_eq!((l.exists(Path::new("/pics/raw/IMG_1.CR3")), l.exists(Path::new("/pics/raw/IMG_2.CR3"))), (there, ask));
    }
}

fn photo(c: &mut Catalog, path: &Path) -> Arc<Photo> {
    let id = c.alloc_photo_id();
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    c.apply(lightcraft_catalog::Op::AddPhoto {
        photo: Box::new(Photo::new(id, Source::File { path: path.to_string_lossy().to_string() }, &name, "JPEG", 6, 4, "2020-01-01T00:00:00")),
    })
    .unwrap();
    Arc::clone(c.photo(id).unwrap())
}

/// Given a folder on disk whose photos are there or gone, with sidecars under every name a
/// sidecar can have, when each photo is checked with the folder's listing, then the answer is
/// the one the disk gives; and for photos with plain names the disk was asked nothing but the
/// sidecars' dates.
#[test]
fn a_check_from_the_listing_is_the_check_from_the_disk() {
    let dir = std::env::temp_dir().join(format!("lc-listing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let news = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="5"/></rdf:RDF></x:xmpmeta>"#;
    for name in [
        "plain.jpg",
        "stem.jpg",
        "stem.xmp",
        "full.jpg",
        "full.jpg.xmp",
        "upper.jpg",
        "upper.XMP",
        "sub/deep.jpg",
        "sub/deep.xmp",
        "Größe.jpg",
        "Größe.xmp",
        "dir.jpg.xmp",
    ] {
        std::fs::write(dir.join(name), if name.ends_with("mp") || name.ends_with("MP") { news.as_bytes() } else { b"jpeg" }).unwrap();
    }
    let mut c = Catalog::new();
    let photos: Vec<Arc<Photo>> =
        ["plain.jpg", "stem.jpg", "full.jpg", "upper.jpg", "sub/deep.jpg", "Größe.jpg", "gone.jpg", "sub/gone.jpg", "dir.jpg"]
            .iter()
            .map(|n| photo(&mut c, &dir.join(n)))
            .collect();
    // (`dir.jpg` is gone, though a sidecar of its name is there)
    let mut l = Listing::default();
    let walked = crate::walk::files_in_noting(&dir, None, crate::walk::Limits::default(), |_| true, &mut |d, names, complete| {
        l.note(d, names, complete);
        true
    });
    assert_eq!(walked.files.len(), 12);
    let nothing = Listing::default();
    for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
        for p in &photos {
            let from_disk = check_photo(p, naming, &c, &nothing);
            assert_eq!(check_photo(p, naming, &c, &l), from_disk, "{:?} {naming:?}", p.file_name);
        }
    }
    let expected = [Check::Nothing, Check::News, Check::News, Check::News, Check::News, Check::News, Check::Missing, Check::Missing, Check::Missing];
    assert_eq!(photos.iter().map(|p| check_photo(p, SidecarNaming::Stem, &c, &l)).collect::<Vec<_>>(), expected);

    // what the disk was asked about the same photos, with the listing and without: fewer questions with it
    let asked = |l: &Listing| {
        for p in &photos {
            check_photo(p, SidecarNaming::Stem, &c, l);
        }
        l.asked.load(std::sync::atomic::Ordering::Relaxed)
    };
    let mut with = Listing::default();
    crate::walk::files_in_noting(&dir, None, crate::walk::Limits::default(), |_| true, &mut |d, names, complete| {
        with.note(d, names, complete);
        true
    });
    let (with, without) = (asked(&with), asked(&Listing::default()));
    // (without: each photo's file, and the names of its sidecar up to the one that is there: 20 questions where
    // names are found in any case, 21 where they are not)
    assert!(without >= 20, "{without}");
    // (with: this folder holds `Größe.*`, so what it does not list is still asked about: the nine names of the
    // sidecar `plain.jpg` has not (`.xmp`, `.XMP` and `.Xmp` of `plain`, `plain.jpg` and `plain.JPG`), the three
    // `full.jpg` has not, `upper.xmp`, and the two photos that are gone. What it lists is not asked about, nor
    // anything in `sub`, whose names are all ASCII)
    assert_eq!(with, 15, "asked {with} times with the listing, {without} without");

    // a folder of plain names only: the disk is asked nothing at all
    let sub = dir.join("sub");
    let mut only_plain = Listing::default();
    crate::walk::files_in_noting(&sub, None, crate::walk::Limits::default(), |_| true, &mut |d, names, complete| {
        only_plain.note(d, names, complete);
        true
    });
    let mut c2 = Catalog::new();
    for (name, expected) in [("deep.jpg", Check::News), ("gone.jpg", Check::Missing), ("gone.jpg.xmp", Check::Missing)] {
        let p = photo(&mut c2, &sub.join(name));
        assert_eq!(check_photo(&p, SidecarNaming::Stem, &c2, &only_plain), expected, "{name}");
    }
    assert_eq!(only_plain.asked.load(std::sync::atomic::Ordering::Relaxed), 0, "every answer came from the listing");
    let _ = std::fs::remove_dir_all(&dir);
}
