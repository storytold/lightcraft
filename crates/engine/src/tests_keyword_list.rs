//! The keyword list's commands (Lightroom Classic's Keyword List): create, edit, move and purge
//! keywords, their attributes, and the default parent for new keywords.

use serde_json::json;

use crate::Session;

/// The keyword filter follows a renamed keyword whatever the case it was typed in, also where a
/// letter's case changes its length (ẞ / ß): it used to be cut by bytes, which could land inside
/// a letter and panic.
#[test]
fn the_filter_follows_a_rename_whatever_the_case() {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0].0;
    s.execute("photo.setMeta", &json!({"ids": [id], "keywords": ["ßßa|é"]})).unwrap();
    s.execute("library.filter", &json!({"keyword": "ßßa|é"})).unwrap();
    s.execute("keyword.rename", &json!({"from": "ẞẞA", "to": "Road"})).unwrap();
    assert_eq!(s.filter.keyword.as_deref(), Some("Road|é"));
    s.execute("keyword.merge", &json!({"from": ["ROAD"], "into": "Weg"})).unwrap();
    assert_eq!(s.filter.keyword.as_deref(), Some("Weg|é"));
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-keyword-list-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn keywords_of(s: &Session, id: u64) -> Vec<String> {
    s.catalog.photo(dac_catalog::PhotoId(id)).unwrap().meta.keywords.clone()
}

/// Create Keyword: a name inside a parent, with attributes, given to the selected photos at once,
/// in one undo step; `keyword.info` reads it back. A keyword that exists isn't created again.
#[test]
fn creating_a_keyword_with_attributes_and_the_selection() {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0].0;
    s.execute("library.select", &json!({"ids": [id]})).unwrap();
    let undo = s.undo.len();
    let r = s
        .execute("keyword.create", &json!({"name": "Weddings", "parent": "Events", "synonyms": ["marriage"], "person": false, "addToSelected": true}))
        .unwrap();
    assert_eq!(r["keyword"], "Events|Weddings", "{r}");
    assert_eq!(s.undo.len(), undo + 1, "one undo step");
    assert!(keywords_of(&s, id).contains(&"Events|Weddings".to_string()));
    let info = s.execute("keyword.info", &json!({"keyword": "events|weddings"})).unwrap();
    assert_eq!(info["path"], "Events|Weddings");
    assert_eq!(info["synonyms"], json!(["marriage"]));
    assert_eq!((info["includeOnExport"].as_bool(), info["listed"].as_bool(), info["count"].as_u64()), (Some(true), Some(true), Some(1)));
    let err = s.execute("keyword.create", &json!({"name": "weddings", "parent": "events"})).unwrap_err().to_string();
    assert!(err.contains("already"), "{err}");
    assert!(s.execute("keyword.info", &json!({"keyword": "lisbon"})).is_err(), "no such keyword");
}

/// "Put new keywords inside this keyword": new keywords go inside the default parent unless the
/// command says otherwise (`parent: null` = the top level). It belongs to its keyword in the
/// library: it follows a rename, goes with a delete, and undo and redo bring it back with them.
#[test]
fn new_keywords_go_inside_the_default_parent() {
    let dir = temp_dir("default-parent");
    let mut s = Session::new();
    s.open_library(&dir, true).unwrap();
    s.execute("keyword.create", &json!({"name": "Events"})).unwrap();
    s.execute("keyword.setDefaultParent", &json!({"keyword": "events"})).unwrap();
    assert_eq!(s.catalog.default_keyword_parent().as_deref(), Some("Events"));
    assert_eq!(s.execute("keyword.create", &json!({"name": "Birthdays"})).unwrap()["keyword"], "Events|Birthdays");
    assert_eq!(s.execute("keyword.create", &json!({"name": "Travel", "parent": null})).unwrap()["keyword"], "Travel");
    s.execute("keyword.rename", &json!({"from": "Events", "to": "Occasions"})).unwrap();
    assert_eq!(s.catalog.default_keyword_parent().as_deref(), Some("Occasions"), "follows the rename");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.default_keyword_parent().as_deref(), Some("Events"), "undo takes it back");
    s.execute("edit.redo", &json!({})).unwrap();
    drop(s);
    let mut s = Session::new();
    s.open_library(&dir, false).unwrap();
    assert_eq!(s.catalog.default_keyword_parent().as_deref(), Some("Occasions"), "kept with the library");
    s.execute("keyword.delete", &json!({"keyword": "occasions"})).unwrap();
    assert_eq!(s.catalog.default_keyword_parent(), None, "gone with its keyword");
    assert_eq!(s.execute("keyword.create", &json!({"name": "Graduations"})).unwrap()["keyword"], "Graduations", "at the top level");
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.default_keyword_parent().as_deref(), Some("Occasions"), "undoing the delete brings it back");
    s.execute("keyword.setDefaultParent", &json!({"keyword": null})).unwrap();
    assert_eq!(s.catalog.default_keyword_parent(), None);
    assert!(s.execute("keyword.setDefaultParent", &json!({"keyword": "lisbon"})).is_err(), "no such keyword");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Edit Keyword: a new name and attributes in one step; attributes not given keep their values.
#[test]
fn editing_a_keyword_changes_what_it_is_given() {
    let mut s = Session::with_demo();
    s.execute("keyword.create", &json!({"name": "Weddings", "synonyms": ["marriage"]})).unwrap();
    let undo = s.undo.len();
    s.execute("keyword.edit", &json!({"keyword": "weddings", "name": "Marriages", "includeOnExport": false})).unwrap();
    assert_eq!(s.undo.len(), undo + 1);
    let info = s.execute("keyword.info", &json!({"keyword": "Marriages"})).unwrap();
    assert_eq!((info["includeOnExport"].as_bool(), info["synonyms"].clone()), (Some(false), json!(["marriage"])));
    assert!(s.execute("keyword.info", &json!({"keyword": "Weddings"})).is_err());
}

/// Move Keyword: inside another or to the top level. Onto a name that is taken it is refused with
/// a message saying it would merge, unless `merge: true`.
#[test]
fn moving_a_keyword_merges_only_when_asked() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(2).map(|p| p.0).collect();
    s.execute("photo.setMeta", &json!({"ids": [ids[0]], "keywords": ["Rome"]})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [ids[1]], "keywords": ["Europe|Rome"]})).unwrap();
    let err = s.execute("keyword.move", &json!({"keyword": "rome", "parent": "europe"})).unwrap_err().to_string();
    assert!(err.contains("merge"), "{err}");
    s.execute("keyword.move", &json!({"keyword": "rome", "parent": "europe", "merge": true})).unwrap();
    assert_eq!(keywords_of(&s, ids[0]), ["Europe|Rome"]);
    s.execute("keyword.move", &json!({"keyword": "europe|rome", "parent": null})).unwrap();
    assert_eq!((keywords_of(&s, ids[0]), keywords_of(&s, ids[1])), (vec!["Rome".to_string()], vec!["Rome".to_string()]));
    assert!(s.execute("keyword.move", &json!({"keyword": "rome"})).is_err(), "`parent` must be said, null for the top level");
}

/// Purge Unused Keywords takes off the list the keywords no photo has, in one undo step.
#[test]
fn purging_unused_keywords() {
    let mut s = Session::with_demo();
    s.execute("keyword.create", &json!({"name": "Weddings"})).unwrap();
    let r = s.execute("keyword.purgeUnused", &json!({})).unwrap();
    assert_eq!(r["purged"], 1, "{r}");
    assert!(s.execute("keyword.info", &json!({"keyword": "Weddings"})).is_err());
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.execute("keyword.info", &json!({"keyword": "Weddings"})).is_ok());
}

/// Moving a keyword into a parent that doesn't exist yet makes that parent, and the keyword filter
/// follows the keyword there (the grid kept filtering by the old path, and showed nothing).
#[test]
fn the_filter_follows_a_keyword_moved_into_a_new_parent() {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0].0;
    s.execute("photo.setMeta", &json!({"ids": [id], "keywords": ["Rome"]})).unwrap();
    s.execute("library.filter", &json!({"keyword": "Rome"})).unwrap();
    s.execute("keyword.move", &json!({"keyword": "rome", "parent": "Italy"})).unwrap();
    assert_eq!(keywords_of(&s, id), ["Italy|Rome"]);
    assert_eq!(s.filter.keyword.as_deref(), Some("Italy|Rome"));
}

/// Keywords given to photos are stored cleaned (" beach " is "beach", "Travel | Italy" is
/// "Travel|Italy"), so the list's actions find them; one a photo has already, whatever the case of
/// any letter, isn't added twice.
#[test]
fn keywords_given_to_photos_are_stored_cleaned() {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0].0;
    s.execute("photo.setMeta", &json!({"ids": [id], "keywords": [" beach ", "Travel | Italy", "Ärzte"]})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [id], "addKeywords": [" sea ", "ÄRZTE", " "]})).unwrap();
    assert_eq!(keywords_of(&s, id), ["beach", "Travel|Italy", "Ärzte", "sea"]);
    s.execute("photo.setMeta", &json!({"ids": [id], "removeKeywords": ["travel | italy"]})).unwrap();
    assert_eq!(keywords_of(&s, id), ["beach", "Ärzte", "sea"]);
    assert!(s.execute("keyword.info", &json!({"keyword": "beach"})).is_ok());
}

/// Rename, merge and delete say how many photos they changed, as before the keyword list: the
/// list's own changes aren't photos.
#[test]
fn keyword_actions_count_the_photos_they_change() {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0].0;
    s.execute("keyword.create", &json!({"name": "Weddings", "synonyms": ["marriage"]})).unwrap();
    assert_eq!(s.execute("keyword.rename", &json!({"from": "Weddings", "to": "Marriages"})).unwrap()["changed"], 0);
    s.execute("photo.setMeta", &json!({"ids": [id], "addKeywords": ["Marriages"]})).unwrap();
    assert_eq!(s.execute("keyword.rename", &json!({"from": "Marriages", "to": "Weddings"})).unwrap()["changed"], 1);
    assert_eq!(s.execute("keyword.delete", &json!({"keyword": "weddings"})).unwrap()["changed"], 1);
}

/// ⌥1–⌥9 take a set's keyword off the selected photos when they all have it, whatever the case of
/// any of its letters (the set has "ärzte", the photos "ÄRZTE" and "ärzte").
#[test]
fn a_set_keyword_toggles_whatever_its_case() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(2).map(|p| p.0).collect();
    s.execute("photo.setMeta", &json!({"ids": [ids[0]], "keywords": ["ÄRZTE"]})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [ids[1]], "keywords": ["ärzte"]})).unwrap();
    s.execute("keyword.saveSet", &json!({"name": "Clinic", "keywords": ["ärzte"]})).unwrap();
    s.execute("keyword.toggleFromSet", &json!({"index": 1, "ids": ids})).unwrap();
    assert!(ids.iter().all(|id| keywords_of(&s, *id).is_empty()), "taken off both");
}

fn set_named(s: &mut Session, name: &str) -> serde_json::Value {
    let sets = s.execute("keyword.sets", &json!({})).unwrap();
    sets["sets"].as_array().unwrap().iter().find(|x| x["name"] == name).cloned().unwrap_or(serde_json::Value::Null)
}

/// Edit Keyword Set: a set's nine slots keep their places, so each keyword stays on its ⌥ key; an
/// empty slot is allowed and does nothing.
#[test]
fn a_keyword_sets_slots_keep_their_places() {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0].0;
    s.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": [" ceremony ", "", "reception", ""]})).unwrap();
    assert_eq!(set_named(&mut s, "Weddings")["keywords"], json!(["ceremony", "", "reception"]), "trailing empty slots go");
    let before = keywords_of(&s, id);
    let r = s.execute("keyword.toggleFromSet", &json!({"index": 2, "ids": [id]})).unwrap();
    assert_eq!((r["changed"].as_u64(), keywords_of(&s, id)), (Some(0), before), "⌥2 is empty: nothing");
    s.execute("keyword.toggleFromSet", &json!({"index": 3, "ids": [id]})).unwrap();
    assert!(keywords_of(&s, id).contains(&"reception".to_string()), "⌥3 is reception");
}

/// A set holds each keyword once, whatever its case: a second one leaves its slot empty.
#[test]
fn a_keyword_set_holds_each_keyword_once() {
    let mut s = Session::with_demo();
    s.execute("keyword.saveSet", &json!({"name": "Travel", "keywords": ["Lisbon", "harbour", "LISBON"]})).unwrap();
    assert_eq!(set_named(&mut s, "Travel")["keywords"], json!(["Lisbon", "harbour"]));
}

/// Renaming a set keeps its place among the sets and keeps it current; a name another set has is
/// refused.
#[test]
fn renaming_a_keyword_set_keeps_its_place() {
    let mut s = Session::with_demo();
    s.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": ["ceremony"]})).unwrap();
    s.execute("keyword.saveSet", &json!({"name": "Travel", "keywords": ["harbour"]})).unwrap();
    s.execute("keyword.useSet", &json!({"name": "Weddings"})).unwrap();
    let r = s.execute("keyword.saveSet", &json!({"name": "Ceremonies", "replace": "weddings", "keywords": ["ceremony", "rings"]})).unwrap();
    let names: Vec<String> = r["sets"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["Recent Keywords", "Ceremonies", "Travel"]);
    assert_eq!(r["current"], "Ceremonies");
    assert_eq!(r["keywords"], json!(["ceremony", "rings"]));
    let err = s.execute("keyword.saveSet", &json!({"name": "travel", "replace": "Ceremonies", "keywords": []})).unwrap_err().to_string();
    assert!(err.contains("already"), "{err}");
    assert!(s.execute("keyword.saveSet", &json!({"name": "Rings", "replace": "Lisbon", "keywords": []})).is_err(), "no such set");
}

/// Renaming a set without giving its keywords keeps its keywords (not the current set's).
#[test]
fn renaming_a_set_alone_keeps_its_keywords() {
    let mut s = Session::with_demo();
    s.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": ["ceremony"]})).unwrap();
    s.execute("keyword.saveSet", &json!({"name": "Travel", "keywords": ["harbour"]})).unwrap();
    s.execute("keyword.saveSet", &json!({"name": "Ceremonies", "replace": "Weddings"})).unwrap();
    assert_eq!(set_named(&mut s, "Ceremonies")["keywords"], json!(["ceremony"]));
}

/// A new set (`new: true`, what Edit Set… on Recent Keywords saves) never takes another set's
/// name: saving over it silently is refused.
#[test]
fn a_new_set_doesnt_take_a_sets_name() {
    let mut s = Session::with_demo();
    s.execute("keyword.saveSet", &json!({"name": "Travel", "keywords": ["harbour", "lisbon"]})).unwrap();
    let err = s.execute("keyword.saveSet", &json!({"name": "travel", "new": true, "keywords": ["x"]})).unwrap_err().to_string();
    assert!(err.contains("already"), "{err}");
    assert_eq!(set_named(&mut s, "Travel")["keywords"], json!(["harbour", "lisbon"]), "left as it was");
}

/// Set names match whatever the case of any of their letters, for every set command alike.
#[test]
fn set_names_match_whatever_their_case() {
    let mut s = Session::with_demo();
    s.execute("keyword.saveSet", &json!({"name": "ÉTÉ", "keywords": ["sunset"]})).unwrap();
    s.execute("keyword.useSet", &json!({"name": "Recent Keywords"})).unwrap();
    s.execute("keyword.useSet", &json!({"name": "été"})).unwrap();
    assert_eq!(s.execute("keyword.sets", &json!({})).unwrap()["current"], "ÉTÉ");
    s.execute("keyword.deleteSet", &json!({"name": "été"})).unwrap();
    assert!(set_named(&mut s, "ÉTÉ").is_null(), "deleted");
}

/// Export Keywords writes the keyword list file, and names the keywords Capture One's importer
/// refuses (it allows none of ; , < > in a list); without a path it gives the text.
#[test]
fn exporting_keywords_writes_the_list() {
    let dir = temp_dir("export-keywords");
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0].0;
    s.execute("photo.setMeta", &json!({"ids": [id], "keywords": ["Places|Lisbon", "fish, chips"]})).unwrap();
    let path = dir.join("keywords.txt");
    let r = s.execute("keyword.export", &json!({"path": path.to_string_lossy()})).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("Places\n\tLisbon\n"), "{text}");
    assert!(r["keywords"].as_u64().unwrap() >= 3, "{r}");
    assert_eq!(r["captureOneRefuses"], json!(["fish, chips"]), "{r}");
    assert_eq!(r["unwritable"], json!([]), "{r}");
    let r = s.execute("keyword.export", &json!({})).unwrap();
    assert_eq!(r["text"].as_str(), Some(text.as_str()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A keyword the format can't hold is left out of the file and named, so the file reads back.
#[test]
fn exporting_keywords_names_what_the_list_cant_hold() {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0].0;
    s.execute("photo.setMeta", &json!({"ids": [id], "keywords": ["Travel|[draft]", "Travel|Lisbon"]})).unwrap();
    let before = s.execute("keyword.export", &json!({})).unwrap()["keywords"].as_u64().unwrap();
    // a name starting with a brace is written, counted and checked for Capture One like any other
    s.execute("photo.setMeta", &json!({"ids": [id], "addKeywords": ["{draft, notes"]})).unwrap();
    let r = s.execute("keyword.export", &json!({})).unwrap();
    assert_eq!(r["keywords"].as_u64(), Some(before + 1), "{r}");
    assert_eq!(r["captureOneRefuses"], json!(["{draft, notes"]), "{r}");
    assert_eq!(r["unwritable"], json!(["Travel|[draft]"]), "{r}");
    let text = r["text"].as_str().unwrap();
    assert!(text.contains("Travel\n\tLisbon\n") && !text.contains("[draft]"), "{text}");
    s.execute("keyword.import", &json!({"text": text})).unwrap();
}

/// A list saved as UTF-16 (as Windows' Notepad can) says to save it as UTF-8, and text too big to
/// be a keyword list is refused, as a file that big is.
#[test]
fn importing_keywords_refuses_what_it_cant_read() {
    let dir = temp_dir("import-keywords-refused");
    let mut s = Session::with_demo();
    let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain("Travel\r\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    std::fs::write(dir.join("travel.txt"), utf16).unwrap();
    let err = s.execute("keyword.import", &json!({"path": dir.join("travel.txt").to_string_lossy()})).unwrap_err().to_string();
    assert!(err.contains("UTF-16") && err.contains("UTF-8"), "{err}");
    let big = "Travel\n".repeat((17 << 20) / 7);
    let err = s.execute("keyword.import", &json!({"text": big})).unwrap_err().to_string();
    assert!(err.contains("16 MB"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Import Keywords reads a keyword list file (or text) as one undo step and says what it added;
/// a file it can't read says which line, and nothing is taken.
#[test]
fn importing_keywords_reads_a_list() {
    let dir = temp_dir("import-keywords");
    let mut s = Session::with_demo();
    let path = dir.join("vocabulary.utf8");
    std::fs::write(&path, "[Places]\r\n\tPortugal\r\n\t\tLisbon\r\n").unwrap();
    let undo = s.undo.len();
    let r = s.execute("keyword.import", &json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!((r["added"].as_u64(), r["updated"].as_u64()), (Some(3), Some(0)), "{r}");
    assert_eq!(s.undo.len(), undo + 1);
    assert!(s.catalog.has_keyword("Places|Portugal|Lisbon"));
    let r = s.execute("keyword.import", &json!({"text": "Events\n\t{celebrations}\n"})).unwrap();
    assert_eq!(r["added"], 1);
    let err = s.execute("keyword.import", &json!({"text": "Events\n\t\tWeddings\n"})).unwrap_err().to_string();
    assert!(err.contains("line 2"), "{err}");
    std::fs::write(dir.join("bad.txt"), [0xff, 0xfe, 0x00, 0x41]).unwrap();
    assert!(s.execute("keyword.import", &json!({"path": dir.join("bad.txt").to_string_lossy()})).is_err(), "not UTF-8: refused");
    assert!(s.execute("keyword.import", &json!({})).is_err(), "a path or text");
    let _ = std::fs::remove_dir_all(&dir);
}
