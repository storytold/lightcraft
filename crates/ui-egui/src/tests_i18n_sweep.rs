//! Built-in text reaches the screen translated: a tour of the main screens in a translated
//! language, failing on any painted text that is an English message the language's catalog
//! translates differently (text shown without `tr`).
//!
//! One app tours every screen (a demo session per screen made this the slowest test of the crate):
//! between screens the open dialog is dropped and Esc closes menus, and each screen gets a fixed
//! run of frames rather than a wait for its renders. What an earlier screen leaves
//! open is at worst reported under a later screen's name too.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::i18n::Locale;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Frames each screen is given before its text is read.
const FRAMES: usize = 20;

/// The screens the tour paints: a name for the failure message and the commands that open it.
fn screens() -> Vec<(String, Vec<(&'static str, serde_json::Value)>)> {
    let mut out: Vec<(String, Vec<(&'static str, serde_json::Value)>)> = vec![
        ("Library grid".into(), vec![("module.library", json!({})), ("view.photoGrid", json!({}))]),
        ("Library loupe".into(), vec![("module.library", json!({})), ("view.detail", json!({}))]),
        ("Library compare".into(), vec![("module.library", json!({})), ("view.compare", json!({}))]),
        ("Library survey".into(), vec![("module.library", json!({})), ("view.survey", json!({}))]),
    ];
    for panel in ["edit", "crop", "masking", "remove", "redeye", "presets", "profiles", "activity", "versions", "info", "keywords"] {
        out.push((format!("Develop {panel}"), vec![("module.develop", json!({})), (leak(format!("panel.{panel}")), json!({}))]));
    }
    // Issue #1050: the People view's three categories, each with its own headings, empty states and
    // counts (the demo library has no faces, which is the empty state's own text)
    for category in ["unknown", "suggested", "confirmed"] {
        out.push((format!("People {category}"), vec![("view.people", json!({})), ("people.category", json!({"category": category}))]));
    }
    for module in ["map", "book", "slideshow", "print", "web"] {
        out.push((format!("{module} module"), vec![("module.switch", json!({"module": module}))]));
    }
    for (tab, _) in crate::panels::settings::TABS {
        out.push((format!("Settings {tab}"), vec![("app.settings", json!({"tab": tab}))]));
    }
    for dialog in [
        "app.about",
        "app.shortcuts",
        "dialog.export",
        "dialog.import",
        "dialog.catalogSettings",
        "dialog.newAlbum",
        "dialog.newSmartAlbum",
        "dialog.createPreset",
        "dialog.copySettings",
        "dialog.syncSettings",
        "dialog.rename",
        "dialog.captureTime",
        "dialog.viewOptions",
        "dialog.watermark",
        "dialog.contactSheet",
        "dialog.aiModels",
        "dialog.presetsManager",
        "dialog.profileManager",
        "dialog.labelNames",
        "dialog.editInPresets",
        "dialog.slideshowExport",
        "dialog.autoStack",
        "plugins.manager",
        "plugins.marketplace",
    ] {
        out.push((dialog.to_string(), vec![("module.library", json!({})), (dialog, json!({}))]));
    }
    // context menus: a right click on a widget, after the commands that show it
    // named, since a mask's automatic name ("Radial Gradient") is stored in the edit, in English
    let radial = json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.2, "name": "Glow"});
    out.push((
        "Mask menu".into(),
        vec![("module.develop", json!({})), ("panel.masking", json!({})), ("mask.add", radial), (RIGHT_CLICK, json!({"widget": "mask:1"}))],
    ));
    out.push((
        "Preset group menu".into(),
        vec![("module.develop", json!({})), (SHOW_PRESETS, json!({})), (RIGHT_CLICK, json!({"widget": "presetGroup:"}))],
    ));
    out
}

/// A tour step that right-clicks the first widget whose id starts with `widget`, instead of running
/// a command.
const RIGHT_CLICK: &str = "rightClick";

/// A tour step that brings the presets on screen: `panel.presets` toggles, so it runs only while
/// they are hidden (they are shown or not by default depending on the layout).
const SHOW_PRESETS: &str = "showPresets";

/// Painted text that is not built-in UI text, though a catalog has the same English words.
const NOT_UI_TEXT: &[(&str, &str)] = &[
    ("Home", "a key name in the Shortcuts list"),
    ("End", "a key name in the Shortcuts list"),
    ("Mountains", "an album name in the demo library"),
    ("Default", "the name of the control-surface profile the settings created"),
    ("User Presets", "the default user preset group, shown by its stored name"),
];

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut session = lcc_engine::Session::with_demo();
    session.selection = lcc_engine::Selection::single(session.visible_cloned()[0]);
    let mut h = Headless::new(LccApp::new(session, services), [1600.0, 1000.0], 1.0);
    h.settle(SETTLE);
    h
}

/// Painted text the language's catalog translates, keyed by the text, with the screens it was on.
fn untranslated(h: &mut Headless, language: Locale) -> BTreeMap<String, Vec<String>> {
    crate::i18n::set_language(language);
    let translations = crate::i18n::translations(language);
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    h.app.ui.language = language;
    for (screen, steps) in screens() {
        // leave the screen before: its dialog, and any menu it opened
        h.app.ui.dialog = None;
        h.request("ui.key", json!({"key": "Escape"}), T);
        h.app.ui.left_panel = true;
        for (id, params) in steps {
            let r = if id == RIGHT_CLICK {
                let widget = params["widget"].as_str().unwrap_or_default();
                match h.app.widgets.iter().find(|(w, _)| w.starts_with(widget)).map(|(_, r)| r.center()) {
                    Some(c) => h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T),
                    None => json!({"ok": false, "error": format!("no widget {widget}")}),
                }
            } else if id == SHOW_PRESETS {
                if crate::panels::presets::shown(&h.app) {
                    json!({"ok": true})
                } else {
                    h.request("engine.execute", json!({"command": "panel.presets"}), T)
                }
            } else {
                h.request("engine.execute", json!({"command": id, "params": params}), T)
            };
            assert_eq!(r["ok"], true, "{screen}: {id} should open the screen: {r}");
            h.step();
        }
        // text needs no finished render: a fixed run of frames lays the screen out and opens its
        // menus (waiting for renders to settle never ends in Compare and Survey after the loupe)
        for _ in 0..FRAMES {
            h.step();
        }
        for text in h.painted_text() {
            let text = text.trim();
            let shown = language.tr(text) != text && !translations.contains(text);
            if shown && !NOT_UI_TEXT.iter().any(|(t, _)| *t == text) {
                let screens = found.entry(text.to_string()).or_default();
                if !screens.contains(&screen) {
                    screens.push(screen.clone());
                }
            }
        }
    }
    crate::i18n::set_language(Locale::En);
    found
}

#[test]
fn every_screen_paints_its_built_in_text_translated() {
    let mut lines = Vec::new();
    let mut h = demo();
    for language in [Locale::De, Locale::Ja] {
        for (text, screens) in untranslated(&mut h, language) {
            lines.push(format!("{}: {text:?} on {}", language.code(), screens.join(", ")));
        }
    }
    assert!(lines.is_empty(), "{} text(s) shown in English though the catalog translates them:\n{}", lines.len(), lines.join("\n"));
}
