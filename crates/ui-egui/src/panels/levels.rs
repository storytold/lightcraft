//! The Levels section (beyond Lightroom, Capture One's tool of that name), laid out as one compact block: the output
//! levels' boxes and the channel tabs on top, the histogram of what the Levels receive as thin colour lines with
//! the markers on its edges (output ones on the top edge, input black, midtone and white on the bottom edge), and
//! the input boxes and pickers under it. Reset is the icon in the section's header. Every edit goes through
//! `develop.levels`, so a drag is one undo step, the boxes and the markers are one state, and an agent sees the
//! same values.

use std::hash::{Hash, Hasher};

use egui::{Align, Color32, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_develop::{ChannelLevels, DevelopSettings};
use serde_json::json;

use crate::LccApp;
use crate::icons::Icon;
use crate::theme::Tokens;
use crate::widgets::{hex, register};

const TABS: [(&str, &str); 4] = [("RGB", "master"), ("Red", "red"), ("Green", "green"), ("Blue", "blue")];

/// Height of the histogram plot, of a strip of markers, and of a row of boxes (screen points).
const PLOT_H: f32 = 96.0;
const STRIP_H: f32 = 12.0;
const ROW_H: f32 = 28.0;
/// Side margins of the block.
const SIDE: f32 = 16.0;
/// Width of a number box.
const BOX_W: f32 = 40.0;
/// How close (screen points) to a marker a press grabs it.
const GRAB: f32 = 12.0;

/// The tab shown (an index into [`TABS`]), kept with the window rather than the photo.
fn tab_of(ui: &egui::Ui) -> usize {
    ui.data(|d| d.get_temp::<usize>(egui::Id::new("levels-tab"))).unwrap_or(0).min(TABS.len() - 1)
}

fn tint(channel: &str) -> Color32 {
    match channel {
        "red" => hex("#dd3333"),
        "green" => hex("#33bb55"),
        "blue" => hex("#3377ee"),
        _ => Color32::from_gray(225),
    }
}

/// The photo's histogram as the Levels receive it, counted again only when something before them changed (not while
/// the Levels themselves move, and not during a drag of another slider: the last one stays until it ends).
fn input_histogram(app: &mut LccApp, ui: &egui::Ui, id: PhotoId, d: &DevelopSettings) -> Option<lcc_raster::Histogram> {
    let mut before = d.clone();
    before.levels = Default::default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{id:?}").hash(&mut h);
    before.hash64().hash(&mut h);
    let key = h.finish();
    let cache = egui::Id::new("levels-input-histogram");
    let cached = ui.data(|dd| dd.get_temp::<(u64, Option<lcc_raster::Histogram>)>(cache));
    match cached {
        Some((k, hist)) if k == key => hist,
        Some((_, hist)) if app.session.interaction.is_some() => hist,
        _ => {
            let hist = lcc_engine::cmd::levels::input_histogram(&mut app.session).ok();
            ui.data_mut(|dd| dd.insert_temp(cache, (key, hist.clone())));
            hist
        }
    }
}

/// A box for a number: shows `shown`, and returns the number typed when the edit is committed. The text being typed
/// lives with the field until the edit ends. A comma counts as the decimal point. `name` is the box's accessible
/// name and tooltip (`more` adds a second line to the tooltip).
fn number_box(ui: &mut egui::Ui, widget: &str, shown: String, name: &str, more: Option<&str>) -> Option<f64> {
    use crate::text_field::TextField;
    let t = Tokens::get(ui.ctx());
    let buf_id = egui::Id::new(("levels-field", widget));
    let focused = ui.memory(|m| m.has_focus(crate::text_field::id(widget)));
    let mut text = if focused { ui.data(|d| d.get_temp::<String>(buf_id)).unwrap_or_else(|| shown.clone()) } else { shown };
    let r = TextField::singleline(widget, &mut text).width(BOX_W).font(t.font(12.0)).select_on_focus(true).label(crate::i18n::tr(name)).show(ui);
    register(ui.ctx(), format!("levels:{widget}"), r.response.rect);
    let tip = match more {
        Some(more) => format!("{}\n{}", crate::i18n::tr(name), crate::i18n::tr(more)),
        None => crate::i18n::tr(name).to_string(),
    };
    r.response.clone().on_hover_text(tip);
    let mut out = None;
    if r.committed() {
        out = text.trim().replace(',', ".").parse::<f64>().ok().filter(|n| n.is_finite());
        ui.data_mut(|d| d.remove::<String>(buf_id));
    } else if r.cancelled() {
        ui.data_mut(|d| d.remove::<String>(buf_id));
    } else if focused || r.response.has_focus() {
        ui.data_mut(|d| d.insert_temp(buf_id, text));
    }
    out
}

/// A small triangle whose tip is at `tip`, pointing up (`up`, a marker under the plot) or down (over it).
fn marker(p: &egui::Painter, tip: Pos2, up: bool, fill: Color32, lit: bool) {
    let dy = if up { STRIP_H - 2.0 } else { -(STRIP_H - 2.0) };
    let pts = vec![tip, pos2(tip.x + 5.5, tip.y + dy), pos2(tip.x - 5.5, tip.y + dy)];
    let edge = if lit { Color32::WHITE } else { Color32::from_gray(150) };
    p.add(egui::Shape::convex_polygon(pts, fill, Stroke::new(1.0, edge)));
}

/// The strip's markers `xs` (0..255 positions, in the order they are grabbed): which one a press at `q` takes.
fn nearest(strip: Rect, xs: &[f64], q: Pos2) -> Option<usize> {
    let to_x = |v: f64| strip.left() + (v / 255.0) as f32 * strip.width();
    xs.iter()
        .enumerate()
        .map(|(i, v)| (i, (to_x(*v) - q.x).abs()))
        .filter(|(_, dist)| *dist <= GRAB)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// One strip of markers over `rect`'s width: press and drag one (one undo step), double-click one to bring it back.
/// `xs` are the markers' places on the 0..255 scale; `set(i, position)` writes marker `i` dragged to `position`;
/// `reset(i)` writes it back to its neutral place. `state` names the drag.
fn strip(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    rect: Rect,
    state: &str,
    xs: &[f64],
    set: impl Fn(&mut LccApp, usize, f64),
    reset: impl Fn(&mut LccApp, usize),
) -> Option<usize> {
    let resp = ui.interact(rect.expand2(vec2(10.0, 0.0)), egui::Id::new(("levels-strip", state)), Sense::click_and_drag());
    let drag_id = egui::Id::new(("levels-drag", state));
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp::<Option<usize>>(drag_id)).flatten();
    let press = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
    if resp.double_clicked()
        && let Some(i) = resp.interact_pointer_pos().and_then(|q| nearest(rect, xs, q))
    {
        reset(app, i);
    } else if resp.drag_started()
        && let Some(i) = press.and_then(|q| nearest(rect, xs, q))
    {
        app.ui.put_away_picker();
        let _ = app.run("develop.beginInteraction", json!({"label": "Levels"}));
        dragging = Some(i);
    } else if resp.dragged()
        && let (Some(i), Some(q)) = (dragging, resp.interact_pointer_pos())
    {
        set(app, i, f64::from(((q.x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * 255.0);
    }
    if super::edit::commit_interaction(app, ui, dragging.is_some(), resp.drag_stopped()) {
        dragging = None;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, dragging));
    // the same left-right cursor as the sliders, on hover and while dragging
    if dragging.is_some() || resp.hover_pos().and_then(|q| nearest(rect, xs, q)).is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    dragging
}

/// The histogram as thin lines (no fill): the red, green and blue ones and the luminance on the RGB tab, the
/// channel's own on another. All are scaled to the tallest, with a square root as the Tone Curve's backdrop has.
fn histogram_lines(p: &egui::Painter, plot: Rect, h: &lcc_raster::Histogram, ch: &str) {
    let series: Vec<(&[u32], Color32)> = if ch == "master" {
        vec![(&h.luma, Color32::from_gray(190)), (&h.r, tint("red")), (&h.g, tint("green")), (&h.b, tint("blue"))]
    } else {
        vec![(
            match ch {
                "red" => &h.r,
                "green" => &h.g,
                _ => &h.b,
            },
            tint(ch),
        )]
    };
    let bins = h.hdr.map_or(256, |hdr| hdr.from.clamp(2, 256));
    let smooth = |s: &[u32]| -> Vec<f32> {
        (0..256)
            .map(|i| {
                let j = i * (bins - 1) / 255;
                let (a, b) = (j.saturating_sub(1), (j + 2).min(bins));
                (a..b).map(|k| s.get(k).copied().unwrap_or(0) as f32).sum::<f32>() / (b - a) as f32
            })
            .collect()
    };
    let lines: Vec<(Vec<f32>, Color32)> = series.iter().map(|(s, c)| (smooth(s), *c)).collect();
    let peak = lines.iter().flat_map(|(v, _)| v.iter().copied()).fold(1.0_f32, f32::max);
    for (v, color) in lines {
        let pts: Vec<Pos2> = v
            .iter()
            .enumerate()
            .map(|(i, n)| pos2(plot.left() + plot.width() * i as f32 / 255.0, plot.bottom() - plot.height() * 0.96 * (n / peak).sqrt()))
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.0, color.gamma_multiply(0.9))));
    }
}

/// A number box with the key it edits: the key and the number typed, when the edit is committed.
fn keyed_box(ui: &mut egui::Ui, ch: &str, key: &str, shown: String, name: &str, more: Option<&str>) -> Option<(String, f64)> {
    number_box(ui, &format!("{ch}.{key}"), shown, name, more).map(|n| (key.to_string(), n))
}

pub(crate) fn levels(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let tab = tab_of(ui);
    let ch = TABS.get(tab).map_or("master", |(_, c)| *c);
    // the picker belongs to the RGB tab: another tab puts it away
    if ch != "master" && matches!(app.ui.tool.as_str(), "levelsBlack" | "levelsWhite") {
        app.ui.tool.clear();
    }
    let c: ChannelLevels = d.levels.channel(ch).copied().unwrap_or_default();
    let w = ui.available_width();
    let mut typed: Vec<(String, f64)> = Vec::new();

    // top row: output black, the channel tabs, output white
    let (top_outer, _) = ui.allocate_exact_size(vec2(w, ROW_H + 8.0), Sense::hover());
    let top = Rect::from_min_max(pos2(top_outer.left() + SIDE, top_outer.top() + 6.0), pos2(top_outer.right() - SIDE, top_outer.bottom() - 2.0));
    ui.scope_builder(UiBuilder::new().max_rect(top).layout(Layout::left_to_right(Align::Center)), |ui| {
        typed.extend(keyed_box(ui, ch, "outBlack", format!("{}", c.out_black.round()), "Output Black", None));
    });
    ui.scope_builder(UiBuilder::new().max_rect(top).layout(Layout::right_to_left(Align::Center)), |ui| {
        typed.extend(keyed_box(ui, ch, "outWhite", format!("{}", c.out_white.round()), "Output White", None));
    });
    // the tabs: plain text, an accent line under the active one
    let font = t.semibold(11.5);
    let galleys: Vec<_> =
        TABS.iter().map(|(label, _)| ui.painter().layout_no_wrap(crate::i18n::tr(label).to_string(), font.clone(), t.text)).collect();
    let gap = 9.0;
    let total = galleys.iter().map(|g| g.size().x).sum::<f32>() + gap * (TABS.len() as f32 - 1.0);
    let mut x = top.center().x - total / 2.0;
    for (i, ((label, value), g)) in TABS.iter().zip(galleys).enumerate() {
        let r = Rect::from_min_size(pos2(x, top.center().y - 11.0), vec2(g.size().x, 22.0));
        let resp = ui.interact(r.expand2(vec2(5.0, 0.0)), egui::Id::new(("levels-tab-button", value)), Sense::click());
        crate::widgets::announce_selected(&resp, crate::i18n::tr(label), i == tab);
        register(ui.ctx(), format!("button:levelsTab-{value}"), r);
        let active = i == tab;
        let color = if active {
            t.accent
        } else if resp.hovered() {
            t.text
        } else {
            t.text_label
        };
        let p = ui.painter();
        p.galley(pos2(r.left(), r.center().y - g.size().y / 2.0), g.clone(), color);
        if active {
            p.line_segment([r.left_bottom() + vec2(0.0, 1.0), r.right_bottom() + vec2(0.0, 1.0)], Stroke::new(2.0, t.accent));
        }
        if resp.clicked() {
            ui.data_mut(|dd| dd.insert_temp(egui::Id::new("levels-tab"), i));
        }
        x += g.size().x + gap;
    }

    // the plot, with the output markers over its top edge and the input markers under its bottom edge
    let (outer, _) = ui.allocate_exact_size(vec2(w, PLOT_H + 2.0 * STRIP_H + 4.0), Sense::hover());
    let plot = Rect::from_min_size(pos2(outer.left() + SIDE, outer.top() + STRIP_H + 2.0), vec2((w - 2.0 * SIDE).max(80.0), PLOT_H));
    let top_strip = Rect::from_min_size(pos2(plot.left(), plot.top() - STRIP_H), vec2(plot.width(), STRIP_H));
    let bottom_strip = Rect::from_min_size(plot.left_bottom(), vec2(plot.width(), STRIP_H));
    register(ui.ctx(), "levels:plot", plot);
    ui.painter().rect_filled(plot, 2.0, t.canvas);
    for i in 1..4 {
        let x = plot.left() + plot.width() * i as f32 / 4.0;
        ui.painter().line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0, Color32::from_gray(44)));
    }
    if let Some(h) = input_histogram(app, ui, id, d) {
        histogram_lines(ui.painter(), plot, &h, ch);
    }
    let to_x = |v: f64| plot.left() + (v.clamp(0.0, 255.0) / 255.0) as f32 * plot.width();
    let mid = c.midtone_position();
    // input and output stay connected: a thin line runs from each output marker (top edge) to its input marker
    // (bottom edge); the midtone's is straight and ends in a small dot at the top
    let line = Stroke::new(1.0, Color32::from_white_alpha(150));
    for (out, inp) in [(c.out_black, c.black), (c.out_white, c.white)] {
        ui.painter().line_segment([pos2(to_x(out), plot.top()), pos2(to_x(inp), plot.bottom())], line);
    }
    let dot = pos2(plot.center().x, plot.top() - STRIP_H / 2.0);
    ui.painter().line_segment([dot, pos2(to_x(mid), plot.bottom())], line);
    ui.painter().circle_filled(dot, 2.5, Color32::from_gray(210));

    // output markers: no midtone
    register(ui.ctx(), "levels:outStrip", top_strip);
    let out_drag = strip(
        app,
        ui,
        top_strip,
        "output",
        &[c.out_black, c.out_white],
        |app, i, pos| {
            let key = if i == 0 { "outBlack" } else { "outWhite" };
            let _ = app.run("develop.levels", json!({"channel": ch, (key): pos.round()}));
        },
        |app, i| {
            let (key, v) = if i == 0 { ("outBlack", 0.0) } else { ("outWhite", 255.0) };
            let _ = app.run("develop.levels", json!({"channel": ch, (key): v}));
        },
    );
    // input markers: black, midtone, white
    register(ui.ctx(), "levels:strip", bottom_strip);
    let centre = (c.black + c.white) / 2.0;
    let in_drag = strip(
        app,
        ui,
        bottom_strip,
        "input",
        &[c.black, mid, c.white],
        |app, i, pos| {
            let _ = match i {
                0 => app.run("develop.levels", json!({"channel": ch, "black": pos.round()})),
                2 => app.run("develop.levels", json!({"channel": ch, "white": pos.round()})),
                _ => {
                    // near the neutral place the marker sticks to it, so 0 is easy to hit
                    let at = if (pos - centre).abs() < 0.75 { centre.round() } else { pos };
                    app.run("develop.levels", json!({"channel": ch, "marker": at}))
                }
            };
        },
        |app, i| {
            let (key, v) = match i {
                0 => ("black", 0.0),
                2 => ("white", 255.0),
                _ => ("midtone", 0.0),
            };
            let _ = app.run("develop.levels", json!({"channel": ch, (key): v}));
        },
    );
    let p = ui.painter();
    marker(p, pos2(to_x(c.out_white), plot.top()), false, Color32::WHITE, out_drag == Some(1));
    marker(p, pos2(to_x(c.out_black), plot.top()), false, Color32::BLACK, out_drag == Some(0));
    marker(p, pos2(to_x(c.white), plot.bottom()), true, Color32::WHITE, in_drag == Some(2));
    marker(p, pos2(to_x(c.black), plot.bottom()), true, Color32::BLACK, in_drag == Some(0));
    marker(p, pos2(to_x(mid), plot.bottom()), true, Color32::from_gray(128), in_drag == Some(1));

    // bottom row: input black and its picker, the midtone, the white picker and input white
    let (bottom_outer, _) = ui.allocate_exact_size(vec2(w, ROW_H + 6.0), Sense::hover());
    let row = Rect::from_min_max(
        pos2(bottom_outer.left() + SIDE, bottom_outer.top() + 2.0),
        pos2(bottom_outer.right() - SIDE, bottom_outer.bottom() - 2.0),
    );
    let picker = |ui: &mut egui::Ui, app: &mut LccApp, tool: &str, tip: &str| {
        if ch != "master" {
            return;
        }
        let on = app.ui.tool == tool;
        let btn = crate::widgets::icon_button(ui, tool, Icon::Picker, vec2(26.0, 26.0), on, true, tip);
        // a small swatch tells the two pickers apart
        let dot = btn.rect.right_bottom() - vec2(6.0, 6.0);
        let fill = if tool == "levelsBlack" { Color32::BLACK } else { Color32::WHITE };
        ui.painter().circle(dot, 3.5, fill, Stroke::new(1.0, Color32::from_gray(150)));
        if on {
            // armed until Esc, a second click, the other picker or leaving the section: say so plainly
            ui.painter().rect_stroke(btn.rect.shrink(1.0), 4.0, Stroke::new(1.5, Tokens::get(ui.ctx()).accent), egui::StrokeKind::Inside);
        }
        if btn.clicked() {
            app.ui.tool = if on { String::new() } else { tool.to_string() };
        }
    };
    ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        typed.extend(keyed_box(ui, ch, "black", format!("{}", c.black.round()), "Black", None));
        picker(ui, app, "levelsBlack", "Pick the black point from the photo");
    });
    ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::right_to_left(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        typed.extend(keyed_box(ui, ch, "white", format!("{}", c.white.round()), "White", None));
        picker(ui, app, "levelsWhite", "Pick the white point from the photo");
    });
    let mid_box = Rect::from_center_size(row.center(), vec2(BOX_W, row.height()));
    ui.scope_builder(UiBuilder::new().max_rect(mid_box).layout(Layout::left_to_right(Align::Center)), |ui| {
        typed.extend(keyed_box(ui, ch, "midtone", format!("{:.2}", c.midtone), "Midtone", Some("Contrast between shadows and highlights")));
    });
    // Reset under the graph, where the Tone Curve puts its own: right-aligned text button
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(ui, egui::Margin { left: 12, right: 16, top: 2, bottom: 4 }, 2, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::widgets::text_button(ui, "levelsReset", "Reset", false).clicked() {
                    let _ = app.run("levels.reset", json!({}));
                }
            });
        });
    });

    // a number typed in any box
    if let Some((key, n)) = typed.into_iter().next() {
        app.ui.put_away_picker();
        let n = if key == "midtone" { (n * 1000.0).round() / 1000.0 } else { n.round() };
        let _ = app.run("develop.levels", json!({"channel": ch, (key.as_str()): n}));
    }
}
