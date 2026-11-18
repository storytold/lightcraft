//! Progress bars, spinners and busy buttons: the fraction is always a sane number, a bar is
//! announced as progress with its percentage and reachable by its id, a busy button is announced
//! busy, ignores clicks and keeps its width, and a thumbnail's placeholder stands apart from what
//! it sits on in every theme.

use egui::{Pos2, Rect, pos2, vec2};

use crate::button::{Button, contrast};
use crate::icons::Icon;
use crate::progress::{Progress, clamp_fraction, fraction_of, spinner};
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
fn an_unknown_total_is_an_indeterminate_bar_not_an_empty_one() {
    assert_eq!(fraction_of(0, 0), None);
    assert_eq!(fraction_of(5, 0), None);
    assert_eq!(fraction_of(0, 10), Some(0.0));
    assert_eq!(fraction_of(5, 10), Some(0.5));
    assert_eq!(fraction_of(20, 10), Some(1.0));
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
            // the light themes' surfaces are close together, so the placeholder has to be set
            // further off them to read at all
            let floor = if matches!(kind, ThemeKind::Silver | ThemeKind::Paper) { 1.15 } else { 1.1 };
            assert!(c >= floor, "{kind:?}: the placeholder on the {name} is {c:.2}:1");
        }
        let mark = contrast(t.text, t.skeleton);
        assert!(mark >= 3.0, "{kind:?}: the failure mark on the placeholder is {mark:.2}:1");
    }
}

#[test]
fn a_failed_filmstrip_thumbnail_is_announced_with_its_reason() {
    let spoken = |failed: bool| {
        let (nodes, _, _) = frames(Vec::new(), |ui| {
            let resp = ui.interact(Rect::from_min_size(pos2(10.0, 10.0), vec2(120.0, 80.0)), egui::Id::unique("film-test"), egui::Sense::click());
            crate::widgets::announce(&resp, crate::panels::detail::film_spoken("IMG_0001.jpg", failed));
        });
        nodes.into_iter().find(|n| n.role == "Button").map(|n| n.label)
    };
    assert_eq!(spoken(false).as_deref(), Some("IMG_0001.jpg"));
    assert_eq!(spoken(true).as_deref(), Some("IMG_0001.jpg, This photo can't be opened"));
}

#[test]
fn a_failed_filmstrip_thumbnail_without_a_file_name_has_no_leading_comma() {
    assert_eq!(crate::panels::detail::film_spoken("", true), "This photo can't be opened");
    assert_eq!(crate::panels::detail::film_spoken("", false), "");
}

#[test]
fn the_sam_bar_keeps_its_name_and_height_while_the_size_is_unknown() {
    let (nodes, _, (known, unknown)) = frames(Vec::new(), |ui| {
        let known = crate::panels::masking::sam_download_bar(ui, 5, 10, 200.0).rect.height();
        let unknown = crate::panels::masking::sam_download_bar(ui, 0, 0, 200.0).rect.height();
        (known, unknown)
    });
    assert_eq!(known, unknown);
    let bars: Vec<_> = nodes.iter().filter(|n| n.role == "ProgressIndicator").collect();
    assert_eq!(bars.len(), 2, "{nodes:?}");
    assert!(bars.iter().all(|b| b.label.starts_with("SAM 3")), "{bars:?}");
}

#[test]
fn the_denoise_bars_keep_their_widths_in_a_wide_container() {
    let (_, ids, (detail, settings, unknown)) = frames(Vec::new(), |ui| {
        let detail = crate::panels::denoise::detail_bar(ui, 3, 10).rect.width();
        let settings = crate::panels::denoise::settings_bar(ui, 3, 10, "Denoising a.jpg: 3 of 10 tiles".into()).rect.width();
        let unknown = crate::panels::denoise::detail_bar(ui, 0, 0).rect.width();
        (detail, settings, unknown)
    });
    assert_eq!((detail, settings, unknown), (240.0, 380.0, 240.0));
    assert!(ids.iter().any(|(id, _)| id == "denoise:settingsProgress"));
}

#[test]
fn the_denoise_bar_label_and_its_accessible_value_agree() {
    // the number drawn on the bar is the one announced, and exact decimals stay exact
    for (done, total, says) in
        [(3, 7, 42.0), (53, 100, 53.0), (59, 100, 59.0), (29, 100, 29.0), (99, 100, 99.0), (100, 100, 100.0), (999, 1000, 99.0)]
    {
        let (nodes, _, _) = frames(Vec::new(), |ui| {
            crate::panels::denoise::detail_bar(ui, done, total);
        });
        let bar = nodes.iter().find(|n| n.role == "ProgressIndicator").unwrap_or_else(|| panic!("{nodes:?}"));
        let shown: f64 = bar.label.rsplit(' ').next().unwrap().trim_end_matches('%').parse().unwrap();
        assert_eq!((shown, bar.value), (says, Some(says)), "{done}/{total}: {bar:?}");
    }
}

#[test]
fn the_sam_bar_text_and_its_accessible_value_agree() {
    for (done, total, says) in [(53, 100, 53.0), (59, 100, 59.0), (29, 100, 29.0), (999, 1000, 99.0), (100, 100, 100.0)] {
        let (nodes, _, _) = frames(Vec::new(), |ui| crate::panels::masking::sam_download_bar(ui, done, total, 200.0).rect.width());
        let bar = nodes.iter().find(|n| n.role == "ProgressIndicator").unwrap_or_else(|| panic!("{nodes:?}"));
        assert_eq!(bar.label, format!("SAM 3: {says} %"), "{done}/{total}");
        assert_eq!(bar.value, Some(says), "{done}/{total}");
    }
}

#[test]
fn an_activity_bar_is_named_after_its_task() {
    let task = lightcraft_engine::activity::TaskInfo {
        id: 1,
        kind: "export",
        label: "Exporting".into(),
        done: 3,
        total: 25,
        unit: lightcraft_engine::activity::Unit::Count,
        detail: String::new(),
        cancellable: true,
        cancelling: false,
        age_ms: 600,
    };
    let (nodes, _, _) = frames(Vec::new(), |ui| crate::panels::activity::bar(ui, &task, 200.0).rect.width());
    let bar = nodes.iter().find(|n| n.role == "ProgressIndicator").unwrap_or_else(|| panic!("{nodes:?}"));
    assert_eq!(bar.label, "Exporting");
    assert_eq!(bar.value, Some(12.0));
}
