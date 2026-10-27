use super::*;
use crate::control;

impl LightcraftApp {
    /// The immutable identity a capture is bound to: the library generation. It changes when a
    /// different library is opened, not on every edit, import batch or keyword op (`catalog.revision`
    /// bumps on all of those), so a screenshot during interactive editing still belongs to the
    /// document the user asked for.
    pub(crate) fn shot_document_generation(&self) -> u64 {
        self.session.library_generation()
    }

    /// The window's current surface geometry as native points plus native pixels per point.
    /// `run_logic` refreshes `input.viewports`, so this is fresh even while the UI is not laid out;
    /// without one, the last laid-out content rect is used.
    fn shot_geometry(&self, ctx: &egui::Context) -> ShotGeometry {
        // Both `viewport.inner_rect` and the content rect are in egui points, and `pixels_per_point`
        // already includes the UI zoom (egui-winit divides by the zoomed ppp). Multiplying by the
        // zoom again would reserve a surface that is too small, so the zoom is only tracked as a
        // change signal.
        let zoom = ctx.zoom_factor();
        let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
        let pixels_per_point = ctx.input(|i| i.pixels_per_point());
        let viewport = ctx.input(|i| {
            i.viewport().inner_rect.filter(|r| r.width() > 0.0 && r.height() > 0.0).map_or_else(|| i.content_rect().size(), |inner| inner.size())
        });
        ShotGeometry { viewport, pixels_per_point, zoom, epoch: self.shot_geometry_epoch }
    }

    /// Track geometry changes as a generation. Called once per logic tick from the refreshed
    /// viewport information, so a hidden window still notices a resize.
    pub(super) fn note_shot_geometry(&mut self, ctx: &egui::Context) {
        let mut now = self.shot_geometry(ctx);
        if let Some(last) = self.last_shot_geometry
            && (last.viewport != now.viewport || last.pixels_per_point != now.pixels_per_point || last.zoom != now.zoom)
        {
            self.shot_geometry_epoch = self.shot_geometry_epoch.wrapping_add(1);
            now.epoch = self.shot_geometry_epoch;
        }
        self.last_shot_geometry = Some(now);
    }

    /// The texture limit the host reported (16 384 unless it said otherwise).
    fn max_texture_side(&self) -> usize {
        self.max_texture_side
    }

    /// Retain the host's `RawInput::max_texture_side`. Hosts (egui-winit, `Headless`) report it
    /// only when it changes, so keep the last value instead of assuming 16 384.
    pub(super) fn note_max_texture_side(&mut self, ctx: &egui::Context) {
        if let Some(limit) = ctx.input(|i| i.raw.max_texture_side)
            && limit > 0
        {
            self.max_texture_side = limit;
        }
    }

    /// Reserve a new image's `bytes` against the aggregate screenshot budget, or report why not.
    fn reserve_shot_bytes(&mut self, bytes: usize) -> Result<(), String> {
        if bytes > MAX_SHOT_BYTES {
            return Err(format!("ui.screenshot: {bytes} bytes is over the {MAX_SHOT_BYTES}-byte per-image limit"));
        }
        let total = self.shot_bytes.checked_add(bytes).ok_or("ui.screenshot: byte reservation overflow")?;
        if total > MAX_SHOT_TOTAL_BYTES {
            return Err(format!("ui.screenshot: {total} bytes reserved is over the {MAX_SHOT_TOTAL_BYTES}-byte limit"));
        }
        self.shot_bytes = total;
        Ok(())
    }

    /// A surface that grew after admission (a stale `content_rect` on Wayland, a resize between
    /// issue and completion): bound the *new image* against the per-image limit, then reserve only
    /// the growth against the aggregate. The old per-image check would compare the delta to the
    /// per-image cap and name the delta as if it were the image.
    fn grow_shot_reservation(&mut self, current_reserved: usize, image_bytes: usize) -> Result<(), String> {
        if image_bytes > MAX_SHOT_BYTES {
            return Err(format!("ui.screenshot: {image_bytes} bytes is over the {MAX_SHOT_BYTES}-byte per-image limit"));
        }
        let extra = image_bytes.saturating_sub(current_reserved);
        let total = self.shot_bytes.checked_add(extra).ok_or("ui.screenshot: byte reservation overflow")?;
        if total > MAX_SHOT_TOTAL_BYTES {
            return Err(format!("ui.screenshot: {total} bytes reserved is over the {MAX_SHOT_TOTAL_BYTES}-byte limit"));
        }
        self.shot_bytes = total;
        Ok(())
    }

    /// Release a reservation; never let an accounting bug underflow `usize`.
    fn release_shot_bytes(&mut self, bytes: usize) {
        self.shot_bytes = self.shot_bytes.saturating_sub(bytes);
    }

    /// Admit a `ui.screenshot` request against the bounded capture budget. Nothing is written and
    /// no capture is issued here: [`Self::issue_screenshots`] waits for a settled, nondiscarded UI
    /// pass and a fresh geometry generation.
    pub(crate) fn admit_shot(
        &mut self,
        ctx: &egui::Context,
        target: Option<lightcraft_engine::originals::WriteTarget>,
        headless: bool,
        reply: Sender<ControlResponse>,
    ) -> Result<(), String> {
        if self.pending_screenshots.len() >= MAX_PENDING_SHOTS {
            return Err(format!("ui.screenshot: {MAX_PENDING_SHOTS} requests are already pending; wait for a reply first"));
        }
        let geometry = self.last_shot_geometry.unwrap_or_else(|| self.shot_geometry(ctx));
        let bytes = geometry.bytes(self.max_texture_side()).ok_or_else(|| geometry.rejection(self.max_texture_side()))?;
        self.reserve_shot_bytes(bytes)?;
        self.screenshot_token = self.screenshot_token.wrapping_add(1);
        let now = self.tick_now_ms();
        self.pending_screenshots.push(PendingShot {
            token: self.screenshot_token,
            target,
            reply,
            headless: headless && !self.headless_host,
            admitted_passes: self.ui_passes,
            not_before_ms: now + SHOT_SETTLE_MS,
            // Capture the current settled frame at this instant even if renders are still in flight
            // (0.36 did this at its three-second deadline, so automation never got an error instead
            // of an image while a slider/import kept the renderer busy).
            force_ms: now + SHOT_FORCE_BUSY_MS,
            // Settle + forced capture + compositor fallback + the headless settle, all from admission.
            deadline_ms: now + SHOT_SETTLE_MS + SCREENSHOT_SETTLE_MS + SCREENSHOT_FALLBACK_MS + SCREENSHOT_SETTLE_MS,
            sent_at_ms: None,
            reserved: bytes,
            slot: None,
            issued: None,
            issued_library: self.shot_document_generation(),
        });
        ctx.request_repaint();
        Ok(())
    }

    /// Advance pending screenshots: wait (≥ 3 completed UI frames, ≥ 120 ms) until no renders are
    /// in flight — or until the forced-capture instant, so sustained rendering cannot turn a
    /// request into a timeout — then capture through the compositor's one-shot callback, or
    /// headlessly ([`Self::headless_screenshot`]) when asked or when the compositor delivers nothing.
    pub(crate) fn issue_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        let busy = self.renderer.in_flight() > 0 || self.merge.busy();
        let max_side = self.max_texture_side();
        let library = self.shot_document_generation();
        let geometry = self.last_shot_geometry.unwrap_or_else(|| self.shot_geometry(ctx));
        let mut shots = std::mem::take(&mut self.pending_screenshots);
        let mut shadow_ticked = false;
        let mut keep = Vec::with_capacity(shots.len());
        for mut shot in shots.drain(..) {
            // Hard deadline from admission: a compositor fallback does not extend it.
            if now > shot.deadline_ms {
                let _ = shot.reply.send(serde_json::json!({"ok": false, "error": format!("screenshot {} timed out", shot.token)}));
                if let Some(slot) = shot.slot.take() {
                    slot.cancel();
                }
                self.release_shot_bytes(shot.reserved);
                continue;
            }
            if let Some(sent) = shot.sent_at_ms {
                if now - sent < SCREENSHOT_FALLBACK_MS || self.headless_host {
                    keep.push(shot);
                    continue;
                }
                // The compositor delivered nothing (display asleep, window occluded): go headless.
                log::warn!("ui.screenshot: no frame from the compositor after {SCREENSHOT_FALLBACK_MS} ms; rendering headlessly");
                if let Some(slot) = shot.slot.take() {
                    slot.cancel();
                }
                shot.sent_at_ms = None;
                shot.issued = None;
                shot.headless = true;
            }
            // A busy renderer only defers the capture until the forced instant; after that the
            // settled frame that exists now is captured (0.36's deadline behavior), never an error.
            let ready = shot_ready(now, &shot, busy, self.ui_passes);
            if !ready {
                keep.push(shot);
                continue;
            }
            if shot.headless {
                // The shadow frame requests the renders the UI needs, even while the window shows nothing.
                let img = if shadow_ticked { None } else { self.headless_screenshot(ctx, true) };
                shadow_ticked = true;
                if let Some(img) = img {
                    let bytes = img.size[0].checked_mul(img.size[1]).and_then(|p| p.checked_mul(std::mem::size_of::<egui::Color32>()));
                    match bytes {
                        Some(bytes) if bytes > shot.reserved => {
                            // A stale/undersized reservation is not a user error: grow it to the
                            // actual image and encode it when the per-image and aggregate caps allow.
                            if let Err(error) = self.grow_shot_reservation(shot.reserved, bytes) {
                                let _ = shot.reply.send(serde_json::json!({"ok": false, "error": error}));
                                self.release_shot_bytes(shot.reserved);
                                continue;
                            }
                            shot.reserved = bytes;
                        }
                        Some(_) => {}
                        None => {
                            let reply = serde_json::json!({
                                "ok": false,
                                "error": "ui.screenshot: the rendered image has invalid dimensions",
                            });
                            let _ = shot.reply.send(reply);
                            self.release_shot_bytes(shot.reserved);
                            continue;
                        }
                    }
                    let _ = shot.reply.send(control::save_screenshot(self, &img, shot.target.as_ref()));
                    self.release_shot_bytes(shot.reserved);
                    continue;
                }
                keep.push(shot);
                continue;
            }
            // The compositor path needs a real, laid-out UI and fresh fonts; a queued request on a
            // hidden window is kept alive by RequestPaintWhileHidden from `logic`.
            if !self.fonts_ready || self.ui_passes == 0 {
                keep.push(shot);
                continue;
            }
            // Revalidate the reservation against the geometry the command is actually bound to.
            let Some(bytes) = geometry.bytes(max_side) else {
                let _ = shot.reply.send(serde_json::json!({"ok": false, "error": geometry.rejection(max_side)}));
                self.release_shot_bytes(shot.reserved);
                continue;
            };
            if bytes > shot.reserved {
                if let Err(error) = self.grow_shot_reservation(shot.reserved, bytes) {
                    let _ = shot.reply.send(serde_json::json!({"ok": false, "error": error}));
                    self.release_shot_bytes(shot.reserved);
                    continue;
                }
                shot.reserved = bytes;
            }
            // One slot and one callback per issue; the callback only holds a weak slot and wakes
            // the viewport. File writes and replies happen in `collect_screenshots`.
            let (slot, callback) = craft_ui::capture::CaptureSlot::new(ctx, egui::ViewportId::ROOT, MAX_SHOT_BYTES);
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(callback));
            log::debug!("ui.screenshot {}: issued at {:?}, {} bytes reserved", shot.token, geometry.viewport, shot.reserved);
            shot.slot = Some(slot);
            shot.sent_at_ms = Some(now);
            shot.issued = Some(geometry);
            shot.issued_library = library;
            keep.push(shot);
        }
        self.pending_screenshots = keep;
        if !self.pending_screenshots.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// Draw the UI into an offscreen context at the window's size; with `capture`, rasterize it
    /// on the CPU (photo textures come from the renderer's CPU copies).
    pub fn headless_screenshot(&mut self, main: &egui::Context, capture: bool) -> Option<egui::ColorImage> {
        let mut view = self.shadow.take().unwrap_or_default();
        let size = main.input(|i| i.content_rect()).size();
        let size = if size.x >= 1.0 && size.y >= 1.0 { size } else { egui::vec2(1600.0, 1000.0) };
        // Feed native viewport points; the shadow context applies the copied UI zoom.
        let zoom = main.zoom_factor();
        let size = size * zoom;
        let ppp = main.pixels_per_point() / zoom;
        let time = main.input(|i| i.time);
        if view.frames() == 0 {
            // warm-up pass: activates our fonts (pending font definitions live in `Memory`, which
            // is replaced below)
            view.run(headless::HeadlessView::raw_input(size, ppp, time, vec![]), |_| {});
        }
        // same scroll offsets, open sections, style… as the window
        let memory = main.memory(|m| m.clone());
        view.ctx.memory_mut(|m| *m = memory);
        view.run(headless::HeadlessView::raw_input(size, ppp, time, vec![]), |ui| self.draw_window(ui));
        let img = capture.then(|| {
            let mut tex = self.renderer.cpu_textures();
            if let (Some((t, ..)), Some(px)) = (&self.merge.preview, &self.merge.preview_pixels) {
                tex.insert(t.id(), crate::softpaint::CpuTexture::linear(px.clone()));
            }
            view.paint(&tex)
        });
        self.shadow = Some(view);
        img
    }

    /// Take each completed capture once, reject stale generations before writing anything, and
    /// reply exactly once. A late callback after cancel/drop finds no slot and is inert.
    pub(crate) fn collect_screenshots(&mut self, _ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let now = self.tick_now_ms();
        let current = self.last_shot_geometry;
        let library = self.shot_document_generation();
        let mut shots = std::mem::take(&mut self.pending_screenshots);
        let mut keep = Vec::with_capacity(shots.len());
        for mut shot in shots.drain(..) {
            let completed = shot.slot.as_ref().and_then(craft_ui::capture::CaptureSlot::take);
            match completed {
                Some(Ok(image)) => {
                    let stale =
                        shot.issued.is_none_or(|issued| current.is_none_or(|now| now.epoch != issued.epoch)) || shot.issued_library != library;
                    if stale {
                        // A resize/DPI/zoom or library change between issue and completion: never
                        // combine old pixels with new geometry. Retry with a fresh slot.
                        shot.slot = None;
                        shot.sent_at_ms = None;
                        shot.issued = None;
                        shot.not_before_ms = now + SHOT_SETTLE_MS;
                        keep.push(shot);
                        continue;
                    }
                    let pixels = image.size[0].checked_mul(image.size[1]).and_then(|p| p.checked_mul(std::mem::size_of::<egui::Color32>()));
                    let Some(bytes) = pixels else {
                        let reply = serde_json::json!({
                            "ok": false,
                            "error": "ui.screenshot: the rendered image has invalid dimensions",
                        });
                        let _ = shot.reply.send(reply);
                        self.release_shot_bytes(shot.reserved);
                        continue;
                    };
                    if bytes > shot.reserved {
                        // Wayland reports no `inner_rect`, so the reservation can be based on a stale
                        // content rect while the compositor returns the new, larger surface. Treat a
                        // larger-but-legal image as a stale reservation and grow it; never mix it
                        // with geometry of another epoch (checked above).
                        if let Err(error) = self.grow_shot_reservation(shot.reserved, bytes) {
                            let _ = shot.reply.send(serde_json::json!({"ok": false, "error": error}));
                            self.release_shot_bytes(shot.reserved);
                            continue;
                        }
                        shot.reserved = bytes;
                    }
                    self.release_shot_bytes(shot.reserved);
                    let _ = shot.reply.send(control::save_screenshot(self, &image, shot.target.as_ref()));
                }
                Some(Err(error)) => {
                    let _ = shot.reply.send(serde_json::json!({"ok": false, "error": error.to_string()}));
                    self.release_shot_bytes(shot.reserved);
                }
                None => {
                    if now > shot.deadline_ms {
                        let _ = shot.reply.send(serde_json::json!({"ok": false, "error": format!("screenshot {} timed out", shot.token)}));
                        if let Some(slot) = shot.slot.take() {
                            slot.cancel();
                        }
                        self.release_shot_bytes(shot.reserved);
                    } else {
                        keep.push(shot);
                    }
                }
            }
        }
        self.pending_screenshots = keep;
    }
}

/// Whether a pending request may be issued now: it has waited for the settle time and completed
/// UI frames, and either no render is in flight or the forced-capture instant has passed.
pub(crate) fn shot_ready(now_ms: f64, shot: &PendingShot, busy: bool, ui_passes: u64) -> bool {
    let settled = ui_passes.saturating_sub(shot.admitted_passes) >= SHOT_SETTLE_FRAMES && now_ms >= shot.not_before_ms;
    settled && (!busy || now_ms >= shot.force_ms)
}
