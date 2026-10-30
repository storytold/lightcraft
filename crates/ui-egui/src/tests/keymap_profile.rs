//! Headless tests of the keymap profiles (Help ▸ Keyboard Shortcuts ▸ Keymap Profile): the
//! Classic profile resolves Lightroom Classic's own defaults and secondary keys, the user's
//! rebindings sit above whichever profile is active, and `app.setKeymapProfile` switches —
//! an unknown name refused, the choice saved with the UI state.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::keymap_profiles::{CLASSIC, KeyProfile};
use crate::shortcuts::{self, Keymap};
use crate::state::RightPanel;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn key(h: &mut Headless, k: &str, shift: bool) {
    let r = h.request("ui.key", json!({"key": k, "shift": shift}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

/// Each profile resolves its own defaults: Classic's table replaces the declared keys, `""` in
/// it removes one, and the shipped profile keeps the shipped keys.
#[test]
fn profiles_resolve_their_own_defaults() {
    let km = Keymap::new();
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Lightroom, &km, "view.detail"), Some("D"));
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Lightroom, &km, "panel.crop"), Some("C"));
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "view.detail"), Some("E"));
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "panel.edit"), Some("D"));
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "panel.crop"), Some("R"));
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "tool.radial"), Some("Shift+M"));
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "panel.info"), None);
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "panel.presets"), None);
}

/// The user's own binding sits above the active profile on either side of a switch, and Reset
/// restores the active profile's key — not the declared one.
#[test]
fn user_bindings_sit_above_the_profile_and_reset_restores_it() {
    let mut km = Keymap::new();
    shortcuts::assign(KeyProfile::Classic, &mut km, "view.detail", Some("J")).unwrap();
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "view.detail"), Some("J"));
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Lightroom, &km, "view.detail"), Some("J"), "the profile doesn't touch user entries");
    shortcuts::reset(KeyProfile::Classic, &mut km, "view.detail").unwrap();
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "view.detail"), Some("E"), "reset restores the profile's key, not the declared one");
    // a key the profile gave to another command is taken from it when the user claims it
    let mut km = Keymap::new();
    let lost = shortcuts::assign(KeyProfile::Classic, &mut km, "view.survey", Some("R")).unwrap();
    assert!(lost.contains(&"panel.crop"), "{lost:?}");
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Classic, &km, "panel.crop"), None);
    assert_eq!(shortcuts::shortcut_of(KeyProfile::Lightroom, &km, "panel.crop"), None, "the user's explicit removal sits above every profile");
}

/// `app.setKeymapProfile` switches profiles; an unknown name is refused and changes nothing;
/// the choice saves with the UI state (`ui.json` → `settings.keymapProfile`).
#[test]
fn set_keymap_profile_switches_and_refuses_unknown_names() {
    let mut h = demo();
    let entry = |h: &crate::headless::Headless, id: &str| crate::menus::menu_entries(&h.app).into_iter().find(|e| e.id == id).unwrap();
    assert_eq!(entry(&h, "view.detail").shortcut.as_deref(), Some("D"));
    let r = h.app.run("app.setKeymapProfile", json!({"profile": "classic"})).unwrap();
    assert_eq!(r["profile"], "classic", "{r}");
    assert_eq!(h.app.ui.settings.profile(), KeyProfile::Classic);
    assert_eq!(h.app.ui.settings.keymap_profile, CLASSIC);
    // menus and the editor read the profile's keys
    assert_eq!(entry(&h, "view.detail").shortcut.as_deref(), Some("E"), "{:?}", entry(&h, "view.detail").shortcut);
    let e = h.app.run("app.setKeymapProfile", json!({"profile": "what"})).unwrap_err();
    assert_eq!(e, "unknown keymap profile");
    assert_eq!(h.app.ui.settings.profile(), KeyProfile::Classic, "an unknown name changes nothing");
    // the choice is saved with the UI state and read back
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    assert!(saved.contains("\"keymapProfile\":\"classic\""), "{saved}");
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap();
    assert_eq!(back.settings.profile(), KeyProfile::Classic);
    h.app.run("app.setKeymapProfile", json!({"profile": "lightroom"})).unwrap();
    assert_eq!(h.app.ui.settings.profile(), KeyProfile::Lightroom);
    assert_eq!(entry(&h, "view.detail").shortcut.as_deref(), Some("D"));
}

/// The shipped profile gives R to the Radial Gradient tool; the Classic profile gives it to
/// Crop, without rewriting the user's keymap — and the user's own removal still wins.
#[test]
fn the_classic_profile_dispatches_its_own_keys() {
    let mut h = demo();
    key(&mut h, "R", false);
    assert_eq!(h.app.ui.tool, "radial", "shipped: R is the Radial Gradient tool");
    let keymap_before = h.app.ui.settings.keymap.clone();
    h.app.run("app.setKeymapProfile", json!({"profile": "classic"})).unwrap();
    h.app.ui.tool.clear();
    h.app.ui.right = RightPanel::None;
    key(&mut h, "R", false);
    assert_eq!(h.app.ui.right, RightPanel::Crop, "Classic: R opens Crop");
    assert!(h.app.ui.tool.is_empty(), "panel.crop clears the tool");
    assert_eq!(h.app.ui.settings.keymap, keymap_before, "the profile doesn't rewrite the user's keymap");
    // the user's own removal wins over the profile's key
    h.app.run("app.setShortcut", json!({"id": "panel.crop", "shortcut": null})).unwrap();
    h.app.ui.right = RightPanel::None;
    key(&mut h, "R", false);
    assert_eq!(h.app.ui.right, RightPanel::None, "an unbound key does nothing");
}

/// The Classic profile's secondary keys (CLASSIC_ALIASES) fire only under that profile: ⇧P is
/// the Presets panel as shipped and flags the photo in the Classic profile.
#[test]
fn classic_aliases_fire_only_under_the_classic_profile() {
    let mut h = demo();
    let active = h.app.session.active().expect("demo has an active photo");
    key(&mut h, "P", true);
    assert!(h.app.ui.presets, "shipped: ⇧P is the Presets panel");
    assert_eq!(h.app.session.catalog.photo(active).unwrap().flag, lightcraft_catalog::Flag::None);
    h.app.run("app.setKeymapProfile", json!({"profile": "classic"})).unwrap();
    h.app.ui.presets = false;
    key(&mut h, "P", true);
    assert!(!h.app.ui.presets, "panel.presets has no key in the Classic profile");
    assert_eq!(h.app.session.catalog.photo(active).unwrap().flag, lightcraft_catalog::Flag::Pick, "Classic: ⇧P flags and advances");
}

/// A user key survives the switch and owns its key alone: the profile default it displaced goes
/// quiet instead of firing beside it (Classic's R is Crop, Lightroom's R is the Radial Gradient
/// tool; give R to Survey in Classic, switch to Lightroom, and only Survey fires).
#[test]
fn a_user_key_silences_the_profile_default_displaced_by_it() {
    let mut h = demo();
    h.app.run("app.setKeymapProfile", json!({"profile": "classic"})).unwrap();
    h.app.run("app.setShortcut", json!({"id": "view.survey", "shortcut": "R"})).unwrap();
    h.app.run("app.setKeymapProfile", json!({"profile": "lightroom"})).unwrap();
    key(&mut h, "R", false);
    assert_eq!(h.app.ui.tool, "", "Lightroom's displaced R default (radial) is quiet");
    assert!(matches!(h.app.ui.view, crate::state::ViewMode::Survey), "R is the user's Survey");
}

/// The grids' Quick Collection stays on B under both profiles: the shipped profile binds the
/// brush to B and its grid branch carries the action; Classic moves the brush to K and B is
/// the album key alone (K never toggles the album).
#[test]
fn classic_grids_keep_the_quick_collection_on_b_and_the_brush_on_k() {
    let mut h = demo();
    h.app.run("app.setKeymapProfile", json!({"profile": "classic"})).unwrap();
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    key(&mut h, "B", false);
    let q = h.app.session.catalog.quick_collection().expect("quick collection");
    assert_eq!(h.app.session.catalog.album_count(q), 1, "Classic: B is the Quick Collection in the grids");
    key(&mut h, "K", false);
    assert_eq!(h.app.session.catalog.album_count(q), 1, "K (the brush) never toggles the album");
}

/// The profile selector at the top of Help ▸ Keyboard Shortcuts switches the profile both ways.
#[test]
fn the_keymap_panel_switches_profiles() {
    let mut h = demo();
    h.app.run("app.shortcuts", json!({})).unwrap();
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:keymapProfile-classic"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.settings.profile(), KeyProfile::Classic);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Shortcuts)), "the editor stays open");
    let r = h.request("ui.clickWidget", json!({"id": "button:keymapProfile-lightroom"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.settings.profile(), KeyProfile::Lightroom);
}
