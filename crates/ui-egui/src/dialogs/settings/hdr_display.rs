//! Settings for HDR display (LR-VIEW-HDR-DISPLAY): the switch on the Performance tab and the note
//! on the Display tab that an HDR window sets the monitor profile aside.

use super::widgets::{check, hint};
use super::{LightcraftApp, RichText, Tokens};

/// Performance ▸ Rendering: "Show HDR photos in HDR on HDR displays" and what it does now.
pub(super) fn hdr_display_switch(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    check(ui, "settings.hdrDisplay", &mut app.ui.settings.hdr_display, "Show HDR photos in HDR on HDR displays");
    let hdr_status = match (app.hdr_presenter.is_some(), app.ui.settings.hdr_display) {
        (true, true) => "HDR display active: HDR edits show their highlights brighter than SDR white.",
        (true, false) => "Turns off after a restart.",
        (false, true) => "This window isn't HDR (no HDR display or system support, or turned on since the start: restart).",
        (false, false) => "Applies after a restart.",
    };
    hint(ui, t, hdr_status);
    if !crate::state::HDR_DISPLAY_DEFAULT {
        hint(ui, t, "Off by default on Linux: HDR display needs a native Wayland window, which can't take files dragged in from the file manager.");
    }
}

/// Display: an HDR window is colour-managed by the system, so no monitor profile applies.
pub(super) fn hdr_window_note(app: &LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.renderer.scrgb_window() {
        let note = crate::i18n::tr("This window is HDR: the system manages its colour, so no monitor profile is applied.");
        ui.label(RichText::new(note).size(11.0).color(t.text_dim));
    }
}
