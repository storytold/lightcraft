//! Grid badges under Settings ▸ Interface ▸ Grid: Auto keeps a mostly rated library clean (the
//! stars and the edited mark show on hover and selection), while the decision marks (flag, colour
//! label) stay visible; Always shows everything, Never nothing.

use std::sync::Arc;
use std::time::Duration;

use lightcraft_catalog::{ColorLabel, Flag, Op, Photo, PhotoId, Source};
use lightcraft_engine::Session;
use serde_json::json;

use crate::headless::Headless;
use crate::state::{GridBadges, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(60);

const RATED: PhotoId = PhotoId(1);
const PICKED: PhotoId = PhotoId(2);
const REJECTED: PhotoId = PhotoId(3);
const EDITED: PhotoId = PhotoId(4);
const PLAIN: PhotoId = PhotoId(5);
const UNRATED_PICK: PhotoId = PhotoId(6);

fn library() -> Session {
    let mut s = Session::new();
    for i in 1..=6u64 {
        let name = format!("IMG_{i:03}.jpg");
        let mut p = Photo::new(
            PhotoId(i),
            Source::File { path: format!("/lightcraft-badges-test/{name}") },
            &name,
            "JPEG",
            6000,
            4000,
            "2026-01-01T00:00:00",
        );
        match PhotoId(i) {
            RATED => p.rating = 4,
            PICKED => {
                p.rating = 5;
                p.flag = Flag::Pick;
                p.label = Some(ColorLabel::Red);
            }
            REJECTED => {
                p.flag = Flag::Reject;
                p.label = Some(ColorLabel::Blue);
            }
            EDITED => Arc::make_mut(&mut p.develop).light.exposure = 1.0,
            UNRATED_PICK => p.flag = Flag::Pick,
            _ => {}
        }
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s
}

/// The grid with `badges`, the plain photo selected and the pointer away from every tile.
fn grid(badges: GridBadges) -> Headless {
    let app = LightcraftApp::new(library(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = ViewMode::PhotoGrid;
    h.app.ui.settings.grid_badges = badges;
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [PLAIN.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    move_to(&mut h, egui::pos2(1.0, 1.0));
    h
}

fn move_to(h: &mut Headless, at: egui::Pos2) {
    h.request("ui.move", json!({"x": at.x, "y": at.y}), T);
    h.settle(SETTLE);
    h.step();
}

fn hover(h: &mut Headless, id: PhotoId) {
    let r = h.request("ui.hoverWidget", json!({"id": format!("thumb:{}", id.0)}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h.step();
}

/// Which marks are drawn on the photo's tile, in a fixed order.
fn marks(h: &Headless, id: PhotoId) -> Vec<&'static str> {
    ["stars", "flag", "label", "edited"].into_iter().filter(|m| h.app.widgets.iter().any(|(w, _)| *w == format!("badge:{m}:{}", id.0))).collect()
}

#[test]
fn auto_hides_the_stars_of_a_photo_that_is_neither_hovered_nor_selected() {
    let h = grid(GridBadges::Auto);
    assert_eq!(marks(&h, RATED), Vec::<&str>::new());
}

#[test]
fn auto_shows_the_stars_while_the_photo_is_hovered_or_selected() {
    let mut h = grid(GridBadges::Auto);
    hover(&mut h, RATED);
    assert_eq!(marks(&h, RATED), ["stars"], "hovered");
    move_to(&mut h, egui::pos2(1.0, 1.0));
    assert_eq!(marks(&h, RATED), Vec::<&str>::new(), "the pointer left");
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [RATED.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(marks(&h, RATED), ["stars"], "selected");
}

#[test]
fn auto_always_shows_the_flag_and_the_colour_label() {
    let mut h = grid(GridBadges::Auto);
    assert_eq!(marks(&h, PICKED), ["flag", "label"], "the stars of a picked photo stay hidden");
    assert_eq!(marks(&h, REJECTED), ["flag", "label"]);
    hover(&mut h, PICKED);
    assert_eq!(marks(&h, PICKED), ["stars", "flag", "label"], "hover adds the stars to the marks");
}

#[test]
fn always_shows_everything_and_never_shows_nothing() {
    let h = grid(GridBadges::Always);
    assert_eq!(marks(&h, RATED), ["stars"]);
    assert_eq!(marks(&h, PICKED), ["stars", "flag", "label"]);
    assert_eq!(marks(&h, EDITED), ["edited"]);
    // (the setting is about ratings and flags: the justified grid has no surround to tint with a
    // colour label, so the label's dot stays, #2164)
    let h = grid(GridBadges::Never);
    for id in [RATED, EDITED] {
        assert_eq!(marks(&h, id), Vec::<&str>::new(), "photo {}", id.0);
    }
    for id in [PICKED, REJECTED] {
        assert_eq!(marks(&h, id), ["label"], "photo {}", id.0);
    }
}

#[test]
fn auto_shows_the_edited_mark_with_the_stars() {
    let mut h = grid(GridBadges::Auto);
    assert_eq!(marks(&h, EDITED), Vec::<&str>::new());
    hover(&mut h, EDITED);
    assert_eq!(marks(&h, EDITED), ["edited"]);
}

fn rect(h: &Headless, id: String) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| *w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

#[test]
fn auto_puts_the_flag_at_the_left_when_the_stars_are_hidden() {
    let mut h = grid(GridBadges::Auto);
    let cell = rect(&h, format!("thumb:{}", PICKED.0));
    let flag = rect(&h, format!("badge:flag:{}", PICKED.0));
    assert!(flag.left() - cell.left() < 12.0, "no gap for stars that aren't drawn: flag {flag:?} in {cell:?}");
    hover(&mut h, PICKED);
    let flag = rect(&h, format!("badge:flag:{}", PICKED.0));
    let stars = rect(&h, format!("badge:stars:{}", PICKED.0));
    assert!(flag.left() >= stars.right(), "the flag follows the stars: {flag:?} after {stars:?}");
}

#[test]
fn an_unrated_photos_flag_does_not_move_on_hover() {
    let mut h = grid(GridBadges::Auto);
    let before = rect(&h, format!("badge:flag:{}", UNRATED_PICK.0));
    hover(&mut h, UNRATED_PICK);
    assert_eq!(rect(&h, format!("badge:flag:{}", UNRATED_PICK.0)), before);
}
