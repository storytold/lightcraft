use super::*;
use crate::{Filter, Photo, PhotoId, Source};

fn lib(kws: &[&[&str]]) -> (Catalog, Vec<PhotoId>) {
    let mut c = Catalog::new();
    let mut ids = Vec::new();
    for k in kws {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a.jpg", "JPEG", 3, 2, "2026-01-01");
        p.meta.keywords = k.iter().map(|s| s.to_string()).collect();
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    (c, ids)
}
fn kws(c: &Catalog, id: PhotoId) -> Vec<String> {
    c.photo(id).unwrap().meta.keywords.clone()
}

#[test]
fn tree_counts_photos_per_level() {
    let (c, _) = lib(&[&["travel|Italy|Rome", "beach"], &["Travel|italy"], &["travel|France", "travel|italy|rome"], &["beach"]]);
    let t = c.keyword_tree();
    assert_eq!(t.iter().map(|n| (n.name.as_str(), n.count)).collect::<Vec<_>>(), vec![("beach", 2), ("travel", 3)]);
    let travel = &t[1];
    assert_eq!(travel.children.iter().map(|n| (n.path.as_str(), n.count)).collect::<Vec<_>>(), vec![("travel|France", 1), ("travel|Italy", 3)]);
    assert_eq!(travel.children[1].children[0].path, "travel|Italy|Rome");
    assert_eq!(travel.children[1].children[0].count, 2);
    // filtering by a parent finds its children
    let f = Filter { keyword: Some("travel|italy".into()), ..Default::default() };
    assert_eq!(c.query(&f, &Default::default()).len(), 3);
}

/// A keyword written twice on a photo, or in two spellings, counts the photo once; the next
/// photo with it counts again.
#[test]
fn tree_counts_a_repeated_keyword_once_per_photo() {
    let (c, _) = lib(&[&["sea|Baltic", "sea|Baltic", "Sea|baltic"], &["sea|baltic"], &[]]);
    let t = c.keyword_tree();
    assert_eq!(tree_paths(&t), [("sea".to_string(), 2), ("sea|Baltic".to_string(), 2)]);
}

#[test]
fn rename_delete_merge_are_single_undoable_batches() {
    let (mut c, ids) = lib(&[&["travel|italy|rome", "beach"], &["Travel|Italy"], &["italia", "travel|italy"], &["sea"]]);
    let before = c.to_snapshot();
    let op = c.rename_keyword_ops("travel|italy", "Europe|Italy").unwrap();
    let Op::Batch { ops } = &op else { panic!() };
    assert_eq!(ops.len(), 3, "only photos that change");
    let inv = c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["Europe|Italy|rome", "beach"]);
    assert_eq!(kws(&c, ids[1]), ["Europe|Italy"]);
    c.apply(inv).unwrap();
    assert_eq!(c.to_snapshot(), before);
    // merge `italia` into the existing hierarchical keyword: duplicates collapse
    let op = c.merge_keywords_ops(&["italia".into()], "travel|italy").unwrap();
    c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[2]), ["travel|italy"]);
    // delete removes the keyword and its children everywhere
    let op = c.delete_keyword_ops("TRAVEL").unwrap();
    c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["beach"]);
    assert!(kws(&c, ids[1]).is_empty());
    assert_eq!(c.keywords(), vec![("beach".to_string(), 1), ("sea".to_string(), 1)]);
    // invalid requests
    assert!(c.rename_keyword_ops("beach", " ").is_err());
    assert!(c.rename_keyword_ops("a", "a|b").is_err());
    assert!(c.merge_keywords_ops(&["a".into()], "a|b").is_err());
    assert_eq!(clean(" a | |b "), "a|b");
}

fn tree_paths(nodes: &[KeywordNode]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for n in nodes {
        out.push((n.path.clone(), n.count));
        out.extend(tree_paths(&n.children));
    }
    out
}

/// A keyword can be listed before any photo has it (Lightroom Classic's Create Keyword Tag):
/// it is in the tree with no photos, its parents too, and undo takes it away.
#[test]
fn a_keyword_listed_with_no_photos_is_in_the_tree() {
    let (mut c, _) = lib(&[&["travel|France"]]);
    let undo = c.apply(Op::SetKeyword { path: " travel | Italy ".into(), info: Some(KeywordInfo::default()) }).unwrap();
    assert_eq!(tree_paths(&c.keyword_tree()), [("travel".to_string(), 1), ("travel|France".to_string(), 1), ("travel|Italy".to_string(), 0)]);
    assert_eq!(c.keyword_info("TRAVEL|italy"), Some(&KeywordInfo::default()), "found whatever the case");
    c.apply(undo).unwrap();
    assert_eq!(tree_paths(&c.keyword_tree()), [("travel".to_string(), 1), ("travel|France".to_string(), 1)]);
    assert_eq!(c.keyword_info("travel|Italy"), None);
}

/// A listed keyword needs a name, and its attributes start as Lightroom Classic's do: exported,
/// with the keywords containing it and its synonyms; not a person.
#[test]
fn a_listed_keyword_needs_a_name_and_starts_exported() {
    let mut c = Catalog::new();
    assert!(c.apply(Op::SetKeyword { path: " | ".into(), info: Some(KeywordInfo::default()) }).is_err());
    let info = KeywordInfo::default();
    assert!(info.include_on_export && info.export_containing && info.export_synonyms && !info.person && info.synonyms.is_empty());
}

/// Changing a listed keyword's attributes is undone to the attributes it had, under the name as
/// it was written.
#[test]
fn changing_a_listed_keyword_is_undone_to_what_it_was() {
    let mut c = Catalog::new();
    c.apply(Op::SetKeyword { path: "Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
    let person = KeywordInfo { person: true, synonyms: vec!["Marriage".into()], ..KeywordInfo::default() };
    let undo = c.apply(Op::SetKeyword { path: "weddings".into(), info: Some(person.clone()) }).unwrap();
    assert_eq!(c.keyword_info("Weddings"), Some(&person));
    assert_eq!(undo, Op::SetKeyword { path: "Weddings".into(), info: Some(KeywordInfo::default()) });
    c.apply(undo).unwrap();
    assert_eq!(c.keyword_info("weddings"), Some(&KeywordInfo::default()));
}

fn listed(c: &Catalog) -> Vec<(String, KeywordInfo)> {
    let mut v: Vec<(String, KeywordInfo)> = c.listed_keywords().map(|k| (k.path.clone(), k.info.clone())).collect();
    v.sort_by_key(|(p, _)| p.to_lowercase());
    v
}

fn with_synonyms(words: &[&str]) -> KeywordInfo {
    KeywordInfo { synonyms: words.iter().map(|w| w.to_string()).collect(), ..KeywordInfo::default() }
}

/// Renaming (or moving) a keyword takes its attributes and its listed children along, photos
/// or not, in the same undo step.
#[test]
fn renaming_a_keyword_carries_its_listing_along() {
    let (mut c, ids) = lib(&[&["travel|italy"]]);
    c.apply(Op::SetKeyword { path: "travel".into(), info: Some(with_synonyms(&["trip"])) }).unwrap();
    c.apply(Op::SetKeyword { path: "travel|Spain".into(), info: Some(KeywordInfo::default()) }).unwrap();
    let before = c.to_snapshot();
    let op = c.rename_keyword_ops("Travel", "Places|Europe").unwrap();
    let undo = c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["Places|Europe|italy"]);
    assert_eq!(listed(&c), [("Places|Europe".to_string(), with_synonyms(&["trip"])), ("Places|Europe|Spain".to_string(), KeywordInfo::default())]);
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
}

/// A keyword only the list holds (no photos) can be renamed too.
#[test]
fn a_keyword_without_photos_can_be_renamed() {
    let mut c = Catalog::new();
    c.apply(Op::SetKeyword { path: "weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
    let op = c.rename_keyword_ops("weddings", "Events|Weddings").unwrap();
    c.apply(op).unwrap();
    assert_eq!(listed(&c), [("Events|Weddings".to_string(), KeywordInfo::default())]);
}

/// Merging keeps the target's attributes and adds the merged keyword's synonyms to them; a target
/// not listed yet takes the merged keyword's attributes.
#[test]
fn merging_keeps_the_targets_attributes_and_gathers_synonyms() {
    let (mut c, _) = lib(&[&["holiday"], &["travel"]]);
    let person = KeywordInfo { person: true, ..with_synonyms(&["trip"]) };
    c.apply(Op::SetKeyword { path: "travel".into(), info: Some(person.clone()) }).unwrap();
    c.apply(Op::SetKeyword { path: "holiday".into(), info: Some(KeywordInfo { include_on_export: false, ..with_synonyms(&["vacation", "Trip"]) }) })
        .unwrap();
    let op = c.merge_keywords_ops(&["holiday".into()], "travel").unwrap();
    c.apply(op).unwrap();
    assert_eq!(listed(&c), [("travel".to_string(), KeywordInfo { person: true, ..with_synonyms(&["trip", "vacation"]) })]);
    // onto a keyword that isn't listed: it takes the merged one's attributes
    let mut c = Catalog::new();
    c.apply(Op::SetKeyword { path: "holiday".into(), info: Some(with_synonyms(&["vacation"])) }).unwrap();
    let op = c.merge_keywords_ops(&["holiday".into()], "Travel").unwrap();
    c.apply(op).unwrap();
    assert_eq!(listed(&c), [("Travel".to_string(), with_synonyms(&["vacation"]))]);
}

/// Deleting a keyword takes it, and the keywords below it, off the list as well as off photos.
#[test]
fn deleting_a_keyword_takes_it_off_the_list() {
    let (mut c, _) = lib(&[&["travel|italy"], &["beach"]]);
    for path in ["travel", "travel|spain", "beach"] {
        c.apply(Op::SetKeyword { path: path.into(), info: Some(KeywordInfo::default()) }).unwrap();
    }
    let before = c.to_snapshot();
    let op = c.delete_keyword_ops("travel").unwrap();
    let undo = c.apply(op).unwrap();
    assert_eq!(listed(&c), [("beach".to_string(), KeywordInfo::default())]);
    assert_eq!(tree_paths(&c.keyword_tree()), [("beach".to_string(), 1)]);
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
}

/// Creating a keyword lists it with its attributes, and can tag photos with it in the same undo
/// step. A keyword that exists already (listed, or on a photo) isn't created again.
#[test]
fn creating_a_keyword_lists_it_and_can_tag_photos() {
    let (mut c, ids) = lib(&[&["beach"], &[]]);
    let before = c.to_snapshot();
    let op = c.create_keyword_ops("Events | Weddings", with_synonyms(&["marriage"]), &[ids[1]]).unwrap();
    let undo = c.apply(op).unwrap();
    assert_eq!(listed(&c), [("Events|Weddings".to_string(), with_synonyms(&["marriage"]))]);
    assert_eq!(kws(&c, ids[1]), ["Events|Weddings"]);
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
    for exists in ["BEACH", "events|weddings"] {
        if exists == "events|weddings" {
            let op = c.create_keyword_ops("Events|Weddings", KeywordInfo::default(), &[]).unwrap();
            c.apply(op).unwrap();
        }
        assert!(c.create_keyword_ops(exists, KeywordInfo::default(), &[]).is_err(), "{exists}");
    }
    assert!(c.create_keyword_ops(" | ", KeywordInfo::default(), &[]).is_err());
}

/// Moving a keyword nests it inside another (or takes it to the top level), with its children,
/// on photos and in the list. It can't go inside itself or one of its children, and moving it
/// where it already is changes nothing.
#[test]
fn moving_a_keyword_nests_it_inside_another() {
    let (mut c, ids) = lib(&[&["Italy|Rome"], &["Europe"]]);
    // typed in any case, the keywords keep the spelling the library has
    let op = c.move_keyword_ops("italy", Some("EUROPE"), false).unwrap();
    c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["Europe|Italy|Rome"]);
    let op = c.move_keyword_ops("europe|italy", None, false).unwrap();
    c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["Italy|Rome"], "back at the top level");
    assert!(c.move_keyword_ops("italy", Some("italy|rome"), false).is_err(), "inside its own child");
    assert!(c.move_keyword_ops("italy", Some("Italy"), false).is_err(), "inside itself");
    assert_eq!(c.move_keyword_ops("italy|rome", Some("italy"), false).unwrap(), Op::Batch { ops: vec![] }, "already there");
    assert!(c.move_keyword_ops("lisbon", Some("europe"), false).is_err(), "no such keyword");
}

/// Moving onto a name that is taken (Europe already has a Rome) merges the two, so the move
/// asks first: refused unless merging is allowed.
#[test]
fn moving_onto_a_taken_name_merges_only_when_allowed() {
    let (mut c, ids) = lib(&[&["rome"], &["europe|rome"]]);
    let err = c.move_keyword_ops("rome", Some("europe"), false).unwrap_err();
    assert!(matches!(err, CatalogError::KeywordExists(ref k) if k == "europe|rome"), "{err:?}");
    let op = c.move_keyword_ops("rome", Some("europe"), true).unwrap();
    c.apply(op).unwrap();
    assert_eq!((kws(&c, ids[0]), kws(&c, ids[1])), (vec!["europe|rome".to_string()], vec!["europe|rome".to_string()]));
}

/// Editing a keyword renames it (its last level: it stays where it is) and sets its attributes,
/// in one undo step. A name that is taken is refused: merging is its own action.
#[test]
fn editing_a_keyword_renames_it_and_sets_its_attributes() {
    let (mut c, ids) = lib(&[&["travel|italy"], &["travel|spain"]]);
    let before = c.to_snapshot();
    let person = KeywordInfo { person: true, ..KeywordInfo::default() };
    let op = c.edit_keyword_ops("travel|italy", "Italia", person.clone()).unwrap();
    let undo = c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["travel|Italia"]);
    assert_eq!(c.keyword_info("travel|italia"), Some(&person));
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
    // attributes alone
    let op = c.edit_keyword_ops("travel", "travel", with_synonyms(&["trip"])).unwrap();
    c.apply(op).unwrap();
    assert_eq!(listed(&c), [("travel".to_string(), with_synonyms(&["trip"]))]);
    assert!(matches!(c.edit_keyword_ops("travel|italy", "Spain", KeywordInfo::default()), Err(CatalogError::KeywordExists(_))));
    assert!(c.edit_keyword_ops("travel|italy", "a|b", KeywordInfo::default()).is_err(), "a name, not a path");
    assert!(c.edit_keyword_ops("travel|italy", "  ", KeywordInfo::default()).is_err());
}

/// Purging takes off the list the keywords no photo has (nor any keyword below them), in one
/// undo step; keywords photos carry stay.
#[test]
fn purging_takes_unused_keywords_off_the_list() {
    let (mut c, _) = lib(&[&["travel|italy"]]);
    for path in ["travel", "travel|italy", "travel|spain", "weddings"] {
        c.apply(Op::SetKeyword { path: path.into(), info: Some(KeywordInfo::default()) }).unwrap();
    }
    let op = c.purge_unused_keywords_ops();
    c.apply(op).unwrap();
    assert_eq!(listed(&c).iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["travel", "travel|italy"]);
    assert_eq!(tree_paths(&c.keyword_tree()), [("travel".to_string(), 1), ("travel|italy".to_string(), 1)]);
}

/// Renaming matches names whatever their case, also where a letter's case changes its length
/// (ẞ is three bytes, ß two): the levels below are kept whole, never cut mid-letter.
#[test]
fn renaming_keeps_the_levels_below_whatever_the_case() {
    let (mut c, ids) = lib(&[&["straße|nord"], &["ßa|ü"]]);
    let op = c.rename_keyword_ops("STRAẞE", "Road").unwrap();
    c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["Road|nord"]);
    let op = c.rename_keyword_ops("ẞA", "Weg").unwrap();
    c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[1]), ["Weg|ü"]);
}

fn exported(c: &Catalog, keywords: &[&str]) -> (Vec<String>, Vec<String>) {
    let e = c.export_keywords(&keywords.iter().map(|k| k.to_string()).collect::<Vec<_>>());
    (e.flat, e.hierarchical)
}

/// By default a keyword is exported as its name together with the keywords containing it (flat,
/// `dc:subject`), and as its full path (`lr:hierarchicalSubject`); each name once.
#[test]
fn a_keyword_exports_its_name_its_parents_and_its_path() {
    let c = Catalog::new();
    let (flat, paths) = exported(&c, &["travel|Italy|Rome", "Travel|Spain", "beach"]);
    assert_eq!(flat, ["Rome", "Italy", "travel", "Spain", "beach"]);
    assert_eq!(paths, ["travel|Italy|Rome", "Travel|Spain", "beach"]);
}

/// A keyword not included on export is left out, path and all; a parent left out (a keyword that
/// only organizes others) isn't exported as a containing keyword, but its children are.
#[test]
fn keywords_left_out_of_export_stay_out() {
    let mut c = Catalog::new();
    let out = KeywordInfo { include_on_export: false, ..KeywordInfo::default() };
    c.apply(Op::SetKeyword { path: "Places".into(), info: Some(out.clone()) }).unwrap();
    c.apply(Op::SetKeyword { path: "draft".into(), info: Some(out) }).unwrap();
    let (flat, paths) = exported(&c, &["Places|Lisbon", "draft"]);
    assert_eq!(flat, ["Lisbon"]);
    assert_eq!(paths, ["Places|Lisbon"]);
}

/// Without "export containing keywords" only the keyword's own name goes out flat; synonyms go
/// out with it unless "export synonyms" is off.
#[test]
fn containing_keywords_and_synonyms_follow_their_options() {
    let mut c = Catalog::new();
    let alone = KeywordInfo { export_containing: false, ..with_synonyms(&["Lisboa"]) };
    c.apply(Op::SetKeyword { path: "Places|Lisbon".into(), info: Some(alone) }).unwrap();
    let quiet = KeywordInfo { export_synonyms: false, ..with_synonyms(&["marriage"]) };
    c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(quiet) }).unwrap();
    let (flat, _) = exported(&c, &["Places|Lisbon", "Events|Weddings"]);
    assert_eq!(flat, ["Lisbon", "Lisboa", "Weddings", "Events"]);
}

/// Names match whatever the case of any letter, not only ASCII ones: changing only the case of
/// "Ärzte" is a rename, not a clash with itself, and moving it where it is changes nothing.
#[test]
fn names_match_whatever_the_case_of_any_letter() {
    let (mut c, ids) = lib(&[&["Ärzte|Wien"]]);
    let op = c.edit_keyword_ops("ärzte", "ÄRZTE", KeywordInfo::default()).unwrap();
    c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["ÄRZTE|Wien"]);
    assert_eq!(c.move_keyword_ops("ärzte|wien", Some("ärzte"), false).unwrap(), Op::Batch { ops: vec![] });
    let op = c.rename_keyword_ops("ärzte", "Ärzte").unwrap();
    c.apply(op).unwrap();
    assert_eq!(kws(&c, ids[0]), ["Ärzte|Wien"]);
    assert!(c.create_keyword_ops("ärzte", KeywordInfo::default(), &[]).is_err(), "exists");
}

/// A keyword that is only the parent of a listed one ("Events" of "Events|Weddings") is a
/// keyword like any other: it is in the tree, so it can be found, moved and renamed.
#[test]
fn the_parent_of_a_listed_keyword_is_a_keyword() {
    let mut c = Catalog::new();
    c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
    assert!(c.has_keyword("events"));
    assert_eq!(c.keyword_path("EVENTS").as_deref(), Some("Events"));
    let op = c.edit_keyword_ops("events", "Occasions", KeywordInfo::default()).unwrap();
    c.apply(op).unwrap();
    assert_eq!(tree_paths(&c.keyword_tree()), [("Occasions".to_string(), 0), ("Occasions|Weddings".to_string(), 0)]);
}

/// Photos in Recently Deleted don't count: a keyword only they have isn't in the tree, so it
/// can be created again and is purged as unused, and its spelling there isn't the library's.
#[test]
fn deleted_photos_dont_hold_keywords() {
    let (mut c, ids) = lib(&[&["Weddings"], &["travel"]]);
    c.apply(Op::SetDeleted { id: ids[0], deleted: true }).unwrap();
    assert!(!c.has_keyword("weddings"));
    assert!(c.create_keyword_ops("weddings", KeywordInfo::default(), &[]).is_ok());
    c.apply(Op::SetKeyword { path: "Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
    let op = c.purge_unused_keywords_ops();
    c.apply(op).unwrap();
    assert!(listed(&c).is_empty(), "only a deleted photo has it: unused");
}

/// Merging several keywords moves each keyword under them by the closest one, whatever the
/// order they are given in, and every listing lands where its keyword does: one landing where
/// another is about to leave isn't merged into it.
#[test]
fn merging_overlapping_keywords_keeps_every_listing() {
    let (mut c, ids) = lib(&[&["a|b|c"], &["a|c"]]);
    let person = KeywordInfo { person: true, ..KeywordInfo::default() };
    c.apply(Op::SetKeyword { path: "a|b|c".into(), info: Some(person.clone()) }).unwrap();
    c.apply(Op::SetKeyword { path: "a|c".into(), info: Some(with_synonyms(&["sea"])) }).unwrap();
    let op = c.merge_keywords_ops(&["a|b".into(), "a|c".into()], "a").unwrap();
    c.apply(op).unwrap();
    assert_eq!((kws(&c, ids[0]), kws(&c, ids[1])), (vec!["a|c".to_string()], vec!["a".to_string()]));
    assert_eq!(listed(&c), [("a".to_string(), with_synonyms(&["sea"])), ("a|c".to_string(), person)]);
    // the order of the keywords merged doesn't matter
    let merged = |from: [&str; 2]| {
        let (mut c, ids) = lib(&[&["a|b|c", "a|d"]]);
        let op = c.merge_keywords_ops(&from.map(String::from), "x").unwrap();
        c.apply(op).unwrap();
        kws(&c, ids[0])
    };
    assert_eq!(merged(["a", "a|b"]), merged(["a|b", "a"]));
    assert_eq!(merged(["a", "a|b"]), ["x|c", "x|d"]);
}

/// One keyword at most is where new keywords go (Put New Keywords Inside This Keyword). Making
/// another one so is one undo step; the mark moves with its keyword (also into a merge) and
/// goes with it.
#[test]
fn one_keyword_is_where_new_keywords_go() {
    let (mut c, _) = lib(&[&["Events|Weddings"], &["Travel"]]);
    let op = c.set_default_parent_ops(Some("events")).unwrap();
    let undo = c.apply(op).unwrap();
    assert_eq!(c.default_keyword_parent().as_deref(), Some("Events"));
    let op = c.set_default_parent_ops(Some("travel")).unwrap();
    c.apply(op).unwrap();
    assert_eq!(c.default_keyword_parent().as_deref(), Some("Travel"), "only one");
    assert!(c.set_default_parent_ops(Some("lisbon")).is_err());
    let op = c.rename_keyword_ops("travel", "Trips").unwrap();
    c.apply(op).unwrap();
    assert_eq!(c.default_keyword_parent().as_deref(), Some("Trips"));
    let op = c.merge_keywords_ops(&["trips".into()], "Events").unwrap();
    c.apply(op).unwrap();
    assert_eq!(c.default_keyword_parent().as_deref(), Some("Events"), "into the merge");
    let op = c.delete_keyword_ops("events").unwrap();
    c.apply(op).unwrap();
    assert_eq!(c.default_keyword_parent(), None);
    let op = c.set_default_parent_ops(None).unwrap();
    assert_eq!(op, Op::Batch { ops: vec![] }, "none to clear");
    c.apply(undo).unwrap();
    assert_eq!(c.default_keyword_parent(), None, "undo of the first: none was");
}

/// The keyword shortcut is a mark on its keywords: setting it is one undo step (a new name is
/// listed), and the mark moves with its keyword (also into a merge), goes with it, and comes
/// back with undo.
#[test]
fn the_keyword_shortcut_moves_with_its_keywords() {
    let (mut c, _) = lib(&[&["Events|Weddings"], &["Travel"]]);
    let op = c.set_keyword_shortcut_ops(&["travel".into(), "Lisbon".into()]);
    let undo = c.apply(op).unwrap();
    assert_eq!(c.keyword_shortcut(), ["Lisbon", "Travel"], "spelled as the library spells it; a new name listed");
    let op = c.set_keyword_shortcut_ops(&["events|weddings".into(), "Travel".into()]);
    c.apply(op).unwrap();
    assert_eq!(c.keyword_shortcut(), ["Events|Weddings", "Travel"], "the old marks are cleared");
    let op = c.rename_keyword_ops("travel", "Trips").unwrap();
    let undo_rename = c.apply(op).unwrap();
    assert_eq!(c.keyword_shortcut(), ["Events|Weddings", "Trips"]);
    c.apply(undo_rename).unwrap();
    assert_eq!(c.keyword_shortcut(), ["Events|Weddings", "Travel"], "undo moves it back");
    let op = c.set_keyword_shortcut_ops(&["Travel".into()]);
    c.apply(op).unwrap();
    let op = c.merge_keywords_ops(&["travel".into()], "Events|Weddings").unwrap();
    c.apply(op).unwrap();
    assert_eq!(c.keyword_shortcut(), ["Events|Weddings"], "into the merge");
    let op = c.delete_keyword_ops("events").unwrap();
    c.apply(op).unwrap();
    assert!(c.keyword_shortcut().is_empty());
    assert_eq!(c.set_keyword_shortcut_ops(&[]), Op::Batch { ops: vec![] }, "none to clear");
    c.apply(undo).unwrap();
    assert!(c.keyword_shortcut().is_empty(), "undo of the first: there was none");
}

/// A file's keywords as a photo carries them: the paths of `lr:hierarchicalSubject`, and the
/// names of `dc:subject` that aren't levels of those paths (an exported `travel|Italy` comes
/// back as one keyword, not three). Without paths, the names as they are.
#[test]
fn a_files_keywords_keep_their_hierarchy() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(from_file(&v(&["Rome", "Italy", "travel", "beach"]), &v(&["travel|Italy|Rome"])), ["travel|Italy|Rome", "beach"]);
    assert_eq!(from_file(&v(&["ROME"]), &v(&["travel|Rome", " travel | rome "])), ["travel|Rome"], "each once, cleaned");
    assert_eq!(from_file(&v(&["beach", " sea "]), &[]), ["beach", "sea"]);
    assert_eq!(from_file(&v(&["a|b"]), &[]), ["a|b"], "sidecars from LightCraft 0.13 and older wrote paths flat");
}

/// A sidecar's keywords: names in `dc:subject`, never paths, and the paths in
/// `lr:hierarchicalSubject`; read back, the same keywords, however they were spelt or nested.
#[test]
fn a_sidecar_writes_names_flat_and_paths_hierarchical() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let f = to_file(&v(&["Places|Italy|Rome", "sunset"]));
    assert_eq!(f.flat, ["Places", "Italy", "Rome", "sunset"]);
    assert_eq!(f.hierarchical, ["Places|Italy|Rome", "sunset"]);
    assert!(f.flat.iter().all(|k| !k.contains(SEP)), "no path in dc:subject");

    let f = to_file(&v(&["beach", " sea ", "Beach"]));
    assert_eq!((f.flat, f.hierarchical), (v(&["beach", "sea"]), v(&[])), "no hierarchy: no lr:hierarchicalSubject");
    assert_eq!(to_file(&[]), FileKeywords::default());

    for keywords in [
        v(&["Places|Italy|Rome", "sunset"]),
        v(&["travel|Italy|Rome", "Rome"]),
        v(&["travel|Italy", "travel|France", "Italy"]),
        v(&["beach", "sea"]),
        v(&[" a | b ", "A|B|c"]),
    ] {
        let f = to_file(&keywords);
        let mut back = from_file(&f.flat, &f.hierarchical);
        let mut want: Vec<String> = keywords.iter().map(|k| clean(k)).collect();
        dedupe(&mut want);
        back.sort_by_key(|k| k.to_lowercase());
        want.sort_by_key(|k| k.to_lowercase());
        assert_eq!(back, want, "round trip of {keywords:?}");
    }
}

/// A new keyword inside one the library has takes the library's spelling of it: creating
/// "TRAVEL|rome" next to "travel|italy" doesn't respell "travel".
#[test]
fn a_new_keyword_keeps_the_spelling_of_its_parents() {
    let (mut c, _) = lib(&[&["travel|italy"]]);
    let op = c.create_keyword_ops("TRAVEL|Rome", KeywordInfo::default(), &[]).unwrap();
    c.apply(op).unwrap();
    assert_eq!(listed(&c).iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["travel|Rome"]);
    assert_eq!(tree_paths(&c.keyword_tree())[0].0, "travel");
}

/// An edit that changes nothing is nothing: no listing, no undo step.
#[test]
fn an_edit_that_changes_nothing_is_nothing() {
    let (mut c, _) = lib(&[&["beach"]]);
    assert_eq!(c.edit_keyword_ops("beach", "beach", KeywordInfo::default()).unwrap(), Op::Batch { ops: vec![] });
    c.apply(Op::SetKeyword { path: "beach".into(), info: Some(with_synonyms(&["shore"])) }).unwrap();
    assert_eq!(c.edit_keyword_ops("BEACH", "beach", with_synonyms(&["shore"])).unwrap(), Op::Batch { ops: vec![] });
    // renamed with the default attributes: the photos change, nothing is listed
    let (mut c, _) = lib(&[&["beach"]]);
    let op = c.edit_keyword_ops("beach", "Shore", KeywordInfo::default()).unwrap();
    c.apply(op).unwrap();
    assert!(listed(&c).is_empty());
}

/// Typing suggests keywords no photo has yet too (created in the Keyword List).
#[test]
fn suggestions_include_keywords_without_photos() {
    let (mut c, _) = lib(&[&["beach"]]);
    c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
    assert_eq!(c.keyword_suggestions(&[], "wed", 5), ["Events|Weddings"]);
    assert!(!c.keyword_suggestions(&["events|weddings".into()], "wed", 5).contains(&"Events|Weddings".to_string()), "not one it has");
}

/// A new name typed for a photo goes inside the default parent; a keyword the library has, and
/// a path typed whole, don't.
#[test]
fn typed_new_keywords_go_inside_the_default_parent() {
    let (mut c, _) = lib(&[&["beach"], &["Events"]]);
    assert_eq!(c.typed_keyword(" Weddings "), "Weddings", "no default parent");
    let op = c.set_default_parent_ops(Some("events")).unwrap();
    c.apply(op).unwrap();
    assert_eq!(c.typed_keyword("Weddings"), "Events|Weddings");
    assert_eq!(c.typed_keyword("BEACH"), "BEACH", "the library has it");
    assert_eq!(c.typed_keyword("Places|Lisbon"), "Places|Lisbon");
}

/// The keyword list as a text file, as Lightroom Classic writes one: a keyword a line, the
/// keywords inside it indented one tab more, a synonym in braces on a line of its own under its
/// keyword, a keyword left out of export in square brackets; by name.
#[test]
fn the_keyword_list_is_written_as_tab_indented_text() {
    let (mut c, _) = lib(&[&["Places|Lisbon", "beach"]]);
    c.apply(Op::SetKeyword { path: "Places".into(), info: Some(KeywordInfo { include_on_export: false, ..KeywordInfo::default() }) }).unwrap();
    c.apply(Op::SetKeyword { path: "Places|Lisbon".into(), info: Some(with_synonyms(&["Lisboa"])) }).unwrap();
    c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
    assert_eq!(c.keyword_list_text().text, "beach\nEvents\n\tWeddings\n[Places]\n\tLisbon\n\t\t{Lisboa}\n");
}

/// Reading a list gives each keyword's path and attributes: what was written comes back, and a
/// file from elsewhere (Windows line ends, a byte-order mark, blank lines) reads too.
#[test]
fn a_keyword_list_reads_back() {
    let read = parse_keyword_list("\u{feff}beach\r\n\r\nEvents\r\n\tWeddings\r\n\t\t{marriage}\r\n[Places]\r\n\tLisbon\r\n").unwrap();
    let out = KeywordInfo { include_on_export: false, ..KeywordInfo::default() };
    assert_eq!(
        read,
        [
            ("beach".to_string(), KeywordInfo::default()),
            ("Events".to_string(), KeywordInfo::default()),
            ("Events|Weddings".to_string(), with_synonyms(&["marriage"])),
            ("Places".to_string(), out),
            ("Places|Lisbon".to_string(), KeywordInfo::default()),
        ]
    );
}

/// Photo Supreme's Formatted Vocabulary File is the same format: plain text with Windows line
/// ends and no byte-order mark, a top-level category in square brackets (an organizing keyword,
/// left out of export), keywords a tab further in at each level.
#[test]
fn a_photo_supreme_vocabulary_file_reads() {
    let file = "[Places]\r\n\tPortugal\r\n\t\tLisbon\r\n\t\t\tAlfama\r\n\t\tPorto\r\n\tSpain\r\n\t\tSeville\r\n";
    let read = parse_keyword_list(file).unwrap();
    let paths: Vec<&str> = read.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        ["Places", "Portugal", "Portugal|Lisbon", "Portugal|Lisbon|Alfama", "Portugal|Porto", "Spain", "Spain|Seville"].map(|p| if p == "Places" {
            p.to_string()
        } else {
            format!("Places|{p}")
        })
    );
    assert!(!read[0].1.include_on_export, "the category organizes: it isn't exported");
    assert!(read[1..].iter().all(|(_, i)| i.include_on_export));
}

/// A list that can't be read says which line and why, and nothing of it is taken.
#[test]
fn a_bad_keyword_list_says_which_line() {
    let err = |text: &str| parse_keyword_list(text).unwrap_err().to_string();
    assert!(err("Events\n\t\tWeddings\n").contains("line 2"), "two levels down at once");
    assert!(err("{marriage}\n").contains("line 1"), "a synonym of nothing");
    assert!(err("Events\nTravel|Lisbon\n").contains("line 2"), "“|” separates levels here");
    let deep: String = (0..70).map(|i| format!("{}k{i}\n", "\t".repeat(i))).collect();
    assert!(err(&deep).contains("line 65"), "a file is input: 64 levels at most");
}

/// Files from elsewhere read as they were meant or say why not: classic Mac line ends (a lone
/// carriage return) are lines; indenting with spaces, which would flatten the hierarchy, is
/// refused at its line; so are control characters in a name and "|" in a synonym.
#[test]
fn a_keyword_list_from_elsewhere_reads_or_says_why_not() {
    let paths = |text: &str| parse_keyword_list(text).unwrap().into_iter().map(|(p, _)| p).collect::<Vec<_>>();
    assert_eq!(paths("Events\r\tWeddings\rPlaces\r"), ["Events", "Events|Weddings", "Places"]);
    let err = |text: &str| parse_keyword_list(text).unwrap_err().to_string();
    assert!(err("Events\n    Weddings\n").contains("line 2"), "space indentation");
    assert!(err("Events\n\t Weddings\n").contains("line 2"), "a space after the tabs");
    assert!(err("Events\nx\u{0}y\n").contains("line 2"), "a control character");
    assert!(err("Events\n\t{sea|shore}\n").contains("line 2"), "“|” in a synonym");
}

/// A keyword repeated in another case is one keyword, and the keywords inside either take the
/// first spelling.
#[test]
fn a_repeated_keyword_keeps_its_first_spelling() {
    let read = parse_keyword_list("Events\n\tWeddings\nevents\n\tBirthdays\n").unwrap();
    assert_eq!(read.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["Events", "Events|Weddings", "Events|Birthdays"]);
}

/// What the list can't hold is left out and named, so the file always reads back: a name the
/// format would read as something else (in brackets or braces), one with a line break, a
/// synonym with one, a keyword nested deeper than a list goes; and the keywords inside one left
/// out go with it.
#[test]
fn an_exported_keyword_list_always_reads_back() {
    let deep = (0..=MAX_LEVELS).map(|i| format!("level {i}")).collect::<Vec<_>>().join("|");
    let (mut c, _) = lib(&[&["Travel|[draft]", "{todo}|Lisbon", "Weddings", "Line\nbreak", deep.as_str()]]);
    c.apply(Op::SetKeyword { path: "Weddings".into(), info: Some(with_synonyms(&["marriage", "two\nlines"])) }).unwrap();
    let KeywordListFile { text, unwritable, .. } = c.keyword_list_text();
    let read = parse_keyword_list(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(read.iter().any(|(p, _)| p == "Travel"));
    assert!(read.iter().all(|(p, _)| !p.contains("draft") && !p.contains("todo") && !p.contains("Line")), "{text}");
    let weddings = read.iter().find(|(p, _)| p == "Weddings").map(|(_, i)| i.synonyms.clone());
    assert_eq!(weddings, Some(vec!["marriage".to_string()]));
    let deepest = read.iter().map(|(p, _)| p.split(SEP).count()).max();
    assert_eq!(deepest, Some(MAX_LEVELS));
    assert!(unwritable.contains(&"Travel|[draft]".to_string()), "{unwritable:?}");
    assert!(unwritable.contains(&"{todo}".to_string()), "{unwritable:?}");
    assert!(unwritable.contains(&"Line\nbreak".to_string()), "{unwritable:?}");
    assert!(unwritable.contains(&"Weddings {two\nlines}".to_string()), "{unwritable:?}");
    assert!(unwritable.iter().any(|u| u.ends_with(&format!("level {MAX_LEVELS}"))), "{unwritable:?}");
}

/// The list leaves out only what it must, and names only what's lost: a name starting with a
/// byte-order mark would lose it, but a synonym in braces reads back, and a synonym that's empty
/// or another's in a different case is no loss. It also says which keywords it wrote.
#[test]
fn an_exported_keyword_list_names_only_what_it_loses() {
    let (mut c, _) = lib(&[&["\u{feff}Albums", "{draft", "Weddings"]]);
    c.apply(Op::SetKeyword { path: "Weddings".into(), info: Some(with_synonyms(&["Marriage", "marriage", " ", "{vows}"])) }).unwrap();
    let out = c.keyword_list_text();
    let read = parse_keyword_list(&out.text).unwrap_or_else(|e| panic!("{e}\n{}", out.text));
    assert_eq!(out.unwritable, ["\u{feff}Albums"]);
    assert_eq!(out.written, ["Weddings", "{draft"]);
    assert_eq!(read.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["Weddings", "{draft"]);
    let weddings = read.iter().find(|(p, _)| p == "Weddings").map(|(_, i)| i.synonyms.clone());
    assert_eq!(weddings, Some(vec!["Marriage".to_string(), "{vows}".to_string()]));
}

/// A big list reads and imports at once, against a big library too: 40,000 keywords used to take
/// half a minute to read, and checking 5,000 new ones against 20,000 photos over a minute, with
/// the app frozen.
#[test]
fn a_big_keyword_list_imports_quickly() {
    let mut text = String::new();
    for g in 0..400 {
        text.push_str(&format!("group {g}\n"));
        for k in 0..99 {
            text.push_str(&format!("\tkeyword {g}-{k}\n"));
        }
    }
    let t0 = std::time::Instant::now();
    let read = parse_keyword_list(&text).unwrap();
    assert_eq!(read.len(), 40_000);
    let mut c = Catalog::new();
    let mut ops = Vec::new();
    for i in 0..20_000u32 {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a.jpg", "JPEG", 3, 2, "2026-01-01");
        p.meta.keywords = (0..5).map(|k| format!("library {}|tag {}", k, (i + k) % 3000)).collect();
        ops.push(Op::AddPhoto { photo: Box::new(p) });
    }
    c.apply(Op::Batch { ops }).unwrap();
    let (op, added, _) = c.import_keywords_ops(&read[..5_000]);
    c.apply(op).unwrap();
    assert_eq!(added, 5_000);
    let took = t0.elapsed();
    assert!(took < std::time::Duration::from_secs(10), "{took:?}");
}

/// Importing a list adds the keywords the library doesn't have, with their attributes, and gives
/// those it has the list's synonyms (their own attributes stay), in one undo step.
#[test]
fn importing_a_keyword_list_adds_what_is_missing() {
    let (mut c, _) = lib(&[&["Events|Weddings"]]);
    c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(KeywordInfo { person: true, ..with_synonyms(&["marriage"]) }) }).unwrap();
    let before = c.to_snapshot();
    let read = parse_keyword_list("events\n\tweddings\n\t\t{nuptials}\n\tBirthdays\n[Drafts]\n").unwrap();
    let (op, added, updated) = c.import_keywords_ops(&read);
    assert_eq!((added, updated), (2, 1));
    let undo = c.apply(op).unwrap();
    assert_eq!(c.keyword_info("Events|Weddings"), Some(&KeywordInfo { person: true, ..with_synonyms(&["marriage", "nuptials"]) }));
    assert!(c.has_keyword("Events|Birthdays"));
    assert_eq!(
        listed(&c).iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        ["Drafts", "Events|Birthdays", "Events|Weddings"],
        "in the library's spelling"
    );
    assert!(!c.keyword_info("Drafts").unwrap().include_on_export);
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
}

#[test]
fn suggestions_rank_co_occurrence_and_prefixes() {
    let (c, _) = lib(&[&["dog", "park"], &["dog", "park", "ball"], &["dog", "beach"], &["cat"], &["cat"], &["cat"], &["parade"], &["eagle"]]);
    // co-occurring with `dog` first (park twice), then the most used
    let s = c.keyword_suggestions(&["dog".into()], "", 3);
    assert_eq!(s, ["park", "ball", "beach"]);
    let s = c.keyword_suggestions(&[], "", 2);
    assert_eq!(s, ["cat", "dog"]);
    let s = c.keyword_suggestions(&["park".into()], "pa", 5);
    assert_eq!(s, ["parade"]);
    let s = c.keyword_suggestions(&[], "E", 5);
    assert_eq!(s, ["eagle", "beach", "parade"], "prefix matches before substring matches");
}

/// A flat keyword already covered by a hierarchical level only in another spelling is not kept
/// again, whatever the size of either list (the per-flat-keyword search of every level, twice
/// lower-cased per comparison, was quadratic: CWE-407).
#[test]
fn from_file_matches_levels_in_any_case() {
    let flat = ["Rome".to_string(), "Seaside".to_string()];
    let hierarchical = ["travel|ITALY|rome".to_string()];
    assert_eq!(from_file(&flat, &hierarchical), ["travel|ITALY|rome", "Seaside"]);
}

/// The same synonym, in any spelling, is kept once (the search of each keyword's growing
/// synonym list per line was quadratic, with lower-cased copies made per comparison).(CWE-407)
#[test]
fn parse_keyword_list_keeps_a_synonym_once() {
    let read = parse_keyword_list("Travel\n\t{Trip}\n\t{journey}\n\t{TRIP}\n").unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].1.synonyms, ["Trip", "journey"]);
}

/// A keyword split into far more levels than a list file accepts is cut at [`MAX_LEVELS`] in the
/// tree, whose level rows hold every prefix twice: unbounded it made the stored bytes quadratic
/// in the level count of one short keyword (CWE-400).
#[test]
fn keyword_tree_caps_a_keyword_at_the_list_depth() {
    let deep: Vec<String> = (0..4096).map(|i| format!("k{i}")).collect();
    let joined = deep.join("|");
    let (c, _) = lib(&[&[joined.as_str()]]);
    let deepest = tree_paths(&c.keyword_tree()).iter().map(|(p, _)| p.split(SEP).count()).max().unwrap_or(0);
    assert_eq!(deepest, MAX_LEVELS);
}
