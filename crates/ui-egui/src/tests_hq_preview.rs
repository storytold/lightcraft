//! High Quality Preview (`view.highQualityPreview`): the command, its View menu check, its place in
//! the saved UI state, and the loupe asking the engine for High Quality jobs only while it is on.

use std::time::Duration;

use lightcraft_engine::hq_preview::HqState;
use serde_json::json;

use crate::headless::Headless;
use crate::render::Slot;
use crate::state::Zoom;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const W: usize = 3000;
const H: usize = 2000;

/// A generated star field (about 5 % white pixels; never committed) larger than the preview
/// source level, open in Detail in a 1400 × 900 window.
fn detail() -> (Headless, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-ui-hq-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("stars.png");
    let img = lightcraft_raster::Rgba8::from_fn(W, H, |x, y| {
        let h = ((x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b)).wrapping_mul(0x2c1b_3c6d);
        if (h >> 13).is_multiple_of(20) { [255, 255, 255, 255] } else { [0, 0, 0, 255] }
    });
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(&src, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.curve", &json!({"channel": "master", "points": [[0.0, 0.0], [0.4, 1.0], [0.85, 0.15], [1.0, 0.55]]})).unwrap();
    let mut app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    (h, dir)
}

fn run(h: &mut Headless, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": "view.highQualityPreview", "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    r["result"].clone()
}

fn main_texture(h: &Headless) -> (u64, [usize; 2]) {
    let t = h.app.renderer.textures.get(&Slot::Main).expect("the loupe rendered");
    (t.key, t.size)
}

/// The command toggles (no params) and sets (`on`), replies `{on}`, and checks the View menu item.
#[test]
fn the_command_toggles_and_sets_and_checks_the_menu() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    assert!(!h.app.ui.high_quality_preview, "off by default");
    assert_eq!(crate::menubar::checked(&h.app, "view.highQualityPreview"), Some(false));
    assert_eq!(run(&mut h, json!({})), json!({"on": true}));
    assert_eq!(crate::menubar::checked(&h.app, "view.highQualityPreview"), Some(true));
    assert_eq!(run(&mut h, json!({"on": true})), json!({"on": true}), "on stays on");
    assert_eq!(run(&mut h, json!({})), json!({"on": false}));
    assert_eq!(run(&mut h, json!({"on": false})), json!({"on": false}));
    assert_eq!(crate::menubar::checked(&h.app, "view.highQualityPreview"), Some(false));
    let cmd = crate::menus::ui_commands().find(|c| c.0 == "view.highQualityPreview").expect("a UI command");
    assert_eq!((cmd.1, cmd.2, cmd.3), ("High Quality Preview", None, "View"));
}

/// It is saved with the UI state (`ui.json`); settings from before it open with it off.
#[test]
fn it_is_saved_with_the_ui_state() {
    let ui = crate::UiState { high_quality_preview: true, ..Default::default() };
    let saved = serde_json::to_value(&ui).unwrap();
    assert_eq!(saved["highQualityPreview"], true);
    let back: crate::UiState = serde_json::from_value(saved.clone()).unwrap();
    assert!(back.sanitized().high_quality_preview);
    let mut old = saved;
    old.as_object_mut().unwrap().remove("highQualityPreview");
    let back: crate::UiState = serde_json::from_value(old).unwrap();
    assert!(!back.high_quality_preview);
}

/// Off, the loupe renders at display size; on, its whole frame (and, zoomed between fit and 100 %,
/// its window) comes from the native frame, and `ui.inspect` says so; off again, the normal
/// render is back.
#[test]
fn the_loupe_asks_for_high_quality_jobs_only_while_it_is_on() {
    let (mut h, dir) = detail();
    let (plain, size) = main_texture(&h);
    assert!(size[0].max(size[1]) < W, "a fit view below 100 %: {size:?}");
    assert!(h.app.hq_report.is_empty(), "{:?}", h.app.hq_report);

    run(&mut h, json!({"on": true}));
    assert_eq!(h.app.hq_report.get("main"), Some(&HqState::Applied { native: (W, H) }));
    let (hq, hq_size) = main_texture(&h);
    assert_ne!(hq, plain, "another render: the native frame scaled down");
    assert_eq!(hq_size, size, "at the same size");
    let r = h.request("ui.inspect", json!({}), T);
    let report = &r["result"]["highQualityPreview"];
    assert_eq!(report["on"], true, "{r}");
    assert_eq!(report["jobs"]["main"]["state"], "applied", "{report}");

    // between fit and 100 %: the window is cut from the native frame too
    h.app.ui.zoom = Zoom::Percent(75.0);
    h.settle(SETTLE);
    let region = h.app.region_view.expect("a window at 75 %");
    assert!(region.full.0.max(region.full.1) < W, "{region:?}");
    assert!(matches!(h.app.hq_report.get("window"), Some(HqState::Applied { .. })), "{:?}", h.app.hq_report);
    let tile = h.app.renderer.textures.get(&Slot::Region).expect("its texture");
    assert_eq!((tile.size[0], tile.size[1]), (region.window.w, region.window.h), "the window's own pixels");
    // at 100 % the window is the normal one
    h.app.ui.zoom = Zoom::Percent(100.0);
    h.settle(SETTLE);
    assert_eq!(h.app.hq_report.get("window"), Some(&HqState::Native));

    h.app.ui.zoom = Zoom::Fit;
    run(&mut h, json!({"on": false}));
    assert!(h.app.hq_report.is_empty());
    assert_eq!(main_texture(&h).0, plain, "the normal render again");
    let _ = std::fs::remove_dir_all(&dir);
}
