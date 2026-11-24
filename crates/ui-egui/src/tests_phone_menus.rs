//! Headless tests of the menu on a phone (#1288): at a phone's size, upright and on its side,
//! every top-level menu of the Modern layout's ☰ bar can be opened, its title scrolls into view,
//! and every row of it and of its submenus lies inside the screen, so it can be read and tapped.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

/// A typical phone upright, and on its side.
const UPRIGHT: [f32; 2] = [390.0, 844.0];
const SIDEWAYS: [f32; 2] = [844.0, 390.0];

fn phone(size: [f32; 2]) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("engine.execute", json!({"command": "view.modernLayout", "params": {"on": true}}), Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(60));
    h
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// Whether `r` lies across inside a screen `w` wide (a row cut off at a side can't be read).
fn across(r: egui::Rect, w: f32) -> bool {
    r.left() >= -0.5 && r.right() <= w + 0.5
}

/// Every row (items and submenus) drawn this frame that lies off the screen.
fn off_screen(h: &Headless, size: [f32; 2]) -> Vec<String> {
    h.app
        .widgets
        .iter()
        .filter(|(w, r)| (w.starts_with("menuitem:") || w.starts_with("menusub:")) && r.height() > 0.0 && !across(*r, size[0]))
        .map(|(w, r)| format!("{w} {r:?}"))
        .collect()
}

fn check_every_menu(size: [f32; 2]) {
    let mut h = phone(size);
    assert!(h.click_as_user("modern:menu"), "the ☰ button is on screen");
    let titles = crate::modern_menu::menu_titles(&h.app);
    assert!(titles.len() > 5, "{titles:?}");
    for title in &titles {
        // (a swipe along the bar brings a title in; picking it scrolls it fully into view)
        h.app.ui.modern.menu = Some(title.clone());
        h.settle(Duration::from_secs(5));
        let t = rect(&h, &format!("modern:menu:{title}")).unwrap_or_else(|| panic!("{title}: no title"));
        assert!(across(t, size[0]), "{title}: title off screen {t:?}");
        assert!(h.click_as_user(&format!("modern:menu:{title}")), "{title}: tappable");
        assert_eq!(h.app.ui.modern.menu.as_deref(), Some(title.as_str()), "{title}: stays open after a tap");
        let items = rect(&h, "modern:menuItems").unwrap_or_else(|| panic!("{title}: no items"));
        assert!(across(items, size[0]), "{title}: menu off screen {items:?}");
        let bad = off_screen(&h, size);
        assert!(bad.is_empty(), "{title}: rows off screen {bad:?}");
        // each submenu opens inside the screen too
        let subs: Vec<String> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("menusub:").map(str::to_string)).collect();
        for sub in subs {
            if !h.click_as_user(&format!("menusub:{sub}")) {
                continue;
            }
            let bad = off_screen(&h, size);
            assert!(bad.is_empty(), "{title} ▸ {sub}: rows off screen {bad:?}");
            // back to the menu alone
            h.app.ui.modern.menu = None;
            h.settle(Duration::from_secs(5));
            h.app.ui.modern.menu = Some(title.clone());
            h.settle(Duration::from_secs(5));
        }
    }
}

#[test]
fn every_menu_and_its_rows_fit_an_upright_phone() {
    check_every_menu(UPRIGHT);
}

#[test]
fn every_menu_and_its_rows_fit_a_phone_on_its_side() {
    check_every_menu(SIDEWAYS);
}
