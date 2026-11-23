//! Real SDK controls; only deterministic local check answers, never update servers/installers.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::take_registries;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct Frames {
    ctx: egui::Context,
    time: f64,
    rects: Vec<(String, Rect)>,
}
impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        Self { ctx, time: 0.0, rects: vec![] }
    }
    fn frame<R>(&mut self, events: Vec<Event>, mut draw: impl FnMut(&egui::Context) -> R) -> R {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(900.0, 700.0), 1.0, self.time, events);
        let mut answer = None;
        let mut output = self.ctx.run_ui(raw, |ui| {
            answer = Some(draw(ui.ctx()));
        });
        output.textures_delta.clear();
        self.rects = take_registries(&self.ctx).0;
        answer.unwrap()
    }
    fn result(&mut self, status: &Status, events: Vec<Event>) -> CheckResultResponse {
        self.frame(events, |ctx| show_result(ctx, status))
    }
    fn settle(&mut self, status: &Status) {
        for _ in 0..4 {
            assert_eq!(self.result(status, vec![]), CheckResultResponse::None);
        }
    }
    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }
    fn click(&mut self, status: &Status, id: &str, button: PointerButton) -> CheckResultResponse {
        let at = self.rect(id).center();
        self.result(status, vec![Event::PointerMoved(at)]);
        self.result(status, vec![pointer(at, button, true)]);
        self.result(status, vec![pointer(at, button, false)])
    }
}
fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}
fn key(key: Key) -> Vec<Event> {
    [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }).to_vec()
}

#[test]
fn updates155_real_retry_then_local_success_and_dismissal() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let private = std::env::temp_dir().join(format!("lc-updates155-{}-{}", std::process::id(), lightcraft_preview::next_tick()));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Default::default());
    app.updates_rt.updater = Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION"))
        .with_pins_path(private.join("no-pins.json"))
        .with_paths(craft_update::Paths::under(&private))
        .with_source(Arc::new(move || if count.fetch_add(1, Ordering::SeqCst) == 0 { Err("local fixture: offline".into()) } else { Ok(None) }));
    let mut frames = Frames::new();
    check(&mut app).unwrap();
    let limit = Instant::now() + Duration::from_secs(5);
    while !matches!(app.updates_rt.updater.status(), Status::Failed(_)) {
        assert!(Instant::now() < limit, "local failure never arrived");
        frames.frame(vec![], |ctx| {
            tick(&mut app, ctx);
            show(&mut app, ctx);
        });
        std::thread::yield_now();
    }
    for _ in 0..4 {
        frames.frame(vec![], |ctx| show(&mut app, ctx));
    }
    assert!(blocks_input(&app));
    // Measurement must neither consume a queued retry nor invoke even the local source.
    app.updates_rt.check_requested = true;
    frames.frame(vec![], |ctx| craft::ui::runtime::measure::run(ctx, || tick(&mut app, ctx)));
    assert!(app.updates_rt.check_requested);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    app.updates_rt.check_requested = false;
    for _ in 0..4 {
        frames.frame(vec![], |ctx| show(&mut app, ctx));
    }
    let at = frames.rect("button:updatesCheckAgain").center();
    frames.frame(vec![Event::PointerMoved(at)], |ctx| show(&mut app, ctx));
    frames.frame(vec![pointer(at, PointerButton::Primary, true)], |ctx| show(&mut app, ctx));
    frames.frame(vec![pointer(at, PointerButton::Primary, false)], |ctx| show(&mut app, ctx));
    assert!(app.updates_rt.check_requested, "retry must queue exactly the regular check path");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "drawing the retry must not call the service");
    let limit = Instant::now() + Duration::from_secs(5);
    while !matches!(app.updates_rt.updater.status(), Status::UpToDate) {
        assert!(Instant::now() < limit, "local success never arrived");
        frames.frame(vec![], |ctx| {
            tick(&mut app, ctx);
            show(&mut app, ctx);
        });
        std::thread::yield_now();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    for _ in 0..4 {
        frames.frame(vec![], |ctx| show(&mut app, ctx));
    }
    frames.frame(key(Key::Escape), |ctx| show(&mut app, ctx));
    assert_eq!(app.updates_rt.updater.status(), &Status::Idle);
    assert!(!blocks_input(&app));
    assert!(!private.exists(), "no downloader, installer, or outcome writer ran");
}

#[test]
fn updates155_status_visibility_keeps_offer_priority_and_notice_modeless() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Default::default());
    // Static state policy also checks every status; result actions below drive the real controls.
    for status in [
        Status::Checking,
        Status::UpToDate,
        Status::Failed("offline".into()),
        Status::Installing,
        Status::Ready,
        Status::Managed,
        Status::Updated("99".into()),
        Status::InstallFailed("disk full".into()),
    ] {
        assert!(result_window_shows(&status, false));
        assert!(!result_window_shows(&status, true));
    }
    assert!(!result_window_shows(&Status::Idle, false));
    app.updates_rt.startup_check = false;
    assert!(!blocks_input(&app));
}

#[test]
fn updates155_result_actions_are_real_primary_release_controls() {
    for (status, id, expected) in [
        (Status::Failed("offline".into()), "button:updatesCheckAgain", CheckResultResponse::CheckAgain),
        (Status::InstallFailed("disk full".into()), "button:updatesCheckAgain", CheckResultResponse::CheckAgain),
        (Status::Ready, "button:updatesRestart", CheckResultResponse::RestartToUpdate),
        (Status::UpToDate, "button:updatesOk", CheckResultResponse::Close),
        (Status::Managed, "button:updatesOk", CheckResultResponse::Close),
        (Status::Installing, "button:updatesOk", CheckResultResponse::Close),
        (Status::Updated("99.0.0".into()), "button:updatesOk", CheckResultResponse::Close),
    ] {
        let mut frames = Frames::new();
        frames.settle(&status);
        assert_eq!(frames.click(&status, id, PointerButton::Secondary), CheckResultResponse::None);
        let at = frames.rect(id).center();
        frames.result(&status, vec![Event::PointerMoved(at)]);
        assert_eq!(frames.result(&status, vec![pointer(at, PointerButton::Primary, true)]), CheckResultResponse::None);
        assert_eq!(frames.result(&status, vec![pointer(at, PointerButton::Primary, false)]), expected);
    }
}

#[test]
fn updates155_checking_can_escape_but_has_no_result_actions() {
    let mut frames = Frames::new();
    frames.settle(&Status::Checking);
    assert!(!frames.rects.iter().any(|(id, _)| id.starts_with("button:updates")));
    assert_eq!(frames.result(&Status::Checking, key(Key::Escape)), CheckResultResponse::Close);
}

#[test]
fn updates155_keyboard_tab_reaches_real_ok_and_enter_dismisses() {
    let mut frames = Frames::new();
    frames.settle(&Status::UpToDate);
    let mut dismissed = false;
    for _ in 0..8 {
        frames.result(&Status::UpToDate, key(Key::Tab));
        if frames.result(&Status::UpToDate, key(Key::Enter)) == CheckResultResponse::Close {
            dismissed = true;
            break;
        }
    }
    assert!(dismissed, "Tab must reach the SDK footer and Enter must activate it");
}

#[test]
fn updates155_long_error_keeps_actions_visible_and_measure_is_inert() {
    let status = Status::InstallFailed("a long local failure\n".repeat(300));
    let mut frames = Frames::new();
    frames.settle(&status);
    assert!(frames.rect("button:updatesCheckAgain").bottom() <= 700.0);
    assert!(frames.rect("button:updatesOk").bottom() <= 700.0);
    frames.frame(key(Key::Escape), |ctx| {
        craft::ui::runtime::measure::run(ctx, || assert_eq!(show_result(ctx, &status), CheckResultResponse::None));
    });
    assert!(frames.rects.is_empty(), "measurement must publish no automation targets");
}

#[test]
fn updates155_outside_primary_release_closes_but_notice_stays_modeless() {
    for (status, expected) in [(Status::Checking, CheckResultResponse::Close), (Status::Updated("99".into()), CheckResultResponse::None)] {
        let mut frames = Frames::new();
        frames.settle(&status);
        let at = egui::pos2(8.0, 8.0);
        assert!(!frames.rect("dialog:updates").contains(at));
        frames.result(&status, vec![Event::PointerMoved(at)]);
        assert_eq!(frames.result(&status, vec![pointer(at, PointerButton::Primary, true)]), CheckResultResponse::None);
        assert_eq!(frames.result(&status, vec![pointer(at, PointerButton::Primary, false)]), expected);
    }
}

#[test]
fn updates155_click_widget_hits_the_real_result_footer() {
    use crate::headless::Headless;
    let private = std::env::temp_dir().join(format!("lc-updates155-hit-{}-{}", std::process::id(), lightcraft_preview::next_tick()));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Default::default());
    app.updates_rt.updater = Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION"))
        .with_pins_path(private.join("no-pins.json"))
        .with_paths(craft_update::Paths::under(&private))
        .with_source(Arc::new(|| Ok(None)));
    let mut h = Headless::new(app, [1300.0, 820.0], 1.0);
    let limit = Duration::from_secs(10);
    check(&mut h.app).unwrap();
    assert!(h.step_until(limit, |h| matches!(h.app.updates_rt.updater.status(), Status::UpToDate)
        && h.app.widget_hits.iter().any(|(id, _)| id == "button:updatesOk")), "missing registered result footer");
    let answer = h.request("ui.clickWidget", serde_json::json!({"id":"button:updatesOk"}), limit);
    assert_eq!(answer["ok"], true, "{answer}");
    assert!(h.step_until(limit, |h| !blocks_input(&h.app) && !h.app.widgets.iter().any(|(id, _)| id == "button:updatesOk")), "registered click did not dismiss the actual result");
}
