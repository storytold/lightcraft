//! The filter of Settings finds what people type, puts the best match first and shows why it
//! matched: words a section does not say itself ("trash" for Culling), either spelling
//! of a word (color, colour), a section named for the word before one that only mentions it,
//! the match in the middle of the line under a result and told apart from the rest, no folder
//! paths among what is looked through, the box ready to type in when Settings opens, and the
//! section landed on marked for a moment.
//!
//! Before, "color" and "trash" found nothing, "detail" + Return opened General ▸ Startup (the
//! first tab that says it) and not Interface ▸ Detail View, a long explanation was cut off before
//! the word it was shown for, the name of the library's folder was a search word, the box had to
//! be clicked first, and nothing said which section of the opened tab had been chosen.

use std::path::PathBuf;
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

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// What the filter lists, top to bottom: `tab:section` of each result.
fn found(h: &Headless) -> Vec<String> {
    let mut found: Vec<(f32, String)> =
        h.app.widgets.iter().filter_map(|(id, r)| Some((r.top(), id.strip_prefix("settingsFound:")?.to_string()))).collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.into_iter().map(|(_, id)| id).collect()
}

/// The galleys painted that have `part` in their text.
fn galleys(h: &Headless, part: &str) -> Vec<std::sync::Arc<egui::Galley>> {
    h.view
        .painted_shapes()
        .into_iter()
        .filter_map(|shape| match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text.contains(part) => Some(text.galley.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_word_people_type_for_a_section_finds_it_though_the_section_does_not_say_it() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    // "trash": Culling says "Recently Deleted"
    filter(&mut h, "trash");
    assert_eq!(found(&h), ["general:Culling"]);
    // "theme" is said by Appearance; "scheme" is not
    filter(&mut h, "scheme");
    assert_eq!(found(&h), ["interface:Appearance"]);
    // And such a word counts with the others typed: every word still has to be there
    filter(&mut h, "trash confirm");
    assert_eq!(found(&h), ["general:Culling"]);
    filter(&mut h, "trash gpu");
    assert!(found(&h).is_empty());
    // And the start of such a word finds it, as part of any word does: the section stays found
    // while the word is typed
    filter(&mut h, "tras");
    assert_eq!(found(&h), ["general:Culling"]);
    // And a section that says the word comes before one that is only given it
    filter(&mut h, "metadata");
    let found = found(&h);
    let place = |id: &str| found.iter().position(|f| f == id).unwrap_or_else(|| panic!("{id} in {found:?}"));
    assert!(place("import:Metadata") < place("import:XMP sidecars"), "{found:?}");
}

#[test]
fn either_spelling_of_a_word_finds_it() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    // Display ▸ Monitor profile says "colours"
    filter(&mut h, "colours");
    let british = found(&h);
    assert!(british.contains(&"display:Monitor profile".to_string()), "{british:?}");
    filter(&mut h, "colors");
    assert_eq!(found(&h), british, "color finds what colour finds");
    // And the word is told apart in the line under the result, in the spelling the line has
    let t = crate::theme::Tokens::get(&h.view.ctx);
    let lines: Vec<_> = galleys(&h, "colours").into_iter().collect();
    assert_eq!(lines.len(), 1);
    let at = lines[0].job.text.find("colours").unwrap();
    let colour =
        |byte: usize| lines[0].job.sections.iter().find(|s| (s.byte_range.start.0..s.byte_range.end.0).contains(&byte)).unwrap().format.color;
    assert_eq!((colour(at), colour(at + 6)), (t.text, t.text), "\"colours\" is told apart for \"colors\"");
    // And on the way to either spelling, letter by letter, the section stays found
    // (Monitor profile says "colours"; External Editing and Appearance are given "color")
    for typed in ["col", "colo", "colou", "colour", "color"] {
        filter(&mut h, typed);
        for section in ["display:Monitor profile", "general:External Editing", "interface:Appearance"] {
            assert!(found(&h).contains(&section.to_string()), "{typed} finds {section}: {:?}", found(&h));
        }
    }
    // And with another word beside it
    for typed in ["colo space", "colou space", "colour space"] {
        filter(&mut h, typed);
        assert_eq!(found(&h), ["general:External Editing"], "{typed}");
    }
    // And for a word Settings has in the one spelling only (Library is given "catalog")
    for typed in ["catalo", "catalog", "catalogu", "catalogue"] {
        filter(&mut h, typed);
        assert!(found(&h).contains(&"general:Library".to_string()), "{typed}: {:?}", found(&h));
    }
}

#[test]
fn a_section_named_for_the_word_comes_before_one_that_mentions_it() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    // "detail": the name of a section of Interface, a choice of General ▸ Startup, and a word
    // in an explanation of Performance ▸ Rendering and of Display ▸ Monitor profile
    filter(&mut h, "detail");
    assert_eq!(found(&h), ["interface:Detail View", "general:Startup", "performance:Rendering", "display:Monitor profile"]);
    // And Return opens the best match, not the first tab that has the word
    key(&mut h, "Enter");
    h.settle(T);
    steps(&mut h);
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "interface".into() }));
}

#[test]
fn sections_that_match_alike_stay_in_the_order_of_the_tabs() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    // "never" is a choice in Performance ▸ Local folders, Interface ▸ Grid and two sections of
    // Slideshow, a name of none of them
    filter(&mut h, "never");
    assert_eq!(found(&h), ["performance:Local folders", "interface:Grid", "slideshow:Playback", "slideshow:Titles"]);
}

#[test]
fn the_line_under_a_result_shows_the_word_it_was_found_by_and_tells_it_apart() {
    use crate::theme::Tokens;
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    // "wide-gamut" stands far into the explanation under Display ▸ Monitor profile
    filter(&mut h, "wide-gamut");
    assert_eq!(found(&h), ["display:Monitor profile"]);
    // (the filter box says the word too: the line under the result is the one with more to it)
    let lines: Vec<_> = galleys(&h, "wide-gamut").into_iter().filter(|line| line.job.text != "wide-gamut").collect();
    assert_eq!(lines.len(), 1, "one line says it");
    let line = &lines[0];
    // Then the line starts shortly before the word, with a sign that something was left out
    let text = &line.job.text;
    assert!(text.starts_with('…'), "{text}");
    assert!(text.find("wide-gamut").unwrap() < 60, "the word is near the start of what is shown: {text}");
    // And the word is in sight: the line is cut after it, not before
    let laid_out: String = line.rows.iter().flat_map(|row| row.glyphs.iter().map(|glyph| glyph.chr)).collect();
    assert!(laid_out.contains("wide-gamut"), "what is drawn of the line has the word: {laid_out}");
    // And the word is drawn apart from the words around it: in the text colour, the rest dimmer
    let t = Tokens::get(&h.view.ctx);
    let at = text.find("wide-gamut").unwrap();
    let of = |byte: usize| line.job.sections.iter().find(|s| (s.byte_range.start.0..s.byte_range.end.0).contains(&byte)).unwrap().format.clone();
    assert_eq!(of(at).color, t.text, "the word found is in the text colour");
    assert_ne!(of(at).color, of(at - 2).color, "and what stands before it is not");
    assert_eq!(of(at + "wide-gamut".len() - 1).color, t.text);
    assert_ne!(of(at + "wide-gamut".len() + 1).color, t.text);
}

#[test]
fn a_short_line_is_shown_whole_with_the_word_told_apart_whatever_its_case() {
    use crate::theme::Tokens;
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "NAVIGATOR");
    assert_eq!(found(&h), ["interface:Detail View"]);
    let lines = galleys(&h, "Show the Navigator while zoomed in");
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert_eq!(line.job.text, "Show the Navigator while zoomed in");
    let t = Tokens::get(&h.view.ctx);
    let at = line.job.text.find("Navigator").unwrap();
    let colour = |byte: usize| line.job.sections.iter().find(|s| (s.byte_range.start.0..s.byte_range.end.0).contains(&byte)).unwrap().format.color;
    assert_eq!(colour(at), t.text);
    assert_ne!(colour(0), t.text);
}

/// A folder under the system's temporary one, removed again when the test ends, passed or not.
struct TempFolder(PathBuf);

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_names_of_folders_are_not_looked_through() {
    // Given a library on disk in a folder with a word of its own in its name
    let dir = TempFolder(std::env::temp_dir().join(format!("lc-ui-filterwords-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.0.join("zebrafolder"), false).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1200.0, 1000.0], 1.0);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "general".into() });
    steps(&mut h);
    assert!(h.painted_text().iter().any(|text| text.contains("zebrafolder")), "General ▸ Library shows the folder");
    // When a word of the folder's name is typed
    filter(&mut h, "zebrafolder");
    // Then nothing is found: where a library happens to be is not a setting's name
    assert!(found(&h).is_empty(), "{:?}", found(&h));
    // And the row that shows it is still found by what it is called
    filter(&mut h, "location");
    assert_eq!(found(&h).first().map(String::as_str), Some("general:Library"));
}

#[test]
fn the_name_of_a_watched_folder_given_without_a_separator_is_not_looked_through() {
    // Given Auto Import watching a folder given as a bare name (relative to where the app runs)
    let mut h = demo([1200.0, 1000.0]);
    h.app.session.import_defaults.auto_folder = Some("zebrafolder".into());
    show(&mut h, "import");
    filter(&mut h, "watched folder");
    assert_eq!(found(&h).first().map(String::as_str), Some("import:Auto Import"), "{:?}", found(&h));
    click(&mut h, "settingsFound:import:Auto Import");
    assert!(has(&h, "text:settingsAutoFolder"), "Import ▸ Auto Import shows the folder");
    // When its name is typed
    filter(&mut h, "zebrafolder");
    // Then nothing is found: a folder is not a setting's name, with a separator in it or without
    assert!(found(&h).is_empty(), "{:?}", found(&h));
    h.app.session.import_defaults.auto_folder = None;
}

#[test]
fn what_stands_where_a_folder_would_is_still_found_when_it_is_no_folder() {
    // Given the demo, whose library is in memory: General ▸ Library says so where a folder would stand
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    assert!(h.painted_text().iter().any(|text| text.contains("nothing is saved")));
    filter(&mut h, "nothing is saved");
    assert_eq!(found(&h), ["general:Library"]);
}

#[test]
fn what_is_typed_into_a_box_of_settings_is_not_looked_through() {
    let mut h = demo([1200.0, 1000.0]);
    // Given an editor of the person's own, in a folder with a word of its own
    h.app.ui.settings.external_editor = "/opt/zebraedit/bin/zed".into();
    show(&mut h, "general");
    // (External Editing is far down General: choose it from the filter, which brings it into view)
    filter(&mut h, "external editing");
    click(&mut h, "settingsFound:general:External Editing");
    assert!(h.painted_text().iter().any(|text| text.contains("zebraedit")));
    // Then that folder is not a word a section is found by
    filter(&mut h, "zebraedit");
    assert!(found(&h).is_empty(), "{:?}", found(&h));
    // And "bin" finds Culling (where photos are thrown away), not the editor's folder
    filter(&mut h, "bin");
    assert_eq!(found(&h), ["general:Culling"]);
}

#[test]
fn the_filter_is_ready_to_type_in_when_settings_opens() {
    let mut h = demo([1200.0, 1000.0]);
    // When Settings is opened and something is typed, without a click
    show(&mut h, "general");
    assert_eq!(h.view.ctx.memory(|m| m.focused()), Some(crate::text_field::id("field:settingsFilter")));
    h.request("ui.text", json!({"text": "culling"}), T);
    steps(&mut h);
    // Then it went into the filter
    assert_eq!(found(&h), ["general:Culling"]);
    // And a tab opened afterwards does not take the focus back to the box
    click(&mut h, "button:settingsTab-performance");
    assert_ne!(h.view.ctx.memory(|m| m.focused()), Some(crate::text_field::id("field:settingsFilter")));
    // And Esc in the empty box, as it opens, closes Settings in one press
    key(&mut h, "Escape");
    h.app.ui.dialog = None;
    steps(&mut h);
    show(&mut h, "general");
    key(&mut h, "Escape");
    assert_eq!(h.app.ui.dialog, None);
}

/// Given Settings with a word typed in the filter, when a quit prompt comes up over it and is
/// answered, then the word is still in the filter: Settings did not close, and the passes the
/// prompt takes for itself are not a reopening, which would empty the box and put the focus
/// back in it (taking it from the prompt's Cancel).
#[test]
fn a_prompt_over_settings_does_not_empty_the_filter_or_take_its_focus() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "culling");
    assert_eq!(found(&h), ["general:Culling"]);
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    steps(&mut h);
    assert_ne!(h.view.ctx.memory(|m| m.focused()), Some(crate::text_field::id("field:settingsFilter")), "the prompt has the focus");
    assert!(has(&h, "button:quitCancel"), "the prompt is up");
    let r = h.request("ui.clickWidget", json!({"id": "button:quitCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    assert!(h.app.quit_prompt.is_none(), "Cancel dismissed the prompt");
    assert_eq!(found(&h), ["general:Culling"], "what was typed is still in the filter");
}

#[test]
fn the_section_landed_on_is_marked_for_a_moment() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    assert!(!has(&h, "settingsLanded:Culling"));
    filter(&mut h, "info overlay");
    click(&mut h, "settingsFound:interface:Detail View");
    steps(&mut h);
    // Then the heading of the section chosen is marked, and no other
    assert!(has(&h, "settingsLanded:Detail View"), "{:?}", h.app.widgets.iter().map(|(w, _)| w).filter(|w| w.contains("Landed")).collect::<Vec<_>>());
    assert!(!has(&h, "settingsLanded:Grid") && !has(&h, "settingsLanded:Appearance"));
    // And leaving the tab and coming back to it shows no mark: it was for arriving
    click(&mut h, "button:settingsTab-general");
    click(&mut h, "button:settingsTab-interface");
    assert!(!has(&h, "settingsLanded:Detail View"), "a tab opened by its own button marks nothing");
    // And nor does leaving by a command, or closing Settings and opening it again
    filter(&mut h, "info overlay");
    click(&mut h, "settingsFound:interface:Detail View");
    steps(&mut h);
    assert!(has(&h, "settingsLanded:Detail View"));
    show(&mut h, "general");
    show(&mut h, "interface");
    assert!(!has(&h, "settingsLanded:Detail View"), "a tab opened by a command marks nothing");
    filter(&mut h, "info overlay");
    click(&mut h, "settingsFound:interface:Detail View");
    steps(&mut h);
    assert!(has(&h, "settingsLanded:Detail View"));
    h.app.ui.dialog = None;
    steps(&mut h);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "interface".into() });
    steps(&mut h);
    assert!(!has(&h, "settingsLanded:Detail View"), "Settings opened again marks nothing");
    // And the mark goes by itself
    filter(&mut h, "info overlay");
    click(&mut h, "settingsFound:interface:Detail View");
    steps(&mut h);
    assert!(has(&h, "settingsLanded:Detail View"));
    for _ in 0..240 {
        h.step();
    }
    assert!(!has(&h, "settingsLanded:Detail View"), "the mark is gone after a few seconds");
    // And a tab opened by its own button marks nothing
    click(&mut h, "button:settingsTab-general");
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("settingsLanded:")));
}

#[test]
fn every_word_given_to_a_section_finds_that_section() {
    // Given a library on disk (Performance ▸ Smart previews is only there for one)
    let dir = TempFolder(std::env::temp_dir().join(format!("lc-ui-filteralso-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.0.join("library"), false).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1200.0, 1000.0], 1.0);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "general".into() });
    steps(&mut h);
    for (tab, section, words) in crate::panels::settings::ALSO {
        for word in words.split_whitespace() {
            // Then each word finds the section it is given to: the section is drawn on that tab
            // under that name, and the word is in the spelling the filter compares in
            filter(&mut h, word);
            assert!(found(&h).contains(&format!("{tab}:{section}")), "\"{word}\" finds {tab} ▸ {section}: {:?}", found(&h));
            // And is not a part of a word half of Settings says, which would bury the section
            assert!(found(&h).len() <= 6, "\"{word}\" finds too much to be of use: {:?}", found(&h));
        }
    }
}
