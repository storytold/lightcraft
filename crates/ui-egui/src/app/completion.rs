//! Per-pass presentation facts only. Genuine input stays in the receiving app body.
use super::LightcraftApp;
use crate::{control::ClipboardFacts, panels, state::GestureFrame, widgets::WidgetHit};
use std::sync::{Weak, atomic::AtomicU8};

pub(super) struct GestureFacts {
    pub now_ms: f64,
    pub frame: GestureFrame,
    pub epoch: u64,
    pub passes: u64,
    pub deadlines: Vec<(Weak<AtomicU8>, f64)>,
    pub clipboard: ClipboardFacts,
    /// Weak allocation identities survive compaction, but terminal request removal revokes liveness.
    pub pastes: Option<Vec<(Weak<AtomicU8>, bool)>>,
}
impl GestureFacts {
    pub fn ready(&self, gesture: &crate::PendingGesture) -> bool {
        gesture.resolved_pass.is_none() && self.now_ms <= gesture.deadline_ms && self.epoch > gesture.admitted_epoch && self.frame == gesture.frame
    }
}
pub(super) struct Snapshot {
    pub widgets: Vec<(String, egui::Rect)>,
    pub hits: Vec<(String, WidgetHit)>,
    pub image: Option<egui::Rect>,
    pub ui_time: f64,
    pub hidden: bool,
    pub gestures: GestureFacts,
    pub notice: panels::notices::PromptCompletion,
    pub library: panels::library_problem::LibraryCompletion,
}
struct Pass {
    owner: egui::ViewportId,
    max_passes: usize,
    body: Option<Snapshot>,
}
#[derive(Default)]
pub(super) struct Completion {
    stack: Vec<Pass>,
    ready: Vec<Snapshot>,
}
impl egui::Plugin for Completion {
    fn debug_name(&self) -> &'static str {
        "lightcraft.host-completion"
    }
    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        // This instance belongs to ONE Context. Capture identity before end_pass pops a viewport.
        self.stack.push(Pass { owner: ui.ctx().viewport_id(), max_passes: ui.ctx().options(|o| o.max_passes.get()), body: None });
    }
    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        let Some(pass) = self.stack.pop() else { return };
        let Some(mut body) = pass.body else { return }; // early return / foreign body is not a layout
        if output.platform_output.requested_discard() && output.platform_output.num_completed_passes < pass.max_passes {
            return;
        }
        // Earlier nested output is already valid. Count that completion, never a newer live App frame.
        if let Some(previous) = self.ready.last() {
            body.gestures.passes = previous.gestures.passes.wrapping_add(1);
            body.gestures.epoch = previous.gestures.epoch.wrapping_add(1);
        }
        for parent in &mut self.stack {
            if let Some(candidate) = &mut parent.body {
                candidate.gestures.passes = body.gestures.passes.wrapping_add(1);
                candidate.gestures.epoch = body.gestures.epoch.wrapping_add(1);
            }
        }
        let clipboard = body.gestures.clipboard;
        if let Some(pastes) = &mut body.gestures.pastes {
            pastes.retain(|(claim, eligible)| claim.upgrade().is_some_and(|claim| clipboard.append_paste(output, pass.owner, &claim, *eligible)));
        }
        let hidden = body.hidden;
        self.ready.push(body);
        // Hidden logic has no next body. Visible input/logic drains first; exit drains the last one.
        if hidden {
            ctx.request_repaint_of(pass.owner);
        }
    }
}
pub(super) fn install(ctx: &egui::Context) {
    if ctx.plugin_opt::<Completion>().is_none() {
        ctx.add_plugin(Completion::default());
    }
}
impl LightcraftApp {
    pub(crate) fn capture_ui_completion(&mut self, ctx: &egui::Context) {
        let Some(plugin) = ctx.plugin_opt::<Completion>() else {
            // Preserve direct legacy callers; native/headless hosts install outside their first pass.
            self.finish_ui_pass(ctx);
            return;
        };
        // Consumed input is never held for paint acceptance, even if a later hook discards this body.
        self.synthetic_in_flight = false;
        let body = self.presentation_snapshot(ctx);
        if let Some(pass) = plugin.lock().stack.last_mut()
            && pass.owner == ctx.viewport_id()
        {
            pass.body = Some(body);
        }
    }
    /// Host seam: before any next App mutation, and immediately after the final headless output.
    pub fn drain_ui_completions(&mut self, ctx: &egui::Context) {
        let Some(plugin) = ctx.plugin_opt::<Completion>() else { return };
        let ready = std::mem::take(&mut plugin.lock().ready);
        for body in ready {
            self.apply_ui_completion(ctx, body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Completion;
    use crate::{LightcraftApp, Services};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    fn app(ctx: &egui::Context) -> LightcraftApp {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
        app.install_framework(ctx);
        app.set_simulated_clock(Some(10.0));
        app
    }
    fn raw() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0))),
            time: Some(7.0),
            ..Default::default()
        }
    }
    /// A framework text field named `name`: the paste target the host needs (an `egui` text editor holding the focus).
    /// `begin` asks for the focus before the field's first input.
    fn field(ui: &mut egui::Ui, name: &str, text: &mut String, begin: bool) {
        let _ = crate::text_field::TextField::singleline(name, text).begin_edit(begin).show(ui);
    }
    /// Frames of `name` in `viewport` until the field has settled with the keyboard focus.
    fn focus_field(ctx: &egui::Context, viewport: egui::ViewportId, name: &str, text: &mut String) {
        for _ in 0..3 {
            let mut input = raw();
            if viewport != egui::ViewportId::ROOT {
                input.viewport_id = viewport;
                input.viewports.entry(viewport).or_default().parent = Some(egui::ViewportId::ROOT);
            }
            ctx.run_ui(input, |ui| field(ui, name, text, true)).drop_without_applying_deltas();
        }
    }
    fn marker(app: &mut LightcraftApp, name: &str) {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(20.0, 20.0));
        app.next_widgets = vec![(name.into(), rect)];
        app.next_widget_hits =
            vec![(name.into(), crate::widgets::WidgetHit { id: egui::Id::unique(name), layer_id: egui::LayerId::background(), interact_rect: rect })];
    }
    #[test]
    fn genuine_input_is_visible_before_late_discard_and_empty_retry_never_replays_it() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let effects = Arc::new(AtomicUsize::new(0));
        let seen = effects.clone();
        ctx.on_end_pass(
            "late SDK discard",
            Arc::new(move |ui| {
                assert_eq!(seen.load(Ordering::SeqCst), 1, "effect already committed before the late hook");
                if ui.ctx().current_pass_index() == 0 {
                    ui.ctx().request_discard("late sizing");
                }
            }),
        );
        let mut input = raw();
        input.events.push(egui::Event::Text("accepted".into()));
        let mut passes = 0;
        let mut output = ctx.run_ui(input, |ui| {
            app.drain_ui_completions(ui.ctx());
            for event in ui.input(|i| i.events.clone()) {
                if let egui::Event::Text(text) = event {
                    app.ui.search.push_str(&text);
                    effects.fetch_add(1, Ordering::SeqCst);
                }
            }
            assert_eq!(app.ui.search, "accepted");
            assert!(app.widgets.is_empty(), "no speculative registry was published");
            marker(&mut app, if passes == 0 { "discarded" } else { "retry" });
            app.synthetic_in_flight = true;
            app.capture_ui_completion(ui.ctx());
            assert!(!app.synthetic_in_flight, "input housekeeping does not wait for paint");
            passes += 1;
        });
        output.textures_delta.clear();
        assert_eq!(passes, 2);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(app.ui_passes, 0);
        app.drain_ui_completions(&ctx); // final output needs no future body
        assert_eq!((app.ui_passes, app.layout_epoch), (1, 1));
        assert_eq!(app.widgets.first().map(|w| w.0.as_str()), Some("retry"));
        app.drain_ui_completions(&ctx);
        assert_eq!(app.ui_passes, 1, "taken exactly once");
    }
    #[test]
    fn no_body_and_fonts_early_return_cannot_mint_a_completion() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let (reply, _rx) = std::sync::mpsc::channel();
        app.admit_gesture(&ctx, "ui.clipboard".into(), serde_json::json!({"action": "paste"}), reply).unwrap();
        ctx.run_ui(raw(), |_| {}).drop_without_applying_deltas();
        ctx.run_ui(raw(), |ui| app.ui(ui)).drop_without_applying_deltas();
        app.drain_ui_completions(&ctx);
        assert_eq!((app.ui_passes, app.layout_epoch), (0, 0));
        assert!(ctx.plugin::<Completion>().lock().stack.is_empty());
        assert!(ctx.plugin::<Completion>().lock().ready.is_empty());
        assert_eq!(app.pending_gestures[0].completion.load(Ordering::SeqCst), 0, "no body cannot claim a request");
    }
    #[test]
    fn no_paste_completion_runs_whole_finish_with_own_clocks_epochs_and_deadlines() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let (reply, rx) = std::sync::mpsc::channel();
        app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({"x": 12, "y": 14}), reply).unwrap();
        let deadline = app.pending_gestures[0].deadline_ms;
        app.loupe_timing.observe(lightcraft_catalog::PhotoId(1), "thumb", 1.0);
        app.loupe_timing.end_frame(1.0);
        ctx.run_ui(raw(), |ui| {
            marker(&mut app, "own");
            app.pointer_cleanup = true;
            app.capture_ui_completion(ui.ctx());
        })
        .drop_without_applying_deltas();
        app.set_simulated_clock(Some(100000.0));
        app.ui_passes = 900;
        app.layout_epoch = 900;
        app.pending_gestures[0].deadline_ms = 0.0;
        app.drain_ui_completions(&ctx);
        assert_eq!((app.ui_passes, app.layout_epoch), (1, 1));
        assert_eq!(app.pending_gestures[0].deadline_ms, deadline);
        assert_eq!(app.pending_gestures[0].resolved_pass, Some(1));
        assert_eq!(app.synthetic.len(), 1);
        assert_eq!(app.loupe_timing.to_json()["steps"][0]["dwellMs"], 6000.0, "own UI clock, not the newer app clock");
        assert_eq!(app.widgets.first().map(|w| w.0.as_str()), Some("own"));
        assert_eq!(app.widget_hits.first().map(|w| w.0.as_str()), Some("own"));
        assert!(ctx.has_requested_repaint(), "pointer cancellation cleanup still asks for a frame");
        assert!(rx.try_recv().is_err(), "resolution is not delivery");
        app.set_simulated_clock(Some(10.0));
        app.synthetic.clear(); // unsent event disappeared, not a successful delivery
        ctx.run_ui(raw(), |ui| app.capture_ui_completion(ui.ctx())).drop_without_applying_deltas();
        app.drain_ui_completions(&ctx);
        let response = rx.try_recv().unwrap();
        assert_eq!(response["ok"], false);
        assert!(response["error"].as_str().unwrap().contains("dropped before delivery"));
    }
    #[test]
    fn hidden_logic_drains_a_real_final_body_but_never_invents_another_body() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let mut input = raw();
        input.viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(true);
        ctx.run_ui(input, |ui| {
            marker(&mut app, "hidden");
            app.capture_ui_completion(ui.ctx());
        })
        .drop_without_applying_deltas();
        assert_eq!(app.ui_passes, 0);
        assert!(ctx.has_requested_repaint(), "output hook woke ordinary hidden logic");
        app.logic(&ctx);
        assert_eq!(app.ui_passes, 1);
        assert_eq!(app.widgets.first().map(|w| w.0.as_str()), Some("hidden"));
        app.logic(&ctx);
        assert_eq!(app.ui_passes, 1, "logic without a body cannot advance layout");
    }
    #[test]
    fn captured_changed_frame_cancels_instead_of_retargeting() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let (reply, rx) = std::sync::mpsc::channel();
        app.admit_gesture(&ctx, "ui.clickWidget".into(), serde_json::json!({"id": "new-target"}), reply).unwrap();
        app.ui.filmstrip = !app.ui.filmstrip;
        ctx.run_ui(raw(), |ui| {
            marker(&mut app, "new-target");
            app.capture_ui_completion(ui.ctx());
        })
        .drop_without_applying_deltas();
        app.drain_ui_completions(&ctx);
        let response = rx.try_recv().unwrap();
        assert_eq!(response["ok"], false);
        assert!(response["error"].as_str().unwrap().contains("layout changed"));
        assert!(app.synthetic.is_empty());
    }
    #[test]
    fn own_paste_is_appended_once_and_unrelated_body_commands_are_not_stripped() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx);
        let mut text = String::new();
        focus_field(&ctx, egui::ViewportId::ROOT, "paste-owner", &mut text);
        assert!(ctx.text_edit_focused());
        let (reply, rx) = std::sync::mpsc::channel();
        app.admit_gesture(&ctx, "ui.clipboard".into(), serde_json::json!({"action": "paste"}), reply).unwrap();
        let mut output = ctx.run_ui(raw(), |ui| {
            field(ui, "paste-owner", &mut text, false);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste); // unrelated body request
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title("keep".into()));
            app.capture_ui_completion(ui.ctx());
        });
        output.textures_delta.clear();
        let commands = &output.viewport_output.get(&egui::ViewportId::ROOT).unwrap().commands;
        assert_eq!(commands.iter().filter(|c| matches!(c, egui::ViewportCommand::RequestPaste)).count(), 2);
        assert!(commands.iter().any(|c| matches!(c, egui::ViewportCommand::Title(s) if s == "keep")));
        app.drain_ui_completions(&ctx);
        assert!(rx.try_recv().is_err(), "queueing a host command is not delivery");
        let mut next = ctx.run_ui(raw(), |ui| app.capture_ui_completion(ui.ctx()));
        next.textures_delta.clear();
        assert!(
            next.viewport_output.values().all(|v| v.commands.iter().all(|c| !matches!(c, egui::ViewportCommand::RequestPaste))),
            "drain did not reissue a command"
        );
        app.drain_ui_completions(&ctx);
        assert_eq!(rx.try_recv().unwrap()["ok"], true, "queue acknowledgement only, not host delivery");
    }
    #[test]
    fn nested_paste_uses_its_child_output_even_after_egui_popped_to_the_parent() {
        for (drain, discard, cancel) in [(false, false, false), (true, false, false), (true, true, false), (true, false, true)] {
            let ctx = egui::Context::default();
            let mut app = app(&ctx);
            let child = egui::ViewportId::from_hash_of("paste-child");
            let (stale, stale_rx) = std::sync::mpsc::channel();
            app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({}), stale).unwrap();
            app.pending_gestures[0].deadline_ms = 0.0;
            let mut text = String::new();
            let mut parent_text = String::new();
            // both viewports' fields hold their own keyboard focus before the pass under test
            focus_field(&ctx, egui::ViewportId::ROOT, "parent-field", &mut parent_text);
            focus_field(&ctx, child, "child-field", &mut text);
            let (reply, rx) = std::sync::mpsc::channel();
            app.admit_gesture(&ctx, "ui.clipboard".into(), serde_json::json!({"action": "paste"}), reply).unwrap();
            let survivor = Arc::downgrade(&app.pending_gestures[1].completion);
            let (fence, _fence_rx) = std::sync::mpsc::channel();
            app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({}), fence).unwrap();
            ctx.on_end_pass(
                "nested sizing discard",
                Arc::new(move |ui| {
                    if discard && ui.ctx().current_pass_index() == 0 {
                        ui.ctx().request_discard("late parent/child sizing");
                    }
                }),
            );
            let mut outer = ctx.run_ui(raw(), |ui| {
                if ui.ctx().current_pass_index() > 0 {
                    app.capture_ui_completion(ui.ctx());
                    return;
                }
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste); // unrelated parent request
                field(ui, "parent-field", &mut parent_text, false);
                app.capture_ui_completion(ui.ctx()); // SAME pending paste in BOTH real candidates
                let mut input = raw();
                input.viewport_id = child;
                input.viewports.entry(child).or_default().parent = Some(egui::ViewportId::ROOT);
                let mut own = ctx.run_ui(input, |child_ui| {
                    if cancel {
                        app.cancel_gestures_for_barrier(false, "nested prompt");
                    }
                    field(child_ui, "child-field", &mut text, false);
                    child_ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title("unrelated child".into()));
                    app.capture_ui_completion(child_ui.ctx());
                });
                own.textures_delta.clear();
                assert_eq!(
                    own.viewport_output.get(&child).unwrap().commands.iter().filter(|c| matches!(c, egui::ViewportCommand::RequestPaste)).count(),
                    if cancel { 0 } else { 1 }
                );
                assert!(
                    own.viewport_output
                        .get(&child)
                        .unwrap()
                        .commands
                        .iter()
                        .any(|c| matches!(c, egui::ViewportCommand::Title(s) if s == "unrelated child"))
                );
                assert!(own.viewport_output.get(&egui::ViewportId::ROOT).unwrap().commands.is_empty(), "child output did not steal parent commands");
                if drain {
                    app.drain_ui_completions(&ctx);
                }
                if drain && !cancel {
                    app.pending_gestures[0].deadline_ms += 2000.0;
                }
            });
            outer.textures_delta.clear();
            assert_eq!(
                outer
                    .viewport_output
                    .get(&egui::ViewportId::ROOT)
                    .unwrap()
                    .commands
                    .iter()
                    .filter(|c| matches!(c, egui::ViewportCommand::RequestPaste))
                    .count(),
                1
            );
            app.drain_ui_completions(&ctx);
            assert_eq!((app.ui_passes, app.layout_epoch), (2, 2));
            if cancel {
                assert_eq!(rx.try_recv().unwrap()["ok"], false);
                assert!(rx.try_recv().is_err());
                continue;
            }
            assert_eq!(app.pending_gestures[0].resolved_pass, Some(1));
            assert!(Arc::ptr_eq(&survivor.upgrade().unwrap(), &app.pending_gestures[0].completion));
            assert_eq!(app.pending_gestures[0].deadline_ms, if drain { 17000.0 } else { 15000.0 });
            assert_eq!(stale_rx.try_recv().unwrap()["ok"], false);
            assert!(rx.try_recv().is_err(), "no false failure/ack while own input remains queued");
        }
    }
    #[test]
    fn nested_viewports_pair_lifo_before_pop_and_foreign_contexts_stay_separate() {
        for drain in [false, true] {
            let ctx = egui::Context::default();
            let other = egui::Context::default();
            let mut app = app(&ctx);
            super::install(&other);
            let child = egui::ViewportId::from_hash_of("child");
            ctx.on_end_pass(
                "discard child sizing",
                Arc::new(move |ui| {
                    if ui.ctx().viewport_id() == child && ui.ctx().current_pass_index() == 0 {
                        ui.ctx().request_discard("child sizing");
                    }
                }),
            );
            let mut output = ctx.run_ui(raw(), |ui| {
                marker(&mut app, "parent");
                app.capture_ui_completion(ui.ctx());
                for index in 0..(if drain { 2 } else { 1 }) {
                    let mut child_input = raw();
                    child_input.viewport_id = child;
                    child_input.viewports.entry(child).or_default().parent = Some(egui::ViewportId::ROOT);
                    let mut nested = ctx.run_ui(child_input, |child_ui| {
                        marker(&mut app, "child");
                        app.capture_ui_completion(child_ui.ctx());
                        child_ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title("child-output".into()));
                    });
                    nested.textures_delta.clear();
                    assert!(
                        nested
                            .viewport_output
                            .get(&child)
                            .unwrap()
                            .commands
                            .iter()
                            .any(|c| matches!(c, egui::ViewportCommand::Title(s) if s == "child-output"))
                    );
                    assert_eq!(ui.ctx().viewport_id(), egui::ViewportId::ROOT);
                    if drain {
                        app.drain_ui_completions(&ctx);
                        assert_eq!(app.ui_passes, index + 1);
                    }
                }
                other.run_ui(raw(), |_| {}).drop_without_applying_deltas();
            });
            output.textures_delta.clear();
            assert!(
                output
                    .viewport_output
                    .get(&egui::ViewportId::ROOT)
                    .unwrap()
                    .commands
                    .iter()
                    .all(|c| !matches!(c, egui::ViewportCommand::RequestPaste)),
                "child command was not reissued to popped parent"
            );
            let plugin = ctx.plugin::<Completion>();
            let mut completion = plugin.lock();
            let ready = std::mem::take(&mut completion.ready);
            assert_eq!(
                ready.iter().map(|s| s.widgets[0].0.as_str()).collect::<Vec<_>>(),
                if drain { vec!["parent"] } else { vec!["child", "parent"] }
            );
            assert_eq!(ready.iter().map(|s| s.gestures.passes).collect::<Vec<_>>(), if drain { vec![3] } else { vec![1, 2] });
            assert!(completion.stack.is_empty());
            assert!(other.plugin::<Completion>().lock().ready.is_empty());
            completion.ready = ready;
            drop(completion);
            app.drain_ui_completions(&ctx);
            let total = if drain { 3 } else { 2 };
            assert_eq!((app.ui_passes, app.layout_epoch), (total, total), "all and only valid children plus parent, across drains/discards");
        }
    }
}
