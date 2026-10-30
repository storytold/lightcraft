use super::clock::{AppClock, monotonic_ms};
use super::services::Services;
use super::state::{GestureFrame, InjectedDrop, InjectedPress};
use super::{
    AiModel, Caches, CommandError, ControlRequest, ControlResponse, ErrorKind, GESTURE_TIMEOUT_MS, LightcraftApp, MAX_PENDING_GESTURES,
    MAX_PENDING_SHOTS, MAX_SHOT_BYTES, MAX_SHOT_TOTAL_BYTES, MAX_TEXTURE_SIDE, PendingGesture, PendingShot, Perf, Receiver, SCREEN_PROFILE,
    SCREENSHOT_FALLBACK_MS, SCREENSHOT_SETTLE_MS, SHOT_FORCE_BUSY_MS, SHOT_SETTLE_FRAMES, SHOT_SETTLE_MS, Sender, Session, ShotGeometry,
    TOAST_SECONDS, UiState, Value, docking, drop_overlay, export_task, focus_scan, frame_rate, headless, i18n, import, lightroom_import, menus,
    merge, model_setup, panels, pick, recheck, render, shortcuts, slideshow_export, state, sync, tasks, theme, updates_ui, widgets,
};

mod control;
mod frame;
mod input;
mod prefs;
mod screenshot;

#[cfg(test)]
pub(crate) use frame::import_tick_allowed;
#[cfg(test)]
pub(crate) use screenshot::shot_ready;

impl LightcraftApp {
    /// Build the app with default UI, renderer, cache and control state, and move AI segmentation
    /// to a worker so frames apply results instead of waiting.
    pub fn new(mut session: Session, services: Services) -> Self {
        // AI mask requests run on the model's worker; frames apply their results (never wait)
        session.segmenter.background = true;
        Self {
            session,
            ui: UiState::default(),
            services,
            renderer: render::Renderer::default(),
            perf: Perf::default(),
            caches: Caches::default(),
            integrated_titlebar: false,
            window_movable: None,
            native_menu: false,
            native_shortcuts: Default::default(),
            recording_shortcut: None,
            text_focus: false,
            headless_host: false,
            notices: vec![],
            updates_rt: Default::default(),
            quit_prompt: None,
            notice_input: Default::default(),
            quit_confirmed: false,
            control_rx: None,
            pending_screenshots: vec![],
            pending_gestures: vec![],
            layout_epoch: 0,
            screenshot_token: 0,
            shot_bytes: 0,
            shot_geometry_epoch: 0,
            last_shot_geometry: None,
            max_texture_side: MAX_TEXTURE_SIDE,
            clock: AppClock::new(),
            accesskit_language: None,
            slideshow_hidden_at: None,
            slideshow_auto_paused: false,
            dropped_files: vec![],
            deferred_dropped_file_count: 0,
            synthetic_in_flight: false,
            ui_passes: 0,
            #[cfg(test)]
            library_passes: vec![],
            shadow: None,
            synthetic: vec![],
            pointer_cleanup: false,
            drop_hover: vec![],
            drop_pending: vec![],
            copied: None,
            pending_picks: vec![],
            synthetic_mods: egui::Modifiers::NONE,
            synthetic_mods_release: false,
            host_mods: egui::Modifiers::NONE,
            styled: false,
            font_language: i18n::Locale::En,
            chinese_font: None,
            fonts_ready: false,
            last_time: 0.0,
            canvas_rect: None,
            image_rect: None,
            grid_scroll: None,
            film_scroll: None,
            widgets: vec![],
            widget_hits: vec![],
            next_widgets: vec![],
            next_widget_hits: vec![],
            gesture: None,
            brush_live: None,
            loupe_shown: None,
            region_view: None,
            region_before_view: None,
            size_hold: Default::default(),
            drag_scale: Default::default(),
            window_refused: None,
            region_tiles: Default::default(),
            compare_windows: [None; 2],
            compare_tiles: Default::default(),
            compare_hold_until: f64::NEG_INFINITY,
            merge: merge::MergeState::default(),
            import: None,
            scan: None,
            review_listed: None,
            sync: None,
            sync_owns_changes: false,
            sync_run: None,
            lightroom: None,
            lightroom_last: None,
            lightroom_result: None,
            review_pending: None,
            review_pending_listed: None,
            shown_library: None,
            export: None,
            tasks: Default::default(),
            last_export_result: None,
            slideshow_export_reported: true,
            hover_preview: None,
            window_is_fullscreen: false,
            gpu_applied: None,
            gpu_notice: false,
            memory_applied: None,
            display_applied: None,
            display_error: None,
            screen_profile: None,
            screen_profile_read: None,
            library_problem: None,
            library_window_input: Default::default(),
            alt_asks_for_library: false,
            recent_checks: Default::default(),
            activity_expanded: false,
            model_setup: Default::default(),
        }
    }

    /// Supply the browser's separately downloaded Simplified Chinese font before the first frame.
    pub fn with_chinese_font(mut self, bytes: Vec<u8>) -> Self {
        if !bytes.is_empty() {
            self.chinese_font = Some(std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        }
        self
    }

    /// Attach the control-channel receiver and keep CPU copies of photo textures, which headless
    /// screenshots need; returns the app for chaining.
    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        // keep CPU copies of photo textures so `ui.screenshot {"headless": true}` can draw them
        self.renderer.keep_pixels = true;
        self
    }

    /// Seconds on the app's monotonic clock. Every timer that must advance while the window is
    /// hidden (imports, autosave, playback, toasts, captures) uses this, never `i.time`.
    pub fn tick_now(&self) -> f64 {
        self.clock.now()
    }

    /// The app clock in milliseconds, for control deadlines and timers.
    fn tick_now_ms(&self) -> f64 {
        self.clock.now() * 1000.0
    }

    /// Give a headless driver its simulated time (`None` returns to the monotonic host clock).
    pub fn set_simulated_clock(&mut self, seconds: Option<f64>) {
        self.clock.simulated = seconds.filter(|s| s.is_finite() && *s >= 0.0);
    }

    /// The open person page, the way back to it, and the photo it was opened from follow a rename.
    fn follow_renamed_person(&mut self, from: &str, to: &str) {
        let same = |n: &str| n.trim().to_lowercase() == from.trim().to_lowercase();
        if self.ui.person_page.as_deref().is_some_and(same) {
            self.ui.person_page = Some(to.to_string());
        }
        if self.ui.last_person.as_deref().is_some_and(same) {
            self.ui.last_person = Some(to.to_string());
        }
        if let Some((Some(name), photo)) = self.ui.person_from.clone()
            && same(&name)
        {
            self.ui.person_from = Some((Some(to.to_string()), photo));
        }
    }

    /// Run a UI or engine command by id. The single entry point for every frontend path.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        self.run_typed(id, params).map_err(|e| e.message)
    }

    /// [`LightcraftApp::run`], keeping what kind of error a refusal is: for code that decides on it
    /// (offer a model's download, treat a cancel as one) instead of reading the message.
    pub fn run_typed(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        if let Some(result) = model_setup::intercept(self, id, &params) {
            return result.map_err(CommandError::from);
        }
        if let Some(r) = menus::run_ui_command(self, id, &params) {
            if r.is_ok() {
                docking::reveal_command(self, id)?;
            }
            return r.map_err(CommandError::from);
        }
        let params = if id == "crop.aspect"
            && params.get("aspect").and_then(Value::as_str) == Some("toggle")
            && self.crop_tool_aspect().is_some()
            && self.session.active().and_then(|id| self.session.develop_of(id)).is_some_and(|d| d.crop == lightcraft_develop::Crop::default())
        {
            serde_json::json!({"aspect": "free"})
        } else {
            params
        };
        let r = self.session.execute(id, &params).map_err(CommandError::from);
        if r.is_ok() && matches!(id, "crop.aspect" | "crop.rotateAspect") {
            self.ui.crop_default_aspect = if id == "crop.aspect" && params.get("aspect").and_then(Value::as_str) == Some("original") {
                Some(serde_json::json!("original"))
            } else {
                self.session
                    .active()
                    .and_then(|photo| self.session.develop_of(photo))
                    .map(|d| d.crop.aspect.map_or(serde_json::json!("free"), |(w, h)| serde_json::json!([w as f64 / 100.0, h as f64 / 100.0])))
            };
        }
        if r.is_ok() && id == "library.toggleFilters" {
            // the search field shows the text filter: empty while the filters are off, back with them
            self.ui.search = self.session.filter.text.clone();
        }
        if r.is_ok() && id == "library.clearFilter" {
            // the search field shows the text filter: cleared from anywhere (the loupe's pill, the
            // menu), it must not keep a search that no longer applies (issue #1648)
            self.ui.search.clear();
        }
        if r.is_ok() && id == "mask.adjust" {
            // Judge local adjustments on the photo, without the selection overlay obscuring them.
            // Keep it hidden after release; O / the overlay eye can show it again.
            self.ui.mask_overlay = false;
        }
        if r.is_ok() && matches!(id, "mask.add" | "mask.addComponent" | "mask.duplicate" | "mask.select") {
            // A new or newly picked mask shows its overlay again, as in Lightroom: hidden by a slider
            // drag above, it stayed hidden while the Overlay menu still named a mode (issue #1800).
            self.ui.mask_overlay = true;
        }
        if r.is_ok() {
            // These commands finish synchronously, so their completion would otherwise
            // be invisible once the activity stack is gone. Never show success on error.
            let completed = match id {
                "library.compact" => Some(format!("✓ {}", i18n::tr("Optimize Library"))),
                "library.clearPreviews" => Some(format!("✓ {}", i18n::tr("Discard Preview Cache"))),
                "develop.autoSync" => Some(format!("{}: {}", i18n::tr("Auto Sync"), i18n::tr(if self.session.auto_sync { "On" } else { "Off" }))),
                _ => None,
            };
            if let Some(message) = completed {
                let now = self.tick_now();
                self.ui.toast = Some((message, now + TOAST_SECONDS, None));
            }
        }
        if r.is_ok() && id == "photo.label" {
            let label = params.get("label").and_then(Value::as_str).and_then(lightcraft_catalog::ColorLabel::parse);
            let text = match label {
                Some(label) => {
                    let name =
                        self.session.catalog.custom_label_name(label).map(str::to_owned).unwrap_or_else(|| i18n::tr(&format!("{label:?}")).into());
                    i18n::tr_format!("{name} Label", name = name)
                }
                None => i18n::tr("Color label cleared").into(),
            };
            let now = self.tick_now();
            self.ui.toast = Some((text, now + TOAST_SECONDS, label));
        }
        match &r {
            Err(e) => {
                log::warn!("{id}: {e}");
                self.ui.status = e.message.clone();
                // no detector yet: offer it (its terms first) instead of only saying so
                if id == "faces.detect" && e.kind == ErrorKind::ModelMissing(AiModel::FaceDetector) {
                    panels::faces::offer_detector(self);
                }
            }
            Ok(v) if id == "faces.detect" => self.ui.status = detect_summary(v),
            Ok(v) if id == "faces.renamePerson" => {
                let to = v["to"].as_str().unwrap_or("").to_string();
                let from = params.get("from").and_then(Value::as_str).unwrap_or("");
                if !to.is_empty() {
                    self.follow_renamed_person(from, &to);
                }
            }
            Ok(_) => {}
        }
        r
    }

    /// The remembered ratio belongs to the tool until the user starts editing the crop.
    pub(crate) fn crop_tool_aspect(&self) -> Option<(u32, u32)> {
        let photo = self.session.active()?;
        let d = self.session.develop_of(photo)?;
        if d.crop != lightcraft_develop::Crop::default() {
            return d.crop.aspect;
        }
        // nothing chosen yet: locked to the photo's own shape, as Lightroom starts (#991); one
        // Lock toggle (remembered as "free") unlocks it for good
        let remembered = self.ui.crop_default_aspect.clone().unwrap_or_else(|| Value::String("original".into()));
        match &remembered {
            Value::String(s) if s == "original" => {
                let p = self.session.catalog.photo(photo)?;
                let (w, h) = (p.width.max(1).saturating_mul(100), p.height.max(1).saturating_mul(100));
                Some(if d.orientation.swaps_axes() { (h, w) } else { (w, h) })
            }
            Value::Array(a) => {
                let number = |i: usize| {
                    let v = a.get(i)?.as_f64()? * 100.0;
                    (v.is_finite() && v >= 1.0 && v <= f64::from(u32::MAX)).then_some(v.round() as u32)
                };
                Some((number(0)?, number(1)?))
            }
            _ => None,
        }
    }

    /// Called only after a crop gesture begins, inside its undo transaction.
    pub(crate) fn begin_crop(&mut self) -> Result<(), String> {
        // nothing chosen yet: the photo's own shape, as `crop_tool_aspect` shows it
        let aspect = self.ui.crop_default_aspect.clone().unwrap_or_else(|| serde_json::json!("original"));
        if self.session.interaction.is_some()
            && self.session.active().and_then(|id| self.session.develop_of(id)).is_some_and(|d| d.crop == lightcraft_develop::Crop::default())
            && aspect != serde_json::json!("free")
            // the remembered ratio, turned like the new photo's crop (as a named ratio is)
            && let Err(e) = self.session.execute("crop.aspect", &serde_json::json!({"aspect": aspect, "orientation": "crop"}))
        {
            // A remembered value the command refuses (a hand-edited settings file) would refuse
            // every crop drag on an untouched photo: forget it and let this drag go ahead.
            self.ui.crop_default_aspect = None;
            self.ui.status = e.to_string();
        }
        Ok(())
    }

    /// Advance a running slideshow, but not while the window is hidden: in 0.36 `i.time` froze
    /// there, and a minimized show must not keep waking the event loop or requesting full-size
    /// slide renders overnight. Playback is auto-paused on hide and resumed on unhide with the
    /// slide's remaining time preserved.
    fn slideshow_tick(&mut self, ctx: &egui::Context, now: f64) {
        let hidden = ctx.input(|i| i.viewport().visible() == Some(false));
        let Some(run) = self.ui.slideshow.as_mut() else {
            self.slideshow_hidden_at = None;
            self.slideshow_auto_paused = false;
            return;
        };
        if hidden {
            if self.slideshow_hidden_at.is_none() {
                self.slideshow_hidden_at = Some(now);
                if !run.play.is_paused() {
                    run.play.toggle_pause(now);
                    self.slideshow_auto_paused = true;
                }
            }
            return;
        }
        if self.slideshow_hidden_at.take().is_some() && self.slideshow_auto_paused {
            self.slideshow_auto_paused = false;
            if run.play.is_paused() {
                run.play.toggle_pause(now);
            }
        }
        panels::slideshow::tick(self, ctx, now);
    }

    /// Announce the end of a Build Previews run.
    fn preview_build_status(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let Some(b) = self.session.preview_build.clone() else { return };
        let key = std::sync::Arc::as_ptr(&b) as usize;
        if b.finished.load(Ordering::Relaxed) {
            if self.ui.preview_build_seen != Some((key, true)) {
                self.ui.preview_build_seen = Some((key, true));
                let (done, failed) = (b.done.load(Ordering::Relaxed), b.failed.load(Ordering::Relaxed));
                let plural = if done == 1 { "" } else { "s" };
                let mut msg = match (b.error(), b.what) {
                    (Some(e), what) => format!("{}: {e}", if what.is_empty() { "previews" } else { what }),
                    (None, "") => crate::i18n::tr_format!("Previews ready for {done} photo{plural}", done = done, plural = plural),
                    (None, what) => {
                        crate::i18n::tr_format!("Done ({what}): {done} photo{plural}", done = done, plural = plural, what = crate::i18n::tr(what))
                    }
                };
                if failed > 0 {
                    msg.push_str(&crate::i18n::tr_format!(" · {failed} couldn't be rendered", failed = failed));
                }
                self.toast(ctx, msg);
            }
        } else {
            // The activity stack shows the run while it works; poll for the end.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Announce when saving the library starts failing (changes then live only in memory and are
    /// retried) and when it works again; the top bar's cloud icon shows the state meanwhile.
    fn save_status(&mut self, ctx: &egui::Context) {
        let unsaved = self.session.unsaved().map(|(n, e)| (n, e.to_string()));
        match (unsaved, self.ui.unsaved_seen) {
            (Some((n, e)), false) => {
                self.ui.unsaved_seen = true;
                let t = self.tick_now();
                self.ui.toast = Some((
                    crate::i18n::tr_format!(
                        "{n} change{} saved in memory but not written to disk: {e} — LightCraft will retry",
                        if n == 1 { "" } else { "s" },
                        n = n,
                        e = e
                    ),
                    t + 6.0,
                    None,
                ));
            }
            (None, true) => {
                self.ui.unsaved_seen = false;
                self.toast(ctx, crate::i18n::tr("Library saved"));
            }
            (Some(_), true) => ctx.request_repaint_after(std::time::Duration::from_secs(2)), // retry
            (None, false) => {}
        }
    }

    pub fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        self.toast_for(ctx, text, TOAST_SECONDS);
    }

    /// Auto straighten (the crop panel's Auto, a double-click with the Straighten tool), saying so
    /// when it finds nothing to level by instead of leaving the photo as it is without a word.
    pub fn auto_straighten(&mut self, ctx: &egui::Context) {
        match self.run("crop.autoStraighten", serde_json::json!({})) {
            Ok(r) if r.get("changed").and_then(Value::as_bool) == Some(false) => {
                self.toast(ctx, crate::i18n::tr("No horizon or vertical lines to level by"));
            }
            Ok(_) => {}
            Err(e) => self.toast(ctx, e),
        }
    }

    /// A toast that stays `secs` seconds (messages that say where to look or what to do next).
    pub fn toast_for(&mut self, _ctx: &egui::Context, text: impl Into<String>, secs: f64) {
        let t = self.tick_now();
        self.ui.toast = Some((text.into(), t + secs, None));
    }

    /// Reverse geocoding: say how a place-names download ended (a failure stays readable, with what to do).
    fn place_names_outcome(&mut self, ctx: &egui::Context) {
        use lightcraft_engine::cmd::geocode::DownloadOutcome;
        match self.session.place_names.take_outcome() {
            Some(DownloadOutcome::Done(_)) => self.toast(ctx, crate::i18n::tr("Place names downloaded")),
            Some(DownloadOutcome::Cancelled) => self.toast(ctx, crate::i18n::tr("Place names download cancelled")),
            Some(DownloadOutcome::Failed(e)) => self.toast_error(ctx, e),
            None if self.session.place_names.downloading() => ctx.request_repaint_after(std::time::Duration::from_millis(500)),
            None => {}
        }
    }

    /// AI masks: apply finished background requests (clicks, descriptions, detail passes) and
    /// show their errors; start a zoomed-in detail pass once the clicking stops
    /// (`ui.detail_due`); watch the model download. Never waits for the model.
    fn ai_mask_detail(&mut self, ctx: &egui::Context, now: f64) {
        let polled = self.session.segment_poll();
        if polled.changed {
            ctx.request_repaint();
        }
        if let Some(mask) = polled.refine {
            // a new selection from a click or a description: show it, as `run` does for new masks
            self.ui.mask_overlay = true;
            self.ui.detail_due = Some((now + 0.3, mask));
        }
        if let Some(e) = polled.messages.into_iter().last() {
            self.ai_error(ctx, e, None);
        }
        let seg = &self.session.segmenter;
        if seg.busy() || seg.pending_clicks().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let download = seg.download_status();
        if download.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // a download finishing while its dialog is closed: say so once
        if self.ui.sam_downloading && !download.running {
            self.ui.sam_downloading = false;
            let open = matches!(self.ui.dialog, Some(state::Dialog::SamModel { .. }));
            match sam_download_ended(&download) {
                _ if open => {}
                Some((text, true)) => self.toast_error(ctx, text),
                Some((text, false)) => self.toast(ctx, text),
                None => {}
            }
        }
        if let Some((due, mask)) = self.ui.detail_due {
            if now >= due && !self.session.segmenter.busy() && !self.session.segmenter.detail_busy() {
                self.ui.detail_due = None;
                if let Err(e) = self.run("mask.refineDetail", serde_json::json!({"id": mask})) {
                    log::warn!("detail pass: {e}");
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
            }
        }
        if self.session.segmenter.detail_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// An AI mask error: when the model isn't installed, the dialog that offers to download it
    /// (`then`: the AI mask to start afterwards); otherwise a toast. Decided on the error's kind
    /// ([`ErrorKind::ModelMissing`]), not its words.
    pub fn ai_error(&mut self, ctx: &egui::Context, e: impl Into<CommandError>, then: Option<(&str, &str)>) {
        let e = e.into();
        if lightcraft_engine::segment::Segmenter::AVAILABLE && e.kind == ErrorKind::ModelMissing(AiModel::Sam3) {
            self.offer_sam_download(then);
        } else {
            self.toast_error(ctx, e.message);
        }
    }

    /// Open the dialog offering the SAM 3 download (never downloads by itself).
    pub fn offer_sam_download(&mut self, then: Option<(&str, &str)>) {
        if self.ui.dialog.is_none() || matches!(self.ui.dialog, Some(state::Dialog::SamModel { .. })) {
            self.ui.dialog = Some(state::Dialog::SamModel { then: then.map(|(k, o)| (k.to_string(), o.to_string())), error: None });
        }
    }

    /// A toast for an error the user has to read and act on (stays 6 s).
    pub fn toast_error(&mut self, _ctx: &egui::Context, text: impl Into<String>) {
        let t = self.tick_now();
        self.ui.toast = Some((text.into(), t + 6.0, None));
    }

    /// The process is about to end (settings and library are saved by then): make sure nothing is
    /// inside the GPU driver when it does (issue #620: a render running on a worker thread while
    /// the process exits crashes in the driver). No GPU work starts from here on; the render
    /// workers get up to `timeout` to finish their jobs, and what is left of it goes to GPU work
    /// on other threads (export, denoise, the device warm-up). `false`: the deadline passed first
    /// (the host exits anyway). The app renders nothing afterwards.
    pub fn shutdown(&mut self, timeout: std::time::Duration) -> bool {
        lightcraft_engine::gpu::begin_shutdown();
        #[cfg(not(target_arch = "wasm32"))]
        let t0 = std::time::Instant::now();
        let stopped = self.renderer.shutdown(timeout);
        #[cfg(not(target_arch = "wasm32"))]
        let timeout = timeout.saturating_sub(t0.elapsed());
        let idle = lightcraft_engine::gpu::wait_idle(timeout);
        stopped && idle
    }

    /// Background and queue work. Called before layout on every host pass and, while the window is
    /// hidden, from `Context::run_logic`. It may read the refreshed viewport fields only: never
    /// `i.time`, events, keys or dropped files.
    pub fn logic(&mut self, ctx: &egui::Context) {
        i18n::set_language(self.ui.language);
        // The fork's AccessKit root language and bidi runs follow the locale; presentation only.
        // Compare against the cached string: `Context::language()` allocates on every call, and
        // this runs on every UI pass and every hidden logic tick.
        let language = self.ui.language.code();
        if self.accesskit_language.as_deref() != Some(language) {
            ctx.set_language(Some(language.to_owned()));
            self.accesskit_language = Some(language.to_owned());
        }
        let t0 = monotonic_ms();
        self.note_max_texture_side(ctx);
        self.note_shot_geometry(ctx);
        self.ensure_fonts(ctx);
        // Applying the theme belongs to logic: a host that only calls `logic` (and every hidden
        // tick) must see an Appearance Mode / system-theme change, and the UI pass that follows
        // then draws with the new visuals. `ui` keeps its own call for a settings change made by
        // a command during the same pass; it is a no-op when the theme already matches.
        self.sync_theme(ctx);
        self.logic_background(ctx);
        self.perf.logic_ms = monotonic_ms() - t0;
    }

    /// Frame timings once layout is done (`t0`: when layout started).
    fn end_frame(&mut self, ctx: &egui::Context, t0: f64) {
        if self.ui.frame_rate {
            frame_rate::show(ctx, self);
        } else {
            frame_rate::forget(ctx);
        }
        panels::edit::wheel_pointer_frame_end(ctx);
        ctx.output(|o| {
            for c in &o.commands {
                if let egui::OutputCommand::CopyText(text) = c {
                    self.copied = Some(text.clone());
                }
            }
        });
        self.perf.frame_ms = monotonic_ms() - t0;
        self.perf.update_ms = self.perf.logic_ms + self.perf.frame_ms;
        self.perf.max_update_ms = self.perf.max_update_ms.max(self.perf.update_ms);
    }

    /// Lay out the whole window: input/setup for the known UI phase, then presentation, then the
    /// frame bookkeeping. A hidden `run_logic` never reaches this.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        i18n::set_language(self.ui.language);
        let ctx = ui.ctx().clone();
        if !self.fonts_ready {
            ctx.request_repaint();
            return;
        }
        self.sync_theme(&ctx);
        self.ui_pass(&ctx);
        self.draw_window(ui);
        self.finish_ui_pass(&ctx);
    }
}

/// "Found 3 faces in 2 photos" for a `faces.detect` result.
pub(crate) fn detect_summary(v: &Value) -> String {
    let photos = v["photos"].as_array().map(Vec::as_slice).unwrap_or_default();
    let counts: Vec<usize> = photos.iter().map(|p| p["faces"].as_array().map_or(0, Vec::len)).collect();
    let (faces, with) = (counts.iter().sum::<usize>(), counts.iter().filter(|n| **n > 0).count());
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    match (faces, photos.len()) {
        (0, 1) => "No faces found".into(),
        (0, n) => format!("No faces found in {}", plural(n, "photo")),
        (f, 1) => format!("Found {}", plural(f, "face")),
        (f, _) => format!("Found {} in {}", plural(f, "face"), plural(with, "photo")),
    }
}

/// What to say when a SAM 3 download ended while its dialog was closed: the text, and whether it is
/// shown as an error toast. A cancel is told by `cancelled`, not by the error's words.
pub(crate) fn sam_download_ended(download: &lightcraft_engine::segment::DownloadStatus) -> Option<(String, bool)> {
    match &download.error {
        Some(_) if download.cancelled => {
            Some((crate::i18n::tr("SAM 3 download stopped: it resumes where it left off next time.").to_string(), false))
        }
        Some(e) => Some((crate::i18n::tr_format!("The SAM 3 download failed: {e}", e = e), true)),
        None if download.finished => Some((crate::i18n::tr("The SAM 3 model is installed: Object and Describe masks are ready.").to_string(), true)),
        None => None,
    }
}
