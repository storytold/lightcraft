//! Reordering in the smart-album rule editor ([`super::rules_editor`]): every rule and every group
//! has a grip at the start of its row. Dragging the grip moves the row, with a line showing where it
//! would land; Esc gives the drag up. With the keyboard, Tab reaches the grip and ↑ ↓ move the row
//! one place (out of its group at the group's ends), → moves it into the group next to it and ←
//! out of its group. Where a row sits is a path of positions from the top list (`[1, 0]`: the first
//! rule of the second rule's group); a drop is a path too: the group's path and the position among
//! its rules, counted before the row leaves its old place.
//!
//! The moves themselves are [`super::rules_move`]'s; this module draws and reads the editor's side of
//! them.

use egui::{Color32, CursorIcon, Id, Key, Modifiers, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use lightcraft_catalog::rules::{Kind, album_rule_id, bool_label, bool_value, choice_label, field_kind, field_label, ops_for};
use lightcraft_catalog::{Match, RuleSet};
use serde_json::Value;

use crate::album_picker::AlbumEntry;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::register;

use super::rules_move::{Dir, Refusal, Verdict, judge, key_dest, move_node};

/// How wide a grip is; its height is a row's.
const GRIP_W: f32 = 16.0;
/// A group frame's top and bottom strip (its padding and a little more): a pointer there is beside
/// the group, between its siblings, not inside it.
const EDGE: f32 = 8.0;
/// How far past the list's visible part a drag still counts as over it.
const REACH: f32 = 24.0;

// ------------------------------------------------------------------ where a drag would drop

/// Where one row of the editor was drawn this frame.
#[derive(Clone, Debug)]
pub struct RowRec {
    pub path: Vec<usize>,
    pub rect: Rect,
    pub group: bool,
}

/// The rows of the list at `list`, in order.
fn children<'a>(rows: &'a [RowRec], list: &[usize]) -> Vec<&'a RowRec> {
    let mut kids: Vec<&RowRec> = rows.iter().filter(|r| r.path.len() == list.len() + 1 && r.path.starts_with(list)).collect();
    kids.sort_by_key(|r| r.path.last().copied().unwrap_or(0));
    kids
}

/// The place a row dragged to height `y` would drop: in the innermost group the pointer is inside
/// of (not on its top or bottom strip: those are beside it), before the first row whose middle is
/// below the pointer, else after the last.
pub fn slot_at(rows: &[RowRec], y: f32) -> Vec<usize> {
    let mut list: Vec<usize> = Vec::new();
    loop {
        let kids = children(rows, &list);
        if let Some(group) = kids.iter().find(|r| r.group && y > r.rect.top() + EDGE && y < r.rect.bottom() - EDGE) {
            list = group.path.clone();
            continue;
        }
        let at = kids.iter().find(|r| y < r.rect.center().y).and_then(|r| r.path.last().copied()).unwrap_or(kids.len());
        list.push(at);
        return list;
    }
}

/// The line showing a drop at `dest`: where it starts and ends along x, and its height.
fn gap_line(rows: &[RowRec], dest: &[usize]) -> Option<(f32, f32, f32)> {
    let (&at, list) = dest.split_last()?;
    let kids = children(rows, list);
    let first = kids.first()?;
    let before = kids.iter().rev().find(|r| r.path.last().is_some_and(|&i| i < at));
    let after = kids.iter().find(|r| r.path.last().is_some_and(|&i| i >= at));
    let y = match (before, after) {
        (Some(b), Some(a)) => (b.rect.bottom() + a.rect.top()) / 2.0,
        (None, Some(a)) => a.rect.top() - 2.0,
        (Some(b), None) => b.rect.bottom() + 2.0,
        (None, None) => return None,
    };
    Some((first.rect.left(), first.rect.right(), y))
}

// ------------------------------------------------------------------ the editor's side

#[derive(Clone, Default)]
struct Drag {
    from: Vec<usize>,
    /// What the dragged row says in the pill that follows the pointer.
    label: String,
    /// The rules when the drag was first seen: a drag whose rules were replaced (another dialog) or
    /// edited meanwhile (typing in a field that kept focus) is given up, its path would point into
    /// other rules; the edit stays.
    base: Option<RuleSet>,
}

/// What the rows record while they are drawn, for the end of the frame to act on (kept in the
/// context: the rows are drawn by nested calls).
#[derive(Clone, Default)]
struct State {
    rows: Vec<RowRec>,
    drag: Option<Drag>,
    /// An arrow key pressed on the grip of this row.
    key_move: Option<(Vec<usize>, Dir)>,
    /// The grip to bring into view (it took the focus after a move).
    refocus: Option<Id>,
}

fn with_state<R>(ctx: &egui::Context, f: impl FnOnce(&mut State) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<State>(Id::unique("lc-rule-reorder"))))
}

/// The salt of the row at `path` under the editor whose top list has salt `root` (what `edit` makes
/// for each row).
fn row_salt(root: &str, path: &[usize]) -> String {
    path.iter().fold(root.to_string(), |salt, i| format!("{salt}-{i}"))
}

pub(crate) fn grip_id(salt: &str) -> Id {
    Id::unique(("ruleGrip", salt))
}

/// Starts a frame of the editor (its top list): forgets last frame's rows.
pub fn begin(ctx: &egui::Context) {
    with_state(ctx, |s| {
        s.rows.clear();
        s.key_move = None;
    });
}

/// Says where the row at `path` was drawn.
pub fn record(ctx: &egui::Context, path: &[usize], rect: Rect, group: bool) {
    with_state(ctx, |s| s.rows.push(RowRec { path: path.to_vec(), rect, group }));
}

/// A rule's or group's row, as its grip needs to know it.
pub struct Row<'a> {
    /// The row's salt (its widget ids; `ruleGrip:<salt>` for tests and agents).
    pub salt: &'a str,
    pub path: &'a [usize],
    /// How many rows are in its list.
    pub count: usize,
    /// For a group: how it matches and how many rows it holds.
    pub group: Option<(Match, usize)>,
    /// What the row says (a rule in words, a group's heading), for the pill and the screen reader;
    /// worked out only when needed.
    pub label: &'a dyn Fn() -> String,
}

fn match_word(m: Match) -> &'static str {
    match m {
        Match::All => "all",
        Match::Any => "any",
        Match::None => "none",
    }
}

/// A group's heading in words, as the editor shows it above the group's rules.
pub fn group_heading(m: Match) -> String {
    use crate::i18n::tr;
    format!("{} {} {}", tr("Match"), tr(match_word(m)), tr("of the following rules:"))
}

/// What a screen reader says for the grip of `row`.
pub(crate) fn spoken(row: &Row) -> String {
    let n = row.path.last().map_or(1, |i| i.saturating_add(1));
    match row.group {
        Some((mode, inside)) => {
            crate::i18n::tr_format!(
                "Group {n} of {m}: match {mode}, {count} inside. Move with the arrow keys",
                n = n,
                m = row.count,
                mode = crate::i18n::tr(match_word(mode)),
                count = inside
            )
        }
        None => crate::i18n::tr_format!("Rule {n} of {m}: {what}. Move with the arrow keys", n = n, m = row.count, what = (row.label)()),
    }
}

/// The grip at the start of a row: drag it, or Tab to it and use the arrow keys. While its row is
/// dragged the rest of the row fades (call it first in the row).
pub fn grip(ui: &mut Ui, row: &Row) {
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let (rect, _) = ui.allocate_exact_size(vec2(GRIP_W, ui.spacing().interact_size.y), Sense::hover());
    let id = grip_id(row.salt);
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    register(ui, format!("ruleGrip:{}", row.salt), rect);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, spoken(row)));
    let (dragging_this, dragging) = with_state(&ctx, |s| (s.drag.as_ref().is_some_and(|d| d.from == row.path), s.drag.is_some()));
    if resp.drag_started_by(egui::PointerButton::Primary) {
        let label = (row.label)();
        with_state(&ctx, |s| s.drag = Some(Drag { from: row.path.to_vec(), label, base: None }));
        // the dragged row's grip takes the focus, so after the drop it follows the row (a focus left
        // on another row's grip or field would stay at its old place, on whichever row lands there)
        resp.request_focus();
    }
    if with_state(&ctx, |s| s.refocus.take_if(|f| *f == id)).is_some() {
        resp.scroll_to_me(None);
    }
    let focused = resp.has_focus();
    if focused && !dragging {
        // the arrow keys are this widget's: not the focus moving to the next one
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(id, egui::EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false })
        });
        let pressed = ui.input_mut(|i| {
            [(Key::ArrowUp, Dir::Up), (Key::ArrowDown, Dir::Down), (Key::ArrowLeft, Dir::Out), (Key::ArrowRight, Dir::In)]
                .into_iter()
                .find(|(key, _)| i.consume_key(Modifiers::NONE, *key))
                .map(|(_, dir)| dir)
        });
        if let Some(dir) = pressed {
            with_state(&ctx, |s| s.key_move = Some((row.path.to_vec(), dir)));
        }
    }
    let color = if dragging_this {
        t.accent
    } else if resp.hovered() || focused {
        t.text
    } else {
        t.text_dim
    };
    paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(14.0, 14.0)), Icon::Grip, color);
    if focused {
        ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
    }
    if resp.hovered() && !dragging {
        ctx.set_cursor_icon(CursorIcon::Grab);
        resp.on_hover_text(crate::i18n::tr("Drag to move, or use the arrow keys"));
    }
    if dragging_this {
        ui.multiply_opacity(0.45);
    }
}

/// The end of the editor's frame (its top list, `salt`, being `rs`): does what the grips asked for.
/// A key press moves its row; a drag shows where it would land and moves it on release.
pub fn finish(ui: &mut Ui, rs: &mut RuleSet, salt: &str) {
    let ctx = ui.ctx().clone();
    let (rows, key_move, drag) = with_state(&ctx, |s| (std::mem::take(&mut s.rows), s.key_move.take(), s.drag.clone()));
    if let Some((from, dir)) = key_move {
        if let Some(dest) = key_dest(rs, &from, dir)
            && let Some(at) = move_node(rs, &from, &dest)
        {
            // the row's grip is where the focus follows it; it is drawn at its new place next frame
            let id = grip_id(&row_salt(salt, &at));
            ctx.memory_mut(|m| m.request_focus(id));
            with_state(&ctx, |s| s.refocus = Some(id));
            ctx.request_repaint();
        }
        return;
    }
    let Some(mut drag) = drag else { return };
    match &drag.base {
        None => {
            drag.base = Some(rs.clone());
            with_state(&ctx, |s| s.drag = Some(drag.clone()));
        }
        Some(base) if base != rs => {
            with_state(&ctx, |s| s.drag = None);
            return;
        }
        Some(_) => {}
    }
    let end = |ctx: &egui::Context| with_state(ctx, |s| s.drag = None);
    let (released, down, pos, esc) =
        ui.input(|i| (i.pointer.primary_released(), i.pointer.primary_down(), i.pointer.latest_pos(), i.key_pressed(Key::Escape)));
    if esc {
        // this Esc is the drag's, not the dialog's
        crate::widgets::take_escape(&ctx);
        end(&ctx);
        return;
    }
    if !down && !released {
        end(&ctx);
        return;
    }
    let view = ui.clip_rect();
    let Some(pos) = pos else { return };
    // near the top or bottom edge the list scrolls, so rows beyond the visible part can be reached
    if pos.x >= view.left() && pos.x <= view.right() {
        let speed = crate::panels::left::auto_scroll_speed(pos.y, view.top(), view.bottom());
        if speed != 0.0 {
            ui.scroll_with_delta_animation(vec2(0.0, -speed * ui.input(|i| i.stable_dt).min(0.1)), egui::style::ScrollAnimation::none());
            ctx.request_repaint();
        }
    }
    let over = view.expand(REACH).contains(pos).then(|| slot_at(&rows, pos.y));
    let verdict = over.as_ref().map(|dest| judge(rs, &drag.from, dest));
    if released {
        if let (Some(dest), Some(Verdict::Moves)) = (&over, verdict)
            && let Some(at) = move_node(rs, &drag.from, dest)
            && ctx.memory(|m| m.has_focus(grip_id(&row_salt(salt, &drag.from))))
        {
            // the grip that had the focus is drawn at the row's new place next frame: the focus follows it
            let id = grip_id(&row_salt(salt, &at));
            ctx.memory_mut(|m| m.request_focus(id));
            with_state(&ctx, |s| s.refocus = Some(id));
            ctx.request_repaint();
        }
        end(&ctx);
        return;
    }
    let t = Tokens::get(&ctx);
    let refused = match verdict {
        Some(Verdict::Refused(why)) => Some(why),
        _ => None,
    };
    ctx.set_cursor_icon(if refused.is_some() { CursorIcon::NotAllowed } else { CursorIcon::Grabbing });
    if let (Some(dest), Some(Verdict::Moves | Verdict::Refused(_))) = (&over, verdict)
        && let Some((x0, x1, y)) = gap_line(&rows, dest)
    {
        let color = if refused.is_some() { t.caution } else { t.accent };
        ui.painter().circle_stroke(pos2(x0, y), 3.0, Stroke::new(1.5, color));
        ui.painter().line_segment([pos2(x0 + 3.0, y), pos2(x1.max(x0 + 3.0), y)], Stroke::new(2.0, color));
    }
    let (text, fill, ink, edge) = match refused {
        Some(Refusal::IntoItself) => (crate::i18n::tr("A group can't go inside itself").to_string(), t.chrome, t.caution, t.caution),
        Some(Refusal::TooDeep) => (crate::i18n::tr("Groups can't nest any deeper").to_string(), t.chrome, t.caution, t.caution),
        None => (drag.label.clone(), t.accent, Color32::WHITE, t.accent),
    };
    egui::Area::new(Id::unique("drag-rule")).order(egui::Order::Tooltip).interactable(false).fixed_pos(pos + vec2(14.0, 10.0)).show(&ctx, |ui| {
        egui::Frame::NONE.fill(fill).stroke(Stroke::new(1.0, edge)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).color(ink).font(t.semibold(12.0))).wrap_mode(egui::TextWrapMode::Extend));
        });
    });
}

// ------------------------------------------------------------------ a rule in words

/// A number as the editor shows it: `3`, not `3.0`; a fraction as it is (`2.8`).
fn number_text(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 { format!("{n:.0}") } else { n.to_string() }
}

/// A rule value as a person would read it.
fn value_text(field: &str, value: &Value) -> String {
    use crate::i18n::tr;
    match value {
        Value::String(s) => choice_label(field, s).map_or_else(|| s.clone(), |l| tr(l).to_string()),
        Value::Null => String::new(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), number_text),
        Value::Bool(b) => tr(bool_label(*b)).to_string(),
        Value::Array(a) => a.iter().map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string)).collect::<Vec<_>>().join(" – "),
        // as the editor's "in the last" row shows it: n as the catalog reads it (a number, or text), the unit
        // lowercased and in the editor's words
        Value::Object(o) => {
            let n = o.get("n").and_then(|n| n.as_f64().or_else(|| n.as_str().and_then(|t| t.trim().parse().ok()))).unwrap_or(30.0);
            let unit = o.get("unit").and_then(Value::as_str).unwrap_or("days").to_lowercase();
            format!("{} {}", number_text(n.round()), tr(&unit))
        }
    }
}

/// A field rule in words, in the language of the editor: `Rating ≥ 3`. `albums` names an Album
/// rule's album.
pub fn rule_text(field: &str, op: &str, value: &Value, albums: &[AlbumEntry]) -> String {
    use crate::i18n::tr;
    let kind = field_kind(field);
    let name = field_label(field).map_or_else(|| field.to_string(), |l| tr(l).to_string());
    let op = kind.and_then(|k| ops_for(k).iter().find(|o| o.0 == op)).map_or_else(|| op.to_string(), |o| tr(o.1).to_string());
    let value = match kind {
        Some(Kind::Album) => album_rule_id(value)
            .map(|id| albums.iter().find(|a| a.id == id.0).map_or_else(|| format!("#{}", id.0), |a| a.name.clone()))
            .unwrap_or_default(),
        Some(Kind::Bool) => bool_value(value).map_or_else(|| value.to_string(), |b| tr(bool_label(b)).to_string()),
        _ => value_text(field, value),
    };
    [name, op, value].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(path: &[usize], top: f32, bottom: f32, group: bool) -> RowRec {
        RowRec { path: path.to_vec(), rect: Rect::from_min_max(pos2(0.0, top), pos2(100.0, bottom)), group }
    }

    /// 0: rule (0–20), 1: group (24–100) holding two rules (40–60, 64–90), 2: rule (104–124)
    fn rows() -> Vec<RowRec> {
        vec![
            row(&[0], 0.0, 20.0, false),
            row(&[1, 0], 40.0, 60.0, false),
            row(&[1, 1], 64.0, 90.0, false),
            row(&[1], 24.0, 100.0, true),
            row(&[2], 104.0, 124.0, false),
        ]
    }

    #[test]
    fn a_drag_drops_in_the_list_under_the_pointer() {
        let rows = rows();
        assert_eq!(slot_at(&rows, 5.0), [0], "above the middle of the first rule");
        assert_eq!(slot_at(&rows, 15.0), [1], "below it");
        assert_eq!(slot_at(&rows, 26.0), [1], "the group's top strip is beside the group: before it");
        assert_eq!(slot_at(&rows, 34.0), [1, 0], "its header line is inside it, before its first rule");
        assert_eq!(slot_at(&rows, 55.0), [1, 1], "in its first rule's lower half");
        assert_eq!(slot_at(&rows, 85.0), [1, 2], "in its last rule's lower half");
        assert_eq!(slot_at(&rows, 98.0), [2], "the bottom strip is beside the group: after it");
        assert_eq!(slot_at(&rows, 500.0), [3], "below everything");
        assert_eq!(slot_at(&rows, -50.0), [0], "above everything");
    }

    #[test]
    fn the_drop_line_sits_in_the_gap() {
        let rows = rows();
        assert_eq!(gap_line(&rows, &[0]), Some((0.0, 100.0, -2.0)));
        assert_eq!(gap_line(&rows, &[1]), Some((0.0, 100.0, 22.0)), "between the rule and the group");
        assert_eq!(gap_line(&rows, &[1, 1]), Some((0.0, 100.0, 62.0)), "inside the group");
        assert_eq!(gap_line(&rows, &[3]), Some((0.0, 100.0, 126.0)));
        assert_eq!(gap_line(&rows, &[7, 0]), None);
    }

    #[test]
    fn rules_read_in_words() {
        let none: [AlbumEntry; 0] = [];
        assert_eq!(rule_text("rating", "gte", &json!(3), &none), "Rating is ≥ 3");
        assert_eq!(rule_text("edited", "is", &json!(true), &none), "Has Edits is Yes");
        let album = AlbumEntry { id: 7, name: "Trip".into(), parent: None, depth: 0, folder: false, smart: false, blocked: None };
        assert!(rule_text("album", "is", &json!(7), std::slice::from_ref(&album)).ends_with("Trip"));
        assert!(rule_text("album", "is", &json!(9), &[album]).ends_with("#9"));
        assert_eq!(rule_text("nonsense", "x", &Value::Null, &none), "nonsense x");
    }

    #[test]
    fn numbers_and_units_read_as_the_editor_shows_them() {
        let none: [AlbumEntry; 0] = [];
        let text = |f: &str, op: &str, v: Value| rule_text(f, op, &v, &none);
        crate::i18n::set_language(crate::i18n::Locale::En);
        assert_eq!(text("rating", "gte", json!(3.0)), "Rating is ≥ 3");
        assert_eq!(text("aperture", "lte", json!(2.8)), "Aperture is ≤ 2.8");
        for n in [json!(30.0), json!("30"), json!(30)] {
            assert_eq!(text("captureDate", "inLast", json!({"n": n, "unit": "days"})), "Capture Date is in the last 30 days");
        }
        crate::i18n::set_language(crate::i18n::Locale::De);
        let de = text("captureDate", "inLast", json!({"n": 7.0, "unit": "Weeks"}));
        let de_str = text("captureDate", "inLast", json!({"n": "7", "unit": "weeks"}));
        crate::i18n::set_language(crate::i18n::Locale::En);
        assert_eq!(de, de_str);
        assert!(de.ends_with(" 7 Wochen") && !de.contains("7.0") && !de.contains('"'), "{de}");
    }

    #[test]
    fn row_salts_follow_the_editors() {
        assert_eq!(row_salt("rules", &[]), "rules");
        assert_eq!(row_salt("rules", &[2, 1]), "rules-2-1");
    }
}
