//! Headless tests of the Channel Mixer's eyedropper (the neutral picker) and Swap Red and Blue.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Detail view with the Edit panel open and the Channel Mixer section expanded.
fn mixer_open() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 1100.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.app.ui.open_sections = vec!["channelMixer".into()];
    h.step();
    h.step();
    h
}

fn dev_mixer(h: &Headless) -> lightcraft_develop::ChannelMixer {
    let id = h.app.session.active().expect("an active photo");
    h.app.session.develop_of(id).unwrap_or_default().channel_mixer
}

/// The Channel Mixer's eyedropper arms the `mixerPicker` tool like W arms the White Balance
/// Selector: from Compare it opens the photo in Detail, a click on the photo runs the pick and ends
/// the tool, Escape cancels it, and a second click on the button disarms it.
#[test]
fn mixer_picker_tool_starts_and_ends_like_wb_picker() {
    let mut h = mixer_open();
    assert!(h.app.widgets.iter().any(|(w, _)| w == "icon:mixerPicker"), "the eyedropper is in the section");
    // from Compare the button opens Detail with the tool armed, like W
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    let r = h.request("ui.clickWidget", json!({"id": "icon:mixerPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.view, ViewMode::Detail, "the button leaves Compare like W");
    assert_eq!(h.app.ui.tool, "mixerPicker");
    // the click on the photo runs the pick and ends the tool
    let before = dev_mixer(&h);
    let r = h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.31, "fy": 0.27}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "the pick ends the tool");
    let after = dev_mixer(&h);
    assert_ne!(after, before, "the pick set the mixer: {after:?}");
    // Escape cancels an armed picker without picking
    h.request("engine.execute", json!({"command": "tool.mixerPicker"}), T);
    h.step();
    assert_eq!(h.app.ui.tool, "mixerPicker", "the command arms it too");
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.step();
    assert_eq!(h.app.ui.tool, "");
    assert_eq!(dev_mixer(&h), after, "Escape picks nothing");
    // a second click on the button disarms it
    let r = h.request("ui.clickWidget", json!({"id": "icon:mixerPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "mixerPicker");
    let r = h.request("ui.clickWidget", json!({"id": "icon:mixerPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "a second click disarms it");
}

/// Swap Red and Blue (the button next to the eyedropper) swaps the mixer's red and blue output rows.
#[test]
fn swap_red_and_blue_button_swaps_the_output_rows() {
    let mut h = mixer_open();
    // a mix the swap is visible on
    h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "channelMixer.red.red", "value": 120.0}}), T);
    h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "channelMixer.blue.green", "value": -40.0}}), T);
    h.settle(SETTLE);
    let before = dev_mixer(&h);
    let r = h.request("ui.clickWidget", json!({"id": "button:mixerSwapRB"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let after = dev_mixer(&h);
    assert_eq!((after.red, after.green, after.blue), (before.blue, before.green, before.red), "the output rows swap");
}

/// The ids of the Window ▸ Tools submenu.
fn tools_menu(app: &LightcraftApp) -> Vec<String> {
    let bar = crate::menubar::menu_bar(app);
    let window = bar.iter().find(|(title, _)| title == "Window").expect("Window menu").1.iter();
    let mut out = Vec::new();
    for node in window {
        if let crate::menubar::MenuNode::Submenu { label, children } = node
            && label.as_str() == crate::i18n::tr("Tools")
        {
            out.extend(children.iter().filter_map(|n| if let crate::menubar::MenuNode::Item { id, .. } = n { Some(id.clone()) } else { None }));
        }
    }
    out
}

/// The eyedropper's command sits in the Window ▸ Tools menu beside the White Balance Selector, so
/// the keyboard and the menu arm the same tool (with no shortcut of its own).
#[test]
fn mixer_picker_is_in_the_tools_menu_beside_the_wb_selector() {
    let h = mixer_open();
    let tools = tools_menu(&h.app);
    let wb = tools.iter().position(|id| id == "tool.wbPicker").expect("the WB Selector in Tools");
    let mixer = tools.iter().position(|id| id == "tool.mixerPicker").expect("the Channel Mixer Neutral in Tools");
    assert!(mixer > wb, "beside the WB Selector: {tools:?}");
}
