//! What the library keeps about its folders: a colour label, like a photo's (see
//! [`crate::folders::FolderRecord`]).
//!
//! Scenarios, in the words of someone organising their library by folder:
//!
//! * Given a folder of the library, when I give it a colour label, its row in the folder tree
//!   shows that label; the folders around it show none.
//! * However the folder is spelled, it is one folder with one label.
//! * When I take the label off, the library keeps nothing about the folder.
//! * Every change is one undo step: undo puts back the label the folder had before.
//! * A path that names no folder is refused and nothing changes.
//! * When the folder is renamed or moved, its label and those of the folders inside it go along;
//!   a folder beside it whose name merely starts the same way keeps its own. Undo brings them
//!   back, also a label that was left at the destination.
//! * Labels survive closing and reopening the library.

use crate::*;

fn add(c: &mut Catalog, path: &str) {
    let id = c.alloc_photo_id();
    let name = path.rsplit('/').next().unwrap_or(path);
    let p = Photo::new(id, Source::File { path: path.into() }, name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
}

fn library() -> Catalog {
    let mut c = Catalog::new();
    for p in ["/pics/trip/a.jpg", "/pics/trip/day1/b.jpg", "/pics/trip2/c.jpg", "/pics/home/d.jpg"] {
        add(&mut c, p);
    }
    c
}

fn label(c: &mut Catalog, path: &str, l: Option<ColorLabel>) -> Op {
    let op = c.folder_label_op(path, l);
    c.apply(op).unwrap()
}

/// The label of each row of the tree, by path, depth first (rows without one left out).
fn tree_labels(c: &Catalog) -> Vec<(String, ColorLabel)> {
    fn walk(n: &FolderNode, out: &mut Vec<(String, ColorLabel)>) {
        if let Some(l) = n.label {
            out.push((n.path.clone(), l));
        }
        n.children.iter().for_each(|c| walk(c, out));
    }
    let mut out = Vec::new();
    c.folder_tree().iter().for_each(|n| walk(n, &mut out));
    out
}

#[test]
fn a_new_library_knows_nothing_about_its_folders() {
    let c = library();
    assert_eq!(c.folder_record("/pics/trip"), None);
    assert_eq!(c.folder_color_label("/pics/trip"), None);
    assert!(tree_labels(&c).is_empty());
}

#[test]
fn a_labelled_folder_shows_its_label_in_the_tree_and_only_there() {
    let mut c = library();
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    assert_eq!(c.folder_color_label("/pics/trip"), Some(ColorLabel::Red));
    assert_eq!(tree_labels(&c), vec![("/pics/trip".to_string(), ColorLabel::Red)]);
}

#[test]
fn however_a_folder_is_spelled_it_has_one_label() {
    let mut c = library();
    label(&mut c, "/pics//trip/", Some(ColorLabel::Green));
    assert_eq!(c.folder_color_label("/pics/./trip"), Some(ColorLabel::Green));
    label(&mut c, "/pics/home/../trip", Some(ColorLabel::Blue));
    assert_eq!(c.folder_color_label("/pics/trip"), Some(ColorLabel::Blue));
    assert_eq!(tree_labels(&c).len(), 1, "one folder, one record");
}

#[test]
fn taking_the_label_off_leaves_no_record() {
    let mut c = library();
    label(&mut c, "/pics/trip", Some(ColorLabel::Purple));
    label(&mut c, "/pics/trip", None);
    assert_eq!(c.folder_record("/pics/trip"), None);
    assert!(!c.to_snapshot().contains("folder_records"), "nothing empty is saved");
}

#[test]
fn an_empty_record_is_no_record() {
    let mut c = library();
    c.apply(Op::SetFolderRecord { folder: "/pics/trip".into(), record: Some(FolderRecord::default()) }).unwrap();
    assert_eq!(c.folder_record("/pics/trip"), None);
}

#[test]
fn undo_puts_back_the_label_the_folder_had() {
    let mut c = library();
    let undo_first = label(&mut c, "/pics/trip", Some(ColorLabel::Yellow));
    let undo_second = label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    c.apply(undo_second).unwrap();
    assert_eq!(c.folder_color_label("/pics/trip"), Some(ColorLabel::Yellow));
    c.apply(undo_first).unwrap();
    assert_eq!(c.folder_record("/pics/trip"), None);
}

#[test]
fn a_path_that_names_no_folder_is_refused() {
    let mut c = library();
    let before = c.to_snapshot();
    for bad in ["", "relative/trip", ".", "a/.."] {
        let op = Op::SetFolderRecord { folder: bad.into(), record: Some(FolderRecord { label: Some(ColorLabel::Red) }) };
        assert!(c.apply(op).is_err(), "{bad:?} names no folder");
    }
    assert_eq!(c.to_snapshot(), before);
}

#[test]
fn a_moved_folder_takes_its_labels_and_its_subfolders_labels_along() {
    let mut c = library();
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    label(&mut c, "/pics/trip/day1", Some(ColorLabel::Green));
    label(&mut c, "/pics/trip2", Some(ColorLabel::Blue));
    let ops = c.folder_records_follow("/pics/trip", "/archive/2026/trip");
    c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.folder_record("/pics/trip"), None);
    assert_eq!(c.folder_color_label("/archive/2026/trip"), Some(ColorLabel::Red));
    assert_eq!(c.folder_color_label("/archive/2026/trip/day1"), Some(ColorLabel::Green));
    assert_eq!(c.folder_color_label("/pics/trip2"), Some(ColorLabel::Blue), "a neighbour that only starts the same stays");
}

#[test]
fn undoing_a_move_brings_back_every_label_also_one_left_at_the_destination() {
    let mut c = library();
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    // a folder that once was at the destination left its labels behind
    label(&mut c, "/pics/holiday", Some(ColorLabel::Purple));
    label(&mut c, "/pics/holiday/day9", Some(ColorLabel::Yellow));
    let before = c.to_snapshot();
    let ops = c.folder_records_follow("/pics/trip", "/pics/holiday");
    let undo = c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.folder_color_label("/pics/holiday"), Some(ColorLabel::Red), "the moved folder's label wins");
    assert_eq!(c.folder_record("/pics/holiday/day9"), None, "what was known about the old folder goes");
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
}

#[test]
fn a_folder_without_labels_moves_without_ops() {
    let c = library();
    assert!(c.folder_records_follow("/pics/trip", "/elsewhere/trip").is_empty());
}

#[test]
fn an_unlabelled_folder_moved_onto_a_stale_label_does_not_inherit_it() {
    let mut c = library();
    // a folder that was at the destination once (its photos are gone) left its label behind
    label(&mut c, "/pics/holiday", Some(ColorLabel::Purple));
    let ops = c.folder_records_follow("/pics/trip", "/pics/holiday");
    let undo = c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.folder_record("/pics/holiday"), None, "the folder now there is another one");
    c.apply(undo).unwrap();
    assert_eq!(c.folder_color_label("/pics/holiday"), Some(ColorLabel::Purple));
}

#[test]
fn labels_survive_the_journal_without_a_snapshot() {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    {
        let p = "/pics/trip/a.jpg";
        let id = c.alloc_photo_id();
        let op = Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: p.into() }, "a.jpg", "JPEG", 6, 4, "2026-01-01")) };
        c.apply(op.clone()).unwrap();
        j.append(std::slice::from_ref(&op)).unwrap();
    }
    let op = c.folder_label_op("/pics/trip", Some(ColorLabel::Yellow));
    c.apply(op.clone()).unwrap();
    j.append(std::slice::from_ref(&op)).unwrap();
    drop(j);
    let (_, reopened, r) = Journal::open(Box::new(m)).unwrap();
    assert!(r.replayed >= 2, "{r:?}");
    assert_eq!(reopened.folder_color_label("/pics/trip"), Some(ColorLabel::Yellow));
}

#[test]
fn labels_survive_closing_and_reopening_the_library() {
    let mut c = library();
    label(&mut c, "/pics/home", Some(ColorLabel::Green));
    let reopened = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
    assert_eq!(reopened.folder_color_label("/pics/home"), Some(ColorLabel::Green));
}

#[test]
fn a_library_saved_before_folder_records_opens_without_any() {
    let c = library();
    let old = c.to_snapshot();
    assert!(!old.contains("folder_records"));
    let reopened = Catalog::from_snapshot(&old).unwrap();
    assert_eq!(reopened.folder_record("/pics/trip"), None);
}

/// Export as Catalog and Import from Another Catalog carry the folder labels and the keyword list
/// (both were lost before: only photos, albums, stacks and label names went across).
#[test]
fn folder_labels_and_the_keyword_list_go_across_catalogs() {
    use crate::keywords::KeywordInfo;
    use crate::transfer::{self, ConflictRule, ExportOptions};
    let mut c = library();
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    label(&mut c, "/pics/home", Some(ColorLabel::Green));
    let info = KeywordInfo { synonyms: vec!["seaside".into()], ..Default::default() };
    c.apply(Op::SetKeyword { path: "Places|beach".into(), info: Some(info.clone()) }).unwrap();
    // export the trip's photos only
    let trip: Vec<PhotoId> =
        c.photos().filter(|p| matches!(&p.source, Source::File { path } if path.starts_with("/pics/trip/"))).map(|p| p.id).collect();
    let sub = transfer::subset(&c, &ExportOptions { photos: trip, ..Default::default() });
    assert_eq!(sub.folder_color_label("/pics/trip"), Some(ColorLabel::Red));
    assert_eq!(sub.folder_record("/pics/home"), None, "a folder none of the exported photos is in stays behind");
    assert_eq!(sub.keyword_info("Places|beach"), Some(&info));
    // the exported catalog keeps them on disk
    let dir = std::env::temp_dir().join(format!("dac-xfer-records-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ids: Vec<PhotoId> = sub.photos().map(|p| p.id).collect();
    let rep = transfer::export_catalog(&c, &ExportOptions { photos: ids, ..Default::default() }, &dir, "Trip").unwrap();
    let read = transfer::load_readonly(&rep.entry).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(read.folder_color_label("/pics/trip"), Some(ColorLabel::Red));
    assert_eq!(read.keyword_info("Places|beach"), Some(&info));
    // importing into an empty library brings them in
    let mut other = Catalog::new();
    let op = transfer::plan_import(&other, &read).ops(&mut other, &read, ConflictRule::Keep);
    other.apply(op).unwrap();
    assert_eq!(other.folder_color_label("/pics/trip"), Some(ColorLabel::Red));
    assert_eq!(other.keyword_info("Places|beach"), Some(&info));
    // what the library already says about a folder or a keyword wins
    let mut mine = library();
    label(&mut mine, "/pics/trip", Some(ColorLabel::Blue));
    mine.apply(Op::SetKeyword { path: "places|Beach".into(), info: Some(KeywordInfo::default()) }).unwrap();
    let op = transfer::plan_import(&mine, &read).ops(&mut mine, &read, ConflictRule::Keep);
    mine.apply(op).unwrap();
    assert_eq!(mine.folder_color_label("/pics/trip"), Some(ColorLabel::Blue));
    assert_eq!(mine.keyword_info("Places|beach"), Some(&KeywordInfo::default()));
}
