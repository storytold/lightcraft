//! `library.source` with an album while the filter holds an album: the album shown wins.
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Selection, Session};

/// Photos 1 to 3 (rated 5, 5 and 2). Album X holds photo 1 and album A holds photo 3.
fn session_with_two_albums() -> Result<(Session, u64, u64)> {
    let mut s = Session::new();
    for (id, rating) in [(1, 5), (2, 5), (3, 2)] {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-source-album/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-23",
        );
        photo.rating = rating;
        photo.flag = Flag::None;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    let mut make = |name: &str, id: u64| {
        s.selection = Selection { ids: vec![PhotoId(id)], active: None };
        s.execute("album.create", &json!({"name": name, "addSelected": true})).unwrap()["id"].as_u64().unwrap()
    };
    let (x, a) = (make("X", 1), make("A", 3));
    Ok((s, x, a))
}

#[test]
fn showing_an_album_drops_the_album_the_filter_held() {
    let (mut s, x, a) = session_with_two_albums().unwrap();
    s.execute("library.filter", &json!({"album": x, "rating": 4})).unwrap();
    assert_eq!(s.visible().len(), 1, "the album narrows the grid, or the hidden album would not matter");
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    assert_eq!(s.filter.album, None, "one album at a time: the one shown wins, as for a library folder");
    assert_eq!(s.filter.rating, 4, "the rest of the filter stays");
    // the old album must not narrow the next source, with no chip to show it
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(s.visible().len(), 2, "photos 1 and 2 have four stars or more");
}

#[test]
fn the_album_filter_stays_while_the_source_is_not_an_album() {
    let (mut s, x, _) = session_with_two_albums().unwrap();
    s.execute("library.filter", &json!({"album": x})).unwrap();
    s.execute("library.source", &json!({"kind": "picks"})).unwrap();
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(s.filter.album.map(|a| a.0), Some(x));
    assert_eq!(s.visible().len(), 1);
}

#[test]
fn a_refused_album_source_leaves_the_filter_alone() {
    let (mut s, x, _) = session_with_two_albums().unwrap();
    s.execute("library.filter", &json!({"album": x})).unwrap();
    assert!(s.execute("library.source", &json!({"kind": "album", "id": 9999})).is_err());
    assert_eq!(s.filter.album.map(|a| a.0), Some(x));
}
