//! The filmstrip's own filter (issue #1296), at the right of its source line as in Lightroom Classic:
//! "Filter", then the flags (picked, unflagged, rejected), the rating operator and five stars, then the
//! five colour labels. Every control narrows what the strip (and the grid) show through `library.filter`,
//! the same command as the Library filter bar; nothing here rates or flags a photo (the toolbar does).
//! Widget ids: `filmfilter:pick|none|reject`, `filmfilter:ratingOp`, `filmfilter:star{1..5}`,
//! `filmfilter:label-<colour>`.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lcc_catalog::{ColorLabel, Flag, RatingOp};
use serde_json::{Value, json};

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::register;

const CELL: f32 = 15.0;
const STEP: f32 = 17.0;
const GROUP_GAP: f32 = 10.0;

/// The flags in Lightroom Classic's filmstrip order, with their `library.filter` value and tooltip.
pub(crate) const FLAGS: [(&str, Flag, &str); 3] =
    [("pick", Flag::Pick, "Picked"), ("none", Flag::None, "Unflagged"), ("reject", Flag::Reject, "Rejected")];

/// The width the controls (without the caption) take.
fn controls_width() -> f32 {
    STEP * (3.0 + 1.0 + 5.0 + 5.0) + GROUP_GAP * 2.0
}

/// The next rating comparison when the operator is clicked: ≥ → = → ≤ → ≥.
pub(crate) fn next_op(op: RatingOp) -> &'static str {
    match op {
        RatingOp::AtLeast => "exactly",
        RatingOp::Exactly => "atMost",
        RatingOp::AtMost => "atLeast",
    }
}

/// The rating a click on star `n` (1–5) asks for: that rating, or none when it is already the one set.
pub(crate) fn star_click(current: u8, n: u8) -> u8 {
    if current == n { 0 } else { n.min(5) }
}

/// Draws the filter at the right of `head`; returns the x where it begins (the source line keeps left of
/// it), or `head.right()` when the strip is too narrow to carry it.
pub(crate) fn show(app: &mut LccApp, ui: &egui::Ui, head: Rect, t: &Tokens) -> f32 {
    let font = t.font(11.0);
    let caption = crate::i18n::tr("Filter");
    let caption_w = ui.painter().layout_no_wrap(caption.to_string(), font.clone(), t.text_dim).size().x;
    let width = caption_w + 8.0 + controls_width();
    // only while the source line keeps room for its count
    if head.height() < CELL || head.width() < width + 260.0 {
        return head.right();
    }
    let f = app.session.filter.clone();
    let y = head.center().y;
    let left = head.right() - 10.0 - width;
    let p = ui.painter().clone();
    p.text(pos2(left, y), Align2::LEFT_CENTER, caption, font.clone(), t.text_dim);
    let mut x = left + caption_w + 8.0;
    let mut patch: Option<Value> = None;
    let cell = |x: f32, id: &str, tip: &str, on: bool| -> (Rect, egui::Response) {
        let r = Rect::from_center_size(pos2(x + STEP / 2.0, y), vec2(CELL, CELL));
        let resp = ui.interact(r, egui::Id::new(("filmfilter", id.to_string())), Sense::click());
        crate::widgets::announce_selected(&resp, crate::i18n::tr(tip), on);
        register(ui.ctx(), format!("filmfilter:{id}"), r);
        if on {
            p.rect_filled(r, 3.0, t.tool_active);
            p.rect_stroke(r, 3.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
        } else if resp.hovered() {
            p.rect_filled(r, 3.0, t.hover);
        }
        (r, resp.on_hover_text(crate::i18n::tr(tip)))
    };
    // flags
    for (id, flag, tip) in FLAGS {
        let on = f.flag == Some(flag);
        let (r, resp) = cell(x, id, tip, on);
        let color = match flag {
            Flag::Pick if on => t.pick,
            Flag::Reject if on => t.reject,
            _ if resp.hovered() => t.text_label,
            _ => t.icon,
        };
        let icon = match flag {
            Flag::Pick if on => Icon::FlagPickFilled,
            Flag::Pick => Icon::FlagPick,
            Flag::Reject => Icon::FlagReject,
            Flag::None => Icon::Circle,
        };
        paint(&p, r.shrink(2.0), icon, color);
        if resp.clicked() {
            patch = Some(json!({"flag": if on { Value::Null } else { json!(id) }}));
        }
        x += STEP;
    }
    x += GROUP_GAP;
    // rating: the comparison, then the stars
    let (op_text, op_tip) = match f.rating_op {
        RatingOp::AtLeast => ("≥", "≥  at least"),
        RatingOp::Exactly => ("=", "=  exactly"),
        RatingOp::AtMost => ("≤", "≤  at most"),
    };
    let (r, resp) = cell(x, "ratingOp", op_tip, false);
    p.text(r.center(), Align2::CENTER_CENTER, op_text, font, if f.rating > 0 { t.text } else { t.text_dim });
    if resp.clicked() {
        patch = Some(json!({"ratingOp": next_op(f.rating_op)}));
    }
    x += STEP;
    for n in 1..=5u8 {
        let (r, resp) = cell(x, &format!("star{n}"), "Rating", false);
        let lit = n <= f.rating;
        let icon = if lit { Icon::StarFilled } else { Icon::Star };
        paint(
            &p,
            r.shrink(1.5),
            icon,
            if lit {
                t.text
            } else if resp.hovered() {
                t.text_label
            } else {
                t.icon
            },
        );
        if resp.clicked() {
            patch = Some(json!({"rating": star_click(f.rating, n)}));
        }
        x += STEP;
    }
    x += GROUP_GAP;
    // colour labels: several can be on (any of them matches), as in the filter bar
    let chosen: Vec<ColorLabel> = f.labels_shown().unwrap_or_default();
    for l in ColorLabel::ALL {
        let on = chosen.contains(&l);
        let name = format!("{l:?}").to_lowercase();
        let tip = crate::i18n::tr_format!("{} label (click more labels to show any of them)", crate::i18n::color_label(&app.session.catalog, l));
        let (r, resp) = cell(x, &format!("label-{name}"), &tip, on);
        let c = super::filterbar::label_color(l);
        p.rect_filled(r.shrink(3.5), 1.5, if on || resp.hovered() { c } else { c.gamma_multiply(0.85) });
        if resp.clicked() {
            let mut next = chosen.clone();
            if on {
                next.retain(|x| *x != l);
            } else {
                next.push(l);
            }
            next.sort();
            let names: Vec<String> = next.iter().map(|x| format!("{x:?}").to_lowercase()).collect();
            patch = Some(json!({"label": Value::Null, "labels": names}));
        }
        x += STEP;
    }
    if let Some(patch) = patch {
        let _ = app.run("library.filter", patch);
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_operator_cycles_and_a_second_star_click_clears() {
        assert_eq!(next_op(RatingOp::AtLeast), "exactly");
        assert_eq!(next_op(RatingOp::Exactly), "atMost");
        assert_eq!(next_op(RatingOp::AtMost), "atLeast");
        assert_eq!(star_click(0, 3), 3);
        assert_eq!(star_click(3, 3), 0);
        assert_eq!(star_click(2, 200), 5);
    }
}
