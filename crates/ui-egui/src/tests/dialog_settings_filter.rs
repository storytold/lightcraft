//! Settings has a filter: typing in it lists the sections, on whatever tab, whose words match, and
//! choosing one opens its tab at that section.
//!
//! Before, finding a setting meant knowing or guessing its tab: eight of them, each read through.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo(size: [f32; 2]) -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), size, 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

fn show(h: &mut Headless, tab: &str) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    steps(h);
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "click {id}: {r}");
    steps(h);
}

fn key(h: &mut Headless, key: &str) {
    assert_eq!(h.request("ui.key", json!({"key": key}), T)["ok"], true, "{key}");
    steps(h);
}

/// Given the filter with `text` typed into it.
fn filter(h: &mut Headless, text: &str) {
    click(h, "field:settingsFilter");
    h.request("ui.key", json!({"key": "A", "cmd": true}), T);
    h.request("ui.text", json!({"text": text}), T);
    steps(h);
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// What the filter lists, top to bottom: `tab:section` of each result (`settingsFound:{tab}:{section}`).
fn found(h: &Headless) -> Vec<String> {
    let mut found: Vec<(f32, String)> =
        h.app.widgets.iter().filter_map(|(id, r)| Some((r.top(), id.strip_prefix("settingsFound:")?.to_string()))).collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.into_iter().map(|(_, id)| id).collect()
}

fn headings(h: &Headless) -> Vec<String> {
    h.app.widgets.iter().filter_map(|(id, _)| Some(id.strip_prefix("heading:")?.to_string())).collect()
}

/// Whether the keyboard focus is on something other than the filter box.
fn on_a_result(h: &Headless) -> bool {
    h.view.ctx.memory(|m| m.focused()).is_some_and(|id| id != crate::text_field::id("field:settingsFilter"))
}

fn says(h: &Headless, part: &str) -> bool {
    h.painted_text().iter().any(|text| text.contains(part))
}

#[test]
fn typing_lists_the_sections_that_match_on_whatever_tab() {
    let mut h = demo([1200.0, 1000.0]);
    // Given Settings open on General, which has nothing about sidecars (they are on Import)
    show(&mut h, "general");
    // When "sidecar names" is typed into the filter
    filter(&mut h, "sidecar names");
    // Then the section about them is listed with the tab it is on, and the page of General is not shown
    assert_eq!(found(&h), ["import:XMP sidecars"]);
    assert!(says(&h, "Import") && says(&h, "XMP sidecars"));
    assert!(headings(&h).is_empty(), "the tab's own page gives way to what was found: {:?}", headings(&h));
    // And the tab that was open is still the one open
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "general".into() }));
}

#[test]
fn a_word_from_a_row_a_hint_or_a_button_finds_its_section() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    // (a row's label)
    filter(&mut h, "Forget unchanged");
    assert_eq!(found(&h), ["performance:Local folders"]);
    // (a button)
    filter(&mut h, "Clear Cache");
    assert_eq!(found(&h), ["performance:Thumbnail cache"]);
    // (a hint, in other letter case)
    filter(&mut h, "CHOOSES THE LINES");
    assert_eq!(found(&h), ["interface:Detail View"]);
    // (a tab's own name finds everything on it)
    filter(&mut h, "Interface");
    let on_the_tab = found(&h);
    assert!(on_the_tab.len() >= 4 && on_the_tab.iter().all(|id| id.starts_with("interface:")), "{on_the_tab:?}");
    assert!(on_the_tab.contains(&"interface:Filmstrip".to_string()), "a section that does not say the word itself: {on_the_tab:?}");
}

#[test]
fn every_word_typed_has_to_be_there() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "cache");
    let cache = found(&h);
    assert!(cache.len() > 1 && cache.contains(&"performance:Thumbnail cache".to_string()), "{cache:?}");
    // When a second word narrows it
    filter(&mut h, "cache limit");
    assert_eq!(found(&h), ["performance:Thumbnail cache"]);
    // And the words can come in any order
    filter(&mut h, "limit cache");
    assert_eq!(found(&h), ["performance:Thumbnail cache"]);
}

#[test]
fn nothing_found_says_so() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "zebra crossing");
    assert!(found(&h).is_empty());
    assert!(says(&h, "No setting matches"), "{:?}", h.painted_text().iter().filter(|t| t.contains("match")).collect::<Vec<_>>());
    assert!(headings(&h).is_empty());
}

#[test]
fn choosing_a_result_opens_its_tab_at_that_section() {
    // (a window short enough that Detail View, near the foot of Interface, is below the fold)
    let mut h = demo([900.0, 560.0]);
    // Given Detail View below the fold of Interface
    show(&mut h, "interface");
    let (detail, room) = (rect(&h, "heading:Detail View").unwrap(), rect(&h, "settings:pageRoom").unwrap());
    assert!(detail.bottom() > room.bottom(), "Detail View {detail:?} starts out of view in {room:?}");
    show(&mut h, "general");
    filter(&mut h, "info overlay");
    assert_eq!(found(&h), ["interface:Detail View"]);
    // When the result is clicked
    click(&mut h, "settingsFound:interface:Detail View");
    h.settle(T);
    steps(&mut h);
    // Then its tab is open, the filter is empty again, and the section is in view
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "interface".into() }));
    assert!(found(&h).is_empty() && !says(&h, "info overlay"), "the filter is cleared");
    let (detail, room) = (rect(&h, "heading:Detail View").unwrap(), rect(&h, "settings:pageRoom").unwrap());
    assert!(detail.top() >= room.top() - 0.5 && detail.bottom() <= room.bottom() + 0.5, "Detail View {detail:?} is in view in {room:?}");
}

#[test]
fn choosing_what_starts_a_tab_shows_the_top_of_its_page() {
    let mut h = demo([900.0, 560.0]);
    // Given General scrolled down, so that its first heading is out of view
    show(&mut h, "general");
    let room = rect(&h, "settings:pageRoom").unwrap();
    h.request("ui.move", json!({"x": room.center().x, "y": room.center().y}), T);
    h.request("ui.scroll", json!({"dy": -300.0}), T);
    for _ in 0..60 {
        h.step();
    }
    assert!(rect(&h, "heading:Library").unwrap().top() < room.top(), "General is scrolled down");
    // When what is above its first heading (the language) is found and chosen
    filter(&mut h, "language");
    assert!(found(&h).contains(&"general:".to_string()), "{:?}", found(&h));
    click(&mut h, "settingsFound:general:");
    h.settle(T);
    for _ in 0..60 {
        h.step();
    }
    // Then the page is at its top again
    assert!(rect(&h, "heading:Library").unwrap().top() >= room.top() - 0.5, "the top of General is in view");
}

#[test]
fn return_opens_the_first_result() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "sidecar names");
    key(&mut h, "Enter");
    h.settle(T);
    steps(&mut h);
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
    assert!(headings(&h).contains(&"XMP sidecars".to_string()));
    assert!(found(&h).is_empty());
}

#[test]
fn esc_empties_the_filter_and_a_second_esc_closes_settings() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "import");
    filter(&mut h, "sidecar names");
    assert_eq!(found(&h).len(), 1);
    // When Esc is pressed
    key(&mut h, "Escape");
    // Then the filter is empty, the tab that was open shows its page again, and Settings is open
    assert!(found(&h).is_empty());
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
    assert!(headings(&h).contains(&"Raw defaults".to_string()));
    // And a second Esc closes Settings
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_none());
}

#[test]
fn opening_a_tab_empties_the_filter() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "sidecar names");
    click(&mut h, "button:settingsTab-performance");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "performance".into() }));
    assert!(found(&h).is_empty());
    assert!(headings(&h).contains(&"Rendering".to_string()), "Performance shows its page");
}

#[test]
fn looking_through_the_tabs_changes_nothing_and_leaves_no_double_names() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    // (what Settings sets: the app's own settings, the language, and what is saved with the library)
    let state = |h: &mut Headless| {
        (
            serde_json::to_value(&h.app.ui.settings).unwrap(),
            h.app.ui.language,
            h.app.ui.auto_advance,
            h.app.run("library.preferences", json!({})).unwrap(),
            h.app.run("library.xmpPreferences", json!({})).unwrap(),
            h.app.ui.dialog.clone(),
        )
    };
    let before = state(&mut h);
    h.app.ui.toast = None;
    filter(&mut h, "cache");
    // Then nothing changed by being looked through, and nothing was said
    assert_eq!(state(&mut h), before);
    assert!(h.app.ui.toast.is_none(), "{:?}", h.app.ui.toast);
    // And no control of a tab that is not shown is among the widgets scripts can click
    let mut ids: Vec<&String> = h.app.widgets.iter().map(|(id, _)| id).collect();
    assert!(!ids.iter().any(|id| id.as_str() == "button:settingsClearCache"), "a tab looked through off screen leaves no widgets behind");
    let all = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), all, "no widget is named twice");
}

#[test]
fn the_filter_is_empty_when_settings_is_opened_again() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "sidecar names");
    // (Esc once empties it; here Settings is closed from outside with the filter still set)
    h.app.ui.dialog = None;
    steps(&mut h);
    show(&mut h, "general");
    assert!(found(&h).is_empty() && !says(&h, "sidecar names"));
    assert!(headings(&h).contains(&"Startup".to_string()));
}

#[test]
fn what_a_page_shows_now_is_what_is_found() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "import");
    // Given a sentence Import only shows with camera-specific defaults on, looked for and not found
    filter(&mut h, "cameras appear");
    assert!(found(&h).is_empty());
    key(&mut h, "Escape");
    // When that is turned on, with Settings still open
    click(&mut h, "check:settings.perCamera");
    assert!(says(&h, "cameras appear"), "the page shows the sentence now");
    // Then looking for it again finds it: the pages are read afresh each time the filter is used
    filter(&mut h, "cameras appear");
    assert_eq!(found(&h), ["import:Raw defaults"]);
}

#[test]
fn the_arrow_goes_from_the_filter_to_what_was_found_and_return_opens_it() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "cache limit");
    assert_eq!(found(&h), ["performance:Thumbnail cache"]);
    // When ↓ is pressed in the filter (the focus goes to the first result), then Return
    key(&mut h, "ArrowDown");
    assert!(on_a_result(&h), "↓ leaves the filter for what was found");
    key(&mut h, "Enter");
    h.settle(T);
    steps(&mut h);
    // Then the section found is open
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "performance".into() }));
    assert!(found(&h).is_empty());
}

#[test]
fn esc_on_a_result_empties_the_filter_and_leaves_settings_open() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "import");
    filter(&mut h, "cache");
    assert!(!found(&h).is_empty());
    // Given the focus on a result, not in the filter
    key(&mut h, "ArrowDown");
    assert!(on_a_result(&h));
    // When Esc is pressed
    key(&mut h, "Escape");
    // Then the filter is emptied and Settings is still open on the tab it was on
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
    assert!(found(&h).is_empty() && headings(&h).contains(&"Raw defaults".to_string()));
    // And the next Esc closes Settings
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_none());
}

/// A section's heading goes in by its English name and is translated once: View Options' General
/// heading used to be passed already translated, so in another language it was translated again
/// and registered under the translated name, where the filter and scripts look for the English one.
#[test]
fn the_general_heading_of_view_options_is_translated_once_and_registered_by_its_english_name() {
    use crate::i18n::Locale;
    struct Restore(Locale);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::i18n::set_language(self.0);
        }
    }
    let _restore = Restore(crate::i18n::language());
    let mut h = demo([1000.0, 800.0]);
    h.app.ui.language = Locale::De;
    crate::i18n::set_language(Locale::De);
    h.app.ui.dialog = Some(Dialog::ViewOptions);
    steps(&mut h);
    let shown = Locale::De.tr("General");
    assert_ne!(shown, "General", "the German catalog translates the heading");
    let headings: Vec<&str> = h.app.widgets.iter().filter_map(|(id, _)| id.strip_prefix("heading:")).collect();
    assert!(headings.contains(&"General"), "registered by its English name: {headings:?}");
    assert!(!headings.contains(&shown), "not by the translated one: {headings:?}");
    assert!(h.painted_text().iter().any(|text| text == shown), "and drawn translated: {shown}");
}
