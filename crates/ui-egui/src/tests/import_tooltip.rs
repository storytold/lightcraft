//! An unreadable file's tooltip, as drawn: egui tooltips don't scroll, so the reason must land inside the window
//! however deep the file's path is (issue #3342). The scan's error for a file it can't open is `{path}: {reason}`.

use std::time::Duration;

use serde_json::json;

use lightcraft_engine::import::ImportCandidate;

use crate::dialogs::import::ImportDialog;
use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const REASON: &str = "Access is denied. (os error 5)";

/// Where the last frame drew the row of text holding the end of [`REASON`], with the shape's whole text.
fn reason_row(h: &Headless) -> Option<(egui::Rect, String)> {
    h.view.painted_shapes().into_iter().find_map(|shape| {
        let egui::epaint::Shape::Text(text) = shape else { return None };
        let row = text.galley.rows.iter().find(|row| row.glyphs.iter().map(|g| g.chr).collect::<String>().contains("os error 5"))?;
        Some((row.rect().translate(text.pos.to_vec2()), text.galley.job.text.clone()))
    })
}

#[test]
fn a_deep_paths_scan_error_keeps_its_reason_inside_the_window() {
    let path = format!("C:/{}a.ARW", "photographs-of-the-long-trip/".repeat(150));
    let candidate = ImportCandidate {
        path: path.clone(),
        name: "a.ARW".into(),
        format: "ARW".into(),
        error: Some(format!("{path}: {REASON}")),
        ..Default::default()
    };
    let app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    h.app.ui.dialog = Some(Dialog::Import { opts: Box::new(ImportDialog::new(vec![candidate; 4])) });
    h.step();
    h.step();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
    for cell in ["import:0", "import:3"] {
        // move off the cells, so the next one starts its own tooltip
        assert_eq!(h.request("ui.move", json!({"x": 2.0, "y": 2.0}), T)["ok"], true);
        h.step();
        assert_eq!(h.request("ui.hoverWidget", json!({"id": cell}), T)["ok"], true, "{cell}");
        assert!(h.step_until(T, |h| reason_row(h).is_some()), "{cell}: no tooltip with the reason");
        let (row, text) = reason_row(&h).unwrap();
        assert!(screen.contains_rect(row), "{cell}: the reason's row {row:?} is outside the {screen:?} window");
        assert!(text.len() > path.len(), "{cell}: the tooltip still has the path");
    }
}
