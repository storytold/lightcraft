//! Restores mouse-button releases that egui-winit drops.
//!
//! egui-winit forwards a button event only while it knows where the pointer is, and it forgets
//! that when the cursor leaves the window (`CursorLeft` → `PointerGone`). A release that comes
//! after the cursor left, with no move in between, is dropped. Ending a slider drag just past the
//! window's edge is enough. egui then believes the button is still down: the slider stays
//! captured, follows the cursor when it comes back, and takes every later press, so clicks on the
//! photo stop working.
//!
//! [`PointerRepair::repair`] runs over each frame's raw events before egui sees them and puts the
//! release back:
//! - **Back inside:** a pointer that left during a press and comes back inside the window without
//!   having moved outside was released out there (while the button is held the window keeps
//!   receiving moves outside it), so it is released where it left.
//! - **Press while down:** a press of a button egui still believes is down gets its missing
//!   release first.

/// The pointer buttons egui knows (`egui::PointerButton` as an index).
const BUTTONS: usize = egui::NUM_POINTER_BUTTONS;

/// What [`PointerRepair::repair`] remembers between frames.
#[derive(Debug, Default)]
pub struct PointerRepair {
    /// Buttons down as far as egui knows (pressed, no release seen).
    down: [bool; BUTTONS],
    /// Where the pointer was last seen.
    last: Option<egui::Pos2>,
    /// Where the pointer left the window while a button was down (`PointerGone`).
    left_at: Option<egui::Pos2>,
    /// The window's area in points (the last one the integration announced).
    screen: Option<egui::Rect>,
}

fn index(b: egui::PointerButton) -> usize {
    match b {
        egui::PointerButton::Primary => 0,
        egui::PointerButton::Secondary => 1,
        egui::PointerButton::Middle => 2,
        egui::PointerButton::Extra1 => 3,
        egui::PointerButton::Extra2 => 4,
    }
}

fn button(i: usize) -> Option<egui::PointerButton> {
    [
        egui::PointerButton::Primary,
        egui::PointerButton::Secondary,
        egui::PointerButton::Middle,
        egui::PointerButton::Extra1,
        egui::PointerButton::Extra2,
    ]
    .get(i)
    .copied()
}

impl PointerRepair {
    /// Puts back the releases egui-winit dropped in `raw` (see the module docs).
    pub fn repair(&mut self, raw: &mut egui::RawInput) {
        if let Some(r) = raw.screen_rect {
            self.screen = Some(r);
        }
        if !raw.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::PointerGone)) {
            return;
        }
        let modifiers = egui::Modifiers::NONE;
        let events = std::mem::take(&mut raw.events);
        // for each event, the buttons released by a real event later in this batch
        let mut released_later = vec![[false; BUTTONS]; events.len()];
        let mut seen = [false; BUTTONS];
        for (e, later) in events.iter().zip(released_later.iter_mut()).rev() {
            *later = seen;
            if let egui::Event::PointerButton { button: b, pressed: false, .. } = e
                && let Some(s) = seen.get_mut(index(*b))
            {
                *s = true;
            }
        }
        let mut out = Vec::with_capacity(events.len() + 1);
        for (e, later) in events.into_iter().zip(released_later) {
            match e {
                egui::Event::PointerMoved(p) => {
                    if let Some(at) = self.left_at.take() {
                        let inside = self.screen.is_none_or(|r| r.contains(p));
                        if inside {
                            // back without a move outside: the button went up out there, unless its own
                            // release is still to come in this batch
                            self.release_all(at, modifiers, &later, &mut out);
                        }
                    }
                    self.last = Some(p);
                    out.push(e);
                }
                egui::Event::PointerGone => {
                    if self.down.iter().any(|d| *d) {
                        self.left_at = self.last;
                    }
                    out.push(e);
                }
                egui::Event::PointerButton { pos, button: b, pressed, modifiers: m } => {
                    let i = index(b);
                    if pressed {
                        if self.down.get(i).copied().unwrap_or(false) {
                            out.push(egui::Event::PointerButton { pos, button: b, pressed: false, modifiers: m });
                        }
                        self.left_at = None;
                    }
                    if let Some(d) = self.down.get_mut(i) {
                        *d = pressed;
                    }
                    self.last = Some(pos);
                    out.push(e);
                }
                other => out.push(other),
            }
        }
        raw.events = out;
    }

    /// Whether a button is down as far as the host's events said (tests).
    #[cfg(test)]
    pub(crate) fn any_down(&self) -> bool {
        self.down.iter().any(|d| *d)
    }

    /// Releases, where the pointer left (`at`), every button still down but those released later in the batch
    /// (`released_later`): those keep their real release. The pointer is moved there first only when a release
    /// is missing.
    fn release_all(&mut self, at: egui::Pos2, modifiers: egui::Modifiers, released_later: &[bool; BUTTONS], out: &mut Vec<egui::Event>) {
        let missing = |(d, later): (&bool, &bool)| *d && !*later;
        if !self.down.iter().zip(released_later).any(missing) {
            return;
        }
        out.push(egui::Event::PointerMoved(at));
        for (i, (d, later)) in self.down.iter_mut().zip(released_later).enumerate() {
            if *d
                && !*later
                && let Some(b) = button(i)
            {
                *d = false;
                out.push(egui::Event::PointerButton { pos: at, button: b, pressed: false, modifiers });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, Rect, pos2};

    fn press(p: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    fn frame(r: &mut PointerRepair, events: Vec<Event>) -> Vec<Event> {
        let mut raw = egui::RawInput { screen_rect: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0))), events, ..Default::default() };
        r.repair(&mut raw);
        raw.events
    }

    fn releases(events: &[Event]) -> usize {
        events.iter().filter(|e| matches!(e, Event::PointerButton { pressed: false, .. })).count()
    }

    /// A batch that comes back inside and then carries the button's real release: that release is kept, and no
    /// release (nor move to where the pointer left) is made up before it. Another button still down with no
    /// release to come is still released (CodeRabbit on #1060).
    #[test]
    fn a_real_release_later_in_the_batch_is_not_made_up_before_it() {
        let mut r = PointerRepair::default();
        frame(&mut r, vec![Event::PointerMoved(pos2(700.0, 590.0)), press(pos2(700.0, 590.0), true)]);
        let back = frame(&mut r, vec![Event::PointerGone, Event::PointerMoved(pos2(300.0, 300.0)), press(pos2(300.0, 300.0), false)]);
        assert_eq!(back, vec![Event::PointerGone, Event::PointerMoved(pos2(300.0, 300.0)), press(pos2(300.0, 300.0), false)]);
        assert!(!r.any_down());
        // two buttons down, only the secondary's release to come: the primary is released where it left
        let secondary =
            |pressed| Event::PointerButton { pos: pos2(700.0, 590.0), button: PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
        frame(&mut r, vec![Event::PointerMoved(pos2(700.0, 590.0)), press(pos2(700.0, 590.0), true), secondary(true)]);
        let back = frame(&mut r, vec![Event::PointerGone, Event::PointerMoved(pos2(300.0, 300.0)), secondary(false)]);
        assert_eq!(
            back,
            vec![
                Event::PointerGone,
                Event::PointerMoved(pos2(700.0, 590.0)),
                press(pos2(700.0, 590.0), false),
                Event::PointerMoved(pos2(300.0, 300.0)),
                secondary(false),
            ]
        );
        assert!(!r.any_down());
    }

    /// The case egui-winit gets wrong: a drag that ends just past the window's edge.
    #[test]
    fn a_release_lost_outside_the_window_is_restored_when_the_pointer_comes_back() {
        let mut r = PointerRepair::default();
        frame(&mut r, vec![Event::PointerMoved(pos2(700.0, 590.0)), press(pos2(700.0, 590.0), true)]);
        frame(&mut r, vec![Event::PointerMoved(pos2(720.0, 599.0)), Event::PointerGone]);
        // (the release happened outside and was dropped) … the cursor comes back over the photo
        let back = frame(&mut r, vec![Event::PointerMoved(pos2(300.0, 300.0))]);
        assert_eq!(releases(&back), 1, "released: {back:?}");
        assert!(
            matches!(back.get(1), Some(Event::PointerButton { pos, pressed: false, .. }) if *pos == pos2(720.0, 599.0)),
            "where it left, before the move inside: {back:?}"
        );
        // a click on the photo now is a click
        let click = frame(&mut r, vec![press(pos2(300.0, 300.0), true), press(pos2(300.0, 300.0), false)]);
        assert_eq!(click.len(), 2, "{click:?}");
    }

    /// A drag that leaves the window and comes back while the button is held keeps going.
    #[test]
    fn a_drag_held_outside_the_window_is_not_ended() {
        let mut r = PointerRepair::default();
        frame(&mut r, vec![Event::PointerMoved(pos2(700.0, 590.0)), press(pos2(700.0, 590.0), true)]);
        frame(&mut r, vec![Event::PointerGone]);
        // the window keeps getting moves outside it while the button is held (mouse capture)
        let outside = frame(&mut r, vec![Event::PointerMoved(pos2(760.0, 640.0)), Event::PointerMoved(pos2(500.0, 580.0))]);
        assert_eq!(releases(&outside), 0, "{outside:?}");
        let up = frame(&mut r, vec![press(pos2(500.0, 580.0), false)]);
        assert_eq!(releases(&up), 1);
    }

    /// Whatever went missing, a new press first ends the stale one.
    #[test]
    fn a_press_while_egui_thinks_the_button_is_down_is_preceded_by_a_release() {
        let mut r = PointerRepair::default();
        frame(&mut r, vec![press(pos2(10.0, 10.0), true)]);
        let again = frame(&mut r, vec![press(pos2(50.0, 50.0), true)]);
        assert_eq!(again.len(), 2);
        assert!(matches!(again.first(), Some(Event::PointerButton { pressed: false, .. })));
        // an ordinary click passes through untouched
        let mut r = PointerRepair::default();
        let click = frame(&mut r, vec![Event::PointerMoved(pos2(5.0, 5.0)), press(pos2(5.0, 5.0), true), press(pos2(5.0, 5.0), false)]);
        assert_eq!(click.len(), 3);
        // leaving and coming back with no button down changes nothing
        frame(&mut r, vec![Event::PointerGone]);
        assert_eq!(frame(&mut r, vec![Event::PointerMoved(pos2(9.0, 9.0))]).len(), 1);
    }
}
