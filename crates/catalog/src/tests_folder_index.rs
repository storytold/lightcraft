//! Which photos have their file in a folder, or a file of a given name, whatever the letter case
//! (see [`crate::index`]): asked once per photo moved to the Trash, so a big library answers from
//! an index instead of a pass over every photo.
//!
//! * Spellings of a folder that differ only in case are one folder; a folder inside it is another.
//! * The answers stay right as photos are added, removed, relinked or pointed at another file, and
//!   when those are undone, or a batch fails and is rolled back.
//! * A plain path is read without building a folder key, and gives the same key.

use crate::*;

fn add(c: &mut Catalog, path: &str) -> PhotoId {
    let id = c.alloc_photo_id();
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let p = Photo::new(id, Source::File { path: path.into() }, name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

const FOLDERS: [&str; 6] = ["/a/b", "/a/b/sub", "/c", "/c/d", "/e", "/"];
const NAMES: [&str; 5] = ["x.jpg", "y.jpg", "z.jpg", "w.jpg", "v.jpg"];

/// Every answer for the folders and names the tests use.
fn answers(c: &Catalog) -> Vec<Vec<PhotoId>> {
    let folders = FOLDERS.iter().map(|f| c.photos_in_folder_any_case(&folder_index_key(f)));
    folders.chain(NAMES.iter().map(|n| c.photos_named_any_case(n))).collect()
}

#[test]
fn spellings_that_differ_in_case_are_one_folder_and_one_name() {
    let mut c = Catalog::new();
    let x = add(&mut c, "/A/b/x.jpg");
    let y = add(&mut c, "/a/B/y.jpg");
    let z = add(&mut c, "/a/b/sub/z.jpg");
    let big_x = add(&mut c, "/c/X.JPG");
    assert_eq!(c.photos_in_folder_any_case(&folder_index_key("/a/b")), vec![x, y]);
    assert_eq!(c.photos_in_folder_any_case(&folder_index_key("/A/B/sub")), vec![z]);
    assert_eq!(c.photos_named_any_case("x.jpg"), vec![x, big_x]);
    assert!(c.photos_named_any_case("X.JPG").is_empty(), "the caller lower-cases the name");
}

#[test]
fn the_answers_follow_every_op_that_moves_a_file_and_its_undo() {
    let mut c = Catalog::new();
    let x = add(&mut c, "/a/b/x.jpg");
    add(&mut c, "/a/b/sub/z.jpg");
    answers(&c); // built: from here on, the ops keep them
    let check = |c: &Catalog, step: &str| assert_eq!(answers(c), answers(&c.clone()), "{step}");
    let id = c.alloc_photo_id();
    let added = Photo::new(id, Source::File { path: "/C/y.jpg".into() }, "y.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    let steps = [
        ("add", Op::AddPhoto { photo: Box::new(added) }),
        ("set file", Op::SetFile { id: x, file_name: "W.jpg".into(), source: Source::File { path: "/e/W.jpg".into() } }),
        ("relink", Op::Relink { id: x, file_name: "v.jpg".into(), source: Source::File { path: "/c/d/v.jpg".into() }, format: None }),
        ("demo", Op::SetFile { id: x, file_name: "x.jpg".into(), source: Source::Demo { scene: 1 } }),
        ("remove", Op::RemovePhoto { id }),
    ];
    for (step, op) in steps {
        let inverse = c.apply(op.clone()).unwrap();
        check(&c, step);
        c.apply(inverse).unwrap();
        check(&c, &format!("{step} undone"));
        c.apply(op).unwrap();
        check(&c, &format!("{step} again"));
    }
    assert!(c.photos_in_folder_any_case(&folder_index_key("/c")).is_empty(), "the added photo is gone");
}

#[test]
fn a_batch_rolled_back_leaves_the_answers_as_they_were() {
    let mut c = Catalog::new();
    let x = add(&mut c, "/a/b/x.jpg");
    let before = answers(&c);
    let failing = Op::Batch {
        ops: vec![
            Op::SetFile { id: x, file_name: "y.jpg".into(), source: Source::File { path: "/c/y.jpg".into() } },
            Op::RemovePhoto { id: x },
            Op::SetRating { id: x, rating: 1 }, // the photo is gone: fails
        ],
    };
    assert!(c.apply(failing).is_err());
    assert_eq!(answers(&c), before);
    assert_eq!(answers(&c), answers(&c.clone()));
}

#[test]
fn a_plain_path_gives_the_same_folder_key_as_any_other_spelling() {
    let slow = |path: &str| {
        let key = crate::query::folder_key(path);
        key.rsplit_once('/').map(|(dir, _)| if dir.is_empty() { "/".to_string() } else { dir.to_lowercase() })
    };
    for path in ["/a/b.jpg", "/b.jpg", "/a//b.jpg", "/a/./b.jpg", "/a/../b.jpg", "/a/b/", "/Volumes/X/[Stills]/0002/o.jpg", "a/b.jpg"] {
        assert_eq!(photo_folder_index_key(path), slow(path), "{path}");
    }
    assert_eq!(photo_folder_index_key("/Volumes/X/[Stills]/0002/o.jpg").as_deref(), Some("/volumes/x/[stills]/0002"));
    assert_eq!(photo_folder_index_key("/a//b.jpg"), photo_folder_index_key("/a/b.jpg"));
}

#[test]
fn a_folder_key_matches_its_files_key_at_a_drive_or_share_root_too() {
    for (file, folder) in
        [("C:\\x.jpg", "C:\\"), ("C:\\x.jpg", "C:"), ("\\\\nas\\share\\x.jpg", "\\\\nas\\share"), ("/x.jpg", "/"), ("/a/b/x.jpg", "/a/b/")]
    {
        assert_eq!(photo_folder_index_key(file), Some(folder_index_key(folder)), "{file} in {folder}");
    }
}

#[test]
fn a_file_name_is_cut_at_either_separator() {
    assert_eq!(file_name_index_key("/a/B.JPG").as_deref(), Some("b.jpg"));
    assert_eq!(file_name_index_key("D:\\Photos\\IMG_1.JPG").as_deref(), Some("img_1.jpg"));
    assert_eq!(file_name_index_key("/a/b/").as_deref(), Some("b"));
    assert_eq!(file_name_index_key("/"), None);
    assert_eq!(file_name_index_key("/a/.."), None);
}
