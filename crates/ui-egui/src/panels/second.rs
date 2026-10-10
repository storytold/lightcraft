//! The secondary window (Window ▸ Secondary Display): for a second display while the main window
//! shows the grid or the tools. Its mode ([`crate::module::SecondMode`]) is Grid, Loupe (Normal:
//! the active photo; Live: the photo under the pointer; Locked: a fixed photo), Compare, Survey or
//! Slideshow; ⇧G / ⇧E / ⇧C / ⇧N / ⌘⇧↩ in the Classic keymap.

use dac_catalog::PhotoId;
use egui::{Color32, Rect, pos2, vec2};

use crate::DacApp;
use crate::module::SecondMode;
use crate::render::Slot;

/// Seconds per photo in the secondary slideshow.
const SLIDE_SECONDS: f64 = 4.0;
/// The most photos the secondary grid or survey draws.
const MAX_TILES: usize = 200;

pub fn show(app: &mut DacApp, ctx: &egui::Context) {
    if !app.ui.second_window {
        return;
    }
    let vid = egui::ViewportId::from_hash_of("app-second-window");
    let builder = egui::ViewportBuilder::default().with_title(format!("{} — Second Window", dac_brand::DISPLAY_NAME)).with_inner_size([960.0, 640.0]);
    ctx.show_viewport_immediate(vid, builder, |ctx, class| {
        // (without native windows — web, headless — egui wraps this in a floating window)
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ctx, |ui| body(app, ui));
        if class != egui::ViewportClass::EmbeddedWindow && ctx.input(|i| i.viewport().close_requested()) {
            app.ui.second_window = false;
        }
    });
}

fn body(app: &mut DacApp, ui: &mut egui::Ui) {
    let area = ui.available_rect_before_wrap();
    crate::widgets::register(ui.ctx(), "view:secondWindow", area);
    ui.allocate_rect(area, egui::Sense::hover());
    let active = app.session.active();
    let photos: Vec<PhotoId> = match app.ui.second_mode {
        SecondMode::Loupe => active.into_iter().collect(),
        SecondMode::Live => app.ui.hovered_photo.map(PhotoId).filter(|p| app.session.catalog.photo(*p).is_some()).or(active).into_iter().collect(),
        SecondMode::Locked => app.ui.second_locked.map(PhotoId).filter(|p| app.session.catalog.photo(*p).is_some()).or(active).into_iter().collect(),
        SecondMode::Compare => match app.ui.compare {
            Some((a, b)) => vec![PhotoId(a), PhotoId(b)],
            None => app.session.selection.ids.iter().take(2).copied().collect(),
        },
        SecondMode::Survey => app.session.selection.ids.iter().take(MAX_TILES).copied().collect(),
        SecondMode::Grid => app.session.visible_cloned().into_iter().take(MAX_TILES).collect(),
        SecondMode::Slideshow => {
            let all = app.session.visible_cloned();
            let now = ui.ctx().input(|i| i.time);
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(SLIDE_SECONDS / 4.0));
            let n = all.len().max(1);
            all.get(((now / SLIDE_SECONDS) as usize) % n).copied().into_iter().collect()
        }
    };
    let Some(first) = photos.first().copied() else {
        ui.painter().text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            crate::i18n::tr("No photo selected"),
            egui::FontId::proportional(14.0),
            Color32::GRAY,
        );
        return;
    };
    if photos.len() == 1 {
        loupe(app, ui, area, first);
        return;
    }
    // tiles: a near-square grid (Compare: two side by side)
    let n = photos.len();
    let cols = if app.ui.second_mode == SecondMode::Compare {
        2
    } else {
        ((n as f32 * area.width() / area.height().max(1.0)).sqrt().ceil() as usize).clamp(1, n)
    };
    let rows = n.div_ceil(cols);
    let cw = area.width() / cols as f32;
    let ch = area.height() / rows.max(1) as f32;
    let ppp = ui.ctx().pixels_per_point();
    for (i, id) in photos.into_iter().enumerate() {
        let cell = Rect::from_min_size(pos2(area.left() + (i % cols) as f32 * cw, area.top() + (i / cols) as f32 * ch), vec2(cw, ch)).shrink(4.0);
        super::grid::request_thumb(app, id, (cell.width().max(cell.height()) * ppp) as usize, 4);
        if let Some(t) = app.renderer.thumb(id) {
            fit(ui, cell, t);
        } else {
            ui.painter().rect_filled(cell, 2.0, Color32::from_gray(30));
        }
    }
}

/// One photo, rendered for this window.
fn loupe(app: &mut DacApp, ui: &mut egui::Ui, area: Rect, id: PhotoId) {
    let ppp = ui.ctx().pixels_per_point();
    let side = super::detail::texture_side(ui.ctx()).clamp(64, 4096);
    let (w, h) = (((area.width() * ppp) as usize).clamp(64, side), ((area.height() * ppp) as usize).clamp(64, side));
    if let Some(job) = app.session.loupe_job(id, w, h, true)
        && app.renderer.textures.get(&Slot::Second).is_none_or(|t| t.key != job.key)
        && !app.renderer.is_pending(Slot::Second)
    {
        app.renderer.request(Slot::Second, job, 95);
    }
    let mine = |s: Slot| app.renderer.textures.get(&s).filter(|t| t.photo == id);
    if let Some(t) = mine(Slot::Second).or_else(|| mine(Slot::Main)).or_else(|| app.renderer.thumb(id)) {
        fit(ui, area, t);
    }
}

fn fit(ui: &egui::Ui, area: Rect, t: &crate::render::Tex) {
    let [tw, th] = t.tex.size();
    let s = (area.width() / tw.max(1) as f32).min(area.height() / th.max(1) as f32);
    let r = Rect::from_center_size(area.center(), vec2(tw as f32 * s, th as f32 * s));
    ui.painter().image(t.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}
