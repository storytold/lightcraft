//! A rating, flag or label that takes the active photo out of the view (the filter bar, Picks) goes on from its place:
//! the photo that took it, not the view's first photo (#2312).
use lightcraft_catalog::{ColorLabel, Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Selection, Session};

/// 100 photos with one star each, in file-name order (photo `n` is the `n`th), the filter showing one-star photos.
fn one_star() -> Result<Session> {
    let mut s = Session::new();
    for id in 1..=100 {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-rated-out/{id:03}.jpg") },
            &format!("{id:03}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-19",
        );
        photo.rating = 1;
        photo.flag = Flag::Pick;
        photo.label = Some(ColorLabel::Red);
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.filter", &json!({"rating": 1, "ratingOp": "exactly"}))?;
    Ok(s)
}

#[test]
fn a_star_taken_off_goes_on_to_the_next_photo() {
    // Given photo 82 of 100 one-star photos, the filter showing one-star photos
    let mut s = one_star().unwrap();
    s.execute("library.select", &json!({"ids": [82]})).unwrap();
    // When its star is taken off, Then it leaves the view and the photo after it is the one selected
    s.execute("photo.rate", &json!({"rating": 0})).unwrap();
    assert!(!s.visible_cloned().contains(&PhotoId(82)));
    assert_eq!(s.selection, Selection::single(PhotoId(83)));
    // and the next photo is the one after that, not the first one of the view
    s.execute("library.next", &json!({})).unwrap();
    assert_eq!(s.active(), Some(PhotoId(84)));
    // With `advance`, the photo that took its place is the next one: no photo is skipped
    s.execute("photo.rate", &json!({"rating": 0, "advance": true})).unwrap();
    assert_eq!(s.active(), Some(PhotoId(85)));
    s.execute("photo.decreaseRating", &json!({"advance": true})).unwrap();
    assert_eq!(s.active(), Some(PhotoId(86)));
    // The last photo of the view hands on to the one before it
    s.execute("library.select", &json!({"ids": [100]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 0, "advance": true})).unwrap();
    assert_eq!(s.active(), Some(PhotoId(99)));
    // A rating that keeps the photo in the view advances as before
    s.execute("photo.rate", &json!({"rating": 1, "advance": true})).unwrap();
    assert_eq!(s.active(), Some(PhotoId(99)), "the last photo of the view stays");
    s.execute("library.select", &json!({"ids": [10]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 1, "advance": true})).unwrap();
    assert_eq!(s.active(), Some(PhotoId(11)));
}

#[test]
fn flags_and_labels_that_leave_the_view_go_on_to_the_next_photo_too() {
    for (filter, command, params) in [
        (json!({"flag": "pick"}), "photo.reject", json!({})),
        (json!({"flag": "pick"}), "photo.flag", json!({"flag": "none"})),
        (json!({"flag": "pick"}), "photo.cycleFlag", json!({})),
        (json!({"flag": "pick"}), "photo.decreaseFlag", json!({})),
        (json!({"label": "red"}), "photo.label", json!({"label": "none"})),
        (json!({"label": "red"}), "photo.clearLabel", json!({})),
        (json!({}), "photo.increaseRating", json!({})),
    ] {
        let mut s = one_star().unwrap();
        s.execute("library.filter", &filter).unwrap();
        s.execute("library.select", &json!({"ids": [40]})).unwrap();
        s.execute(command, &params).unwrap();
        assert!(!s.visible_cloned().contains(&PhotoId(40)), "{command} {params}");
        assert_eq!(s.selection, Selection::single(PhotoId(41)), "{command} {params}");
    }
}

#[test]
fn several_photos_rated_out_go_on_after_the_active_one() {
    let mut s = one_star().unwrap();
    // three selected, the middle one active: all leave, the photo after the active one is selected
    s.execute("library.select", &json!({"ids": [20, 21, 22], "active": 21})).unwrap();
    s.execute("photo.rate", &json!({"rating": 0})).unwrap();
    assert_eq!(s.selection, Selection::single(PhotoId(23)));
    // the active one leaves and another selected photo stays: that one is active
    s.execute("photo.rate", &json!({"ids": [30], "rating": 3})).unwrap();
    s.execute("library.filter", &json!({"rating": 1, "ratingOp": "atLeast"})).unwrap();
    s.execute("library.select", &json!({"ids": [29, 30], "active": 29})).unwrap();
    s.execute("photo.decreaseRating", &json!({})).unwrap();
    assert_eq!(s.selection, Selection { ids: vec![PhotoId(30)], active: Some(PhotoId(30)) });
}

#[test]
fn named_photos_and_a_held_view_keep_the_selection() {
    // Photos named by `ids` leave the selection to the caller (Compare and Survey keep the photo they show)
    let mut s = one_star().unwrap();
    s.execute("library.select", &json!({"ids": [5, 6], "active": 6})).unwrap();
    s.execute("photo.rate", &json!({"ids": [6], "rating": 0})).unwrap();
    assert_eq!(s.selection, Selection { ids: vec![PhotoId(5), PhotoId(6)], active: Some(PhotoId(6)) });
    // A held view keeps the photo, so the selection stays with it
    let mut s = one_star().unwrap();
    s.execute("library.holdView", &json!({"on": true})).unwrap();
    // (what the grid shows is what is held)
    assert_eq!(s.visible_cloned().len(), 100);
    s.execute("library.select", &json!({"ids": [7]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 0})).unwrap();
    assert!(s.visible_cloned().contains(&PhotoId(7)));
    assert_eq!(s.selection, Selection::single(PhotoId(7)));
    s.execute("photo.rate", &json!({"rating": 0, "advance": true})).unwrap();
    assert_eq!(s.active(), Some(PhotoId(8)));
}

/// A slider drag still open when a rating takes the photo out of the view, or when the photo is
/// deleted, ends as its own undo step before that change, as moving to the next photo does: undo
/// brings the rating or the photo back first, then the edit. Before, the move left the drag open
/// with no undo step (CodeRabbit on #2689).
#[test]
fn an_open_edit_ends_before_the_selection_moves_on() {
    let exposure = |s: &Session, id| s.develop_of(PhotoId(id)).map(|d| d.light.exposure);
    for (command, params) in [("photo.rate", json!({"rating": 0})), ("photo.delete", json!({})), ("photo.deletePermanently", json!({}))] {
        let mut s = one_star().unwrap();
        s.execute("library.select", &json!({"ids": [82]})).unwrap();
        s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.5})).unwrap();
        s.execute(command, &params).unwrap();
        assert_eq!(s.active(), Some(PhotoId(83)), "{command}");
        assert!(s.interaction.is_none(), "{command}: the drag ended with the move");
        if command != "photo.deletePermanently" {
            assert_eq!(exposure(&s, 82), Some(1.5), "{command}");
        }
        // undo: the rating or the photo comes back first, with the edit; then the edit goes
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.visible_cloned().contains(&PhotoId(82)), "{command}: back in the view");
        assert_eq!(exposure(&s, 82), Some(1.5), "{command}: with its edit");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(exposure(&s, 82), Some(0.0), "{command}");
    }
}
