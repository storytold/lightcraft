//! Headless test of Photo ▸ Enhance…: the dialog opens on the selection, previews each method with an estimate,
//! zooms on a click, and its Enhance button runs the command (a new photo stacked with its source).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(180);

fn click_at(h: &mut Headless, pos: egui::Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
    }
    h.settle(T);
}

#[test]
fn the_enhance_dialog_previews_both_methods_and_enhances_the_selection() {
    let dir = lcc_testkit::TempDir::new("lc-enhance-dialog");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bytes = lcc_merge::synth::bracket_dngs(640, 480, &[0.0]).unwrap().remove(0);
    let path = dir.join("IMG_1.dng");
    std::fs::write(&path, bytes).unwrap();
    let mut session = lcc_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = r["imported"][0].as_u64().unwrap();
    session.execute("library.select", &json!({"ids": [id]})).unwrap();
    let mut h = Headless::new(LccApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);

    h.app.run("dialog.enhance", json!({})).unwrap();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Enhance { .. })), "the command opens the dialog");
    assert!(h.step_until(T, |h| h.app.enhance.preview.is_some() && !h.app.enhance.busy()), "a Raw Details preview: {:?}", h.app.enhance.error);
    let p = h.app.enhance.preview.as_ref().unwrap();
    assert_eq!(p.out_size, (640, 480), "Raw Details keeps the size");
    assert!(p.estimate_secs.is_finite() && p.estimate_secs >= 0.0);

    let area = h.app.widgets.iter().find(|(w, _)| w == "enhance.preview").map(|(_, r)| *r).expect("the preview box");
    click_at(&mut h, area.center());
    assert!(h.app.enhance.zoom.is_some(), "a click zooms the preview in");

    if let Some(Dialog::Enhance { opts }) = &mut h.app.ui.dialog {
        opts.super_resolution = true;
    }
    assert!(
        h.step_until(T, |h| h.app.enhance.preview.as_ref().is_some_and(|p| p.out_size == (1280, 960)) && !h.app.enhance.busy()),
        "switching to Super Resolution previews it at twice the size: {:?}",
        h.app.enhance.error
    );
    let photos = h.app.session.catalog.photos().count();
    let Some(dlg) = h.app.ui.dialog.clone() else { panic!("the dialog closed") };
    crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).unwrap();
    h.app.ui.dialog = None;
    assert!(h.step_until(T, |h| h.app.session.catalog.photos().count() > photos), "Enhance made a new photo");
    let made = h.app.session.catalog.photos().find(|p| p.file_name.contains("Enhanced-SR")).expect("a Super Resolution DNG");
    assert!(h.app.session.catalog.stack_of(made.id).is_some(), "Create Stack (on by default) stacked it with its source");
    let _ = std::fs::remove_dir_all(dir);
}
