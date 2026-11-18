//! Next / Previous Unrated Photo (ported from upstream #3904): jump over rated photos, wrap, keep a multi-selection.
use lcc_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Session};

fn session() -> Result<Session> {
    let mut s = Session::new();
    for (id, rating) in [(1, 5), (2, 1), (3, 0), (4, 0)] {
        let mut photo =
            Photo::new(PhotoId(id), Source::File { path: format!("/next-unrated/{id}.jpg") }, &format!("{id}.jpg"), "JPEG", 40, 30, "2026-10-09");
        photo.rating = rating;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [1]}))?;
    Ok(s)
}

#[test]
fn next_and_previous_skip_rated_photos_and_wrap() {
    // next skips rated photos and wraps
    {
        let mut s = session().unwrap();
        s.execute("library.nextUnrated", &json!({})).unwrap();
        assert_eq!(s.active(), Some(PhotoId(3)));
        s.execute("library.nextUnrated", &json!({})).unwrap();
        assert_eq!(s.active(), Some(PhotoId(4)));
        s.execute("library.nextUnrated", &json!({})).unwrap();
        assert_eq!(s.active(), Some(PhotoId(3)), "wraps past the rated photos at the start");
    }
    // previous skips rated photos and wraps
    {
        let mut s = session().unwrap();
        assert_eq!(s.execute("library.prevUnrated", &json!({})).unwrap(), json!({"active": 4}));
        s.execute("library.prevUnrated", &json!({})).unwrap();
        assert_eq!(s.active(), Some(PhotoId(3)));
    }
    // everything rated says so and stays
    {
        let mut s = session().unwrap();
        s.execute("photo.rate", &json!({"ids": [3, 4], "rating": 5})).unwrap();
        for c in ["library.nextUnrated", "library.prevUnrated"] {
            assert_eq!(s.execute(c, &json!({})).unwrap(), json!({"allRated": true}));
            assert_eq!(s.active(), Some(PhotoId(1)));
        }
    }
    // an empty view is not an error
    {
        let mut s = Session::new();
        assert!(s.execute("library.nextUnrated", &json!({})).is_ok());
        assert!(s.execute("library.prevUnrated", &json!({})).is_ok());
    }
}

#[test]
fn a_multi_selection_is_kept_and_the_target_added() {
    // a multi selection is kept and the target added
    {
        let mut s = session().unwrap();
        s.execute("library.select", &json!({"ids": [1, 2, 4], "active": 1})).unwrap();
        s.execute("library.nextUnrated", &json!({})).unwrap();
        assert_eq!(s.active(), Some(PhotoId(3)));
        assert_eq!(s.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(4), PhotoId(3)]);
    }
    // the only unrated photo keeps selection and mask
    {
        let mut s = session().unwrap();
        s.execute("photo.rate", &json!({"ids": [4], "rating": 5})).unwrap();
        s.execute("library.select", &json!({"ids": [1, 2, 3], "active": 3})).unwrap();
        s.active_mask = Some(7);
        let r = s.execute("library.nextUnrated", &json!({})).unwrap();
        assert_eq!(r, json!({"active": 3}));
        assert_eq!(s.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(3)]);
        assert_eq!(s.active_mask, Some(7));
    }
}
