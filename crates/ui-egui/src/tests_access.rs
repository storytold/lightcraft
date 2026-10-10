//! P6.3 accessibility: what a screen reader sees. Each module's AccessKit tree is walked headless
//! (AccessKit on) and every interactive node the fork's own UI draws must carry a name: a label,
//! a value, a description or a `labelled_by` relation.
//!
//! Scope: the Map, Book, Slideshow, Print and Web modules (everything they draw besides the shared
//! chrome), and in Library the module picker and the fork's own Navigator and Metadata panels.
//! Shared upstream widgets are left out (they are listed as upstream candidates in
//! `docs/accessibility.md`): the egui scroll bars, the shared filmstrip's cells, and whatever the
//! Library shell already draws without a name (the top bar, upstream panels).

use std::collections::BTreeSet;
use std::time::Duration;

use egui::accesskit::{Action, Node, NodeId, Rect as AkRect, Role};
use serde_json::json;

use crate::headless::Headless;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = DacApp::new(dac_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.view.ctx.enable_accesskit();
    h.settle(SETTLE);
    h
}

fn run(h: &mut Headless, id: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    for _ in 0..4 {
        h.step();
    }
}

fn interactive(node: &Node) -> bool {
    matches!(
        node.role(),
        Role::Button
            | Role::CheckBox
            | Role::RadioButton
            | Role::Slider
            | Role::SpinButton
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::PasswordInput
            | Role::ComboBox
            | Role::ColorWell
            | Role::Link
            | Role::Tab
            | Role::Switch
            | Role::MenuItem
    ) || node.supports_action(Action::Click)
}

fn named(node: &Node) -> bool {
    let has = |s: Option<&str>| s.is_some_and(|s| !s.trim().is_empty());
    has(node.label()) || has(node.value()) || has(node.description()) || !node.labelled_by().is_empty()
}

/// Shared widgets the test leaves to upstream: egui's scroll bars and the shared filmstrip cells.
fn shared_widget(film: &BTreeSet<NodeId>, id: NodeId, node: &Node) -> bool {
    node.role() == Role::ScrollBar || film.contains(&id)
}

/// The interactive nodes without a name: (id, role, bounds).
fn unnamed(h: &mut Headless) -> Vec<(NodeId, Role, Option<AkRect>)> {
    let film: BTreeSet<NodeId> = h.app.session.visible_cloned().iter().map(|p| egui::Id::new(("film", p.0)).accesskit_id()).collect();
    let Some(update) = &h.view.access else { return vec![] };
    update
        .nodes
        .iter()
        .filter(|(id, n)| interactive(n) && !named(n) && !shared_widget(&film, *id, n))
        .map(|(id, n)| (*id, n.role(), n.bounds()))
        .collect()
}

fn widget(h: &Headless, name: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == name).map(|(_, r)| *r)
}

fn inside(b: Option<AkRect>, r: egui::Rect) -> bool {
    b.is_some_and(|b| r.contains(egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)))
}

/// A Classic panel's area: from its header down to the next panel's header in the same column
/// (or far below, for the last one).
fn panel_area(h: &Headless, id: &str) -> Option<egui::Rect> {
    let head = widget(h, &format!("classicPanel:{id}"))?;
    let next = h
        .app
        .widgets
        .iter()
        .filter(|(w, r)| w.starts_with("classicPanel:") && (r.left() - head.left()).abs() < 40.0 && r.top() > head.top())
        .map(|(_, r)| r.top())
        .fold(f32::INFINITY, f32::min);
    Some(egui::Rect::from_min_max(head.left_top(), egui::pos2(head.right() + 40.0, next.min(1.0e6))))
}

#[test]
fn every_module_names_its_interactive_widgets() {
    let mut h = demo();
    run(&mut h, "module.library", json!({}));
    run(&mut h, "panel.left", json!({"show": true}));
    let mut failures = Vec::new();
    // Library: the module picker and the fork's own panels
    let owned: Vec<egui::Rect> =
        [widget(&h, "region:moduleBar"), panel_area(&h, "navigator"), panel_area(&h, "metadata")].into_iter().flatten().collect();
    assert!(!owned.is_empty(), "the module bar is on screen");
    let shell: BTreeSet<NodeId> = unnamed(&mut h).into_iter().map(|(id, _, _)| id).collect();
    for (_, role, b) in unnamed(&mut h) {
        if owned.iter().any(|r| inside(b, *r)) {
            failures.push(format!("library: unnamed {role:?} at {b:?}"));
        }
    }
    for m in ["map", "book", "slideshow", "print", "web"] {
        run(&mut h, "module.switch", json!({"module": m}));
        let total = h.view.access.as_ref().map_or(0, |u| u.nodes.len());
        assert!(total > 20, "{m}: the tree has its widgets ({total} nodes)");
        for (id, role, b) in unnamed(&mut h) {
            if !shell.contains(&id) {
                failures.push(format!("{m}: unnamed {role:?} at {b:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "interactive widgets without an accessible name:\n{}", failures.join("\n"));
}

/// Tab walks the Web module's settings column top to bottom (keyboard users reach every field).
#[test]
fn tab_moves_focus_down_the_web_settings() {
    let mut h = demo();
    run(&mut h, "module.switch", json!({"module": "web"}));
    let r = h.request("ui.clickWidget", json!({"id": "field:web:title"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let focused = |h: &Headless| h.view.ctx.memory(|m| m.focused()).and_then(|id| h.view.ctx.read_response(id)).map(|r| r.rect.top());
    let mut ys = vec![focused(&h).expect("the clicked field has focus")];
    for _ in 0..3 {
        let r = h.request("ui.key", json!({"key": "Tab"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        ys.push(focused(&h).expect("Tab moves focus to the next field"));
    }
    assert!(ys.windows(2).all(|w| w[1] > w[0]), "focus moves down the column: {ys:?}");
}

/// The fork's modules draw their text through `tr`: in German, no painted text is an English
/// message the German catalog translates differently (that text skipped `tr`).
#[test]
fn module_text_is_routed_through_tr() {
    let mut h = demo();
    h.app.ui.language = crate::i18n::Locale::De;
    crate::i18n::set_language(crate::i18n::Locale::De);
    let de: std::collections::BTreeMap<String, String> = serde_json::from_str(include_str!("../locales/de.json")).unwrap_or_default();
    let mut english = Vec::new();
    for m in ["map", "book", "slideshow", "print", "web"] {
        run(&mut h, "module.switch", json!({"module": m}));
        for t in h.painted_text() {
            if de.get(&t).is_some_and(|g| *g != t) {
                english.push(format!("{m}: {t:?}"));
            }
        }
    }
    crate::i18n::set_language(crate::i18n::Locale::En);
    english.sort();
    english.dedup();
    assert!(english.is_empty(), "shown in English although translated:\n{}", english.join("\n"));
}
