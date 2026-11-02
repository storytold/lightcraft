//! View ▸ Hide Rejected Photos (Cmd/Ctrl+Alt+X, issue #3513): the key hides the grid's rejected
//! photos and shows them again, and the View menu row is checked while they are hidden.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ctrl_alt_x(h: &mut Headless) {
    let r = h.request("ui.key", json!({"key": "X", "cmd": true, "alt": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn the_key_hides_rejected_photos_and_the_menu_shows_it() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let all: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    let rejected = all[1];
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [rejected]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "photo.flag", "params": {"flag": "reject"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);

    let entry = crate::menus::menu_entries(&h.app).into_iter().find(|e| e.id == "library.hideRejected").expect("in the menus");
    assert_eq!((entry.menu, entry.shortcut.as_deref()), (vec!["View".to_string()], Some("Cmd+Alt+X")));
    assert_eq!(crate::menubar::checked(&h.app, "library.hideRejected"), Some(false));

    ctrl_alt_x(&mut h);
    let shown: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    let catalog = &h.app.session.catalog;
    let not_rejected: Vec<u64> = all
        .iter()
        .copied()
        .filter(|id| catalog.photo(lightcraft_catalog::PhotoId(*id)).is_some_and(|p| p.flag != lightcraft_catalog::Flag::Reject))
        .collect();
    assert!(!shown.contains(&rejected), "the rejected photo is hidden");
    assert_eq!(shown, not_rejected, "every other photo stays");
    assert_eq!(crate::menubar::checked(&h.app, "library.hideRejected"), Some(true));

    ctrl_alt_x(&mut h);
    assert_eq!(h.app.session.visible_cloned().len(), all.len(), "the rejected photo is back");
    assert_eq!(crate::menubar::checked(&h.app, "library.hideRejected"), Some(false));
}
