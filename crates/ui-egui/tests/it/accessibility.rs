//! Accessibility audit: walks the AccessKit tree egui exposes to screen readers (VoiceOver,
//! Narrator, AT-SPI) on each main screen, headless, and fails on an interactive node with no
//! name, a slider without its value and range, a dialog without a title, or a canvas tool no key
//! can reach.
//!
//! The naming check is a gate with a baseline. A control counts as unnamed when its name (its label; for a plain
//! label, its text, which is its value) is empty or has no letter or digit in it: an icon-only
//! button is announced as just "button". Today's gaps are listed in `tests/a11y-gaps.json`
//! (screen, then the control, then how many of them): a gap not listed there, or one that occurs more often than its count, fails. A
//! control that gets a name does not have to touch the file: a stale entry is tolerated and
//! printed as a hint. `LIGHTCRAFT_A11Y_BASELINE=shrink` deletes the stale entries; `=write`
//! regenerates the whole file. A control is identified by its role, its name and the names of the
//! windows or groups around it, never by egui's widget ids, which move when unrelated widgets are
//! added.
use std::collections::BTreeMap;
use std::path::Path;

use egui::accesskit::{Action, Role};
use egui::vec2;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{Dialog, RightPanel, ViewMode},
};

type App = Harness<'static, LightcraftApp>;

fn harness_with(setup: impl FnOnce(&mut LightcraftApp)) -> App {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    setup(&mut app);
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).with_accessibility_check(false).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    h
}

fn harness(view: ViewMode, right: RightPanel) -> App {
    harness_with(|app| {
        app.ui.view = view;
        app.ui.right = right;
    })
}

/// What a People screen shows beyond the sidebar.
#[derive(Clone, Copy)]
enum People {
    /// The cards of the named people and the unnamed face's tile.
    Everyone,
    /// The unnamed face selected, so the name field and its buttons under the tiles are drawn.
    UnnamedSelected,
    /// One person's page: a tile for each of their faces.
    Page,
}

/// The People view over the demo library with faces in it: two named people and one unnamed face.
fn people_harness(what: People) -> App {
    use lightcraft_catalog::Op;
    use lightcraft_meta::{Region, RegionKind};
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let ids: Vec<_> = app.session.catalog.photos().map(|p| p.id).take(3).collect();
    let face = |x: f64, name: Option<&str>| Region {
        rect: lightcraft_geom::Rect { x0: x, y0: 0.2, x1: x + 0.25, y1: 0.6 },
        kind: RegionKind::Face,
        name: name.map(str::to_string),
        description: None,
    };
    for (id, regions) in [
        (ids[0], vec![face(0.1, Some("Jane Doe")), face(0.5, None)]),
        (ids[1], vec![face(0.3, Some("Jane Doe"))]),
        (ids[2], vec![face(0.3, Some("John Roe"))]),
    ] {
        let mut meta = app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = regions;
        app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }
    app.ui.left_panel = true;
    app.ui.view = ViewMode::People;
    match what {
        People::Everyone => {}
        People::UnnamedSelected => {
            app.ui.unnamed_selected.insert((ids[0].0, 1));
        }
        People::Page => app.ui.person_page = Some("Jane Doe".to_string()),
    }
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).with_accessibility_check(false).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    // what the screen is about is on it, so a seeding that failed cannot pass as an empty, clean screen
    // a plain label's text is the node's value, a control's name its label
    let mut all = names(&h);
    all.extend(h.root().children_recursive().filter_map(|n| n.accesskit_node().value()));
    let shown = |text: &str| all.iter().any(|n| n.contains(text));
    match what {
        People::Everyone => assert!(shown("Jane Doe") && shown("Unnamed face"), "the cards and the unnamed tile: {all:?}"),
        People::UnnamedSelected => assert!(shown("Name face"), "the unnamed faces' name bar: {all:?}"),
        People::Page => assert!(shown("2 faces"), "the person's page with both of their faces: {all:?}"),
    }
    h
}

fn interactive(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::CheckBox
            | Role::Switch
            | Role::RadioButton
            | Role::Slider
            | Role::SpinButton
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::SearchInput
            | Role::ComboBox
            | Role::EditableComboBox
            | Role::Tab
            | Role::Link
            | Role::MenuItem
            | Role::MenuItemCheckBox
            | Role::MenuItemRadio
            | Role::ListBoxOption
    )
}

/// A name a screen reader can say: it has a letter or a digit in it. "", "  ", "⚙" and "×" are
/// read as nothing, or as the symbol's Unicode name.
fn is_name(name: &str) -> bool {
    name.chars().any(char::is_alphanumeric)
}

/// The names of the (up to three) nearest named ancestors, outermost first.
fn context(n: &egui_kittest::kittest::AccessKitNode<'_>) -> String {
    let mut names = Vec::new();
    let mut up = n.parent();
    while let Some(p) = up {
        if let Some(l) = p.label().filter(|l| is_name(l)) {
            names.push(l);
            if names.len() == 3 {
                break;
            }
        }
        up = p.parent();
    }
    names.reverse();
    if names.is_empty() { "(top level)".into() } else { names.join(" › ") }
}

/// One problem: its stable `key` (what the baseline lists) and where the node is, to find it by.
struct Problem {
    key: String,
    at: String,
}

/// The interactive nodes in the harness's current tree, and the problems found in them.
fn audit<S>(h: &Harness<'_, S>) -> (usize, Vec<Problem>) {
    let mut keys = Vec::new();
    let mut count = 0;
    let add = |keys: &mut Vec<Problem>, key: String, n: &egui_kittest::kittest::AccessKitNode<'_>| {
        keys.push(Problem {
            key,
            at: n.bounding_box().map_or("nowhere".into(), |b| format!("x {:.0}..{:.0}, y {:.0}..{:.0}", b.x0, b.x1, b.y0, b.y1)),
        });
    };
    for node in h.root().children_recursive() {
        let n = node.accesskit_node();
        let role = n.role();
        // a window is not a control, but its title is what a screen reader says on entering it
        if role == Role::Window {
            if !n.label().is_some_and(|l| is_name(&l)) {
                add(&mut keys, format!("Window without a title in {}", context(&n)), &n);
            }
            continue;
        }
        // egui's own scroll bars are announced by their role, as native ones are
        if role == Role::ScrollBar {
            continue;
        }
        // a widget that takes the focus or a click is interactive whatever its role
        let acts = |a| n.data().supports_action(a);
        if !interactive(role) && !acts(Action::Focus) && !acts(Action::Click) {
            continue;
        }
        // nothing of it is on screen and Tab cannot reach it (an empty label that takes clicks)
        if !acts(Action::Focus) && n.bounding_box().is_some_and(|b| b.width() <= 0.0 || b.height() <= 0.0) {
            continue;
        }
        count += 1;
        let name = n.label().unwrap_or_default();
        // a label's text is its value; any other control's value (typed text, "+0.50") is not its name
        let name = if is_name(&name) || role != Role::Label { name } else { n.value().unwrap_or_default() };
        let shown = if is_name(&name) { name.clone() } else { String::new() };
        if shown.is_empty() {
            add(&mut keys, format!("{role:?} without a name in {}", context(&n)), &n);
        }
        if role == Role::Slider && (n.numeric_value().is_none() || n.min_numeric_value().is_none() || n.max_numeric_value().is_none()) {
            add(&mut keys, format!("Slider {shown:?} without value or range in {}", context(&n)), &n);
        }
    }
    (count, keys)
}

/// screen → problem → how many nodes have it.
type Gaps = BTreeMap<String, BTreeMap<String, usize>>;

fn tally(problems: &[Problem]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for p in problems {
        *counts.entry(p.key.clone()).or_insert(0) += 1;
    }
    counts
}

/// A problem on a screen, `n` times where the baseline lists `listed`.
#[derive(Debug, PartialEq)]
struct Found {
    screen: String,
    key: String,
    n: usize,
    listed: usize,
}

#[derive(Debug, Default, PartialEq)]
struct Report {
    /// Problems the baseline does not list (or lists fewer of): the failures.
    new: Vec<Found>,
    /// Baseline entries that no longer occur (or occur fewer times): a hint, never a failure.
    stale: Vec<String>,
}

fn compare(found: &Gaps, listed: &Gaps) -> Report {
    let mut report = Report::default();
    let none = BTreeMap::new();
    for (screen, counts) in found {
        let known = listed.get(screen).unwrap_or(&none);
        for (key, n) in counts {
            let before = known.get(key).copied().unwrap_or(0);
            if *n > before {
                report.new.push(Found { screen: screen.clone(), key: key.clone(), n: *n, listed: before });
            }
        }
    }
    for (screen, known) in listed {
        let counts = found.get(screen).unwrap_or(&none);
        for (key, before) in known {
            let n = counts.get(key).copied().unwrap_or(0);
            if n < *before {
                report.stale.push(format!("{screen}: {key} (×{n}, {before} listed)"));
            }
        }
    }
    report
}

fn parse_baseline(text: &str) -> Gaps {
    serde_json::from_str(text).expect("tests/a11y-gaps.json is {screen: {problem: count}}")
}

fn render_baseline(gaps: &Gaps) -> String {
    let mut out = serde_json::to_string_pretty(gaps).unwrap();
    out.push('\n');
    out
}

/// The failures grouped by problem: a control in the sidebar is on every screen, and one line
/// says so. `places` has where the first node of each (screen, problem) is.
fn failure_message(report: &Report, places: &BTreeMap<(String, String), String>) -> String {
    let mut by_key: BTreeMap<&str, Vec<&Found>> = BTreeMap::new();
    for f in &report.new {
        by_key.entry(&f.key).or_default().push(f);
    }
    let mut out = format!(
        "{} accessibility problem(s) not in tests/a11y-gaps.json. Give the control a name a screen reader can say (a text label, \
         `widgets::screen_reader_name`, or the icon button's tooltip text as its name):\n",
        by_key.len()
    );
    for (key, found) in by_key {
        let first = found[0];
        let screens: Vec<&str> = found.iter().take(4).map(|f| f.screen.as_str()).collect();
        let more = if found.len() > 4 { format!(" and {} more", found.len() - 4) } else { String::new() };
        let at = places.get(&(first.screen.clone(), key.to_string())).map_or("", String::as_str);
        let listed = if first.listed > 0 { format!(", {} listed", first.listed) } else { String::new() };
        out += &format!("- {key} (×{}{listed}) on {}{more}; first on {} at {at}\n", first.n, screens.join(", "), first.screen);
    }
    out
}

/// The dialog windows in the tree.
fn windows<S>(h: &Harness<'_, S>) -> usize {
    h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::Window).count()
}

/// Every name in the tree, sorted.
fn names<S>(h: &Harness<'_, S>) -> Vec<String> {
    let mut all: Vec<String> = h.root().children_recursive().filter_map(|n| n.accesskit_node().label()).collect();
    all.sort();
    all
}

/// Every view the gate walks; `every_view_is_a_screen_of_the_gate` names each `ViewMode` variant.
const VIEWS: [ViewMode; 7] =
    [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail, ViewMode::Compare, ViewMode::Survey, ViewMode::Reference, ViewMode::People];

type Screen = (String, Box<dyn Fn() -> App>);

fn screen(name: impl Into<String>, open: impl Fn() -> App + 'static) -> Screen {
    (name.into(), Box::new(open))
}

/// A dialog opened as a command does, with a title and every control named.
fn command_dialog(command: &'static str) -> Screen {
    screen(format!("Dialog {command}"), move || {
        let mut h = harness(ViewMode::PhotoGrid, RightPanel::None);
        if let Err(e) = h.state_mut().run(command, serde_json::json!({})) {
            panic!("{command}: does not open ({e})");
        }
        h.run_steps(6);
        assert!(windows(&h) > 0, "{command}: no dialog window is open");
        h
    })
}

/// A dialog set in the UI state: it must be on screen as a window.
fn state_dialog(name: String, dialog: Dialog) -> Screen {
    screen(format!("Dialog {name}"), move || {
        let h = harness_with(|app| app.ui.dialog = Some(dialog.clone()));
        assert!(windows(&h) > 0, "{name}: no dialog window is open");
        h
    })
}

/// A menu of the in-window menu bar, opened by a click on its title.
fn menu(title: &'static str) -> Screen {
    screen(format!("Menu {title}"), move || {
        let mut h = harness(ViewMode::PhotoGrid, RightPanel::None);
        let before = audit(&h).0;
        h.get_by_label(title).click();
        h.run_steps(8);
        assert!(audit(&h).0 > before, "the {title} menu did not open");
        h
    })
}

/// A context menu: the secondary click on the first control whose name contains `label`.
fn context_menu(what: &'static str, label: &'static str) -> Screen {
    screen(format!("Context menu {what}"), move || {
        let mut h = harness(ViewMode::PhotoGrid, RightPanel::None);
        let before = audit(&h).0;
        h.query_all_by_label_contains(label).next().unwrap_or_else(|| panic!("no control named {label:?}")).click_secondary();
        h.run_steps(8);
        assert!(audit(&h).0 > before, "the context menu of {label} did not open");
        h
    })
}

/// The screens the gate walks: every view, every right panel, the dialogs, Settings' tabs, the
/// import review, the menus and two context menus. The left sidebar is part of every one of them.
fn screens() -> Vec<Screen> {
    let mut all = Vec::new();
    for view in VIEWS {
        // People needs faces in the library: its screens are below
        if view != ViewMode::People {
            all.push(screen(format!("View {view:?}"), move || harness(view, RightPanel::None)));
        }
    }
    all.push(screen("View People", || people_harness(People::Everyone)));
    all.push(screen("View People with an unnamed face selected", || people_harness(People::UnnamedSelected)));
    all.push(screen("View People of one person", || people_harness(People::Page)));
    for right in [
        RightPanel::Edit,
        RightPanel::Profiles,
        RightPanel::Crop,
        RightPanel::Remove,
        RightPanel::Masking,
        RightPanel::RedEye,
        RightPanel::Versions,
        RightPanel::Activity,
        RightPanel::Keywords,
        RightPanel::Info,
    ] {
        all.push(screen(format!("Right panel {right:?}"), move || harness(ViewMode::Detail, right)));
    }
    for command in [
        "dialog.export",
        "dialog.copySettings",
        "dialog.pasteSettings",
        "dialog.createPreset",
        "dialog.rename",
        "dialog.newAlbum",
        "dialog.newSmartAlbum",
        "dialog.captureTime",
        "dialog.labelNames",
        "dialog.allMetadata",
        "dialog.saveMetadataPreset",
        "dialog.autoStack",
        "dialog.contactSheet",
    ] {
        all.push(command_dialog(command));
    }
    for (name, dialog) in [("About", Dialog::About), ("ViewOptions", Dialog::ViewOptions)] {
        all.push(state_dialog(name.into(), dialog));
    }
    // Keyboard Shortcuts has a row per command with a shortcut: searched for one command, so a new
    // command does not change what is counted
    all.push(screen("Dialog Shortcuts", || {
        let mut h = harness_with(|app| app.ui.dialog = Some(Dialog::Shortcuts));
        h.ctx.data_mut(|d| d.insert_temp(egui::Id::unique("shortcuts-search"), "undo".to_string()));
        h.run_steps(6);
        assert!(windows(&h) > 0, "Shortcuts: no dialog window is open");
        h
    }));
    for (tab, _) in lightcraft_ui_egui::panels::settings::TABS {
        all.push(screen(format!("Dialog Settings {tab}"), move || {
            let open = |tab: &str| harness_with(|app| app.ui.dialog = Some(Dialog::Settings { tab: tab.into() }));
            let h = open(tab);
            assert!(windows(&h) > 0, "Settings {tab}: no dialog window is open");
            // the tab's own body is drawn: another tab's body has other controls in it
            let other = open(if *tab == "general" { "import" } else { "general" });
            assert_ne!(names(&h), names(&other), "Settings {tab}: the tab's body is the same as another tab's");
            h
        }));
    }
    all.push(screen("Import review", || {
        let h = harness_with(|app| {
            let candidates = (0..6)
                .map(|i| lightcraft_engine::import::ImportCandidate {
                    path: format!("/a11y-gate/img{i}.jpg"),
                    name: format!("img{i}.jpg"),
                    ..Default::default()
                })
                .collect();
            app.ui.dialog = Some(Dialog::Import { opts: Box::new(lightcraft_ui_egui::import::ImportDialog::new(candidates)) });
        });
        assert!(windows(&h) > 0, "Import review: no dialog window is open");
        h
    }));
    for title in ["File", "Edit", "View", "Photo", "Window", "Help"] {
        all.push(menu(title));
    }
    all.push(context_menu("of a photo", ".JPG"));
    all.push(context_menu("of an album", "Coastlines"));
    all
}

fn baseline_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/a11y-gaps.json")
}

/// Every view in `VIEWS` is a screen of the gate. The match has no wildcard, so a new `ViewMode`
/// does not compile until it is named here; add it to `VIEWS` (above `screens()`) at the same time.
#[test]
fn every_view_is_a_screen_of_the_gate() {
    for view in VIEWS {
        match view {
            ViewMode::PhotoGrid
            | ViewMode::SquareGrid
            | ViewMode::Detail
            | ViewMode::Compare
            | ViewMode::Survey
            | ViewMode::Reference
            | ViewMode::People => {}
        }
    }
    let audited: Vec<String> = screens().into_iter().map(|(name, _)| name).collect();
    for view in VIEWS {
        let name = format!("View {view:?}");
        assert!(audited.contains(&name), "{name} is not audited: add it to screens() ({audited:?})");
    }
}

#[test]
fn every_actionable_control_is_named_or_a_listed_gap() {
    assert_eq!(
        windows(&harness(ViewMode::PhotoGrid, RightPanel::None)),
        0,
        "a plain view has no dialog window: the dialog screens' open check means something"
    );
    let mut found = Gaps::new();
    let mut places = BTreeMap::new();
    for (name, open) in screens() {
        let h = open();
        let (count, keys) = audit(&h);
        assert!(count > 3, "{name}: the tree has too few interactive nodes ({count}): the screen did not open");
        let counts = tally(&keys);
        for p in keys {
            places.entry((name.clone(), p.key)).or_insert(p.at);
        }
        if !counts.is_empty() {
            found.insert(name, counts);
        }
    }
    let path = baseline_path();
    let read = || parse_baseline(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
    let listed = match std::env::var("LIGHTCRAFT_A11Y_BASELINE").unwrap_or_default().as_str() {
        "write" => {
            std::fs::write(&path, render_baseline(&found)).unwrap();
            return;
        }
        // drops what is fixed, keeps the rest, and still checks the screens against it
        "shrink" => {
            let mut kept = read();
            for (screen, known) in kept.iter_mut() {
                let counts = found.get(screen);
                known.retain(|key, _| counts.is_some_and(|c| c.contains_key(key)));
                for (key, n) in known.iter_mut() {
                    *n = (*n).min(counts.map_or(0, |c| c[key]));
                }
            }
            kept.retain(|_, known| !known.is_empty());
            std::fs::write(&path, render_baseline(&kept)).unwrap();
            kept
        }
        "" => read(),
        other => panic!("LIGHTCRAFT_A11Y_BASELINE is `write` or `shrink`, not {other:?}"),
    };
    let report = compare(&found, &listed);
    if !report.stale.is_empty() {
        eprintln!(
            "{} entries of tests/a11y-gaps.json are fixed now; `LIGHTCRAFT_A11Y_BASELINE=shrink cargo test -p lightcraft-ui-egui --test \
             it accessibility` removes them:\n{}",
            report.stale.len(),
            report.stale.join("\n")
        );
    }
    assert!(report.new.is_empty(), "{}", failure_message(&report, &places));
}

/// The checker on a tiny UI, so it is trusted before it is pointed at the app.
mod checker {
    use super::*;

    fn keys_of(mut ui: impl FnMut(&mut egui::Ui) + 'static) -> Vec<String> {
        // (it builds unnamed controls on purpose; kittest 0.37 would refuse them before the checker sees them)
        let mut h = Harness::builder().with_accessibility_check(false).build_ui(move |u| ui(u));
        h.run();
        audit(&h).1.into_iter().map(|p| p.key).collect()
    }

    #[test]
    fn an_unnamed_icon_button_is_a_problem() {
        assert_eq!(keys_of(|ui| drop(ui.button(""))), ["Button without a name in (top level)"]);
    }

    #[test]
    fn a_button_of_only_whitespace_or_a_symbol_is_a_problem() {
        let keys = keys_of(|ui| {
            let _ = ui.button("   ");
            let _ = ui.button("⚙");
            let _ = ui.button("×");
        });
        assert_eq!(keys, ["Button without a name in (top level)"; 3]);
    }

    /// A control's typed text or shown value is not its name: only a label's text is.
    #[test]
    fn a_field_with_text_in_it_but_no_caption_is_a_problem() {
        let keys = keys_of(|ui| drop(ui.add(egui::TextEdit::singleline(&mut "typed text".to_string()))));
        assert_eq!(keys, ["TextInput without a name in (top level)"]);
    }

    #[test]
    fn a_named_button_and_a_checkbox_pass() {
        assert!(
            keys_of(|ui| {
                let _ = ui.button("Save");
                let _ = ui.button("4K");
                ui.checkbox(&mut true, "Show grid");
            })
            .is_empty()
        );
    }

    #[test]
    fn the_context_is_the_names_around_the_control() {
        let keys = keys_of(|ui| {
            let ctx = ui.ctx().clone();
            egui::Window::new("Tools").show(&ctx, |ui| drop(ui.button("")));
        });
        // egui's own window frame adds a node of its own to the list
        assert!(keys.contains(&"Button without a name in Tools".to_string()), "{keys:?}");
    }

    fn gaps(screen: &str, key: &str, n: usize) -> Gaps {
        Gaps::from([(screen.to_string(), BTreeMap::from([(key.to_string(), n)]))])
    }

    #[test]
    fn a_listed_gap_passes() {
        let report = compare(&gaps("Menu File", "Button without a name in X", 2), &gaps("Menu File", "Button without a name in X", 2));
        assert_eq!(report, Report::default());
    }

    #[test]
    fn a_gap_the_baseline_lacks_fails_even_beside_a_listed_one() {
        let found = Gaps::from([(
            "Menu File".to_string(),
            BTreeMap::from([("Button without a name in X".to_string(), 1), ("Button without a name in Y".to_string(), 1)]),
        )]);
        let report = compare(&found, &gaps("Menu File", "Button without a name in X", 1));
        assert_eq!(report.new, [Found { screen: "Menu File".into(), key: "Button without a name in Y".into(), n: 1, listed: 0 }]);
        assert!(report.stale.is_empty());
    }

    #[test]
    fn one_more_of_a_listed_gap_fails() {
        let report = compare(&gaps("Menu File", "Button without a name in X", 3), &gaps("Menu File", "Button without a name in X", 2));
        assert_eq!(report.new, [Found { screen: "Menu File".into(), key: "Button without a name in X".into(), n: 3, listed: 2 }]);
    }

    #[test]
    fn a_fixed_gap_is_a_hint_not_a_failure() {
        let report = compare(&Gaps::new(), &gaps("Menu File", "Button without a name in X", 2));
        assert!(report.new.is_empty());
        assert_eq!(report.stale, ["Menu File: Button without a name in X (×0, 2 listed)"]);
    }

    #[test]
    fn the_baseline_round_trips() {
        let g = gaps("Menu File", "Button without a name in X", 2);
        assert_eq!(parse_baseline(&render_baseline(&g)), g);
    }
}

fn slider_value(h: &Harness<'_, LightcraftApp>, name: &str) -> Option<f64> {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .find(|n| n.role() == Role::Slider && n.label().as_deref() == Some(name))
        .and_then(|n| n.numeric_value())
}

/// A screen reader's Increment on a develop slider moves it one step, as an arrow key does.
#[test]
fn screen_reader_increment_moves_a_slider() {
    let mut h = harness(ViewMode::Detail, RightPanel::Edit);
    let before = slider_value(&h, "Exposure").expect("Exposure is in the tree");
    let target = h
        .root()
        .children_recursive()
        .find(|n| n.accesskit_node().role() == Role::Slider && n.accesskit_node().label().as_deref() == Some("Exposure"))
        .unwrap()
        .accesskit_node()
        .locate();
    h.event(egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::Increment,
        target_node: target.0,
        target_tree: target.1,
        data: None,
    }));
    h.run_steps(4);
    let after = slider_value(&h, "Exposure").unwrap();
    assert!(after > before, "{before} -> {after}");
}

/// The canvas tools' settings (crop angle and offsets, spot size, mask amount) are reachable with
/// Tab alone, and the arrow keys then adjust them.
#[test]
fn canvas_tool_sliders_are_reachable_with_tab() {
    for right in [RightPanel::Crop, RightPanel::Remove] {
        let mut h = harness(ViewMode::Detail, right);
        let mut reached = None;
        for _ in 0..300 {
            h.key_press(egui::Key::Tab);
            h.run_steps(1);
            let focused = h.root().children_recursive().map(|n| n.accesskit_node()).find(|n| n.is_focused() && n.role() == Role::Slider);
            if let Some(n) = focused {
                reached = Some((n.label().unwrap_or_default(), n.numeric_value()));
                break;
            }
        }
        let (name, before) = reached.unwrap_or_else(|| panic!("{right:?}: no slider takes the focus with Tab"));
        h.key_press(egui::Key::ArrowUp);
        h.run_steps(4);
        let after = slider_value(&h, &name);
        assert_ne!(before, after, "{right:?}: ArrowUp on the Tab-focused {name:?}");
    }
}

/// The canvas handles are in the tree by name, with their position as the value.
#[test]
fn canvas_handles_are_named_with_their_position() {
    let h = harness(ViewMode::Detail, RightPanel::Crop);
    let found: Vec<(String, String)> = h
        .root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .filter(|n| n.role() == Role::Button)
        .map(|n| (n.label().unwrap_or_default(), n.value().unwrap_or_default()))
        .collect();
    for name in ["Crop top-left corner", "Crop right edge", "Crop box"] {
        assert!(found.iter().any(|(l, v)| l == name && v.contains('%')), "{name}: {found:?}");
    }
    let h = harness(ViewMode::Detail, RightPanel::Remove);
    assert!(h.root().children_recursive().any(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with("New spot"))));
}

/// Tab walks each right panel from top to bottom (rows left to right): the order a sighted user
/// reads it in. Each step down may not jump back up by more than a row.
#[test]
fn tab_walks_the_right_panels_top_to_bottom() {
    for right in [RightPanel::Edit, RightPanel::Crop, RightPanel::Remove, RightPanel::Masking] {
        let mut h = harness(ViewMode::Detail, right);
        let panel_left = 1500.0 - h.state().ui.right_width - 10.0;
        // the tool strip beside the panel is a column of its own, walked before it
        let strip = h
            .root()
            .children_recursive()
            .map(|n| n.accesskit_node())
            .find(|n| n.label().as_deref() == Some("Edit (E)"))
            .and_then(|n| n.bounding_box())
            .map(|b| b.x0 as f32)
            .unwrap_or(f32::INFINITY);
        let mut seen: Vec<(String, f32)> = Vec::new();
        for _ in 0..120 {
            h.key_press(egui::Key::Tab);
            h.run_steps(1);
            let Some(n) = h.root().children_recursive().map(|n| n.accesskit_node()).find(|n| n.is_focused()) else { continue };
            let Some(b) = n.bounding_box() else { continue };
            if (b.x0 as f32) < panel_left || (b.x0 as f32) >= strip - 1.0 {
                continue;
            }
            let name = n.label().unwrap_or_default();
            if seen.iter().any(|(s, y)| *s == name && (*y - b.y0 as f32).abs() < 1.0) && !name.is_empty() {
                break;
            }
            seen.push((name, b.y0 as f32));
        }
        assert!(seen.len() > 3, "{right:?}: Tab reaches the panel: {seen:?}");
        for w in seen.windows(2) {
            assert!(w[1].1 >= w[0].1 - 30.0, "{right:?}: Tab jumps up from {:?} to {:?}\n{seen:?}", w[0], w[1]);
        }
    }
}
