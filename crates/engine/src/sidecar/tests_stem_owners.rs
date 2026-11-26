//! Who owns a stem sidecar, worked out for the photos asked about only (`StemOwners::among`), and
//! for the files an import is adding (`ImportStems`).
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

/// The kind a file of this name is catalogued as: the `.cr3` ones are raws.
fn kind_of(path: &str) -> MediaKind {
    if path.to_lowercase().ends_with("cr3") { MediaKind::Raw } else { MediaKind::Image }
}

fn library() -> (Catalog, Vec<PhotoId>) {
    let mut c = Catalog::new();
    let mut ids = Vec::new();
    for path in PATHS {
        let id = c.alloc_photo_id();
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut p = Photo::new(id, Source::File { path: path.to_string() }, name, "X", 6, 4, "2026-01-01");
        p.kind = kind_of(path);
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

/// Given a library where files share stems in every way a name can, when files arrive in an
/// import, then each of them may read the shared stem sidecar exactly when it would own it once
/// it is catalogued: the arriving files are ranked against the library's files and against each
/// other, as the write rule ranks them (issue #1905).
#[test]
fn an_arriving_file_owns_the_stem_sidecar_exactly_when_it_would_once_catalogued() {
    let (c, _) = library();
    // the arrivals: the same names under an upper-case folder, so each shares its stem with the
    // library's files (the key lower-cases the path) without being one of them
    let incoming: Vec<(String, MediaKind)> = PATHS
        .iter()
        .map(|p| {
            let (dir, name) = p.rsplit_once('/').unwrap_or(("", p));
            (format!("{}/{name}", dir.to_uppercase()), kind_of(p))
        })
        .collect();
    // the library with these arrivals in it, the ids drawn in the order they arrive
    let arriving_in = |base: &Catalog, add: &[usize]| -> Catalog {
        let mut cat = base.clone();
        for &k in add {
            let (path, kind) = &incoming[k];
            let id = cat.alloc_photo_id();
            let name = path.rsplit('/').next().unwrap_or(path);
            let mut p = Photo::new(id, Source::File { path: path.clone() }, name, "X", 6, 4, "2026-01-01");
            p.kind = *kind;
            cat.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        cat
    };
    let owns_once_catalogued = |cat: &Catalog, k: usize| -> bool {
        let path = &incoming[k].0;
        let p = cat.photos().find(|p| matches!(&p.source, Source::File { path: x } if x == path)).unwrap();
        StemOwners::of(cat).naming(p, SidecarNaming::Stem) == SidecarNaming::Stem
    };
    let as_arriving = |ks: &[usize]| -> Vec<(&str, MediaKind)> { ks.iter().map(|k| (incoming[*k].0.as_str(), incoming[*k].1)).collect() };
    let all: Vec<usize> = (0..incoming.len()).collect();
    for i in 0..incoming.len() {
        let alone = ImportStems::for_import(Some(&c), as_arriving(&[i]).into_iter());
        assert_eq!(alone.owns(&incoming[i].0), owns_once_catalogued(&arriving_in(&c, &[i]), i), "{} alone", incoming[i].0);
        for j in 0..incoming.len() {
            // every pair arriving together
            if i == j {
                continue;
            }
            let pair = ImportStems::for_import(Some(&c), as_arriving(&[i, j]).into_iter());
            let cat = arriving_in(&c, &[i, j]);
            for k in [i, j] {
                assert_eq!(
                    pair.owns(&incoming[k].0),
                    owns_once_catalogued(&cat, k),
                    "{} with {}",
                    incoming[k].0,
                    incoming[if k == i { j } else { i }].0
                );
            }
        }
    }
    // a job prepared twice (a library already looked over) has nothing to rank against: every
    // one of them is ranked among the arrivals alone, as it would be in a library of just them
    let alone = ImportStems::for_import(None, as_arriving(&all).into_iter());
    let only_arrivals = arriving_in(&Catalog::new(), &all);
    for k in &all {
        assert_eq!(alone.owns(&incoming[*k].0), owns_once_catalogued(&only_arrivals, *k), "{} without a library", incoming[*k].0);
    }
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
