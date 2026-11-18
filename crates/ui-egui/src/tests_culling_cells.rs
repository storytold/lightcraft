//! Culling marks in the Compact layout: every grid and filmstrip cell has a strip under its
//! picture with the photo's flag, stars and colour label, readable at a glance while culling and
//! covering nothing. Classic keeps its marks over the picture's bottom edge.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);

fn demo() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

fn run(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn first(h: &mut Headless) -> u64 {
    h.app.session.visible_cloned().first().map(|p| p.0).expect("a demo photo")
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn compact_grid_cells_and_the_compact_filmstrip_carry_their_marks_in_a_strip() {
    let mut h = demo();
    // compact grid cells carry their marks in a strip under the picture
    {
        let id = first(&mut h);
        run(&mut h, "library.select", json!({"ids": [id]}));
        run(&mut h, "photo.pick", json!({}));
        run(&mut h, "photo.rate", json!({"rating": 3, "ids": [id]}));
        for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
            h.app.ui.view = view;
            h.app.ui.settings.compact_layout = false;
            h.settle(T);
            assert!(widget(&h, &format!("cellmarks:{id}")).is_none(), "{view:?}: Classic draws its marks over the picture");
            let classic = widget(&h, &format!("thumb:{id}")).expect("the cell");

            run(&mut h, "view.compactLayout", json!({"on": true}));
            h.settle(T);
            let cell = widget(&h, &format!("thumb:{id}")).expect("the compact cell");
            let marks = widget(&h, &format!("cellmarks:{id}")).expect("the strip of marks");
            assert!(cell.contains_rect(marks), "{view:?}: the marks {marks:?} sit in the cell {cell:?}");
            assert!(marks.bottom() >= cell.bottom() - 2.0, "{view:?}: at its bottom");
            assert!(marks.height() >= 12.0, "{view:?}: tall enough to read: {marks:?}");
            if view == ViewMode::PhotoGrid {
                // the picture keeps its size; the cell grows by the strip
                assert!((cell.width() - classic.width()).abs() < 1.0 || cell.height() > classic.height(), "{classic:?} → {cell:?}");
            }
            run(&mut h, "view.compactLayout", json!({"on": false}));
        }
    }
    // the compact filmstrip shows the marks under each thumbnail
    {
        let id = first(&mut h);
        run(&mut h, "library.select", json!({"ids": [id]}));
        run(&mut h, "photo.reject", json!({}));
        h.app.ui.view = ViewMode::Detail;
        h.app.ui.filmstrip = true;
        h.settle(T);
        assert!(widget(&h, &format!("filmmarks:{id}")).is_none(), "Classic: badges over the thumbnail");
        run(&mut h, "view.compactLayout", json!({"on": true}));
        h.settle(T);
        let cell = widget(&h, &format!("film:{id}")).expect("the filmstrip cell");
        let marks = widget(&h, &format!("filmmarks:{id}")).expect("the marks under the thumbnail");
        assert!(cell.contains_rect(marks), "{marks:?} in {cell:?}");
        assert!(marks.center().y > cell.center().y, "under the picture");
    }
}
