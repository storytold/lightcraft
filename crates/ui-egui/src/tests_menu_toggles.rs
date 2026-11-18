//! The Map, Book, Slideshow and Print menu items that used to be "Coming soon": toggles show their
//! state, locking the map markers drops a pin drag, Book's page navigation stops at the ends.

use serde_json::json;

use crate::menubar::checked;
use crate::{LccApp, Services};

fn app() -> LccApp {
    let mut session = lcc_engine::Session::with_demo();
    session.selection = lcc_engine::Selection::single(session.visible_cloned()[0]);
    LccApp::new(session, Services { png: None, ..Default::default() })
}

/// Toggles show their state in the menu; locking the markers drops a pin drag under way; Book's
/// page navigation stops at both ends.
#[test]
fn toggles_show_their_state_and_book_navigation_stops_at_the_ends() {
    let mut a = app();
    assert_eq!(checked(&a, "map.toggleLockMarkers"), Some(false));
    assert_eq!(checked(&a, "map.toggleSavedLocations"), Some(true));
    a.run("map.toggleLockMarkers", json!({})).unwrap();
    a.run("map.toggleSavedLocations", json!({})).unwrap();
    assert_eq!(checked(&a, "map.toggleLockMarkers"), Some(true));
    assert_eq!(checked(&a, "map.toggleSavedLocations"), Some(false));
    // locking drops a pin drag already under way
    a.map.dragging_pin = Some(vec![1]);
    a.run("map.toggleLockMarkers", json!({})).unwrap();
    assert!(a.map.dragging_pin.is_none());

    assert_eq!(checked(&a, "printui.toggleGuides"), Some(true));
    a.run("printui.toggleGuides", json!({})).unwrap();
    assert_eq!(checked(&a, "printui.toggleGuides"), Some(false));

    a.run("book.autoLayout", json!({"preset": "Two Per Page"})).unwrap();
    let before = checked(&a, "book.toggleGuides").unwrap();
    a.run("book.toggleGuides", json!({})).unwrap();
    assert_eq!(checked(&a, "book.toggleGuides"), Some(!before));
    let n = a.ui.book.book.all_pages().len();
    assert!(n >= 2, "{n} pages");
    a.run("book.go", json!({"delta": 1000})).unwrap();
    let last = a.ui.book.current;
    a.run("book.go", json!({"delta": 1})).unwrap();
    assert_eq!(a.ui.book.current, last, "Next Page at the end stays put");
    a.run("book.go", json!({"delta": -1000})).unwrap();
    let first = a.ui.book.current;
    a.run("book.go", json!({"delta": -1})).unwrap();
    assert_eq!(a.ui.book.current, first, "Previous Page at the start stays put");
    a.run("book.go", json!({"delta": 1})).unwrap();
    assert_ne!(a.ui.book.current, first);
    assert!(a.run("book.go", json!({})).is_err(), "book.go needs its delta");
}
