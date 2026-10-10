//! Relative ratings dispatch through the real engine, including history and AutoWrite.
use dac_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Selection, Session};

fn session() -> Result<Session> {
    let mut s = Session::new();
    for (id, rating) in [(1, 0), (2, 2), (3, 5), (4, 1)] {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-relative-ratings/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-09",
        );
        photo.rating = rating;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [1, 2, 3], "active": 2}))?;
    Ok(s)
}

fn ratings(s: &Session) -> Result<Vec<u8>> {
    (1..=4).map(|id| s.catalog.photo(PhotoId(id)).map(|p| p.rating).ok_or_else(|| dac_catalog::CatalogError::NoPhoto(PhotoId(id)).into())).collect()
}

#[test]
fn relative_ratings_clamp_each_photo_and_form_one_history_step() {
    let mut s = session().unwrap();
    let undo = s.undo.len();
    assert_eq!(s.execute("photo.increaseRating", &json!({})).unwrap()["changed"], 2);
    assert_eq!(ratings(&s).unwrap(), [1, 3, 5, 1]);
    assert_eq!(s.undo.len(), undo + 1);
    assert_eq!(s.active(), Some(PhotoId(2)));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(ratings(&s).unwrap(), [0, 2, 5, 1]);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(ratings(&s).unwrap(), [1, 3, 5, 1]);
    assert_eq!(s.execute("photo.decreaseRating", &json!({})).unwrap()["changed"], 3);
    assert_eq!(ratings(&s).unwrap(), [0, 2, 4, 1]);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(ratings(&s).unwrap(), [1, 3, 5, 1]);
}

#[test]
fn bounds_active_fallback_explicit_targets_and_errors_preserve_history() {
    let mut s = session().unwrap();
    assert_eq!(s.execute("photo.decreaseRating", &json!({"ids": [1]})).unwrap()["changed"], 0);
    assert_eq!(s.execute("photo.increaseRating", &json!({"ids": [3]})).unwrap()["changed"], 0);
    assert!(s.undo.is_empty());
    s.selection = Selection { ids: vec![], active: Some(PhotoId(2)) };
    assert_eq!(s.execute("photo.increaseRating", &json!({})).unwrap()["changed"], 1);
    assert_eq!(ratings(&s).unwrap(), [0, 3, 5, 1]);
    assert_eq!(s.execute("photo.decreaseRating", &json!({"ids": [4]})).unwrap()["changed"], 1);
    assert_eq!(ratings(&s).unwrap(), [0, 3, 5, 0]);
    let undo = s.undo.len();
    assert!(s.execute("photo.increaseRating", &json!({"ids": [1, 999]})).is_err());
    assert_eq!(ratings(&s).unwrap(), [0, 3, 5, 0]);
    assert_eq!(s.undo.len(), undo);
    s.execute("photo.increaseRating", &json!({"advance": true})).unwrap();
    assert_eq!(s.active(), Some(PhotoId(3)));
    s.selection = Selection::default();
    assert!(s.execute("photo.decreaseRating", &json!({})).is_err());
}

#[test]
fn relative_ratings_and_history_use_auto_write() {
    let mut s = session().unwrap();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("lc-relative-ratings-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Metadata-only fixtures: no import, image decoding or model/service dependency.
    for id in 1..=4 {
        s.catalog
            .apply(Op::SetFile {
                id: PhotoId(id),
                file_name: format!("{id}.jpg"),
                source: Source::File { path: dir.join(format!("{id}.jpg")).to_string_lossy().into_owned() },
            })
            .unwrap();
    }
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.increaseRating", &json!({})).unwrap();
    assert!(std::fs::read_to_string(dir.join("1.xmp")).unwrap().contains("<xmp:Rating>1</xmp:Rating>"));
    assert!(std::fs::read_to_string(dir.join("2.xmp")).unwrap().contains("<xmp:Rating>3</xmp:Rating>"));
    assert!(!dir.join("3.xmp").exists(), "unchanged upper bound must not write");
    assert!(!dir.join("4.xmp").exists(), "unselected photo must not write");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(std::fs::read_to_string(dir.join("1.xmp")).unwrap().contains("<xmp:Rating>0</xmp:Rating>"));
    assert!(std::fs::read_to_string(dir.join("2.xmp")).unwrap().contains("<xmp:Rating>2</xmp:Rating>"));
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(std::fs::read_to_string(dir.join("2.xmp")).unwrap().contains("<xmp:Rating>3</xmp:Rating>"));
    s.execute("photo.decreaseRating", &json!({})).unwrap();
    assert!(std::fs::read_to_string(dir.join("3.xmp")).unwrap().contains("<xmp:Rating>4</xmp:Rating>"));
    std::fs::remove_dir_all(dir).unwrap();
}
