//! Headless tests of Library's Classic panels (P1.4): the panel stacks, Folders, Collections,
//! Quick Develop, Keywording and Keyword List; and Help ▸ What's New, joined from release-note files.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::module::PanelId;
use crate::panels::classic;
use crate::state::RightPanel;
use crate::{LccApp, Services};

pub(crate) const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

pub(crate) fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.settle(SETTLE);
    run(&mut h, "module.library", json!({}));
    h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true, "right": "none"}), T);
    h.step();
    h.step();
    h
}

pub(crate) fn run(h: &mut Headless, id: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

pub(crate) fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

pub(crate) fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

/// Help ▸ What's New shows upstream's file and every entry file in docs/whats-new/, line for line (so their
/// translations still match), and nothing else but the date headings that group them.
#[test]
fn whats_new_shows_upstreams_file_and_every_entry_file() {
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let mut sources = vec![std::fs::read_to_string(docs.join("whats-new.md")).unwrap()];
    for item in std::fs::read_dir(docs.join("whats-new")).unwrap().flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") && name != "README.md" {
            sources.push(std::fs::read_to_string(item.path()).unwrap());
        }
    }
    assert!(sources.len() > 1, "docs/whats-new/ holds entries");
    let shown: std::collections::BTreeSet<&str> = crate::panels::dialogs::WHATS_NEW.lines().map(str::trim_end).collect();
    let written: std::collections::BTreeSet<&str> = sources.iter().flat_map(|s| s.lines()).map(str::trim_end).collect();
    let lost: Vec<&&str> = written.difference(&shown).collect();
    assert!(lost.is_empty(), "every line of the entries and of upstream's file is shown; missing: {lost:?}");
    let extra: Vec<&&str> =
        shown.difference(&written).filter(|l| !l.strip_prefix("## ").is_some_and(|d| d.len() == 10 && d.starts_with("20"))).collect();
    assert!(extra.is_empty(), "nothing but date headings is added: {extra:?}");
    let dates: Vec<&str> = crate::panels::dialogs::WHATS_NEW
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .filter(|d| d.starts_with(|c: char| c.is_ascii_digit()))
        .collect();
    let mut newest_first = dates.clone();
    newest_first.sort_by(|a, b| b.cmp(a));
    newest_first.dedup();
    assert_eq!(dates, newest_first, "each date once, newest first");
}

/// Quick Develop: a step button adds to every selected photo's own value in one undo step; the
/// menus set a choice on each.
#[test]
fn quick_develop_steps_every_selected_photo() {
    let mut h = demo();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    run(&mut h, "library.select", json!({"ids": ids}));
    assert!(has(&h, "button:qd-light.exposure-3"), "the exposure row is on screen");
    let exposure = |h: &Headless, id: u64| h.app.session.develop_of(lcc_catalog::PhotoId(id)).unwrap().light.exposure;
    let before: Vec<f64> = ids.iter().map(|i| exposure(&h, *i)).collect();
    let undo = h.app.session.undo.len();
    click(&mut h, "button:qd-light.exposure-3");
    for (i, id) in ids.iter().enumerate() {
        assert!((exposure(&h, *id) - before[i] - 1.0).abs() < 1e-9, "+1 stop on photo {id}");
    }
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step");
    run(&mut h, "develop.quickSet", json!({"treatment": "bw"}));
    for id in &ids {
        assert_eq!(h.app.session.develop_of(lcc_catalog::PhotoId(*id)).unwrap().treatment, lcc_develop::Treatment::Bw);
    }
    assert!(has(&h, "button:qd-crop") && has(&h, "button:qd-wb") && has(&h, "button:qd-autoTone"));
}

/// Folders: each disk the library's photos are on is a volume row with its free space; a folder
/// created through the command shows up on disk and undoes.
#[test]
fn folders_panel_lists_volumes_with_free_space() {
    let mut h = demo();
    let dir = std::env::temp_dir().join(format!("classic-ui-vol-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.jpg").to_string_lossy().to_string();
    let id = h.app.session.catalog.alloc_photo_id();
    let p = lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path }, "a.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    h.app.session.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    // the space is read off the UI thread: wait for it
    h.step_until(SETTLE, |h| h.app.widgets.iter().any(|(w, _)| w.starts_with("volume:")));
    assert!(h.app.widgets.iter().any(|(w, _)| w.starts_with("volume:")), "a volume row");
    run(&mut h, "folder.create", json!({"parent": dir.to_string_lossy(), "name": "New"}));
    assert!(dir.join("New").is_dir());
    run(&mut h, "edit.undo", json!({}));
    assert!(!dir.join("New").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Collections: the target collection shows under the tree; its × goes back to the Quick
/// Collection.
#[test]
fn collections_panel_shows_and_clears_the_target() {
    let mut h = demo();
    // the Collections panel near the top
    h.app.ui.toggle_sidebar_section("panel:catalog");
    h.app.ui.toggle_sidebar_section("panel:folders");
    let id = run(&mut h, "album.create", json!({"name": "Keepers"}))["id"].as_u64().unwrap();
    assert!(has(&h, "collectionsTarget") && !has(&h, "collectionsTargetClear"));
    run(&mut h, "album.setTarget", json!({"id": id}));
    assert!(has(&h, "collectionsTargetClear"));
    click(&mut h, "collectionsTargetClear");
    assert_eq!(h.app.session.target_album, None);
}

/// Keywording (upstream's keyword box in the Classic panel): the entry field and suggestions (a
/// click adds to every selected photo) are on screen.
#[test]
fn keywording_panel_adds_suggestions_to_the_selection() {
    let mut h = demo();
    h.app.ui.hidden_panels = vec![PanelId::QuickDevelop];
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    run(&mut h, "library.select", json!({"ids": ids}));
    assert!(has(&h, "field:keyword"));
    let k = h.app.widgets.iter().find_map(|(w, _)| w.strip_prefix("kwSuggest:").map(str::to_string)).expect("a suggestion");
    click(&mut h, &format!("kwSuggest:{k}"));
    for id in &ids {
        assert!(
            h.app.session.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().meta.keywords.iter().any(|x| x.eq_ignore_ascii_case(&k)),
            "{k} on {id}"
        );
    }
}

/// Keyword List panel (upstream's list): created keywords are listed; the tick box toggles a
/// keyword on the selection; the arrow filters by it.
#[test]
fn keyword_list_marks_filters_and_lists_created_keywords() {
    let mut h = demo();
    h.app.ui.hidden_panels = vec![PanelId::QuickDevelop, PanelId::Keywording];
    run(&mut h, "keyword.create", json!({"name": "Anna", "person": true}));
    let id = h.app.session.visible_cloned()[0].0;
    run(&mut h, "library.select", json!({"ids": [id]}));
    assert!(has(&h, "keywordRow:Anna"), "a created keyword is listed");
    click(&mut h, "keywordCheck:Anna");
    let has_kw = |h: &Headless| h.app.session.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().meta.keywords.iter().any(|k| k == "Anna");
    assert!(has_kw(&h), "the tick adds it");
    click(&mut h, "keywordCheck:Anna");
    assert!(!has_kw(&h), "and takes it away");
    click(&mut h, "keywordShow:Anna");
    assert_eq!(h.app.session.filter.keyword.as_deref(), Some("Anna"));
}

/// Library's columns are Classic panel stacks: every panel has a foldable header, Solo Mode
/// keeps one open per side, hidden panels go away, and the order is the user's.
#[test]
fn library_columns_are_classic_panels_with_solo_mode() {
    let mut h = demo();
    for p in ["catalog", "folders", "collections", "quickDevelop", "keywording", "keywordList", "metadata"] {
        assert!(has(&h, &format!("classicPanel:{p}")), "{p}");
    }
    assert!(has(&h, "sidebarSection:navigator"));
    // a click folds a panel and unfolds it again
    click(&mut h, "classicPanel:catalog");
    assert!(!classic::is_open(&h.app, PanelId::Catalog) && !has(&h, "source:all"));
    click(&mut h, "classicPanel:catalog");
    assert!(classic::is_open(&h.app, PanelId::Catalog) && has(&h, "source:all"));
    // solo mode: opening one panel folds the others on its side only
    run(&mut h, "panel.solo", json!({"on": true}));
    classic::set_open(&mut h.app, PanelId::Folders, false);
    classic::set_open(&mut h.app, PanelId::Folders, true);
    assert!(classic::is_open(&h.app, PanelId::Folders));
    assert!(!classic::is_open(&h.app, PanelId::Catalog) && !classic::is_open(&h.app, PanelId::Collections));
    assert!(classic::is_open(&h.app, PanelId::Keywording), "the other side is left alone");
    // hidden panels leave the column; the order is the user's
    run(&mut h, "panel.show", json!({"panel": "quickDevelop", "visible": false}));
    run(&mut h, "panel.order", json!({"order": ["metadata", "keywording"]}));
    assert!(!has(&h, "classicPanel:quickDevelop"));
    let y = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.top()).unwrap();
    assert!(y(&h, "classicPanel:metadata") < y(&h, "classicPanel:keywording"));
}

fn widget_center(h: &mut Headless, id: &str) -> (f64, f64) {
    let w = h.request("ui.widgets", json!({}), T);
    let r = w["result"].as_array().and_then(|a| a.iter().find(|x| x["id"] == id)).map(|x| x["rect"].clone()).unwrap();
    (r[0].as_f64().unwrap() + r[2].as_f64().unwrap() / 2.0, r[1].as_f64().unwrap() + r[3].as_f64().unwrap() / 2.0)
}

#[test]
fn reorder_moves_a_panel_to_a_slot() {
    use PanelId::*;
    let l = [QuickDevelop, Keywording, KeywordList, Metadata];
    assert_eq!(classic::reorder(&l, Metadata, 0), Some(vec![Metadata, QuickDevelop, Keywording, KeywordList]));
    assert_eq!(classic::reorder(&l, QuickDevelop, 4), Some(vec![Keywording, KeywordList, Metadata, QuickDevelop]));
    assert_eq!(classic::reorder(&l, Keywording, 1), None, "its own slot");
    assert_eq!(classic::reorder(&l, Keywording, 2), None, "just below itself");
    assert_eq!(classic::reorder(&l, Info, 0), None, "not on this side");
    assert_eq!(classic::reorder(&l, QuickDevelop, 99), Some(vec![Keywording, KeywordList, Metadata, QuickDevelop]));
}

/// Dragging a Classic panel header up its side moves the panel there.
#[test]
fn dragging_a_header_reorders_the_side() {
    let mut h = demo();
    run(&mut h, "panel.right", json!({"show": true}));
    for p in [PanelId::QuickDevelop, PanelId::Keywording, PanelId::KeywordList, PanelId::Metadata] {
        classic::set_open(&mut h.app, p, false);
    }
    h.step();
    h.step();
    let from = widget_center(&mut h, "classicPanel:metadata");
    let to = widget_center(&mut h, "classicPanel:quickDevelop");
    let r = h.request("ui.drag", json!({"x": from.0, "y": from.1, "toX": to.0, "toY": to.1 - 6.0, "steps": 8}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let order = classic::ordered(&h.app, crate::module::LIBRARY_RIGHT);
    assert_eq!(order.first(), Some(&PanelId::Metadata), "{order:?}");
}

/// Auto Hide: a hidden side comes back on a click at the window edge (not on hover), and the
/// two modes exclude each other.
#[test]
fn auto_hide_shows_a_side_on_a_click_at_the_edge() {
    let mut h = demo();
    run(&mut h, "panel.left", json!({"show": false}));
    run(&mut h, "panel.autoShow", json!({"edge": "left", "on": true}));
    run(&mut h, "panel.autoHide", json!({"edge": "left", "on": true}));
    assert!(h.app.ui.auto_hide.left && !h.app.ui.auto_show.left);
    h.request("ui.move", json!({"x": 1.0, "y": 450.0}), T);
    h.step();
    h.step();
    assert!(!h.app.ui.peek.left, "hovering does not show it");
    h.request("ui.click", json!({"x": 1.0, "y": 450.0}), T);
    h.step();
    h.step();
    assert!(h.app.ui.peek.left, "a click at the edge shows it");
    h.request("ui.move", json!({"x": 900.0, "y": 450.0}), T);
    h.step();
    h.step();
    assert!(!h.app.ui.peek.left, "leaving hides it again");
    let r = h.request("engine.execute", json!({"command": "panel.autoHide", "params": {"edge": "middle"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}

/// The module picker: clicking a module's name switches to it (the bar's own context-menu area
/// used to sit on top of the names and swallow the click).
#[test]
fn clicking_a_module_name_switches_module() {
    let mut h = demo();
    click(&mut h, "module:map");
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Map);
    click(&mut h, "module:develop");
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
}

/// Widgets whose id starts with `prefix`.
fn rows(h: &Headless, prefix: &str) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with(prefix)).count()
}

/// The demo library in Develop, with the left column shown: a module keeps the edges it was left
/// with (see `module::ModuleLayout`), and this one starts with the column out of the way.
fn develop_demo() -> Headless {
    let mut h = demo();
    click(&mut h, "module:develop");
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    h.request("ui.set", json!({"leftPanel": true}), T);
    h.settle(SETTLE);
    h
}

/// The top of a widget.
fn top(h: &Headless, id: &str) -> f32 {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.top()).unwrap_or_else(|| panic!("no widget {id}"))
}

/// The lowest bottom edge among the preset rows of the Presets panel (where the panel's own list
/// ends): the group headers and the presets themselves.
fn presets_bottom(h: &Headless) -> f32 {
    h.app
        .widgets
        .iter()
        .filter(|(w, _)| w.starts_with("presetGroup:") || w.starts_with("preset:") && !w.starts_with("presetMenu:"))
        .map(|(_, r)| r.bottom())
        .fold(f32::MIN, f32::max)
}

/// Develop's left column is its own Classic stack — Navigator, Presets, Versions, History,
/// Collections — drawn like Library's, not the Library's structure in the editing module. The
/// Catalog, Folders and Keywords panels stay in Library; upstream's My Photos sidebar is offered
/// here as a panel of its own, hidden until it is asked for.
#[test]
fn develop_column_is_its_own_classic_stack() {
    let mut h = develop_demo();
    // the Develop column's panels: the Navigator pinned above, then the presets and the Versions,
    // History and Collections panels, each under its Classic header
    for p in ["presets", "versions", "history", "collections"] {
        assert!(has(&h, &format!("classicPanel:{p}")), "{p}");
    }
    assert!(has(&h, "sidebarSection:navigator"), "the Navigator, where Classic puts it");
    assert!(rows(&h, "presetGroup:") > 0, "the presets themselves");
    // the presets are drawn once: with the presets column open (Window ▸ Presets), that column is the
    // presets — the same panel, with a header of its own
    let in_column = rows(&h, "presetGroup:");
    run(&mut h, "panel.presets", json!({}));
    assert!(h.app.ui.presets);
    assert!(rows(&h, "presetGroup:") <= in_column, "the presets are not drawn twice");
    run(&mut h, "panel.presets", json!({}));
    assert!(!h.app.ui.presets && rows(&h, "presetGroup:") == in_column, "back in the column");
    // the Library's panels are not here
    for p in ["catalog", "folders", "publish"] {
        assert!(!has(&h, &format!("classicPanel:{p}")), "{p} belongs to Library");
    }
    assert!(!has(&h, "sidebarSection:keywords"), "Keywords stays in Library");
    // upstream's My Photos sidebar: there once asked for, out of the way until then. It draws its
    // own title row and sections (as the Navigator draws its own header), not a Classic header.
    assert!(!has(&h, "sidebarTitle") && !has(&h, "sidebarSection:albums"));
    run(&mut h, "panel.show", json!({"panel": "sources", "visible": true}));
    assert!(!has(&h, "classicPanel:sources") && has(&h, "sidebarTitle") && has(&h, "sidebarSection:albums"));
    run(&mut h, "panel.show", json!({"panel": "sources", "visible": false}));
    assert!(!has(&h, "sidebarTitle") && !has(&h, "sidebarSection:albums"));
}

/// Develop's column folds, solos, reorders and hides its Classic panels exactly as Library's does.
#[test]
fn develop_column_folds_solos_reorders_and_hides() {
    let mut h = develop_demo();
    // a click folds a panel and unfolds it again (the presets panel above it is bounded, so these
    // Classic headers are on screen without scrolling)
    click(&mut h, "classicPanel:versions");
    assert!(!classic::is_open(&h.app, PanelId::Versions) && !has(&h, "button:versionCreate"));
    click(&mut h, "classicPanel:versions");
    assert!(classic::is_open(&h.app, PanelId::Versions) && has(&h, "button:versionCreate"));
    // Solo Mode: opening one panel folds the others on its side only
    run(&mut h, "panel.solo", json!({"on": true}));
    classic::set_open(&mut h.app, PanelId::History, false);
    classic::set_open(&mut h.app, PanelId::History, true);
    assert!(classic::is_open(&h.app, PanelId::History));
    assert!(!classic::is_open(&h.app, PanelId::Versions) && !classic::is_open(&h.app, PanelId::Collections));
    run(&mut h, "panel.solo", json!({"on": false}));
    // hiding takes the panel (header and rows) out of the column
    run(&mut h, "panel.show", json!({"panel": "versions", "visible": false}));
    assert!(!has(&h, "classicPanel:versions") && !has(&h, "button:versionCreate"));
    // and the order is the user's
    run(&mut h, "panel.order", json!({"order": ["history", "collections"]}));
    assert!(top(&h, "classicPanel:history") < top(&h, "classicPanel:collections"), "History moved above Collections");
}

/// A settings file written before Develop had its Classic column has never seen the My Photos
/// panel that column offers: it starts hidden there, as for a fresh install, and what the file
/// says about it from then on is kept.
#[test]
fn an_older_settings_file_keeps_the_my_photos_panel_hidden() {
    let older = json!({
        "leftPanel": true,
        "hiddenPanels": [],
        "layouts": {"develop": {"hidden": [], "order": []}},
    });
    let ui = serde_json::from_value::<crate::UiState>(older).unwrap().sanitized();
    assert!(ui.hidden_panels.contains(&PanelId::Sources) && ui.develop_classic);
    assert!(ui.layouts[&crate::module::ModuleId::Develop].hidden.contains(&PanelId::Sources), "the saved layout too");
    // a file written since keeps whatever it says (here: shown on purpose)
    let now = json!({"developClassic": true, "hiddenPanels": [], "layouts": {"develop": {"hidden": [], "order": []}}});
    let ui = serde_json::from_value::<crate::UiState>(now).unwrap().sanitized();
    assert!(ui.hidden_panels.is_empty() && ui.layouts[&crate::module::ModuleId::Develop].hidden.is_empty());
}

/// The tool strip is per module: Develop's editing tools are not offered in Library (a click there
/// would send the user into another module), and #108: Develop's strip holds Develop's tools only —
/// the Versions, Activity and Keywords panels are not Develop's, even though they were written with
/// it. Library's own strip has its panels (Versions, History & Activity), which open over its column.
#[test]
fn the_tool_strip_is_develops_alone() {
    let lib = demo();
    assert!(!has(&lib, "icon:edit") && !has(&lib, "icon:crop") && !has(&lib, "icon:presets"), "no Develop tools in Library");
    assert!(has(&lib, "classicPanel:metadata"), "Library's right column is still there");
    assert!(has(&lib, "icon:versions") && has(&lib, "icon:activity"), "Library's strip has its own panels");
    let mut h = develop_demo();
    assert!(has(&h, "icon:edit") && has(&h, "icon:crop") && has(&h, "icon:presets"), "the strip is Develop's");
    for id in ["versions", "activity", "keywords", "info"] {
        assert!(!has(&h, &format!("icon:{id}")), "Develop's strip is its tools, not {id}");
    }
    // Versions is still reached from Develop's own column and by its shortcut
    assert!(has(&h, "classicPanel:versions"), "Develop's left column holds the Versions panel");
    run(&mut h, "panel.versions", json!({}));
    assert_eq!(h.app.ui.right, RightPanel::Versions, "the shortcut still opens Versions");
}

/// Issue #44: folded Classic panels (Library and Develop), folded edit sections and Solo Mode are
/// written to the settings file and come back folded in an app started from it.
#[test]
fn folded_panels_and_solo_mode_survive_a_restart() {
    let mut h = develop_demo();
    click(&mut h, "classicPanel:versions");
    run(&mut h, "panel.solo", json!({"on": true}));
    let section = h.app.ui.open_sections.first().cloned();
    if let Some(s) = &section {
        h.app.ui.toggle_section(s);
    }
    click(&mut h, "module:library");
    click(&mut h, "classicPanel:catalog");
    assert!(!classic::is_open(&h.app, PanelId::Catalog));
    // the settings file, as the host writes and reads it
    let dir = std::env::temp_dir().join(format!("lc-fold-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ui.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&h.app.ui).unwrap()).unwrap();
    let saved: crate::UiState = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let services = Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
    app.ui = saved.sanitized();
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.settle(SETTLE);
    assert!(!classic::is_open(&h.app, PanelId::Catalog) && !has(&h, "source:all"), "Library's folded panel stays folded");
    click(&mut h, "module:develop");
    h.settle(SETTLE);
    assert!(!classic::is_open(&h.app, PanelId::Versions) && !has(&h, "button:versionCreate"), "Develop's folded panel stays folded");
    assert!(h.app.ui.single_panel, "Solo Mode is back on in Develop");
    if let Some(s) = &section {
        assert!(!h.app.ui.section_open(s), "the folded edit section {s} stays folded");
    }
}

/// Issue #44: a folded Book side-panel section (egui's own collapsing header, kept in the UI
/// state) stays folded in an app started from the saved settings; a junk entry is ignored.
#[test]
fn folded_book_section_survives_a_restart() {
    let mut h = demo();
    run(&mut h, "module.book", json!({}));
    h.settle(SETTLE);
    click(&mut h, "book:panel:Page Templates");
    h.step();
    assert_eq!(h.app.ui.section_open.get("book:Page Templates"), Some(&false));
    let mut saved = serde_json::to_value(&h.app.ui).unwrap();
    saved["sectionOpen"]["junk"] = json!(7);
    let saved: crate::UiState = serde_json::from_value(saved).unwrap();
    assert!(!saved.section_open.contains_key("junk"));
    let services = Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
    app.ui = saved.sanitized();
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Book);
    assert_eq!(h.app.ui.section_open.get("book:Page Templates"), Some(&false), "still folded after the restart");
    click(&mut h, "book:panel:Page Templates");
    h.step();
    assert_eq!(h.app.ui.section_open.get("book:Page Templates"), Some(&true), "and unfolds again");
}

/// Issue #36: with a preset collection imported whole (thousands of presets), Quick Develop's
/// Saved Preset menu builds only the rows in view; building one button per preset every frame
/// made Library sluggish while it was open. Picking a preset still applies it.
#[test]
fn quick_develop_preset_menu_builds_only_visible_rows() {
    let mut h = demo();
    let many = (0..5000)
        .map(|i| lcc_develop::Preset {
            id: format!("user.big.{i}"),
            name: format!("Look {i}"),
            group: format!("Big {:02}", i / 500),
            settings: json!({"light": {"exposure": 1.0}}),
            favorite: false,
            builtin: false,
        })
        .collect();
    assert_eq!(h.app.session.add_presets(many).len(), 5000);
    let id = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [id.0]}));
    let widgets = |h: &Headless| h.view.ctx.viewport(|v| v.prev_pass.widgets.layers().map(|(_, w)| w.len()).sum::<usize>());
    h.step();
    let closed = widgets(&h);
    click(&mut h, "button:qd-preset");
    h.step();
    let open = widgets(&h);
    assert!(open > closed + 5 && open < closed + 200, "the open menu built {} widgets", open.saturating_sub(closed));
}

/// Issue #46: clicking Black & White in Develop's Treatment row makes the photo black and white, and a
/// second click is back in colour.
#[test]
fn black_and_white_in_the_treatment_row_toggles() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 2400.0], 1.0);
    h.settle(SETTLE);
    let id = h.app.session.visible_cloned()[0].0;
    run(&mut h, "library.select", json!({"ids": [id]}));
    h.request("ui.set", json!({"view": "detail", "right": "edit", "openSections": ["light"], "histogram": false}), T);
    h.settle(SETTLE);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:treatment-bw"}), T)["ok"], true);
    h.settle(SETTLE);
    assert!(crate::is_bw(&h.app.session.develop_of(lcc_catalog::PhotoId(id)).unwrap_or_default()), "Black & White sets the treatment");
    // B&W toggles: a second click is back in colour
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:treatment-bw"}), T)["ok"], true);
    h.settle(SETTLE);
    assert!(!crate::is_bw(&h.app.session.develop_of(lcc_catalog::PhotoId(id)).unwrap_or_default()), "B&W toggles back to colour");
}

/// Issue #75: the Presets panel folds from its own Classic header, exactly as the Versions,
/// History and Collections panels below it do. Folded, the panels below are that much higher up
/// the column; unfolded, the presets are back. The header and its buttons stay either way.
#[test]
fn the_presets_fold_from_their_header() {
    let mut h = develop_demo();
    let open = |h: &Headless| classic::is_open_with_key(&h.app, crate::panels::presets::IN_COLUMN_KEY);
    assert!(rows(&h, "presetGroup:") > 0, "the presets are listed");
    assert!(has(&h, "classicPanel:presets"), "a Classic header like the panels below");
    // the panel's foot, and where the panels below it start
    let open_bottom = presets_bottom(&h);
    let versions_top = top(&h, "classicPanel:versions");
    click(&mut h, "classicPanel:presets");
    assert!(!open(&h), "folded");
    assert_eq!(rows(&h, "presetGroup:"), 0, "the list is folded away");
    assert!(has(&h, "classicPanel:presets") && has(&h, "icon:presetCreate") && has(&h, "icon:presetMore"), "the header stays");
    assert!(presets_bottom(&h) < open_bottom, "the panel is shorter: {open_bottom} -> {}", presets_bottom(&h));
    assert!(top(&h, "classicPanel:versions") < versions_top, "the panels below moved up: {versions_top} -> {}", top(&h, "classicPanel:versions"));
    // and back
    click(&mut h, "classicPanel:presets");
    assert!(open(&h) && rows(&h, "presetGroup:") > 0, "unfolded again");
    assert_eq!(top(&h, "classicPanel:versions"), versions_top, "the panels below are back where they were");
}

/// The presets fold in the left column only: opening the presets column (Window ▸ Presets) after folding
/// them there shows the presets, not an empty column, and closing it brings the folded panel back.
#[test]
fn folding_the_presets_in_the_column_leaves_the_presets_column_whole() {
    let mut h = develop_demo();
    click(&mut h, "classicPanel:presets");
    assert!(!classic::is_open_with_key(&h.app, crate::panels::presets::IN_COLUMN_KEY), "folded in the column");
    run(&mut h, "panel.presets", json!({}));
    assert!(h.app.ui.presets, "the presets column is open");
    assert!(rows(&h, "presetGroup:") > 0, "and it shows the presets");
    run(&mut h, "panel.presets", json!({}));
    assert!(!h.app.ui.presets && rows(&h, "presetGroup:") == 0, "the column is closed and the column's panel is still folded");
    click(&mut h, "classicPanel:presets");
    assert!(rows(&h, "presetGroup:") > 0, "unfolded again");
}

/// Issue #76: the Navigator stays at the top of the left column — the loupe's presets and the
/// photo itself — while the panels below it scroll under it, as in Lightroom Classic. The panels
/// below are what scrolls, and the Navigator keeps taking clicks where it sits.
#[test]
fn the_navigator_stays_at_the_top_while_the_panels_scroll_under_it() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    // a window too short for the column, so the panels below have somewhere to scroll
    let mut h = Headless::new(app, [1400.0, 600.0], 1.0);
    h.settle(SETTLE);
    // a module keeps the edges it was left with, and this one starts with the column out of the
    // way: switch first, then show it (see `develop_demo`)
    run(&mut h, "module.library", json!({}));
    h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true, "right": "none"}), T);
    h.settle(SETTLE);
    // better than the 1400 × 900 `demo()`: four panels under the Navigator overflow 600 points
    let nav = h.app.widgets.iter().find(|(w, _)| w == "sidebarSection:navigator").map(|(_, r)| *r).expect("the Navigator is drawn");
    let panel = h.app.widgets.iter().find(|(w, _)| w == "panel:left_panel").map(|(_, r)| *r).expect("the left panel is drawn");
    assert!(nav.top() >= panel.top() - 1.0 && nav.bottom() <= panel.bottom() + 1.0, "the Navigator is inside the column: {nav:?} {panel:?}");
    let all = |h: &Headless| h.app.widgets.iter().find(|(w, _)| w == "source:all").map(|(_, r)| r.top()).expect("All Photos");
    let before = all(&h);
    // the wheel scrolls the column the pointer is over
    h.request("ui.move", json!({"x": 120.0, "y": 300.0}), T);
    let r = h.request("ui.scroll", json!({"dy": -220.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..40 {
        h.step();
    }
    let after = all(&h);
    assert!(after < before - 40.0, "the panels below scrolled: {before} -> {after}");
    let nav_after = h.app.widgets.iter().find(|(w, _)| w == "sidebarSection:navigator").map(|(_, r)| *r).expect("the Navigator is still drawn");
    assert!((nav_after.top() - nav.top()).abs() < 1.0, "the Navigator stayed put: {:?} -> {:?}", nav.top(), nav_after.top());
    assert!(nav_after.bottom() <= panel.bottom() + 1.0, "and is not pushed out of the column: {nav_after:?} {panel:?}");
    // the presets are still where they are drawn: a click there reaches the loupe
    assert_eq!(h.request("ui.clickWidget", json!({"id": "navigator:1:1"}), T)["ok"], true);
    h.step();
    assert_eq!(h.app.ui.zoom, crate::state::Zoom::Percent(100.0), "the pinned Navigator takes clicks");
}

/// #155: Settings ▸ Shortcuts hosts the same editor as Help ▸ Keyboard Shortcuts, but a click on a
/// shortcut there stopped recording on the next frame, so no key could be set.
#[test]
fn settings_shortcuts_tab_records_a_key() {
    let mut h = demo();
    run(&mut h, "app.settings", json!({"tab": "shortcuts"}));
    let r = h.request("ui.clickWidget", json!({"id": "field:shortcutsSearch"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.request("ui.text", json!({"text": "compare"}), T)["ok"], true);
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "button:shortcut-view.compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.recording_shortcut.as_deref(), Some("view.compare"));
    let r = h.request("ui.key", json!({"key": "J", "shift": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.recording_shortcut, None);
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.compare"), Some("Shift+J"));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { .. })));
}

/// #219: In Classic Library view, the Keyword List panel renders its filter box and rows
/// directly under its Classic panel header, without a nested collapsible section header.
#[test]
fn keyword_list_has_no_nested_collapsible_header_in_classic() {
    let mut h = demo();
    run(&mut h, "panel.right", json!({"show": true}));
    classic::set_open(&mut h.app, PanelId::KeywordList, true);
    h.step();
    h.step();
    assert!(has(&h, "classicPanel:keywordList"), "Classic header is shown");
    assert!(has(&h, "field:keywordFilter"), "Filter field is shown in body");
    assert!(has(&h, "keywordList:create"), "+ button is shown in body");
    assert!(!has(&h, "section:keywordList"), "No nested collapsible fold title in Classic view");
}
