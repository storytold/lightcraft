//! The filter and the source shown: what the source owns (the album, the library folder, Picks' flag) never
//! stays in the filter, whichever command sets it (`library.filter`, `filter.applyPreset`,
//! `library.toggleFilters`, `library.source`, `Session::set_filter`).
use lightcraft_catalog::{AlbumId, Filter, Flag, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::cmd::filters::FilterPreset;
use crate::filter_state::SourceOwned;
use crate::{Result, Selection, Session};

const A: &str = "/lightcraft-reconcile/a";
const B: &str = "/lightcraft-reconcile/b";

/// Photos 1 and 2 (rated 5, 5; photo 1 picked) are in folder A, photos 3 and 4 (rated 2, 1; photo 3 picked)
/// in folder B. Album X holds photo 1, album Y photo 3.
fn session() -> Result<(Session, AlbumId, AlbumId)> {
    let mut s = Session::new();
    for (id, dir, rating, flag) in [(1, A, 5, Flag::Pick), (2, A, 5, Flag::None), (3, B, 2, Flag::Pick), (4, B, 1, Flag::None)] {
        let mut photo = Photo::new(PhotoId(id), Source::File { path: format!("{dir}/{id}.jpg") }, &format!("{id}.jpg"), "JPEG", 40, 30, "2026-10-23");
        photo.rating = rating;
        photo.flag = flag;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    let mut make = |name: &str, id: u64| {
        s.selection = Selection { ids: vec![PhotoId(id)], active: None };
        AlbumId(s.execute("album.create", &json!({"name": name, "addSelected": true})).unwrap()["id"].as_u64().unwrap())
    };
    let (x, y) = (make("X", 1), make("Y", 3));
    Ok((s, x, y))
}

fn folder(s: &mut Session, path: &str) {
    s.execute("library.source", &json!({"kind": "libraryFolder", "path": path})).unwrap();
}

fn album(s: &mut Session, id: AlbumId) {
    s.execute("library.source", &json!({"kind": "album", "id": id.0})).unwrap();
}

fn preset(s: &mut Session, filter: Filter) {
    s.filter_presets.push(FilterPreset { name: "p".into(), filter });
}

fn apply(s: &mut Session) -> Value {
    s.execute("filter.applyPreset", &json!({"name": "p"})).unwrap()
}

#[test]
fn a_preset_with_a_library_folder_leaves_it_out_while_a_folder_is_shown() {
    let (mut s, ..) = session().unwrap();
    preset(&mut s, Filter { library_folder: Some(B.into()), rating: 1, ..Default::default() });
    folder(&mut s, A);
    let r = apply(&mut s);
    assert_eq!(r, json!({"photos": 2, "skipped": ["libraryFolder"]}));
    assert_eq!(s.filter().library_folder, None, "no \"Folder: B\" chip over folder A");
    assert_eq!(s.filter().rating, 1, "the rest of the preset applies");
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(s.visible().len(), 4, "folder B does not narrow the next source");
}

#[test]
fn a_preset_with_a_library_folder_applies_whole_while_no_folder_is_shown() {
    let (mut s, ..) = session().unwrap();
    preset(&mut s, Filter { library_folder: Some(B.into()), ..Default::default() });
    assert_eq!(apply(&mut s), json!({"photos": 2}));
    assert_eq!(s.filter().library_folder.as_deref(), Some(B));
}

#[test]
fn a_preset_with_an_album_leaves_it_out_whichever_source_is_shown() {
    let (mut s, x, y) = session().unwrap();
    preset(&mut s, Filter { album: Some(x), rating: 4, ..Default::default() });
    album(&mut s, y);
    assert_eq!(apply(&mut s), json!({"photos": 0, "skipped": ["album"]}), "album Y holds photo 3, rated 2");
    assert_eq!(s.filter().album, None);
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(apply(&mut s), json!({"photos": 2, "skipped": ["album"]}), "no album chip, so no album narrowing the grid");
    assert_eq!(s.filter().album, None);
    assert_eq!(s.filter().rating, 4);
}

#[test]
fn a_preset_with_a_flag_leaves_it_out_while_picks_is_shown() {
    let (mut s, ..) = session().unwrap();
    preset(&mut s, Filter { flag: Some(Flag::Reject), rating: 2, ..Default::default() });
    s.execute("library.source", &json!({"kind": "picks"})).unwrap();
    assert_eq!(apply(&mut s), json!({"photos": 2, "skipped": ["flag"]}));
    assert_eq!(s.filter().flag, None, "no \"Rejected\" chip over the picks");
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(apply(&mut s), json!({"photos": 0}), "off Picks the flag applies whole");
    assert_eq!(s.filter().flag, Some(Flag::Reject));
}

/// A browsed (Local) folder is shown through `Session::browse`, which `view_filter` turns into the folder
/// filter while the Local folder is shown. The filter never holds it: no chip names it.
fn browsing(s: &mut Session, path: &str) {
    s.browse = Some(crate::view::Browse { path: path.into(), subfolders: false });
}

#[test]
fn a_renamed_browsed_folder_does_not_narrow_all_photos() {
    let (mut s, ..) = session().unwrap();
    browsing(&mut s, "/lightcraft-reconcile/local/trip");
    s.execute("library.filter", &json!({"rating": 5})).unwrap();
    crate::cmd::browse::follow_folder(&mut s, "/lightcraft-reconcile/local/trip", "/lightcraft-reconcile/local/holiday");
    assert_eq!(
        s.browse.as_ref().map(|b| std::path::Path::new(&b.path)),
        Some(std::path::Path::new("/lightcraft-reconcile/local/holiday")),
        "the browsed folder follows"
    );
    assert_eq!(s.filter().folder, None, "no chip names a folder, so the filter holds none");
    assert_eq!(s.visible().len(), 2, "All Photos shows what its rating chip says: the two photos rated 5");
}

#[test]
fn a_renamed_browsed_folder_keeps_the_filter_put_aside() {
    let (mut s, ..) = session().unwrap();
    browsing(&mut s, "/lightcraft-reconcile/local/trip");
    s.execute("library.filter", &json!({"keyword": "cat", "rating": 3})).unwrap();
    let put_aside = s.filter().clone();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert!(s.filters_off());
    crate::cmd::browse::follow_folder(&mut s, "/lightcraft-reconcile/local/trip", "/lightcraft-reconcile/local/holiday");
    assert_eq!(*s.filter(), Filter::default(), "filters stay off");
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(*s.filter(), put_aside, "Enable Filters brings back the filter put aside, not a folder");
}

/// A `folder` a script set through `library.filter` names a place on disk like a library folder does: when that
/// folder (or one above it) moves, the filter and the one put aside follow it; another folder's move changes nothing.
#[test]
fn a_folder_filter_set_by_a_script_follows_the_folder_it_names() {
    let (mut s, ..) = session().unwrap();
    let at = |s: &Session| s.filter().folder.clone().map(std::path::PathBuf::from);
    s.execute("library.filter", &json!({"folder": "/lightcraft-reconcile/local/trip/day1", "subfolders": true, "rating": 3})).unwrap();
    crate::cmd::browse::follow_folder(&mut s, "/lightcraft-reconcile/local/trip", "/lightcraft-reconcile/local/holiday");
    assert_eq!(at(&s), Some("/lightcraft-reconcile/local/holiday/day1".into()));
    assert!(s.filter().subfolders && s.filter().rating == 3, "the rest of the filter is untouched");
    s.execute("library.toggleFilters", &json!({})).unwrap();
    crate::cmd::browse::follow_folder(&mut s, "/lightcraft-reconcile/local/holiday", "/lightcraft-reconcile/local/trip2");
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(at(&s), Some("/lightcraft-reconcile/local/trip2/day1".into()), "the one put aside follows too");
    crate::cmd::browse::follow_folder(&mut s, "/lightcraft-reconcile/local/other", "/lightcraft-reconcile/local/elsewhere");
    assert_eq!(at(&s), Some("/lightcraft-reconcile/local/trip2/day1".into()), "another folder's move changes nothing");
    assert!(s.browse.is_none(), "following a filter never starts browsing");
}

#[test]
fn library_filter_refuses_what_the_source_owns_and_changes_nothing() {
    let (mut s, x, y) = session().unwrap();
    s.execute("library.filter", &json!({"rating": 2})).unwrap();
    let before = s.filter().clone();
    let refuse = |s: &mut Session, patch: Value, why: &str| {
        let e = s.execute("library.filter", &patch).unwrap_err().to_string();
        assert!(e.contains(why), "{e}");
        assert_eq!(*s.filter(), before, "nothing of the call is applied: {patch}");
    };
    // under every source an album is refused: the filter bar has no album chip
    refuse(&mut s, json!({"rating": 3, "album": x.0}), "holds no album");
    album(&mut s, y);
    refuse(&mut s, json!({"rating": 3, "album": x.0}), "holds no album");
    folder(&mut s, A);
    refuse(&mut s, json!({"rating": 3, "libraryFolder": B}), "a folder is already shown");
    s.execute("library.source", &json!({"kind": "picks"})).unwrap();
    refuse(&mut s, json!({"rating": 3, "flag": "reject"}), "Picks is already shown");
    refuse(&mut s, json!({"flag": "pick"}), "Picks is already shown");
}

#[test]
fn library_filter_still_clears_and_sets_the_other_fields_while_a_source_owns_one() {
    let (mut s, ..) = session().unwrap();
    s.execute("library.source", &json!({"kind": "picks"})).unwrap();
    s.execute("library.filter", &json!({"rating": 4, "flag": null})).unwrap();
    assert_eq!(s.filter().rating, 4);
    folder(&mut s, A);
    s.execute("library.filter", &json!({"libraryFolder": null, "album": null, "rating": 5})).unwrap();
    assert_eq!(s.filter().rating, 5);
}

#[test]
fn set_filter_leaves_out_what_the_source_owns_and_says_so() {
    let (mut s, x, _) = session().unwrap();
    let left_out = s.set_filter(Filter { album: Some(x), rating: 3, flag: Some(Flag::Pick), ..Default::default() });
    assert_eq!(left_out, [SourceOwned::Album], "the album goes under any source; the flag only under Picks");
    assert_eq!((s.filter().album, s.filter().rating, s.filter().flag), (None, 3, Some(Flag::Pick)));
    s.execute("library.source", &json!({"kind": "picks"})).unwrap();
    assert_eq!(s.filter().flag, None, "showing Picks takes the flag out too");
    let left_out = s.set_filter(Filter { flag: Some(Flag::Pick), library_folder: Some(A.into()), ..Default::default() });
    assert_eq!(left_out, [SourceOwned::Flag], "a library folder stays while no folder is shown");
}

#[test]
fn enable_filters_brings_a_filter_back_without_what_the_source_owns() {
    let (mut s, ..) = session().unwrap();
    s.execute("library.filter", &json!({"rating": 1, "flag": "pick"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.source", &json!({"kind": "picks"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter().rating, 1, "the rest of the filter is back");
    assert_eq!(s.filter().flag, None, "Picks shows the picks: no flag chip over it");
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(s.visible().len(), 4, "the old flag does not narrow the next source");
}

#[test]
fn a_flag_carried_into_picks_does_not_block_the_other_filter_changes() {
    // a source set without `source_shown` (as a library opening did before it reconciled) over a flag filter: here Picks
    let (mut s, ..) = session().unwrap();
    s.execute("library.filter", &json!({"flag": "reject"})).unwrap();
    s.source = crate::LibrarySource::Picks;
    s.execute("library.filter", &json!({"rating": 2})).expect("a change that does not set the flag is not refused");
    assert_eq!((s.filter().rating, s.filter().flag), (2, None), "and the flag Picks owns is taken out");
}
