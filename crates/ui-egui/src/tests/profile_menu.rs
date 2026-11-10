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

/// Opens the profile dropdown on a raw from `camera`, hovers `hover` when given, and returns the widgets drawn.
fn profile_menu_on(camera: &str, hover: Option<&str>) -> Vec<String> {
    use lightcraft_catalog::{MediaKind, Op, Photo, Source};
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let id = app.session.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, "raw.RAF", "RAF", 6240, 4160, "2026-10-22T00:00:00");
    p.kind = MediaKind::Raw;
    p.meta.camera = camera.into();
    app.session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    go(&mut h, "ui.clickWidget", "dropdown:profile");
    assert!(widget(&h, "menu-level:1").is_some(), "the profile menu is open");
    if let Some(w) = hover {
        go(&mut h, "ui.hoverWidget", w);
    }
    h.app.widgets.iter().map(|(w, _)| w.clone()).collect()
}

/// The dropdown lists the measured looks an X-T4 raw renders in their own submenu, and none for another camera's raw.
#[test]
fn an_x_t4_raw_lists_its_measured_looks() {
    let look = lightcraft_engine::camera_looks::find("look:x-t4/velvia").expect("the bundled X-T4 looks");
    let group = format!("profileMenu:lookGroupMenu:{}", look.group);
    let drawn = profile_menu_on("FUJIFILM X-T4", Some(&group));
    assert!(drawn.contains(&group), "an X-T4 raw's menu has the looks' submenu");
    assert!(drawn.contains(&format!("profileMenu:group:{}", look.id)), "the submenu lists the looks");
    let drawn = profile_menu_on("FUJIFILM X-T3", None);
    assert!(!drawn.iter().any(|w| w.starts_with("profileMenu:lookGroupMenu:")), "another camera's raw has none");
}
