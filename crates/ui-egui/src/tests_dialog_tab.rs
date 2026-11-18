//! A dialog's name field takes the keyboard focus once, when the dialog opens: Tab then carries it
//! on to the dialog's other controls (a check box, Cancel, OK) and Shift+Tab back, instead of the
//! field taking it back every frame.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo_with_selection() -> Headless {
    let mut session = lcc_engine::Session::with_demo();
    session.selection.ids = session.catalog.photos().take(2).map(|p| p.id).collect();
    let mut h = Headless::new(LccApp::new(session, Services::default()), [1400.0, 900.0], 1.0);
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

/// Open `dialog` (closing any other first) and let it take the focus.
fn open(h: &mut Headless, dialog: Dialog) {
    h.app.ui.dialog = None;
    h.step();
    h.step();
    h.app.ui.dialog = Some(dialog);
    for _ in 0..4 {
        h.step();
    }
}

/// Open `dialog`, then Tab through it; the focus order, starting with the field it opens on.
fn tab_order(h: &mut Headless, dialog: Dialog) -> Vec<String> {
    open(h, dialog);
    let mut order = vec![focused(h).unwrap_or_default()];
    for _ in 0..4 {
        key(h, json!({"key": "Tab"}));
        order.push(focused(h).unwrap_or_default());
    }
    order
}

fn new_album(name: &str, include_selected: bool) -> Dialog {
    Dialog::NewAlbum { name: name.into(), folder: false, parent: None, include_selected }
}

/// One session, a dialog opened for each step: the name field has the focus when a dialog opens, Tab carries it on
/// to the check box, Cancel and OK and Shift+Tab back; Esc, Return and typing still work around that.
#[test]
fn dialogs_open_on_their_name_and_tab_escape_and_return_work_from_there() {
    let mut h = demo_with_selection();
    let order = tab_order(&mut h, new_album("Trip", true));
    assert_eq!(order[..4], ["field:albumName", "check:albumIncludeSelected", "button:dialogCancel", "button:dialogOk"], "New Album: {order:?}");

    let order = tab_order(&mut h, Dialog::NewSmartAlbum { name: "Rejected".into(), parent: None });
    let (cancel, ok) = (order.iter().position(|w| w == "button:dialogCancel"), order.iter().position(|w| w == "button:dialogOk"));
    assert_eq!(order[0], "field:smartAlbumName", "New Smart Album: {order:?}");
    assert!(cancel.is_some() && ok.is_some() && cancel < ok, "Cancel then OK are reached by Tab: {order:?}");

    let prompt = Dialog::TextPrompt {
        title: "Save Metadata Preset".into(),
        hint: "Preset name".into(),
        value: String::new(),
        command: "metadata.savePreset".into(),
        params: json!({}),
        key: "name".into(),
    };
    let order = tab_order(&mut h, prompt);
    assert_eq!(order[..3], ["field:prompt", "button:dialogCancel", "button:dialogOk"], "Save Metadata Preset: {order:?}");

    // Shift+Tab goes back from OK to the name
    open(&mut h, new_album("Trip", true));
    for _ in 0..3 {
        key(&mut h, json!({"key": "Tab"}));
    }
    assert_eq!(focused(&h).as_deref(), Some("button:dialogOk"));
    for _ in 0..3 {
        key(&mut h, json!({"key": "Tab", "shift": true}));
    }
    assert_eq!(focused(&h).as_deref(), Some("field:albumName"), "Shift+Tab back to the name");

    // Esc still cancels after tabbing away from the name
    open(&mut h, new_album("Trip", true));
    key(&mut h, json!({"key": "Tab"}));
    key(&mut h, json!({"key": "Escape"}));
    assert!(h.app.ui.dialog.is_none(), "Esc closed the dialog");

    // an empty name is refused on Return, and the field has the focus again for the correction
    open(&mut h, new_album("", false));
    key(&mut h, json!({"key": "Enter"}));
    h.step();
    assert!(h.app.ui.dialog.is_some(), "an empty name is refused and the dialog stays");
    assert_eq!(focused(&h).as_deref(), Some("field:albumName"), "the field has the focus again");

    // the name still takes typing and Return after opening
    open(&mut h, new_album("", false));
    let r = h.request("ui.text", json!({"text": "Holiday"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    key(&mut h, json!({"key": "Enter"}));
    h.settle(T);
    assert!(h.app.ui.dialog.is_none(), "Enter confirmed the dialog");
    assert!(h.app.session.catalog.albums().any(|a| a.name == "Holiday"), "the typed name made the album");

    // a click on a check box leaves Return confirming the name
    open(&mut h, new_album("Trip", true));
    let r = h.request("ui.clickWidget", json!({"id": "check:albumIncludeSelected"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    key(&mut h, json!({"key": "Enter"}));
    h.settle(T);
    assert!(h.app.ui.dialog.is_none(), "Enter confirmed the dialog after the click");
    assert!(h.app.session.catalog.albums().any(|a| a.name == "Trip"), "the album was made after the click");
}
