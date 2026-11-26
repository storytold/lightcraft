//! Real input coverage shared by the native-main and SDK-candidate comparisons.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::{Dialog, LightroomImport, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn fixture() -> (Headless, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let picked = calls.clone();
    let services = Services {
        // First cancel, then choose. No real picker or filesystem writes.
        pick_folder: Some(Box::new(move || (picked.fetch_add(1, Ordering::SeqCst) > 0).then(|| "/chosen/destination".into()))),
        png: None,
        ..Default::default()
    };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), services);
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.dialog = Some(Dialog::LightroomDestination {
        catalog: "/fixture/Travel.lrcat".into(),
        library: "Open library".into(),
        photos: 7,
        new_library: true,
        name: "Travel".into(),
        location: "/initial".into(),
        dont_ask: false,
        generation: app.session.library_generation(),
    });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    assert!(h.settle(T));
    (h, calls)
}

fn draft(h: &Headless) -> (bool, String, String, bool) {
    let Some(Dialog::LightroomDestination { new_library, name, location, dont_ask, .. }) = &h.app.ui.dialog else { panic!("destination closed") };
    (*new_library, name.clone(), location.clone(), *dont_ask)
}

fn ask(h: &mut Headless, method: &str, params: Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    h.step();
    h.step();
}

fn click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id}));
}

fn edit(h: &mut Headless, id: &str, value: &str) {
    click(h, id);
    ask(h, "ui.key", json!({"key": "A", "cmd": true, "ctrl": true}));
    ask(h, "ui.text", json!({"text": value}));
    ask(h, "ui.key", json!({"key": "Enter"}));
}

#[test]
fn destination_controls_edit_choose_and_cancel_are_draft_only() {
    let (mut h, calls) = fixture();
    let revision = h.app.session.catalog.revision;
    let undo = h.app.session.undo.len();
    edit(&mut h, "field:lightroomLibraryName", "Summer proofs");
    edit(&mut h, "field:lightroomLibraryLocation", "/typed/destination");
    assert_eq!(draft(&h), (true, "Summer proofs".into(), "/typed/destination".into(), false));
    assert!(h.app.lightroom.is_none(), "Enter in an edit field is not confirmation");
    click(&mut h, "button:lightroomLibraryChoose");
    assert_eq!(draft(&h).2, "/typed/destination", "picker cancel keeps typed text");
    click(&mut h, "button:lightroomLibraryChoose");
    assert_eq!(draft(&h).2, "/chosen/destination");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none());
    assert!(h.app.session.library.is_none(), "draft edits never create a library");
    assert_eq!(h.app.session.catalog.revision, revision);
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(h.app.ui.settings.lightroom_import, LightroomImport::Ask);
    h.settle(T);
}

#[test]
fn destination_controls_disabled_options_preserve_drafts_and_preference() {
    let (mut h, calls) = fixture();
    // The merge-only checkbox cannot toggle while a new library is selected.
    click(&mut h, "check:lightroomDontAsk");
    assert!(!draft(&h).3);
    click(&mut h, "radio:lightroomDestination-open");
    assert!(!draft(&h).0);
    click(&mut h, "check:lightroomDontAsk");
    assert!(draft(&h).3);
    let before = draft(&h);
    click(&mut h, "button:lightroomLibraryChoose");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "disabled Choose must not open the picker");
    click(&mut h, "field:lightroomLibraryName");
    ask(&mut h, "ui.text", json!({"text": "must not edit"}));
    assert_eq!(draft(&h), before);
    click(&mut h, "radio:lightroomDestination-new");
    assert!(draft(&h).0);
    click(&mut h, "check:lightroomDontAsk");
    assert!(draft(&h).3, "new-library mode displays unchecked without erasing the remembered merge draft");
    click(&mut h, "radio:lightroomDestination-open");
    assert!(draft(&h).3);
    assert_eq!(h.app.ui.settings.lightroom_import, LightroomImport::Ask, "draft is not a saved preference");
    click(&mut h, "button:dialogCancel");
    assert_eq!(h.app.ui.settings.lightroom_import, LightroomImport::Ask);
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none());
    h.settle(T);
}

/// SDK radios deliberately use one roving Tab stop, arrows for focus, Space for selection.
#[test]
fn sdk_destination_keyboard_roves_and_selects_without_importing() {
    let (mut h, calls) = fixture();
    click(&mut h, "radio:lightroomDestination-open");
    assert!(!draft(&h).0);
    let open = crate::control::widget_hit(&h.app, "radio:lightroomDestination-open").expect("real open-library radio hit");
    let mut reached = false;
    for _ in 0..24 {
        ask(&mut h, "ui.key", json!({"key": "Tab"}));
        if craft::ui::runtime::focus::focused_rec(&h.view.ctx).is_some_and(|record| record.stop.id == open.id) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab must reach the selected destination radio");
    ask(&mut h, "ui.key", json!({"key": "ArrowRight"}));
    ask(&mut h, "ui.key", json!({"key": "Space"}));
    assert_eq!(draft(&h), (true, "Travel".into(), "/initial".into(), false));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none());
    assert!(h.app.session.library.is_none());
    assert_eq!(h.app.ui.settings.lightroom_import, LightroomImport::Ask);
    click(&mut h, "button:dialogCancel");
    h.settle(T);
}
