//! The label filter's "no label" (issue #1192) through the real engine: `library.filter` takes
//! `labelsNone`, it ANDs with the colour multi-select, and the text search's `label:none` asks for
//! the same photos.
use lcc_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Session};

/// Three photos: one without a label, one red, one green.
fn session() -> Result<Session> {
    let mut s = Session::new();
    for (id, label) in [(1, None), (2, Some("red")), (3, Some("green"))] {
        let mut photo =
            Photo::new(PhotoId(id), Source::File { path: format!("/label-none/{id}.jpg") }, &format!("{id}.jpg"), "JPEG", 40, 30, "2026-11-02");
        photo.label = label.and_then(lcc_catalog::ColorLabel::parse);
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
fn it_filters_the_unlabeled_and_back() -> Result<()> {
    let mut s = session()?;
    s.execute("library.filter", &json!({"labelsNone": true}))?;
    assert_eq!(shown(&mut s), vec![1]);
    // ANDs with the colour multi-select: red and no label match nothing
    s.execute("library.filter", &json!({"labels": ["red"]}))?;
    assert_eq!(shown(&mut s), Vec::<u64>::new());
    s.execute("library.filter", &json!({"labels": [], "labelsNone": false}))?;
    assert_eq!(shown(&mut s).len(), 3, "cleared, every photo is back");
    Ok(())
}

#[test]
fn text_search_label_none() -> Result<()> {
    let mut s = session()?;
    s.execute("library.filter", &json!({"text": "label:none"}))?;
    assert_eq!(shown(&mut s), vec![1]);
    s.execute("library.filter", &json!({"text": "color:red"}))?;
    assert_eq!(shown(&mut s), vec![2]);
    Ok(())
}
