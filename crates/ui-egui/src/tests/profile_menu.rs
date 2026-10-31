//! #2059: the profile dropdown and its submenus stay inside the window. A folder of many imported
//! profiles used to make its submenu run past the window's bottom edge, and the profiles below it
//! could not be reached.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(5);
const SETTLE: Duration = Duration::from_secs(10);
const FOLDER: &str = "Imported";

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn go(h: &mut Headless, method: &str, id: &str) {
    let r = h.request(method, json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{method} {id}: {r}");
    h.settle(SETTLE);
}

#[test]
fn a_folder_of_many_imported_profiles_scrolls_inside_the_window() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    for i in 0..80 {
        app.session.lut_profiles.push(lightcraft_engine::cmd::lut_profiles::LutProfile {
            id: format!("lut:imported-{i}"),
            name: format!("Imported {i}"),
            group: FOLDER.into(),
            file: String::new(),
        });
    }
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.settle(SETTLE);
    go(&mut h, "ui.clickWidget", "dropdown:profile");
    let menu = widget(&h, "menu-level:1").expect("the profile menu is open");
    assert!(menu.bottom() <= h.size.y + 0.5, "the profile menu {menu:?} runs past the window's bottom {}", h.size.y);
    go(&mut h, "ui.hoverWidget", &format!("profileMenu:lutGroupMenu:{FOLDER}"));
    let sub = widget(&h, "menu-level:2").expect("the imported folder's submenu is open");
    assert!(sub.bottom() <= h.size.y + 0.5, "the submenu {sub:?} runs past the window's bottom {}", h.size.y);
    // Its 80 rows don't all fit: some are scrolled out of view rather than drawn off screen.
    let rows: Vec<egui::Rect> = (0..80).filter_map(|i| widget(&h, &format!("profileMenu:group:lut:imported-{i}"))).collect();
    assert!(!rows.is_empty(), "no imported profile rows drawn");
    assert!(rows.iter().any(|r| !sub.contains_rect(*r)), "every row fits a 900 pt window");
}
