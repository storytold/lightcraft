use super::{
    LightcraftApp, docking, drop_overlay, export_task, focus_scan, import, lightroom_import, merge, model_setup, monotonic_ms, panels, pick, recheck,
    shortcuts, slideshow_export, state, sync, tasks, theme, updates_ui, widgets,
};

impl LightcraftApp {
    /// Install/refresh fonts once and watch the device and availability caches. Setup, not input.
    pub(super) fn ensure_fonts(&mut self, ctx: &egui::Context) {
        if !self.styled {
            theme::install_fonts_with_chinese(ctx, self.chinese_font.as_ref());
            theme::apply(ctx);
            Self::drop_legacy_panel_state(ctx);
            // File → Add from Device lists cards scanned in the background: show hot-plugs
            let repaint = ctx.clone();
            lightcraft_engine::devices::on_change(move || repaint.request_repaint());
            // "is the original there?" (grid, Info panel, Missing Photos) answers from a cache a
            // worker fills: a sleeping NAS or a dropped share never stalls a frame
            let repaint = ctx.clone();
            self.session.media.availability.run_in_background(std::sync::Arc::new(move || repaint.request_repaint()));
            self.styled = true;
            self.font_language = self.ui.language;
        } else if self.font_language != self.ui.language {
            // Shared Han characters take the active language's forms (Japanese faces for 日本語,
            // the Simplified Chinese face for 简体中文): rebuild the fallback order.
            theme::install_fonts_with_chinese(ctx, self.chinese_font.as_ref());
            self.font_language = self.ui.language;
        } else {
            self.fonts_ready = true;
        }
    }

    /// 0.36 stored each `PanelState` under the panel's exact `Id`; 0.37 stores it under
    /// `parent_ui.scope_id().with_salt(id_salt)`, so the old entries are unreachable and the
    /// panels reset once by simply not finding them. Remove the old keys anyway so egui memory
    /// does not retain dead entries (no document, workspace JSON, catalog or XMP data is
    /// migrated; nothing above a user's photo edits depends on them).
    fn drop_legacy_panel_state(ctx: &egui::Context) {
        for salt in ["top_bar", "bottom_bar", "tool_strip", "library-problem-banner"] {
            ctx.data_mut(|d| d.remove::<egui::containers::panel::PanelState>(egui::Id::unique(salt)));
        }
    }

    /// Queues and timers. Each consumed queue is drained once per call; time-gated work uses the
    /// monotonic app clock, so a hidden `run_logic` keeps it advancing and a second layout pass is
    /// a no-op rather than a duplicate.
    pub(super) fn logic_background(&mut self, ctx: &egui::Context) {
        panels::notices::logic(self);
        // closing the window (or Quit) with changes only in memory: retry, else ask first
        if ctx.input(|i| i.viewport().close_requested()) && !panels::notices::may_close(self) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let now = self.tick_now();
        self.drain_control(ctx);
        self.apply_settings(ctx);
        if self.queued_input_needs_repaint() {
            ctx.request_repaint();
        }
        self.renderer.poll(ctx, &mut self.session);
        merge::poll(self, ctx);
        import::poll_scan(self, ctx);
        sync::poll(self, ctx);
        sync::poll_run(self, ctx);
        // Import commits batches and must not run twice in one host frame. The first layout pass
        // and a hidden logic tick both report pass index 0; a second pass reports 1. A wall-clock
        // gate cannot express that (a slow discarded first pass would re-run it).
        if import_tick_allowed(ctx) {
            import::tick(self, ctx);
        }
        lightroom_import::tick(self, ctx);
        recheck::tick(self);
        tasks::poll(self, ctx);
        updates_ui::tick(self, ctx);
        self.preview_build_status(ctx);
        self.save_status(ctx);
        self.ai_mask_detail(ctx, now);
        self.place_names_outcome(ctx);
        self.slideshow_tick(ctx, now);
        panels::denoise::pump_background(self, ctx, now);
        panels::faces::pump_background(self, ctx, now);
        model_setup::pump(self, ctx, now);
        focus_scan::pump(self, ctx, now);
        // auto import: list the watched folder every few seconds (on a worker thread: it may be on
        // a network share), then import what's new like any import (also on a worker thread)
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(folder) = self.session.import_defaults.auto_folder.clone() {
            const LABEL: &str = "Auto Import";
            if now - self.ui.auto_import_at >= 3.0
                && self.import.is_none()
                && self.sync_run.is_none()
                && self.lightroom.is_none()
                && !self.tasks.is_running(LABEL)
            {
                self.ui.auto_import_at = now;
                let work = move || lightcraft_engine::cmd::library::list_auto_import_folder(&folder);
                let done = |app: &mut LightcraftApp, _ctx: &egui::Context, listing: Result<Vec<(String, u64)>, String>| {
                    let listing = match listing {
                        Ok(l) => l,
                        Err(e) => return log::debug!("auto import: {e}"),
                    };
                    let p = serde_json::json!({"listing": listing, "start": false});
                    let Ok(r) = app.session.execute("library.autoImportScan", &p) else { return };
                    let Some(mut params) = r.get("import").cloned().filter(|_| app.import.is_none()) else { return };
                    let paths: Vec<String> =
                        params["paths"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
                    if let Some(o) = params.as_object_mut() {
                        o.remove("paths");
                    }
                    let undo0 = app.session.undo.len();
                    app.import = Some(import::ImportTask::new(paths, params, undo0, false).auto());
                };
                // listed every few seconds: quiet, no row in the activity stack
                if let Err(e) = tasks::spawn(self, LABEL, None, work, done) {
                    log::warn!("{e}");
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(3));
        }
        // automatic XMP writes that failed (a network share offline): tried again every few seconds once their files
        // are reachable, and said once when they start waiting and once when they're written
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.session.unsaved_sidecars() > 0 {
                if now - self.ui.sidecar_retry_at >= 10.0 {
                    self.ui.sidecar_retry_at = now;
                    self.session.retry_unsaved_sidecars();
                }
                ctx.request_repaint_after(std::time::Duration::from_secs(10));
            }
            let waiting = self.session.unsaved_sidecars();
            if waiting > 0 && self.ui.sidecars_waiting == 0 {
                self.toast_for(
                    ctx,
                    crate::i18n::tr_format!(
                        "The XMP sidecars of {n} photos couldn't be written (is their folder offline?). LightCraft keeps trying.",
                        n = waiting
                    ),
                    8.0,
                );
            } else if waiting == 0 && self.ui.sidecars_waiting > 0 {
                self.toast(ctx, crate::i18n::tr("Wrote the XMP sidecars that were waiting"));
            }
            self.ui.sidecars_waiting = waiting;
        }
        // Settings → Import: open the import review when a camera or card is connected
        #[cfg(not(target_arch = "wasm32"))]
        if self.session.import_defaults.show_on_card {
            if let Some(now) = lightcraft_engine::devices::devices_scanned() {
                import::cards_seen(self, now);
            }
            ctx.request_repaint_after(lightcraft_engine::devices::MAX_AGE);
        } else {
            self.ui.known_cards = None;
        }
        self.session.persist_if_dirty();
        self.expire_gestures();
        self.collect_screenshots(ctx);
        self.issue_screenshots(ctx);
        self.request_paint_while_hidden(ctx);
    }

    /// While hidden, queued captures and an injected gesture step need a real UI pass; a pending
    /// screenshot command paints by itself once issued.
    fn request_paint_while_hidden(&self, ctx: &egui::Context) {
        // `logic` runs per layout pass; one command per host frame is enough.
        if ctx.current_pass_index() != 0 {
            return;
        }
        // `ViewportInfo::visible()`: minimized or fully occluded is `Some(false)`; `None`
        // (headless, `RawInput::default()`) is normal and must not be treated as hidden.
        let hidden = ctx.input(|i| i.viewport().visible() == Some(false));
        if hidden && self.needs_ui_pass_while_hidden() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
    }

    /// A hidden window needs a real UI pass for queued/settling captures, for control gestures
    /// that still have to resolve or deliver, and for an injected synthetic step. An already
    /// issued screenshot has its own compositor-driven paint and must not request another
    /// `RequestPaintWhileHidden`; its fallback and timeout keep running from `logic`.
    pub(crate) fn needs_ui_pass_while_hidden(&self) -> bool {
        !self.pending_gestures.is_empty()
            || self.pointer_cleanup
            || self.pending_screenshots.iter().any(|shot| shot.sent_at_ms.is_none())
            || !self.synthetic.is_empty()
            || self.synthetic_in_flight
    }

    /// Queued synthetic input, including a queued pointer cancellation that is not part of
    /// `synthetic`, needs the next visible frame even when an earlier stall left the synthetic
    /// queue empty.
    pub(crate) fn queued_input_needs_repaint(&self) -> bool {
        !self.synthetic.is_empty() || self.pointer_cleanup
    }

    /// Input, layout and setup for a known UI phase: at most once per host frame, and never from a
    /// hidden logic tick. `current_pass_index() == 0` is the first pass of this `run_ui`; a second
    /// pass has empty events.
    pub(super) fn ui_pass(&mut self, ctx: &egui::Context) {
        if ctx.current_pass_index() != 0 {
            return;
        }
        // A prompt owns the keyboard and focus. A notice may have been added by this frame's logic
        // after `raw_input_hook`; capture its Tab/Enter/Space/Escape and drop everything else
        // before any panel, focused control or shortcut reads the input. A new prompt gets a
        // shielding pass whose background is not drawn at all (see `draw_window`).
        let batch_in_flight = self.synthetic_in_flight;
        if panels::notices::prepare_ui_pass(self, ctx) {
            self.cancel_gestures_for_barrier(batch_in_flight, "a modal prompt appeared");
        }
        panels::library_problem::prepare_ui_pass(self, ctx, batch_in_flight);
        let now = self.tick_now();
        // the last pass's time is fine here: this is a UI phase, not a hidden tick
        let t = ctx.input(|i| i.time);
        let dt = t - self.last_time;
        if dt > 0.0 {
            self.perf.fps = self.perf.fps * 0.9 + (1.0 / dt).min(240.0) * 0.1;
        }
        self.last_time = t;
        panels::denoise::pump_ui_input(self, ctx, now);
        panels::faces::pump_ui_input(self, ctx, now);
        // A prompt or the blocking library window owns the library too: files dropped while either
        // is up wait in the queue and are imported on the first UI pass after it goes away, never
        // behind it. With no library open the window's `hold_dropped` path still takes the photos
        // once its own dialog and asynchronous library picker are gone, so `blocks_file_drops`
        // only holds them back until then. Mark those drops: once the barrier opens they may find
        // a dialog, Lightroom import or import that the library's own open started still running.
        if panels::notices::blocks_input(self) || panels::library_problem::blocks_file_drops(self) {
            self.defer_dropped_files();
        } else {
            self.import_dropped_files(ctx);
        }
        self.check_external_editor(ctx);
        // Recent-library checks and launch-list keys read focus/time/keys: they belong to the UI
        // phase, not the hidden-capable `logic` tick (plan §4.5 / migration input contract).
        panels::library_problem::logic(self, ctx);
        if self.fonts_ready && !updates_ui::blocks_input(self) {
            shortcuts::handle(self, ctx);
        }
    }

    /// Lights Out: veil everything but the photo (the loupe's image, or the grid / Compare /
    /// Survey area) in black. Painted above the panels, below dialogs and notices.
    fn lights_out(&self, ctx: &egui::Context) {
        let a = self.ui.lights_out.veil();
        if a <= 0.0 {
            return;
        }
        let screen = ctx.content_rect();
        let keep = if self.ui.view == state::ViewMode::Detail { self.image_rect.or(self.canvas_rect) } else { self.canvas_rect };
        let veil = egui::Color32::from_black_alpha((a.clamp(0.0, 1.0) * 255.0).round() as u8);
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("lights-out")));
        let k = keep.unwrap_or(egui::Rect::NOTHING).intersect(screen);
        if !k.is_positive() {
            p.rect_filled(screen, 0.0, veil);
            return;
        }
        // four bands around the photo
        use egui::{Rect, pos2};
        p.rect_filled(Rect::from_min_max(screen.min, pos2(screen.right(), k.top())), 0.0, veil);
        p.rect_filled(Rect::from_min_max(pos2(screen.left(), k.bottom()), screen.max), 0.0, veil);
        p.rect_filled(Rect::from_min_max(pos2(screen.left(), k.top()), pos2(k.left(), k.bottom())), 0.0, veil);
        p.rect_filled(Rect::from_min_max(pos2(k.right(), k.top()), pos2(screen.right(), k.bottom())), 0.0, veil);
    }

    /// Once per library opened (at launch too): name it in the window title, so nobody edits in
    /// the wrong one, and put it first in File ▸ Open Recent Library.
    pub(crate) fn library_changed(&mut self, ctx: &egui::Context) {
        let generation = self.session.library_generation();
        if self.shown_library == Some(generation) {
            return;
        }
        self.shown_library = Some(generation);
        if let Some(l) = self.session.library.as_ref().filter(|l| l.on_disk) {
            let path = l.dir.to_string_lossy().into_owned();
            panels::settings::remember_library(&mut self.ui.settings.recent_libraries, &path);
            // it is open: whatever an earlier check said about it is over
            self.recent_checks.opened(&path);
            // when, for the launch window (issue #1290); only the listed libraries keep a time
            let settings = &mut self.ui.settings;
            settings.recent_opened.insert(path, (self.session.clock)());
            settings.recent_opened.retain(|p, _| settings.recent_libraries.contains(p));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.window_title()));
    }

    /// "LightCraft — <library name>" (just "LightCraft" for a library that isn't saved).
    pub fn window_title(&self) -> String {
        match panels::settings::open_library_name(self) {
            Some(name) => format!("LightCraft — {name}"),
            None => "LightCraft".to_string(),
        }
    }

    /// The browser page's title: "<file name>[ •] — LightCraft" for the active photo (• while
    /// `unsaved`: changes not yet written to storage), else the window title. The web build has no
    /// window for `ViewportCommand::Title`; a host embedding it reads this title to tell which
    /// document a drop opened and whether it is saved (issue #2397).
    pub fn page_title(&self, unsaved: bool) -> String {
        let dot = if unsaved { " •" } else { "" };
        match self.session.active().and_then(|id| self.session.catalog.photo(id)) {
            Some(p) => format!("{}{dot} — LightCraft", p.file_name),
            None => format!("{}{dot}", self.window_title()),
        }
    }

    /// A completed, nondiscarded pass is a settled frame; a discarded pass threw its widgets away
    /// and must not advance the geometry generation a capture binds to.
    pub(crate) fn finish_ui_pass(&mut self, ctx: &egui::Context) {
        // Any pass consumed the injected batch; a discarded pass still read its events and the
        // next pass has none. Only a nondiscarded pass, however, is a settled frame a capture may
        // bind to and a control gesture may resolve against.
        self.synthetic_in_flight = false;
        if ctx.will_discard() {
            self.next_widgets.clear();
            self.next_widget_hits.clear();
            return;
        }
        self.widgets = std::mem::take(&mut self.next_widgets);
        self.widget_hits = std::mem::take(&mut self.next_widget_hits);
        panels::notices::finish_ui_pass(self, ctx);
        panels::library_problem::finish_ui_pass(self, ctx);
        self.ui_passes = self.ui_passes.wrapping_add(1);
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        self.resolve_pending_gestures(ctx);
        self.reply_delivered_gestures();
        if self.pointer_cleanup {
            // A canceled press may be discovered by a timeout after the synthetic queue emptied;
            // nothing else may ask for the frame that applies its cancellation.
            ctx.request_repaint();
        }
    }

    /// Presentation only: the window's panels, dialogs and overlays.
    pub(super) fn draw_window(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let t0 = monotonic_ms();
        panels::library_problem::begin_draw(self);
        // A command or shortcut drawn before this can create the window after `ui_pass`; catch it
        // here before any panel draws with the still-focused background text field.
        let batch_in_flight = self.synthetic_in_flight;
        panels::library_problem::prepare_ui_pass(self, &ctx, batch_in_flight);
        // A newly shown blocking library window gets the same barrier as a prompt: its first Area
        // layout is non-interactable and hit testing still uses the previous pass, so background
        // panels (and a focused text field) must not draw in that pass.
        if panels::library_problem::shield_pass(self, &ctx) {
            #[cfg(test)]
            self.library_passes.push(panels::library_problem::LibraryPass::Shield);
            panels::library_problem::show(self, &ctx, false);
            (self.next_widgets, self.next_widget_hits) = widgets::take_registries(&ctx);
            ctx.request_discard("library problem: shield the window's creation frame");
            self.end_frame(&ctx, t0);
            return;
        }
        // A newly shown prompt gets one pass with no background. egui's first Area layout is
        // non-interactable and its hit test uses the previous pass, so drawing panels here would
        // let a pointer press/release in the prompt's creation frame act underneath it. `ui()` then
        // reruns this pass with empty raw input, where the settled prompt and the full window draw
        // together. This is a state/input-routing barrier, not a wholesale input disable: once
        // settled, raw pointer input still reaches the prompt backdrop (a no-op) or its buttons.
        if panels::notices::shield_pass(self, &ctx) {
            panels::notices::show(self, &ctx);
            (self.next_widgets, self.next_widget_hits) = widgets::take_registries(&ctx);
            ctx.request_discard("notice: shield the prompt's creation frame");
            self.end_frame(&ctx, t0);
            return;
        }
        #[cfg(test)]
        self.library_passes.push(panels::library_problem::LibraryPass::Background);
        panels::dialogs::prepare(self, ui);
        // Freeze this frame's notice barrier before `notices::show` can dismiss a prompt: the same
        // click or key must never be acted on again by the dialog drawn after it.
        let notice_blocks = panels::notices::blocks_input(self);
        if std::mem::take(&mut self.ui.quit) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.library_changed(&ctx);
        // panels set it again this frame while the pointer rests on a preset or profile
        self.hover_preview = None;
        if self.ui.fullscreen {
            // full-screen preview: the photo alone on black
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                .show(ui, |ui| if self.ui.slideshow.is_some() { panels::slideshow::show(self, ui) } else { panels::detail::show(self, ui) });
            panels::second::show(self, &ctx);
            if notice_blocks {
                panels::notices::restore_prompt_pointer(self, &ctx);
            }
            panels::notices::show(self, &ctx);
            if notice_blocks {
                panels::notices::hide_background_pointer(&ctx);
            }
            panels::dialogs::show(self, &ctx, notice_blocks);
            updates_ui::show(self, &ctx);
            panels::library_problem::show(self, &ctx, notice_blocks);
            panels::toast(self, &ctx);
            drop_overlay::show(self, &ctx);
            if notice_blocks {
                // Keep the real press/release state for the next pass: an ordinary click spans
                // host frames, and resetting it above must only hide it from this pass's dialogs.
                panels::notices::restore_prompt_pointer(self, &ctx);
            }
            (self.next_widgets, self.next_widget_hits) = widgets::take_registries(&ctx);
            self.end_frame(&ctx, t0);
            return;
        }
        // Order matters: earlier panels take the full edge (top bar spans the window; the tool strip,
        // right panels and left panel run to the bottom; the bottom bar sits between them).
        panels::topbar::show(self, ui);
        panels::library_problem::banner(self, ui);
        panels::strip::show(self, ui);
        // filled: the gaps the dock leaves between its panes show chrome, not the dark clear colour
        let chrome = theme::Tokens::get(&ctx).chrome;
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(chrome)).show(ui, |ui| docking::show(self, ui));
        self.lights_out(&ctx);
        panels::second::show(self, &ctx);
        if notice_blocks {
            panels::notices::restore_prompt_pointer(self, &ctx);
        }
        panels::notices::show(self, &ctx);
        if notice_blocks {
            panels::notices::hide_background_pointer(&ctx);
        }
        panels::dialogs::show(self, &ctx, notice_blocks);
        updates_ui::show(self, &ctx);
        panels::library_problem::show(self, &ctx, notice_blocks);
        export_task::poll(self, &ctx);
        slideshow_export::poll(self, &ctx);
        panels::activity::show(self, &ctx);
        export_task::poll_photos(self, &ctx);
        pick::poll(self, &ctx);
        panels::grid::drag_feedback(self, &ctx);
        panels::left::album_drag_feedback(self, &ctx);
        panels::left::section_drag_feedback(self, &ctx);
        panels::keyword_list::drag_feedback(self, &ctx);
        panels::toast(self, &ctx);
        drop_overlay::show(self, &ctx);
        // AppKit moves the window on a drag anywhere in the title-bar band unless the window isn't
        // movable: it stops being movable while the pointer is on a bar control or a panel over it
        #[cfg(target_os = "macos")]
        craft_window::title_bar::sync_movable(&ctx, &mut self.window_movable);
        if notice_blocks {
            // Keep the real press/release state for the next pass: an ordinary click spans host
            // frames, and resetting it above must only hide it from this pass's dialogs.
            panels::notices::restore_prompt_pointer(self, &ctx);
        }
        (self.next_widgets, self.next_widget_hits) = widgets::take_registries(&ctx);
        self.text_focus = ctx.egui_wants_keyboard_input();
        self.end_frame(&ctx, t0);
    }
}

/// Import commits batches at most once per host frame. The first layout pass and a hidden
/// `run_logic` tick both report pass index 0; a discarded frame's later passes report 1+. A
/// wall-clock gate cannot express that (a slow first pass could re-run it within one frame).
pub(crate) fn import_tick_allowed(ctx: &egui::Context) -> bool {
    ctx.current_pass_index() == 0
}
