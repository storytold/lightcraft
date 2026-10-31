//! What is written or marked over a picture is in one light ink on a dark shade of its own, in
//! every theme: the ratings and flags on a thumbnail, the toast, the name of a side in Before and
//! After. A picture has no theme, so the colours of a theme's text do not belong on it.
//!
//! Before: in the light themes a thumbnail's stars were dark grey (the theme's star colour) on the
//! photo or on a dark bar, and the toast wrote the theme's near-black text on a black shade; in
//! the dark themes a rated thumbnail's stars lay on the bare photo and went out on a bright one.

use std::time::Duration;

use egui::{Color32, Rect};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::theme::{ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo(kind: ThemeKind) -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1600.0, 1000.0], 1.0);
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

/// Everything drawn and at rest, the pointer over nothing.
fn rest(h: &mut Headless) {
    h.settle(T);
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

fn pixels(image: &egui::ColorImage, rect: Rect) -> Vec<Color32> {
    let [w, h] = image.size;
    let (x0, x1) = (rect.left().floor().max(0.0) as usize, (rect.right().ceil().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().floor().max(0.0) as usize, (rect.bottom().ceil().max(0.0) as usize).min(h));
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| image.pixels[y * w + x]).collect()
}

/// The colours a drawn shape is filled and outlined in (text aside).
fn colours(shape: &egui::Shape) -> Vec<Color32> {
    match shape {
        egui::Shape::Path(path) => match &path.stroke.color {
            egui::epaint::ColorMode::Solid(stroke) => vec![path.fill, *stroke],
            egui::epaint::ColorMode::UV(_) => vec![path.fill],
        },
        egui::Shape::LineSegment { stroke, .. } => vec![stroke.color],
        egui::Shape::Circle(circle) => vec![circle.fill, circle.stroke.color],
        egui::Shape::Ellipse(ellipse) => vec![ellipse.fill, ellipse.stroke.color],
        egui::Shape::Rect(rect) => vec![rect.fill, rect.stroke.color],
        _ => Vec::new(),
    }
}

/// A rated photo's cell that is on screen and not the chosen one: `prefix` is `thumb` or `film`.
fn rated_cell(h: &Headless, prefix: &str, chosen: u64) -> (Rect, u8) {
    let screen = Rect::from_min_size(egui::Pos2::ZERO, h.size);
    h.app
        .widgets
        .iter()
        .filter_map(|(id, r)| Some((id.strip_prefix(prefix)?.strip_prefix(':')?.parse::<u64>().ok()?, *r)))
        .filter(|(id, r)| *id != chosen && screen.contains_rect(*r))
        .find_map(|(id, r)| {
            let photo = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id))?;
            (photo.rating >= 2 && photo.label.is_none()).then_some((r, photo.rating))
        })
        .unwrap_or_else(|| panic!("no rated {prefix} on screen"))
}

/// In the strip along the bottom of `cell`: marks are drawn in the ink for pictures, each on a
/// rectangle of the shade, and the shade is under them in the picture that results.
fn assert_marked(h: &Headless, cell: Rect, rating: u8, what: &str) {
    let (ink, shade) = (Tokens::on_photo(), Tokens::photo_shade());
    let strip = Rect::from_min_max(egui::pos2(cell.left(), cell.bottom() - 30.0), cell.right_bottom());
    let shapes = h.view.painted_shapes();
    let within: Vec<&egui::Shape> = shapes.iter().filter(|s| strip.contains_rect(s.visual_bounding_rect())).collect();
    // (a star is one shape or a few; there are at least as many in the ink as stars)
    let marks: Vec<Rect> =
        within.iter().filter(|s| !matches!(s, egui::Shape::Rect(_)) && colours(s).contains(&ink)).map(|s| s.visual_bounding_rect()).collect();
    assert!(marks.len() >= usize::from(rating), "{what}: {} marks in the ink {ink:?} for {rating} stars", marks.len());
    // (the shade may be wider than the cell: a photo cut to its cell brings its strip along)
    let shades: Vec<Rect> =
        shapes.iter().filter(|s| matches!(s, egui::Shape::Rect(r) if r.fill == shade)).map(|s| s.visual_bounding_rect()).collect();
    for mark in &marks {
        assert!(shades.iter().any(|shade| shade.expand(1.0).contains_rect(*mark)), "{what}: the mark at {mark:?} has no shade under it ({shades:?})");
    }
    // And in the picture: the shade beside the marks is dark enough for the ink to stand 3:1
    let image = h.paint();
    let [w, height] = image.size;
    let mut stands = Vec::new();
    for shade in shades.iter().filter(|shade| marks.iter().any(|mark| shade.expand(1.0).contains_rect(*mark))) {
        let inside = shade.shrink(1.0).intersect(cell);
        for y in inside.top().ceil().max(0.0) as usize..(inside.bottom().floor().max(0.0) as usize).min(height) {
            for x in inside.left().ceil().max(0.0) as usize..(inside.right().floor().max(0.0) as usize).min(w) {
                let here = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                if !marks.iter().any(|mark| mark.expand(1.5).contains(here)) {
                    stands.push(contrast(ink, image.pixels[y * w + x]));
                }
            }
        }
    }
    stands.sort_by(f32::total_cmp);
    // (a tenth in: a mark's soft edge may reach a pixel past its rectangle)
    let ground = stands.get(stands.len() / 10).copied().unwrap_or(1.0);
    assert!(stands.len() >= 12 && ground >= 3.0, "{what}: the ink stands {ground:.2} against its shade ({} pixels of it)", stands.len());
}

/// The first text shape that says `part`: its colour and where it is.
fn text(h: &Headless, part: &str) -> (Color32, Rect) {
    h.view
        .painted_shapes()
        .iter()
        .find_map(|shape| match shape {
            egui::Shape::Text(text) if text.galley.job.text.contains(part) => {
                let own = text.galley.job.sections.first().map(|s| s.format.color).filter(|c| *c != Color32::PLACEHOLDER);
                Some((text.override_text_color.or(own).unwrap_or(text.fallback_color), text.visual_bounding_rect()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text with {part:?} is drawn"))
}

/// The colour most of `rect` is.
fn ground(image: &egui::ColorImage, rect: Rect) -> Color32 {
    let mut seen = std::collections::BTreeMap::new();
    for p in pixels(image, rect) {
        *seen.entry(p.to_array()).or_insert(0usize) += 1;
    }
    let most = seen.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c).unwrap_or([0; 4]);
    Color32::from_rgb(most[0], most[1], most[2])
}

#[test]
fn the_marks_on_a_thumbnail_stand_out_from_the_photo_in_every_theme() {
    for kind in ThemeKind::ALL {
        // Given the grid in a theme, and a rated photo that is neither chosen nor under the pointer
        let mut h = demo(kind);
        run(&mut h, "view.photoGrid", json!({}));
        run(&mut h, "library.select", json!({"ids": [2]}));
        let r = h.request("ui.set", json!({"leftPanel": true, "right": "info", "thumbSize": 200}), T);
        assert_eq!(r["ok"], true, "{r}");
        rest(&mut h);
        // When it is drawn
        let (thumb, rating) = rated_cell(&h, "thumb", 2);
        // Then its stars are in the ink for pictures, on a shade that lets them stand 3:1
        assert_marked(&h, thumb, rating, &format!("{kind:?}: the stars on a thumbnail"));
    }
}

#[test]
fn the_marks_on_a_filmstrip_cell_stand_out_in_every_theme() {
    for kind in ThemeKind::ALL {
        // Given a photo opened by itself in a theme, the filmstrip under it
        let mut h = demo(kind);
        run(&mut h, "library.select", json!({"ids": [2]}));
        run(&mut h, "view.detail", json!({}));
        // When a rated photo's cell is drawn
        let (cell, rating) = rated_cell(&h, "film", 2);
        // Then its stars are in the ink for pictures on their shade
        assert_marked(&h, cell, rating, &format!("{kind:?}: the stars on a filmstrip cell"));
    }
}

#[test]
fn a_toast_can_be_read_in_every_theme() {
    for kind in ThemeKind::ALL {
        // Given a photo opened by itself in a theme
        let mut h = demo(kind);
        run(&mut h, "library.select", json!({"ids": [1]}));
        run(&mut h, "view.detail", json!({}));
        // When something is said in a toast
        let now = h.app.tick_now();
        h.app.ui.toast = Some(("Rated 3 stars".into(), now + 60.0, None));
        h.step();
        h.step();
        // Then it is written in the ink for pictures and stands 4.5:1 against its shade
        let (colour, at) = text(&h, "Rated 3 stars");
        assert_eq!(colour, Tokens::on_photo(), "{kind:?}");
        let on = ground(&h.paint(), at.expand(6.0));
        assert!(contrast(colour, on) >= 4.5, "{kind:?}: {colour:?} on {on:?} is {:.2}", contrast(colour, on));
    }
}

#[test]
fn before_is_named_on_a_shade_of_its_own_in_every_theme() {
    for kind in ThemeKind::ALL {
        // Given a photo opened by itself in a theme
        let mut h = demo(kind);
        run(&mut h, "library.select", json!({"ids": [1]}));
        run(&mut h, "view.detail", json!({}));
        // When the photo is shown as it was before its edits
        run(&mut h, "view.showOriginal", json!({}));
        // Then "Before" is written over the picture in the ink for pictures, on a shade
        let (colour, at) = text(&h, "Before");
        assert_eq!(colour, Tokens::on_photo(), "{kind:?}");
        let on = ground(&h.paint(), at.expand(3.0));
        assert!(contrast(colour, on) >= 4.5, "{kind:?}: {colour:?} on {on:?} is {:.2}", contrast(colour, on));
    }
}

#[test]
fn the_ink_for_pictures_stands_out_from_its_shade_over_any_picture() {
    // Given the shade laid over the brightest and the darkest a picture can be
    let (ink, shade) = (Tokens::on_photo(), Tokens::photo_shade());
    let a = f32::from(shade.a()) / 255.0;
    for under in [Color32::WHITE, Color32::BLACK, Color32::from_rgb(255, 230, 40)] {
        let mix = |u: u8| (f32::from(u) * (1.0 - a)).round() as u8;
        let on = Color32::from_rgb(mix(under.r()), mix(under.g()), mix(under.b()));
        // Then the ink stands 4.5:1 on it
        assert!(contrast(ink, on) >= 4.5, "{ink:?} over {under:?}: {:.2}", contrast(ink, on));
    }
}

#[test]
fn a_rejection_over_a_picture_is_a_red_that_stands_out_from_its_shade() {
    // Given the red a rejected photo's flag is drawn in over a picture
    let (red, shade) = (Tokens::on_photo_reject(), Tokens::photo_shade());
    let a = f32::from(shade.a()) / 255.0;
    // When the shade lies over the brightest a picture can be, and over the darkest
    for under in [Color32::WHITE, Color32::BLACK] {
        let mix = |u: u8| (f32::from(u) * (1.0 - a)).round() as u8;
        let on = Color32::from_rgb(mix(under.r()), mix(under.g()), mix(under.b()));
        // Then the flag stands 3:1, as a mark must
        assert!(contrast(red, on) >= 3.0, "{red:?} over {under:?}: {:.2}", contrast(red, on));
    }
    // And it is drawn in that red in every theme, on a shade (the theme's own red is for panels)
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        run(&mut h, "view.photoGrid", json!({}));
        let rejected = h
            .app
            .session
            .catalog
            .photos()
            .find(|p| p.flag == lightcraft_catalog::Flag::Reject)
            .map(|p| p.id.0)
            .expect("the demo has a rejected photo");
        run(&mut h, "library.select", json!({"ids": [rejected]}));
        run(&mut h, "view.detail", json!({}));
        let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("film:{rejected}")).map(|(_, r)| *r).expect("its filmstrip cell");
        let shapes = h.view.painted_shapes();
        let flags: Vec<Rect> = shapes
            .iter()
            .filter(|s| colours(s).contains(&red) && cell.intersects(s.visual_bounding_rect()))
            .map(|s| s.visual_bounding_rect())
            .collect();
        assert!(!flags.is_empty(), "{kind:?}: the flag is in {red:?}");
        let shades: Vec<Rect> =
            shapes.iter().filter(|s| matches!(s, egui::Shape::Rect(r) if r.fill == shade)).map(|s| s.visual_bounding_rect()).collect();
        for flag in flags {
            assert!(shades.iter().any(|shade| shade.expand(1.0).contains_rect(flag)), "{kind:?}: the flag at {flag:?} has no shade ({shades:?})");
        }
    }
}

#[test]
fn the_marks_of_a_narrow_thumbnail_stay_on_the_thumbnail() {
    // Given the grid with thumbnails so small that a portrait one is narrower than its marks
    let mut h = demo(ThemeKind::Charcoal);
    run(&mut h, "view.photoGrid", json!({}));
    let r = h.request("ui.set", json!({"leftPanel": true, "right": "info", "thumbSize": 60}), T);
    assert_eq!(r["ok"], true, "{r}");
    rest(&mut h);
    let cells: Vec<Rect> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("thumb:")).map(|(_, r)| *r).collect();
    assert!(cells.len() >= 8 && cells.iter().any(|c| c.width() < 60.0), "narrow thumbnails are on screen");
    // When their marks are drawn
    fn marks(shape: &egui::Shape, clip: Rect, out: &mut Vec<(Rect, Rect)>) {
        match shape {
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| marks(s, clip, out)),
            other if colours(other).contains(&Tokens::on_photo()) && !matches!(other, egui::Shape::Rect(_)) => {
                out.push((other.visual_bounding_rect(), clip))
            }
            _ => {}
        }
    }
    let mut drawn = Vec::new();
    h.view.shapes.iter().for_each(|s| marks(&s.shape, s.clip_rect, &mut drawn));
    assert!(drawn.len() >= 8, "marks are drawn ({})", drawn.len());
    // Then what shows of each lies on its own thumbnail: none reaches over the gap to the next
    for (mark, clip) in drawn {
        let shown = mark.intersect(clip);
        if shown.width() <= 0.0 || shown.height() <= 0.0 {
            continue;
        }
        assert!(cells.iter().any(|cell| cell.expand(0.5).contains_rect(shown)), "the mark at {shown:?} is on no thumbnail");
    }
}

#[test]
fn a_photo_chosen_or_under_the_pointer_keeps_the_band_along_its_lower_edge() {
    for kind in ThemeKind::ALL {
        // Given the grid in a theme, and a photo with no rating, flag, label or edit
        let mut h = demo(kind);
        run(&mut h, "view.photoGrid", json!({}));
        let r = h.request("ui.set", json!({"leftPanel": true, "right": "info", "thumbSize": 200}), T);
        assert_eq!(r["ok"], true, "{r}");
        rest(&mut h);
        let screen = Rect::from_min_size(egui::Pos2::ZERO, h.size);
        let plain = h
            .app
            .widgets
            .iter()
            .filter_map(|(id, r)| Some((id.strip_prefix("thumb:")?.parse::<u64>().ok()?, *r)))
            .filter(|(_, r)| screen.contains_rect(*r))
            .find(|(id, _)| {
                h.app
                    .session
                    .catalog
                    .photo(lightcraft_catalog::PhotoId(*id))
                    .is_some_and(|p| p.rating == 0 && p.flag == lightcraft_catalog::Flag::None && p.label.is_none() && !p.is_edited())
            })
            .map(|(id, _)| id)
            .expect("a plain photo is on screen");
        let band = |h: &Headless| {
            let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("thumb:{plain}")).map(|(_, r)| *r).expect("its thumbnail");
            let strip = Rect::from_min_max(egui::pos2(cell.left(), cell.bottom() - 24.0), cell.right_bottom());
            h.view.painted_shapes().iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == Color32::from_black_alpha(120) && (r.rect.width() - strip.width()).abs() < 1.0 && strip.expand(1.0).contains_rect(r.rect)))
        };
        // When it is not chosen, it has no band; when it is chosen, it has one the width of the
        // photo: in the light themes its outline alone is 1.1 to 2.1:1 against the grid
        run(&mut h, "library.select", json!({"ids": []}));
        assert!(!band(&h), "{kind:?}: a photo that is not chosen has no band");
        // When the pointer is on it, it has the band (nothing else says the pointer is there)
        let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("thumb:{plain}")).map(|(_, r)| *r).expect("its thumbnail");
        h.request("ui.move", json!({"x": cell.center().x, "y": cell.center().y}), T);
        for _ in 0..3 {
            h.step();
        }
        assert!(band(&h), "{kind:?}: the photo under the pointer has its band");
        rest(&mut h);
        assert!(!band(&h), "{kind:?}: the band goes when the pointer does");
        run(&mut h, "library.select", json!({"ids": [plain]}));
        assert!(band(&h), "{kind:?}: the chosen photo has its band");
    }
}

#[test]
fn a_rejected_photos_flag_is_not_dimmed_with_the_photo() {
    for kind in ThemeKind::ALL {
        // Given the grid in a theme, with a rejected photo (which is drawn dimmed)
        let mut h = demo(kind);
        run(&mut h, "view.photoGrid", json!({}));
        let r = h.request("ui.set", json!({"leftPanel": false, "right": "none", "thumbSize": 110}), T);
        assert_eq!(r["ok"], true, "{r}");
        run(&mut h, "library.select", json!({"ids": []}));
        let screen = Rect::from_min_size(egui::Pos2::ZERO, h.size);
        let cell = h
            .app
            .widgets
            .iter()
            .filter_map(|(id, r)| Some((id.strip_prefix("thumb:")?.parse::<u64>().ok()?, *r)))
            .filter(|(_, r)| screen.contains_rect(*r))
            .find(|(id, _)| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(*id)).is_some_and(|p| p.flag == lightcraft_catalog::Flag::Reject))
            .map(|(_, r)| r)
            .expect("a rejected photo is on screen");
        // When it is painted
        let image = h.paint();
        let strip = Rect::from_min_max(egui::pos2(cell.left(), cell.bottom() - 24.0), cell.right_bottom());
        // Then its flag is in the red for pictures as that red is: the dimming lies under the
        // marks, not over them
        let red = Tokens::on_photo_reject();
        let all = pixels(&image, strip);
        let flag = all.iter().filter(|p| p.r().abs_diff(red.r()).max(p.g().abs_diff(red.g())).max(p.b().abs_diff(red.b())) <= 24).count();
        assert!(flag >= 6, "{kind:?}: {flag} pixels of the flag's red {red:?} along the lower edge");
    }
}
