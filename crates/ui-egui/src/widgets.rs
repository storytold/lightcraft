//! Lightroom-style widgets: sliders with gradient tracks and hollow thumbs, section headers,
//! flyout rows, bordered buttons. Every interactive widget registers an automation id + rect
//! ([`register`]) so the control channel can find and click it by name.

use egui::{Align2, Color32, CornerRadius, Rect, Response, Stroke, StrokeKind, Ui, pos2, vec2};
use lightcraft_develop::{ControlSpec, Track};

use crate::icons::{Icon, paint};
use crate::theme::Tokens;

/// Whether this run draws nothing the user can reach: a sizing or measuring run registers no automation name and
/// takes no pick.
fn measuring(ui: &Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

/// A framework dropdown (`Select`) named `label` over `entries`, showing `current` (or `placeholder` while `current` is
/// none of them), registered as `name` with its real response. `width` fixes the field's width; without it the field
/// is as wide as its widest entry. Returns the entry picked in this pass when it differs from `current`.
pub(crate) fn select<T: craft::behavior::Value>(
    ui: &mut Ui,
    name: &str,
    label: &str,
    current: T,
    entries: &[craft::ui::contract::Entry<T>],
    placeholder: Option<String>,
    width: Option<f32>,
) -> Option<T> {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    use craft::ui::pickers::Select;
    let mut select = Select::new(label.to_owned(), Bind::given(current.clone()), entries).id_salt(("lc-select", name));
    if let Some(text) = placeholder {
        select = select.placeholder(text);
    }
    let shown = match width {
        Some(width) => {
            let (_, rect) = ui.allocate_space(vec2(width.min(ui.available_width()).max(1.0), 24.0));
            select.show_at(ui, rect)
        }
        None => select.show(ui),
    };
    if measuring(ui) {
        return None;
    }
    register_response(name, shown.response.rect, &shown.response);
    shown.events.committed().cloned().filter(|next| ui.is_enabled() && *next != current)
}

/// A framework dropdown whose list is a menu of rows the caller fills (choices with automation names of their own,
/// actions after a divider): a field showing `face`, named `label`, registered as `name` with its real response;
/// `rows` runs while the menu is open. `width` fixes the field's width.
pub(crate) fn dropdown_menu(ui: &mut Ui, name: &str, label: &str, face: &str, width: Option<f32>, mut rows: impl FnMut(&mut Ui)) -> Response {
    use craft::ui::buttons::Button;
    use craft::ui::contract::{Props as _, Show as _, ShowContainer as _, Trigger};
    use craft::ui::menu::Menu;
    let button = Button::new(label.to_owned())
        .painted(face.to_owned())
        .trailing_icon(craft::ui::controls::disclosure::DisclosureGlyph::dropdown())
        .id_salt(("lc-dropdown", name));
    let shown = match width {
        Some(width) => {
            let (_, rect) = ui.allocate_space(vec2(width.min(ui.available_width()).max(1.0), 24.0));
            button.show_at(ui, rect)
        }
        None => button.show(ui),
    };
    if !measuring(ui) {
        register_response(name, shown.response.rect, &shown.response);
    }
    Menu::new(label.to_owned()).id_salt(("lc-dropdown-menu", name)).values().opened_by(&shown.response, Trigger::Click).show(ui, |ui, _| rows(ui));
    shown.response
}

/// One row of a [`dropdown_menu`], checked when it is the current choice, with `tooltip` on hover, registered as
/// `name` when it has one. True when it ran in a real run.
pub(crate) fn menu_row(
    ui: &mut Ui,
    salt: impl egui::AsIdSalt,
    name: Option<&str>,
    label: &str,
    tooltip: Option<&str>,
    checked: bool,
    enabled: bool,
) -> bool {
    use craft::ui::contract::{Props as _, Show as _};
    use craft::ui::menu::MenuItem;
    let mut item = MenuItem::new(label.to_owned()).id_salt(salt).entry_checked(checked).enabled(enabled);
    if let Some(tooltip) = tooltip {
        item = item.tooltip(tooltip.to_owned());
    }
    let row = item.show(ui);
    let real = !measuring(ui);
    if real && let Some(name) = name {
        register_response(name, row.response.rect, &row.response);
    }
    real && enabled && row.ran
}

/// A divider between groups of [`dropdown_menu`] rows.
pub(crate) fn menu_divider(ui: &mut Ui) {
    use craft::ui::contract::Show as _;
    craft::ui::display::Divider::horizontal().show(ui);
}

/// A button with a vector arrow beside its text, on the framework pipeline; the arrow takes the room an egui button
/// gives an icon atom (16 points and the icon gap), and the face follows the ui's button visuals.
pub fn arrow_button(ui: &mut Ui, text: &str, icon: Icon, leading: bool) -> HitAreaOutput {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::TextStyle::Button.resolve(ui.style()), Color32::PLACEHOLDER);
    let padding = ui.spacing().button_padding;
    let gap = ui.spacing().icon_spacing;
    let content = vec2(galley.size().x + gap + 16.0, galley.size().y.max(16.0));
    let size = (content + padding * 2.0).max(ui.spacing().interact_size);
    let (id, rect) = ui.allocate_space(size);
    let resp = HitArea::new(text.to_owned(), id).show_at(ui, rect);
    if ui.is_rect_visible(rect) {
        let visuals = *ui.style().interact(&resp);
        let painter = ui.painter();
        painter.rect(rect.expand(visuals.expansion), visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, StrokeKind::Inside);
        // the content sits centred as one run: the text, the gap, the arrow (or the arrow first)
        let left = rect.center().x - content.x / 2.0;
        let (text_x, arrow_x) = if leading { (left + 16.0 + gap, left) } else { (left, left + galley.size().x + gap) };
        let centre_y = rect.center().y;
        painter.galley_with_override_text_color(pos2(text_x, centre_y - galley.size().y / 2.0), galley, visuals.fg_stroke.color);
        paint(painter, Rect::from_min_size(pos2(arrow_x, centre_y - 8.0), vec2(16.0, 16.0)), icon, visuals.fg_stroke.color);
    }
    resp
}

/// A context menu on `response`, as `Response::context_menu` gives, that a right-click moves
/// (#1877). egui closes a menu on any click once it was open on the previous frame, and judges
/// that by the menu's id: a right-click elsewhere on the same widget while its menu is open
/// reopened the menu at the pointer and closed it again on the same frame, a one-frame flash.
/// The click that opens (or moves) the menu is not the one that closes it.
///
/// While photos are carried (dragged from the grid, the primary button still held) a right-click
/// opens nothing here: a menu is not what the person is doing with the pointer.
pub fn context_menu(response: &Response, add_contents: impl FnOnce(&mut Ui)) -> Option<egui::InnerResponse<()>> {
    if response.secondary_clicked() && carrying(&response.ctx) {
        return None;
    }
    egui::Popup::context_menu(response).close_behavior(context_menu_close(response, egui::PopupCloseBehavior::CloseOnClick)).show(add_contents)
}

/// Record whether photos are being carried as the frame starts, for [`context_menu`] to read. A
/// drag left over without its button (full screen skips the code that ends it) is not carrying.
pub fn set_carrying(ctx: &egui::Context, carrying: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("lc-carrying"), carrying));
}

pub(crate) fn carrying(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("lc-carrying"))).unwrap_or(false)
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
    register_with_tooltip(ui, id, rect, None);
}

/// Adds guidance to the existing hover-only automation probe; never creates a second hit owner.
pub(crate) fn register_with_tooltip(ui: &egui::Ui, id: impl Into<String>, rect: Rect, hint: Option<&str>) {
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
        if let Some(hint) = hint {
            craft::ui::contract::tooltip::attach(&response, &hint.to_owned().into());
        }
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

/// A click pressed and released within this one pass on `response` (a host that delivers a whole click in one batch).
/// The framework's own gesture needs to see the press before the release; egui still reports such a click.
/// An assistive technology's Click on the response's own node counts too.
pub(crate) fn same_pass_click(response: &egui::Response) -> bool {
    let ctx = &response.ctx;
    if !response.enabled() {
        return false;
    }
    let at = |pressed: bool| {
        ctx.input(|i| {
            i.events.iter().find_map(|event| match event {
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: p, .. } if *p == pressed => Some(*pos),
                _ => None,
            })
        })
    };
    let pointer = match (at(true), at(false)) {
        (Some(down), Some(up)) => {
            response.interact_rect.contains(down) && response.interact_rect.contains(up) && ctx.layer_id_at(down) == Some(response.layer_id)
        }
        _ => false,
    };
    pointer || ctx.input(|i| i.has_accesskit_action_request(response.id, egui::accesskit::Action::Click))
}

/// Adds `response` (its id, layer and clipped rect) as a second hit owner of the widget registered as `id`, without a
/// second rectangle: for a widget whose own response and one of its parts both name it (a text field and its text).
pub(crate) fn add_hit(id: &str, response: &egui::Response) {
    response.ctx.data_mut(|d| {
        let registry = d.get_temp_mut_or_default::<Registry>(egui::Id::unique("lc-registry"));
        registry.hits.push((id.to_owned(), WidgetHit { id: response.id, layer_id: response.layer_id, interact_rect: response.interact_rect }));
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
        .corner_radius(6.0)
        .inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 9 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (_, ir) = ui.allocate_space(vec2(16.0, 16.0));
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

/// Native keyboard nudge in model units, before any value-dependent rounding or clamping.
pub(crate) fn nudge_unit(spec: &ControlSpec) -> f64 {
    let step = spec.step.max(1e-9);
    if spec.id == "wb.temp" { 50.0 } else { (((spec.max - spec.min) / 200.0 / step).round().max(1.0)) * step }
}

/// `value` moved by `steps` keyboard nudges: one nudge is about 1/200 of the range, a whole
/// number of the control's steps (exposure 0.05, most sliders 1, temperature 50 K).
pub fn nudged(spec: &ControlSpec, value: f64, steps: f64) -> f64 {
    let step = spec.step.max(1e-9);
    let unit = nudge_unit(spec);
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

/// Collapsible section header ("› Light"). Returns the header's output (a click toggles) and, when its eye was
/// clicked, the new state of the section's switch.
pub fn section_header(ui: &mut Ui, id: &str, title: &str, open: bool, enabled: Option<bool>) -> (HitAreaOutput, Option<bool>) {
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (_, r) = ui.allocate_space(vec2(w, 52.0));
    let resp = HitArea::new(title.to_string(), ui.scope_id().with(("section", id))).widget(format!("section:{id}")).expanded(open).show_at(ui, r);
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
        let er = HitArea::new(crate::i18n::tr("Show / hide").to_string(), ui.scope_id().with(("eye", id)))
            .widget(format!("sectionEye:{id}"))
            .selected(on)
            .show_at(ui, eye);
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
    let (_, r) = ui.allocate_space(vec2(w, 1.0));
    ui.painter().rect_filled(r, 0.0, t.divider);
}

/// A flyout row inside a section (Curve, Color Mixer, Color Grading…): inset dark box with icon tile.
pub fn flyout_row(ui: &mut Ui, id: &str, title: &str, icon: Icon, open: bool) -> HitAreaOutput {
    let title = crate::i18n::tr(title);
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (_, outer) = ui.allocate_space(vec2(w, 40.0));
    let r = Rect::from_min_max(pos2(outer.left() + 16.0, outer.top() + 4.0), pos2(outer.right() - 16.0, outer.bottom() - 4.0));
    let resp = HitArea::new(title.to_string(), ui.scope_id().with(("flyout", id))).widget(format!("flyout:{id}")).expanded(open).show_at(ui, r);
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

/// A small bordered text button (Auto, B&W, HDR…) on the SDK interaction pipeline.
/// Its output keeps framework activation through tooltip chains; a menu it opens is a framework `Menu`
/// opened by its response with `Trigger::Click`.
pub fn text_button(ui: &mut Ui, id: &str, label: &str, active: bool) -> TextButtonOutput {
    text_button::show(ui, id, label, active)
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
                let (_, r) = ui.allocate_space(vec2(seg_w, 24.0));
                let resp = HitArea::new(label.to_string(), ui.scope_id().with(("segment", id, *key)))
                    .widget(format!("button:{id}-{key}"))
                    .selected(active == Some(i))
                    .show_at(ui, r);
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

/// Splits a tooltip that ends in its key hint ("Edit (E)", "Import Photos… (⌘⇧I)") into the label
/// and the hint: `("Edit", Some("E"))`. A tooltip without one is all label. The Japanese and
/// Traditional Chinese catalogs write the hint in full-width brackets without a space ("編集（E）").
fn split_key_hint(tip: &str) -> (&str, Option<&str>) {
    let split =
        tip.strip_suffix(')').and_then(|body| body.rsplit_once(" (")).or_else(|| tip.strip_suffix('）').and_then(|body| body.rsplit_once('（')));
    let Some((label, hint)) = split else { return (tip, None) };
    let label = label.trim_end();
    if label.is_empty() || hint.is_empty() { (tip, None) } else { (label, Some(hint)) }
}

/// An icon-only SDK button. `active` keeps the selected tool-strip face.
pub fn icon_button(ui: &mut Ui, id: &str, icon: Icon, size: egui::Vec2, active: bool, enabled: bool, tooltip: &str) -> IconButtonOutput {
    icon_button::show(ui, id, icon, size, active, enabled, tooltip)
}

/// A borderless dropdown button: `text` followed by a painted chevron (no font glyph needed).
pub fn dropdown(ui: &mut Ui, id: &str, text: &str, font: egui::FontId, color: Color32) -> HitAreaOutput {
    dropdown_within(ui, id, text, font, color, f32::INFINITY)
}

/// [`dropdown`] at most `max` wide, chevron included: a longer `text` is cut short with …, and
/// shown in full on hover.
pub fn dropdown_within(ui: &mut Ui, id: &str, text: &str, font: egui::FontId, color: Color32, max: f32) -> HitAreaOutput {
    let t = Tokens::get(ui.ctx());
    let galley = one_line(ui.painter(), text, font, color, max - 20.0);
    let elided = galley.elided;
    let size = vec2(galley.size().x + 20.0, galley.size().y.max(20.0));
    let (_, r) = ui.allocate_space(size);
    let resp = HitArea::new(text.to_owned(), ui.scope_id().with(("dropdown", id)))
        .widget(format!("dropdown:{id}"))
        .role(craft::ui::contract::Role::COMBO_BOX)
        .show_at(ui, r);
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
    fn a_trailing_key_hint_leaves_the_name() {
        assert_eq!(split_key_hint("Edit (E)"), ("Edit", Some("E")));
        assert_eq!(split_key_hint("Presets (Shift+P)"), ("Presets", Some("Shift+P")));
        assert_eq!(split_key_hint("Import Photos… (⌘⇧I)"), ("Import Photos…", Some("⌘⇧I")));
        assert_eq!(split_key_hint("Filmstrip (/)"), ("Filmstrip", Some("/")));
        assert_eq!(split_key_hint("Square Grid (G toggles)"), ("Square Grid", Some("G toggles")));
        // nothing to split: no hint, an empty hint, no label before it, the hint not at the end
        for tip in ["Export", "Edit ()", "(E)", " (E)", "Edit (E) now", "Edit(E)", ""] {
            assert_eq!(split_key_hint(tip), (tip, None), "{tip:?}");
        }
    }

    /// Every language's tooltip for an icon-only button splits into the bare label and its hint,
    /// whatever brackets the catalog writes it in (Japanese and Traditional Chinese use full-width ones).
    #[test]
    fn every_language_names_an_icon_button_by_its_label() {
        assert_eq!(split_key_hint("編集（E）"), ("編集", Some("E")));
        for locale in crate::i18n::Locale::ALL {
            for tip in ["Edit (E)", "Presets (Shift+P)", "Photo Grid (G)", "Filmstrip (/)"] {
                let translated = locale.tr(tip);
                let (label, hint) = split_key_hint(translated);
                assert!(hint.is_some_and(|h| !h.is_empty()), "{locale:?}: {translated:?} keeps its hint in the name");
                assert!(!label.contains(['(', ')', '（', '）']), "{locale:?}: {label:?} is the bare label");
            }
        }
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

    /// Shift-drag moves a tenth of the pointer's way along the track, also on the Kelvin track (where a pixel of
    /// fine drag near the centre is about 4 K): a pointer creeping a pixel a frame still adds up to steps.
    #[test]
    fn a_slow_shift_drag_adds_up() {
        // the framework's runtime senses and routes the slider's input
        let ctx = crate::headless::HeadlessView::new().ctx;
        let temp = lightcraft_develop::controls::find("wb.temp").unwrap();
        let mut value = 7000.0;
        let frame = |time: f64, events: Vec<egui::Event>, value: f64| {
            let input = crate::headless::HeadlessView::raw_input(vec2(320.0, 120.0), 1.0, time, events);
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
        // the framework's runtime builds the controls' accessibility nodes
        let ctx = crate::headless::HeadlessView::new().ctx;
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

/// Names the part of the window `ui` draws for screen readers: a toolbar, or a landmark (VoiceOver's
/// rotor lists the landmarks and jumps to one). Set on the node the `Ui` already has, so nothing is
/// laid out differently. The node gets the `Ui`'s area as its bounds, so a screen reader's cursor
/// lands on the part and not on an empty spot.
pub(crate) fn announce_region(ui: &Ui, role: egui::accesskit::Role, name: impl Into<String>) {
    let name = name.into();
    let r = ui.max_rect();
    ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
        node.set_role(role);
        node.set_label(name);
        node.set_bounds(egui::accesskit::Rect { x0: f64::from(r.min.x), y0: f64::from(r.min.y), x1: f64::from(r.max.x), y1: f64::from(r.max.y) });
    });
}

/// A hand-drawn clickable area (an icon, a tile, a row) announced as a button named `name`.
pub(crate) fn announce(resp: &egui::Response, name: impl Into<String>) {
    let name = name.into();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, resp.enabled(), name.clone()));
}

/// What a window with its own title and buttons is, to a screen reader.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modal {
    /// A dialog with a task (Export, Settings, a name prompt) that stops the rest of the application.
    Dialog,
    /// One that interrupts to say something that needs an answer (the quit prompt, a library that
    /// couldn't be opened) and stops the rest of the application: an alert dialog (AT-SPI and
    /// Windows tell it from a dialog; macOS reads both alike).
    Alert,
    /// A dialog that leaves the rest of the application reachable (the update check's answer).
    Window,
}

/// Announces the window `response` drew as a dialog (an alert dialog for [`Modal::Alert`]), and for
/// [`Modal::Dialog`] and [`Modal::Alert`] as modal, instead of as a group that carries the title:
/// the platform's accessibility layer then sees a dialog and that the rest of the application is
/// blocked while it is open.
pub(crate) fn announce_dialog(ctx: &egui::Context, response: &Response, kind: Modal) {
    ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(if kind == Modal::Alert { egui::accesskit::Role::AlertDialog } else { egui::accesskit::Role::Dialog });
        if kind != Modal::Window {
            node.set_modal();
        }
    });
}

/// Gives the dialog `response` drew the words it says as its accessible description, for an alert
/// whose content (the task names, the error) is drawn as separate labels: a screen reader then
/// has what the alert is about when it announces the alert, not only its title.
pub(crate) fn describe_dialog(ctx: &egui::Context, response: &Response, text: &str) {
    ctx.accesskit_node_builder(response.id, |node| node.set_description(text));
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
mod hit_area;
#[cfg(test)]
#[path = "widgets/tests_numeric_entry.rs"]
mod numeric_entry_tests;
pub mod progress;
pub mod rating;
mod icon_button;
mod slider;
pub(crate) use slider::{Row as SliderRow, field as slider_field, slider_row, slider_salted};
pub use slider::{SliderOut, slider};
pub use hit_area::{HitArea, HitAreaOutput};
pub use icon_button::IconButtonOutput;
mod text_button;
pub mod text_field;
pub use text_button::TextButtonOutput;

#[cfg(test)]
#[path = "widgets/tests_slider_reset.rs"]
mod slider_reset_tests;

fn focused_slider_key() -> egui::Id {
    egui::Id::unique("lightcraft-focused-slider")
}

/// Adopt the actual egui focus root of a framework slider track. Numeric text parts do not
/// call this: they keep the full text-edit shortcut barrier.
pub(crate) fn note_slider_focus(ctx: &egui::Context, root: egui::Id) {
    ctx.data_mut(|data| data.insert_temp(focused_slider_key(), root));
}

/// Whether the widget with keyboard focus is a develop slider selected by a click, drag or Tab.
/// It owns only the arrow keys: every other shortcut still works. Wheel selection is separate.
pub fn slider_has_focus(ctx: &egui::Context) -> bool {
    let focused = ctx.memory(|m| m.focused());
    focused.is_some() && focused == ctx.data(|d| d.get_temp::<egui::Id>(focused_slider_key()))
}
