//! A dialog's confirm button reads as its primary action: on the right, Cancel to its left, and
//! filled with the accent when it makes or brings in something (Import, Merge, Save…). A
//! destructive confirm (Delete, Remove) stays plain: never the eye-catching default.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::dialogs::{ConfirmStyle, confirm_style};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} not on screen"))
}

fn png(dir: &std::path::Path, name: &str) -> String {
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 160, 255]; 64] };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn the_import_review_puts_its_confirm_on_the_right_as_the_primary_action() {
    // Given the import review of two photos
    let dir = std::env::temp_dir().join(format!("lc-ui-dialog-primary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let paths = [png(&dir, "a.png"), png(&dir, "b.png")];
    let app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.step();
    h.app.run("file.addPhotos", json!({"paths": paths})).unwrap();
    h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| w == "button:dialogOk"));
    h.step();
    // Then "Import 2 Photos" is right of Cancel, at the dialog's right edge
    let (ok, cancel, window) = (rect(&h, "button:dialogOk"), rect(&h, "button:dialogCancel"), rect(&h, "dialog:window"));
    assert!(ok.left() > cancel.right(), "confirm {ok:?} right of cancel {cancel:?}");
    assert!(window.right() - ok.right() < 40.0, "confirm {ok:?} at the right edge of {window:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn constructive_confirms_are_primary_and_destructive_ones_plain() {
    use crate::state::Dialog;
    assert_eq!(confirm_style(&Dialog::AutoStack { gap: 60.0 }), ConfirmStyle::Primary);
    assert_eq!(confirm_style(&Dialog::ConfirmDelete { count: 3 }), ConfirmStyle::Plain);
    // a dialog that only shows something has nothing to confirm: its Close is plain
    assert_eq!(confirm_style(&Dialog::About), ConfirmStyle::Plain);
}

#[test]
fn a_dialog_wider_than_its_content_still_ends_its_footer_at_its_edge() {
    // Auto-Stack's title makes it wider than its slider and note: the buttons go to the edge, and
    // the window doesn't grow for them
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.step();
    h.app.run("dialog.autoStack", json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    let (ok, window) = (rect(&h, "button:dialogOk"), rect(&h, "dialog:window"));
    assert!(window.right() - ok.right() < 24.0, "confirm {ok:?} at the right edge of {window:?}");
    let width = window.width();
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(rect(&h, "dialog:window").width(), width, "the footer doesn't widen the window");
}
