//! `library.hideRejected` (View > Hide Rejected Photos, Cmd+Alt+X; issue #3513) through the real engine.
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Session};

fn session() -> Result<Session> {
    let mut s = Session::new();
    for (id, rating, flag) in [(1, 5, Flag::Pick), (2, 5, Flag::Reject), (3, 2, Flag::None), (4, 5, Flag::None), (5, 1, Flag::Reject)] {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-hide-rejected/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-11-02",
        );
        photo.rating = rating;
        photo.flag = flag;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    Ok(s)
}

fn shown(s: &mut Session) -> Vec<u64> {
    let mut ids: Vec<u64> = s.visible().iter().map(|p| p.0).collect();
    ids.sort();
    ids
}

#[test]
fn it_hides_the_rejects_and_shows_them_again() {
    let mut s = session().unwrap();
    assert!(!s.rejects_hidden());
    let r = s.execute("library.hideRejected", &json!({})).unwrap();
    assert_eq!((r["hidden"].clone(), r["count"].clone()), (json!(true), json!(3)));
    assert_eq!(shown(&mut s), vec![1, 3, 4]);
    assert!(s.rejects_hidden());
    // the same as Picked + Unflagged in the filter bar
    assert_eq!(s.filter.flags, vec![Flag::Pick, Flag::None]);
    let r = s.execute("library.hideRejected", &json!({})).unwrap();
    assert_eq!((r["hidden"].clone(), r["count"].clone()), (json!(false), json!(5)));
    assert!(!s.rejects_hidden());
    assert!(s.filter.flags.is_empty() && s.filter.flag.is_none());
}

#[test]
fn the_rest_of_the_filter_stays() {
    let mut s = session().unwrap();
    s.execute("library.filter", &json!({"rating": 5})).unwrap();
    s.execute("library.hideRejected", &json!({})).unwrap();
    assert_eq!(shown(&mut s), vec![1, 4], "five stars and not rejected");
    s.execute("library.hideRejected", &json!({})).unwrap();
    assert_eq!(shown(&mut s), vec![1, 2, 4], "five stars again, rejects included");
    assert_eq!(s.filter.rating, 5);
}

#[test]
fn another_flag_filter_is_replaced_and_hide_sets_the_state_asked_for() {
    let mut s = session().unwrap();
    // only rejects shown: Hide Rejected shows the others instead
    s.execute("library.filter", &json!({"flag": "reject"})).unwrap();
    assert!(!s.rejects_hidden());
    s.execute("library.hideRejected", &json!({})).unwrap();
    assert_eq!(shown(&mut s), vec![1, 3, 4]);
    // `hide` is a state, not a toggle
    let r = s.execute("library.hideRejected", &json!({"hide": true})).unwrap();
    assert_eq!(r["hidden"], json!(true));
    assert_eq!(shown(&mut s), vec![1, 3, 4]);
    s.execute("library.hideRejected", &json!({"hide": false})).unwrap();
    s.execute("library.hideRejected", &json!({"hide": false})).unwrap();
    assert_eq!(shown(&mut s), vec![1, 2, 3, 4, 5]);
    assert!(s.execute("library.hideRejected", &json!({"hide": "yes"})).is_err(), "hide is a bool");
    assert_eq!(shown(&mut s), vec![1, 2, 3, 4, 5], "a bad call changes nothing");
}

#[test]
fn while_filters_are_off_it_turns_filtering_on() {
    let mut s = session().unwrap();
    s.execute("library.filter", &json!({"rating": 5})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert!(s.filters_off());
    s.execute("library.hideRejected", &json!({})).unwrap();
    assert!(!s.filters_off(), "a filter change forgets the one put aside, as library.filter does");
    assert_eq!(shown(&mut s), vec![1, 3, 4]);
}
