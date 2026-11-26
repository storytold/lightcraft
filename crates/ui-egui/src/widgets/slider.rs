//! LightCraft's develop slider row on the framework's `SliderField`: the caption and the typed value on one line
//! over the track, as in Lightroom. The framework owns the gestures: a drag or click on the track, Shift for a
//! fine drag, a click on the value to type one (Return applies, Escape restores), a drag on the value or the
//! caption to scrub it, a double-click on the caption or the thumb to reset, the vertical arrows of a hovered or
//! focused row (Shift: five nudges), sideways and armed wheel nudges, and AccessKit increment and decrement.
//! This module keeps LightCraft's own words: the control's steps and nudges, its typed-value grammar
//! ([`super::typed_value`]), the Kelvin temperature track, the coloured tracks and the automation names
//! (`slider:`, `sliderValue:`, `sliderField:`, `sliderLabel:`, `sliderThumb:`).

use craft::behavior::{EditEvent, EditEvents, Mixed, NumberCodec, Proposal, Reader};
use craft::ui::contract::{Bind, Props as _, Show as _, Sizing};
use craft::ui::form::{SliderField, SliderFieldOutput};
use craft::ui::runtime::env::{WheelScope, with_env};
use craft::ui::runtime::keys::{ScalarKeyPolicy, ScalarKeyTrigger};
use craft::ui::value_drag::{DragModifier, DragModifiers, SliderScale, TrackFace};
use egui::{Rect, Ui};
use lightcraft_develop::{ControlSpec, Track};

/// What a slider did this pass, as one summary of the framework's edit stream.
#[derive(Default, Debug, Clone, Copy)]
pub struct SliderOut {
    /// The value to show and store now: a drag's or scrub's live value, a typed or stepped one, or the value an
    /// abandoned drag restores.
    pub value: Option<f64>,
    /// A gesture began: an undo step opens (a whole change in one go both begins and stops).
    pub drag_started: bool,
    /// The gesture ended with its value: the undo step closes.
    pub drag_stopped: bool,
    /// The gesture was abandoned (Escape during a drag, the row going away): what it changed goes back.
    pub cancelled: bool,
}

/// The instruction a slider's value gives on hover and to screen readers.
pub(crate) const VALUE_HINT: &str = "Click to type a value, drag to change it";

/// One slider row: its control, the value shown, and what tells its state apart.
pub(crate) struct Row<'a> {
    pub spec: &'a ControlSpec,
    pub value: f64,
    pub enabled: bool,
    /// A caption in place of the control's own label ("Straighten" for the crop angle).
    pub label: Option<&'a str>,
    /// Keeps a typed draft or a gesture with what it edits (the photo): a new owner starts afresh.
    pub salt: egui::Id,
    /// The key the row's parts are recorded under (`inspect::recorded`), for a panel whose callers read them;
    /// by default one derived from the row's id.
    pub record: Option<egui::Id>,
}

/// What [`field`] showed.
pub(crate) struct Shown {
    pub out: SliderFieldOutput,
    /// The track's component id, for adapters that route keys to a row themselves.
    pub track: Option<egui::Id>,
}

/// A slider row on a develop control: `slider:{id}` etc. name its parts for automation.
pub fn slider(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, label_override: Option<&str>) -> SliderOut {
    slider_salted(ui, spec, value, enabled, label_override, egui::Id::NULL)
}

/// [`slider`], its state kept apart per `salt` (the photo it edits).
pub(crate) fn slider_salted(ui: &mut Ui, spec: &ControlSpec, value: f64, enabled: bool, label: Option<&str>, salt: egui::Id) -> SliderOut {
    slider_row(ui, &Row { spec, value, enabled, label, salt, record: None })
}

/// [`slider`] for a full [`Row`].
pub(crate) fn slider_row(ui: &mut Ui, row: &Row<'_>) -> SliderOut {
    let shown = field(ui, row);
    if !real(ui) {
        return SliderOut::default();
    }
    summarize(&shown.out.events, row.value)
}

/// Whether this pass acts: never a sizing pass or the framework's measuring pass.
fn real(ui: &Ui) -> bool {
    !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx())
}

/// The pass's edit events as a [`SliderOut`]. A `Set` (a key, a typed value, a reset, a click on the track) is a
/// whole change: it begins and stops at once, so it is one undo step.
pub(crate) fn summarize(events: &EditEvents<f64>, current: f64) -> SliderOut {
    let mut out = SliderOut::default();
    let mut shown = current;
    let mut land = |out: &mut SliderOut, value: f64| {
        if value.is_finite() && (value - shown).abs() > 1e-12 {
            out.value = Some(value);
            shown = value;
        }
    };
    for event in events {
        match event {
            EditEvent::Begin { .. } => out.drag_started = true,
            EditEvent::Update { value, .. } => land(&mut out, *value),
            EditEvent::Commit { after, .. } => {
                land(&mut out, *after);
                out.drag_stopped = true;
            }
            EditEvent::Set { after, .. } => {
                land(&mut out, *after);
                out.drag_started = true;
                out.drag_stopped = true;
            }
            EditEvent::Cancel { before, .. } => {
                if let Some(Mixed::Same(before)) = before {
                    land(&mut out, *before);
                }
                out.cancelled = true;
            }
            _ => {}
        }
    }
    out
}

/// The framework row for `row`, with LightCraft's steps, grammar, track and automation names.
pub(crate) fn field(ui: &mut Ui, row: &Row<'_>) -> Shown {
    let spec = row.spec;
    let number_spec = *spec;
    let name = crate::i18n::tr(row.label.unwrap_or(spec.label)).to_owned();
    let nudge = super::nudge_unit(spec);
    // Lightroom's keys: a nudge is about 1/200 of the range on the control's steps, Shift five, on the hovered or
    // the focused row; a wheel notch is a nudge too.
    let policy = ScalarKeyPolicy::new(nudge).model_step(spec.step).shift_factor(5.0).trigger(ScalarKeyTrigger::HoveredOrFocused);
    let id = ui.scope_id().with(("lc-slider", spec.id, row.salt));
    let enabled = row.enabled;
    // A scrub of the value moves it as far as the same drag on the thumb: the range over the track's value span
    // (as the last pass laid it out; before that, the row's width less its margins and a thumb).
    let span = ui.data(|data| data.get_temp::<f32>(id.with("span"))).unwrap_or(ui.available_width() - 46.0 - 14.0).max(1.0);
    let scrub_rate = (spec.max - spec.min) / f64::from(span);
    let (out, record) = craft::ui::runtime::inspect::record(ui, row.record.unwrap_or_else(|| id.with("record")), |ui| {
        // A vertical wheel edits only a row a press selected; a sideways swipe edits the row under the pointer.
        with_env(
            ui,
            |set| {
                set.wheel = Some(WheelScope::Directional);
                // Lightroom's dense rows: the caption line and the track at the small control size
                set.size = Some(craft::theme::ControlSize::Sm);
            },
            |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 0 })
                    .show(ui, |ui| {
                        let face = (enabled && ui.is_enabled()).then(|| track_face(&spec.track)).flatten();
                        let mut field = SliderField::new(name.clone(), Bind::given(row.value).reset(spec.default), spec.min..=spec.max)
                            .id(id)
                            .step(spec.step)
                            .wheel_step(nudge)
                            .key_policy(policy)
                            .pointer_model_step(spec.step)
                            .scrub_step(scrub_rate)
                            // Shift drags a tenth as far, as Lightroom's sliders do
                            .pointer_modifiers(DragModifiers::new(Some(DragModifier::Shift), None))
                            // the product's whole-value grammar: +12 is absolute, either decimal separator, K or %
                            .number_codec(
                                NumberCodec::new(Reader::new(), usize::from(spec.decimals))
                                    .parse_with(move |text| super::typed_value(&number_spec, text)),
                            )
                            .scale(if spec.id == "wb.temp" {
                                SliderScale::Custom { position: kelvin_position, value_at: kelvin_value_at }
                            } else {
                                SliderScale::Linear
                            })
                            .fill(false)
                            .tooltip(crate::i18n::tr(VALUE_HINT).to_owned())
                            .enabled(enabled)
                            .sizing(Sizing::FillWidth);
                        if let Some(face) = face {
                            field = field.track_face(face);
                        }
                        // The row takes the room it measures at the panel's width and is shown there. Measured in the
                        // flow of the run it is in, so the body's measuring run and its real run record and check the same
                        // leaf (a measure wrapped in a measuring run of its own was a leaf the real run never asked).
                        let width = ui.available_width().max(1.0);
                        let size = field.measure_in(ui, Proposal::Width(width));
                        let (_, room) = ui.allocate_space(egui::vec2(width, size.y));
                        field.show_at(ui, room)
                    })
                    .inner
            },
        )
    });
    let mut track = None;
    if let Some(record) = record {
        track = publish(ui, row, id, &name, &out, &record);
    }
    Shown { out, track }
}

/// Names the row's real parts for automation, says how to reset it, keeps the shortcut gate's slider focus and
/// gives the value field back after a typed edit ends. Returns the track's id.
fn publish(
    ui: &Ui,
    row: &Row<'_>,
    id: egui::Id,
    name: &str,
    out: &SliderFieldOutput,
    record: &craft::ui::runtime::inspect::Recorded,
) -> Option<egui::Id> {
    let spec = row.spec;
    let ctx = ui.ctx();
    let focused = craft::ui::runtime::focus::focused_rec(ctx);
    let reset_hint = crate::i18n::tr_format!("{name} — double-click to reset it", name = name);
    let mut track = None;
    let mut number_focus = None;
    let ended = out.events.iter().any(|event| matches!(event, EditEvent::Set { .. } | EditEvent::Commit { .. } | EditEvent::Cancel { .. }));
    // Return or Escape gives the keyboard back too, as LightCraft's fields did (and the dialogs' number fields do):
    // the field has applied its commit or put its text back first, so nothing is lost.
    // (The framework's key stage takes the keys from egui's events; it still says which keys are down.)
    let keyed = key_went_down(ctx, "Enter", egui::Id::unique(("lc-slider-enter-down", spec.id)).with(row.salt))
        | key_went_down(ctx, "Escape", egui::Id::unique(("lc-slider-escape-down", spec.id)).with(row.salt));
    // On the opening release's pass the key arrives before the field has the focus: it gives the keyboard back on
    // the next pass, once the focus has landed.
    let pending_key = egui::Id::unique(("lc-slider-blur-pending", spec.id)).with(row.salt);
    let pending = ctx.data_mut(|data| data.remove_temp::<bool>(pending_key)).unwrap_or(false);
    let giving_back = ended || keyed || pending;
    for (index, element) in record.elements.iter().enumerate() {
        let Some(facts) = &element.facts else { continue };
        // The field's text menu is recorded inside this row's overlay. Its real row takes the press; this only
        // keeps the product's locator.
        if element.component == "MenuItem" {
            let paste = craft::ui::runtime::commands::resolve(ctx, craft::ui::runtime::commands::EditCommands::Paste);
            if element.part.is_none()
                && facts.name.as_deref() == Some(paste.label.as_ref())
                && let Some(response) = ctx.read_response(element.id)
            {
                super::register_response(format!("sliderField:{}:paste", spec.id), element.rect, &response);
            }
            continue;
        }
        let prefix = match facts.role.as_str() {
            "Slider" => "slider",
            "SpinButton" => "sliderValue",
            "Label" => "sliderLabel",
            _ => continue,
        };
        let rect = record.stop_rect(index).unwrap_or(element.rect);
        // The recorded element is the actual framework owner, also for an arena child with no egui response of its
        // own: the alias names it, it is no second widget.
        let response = ctx.read_response(element.id).unwrap_or_else(|| {
            let mut response = out.response.clone();
            response.id = element.id;
            response.rect = rect;
            response.interact_rect = rect.intersect(ui.clip_rect());
            response
        });
        let focus = focused.as_ref().filter(|focused| focused.stop.id == element.id);
        let mut target = rect;
        match prefix {
            "slider" => {
                track = Some(element.id);
                let thumb = record
                    .hits
                    .iter()
                    .find(|hit| hit.owner == element.id && hit.part.is_some_and(|part| part.key == craft::ui::value_drag::Slider::THUMB));
                if let Some(thumb) = thumb {
                    // The thumb stays wholly inside the track: its values run between the centres of the two end
                    // thumbs. Naming that span keeps a fraction along `slider:` the same fraction of the scale.
                    target = rect.shrink2(egui::vec2((thumb.bounds.width() * 0.5).min(rect.width() * 0.5), 0.0));
                    ctx.data_mut(|data| data.insert_temp(id.with("span"), target.width()));
                    super::register_geometry(ctx, format!("sliderThumb:{}", spec.id), thumb.bounds);
                    hint(ui, element.id, thumb.bounds, row.enabled.then_some(reset_hint.as_str()));
                }
                if let Some(focus) = focus
                    && !focus.stop.text
                {
                    // The shortcut gate tells a focused slider (it owns the vertical arrows) from a text field.
                    super::note_slider_focus(ctx, focus.stop.root);
                }
            }
            "sliderValue" => {
                if let Some(focus) = focus {
                    number_focus = Some(focus.stop.root);
                }
                // The value field is "Exposure value"; how to use it is its description.
                let value_name = crate::i18n::tr_format!("{name} value", name = name);
                ctx.accesskit_node_builder(element.id, |node| {
                    node.set_label(value_name);
                    node.set_description(crate::i18n::tr(VALUE_HINT).to_owned());
                });
            }
            _ => {
                hint(ui, element.id, rect, row.enabled.then_some(reset_hint.as_str()));
            }
        }
        super::register_response(format!("{prefix}:{}", spec.id), target, &response);
        // `sliderField:` is the editor: it is there while a value is being typed, as the old field was
        if prefix == "sliderValue" && focus.is_some_and(|focus| focus.stop.text) && !giving_back {
            super::register_response(format!("sliderField:{}", spec.id), target, &response);
        }
    }
    // The readout goes back to showing the value once a typed edit ends (Return, a step): a field kept
    // focused would send the next Undo to its own text history instead of the photo's.
    if let Some(root) = number_focus
        && giving_back
    {
        super::text_field::blur_id(ctx, root);
    } else if keyed && number_focus.is_none() && ui.input(|input| input.pointer.any_released()) {
        ctx.data_mut(|data| data.insert_temp(pending_key, true));
    }
    track
}

/// Whether the key named `code` went down in this pass, for the reader `key`, which keeps its own memory of the key
/// being held.
fn key_went_down(ctx: &egui::Context, code: &str, key: egui::Id) -> bool {
    let down = craft::ui::runtime::keys::held(ctx).iter().any(|held| held.code_name() == code);
    let was = ctx.data_mut(|data| {
        let was = data.get_temp::<bool>(key).unwrap_or(false);
        data.insert_temp(key, down);
        was
    });
    down && !was
}

/// Says how to reset the row while the pointer rests on `area` (the caption, the thumb) of the row's part `owner`: the
/// framework's tooltip at the pointer, kept by that part's memory. No probe or second widget goes over the row, and
/// the part's own tooltip stays its own.
fn hint(ui: &Ui, owner: egui::Id, area: Rect, text: Option<&str>) {
    // once a pass per owner: a row drawn twice in one pass (a replay, a second run of its panel) shows one tip
    let pass = ui.ctx().cumulative_pass_nr();
    let shown = owner.with("lc-reset-hint-pass");
    if ui.ctx().data(|data| data.get_temp::<u64>(shown)) == Some(pass) {
        return;
    }
    ui.ctx().data_mut(|data| data.insert_temp(shown, pass));
    let at = ui.ctx().pointer_hover_pos().filter(|at| area.is_finite() && area.contains(*at));
    let pressed = ui.input(|input| input.pointer.any_down());
    let tip = text.map(|text| craft::ui::contract::Tooltip::from(text.to_owned()));
    craft::ui::contract::tooltip::at_pointer(ui, owner, tip.as_ref(), at, pressed);
}

/// Lightroom's Kelvin track: 2000–7000 K over the left half, logarithmic up to 50000 K ([`ControlSpec::position`]).
fn kelvin_position(value: f64, _min: f64, _max: f64) -> f64 {
    lightcraft_develop::controls::find("wb.temp").map_or(0.0, |spec| spec.position(value))
}

fn kelvin_value_at(position: f64, _min: f64, _max: f64) -> f64 {
    lightcraft_develop::controls::find("wb.temp").map_or(6500.0, |spec| spec.value_at(position))
}

/// The colour ramp of a coloured track (temperature, tint, the mixer's bands, gradients), drawn in the real
/// slider's groove; a plain track keeps the framework's own.
fn track_face(track: &Track) -> Option<TrackFace<'static>> {
    super::track_stops(track).map(|stops| TrackFace::new(face::Face::new(stops)))
}

/// A passive face: no binding, hit parts, focus stops or accessibility node. The real slider owns the groove's
/// geometry, the thumb, the focus ring, the gestures and the value.
mod face {
    use craft::behavior::{Input, Metrics, Sense};
    use craft::theme::Contrast;
    use craft::ui::contract::{Common, Component, Cx, MeasureCx, PaintUi, Role, Styled as _};
    use craft::ui::primitives::PaintShape;
    use craft::ui::style::{ComponentStyle, DataColor, StyleCx, TrackLook};
    use egui::{Color32, CornerRadius, Rect, Response, Vec2, pos2};

    #[derive(Clone, Debug, PartialEq, ComponentStyle)]
    pub(super) struct FaceStyle {
        #[style(metrics)]
        metrics: Metrics,
        track: TrackLook,
        high_contrast: bool,
    }

    #[derive(Clone, Component)]
    #[component(unregistered)]
    pub(super) struct Face {
        #[common]
        common: Common,
        stops: Vec<Color32>,
    }

    impl Face {
        pub(super) fn new(stops: Vec<Color32>) -> Self {
            Self { common: Common::new(), stops }
        }
    }

    impl Component for Face {
        const ROLE: Role = Role::None;
        const INPUT: Input = Input::Display;
        const PARTS: bool = false;
        const STOPS: bool = false;
        const OVERLAYS: bool = false;
        const SHOWS_LABEL: bool = false;
        type Style = FaceStyle;
        type Output = Response;

        fn style(cx: &StyleCx<'_>, _: FaceStyleKey) -> FaceStyle {
            FaceStyle { metrics: Metrics::new(), track: cx.track(), high_contrast: cx.contrast() == Contrast::High }
        }

        fn measure(&mut self, cx: &MeasureCx<'_, Self>) -> Vec2 {
            Vec2::splat(cx.style.track.thickness)
        }

        fn sense(&self) -> Sense {
            Sense::empty()
        }

        fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
            let track = &cx.style.track;
            // the theme's groove thickness, even when the ramp's own box is taller
            let rect = track.groove(cx.rect);
            let draw = ui.draw();
            if cx.style.high_contrast || cx.state().is_disabled() {
                // colour data never replaces the contrast or disabled cues
                draw.fill(&PaintShape::Rect(rect, CornerRadius::same((track.thickness / 2.0).round() as u8)), cx.resolve(&track.unfilled));
                return;
            }
            let segments = self.stops.len().saturating_sub(1);
            if segments == 0 {
                return;
            }
            // 48 strips whatever the width; the ends follow the slider's value direction
            for i in 0..48 {
                let a = i as f32 / 48.0;
                let b = (i + 1) as f32 / 48.0;
                let position = if cx.flow.is_rtl() { 1.0 - a } else { a };
                let along = position * segments as f32;
                let index = (along.floor() as usize).min(segments - 1);
                if let (Some(from), Some(to)) = (self.stops.get(index), self.stops.get(index + 1)) {
                    let color = crate::widgets::lerp(*from, *to, along - index as f32);
                    let strip =
                        Rect::from_min_max(pos2(rect.left() + rect.width() * a, rect.top()), pos2(rect.left() + rect.width() * b, rect.bottom()));
                    draw.data_fill(&PaintShape::Rect(strip, CornerRadius::ZERO), DataColor::new(color));
                }
            }
        }

        fn finish(self, cx: Cx<'_, Self>) -> Response {
            cx.response.clone()
        }
    }
}
