//! The module shell's steps of [`DacApp::ui`] and [`DacApp::raw_input_hook`], kept out of the
//! shared `lib.rs` (U.5): the window's edges (top bar, module bar, strip and right column, left
//! panel, toolbar) as the module asks for them, and Tab held back from focus navigation.

use crate::{DacApp, module, panels, state};

/// The edges, in order (earlier panels take the full edge); returns the module, which draws the
/// centre.
pub(crate) fn edges(app: &mut DacApp, ui: &mut egui::Ui, ctx: &egui::Context) -> &'static dyn module::Module {
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
        panels::strip::show(app, ui);
        // Library's right column is its Classic panels, unless a Library panel of the strip
        // (Info, Keywords, Versions, Activity) was opened in its place
        if app.ui.module == module::ModuleId::Library && app.ui.right == state::RightPanel::None {
            panels::classic::right_column(app, ui);
        } else if app.ui.right != state::RightPanel::None {
            panels::right::show(app, ui);
        }
        if app.ui.presets {
            panels::presets::show(app, ui);
        }
    }
    if module::edge_visible(app, module::Edge::Left) && !m.own_sides() {
        panels::left::show(app, ui);
    }
    if app.ui.toolbar && app.ui.screen_mode != module::ScreenMode::FullScreenHidePanels {
        m.toolbar(ui, app);
    }
    m
}

/// Tab outside a text field toggles panels (Classic): keep it from moving egui's focus.
pub(crate) fn defer_tabs(app: &mut DacApp, raw: &mut egui::RawInput) {
    if app.text_focus || app.recording_shortcut.is_some() {
        return;
    }
    let deferred = &mut app.deferred_tabs;
    raw.events.retain(|e| match e {
        egui::Event::Key { key: egui::Key::Tab, pressed, modifiers, .. } => {
            if *pressed {
                deferred.push(*modifiers);
            }
            false
        }
        _ => true,
    });
}
