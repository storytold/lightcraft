//! The Export dialog's dropdowns by keyboard: Enter on a menu item chooses it, closes the menu
//! and gives the focus back to the dropdown, and screen readers hear whether its menu is open.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_engine::export::{ExportFormat, SharpenFor};
use lightcraft_ui_egui::{LightcraftApp, Services, state::Dialog};

fn export_dialog() -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    // a headless host never gains the first-run tour: its modal would take the focus under test
    app.headless_host = true;
    let mut builder = Harness::builder().with_size(vec2(1500.0, 1100.0));
    if lightcraft_engine::CRAFT_FONTS.is_empty() {
        builder = builder.allow_missing_glyphs();
    }
    let mut h = builder.build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(4);
    h.state_mut().run("dialog.export", serde_json::json!({})).expect("the Export dialog opens");
    h.run_steps(6);
    h
}

/// The dropdown named `name`: whether its menu is open, and whether it has the focus.
fn dropdown(h: &Harness<'_, LightcraftApp>, name: &str) -> (bool, bool) {
    let n = h
        .root()
        .children_recursive()
        .find(|n| n.accesskit_node().role() == Role::ComboBox && n.accesskit_node().label().as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no dropdown {name}"));
    (n.accesskit_node().data().is_expanded() == Some(true), n.is_focused())
}

/// Whether a node other than a dropdown is named `name` (an item of an open menu).
fn item_shows(h: &Harness<'_, LightcraftApp>, name: &str) -> bool {
    h.root().children_recursive().any(|n| n.accesskit_node().role() != Role::ComboBox && n.accesskit_node().label().as_deref() == Some(name))
}

/// Focus the dropdown `name`, open it with Enter, focus its item `item` and choose it with Enter.
fn choose_by_keyboard(h: &mut Harness<'_, LightcraftApp>, name: &str, item: &str) {
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().role() == Role::ComboBox && n.accesskit_node().label().as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no dropdown {name}"))
        .focus();
    h.run_steps(2);
    assert_eq!(dropdown(h, name), (false, true), "{name}: closed and focused");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(dropdown(h, name).0, "{name}: screen readers hear the open menu as expanded");
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().role() != Role::ComboBox && n.accesskit_node().label().as_deref() == Some(item))
        .unwrap_or_else(|| panic!("{name}: no menu item {item}"))
        .focus();
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert_eq!(dropdown(h, name), (false, true), "{name}: Enter on {item} closes the menu and the focus is back on the dropdown");
    assert!(!item_shows(h, item), "{name}: the menu is gone");
}

fn options(h: &Harness<'_, LightcraftApp>) -> lightcraft_engine::export::ExportOptions {
    match &h.state().ui.dialog {
        Some(Dialog::Export { opts, .. }) => opts.clone(),
        _ => panic!("the Export dialog is open"),
    }
}

/// Format (a borderless dropdown) and Sharpen (a boxed one) by keyboard alone.
#[test]
fn export_dropdowns_close_on_a_keyboard_choice() {
    let mut h = export_dialog();
    choose_by_keyboard(&mut h, "Format", "TIFF");
    assert_eq!(options(&h).format, ExportFormat::Tiff);
    choose_by_keyboard(&mut h, "Sharpen", "Matte");
    assert_eq!(options(&h).sharpen, SharpenFor::Matte);
}
