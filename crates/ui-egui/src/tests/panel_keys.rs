//! Headless tests of Shift+Tab (View ▸ Hide All Panels), which hides the side panels, the Presets
//! column and the filmstrip and brings back exactly what it hid, and of how it gets on with F8.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::menus::menubar::{MenuNode, checked, menu_bar};
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
    assert_eq!(r["result"]["open"], false, "{r}");
    assert_eq!(h.app.ui.right, RightPanel::None);
    run(&mut h, "view.rightPanel", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::Keywords);

    let r = run(&mut h, "view.allPanels", json!({}));
    assert_eq!(r["result"]["hidden"], true, "{r}");
    assert_eq!(shown(&h), (false, RightPanel::None, false, false));
    let r = run(&mut h, "view.allPanels", json!({"hide": true}));
    assert_eq!(r["result"]["hidden"], true, "forced hide says the panels are hidden: {r}");
    assert_eq!(shown(&h), (false, RightPanel::None, false, false), "hiding hidden panels keeps what was remembered");
    let e = h.request("ui.menu.invoke", json!({"id": "view.allPanels", "params": {"hide": "yes"}}), T);
    assert_eq!(e["ok"], false, "a `hide` that isn't true or false is refused: {e}");
    assert_eq!(shown(&h), (false, RightPanel::None, false, false), "and changes nothing");
    let r = run(&mut h, "view.allPanels", json!({}));
    assert_eq!(r["result"]["hidden"], false, "{r}");
    assert_eq!(shown(&h), (true, RightPanel::Keywords, false, true));
}

#[test]
fn hiding_the_panels_ends_the_tool_in_use() {
    let mut h = loupe();
    run(&mut h, "panel.remove", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    assert!(!h.app.ui.tool.is_empty(), "Remove's tool is in use");
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::None);
    assert!(h.app.ui.tool.is_empty(), "F8 ends the tool");
    run(&mut h, "panel.remove", json!({}));
    assert!(!h.app.ui.tool.is_empty(), "Remove's tool is in use again");
    run(&mut h, "view.allPanels", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::None);
    assert!(h.app.ui.tool.is_empty(), "hiding the panels ends the tool");
    run(&mut h, "view.allPanels", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::Remove, "the panel comes back, as with F8");
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
    let (label, _, on) = item(&app, "view.rightPanel").expect("View ▸ Right Panel");
    assert_eq!((label.as_str(), on), ("Right Panel", Some(true)));
    let (label, _, on) = item(&app, "view.allPanels").expect("View ▸ Hide All Panels");
    assert_eq!((label.as_str(), on), ("Hide All Panels", Some(false)));
    app.run("view.allPanels", json!({})).unwrap();
    assert_eq!(checked(&app, "view.rightPanel"), Some(false));
    assert_eq!(checked(&app, "view.allPanels"), Some(true));
    assert_eq!(checked(&app, "view.leftPanel"), Some(false));
    app.run("view.rightPanel", json!({})).unwrap();
    assert_eq!(checked(&app, "view.rightPanel"), Some(true));
    assert_eq!(checked(&app, "view.allPanels"), Some(false), "a panel is shown again, so the panels are no longer all hidden");
}

/// Shift+Tab is a fixed key like F8: a saved keymap that gives Shift+Tab to another command keeps
/// it, and the panels stay as they are.
#[test]
fn a_saved_assignment_of_shift_tab_wins() {
    assert!(crate::shortcuts::ALIASES.iter().any(|(sc, id, _)| (*sc, *id) == ("Shift+Tab", "view.allPanels")));
    let mut h = loupe();
    arrange(&mut h, true, RightPanel::Edit, false, true);
    h.app.ui.settings.keymap.insert("view.photoGrid".into(), "Shift+Tab".into());
    key(&mut h, "Tab", true);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "the saved assignment ran");
    assert_eq!(shown(&h), (true, RightPanel::Edit, false, true), "and the panels were left alone");
    assert!(h.app.ui.hidden_panels.is_none());
}

/// Bringing the panels back keeps the view: an Edit panel shown over the grid comes back over the
/// grid (F8 still opens an edit tool in the loupe).
#[test]
fn shift_tab_keeps_the_view_when_it_brings_the_panels_back() {
    let mut h = loupe();
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    arrange(&mut h, true, RightPanel::Edit, false, true);
    key(&mut h, "Tab", true);
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (true, RightPanel::Edit, false, true));
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "the grid stays");
}

/// A saved Shift+Tab binding of Hide All Panels itself leaves keyboard focus alone too.
#[test]
fn a_saved_shift_tab_for_hide_all_panels_leaves_focus_alone() {
    let mut h = loupe();
    arrange(&mut h, true, RightPanel::Edit, false, true);
    h.app.ui.settings.keymap.insert("view.allPanels".into(), "Shift+Tab".into());
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (false, RightPanel::None, false, false), "the binding hides the panels once");
    assert_eq!(h.view.ctx.memory(|m| m.focused()), None, "and moves no focus");
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (true, RightPanel::Edit, false, true));
    assert_eq!(h.view.ctx.memory(|m| m.focused()), None);
}

/// Showing a panel and closing it again forgets what Hide All Panels hid: the next Shift+Tab shows
/// the usual panels instead of the old arrangement.
#[test]
fn a_panel_shown_and_closed_again_forgets_the_hidden_arrangement() {
    let mut h = loupe();
    arrange(&mut h, false, RightPanel::Keywords, true, false);
    key(&mut h, "Tab", true);
    assert!(h.app.ui.all_panels_hidden());
    run(&mut h, "view.leftPanel", json!({}));
    run(&mut h, "view.leftPanel", json!({}));
    assert!(h.app.ui.hidden_panels.is_none(), "the snapshot went when the sidebar showed");
    key(&mut h, "Tab", true);
    assert_eq!(shown(&h), (true, RightPanel::Keywords, false, true), "the usual panels, with the right panel F8 remembers");
}

/// The same within one batch of commands, with no frame between showing and closing the panel.
#[test]
fn a_panel_shown_and_closed_within_one_batch_forgets_the_arrangement_too() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.ui.left_panel = false;
    app.ui.right = RightPanel::Keywords;
    app.ui.presets = true;
    app.ui.filmstrip = false;
    app.run("view.allPanels", json!({})).unwrap();
    assert!(app.ui.all_panels_hidden());
    app.run("view.filmstrip", json!({})).unwrap();
    app.run("view.filmstrip", json!({})).unwrap();
    assert!(app.ui.hidden_panels.is_none(), "showing the filmstrip dropped the snapshot");
}
