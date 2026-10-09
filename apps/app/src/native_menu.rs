//! The native macOS menu bar (via `muda`, pure Rust over AppKit), generated from the same model as
//! the in-window menus ([`dac_ui_egui::menubar`]): the registry's menu paths, shortcuts,
//! enabled and checked state, and live labels ("Undo Exposure").
//!
//! Key handling: AppKit offers a key press to the menu bar only when it carries ⌘ or ⌃, or is a
//! function key (F1…). Every other key (`E`, `1`, `⇧P`, `⌥Y`…) goes straight to the window, whose
//! winit view always takes it, so a menu item never fires from it. Hence
//! - shortcuts the menu bar really receives ([`menu_delivers`]) are listed in
//!   `DacApp::native_shortcuts` and skipped by the egui shortcut handler (nothing fires
//!   twice); the others stay on their menu items for display and egui runs them;
//! - while a text field has keyboard focus, accelerators without ⌘ (`G`, `1`, `Delete`…) and the
//!   text-editing ones (⌘A/⌘C/⌘V/⌘X/⌘Z) are removed, so typing and text editing work; they come
//!   back when the field loses focus.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, channel};

use dac_ui_egui::DacApp;
use dac_ui_egui::menubar::{MenuNode, menu_bar};
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use serde_json::Value;

const QUIT: &str = "app.quit";
/// Settings… (the app menu on macOS; Edit elsewhere).
const SETTINGS: &str = "app.settings";
const SETTINGS_KEY: &str = "Cmd+,";
/// Keys whose meaning depends on the panel (X = reject or swap crop aspect, ⌫ = delete the photo
/// or the active mask): left to the egui handler, which knows the context.
const CONTEXTUAL: &[&str] = &["X", "Delete"];
/// Shortcuts text fields need while they have focus.
const TEXT_EDIT: &[&str] = &["Cmd+A", "Cmd+C", "Cmd+V", "Cmd+X", "Cmd+Z", "Cmd+Shift+Z"];

enum Handle {
    Plain(MenuItem),
    Check(CheckMenuItem),
}

struct Item {
    handle: Handle,
    cmd: String,
    params: Value,
    shortcut: Option<String>,
    accel: Option<Accelerator>,
    label: String,
    enabled: bool,
    checked: Option<bool>,
}

pub struct NativeMenu {
    menu: Menu,
    items: HashMap<String, Item>,
    rx: Receiver<String>,
    /// Structure of the installed menu (ids, kinds, submenu names); a change rebuilds it.
    structure: String,
    text_focus: bool,
    /// The keyboard shortcuts editor is recording a key: every accelerator is off so the key
    /// press reaches it.
    capturing: bool,
}

/// `Cmd+Shift+Z` → a muda accelerator (`None` for keys we leave to egui, e.g. Escape).
fn accelerator(sc: &str) -> Option<Accelerator> {
    let mut mods = Modifiers::empty();
    let mut key = None;
    for part in sc.split('+') {
        match part {
            "Cmd" => mods |= Modifiers::META,
            "Shift" => mods |= Modifiers::SHIFT,
            "Alt" => mods |= Modifiers::ALT,
            "Ctrl" => mods |= Modifiers::CONTROL,
            k => key = Some(k),
        }
    }
    let code = match key? {
        k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphabetic() => {
            const LETTERS: [Code; 26] = [
                Code::KeyA,
                Code::KeyB,
                Code::KeyC,
                Code::KeyD,
                Code::KeyE,
                Code::KeyF,
                Code::KeyG,
                Code::KeyH,
                Code::KeyI,
                Code::KeyJ,
                Code::KeyK,
                Code::KeyL,
                Code::KeyM,
                Code::KeyN,
                Code::KeyO,
                Code::KeyP,
                Code::KeyQ,
                Code::KeyR,
                Code::KeyS,
                Code::KeyT,
                Code::KeyU,
                Code::KeyV,
                Code::KeyW,
                Code::KeyX,
                Code::KeyY,
                Code::KeyZ,
            ];
            LETTERS[(k.as_bytes()[0].to_ascii_uppercase() - b'A') as usize]
        }
        k if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() => {
            const DIGITS: [Code; 10] = [
                Code::Digit0,
                Code::Digit1,
                Code::Digit2,
                Code::Digit3,
                Code::Digit4,
                Code::Digit5,
                Code::Digit6,
                Code::Digit7,
                Code::Digit8,
                Code::Digit9,
            ];
            DIGITS[(k.as_bytes()[0] - b'0') as usize]
        }
        "[" => Code::BracketLeft,
        "]" => Code::BracketRight,
        "\\" => Code::Backslash,
        "/" => Code::Slash,
        "=" => Code::Equal,
        "-" => Code::Minus,
        "'" => Code::Quote,
        "," => Code::Comma,
        "Delete" => Code::Backspace,
        // with modifiers only: a bare Enter belongs to text fields and tools
        "Enter" if !mods.is_empty() => Code::Enter,
        "F1" => Code::F1,
        "F2" => Code::F2,
        "F3" => Code::F3,
        "F4" => Code::F4,
        "F5" => Code::F5,
        "F6" => Code::F6,
        "F7" => Code::F7,
        "F8" => Code::F8,
        "F9" => Code::F9,
        "F10" => Code::F10,
        "F11" => Code::F11,
        "F12" => Code::F12,
        _ => return None,
    };
    Some(Accelerator::new(mods, code))
}

/// Accelerators that must step aside while a text field has focus.
fn yields_to_text(sc: &str) -> bool {
    !sc.contains("Cmd") && !sc.contains("Ctrl") || TEXT_EDIT.contains(&sc)
}

/// Whether AppKit hands this shortcut to the menu bar: only key presses with ⌘ or ⌃, and function
/// keys. A plain `C` or `⇧P` never reaches it (the window's view takes the key), so egui has to run
/// those even though their menu item shows the key.
fn menu_delivers(sc: &str) -> bool {
    sc.split('+').any(|part| matches!(part, "Cmd" | "Ctrl") || part.strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok()))
}

/// The shortcuts the menu bar runs itself, for `DacApp::native_shortcuts`.
fn owned_by_menu<'a>(installed: impl Iterator<Item = &'a str>, text_focus: bool) -> HashSet<String> {
    installed.filter(|sc| menu_delivers(sc) && !(text_focus && yields_to_text(sc))).map(str::to_string).collect()
}

/// `&` marks a mnemonic in muda labels.
fn label_text(s: &str) -> String {
    dac_ui_egui::i18n::tr(s).replace('&', "&&")
}

fn structure_of(bar: &[(String, Vec<MenuNode>)]) -> String {
    fn walk(n: &[MenuNode], out: &mut String) {
        for x in n {
            match x {
                MenuNode::Item { id, params, checked, shortcut, .. } => {
                    out.push_str(&MenuNode::key(id, params));
                    out.push(if checked.is_some() { 'c' } else { 'i' });
                    out.push_str(shortcut.as_deref().unwrap_or(""));
                }
                MenuNode::Separator => out.push('-'),
                MenuNode::Submenu { label, children } => {
                    out.push('[');
                    out.push_str(label);
                    walk(children, out);
                    out.push(']');
                }
            }
            out.push(';');
        }
    }
    let mut s = format!("{};", dac_ui_egui::i18n::language().code());
    for (t, items) in bar {
        s.push_str(t);
        walk(items, &mut s);
    }
    s
}

/// Drop a trailing separator (left behind when an item moved to the app menu).
fn tidy_separators(mut nodes: Vec<MenuNode>) -> Vec<MenuNode> {
    while matches!(nodes.last(), Some(MenuNode::Separator)) {
        nodes.pop();
    }
    nodes
}

impl NativeMenu {
    /// Build the menu bar and install it as the application menu (main thread, after launch).
    pub fn install(app: &mut DacApp, ctx: &egui::Context) -> NativeMenu {
        let (tx, rx) = channel::<String>();
        let repaint = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = tx.send(e.id.0);
            repaint.request_repaint();
        }));
        let mut m = NativeMenu { menu: Menu::new(), items: HashMap::new(), rx, structure: String::new(), text_focus: false, capturing: false };
        m.rebuild(app);
        app.native_menu = true;
        m
    }

    fn rebuild(&mut self, app: &mut DacApp) {
        let bar = menu_bar(app);
        self.structure = structure_of(&bar);
        self.menu = Menu::new();
        self.items.clear();

        // the application menu
        let app_menu = Submenu::new(dac_brand::SHORT_NAME, true);
        let about = MenuItem::with_id("app.about", dac_brand::fill(&dac_ui_egui::i18n::tr("About {app}")), true, None);
        let settings = MenuItem::with_id(SETTINGS, dac_ui_egui::i18n::tr("Settings…"), true, accelerator(SETTINGS_KEY));
        let quit = MenuItem::with_id(QUIT, dac_brand::fill(&dac_ui_egui::i18n::tr("Quit {app}")), true, accelerator("Cmd+Q"));
        let _ = app_menu.append_items(&[
            &about,
            &PredefinedMenuItem::separator(),
            &settings,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(Some(dac_ui_egui::i18n::tr("Services"))),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some(dac_brand::fill(&dac_ui_egui::i18n::tr("Hide {app}")))),
            &PredefinedMenuItem::hide_others(Some(dac_ui_egui::i18n::tr("Hide Others"))),
            &PredefinedMenuItem::show_all(Some(dac_ui_egui::i18n::tr("Show All"))),
            &PredefinedMenuItem::separator(),
            &quit,
        ]);
        let _ = self.menu.append(&app_menu);

        for (title, nodes) in &bar {
            let sub = Submenu::new(label_text(title), true);
            if title == "Window" {
                let _ = sub.append_items(&[
                    &PredefinedMenuItem::minimize(Some(dac_ui_egui::i18n::tr("Minimize"))),
                    &PredefinedMenuItem::maximize(Some(dac_ui_egui::i18n::tr("Zoom"))),
                    &PredefinedMenuItem::separator(),
                ]);
            }
            // About and Settings live in the app menu on macOS
            let nodes: Vec<MenuNode> = nodes
                .iter()
                .filter(|n| !matches!(n, MenuNode::Item { id, .. } if id == "app.about" || id == SETTINGS || id == QUIT))
                .cloned()
                .collect();
            let nodes = tidy_separators(nodes);
            self.append_nodes(&sub, &nodes);
            if title == "File" {
                // ⌘W, the system's own item
                let _ = sub.append_items(&[
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::close_window(Some(dac_ui_egui::i18n::tr("Close Window"))),
                ]);
            }
            if title == "Window" {
                let _ = sub.append_items(&[
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::bring_all_to_front(Some(dac_ui_egui::i18n::tr("Bring All to Front"))),
                ]);
                sub.set_as_windows_menu_for_nsapp();
            }
            if title == "Help" {
                sub.set_as_help_menu_for_nsapp();
            }
            let _ = self.menu.append(&sub);
        }
        self.menu.init_for_nsapp();
        self.text_focus = false;
        self.capturing = false;
        self.publish_shortcuts(app);
    }

    fn append_nodes(&mut self, sub: &Submenu, nodes: &[MenuNode]) {
        for n in nodes {
            match n {
                MenuNode::Separator => {
                    let _ = sub.append(&PredefinedMenuItem::separator());
                }
                MenuNode::Submenu { label, children } => {
                    let s = Submenu::new(label_text(label), true);
                    self.append_nodes(&s, children);
                    let _ = sub.append(&s);
                }
                MenuNode::Item { id, params, label, shortcut, enabled, checked } => {
                    let key = MenuNode::key(id, params);
                    if self.items.contains_key(&key) {
                        continue; // muda ids must be unique
                    }
                    let accel = shortcut.as_deref().filter(|s| !CONTEXTUAL.contains(s)).and_then(accelerator);
                    let handle = match checked {
                        Some(c) => {
                            let it = CheckMenuItem::with_id(
                                key.clone(),
                                dac_ui_egui::menubar::display_item_label(id, params, label).replace('&', "&&"),
                                *enabled,
                                *c,
                                accel,
                            );
                            let _ = sub.append(&it);
                            Handle::Check(it)
                        }
                        None => {
                            let it = MenuItem::with_id(
                                key.clone(),
                                dac_ui_egui::menubar::display_item_label(id, params, label).replace('&', "&&"),
                                *enabled,
                                accel,
                            );
                            let _ = sub.append(&it);
                            Handle::Plain(it)
                        }
                    };
                    self.items.insert(
                        key,
                        Item {
                            handle,
                            cmd: id.clone(),
                            params: params.clone(),
                            shortcut: shortcut.clone(),
                            accel,
                            label: label.clone(),
                            enabled: *enabled,
                            checked: *checked,
                        },
                    );
                }
            }
        }
    }

    /// Tell the egui shortcut handler which shortcuts the menu bar currently owns.
    fn publish_shortcuts(&self, app: &mut DacApp) {
        let installed = self.items.values().filter(|i| i.accel.is_some()).filter_map(|i| i.shortcut.as_deref());
        // (none while the keymap editor records a shortcut: every key goes to it)
        app.native_shortcuts = if self.capturing { HashSet::new() } else { owned_by_menu(installed, self.text_focus) };
        app.native_shortcuts.insert(SETTINGS_KEY.to_string());
    }

    /// Per frame: run chosen items, then sync labels / enabled / checked and the text-focus
    /// accelerators with the app state. Returns whether Quit was chosen: the caller saves
    /// everything and exits the process itself (closing the viewport while AppKit terminates the
    /// app can crash in winit's teardown, PR #444).
    pub fn update(&mut self, app: &mut DacApp, ctx: &egui::Context) -> bool {
        dac_ui_egui::i18n::set_language(app.ui.language);
        let mut quit = false;
        while let Ok(key) = self.rx.try_recv() {
            if key == QUIT {
                quit = true;
                continue;
            }
            if key == "app.about" || key == SETTINGS {
                let _ = app.run(&key, serde_json::json!({}));
                continue;
            }
            if let Some(it) = self.items.get(&key) {
                let (cmd, params) = (it.cmd.clone(), it.params.clone());
                let _ = dac_ui_egui::menubar::run_item(app, &cmd, params);
            }
        }
        let bar = menu_bar(app);
        if structure_of(&bar) != self.structure {
            self.rebuild(app);
            return quit;
        }
        fn walk(nodes: &[MenuNode], items: &mut HashMap<String, Item>) {
            for n in nodes {
                match n {
                    MenuNode::Submenu { children, .. } => walk(children, items),
                    MenuNode::Item { id, params, label, enabled, checked, .. } => {
                        let Some(it) = items.get_mut(&MenuNode::key(id, params)) else { continue };
                        if it.label != *label {
                            match &it.handle {
                                Handle::Plain(h) => h.set_text(dac_ui_egui::menubar::display_item_label(id, params, label).replace('&', "&&")),
                                Handle::Check(h) => h.set_text(dac_ui_egui::menubar::display_item_label(id, params, label).replace('&', "&&")),
                            }
                            it.label = label.clone();
                        }
                        if it.enabled != *enabled {
                            match &it.handle {
                                Handle::Plain(h) => h.set_enabled(*enabled),
                                Handle::Check(h) => h.set_enabled(*enabled),
                            }
                            it.enabled = *enabled;
                        }
                        if let (Handle::Check(h), Some(c)) = (&it.handle, checked) {
                            // also resyncs after AppKit toggled it on click
                            if h.is_checked() != *c {
                                h.set_checked(*c);
                            }
                            it.checked = Some(*c);
                        }
                    }
                    MenuNode::Separator => {}
                }
            }
        }
        walk(&bar.iter().flat_map(|(_, v)| v.clone()).collect::<Vec<_>>(), &mut self.items);

        let focus = ctx.egui_wants_keyboard_input();
        let capturing = app.recording_shortcut.is_some();
        if focus != self.text_focus || capturing != self.capturing {
            self.text_focus = focus;
            self.capturing = capturing;
            for it in self.items.values() {
                let Some(sc) = it.shortcut.as_deref() else { continue };
                if it.accel.is_none() {
                    continue;
                }
                let accel = if capturing || (focus && yields_to_text(sc)) { None } else { it.accel };
                let _ = match &it.handle {
                    Handle::Plain(h) => h.set_accelerator(accel),
                    Handle::Check(h) => h.set_accelerator(accel),
                };
            }
            self.publish_shortcuts(app);
        }
        quit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The menu model's titles stay English, so only the language can tell the native menu to
    /// rebuild its titles (and the macOS app menu): every language switch — also between two CJK
    /// languages — must change the structure key.
    #[test]
    fn switching_language_rebuilds_native_menu_structure() {
        use dac_ui_egui::i18n::{Locale, set_language};
        let app = DacApp::new(dac_engine::Session::with_demo(), Default::default());
        set_language(Locale::En);
        let bar = menu_bar(&app);
        let mut seen = std::collections::BTreeSet::new();
        for language in Locale::ALL {
            set_language(*language);
            assert!(seen.insert(structure_of(&bar)), "{language:?} shares another language's menu structure key");
        }
        set_language(Locale::ZhHant);
        assert_eq!(label_text("File"), "檔案");
        assert_eq!(label_text("Settings…"), "設定…");
        assert_eq!(dac_brand::fill(&label_text("Quit {app}")), dac_brand::fill("結束 {app}"));
        set_language(Locale::En);
    }

    #[test]
    fn shortcuts_map_to_accelerators() {
        assert_eq!(accelerator("Cmd+Shift+Z"), Some(Accelerator::new(Modifiers::META | Modifiers::SHIFT, Code::KeyZ)));
        assert_eq!(accelerator("G"), Some(Accelerator::new(Modifiers::empty(), Code::KeyG)));
        assert_eq!(accelerator("Cmd+'"), Some(Accelerator::new(Modifiers::META, Code::Quote)));
        assert_eq!(accelerator("Escape"), None);
        assert!(yields_to_text("G") && yields_to_text("Delete") && yields_to_text("Cmd+V"));
        assert!(!yields_to_text("Cmd+Shift+E"));
        // every registry shortcut that a menu shows maps (or is deliberately left to egui)
        let app = DacApp::new(dac_engine::Session::with_demo(), Default::default());
        fn all(n: &[MenuNode], out: &mut Vec<String>) {
            for x in n {
                match x {
                    MenuNode::Item { shortcut: Some(s), .. } => out.push(s.clone()),
                    MenuNode::Submenu { children, .. } => all(children, out),
                    _ => {}
                }
            }
        }
        let mut scs = Vec::new();
        for (_, v) in menu_bar(&app) {
            all(&v, &mut scs);
        }
        for sc in scs.iter().filter(|s| !CONTEXTUAL.contains(&s.as_str())) {
            assert!(accelerator(sc).is_some(), "{sc}");
        }
    }

    /// Single keys shown in the menu bar (E, C, ⇧P, ratings…) never reach it on macOS, so egui must
    /// keep handling them; ⌘ / ⌃ combinations and function keys are the menu bar's own.
    #[test]
    fn single_key_shortcuts_stay_with_egui() {
        assert!(menu_delivers("Cmd+Shift+H") && menu_delivers("Ctrl+H") && menu_delivers("F2") && menu_delivers("Cmd+F11"));
        assert!(!menu_delivers("E") && !menu_delivers("Shift+P") && !menu_delivers("Alt+Y") && !menu_delivers("1") && !menu_delivers("F"));
        let app = DacApp::new(dac_engine::Session::with_demo(), Default::default());
        fn all(n: &[MenuNode], out: &mut Vec<String>) {
            for x in n {
                match x {
                    MenuNode::Item { shortcut: Some(s), .. } if accelerator(s).is_some() => out.push(s.clone()),
                    MenuNode::Submenu { children, .. } => all(children, out),
                    _ => {}
                }
            }
        }
        let mut installed = Vec::new();
        for (_, v) in menu_bar(&app) {
            all(&v, &mut installed);
        }
        for sc in ["E", "C", "Shift+P", "Cmd+Shift+H", "F2"] {
            assert!(installed.iter().any(|s| s == sc), "{sc} is in the menu bar");
        }
        let owned = owned_by_menu(installed.iter().map(String::as_str), false);
        for sc in ["E", "C", "Shift+P", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "P", "U"] {
            assert!(installed.iter().any(|s| s == sc), "{sc} is displayed in the menu");
            assert!(!owned.contains(sc), "{sc} must be left to egui");
        }
        for sc in ["Cmd+Shift+H", "F2"] {
            assert!(owned.contains(sc), "{sc} is run by the menu bar");
        }
        // while typing, the menu bar gives up F2 too (the text field has it)
        assert!(!owned_by_menu(installed.iter().map(String::as_str), true).contains("F2"));
    }
}
