use super::*;

impl LightcraftApp {
    /// Take the host's dropped files, exactly once. The queue is filled by `raw_input_hook`, so a
    /// hidden logic tick can never replay the last pass's drop. Web hosts call this in their UI
    /// phase (they need `bytes_async`, which is not available on a native file path).
    pub fn take_dropped_files(&mut self) -> Vec<egui::DroppedFileHandle> {
        std::mem::take(&mut self.dropped_files)
    }

    /// Import native drops as presets, face models or photos.
    pub(super) fn import_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = self.take_dropped_files();
        if dropped.is_empty() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.handle_dropped_files(ctx, dropped);
        #[cfg(target_arch = "wasm32")]
        let _ = (dropped, ctx);
    }

    /// Files dropped on the window: face models open their licence dialog, presets import as
    /// presets, photos and folders go to the import review (or straight in, Settings ▸ Import).
    #[cfg(not(target_arch = "wasm32"))]
    fn handle_dropped_files(&mut self, ctx: &egui::Context, dropped: Vec<egui::DroppedFileHandle>) {
        let dropped: Vec<String> =
            dropped.iter().map(|f| egui::DroppedFile::path(f.as_ref()).to_string_lossy().to_string()).filter(|p| !p.is_empty()).collect();
        if dropped.is_empty() {
            return;
        }
        // a dialog is on screen: the review would replace it, losing what it holds
        if self.ui.dialog.is_some() {
            self.toast(ctx, crate::i18n::tr("Close the open dialog, then drop the files again"));
            return;
        }
        let drop = drop_overlay::classify(dropped);
        if let Some(model) = drop.models.first()
            && let Err(e) = panels::faces::open_dialog(self, model)
        {
            self.toast(ctx, e);
        }
        if !drop.presets.is_empty() {
            // presets live in a library: with none open they would go with the session it replaces
            if panels::library_problem::holds_drops(self) {
                self.toast(ctx, crate::i18n::tr("Open a library first, then drop the presets again"));
            } else {
                let _ = self.run("file.importPresets", serde_json::json!({"paths": drop.presets}));
            }
        }
        if drop.photos.is_empty() {
            return;
        }
        // no library open yet (the launch list, a library that failed to open): they wait for one
        let Some(photos) = panels::library_problem::hold_dropped(self, ctx, drop.photos) else { return };
        if self.lightroom.is_some() {
            self.toast(ctx, crate::i18n::tr("Finish Lightroom catalog import before adding photos"));
            return;
        }
        if import::running(self) {
            self.toast(ctx, crate::i18n::tr("An import is running"));
            return;
        }
        let started = if self.ui.settings.import_dropped_directly {
            // read and added on a worker thread (dropped folders can be large, or on a slow drive)
            import::start_paths(self, photos).map(|_| ())
        } else {
            import::open(self, photos).map(|_| ())
        };
        if let Err(e) = started {
            self.toast(ctx, e);
        }
    }

    /// A file dropped by the host, kept for the next UI pass (web hosts consume their own queue).
    fn queue_dropped_files(&mut self, raw: &mut egui::RawInput) {
        if !raw.dropped_files.is_empty() {
            self.dropped_files.append(&mut raw.dropped_files);
        }
    }

    /// Take the host's dropped files once and inject synthetic events (pointer events one per
    /// frame; key sequences up to the release).
    ///
    /// The hook also runs before a hidden logic-only tick, so it injects at most one batch and
    /// keeps that batch in flight until a completed UI pass consumed it. It never injects while
    /// the fonts are not installed (the pass would draw nothing and the events would be lost).
    pub fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let file = |p: &String| (!p.is_empty()).then(|| std::path::PathBuf::from(p));
        if !self.drop_pending.is_empty() {
            raw.dropped_files = std::mem::take(&mut self.drop_pending)
                .into_iter()
                .map(|p| std::sync::Arc::new(InjectedDrop(p.into())) as egui::DroppedFileHandle)
                .collect();
        }
        if !self.drop_hover.is_empty() {
            raw.hovered_files = self.drop_hover.iter().map(|p| egui::HoveredFile { path: file(p), ..Default::default() }).collect();
        }
        // Queue after injecting, so a `ui.dropFiles` request is consumed exactly once by the UI phase.
        self.queue_dropped_files(raw);
        // Track the real host modifier state separately from the app's synthetic ownership. A host
        // change means the host now owns the modifier state: relinquish any synthetic claim and do
        // not later synthesise a release for a key that is physically held.
        if let Some(host_mods) = raw.events.iter().rev().find_map(|event| match event {
            egui::Event::ModifiersChanged(modifiers) => Some(*modifiers),
            _ => None,
        }) {
            self.host_mods = host_mods;
            self.synthetic_mods = egui::Modifiers::NONE;
            self.synthetic_mods_release = false;
        }
        // A prompt owns all input. On its first host frame this also removes pointer/touch input
        // before egui computes its hit test; later frames keep only key releases, the prompt's own
        // keys, and AccessKit actions targeted at a prompt button. See `notices::filter_raw_input`.
        let batch_in_flight = self.synthetic_in_flight;
        if crate::panels::notices::prepare_raw_input(self, ctx, raw) {
            // A prompt appearing during a synthetic drag must abort the drag, not let its remaining
            // move/release events act under the prompt or after it is dismissed. If a whole batch
            // was already injected, it is unsafe as well: this frame's prompt filter drops it
            // before a panel sees it, so it must not report a successful delivery.
            self.cancel_gestures_for_barrier(batch_in_flight, "a modal prompt appeared");
            self.queue_pointer_cleanup();
        }
        // Enforce the stall deadline before injecting another event: a request that expired during
        // a long frame gap must not get one more event on the frame that resumes.
        self.expire_gestures();
        // A canceled press is released by resetting egui's pointer state before its next input
        // pass, even while another batch is in flight or the fonts are not ready, so the pointer
        // cannot stay down. No move/release event is injected: either could start a drag or
        // become a click on the widget being canceled.
        self.apply_pointer_cleanup(ctx, raw);
        if !self.fonts_ready || self.synthetic_in_flight {
            return;
        }
        if self.drop_gestures_stale_before_injection(ctx) {
            // The stale check can cancel the head gesture while its press is already down.
            self.apply_pointer_cleanup(ctx, raw);
            return;
        }
        // the frame after a synthetic release / key: restore the host's physical modifiers, not
        // necessarily NONE (Shift may still be held while a synthetic Alt gesture is released)
        if std::mem::take(&mut self.synthetic_mods_release) && self.synthetic_mods != egui::Modifiers::NONE {
            self.synthetic_mods = egui::Modifiers::NONE;
            raw.events.push(egui::Event::ModifiersChanged(self.host_mods));
        }
        if self.synthetic.is_empty() {
            return;
        }
        let n = match self.synthetic[0] {
            egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. } => 1,
            _ => self.synthetic.iter().position(|e| matches!(e, egui::Event::Key { pressed: false, .. })).map_or(self.synthetic.len(), |i| i + 1),
        };
        // A press needs the pointer at its position before the button event; the move that is
        // itself the first event is delivered by the drain below, so it must not be sent twice.
        if let Some(egui::Event::PointerButton { pos: p, .. }) = self.synthetic.first() {
            raw.events.push(egui::Event::PointerMoved(*p));
        }
        // egui's `input.modifiers` follow `ModifiersChanged` events, not the modifiers carried by
        // pointer/key events: announce the injected events' modifiers (held from button down to
        // the frame after up), so ⌥-drag, ⇧-click … work through the control channel
        let carried = self.synthetic[..n].iter().find_map(|e| match e {
            egui::Event::PointerButton { pressed, modifiers, .. } => Some((*modifiers, !pressed)),
            egui::Event::Key { modifiers, .. } => Some((*modifiers, true)),
            _ => None,
        });
        if let Some((m, ends)) = carried {
            if m != self.synthetic_mods {
                self.synthetic_mods = m;
                raw.events.push(egui::Event::ModifiersChanged(m));
            }
            self.synthetic_mods_release = ends;
        }
        let blocked_by_prompt = self.synthetic[..n].iter().any(|event| !crate::panels::notices::prompt_allows_event(self, event));
        let head = self.head_gesture();
        if let Some(head) = head {
            let gesture = &mut self.pending_gestures[head];
            for event in &self.synthetic[..n] {
                match event {
                    egui::Event::PointerButton { button, pressed: true, .. } => {
                        gesture.pressed = Some(InjectedPress { button: *button });
                    }
                    egui::Event::PointerButton { button, pressed: false, .. }
                        if gesture.pressed.as_ref().is_some_and(|press| press.button == *button) =>
                    {
                        gesture.pressed = None;
                    }
                    _ => {}
                }
            }
            gesture.started = true;
            gesture.remaining_events = gesture.remaining_events.saturating_sub(n);
        }
        raw.events.extend(self.synthetic.drain(..n));
        // The same barrier applies after injection: only the prompt's own keys and targeted
        // AccessKit actions survive; host input and `ui.text`/clipboard never reach the background.
        crate::panels::notices::filter_raw_input(self, raw);
        if blocked_by_prompt && let Some(index) = head {
            // A settled prompt is already laid out, so this is not the prompt-creation-cancels-all
            // path: only the gesture whose batch the barrier removed fails. Allowed prompt keys,
            // pointer input and targeted AccessKit actions still report a successful delivery.
            let gesture = self.pending_gestures.remove(index);
            self.cancel_gesture_events(&gesture, true);
            let _ = gesture.reply.send(serde_json::json!({
                "ok": false,
                "error": format!("{} canceled: the open modal prompt blocked this input", gesture.method),
            }));
        }
        // The 5 s deadline is a delivery-stall limit, not a limit on a gesture's frame count: a
        // drag feeds one event per host frame, so every batch that the app consumes proves the UI
        // is alive and restarts the clock for the whole queue.
        let now = self.tick_now_ms();
        for gesture in &mut self.pending_gestures {
            gesture.deadline_ms = now + GESTURE_TIMEOUT_MS;
        }
        self.synthetic_in_flight = true;
    }
}

#[cfg(test)]
mod drop_tests {
    #[test]
    fn photo_sidecars_are_not_presets() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let dropped = v(&["/a/IMG_1.CR2", "/a/IMG_1.xmp", "/a/IMG_2.jpg", "/a/IMG_2.jpg.XMP", "/a/Look.xmp", "/b/IMG_1.xmp"]);
        let (presets, photos) = super::split_presets(dropped);
        assert_eq!(photos, v(&["/a/IMG_1.CR2", "/a/IMG_2.jpg"]));
        assert_eq!(presets, v(&["/a/Look.xmp", "/b/IMG_1.xmp"]));
    }

    #[test]
    fn detection_results_are_summarised_in_words() {
        let r =
            |counts: &[usize]| serde_json::json!({"photos": counts.iter().map(|n| serde_json::json!({"faces": vec![0; *n]})).collect::<Vec<_>>()});
        assert_eq!(super::detect_summary(&r(&[0])), "No faces found");
        assert_eq!(super::detect_summary(&r(&[1])), "Found 1 face");
        assert_eq!(super::detect_summary(&r(&[3])), "Found 3 faces");
        assert_eq!(super::detect_summary(&r(&[0, 0])), "No faces found in 2 photos");
        assert_eq!(super::detect_summary(&r(&[2, 0, 1])), "Found 3 faces in 2 photos");
        assert_eq!(super::detect_summary(&serde_json::json!({})), "No faces found in 0 photos");
    }

    #[test]
    fn dropped_face_models_are_told_apart() {
        for p in ["/a/model.onnx", "/a/dir/M.ONNX"] {
            assert!(super::is_model_file(p), "{p}");
        }
        for p in ["/a/model.onnx.jpg", "/a/onnx", "/a/b.png"] {
            assert!(!super::is_model_file(p), "{p}");
        }
    }

    #[test]
    fn dropped_presets_are_told_apart_from_photos() {
        for p in ["/a/Look.lrtemplate", "/a/b.XMP", "/a/pack.zip", "/a/x.lcpreset", "/a/Magic Hour.mplumpack", "/a/Pop.lmp", "/a/Bundle.LMP"] {
            assert!(super::is_preset_file(p), "{p}");
        }
        for p in ["/a/IMG_1.CR2", "/a/b.dng", "/a/c.jpg", "/a/folder"] {
            assert!(!super::is_preset_file(p), "{p}");
        }
    }
}
