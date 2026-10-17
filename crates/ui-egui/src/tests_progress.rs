//! Progress bars, spinners and busy buttons: the fraction is always a sane number, a bar is
//! announced as progress with its percentage and reachable by its id, a busy button is announced
//! busy, ignores clicks and keeps its width, and a thumbnail's placeholder stands apart from what
//! it sits on in every theme.

use egui::{Pos2, Rect, pos2, vec2};

use crate::button::{Button, contrast};
use crate::icons::Icon;
use crate::progress::{Progress, clamp_fraction, spinner};
use crate::theme::{ThemeKind, Tokens};

/// What a screen reader gets for one node.
#[derive(Debug, Clone, PartialEq)]
struct Node {
    role: String,
    label: String,
    busy: bool,
    value: Option<f64>,
}

/// Run three frames of `draw` (pointer `events` in the second); return the nodes of the last
/// frame with an accessibility tree, the automation ids registered in the last frame, and what
/// `draw` returned.
fn frames<T: Default>(events: Vec<egui::Event>, mut draw: impl FnMut(&mut egui::Ui) -> T) -> (Vec<Node>, Vec<(String, Rect)>, T) {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let (mut nodes, mut ids, mut last) = (Vec::new(), Vec::new(), T::default());
    for frame in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            events: if frame == 1 { events.clone() } else { Vec::new() },
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| last = draw(ui));
        out.textures_delta.clear();
        ids = crate::widgets::take_registry(&ctx);
        if let Some(update) = out.platform_output.accesskit_update.take() {
            nodes = update
                .nodes
                .iter()
                .map(|(_, n)| Node {
                    role: format!("{:?}", n.role()),
                    label: n.label().unwrap_or_default().to_string(),
                    busy: n.is_busy(),
                    value: n.numeric_value(),
                })
                .collect();
        }
    }
    (nodes, ids, last)
}

#[test]
fn the_fraction_is_always_between_nothing_and_all() {
    assert_eq!(clamp_fraction(f32::NAN), 0.0);
    assert_eq!(clamp_fraction(-3.0), 0.0);
    assert_eq!(clamp_fraction(7.5), 1.0);
    assert_eq!(clamp_fraction(f32::INFINITY), 1.0);
    assert_eq!(clamp_fraction(f32::NEG_INFINITY), 0.0);
    assert_eq!(clamp_fraction(0.25), 0.25);
    assert_eq!(Progress::new(Some(f32::NAN)).fraction(), Some(0.0));
    assert_eq!(Progress::new(Some(2.0)).fraction(), Some(1.0));
    assert_eq!(Progress::new(None).fraction(), None);
}

#[test]
fn a_bar_is_progress_with_a_value_and_reachable_by_its_id() {
    let (nodes, ids, _) = frames(Vec::new(), |ui| {
        Progress::new(Some(0.42)).label("Making the denoised picture… 42%").widget("denoise:progress").show(ui);
    });
    let bar = nodes.iter().find(|n| n.role == "ProgressIndicator").unwrap_or_else(|| panic!("no progress node: {nodes:?}"));
    assert_eq!(bar.label, "Making the denoised picture… 42%");
    assert_eq!(bar.value, Some(42.0));
    assert!(ids.iter().any(|(id, r)| id == "denoise:progress" && r.width() > 0.0), "{ids:?}");
}

#[test]
fn an_indeterminate_bar_says_progress_without_a_value_and_is_drawn() {
    let (nodes, ids, width) = frames(Vec::new(), |ui| Progress::new(None).label("Starting…").widget("p:x").show(ui).rect.width());
    let bar = nodes.iter().find(|n| n.role == "ProgressIndicator").unwrap_or_else(|| panic!("{nodes:?}"));
    assert_eq!(bar.value, None);
    assert!(ids.iter().any(|(id, _)| id == "p:x"));
    assert!(width >= 96.0, "fills the width the layout has");
}

#[test]
fn a_bar_fills_the_layout_unless_given_a_width() {
    let (_, _, (fill, fixed, tiny_room)) = frames(Vec::new(), |ui| {
        let fill = Progress::new(Some(0.5)).show(ui).rect.width();
        let fixed = Progress::new(Some(0.5)).width(150.0).show(ui).rect.width();
        let tiny_room = ui.scope(|ui| {
            ui.set_max_width(40.0);
            Progress::new(Some(0.5)).show(ui).rect.width()
        });
        (fill, fixed, tiny_room.inner)
    });
    assert!(fill > 300.0, "fills: {fill}");
    assert_eq!(fixed, 150.0);
    assert!(tiny_room >= 96.0, "never a sliver: {tiny_room}");
}

#[test]
fn a_spinner_takes_the_room_it_is_given() {
    let (_, _, size) = frames(Vec::new(), |ui| spinner(ui, 14.0).rect.size());
    assert_eq!(size, vec2(14.0, 14.0));
}

fn click_at(at: Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ]
}

#[test]
fn a_busy_button_ignores_clicks_and_is_announced_busy() {
    let place = Rect::from_min_size(Pos2::ZERO, vec2(120.0, 30.0));
    let click = |busy: bool| {
        let mut clicked = false;
        let (nodes, _, ()) = frames(click_at(pos2(40.0, 15.0)), |ui| clicked |= Button::new("Analyze").busy(busy).show_at(ui, place).clicked());
        (nodes, clicked)
    };
    let (nodes, clicked) = click(true);
    assert!(!clicked, "a busy button does nothing");
    let node = nodes.iter().find(|n| n.role == "Button" && n.label == "Analyze").unwrap_or_else(|| panic!("{nodes:?}"));
    assert!(node.busy, "announced busy");

    let (nodes, clicked) = click(false);
    assert!(clicked, "the same button, not busy, is clicked");
    assert!(!nodes.iter().any(|n| n.busy), "{nodes:?}");
}

#[test]
fn a_busy_button_stays_reachable_by_the_keyboard() {
    let place = Rect::from_min_size(Pos2::ZERO, vec2(120.0, 30.0));
    let mut asked = false;
    let (_, _, focused) = frames(Vec::new(), |ui| {
        let r = Button::new("Analyze").busy(true).show_at(ui, place);
        if !std::mem::replace(&mut asked, true) {
            r.request_focus();
        }
        r.has_focus()
    });
    assert!(focused, "focus is kept while it works");
}

#[test]
fn a_button_keeps_its_width_while_busy() {
    for (text, icon) in [("Analyze", None), ("Analyze", Some(Icon::Trash)), ("", Some(Icon::Trash))] {
        for small in [false, true] {
            let (_, _, (idle, busy)) = frames(Vec::new(), |ui| {
                let mut make = |b: bool| {
                    let mut btn = if text.is_empty() { Button::icon_only(Icon::Trash, "Remove") } else { Button::new(text) };
                    if let Some(i) = icon {
                        btn = btn.icon(i);
                    }
                    if small {
                        btn = btn.small();
                    }
                    btn.busy(b).show(ui).rect.size()
                };
                (make(false), make(true))
            });
            assert_eq!(idle, busy, "{text:?} {icon:?} small={small}");
        }
    }
}

#[test]
fn the_thumbnail_placeholder_stands_apart_in_every_theme() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        for (name, behind) in [("grid", t.grid_bg), ("cell", t.cell), ("selected cell", t.cell_selected), ("canvas", t.canvas)] {
            let c = contrast(t.skeleton, behind);
            assert!(c >= 1.1, "{kind:?}: the placeholder on the {name} is {c:.2}:1");
        }
    }
}
