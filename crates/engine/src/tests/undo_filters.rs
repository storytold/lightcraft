//! Undo and redo carry what a step did to the person's filter: a keyword deleted or renamed, a person renamed, a
//! library folder removed. Nothing in those commands records it; `run_command` compares the filter before and after
//! any command that committed a step.
//!
//! * Given a filter that names a keyword, person or folder the step took out or renamed, when the step is undone, then
//!   the filter names it again (put aside or in use, wherever the filter is now), and redo carries it forward again.
//! * Given the filter was changed after the step, when it is undone, then only what the step changed comes back and
//!   nothing the person did since is overwritten.
//! * Given the person threw that filter away after the step and built another one, when the step is undone, then the
//!   new filter is left alone.

use lightcraft_catalog::{Catalog, Filter, Op, Photo, PhotoId, Source};
use lightcraft_geom::Rect;
use lightcraft_meta::{Region, RegionKind};
use serde_json::{Value, json};

use crate::Session;

/// Photos in two folders; the ones in /pics/trip carry the keyword "cat" and the one in /pics/home "dog".
fn session() -> Session {
    let mut s = Session::new();
    for (path, keyword) in [("/pics/trip/a.jpg", "cat"), ("/pics/trip/b.jpg", "cat"), ("/pics/home/c.jpg", "dog")] {
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        p.meta.keywords = vec![keyword.into()];
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s
}

fn run(s: &mut Session, command: &str, params: Value) {
    s.execute(command, &params).unwrap();
}

fn delete_cat(s: &mut Session) {
    run(s, "keyword.delete", json!({"keyword": "cat"}));
}

fn remove_trip(s: &mut Session) {
    run(s, "library.removeFolder", json!({"path": "/pics/trip"}));
}

fn undo(s: &mut Session) {
    run(s, "edit.undo", json!({}));
}

fn redo(s: &mut Session) {
    run(s, "edit.redo", json!({}));
}

fn toggle(s: &mut Session) {
    run(s, "library.toggleFilters", json!({}));
}

fn filtered(keyword: Option<&str>, folder: Option<&str>, rating: u8) -> Filter {
    Filter { keyword: keyword.map(Into::into), library_folder: folder.map(Into::into), rating, ..Default::default() }
}

#[test]
fn undoing_a_keyword_delete_brings_the_live_filters_keyword_back() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    delete_cat(&mut s);
    assert_eq!(s.filter().keyword, None, "the deletion takes the keyword out of the filter");
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"), "undo puts the keyword back in the filter");
    assert_eq!(s.visible().len(), 2);
    redo(&mut s);
    assert_eq!(s.filter().keyword, None, "redo takes it out again");
    assert_eq!(s.visible().len(), 3);
}

#[test]
fn undoing_a_keyword_delete_brings_the_put_aside_filters_keyword_back() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat", "rating": 0}));
    toggle(&mut s);
    delete_cat(&mut s);
    undo(&mut s);
    assert!(s.filters_off(), "the filter is still put aside, and Enable Filters has something to bring back");
    toggle(&mut s);
    assert_eq!(*s.filter(), filtered(Some("cat"), None, 0));
    assert_eq!(s.visible().len(), 2);
}

#[test]
fn undoing_a_keyword_rename_brings_the_filters_old_keyword_back() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat", "rating": 0}));
    run(&mut s, "keyword.rename", json!({"from": "cat", "to": "kitten"}));
    assert_eq!(s.filter().keyword.as_deref(), Some("kitten"), "the filter follows the rename");
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"), "undo gives the photos their old keyword, so the filter too");
    assert_eq!(s.visible().len(), 2);
    redo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("kitten"));
    assert_eq!(s.visible().len(), 2);
}

#[test]
fn undoing_a_keyword_rename_that_merged_two_keywords_brings_the_filters_old_keyword_back() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    run(&mut s, "keyword.rename", json!({"from": "cat", "to": "dog"}));
    assert_eq!(s.filter().keyword.as_deref(), Some("dog"));
    assert_eq!(s.visible().len(), 3, "the merged keyword is on every photo");
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"));
    assert_eq!(s.visible().len(), 2);
}

#[test]
fn undoing_a_keyword_rename_follows_the_filter_put_aside_since() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat", "rating": 3}));
    run(&mut s, "keyword.rename", json!({"from": "cat", "to": "kitten"}));
    toggle(&mut s);
    undo(&mut s);
    assert!(s.filters_off());
    toggle(&mut s);
    assert_eq!(*s.filter(), filtered(Some("cat"), None, 3), "the put-aside filter has the old keyword again, and its stars");
}

#[test]
fn undoing_a_person_rename_brings_the_filters_old_name_back() {
    let mut s = session();
    let id = s.catalog.photos().map(|p| p.id).min().unwrap();
    let mut meta = s.catalog.photo(id).unwrap().meta.clone();
    let rect = Rect { x0: 0.1, y0: 0.2, x1: 0.35, y1: 0.6 };
    meta.regions = vec![Region { rect, kind: RegionKind::Face, name: Some("Ann".into()), description: None }];
    s.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    run(&mut s, "library.filter", json!({"person": "Ann"}));
    assert_eq!(s.visible().len(), 1);
    run(&mut s, "faces.renamePerson", json!({"from": "Ann", "to": "Ada"}));
    assert_eq!(s.filter().person.as_deref(), Some("Ada"), "the filter follows the rename");
    undo(&mut s);
    assert_eq!(s.filter().person.as_deref(), Some("Ann"), "no empty grid under a name nobody has");
    assert_eq!(s.visible().len(), 1);
    redo(&mut s);
    assert_eq!(s.filter().person.as_deref(), Some("Ada"));
    assert_eq!(s.visible().len(), 1);
}

#[test]
fn undoing_a_person_rename_shows_the_one_photo_it_changed_under_the_filter_it_gives_back() {
    // one photo has the person: undo shows it (makes it the active photo) when it is in view, and it is in view
    // under the old name the filter gets back, not under the new one the filter still has while the catalog is undone
    let mut s = session();
    let mut ids: Vec<PhotoId> = s.catalog.photos().map(|p| p.id).collect();
    ids.sort();
    let mut meta = s.catalog.photo(ids[0]).unwrap().meta.clone();
    let rect = Rect { x0: 0.1, y0: 0.2, x1: 0.35, y1: 0.6 };
    meta.regions = vec![Region { rect, kind: RegionKind::Face, name: Some("Ann".into()), description: None }];
    s.commit("setup", Op::SetMeta { id: ids[0], meta: Box::new(meta) }).unwrap();
    run(&mut s, "library.filter", json!({"person": "Ann"}));
    run(&mut s, "faces.renamePerson", json!({"from": "Ann", "to": "Ada"}));
    s.selection = crate::Selection::single(ids[1]);
    undo(&mut s);
    assert_eq!(s.filter().person.as_deref(), Some("Ann"));
    assert_eq!(s.selection.active, Some(ids[0]), "the photo the undo changed is on screen");
    s.selection = crate::Selection::single(ids[1]);
    redo(&mut s);
    assert_eq!(s.selection.active, Some(ids[0]), "and so it is for redo");
}

#[test]
fn undoing_a_step_keeps_what_the_filter_got_since() {
    // the delete emptied the filter, which the person did not throw away: the stars they add are an edit of it (when the
    // person empties the filter by hand instead, what comes after is a new filter: see
    // `undoing_a_step_leaves_a_filter_built_after_the_old_one_was_emptied_or_replaced_alone`)
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    delete_cat(&mut s);
    run(&mut s, "library.filter", json!({"rating": 3}));
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(Some("cat"), None, 3), "the stars stay, the keyword returns");
    // a keyword the person chose after the delete is not replaced
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    delete_cat(&mut s);
    run(&mut s, "library.filter", json!({"keyword": "dog"}));
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("dog"));
}

#[test]
fn undoing_a_step_leaves_a_filter_built_after_the_old_one_was_put_aside_alone() {
    // the keyword the step took out of the put-aside filter does not join the filter built since: setting a filter while
    // filters are off forgets the one put aside (`library.toggleFilters`)
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    toggle(&mut s);
    delete_cat(&mut s);
    run(&mut s, "library.filter", json!({"rating": 3}));
    undo(&mut s);
    assert!(!s.filters_off(), "the filter the person set after the delete stays in use");
    assert_eq!(*s.filter(), filtered(None, None, 3), "and nothing joins it");
    // the same for a folder
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    toggle(&mut s);
    remove_trip(&mut s);
    run(&mut s, "library.filter", json!({"rating": 3}));
    undo(&mut s);
    assert!(!s.filters_off());
    assert_eq!(*s.filter(), filtered(None, None, 3));
    // redo leaves it alone too
    redo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 3));
}

#[test]
fn undoing_a_step_leaves_a_filter_built_after_the_old_one_was_emptied_or_replaced_alone() {
    // taking the last criterion off by hand ends the filter
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat", "rating": 3}));
    delete_cat(&mut s);
    run(&mut s, "library.filter", json!({"rating": 0}));
    run(&mut s, "library.filter", json!({"rating": 4}));
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 4));
    // so does Clear Filters
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat", "rating": 3}));
    delete_cat(&mut s);
    run(&mut s, "library.clearFilter", json!({}));
    run(&mut s, "library.filter", json!({"rating": 4}));
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 4));
    // and a saved preset replaces it
    let mut s = session();
    run(&mut s, "library.filter", json!({"rating": 5}));
    run(&mut s, "filter.savePreset", json!({"name": "five"}));
    run(&mut s, "library.filter", json!({"rating": 0, "keyword": "cat"}));
    delete_cat(&mut s);
    run(&mut s, "filter.applyPreset", json!({"name": "five"}));
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 5), "the keyword does not join the preset's filter");
}

#[test]
fn undoing_a_step_leaves_a_filter_built_after_showing_a_source_emptied_the_old_one_alone() {
    // the folder removal leaves only the pick flag; showing Picks takes that flag out, so the filter is empty by the
    // person's own doing, and the rating they set next is a new filter, not an edit of the old one
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip", "flag": "pick"}));
    remove_trip(&mut s);
    run(&mut s, "library.source", json!({"kind": "picks"}));
    assert_eq!(*s.filter(), Filter::default());
    run(&mut s, "library.filter", json!({"rating": 3}));
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 3), "the folder does not join the new filter");
}

#[test]
fn undoing_a_step_leaves_a_filter_built_after_showing_a_source_emptied_the_put_aside_one_alone() {
    // the same, with the filter put aside: Enable Filters under Picks takes the pick flag out of the one it brings back
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip", "flag": "pick"}));
    remove_trip(&mut s);
    toggle(&mut s);
    run(&mut s, "library.source", json!({"kind": "picks"}));
    toggle(&mut s);
    assert!(!s.filters_off());
    assert_eq!(*s.filter(), Filter::default(), "nothing is left of the filter put aside");
    run(&mut s, "library.filter", json!({"rating": 3}));
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 3), "the folder does not join the new filter");
}

#[test]
fn a_filter_ended_by_the_source_stays_ended_through_enable_filters_and_empty_edits() {
    // Enable Filters pressed again with nothing to bring back, Clear Filters on the empty filter, and an edit that
    // sets nothing, do not make the filter set next an edit of the one that ended
    for presses in 0..4 {
        let mut s = session();
        run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip", "flag": "pick"}));
        remove_trip(&mut s);
        toggle(&mut s);
        run(&mut s, "library.source", json!({"kind": "picks"}));
        toggle(&mut s);
        assert_eq!(*s.filter(), Filter::default());
        match presses {
            0 => toggle(&mut s),
            1 => run(&mut s, "library.clearFilter", json!({})),
            2 => run(&mut s, "library.filter", json!({})),
            _ => run(&mut s, "library.filter", json!({"rating": 0})),
        }
        run(&mut s, "library.filter", json!({"rating": 3}));
        undo(&mut s);
        assert_eq!(*s.filter(), filtered(None, None, 3), "variant {presses}: the folder does not join the new filter");
    }
}

#[test]
fn an_undo_that_moves_nothing_does_not_make_the_filter_that_ended_an_edit_target() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip", "flag": "pick"}));
    remove_trip(&mut s);
    // a later step that touched the filter, then left nothing of it in the filter
    run(&mut s, "library.filter", json!({"keyword": "dog"}));
    run(&mut s, "keyword.rename", json!({"from": "dog", "to": "hound"}));
    run(&mut s, "library.filter", json!({"keyword": null}));
    toggle(&mut s);
    run(&mut s, "library.source", json!({"kind": "picks"}));
    toggle(&mut s);
    assert_eq!(*s.filter(), Filter::default(), "the source emptied the filter that came back");
    // undoing the rename finds no keyword in the filter: it moves nothing, and the filter is still the one that ended
    undo(&mut s);
    assert_eq!(*s.filter(), Filter::default());
    run(&mut s, "library.filter", json!({"rating": 3}));
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 3), "the folder does not join the new filter");
}

#[test]
fn what_the_person_sets_after_an_undo_brought_a_folder_back_into_the_ended_filter_is_an_edit_of_it() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip", "flag": "pick"}));
    remove_trip(&mut s);
    toggle(&mut s);
    run(&mut s, "library.source", json!({"kind": "picks"}));
    toggle(&mut s);
    assert_eq!(*s.filter(), Filter::default());
    // undo brings the folder back into the empty filter: it is the person's filter again, not an ended one
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(None, Some("/pics/trip"), 0), "undo brings the folder back into the empty filter");
    run(&mut s, "library.filter", json!({"rating": 3}));
    redo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 3), "redoing the removal takes the folder out of the filter again");
}

#[test]
fn undoing_a_step_follows_the_filter_to_where_it_is_now() {
    // put aside after the delete: the keyword goes back into the put-aside filter, and the live filter stays empty
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat", "rating": 4}));
    delete_cat(&mut s);
    toggle(&mut s);
    undo(&mut s);
    assert!(s.filters_off());
    assert_eq!(*s.filter(), Filter::default(), "nothing is filtered while the filter is put aside");
    toggle(&mut s);
    assert_eq!(*s.filter(), filtered(Some("cat"), None, 4));
    // brought back after the delete: the keyword goes back into the filter in use
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat", "rating": 4}));
    toggle(&mut s);
    delete_cat(&mut s);
    toggle(&mut s);
    undo(&mut s);
    assert!(!s.filters_off());
    assert_eq!(*s.filter(), filtered(Some("cat"), None, 4));
    // and redo after the filter was put aside takes it out of the put-aside filter
    toggle(&mut s);
    redo(&mut s);
    assert!(s.filters_off());
    toggle(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 4));
}

#[test]
fn undoing_a_folder_removal_brings_the_live_filters_folder_back() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    remove_trip(&mut s);
    assert_eq!(s.filter().library_folder, None);
    undo(&mut s);
    assert_eq!(s.filter().library_folder.as_deref(), Some("/pics/trip"));
    assert_eq!(s.visible().len(), 2);
    redo(&mut s);
    assert_eq!(s.filter().library_folder, None, "redo takes it out again");
}

#[test]
fn undoing_a_folder_removal_brings_the_put_aside_filters_folder_back() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip", "rating": 4}));
    toggle(&mut s);
    remove_trip(&mut s);
    undo(&mut s);
    toggle(&mut s);
    assert_eq!(*s.filter(), filtered(None, Some("/pics/trip"), 4));
}

#[test]
fn undoing_a_folder_removal_that_dropped_the_put_aside_filter_brings_it_back() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    toggle(&mut s);
    remove_trip(&mut s);
    assert!(!s.filters_off(), "the emptied put-aside filter is dropped");
    undo(&mut s);
    assert!(s.filters_off(), "undo puts it aside again");
    toggle(&mut s);
    assert_eq!(s.filter().library_folder.as_deref(), Some("/pics/trip"));
    assert_eq!(s.visible().len(), 2);
}

#[test]
fn redoing_a_folder_removal_drops_the_put_aside_filter_it_emptied_again() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    toggle(&mut s);
    remove_trip(&mut s);
    undo(&mut s);
    redo(&mut s);
    assert!(!s.filters_off(), "nothing is left to bring back");
    assert_eq!(*s.filter(), Filter::default());
}

#[test]
fn undoing_a_folder_removal_puts_the_filter_aside_again_when_another_folder_is_shown() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    toggle(&mut s);
    remove_trip(&mut s);
    run(&mut s, "library.source", json!({"kind": "libraryFolder", "path": "/pics/home"}));
    undo(&mut s);
    assert!(s.filters_off(), "the filter the step dropped is put aside again, as it was");
    assert_eq!(*s.filter(), Filter::default(), "and the folder shown stays the only folder");
    toggle(&mut s);
    assert_eq!(*s.filter(), Filter::default(), "the folder shown wins when the filter comes back, as for any put-aside filter");
}

#[test]
fn undoing_a_folder_removal_does_not_replace_the_folder_shown_since() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    remove_trip(&mut s);
    run(&mut s, "library.source", json!({"kind": "libraryFolder", "path": "/pics/home"}));
    undo(&mut s);
    assert_eq!(s.filter().library_folder, None, "one folder at a time: the one shown wins");
    run(&mut s, "library.filter", json!({"rating": 3}));
    assert_eq!(*s.filter(), filtered(None, None, 3), "and filtering goes on, as no folder filter is refused");
}

#[test]
fn undoing_two_steps_puts_an_emptied_put_aside_filter_back_as_it_was() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    toggle(&mut s);
    delete_cat(&mut s);
    remove_trip(&mut s);
    assert!(!s.filters_off(), "the removal dropped the put-aside filter the delete had emptied");
    undo(&mut s);
    undo(&mut s);
    assert!(s.filters_off(), "filtering is still off, as before the two steps");
    assert_eq!(*s.filter(), Filter::default());
    toggle(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"));
}

#[test]
fn a_step_that_never_touched_the_filter_leaves_it_alone_on_undo() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"rating": 2}));
    remove_trip(&mut s);
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 2));
    assert!(!s.filters_off());
    assert!(s.undo.iter().chain(s.redo.iter()).all(|e| e.filters.is_none()), "no filter change is kept for it");
}

/// The fields one chip clears move together, or not at all: `only` and `onlyNone` are one constraint.
#[test]
fn a_step_carries_the_fields_of_a_chip_together() {
    let cat = Catalog::new();
    let from = Filter { only: vec![PhotoId(1)], only_none: false, ..Default::default() };
    let to = Filter { only: Vec::new(), only_none: true, ..Default::default() };
    let mut now = from.clone();
    crate::undo_filters::carry(&mut now, &from, &to, &cat, &serde_json::Map::new());
    assert_eq!(now, to, "both fields move");
    let mut half = Filter { only: vec![PhotoId(1)], only_none: true, ..Default::default() };
    crate::undo_filters::carry(&mut half, &from, &to, &cat, &serde_json::Map::new());
    assert_eq!(half, Filter { only: vec![PhotoId(1)], only_none: true, ..Default::default() }, "a chip changed since moves not at all");
    // what the step left alone is never touched, whatever it holds now
    let mut other = Filter { only: vec![PhotoId(1)], rating: 5, text: "x".into(), ..Default::default() };
    crate::undo_filters::carry(&mut other, &from, &to, &cat, &serde_json::Map::new());
    assert_eq!((other.only_none, other.rating, other.text.as_str()), (true, 5, "x"));
}

#[test]
fn redoing_a_step_leaves_alone_what_undo_left_alone() {
    // the person picks "cat" by hand after the rename: undo leaves it, and so must redo, though it now holds what the
    // step's undo found
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    run(&mut s, "keyword.rename", json!({"from": "cat", "to": "kitten"}));
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"), "undo leaves the keyword picked since");
    redo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"), "and so does redo: the person's filter is theirs");
    assert_eq!(s.visible().len(), 0, "whatever it matches: the photos have \"kitten\" again, and the chip says \"cat\"");
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"));
}

#[test]
fn redoing_a_step_still_carries_what_undo_carried() {
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat", "rating": 3}));
    run(&mut s, "keyword.rename", json!({"from": "cat", "to": "kitten"}));
    run(&mut s, "library.filter", json!({"rating": 4}));
    undo(&mut s);
    assert_eq!(*s.filter(), filtered(Some("cat"), None, 4));
    redo(&mut s);
    assert_eq!(*s.filter(), filtered(Some("kitten"), None, 4), "the keyword goes forward, the stars are the person's");
}

#[test]
fn a_command_that_commits_several_steps_keeps_no_filter_change_with_the_last_of_them() {
    // none does today (the ones that merge steps keep `filters` empty on purpose): the change belongs to no single step
    let mut s = session();
    let id = s.catalog.photos().map(|p| p.id).min().unwrap();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    s.execute_fn("two steps", |s| {
        s.commit("one", Op::SetRating { id, rating: 1 })?;
        s.commit("two", Op::SetRating { id, rating: 2 })?;
        s.set_filter(Filter::default());
        Ok(json!({}))
    })
    .unwrap();
    assert!(s.undo.iter().all(|e| e.filters.is_none()), "no step takes the change for its own");
    undo(&mut s);
    assert_eq!(*s.filter(), Filter::default(), "undoing the last step leaves the filter as it is");
}

#[test]
fn a_command_that_committed_a_step_and_then_failed_keeps_its_filter_change_with_the_step() {
    // the step is on the undo stack whatever the command answered, and undo moves the filter back with it
    let mut s = session();
    let id = s.catalog.photos().map(|p| p.id).min().unwrap();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    let failed = s.execute_fn("fails after its step", |s| {
        s.commit("one", Op::SetRating { id, rating: 1 })?;
        s.set_filter(Filter::default());
        Err(crate::EngineError::Other("boom".into()))
    });
    assert!(failed.is_err());
    assert_eq!(*s.filter(), Filter::default());
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"), "the step's filter change is carried back with it");
}

#[test]
fn undoing_a_folder_removal_while_another_folder_is_shown_keeps_the_put_aside_folder_for_later() {
    // the folder is restored as Enable Filters would restore it: put aside, and reconciled when it comes back
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    toggle(&mut s);
    remove_trip(&mut s);
    run(&mut s, "library.source", json!({"kind": "libraryFolder", "path": "/pics/home"}));
    undo(&mut s);
    assert!(s.filters_off(), "the folder it had is put aside again");
    run(&mut s, "library.source", json!({"kind": "all"}));
    toggle(&mut s);
    assert_eq!(s.filter().library_folder.as_deref(), Some("/pics/trip"), "and comes back once no other folder is shown");
    assert_eq!(s.visible().len(), 2);
}

#[test]
fn undoing_redoing_and_undoing_again_puts_the_dropped_filter_aside_again_not_an_empty_one() {
    // Enable Filters was turned back on under the other folder before redo: the folder is out of the filter in use, redo
    // moves nothing, and the second undo puts the filter the step dropped aside with its folder, which comes back
    // once no other folder is shown (an empty filter aside would leave filters off with nothing to bring back)
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    toggle(&mut s);
    remove_trip(&mut s);
    run(&mut s, "library.source", json!({"kind": "libraryFolder", "path": "/pics/home"}));
    undo(&mut s);
    toggle(&mut s);
    assert!(!s.filters_off());
    redo(&mut s);
    undo(&mut s);
    assert!(s.filters_off(), "the filter the step dropped is put aside again");
    run(&mut s, "library.source", json!({"kind": "all"}));
    toggle(&mut s);
    assert_eq!(s.filter().library_folder.as_deref(), Some("/pics/trip"), "with the folder it had");
}

#[test]
fn a_folder_the_shown_source_took_out_of_the_filter_comes_back_with_the_second_undo() {
    // undo restored the folder while another folder was shown, which left it out; redo and a second undo under All
    // Photos restore it all the same, as they would have without the other folder shown
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    remove_trip(&mut s);
    run(&mut s, "library.source", json!({"kind": "libraryFolder", "path": "/pics/home"}));
    undo(&mut s);
    redo(&mut s);
    run(&mut s, "library.source", json!({"kind": "all"}));
    undo(&mut s);
    assert_eq!(s.filter().library_folder.as_deref(), Some("/pics/trip"));
    assert_eq!(s.visible().len(), 2);
}

#[test]
fn a_folder_the_shown_source_took_out_comes_back_when_redo_runs_under_another_source() {
    // undo ran under another folder (which left the restored folder out), redo under All Photos: the second undo restores it
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    remove_trip(&mut s);
    run(&mut s, "library.source", json!({"kind": "libraryFolder", "path": "/pics/home"}));
    undo(&mut s);
    run(&mut s, "library.source", json!({"kind": "all"}));
    redo(&mut s);
    undo(&mut s);
    assert_eq!(s.filter().library_folder.as_deref(), Some("/pics/trip"));
}

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_folder_the_person_took_out_of_the_put_aside_filter_stays_out_whichever_folder_is_shown() {
    // the put-aside filter is not cleaned up for the folder shown, so a folder missing from it was removed by the person,
    // not left out by the source: undo must not bring it back because another folder is shown
    let dir = Scratch(std::env::temp_dir().join(format!("lc-undo-filters-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(dir.0.join("trip")).unwrap();
    let trip = dir.0.join("trip").to_string_lossy().to_string();
    let mut s = session();
    let id = s.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: format!("{trip}/d.jpg") }, "d.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    run(&mut s, "library.filter", json!({"libraryFolder": trip, "rating": 3}));
    run(&mut s, "folder.rename", json!({"path": trip, "name": "holiday"}));
    run(&mut s, "library.filter", json!({"libraryFolder": null}));
    toggle(&mut s);
    run(&mut s, "library.source", json!({"kind": "libraryFolder", "path": "/pics/home"}));
    undo(&mut s);
    run(&mut s, "library.source", json!({"kind": "all"}));
    toggle(&mut s);
    assert_eq!(*s.filter(), filtered(None, None, 3), "the folder chip the person removed stays removed");
}

#[test]
fn a_group_left_alone_once_moves_again_when_the_filter_holds_what_the_step_left() {
    // the person picked another keyword after the rename: undo and redo leave it, and a later undo still brings "cat" back
    // once the filter holds "kitten" again
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    run(&mut s, "keyword.rename", json!({"from": "cat", "to": "kitten"}));
    run(&mut s, "library.filter", json!({"keyword": "dog"}));
    undo(&mut s);
    redo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("dog"));
    run(&mut s, "library.filter", json!({"keyword": "kitten"}));
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"));
    assert_eq!(s.visible().len(), 2);
    // the same when the person had picked the keyword the step found
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    run(&mut s, "keyword.rename", json!({"from": "cat", "to": "kitten"}));
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    undo(&mut s);
    redo(&mut s);
    run(&mut s, "library.filter", json!({"keyword": "kitten"}));
    undo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("cat"));
}

#[test]
fn a_group_left_alone_is_carried_again_when_the_person_changed_it_since() {
    // undo left the person's "dog", then they picked "cat", which is what redo moves from: redo follows the rename
    let mut s = session();
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    run(&mut s, "keyword.rename", json!({"from": "cat", "to": "kitten"}));
    run(&mut s, "library.filter", json!({"keyword": "dog"}));
    undo(&mut s);
    run(&mut s, "library.filter", json!({"keyword": "cat"}));
    redo(&mut s);
    assert_eq!(s.filter().keyword.as_deref(), Some("kitten"), "no filter on a keyword the redo took away");
    // the same for a folder removal: the folder is not brought back by hand and left in the filter
    let mut s = session();
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    remove_trip(&mut s);
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/home"}));
    undo(&mut s);
    run(&mut s, "library.filter", json!({"libraryFolder": "/pics/trip"}));
    redo(&mut s);
    assert_eq!(s.filter().library_folder, None);
}
