use super::{ControlResponse, GESTURE_TIMEOUT_MS, GestureFrame, LightcraftApp, MAX_PENDING_GESTURES, PendingGesture, Sender, Value};
use crate::control;

impl LightcraftApp {
    /// Take the control receiver for this frame, run every queued request whose sender still
    /// wants it, and reply directly or admit its screenshot/gesture to a later completed pass.
    /// Requests abandoned by their sender are skipped rather than executed late.
    pub(super) fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        while let Ok(req) = rx.try_recv() {
            // a request its sender gave up on (the window drew nothing for a while) is not run
            // late: it would happen twice once the sender retries (issue #718)
            if !req.take() {
                continue;
            }
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Screenshot { target, headless } => {
                    if let Err(error) = self.admit_shot(ctx, target, headless, reply.clone()) {
                        let _ = reply.send(serde_json::json!({"ok": false, "error": error}));
                    }
                }
                control::Outcome::Gesture { method, params } => {
                    if let Err(error) = self.admit_gesture(ctx, method, params, reply.clone()) {
                        let _ = reply.send(serde_json::json!({"ok": false, "error": error}));
                    }
                }
            }
        }
        self.control_rx = Some(rx);
    }

    /// The navigation/document/modal/panel state that decides what an automation id means.
    fn gesture_frame(&self) -> GestureFrame {
        GestureFrame {
            view: self.ui.view,
            active: self.session.active(),
            library: self.session.library_generation(),
            dialog: self.ui.dialog.as_ref().map(std::mem::discriminant),
            left_panel: self.ui.left_panel,
            right: self.ui.right,
            presets: self.ui.presets,
            filmstrip: self.ui.filmstrip,
        }
    }

    /// Queue a synthetic-input request. Nothing is answered and no event is queued until a
    /// completed layout newer than this admission exists (see [`Self::resolve_pending_gestures`]).
    pub(crate) fn admit_gesture(&mut self, ctx: &egui::Context, method: String, params: Value, reply: Sender<ControlResponse>) -> Result<(), String> {
        if self.pending_gestures.len() >= MAX_PENDING_GESTURES {
            return Err(format!("{}: {MAX_PENDING_GESTURES} control gestures are already pending", method));
        }
        let now = self.tick_now_ms();
        self.pending_gestures.push(PendingGesture {
            method,
            params,
            reply,
            admitted_epoch: self.layout_epoch,
            frame: self.gesture_frame(),
            deadline_ms: now + GESTURE_TIMEOUT_MS,
            resolved_pass: None,
            remaining_events: 0,
            started: false,
            target: None,
            pressed: None,
            result: Value::Null,
        });
        ctx.request_repaint();
        Ok(())
    }

    /// Resolve one queued gesture against the fresh widgets/image rect of the pass that just
    /// completed. A changed view/document/dialog/panel state cancels instead of retargeting.
    pub(crate) fn resolve_pending_gestures(&mut self, ctx: &egui::Context) {
        if self.pending_gestures.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        let frame = self.gesture_frame();
        let epoch = self.layout_epoch;
        let mut gestures = std::mem::take(&mut self.pending_gestures);
        let mut keep = Vec::with_capacity(gestures.len());
        for mut gesture in gestures.drain(..) {
            if gesture.resolved_pass.is_some() {
                // Already handed to the delivery queue; `reply_delivered_gestures` (or
                // `expire_gestures`) owns its deadline and must remove its unsent events too.
                keep.push(gesture);
                continue;
            }
            if now > gesture.deadline_ms {
                // Only this request failed to get a fresh layout. A later gesture has its own
                // deadline and must not be canceled with it.
                let _ =
                    gesture.reply.send(serde_json::json!({"ok": false, "error": format!("{} timed out before the UI consumed it", gesture.method)}));
                continue;
            }
            if epoch <= gesture.admitted_epoch {
                keep.push(gesture);
                continue;
            }
            if gesture.frame != frame {
                let _ = gesture.reply.send(serde_json::json!({
                    "ok": false,
                    "error": format!("{} canceled: the view, document, dialog or panel layout changed before a fresh layout", gesture.method),
                }));
                continue;
            }
            let before = self.synthetic.len();
            match control::gesture_events(self, ctx, &gesture.method, &gesture.params) {
                Ok(outcome) => {
                    gesture.target = outcome.target;
                    gesture.result = outcome.result;
                    gesture.resolved_pass = Some(self.ui_passes);
                    gesture.remaining_events = self.synthetic.len().saturating_sub(before);
                    gesture.started = gesture.remaining_events == 0;
                    keep.push(gesture);
                }
                Err(error) => {
                    let _ = gesture.reply.send(serde_json::json!({"ok": false, "error": error}));
                }
            }
        }
        self.pending_gestures = keep;
    }

    /// Answer gestures whose last synthetic event a completed, nondiscarded UI pass consumed.
    /// A zero-event action waits for one completed pass after resolution.
    ///
    /// Delivery does not compare the frame again: the gesture's own event may legitimately change
    /// the view, document, dialog or panel (a click that opens a dialog, a key that starts Compare),
    /// so a second snapshot would cancel exactly the input that did its job. Freshness is checked
    /// before resolution ([`Self::resolve_pending_gestures`]) and again immediately before the
    /// batch is injected ([`Self::drop_gestures_stale_before_injection`]).
    pub(crate) fn reply_delivered_gestures(&mut self) {
        if self.pending_gestures.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        // Cancel only the gestures that actually stalled: their unsent events are at the front of
        // the queue, and gestures queued behind them are independent input that must survive.
        let mut i = 0;
        while i < self.pending_gestures.len() {
            if self.pending_gestures[i].resolved_pass.is_none() || now <= self.pending_gestures[i].deadline_ms {
                i += 1;
                continue;
            }
            let at_head = self.head_gesture() == Some(i);
            let gesture = self.pending_gestures.remove(i);
            self.cancel_gesture_events(&gesture, at_head);
            let _ = gesture.reply.send(serde_json::json!({
                "ok": false,
                "error": format!("{} timed out before the UI consumed its last event", gesture.method),
            }));
        }
        if !self.synthetic.is_empty() || self.synthetic_in_flight {
            return;
        }
        let mut gestures = std::mem::take(&mut self.pending_gestures);
        let mut keep = Vec::with_capacity(gestures.len());
        for mut gesture in gestures.drain(..) {
            let Some(resolved) = gesture.resolved_pass else {
                keep.push(gesture);
                continue;
            };
            if self.ui_passes <= resolved {
                keep.push(gesture);
                continue;
            }
            if gesture.remaining_events > 0 {
                // A defensive queue reset (a non-head gesture owned unsent events) dropped this
                // batch. It must not become an `ok: true` for input no panel ever saw.
                let _ = gesture.reply.send(serde_json::json!({
                    "ok": false,
                    "error": format!("{} canceled: its queued input was dropped before delivery", gesture.method),
                }));
                continue;
            }
            let result = std::mem::take(&mut gesture.result);
            let _ = gesture.reply.send(serde_json::json!({"ok": true, "result": result}));
        }
        self.pending_gestures = keep;
    }

    /// The gesture whose events sit at the head of the synthetic queue.
    pub(super) fn head_gesture(&self) -> Option<usize> {
        self.pending_gestures.iter().position(|gesture| gesture.resolved_pass.is_some() && gesture.remaining_events > 0)
    }

    /// Fail resolved gestures whose view/document/dialog/panel snapshot changed before their
    /// first synthetic batch reached the next completed pass. A hidden logic tick runs between
    /// passes and may change what an automation id or a photo click means; fail closed instead of
    /// delivering it against the new state.
    ///
    /// A widget gesture also has to keep aiming at the requested widget. egui attributes a click
    /// to the widget under the press point in the *previous completed pass*, while the control
    /// batch spans several host frames (a move, then press, then release). Recheck the target
    /// before the first event and before every press; if it moved or disappeared, cancel this
    /// gesture with an error instead of clicking whatever is now at those coordinates.
    ///
    /// Only the gesture at the head of the queue is checked, and later batches of the same gesture
    /// may see its own partial effect (the first click of a double click selects the photo the
    /// second one opens). A second, concurrently queued gesture is checked when its events reach
    /// the head.
    pub(super) fn drop_gestures_stale_before_injection(&mut self, ctx: &egui::Context) -> bool {
        let Some(head) = self.head_gesture() else { return false };
        let first_batch = !self.pending_gestures[head].started;
        if first_batch && self.pending_gestures[head].frame != self.gesture_frame() {
            let gestures = std::mem::take(&mut self.pending_gestures);
            for gesture in gestures {
                if gesture.resolved_pass.is_none() {
                    self.pending_gestures.push(gesture);
                } else {
                    if gesture.pressed.is_some() {
                        self.queue_pointer_cleanup();
                    }
                    let _ = gesture.reply.send(serde_json::json!({
                        "ok": false,
                        "error": format!("{} canceled: the view, document, dialog or panel layout changed before delivery", gesture.method),
                    }));
                }
            }
            self.synthetic.clear();
            return true;
        }
        let Some(target) = self.pending_gestures[head].target.clone() else { return false };
        let press_next = matches!(self.synthetic.first(), Some(egui::Event::PointerButton { pressed: true, .. }));
        let release_next = matches!(self.synthetic.first(), Some(egui::Event::PointerButton { pressed: false, .. }));
        // A drag legitimately ends outside the widget it started on; only a click must still find
        // its target at the release that egui attributes the click to. A hover must still find a
        // visible widget for its single pointer move.
        let check_release = release_next && target.check_release;
        let check_visible = first_batch && target.check_visible;
        if !first_batch && !press_next && !check_release && !check_visible {
            return false;
        }
        let latest = control::widget_rect(self, &target.id);
        let latest_hit = control::widget_hit(self, &target.id);
        // A press must go to the widget that owns the point. A click's release must too (a drag
        // may legitimately end outside the widget it started on), so an overlay that appears
        // between press and release cancels the click before the release rather than activating it.
        // A hover's single move must also still find the widget visible: the documented contract
        // rejects a clipped or newly covered target instead of moving the pointer at stale coordinates.
        let must_own = press_next || (release_next && target.check_release) || check_visible;
        // A quit prompt or settings notice owns input even before its Foreground areas have
        // settled (egui's first layout pass is non-interactable), and even when a control request
        // is resolved against the registry of that first pass. Only the prompt's own buttons pass.
        let notice_ok = !must_own || crate::panels::notices::allows_target(self, latest_hit);
        let still_aimed = latest.is_some_and(|rect| {
            // A changed widget cancels any gesture before its first event. Owned presses, click
            // releases and hovers must also be inside egui's clipped, visible part and be the
            // topmost widget hit: a target scrolled out of its panel, or one under a non-modal
            // overlay, must not be pressed or hovered at stale coordinates.
            let geometry_ok = rect == target.rect || rect.contains(target.at);
            geometry_ok && notice_ok && (!must_own || control::widget_target_visible(ctx, latest_hit, target.at))
        });
        if still_aimed {
            return false;
        }
        let outside_registered = !target.rect.contains(target.at);
        let reason = match latest {
            None => "disappeared",
            Some(rect) if !(rect == target.rect || rect.contains(target.at)) => "moved",
            Some(_) if outside_registered => "has no visible interaction area at the requested point",
            Some(_) => "is no longer visible",
        };
        let phase = if first_batch {
            "its first event"
        } else if check_release {
            "its release"
        } else {
            "its press"
        };
        // Only this gesture's unsent events are removed: they are at the front of the queue, and
        // gestures resolved after it are checked in turn when they reach the head. Failing the
        // whole click would also discard unrelated input.
        let gesture = self.pending_gestures.remove(head);
        self.cancel_gesture_events(&gesture, true);
        let _ = gesture.reply.send(serde_json::json!({
            "ok": false,
            "error": format!("{} canceled: widget `{}` {reason} before {phase}; nothing was retargeted", gesture.method, target.id),
        }));
        true
    }

    /// Remove a canceled gesture's unsent events from the front of the queue and release a pointer
    /// button it left down. Callers cancel in queue order, so the events removed are exactly this
    /// gesture's unsent prefix; a later gesture's events stay queued.
    pub(super) fn cancel_gesture_events(&mut self, gesture: &PendingGesture, at_head: bool) {
        if at_head {
            let unsent = gesture.remaining_events.min(self.synthetic.len());
            self.synthetic.drain(..unsent);
        } else if gesture.remaining_events > 0 {
            // The queue order makes only the head own unsent events. If a caller ever breaks that
            // invariant, fail safe: stale synthetic input must not reach an unrelated widget, even
            // at the cost of dropping the rest of this batch instead of panicking or guessing a
            // prefix. `reply_delivered_gestures` fails any gesture whose events disappeared this
            // way instead of reporting a false success.
            log::warn!("canceled non-head gesture still owned {} unsent synthetic events; dropping the queue", gesture.remaining_events);
            self.synthetic.clear();
            self.synthetic_in_flight = false;
        }
        if gesture.pressed.is_some() {
            self.queue_pointer_cleanup();
        }
    }

    /// Cancel every synthetic gesture that a newly shown prompt or blocking library window makes
    /// unsafe.
    ///
    /// A gesture whose input is still queued must not be delivered under the barrier or later when
    /// it is gone. A gesture whose press already reached egui must be released without a click. A
    /// gesture that already delivered all its events may still complete its reply: the barrier may
    /// well be its own result (e.g. a key that opened the quit prompt). `all` is for a batch already
    /// injected this host frame before the barrier was discovered after `raw_input_hook`: the
    /// barrier filter drops it before any panel sees it, so even a fully injected gesture must fail
    /// rather than report a successful delivery.
    pub(crate) fn cancel_gestures_for_barrier(&mut self, all: bool, reason: &str) {
        // All not-yet-injected events belong to the queue's head, one gesture at a time. Cancel
        // them in order so only that gesture's unsent prefix is removed.
        while let Some(head) = self.head_gesture() {
            let gesture = self.pending_gestures.remove(head);
            self.cancel_gesture_events(&gesture, true);
            let _ = gesture.reply.send(serde_json::json!({
                "ok": false,
                "error": format!("{} canceled: {reason} before its input was delivered", gesture.method),
            }));
        }
        let mut i = 0;
        while i < self.pending_gestures.len() {
            if all || self.pending_gestures[i].resolved_pass.is_none() || self.pending_gestures[i].pressed.is_some() {
                let gesture = self.pending_gestures.remove(i);
                self.cancel_gesture_events(&gesture, false);
                let _ = gesture.reply.send(serde_json::json!({
                    "ok": false,
                    "error": format!("{} canceled: {reason} before its input was delivered", gesture.method),
                }));
            } else {
                i += 1;
            }
        }
    }

    /// Cancel a press that reached egui without making it a click or starting a drag. The reset
    /// itself happens before egui's next input pass in [`Self::raw_input_hook`], where replacing
    /// the pointer state and stopping the drag is safe (the same approach as the sibling
    /// SoundCraft/VectorCraft apps). No pointer movement is injected: moving beyond
    /// `max_click_dist` suppresses a click but can also start a drag on a slider or curve.
    pub(super) fn queue_pointer_cleanup(&mut self) {
        self.pointer_cleanup = true;
    }

    /// Apply a queued pointer cancellation before egui processes the next input pass.
    pub(super) fn apply_pointer_cleanup(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if !std::mem::take(&mut self.pointer_cleanup) {
            return;
        }
        ctx.input_mut(|input| input.pointer = egui::PointerState::default());
        ctx.stop_dragging();
        if self.synthetic_mods != egui::Modifiers::NONE {
            // The cleanup removes only the app's synthetic modifiers, never a real host change.
            // A host `ModifiersChanged` in this same raw batch is already authoritative and was
            // recorded before this call; otherwise restore the host's last known state (which may
            // be a physically held Shift), rather than clearing it unconditionally with NONE.
            let host_changed = raw.events.iter().any(|event| matches!(event, egui::Event::ModifiersChanged(_)));
            self.synthetic_mods = egui::Modifiers::NONE;
            if !host_changed {
                raw.events.push(egui::Event::ModifiersChanged(self.host_mods));
            }
        }
        self.synthetic_mods_release = false;
    }

    /// Fail pending gestures at the monotonic deadline even when no UI pass runs (a hidden window
    /// whose paint request the host ignored). `logic` calls this every tick.
    pub(super) fn expire_gestures(&mut self) {
        if self.pending_gestures.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        if !self.pending_gestures.iter().any(|gesture| now > gesture.deadline_ms) {
            return;
        }
        // Cancel exactly the stalled requests, in queue order, so a later gesture's input survives.
        let mut i = 0;
        while i < self.pending_gestures.len() {
            if now <= self.pending_gestures[i].deadline_ms {
                i += 1;
                continue;
            }
            let at_head = self.head_gesture() == Some(i);
            let gesture = self.pending_gestures.remove(i);
            self.cancel_gesture_events(&gesture, at_head);
            let _ = gesture.reply.send(serde_json::json!({"ok": false, "error": format!("{} timed out before the UI consumed it", gesture.method)}));
        }
    }
}
