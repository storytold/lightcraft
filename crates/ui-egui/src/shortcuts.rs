//! Keyboard shortcuts: parse `Cmd+Shift+X` style strings, apply the user's keymap (Help ▸
//! Keyboard Shortcuts, `app.setShortcut`) and dispatch UI and engine commands.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use egui::{Key, Modifiers};
use serde_json::{Value, json};

use crate::LightcraftApp;

/// The user's changes to the keymap: command id → shortcut (`""` = no shortcut). Saved with the
/// app settings (`ui.json`); commands not listed keep their declared shortcut.
pub type Keymap = BTreeMap<String, String>;

/// Shortcuts the app menu owns (Settings…, Quit), which no command can take.
pub const RESERVED: &[&str] = &["Cmd+,", "Cmd+Q"];

/// A command that can have a shortcut.
#[derive(Clone, Copy, Debug)]
pub struct Bindable {
    pub id: &'static str,
    pub label: &'static str,
    /// The declared shortcut. An engine command whose key a UI command wraps (e.g. `W` opens the
    /// White Balance Selector rather than sampling without a point) has none: the UI one owns it.
    pub default: Option<&'static str>,
}

/// Every command that can have a shortcut: UI commands, then engine commands (one entry per id).
pub fn bindable() -> &'static [Bindable] {
    static ALL: OnceLock<Vec<Bindable>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<Bindable> = Vec::new();
        for (id, label, sc, _) in crate::menus::ui_commands() {
            if !v.iter().any(|b| b.id == *id) {
                v.push(Bindable { id, label, default: *sc });
            }
        }
        let ui_keys: Vec<(Modifiers, Key)> = v.iter().filter_map(|b| b.default.and_then(parse)).collect();
        for c in lightcraft_engine::command_specs() {
            if v.iter().any(|b| b.id == c.id) {
                continue;
            }
            let default = c.shortcut.filter(|sc| parse(sc).is_some_and(|k| !ui_keys.contains(&k)));
            v.push(Bindable { id: c.id, label: c.label, default });
        }
        v
    })
}

pub fn find_bindable(id: &str) -> Option<&'static Bindable> {
    bindable().iter().find(|b| b.id == id)
}

/// The shortcut `id` has now: the user's choice (`""` = none), else the declared one. An entry
/// that doesn't parse (a hand-edited `ui.json`, a modifier-only key saved by an older build) is
/// ignored. The declared shortcut yields to a saved entry that gives its key to another known
/// command other than its contextual partner.
pub fn binding<'a>(keymap: &'a Keymap, id: &str, default: Option<&'a str>) -> Option<&'a str> {
    match keymap.get(id) {
        Some(sc) if sc.is_empty() => None,
        Some(sc) if parse(sc).is_some() => Some(sc.as_str()),
        _ => default.filter(|sc| !assigned_elsewhere(keymap, id, sc)),
    }
}

/// Whether the keymap gives the key `sc` to a known command other than `id` and its contextual
/// partner.
fn assigned_elsewhere(keymap: &Keymap, id: &str, sc: &str) -> bool {
    let Some(key) = parse(sc) else { return false };
    saved_keys(keymap)
        .any(|(other, k)| other != id && k == key && !CONTEXTUAL.iter().any(|(x, y)| (*x == id && *y == other) || (*y == id && *x == other)))
}

/// The keys the keymap gives to known commands, with each command's id. Entries for commands this
/// build doesn't know and entries that don't parse are ignored.
pub(crate) fn saved_keys(keymap: &Keymap) -> impl Iterator<Item = (&str, (Modifiers, Key))> {
    keymap.iter().filter(|(id, _)| find_bindable(id).is_some()).filter_map(|(id, sc)| Some((id.as_str(), parse(sc)?)))
}

/// The effective shortcut of command `id` (`None` for unknown commands).
pub fn shortcut_of<'a>(keymap: &'a Keymap, id: &str) -> Option<&'a str> {
    find_bindable(id).and_then(|b| binding(keymap, id, b.default))
}

/// Engine commands that intentionally share a key and are disambiguated by context in [`handle`]:
/// the X pair by the Crop panel, A by whether it is open (Visualize Spots everywhere, the aspect
/// lock under the Crop panel).
pub const CONTEXTUAL: &[(&str, &str)] = &[("photo.reject", "crop.rotateAspect"), ("view.visualizeSpots", "crop.lockAspect")];

/// Commands other than `id` whose shortcut is the key `sc` (contextual partners excepted).
pub fn conflicts(keymap: &Keymap, id: &str, sc: &str) -> Vec<&'static str> {
    let Some(key) = parse(sc) else { return vec![] };
    bindable()
        .iter()
        .filter(|b| b.id != id && !CONTEXTUAL.iter().any(|(x, y)| (*x == id && *y == b.id) || (*y == id && *x == b.id)))
        .filter(|b| binding(keymap, b.id, b.default).and_then(parse) == Some(key))
        .map(|b| b.id)
        .collect()
}

/// Give `id` the shortcut `sc` (`None` = no shortcut), taking it away from commands that had the
/// same key. Returns the commands that lost it.
pub fn assign(keymap: &mut Keymap, id: &str, sc: Option<&str>) -> Result<Vec<&'static str>, String> {
    let b = find_bindable(id).ok_or_else(|| format!("unknown command: {id}"))?;
    let sc = match sc.map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => {
            let key = parse(s).ok_or_else(|| format!("not a shortcut: {s}"))?;
            // store the canonical spelling so equal keys compare equal (menus, native accelerators)
            let canonical = format(key.0, key.1).unwrap_or_else(|| s.to_string());
            if RESERVED.iter().any(|r| parse(r) == Some(key)) {
                return Err(format!("{canonical} is reserved for the app menu"));
            }
            Some(canonical)
        }
    };
    let lost = sc.as_deref().map(|s| conflicts(keymap, id, s)).unwrap_or_default();
    for other in &lost {
        set(keymap, other, None);
    }
    set(keymap, b.id, sc.as_deref());
    Ok(lost)
}

/// Store `sc` for `id`, dropping the entry when it equals the declared shortcut.
fn set(keymap: &mut Keymap, id: &str, sc: Option<&str>) {
    let default = find_bindable(id).and_then(|b| b.default);
    let same = match (sc, default) {
        (None, None) => true,
        (Some(a), Some(b)) => parse(a) == parse(b),
        _ => false,
    };
    if same {
        keymap.remove(id);
    } else {
        keymap.insert(id.to_string(), sc.unwrap_or_default().to_string());
    }
}

/// Restore the declared shortcut of `id`, taking it away from a command the user gave it to.
pub fn reset(keymap: &mut Keymap, id: &str) -> Result<Vec<&'static str>, String> {
    let b = find_bindable(id).ok_or_else(|| format!("unknown command: {id}"))?;
    assign(keymap, id, b.default)
}

/// `app.setShortcut {id, shortcut?, reset?}`: `shortcut` null or `""` removes it.
pub fn set_shortcut(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let id = p.get("id").and_then(Value::as_str).ok_or("missing id")?;
    let keymap = &mut app.ui.settings.keymap;
    let lost = if p.get("reset").and_then(Value::as_bool).unwrap_or(false) {
        reset(keymap, id)?
    } else {
        match p.get("shortcut") {
            None => return Err("missing shortcut (null removes it)".into()),
            Some(Value::Null) => assign(keymap, id, None)?,
            Some(Value::String(s)) => assign(keymap, id, Some(s))?,
            Some(_) => return Err("shortcut must be a string or null".into()),
        }
    };
    Ok(json!({"id": id, "shortcut": shortcut_of(&app.ui.settings.keymap, id), "removedFrom": lost}))
}

/// ⌘ / ⇧ / ⌥ / ⌃ themselves: they modify a shortcut's key, they can't be it.
pub fn is_modifier(k: Key) -> bool {
    matches!(
        k,
        Key::ShiftLeft | Key::ShiftRight | Key::ControlLeft | Key::ControlRight | Key::AltLeft | Key::AltRight | Key::SuperLeft | Key::SuperRight
    )
}

/// The shortcut text for a key press (`Cmd+Shift+K`), or `None` for keys a shortcut can't name.
/// `Cmd` is ⌘ on macOS and Ctrl elsewhere; `Ctrl` is the macOS Control key.
pub fn format(m: Modifiers, k: Key) -> Option<String> {
    if is_modifier(k) {
        return None;
    }
    let key = match k {
        Key::Backspace | Key::Delete => "Delete",
        Key::Slash => "/",
        Key::Backslash => "\\",
        Key::Equals => "=",
        Key::Minus => "-",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Quote => "'",
        Key::Comma => ",",
        Key::Backtick => "`",
        k => k.name(),
    };
    let mut s = String::new();
    if m.command || m.mac_cmd {
        s.push_str("Cmd+");
    }
    // off macOS Ctrl *is* Cmd; on macOS it is the separate Control key
    if m.ctrl && (m.mac_cmd || !m.command) {
        s.push_str("Ctrl+");
    }
    if m.alt {
        s.push_str("Alt+");
    }
    if m.shift {
        s.push_str("Shift+");
    }
    s.push_str(key);
    let back = parse(&s)?;
    let want = if k == Key::Delete { Key::Backspace } else { k };
    (back.1 == want).then_some(s)
}

/// Secondary key bindings for commands that already exist: `(shortcut, command id, params JSON)`.
/// They complement the primary shortcut declared on the command (Lightroom-desktop keys that our
/// primary keymap assigns elsewhere, see docs/ui-parity.md → Shortcuts). Shown in Help → Keyboard Shortcuts.
pub const ALIASES: &[(&str, &str, &str)] = &[
    ("Cmd+D", "library.selectNone", "{}"),
    ("Shift+E", "dialog.export", "{}"),
    ("Cmd+E", "app.exportPrevious", "{}"),
    ("Space", "view.zoomToggle", "{}"),
    // Lightroom Classic: F6 toggles the filmstrip, F7 the left library panel.
    // Keep existing primary shortcuts and honour custom keymap assignments.
    ("F6", "view.filmstrip", "{}"),
    ("F7", "view.leftPanel", "{}"),
    // and F8 the right panel
    ("F8", "view.rightPanel", "{}"),
    ("Shift+M", "version.create", "{}"),
    // grid thumbnail size: Classic's key is `=`; `+` (keypad, or Shift+= reported as Plus) agrees
    ("Plus", "view.thumbSizeUp", "{}"),
    ("Shift+Plus", "view.thumbSizeUp", "{}"),
    ("Shift+X", "photo.flag", r#"{"flag": "reject", "advance": true}"#),
    ("Shift+Z", "photo.flag", r#"{"flag": "pick", "advance": true}"#),
    ("Shift+U", "photo.flag", r#"{"flag": "none", "advance": true}"#),
    ("Shift+6", "photo.label", r#"{"label": "red", "advance": true}"#),
    ("Shift+7", "photo.label", r#"{"label": "yellow", "advance": true}"#),
    ("Shift+8", "photo.label", r#"{"label": "green", "advance": true}"#),
    ("Shift+9", "photo.label", r#"{"label": "blue", "advance": true}"#),
    // Shift+[ / Shift+] arrive as { / } on most layouts
    ("Shift+{", "brush.featherLess", "{}"),
    ("Shift+}", "brush.featherMore", "{}"),
    // keyword set: ⌥1–⌥9 toggle the current set's keywords on the selection
    ("Alt+1", "keyword.toggleFromSet", r#"{"index": 1}"#),
    ("Alt+2", "keyword.toggleFromSet", r#"{"index": 2}"#),
    ("Alt+3", "keyword.toggleFromSet", r#"{"index": 3}"#),
    ("Alt+4", "keyword.toggleFromSet", r#"{"index": 4}"#),
    ("Alt+5", "keyword.toggleFromSet", r#"{"index": 5}"#),
    ("Alt+6", "keyword.toggleFromSet", r#"{"index": 6}"#),
    ("Alt+7", "keyword.toggleFromSet", r#"{"index": 7}"#),
    ("Alt+8", "keyword.toggleFromSet", r#"{"index": 8}"#),
    ("Alt+9", "keyword.toggleFromSet", r#"{"index": 9}"#),
];

pub fn parse(s: &str) -> Option<(Modifiers, Key)> {
    let mut m = Modifiers::NONE;
    let mut key = None;
    for part in s.split('+') {
        match part {
            "Cmd" => m.command = true,
            "Shift" => m.shift = true,
            "Alt" => m.alt = true,
            "Ctrl" => m.ctrl = true,
            // "Delete" means the key labelled ⌫ (egui's Backspace); forward-delete also matches, see `matches`.
            "Delete" => key = Some(Key::Backspace),
            k => {
                // an unknown part (`Hyper`, a typo) makes the whole shortcut invalid
                key = Some(
                    Key::from_name(k)
                        .or(match k {
                            "Right" => Some(Key::ArrowRight),
                            "Left" => Some(Key::ArrowLeft),
                            "Up" => Some(Key::ArrowUp),
                            "Down" => Some(Key::ArrowDown),
                            "\\" => Some(Key::Backslash),
                            "/" => Some(Key::Slash),
                            "=" => Some(Key::Equals),
                            "-" => Some(Key::Minus),
                            "[" => Some(Key::OpenBracket),
                            "]" => Some(Key::CloseBracket),
                            "'" => Some(Key::Quote),
                            "," => Some(Key::Comma),
                            _ => None,
                        })
                        .filter(|k| !is_modifier(*k))?,
                )
            }
        }
    }
    key.map(|k| (m, k))
}

fn matches(i: &egui::InputState, m: Modifiers, k: Key) -> bool {
    // the windowing layer turns ⌘C / ⌘X / ⌘V (with any other modifiers held) and the keyboard's
    // Copy / Cut / Paste keys into these on Windows and Linux (macOS's menu bar takes them first);
    // without ⌘/Ctrl held they came from a key that is the plain command (Windows' ⇧Insert pastes)
    let clipboard = if i.modifiers.command { i.modifiers } else { Modifiers::COMMAND };
    i.events.iter().any(|e| {
        let (key, physical_key, modifiers) = match e {
            egui::Event::Key { key, physical_key, pressed: true, modifiers, .. } => (key, physical_key, modifiers),
            egui::Event::Copy => (&Key::C, &None, &clipboard),
            egui::Event::Cut => (&Key::X, &None, &clipboard),
            egui::Event::Paste(_) => (&Key::V, &None, &clipboard),
            _ => return false,
        };
        // `Ctrl` is the physical Control key (on macOS distinct from Cmd; elsewhere Cmd = Ctrl);
        // "Delete" (⌫ = Backspace) also matches forward-delete
        let ctrl_ok = if m.ctrl { modifiers.ctrl } else { !modifiers.ctrl || modifiers.command };
        let cmd_ok = m.ctrl || modifiers.command == m.command;
        // winit can report shifted digits as punctuation (Shift+1 = `!`). Recognize the
        // physical number key for culling, without changing layout-aware letter shortcuts.
        let shifted_digit = m.shift
            && *physical_key == Some(k)
            && matches!(k, Key::Num0 | Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6 | Key::Num7 | Key::Num8 | Key::Num9);
        (*key == k || shifted_digit || (k == Key::Backspace && *key == Key::Delete))
            && ctrl_ok
            && cmd_ok
            && modifiers.shift == m.shift
            && modifiers.alt == m.alt
    })
}

/// Default brackets rate in grids, and the default backslash shows / hides the Filter Bar there
/// (Lightroom Classic's Library keys; Detail keeps brush size and Show Original). The full-screen
/// preview keeps the grid's view but shows the photo alone, so backslash stays Show Original there.
/// Remapped brush and Show Original keys retain their action, and a saved assignment of the key to
/// another command takes precedence over this default.
fn grid_key_command(keymap: &Keymap, grid: bool, fullscreen: bool, id: &'static str, shortcut: (Modifiers, Key)) -> Option<&'static str> {
    let command = match (grid, id, shortcut) {
        (true, "brush.smaller", (m, Key::OpenBracket)) if m == Modifiers::NONE => "photo.decreaseRating",
        (true, "brush.larger", (m, Key::CloseBracket)) if m == Modifiers::NONE => "photo.increaseRating",
        (true, "view.showOriginal", (m, Key::Backslash)) if m == Modifiers::NONE && !fullscreen => "view.filterBar",
        _ => return Some(id),
    };
    if keymap.iter().any(|(other, sc)| other != id && find_bindable(other).is_some() && parse(sc) == Some(shortcut)) { None } else { Some(command) }
}

/// Content widgets keep keyboard ownership, including text fields and arrow-driven sliders.
/// Dock chrome owns activation, traversal, resizing and cancellation; its other keys reach commands.
fn keyboard_owned(app: &LightcraftApp, ctx: &egui::Context) -> bool {
    if !ctx.egui_wants_keyboard_input() {
        // Escape drops egui focus before shortcuts run; keep the numeric edit's whole input batch.
        return ctx.input(|input| input.key_pressed(Key::Escape))
            && app.widgets.iter().any(|(widget, _)| widget.starts_with("sliderField:") && crate::text_field::was_editing(ctx, widget));
    }
    // a selected slider keeps its arrow keys; everything else is a shortcut
    if crate::widgets::slider_has_focus(ctx) {
        return ctx.input(|input| [Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown].iter().any(|key| input.key_pressed(*key)));
    }
    // a handle on the photo keeps the arrows (it moves) and Return / Space (it acts)
    if crate::canvas_keys::has_focus(ctx) {
        return ctx.input(|input| {
            [Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown, Key::Enter, Key::Space].iter().any(|key| input.key_pressed(*key))
        });
    }
    let chrome =
        app.ui.docking.as_ref().is_some_and(|workspace| {
            craft_ui::docking::DockArea::new(egui::Id::unique("lightcraft-docking")).chrome_has_focus(ctx, &workspace.layout)
        });
    if !chrome {
        return true;
    }
    ctx.input(|input| {
        [Key::Enter, Key::Space, Key::Tab, Key::Escape, Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown]
            .iter()
            .any(|key| input.key_pressed(*key))
    })
}

/// Dispatch the global shortcut pass. It yields while a dialog, prompt, blocking library window,
/// text field, name edit or shortcut recording owns input, then fires saved command bindings,
/// aliases and fixed photo keys, respecting native menu ownership and open popups.
pub fn handle(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !matches!(app.ui.dialog, Some(crate::state::Dialog::Shortcuts)) {
        app.recording_shortcut = None;
    }
    // Dialogs own their keys (including Esc and text editing); background photo actions wait. A
    // quit prompt or settings notice owns every key too: its Foreground dim only blocks the
    // pointer, so without this a rating or navigation shortcut would still act behind it. The
    // blocking library window (error or launch list) owns all app shortcuts for every settled
    // frame, not only its shield pass; its own list keys are consumed earlier by
    // `library_problem::logic`, and its buttons remain reachable through the control channel.
    if app.ui.dialog.is_some() || crate::panels::notices::blocks_input(app) || crate::panels::library_problem::blocks_dialog_input(app) {
        return;
    }
    if app.recording_shortcut.is_none() && app.ui.name_edit.is_none() && crate::panels::person::handle_bulk_key(app, ctx) {
        return;
    }
    // don't steal keys from text fields or from the keymap editor recording a shortcut
    if keyboard_owned(app, ctx) || app.recording_shortcut.is_some() || app.ui.name_edit.is_some() {
        return;
    }
    let mut fire: Vec<String> = Vec::new();
    // shortcuts the native menu bar handles (it consumes those key presses itself)
    let native = |sc: &str| app.native_shortcuts.contains(sc);
    let mut aliased: Vec<(&str, serde_json::Value)> = Vec::new();
    let keymap = &app.ui.settings.keymap;
    let grid = library_grid(app);
    // The grid navigation keys act on the photos in view, so they wait while the full-screen preview
    // covers the grid: a key must not move the selection onto photos nobody can see.
    let grid_navigation = grid && !app.ui.fullscreen;
    // keys the user gave to a known command: the fixed bindings below (aliases, ratings) yield to them
    let taken: Vec<(Modifiers, Key)> = saved_keys(keymap).map(|(_, k)| k).collect();
    // an open popup (a menu, a date picker's calendar) closes on Esc itself: Esc's command (Back,
    // which also closes dialogs) waits until nothing is open
    let popup_open = egui::Popup::is_any_open(ctx);
    // Esc that just took a text field's focus away (it gives back the field's text) does nothing else
    let popup_open = popup_open || app.text_focus;
    ctx.input(|i| {
        // Enter opens the active grid photo; widgets, popups and custom bindings keep the key.
        if grid
            && app.session.active().is_some()
            && !popup_open
            && !native("Enter")
            && !taken.contains(&(Modifiers::NONE, Key::Enter))
            && matches(i, Modifiers::NONE, Key::Enter)
        {
            fire.push("view.detail".to_string());
        }
        for b in bindable() {
            if let Some(sc) = binding(keymap, b.id, b.default)
                && let Some((m, k)) = parse(sc)
                && !native(sc)
                && !(k == Key::Escape && popup_open)
                && matches(i, m, k)
                && let Some(id) = grid_key_command(keymap, grid, app.ui.fullscreen, b.id, (m, k))
            {
                fire.push(id.to_string());
            }
        }
        for (sc, id, params) in ALIASES {
            if let Some((m, k)) = parse(sc).filter(|_| !native(sc))
                && !(k == Key::Escape && popup_open)
                && !taken.contains(&(m, k))
                && matches(i, m, k)
            {
                aliased.push((id, serde_json::from_str(params).unwrap_or_default()));
            }
        }
        // rating 0-5, colour labels 6-9 (with Shift: and advance)
        for (n, key) in [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5].iter().enumerate() {
            if matches(i, Modifiers::NONE, *key) && !native(&n.to_string()) && !taken.contains(&(Modifiers::NONE, *key)) {
                fire.push(format!("rate:{n}:0"));
            }
            if matches(i, Modifiers::SHIFT, *key) && !taken.contains(&(Modifiers::SHIFT, *key)) {
                fire.push(format!("rate:{n}:1"));
            }
        }
        for (label, key, sc) in [("red", Key::Num6, "6"), ("yellow", Key::Num7, "7"), ("green", Key::Num8, "8"), ("blue", Key::Num9, "9")] {
            if matches(i, Modifiers::NONE, key) && !native(sc) && !taken.contains(&(Modifiers::NONE, key)) {
                fire.push(format!("label:{label}"));
            }
        }
        if grid_navigation {
            // Home / End: the first / last photo of the view
            for (key, id) in [(Key::Home, "grid:first"), (Key::End, "grid:last")] {
                if matches(i, Modifiers::NONE, key) && !taken.contains(&(Modifiers::NONE, key)) {
                    fire.push(id.to_string());
                }
            }
            // Shift+← / Shift+→: the previous / next photo of the view joins the selection
            for (key, step) in [(Key::ArrowLeft, -1i64), (Key::ArrowRight, 1)] {
                if matches(i, Modifiers::SHIFT, key) && !taken.contains(&(Modifiers::SHIFT, key)) {
                    fire.push(format!("grid:extend:{step}"));
                }
            }
        }
    });
    use crate::panels::compare;
    // rating/flag/label keys: in Compare/Survey they act on the active photo only; Shift+key or
    // Auto Advance then moves on (next candidate in Compare, next photo elsewhere)
    let cull = |app: &mut LightcraftApp, id: &str, mut params: serde_json::Value, advance: bool| {
        compare::target_active(app, &mut params);
        let was = app.session.active();
        let ok = app.run(id, params).is_ok();
        if ok && (advance || app.ui.auto_advance) {
            compare::advance(app, was);
        }
    };
    for (id, params) in aliased {
        // a slideshow: Space pauses / resumes
        if id == "view.zoomToggle" && app.ui.slideshow.is_some() {
            if let Ok(v) = app.run("slideshow.togglePause", json!({})) {
                app.toast(ctx, crate::i18n::tr(if v["paused"] == true { "Slideshow paused" } else { "Slideshow resumed" }));
            }
            continue;
        }
        // flag/rate aliases (Shift+X…) go through the culling path: active photo in Compare/Survey,
        // `advance` moves to the next candidate there
        if matches!(id, "photo.flag" | "photo.rate" | "photo.label") {
            let mut params = params;
            let advance = params.get("advance").and_then(serde_json::Value::as_bool).unwrap_or(false);
            if let Some(o) = params.as_object_mut() {
                o.remove("advance");
            }
            cull(app, id, params, advance);
        } else if let Err(e) = app.run(id, params)
            && matches!(id, "app.export" | "app.exportPrevious")
        {
            // an export that can't start (e.g. no folder) says why instead of doing nothing
            app.toast(ctx, e);
        }
    }
    let rotate_fires = fire.iter().any(|f| f == "crop.rotateAspect");
    for f in fire {
        // a slideshow: ←/→ step (a cut)
        if app.ui.slideshow.is_some() && matches!(f.as_str(), "library.next" | "library.previous") {
            let _ = app.run(if f == "library.next" { "slideshow.next" } else { "slideshow.previous" }, json!({}));
            continue;
        }
        if let Some(rest) = f.strip_prefix("rate:") {
            let (n, adv) = rest.split_once(':').unwrap_or(("0", "0"));
            cull(app, "photo.rate", json!({"rating": n.parse::<u8>().unwrap_or(0)}), adv == "1");
            let label = if n == "0" {
                crate::i18n::tr("Rating cleared").to_string()
            } else {
                crate::i18n::tr_format!("Rated {}", "★".repeat(n.parse().unwrap_or(0)))
            };
            app.toast(ctx, label);
        } else if matches!(f.as_str(), "photo.decreaseRating" | "photo.increaseRating") {
            cull(app, &f, json!({}), false);
        } else if let Some(l) = f.strip_prefix("label:") {
            cull(app, "photo.label", json!({"label": l}), false);
        } else if f == "panel.presets" && library_grid(app) {
            // Classic's Shift+P picks and advances in Library; keep the Presets binding elsewhere.
            cull(app, "photo.flag", json!({"flag": "pick"}), true);
        } else if f == "view.softProof" && matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid) {
            // S in a grid: expand / collapse the stack (Lightroom's Library binding)
            let _ = app.run("stack.toggle", json!({}));
        } else if f == "grid:first" || f == "grid:last" {
            // Home / End in a grid: make the first / last photo of the view the active, selected one
            let vis = app.session.visible_cloned();
            let edge = if f == "grid:first" { vis.first() } else { vis.last() };
            if let Some(id) = edge {
                let _ = app.run("library.select", json!({"ids": [id.0], "active": id.0}));
            }
        } else if let Some(step) = f.strip_prefix("grid:extend:") {
            // Shift+← / Shift+→ in a grid: the neighbour of the active photo joins the selection and
            // becomes the active one. At either end of the view the key does nothing.
            let step = step.parse::<i64>().unwrap_or(0);
            let vis = app.session.visible_cloned();
            let neighbour = app
                .session
                .active()
                .and_then(|active| vis.iter().position(|p| *p == active))
                .and_then(|pos| i64::try_from(pos).ok())
                .and_then(|pos| pos.checked_add(step))
                .and_then(|pos| usize::try_from(pos).ok())
                .and_then(|pos| vis.get(pos));
            if let Some(id) = neighbour {
                let _ = app.run("library.select", json!({"ids": [id.0], "mode": "add"}));
            }
        } else if compare::culling(app) && (f == "library.next" || f == "library.previous") {
            let d = if f == "library.next" { 1 } else { -1 };
            let _ = if app.ui.view == crate::state::ViewMode::Compare { compare::compare_step(app, d) } else { compare::survey_step(app, d) };
        } else if matches!(f.as_str(), "photo.cycleFlag" | "photo.increaseFlag" | "photo.decreaseFlag") {
            // `, ⌘↑, ⌘↓: step the flag; the toast names the active photo's new state (read before Auto Advance moves on)
            let active = app.session.active();
            cull(app, &f, json!({}), false);
            let flag = active.and_then(|id| app.session.catalog.photo(id)).map(|p| p.flag);
            match flag {
                Some(lightcraft_catalog::Flag::Pick) => app.toast(ctx, crate::i18n::tr("Flagged as Pick")),
                Some(lightcraft_catalog::Flag::Reject) => app.toast(ctx, crate::i18n::tr("Flagged as Reject")),
                Some(lightcraft_catalog::Flag::None) => app.toast(ctx, crate::i18n::tr("Unflagged")),
                None => {}
            }
        } else if matches!(f.as_str(), "photo.pick" | "photo.reject" | "photo.unflag") && app.ui.right != crate::state::RightPanel::Crop {
            cull(app, &f, json!({}), false);
            match f.as_str() {
                "photo.pick" => app.toast(ctx, crate::i18n::tr("Flagged as Pick")),
                "photo.reject" => app.toast(ctx, crate::i18n::tr("Flagged as Reject")),
                _ => app.toast(ctx, crate::i18n::tr("Unflagged")),
            }
        } else {
            // in the full-screen preview (no panels) I cycles the info overlay instead
            if f == "panel.info" && app.ui.fullscreen {
                let _ = app.run("view.infoOverlay", json!({}));
                continue;
            }
            // Delete acts on what's being edited: the active mask in the Masking panel; never the
            // photo while retouching (spots are removed from their own panel).
            if f == "photo.delete" {
                use crate::state::RightPanel as R;
                match app.ui.right {
                    R::Masking => {
                        if app.session.active_mask.is_some() {
                            let _ = app.run("mask.delete", json!({}));
                        }
                        continue;
                    }
                    R::Remove => {
                        if app.session.active_spot.is_some() {
                            let _ = app.run("spot.delete", json!({}));
                        }
                        continue;
                    }
                    R::Crop if app.ui.tool == "guidedUpright" => {
                        // Delete removes the selected guide alone; with none selected it removes
                        // nothing (the photo itself is never deleted while the tool is open)
                        if let (Some((photo, i)), Some(active)) = (app.ui.selected_guide, app.session.active()) {
                            if photo == active.0
                                && let Some(d) = app.session.develop_of(active)
                                && i < d.geometry.guides.len()
                            {
                                let guides: Vec<[f64; 4]> =
                                    d.geometry.guides.iter().enumerate().filter(|(k, _)| *k != i).map(|(_, (a, b))| [a.x, a.y, b.x, b.y]).collect();
                                let _ = app.run("geometry.guides", json!({"guides": guides}));
                            }
                            app.ui.selected_guide = None;
                        }
                        continue;
                    }
                    R::RedEye => continue,
                    _ => {}
                }
                // in an album, Delete takes the photos out of the album only, as in Lightroom: they
                // stay in the library and their folder (issue #1805)
                if crate::menus::delete_in_album(app, ctx) {
                    continue;
                }
                if crate::menus::confirm_delete(app) {
                    continue;
                }
            }
            // B: the brush while editing; in the grids, add to the target album (Quick Collection)
            if f == "tool.brush" && matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid) {
                if let Ok(r) = app.run("album.toggleTarget", json!({})) {
                    let n = app.session.selected().len();
                    let what = crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n);
                    let name = r["name"].as_str().unwrap_or("Quick Collection").to_string();
                    app.toast(
                        ctx,
                        if r["added"] == true {
                            crate::i18n::tr_format!("Added {what} to {name}", name = name, what = what)
                        } else {
                            crate::i18n::tr_format!("Removed {what} from {name}", name = name, what = what)
                        },
                    );
                }
                continue;
            }
            // X is both reject (library) and swap crop aspect (crop tool). When both bindings fire,
            // swap once: twice would put the crop straight back (issue #1866).
            if f == "photo.reject" && app.ui.right == crate::state::RightPanel::Crop {
                if !rotate_fires {
                    let _ = app.run("crop.rotateAspect", json!({}));
                }
                continue;
            }
            if f == "crop.rotateAspect" && app.ui.right != crate::state::RightPanel::Crop {
                continue;
            }
            // A is both Visualize Spots and the crop aspect lock: each key only in its own context
            if f == "view.visualizeSpots" && app.ui.right == crate::state::RightPanel::Crop {
                continue;
            }
            if f == "crop.lockAspect" && app.ui.right != crate::state::RightPanel::Crop {
                continue;
            }
            // / refreshes the selected spot's source in the Remove tool (the filmstrip elsewhere)
            if f == "view.filmstrip" && app.ui.right == crate::state::RightPanel::Remove && app.session.active_spot.is_some() {
                let _ = app.run("spot.refreshSource", json!({}));
                continue;
            }
            // Shift+O cycles the mask overlay colour while masking (the crop overlay elsewhere)
            if f == "view.cropOverlay" && app.ui.right == crate::state::RightPanel::Masking {
                let _ = app.run("view.maskOverlayColor", json!({}));
                continue;
            }
            // while cropping: O cycles the guides, Shift+O their orientation
            if app.ui.right == crate::state::RightPanel::Crop {
                let crop_key = match f.as_str() {
                    "view.maskOverlay" => Some(("view.cropOverlay", json!({}))),
                    "view.cropOverlay" => Some(("view.cropOverlayOrientation", json!({}))),
                    _ => None,
                };
                if let Some((cmd, p)) = crop_key {
                    let _ = app.run(cmd, p);
                    continue;
                }
            }
            // Up / Down in the grids go to the cell above / below (the filmstrip steps one photo)
            if matches!(f.as_str(), "library.up" | "library.down") {
                // over a slider the arrows nudge it (widgets::slider takes the key)
                let over_slider =
                    ctx.pointer_hover_pos().is_some_and(|p| app.widgets.iter().any(|(id, r)| id.starts_with("slider:") && r.contains(p)));
                if over_slider {
                    continue;
                }
                match crate::panels::grid::row_distance(app, f == "library.down") {
                    Some(0) => {}
                    Some(rows) => {
                        let _ = app.run(&f, json!({"rows": rows}));
                    }
                    None => {
                        let _ = app.run(&f, json!({}));
                    }
                }
                continue;
            }
            // an export that can't start (e.g. no folder) says why instead of doing nothing
            if let Err(e) = app.run(&f, json!({}))
                && matches!(f.as_str(), "app.export" | "app.exportPrevious")
            {
                app.toast(ctx, e);
            }
            match f.as_str() {
                "photo.pick" => app.toast(ctx, crate::i18n::tr("Flagged as Pick")),
                "photo.reject" => app.toast(ctx, crate::i18n::tr("Flagged as Reject")),
                "photo.unflag" => app.toast(ctx, crate::i18n::tr("Unflagged")),
                "edit.undo" => app.toast(ctx, crate::i18n::tr("Undo")),
                "edit.redo" => app.toast(ctx, crate::i18n::tr("Redo")),
                _ => {}
            }
        }
    }
}

pub(crate) fn library_grid(app: &LightcraftApp) -> bool {
    matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "shortcuts/tests_relative_ratings.rs"]
mod relative_rating_tests;

#[cfg(test)]
#[path = "shortcuts/tests_flag_cycle.rs"]
mod flag_cycle_tests;

#[cfg(test)]
#[path = "shortcuts/tests_enable_filters.rs"]
mod enable_filters_tests;

#[cfg(test)]
#[path = "shortcuts/tests_filter_bar_key.rs"]
mod filter_bar_key_tests;
