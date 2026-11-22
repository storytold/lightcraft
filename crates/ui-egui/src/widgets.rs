//! Lightroom-style widgets: sliders with gradient tracks and hollow thumbs, section headers,
//! flyout rows, bordered buttons. Every interactive widget registers an automation id + rect
//! ([`register`]) so the control channel can find and click it by name.

use egui::{Align2, Color32, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use lcc_develop::{ControlSpec, Shade, Track};

use crate::icons::{Icon, paint};
use crate::theme::Tokens;

/// Standard dropdown chevrons, including settings, import/export and rule editors.
pub fn combo_box(id: impl egui::AsIdSalt) -> egui::ComboBox {
    egui::ComboBox::from_id_salt(id)
        .icon(|ui, rect, visuals, _| {
            paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), Icon::ChevronDown, visuals.fg_stroke.color);
        })
        .popup_style(list_style.into())
}

/// A dropdown's list and its submenus: items without the resting border of a button, as egui's
/// own menus draw them. An unhovered item gives up its border's width of padding, so with one it
/// grew 2 points when hovered, and the longest name of a list sized to it wrapped (#2354).
fn list_style(style: &mut egui::Style) {
    style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
}

/// A native button with a vector arrow beside its text; egui reserves the icon's space.
pub fn arrow_button<'a>(ui: &mut Ui, text: impl egui::IntoAtoms<'a>, icon: Icon, leading: bool) -> Response {
    let id = ui.next_auto_id().with("arrow");
    let mut atoms = text.into_atoms();
    let arrow = egui::Atom::custom(id, vec2(16.0, 16.0));
    if leading {
        atoms.push_left(arrow);
    } else {
        atoms.push_right(arrow);
    }
    let r = egui::Button::new(atoms).atom_ui(ui);
    if let Some(rect) = r.rect(id) {
        paint(ui.painter(), rect, icon, ui.style().interact(&r.response).fg_stroke.color);
    }
    r.response
}

/// Keep egui's submenu interaction and layout, using our vector chevron instead of a font glyph.
pub fn menu_button<'a, R>(ui: &mut Ui, atoms: impl egui::IntoAtoms<'a>, contents: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<Option<R>> {
    if !egui::containers::menu::is_in_menu(ui) {
        return ui.menu_button(atoms, contents);
    }
    let button = egui::Button::new(atoms.into_atoms()).right_text("    ");
    let (response, inner) = egui::containers::menu::SubMenuButton::from_button(button).ui(ui, contents);
    let center = pos2(response.rect.right() - ui.spacing().button_padding.x - 8.0, response.rect.center().y);
    paint(ui.painter(), Rect::from_center_size(center, vec2(16.0, 16.0)), Icon::ChevronRight, ui.style().interact(&response).fg_stroke.color);
    egui::InnerResponse::new(inner.map(|r| r.inner), response)
}

/// A context menu on `response`, as `Response::context_menu` gives, that a right-click moves
/// (#1877). egui closes a menu on any click once it was open on the previous frame, and judges
/// that by the menu's id: a right-click elsewhere on the same widget while its menu is open
/// reopened the menu at the pointer and closed it again on the same frame, a one-frame flash.
/// The click that opens (or moves) the menu is not the one that closes it.
pub fn context_menu(response: &Response, add_contents: impl FnOnce(&mut Ui)) -> Option<egui::InnerResponse<()>> {
    egui::Popup::context_menu(response).close_behavior(context_menu_close(response, egui::PopupCloseBehavior::CloseOnClick)).show(add_contents)
}

/// How a context menu on `response` closes this frame: `default`, except on the frame of the
/// right-click that opens or moves it, which must not close it (see [`context_menu`]).
pub fn context_menu_close(response: &Response, default: egui::PopupCloseBehavior) -> egui::PopupCloseBehavior {
    if response.secondary_clicked() { egui::PopupCloseBehavior::IgnoreClicks } else { default }
}

// ------------------------------------------------------------------ automation registry

#[derive(Clone, Default)]
pub struct Registry(pub Vec<(String, Rect)>);

/// Record a widget's automation id and screen rect for this frame.
pub fn register(ctx: &egui::Context, id: impl Into<String>, rect: Rect) {
    let id = id.into();
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Registry>(egui::Id::new("lc-registry")).0.push((id, rect)));
}

/// Called at the end of a scroll area's contents: when the keyboard focus (Tab) has just moved onto
/// one of its widgets that lies scrolled out of view, scroll it in, so the focused control is never
/// hidden (egui leaves that to each widget). A pass with a pointer press or release is left alone:
/// the pointer's widget is already in view, a drag must not scroll under it, and egui's response
/// lookup would hand the focus back on a click elsewhere.
pub fn follow_focus(ui: &Ui) {
    if ui.input(|i| i.pointer.any_down() || i.pointer.any_pressed() || i.pointer.any_released()) {
        return;
    }
    let Some(r) = ui.memory(|m| m.focused()).and_then(|id| ui.ctx().read_response(id)) else {
        return;
    };
    if !r.gained_focus() {
        return;
    }
    if r.layer_id == ui.layer_id() && ui.min_rect().contains_rect(r.rect) && !ui.clip_rect().contains_rect(r.rect) {
        r.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
    }
}

/// Esc was handled this frame by something nested (an open popup closed on it): containers such
/// as dialogs shouldn't close too. Keyed to the frame, so it lapses by itself on the next one.
pub fn take_escape(ctx: &egui::Context) {
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("lc-escape-taken"), frame));
}

/// Whether [`take_escape`] was called this frame.
pub fn escape_taken(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(egui::Id::new("lc-escape-taken"))) == Some(ctx.cumulative_frame_nr())
}

pub fn take_registry(ctx: &egui::Context) -> Vec<(String, Rect)> {
    ctx.data_mut(|d| std::mem::take(&mut d.get_temp_mut_or_default::<Registry>(egui::Id::new("lc-registry")).0))
}

// ------------------------------------------------------------------ preview-only raws

/// The decoder's reason why a raw is shown from its embedded preview, shortened for the UI
/// ("Nikon Huffman-compressed NEF (no clean-room …)" → "Nikon Huffman-compressed NEF").
pub fn preview_only_variant(reason: &str) -> &str {
    reason.split(" (").next().unwrap_or(reason).trim()
}

/// What a preview-only raw means for the user (see `Photo::preview_only`).
pub fn preview_only_explanation(reason: &str) -> String {
    if reason.starts_with("Immich link only") {
        return crate::i18n::tr(
            "Linked from Immich: you see the server's preview until the original is downloaded (it is when the photo is opened in Develop).",
        )
        .to_string();
    }
    if reason.starts_with("Nikon High Efficiency NEF (") {
        return crate::i18n::tr(
            "Nikon High Efficiency NEF (HE / HE★) uses intoPIX TicoRAW compression. {app} does not currently have a license for this codec, so only the camera's embedded JPEG preview is available, not the raw sensor data. To edit raw data, shoot Lossless compressed NEF or convert existing HE files to DNG using a licensed converter.",
        )
        .to_string();
    }
    crate::i18n::tr_format!(
        "{app} can't decode this raw variant yet ({}). You're editing the camera's embedded JPEG preview, which already includes the camera's picture style (e.g. Monochrome) and white balance.",
        preview_only_variant(reason)
    )
}

/// A panel notice for a raw shown from its embedded preview (Edit, Info): an amber info icon,
/// "Preview only", the explanation and a Reload from Disk button (`button:reloadPreviewOnly:{key}`:
/// for a file a newer version can decode that was offline when its library opened). Registered as
/// `notice:previewOnly:{key}`. `true` when the button was clicked.
pub fn preview_only_notice(ui: &mut Ui, key: &str, reason: &str) -> bool {
    let mut reload = false;
    let t = Tokens::get(ui.ctx());
    let r = egui::Frame::NONE
        .fill(t.canvas)
        .stroke(Stroke::new(1.0, t.caution.gamma_multiply(0.45)))
        .corner_radius(6.0)
        .inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 9 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (ir, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                paint(ui.painter(), ir, Icon::Info, t.caution);
                ui.label(egui::RichText::new(crate::i18n::tr("Preview only")).font(t.semibold(12.5)).color(t.text));
            });
            ui.add_space(2.0);
            ui.label(egui::RichText::new(preview_only_explanation(reason)).size(11.5).color(t.text_label));
            ui.add_space(6.0);
            reload = text_button(ui, &format!("reloadPreviewOnly:{key}"), "Reload from Disk", false).clicked();
        })
        .response;
    let r = r.on_hover_text(crate::i18n::tr_format!("Decoder: {reason}", reason = reason));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, crate::i18n::tr("Preview only: editing the camera's embedded JPEG")));
    register(ui.ctx(), format!("notice:previewOnly:{key}"), r.rect);
    reload
}

// ------------------------------------------------------------------ colour helpers

pub fn hex(s: &str) -> Color32 {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0x808080);
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Band colours for the 8 colour-mixer bands (swatches and tracks).
pub const BAND_COLORS: [&str; 8] = ["#aa0000", "#bb6600", "#aaaa00", "#009900", "#00aaaa", "#0044cc", "#7722aa", "#aa0077"];

/// The colour of HSL band `i` (wrapping round the eight bands).
fn band_color(i: usize) -> Color32 {
    hex(BAND_COLORS.get(i % BAND_COLORS.len()).copied().unwrap_or("#808080"))
}

fn track_stops(track: &Track) -> Option<Vec<Color32>> {
    Some(match track {
        Track::Plain | Track::Centered => return None,
        Track::Temp => ["#193961", "#47566a", "#60666d", "#70705b", "#737339", "#75742a"].iter().map(|h| hex(h)).collect(),
        Track::Tint => ["#246123", "#2c552c", "#394739", "#4a3d4c", "#6b336b", "#872c87"].iter().map(|h| hex(h)).collect(),
        Track::Gradient { from, to } => vec![hex(from), hex(to)],
        Track::Hue { band } => {
            let i = *band as usize;
            let prev = band_color(i + 7);
            let next = band_color(i + 1);
            let me = band_color(i);
            vec![
                prev.gamma_multiply(0.7),
                lerp(prev, me, 0.5).gamma_multiply(0.7),
                me.gamma_multiply(0.7),
                lerp(me, next, 0.5).gamma_multiply(0.7),
                next.gamma_multiply(0.7),
            ]
        }
        Track::Sat { band } => vec![hex("#454343"), lerp(hex("#454343"), band_color(*band as usize), 0.6), band_color(*band as usize)],
        Track::Lum { band } => {
            let c = band_color(*band as usize);
            vec![c.gamma_multiply(0.2), c.gamma_multiply(0.55), lerp(c, Color32::WHITE, 0.35), lerp(c, Color32::WHITE, 0.6)]
        }
        Track::Rainbow => ["#aa0000", "#aaaa00", "#009900", "#00aaaa", "#0044cc", "#aa0077", "#aa0000"].iter().map(|h| hex(h)).collect(),
    })
}

fn paint_track(ui: &Ui, rect: Rect, track: &Track, shade: Shade, t: &Tokens, thumb_x: f32, ring: f32) {
    let p = ui.painter();
    let y = rect.center().y;
    let h = 1.0;
    if shade != Shade::Plain && track_stops(track).is_none() {
        // the Compact layout's grey ramp: one mesh with a colour per column of vertices
        let hh = h + 0.5;
        let steps: u32 = 48;
        let mut mesh = egui::Mesh::default();
        for i in 0..=steps {
            let a = i as f32 / steps as f32;
            let x = rect.left() + rect.width() * a;
            let c = shade_color(shade, t, a);
            mesh.colored_vertex(pos2(x, y - hh), c);
            mesh.colored_vertex(pos2(x, y + hh), c);
            if i > 0 {
                let k = 2 * i;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        p.add(egui::Shape::mesh(mesh));
        if matches!(track, Track::Centered) {
            // where the slider has no effect, lighter than the ramp around it
            p.rect_filled(Rect::from_center_size(pos2(rect.center().x, y), vec2(1.0, 6.0)), 0.0, t.text_label);
        }
    } else if let Some(stops) = track_stops(track) {
        // gradient: segments between stops
        let n = stops.len().max(2) - 1;
        let steps = 48;
        for i in 0..steps {
            let (a, b) = (i as f32 / steps as f32, (i + 1) as f32 / steps as f32);
            let seg = (a * n as f32).floor() as usize;
            let local = a * n as f32 - seg as f32;
            // clamped to the last stop: a one-stop track is flat rather than read past its end
            let at = |k: usize| stops.get(k.min(stops.len().saturating_sub(1))).copied().unwrap_or(Color32::GRAY);
            let c = lerp(at(seg), at(seg + 1), local);
            let x0 = rect.left() + rect.width() * a;
            let x1 = rect.left() + rect.width() * b;
            p.rect_filled(Rect::from_min_max(pos2(x0, y - h - 0.5), pos2(x1 + 0.5, y + h + 0.5)), 0.0, c);
        }
    } else {
        p.rect_filled(Rect::from_min_max(pos2(rect.left(), y - h), pos2(rect.right(), y + h)), 1.0, t.track);
    }
    // gap around the hollow thumb
    p.rect_filled(Rect::from_center_size(pos2(thumb_x, y), vec2(ring * 2.0 + 4.0, 6.0)), 0.0, t.chrome);
}

/// The colour of a grey track shaded `shade` at `x` (0 = left end, 1 = right end): between the
/// theme's track colour pulled toward its darkest and its lightest colour, so it stays subtle in
/// every theme. A plain shade is the track colour.
pub(crate) fn shade_color(shade: Shade, t: &Tokens, x: f32) -> Color32 {
    let x = if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.5 };
    let lum = |c: Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
    let (darkest, lightest) = if lum(t.chrome) < lum(t.text) { (t.chrome, t.text) } else { (t.text, t.chrome) };
    let dark = t.track.lerp_to_gamma(darkest, 0.4);
    let light = t.track.lerp_to_gamma(lightest, 0.3);
    match shade {
        Shade::Plain => t.track,
        Shade::Ramp => dark.lerp_to_gamma(light, x),
        Shade::ReverseRamp => light.lerp_to_gamma(dark, x),
        Shade::CentreLight => dark.lerp_to_gamma(light, 1.0 - (2.0 * x - 1.0).abs()),
        Shade::Tinted => dark.lerp_to_gamma(light.lerp_to_gamma(Color32::from_rgb(0xc8, 0x40, 0x40), 0.45), x),
    }
}

/// Result of a slider interaction this frame.
#[derive(Default, Debug, Clone, Copy)]
pub struct SliderOut {
    pub value: Option<f64>,
    pub drag_started: bool,
    pub drag_stopped: bool,
    pub reset: bool,
    /// A pill with more settings behind it was long-pressed or right-clicked ([`pill`]).
    pub more: bool,
}

/// What a value typed into a slider asks for (issue #322): a number such as `0.5`, `+12`, `-1,5`
/// (comma or point) or `5600 K`, on the control's steps and clamped to its range. `None` when it
/// isn't a finite number, so the slider keeps its value.
pub fn typed_value(spec: &ControlSpec, text: &str) -> Option<f64> {
    let t = text.trim().replace(',', ".");
    // a unit typed after the number: "%", "K", "°" (an angle)
    let t = t.trim_end_matches(|c: char| c == '%' || c == 'K' || c == 'k' || c == '°' || c.is_whitespace());
    let v: f64 = t.parse().ok().filter(|v: &f64| v.is_finite())?;
    let step = spec.step.max(1e-9);
    Some(((v / step).round() * step).clamp(spec.min, spec.max))
}

/// A slider's value as shown next to its label.
pub(crate) fn shown_value(spec: &ControlSpec, v: f64) -> String {
    let shown = if spec.id == "wb.temp" { format!("{v:.0}") } else { spec.format(v).replace("+0.00", "0").replace("-0.00", "0") };
    if shown == "+0" || shown == "-0" { "0".to_string() } else { shown }
}

/// Pointer travel (points, sideways from the press) that turns a press on a slider's value into a
/// scrub rather than a click that types a value.
const SCRUB_START: f32 = 3.0;

/// How far (points) the pointer moves after a press on a slider's track before the press drags it: any move, so the
/// thumb follows from the first frame (ported from OneSlips, NOTES-1028 note 15).
const DRAG_START: f32 = 1.0;

/// The radius of a slider's thumb: a small solid dot, as in DaVinci Resolve (issue #346: the
/// larger ring crowded the panel).
const THUMB_RING: f32 = 4.0;

/// Size of a slider's name and value in a panel (issue #346: small enough that names fit).
const LABEL_FONT: f32 = 10.5;

/// The font of a panel slider's name and value: [`LABEL_FONT`] when compact, the interface's
/// regular text (`TextStyle::Body`, as dialog form sliders) at Full Size (Settings ▸ Interface).
fn slider_font(ui: &Ui) -> egui::FontId {
    match crate::theme::slider_text(ui.ctx()) {
        crate::theme::SliderText::Compact => Tokens::get(ui.ctx()).font(LABEL_FONT),
        crate::theme::SliderText::FullSize => egui::TextStyle::Body.resolve(ui.style()),
    }
}

/// How much larger the slider text is than [`LABEL_FONT`], to widen the value and name columns.
fn text_scale(font: &egui::FontId) -> f32 {
    if font.size.is_finite() && font.size > 0.0 { (font.size / LABEL_FONT).max(1.0) } else { 1.0 }
}

/// A slider row: the name (left-aligned, flush with section headers),
/// a track with a hollow ring thumb occupying most of the panel width, and a compact value readout.
/// Double-click the label or thumb to reset (hovering either says so). Shift-drag = fine adjustment.
/// Scroll sideways over it to nudge it. Click the value to type one: Return or clicking away applies it,
/// Esc keeps the old one. Drag the value sideways to scrub it, as if dragging the thumb (Shift: a tenth
/// as far).
pub fn slider(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, label_override: Option<&str>) -> SliderOut {
    slider_laid_out(ui, spec, value, enabled, label_override, None)
}

/// [`slider`] as a row of a dialog form: the name right-aligned in the form's label column
/// ([`crate::dialog_frame::label_width`]), the track and value in its control column, so the name
/// has the room every other row's label has.
pub fn form_slider(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool) -> SliderOut {
    let label_w = crate::dialog_frame::label_width(ui);
    slider_laid_out(ui, spec, value, enabled, None, Some(label_w))
}

fn slider_laid_out(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, label_override: Option<&str>, form_label: Option<f32>) -> SliderOut {
    if form_label.is_none() && crate::theme::slider_layout(ui.ctx()) == crate::theme::SliderLayout::Pill {
        return pill_slider(ui, spec, value, enabled, label_override, false);
    }
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    // a panel slider on two rows (Settings ▸ Interface): name and value, then the track under them
    let two_rows = form_label.is_none() && crate::theme::slider_layout(ui.ctx()) == crate::theme::SliderLayout::TwoRows;
    let font = slider_font(ui);
    let k = text_scale(&font);
    let compact = form_label.is_none() && crate::theme::compact(ui.ctx());
    let row_h = if two_rows {
        t.slider_row_h + TWO_ROW_EXTRA + TWO_ROW_TEXT_H * (k - 1.0)
    } else if compact {
        // one dense line, still tall enough for the text at its size
        crate::theme::COMPACT_SLIDER_ROW_H.max(font.size + 4.0)
    } else {
        t.slider_row_h
    };
    let (row, _) = ui.allocate_exact_size(vec2(w, row_h), Sense::hover());
    // one row: name (left-aligned flush with section header) | wide track | compact value
    let label_str = crate::i18n::tr(label_override.unwrap_or(spec.label)).to_string();
    let geo = match form_label {
        Some(label_w) => SliderGeometry::form(row, label_w),
        None if two_rows => SliderGeometry::two_rows(row, k),
        // Compact: the name column fitted to the longest name of its section or flyout
        None if compact => {
            let name_w = ui.painter().layout_no_wrap(label_str.clone(), font.clone(), t.text_label).size().x.ceil();
            SliderGeometry::dense(row, name_column(ui, name_w).clamp(24.0_f32.min(max_label_col(w, k)), max_label_col(w, k)), k)
        }
        None => SliderGeometry::new(row, label_column(ui, w, &font, k), k),
    };
    let (label_rect, track_rect, value_rect) = (geo.label, geo.track, geo.value);
    let id = ui.id().with(spec.id);
    let resp = ui.interact(track_rect.expand2(vec2(THUMB_RING - 1.0, 0.0)), id, if enabled { Sense::click_and_drag() } else { Sense::hover() });
    // Keyboard focus survives leaving the row; vertical wheel edits need a fresh click or drag
    // in it (at most one slider is armed: `wheel_slider_key`).
    let wheel_id = id.with("wheel-rem");
    let mut wheel_armed = ui.data(|d| d.get_temp::<egui::Id>(wheel_slider_key())) == Some(id);
    let pointer_in_row = ui.rect_contains_pointer(row);
    // Every move this frame counts, not only where the pointer ended up: a slow frame (a slider
    // drag, soft proofing) can merge leaving the row and coming back into one.
    let left_row = wheel_armed
        && ui.input(|i| {
            i.events.iter().any(|e| match e {
                egui::Event::PointerMoved(p) => !row.contains(*p),
                egui::Event::PointerGone => true,
                _ => false,
            })
        });
    // Tab reaching the slider selects it for arrow keys, not vertical wheel edits.
    if enabled && resp.gained_focus() {
        ui.data_mut(|d| d.insert_temp(focused_slider_key(), id));
    }
    let selected = enabled && (resp.clicked() || resp.drag_started());
    if selected {
        resp.request_focus();
        ui.data_mut(|d| d.insert_temp(focused_slider_key(), id));
    }
    // egui's arrow-key focus moves must not carry the focus to a neighbouring widget: the slider
    // keeps Up / Down and Left / Right reach the photo shortcuts (#880)
    if resp.has_focus() {
        let lock = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
        ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, lock));
    }
    // A drag that leaves the row disarms too (it keeps dragging: that needs no wheel selection),
    // and one that only starts once the pointer is outside doesn't arm it again.
    if selected && pointer_in_row && !left_row {
        ui.data_mut(|d| {
            d.insert_temp(wheel_slider_key(), id);
            d.remove::<f32>(wheel_id);
        });
        wheel_armed = true;
    } else if wheel_armed && (!enabled || !resp.has_focus() || !pointer_in_row || left_row) {
        ui.data_mut(|d| {
            d.remove::<egui::Id>(wheel_slider_key());
            d.remove::<f32>(wheel_id);
        });
        wheel_armed = false;
    }
    // screen readers: a slider named after its control, with its value
    let label_text = crate::i18n::tr(label_override.unwrap_or(spec.label)).to_string();
    resp.widget_info(|| egui::WidgetInfo::slider(enabled, value, label_text.clone()));
    // and its range, step and the value as shown ("+0.50", "5600")
    let shown = shown_value(spec, value);
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_min_numeric_value(spec.min);
        node.set_max_numeric_value(spec.max);
        node.set_numeric_value_step(spec.step);
        node.set_value(shown);
        if enabled {
            node.add_action(egui::accesskit::Action::Increment);
            node.add_action(egui::accesskit::Action::Decrement);
        }
    });
    // the name: double-click it to reset, as the thumb (a disabled slider's name is only text)
    let label_resp = ui.interact(label_rect, id.with("label"), if enabled { Sense::click() } else { Sense::hover() });
    // say how to reset (issue #1436: the double-click was hard to discover), as the curve channels do
    let reset_hint = crate::i18n::tr_format!("{name}: double-click to reset it", name = label_text);
    let label_resp = if enabled { label_resp.on_hover_text(reset_hint.as_str()) } else { label_resp };
    crate::widgets::announce(&label_resp, crate::i18n::tr_format!("Reset {name}", name = label_text.clone()));
    register(ui.ctx(), format!("sliderLabel:{}", spec.id), label_rect);
    register(ui.ctx(), format!("slider:{}", spec.id), track_rect);
    // the value: click it to type an exact one, drag it sideways to scrub
    let value_resp = ui.interact(value_rect, id.with("value"), if enabled { Sense::click_and_drag() } else { Sense::hover() });
    crate::widgets::announce(&value_resp, format!("{label_text}: {}", crate::i18n::tr("Click to type a value, drag to change it")));
    register(ui.ctx(), format!("sliderValue:{}", spec.id), value_rect);
    // say that an exact value can be typed here (issue #534: an exact crop angle was hard to find)
    let value_resp = if enabled { value_resp.on_hover_text(crate::i18n::tr("Click to type a value, drag to change it")) } else { value_resp };
    let typing_id = id.with("typing");
    // Keep the short focus-acquisition guard, but let an explicit Return/Escape end the edit.
    let mut typing: Option<(String, u8, bool)> = ui.data(|m| m.get_temp(typing_id));
    // A scrub: the unrounded track position (0..1) it has reached, so a slow drag adds up instead
    // of rounding back to the same step every frame.
    let scrub_id = id.with("scrub-pos");
    let mut scrub: Option<f64> = if value_resp.dragged() || value_resp.drag_stopped() { ui.data(|m| m.get_temp(scrub_id)) } else { None };
    let mut scrub_started = false;
    if enabled
        && scrub.is_none()
        && value_resp.dragged()
        && let (Some(origin), Some(p)) = ui.input(|i| (i.pointer.press_origin(), i.pointer.interact_pos()))
        && (p.x - origin.x).abs() >= SCRUB_START
    {
        // a press on the value that moves sideways scrubs: the edit the press opened is dropped
        typing = None;
        ui.data_mut(|m| m.remove::<(String, u8, bool)>(typing_id));
        ui.memory_mut(|m| m.surrender_focus(crate::text_field::id(&format!("sliderField:{}", spec.id))));
        // the travel so far counts, so the value doesn't lag the pointer by the start distance
        let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
        scrub = Some((spec.position(value) + f64::from((p.x - origin.x) / track_rect.width()) * fine).clamp(0.0, 1.0));
        scrub_started = true;
    }
    if scrub.is_none() {
        ui.data_mut(|m| m.remove::<f64>(scrub_id));
    }
    let value_pressed = value_resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
    // Focus on the owned primary press before fast typing; retain the readout's click activation.
    if enabled && typing.is_none() && (value_resp.clicked() || value_pressed) {
        typing = Some((shown_value(spec, value).trim_start_matches('+').to_string(), 0, value_pressed));
    }
    let value_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
    // A release over the value keeps the field the press opened, however long the button was held
    // (a press held past egui's click time ends without a click). A release elsewhere cancels the
    // gesture, and a press that scrubs already dropped the field above.
    let released_over_value = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| value_rect.contains(p));
    if value_released && !released_over_value && typing.as_ref().is_some_and(|(_, _, pressed)| *pressed) && !value_resp.clicked() {
        typing = None;
        ui.data_mut(|m| m.remove::<(String, u8, bool)>(typing_id));
        ui.memory_mut(|m| m.surrender_focus(crate::text_field::id(&format!("sliderField:{}", spec.id))));
    }
    let mut out = SliderOut::default();
    // value ↔ track position: linear, except for the Kelvin temperature (`ControlSpec::position`)
    let to_x = |v: f64| track_rect.left() + spec.position(v) as f32 * track_rect.width();
    let from_x = |x: f32| spec.value_at(f64::from((x - track_rect.left()) / track_rect.width()));
    // the thumb: a click there doesn't move the value, and resting on it says how to reset (the rest
    // of the track, which is clicked and dragged, gets no tooltip)
    let on_thumb = |p: egui::Pos2| (p.x - to_x(value)).abs() <= THUMB_RING + 1.0;
    if enabled && !resp.dragged() && resp.hover_pos().is_some_and(on_thumb) {
        resp.clone().on_hover_text(reset_hint.as_str());
    }
    let mut v = value;
    if let Some(mut pos) = scrub {
        // scrubbing the value moves it as dragging the thumb would: the pointer's sideways travel
        // over the track's width (a tenth with Shift), on the control's steps
        if !scrub_started && value_resp.dragged() {
            let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
            let dx = ui.input(|i| i.pointer.delta().x);
            // with acceleration on the number, slow movement moves it less (Settings ▸ Slider control)
            let gain = if fine == 1.0 && crate::theme::slider_control(ui.ctx()).on_number() { drag_gain(dx) } else { 1.0 };
            pos = (pos + f64::from(dx * gain / track_rect.width()) * fine).clamp(0.0, 1.0);
        }
        let step = spec.step.max(1e-9);
        let nv = ((spec.value_at(pos) / step).round() * step).clamp(spec.min, spec.max);
        if (nv - value).abs() > 1e-12 {
            out.value = Some(nv);
            v = nv;
        }
        out.drag_started = scrub_started;
        if value_resp.drag_stopped() {
            out.drag_stopped = true;
            ui.data_mut(|m| m.remove::<f64>(scrub_id));
        } else {
            ui.data_mut(|m| m.insert_temp(scrub_id, pos));
        }
    } else if let Some((mut text, frames, pressed)) = typing.take() {
        // Keep the owned opening release focused without restarting its draft or selection.
        // egui gives that release's click to the readout, so the field under it gives up the focus
        // whatever frame it lands on (issue #1362): the field takes it back after drawing too.
        let opening_release = pressed && value_released;
        if opening_release {
            ui.memory_mut(|m| m.request_focus(crate::text_field::id(&format!("sliderField:{}", spec.id))));
        }
        let pressed = pressed && !value_released;
        // a child over the value, so the field never moves the rows around it
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(value_rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
        // the old value is selected when the field takes the focus, so typing replaces it
        let field = crate::text_field::TextField::singleline(&format!("sliderField:{}", spec.id), &mut text)
            .font(if form_label.is_some() { t.font(LABEL_FONT) } else { font.clone() })
            .horizontal_align(egui::Align::RIGHT)
            .width(value_rect.width())
            .margin(egui::Margin::ZERO)
            .select_on_focus(true)
            .begin_edit(frames == 0)
            .show(&mut child);
        if (frames < 2 || opening_release) && !matches!(field.ending, Some(crate::text_field::Ending::Return | crate::text_field::Ending::Escape)) {
            field.response.request_focus();
            let frames = frames.saturating_add(1);
            typing = Some((text.clone(), frames, pressed));
            ui.data_mut(|m| m.insert_temp(typing_id, (text, frames, pressed)));
        } else if field.ending.is_some() || !field.editing || !enabled {
            // Esc (or the slider turning off) keeps the old value; Return or clicking away applies
            let cancelled = !enabled || field.cancelled();
            if let Some(nv) = typed_value(spec, &text).filter(|nv| !cancelled && (nv - value).abs() > 1e-12) {
                out.value = Some(nv);
                out.drag_started = true;
                out.drag_stopped = true;
                v = nv;
            }
            ui.data_mut(|m| m.remove::<(String, u8, bool)>(typing_id));
        } else {
            typing = Some((text.clone(), frames, pressed));
            ui.data_mut(|m| m.insert_temp(typing_id, (text, frames, pressed)));
        }
    } else if resp.double_clicked() || label_resp.double_clicked() {
        out.reset = true;
        out.value = Some(spec.default);
        v = spec.default;
    } else {
        // The drag starts with the pointer's first move after the press, not once it has gone past egui's click
        // distance (6 points): a slow, careful drag moved nothing for a second or two (ported from OneSlips,
        // NOTES-1028 note 15). Held as this slider's own state until the button is let go.
        let early_id = id.with("drag-early");
        let pressed = enabled && resp.is_pointer_button_down_on();
        let was: bool = ui.data(|m| m.get_temp(early_id)).unwrap_or(false);
        let moved = ui.input(|i| match (i.pointer.press_origin(), i.pointer.interact_pos()) {
            (Some(a), Some(b)) => a.distance(b) >= DRAG_START,
            _ => false,
        });
        let start_now = enabled && !was && (resp.drag_started() || (pressed && moved));
        let dragging = start_now || (was && (pressed || resp.dragged()));
        let ended = was && !dragging;
        ui.data_mut(|m| {
            if dragging {
                m.insert_temp(early_id, true);
            } else {
                m.remove::<bool>(early_id);
            }
        });
        if start_now {
            out.drag_started = true;
        }
        if dragging && let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
            let fine = ui.input(|i| i.modifiers.shift);
            let step = spec.step.max(1e-9);
            // the thumb's track position under pointer x, less where on the thumb it was grabbed
            let grab_id = id.with("grab");
            let at = |x: f32, grab: f32| f64::from((x - grab - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
            let nv = if fine {
                // a tenth of the pointer's movement along the track; the unrounded position is kept, so a
                // slow drag adds up instead of rounding back to the same step every frame
                let fine_id = id.with("fine-pos");
                let dx = ui.input(|i| i.pointer.delta().x);
                let kept = ui.data(|m| m.get_temp::<f64>(fine_id)).filter(|&p| ((spec.value_at(p) / step).round() * step - value).abs() < step / 2.0);
                let pos = (kept.unwrap_or_else(|| spec.position(value)) + f64::from(dx / track_rect.width()) * 0.1).clamp(0.0, 1.0);
                ui.data_mut(|m| {
                    m.insert_temp(fine_id, pos);
                    // letting go of Shift carries on from here: the thumb stays where it is
                    m.insert_temp(grab_id, p.x - (track_rect.left() + pos as f32 * track_rect.width()));
                    m.insert_temp(id.with("accel-pos"), pos);
                });
                spec.value_at(pos)
            } else if start_now {
                // pressed on the thumb, it keeps the spot it was grabbed by; elsewhere on the track it
                // comes under the pointer
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
                let grab = if on_thumb(origin) { origin.x - to_x(value) } else { 0.0 };
                let pos = at(p.x, grab);
                ui.data_mut(|m| {
                    m.insert_temp(grab_id, grab);
                    m.insert_temp(id.with("accel-pos"), pos);
                });
                spec.value_at(pos)
            } else if crate::theme::slider_control(ui.ctx()).on_slider() {
                // with acceleration on the slider (Settings ▸ Slider control, issue #346): slow movement
                // moves the thumb at 30 % speed, for fine changes; a quick one at full speed
                let accel_id = id.with("accel-pos");
                let dx = ui.input(|i| i.pointer.delta().x);
                let pos = (ui.data(|m| m.get_temp::<f64>(accel_id)).unwrap_or_else(|| spec.position(value))
                    + f64::from(dx * drag_gain(dx) / track_rect.width()))
                .clamp(0.0, 1.0);
                ui.data_mut(|m| m.insert_temp(accel_id, pos));
                spec.value_at(pos)
            } else {
                // then it stays under the pointer, as in Lightroom Classic: past an end it waits there
                // until the pointer comes back over it
                let grab = ui.data(|m| m.get_temp::<f32>(grab_id)).unwrap_or(0.0);
                spec.value_at(at(p.x, grab))
            };
            let nv = ((nv / step).round() * step).clamp(spec.min, spec.max);
            if (nv - value).abs() > 1e-12 {
                out.value = Some(nv);
                v = nv;
            }
        } else if !ended
            && resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
            // a click on the thumb leaves it there, so the first click of a double-click on it isn't a
            // history step of its own before the reset
            && !on_thumb(p)
        {
            let step = spec.step.max(1e-9);
            let nv = ((from_x(p.x) / step).round() * step).clamp(spec.min, spec.max);
            out.value = Some(nv);
            v = nv;
            out.drag_started = true;
            out.drag_stopped = true;
        }
        if ended || (!was && !dragging && resp.drag_stopped()) {
            out.drag_stopped = true;
        }
        // a screen reader's increment / decrement (VoiceOver: Ctrl-Option-Up / Down)
        let (inc, dec) = ui.input(|i| {
            i.events.iter().fold((false, false), |(inc, dec), e| match e {
                egui::Event::AccessKitActionRequest(r) if r.target_node == resp.id.accesskit_id() => {
                    (inc || r.action == egui::accesskit::Action::Increment, dec || r.action == egui::accesskit::Action::Decrement)
                }
                _ => (inc, dec),
            })
        });
        // Arrow keys work with either a hovered or a focused slider.
        if enabled && out.value.is_none() && typing.is_none() && (pointer_in_row || resp.has_focus() || inc || dec) {
            let (up, down, shift) = ui.input_mut(|i| {
                let shift = i.modifiers.shift;
                let m = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
                (i.consume_key(m, egui::Key::ArrowUp), i.consume_key(m, egui::Key::ArrowDown), shift)
            });
            let (up, down) = (up || inc, down || dec);
            if up || down {
                let nv = nudged(spec, value, if up { 1.0 } else { -1.0 } * if shift { 5.0 } else { 1.0 });
                if (nv - value).abs() > 1e-12 {
                    out.value = Some(nv);
                    out.drag_started = true;
                    out.drag_stopped = true;
                    v = nv;
                }
            }
        }
        // Sideways scrolling (a trackpad swipe, a tilting wheel, Shift+wheel) adjusts the slider under
        // the pointer, selected or not: the panel only scrolls vertically, so nothing else wants it.
        // Only a mostly sideways movement counts, so a slightly diagonal swipe that scrolls the panel
        // leaves the sliders it passes alone. Right raises the value, as dragging the thumb right does.
        let side_id = id.with("side-rem");
        let (dx, dy) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y));
        if !enabled || typing.is_some() || out.value.is_some() || !pointer_in_row || dx.abs() <= dy.abs() {
            ui.data_mut(|m| m.remove::<f32>(side_id));
        } else {
            // the vertical part would scroll the panel and move the slider out from under the pointer
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            let mut remainder = ui.data(|m| m.get_temp::<f32>(side_id)).unwrap_or(0.0);
            let steps = wheel_nudges(&mut remainder, dx);
            ui.data_mut(|m| m.insert_temp(side_id, remainder));
            let nv = nudged(spec, value, steps);
            if steps != 0.0 && (nv - value).abs() > 1e-12 {
                out.value = Some(nv);
                out.drag_started = true;
                out.drag_stopped = true;
                v = nv;
            }
        }
        // Wheel nudges require an explicitly selected slider, unlike regular panel scrolling.
        // Consume only the vertical component so a trackpad's horizontal scroll still works.
        // (`wheel_armed` is cleared above with its remainder whenever focus or the pointer leaves.)
        if wheel_armed && enabled && typing.is_none() && out.value.is_none() && pointer_in_row {
            let delta = ui.input_mut(|i| {
                let dy = i.smooth_scroll_delta.y;
                if dy != 0.0 {
                    i.smooth_scroll_delta.y = 0.0;
                }
                dy
            });
            if delta != 0.0 {
                let mut remainder = ui.data(|m| m.get_temp::<f32>(wheel_id)).unwrap_or(0.0);
                let steps = wheel_nudges(&mut remainder, delta);
                ui.data_mut(|m| m.insert_temp(wheel_id, remainder));
                if steps != 0.0 {
                    let nv = nudged(spec, value, steps);
                    if (nv - value).abs() > 1e-12 {
                        out.value = Some(nv);
                        out.drag_started = true;
                        out.drag_stopped = true;
                        v = nv;
                    }
                }
            }
        }
    }
    // paint
    let hovered = resp.hovered() || resp.dragged();
    let text_c = if enabled { t.text_label } else { t.text_disabled };
    let p = ui.painter();
    // the name, right-aligned against its track; ellipsized when the column is too narrow for it
    // (in a form, the font of the other rows' labels)
    let label_font = if form_label.is_some() { egui::TextStyle::Body.resolve(ui.style()) } else { font.clone() };
    let mut job = egui::text::LayoutJob::simple_singleline(label_str, label_font, text_c);
    job.wrap = egui::text::TextWrapping { max_width: label_rect.width(), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = ui.painter().layout_job(job);
    // (on two rows, left-aligned over the start of the track)
    let gx = if two_rows { label_rect.left() } else { label_rect.right() - galley.size().x };
    let gpos = pos2(gx, label_rect.center().y - galley.size().y / 2.0);
    p.galley(gpos, galley, text_c);
    if typing.is_none() {
        p.text(
            value_rect.right_center(),
            Align2::RIGHT_CENTER,
            shown_value(spec, v),
            if form_label.is_some() { t.font(LABEL_FONT) } else { font.clone() },
            text_c,
        );
        // the scrub pointer, as over Lightroom's values: a drag here changes the value
        if enabled && (value_resp.hovered() || scrub.is_some()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }
    let ring = THUMB_RING;
    let tx = to_x(v);
    // (the grey ramps are the Compact layout's; Classic's tracks stay flat)
    let shade = if compact { spec.shade } else { Shade::Plain };
    paint_track(ui, track_rect, &spec.track, shade, &t, tx, ring);
    let p = ui.painter();
    let ring_c = if !enabled {
        t.text_disabled
    } else if hovered {
        t.thumb_hover
    } else {
        t.thumb
    };
    p.circle_filled(pos2(tx, track_rect.center().y), ring, ring_c);
    p.circle_stroke(pos2(tx, track_rect.center().y), ring, Stroke::new(1.0, t.chrome));
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    out
}

/// A pill slider's height (points): big enough for a finger, small enough for a mouse.
pub(crate) const PILL_H: f32 = 40.0;

/// The Modern layout's slider (also Settings ▸ Sliders ▸ Pill): one rounded bar the width of the
/// panel with the name on the left and the value on the right, inside it. A drag anywhere on it
/// moves the value by the drag's travel over the bar's width (Shift: a tenth), so a tap never jumps
/// it; the colour grows from the default (the middle of a ± slider). Double-click resets, a click on
/// the value types one, sideways scrolling and the arrow keys nudge it. Registers the same ids as
/// [`slider`] (`slider:`, `sliderLabel:`, `sliderValue:`). Own design.
/// A [`pill_slider`] that says it has more settings behind it (`more`): a small mark after its name,
/// and a long press or right-click sets [`SliderOut::more`].
pub fn pill(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, more: bool) -> SliderOut {
    pill_slider(ui, spec, value, enabled, None, more)
}

/// How long a still press on a pill waits before it opens its settings (s).
const PILL_HOLD: f64 = 0.5;

fn pill_slider(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, label_override: Option<&str>, more: bool) -> SliderOut {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(w, PILL_H + 4.0), Sense::hover());
    let pill = row.shrink2(vec2(0.0, 2.0));
    let radius = pill.height() / 2.0;
    let name = crate::i18n::tr(label_override.unwrap_or(spec.label)).to_string();
    // Settings ▸ Slider text: Full Size makes the name and value larger, as on the other layouts
    let k = text_scale(&slider_font(ui));
    let font = t.semibold(13.0 * k);
    let value_font = t.font(13.0 * k);
    let id = ui.id().with(spec.id);
    let value_w = (72.0 * k).min(pill.width() * 0.4);
    let value_rect = Rect::from_min_max(pos2(pill.right() - radius * 0.6 - value_w, pill.top()), pos2(pill.right() - radius * 0.6, pill.bottom()));
    let label_rect =
        Rect::from_min_max(pos2(pill.left() + radius * 0.6, pill.top()), pos2((value_rect.left() - 6.0).max(pill.left()), pill.bottom()));
    let resp = ui.interact(pill, id, if enabled { Sense::click_and_drag() } else { Sense::hover() });
    let value_resp = ui.interact(value_rect, id.with("value"), if enabled { Sense::click() } else { Sense::hover() });
    register(ui.ctx(), format!("slider:{}", spec.id), pill);
    register(ui.ctx(), format!("sliderLabel:{}", spec.id), label_rect);
    register(ui.ctx(), format!("sliderValue:{}", spec.id), value_rect);
    resp.widget_info(|| egui::WidgetInfo::slider(enabled, value, name.clone()));
    let shown = shown_value(spec, value);
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_min_numeric_value(spec.min);
        node.set_max_numeric_value(spec.max);
        node.set_numeric_value_step(spec.step);
        node.set_value(shown);
        if enabled {
            node.add_action(egui::accesskit::Action::Increment);
            node.add_action(egui::accesskit::Action::Decrement);
        }
    });
    if enabled && (resp.clicked() || resp.drag_started()) {
        resp.request_focus();
    }
    // egui's arrow-key focus moves must not carry the focus to a neighbouring widget: the slider
    // keeps Up / Down and Left / Right reach the photo shortcuts (#880)
    if resp.has_focus() {
        let lock = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
        ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, lock));
    }
    let mut out = SliderOut::default();
    if more && enabled {
        // a right-click, or a still press held a moment, opens the settings behind it (once a press)
        let held_id = id.with("more-held");
        let held = ui.input(|i| i.pointer.press_start_time().map(|t| i.time - t).unwrap_or(0.0));
        let still = ui.input(|i| match (i.pointer.press_origin(), i.pointer.interact_pos()) {
            (Some(a), Some(b)) => a.distance(b) < 8.0,
            _ => true,
        });
        let fired: bool = ui.data(|m| m.get_temp(held_id)).unwrap_or(false);
        if resp.is_pointer_button_down_on() && !resp.dragged() {
            if !fired && held >= PILL_HOLD && still {
                out.more = true;
                ui.data_mut(|m| m.insert_temp(held_id, true));
            } else if !fired {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        } else if fired && !resp.is_pointer_button_down_on() {
            ui.data_mut(|m| m.remove::<bool>(held_id));
        }
        if resp.secondary_clicked() {
            out.more = true;
        }
    }
    let step = spec.step.max(1e-9);
    let snap = |v: f64| ((v / step).round() * step).clamp(spec.min, spec.max);
    let mut v = value;
    // typing a value: (text, frames since it opened)
    let typing_id = id.with("typing");
    let field_id = format!("sliderField:{}", spec.id);
    let mut typing: Option<(String, u8)> = ui.data(|m| m.get_temp(typing_id));
    if enabled && typing.is_none() && value_resp.clicked() {
        typing = Some((shown_value(spec, value).trim_start_matches('+').to_string(), 0));
    }
    let pos_id = id.with("pill-pos");
    if let Some((mut text, frames)) = typing.take() {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(value_rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
        let field = crate::text_field::TextField::singleline(&field_id, &mut text)
            .font(value_font.clone())
            .horizontal_align(egui::Align::RIGHT)
            .width(value_rect.width())
            .margin(egui::Margin::ZERO)
            .select_on_focus(true)
            .begin_edit(frames == 0)
            .show(&mut child);
        if frames < 2 && !matches!(field.ending, Some(crate::text_field::Ending::Return | crate::text_field::Ending::Escape)) {
            field.response.request_focus();
            typing = Some((text, frames.saturating_add(1)));
        } else if field.ending.is_some() || !field.editing || !enabled {
            // Esc keeps the old value; Return or clicking away applies the typed one
            let cancelled = !enabled || field.cancelled();
            if let Some(nv) = typed_value(spec, &text).filter(|nv| !cancelled && (nv - value).abs() > 1e-12) {
                out.value = Some(nv);
                out.drag_started = true;
                out.drag_stopped = true;
                v = nv;
            }
        } else {
            typing = Some((text, frames));
        }
        ui.data_mut(|m| match &typing {
            Some(t) => {
                m.insert_temp(typing_id, t.clone());
            }
            None => {
                m.remove::<(String, u8)>(typing_id);
            }
        });
    } else if resp.double_clicked() {
        out.reset = true;
        out.value = Some(spec.default);
        v = spec.default;
    } else {
        // (the drag starts with the first move, as on the track slider)
        let early_id = id.with("drag-early");
        let pressed = enabled && resp.is_pointer_button_down_on();
        let was: bool = ui.data(|m| m.get_temp(early_id)).unwrap_or(false);
        let moved = ui.input(|i| match (i.pointer.press_origin(), i.pointer.interact_pos()) {
            (Some(a), Some(b)) => a.distance(b) >= DRAG_START,
            _ => false,
        });
        let start_now = enabled && !was && (resp.drag_started() || (pressed && moved));
        let dragging = start_now || (was && (pressed || resp.dragged()));
        let ended = was && !dragging;
        ui.data_mut(|m| {
            if dragging {
                m.insert_temp(early_id, true);
            } else {
                m.remove::<bool>(early_id);
            }
        });
        if start_now {
            out.drag_started = true;
            ui.data_mut(|m| m.insert_temp(pos_id, spec.position(value)));
        }
        if dragging {
            let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
            let dx = ui.input(|i| i.pointer.delta().x);
            let width = f64::from(pill.width().max(1.0));
            let start = ui.data(|m| m.get_temp::<f64>(pos_id)).unwrap_or_else(|| spec.position(value));
            let pos = (start + f64::from(dx) / width * fine).clamp(0.0, 1.0);
            ui.data_mut(|m| m.insert_temp(pos_id, pos));
            let nv = snap(spec.value_at(pos));
            if (nv - value).abs() > 1e-12 {
                out.value = Some(nv);
                v = nv;
            }
        }
        if ended || (!was && !dragging && resp.drag_stopped()) {
            out.drag_stopped = true;
            ui.data_mut(|m| m.remove::<f64>(pos_id));
        }
        // the arrow keys (hovered or focused), a screen reader's increment / decrement
        let (inc, dec) = ui.input(|i| {
            i.events.iter().fold((false, false), |(inc, dec), e| match e {
                egui::Event::AccessKitActionRequest(r) if r.target_node == resp.id.accesskit_id() => {
                    (inc || r.action == egui::accesskit::Action::Increment, dec || r.action == egui::accesskit::Action::Decrement)
                }
                _ => (inc, dec),
            })
        });
        let pointer_in = ui.rect_contains_pointer(pill);
        if enabled && out.value.is_none() && (pointer_in || resp.has_focus() || inc || dec) {
            let (up, down, shift) = ui.input_mut(|i| {
                let shift = i.modifiers.shift;
                let m = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
                (i.consume_key(m, egui::Key::ArrowUp), i.consume_key(m, egui::Key::ArrowDown), shift)
            });
            let (up, down) = (up || inc, down || dec);
            if up || down {
                let nv = nudged(spec, value, if up { 1.0 } else { -1.0 } * if shift { 5.0 } else { 1.0 });
                if (nv - value).abs() > 1e-12 {
                    out.value = Some(nv);
                    out.drag_started = true;
                    out.drag_stopped = true;
                    v = nv;
                }
            }
        }
        // a mostly sideways scroll over it nudges it (the panel scrolls only up and down)
        let side_id = id.with("side-rem");
        let (dx, dy) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y));
        if !enabled || out.value.is_some() || !pointer_in || dx.abs() <= dy.abs() {
            ui.data_mut(|m| m.remove::<f32>(side_id));
        } else {
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            let mut remainder = ui.data(|m| m.get_temp::<f32>(side_id)).unwrap_or(0.0);
            let steps = wheel_nudges(&mut remainder, dx);
            ui.data_mut(|m| m.insert_temp(side_id, remainder));
            let nv = nudged(spec, value, steps);
            if steps != 0.0 && (nv - value).abs() > 1e-12 {
                out.value = Some(nv);
                out.drag_started = true;
                out.drag_stopped = true;
                v = nv;
            }
        }
    }
    // paint: the bar, the fill from the default to the value, the default's tick, name and value
    let p = ui.painter();
    let active = resp.dragged() || resp.has_focus();
    p.rect_filled(pill, radius, t.button);
    let to_x = |v: f64| pill.left() + spec.position(v) as f32 * pill.width();
    let (x0, x1) = (to_x(spec.default.clamp(spec.min, spec.max)), to_x(v));
    if (x1 - x0).abs() > 0.5 {
        let fill = Rect::from_x_y_ranges(x0.min(x1)..=x0.max(x1), pill.y_range());
        let c = if enabled { t.accent.gamma_multiply(if active { 0.75 } else { 0.55 }) } else { t.text_disabled.gamma_multiply(0.35) };
        p.with_clip_rect(fill.intersect(p.clip_rect())).rect_filled(pill, radius, c);
    }
    if let Some(stops) = track_stops(&spec.track) {
        // a coloured track (temperature, tint, hue…) as a thin line along the bottom
        let line = Rect::from_min_max(
            pos2(pill.left() + radius, pill.bottom() - 6.0),
            pos2((pill.right() - radius).max(pill.left() + radius), pill.bottom() - 4.0),
        );
        let n = stops.len().max(2) - 1;
        let segs = 32;
        let stop = |i: usize| stops.get(i.min(n)).copied().unwrap_or(t.track);
        for i in 0..segs {
            let (a, b) = (i as f32 / segs as f32, (i + 1) as f32 / segs as f32);
            let seg = (a * n as f32).floor() as usize;
            let c = lerp(stop(seg), stop(seg + 1), a * n as f32 - seg as f32);
            p.rect_filled(Rect::from_x_y_ranges(line.left() + line.width() * a..=line.left() + line.width() * b + 0.5, line.y_range()), 0.0, c);
        }
    }
    if spec.default > spec.min && spec.default < spec.max {
        p.vline(x0, pill.shrink(radius * 0.55).y_range(), Stroke::new(1.0, t.text_dim.gamma_multiply(0.5)));
    }
    if resp.hovered() && enabled && !active {
        p.rect_stroke(pill, radius, Stroke::new(1.0, t.accent.gamma_multiply(0.6)), StrokeKind::Inside);
    }
    let text_c = if enabled { t.text } else { t.text_disabled };
    let mut job = egui::text::LayoutJob::simple_singleline(name, font, text_c);
    job.wrap =
        egui::text::TextWrapping { max_width: label_rect.width().max(0.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = p.layout_job(job);
    let gw = galley.size().x;
    p.galley(pos2(label_rect.left(), label_rect.center().y - galley.size().y / 2.0), galley, text_c);
    let mut after = label_rect.left() + gw;
    if (v - spec.default).abs() > 1e-9 && !spec.id.starts_with("wb.") {
        // changed from its default: a small dot after the name
        p.circle_filled(pos2(after + 7.0, label_rect.center().y), 2.5, t.pick);
        after += 10.0;
    }
    if more {
        // more settings behind it: a small "more" mark (long press or right-click)
        let m = Rect::from_center_size(pos2(after + 12.0, label_rect.center().y), egui::Vec2::splat(12.0));
        crate::icons::paint(p, m, crate::icons::Icon::More, t.text_dim);
        register(ui.ctx(), format!("sliderMore:{}", spec.id), m);
    }
    if typing.is_none() {
        p.text(value_rect.right_center(), Align2::RIGHT_CENTER, shown_value(spec, v), value_font, text_c);
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    out
}

/// Where a one-row slider's parts go: name column, track and value.
struct SliderGeometry {
    label: Rect,
    track: Rect,
    value: Rect,
}

/// How much taller a two-row slider is than a one-row one.
const TWO_ROW_EXTRA: f32 = 14.0;

/// Height of a two-row slider's name and value line at [`LABEL_FONT`].
const TWO_ROW_TEXT_H: f32 = 12.0;

/// Width of a slider's value column: fits numbers like "+100", "8978", "+0.80", "-5.00" at
/// [`LABEL_FONT`].
const VALUE_W: f32 = 28.0;

/// Space between a Compact slider row's name, track and value.
const DENSE_GAP: f32 = 6.0;

/// Gap between a Compact slider's right-aligned name and its track: room for the thumb sitting at
/// the track's left end (and its hover halo), so a name never runs into it.
const NAME_GAP: f32 = DENSE_GAP + THUMB_RING + 1.5;

/// Maximum width of the slider label column: room for names such as "Vignetting Midpoint" or
/// "Shadows Luminance" at [`LABEL_FONT`] (issue #346: names were cut off), while the track keeps
/// more than half the panel.
const MAX_LABEL_COL: f32 = 76.0;

impl SliderGeometry {
    fn new(row: Rect, label_col: f32, k: f32) -> Self {
        let (pad_l, pad_r, gap) = (12.0, 12.0, 8.0);
        let label = Rect::from_min_size(pos2(row.left() + pad_l, row.top()), vec2(label_col, row.height()));
        let value =
            Rect::from_min_max(pos2((row.right() - pad_r - VALUE_W * k).max(label.right()), row.top()), pos2(row.right() - pad_r, row.bottom()));
        let track_l = label.right() + gap;
        let track_r = (value.left() - gap).max(track_l + 1.0);
        let track = Rect::from_min_max(pos2(track_l, row.top()), pos2(track_r, row.bottom()));
        Self { label, track, value }
    }

    /// The Compact layout's row (LAYOUT-DENSE, ported from OneSlips): on the rows' shared margins
    /// ([`PAD_L`], [`PAD_R`]), the name right-aligned in its column [`NAME_GAP`] before the track,
    /// so it never runs into the thumb at the track's left end (the gap shrinks in a very narrow
    /// row).
    fn dense(row: Rect, label_col: f32, k: f32) -> Self {
        let left = row.left() + PAD_L;
        let right = (row.right() - PAD_R).max(left);
        let w = right - left;
        let label = Rect::from_min_size(pos2(left, row.top()), vec2(label_col.min(w).max(0.0), row.height()));
        let value = Rect::from_min_max(pos2((right - VALUE_W * k).max(label.right()), row.top()), pos2(right, row.bottom()));
        let gap = NAME_GAP.min(DENSE_GAP.max(w * 0.05));
        let track_l = label.right() + gap;
        let track_r = (value.left() - DENSE_GAP).max(track_l + 1.0);
        let track = Rect::from_min_max(pos2(track_l, row.top()), pos2(track_r, row.bottom()));
        Self { label, track, value }
    }

    /// Two rows: the name at the left and the value at the right of the top line, the track the
    /// width of both under them (its thumb at either end just inside the row's padding).
    fn two_rows(row: Rect, k: f32) -> Self {
        let (pad_l, pad_r) = (12.0, 12.0);
        // the name and value sit close over their own track, with the gap above them, so each
        // slider reads as one unit and the rows stand apart (issue #346)
        let split = (row.bottom() - 14.0).max(row.top());
        let text_top = (split - TWO_ROW_TEXT_H * k).max(row.top());
        let value = Rect::from_min_max(pos2((row.right() - pad_r - VALUE_W * k).max(row.left() + pad_l), text_top), pos2(row.right() - pad_r, split));
        let label = Rect::from_min_max(pos2(row.left() + pad_l, text_top), pos2((value.left() - 8.0).max(row.left() + pad_l), split));
        let track_l = row.left() + pad_l + THUMB_RING;
        let track_r = (row.right() - pad_r - THUMB_RING).max(track_l + 1.0);
        let track = Rect::from_min_max(pos2(track_l, split), pos2(track_r, row.bottom()));
        Self { label, track, value }
    }

    /// A form row: the name in the label column (`label_w`), the track from the control column
    /// (the thumb at its minimum just inside it), the value at the right.
    fn form(row: Rect, label_w: f32) -> Self {
        let pad_r = 8.0;
        let label = Rect::from_min_size(row.left_top(), vec2(label_w.min(row.width()), row.height()));
        let value = Rect::from_min_max(pos2((row.right() - pad_r - VALUE_W).max(label.right()), row.top()), pos2(row.right() - pad_r, row.bottom()));
        let track_l = label.right() + crate::dialog_frame::LABEL_GAP + THUMB_RING;
        let track_r = (value.left() - 12.0).max(track_l + 1.0);
        let track = Rect::from_min_max(pos2(track_l, row.top()), pos2(track_r, row.bottom()));
        Self { label, track, value }
    }
}

/// The name column's width for a panel slider: the widest name of any develop control at
/// [`LABEL_FONT`] in the current language, so the column (and every track) stays the same width as
/// sections and flyouts open and close (issue #397), strictly capped so the interactive track
/// occupies most of the panel width (names past it are ellipsized).
fn label_column(ui: &Ui, row_w: f32, font: &egui::FontId, k: f32) -> f32 {
    let key = egui::Id::new("slider-label-column");
    let lang = format!("{:?}/{}", crate::i18n::language(), font.size);
    let widest = match ui.data(|d| d.get_temp::<(String, f32)>(key)) {
        Some((l, w)) if l == lang => w,
        _ => {
            let t = Tokens::get(ui.ctx());
            let w = lcc_develop::controls::CONTROLS
                .iter()
                .chain(lcc_develop::controls::INDEXED)
                .map(|c| ui.painter().layout_no_wrap(crate::i18n::tr(c.label).to_string(), font.clone(), t.text_label).size().x)
                .fold(0.0_f32, f32::max)
                .ceil();
            ui.data_mut(|d| d.insert_temp(key, (lang, w)));
            w
        }
    };
    let max_col = max_label_col(row_w, k);
    widest.clamp(24.0_f32.min(max_col), max_col)
}

/// The widest a slider's name column may be in a row `row_w` wide, so the track keeps most of it.
fn max_label_col(row_w: f32, k: f32) -> f32 {
    ((row_w - 48.0) * (0.34 * k).min(0.42)).clamp(24.0, MAX_LABEL_COL * k)
}

/// Where the current slider group is kept, per layer (a dialog's sliders aren't the panel's).
fn group_slot(layer: egui::LayerId) -> egui::Id {
    egui::Id::new(("slider-group", layer))
}

/// Starts a group of sliders whose names share one column in the Compact layout (a section:
/// [`section_header`] calls it). Sliders drawn outside any group this pass share a column with
/// the others in their `Ui`.
pub(crate) fn begin_slider_group(ui: &Ui, id: &str) {
    let pass = ui.ctx().cumulative_pass_nr();
    ui.data_mut(|d| d.insert_temp(group_slot(ui.layer_id()), (egui::Id::new(("slider-names", id)), pass)));
}

/// Draws `body` as a slider group of its own (a flyout such as SDR Rendition or Defringe): in the
/// Compact layout its names get their own column, so a few long ones neither shorten the tracks
/// of the section around it nor move them as it opens and closes. The section's group carries on
/// after it.
pub(crate) fn slider_group<R>(ui: &mut Ui, id: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    let slot = group_slot(ui.layer_id());
    let outer = ui.data(|d| d.get_temp::<(egui::Id, u64)>(slot));
    begin_slider_group(ui, id);
    let out = body(ui);
    ui.data_mut(|d| match outer {
        Some(g) => {
            d.insert_temp(slot, g);
        }
        None => d.remove::<(egui::Id, u64)>(slot),
    });
    out
}

/// The Compact layout's name column of the slider group being drawn: the longest name in it, as
/// measured over this pass and the one before (a group's rows below this one count once they
/// have been drawn), so a section's tracks start together just after its longest name.
fn name_column(ui: &Ui, name_w: f32) -> f32 {
    let pass = ui.ctx().cumulative_pass_nr();
    let group = match ui.data(|d| d.get_temp::<(egui::Id, u64)>(group_slot(ui.layer_id()))) {
        Some((g, p)) if p == pass => g,
        _ => ui.id().with("slider-names"),
    };
    let name_w = if name_w.is_finite() { name_w.max(0.0) } else { 0.0 };
    let (seen, cur, prev) = ui.data(|d| d.get_temp::<(u64, f32, f32)>(group)).unwrap_or((pass, 0.0, 0.0));
    // a new pass: the last one's widest becomes the remembered one
    let (cur, prev) = if seen == pass { (cur, prev) } else { (0.0, cur) };
    let cur = cur.max(name_w);
    ui.data_mut(|d| d.insert_temp(group, (pass, cur, prev)));
    cur.max(prev)
}

/// How far a slider moves per point of pointer travel in a frame that moved it `dx`, with
/// acceleration: 30 % for slow movement (up to 2 points a frame), rising to one for quick movement
/// (8 points or more).
fn drag_gain(dx: f32) -> f32 {
    const SLOW: f32 = 2.0;
    const FAST: f32 = 8.0;
    // the slowest speed: 30 % of the pointer's (issue #346)
    const MIN_GAIN: f32 = 0.3;
    let t = ((dx.abs() - SLOW) / (FAST - SLOW)).clamp(0.0, 1.0);
    MIN_GAIN + (1.0 - MIN_GAIN) * t
}

/// Convert vertical wheel movement to bounded slider steps, keeping sub-notch
/// trackpad movement between frames without amplifying a single large event. One mouse-wheel
/// notch ([`crate::theme::WHEEL_LINE_POINTS`]) is one step.
fn wheel_nudges(remainder: &mut f32, delta: f32) -> f64 {
    const POINTS_PER_STEP: f32 = crate::theme::WHEEL_LINE_POINTS;
    let total = (*remainder + delta).clamp(-10.0 * POINTS_PER_STEP, 10.0 * POINTS_PER_STEP);
    let steps = (total / POINTS_PER_STEP).trunc();
    *remainder = total - steps * POINTS_PER_STEP;
    f64::from(steps)
}

/// `value` moved by `steps` keyboard nudges: one nudge is about 1/200 of the range, a whole
/// number of the control's steps (exposure 0.05, most sliders 1, temperature 50 K).
pub fn nudged(spec: &ControlSpec, value: f64, steps: f64) -> f64 {
    let step = spec.step.max(1e-9);
    let unit = if spec.id == "wb.temp" { 50.0 } else { (((spec.max - spec.min) / 200.0 / step).round().max(1.0)) * step };
    (((value + unit * steps) / step).round() * step).clamp(spec.min, spec.max)
}

/// Collapsible section header ("› Light"). Returns the response (click toggles). In the Compact
/// layout it is a loud band ([`paint_section_band`]) that the sub-tool rows under it stay quieter
/// than.
pub fn section_header(ui: &mut Ui, id: &str, title: &str, open: bool, enabled: Option<bool>, badge: Option<&str>) -> (Response, Option<bool>) {
    if crate::theme::compact(ui.ctx()) {
        return dense_section_header(ui, id, title, open, enabled, badge);
    }
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click());
    // the section's sliders share one name column (Compact)
    begin_slider_group(ui, id);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    register(ui.ctx(), format!("section:{id}"), r);
    let p = ui.painter();
    if ui.rect_contains_pointer(r) {
        p.rect_filled(r, 0.0, t.chrome.gamma_multiply(1.06));
    }
    let chev = Rect::from_center_size(pos2(r.left() + 18.0, r.center().y), vec2(16.0, 16.0));
    paint(p, chev, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_label);
    let galley = p.layout_no_wrap(title.to_string(), t.semibold(14.0), t.text);
    p.galley(pos2(r.left() + 32.0, r.center().y - galley.size().y / 2.0), galley.clone(), t.text);
    if let Some(b) = badge {
        section_badge(ui, id, pos2(r.left() + 32.0 + galley.size().x + 8.0, r.center().y), b, &t);
    }
    let mut toggled = None;
    // shown while the pointer is anywhere on the header: `resp.hovered()` turns false as soon as the
    // pointer is over the eye itself (the eye is on top), which hid the eye before it could be
    // clicked (issue #316)
    if let Some(on) = enabled
        && (ui.rect_contains_pointer(r) || !on)
    {
        let eye = Rect::from_center_size(pos2(r.right() - 34.0, r.center().y), vec2(18.0, 18.0));
        toggled = section_eye(ui, id, eye, on);
    }
    (resp, toggled)
}

/// A section title's small pill (`badge`, e.g. "Witchcraft"): the grading mode the section
/// belongs to, beside the title. Registered as `sectionBadge:{id}` for the control channel.
fn section_badge(ui: &Ui, id: &str, at: Pos2, text: &str, t: &Tokens) {
    let p = ui.painter();
    let galley = p.layout_no_wrap(crate::i18n::tr(text).to_string(), t.font(9.5), t.accent);
    let (h, pad) = (15.0, 7.0);
    let r = Rect::from_min_size(pos2(at.x, at.y - h / 2.0), vec2(galley.size().x + 2.0 * pad, h));
    p.rect(r, h / 2.0, t.accent.gamma_multiply(0.13), Stroke::new(1.0, t.accent.gamma_multiply(0.55)), egui::StrokeKind::Inside);
    p.galley(pos2(r.left() + pad, r.center().y - galley.size().y / 2.0), galley, t.accent);
    register(ui.ctx(), format!("sectionBadge:{id}"), r);
}

/// A section's eye (show the photo without the section), drawn at `eye`; the new state when clicked.
fn section_eye(ui: &mut Ui, id: &str, eye: Rect, on: bool) -> Option<bool> {
    let t = Tokens::get(ui.ctx());
    let er = ui.interact(eye, ui.id().with(("eye", id)), Sense::click());
    crate::widgets::announce_selected(&er, crate::i18n::tr("Show / hide"), on);
    register(ui.ctx(), format!("sectionEye:{id}"), eye);
    paint(ui.painter(), eye, Icon::Eye, if on { t.icon } else { t.text_disabled });
    if !on {
        ui.painter().line_segment([eye.left_bottom(), eye.right_top()], Stroke::new(1.5, t.text_disabled));
    }
    er.clicked().then_some(!on)
}

/// Left and right margins of the Compact layout's panel rows: sliders, captions, buttons and
/// checkboxes line up on them (LAYOUT-DENSE, ported from OneSlips).
pub const PAD_L: f32 = 14.0;
pub const PAD_R: f32 = 12.0;

/// A panel row's frame margin: Classic's `classic` as it always was, or in the Compact layout the
/// rows' shared margins ([`PAD_L`], [`PAD_R`]), `top` and `bottom` apart.
pub fn row_margin(ui: &Ui, classic: egui::Margin, top: i8, bottom: i8) -> egui::Margin {
    if crate::theme::compact(ui.ctx()) { egui::Margin { left: PAD_L as i8, right: PAD_R as i8, top, bottom } } else { classic }
}

/// A caption over a row of buttons (Upright, Adjust): Classic's body label as it always was; in the Compact layout a
/// quiet one in the slider names' small type (`label:{id}`), so it reads below the section header and its buttons.
pub fn row_caption(ui: &mut Ui, id: &str, text: &str) -> Response {
    let r = if crate::theme::compact(ui.ctx()) {
        let t = Tokens::get(ui.ctx());
        ui.label(egui::RichText::new(crate::i18n::tr(text)).font(t.font(11.0)).color(t.text_dim))
    } else {
        ui.label(crate::i18n::tr(text))
    };
    register(ui.ctx(), format!("label:{id}"), r.rect);
    r
}

/// Title size of a Compact section header (Basic, Detail…); sub-tool rows use [`SUB_TITLE`].
pub(crate) const SECTION_TITLE: f32 = 13.0;
/// Title size of a Compact sub-tool row (Defringe, SDR Rendition…): smaller and regular weight, so
/// it reads as part of its section.
pub(crate) const SUB_TITLE: f32 = 12.0;

/// The fill of a Compact section header's band: the section-band token, a shade lighter (darker in
/// light themes) under the pointer.
pub(crate) fn section_fill(t: &Tokens, hovered: bool) -> Color32 {
    if hovered { t.section_band.lerp_to_gamma(t.text, 0.06) } else { t.section_band }
}

/// The fill of a Compact sub-tool row: none at rest, a light hover, so the rows stay quieter than
/// the section header above them.
pub(crate) fn flyout_fill(t: &Tokens, hovered: bool) -> Option<Color32> {
    hovered.then(|| t.chrome.lerp_to_gamma(t.hover, 0.6))
}

/// Paints a Compact section header's band over `r`: the band fill and a separator along its top.
pub(crate) fn paint_section_band(p: &egui::Painter, r: Rect, t: &Tokens, hovered: bool) {
    p.rect_filled(r, 0.0, section_fill(t, hovered));
    p.rect_filled(Rect::from_min_max(r.left_top(), pos2(r.right(), r.top() + 1.0)), 0.0, t.divider);
}

/// The Compact layout's section header: one banded bar ([`crate::theme::COMPACT_SECTION_H`]), the
/// chevron and name at the left on the rows' margin, the eye at the right.
fn dense_section_header(ui: &mut Ui, id: &str, title: &str, open: bool, enabled: Option<bool>, badge: Option<&str>) -> (Response, Option<bool>) {
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, crate::theme::COMPACT_SECTION_H), Sense::click());
    begin_slider_group(ui, id);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    register(ui.ctx(), format!("section:{id}"), r);
    let p = ui.painter();
    paint_section_band(p, r, &t, ui.rect_contains_pointer(r));
    if resp.has_focus() {
        p.rect_stroke(r.shrink(1.0), t.radius(crate::theme::DENSE_RADIUS), Stroke::new(1.5, t.accent), StrokeKind::Inside);
    }
    let chev = Rect::from_center_size(pos2(r.left() + PAD_L + 5.0, r.center().y), vec2(12.0, 12.0));
    paint(p, chev, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text);
    let galley = p.layout_no_wrap(title.to_string(), t.semibold(SECTION_TITLE), t.text);
    p.galley(pos2(r.left() + PAD_L + 16.0, r.center().y - galley.size().y / 2.0), galley.clone(), t.text);
    if let Some(b) = badge {
        section_badge(ui, id, pos2(r.left() + PAD_L + 16.0 + galley.size().x + 7.0, r.center().y), b, &t);
    }
    let mut toggled = None;
    if let Some(on) = enabled
        && (ui.rect_contains_pointer(r) || !on)
    {
        let eye = Rect::from_center_size(pos2(r.right() - PAD_R - 8.0, r.center().y), vec2(15.0, 15.0));
        toggled = section_eye(ui, id, eye, on);
    }
    (resp, toggled)
}

fn section_folds_id() -> egui::Id {
    egui::Id::new("persisted-section-folds")
}

/// Carry the side-panel folds between the UI state (saved in `ui.json`) and the frame: changes
/// made by [`folding_section`] last frame go to `saved`, then `saved` is what this frame draws.
pub fn sync_section_folds(ctx: &egui::Context, saved: &mut std::collections::BTreeMap<String, bool>) {
    let changed: Option<(bool, std::collections::BTreeMap<String, bool>)> = ctx.data(|d| d.get_temp(section_folds_id()));
    if let Some((true, folds)) = changed {
        *saved = folds;
    }
    ctx.data_mut(|d| d.insert_temp(section_folds_id(), (false, saved.clone())));
}

/// A collapsible side-panel section whose open state is remembered across launches under `key`
/// (see [`sync_section_folds`]); `default_open` until the user folds or unfolds it.
pub fn folding_section<R>(
    ui: &mut Ui,
    key: &str,
    title: impl Into<egui::WidgetText>,
    default_open: bool,
    body: impl FnOnce(&mut Ui) -> R,
) -> egui::CollapsingResponse<R> {
    let id = section_folds_id();
    let open = ui
        .ctx()
        .data(|d| d.get_temp::<(bool, std::collections::BTreeMap<String, bool>)>(id))
        .and_then(|(_, m)| m.get(key).copied())
        .unwrap_or(default_open);
    let r = egui::CollapsingHeader::new(title).id_salt(("folding-section", key)).open(Some(open)).show(ui, body);
    if r.header_response.clicked() {
        ui.ctx().data_mut(|d| {
            let (dirty, folds) = d.get_temp_mut_or_default::<(bool, std::collections::BTreeMap<String, bool>)>(id);
            *dirty = true;
            folds.insert(key.to_string(), !open);
        });
    }
    r
}

/// Where a panel row that isn't a slider puts its name so it lines up with the sliders' names:
/// (left padding, name column width, gap before the control) and the names' font, or `None` when
/// sliders put their name on a line of their own (Settings ▸ Interface: two rows), so rows lay
/// themselves out.
pub(crate) fn name_columns(ui: &Ui) -> Option<(f32, f32, f32, egui::FontId)> {
    if crate::theme::slider_layout(ui.ctx()) == crate::theme::SliderLayout::TwoRows {
        return None;
    }
    let font = slider_font(ui);
    let k = text_scale(&font);
    let w = ui.available_width();
    let col = label_column(ui, w, &font, k);
    Some(if crate::theme::compact(ui.ctx()) { (PAD_L, col, NAME_GAP, font) } else { (12.0, col, 8.0, font) })
}

/// Thin divider between groups.
pub fn divider(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.divider);
}

/// A flyout row inside a section (Curve, Color Mixer, Color Grading…): inset dark box with icon
/// tile. In the Compact layout a quiet row ([`dense_flyout_row`]).
pub fn flyout_row(ui: &mut Ui, id: &str, title: &str, icon: Icon, open: bool) -> Response {
    if crate::theme::compact(ui.ctx()) {
        return dense_flyout_row(ui, id, title, icon, open);
    }
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (outer, _) = ui.allocate_exact_size(vec2(w, 40.0), Sense::hover());
    let r = Rect::from_min_max(pos2(outer.left() + 16.0, outer.top() + 4.0), pos2(outer.right() - 16.0, outer.bottom() - 4.0));
    let resp = ui.interact(r, ui.id().with(("flyout", id)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    register(ui.ctx(), format!("flyout:{id}"), r);
    let p = ui.painter();
    p.rect_filled(r, 4.0, if resp.hovered() { t.hover.gamma_multiply(0.8) } else { t.inset });
    let tile = Rect::from_min_size(pos2(r.left() + 4.0, r.top() + 4.0), vec2(r.height() - 8.0, r.height() - 8.0));
    p.rect_filled(tile, 3.0, t.chrome.gamma_multiply(1.15));
    paint(p, tile.shrink(3.0), icon, t.text_label);
    p.text(pos2(tile.right() + 10.0, r.center().y), Align2::LEFT_CENTER, title, t.semibold(13.0), t.text_label);
    paint(
        p,
        Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(16.0, 16.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronLeft },
        t.text_label,
    );
    resp
}

/// Height of a Compact row that holds a control (a sub-tool row, a caption, a button or segment
/// row), so the rows of a section line up at one height.
pub const DENSE_ROW_H: f32 = 22.0;

/// The Compact layout's sub-tool row (Defringe, SDR Rendition…): an indented icon and a
/// regular-weight title on the rows' margin, no box of its own, so it reads as part of the section
/// above it (LAYOUT-DENSE, ported from OneSlips).
fn dense_flyout_row(ui: &mut Ui, id: &str, title: &str, icon: Icon, open: bool) -> Response {
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (outer, _) = ui.allocate_exact_size(vec2(w, DENSE_ROW_H), Sense::hover());
    let r = Rect::from_min_max(pos2(outer.left() + PAD_L - 4.0, outer.top() + 1.0), pos2(outer.right() - PAD_R + 4.0, outer.bottom() - 1.0));
    let resp = ui.interact(r, ui.id().with(("flyout", id)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    register(ui.ctx(), format!("flyout:{id}"), r);
    let p = ui.painter();
    if let Some(fill) = flyout_fill(&t, resp.hovered()) {
        p.rect_filled(r, t.radius(crate::theme::DENSE_RADIUS), fill);
    }
    // indented under the section's title: a nested part of it, not a section of its own
    let tile = Rect::from_center_size(pos2(r.left() + 4.0 + 16.0 - 5.0, r.center().y), vec2(13.0, 13.0));
    paint(p, tile, icon, t.text_dim);
    p.text(pos2(tile.right() + 6.0, r.center().y), Align2::LEFT_CENTER, title, t.font(SUB_TITLE), t.text_label);
    paint(
        p,
        Rect::from_center_size(pos2(r.right() - 4.0 - 6.0, r.center().y), vec2(12.0, 12.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronLeft },
        t.text_dim,
    );
    resp
}

/// A small bordered text button (Auto, B&W, HDR…).
pub fn text_button(ui: &mut Ui, id: &str, label: &str, active: bool) -> Response {
    text_button_sized(ui, id, label, active, None)
}

/// [`text_button`] exactly `width` wide (at least 37 points), its label cut at the edges when it doesn't fit: for
/// buttons that share a bar's width between them, as Lightroom Classic's do (issue #221).
pub fn text_button_wide(ui: &mut Ui, id: &str, label: &str, active: bool, width: f32) -> Response {
    text_button_sized(ui, id, label, active, Some(width))
}

fn text_button_sized(ui: &mut Ui, id: &str, label: &str, active: bool, width: Option<f32>) -> Response {
    let label = if id.starts_with("labelSet-") { label } else { crate::i18n::tr(label) };
    let t = Tokens::get(ui.ctx());
    // finger-sized on the touch Settings screen
    let finger = crate::dialog_frame::finger_sized(ui);
    // (Compact: the panels' 20 pt buttons, as their segment rows)
    let dense = t.dense && !finger;
    let font = t.semibold(if finger {
        14.0
    } else if dense {
        11.0
    } else {
        11.5
    });
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, t.text);
    let w = width.filter(|w| w.is_finite()).unwrap_or(galley.size().x + if finger { 28.0 } else { 18.0 });
    let size = vec2(
        w.max(if finger {
            56.0
        } else if dense {
            34.0
        } else {
            37.0
        }),
        if finger {
            40.0
        } else if dense {
            DENSE_ROW_H - 2.0
        } else {
            24.0
        },
    );
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, label));
    register(ui.ctx(), format!("button:{id}"), r);
    let fill = if active {
        t.pressed
    } else if resp.hovered() {
        t.hover
    } else {
        t.button
    };
    let p = ui.painter();
    // #122: an "on" choice gets a brighter edge as well as a brighter fill, so it reads as selected.
    let edge = if active { t.text_dim } else { t.button_border };
    p.rect(r, t.radius(if finger { 10.0 } else { 6.0 }), fill, Stroke::new(1.0, edge), StrokeKind::Inside);
    p.with_clip_rect(r.intersect(p.clip_rect())).galley(r.center() - galley.size() / 2.0, galley, t.text);
    resp
}

/// The font, option height, track padding and each option's width of [`segments`] for `labels`.
fn segment_metrics(ui: &Ui, labels: &[&str]) -> (egui::FontId, f32, f32, Vec<f32>) {
    let t = Tokens::get(ui.ctx());
    let finger = crate::dialog_frame::finger_sized(ui);
    let font = t.semibold(if finger { 13.5 } else { 11.5 });
    // (Compact: a tighter track)
    let (h, pad) = if finger {
        (34.0, 3.0)
    } else if t.dense {
        (20.0, 1.0)
    } else {
        (24.0, 2.0)
    };
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| ui.painter().layout_no_wrap(crate::i18n::tr(l).to_string(), font.clone(), t.text).size().x + if finger { 26.0 } else { 16.0 })
        .collect();
    (font, h, pad, widths)
}

/// How wide [`segments`] draws `labels` on one row.
pub fn segments_width(ui: &Ui, labels: &[&str]) -> f32 {
    let (_, _, pad, widths) = segment_metrics(ui, labels);
    widths.iter().sum::<f32>() + 2.0 * pad
}

/// A segmented choice: the options side by side in one rounded track, the chosen one raised on a
/// soft highlight. Each option is a button registered `button:{id}-{i}`; returns the one clicked.
/// Wraps onto further rows when the options don't fit. Own design.
pub fn segments(ui: &mut Ui, id: &str, labels: &[&str], chosen: Option<usize>) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let (font, h, pad, widths) = segment_metrics(ui, labels);
    let total = widths.iter().sum::<f32>() + 2.0 * pad;
    let mut clicked = None;
    let one_row = total <= ui.available_width();
    let draw = |ui: &mut Ui, range: std::ops::Range<usize>, clicked: &mut Option<usize>| {
        let w = range.clone().map(|i| widths.get(i).copied().unwrap_or(0.0)).sum::<f32>() + 2.0 * pad;
        let (track, _) = ui.allocate_exact_size(vec2(w, h + 2.0 * pad), Sense::hover());
        ui.painter().rect_filled(track, t.radius((h + 2.0 * pad) / 2.0), t.button);
        let mut x = track.left() + pad;
        for i in range {
            let cw = widths.get(i).copied().unwrap_or(0.0);
            let cell = Rect::from_min_size(pos2(x, track.top() + pad), vec2(cw, h));
            x += cw;
            let label = crate::i18n::tr(labels.get(i).copied().unwrap_or(""));
            let resp = ui.interact(cell, ui.id().with((id, i)), Sense::click());
            let on = chosen == Some(i);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
            register(ui.ctx(), format!("button:{id}-{i}"), cell);
            if on {
                ui.painter().rect(cell, t.radius(h / 2.0), t.pressed, Stroke::new(1.0, t.text_dim.gamma_multiply(0.5)), StrokeKind::Inside);
            } else if resp.hovered() {
                ui.painter().rect_filled(cell, t.radius(h / 2.0), t.hover);
            }
            ui.painter().text(cell.center(), Align2::CENTER_CENTER, label, font.clone(), if on { t.text } else { t.text_label });
            if resp.clicked() {
                *clicked = Some(i);
            }
        }
    };
    if one_row {
        draw(ui, 0..labels.len(), &mut clicked);
    } else {
        // too wide for one track: one per option, wrapping
        ui.horizontal_wrapped(|ui| {
            for i in 0..labels.len() {
                draw(ui, i..i + 1, &mut clicked);
            }
        });
    }
    clicked
}

/// The width each of `n` buttons gets when they share `room` points, `gap` apart.
pub fn shared_width(room: f32, n: usize, gap: f32) -> f32 {
    let n = n.max(1) as f32;
    ((room - gap * (n - 1.0)) / n).floor().max(0.0)
}

/// A segmented control: `labels` share the available width equally, `per_row` per row. Each segment is
/// addressable as `button:{id}-{key}` (keys parallel to labels). Returns the clicked index.
pub fn segmented(ui: &mut Ui, id: &str, items: &[(&str, &str)], active: Option<usize>, per_row: usize) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let finger = crate::dialog_frame::finger_sized(ui);
    if crate::theme::compact(ui.ctx()) && !finger {
        return dense_segmented(ui, id, items, active, per_row);
    }
    let per_row = per_row.max(1);
    let (seg_h, font) = if finger { (44.0, t.semibold(14.0)) } else { (24.0, t.semibold(11.5)) };
    let gap = 4.0;
    let w = ui.available_width();
    let seg_w = ((w - gap * (per_row as f32 - 1.0)) / per_row as f32).max(24.0);
    let mut clicked = None;
    for (row_i, row) in items.chunks(per_row).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (j, (label, key)) in row.iter().enumerate() {
                let label = crate::i18n::tr(label);
                let i = row_i * per_row + j;
                let (r, resp) = ui.allocate_exact_size(vec2(seg_w, seg_h), Sense::click());
                crate::widgets::announce_selected(&resp, label.to_string(), active == Some(i));
                register(ui.ctx(), format!("button:{id}-{key}"), r);
                let fill = if active == Some(i) {
                    t.pressed
                } else if resp.hovered() {
                    t.hover
                } else {
                    t.button
                };
                let p = ui.painter();
                p.rect(r, t.radius(4.0), fill, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, label, font.clone(), t.text);
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
    }
    clicked
}

/// The Compact layout's [`segmented`]: the options as equal cells of one flat track
/// ([`DENSE_ROW_H`] tall, the chosen cell raised), all on one row when every label fits its cell,
/// else `per_row` to a row. Same widget ids (`button:{id}-{key}`).
fn dense_segmented(ui: &mut Ui, id: &str, items: &[(&str, &str)], active: Option<usize>, per_row: usize) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let font = t.font(11.0);
    let w = ui.available_width().max(1.0);
    let widest =
        items.iter().map(|(l, _)| ui.painter().layout_no_wrap(crate::i18n::tr(l).to_string(), font.clone(), t.text).size().x).fold(0.0_f32, f32::max);
    let n = items.len().max(1);
    let per_row = if widest + 8.0 <= w / n as f32 { n } else { per_row.clamp(1, n) };
    let h = DENSE_ROW_H - 2.0;
    let mut clicked = None;
    for (row_i, row) in items.chunks(per_row).enumerate() {
        let (track, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let r = t.radius(crate::theme::DENSE_RADIUS);
        ui.painter().rect(track, r, t.button, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
        let cell_w = track.width() / per_row as f32;
        for (j, (label, key)) in row.iter().enumerate() {
            let label = crate::i18n::tr(label);
            let i = row_i * per_row + j;
            let cell = Rect::from_min_size(pos2(track.left() + cell_w * j as f32, track.top()), vec2(cell_w, h));
            let resp = ui.interact(cell, ui.id().with((id, i)), Sense::click());
            crate::widgets::announce_selected(&resp, label.to_string(), active == Some(i));
            register(ui.ctx(), format!("button:{id}-{key}"), cell);
            let on = active == Some(i);
            if on {
                ui.painter().rect(cell.shrink(1.0), r, t.pressed, Stroke::new(1.0, t.text_dim.gamma_multiply(0.5)), StrokeKind::Inside);
            } else if resp.hovered() {
                ui.painter().rect_filled(cell.shrink(1.0), r, t.hover);
            }
            if j > 0 && !on && active != Some(i - 1) {
                ui.painter().vline(cell.left(), cell.y_range().shrink(4.0), Stroke::new(1.0, t.button_border));
            }
            let g = one_line_clipped(ui, label, font.clone(), if on { t.text } else { t.text_label }, cell.width() - 4.0);
            ui.painter().galley(cell.center() - g.size() / 2.0, g, t.text);
            if resp.clicked() {
                clicked = Some(i);
            }
        }
        ui.add_space(2.0);
    }
    clicked
}

/// `text` on one line at most `max_w` wide, cut short with … when it doesn't fit.
fn one_line_clipped(ui: &Ui, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: max_w.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

/// [`segmented`] with icons: one row of equal icon segments, each with its label as hover text and
/// accessible name, addressable as `button:{id}-{key}`. Returns the clicked index.
pub fn icon_segmented(ui: &mut Ui, id: &str, items: &[(Icon, &str, &str)], active: Option<usize>) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let gap = 4.0;
    let n = items.len().max(1) as f32;
    let seg_w = ((ui.available_width() - gap * (n - 1.0)) / n).clamp(24.0, 40.0);
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, (icon, label, key)) in items.iter().enumerate() {
            let label = crate::i18n::tr(label);
            let on = active == Some(i);
            let (r, resp) = ui.allocate_exact_size(vec2(seg_w, 24.0), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
            register(ui.ctx(), format!("button:{id}-{key}"), r);
            let fill = if on {
                t.pressed
            } else if resp.hovered() {
                t.hover
            } else {
                t.button
            };
            let p = ui.painter();
            p.rect(r, t.radius(4.0), fill, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
            let c = if on {
                t.pick
            } else if resp.hovered() {
                t.text
            } else {
                t.icon
            };
            paint(p, Rect::from_center_size(r.center(), vec2(18.0, 18.0)), *icon, c);
            if resp.on_hover_text(label).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// An icon-only button. `active` draws the selected background (tool strip).
pub fn icon_button(ui: &mut Ui, id: &str, icon: Icon, size: egui::Vec2, active: bool, enabled: bool, tooltip: &str) -> Response {
    let tooltip = crate::i18n::tr(tooltip);
    icon_button_named(ui, id, icon, size, active, enabled, tooltip, tooltip)
}

/// The tooltip a panel's icon button shows: its (translated) label, then the shortcut of
/// `command` as menus write it (`Rotate Left (⌘[)`), when it has one.
pub fn command_tooltip(ui: &Ui, label: &str, shortcut: Option<&str>) -> String {
    let label = crate::i18n::tr(label);
    match shortcut {
        Some(sc) => format!("{label} ({})", crate::menubar::shortcut_text(sc, ui.ctx().os() == egui::os::OperatingSystem::Mac)),
        None => label.to_string(),
    }
}

/// [`icon_button`] with its accessible `name` (already translated) apart from its hover text
/// (`tooltip`, e.g. the name plus a shortcut).
#[allow(clippy::too_many_arguments)]
pub fn icon_button_named(ui: &mut Ui, id: &str, icon: Icon, size: egui::Vec2, active: bool, enabled: bool, name: &str, tooltip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, active, name));
    register(ui.ctx(), format!("icon:{id}"), r);
    let p = ui.painter();
    let sq = Rect::from_center_size(r.center(), vec2(32.0f32.min(size.x), 32.0f32.min(size.y)));
    if active {
        p.rect_filled(sq, 4.0, t.tool_active);
    } else if resp.is_pointer_button_down_on() && enabled {
        p.rect_filled(sq, 4.0, t.pressed);
    } else if resp.hovered() && enabled {
        p.rect_filled(sq, 4.0, t.hover);
    }
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = if !enabled {
        t.text_disabled
    } else if active {
        t.pick
    } else if resp.hovered() {
        t.text
    } else {
        t.icon
    };
    let glyph = (size.x.min(size.y) * 0.62).min(22.0);
    paint(p, Rect::from_center_size(r.center(), vec2(glyph, glyph)), icon, c);
    if !tooltip.is_empty() { resp.on_hover_text(tooltip) } else { resp }
}

/// A borderless dropdown button: `text` followed by a painted chevron (no font glyph needed).
pub fn dropdown(ui: &mut Ui, id: &str, text: &str, font: egui::FontId, color: Color32) -> Response {
    dropdown_within(ui, id, text, font, color, f32::INFINITY)
}

/// [`dropdown`] at most `max` wide, chevron included: a longer `text` is cut short with …, and
/// shown in full on hover.
pub fn dropdown_within(ui: &mut Ui, id: &str, text: &str, font: egui::FontId, color: Color32, max: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let galley = one_line(ui.painter(), text, font, color, max - 20.0);
    let elided = galley.elided;
    let size = vec2(galley.size().x + 20.0, galley.size().y.max(20.0));
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, text));
    register(ui.ctx(), format!("dropdown:{id}"), r);
    let c = if resp.hovered() { t.text } else { color };
    ui.painter().galley(pos2(r.left(), r.center().y - galley.size().y / 2.0), galley, c);
    paint(ui.painter(), Rect::from_center_size(pos2(r.right() - 10.0, r.center().y), vec2(16.0, 16.0)), Icon::ChevronDown, c);
    if elided { resp.on_hover_text(text) } else { resp }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_slider_drags_move_less_than_the_pointer_and_quick_ones_keep_up() {
        assert!(drag_gain(0.5) < 1.0 && drag_gain(0.5) > 0.0);
        assert!(drag_gain(-1.0) <= drag_gain(5.0));
        assert!((drag_gain(40.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn nudges_are_a_sensible_step() {
        let spec = |id, min, max, step| ControlSpec {
            id,
            label: "",
            section: lcc_develop::controls::Section::Light,
            min,
            max,
            default: 0.0,
            step,
            decimals: 2,
            track: Track::Plain,
            shade: lcc_develop::Shade::Plain,
        };
        assert!((nudged(&spec("light.exposure", -5.0, 5.0, 0.01), 0.0, 1.0) - 0.05).abs() < 1e-9);
        assert_eq!(nudged(&spec("light.contrast", -100.0, 100.0, 1.0), 10.0, -5.0), 5.0);
        assert_eq!(nudged(&spec("wb.temp", 2000.0, 50000.0, 1.0), 6500.0, 1.0), 6550.0);
        assert_eq!(nudged(&spec("light.contrast", -100.0, 100.0, 1.0), 99.0, 5.0), 100.0, "clamped");
    }

    #[test]
    fn wheel_steps_accumulate_small_movements_and_preserve_direction() {
        // one mouse-wheel notch is one step, whatever a notch scrolls
        let notch = crate::theme::WHEEL_LINE_POINTS;
        let mut remainder = 0.0;
        assert_eq!(wheel_nudges(&mut remainder, notch * 0.375), 0.0);
        assert_eq!(wheel_nudges(&mut remainder, notch * 0.625), 1.0);
        assert_eq!(remainder, 0.0);
        assert_eq!(wheel_nudges(&mut remainder, -notch * 2.5), -2.0);
        assert_eq!(wheel_nudges(&mut remainder, -notch * 0.5), -1.0);
        assert_eq!(remainder, 0.0);
        assert_eq!(wheel_nudges(&mut remainder, notch), 1.0, "a single notch is a single nudge");
        assert_eq!(remainder, 0.0);
        assert_eq!(wheel_nudges(&mut remainder, notch * 50.0), 10.0, "a single huge event is capped");
    }

    /// Shift-drag moves a tenth of the pointer's way along the track, also on the Kelvin track (where a pixel of
    /// fine drag near the centre is about 4 K): a pointer creeping a pixel a frame still adds up to steps.
    #[test]
    fn a_slow_shift_drag_adds_up() {
        let ctx = egui::Context::default();
        let temp = lcc_develop::controls::find("wb.temp").unwrap();
        let mut value = 7000.0;
        let frame = |time: f64, events: Vec<egui::Event>, value: f64| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 120.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut out = SliderOut::default();
            ctx.run_ui(input, |ui| out = slider(ui, temp, value, true, None)).textures_delta.clear();
            let track = take_registry(&ctx).into_iter().find(|(id, _)| id == "slider:wb.temp").unwrap().1;
            (out, track)
        };
        // Shift is held from the start (the modifiers stay until they change)
        let (_, track) = frame(0.0, vec![egui::Event::ModifiersChanged(egui::Modifiers::SHIFT)], value);
        let mut at = pos2(track.left() + temp.position(value) as f32 * track.width(), track.center().y);
        let press =
            |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::SHIFT };
        frame(0.1, vec![egui::Event::PointerMoved(at), press(at, true)], value);
        let moves = 60;
        for i in 0..moves {
            at.x += 1.0;
            let (out, _) = frame(0.2 + f64::from(i) * 0.02, vec![egui::Event::PointerMoved(at)], value);
            value = out.value.unwrap_or(value);
        }
        frame(2.0, vec![press(at, false)], value);
        // the drag starts once the pointer has moved a few pixels; every pixel after that counts
        let lo = temp.value_at(temp.position(7000.0) + f64::from(moves - 10) / f64::from(track.width()) * 0.1);
        let hi = temp.value_at(temp.position(7000.0) + f64::from(moves) / f64::from(track.width()) * 0.1);
        assert!(value >= lo - 50.0 && value <= hi + 50.0 && value > 7000.0, "{value} K, expected {lo:.0}–{hi:.0} K");
    }

    /// Typed values (issue #322): signs, a decimal comma, units; on the control's steps, in range.
    #[test]
    fn typed_values_are_stepped_and_clamped() {
        let exposure = lcc_develop::controls::find("light.exposure").unwrap();
        let contrast = lcc_develop::controls::find("light.contrast").unwrap();
        let temp = lcc_develop::controls::find("wb.temp").unwrap();
        assert_eq!(typed_value(exposure, "1.5"), Some(1.5));
        assert_eq!(typed_value(exposure, " +0,25 "), Some(0.25));
        assert_eq!(typed_value(exposure, "-12"), Some(exposure.min), "clamped");
        assert_eq!(typed_value(contrast, "33.4"), Some(33.0), "on the control's steps");
        assert_eq!(typed_value(contrast, "-20 %"), Some(-20.0));
        assert_eq!(typed_value(temp, "5600 K"), Some(5600.0));
        for bad in ["", "abc", "1.2.3", "NaN", "inf", "--1"] {
            assert_eq!(typed_value(contrast, bad), None, "{bad:?}");
        }
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;

    /// Screen readers see the custom widgets: sliders by control name with their value, buttons
    /// by label or tooltip.
    #[test]
    fn custom_widgets_describe_themselves() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        let spec = lcc_develop::controls::find("light.exposure").unwrap();
        let mut found = Vec::new();
        let mut ranges = Vec::new();
        for _ in 0..3 {
            let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = slider(ui, spec, 0.5, true, None);
                let _ = text_button(ui, "auto", "Auto", false);
                let _ = icon_button(ui, "trash", Icon::Trash, vec2(24.0, 24.0), false, true, "Delete mask");
                let _ = icon_button_named(ui, "rotl", Icon::RotateLeft, vec2(28.0, 24.0), false, true, "Rotate Left", "Rotate Left (Ctrl+[)");
                let _ = icon_segmented(ui, "ov", &[(Icon::OverlayThirds, "Thirds", "thirds"), (Icon::OverlayNone, "None", "none")], Some(0));
            });
            let mut out = out;
            out.textures_delta.clear();
            if let Some(update) = out.platform_output.accesskit_update.take() {
                ranges = update
                    .nodes
                    .iter()
                    .filter(|(_, n)| n.role() == egui::accesskit::Role::Slider)
                    .map(|(_, n)| (n.min_numeric_value(), n.max_numeric_value()))
                    .collect();
                found = update
                    .nodes
                    .iter()
                    .map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string(), n.numeric_value()))
                    .collect();
            }
        }
        let has = |role: &str, label: &str| found.iter().any(|(r, l, _)| r == role && l == label);
        assert!(has("Slider", "Exposure"), "{found:?}");
        assert!(found.iter().any(|(r, l, v)| r == "Slider" && l == "Exposure" && *v == Some(0.5)), "the slider's value: {found:?}");
        assert!(ranges.contains(&(Some(spec.min), Some(spec.max))), "the slider's range: {ranges:?}");
        assert!(has("Button", "Auto"), "{found:?}");
        assert!(has("Button", "Delete mask"), "{found:?}");
        assert!(has("Button", "Rotate Left"), "an icon button is named by its label, not its hover text: {found:?}");
        assert!(has("Button", "Thirds") && has("Button", "None"), "icon segments are named: {found:?}");
    }
}

/// Names a widget for screen readers (`name` replaces what egui gave it): for a plain egui field
/// whose caption is a separate label beside it.
pub(crate) fn screen_reader_name(ctx: &egui::Context, id: egui::Id, name: &str) {
    ctx.accesskit_node_builder(id, |node| node.set_label(name.to_string()));
}

/// A hand-drawn clickable area (an icon, a tile, a row) announced as a button named `name`.
pub(crate) fn announce(resp: &egui::Response, name: impl Into<String>) {
    let name = name.into();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, resp.enabled(), name.clone()));
}

/// The same for one that is on or off (a toggle, a selected row or swatch).
pub(crate) fn announce_selected(resp: &egui::Response, name: impl Into<String>, selected: bool) {
    let name = name.into();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, resp.enabled(), selected, name.clone()));
}

/// `r`, named `name` for screen readers: a plain egui field or slider whose caption is drawn
/// beside it, or whose hint is all it shows.
pub(crate) fn named(r: egui::Response, name: &str) -> egui::Response {
    screen_reader_name(&r.ctx, r.id, name);
    r
}

/// `text` laid out on one line, cut short with … when it is wider than `max` (the galley's
/// `elided` says whether it was): for text painted into a fixed box (a tile, a row before its
/// buttons), which a plain `Painter::text` would draw past.
pub(crate) fn one_line(painter: &egui::Painter, text: &str, font: egui::FontId, color: Color32, max: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max.max(1.0));
    painter.layout_job(job)
}

/// `text` shortened to fit `max` (as `width` measures it): leading folders drop first
/// (`Users/me/Pictures/Lightroom` → `…/Pictures/Lightroom`) so the end of a path, which says the
/// most, stays; a single name still too long loses its end (`2024-06-12 Tri…`: folders tend to
/// differ at the start). Cuts fall on characters. The result is never empty.
pub(crate) fn elide_head(text: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    let mut rest = text;
    while let Some(i) = rest.find('/') {
        rest = rest.get(i + 1..).unwrap_or("");
        let cand = format!("…/{rest}");
        if width(&cand) <= max {
            return cand;
        }
    }
    let n = rest.chars().count();
    for keep in (1..n).rev() {
        let cand: String = rest.chars().take(keep).chain(std::iter::once('…')).collect();
        if width(&cand) <= max {
            return cand;
        }
    }
    "…".to_string()
}

#[cfg(test)]
#[path = "widgets/tests_numeric_entry.rs"]
mod numeric_entry_tests;

/// Whether Up / Down go to a slider this frame (the pointer over a slider's row, or a slider holding
/// the keyboard focus), as [`slider`] reads them: the photo-stepping shortcuts then leave them alone.
pub fn arrows_go_to_a_slider(ctx: &egui::Context, widgets: &[(String, egui::Rect)]) -> bool {
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    // the registered rect is the track; its row reaches up over the label and out to the paddings
    let over = pointer.is_some_and(|p| {
        widgets.iter().any(|(id, r)| {
            id.starts_with("slider:") && Rect::from_min_max(pos2(r.left() - 24.0, r.top() - 22.0), pos2(r.right() + 22.0, r.bottom())).contains(p)
        })
    });
    let focused = ctx.data(|d| d.get_temp::<egui::Id>(focused_slider_key())).is_some_and(|id| ctx.memory(|m| m.has_focus(id)));
    over || focused
}
#[cfg(test)]
#[path = "widgets/tests_slider_reset.rs"]
mod slider_reset_tests;

fn focused_slider_key() -> egui::Id {
    egui::Id::new("app-focused-slider")
}

/// The slider a click or drag armed for vertical wheel edits, if any.
fn wheel_slider_key() -> egui::Id {
    egui::Id::new("app-wheel-slider")
}

/// Whether the widget with keyboard focus is a develop slider selected by a click, drag or Tab.
/// It owns only the arrow keys: every other shortcut still works. Wheel selection is separate.
pub fn slider_has_focus(ctx: &egui::Context) -> bool {
    let focused = ctx.memory(|m| m.focused());
    focused.is_some() && focused == ctx.data(|d| d.get_temp::<egui::Id>(focused_slider_key()))
}
