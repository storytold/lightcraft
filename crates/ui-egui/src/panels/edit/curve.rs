use super::histogram::preview_histogram;
use super::sliders::control;
use super::*;

// ------------------------------------------------------------------------------ tone curve

/// A 256-point, peak-normalized display profile of the photo histogram.
/// For HDR, only the SDR portion maps to a curve's input (0..1).
fn curve_histogram_profile(h: &lightcraft_raster::Histogram, channel: &str) -> [f32; 256] {
    let source = match channel {
        "red" => &h.r,
        "green" => &h.g,
        "blue" => &h.b,
        _ => &h.luma,
    };
    let bins = h.hdr.map_or(256, |hdr| hdr.from.clamp(2, 256));
    let samples: [f32; 256] = std::array::from_fn(|i| {
        let j = i * (bins - 1) / 255;
        let start = j.saturating_sub(2);
        let end = (j + 3).min(bins);
        let sum: f32 = (start..end).map(|k| source.get(k).copied().unwrap_or(0) as f32).sum();
        sum / (end - start) as f32
    });
    let peak = samples.iter().copied().fold(1.0_f32, f32::max);
    samples.map(|v| (v / peak).sqrt())
}

/// Show the currently rendered photo's tonal distribution behind the editable
/// curve, without requesting another render or blocking pointer interactions.
fn draw_curve_histogram(p: &egui::Painter, r: Rect, h: &lightcraft_raster::Histogram, channel: &str) {
    let profile = curve_histogram_profile(h, channel);
    let color = match channel {
        "red" => hex("#dd3333"),
        "green" => hex("#33bb55"),
        "blue" => hex("#3377ee"),
        _ => Color32::from_gray(130),
    }
    .gamma_multiply(0.27);
    let mut mesh = Mesh::default();
    for (i, height) in profile.into_iter().enumerate() {
        let x = r.left() + r.width() * i as f32 / 255.0;
        let top = pos2(x, r.bottom() - r.height() * height);
        mesh.vertices.push(Vertex { pos: top, uv: Pos2::ZERO, color });
        mesh.vertices.push(Vertex { pos: pos2(x, r.bottom()), uv: Pos2::ZERO, color });
        if i > 0 {
            let k = i as u32 * 2;
            mesh.indices.extend([k - 2, k - 1, k, k - 1, k + 1, k]);
        }
    }
    p.add(mesh);
}

/// Segments the graph's curve line is drawn with.
pub(crate) const CURVE_SAMPLES: i32 = 96;

/// Close a graph drag's `develop` interaction, as one undo step.
///
/// It is closed when the drag ends *or* the pointer comes up without egui reporting `drag_stopped`
/// for this widget (released outside the window, over a popup, or on a frame the widget does not
/// run). A dangling interaction would make every later edit a silent preview — Undo would then
/// appear to do nothing at all. Returns whether the drag is over, so the caller clears its state.
fn commit_interaction(app: &mut LightcraftApp, ui: &egui::Ui, dragging: bool, stopped: bool) -> bool {
    let released = ui.input(|i| i.pointer.any_released()) || !ui.input(|i| i.pointer.primary_down());
    if !dragging || !(stopped || released) {
        return false;
    }
    let _ = app.run("develop.endInteraction", json!({}));
    true
}

/// How close (screen points) to the curve a press counts as grabbing it.
const PARAM_GRAB: f32 = 14.0;

/// The parametric curve's own drag: the graph shows the curve and nothing else, and dragging the
/// line bends the region the pointer is over, with that region's slider following on the same frame.
///
/// The whole gesture runs inside one `develop` interaction, so it is one undo step (Ctrl+Z takes the
/// gesture back, not the last frame of it), and every write goes through `develop.set`, so the
/// slider, the curve and what is drawn are the same state rather than three things kept in step.
fn param_drag(app: &mut LightcraftApp, ui: &mut egui::Ui, r: Rect, resp: &egui::Response, d: &DevelopSettings) {
    if r.width() <= 1.0 || r.height() <= 1.0 {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let model = ParametricCurve::new(&d.curve);
    let to_screen = |x: f64, y: f64| pos2(r.left() + x as f32 * r.width(), r.bottom() - y as f32 * r.height());
    let from_screen_x = |q: Pos2| ((q.x - r.left()) / r.width()).clamp(0.0, 1.0) as f64;
    // the graph x of a point, and how far it is from the drawn curve (in screen points)
    let near_curve = |q: Pos2| {
        let x = from_screen_x(q);
        to_screen(x, model.at(x)).distance(q)
    };
    let drag_id = egui::Id::unique("curve-param-drag");
    let cursor_id = egui::Id::unique("curve-param-cursor");
    // (region, x, initial_amount, start_y): the region being dragged, the graph x the drag is anchored
    // at, the region's starting amount, and the starting screen Y position.
    let mut dragged: Option<(usize, f64, f64, f32)> = ui.data(|dd| dd.get_temp::<Option<(usize, f64, f64, f32)>>(drag_id)).flatten();
    let hovered = dragged.is_none() && resp.hover_pos().is_some_and(|q| near_curve(q) <= PARAM_GRAB);
    let mut set: Option<(&'static str, f64)> = None;
    // the press, not where the pointer is once the drag threshold has passed: a press slightly off
    // the line is still a grab, and the pointer has usually left that spot by the time it starts
    let press = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
    if resp.drag_started()
        && let Some(q) = press
        && near_curve(q) <= PARAM_GRAB
    {
        let x = from_screen_x(q);
        let _ = app.run("develop.beginInteraction", json!({"label": "Parametric Curve"}));
        let region = model.region_at(x);
        let initial_amount = model.amounts.get(region).copied().unwrap_or(0.0);
        let start_pos = to_screen(x, model.at(x));
        dragged = Some((region, x, initial_amount, start_pos.y));
        ui.data_mut(|dd| dd.insert_temp(cursor_id, start_pos));
    }
    if let Some((region, x, initial_amount, start_y)) = dragged
        && resp.dragged()
    {
        // Follow the pointer's motion from where the curve was grabbed, so the press itself moves
        // nothing and the line tracks the hand. ⌥ = a tenth of the motion, as the point curve does.
        let alt = ui.input(|i| i.modifiers.alt);
        let dd = ui.input(|i| i.pointer.delta());
        let step = if alt { vec2(dd.x * 0.1, dd.y * 0.1) } else { vec2(dd.x, dd.y) };
        let cursor = ui.data(|d| d.get_temp::<Pos2>(cursor_id)).unwrap_or(pos2(0.0, start_y)) + step;
        ui.data_mut(|d| d.insert_temp(cursor_id, cursor));
        let delta_y = start_y - cursor.y;
        let delta_level = (delta_y / r.height()) as f64;
        let per_unit = model.level_per_unit(region, x);
        let new_amount =
            if per_unit.abs() > f64::EPSILON { (initial_amount + delta_level / per_unit).clamp(-100.0, 100.0).round() } else { initial_amount };
        if let Some(&id) = PARAM_AMOUNTS.get(region) {
            set = Some((id, new_amount));
        }
    }
    if let Some((id, v)) = set {
        let _ = app.run("develop.set", json!({"control": id, "value": v}));
    }
    let dropped = resp.drag_stopped();
    if commit_interaction(app, ui, dragged.is_some(), dropped) {
        dragged = None;
        ui.data_mut(|d| {
            d.remove::<Pos2>(cursor_id);
            d.remove::<Option<(usize, f64, f64, f32)>>(drag_id);
        });
    }
    ui.data_mut(|dd| dd.insert_temp(drag_id, dragged));
    if dragged.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    // the region under the pointer (or the one in hand), named as its slider below reads it
    let shown = dragged.map(|(region, ..)| region).or_else(|| hovered.then(|| resp.hover_pos()).flatten().map(|q| model.region_at(from_screen_x(q))));
    if let Some(region) = shown
        && let Some(&id) = PARAM_AMOUNTS.get(region)
        && let Some(v) = set.and_then(|(s_id, v)| (s_id == id).then_some(v)).or_else(|| controls::get(d, id))
    {
        let name = controls::find(id).map_or("", |s| s.label);
        ui.painter().text(r.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, format!("{name} {v:.0}"), t.font(11.0), t.text_dim);
    }
}

pub(super) fn curve_editor(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    // channel selector
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (ch, c) in [
                ("parametric", Color32::from_gray(200)),
                ("master", Color32::WHITE),
                ("red", hex("#dd3333")),
                ("green", hex("#33bb55")),
                ("blue", hex("#3377ee")),
            ] {
                let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                crate::widgets::announce(&resp, format!("{} {ch}", crate::i18n::tr("Curve channel")));
                register(ui, format!("curveChannel:{ch}"), r);
                let sel = app.ui.curve_channel == ch;
                if ch == "parametric" {
                    paint(ui.painter(), r.shrink(4.0), Icon::Curve, if sel { t.text } else { t.icon });
                } else {
                    ui.painter().circle_stroke(r.center(), 7.0, Stroke::new(2.0, c));
                    if sel {
                        ui.painter().circle_filled(r.center(), 3.5, c);
                    }
                }
                if sel {
                    ui.painter().line_segment([r.left_bottom() + vec2(4.0, 1.0), r.right_bottom() + vec2(-4.0, 1.0)], Stroke::new(2.0, t.text));
                }
                let name = match ch {
                    "parametric" => "Parametric curve",
                    "master" => "Point curve",
                    "red" => "Red channel",
                    "green" => "Green channel",
                    _ => "Blue channel",
                };
                let resp = resp.on_hover_text(crate::i18n::tr_format!("{name} — double-click to reset it", name = name));
                if resp.double_clicked() {
                    app.ui.curve_channel = ch.into();
                    let _ = app.run("curve.reset", json!({"channel": ch}));
                } else if resp.clicked() {
                    app.ui.curve_channel = ch.into();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                tat_button(app, ui, "tat:curve", "Targeted adjustment: drag up/down on the photo to adjust the curve there");
            });
        });
    });
    let ch = app.ui.curve_channel.clone();
    let w = ui.available_width();
    let side = (w - 48.0).max(80.0);
    let (outer, _) = ui.allocate_exact_size(vec2(w, side + 12.0), Sense::hover());
    let r = Rect::from_min_size(pos2(outer.left() + 24.0, outer.top() + 6.0), vec2(side, side));
    let resp = ui.interact(r, egui::Id::unique(("curve", &ch)), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Tone Curve"));
    register(ui, "curve", r);
    crate::widgets::context_menu(&resp, |ui| curve_reset_menu(app, ui, &ch));
    let p = ui.painter();
    p.rect_filled(r, 2.0, t.canvas);
    if let Some(hist) = preview_histogram(app, id) {
        draw_curve_histogram(p, r, &hist, &ch);
    }
    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment(
            [pos2(r.left() + r.width() * f, r.top()), pos2(r.left() + r.width() * f, r.bottom())],
            Stroke::new(1.0, Color32::from_gray(52)),
        );
        p.line_segment(
            [pos2(r.left(), r.top() + r.height() * f), pos2(r.right(), r.top() + r.height() * f)],
            Stroke::new(1.0, Color32::from_gray(52)),
        );
    }
    p.line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.0, Color32::from_gray(70)));
    let to_screen = |x: f64, y: f64| pos2(r.left() + x as f32 * r.width(), r.bottom() - y as f32 * r.height());
    let from_screen = |q: Pos2| (((q.x - r.left()) / r.width()).clamp(0.0, 1.0) as f64, ((r.bottom() - q.y) / r.height()).clamp(0.0, 1.0) as f64);
    if ch == "parametric" {
        // The Effective curve: the model the renderer builds from the four region sliders, sampled
        // here and rebuilt every frame. The graph holds no state of its own, so a slider moved by
        // hand, a Reset or an Undo redraws it from the settings in the same frame it changes.
        let c = &d.curve;
        let model = ParametricCurve::new(c);
        let pts: Vec<Pos2> =
            (0..=CURVE_SAMPLES).map(|i| to_screen(i as f64 / CURVE_SAMPLES as f64, model.at(i as f64 / CURVE_SAMPLES as f64))).collect();
        p.add(egui::Shape::line(pts, Stroke::new(2.0, Color32::from_gray(220))));
        param_drag(app, ui, r, &resp, d);
        curve_footer(app, ui, d);
        for &c in PARAM_AMOUNTS.iter().rev() {
            control(app, ui, d, c, true);
        }
        control(app, ui, d, "curve.refineSaturation", true);
        return;
    }
    let pts_of = |d: &DevelopSettings| -> Vec<Point> {
        let v = match ch.as_str() {
            "red" => &d.curve.red,
            "green" => &d.curve.green,
            "blue" => &d.curve.blue,
            _ => &d.curve.master,
        };
        // in the order the curve is drawn (points from a sidecar, a preset or an agent can come
        // unsorted or not finite): the drag below keeps each point between its neighbours
        let mut v: Vec<Point> = v.iter().copied().filter(|q| q.x.is_finite() && q.y.is_finite()).collect();
        v.sort_by(|a, b| a.x.total_cmp(&b.x));
        if v.len() < 2 { vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)] } else { v }
    };
    let pts = pts_of(d);
    let curve = lightcraft_color::spline::MonotoneCurve::new(&pts.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>());
    let color = match ch.as_str() {
        "red" => hex("#dd3333"),
        "green" => hex("#33bb55"),
        "blue" => hex("#3377ee"),
        _ => Color32::from_gray(225),
    };
    let line: Vec<Pos2> = (0..=96).map(|i| to_screen(i as f64 / 96.0, curve.eval(i as f64 / 96.0).clamp(0.0, 1.0))).collect();
    p.add(egui::Shape::line(line, Stroke::new(2.0, color)));
    // interaction: drag a point (both axes, between its neighbours; ⌥ = fine), drag empty space to add and
    // drag a new point, click empty space to add, double-click a point to delete it
    let drag_id = egui::Id::unique(("curve-drag", &ch));
    // the point being dragged (stored as `Option<usize>` so a stale value can be cleared)
    let mut dragging: Option<usize> = ui.data(|dd| dd.get_temp::<Option<usize>>(drag_id)).flatten();
    let nearest = |q: Pos2| pts.iter().enumerate().map(|(i, p)| (i, to_screen(p.x, p.y).distance(q))).min_by(|a, b| a.1.total_cmp(&b.1));
    let hovered = resp.hover_pos().and_then(nearest).filter(|(_, dist)| *dist < 10.0).map(|(i, _)| i);
    if dragging.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hovered.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    for (i, q) in pts.iter().enumerate() {
        let c = to_screen(q.x, q.y);
        if dragging == Some(i) || (dragging.is_none() && hovered == Some(i)) {
            p.circle_filled(c, 5.5, color);
            p.circle_stroke(c, 5.5, Stroke::new(1.0, t.canvas));
        } else {
            p.circle_filled(c, 4.0, color);
        }
    }
    // input / output readout of the point being dragged (0–255 like the histogram)
    if let Some(q) = dragging.and_then(|i| pts.get(i)) {
        p.text(
            r.left_top() + vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            format!("{} / {}", (q.x * 255.0).round(), (q.y * 255.0).round()),
            t.font(11.0),
            t.text_dim,
        );
    }
    let mut new_pts = None;
    if resp.double_clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && let Some((i, dist)) = nearest(q)
        && dist < 10.0
        && i != 0
        && i != pts.len() - 1
    {
        let mut v = pts.clone();
        v.remove(i);
        new_pts = Some(v);
    } else if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        // pick the point under the press, not where the pointer is once the drag threshold passed
        let _ = app.run("develop.beginInteraction", json!({"label": "Tone Curve"}));
        match nearest(q) {
            Some((i, dist)) if dist < 10.0 => dragging = Some(i),
            _ => {
                let (x, y) = from_screen(q);
                let mut v = pts.clone();
                let i = v.iter().position(|p| p.x > x).unwrap_or(v.len());
                v.insert(i, Point::new(x, y));
                dragging = Some(i);
                new_pts = Some(v);
            }
        }
    } else if resp.dragged()
        && let (Some(i), Some(q)) = (dragging, resp.interact_pointer_pos())
    {
        let mut v = pts.clone();
        // ⌥-drag = fine adjustment, as in Lightroom: the point moves a tenth of the pointer's
        // motion (relative to where it is, so pressing ⌥ mid-drag doesn't make it jump)
        let fine = ui.input(|i| i.modifiers.alt);
        let (mut x, y) = match v.get(i) {
            Some(cur) if fine => {
                let dd = ui.input(|i| i.pointer.delta());
                ((cur.x + (dd.x / r.width()) as f64 * 0.1).clamp(0.0, 1.0), (cur.y - (dd.y / r.height()) as f64 * 0.1).clamp(0.0, 1.0))
            }
            _ => from_screen(q),
        };
        if i < v.len() {
            let lo = if i == 0 { 0.0 } else { v[i - 1].x + 0.01 };
            let hi = if i + 1 >= v.len() { 1.0 } else { v[i + 1].x - 0.01 };
            if i == 0 {
                x = x.min(hi);
            } else if i + 1 == v.len() {
                x = x.max(lo);
            } else if lo <= hi {
                x = x.clamp(lo, hi);
            } else {
                // neighbours closer than the 0.02 spacing (two clicks side by side, or points
                // from a file) leave no room: the point stays midway (`clamp` would panic: min above max)
                x = (v[i - 1].x + v[i + 1].x) / 2.0;
            }
            v[i] = Point::new(x, y);
            new_pts = Some(v);
        }
    } else if resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && nearest(q).is_none_or(|(_, dist)| dist >= 10.0)
    {
        let (x, y) = from_screen(q);
        let mut v = pts.clone();
        let i = v.iter().position(|p| p.x > x).unwrap_or(v.len());
        v.insert(i, Point::new(x, y));
        new_pts = Some(v);
    }
    if let Some(v) = new_pts {
        let arr: Vec<[f64; 2]> = v.iter().map(|p| [p.x, p.y]).collect();
        let _ = app.run("develop.curve", json!({"channel": ch, "points": arr}));
    }
    // the drag is over when egui says so, or when the pointer comes up without it saying so (a
    // release outside the window would otherwise leave the interaction open, and every later edit a
    // silent preview that Undo cannot take back)
    if commit_interaction(app, ui, dragging.is_some(), resp.drag_stopped()) {
        dragging = None;
    }
    ui.data_mut(|dd| dd.insert_temp(drag_id, dragging));
    curve_footer(app, ui, d);
    control(app, ui, d, "curve.refineSaturation", true);
    let _ = id;
}

/// The row under the curve graph: point-curve presets and reset every curve.
fn curve_footer(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    use lightcraft_engine::cmd::curves::{all_presets, matching_preset};
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("Point Curve")).font(t.font(12.0)).color(t.text_label));
            let current = matching_preset(&app.session, &d.curve);
            let builtin = current.as_ref().is_none_or(|name| all_presets(&app.session).iter().any(|p| p.builtin && &p.name == name));
            let label = crate::i18n::builtin_label(current.as_deref().unwrap_or("Custom"), builtin);
            let r = crate::widgets::dropdown(ui, "curvePreset", label, t.font(12.0), t.text);
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(180.0);
                let presets = all_presets(&app.session);
                let mut user_seen = false;
                for p in &presets {
                    if !p.builtin && !user_seen {
                        user_seen = true;
                        ui.separator();
                    }
                    let r = ui.selectable_label(current.as_deref() == Some(p.name.as_str()), crate::i18n::builtin_label(&p.name, p.builtin));
                    register(ui, format!("curvePreset:{}", p.name), r.rect);
                    if r.clicked() {
                        let _ = app.run("curve.applyPreset", json!({"name": p.name}));
                        ui.close();
                    }
                    if !p.builtin {
                        crate::widgets::context_menu(&r, |ui| {
                            if ui.button(crate::i18n::tr("Delete Preset")).clicked() {
                                let _ = app.run("curve.deletePreset", json!({"name": p.name}));
                                ui.close();
                            }
                        });
                    }
                }
                ui.separator();
                let shaped = current.as_deref() != Some("Linear");
                let r = ui.add_enabled(shaped, egui::Button::new(crate::i18n::tr("Save Point Curve…")));
                register(ui, "curvePresetMenu:save", r.rect);
                if r.clicked() {
                    crate::panels::dialogs::prompt(app, "Save Point Curve Preset", "Preset name", "", "curve.savePreset", json!({}), "name");
                    ui.close();
                }
                if ui.add_enabled(app.services.pick_curve_preset_files.is_some(), egui::Button::new(crate::i18n::tr("Import Presets…"))).clicked() {
                    let _ = app.run("file.importCurvePresets", json!({}));
                    ui.close();
                }
                let can_export = app.services.save_curve_preset_file.is_some() && !app.session.curve_presets.is_empty();
                if ui.add_enabled(can_export, egui::Button::new(crate::i18n::tr("Export Presets…"))).clicked() {
                    let _ = app.run("file.exportCurvePresets", json!({}));
                    ui.close();
                }
                if presets.iter().any(|p| !p.builtin) {
                    ui.label(RichText::new(crate::i18n::tr("Right-click a preset to delete it")).font(t.font(11.0)).color(t.text_dim));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = text_button(ui, "curveReset", crate::i18n::tr("Reset"), false).on_hover_text(crate::i18n::tr(
                    "Reset all curves: point curves (all channels) and parametric (double-click a channel to reset only that one)",
                ));
                if r.clicked() {
                    let _ = app.run("curve.reset", json!({"channel": "all"}));
                }
            });
        });
    });
}

/// Right-click menu of the curve graph.
fn curve_reset_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, ch: &str) {
    let label = if ch == "parametric" {
        crate::i18n::tr("Reset Parametric Curve").to_string()
    } else {
        crate::i18n::tr_format!("Reset {} Channel", crate::i18n::tr(channel_label(ch)))
    };
    let r = ui.button(label);
    register(ui, "curveMenu:resetChannel", r.rect);
    if r.clicked() {
        let _ = app.run("curve.reset", json!({"channel": ch}));
        ui.close();
    }
    let r = ui.button(crate::i18n::tr("Reset All Curves"));
    register(ui, "curveMenu:resetAll", r.rect);
    if r.clicked() {
        let _ = app.run("curve.reset", json!({"channel": "all"}));
        ui.close();
    }
}

fn channel_label(ch: &str) -> &'static str {
    match ch {
        "red" => "Red",
        "green" => "Green",
        "blue" => "Blue",
        _ => "RGB",
    }
}

/// Toggle for a targeted-adjustment tool (`tool` = `tat:<target>`).
fn tat_button(app: &mut LightcraftApp, ui: &mut egui::Ui, tool: &str, tip: &str) {
    let active = app.ui.tool == tool;
    let id = tool.replace(':', "-");
    if crate::widgets::icon_button(ui, &id, Icon::Target, vec2(26.0, 26.0), active, true, tip).clicked() {
        set_tat(app, ui.ctx(), tool, active);
    }
}

/// Turn targeted-adjustment tool `tool` off (`active`) or on. Turning it on says how to use and
/// leave it: while it is on, dragging on the photo adjusts instead of zooming or panning, which is
/// easy to miss when the button looks like a tab.
pub(super) fn set_tat(app: &mut LightcraftApp, ctx: &egui::Context, tool: &str, active: bool) {
    if active {
        app.ui.tool.clear();
    } else {
        app.ui.tool = tool.to_string();
        app.toast(ctx, crate::i18n::tr("Targeted adjustment on: drag up or down on the photo · Esc to stop"));
    }
}

#[cfg(test)]
mod curve_histogram_tests {
    use super::*;

    #[test]
    fn curve_backdrop_tracks_luma_and_selected_channel() {
        let img = lightcraft_raster::Rgba8::from_fn(64, 32, |x, _| if x < 32 { [32, 100, 32, 255] } else { [224, 100, 224, 255] });
        let h = lightcraft_raster::Histogram::of_srgb8(&img);
        let red = curve_histogram_profile(&h, "red");
        let green = curve_histogram_profile(&h, "green");
        let luma = curve_histogram_profile(&h, "master");
        assert!(red[32] > red[150] && red[224] > red[150]);
        assert!(green[100] > green[32] && green[100] > green[224]);
        assert!(luma.iter().any(|v| *v > 0.0));
    }

    #[test]
    fn hdr_backdrop_uses_only_the_sdr_bins_and_never_overflows() {
        let h = lightcraft_raster::Histogram::of_hdr(&[[0.1, 0.1, 0.1], [1.0, 1.0, 1.0], [8.0, 8.0, 8.0]], 4.0);
        let values = curve_histogram_profile(&h, "parametric");
        assert!(values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        assert!(values.iter().any(|v| *v > 0.0));
    }
}
