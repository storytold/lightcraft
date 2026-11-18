//! Rows on the Settings screen: a label longer than its column leaves the controls in line, and a
//! short path is shown whole with no tooltip. (Upstream's tests of its Settings window, one size
//! from tab to tab with tabs that stay put, don't apply to the fork's full-window Settings screen.)

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn headless(session: lcc_engine::Session, size: [f32; 2]) -> Headless {
    let mut h = Headless::new(LccApp::new(session, Services::default()), size, 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn demo(size: [f32; 2]) -> Headless {
    headless(lcc_engine::Session::with_demo(), size)
}

/// Given Settings open on `tab`, drawn until its layout has settled.
fn show(h: &mut Headless, tab: &str) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    for _ in 0..4 {
        h.step();
    }
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn settings_rows_keep_their_controls_in_line_and_a_short_path_has_no_tooltip() {
    // Given Performance, whose "Forget unchanged photos" label is wider than the label column
    let mut h = demo([1600.0, 1000.0]);
    // a label longer than its column leaves the controls in line
    {
        show(&mut h, "performance");
        // Then its choices start where the choices of a short label do
        let (short, long) = (rect(&h, "button:settingsPreview-0"), rect(&h, "button:settingsForgetLocal-0"));
        assert!((short.left() - long.left()).abs() < 0.5, "Preview size starts at {} and Forget unchanged photos at {}", short.left(), long.left());
    }
    // a short path is shown whole and has no tooltip
    {
        // Given the in-memory library, whose Location is a short sentence
        show(&mut h, "general");
        assert!(h.painted_text().iter().any(|text| text == "In memory (nothing is saved)"), "it is all there");
        let before = h.painted_text().len();
        // When the pointer rests on it
        assert_eq!(h.request("ui.hoverWidget", json!({"id": "text:settingsLocation"}), T)["ok"], true);
        let _ = h.step_until(Duration::from_secs(2), |h| h.painted_text().len() > before);
        // Then nothing more is drawn: there is nothing a tooltip could add
        assert_eq!(h.painted_text().len(), before, "no tooltip for a path that is all there");
    }
}
