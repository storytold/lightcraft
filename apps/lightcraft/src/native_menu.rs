//! The native macOS menu bar (via `muda`, pure Rust over AppKit), generated from the same model as
//! the in-window menus ([`lightcraft_ui_egui::menubar`]): the registry's menu paths, shortcuts,
//! enabled and checked state, and live labels ("Undo Exposure").
//!
//! Key handling: AppKit offers a key press to the menu bar only when it carries ⌘ or ⌃, or is a
//! function key (F1…). Every other key (`E`, `1`, `⇧P`, `⌥Y`…) goes straight to the window, whose
//! winit view always takes it, so a menu item never fires from it. Hence
//! - shortcuts the menu bar really receives ([`menu_delivers`]) are listed in
//!   `LightcraftApp::native_shortcuts` and skipped by the egui shortcut handler (nothing fires
//!   twice); the others stay on their menu items for display and egui runs them;
//! - while a text field has keyboard focus, accelerators without ⌘ (`G`, `1`, `Delete`…) and the
//!   text-editing ones (⌘A/⌘C/⌘V/⌘X/⌘Z) are removed, so typing and text editing work; they come
//!   back when the field loses focus.
//!
//! Menu events never pass through the egui shortcut handler. [`NativeMenu::update`] therefore runs
//! every queued command through [`lightcraft_ui_egui::menubar::run_native_command`]: while the
//! blocking library window or a quit/settings notice owns input it drops the command (Settings…
//! and About included), and only Quit stays available for the host's existing confirmation.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, channel};

use lightcraft_ui_egui::LightcraftApp;
use lightcraft_ui_egui::menubar::{
    MenuNode, menu_bar, native_item_presentation, native_menu_delivers as menu_delivers, native_menu_structure as structure_of,
};
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use serde_json::Value;

const QUIT: &str = "app.quit";
/// Settings… (the app menu on macOS; Edit elsewhere).
const SETTINGS: &str = "app.settings";
const SETTINGS_KEY: &str = "Cmd+,";
/// Keys whose meaning depends on the panel (X = reject or swap crop aspect, ⌫ = delete the photo
/// or the active mask; ⌘↑/⌘↓ flag status follows Compare/Survey's active photo): left to the egui
/// handler, which knows the context.
const CONTEXTUAL: &[&str] = &["X", "Delete", "Cmd+Up", "Cmd+Down"];
/// Shortcuts text fields need while they have focus.
const TEXT_EDIT: &[&str] = &["Cmd+A", "Cmd+C", "Cmd+V", "Cmd+X", "Cmd+Z", "Cmd+Shift+Z"];

enum Handle {
    Plain(MenuItem),
    Check(CheckMenuItem),
}

struct Item {
    handle: Handle,
    /// The submenu the item sits in, to swap the item when its accelerator has to go (macOS).
    parent: Submenu,
    cmd: String,
    params: Value,
    shortcut: Option<String>,
    /// The accelerator the item's shortcut maps to.
    accel: Option<Accelerator>,
    /// The accelerator the native item has now: `accel`, or `None` while it yields.
    current: Option<Accelerator>,
    label: String,
    enabled: bool,
    checked: Option<bool>,
    /// Whether the item may be replaced to drop its key. Not in the Help menu: a swap there left
    /// the old row and added a second one (muda 0.21, macOS 27), so those keys stay.
    swappable: bool,
}

impl Item {
    /// The row's text as installed: `label` is already what the row shows (translated, with a
    /// borrowed key's hint, `native_item_presentation`).
    fn native_label(&self) -> String {
        self.label.replace('&', "&&")
    }

    /// Puts `want` on the native item. muda's `set_accelerator(None)` does nothing on macOS (the
    /// old key equivalent stays and the menu still takes the key), so there the item is replaced
    /// by an identical one without an accelerator, at the same position and with the same id,
    /// text, enabled and checked state. Only done when the accelerator changes (and again while a
    /// native call keeps failing), never per frame otherwise.
    fn set_accelerator(&mut self, want: Option<Accelerator>) {
        if self.current == want {
            return;
        }
        if cfg!(target_os = "macos") && want.is_none() && self.swappable {
            // `current` follows the native item: it changes only when the swap went through, so a
            // failed swap (the old item, key and all, is still there) is tried again
            let done = self.replace_without_accelerator();
            self.current = settled(self.current, want, done);
            return;
        }
        let done = match &self.handle {
            Handle::Plain(h) => h.set_accelerator(want),
            Handle::Check(h) => h.set_accelerator(want),
        }
        .is_ok();
        self.current = settled(self.current, want, done);
    }

    /// Replaces the native item by one without an accelerator. `self.handle` changes only when the
    /// new item is attached; on failure the old item is still the one in the submenu.
    fn replace_without_accelerator(&mut self) -> bool {
        let (id, old): (_, &dyn muda::IsMenuItem) = match &self.handle {
            Handle::Plain(h) => (h.id().clone(), h),
            Handle::Check(h) => (h.id().clone(), h),
        };
        let Some(position) = self.parent.items().iter().position(|k| *k.id() == id) else { return false };
        let new = match &self.handle {
            Handle::Plain(_) => Handle::Plain(MenuItem::with_id(id, self.native_label(), self.enabled, None)),
            Handle::Check(h) => Handle::Check(CheckMenuItem::with_id(id, self.native_label(), self.enabled, h.is_checked(), None)),
        };
        let new_ref: &dyn muda::IsMenuItem = match &new {
            Handle::Plain(h) => h,
            Handle::Check(h) => h,
        };
        if !swap_item(&self.parent, old, new_ref, position) {
            return false;
        }
        self.handle = new;
        true
    }
}

/// The accelerator the native item has after trying to put `want` on it: `want` when the native
/// call worked, else what it had, so the caller sees the mismatch and tries again.
fn settled(had: Option<Accelerator>, want: Option<Accelerator>, native_call_worked: bool) -> Option<Accelerator> {
    if native_call_worked { want } else { had }
}

/// Whether the per-frame accelerator sync has to run: the focus or recording state changed, or an
/// earlier sync left an item whose native accelerator is not the wanted one (a failed swap).
fn sync_needed(focus: bool, capturing: bool, synced_focus: bool, synced_capturing: bool, pending: bool) -> bool {
    focus != synced_focus || capturing != synced_capturing || pending
}

/// The three submenu calls a swap needs, so the failure paths can be tested without a main-thread
/// `muda::Menu`.
trait SwapTarget {
    type Row: ?Sized;
    fn remove(&self, row: &Self::Row) -> bool;
    fn insert(&self, row: &Self::Row, position: usize) -> bool;
    fn append(&self, row: &Self::Row) -> bool;
}

impl SwapTarget for Submenu {
    type Row = dyn muda::IsMenuItem;
    fn remove(&self, row: &Self::Row) -> bool {
        Submenu::remove(self, row).is_ok()
    }
    fn insert(&self, row: &Self::Row, position: usize) -> bool {
        Submenu::insert(self, row, position).is_ok()
    }
    fn append(&self, row: &Self::Row) -> bool {
        Submenu::append(self, row).is_ok()
    }
}

/// Puts `new` where `old` is. Returns whether `new` is attached; when it is not, `old` is back in
/// the menu (at its position, else at the end), so the caller keeps `old` as its handle.
fn swap_item<T: SwapTarget + ?Sized>(target: &T, old: &T::Row, new: &T::Row, position: usize) -> bool {
    if !target.remove(old) {
        return false;
    }
    // (an item at the end beats a missing one)
    if target.insert(new, position) || target.append(new) {
        return true;
    }
    let _ = target.insert(old, position) || target.append(old);
    false
}

/// The accelerator a native item has: none while a shortcut is being recorded, and none for the
/// keys a focused text field needs.
fn wanted_accelerator(accel: Option<Accelerator>, sc: &str, text_focus: bool, capturing: bool) -> Option<Accelerator> {
    if capturing || (text_focus && yields_to_text(sc)) { None } else { accel }
}

pub struct NativeMenu {
    menu: Menu,
    items: HashMap<String, Item>,
    /// Settings… and Quit in the application submenu: ordinary items, so they give up ⌘, and ⌘Q
    /// to the shortcut recorder like the rest. (The system's own items there and in File/Window,
    /// such as Hide ⌘H, Close Window ⌘W and Minimize ⌘M, are muda predefined items whose key
    /// cannot be changed or swapped.)
    app_items: Vec<Item>,
    rx: Receiver<String>,
    /// Structure of the installed menu (ids, kinds, submenu names); a change rebuilds it.
    structure: String,
    text_focus: bool,
    /// The keyboard shortcuts editor is recording a key: every accelerator is off so the key
    /// press reaches it.
    capturing: bool,
    /// An item's native accelerator is not the wanted one (its swap or accelerator call failed): sync again next frame.
    accelerators_pending: bool,
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
        "`" => Code::Backquote,
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

/// The shortcuts the menu bar runs itself, for `LightcraftApp::native_shortcuts`.
fn owned_by_menu<'a>(installed: impl Iterator<Item = &'a str>, text_focus: bool) -> HashSet<String> {
    installed.filter(|sc| menu_delivers(sc) && !(text_focus && yields_to_text(sc))).map(str::to_string).collect()
}

/// `&` marks a mnemonic in muda labels.
fn label_text(s: &str) -> String {
    lightcraft_ui_egui::i18n::tr(s).replace('&', "&&")
}

/// Drop the separators left behind when an item moved to the app menu: a leading one, a trailing
/// one and the second of two in a row.
fn tidy_separators(nodes: Vec<MenuNode>) -> Vec<MenuNode> {
    let mut out: Vec<MenuNode> = Vec::with_capacity(nodes.len());
    for node in nodes {
        if node == MenuNode::Separator && out.last().is_none_or(|last| *last == MenuNode::Separator) {
            continue;
        }
        out.push(node);
    }
    while out.last() == Some(&MenuNode::Separator) {
        out.pop();
    }
    out
}

impl NativeMenu {
    /// Build the menu bar and install it as the application menu (main thread, after launch).
    pub fn install(app: &mut LightcraftApp, ctx: &egui::Context) -> NativeMenu {
        let (tx, rx) = channel::<String>();
        let repaint = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = tx.send(e.id.0);
            repaint.request_repaint();
        }));
        let mut m = NativeMenu {
            menu: Menu::new(),
            items: HashMap::new(),
            app_items: Vec::new(),
            rx,
            structure: String::new(),
            text_focus: false,
            capturing: false,
            accelerators_pending: false,
        };
        m.rebuild(app);
        app.native_menu = true;
        m
    }

    /// Recreate native handles and shortcut ownership from the current shared menu model.
    fn rebuild(&mut self, app: &mut LightcraftApp) {
        let bar = menu_bar(app);
        self.structure = structure_of(&bar, &app.ui.settings.keymap);
        self.menu = Menu::new();
        self.items.clear();
        self.app_items.clear();

        // the application menu
        let app_menu = Submenu::new("LightCraft", true);
        let about = MenuItem::with_id("app.about", lightcraft_ui_egui::i18n::tr("About LightCraft"), true, None);
        let settings = MenuItem::with_id(SETTINGS, lightcraft_ui_egui::i18n::tr("Settings…"), true, accelerator(SETTINGS_KEY));
        let quit = MenuItem::with_id(QUIT, lightcraft_ui_egui::i18n::tr("Quit LightCraft"), true, accelerator("Cmd+Q"));
        let _ = app_menu.append_items(&[
            &about,
            &PredefinedMenuItem::separator(),
            &settings,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(Some(lightcraft_ui_egui::i18n::tr("Services"))),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some(lightcraft_ui_egui::i18n::tr("Hide LightCraft"))),
            &PredefinedMenuItem::hide_others(Some(lightcraft_ui_egui::i18n::tr("Hide Others"))),
            &PredefinedMenuItem::show_all(Some(lightcraft_ui_egui::i18n::tr("Show All"))),
            &PredefinedMenuItem::separator(),
            &quit,
        ]);
        for (handle, cmd, shortcut, text) in [(&settings, SETTINGS, SETTINGS_KEY, "Settings…"), (&quit, QUIT, "Cmd+Q", "Quit LightCraft")] {
            let accel = accelerator(shortcut);
            self.app_items.push(Item {
                handle: Handle::Plain(handle.clone()),
                parent: app_menu.clone(),
                swappable: true,
                cmd: cmd.to_string(),
                params: Value::Null,
                shortcut: Some(shortcut.to_string()),
                accel,
                current: accel,
                label: lightcraft_ui_egui::menubar::display_item_label(cmd, &Value::Null, text).to_string(),
                enabled: true,
                checked: None,
            });
        }
        let _ = self.menu.append(&app_menu);

        for (title, nodes) in &bar {
            let sub = Submenu::new(label_text(title), true);
            if title == "Window" {
                let _ = sub.append_items(&[
                    &PredefinedMenuItem::minimize(Some(lightcraft_ui_egui::i18n::tr("Minimize"))),
                    &PredefinedMenuItem::maximize(Some(lightcraft_ui_egui::i18n::tr("Zoom"))),
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
            // Swapping an item in the Help menu leaves the old row in place and adds a second one
            // (measured on macOS 27 with muda 0.21): the Help menu is not addressed row for row.
            self.append_nodes(&sub, &nodes, &app.ui.settings.keymap, title != "Help");
            if title == "File" {
                // ⌘W, the system's own item
                let _ = sub.append_items(&[
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::close_window(Some(lightcraft_ui_egui::i18n::tr("Close Window"))),
                ]);
            }
            if title == "Window" {
                let _ = sub.append_items(&[
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::bring_all_to_front(Some(lightcraft_ui_egui::i18n::tr("Bring All to Front"))),
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
        self.accelerators_pending = false;
        self.publish_shortcuts(app);
    }

    /// Install translated rows with their effective accelerators, preserving hint-only keys.
    /// `swappable`: whether its items may be replaced to drop their key (see `Item::swappable`).
    fn append_nodes(&mut self, sub: &Submenu, nodes: &[MenuNode], keymap: &lightcraft_ui_egui::shortcuts::Keymap, swappable: bool) {
        for n in nodes {
            match n {
                MenuNode::Separator => {
                    let _ = sub.append(&PredefinedMenuItem::separator());
                }
                MenuNode::Submenu { label, children } => {
                    let s = Submenu::new(label_text(label), true);
                    self.append_nodes(&s, children, keymap, swappable);
                    let _ = sub.append(&s);
                }
                MenuNode::Item { id, params, label, shortcut, enabled, checked } => {
                    let key = MenuNode::key(id, params);
                    if self.items.contains_key(&key) {
                        continue; // muda ids must be unique
                    }
                    let (native_label, native_key) = native_item_presentation(keymap, id, params, label, shortcut.as_deref());
                    let accel = native_key.filter(|s| !CONTEXTUAL.contains(s)).and_then(accelerator);
                    let handle = match checked {
                        Some(c) => {
                            let it = CheckMenuItem::with_id(key.clone(), native_label.replace('&', "&&"), *enabled, *c, accel);
                            let _ = sub.append(&it);
                            Handle::Check(it)
                        }
                        None => {
                            let it = MenuItem::with_id(key.clone(), native_label.replace('&', "&&"), *enabled, accel);
                            let _ = sub.append(&it);
                            Handle::Plain(it)
                        }
                    };
                    self.items.insert(
                        key,
                        Item {
                            handle,
                            parent: sub.clone(),
                            swappable,
                            cmd: id.clone(),
                            params: params.clone(),
                            shortcut: shortcut.clone(),
                            accel,
                            current: accel,
                            label: native_label,
                            enabled: *enabled,
                            checked: *checked,
                        },
                    );
                }
            }
        }
    }

    /// Tell the egui shortcut handler which shortcuts the menu bar currently owns.
    fn publish_shortcuts(&self, app: &mut LightcraftApp) {
        let installed = self.items.values().filter(|i| i.accel.is_some()).filter_map(|i| i.shortcut.as_deref());
        // (none while the keymap editor records a shortcut: every key goes to it)
        app.native_shortcuts = if self.capturing { HashSet::new() } else { owned_by_menu(installed, self.text_focus) };
        app.native_shortcuts.insert(SETTINGS_KEY.to_string());
    }

    /// Per frame: run chosen items, then sync labels / enabled / checked and the text-focus
    /// accelerators with the app state. Returns whether Quit was chosen: the caller saves
    /// everything and exits the process itself (closing the viewport while AppKit terminates the
    /// app can crash in winit's teardown, PR #444).
    pub fn update(&mut self, app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
        lightcraft_ui_egui::i18n::set_language(app.ui.language);
        let mut quit = false;
        while let Ok(key) = self.rx.try_recv() {
            // Resolve the native id to its command, then let the shared policy decide. Native menu
            // events never reach `shortcuts::handle`, so this is the one guard for AppKit's
            // accelerators and clicks: while the blocking library window or a quit/settings notice
            // owns input, a queued command is dropped (Quit stays for the existing confirmation).
            let (cmd, params) = if key == QUIT || key == "app.about" || key == SETTINGS {
                (key, serde_json::json!({}))
            } else if let Some(it) = self.items.get(&key) {
                (it.cmd.clone(), it.params.clone())
            } else {
                continue;
            };
            if lightcraft_ui_egui::menubar::run_native_command(app, &cmd, params) == lightcraft_ui_egui::menubar::NativeCommand::Quit {
                quit = true;
            }
        }
        let bar = menu_bar(app);
        if structure_of(&bar, &app.ui.settings.keymap) != self.structure {
            // (no early return: a rebuild installs every accelerator and resets the state they were synced
            // to, so the sync below runs in this same frame and no key reaches the menu in between)
            self.rebuild(app);
        }
        /// Synchronize live labels and states; accelerator ownership changes already triggered a rebuild.
        fn walk(nodes: &[MenuNode], items: &mut HashMap<String, Item>, keymap: &lightcraft_ui_egui::shortcuts::Keymap) {
            for n in nodes {
                match n {
                    MenuNode::Submenu { children, .. } => walk(children, items, keymap),
                    MenuNode::Item { id, params, label, shortcut, enabled, checked } => {
                        let Some(it) = items.get_mut(&MenuNode::key(id, params)) else { continue };
                        let (native_label, _) = native_item_presentation(keymap, id, params, label, shortcut.as_deref());
                        if it.label != native_label {
                            match &it.handle {
                                Handle::Plain(h) => h.set_text(native_label.replace('&', "&&")),
                                Handle::Check(h) => h.set_text(native_label.replace('&', "&&")),
                            }
                            it.label = native_label;
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
        walk(&bar.iter().flat_map(|(_, v)| v.clone()).collect::<Vec<_>>(), &mut self.items, &app.ui.settings.keymap);

        let focus = ctx.egui_wants_keyboard_input();
        let capturing = app.recording_shortcut.is_some();
        if sync_needed(focus, capturing, self.text_focus, self.capturing, self.accelerators_pending) {
            self.text_focus = focus;
            self.capturing = capturing;
            let mut pending = false;
            for it in self.items.values_mut().chain(self.app_items.iter_mut()) {
                let Some(sc) = it.shortcut.as_deref() else { continue };
                if it.accel.is_none() {
                    continue;
                }
                let want = wanted_accelerator(it.accel, sc, focus, capturing);
                it.set_accelerator(want);
                pending |= it.current != want;
            }
            self.accelerators_pending = pending;
            self.publish_shortcuts(app);
        }
        quit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An item that moves to the app menu (Settings…) can sit between two separators, or open or
    /// close the menu's list: the rows left behind keep at most one separator between them.
    #[test]
    fn rows_left_after_a_move_keep_single_separators() {
        let item = |id: &str| MenuNode::Item {
            id: id.to_string(),
            label: id.to_string(),
            shortcut: None,
            enabled: true,
            checked: None,
            params: Value::Null,
        };
        let sep = || MenuNode::Separator;
        let tidy = tidy_separators(vec![sep(), item("a"), sep(), sep(), item("b"), sep(), sep()]);
        assert_eq!(tidy, vec![item("a"), sep(), item("b")]);
        assert_eq!(tidy_separators(vec![sep(), sep()]), Vec::<MenuNode>::new());
    }

    /// The menu model's titles stay English, so only the language can tell the native menu to
    /// rebuild its titles (and the macOS app menu): every language switch — also between two CJK
    /// languages — must change the structure key.
    #[test]
    fn switching_language_rebuilds_native_menu_structure() {
        use lightcraft_ui_egui::i18n::{Locale, set_language};
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        set_language(Locale::En);
        let bar = menu_bar(&app);
        let mut seen = std::collections::BTreeSet::new();
        for language in Locale::ALL {
            set_language(*language);
            assert!(seen.insert(structure_of(&bar, &app.ui.settings.keymap)), "{language:?} shares another language's menu structure key");
        }
        set_language(Locale::En);
    }

    #[test]
    fn accelerators_yield_to_text_focus_and_shortcut_recording() {
        let g = accelerator("G");
        let cmd_s = accelerator("Cmd+S");
        assert_eq!(wanted_accelerator(g, "G", false, false), g);
        assert_eq!(wanted_accelerator(g, "G", true, false), None, "typing a G in a field");
        assert_eq!(wanted_accelerator(cmd_s, "Cmd+S", true, false), cmd_s, "⌘S still saves while typing");
        assert_eq!(wanted_accelerator(cmd_s, "Cmd+S", false, true), None, "the editor records the key");
        assert_eq!(wanted_accelerator(cmd_s, "Cmd+S", true, true), None, "recording wins over typing");
        // the application menu's Settings… and Quit are tracked like the other items
        for sc in [SETTINGS_KEY, "Cmd+Q"] {
            let a = accelerator(sc);
            assert!(a.is_some());
            assert_eq!(wanted_accelerator(a, sc, true, false), a, "{sc} stays while typing");
            assert_eq!(wanted_accelerator(a, sc, false, true), None, "{sc} goes while recording");
        }
        assert_eq!(wanted_accelerator(None, "Escape", false, false), None, "no accelerator stays none");
    }

    /// A submenu of numbers whose calls can be made to fail, to run `swap_item`'s failure paths.
    /// Row 2 is the old item and row 9 its replacement.
    #[derive(Default)]
    struct Fake {
        rows: std::cell::RefCell<Vec<u32>>,
        fail_remove: bool,
        /// The new item (9) cannot be inserted / appended.
        reject_new_insert: bool,
        reject_new_append: bool,
        /// The old item (2) cannot be inserted back.
        reject_old_insert: bool,
    }
    impl SwapTarget for Fake {
        type Row = u32;
        fn remove(&self, row: &u32) -> bool {
            if self.fail_remove {
                return false;
            }
            self.rows.borrow_mut().retain(|x| x != row);
            true
        }
        fn insert(&self, row: &u32, position: usize) -> bool {
            if (self.reject_new_insert && *row == 9) || (self.reject_old_insert && *row == 2) {
                return false;
            }
            self.rows.borrow_mut().insert(position, *row);
            true
        }
        fn append(&self, row: &u32) -> bool {
            if self.reject_new_append && *row == 9 {
                return false;
            }
            self.rows.borrow_mut().push(*row);
            true
        }
    }

    fn swap(fake: Fake) -> (bool, Vec<u32>) {
        *fake.rows.borrow_mut() = vec![1, 2, 3];
        let done = swap_item(&fake, &2, &9, 1);
        (done, fake.rows.into_inner())
    }

    #[test]
    fn a_swap_changes_the_item_only_when_the_new_one_is_attached() {
        assert_eq!(swap(Fake::default()), (true, vec![1, 9, 3]), "in place");
        assert_eq!(swap(Fake { fail_remove: true, ..Default::default() }), (false, vec![1, 2, 3]), "remove fails: the old item stays");
        assert_eq!(swap(Fake { reject_new_insert: true, ..Default::default() }), (true, vec![1, 3, 9]), "insert fails: the new item goes last");
        assert_eq!(
            swap(Fake { reject_new_insert: true, reject_new_append: true, ..Default::default() }),
            (false, vec![1, 2, 3]),
            "neither works: the old item is back where it was"
        );
        assert_eq!(
            swap(Fake { reject_new_insert: true, reject_new_append: true, reject_old_insert: true, ..Default::default() }),
            (false, vec![1, 3, 2]),
            "the old item cannot go back in place either: it is appended"
        );
    }

    #[test]
    fn a_failed_native_call_is_retried_while_the_state_holds() {
        let g = accelerator("G");
        assert_eq!(settled(g, None, true), None, "the swap worked");
        assert_eq!(settled(g, None, false), g, "it failed: the native item still has its key");
        assert_eq!(settled(None, g, false), None, "restoring a key failed");
        assert!(!sync_needed(true, false, true, false, false), "nothing changed");
        assert!(sync_needed(true, false, false, false, false), "focus changed");
        assert!(sync_needed(false, true, false, false, false), "recording changed");
        assert!(sync_needed(true, false, true, false, true), "a swap failed last time: try again with the state unchanged");
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
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
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
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
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
