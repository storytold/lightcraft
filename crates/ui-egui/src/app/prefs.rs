use super::{LightcraftApp, SCREEN_PROFILE, theme};

impl LightcraftApp {
    /// The system's appearance: the host's answer first, then egui's (none: unknown).
    pub fn system_theme(&self, ctx: &egui::Context) -> Option<egui::Theme> {
        self.services.system_theme.as_ref().and_then(|read| read(ctx)).or_else(|| ctx.system_theme())
    }

    /// Restyle `ctx` when the theme the settings select (Appearance Mode, the dark and light
    /// themes, the system's appearance in Auto) is not the one it shows. A no-op otherwise.
    pub fn sync_theme(&mut self, ctx: &egui::Context) {
        let wanted = self.ui.settings.theme(self.system_theme(ctx));
        if theme::current(ctx) != Some(wanted) {
            theme::apply_kind(ctx, wanted);
        }
    }

    /// Back from an external editor: pick up the files it saved. Reads `i.focused`, which
    /// `run_logic` does not refresh, so it belongs to the UI phase.
    pub(super) fn check_external_editor(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        if focused && !self.ui.was_focused && !self.ui.external_edits.is_empty() {
            let ids = self.ui.external_edits.clone();
            if let Ok(r) = self.session.execute("photo.reload", &serde_json::json!({"ids": ids}))
                && r["reloaded"].as_array().is_some_and(|a| !a.is_empty())
            {
                self.toast(ctx, crate::i18n::tr("Updated the edits saved in the external editor"));
            }
        }
        self.ui.was_focused = focused;
    }

    /// The window and the GPU's batch work (exports): a resize makes room for the window's own
    /// drawing first (`lightcraft_gpu::hold_batch`: the surface is reconfigured, which waits for
    /// the GPU); what happened to the GPU (it stopped responding, it was restarted) is told once.
    fn gpu_housekeeping(&mut self, ctx: &egui::Context) {
        let screen = ctx.content_rect().size();
        if screen != self.last_screen {
            if self.last_screen != egui::Vec2::ZERO {
                lightcraft_engine::gpu::hold_batch(std::time::Duration::from_millis(800));
            }
            self.last_screen = screen;
        }
        // input this frame: batch work leaves the editing card to the interface for a moment (§8)
        if ctx.input(|i| !i.events.is_empty() || i.pointer.any_down()) {
            lightcraft_engine::gpu::note_interaction();
        }
        #[cfg(not(target_arch = "wasm32"))]
        for e in lightcraft_engine::gpu::events_after(self.gpu_event_seen) {
            self.gpu_event_seen = self.gpu_event_seen.max(e.seq);
            let text = crate::i18n::tr(&e.text).to_string();
            match e.kind {
                lightcraft_engine::gpu::EventKind::Recovered => self.toast_for(ctx, text, 8.0),
                _ => self.toast_for(ctx, text, 15.0),
            }
        }
    }

    /// Apply settings that act outside the UI state: GPU rendering and window full screen.
    pub(super) fn apply_settings(&mut self, ctx: &egui::Context) {
        // the second graphics card (Settings ▸ Performance), before the warm-up below sets it up
        if self.gpu_helper_applied.as_deref() != Some(self.ui.settings.gpu_helper.as_str()) {
            self.gpu_helper_applied = Some(self.ui.settings.gpu_helper.clone());
            let helper = if self.ui.settings.gpu_helper.is_empty() { "auto".to_string() } else { self.ui.settings.gpu_helper.clone() };
            if let Err(e) = self.session.execute("app.gpu", &serde_json::json!({"helper": helper, "wait": false})) {
                log::warn!("second graphics card: {e}");
            }
        }
        if self.gpu_applied != Some(self.ui.settings.gpu) {
            self.gpu_applied = Some(self.ui.settings.gpu);
            let _ = self.session.execute("app.gpu", &serde_json::json!({"enabled": self.ui.settings.gpu, "wait": false}));
            // GPU device + kernels off the UI thread, once the window is up and only when GPU
            // rendering is on: a broken driver must not keep the window from appearing (issue #136)
            if self.ui.settings.gpu {
                lightcraft_engine::gpu::warm_up();
            }
        }
        // the graphics card for editing (Settings ▸ Performance): applied at once, renders move over when it is ready
        if self.gpu_card_applied.as_deref() != Some(self.ui.settings.gpu_card.as_str()) {
            self.gpu_card_applied = Some(self.ui.settings.gpu_card.clone());
            let card = if self.ui.settings.gpu_card.is_empty() { "auto".to_string() } else { self.ui.settings.gpu_card.clone() };
            if let Err(e) = self.session.execute("app.gpu", &serde_json::json!({"card": card, "wait": false})) {
                log::warn!("graphics card for editing: {e}");
            }
        }
        let mb = self.ui.settings.memory_mb;
        // automatic at startup: leave the engine's default alone
        if self.memory_applied != Some(mb) && (mb > 0 || self.memory_applied.is_some()) {
            let mb = if mb == 0 { (lightcraft_engine::memory::default_budget() >> 20) as u32 } else { mb };
            let _ = self.session.execute("app.memoryBudget", &serde_json::json!({"mb": mb}));
        }
        self.memory_applied = Some(mb);
        // export lanes and the GPU's share for exports (Settings ▸ Performance, "Test my machine")
        let st = &self.ui.settings;
        let tuning = crate::machine_test::tuning(st);
        if self.export_tuning_applied != Some(tuning) {
            crate::machine_test::apply(st);
            self.export_tuning_applied = Some(tuning);
        }
        self.gpu_housekeeping(ctx);
        // the monitor profile (Settings ▸ Display): previews follow it from the next frame
        if self.display_applied.as_ref() != Some(&self.ui.settings.display_profile) {
            let path = self.ui.settings.display_profile.clone();
            match lightcraft_engine::display::Display::load_opt(&path) {
                Ok(d) => {
                    self.renderer.set_display(d);
                    self.display_error = None;
                }
                Err(msg) => {
                    // a profile that can't be used: previews are sRGB, the setting is kept
                    self.renderer.set_display(None);
                    log::warn!("display profile: {msg}");
                    self.toast_error(ctx, format!("{}: {msg}", crate::i18n::tr("Display profile not used")));
                    self.display_error = Some(msg);
                }
            }
            self.display_applied = Some(path);
            // none chosen again: the screen's profile is read afresh
            self.screen_profile = None;
            self.screen_profile_read = None;
        }
        if self.ui.settings.display_profile.is_empty() {
            self.follow_screen_profile(ctx);
        }
        if let Some(fs) = ctx.input(|i| i.viewport().fullscreen) {
            self.window_is_fullscreen = fs;
        }
        if let Some(on) = self.ui.window_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
            self.window_is_fullscreen = on;
        }
    }

    /// While no display profile is chosen, show previews through the profile of the screen the
    /// window is on (`services.screen_profile`), read about once a second so moving the window to
    /// another screen is followed. Without the service (or a profile) the display is sRGB.
    fn follow_screen_profile(&mut self, _ctx: &egui::Context) {
        let now = self.tick_now();
        if self.services.screen_profile.is_none() || self.screen_profile_read.is_some_and(|t| (0.0..1.0).contains(&(now - t))) {
            return;
        }
        self.screen_profile_read = Some(now);
        self.apply_screen_profile();
    }

    /// Read the screen's profile now and show previews through it (sRGB without one).
    pub(crate) fn apply_screen_profile(&mut self) {
        let Some(read) = self.services.screen_profile.as_mut() else { return };
        let icc = read();
        if icc == self.screen_profile {
            return;
        }
        let display = icc.as_deref().and_then(|bytes| match lightcraft_engine::display::Display::from_bytes(SCREEN_PROFILE, bytes) {
            Ok(d) => Some(std::sync::Arc::new(d)),
            Err(msg) => {
                // a profile we can't use: previews stay sRGB, as before #787
                log::warn!("screen profile: {msg}");
                None
            }
        });
        self.renderer.set_display(display);
        self.screen_profile = icc;
    }
}
