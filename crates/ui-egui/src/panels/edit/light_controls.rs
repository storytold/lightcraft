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

/// UI phase only, before the panel reads its DevelopSettings snapshot.
pub(super) fn apply_pending(app: &mut LightcraftApp, ctx: &egui::Context) {
    let pending = ctx.data_mut(|data| {
        let key = egui::Id::unique(NUDGES);
        let pending = data.get_temp::<Nudges>(key);
        data.remove::<Nudges>(key);
        pending
    });
    let Some(pending) = pending.filter(|pending| available(app) && app.session.active() == Some(pending.photo)) else { return };
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
    let row = crate::widgets::SliderRow { spec, value, enabled, label: None, salt: egui::Id::new(app.session.active()) };
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
