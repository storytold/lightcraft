//! Slideshow playback: the settings of Lightroom Classic's Playback panel and the timeline that
//! plays the photos in view (durations, fades, random order, repeat, pause, stepping, manual
//! advance), between an optional intro and ending screen (the Titles panel,
//! [`crate::slide_title`]).
//!
//! No UI here: [`Playback`] is a pure state machine driven with injected times (seconds on any
//! monotonic clock), so the desktop app, a later Slideshow module and tests run the same timeline.
//! The frontend asks [`Playback::frame`] what to draw (the current slide, and while a transition
//! runs the previous one and its progress) and [`Playback::upcoming`] which slides to prepare; it
//! tells [`Playback::tick`] which slides are ready, so a transition never starts on a slide that
//! has not been rendered yet (the current one is held instead).
//!
//! The settings are a library preference (`prefs.json`, see [`crate::library`]), read and changed
//! with the `slideshow.playback` command.

use lightcraft_catalog::PhotoId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Shortest and longest time a slide stays up (seconds).
pub const SLIDE_MIN: f64 = 0.5;
pub const SLIDE_MAX: f64 = 120.0;
/// Longest fade (seconds); a fade is never longer than the slide duration.
pub const FADE_MAX: f64 = 10.0;

/// How playback runs (Lightroom Classic's Playback panel, without music and pan & zoom).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaybackSettings {
    /// Seconds from the start of one slide's transition to the start of the next one's (the
    /// fade in is part of it).
    pub slide_duration: f64,
    /// Seconds a transition takes (0 = cut).
    pub fade_duration: f64,
    /// Fade through this colour (out to it, then in from it) instead of crossfading.
    pub fade_color: Option<[u8; 3]>,
    /// Shuffle the photos (a new order for every pass when repeating).
    pub random_order: bool,
    /// Start over after the last slide; off: playback ends there.
    pub repeat: bool,
    /// Render the next slides before they are due.
    pub prepare_previews: bool,
    /// Manual slideshow: a slide stays up until ← / → (or the controls) change it; the slide
    /// duration is not used for playback then.
    pub manual: bool,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        PlaybackSettings {
            slide_duration: 4.0,
            fade_duration: 1.0,
            fade_color: None,
            random_order: false,
            repeat: true,
            prepare_previews: true,
            manual: false,
        }
    }
}

impl PlaybackSettings {
    /// The settings with every value in range (a non-finite duration takes the default; the fade
    /// is never longer than the slide).
    pub fn sanitized(mut self) -> Self {
        let d = PlaybackSettings::default();
        self.slide_duration = if self.slide_duration.is_finite() { self.slide_duration.clamp(SLIDE_MIN, SLIDE_MAX) } else { d.slide_duration };
        let fade = if self.fade_duration.is_finite() { self.fade_duration } else { d.fade_duration };
        self.fade_duration = fade.clamp(0.0, FADE_MAX).min(self.slide_duration);
        self
    }

    pub fn to_json(&self) -> Value {
        json!({
            "slideDuration": self.slide_duration,
            "fadeDuration": self.fade_duration,
            "fadeColor": self.fade_color.map(hex),
            "randomOrder": self.random_order,
            "repeat": self.repeat,
            "preparePreviews": self.prepare_previews,
            "manual": self.manual,
        })
    }

    /// These settings with the keys `p` names changed (the others kept), validated and clamped.
    /// A wrong type, an unknown key or a non-finite number is an error; nothing changes then.
    pub fn merged(&self, p: &Value) -> std::result::Result<Self, String> {
        let Some(obj) = p.as_object() else { return Err("parameters must be an object".into()) };
        let mut s = self.clone();
        for (k, v) in obj {
            match k.as_str() {
                "slideDuration" => s.slide_duration = finite(k, v)?,
                "fadeDuration" => s.fade_duration = finite(k, v)?,
                "fadeColor" => s.fade_color = color(v)?,
                "randomOrder" => s.random_order = boolean(k, v)?,
                "repeat" => s.repeat = boolean(k, v)?,
                "preparePreviews" => s.prepare_previews = boolean(k, v)?,
                "manual" => s.manual = boolean(k, v)?,
                other => {
                    return Err(format!(
                        "unknown setting `{other}` (slideDuration, fadeDuration, fadeColor, randomOrder, repeat, preparePreviews, manual)"
                    ));
                }
            }
        }
        // a fade longer than a slide asked for together with a shorter slide: the slide wins
        Ok(s.sanitized())
    }
}

fn finite(k: &str, v: &Value) -> std::result::Result<f64, String> {
    match v.as_f64() {
        Some(x) if x.is_finite() => Ok(x),
        _ => Err(format!("`{k}` must be a finite number of seconds, not {v}")),
    }
}

fn boolean(k: &str, v: &Value) -> std::result::Result<bool, String> {
    v.as_bool().ok_or_else(|| format!("`{k}` must be true or false, not {v}"))
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `null` / `""` / `"none"` = crossfade; `"#rrggbb"` or `[r, g, b]` (0–255) = fade through it.
fn color(v: &Value) -> std::result::Result<Option<[u8; 3]>, String> {
    let bad = || format!("`fadeColor` must be \"#rrggbb\", [r, g, b] or null, not {v}");
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() || s.eq_ignore_ascii_case("none") => Ok(None),
        Value::String(s) => {
            let h = s.strip_prefix('#').unwrap_or(s);
            if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(bad());
            }
            let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).ok_or_else(bad);
            Ok(Some([byte(0)?, byte(2)?, byte(4)?]))
        }
        Value::Array(a) if a.len() == 3 => {
            let mut c = [0u8; 3];
            for (o, x) in c.iter_mut().zip(a) {
                *o = x.as_u64().filter(|n| *n <= 255).ok_or_else(bad)? as u8;
            }
            Ok(Some(c))
        }
        _ => Err(bad()),
    }
}

/// A title screen (Lightroom Classic's Titles panel): the intro before the first slide, the
/// ending after the last one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Title {
    Intro,
    Ending,
}

impl Title {
    pub const ALL: [Title; 2] = [Title::Intro, Title::Ending];

    pub fn id(self) -> &'static str {
        match self {
            Title::Intro => "intro",
            Title::Ending => "ending",
        }
    }

    pub fn parse(s: &str) -> Option<Title> {
        Title::ALL.into_iter().find(|t| t.id().eq_ignore_ascii_case(s.trim()))
    }
}

/// One slide of the timeline: a photo, or a title screen (not a photo: it shows no photo, never
/// moves the selection and isn't counted in "n of N").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Item {
    Title(Title),
    Photo(PhotoId),
}

impl From<PhotoId> for Item {
    fn from(p: PhotoId) -> Item {
        Item::Photo(p)
    }
}

impl Item {
    pub const INTRO: Item = Item::Title(Title::Intro);
    pub const ENDING: Item = Item::Title(Title::Ending);

    /// The photo this slide shows (`None` for a title screen).
    pub fn photo(self) -> Option<PhotoId> {
        match self {
            Item::Photo(p) => Some(p),
            Item::Title(_) => None,
        }
    }

    pub fn title(self) -> Option<Title> {
        match self {
            Item::Title(t) => Some(t),
            Item::Photo(_) => None,
        }
    }
}

/// What a tick did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// Nothing changed (the slide is up, paused, or a transition is running).
    Idle,
    /// The next slide is due but not ready: the current one is held.
    Waiting(Item),
    /// A new slide came up (its transition starts now).
    Advanced(Item),
    /// The last slide was shown and repeat is off.
    Ended,
}

/// What to draw at a moment of playback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub current: Item,
    /// The slide being faded out, while a transition runs.
    pub from: Option<Item>,
    /// How far the transition is (0–1; 1 when there is none).
    pub progress: f32,
    pub fade_color: Option<[u8; 3]>,
}

/// Where the timeline is: the intro, the photos, or the ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Intro,
    Photos,
    Ending,
}

/// One run of a slideshow over a fixed list of photos, with an optional intro and ending.
///
/// **Titles:** the intro plays once, before the first photo; the ending once, after the last photo
/// when repeat is off (with repeat on the show only ends when it is stopped, so the ending never
/// comes up; passes follow each other without titles between them). Random order shuffles the
/// photos only: titles keep their places.
#[derive(Clone, Debug)]
pub struct Playback {
    settings: PlaybackSettings,
    photos: Vec<PhotoId>,
    /// This pass's order (indices into `photos`).
    order: Vec<usize>,
    /// Index into `order` of the photo up now (the last one while the ending is up).
    pos: usize,
    phase: Phase,
    pass: u64,
    seed: u64,
    /// How long the intro and the ending stay up (`None`: no such screen).
    intro: Option<f64>,
    ending: Option<f64>,
    /// When the current slide came up (its transition started).
    started: f64,
    /// The slide fading out (until the transition ends).
    from: Option<Item>,
    /// Paused at this time.
    paused_at: Option<f64>,
    ended: bool,
    /// The next slide was due but not ready at the last tick.
    held: Option<Item>,
}

/// SplitMix64: a small, well-mixed generator for the shuffle (deterministic per seed).
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Fisher–Yates shuffle of `v` with `seed`.
fn shuffle(v: &mut [usize], seed: u64) {
    let mut st = seed;
    for i in (1..v.len()).rev() {
        let j = (splitmix(&mut st) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

impl Playback {
    /// Start playing `photos` (the ones in view, in view order) at `start` (the selected photo;
    /// the first one when absent or not among them), at time `now`. `seed` fixes the random order.
    /// `None` when there is nothing to show.
    pub fn new(photos: Vec<PhotoId>, start: Option<PhotoId>, settings: PlaybackSettings, seed: u64, now: f64) -> Option<Self> {
        if photos.is_empty() {
            return None;
        }
        let settings = settings.sanitized();
        let n = photos.len();
        let first = start.and_then(|s| photos.iter().position(|p| *p == s)).unwrap_or(0);
        let order: Vec<usize> = if settings.random_order {
            // the starting photo first, the others shuffled
            let mut rest: Vec<usize> = (0..n).filter(|i| *i != first).collect();
            shuffle(&mut rest, seed);
            std::iter::once(first).chain(rest).collect()
        } else {
            // from the starting photo to the end, then the ones before it: every photo once
            (first..n).chain(0..first).collect()
        };
        let now = if now.is_finite() { now } else { 0.0 };
        Some(Playback {
            settings,
            photos,
            order,
            pos: 0,
            phase: Phase::Photos,
            pass: 0,
            seed,
            intro: None,
            ending: None,
            started: now,
            from: None,
            paused_at: None,
            ended: false,
            held: None,
        })
    }

    /// With an intro and / or an ending screen, each up for its duration in seconds (clamped like
    /// a slide's; a non-finite one takes the slide duration). Call it right after [`Self::new`]:
    /// the show then starts at the intro.
    pub fn with_titles(mut self, intro: Option<f64>, ending: Option<f64>) -> Self {
        let slide = self.settings.slide_duration;
        let dur = |d: f64| if d.is_finite() { d.clamp(SLIDE_MIN, SLIDE_MAX) } else { slide };
        self.intro = intro.map(dur);
        self.ending = ending.map(dur);
        if self.intro.is_some() && self.pass == 0 && self.pos == 0 && self.phase == Phase::Photos {
            self.phase = Phase::Intro;
        }
        self
    }

    pub fn settings(&self) -> &PlaybackSettings {
        &self.settings
    }

    /// The order of pass `pass` (after the first), given the order of the pass before it.
    fn order_after(&self, pass: u64, prev: &[usize]) -> Vec<usize> {
        let mut order = prev.to_vec();
        if self.settings.random_order {
            order.sort_unstable();
            shuffle(&mut order, self.seed ^ pass.wrapping_mul(0xa076_1d64_78bd_642f));
            // never the same photo twice in a row across passes
            if order.len() > 1 && order.first() == prev.last() {
                order.swap(0, 1);
            }
        }
        order
    }

    fn photo_at(&self, order: &[usize], pos: usize) -> Option<PhotoId> {
        order.get(pos).and_then(|i| self.photos.get(*i)).copied()
    }

    fn item_at(&self, phase: Phase, order: &[usize], pos: usize) -> Option<Item> {
        match phase {
            Phase::Intro => Some(Item::INTRO),
            Phase::Ending => Some(Item::ENDING),
            Phase::Photos => self.photo_at(order, pos).map(Item::Photo),
        }
    }

    /// The place after (`phase`, `pos`, `pass`); `order` is that pass's order and becomes the next
    /// pass's when it wraps. `None`: the show ends there.
    fn following(&self, phase: Phase, pos: usize, pass: u64, order: &mut Vec<usize>) -> Option<(Phase, usize, u64)> {
        match phase {
            Phase::Intro => Some((Phase::Photos, 0, pass)),
            Phase::Photos if pos + 1 < order.len() => Some((Phase::Photos, pos + 1, pass)),
            Phase::Photos if self.settings.repeat => {
                *order = self.order_after(pass + 1, order);
                Some((Phase::Photos, 0, pass + 1))
            }
            Phase::Photos if self.ending.is_some() => Some((Phase::Ending, pos, pass)),
            Phase::Photos | Phase::Ending => None,
        }
    }

    /// The slide up now.
    pub fn current(&self) -> Item {
        self.item_at(self.phase, &self.order, self.pos).or_else(|| self.photos.first().copied().map(Item::Photo)).unwrap_or(Item::Photo(PhotoId(0)))
    }

    /// The photo up now (`None` while a title screen is up).
    pub fn current_photo(&self) -> Option<PhotoId> {
        self.current().photo()
    }

    /// (photo number from 1, photos per pass). Titles aren't photos: (0, N) while the intro is up,
    /// (N, N) while the ending is; [`Self::title`] says which.
    pub fn position(&self) -> (usize, usize) {
        let n = self.order.len();
        match self.phase {
            Phase::Intro => (0, n),
            Phase::Photos => (self.pos + 1, n),
            Phase::Ending => (n, n),
        }
    }

    /// The title screen up now, if one is.
    pub fn title(&self) -> Option<Title> {
        self.current().title()
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn is_ended(&self) -> bool {
        self.ended
    }

    /// The slide that was due but not ready at the last tick (the current one is held for it).
    pub fn waiting_for(&self) -> Option<Item> {
        self.held
    }

    /// The clock as playback sees it: stopped while paused.
    fn clock(&self, now: f64) -> f64 {
        let now = if now.is_finite() { now } else { self.started };
        self.paused_at.map_or(now, |p| p.min(now))
    }

    /// How long `item` stays up (seconds).
    pub fn duration(&self, item: Item) -> f64 {
        match item {
            Item::Title(Title::Intro) => self.intro.unwrap_or(self.settings.slide_duration),
            Item::Title(Title::Ending) => self.ending.unwrap_or(self.settings.slide_duration),
            Item::Photo(_) => self.settings.slide_duration,
        }
    }

    /// How long the transition to `item` takes: the fade, never longer than the slide.
    fn fade_to(&self, item: Item) -> f64 {
        self.settings.fade_duration.min(self.duration(item))
    }

    /// When the next slide is due.
    pub fn due(&self) -> f64 {
        self.started + self.duration(self.current())
    }

    /// Seconds until the next slide is due (as of `now`; frozen while paused; infinite in a
    /// manual slideshow, where no slide is ever due).
    pub fn remaining(&self, now: f64) -> f64 {
        if self.settings.manual {
            return f64::INFINITY;
        }
        (self.due() - self.clock(now)).max(0.0)
    }

    /// The next `n` slides in playing order (titles included; into the next pass when repeating;
    /// never the one up now twice).
    pub fn upcoming(&self, n: usize) -> Vec<Item> {
        let mut out = Vec::new();
        let mut order = self.order.clone();
        let (mut phase, mut pos, mut pass) = (self.phase, self.pos, self.pass);
        let current = self.current();
        while out.len() < n {
            let Some(next) = self.following(phase, pos, pass, &mut order) else { break };
            (phase, pos, pass) = next;
            match self.item_at(phase, &order, pos) {
                Some(it) if it != current && !out.contains(&it) => out.push(it),
                _ => break,
            }
        }
        out
    }

    /// Move one slide on (`Advanced`), or end (`Ended`) past the last one without repeat.
    fn advance(&mut self) -> Tick {
        let mut order = std::mem::take(&mut self.order);
        let next = self.following(self.phase, self.pos, self.pass, &mut order);
        self.order = order;
        match next {
            Some((phase, pos, pass)) => {
                (self.phase, self.pos, self.pass) = (phase, pos, pass);
                Tick::Advanced(self.current())
            }
            None => {
                self.ended = true;
                Tick::Ended
            }
        }
    }

    /// Advance the timeline to `now`. `ready(slide)` says whether a slide can be shown: the
    /// transition to the next slide waits until it is.
    pub fn tick(&mut self, now: f64, ready: impl Fn(Item) -> bool) -> Tick {
        if self.ended {
            return Tick::Ended;
        }
        let now = self.clock(now);
        if self.from.is_some() && now >= self.started + self.fade_to(self.current()) {
            self.from = None;
        }
        if self.paused_at.is_some() {
            return Tick::Idle;
        }
        // a slide's time starts once it can be shown (the first one, one stepped to with ←/→)
        if self.from.is_none() && !ready(self.current()) {
            self.started = now;
            return Tick::Idle;
        }
        // a manual slideshow moves only with `step` (← / →)
        if self.settings.manual || now < self.due() {
            return Tick::Idle;
        }
        let Some(next) = self.upcoming(1).first().copied() else {
            if self.settings.repeat {
                // a single photo repeating: nothing to change to
                self.started = now;
                return Tick::Idle;
            }
            self.ended = true;
            return Tick::Ended;
        };
        if !ready(next) {
            self.held = Some(next);
            return Tick::Waiting(next);
        }
        self.held = None;
        let old = self.current();
        let t = self.advance();
        if let Tick::Advanced(new) = t {
            self.started = now;
            self.from = (self.fade_to(new) > 0.0).then_some(old);
        }
        t
    }

    /// What to draw at `now`.
    pub fn frame(&self, now: f64) -> Frame {
        let now = self.clock(now);
        let current = self.current();
        let fade = self.fade_to(current);
        let (from, progress) = match self.from {
            Some(f) if fade > 0.0 && now < self.started + fade => (Some(f), ((now - self.started) / fade).clamp(0.0, 1.0) as f32),
            _ => (None, 1.0),
        };
        Frame { current, from, progress, fade_color: self.settings.fade_color }
    }

    /// Pause or resume at `now`; resuming keeps the time the slide had left. Returns whether it
    /// is paused now.
    pub fn toggle_pause(&mut self, now: f64) -> bool {
        let now = if now.is_finite() { now } else { self.started };
        match self.paused_at.take() {
            Some(p) => {
                // the slide (and a transition in progress) carries on where it stopped
                self.started += (now - p).max(0.0);
            }
            None => self.paused_at = Some(now),
        }
        self.paused_at.is_some()
    }

    /// Step `delta` slides (←/→) at `now`: a cut, and the new slide gets its full time (paused
    /// playback stays paused). Past the last slide (the ending, when there is one) without repeat
    /// playback ends; before the first it stays there (the intro, when there is one: in the first
    /// pass ← from the first photo goes back to it).
    pub fn step(&mut self, delta: i32, now: f64) -> Tick {
        if self.ended {
            return Tick::Ended;
        }
        let now = if now.is_finite() { now } else { self.started };
        // (more steps than slides would only go round again)
        let slides = self.order.len().saturating_add(2).min(u32::MAX as usize) as u32;
        let steps = delta.unsigned_abs().min(slides);
        let mut t = Tick::Idle;
        if delta > 0 {
            for _ in 0..steps {
                t = self.advance();
                if t == Tick::Ended {
                    return t;
                }
            }
        } else {
            for _ in 0..steps {
                match self.phase {
                    // (the ending keeps the last photo's place)
                    Phase::Ending => self.phase = Phase::Photos,
                    Phase::Photos if self.pos > 0 => self.pos -= 1,
                    Phase::Photos if self.intro.is_some() && self.pass == 0 => self.phase = Phase::Intro,
                    Phase::Photos if self.settings.repeat => self.pos = self.order.len().saturating_sub(1),
                    Phase::Photos | Phase::Intro => {}
                }
            }
            t = Tick::Advanced(self.current());
        }
        self.from = None;
        self.held = None;
        self.started = now;
        if self.paused_at.is_some() {
            self.paused_at = Some(now);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u64) -> Vec<PhotoId> {
        (1..=n).map(PhotoId).collect()
    }

    fn settings(slide: f64, fade: f64) -> PlaybackSettings {
        PlaybackSettings { slide_duration: slide, fade_duration: fade, ..Default::default() }
    }

    fn ph(n: u64) -> Item {
        Item::Photo(PhotoId(n))
    }

    fn all_ready(_: Item) -> bool {
        true
    }

    #[test]
    fn timeline_durations_and_fade_progress() {
        let mut p = Playback::new(ids(3), Some(PhotoId(2)), settings(4.0, 1.0), 1, 10.0).unwrap();
        assert_eq!(p.current(), ph(2), "starts at the selected photo");
        assert_eq!(p.position(), (1, 3));
        assert_eq!(p.tick(13.9, all_ready), Tick::Idle);
        assert_eq!(p.frame(13.9).from, None);
        assert_eq!(p.tick(14.0, all_ready), Tick::Advanced(ph(3)));
        let f = p.frame(14.25);
        assert_eq!((f.current, f.from), (ph(3), Some(ph(2))));
        assert!((f.progress - 0.25).abs() < 1e-6, "{f:?}");
        assert!((p.frame(14.5).progress - 0.5).abs() < 1e-6);
        assert_eq!(p.tick(15.0, all_ready), Tick::Idle);
        assert_eq!(p.frame(15.0).from, None, "the fade is over after fadeDuration");
        // the slide period counts from its transition: next at 18
        assert_eq!(p.tick(17.99, all_ready), Tick::Idle);
        assert_eq!(p.tick(18.0, all_ready), Tick::Advanced(ph(1)), "wraps to the photos before the start");
        assert_eq!(p.position(), (3, 3));
    }

    #[test]
    fn fade_zero_is_a_cut_and_fade_color_is_reported() {
        let mut p = Playback::new(ids(2), None, settings(2.0, 0.0), 1, 0.0).unwrap();
        assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(ph(2)));
        assert_eq!(p.frame(2.0).from, None);
        let s = PlaybackSettings { fade_color: Some([255, 0, 0]), ..settings(2.0, 1.0) };
        let mut p = Playback::new(ids(2), None, s, 1, 0.0).unwrap();
        p.tick(2.0, all_ready);
        assert_eq!(p.frame(2.5).fade_color, Some([255, 0, 0]));
    }

    #[test]
    fn arrows_cut_immediately_and_restart_the_slide_time() {
        let mut p = Playback::new(ids(4), None, settings(4.0, 2.0), 1, 0.0).unwrap();
        p.tick(4.0, all_ready);
        assert!(p.frame(4.5).from.is_some(), "fading");
        assert_eq!(p.step(1, 4.5), Tick::Advanced(ph(3)));
        assert_eq!(p.frame(4.5), Frame { current: ph(3), from: None, progress: 1.0, fade_color: None }, "a cut");
        assert_eq!(p.tick(8.4, all_ready), Tick::Idle, "full time from the step");
        assert_eq!(p.step(-1, 6.0), Tick::Advanced(ph(2)));
        assert_eq!(p.step(-1, 6.0), Tick::Advanced(ph(1)));
        // repeat on: before the first is the last
        assert_eq!(p.step(-1, 6.0), Tick::Advanced(ph(4)));
        assert_eq!(p.remaining(6.0), 4.0);
    }

    #[test]
    fn random_order_is_deterministic_per_seed_and_covers_every_photo_once_per_pass() {
        let photos = ids(20);
        let run = |seed: u64| {
            let s = PlaybackSettings { random_order: true, ..settings(1.0, 0.0) };
            let mut p = Playback::new(photos.clone(), Some(PhotoId(5)), s, seed, 0.0).unwrap();
            let mut seen = vec![p.current_photo().unwrap()];
            for k in 1..60 {
                p.tick(k as f64, all_ready);
                seen.push(p.current_photo().unwrap());
            }
            seen
        };
        let a = run(42);
        assert_eq!(a, run(42), "same seed, same order");
        assert_ne!(a, run(43), "another seed, another order");
        assert_eq!(a[0], PhotoId(5), "starts at the selected photo");
        assert_ne!(a[..20].to_vec(), photos, "shuffled");
        for pass in a.chunks(20).take(2) {
            let mut sorted = pass.to_vec();
            sorted.sort();
            assert_eq!(sorted, photos, "every photo once per pass");
        }
        assert_ne!(a[..20], a[20..40], "a new order for the next pass");
        assert!(a.windows(2).all(|w| w[0] != w[1]), "never the same photo twice in a row");
    }

    #[test]
    fn repeat_off_stops_after_the_last_slide() {
        let s = PlaybackSettings { repeat: false, ..settings(1.0, 0.0) };
        let mut p = Playback::new(ids(3), Some(PhotoId(2)), s.clone(), 1, 0.0).unwrap();
        assert_eq!(p.tick(1.0, all_ready), Tick::Advanced(ph(3)));
        assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(ph(1)));
        assert_eq!(p.upcoming(2), vec![], "nothing after the last");
        assert_eq!(p.tick(3.0, all_ready), Tick::Ended);
        assert!(p.is_ended());
        // → past the last one ends it too
        let mut p = Playback::new(ids(2), None, s, 1, 0.0).unwrap();
        assert_eq!(p.step(1, 0.5), Tick::Advanced(ph(2)));
        assert_eq!(p.step(1, 0.6), Tick::Ended);
        // with repeat: it goes on
        let mut p = Playback::new(ids(2), None, settings(1.0, 0.0), 1, 0.0).unwrap();
        for k in 1..10 {
            assert!(matches!(p.tick(k as f64, all_ready), Tick::Advanced(_)));
        }
    }

    #[test]
    fn pause_and_resume_keep_the_remaining_time() {
        let mut p = Playback::new(ids(3), None, settings(4.0, 1.0), 1, 0.0).unwrap();
        assert!(p.toggle_pause(3.0));
        assert_eq!(p.tick(100.0, all_ready), Tick::Idle, "paused: nothing moves");
        assert_eq!(p.remaining(100.0), 1.0);
        assert!(!p.toggle_pause(100.0));
        assert_eq!(p.tick(100.9, all_ready), Tick::Idle);
        assert_eq!(p.tick(101.0, all_ready), Tick::Advanced(ph(2)), "1 s left after resuming");
        // pausing mid-fade freezes the fade
        p.toggle_pause(101.25);
        assert!((p.frame(150.0).progress - 0.25).abs() < 1e-6);
        p.toggle_pause(150.0);
        assert!((p.frame(150.5).progress - 0.75).abs() < 1e-6);
    }

    #[test]
    fn a_transition_waits_for_the_next_slide_to_be_ready() {
        let mut p = Playback::new(ids(3), None, settings(2.0, 1.0), 1, 0.0).unwrap();
        assert_eq!(p.upcoming(2), vec![ph(2), ph(3)], "what to prepare");
        let not_two = |id: Item| id != ph(2);
        assert_eq!(p.tick(2.0, not_two), Tick::Waiting(ph(2)));
        assert_eq!(p.frame(3.0), Frame { current: ph(1), from: None, progress: 1.0, fade_color: None }, "held, not flashed");
        assert_eq!(p.waiting_for(), Some(ph(2)));
        assert_eq!(p.tick(3.5, all_ready), Tick::Advanced(ph(2)));
        assert_eq!(p.frame(3.5).from, Some(ph(1)), "the fade starts once it is ready");
        assert!((p.frame(4.0).progress - 0.5).abs() < 1e-6, "and runs its full length");
    }

    #[test]
    fn a_slide_s_time_starts_once_it_is_ready() {
        let mut p = Playback::new(ids(3), None, settings(2.0, 0.5), 1, 0.0).unwrap();
        let none = |_: Item| false;
        assert_eq!(p.tick(1.0, none), Tick::Idle);
        assert_eq!(p.tick(5.0, none), Tick::Idle, "the first slide isn't rendered yet: its clock waits");
        assert_eq!(p.remaining(5.0), 2.0);
        assert_eq!(p.tick(6.0, all_ready), Tick::Idle);
        assert_eq!(p.tick(7.0, all_ready), Tick::Advanced(ph(2)), "2 s from when it could be shown");
    }

    #[test]
    fn hostile_settings_are_rejected_or_clamped() {
        let d = PlaybackSettings::default();
        for bad in [
            json!({"slideDuration": "fast"}),
            json!({"fadeDuration": null}),
            json!({"repeat": 1}),
            json!({"fadeColor": "#12345"}),
            json!({"fadeColor": "#ééé"}),
            json!({"fadeColor": "#+f+f+f"}),
            json!({"fadeColor": "-12345"}),
            json!({"fadeColor": [300, 0, 0]}),
            json!({"fadeColor": [1, 2]}),
            json!({"speed": 2}),
            json!([1, 2]),
        ] {
            assert!(d.merged(&bad).is_err(), "{bad}");
        }
        let s = d.merged(&json!({"slideDuration": 1e300, "fadeDuration": -5})).unwrap();
        assert_eq!((s.slide_duration, s.fade_duration), (SLIDE_MAX, 0.0));
        let s = d.merged(&json!({"slideDuration": 0.0, "fadeDuration": 9})).unwrap();
        assert_eq!((s.slide_duration, s.fade_duration), (SLIDE_MIN, SLIDE_MIN), "the fade fits in the slide");
        let s = d.merged(&json!({"fadeColor": "#FF8000"})).unwrap();
        assert_eq!(s.fade_color, Some([255, 128, 0]));
        assert_eq!(s.to_json()["fadeColor"], "#ff8000");
        assert_eq!(d.merged(&json!({"fadeColor": [1, 2, 3]})).unwrap().fade_color, Some([1, 2, 3]));
        // NaN / inf (from a hand-edited file or a caller in-process) take the defaults
        let s = PlaybackSettings { slide_duration: f64::NAN, fade_duration: f64::INFINITY, ..d.clone() }.sanitized();
        assert_eq!((s.slide_duration, s.fade_duration), (d.slide_duration, d.fade_duration));
        // and the timeline never panics on hostile times
        let mut p = Playback::new(ids(2), None, s, 0, f64::NAN).unwrap();
        p.tick(f64::NAN, all_ready);
        p.tick(f64::INFINITY, all_ready);
        p.toggle_pause(f64::NEG_INFINITY);
        p.step(i32::MIN, f64::NAN);
        let _ = p.frame(f64::NAN);
        assert!(Playback::new(vec![], None, d, 0, 0.0).is_none());
    }

    #[test]
    fn a_manual_slideshow_moves_only_with_the_arrows() {
        let s = PlaybackSettings { manual: true, repeat: false, ..settings(1.0, 0.5) };
        let mut p = titled(3, s, Some(1.0), Some(1.0));
        assert_eq!(p.tick(1000.0, all_ready), Tick::Idle, "the intro stays up past its duration");
        assert_eq!(p.current(), Item::INTRO);
        assert_eq!(p.remaining(1000.0), f64::INFINITY, "nothing is ever due");
        assert_eq!(p.upcoming(1), vec![ph(1)], "the next slide is still prepared for →");
        assert_eq!(p.step(1, 1000.0), Tick::Advanced(ph(1)));
        assert_eq!(p.tick(5000.0, all_ready), Tick::Idle);
        assert_eq!(p.current(), ph(1), "a photo stays up too");
        assert_eq!(p.step(-1, 5000.0), Tick::Advanced(Item::INTRO), "← goes back");
        assert_eq!(p.step(4, 5000.0), Tick::Advanced(Item::ENDING));
        assert_eq!(p.tick(9000.0, all_ready), Tick::Idle, "the ending stays up until → ends the show");
        assert_eq!(p.step(1, 9000.0), Tick::Ended);
        // the setting round-trips and is checked like the others
        let d = PlaybackSettings::default();
        assert!(!d.manual, "off by default");
        let m = d.merged(&json!({"manual": true})).unwrap();
        assert!(m.manual);
        assert_eq!(m.to_json()["manual"], true);
        assert!(d.merged(&json!({"manual": "yes"})).is_err());
    }

    #[test]
    fn a_single_photo_repeats_without_changing() {
        let mut p = Playback::new(ids(1), None, settings(1.0, 0.5), 1, 0.0).unwrap();
        assert_eq!(p.upcoming(2), vec![]);
        assert_eq!(p.tick(5.0, all_ready), Tick::Idle);
        assert_eq!(p.frame(5.0).from, None);
    }

    fn titled(n: u64, s: PlaybackSettings, intro: Option<f64>, ending: Option<f64>) -> Playback {
        Playback::new(ids(n), None, s, 1, 0.0).unwrap().with_titles(intro, ending)
    }

    #[test]
    fn titles_play_once_around_the_photos_for_their_own_durations() {
        let s = PlaybackSettings { repeat: false, ..settings(4.0, 1.0) };
        let mut p = titled(3, s, Some(2.0), Some(3.0));
        assert_eq!(p.current(), Item::INTRO, "the intro comes first");
        assert_eq!((p.position(), p.title(), p.current_photo()), ((0, 3), Some(Title::Intro), None), "titles aren't photos");
        assert_eq!(p.upcoming(2), vec![ph(1), ph(2)]);
        assert_eq!(p.tick(1.99, all_ready), Tick::Idle);
        assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(ph(1)), "the intro's own 2 s");
        let f = p.frame(2.5);
        assert_eq!((f.current, f.from), (ph(1), Some(Item::INTRO)), "the intro fades out like a slide");
        assert!((f.progress - 0.5).abs() < 1e-6);
        assert_eq!(p.position(), (1, 3));
        assert_eq!(p.tick(6.0, all_ready), Tick::Advanced(ph(2)));
        assert_eq!(p.tick(10.0, all_ready), Tick::Advanced(ph(3)));
        assert_eq!(p.upcoming(2), vec![Item::ENDING], "the ending is prepared like a slide");
        assert_eq!(p.tick(14.0, all_ready), Tick::Advanced(Item::ENDING));
        assert_eq!((p.position(), p.title()), ((3, 3), Some(Title::Ending)));
        assert_eq!(p.frame(14.5).from, Some(ph(3)), "the ending fades in");
        assert_eq!(p.tick(16.99, all_ready), Tick::Idle);
        assert_eq!(p.tick(17.0, all_ready), Tick::Ended, "the ending's own 3 s, then the show ends");
        // without titles nothing changes
        let mut p = titled(2, PlaybackSettings { repeat: false, ..settings(1.0, 0.0) }, None, None);
        assert_eq!(p.current(), ph(1));
        assert_eq!(p.tick(1.0, all_ready), Tick::Advanced(ph(2)));
        assert_eq!(p.tick(2.0, all_ready), Tick::Ended);
    }

    #[test]
    fn a_fade_never_outlasts_a_short_title() {
        let s = PlaybackSettings { repeat: false, ..settings(4.0, 2.0) };
        let mut p = titled(1, s, Some(0.5), Some(0.5));
        assert_eq!(p.tick(0.5, all_ready), Tick::Advanced(ph(1)));
        assert!((p.frame(1.5).progress - 0.5).abs() < 1e-6, "the photo fades in over the full 2 s");
        assert_eq!(p.tick(4.5, all_ready), Tick::Advanced(Item::ENDING));
        assert!((p.frame(4.75).progress - 0.5).abs() < 1e-6, "into the 0.5 s ending: a 0.5 s fade");
        assert_eq!(p.frame(5.0).from, None);
        assert_eq!(p.tick(5.0, all_ready), Tick::Ended);
    }

    #[test]
    fn with_repeat_the_intro_plays_once_and_the_ending_never() {
        let mut p = titled(2, settings(1.0, 0.0), Some(1.0), Some(1.0));
        let mut seen = vec![p.current()];
        for k in 1..12 {
            p.tick(k as f64, all_ready);
            seen.push(p.current());
            assert!(!p.upcoming(3).contains(&Item::ENDING), "no ending while repeating");
        }
        assert_eq!(seen[0], Item::INTRO);
        assert!(seen[1..].iter().all(|i| i.photo().is_some()), "passes follow each other without titles: {seen:?}");
        assert_eq!(seen[1..5], [ph(1), ph(2), ph(1), ph(2)]);
    }

    #[test]
    fn random_order_shuffles_the_photos_and_keeps_the_titles_in_place() {
        for seed in [1, 2, 3] {
            let s = PlaybackSettings { random_order: true, repeat: false, ..settings(1.0, 0.0) };
            let mut p = Playback::new(ids(10), None, s, seed, 0.0).unwrap().with_titles(Some(1.0), Some(1.0));
            let mut seen = vec![p.current()];
            let mut k = 1.0;
            while p.tick(k, all_ready) != Tick::Ended {
                seen.push(p.current());
                k += 1.0;
            }
            assert_eq!(seen.len(), 12, "{seen:?}");
            assert_eq!((seen[0], seen[11]), (Item::INTRO, Item::ENDING), "seed {seed}");
            let mut photos: Vec<PhotoId> = seen[1..11].iter().filter_map(|i| i.photo()).collect();
            assert_ne!(photos, ids(10), "shuffled");
            photos.sort();
            assert_eq!(photos, ids(10), "every photo once, between the titles");
        }
    }

    #[test]
    fn arrows_step_across_the_titles() {
        let s = PlaybackSettings { repeat: false, ..settings(4.0, 1.0) };
        let mut p = titled(3, s, Some(2.0), Some(2.0));
        assert_eq!(p.step(-1, 0.5), Tick::Advanced(Item::INTRO), "← on the intro stays there");
        assert_eq!(p.step(1, 0.5), Tick::Advanced(ph(1)));
        assert_eq!(p.step(-1, 0.6), Tick::Advanced(Item::INTRO), "← from the first photo: back to the intro");
        assert_eq!(p.remaining(0.6), 2.0, "with its full time");
        assert_eq!(p.step(3, 1.0), Tick::Advanced(ph(3)));
        assert_eq!(p.step(1, 1.0), Tick::Advanced(Item::ENDING));
        assert_eq!(p.position(), (3, 3));
        assert_eq!(p.step(-1, 1.0), Tick::Advanced(ph(3)), "← from the ending: the last photo");
        assert_eq!(p.step(1, 1.0), Tick::Advanced(Item::ENDING));
        assert_eq!(p.step(1, 1.0), Tick::Ended, "→ past the ending ends the show");
        assert_eq!(p.step(1, 2.0), Tick::Ended);
        // repeating: ← from the first photo of a later pass wraps to the last, not the intro
        let mut p = titled(3, settings(4.0, 0.0), Some(2.0), None);
        assert_eq!(p.step(4, 0.0), Tick::Advanced(ph(1)), "intro, 1, 2, 3, then 1 again");
        assert_eq!(p.step(-1, 0.0), Tick::Advanced(ph(3)));
        assert_eq!(p.step(i32::MAX, 0.0), Tick::Advanced(p.current()));
        assert_eq!(p.step(i32::MIN, f64::NAN), Tick::Advanced(p.current()));
    }

    #[test]
    fn a_title_pauses_and_waits_like_a_slide() {
        let mut p = titled(2, settings(4.0, 1.0), Some(3.0), None);
        let not_intro = |i: Item| i != Item::INTRO;
        assert_eq!(p.tick(5.0, not_intro), Tick::Idle, "the intro isn't rendered yet: its clock waits");
        assert_eq!(p.remaining(5.0), 3.0);
        assert!(p.toggle_pause(6.0));
        assert_eq!(p.tick(100.0, all_ready), Tick::Idle);
        assert_eq!(p.remaining(100.0), 2.0, "paused on the intro");
        assert!(!p.toggle_pause(100.0));
        assert_eq!(p.tick(101.9, all_ready), Tick::Idle);
        let not_one = |i: Item| i != ph(1);
        assert_eq!(p.tick(102.0, not_one), Tick::Waiting(ph(1)), "the first photo isn't ready: the intro is held");
        assert_eq!(p.frame(102.5).current, Item::INTRO);
        assert_eq!(p.tick(103.0, all_ready), Tick::Advanced(ph(1)));
    }

    #[test]
    fn hostile_title_durations_are_clamped() {
        let p = titled(2, settings(4.0, 1.0), Some(f64::NAN), Some(1e300));
        assert_eq!((p.duration(Item::INTRO), p.duration(Item::ENDING)), (4.0, SLIDE_MAX));
        let p = titled(2, settings(4.0, 1.0), Some(-1.0), Some(f64::NEG_INFINITY));
        assert_eq!((p.duration(Item::INTRO), p.duration(Item::ENDING)), (SLIDE_MIN, 4.0));
        // a single photo between titles, repeating: the photo holds after the intro
        let mut p = titled(1, settings(1.0, 0.0), Some(1.0), Some(1.0));
        assert_eq!(p.upcoming(3), vec![ph(1)]);
        assert_eq!(p.tick(1.0, all_ready), Tick::Advanced(ph(1)));
        assert_eq!(p.upcoming(3), vec![]);
        assert_eq!(p.tick(9.0, all_ready), Tick::Idle);
        assert_eq!(Title::parse(" Ending "), Some(Title::Ending));
        assert_eq!(Title::parse("credits"), None);
    }
}
