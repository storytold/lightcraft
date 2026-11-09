//! The Settings window is one size in one place whatever tab is open, and lies inside the app
//! window: a row with more choices than fit wraps, a label longer than its column wraps, a long
//! folder path is cut short (its tooltip has all of it), and the tabs stay put while a page scrolls.
//!
//! Before, a wide row widened the whole window: General was 970 points wide and the other tabs
//! about 600, the window moved up and down between tabs, and in a 900 point wide app window the
//! row labels and the Close button of General were cut off at the left.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::settings::TABS;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// The app window sizes tried: roomy, the desktop minimum, and smaller than that.
const SIZES: [[f32; 2]; 3] = [[1600.0, 1000.0], [900.0, 560.0], [640.0, 480.0]];

fn headless(session: lightcraft_engine::Session, size: [f32; 2]) -> Headless {
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), size, 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn demo(size: [f32; 2]) -> Headless {
    headless(lightcraft_engine::Session::with_demo(), size)
}

/// Given Settings open on `tab`, drawn until its layout has settled.
fn show(h: &mut Headless, tab: &str) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    for _ in 0..4 {
        h.step();
    }
}

/// A folder under the system's temporary one, removed again when the test ends, passed or not.
struct TempFolder(std::path::PathBuf);

impl TempFolder {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// The controls the Settings window registers on the tab that is open (and its tab buttons).
fn controls(h: &Headless) -> Vec<(String, egui::Rect)> {
    let named = |id: &str| id.split_once(':').is_some_and(|(_, name)| name.starts_with("settings") || name.starts_with("theme"));
    h.app.widgets.iter().filter(|(id, _)| named(id)).cloned().collect()
}

/// Whether egui has a visible tooltip area, independent of unrelated window text.
fn tooltip_shown(h: &Headless) -> bool {
    h.view.ctx.memory(|m| m.areas().visible_layer_ids().into_iter().any(|layer| layer.order == egui::Order::Tooltip))
}

fn same(a: egui::Rect, b: egui::Rect) -> bool {
    (a.min - b.min).abs().max_elem() < 0.5 && (a.max - b.max).abs().max_elem() < 0.5
}

#[test]
fn the_window_keeps_its_size_and_place_from_tab_to_tab() {
    for size in SIZES {
        let mut h = demo(size);
        // Given the first tab, when every other tab is opened in turn
        show(&mut h, "general");
        let first = rect(&h, "dialog:window");
        for (tab, _) in TABS {
            show(&mut h, tab);
            // Then the window has not moved or changed size
            let now = rect(&h, "dialog:window");
            assert!(same(first, now), "{size:?}: Settings is {now:?} on {tab}, and was {first:?} on general");
        }
        // And going back to the first tab finds it as it was
        show(&mut h, "general");
        assert!(same(first, rect(&h, "dialog:window")), "{size:?}: general again");
    }
}

#[test]
fn every_tab_keeps_its_controls_inside_the_window() {
    for size in SIZES {
        let mut h = demo(size);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
        for (tab, _) in TABS {
            // When a tab is opened
            show(&mut h, tab);
            // Then the Settings window lies inside the app window
            let window = rect(&h, "dialog:window");
            assert!(screen.contains_rect(window), "{size:?} {tab}: Settings {window:?} runs off the {screen:?} window");
            // And no control reaches past its left or right edge
            // (the page is everything the tab drew, whether or not it has a name of its own; a
            // control past the room for it would be clipped there, inside the window's margin)
            let (page, room) = (rect(&h, "settings:page"), rect(&h, "settings:pageRoom"));
            assert!(
                page.left() >= room.left() - 0.5 && page.right() <= room.right() + 0.5,
                "{size:?} {tab}: the page {page:?} is wider than the room for it {room:?}"
            );
            assert!(window.contains_rect(room), "{size:?} {tab}: the page's room {room:?} is inside Settings {window:?}");
            for (id, r) in controls(&h) {
                let within = if id.contains("settingsTab-") { window } else { room };
                assert!(r.left() >= within.left() - 0.5 && r.right() <= within.right() + 0.5, "{size:?} {tab}: {id} {r:?} is outside {within:?}");
            }
        }
    }
}

#[test]
fn a_row_of_many_choices_wraps_under_itself() {
    // Given an app window too narrow for the languages in one line
    let mut h = demo([900.0, 560.0]);
    show(&mut h, "general");
    let languages: Vec<egui::Rect> = (0..crate::i18n::Locale::ALL.len()).map(|i| rect(&h, &format!("button:settingsLanguage-{i}"))).collect();
    // Then they take more than one line
    let first = languages[0];
    assert!(languages.iter().any(|r| r.top() > first.bottom()), "the languages wrap: {languages:?}");
    // And each further line starts under the first choice, not under the row's label
    for r in languages.iter().filter(|r| r.top() > first.bottom()) {
        assert!(r.left() >= first.left() - 0.5, "{r:?} starts left of the first language {first:?}");
    }
    assert!(
        languages.iter().filter(|r| r.top() > first.bottom()).any(|r| (r.left() - first.left()).abs() < 0.5),
        "a wrapped line starts at the first choice"
    );
}

#[test]
fn a_label_longer_than_its_column_leaves_the_controls_in_line() {
    // Given Performance, whose "Forget unchanged photos" label is wider than the label column
    let mut h = demo([1600.0, 1000.0]);
    show(&mut h, "performance");
    // Then its choices start where the choices of a short label do
    let (short, long) = (rect(&h, "button:settingsPreview-0"), rect(&h, "button:settingsForgetLocal-0"));
    assert!((short.left() - long.left()).abs() < 0.5, "Preview size starts at {} and Forget unchanged photos at {}", short.left(), long.left());
}

#[test]
fn a_long_library_folder_is_cut_short_and_its_tooltip_has_all_of_it() {
    // Given a library in a folder with a very long path
    let base = TempFolder::new("settings-window");
    let dir =
        base.0.join("photographs-from-2019-to-2026-kept-on-the-second-external-drive-that-nobody-wants-to-type-twice").join("the-master-library");
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&dir, true).unwrap();
    let path = dir.display().to_string();

    let mut h = headless(session, [1600.0, 1000.0]);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
    // When Settings opens on General, which shows where the library is
    show(&mut h, "general");
    let general = rect(&h, "dialog:window");
    show(&mut h, "display");
    // Then the window is as wide as on a tab without a path
    assert!(same(general, rect(&h, "dialog:window")), "General is {general:?} with a long path, Display is {:?}", rect(&h, "dialog:window"));
    show(&mut h, "general");
    assert!(screen.contains_rect(general), "Settings {general:?} runs off the {screen:?} window");
    // And the path is cut short, inside the window
    let location = rect(&h, "text:settingsLocation");
    let room = rect(&h, "settings:pageRoom");
    assert!(location.right() <= room.right() + 0.5, "the path {location:?} stays inside the room for the page {room:?}");
    let texts = h.painted_text();
    assert!(!texts.iter().any(|text| text.contains(&path)), "the whole path is not drawn in the row");
    // And what is left of it is its end, which says which library this is
    let name = "the-master-library";
    assert!(path.ends_with(name));
    assert!(
        texts.iter().any(|text| text.starts_with('…') && text.ends_with(name)),
        "the row keeps the end of the path: {:?}",
        texts.iter().filter(|t| t.contains("library")).collect::<Vec<_>>()
    );
    // And resting the pointer on it shows the whole path
    assert_eq!(h.request("ui.hoverWidget", json!({"id": "text:settingsLocation"}), T)["ok"], true);
    assert!(
        h.step_until(T, |h| tooltip_shown(h) && h.painted_text().iter().any(|text| text.contains(&path))),
        "the tooltip layer shows the whole path"
    );

    // And the same holds for the folder Auto Import watches, which stays on one line
    let watched = h.app.run("library.autoImport", json!({"folder": path}));
    assert!(watched.is_ok(), "{watched:?}");
    show(&mut h, "import");
    // (the row is at the foot of the page: scrolled to, so that it is drawn however long the page is)
    let window = rect(&h, "dialog:window");
    h.request("ui.move", json!({"x": window.center().x, "y": window.center().y}), T);
    for _ in 0..6 {
        h.request("ui.scroll", json!({"dy": -400.0}), T);
        h.step();
    }
    h.settle(T);
    let (folder, room) = (rect(&h, "text:settingsAutoFolder"), rect(&h, "settings:pageRoom"));
    assert!(room.intersects(folder), "the watched folder {folder:?} is in view in {room:?}");
    assert!(folder.height() < 24.0 && folder.right() <= room.right(), "the watched folder {folder:?} is one line inside {room:?}");
    assert!(h.painted_text().iter().any(|text| text.starts_with('…') && text.ends_with(name)), "and keeps its end");
    // And Choose… stays on that line, inside the room, whatever length the path is cut to: the
    // path takes what the button leaves (the folders' names vary, so the cut falls everywhere)
    for n in 0..14 {
        let path = format!("{path}/{}/{name}", "d".repeat(2 + 3 * n));
        std::fs::create_dir_all(&path).unwrap();
        assert!(h.app.run("library.autoImport", json!({"folder": path})).is_ok());
        h.settle(T);
        for _ in 0..4 {
            h.step();
        }
        let (folder, button) = (rect(&h, "text:settingsAutoFolder"), rect(&h, "button:settingsAutoFolder"));
        assert!(button.right() <= room.right() + 0.5, "Choose… {button:?} is inside {room:?} (path of {n})");
        assert!((button.center().y - folder.center().y).abs() < 6.0, "Choose… {button:?} is on the line of the path {folder:?} (path of {n})");
        assert!(folder.right() <= button.left(), "the path {folder:?} ends before Choose… {button:?} (path of {n})");
    }
    drop(h);
}

#[test]
fn library_default_stays_on_the_line_of_the_smart_previews_folder_whatever_its_length() {
    // Given libraries whose folder names grow in steps (the smart previews folder is `Smart Previews` in
    // each, so its path is cut at a different place every time, and some cut leaves less room than the note needs)
    let base = TempFolder::new("settings-smart");
    for n in 0..24 {
        let dir = base.0.join(format!("photographs-kept-on-the-second-external-drive-{n}")).join("m".repeat(2 + 3 * n));
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = lightcraft_engine::Session::new().with_fs();
        session.open_library(&dir, true).unwrap();
        let mut h = headless(session, [1600.0, 1000.0]);
        // When Performance is scrolled to the foot of its page
        show(&mut h, "performance");
        let window = rect(&h, "dialog:window");
        h.request("ui.move", json!({"x": window.center().x, "y": window.center().y}), T);
        for _ in 0..6 {
            h.request("ui.scroll", json!({"dy": -400.0}), T);
            h.step();
        }
        h.settle(T);
        // Then "(library default)" stays inside the room, on the line of the path, after it
        let room = rect(&h, "settings:pageRoom");
        let (smart, note) = (rect(&h, "text:settingsSmartFolder"), rect(&h, "text:settingsSmartDefault"));
        assert!(room.intersects(smart) && room.intersects(note), "the smart previews folder {smart:?} {note:?} is in view in {room:?} (name of {n})");
        assert!(note.right() <= room.right() + 0.5, "(library default) {note:?} is inside {room:?} (name of {n})");
        assert!((note.center().y - smart.center().y).abs() < 6.0, "(library default) {note:?} is on the line of the path {smart:?} (name of {n})");
        assert!(smart.right() <= note.left(), "the path {smart:?} ends before (library default) {note:?} (name of {n})");
        drop(h);
    }
}

#[test]
fn a_short_path_is_shown_whole_and_has_no_tooltip() {
    // Given the in-memory library, whose Location is a short sentence
    let mut h = demo([1600.0, 1000.0]);
    show(&mut h, "general");
    let location = "In memory — nothing is saved";
    assert!(h.painted_text().iter().any(|text| text == location), "it is all there");
    assert!(!tooltip_shown(&h), "no tooltip before hovering the location");
    assert_eq!(h.request("ui.hoverWidget", json!({"id": "text:settingsLocation"}), T)["ok"], true);
    // Observe every frame for two simulated seconds beyond the configured tooltip delay.
    // A render completing or a toast disappearing may change other text in the window.
    let rest_until = h.time() + f64::from(h.view.ctx.global_style().interaction.tooltip_delay) + 2.0;
    assert!(!tooltip_shown(&h), "no tooltip when hovering the complete location");
    while h.time() < rest_until {
        h.step();
        assert!(!tooltip_shown(&h), "no tooltip for a path that is all there at {}", h.time());
        assert_eq!(
            h.painted_text().iter().filter(|text| text.as_str() == location).count(),
            1,
            "the full location is drawn once throughout the hover"
        );
    }
}

/// How many pixels of `rect` are not `colour` (one pixel per point here).
fn other_than(image: &egui::ColorImage, rect: egui::Rect, colour: egui::Color32) -> usize {
    let [w, h] = image.size;
    let (x0, x1) = (rect.left().round().max(0.0) as usize, (rect.right().round().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().round().max(0.0) as usize, (rect.bottom().round().max(0.0) as usize).min(h));
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|(x, y)| image.pixels[y * w + x] != colour).count()
}

#[test]
fn a_page_that_goes_on_below_shows_a_scroll_handle_without_the_pointer_on_it() {
    let mut h = demo([900.0, 560.0]);
    let chrome = crate::theme::Tokens::get(&h.view.ctx).chrome;
    // (the pointer is nowhere near the window)
    h.request("ui.move", json!({"x": 5.0, "y": 5.0}), T);
    let handle = |h: &Headless| {
        let room = rect(h, "settings:pageRoom");
        other_than(&h.paint(), egui::Rect::from_min_max(egui::pos2(room.right() - 6.0, room.top()), room.max), chrome)
    };
    // Given Import, which is taller than the room a 560 pt tall app window has for it
    show(&mut h, "import");
    assert!(rect(&h, "settings:page").height() > rect(&h, "settings:pageRoom").height());
    // Then a handle is drawn at the page's right edge
    assert!(handle(&h) > 40, "Import shows a scroll handle ({} px)", handle(&h));
    // And Display, which fits, has none
    show(&mut h, "display");
    assert!(rect(&h, "settings:page").height() <= rect(&h, "settings:pageRoom").height());
    assert_eq!(handle(&h), 0, "Display has nothing to scroll");
}

#[test]
fn a_window_too_short_for_a_page_of_its_own_scrolls_tabs_and_page_together() {
    // Given an app window far below the desktop minimum
    let mut h = demo([900.0, 220.0]);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
    show(&mut h, "import");
    let window = rect(&h, "dialog:window");
    assert!(screen.contains_rect(window), "Settings {window:?} is inside the {screen:?} window");
    let (tab, page) = (rect(&h, "button:settingsTab-import"), rect(&h, "check:settings.perCamera"));
    // When it is scrolled
    h.request("ui.move", json!({"x": window.center().x, "y": window.center().y}), T);
    for _ in 0..3 {
        h.request("ui.scroll", json!({"dy": -120.0}), T);
        h.step();
    }
    h.settle(T);
    // Then the tabs and the page move as one, so every setting can still be reached
    let (tab_now, page_now) = (rect(&h, "button:settingsTab-import"), rect(&h, "check:settings.perCamera"));
    assert!(page_now.top() < page.top() - 50.0, "the page scrolled: {page:?} -> {page_now:?}");
    assert!(((tab.top() - tab_now.top()) - (page.top() - page_now.top())).abs() < 0.5, "tabs and page moved together");
    assert!(same(window, rect(&h, "dialog:window")), "the window did not move");
}

#[test]
fn the_tabs_stay_put_while_a_page_scrolls() {
    // Given a page taller than the window has room for
    let mut h = demo([900.0, 560.0]);
    show(&mut h, "import");
    let window = rect(&h, "dialog:window");
    let tabs: Vec<egui::Rect> = TABS.iter().map(|(id, _)| rect(&h, &format!("button:settingsTab-{id}"))).collect();
    let page_before = rect(&h, "check:settings.perCamera");
    // When the page is scrolled
    h.request("ui.move", json!({"x": window.center().x, "y": window.center().y}), T);
    for _ in 0..3 {
        h.request("ui.scroll", json!({"dy": -120.0}), T);
        h.step();
    }
    h.settle(T);
    // Then the page moved
    let page_after = rect(&h, "check:settings.perCamera");
    assert!(page_after.top() < page_before.top() - 50.0, "the page scrolled: {page_before:?} -> {page_after:?}");
    // And the tabs and the window did not
    for ((id, _), before) in TABS.iter().zip(tabs) {
        assert!(same(before, rect(&h, &format!("button:settingsTab-{id}"))), "tab {id} moved");
    }
    assert!(same(window, rect(&h, "dialog:window")), "the window moved");
}

#[test]
fn each_tab_remembers_how_far_it_was_scrolled() {
    let mut h = demo([900.0, 560.0]);
    show(&mut h, "interface");
    let fresh = rect(&h, "button:settingsAppearance-0");
    // Given Import scrolled down
    show(&mut h, "import");
    let window = rect(&h, "dialog:window");
    let top = rect(&h, "check:settings.perCamera").top();
    h.request("ui.move", json!({"x": window.center().x, "y": window.center().y}), T);
    for _ in 0..3 {
        h.request("ui.scroll", json!({"dy": -120.0}), T);
        h.step();
    }
    h.settle(T);
    let scrolled = rect(&h, "check:settings.perCamera").top();
    assert!(scrolled < top - 50.0);
    // When another tab is opened, it starts at its top
    show(&mut h, "interface");
    let mode = rect(&h, "button:settingsAppearance-0");
    assert!(same(fresh, mode), "Interface is at its top, as when first opened: {fresh:?} -> {mode:?}");
    // And Import is where it was left
    show(&mut h, "import");
    assert!((rect(&h, "check:settings.perCamera").top() - scrolled).abs() < 0.5, "Import is where it was left");
    // And it is still there when Settings is closed and opened again (documented in What's New)
    assert_eq!(h.request("ui.dialog.cancel", json!({}), T)["ok"], true);
    h.settle(T);
    show(&mut h, "import");
    assert!((rect(&h, "check:settings.perCamera").top() - scrolled).abs() < 0.5, "Import is where it was left after a reopen");
}
