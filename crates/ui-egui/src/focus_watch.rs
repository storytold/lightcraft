//! The window's focus and visibility changes, timed (issue #1188: a spinning cursor on macOS
//! when the app is brought back to the front, even while idle).
//!
//! Each frame in which the window gains or loses the keyboard focus, or is hidden (occluded,
//! minimized) or shown again, is named in the frame's log (so a slow frame says it was one), and
//! its time on the UI thread is logged at debug level; with `<PREFIX>_PROFILE` set it is also
//! printed to stderr, so a macOS user can confirm where a stall on re-activation is spent.
//!
//! What runs on such a frame must stay cheap: the work a regained focus starts (the folder poll,
//! the camera-roll watch, the recent-library checks) runs on worker threads under a `TaskGuard`.

/// What the window is, as the host reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowState {
    pub focused: bool,
    pub visible: bool,
}

impl WindowState {
    /// The root viewport's state this frame (a host that reports nothing counts as focused and
    /// visible).
    pub fn of(ctx: &egui::Context) -> Self {
        ctx.input(|i| WindowState { focused: i.focused, visible: i.viewport().visible().unwrap_or(true) })
    }
}

/// Watches [`WindowState`] from frame to frame.
#[derive(Debug, Default)]
pub struct FocusWatch {
    last: Option<WindowState>,
    /// The change seen at the start of this frame, reported at its end.
    pending: Option<&'static str>,
    /// The change of the previous frame, whose host time (painting and presenting, where a GPU
    /// surface reconfigure would show) is only known on the next frame.
    follow: Option<&'static str>,
    /// Changes seen since start (the first frame is not one).
    pub changes: u64,
    /// The UI-thread time of the last frame with a change.
    pub last_ms: Option<f64>,
    /// The slowest such frame.
    pub worst_ms: f64,
}

/// The name of the change from `before` to `now` (`None`: no change). A frame that both shows and
/// focuses the window is named by its focus.
pub fn change(before: WindowState, now: WindowState) -> Option<&'static str> {
    match (before.focused, now.focused, before.visible, now.visible) {
        (false, true, ..) => Some("window focus gained"),
        (true, false, ..) => Some("window focus lost"),
        (_, _, false, true) => Some("window shown"),
        (_, _, true, false) => Some("window hidden"),
        _ => None,
    }
}

impl FocusWatch {
    /// Note this frame's state (at the start of the frame's logic); the change it makes, if any.
    /// `host_prev_ms` is the host's time for the previous frame, painting included (`None`
    /// headless): logged for the frame of a change, as the time `finish` cannot see.
    pub fn observe(&mut self, now: WindowState, host_prev_ms: Option<f64>) -> Option<&'static str> {
        if let (Some(what), Some(ms)) = (self.follow.take(), host_prev_ms.filter(|m| m.is_finite())) {
            log::debug!("{what}: {ms:.1} ms host frame (painting and presenting included)");
            if profiling() {
                eprintln!("[profile] {what}: {ms:.1} ms host frame (painting and presenting included)");
            }
        }
        let before = self.last.replace(now);
        self.pending = before.and_then(|b| change(b, now));
        if self.pending.is_some() {
            self.changes = self.changes.saturating_add(1);
        }
        self.pending
    }

    /// The frame ended after `logic_ms` + `layout_ms` on the UI thread: log a change seen in it.
    pub fn finish(&mut self, logic_ms: f64, layout_ms: f64) {
        let Some(what) = self.pending.take() else { return };
        self.follow = Some(what);
        let ms = if (logic_ms + layout_ms).is_finite() { (logic_ms + layout_ms).max(0.0) } else { 0.0 };
        self.last_ms = Some(ms);
        self.worst_ms = self.worst_ms.max(ms);
        log::debug!("{what}: {ms:.1} ms on the UI thread (logic {logic_ms:.1} ms, layout {layout_ms:.1} ms)");
        if profiling() {
            eprintln!("[profile] {what}: {ms:.1} ms on the UI thread (logic {logic_ms:.1} ms, layout {layout_ms:.1} ms)");
        }
    }
}

fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| lcc_brand::env_is_set("PROFILE"))
}

#[cfg(test)]
mod tests {
    use super::*;

    impl FocusWatch {
        fn observe_t(&mut self, now: WindowState) -> Option<&'static str> {
            self.observe(now, Some(f64::NAN))
        }
    }

    const FV: WindowState = WindowState { focused: true, visible: true };
    const BV: WindowState = WindowState { focused: false, visible: true };
    const BH: WindowState = WindowState { focused: false, visible: false };

    #[test]
    fn changes_are_named_and_counted_once() {
        let mut w = FocusWatch::default();
        assert_eq!(w.observe_t(FV), None, "the first frame is no change");
        assert_eq!(w.observe_t(FV), None);
        assert_eq!(w.observe_t(BV), Some("window focus lost"));
        assert_eq!(w.observe_t(BH), Some("window hidden"));
        assert_eq!(w.observe_t(BH), None);
        assert_eq!(w.observe_t(FV), Some("window focus gained"));
        assert_eq!(w.changes, 3);
    }

    #[test]
    fn finish_records_only_changed_frames_and_survives_bad_times() {
        let mut w = FocusWatch::default();
        w.observe_t(FV);
        w.finish(5.0, 5.0);
        assert_eq!(w.last_ms, None);
        w.observe_t(BV);
        w.finish(f64::NAN, 1.0);
        assert_eq!(w.last_ms, Some(0.0));
        w.observe_t(FV);
        w.finish(-3.0, 1.0);
        assert_eq!(w.last_ms, Some(0.0));
        assert!(w.worst_ms.is_finite());
    }
}
