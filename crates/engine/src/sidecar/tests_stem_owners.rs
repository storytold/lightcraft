//! Who owns a stem sidecar, worked out for the photos asked about only (`StemOwners::among`).
//!
//! A library can hold millions of photos; a sidecar that is written for one of them, or a folder
//! that is synchronized, must not cost a table of them all.
use super::*;
use lightcraft_catalog::{Catalog, Op, Photo, Source};

/// Paths that try the key: case in folders, names and endings, no ending, a dot file, endings
/// inside folder names, non-ASCII letters (one that lower-cases to ASCII: the Kelvin sign).
const PATHS: [&str; 26] = [
    "/pics/trip/IMG_1.CR3",
    "/pics/trip/IMG_1.JPG",
    "/pics/trip/img_1.jpeg",
    "/Pics/Trip/IMG_1.tif",
    "/pics/trip/IMG_2.JPG",
    "/pics/trip/IMG_3.CR3",
    "/pics/trip/IMG_3.cr3.JPG",
    "/pics/trip/IMG_3.cr3",
    "/pics/other/IMG_1.JPG",
    "/pics/other/IMG_1",
    "/pics/other/.hidden",
    "/pics/other/.hidden.jpg",
    "/pics/v1.2/a.b.JPG",
    "/pics/v1.2/a.b.cr3",
    "/pics/v1.2/a",
    "/pics/straße/Größe.JPG",
    "/pics/STRASSE/GRÖSSE.cr3",
    "/pics/straße/größe.CR3",
    "/pics/kelvin/\u{212a}1.JPG",
    "/pics/kelvin/k1.cr3",
    "/pics/ΟΔΟΣ/ΟΔΟΣ.JPG",
    "/pics/οδος/οδος.CR3",
    // (names that are all dots before their ending: the ending is no extension to replace)
    "/pics/dots/..cr3",
    "/pics/dots/..jpg",
    "/pics/dots/a./.",
    "/pics/dots/a.jpg",
];

fn library() -> (Catalog, Vec<PhotoId>) {
    let mut c = Catalog::new();
    let mut ids = Vec::new();
    for path in PATHS {
        let id = c.alloc_photo_id();
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut p = Photo::new(id, Source::File { path: path.to_string() }, name, "X", 6, 4, "2026-01-01");
        if path.to_lowercase().ends_with("cr3") {
            p.kind = MediaKind::Raw;
        }
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    // a virtual copy of the first (it owns nothing) and a photo that is no file
    let copy = c.alloc_photo_id();
    let mut p = Photo::new(copy, Source::File { path: PATHS[0].to_string() }, "IMG_1.CR3", "X", 6, 4, "2026-01-01");
    p.copy_of = Some(ids[0]);
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    ids.push(copy);
    let demo = c.alloc_photo_id();
    c.apply(Op::AddPhoto { photo: Box::new(Photo::new(demo, Source::Demo { scene: 1 }, "demo", "X", 6, 4, "2026-01-01")) }).unwrap();
    ids.push(demo);
    (c, ids)
}

fn namings(owners: &StemOwners, c: &Catalog, ids: &[PhotoId]) -> Vec<SidecarNaming> {
    ids.iter().map(|id| owners.naming(c.photo(*id).unwrap(), SidecarNaming::Stem)).collect()
}

/// Given a library where files share stems in every way a name can, when the owners are worked
/// out for some of its photos only, then each of those photos is named exactly as the table of
/// the whole library names it: whichever photos are asked about, alone or together.
#[test]
fn owners_among_some_photos_name_them_as_the_whole_library_does() {
    let (c, ids) = library();
    let whole = StemOwners::of(&c);
    let expected = namings(&whole, &c, &ids);
    assert!(expected.contains(&SidecarNaming::Full) && expected.contains(&SidecarNaming::Stem), "both cases are there: {expected:?}");
    // all at once
    assert_eq!(namings(&StemOwners::among(&c, &ids), &c, &ids), expected);
    // one at a time, and every pair
    for (i, a) in ids.iter().enumerate() {
        assert_eq!(namings(&StemOwners::among(&c, &[*a]), &c, &[*a]), [expected[i]], "{:?}", PATHS.get(i));
        for (j, b) in ids.iter().enumerate() {
            assert_eq!(namings(&StemOwners::among(&c, &[*a, *b]), &c, &[*a, *b]), [expected[i], expected[j]]);
        }
    }
    // an id that is not in the library is passed over
    assert_eq!(StemOwners::among(&c, &[PhotoId(9_999)]).0.len(), 0);
}

/// Given a library of many folders, when the owners are worked out for the photos of one, then
/// the table holds that folder's shared stems and nothing of the others: what it costs follows
/// the photos asked about, not the library.
#[test]
fn owners_among_some_photos_hold_nothing_of_the_rest() {
    let mut c = Catalog::new();
    let mut first = Vec::new();
    for folder in 0..200 {
        for name in ["IMG_1.CR3", "IMG_1.JPG", "IMG_2.JPG"] {
            let id = c.alloc_photo_id();
            let mut p = Photo::new(id, Source::File { path: format!("/pics/{folder:03}/{name}") }, name, "X", 6, 4, "2026-01-01");
            if name.ends_with("CR3") {
                p.kind = MediaKind::Raw;
            }
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            if folder == 0 {
                first.push(id);
            }
        }
    }
    assert_eq!(StemOwners::of(&c).0.len(), 200, "one shared stem a folder");
    let some = StemOwners::among(&c, &first);
    assert_eq!(some.0.len(), 1, "the one shared stem of the folder asked about");
    assert_eq!(namings(&some, &c, &first), [SidecarNaming::Stem, SidecarNaming::Full, SidecarNaming::Stem]);
}

/// The key written into a buffer (no allocation for a plain path) is the key: for every path
/// above, for shapes a path from a file can have, and for every string of up to six of the
/// characters that decide a key (a letter in each case, a dot, both separators, a letter that is
/// not ASCII).
#[test]
fn the_key_in_a_buffer_is_the_key() {
    let odd = [
        "",
        "/",
        "a",
        "a.",
        ".a",
        "..a",
        "...a",
        "a.b",
        "a/b.c/",
        "a/b.c//",
        "a/.b",
        "a/..b",
        "a/b.",
        "a/b..c",
        "a./.",
        "a\\b.c",
        "C:\\x\\y.JPG",
        "a/b.c/.",
        "a/b.c/..",
        "x.JPG ",
        "É.JPG",
    ];
    let mut buf = String::new();
    for path in PATHS.iter().chain(&odd) {
        stem_key_into(&mut buf, path);
        assert_eq!(buf, stem_key(path), "{path:?}");
    }
    let letters = ['a', 'B', '.', '/', '\\', 'é'];
    let mut tried = 0usize;
    let mut todo: Vec<String> = vec![String::new()];
    while let Some(path) = todo.pop() {
        stem_key_into(&mut buf, &path);
        assert_eq!(buf, stem_key(&path), "{path:?}");
        tried += 1;
        if path.chars().count() < 6 {
            todo.extend(letters.iter().map(|c| format!("{path}{c}")));
        }
    }
    assert_eq!(tried, (0..=6).map(|n| 6usize.pow(n)).sum::<usize>());
}
