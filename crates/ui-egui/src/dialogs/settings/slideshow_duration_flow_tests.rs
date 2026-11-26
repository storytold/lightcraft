//! Local production-control coverage for the Playback panel's durations, not whole-app or
//! native-window acceptance. One in-memory library, two durations: no photo, preview, playback,
//! disk, chooser or app logic tick.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect, vec2};
use lightcraft_catalog::MemStore;
use lightcraft_engine::Session;
use lightcraft_engine::library::LibraryStores;
use lightcraft_engine::slideshow::SLIDE_MIN;

const SLIDE: &str = "slider:slideshowSlide";
const FADE: &str = "slider:slideshowFade";

struct Drain(egui::FullOutput);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    files: MemStore,
    tick: u32,
    rects: Vec<(String, Rect)>,
}

impl Fixture {
    /// A memory-only library holding the 4s slide and 3s fade of the review threads, set through
    /// the real command so `prefs.json` holds them too.
    fn new() -> Self {
        let files = MemStore::new();
        let mut session = Session::new();
        session
            .open_library_in(
                LibraryStores {
                    dir: "memory-slideshow-durations".into(),
                    catalog: Box::new(MemStore::new()),
                    files: Box::new(files.clone()),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
        session.execute("slideshow.playback", &json!({"slideDuration": 4.0, "fadeDuration": 3.0})).unwrap();
        let app = LightcraftApp::new(session, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self { app, ctx, files, tick: 0, rects: vec![] };
        for _ in 0..3 {
            f.frame(vec![]);
        }
        assert_eq!(f.playback(), (4.0, 3.0));
        f
    }

    fn frame(&mut self, events: Vec<Event>) {
        assert!(self.app.session.library.as_ref().is_some_and(|lib| !lib.on_disk));
        assert!(self.app.session.media.rendered.disk().is_none());
        self.tick += 1;
        let _ = crate::widgets::take_registries(&self.ctx);
        let raw = HeadlessView::raw_input(vec2(1000.0, 1200.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        drop(Drain(self.ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let t = Tokens::get(ui.ctx());
                playback_section(app, ui, &t);
            });
        })));
        (self.rects, _) = crate::widgets::take_registries(&self.ctx);
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    /// The slide and fade durations in memory.
    fn playback(&self) -> (f64, f64) {
        let s = &self.app.session.slideshow;
        (s.slide_duration, s.fade_duration)
    }

    fn saved(&self) -> Value {
        serde_json::from_slice(&self.files.get("prefs.json").unwrap()).unwrap()
    }

    /// The preferences hold exactly the settings in memory (the command saved them).
    fn saved_matches(&self) {
        assert_eq!(self.saved()["slideshow"], serde_json::to_value(&self.app.session.slideshow).unwrap());
    }

    /// `f` of the way along `id`'s track, from its left end to its right one.
    fn at(&self, id: &str, f: f32) -> egui::Pos2 {
        let track = self.rect(id);
        egui::pos2(track.left() + track.width() * f, track.center().y)
    }

    /// A point past `id`'s right end, so a drag taken there lands on the slider's own maximum.
    fn past_end(&self, id: &str) -> egui::Pos2 {
        let track = self.rect(id);
        egui::pos2(track.right() + 60.0, track.center().y)
    }

    /// Press off the thumb (a press on the track follows the pointer), drag through each of
    /// `through` and let go at the last of them.
    fn drag(&mut self, press: egui::Pos2, through: &[egui::Pos2]) {
        self.pointer(press, true);
        for at in through {
            let at = *at;
            self.frame(vec![Event::PointerMoved(at)]);
        }
        let end = through.last().copied().unwrap_or(press);
        self.pointer(end, false);
        self.frame(vec![]);
    }

    /// A press and its release at `id`'s centre: one pass, no drag.
    fn click(&mut self, id: &str) {
        let at = self.rect(id).center();
        self.pointer(at, true);
        self.pointer(at, false);
        self.frame(vec![]);
    }

    fn pointer(&mut self, at: egui::Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }

    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.frame(vec![]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(vec2(1000.0, 1200.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drop(Drain(self.ctx.run_ui(raw, |_| {})));
            let _ = crate::widgets::take_registries(&self.ctx);
            self.app.session.close_library().unwrap(); // MemStore, never a filesystem library.
        }
    }
}

/// The review's case: a slide dragged shorter than the fade clamps the fade in the preview, and
/// Escape gives both durations back (4s and 3s), not just the slide.
#[test]
fn slideshow_duration_escape_restores_the_fade_a_shorter_slide_clamped() {
    let mut f = Fixture::new();
    let before = f.playback();
    let saved = f.saved();
    let journal = f.app.session.journal.len();
    let press = f.at(SLIDE, 0.5);
    let short = f.at(SLIDE, 0.0);
    f.pointer(press, true);
    f.frame(vec![Event::PointerMoved(f.at(SLIDE, 0.25))]);
    f.frame(vec![Event::PointerMoved(short)]);
    let held = f.playback();
    assert_eq!(held.0, SLIDE_MIN, "the held drag shows the shortest slide");
    assert_eq!(held.1, SLIDE_MIN, "the preview clamps the fade to the shorter slide");
    assert_eq!(f.app.session.journal.len(), journal, "a preview saves nothing");
    f.key(Key::Escape);
    f.pointer(short, false);
    f.frame(vec![]);
    assert_eq!(f.playback(), before, "Escape gives both durations back");
    assert_eq!(f.saved(), saved, "a cancelled gesture saves nothing");
    assert_eq!(f.app.session.journal.len(), journal);
}

/// Letting go at a slide long enough for the fade keeps the fade: only the duration let go of
/// changes, and the other one is clamped by that final value alone.
#[test]
fn slideshow_duration_release_clamps_the_fade_by_the_value_let_go_of() {
    let mut f = Fixture::new();
    let journal = f.app.session.journal.len();
    // down to the shortest slide and back up past the longest: the fade comes back with it
    f.drag(f.at(SLIDE, 0.5), &[f.at(SLIDE, 0.25), f.at(SLIDE, 0.0), f.at(SLIDE, 0.5), f.past_end(SLIDE)]);
    let (slide, fade) = f.playback();
    assert!(slide > 3.0, "the gesture was let go of at a slide that allows the original fade: {slide}");
    assert_eq!(fade, 3.0, "the fade a shorter preview clamped is still there when the slide allows it");
    assert_eq!(f.app.session.journal.len(), journal + 1, "letting go saves once");
    let saved = f.app.session.journal[journal].clone();
    assert_eq!(saved.0, "slideshow.playback");
    assert_eq!(saved.1, json!({"slideDuration": slide}), "only the duration let go of is saved");
    f.saved_matches();

    // a release at the shortest slide still clamps the fade to it
    let journal = f.app.session.journal.len();
    f.drag(f.at(SLIDE, 0.5), &[f.at(SLIDE, 0.25), f.at(SLIDE, 0.0)]);
    assert_eq!(f.playback(), (SLIDE_MIN, SLIDE_MIN), "a commit to the shortest slide clamps the fade to it");
    assert_eq!(f.app.session.journal.len(), journal + 1);
    assert_eq!(f.app.session.journal[journal].1, json!({"slideDuration": SLIDE_MIN}));
    f.saved_matches();
}

/// The fade's own gesture starts from the fade's saved value: dragging it to the top saves the
/// slide's length, and leaves the slide alone.
#[test]
fn slideshow_fade_duration_is_clamped_by_the_slide_alone() {
    let mut f = Fixture::new();
    f.app.session.execute("slideshow.playback", &json!({"slideDuration": 6.0, "fadeDuration": 2.0})).unwrap();
    f.frame(vec![]);
    let journal = f.app.session.journal.len();
    f.drag(f.at(FADE, 0.5), &[f.at(FADE, 0.75), f.past_end(FADE)]);
    let (slide, fade) = f.playback();
    assert_eq!(slide, 6.0, "dragging the fade leaves the slide alone");
    assert_eq!(fade, 6.0, "the fade stops at the slide");
    assert_eq!(f.app.session.journal.len(), journal + 1, "one save for the fade");
    assert_eq!(f.app.session.journal[journal].1, json!({"fadeDuration": fade}));
    f.saved_matches();
}

/// The review's follow-up: a gesture that ends without a save (Escape, or a press let go of where it
/// began) leaves nothing behind, so a one-pass change afterwards does not write its old settings
/// back over what was changed in between.
#[test]
fn slideshow_duration_a_one_pass_change_keeps_what_changed_since_a_cancelled_drag() {
    let mut f = Fixture::new();
    // a slide drag cancelled with Escape: undone, and nothing of it stays for the next change
    let press = f.at(SLIDE, 0.5);
    let short = f.at(SLIDE, 0.0);
    f.pointer(press, true);
    f.frame(vec![Event::PointerMoved(f.at(SLIDE, 0.25))]);
    f.frame(vec![Event::PointerMoved(short)]);
    f.key(Key::Escape);
    f.pointer(short, false);
    f.frame(vec![]);
    assert_eq!(f.playback(), (4.0, 3.0), "the cancelled drag is undone");
    // another playback setting, saved on its own
    f.click("check:settings.slideshowManual");
    assert!(f.app.session.slideshow.manual, "the checkbox saves through slideshow.playback");
    // a one-pass change: a press on the track sets the slide and saves it, with no gesture around it
    let journal = f.app.session.journal.len();
    let at = f.at(SLIDE, 0.5);
    f.pointer(at, true);
    f.pointer(at, false);
    f.frame(vec![]);
    let (slide, fade) = f.playback();
    assert!(slide > 3.0, "the press set a longer slide: {slide}");
    assert_eq!(fade, 3.0, "the fade that nothing edited is kept");
    assert!(f.app.session.slideshow.manual, "Manual, changed since the cancelled drag, is kept rather than written back over");
    assert_eq!(f.app.session.journal.len(), journal + 1, "the press saves once");
    assert_eq!(f.app.session.journal[journal].1, json!({"slideDuration": slide}));
    f.saved_matches();
    assert_eq!(f.saved()["slideshow"]["manual"], json!(true), "and the preferences keep Manual");
}
