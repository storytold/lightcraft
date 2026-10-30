//! Synchronize Folder's "moved" match never takes a file that is only browsed in Local as the new
//! place of a missing photo: that file already has a record of its own.
use super::*;
use lightcraft_catalog::{Catalog, Op};

/// Given a missing photo with no hash on record and one file of the folder with its size and
/// dimensions, when the files are matched, then the file is its new place, unless the file is one
/// the library only browsed.
#[test]
fn a_browsed_file_is_not_where_a_missing_photo_went() {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/pics/old.jpg".into() }, "old.jpg", "JPEG", 6, 4, "2026-01-01");
    p.file_size = 1000;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let missing = [FolderPhoto { id: id.0, path: "/pics/old.jpg".into() }];
    let mut found = ImportCandidate { path: "/pics/new.jpg".into(), width: 6, height: 4, file_size: 1000, existing: Some(99), ..Default::default() };
    let moved = match_moved(&missing, std::slice::from_ref(&found), &HashMap::new(), &c);
    assert_eq!(moved.get("/pics/old.jpg").map(String::as_str), Some("/pics/new.jpg"), "a new file is where it went");
    found.local = true;
    assert!(match_moved(&missing, &[found], &HashMap::new(), &c).is_empty(), "a browsed file has a record already");
}
