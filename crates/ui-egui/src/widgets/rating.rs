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
//! The row is the framework's `Rating`: hovering a star previews the rating a click would set; clicking the
//! current rating clears it. The row is one Tab stop; inside it, ← and → move between the stars, Space sets the
//! focused star's rating and the digits 0 to 5 set the rating while the row has the focus.

use craft::ui::buttons::{ButtonOutput, IconButton};
use craft::ui::contract::{Bind, Props as _, Show as _};
use craft::ui::primitives::{Glyph, IconSpec};
use egui::{Color32, Painter, Pos2, Rect, Ui, Vec2, pos2, vec2};

use crate::icons::{Icon, paint};
use crate::theme::Tokens;

/// Number of stars in a rating.
const STARS: u8 = 5;

/// How big a rating is drawn. Each size matches one of the renderers it replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RatingSize {
    /// Film badges: 8 px stars every 9 px.
    Small,
    /// Grid badges and compare captions: 12 px stars every 13 px.
    Medium,
    /// 14 px stars every 17 px.
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

    /// Draw the row. Returns the new rating when a click, a key or a digit changed it.
    pub fn show(self, ui: &mut Ui) -> Option<u8> {
        use craft::ui::choice::Rating as Stars;
        let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
        let shown = Stars::new(Bind::given(self.rating), STARS)
            .id_salt(("lc-rating", self.id.as_str()))
            .label(crate::i18n::tr("Rating").to_owned())
            .size(craft::theme::ControlSize::Sm)
            .show(ui);
        // a measuring run draws nothing the user can reach: no automation names, no change
        if !real {
            return None;
        }
        // Each star is addressable by its own name at its place in the row: the row's response owns the press, and the
        // framework's part under the pointer takes it.
        let row = shown.response.rect;
        let cell = row.width() / f32::from(STARS);
        let rtl = ui.layout().prefer_right_to_left();
        for n in 1..=STARS {
            let at = f32::from(if rtl { STARS - n } else { n - 1 });
            let star = Rect::from_min_size(pos2(row.left() + cell * at, row.top()), vec2(cell, row.height()));
            crate::widgets::register_response(self.widget_id(n), star, &shown.response);
        }
        shown.events.committed().copied().filter(|_| ui.is_enabled())
    }
}

/// The star of a favourites toggle: filled and lit when `favorite`, outlined and quiet when not.
#[derive(craft::ui::contract::MemoProps)]
struct FavoriteStar {
    favorite: bool,
    lit: Color32,
    quiet: Color32,
}

impl Glyph for FavoriteStar {
    fn paint(&self, painter: &Painter, _: &craft::ui::style::UiTokens, rect: Rect, _: Color32, _: bool) {
        let side = RatingSize::Medium.icon();
        let star = Rect::from_center_size(rect.center(), Vec2::splat(side));
        paint_star(painter, star, self.favorite, if self.favorite { self.lit } else { self.quiet });
    }
}

/// A frameless star toggle for favourites: filled when `favorite`, named `name` for screen readers and as
/// its tooltip, reachable as `widget` (a full automation id). A square one line tall.
pub fn favorite_button(ui: &mut Ui, widget: String, favorite: bool, name: &str) -> ButtonOutput {
    let t = Tokens::get(ui.ctx());
    let side = ui.spacing().interact_size.y;
    // the layout's room, sensing nothing: the framework button owns the pointer
    let (id, rect) = ui.allocate_space(vec2(side, side));
    let star = FavoriteStar { favorite, lit: t.star, quiet: t.icon };
    let out = IconButton::new(IconSpec::glyph(&star))
        .id(id)
        .label(name.to_owned())
        .tooltip(name.to_owned())
        .selected(favorite)
        .show_at(ui, rect);
    // (rows scrolled out of view can't be clicked)
    if ui.is_rect_visible(rect) && !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        crate::widgets::register_response(widget, rect, &out.response);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_scrolled_out_of_view_are_not_registered() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        crate::app::install_runtime(&ctx).expect("the UI runtime installs");
        for _ in 0..3 {
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
}
