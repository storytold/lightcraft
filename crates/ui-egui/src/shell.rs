//! The module shell's steps of [`LccApp::ui`] and [`LccApp::raw_input_hook`], kept out of the
//! shared `lib.rs` (U.5): the window's edges (top bar, module bar, strip and right column, left
//! panel, toolbar) as the module asks for them, and Tab held back from focus navigation.

use crate::{LccApp, module, panels, state};

/// The edges, in order (earlier panels take the full edge); returns the module, which draws the
/// centre.
pub(crate) fn edges(app: &mut LccApp, ui: &mut egui::Ui, ctx: &egui::Context) -> &'static dyn module::Module {
    module::sync(app);
    module::auto_show(app, ctx);
    let m = module::get(app.ui.module);
    if app.ui.screen_mode != module::ScreenMode::FullScreen && app.ui.screen_mode != module::ScreenMode::FullScreenHidePanels {
        panels::topbar::show(app, ui);
    }
    if module::edge_visible(app, module::Edge::Top) {
        module::module_bar(app, ui);
    }
    panels::library_problem::banner(app, ui);
    if module::edge_visible(app, module::Edge::Right) && !m.own_sides() {
        // The tool strip: each module declares its own panels (`strip_declared`) — Develop its
        // editing tools, Library its Versions and History & Activity. A module with no strip of its
        // own (Map, Book, …) draws none.
        if !panels::strip::strip_declared(app.ui.module).is_empty() {
            panels::strip::show(app, ui);
        }
        // Library's right column is its Classic panels, unless one of the strip's panels (Versions,
        // History & Activity) is open in its place
        if app.ui.module == module::ModuleId::Library && app.ui.right == state::RightPanel::None {
            panels::classic::right_column(app, ui);
        } else if app.ui.right != state::RightPanel::None {
            right(app, ui);
        }
        if app.ui.presets {
            let t = crate::theme::Tokens::get(ui.ctx());
            egui::Panel::right("presets_panel")
                .exact_size(t.panel_w)
                .resizable(false)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| panels::presets::show(app, ui, module::DEVELOP_LEFT, false));
        }
    }
    if module::edge_visible(app, module::Edge::Left) && !m.own_sides() {
        left(app, ui);
    }
    if app.ui.toolbar && app.ui.screen_mode != module::ScreenMode::FullScreenHidePanels {
        m.toolbar(ui, app);
    }
    m
}

/// The shared panels draw only their contents (upstream's docking hosts them); the Classic edges give
/// them their resizable sides.
fn right(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    // the presets column and the left sidebar are laid out after this panel: leave them their room
    let reserve = if app.ui.presets { t.panel_w } else { 0.0 } + if app.ui.left_panel { state::LEFT_WIDTH.min } else { 0.0 };
    let width = app.ui.right_width;
    let resized =
        panels::resizable_side(ui, false, "right_panel", egui::Frame::NONE, width, state::RIGHT_WIDTH, reserve, |ui| panels::right::show(app, ui));
    if let Some(w) = resized {
        app.ui.right_width = w;
    }
}

fn left(app: &mut LccApp, ui: &mut egui::Ui) {
    let width = app.ui.left_width;
    let resized = panels::resizable_side(ui, true, "left_panel", egui::Frame::NONE, width, state::LEFT_WIDTH, 0.0, |ui| panels::left::show(app, ui));
    if let Some(w) = resized {
        app.ui.left_width = w;
    }
}

/// Tab outside a text field toggles panels (Classic): keep it from moving egui's focus. A Tab
/// the keymap gives no command (the Alternative set's) moves the keyboard focus, as usual.
pub(crate) fn defer_tabs(app: &mut LccApp, raw: &mut egui::RawInput) {
    if app.text_focus || app.recording_shortcut.is_some() {
        return;
    }
    crate::shortcuts::set_active(app.ui.settings.keymap_set);
    let keymap = &app.ui.settings.keymap;
    let deferred = &mut app.deferred_tabs;
    raw.events.retain(|e| match e {
        egui::Event::Key { key: egui::Key::Tab, pressed, modifiers, .. } if crate::shortcuts::tab_bound(keymap, *modifiers) => {
            if *pressed {
                deferred.push(*modifiers);
            }
            false
        }
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use crate::shortcuts::KeymapSet;
    use crate::{LccApp, Services};

    fn tab(shift: bool) -> egui::Event {
        let modifiers = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
        egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// The Classic set's Tab hides the side panels and does not move the focus; in the
    /// Alternative set, where no command has Tab, Tab moves the keyboard focus.
    #[test]
    fn only_a_tab_the_keymap_binds_is_held_back_from_focus_navigation() {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut raw = egui::RawInput { events: vec![tab(false), tab(true)], ..Default::default() };
        super::defer_tabs(&mut app, &mut raw);
        assert!(raw.events.is_empty() && app.deferred_tabs.len() == 2, "Classic: Tab and Shift+Tab are commands");
        app.deferred_tabs.clear();
        app.ui.settings.keymap_set = KeymapSet::Alternative;
        let mut raw = egui::RawInput { events: vec![tab(false), tab(true)], ..Default::default() };
        super::defer_tabs(&mut app, &mut raw);
        assert_eq!(raw.events.len(), 2, "Alternative: Tab moves the focus");
        assert!(app.deferred_tabs.is_empty());
        crate::shortcuts::set_active(KeymapSet::Classic);
    }
}
