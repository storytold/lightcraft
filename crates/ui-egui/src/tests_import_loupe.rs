//! The Import tab's Folders panel and loupe (user test notes, 9 Nov 2026): the Folders panel drags wider by its
//! right edge and keeps the width; the loupe takes no Develop tool from the right panel left open there (no
//! "Overlay on · O" note over a browsed photo), and one click zooms Fit ↔ 1:1 at the point clicked, as in Library.

use std::time::Duration;

use serde_json::{Value, json};

use crate::LccApp;
use crate::headless::Headless;
use crate::import_tab::Tab;
use crate::state::{RightPanel, ViewMode, Zoom};

const T: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(120);

fn app(compact: bool) -> Headless {
    app_sized(compact, [1600.0, 900.0])
}

fn app_sized(compact: bool, size: [f32; 2]) -> Headless {
    let services = crate::Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::new(), services);
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.settings.compact_layout = compact;
    Headless::new(app, size, 1.0)
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command} {params}: {r}");
    h.settle(SETTLE);
    r["result"].clone()
}

/// A folder of `n` small PNGs in the temporary folder, removed when the test ends.
fn folder(name: &str, n: u8) -> lcc_testkit::TempDir {
    let dir = lcc_testkit::TempDir::new(&format!("lc-ui-importloupe-{name}"));
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    for i in 0..n {
        let img = lcc_raster::Rgba8::from_fn(240, 160, |x, y| [(x % 255) as u8, (y % 255) as u8, 40 * i, 255]);
        std::fs::write(dir.join(format!("p{i}.png")), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    }
    dir
}

fn steps(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn click_at(h: &mut Headless, at: egui::Pos2) {
    let m = egui::Modifiers::NONE;
    h.events.push(egui::Event::PointerMoved(at));
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    h.step();
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
    steps(h, 3);
}

/// The Import tab's loupe on a browsed folder, the right panel left on Masks (from Develop) with the overlay
/// switched on to stay (O), as the user had it. The folder comes first: bound as `let (dir, h)`, the app
/// drops before the folder goes.
fn import_loupe(name: &str) -> (lcc_testkit::TempDir, Headless) {
    let mut h = app(true);
    h.settle(SETTLE);
    exec(&mut h, "dialog.import", json!({}));
    let dir = folder(name, 3);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.ui.tab, Tab::Import);
    h.app.ui.right = RightPanel::Masking;
    h.app.ui.mask_overlay = true;
    h.app.ui.view = ViewMode::Detail;
    h.settle(SETTLE);
    steps(&mut h, 3);
    assert!(widget(&h, "canvas:image").is_some(), "the loupe shows the photo");
    (dir, h)
}

/// The Folders panel (the sources on the left of the Import tab) drags wider by its right edge, as Library's left
/// panel does (over its rows and scroll bar too), and the width is part of the saved UI state.
#[test]
fn the_import_folders_panel_resizes_by_its_right_edge_and_remembers_it() {
    // a short window: the panel's list scrolls, so its scroll bar runs along the right edge (as with a real
    // computer's drives and folders), where it took the drag
    let mut h = app_sized(true, [1600.0, 320.0]);
    h.settle(SETTLE);
    exec(&mut h, "dialog.import", json!({}));
    // (the drives are listed on a worker thread)
    for _ in 0..200 {
        if h.app.widgets.iter().any(|(w, _)| w.starts_with("source:")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        h.step();
    }
    let before = widget(&h, "panel:importSources").expect("the Folders panel is drawn");
    assert!((before.width() - crate::state::IMPORT_SOURCES_WIDTH.default).abs() <= 1.0, "{before:?}");
    // grabbed over a row of the panel (a drive's folder tree), as in the app, where the rows took the drag
    let x = before.right() - 1.0;
    let row = h.app.widgets.iter().find(|(w, r)| w.starts_with("source:") && before.contains(r.center())).map(|(w, r)| (w.clone(), *r));
    let (w_id, row) = row.unwrap_or_else(|| panic!("a row in the Folders panel: {:?}", h.app.widgets.iter().map(|(w, _)| w).collect::<Vec<_>>()));
    let y = row.center().y;
    assert!(row.right() >= x - 6.0, "{w_id} {row:?}");
    let r = h.request("ui.drag", json!({"x": x, "y": y, "toX": x + 150.0, "toY": y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h, 3);
    let w = h.app.ui.import_sources_width;
    assert!((w - (crate::state::IMPORT_SOURCES_WIDTH.default + 150.0)).abs() <= 2.0, "{w}");
    assert!((widget(&h, "panel:importSources").unwrap().width() - w).abs() <= 1.0);
    // kept in the saved UI state, and a hostile value comes back in range
    let saved = serde_json::to_value(&h.app.ui).unwrap();
    let back: crate::state::UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(back.import_sources_width, w);
    let mut bad = back.clone();
    bad.import_sources_width = f32::NAN;
    bad.clamp_ranges();
    assert_eq!(bad.import_sources_width, crate::state::IMPORT_SOURCES_WIDTH.default);
}

/// No mask tool works in the Import tab, so its loupe never says "Overlay on · O", whatever the right panel was
/// left on in Develop.
#[test]
fn the_import_loupe_shows_no_overlay_on_note() {
    let (dir, h) = import_loupe("pill");
    assert!(widget(&h, "canvas:overlayOn").is_none(), "no Overlay on note in the Import tab");
    let _ = std::fs::remove_dir_all(&dir);
}

/// One click in the Import tab's loupe zooms to the click-zoom ratio at the point clicked; another goes back to Fit.
/// A mask tool left armed in Develop (the Brush) took the click before: it painted a mask on the browsed photo.
#[test]
fn one_click_zooms_the_import_loupe_and_back() {
    let (dir, mut h) = import_loupe("zoom");
    h.app.ui.tool = "brush".into();
    steps(&mut h, 2);
    assert!(matches!(h.app.ui.zoom, Zoom::Fit));
    let img = widget(&h, "canvas:image").unwrap();
    click_at(&mut h, img.min + egui::vec2(img.width() * 0.25, img.height() * 0.75));
    assert_eq!(h.app.ui.zoom, Zoom::Percent(h.app.ui.click_zoom as f32), "one click zooms in");
    let (u, v) = h.app.ui.pan;
    assert!((u - 0.25).abs() < 0.05 && (v - 0.75).abs() < 0.05, "at the point clicked: {:?}", h.app.ui.pan);
    assert_eq!(h.app.session.develop_of(h.app.session.active().unwrap()).map(|d| d.masks.len()), Some(0), "and makes no mask");
    steps(&mut h, 10);
    let canvas = h.app.canvas_rect.unwrap();
    click_at(&mut h, canvas.center());
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "another click goes back to Fit");
    let _ = std::fs::remove_dir_all(&dir);
}
