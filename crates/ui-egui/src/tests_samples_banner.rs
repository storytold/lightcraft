//! The samples banner can't be missed: a band of its own colour under the top bar, words at the
//! body size that take a second line before they are shortened, and read out by screen readers
//! when they change (as toasts are).

use egui::{Rect, pos2, vec2};
use serde_json::json;

use crate::headless::Headless;
use crate::panels::samples;
use crate::state::ViewMode;
use crate::theme::{ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

fn library(tag: &str) -> lightcraft_engine::Session {
    let dir = std::env::temp_dir().join(format!("lc-ui-banner-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    s
}

/// The samples landed in a new library, in a window `width` wide.
fn landed(tag: &str, width: f32) -> Headless {
    let mut a = LightcraftApp::new(library(tag), Services::default());
    a.ui.view = ViewMode::PhotoGrid;
    let mut h = Headless::new(a, [width, 900.0], 1.0);
    // (the library is taken in over the first frames: samples land in it after)
    h.step();
    h.step();
    h.app.run("library.addSamples", json!({})).unwrap();
    h.step();
    h.step();
    h
}

fn banner(h: &Headless) -> Rect {
    h.app.widgets.iter().find(|(w, _)| w == "banner:samples").map(|(_, r)| *r).unwrap()
}

#[test]
fn the_band_stands_apart_and_its_words_read_on_it() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        let fill = samples::fill(&t);
        // the band itself is a tint; its edge in the accent sets it off from the top bar and the
        // window below (at least 2:1 both ways: the accent is a mid blue, short of 3:1 on the dark
        // themes' tint, and the banner is a note, not a control that must be found)
        assert_ne!(fill, t.chrome, "{kind:?}: the same as the top bar");
        let edge = samples::edge(&t);
        for (against, what) in [(fill, "band"), (t.chrome, "chrome")] {
            let c = crate::button::contrast(edge, against);
            assert!(c >= 2.0, "{kind:?}: the edge against the {what} at {c:.2}:1");
        }
        let c = crate::button::contrast(t.text, fill);
        assert!(c >= 4.5, "{kind:?}: the words at {c:.2}:1");
    }
}

#[test]
fn a_long_message_takes_a_second_line_before_it_is_shortened() {
    let wide = landed("wide", 1600.0);
    let text = samples::message(&wide.app, true).unwrap_or_default();
    assert!(text.starts_with("Drag any slider"), "{text}");
    assert_eq!(banner(&wide).height(), 34.0, "one line where there is room");
    let narrow = landed("narrow", 760.0);
    assert_eq!(banner(&narrow).height(), 50.0, "two lines where there isn't");
}

#[test]
fn screen_readers_hear_the_banner_and_toasts_as_they_come() {
    let mut app = LightcraftApp::new(library("live"), Services::default());
    app.run("library.addSamples", json!({})).unwrap();
    let words = samples::message(&app, true).unwrap();
    app.canvas_rect = Some(Rect::from_min_size(pos2(0.0, 40.0), vec2(1200.0, 700.0)));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut live = Vec::new();
    for _ in 0..3 {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            samples::banner(&mut app, ui);
            crate::panels::toast(&mut app, ui.ctx());
        });
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update.take() {
            live = update
                .nodes
                .iter()
                .filter(|(_, n)| n.live() == Some(egui::accesskit::Live::Polite))
                .map(|(_, n)| format!("{}{}", n.label().unwrap_or_default(), n.value().unwrap_or_default()))
                .collect();
        }
    }
    assert!(live.contains(&words), "the banner: {live:?}");
    // (adding them says so in a toast)
    assert!(live.iter().any(|w| w.starts_with("Added 24 sample photos")), "the toast: {live:?}");
}

#[test]
fn a_ghost_button_shows_its_edge_when_pointed_at() {
    use crate::button::{Kind, State, look};
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        assert!(look(&t, Kind::Ghost, State::Idle).border.is_none(), "{kind:?}: quiet at rest");
        assert!(look(&t, Kind::Ghost, State::Hovered).border.is_some(), "{kind:?}");
    }
}
