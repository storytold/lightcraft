//! The facts cached about the selection (`Session::selection_facts`) follow the selection and the
//! catalog: an enabled check never answers for a selection or a photo state that is gone, however
//! the selection's `ids` were changed.

use lightcraft_catalog::PhotoId;
use lightcraft_develop::ProcessVersion;
use serde_json::json;

use crate::{Selection, SelectionState, Session};

fn enabled(s: &Session, id: &str) -> bool {
    s.commands().into_iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no command {id}")).enabled
}

fn photos(s: &mut Session, n: usize) -> Vec<PhotoId> {
    s.visible_cloned().into_iter().take(n).collect()
}

#[test]
fn the_stack_fact_follows_the_selection_and_the_catalog() {
    let mut s = Session::with_demo();
    let ids = photos(&mut s, 4);
    s.selection = Selection { ids: ids[..2].to_vec(), active: Some(ids[0]) };
    assert!(!enabled(&s, "stack.ungroup"), "nothing is stacked yet");
    // the catalog changes under the same selection: the answer is worked out again
    s.execute("stack.group", &json!({"ids": [ids[0].0, ids[1].0]})).unwrap();
    s.selection = Selection { ids: ids[..2].to_vec(), active: Some(ids[0]) };
    assert!(enabled(&s, "stack.ungroup"));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(!enabled(&s, "stack.ungroup"), "undoing the stack changed the catalog under the same selection");
    s.execute("edit.redo", &json!({})).unwrap();
    s.selection = Selection { ids: ids[..2].to_vec(), active: Some(ids[0]) };
    assert!(enabled(&s, "stack.ungroup"));
    // the selection's ids are changed in place, as the UI and commands do
    s.selection.ids = vec![ids[2], ids[3]];
    assert!(!enabled(&s, "stack.ungroup"));
    s.selection.ids.push(ids[1]);
    assert!(enabled(&s, "stack.ungroup"));
    s.selection.ids.retain(|id| *id != ids[1]);
    assert!(!enabled(&s, "stack.ungroup"));
    // no selected ids: the active photo stands in for them
    s.selection = Selection { ids: Vec::new(), active: Some(ids[0]) };
    assert!(enabled(&s, "stack.ungroup"));
    s.selection.active = Some(ids[2]);
    assert!(!enabled(&s, "stack.ungroup"), "a changed active photo is a changed selection");
}

#[test]
fn the_outdated_process_fact_follows_an_edit_to_a_selected_photo() {
    let mut s = Session::with_demo();
    let ids = photos(&mut s, 3);
    s.selection = Selection { ids: ids.clone(), active: Some(ids[0]) };
    assert!(!enabled(&s, "develop.updateProcess"));
    let mut d = (*s.develop_of(ids[2]).unwrap()).clone();
    d.process = ProcessVersion(0);
    s.set_develop(ids[2], d, "test").unwrap();
    assert!(enabled(&s, "develop.updateProcess"), "a selected photo went onto an older process");
    s.execute("develop.updateProcess", &json!({})).unwrap();
    assert!(!enabled(&s, "develop.updateProcess"), "and came off it");
}

/// A selection past the size that is fingerprinted on every ask: changes made in place between
/// frames are seen at the next frame, and the view's membership follows them.
#[test]
fn a_long_selection_changed_in_place_is_seen_at_the_next_frame() {
    let ids: Vec<PhotoId> = (1..=5000).map(PhotoId).collect();
    let mut s = Session::with_demo();
    s.selection = Selection { ids: ids.clone(), active: Some(ids[0]) };
    let key = s.selection_key();
    assert!(s.selection_view().contains(ids[2500]));
    // the same length and ends, one photo in the middle swapped
    s.selection.ids[2500] = PhotoId(9_999_999);
    s.begin_frame();
    assert_ne!(s.selection_key(), key, "a change in the middle is a different selection");
    assert!(!s.selection_view().contains(ids[2500]) && s.selection_view().contains(PhotoId(9_999_999)));
    // clicking another photo inside the selection keeps the key
    let key = s.selection_key();
    s.selection.active = Some(ids[1]);
    assert_eq!(s.selection_key(), key);
    assert_eq!(s.selection_view().state_of(ids[1]), SelectionState::Active);
    assert_eq!(s.selection_view().state_of(ids[3]), SelectionState::Selected);
    assert_eq!(s.selection_view().state_of(PhotoId(77_777)), SelectionState::NotSelected);
}

#[test]
fn a_short_selection_changed_in_place_is_seen_at_once() {
    let ids: Vec<PhotoId> = (1..=6).map(PhotoId).collect();
    let mut s = Session::with_demo();
    s.selection = Selection { ids: ids[..3].to_vec(), active: None };
    let key = s.selection_key();
    assert!(s.selection_view().contains(ids[1]) && !s.selection_view().contains(ids[4]));
    s.selection.ids[1] = ids[4];
    assert_ne!(s.selection_key(), key);
    assert!(!s.selection_view().contains(ids[1]) && s.selection_view().contains(ids[4]));
    s.selection.ids.reverse();
    let reversed = s.selection_key();
    s.selection.ids.reverse();
    assert_ne!(s.selection_key(), reversed, "the order counts");
}

#[test]
fn clicking_another_photo_inside_a_selection_keeps_its_key() {
    let mut s = Session::with_demo();
    let ids = photos(&mut s, 3);
    s.selection = Selection { ids: ids.clone(), active: Some(ids[0]) };
    let key = s.selection_key();
    s.selection.active = Some(ids[1]);
    assert_eq!(s.selection_key(), key, "the same photos are selected");
    s.selection = Selection { ids: Vec::new(), active: Some(ids[0]) };
    let alone = s.selection_key();
    s.selection.active = Some(ids[1]);
    assert_ne!(s.selection_key(), alone, "with none selected the active photo is the selection");
}

/// `fingerprint` of [] and of [PhotoId(32)] hash alike; the length tells them apart.
#[test]
fn an_empty_selection_and_photo_32_alone_are_different_selections() {
    let mut s = Session::with_demo();
    s.selection = Selection { ids: Vec::new(), active: None };
    let empty = s.selection_key();
    assert!(!s.selection_view().contains(PhotoId(32)));
    s.selection = Selection { ids: vec![PhotoId(32)], active: Some(PhotoId(32)) };
    assert_ne!(s.selection_key(), empty);
    assert!(s.selection_view().contains(PhotoId(32)));
    s.selection.ids.clear();
    assert!(!s.selection_view().contains(PhotoId(32)), "deselected again");
}

#[test]
fn a_long_selection_that_grows_within_a_frame_is_seen_at_once() {
    let ids: Vec<PhotoId> = (1..=5000).map(PhotoId).collect();
    let mut s = Session::with_demo();
    s.selection = Selection { ids: ids.clone(), active: Some(ids[0]) };
    let key = s.selection_key();
    assert!(!s.selection_view().contains(PhotoId(9_000_000)));
    s.selection.ids.push(PhotoId(9_000_000));
    assert_ne!(s.selection_key(), key);
    assert!(s.selection_view().contains(PhotoId(9_000_000)));
}

#[test]
fn a_command_that_swaps_selected_ids_after_reading_the_index_leaves_no_stale_answer() {
    let ids: Vec<PhotoId> = (1..=2000).map(PhotoId).collect();
    let mut s = Session::with_demo();
    s.selection = Selection { ids: ids.clone(), active: Some(ids[0]) };
    let key = s.selection_key();
    s.execute_fn("test", |s| {
        assert!(s.selection_view().contains(ids[1000]));
        // the same length, first and last ids
        s.selection.ids[1000] = PhotoId(9_000_000);
        Ok(json!({}))
    })
    .unwrap();
    assert_ne!(s.selection_key(), key);
    assert!(!s.selection_view().contains(ids[1000]) && s.selection_view().contains(PhotoId(9_000_000)));
}
