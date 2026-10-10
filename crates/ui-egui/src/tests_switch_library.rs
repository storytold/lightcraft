//! Switching libraries must not leave the old library's sidebar numbers, keyword tree or date
//! groups on screen. A library loaded from disk starts at catalog revision 0, and so does a brand
//! new empty one, so caches keyed on the revision alone would see "nothing changed".

use serde_json::json;

use crate::{DacApp, Services};

#[test]
fn opening_another_library_drops_the_old_librarys_cached_panels() {
    let dir = std::env::temp_dir().join(format!("lc-ui-switchlib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();

    // library A with photos, reopened from disk (as at launch)
    let mut session = dac_engine::Session::new().with_fs();
    session.open_library(&a, true).unwrap();
    session.close_library().unwrap();
    session.open_library(&a, false).unwrap();
    assert!(!session.catalog.is_empty());

    let mut app = DacApp::new(session, Services::default());
    let (cat, caches) = (&app.session.catalog, &mut app.caches);
    assert!(caches.counts(cat).total > 0);
    assert!(!caches.keyword_tree(cat).is_empty());
    assert!(!caches.date_groups(cat).is_empty());

    // the grid's own list is cached too
    assert!(!app.session.visible().is_empty());

    app.run("app.openLibrary", json!({"path": b.to_string_lossy()})).unwrap();
    assert!(app.session.catalog.is_empty(), "library B is empty");
    let (cat, caches) = (&app.session.catalog, &mut app.caches);
    assert_eq!(caches.counts(cat).total, 0, "All Photos count");
    assert_eq!(caches.counts(cat).picks, 0);
    assert!(caches.keyword_tree(cat).is_empty(), "keywords");
    assert!(caches.date_groups(cat).is_empty(), "By Date");
    assert!(app.session.visible().is_empty(), "grid");

    let _ = std::fs::remove_dir_all(&dir);
}
