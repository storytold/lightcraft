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

/// A line of text as it was painted, with the fills painted behind it.
struct Line {
    text: egui::epaint::TextShape,
    /// (where, in what colour), left to right
    marks: Vec<(egui::Rect, egui::Color32)>,
}

impl Line {
    /// The letters that stand on each fill, in the order of the line's chars (a mark drawn on a
    /// letter is not counted beside it).
    fn told_apart(&self) -> Vec<String> {
        let marked = |mark: &egui::Rect| {
            // (a fill reaches a point past the letters at either end, and a glyph is placed on a
            // whole pixel: the middle of a space beside them may be just inside it)
            let mark = mark.shrink2(egui::vec2(1.5, 0.0));
            let rows = self.text.galley.rows.iter();
            rows.flat_map(|row| row.glyphs.iter().map(move |glyph| (self.text.pos + row.pos.to_vec2() + glyph.pos.to_vec2(), glyph)))
                .filter(|(at, glyph)| {
                    glyph.advance_width > 0.0 && mark.x_range().contains(at.x + glyph.advance_width / 2.0) && mark.y_range().contains(at.y)
                })
                .map(|(_, glyph)| glyph.chr)
                .collect()
        };
        self.marks.iter().map(|(mark, _)| marked(mark)).collect()
    }

    /// The colour the line is in.
    fn colour(&self) -> egui::Color32 {
        self.text.galley.job.sections.first().map_or(self.text.fallback_color, |section| section.format.color)
    }
}

/// The lines among `shapes` (in the order painted) that have `part` in their text.
fn lines_of(shapes: &[egui::epaint::Shape], part: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for (place, shape) in shapes.iter().enumerate() {
        let egui::epaint::Shape::Text(text) = shape else { continue };
        if !text.galley.job.text.contains(part) {
            continue;
        }
        // (a fill behind the line: painted before it, and no larger than the line is)
        let room = text.galley.rect.translate(text.pos.to_vec2()).expand(2.0);
        let mut marks: Vec<_> = shapes[..place]
            .iter()
            .filter_map(|shape| match shape {
                egui::epaint::Shape::Rect(rect) if room.contains_rect(rect.rect) && rect.fill.a() > 0 => Some((rect.rect, rect.fill)),
                _ => None,
            })
            .collect();
        marks.sort_by(|a, b| a.0.left().total_cmp(&b.0.left()));
        lines.push(Line { text: text.clone(), marks });
    }
    lines
}

fn lines(h: &Headless, part: &str) -> Vec<Line> {
    lines_of(&h.view.painted_shapes(), part)
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
    let lines = lines(&h, "colours");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].told_apart(), ["colours"], "\"colours\" is told apart for \"colors\"");
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
    // And the word is drawn apart from the words around it: it has a fill behind it, they have none
    let marked: Vec<_> = self::lines(&h, "wide-gamut").into_iter().filter(|line| line.text.galley.job.text != "wide-gamut").collect();
    assert_eq!(marked[0].told_apart(), ["wide-gamut"]);
}

#[test]
fn a_short_line_is_shown_whole_with_the_word_told_apart_whatever_its_case() {
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    filter(&mut h, "NAVIGATOR");
    assert_eq!(found(&h), ["interface:Detail View"]);
    let lines = lines(&h, "Show the Navigator while zoomed in");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text.galley.job.text, "Show the Navigator while zoomed in");
    assert_eq!(lines[0].told_apart(), ["Navigator"]);
}

#[test]
fn letters_found_inside_a_word_are_marked_behind_it_and_the_word_is_drawn_whole() {
    // Given a result found by letters in the middle of a word
    let mut h = demo([1200.0, 1000.0]);
    show(&mut h, "general");
    // When Settings draws the line under the result
    filter(&mut h, "aviga");
    assert_eq!(found(&h), ["interface:Detail View"]);
    let lines = lines(&h, "Show the Navigator while zoomed in");
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    // Then the line is one piece of text in one format: the word is not cut where the letters
    // found start and end (each piece would be shaped by itself: no kerning across the cut, and
    // in a joined script the letters on either side of it left unjoined)
    let job = &line.text.galley.job;
    let pieces: Vec<&str> = job.sections.iter().map(|section| &job.text[section.byte_range.start.0..section.byte_range.end.0]).collect();
    assert_eq!(pieces, ["Show the Navigator while zoomed in"], "the line is laid out as one piece");
    // And the letters found, and no others, have a fill behind them
    assert_eq!(line.told_apart(), ["aviga"]);
    // And the fill is within the line's row
    let row = line.text.galley.rect.translate(line.text.pos.to_vec2());
    let (mark, _) = line.marks[0];
    assert!(row.y_range().contains(mark.top()) && row.y_range().contains(mark.bottom()), "{mark:?} in {row:?}");
}

/// Scenario: the letters found are told apart by more than a colour: a line is drawn under them.
#[test]
fn the_letters_found_are_underlined_so_that_colour_is_not_the_only_sign() {
    use crate::theme::{ThemeKind, Tokens};
    for kind in ThemeKind::ALL {
        // Given a theme, and a result found by letters inside a word
        let mut h = demo([1200.0, 1000.0]);
        let r = h.request("engine.execute", json!({"command": "view.theme", "params": {"theme": kind.id()}}), T);
        assert_eq!(r["ok"], true, "{r}");
        let t = Tokens::for_kind(kind);
        show(&mut h, "general");
        // When Settings draws the line under the result
        filter(&mut h, "aviga");
        let shapes = h.view.painted_shapes();
        let lines = lines_of(&shapes, "Show the Navigator while zoomed in");
        assert_eq!(lines.len(), 1, "{kind:?}");
        let (mark, fill) = lines[0].marks[0];
        // Then a line runs under the letters found, from their first to their last, in the lower part of the row
        let under: Vec<_> = shapes
            .iter()
            .filter_map(|shape| match shape {
                egui::epaint::Shape::LineSegment { points, stroke } if (points[0].y - points[1].y).abs() < 0.01 => Some((points, stroke)),
                _ => None,
            })
            .filter(|(points, _)| mark.y_range().contains(points[0].y) && points[0].y > mark.center().y)
            .filter(|(points, _)| {
                (points[0].x.min(points[1].x) - mark.left()).abs() <= 1.5 && (points[0].x.max(points[1].x) - mark.right()).abs() <= 1.5
            })
            .collect();
        assert_eq!(under.len(), 1, "{kind:?}: one line under the letters found, {mark:?}");
        // And it can be seen without telling colours apart: 3:1 on the fill and on the row
        let stroke = under[0].1;
        assert!(stroke.width >= 1.0, "{kind:?}: {stroke:?}");
        for on in [fill, t.chrome, t.hover] {
            let ratio = crate::button::contrast(stroke.color, on);
            assert!(ratio >= 3.0, "{kind:?}: the line under the letters on {on:?} stands at {ratio}:1");
        }
    }
}

#[test]
fn the_letters_found_can_be_read_on_their_fill_in_every_theme() {
    use crate::theme::{ThemeKind, Tokens};
    for kind in ThemeKind::ALL {
        // Given a theme, and a result found by letters inside a word
        let mut h = demo([1200.0, 1000.0]);
        let r = h.request("engine.execute", json!({"command": "view.theme", "params": {"theme": kind.id()}}), T);
        assert_eq!(r["ok"], true, "{r}");
        let t = Tokens::for_kind(kind);
        show(&mut h, "general");
        filter(&mut h, "aviga");
        let lines = lines(&h, "Show the Navigator while zoomed in");
        let line = &lines[0];
        let (_, fill) = *line.marks.first().unwrap_or_else(|| panic!("{kind:?}: the letters found have a fill"));
        // Then the line, which is in one colour, stands 4.5:1 on the fill, as on a row with the
        // pointer or the focus on it, or neither
        let colour = line.colour();
        for (ground, what) in [(fill, "the fill"), (t.hover, "a hovered row"), (t.chrome, "a plain row")] {
            let ratio = crate::button::contrast(colour, ground);
            assert!(ratio >= 4.5, "{kind:?}: {colour:?} on {what} ({ground:?}) stands at {ratio}:1");
        }
        // And the fill is not the colour of a row, hovered or not: it is seen on both
        assert!(fill != t.hover && fill != t.chrome, "{kind:?}");
    }
}

#[test]
fn letters_found_inside_an_arabic_word_leave_its_letters_joined() {
    use crate::theme::Tokens;
    if !lightcraft_engine::CRAFT_FONTS.iter().any(|font| font.covers("Arab")) {
        lightcraft_testkit::skip!(Fonts, "no Arabic face in craft-fonts: nothing to check");
        return;
    }
    // Given a line with an Arabic word, found by letters in its middle (Settings has no Arabic
    // yet: the line is made by hand and drawn as the line under a result is)
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let t = Tokens::default();
    let line = "Preview المعاينة size";
    let at = line.find("معاين").unwrap();
    let found = at..at + "معاين".len();
    // When it is drawn
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 100.0));
    let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
        crate::panels::settings::found_line(ui, &t, line.to_string(), std::slice::from_ref(&found));
    });
    output.textures_delta.clear();
    let mut shapes = Vec::new();
    fn flat(shape: &egui::epaint::Shape, out: &mut Vec<egui::epaint::Shape>) {
        match shape {
            egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| flat(shape, out)),
            other => out.push(other.clone()),
        }
    }
    output.shapes.iter().for_each(|clipped| flat(&clipped.shape, &mut shapes));
    let lines = lines_of(&shapes, "المعاينة");
    assert_eq!(lines.len(), 1);
    let drawn = &lines[0].text.galley;
    // Then the line is as wide as the same line with nothing found in it: the word's letters
    // join one another as they do there (a word cut at the letters found is three pieces, each
    // with the forms letters have at a word's start and end, and is wider)
    let format = egui::TextFormat { font_id: t.font(11.0), ..Default::default() };
    let plain = ctx.fonts_mut(|fonts| fonts.layout_job(egui::text::LayoutJob::single_section(line.to_string(), format)));
    let glyphs = |galley: &egui::Galley| galley.rows.iter().map(|row| row.glyphs.len()).sum::<usize>();
    assert_eq!(glyphs(drawn), glyphs(&plain));
    assert!((drawn.size().x - plain.size().x).abs() < 0.01, "drawn {} wide, the plain line {}", drawn.size().x, plain.size().x);
    // And the fill is behind the letters found (a row's glyphs are listed in the order of the chars)
    assert_eq!(lines[0].told_apart(), ["معاين"]);
}

/// Scenario: a Persian word holds a zero-width non-joiner (it keeps two letters from joining). The
/// letters found in the word, and those found after it, are still the ones marked.
#[test]
fn a_non_joiner_inside_a_word_does_not_move_the_marks() {
    use crate::theme::Tokens;
    if !lightcraft_engine::CRAFT_FONTS.iter().any(|font| font.covers("Arab")) {
        lightcraft_testkit::skip!(Fonts, "no Arabic face in craft-fonts: nothing to check");
        return;
    }
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let t = Tokens::default();
    // Given a line with such a word and a Latin word after it
    let line = "می\u{200c}خواهم ok";
    let told = |part: &str| {
        let at = line.find(part).unwrap();
        let found = at..at + part.len();
        // When it is drawn with `part` found in it
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 100.0));
        let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
            crate::panels::settings::found_line(ui, &t, line.to_string(), std::slice::from_ref(&found));
        });
        output.textures_delta.clear();
        fn flat(shape: &egui::epaint::Shape, out: &mut Vec<egui::epaint::Shape>) {
            match shape {
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| flat(shape, out)),
                other => out.push(other.clone()),
            }
        }
        let mut shapes = Vec::new();
        output.shapes.iter().for_each(|clipped| flat(&clipped.shape, &mut shapes));
        let lines = lines_of(&shapes, "ok");
        assert_eq!(lines.len(), 1);
        lines[0].told_apart()
    };
    // Then the Latin word after it is the one marked, not the letters a place before it
    assert_eq!(told("ok"), ["ok"]);
    // And letters found in the word itself are marked
    assert_eq!(told("خواه"), ["خواه"]);
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
