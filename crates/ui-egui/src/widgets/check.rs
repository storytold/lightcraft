//! The tick box and the round radio for the whole interface: an option that is on or off
//! ([`Check`]), and one choice among several ([`Radio`]). They replace egui's `ui.checkbox`,
//! `ui.radio` and `radio_value`, so every one looks the same in all four themes, shows the keyboard's
//! place with the accent ring, reads as a checkbox or a radio button to screen readers, and is
//! addressable as `check:{id}` or `radio:{id}`.
//!
//! ```ignore
//! if check(ui, "settings.gpu", &mut on, "Use the GPU for rendering").changed() { … }
//! Check::new("export.hdr", &mut hdr, "HDR output").enabled(can_hdr).tooltip("…").show(ui);
//! radio(ui, "merge.projection-cylindrical", &mut projection, Projection::Cylindrical, "Cylindrical");
//! ```
//!
//! A click, or Space or Return on the focused control, changes the value and marks the response
//! changed. A disabled one is drawn faded, ignores clicks and the keyboard, and shows its tooltip
//! as the reason. Labels and tooltips are English keys: they are translated here when drawn, so a
//! caller passes them untranslated. A label that has a value in it is a `tr_format!` result, already
//! translated: [`Check::translated`] draws it as it is.
//!
//! Every control's egui id and its `check:`/`radio:` automation name come from the `id` string and
//! the enclosing `ui`: two controls with the same `id` in one `ui` (a loop that leaves its index
//! out of the id) share both, so each must be given a distinct one.

use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2};

use crate::button::{Kind, State, look};
use crate::theme::Tokens;

/// The side of the box (the diameter of the radio), in points.
const BOX: f32 = 16.0;
/// Between the box and its label.
const GAP: f32 = 7.0;

/// The colours of the box in `state`: its fill, its outline, and the mark drawn on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxLook {
    pub fill: Color32,
    pub outline: Color32,
    pub mark: Color32,
}

/// What the box looks like when it is `on` or off, in `state`, with the theme `t`. On is the
/// accent with a white mark (the accent darkened as far as the mark needs, as a primary button's is);
/// off is the neutral button fill with a clear outline.
pub fn box_look(t: &Tokens, on: bool, state: State) -> BoxLook {
    if on {
        let lk = look(t, Kind::Primary, state);
        let fill = lk.fill.unwrap_or(t.accent);
        BoxLook { fill, outline: fill, mark: lk.content }
    } else {
        let lk = look(t, Kind::Secondary, state);
        let fade = |c: Color32| if state == State::Disabled { c.gamma_multiply(0.45) } else { c };
        BoxLook { fill: lk.fill.unwrap_or(t.button), outline: fade(t.text_label), mark: lk.content }
    }
}

/// An option that is on or off.
pub struct Check<'a> {
    id: &'a str,
    value: &'a mut bool,
    label: &'a str,
    enabled: bool,
    tooltip: Option<&'a str>,
    reason: Option<&'a str>,
    emphasis: Emphasis,
    translated: bool,
    accessible_name: Option<&'a str>,
    suffix: Option<&'a str>,
}

/// How loud the label reads among its neighbours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Emphasis {
    #[default]
    Normal,
    /// Bolder, in the text colour: what stands out in a list (a setting this mask changes).
    Strong,
    /// In the dimmed text colour: what is quiet in a list.
    Muted,
}

impl<'a> Check<'a> {
    /// A tick box for `value`, addressable as `check:{id}`.
    pub fn new(id: &'a str, value: &'a mut bool, label: &'a str) -> Self {
        Check {
            id,
            value,
            label,
            enabled: true,
            tooltip: None,
            reason: None,
            emphasis: Emphasis::Normal,
            translated: false,
            accessible_name: None,
            suffix: None,
        }
    }

    /// Off: drawn faded, and a click or a key does nothing.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Shown on hover, whether it is on or disabled (while disabled it reads as the reason why);
    /// [`Check::disabled_reason`] is for a reason that must not show while it is on.
    pub fn tooltip(mut self, text: &'a str) -> Self {
        self.tooltip = Some(text);
        self
    }

    /// Shown on hover only while it is disabled, as the reason why; an enabled one shows nothing.
    pub fn disabled_reason(mut self, text: &'a str) -> Self {
        self.reason = Some(text);
        self
    }

    /// The label is already in the chosen language (a `tr_format!` result with a value in it), so
    /// it is drawn as it is instead of being looked up as an English key.
    pub fn translated(mut self) -> Self {
        self.translated = true;
        self
    }

    /// What screen readers announce instead of the label, for a row whose visible text is
    /// completed by a neighbour (a count beside it). Already in the chosen language; the visible
    /// label is unchanged.
    pub fn accessible_name(mut self, name: &'a str) -> Self {
        self.accessible_name = Some(name);
        self
    }

    /// Dimmed text after the label inside the row (a count): the hit area takes it in, so a click on it is a
    /// click on the row. It is not announced by itself: give the row an [`Check::accessible_name`] that says it.
    pub fn suffix(mut self, text: &'a str) -> Self {
        self.suffix = Some(text);
        self
    }

    /// How loud the label reads.
    pub fn emphasis(mut self, emphasis: Emphasis) -> Self {
        self.emphasis = emphasis;
        self
    }

    /// Show it where the layout puts it; the response is `changed()` when it was toggled.
    pub fn show(self, ui: &mut Ui) -> Response {
        let on = *self.value;
        let (mut resp, toggled) = row(
            ui,
            Spec {
                widget: format!("check:{}", self.id),
                label: self.label,
                enabled: self.enabled,
                tooltip: self.tooltip,
                reason: self.reason,
                form: Form::Box,
                on,
                emphasis: self.emphasis,
                translated: self.translated,
                accessible_name: self.accessible_name,
                suffix: self.suffix,
            },
        );
        if toggled {
            *self.value = !on;
            resp.mark_changed();
        }
        resp
    }
}

/// A tick box addressable as `check:{id}`; the response is `changed()` when it was toggled.
pub fn check(ui: &mut Ui, id: &str, value: &mut bool, label: &str) -> Response {
    Check::new(id, value, label).show(ui)
}

/// One choice among several: selected when `*current == value`, and a click sets it.
pub struct Radio<'a, V> {
    id: &'a str,
    current: &'a mut V,
    value: V,
    label: &'a str,
    enabled: bool,
    tooltip: Option<&'a str>,
    translated: bool,
}

impl<'a, V: PartialEq> Radio<'a, V> {
    /// A radio for `value`, addressable as `radio:{id}`.
    pub fn new(id: &'a str, current: &'a mut V, value: V, label: &'a str) -> Self {
        Radio { id, current, value, label, enabled: true, tooltip: None, translated: false }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn tooltip(mut self, text: &'a str) -> Self {
        self.tooltip = Some(text);
        self
    }

    /// The label is already in the chosen language (a `tr_format!` result with a value in it), so
    /// it is drawn as it is instead of being looked up as an English key.
    pub fn translated(mut self) -> Self {
        self.translated = true;
        self
    }

    /// Show it where the layout puts it; the response is `changed()` when it picked another value.
    pub fn show(self, ui: &mut Ui) -> Response {
        let on = *self.current == self.value;
        let (mut resp, clicked) = row(
            ui,
            Spec {
                widget: format!("radio:{}", self.id),
                label: self.label,
                enabled: self.enabled,
                tooltip: self.tooltip,
                reason: None,
                form: Form::Round,
                on,
                emphasis: Emphasis::Normal,
                translated: self.translated,
                accessible_name: None,
                suffix: None,
            },
        );
        if clicked && !on {
            *self.current = self.value;
            resp.mark_changed();
        }
        resp
    }
}

/// A radio addressable as `radio:{id}`; the response is `changed()` when it picked `value`.
pub fn radio<V: PartialEq>(ui: &mut Ui, id: &str, current: &mut V, value: V, label: &str) -> Response {
    Radio::new(id, current, value, label).show(ui)
}

/// What one row is: its automation id, text, state and shape.
struct Spec<'a> {
    widget: String,
    label: &'a str,
    enabled: bool,
    tooltip: Option<&'a str>,
    reason: Option<&'a str>,
    form: Form,
    on: bool,
    emphasis: Emphasis,
    /// `label` is already translated.
    translated: bool,
    /// What assistive technology announces instead of `label`.
    accessible_name: Option<&'a str>,
    /// Dimmed text after the label, inside the hit area.
    suffix: Option<&'a str>,
}

#[derive(Clone, Copy, PartialEq)]
enum Form {
    Box,
    Round,
}

/// Lay out, interact with and draw one row (the box and its wrapped label); true when it was
/// activated by a click or the keyboard.
fn row(ui: &mut Ui, spec: Spec) -> (Response, bool) {
    let Spec { widget, label, enabled, tooltip, reason, form: shape, on, emphasis, translated, accessible_name, suffix } = spec;
    let kind = match shape {
        Form::Box => egui::Role::CheckBox,
        Form::Round => egui::Role::RadioButton,
    };
    let t = Tokens::get(ui.ctx());
    let label = if translated { label } else { crate::i18n::tr(label) };
    let enabled = enabled && ui.is_enabled();
    // (the dimmed text after the label has its width taken off what the label may wrap to)
    let suffix = suffix.map(|s| ui.painter().layout_no_wrap(s.to_string(), t.font(13.0), t.text_dim));
    let after = suffix.as_ref().map_or(0.0, |s| GAP + s.size().x);
    let wrap = (ui.available_width() - BOX - GAP - after).max(60.0);
    let (font, base) = match emphasis {
        Emphasis::Normal => (t.font(13.0), t.text),
        Emphasis::Strong => (t.semibold(13.0), t.text),
        Emphasis::Muted => (t.font(13.0), t.text_dim),
    };
    let galley = ui.painter().layout(label.to_string(), font, base, wrap);
    let label_width = galley.size().x;
    let size = vec2(BOX + GAP + label_width + after, galley.size().y.max(ui.spacing().interact_size.y));
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let id = ui.scope_id().with(("lc-check", widget.as_str()));
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let activated = resp.clicked();
    // what the row is once this activation is applied: the caller updates its value only after `row` returns,
    // and a screen reader must not hear the previous state in the frame that changed it (a radio stays on)
    let announced = match shape {
        Form::Box => on != activated,
        Form::Round => on || activated,
    };
    resp.widget_info(|| WidgetInfo::selected(kind, enabled, announced, accessible_name.unwrap_or(label)));
    crate::widgets::register(ui, widget, rect);

    let state = if !enabled {
        State::Disabled
    } else if resp.is_pointer_button_down_on() {
        State::Pressed
    } else if resp.hovered() {
        State::Hovered
    } else {
        State::Idle
    };
    let lk = box_look(&t, on, state);
    let p = ui.painter_at(rect.expand(4.0));
    let b = Rect::from_min_size(pos2(rect.min.x, rect.center().y - BOX / 2.0), vec2(BOX, BOX));
    match shape {
        Form::Box => {
            p.rect_filled(b, 3.0, lk.fill);
            p.rect_stroke(b, 3.0, Stroke::new(1.0, lk.outline), StrokeKind::Inside);
            if on {
                let at = |x: f32, y: f32| -> Pos2 { pos2(b.min.x + x * BOX, b.min.y + y * BOX) };
                p.add(Shape::line(vec![at(0.24, 0.52), at(0.43, 0.70), at(0.77, 0.31)], Stroke::new(2.0, lk.mark)));
            }
        }
        Form::Round => {
            p.circle_filled(b.center(), BOX / 2.0, lk.fill);
            p.circle_stroke(b.center(), BOX / 2.0 - 0.5, Stroke::new(1.0, lk.outline));
            if on {
                p.circle_filled(b.center(), 3.0, lk.mark);
            }
        }
    }
    if resp.has_focus() {
        let ring = crate::widgets::focus_stroke(&t);
        match shape {
            Form::Box => {
                p.rect_stroke(b.expand(2.0), 5.0, ring, StrokeKind::Outside);
            }
            Form::Round => {
                p.circle_stroke(b.center(), BOX / 2.0 + 2.0, ring);
            }
        }
    }
    let text_color = if enabled { base } else { t.text_disabled };
    p.galley_with_override_text_color(pos2(rect.min.x + BOX + GAP, rect.center().y - galley.size().y / 2.0), galley, text_color);
    if let Some(suffix) = suffix {
        let at = pos2(rect.min.x + BOX + GAP + label_width + GAP, rect.center().y - suffix.size().y / 2.0);
        p.galley_with_override_text_color(at, suffix, if enabled { t.text_dim } else { t.text_disabled });
    }

    // `resp.enabled()` is false only when the whole ui is disabled around this control; one turned
    // off with `.enabled(false)` sits in an enabled ui. egui's disabled hover text shows only in the
    // first case, so the reason is shown with the plain hover text in the second.
    let tooltip = if enabled { tooltip } else { reason.or(tooltip) };
    let resp = match tooltip {
        Some(tip) if resp.enabled() => resp.on_hover_text(crate::i18n::tr(tip)),
        Some(tip) => resp.on_disabled_hover_text(crate::i18n::tr(tip)),
        None => resp,
    };
    (resp, activated)
}
