//! Issue #781: a dialog taller than the window keeps its title and buttons on screen and scrolls
//! its body (`dialog_frame::ModalLayout`); a short dialog is no taller than its content.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"))
}

fn settle(h: &mut Headless) {
    for _ in 0..6 {
        h.step();
    }
}

fn open(size: [f32; 2], dlg: Dialog) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    h.app.ui.dialog = Some(dlg);
    // (the first request switches the widget list on; the layout settles on measured heights)
    h.request("ui.widgets", json!({}), T);
    settle(&mut h);
    h
}

fn assert_fits(h: &Headless, size: [f32; 2], what: &str) {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]));
    let window = rect(h, "dialog:window");
    assert!(window.top() >= screen.top() + 8.0 && window.bottom() <= screen.bottom() - 8.0, "{what}: window {window:?} in {screen:?}");
    let ok = rect(h, "button:dialogOk");
    assert!(window.contains_rect(ok) && ok.height() > 10.0, "{what}: OK {ok:?} outside {window:?}");
}

#[test]
fn tall_dialogs_fit_a_small_window_with_their_buttons_on_screen() {
    for size in [[1366.0, 768.0], [1000.0, 520.0]] {
        let mut h = open(size, Dialog::Settings { tab: "general".into() });
        assert_fits(&h, size, "Settings");
        let r = h.request("command", json!({"command": "dialog.export"}), T);
        assert_eq!(r["ok"], true, "{r}");
        settle(&mut h);
        assert!(matches!(h.app.ui.dialog, Some(Dialog::Export { .. })), "{:?}", h.app.ui.dialog);
        assert_fits(&h, size, "Export");
        // the footer buttons can be pressed
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
        assert_eq!(r["ok"], true, "{r}");
        settle(&mut h);
        assert!(h.app.ui.dialog.is_none(), "Cancel closed Export");

        h.app.ui.dialog = Some(Dialog::Update);
        settle(&mut h);
        assert_fits(&h, size, "Update");
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        settle(&mut h);
        assert!(h.app.ui.dialog.is_none(), "Close closed Update");
    }
}
