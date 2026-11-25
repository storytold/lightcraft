//! Framework-owned window geometry over LightCraft's live regions.
//!
//! The shell owns the title/search bar, tool rail and remaining workspace rectangle. The
//! existing docking workspace is still the only saved panel arrangement; its callbacks own
//! the Library, Tools, Presets and photo bodies, including detail rendering and filmstrip.

use craft::ui::contract::Bind;
use craft::ui::runtime::env::{RootEnv, Scale, WindowKey};
use craft::ui::runtime::frame::{FrameInput, set_input};
use craft::ui::shell::{AppShell, ScreenMode, Slot};
use std::sync::{Arc, LazyLock};

use super::LightcraftApp;

/// Sizes describe existing chrome, not a second persisted layout. Keep one Arc for the SDK's
/// token cache. Dock widths and resize gestures remain in `UiState::docking`.
static SIZES: LazyLock<Arc<craft::theme::AppSizes>> =
    LazyLock::new(|| Arc::new(craft::theme::AppSizes::parse_or_default(br#"{ "all": { "size.bar.title": 42, "size.rail.width": 48 } }"#)));


/// One current derived size set per context. Changes replace it rather than accumulating
/// width/theme/scale entries, while an unchanged extent retains the SDK token-cache identity.
fn shell_sizes(ctx: &egui::Context) -> Arc<craft::theme::AppSizes> {
    let extent = crate::panels::strip::shell_extent(ctx);
    let key = egui::Id::unique("lightcraft-shell-sizes");
    ctx.data_mut(|data| {
        if let Some((previous, sizes)) = data.get_temp::<(f32, Arc<craft::theme::AppSizes>)>(key)
            && previous == extent
        {
            return sizes;
        }
        let mut sizes = SIZES.as_ref().clone();
        sizes.all.insert("size.rail.width".into(), craft::theme::SizeValue::Number(extent));
        let sizes = Arc::new(sizes);
        data.insert_temp(key, (extent, Arc::clone(&sizes)));
        sizes
    })
}

/// Supply presentation values before the runtime's pass hooks. LightCraft retains its own
/// shortcut dispatcher and panel visibility commands; the framework must not add a Tab toggle.
pub(super) fn prepare(app: &LightcraftApp, ctx: &egui::Context) {
    let kind = crate::theme::current(ctx).unwrap_or_else(|| app.ui.settings.theme(app.system_theme(ctx)));
    // The framework's controls, docks and dialogs in LightCraft's own colours (`theme::framework_theme`).
    let mut root = match crate::theme::framework_theme(kind) {
        Some(theme) => RootEnv::new().library_theme(theme),
        None => RootEnv::new(),
    };
    root.sizes = shell_sizes(ctx);
    root.mode = if kind.is_dark() { craft::theme::ModeChoice::Dark } else { craft::theme::ModeChoice::Light };
    root.scale = Scale::new(ctx.zoom_factor());
    root.tab_toggles_chrome = false;
    let mut input = FrameInput::new(root).commands(crate::shortcuts::framework_commands(&app.ui.settings.keymap));
    // The framework's own words (menus, tooltips, dialog titles, table headers) in LightCraft's language, for the
    // whole window; English keeps the framework's own.
    if let Some(host) = crate::i18n::framework_host(app.ui.language) {
        input = input.host(host);
    }
    set_input(ctx, input);
}

pub(super) fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    // The window's shell is never content to measure: a caller's measuring run (a test that measures the whole UI)
    // draws nothing here, so no region registers or claims its id twice in the pass.
    if craft::ui::runtime::measure::active(ui.ctx()) {
        return;
    }
    // Fullscreen preview and its reveal rules remain LightCraft's existing presentation path.
    // A given Standard mode prevents a separate framework-owned screen-mode preference.
    // The migrated shell owns placement, not the spacing of still-unported controls.
    // Reservations use intrinsic frame/content metrics. Running legacy panels to measure would
    // register phantom hits and handle real input twice, at different origins.
    let host_style = ui.style().clone();
    AppShell::new(WindowKey::main())
        .screen_mode(Bind::given(ScreenMode::Standard))
        .allow_reveal(false)
        .edge_reveal(false)
        .document_tabs(false)
        .body_label(crate::i18n::tr("Photos"))
        .title_bar(|ui, _cx, app| {
            if craft::ui::runtime::measure::active(ui.ctx()) {
                return;
            }
            ui.set_style(host_style.clone());
            crate::panels::topbar::show(app, ui);
        })
        .end(Slot::new("lightcraft.tools").label(crate::i18n::tr("Tool strip")), |ui, cx, app| {
            if craft::ui::runtime::measure::active(ui.ctx()) {
                return;
            }
            ui.set_style(host_style.clone());
            crate::panels::strip::show_in_shell(app, ui, cx.env.direction().is_rtl());
        })
        .body(|ui, _cx, app| {
            if craft::ui::runtime::measure::active(ui.ctx()) {
                return;
            }
            ui.set_style(host_style.clone());
            crate::panels::library_problem::banner(app, ui);
            let chrome = crate::theme::Tokens::get(ui.ctx()).chrome;
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(chrome)).show(ui, |ui| crate::docking::show(app, ui));
        })
        .show(ui, app);
}

#[cfg(test)]
mod intrinsic_shell_tests {
    use super::*;
    use crate::headless::HeadlessView;

    fn capture(width: f32, scale: f32, zoom: f32, legacy: bool) -> (egui::Rect, Vec<(String, egui::Rect)>) {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.ui.view = crate::state::ViewMode::Detail;
        app.ui.left_panel = true;
        app.ui.right = crate::state::RightPanel::Edit;
        let mut view = HeadlessView::new();
        view.ctx.set_zoom_factor(zoom);
        let mut widgets = Vec::new();
        for frame in 0..4 {
            app.install_framework(&view.ctx);
            let raw = HeadlessView::raw_input(egui::vec2(width, 1000.0) * zoom, scale, frame as f64 / 60.0, vec![]);
            view.run(raw, |ui| {
                if legacy {
                    crate::panels::topbar::show(&mut app, ui);
                    crate::panels::library_problem::banner(&mut app, ui);
                    crate::panels::strip::show(&mut app, ui);
                    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| crate::docking::show(&mut app, ui));
                } else {
                    show(&mut app, ui);
                }
            });
            widgets = crate::widgets::take_registry(&view.ctx);
        }
        (app.canvas_rect.expect("the empty detail view still allocates its canvas"), widgets)
    }

    /// This fails with the previous shell (same root, different reserved body), independently
    /// of the source-level intrinsic formula. It compares the actual old and new drawing paths.
    #[test]
    fn shell_matches_actual_legacy_canvas_and_rail_rectangles() {
        const RAIL: &[&str] = &["icon:presets", "icon:edit", "icon:crop", "icon:remove", "icon:masking", "icon:redeye",
            "icon:versions", "icon:activity", "icon:more", "icon:keywords", "icon:info"];
        for width in [1025.0, 1400.0] {
            for scale in [1.0, 1.25] {
                for zoom in [1.0, 1.25] {
                    let (legacy_canvas, legacy_widgets) = capture(width, scale, zoom, true);
                    let (shell_canvas, shell_widgets) = capture(width, scale, zoom, false);
                    assert_eq!(shell_canvas, legacy_canvas, "width={width} scale={scale} zoom={zoom}");
                    for id in RAIL {
                        let rect = |ws: &[(String, egui::Rect)]| ws.iter().find(|(name, _)| name == id).map(|(_, r)| *r).expect("rail button");
                        assert_eq!(rect(&shell_widgets), rect(&legacy_widgets), "{id} width={width} scale={scale} zoom={zoom}");
                    }
                }
            }
        }
    }

    #[test]
    fn unchanged_intrinsic_extent_reuses_one_size_identity() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let a = shell_sizes(&ctx);
        let b = shell_sizes(&ctx);
        assert!(Arc::ptr_eq(&a, &b));
    }
    fn capture_side(width: f32, scale: f32, zoom: f32, rtl: bool, adapted: bool)
        -> (egui::Rect, Vec<(String, egui::Rect, egui::Rect)>)
    {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        let mut view = HeadlessView::new();
        view.ctx.set_zoom_factor(zoom);
        let mut body = egui::Rect::NOTHING;
        let mut hits = Vec::new();
        for frame in 0..4 {
            app.install_framework(&view.ctx);
            let mut root = RootEnv::new();
            root.sizes = if adapted { shell_sizes(&view.ctx) } else { Arc::clone(&SIZES) };
            root.scale = Scale::new(zoom);
            root.direction = if rtl { craft::i18n::Direction::Rtl } else { craft::i18n::Direction::Ltr };
            set_input(&view.ctx, FrameInput::new(root));
            let raw = HeadlessView::raw_input(egui::vec2(width, 768.0) * zoom, scale, frame as f64 / 60.0, vec![]);
            view.run(raw, |ui| {
                let host_style = ui.style().clone();
                AppShell::new(WindowKey::main())
                    .screen_mode(Bind::given(ScreenMode::Standard))
                    .allow_reveal(false).edge_reveal(false).document_tabs(false)
                    .title_bar(|ui, _cx, app| {
                        if craft::ui::runtime::measure::active(ui.ctx()) { return; }
                        ui.set_style(host_style.clone());
                        crate::panels::topbar::show(app, ui);
                    })
                    .end(Slot::new("lightcraft.tools"), |ui, cx, app| {
                        if craft::ui::runtime::measure::active(ui.ctx()) { return; }
                        ui.set_style(host_style.clone());
                        if adapted {
                            crate::panels::strip::show_in_shell(app, ui, cx.env.direction().is_rtl());
                        } else {
                            crate::panels::strip::show(app, ui);
                        }
                    })
                    .body(|ui, cx, _app| {
                        if craft::ui::runtime::measure::active(ui.ctx()) { return; }
                        body = cx.shell().expect("shell body").regions().body;
                    }).show(ui, &mut app);
            });
            let (rects, registered) = crate::widgets::take_registries(&view.ctx);
            hits = registered.iter().filter(|(id, _)| matches!(id.as_str(),
                "icon:presets" | "icon:edit" | "icon:crop" | "icon:remove" | "icon:masking" |
                "icon:redeye" | "icon:versions" | "icon:activity" | "icon:more" | "icon:keywords" | "icon:info"))
                .map(|(id, hit)| (id.clone(), rects.iter().find(|(name, _)| name == id).expect("registered rect").1, hit.interact_rect))
                .collect();
        }
        (body, hits)
    }

    #[test]
    fn intrinsic_slot_preserves_real_rail_hits_and_clipping_in_both_directions() {
        for width in [1025.0, 1400.0] {
            for scale in [1.0, 1.25] {
                for zoom in [1.0, 1.25] {
                    for rtl in [false, true] {
                        let (old_body, old_hits) = capture_side(width, scale, zoom, rtl, false);
                        let (new_body, new_hits) = capture_side(width, scale, zoom, rtl, true);
                        assert_eq!(old_hits.len(), 11);
                        assert_eq!(new_hits, old_hits, "rectangles AND clipped interaction areas: width={width} scale={scale} zoom={zoom} rtl={rtl}");
                        assert_eq!((old_body.top(), old_body.bottom()), (new_body.top(), new_body.bottom()));
                        assert!(new_body.width() >= old_body.width(), "the workspace cannot lose more width");
                        if rtl { assert_eq!(old_body.right(), new_body.right()); }
                        else { assert_eq!(old_body.left(), new_body.left()); }
                    }
                }
            }
        }
    }

}
