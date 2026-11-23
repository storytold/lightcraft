//! The connected Light tonal controls, using the framework's slider + numeric field.
//! Values, preview renders and undo still belong to the existing develop commands. No binding
//! keeps a second value: each pass reads the current photo's real DevelopSettings.

use craft::behavior::EditEvent;
use craft::ui::contract::{Bind, Props as _, Show as _, Sizing};
use craft::ui::form::SliderField;
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


/// One of the six tonal controls in the existing Light section. HDR-specific controls keep
/// their current implementation; this adapter does not alter the section or control specs.
pub(super) fn control(app: &mut LightcraftApp, ui: &mut egui::Ui, settings: &DevelopSettings, id: &str, enabled: bool) {
    let Some(spec) = controls::find(id) else { return };
    let value = controls::get(settings, id).unwrap_or(spec.default);
    let record_key = ui.scope_id().with(("framework-light", spec.id));
    let (out, record) = craft::ui::runtime::inspect::record(ui, record_key, |ui| {
        craft::ui::runtime::env::with_env(ui, |set| set.wheel = Some(craft::ui::runtime::env::WheelScope::Directional), |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 0 }).show(ui, |ui| {
            SliderField::new(crate::i18n::tr(spec.label), Bind::given(value).reset(spec.default), spec.min..=spec.max)
                .id_salt(spec.id)
                .step(spec.step)
                .fill(false)
                .tooltip(crate::i18n::tr("Click to type a value, drag to change it").to_string())
                .enabled(enabled)
                .sizing(Sizing::FillWidth)
                .show(ui)
        }).inner
        })
    });
    let mut number_focus = None;
    if let Some(record) = record {
        // Preserve the product's automation names using the SDK's actual laid-out hit areas,
        // not guessed rectangles or duplicate interactive controls.
        for (index, element) in record.elements.iter().enumerate() {
            let Some(facts) = &element.facts else { continue };
            // The native text menu is recorded inside this composite's overlay hook. Its
            // real menu row receives the press; this only preserves the product locator.
            if element.component == "MenuItem" {
                let paste = craft::ui::runtime::commands::resolve(ui.ctx(), craft::ui::runtime::commands::EditCommands::Paste);
                if element.part.is_none() && facts.name.as_deref() == Some(paste.label.as_ref())
                    && let Some(response) = ui.ctx().read_response(element.id)
                {
                    crate::widgets::register_response(format!("sliderField:{}:paste", spec.id), element.rect, &response);
                }
                continue;
            }
            let prefix = match facts.role.as_str() {
                "Slider" => "slider",
                "SpinButton" => "sliderValue",
                "Label" => "sliderLabel",
                _ => continue,
            };
            if prefix == "slider" && let Some(index) = TONAL.iter().position(|id| *id == spec.id) {
                ui.ctx().data_mut(|data| {
                    let key = egui::Id::unique(TRACKS);
                    let mut tracks = data.get_temp::<[Option<egui::Id>; 6]>(key).unwrap_or([None; 6]);
                    if let Some(slot) = tracks.get_mut(index) { *slot = Some(element.id); }
                    data.insert_temp(key, tracks);
                });
            }
            if prefix == "sliderValue"
                && let Some(focused) = craft::ui::runtime::focus::focused_rec(ui.ctx())
                && focused.stop.id == element.id
            {
                number_focus = Some(focused.stop.root);
            }
            if prefix == "slider"
                && let Some(focused) = craft::ui::runtime::focus::focused_rec(ui.ctx())
                && focused.stop.id == element.id
                && !focused.stop.text
            {
                // LightCraft's shortcut gate distinguishes sliders from text fields. The
                // runtime may put egui focus on an arena root rather than this component id.
                crate::widgets::note_slider_focus(ui.ctx(), focused.stop.root);
            }
            let mut rect = record.stop_rect(index).unwrap_or(element.rect);
            if prefix == "slider"
                && let Some(thumb) = record.hits.iter().find(|part| {
                    part.owner == element.id && part.part.is_some_and(|part| part.key == craft::ui::value_drag::Slider::THUMB)
                })
            {
                // SDK thumbs stay wholly inside the track. Its value span runs between the
                // centres of the two endpoint thumbs, not across the full component box.
                // Name that actual span so a fractional automation click means the same
                // fraction of the value range, with no invented hit owner or input mapping.
                let half = (thumb.bounds.width() * 0.5).min(rect.width() * 0.5);
                rect = rect.shrink2(egui::vec2(half, 0.0));
            }
            let label = crate::i18n::tr(spec.label).to_string();
            let reset_hint = crate::i18n::tr_format!("{name} — double-click to reset it", name = label);
            let on_thumb = prefix == "slider" && record.hits.iter().any(|hit| {
                hit.owner == element.id
                    && hit.part.is_some_and(|part| part.key == craft::ui::value_drag::Slider::THUMB)
                    && ui.ctx().pointer_hover_pos().is_some_and(|at| hit.bounds.contains(at))
            });
            let hint = (enabled && (prefix == "sliderLabel" || on_thumb)).then_some(reset_hint.as_str());
            crate::widgets::register_with_tooltip(ui, format!("{prefix}:{}", spec.id), rect, hint);
            if prefix == "slider"
                && let Some(thumb) = record.hits.iter().find(|hit| hit.owner == element.id
                    && hit.part.is_some_and(|part| part.key == craft::ui::value_drag::Slider::THUMB))
            {
                crate::widgets::register_geometry(ui.ctx(), format!("sliderThumb:{}", spec.id), thumb.bounds);
            }
            if prefix == "sliderValue" {
                crate::widgets::register(ui, format!("sliderField:{}", spec.id), rect);
            }
        }
    }
    if !craft::ui::runtime::measure::active(ui.ctx()) {
        for event in &out.events {
            // Disabling a running control cancels its SDK gesture; that cancellation must
            // still close the existing engine transaction, while disabled edits never run.
            if enabled && ui.is_enabled() || matches!(event, EditEvent::Cancel { .. }) {
                apply(app, ui.ctx(), spec, event);
            }
        }
        // LightCraft's readout returns to display mode after a numeric commit or cancellation;
        // leaving the SDK field focused would send the next Undo to its private text history.
        if let Some(root) = number_focus
            && out.events.iter().any(|event| matches!(event,
                EditEvent::Set { .. } | EditEvent::Commit { .. } | EditEvent::Cancel { .. }
            ))
        {
            ui.memory_mut(|memory| memory.surrender_focus(root));
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
