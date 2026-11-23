//! Save Effect's SDK modal shell. The application draft and existing confirm command
//! remain the only owners of preset validation and writes. Other dialogs stay unchanged.

use std::cell::Cell;

use craft::ui::buttons::Button;
use craft::ui::contract::overlay::Outside;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, clicked};
use craft::ui::dialog::Dialog as FrameworkDialog;

use crate::LightcraftApp;
use crate::state::Dialog;

pub(super) fn active(app: &LightcraftApp, ctx: &egui::Context) -> bool {
    matches!(app.ui.dialog, Some(Dialog::SaveEffect { .. })) && !crate::panels::settings::native_controls_active(ctx)
}

pub(super) fn owner() -> egui::Id {
    egui::Id::unique("lightcraft.save-effect")
}

/// Context-only legacy callers still mount the same real SDK shell, never an egui Window.
/// The production root path below does not introduce this extra Area.
pub(super) fn show(app: &mut LightcraftApp, ctx: &egui::Context, blocked: bool) {
    egui::Area::new(egui::Id::unique("save-effect-host")).fixed_pos(ctx.content_rect().min).show(ctx, |ui| show_root(app, ui, blocked));
}

pub(super) fn show_root(app: &mut LightcraftApp, root: &mut egui::Ui, blocked: bool) {
    let Some(mut dialog @ Dialog::SaveEffect { .. }) = app.ui.dialog.clone() else { return };
    let at_start = dialog.clone();
    let ctx = root.ctx().clone();
    let blocked = blocked || crate::panels::library_problem::blocks_dialog_input(app);
    let title_bar = super::title_bar_band(app);
    let popup_was_open = egui::Popup::is_any_open(&ctx) || craft::ui::runtime::overlay::family(&ctx, owner()).len() > 1;
    let close = Cell::new(false);
    let confirm = Cell::new(false);
    let footer_dialog = dialog.clone();
    // The Save body and footer own real SDK leaves. Name alone supplies the body
    // submit signal; no default-Enter route is enabled by the footer Buttons.
    let output = FrameworkDialog::new(crate::i18n::tr("New Effect Preset").to_owned())
        .id(owner())
        .opened(Bind::given(true))
        .closable(false)
        .close_on_enter(false)
        .enabled(!blocked)
        .overlay(|mut overlay| {
            overlay.dismiss = overlay.dismiss.outside(Outside::Keep);
            overlay
        })
        .footer(|ui, _| {
            let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
            let sam = super::SamState::of(app);
            let cancel_label = crate::i18n::tr(super::cancel_label(&footer_dialog, &sam)).to_owned();
            let ok_label = crate::i18n::tr(&super::ok_label(app, &footer_dialog, false, &sam)).to_owned();
            let can_confirm = super::can_confirm(app, &footer_dialog, false);
            let primary = can_confirm && super::confirm_style(&footer_dialog) == super::ConfirmStyle::Primary;
            let mut cancel = Button::new(cancel_label).id_salt("button:dialogCancel");
            let mut accept = Button::new(ok_label).id_salt("button:dialogOk").enabled(can_confirm).variant(if primary {
                craft::theme::Variant::PRIMARY
            } else {
                craft::theme::Variant::SECONDARY
            });
            let cancel_size = cancel.measure_in(ui, craft::behavior::Proposal::Ideal).max(egui::vec2(72.0, ui.spacing().interact_size.y));
            let accept_size = accept.measure_in(ui, craft::behavior::Proposal::Ideal).max(egui::vec2(72.0, ui.spacing().interact_size.y));
            let width = ui.available_width().max(ui.min_rect().width()).max(1.0);
            let height = cancel_size.y.max(accept_size.y);
            ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let gap = ui.spacing().item_spacing.x;
                ui.add_space((ui.available_width() - cancel_size.x - gap - accept_size.x).max(0.0));
                // Display and actual SDK Tab registration stay Cancel then Create.
                let (rect, _) = ui.allocate_exact_size(cancel_size, egui::Sense::hover());
                let cancel = cancel.show_at(ui, rect);
                let (rect, _) = ui.allocate_exact_size(accept_size, egui::Sense::hover());
                let accept = accept.show_at(ui, rect);
                if real {
                    crate::widgets::register_response("button:dialogCancel", cancel.rect, &cancel);
                    crate::widgets::register_response("button:dialogOk", accept.rect, &accept);
                    close.set(close.get() || clicked(&cancel));
                    confirm.set(confirm.get() || clicked(&accept));
                }
            });
        })
        .show(root, |ui, _| {
            ui.set_min_width(380.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            if let Dialog::SaveEffect { name, group, fields, amount, brush } = &mut dialog {
                let submitted = crate::panels::mask_effects::save_body(app, ui, name, group, fields, amount, brush);
                confirm.set(confirm.get() || submitted);
            }
        });
    if root.is_sizing_pass() || craft::ui::runtime::measure::active(&ctx) {
        return;
    }
    crate::widgets::register_response("dialog:window", output.response.rect, &output.response);
    if !popup_was_open && let Some(band) = title_bar {
        super::title_bar_handle(&ctx, band, output.response.rect);
    }
    if blocked {
        ctx.data_mut(|data| data.remove_temp::<bool>(egui::Id::unique("dialog-backdrop-press")));
    } else {
        close.set(close.get() || output.closed.is_some());
        if app.ui.settings.close_dialogs_outside && super::clicked_backdrop(&ctx, output.response.rect, title_bar, popup_was_open) {
            close.set(true);
        }
        if confirm.get() {
            close.set(close.get() || super::confirmed(app, &ctx, &mut dialog));
        }
    }
    // A command may replace the dialog. Never overwrite that handoff with our old draft.
    if app.ui.dialog.as_ref().is_some_and(|now| *now != at_start) {
        return;
    }
    if !close.get() && !blocked {
        // Context callbacks hold egui's lock. Read the frame before the write
        // callback rather than recursively acquiring a read lock inside data_mut.
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|data| data.insert_temp(egui::Id::unique(super::BACKDROP_SHOWN), frame));
    }
    if close.get() {
        let popup = egui::Id::unique("lightcraft.save-effect.group-popup");
        ctx.data_mut(|data| {
            data.remove_temp::<bool>(popup);
            data.remove_temp::<u64>(popup.with("shown-pass"));
        });
    }
    app.ui.dialog = if close.get() { None } else { Some(dialog) };
}
