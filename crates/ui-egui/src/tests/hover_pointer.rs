//! The chrome answers the pointer: the top bar's icon buttons and in-window menu titles show the
//! theme's hover fill, and they, the sidebar's rows, section headers and disclosure chevrons show
//! the pointing hand. A disabled button, and the sidebar title (a right-click menu only), keep the
//! arrow.
//!
//! With `LIGHTCRAFT_TEST_SHOTS=<dir>` the tests also write `hover-*.png`.

use std::time::Duration;

use egui::CursorIcon;
use serde_json::json;

use crate::headless::Headless;
use crate::theme::Tokens;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    // a PNG encoder and file writes, for the screenshots `LIGHTCRAFT_TEST_SHOTS` asks for
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let services = Services {
        png: Some(Box::new(png)),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        write_new: Some(Box::new(lightcraft_engine::export::write_file_new)),
        ..Default::default()
    };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1600.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// What resting the pointer on the middle of `id` shows: the cursor, and whether a rectangle under
/// the pointer is filled with the theme's hover colour.
fn hover(h: &mut Headless, id: &str) -> (CursorIcon, bool) {
    let at = widget(h, id).center();
    let r = h.request("ui.move", json!({"x": at.x, "y": at.y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let fill = Tokens::get(&h.view.ctx).hover;
    let filled = h.view.painted_shapes().iter().any(|s| matches!(s, egui::epaint::Shape::Rect(r) if r.fill == fill && r.rect.contains(at)));
    let cursor = h.last_cursor;
    if let Some(dir) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") {
        let path = std::path::Path::new(&dir).join(format!("hover-{}.png", id.replace([':', '/'], "-")));
        let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    (cursor, filled)
}

#[test]
fn top_bar_buttons_and_menu_titles_show_the_hover_fill_and_the_hand() {
    let mut h = demo();
    let mut wrong = Vec::new();
    for id in ["icon:back", "menu:File", "menu:Help"] {
        let (cursor, filled) = hover(&mut h, id);
        if cursor != CursorIcon::PointingHand || !filled {
            wrong.push(format!("{id}: {cursor:?}, hover fill {filled}"));
        }
    }
    // Forward has nowhere to go: no hand, no fill
    let (cursor, filled) = hover(&mut h, "icon:forward");
    if cursor != CursorIcon::Default || filled {
        wrong.push(format!("icon:forward (disabled): {cursor:?}, hover fill {filled}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn sidebar_rows_headers_and_chevrons_show_the_hand() {
    let mut h = demo();
    let chevron = h
        .app
        .widgets
        .iter()
        .map(|(w, _)| w.clone())
        .find(|w| w.starts_with("dateToggle:") || w.starts_with("folderToggle:"))
        .expect("a disclosure chevron in the sidebar");
    let mut wrong = Vec::new();
    for id in ["source:all", "source:recentlyAdded", "sidebarSection:albums", chevron.as_str()] {
        let (cursor, _) = hover(&mut h, id);
        if cursor != CursorIcon::PointingHand {
            wrong.push(format!("{id}: {cursor:?}"));
        }
    }
    // the title only has a right-click menu (the sidebar's sections)
    let (cursor, _) = hover(&mut h, "sidebarTitle");
    if cursor != CursorIcon::Default {
        wrong.push(format!("sidebarTitle: {cursor:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
