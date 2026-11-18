//! ← / → with several photos selected step through the selection, keep it and wrap around (test notes 2026-10-28
//! note 16); with one photo selected they step through the whole view as before.
use lcc_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Selection, Session};

/// 30 photos in file-name order (photo `n` is the `n`th of the view).
fn thirty() -> Result<Session> {
    let mut s = Session::new();
    for id in 1..=30 {
        let photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/step-selection/{id:03}.jpg") },
            &format!("{id:03}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-28",
        );
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    Ok(s)
}

fn ids(v: &[u64]) -> Vec<PhotoId> {
    v.iter().copied().map(PhotoId).collect()
}

/// Steps `n` times with `command`, the active photo after each.
fn walk(s: &mut Session, command: &str, n: usize) -> Vec<u64> {
    (0..n)
        .map(|_| {
            s.execute(command, &json!({})).unwrap();
            s.active().unwrap().0
        })
        .collect()
}

fn selected(s: &Session) -> Vec<u64> {
    let mut v: Vec<u64> = s.selection.ids.iter().map(|x| x.0).collect();
    v.sort_unstable();
    v
}

#[test]
fn arrows_step_through_a_multi_selection_and_wrap() {
    // arrows step through ctrl clicked photos and wrap
    {
        let mut s = thirty().unwrap();
        s.execute("library.select", &json!({"ids": [5]})).unwrap();
        s.execute("library.select", &json!({"ids": [13], "mode": "toggle"})).unwrap();
        s.execute("library.select", &json!({"ids": [25], "mode": "toggle"})).unwrap();
        s.execute("library.select", &json!({"ids": [5], "mode": "click"})).unwrap();
        assert_eq!(s.active(), Some(PhotoId(5)));
        assert_eq!(walk(&mut s, "library.next", 4), vec![13, 25, 5, 13], "→ skips the photos between and wraps");
        assert_eq!(selected(&s), vec![5, 13, 25], "the selection is kept");
        assert_eq!(walk(&mut s, "library.previous", 4), vec![5, 25, 13, 5], "← wraps from the first to the last");
        assert_eq!(selected(&s), vec![5, 13, 25]);
    }
    // arrows loop through a shift range
    {
        let mut s = thirty().unwrap();
        s.execute("library.select", &json!({"ids": [10]})).unwrap();
        s.execute("library.select", &json!({"ids": [13], "mode": "range"})).unwrap();
        assert_eq!(selected(&s), vec![10, 11, 12, 13]);
        s.execute("library.select", &json!({"ids": [10], "mode": "click"})).unwrap();
        assert_eq!(walk(&mut s, "library.next", 5), vec![11, 12, 13, 10, 11]);
        assert_eq!(walk(&mut s, "library.previous", 3), vec![10, 13, 12]);
        assert_eq!(selected(&s), vec![10, 11, 12, 13]);
    }
    // arrows visit a range and a far photo in view order
    {
        let mut s = thirty().unwrap();
        s.execute("library.select", &json!({"ids": [3]})).unwrap();
        s.execute("library.select", &json!({"ids": [5], "mode": "range"})).unwrap();
        s.execute("library.select", &json!({"ids": [20], "mode": "toggle"})).unwrap();
        s.execute("library.select", &json!({"ids": [3], "mode": "click"})).unwrap();
        assert_eq!(walk(&mut s, "library.next", 5), vec![4, 5, 20, 3, 4]);
        assert_eq!(walk(&mut s, "library.previous", 3), vec![3, 20, 5]);
        assert_eq!(selected(&s), vec![3, 4, 5, 20]);
    }
}

#[test]
fn one_photo_selected_steps_through_the_view_as_before() {
    let mut s = thirty().unwrap();
    s.execute("library.select", &json!({"ids": [29]})).unwrap();
    assert_eq!(walk(&mut s, "library.next", 2), vec![30, 30], "the end of the view stops, no wrap");
    assert_eq!(s.selection, Selection::single(PhotoId(30)));
    assert_eq!(walk(&mut s, "library.previous", 1), vec![29]);
    assert_eq!(s.selection, Selection::single(PhotoId(29)));
    // ↓ / ↑ and Auto Advance still step through the view (only ← / → follow the selection)
    s.execute("library.select", &json!({"ids": ids(&[3, 4, 5]), "active": 3})).unwrap();
    s.execute("library.down", &json!({"rows": 10})).unwrap();
    assert_eq!(s.selection, Selection::single(PhotoId(13)));
}
