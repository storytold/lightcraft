//! `library.toggleFilters` (Library > Enable Filters, Cmd+L) through the real engine.
use lightcraft_catalog::{Filter, Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{LibrarySource, Result, Session};

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

/// The session's photos plus one photo that carries the keyword "cat".
fn session_with_a_cat() -> Session {
    let mut s = session().unwrap();
    let mut photo = Photo::new(PhotoId(5), Source::File { path: "/lightcraft-enable-filters/5.jpg".into() }, "5.jpg", "JPEG", 40, 30, "2026-10-23");
    photo.meta.keywords = vec!["cat".into()];
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    s
}

#[test]
fn deleting_the_only_keyword_put_aside_leaves_filtering_on() {
    let mut s = session_with_a_cat();
    s.execute("library.filter", &json!({"keyword": "cat"})).unwrap();
    assert_eq!(s.visible().len(), 1);
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert!(s.filters_off());
    s.execute("keyword.delete", &json!({"keyword": "cat"})).unwrap();
    assert!(!s.filters_off(), "an empty filter is not left put aside");
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!((r["enabled"].clone(), r["count"].clone()), (json!(true), json!(5)), "nothing was put aside, so nothing is switched off");
    assert_eq!(s.filter, Filter::default());
}

#[test]
fn deleting_a_keyword_put_aside_keeps_the_rest_of_the_filter() {
    let mut s = session_with_a_cat();
    s.execute("library.filter", &json!({"keyword": "cat", "rating": 4})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("keyword.delete", &json!({"keyword": "cat"})).unwrap();
    assert!(s.filters_off(), "the stars are still put aside");
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter, Filter { rating: 4, ..Default::default() });
}

#[test]
fn a_rating_operator_without_stars_does_not_keep_an_emptied_filter_put_aside() {
    let mut s = session_with_a_cat();
    // the filter bar's operator picker can be set with no stars chosen; it filters nothing
    s.execute("library.filter", &json!({"keyword": "cat", "ratingOp": "exactly"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert!(s.filters_off());
    s.execute("keyword.delete", &json!({"keyword": "cat"})).unwrap();
    assert!(!s.filters_off(), "only an operator with no stars is left: nothing to bring back");
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(r["enabled"], json!(true));
}

#[test]
fn a_rating_operator_put_aside_survives_a_keyword_change_that_does_not_touch_it() {
    let mut s = session_with_a_cat();
    s.execute("library.filter", &json!({"ratingOp": "exactly"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert!(s.filters_off());
    s.execute("keyword.rename", &json!({"from": "cat", "to": "dog"})).unwrap();
    assert!(s.filters_off(), "the filter put aside held no keyword, so the rename leaves it alone");
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(s.filter.rating_op, lightcraft_catalog::RatingOp::Exactly);
}

#[test]
fn a_rating_operator_with_stars_is_kept_when_the_keyword_goes() {
    let mut s = session_with_a_cat();
    s.execute("library.filter", &json!({"keyword": "cat", "rating": 4, "ratingOp": "exactly"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("keyword.delete", &json!({"keyword": "cat"})).unwrap();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!((s.filter.rating, s.filter.rating_op), (4, lightcraft_catalog::RatingOp::Exactly));
}
