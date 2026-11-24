//! Help ▸ What's New: one disclosure per date (the newest open), built a batch at a time as the scroll reaches
//! the end; inline Markdown in the notes. (The update panel's notes: `tests_update.rs`.)
#![allow(clippy::indexing_slicing, reason = "test code: clippy.toml allows indexing in tests, but its test check misses cfg(all(test, ...))")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::headless::Headless;
use crate::notes_view::{FIRST_BATCH, Section, loaded, parsed, total};
use crate::{LccApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

/// The app with What's New open in a window of `size`, settled.
fn open(size: [f32; 2]) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    h.view.ctx.enable_accesskit();
    h.settle(Duration::from_secs(60));
    let r = h.request("ui.menu.invoke", json!({"id": "app.whatsNew"}), T);
    assert_eq!(r["ok"], true, "{r}");
    settle(&mut h);
    h
}

fn settle(h: &mut Headless) {
    for _ in 0..40 {
        h.step();
    }
}

fn shown(h: &Headless) -> Vec<String> {
    h.painted_text()
}

fn is_shown(h: &Headless, text: &str) -> bool {
    shown(h).iter().any(|t| t == text)
}

/// The first `### ` title of section `n` that no earlier section shares.
fn title_of(n: usize) -> &'static str {
    let all = parsed();
    all[n]
        .lines
        .iter()
        .filter_map(|l| l.strip_prefix("### "))
        .find(|t| !all[..n].iter().any(|s| s.lines.iter().any(|l| l.strip_prefix("### ") == Some(*t))))
        .expect("the section has a title of its own")
}

fn header_ids(h: &Headless) -> Vec<String> {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with("disclosure:whatsNew-")).map(|(w, _)| w.clone()).collect()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    settle(h);
}

#[test]
fn each_date_is_a_header_and_only_the_newest_starts_open() {
    let mut h = open([1300.0, 900.0]);
    let all = parsed();
    assert!(all.len() > 3);
    assert!(is_shown(&h, all[0].heading), "the newest date is a header: {:?}", shown(&h));
    assert!(header_ids(&h).len() >= 2, "every date has a header, built as far as the batch goes");
    assert!(is_shown(&h, title_of(0)), "the newest date's entries are shown");
    // closing the newest brings the next header into the window whatever the newest holds
    click(&mut h, "disclosure:whatsNew-0");
    assert!(!is_shown(&h, title_of(0)), "the newest closed");
    assert!(is_shown(&h, all[1].heading), "so the next date's header is in view: {:?}", shown(&h));
    assert!(!is_shown(&h, title_of(1)), "the next date's entries are folded away");
    // opening one leaves the others as they were
    click(&mut h, "disclosure:whatsNew-1");
    assert!(is_shown(&h, title_of(1)), "the header opened");
    assert!(!is_shown(&h, title_of(0)), "the newest stayed closed");
    assert!(!is_shown(&h, title_of(2)), "the third stayed closed");
    click(&mut h, "disclosure:whatsNew-0");
    assert!(is_shown(&h, title_of(0)), "the newest opened again");
}

#[test]
fn each_opening_starts_over_with_the_newest_open() {
    let mut h = open([1300.0, 900.0]);
    click(&mut h, "disclosure:whatsNew-1");
    click(&mut h, "disclosure:whatsNew-0");
    h.request("ui.key", json!({"key": "Escape"}), T);
    settle(&mut h);
    assert!(h.app.ui.dialog.is_none());
    let r = h.request("ui.menu.invoke", json!({"id": "app.whatsNew"}), T);
    assert_eq!(r["ok"], true, "{r}");
    settle(&mut h);
    assert!(is_shown(&h, title_of(0)) && !is_shown(&h, title_of(1)), "the newest open, the rest closed, as on a first opening");
}

#[test]
fn only_what_fills_the_window_is_built_and_the_rest_comes_with_the_scroll() {
    let mut h = open([1000.0, 420.0]);
    let opens = h.app.ui.whats_new_opens;
    let first = loaded(&h.view.ctx, opens);
    assert!(first >= FIRST_BATCH, "at least the first batch: {first}");
    assert!(first < total(), "not every date is built when the window opens: {first} of {}", total());
    assert_eq!(header_ids(&h).len(), first, "what is built is what is laid out");
    // scroll to the end, as far as it goes at each step
    let window = h.app.widgets.iter().find(|(w, _)| w == "dialog:window").map(|(_, r)| r.center()).unwrap();
    h.request("ui.move", json!({"x": window.x, "y": window.y}), T);
    let mut last = first;
    for _ in 0..400 {
        h.request("ui.scroll", json!({"dy": -400.0}), T);
        settle_short(&mut h);
        let now = loaded(&h.view.ctx, opens);
        assert!(now >= last, "dates are never taken back: {last} then {now}");
        last = now;
        if now == total() {
            break;
        }
    }
    assert_eq!(last, total(), "scrolling to the end builds every date");
    let ids = header_ids(&h);
    let want: Vec<String> = (0..total()).map(|n| format!("disclosure:whatsNew-{n}")).collect();
    assert_eq!(ids, want, "each date once, in order");
}

fn settle_short(h: &mut Headless) {
    for _ in 0..6 {
        h.step();
    }
}

#[test]
fn headers_can_be_reached_and_named_without_the_pointer() {
    use egui::accesskit::Action;
    let h = open([1300.0, 900.0]);
    let all = parsed();
    let update = h.view.access.as_ref().expect("an accessibility tree");
    for n in 0..2 {
        let node = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.label() == Some(all[n].heading) || node.value() == Some(all[n].heading))
            .unwrap_or_else(|| panic!("a node named {:?}", all[n].heading));
        assert!(node.supports_action(Action::Focus), "{:?} can take the keyboard focus: {node:?}", all[n].heading);
        assert!(node.supports_action(Action::Click), "{:?} can be pressed: {node:?}", all[n].heading);
    }
}

/// One frame of `sections` in an egui context `ctx`, with the pointer events `events`: the text painted, the
/// automation ids registered and the link that was clicked.
fn frame(ctx: &egui::Context, sections: &[Section<'_>], events: Vec<egui::Event>) -> (Vec<String>, Vec<(String, egui::Rect)>, Option<String>) {
    let t = crate::theme::Tokens::default();
    let raw =
        egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), events, ..Default::default() };
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("lc-registry"), crate::widgets::Registry::default()));
    let mut clicked = None;
    let mut out = ctx.run_ui(raw, |ui| {
        clicked = crate::notes_view::sections_view(ui, &t, 0, sections);
    });
    out.textures_delta.clear();
    let text = out
        .shapes
        .iter()
        .filter_map(|s| if let egui::epaint::Shape::Text(t) = &s.shape { Some(t.galley.text().to_string()) } else { None })
        .collect();
    let ids = ctx.data(|d| d.get_temp::<crate::widgets::Registry>(egui::Id::new("lc-registry"))).unwrap_or_default().0;
    (text, ids, clicked)
}

/// `sections` drawn once the layout has settled: the text painted and the automation ids registered.
fn drawn(sections: &[Section<'_>]) -> (Vec<String>, Vec<(String, egui::Rect)>) {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut last = (Vec::new(), Vec::new());
    for _ in 0..3 {
        let (text, ids, _) = frame(&ctx, sections, vec![]);
        last = (text, ids);
    }
    last
}

#[test]
fn a_section_whose_heading_is_not_a_date_or_is_missing_still_shows() {
    let sections = vec![
        Section { heading: "2026-13-45", lines: vec!["### Odd date", "- stays"] },
        Section { heading: "Upstream notes", lines: vec!["### Titled", "- kept"] },
        Section { heading: "", lines: vec!["### Before any heading", "- still here"] },
    ];
    let (text, ids) = drawn(&sections);
    let text = text.join("\n");
    assert!(text.contains("2026-13-45"), "a date that can't be read is the header as written: {text}");
    assert!(text.contains("Upstream notes"), "so is an undated title: {text}");
    assert!(text.contains("Before any heading") && text.contains("still here"), "lines with no heading are drawn flat: {text}");
    let heads: Vec<&str> = ids.iter().map(|(id, _)| id.as_str()).filter(|id| id.starts_with("disclosure:")).collect();
    assert!(heads.contains(&"disclosure:whatsNew-0") && heads.contains(&"disclosure:whatsNew-1"), "the two with a heading fold: {heads:?}");
    assert!(!heads.contains(&"disclosure:whatsNew-2"), "the one with none is not a header: {heads:?}");
}

#[test]
fn inline_markup_is_drawn_without_its_markers() {
    let sections = vec![Section {
        heading: "2026-11-23",
        lines: vec!["- Press `Cmd+K` for **quick** help and [the guide](https://example.com/g)", "- a lone ** and a ` and [x]( stay"],
    }];
    let text = drawn(&sections).0.join("\n");
    assert!(text.contains("Cmd+K") && text.contains("quick") && text.contains("the guide"), "{text}");
    assert!(!text.contains("`Cmd") && !text.contains("**quick") && !text.contains("](https"), "markers are not shown: {text}");
    assert!(text.contains("a lone ** and a ` and [x]( stay"), "markup that isn't closed shows as written: {text}");
}

/// Clicks the centre of the link `id` in `sections`: the URL the click opened, and the link ids that were drawn.
fn click_link(sections: &[Section<'_>], id: &str) -> (Option<String>, Vec<String>) {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut ids: Vec<String> = Vec::new();
    for _ in 0..3 {
        ids = frame(&ctx, sections, vec![]).1.into_iter().map(|(i, _)| i).filter(|i| i.starts_with("link:notes:")).collect();
    }
    let rect = frame(&ctx, sections, vec![]).1.into_iter().find(|(i, _)| i == id).map(|(_, r)| r);
    let Some(rect) = rect else { return (None, ids) };
    let at = rect.center();
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(&ctx, sections, vec![egui::Event::PointerMoved(at)]);
    frame(&ctx, sections, vec![button(true)]);
    let (_, _, clicked) = frame(&ctx, sections, vec![button(false)]);
    (clicked, ids)
}

#[test]
fn a_click_on_a_link_in_what_s_new_gives_its_url_and_a_refused_link_is_not_one() {
    let line = "- see [the guide](https://example.com/guide) or [plain http](http://example.com) or [script](javascript:alert(1))";
    let sections = vec![Section { heading: "2026-11-23", lines: vec![line] }];
    let (clicked, ids) = click_link(&sections, "link:notes:0");
    assert_eq!(ids, ["link:notes:0"], "only the https link is a link");
    assert_eq!(clicked.as_deref(), Some("https://example.com/guide"), "the click names the link's URL");
    assert_eq!(click_link(&sections, "link:notes:1").0, None, "there is no second link to click");
}

#[test]
fn a_clicked_link_opens_through_the_host_and_a_host_that_cannot_says_so() {
    use std::sync::{Arc, Mutex};
    let mut h = Headless::new(LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() }), [800.0, 600.0], 1.0);
    h.settle(Duration::from_secs(60));
    let ctx = h.view.ctx.clone();
    // no host service: the person is told
    assert!(h.app.ui.toast().is_none());
    crate::notes_view::open_link(&mut h.app, &ctx, "https://example.com/a");
    assert!(h.app.ui.toast().is_some(), "a host with no way to open links says so");
    // a host that opens it gets the URL as written
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let o = opened.clone();
    h.app.services.open_url = Some(Box::new(move |u: &str| {
        o.lock().unwrap().push(u.to_string());
        Ok(())
    }));
    crate::notes_view::open_link(&mut h.app, &ctx, "https://example.com/b_(c)");
    assert_eq!(opened.lock().unwrap().as_slice(), ["https://example.com/b_(c)"]);
}
