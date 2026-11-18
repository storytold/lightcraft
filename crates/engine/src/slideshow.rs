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
mod tests;
