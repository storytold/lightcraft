//! LR-PROF-MANAGE: the Profile Manager window (opened from the profile browser's Manage… and from
//! Settings ▸ Profiles) hides and deletes profiles, a deleted profile a photo uses breaks nothing,
//! the browser's None leaves only the camera profile, and the window fits the screen.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::tests_classic::{click, demo, has, run};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(10);

/// The demo library in Detail with photo `0` selected and the profile browser open.
fn browser() -> Headless {
    let mut h = demo();
    let id = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [id.0]}));
    h.request("ui.set", json!({"view": "detail"}), T);
    run(&mut h, "panel.profiles", json!({}));
    h.step();
    h
}

fn manager_open(h: &Headless) -> bool {
    matches!(h.app.ui.dialog, Some(crate::state::Dialog::ProfileManager { .. }))
}

/// Give the session a LUT profile of its own (no file: nothing on disk is touched).
fn add_lut(h: &mut Headless, id: &str) {
    let cube = "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    lcc_pipeline::lut::register(id, lcc_pipeline::lut::Lut3d::parse_cube(cube).unwrap());
    h.app.session.lut_profiles.push(lcc_engine::cmd::lut_profiles::LutProfile {
        id: id.into(),
        name: "Test LUT".into(),
        group: "My LUTs".into(),
        file: String::new(),
    });
}

#[test]
fn the_browser_takes_the_look_off_and_opens_the_manager_which_hides_profiles() {
    let mut h = browser();
    // None in the profile browser takes the creative profile off in one click
    {
        run(&mut h, "develop.profile", json!({"id": "lc.vivid", "amount": 100}));
        let id = h.app.session.active().unwrap();
        assert_eq!(h.app.session.develop_of(id).unwrap().profile.id, "lc.vivid");
        click(&mut h, "button:profileNone");
        let d = h.app.session.develop_of(id).unwrap();
        assert_eq!(d.profile.id, crate::panels::profiles::NO_LOOK);
        assert!(matches!(lcc_pipeline::profiles::effective(&d), std::borrow::Cow::Borrowed(_)), "no look on top of the camera profile");
    }
    assert!(has(&h, "button:profilesManage"), "the browser has Manage…");
    click(&mut h, "button:profilesManage");
    assert!(manager_open(&h), "Manage… opens the Profile Manager");
    search(&mut h, "vivid");
    assert!(has(&h, "profileManagerRow:lc.vivid"), "the built-in looks are listed");
    click(&mut h, "icon:profileManagerHide:lc.vivid");
    assert!(h.app.session.profile_hidden.iter().any(|x| x == "lc.vivid"), "Hide hides it");
    let listed = h.app.session.execute("profiles.list", &json!({})).unwrap();
    assert!(listed.as_array().unwrap().iter().all(|p| p["id"] != "lc.vivid"), "out of the profile menu");
    assert!(has(&h, "profileManagerRow:lc.vivid"), "a hidden profile stays in the manager, to show it again");
    // a built-in can't be deleted: its Delete asks nothing
    click(&mut h, "icon:profileManagerDelete:lc.vivid");
    assert!(!has(&h, "button:profileManager:lc.vividDeleteConfirm"), "built-ins are hidden, not deleted");
    assert!(lcc_engine::presets::PROFILES.iter().any(|p| p.id == "lc.vivid"));
    click(&mut h, "icon:profileManagerHide:lc.vivid");
    assert!(h.app.session.profile_hidden.is_empty(), "Show shows it again");
}

/// Deleting a profile a photo uses asks in its row, deletes it, and the photo keeps working: its
/// settings stay, the browser and the manager still draw.
#[test]
fn deleting_a_profile_a_photo_uses_does_not_break_it() {
    let mut h = browser();
    let lut = "lut:ui-pm-in-use";
    add_lut(&mut h, lut);
    run(&mut h, "develop.profile", json!({"id": lut, "amount": 100}));
    let id = h.app.session.active().unwrap();
    run(&mut h, "dialog.profileManager", json!({}));
    search(&mut h, "Test LUT");
    assert!(has(&h, &format!("profileManagerRow:{lut}")), "the LUT profile is listed");
    click(&mut h, &format!("icon:profileManagerDelete:{lut}"));
    let confirm = format!("button:profileManager:{lut}DeleteConfirm");
    assert!(has(&h, &confirm), "Delete asks in the row first");
    assert!(h.app.session.lut_profiles.iter().any(|p| p.id == lut), "nothing deleted before the yes");
    click(&mut h, &confirm);
    assert!(h.app.session.lut_profiles.iter().all(|p| p.id != lut), "deleted");
    assert!(lcc_pipeline::lut::get(lut).is_none());
    assert_eq!(h.app.session.develop_of(id).unwrap().profile.id, lut, "the photo keeps its settings");
    for _ in 0..4 {
        h.step();
    }
    assert!(!has(&h, &format!("profileManagerRow:{lut}")), "gone from the manager");
    // the browser of the photo using it still draws
    h.app.ui.dialog = None;
    // (panel.profiles toggles: open it only when it isn't)
    if h.app.ui.right != crate::state::RightPanel::Profiles {
        run(&mut h, "panel.profiles", json!({}));
    }
    for _ in 0..4 {
        h.step();
    }
    assert!(has(&h, "button:profileNone"));
}

fn window(h: &Headless) -> egui::Rect {
    h.app.widgets.iter().rev().find(|(w, _)| w == "dialog:window").map(|(_, r)| *r).expect("the manager's window")
}

/// The window opens inside the screen, never grows frame after frame, remembers its size and opens
/// at it again.
#[test]
fn the_manager_fits_the_screen_and_keeps_its_size() {
    for screen in [[1600.0, 1000.0], [900.0, 640.0]] {
        let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, screen, 1.0);
        h.settle(Duration::from_secs(60));
        assert_eq!(h.request("engine.execute", json!({"command": "dialog.profileManager", "params": {}}), T)["ok"], true);
        for _ in 0..4 {
            h.step();
        }
        let first = window(&h);
        assert!(
            first.left() >= 0.0 && first.right() <= screen[0] && first.top() >= 0.0 && first.bottom() <= screen[1],
            "{first:?} inside {screen:?}"
        );
        for _ in 0..30 {
            h.step();
        }
        assert_eq!(window(&h).size(), first.size(), "no growth frame after frame");
        assert!(h.app.ui.profile_manager_size.is_some(), "its size is remembered");
        assert!(h.app.ui.presets_manager_size.is_none(), "apart from the Presets Manager's");
    }
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.profile_manager_size = Some([700.0, 500.0]);
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.settle(Duration::from_secs(60));
    assert_eq!(h.request("engine.execute", json!({"command": "dialog.profileManager", "params": {}}), T)["ok"], true);
    for _ in 0..6 {
        h.step();
    }
    let w = window(&h);
    assert!((w.width() - 700.0).abs() < 2.0 && w.height() > 440.0 && w.height() <= 500.5, "{w:?}");
}

/// Pictures of the manager and the browser for a review, into `$PROFILE_MANAGER_SNAPSHOTS` (run with
/// `--ignored`): the table, a row's Delete asking, and the browser with Manage… and None.
#[test]
#[ignore = "writes pictures for a review"]
fn profile_manager_snapshots() {
    let Ok(dir) = std::env::var("PROFILE_MANAGER_SNAPSHOTS") else { return };
    let dir = std::path::PathBuf::from(dir);
    let save = |h: &mut Headless, name: &str| {
        let img = h.snapshot(Duration::from_secs(20));
        let [w, hh] = img.size;
        let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        image::RgbaImage::from_raw(w as u32, hh as u32, raw).unwrap().save(dir.join(name)).unwrap();
    };
    let mut h = browser();
    save(&mut h, "01-browser.png");
    add_lut(&mut h, "lut:snapshot-look");
    run(&mut h, "develop.profile", json!({"id": "lut:snapshot-look", "amount": 100}));
    click(&mut h, "button:profilesManage");
    h.app.session.profile_hidden.push("lc.portrait".into());
    h.step();
    h.step();
    save(&mut h, "02-manager.png");
    click(&mut h, "icon:profileManagerDelete:lut:snapshot-look");
    save(&mut h, "03-delete-asks.png");
}

/// Narrow the open manager to `q`, so the row under test is in view whatever else other tests put
/// in the process-wide profile registries.
fn search(h: &mut Headless, q: &str) {
    if let Some(crate::state::Dialog::ProfileManager { query, .. }) = h.app.ui.dialog.as_mut() {
        *query = q.into();
    }
    h.step();
    h.step();
}
