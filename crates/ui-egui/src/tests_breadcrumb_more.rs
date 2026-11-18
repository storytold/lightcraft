//! The folder view's breadcrumb collapses the folders above the last few into "…": a click on it
//! lists those folders, and each one opens.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(30);

#[test]
fn the_breadcrumb_dots_list_the_hidden_folders_and_each_opens() {
    let root = lcc_testkit::TempDir::new("app-crumb-more");
    let deep = root.join("a").join("b").join("c").join("d").join("e");
    std::fs::create_dir_all(&deep).unwrap();
    let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(deep.join("a.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    // the grid's folder view (with the Import tab, a browsed folder shows there instead)
    app.ui.settings.import_as_tab = false;
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": deep.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.settle(T);
    let names: Vec<&String> = h.app.widgets.iter().map(|(w, _)| w).filter(|w| w.starts_with("crumb:")).collect();
    assert!(h.app.widgets.iter().any(|(w, _)| w == "crumb:more"), "a deep folder collapses its ancestors into …: {names:?} in {:?}", h.app.ui.view);
    // the first folder of the path is hidden behind "…" until the menu opens
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "crumb:0"));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "crumb:more"}), T)["ok"], true);
    h.step();
    h.step();
    let crumbs = crate::panels::grid::crumbs(&deep.to_string_lossy()).into_iter().map(|(_, to)| to).collect::<Vec<_>>();
    let parent_of_a = crumbs.len() - 6;
    let id = format!("crumb:{parent_of_a}");
    assert!(h.app.widgets.iter().any(|(w, _)| *w == id), "the menu lists the hidden {id}");
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true);
    h.settle(T);
    let browsed = h.app.session.browse.as_ref().map(|b| std::path::PathBuf::from(&b.path));
    assert_eq!(browsed.as_deref(), Some(root.path()), "the hidden folder opens");
    let _ = std::fs::remove_dir_all(&root);
}
