//! The second window (Window ▸ Second Window): the active photo, fitted, in its own window —
//! for a second display while the main window shows the grid or the tools.

use egui::{Color32, Rect, pos2, vec2};

use crate::LightcraftApp;
use crate::render::Slot;

/// Show the optional second window: an immediate viewport titled "LightCraft — Second Window"
/// whose body draws the active photo fitted. Closing a real window turns the option off;
/// embedded web and headless hosts get a floating window instead.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.ui.second_window {
        return;
    }
    let vid = egui::ViewportId::from_hash_of("lightcraft-second-window");
    let builder = egui::ViewportBuilder::default().with_title(crate::i18n::tr("LightCraft — Second Window")).with_inner_size([960.0, 640.0]);
    ctx.show_viewport_immediate(vid, builder, |ui, class| {
        // 0.37 passes the viewport's root `Ui` (not a `Context`). Do not enter `root_ui` from it.
        // (without native windows — web, headless — egui wraps this in a floating window)
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ui, |ui| body(app, ui));
        if class != egui::ViewportClass::EmbeddedWindow && ui.ctx().input(|i| i.viewport().close_requested()) {
            app.ui.second_window = false;
        }
    });
}

/// Draw the second window's content: register `view:secondWindow`, request a loupe render sized
/// for the window (each edge clamped to 64 through the texture-side limit) and paint the active
/// photo fitted, or "No photo selected".
fn body(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let area = ui.available_rect_before_wrap();
    crate::widgets::register(ui, "view:secondWindow", area);
    ui.allocate_rect(area, egui::Sense::hover());
    let Some(id) = app.session.active() else {
        ui.painter().text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            crate::i18n::tr("No photo selected"),
            egui::FontId::proportional(14.0),
            Color32::GRAY,
        );
        return;
    };
    // a render sized for this window
    let ppp = ui.ctx().pixels_per_point();
    let side = super::detail::texture_side(ui.ctx()).clamp(64, 4096);
    let (w, h) = (((area.width() * ppp) as usize).clamp(64, side), ((area.height() * ppp) as usize).clamp(64, side));
    if let Some(job) = app.session.loupe_job(id, w, h, true).map(|j| app.renderer.job_for(Slot::Second, j))
        && app.renderer.textures.get(&Slot::Second).is_none_or(|t| t.key != job.key)
        && !app.renderer.is_pending(Slot::Second)
    {
        app.renderer.request(Slot::Second, job, 95);
    }
    let mine = |s: Slot| app.renderer.textures.get(&s).filter(|t| t.photo == id);
    // Main can be explicitly zoomed while this automatic Fit view is open. Do not borrow
    // its nearest sampler: use the smooth thumbnail until this window's render is ready.
    let tex =
        mine(Slot::Second).or_else(|| app.renderer.main_for_fit(id, [area.width() * ppp, area.height() * ppp])).or_else(|| app.renderer.thumb(id));
    if let Some(t) = tex {
        let [tw, th] = t.tex.size();
        let s = (area.width() / tw as f32).min(area.height() / th as f32);
        let r = Rect::from_center_size(area.center(), vec2(tw as f32 * s, th as f32 * s));
        ui.painter().image(t.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
}
