//! The connected Light tonal controls, using the framework's slider + numeric field.
//! Values, preview renders and undo still belong to the existing develop commands. No binding
//! keeps a second value: each pass reads the current photo's real DevelopSettings.

use craft::behavior::EditEvent;
use lightcraft_develop::{ControlSpec, DevelopSettings, controls};
use serde_json::json;

use crate::LightcraftApp;

const TONAL: [&str; 6] = ["light.exposure", "light.contrast", "light.highlights", "light.shadows", "light.whites", "light.blacks"];
const TRACKS: &str = "lc-sdk-light-tracks";
const NUDGES: &str = "lc-sdk-light-nudges";

#[derive(Clone)]
struct Nudges {
    photo: lightcraft_catalog::PhotoId,
    steps: [i32; 6],
}

fn available(app: &LightcraftApp) -> bool {
    app.ui.right == crate::state::RightPanel::Edit && !app.ui.fullscreen
        && app.ui.section_open("light") && !app.ui.section_folded("light")
        && app.ui.dialog.is_none() && app.ui.name_edit.is_none() && app.recording_shortcut.is_none()
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
    // LightCraft has always accepted both decimal separators, independently of UI locale.
    // Limit normalization to a genuine currently registered tonal numeric editor; other
    // text fields and all shortcuts keep their original input.
    if let Some(rec) = focused.as_ref().filter(|rec| rec.stop.text) {
        let numeric = app.widgets.iter().any(|(id, rect)| {
            TONAL.iter().any(|control| id == &format!("sliderValue:{control}"))
                && ctx.read_response(rec.stop.root).is_some_and(|response| response.rect.intersects(*rect))
        });
        if numeric {
            for event in &mut raw.events {
                if let egui::Event::Text(text) | egui::Event::Paste(text) = event {
                    *text = text.replace(',', ".");
                }
            }
        }
    }
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
    let visible = |index: usize| app.widgets.iter().find(|(id, _)| id == &format!("slider:{}", TONAL[index]));
    let hovered = pointer.and_then(|pos| {
        (0..TONAL.len()).find(|&index| {
            tracks[index].is_some()
                && visible(index).is_some_and(|(_, rect)| rect.contains(pos))
                && app
                    .widget_hits
                    .iter()
                    .find(|(id, _)| id == &format!("slider:{}", TONAL[index]))
                    .is_some_and(|(_, hit)| hit.interact_rect.contains(pos) && ctx.layer_id_at(pos) == Some(hit.layer_id))
        })
    });
    let target = hovered
        .or_else(|| focused.as_ref().and_then(|rec| (0..TONAL.len()).find(|&index| tracks[index] == Some(rec.stop.id) && visible(index).is_some())));
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

/// UI phase only, before the panel reads its DevelopSettings snapshot. The nudge lands on the pass that draws
/// for real: a sizing pass or the SDK's measuring run writes nothing, and the nudge waits for that pass. Where
/// there is nothing to nudge — the panel is gone, the photo changed, or the Light controls are disabled — the
/// input is dropped instead of being held for a later pass.
pub(super) fn apply_pending(app: &mut LightcraftApp, ui: &egui::Ui) {
    let ctx = ui.ctx();
    let key = egui::Id::unique(NUDGES);
    let Some(pending) = ctx.data(|data| data.get_temp::<Nudges>(key)) else { return };
    if !available(app) || app.session.active() != Some(pending.photo) {
        // Nothing is nudged in a panel that is gone, or on another photo: the input is dropped.
        ctx.data_mut(|data| data.remove::<Nudges>(key));
        return;
    }
    if !ui.is_enabled() {
        // The Light controls can't be edited: the input is dropped rather than kept for a pass that would apply it.
        ctx.data_mut(|data| data.remove::<Nudges>(key));
        return;
    }
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ctx) {
        // A measuring run writes no edit: the nudge stays for the pass that draws for real.
        return;
    }
    ctx.data_mut(|data| data.remove::<Nudges>(key));
    for (id, steps) in TONAL.iter().zip(pending.steps) {
        if steps == 0 { continue; }
        let Some(spec) = controls::find(id) else { continue };
        let settings = app.session.develop_of(pending.photo).unwrap_or_default();
        let before = controls::get(&settings, id).unwrap_or(spec.default);
        let after = crate::widgets::nudged(spec, before, f64::from(steps));
        if after == before { continue; }
        let event = EditEvent::set(craft::behavior::Mixed::Same(before), after, spec.label.into(), None);
        apply(app, ctx, spec, &event);
    }
}

/// One of the six tonal controls in the existing Light section, on the shared develop slider row. HDR-specific
/// controls keep their current implementation; this adapter does not alter the section or control specs.
pub(super) fn control(app: &mut LightcraftApp, ui: &mut egui::Ui, settings: &DevelopSettings, id: &str, enabled: bool) {
    let Some(spec) = controls::find(id) else { return };
    let value = controls::get(settings, id).unwrap_or(spec.default);
    let row = crate::widgets::SliderRow { spec, value, enabled, label: None, salt: egui::Id::new(app.session.active()), record: None };
    let shown = crate::widgets::slider_field(ui, &row);
    if let Some(track) = shown.track
        && let Some(index) = TONAL.iter().position(|id| *id == spec.id)
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
                apply(app, ui.ctx(), spec, event);
            }
        }
    }
}

fn set(app: &mut LightcraftApp, spec: &ControlSpec, value: f64) -> Result<(), String> {
    app.run("develop.set", json!({"control": spec.id, "value": value})).map(|_| ())
}

/// SDK events are the same begin → live update → end transaction already used by LightCraft's
/// sliders. A typed commit or arrow-key Set is one complete transaction; Escape cancels the
/// engine interaction, restoring its captured settings instead of creating an undo step.
fn apply(app: &mut LightcraftApp, ctx: &egui::Context, spec: &ControlSpec, event: &EditEvent<f64>) {
    let result = match event {
        EditEvent::Begin { .. } => app.run("develop.beginInteraction", json!({"label": spec.label})).map(|_| {
            app.ui.dragging_control = Some(spec.id.to_owned());
        }),
        EditEvent::Update { value, .. } => set(app, spec, *value),
        EditEvent::Commit { after, .. } => {
            let result = set(app, spec, *after);
            let ended = app.run("develop.endInteraction", json!({})).map(|_| ());
            app.ui.dragging_control = None;
            result.and(ended)
        }
        EditEvent::Cancel { .. } => {
            app.ui.dragging_control = None;
            app.run("develop.cancelInteraction", json!({})).map(|_| ())
        }
        EditEvent::Set { after, .. } => {
            let begun = app.run("develop.beginInteraction", json!({"label": spec.label}));
            match begun {
                Ok(_) => {
                    let result = set(app, spec, *after);
                    let ended = app.run("develop.endInteraction", json!({})).map(|_| ());
                    app.ui.dragging_control = None;
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

    use super::*;
    use crate::headless::{Headless, HeadlessView};
    use crate::{LightcraftApp, Services};

    const SETTLE: Duration = Duration::from_secs(120);
    const TIMEOUT: Duration = Duration::from_secs(20);

    /// The demo library in Detail with the Edit panel showing and its Light section open.
    fn light_panel() -> Headless {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
        let id = h.app.session.visible_cloned()[0].0;
        let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), TIMEOUT);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), TIMEOUT);
        assert_eq!(r["ok"], true, "{r}");
        h.app.ui.open_sections = vec!["light".into()];
        h.app.ui.folded_sections.clear();
        h.settle(SETTLE);
        assert!(available(&h.app), "the Light section is on screen");
        h
    }

    /// Queue `steps` nudges on `TONAL[index]`, as the Up or Down key over its row does in the input phase.
    fn queue(h: &Headless, index: usize, steps: i32) {
        let photo = h.app.session.active().expect("an active photo");
        let mut queued = Nudges { photo, steps: [0; 6] };
        if let Some(slot) = queued.steps.get_mut(index) {
            *slot = steps;
        }
        h.view.ctx.data_mut(|data| data.insert_temp(egui::Id::unique(NUDGES), queued));
    }

    /// Whether a nudge is still waiting for its pass.
    fn waiting(h: &Headless) -> bool {
        h.view.ctx.data(|data| data.get_temp::<Nudges>(egui::Id::unique(NUDGES))).is_some()
    }

    /// The active photo's Contrast, the control the queued nudge moves.
    fn contrast(h: &Headless) -> f64 {
        let photo = h.app.session.active().expect("an active photo");
        let spec = controls::find("light.contrast").expect("the Contrast control");
        controls::get(&h.app.session.develop_of(photo).unwrap_or_default(), "light.contrast").unwrap_or(spec.default)
    }

    /// One pass of the app's own context, handing the panel's `Ui` to `run`.
    fn pass(h: &mut Headless, time: f64, run: impl FnOnce(&mut LightcraftApp, &mut egui::Ui)) {
        let ctx = h.view.ctx.clone();
        let app = &mut h.app;
        // egui may run the pass again; the body runs once, on the first
        let mut run = Some(run);
        let raw = HeadlessView::raw_input(egui::vec2(1400.0, 1000.0), 1.0, time, vec![]);
        let mut out = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                if let Some(run) = run.take() {
                    run(&mut *app, ui);
                }
            });
        });
        out.textures_delta.clear();
    }

    /// A queued Light nudge is delivered by the pass that draws for real, and by that one only: a sizing pass or
    /// the SDK's measuring run writes no edit (the nudge waits for the real pass), and a disabled Light section
    /// drops the input instead of holding it for a later pass.
    #[test]
    fn a_light_nudge_lands_only_on_an_enabled_pass_that_draws_for_real() {
        let mut h = light_panel();
        let before = contrast(&h);
        let undo = h.app.session.undo.len();
        let mut at = h.time() + 0.1;
        assert_eq!(TONAL[1], "light.contrast", "the second tonal control, the one the nudge moves");
        for measuring in ["sizing", "measure"] {
            queue(&h, 1, 1);
            at += 0.1;
            pass(&mut h, at, move |app, ui| match measuring {
                "sizing" => {
                    let child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    apply_pending(app, &child);
                }
                _ => {
                    let ctx = ui.ctx().clone();
                    craft::ui::runtime::measure::run(&ctx, || apply_pending(app, ui));
                }
            });
            assert_eq!(contrast(&h), before, "{measuring}: a measuring pass writes no edit");
            assert_eq!(h.app.session.undo.len(), undo, "{measuring}: and records no undo step");
            assert!(waiting(&h), "{measuring}: the nudge waits for the pass that draws for real");
        }
        // the real pass delivers it, once
        at += 0.1;
        pass(&mut h, at, |app, ui| apply_pending(app, ui));
        let spec = controls::find("light.contrast").expect("the Contrast control");
        assert_eq!(contrast(&h), crate::widgets::nudged(spec, before, 1.0), "one step over Contrast, on the real pass");
        assert!(!waiting(&h), "the nudge is consumed exactly once");
        assert_eq!(h.app.session.undo.len() - undo, 1, "one undo step");
        // a disabled Light section takes no edit, and the input is dropped rather than replayed later
        let held = contrast(&h);
        queue(&h, 1, 1);
        at += 0.1;
        pass(&mut h, at, |app, ui| {
            let _ = ui.add_enabled_ui(false, |ui| apply_pending(app, ui));
        });
        assert_eq!(contrast(&h), held, "a disabled Light section writes no edit");
        assert!(!waiting(&h), "and the input is dropped, not held for a later pass");
        assert_eq!(h.app.session.undo.len() - undo, 1, "the disabled pass records no undo step");
    }
}
