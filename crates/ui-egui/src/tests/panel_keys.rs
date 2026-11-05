//! Headless tests of the panel keys: F8 (Right Panel) and Shift+Tab (Hide All Panels), which hides
//! the side panels, the Presets column and the filmstrip, and brings back exactly what it hid.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::menubar::{MenuNode, checked, menu_bar};
use crate::state::RightPanel;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn loupe() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
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

fn run(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("ui.menu.invoke", json!({"id": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    r
}

/// (left panel, right panel, presets, filmstrip)
fn shown(h: &Headless) -> (bool, RightPanel, bool, bool) {
    (h.app.ui.left_panel, h.app.ui.right, h.app.ui.presets, h.app.ui.filmstrip)
}

fn arrange(h: &mut Headless, left: bool, right: RightPanel, presets: bool, filmstrip: bool) {
    h.app.ui.left_panel = left;
    h.app.ui.right = right;
    h.app.ui.presets = presets;
    h.app.ui.filmstrip = filmstrip;
    h.step();
    h.step();
}

#[test]
fn f8_hides_the_right_panel_and_brings_back_the_tool_that_was_there() {
    let mut h = loupe();
    for tool in [RightPanel::Edit, RightPanel::Keywords, RightPanel::Info, RightPanel::Versions, RightPanel::Activity, RightPanel::Profiles] {
        arrange(&mut h, true, tool, false, true);
        key(&mut h, "F8", false);
        assert_eq!(shown(&h), (true, RightPanel::None, false, true), "F8 hides {tool:?} and nothing else");
        key(&mut h, "F8", false);
        assert_eq!(shown(&h), (true, tool, false, true), "F8 shows {tool:?} again");
    }
    // the tools that act on the photo end when hidden and come back as Edit
    arrange(&mut h, true, RightPanel::Crop, false, true);
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::None);
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::Edit);
    // closed some other way, with nothing remembered: Edit
    let mut h = loupe();
    arrange(&mut h, true, RightPanel::None, false, true);
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::Edit);
}

#[test]
fn shift_tab_hides_all_panels_and_again_brings_back_exactly_those() {
    let mut h = loupe();
    arrange(&mut h, true, RightPanel::Keywords, true, true);
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (false, RightPanel::None, false, false));
    assert!(h.app.ui.all_panels_hidden());
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (true, RightPanel::Keywords, true, true));
    assert!(!h.app.ui.all_panels_hidden());
    // a panel that was already hidden stays hidden
    arrange(&mut h, false, RightPanel::Info, false, true);
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (false, RightPanel::None, false, false));
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (false, RightPanel::Info, false, true));
    arrange(&mut h, true, RightPanel::None, false, false);
    key(&mut h, "Tab", true);
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (true, RightPanel::None, false, false), "a hidden right panel stays hidden too");
    // F8 afterwards still finds the tool it had
    arrange(&mut h, true, RightPanel::Versions, false, true);
    key(&mut h, "Tab", true);
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::Versions);
}

#[test]
fn hide_all_with_nothing_shown_and_nothing_remembered_shows_the_usual_panels() {
    let mut h = loupe();
    arrange(&mut h, false, RightPanel::None, false, false);
    assert!(h.app.ui.hidden_panels.is_none());
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (true, RightPanel::Edit, false, true));
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (false, RightPanel::None, false, false));
}

#[test]
fn showing_one_panel_while_all_are_hidden_makes_the_next_shift_tab_hide_again() {
    for show in ["view.leftPanel", "view.filmstrip", "view.rightPanel", "panel.presets", "panel.info"] {
        let mut h = loupe();
        arrange(&mut h, true, RightPanel::Edit, false, true);
        key(&mut h, "Tab", true);
        assert!(h.app.ui.all_panels_hidden());
        run(&mut h, show, json!({}));
        assert!(!h.app.ui.all_panels_hidden(), "{show} shows a panel");
        let one = shown(&h);
        key(&mut h, "Tab", true);
        assert_eq!(
            shown(&h),
            (false, RightPanel::None, false, false),
            "{show}: Shift+Tab hides what is shown ({one:?}), it doesn't resurrect the old state"
        );
        key(&mut h, "Tab", true);
        assert_eq!(shown(&h), one, "{show}: and the next one brings back what was shown");
    }
}

#[test]
fn shift_tab_leaves_keyboard_focus_alone() {
    let mut h = loupe();
    arrange(&mut h, true, RightPanel::Edit, false, true);
    assert_eq!(h.view.ctx.memory(|m| m.focused()), None, "nothing has focus to begin with");
    key(&mut h, "Tab", true);
    assert_eq!(h.view.ctx.memory(|m| m.focused()), None, "Shift+Tab doesn't focus the last widget");
    assert!(!h.view.ctx.egui_wants_keyboard_input());
    key(&mut h, "Tab", true);
    assert_eq!(h.view.ctx.memory(|m| m.focused()), None);
    assert!(!h.view.ctx.egui_wants_keyboard_input());
    assert_eq!(shown(&h), (true, RightPanel::Edit, false, true));
}

#[test]
fn panel_keys_belong_to_a_focused_text_field() {
    let mut h = loupe();
    arrange(&mut h, true, RightPanel::Info, false, true);
    let r = h.request("ui.clickWidget", json!({"id": "field:search"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the search field has the keyboard");
    let field = h.view.ctx.memory(|m| m.focused());
    key(&mut h, "F8", false);
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (true, RightPanel::Info, false, true), "F8 and Shift+Tab are typed into, not shortcuts");
    assert_ne!(h.view.ctx.memory(|m| m.focused()), field, "Shift+Tab still moves focus out of the field");
    // with the field left, the keys work again
    key(&mut h, "Escape", false);
    h.view.ctx.memory_mut(|m| m.stop_text_input());
    h.step();
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::None);
}

#[test]
fn the_panel_commands_work_without_the_keys() {
    let mut h = loupe();
    arrange(&mut h, true, RightPanel::Keywords, false, true);
    let r = run(&mut h, "view.rightPanel", json!({}));
    assert_eq!(r["result"]["shown"], false, "{r}");
    assert_eq!(h.app.ui.right, RightPanel::None);
    // `show` says which way, whatever the state
    run(&mut h, "view.rightPanel", json!({"show": false}));
    assert_eq!(h.app.ui.right, RightPanel::None);
    run(&mut h, "view.rightPanel", json!({"show": true}));
    assert_eq!(h.app.ui.right, RightPanel::Keywords);
    run(&mut h, "view.rightPanel", json!({"show": true}));
    assert_eq!(h.app.ui.right, RightPanel::Keywords, "showing a shown panel keeps it");

    let r = run(&mut h, "view.allPanels", json!({}));
    assert_eq!(r["result"]["hidden"], true, "{r}");
    assert_eq!(shown(&h), (false, RightPanel::None, false, false));
    run(&mut h, "view.allPanels", json!({"hide": true}));
    assert_eq!(shown(&h), (false, RightPanel::None, false, false), "hiding hidden panels keeps what was remembered");
    let r = run(&mut h, "view.allPanels", json!({}));
    assert_eq!(r["result"]["hidden"], false, "{r}");
    assert_eq!(shown(&h), (true, RightPanel::Keywords, false, true));
}

#[test]
fn hiding_the_panels_ends_the_tool_in_use() {
    let mut h = loupe();
    run(&mut h, "panel.crop", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::None);
    assert!(h.app.ui.tool.is_empty());
    run(&mut h, "panel.masking", json!({}));
    run(&mut h, "view.allPanels", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::None);
    run(&mut h, "view.allPanels", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::Edit);
}

#[test]
fn the_view_menu_lists_the_panel_commands_with_their_state() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let item = |app: &LightcraftApp, id: &str| -> Option<(String, Option<String>, Option<bool>)> {
        fn find(nodes: &[MenuNode], id: &str) -> Option<(String, Option<String>, Option<bool>)> {
            nodes.iter().find_map(|n| match n {
                MenuNode::Item { id: i, label, shortcut, checked, .. } if i == id => Some((label.clone(), shortcut.clone(), *checked)),
                MenuNode::Submenu { children, .. } => find(children, id),
                _ => None,
            })
        }
        let bar = menu_bar(app);
        find(&bar.iter().find(|(t, _)| t == "View")?.1, id)
    };
    app.ui.left_panel = true;
    app.ui.right = RightPanel::Edit;
    app.ui.filmstrip = true;
    let (label, shortcut, on) = item(&app, "view.rightPanel").expect("View ▸ Right Panel");
    assert_eq!((label.as_str(), shortcut.as_deref(), on), ("Right Panel", Some("F8"), Some(true)));
    let (label, shortcut, on) = item(&app, "view.allPanels").expect("View ▸ Hide All Panels");
    assert_eq!((label.as_str(), shortcut.as_deref(), on), ("Hide All Panels", Some("Shift+Tab"), Some(false)));
    app.run("view.allPanels", json!({})).unwrap();
    assert_eq!(checked(&app, "view.rightPanel"), Some(false));
    assert_eq!(checked(&app, "view.allPanels"), Some(true));
    assert_eq!(checked(&app, "view.leftPanel"), Some(false));
    app.run("view.rightPanel", json!({})).unwrap();
    assert_eq!(checked(&app, "view.rightPanel"), Some(true));
    assert_eq!(checked(&app, "view.allPanels"), Some(false), "a panel is shown again, so the panels are no longer all hidden");
}

#[test]
fn the_keys_are_in_the_keymap_and_f1_opens_help() {
    use crate::shortcuts::{parse, shortcut_of};
    let keymap = Default::default();
    assert_eq!(shortcut_of(&keymap, "view.rightPanel"), Some("F8"));
    assert_eq!(shortcut_of(&keymap, "view.allPanels"), Some("Shift+Tab"));
    assert!(parse("F8").is_some() && parse("Shift+Tab").is_some());
    // F1 (LightCraft Help) goes to the docs
    let mut h = loupe();
    let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let o = opened.clone();
    h.app.services.open_url = Some(Box::new(move |u: &str| {
        o.lock().unwrap().push(u.to_string());
        Ok(())
    }));
    key(&mut h, "F1", false);
    assert_eq!(opened.lock().unwrap().as_slice(), [crate::links::HELP]);
}
