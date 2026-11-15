//! The device-sync pill on grid cells and collection rows: two letters (where the original lives,
//! how much of it is here) and an arrow for which way changes still travel (`↑` this device is
//! ahead, `↓` another one is). No colour carries meaning; while the pill is syncing its arrow
//! pulses. The words are in the tooltip ([`tooltip`]).

use egui::{Color32, Pos2, Rect, Sense, pos2, vec2};
use lcc_sync::pill::{Depth, Location, Pill, Status};

use crate::i18n::tr;
use crate::theme::Tokens;
use crate::widgets::register;

/// What the pill says, in words (one line per letter and one for the arrow).
pub fn tooltip(p: &Pill) -> String {
    let location = match p.location {
        Location::Local => tr("L: the original is on this device"),
        Location::Remote => tr("R: the original is on another device"),
        Location::Immich => tr("I: the original is on the Immich server"),
        Location::Equal => tr("E: every device keeps the original and can add photos"),
    };
    let depth = match p.depth {
        Depth::Full => tr("F: the full-quality image is here"),
        Depth::Preview => tr("P: a preview is here, good for developing; edits go back"),
        Depth::None => tr("N: no image is kept here"),
    };
    let status = match p.status {
        Status::Synced => tr("In step with the other devices"),
        Status::LocalAhead => tr("↑ This device has changes still to send"),
        Status::RemoteAhead => tr("↓ Another device has changes still to fetch"),
    };
    let mut out = format!("{}\n{location}\n{depth}\n{status}", tr("Device sync"));
    if p.syncing {
        out.push('\n');
        out.push_str(tr("Syncing now"));
    }
    out
}

/// The arrow's opacity at time `now`: steady, or pulsing while syncing.
fn arrow_alpha(p: &Pill, now: f64) -> f32 {
    if p.syncing { 0.35 + 0.65 * (0.5 + 0.5 * (now * 4.0).sin() as f32) } else { 1.0 }
}

/// The pill's size for `p` (so a caller can place it before painting).
pub fn size(ui: &egui::Ui, p: &Pill) -> egui::Vec2 {
    let t = Tokens::get(ui.ctx());
    let g = ui.painter().layout_no_wrap(p.text(), t.semibold(9.0), Color32::WHITE);
    vec2(g.size().x + 10.0, 14.0)
}

/// Paints `p` with its top left at `at`, addressable as `{key}`, with its tooltip on hover.
pub fn paint(ui: &egui::Ui, at: Pos2, p: &Pill, key: &str) -> Rect {
    let t = Tokens::get(ui.ctx());
    let now = ui.input(|i| i.time);
    let letters = ui.painter().layout_no_wrap(p.letters(), t.semibold(9.0), Color32::WHITE);
    let arrow = p.status.arrow().map(|a| {
        let c = Color32::WHITE.gamma_multiply(arrow_alpha(p, now));
        ui.painter().layout_no_wrap(format!(" {a}"), t.semibold(9.0), c)
    });
    let w = letters.size().x + arrow.as_ref().map_or(0.0, |a| a.size().x) + 10.0;
    let r = Rect::from_min_size(at, vec2(w, 14.0));
    let p_ = ui.painter();
    p_.rect_filled(r, 7.0, Color32::from_black_alpha(160));
    let y = r.center().y - letters.size().y / 2.0;
    let lw = letters.size().x;
    p_.galley(pos2(r.left() + 5.0, y), letters, Color32::WHITE);
    if let Some(a) = arrow {
        p_.galley(pos2(r.left() + 5.0 + lw, y), a, Color32::WHITE);
    }
    if p.syncing {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
    register(ui.ctx(), key.to_string(), r);
    ui.interact(r, egui::Id::new(("sync-pill", key)), Sense::hover()).on_hover_text(tooltip(p));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pill(location: Location, depth: Depth, status: Status, syncing: bool) -> Pill {
        Pill { location, depth, status, syncing }
    }

    #[test]
    fn the_text_is_two_letters_and_an_arrow_only_when_out_of_step() {
        assert_eq!(pill(Location::Remote, Depth::Full, Status::RemoteAhead, false).text(), "RF ↓");
        assert_eq!(pill(Location::Local, Depth::Preview, Status::LocalAhead, false).text(), "LP ↑");
        assert_eq!(pill(Location::Equal, Depth::Full, Status::Synced, false).text(), "EF");
    }

    #[test]
    fn the_tooltip_spells_out_every_part() {
        let p = pill(Location::Remote, Depth::Preview, Status::RemoteAhead, true);
        let tip = tooltip(&p);
        assert!(tip.lines().count() >= 5, "{tip}");
        assert!(tip.contains("R:") && tip.contains("P:") && tip.contains('↓'), "{tip}");
        let quiet = tooltip(&pill(Location::Local, Depth::Full, Status::Synced, false));
        assert!(!quiet.contains('↑') && !quiet.contains('↓'), "{quiet}");
    }

    #[test]
    fn only_a_syncing_pill_pulses() {
        let still = pill(Location::Local, Depth::Full, Status::LocalAhead, false);
        assert_eq!(arrow_alpha(&still, 1.0), 1.0);
        let moving = Pill { syncing: true, ..still };
        let a: Vec<f32> = (0..20).map(|i| arrow_alpha(&moving, f64::from(i) * 0.1)).collect();
        assert!(a.iter().any(|x| *x < 0.6) && a.iter().any(|x| *x > 0.9));
    }
}
