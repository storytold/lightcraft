//! How large the app's window was and whether it was zoomed (macOS) / maximized, so that it opens
//! like that again. Kept with the settings (`ui.json`).
//!
//! The size is the window's as it is when neither zoomed nor full screen: a zoomed window opens
//! zoomed, and goes back to that size when it is un-zoomed. Full screen is not remembered (the
//! app opens as a window), and neither is where on the desktop the window was: the system places
//! it. A place can be on a display that is gone, and only the window system knows which
//! displays there are.
//!
//! Windows and Linux say whether a window is maximized. macOS says so only when the window opens
//! and when the app zooms it itself, so there the window's size is gone by as well (`Unsaid`): a
//! window is zoomed when the system said so, or when it was seen to come to fill its screen, and
//! for as long as it then keeps its size (or is fitted to another screen that it fills). That
//! is a judgement, and it errs to one side on purpose: whatever is in doubt is a window of the
//! size it has, which opens as large as it was left, and only the size it would have gone back
//! to is not kept. So it is with a zoom from the Window menu beside a Dock at the side of the
//! screen, or of a window already as wide as its screen. The other way round there is one case:
//! a window dragged out by hand until it is as wide as its screen and nearly as tall is taken
//! for zoomed, and opens filling the screen.
//!
//! This module only decides (what to remember from what the window reports, what to open with);
//! the desktop app builds its window from it and feeds it each frame.

use serde::{Deserialize, Serialize};

/// The size a window opens with when none is remembered, in points.
pub const DEFAULT_SIZE: [f32; 2] = [1600.0, 1000.0];
/// The smallest the window gets.
pub const MIN_SIZE: [f32; 2] = [900.0, 560.0];
/// No window is wider or taller than this: a larger number in the settings is not a size.
const MAX_SIDE: f32 = 16_384.0;
/// How long a size must stay as it is before it is remembered: a window that is being resized
/// or zoomed (which the system animates) passes through sizes it never rests at.
const SETTLED_SECONDS: f64 = 0.5;
/// A size that has stayed this long when the app quits is remembered too (an animation changes
/// it with every frame; a person who resizes and quits at once has let go of it by then).
const SETTLED_AT_EXIT_SECONDS: f64 = 0.15;
/// Two sizes this near each other, across and down, are one size (a size goes through the
/// screen's pixels and back).
const SAME_WITHIN: f32 = 1.5;
/// How much of its screen's height a zoomed window leaves to the system at most: the menu bar
/// (taller beside a camera housing) and a Dock along the bottom at its largest.
const SCREEN_KEPT_DOWN: f32 = 240.0;
/// A little longer than the wait, so that the frame asked for comes after it and not just before.
const LOOK_AGAIN_MARGIN_SECONDS: f64 = 0.05;
/// How long a window that has just left full screen is not looked at: the system animates the
/// way back, and not every way of leaving says that it is still under way.
const AFTER_FULL_SCREEN_SECONDS: f64 = 1.0;

/// How the window was.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowPlace {
    /// Its inner size while neither zoomed nor full screen, in points (of the system, not of
    /// the interface's own scale).
    pub size: [f32; 2],
    /// Zoomed (macOS) / maximized.
    #[serde(default)]
    pub zoomed: bool,
}

fn size_is_one(size: [f32; 2]) -> bool {
    size.iter().all(|s| s.is_finite() && (1.0..=MAX_SIDE).contains(s))
}

/// `size`, or the smallest the window gets where it is smaller (a window manager that does not
/// keep to the minimum, a script): what is remembered is always a size the window opens at.
fn at_least_min(size: [f32; 2]) -> [f32; 2] {
    [size[0].max(MIN_SIZE[0]), size[1].max(MIN_SIZE[1])]
}

fn same(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).abs() <= SAME_WITHIN && (a[1] - b[1]).abs() <= SAME_WITHIN
}

/// Whether a window of `size` fills `screen` as a zoomed one does where the Dock is hidden or
/// along the bottom: from side to side, and down to what the system keeps.
fn fills(size: [f32; 2], screen: [f32; 2]) -> bool {
    size[0] >= screen[0] - SAME_WITHIN && size[1] >= screen[1] - SCREEN_KEPT_DOWN
}

/// Whether a window of `size` covers `screen` altogether, as one in full screen does.
fn covers(size: [f32; 2], screen: [f32; 2]) -> bool {
    size[0] >= screen[0] - SAME_WITHIN && size[1] >= screen[1] - SAME_WITHIN
}

impl WindowPlace {
    /// What the settings hold (which a person or another version may have written), as
    /// something a window can be: a size within bounds. `None` when the size is no size at all.
    pub fn checked(self) -> Option<WindowPlace> {
        size_is_one(self.size).then(|| WindowPlace { size: at_least_min(self.size), zoomed: self.zoomed })
    }

    /// Reads the window from the settings without letting it cost the rest of them: anything that
    /// is not a window as this version writes it (another shape, a hand edit) reads as nothing
    /// remembered, where a strict reading would have the whole file taken for damaged.
    pub fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<WindowPlace>, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(serde_json::from_value::<Option<WindowPlace>>(value).ok().flatten().and_then(WindowPlace::checked))
    }
}

/// What the window reports in one frame, in the system's points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Seen {
    /// The size of what it shows.
    pub size: Option<[f32; 2]>,
    /// The size of the screen it is on.
    pub screen: Option<[f32; 2]>,
    /// What the window system last said of its being zoomed / maximized.
    pub zoomed: bool,
    pub full_screen: bool,
    pub minimized: bool,
}

impl Seen {
    /// From what egui holds of the window: its `info`, the rectangle it draws in (`content`: the
    /// one size every platform reports, Wayland too) and the interface's own scale (`zoom`), by
    /// which egui's points differ from the system's.
    pub fn of(info: &egui::ViewportInfo, content: egui::Rect, zoom: f32) -> Seen {
        let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
        let scaled = |v: egui::Vec2| [v.x * zoom, v.y * zoom];
        Seen {
            size: Some(scaled(content.size())).filter(|s| size_is_one(*s)),
            screen: info.monitor_size.map(scaled).filter(|s| size_is_one(*s)),
            zoomed: info.maximized == Some(true),
            full_screen: info.fullscreen == Some(true),
            minimized: info.minimized == Some(true),
        }
    }
}

/// Follows the window from frame to frame and says what to remember of it.
#[derive(Debug)]
pub struct WindowWatch {
    /// A size the window has, and since when (seconds), until it has rested there long enough.
    resting: Option<(WindowPlace, f64)>,
    /// Whether the window system's word on being zoomed is kept up to date. On macOS it is only
    /// what was true when the window opened or the app last zoomed it itself (the top bar
    /// double-clicked): Window ▸ Zoom and a drag of the window's edge go unseen.
    told_live: bool,
    /// What the watch goes by where that word is not live.
    unsaid: Unsaid,
    /// The window was full screen when it was last seen.
    full_screen: bool,
    /// Until when (seconds) a window that has left full screen is not looked at.
    back_from_full_screen: Option<f64>,
}

/// Where the window system's word on being zoomed is not live: what the window is taken for.
/// It is zoomed on the system's word, or when it was seen to come to fill its screen, and for as
/// long as it then keeps the size it has zoomed. Anything else is a window: taking a window for
/// zoomed would open it larger than it was left, where taking a zoomed one for a window opens
/// it as large as it was.
#[derive(Debug, Default)]
struct Unsaid {
    /// The system's word at the last frame. When it changes, the app has zoomed or un-zoomed the
    /// window itself, and what it is taken for starts over from that.
    said: bool,
    taken: Taken,
}

#[derive(Clone, Copy, Debug, Default)]
enum Taken {
    /// Not seen yet (or let go of): the system's word says what it is.
    #[default]
    Unseen,
    /// A window, of this size when it was last seen.
    Window { size: [f32; 2] },
    /// Zoomed: at this size since then (seconds), on this screen; `rested` once it has stayed
    /// that size for a moment. Until then it may be on its way there (the system animates a
    /// zoom), and `size` is the largest it has been, across and down: on its way it only grows.
    Zoomed { size: [f32; 2], since: f64, screen: Option<[f32; 2]>, rested: bool },
}

impl Default for WindowWatch {
    fn default() -> Self {
        WindowWatch::told_live(!cfg!(target_os = "macos"))
    }
}

impl WindowWatch {
    /// A watch on a platform where the window system's word on being zoomed is live, or not.
    pub fn told_live(told_live: bool) -> WindowWatch {
        WindowWatch { resting: None, told_live, unsaid: Unsaid::default(), full_screen: false, back_from_full_screen: None }
    }

    /// The window is `seen` at time `now` (seconds), and `remembered` is what the settings hold:
    /// returns what they should hold from now on. Whether the window is zoomed is taken at
    /// once; a size only once the window has rested at it for a moment. A minimized or
    /// full-screen window changes nothing, nor does one that has only just left full screen.
    pub fn note(&mut self, remembered: Option<WindowPlace>, seen: &Seen, now: f64) -> Option<WindowPlace> {
        if seen.full_screen {
            self.full_screen = true;
            self.back_from_full_screen = None;
            self.let_go();
            return remembered;
        }
        if std::mem::take(&mut self.full_screen) {
            self.back_from_full_screen = Some(now + AFTER_FULL_SCREEN_SECONDS);
        }
        if seen.minimized {
            self.back_from_full_screen = None;
            self.let_go();
            return remembered;
        }
        if self.back_from_full_screen.is_some_and(|until| now < until) {
            self.let_go();
            return remembered;
        }
        self.back_from_full_screen = None;
        let zoomed = if self.told_live { seen.zoomed } else { self.taken_for_zoomed(seen, now) };
        if zoomed {
            // (its size now is the screen's, not the window's own: the size it goes back to stays)
            self.resting = None;
            return Some(WindowPlace { size: remembered.map_or(DEFAULT_SIZE, |r| r.size), zoomed: true });
        }
        let Some(size) = seen.size else {
            self.resting = None;
            return remembered;
        };
        if seen.screen.is_some_and(|screen| covers(size, screen)) {
            // (no window of its own size covers its screen: full screen, though nothing says so)
            self.resting = None;
            return remembered;
        }
        let place = WindowPlace { size: at_least_min(size), zoomed: false };
        if Some(place) == remembered {
            self.resting = None;
            return remembered;
        }
        match self.resting {
            Some((resting, since)) if resting == place => {
                if now - since >= SETTLED_SECONDS {
                    self.resting = None;
                    return Some(place);
                }
            }
            _ => self.resting = Some((place, now)),
        }
        // (meanwhile only what is certain: the window is not zoomed)
        remembered.map(|r| WindowPlace { zoomed: false, ..r })
    }

    /// Lets go of whatever has not rested yet: nothing waits (and wakes the app) while the
    /// window is not one to look at. A window taken for zoomed stays that, at the size it was
    /// last seen at.
    fn let_go(&mut self) {
        self.resting = None;
        if let Taken::Zoomed { rested, .. } = &mut self.unsaid.taken {
            *rested = true;
        }
    }

    /// Whether the window is zoomed, where the window system does not keep saying (`Unsaid`).
    fn taken_for_zoomed(&mut self, seen: &Seen, now: f64) -> bool {
        let Some(size) = seen.size else {
            // (nothing to go by in this frame: as it was taken, or by the system's word if it
            // was never seen)
            self.let_go();
            return match self.unsaid.taken {
                Taken::Unseen => seen.zoomed,
                taken => matches!(taken, Taken::Zoomed { .. }),
            };
        };
        let zoomed_now = Taken::Zoomed { size, since: now, screen: seen.screen, rested: false };
        let fills_its_screen = |size: [f32; 2]| seen.screen.is_some_and(|screen| fills(size, screen));
        self.unsaid.taken = if matches!(self.unsaid.taken, Taken::Unseen) || seen.zoomed != self.unsaid.said {
            if seen.zoomed { zoomed_now } else { Taken::Window { size } }
        } else {
            match self.unsaid.taken {
                // it comes to fill its screen: zoomed from the Window menu
                Taken::Window { size: was } if fills_its_screen(size) && !fills_its_screen(was) => zoomed_now,
                Taken::Window { .. } | Taken::Unseen => Taken::Window { size },
                Taken::Zoomed { size: at, since, screen, rested } => {
                    // (at one size for a moment: rested, whether or not a frame came to see it)
                    let rested = rested || now - since >= SETTLED_SECONDS;
                    let other_screen = seen.screen.is_some() && screen != seen.screen;
                    if same(at, size) && (!other_screen || fills_its_screen(size)) {
                        // as it was (not carried, as large as it is, to a screen it does not fill)
                        Taken::Zoomed { size: at, since, screen, rested }
                    } else if !rested && !other_screen && size[0] >= at[0] - SAME_WITHIN && size[1] >= at[1] - SAME_WITHIN {
                        // on its way there: larger than it has been, and never smaller again
                        Taken::Zoomed { size: [size[0].max(at[0]), size[1].max(at[1])], since: now, screen, rested: false }
                    } else if rested && other_screen && fills_its_screen(size) {
                        // its screen changed (a display unplugged, another resolution) and it fills the new one
                        zoomed_now
                    } else {
                        // resized by its edge, or zoomed back from the Window menu
                        Taken::Window { size }
                    }
                }
            }
        };
        self.unsaid.said = seen.zoomed;
        matches!(self.unsaid.taken, Taken::Zoomed { .. })
    }

    /// How long until a size the window has will have rested long enough, if one is waiting: the
    /// app then looks again (nothing else may ask for a frame once the window is let go of).
    pub fn waits(&self, now: f64) -> Option<std::time::Duration> {
        let (since, wait) = match (self.back_from_full_screen, self.resting, self.unsaid.taken) {
            (Some(until), _, _) => (until - AFTER_FULL_SCREEN_SECONDS, AFTER_FULL_SCREEN_SECONDS),
            (None, Some((_, since)), _) | (None, None, Taken::Zoomed { since, rested: false, .. }) => (since, SETTLED_SECONDS),
            _ => return None,
        };
        let left = (wait - (now - since)).clamp(0.0, wait) + LOOK_AGAIN_MARGIN_SECONDS;
        Some(std::time::Duration::try_from_secs_f64(left).unwrap_or(std::time::Duration::from_secs(1)))
    }

    /// The app quits at time `now`: what the settings should hold. A size the window has rested
    /// at, if only briefly, is remembered; one it is passing through is not.
    pub fn at_exit(&mut self, remembered: Option<WindowPlace>, now: f64) -> Option<WindowPlace> {
        match self.resting.take() {
            Some((place, since)) if now - since >= SETTLED_AT_EXIT_SECONDS => Some(place),
            _ => remembered,
        }
    }
}

#[cfg(test)]
mod tests;
