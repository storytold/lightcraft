//! The slideshow in the app (View ▸ Slideshow): playback with the library's settings, slides
//! prepared ahead off the UI thread, transitions only between ready slides, keys and controls.
//! Simulated time: every headless frame is 1/60 s, so nothing here sleeps.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::slideshow::Item;
use serde_json::json;

use crate::headless::{Headless, RenderGate};
use crate::render::Slot;
use crate::tests_fixtures::SyntheticLibrary;
use crate::tests_render_fixtures::{HEIGHT, RenderFixture, WIDTH};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn grid() -> Headless {
    grid_sized([1000.0, 700.0])
}

fn grid_sized(size: [f32; 2]) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    h.step();
    h
}

/// The slideshow's 1000×700 window over `count` real 6000×4000 file-backed photos. Used only by
/// `next_slides_are_prepared_before_they_are_due`, where the 24 MP decode/render is the point:
/// destructure the tuple immediately with the fixture binding first, so the renderer drops first.
fn fixture_grid(count: usize, open: Rc<Cell<bool>>) -> (RenderFixture, Headless) {
    let fixture = RenderFixture::new("slideshow", count);
    assert_eq!(fixture.paths().len(), count);
    for (index, path) in fixture.paths().iter().enumerate() {
        assert!(std::path::Path::new(path).is_file(), "the fixture photo exists");
        assert!(!fixture.paths()[..index].contains(path), "each fixture photo has its own path");
    }
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(fixture.session(), services);
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    // Control native grid work before its first frame too: asynchronous setup must not
    // consume the wall-clock budget before the slideshow scheduling assertions begin.
    assert!(open.get(), "the native grid setup gate starts open");
    h.app.renderer.set_offload(Box::new(RenderGate::new(open)));
    assert!(h.settle(SETTLE), "the grid renders settle before controlling slide renders");
    assert_eq!(h.app.session.catalog.len(), count, "the slideshow fixture photos");
    let id = h.app.session.active().expect("a photo is open");
    let photo = h.app.session.catalog.photo(id).expect("it is in the catalog");
    assert_eq!((photo.width, photo.height), (WIDTH, HEIGHT), "the slideshow fixture is a real 24 MP photo");
    (fixture, h)
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn play(h: &Headless) -> &lightcraft_engine::slideshow::Playback {
    &h.app.ui.slideshow.as_ref().expect("a slideshow is playing").play
}

fn has_slide(h: &Headless, item: impl Into<Item>) -> bool {
    let item = item.into();
    let tex = h.app.renderer.textures.get(&Slot::Slide(item));
    match item {
        Item::Photo(id) => tex.is_some_and(|t| t.photo == id),
        Item::Title(_) => tex.is_some(),
    }
}

/// The photo up now (none while a title screen is).
fn photo_up(h: &Headless) -> PhotoId {
    play(h).current_photo().expect("a photo is up")
}

fn rect_json(r: egui::Rect) -> serde_json::Value {
    json!([r.left(), r.top(), r.width(), r.height()])
}

fn slides_held(h: &Headless) -> usize {
    h.app.renderer.textures.keys().filter(|s| matches!(s, Slot::Slide(_))).count()
}

#[test]
fn slideshow_advances_pauses_and_ends() {
    // This tests the automatic playback clock and controls; native 24 MP rendering
    // is exercised separately by next_slides_are_prepared_before_they_are_due.
    let library = SyntheticLibrary::with_photos("slideshow-automatic-controls", 3);
    let mut h = Headless::new(library.app(), [1000.0, 700.0], 1.0);
    assert!(h.settle(SETTLE), "the automatic playback fixture settles");
    let first = h.app.session.visible_cloned()[0];
    // The fixture selects its first imported photo; newest-first view order differs.
    // Establish the same first-in-view starting selection as the original demo test.
    run(&mut h, "library.select", json!({"ids": [first.0]}));
    assert_eq!(h.app.session.active(), Some(first), "the first visible photo is selected before playback");
    run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0, "manual": false}));
    assert!(!play(&h).settings().manual, "playback advances automatically");
    assert!(h.app.ui.fullscreen && h.app.ui.slideshow.is_some());
    assert_eq!(h.app.session.active(), Some(first), "starts at the first photo in view");
    assert!(h.step_until(SETTLE, |h| h.app.session.active() != Some(first)), "the next photo comes up and is selected");
    let advanced = photo_up(&h);
    assert_eq!(h.app.session.active(), Some(advanced), "selection follows automatic playback");
    assert!(has_slide(&h, advanced), "automatic playback shows a rendered next photo");
    // Space pauses: nothing moves
    let r = h.request("ui.key", json!({"key": "space"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(play(&h).is_paused(), "paused");
    let held = h.app.session.active();
    for _ in 0..120 {
        h.step();
    }
    assert_eq!(h.app.session.active(), held);
    // Esc ends it, and the slides are let go
    let r = h.request("ui.key", json!({"key": "escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!h.app.ui.fullscreen && h.app.ui.slideshow.is_none());
    assert_eq!(slides_held(&h), 0, "slide textures freed");
    h.settle(SETTLE);
}

#[test]
fn turning_off_the_full_screen_preview_frees_the_slides() {
    let mut h = grid();
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 0, "preparePreviews": false}));
    assert!(h.step_until(SETTLE, |h| slides_held(h) > 0), "a slide is rendered");
    let r = run(&mut h, "view.fullScreenPreview", json!({}));
    assert_eq!(r["fullscreen"], false);
    assert!(!h.app.ui.fullscreen && h.app.ui.slideshow.is_none(), "the toggle ends the slideshow");
    assert_eq!(slides_held(&h), 0, "slide textures freed");
}

#[test]
fn next_slides_are_prepared_before_they_are_due() {
    let open = Rc::new(Cell::new(true));
    let (fx, mut h) = fixture_grid(8, open.clone());
    // Grid setup is complete. Hold photo slide jobs until their ahead requests are
    // observable; opening the gate runs the actual native decode/render synchronously.
    open.set(false);
    let reply = run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 1, "preparePreviews": true, "seed": 7}));
    assert_eq!(reply["settings"]["preparePreviews"], true);
    assert_eq!(reply["settings"]["slideDuration"], 60.0);
    let ahead = play(&h).upcoming(crate::panels::slideshow::AHEAD);
    assert_eq!(ahead.len(), crate::panels::slideshow::AHEAD);
    let start = h.time();
    // The current photo and next two are queued before any slide render completes.
    let current = photo_up(&h);
    for item in std::iter::once(Item::Photo(current)).chain(ahead.iter().copied()) {
        assert!(h.app.renderer.is_pending(Slot::Slide(item)), "requested before it is due: {item:?}");
        assert!(!has_slide(&h, item), "not rendered before the gate opens: {item:?}");
    }
    assert!(play(&h).remaining(h.time()) > 59.0, "the first slide is not due before rendering");
    // The gate executes the actual decode/render work synchronously when polled.
    open.set(true);
    assert!(
        h.step_until(SETTLE, |h| has_slide(h, current) && ahead.iter().all(|id| has_slide(h, *id))),
        "the slide up and the next two slides are rendered"
    );
    assert!(h.time() - start < 60.0, "before the first is due");
    assert_eq!(photo_up(&h), current, "still on the first slide");
    assert!(play(&h).remaining(h.time()) > 59.0, "renders are accepted before the first slide is due");
    assert!(h.settle(SETTLE), "enabled-prefetch renders settle");
    drop(h);
    drop(fx);

    // off: only the slide up is rendered until the next one is due
    let open = Rc::new(Cell::new(true));
    let (_fx2, mut h) = fixture_grid(8, open.clone());
    open.set(false);
    let reply = run(&mut h, "view.slideshow", json!({"interval": 60, "preparePreviews": false}));
    assert_eq!(reply["settings"]["preparePreviews"], false);
    let next = play(&h).upcoming(1)[0];
    let current = photo_up(&h);
    for _ in 0..30 {
        h.step();
    }
    assert!(h.app.renderer.is_pending(Slot::Slide(Item::Photo(current))), "the current slide is requested");
    assert!(!has_slide(&h, next) && !h.app.renderer.is_pending(Slot::Slide(next)), "not prepared");
    open.set(true);
    assert!(h.step_until(SETTLE, |h| has_slide(h, current)), "the current slide renders once allowed");
    assert!(!has_slide(&h, next) && !h.app.renderer.is_pending(Slot::Slide(next)), "still not prepared");
    assert!(h.settle(SETTLE), "disabled-prefetch renders settle");
}

#[test]
fn a_fade_only_runs_between_rendered_slides() {
    let mut h = grid();
    // without previews prepared, each slide renders when it is due: the transition must wait
    run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0.4, "preparePreviews": false}));
    let mut fades = 0;
    let t0 = std::time::Instant::now();
    while fades < 20 && t0.elapsed() < SETTLE {
        h.step();
        let f = play(&h).frame(h.time());
        if let Some(from) = f.from {
            fades += 1;
            assert!(has_slide(&h, f.current), "fading in a slide that isn't rendered");
            assert!(has_slide(&h, from), "fading out a slide that was let go");
        }
    }
    assert!(fades >= 20, "fades ran ({fades})");
    h.settle(SETTLE);
}

#[test]
fn arrows_cut_and_repeat_off_ends_after_the_last_slide() {
    let mut h = grid();
    let photos = h.app.session.visible_cloned();
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 2, "repeat": false}));
    let r = h.request("ui.key", json!({"key": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(photo_up(&h), photos[1]);
    assert_eq!(h.app.session.active(), Some(photos[1]), "the selection follows");
    assert!(play(&h).frame(h.time()).from.is_none(), "a cut, not a fade");
    let r = h.request("ui.key", json!({"key": "left"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(photo_up(&h), photos[0]);
    for _ in 1..photos.len() {
        run(&mut h, "slideshow.next", json!({}));
    }
    assert_eq!(play(&h).position(), (photos.len(), photos.len()));
    run(&mut h, "slideshow.next", json!({}));
    assert!(h.app.ui.slideshow.is_none() && !h.app.ui.fullscreen, "no repeat: past the last one it ends");
    h.settle(SETTLE);
}

#[test]
fn a_manual_slideshow_waits_for_the_arrow_keys() {
    let mut h = grid();
    let photos = h.app.session.visible_cloned();
    // the library setting, as the Settings checkbox sets it
    run(&mut h, "slideshow.playback", json!({"manual": true}));
    let r = run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0}));
    assert_eq!(r["settings"]["manual"], true, "{r}");
    // once the first two slides are rendered, and far longer than the slide duration: the first
    // photo stays up
    assert!(h.step_until(SETTLE, |h| has_slide(h, photos[0]) && has_slide(h, photos[1])), "the slides are rendered");
    for _ in 0..120 {
        h.step();
    }
    assert_eq!(photo_up(&h), photos[0], "nothing advances on its own");
    let r = h.request("ui.key", json!({"key": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(photo_up(&h), photos[1]);
    assert_eq!(h.app.session.active(), Some(photos[1]), "the selection follows");
    let s = run(&mut h, "slideshow.togglePause", json!({}));
    assert_eq!((s["manual"].clone(), s["remaining"].clone()), (json!(true), json!(null)), "{s}");
    run(&mut h, "slideshow.end", json!({}));
    assert!(h.app.ui.slideshow.is_none());
    // off again: it advances by itself
    run(&mut h, "slideshow.playback", json!({"manual": false}));
    run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0}));
    let up = photo_up(&h);
    assert!(h.step_until(SETTLE, |h| h.app.ui.slideshow.as_ref().is_some_and(|r| r.play.current_photo() != Some(up))));
    run(&mut h, "slideshow.end", json!({}));
    // the Settings checkbox turns it on
    run(&mut h, "view.slideshowOptions", json!({}));
    h.step();
    let target = reveal(&mut h, "check:settings.slideshowManual");
    let diagnose = |phase: &str, h: &Headless| {
        let id = "check:settings.slideshowManual";
        let rect = latest_widget_rect(h, id);
        let hit = crate::control::widget_hit(&h.app, id);
        let visible = rect.is_some_and(|r| crate::control::widget_target_visible(&h.view.ctx, hit, r.center()));
        eprintln!(
            "manual-checkbox {phase}: manual={} time={} frames={} target={target:?} rect={rect:?} room={:?} hit={hit:?} visible={visible} input={:?} dialog={:?} notices={:?} quit_prompt={} notice_barrier={} library_problem={} library_barrier={} in_flight={} pending={:?} toast={:?}",
            h.app.session.slideshow.manual,
            h.time(),
            h.frames(),
            latest_widget_rect(h, "settings:pageRoom"),
            h.view.ctx.input(|i| (i.is_scrolling(), i.modifiers, i.keys_down.clone(), i.pointer.any_down())),
            h.app.ui.dialog,
            h.app.notices,
            h.app.quit_prompt.is_some(),
            crate::panels::notices::blocks_input(&h.app),
            h.app.library_problem.is_some(),
            crate::panels::library_problem::blocks_file_drops(&h.app),
            h.app.renderer.in_flight(),
            h.app.renderer.pending_slots(),
            h.app.ui.toast,
        );
    };
    diagnose("before-click", &h);
    assert!(!h.app.session.slideshow.manual, "the checkbox starts unchecked before the real click");
    let journal_before = h.app.session.journal.len();
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowManual"}), T);
    diagnose("after-request", &h);
    eprintln!("manual-checkbox reply={r} journal_since_click={:?}", h.app.session.journal.get(journal_before..));
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    diagnose("after-existing-step", &h);
    eprintln!("manual-checkbox journal_after_existing_step={:?}", h.app.session.journal.get(journal_before..));
    assert!(h.app.session.slideshow.manual, "the checkbox goes through slideshow.playback");
    h.settle(SETTLE);
}

#[test]
fn random_order_starts_at_the_selection_and_is_fixed_by_the_seed() {
    let order = |seed: u64| {
        let mut h = grid();
        let photos = h.app.session.visible_cloned();
        run(&mut h, "library.select", json!({"ids": [photos[3].0]}));
        run(&mut h, "view.slideshow", json!({"randomOrder": true, "seed": seed, "interval": 60}));
        let mut seen = vec![photo_up(&h)];
        for _ in 1..photos.len() {
            run(&mut h, "slideshow.next", json!({}));
            seen.push(photo_up(&h));
        }
        h.settle(SETTLE);
        (photos, seen)
    };
    let (photos, a) = order(11);
    assert_eq!(a[0], photos[3]);
    let in_view_order: Vec<PhotoId> = photos.iter().copied().filter(|p| *p != photos[3]).collect();
    assert_ne!(a[1..].to_vec(), in_view_order, "shuffled");
    let mut sorted = a.clone();
    sorted.sort();
    let mut all = photos.clone();
    all.sort();
    assert_eq!(sorted, all, "every photo once");
    assert_eq!(order(11).1, a, "same seed, same order");
}

#[test]
fn playback_controls_show_on_pointer_movement_and_hide() {
    let mut h = grid();
    run(&mut h, "view.slideshow", json!({"interval": 60}));
    h.step();
    let shown = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "icon:slideshowPlayPause");
    assert!(!shown(&h), "hidden until the pointer moves");
    for fx in [0.3, 0.6] {
        let r = h.request("ui.hoverWidget", json!({"id": "slideshow:canvas", "fx": fx, "fy": 0.3}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.step();
    assert!(shown(&h), "the pointer moved: the controls show");
    assert!(h.painted_text().iter().any(|t| t.contains(&format!("1 / {}", h.app.session.visible_cloned().len()))), "slide n of N");
    let r = h.request("ui.clickWidget", json!({"id": "icon:slideshowPlayPause"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(play(&h).is_paused(), "the play / pause button pauses");
    // the pointer rests away from the bar: gone after a few seconds
    let r = h.request("ui.hoverWidget", json!({"id": "slideshow:canvas", "fx": 0.5, "fy": 0.2}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..(60.0 * (crate::panels::slideshow::CONTROLS_SECS + 0.5)) as usize {
        h.step();
    }
    assert!(!shown(&h), "hidden again");
    let r = h.request("ui.hoverWidget", json!({"id": "slideshow:canvas", "fx": 0.55, "fy": 0.25}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "icon:slideshowEnd"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.slideshow.is_none() && !h.app.ui.fullscreen, "the end button ends it");
    h.settle(SETTLE);
}

#[test]
fn playback_settings_drive_the_slideshow_and_bad_ones_are_refused() {
    let mut h = grid();
    let r = run(&mut h, "slideshow.playback", json!({"slideDuration": 9, "fadeDuration": 0.5, "repeat": false}));
    assert_eq!(r["slideDuration"], 9.0);
    let r = h.request("engine.execute", json!({"command": "view.slideshow", "params": {"slideDuration": "slow"}}), T);
    assert_ne!(r["ok"], true, "{r}");
    assert!(h.app.ui.slideshow.is_none());
    let r = h.request("engine.execute", json!({"command": "view.slideshow", "params": {"seed": -1}}), T);
    assert_ne!(r["ok"], true, "{r}");
    let r = run(&mut h, "view.slideshow", json!({}));
    assert_eq!(r["settings"]["slideDuration"], 9.0);
    assert_eq!(r["settings"]["repeat"], false);
    // `interval` (the older parameter) is this run's slide duration; the saved one stays
    run(&mut h, "slideshow.end", json!({}));
    let r = run(&mut h, "view.slideshow", json!({"interval": 2}));
    assert_eq!(r["settings"]["slideDuration"], 2.0);
    assert_eq!(h.app.session.slideshow.slide_duration, 9.0);
    // the options open in Settings
    run(&mut h, "slideshow.end", json!({}));
    run(&mut h, "view.slideshowOptions", json!({}));
    h.step();
    assert!(h.app.widgets.iter().any(|(w, _)| w == "check:settings.slideshowRandom"));
    // The preview render is a layout input too: wait for the selected photo's real slide before
    // scrolling and clicking, so its completion cannot move the checkbox between those steps.
    let preview = h.app.session.active().expect("the demo library has a selected photo");
    assert!(
        h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == preview)),
        "the real preview is rendered"
    );
    // (below the Templates at the top of the tab)
    let validated = reveal(&mut h, "check:settings.slideshowRandom");
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowRandom"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["rect"], rect_json(validated), "the click used the rect reveal validated");
    assert!(h.app.session.slideshow.random_order, "the checkbox goes through slideshow.playback");
    h.settle(SETTLE);
}

/// The slide preview's render lands on a frame nobody can name (a worker thread); the page under it must
/// not move when it does, or a click made in that frame lands beside the control aimed at (the gate went
/// red on it: `playback_settings_…` clicks "Random order" and the checkbox never changed).
#[test]
fn the_settings_page_does_not_move_when_the_slide_preview_finishes_rendering() {
    let mut h = grid();
    let open = std::rc::Rc::new(std::cell::Cell::new(false));
    h.app.renderer.set_offload(Box::new(crate::headless::RenderGate::new(open.clone())));
    run(&mut h, "view.slideshowOptions", json!({}));
    // (the held render keeps the app "busy", so settle() would wait out its timeout: a second of frames instead)
    for _ in 0..60 {
        h.step();
    }
    let rect = |h: &Headless, w: &str| h.app.widgets.iter().find(|(id, _)| id == w).map(|(_, r)| *r).unwrap_or_else(|| panic!("{w} is on screen"));
    let (room, check) = (rect(&h, "settings:pageRoom"), rect(&h, "check:settings.slideshowRandom"));
    assert!(!h.app.renderer.textures.contains_key(&Slot::SlidePreview), "the render is held back");
    open.set(true);
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.contains_key(&Slot::SlidePreview)), "the render lands");
    h.settle(Duration::from_secs(5));
    assert_eq!(rect(&h, "settings:pageRoom"), room, "the page's room is the same before and after the render");
    assert_eq!(rect(&h, "check:settings.slideshowRandom"), check, "a control stays where it was drawn");
}

#[test]
fn a_click_on_a_control_scrolled_out_of_the_settings_page_is_refused() {
    // A compact window leaves the lower controls of the Slideshow settings page outside the
    // scrolled page room, but they are still laid out and registered.
    let mut h = grid_sized([1000.0, 560.0]);
    run(&mut h, "view.slideshowOptions", json!({}));
    assert!(h.settle(SETTLE), "the settings page settles");
    let target = latest_widget_rect(&h, "check:settings.slideshowRandom").expect("the checkbox is registered");
    let hit = crate::control::widget_hit(&h.app, "check:settings.slideshowRandom").expect("the checkbox has an egui hit owner");
    assert!(
        !crate::control::widget_target_visible(&h.view.ctx, Some(hit), target.center()),
        "the random-order checkbox is clipped out of the page: {target:?}"
    );
    // Its centre is clipped out, so egui would not hit it. Clicking by id must fail closed rather
    // than pressing whatever is at those coordinates.
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowRandom"}), T);
    assert_eq!(r["ok"], false, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(error.contains("check:settings.slideshowRandom"), "{r}");
    assert!(error.contains("nothing was retargeted"), "{r}");
    assert!(!h.app.session.slideshow.random_order, "the clipped control was not clicked");
}

#[test]
fn a_photo_deleted_while_it_plays_does_not_stall_the_slideshow() {
    let mut h = grid();
    let photos = h.app.session.visible_cloned();
    run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0, "preparePreviews": false}));
    // the next slide goes away before it is due
    let next = play(&h).upcoming(1)[0];
    let next = next.photo().unwrap();
    run(&mut h, "photo.delete", json!({"ids": [next.0]}));
    run(&mut h, "photo.deletePermanently", json!({"ids": [next.0]}));
    assert!(h.app.session.catalog.photo(next).is_none());
    assert!(h.step_until(SETTLE, |h| h.app.ui.slideshow.as_ref().is_some_and(|r| r.play.position().0 >= 3)), "it plays on");
    assert!(photos.len() > 3);
    h.settle(SETTLE);
}

/// The latest registry entry for `id`, as [`crate::control::widget_rect`] looks it up (the last
/// registered rect wins).
fn latest_widget_rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn in_page_room(room: egui::Rect, widget: egui::Rect) -> bool {
    widget.center().y >= room.top() + 10.0 && widget.center().y <= room.bottom() - 10.0
}

/// egui 0.37 smooths wheel deltas over several frames (and ends the action after a short idle).
/// Do not measure or click until the Settings page has really stopped scrolling.
fn wait_for_scroll_settled(h: &mut Headless) {
    for _ in 0..600 {
        if !h.view.ctx.input(|i| i.is_scrolling()) {
            return;
        }
        h.step();
    }
    panic!("the Settings page kept scrolling");
}

/// The widget's latest rect once the whole rect (not just its centre) has stayed unchanged for
/// three completed passes. A pinned widget is allowed above the page; a page widget must still be
/// inside the page's room.
fn settled_revealed(h: &mut Headless, id: &str, pinned: bool) -> egui::Rect {
    let mut last = None;
    let mut stable = 0;
    for _ in 0..240 {
        h.step();
        let Some(now) = latest_widget_rect(h, id) else {
            last = None;
            stable = 0;
            continue;
        };
        let room = latest_widget_rect(h, "settings:pageRoom").expect("the Slideshow settings are open");
        let in_place = if pinned { now.bottom() <= room.top() } else { in_page_room(room, now) };
        if !in_place || h.view.ctx.input(|i| i.is_scrolling()) {
            last = Some(now);
            stable = 1;
            continue;
        }
        if last == Some(now) {
            stable += 1;
            if stable >= 3 {
                return now;
            }
        } else {
            stable = 1;
        }
        last = Some(now);
    }
    panic!("{id} never settled in view");
}

/// Scroll the Settings ▸ Slideshow page until widget `id` is in view (in the middle of the room
/// for the page): the sections are taller than the dialog. (The slide preview and its choice are pinned
/// above the page and are in view already.) Returns the settled rect that the click will use.
fn reveal(h: &mut Headless, id: &str) -> egui::Rect {
    // (the preview and its Photo / Intro / Ending choice are pinned above the page: always in
    // view, never scrolled to)
    let pinned = id == "slideshow:preview" || id.starts_with("button:settingsSlideshowPreviewShows-");
    for _ in 0..40 {
        h.step();
        wait_for_scroll_settled(h);
        let room = latest_widget_rect(h, "settings:pageRoom").expect("the Slideshow settings are open");
        let in_view = latest_widget_rect(h, id).is_some_and(|w| if pinned { w.bottom() <= room.top() } else { in_page_room(room, w) });
        if in_view {
            return settled_revealed(h, id, pinned);
        }
        let dy = match latest_widget_rect(h, id) {
            Some(w) => (room.center().y - w.center().y).clamp(-500.0, 500.0),
            None => -300.0,
        };
        let r = h.request("ui.hoverWidget", json!({"id": "settings:pageRoom"}), T);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.scroll", json!({"dy": dy}), T);
        assert_eq!(r["ok"], true, "{r}");
        wait_for_scroll_settled(h);
    }
    panic!("{id} never came into view");
}

fn painted(img: &egui::ColorImage, x: usize, y: usize) -> [u8; 3] {
    let c = img.pixels[y * img.size[0] + x];
    [c.r(), c.g(), c.b()]
}

#[test]
fn a_composed_slide_shows_during_playback() {
    let mut h = grid();
    let r = run(&mut h, "slideshow.layout", json!({"margins": 0.1, "backdropColor": "#ff0000"}));
    assert_eq!(r["marginTop"], 0.1);
    // This checks composed pixels and explicit navigation. Rendering can take enough
    // headless frames to pass any automatic interval before the next slide is ready.
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 0, "manual": true}));
    let current = photo_up(&h);
    assert!(h.step_until(SETTLE, |h| has_slide(h, current)), "the slide is composed and ready");
    h.step();
    let tex = h.app.renderer.textures.get(&Slot::Slide(current.into())).unwrap();
    assert_eq!(tex.size, [1000, 700], "a composed slide fills the screen");
    let img = h.paint();
    assert_eq!(img.size, [1000, 700]);
    // 10 % of the 700 px short side = 70 px of red backdrop around the image cell
    for (x, y) in [(5, 5), (995, 5), (500, 30), (500, 690), (30, 350), (985, 350)] {
        assert_eq!(painted(&img, x, y), [255, 0, 0], "margin at ({x}, {y})");
    }
    let mid = painted(&img, 500, 350);
    assert_ne!(mid, [255, 0, 0], "the photo in the cell");
    // the next slide is composed too, and stepping shows it
    let next = play(&h).upcoming(1)[0];
    assert!(h.step_until(SETTLE, |h| has_slide(h, next)));
    assert_eq!(photo_up(&h), current, "rendering ahead keeps the inspected slide up");
    run(&mut h, "slideshow.next", json!({}));
    h.step();
    assert_eq!(play(&h).current(), next, "navigation shows the prepared slide");
    assert_eq!(painted(&h.paint(), 5, 5), [255, 0, 0]);
    h.settle(SETTLE);
}

#[test]
fn the_default_layout_plays_the_photo_as_before() {
    let mut h = grid();
    assert!(h.app.session.slide_layout.is_plain());
    // Inspect one photo's aspect and backdrop without automatic playback advancing
    // the simulated clock past it while the asynchronous renderer catches up.
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 0, "manual": true}));
    let current = photo_up(&h);
    assert!(h.step_until(SETTLE, |h| has_slide(h, current)));
    h.step();
    assert_eq!(photo_up(&h), current, "the inspected photo stays up while rendering");
    // the photo's own render (its aspect, not the screen's), fitted on black, as before
    let tex = h.app.renderer.textures.get(&Slot::Slide(current.into())).unwrap();
    let aspect = h.app.session.slide_photo_aspect(current).unwrap();
    let tex_aspect = tex.size[0] as f64 / tex.size[1] as f64;
    assert!((tex_aspect - aspect).abs() < 0.02, "{tex_aspect} vs {aspect}");
    assert_ne!(tex.size, [1000, 700]);
    let img = h.paint();
    let r = crate::panels::detail::fit_texture_rect(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0)), tex.size);
    if r.left() > 2.0 {
        assert_eq!(painted(&img, 1, 350), [0, 0, 0], "black beside the photo");
    }
    if r.top() > 2.0 {
        assert_eq!(painted(&img, 500, 1), [0, 0, 0], "black above the photo");
    }
    h.settle(SETTLE);
}

#[test]
fn settings_shows_the_slide_layout_and_a_live_preview() {
    let mut h = grid();
    let first = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [first.0]}));
    run(&mut h, "view.slideshowOptions", json!({}));
    h.step();
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    for w in [
        "slider:slideshowMargins",
        "check:settings.slideshowZoomToFill",
        "check:settings.slideshowStroke",
        "check:settings.slideshowShadow",
        "button:slideshowBackdropColor",
        "check:settings.slideshowWash",
        "slideshow:preview",
    ] {
        assert!(has(&h, w), "{w}");
    }
    // the preview is the composed slide, rendered off the UI thread
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == first)));
    // a checkbox goes through slideshow.layout, and its controls appear (scrolled into view)
    reveal(&mut h, "check:settings.slideshowShadow");
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowShadow"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slide_layout.cast_shadow);
    assert!(has(&h, "slider:slideshowShadowAngle"));
    // a changed layout renders the preview again (once it has settled)
    let before = h.app.renderer.textures.get(&Slot::SlidePreview).map(|t| t.key);
    run(&mut h, "slideshow.layout", json!({"margins": 0.2, "backdropColor": "#00ff00"}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).map(|t| t.key) != before));
    h.step();
    // (the preview is pinned above the page: in view already)
    reveal(&mut h, "slideshow:preview");
    // (painted once the new preview is on screen: one frame isn't always enough on a loaded machine)
    let t0 = std::time::Instant::now();
    let c = loop {
        let rect = h.app.widgets.iter().find(|(id, _)| id == "slideshow:preview").map(|(_, r)| *r).unwrap();
        let img = h.paint();
        let c = painted(&img, (rect.left() + 3.0) as usize, (rect.center().y) as usize);
        if (c[1] > 200 && c[0] < 60) || t0.elapsed() > T {
            break c;
        }
        h.step();
    };
    assert!(c[1] > 200 && c[0] < 60, "the green backdrop in the preview margin: {c:?}");
    h.settle(SETTLE);
}

#[test]
fn overlays_show_during_playback() {
    let mut h = grid();
    assert!(h.app.session.slides_plain());
    // the default layout with an identity plate: playback composes the slide to draw it
    run(
        &mut h,
        "slideshow.overlays",
        json!({"identityPlate": {"enabled": true, "text": "LC", "size": 0.12, "color": "#00ff00", "opacity": 1, "anchor": "topLeft", "offsetX": 0.02, "offsetY": 0.02}}),
    );
    assert!(!h.app.session.slides_plain());
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 0}));
    let current = photo_up(&h);
    assert!(h.step_until(SETTLE, |h| has_slide(h, current)), "the slide is composed and ready");
    h.step();
    assert_eq!(h.app.renderer.textures.get(&Slot::Slide(current.into())).unwrap().size, [1000, 700]);
    let img = h.paint();
    // the plate's box: 14 px in from the top-left corner, 84 px high
    let green = (14..98).flat_map(|y| (14..120).map(move |x| (x, y))).filter(|&(x, y)| {
        let c = painted(&img, x, y);
        c[1] > 200 && c[0] < 80 && c[2] < 80
    });
    assert!(green.count() > 200, "the identity plate is on the slide");
    h.settle(SETTLE);
}

#[test]
fn settings_shows_the_overlays_and_the_preview_follows_them() {
    let mut h = grid();
    let first = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [first.0]}));
    run(&mut h, "view.slideshowOptions", json!({}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == first)));
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    // scroll down to the Overlays section
    reveal(&mut h, "button:slideshowAddText");
    for w in ["check:settings.slideshowPlate", "check:settings.slideshowStars", "button:slideshowAddText", "check:settings.slideshowWatermark"] {
        assert!(has(&h, w), "{w}");
    }
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowAddText"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.slide_overlays.texts.len(), 1);
    h.step();
    assert!(has(&h, "field:slideshowText0"), "the text overlay's template field");
    // its rows are below the fold: scroll on down
    reveal(&mut h, "button:slideshowText0Anchor-0");
    assert!(has(&h, "button:slideshowText0Anchor-4"), "its anchor grid");
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowText0Anchor-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.slide_overlays.texts[0].place.anchor, lightcraft_engine::export::Anchor::TopLeft, "picked in the grid");
    // removed from its row the text overlay from its row
    reveal(&mut h, "button:slideshowText0Remove");
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowText0Remove"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slide_overlays.texts.is_empty());
    h.step();
    reveal(&mut h, "check:settings.slideshowPlate");
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowPlate"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slide_overlays.identity_plate.enabled);
    // the preview is rendered again with the overlays
    let before = h.app.renderer.textures.get(&Slot::SlidePreview).map(|t| t.key);
    run(&mut h, "slideshow.overlays", json!({"identityPlate": {"text": "Studio", "color": "#ff00ff"}}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).map(|t| t.key) != before));
    h.settle(SETTLE);
}

/// Shows the playback controls (the pointer moves over the canvas).
fn show_controls(h: &mut Headless) {
    for fx in [0.3, 0.6] {
        let r = h.request("ui.hoverWidget", json!({"id": "slideshow:canvas", "fx": fx, "fy": 0.3}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.step();
}

#[test]
fn the_intro_plays_before_the_first_photo_and_the_ending_closes_the_show() {
    let mut h = grid();
    let photos = h.app.session.visible_cloned();
    let n = photos.len();
    run(
        &mut h,
        "slideshow.titles",
        json!({
            "intro": {"enabled": true, "color": "#ff00ff", "content": "text", "text": "Hello", "overrideColor": true, "textColor": "#00ff00", "scale": 0.15, "duration": 0.5},
            "ending": {"enabled": true, "color": "#00ffff", "content": "none"}
        }),
    );
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 0, "repeat": false}));
    assert_eq!(play(&h).current(), Item::INTRO, "the intro comes first");
    assert_eq!(h.app.session.active(), Some(photos[0]), "the selection is the first photo");
    let st = run(&mut h, "slideshow.togglePause", json!({}));
    assert_eq!(
        (st["title"].as_str(), st["current"].as_u64(), st["slide"].as_u64(), st["of"].as_u64()),
        (Some("intro"), None, Some(0), Some(n as u64)),
        "{st}"
    );
    run(&mut h, "slideshow.togglePause", json!({}));
    // the intro screen is composed off the UI thread at the screen's size, then shown
    assert!(h.step_until(SETTLE, |h| has_slide(h, Item::INTRO)), "the intro is composed");
    assert_eq!(h.app.renderer.textures.get(&Slot::Slide(Item::INTRO)).unwrap().size, [1000, 700]);
    h.step();
    let img = h.paint();
    assert_eq!(painted(&img, 5, 5), [255, 0, 255], "the intro's colour");
    let green = (280..420).flat_map(|y| (300..700).map(move |x| (x, y))).filter(|&(x, y)| {
        let c = painted(&img, x, y);
        c[1] > 200 && c[0] < 80 && c[2] < 80
    });
    assert!(green.count() > 300, "its text, centred");
    show_controls(&mut h);
    let label = h.painted_text();
    assert!(label.iter().any(|t| t == "Intro"), "the controls say Intro, not a photo number: {label:?}");
    assert!(!label.iter().any(|t| t.contains(&format!("0 / {n}"))));
    // after its own 0.5 s, the first photo (plain layout: the photo on black)
    assert!(h.step_until(SETTLE, |h| play(h).current_photo() == Some(photos[0]) && has_slide(h, photos[0])), "the first photo follows");
    h.step();
    let img = h.paint();
    for (x, y) in [(5, 5), (500, 350)] {
        assert_ne!(painted(&img, x, y), [255, 0, 255], "the intro is gone at ({x}, {y})");
    }
    show_controls(&mut h);
    assert!(h.painted_text().iter().any(|t| t == &format!("1 / {n}")), "titles aren't counted: photo 1 of {n}");
    // to the last photo, then the ending
    for _ in 1..n {
        run(&mut h, "slideshow.next", json!({}));
    }
    let st = run(&mut h, "slideshow.next", json!({}));
    assert_eq!((st["title"].as_str(), st["slide"].as_u64()), (Some("ending"), Some(n as u64)), "{st}");
    assert_eq!(h.app.session.active(), Some(photos[n - 1]), "the selection stays on the last photo");
    assert!(h.step_until(SETTLE, |h| has_slide(h, Item::ENDING)));
    h.step();
    assert_eq!(painted(&h.paint(), 500, 350), [0, 255, 255], "the ending's colour alone");
    show_controls(&mut h);
    assert!(h.painted_text().iter().any(|t| t == "End"));
    // ← goes back to the last photo, → past the ending ends the show
    let st = run(&mut h, "slideshow.previous", json!({}));
    assert_eq!(st["current"].as_u64(), Some(photos[n - 1].0));
    run(&mut h, "slideshow.next", json!({}));
    run(&mut h, "slideshow.next", json!({}));
    assert!(h.app.ui.slideshow.is_none() && !h.app.ui.fullscreen, "ended after the ending");
    h.settle(SETTLE);
}

#[test]
fn titles_survive_deleting_the_only_photo_and_need_photos_to_start() {
    let mut h = grid();
    let photos = h.app.session.visible_cloned();
    let rest: Vec<u64> = photos[1..].iter().map(|p| p.0).collect();
    run(&mut h, "photo.delete", json!({"ids": rest}));
    assert_eq!(h.app.session.visible_cloned(), vec![photos[0]]);
    run(&mut h, "slideshow.titles", json!({"intro": {"enabled": true, "duration": 0.5}, "ending": {"enabled": true, "duration": 0.5}}));
    run(&mut h, "slideshow.overlays", json!({"identityPlate": {"image": "/no/such/plate.png"}}));
    run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0.2, "repeat": false}));
    assert_eq!(play(&h).current(), Item::INTRO);
    // the only photo goes away while the intro is up
    assert_eq!(h.app.session.active(), Some(photos[0]));
    run(&mut h, "photo.deletePermanently", json!({"ids": [photos[0].0]}));
    assert!(h.app.session.catalog.photo(photos[0]).is_none());
    let mut seen = Vec::new();
    let t0 = std::time::Instant::now();
    while let Some(r) = h.app.ui.slideshow.as_ref() {
        if !seen.contains(&r.play.current()) {
            seen.push(r.play.current());
        }
        assert!(t0.elapsed() < SETTLE, "the show plays on to its end: {seen:?}");
        h.step();
    }
    assert_eq!(seen, vec![Item::INTRO, Item::Photo(photos[0]), Item::ENDING], "intro, the (gone) photo, ending");
    // no photos in view: nothing to show, titles or not (as before)
    let r = h.request("engine.execute", json!({"command": "view.slideshow", "params": {}}), T);
    assert_ne!(r["ok"], true, "{r}");
    assert!(r.to_string().contains("no photos to show"), "{r}");
    assert!(h.app.ui.slideshow.is_none());
    h.settle(SETTLE);
}

#[test]
fn settings_shows_the_titles_and_the_preview_shows_a_title_screen() {
    let mut h = grid();
    let first = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [first.0]}));
    run(&mut h, "view.slideshowOptions", json!({}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == first)));
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    for w in ["button:settingsSlideshowPreviewShows-0", "button:settingsSlideshowPreviewShows-1", "button:settingsSlideshowPreviewShows-2"] {
        assert!(has(&h, w), "{w}");
    }
    // scroll down to the Titles section, at the end
    reveal(&mut h, "check:settings.slideshowIntro");
    assert!(has(&h, "check:settings.slideshowEnding"));
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowIntro"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slide_titles.intro.enabled, "the checkbox goes through slideshow.titles");
    h.step();
    for w in [
        "button:slideshowIntroColor",
        "button:settingsSlideshowIntroContent-1",
        "slider:slideshowIntroScale",
        "check:settings.slideshowIntroOwnDuration",
    ] {
        assert!(has(&h, w), "{w}");
    }
    // the preview switched to the intro screen
    assert_eq!(crate::panels::slide_titles::preview_shows(&h.view.ctx), Some(lightcraft_engine::slideshow::Title::Intro));
    run(&mut h, "slideshow.titles", json!({"intro": {"color": "#ff8000", "content": "none"}}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == PhotoId(0))));
    reveal(&mut h, "slideshow:preview");
    assert!(h.step_until(SETTLE, |h| {
        let rect = h.app.widgets.iter().find(|(id, _)| id == "slideshow:preview").map(|(_, r)| *r).unwrap();
        let img = h.paint();
        painted(&img, rect.center().x as usize, rect.center().y as usize) == [255, 128, 0]
    }));
    // Text: its field shows
    reveal(&mut h, "button:settingsSlideshowIntroContent-1");
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsSlideshowIntroContent-1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.slide_titles.intro.content, lightcraft_engine::slide_title::TitleContent::Text);
    h.step();
    assert!(has(&h, "field:slideshowIntroText"));
    // back to the photo's slide
    reveal(&mut h, "button:settingsSlideshowPreviewShows-0");
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsSlideshowPreviewShows-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == first)));
    h.settle(SETTLE);
}

fn tpl_hover_previews_without_changes(h: &mut Headless) -> lightcraft_engine::slide_template::SlideLook {
    use lightcraft_engine::slide_template::SlideLook;
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    let rect = |h: &Headless, w: &str| h.app.widgets.iter().find(|(id, _)| id == w).map(|(_, r)| *r).unwrap();
    for w in [
        "template:builtin:plain-black",
        "template:builtin:gallery-wall",
        "template:builtin:edge-to-edge",
        "template:builtin:cinema-wide",
        "template:builtin:shooting-data",
        "field:slideshowTemplateName",
        "button:slideshowTemplateSave",
    ] {
        assert!(has(h, w), "{w}");
    }
    // hover: the preview shows Gallery Wall (its light grey wall in the margin), nothing changes
    let before = SlideLook::of(&h.app.session);
    let r = h.request("ui.hoverWidget", json!({"id": "template:builtin:gallery-wall"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // (the pointer is over the row one frame after it moved there, and the page has settled the next)
    h.step();
    h.step();
    assert_eq!(crate::panels::slide_templates::hovered(&h.view.ctx).as_deref(), Some("builtin:gallery-wall"));
    let grey = |h: &Headless| {
        let r = rect(h, "slideshow:preview");
        let img = h.paint();
        let c = painted(&img, (r.left() + 3.0) as usize, r.center().y as usize);
        c.iter().all(|v| (200..=230).contains(v))
    };
    assert!(h.step_until(SETTLE, grey), "the hovered template on the selected photo");
    assert_eq!(SlideLook::of(&h.app.session), before, "hovering changes nothing");
    assert!(h.app.session.slide_templates_previous.is_none());
    before
}

fn tpl_click_applies_and_saves_own(h: &mut Headless, dir: &std::path::Path) {
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    // a click applies it
    reveal(h, "template:builtin:gallery-wall");
    let r = h.request("ui.clickWidget", json!({"id": "template:builtin:gallery-wall"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slide_overlays.rating_stars.enabled && h.app.session.slide_layout.stroke_border);
    assert!(has(h, "template:previous"), "Previous settings appear");
    // save as a template of our own
    reveal(h, "field:slideshowTemplateName");
    let r = h.request("ui.clickWidget", json!({"id": "field:slideshowTemplateName"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.text", json!({"text": "Holiday / 2026"}), T);
    assert_eq!(r["ok"], true, "{r}");
    reveal(h, "button:slideshowTemplateSave");
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowTemplateSave"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(dir.join("Holiday _ 2026.json").exists(), "the separator never makes a folder");
    h.step();
    assert!(has(h, "template:user:Holiday _ 2026"));
}

fn tpl_update_from_menu(h: &mut Headless) {
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    // update it from its menu with changed settings
    run(h, "slideshow.layout", json!({"backdropColor": "#123456"}));
    reveal(h, "template:user:Holiday _ 2026");
    let r = h.request("ui.clickWidget", json!({"id": "template:user:Holiday _ 2026", "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    for w in ["button:slideshowTemplateUpdate", "button:slideshowTemplateRename", "button:slideshowTemplateExport", "button:slideshowTemplateDelete"]
    {
        assert!(has(h, w), "{w}");
    }
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowTemplateUpdate"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.slide_template("user:Holiday _ 2026").unwrap().look.layout.backdrop_color, [0x12, 0x34, 0x56]);
}

fn tpl_rename_in_place(h: &mut Headless) {
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    // rename it in place
    reveal(h, "template:user:Holiday _ 2026");
    let r = h.request("ui.clickWidget", json!({"id": "template:user:Holiday _ 2026", "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowTemplateRename"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(has(h, "field:slideshowTemplateRename"));
    let r = h.request("ui.key", json!({"key": "a", "cmd": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.text", json!({"text": "Summer"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.key", json!({"key": "enter"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(has(h, "template:user:Summer"), "{:?}", h.app.session.slide_templates().templates.iter().map(|t| t.id.clone()).collect::<Vec<_>>());
}

fn tpl_delete_then_back_to_previous(h: &mut Headless, dir: &std::path::Path, before: &lightcraft_engine::slide_template::SlideLook) {
    use lightcraft_engine::slide_template::SlideLook;
    // delete it (after confirming)
    reveal(h, "template:user:Summer");
    let r = h.request("ui.clickWidget", json!({"id": "template:user:Summer", "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowTemplateDelete"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(dir.join("Summer.json").exists(), "not before it is confirmed");
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowTemplateDeleteConfirm"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!dir.join("Summer.json").exists());
    // back to the settings before Gallery Wall
    reveal(h, "template:previous");
    let r = h.request("ui.clickWidget", json!({"id": "template:previous"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(SlideLook::of(&h.app.session).overlays, before.overlays);
    h.settle(SETTLE);
}

/// Settings ▸ Slideshow ▸ Templates: built-ins listed, hover previews on the selected photo
/// without changing anything, a click applies, and the user's own templates are saved, updated,
/// renamed and deleted from the browser.
#[test]
fn template_browser_previews_on_hover_applies_and_manages_user_templates() {
    let mut h = grid();
    let dir = std::env::temp_dir().join(format!("lc-ui-sstpl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    h.app.session.slide_templates_dir = Some(dir.clone());
    let first = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [first.0]}));
    run(&mut h, "view.slideshowOptions", json!({}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == first)));
    let before = tpl_hover_previews_without_changes(&mut h);
    tpl_click_applies_and_saves_own(&mut h, &dir);
    tpl_update_from_menu(&mut h);
    tpl_rename_in_place(&mut h);
    tpl_delete_then_back_to_previous(&mut h, &dir, &before);
    h.settle(SETTLE);
    let _ = std::fs::remove_dir_all(dir);
}

/// Hovering a template in the Template Browser previews it in the slide preview, so the preview
/// must be in view while the templates (further down the page) are: it is pinned above the page,
/// under the tab bar, and does not scroll away with it.
#[test]
fn the_slide_preview_stays_in_view_while_the_templates_are_browsed() {
    // (900 x 560 is the least the desktop app's window is)
    for size in [[1000.0, 700.0], [900.0, 560.0]] {
        let mut h = grid_sized(size);
        run(&mut h, "view.slideshowOptions", json!({}));
        h.step();
        let find = |h: &Headless, w: &str| h.app.widgets.iter().find(|(i, _)| i == w).map(|(_, r)| *r);
        let tabs = find(&h, "button:settingsTab-slideshow").expect("the tab bar");
        // down to the last section of the page: the templates are far above the preview's old place
        reveal(&mut h, "button:slideshowExportPdf");
        let preview = find(&h, "slideshow:preview").expect("the preview is still drawn");
        let room = find(&h, "settings:pageRoom").unwrap();
        assert!(preview.height() > 40.0, "{size:?}: {preview:?}");
        assert!(preview.top() >= tabs.bottom(), "{size:?}: under the tab bar, not scrolled up behind it: {preview:?} {tabs:?}");
        assert!(preview.bottom() <= room.top() + 1.0, "{size:?}: above the page, not in it: {preview:?} {room:?}");
    }
}
