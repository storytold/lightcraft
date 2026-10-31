//! What the interface writes can be read in every theme: text stands 4.5:1 against what it is
//! drawn on (3:1 at display size), on the panels, the grid, a photo being edited, Settings and the
//! dialogs. Left out: what is disabled, and text over a picture (its own scrim is another test).
//!
//! Before: the dim text of captions, counts and explanations was 4.20:1 on a panel in Charcoal and
//! 3.3 to 4.3:1 in the light themes, a link in the accent colour 2.56:1 in Charcoal, a caution
//! 3.09:1 in Silver, and anything `.weak()` under 4.1:1 in every theme.

use std::collections::BTreeMap;
use std::time::Duration;

use egui::{Color32, Rect};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::panels::library_problem::LibraryProblem;
use crate::panels::settings::TABS;
use crate::theme::{TextSize, ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo(kind: ThemeKind) -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1280.0, 900.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    run(&mut h, "view.theme", json!({"theme": kind.id()}));
    h
}

fn run(h: &mut Headless, command: &str, params: Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    rest(h);
}

fn set(h: &mut Headless, what: Value) {
    let r = h.request("ui.set", what, T);
    assert_eq!(r["ok"], true, "ui.set: {r}");
    rest(h);
}

/// Everything drawn and at rest, the pointer over nothing, and no toast: a toast (the one a label
/// gives, say) fades out by the clock, so how much of it is left depends on how fast the frames
/// run, and it is not what is being read.
fn rest(h: &mut Headless) {
    h.settle(T);
    h.app.ui.toast = None;
    h.request("ui.move", json!({"x": 2.0, "y": 2.0}), T);
    for _ in 0..3 {
        h.step();
    }
}

/// WCAG contrast of two opaque colours.
fn contrast(a: Color32, b: Color32) -> f32 {
    let lum = |c: Color32| {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    };
    let (a, b) = (lum(a), lum(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `over` drawn on `under`.
fn blend(over: Color32, under: Color32) -> Color32 {
    let a = f32::from(over.a()) / 255.0;
    // (premultiplied: the colour's channels already carry its alpha)
    let mix = |o: u8, u: u8| (f32::from(o) + f32::from(u) * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(mix(over.r(), under.r()), mix(over.g(), under.g()), mix(over.b(), under.b()))
}

/// The colour most of `rect` is, and the share of its pixels that are it (one pixel a point).
fn ground(image: &egui::ColorImage, rect: Rect) -> Option<(Color32, f32)> {
    let [w, h] = image.size;
    let (x0, x1) = (rect.left().floor().max(0.0) as usize, (rect.right().ceil().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().floor().max(0.0) as usize, (rect.bottom().ceil().max(0.0) as usize).min(h));
    let mut seen: BTreeMap<[u8; 4], usize> = BTreeMap::new();
    for y in y0..y1 {
        for x in x0..x1 {
            *seen.entry(image.pixels[y * w + x].to_array()).or_default() += 1;
        }
    }
    let all: usize = seen.values().sum();
    let (most, n) = seen.into_iter().max_by_key(|(_, n)| *n)?;
    Some((Color32::from_rgba_premultiplied(most[0], most[1], most[2], most[3]), n as f32 / all as f32))
}

/// Whether any pixel of `rect` is nearly `colour`, and nearer it than the ground it would be on
/// (text under a light header is not "inked" by the header's being nearly white).
fn inked(image: &egui::ColorImage, rect: Rect, colour: Color32, ground: Color32) -> bool {
    let [w, h] = image.size;
    let (x0, x1) = (rect.left().floor().max(0.0) as usize, (rect.right().ceil().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().floor().max(0.0) as usize, (rect.bottom().ceil().max(0.0) as usize).min(h));
    let far = |p: Color32, from: Color32| p.r().abs_diff(from.r()).max(p.g().abs_diff(from.g())).max(p.b().abs_diff(from.b()));
    (y0..y1).any(|y| (x0..x1).any(|x| image.pixels.get(y * w + x).is_some_and(|p| far(*p, colour) <= 20 && far(*p, colour) < far(*p, ground))))
}

/// The text on screen that cannot be read: what it says, its colour, what it is on and how far
/// apart the two stand.
fn unreadable(h: &Headless, t: &Tokens, within: Option<Rect>) -> Vec<String> {
    unreadable_in(&h.paint(), &h.view.shapes, t, within)
}

/// [`unreadable`] for a painted `image` and the `shapes` that made it.
fn unreadable_in(image: &egui::ColorImage, shapes: &[egui::epaint::ClippedShape], t: &Tokens, within: Option<Rect>) -> Vec<String> {
    let mut out = BTreeMap::new();
    // (`window_at`: how many texts were drawn before the dialog window's own frame: they lie under it, covered)
    fn texts(shape: &egui::Shape, clip: Rect, within: Option<Rect>, out: &mut Vec<(egui::epaint::TextShape, Rect)>, window_at: &mut Option<usize>) {
        match shape {
            egui::Shape::Text(text) => out.push((text.clone(), clip)),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| texts(s, clip, within, out, window_at)),
            egui::Shape::Rect(r) if within.is_some_and(|w| (r.rect.min - w.min).length() < 2.0 && (r.rect.max - w.max).length() < 2.0) => {
                *window_at = Some(out.len());
            }
            _ => {}
        }
    }
    let mut drawn = Vec::new();
    let mut window_at = None;
    shapes.iter().for_each(|s| texts(&s.shape, s.clip_rect, within, &mut drawn, &mut window_at));
    for (text, clip) in drawn.into_iter().skip(window_at.unwrap_or(0)) {
        let said = text.galley.job.text.trim();
        // (a disabled control is drawn fainter as a whole; nothing to read in an empty string)
        if said.is_empty() || text.opacity_factor < 1.0 {
            continue;
        }
        let rect = text.visual_bounding_rect().intersect(clip);
        // (what lies under a dialog's shade is not what is being read)
        if within.is_some_and(|within| !within.contains_rect(rect)) {
            continue;
        }
        // (a row cut by the edge of what scrolls: too little of it is left to say what it is on)
        let whole = text.visual_bounding_rect();
        if rect.width() < 2.0 || rect.height() < 2.0 || rect.area() < 0.6 * whole.area() {
            continue;
        }
        // (a little around the letters: a dash's own rectangle is mostly dash)
        let Some((on, share)) = ground(image, rect.expand(2.0)) else { continue };
        // (over a picture no colour is most of the rectangle)
        if share < 0.3 {
            continue;
        }
        let large = text.galley.job.sections.iter().all(|s| s.format.font_id.size >= TextSize::Display.points());
        for section in &text.galley.job.sections {
            let own = Some(section.format.color).filter(|c| *c != Color32::PLACEHOLDER);
            let colour = text.override_text_color.or(own).unwrap_or(text.fallback_color);
            // (a count of nothing is fainter on purpose: the rows that hold photos stand out)
            if colour == t.text_disabled || colour.a() == 0 || colour == t.text_dim.gamma_multiply(0.55) {
                continue;
            }
            let shown = blend(colour, on);
            // (text under a window is drawn and then covered: none of its colour is left)
            if !inked(image, rect, shown, on) {
                continue;
            }
            let stands = contrast(shown, on);
            if stands < if large { 3.0 } else { 4.5 } {
                let what: String = said.chars().take(28).collect();
                out.entry((shown.to_array(), on.to_array()))
                    .or_insert_with(|| format!("{what:?} {:?} on {:?}: {stands:.2}", shown.to_array(), on.to_array()));
            }
        }
    }
    out.into_values().collect()
}

fn assert_readable(h: &Headless, kind: ThemeKind, screen: &str, bad: &mut Vec<String>) {
    let t = Tokens::for_kind(kind);
    // (with a dialog open, the dialog is what is read)
    let dialog = h.app.widgets.iter().find(|(w, _)| w == "dialog:window").map(|(_, r)| *r);
    assert_eq!(dialog.is_some(), h.app.ui.dialog.is_some(), "{screen}: a dialog and its window go together");
    bad.extend(unreadable(h, &t, dialog).into_iter().map(|line| format!("{kind:?}, {screen}: {line}")));
}

fn report(bad: Vec<String>) {
    assert!(bad.is_empty(), "{} kinds of text cannot be read:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn the_library_can_be_read_in_every_theme() {
    let mut bad = Vec::new();
    for kind in ThemeKind::ALL {
        // Given the demo library in the grid in a theme, with the sidebar and each right panel
        let mut h = demo(kind);
        run(&mut h, "view.photoGrid", json!({}));
        run(&mut h, "library.select", json!({"ids": [1]}));
        for right in ["info", "keywords", "edit", "versions", "activity"] {
            // When the panel is shown
            set(&mut h, json!({"leftPanel": true, "right": right, "thumbSize": 200}));
            // Then its text stands out from what it is on
            assert_readable(&h, kind, &format!("grid with {right}"), &mut bad);
        }
        // And with a filter on (its count is written on a circle of the accent in the top bar),
        // in square cells, each with its name and format over it, one of them the chosen one
        run(&mut h, "library.filter", json!({"rating": 1}));
        run(&mut h, "view.squareGrid", json!({}));
        // (two of them with a colour label, which tints the cell: the chosen one and another)
        let shown: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|id| id.0).collect();
        let [chosen, other, ..] = shown[..] else { panic!("{kind:?}: two photos pass the filter") };
        run(&mut h, "photo.label", json!({"ids": [chosen], "label": "red"}));
        run(&mut h, "photo.label", json!({"ids": [other], "label": "yellow"}));
        run(&mut h, "library.select", json!({"ids": [chosen]}));
        for id in [chosen, other] {
            assert!(h.app.widgets.iter().any(|(w, _)| *w == format!("thumb:{id}")), "{kind:?}: photo {id} is on screen");
        }
        assert!(h.painted_text().iter().any(|t| t == "JPEG"), "{kind:?}: the cells say their format");
        assert_readable(&h, kind, "square grid, filtered", &mut bad);
    }
    report(bad);
}

#[test]
fn a_photo_being_edited_can_be_read_in_every_theme() {
    let mut bad = Vec::new();
    for kind in ThemeKind::ALL {
        // Given a photo opened by itself in a theme, the filmstrip under it
        let mut h = demo(kind);
        run(&mut h, "library.select", json!({"ids": [1]}));
        run(&mut h, "view.detail", json!({}));
        for right in ["edit", "info", "masking", "crop", "remove", "profiles"] {
            // When each tool panel is shown
            set(&mut h, json!({"leftPanel": false, "right": right, "openSections": ["light", "color", "effects", "detail"]}));
            // Then its text stands out from what it is on
            assert_readable(&h, kind, &format!("detail with {right}"), &mut bad);
        }
    }
    report(bad);
}

#[test]
fn the_names_over_the_filmstrip_can_be_read_on_any_cell_in_every_theme() {
    /// The filmstrip's cells that are wholly on screen, by photo.
    fn cells(h: &Headless) -> Vec<(u64, Rect)> {
        let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 900.0));
        let cell = |(w, r): &(String, Rect)| Some((w.strip_prefix("film:")?.parse().ok()?, *r));
        h.app.widgets.iter().filter_map(cell).filter(|(_, r)| screen.contains_rect(*r)).collect()
    }
    const LABELS: [&str; 5] = ["red", "yellow", "green", "blue", "purple"];
    let mut bad = Vec::new();
    for kind in ThemeKind::ALL {
        // Given a photo opened by itself in a theme, and on the filmstrip under it a photo with
        // each colour label, which tints its cell, and one with none
        let mut h = demo(kind);
        run(&mut h, "library.select", json!({"ids": [1]}));
        run(&mut h, "view.detail", json!({}));
        set(&mut h, json!({"leftPanel": false, "right": "info"}));
        let others: Vec<u64> = cells(&h).iter().map(|(id, _)| *id).filter(|id| *id != 1).take(LABELS.len() + 1).collect();
        let [labelled @ .., plain] = &others[..] else { panic!("{kind:?}: other photos are on the filmstrip") };
        assert_eq!(labelled.len(), LABELS.len(), "{kind:?}: a photo for each label and one more are on the filmstrip");
        for (id, label) in labelled.iter().zip(LABELS) {
            run(&mut h, "photo.label", json!({"ids": [id], "label": label}));
        }
        assert!(h.painted_text().iter().any(|t| t == "JPEG"), "{kind:?}: the cells say their format");
        // Then at rest the names stand out from their cells
        assert_readable(&h, kind, "filmstrip at rest", &mut bad);
        for (id, label) in labelled.iter().zip(LABELS).chain([(plain, "no label")]) {
            // When the pointer is on a cell that is not the chosen one (its fill is see-through)
            let cell = cells(&h).iter().find(|(c, _)| c == id).map(|(_, r)| *r);
            let Some(cell) = cell else { panic!("{kind:?}: photo {id} is on the filmstrip") };
            h.request("ui.move", json!({"x": cell.center().x, "y": cell.top() + 10.0}), T);
            for _ in 0..3 {
                h.step();
            }
            // Then its name and format stand out from the cell as it is painted
            assert_readable(&h, kind, &format!("filmstrip, pointer on a cell with {label}"), &mut bad);
        }
        for (id, label) in labelled.iter().zip(LABELS) {
            // And when a labelled photo is the chosen one
            run(&mut h, "library.select", json!({"ids": [id]}));
            assert_readable(&h, kind, &format!("filmstrip, the photo with {label} chosen"), &mut bad);
        }
    }
    report(bad);
}

#[test]
fn settings_and_the_dialogs_can_be_read_in_every_theme() {
    let mut bad = Vec::new();
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        // Given each tab of Settings in a theme
        for (tab, _) in TABS {
            run(&mut h, "app.settings", json!({"tab": tab}));
            assert_readable(&h, kind, &format!("Settings ▸ {tab}"), &mut bad);
        }
        h.request("ui.dialog.cancel", json!({}), T);
        // And the dialogs a person meets most
        for command in ["dialog.export", "app.shortcuts", "app.about"] {
            run(&mut h, command, json!({}));
            assert_readable(&h, kind, command, &mut bad);
            h.request("ui.dialog.cancel", json!({}), T);
            h.step();
        }
    }
    report(bad);
}

#[test]
fn the_colours_that_say_something_stand_out_from_every_surface_in_every_theme() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        // Given the surfaces text is written on at rest
        for on in [t.chrome, t.canvas, t.grid_bg, t.cell, t.inset, t.field, t.button] {
            // When dim text, a link, a caution or an error is written on one
            for (what, colour) in [("dim", t.text_dim), ("link", t.text_link()), ("caution", t.text_caution()), ("error", t.text_error())] {
                // Then it stands 4.5:1
                assert!(contrast(colour, on) >= 4.5, "{kind:?}: {what} {colour:?} on {on:?} is {:.2}", contrast(colour, on));
            }
        }
        // And dim text is still dimmer than a label, which is dimmer than what is written
        let (dim, label, text) = (contrast(t.text_dim, t.chrome), contrast(t.text_label, t.chrome), contrast(t.text, t.chrome));
        assert!(dim + 1.0 < label && label < text, "{kind:?}: dim {dim:.2}, label {label:.2}, text {text:.2}");
        // And what is chosen keeps its label readable on the accent mixed in behind it
        assert!(contrast(t.text_label, t.selection_fill()) >= 4.5, "{kind:?}: a chosen chip");
        assert_ne!(t.selection_fill(), t.chrome, "{kind:?}: what is chosen is marked");
        // And a count or a mark on a filled circle is in the ink that stands out from the fill
        for fill in [t.accent, t.reject, t.caution] {
            let ink = t.ink_on(fill);
            assert!(contrast(ink, fill) >= 4.5, "{kind:?}: {ink:?} on {fill:?} is {:.2}", contrast(ink, fill));
        }
    }
    // And black or white is chosen by which is farther, also where neither is far
    let t = Tokens::for_kind(ThemeKind::Charcoal);
    assert_eq!(t.ink_on(Color32::BLACK), Color32::WHITE);
    assert_eq!(t.ink_on(Color32::WHITE), Color32::BLACK);
    assert_eq!(t.ink_on(Color32::from_gray(118)), Color32::BLACK);
}

#[test]
fn the_warnings_of_the_slideshow_overlays_can_be_read_in_every_theme() {
    let mut bad = Vec::new();
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        // Given a text overlay with a tag that is no token, which the overlays page warns about
        run(&mut h, "slideshow.overlays", json!({"addText": {"text": "{notatoken}"}}));
        assert!(!h.app.session.slide_overlays.warnings().is_empty(), "{kind:?}: the overlay is warned about");
        // When Settings ▸ Slideshow shows it
        run(&mut h, "app.settings", json!({"tab": "slideshow"}));
        // (it ends the overlays section, below the fold: the page is scrolled to it)
        // (with the pointer over the page, not over the pinned preview or a list that scrolls on its own)
        h.request("ui.hoverWidget", json!({"id": "settings:pageRoom"}), T);
        for _ in 0..40 {
            if h.painted_text().iter().any(|t| t.contains("isn't a token")) {
                break;
            }
            h.request("ui.scroll", json!({"dx": 0.0, "dy": -150.0}), T);
            h.settle(T);
        }
        // (and a little further, so that the line is whole and not cut by the edge of what scrolls)
        h.request("ui.scroll", json!({"dx": 0.0, "dy": -120.0}), T);
        h.settle(T);
        assert!(h.painted_text().iter().any(|t| t.contains("isn't a token")), "{kind:?}: the warning is on the page");
        // Then it stands 4.5:1 from the panel
        assert_readable(&h, kind, "Settings ▸ slideshow, an overlay warning", &mut bad);
    }
    report(bad);
}

#[test]
fn the_unknown_tag_warning_of_a_template_can_be_read_in_every_theme() {
    let mut bad = Vec::new();
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        // Given photos chosen, and the Rename dialog open with a template that has a tag that is no token
        run(&mut h, "library.select", json!({"ids": [1, 2]}));
        run(&mut h, "dialog.rename", json!({"template": "{notatoken}-{seq:3}"}));
        assert!(h.painted_text().iter().any(|t| t.contains("as typed")), "{kind:?}: the template is warned about");
        // Then the warning stands 4.5:1 from the dialog
        assert_readable(&h, kind, "Rename, an unknown tag", &mut bad);
    }
    report(bad);
}

#[test]
fn the_temporary_session_banner_can_be_read_in_every_theme() {
    let mut bad = Vec::new();
    for kind in ThemeKind::ALL {
        // Given a session that is temporary (the library did not open), in a theme
        let mut h = demo(kind);
        h.app.library_problem = Some(LibraryProblem { dismissed: true, ..LibraryProblem::new("/no/such/library", "a test error") });
        rest(&mut h);
        assert!(h.app.widgets.iter().any(|(w, _)| w == "indicator:temporarySession"), "{kind:?}: the banner is shown");
        // Then its words and its button stand 4.5:1 from the caution colour it is filled with
        assert_readable(&h, kind, "the temporary-session banner", &mut bad);
    }
    report(bad);
}

/// Given text drawn before a dialog's own frame (it lies under the dialog, covered) and, where it lies,
/// pixels of the dialog (a preview's bars) in the very colour the text was drawn in, when the screen is read,
/// then that text is left out; and the same text drawn after the frame is still read and still fails.
#[test]
fn text_under_the_dialogs_own_frame_is_not_read() {
    use egui::{pos2, vec2};
    let h = demo(ThemeKind::Charcoal);
    let t = Tokens::for_kind(ThemeKind::Charcoal);
    let (ground, ink) = (Color32::from_gray(63), Color32::from_gray(150));
    assert!(contrast(ink, ground) < 4.5, "the ink is too faint to read on the ground");
    let galley = h.view.ctx.layer_painter(egui::LayerId::background()).layout_no_wrap("JPEG".into(), egui::FontId::proportional(12.0), ink);
    let window = Rect::from_min_size(pos2(0.0, 0.0), vec2(120.0, 60.0));
    let mut image = egui::ColorImage::new([120, 60], vec![ground; 120 * 60]);
    // (a bar of the dialog, in the ink's colour, where the text lies)
    for x in 20..60 {
        image.pixels[25 * 120 + x] = ink;
    }
    let clipped = |shape| egui::epaint::ClippedShape { clip_rect: window, shape, transform_after_tessellation: Default::default() };
    let frame = clipped(egui::Shape::rect_filled(window, 0.0, ground));
    let text = clipped(egui::Shape::galley(pos2(20.0, 20.0), galley, ink));
    // When the text was drawn before the frame, then it is covered, and not read
    let covered = unreadable_in(&image, &[text.clone(), frame.clone()], &t, Some(window));
    assert!(covered.is_empty(), "text under the frame is read: {covered:?}");
    // And when it is drawn after the frame, it is on the dialog, read and found too faint
    let shown = unreadable_in(&image, &[frame, text], &t, Some(window));
    assert_eq!(shown.len(), 1, "text on the dialog is read: {shown:?}");
}
