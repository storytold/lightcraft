//! The star rating: clicking sets the rating, clicking the current one clears it, the row is one Tab
//! stop, the digits set it, and screen readers hear every star.

use egui::{Pos2, Rect, vec2};

use crate::rating::Rating;

/// A context with the framework's runtime, which senses the row and builds its accessibility nodes.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    ctx.enable_accesskit();
    crate::app::install_runtime(&ctx).expect("the UI runtime installs");
    ctx
}

fn raw(size: egui::Vec2, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() }
}

fn press(key: egui::Key) -> egui::Event {
    egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Default::default() }
}

/// Draw `build` for a few frames. From the second on, move to the centre of star `click` (1 to 5) if given, press
/// and release there (a frame each: the framework counts a click from a press in one pass and a release in a later
/// one), then press `keys`, one a frame. Returns every value `build` reported and the AccessKit (role, name) pairs.
fn drive(click: Option<u8>, keys: &[egui::Key], build: impl Fn(&mut egui::Ui) -> Option<u8>) -> (Vec<u8>, Vec<(String, String)>) {
    let ctx = context();
    let mut reported = Vec::new();
    let mut nodes = Vec::new();
    let mut rects: Vec<(String, Rect)> = Vec::new();
    let button = |at: Pos2, pressed: bool| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    for frame in 0..10 {
        let mut events = Vec::new();
        if let Some(n) = click {
            let want = format!("star:t:{n}");
            let at = rects.iter().find(|(id, _)| *id == want).map(|(_, r)| r.center()).unwrap_or(Pos2::ZERO);
            match frame {
                3 => events.push(egui::Event::PointerMoved(at)),
                4 => events.push(button(at, true)),
                5 => events.push(button(at, false)),
                _ => {}
            }
        }
        // one key per frame from the fifth on, so Tab has moved the focus before the next key arrives
        if frame >= 4
            && let Some(k) = keys.get(frame - 4)
        {
            events.push(press(*k));
        }
        let mut out = ctx.run_ui(raw(vec2(400.0, 200.0), events), |ui| {
            if let Some(v) = build(ui) {
                reported.push(v);
            }
        });
        out.textures_delta.clear();
        let seen = crate::widgets::take_registry(&ctx);
        if rects.is_empty() {
            rects = seen;
        }
        if let Some(update) = out.platform_output.accesskit_update.take() {
            nodes = update.nodes.iter().map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string())).collect();
        }
    }
    (reported, nodes)
}

fn row(rating: u8) -> Rating {
    Rating::new("t", rating)
}

#[test]
fn clicking_a_star_sets_that_rating() {
    let (got, _) = drive(Some(4), &[], |ui| row(2).show(ui));
    assert_eq!(got, vec![4]);
}

#[test]
fn clicking_the_current_rating_clears_it() {
    let (got, _) = drive(Some(3), &[], |ui| row(3).show(ui));
    assert_eq!(got, vec![0]);
}

#[test]
fn hovering_alone_changes_nothing() {
    // the preview is only painted; the rating is reported on click
    let (got, _) = drive(None, &[], |ui| row(2).show(ui));
    assert!(got.is_empty());
}

/// What `row(rating)` paints in the last of a few frames after the pointer has moved to the centre of star `hover`
/// (1 to 5), if given.
fn painted(rating: u8, hover: Option<u8>) -> Vec<egui::epaint::ClippedShape> {
    let ctx = context();
    let mut at = None;
    let mut shapes = Vec::new();
    for frame in 0..6 {
        let events = match at.filter(|_| frame >= 1) {
            Some(at) => vec![egui::Event::PointerMoved(at)],
            None => Vec::new(),
        };
        let mut out = ctx.run_ui(raw(vec2(400.0, 200.0), events), |ui| {
            let _ = row(rating).show(ui);
        });
        out.textures_delta.clear();
        let seen = crate::widgets::take_registry(&ctx);
        if let (Some(n), true) = (hover, frame == 0) {
            let want = format!("star:t:{n}");
            at = seen.iter().find(|(id, _)| *id == want).map(|(_, r)| r.center());
            assert!(at.is_some(), "star {n} is registered");
        }
        shapes = out.shapes;
    }
    shapes
}

#[test]
fn hovering_a_star_paints_the_rating_a_click_would_set() {
    // the pointer over star 4 of a row showing 2 paints what a row showing 4 paints under the same pointer, and not
    // what the row paints at rest
    let hovered = painted(2, Some(4));
    assert_eq!(hovered, painted(4, Some(4)), "hovering star 4 previews 4 stars");
    assert_ne!(hovered, painted(2, None), "the preview differs from the rating shown");
    assert_ne!(painted(2, Some(1)), painted(2, None), "hovering a lower star previews the lower rating");
}

#[test]
fn a_screen_reader_hears_every_star_with_its_state() {
    let (_, nodes) = drive(None, &[], |ui| row(2).show(ui));
    for n in 1..=5 {
        let name = if n == 1 { "1 star".to_owned() } else { format!("{n} stars") };
        assert!(nodes.iter().any(|(role, label)| role == "RadioButton" && *label == name), "{name}: {nodes:?}");
    }
}

#[test]
fn the_digits_set_the_rating_while_the_row_has_the_focus() {
    // Tab focuses the row; 4 then sets four stars
    let (got, _) = drive(None, &[egui::Key::Tab, egui::Key::Num4], |ui| row(2).show(ui));
    assert_eq!(got, vec![4], "{got:?}");
}

#[test]
fn the_arrow_keys_move_between_the_stars_without_changing_the_rating() {
    let (got, _) = drive(None, &[egui::Key::Tab, egui::Key::ArrowRight, egui::Key::ArrowRight], |ui| row(3).show(ui));
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn a_rating_row_is_one_tab_stop() {
    use craft::ui::buttons::Button;
    use craft::ui::contract::Show as _;
    // left neighbour, one stop for the five stars, right neighbour: the third Tab is already past the row
    let ctx = context();
    let mut focused = Vec::new();
    for frame in 0..8 {
        // Tab in the frames 3 to 5
        let events = if (3..=5).contains(&frame) { vec![press(egui::Key::Tab)] } else { Vec::new() };
        let mut right = None;
        let mut out = ctx.run_ui(raw(vec2(600.0, 200.0), events), |ui| {
            ui.horizontal(|ui| {
                let _ = Button::new("left neighbour").show(ui);
                let _ = row(3).show(ui);
                right = Some(Button::new("right neighbour").show(ui).response.id);
            });
        });
        out.textures_delta.clear();
        focused.push(right.is_some_and(|id| ctx.memory(|m| m.has_focus(id))));
    }
    // frames 3 to 5 press Tab: left neighbour, the row, right neighbour
    assert_eq!(focused, vec![false, false, false, false, false, true, true, true], "{focused:?}");
}
