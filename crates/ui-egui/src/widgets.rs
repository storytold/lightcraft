//! Lightroom-style widgets: sliders with gradient tracks and hollow thumbs, section headers,
//! flyout rows, bordered buttons. Every interactive widget registers an automation id + rect
//! ([`register`]) so the control channel can find and click it by name.

use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use lightcraft_develop::{ControlSpec, Track};

use crate::icons::{Icon, paint};
use crate::theme::Tokens;

/// Standard dropdown chevrons, including settings, import/export and rule editors.
pub fn combo_box(id: impl egui::AsIdSalt) -> craft_ui::ComboBox<'static> {
    craft_ui::ComboBox::from_id_salt(id)
        .icon(|ui, rect, visuals, _| {
            paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), Icon::ChevronDown, visuals.fg_stroke.color);
        })
        .popup_style(list_style.into())
}

/// Name a dropdown for screen readers. egui 0.37's `ComboBox::from_id_salt` publishes only the
/// selected text as the node's value, so an otherwise unlabelled combo has no accessible name and
/// the strict kittest check rejects it. Call this on the response returned by `show_ui`.
pub fn name_combo(response: &egui::Response, label: &str) {
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, response.enabled(), label));
}

/// A dropdown's list and its submenus: items without the resting border of a button, as egui's
/// own menus draw them. An unhovered item gives up its border's width of padding, so with one it
/// grew 2 points when hovered, and the longest name of a list sized to it wrapped (#2354).
fn list_style(style: &mut egui::Style) {
    style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
}

/// A native button with a vector arrow beside its text; egui reserves the icon's space.
pub fn arrow_button<'a>(ui: &mut Ui, text: impl egui::IntoAtoms<'a>, icon: Icon, leading: bool) -> Response {
    // 0.37 atoms take a local `IdSalt` (not a root `Id`); the button reports the custom atom's
    // rect under the same salt.
    let id = egui::IdSalt::new(("lc-arrow", ui.next_auto_id()));
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

/// The real egui widget that owns a registered automation rectangle.
///
/// Automation ids are names for scripts; egui hit-tests widget [`egui::Id`]s in a [`egui::LayerId`].
/// [`register`] records a small non-interactive probe with the registered rect so the control
/// channel can ask egui itself whether that rectangle owns the pointer, rather than guessing from
/// unrelated interactive rectangles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WidgetHit {
    pub id: egui::Id,
    pub layer_id: egui::LayerId,
    /// The widget's clipped interaction rectangle in its layer's local coordinates, recorded in
    /// the same pass as the registration. Delivery checks this instead of calling
    /// `Context::read_response` before the next pass: at that point `read_response` can still see
    /// the pass before the registry that was just published.
    pub interact_rect: Rect,
}

#[derive(Clone, Default)]
pub struct Registry {
    pub rects: Vec<(String, Rect)>,
    pub hits: Vec<(String, WidgetHit)>,
    /// How many times each automation id was registered in this pass (for stable duplicate probe
    /// ids). A map keeps registration linear; scanning `rects` would make a pass O(N²).
    occurrences: std::collections::HashMap<String, usize>,
}

/// Record a widget's automation id and screen rect for this frame, and register a hover probe at
/// the same rectangle in the current UI layer.
///
/// The probe senses hover only (no click or drag), so it never takes a click from the widget it
/// describes. It records the current UI layer and its clipped `Response::interact_rect` in the
/// same pass, and the control channel asks egui for the top-most interactable layer at the point.
/// A probe below a non-modal foreground/dim overlay is therefore refused without ever pressing the
/// overlay. A point below the top modal dialog is instead left to egui's modal routing: the dialog
/// consumes it, or a click on the backdrop closes the dialog, and no background widget is pressed.
pub fn register(ui: &egui::Ui, id: impl Into<String>, rect: Rect) {
    let ctx = ui.ctx();
    let registry = egui::Id::unique("lc-registry");
    let id = id.into();
    // Stable across frames even when unrelated widgets come and go: the probe id names the
    // automation id (and a duplicate occurrence of the same id in this pass), not its position.
    let duplicate = ctx.data_mut(|d| {
        let registry = d.get_temp_mut_or_default::<Registry>(registry);
        let occurrence = registry.occurrences.entry(id.clone()).or_insert(0);
        let duplicate = *occurrence;
        *occurrence = occurrence.saturating_add(1);
        duplicate
    });
    let hit = rect.is_finite().then(|| {
        let id = egui::Id::unique(("lc-automation-hit", id.as_str(), duplicate));
        let response = ui.interact(rect, id, egui::Sense::hover());
        WidgetHit { id, layer_id: response.layer_id, interact_rect: response.interact_rect }
    });
    ctx.data_mut(|d| {
        let registry = d.get_temp_mut_or_default::<Registry>(registry);
        registry.rects.push((id.clone(), rect));
        if let Some(hit) = hit {
            registry.hits.push((id, hit));
        }
    });
}

/// Record a widget whose real egui [`egui::Response`] is available. More precise than
/// [`register`]'s hover probe: the response carries the widget's own id, layer and clipped
/// geometry, and remains a valid automation target when disabled (`ui.clickWidget` on a disabled
/// control is a successful no-op, which is the documented automation semantics).
pub fn register_response(id: impl Into<String>, rect: Rect, response: &egui::Response) {
    let id = id.into();
    let ctx = response.ctx.clone();
    ctx.data_mut(|d| {
        let registry = d.get_temp_mut_or_default::<Registry>(egui::Id::unique("lc-registry"));
        registry.rects.push((id.clone(), rect));
        registry.hits.push((id, WidgetHit { id: response.id, layer_id: response.layer_id, interact_rect: response.interact_rect }));
    });
}

/// Record painted geometry that has no interactive egui widget of its own (a preview image, a
/// drop card). `ui.clickWidget` will not press it: there is no widget hit to own the press.
pub fn register_geometry(ctx: &egui::Context, id: impl Into<String>, rect: Rect) {
    let id = id.into();
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Registry>(egui::Id::unique("lc-registry")).rects.push((id, rect)));
}

/// Esc was handled this frame by something nested (an open popup closed on it): containers such
/// as dialogs shouldn't close too. Keyed to the frame, so it lapses by itself on the next one.
pub fn take_escape(ctx: &egui::Context) {
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("lc-escape-taken"), frame));
}

/// Whether [`take_escape`] was called this frame.
pub fn escape_taken(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(egui::Id::unique("lc-escape-taken"))) == Some(ctx.cumulative_frame_nr())
}

/// Take this pass's registered rectangles and their egui hit owners. The app publishes both only
/// for a completed, nondiscarded pass.
pub fn take_registries(ctx: &egui::Context) -> (Vec<(String, Rect)>, Vec<(String, WidgetHit)>) {
    let registry = ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<Registry>(egui::Id::unique("lc-registry"))));
    (registry.rects, registry.hits)
}

/// Take only the registered rectangles (for unit tests that inspect layout).
pub fn take_registry(ctx: &egui::Context) -> Vec<(String, Rect)> {
    take_registries(ctx).0
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
        .corner_radius(crate::theme::RADIUS_PANEL)
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
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, crate::i18n::tr("Preview only: editing the camera's embedded JPEG")));
    register(ui, format!("notice:previewOnly:{key}"), r.rect);
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

/// A slider's track: a thin line, a colour gradient where colour is the meaning (Temp, Tint, Hue,
/// Saturation…), with a small tick at the middle of a centred one. Dimmed when disabled.
fn paint_track(ui: &Ui, rect: Rect, track: &Track, t: &Tokens, enabled: bool) {
    let p = ui.painter();
    let y = rect.center().y;
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
    // one mesh with a colour per column of vertices: smooth, and no overlapping strips that would
    // stripe a dimmed (translucent) track
    let paint_ramp = |colour: &dyn Fn(f32) -> Color32| {
        let h = TRACK_COLOR_H / 2.0;
        let steps: u32 = 48;
        let mut mesh = egui::Mesh::default();
        for i in 0..=steps {
            let a = i as f32 / steps as f32;
            let x = rect.left() + rect.width() * a;
            let c = dim(colour(a));
            mesh.colored_vertex(pos2(x, y - h), c);
            mesh.colored_vertex(pos2(x, y + h), c);
            if i > 0 {
                let k = 2 * i;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        p.add(egui::Shape::mesh(mesh));
    };
    if let Some(stops) = track_stops(track) {
        // gradient: segments between stops
        let n = stops.len().max(2) - 1;
        paint_ramp(&|a: f32| {
            let seg = (a * n as f32).floor() as usize;
            let local = a * n as f32 - seg as f32;
            lerp(stops[seg.min(n)], stops[(seg + 1).min(n)], local)
        });
    } else {
        let h = TRACK_H / 2.0;
        p.rect_filled(Rect::from_min_max(pos2(rect.left(), y - h), pos2(rect.right(), y + h)), 1.0, dim(t.track));
    }
    if matches!(track, Track::Centered) {
        // where the slider has no effect
        p.rect_filled(Rect::from_center_size(pos2(rect.center().x, y), vec2(1.0, 6.0)), 0.0, dim(t.track));
    }
}

/// A slider's handle: a small dot on the track, a little larger while hovered or dragged, ringed
/// in the panel colour so it stands off a colour track.
fn paint_handle(ui: &Ui, at: egui::Pos2, hot: bool, color: Color32, t: &Tokens) {
    let r = if hot { HANDLE_R_HOT } else { HANDLE_R };
    let p = ui.painter();
    p.circle_filled(at, r + 1.0, t.chrome);
    p.circle_filled(at, r, color);
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

/// Which slider the vertical wheel edits: clicking or dragging a slider arms it (and disarms the others), leaving
/// its row or its focus disarms it. Returns whether this slider is armed, and whether the pointer is in its row.
fn update_wheel_arming(ui: &mut Ui, id: egui::Id, resp: &egui::Response, row: Rect, enabled: bool) -> (bool, bool) {
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
    (wheel_armed, pointer_in_row)
}

/// The radius of a slider's handle, and while it is hovered or dragged.
pub(crate) const HANDLE_R: f32 = 4.5;
pub(crate) const HANDLE_R_HOT: f32 = 5.5;
/// A plain track's thickness, and a colour track's (a little thicker, so its colours read).
const TRACK_H: f32 = 2.0;
const TRACK_COLOR_H: f32 = 3.0;
/// Space between a slider row's name, track and value.
const ROW_GAP: f32 = 6.0;
/// Gap between a right-aligned name and its track: room for the handle sitting at the track's
/// left end, so a name never runs into it.
const NAME_GAP: f32 = ROW_GAP + HANDLE_R_HOT;
/// Left and right margins of the panels' rows (sliders, captions, buttons line up on them).
pub const PAD_L: f32 = 14.0;
pub const PAD_R: f32 = 12.0;
/// The panels' row margins, `top` and `bottom` apart, as an egui frame margin.
pub fn row_margin(top: i8, bottom: i8) -> egui::Margin {
    egui::Margin { left: PAD_L as i8, right: PAD_R as i8, top, bottom }
}
/// The size of a slider's name and value.
const ROW_FONT: f32 = 11.0;
/// The name column's narrowest, and the most of a row (inside its margins) it may take: a longer
/// name is cut short with … (shown whole on hover).
const NAME_MIN: f32 = 24.0;
const NAME_SHARE_MAX: f32 = 0.4;
/// Width of the value column at a row's right ("50000", "+0.17", "-100" at [`ROW_FONT`]).
const VALUE_W: f32 = 32.0;

/// Where the parts of a one-line slider row sit: the name in a column at the left (`name_w`
/// wide: the longest name of its group, see [`name_column`]), the value right-aligned at the
/// right, the track between them — as long as the names leave it. Each part is the row's full
/// height, so all of the row takes the pointer. A row too narrow for its parts still gives the
/// track a point of width (it is divided by).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RowParts {
    pub label: Rect,
    pub track: Rect,
    pub value: Rect,
}

pub(crate) fn row_parts(row: Rect, name_w: f32) -> RowParts {
    // a row too narrow for everything gives up its margins first, then shrinks the name and value
    // columns, then the gaps, so every part stays inside it with a point of track to divide by
    let row_w = if row.width().is_finite() { row.width().max(0.0) } else { 0.0 };
    let pads = PAD_L + PAD_R;
    let kp = (row_w - 1.0).max(0.0).min(pads) / pads;
    let left = row.left() + PAD_L * kp;
    let right = (row.right() - PAD_R * kp).max(left);
    let w = right - left;
    let track_min = w.min(1.0);
    let name_w = if name_w.is_finite() { name_w } else { 0.0 };
    let mut label_w = name_w.max(NAME_MIN).min(w * NAME_SHARE_MAX);
    let mut value_w = VALUE_W.min(w * 0.25);
    let columns = (w - track_min).max(0.0);
    if label_w + value_w > columns {
        let k = columns / (label_w + value_w);
        label_w *= k;
        value_w *= k;
    }
    // room for the handle at the track's left end, as far as a very narrow row allows
    let name_gap = NAME_GAP.min(ROW_GAP.max(w * 0.05));
    let spare = (w - label_w - value_w - track_min).max(0.0);
    let kg = if name_gap + ROW_GAP > spare { spare / (name_gap + ROW_GAP) } else { 1.0 };
    let label = Rect::from_min_max(pos2(left, row.top()), pos2(left + label_w, row.bottom()));
    let value = Rect::from_min_max(pos2(right - value_w, row.top()), pos2(right, row.bottom()));
    let track_l = (label.right() + name_gap * kg).min(value.left());
    let track_r = (value.left() - ROW_GAP * kg).clamp(track_l, value.left());
    let track = Rect::from_min_max(pos2(track_l, row.top()), pos2(track_r, row.bottom()));
    RowParts { label, track, value }
}

/// Where a slider's name starts: right-aligned in its name column, so it ends [`NAME_GAP`] before
/// its own track however short it is (a name too wide for the column starts at its left edge).
pub(crate) fn name_left(label: Rect, text_w: f32) -> f32 {
    let w = if text_w.is_finite() { text_w.max(0.0) } else { 0.0 };
    (label.right() - w).max(label.left())
}

/// Where the current slider group is kept, per layer (a dialog's sliders aren't the panel's).
fn group_slot(layer: egui::LayerId) -> egui::Id {
    egui::Id::unique(("slider-group", layer))
}

/// Starts a group of sliders whose names share one column (a section: [`section_header`] calls
/// it). Sliders drawn outside any group this pass share a column with the others in their `Ui`.
pub(crate) fn begin_slider_group(ui: &Ui, id: &str) {
    let pass = ui.ctx().cumulative_pass_nr();
    ui.data_mut(|d| d.insert_temp(group_slot(ui.layer_id()), (egui::Id::unique(("slider-names", id)), pass)));
}

/// Draws `body` as a slider group of its own (a flyout such as Color Grading or the Color Mixer):
/// its names get their own column, so a few long ones ("Highlights Luminance") neither shorten the
/// tracks of the section around it nor move them as it opens and closes. The section's group
/// carries on after it.
pub(crate) fn slider_group<R>(ui: &mut Ui, id: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    let slot = group_slot(ui.layer_id());
    let outer = ui.data(|d| d.get_temp::<(egui::Id, u64)>(slot));
    begin_slider_group(ui, id);
    let out = body(ui);
    ui.data_mut(|d| {
        if let Some(g) = outer {
            d.insert_temp(slot, g);
        } else {
            d.remove::<(egui::Id, u64)>(slot);
        }
    });
    out
}

/// The width of the name column of the slider group being drawn: the longest name in it, as
/// measured over this pass and the one before (a group's rows below this one count once they have
/// been drawn), so a section's tracks start together just after its longest name.
fn name_column(ui: &Ui, name_w: f32) -> f32 {
    let pass = ui.ctx().cumulative_pass_nr();
    let group = match ui.data(|d| d.get_temp::<(egui::Id, u64)>(group_slot(ui.layer_id()))) {
        Some((g, p)) if p == pass => g,
        _ => ui.scope_id().with("slider-names"),
    };
    let name_w = if name_w.is_finite() { name_w.max(0.0) } else { 0.0 };
    let (seen, cur, prev) = ui.data(|d| d.get_temp::<(u64, f32, f32)>(group)).unwrap_or((pass, 0.0, 0.0));
    // a new pass: the last one's widest becomes the remembered one
    let (cur, prev) = if seen == pass { (cur, prev) } else { (0.0, cur) };
    let cur = cur.max(name_w);
    ui.data_mut(|d| d.insert_temp(group, (pass, cur, prev)));
    cur.max(prev)
}

/// The one slider of the app (Edit panel, mask panel, dialogs): ONE row of the panel's
/// `slider_row_h` — the name at the left, a thin track with a small handle, the value
/// right-aligned at the right. The whole row takes the pointer: click or drag the track to set the
/// value there, drag the name or the value sideways to move it from where it is (Shift: a tenth as
/// far), double-click the name or the handle to reset (hovering either says so), click the value
/// to type one (Return or clicking away applies it, Esc keeps the old one). Scroll sideways over
/// it, or the wheel after clicking it, to nudge it; ↑ / ↓ nudge a hovered or focused slider. A
/// value at its default is dim, a changed one bright.
pub fn slider(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, label_override: Option<&str>) -> SliderOut {
    slider_named(ui, spec, value, enabled, label_override, label_override)
}

/// [`slider`] showing the name `shown` (or the spec's) while screen readers announce `spoken` (or
/// the spec's): a short name under a caption ("Amount" under Grain) only makes sense to the eye,
/// so assistive tools get the control's full name ("Grain").
pub fn slider_named(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, shown: Option<&str>, spoken: Option<&str>) -> SliderOut {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(w, t.slider_row_h), Sense::hover());
    // the focused row's highlight goes under everything drawn in it
    let backdrop = ui.painter().add(egui::Shape::Noop);
    // (a hostile value is drawn at the nearest end, never at NaN)
    let value = if value.is_finite() { value } else { spec.default };
    let label_text = crate::i18n::tr(shown.unwrap_or(spec.label)).to_string();
    let access_name = crate::i18n::tr(spoken.unwrap_or(spec.label)).to_string();
    let font = t.font(ROW_FONT);
    let name_w = ui.painter().layout_no_wrap(label_text.clone(), font.clone(), t.text).size().x.ceil();
    let RowParts { label: label_rect, track: track_rect, value: value_rect } = row_parts(row, name_column(ui, name_w));
    let id = ui.scope_id().with(spec.id);
    let g = SliderLayout { spec, value, enabled, id, track_rect, value_rect };
    let resp = ui.interact(track_rect.expand2(vec2(HANDLE_R_HOT, 0.0)), id, if enabled { Sense::click_and_drag() } else { Sense::hover() });
    let (wheel_armed, pointer_in_row) = update_wheel_arming(ui, id, &resp, row, enabled);
    // screen readers: a slider named after its control, with its value
    resp.widget_info(|| egui::WidgetInfo::slider(enabled, value, access_name.clone()));
    describe_slider(ui, &g, &resp);
    // the name: double-click it to reset, as the handle; drag it sideways to move the value from
    // where it is (a disabled slider's name is only text)
    let label_resp = ui.interact(label_rect, id.with("label"), if enabled { Sense::click_and_drag() } else { Sense::hover() });
    // say how to reset (issue #1436: the double-click was hard to discover), as the curve channels do
    let reset_hint = crate::i18n::tr_format!("{name} — double-click to reset it", name = label_text);
    let label_resp = if enabled { label_resp.on_hover_text(reset_hint.as_str()) } else { label_resp };
    crate::widgets::announce(&label_resp, crate::i18n::tr_format!("Reset {name}", name = access_name.clone()));
    register(ui, format!("sliderLabel:{}", spec.id), label_rect);
    register(ui, format!("slider:{}", spec.id), track_rect);
    // the value: click it to type an exact one, drag it sideways to scrub
    let value_resp = ui.interact(value_rect, id.with("value"), if enabled { Sense::click_and_drag() } else { Sense::hover() });
    crate::widgets::announce(&value_resp, format!("{access_name}: {}", crate::i18n::tr("Click to type a value, drag to change it")));
    register(ui, format!("sliderValue:{}", spec.id), value_rect);
    // say that an exact value can be typed here (issue #534: an exact crop angle was hard to find)
    let value_resp = if enabled { value_resp.on_hover_text(crate::i18n::tr("Click to type a value, drag to change it")) } else { value_resp };
    // Keep the short focus-acquisition guard, but let an explicit Return/Escape end the edit.
    let mut typing: Option<Draft> = ui.data(|m| m.get_temp(g.typing_id()));
    // A scrub: the unrounded track position (0..1) it has reached, so a slow drag adds up instead
    // of rounding back to the same step every frame. (The name scrubs as the value does.)
    let scrubber = if label_resp.dragged() || label_resp.drag_stopped() { &label_resp } else { &value_resp };
    let mut scrub: Option<f64> = if scrubber.dragged() || scrubber.drag_stopped() { ui.data(|m| m.get_temp(g.scrub_id())) } else { None };
    let scrub_started = begin_scrub(ui, &g, scrubber, &mut scrub, &mut typing);
    if scrub.is_none() {
        ui.data_mut(|m| m.remove::<f64>(g.scrub_id()));
    }
    update_typing_gesture(ui, &g, &value_resp, &mut typing);
    let mut out = SliderOut::default();
    // the handle: a click there doesn't move the value, and resting on it says how to reset (the
    // rest of the track, which is clicked and dragged, gets no tooltip)
    if enabled && !resp.dragged() && resp.hover_pos().is_some_and(|p| g.on_thumb(p)) {
        resp.clone().on_hover_text(reset_hint.as_str());
    }
    if let Some(pos) = scrub {
        scrub_value(ui, &g, scrubber, pos, scrub_started, &mut out);
    } else if let Some(draft) = typing.take() {
        typing = edit_value(ui, &t, &g, draft, &mut out);
    } else if resp.double_clicked() || label_resp.double_clicked() {
        out.reset = true;
        out.value = Some(spec.default);
    } else {
        pointer_edit(ui, &g, &resp, &mut out);
        nudge_edit(ui, &g, &resp, pointer_in_row, &mut out);
        side_scroll_edit(ui, &g, pointer_in_row, &mut out);
        wheel_edit(ui, &g, wheel_armed, pointer_in_row, &mut out);
    }
    let v = out.value.unwrap_or(value);
    let looks = SliderLooks {
        row,
        backdrop,
        label_rect,
        label_text: &label_text,
        font,
        hovered: resp.hovered() || resp.dragged(),
        scrubbing: scrub.is_some(),
        name_hovered: label_resp.hovered(),
        value_hovered: value_resp.hovered(),
        focused: enabled && resp.has_focus(),
        typing: typing.is_some(),
    };
    paint_slider(ui, &t, &g, &looks, v);
    out
}

/// What [`paint_slider`] draws a slider row with.
struct SliderLooks<'a> {
    row: Rect,
    /// the shape under the row, for the focused row's highlight
    backdrop: egui::layers::ShapeIdx,
    label_rect: Rect,
    label_text: &'a str,
    font: egui::FontId,
    /// the track hovered or dragged
    hovered: bool,
    scrubbing: bool,
    name_hovered: bool,
    value_hovered: bool,
    focused: bool,
    typing: bool,
}

/// Paints a slider row at value `v`: the focused row's highlight, the name, the value (unless one is typed), the track
/// and its handle, and the pointer.
fn paint_slider(ui: &Ui, t: &Tokens, g: &SliderLayout, l: &SliderLooks, v: f64) {
    let enabled = g.enabled;
    let hot = enabled && (l.hovered || l.scrubbing || l.name_hovered || l.value_hovered);
    if l.focused {
        // the slider the arrow keys (and the console) move
        ui.painter().set(l.backdrop, egui::Shape::rect_filled(l.row, 2.0, t.hover.gamma_multiply(0.45)));
    }
    let name_c = if !enabled {
        t.text_disabled
    } else if hot || l.focused {
        t.text
    } else {
        t.text_label
    };
    // a name too long for its column is cut short (hovering it shows it whole, with the reset hint)
    let name = one_line(ui.painter(), l.label_text, l.font.clone(), name_c, l.label_rect.width());
    let name_x = name_left(l.label_rect, name.size().x);
    ui.painter().galley(pos2(name_x, l.label_rect.center().y - name.size().y / 2.0), name, name_c);
    if !l.typing {
        // a value at its default is dim, a changed one bright: what was touched shows at a glance
        let changed = (v - g.spec.default).abs() > g.spec.step.max(1e-9) / 2.0;
        let value_c = if !enabled {
            t.text_disabled
        } else if changed || hot {
            t.text
        } else {
            t.text_dim
        };
        ui.painter().text(g.value_rect.right_center(), Align2::RIGHT_CENTER, shown_value(g.spec, v), l.font.clone(), value_c);
    }
    // the scrub pointer, as over Lightroom's values: a drag on the value or the name changes it
    if enabled && ((!l.typing && (l.value_hovered || l.scrubbing)) || l.name_hovered) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    paint_track(ui, g.track_rect, &g.spec.track, t, enabled);
    let handle_c = if !enabled {
        t.text_disabled
    } else if l.hovered || l.scrubbing {
        t.thumb_hover
    } else {
        t.thumb
    };
    paint_handle(ui, pos2(g.to_x(v), g.track_rect.center().y), l.hovered || l.scrubbing, handle_c, t);
    if l.hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
}

/// A value being typed into a slider: the text, the frames it has been open, and whether the press that opened it is
/// still down.
type Draft = (String, u8, bool);

/// What a slider is, as its interaction code needs it.
struct SliderLayout<'a> {
    spec: &'a ControlSpec,
    value: f64,
    enabled: bool,
    id: egui::Id,
    /// where the thumb moves
    track_rect: Rect,
    /// the readout, where a value is typed or scrubbed
    value_rect: Rect,
}

impl SliderLayout<'_> {
    fn typing_id(&self) -> egui::Id {
        self.id.with("typing")
    }

    fn scrub_id(&self) -> egui::Id {
        self.id.with("scrub-pos")
    }

    /// The text field a value is typed in.
    fn field_id(&self) -> egui::Id {
        crate::text_field::id(&format!("sliderField:{}", self.spec.id))
    }

    fn step(&self) -> f64 {
        self.spec.step.max(1e-9)
    }

    /// `v` on the control's steps and within its range.
    fn snap(&self, v: f64) -> f64 {
        ((v / self.step()).round() * self.step()).clamp(self.spec.min, self.spec.max)
    }

    /// Whether `nv` is a different value from the one shown.
    fn differs(&self, nv: f64) -> bool {
        (nv - self.value).abs() > 1e-12
    }

    /// The track's width, never zero (it is divided by).
    fn track_w(&self) -> f32 {
        self.track_rect.width().max(1.0)
    }

    /// value ↔ track position: linear, except for the Kelvin temperature (`ControlSpec::position`)
    fn to_x(&self, v: f64) -> f32 {
        self.track_rect.left() + (self.spec.position(v) as f32).clamp(0.0, 1.0) * self.track_rect.width()
    }

    fn value_at_x(&self, x: f32) -> f64 {
        self.spec.value_at(f64::from(((x - self.track_rect.left()) / self.track_w()).clamp(0.0, 1.0)))
    }

    /// Whether `p` is on the handle.
    fn on_thumb(&self, p: egui::Pos2) -> bool {
        (p.x - self.to_x(self.value)).abs() <= HANDLE_R_HOT + 1.0
    }

    /// A change made in one go (a click, a key, a wheel step): it starts and stops the drag at once.
    fn set_whole(&self, out: &mut SliderOut, nv: f64) {
        out.value = Some(nv);
        out.drag_started = true;
        out.drag_stopped = true;
    }

    /// Move to `nv` as a whole change, when it is a different value.
    fn set_whole_if_changed(&self, out: &mut SliderOut, nv: f64) {
        if self.differs(nv) {
            self.set_whole(out, nv);
        }
    }
}

/// Tell screen readers the slider's range, step and the value as shown ("+0.50", "5600").
fn describe_slider(ui: &Ui, g: &SliderLayout, resp: &Response) {
    let shown = shown_value(g.spec, g.value);
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_min_numeric_value(g.spec.min);
        node.set_max_numeric_value(g.spec.max);
        node.set_numeric_value_step(g.spec.step);
        node.set_value(shown);
        if g.enabled {
            node.add_action(egui::accesskit::Action::Increment);
            node.add_action(egui::accesskit::Action::Decrement);
        }
    });
}

/// A press on the readout (or the name) that moves sideways starts a scrub: the edit the press opened is dropped. Returns whether
/// a scrub started this frame (`scrub` then holds its first position).
fn begin_scrub(ui: &mut Ui, g: &SliderLayout, value_resp: &Response, scrub: &mut Option<f64>, typing: &mut Option<Draft>) -> bool {
    if !g.enabled || scrub.is_some() || !value_resp.dragged() {
        return false;
    }
    let (Some(origin), Some(p)) = ui.input(|i| (i.pointer.press_origin(), i.pointer.interact_pos())) else { return false };
    if (p.x - origin.x).abs() < SCRUB_START {
        return false;
    }
    *typing = None;
    ui.data_mut(|m| m.remove::<Draft>(g.typing_id()));
    ui.memory_mut(|m| m.surrender_focus(g.field_id()));
    // the travel so far counts, so the value doesn't lag the pointer by the start distance
    let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
    *scrub = Some((g.spec.position(g.value) + f64::from((p.x - origin.x) / g.track_w()) * fine).clamp(0.0, 1.0));
    true
}

/// Open the typed edit on a press or click of the readout; drop it when the press ends elsewhere.
fn update_typing_gesture(ui: &mut Ui, g: &SliderLayout, value_resp: &Response, typing: &mut Option<Draft>) {
    let value_pressed = value_resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
    // Focus on the owned primary press before fast typing; retain the readout's click activation.
    if g.enabled && typing.is_none() && (value_resp.clicked() || value_pressed) {
        *typing = Some((shown_value(g.spec, g.value).trim_start_matches('+').to_string(), 0, value_pressed));
    }
    let value_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
    // A release over the value keeps the field the press opened, however long the button was held
    // (a press held past egui's click time ends without a click). A release elsewhere cancels the
    // gesture, and a press that scrubs already dropped the field above.
    let released_over_value = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| g.value_rect.contains(p));
    if value_released && !released_over_value && typing.as_ref().is_some_and(|(_, _, pressed)| *pressed) && !value_resp.clicked() {
        *typing = None;
        ui.data_mut(|m| m.remove::<Draft>(g.typing_id()));
        ui.memory_mut(|m| m.surrender_focus(g.field_id()));
    }
}

/// Scrubbing the value moves it as dragging the thumb would: the pointer's sideways travel over the track's width (a
/// tenth with Shift), on the control's steps. `pos` is the unrounded track position reached so far.
fn scrub_value(ui: &mut Ui, g: &SliderLayout, value_resp: &Response, mut pos: f64, started: bool, out: &mut SliderOut) {
    if !started && value_resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
        let dx = ui.input(|i| i.pointer.delta().x);
        pos = (pos + f64::from(dx / g.track_w()) * fine).clamp(0.0, 1.0);
    }
    let nv = g.snap(g.spec.value_at(pos));
    if g.differs(nv) {
        out.value = Some(nv);
    }
    out.drag_started = started;
    if value_resp.drag_stopped() {
        out.drag_stopped = true;
        ui.data_mut(|m| m.remove::<f64>(g.scrub_id()));
    } else {
        ui.data_mut(|m| m.insert_temp(g.scrub_id(), pos));
    }
}

/// The text field over the readout while a value is typed. Returns the draft to keep, if the edit goes on.
fn edit_value(ui: &mut Ui, t: &Tokens, g: &SliderLayout, draft: Draft, out: &mut SliderOut) -> Option<Draft> {
    let (mut text, frames, pressed) = draft;
    // Keep the owned opening release focused without restarting its draft or selection.
    // egui gives that release's click to the readout, so the field under it gives up the focus
    // whatever frame it lands on (issue #1362): the field takes it back after drawing too.
    let released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
    let opening_release = pressed && released;
    if opening_release {
        ui.memory_mut(|m| m.request_focus(g.field_id()));
    }
    let pressed = pressed && !released;
    // a child over the value, so the field never moves the rows around it
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(g.value_rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
    // the old value is selected when the field takes the focus, so typing replaces it
    let field = crate::text_field::TextField::singleline(&format!("sliderField:{}", g.spec.id), &mut text)
        .font(t.font(ROW_FONT))
        .horizontal_align(egui::Align::RIGHT)
        .width(g.value_rect.width())
        .margin(egui::Margin::ZERO)
        .select_on_focus(true)
        .begin_edit(frames == 0)
        .show(&mut child);
    if (frames < 2 || opening_release) && !matches!(field.ending, Some(crate::text_field::Ending::Return | crate::text_field::Ending::Escape)) {
        field.response.request_focus();
        let draft = (text, frames.saturating_add(1), pressed);
        ui.data_mut(|m| m.insert_temp(g.typing_id(), draft.clone()));
        return Some(draft);
    }
    if field.ending.is_some() || !field.editing || !g.enabled {
        // Esc (or the slider turning off) keeps the old value; Return or clicking away applies
        let cancelled = !g.enabled || field.cancelled();
        if let Some(nv) = typed_value(g.spec, &text).filter(|nv| !cancelled && g.differs(*nv)) {
            g.set_whole(out, nv);
        }
        ui.data_mut(|m| m.remove::<Draft>(g.typing_id()));
        return None;
    }
    let draft = (text, frames, pressed);
    ui.data_mut(|m| m.insert_temp(g.typing_id(), draft.clone()));
    Some(draft)
}

/// Dragging the thumb or the track, or clicking the track.
fn pointer_edit(ui: &mut Ui, g: &SliderLayout, resp: &Response, out: &mut SliderOut) {
    if resp.drag_started() {
        out.drag_started = true;
    }
    if (resp.dragged() || resp.drag_started())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let raw = if ui.input(|i| i.modifiers.shift) { fine_drag_value(ui, g) } else { g.value_at_x(p.x) };
        let nv = g.snap(raw);
        if g.differs(nv) {
            out.value = Some(nv);
        }
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
        // a click on the thumb leaves it there, so the first click of a double-click on it isn't a
        // history step of its own before the reset
        && !g.on_thumb(p)
    {
        g.set_whole(out, g.snap(g.value_at_x(p.x)));
    }
    if resp.drag_stopped() {
        out.drag_stopped = true;
    }
}

/// A tenth of the pointer's movement along the track; the unrounded position is kept, so a slow drag adds up instead
/// of rounding back to the same step every frame.
fn fine_drag_value(ui: &mut Ui, g: &SliderLayout) -> f64 {
    let step = g.step();
    let fine_id = g.id.with("fine-pos");
    let dx = ui.input(|i| i.pointer.delta().x);
    let kept = ui.data(|m| m.get_temp::<f64>(fine_id)).filter(|&p| ((g.spec.value_at(p) / step).round() * step - g.value).abs() < step / 2.0);
    let pos = (kept.unwrap_or_else(|| g.spec.position(g.value)) + f64::from(dx / g.track_w()) * 0.1).clamp(0.0, 1.0);
    ui.data_mut(|m| m.insert_temp(fine_id, pos));
    g.spec.value_at(pos)
}

/// A screen reader's increment / decrement (VoiceOver: Ctrl-Option-Up / Down) as `(increment, decrement)`.
fn accessibility_nudge(ui: &Ui, resp: &Response) -> (bool, bool) {
    ui.input(|i| {
        i.events.iter().fold((false, false), |(inc, dec), e| match e {
            egui::Event::AccessKitActionRequest(r) if r.target_node == resp.id.accesskit_id() => {
                (inc || r.action == egui::accesskit::Action::Increment, dec || r.action == egui::accesskit::Action::Decrement)
            }
            _ => (inc, dec),
        })
    })
}

/// The arrow keys (with either a hovered or a focused slider; Shift: five steps) and a screen reader's actions.
fn nudge_edit(ui: &mut Ui, g: &SliderLayout, resp: &Response, pointer_in_row: bool, out: &mut SliderOut) {
    let (inc, dec) = accessibility_nudge(ui, resp);
    if !g.enabled || out.value.is_some() || !(pointer_in_row || resp.has_focus() || inc || dec) {
        return;
    }
    let (up, down, shift) = ui.input_mut(|i| {
        let shift = i.modifiers.shift;
        let m = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
        (i.consume_key(m, egui::Key::ArrowUp), i.consume_key(m, egui::Key::ArrowDown), shift)
    });
    let (up, down) = (up || inc, down || dec);
    if up || down {
        let nv = nudged(g.spec, g.value, if up { 1.0 } else { -1.0 } * if shift { 5.0 } else { 1.0 });
        g.set_whole_if_changed(out, nv);
    }
}

/// Sideways scrolling (a trackpad swipe, a tilting wheel, Shift+wheel) adjusts the slider under
/// the pointer, selected or not: the panel only scrolls vertically, so nothing else wants it.
/// Only a mostly sideways movement counts, so a slightly diagonal swipe that scrolls the panel
/// leaves the sliders it passes alone. Right raises the value, as dragging the thumb right does.
fn side_scroll_edit(ui: &mut Ui, g: &SliderLayout, pointer_in_row: bool, out: &mut SliderOut) {
    let side_id = g.id.with("side-rem");
    let (dx, dy) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y));
    if !g.enabled || out.value.is_some() || !pointer_in_row || dx.abs() <= dy.abs() {
        ui.data_mut(|m| m.remove::<f32>(side_id));
        return;
    }
    // the vertical part would scroll the panel and move the slider out from under the pointer
    ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    let mut remainder = ui.data(|m| m.get_temp::<f32>(side_id)).unwrap_or(0.0);
    let steps = wheel_nudges(&mut remainder, dx);
    ui.data_mut(|m| m.insert_temp(side_id, remainder));
    if steps != 0.0 {
        g.set_whole_if_changed(out, nudged(g.spec, g.value, steps));
    }
}

/// Wheel nudges require an explicitly selected (armed) slider, unlike regular panel scrolling.
/// Consume only the vertical component so a trackpad's horizontal scroll still works.
fn wheel_edit(ui: &mut Ui, g: &SliderLayout, armed: bool, pointer_in_row: bool, out: &mut SliderOut) {
    if !armed || !g.enabled || out.value.is_some() || !pointer_in_row {
        return;
    }
    let delta = ui.input_mut(|i| {
        let dy = i.smooth_scroll_delta.y;
        if dy != 0.0 {
            i.smooth_scroll_delta.y = 0.0;
        }
        dy
    });
    if delta == 0.0 {
        return;
    }
    let wheel_id = g.id.with("wheel-rem");
    let mut remainder = ui.data(|m| m.get_temp::<f32>(wheel_id)).unwrap_or(0.0);
    let steps = wheel_nudges(&mut remainder, delta);
    ui.data_mut(|m| m.insert_temp(wheel_id, remainder));
    if steps != 0.0 {
        g.set_whole_if_changed(out, nudged(g.spec, g.value, steps));
    }
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

/// Announce a disclosure row with AccessKit's real `Expanded` state.
///
/// egui 0.37's `WidgetInfo::selected` maps to AccessKit `Toggled`; a custom disclosure row must
/// publish `Expanded` instead (the built-in `CollapsingHeader` does this through
/// `accesskit_node_builder`). Use one helper so section headers, flyouts and fold titles agree.
pub(crate) fn announce_disclosure(resp: &egui::Response, enabled: bool, expanded: bool, label: impl ToString) {
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::DisclosureTriangle, enabled, label.to_string()));
    resp.ctx.accesskit_node_builder(resp.id, |node| node.set_expanded(expanded));
}

/// Title size of a major section header (Light, Color…); sub-section rows use [`SUB_TITLE`].
pub(crate) const SECTION_TITLE: f32 = 13.0;
/// Title size of a sub-section row (Curve, Color Mixer…): smaller and regular weight, so it reads
/// as part of its section.
pub(crate) const SUB_TITLE: f32 = 12.0;

/// The fill of a major section header's band: the section-band token, a shade lighter (darker in
/// light themes) under the pointer.
pub(crate) fn section_fill(t: &Tokens, hovered: bool) -> Color32 {
    if hovered { t.section_band.lerp_to_gamma(t.text, 0.06) } else { t.section_band }
}

/// The fill of a sub-section row (Curve, Color Mixer…): none at rest, a light hover, so the rows
/// stay quieter than the section header above them.
pub(crate) fn flyout_fill(t: &Tokens, hovered: bool) -> Option<Color32> {
    hovered.then(|| t.chrome.lerp_to_gamma(t.hover, 0.6))
}

/// Paints a major section header's band over `r`: the band fill and a separator along its top.
pub(crate) fn paint_section_band(p: &egui::Painter, r: Rect, t: &Tokens, hovered: bool) {
    p.rect_filled(r, 0.0, section_fill(t, hovered));
    p.rect_filled(Rect::from_min_max(r.left_top(), pos2(r.right(), r.top() + 1.0)), 0.0, t.divider);
}

/// Collapsible section header ("› Light"): one thin bar of the theme's `section_h`, the chevron
/// and name at the left, the section's eye (show the photo without it) at the right. The same bar
/// heads the Edit panel's and the mask panel's sections, and would head a tab's (Tabs layout).
/// Returns the response (click toggles) and the eye's new state when it was clicked.
pub fn section_header(ui: &mut Ui, id: &str, title: &str, open: bool, enabled: Option<bool>) -> (Response, Option<bool>) {
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, t.section_h), Sense::click());
    announce_disclosure(&resp, true, open, title);
    register(ui, format!("section:{id}"), r);
    // the section's sliders line their names up in one column of their own
    begin_slider_group(ui, id);
    let p = ui.painter();
    paint_section_band(p, r, &t, ui.rect_contains_pointer(r));
    if resp.has_focus() {
        p.rect_stroke(r.shrink(1.0), crate::theme::RADIUS, Stroke::new(1.5, t.accent), StrokeKind::Inside);
    }
    let chev = Rect::from_center_size(pos2(r.left() + PAD_L + 5.0, r.center().y), vec2(12.0, 12.0));
    paint(p, chev, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text);
    p.text(pos2(r.left() + PAD_L + 16.0, r.center().y), Align2::LEFT_CENTER, title, t.semibold(SECTION_TITLE), t.text);
    let mut toggled = None;
    // shown while the pointer is anywhere on the header: `resp.hovered()` turns false as soon as the
    // pointer is over the eye itself (the eye is on top), which hid the eye before it could be
    // clicked (issue #316)
    if let Some(on) = enabled
        && (ui.rect_contains_pointer(r) || !on)
    {
        let eye = Rect::from_center_size(pos2(r.right() - PAD_R - 8.0, r.center().y), vec2(15.0, 15.0));
        let er = ui.interact(eye, ui.scope_id().with(("eye", id)), Sense::click());
        crate::widgets::announce_selected(&er, crate::i18n::tr("Show / hide"), on);
        register(ui, format!("sectionEye:{id}"), eye);
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

/// A sub-section row inside a section (Curve, Color Mixer, Color Grading…): indented icon and a
/// regular-weight title, no box of its own, so it reads as part of the section above it.
pub fn flyout_row(ui: &mut Ui, id: &str, title: &str, icon: Icon, open: bool) -> Response {
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (outer, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    let r = Rect::from_min_max(pos2(outer.left() + PAD_L - 4.0, outer.top() + 2.0), pos2(outer.right() - PAD_R + 4.0, outer.bottom() - 2.0));
    let resp = ui.interact(r, ui.scope_id().with(("flyout", id)), Sense::click());
    announce_disclosure(&resp, true, open, title);
    register(ui, format!("flyout:{id}"), r);
    let p = ui.painter();
    if let Some(fill) = flyout_fill(&t, resp.hovered()) {
        p.rect_filled(r, crate::theme::RADIUS, fill);
    }
    // indented under the section's title: a nested part of it, not a section of its own
    let tile = Rect::from_min_size(pos2(r.left() + 10.0, r.top() + 4.0), vec2(r.height() - 8.0, r.height() - 8.0));
    paint(p, tile.shrink(1.0), icon, t.text_dim);
    p.text(pos2(tile.right() + 6.0, r.center().y), Align2::LEFT_CENTER, title, t.font(SUB_TITLE), t.text_label);
    paint(
        p,
        Rect::from_center_size(pos2(r.right() - 10.0, r.center().y), vec2(12.0, 12.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronLeft },
        t.text_label,
    );
    resp
}

/// Height of the panels' small buttons and segmented controls.
pub const BUTTON_H: f32 = 20.0;

/// A small bordered text button (Auto, B&W, HDR…).
pub fn text_button(ui: &mut Ui, id: &str, label: &str, active: bool) -> Response {
    let label = if id.starts_with("labelSet-") { label } else { crate::i18n::tr(label) };
    let t = Tokens::get(ui.ctx());
    let font = t.semibold(11.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, t.text);
    let size = vec2((galley.size().x + 14.0).max(34.0), BUTTON_H);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, label));
    register(ui, format!("button:{id}"), r);
    let fill = if active {
        t.pressed
    } else if resp.hovered() {
        t.hover
    } else {
        t.button
    };
    let p = ui.painter();
    p.rect(r, CornerRadius::same(crate::theme::RADIUS), fill, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
    p.galley(r.center() - galley.size() / 2.0, galley, t.text);
    resp
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
                let (r, resp) = ui.allocate_exact_size(vec2(seg_w, BUTTON_H), Sense::click());
                crate::widgets::announce_selected(&resp, label.to_string(), active == Some(i));
                register(ui, format!("button:{id}-{key}"), r);
                let fill = if active == Some(i) {
                    t.pressed
                } else if resp.hovered() {
                    t.hover
                } else {
                    t.button
                };
                let p = ui.painter();
                p.rect(r, CornerRadius::same(crate::theme::RADIUS), fill, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, label, t.semibold(11.0), t.text);
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
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, active, tooltip));
    register(ui, format!("icon:{id}"), r);
    let p = ui.painter();
    let sq = Rect::from_center_size(r.center(), vec2(32.0f32.min(size.x), 32.0f32.min(size.y)));
    if active {
        p.rect_filled(sq, crate::theme::RADIUS, t.tool_active);
    } else if resp.hovered() && enabled {
        p.rect_filled(sq, crate::theme::RADIUS, t.hover.gamma_multiply(0.7));
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
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, text));
    register(ui, format!("dropdown:{id}"), r);
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
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, resp.enabled(), name.clone()));
}

/// The same for one that is on or off (a toggle, a selected row or swatch).
pub(crate) fn announce_selected(resp: &egui::Response, name: impl Into<String>, selected: bool) {
    let name = name.into();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, resp.enabled(), selected, name.clone()));
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

pub mod album_picker;
pub mod button;
pub mod date_picker;
#[cfg(test)]
#[path = "widgets/tests_numeric_entry.rs"]
mod numeric_entry_tests;
pub mod progress;
pub mod rating;
pub mod text_field;

#[cfg(test)]
#[path = "widgets/tests_slider_reset.rs"]
mod slider_reset_tests;

#[cfg(test)]
#[path = "widgets/tests_dense_row.rs"]
mod dense_row_tests;

fn focused_slider_key() -> egui::Id {
    egui::Id::unique("lightcraft-focused-slider")
}

/// The slider a click or drag armed for vertical wheel edits, if any.
fn wheel_slider_key() -> egui::Id {
    egui::Id::unique("lightcraft-wheel-slider")
}

/// Whether the widget with keyboard focus is a develop slider selected by a click, drag or Tab.
/// It owns only the arrow keys: every other shortcut still works. Wheel selection is separate.
pub fn slider_has_focus(ctx: &egui::Context) -> bool {
    let focused = ctx.memory(|m| m.focused());
    focused.is_some() && focused == ctx.data(|d| d.get_temp::<egui::Id>(focused_slider_key()))
}
