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
//!
//! A second gate, `every_actionable_control_is_reached_by_tab_and_shows_its_focus_or_is_a_listed_gap`,
//! presses Tab on the same screens: every actionable control must take the focus, and the frame must
//! paint something around it that it does not paint without the focus. Its baseline is
//! `tests/a11y-keyboard-gaps.json`, rewritten with `LIGHTCRAFT_A11Y_KEYBOARD_BASELINE`.
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

const BASELINE_FILE: &str = "a11y-gaps.json";

/// The sidebar lists Pictures, Desktop, Downloads and Home when this computer has them, found by a
/// worker, so the controls the audit counts would follow the machine that runs it. They are hidden
/// from Local here (as a person can), and what is audited is the same everywhere.
fn hide_the_computers_places(app: &mut LightcraftApp) {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    if home.is_empty() {
        return;
    }
    for sub in ["Pictures", "Desktop", "Downloads", ""] {
        let path = if sub.is_empty() { home.clone() } else { Path::new(&home).join(sub).to_string_lossy().to_string() };
        app.ui.hidden_locations.push(path);
    }
}

/// Run frames until the renders the screen asked for have come back (a photo the view shows is a
/// control only once its picture is there), so what the audit counts does not depend on how fast
/// the workers were.
fn settle_renders(h: &mut App) {
    let t0 = std::time::Instant::now();
    let mut quiet = 0;
    while quiet < 5 && t0.elapsed() < std::time::Duration::from_secs(20) {
        h.run_steps(1);
        if h.state().renderer.in_flight() == 0 {
            quiet += 1;
        } else {
            quiet = 0;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    assert!(quiet >= 5, "the renders did not finish in 20 s");
}

fn harness_with(setup: impl FnOnce(&mut LightcraftApp)) -> App {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    hide_the_computers_places(&mut app);
    setup(&mut app);
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).with_accessibility_check(false).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    settle_renders(&mut h);
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
    hide_the_computers_places(&mut app);
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
    settle_renders(&mut h);
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

/// A widget that takes the focus or a click is actionable whatever its role, unless nothing of it
/// is on screen and Tab cannot reach it (an empty label that takes clicks).
fn actionable(n: &egui_kittest::kittest::AccessKitNode<'_>) -> bool {
    let acts = |a| n.data().supports_action(a);
    if !interactive(n.role()) && !acts(Action::Focus) && !acts(Action::Click) {
        return false;
    }
    acts(Action::Focus) || !n.bounding_box().is_some_and(|b| b.width() <= 0.0 || b.height() <= 0.0)
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
        if !actionable(&n) {
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

fn parse_baseline(file: &str, text: &str) -> Gaps {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("tests/{file} is {{screen: {{problem: count}}}}: {e}"))
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

/// Compares what the screens show with the baseline file `file` (in `tests/`). `var` is the
/// environment variable that rewrites it. `shrink` drops the entries that are fixed.
///
/// With `strict` the file lists every screen checked, a clean one as `{}`, and `write` adds only the
/// screens the file does not list yet: a new problem on a listed screen still fails instead of being
/// written down. `write` also drops the entries that are fixed (like `shrink`). `regenerate` rewrites the whole file, for when keys move on purpose. Without it the
/// file lists only the screens with gaps and `write` regenerates it.
fn settle(file: &str, var: &str, found: &Gaps, strict: bool) -> Report {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join(file);
    let read = || parse_baseline(file, &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
    let shrunk = |mut kept: Gaps| {
        for (screen, known) in kept.iter_mut() {
            let counts = found.get(screen);
            known.retain(|key, _| counts.is_some_and(|c| c.contains_key(key)));
            for (key, n) in known.iter_mut() {
                *n = (*n).min(counts.map_or(0, |c| c[key]));
            }
        }
        if strict {
            kept.retain(|screen, _| found.contains_key(screen));
        } else {
            kept.retain(|_, known| !known.is_empty());
        }
        kept
    };
    let listed = match std::env::var(var).unwrap_or_default().as_str() {
        "write" if !strict => {
            std::fs::write(&path, render_baseline(found)).unwrap();
            found.clone()
        }
        "regenerate" if strict => {
            std::fs::write(&path, render_baseline(found)).unwrap();
            found.clone()
        }
        "write" => {
            let old = read();
            let mut next = shrunk(old.clone());
            for (screen, counts) in found {
                if !old.contains_key(screen) {
                    next.insert(screen.clone(), counts.clone());
                }
            }
            std::fs::write(&path, render_baseline(&next)).unwrap();
            next
        }
        "shrink" => {
            let kept = shrunk(read());
            std::fs::write(&path, render_baseline(&kept)).unwrap();
            kept
        }
        "" => read(),
        other => panic!("{var} is `write`{} or `shrink`, not {other:?}", if strict { ", `regenerate`" } else { "" }),
    };
    let report = compare(found, &listed);
    if !report.stale.is_empty() {
        eprintln!(
            "{} entries of tests/{file} are fixed now; `{var}=shrink cargo test -p lightcraft-ui-egui --test it accessibility` removes \
             them:\n{}",
            report.stale.len(),
            report.stale.join("\n")
        );
    }
    report
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
    let report = settle(BASELINE_FILE, "LIGHTCRAFT_A11Y_BASELINE", &found, false);
    assert!(report.new.is_empty(), "{}", failure_message(&report, &places));
}

const KEYBOARD_BASELINE_FILE: &str = "a11y-keyboard-gaps.json";

/// More Tab presses than any screen has controls: a screen where every press still reaches a control
/// not seen before has no end to its Tab order, and fails on that. (A trap that holds the focus on one
/// control ends the round at once, and shows as the controls behind it "not reached by Tab".)
const MAX_TABS: usize = 1500;

/// An actionable control as the keyboard sees it: what it is, and where.
struct Control {
    id: u64,
    role: Role,
    name: String,
    context: String,
    at: String,
}

impl Control {
    fn key(&self, what: &str) -> String {
        format!("{:?} {:?} {what} in {}", self.role, self.name, self.context)
    }
}

/// The controls a pointer can click or a screen reader can activate, without the ones that are
/// switched off (a disabled control is skipped by Tab on purpose).
fn controls<S>(h: &Harness<'_, S>) -> Vec<Control> {
    let mut out = Vec::new();
    for node in h.root().children_recursive() {
        let n = node.accesskit_node();
        let role = n.role();
        if role == Role::Window || role == Role::ScrollBar || n.data().is_disabled() || !actionable(&n) {
            continue;
        }
        // text egui lets the pointer select is a click target to AccessKit, not a control
        if role == Role::Label && !n.data().supports_action(Action::Focus) {
            continue;
        }
        let name = n.label().unwrap_or_default();
        let name = if is_name(&name) || role != Role::Label { name } else { n.value().unwrap_or_default() };
        let name = if is_name(&name) { name } else { String::new() };
        let b = n.bounding_box();
        out.push(Control {
            id: n.locate().0.0,
            role,
            name,
            context: context(&n),
            at: b.map_or("nowhere".into(), |b| format!("x {:.0}..{:.0}, y {:.0}..{:.0}", b.x0, b.x1, b.y0, b.y1)),
        });
    }
    out
}

/// What the last frame painted on and around `rect`: the shapes that lie wholly within a margin
/// of it. A focused control paints something extra there (a stroke, a fill, a ring). Shapes that
/// reach further (a window's backdrop, its shadow, a panel's background) are left out: they fade
/// and move on their own and are not the control's.
fn painted_near<S>(h: &Harness<'_, S>, rect: egui::Rect) -> Vec<egui::Shape> {
    let zone = rect.expand(6.0);
    h.output()
        .shapes
        .iter()
        .filter(|s| s.clip_rect.intersects(zone))
        .filter(|s| {
            let r = s.shape.visual_bounding_rect();
            r.is_finite() && zone.contains_rect(r)
        })
        .map(|s| s.shape.clone())
        .collect()
}

/// Presses Tab until the focus comes back to a control it already visited (a round, or a control
/// that holds the focus and lets Tab go nowhere). Returns the focus order (egui's widget ids), and
/// for each control focused whether the frame looked different with the focus.
fn tab_round<S>(h: &mut Harness<'_, S>) -> Result<(Vec<u64>, BTreeMap<u64, bool>), String> {
    let mut order: Vec<u64> = Vec::new();
    let mut shows: BTreeMap<u64, bool> = BTreeMap::new();
    let mut lost = 0;
    for _ in 0..MAX_TABS {
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
        let Some(focus) = h.ctx.memory(|m| m.focused()) else {
            lost += 1;
            if lost >= 3 {
                return Ok((order, shows));
            }
            continue;
        };
        lost = 0;
        let id = focus.value();
        // back at a control already visited: the round is over, or the focus is held on one control
        if order.contains(&id) {
            return Ok((order, shows));
        }
        let rect = h
            .root()
            .children_recursive()
            .map(|n| n.accesskit_node())
            .find(|n| n.locate().0.0 == id)
            .and_then(|n| n.bounding_box())
            .map(|b| egui::Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32)));
        if let Some(rect) = rect.filter(|r| r.width() > 0.0 && r.height() > 0.0) {
            // a widget that drew itself before the focus reached it paints the focus a frame later
            h.run_steps(1);
            let focused = painted_near(h, rect);
            h.run_steps(1);
            let focused_again = painted_near(h, rect);
            h.ctx.memory_mut(|m| m.surrender_focus(focus));
            h.run_steps(1);
            let plain = painted_near(h, rect);
            h.run_steps(1);
            let plain_again = painted_near(h, rect);
            // No verdict where the answer is not the focus alone: a field that takes the focus back
            // at once cannot be shown without it, and something that moves on its own (a fade, a
            // thumbnail arriving) would make the same control pass on one run and fail on the next.
            let held_back = h.ctx.memory(|m| m.focused()) == Some(focus);
            h.ctx.memory_mut(|m| m.request_focus(focus));
            h.run_steps(2);
            // the focused frame again, after the plain ones: what changed between the two (a picture that
            // came back, a row that moved) is not the focus
            let focused_last = painted_near(h, rect);
            if !held_back && focused == focused_again && plain == plain_again && focused == focused_last {
                shows.insert(id, focused != plain);
            }
        }
        order.push(id);
    }
    Err(format!("Tab reached a new control on each of {MAX_TABS} presses: the order never comes round"))
}

/// The keyboard problems of the screen the harness shows: `(problem, where)` for each control Tab
/// does not reach or that shows no focus, and how many controls Tab visited.
fn keyboard_problems<S>(h: &mut Harness<'_, S>) -> Result<(Vec<(String, String)>, usize), String> {
    let all = controls(h);
    let (order, shows) = tab_round(h)?;
    let mut keys = Vec::new();
    for c in &all {
        if !order.contains(&c.id) {
            keys.push((c.key("not reached by Tab"), c.at.clone()));
        } else if shows.get(&c.id) == Some(&false) {
            // counted by role and place, not by name: most of the app's buttons share one painting, and a
            // name per button would list them all on every screen they are on
            keys.push((format!("{:?} shows no focus in {}", c.role, c.context), c.at.clone()));
        }
    }
    Ok((keys, order.len()))
}

/// Every actionable control is reached by Tab and shows that it has the focus, or is a listed gap.
#[test]
fn every_actionable_control_is_reached_by_tab_and_shows_its_focus_or_is_a_listed_gap() {
    let mut found = Gaps::new();
    let mut most = 0;
    let mut places = BTreeMap::new();
    for (name, open) in screens() {
        let mut h = open();
        let count = audit(&h).0;
        assert!(count > 3, "{name}: the tree has too few interactive nodes ({count}): the screen did not open");
        let (keys, visited) = keyboard_problems(&mut h).unwrap_or_else(|e| panic!("{name}: {e}"));
        most = most.max(visited);
        let mut counts = BTreeMap::new();
        for (key, at) in keys {
            places.entry((name.clone(), key.clone())).or_insert(at);
            *counts.entry(key).or_insert(0) += 1;
        }
        found.insert(name, counts);
    }
    assert!(most > 10, "Tab reached at most {most} controls on any screen: the keyboard focus does not work in the harness");
    let report = settle(KEYBOARD_BASELINE_FILE, "LIGHTCRAFT_A11Y_KEYBOARD_BASELINE", &found, true);
    assert!(report.new.is_empty(), "{}", keyboard_failure_message(&report, &places));
}

/// The failures grouped by problem, like `failure_message`, with what to do about each kind.
fn keyboard_failure_message(report: &Report, places: &BTreeMap<(String, String), String>) -> String {
    let mut by_key: BTreeMap<&str, Vec<&Found>> = BTreeMap::new();
    for f in &report.new {
        by_key.entry(&f.key).or_default().push(f);
    }
    let mut out = format!(
        "{} keyboard problem(s) not in tests/{KEYBOARD_BASELINE_FILE}. A control Tab skips needs `Sense::click()` (not `Sense::CLICK`) \
         or `Sense::focusable_noninteractive()`; a control that shows no focus needs `ui.style().interact(&response)` (or a stroke \
         painted when `response.has_focus()`):\n",
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

    fn keyboard_keys_of(mut ui: impl FnMut(&mut egui::Ui) + 'static) -> Vec<String> {
        // (it builds unnamed controls on purpose; kittest 0.37 would refuse them before the checker sees them)
        let mut h = Harness::builder().with_accessibility_check(false).build_ui(move |u| ui(u));
        h.run();
        let (keys, _) = keyboard_problems(&mut h).expect("the Tab round ends");
        keys.into_iter().map(|p| p.0).collect()
    }

    /// A clickable widget that is not focusable: a mouse user can press it, Tab skips it.
    fn click_only(ui: &mut egui::Ui, label: &str) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 24.0), egui::Sense::hover());
        let r = ui.interact(rect, ui.scope_id().with(label), egui::Sense::CLICK);
        let label = label.to_string();
        r.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        ui.painter().rect_filled(rect, 2.0, egui::Color32::GRAY);
    }

    /// A focusable widget that paints nothing of its own, whatever its focus.
    fn unpainted(ui: &mut egui::Ui, label: &str) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 24.0), egui::Sense::hover());
        let r = ui.interact(rect, ui.scope_id().with(label), egui::Sense::click());
        let label = label.to_string();
        r.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        ui.painter().rect_filled(rect, 2.0, egui::Color32::GRAY);
    }

    #[test]
    fn standard_widgets_are_reached_and_show_their_focus() {
        let keys = keyboard_keys_of(|ui| {
            let _ = ui.button("Save");
            let _ = ui.button("Cancel");
            ui.checkbox(&mut true, "Show grid");
            ui.add(egui::Slider::new(&mut 0.5, 0.0..=1.0).text("Amount"));
        });
        assert!(keys.is_empty(), "{keys:?}");
    }

    #[test]
    fn a_clickable_widget_tab_skips_is_a_problem() {
        let keys = keyboard_keys_of(|ui| {
            let _ = ui.button("Save");
            click_only(ui, "Mouse only");
            let _ = ui.button("Cancel");
        });
        assert_eq!(keys, ["Button \"Mouse only\" not reached by Tab in (top level)"]);
    }

    #[test]
    fn a_focusable_widget_that_paints_no_focus_is_a_problem() {
        let keys = keyboard_keys_of(|ui| {
            let _ = ui.button("Save");
            unpainted(ui, "Silent");
            let _ = ui.button("Cancel");
        });
        assert_eq!(keys, ["Button shows no focus in (top level)"]);
    }

    #[test]
    fn a_disabled_button_is_not_expected_to_take_the_focus() {
        let keys = keyboard_keys_of(|ui| {
            let _ = ui.button("Save");
            ui.add_enabled(false, egui::Button::new("Greyed"));
        });
        assert!(keys.is_empty(), "{keys:?}");
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
        assert_eq!(parse_baseline(BASELINE_FILE, &render_baseline(&g)), g);
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
