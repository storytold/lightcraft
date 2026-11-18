//! The star rating: clicking sets the rating, clicking the current one clears it, the row is one
//! Tab stop, the arrow keys step it, and screen readers hear every star.

use egui::{Pos2, Rect, vec2};

use crate::rating::Rating;

/// Draw `build` for a few frames. After the first, click the centre of star `click` (1 to 5) if given,
/// then press `keys`. Returns every value `build` reported and the AccessKit
/// (role, name) pairs.
fn drive(click: Option<u8>, keys: &[egui::Key], build: impl Fn(&mut egui::Ui) -> Option<u8>) -> (Vec<u8>, Vec<(String, String)>) {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut reported = Vec::new();
    let mut nodes = Vec::new();
    let mut rects: Vec<(String, Rect)> = Vec::new();
    for frame in 0..6 {
        let mut events = Vec::new();
        if frame == 1
            && let Some(n) = click
        {
            let want = format!("star:t:{n}");
            let at = rects.iter().find(|(id, _)| *id == want).map(|(_, r)| r.center()).unwrap_or(Pos2::ZERO);
            events.push(egui::Event::PointerMoved(at));
            events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
            events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        }
        // one key per frame from the third on, so Tab has moved the focus before an arrow arrives
        if frame >= 2
            && let Some(k) = keys.get(frame - 2)
        {
            events.push(egui::Event::Key { key: *k, physical_key: Some(*k), pressed: true, repeat: false, modifiers: Default::default() });
        }
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
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

/// How many stars `Rating::show` paints filled (in the star colour) after the pointer has moved to
/// the centre of star `hover` (1 to 5), if given. A filled star is ten triangles of one colour.
fn filled_stars(rating: u8, hover: Option<u8>) -> usize {
    fn count(shape: &egui::Shape, colour: egui::Color32) -> usize {
        match shape {
            egui::Shape::Path(path) => usize::from(path.fill == colour),
            egui::Shape::Vec(shapes) => shapes.iter().map(|shape| count(shape, colour)).sum(),
            _ => 0,
        }
    }
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let colour = crate::theme::Tokens::get(&ctx).star;
    let mut at = None;
    let mut filled = 0;
    for frame in 0..4 {
        let mut events = Vec::new();
        if frame >= 1
            && let Some(at) = at
        {
            events.push(egui::Event::PointerMoved(at));
        }
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = row(rating).show(ui);
        });
        out.textures_delta.clear();
        let seen = crate::widgets::take_registry(&ctx);
        if let (Some(n), true) = (hover, frame == 0) {
            let want = format!("star:t:{n}");
            at = seen.iter().find(|(id, _)| *id == want).map(|(_, r)| r.center());
            assert!(at.is_some(), "star {n} is registered");
        }
        filled = out.shapes.iter().map(|clipped| count(&clipped.shape, colour)).sum::<usize>() / 10;
    }
    filled
}

#[test]
fn hovering_a_star_paints_the_rating_a_click_would_set() {
    // the row shows 2 filled stars; the pointer over star 4 shows 4, and over star 1 shows 1
    assert_eq!(filled_stars(2, None), 2);
    assert_eq!(filled_stars(2, Some(4)), 4);
    assert_eq!(filled_stars(2, Some(1)), 1, "hovering a lower star previews the lower rating");
}

#[test]
fn a_screen_reader_hears_every_star_with_its_state() {
    let (_, nodes) = drive(None, &[], |ui| row(2).show(ui));
    for n in 1..=5 {
        let name = format!("{n} ★");
        assert!(nodes.iter().any(|(role, label)| role == "Button" && *label == name), "{name}: {nodes:?}");
    }
}

#[test]
fn the_arrow_keys_step_the_rating_of_a_focused_star() {
    // Tab focuses the first star; the row shows 3, so → asks for 4 and ← for 2
    let (got, _) = drive(None, &[egui::Key::Tab, egui::Key::ArrowRight], |ui| row(3).show(ui));
    assert_eq!(got, vec![4], "{got:?}");
    let (got, _) = drive(None, &[egui::Key::Tab, egui::Key::ArrowLeft], |ui| row(3).show(ui));
    assert_eq!(got, vec![2], "{got:?}");
}

#[test]
fn arrow_keys_do_nothing_when_no_star_is_focused() {
    let (got, _) = drive(None, &[egui::Key::ArrowRight], |ui| row(3).show(ui));
    assert!(got.is_empty(), "{got:?}");
}

/// Feed the shown rating back each frame, with other focusable widgets on both sides.
fn step_with_neighbours(start: u8, keys: &[egui::Key]) -> Vec<u8> {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let rating = std::cell::Cell::new(start);
    let mut seen = Vec::new();
    // a key every frame, Tab then an arrow straight away
    for frame in 0..keys.len() + 3 {
        let mut events = Vec::new();
        if let Some(k) = frame.checked_sub(1).and_then(|f| keys.get(f)) {
            events.push(egui::Event::Key { key: *k, physical_key: Some(*k), pressed: true, repeat: false, modifiers: Default::default() });
        }
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 200.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            ui.horizontal(|ui| {
                let _ = ui.button("left neighbour");
                if let Some(r) = row(rating.get()).show(ui) {
                    rating.set(r);
                }
                let _ = ui.button("right neighbour");
            });
        });
        out.textures_delta.clear();
        seen.push(rating.get());
    }
    seen
}

#[test]
fn arrows_keep_stepping_between_other_focusable_widgets() {
    use egui::Key::*;
    // Tab, Tab: the left neighbour, then star 1
    let seen = step_with_neighbours(5, &[Tab, Tab, ArrowLeft, ArrowLeft, ArrowLeft, ArrowLeft, ArrowLeft]);
    assert_eq!(seen.last(), Some(&0), "{seen:?}");
    let seen = step_with_neighbours(0, &[Tab, Tab, ArrowRight, ArrowRight, ArrowRight, ArrowRight, ArrowRight, ArrowLeft]);
    assert_eq!(seen.last(), Some(&4), "{seen:?}");
}

#[test]
fn a_rating_row_is_one_tab_stop() {
    use egui::Key::Tab;
    // left neighbour, one stop for the five stars, right neighbour: the third Tab is already past the row
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut focused = Vec::new();
    for frame in 0..5 {
        let events = if frame >= 1 {
            vec![egui::Event::Key { key: Tab, physical_key: Some(Tab), pressed: true, repeat: false, modifiers: Default::default() }]
        } else {
            Vec::new()
        };
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 200.0))), events, ..Default::default() };
        let mut right = None;
        let mut out = ctx.run_ui(raw, |ui| {
            ui.horizontal(|ui| {
                let _ = ui.button("left neighbour");
                let _ = row(3).show(ui);
                right = Some(ui.button("right neighbour").id);
            });
        });
        out.textures_delta.clear();
        focused.push(right.is_some_and(|id| ctx.memory(|m| m.has_focus(id))));
    }
    // frames 1 to 3 press Tab: left neighbour, the row, right neighbour
    assert_eq!(focused, vec![false, false, false, true, false], "{focused:?}");
}

#[test]
fn a_rating_changed_from_outside_moves_the_focus_to_the_tab_stop() {
    use egui::Key::Tab;
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let rating = std::cell::Cell::new(2u8);
    let mut stars_focused = Vec::new();
    for frame in 0..5 {
        // frame 1: Tab focuses the row's stop (star 2); frame 2: the rating changes elsewhere to 5
        if frame == 2 {
            rating.set(5);
        }
        let events = if frame == 1 {
            vec![egui::Event::Key { key: Tab, physical_key: Some(Tab), pressed: true, repeat: false, modifiers: Default::default() }]
        } else {
            Vec::new()
        };
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 200.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = row(rating.get()).show(ui);
            stars_focused = (0..5).map(|i| ctx.memory(|m| m.has_focus(ui.scope_id().with(("rating-star", "t", i as u8))))).collect();
        });
        out.textures_delta.clear();
    }
    assert_eq!(stars_focused, vec![false, false, false, false, true], "{stars_focused:?}");
}
