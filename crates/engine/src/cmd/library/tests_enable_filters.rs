//! `library.toggleFilters` (Library > Enable Filters, Cmd+L) through the real engine.
use lightcraft_catalog::{Filter, Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{LibrarySource, Result, Selection, Session};

fn session() -> Result<Session> {
    let mut s = Session::new();
    for (id, rating, flag) in [(1, 5, Flag::Pick), (2, 5, Flag::None), (3, 2, Flag::Pick), (4, 1, Flag::None)] {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-enable-filters/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-23",
        );
        photo.rating = rating;
        photo.flag = flag;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    Ok(s)
}

fn stars_and_pick(s: &mut Session) {
    s.execute("library.filter", &json!({"rating": 4, "flag": "pick"})).unwrap();
}

#[test]
fn off_shows_everything_and_on_brings_back_the_same_filter() {
    let mut s = session().unwrap();
    stars_and_pick(&mut s);
    let set = s.filter.clone();
    assert_eq!(s.visible().len(), 1, "the filter hides photos, or off and on would look alike");
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!((r["enabled"].clone(), r["count"].clone()), (json!(false), json!(4)));
    assert_eq!(s.filter, Filter::default());
    assert_eq!(s.visible().len(), 4);
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!((r["enabled"].clone(), r["count"].clone()), (json!(true), json!(1)));
    assert_eq!(s.filter, set, "stars and flag come back together");
    assert_eq!(s.visible().len(), 1);
}

#[test]
fn with_no_filter_it_does_nothing_and_leaves_nothing_to_bring_back() {
    let mut s = session().unwrap();
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!((r["enabled"].clone(), r["count"].clone()), (json!(true), json!(4)));
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(r["enabled"], json!(true));
    assert_eq!(s.filter, Filter::default());
    assert!(s.shelved_filter.is_none());
}

#[test]
fn setting_a_filter_while_off_turns_filtering_on_and_forgets_the_old_one() {
    let mut s = session().unwrap();
    stars_and_pick(&mut s);
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.filter", &json!({"rating": 2})).unwrap();
    assert_eq!(s.visible().len(), 3);
    // off again, then on: the filter that was set last, not the first one
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter.rating, 2);
    assert_eq!(s.filter.flag, None);
}

#[test]
fn emptying_the_filter_by_hand_while_off_leaves_nothing_to_bring_back() {
    let mut s = session().unwrap();
    stars_and_pick(&mut s);
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.filter", &json!({"rating": 2})).unwrap();
    s.execute("library.filter", &json!({"rating": 0})).unwrap();
    assert_eq!(s.filter, Filter::default());
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter, Filter::default(), "the filter from before it was switched off is not back");
    assert_eq!(s.visible().len(), 4);
}

#[test]
fn clearing_the_filter_while_off_leaves_nothing_to_bring_back() {
    let mut s = session().unwrap();
    stars_and_pick(&mut s);
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.clearFilter", &json!({})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter, Filter::default());
    assert_eq!(s.visible().len(), 4);
}

#[test]
fn applying_a_preset_while_off_turns_filtering_on_with_the_preset() {
    let mut s = session().unwrap();
    s.filter_presets.push(super::super::filters::FilterPreset { name: "Fives".into(), filter: Filter { rating: 5, ..Default::default() } });
    stars_and_pick(&mut s);
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("filter.applyPreset", &json!({"name": "Fives"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter.rating, 5);
    assert_eq!(s.filter.flag, None);
}

#[test]
fn a_folder_filter_does_not_come_back_over_the_folder_now_shown() {
    let mut s = session().unwrap();
    s.execute("library.filter", &json!({"rating": 4, "libraryFolder": "/lightcraft-enable-filters"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.source = LibrarySource::LibraryFolder;
    s.library_folder = Some("/lightcraft-enable-filters".into());
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter.rating, 4);
    assert_eq!(s.filter.library_folder, None);
}

#[test]
fn removing_a_folder_while_off_drops_it_from_the_filter_put_aside() {
    let mut s = session().unwrap();
    s.execute("library.filter", &json!({"rating": 4, "libraryFolder": "/lightcraft-enable-filters"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.removeFolder", &json!({"path": "/lightcraft-enable-filters"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter.library_folder, None, "the removed folder is not brought back");
    assert_eq!(s.filter.rating, 4, "the rest of the filter is");
}

#[test]
fn removing_the_only_criterion_put_aside_leaves_filtering_on() {
    let mut s = session().unwrap();
    s.execute("library.filter", &json!({"libraryFolder": "/lightcraft-enable-filters"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.removeFolder", &json!({"path": "/lightcraft-enable-filters"})).unwrap();
    assert!(s.shelved_filter.is_none(), "an empty filter is not left put aside");
}

#[test]
fn a_keyword_renamed_while_off_is_followed_by_the_filter_put_aside() {
    let mut s = session().unwrap();
    let mut photo = Photo::new(PhotoId(5), Source::File { path: "/lightcraft-enable-filters/5.jpg".into() }, "5.jpg", "JPEG", 40, 30, "2026-10-23");
    photo.meta.keywords = vec!["cat".into()];
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    s.filter.keyword = Some("cat".into());
    assert_eq!(s.visible().len(), 1);
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("keyword.rename", &json!({"from": "cat", "to": "dog"})).unwrap();
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(r["enabled"], json!(true));
    assert_eq!(s.filter.keyword.as_deref(), Some("dog"), "the put-aside filter follows the rename");
    assert_eq!(s.visible().len(), 1);
}

/// Two albums: the first holds photo 1, the second photo 3.
fn session_with_two_albums() -> (Session, u64, u64) {
    let mut s = session().unwrap();
    let mut make = |name: &str, ids: &[u64]| {
        s.selection = Selection { ids: ids.iter().map(|i| PhotoId(*i)).collect(), active: None };
        s.execute("album.create", &json!({"name": name, "addSelected": true})).unwrap()["id"].as_u64().unwrap()
    };
    let (x, a) = (make("X", &[1]), make("A", &[3]));
    (s, x, a)
}

#[test]
fn an_album_filter_does_not_come_back_over_the_album_now_shown() {
    let (mut s, x, a) = session_with_two_albums();
    s.execute("library.filter", &json!({"album": x, "rating": 4})).unwrap();
    assert_eq!(s.visible().len(), 1, "the album filter hides photo 2, or off and on would look alike");
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter.rating, 4, "the rest of the filter is back");
    assert_eq!(s.filter.album, None, "one album at a time: the one shown wins, as for a folder");
    // the old album must not narrow the next source, with no chip to show it
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(s.visible().len(), 2, "photos 1 and 2 have four stars or more");
}

#[test]
fn an_album_filter_comes_back_when_no_album_is_shown() {
    let (mut s, x, _) = session_with_two_albums();
    s.execute("library.filter", &json!({"album": x})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter.album.map(|a| a.0), Some(x));
}
