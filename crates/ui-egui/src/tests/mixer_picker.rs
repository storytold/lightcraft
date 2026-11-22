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

/// A pick whose source cannot be loaded toasts the load failure itself (a source gone from disk),
/// not the picker's "can't be made neutral" advice — that is only for a spot the maths refuses
/// (issue #2158 review). The refusal branch is guarded by the flip-mixer case at the end.
#[test]
fn mixer_picker_toasts_a_source_failure_as_itself_not_the_refusal() {
    let dir = std::env::temp_dir().join(format!("lc-ui-mixer-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8 { width: 16, height: 12, data: vec![[200, 120, 40, 255]; 16 * 12] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let file = dir.join("gone.png");
    std::fs::write(&file, png).unwrap();

    let services = crate::Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1400.0, 1100.0], 1.0);
    h.request("ui.set", json!({"view": "detail", "right": "edit", "openSections": ["channelMixer"]}), T);
    // import the file and wait for it to finish, so the photo is active with a rendered canvas
    crate::import::start_paths(&mut h.app, vec![file.to_string_lossy().to_string()]).unwrap();
    let t0 = std::time::Instant::now();
    while h.app.import.is_some() && t0.elapsed() < SETTLE {
        h.step();
    }
    assert!(h.app.import.is_none(), "import finished");
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "canvas:image"), "the canvas is there");

    // the source is gone and nothing holds it: the pick cannot load it
    std::fs::remove_file(&file).unwrap();
    h.app.session.media.clear_sources();
    let r = h.request("engine.execute", json!({"command": "tool.mixerPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.5, "fy": 0.5}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let toast = h.app.ui.toast.clone().expect("a toast").0;
    assert!(toast.contains("gone.png"), "the load failure names the file: {toast}");
    assert!(!toast.contains("can't be made neutral"), "not the refusal: {toast}");

    // the refusal branch still shows localized advice (not the raw error message): the flip mixer's
    // gains come out non-positive at every spot, so it answers with the mixer-level message
    // (a_mixer_no_spot_can_fix_toasts_its_own_message_not_the_pick_elsewhere_advice)
    let gone = h.app.session.active().expect("the imported photo");
    let demo_id = h.app.session.visible_cloned().into_iter().find(|id| *id != gone).expect("a demo photo");
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [demo_id.0], "active": demo_id.0}}), T);
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "canvas:image"), "the canvas is back");
    for (control, value) in
        [("channelMixer.red.red", 100.0), ("channelMixer.red.green", 200.0), ("channelMixer.green.red", -100.0), ("channelMixer.blue.blue", 100.0)]
    {
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": control, "value": value}}), T);
    }
    h.app.ui.toast = None;
    h.request("engine.execute", json!({"command": "tool.mixerPicker"}), T);
    let r = h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.5, "fy": 0.5}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let toast = h.app.ui.toast.clone().expect("a toast").0;
    assert!(toast.contains("can't be made neutral at any spot"), "the localized advice for a refused pick: {toast}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A spot the maths refuses (here a black one) toasts the advice to pick a brighter area and leaves
/// the eyedropper armed, so the next click is that other pick: before, the refusal ended the tool
/// and the photographer had to arm it again to follow the advice. A pick that works still ends it.
#[test]
fn a_refused_spot_keeps_the_eyedropper_armed_for_another_pick() {
    let dir = std::env::temp_dir().join(format!("lc-ui-mixer-rearm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // left half black (nothing to neutralize), right half a warm grey
    let (w, h_px) = (64usize, 48usize);
    let data: Vec<[u8; 4]> = (0..w * h_px).map(|i| if i % w < w / 2 { [0, 0, 0, 255] } else { [190, 150, 110, 255] }).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h_px, data };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let file = dir.join("halves.png");
    std::fs::write(&file, png).unwrap();

    let services = crate::Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
    let mut h = Headless::new(app, [1400.0, 1100.0], 1.0);
    h.request("ui.set", json!({"view": "detail", "right": "edit", "openSections": ["channelMixer"]}), T);
    crate::import::start_paths(&mut h.app, vec![file.to_string_lossy().to_string()]).unwrap();
    let t0 = std::time::Instant::now();
    while h.app.import.is_some() && t0.elapsed() < SETTLE {
        h.step();
    }
    assert!(h.app.import.is_none(), "import finished");
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "canvas:image"), "the canvas is there");

    let before = dev_mixer(&h);
    h.app.ui.toast = None;
    let r = h.request("engine.execute", json!({"command": "tool.mixerPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.2, "fy": 0.5}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let toast = h.app.ui.toast.clone().expect("a toast").0;
    assert!(toast.contains("pick a brighter"), "the black spot is refused with the advice: {toast}");
    assert_eq!(dev_mixer(&h), before, "a refused pick changes nothing");
    assert_eq!(h.app.ui.tool, "mixerPicker", "the eyedropper stays armed for the other spot");

    // the next click, on the grey half, is the pick: it sets the mixer and ends the tool
    let r = h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.8, "fy": 0.5}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_ne!(dev_mixer(&h), before, "the second click picked");
    assert_eq!(h.app.ui.tool, "", "a pick that works ends the tool");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The eyedropper and Swap Red and Blue share a wrapping row: in a narrow panel with a longer
/// translation (Finnish) the text button drops to its own line below the eyedropper instead of
/// running past the panel's edge (issue #2158 review).
#[test]
fn swap_button_wraps_to_its_own_line_in_a_narrow_panel() {
    let services = crate::Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [900.0, 1000.0], 1.0);
    let r =
        h.request("ui.set", json!({"view": "detail", "right": "edit", "language": "fi", "rightWidth": 230.0, "openSections": ["channelMixer"]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let rect = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r);
    let eye = rect("icon:mixerPicker").expect("the eyedropper");
    let swap = rect("button:mixerSwapRB").expect("the swap button");
    assert!(swap.right() <= 900.0 - 4.0, "the swap button fits the panel: {swap:?}");
    assert!(swap.top() >= eye.bottom() - 2.0, "it wrapped below the eyedropper: {eye:?} vs {swap:?}");
}

/// A mixer no spot can fix — singular rows, or the review's red-takes-green mix whose gains come
/// out non-positive everywhere — refuses with the mixer-level message instead of advising to pick
/// elsewhere (issue #2158 review).
#[test]
fn a_mixer_no_spot_can_fix_toasts_its_own_message_not_the_pick_elsewhere_advice() {
    let mut h = mixer_open();
    let set = |h: &mut Headless, control: &str, value: f64| {
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": control, "value": value}}), T);
    };
    let pick = |h: &mut Headless| {
        h.app.ui.toast = None;
        h.request("engine.execute", json!({"command": "tool.mixerPicker"}), T);
        let r = h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.5, "fy": 0.5}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.app.ui.toast.clone().expect("a toast").0
    };

    // three identical rows: singular, refused for every spot with its own message
    for (control, value) in [
        ("channelMixer.red.red", 100.0),
        ("channelMixer.green.red", 100.0),
        ("channelMixer.green.green", 0.0),
        ("channelMixer.blue.red", 100.0),
        ("channelMixer.blue.green", 0.0),
        ("channelMixer.blue.blue", 0.0),
    ] {
        set(&mut h, control, value);
    }
    let toast = pick(&mut h);
    assert!(toast.contains("can't be made neutral at any spot"), "the mixer-level cause is named: {toast}");
    assert!(!toast.contains("pick a brighter"), "not the pick-elsewhere advice: {toast}");

    // the review's example: Red Output takes Red +100 and Green +100 — invertible, but the gains'
    // direction has a zero part, so every spot is refused the same way
    for (control, value) in [
        ("channelMixer.red.red", 100.0),
        ("channelMixer.red.green", 100.0),
        ("channelMixer.green.red", 0.0),
        ("channelMixer.green.green", 100.0),
        ("channelMixer.blue.blue", 100.0),
    ] {
        set(&mut h, control, value);
    }
    let toast = pick(&mut h);
    assert!(toast.contains("can't be made neutral at any spot"), "the mixer-level message again: {toast}");
    assert!(!toast.contains("pick a brighter"), "not the pick-elsewhere advice: {toast}");
}
