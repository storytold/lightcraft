//! The secondary window (Window ▸ Secondary Display): for a second display while the main window
//! shows the grid or the tools. Its mode ([`crate::module::SecondMode`]) is Grid, Loupe (Normal:
//! the active photo; Live: the photo under the pointer; Locked: a fixed photo), Compare, Survey or
//! Slideshow; ⇧G / ⇧E / ⇧C / ⇧N / ⌘⇧↩ in the Classic keymap. The window has its own mode
//! switcher (top), filter bar (`second.filter`, Grid) and filmstrip (`second.filmstrip`), the
//! same in a native window and in the floating window egui uses where there are none.

use dac_catalog::{Flag, PhotoId};
use egui::{Color32, Rect, Sense, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::DacApp;
use crate::module::SecondMode;
use crate::render::Slot;
use crate::widgets::register;

/// Seconds per photo in the secondary slideshow.
const SLIDE_SECONDS: f64 = 4.0;
/// The most photos the secondary grid or survey draws.
const MAX_TILES: usize = 200;
/// The most photos the filmstrip lists.
const MAX_FILM: usize = 500;
const BAR_H: f32 = 30.0;
const FILM_H: f32 = 84.0;

/// The secondary window's filter: text in the file name, a minimum rating, picks only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SecondFilter {
    pub text: String,
    pub rating: u8,
    pub picks: bool,
}

impl SecondFilter {
    fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.rating == 0 && !self.picks
    }
}

/// `second.filter {text?, rating?: 0..5, picks?: bool, clear?: bool}`.
pub fn set_filter(app: &mut DacApp, p: &Value) -> Result<Value, String> {
    let f = &mut app.ui.second_filter;
    if p.get("clear").and_then(Value::as_bool).unwrap_or(false) {
        *f = SecondFilter::default();
    }
    if let Some(t) = p.get("text").and_then(Value::as_str) {
        f.text = t.chars().take(200).collect();
    }
    if let Some(r) = p.get("rating") {
        let r = r.as_u64().filter(|r| *r <= 5).ok_or("second.filter: rating is 0..5")?;
        f.rating = r as u8;
    }
    if let Some(b) = p.get("picks").and_then(Value::as_bool) {
        f.picks = b;
    }
    Ok(json!(app.ui.second_filter))
}

/// The library's visible photos that pass the secondary filter (at most `max`).
pub fn filtered(app: &mut DacApp, max: usize) -> Vec<PhotoId> {
    let f = &app.ui.second_filter;
    let all = app.session.visible_cloned();
    if f.is_empty() {
        return all.into_iter().take(max).collect();
    }
    let needle = f.text.trim().to_lowercase();
    all.into_iter()
        .filter(|id| {
            app.session.catalog.photo(*id).is_some_and(|p| {
                p.rating >= f.rating && (!f.picks || p.flag == Flag::Pick) && (needle.is_empty() || p.file_name.to_lowercase().contains(&needle))
            })
        })
        .take(max)
        .collect()
}

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

/// The mode switcher's entries: (command, label, mode).
const MODES: &[(&str, &str, SecondMode)] = &[
    ("second.grid", "Grid", SecondMode::Grid),
    ("second.loupe", "Normal", SecondMode::Loupe),
    ("second.live", "Live", SecondMode::Live),
    ("second.locked", "Locked", SecondMode::Locked),
    ("second.compare", "Compare", SecondMode::Compare),
    ("second.survey", "Survey", SecondMode::Survey),
    ("second.slideshow", "Slideshow", SecondMode::Slideshow),
];

fn body(app: &mut DacApp, ui: &mut egui::Ui) {
    let full = ui.available_rect_before_wrap();
    ui.allocate_rect(full, Sense::hover());
    let mut area = full;
    // mode switcher
    let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
    area.min.y = bar.bottom();
    mode_bar(app, ui, bar);
    // filter bar (Grid)
    if app.ui.second_mode == SecondMode::Grid {
        let fb = Rect::from_min_size(pos2(full.left(), area.top()), vec2(full.width(), BAR_H));
        area.min.y = fb.bottom();
        filter_bar(app, ui, fb);
    }
    // filmstrip (not under the grid or the slideshow)
    let film_on = app.ui.second_filmstrip && !matches!(app.ui.second_mode, SecondMode::Grid | SecondMode::Slideshow);
    if film_on && area.height() > FILM_H * 2.0 {
        let film = Rect::from_min_max(pos2(full.left(), full.bottom() - FILM_H), full.max);
        area.max.y = film.top();
        filmstrip(app, ui, film);
    }
    register(ui.ctx(), "view:secondWindow", area);
    view(app, ui, area);
}

fn mode_bar(app: &mut DacApp, ui: &mut egui::Ui, bar: Rect) {
    ui.painter().rect_filled(bar, 0.0, Color32::from_gray(24));
    let mut x = bar.left() + 8.0;
    for (cmd, label, mode) in MODES {
        let text = crate::i18n::tr(label);
        let w = 18.0 + ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(12.0), Color32::WHITE).size().x;
        let r = Rect::from_min_size(pos2(x, bar.top() + 4.0), vec2(w, BAR_H - 8.0));
        let on = app.ui.second_mode == *mode;
        let resp = ui.put(r, egui::Button::new(egui::RichText::new(text).size(12.0)).selected(on));
        register(ui.ctx(), format!("secondMode:{}", cmd.trim_start_matches("second.")), r);
        if resp.clicked() {
            let _ = app.run(cmd, json!({}));
        }
        x += w + 4.0;
        if *mode == SecondMode::Grid || *mode == SecondMode::Locked {
            x += 8.0;
        }
    }
    // the filmstrip toggle, right-aligned
    if !matches!(app.ui.second_mode, SecondMode::Grid | SecondMode::Slideshow) {
        let r = Rect::from_min_size(pos2(bar.right() - 90.0, bar.top() + 4.0), vec2(82.0, BAR_H - 8.0));
        let resp = ui.put(r, egui::Button::new(egui::RichText::new(crate::i18n::tr("Filmstrip")).size(12.0)).selected(app.ui.second_filmstrip));
        register(ui.ctx(), "secondFilmstripToggle", r);
        if resp.clicked() {
            let _ = app.run("second.filmstrip", json!({}));
        }
    }
}

fn filter_bar(app: &mut DacApp, ui: &mut egui::Ui, fb: Rect) {
    ui.painter().rect_filled(fb, 0.0, Color32::from_gray(32));
    let mut f = app.ui.second_filter.clone();
    let mut x = fb.left() + 8.0;
    let r = Rect::from_min_size(pos2(x, fb.top() + 4.0), vec2(180.0, BAR_H - 8.0));
    ui.put(r, egui::TextEdit::singleline(&mut f.text).hint_text(crate::i18n::tr("File name")));
    register(ui.ctx(), "field:secondFilterText", r);
    x = r.right() + 12.0;
    for n in 0..=5u8 {
        let label = if n == 0 { crate::i18n::tr("Any").to_string() } else { format!("≥{}", "★".repeat(n as usize)) };
        let w = 16.0 + ui.painter().layout_no_wrap(label.clone(), egui::FontId::proportional(11.0), Color32::WHITE).size().x;
        let r = Rect::from_min_size(pos2(x, fb.top() + 4.0), vec2(w, BAR_H - 8.0));
        if ui.put(r, egui::Button::new(egui::RichText::new(label).size(11.0)).selected(f.rating == n)).clicked() {
            f.rating = n;
        }
        register(ui.ctx(), format!("secondFilterRating:{n}"), r);
        x = r.right() + 3.0;
    }
    let r = Rect::from_min_size(pos2(x + 10.0, fb.top() + 4.0), vec2(90.0, BAR_H - 8.0));
    ui.put(r, egui::Checkbox::new(&mut f.picks, crate::i18n::tr("Picks only")));
    register(ui.ctx(), "check:secondFilterPicks", r);
    if f != app.ui.second_filter {
        let _ = app.run("second.filter", json!({"text": f.text, "rating": f.rating, "picks": f.picks}));
    }
}

fn filmstrip(app: &mut DacApp, ui: &mut egui::Ui, film: Rect) {
    ui.painter().rect_filled(film, 0.0, Color32::from_gray(20));
    let photos = filtered(app, MAX_FILM);
    let side = film.height() - 12.0;
    let active = app.session.active();
    // keep the active photo in view: start the strip so it sits near the middle
    let per = side + 6.0;
    let fits = ((film.width() / per).floor() as usize).max(1);
    let at = active.and_then(|a| photos.iter().position(|p| *p == a)).unwrap_or(0);
    let start = at.saturating_sub(fits / 2).min(photos.len().saturating_sub(fits));
    let ppp = ui.ctx().pixels_per_point();
    for (i, id) in photos.iter().skip(start).take(fits).enumerate() {
        let cell = Rect::from_min_size(pos2(film.left() + 6.0 + i as f32 * per, film.top() + 6.0), vec2(side, side));
        super::grid::request_thumb(app, *id, (side * ppp) as usize, 3);
        if let Some(t) = app.renderer.thumb(*id) {
            fit(ui, cell, t);
        } else {
            ui.painter().rect_filled(cell, 2.0, Color32::from_gray(40));
        }
        if Some(*id) == active {
            ui.painter().rect_stroke(cell, 2.0, egui::Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Outside);
        }
        register(ui.ctx(), format!("secondFilm:{}", id.0), cell);
        if ui.interact(cell, egui::Id::new(("second-film", id.0)), Sense::click()).clicked() {
            let _ = app.run("library.select", json!({"ids": [id.0]}));
        }
    }
}

fn view(app: &mut DacApp, ui: &mut egui::Ui, area: Rect) {
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
        SecondMode::Grid => filtered(app, MAX_TILES),
        SecondMode::Slideshow => {
            let all = filtered(app, usize::MAX);
            let now = ui.ctx().input(|i| i.time);
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(SLIDE_SECONDS / 4.0));
            let n = all.len().max(1);
            all.get(((now / SLIDE_SECONDS) as usize) % n).copied().into_iter().collect()
        }
    };
    let Some(first) = photos.first().copied() else {
        let msg = if app.ui.second_mode == SecondMode::Grid { "No photos match the filter" } else { "No photo selected" };
        ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, crate::i18n::tr(msg), egui::FontId::proportional(14.0), Color32::GRAY);
        return;
    };
    if photos.len() == 1 && app.ui.second_mode != SecondMode::Grid {
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
        // a grid tile selects its photo
        if app.ui.second_mode == SecondMode::Grid {
            register(ui.ctx(), format!("secondTile:{}", id.0), cell);
            if ui.interact(cell, egui::Id::new(("second-tile", id.0)), Sense::click()).clicked() {
                let _ = app.run("library.select", json!({"ids": [id.0]}));
            }
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
