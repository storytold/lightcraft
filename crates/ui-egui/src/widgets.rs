//! Lightroom-style widgets: sliders with gradient tracks and hollow thumbs, section headers,
//! flyout rows, bordered buttons. Every interactive widget registers an automation id + rect
//! ([`register`]) so the control channel can find and click it by name.

use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use lightcraft_develop::{ControlSpec, Track};

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
    if reason.starts_with("Nikon High Efficiency NEF (") {
        return crate::i18n::tr(
            "Nikon High Efficiency NEF (HE / HE★) uses intoPIX TicoRAW compression. LightCraft does not currently have a license for this codec, so only the camera's embedded JPEG preview is available, not the raw sensor data. To edit raw data, shoot Lossless compressed NEF or convert existing HE files to DNG using a licensed converter.",
        )
        .to_string();
    }
    crate::i18n::tr_format!(
        "LightCraft can't decode this raw variant yet ({}). You're editing the camera's embedded JPEG preview, which already includes the camera's picture style (e.g. Monochrome) and white balance.",
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

fn track_stops(track: &Track) -> Option<Vec<Color32>> {
    Some(match track {
        Track::Plain | Track::Centered => return None,
        Track::Temp => ["#193961", "#47566a", "#60666d", "#70705b", "#737339", "#75742a"].iter().map(|h| hex(h)).collect(),
        Track::Tint => ["#246123", "#2c552c", "#394739", "#4a3d4c", "#6b336b", "#872c87"].iter().map(|h| hex(h)).collect(),
        Track::Gradient { from, to } => vec![hex(from), hex(to)],
        Track::Hue { band } => {
            let i = *band as usize;
            let prev = hex(BAND_COLORS[(i + 7) % 8]);
            let next = hex(BAND_COLORS[(i + 1) % 8]);
            let me = hex(BAND_COLORS[i]);
            vec![
                prev.gamma_multiply(0.7),
                lerp(prev, me, 0.5).gamma_multiply(0.7),
                me.gamma_multiply(0.7),
                lerp(me, next, 0.5).gamma_multiply(0.7),
                next.gamma_multiply(0.7),
            ]
        }
        Track::Sat { band } => vec![hex("#454343"), lerp(hex("#454343"), hex(BAND_COLORS[*band as usize]), 0.6), hex(BAND_COLORS[*band as usize])],
        Track::Lum { band } => {
            let c = hex(BAND_COLORS[*band as usize]);
            vec![c.gamma_multiply(0.2), c.gamma_multiply(0.55), lerp(c, Color32::WHITE, 0.35), lerp(c, Color32::WHITE, 0.6)]
        }
        Track::Rainbow => ["#aa0000", "#aaaa00", "#009900", "#00aaaa", "#0044cc", "#aa0077", "#aa0000"].iter().map(|h| hex(h)).collect(),
    })
}

fn paint_track(ui: &Ui, rect: Rect, track: &Track, t: &Tokens, thumb_x: f32, ring: f32) {
    let p = ui.painter();
    let y = rect.center().y;
    let h = 1.0;
    if let Some(stops) = track_stops(track) {
        // gradient: segments between stops
        let n = stops.len().max(2) - 1;
        let steps = 48;
        for i in 0..steps {
            let (a, b) = (i as f32 / steps as f32, (i + 1) as f32 / steps as f32);
            let seg = (a * n as f32).floor() as usize;
            let local = a * n as f32 - seg as f32;
            let c = lerp(stops[seg.min(n)], stops[(seg + 1).min(n)], local);
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

/// Result of a slider interaction this frame.
#[derive(Default, Debug, Clone, Copy)]
pub struct SliderOut {
    pub value: Option<f64>,
    pub drag_started: bool,
    pub drag_stopped: bool,
    pub reset: bool,
}

/// What a value typed into a slider asks for (issue #322): a number such as `0.5`, `+12`, `-1,5`
/// (comma or point) or `5600 K`, on the control's steps and clamped to its range. `None` when it
/// isn't a finite number, so the slider keeps its value.
pub fn typed_value(spec: &ControlSpec, text: &str) -> Option<f64> {
    let t = text.trim().replace(',', ".");
    let t = t.trim_end_matches(|c: char| c == '%' || c == 'K' || c == 'k' || c.is_whitespace());
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

/// The radius of a slider's hollow ring thumb.
const THUMB_RING: f32 = 7.0;

/// A Lightroom slider row (label + value above a track with a hollow ring thumb).
/// Double-click the label or thumb to reset (hovering either says so). Shift-drag = fine adjustment.
/// Scroll sideways over it to nudge it. Click the value to type one: Return or clicking away applies it,
/// Esc keeps the old one. Drag the value sideways to scrub it, as if dragging the thumb (Shift: a tenth
/// as far).
pub fn slider(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, label_override: Option<&str>) -> SliderOut {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(w, t.slider_row_h), Sense::hover());
    let pad_l = 24.0;
    let pad_r = 22.0;
    let label_rect = Rect::from_min_size(pos2(row.left() + pad_l, row.top() + 4.0), vec2(w - pad_l - pad_r, 18.0));
    let track_rect = Rect::from_min_max(pos2(row.left() + pad_l, row.top() + 22.0), pos2(row.right() - pad_r, row.top() + 40.0));
    let id = ui.id().with(spec.id);
    let resp = ui.interact(track_rect.expand2(vec2(8.0, 2.0)), id, if enabled { Sense::click_and_drag() } else { Sense::hover() });
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
    let reset_hint = crate::i18n::tr_format!("{name} — double-click to reset it", name = label_text);
    let label_resp = if enabled { label_resp.on_hover_text(reset_hint.as_str()) } else { label_resp };
    crate::widgets::announce(&label_resp, crate::i18n::tr_format!("Reset {name}", name = label_text.clone()));
    register(ui.ctx(), format!("sliderLabel:{}", spec.id), label_rect);
    register(ui.ctx(), format!("slider:{}", spec.id), track_rect);
    // the value: click it to type an exact one, drag it sideways to scrub (over the label, so it
    // takes the click)
    let value_rect = Rect::from_min_max(pos2(label_rect.right() - 64.0, label_rect.top()), label_rect.max);
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
            pos = (pos + f64::from(dx / track_rect.width()) * fine).clamp(0.0, 1.0);
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
            .font(t.font(12.5))
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
        if resp.drag_started() {
            out.drag_started = true;
        }
        if (resp.dragged() || resp.drag_started())
            && let Some(p) = resp.interact_pointer_pos()
        {
            let fine = ui.input(|i| i.modifiers.shift);
            let step = spec.step.max(1e-9);
            let nv = if fine {
                // a tenth of the pointer's movement along the track; the unrounded position is kept, so a
                // slow drag adds up instead of rounding back to the same step every frame
                let fine_id = id.with("fine-pos");
                let dx = ui.input(|i| i.pointer.delta().x);
                let kept = ui.data(|m| m.get_temp::<f64>(fine_id)).filter(|&p| ((spec.value_at(p) / step).round() * step - value).abs() < step / 2.0);
                let pos = (kept.unwrap_or_else(|| spec.position(value)) + f64::from(dx / track_rect.width()) * 0.1).clamp(0.0, 1.0);
                ui.data_mut(|m| m.insert_temp(fine_id, pos));
                spec.value_at(pos)
            } else {
                from_x(p.x)
            };
            let nv = ((nv / step).round() * step).clamp(spec.min, spec.max);
            if (nv - value).abs() > 1e-12 {
                out.value = Some(nv);
                v = nv;
            }
        } else if resp.clicked()
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
        if resp.drag_stopped() {
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
    p.text(label_rect.left_center(), Align2::LEFT_CENTER, crate::i18n::tr(label_override.unwrap_or(spec.label)), t.font(12.5), text_c);
    if typing.is_none() {
        p.text(label_rect.right_center(), Align2::RIGHT_CENTER, shown_value(spec, v), t.font(12.5), text_c);
        // the scrub pointer, as over Lightroom's values: a drag here changes the value
        if enabled && (value_resp.hovered() || scrub.is_some()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }
    let ring = THUMB_RING;
    let tx = to_x(v);
    paint_track(ui, track_rect, &spec.track, &t, tx, ring);
    let p = ui.painter();
    let ring_c = if !enabled {
        t.text_disabled
    } else if hovered {
        t.thumb_hover
    } else {
        t.thumb
    };
    p.circle_filled(pos2(tx, track_rect.center().y), ring, t.chrome);
    p.circle_stroke(pos2(tx, track_rect.center().y), ring, Stroke::new(2.0, ring_c));
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    out
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

/// Collapsible section header ("› Light"). Returns the response (click toggles).
pub fn section_header(ui: &mut Ui, id: &str, title: &str, open: bool, enabled: Option<bool>) -> (Response, Option<bool>) {
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, 52.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    register(ui.ctx(), format!("section:{id}"), r);
    let p = ui.painter();
    if ui.rect_contains_pointer(r) {
        p.rect_filled(r, 0.0, t.chrome.gamma_multiply(1.06));
    }
    let chev = Rect::from_center_size(pos2(r.left() + 30.0, r.center().y), vec2(16.0, 16.0));
    paint(p, chev, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_label);
    p.text(pos2(r.left() + 44.0, r.center().y), Align2::LEFT_CENTER, title, t.semibold(14.0), t.text);
    let mut toggled = None;
    // shown while the pointer is anywhere on the header: `resp.hovered()` turns false as soon as the
    // pointer is over the eye itself (the eye is on top), which hid the eye before it could be
    // clicked (issue #316)
    if let Some(on) = enabled
        && (ui.rect_contains_pointer(r) || !on)
    {
        let eye = Rect::from_center_size(pos2(r.right() - 34.0, r.center().y), vec2(18.0, 18.0));
        let er = ui.interact(eye, ui.id().with(("eye", id)), Sense::click());
        crate::widgets::announce_selected(&er, crate::i18n::tr("Show / hide"), on);
        register(ui.ctx(), format!("sectionEye:{id}"), eye);
        paint(ui.painter(), eye, Icon::Eye, if on { t.icon } else { t.text_disabled });
        if !on {
            ui.painter().line_segment([eye.left_bottom(), eye.right_top()], Stroke::new(1.5, t.text_disabled));
        }
        if er.clicked() {
            toggled = Some(!on);
        }
    }
    (resp, toggled)
}

/// Thin divider between groups.
pub fn divider(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.divider);
}

/// A flyout row inside a section (Curve, Color Mixer, Color Grading…): inset dark box with icon tile.
pub fn flyout_row(ui: &mut Ui, id: &str, title: &str, icon: Icon, open: bool) -> Response {
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

/// A small bordered text button (Auto, B&W, HDR…).
pub fn text_button(ui: &mut Ui, id: &str, label: &str, active: bool) -> Response {
    let label = if id.starts_with("labelSet-") { label } else { crate::i18n::tr(label) };
    let t = Tokens::get(ui.ctx());
    let font = t.semibold(11.5);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, t.text);
    let size = vec2(text_button_width(galley.size().x), 24.0);
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
    p.rect(r, CornerRadius::same(4), fill, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
    p.galley(r.center() - galley.size() / 2.0, galley, t.text);
    resp
}

/// The width of a [`text_button`] whose label is `text_width` wide.
fn text_button_width(text_width: f32) -> f32 {
    (text_width + 18.0).max(37.0)
}

/// The width [`text_button`] takes for `label` (translated as it would be), to plan a row before drawing it.
pub fn text_button_size(ui: &Ui, label: &str) -> f32 {
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(crate::i18n::tr(label).to_string(), t.semibold(11.5), t.text);
    text_button_width(galley.size().x)
}

/// A segmented control: `labels` share the available width equally, `per_row` per row. Each segment is
/// addressable as `button:{id}-{key}` (keys parallel to labels). Returns the clicked index.
pub fn segmented(ui: &mut Ui, id: &str, items: &[(&str, &str)], active: Option<usize>, per_row: usize) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let per_row = per_row.max(1);
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
                let (r, resp) = ui.allocate_exact_size(vec2(seg_w, 24.0), Sense::click());
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
                p.rect(r, CornerRadius::same(4), fill, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, label, t.semibold(11.5), t.text);
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
    }
    clicked
}

/// An icon-only button. `active` draws the selected background (tool strip).
pub fn icon_button(ui: &mut Ui, id: &str, icon: Icon, size: egui::Vec2, active: bool, enabled: bool, tooltip: &str) -> Response {
    let tooltip = crate::i18n::tr(tooltip);
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, active, tooltip));
    register(ui.ctx(), format!("icon:{id}"), r);
    let p = ui.painter();
    let sq = Rect::from_center_size(r.center(), vec2(32.0f32.min(size.x), 32.0f32.min(size.y)));
    if active {
        p.rect_filled(sq, 4.0, t.tool_active);
    } else if resp.hovered() && enabled {
        p.rect_filled(sq, 4.0, t.hover.gamma_multiply(0.7));
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
    fn high_efficiency_nef_explains_the_codec_limit_and_workarounds() {
        let reason = "Nikon High Efficiency NEF (HE / HE★, a JPEG XS-based codec with no public specification; shoot RAW Lossless compressed to edit the raw data)";
        let explanation = preview_only_explanation(reason);
        assert!(explanation.contains("TicoRAW"), "{explanation}");
        assert!(explanation.contains("does not currently have a license"), "{explanation}");
        assert!(!explanation.contains("cannot decode"), "{explanation}");
        assert!(explanation.contains("Lossless compressed NEF"), "{explanation}");
        assert!(explanation.contains("DNG"), "{explanation}");
        assert!(!explanation.contains("yet"), "{explanation}");
        assert!(preview_only_explanation("unsupported CR3").contains("unsupported CR3"));
    }

    #[test]
    fn nudges_are_a_sensible_step() {
        let spec = |id, min, max, step| ControlSpec {
            id,
            label: "",
            section: lightcraft_develop::controls::Section::Light,
            min,
            max,
            default: 0.0,
            step,
            decimals: 2,
            track: Track::Plain,
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
        let temp = lightcraft_develop::controls::find("wb.temp").unwrap();
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
        let exposure = lightcraft_develop::controls::find("light.exposure").unwrap();
        let contrast = lightcraft_develop::controls::find("light.contrast").unwrap();
        let temp = lightcraft_develop::controls::find("wb.temp").unwrap();
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
        let spec = lightcraft_develop::controls::find("light.exposure").unwrap();
        let mut found = Vec::new();
        let mut ranges = Vec::new();
        for _ in 0..3 {
            let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = slider(ui, spec, 0.5, true, None);
                let _ = text_button(ui, "auto", "Auto", false);
                let _ = icon_button(ui, "trash", Icon::Trash, vec2(24.0, 24.0), false, true, "Delete mask");
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

#[cfg(test)]
#[path = "widgets/tests_slider_reset.rs"]
mod slider_reset_tests;

fn focused_slider_key() -> egui::Id {
    egui::Id::new("lightcraft-focused-slider")
}

/// The slider a click or drag armed for vertical wheel edits, if any.
fn wheel_slider_key() -> egui::Id {
    egui::Id::new("lightcraft-wheel-slider")
}

/// Whether the widget with keyboard focus is a develop slider selected by a click, drag or Tab.
/// It owns only the arrow keys: every other shortcut still works. Wheel selection is separate.
pub fn slider_has_focus(ctx: &egui::Context) -> bool {
    let focused = ctx.memory(|m| m.focused());
    focused.is_some() && focused == ctx.data(|d| d.get_temp::<egui::Id>(focused_slider_key()))
}
