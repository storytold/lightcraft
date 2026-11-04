use super::sliders::control;
use super::{
    Align2, Color32, DevelopSettings, Icon, LightcraftApp, Mesh, PhotoId, Pos2, Rect, Sense, Stroke, Tokens, Vertex, json, paint, pos2, register,
    vec2,
};

// ------------------------------------------------------------------------------ colour grading

/// The colour grading tabs: key, tooltip, icon.
const GRADING_TABS: [(&str, &str, Icon); 4] = [
    ("3way", "3-Way", Icon::GradeAll),
    ("shadows", "Shadows", Icon::GradeShadows),
    ("midtones", "Midtones", Icon::GradeMidtones),
    ("highlights", "Highlights", Icon::GradeHighlights),
];

/// The wheels, in the order of `UiState::grading_hidden`.
const GRADING_WHEELS: [(&str, &str); 3] = [("shadows", "Shadows"), ("midtones", "Midtones"), ("highlights", "Highlights")];

pub(super) fn grading(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings, photo: PhotoId) {
    let mode = GRADING_TABS.iter().find(|(k, ..)| *k == app.ui.grading_mode).map_or("3way", |(k, ..)| *k);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 2 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (key, tip, icon) in GRADING_TABS {
                if crate::widgets::icon_button(ui, &format!("grading-{key}"), icon, vec2(28.0, 28.0), key == mode, true, crate::i18n::tr(tip))
                    .clicked()
                {
                    app.ui.grading_mode = key.to_string();
                }
            }
        });
    });
    let w = ui.available_width();
    if mode == "3way" {
        // three wheels of one size: midtones on top, shadows and highlights below
        let rad = 40.0;
        let (area, _) = ui.allocate_exact_size(vec2(w, 276.0), Sense::hover());
        let at = [
            pos2(area.left() + w * 0.28, area.top() + 196.0),
            pos2(area.center().x, area.top() + 70.0),
            pos2(area.left() + w * 0.72, area.top() + 196.0),
        ];
        for (i, c) in at.into_iter().enumerate() {
            grading_wheel(app, ui, d, photo, i, c, rad, true);
        }
        for c in ["grading.shadows.lum", "grading.midtones.lum", "grading.highlights.lum"] {
            control(app, ui, d, c, true);
        }
    } else {
        let i = GRADING_WHEELS.iter().position(|(k, _)| *k == mode).unwrap_or(1);
        let rad = (w * 0.3).clamp(40.0, 80.0);
        let (area, _) = ui.allocate_exact_size(vec2(w, rad * 2.0 + 40.0), Sense::hover());
        grading_wheel(app, ui, d, photo, i, pos2(area.center().x, area.top() + rad + 10.0), rad, false);
        for part in ["hue", "sat", "lum"] {
            control(app, ui, d, &format!("grading.{mode}.{part}"), true);
        }
    }
    for c in ["grading.blending", "grading.balance"] {
        control(app, ui, d, c, true);
    }
}

/// One colour grading wheel (`i` into [`GRADING_WHEELS`]) centred on `c`: drag to pick hue and
/// saturation, double-click to reset, and an eye at its bottom-left corner that hides its effect
/// on the photo for viewing. `readout`: show hue and saturation centred above the wheel while
/// hovered (3-Way); otherwise the wheel is the large one and gets saturation guide rings.
#[allow(clippy::too_many_arguments)]
fn grading_wheel(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings, photo: PhotoId, i: usize, c: Pos2, rad: f32, readout: bool) {
    let t = Tokens::get(ui.ctx());
    let Some(&(key, label)) = GRADING_WHEELS.get(i) else { return };
    let wh = match key {
        "shadows" => d.grading.shadows,
        "midtones" => d.grading.midtones,
        _ => d.grading.highlights,
    };
    let hidden = app.ui.grading_hidden_for(photo).get(i).copied().unwrap_or(false);
    let r = Rect::from_center_size(c, vec2(rad * 2.0, rad * 2.0));
    // the corners (eye, readout) count as the wheel for hovering
    let zone = r.expand(10.0);
    let hovering = ui.rect_contains_pointer(zone);
    let resp = ui.interact(r, egui::Id::unique(("wheel", key)), Sense::click_and_drag());
    crate::widgets::announce(&resp, format!("{} {key}", crate::i18n::tr("Color wheel")));
    let resp = resp.on_hover_text(crate::i18n::tr(
        "Drag to set hue and saturation. Hold Shift to keep the hue, Cmd/Ctrl to keep the saturation, Alt/Option to fine-adjust. Double-click to reset.",
    ));
    register(ui, format!("wheel:{key}"), r);
    paint_wheel(ui.painter(), c, rad);
    if hidden {
        ui.painter().circle_filled(c, rad, Color32::from_black_alpha(150));
    }
    // the large single wheel: dashed rings at 25 / 50 / 75 % saturation to judge strength by eye
    if !readout {
        for f in [0.25f32, 0.5, 0.75] {
            let ring_rad = rad * f;
            let n = (ring_rad * std::f32::consts::TAU / 2.0).max(8.0) as usize;
            let ring: Vec<Pos2> = (0..=n).map(|k| c + egui::Vec2::angled(k as f32 / n as f32 * std::f32::consts::TAU) * ring_rad).collect();
            ui.painter().extend(egui::Shape::dashed_line(&ring, Stroke::new(0.8, Color32::from_white_alpha(45)), 4.0, 3.5));
        }
    }
    let a = (wh.hue as f32).to_radians();
    let dot = c + vec2(a.cos(), -a.sin()) * rad * (wh.sat as f32 / 100.0);
    // Modifiers (as in Lightroom): Shift keeps the hue, Cmd/Ctrl keeps the saturation, Alt moves
    // the puck a tenth as far as the pointer. Pressing or releasing one mid-drag re-grabs the
    // puck as it is at that moment.
    let grab_id = egui::Id::unique(("wheelGrab", key));
    let mods = ui.input(|i| WheelMods::from_modifiers(i.modifiers));
    let pointer = wheel_pointer(ui, &resp);
    let grab = match pointer {
        Some(q) if resp.dragged() || resp.clicked() => {
            let held = ui.data(|m| m.get_temp::<WheelGrab>(grab_id)).filter(|g| g.mods == mods && !resp.drag_started());
            Some(held.unwrap_or_else(|| {
                // a fine drag counts the pointer's travel from the press, not from the frame
                // egui first called it a drag
                let origin = if resp.drag_started() { ui.input(|i| i.pointer.press_origin()).unwrap_or(q) } else { q };
                let g = WheelGrab::new(mods, wh.hue as f32, wh.sat as f32, origin - c, rad);
                ui.data_mut(|m| m.insert_temp(grab_id, g));
                g
            }))
        }
        _ => {
            ui.data_mut(|m| m.remove::<WheelGrab>(grab_id));
            None
        }
    };
    // while a constraint is held: the guide for the one move left (the whole spoke for the hue,
    // the circle for the saturation)
    let guide = Stroke::new(1.5, Color32::from_white_alpha(170));
    match grab.and_then(|g| g.lock()) {
        Some(WheelLock::Hue(h)) => {
            let a = h.to_radians();
            ui.painter().line_segment([c, c + vec2(a.cos(), -a.sin()) * rad], guide);
        }
        Some(WheelLock::Sat(s)) => {
            ui.painter().circle_stroke(c, rad * s / 100.0, guide);
        }
        None => {}
    }
    // the neutral centre (issue #559), under the point
    ui.painter().circle_filled(c, 2.0, Color32::WHITE);
    ui.painter().circle_filled(dot, 5.0, if hidden { Color32::from_gray(150) } else { Color32::WHITE });
    ui.painter().circle_stroke(dot, 5.0, Stroke::new(1.0, Color32::BLACK));
    ui.painter().text(pos2(c.x, c.y + rad + 12.0), Align2::CENTER_CENTER, crate::i18n::tr(label), t.font(12.0), t.text_label);
    if resp.drag_started() {
        let _ = app.run("develop.beginInteraction", json!({"label": crate::i18n::tr_format!("{label} Grading", label = label)}));
    }
    if (resp.dragged() || resp.clicked())
        && let Some(q) = pointer
    {
        let (hue, sat) = grab.map_or_else(|| wheel_pick(q - c, rad, None), |g| g.pick(q - c, rad));
        let _ = app.run("develop.set", json!({"values": {format!("grading.{key}.hue"): hue.round(), format!("grading.{key}.sat"): sat.round()}}));
    }
    if resp.drag_stopped() || resp.clicked() {
        let _ = app.run("develop.endInteraction", json!({}));
    }
    // released, or Esc (egui ends a drag on it)
    if resp.drag_stopped()
        && let Some(held) = ui.ctx().data_mut(|m| m.remove_temp::<WheelCapture>(egui::Id::unique(WHEEL_CAPTURE)))
        && !held.dropped
    {
        give_pointer_back(ui.ctx(), held.origin);
    }
    if resp.double_clicked() {
        let _ = app.run("develop.set", json!({"values": {format!("grading.{key}.hue"): 0, format!("grading.{key}.sat"): 0}}));
    }
    // hue and saturation readout: centred above the wheel, shown like the eye (no fade)
    if readout && (hovering || resp.dragged()) {
        // set a little below the gap's midpoint: the eye reads it as centred there
        ui.painter().text(
            pos2(c.x, r.top() - 13.0),
            Align2::CENTER_CENTER,
            format!("H {}°  S {}", wh.hue.round() as i64, wh.sat.round() as i64),
            t.font(11.0),
            t.text_label,
        );
    }
    // the eye: on top of the wheel, shown while hovering or while the wheel is hidden
    if hovering || hidden {
        let eye = Rect::from_center_size(r.left_bottom() + vec2(2.0, -2.0), vec2(16.0, 16.0));
        let er = ui.interact(eye, egui::Id::unique(("wheelEye", key)), Sense::click());
        crate::widgets::announce(&er, crate::i18n::tr("Show / hide"));
        register(ui, format!("wheelEye:{key}"), eye);
        paint(ui.painter(), eye, if hidden { Icon::EyeOff } else { Icon::Eye }, if hidden { t.text_disabled } else { t.icon });
        let er = er.on_hover_text(crate::i18n::tr(if hidden { "Show this wheel's effect" } else { "Hide this wheel's effect" }));
        if er.clicked() {
            app.ui.set_grading_hidden(photo, i, !hidden);
        }
    }
}

/// A wheel's modifier state. Shift keeps the hue, Cmd/Ctrl the saturation (Shift wins when both
/// are down: keeping both would freeze the puck), Alt fine-adjusts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WheelMods {
    pub keep_hue: bool,
    pub keep_sat: bool,
    pub fine: bool,
}

impl WheelMods {
    pub(crate) fn from_modifiers(m: egui::Modifiers) -> Self {
        Self { keep_hue: m.shift, keep_sat: m.command && !m.shift, fine: m.alt }
    }
}

/// What a constraint holds still while the pointer moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WheelLock {
    /// Keep this hue (degrees): only the saturation changes, along the spoke at that hue.
    Hue(f32),
    /// Keep this saturation (0..100): only the hue changes, round the circle at that radius.
    Sat(f32),
}

/// Pointer travel to puck travel with Alt held, as for a slider's fine adjust.
const WHEEL_FINE: f32 = 0.1;

/// The puck as it was when the pointer grabbed it (or a modifier last changed): the hue and
/// saturation a constraint keeps, and the origin a fine adjust measures the pointer's travel from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WheelGrab {
    pub mods: WheelMods,
    /// The puck's hue (degrees) and saturation (0..100) at the grab.
    hue: f32,
    sat: f32,
    /// The pointer at the grab, as an offset from the wheel's centre.
    origin: egui::Vec2,
}

impl WheelGrab {
    /// Grab a puck at `hue` / `sat` with the pointer `origin` from the centre of a wheel of
    /// radius `rad`. The kept hue is the puck's own, never the angle the pointer happens to be
    /// at. A puck at the centre (saturation under 1) has no hue yet, so there it is the angle of
    /// the pointer at the grab; the same rule covers a non-finite hue.
    pub(crate) fn new(mods: WheelMods, hue: f32, sat: f32, origin: egui::Vec2, rad: f32) -> Self {
        let sat = if sat.is_finite() { sat.clamp(0.0, 100.0) } else { 0.0 };
        let hue = if hue.is_finite() && sat >= 1.0 { hue.rem_euclid(360.0) } else { wheel_pick(origin, rad, None).0 };
        Self { mods, hue, sat, origin }
    }

    /// The constraint this grab holds, if any.
    pub(crate) fn lock(&self) -> Option<WheelLock> {
        if self.mods.keep_hue {
            Some(WheelLock::Hue(self.hue))
        } else if self.mods.keep_sat {
            Some(WheelLock::Sat(self.sat))
        } else {
            None
        }
    }

    /// The new hue and saturation for the pointer at offset `v` from the centre.
    pub(crate) fn pick(&self, v: egui::Vec2, rad: f32) -> (f32, f32) {
        let v = if self.mods.fine {
            // the puck where it was, moved a tenth of the pointer's travel since the grab
            let a = self.hue.to_radians();
            let puck = vec2(a.cos(), -a.sin()) * rad.max(1.0) * (self.sat / 100.0);
            puck + (v - self.origin) * WHEEL_FINE
        } else {
            v
        };
        wheel_pick(v, rad, self.lock())
    }
}

/// A wheel drag holds the pointer, as in Lightroom (issue #1899): it is hidden for the drag, the
/// puck follows the mouse's own travel, and on release the pointer is back where the button went
/// down, so a long (Alt) drag doesn't leave it far from the wheels. On macOS the pointer is locked
/// in place for the drag; elsewhere it moves unseen and is put back. The browser can't move it
/// (eframe's web host ignores the command), so there it only shows again where the drag ended.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct WheelCapture {
    /// Where the button went down: the pointer goes back there.
    origin: Pos2,
    /// Where the pointer would be had it been free: where the drag began plus the travel since.
    pos: Pos2,
    /// Let go early (the window lost focus): the rest of this drag leaves the wheel alone.
    dropped: bool,
    /// The pass that last drew the dragged wheel.
    pass: u64,
}

/// One wheel is dragged at a time, so one capture for them all.
const WHEEL_CAPTURE: &str = "wheelCapture";

/// The mouse's travel this frame: the pointer's, or, while the pointer stands still (locked, or
/// against a screen edge), the raw motion the host reports.
fn wheel_travel(delta: egui::Vec2, motion: Option<egui::Vec2>) -> egui::Vec2 {
    if delta != egui::Vec2::ZERO {
        return delta;
    }
    motion.filter(|m| m.x.is_finite() && m.y.is_finite()).unwrap_or(egui::Vec2::ZERO)
}

/// Where the wheel `resp` reads the pointer this frame: where it clicked, or for a drag the held
/// pointer ([`WheelCapture`]), `None` once a lost focus let go of it (Esc ends the drag itself:
/// egui stops a drag on Esc).
fn wheel_pointer(ui: &egui::Ui, resp: &egui::Response) -> Option<Pos2> {
    let q = resp.interact_pointer_pos()?;
    if !resp.dragged() {
        return Some(q);
    }
    let ctx = ui.ctx();
    let id = egui::Id::unique(WHEEL_CAPTURE);
    let held = ctx.data(|m| m.get_temp::<WheelCapture>(id)).filter(|_| !resp.drag_started());
    let mut cap = held.unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Locked));
        }
        WheelCapture { origin: ui.input(|i| i.pointer.press_origin()).unwrap_or(q), pos: q, ..Default::default() }
    });
    cap.pass = ctx.cumulative_pass_nr();
    if !cap.dropped {
        let (travel, unfocused) = ui.input(|i| {
            // the event, not `focused`: a click that activates the window may land before its focus
            let unfocused = i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false)));
            (wheel_travel(i.pointer.delta(), i.pointer.motion()), unfocused)
        });
        if !resp.drag_started() {
            cap.pos += travel;
        }
        if unfocused {
            give_pointer_back(ctx, cap.origin);
            cap.dropped = true;
        } else {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
    }
    ctx.data_mut(|m| m.insert_temp(id, cap));
    (!cap.dropped).then_some(cap.pos)
}

/// Unlock and show the pointer a wheel drag held, back at `origin`.
fn give_pointer_back(ctx: &egui::Context, origin: Pos2) {
    if cfg!(target_os = "macos") {
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
    }
    if cfg!(not(target_arch = "wasm32")) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorPosition(origin));
    }
}

/// Once a frame, after the panels: a held pointer stays hidden over whatever set its own cursor,
/// and is given back if no wheel drew this pass (the flyout closed or the view changed under the
/// drag), so it is never left locked or hidden.
pub(crate) fn wheel_pointer_frame_end(ctx: &egui::Context) {
    let id = egui::Id::unique(WHEEL_CAPTURE);
    let Some(cap) = ctx.data(|m| m.get_temp::<WheelCapture>(id)) else { return };
    if cap.pass != ctx.cumulative_pass_nr() {
        ctx.data_mut(|m| m.remove::<WheelCapture>(id));
        if !cap.dropped {
            give_pointer_back(ctx, cap.origin);
        }
    } else if !cap.dropped {
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }
}

/// Hue (degrees) and saturation (0..100) for a pointer at offset `v` from a wheel's centre.
/// `lock`: hold the hue (the saturation is `v` projected onto that hue's spoke) or hold the
/// saturation (the hue is the angle of `v`). A non-finite `v` gives a finite answer (the locked
/// value, else 0).
pub(crate) fn wheel_pick(v: egui::Vec2, rad: f32, lock: Option<WheelLock>) -> (f32, f32) {
    let rad = rad.max(1.0);
    let v = if v.x.is_finite() && v.y.is_finite() { v } else { egui::Vec2::ZERO };
    let angle = || ((-v.y).atan2(v.x).to_degrees() + 360.0) % 360.0;
    match lock {
        Some(WheelLock::Hue(h)) => {
            let h = if h.is_finite() { h } else { 0.0 };
            let a = h.to_radians();
            let along = v.x * a.cos() - v.y * a.sin();
            (h, (along / rad * 100.0).clamp(0.0, 100.0))
        }
        Some(WheelLock::Sat(s)) => (angle(), if s.is_finite() { s.clamp(0.0, 100.0) } else { 0.0 }),
        None => (angle(), (v.length() / rad * 100.0).min(100.0)),
    }
}

fn paint_wheel(p: &egui::Painter, c: Pos2, rad: f32) {
    p.add(wheel_mesh(c, rad));
    p.circle_stroke(c, rad, Stroke::new(1.0, Color32::from_gray(40)));
}

/// The picture of a grading wheel: a hue disc whose rim is fully saturated display sRGB at full
/// brightness, fading to a neutral grey at the centre. It is built from concentric rings so the
/// saturation ramps evenly with the radius instead of one fan of triangles blending each hue
/// straight into grey (which made the whole disc look muddy, issue #77). Only the drawing:
/// what a position means for the edit is [`wheel_pick`].
pub(crate) fn wheel_mesh(c: Pos2, rad: f32) -> Mesh {
    const SEGMENTS: u32 = 72;
    const RINGS: u32 = 12;
    /// Brightness (HSV value) at the centre; the rim is 1.0.
    const CENTRE_V: f32 = 0.6;
    let colour = |hue_deg: f32, t: f32| {
        let rgb = lightcraft_color::perceptual::hsv_to_rgb(hue_deg, t, CENTRE_V + (1.0 - CENTRE_V) * t);
        let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        Color32::from_rgb(u(rgb[0]), u(rgb[1]), u(rgb[2]))
    };
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex { pos: c, uv: Pos2::ZERO, color: colour(0.0, 0.0) });
    // ring k (1..=RINGS) holds SEGMENTS + 1 vertices starting at index 1 + (k - 1) * (SEGMENTS + 1)
    let row = SEGMENTS + 1;
    for k in 1..=RINGS {
        let t = k as f32 / RINGS as f32;
        for i in 0..=SEGMENTS {
            let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            mesh.vertices.push(Vertex { pos: c + vec2(a.cos(), -a.sin()) * rad * t, uv: Pos2::ZERO, color: colour(a.to_degrees(), t) });
        }
    }
    for i in 0..SEGMENTS {
        mesh.indices.extend([0, 1 + i, 2 + i]);
    }
    for k in 1..RINGS {
        let inner = 1 + (k - 1) * row;
        let outer = inner + row;
        for i in 0..SEGMENTS {
            mesh.indices.extend([inner + i, outer + i, outer + i + 1, inner + i, outer + i + 1, inner + i + 1]);
        }
    }
    mesh
}
