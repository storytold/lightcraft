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
//! * When a folder moved outside the app is found again (Update Folder Location) in a folder the
//!   library already has, the labels go along and the folders already there keep theirs.
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

/// A new library knows nothing about its folders; a label shows on its folder's row and only there; a second label
/// replaces it; undo puts back what the folder had; taking the label off (or an empty record) leaves no record.
#[test]
fn a_folders_label_is_set_shown_undone_and_cleared() {
    let mut c = library();
    assert_eq!(c.folder_record("/pics/trip"), None);
    assert_eq!(c.folder_color_label("/pics/trip"), None);
    assert!(tree_labels(&c).is_empty());
    let undo_first = label(&mut c, "/pics/trip", Some(ColorLabel::Yellow));
    assert_eq!(c.folder_color_label("/pics/trip"), Some(ColorLabel::Yellow));
    assert_eq!(tree_labels(&c), vec![("/pics/trip".to_string(), ColorLabel::Yellow)], "its row, and only that one");
    let undo_second = label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    c.apply(undo_second).unwrap();
    assert_eq!(c.folder_color_label("/pics/trip"), Some(ColorLabel::Yellow));
    c.apply(undo_first).unwrap();
    assert_eq!(c.folder_record("/pics/trip"), None);
    label(&mut c, "/pics/trip", Some(ColorLabel::Purple));
    label(&mut c, "/pics/trip", None);
    assert_eq!(c.folder_record("/pics/trip"), None);
    assert!(!c.to_snapshot().contains("folder_records"), "nothing empty is saved");
    c.apply(Op::SetFolderRecord { folder: "/pics/trip".into(), record: Some(FolderRecord::default()) }).unwrap();
    assert_eq!(c.folder_record("/pics/trip"), None, "an empty record is no record");
    assert!(c.folder_records_follow("/pics/trip", "/elsewhere/trip").is_empty(), "a folder without labels moves without ops");
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
fn a_folder_found_again_inside_a_known_one_leaves_that_folders_labels_alone() {
    let mut c = library();
    for p in ["/x/own.jpg", "/x/other/e.jpg"] {
        add(&mut c, p);
    }
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    label(&mut c, "/pics/trip/day1", Some(ColorLabel::Yellow));
    label(&mut c, "/x", Some(ColorLabel::Green));
    label(&mut c, "/x/other", Some(ColorLabel::Blue));
    // a folder that was there once (its photos are gone) left its label behind
    label(&mut c, "/x/gone", Some(ColorLabel::Purple));
    let before = c.to_snapshot();
    let ops = c.folder_records_relocate("/pics/trip", "/x");
    let undo = c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.folder_record("/pics/trip"), None, "nothing stays at the old place");
    assert_eq!(c.folder_color_label("/x"), Some(ColorLabel::Red), "the label carried over wins");
    assert_eq!(c.folder_color_label("/x/day1"), Some(ColorLabel::Yellow));
    assert_eq!(c.folder_color_label("/x/other"), Some(ColorLabel::Blue), "a folder already there keeps its label");
    assert_eq!(c.folder_record("/x/gone"), None, "a stale one goes, as for a move");
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
}

#[test]
fn an_unlabelled_folder_found_again_in_a_labelled_one_keeps_that_label() {
    let mut c = library();
    label(&mut c, "/pics/home", Some(ColorLabel::Green));
    // the trip's photos now sit in the folder above them: the folders beside it keep theirs
    label(&mut c, "/pics/trip2", Some(ColorLabel::Blue));
    label(&mut c, "/pics", Some(ColorLabel::Purple));
    let ops = c.folder_records_relocate("/pics/trip", "/pics");
    c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.folder_color_label("/pics"), Some(ColorLabel::Purple));
    assert_eq!(c.folder_color_label("/pics/home"), Some(ColorLabel::Green));
    assert_eq!(c.folder_color_label("/pics/trip2"), Some(ColorLabel::Blue));
}

#[test]
fn a_disk_found_holding_a_folder_again_keeps_its_own_label() {
    let mut c = library();
    for p in ["D:/own.jpg", "D:/x/e.jpg", "//nas/photos/f.jpg", "//nas/photos/y/g.jpg"] {
        add(&mut c, p);
    }
    label(&mut c, "D:\\", Some(ColorLabel::Green));
    label(&mut c, "//nas/photos", Some(ColorLabel::Blue));
    for disk in ["D:\\", "//nas/photos"] {
        let ops = c.folder_records_relocate("/pics/home", disk);
        assert!(ops.is_empty(), "nothing to carry, and {disk} still holds photos: {ops:?}");
    }
}

#[test]
fn a_located_folder_keeps_what_the_library_knows_of_the_folders_already_at_the_destination() {
    let mut c = library();
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    label(&mut c, "/pics/trip/day1", Some(ColorLabel::Green));
    // other folders of the library at the place it is said to be at: nothing arrived there
    label(&mut c, "/pics/home", Some(ColorLabel::Blue));
    label(&mut c, "/pics/home/old", Some(ColorLabel::Yellow));
    let before = c.to_snapshot();
    let ops = c.folder_records_located("/pics/trip", "/pics/home");
    let undo = c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.folder_record("/pics/trip"), None, "what was known at the old place goes");
    assert_eq!(c.folder_record("/pics/trip/day1"), None);
    assert_eq!(c.folder_color_label("/pics/home"), Some(ColorLabel::Red), "a record carried over replaces the one at the same place");
    assert_eq!(c.folder_color_label("/pics/home/old"), Some(ColorLabel::Yellow), "a folder of the library there keeps its own");
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
}

#[test]
fn a_located_folder_with_labels_inside_lands_them_at_the_same_places_below() {
    let mut c = library();
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    label(&mut c, "/pics/trip/day1", Some(ColorLabel::Green));
    let ops = c.folder_records_located("/pics/trip", "/mnt/nas/trip");
    c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.folder_color_label("/mnt/nas/trip"), Some(ColorLabel::Red));
    assert_eq!(c.folder_color_label("/mnt/nas/trip/day1"), Some(ColorLabel::Green));
}

#[test]
fn a_folder_located_where_it_is_or_without_labels_gives_no_ops() {
    let mut c = library();
    assert!(c.folder_records_located("/pics/trip", "/elsewhere/trip").is_empty());
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    assert!(c.folder_records_located("/pics/trip", "/pics/trip").is_empty());
}

#[test]
fn a_folder_located_below_itself_leaves_the_records_of_the_folders_already_there_in_place() {
    let mut c = library();
    label(&mut c, "/pics/trip", Some(ColorLabel::Red));
    label(&mut c, "/pics/trip/day1", Some(ColorLabel::Green));
    // another folder of the library already sits at the place the folder is said to be at
    label(&mut c, "/pics/trip/day1/raw", Some(ColorLabel::Blue));
    let before = c.to_snapshot();
    let ops = c.folder_records_located("/pics/trip", "/pics/trip/day1");
    let undo = c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.folder_record("/pics/trip"), None, "what was known at the old place goes");
    assert_eq!(c.folder_color_label("/pics/trip/day1"), Some(ColorLabel::Red), "a record carried over replaces the one at the same place");
    assert_eq!(c.folder_color_label("/pics/trip/day1/raw"), Some(ColorLabel::Blue), "a folder below the destination keeps its own");
    assert_eq!(c.folder_record("/pics/trip/day1/day1"), None, "the record at the destination is not carried below itself");
    assert_eq!(c.folder_record("/pics/trip/day1/day1/raw"), None);
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
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
    let dir = std::env::temp_dir().join(format!("lcc-xfer-records-{}", std::process::id()));
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
