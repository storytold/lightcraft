use super::{LightcraftApp, Value};
use crate::MIDI_IDLE;

impl LightcraftApp {
    /// Apply queued MIDI actions. A turn of one knob or fader is one undo step: the interaction opens
    /// on its first event and closes after [`MIDI_IDLE`] seconds without one (they have no release).
    pub(super) fn drain_midi(&mut self, ctx: &egui::Context) {
        use lightcraft_engine::midi::Action;

        let now = ctx.input(|i| i.time);
        if let Some(rx) = self.midi_rx.take() {
            // within a frame only the latest fader position and the sum of the knob ticks matter
            let mut actions: Vec<Action> = vec![];
            while let Ok(action) = rx.try_recv() {
                match (action, actions.last_mut()) {
                    (Action::Set { control, value }, Some(Action::Set { control: c, value: v })) if *c == control => *v = value,
                    (Action::Nudge { control, delta }, Some(Action::Nudge { control: c, delta: d })) if *c == control => *d += delta,
                    (action, _) => actions.push(action),
                }
            }
            self.midi_rx = Some(rx);
            for action in actions {
                match action {
                    Action::Command(id) => {
                        self.end_midi_interaction();
                        let _ = self.run(&id, serde_json::json!({}));
                    }
                    Action::Set { control, value } => {
                        let params = serde_json::json!({ "control": control, "value": value });
                        self.midi_edit(now, control, "develop.set", params);
                    }
                    Action::Nudge { control, delta } => {
                        let params = serde_json::json!({ "control": control, "delta": delta });
                        self.midi_edit(now, control, "develop.adjust", params);
                    }
                }
            }
        }
        if let Some((_, last)) = self.midi_active {
            if now - last >= MIDI_IDLE {
                self.end_midi_interaction();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(MIDI_IDLE - (now - last)));
            }
        }
    }

    /// Close the open MIDI interaction, if any.
    pub(crate) fn end_midi_interaction(&mut self) {
        if self.midi_active.take().is_some() {
            let _ = self.run("develop.endInteraction", serde_json::json!({}));
        }
    }

    /// One edit of `control` from a MIDI controller, inside that control's interaction (one undo step).
    /// Nothing happens without an active photo.
    fn midi_edit(&mut self, now: f64, control: String, command: &str, params: Value) {
        // a mouse drag owns the engine's interaction slot until it ends
        if self.session.active().is_none() || self.ui.dragging_control.is_some() {
            return;
        }
        // the engine's interaction was ended elsewhere: this turn no longer owns anything
        if self.session.interaction.is_none() {
            self.midi_active = None;
        }
        if self.midi_active.as_ref().is_some_and(|(c, _)| *c != control) {
            self.end_midi_interaction();
        }
        if self.midi_active.is_none() {
            let label = lightcraft_develop::controls::find(&control).map_or("Edit", |c| c.label);
            if self.run("develop.beginInteraction", serde_json::json!({ "label": label })).is_err() {
                return;
            }
            self.midi_active = Some((control.clone(), now));
        }
        if self.run(command, params).is_ok() {
            self.midi_active = Some((control, now));
        }
    }
}
