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
    for size in [[1000.0, 520.0]] {
        let mut h = open(size, Dialog::Settings { tab: "general".into() });
        // Settings fills the window: its way out (‹) is on screen and closes it
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]));
        assert!(screen.contains_rect(rect(&h, "settings:back")) && screen.contains_rect(rect(&h, "settings:rows")), "Settings in {screen:?}");
        let r = h.request("ui.clickWidget", json!({"id": "settings:back"}), T);
        assert_eq!(r["ok"], true, "{r}");
        settle(&mut h);
        assert!(h.app.ui.dialog.is_none(), "‹ closed Settings");
        let r = h.request("command", json!({"command": "dialog.export"}), T);
        assert_eq!(r["ok"], true, "{r}");
        settle(&mut h);
        assert!(matches!(h.app.ui.dialog, Some(Dialog::Export { .. })), "{:?}", h.app.ui.dialog);
        // Export fills the window too: its way back and its buttons are on screen
        for id in ["export:back", "button:dialogOk", "button:dialogCancel"] {
            assert!(screen.contains_rect(rect(&h, id)), "Export: {id} in {screen:?}");
        }
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

/// On a phone a small dialog is a bottom sheet: the window's width, on its bottom edge, its buttons
/// finger-sized and reachable; its grab handle cancels. On a desktop window it stays a centred card.
#[test]
fn small_dialogs_are_bottom_sheets_on_a_phone() {
    let new_album = || Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: false };
    let size = [412.0, 900.0];
    let mut h = open(size, new_album());
    let w = rect(&h, "dialog:window");
    assert!(w.width() >= 412.0 - 1.0 && w.left() <= 0.5, "full width: {w:?}");
    assert!((w.bottom() - 900.0).abs() <= 1.0, "on the bottom edge: {w:?}");
    let ok = rect(&h, "button:dialogOk");
    assert!(w.contains_rect(ok) && ok.height() >= 44.0, "OK finger-sized and inside: {ok:?}");
    let r = h.request("ui.clickWidget", json!({"id": "dialog:grab"}), T);
    assert_eq!(r["ok"], true, "{r}");
    settle(&mut h);
    assert!(h.app.ui.dialog.is_none(), "the grab handle closed it");
    // a desktop window: a centred card as before
    let size = [1280.0, 800.0];
    let h = open(size, new_album());
    let w = rect(&h, "dialog:window");
    assert!((w.center().x - 640.0).abs() < 2.0 && w.bottom() < 790.0 && w.width() < 600.0, "centred card: {w:?}");
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "dialog:grab"));
}
