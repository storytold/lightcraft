//! A filter Enable Filters (Cmd+L) put aside and a folder moved on disk meanwhile: switched back
//! on, the filter is the one put aside, and a folder it names is where the folder went.

use lightcraft_catalog::{Op, Photo, Source};
use serde_json::json;

use crate::Session;

fn add(s: &mut Session, path: &str, rating: u8) {
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    p.rating = rating;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
}

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-shelved-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("trip")).unwrap();
        std::fs::create_dir_all(dir.join("archive")).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).components().collect::<std::path::PathBuf>().to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_folder_in_the_filter_put_aside_follows_a_move() {
    let dir = Scratch::new("filter");
    let (trip, archive) = (dir.path("trip"), dir.path("archive"));
    let mut s = Session::new();
    add(&mut s, &format!("{trip}/a.jpg"), 0);
    add(&mut s, &dir.path("elsewhere.jpg"), 0);
    s.execute("library.filter", &json!({"folder": trip})).unwrap();
    assert_eq!(s.visible().len(), 1, "the filter hides a photo, or a stale folder would look alike");
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("folder.move", &json!({"path": trip, "into": archive})).unwrap();
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(r["enabled"], json!(true));
    assert_eq!(s.filter.folder.as_deref(), Some(dir.path("archive/trip").as_str()), "the folder where it went");
    assert_eq!(s.visible().len(), 1, "the moved photo, as before the move");
}

#[test]
fn a_folder_in_the_filter_follows_a_move_with_filters_on() {
    let dir = Scratch::new("live");
    let (trip, archive) = (dir.path("trip"), dir.path("archive"));
    let mut s = Session::new();
    add(&mut s, &format!("{trip}/a.jpg"), 0);
    add(&mut s, &dir.path("elsewhere.jpg"), 0);
    s.execute("library.filter", &json!({"folder": trip})).unwrap();
    assert_eq!(s.visible().len(), 1, "the filter hides a photo, or a stale folder would look alike");
    s.execute("folder.move", &json!({"path": trip, "into": archive})).unwrap();
    assert_eq!(s.filter.folder.as_deref(), Some(dir.path("archive/trip").as_str()), "the folder where it went");
    assert_eq!(s.visible().len(), 1, "the moved photo, as before the move");
}

#[test]
fn moving_the_browsed_folder_while_off_brings_back_the_filter_put_aside() {
    let dir = Scratch::new("browse");
    let (trip, archive) = (dir.path("trip"), dir.path("archive"));
    let mut s = Session::new();
    s.execute("library.browse", &json!({"path": trip})).unwrap();
    s.execute("library.filter", &json!({"rating": 4})).unwrap();
    let set = s.filter.clone();
    s.execute("library.toggleFilters", &json!({})).unwrap();
    s.execute("folder.move", &json!({"path": trip, "into": archive})).unwrap();
    assert!(s.filters_off(), "still off after the move");
    let r = s.execute("library.toggleFilters", &json!({})).unwrap();
    assert_eq!(r["enabled"], json!(true), "Cmd+L turns filtering back on");
    assert_eq!(s.filter, set, "the stars put aside, nothing added");
}
