//! A dialog's name field takes the keyboard focus once, when the dialog opens: Tab then carries it
//! on to the dialog's other controls (a check box, Cancel, OK) and Shift+Tab back, instead of the
//! field taking it back every frame.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo_with_selection() -> Headless {
    let mut session = lightcraft_engine::Session::with_demo();
    session.selection.ids = session.catalog.photos().take(2).map(|p| p.id).collect();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

/// The registered widget (`field:…`, `check:…`, `button:…`) that has the keyboard focus.
fn focused(h: &Headless) -> Option<String> {
    let id = h.view.ctx.memory(|m| m.focused())?;
    let rect = h.view.ctx.read_response(id)?.rect;
    h.app.widgets.iter().find(|(_, r)| *r == rect).map(|(w, _)| w.clone())
}

fn key(h: &mut Headless, params: serde_json::Value) {
    let r = h.request("ui.key", params, T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

/// Open `dialog`, then Tab through it; the focus order, starting with the field it opens on.
fn tab_order(dialog: Dialog) -> Vec<String> {
    let mut h = demo_with_selection();
    h.app.ui.dialog = Some(dialog);
    for _ in 0..4 {
        h.step();
    }
    let mut order = vec![focused(&h).unwrap_or_default()];
    for _ in 0..4 {
        key(&mut h, json!({"key": "Tab"}));
        order.push(focused(&h).unwrap_or_default());
    }
    order
}

#[test]
fn new_album_opens_on_its_name_and_tab_reaches_the_check_box_cancel_and_ok() {
    let order = tab_order(Dialog::NewAlbum { name: "Trip".into(), folder: false, parent: None, include_selected: true });
    assert_eq!(order[..4], ["field:albumName", "check:albumIncludeSelected", "button:dialogCancel", "button:dialogOk"], "{order:?}");
}

#[test]
fn new_smart_album_opens_on_its_name_and_tab_reaches_cancel_and_ok() {
    let order = tab_order(Dialog::NewSmartAlbum { name: "Rejected".into(), parent: None });
    let (cancel, ok) = (order.iter().position(|w| w == "button:dialogCancel"), order.iter().position(|w| w == "button:dialogOk"));
    assert_eq!(order[0], "field:smartAlbumName", "{order:?}");
    assert!(cancel.is_some() && ok.is_some() && cancel < ok, "Cancel then OK are reached by Tab: {order:?}");
}

#[test]
fn save_metadata_preset_opens_on_its_name_and_tab_reaches_cancel_and_ok() {
    let prompt = Dialog::TextPrompt {
        title: "Save Metadata Preset".into(),
        hint: "Preset name".into(),
        value: String::new(),
        command: "metadata.savePreset".into(),
        params: json!({}),
        key: "name".into(),
    };
    let order = tab_order(prompt);
    assert_eq!(order[..3], ["field:prompt", "button:dialogCancel", "button:dialogOk"], "{order:?}");
}

#[test]
fn shift_tab_goes_back_from_ok_to_the_name() {
    let mut h = demo_with_selection();
    h.app.ui.dialog = Some(Dialog::NewAlbum { name: "Trip".into(), folder: false, parent: None, include_selected: true });
    for _ in 0..4 {
        h.step();
    }
    for _ in 0..3 {
        key(&mut h, json!({"key": "Tab"}));
    }
    assert_eq!(focused(&h).as_deref(), Some("button:dialogOk"));
    for _ in 0..3 {
        key(&mut h, json!({"key": "Tab", "shift": true}));
    }
    assert_eq!(focused(&h).as_deref(), Some("field:albumName"));
}

#[test]
fn the_name_still_takes_typing_and_enter_after_opening() {
    let mut h = demo_with_selection();
    h.app.ui.dialog = Some(Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: false });
    for _ in 0..4 {
        h.step();
    }
    let r = h.request("ui.text", json!({"text": "Holiday"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    key(&mut h, json!({"key": "Enter"}));
    h.settle(T);
    assert!(h.app.ui.dialog.is_none(), "Enter confirmed the dialog");
    assert!(h.app.session.catalog.albums().any(|a| a.name == "Holiday"), "the typed name made the album");
}

#[test]
fn escape_still_cancels_after_tabbing_away_from_the_name() {
    let mut h = demo_with_selection();
    h.app.ui.dialog = Some(Dialog::NewAlbum { name: "Trip".into(), folder: false, parent: None, include_selected: true });
    for _ in 0..4 {
        h.step();
    }
    key(&mut h, json!({"key": "Tab"}));
    key(&mut h, json!({"key": "Escape"}));
    assert!(h.app.ui.dialog.is_none(), "Esc closed the dialog");
}

#[test]
fn a_click_on_a_check_box_leaves_return_confirming_the_name() {
    let mut h = demo_with_selection();
    h.app.ui.dialog = Some(Dialog::NewAlbum { name: "Trip".into(), folder: false, parent: None, include_selected: true });
    for _ in 0..4 {
        h.step();
    }
    let r = h.request("ui.clickWidget", json!({"id": "check:albumIncludeSelected"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    key(&mut h, json!({"key": "Enter"}));
    h.settle(T);
    assert!(h.app.ui.dialog.is_none(), "Enter confirmed the dialog after the click");
    assert!(h.app.session.catalog.albums().any(|a| a.name == "Trip"));
}

#[test]
fn a_refused_return_leaves_the_name_focused_for_the_correction() {
    let mut h = demo_with_selection();
    h.app.ui.dialog = Some(Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: false });
    for _ in 0..4 {
        h.step();
    }
    key(&mut h, json!({"key": "Enter"}));
    h.step();
    assert!(h.app.ui.dialog.is_some(), "an empty name is refused and the dialog stays");
    assert_eq!(focused(&h).as_deref(), Some("field:albumName"), "the field has the focus again");
}
