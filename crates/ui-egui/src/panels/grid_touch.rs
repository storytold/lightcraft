//! iPad-only behaviour of the Grid views (the empty library's import buttons, the missing-original
//! badge and the circular selection ticks on a cell). `grid.rs` calls these; each returns at once
//! unless `theme::touch_ui` is on, so desktop is untouched.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::{Photo, PhotoId, Source};
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::theme::touch_ui;
use crate::widgets::register;

/// The iPad's empty library: what to do next, with the actions on screen. Returns true when it drew
/// something, so the caller knows not to fall back to the desktop message.
pub(super) fn empty_library(app: &mut LightcraftApp, ui: &mut egui::Ui) -> bool {
    let r = ui.max_rect();
    empty_library_in(app, ui, r)
}

/// The iPad's empty-library screen in `r`: Files folder first (the Photos picker cannot reach Files), then Photos.
pub(super) fn empty_library_in(app: &mut LightcraftApp, ui: &mut egui::Ui, r: egui::Rect) -> bool {
    if !touch_ui(ui.ctx()) {
        return false;
    }
    let t = Tokens::get(ui.ctx());
    let c = r.center();
    let p = ui.painter();
    p.text(c - vec2(0.0, 74.0), Align2::CENTER_CENTER, crate::i18n::tr("No photos yet"), t.semibold(22.0), t.text);
    p.text(
        c - vec2(0.0, 40.0),
        Align2::CENTER_CENTER,
        crate::i18n::tr("Bring in photos from Files or your Photos library to start editing."),
        t.font(14.0),
        t.text_dim,
    );
    let gap = 12.0;
    // two buttons side by side; narrower on a Split View / portrait panel so neither is clipped
    let (w, h) = (210.0f32.min((r.width() - gap - 32.0) / 2.0).max(110.0), 46.0);
    let y = c.y + 2.0;
    let import_folder = Rect::from_min_size(pos2(c.x - w - gap / 2.0, y), vec2(w, h));
    let import_photos = Rect::from_min_size(pos2(c.x + gap / 2.0, y), vec2(w, h));
    for (rect, id, label, command, primary) in [
        (import_folder, "emptyImportFolder", "Import from Folder", "file.addFolder", true),
        (import_photos, "emptyImportPhotos", "Import Photos", "file.addPhotos", false),
    ] {
        let resp = ui.interact(rect, egui::Id::unique(id), Sense::click());
        register(ui, format!("button:{id}"), rect);
        crate::widgets::announce(&resp, crate::i18n::tr(label));
        let fill = match (primary, resp.is_pointer_button_down_on(), resp.hovered()) {
            (true, true, _) => t.accent.gamma_multiply(0.8),
            (true, _, _) => t.accent,
            (false, true, _) => Color32::from_white_alpha(50),
            (false, _, true) => Color32::from_white_alpha(34),
            _ => Color32::from_white_alpha(22),
        };
        ui.painter().rect_filled(rect, h / 2.0, fill);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, crate::i18n::tr(label), t.semibold(15.0), Color32::WHITE);
        if resp.clicked() {
            let _ = app.run(command, json!({}));
        }
    }
    true
}

/// The original is gone (a drive that left, or an old install): say so on the cell, not only in the
/// detail view.
pub(super) fn missing_badge(app: &LightcraftApp, ui: &egui::Ui, p: &egui::Painter, t: &Tokens, img_rect: Rect, photo: &Photo, id: PhotoId) {
    if !touch_ui(ui.ctx()) {
        return;
    }
    let Source::File { path } = &photo.source else { return };
    if !app.session.media.availability.is_offline(path) {
        return;
    }
    let g = p.layout_no_wrap(crate::i18n::tr("Original missing").into(), t.semibold(10.5), Color32::WHITE);
    let br = Rect::from_min_size(img_rect.left_top() + vec2(6.0, 6.0), vec2(g.size().x + 26.0, 20.0));
    p.rect_filled(br, 10.0, Color32::from_rgba_unmultiplied(40, 30, 10, 220));
    paint(p, Rect::from_center_size(pos2(br.left() + 11.0, br.center().y), vec2(13.0, 13.0)), Icon::Info, t.caution);
    p.galley(pos2(br.left() + 20.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
    register(ui, format!("badge:missing:{}", id.0), br);
}

/// Apple-style circular multiselect checkbox (multi-select mode, or more than one photo selected).
pub(super) fn select_circle(app: &LightcraftApp, ui: &egui::Ui, p: &egui::Painter, t: &Tokens, img_rect: Rect, id: PhotoId, selected: bool) {
    if !touch_ui(ui.ctx()) || !(app.ui.multi_select || selected || app.session.selection.ids.len() > 1) {
        return;
    }
    let center = img_rect.right_top() + vec2(-18.0, 18.0);
    let check_circle = Rect::from_center_size(center, vec2(26.0, 26.0));
    register(ui, format!("select_circle:{}", id.0), check_circle);
    if selected {
        p.circle_filled(center, 12.0, t.accent);
        p.circle_stroke(center, 12.0, Stroke::new(1.0, Color32::WHITE));
        p.line_segment([center + vec2(-4.5, 0.0), center + vec2(-1.5, 3.5)], Stroke::new(2.4, Color32::WHITE));
        p.line_segment([center + vec2(-1.5, 3.5), center + vec2(5.0, -3.5)], Stroke::new(2.4, Color32::WHITE));
    } else {
        p.circle_filled(center, 12.0, Color32::from_rgba_unmultiplied(0, 0, 0, 100));
        p.circle_stroke(center, 12.0, Stroke::new(1.6, Color32::from_rgba_unmultiplied(255, 255, 255, 200)));
    }
}
