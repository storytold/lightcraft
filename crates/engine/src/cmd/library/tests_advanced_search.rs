//! `library.advancedSearch`: the Advanced Search panel's structured query as a command that lowers
//! into the library filter, so the grid shows its results (issue #846).
use lcc_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Session};

fn session() -> Result<Session> {
    let mut s = Session::new();
    let mut p = Photo::new(PhotoId(1), Source::File { path: "/app-advanced-search/1.jpg".into() }, "IMG_4001.jpg", "JPEG", 6000, 4000, "2026-05-01");
    p.rating = 4;
    p.meta.camera = "Canon EOS R6".into();
    p.meta.iso = Some(400);
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) })?;
    let v = Photo::new(PhotoId(2), Source::File { path: "/app-advanced-search/2.mp4".into() }, "clip.mp4", "MP4", 1920, 1080, "2026-06-01");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(v) })?;
    Ok(s)
}

fn visible(s: &mut Session) -> Vec<u64> {
    s.visible_cloned().into_iter().map(|id| id.0).collect()
}

#[test]
fn a_structured_query_filters_the_grid_and_a_null_one_clears_it() {
    let mut s = session().unwrap();
    let r = s.execute("library.advancedSearch", &json!({"query": {"words": "img_", "properties": {"rating": 3, "ratingOp": "atLeast"}}})).unwrap();
    assert_eq!(r["count"], 1);
    assert_eq!(visible(&mut s), vec![1]);
    // null and an absent query both clear it, as the panel's reset does
    s.execute("library.advancedSearch", &json!({"query": null})).unwrap();
    assert_eq!(visible(&mut s), vec![1, 2]);
    s.execute("library.advancedSearch", &json!({})).unwrap();
    assert_eq!(visible(&mut s), vec![1, 2]);
}

#[test]
fn a_hostile_query_is_an_error_naming_the_problem_never_an_empty_grid() {
    let mut s = session().unwrap();
    for query in [
        json!({"properties": {"rating": 9}}),
        json!({"properties": {"width": {"min": 100, "max": 50}}}),
        json!({"media": {"codec": "h264"}}),
        json!({"words": 5}),
        json!({"file": {"album": 99}}),
    ] {
        let e = s.execute("library.advancedSearch", &json!({"query": query})).unwrap_err();
        let e = e.to_string();
        assert!(!e.is_empty(), "{query}");
        // the filter is untouched by a refused query
        assert_eq!(visible(&mut s), vec![1, 2], "{query}");
    }
}
