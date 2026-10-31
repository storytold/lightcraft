//! The Keywording box at the top of the right panel's Keywords: the selection's keywords, driven
//! through the control channel.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The Library grid with the Keywords panel open and two photos selected: the first with beach and
/// Weddings, the second with beach.
fn two_selected() -> (Headless, Vec<u64>) {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    for (method, params) in [
        ("ui.set", json!({"view": "photoGrid", "right": "keywords"})),
        ("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[0]], "keywords": ["beach", "Weddings"]}})),
        ("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[1]], "keywords": ["beach"]}})),
        ("engine.execute", json!({"command": "library.select", "params": {"ids": ids}})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    (h, ids)
}

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn keywords_of(h: &Headless, id: u64) -> Vec<String> {
    h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).unwrap().meta.keywords.clone()
}

/// The chips are the keywords of every selected photo, not only the active one's; a keyword only
/// some of them have is marked.
#[test]
fn the_chips_are_the_selections_keywords() {
    let (h, _) = two_selected();
    assert!(
        has(&h, "keywordChip:beach") && has(&h, "keywordChip:Weddings"),
        "{:?}",
        h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordChip")).collect::<Vec<_>>()
    );
    assert!(has(&h, "keywordChipPartial:Weddings"), "only one of the two has Weddings");
    assert!(!has(&h, "keywordChipPartial:beach"), "both have beach");
}

/// Only the × removes a keyword, from every selected photo; a click on the chip itself doesn't.
#[test]
fn only_the_cross_removes_a_keyword() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChip:beach"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"beach".to_string())), "a click on the chip keeps it");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipRemove:beach"}));
    assert!(ids.iter().all(|id| !keywords_of(&h, *id).contains(&"beach".to_string())), "× took it off both");
}

/// A chip's menu acts on the selection, never on the whole library: give a keyword only some have
/// to all of them, take it off them, show its photos, or edit it (Edit Keyword Tag). Deleting a
/// keyword from every photo of the library is the Keyword List's.
#[test]
fn a_chips_menu_acts_on_the_selection() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChip:Weddings", "button": "right"}));
    for item in ["add", "remove", "show", "edit"] {
        assert!(has(&h, &format!("keywordChipMenu:{item}")), "{item}");
    }
    assert!(!has(&h, "keywordChipMenu:delete"), "no library-wide delete");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipMenu:add"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"Weddings".to_string())), "given to both");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChip:Weddings", "button": "right"}));
    assert!(!has(&h, "keywordChipMenu:add"), "both have it now");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipMenu:edit"}));
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::KeywordTag { editing: Some(k), .. }) if k == "Weddings"), "{:?}", h.app.ui.dialog);
}

/// The "Add keyword" box is a shared text field: right-click ▸ Paste fills it, Return gives the
/// keywords to every selected photo, and Esc gives up what was typed.
#[test]
fn the_add_keyword_box_pastes_adds_and_gives_up() {
    let (mut h, ids) = two_selected();
    h.view.clipboard = "travel, sunset".into();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword:paste"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    for id in &ids {
        let k = keywords_of(&h, *id);
        assert!(k.contains(&"travel".to_string()) && k.contains(&"sunset".to_string()), "{k:?}");
    }
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.text", json!({"text": "harbour"}));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(ids.iter().all(|id| !keywords_of(&h, *id).contains(&"harbour".to_string())), "Esc gave it up");
}

/// ⌘K (Ctrl+K) is Add Keywords: it shows the Keywords panel, opens Keywording when it is folded
/// shut, and puts the cursor in the Add keyword box, so what is typed next goes to the selection.
#[test]
fn cmd_k_puts_the_cursor_in_the_add_keyword_box() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywording"}));
    ask(&mut h, "ui.set", json!({"right": "none"}));
    ask(&mut h, "ui.key", json!({"key": "K", "cmd": true}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Keywords, "the Keywords panel");
    assert!(!h.app.ui.section_folded("keywording") && has(&h, "field:keyword"), "Keywording is open");
    assert!(h.view.ctx.memory(|m| m.has_focus(crate::text_field::id("field:keyword"))), "the box has the cursor");
    ask(&mut h, "ui.text", json!({"text": "harbour"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"harbour".to_string())), "typed and given to both");
    assert!(!h.view.ctx.memory(|m| m.has_focus(crate::text_field::id("field:keyword"))), "Return gave up the cursor");
    ask(&mut h, "ui.key", json!({"key": "K", "cmd": true}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Keywords, "pressed again: the panel stays");
    assert!(h.view.ctx.memory(|m| m.has_focus(crate::text_field::id("field:keyword"))), "and the cursor is back in the box");
}

/// With no photo, ⌘K shows the Keywords panel and its note; selecting photos afterwards doesn't put the
/// cursor in the Add keyword box.
#[test]
fn cmd_k_without_a_photo_leaves_the_cursor_alone() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": []}}));
    assert!(h.app.session.active().is_none(), "no active photo");
    ask(&mut h, "ui.set", json!({"right": "none"}));
    ask(&mut h, "ui.key", json!({"key": "K", "cmd": true}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Keywords, "the Keywords panel");
    assert!(!has(&h, "field:keyword"), "no box without a photo");
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": ids}}));
    assert!(has(&h, "field:keyword"));
    assert!(!h.view.ctx.memory(|m| m.has_focus(crate::text_field::id("field:keyword"))), "the box waits for a click");
}

/// An Add Keywords request the box wasn't drawn for in time (the panel couldn't show) expires: a
/// later draw of the box leaves the cursor alone.
#[test]
fn a_stale_add_keywords_request_leaves_the_cursor_alone() {
    let (mut h, _) = two_selected();
    ask(&mut h, "ui.set", json!({"right": "keywords"}));
    assert!(has(&h, "field:keyword"));
    h.app.ui.focus_keywords = Some(h.view.ctx.cumulative_pass_nr().saturating_sub(crate::state::FOCUS_KEYWORDS_PASSES + 1));
    h.step();
    assert_eq!(h.app.ui.focus_keywords, None, "the request is used up");
    assert!(!h.view.ctx.memory(|m| m.has_focus(crate::text_field::id("field:keyword"))), "the box waits for a click");
}

/// The keyword set's buttons say whether the selection has each keyword, as their ⌥1–⌥9 toggle it
/// on the selection: on when every selected photo has it, partly on when only some do.
#[test]
fn keyword_set_buttons_reflect_the_selection() {
    let (mut h, _) = two_selected();
    let sets = h.app.session.execute("keyword.sets", &json!({})).unwrap();
    let at = |k: &str| sets["keywords"].as_array().unwrap().iter().position(|x| x == k).map(|i| i + 1).unwrap_or_else(|| panic!("{k} in {sets}"));
    assert!(has(&h, &format!("kwSetOn:{}", at("beach"))), "both have beach");
    assert!(has(&h, &format!("kwSetSome:{}", at("Weddings"))), "only the active one has Weddings");
    assert!(!has(&h, &format!("kwSetOn:{}", at("Weddings"))));
}

/// "Will Export" shows the names exported files will carry for the selection instead of the chips:
/// a keyword left out of export isn't there; "Keywords" brings the chips back.
#[test]
fn will_export_shows_what_exports_carry() {
    let (mut h, _) = two_selected();
    let r = h.request("engine.execute", json!({"command": "keyword.edit", "params": {"keyword": "Weddings", "includeOnExport": false}}), T);
    assert_eq!(r["ok"], true, "{r}");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordView:willExport"}));
    assert!(has(&h, "keywordExport:beach"));
    assert!(!has(&h, "keywordExport:Weddings"), "left out of export");
    assert!(!has(&h, "keywordChip:beach"), "the chips give way");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordView:keywords"}));
    assert!(has(&h, "keywordChip:beach") && !has(&h, "keywordExport:beach"));
}

/// The painter's keyword field is a shared text field: right-click ▸ Paste fills it, and Return
/// starts painting as the Paint button does.
#[test]
fn the_painters_field_pastes_and_return_paints() {
    let (mut h, _) = two_selected();
    h.view.clipboard = "Weddings".into();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter:paste"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.keyword_painter.as_deref(), Some("Weddings"));
}

/// ⌘⌥K (Ctrl+Alt+K) is Enable Painting: with the painter's box empty it shows the Keywords panel to
/// type a keyword in (unfolding the Keywords section when it was folded shut), with a keyword in the
/// box it paints that keyword, and pressed again it stops.
#[test]
fn cmd_alt_k_paints_the_painters_keyword_and_stops() {
    let (mut h, _) = two_selected();
    let key = json!({"key": "K", "cmd": true, "alt": true});
    ask(&mut h, "ui.set", json!({"right": "none"}));
    h.app.ui.toggle_folded("keywording");
    ask(&mut h, "ui.key", key.clone());
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Keywords, "an empty box: the Keywords panel");
    assert!(!h.app.ui.section_folded("keywording") && has(&h, "field:keywordPainter"), "the painter's box shows");
    assert!(h.app.ui.keyword_painter.is_none(), "nothing to paint yet");
    h.view.clipboard = "Weddings".into();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter:paste"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordView:keywords"}));
    ask(&mut h, "ui.key", key.clone());
    assert_eq!(h.app.ui.keyword_painter.as_deref(), Some("Weddings"), "the box's keyword");
    ask(&mut h, "ui.key", key);
    assert!(h.app.ui.keyword_painter.is_none(), "pressed again: painting stops");
    assert_eq!(h.app.ui.keyword_painter_text, "Weddings", "the box keeps the keyword");
}

/// In the full-screen preview, ⌘⌥K leaves the preview first: with an empty box the Keywords panel
/// shows, and with a keyword in the box the grid shows to paint on.
#[test]
fn cmd_alt_k_leaves_the_full_screen_preview() {
    let (mut h, _) = two_selected();
    let key = json!({"key": "K", "cmd": true, "alt": true});
    h.app.run("view.fullScreenPreview", json!({})).unwrap();
    assert!(h.app.ui.fullscreen);
    ask(&mut h, "ui.key", key.clone());
    assert!(!h.app.ui.fullscreen, "an empty box: the preview ends");
    assert!(has(&h, "field:keywordPainter"), "the painter's box shows");
    h.app.ui.keyword_painter_text = "Weddings".into();
    h.app.run("view.fullScreenPreview", json!({})).unwrap();
    assert!(h.app.ui.fullscreen);
    ask(&mut h, "ui.key", key);
    assert!(!h.app.ui.fullscreen, "a keyword in the box: the preview ends");
    assert_eq!(h.app.ui.keyword_painter.as_deref(), Some("Weddings"));
}

/// ⌘⌥K with an empty box turns off a targeted-adjustment tool as it opens the Keywords panel, as
/// opening the panel any other way does.
#[test]
fn cmd_alt_k_turns_off_a_targeted_adjustment_tool() {
    let (mut h, _) = two_selected();
    ask(&mut h, "ui.set", json!({"view": "detail", "right": "edit"}));
    h.app.ui.tool = "tat:curve".into();
    ask(&mut h, "ui.key", json!({"key": "K", "cmd": true, "alt": true}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Keywords);
    assert!(h.app.ui.tool.is_empty(), "the tool is off: {:?}", h.app.ui.tool);
}

/// Suggestions follow the selection: a keyword only some selected photos have is suggested, to give
/// it to all of them; one every selected photo has isn't.
#[test]
fn suggestions_follow_the_selection() {
    let (h, _) = two_selected();
    assert!(has(&h, "kwSuggest:Weddings"), "only the active one has it");
    assert!(!has(&h, "kwSuggest:beach"), "both have it");
}

/// Chips wrap whole and stay inside the panel: none overlaps another or runs past the panel's
/// edge, and one longer than the panel is shortened to fit (a chip laid out in what was left of a
/// line used to wrap its own text, a letter a line, over the others).
#[test]
fn chips_wrap_whole_inside_the_panel() {
    let (mut h, ids) = two_selected();
    let long = "Places|Portugal|Lisbon|Belém tower and the old town square on the river";
    let r = h.request(
        "engine.execute",
        json!({"command": "photo.setMeta", "params": {"ids": ids, "addKeywords": ["Events|Weddings", "sunset", "Places|Portugal|Lisbon", "harbour", long]}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let panel = h.app.widgets.iter().find(|(w, _)| w == "panel:right_panel").map(|(_, r)| *r).expect("the right panel");
    let chips: Vec<(String, egui::Rect)> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordChip:")).cloned().collect();
    assert!(chips.len() >= 6, "{chips:?}");
    for (i, (id, r)) in chips.iter().enumerate() {
        assert!(r.height() < 30.0, "{id} on one line: {r:?}");
        assert!(r.left() >= panel.left() && r.right() <= panel.right(), "{id} inside the panel: {r:?} in {panel:?}");
        for (other, o) in chips.iter().skip(i + 1) {
            assert!(!r.intersects(o.shrink(0.5)), "{id} overlaps {other}");
        }
    }
}

/// With an active photo and nothing else selected, the box is that photo's, as the commands are:
/// its chips show (keywords typed go to it), and the Keyword List ticks what it has.
#[test]
fn the_active_photo_alone_is_the_selection() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [], "active": ids[0]}}));
    assert_eq!(h.app.session.selection.active.map(|p| p.0), Some(ids[0]));
    assert!(h.app.session.selection.ids.is_empty());
    assert!(has(&h, "keywordChip:Weddings") && !has(&h, "keywordChipPartial:Weddings"), "the active photo's keywords");
    let ticks = crate::panels::keyword_list::Ticks::of(&h.app.session.catalog, &h.app.session.selected());
    assert_eq!(ticks.tick("Weddings"), crate::panels::keyword_list::Tick::All);
    assert!(has(&h, "keywordCheck:beach"));
}

/// Given a selection with a keyword that is inside others, when the Keyword List is shown, then
/// every keyword above it carries a dot in its box (also folded, where the keyword itself is out
/// of sight), and its own box is the one that is ticked. A keyword above it that the selection
/// has itself shows its tick and no dot; a keyword with nothing applied inside shows neither.
#[test]
fn a_keyword_shows_that_one_inside_it_is_applied() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": ids, "addKeywords": ["travel|Italy|Rome"]}}));
    h.settle(SETTLE);
    // folded: `travel` is the only row of the three, and says there is something inside
    assert!(has(&h, "keywordRow:travel") && !has(&h, "keywordRow:travel|Italy"), "folded");
    assert!(has(&h, "keywordInside:travel"));
    assert!(!has(&h, "keywordInside:beach") && !has(&h, "keywordInside:Weddings"), "nothing is inside these");
    // the dot sits in the keyword's own box
    let rect = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap();
    assert!(rect(&h, "keywordCheck:travel").contains_rect(rect(&h, "keywordInside:travel")));
    // opened down to it: each level above has the dot, the keyword itself the tick
    h.app.ui.keyword_list_open.extend(["travel".to_string(), "travel|italy".to_string()]);
    h.settle(SETTLE);
    assert!(has(&h, "keywordInside:travel") && has(&h, "keywordInside:travel|Italy"));
    assert!(has(&h, "keywordRow:travel|Italy|Rome") && !has(&h, "keywordInside:travel|Italy|Rome"));
    // a keyword the selection has itself shows its own state: the tick, or the dash when only some photos have it
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": ids, "addKeywords": ["travel"]}}));
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[0]], "addKeywords": ["travel|Italy"]}}));
    h.settle(SETTLE);
    assert!(!has(&h, "keywordInside:travel") && !has(&h, "keywordInside:travel|Italy"));
    // with nothing selected nothing is applied, inside or not
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": []}}));
    h.settle(SETTLE);
    assert!(has(&h, "keywordRow:travel") && !h.app.widgets.iter().any(|(w, _)| w.starts_with("keywordInside:")));
}

/// Esc in the Add keyword box gives back what it held before this edit, as every text field does:
/// text left there by an earlier edit stays.
#[test]
fn escape_gives_back_the_text_from_before_the_edit() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.text", json!({"text": "harbour"}));
    ask(&mut h, "ui.key", json!({"key": "Tab"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "End"}));
    ask(&mut h, "ui.text", json!({"text": ", sea"}));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    let k = keywords_of(&h, ids[0]);
    assert!(k.contains(&"harbour".to_string()) && !k.contains(&"sea".to_string()), "{k:?}");
}

/// "Keywords & Containing Keywords" lists the selection's keywords with the keywords containing
/// them, flat (read only).
#[test]
fn containing_keywords_view_lists_parents_too() {
    let (mut h, ids) = two_selected();
    let r = h.request("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[0]], "addKeywords": ["Events|Birthdays"]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordView:containing"}));
    for name in ["Events", "Birthdays", "beach"] {
        assert!(has(&h, &format!("keywordContaining:{name}")), "{name}");
    }
    assert!(!has(&h, "keywordChip:beach"), "the chips give way");
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn count(h: &Headless, prefix: &str) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with(prefix)).count()
}

/// Give both selected photos `keywords`, replacing what they had.
fn give(h: &mut Headless, ids: &[u64], keywords: &[String]) {
    ask(h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": ids, "keywords": keywords}}));
}

/// Sixteen keywords inside "Subjects", each long enough to take a line of the panel.
fn sixteen() -> Vec<String> {
    (1..=16).map(|i| format!("Subjects|a keyword with quite a long name {i:02}")).collect()
}

// Given keywords inside the same keyword and the box set to group them,
// when the panel is drawn,
// then they share a line naming that keyword and their chips say only what is below it, side by
// side where they fit; each chip still acts on its whole keyword.
#[test]
fn grouped_keywords_share_a_line_naming_their_parent() {
    let (mut h, ids) = two_selected();
    let keywords: Vec<String> = ["Places|Italy|Rome", "Places|Italy|Venice", "Transport|Ferry", "sea"].iter().map(|s| s.to_string()).collect();
    give(&mut h, &ids, &keywords);
    assert_eq!(
        count(&h, "keywordGroup:"),
        0,
        "whole names unless asked: {:?}",
        h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordGroup")).collect::<Vec<_>>()
    );
    let whole = rect(&h, "keywordChip:Places|Italy|Rome").width();

    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    h.settle(SETTLE);
    let heading = rect(&h, "keywordGroup:Places|Italy");
    let (rome, venice) = (rect(&h, "keywordChip:Places|Italy|Rome"), rect(&h, "keywordChip:Places|Italy|Venice"));
    assert!(rome.top() >= heading.bottom(), "under its heading: {rome:?} {heading:?}");
    assert_eq!(rome.top(), venice.top(), "side by side");
    assert!(rome.width() < whole, "the chip says only “Rome”: {} < {whole}", rome.width());
    // a keyword that shares nothing has no heading and comes first
    assert!(!has(&h, "keywordGroup:Transport") && !has(&h, "keywordGroup:"), "one keyword is no group");
    assert!(rect(&h, "keywordChip:sea").top() < heading.top());
    // the × still takes the whole keyword off both
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipRemove:Places|Italy|Rome"}));
    assert!(ids.iter().all(|id| !keywords_of(&h, *id).iter().any(|k| k == "Places|Italy|Rome")));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).iter().any(|k| k == "Places|Italy|Venice")));
}

// Given sixteen keywords and the grouped box,
// when the panel is drawn,
// then eight lines show with "+N more" for the rest, which shows them all; "Show fewer" brings the
// short list back. The Keyword List is nearer the top for it.
#[test]
fn a_long_grouped_list_is_shortened_until_asked_for() {
    let (mut h, ids) = two_selected();
    give(&mut h, &ids, &sixteen());
    assert_eq!(count(&h, "keywordChip:"), 16, "whole names: every keyword, as before");
    assert!(!has(&h, "keywordChipsMore"));
    let list_before = rect(&h, "field:keywordFilter").top();

    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    h.settle(SETTLE);
    assert_eq!(count(&h, "keywordChip:"), 7, "a heading and seven chips");
    assert!(has(&h, "keywordChipsMore") && !has(&h, "keywordChipsFewer"));
    assert!(rect(&h, "field:keywordFilter").top() < list_before - 80.0, "{} from {list_before}", rect(&h, "field:keywordFilter").top());

    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipsMore"}));
    assert_eq!(count(&h, "keywordChip:"), 16);
    assert!(has(&h, "keywordChipsFewer") && !has(&h, "keywordChipsMore"));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipsFewer"}));
    assert_eq!(count(&h, "keywordChip:"), 7);
}

// Given the grouped box, chips stay inside the panel and never overlap, whatever their length.
#[test]
fn grouped_chips_stay_inside_the_panel() {
    let (mut h, ids) = two_selected();
    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    let long = "Places|Portugal|Lisbon|Belém tower and the old town square on the river by the bridge";
    let keywords: Vec<String> =
        ["Events|Weddings", "Events|Birthdays", "sunset", "Places|Portugal|Porto", long].iter().map(|s| s.to_string()).collect();
    give(&mut h, &ids, &keywords);
    let panel = rect(&h, "panel:right_panel");
    let chips: Vec<(String, egui::Rect)> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordChip:")).cloned().collect();
    assert_eq!(chips.len(), 5, "{chips:?}");
    for (i, (id, r)) in chips.iter().enumerate() {
        assert!(r.left() >= panel.left() && r.right() <= panel.right(), "{id} inside the panel: {r:?} in {panel:?}");
        for (other, o) in chips.iter().skip(i + 1) {
            assert!(!r.intersects(o.shrink(0.5)), "{id} overlaps {other}");
        }
    }
}

// Given a selection without keywords, the box says so (as its other two views do).
#[test]
fn a_selection_without_keywords_says_so() {
    let (mut h, ids) = two_selected();
    give(&mut h, &ids, &[]);
    assert!(has(&h, "keywordChipsEmpty"));
    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    h.settle(SETTLE);
    assert!(has(&h, "keywordChipsEmpty"));
}

// Given the Keywords panel,
// when the title of Keywording or of the Keyword List is clicked,
// then that section folds shut and the other stays; a second click opens it, and what is folded is
// kept with the interface's settings.
#[test]
fn the_sections_fold_by_their_titles() {
    let (mut h, _) = two_selected();
    assert!(has(&h, "field:keyword") && has(&h, "field:keywordFilter"));
    let list_before = rect(&h, "field:keywordFilter").top();
    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywording"}));
    assert!(!has(&h, "field:keyword") && !has(&h, "keywordChip:beach") && !has(&h, "kwSet:1"), "Keywording is shut");
    assert!(has(&h, "field:keywordFilter"), "the Keyword List stays");
    assert!(rect(&h, "field:keywordFilter").top() < list_before - 100.0);
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
    assert!(back.section_folded("keywording") && !back.section_folded("keywordList"));
    assert!(!serde_json::from_str::<crate::UiState>("{}").unwrap().section_folded("keywording"), "settings from before: open");

    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywordList"}));
    assert!(!has(&h, "field:keywordFilter") && !has(&h, "keywordCheck:beach"), "the Keyword List is shut");
    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywording"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywordList"}));
    assert!(has(&h, "field:keyword") && has(&h, "field:keywordFilter") && has(&h, "keywordChip:beach"));
}

// Given the grouped box, the suggestions say only the keyword's own name (its whole path when two
// of them have the same name) and still give the whole keyword.
#[test]
fn grouped_suggestions_name_the_keyword_alone() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[0]], "addKeywords": ["Places|Portugal|Lisbon"]}}));
    let whole = rect(&h, "kwSuggest:Places|Portugal|Lisbon").width();
    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    h.settle(SETTLE);
    assert!(rect(&h, "kwSuggest:Places|Portugal|Lisbon").width() < whole);
    ask(&mut h, "ui.clickWidget", json!({"id": "kwSuggest:Places|Portugal|Lisbon"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).iter().any(|k| k == "Places|Portugal|Lisbon")));
}

/// Two photos selected (beach + Weddings, beach), and a third photo of the library, not selected,
/// with Places|Portugal|Lisbon and Places|Portugal|Porto.
fn with_places() -> (Headless, Vec<u64>) {
    let (mut h, ids) = two_selected();
    let third = h.app.session.visible_cloned().get(2).map(|p| p.0).expect("a third photo");
    ask(
        &mut h,
        "engine.execute",
        json!({"command": "photo.setMeta", "params": {"ids": [third], "keywords": ["Places|Portugal|Lisbon", "Places|Portugal|Porto"]}}),
    );
    (h, ids)
}

fn typed(h: &mut Headless, text: &str) {
    ask(h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(h, "ui.text", json!({"text": text}));
}

fn all_have(h: &Headless, ids: &[u64], keyword: &str) -> bool {
    ids.iter().all(|id| keywords_of(h, *id).iter().any(|k| k == keyword))
}

fn none_has(h: &Headless, ids: &[u64], keyword: &str) -> bool {
    ids.iter().all(|id| !keywords_of(h, *id).iter().any(|k| k == keyword))
}

// Given a library with Places › Portugal › Lisbon,
// when "lis" is typed in the Add keyword box,
// then the keyword is offered right under the box, and the suggestions further down stay what
// they were (they used to turn into the completions, a long way from the box).
#[test]
fn typing_offers_the_librarys_keywords_under_the_box() {
    let (mut h, _) = with_places();
    assert!(has(&h, "kwSuggest:Weddings"));
    typed(&mut h, "lis");
    let (field, offer) = (rect(&h, "field:keyword"), rect(&h, "kwComplete:Places|Portugal|Lisbon"));
    assert!(offer.top() >= field.bottom() && offer.top() - field.bottom() < 12.0, "right under the box: {offer:?} {field:?}");
    assert!(offer.left() >= field.left() - 1.0 && offer.right() <= field.right() + 1.0, "as wide as the box: {offer:?} {field:?}");
    assert!(!has(&h, "kwComplete:Places|Portugal|Porto"), "only what matches");
    assert!(has(&h, "kwSuggest:Weddings") && !has(&h, "kwSuggest:Places|Portugal|Lisbon"), "the suggestions stay");
    // nothing typed, nothing offered
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(count(&h, "kwComplete"), 0);
}

// Given the offers under the box,
// when ↓ then Return are pressed,
// then the highlighted keyword goes to every selected photo (not the letters typed), and the box
// is empty and still has the cursor for the next keyword.
#[test]
fn down_and_return_give_the_offered_keyword() {
    let (mut h, ids) = with_places();
    typed(&mut h, "lis");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    assert!(has(&h, "kwCompleteAt:Places|Portugal|Lisbon"), "highlighted");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "Places|Portugal|Lisbon"));
    assert!(none_has(&h, &ids, "lis"), "the letters typed are not a keyword");
    assert_eq!(count(&h, "kwComplete"), 0, "the box is empty again");
    // still in the box: the next keyword is typed straight away
    ask(&mut h, "ui.text", json!({"text": "harbour"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "harbour"));
}

// Given the offers under the box, Return alone still gives what was typed, as it always did; ↑
// from the first offer goes back to that.
#[test]
fn return_alone_gives_what_was_typed() {
    let (mut h, ids) = with_places();
    typed(&mut h, "lis");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "lis") && none_has(&h, &ids, "Places|Portugal|Lisbon"));

    typed(&mut h, "por");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    ask(&mut h, "ui.key", json!({"key": "ArrowUp"}));
    assert_eq!(count(&h, "kwCompleteAt:"), 0, "none highlighted");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "por") && none_has(&h, &ids, "Places|Portugal|Porto"));
}

// Given a name the library doesn't have,
// when it is typed,
// then the box says Return will create it (and inside which keyword, with a default parent); a
// keyword the library has is not announced as new.
#[test]
fn a_new_name_is_announced_as_new() {
    let (mut h, _) = with_places();
    typed(&mut h, "Lisbn");
    assert!(has(&h, "kwCompleteCreate"), "a misspelling is a new keyword: say so");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    typed(&mut h, "weddings");
    assert!(!has(&h, "kwCompleteCreate"), "the library has Weddings");
    assert!(has(&h, "kwComplete:Weddings"), "one of the two lacks it");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    typed(&mut h, "Places|Portugal");
    assert!(!has(&h, "kwCompleteCreate"), "a keyword with keywords inside it is there too");
}

// Given the offers under the box, a click on one gives that keyword to the selected photos; a
// click on the "create" line gives what was typed. Keywords typed before a comma go along.
#[test]
fn a_click_on_an_offer_gives_it() {
    let (mut h, ids) = with_places();
    typed(&mut h, "sea, lis");
    assert!(has(&h, "kwComplete:Places|Portugal|Lisbon"), "the offers are for what follows the comma");
    ask(&mut h, "ui.clickWidget", json!({"id": "kwComplete:Places|Portugal|Lisbon"}));
    assert!(all_have(&h, &ids, "Places|Portugal|Lisbon") && all_have(&h, &ids, "sea") && none_has(&h, &ids, "lis"));

    typed(&mut h, "harbour");
    ask(&mut h, "ui.clickWidget", json!({"id": "kwCompleteCreate"}));
    assert!(all_have(&h, &ids, "harbour"));
}

// Given more offers than there is room for, ↓ stops at the last and never runs off the list.
#[test]
fn the_highlight_stays_on_the_list() {
    let (mut h, ids) = with_places();
    typed(&mut h, "por");
    for _ in 0..40 {
        ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    }
    let lit: Vec<String> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("kwCompleteAt:").map(str::to_string)).collect();
    assert_eq!(lit.len(), 1, "{lit:?}");
    let offered = count(&h, "kwComplete:");
    assert!(offered >= 2, "Portugal and Porto both match");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, &lit[0]), "the last offer, {}, was given", lit[0]);
    assert!(none_has(&h, &ids, "por"));
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

// Given several photos selected, the box says how many its keywords are of; with one photo there
// is nothing to say.
#[test]
fn the_box_says_how_many_photos_it_is_for() {
    let (mut h, ids) = two_selected();
    assert!(has(&h, "keywordScope"), "two photos are selected");
    assert!(rect(&h, "keywordScope").bottom() <= rect(&h, "keywordChip:beach").top(), "above the chips it is about");
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0]]}}));
    assert!(!has(&h, "keywordScope"), "one photo");
}

// The × is a target as tall as its chip and as wide as that, apart from the name next to it.
#[test]
fn the_cross_is_as_tall_as_its_chip() {
    let (h, _) = two_selected();
    let (name, cross) = (rect(&h, "keywordChip:beach"), rect(&h, "keywordChipRemove:beach"));
    assert!(cross.height() >= 22.0 && cross.width() >= 22.0, "{cross:?}");
    assert_eq!(cross.height(), name.height());
    assert!(cross.left() >= name.right() - 0.5, "beside the name, not over it: {cross:?} {name:?}");
}

// Given a keyword on several selected photos,
// when its × takes it off them,
// then a message names the keyword and how many photos lost it and says how to get it back, and
// Undo does. Taking a keyword off one photo says nothing: the chip going is enough.
#[test]
fn taking_a_keyword_off_several_photos_says_so_and_undo_puts_it_back() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipRemove:Weddings"}));
    assert_eq!(toast(&h), "", "only one of the two had Weddings");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipRemove:beach"}));
    assert!(none_has(&h, &ids, "beach"));
    let said = toast(&h);
    assert!(said.contains("beach") && said.contains('2') && said.contains("Undo"), "{said}");
    ask(&mut h, "engine.execute", json!({"command": "edit.undo", "params": {}}));
    assert!(all_have(&h, &ids, "beach"), "one undo step puts it back on both");

    // the menu's Remove from Selected Photos says the same
    h.app.ui.toast = None;
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChip:beach", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipMenu:remove"}));
    assert!(none_has(&h, &ids, "beach") && toast(&h).contains("beach"), "{}", toast(&h));
}

// Names of any length and script stay inside the panel in both layouts, and nothing breaks: a
// name of 400 letters, one of emoji and right-to-left text, levels of one letter.
#[test]
fn any_name_fits_the_panel() {
    let (mut h, ids) = two_selected();
    let long = "x".repeat(400);
    let keywords: Vec<String> =
        vec![long.clone(), format!("a|{long}"), "🙂|العربية|שלום".into(), "🙂|العربية|مرحبا".into(), "a|b|c|d|e|f|g|h".into(), "İ|ı".into()];
    give(&mut h, &ids, &keywords);
    for layout in [crate::state::KeywordChips::Paths, crate::state::KeywordChips::Grouped] {
        h.app.ui.settings.keyword_chips = layout;
        h.settle(SETTLE);
        let panel = rect(&h, "panel:right_panel");
        let chips: Vec<(String, egui::Rect)> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordChip:")).cloned().collect();
        assert_eq!(chips.len(), keywords.len(), "{layout:?}");
        for (id, r) in &chips {
            assert!(
                r.left() >= panel.left() && r.right() <= panel.right(),
                "{layout:?}: {} inside the panel: {r:?}",
                id.chars().take(40).collect::<String>()
            );
            assert!(r.width() > 0.0 && r.height() < 30.0);
        }
    }
    // and typing them offers them without trouble: the photos here all have them, so another
    // photo's emoji keyword is what is offered
    let next = others(&mut h);
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [next[0]], "keywords": ["🙂|العربية|سلام"]}}));
    typed(&mut h, &"x".repeat(300));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    typed(&mut h, "🙂");
    assert!(has(&h, "kwComplete:🙂|العربية|سلام"), "{:?}", h.app.widgets.iter().filter(|(w, _)| w.starts_with("kwComplete")).collect::<Vec<_>>());
}

fn lit(h: &Headless) -> Vec<String> {
    h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("kwCompleteAt:").map(str::to_string)).collect()
}

/// Ids of the library's photos after the two selected ones.
fn others(h: &mut Headless) -> Vec<u64> {
    h.app.session.visible_cloned().iter().skip(2).take(4).map(|p| p.0).collect()
}

// Given a long grouped list shown whole after "+N more",
// when other photos with a long list are selected,
// then theirs is short again: asking for more was for the photos it was asked of.
#[test]
fn asking_for_more_is_for_the_selection_it_was_asked_of() {
    let (mut h, ids) = two_selected();
    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    give(&mut h, &ids, &sixteen());
    let next = others(&mut h);
    give(&mut h, &next, &sixteen());
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipsMore"}));
    assert_eq!(count(&h, "keywordChip:"), 16);
    // taking a keyword off keeps the list whole: it is still theirs
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": ids, "removeKeywords": [sixteen()[15]]}}));
    assert_eq!(count(&h, "keywordChip:"), 15);
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": next}}));
    assert_eq!(count(&h, "keywordChip:"), 7, "short again");
    assert!(has(&h, "keywordChipsMore") && !has(&h, "keywordChipsFewer"));
}

// Given a keyword two photos spell differently (Lisbon, lisbon),
// when it is typed,
// then it is offered once, and a click gives it.
#[test]
fn a_keyword_spelled_two_ways_is_offered_once() {
    let (mut h, ids) = two_selected();
    let next = others(&mut h);
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [next[0]], "keywords": ["Lisbon"]}}));
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [next[1]], "keywords": ["lisbon"]}}));
    typed(&mut h, "lis");
    let offered: Vec<String> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("kwComplete:")).map(|(w, _)| w.clone()).collect();
    assert_eq!(offered.len(), 1, "{offered:?}");
    ask(&mut h, "ui.clickWidget", json!({"id": offered[0]}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).iter().any(|k| k.eq_ignore_ascii_case("lisbon"))));
}

// Given a highlighted offer,
// when the library changes under it and the offers come in another order,
// then the highlight is still on that keyword (it used to stay on the line, and Return gave
// whatever keyword had moved there).
#[test]
fn the_highlight_stays_with_its_keyword() {
    let (mut h, ids) = with_places();
    typed(&mut h, "por");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    let before = lit(&h);
    assert_eq!(before.len(), 1);
    let next = others(&mut h);
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": next, "addKeywords": ["Portraits"]}}));
    assert!(has(&h, "kwComplete:Portraits"), "the offers changed");
    assert_eq!(lit(&h), before, "the highlight did not move to another keyword");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, &before[0]) && none_has(&h, &ids, "Portraits"));
}

// Given an offer taken with ↓ and Return, Esc afterwards does not bring back the keywords that
// were just added: the box stays empty.
#[test]
fn escape_after_an_offer_keeps_the_box_empty() {
    let (mut h, ids) = with_places();
    typed(&mut h, "sea");
    ask(&mut h, "ui.key", json!({"key": "Tab"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "End"}));
    ask(&mut h, "ui.text", json!({"text": ", lis"}));
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "sea") && all_have(&h, &ids, "Places|Portugal|Lisbon"));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "End"}));
    ask(&mut h, "ui.text", json!({"text": "x"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "x") && none_has(&h, &ids, "seax"), "{:?}", keywords_of(&h, ids[0]));
}

// ↓ / ↑ / Return with Shift or Alt held are the text box's own (select to the start of the text):
// they leave the offers alone.
#[test]
fn modified_arrows_are_the_text_boxs() {
    let (mut h, ids) = with_places();
    typed(&mut h, "por");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown", "shift": true}));
    assert!(lit(&h).is_empty(), "Shift+↓ highlights nothing: {:?}", lit(&h));
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    let before = lit(&h);
    ask(&mut h, "ui.key", json!({"key": "ArrowUp", "shift": true}));
    assert_eq!(lit(&h), before);
    let _ = ids;
}

// Given the box scrolled out of the panel with the cursor still in it, its offers are not drawn
// (they used to float over the top bar, where Return still took one).
#[test]
fn offers_go_when_the_box_scrolls_out_of_sight() {
    let (mut h, ids) = with_places();
    give(&mut h, &ids, &sixteen());
    typed(&mut h, "lis");
    assert!(has(&h, "kwComplete:Places|Portugal|Lisbon"));
    let panel = rect(&h, "panel:right_panel");
    ask(&mut h, "ui.move", json!({"x": panel.center().x, "y": panel.center().y}));
    for _ in 0..8 {
        ask(&mut h, "ui.scroll", json!({"dy": -400.0}));
    }
    assert!(rect(&h, "field:keyword").bottom() < panel.top(), "scrolled out: {:?}", rect(&h, "field:keyword"));
    assert_eq!(count(&h, "kwComplete"), 0);
}

// Settings files with something else under the two new keys keep every other setting (and these
// two at their defaults): a wrong value used to discard the whole file.
#[test]
fn wrong_values_for_the_new_settings_keep_the_rest() {
    let ui: crate::UiState = serde_json::from_str(r#"{"foldedSections": "oops", "rightWidth": 300.0}"#).unwrap();
    assert!(!ui.section_folded("keywording") && ui.right_width == 300.0);
    let ui: crate::UiState =
        serde_json::from_str(r#"{"foldedSections": [1, "keywordList", null], "settings": {"keywordChips": "bogus", "filmNames": false}}"#).unwrap();
    assert!(ui.section_folded("keywordList") && !ui.section_folded("keywording"));
    assert_eq!(ui.settings.keyword_chips, crate::state::KeywordChips::Paths);
    assert!(!ui.settings.film_names, "the other settings are read");
    let ui: crate::UiState = serde_json::from_str(r#"{"settings": {"keywordChips": "grouped"}}"#).unwrap();
    assert_eq!(ui.settings.keyword_chips, crate::state::KeywordChips::Grouped);
}

// Given an offer taken with ↓ and Return,
// when the panel is switched away at once and comes back much later,
// then the box does not take the cursor unasked: keys typed then are not the box's.
#[test]
fn an_offer_taken_does_not_claim_the_cursor_later() {
    let (mut h, ids) = with_places();
    typed(&mut h, "lis");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    // (Return and the switch on consecutive frames, as a script does them)
    let r = h.request("ui.key", json!({"key": "Enter"}), T);
    assert_eq!(r["ok"], true, "{r}");
    ask(&mut h, "ui.set", json!({"right": "info"}));
    assert!(all_have(&h, &ids, "Places|Portugal|Lisbon"));
    ask(&mut h, "ui.set", json!({"right": "keywords"}));
    ask(&mut h, "ui.text", json!({"text": "zz"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(none_has(&h, &ids, "zz"), "typed with the cursor nowhere: {:?}", keywords_of(&h, ids[0]));
}

// Given a highlighted offer and the box scrolled out of sight, Return gives nothing: what isn't
// drawn can't be taken, not even on the frame the box leaves.
#[test]
fn an_offer_out_of_sight_is_not_taken() {
    let (mut h, ids) = with_places();
    give(&mut h, &ids, &sixteen());
    typed(&mut h, "lis");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    let panel = rect(&h, "panel:right_panel");
    ask(&mut h, "ui.move", json!({"x": panel.center().x, "y": panel.center().y}));
    for _ in 0..8 {
        // the wheel and Return together, frame after frame
        let r = h.request("ui.scroll", json!({"dy": -400.0}), T);
        assert_eq!(r["ok"], true, "{r}");
        if rect(&h, "field:keyword").bottom() < panel.top() {
            let r = h.request("ui.key", json!({"key": "Enter"}), T);
            assert_eq!(r["ok"], true, "{r}");
        }
    }
    h.settle(SETTLE);
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(none_has(&h, &ids, "Places|Portugal|Lisbon"), "{:?}", keywords_of(&h, ids[0]));
}

// Given a window too short for the offers under the box, they never cover the box being typed
// in: they go where there is more room and show as many lines as fit there.
#[test]
fn offers_never_cover_the_box() {
    for height in [1000.0, 420.0, 300.0, 230.0, 180.0] {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1400.0, height], 1.0);
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(12).map(|p| p.0).collect();
        ask(&mut h, "ui.set", json!({"view": "photoGrid", "right": "keywords"}));
        // nine keywords to offer for "pa", on photos that are not selected
        let many: Vec<String> = (1..=9).map(|i| format!("Parks|park {i}")).collect();
        ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": &ids[2..], "keywords": many}}));
        ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": &ids[..2]}}));
        typed(&mut h, "pa");
        let field = rect(&h, "field:keyword");
        let offers: Vec<egui::Rect> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("kwComplete")).map(|(_, r)| *r).collect();
        assert!(!offers.is_empty(), "{height}: something is offered");
        for r in &offers {
            assert!(!r.intersects(field.shrink(0.5)), "{height}: an offer at {r:?} covers the box at {field:?}");
            assert!(r.top() >= -0.5 && r.bottom() <= height + 0.5, "{height}: an offer at {r:?} is off the window");
        }
        // ↓ still reaches every offer drawn, and the highlighted one is among them
        for _ in 0..12 {
            ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
            assert_eq!(lit(&h).len(), 1, "{height}");
        }
    }
}

/// Rename Keyword's "used by n photos" and Create Smart Album's live count scan the library once
/// per change to it, not every frame the dialog is open.
#[test]
fn open_dialogs_count_once_per_library_change() {
    let (mut h, ids) = two_selected();
    h.app.ui.dialog = Some(crate::state::Dialog::RenameKeyword { from: "beach".into(), to: "beach".into() });
    let before = h.app.caches.dialog_count_scans;
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.caches.dialog_count_scans, before + 1, "unchanged frames reuse the count");
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[1]], "keywords": ["sea"]}}));
    h.step();
    assert_eq!(h.app.caches.dialog_count_scans, before + 2, "a library change counts again");
    h.app.ui.dialog = Some(crate::state::Dialog::NewSmartAlbum { name: "Beach".into(), parent: None });
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.caches.dialog_count_scans, before + 3, "another dialog counts its own photos, once");
}
