//! Color controls and white-balance presets on the SDK, with the existing develop model as authority.
//! Values, preview renders and undo still belong to the existing develop commands. No binding
//! keeps a second value: each pass reads the current photo's real DevelopSettings.

use super::sliders::{REL_TEMP, REL_TINT, k_to_rel, rel_to_k};
use craft::behavior::EditEvent;
use craft::ui::contract::overlay::Trigger;
use craft::ui::contract::{Props as _, Show as _, ShowContainer as _};
use craft::ui::menu::{Menu, MenuItem};
use lightcraft_develop::{ControlSpec, DevelopSettings, WbMode, controls};
use serde_json::json;

use crate::LightcraftApp;

const NUMERIC: [&str; 6] = ["wb.temp", "wb.tint", "wb.tempRel", "wb.tintRel", "color.vibrance", "color.saturation"];
const TRACKS: &str = "lc-sdk-color-tracks";
const NUDGES: &str = "lc-sdk-color-nudges";
const ACTIVE: &str = "lc-sdk-color-active";

#[derive(Clone)]
struct Active {
    gesture: craft::behavior::GestureId,
    photo: lightcraft_catalog::PhotoId,
    control: String,
}

/// Only the visible UI phase can end a Color edit. A hidden logic tick never edits a photo.
/// Closing Tools/Color, opening a modal or switching photos cancels this adapter's live edit,
/// not a later edit owned by an unported control or by the existing Light adapter.
pub(crate) fn frame_begin(app: &mut LightcraftApp, ctx: &egui::Context) {
    let key = egui::Id::unique(ACTIVE);
    let active = ctx.data(|data| data.get_temp::<Active>(key));
    let Some(active) = active else { return };
    if available(app) && app.session.active() == Some(active.photo) && app.ui.dragging_control.as_deref() == Some(active.control.as_str()) {
        return;
    }
    ctx.data_mut(|data| data.remove::<Active>(key));
    if app.ui.dragging_control.as_deref() == Some(active.control.as_str()) {
        if let Err(error) = app.run("develop.cancelInteraction", json!({})) {
            app.toast_error(ctx, error);
        }
        app.ui.dragging_control = None;
    }
}

#[derive(Clone)]
struct Nudges {
    photo: lightcraft_catalog::PhotoId,
    steps: [i32; 6],
}

fn available(app: &LightcraftApp) -> bool {
    app.ui.right == crate::state::RightPanel::Edit
        && !app.ui.fullscreen
        && app.ui.section_open("color")
        && !app.ui.section_folded("color")
        && app.ui.dialog.is_none()
        && app.ui.name_edit.is_none()
        && app.recording_shortcut.is_none()
        && !crate::panels::notices::blocks_input(app)
        && !crate::panels::library_problem::blocks_dialog_input(app)
        && !crate::updates_ui::blocks_input(app)
        && crate::docking::exposed(app, crate::docking::Panel::Tools)
}

/// Preserve LightCraft's hovered-or-focused Up/Down policy (Shift = five steps). This reads
/// host input before SDK hooks; it queues only bounded deltas, never edits a photo in a hidden
/// logic tick. SDK typing, pointer gestures, wheel and all other shortcuts stay untouched.
pub(crate) fn prepare_keys(app: &LightcraftApp, ctx: &egui::Context, raw: &mut egui::RawInput) {
    // A delta belongs to this one host pass. If Tools disappeared before its UI phase, never
    // replay the old key when the user opens it again (including Up+F8 in one input batch).
    ctx.data_mut(|data| data.remove::<Nudges>(egui::Id::unique(NUDGES)));
    if !available(app) {
        return;
    }
    // The SDK owns keys while its pointer gesture is captured. A host nudge is a
    // standalone Set: beginning it here would end the live engine interaction and
    // split one drag into separate undo records (even when another slider is hovered).
    if app.ui.dragging_control.is_some() && ctx.input(|input| input.pointer.any_down()) {
        return;
    }
    let Some(photo) = app.session.active() else { return };
    let focused = craft::ui::runtime::focus::focused_rec(ctx);
    if focused.as_ref().is_some_and(|rec| rec.stop.text) || ctx.egui_wants_keyboard_input() && !crate::widgets::slider_has_focus(ctx) {
        return;
    }
    let tracks = ctx.data(|data| data.get_temp::<[Option<egui::Id>; 6]>(egui::Id::unique(TRACKS))).unwrap_or([None; 6]);
    let mut pointer = ctx.pointer_hover_pos();
    for event in &raw.events {
        match event {
            egui::Event::PointerMoved(pos) => pointer = Some(*pos),
            egui::Event::PointerGone => pointer = None,
            _ => {}
        }
    }
    let visible = |index: usize| app.widgets.iter().find(|(id, _)| id == &format!("slider:{}", NUMERIC[index]));
    let hovered = pointer.and_then(|pos| {
        (0..NUMERIC.len()).find(|&index| {
            tracks[index].is_some()
                && visible(index).is_some_and(|(_, rect)| rect.contains(pos))
                && app
                    .widget_hits
                    .iter()
                    .find(|(id, _)| id == &format!("slider:{}", NUMERIC[index]))
                    .is_some_and(|(_, hit)| hit.interact_rect.contains(pos) && ctx.layer_id_at(pos) == Some(hit.layer_id))
        })
    });
    let target = hovered.or_else(|| {
        focused.as_ref().and_then(|rec| (0..NUMERIC.len()).find(|&index| tracks[index] == Some(rec.stop.id) && visible(index).is_some()))
    });
    let Some(index) = target else { return };
    let mut steps = 0i32;
    raw.events.retain(|event| {
        let egui::Event::Key { key, pressed, modifiers, .. } = event else { return true };
        if modifiers.command || modifiers.ctrl || modifiers.alt || !matches!(key, egui::Key::ArrowUp | egui::Key::ArrowDown) {
            return true;
        }
        if *pressed {
            let amount = if modifiers.shift { 5 } else { 1 };
            steps = steps.saturating_add(if *key == egui::Key::ArrowUp { amount } else { -amount });
        }
        false
    });
    if steps != 0 {
        ctx.data_mut(|data| {
            let key = egui::Id::unique(NUDGES);
            let mut pending = data.get_temp::<Nudges>(key).filter(|old| old.photo == photo).unwrap_or(Nudges { photo, steps: [0; 6] });
            if let Some(slot) = pending.steps.get_mut(index) {
                *slot = slot.saturating_add(steps);
            }
            data.insert_temp(key, pending);
        });
        ctx.request_repaint();
    }
}

/// UI phase only, before the panel reads its DevelopSettings snapshot.
pub(super) fn apply_pending(app: &mut LightcraftApp, ctx: &egui::Context) {
    let pending = ctx.data_mut(|data| {
        let key = egui::Id::unique(NUDGES);
        let pending = data.get_temp::<Nudges>(key);
        data.remove::<Nudges>(key);
        pending
    });
    let Some(pending) = pending.filter(|pending| available(app) && app.session.active() == Some(pending.photo)) else { return };
    for (id, steps) in NUMERIC.iter().zip(pending.steps) {
        if steps == 0 {
            continue;
        }
        let Some(spec) = spec(id) else { continue };
        let settings = app.session.develop_of(pending.photo).unwrap_or_default();
        let before = if id.starts_with("wb.") {
            let (raw, temp, tint) = shown_wb(app, pending.photo, &settings);
            if id.ends_with("Rel") == raw {
                continue;
            }
            if id.starts_with("wb.temp") { if raw { temp } else { k_to_rel(temp) } } else { tint.clamp(spec.min, spec.max) }
        } else {
            controls::get(&settings, id).unwrap_or(spec.default)
        };
        let after = crate::widgets::nudged(spec, before, f64::from(steps));
        if after == before {
            continue;
        }
        let event = EditEvent::set(craft::behavior::Mixed::Same(before), after, spec.label.into(), None);
        apply_number(app, ctx, spec, &event);
    }
}

fn spec(id: &str) -> Option<&'static ControlSpec> {
    match id {
        "wb.tempRel" => Some(&REL_TEMP),
        "wb.tintRel" => Some(&REL_TINT),
        _ => controls::find(id),
    }
}

/// Match the panel's effective As Shot/preset numbers, including embedded-JPEG raws.
fn shown_wb(app: &LightcraftApp, photo: lightcraft_catalog::PhotoId, settings: &DevelopSettings) -> (bool, f64, f64) {
    let raw = app.session.catalog.photo(photo).is_some_and(|p| p.develops_raw() && !p.relative_wb());
    let (temp, tint) = if settings.wb.mode == WbMode::AsShot && app.session.catalog.photo(photo).is_some_and(|p| p.relative_wb()) {
        (6500.0, 0.0)
    } else if raw {
        lightcraft_engine::pipeline::local::effective_wb(&app.session.source_info(photo), settings)
    } else {
        (settings.wb.temp, settings.wb.tint)
    };
    (raw, temp, tint)
}

pub(super) fn values(app: &mut LightcraftApp, ui: &mut egui::Ui, settings: &DevelopSettings, raw: bool) {
    if raw {
        if let Some(spec) = controls::find("wb.temp") {
            control(app, ui, spec, settings.wb.temp, true);
        }
        if let Some(spec) = controls::find("wb.tint") {
            control(app, ui, spec, settings.wb.tint, true);
        }
    } else {
        control(app, ui, &REL_TEMP, k_to_rel(settings.wb.temp), true);
        control(app, ui, &REL_TINT, settings.wb.tint.clamp(-100.0, 100.0), true);
    }
}

/// The remaining two Color adjustments use their authoritative develop specs directly.
/// Keep this entry point bounded: WB has its effective-value presentation above, while
/// mixer/grading controls retain their own adapters.
pub(super) fn adjustment(app: &mut LightcraftApp, ui: &mut egui::Ui, settings: &DevelopSettings, id: &str) {
    if !matches!(id, "color.vibrance" | "color.saturation") {
        return;
    }
    let Some(spec) = controls::find(id) else { return };
    control(app, ui, spec, controls::get(settings, id).unwrap_or(spec.default), true);
}

/// Custom is a display state, not an actionable preset, as in the original menu.
pub(super) fn preset(app: &mut LightcraftApp, ui: &mut egui::Ui, mode: WbMode) {
    // WB presets are actions, not live-preview values: use the SDK's existing value menu.
    // Its rows own real responses and its menu lifecycle handles keyboard focus on close.
    let t = crate::theme::Tokens::get(ui.ctx());
    let opener = crate::widgets::dropdown(ui, "wbMode", crate::i18n::tr(mode.label()), t.font(14.0), t.text_label);
    let mut picked = None;
    Menu::new(crate::i18n::tr("White Balance")).id_salt(("wbMode", app.session.active())).values().opened_by(&opener, Trigger::Click).show(
        ui,
        |ui, _| {
            for (index, value) in WbMode::ALL.into_iter().enumerate() {
                if value == WbMode::Custom {
                    continue;
                }
                let mut row = MenuItem::new(crate::i18n::tr(value.label())).id_salt(index).entry_checked(mode == value);
                if mode == value {
                    row = row.default_stop();
                }
                let out = row.show(ui);
                if !craft::ui::runtime::measure::active(ui.ctx()) {
                    crate::widgets::register_response(
                        format!("wbPreset:{}", serde_json::to_value(value).unwrap_or_default().as_str().unwrap_or("unknown")),
                        out.response.rect,
                        &out.response,
                    );
                    if out.ran {
                        picked = Some(value);
                    }
                }
            }
        },
    );
    if craft::ui::runtime::measure::active(ui.ctx()) {
        return;
    }
    if ui.is_enabled()
        && let Some(mode) = picked
        && let Err(error) = app.run("develop.wb", json!({"mode": mode}))
    {
        app.toast_error(ui.ctx(), error);
    }
}

/// A numeric value in the existing Color section, on its original displayed scale, on the shared develop slider
/// row (the Kelvin temperature has Lightroom's track there).
fn control(app: &mut LightcraftApp, ui: &mut egui::Ui, spec: &ControlSpec, value: f64, enabled: bool) {
    let row = crate::widgets::SliderRow { spec, value, enabled, label: None, salt: egui::Id::new(app.session.active()) };
    let shown = crate::widgets::slider_field(ui, &row);
    if let Some(track) = shown.track
        && let Some(index) = NUMERIC.iter().position(|id| *id == spec.id)
    {
        // the host's hovered-or-focused arrow keys find the row by its real track
        ui.ctx().data_mut(|data| {
            let key = egui::Id::unique(TRACKS);
            let mut tracks = data.get_temp::<[Option<egui::Id>; 6]>(key).unwrap_or([None; 6]);
            if let Some(slot) = tracks.get_mut(index) {
                *slot = Some(track);
            }
            data.insert_temp(key, tracks);
        });
    }
    if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        for event in &shown.out.events {
            // Disabling a running control cancels its SDK gesture; that cancellation must
            // still close the existing engine transaction, while disabled edits never run.
            if enabled && ui.is_enabled() || matches!(event, EditEvent::Cancel { .. }) {
                apply_number(app, ui.ctx(), spec, event);
            }
        }
    }
}

fn set(app: &mut LightcraftApp, spec: &ControlSpec, value: f64) -> Result<(), String> {
    let (id, value) = match spec.id {
        "wb.tempRel" => ("wb.temp", rel_to_k(value)),
        "wb.tintRel" => ("wb.tint", value),
        id => (id, value),
    };
    app.run("develop.set", json!({"control": id, "value": value})).map(|_| ())
}

/// SDK events are the same begin → live update → end transaction already used by LightCraft's
/// sliders. A typed commit or arrow-key Set is one complete transaction; Escape cancels the
/// engine interaction, restoring its captured settings instead of creating an undo step.
fn apply_number(app: &mut LightcraftApp, ctx: &egui::Context, spec: &ControlSpec, event: &EditEvent<f64>) {
    apply(app, ctx, spec.id, spec.label, event, |app, value| set(app, spec, *value));
}

fn apply<T>(
    app: &mut LightcraftApp,
    ctx: &egui::Context,
    control: &str,
    label: &str,
    event: &EditEvent<T>,
    mut set: impl FnMut(&mut LightcraftApp, &T) -> Result<(), String>,
) {
    let key = egui::Id::unique(ACTIVE);
    let owns = |gesture| {
        ctx.data(|data| data.get_temp::<Active>(key)).is_some_and(|active| {
            active.gesture == gesture
                && active.control == control
                && app.session.active() == Some(active.photo)
                && app.ui.dragging_control.as_deref() == Some(control)
        })
    };
    // A delayed SDK event after a panel/photo switch must not reopen or finish a transaction
    // belonging to a different control. Only a fresh Begin or one-shot Set can start an edit.
    match event {
        EditEvent::Update { id, .. } | EditEvent::Commit { id, .. } | EditEvent::Cancel { id, .. } if !owns(*id) => return,
        _ => {}
    }
    let result = match event {
        EditEvent::Begin { id, .. } => {
            let Some(photo) = app.session.active() else { return };
            app.run("develop.beginInteraction", json!({"label": label})).map(|_| {
                app.ui.dragging_control = Some(control.to_owned());
                ctx.data_mut(|data| data.insert_temp(key, Active { gesture: *id, photo, control: control.to_owned() }));
            })
        }
        EditEvent::Update { value, .. } => set(app, value),
        EditEvent::Commit { after, .. } => {
            let result = set(app, after);
            let ended = app.run("develop.endInteraction", json!({})).map(|_| ());
            app.ui.dragging_control = None;
            ctx.data_mut(|data| data.remove::<Active>(key));
            result.and(ended)
        }
        EditEvent::Cancel { .. } => {
            app.ui.dragging_control = None;
            ctx.data_mut(|data| data.remove::<Active>(key));
            app.run("develop.cancelInteraction", json!({})).map(|_| ())
        }
        EditEvent::Set { after, .. } => {
            let begun = app.run("develop.beginInteraction", json!({"label": label}));
            match begun {
                Ok(_) => {
                    let result = set(app, after);
                    let ended = app.run("develop.endInteraction", json!({})).map(|_| ());
                    app.ui.dragging_control = None;
                    ctx.data_mut(|data| data.remove::<Active>(key));
                    result.and(ended)
                }
                Err(error) => Err(error),
            }
        }
        _ => Ok(()),
    };
    if let Err(error) = result {
        app.toast_error(ctx, error);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::headless::Headless;
    use crate::tests_fixtures::SyntheticLibrary;
    use crate::{LightcraftApp, Services};
    use lightcraft_catalog::PhotoId;
    use lightcraft_develop::controls;
    use serde_json::json;

    const ADJUSTMENTS: [&str; 2] = ["color.vibrance", "color.saturation"];
    const TIMEOUT: Duration = Duration::from_secs(20);
    const SETTLE: Duration = Duration::from_secs(120);

    fn panel() -> (SyntheticLibrary, Headless) {
        // Real file-backed 64x48 photos keep renderer work bounded. The demo fixture
        // generates 24MP scenes; waiting for those hid the interaction assertions.
        let thread = std::thread::current();
        let tag: String = thread.name().unwrap_or("color-controls").chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' }).collect();
        let files = SyntheticLibrary::with_photos(&format!("sdk-color-{tag}"), 2);
        let mut h = Headless::new(files.app(), [1400.0, 1600.0], 1.0);
        let id = h.app.session.visible_cloned()[0];
        ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}));
        ask(&mut h, "ui.set", json!({"view": "detail", "right": "edit"}));
        h.app.ui.open_sections = vec!["color".into()];
        h.app.ui.folded_sections.clear();
        settle(&mut h);
        (files, h)
    }

    fn settle(h: &mut Headless) {
        assert!(
            h.settle(SETTLE),
            "Color panel settles: pending render slots={:?}, synthetic={}, pending gestures={}",
            h.app.renderer.pending_slots(),
            h.app.synthetic.len(),
            h.app.pending_gestures.len()
        );
    }

    fn ask(h: &mut Headless, method: &str, params: serde_json::Value) {
        let reply = h.request(method, params.clone(), TIMEOUT);
        assert_eq!(reply["ok"], true, "{method} {params}: {reply}");
        settle(h);
    }

    fn command(h: &mut Headless, command: &str, params: serde_json::Value) {
        ask(h, "engine.execute", json!({"command": command, "params": params}));
    }

    fn active(h: &Headless) -> PhotoId {
        h.app.session.active().expect("active test photo")
    }

    fn value_of(h: &Headless, photo: PhotoId, id: &str) -> f64 {
        controls::get(&h.app.session.develop_of(photo).expect("test photo settings"), id).expect("known Color control")
    }

    fn value(h: &Headless, id: &str) -> f64 {
        value_of(h, active(h), id)
    }

    fn widget(h: &Headless, id: &str) -> egui::Rect {
        h.app.widgets.iter().find(|(name, _)| name == id).map(|(_, rect)| *rect).unwrap_or_else(|| panic!("no actual SDK widget {id}"))
    }

    fn set(h: &mut Headless, id: &str, value: f64) {
        command(h, "develop.set", json!({"control": id, "value": value}));
    }

    fn type_draft(h: &mut Headless, id: &str, text: &str) {
        ask(h, "ui.clickWidget", json!({"id": format!("sliderValue:{id}")}));
        ask(h, "ui.key", json!({"key": "A", "cmd": true}));
        ask(h, "ui.text", json!({"text": text}));
    }

    fn press(h: &mut Headless, at: egui::Pos2, down: bool) {
        h.events.extend([
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: down, modifiers: egui::Modifiers::NONE },
        ]);
        h.step();
    }

    fn live_drag(h: &mut Headless, id: &str) -> egui::Pos2 {
        let thumb = widget(h, &format!("sliderThumb:{id}")).center();
        let track = widget(h, &format!("slider:{id}"));
        let to = egui::pos2(track.left() + track.width() * 0.8, track.center().y);
        press(h, thumb, true);
        for step in 1..=8 {
            h.events.push(egui::Event::PointerMoved(thumb + (to - thumb) * (step as f32 / 8.0)));
            h.step();
        }
        assert_eq!(h.app.ui.dragging_control.as_deref(), Some(id), "the real SDK drag opened an engine interaction");
        assert!(value(h, id) > 0.0, "the drag really previewed an edit");
        to
    }

    #[test]
    fn color_adjustments_pointer_drag_is_linear_and_one_undo_redo_step() {
        let (_files, mut h) = panel();
        for id in ADJUSTMENTS {
            set(&mut h, id, 0.0);
            let photo = active(&h);
            let before = h.app.session.develop_of(photo).unwrap();
            let undo = h.app.session.undo.len();
            let to = live_drag(&mut h, id);
            assert_eq!(h.app.session.undo.len(), undo, "live preview is not an undo entry");
            press(&mut h, to, false);
            settle(&mut h);
            assert!((value(&h, id) - 60.0).abs() <= 1.0, "{id}: linear 80% track position");
            let after = value(&h, id);
            assert_eq!(h.app.session.undo.len(), undo + 1, "one drag, one undo");
            assert_eq!(h.app.session.develop_of(photo).unwrap().wb, before.wb, "Color adjustment leaves WB alone");
            command(&mut h, "edit.undo", json!({}));
            assert_eq!(value(&h, id), 0.0);
            command(&mut h, "edit.redo", json!({}));
            assert_eq!(value(&h, id), after);
        }
    }

    #[test]
    fn color_adjustments_typed_absolute_commit_and_escape_restore_settings() {
        let (_files, mut h) = panel();
        for id in ADJUSTMENTS {
            set(&mut h, id, -30.0);
            let undo = h.app.session.undo.len();
            type_draft(&mut h, id, "+24");
            ask(&mut h, "ui.key", json!({"key": "Enter"}));
            assert_eq!(value(&h, id), 24.0, "a signed full value is absolute, not +=24");
            assert_eq!(h.app.session.undo.len(), undo + 1);
            // This must reach document undo, not the SDK field's private text undo.
            ask(&mut h, "ui.key", json!({"key": "Z", "cmd": true}));
            assert_eq!(value(&h, id), -30.0);
            let undo = h.app.session.undo.len();
            type_draft(&mut h, id, "73");
            ask(&mut h, "ui.key", json!({"key": "Escape"}));
            assert_eq!(value(&h, id), -30.0);
            assert_eq!(h.app.session.undo.len(), undo, "discarding a draft adds no history");
        }
    }

    #[test]
    fn color_adjustments_hover_keys_preserve_steps_and_range_clamps() {
        let (_files, mut h) = panel();
        for id in ADJUSTMENTS {
            let spec = controls::find(id).unwrap();
            assert_eq!((spec.min, spec.max, spec.default, spec.step, spec.decimals), (-100.0, 100.0, 0.0, 1.0, 0));
            set(&mut h, id, 0.0);
            ask(&mut h, "ui.hoverWidget", json!({"id": format!("slider:{id}"), "fx": 0.5}));
            ask(&mut h, "ui.key", json!({"key": "ArrowUp"}));
            assert_eq!(value(&h, id), 1.0);
            ask(&mut h, "ui.key", json!({"key": "ArrowDown", "shift": true}));
            assert_eq!(value(&h, id), -4.0);
            for (fraction, limit, key) in [(1.0, 100.0, "ArrowUp"), (0.0, -100.0, "ArrowDown")] {
                ask(&mut h, "ui.clickWidget", json!({"id": format!("slider:{id}"), "fx": fraction}));
                assert_eq!(value(&h, id), limit, "the actual SDK track reaches its endpoint");
                let undo = h.app.session.undo.len();
                ask(&mut h, "ui.key", json!({"key": key, "shift": true}));
                assert_eq!(value(&h, id), limit);
                assert_eq!(h.app.session.undo.len(), undo, "clamped nudge is a no-op");
            }
        }
    }

    #[test]
    fn color_adjustments_label_and_thumb_reset_are_single_undo_steps() {
        let (_files, mut h) = panel();
        for id in ADJUSTMENTS {
            for part in ["sliderLabel", "sliderThumb"] {
                set(&mut h, id, 37.0);
                let undo = h.app.session.undo.len();
                let (target, fraction) = if part == "sliderThumb" {
                    (format!("slider:{id}"), controls::find(id).unwrap().position(value(&h, id)))
                } else {
                    (format!("{part}:{id}"), 0.5)
                };
                ask(&mut h, "ui.clickWidget", json!({"id": target, "fx": fraction, "count": 2}));
                assert_eq!(value(&h, id), 0.0, "{part} resets {id}");
                assert_eq!(h.app.session.undo.len(), undo + 1, "reset is one undo step");
                command(&mut h, "edit.undo", json!({}));
                assert_eq!(value(&h, id), 37.0);
            }
        }
    }

    #[test]
    fn color_adjustments_escape_cancels_live_drag_without_undo() {
        let (_files, mut h) = panel();
        for id in ADJUSTMENTS {
            set(&mut h, id, 0.0);
            let before = h.app.session.develop_of(active(&h)).unwrap();
            let undo = h.app.session.undo.len();
            let to = live_drag(&mut h, id);
            h.events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            h.step();
            press(&mut h, to, false);
            settle(&mut h);
            assert_eq!(*h.app.session.develop_of(active(&h)).unwrap(), *before);
            assert_eq!(h.app.session.undo.len(), undo);
            assert!(h.app.ui.dragging_control.is_none());
        }
    }

    #[test]
    fn color_adjustments_panel_cancel_and_photo_switch_commit_preserve_ownership() {
        for id in ADJUSTMENTS {
            for boundary in ["fold", "tools", "photo", "modal"] {
                let (_files, mut h) = panel();
                let first = active(&h);
                let other = h.app.session.visible_cloned().into_iter().find(|photo| *photo != first).unwrap();
                let first_before = h.app.session.develop_of(first).unwrap();
                let other_before = h.app.session.develop_of(other).unwrap();
                let undo = h.app.session.undo.len();
                let to = live_drag(&mut h, id);
                let dragged_preview = h.app.session.develop_of(first).unwrap();
                match boundary {
                    "fold" => h.app.ui.folded_sections.push("color".into()),
                    "tools" => h.app.ui.right = crate::state::RightPanel::Info,
                    "photo" => {
                        h.app.run("library.select", json!({"ids": [other.0]})).unwrap();
                    }
                    "modal" => h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("A running task needs confirmation".into())),
                    _ => unreachable!(),
                }
                // A hidden logic tick does not consume/cancel the UI's owned interaction.
                let preview = h.app.session.develop_of(first).unwrap();
                h.app.logic(&h.view.ctx);
                assert_eq!(*h.app.session.develop_of(first).unwrap(), *preview);
                h.step();
                press(&mut h, to, false);
                settle(&mut h);
                if boundary == "photo" {
                    // library.select deliberately ends the interaction BEFORE changing
                    // selection (engine cmd/library/specs.rs). Preserve that product contract.
                    assert_eq!(*h.app.session.develop_of(first).unwrap(), *dragged_preview, "photo navigation commits the old photo's preview");
                    assert_eq!(h.app.session.undo.len(), undo + 1, "navigation commits exactly one drag history entry");
                    command(&mut h, "edit.undo", json!({}));
                    assert_eq!(*h.app.session.develop_of(first).unwrap(), *first_before, "one undo restores the prior photo after navigation");
                } else {
                    assert_eq!(*h.app.session.develop_of(first).unwrap(), *first_before, "{boundary} restores the original photo");
                    assert_eq!(h.app.session.undo.len(), undo, "cancel does not commit history");
                }
                assert_eq!(*h.app.session.develop_of(other).unwrap(), *other_before, "{boundary} cannot edit the other photo");
                h.app.ui.folded_sections.clear();
                h.app.ui.right = crate::state::RightPanel::Edit;
                h.app.quit_prompt = None;
                settle(&mut h);
                assert_eq!(*h.app.session.develop_of(first).unwrap(), *first_before, "reopening never replays the cancelled gesture");
                assert_eq!(*h.app.session.develop_of(other).unwrap(), *other_before);
            }
        }
    }

    #[test]
    fn color_adjustments_photo_switch_discards_numeric_draft() {
        let (_files, mut h) = panel();
        for id in ADJUSTMENTS {
            let first = active(&h);
            let other = h.app.session.visible_cloned().into_iter().find(|photo| *photo != first).unwrap();
            let before = h.app.session.develop_of(other).unwrap();
            let first_before = h.app.session.develop_of(first).unwrap();
            type_draft(&mut h, id, "91");
            command(&mut h, "library.select", json!({"ids": [other.0]}));
            ask(&mut h, "ui.key", json!({"key": "Enter"}));
            assert_eq!(*h.app.session.develop_of(other).unwrap(), *before, "old draft is photo-salted");
            assert_eq!(*h.app.session.develop_of(first).unwrap(), *first_before);
        }
    }

    #[test]
    fn color_adjustments_multi_photo_isolation_and_auto_sync_keep_one_undo() {
        let (_files, mut h) = panel();
        let first = active(&h);
        let other = h.app.session.visible_cloned().into_iter().find(|photo| *photo != first).unwrap();
        command(&mut h, "library.select", json!({"ids": [first.0, other.0], "active": first.0}));
        for id in ADJUSTMENTS {
            command(&mut h, "develop.autoSync", json!({"on": false}));
            let other_before = h.app.session.develop_of(other).unwrap();
            ask(&mut h, "ui.clickWidget", json!({"id": format!("slider:{id}"), "fx": 0.75}));
            assert_eq!(value_of(&h, first, id), 50.0);
            assert_eq!(*h.app.session.develop_of(other).unwrap(), *other_before, "selection alone never syncs");
            command(&mut h, "develop.autoSync", json!({"on": true}));
            let first_before = h.app.session.develop_of(first).unwrap();
            let undo = h.app.session.undo.len();
            ask(&mut h, "ui.clickWidget", json!({"id": format!("slider:{id}"), "fx": 0.25}));
            for photo in [first, other] {
                assert_eq!(value_of(&h, photo, id), -50.0, "Auto Sync uses the actual control value");
            }
            assert_eq!(h.app.session.undo.len(), undo + 1, "one edit for all photos");
            command(&mut h, "edit.undo", json!({}));
            assert_eq!(*h.app.session.develop_of(first).unwrap(), *first_before);
            assert_eq!(*h.app.session.develop_of(other).unwrap(), *other_before);
        }
    }

    #[test]
    fn color_adjustments_measure_pass_cannot_edit_or_consume_a_pointer_action() {
        use crate::headless::HeadlessView;

        for id in ADJUSTMENTS {
            let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
            let photo = app.session.active().unwrap();
            let before = app.session.develop_of(photo).unwrap();
            let undo = app.session.undo.len();
            let mut view = HeadlessView::new();
            app.install_framework(&view.ctx);
            let mut at = egui::Pos2::ZERO;
            for frame in 0..5 {
                let measuring = matches!(frame, 2 | 3);
                let events = if measuring {
                    vec![
                        egui::Event::PointerMoved(at),
                        egui::Event::PointerButton {
                            pos: at,
                            button: egui::PointerButton::Primary,
                            pressed: frame == 2,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                } else {
                    vec![]
                };
                let raw = HeadlessView::raw_input(egui::vec2(400.0, 300.0), 1.0, f64::from(frame) / 60.0, events);
                view.run(raw, |ui| {
                    let settings = app.session.develop_of(photo).unwrap();
                    let ctx = ui.ctx().clone();
                    egui::CentralPanel::default().show(ui, |ui| {
                        if measuring {
                            craft::ui::runtime::measure::run(&ctx, || super::adjustment(&mut app, ui, &settings, id));
                        } else {
                            super::adjustment(&mut app, ui, &settings, id);
                        }
                    });
                });
                let registry = crate::widgets::take_registry(&view.ctx);
                if frame == 1 {
                    let track = registry.iter().find(|(name, _)| name == &format!("slider:{id}")).unwrap().1;
                    at = egui::pos2(track.left() + track.width() * 0.8, track.center().y);
                }
                assert_eq!(*app.session.develop_of(photo).unwrap(), *before, "{id}: measuring input cannot edit, nor replay on a real pass");
                assert_eq!(app.session.undo.len(), undo);
                assert!(app.ui.dragging_control.is_none());
            }
        }
    }

    #[test]
    fn color_adjustments_and_light_mid_drag_keys_never_split_the_captured_edit() {
        for dragged in ["light.contrast", "color.vibrance", "color.saturation"] {
            let (_files, mut h) = panel();
            h.app.ui.open_sections = vec!["light".into(), "color".into()];
            settle(&mut h);
            for hover_other_adapter in [false, true] {
                set(&mut h, dragged, 0.0);
                let before = h.app.session.develop_of(active(&h)).unwrap();
                let undo = h.app.session.undo.len();
                let mut release_at = live_drag(&mut h, dragged);
                if hover_other_adapter {
                    let hovered = if dragged.starts_with("light.") { "color.saturation" } else { "light.contrast" };
                    let hover_track = widget(&h, &format!("slider:{hovered}"));
                    // Hover the other adapter without returning the captured slider to
                    // its original zero. A no-op drag correctly creates no undo entry.
                    release_at = egui::pos2(hover_track.left() + hover_track.width() * 0.8, hover_track.center().y);
                    h.events.push(egui::Event::PointerMoved(release_at));
                    h.step();
                }
                let preview = value(&h, dragged);
                assert!(preview.abs() > 0.5, "{dragged}, cross-hover={hover_other_adapter}: the falsifier must retain a real edit");
                for (key, shift) in [(egui::Key::ArrowUp, false), (egui::Key::ArrowDown, true)] {
                    for pressed in [true, false] {
                        h.events.push(egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers: egui::Modifiers { shift, ..egui::Modifiers::NONE },
                        });
                        h.step();
                        assert_eq!(h.app.ui.dragging_control.as_deref(), Some(dragged), "arrow input cannot terminate the captured drag");
                        assert_eq!(h.app.session.undo.len(), undo, "arrow input cannot commit an intermediate history step");
                        assert_eq!(value(&h, dragged), preview, "SDK defers step keys until the pointer interaction ends");
                    }
                }
                press(&mut h, release_at, false);
                settle(&mut h);
                assert_eq!(
                    h.app.session.undo.len(),
                    undo + 1,
                    "{dragged}, cross-hover={hover_other_adapter}: the full pointer drag remains one undo step"
                );
                command(&mut h, "edit.undo", json!({}));
                assert_eq!(*h.app.session.develop_of(active(&h)).unwrap(), *before, "one undo restores the entire drag");
            }
        }
    }
}
