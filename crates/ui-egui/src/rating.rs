//! One star rating for the whole interface: the clickable row of five stars ([`Rating`]) and the
//! painter-only helpers for stars drawn onto photos and captions ([`paint_rating`], [`paint_star`]).
//! It replaces `widgets::stars` and the private star-painting loops of the grid badge, the film
//! badges, the compare caption and the filter bar.
//!
//! ```ignore
//! if let Some(r) = Rating::new("bottom", photo.rating).show(ui) { … }
//! paint_rating(ui.painter(), pos, 3, RatingSize::Small, color);
//! ```
//!
//! Hovering a star previews the rating a click would set; clicking the current rating clears it.
//! The row is one Tab stop (the star of the current rating, or the first when unrated); inside it, ← and →
//! lower and raise the rating (0 to 5) and the focus follows.

use egui::{Color32, Key, Modifiers, Painter, Pos2, Rect, Sense, Ui, pos2, vec2};

use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{announce_selected, register};

/// Number of stars in a rating.
const STARS: u8 = 5;

/// How big a rating is drawn. Each size matches one of the renderers it replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RatingSize {
    /// Film badges: 8 px stars every 9 px.
    Small,
    /// Grid badges and compare captions: 12 px stars every 13 px.
    Medium,
    /// The clickable row (bottom bar, Info panel, filter bar): 14 px stars in 19 px cells.
    Large,
}

impl RatingSize {
    /// Side of one painted star.
    pub fn icon(self) -> f32 {
        match self {
            Self::Small => 8.0,
            Self::Medium => 12.0,
            Self::Large => 14.0,
        }
    }

    /// Distance from one painted star to the next in [`paint_rating`].
    pub fn step(self) -> f32 {
        match self {
            Self::Small => 9.0,
            Self::Medium => 13.0,
            Self::Large => 17.0,
        }
    }

    /// Side of one clickable cell in a [`Rating`] row.
    fn cell(self) -> f32 {
        self.step() + 2.0
    }
}

/// Paint one star (outline or filled) centred in `rect`.
pub fn paint_star(painter: &Painter, rect: Rect, filled: bool, color: Color32) {
    paint(painter, rect, if filled { Icon::StarFilled } else { Icon::Star }, color);
}

/// Paint `rating` filled stars in a row, the first one's left edge at `pos.x` and its vertical centre
/// at `pos.y`. Returns the width the row takes (0 for no stars), so the caller can lay out what follows.
pub fn paint_rating(painter: &Painter, pos: Pos2, rating: u8, size: RatingSize, color: Color32) -> f32 {
    let n = rating.min(STARS);
    let side = size.icon();
    for i in 0..n {
        let left = pos.x + f32::from(i) * size.step();
        paint_star(painter, Rect::from_min_size(pos2(left, pos.y - side / 2.0), vec2(side, side)), true, color);
    }
    f32::from(n) * size.step()
}

/// What the stars show once hovering is taken into account: the hovered star's rating previews over
/// the current one.
fn star_lit(i: u8, rating: u8, hover: Option<u8>) -> bool {
    i < hover.unwrap_or(rating)
}

/// The rating a click on star `n` (1 to 5) sets: clicking the current rating clears it.
fn clicked_rating(current: u8, n: u8) -> u8 {
    if current == n { 0 } else { n }
}

/// The rating the arrow keys move to from `current`, if any.
fn nudged_rating(current: u8, left: bool, right: bool) -> Option<u8> {
    match (left, right) {
        (true, false) if current > 0 => Some(current - 1),
        (false, true) if current < STARS => Some(current + 1),
        _ => None,
    }
}

/// A row of five stars. Build it, then [`Rating::show`].
pub struct Rating {
    id: String,
    rating: u8,
    ids_prefix: Option<String>,
}

impl Rating {
    /// A rating of `rating` (0 to 5) whose stars are reachable as `star:{id}:{n}`.
    pub fn new(id: impl Into<String>, rating: u8) -> Self {
        Self { id: id.into(), rating: rating.min(STARS), ids_prefix: None }
    }

    /// Register the stars under `{prefix}{n}` instead of `star:{id}:{n}`, for rows that already have
    /// published automation ids (the filter bar's `filter:star3`).
    pub fn automation_ids(mut self, prefix: impl Into<String>) -> Self {
        self.ids_prefix = Some(prefix.into());
        self
    }

    fn widget_id(&self, n: u8) -> String {
        match &self.ids_prefix {
            Some(prefix) => format!("{prefix}{n}"),
            None => format!("star:{}:{n}", self.id),
        }
    }

    fn star_id(&self, ui: &Ui, i: u8) -> egui::Id {
        ui.id().with(("rating-star", &self.id, i))
    }

    /// Draw the row. Returns the new rating when a click or an arrow key changed it.
    pub fn show(self, ui: &mut Ui) -> Option<u8> {
        let t = Tokens::get(ui.ctx());
        let cell = RatingSize::Large.cell();
        let mut out = None;
        let mut hover_n = None;
        let mut focused_at = None;
        let mut cells = [Rect::NOTHING; STARS as usize];
        // One Tab stop for the row, like a radio group: the star of the current rating (the first when
        // unrated), plus whichever star already has the focus so it keeps it
        let stop = self.rating.max(1) - 1;
        for (i, slot) in (0..STARS).zip(cells.iter_mut()) {
            let id = self.star_id(ui, i);
            let (r, _) = ui.allocate_exact_size(vec2(cell, cell), Sense::hover());
            let sense = if i == stop || ui.memory(|m| m.has_focus(id)) { Sense::click() } else { Sense::CLICK };
            let resp = ui.interact(r, id, sense);
            announce_selected(&resp, format!("{} ★", i + 1), i < self.rating);
            register(ui.ctx(), self.widget_id(i + 1), r);
            if resp.hovered() {
                hover_n = Some(i + 1);
            }
            if resp.clicked() {
                out = Some(clicked_rating(self.rating, i + 1));
            }
            if resp.has_focus() {
                focused_at = Some(i);
                // ← and → step the rating instead of moving the focus to the neighbouring widget
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(resp.id, egui::EventFilter { horizontal_arrows: true, ..Default::default() });
                });
                ui.painter().rect_stroke(r.expand(1.0), 6.0, egui::Stroke::new(2.0, t.accent), egui::StrokeKind::Outside);
            }
            *slot = r;
        }
        let focused = focused_at.is_some();
        if focused && out.is_none() {
            let (left, right) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowLeft), i.consume_key(Modifiers::NONE, Key::ArrowRight)));
            if left || right {
                // egui picked the neighbour to focus from this same key before the lock above could apply (the
                // first arrow after Tab): cancel that move
                ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            }
            out = nudged_rating(self.rating, left, right);
            if let Some(n) = out {
                // keep the focus on the star that is now the row's Tab stop
                let to = self.star_id(ui, n.max(1) - 1);
                ui.memory_mut(|m| m.request_focus(to));
            } else if focused_at != Some(stop) {
                // the rating changed from outside (undo, a shortcut, another photo): the focus follows the Tab stop
                let to = self.star_id(ui, stop);
                ui.memory_mut(|m| m.request_focus(to));
            }
        }
        // The hovered star also gets the icon-button hover square, so hovering the current rating (where
        // the preview looks the same) still shows feedback.
        let side = RatingSize::Large.icon();
        for (i, r) in (0..STARS).zip(cells) {
            if hover_n == Some(i + 1) {
                ui.painter().rect_filled(r, 4.0, t.hover.gamma_multiply(0.7));
            }
            let filled = star_lit(i, self.rating, hover_n);
            paint_star(ui.painter(), Rect::from_center_size(r.center(), vec2(side, side)), filled, if filled { t.star } else { t.icon });
        }
        out
    }
}

/// A frameless star toggle for favourites: filled when `favorite`, named `name` for screen readers and as
/// its tooltip, reachable as `widget` (a full automation id). A square one line tall.
pub fn favorite_button(ui: &mut Ui, widget: String, favorite: bool, name: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let side = ui.spacing().interact_size.y;
    let (r, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    announce_selected(&resp, name, favorite);
    // (rows scrolled out of view can't be clicked)
    if ui.is_rect_visible(r) {
        register(ui.ctx(), widget, r);
    }
    if resp.hovered() {
        ui.painter().rect_filled(r, 4.0, t.hover.gamma_multiply(0.7));
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(r.expand(1.0), 6.0, egui::Stroke::new(2.0, t.accent), egui::StrokeKind::Outside);
    }
    let icon = RatingSize::Medium.icon();
    paint_star(ui.painter(), Rect::from_center_size(r.center(), vec2(icon, icon)), favorite, if favorite { t.star } else { t.icon });
    resp.on_hover_text(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hovered_star_previews_its_rating() {
        let lit = |rating, hover| (0..STARS).filter(|&i| star_lit(i, rating, hover)).count();
        assert_eq!(lit(2, None), 2, "no hover shows the rating");
        assert_eq!(lit(2, Some(4)), 4);
        assert_eq!(lit(5, Some(1)), 1);
        assert_eq!(lit(0, Some(3)), 3);
    }

    #[test]
    fn arrows_stay_between_zero_and_five() {
        assert_eq!(nudged_rating(0, true, false), None);
        assert_eq!(nudged_rating(5, false, true), None);
        assert_eq!(nudged_rating(3, true, false), Some(2));
        assert_eq!(nudged_rating(3, false, true), Some(4));
        assert_eq!(nudged_rating(3, true, true), None);
    }

    #[test]
    fn rows_scrolled_out_of_view_are_not_registered() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut out =
            ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(300.0, 100.0))), ..Default::default() }, |ui| {
                egui::ScrollArea::vertical().max_height(60.0).show(ui, |ui| {
                    for i in 0..30 {
                        favorite_button(ui, format!("effectStar:{i}"), false, "Favorite");
                    }
                });
            });
        out.textures_delta.clear();
        let ids: Vec<String> = crate::widgets::take_registry(&ctx).into_iter().map(|(id, _)| id).collect();
        assert!(ids.contains(&"effectStar:0".to_string()), "{ids:?}");
        assert!(!ids.contains(&"effectStar:29".to_string()), "{ids:?}");
    }
}
