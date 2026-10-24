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
/// ignored.
pub fn binding<'a>(keymap: &'a Keymap, id: &str, default: Option<&'a str>) -> Option<&'a str> {
    match keymap.get(id) {
        Some(sc) if sc.is_empty() => None,
        Some(sc) if parse(sc).is_some() => Some(sc.as_str()),
        _ => default,
    }
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
    let chrome = app
        .ui
        .docking
        .as_ref()
        .is_some_and(|workspace| craft_ui::docking::DockArea::new(egui::Id::new("lightcraft-docking")).chrome_has_focus(ctx, &workspace.layout));
    if !chrome {
        return true;
    }
    ctx.input(|input| {
        [Key::Enter, Key::Space, Key::Tab, Key::Escape, Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown]
            .iter()
            .any(|key| input.key_pressed(*key))
    })
}

pub fn handle(app: &mut LightcraftApp, ctx: &egui::Context) {
    // Space held over a tool ends when the key is up, whoever has the keyboard by then
    crate::space_hand::tick(app, ctx);
    if !matches!(app.ui.dialog, Some(crate::state::Dialog::Shortcuts)) {
        app.recording_shortcut = None;
    }
    // Dialogs own their keys (including Esc and text editing); background photo actions wait.
    if app.ui.dialog.is_some() {
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
    // keys the user gave to a command: the fixed bindings below (aliases, ratings) yield to them
    let taken: Vec<(Modifiers, Key)> = keymap.values().filter_map(|s| parse(s)).collect();
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
        // a held Space repeats: it toggles the zoom (or takes the hand) once per press
        let space_repeat = !i.events.iter().any(|e| matches!(e, egui::Event::Key { key: Key::Space, pressed: true, repeat: false, .. }));
        for (sc, id, params) in ALIASES {
            if let Some((m, k)) = parse(sc).filter(|_| !native(sc))
                && !(k == Key::Escape && popup_open)
                && !(k == Key::Space && space_repeat)
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
        // over a tool on the photo, held Space is the view's hand; a tap toggles on its release
        if id == "view.zoomToggle" && crate::space_hand::begin(app, ctx) {
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
mod tests {

    fn library() -> crate::headless::Headless {
        use lightcraft_catalog::{Op, Photo, PhotoId, Source};
        let mut session = lightcraft_engine::Session::new();
        for id in 1..=4 {
            let photo = Photo::new(
                PhotoId(id),
                Source::File { path: format!("/lightcraft-shortcuts/{id}.jpg") },
                &format!("{id}.jpg"),
                "JPEG",
                100,
                100,
                "2026-10-08",
            );
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
        session.execute("library.select", &json!({"ids": [1]})).unwrap();
        let app = LightcraftApp::new(session, crate::Services { png: None, ..Default::default() });
        let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        h.app.ui.view = crate::state::ViewMode::PhotoGrid;
        for _ in 0..3 {
            h.step();
        }
        h
    }

    fn key(h: &mut crate::headless::Headless, key: &str, shift: bool) {
        let r = h.request("ui.key", json!({"key": key, "shift": shift}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
    }

    /// Press the default Select Only Active Photo chord (the command key on every platform).
    fn select_only_active_chord(h: &mut crate::headless::Headless) {
        let r = h.request("ui.key", json!({"key": "D", "cmd": true, "shift": true}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
    }

    #[test]
    fn select_only_active_keeps_the_active_photo_and_the_edit_undo_step() {
        use crate::state::ViewMode;
        use lightcraft_catalog::PhotoId;
        for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail] {
            let mut h = library();
            h.app.ui.view = view;
            h.app.session.execute("library.select", &json!({"ids": [1, 2, 3], "active": 2})).unwrap();
            h.app.session.execute("photo.rate", &json!({"rating": 4})).unwrap();
            select_only_active_chord(&mut h);
            assert_eq!(h.app.session.selection.ids, vec![PhotoId(2)]);
            assert_eq!(h.app.session.active(), Some(PhotoId(2)));
            // Narrowing the selection does not replace the last photo-edit undo step.
            h.app.session.execute("edit.undo", &json!({})).unwrap();
            for id in 1..=3 {
                assert_eq!(h.app.session.catalog.photo(PhotoId(id)).unwrap().rating, 0);
            }
        }
    }

    #[test]
    fn select_only_active_yields_to_a_focused_text_field() {
        use lightcraft_catalog::PhotoId;
        let mut h = library();
        h.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
        let r = h.request("ui.clickWidget", json!({"id": "field:search"}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
        select_only_active_chord(&mut h);
        assert_eq!(h.app.session.selection.ids, vec![PhotoId(1), PhotoId(2)]);
        assert_eq!(h.app.session.active(), Some(PhotoId(2)));
    }

    #[test]
    fn select_only_active_follows_the_keymap() {
        use lightcraft_catalog::PhotoId;
        let mut h = library();
        let both = vec![PhotoId(1), PhotoId(2)];
        h.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
        assert_eq!(super::shortcut_of(&h.app.ui.settings.keymap, "library.selectOnlyActive"), Some("Cmd+Shift+D"));
        // unbound: the default chord does nothing
        super::assign(&mut h.app.ui.settings.keymap, "library.selectOnlyActive", None).unwrap();
        select_only_active_chord(&mut h);
        assert_eq!(h.app.session.selection.ids, both);
        // rebound: the new key narrows the selection and the old chord still does not
        super::assign(&mut h.app.ui.settings.keymap, "library.selectOnlyActive", Some("F9")).unwrap();
        select_only_active_chord(&mut h);
        assert_eq!(h.app.session.selection.ids, both);
        key(&mut h, "F9", false);
        assert_eq!(h.app.session.selection.ids, vec![PhotoId(2)]);
    }

    #[test]
    fn select_only_active_without_active_photo_does_not_change_selection() {
        let mut h = library();
        // Deselect All keeps the active photo selected, so clear the whole selection state instead.
        h.app.session.selection = Default::default();
        assert!(!crate::menus::ui_enabled(&h.app, "library.selectOnlyActive"));
        assert!(h.app.run("library.selectOnlyActive", json!({})).is_err());
        select_only_active_chord(&mut h);
        assert!(h.app.session.selection.ids.is_empty());
    }

    #[test]
    fn library_rating_labels_flags_and_undo() {
        use lightcraft_catalog::{ColorLabel, Flag, PhotoId};
        for view in [crate::state::ViewMode::PhotoGrid, crate::state::ViewMode::SquareGrid] {
            let mut h = library();
            h.app.ui.view = view;
            h.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 1})).unwrap();
            for rating in [1, 2, 3, 4, 5, 0] {
                key(&mut h, &rating.to_string(), false);
                for id in [1, 2] {
                    assert_eq!(h.app.session.catalog.photo(PhotoId(id)).unwrap().rating, rating);
                }
            }
            for (k, label) in [("6", ColorLabel::Red), ("7", ColorLabel::Yellow), ("8", ColorLabel::Green), ("9", ColorLabel::Blue)] {
                key(&mut h, k, false);
                assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().label, Some(label));
                assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().label, Some(label));
            }
            for (k, flag) in [("P", Flag::Pick), ("X", Flag::Reject), ("U", Flag::None)] {
                key(&mut h, k, false);
                assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, flag);
                assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().flag, flag);
            }
            h.app.session.execute("edit.undo", &json!({})).unwrap();
            assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::Reject);
            assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().flag, Flag::Reject);
            h.app.session.execute("edit.redo", &json!({})).unwrap();
            assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::None);
            assert_eq!(h.app.session.active(), Some(PhotoId(1)));
        }
    }

    #[test]
    fn classic_f6_and_f7_toggle_the_filmstrip_and_left_panel() {
        let mut h = library();
        let (filmstrip, left) = (h.app.ui.filmstrip, h.app.ui.left_panel);
        key(&mut h, "F6", false);
        assert_eq!(h.app.ui.filmstrip, !filmstrip);
        assert_eq!(h.app.ui.left_panel, left);
        key(&mut h, "F7", false);
        assert_eq!(h.app.ui.left_panel, !left);
        key(&mut h, "F6", false);
        key(&mut h, "F7", false);
        assert_eq!((h.app.ui.filmstrip, h.app.ui.left_panel), (filmstrip, left));
    }

    /// Classic's F8 hides the right panel and shows the same one again; the other panels stay.
    #[test]
    fn classic_f8_hides_the_right_panel_and_brings_the_same_one_back() {
        use crate::state::RightPanel;
        let mut h = library();
        h.app.ui.view = crate::state::ViewMode::Detail;
        h.app.ui.right = RightPanel::Keywords;
        h.settle(std::time::Duration::from_secs(10));
        let (filmstrip, left) = (h.app.ui.filmstrip, h.app.ui.left_panel);
        key(&mut h, "F8", false);
        assert_eq!(h.app.ui.right, RightPanel::None);
        assert_eq!((h.app.ui.filmstrip, h.app.ui.left_panel), (filmstrip, left));
        key(&mut h, "F8", false);
        assert_eq!(h.app.ui.right, RightPanel::Keywords, "the panel F8 hid comes back");
        // a tool ends when F8 hides it; with nothing hidden before, F8 opens Edit
        h.app.ui.right = RightPanel::None;
        h.app.ui.right_hidden = RightPanel::None;
        key(&mut h, "F8", false);
        assert_eq!(h.app.ui.right, RightPanel::Edit);
    }

    #[test]
    fn shift_culling_keys_advance_exactly_once() {
        use lightcraft_catalog::{ColorLabel, Flag, PhotoId};
        for auto in [false, true] {
            for (k, rating, label, flag) in [
                ("0", 0, None, Flag::Pick),
                ("5", 5, None, Flag::Pick),
                ("6", 3, Some(ColorLabel::Red), Flag::Pick),
                ("7", 3, Some(ColorLabel::Yellow), Flag::Pick),
                ("8", 3, Some(ColorLabel::Green), Flag::Pick),
                ("9", 3, Some(ColorLabel::Blue), Flag::Pick),
                ("P", 3, None, Flag::Pick),
                ("X", 3, None, Flag::Reject),
                ("U", 3, None, Flag::None),
            ] {
                let mut h = library();
                h.app.ui.auto_advance = auto;
                h.app.session.execute("photo.rate", &json!({"rating": 3})).unwrap();
                h.app.session.execute("photo.pick", &json!({})).unwrap();
                key(&mut h, k, true);
                let p = h.app.session.catalog.photo(PhotoId(1)).unwrap();
                assert_eq!(p.rating, rating, "Shift+{k}");
                assert_eq!(p.label, label, "Shift+{k}");
                assert_eq!(p.flag, flag, "Shift+{k}");
                assert_eq!(h.app.session.active(), Some(PhotoId(2)), "Shift+{k}, auto={auto}");
            }
        }
        let mut h = library();
        h.app.session.execute("library.select", &json!({"ids": [4]})).unwrap();
        key(&mut h, "6", true);
        assert_eq!(h.app.session.active(), Some(PhotoId(4)), "last photo stays selected");
    }

    #[test]
    fn shift_pick_preserves_presets_outside_the_library_grids() {
        let mut h = library();
        h.app.ui.view = crate::state::ViewMode::SquareGrid;
        key(&mut h, "P", true);
        assert_eq!(h.app.session.active(), Some(lightcraft_catalog::PhotoId(2)));
        assert!(!h.app.ui.presets, "grid Shift+P must not open Presets");
        assert_eq!(h.app.ui.view, crate::state::ViewMode::SquareGrid);
        h.app.ui.view = crate::state::ViewMode::Detail;
        key(&mut h, "P", true);
        assert!(h.app.ui.presets);
        assert_eq!(h.app.session.active(), Some(lightcraft_catalog::PhotoId(2)));
    }

    #[test]
    fn shift_label_targets_only_candidate_in_compare() {
        use lightcraft_catalog::{ColorLabel, PhotoId};
        let mut h = library();
        h.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 1})).unwrap();
        crate::panels::compare::enter_compare(&mut h.app).unwrap();
        key(&mut h, "6", true);
        assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().label, None);
        assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().label, Some(ColorLabel::Red));
        assert_eq!(h.app.session.active(), Some(PhotoId(3)));
    }

    #[test]
    fn plain_culling_keys_auto_advance_once() {
        use lightcraft_catalog::PhotoId;
        for k in ["0", "5", "6", "9", "P", "X", "U"] {
            let mut h = library();
            h.app.ui.auto_advance = true;
            key(&mut h, k, false);
            assert_eq!(h.app.session.active(), Some(PhotoId(2)), "{k}");
        }
    }

    #[test]
    fn culling_keys_go_on_from_a_photo_that_leaves_the_filter() {
        // #2312: the filter shows one-star photos; taking photo 2's star off makes photo 3 active, once, with or
        // without Shift and Auto Advance, and → goes on from there instead of from the first photo
        use lightcraft_catalog::PhotoId;
        let one_star = || {
            let mut h = library();
            h.app.session.execute("library.selectAll", &json!({})).unwrap();
            h.app.session.execute("photo.rate", &json!({"rating": 1})).unwrap();
            h.app.session.execute("library.filter", &json!({"rating": 1, "ratingOp": "exactly"})).unwrap();
            h.app.session.execute("library.select", &json!({"ids": [2]})).unwrap();
            h
        };
        for view in [crate::state::ViewMode::PhotoGrid, crate::state::ViewMode::Detail] {
            for (shift, auto) in [(false, false), (true, false), (false, true), (true, true)] {
                let mut h = one_star();
                h.app.ui.view = view;
                h.app.ui.auto_advance = auto;
                key(&mut h, "0", shift);
                assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().rating, 0);
                assert_eq!(h.app.session.active(), Some(PhotoId(3)), "{view:?} Shift={shift} Auto Advance={auto}");
                key(&mut h, "right", false);
                assert_eq!(h.app.session.active(), Some(PhotoId(4)), "{view:?} Shift={shift} Auto Advance={auto}");
            }
        }
        // the Photo menu with Auto Advance too
        let mut h = one_star();
        h.app.ui.auto_advance = true;
        crate::menubar::run_item(&mut h.app, "photo.rate", json!({"rating": 0})).unwrap();
        assert_eq!(h.app.session.active(), Some(PhotoId(3)));
    }

    #[test]
    fn color_label_actions_show_feedback_only_after_success() {
        use lightcraft_catalog::PhotoId;
        let mut h = library();
        for shift in [false, true] {
            for (k, name) in [("6", "Red"), ("7", "Yellow"), ("8", "Green"), ("9", "Blue")] {
                h.app.session.execute("library.select", &json!({"ids": [1]})).unwrap();
                h.app.ui.toast = None;
                key(&mut h, k, shift);
                assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some(format!("{name} Label").as_str()));
                assert_eq!(h.app.ui.toast.as_ref().and_then(|t| t.2), h.app.session.catalog.photo(PhotoId(1)).unwrap().label);
                assert_eq!(h.app.session.active(), Some(PhotoId(if shift { 2 } else { 1 })));
            }
        }
        // Menus and the Info-panel swatches dispatch the same command, including purple/clear.
        h.app.run("photo.label", json!({"label": "purple"})).unwrap();
        assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Purple Label"));
        h.app.run("photo.label", json!({"label": "none"})).unwrap();
        assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Color label cleared"));
        assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.2.is_none()), "clear uses neutral feedback");
        h.app.session.execute("label.setNames", &json!({"names": {"red": "Needs review"}})).unwrap();
        key(&mut h, "6", false);
        assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Needs review Label"));
        // Native menu callbacks run before logic() catches up to the current frame's clock.
        let now = h.view.ctx.input(|i| i.time);
        h.app.last_time = now - 10.0;
        h.app.run("photo.label", json!({"label": "blue"})).unwrap();
        assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.1 > now), "menu feedback must survive an idle gap");
        h.app.ui.toast = None;
        assert!(h.app.run("photo.label", json!({"label": "invalid"})).is_err());
        assert!(h.app.ui.toast.is_none(), "failed actions must not announce success");
    }

    #[test]
    fn shifted_number_punctuation_rates_and_advances() {
        use lightcraft_catalog::PhotoId;
        let mut h = library();
        // A real winit event on a layout with Shift+1 = !, rather than ui.key's logical Num1.
        for pressed in [true, false] {
            h.app.synthetic.push(egui::Event::Key {
                key: Key::Exclamationmark,
                physical_key: Some(Key::Num1),
                pressed,
                repeat: false,
                modifiers: Modifiers::SHIFT,
            });
        }
        h.step();
        assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().rating, 1);
        assert_eq!(h.app.session.active(), Some(PhotoId(2)));
    }

    #[test]
    fn library_keys_yield_to_search_and_crop_context() {
        use lightcraft_catalog::{Flag, PhotoId};
        let mut h = library();
        let r = h.request("ui.clickWidget", json!({"id": "field:search"}), std::time::Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
        for k in ["5", "6", "P", "X", "U"] {
            key(&mut h, k, false);
            key(&mut h, k, true);
        }
        let p = h.app.session.catalog.photo(PhotoId(1)).unwrap();
        assert_eq!(p.rating, 0);
        assert_eq!(p.label, None);
        assert_eq!(p.flag, Flag::None);
        assert_eq!(h.app.session.active(), Some(PhotoId(1)));

        let mut h = library();
        h.app.ui.view = crate::state::ViewMode::Detail;
        h.app.ui.right = crate::state::RightPanel::Crop;
        key(&mut h, "X", false);
        assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::None);
    }

    #[test]
    fn delete_means_the_backspace_key() {
        assert_eq!(parse("Delete"), Some((Modifiers::NONE, Key::Backspace)));
        assert_eq!(parse("Cmd+Delete"), Some((Modifiers::COMMAND, Key::Backspace)));
    }
    use super::*;

    #[test]
    fn parses_shortcuts() {
        let (m, k) = parse("Cmd+Shift+Z").unwrap();
        assert!(m.command && m.shift && !m.alt);
        assert_eq!(k, Key::Z);
        assert_eq!(parse("Right").unwrap().1, Key::ArrowRight);
        assert_eq!(parse("\\").unwrap().1, Key::Backslash);
        // every declared shortcut parses
        for (id, _, sc, _) in crate::menus::ui_commands() {
            if let Some(sc) = sc {
                assert!(parse(sc).is_some(), "{id}: {sc}");
            }
        }
        for c in lightcraft_engine::command_specs() {
            if let Some(sc) = c.shortcut {
                assert!(parse(sc).is_some(), "{}: {sc}", c.id);
            }
        }
    }

    #[test]
    fn aliases_parse_and_target_existing_commands() {
        let ui: Vec<&str> = crate::menus::ui_commands().map(|c| c.0).collect();
        for (sc, id, params) in ALIASES {
            assert!(parse(sc).is_some(), "{id}: {sc}");
            assert!(ui.contains(id) || lightcraft_engine::find_command(id).is_some(), "alias {sc} → unknown command {id}");
            assert!(serde_json::from_str::<serde_json::Value>(params).is_ok(), "alias {sc}: bad params");
        }
    }

    #[test]
    fn key_presses_format_to_shortcuts_that_parse_back() {
        for k in Key::ALL {
            for m in [Modifiers::NONE, Modifiers::SHIFT, Modifiers::COMMAND, Modifiers::ALT | Modifiers::SHIFT] {
                if let Some(sc) = format(m, *k) {
                    let want = if *k == Key::Delete { Key::Backspace } else { *k };
                    assert_eq!(parse(&sc).map(|p| p.1), Some(want), "{sc}");
                }
            }
        }
        assert_eq!(format(Modifiers::COMMAND | Modifiers::SHIFT, Key::K).as_deref(), Some("Cmd+Shift+K"));
        assert_eq!(format(Modifiers::NONE, Key::Slash).as_deref(), Some("/"));
        assert_eq!(format(Modifiers::NONE, Key::Backspace).as_deref(), Some("Delete"));
    }

    #[test]
    fn assigning_a_key_moves_it_and_reset_restores_it() {
        let mut keymap = Keymap::new();
        assert_eq!(shortcut_of(&keymap, "view.survey"), Some("N"));
        // D belongs to Detail: Survey takes it, Detail loses it
        let lost = assign(&mut keymap, "view.survey", Some("D")).unwrap();
        assert_eq!(lost, vec!["view.detail"]);
        assert_eq!(shortcut_of(&keymap, "view.survey"), Some("D"));
        assert_eq!(shortcut_of(&keymap, "view.detail"), None);
        // restoring Detail takes D back
        let lost = reset(&mut keymap, "view.detail").unwrap();
        assert_eq!(lost, vec!["view.survey"]);
        assert_eq!(shortcut_of(&keymap, "view.detail"), Some("D"));
        reset(&mut keymap, "view.survey").unwrap();
        assert!(keymap.is_empty(), "declared shortcuts aren't stored: {keymap:?}");
        // a command without a declared shortcut can get one, and lose it again
        assign(&mut keymap, "view.photoGrid", Some("cmd+shift+1")).unwrap_err();
        assign(&mut keymap, "view.photoGrid", Some("Cmd+Shift+1")).unwrap();
        assert_eq!(shortcut_of(&keymap, "view.photoGrid"), Some("Cmd+Shift+1"));
        assign(&mut keymap, "view.photoGrid", None).unwrap();
        assert!(keymap.is_empty());
    }

    #[test]
    fn bad_assignments_are_errors() {
        let mut keymap = Keymap::new();
        assert!(assign(&mut keymap, "no.suchCommand", Some("K")).is_err());
        assert!(assign(&mut keymap, "view.survey", Some("Cmd+Nonsense")).is_err());
        assert!(assign(&mut keymap, "view.survey", Some("Cmd+Q")).is_err());
        assert!(assign(&mut keymap, "view.survey", Some("Cmd+,")).is_err());
        assert!(keymap.is_empty());
    }

    /// Junk in a hand-edited `ui.json` keymap is ignored (the declared shortcut stays), never a panic.
    #[test]
    fn junk_keymap_entries_mean_no_shortcut() {
        let mut keymap = Keymap::new();
        keymap.insert("view.survey".into(), "Hyper+☃".into());
        keymap.insert("no.suchCommand".into(), "K".into());
        keymap.insert("view.detail".into(), "Cmd+SuperLeft".into());
        assert_eq!(shortcut_of(&keymap, "view.survey"), Some("N"));
        assert_eq!(shortcut_of(&keymap, "view.detail"), Some("D"));
        assert!(conflicts(&keymap, "view.compare", "N").contains(&"view.survey"));
        assert_eq!(parse("Hyper+K"), None);
    }

    /// Contextual partners keep sharing their key when one is reassigned.
    #[test]
    fn contextual_partners_are_not_conflicts() {
        let keymap = Keymap::new();
        assert!(conflicts(&keymap, "photo.reject", "X").is_empty());
    }

    /// No key fires two different actions (a UI command may shadow the engine command it wraps).
    #[test]
    fn no_conflicting_bindings() {
        let mut ui: Vec<((Modifiers, Key), String)> = Vec::new();
        for (id, _, sc, _) in crate::menus::ui_commands() {
            if let Some(k) = sc.and_then(parse) {
                ui.push((k, id.to_string()));
            }
        }
        for (sc, id, _) in ALIASES {
            ui.push((parse(sc).unwrap(), format!("alias {id}")));
        }
        let mut engine: Vec<((Modifiers, Key), &str)> = Vec::new();
        for c in lightcraft_engine::command_specs() {
            if let Some(k) = c.shortcut.and_then(parse) {
                engine.push((k, c.id));
            }
        }
        for (sc, id, _) in ALIASES {
            let k = parse(sc).unwrap();
            assert!(!engine.iter().any(|(k2, _)| *k2 == k), "alias {sc} ({id}) shadows an engine shortcut");
        }
        for (i, (k, a)) in ui.iter().enumerate() {
            for (k2, b) in &ui[i + 1..] {
                let contextual = CONTEXTUAL.iter().any(|(x, y)| (x == a && y == b) || (x == b && y == a));
                assert!(k != k2 || contextual, "{a} and {b} share a key");
            }
        }
        for (i, (k, a)) in engine.iter().enumerate() {
            for (k2, b) in &engine[i + 1..] {
                let contextual = CONTEXTUAL.iter().any(|(x, y)| (x == a && y == b) || (x == b && y == a));
                assert!(k != k2 || contextual, "{a} and {b} share a key");
            }
        }
    }
    mod issue1771 {
        use super::*;
        use crate::state::{Dialog, ViewMode};
        use lightcraft_catalog::PhotoId;

        fn grid(view: ViewMode) -> crate::headless::Headless {
            let mut h = library();
            h.app.ui.view = view;
            h.app.session.execute("library.select", &json!({"ids": [1, 2, 3], "active": 2})).unwrap();
            for _ in 0..3 {
                h.step();
            }
            h
        }

        #[test]
        fn issue1771_photo_grid_enter_opens_active_preserving_multiselection() {
            opens(ViewMode::PhotoGrid);
        }

        #[test]
        fn issue1771_square_grid_enter_opens_active_preserving_multiselection() {
            opens(ViewMode::SquareGrid);
        }

        fn opens(view: ViewMode) {
            let mut h = grid(view);
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, ViewMode::Detail, "Enter from {view:?}");
            assert_eq!(h.app.session.active(), Some(PhotoId(2)));
            assert_eq!(h.app.session.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(3)]);
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, ViewMode::Detail, "second Enter remains in Detail");
            assert_eq!(h.app.session.active(), Some(PhotoId(2)));
        }

        #[test]
        fn issue1771_no_selection_and_modifier_chords_do_not_open() {
            for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
                let mut h = grid(view);
                for modifiers in [json!({"shift":true}), json!({"cmd":true}), json!({"alt":true}), json!({"ctrl":true})] {
                    let mut args = modifiers;
                    args["key"] = json!("Enter");
                    assert_eq!(h.request("ui.key", args, std::time::Duration::from_secs(20))["ok"], true);
                    assert_eq!(h.app.ui.view, view);
                }
                h.app.session.selection = Default::default();
                key(&mut h, "Enter", false);
                assert_eq!(h.app.ui.view, view);
                assert!(h.app.session.selection.ids.is_empty());
                assert_eq!(h.app.session.active(), None);
            }
        }

        #[test]
        fn issue1771_search_dialog_and_shortcut_recording_keep_enter() {
            for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
                let mut h = grid(view);
                assert_eq!(h.request("ui.clickWidget", json!({"id":"field:search"}), std::time::Duration::from_secs(20))["ok"], true);
                key(&mut h, "Enter", false);
                assert_eq!(h.app.ui.view, view, "focused search");
                h.app.run("app.shortcuts", json!({})).unwrap();
                h.step();
                h.step();
                key(&mut h, "Enter", false);
                assert_eq!(h.app.ui.view, view, "shortcut dialog");
                assert!(matches!(h.app.ui.dialog, Some(Dialog::Shortcuts)));
                h.app.recording_shortcut = Some("view.survey".into());
                key(&mut h, "Enter", false);
                assert_eq!(h.app.ui.view, view, "recording owns Enter");
                assert_eq!(super::super::shortcut_of(&h.app.ui.settings.keymap, "view.survey"), Some("Enter"));
            }
        }

        #[test]
        fn issue1771_custom_binding_and_native_menu_keep_enter() {
            for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
                let mut h = grid(view);
                h.app.run("app.setShortcut", json!({"id":"view.survey", "shortcut":"Enter"})).unwrap();
                key(&mut h, "Enter", false);
                assert_eq!(h.app.ui.view, ViewMode::Survey, "custom binding wins");
                assert_eq!(h.app.session.active(), Some(PhotoId(2)));
                assert_eq!(h.app.session.selection.ids.len(), 3);
                let mut h = grid(view);
                h.app.native_shortcuts.insert("Enter".into());
                key(&mut h, "Enter", false);
                assert_eq!(h.app.ui.view, view, "native menu owns Enter");
            }
        }

        #[test]
        fn issue1771_popup_keeps_enter() {
            for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
                let mut h = grid(view);
                let ctx = egui::Context::default();
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        events: vec![egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }],
                        ..Default::default()
                    },
                    |ui| {
                        let ctx = ui.ctx();
                        egui::Popup::open_id(ctx, egui::Id::new("issue1771-menu"));
                        assert!(egui::Popup::is_any_open(ctx));
                        super::super::handle(&mut h.app, ctx);
                    },
                );
                output.textures_delta.clear();
                assert_eq!(h.app.ui.view, view);
            }
        }

        #[test]
        fn issue1771_detail_enter_keeps_active_and_multiselection() {
            let mut h = grid(ViewMode::Detail);
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, ViewMode::Detail);
            assert_eq!(h.app.session.active(), Some(PhotoId(2)));
            assert_eq!(h.app.session.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(3)]);
        }

        #[test]
        fn issue1771_open_file_menu_keeps_enter() {
            for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
                let mut h = grid(view);
                assert_eq!(h.request("ui.clickWidget", json!({"id":"menu:File"}), std::time::Duration::from_secs(20))["ok"], true);
                h.step();
                assert!(egui::Popup::is_any_open(&h.view.ctx), "File menu open before Enter");
                key(&mut h, "Enter", false);
                assert_eq!(h.app.ui.view, view);
                assert_eq!(h.app.session.active(), Some(PhotoId(2)));
                assert_eq!(h.app.session.selection.ids.len(), 3);
            }
        }

        #[test]
        fn issue1771_repeat_event_opens_once_and_detail_keeps_selection() {
            let mut h = grid(ViewMode::PhotoGrid);
            h.events.push(egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: egui::Modifiers::NONE,
            });
            h.step();
            h.step();
            assert_eq!(h.app.ui.view, ViewMode::Detail);
            assert_eq!(h.app.session.active(), Some(PhotoId(2)));
            assert_eq!(h.app.session.selection.ids.len(), 3);
            h.events.push(egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: egui::Modifiers::NONE,
            });
            h.step();
            h.step();
            assert_eq!(h.app.ui.view, ViewMode::Detail);
            assert_eq!(h.app.session.selection.ids.len(), 3);
        }
    }
}

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
