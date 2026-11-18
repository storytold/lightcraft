//! `photo.clearLabel`: the colour label taken off, as a command that can have a key of its own (#2980).
use lcc_catalog::{ColorLabel, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Session};

fn session() -> Result<Session> {
    let mut s = Session::new();
    for (id, label) in [(1, Some(ColorLabel::Red)), (2, Some(ColorLabel::Purple)), (3, Some(ColorLabel::Blue)), (4, None)] {
        let mut photo =
            Photo::new(PhotoId(id), Source::File { path: format!("/app-clear-label/{id}.jpg") }, &format!("{id}.jpg"), "JPEG", 40, 30, "2026-10-25");
        photo.label = label;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    Ok(s)
}

fn labels(s: &Session) -> Vec<Option<ColorLabel>> {
    (1..=4).map(|id| s.catalog.photo(PhotoId(id)).unwrap().label).collect()
}

#[test]
fn the_selected_photos_lose_their_label_in_one_undo_step() {
    // the selected photos lose their label in one undo step
    {
        let mut s = session().unwrap();
        s.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
        s.execute("photo.clearLabel", &json!({})).unwrap();
        assert_eq!(labels(&s), [None, None, Some(ColorLabel::Blue), None]);
        assert_eq!(s.undo.len(), 1);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(labels(&s), [Some(ColorLabel::Red), Some(ColorLabel::Purple), Some(ColorLabel::Blue), None]);
    }
    // it does what photo label none does
    {
        let mut a = session().unwrap();
        let mut b = session().unwrap();
        for s in [&mut a, &mut b] {
            s.execute("library.select", &json!({"ids": [3]})).unwrap();
        }
        a.execute("photo.clearLabel", &json!({})).unwrap();
        b.execute("photo.label", &json!({"label": "none"})).unwrap();
        assert_eq!(labels(&a), labels(&b));
        assert_eq!(a.undo.len(), b.undo.len());
    }
    // ids name the photos and an unlabelled or unknown photo is handled
    {
        let mut s = session().unwrap();
        s.execute("library.select", &json!({"ids": [4]})).unwrap();
        s.execute("photo.clearLabel", &json!({"ids": [1]})).unwrap();
        assert_eq!(labels(&s), [None, Some(ColorLabel::Purple), Some(ColorLabel::Blue), None]);
        // a photo without a label is left as it is, and a photo that does not exist is an error
        s.execute("photo.clearLabel", &json!({"ids": [4]})).unwrap();
        assert!(s.execute("photo.clearLabel", &json!({"ids": [999]})).is_err());
    }
}
