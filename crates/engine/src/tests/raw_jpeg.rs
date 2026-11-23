//! Raw + JPEG pairs are kept as stacks, the raw on top (`library.groupRawJpeg`).

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::Session;

fn add(s: &mut Session, id: u64, name: &str) -> PhotoId {
    let id = PhotoId(id);
    let raw = name.ends_with(".ARW");
    let mut photo =
        Photo::new(id, Source::File { path: format!("/shoot/{name}") }, name, if raw { "ARW" } else { "JPEG" }, 2, 2, "2026-10-01T00:00:00");
    if raw {
        photo.kind = MediaKind::Raw;
    }
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    id
}

/// `IMG_1.ARW` + `IMG_1.JPG`, `IMG_2.ARW` + `IMG_2.JPG` and a lone `IMG_3.JPG`, in name order.
fn shoot() -> (Session, Vec<PhotoId>) {
    let mut s = Session::new();
    let ids = ["IMG_1.ARW", "IMG_1.JPG", "IMG_2.ARW", "IMG_2.JPG", "IMG_3.JPG"]
        .iter()
        .enumerate()
        .map(|(i, name)| add(&mut s, i as u64 + 1, name))
        .collect();
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    (s, ids)
}

fn stacks(s: &Session) -> Vec<Vec<PhotoId>> {
    let mut all: Vec<Vec<PhotoId>> = s.catalog.stacks().map(|st| st.photos.clone()).collect();
    all.sort();
    all
}

#[test]
fn turning_it_on_stacks_the_pairs_and_turning_it_off_takes_them_apart() {
    let (mut s, ids) = shoot();
    assert_eq!(s.visible_cloned(), ids, "off by default: nothing is stacked");
    let on = s.execute("library.groupRawJpeg", &json!({})).unwrap();
    assert_eq!(on, json!({"group": true, "stacks": 2}), "toggles without `on`");
    assert_eq!(stacks(&s), vec![vec![ids[0], ids[1]], vec![ids[2], ids[3]]], "real stacks, the raw on top");
    assert_eq!(s.visible_cloned(), vec![ids[0], ids[2], ids[4]], "collapsed: each shot once; the lone JPEG stays");
    // they are ordinary stacks: expanding one shows its JPEG
    s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
    s.execute("stack.toggle", &json!({})).unwrap();
    assert_eq!(s.visible_cloned(), vec![ids[0], ids[1], ids[2], ids[4]]);
    assert_eq!(s.execute("library.groupRawJpeg", &json!({"on": false})).unwrap(), json!({"group": false, "stacks": 2}));
    assert!(stacks(&s).is_empty());
    assert_eq!(s.visible_cloned(), ids);
}

#[test]
fn stacking_is_one_undo_step() {
    let (mut s, ids) = shoot();
    s.execute("library.groupRawJpeg", &json!({"on": true})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(stacks(&s).is_empty());
    assert_eq!(s.visible_cloned(), ids);
}

#[test]
fn photos_added_while_it_is_on_are_stacked_with_their_other_half() {
    let (mut s, ids) = shoot();
    s.execute("library.groupRawJpeg", &json!({"on": true})).unwrap();
    // what an import does after it commits a batch: a raw for the lone JPEG, and a new lone raw
    let raw = add(&mut s, 10, "IMG_3.ARW");
    let lone = add(&mut s, 11, "IMG_4.ARW");
    assert_eq!(s.stack_raw_jpeg_pairs(Some(&[raw, lone])).unwrap(), 1);
    assert_eq!(stacks(&s), vec![vec![ids[0], ids[1]], vec![ids[2], ids[3]], vec![raw, ids[4]]]);
}

#[test]
fn the_active_photo_moves_from_a_folded_jpeg_to_its_raw() {
    let (mut s, ids) = shoot();
    s.execute("library.select", &json!({"ids": [ids[1].0]})).unwrap();
    s.execute("library.groupRawJpeg", &json!({"on": true})).unwrap();
    assert_eq!(s.selection.active, Some(ids[0]));
}

#[test]
fn asking_for_the_state_it_has_changes_nothing_and_a_word_is_not_a_bool() {
    let (mut s, ids) = shoot();
    assert_eq!(s.execute("library.groupRawJpeg", &json!({"on": false})).unwrap(), json!({"group": false, "stacks": 0}));
    assert_eq!(s.visible_cloned(), ids);
    assert!(s.execute("library.groupRawJpeg", &json!({"on": "true"})).is_err());
    assert!(!s.group_raw_jpeg, "a refused call changes nothing");
}
