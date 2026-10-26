//! 0.37 lifecycle regressions: the app clock, one-consumer queues, one pending synthetic batch,
//! parent-salted panel state, bundled monochrome emoji, and bounded/stale-safe screenshot capture.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::headless::Headless;
use crate::render::{Slot, Tex};
use crate::{ControlResponse, LightcraftApp, MAX_PENDING_SHOTS, MAX_SHOT_BYTES, PendingShot, Services, ShotGeometry};
use lightcraft_engine::slideshow::Item;

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() })
}

fn raw(size: egui::Vec2) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() }
}

fn key_event(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }
}

fn pointer_event(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

fn accesskit_action(action: egui::accesskit::Action, target_node: egui::accesskit::NodeId) -> egui::Event {
    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action,
        target_tree: egui::accesskit::TreeId::ROOT,
        target_node,
        data: None,
    })
}

fn viewport_raw(size: egui::Vec2, visible: bool) -> egui::RawInput {
    let mut raw = raw(size);
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(!visible);
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().occluded = Some(false);
    raw
}

fn reply_channel() -> (Sender<ControlResponse>, Receiver<ControlResponse>) {
    channel()
}

fn geometry(width: f32, height: f32) -> ShotGeometry {
    ShotGeometry { viewport: egui::vec2(width, height), pixels_per_point: 1.0, zoom: 1.0, epoch: 0 }
}

fn shot_bytes_for(width: usize, height: usize) -> usize {
    width * height * std::mem::size_of::<egui::Color32>()
}

/// One layout pass whose texture delta is consumed. 0.37's `TexturesDelta` panics when a non-empty
/// delta is dropped, so a test that only inspects input/state must still acknowledge the font atlas
/// the pass produced. `HeadlessView::run` does this with `TexturesDelta::apply`; a bare
/// `Context::run_ui` has no renderer, so clearing the delta is the explicit handling.
fn frame(ctx: &egui::Context, raw: egui::RawInput, ui: impl FnMut(&mut egui::Ui)) {
    let mut output = ctx.run_ui(raw, ui);
    output.textures_delta.clear();
}

/// An app whose screenshot encoder writes through the real file path and records the pixels it saw.
fn app_with_screenshot_files() -> (LightcraftApp, Arc<Mutex<Option<(usize, usize, Vec<u8>)>>>) {
    let mut app = app();
    let seen = Arc::new(Mutex::new(None));
    let recorder = Arc::clone(&seen);
    app.services.png = Some(Box::new(move |img: &lightcraft_raster::Rgba8| {
        *recorder.lock().unwrap() = Some((img.width, img.height, img.as_bytes()));
        vec![0x89, b'P', b'N', b'G']
    }));
    app.services.write = Some(Box::new(|path: &str, bytes: &[u8]| std::fs::write(path, bytes).map_err(|e| e.to_string())));
    app.services.write_new = Some(Box::new(|path: &str, bytes: &[u8]| lightcraft_engine::export::write_file_new(path, bytes)));
    (app, seen)
}

fn temp_png(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("lightcraft-{name}-{}.png", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Bind a pending request to the geometry/library generation a real issue would have used.
fn issue_into_slot(ctx: &egui::Context, shot: &mut PendingShot, issued: ShotGeometry, library: u64) -> egui::ScreenshotCallback {
    let (slot, callback) = craft_ui::capture::CaptureSlot::new(ctx, egui::ViewportId::ROOT, MAX_SHOT_BYTES);
    shot.slot = Some(slot);
    shot.sent_at_ms = Some(0.0);
    shot.issued = Some(issued);
    shot.issued_library = library;
    callback
}

#[test]
fn bundled_faces_include_monochrome_emoji() {
    // 0.37 moved the monochrome emoji faces behind `monochrome_emoji_fonts`, and `default_fonts`
    // is no longer an egui default. Both come from the root `egui` features (kittest's own
    // dev-dependencies do not propagate), so this test fails if either is lost.
    let definitions = crate::theme::font_definitions(&[]);
    for name in ["Hack", "Ubuntu-Light", "egui-icons", "NotoEmoji-Regular", "emoji-icon-font"] {
        assert!(definitions.font_data.contains_key(name), "bundled face {name} is missing: {:?}", definitions.font_data.keys().collect::<Vec<_>>());
    }
    let ctx = egui::Context::default();
    ctx.set_fonts(definitions);
    // `Fonts` needs a first pass for its pixels-per-point before `fonts_mut` may inspect glyphs.
    frame(&ctx, raw(egui::vec2(240.0, 120.0)), |ui| {
        // U+1F600 and U+1F44D are in the bundled font cmaps. The older bundled subset
        // does not contain U+1F642, so it cannot serve as a feature-wiring oracle.
        ui.label("😀👍");
    });
    let font = egui::FontId::proportional(16.0);
    assert!(ctx.fonts_mut(|f| f.has_glyph(&font, '\u{1F600}')), "😀 must be paintable from the bundled faces");
    assert!(ctx.fonts_mut(|f| f.has_glyph(&font, '\u{1F44D}')), "👍 must be paintable from the bundled faces");
}

#[test]
fn resizable_side_stores_panel_state_under_the_parent_salted_key() {
    let ctx = egui::Context::default();
    let raw = raw(egui::vec2(1000.0, 700.0));
    frame(&ctx, raw, |ui| {
        crate::panels::resizable_side(ui, true, "side-test", egui::Frame::NONE, 260.0, crate::state::LEFT_WIDTH, 300.0, |ui| {
            ui.label("side");
        });
    });
    let modern = ctx.viewport_id().root_ui_id().with("side-test");
    assert!(
        egui::containers::panel::PanelState::load(&ctx, modern).is_some(),
        "0.37 stores panel state under parent_ui.scope_id().with_salt(id_salt)"
    );
    assert!(
        egui::containers::panel::PanelState::load(&ctx, egui::Id::unique("side-test")).is_none(),
        "an already-built Id must not be salted a second time (the 0.36 exact-Id key)"
    );
}

#[test]
fn legacy_panel_state_is_dropped_once_and_modern_keys_survive() {
    let (ctx, mut app) = (egui::Context::default(), app());
    let state = || egui::containers::panel::PanelState { outer_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)) };
    let modern = ctx.viewport_id().root_ui_id();
    let salts = ["top_bar", "bottom_bar", "tool_strip", "library-problem-banner"];
    for salt in salts {
        let legacy = egui::Id::unique(salt);
        ctx.data_mut(|d| d.insert_persisted(legacy, state()));
        ctx.data_mut(|d| d.insert_persisted(modern.with(salt), state()));
    }
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |_| {});
    app.logic(&ctx);
    for salt in salts {
        let legacy = egui::Id::unique(salt);
        assert!(egui::containers::panel::PanelState::load(&ctx, legacy).is_none(), "the dead 0.36 key for {salt} is removed");
        assert!(egui::containers::panel::PanelState::load(&ctx, modern.with(salt)).is_some(), "the 0.37 parent-salted key for {salt} is not touched");
    }
}

#[test]
fn capture_admission_bounds_count_and_bytes_before_issue() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    for _ in 0..MAX_PENDING_SHOTS {
        let (tx, _rx) = reply_channel();
        app.admit_shot(&ctx, None, false, tx).expect("admission within all limits");
    }
    let per_image = shot_bytes_for(100, 100);
    assert_eq!(app.shot_bytes, MAX_PENDING_SHOTS * per_image);
    let (tx, _rx) = reply_channel();
    assert!(app.admit_shot(&ctx, None, false, tx).is_err(), "a fifth request is refused before issue");
    // The count gate runs before geometry admission. Free the four slots so the next check reaches
    // the byte branch instead of answering the count error again.
    app.pending_screenshots.clear();
    app.shot_bytes = 0;

    // An image over the per-image cap is refused, and nothing is reserved for it. The geometry is
    // inside the texture limit (16 384 × 2 049), so this exercises the byte branch, not `pixels()`.
    let before = app.shot_bytes;
    app.last_shot_geometry = Some(geometry(16_384.0, 2_049.0));
    let (tx, _rx) = reply_channel();
    let error = app.admit_shot(&ctx, None, false, tx).expect_err("128 MiB + one row is over the per-image cap");
    assert!(error.contains("134283264"), "the error names the image size: {error}");
    assert!(error.contains("134217728"), "the error names the per-image cap: {error}");
    assert_eq!(app.shot_bytes, before, "a refused request reserves no bytes");

    // Two 100 MB surfaces fit the 128 MiB per-image cap, but a third would exceed the 256 MiB
    // aggregate budget (completed-but-undrained images stay counted).
    let (ctx, mut app) = (egui::Context::default(), self::app());
    app.last_shot_geometry = Some(geometry(5000.0, 5000.0));
    for _ in 0..2 {
        let (tx, _rx) = reply_channel();
        app.admit_shot(&ctx, None, false, tx).expect("100 MB is under the per-image cap");
    }
    let (tx, _rx) = reply_channel();
    assert!(app.admit_shot(&ctx, None, false, tx).is_err(), "the aggregate budget bounds a third image");
    assert_eq!(app.shot_bytes, 200_000_000);
}

#[test]
fn texture_limit_and_missing_surface_have_distinct_admission_errors() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.max_texture_side = 1024;
    app.last_shot_geometry = Some(geometry(1200.0, 100.0));
    let (tx, _rx) = reply_channel();
    let error = app.admit_shot(&ctx, None, false, tx).expect_err("wider than the host's texture limit");
    assert!(error.contains("1200×100") && error.contains("1024"), "the error names the surface and the host limit: {error}");
    assert_eq!(app.shot_bytes, 0);

    app.last_shot_geometry = Some(geometry(0.0, 0.0));
    let (tx, _rx) = reply_channel();
    let error = app.admit_shot(&ctx, None, false, tx).expect_err("no laid-out surface");
    assert!(error.contains("no usable surface size"), "the error names the missing layout, not the texture limit: {error}");
    assert_eq!(app.shot_bytes, 0);
}

#[test]
fn stale_geometry_completion_is_rejected_before_any_write() {
    let (ctx, mut app) = (egui::Context::default(), app());
    let path = temp_png("stale-shot");
    let (tx, rx) = reply_channel();
    let issued = ShotGeometry { epoch: 7, ..geometry(100.0, 100.0) };
    app.last_shot_geometry = Some(issued);
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("pin the path");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    let library = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, ShotGeometry { epoch: 7, ..geometry(100.0, 100.0) }, library)
    };
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)));
    // The window changed size while the callback was in flight: the image belongs to a stale
    // generation and must be retried, never written.
    app.last_shot_geometry = Some(ShotGeometry { epoch: 8, ..geometry(100.0, 100.0) });
    app.collect_screenshots(&ctx);
    assert_eq!(app.pending_screenshots.len(), 1, "the request is retried");
    assert!(app.pending_screenshots[0].slot.is_none(), "the stale slot is dropped");
    assert!(!path.exists(), "a stale image is never written");
    assert!(rx.try_recv().is_err(), "a retry replies only when it succeeds or times out");
    assert_eq!(app.shot_bytes, shot_bytes_for(100, 100), "the reservation is kept for the retry");
}

#[test]
fn a_completed_capture_writes_once_and_releases_its_reservation() {
    let (ctx, mut app, seen) = {
        let (app, seen) = app_with_screenshot_files();
        (egui::Context::default(), app, seen)
    };
    let path = temp_png("happy-shot");
    let (tx, rx) = reply_channel();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("pin the path");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    let issued = geometry(100.0, 100.0);
    let generation = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::from_rgb(1, 2, 3))));
    app.collect_screenshots(&ctx);

    let reply = rx.try_recv().expect("exactly one reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["width"], 2);
    assert_eq!(reply["result"]["height"], 2);
    assert_eq!(std::fs::read(&path).expect("the file is written"), vec![0x89, b'P', b'N', b'G']);
    let seen = seen.lock().unwrap().clone().expect("the encoder ran");
    assert_eq!((seen.0, seen.1), (2, 2));
    assert_eq!(seen.2, vec![1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255]);
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0, "success returns the reservation");
    assert!(rx.try_recv().is_err(), "one reply, not two");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_capture_completed_after_a_catalog_edit_is_still_written() {
    // Catalog edits bump `catalog.revision` on every op; that is content, not document identity.
    // Binding the capture to it retried once per edit and could only end in a timeout.
    let (ctx, mut app, _seen) = {
        let (app, seen) = app_with_screenshot_files();
        (egui::Context::default(), app, seen)
    };
    let path = temp_png("edited-shot");
    let (tx, rx) = reply_channel();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("pin the path");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    let generation = app.shot_document_generation();
    let issued = geometry(100.0, 100.0);
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    app.session.catalog.revision = app.session.catalog.revision.wrapping_add(1);
    assert_eq!(app.shot_document_generation(), generation, "an edit is not a new library");
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)));
    app.collect_screenshots(&ctx);
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(path.exists());
    assert_eq!(app.shot_bytes, 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_image_larger_than_its_reservation_is_grown_and_written() {
    // Wayland has no `inner_rect`, so on a hidden tick the reservation can be based on the old
    // content rect while the compositor returns the resized surface. A legal image is not an error.
    let (ctx, mut app, _seen) = {
        let (app, seen) = app_with_screenshot_files();
        (egui::Context::default(), app, seen)
    };
    let path = temp_png("grown-shot");
    let (tx, rx) = reply_channel();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("pin the path");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    let issued = geometry(100.0, 100.0);
    let generation = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    callback.complete(Arc::new(egui::ColorImage::filled([200, 200], egui::Color32::WHITE)));
    app.collect_screenshots(&ctx);
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["width"], 200);
    assert_eq!(app.shot_bytes, 0, "the grown reservation is released too");
    assert!(path.exists());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_surface_that_grew_past_the_per_image_cap_is_refused_by_issue() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.set_simulated_clock(Some(0.0));
    app.ui_passes = 3;
    app.fonts_ready = true;
    app.last_shot_geometry = Some(geometry(5000.0, 5000.0));
    let (tx, rx) = reply_channel();
    app.admit_shot(&ctx, None, false, tx).expect("100 MB is admitted");
    app.set_simulated_clock(Some(1.0));
    app.ui_passes = 6;
    // The window grew after admission: 7000² × 4 = 196 MB, over the 128 MiB per-image cap.
    app.last_shot_geometry = Some(geometry(7000.0, 7000.0));
    app.issue_screenshots(&ctx);
    let reply = rx.try_recv().expect("the refusal is a reply");
    assert_eq!(reply["ok"], false, "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("196000000"), "the error names the new image, not the delta: {error}");
    assert!(error.contains("134217728"), "{error}");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0);
}

#[test]
fn timeout_replies_once_and_a_late_callback_is_inert() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.set_simulated_clock(Some(0.0));
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    app.admit_shot(&ctx, None, false, tx).expect("admitted");
    let issued = geometry(100.0, 100.0);
    let generation = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    app.set_simulated_clock(Some(10.0));
    app.collect_screenshots(&ctx);
    let reply = rx.try_recv().expect("the timeout reply");
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap_or_default().contains("timed out"), "{reply}");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0, "a timed-out request returns its reservation");
    // The renderer may still deliver the cancelled one-shot: it finds no slot and changes nothing.
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)));
    app.collect_screenshots(&ctx);
    assert!(rx.try_recv().is_err(), "no second reply");
    assert_eq!(app.shot_bytes, 0);
}

#[test]
fn a_busy_renderer_cannot_turn_a_settled_capture_into_a_timeout() {
    let (tx, _rx) = reply_channel();
    let shot = PendingShot {
        token: 1,
        target: None,
        reply: tx,
        headless: false,
        admitted_passes: 3,
        not_before_ms: 120.0,
        force_ms: 3000.0,
        deadline_ms: 5120.0,
        sent_at_ms: None,
        reserved: 1,
        slot: None,
        issued: None,
        issued_library: 0,
    };
    assert!(!crate::shot_ready(100.0, &shot, false, 6), "settle time has not passed");
    assert!(crate::shot_ready(200.0, &shot, false, 6), "an idle renderer captures as soon as it is settled");
    assert!(!crate::shot_ready(200.0, &shot, true, 6), "a busy renderer still waits for the settle window");
    assert!(crate::shot_ready(3000.0, &shot, true, 6), "the forced instant captures the current frame anyway");
}

#[test]
fn a_silent_compositor_falls_back_to_the_headless_capture_once() {
    let (ctx, mut app) = (egui::Context::default(), app());
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    app.set_simulated_clock(Some(0.0));
    app.last_shot_geometry = Some(geometry(160.0, 100.0));
    app.ui_passes = 6;
    let (tx, rx) = reply_channel();
    app.admit_shot(&ctx, None, false, tx).expect("a windowed request is admitted");
    let issued = geometry(160.0, 100.0);
    let generation = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    app.ui_passes = 9;
    // Two seconds after the command went out, no compositor frame has arrived.
    app.set_simulated_clock(Some(2.5));
    app.issue_screenshots(&ctx);
    let reply = rx.try_recv().expect("the fallback answers");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0);
    // The canceled compositor callback may still fire; it must not produce a second reply.
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)));
    app.collect_screenshots(&ctx);
    assert!(rx.try_recv().is_err(), "one reply from the fallback");
}

#[test]
fn a_busy_renderer_still_answers_a_headless_capture_at_the_forced_instant() {
    let (ctx, mut app) = (egui::Context::default(), app());
    // Install fonts and reach the "ready" state a real host has after its setup frame.
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready, "fonts are installed for the shadow frame");
    app.set_simulated_clock(Some(0.0));
    app.last_shot_geometry = Some(geometry(160.0, 100.0));
    app.ui_passes = 10;
    let (tx, rx) = reply_channel();
    app.admit_shot(&ctx, None, true, tx).expect("a headless request is admitted");
    app.ui_passes = 13;
    app.renderer.force_in_flight_for_test(1);
    app.set_simulated_clock(Some(1.0));
    app.issue_screenshots(&ctx);
    assert!(rx.try_recv().is_err(), "a busy renderer defers the capture before the forced instant");
    assert_eq!(app.pending_screenshots.len(), 1);
    app.set_simulated_clock(Some(4.0));
    app.issue_screenshots(&ctx);
    let reply = rx.try_recv().expect("the forced instant answers, it does not time out");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0, "the grown/shrunk reservation is released");
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct TestDropped(std::path::PathBuf);

#[cfg(not(target_arch = "wasm32"))]
impl egui::DroppedFile for TestDropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Ok(vec![])
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn one_synthetic_batch_until_a_completed_pass_and_drops_wait_for_the_ui_phase() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.fonts_ready = true;
    app.synthetic.push(egui::Event::PointerMoved(egui::pos2(4.0, 4.0)));
    let mut first = raw(egui::vec2(800.0, 600.0));
    app.raw_input_hook(&ctx, &mut first);
    assert_eq!(first.events.len(), 1, "one pointer move is injected");
    assert!(app.synthetic_in_flight);
    let mut second = raw(egui::vec2(800.0, 600.0));
    app.raw_input_hook(&ctx, &mut second);
    assert!(second.events.is_empty(), "no second step while the first waits for a UI pass");

    // Drops are queued by the hook even while a batch is in flight, but only the UI phase takes them.
    second.dropped_files.push(Arc::new(TestDropped(std::env::temp_dir().join("lightcraft-drop.txt"))));
    app.raw_input_hook(&ctx, &mut second);
    assert_eq!(app.dropped_files.len(), 1);
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |_| {});
    app.logic(&ctx);
    app.logic(&ctx);
    assert_eq!(app.dropped_files.len(), 1, "a hidden logic tick never replays the drop");
    assert_eq!(app.take_dropped_files().len(), 1, "the UI phase takes the queued drop exactly once");
    assert!(app.dropped_files.is_empty());

    // A completed, nondiscarded app pass (not a bare `run_ui` closure) ends the in-flight batch, so
    // the next host frame injects the next synthetic step. This fails if `finish_ui_pass` stops
    // clearing the gate.
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| app.ui(ui));
    assert!(!app.synthetic_in_flight, "a completed UI pass consumed the batch");
    app.synthetic.push(egui::Event::PointerMoved(egui::pos2(9.0, 9.0)));
    let mut third = raw(egui::vec2(800.0, 600.0));
    app.raw_input_hook(&ctx, &mut third);
    assert_eq!(third.events.len(), 1, "the next batch is injected after the completed pass");
}

#[test]
fn first_headless_frame_runs_logic_and_uses_its_simulated_clock() {
    let mut h = Headless::new(app(), [800.0, 600.0], 1.0);
    assert_eq!(h.time(), 0.0);
    h.step();
    assert!(h.app.styled, "the first frame at t=0 runs logic and installs fonts");
    assert_eq!(h.app.tick_now(), 0.0, "the app clock follows the headless host's simulated time");
    h.step();
    h.step();
    assert!(h.app.fonts_ready, "fonts are ready after the setup pass");
    assert_eq!(h.app.ui_passes, 2, "exactly the two steps after the setup pass were laid out");
    assert!(h.app.tick_now() > 0.0, "the simulated clock advanced with the steps");
}

#[test]
fn import_ticks_once_per_host_frame_and_once_per_hidden_tick() {
    let ctx = egui::Context::default();
    let seen = std::cell::RefCell::new(Vec::new());
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| {
        seen.borrow_mut().push(crate::import_tick_allowed(ui.ctx()));
        if seen.borrow().len() == 1 {
            ui.ctx().request_discard("test: force a second layout pass");
        }
    });
    assert_eq!(*seen.borrow(), vec![true, false], "the first pass commits import batches; the discarded re-run does not");

    let hidden = viewport_raw(egui::vec2(800.0, 600.0), false);
    let mut allowed = false;
    let _ = ctx.run_logic(&hidden, |ctx| allowed = crate::import_tick_allowed(ctx));
    assert!(allowed, "a hidden logic tick reports pass index 0 and must still commit import batches");
}

#[test]
fn hidden_slideshow_pauses_and_keeps_its_remaining_time() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.set_simulated_clock(Some(10.0));
    let started = crate::panels::slideshow::start(
        &mut app,
        &serde_json::json!({"slideDuration": 2.0, "fadeDuration": 0.0, "randomOrder": false, "repeat": false}),
        10.0,
    );
    assert!(started.is_ok(), "the demo library has slides: {started:?}");
    // Every slide is "ready": without the hidden gate, 10 hidden seconds would advance the show.
    for id in app.session.visible_cloned() {
        let tex = ctx.load_texture(format!("slide-{}", id.0), egui::ColorImage::filled([2, 2], egui::Color32::WHITE), Default::default());
        app.renderer
            .textures
            .insert(Slot::Slide(Item::Photo(id)), Tex { key: 0, photo: id, tex, size: [2, 2], histogram: None, ms: 0.0, quick: None, pixels: None });
    }
    let first = app.ui.slideshow.as_ref().unwrap().play.current();
    let selection = app.session.active();
    app.set_simulated_clock(Some(10.5));
    let remaining = app.ui.slideshow.as_ref().unwrap().play.remaining(10.5);
    assert!(remaining > 0.0);
    let pending_before = app.renderer.pending_slots();

    let hidden = viewport_raw(egui::vec2(800.0, 600.0), false);
    let _ = ctx.run_logic(&hidden, |ctx| app.logic(ctx));
    assert!(app.ui.slideshow.as_ref().unwrap().play.is_paused(), "hiding auto-pauses playback");
    app.set_simulated_clock(Some(20.0));
    let _ = ctx.run_logic(&hidden, |ctx| app.logic(ctx));
    {
        let play = &app.ui.slideshow.as_ref().unwrap().play;
        assert_eq!(play.current(), first, "a hidden slideshow does not advance");
        assert!((play.remaining(20.0) - remaining).abs() < 1e-6, "the slide keeps its remaining time while hidden");
    }
    assert_eq!(app.session.active(), selection, "a hidden slideshow does not move the selection");
    assert_eq!(app.renderer.pending_slots(), pending_before, "a hidden slideshow requests no new slide renders");

    let visible = viewport_raw(egui::vec2(800.0, 600.0), true);
    app.set_simulated_clock(Some(20.0));
    let _ = ctx.run_logic(&visible, |ctx| app.logic(ctx));
    let play = &app.ui.slideshow.as_ref().unwrap().play;
    assert!(!play.is_paused(), "showing the window resumes playback");
    assert_eq!(play.current(), first);
    assert!((play.remaining(20.0) - remaining).abs() < 1e-6, "resuming does not skip the hidden minutes");
}

#[test]
fn text_field_inherits_ime_delete_surrounding_bytes_exactly() {
    // The pinned 0.37 fork sends winit's incoming byte deletion as
    // `ImeEvent::DeleteSurroundingBytes`; the app's `TextField` wraps the built-in `TextEdit`,
    // so it must apply the range exactly (not extend it to a grapheme cluster).
    let ctx = egui::Context::default();
    let id = crate::text_field::id("ime-bytes");
    let mut text = String::from("abc");
    let pass = |events: Vec<egui::Event>, text: &mut String| {
        let raw =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0))), events, ..Default::default() };
        frame(&ctx, raw, |ui| {
            ui.memory_mut(|memory| memory.request_focus(id));
            let _ = crate::text_field::TextField::singleline("ime-bytes", text).show(ui);
        });
    };
    pass(vec![], &mut text);
    pass(vec![], &mut text);
    let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(3))));
    state.store(&ctx, id);
    pass(vec![egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 2, after_bytes: 0 })], &mut text);
    assert_eq!(text, "a", "the reported UTF-8 range is deleted exactly");

    // The same path must not split a multi-byte codepoint when the IME reports its whole width.
    let mut text = String::from("aé🙂z");
    pass(vec![], &mut text);
    pass(vec![], &mut text);
    let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(3)))); // after 🙂
    state.store(&ctx, id);
    pass(vec![egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 4, after_bytes: 0 })], &mut text);
    assert_eq!(text, "aéz", "the whole emoji is removed without touching the combining-free neighbours");
}

#[test]
fn text_field_ime_commit_replaces_the_selection() {
    let ctx = egui::Context::default();
    let id = crate::text_field::id("ime-commit");
    let mut text = String::from("abc");
    let pass = |events: Vec<egui::Event>, text: &mut String| {
        let raw =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0))), events, ..Default::default() };
        frame(&ctx, raw, |ui| {
            ui.memory_mut(|memory| memory.request_focus(id));
            let _ = crate::text_field::TextField::singleline("ime-commit", text).show(ui);
        });
    };
    pass(vec![], &mut text);
    pass(vec![], &mut text);
    let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(2))));
    state.store(&ctx, id);
    pass(vec![egui::Event::Ime(egui::ImeEvent::Commit("é".into()))], &mut text);
    assert_eq!(text, "éc", "the committed text replaces the selected range once");
}

fn shot_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lightcraft-shot-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp screenshot folder");
    d
}

/// Complete one admitted capture whose slot is bound to the current geometry.
fn complete_admitted(ctx: &egui::Context, app: &mut LightcraftApp, image: egui::ColorImage) {
    let generation = app.shot_document_generation();
    let issued = geometry(100.0, 100.0);
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(ctx, shot, issued, generation)
    };
    callback.complete(Arc::new(image));
    app.collect_screenshots(ctx);
}

#[test]
fn a_new_file_at_an_absent_screenshot_path_survives() {
    let dir = shot_dir("absent-race");
    let path = dir.join("shot.png");
    let (mut app, _seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("the path is free");
    assert!(target.was_absent());
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    // A photo (or any other file) appears at the path while the capture is queued.
    std::fs::write(&path, b"new original").expect("created while queued");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("appeared"), "{reply}");
    assert_eq!(std::fs::read(&path).expect("the new file is still there"), b"new original");
    assert_eq!(app.shot_bytes, 0, "the reservation is released on refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_absent_screenshot_path_that_becomes_a_hard_link_to_an_original_survives() {
    let dir = shot_dir("absent-original");
    let original = dir.join("IMG_1.jpg");
    let path = dir.join("shot.png");
    std::fs::write(&original, b"original").expect("original");
    let (mut app, _seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("the path is free");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    std::fs::hard_link(&original, &path).expect("an imported hard link appears while queued");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(std::fs::read(&original).expect("original survives"), b"original");
    assert_eq!(std::fs::read(&path).expect("the link still names the original"), b"original");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_existing_allowed_screenshot_file_is_still_overwritten() {
    let dir = shot_dir("existing-allowed");
    let path = dir.join("shot.png");
    std::fs::write(&path, b"old screenshot").expect("old file");
    let (mut app, _seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("ordinary existing file");
    assert!(!target.was_absent());
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(std::fs::read(&path).expect("written"), vec![0x89, b'P', b'N', b'G']);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_screenshot_parent_retargeted_while_queued_is_not_written() {
    use std::os::unix::fs::symlink;

    let dir = shot_dir("parent-retargeted");
    let admitted = dir.join("admitted");
    let unexpected = dir.join("unexpected");
    let link = dir.join("destination");
    std::fs::create_dir(&admitted).expect("admitted destination folder");
    std::fs::create_dir(&unexpected).expect("unexpected destination folder");
    symlink(&admitted, &link).expect("destination folder link");
    let path = link.join("shot.png");
    let (mut app, seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("free path through a folder link");
    assert!(target.was_absent());
    assert!(app.session.revalidate_write_target(&target).is_ok(), "an unchanged folder link is a valid destination");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");

    // The path's spelling is unchanged, but it now resolves to another folder while the real
    // capture is pending. Re-checking only the saved resolved folder would allow this write.
    std::fs::remove_file(&link).expect("unlink only the test folder link");
    symlink(&unexpected, &link).expect("retarget within the test temp folder");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));

    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("destination folder changed"), "{reply}");
    assert!(!admitted.join("shot.png").exists(), "the refused capture writes nothing to the admitted folder");
    assert!(!unexpected.join("shot.png").exists(), "the refused capture writes nothing to the retargeted folder");
    assert!(seen.lock().unwrap().is_none(), "a changed destination is refused before encoding");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0, "the refused capture releases its reservation");
    assert!(rx.try_recv().is_err(), "the refused capture has exactly one reply");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn an_existing_screenshot_target_replaced_while_queued_is_not_overwritten() {
    let dir = shot_dir("existing-replaced");
    let path = dir.join("shot.png");
    let keep = dir.join("keep.png");
    let other = dir.join("other.png");
    std::fs::write(&path, b"old screenshot").expect("old file");
    let (mut app, _seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("ordinary existing file");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    // Keep the admitted inode alive, then replace the directory entry with a different file.
    std::fs::hard_link(&path, &keep).expect("keep the admitted inode");
    std::fs::write(&other, b"new file").expect("new file");
    std::fs::rename(&other, &path).expect("replace the entry");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("changed"), "{reply}");
    assert_eq!(std::fs::read(&path).expect("the replacing file survives"), b"new file");
    assert_eq!(std::fs::read(&keep).expect("the admitted file survives"), b"old screenshot");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ui_scroll_carries_validated_unit_phase_and_source() {
    let (mut app, ctx) = (app(), egui::Context::default());
    let outcome = crate::control::gesture_events(
        &mut app,
        &ctx,
        "ui.scroll",
        &serde_json::json!({"source": "wheel", "unit": "line", "phase": "start", "dy": 1.0}),
    )
    .expect("a wheel line step is valid");
    assert_eq!(outcome.result, serde_json::Value::Null);
    assert!(outcome.target.is_none(), "scrolling has no widget target to revalidate");
    match app.synthetic.first().expect("one wheel event") {
        egui::Event::MouseWheel { unit, phase, source, .. } => {
            assert_eq!(*unit, egui::MouseWheelUnit::Line);
            assert_eq!(*phase, egui::TouchPhase::Start);
            assert_eq!(*source, egui::MouseWheelSource::Wheel);
        }
        other => panic!("expected a wheel event, got {other:?}"),
    }

    app.synthetic.clear();
    crate::control::gesture_events(
        &mut app,
        &ctx,
        "ui.scroll",
        &serde_json::json!({"source": "trackpad", "unit": "point", "phase": "end", "dy": -12.0}),
    )
    .expect("a trackpad end event is valid");
    match app.synthetic.first().expect("one wheel event") {
        egui::Event::MouseWheel { unit, phase, source, .. } => {
            assert_eq!(*unit, egui::MouseWheelUnit::Point);
            assert_eq!(*phase, egui::TouchPhase::End);
            assert_eq!(*source, egui::MouseWheelSource::Trackpad);
        }
        other => panic!("expected a wheel event, got {other:?}"),
    }

    // Omitted keys keep the pre-0.37 request meaning.
    app.synthetic.clear();
    crate::control::gesture_events(&mut app, &ctx, "ui.scroll", &serde_json::json!({"dy": 3.0})).expect("defaults are valid");
    match app.synthetic.first().expect("one wheel event") {
        egui::Event::MouseWheel { unit, phase, source, .. } => {
            assert_eq!(*unit, egui::MouseWheelUnit::Point);
            assert_eq!(*phase, egui::TouchPhase::Move);
            assert_eq!(*source, egui::MouseWheelSource::Unknown);
        }
        other => panic!("expected a wheel event, got {other:?}"),
    }

    // An invalid value is an error and queues nothing.
    app.synthetic.clear();
    let error = crate::control::gesture_events(&mut app, &ctx, "ui.scroll", &serde_json::json!({"unit": "notch"})).expect_err("unknown unit");
    assert!(error.contains("unit"), "{error}");
    assert!(app.synthetic.is_empty());
}

#[test]
fn synthetic_pointer_requests_bound_clicks_and_drag_steps() {
    let (mut app, ctx) = (app(), egui::Context::default());
    app.widgets.push(("bounded:test".into(), egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(20.0, 20.0))));
    let max_clicks = crate::control::MAX_CLICKS;
    let max_steps = crate::control::MAX_DRAG_STEPS;

    for (method, params) in [
        ("ui.clickWidget", serde_json::json!({"id": "bounded:test", "count": u64::MAX})),
        ("ui.click", serde_json::json!({"x": 1.0, "y": 1.0, "count": u64::MAX})),
    ] {
        app.synthetic.clear();
        crate::control::gesture_events(&mut app, &ctx, method, &params).expect("a huge click count is bounded, not refused");
        assert_eq!(app.synthetic.len(), 1 + 2 * max_clicks as usize, "{method}: one move and two events per click");
        let presses = app.synthetic.iter().filter(|e| matches!(e, egui::Event::PointerButton { pressed: true, .. })).count();
        assert_eq!(presses, max_clicks as usize, "{method}: the click count is capped");
    }

    for (method, params) in [
        ("ui.dragWidget", serde_json::json!({"id": "bounded:test", "toX": 50.0, "toY": 50.0, "steps": u64::MAX})),
        ("ui.drag", serde_json::json!({"x": 1.0, "y": 1.0, "toX": 50.0, "toY": 50.0, "steps": u64::MAX})),
    ] {
        app.synthetic.clear();
        crate::control::gesture_events(&mut app, &ctx, method, &params).expect("a huge drag step count is bounded, not refused");
        assert_eq!(app.synthetic.len(), 3 + max_steps as usize, "{method}: press, release and the capped moves");
        let moves = app.synthetic.iter().filter(|e| matches!(e, egui::Event::PointerMoved(_))).count();
        assert_eq!(moves, 1 + max_steps as usize, "{method}: the step count is capped");
    }
}

#[test]
fn a_gesture_never_uses_the_previous_widget_registry() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    app.widgets.push(("stale:test".into(), egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0))));
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.clickWidget".into(), serde_json::json!({"id": "stale:test"}), tx).expect("admitted");
    assert!(rx.try_recv().is_err(), "nothing is answered at admission");
    // A completed pass replaces the registry; the stale id is not in the fresh one.
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    let reply = rx.try_recv().expect("the resolution error answers once");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("no widget"), "{reply}");
    assert!(app.pending_gestures.is_empty());
    assert!(app.synthetic.is_empty());
}

#[test]
fn a_gesture_cancels_when_the_view_changes_before_the_fresh_layout() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({"x": 10.0, "y": 10.0}), tx).expect("admitted");
    // the app opens in Detail, so a cancellation test must move to a genuinely different view
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    let reply = rx.try_recv().expect("the cancel error answers once");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("canceled"), "{reply}");
    assert!(app.synthetic.is_empty());
}

#[test]
fn a_widget_gesture_cancels_if_its_target_moves_between_resolution_and_the_press() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    // Resolve a click against a completed layout at the widget's old place.
    app.widgets.push(("moving:test".into(), egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(30.0, 20.0))));
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.clickWidget".into(), serde_json::json!({"id": "moving:test"}), tx).expect("admitted");
    app.layout_epoch = app.layout_epoch.wrapping_add(1);
    app.resolve_pending_gestures(&ctx);
    assert_eq!(app.synthetic.len(), 3, "move, press and release are queued");
    assert!(rx.try_recv().is_err(), "nothing is answered at resolution");
    // The first pass consumes the move; the pointer now rests at the resolved point.
    let mut next = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut next);
    assert!(next.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(p) if *p == egui::pos2(25.0, 20.0))));
    app.synthetic_in_flight = false;
    // The next completed layout puts the widget somewhere else. egui would attribute the press
    // in the following pass to whatever sits at the old coordinates; fail closed instead.
    app.widgets = vec![("moving:test".into(), egui::Rect::from_min_size(egui::pos2(100.0, 10.0), egui::vec2(30.0, 20.0)))];
    let mut next = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut next);
    assert!(next.events.is_empty(), "no press is delivered at the stale point");
    assert!(app.synthetic.is_empty(), "the rest of the canceled click is dropped");
    let reply = rx.try_recv().expect("the moved target answers once");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("moving:test"), "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("nothing was retargeted"), "{reply}");
    assert!(app.pending_gestures.is_empty());
}

#[test]
fn a_maximum_step_drag_is_delivered_past_the_five_second_stall_limit() {
    let mut h = Headless::new(app(), [900.0, 650.0], 1.0);
    h.step();
    h.step();
    assert!(h.app.fonts_ready);
    // A capped drag is delivered one event per host frame. At 1/60 s each the whole gesture takes
    // far longer than GESTURE_TIMEOUT_MS, so only a deadline refreshed by real progress lets it
    // finish; the old absolute deadline failed it (and every queued gesture) halfway through.
    let r = h.request(
        "ui.drag",
        serde_json::json!({
            "x": 30.0,
            "y": 30.0,
            "toX": 30.0,
            "toY": 30.0,
            "steps": crate::control::MAX_DRAG_STEPS,
        }),
        Duration::from_secs(90),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.time() > 5.0, "the drag's simulated delivery outlived the old 5 s deadline: {}", h.time());
    assert!(h.app.pending_gestures.is_empty());
    assert!(h.app.synthetic.is_empty());
}

#[test]
fn a_click_on_a_control_under_a_nonmodal_overlay_is_refused_without_activating_the_overlay() {
    let mut h = Headless::new(app(), [1400.0, 900.0], 1.0);
    let photo = h.app.session.visible_cloned()[0];
    let t = Duration::from_secs(20);
    assert_eq!(h.request("engine.execute", serde_json::json!({"command": "library.select", "params": {"ids": [photo.0]}}), t)["ok"], true);
    assert_eq!(h.request("ui.set", serde_json::json!({"view": "detail", "right": "edit"}), t)["ok"], true);
    h.app.ui.open_sections = vec!["light".to_string()];
    assert!(h.settle(Duration::from_secs(60)), "the edit panel settles");
    let id = "slider:light.exposure";
    assert!(h.app.widgets.iter().any(|(widget, _)| widget == id), "{id} is on screen");
    let before = h.app.session.develop_of(photo).expect("its develop settings").light.exposure;
    // The quit prompt's Foreground dim and window own the pointer (and the notice barrier also
    // covers the control channel's own hit check). The settings slider remains registered
    // underneath it, and its rect still contains the press point.
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    assert!(h.app.widgets.iter().any(|(widget, _)| widget == "button:quitCancel"), "the overlay is shown");
    let r = h.request("ui.clickWidget", serde_json::json!({"id": id}), Duration::from_secs(20));
    assert_eq!(r["ok"], false, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(error.contains(id), "{r}");
    assert!(error.contains("no longer visible"), "{r}");
    assert!(error.contains("nothing was retargeted"), "{r}");
    assert!(h.app.quit_prompt.is_some(), "the click did not activate (or dismiss) the overlay");
    let r = h.request("ui.dragWidget", serde_json::json!({"id": id, "dx": 12.0, "steps": 4}), Duration::from_secs(20));
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap_or_default().contains("before its press"), "{r}");
    let after = h.app.session.develop_of(photo).expect("its develop settings").light.exposure;
    assert_eq!(after, before, "the covered slider did not change");
    assert!(h.app.quit_prompt.is_some(), "the drag did not activate (or dismiss) the overlay");
    assert!(h.app.pending_gestures.is_empty());
    assert!(h.app.synthetic.is_empty());
}

/// The release ownership check must not reject an ordinary unobstructed click, including a click
/// on a sub-rect of a larger widget (the bottom rating row registers each star, not the row's
/// interactive centre). This is the positive control the hover-only probe lacked.
#[test]
fn an_unobstructed_click_on_a_subrect_reaches_the_widget() {
    let mut h = Headless::new(app(), [1400.0, 900.0], 1.0);
    let photo = h.app.session.visible_cloned()[0];
    let t = Duration::from_secs(20);
    assert_eq!(h.request("engine.execute", serde_json::json!({"command": "library.select", "params": {"ids": [photo.0]}}), t)["ok"], true);
    assert_eq!(h.request("ui.set", serde_json::json!({"view": "detail", "right": "edit"}), t)["ok"], true);
    assert!(h.settle(Duration::from_secs(60)), "the detail view settles");
    let id = "star:bottom:2";
    assert!(h.app.widgets.iter().any(|(widget, _)| widget == id), "{id} is on screen");
    assert!(h.app.widget_hits.iter().any(|(widget, _)| widget == id), "{id} has a probe");
    let r = h.request("ui.clickWidget", serde_json::json!({"id": id}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(photo).expect("the photo").rating, 2, "the sub-rect click reached the rating row");
}

/// A non-modal overlay that appears after the press must cancel the click before its release and
/// must not turn the release into an overlay click; the pointer is released away from the target.
#[test]
fn an_overlay_that_appears_after_the_press_cancels_the_release() {
    let mut h = Headless::new(app(), [1000.0, 760.0], 1.0);
    h.step();
    let t = Duration::from_secs(20);
    assert_eq!(h.request("ui.key", serde_json::json!({"key": ",", "cmd": true}), t)["ok"], true);
    assert!(h.settle(Duration::from_secs(60)), "the Settings dialog settles");
    let id = "check:settings.confirmDelete";
    let target = h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| *rect).expect("the checkbox is on screen");
    let before = h.app.ui.settings.confirm_delete;
    let (tx, rx) = reply_channel();
    h.app.admit_gesture(&h.view.ctx.clone(), "ui.clickWidget".into(), serde_json::json!({"id": id}), tx).expect("the click is admitted");
    h.step(); // resolve: move, press and release are queued
    h.step(); // the move is consumed
    h.step(); // the press is consumed: the pointer is now down
    assert!(h.view.ctx.input(|i| i.pointer.any_down()), "the press reached egui");
    // Let the prompt complete its sizing pass while the release waits, as it does when it opens
    // between two host frames. A prompt now cancels the gesture at its appearance, before the
    // release is delivered; the release must still end the held press without a click.
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.app.synthetic_in_flight = true;
    h.step();
    assert!(!h.app.synthetic_in_flight, "the prompt's pass completed");
    assert!(h.app.widgets.iter().any(|(widget, _)| widget == "button:quitCancel"), "the prompt is laid out");
    h.step(); // the release is refused; the pointer cleanup releases the held press
    let reply = rx.try_recv().expect("the canceled click answers");
    assert_eq!(reply["ok"], false, "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("modal prompt appeared"), "{reply}");
    assert!(error.contains("before its input was delivered"), "{reply}");
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the canceled press was released");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the covered checkbox did not change");
    assert_eq!(
        h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| *rect),
        Some(target),
        "the target stayed where it was; nothing was retargeted"
    );
    assert!(h.app.quit_prompt.is_some(), "the prompt was not activated");
    // Remove the overlay: the same target is clickable again (a positive control after refusal).
    h.app.quit_prompt = None;
    assert!(h.settle(Duration::from_secs(60)), "the Settings dialog settles after the overlay");
    let r = h.request("ui.clickWidget", serde_json::json!({"id": id}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.settings.confirm_delete, !before, "the unobstructed click reached the checkbox");
}

/// A canceled click on a drag-sensing control must not keep the pointer down or move it. The old
/// cleanup moved 8 px to suppress the click, which could start a drag and change a slider/curve.
#[test]
fn canceling_a_pressed_slider_releases_it_without_dragging_it() {
    let mut h = Headless::new(app(), [1400.0, 900.0], 1.0);
    let photo = h.app.session.visible_cloned()[0];
    let t = Duration::from_secs(20);
    assert_eq!(h.request("engine.execute", serde_json::json!({"command": "library.select", "params": {"ids": [photo.0]}}), t)["ok"], true);
    assert_eq!(h.request("ui.set", serde_json::json!({"view": "detail", "right": "edit"}), t)["ok"], true);
    h.app.ui.open_sections = vec!["light".to_string()];
    assert!(h.settle(Duration::from_secs(60)), "the edit panel settles");
    let id = "slider:light.exposure";
    let target = h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| *rect).expect("the slider is on screen");
    let before = h.app.session.develop_of(photo).expect("its develop settings").light.exposure;
    let (tx, rx) = reply_channel();
    h.app.admit_gesture(&h.view.ctx.clone(), "ui.clickWidget".into(), serde_json::json!({"id": id}), tx).expect("the click is admitted");
    h.step(); // resolve: move, press and release are queued
    h.step(); // the move is consumed
    h.step(); // the press is consumed: the pointer is now down
    assert!(h.view.ctx.input(|i| i.pointer.any_down()), "the press reached egui");
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.app.synthetic_in_flight = true;
    h.step(); // the prompt's pass completes while the release waits
    assert!(!h.app.synthetic_in_flight);
    h.step(); // the release is refused; cleanup resets the pointer before the next pass
    let reply = rx.try_recv().expect("the canceled click answers");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("modal prompt appeared"), "{reply}");
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the canceled press was released without a move");
    assert_eq!(h.app.session.develop_of(photo).expect("its develop settings").light.exposure, before, "cleanup did not drag the slider");
    assert_eq!(
        h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| *rect),
        Some(target),
        "the target stayed where it was; nothing was retargeted"
    );
    assert!(h.app.quit_prompt.is_some(), "the prompt was not activated");
    assert!(h.app.pending_gestures.is_empty());
    assert!(h.app.synthetic.is_empty());
}

/// A non-prompt `Foreground` window that appears after the press (the launch list / library error)
/// makes the target invisible too: the release is refused rather than clicking through it.
#[test]
fn a_nonprompt_overlay_that_appears_after_the_press_cancels_the_release() {
    let mut h = Headless::new(app(), [1000.0, 760.0], 1.0);
    h.step();
    let t = Duration::from_secs(20);
    assert_eq!(h.request("ui.key", serde_json::json!({"key": ",", "cmd": true}), t)["ok"], true);
    assert!(h.settle(Duration::from_secs(60)), "the Settings dialog settles");
    let id = "check:settings.confirmDelete";
    let target = h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| *rect).expect("the checkbox is on screen");
    let before = h.app.ui.settings.confirm_delete;
    let (tx, rx) = reply_channel();
    h.app.admit_gesture(&h.view.ctx.clone(), "ui.clickWidget".into(), serde_json::json!({"id": id}), tx).expect("the click is admitted");
    h.step(); // resolve: move, press and release are queued
    h.step(); // the move is consumed
    h.step(); // the press is consumed: the pointer is now down
    assert!(h.view.ctx.input(|i| i.pointer.any_down()), "the press reached egui");
    // Draw the non-prompt overlay while the release waits. Its creation barrier cancels the
    // queued release before the later stale-target visibility check can deliver it.
    h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::new("/no/such/library", "test error"));
    h.app.synthetic_in_flight = true;
    h.step();
    assert!(!h.app.synthetic_in_flight, "the overlay's pass completed");
    assert!(h.app.widgets.iter().any(|(widget, _)| widget == "button:libraryRetry"), "the library window is laid out");
    h.step();
    let reply = rx.try_recv().expect("the canceled click answers");
    assert_eq!(reply["ok"], false, "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("ui.clickWidget canceled: a blocking library window appeared"), "{reply}");
    assert!(error.contains("before its input was delivered"), "{reply}");
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the canceled press was released");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the covered checkbox did not change");
    assert_eq!(
        h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| *rect),
        Some(target),
        "the target stayed where it was; nothing was retargeted"
    );
    assert!(h.app.library_problem.is_some(), "the overlay was not activated");
    h.app.library_problem = None;
    assert!(h.settle(Duration::from_secs(60)), "Settings settles after the overlay");
    let r = h.request("ui.clickWidget", serde_json::json!({"id": id}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.settings.confirm_delete, !before, "the unobstructed click reached the checkbox");
}

/// The library-problem dim is above a `Middle` Settings dialog: a control-channel click on the
/// covered checkbox is refused instead of being delivered through the dim. This is the direct
/// `ui.clickWidget` counterpart of the press/release cancellation above.
#[test]
fn a_library_problem_overlay_blocks_a_control_click_in_settings() {
    let mut h = Headless::new(app(), [1000.0, 760.0], 1.0);
    let t = Duration::from_secs(20);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    assert!(h.settle(Duration::from_secs(60)), "Settings settles");
    let id = "check:settings.confirmDelete";
    let before = h.app.ui.settings.confirm_delete;
    h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::new("/no/such/library", "test error"));
    assert!(h.settle(Duration::from_secs(60)), "the library problem window settles");
    assert!(h.app.widgets.iter().any(|(widget, _)| widget == "button:libraryRetry"), "the library window is laid out");
    let r = h.request("ui.clickWidget", serde_json::json!({"id": id}), t);
    assert_eq!(r["ok"], false, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(error.contains(id), "{r}");
    assert!(error.contains("no longer visible"), "{r}");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the covered checkbox did not change");
    assert!(h.app.library_problem.is_some(), "the overlay was not activated");
    // Positive control: the same control works once the covering window is gone.
    h.app.library_problem = None;
    assert!(h.settle(Duration::from_secs(60)), "Settings settles after the overlay");
    let r = h.request("ui.clickWidget", serde_json::json!({"id": id}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.settings.confirm_delete, !before, "the unobstructed click reached the checkbox");
}

/// A quit prompt over Settings owns the pointer even when Settings asks for outside clicks: the
/// raw backdrop handler must not close the dialog it cannot see. Dismissing the prompt with its
/// own button keeps Settings and does not replay the backdrop press into it.
#[test]
fn a_quit_prompt_over_settings_blocks_the_outside_click_and_keeps_the_dialog() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    let t = Duration::from_secs(20);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Settings is open");
    h.app.ui.settings.close_dialogs_outside = true;
    let confirm_delete = h.app.ui.settings.confirm_delete;
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    assert!(h.app.widgets.iter().any(|(id, _)| id == "button:quitCancel"), "the prompt is laid out");
    // The prompt's first completed frame is already the real, interactable layer: no sizing pass
    // (which egui makes invisible and non-interactable) may leak a release into Settings below.
    let cancel = h.app.widgets.iter().find(|(id, _)| id == "button:quitCancel").map(|(_, r)| r.center()).expect("the Cancel rect");
    assert!(h.view.ctx.layer_id_at(cancel) == crate::panels::notices::prompt_layer(&h.app), "the prompt owns its first settled button click");
    // Outside both the prompt and Settings, with the outside-click preference on.
    let at = egui::pos2(10.0, 890.0);
    let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(at), press(true), press(false)]);
    h.step();
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "the backdrop click did not close Settings");
    assert!(h.app.quit_prompt.is_some(), "the backdrop click did not dismiss the prompt");
    assert_eq!(h.app.ui.settings.confirm_delete, confirm_delete, "the raw click did not reach a Settings control");
    // A backdrop press raises its area; the prompt must stay above it, and its Cancel must work.
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "button:quitCancel"}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.quit_prompt.is_none(), "Cancel dismissed the prompt");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "dismissing the prompt kept Settings");
    assert_eq!(h.app.ui.settings.confirm_delete, confirm_delete, "the prompt click did not reach Settings");
}

/// Escape and Return are the prompt's safe Cancel, not Settings' dismissal/confirmation. The
/// dialog and its Settings stay exactly as they were, then keep their normal Escape afterwards.
#[test]
fn a_quit_prompt_takes_safe_keyboard_cancel_without_closing_settings() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    let t = Duration::from_secs(20);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Settings is open");
    let confirm_delete = h.app.ui.settings.confirm_delete;
    for key in ["Escape", "Enter"] {
        h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
        h.step();
        let r = h.request("ui.key", serde_json::json!({"key": key}), t);
        assert_eq!(r["ok"], true, "{key}: {r}");
        assert!(h.app.quit_prompt.is_none(), "{key} safely dismissed the prompt");
        assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "{key} did not close Settings behind the prompt");
        assert_eq!(h.app.ui.settings.confirm_delete, confirm_delete, "{key} did not change Settings");
    }
    // Positive control: the dialog's own Escape still works once no prompt owns it.
    let r = h.request("ui.key", serde_json::json!({"key": "Escape"}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none(), "Escape closes Settings without a prompt");
}

/// The prompt's buttons are reachable with Tab/Shift+Tab and activate with Space. Focus is real
/// egui focus on the actual button responses, not a separate keyboard-only path.
#[test]
fn prompt_tab_shift_tab_and_space_reach_cancel() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    let t = Duration::from_secs(20);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    let confirm_delete = h.app.ui.settings.confirm_delete;
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    let cancel_id =
        h.app.widget_hits.iter().rev().find(|(id, _)| id == "button:quitCancel").map(|(_, hit)| hit.id).expect("Cancel has a real response");
    let anyway_id =
        h.app.widget_hits.iter().rev().find(|(id, _)| id == "button:quitAnyway").map(|(_, hit)| hit.id).expect("Quit Anyway has a real response");
    assert_eq!(h.view.ctx.memory(|m| m.focused()), Some(cancel_id), "the safe Cancel starts focused");
    assert_eq!(h.request("ui.key", serde_json::json!({"key": "Tab"}), t)["ok"], true);
    assert_eq!(h.view.ctx.memory(|m| m.focused()), Some(anyway_id), "Tab moved to Quit Anyway");
    assert!(h.app.quit_prompt.is_some(), "Tab did not activate anything");
    assert_eq!(h.request("ui.key", serde_json::json!({"key": "Tab", "shift": true}), t)["ok"], true);
    assert_eq!(h.view.ctx.memory(|m| m.focused()), Some(cancel_id), "Shift+Tab moved back to Cancel");
    assert_eq!(h.request("ui.key", serde_json::json!({"key": "Space"}), t)["ok"], true);
    assert!(h.app.quit_prompt.is_none(), "Space activated the focused safe Cancel");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Cancel kept Settings");
    assert_eq!(h.app.ui.settings.confirm_delete, confirm_delete, "the prompt key did not reach Settings");
}

/// Raw screen coordinates at a settled prompt button still work. Pointer state is hidden from the
/// background during each pass, but the real press is preserved for the next frame's release.
#[test]
fn raw_pointer_at_a_settled_prompt_button_activates_it() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    let confirm_delete = h.app.ui.settings.confirm_delete;
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    let cancel = h.app.widgets.iter().rev().find(|(id, _)| id == "button:quitCancel").map(|(_, rect)| rect.center()).expect("the Cancel rect");
    h.events.extend([egui::Event::PointerMoved(cancel), pointer_event(cancel, true)]);
    h.step();
    assert!(h.app.quit_prompt.is_some(), "the press alone does not dismiss the prompt");
    assert!(h.view.ctx.input(|i| i.pointer.any_down()), "the real press survives for its release");
    h.events.push(pointer_event(cancel, false));
    h.step();
    assert!(h.app.quit_prompt.is_none(), "the release activated the settled prompt button");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Cancel kept Settings");
    assert_eq!(h.app.ui.settings.confirm_delete, confirm_delete, "the raw click did not reach Settings");
}

/// AccessKit Focus and Click target the prompt's actual button node. An action at a covered
/// Settings control is dropped, so it can neither focus nor activate the background.
#[test]
fn prompt_accepts_targeted_accesskit_and_rejects_background_actions() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    let t = Duration::from_secs(20);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    let confirm_delete = h.app.ui.settings.confirm_delete;
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    let cancel_id =
        h.app.widget_hits.iter().rev().find(|(id, _)| id == "button:quitCancel").map(|(_, hit)| hit.id).expect("Cancel has a real response");
    let checkbox_node = h
        .app
        .widget_hits
        .iter()
        .rev()
        .find(|(id, _)| id == "check:settings.confirmDelete")
        .map(|(_, hit)| hit.id.accesskit_id())
        .expect("the covered checkbox has a response");
    h.events.push(accesskit_action(egui::accesskit::Action::Click, checkbox_node));
    h.step();
    assert!(h.app.quit_prompt.is_some(), "a background AccessKit Click did not dismiss the prompt");
    assert_eq!(h.app.ui.settings.confirm_delete, confirm_delete, "a background AccessKit Click did not change Settings");
    h.events.push(accesskit_action(egui::accesskit::Action::Focus, cancel_id.accesskit_id()));
    h.events.push(accesskit_action(egui::accesskit::Action::Click, cancel_id.accesskit_id()));
    h.step();
    assert!(h.app.quit_prompt.is_none(), "the prompt's own AccessKit node activated Cancel");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "AccessKit Cancel kept Settings");
    assert_eq!(h.app.ui.settings.confirm_delete, confirm_delete, "the AccessKit action did not reach Settings");
    // The control still answers normally after the prompt is gone.
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "check:settings.confirmDelete"}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.settings.confirm_delete, !confirm_delete, "Settings is usable again");
}

/// A prompt created in the same host frame as a raw press/release must shield it. A press that
/// began before the prompt is released by the creation barrier, without becoming a background
/// click after the prompt settles.
#[test]
fn a_new_prompt_shields_raw_pointer_and_cancels_a_held_press() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    let id = "check:settings.confirmDelete";
    let at = h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| rect.center()).expect("the checkbox is on screen");
    let before = h.app.ui.settings.confirm_delete;
    // Press and release in the prompt's creation frame.
    h.events.extend([egui::Event::PointerMoved(at), pointer_event(at, true), pointer_event(at, false)]);
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    assert!(h.app.quit_prompt.is_some(), "the raw click did not dismiss the prompt");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the creation-frame click did not reach Settings");
    // Dismiss that prompt, then start a press over Settings before a new prompt appears.
    assert_eq!(h.request("ui.key", serde_json::json!({"key": "Escape"}), Duration::from_secs(20))["ok"], true);
    h.step();
    h.events.extend([egui::Event::PointerMoved(at), pointer_event(at, true)]);
    h.step();
    assert!(h.view.ctx.input(|i| i.pointer.any_down()), "the pre-prompt press reached egui");
    h.events.push(pointer_event(at, false));
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the prompt creation cancelled the held press");
    assert!(h.app.quit_prompt.is_some(), "the pre-prompt release did not activate the prompt");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the press-before-prompt became a click in Settings");
}

/// A prompt can be created after `raw_input_hook`, when eframe's `logic` runs. The prompt barrier
/// must then drop wheel/new-touch/pointer input that egui already accepted for the pass, before
/// any panel reads it, while a touch End still clears egui's private touch state.
#[test]
fn a_prompt_first_seen_after_begin_pass_drops_wheel_touch_and_background_pointer_input() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.fonts_ready = true;
    let at = egui::pos2(30.0, 40.0);
    let mut input = raw(egui::vec2(800.0, 600.0));
    input.events = vec![
        key_event(egui::Key::A, true, egui::Modifiers::NONE),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 2.0),
            phase: egui::TouchPhase::Start,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase: egui::TouchPhase::Start, pos: at, force: None },
        egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase: egui::TouchPhase::End, pos: at, force: None },
        egui::Event::PointerMoved(at),
        pointer_event(at, true),
        egui::Event::ModifiersChanged(egui::Modifiers::NONE),
    ];
    let mut output = ctx.run_ui(input, |ui| {
        app.quit_prompt = Some(crate::QuitPrompt::Tasks("tasks".into()));
        assert!(crate::panels::notices::prepare_ui_pass(&mut app, ui.ctx()), "the prompt is first seen after begin_pass");
        let events = ui.ctx().input(|i| i.events.clone());
        assert!(
            events.iter().all(|event| !matches!(
                event,
                egui::Event::Key { .. }
                    | egui::Event::Touch { phase: egui::TouchPhase::Start | egui::TouchPhase::Move, .. }
                    | egui::Event::MouseWheel { .. }
                    | egui::Event::PointerMoved(_)
                    | egui::Event::PointerButton { .. }
            )),
            "background input must not survive the prompt's creation pass: {events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(event, egui::Event::Touch { phase: egui::TouchPhase::End, .. })),
            "a touch End must still clear egui's private touch state: {events:?}"
        );
        assert!(events.iter().any(|event| matches!(event, egui::Event::ModifiersChanged(_))), "modifier changes still reach egui");
        assert!(!ui.ctx().input(|i| i.pointer.any_down()), "the pointer is not left down");
        assert_eq!(ui.ctx().input(|i| i.smooth_scroll_delta), egui::Vec2::ZERO, "wheel scrolling is cleared");
    });
    output.textures_delta.clear();
}

/// A held host press in the same frame as a prompt created after `raw_input_hook` is released by
/// the prompt's first pass, not delivered to the covered control when the release arrives.
#[test]
fn a_prompt_created_after_raw_input_hook_shields_a_held_background_press() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    let id = "check:settings.confirmDelete";
    let at = h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| rect.center()).expect("the checkbox is on screen");
    let before = h.app.ui.settings.confirm_delete;
    h.events.extend([egui::Event::PointerMoved(at), pointer_event(at, true)]);
    h.set_after_raw_input_hook(|app| app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into())));
    h.step();
    assert!(h.app.quit_prompt.is_some(), "the prompt is shown");
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the creation pass released the held press");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the press did not reach Settings");
    h.events.push(pointer_event(at, false));
    h.step();
    assert!(h.app.quit_prompt.is_some(), "the release did not activate the prompt");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the release did not become a background click");
}

/// A synthetic press already delivered when `raw_input_hook` runs, then a prompt created by logic
/// before the UI pass: the whole in-flight batch was not seen by a panel and must fail, not report
/// success after the prompt filters it.
#[test]
fn a_synthetic_press_already_delivered_is_canceled_by_a_prompt_created_after_raw_input_hook() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    assert!(h.settle(Duration::from_secs(60)), "the Settings dialog settles before its target rect is captured");
    let id = "check:settings.confirmDelete";
    let target = h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| *rect).expect("the checkbox is on screen");
    let before = h.app.ui.settings.confirm_delete;
    let (tx, rx) = reply_channel();
    h.app.admit_gesture(&h.view.ctx.clone(), "ui.clickWidget".into(), serde_json::json!({"id": id}), tx).expect("the click is admitted");
    h.step(); // resolve: move, press and release are queued
    h.step(); // the move is consumed
    h.step(); // the press is consumed: the pointer is now down
    assert!(h.view.ctx.input(|i| i.pointer.any_down()), "the press reached egui");
    assert!(!h.app.synthetic.is_empty(), "the release is queued");
    h.set_after_raw_input_hook(|app| app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into())));
    h.step(); // the hook creates the prompt after the release was injected for this pass
    let reply = rx.try_recv().expect("the canceled click answers");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("modal prompt appeared"), "{reply}");
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the canceled press was released");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the filtered release did not reach Settings");
    assert_eq!(
        h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| *rect),
        Some(target),
        "the target stayed where it was; nothing was retargeted"
    );
    assert!(h.app.quit_prompt.is_some(), "the prompt was not activated");
    assert!(h.app.pending_gestures.is_empty());
    assert!(h.app.synthetic.is_empty());
}

/// Raw `ui.click`, `ui.drag` and `ui.pointer` at a settled prompt are delivered to the prompt's
/// backdrop, not to the covered Settings control or image. The dialog, its control and the image
/// stay unchanged.
#[test]
fn raw_background_gestures_at_a_settled_prompt_do_not_reach_the_covered_view() {
    let mut h = Headless::new(app(), [1400.0, 900.0], 1.0);
    let photo = h.app.session.visible_cloned()[0];
    let t = Duration::from_secs(20);
    assert_eq!(h.request("engine.execute", serde_json::json!({"command": "library.select", "params": {"ids": [photo.0]}}), t)["ok"], true);
    assert_eq!(h.request("ui.set", serde_json::json!({"view": "detail", "right": "edit"}), t)["ok"], true);
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    assert!(h.settle(Duration::from_secs(60)), "the detail view and Settings settle");
    let image = h.app.image_rect.expect("the detail image is on screen");
    let at = image.min + egui::vec2(12.0, 12.0);
    let before = h.app.ui.settings.confirm_delete;
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    h.step();
    assert!(h.app.widgets.iter().any(|(widget, _)| widget == "button:quitCancel"), "the prompt is settled");
    let click = h.request("ui.click", serde_json::json!({"x": at.x, "y": at.y}), t);
    assert_eq!(click["ok"], true, "{click}");
    h.step();
    let drag = h.request("ui.drag", serde_json::json!({"x": at.x, "y": at.y, "toX": at.x + 30.0, "toY": at.y + 20.0, "steps": 4}), t);
    assert_eq!(drag["ok"], true, "{drag}");
    h.step();
    let pointer = h.request(
        "ui.pointer",
        serde_json::json!({"events": [
            {"kind": "down", "x": 0.05, "y": 0.05},
            {"kind": "move", "x": 0.08, "y": 0.08},
            {"kind": "up", "x": 0.08, "y": 0.08}
        ]}),
        t,
    );
    assert_eq!(pointer["ok"], true, "{pointer}");
    h.step();
    assert!(h.app.quit_prompt.is_some(), "the raw background gestures did not dismiss the prompt");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the raw gestures did not reach Settings");
    assert_eq!(h.app.image_rect, Some(image), "the raw pointer gestures did not pan, zoom or drag the image");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Settings is still open");
}

/// Releases are not keyboard actions, but egui needs them to clear `keys_down`. A prompt must keep
/// the release (and the held key must not survive the prompt) even though it drops every press.
#[test]
fn a_prompt_keeps_key_releases_and_clears_held_keys() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.fonts_ready = true;
    app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    let mut raw = raw(egui::vec2(800.0, 600.0));
    raw.events.push(key_event(egui::Key::Backslash, false, egui::Modifiers::NONE));
    app.raw_input_hook(&ctx, &mut raw);
    assert!(
        raw.events.iter().any(|event| matches!(event, egui::Event::Key { key: egui::Key::Backslash, pressed: false, .. })),
        "the prompt's raw filter kept the key release for keys_down"
    );
    // A held background key is cleared at the prompt transition, not left asserted under it.
    let mut h = Headless::new(app, [800.0, 600.0], 1.0);
    h.step();
    h.view.ctx.input_mut(|i| {
        i.keys_down.insert(egui::Key::Backslash);
    });
    h.events.push(key_event(egui::Key::Backslash, false, egui::Modifiers::NONE));
    h.step();
    assert!(!h.view.ctx.input(|i| i.key_down(egui::Key::Backslash)), "the release cleared the held key under the prompt");
}

/// A synthetic drag whose press already reached egui is aborted when a prompt appears: its queued
/// moves/release are discarded, the pointer is released without a click, and the slider is intact.
#[test]
fn a_prompt_aborts_an_in_progress_synthetic_drag() {
    let mut h = Headless::new(app(), [1400.0, 900.0], 1.0);
    let photo = h.app.session.visible_cloned()[0];
    let t = Duration::from_secs(20);
    assert_eq!(h.request("engine.execute", serde_json::json!({"command": "library.select", "params": {"ids": [photo.0]}}), t)["ok"], true);
    assert_eq!(h.request("ui.set", serde_json::json!({"view": "detail", "right": "edit"}), t)["ok"], true);
    h.app.ui.open_sections = vec!["light".to_string()];
    assert!(h.settle(Duration::from_secs(60)), "the edit panel settles");
    let id = "slider:light.exposure";
    let before = h.app.session.develop_of(photo).expect("its develop settings").light.exposure;
    let (tx, rx) = reply_channel();
    h.app
        .admit_gesture(&h.view.ctx.clone(), "ui.dragWidget".into(), serde_json::json!({"id": id, "dx": 40.0, "steps": 20}), tx)
        .expect("the drag is admitted");
    h.step(); // resolve: move, press and many moves are queued
    h.step(); // move
    h.step(); // press
    assert!(h.view.ctx.input(|i| i.pointer.any_down()), "the drag press reached egui");
    assert!(!h.app.synthetic.is_empty(), "the drag still has queued moves");
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    let reply = rx.try_recv().expect("the aborted drag answers");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("modal prompt appeared"), "{reply}");
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the drag's pointer was released");
    assert_eq!(h.app.session.develop_of(photo).expect("its develop settings").light.exposure, before, "the drag did not continue under the prompt");
    assert!(h.app.synthetic.is_empty(), "the rest of the drag was dropped");
    assert!(h.app.pending_gestures.is_empty());
    assert!(h.app.quit_prompt.is_some(), "the drag did not dismiss the prompt");
}

/// Changing from a Tasks prompt to an Unsaved prompt, or from one settings notice to the next,
/// is a new prompt. Its creation frame must shield pointer input even though the old prompt was
/// already settled.
#[test]
fn prompt_queue_and_kind_transitions_shield_the_first_frame() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    let id = "check:settings.confirmDelete";
    let at = h.app.widgets.iter().rev().find(|(widget, _)| widget == id).map(|(_, rect)| rect.center()).expect("the checkbox is on screen");
    let before = h.app.ui.settings.confirm_delete;
    // A raw click resolved before the prompt exists must be refused before its first event, not
    // land in the prompt's creation frame or be replayed after it goes away.
    let (tx, rx) = reply_channel();
    h.app.admit_gesture(&h.view.ctx.clone(), "ui.click".into(), serde_json::json!({"x": at.x, "y": at.y}), tx).expect("the raw click is admitted");
    h.step();
    assert!(!h.app.synthetic.is_empty(), "the raw click is queued");
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("tasks".into()));
    h.step();
    let reply = rx.try_recv().expect("the pre-prompt raw click answers");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("modal prompt appeared"), "{reply}");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the queued raw click did not reach Settings");
    // Tasks -> Unsaved in the same host frame as a raw click on the covered checkbox.
    h.events.extend([egui::Event::PointerMoved(at), pointer_event(at, true), pointer_event(at, false)]);
    h.app.quit_prompt = Some(crate::QuitPrompt::Unsaved("unsaved".into()));
    h.step();
    assert!(matches!(h.app.quit_prompt, Some(crate::QuitPrompt::Unsaved(_))), "the new prompt is shown");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the transition frame did not click Settings");
    // Dismiss Unsaved, then switch from one settings notice to the next in a pointer frame.
    assert_eq!(h.request("ui.key", serde_json::json!({"key": "Escape"}), Duration::from_secs(20))["ok"], true);
    h.app.notices = vec!["first notice".into()];
    h.step();
    h.step();
    h.events.extend([egui::Event::PointerMoved(at), pointer_event(at, true), pointer_event(at, false)]);
    h.app.notices = vec!["second notice".into()];
    h.step();
    assert_eq!(h.app.notices, vec!["second notice".to_string()], "the queued notice is still shown");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "the notice transition did not click Settings");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Settings survived both transitions");
}

/// A prompt with no dialog must still stop global shortcuts. The rating key is a background
/// photo action, not part of the prompt.
#[test]
fn a_quit_prompt_blocks_global_rating_shortcuts() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    let t = Duration::from_secs(20);
    let photo = h.app.session.visible_cloned()[0];
    assert_eq!(h.request("engine.execute", serde_json::json!({"command": "library.select", "params": {"ids": [photo.0]}}), t)["ok"], true);
    h.step();
    // Positive control: the same key rates the photo while no prompt is up.
    let r = h.request("ui.key", serde_json::json!({"key": "4"}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(photo).expect("the photo").rating, 4, "the rating key works without a prompt");
    let before = h.app.session.catalog.photo(photo).expect("the photo").rating;
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    let r = h.request("ui.key", serde_json::json!({"key": "5"}), t);
    assert_eq!(r["ok"], false, "the prompt blocked the key, so it must not report a delivered shortcut: {r}");
    assert!(r["error"].as_str().unwrap_or_default().contains("modal prompt"), "{r}");
    assert_eq!(h.app.session.catalog.photo(photo).expect("the photo").rating, before, "no rating behind the prompt");
    assert!(h.app.quit_prompt.is_some(), "the shortcut did not dismiss the prompt");
    assert!(h.app.pending_gestures.is_empty() && h.app.synthetic.is_empty(), "the blocked key left no queued input");
}

/// A settled prompt drops text and scroll input, and the control channel must report that refusal
/// instead of a false success: neither event has a prompt meaning. The prompt stays up.
#[test]
fn a_settled_prompt_rejects_blocked_text_and_scroll_gestures() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    let t = Duration::from_secs(20);
    h.step();
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.step();
    for (method, params) in [("ui.text", serde_json::json!({"text": "x"})), ("ui.scroll", serde_json::json!({"dx": 0.0, "dy": 30.0}))] {
        let r = h.request(method, params, t);
        assert_eq!(r["ok"], false, "{method}: {r}");
        assert!(r["error"].as_str().unwrap_or_default().contains("modal prompt"), "{method}: {r}");
        assert!(h.app.quit_prompt.is_some(), "{method} did not dismiss the prompt");
        assert!(h.app.pending_gestures.is_empty() && h.app.synthetic.is_empty(), "{method} left queued input");
    }
}

/// The settings-notice path uses the same barrier and window: it blocks the dialog below it (both
/// pointer and the outside-click preference), and its own OK dismisses it without closing Settings.
#[test]
fn a_settings_notice_blocks_background_input_and_its_ok_button_works() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    let t = Duration::from_secs(20);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "Settings is open");
    h.app.ui.settings.close_dialogs_outside = true;
    let confirm_delete = h.app.ui.settings.confirm_delete;
    h.app.notices.push("The settings file could not be read and was reset.".into());
    h.step();
    assert!(h.app.widgets.iter().any(|(id, _)| id == "button:noticeOk"), "the notice is laid out");
    assert!(h.app.widgets.iter().any(|(id, _)| id == "check:settings.confirmDelete"), "the covered checkbox is laid out");
    // A covered Settings control cannot be clicked, and a backdrop click cannot close Settings.
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "check:settings.confirmDelete"}), t);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(h.app.ui.settings.confirm_delete, confirm_delete, "the covered checkbox did not change");
    let at = egui::pos2(10.0, 890.0);
    let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(at), press(true), press(false)]);
    h.step();
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "the backdrop click did not close Settings");
    assert!(!h.app.notices.is_empty(), "the backdrop click did not dismiss the notice");
    // OK dismisses only the notice; Settings survives and becomes usable again.
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "button:noticeOk"}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.notices.is_empty(), "OK dismissed the notice");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })), "dismissing the notice kept Settings");
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "check:settings.confirmDelete"}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.settings.confirm_delete, !confirm_delete, "the checkbox works again after the notice");
}

#[test]
fn clipping_changed_between_the_move_and_press_is_seen_in_the_latest_registry() {
    let mut h = Headless::new(app(), [1400.0, 900.0], 1.0);
    let photo = h.app.session.visible_cloned()[0];
    let t = Duration::from_secs(20);
    assert_eq!(h.request("engine.execute", serde_json::json!({"command": "library.select", "params": {"ids": [photo.0]}}), t)["ok"], true);
    assert_eq!(h.request("ui.set", serde_json::json!({"view": "detail", "right": "edit"}), t)["ok"], true);
    assert!(h.settle(Duration::from_secs(60)), "the detail view settles");
    let id = "star:bottom:2";
    let (tx, rx) = reply_channel();
    h.app.admit_gesture(&h.view.ctx.clone(), "ui.clickWidget".into(), serde_json::json!({"id": id}), tx).expect("the click is admitted");
    h.step(); // resolve: move, press and release are queued
    h.step(); // the move is consumed
    // The latest completed pass clipped the widget without moving it to another layer. Only the
    // clipped rect in the registry knows that; `read_response` before `begin_pass` can still see
    // the pass before the registry that was just published.
    let hit = &mut h.app.widget_hits.iter_mut().rev().find(|(widget, _)| widget == id).expect("the target has a probe").1;
    hit.interact_rect = egui::Rect::NOTHING;
    h.step(); // the press is refused instead of being delivered to the clipped target
    let reply = rx.try_recv().expect("the clipped click answers");
    assert_eq!(reply["ok"], false, "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("no longer visible"), "{reply}");
    assert!(error.contains("before its press"), "{reply}");
    assert!(h.app.pending_gestures.is_empty());
    assert!(h.app.synthetic.is_empty());
}

#[test]
fn a_timeout_cancels_only_its_own_gesture_and_preserves_a_later_queue() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    app.widgets = vec![
        ("first:test".into(), egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(30.0, 20.0))),
        ("second:test".into(), egui::Rect::from_min_size(egui::pos2(60.0, 10.0), egui::vec2(30.0, 20.0))),
    ];
    let (tx1, rx1) = reply_channel();
    let (tx2, rx2) = reply_channel();
    app.admit_gesture(&ctx, "ui.clickWidget".into(), serde_json::json!({"id": "first:test"}), tx1).expect("the first click is admitted");
    app.admit_gesture(&ctx, "ui.clickWidget".into(), serde_json::json!({"id": "second:test"}), tx2).expect("the second click is admitted");
    app.layout_epoch = app.layout_epoch.wrapping_add(1);
    app.resolve_pending_gestures(&ctx);
    assert_eq!(app.synthetic.len(), 6, "move, press and release for each click");
    // The first gesture stalls after its events were queued; the second was resolved behind it and
    // has its own deadline. A timeout must remove only the first gesture's unsent prefix.
    app.pending_gestures[0].deadline_ms = 0.0;
    app.reply_delivered_gestures();
    let reply1 = rx1.try_recv().expect("the stalled first gesture answers");
    assert_eq!(reply1["ok"], false, "{reply1}");
    assert!(reply1["error"].as_str().unwrap_or_default().contains("timed out before the UI consumed its last event"), "{reply1}");
    assert_eq!(app.synthetic.len(), 3, "the later gesture's events are preserved");
    assert!(rx2.try_recv().is_err(), "the later gesture is still waiting");
    assert_eq!(app.pending_gestures.len(), 1);
    assert_eq!(app.pending_gestures[0].target.as_ref().map(|target| target.id.as_str()), Some("second:test"));
}

#[test]
fn queued_pointer_cleanup_requests_a_repaint_without_a_synthetic_event() {
    let mut app = app();
    app.synthetic.clear();
    app.pointer_cleanup = true;
    assert!(app.synthetic.is_empty());
    assert!(app.queued_input_needs_repaint(), "the pending cleanup alone must ask for the frame that injects it");
    // A timeout after the last synthetic batch is drained can queue cleanup inside finish_ui_pass;
    // that pass itself has to ask for the frame that delivers it.
    let ctx = egui::Context::default();
    app.finish_ui_pass(&ctx);
    assert!(ctx.has_requested_repaint(), "finish_ui_pass requested the cleanup frame");
    app.pointer_cleanup = false;
    assert!(!app.queued_input_needs_repaint());
}

#[test]
fn a_moved_widget_cancels_before_its_first_event_and_leaves_a_later_gesture_queued() {
    let mut h = Headless::new(app(), [1000.0, 760.0], 1.0);
    h.step();
    let t = Duration::from_secs(20);
    let r = h.request("ui.key", serde_json::json!({"key": ",", "cmd": true}), t);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(Duration::from_secs(60)), "the Settings dialog settles");
    assert!(h.app.widgets.iter().any(|(id, _)| id == "check:settings.confirmDelete"));
    assert!(h.app.widgets.iter().any(|(id, _)| id == "button:settingsStartup-2"));

    let (tx1, rx1) = reply_channel();
    let (tx2, rx2) = reply_channel();
    let ctx = h.view.ctx.clone();
    h.app
        .admit_gesture(&ctx, "ui.clickWidget".into(), serde_json::json!({"id": "check:settings.confirmDelete"}), tx1)
        .expect("the first click is admitted");
    h.app
        .admit_gesture(&ctx, "ui.clickWidget".into(), serde_json::json!({"id": "button:settingsStartup-2"}), tx2)
        .expect("the second click is admitted");
    // This completed pass resolves both against the same registry and queues move, press, release
    // for each.
    h.step();
    assert!(rx1.try_recv().is_err(), "nothing is answered at resolution");
    assert!(rx2.try_recv().is_err(), "nothing is answered at resolution");
    assert_eq!(h.app.synthetic.len(), 6, "both gestures are queued");

    // Before the first event the first target's latest rect moves (the second target stays put).
    let moved = egui::Rect::from_min_size(egui::pos2(900.0, 700.0), egui::vec2(10.0, 10.0));
    let entry = h.app.widgets.iter_mut().rev().find(|entry| entry.0 == "check:settings.confirmDelete").expect("the first target is registered");
    entry.1 = moved;
    // The first event's injection now cancels only the head; the second gesture's three events must
    // stay at the front of the queue.
    h.step();
    let reply1 = rx1.try_recv().expect("the canceled first gesture answers");
    assert_eq!(reply1["ok"], false, "{reply1}");
    let error = reply1["error"].as_str().unwrap_or_default();
    assert!(error.contains("before its first event"), "{reply1}");
    assert!(error.contains("nothing was retargeted"), "{reply1}");
    assert_eq!(h.app.synthetic.len(), 3, "the later gesture's events are preserved");
    assert!(rx2.try_recv().is_err(), "the later gesture is still waiting");

    for _ in 0..8 {
        h.step();
    }
    let reply2 = rx2.try_recv().expect("the later gesture is delivered");
    assert_eq!(reply2["ok"], true, "{reply2}");
    assert_eq!(h.app.ui.settings.startup_view, crate::state::StartupView::Detail, "the second click ran");
    assert!(h.app.pending_gestures.is_empty());
}

#[test]
fn a_click_whose_target_vanishes_after_the_press_fails_and_releases_the_pointer() {
    let mut h = Headless::new(app(), [1000.0, 760.0], 1.0);
    h.step();
    let t = Duration::from_secs(20);
    let r = h.request("ui.key", serde_json::json!({"key": ",", "cmd": true}), t);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(Duration::from_secs(60)), "the Settings dialog settles");
    let before = h.app.ui.settings.confirm_delete;

    let (tx, rx) = reply_channel();
    h.app
        .admit_gesture(&h.view.ctx.clone(), "ui.clickWidget".into(), serde_json::json!({"id": "check:settings.confirmDelete"}), tx)
        .expect("the click is admitted");
    h.step(); // resolve: move, press, release are queued
    h.step(); // the move is consumed
    h.step(); // the press is consumed: the pointer is now down
    assert!(h.view.ctx.input(|i| i.pointer.any_down()), "the press reached egui");

    // The widget vanishes between press and release (the click's own effect or a layout change).
    h.app.widgets.retain(|(id, _)| id != "check:settings.confirmDelete");
    h.step(); // the release is refused, and a safe release is injected for the down pointer
    let reply = rx.try_recv().expect("the canceled click answers");
    assert_eq!(reply["ok"], false, "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("disappeared before its release"), "{reply}");
    assert!(error.contains("nothing was retargeted"), "{reply}");
    assert!(!h.view.ctx.input(|i| i.pointer.any_down()), "the canceled press was released");
    assert_eq!(h.app.ui.settings.confirm_delete, before, "no click was delivered");
    assert!(h.app.pending_gestures.is_empty());
    assert!(h.app.synthetic.is_empty());
}

#[test]
fn a_discarded_layout_does_not_publish_its_widget_registry() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    let kept = egui::Rect::from_min_size(egui::pos2(1.0, 2.0), egui::vec2(3.0, 4.0));
    let discarded = egui::Rect::from_min_size(egui::pos2(5.0, 6.0), egui::vec2(7.0, 8.0));
    app.widgets = vec![("kept:test".into(), kept)];
    app.next_widgets = vec![("discarded:test".into(), discarded)];
    let mut passes = 0;
    let mut output = ctx.run_pass(raw(egui::vec2(800.0, 600.0)), |ctx| {
        if passes == 0 {
            ctx.request_discard("test: throw this layout away");
            app.finish_ui_pass(ctx);
        }
        passes += 1;
    });
    output.textures_delta.clear();
    assert!(passes > 1, "egui reran the discarded layout");
    assert_eq!(app.widgets, vec![("kept:test".into(), kept)], "the discarded registry was not published");
    assert!(app.next_widgets.is_empty());
}

#[test]
fn a_gesture_replies_only_after_the_completed_pass_consumed_its_event() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    app.ui.view = crate::state::ViewMode::Detail;
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({"x": 12.0, "y": 14.0}), tx).expect("admitted");
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    assert_eq!(app.synthetic.len(), 1, "resolution queued the event");
    assert!(rx.try_recv().is_err(), "not answered before the UI consumed it");
    let mut next = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut next);
    assert!(next.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_))), "the event is injected next frame");
    frame(&ctx, next, |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    let reply = rx.try_recv().expect("the delivery reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(app.pending_gestures.is_empty());
}

#[test]
fn a_delivered_gesture_may_change_the_state_it_acted_on() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.key".into(), serde_json::json!({"key": "j", "cmd": true}), tx).expect("admitted");
    // The first completed pass resolves the key against the Detail layout and queues it.
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    assert!(!app.synthetic.is_empty(), "the key is queued");
    // The pass that consumes it opens the dialog: that is the gesture doing its job, not drift.
    let mut next = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut next);
    frame(&ctx, next, |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    assert_eq!(app.ui.dialog, Some(crate::state::Dialog::ViewOptions));
    let reply = rx.try_recv().expect("the delivered gesture answers");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(app.pending_gestures.is_empty());
}

#[test]
fn a_gesture_cancels_when_the_view_changes_before_its_batch_is_injected() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({"x": 10.0, "y": 10.0}), tx).expect("admitted");
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    assert_eq!(app.synthetic.len(), 1, "resolution queued the event");
    // A hidden logic tick changes what the queued pointer event would mean before a visible
    // pass consumes it: fail closed instead of injecting it into the new view.
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    let mut next = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut next);
    assert!(next.events.is_empty(), "no stale event reaches the new view");
    assert!(app.synthetic.is_empty());
    let reply = rx.try_recv().expect("the cancel error answers once");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("canceled"), "{reply}");
    assert!(app.pending_gestures.is_empty());
}

fn queued_shot(sent_at_ms: Option<f64>) -> PendingShot {
    let (tx, _rx) = reply_channel();
    PendingShot {
        token: 1,
        target: None,
        reply: tx,
        headless: false,
        admitted_passes: 0,
        not_before_ms: 0.0,
        force_ms: 0.0,
        deadline_ms: 10_000.0,
        sent_at_ms,
        reserved: 1,
        slot: None,
        issued: None,
        issued_library: 0,
    }
}

#[test]
fn hidden_paint_requests_only_queued_shots_not_issued_ones() {
    let mut app = app();
    app.pending_screenshots.push(queued_shot(Some(1.0)));
    assert!(!app.needs_ui_pass_while_hidden(), "an issued screenshot forces its own paint");
    app.pending_screenshots.clear();
    app.pending_screenshots.push(queued_shot(None));
    assert!(app.needs_ui_pass_while_hidden(), "a queued/settling screenshot needs a UI pass");
    app.pending_screenshots.clear();
    app.synthetic.push(egui::Event::PointerMoved(egui::Pos2::ZERO));
    assert!(app.needs_ui_pass_while_hidden(), "a gesture step needs a UI pass");
}

#[test]
fn disclosure_rows_publish_expanded_not_toggled() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut seen = Vec::new();
    frame(&ctx, raw(egui::vec2(400.0, 300.0)), |ui| {
        let (resp, _) = crate::widgets::section_header(ui, "test-section", "Light", true, None, false);
        seen.push(ui.ctx().accesskit_node_builder(resp.id, |node| node.is_expanded()).flatten());
    });
    frame(&ctx, raw(egui::vec2(400.0, 300.0)), |ui| {
        let (resp, _) = crate::widgets::section_header(ui, "test-section", "Light", false, None, false);
        seen.push(ui.ctx().accesskit_node_builder(resp.id, |node| node.is_expanded()).flatten());
    });
    assert_eq!(seen, vec![Some(true), Some(false)], "the disclosure reports Expanded, not Toggled");
}

/// `ui.hoverWidget` obeys the same clipped/covered-target contract as a press: a hover whose
/// widget was clipped before delivery is refused, as is one under the non-modal library window.
#[test]
fn hover_widget_refuses_clipped_and_covered_targets() {
    let mut h = Headless::new(app(), [1200.0, 900.0], 1.0);
    let t = Duration::from_secs(20);
    h.step();
    assert!(h.app.run("app.settings", serde_json::json!({})).is_ok(), "Settings opens");
    assert!(h.settle(Duration::from_secs(60)), "Settings settles");
    let id = "check:settings.confirmDelete";
    assert!(h.app.widget_hits.iter().any(|(widget, _)| widget == id), "{id} has a probe");

    let (tx, rx) = reply_channel();
    let ctx = h.view.ctx.clone();
    h.app.admit_gesture(&ctx, "ui.hoverWidget".into(), serde_json::json!({"id": id}), tx).expect("the hover is admitted");
    h.step(); // resolve: the pointer move is queued, not consumed
    assert!(rx.try_recv().is_err(), "nothing is answered at resolution");
    // The latest completed registry says the widget has no clipped interaction area. The hover
    // must fail before its first pointer move, like a press at the same stale point.
    if let Some(hit) = h.app.widget_hits.iter_mut().rev().find(|(widget, _)| widget == id) {
        hit.1.interact_rect = egui::Rect::NOTHING;
    }
    h.step();
    let reply = rx.try_recv().expect("the clipped hover answers");
    assert_eq!(reply["ok"], false, "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("no longer visible"), "{reply}");
    assert!(error.contains("before its first event"), "{reply}");
    assert!(h.app.pending_gestures.is_empty() && h.app.synthetic.is_empty());

    // Positive control: the next pass publishes the real clipped rect again.
    let r = h.request("ui.hoverWidget", serde_json::json!({"id": id}), t);
    assert_eq!(r["ok"], true, "an unobstructed hover still works: {r}");

    // The library window is a non-modal overlay (its dim is the modal layer). A target below it
    // must be refused before a pointer move is queued, then work again after the window closes.
    h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::new("/no/such/library", "test error"));
    assert!(h.settle(Duration::from_secs(60)), "the library window settles");
    assert!(h.app.widgets.iter().any(|(widget, _)| widget == "button:libraryRetry"), "the library window is laid out");
    let r = h.request("ui.hoverWidget", serde_json::json!({"id": id}), t);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap_or_default().contains(id), "{r}");
    h.app.library_problem = None;
    assert!(h.settle(Duration::from_secs(60)), "Settings settles after the overlay");
    let r = h.request("ui.hoverWidget", serde_json::json!({"id": id}), t);
    assert_eq!(r["ok"], true, "the same hover works once the overlay is gone: {r}");
}

/// A system paste request is a viewport command, not a synthetic event, so the prompt/library
/// barrier cannot filter it later. It must be refused while either newly-created barrier owns the
/// pass, and still work normally without one.
#[test]
fn a_system_paste_under_a_new_barrier_is_refused() {
    let mut h = Headless::new(app(), [1200.0, 760.0], 1.0);
    let t = Duration::from_secs(20);
    h.settle(Duration::from_secs(60));
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "field:search"}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.view.clipboard = "blocked".into();
    let ctx = h.view.ctx.clone();

    for (barrier, message) in [
        (Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into())), "modal prompt"),
        (None, "blocking library window"),
    ] {
        if barrier.is_some() {
            h.app.quit_prompt = barrier;
        } else {
            h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::new("/no/such/library", "test error"));
        }
        let (tx, rx) = reply_channel();
        h.app.admit_gesture(&ctx, "ui.clipboard".into(), serde_json::json!({"action": "paste"}), tx).expect("the paste is admitted");
        h.app.layout_epoch = h.app.layout_epoch.wrapping_add(1);
        h.app.resolve_pending_gestures(&ctx);
        let reply = rx.try_recv().expect("the blocked paste answers");
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(reply["error"].as_str().unwrap_or_default().contains(message), "{reply}");
        assert!(h.app.pending_gestures.is_empty() && h.app.synthetic.is_empty());
        h.app.quit_prompt = None;
        h.app.library_problem = None;
    }

    // Positive control: with no barrier, the host clipboard answers and the focused field changes.
    h.step();
    h.view.clipboard = "allowed".into();
    let r = h.request("ui.clipboard", serde_json::json!({"action": "paste"}), t);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.search, "allowed", "an ordinary system paste still reaches the focused field");
}

/// A canceled synthetic press must not overwrite a real host modifier change that arrived in the
/// same frame, later cleanup must not clear that still-held host modifier, and a host release must
/// not leave stale synthetic ownership that suppresses the next synthetic Shift gesture.
#[test]
fn synthetic_modifier_ownership_follows_host_changes_across_frames() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.fonts_ready = true;
    // The synthetic gesture held Alt; its cleanup is in flight when the real host presses Shift.
    app.synthetic_mods = egui::Modifiers::ALT;
    app.synthetic_mods_release = true;
    app.pointer_cleanup = true;
    let mut input = raw(egui::vec2(1000.0, 700.0));
    input.events.push(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    app.raw_input_hook(&ctx, &mut input);
    assert_eq!(input.events, vec![egui::Event::ModifiersChanged(egui::Modifiers::SHIFT)], "the host's Shift press survives cleanup");
    assert_eq!(app.host_mods, egui::Modifiers::SHIFT, "the host state is tracked separately");
    assert_eq!(app.synthetic_mods, egui::Modifiers::NONE, "the canceled gesture no longer owns modifiers");
    assert!(!app.synthetic_mods_release);

    // A later synthetic cleanup with no host event must restore the still-held Shift, not NONE.
    app.synthetic_mods = egui::Modifiers::ALT;
    app.synthetic_mods_release = true;
    app.pointer_cleanup = true;
    let mut input = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut input);
    assert_eq!(
        input.events,
        vec![egui::Event::ModifiersChanged(egui::Modifiers::SHIFT)],
        "cleanup restored the physical Shift rather than clearing it"
    );
    assert_eq!(app.host_mods, egui::Modifiers::SHIFT);
    assert_eq!(app.synthetic_mods, egui::Modifiers::NONE);

    // A plain synthetic event carrying NONE likewise must not clear the host's Shift.
    app.synthetic.push(egui::Event::Text("plain".into()));
    let mut input = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut input);
    assert!(
        !input.events.iter().any(|e| matches!(e, egui::Event::ModifiersChanged(_))),
        "a NONE synthetic event did not announce NONE: {:?}",
        input.events
    );
    assert!(input.events.iter().any(|e| matches!(e, egui::Event::Text(text) if text == "plain")));
    app.synthetic_in_flight = false;

    // The host releases Shift. Any stale synthetic ownership would now suppress a later gesture.
    let mut input = raw(egui::vec2(1000.0, 700.0));
    input.events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    app.raw_input_hook(&ctx, &mut input);
    assert_eq!(app.host_mods, egui::Modifiers::NONE);
    assert_eq!(app.synthetic_mods, egui::Modifiers::NONE);

    // The next synthetic gesture that carries Shift must announce it.
    app.synthetic.push(key_event(egui::Key::A, true, egui::Modifiers::SHIFT));
    app.synthetic.push(key_event(egui::Key::A, false, egui::Modifiers::SHIFT));
    let mut input = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut input);
    assert!(
        input.events.iter().any(|e| matches!(e, egui::Event::ModifiersChanged(m) if *m == egui::Modifiers::SHIFT)),
        "the fresh synthetic Shift gesture announces its modifier: {:?}",
        input.events
    );
    assert_eq!(app.synthetic_mods, egui::Modifiers::SHIFT);
    app.synthetic_in_flight = false;

    // Its release returns to the host's state (NONE), not a stale synthetic Shift.
    let mut input = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut input);
    assert_eq!(input.events, vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)]);
    assert_eq!(app.synthetic_mods, egui::Modifiers::NONE);

    // Positive control: with no host change, cleanup still releases synthetic modifiers to the
    // host's last known (NONE) state.
    app.synthetic_mods = egui::Modifiers::ALT;
    app.synthetic_mods_release = true;
    app.pointer_cleanup = true;
    let mut input = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&ctx, &mut input);
    assert_eq!(input.events, vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)]);
    assert_eq!(app.synthetic_mods, egui::Modifiers::NONE);
    assert!(!app.synthetic_mods_release);
}
