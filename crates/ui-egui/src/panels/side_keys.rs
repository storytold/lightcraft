//! The keyboard in the left sidebar: with Keyboard in Sidebar on (`view.sidebarKeys`, Cmd+Alt+B), the
//! arrow keys move a cursor over the rows and section headers that are drawn, Left / Right fold
//! and unfold, Enter or Space chooses the row as a click does, typing jumps to the next row that
//! starts so, F2 renames, and Esc gives the keys back to the photos.
//!
//! It is a mode asked for, never entered by a click: with it off every key is what it always
//! was (P picks a photo also right after an album was clicked). While it is on, keys without Cmd /
//! Ctrl / Alt belong to the sidebar ([`owns_keys`]); the others stay shortcuts. Tab, a click
//! elsewhere, a text field taking the keys (Find), or the sidebar going out of sight end it.
//!
//! The rows say what they are as they are drawn ([`note_row`], [`note_open`]); a key acts on what
//! the last frame drew, and what it asks of a row (choose, fold) is handed to that row when it is
//! drawn next, so a key does exactly what the click on the row does.
//!
//! Nothing calls this yet: it is the tested core of Keyboard in Sidebar (LR-BEHAV-SIDEBAR-KEYS), and
//! the next part of that feature starts it: the sidebar's rows say what they are through [`note_row`]
//! and the rest, and the `view.sidebarKeys` command, its menu item and its shortcut turn the mode on.
//! Until then the app's keys are what they were, and the module is not counted as dead code outside tests.
#![cfg_attr(not(test), allow(dead_code))]

use egui::{Key, Rect};

use crate::LightcraftApp;

/// A row or section header as the keys see it.
#[derive(Clone, Debug)]
struct NavRow {
    /// The row's widget id without `source:` (`all`, `album:3`, `libfolder:/pics`), or a
    /// header's whole id (`sidebarSection:albums`); a second Local row of the same id in one
    /// frame (a folder that is a place and also lies inside another) has [`TWIN`] and `2` after
    /// it, which no file name can hold.
    id: String,
    /// What typing is matched against.
    label: String,
    indent: f32,
    header: bool,
    /// Whether it is open, when it can be opened at all.
    open: Option<bool>,
    selected: bool,
}

/// What a key asks of a row, done when the row is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// As a click on the row.
    Choose,
    /// As a click on its triangle.
    Fold,
}

#[derive(Clone, Default)]
struct Nav {
    /// The rows drawn so far this frame, top to bottom.
    rows: Vec<NavRow>,
    /// The rows of the last frame: what a key press acts on.
    prev: Vec<NavRow>,
    ask: Option<(String, Ask)>,
    /// What was typed lately, and when the last letter came.
    typed: String,
    typed_at: f64,
    /// The pass the sidebar last began in: keys are the sidebar's only while it is drawn.
    seen: u64,
    /// Whether the keys were the sidebar's when it was last drawn (to say so when that begins).
    was_on: bool,
    /// Whether the keys' toast was shown this session: it explains the mode once.
    told: bool,
    /// How often each Local row's id was drawn so far this frame (only they can repeat).
    twins: std::collections::HashMap<String, u32>,
    /// Whether a dialog or a menu was open when the sidebar was last drawn: what it leaves
    /// focused as it closes is let go, where any other focus ends the mode.
    had_window: bool,
    /// Whether an album or a section was being dragged when the sidebar was last drawn: the Esc
    /// that abandons a drag ends the drag before the sidebar is drawn again, and is the drag's.
    was_dragging: bool,
}

/// What parts a Local row's id from its number when the same folder is drawn more than once: a
/// character no path contains, so a folder really called `Day #2` is never taken for a second row.
const TWIN: char = '\u{1f}';

fn nav_id() -> egui::Id {
    egui::Id::unique("left-side-keys")
}

/// What every row asks about each frame, kept apart from [`Nav`] so asking copies two words:
/// whether the keys are the sidebar's, and the row a key chose this frame (by its response's id).
#[derive(Clone, Copy, Default)]
struct Now {
    on: bool,
    chose: Option<egui::Id>,
}

fn now(ctx: &egui::Context) -> Now {
    ctx.data(|d| d.get_temp::<Now>(nav_id())).unwrap_or_default()
}

fn with<R>(ctx: &egui::Context, f: impl FnOnce(&mut Nav) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<Nav>(nav_id())))
}

/// How long typed letters add up to one word.
const TYPE_AHEAD_SECONDS: f64 = 1.0;
/// The most letters kept: nothing typed by hand is longer, and it bounds what is compared.
const TYPE_AHEAD_MAX: usize = 64;
/// Rows a Page Up / Page Down moves by.
const PAGE: usize = 10;

/// Whether keys without Cmd / Ctrl / Alt are the sidebar's this frame (so not shortcuts; of them it reads typed
/// text, Shift+Tab and the listed keys with no Shift, and any other key with Shift is nothing): the mode is on and
/// the sidebar was drawn within the last two passes or in this one. A sidebar that is not drawn owns no key,
/// and ends the mode (after the few frames it has to appear in when the mode was just asked
/// for), so the keys can never be lost to something out of sight.
pub(crate) fn owns_keys(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    if !app.ui.sidebar_keys {
        return false;
    }
    let seen = with(ctx, |n| n.seen);
    if ctx.cumulative_pass_nr().saturating_sub(seen) > 2 {
        if app.ui.sidebar_keys_grace == 0 {
            app.ui.sidebar_keys = false;
        } else {
            app.ui.sidebar_keys_grace -= 1;
        }
        return false;
    }
    app.ui.sidebar_keys_grace = 0;
    !ctx.input(|i| i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt)
}

/// Forget the rows drawn so far, this frame's and the last one's: they are another library's, so the
/// next [`begin`] has no row to put the cursor on until the sidebar has been drawn again.
pub(crate) fn forget_rows(ctx: &egui::Context) {
    with(ctx, |n| {
        n.rows.clear();
        n.prev.clear();
    });
}

/// A key press or typed text, in the order it came.
enum Typed {
    Key(Key),
    Text(String),
}

/// Start of the sidebar's frame (`panel`: its rect): the rows of the last frame become what the
/// keys act on, and this frame's key presses are read.
pub(crate) fn begin(app: &mut LightcraftApp, ui: &egui::Ui, panel: Rect) {
    let ctx = ui.ctx();
    let pass = ctx.cumulative_pass_nr();
    let on = app.ui.sidebar_keys;
    let (began, tell) = with(ctx, |n| {
        // a sidebar that was not drawn for a while (hidden meanwhile) has stale rows, and the mode was not on in it
        if pass.saturating_sub(n.seen) > 2 {
            n.rows.clear();
            n.was_on = false;
        }
        n.prev = std::mem::take(&mut n.rows);
        n.twins.clear();
        n.seen = pass;
        // a row that was not drawn (folded away meanwhile) is asked nothing later
        n.ask = None;
        let was_on = std::mem::replace(&mut n.was_on, on);
        let began = on && !was_on;
        (began, began && !std::mem::replace(&mut n.told, true))
    });
    ctx.data_mut(|d| d.insert_temp(nav_id(), Now { on, chose: None }));
    if !on {
        return;
    }
    if tell {
        // a mode: say that it is on, what the keys do now and how it ends (once: after that the ring says it)
        app.toast_for(ctx, crate::i18n::tr("Keys are in the sidebar: arrows move, Enter opens, type to find, Esc goes back to the photos"), 6.0);
    }
    // a press anywhere else gives the keys back to what was pressed (a menu or a dialog of the
    // sidebar's own is not "elsewhere"). The in-window View menu is elsewhere: pressing it ends the
    // mode, and its Keyboard in Sidebar item then turns it on, so that item is a way in, not out
    let outside = ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !panel.contains(p)));
    let own_window = app.ui.dialog.is_some() || egui::Popup::is_any_open(ctx);
    if outside && !own_window {
        app.ui.sidebar_keys = false;
        return;
    }
    // a dialog or an open menu has the keys meanwhile, and so has a drag (Esc abandons it)
    let dragging = app.ui.dragging_album.is_some() || app.ui.dragging_section.is_some();
    let dragged = with(ctx, |n| std::mem::replace(&mut n.was_dragging, dragging));
    if own_window || dragging || dragged {
        with(ctx, |n| n.had_window = own_window);
        return;
    }
    // a field that takes the keys has them: Find, a box that was clicked. Only what a dialog or menu left
    // focused as it closed is let go, so renaming a row comes back to the row
    let had_window = with(ctx, |n| std::mem::replace(&mut n.had_window, false));
    if let Some(focused) = ctx.memory(|m| m.focused()) {
        if had_window {
            ctx.memory_mut(|m| m.surrender_focus(focused));
        } else if take_tab(ui) {
            // (egui gave a control the focus for this very Tab, before the sidebar was drawn: it leaves)
            app.ui.sidebar_keys = false;
            ctx.memory_mut(|m| m.surrender_focus(focused));
        } else {
            app.ui.sidebar_keys = false;
        }
        return;
    }
    let prev = with(ctx, |n| std::mem::take(&mut n.prev));
    // nowhere yet, or its row is gone: start at what is shown, else at the top (before the keys, so a key
    // pressed in the first frame that has rows acts from there)
    if !prev.is_empty() && !app.ui.sidebar_cursor.as_ref().is_some_and(|c| prev.iter().any(|r| &r.id == c)) {
        move_to(app, &prev, prev.iter().position(|r| r.selected).unwrap_or(0));
    }
    if !began {
        keys(app, ui, &prev);
    }
    with(ctx, |n| n.prev = prev);
}

/// A Tab the sidebar took still moves egui's focus on (egui reads it before the sidebar is drawn): cancel that
/// move, so the keys go back to the photos and not to the first control.
fn no_focus_move(ctx: &egui::Context) {
    ctx.memory_mut(|m| {
        m.move_focus(egui::FocusDirection::None);
        if let Some(f) = m.focused() {
            m.surrender_focus(f);
        }
    });
}

/// Whether this frame has a Tab (or Shift+Tab) press, taken out so that nothing else reads it.
fn take_tab(ui: &egui::Ui) -> bool {
    ui.input_mut(|i| {
        let mut found = false;
        i.events.retain(|e| match e {
            egui::Event::Key { key: Key::Tab, pressed: true, modifiers, .. } if modifiers.is_none() || modifiers.shift_only() => {
                found = true;
                false
            }
            _ => true,
        });
        found
    })
}

/// This frame's key presses, one after the other, on the rows of the last frame.
fn keys(app: &mut LightcraftApp, ui: &egui::Ui, rows: &[NavRow]) {
    let ctx = ui.ctx();
    let Some(mut at) = app.ui.sidebar_cursor.as_ref().and_then(|c| rows.iter().position(|r| &r.id == c)) else {
        // no row to act on (none was drawn: an empty sidebar, or another library's just opened): Esc and Tab still leave
        let leaves = ui.input_mut(|i| {
            let mut leaves = false;
            i.events.retain(|e| match e {
                egui::Event::Key { key: key @ (Key::Escape | Key::Tab), pressed: true, modifiers, .. }
                    if modifiers.is_none() || (*key == Key::Tab && modifiers.shift_only()) =>
                {
                    leaves = true;
                    false
                }
                _ => true,
            });
            leaves
        });
        if leaves {
            app.ui.sidebar_keys = false;
            crate::widgets::take_escape(ctx);
            no_focus_move(ctx);
        }
        return;
    };
    let (typed, now) = ui.input_mut(|i| {
        const KEYS: [Key; 13] = [
            Key::ArrowDown,
            Key::ArrowUp,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Enter,
            Key::Space,
            Key::Escape,
            Key::F2,
            Key::Tab,
        ];
        let mut typed = Vec::new();
        // presses with no modifier at all are the sidebar's (Tab also with Shift: either way it
        // leaves); taken out of the events so nothing else acts on them
        i.events.retain(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, .. }
                if KEYS.contains(key) && (modifiers.is_none() || (*key == Key::Tab && modifiers.shift_only())) =>
            {
                typed.push(Typed::Key(*key));
                false
            }
            egui::Event::Text(t) if !(i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt) => {
                typed.push(Typed::Text(t.clone()));
                false
            }
            _ => true,
        });
        (typed, i.time)
    });
    let last = rows.len().saturating_sub(1);
    for t in typed {
        let Some(row) = rows.get(at) else { return };
        // a key that is no letter ends the word being typed (a space inside one is a letter)
        let in_word = with(ctx, |n| !n.typed.is_empty() && now - n.typed_at <= TYPE_AHEAD_SECONDS);
        if matches!(t, Typed::Key(k) if !(k == Key::Space && in_word)) {
            with(ctx, |n| n.typed.clear());
        }
        let to = match t {
            Typed::Key(Key::ArrowDown) => Some((at + 1).min(last)),
            Typed::Key(Key::ArrowUp) => Some(at.saturating_sub(1)),
            Typed::Key(Key::PageDown) => Some(at.saturating_add(PAGE).min(last)),
            Typed::Key(Key::PageUp) => Some(at.saturating_sub(PAGE)),
            Typed::Key(Key::Home) => Some(0),
            Typed::Key(Key::End) => Some(last),
            Typed::Key(Key::ArrowRight) => match row.open {
                Some(false) => return ask(ctx, row, Ask::Fold),
                // open: on to the first row inside it, if it has one
                Some(true) => {
                    (at < last && rows.get(at + 1).is_some_and(|next| !next.header && (row.header || next.indent > row.indent))).then_some(at + 1)
                }
                None => None,
            },
            Typed::Key(Key::ArrowLeft) => match row.open {
                Some(true) => return ask(ctx, row, Ask::Fold),
                _ => parent_of(rows, at),
            },
            // (a space while a word is being typed is part of the word when a row goes on so,
            // "blue hour"; after "p" on Picks it chooses)
            Typed::Key(Key::Space)
                if in_word
                    && with(ctx, |n| rows.iter().any(|r| !r.header && name_of(&r.label).to_lowercase().starts_with(&format!("{} ", n.typed)))) =>
            {
                type_more(ctx, rows, at, " ", now)
            }
            // what the row does next is the row's: later keys of this frame wait for nothing
            Typed::Key(Key::Enter | Key::Space) => return ask(ctx, row, Ask::Choose),
            Typed::Key(Key::F2) => return rename(app, ctx, &row.id),
            Typed::Key(Key::Escape) => {
                app.ui.sidebar_keys = false;
                crate::widgets::take_escape(ctx);
                return;
            }
            // Tab leaves, as it leaves any list: the keys go back to the photos
            Typed::Key(Key::Tab) => {
                app.ui.sidebar_keys = false;
                no_focus_move(ctx);
                return;
            }
            Typed::Key(_) => None,
            Typed::Text(text) => {
                let letters: String = text.chars().filter(|c| !c.is_control() && *c != ' ').collect();
                if letters.is_empty() { None } else { type_more(ctx, rows, at, &letters, now) }
            }
        };
        if let Some(to) = to {
            move_to(app, rows, to);
            at = to;
        }
    }
}

/// `more` was typed at `now` with the cursor on row `at`: the row to go to, if any.
fn type_more(ctx: &egui::Context, rows: &[NavRow], at: usize, more: &str, now: f64) -> Option<usize> {
    let word = with(ctx, |n| {
        if now - n.typed_at > TYPE_AHEAD_SECONDS {
            n.typed.clear();
        }
        n.typed_at = now;
        let room = TYPE_AHEAD_MAX.saturating_sub(n.typed.chars().count());
        n.typed.extend(more.to_lowercase().chars().take(room));
        n.typed.clone()
    });
    type_ahead(rows, at, &word)
}

/// The row typing `word` goes to from row `at`: the next one down whose name starts so, round
/// past the end; headers are not typed to. One letter (also the same letter again and again:
/// `s`, `ss`, `sss`) moves on to the next such row each time; a longer word narrows from the
/// row the cursor is on.
fn type_ahead(rows: &[NavRow], at: usize, word: &str) -> Option<usize> {
    let first = word.chars().next()?;
    if rows.is_empty() {
        return None;
    }
    let one_letter = word.chars().all(|c| c == first);
    let (from, word) = if one_letter { (at + 1, first.to_string()) } else { (at, word.to_string()) };
    (0..rows.len())
        .map(|k| (from + k) % rows.len())
        .find(|i| rows.get(*i).is_some_and(|r| !r.header && name_of(&r.label).to_lowercase().starts_with(&word)))
}

/// What a row is called, without the mark the sidebar draws before a smart album whose rules need fixing
/// (`left::albums`): typing the album's name finds it.
fn name_of(label: &str) -> &str {
    label.strip_prefix("⚠ ").unwrap_or(label)
}

/// The row `at` is inside: the nearest one above it that is indented less, or its section's
/// header. None for a header and for the rows above every section (All Photos, Picks…).
fn parent_of(rows: &[NavRow], at: usize) -> Option<usize> {
    let row = rows.get(at)?;
    if row.header {
        return None;
    }
    (0..at).rev().find(|i| rows.get(*i).is_some_and(|r| r.header || r.indent < row.indent))
}

fn move_to(app: &mut LightcraftApp, rows: &[NavRow], to: usize) {
    if let Some(row) = rows.get(to) {
        if app.ui.sidebar_cursor.as_deref() != Some(row.id.as_str()) {
            app.ui.sidebar_cursor_moved = true;
        }
        app.ui.sidebar_cursor = Some(row.id.clone());
    }
}

fn ask(ctx: &egui::Context, row: &NavRow, what: Ask) {
    with(ctx, |n| n.ask = Some((row.id.clone(), what)));
}

/// F2 on a row that has a name of its own: the dialog its menu's Rename opens (a Local place is offered under its
/// folder's own name, where the menu offers the name the row shows, which may be translated).
fn rename(app: &mut LightcraftApp, ctx: &egui::Context, id: &str) {
    use crate::state::Dialog;
    // (a second row of one Local folder carries its number after `TWIN`: the same folder)
    let id = id.split_once(TWIN).map_or(id, |(id, _)| id);
    // a folder renamed on disk, as Rename Folder… of its menu does (Local's by its name, a library folder's by its last two)
    let folder = |path: &str, called: String| Dialog::TextPrompt {
        title: crate::i18n::tr_format!("Rename “{name}”", name = called),
        hint: "Folder name (renamed on disk; its photos follow)".into(),
        value: std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string()),
        command: "folder.rename".into(),
        params: serde_json::json!({"path": path}),
        key: "name".into(),
    };
    // (an album's row is `album:<id>`, a folder of albums' `folder:<id>`)
    let dialog = if let Some(album) = id.strip_prefix("album:").or_else(|| id.strip_prefix("folder:")).and_then(|n| n.parse::<u64>().ok()) {
        let name = app.session.catalog.albums().find(|a| a.id.0 == album).map(|a| a.name.clone());
        name.map(|name| Dialog::RenameAlbum { id: album, name })
    } else if let Some(path) = id.strip_prefix("keyword:") {
        Some(Dialog::RenameKeyword { from: path.to_string(), to: path.to_string() })
    } else if let Some(path) = id.strip_prefix("libfolder:").filter(|p| !lightcraft_catalog::folders::is_disk_root(p)) {
        Some(folder(path, lightcraft_catalog::folders::folder_label(path)))
    } else if let Some(path) = id.strip_prefix("local:").filter(|p| std::path::Path::new(p).is_absolute()) {
        let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string());
        name.map(|name| folder(path, name))
    } else {
        None
    };
    match dialog {
        Some(d) => app.ui.dialog = Some(d),
        None => app.toast(ctx, crate::i18n::tr("This row has no name to change")),
    }
}

/// What the keys make of a row as it is drawn.
#[derive(Clone, Copy, Default)]
pub(crate) struct Noted {
    /// The cursor is on it: it is drawn ringed.
    pub cursor: bool,
    /// A key asked for it to be chosen (headers: folded or unfolded) this frame.
    pub chosen: bool,
}

/// A row (or, with `header`, a section's header) is being drawn: `id` as in its widget id,
/// `label` what it says, `resp` its response. With the keys elsewhere this is one comparison:
/// a library's every row passes through here each frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn note_row(
    app: &mut LightcraftApp,
    ui: &egui::Ui,
    id: &str,
    label: &str,
    indent: f32,
    header: bool,
    open: Option<bool>,
    selected: bool,
    resp: &egui::Response,
) -> Noted {
    if !app.ui.sidebar_keys {
        return Noted::default();
    }
    let (id, asked) = with(ui.ctx(), |n| {
        // a second row of one Local folder is a row of its own to the keys
        let nth = if id.starts_with("local:") {
            let seen = n.twins.entry(id.to_string()).or_insert(0);
            *seen = seen.saturating_add(1);
            *seen
        } else {
            1
        };
        let id = if nth <= 1 { id.to_string() } else { format!("{id}{TWIN}{nth}") };
        let mine = n.ask.as_ref().is_some_and(|(row, what)| *row == id && (*what == Ask::Choose || header));
        if mine {
            n.ask = None;
        }
        // (a header that was asked folds or unfolds as it is drawn: the keys' next press sees it as it is now, not as it was)
        let open = open.map(|o| o != (mine && header));
        n.rows.push(NavRow { id: id.clone(), label: label.to_string(), indent, header, open, selected });
        (id, mine)
    });
    if asked {
        ui.ctx().data_mut(|d| d.insert_temp(nav_id(), Now { on: true, chose: Some(resp.id) }));
    }
    // a click puts the cursor where the pointer went, so keys go on from there
    if resp.clicked() {
        app.ui.sidebar_cursor = Some(id.clone());
    }
    let cursor = app.ui.sidebar_cursor.as_deref() == Some(id.as_str());
    if cursor && std::mem::take(&mut app.ui.sidebar_cursor_moved) {
        // into view at once (a key held down moves row by row, and an eased scroll would trail
        // behind it), and clear of the header pinned over the top rows
        let clear = if header { 0.0 } else { super::left::SECTION_HEADER };
        let rect = Rect::from_min_max(resp.rect.min - egui::vec2(0.0, clear), resp.rect.max);
        ui.scroll_to_rect_animation(rect, None, egui::style::ScrollAnimation::none());
    }
    Noted { cursor, chosen: asked }
}

/// A row that is out of sight and not drawn (`left::out_of_sight`) is still one of the keys'
/// rows: the arrows pass over it and typing finds it. Returns whether it has to be drawn after
/// all: the cursor is on it (it is then scrolled to), or a key asked something of it. `selected`
/// is asked only for a row that is listed (the mode is on), so a sidebar without it pays nothing. A second row
/// of one Local folder is counted as [`note_row`] counts it (a row that has to be drawn after all is counted
/// there, not here).
pub(crate) fn note_unseen(app: &LightcraftApp, ui: &egui::Ui, id: &str, label: &str, indent: f32, selected: impl FnOnce() -> bool) -> bool {
    if !app.ui.sidebar_keys {
        return false;
    }
    with(ui.ctx(), |n| {
        let local = id.starts_with("local:");
        let nth = if local { n.twins.get(id).copied().unwrap_or(0).saturating_add(1) } else { 1 };
        let id_of_row = if nth <= 1 { id.to_string() } else { format!("{id}{TWIN}{nth}") };
        if app.ui.sidebar_cursor.as_deref() == Some(id_of_row.as_str()) || n.ask.as_ref().is_some_and(|(row, _)| *row == id_of_row) {
            return true;
        }
        if local {
            n.twins.insert(id.to_string(), nth);
        }
        n.rows.push(NavRow { id: id_of_row, label: label.to_string(), indent, header: false, open: None, selected: selected() });
        false
    })
}

/// The row drawn last can be opened and is `open`; returns whether a key asked to fold or
/// unfold it (the caller flips it, as for a click on its triangle).
pub(crate) fn note_open(ui: &egui::Ui, open: bool) -> bool {
    if !now(ui.ctx()).on {
        return false;
    }
    with(ui.ctx(), |n| {
        let Some(row) = n.rows.last_mut() else { return false };
        let asked = n.ask.as_ref().is_some_and(|(id, what)| *id == row.id && *what == Ask::Fold);
        if asked {
            n.ask = None;
        }
        row.open = Some(open != asked);
        asked
    })
}

/// Whether row `resp` was chosen: clicked, or chosen with Enter / Space while the keys are the
/// sidebar's.
pub(crate) fn chosen(ui: &egui::Ui, resp: &egui::Response) -> bool {
    resp.clicked() || now(ui.ctx()).chose == Some(resp.id)
}

/// The ring round the row the cursor is on (`bar`: the row's highlight bar): 2 pt in the text's
/// own color, drawn inside `bar`, so it shows on the row that is shown (a filled bar) and on one that is not.
pub(crate) fn ring(ui: &egui::Ui, bar: Rect) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.painter().rect_stroke(bar, 4.0, egui::Stroke::new(2.0, t.text), egui::StrokeKind::Inside);
    crate::widgets::register(ui, "sidebarCursor", bar);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<NavRow> {
        let r = |id: &str, label: &str, indent: f32, header: bool| NavRow {
            id: id.into(),
            label: label.into(),
            indent,
            header,
            open: None,
            selected: false,
        };
        vec![
            r("all", "All Photos", 0.0, false),
            r("sidebarSection:albums", "Albums", 0.0, true),
            r("album:1", "Travel", 0.0, false),
            r("album:2", "Lisbon", 16.0, false),
            r("album:3", "Tokyo", 16.0, false),
            r("album:4", "Weddings", 0.0, false),
            r("sidebarSection:keywords", "Keywords", 0.0, true),
            r("keyword:travel", "travel", 0.0, false),
        ]
    }

    #[test]
    fn left_goes_to_the_row_a_row_is_inside_then_to_its_sections_header() {
        let rows = rows();
        assert_eq!(parent_of(&rows, 4), Some(2), "Tokyo is inside Travel");
        assert_eq!(parent_of(&rows, 2), Some(1), "Travel is in Albums");
        assert_eq!(parent_of(&rows, 1), None, "a header is inside nothing");
        assert_eq!(parent_of(&rows, 0), None, "All Photos is in no section");
        assert_eq!(parent_of(&rows, 99), None, "a row that is not there");
    }

    #[test]
    fn typing_goes_to_the_next_row_that_starts_so_and_wraps() {
        let rows = rows();
        assert_eq!(type_ahead(&rows, 0, "t"), Some(2), "Travel");
        assert_eq!(type_ahead(&rows, 2, "t"), Some(4), "the same letter again moves on: Tokyo");
        assert_eq!(type_ahead(&rows, 4, "t"), Some(7), "then the keyword, whatever its case");
        assert_eq!(type_ahead(&rows, 7, "t"), Some(2), "and round to the first again");
        assert_eq!(type_ahead(&rows, 2, "to"), Some(4), "a second letter narrows from where the first landed");
        assert_eq!(type_ahead(&rows, 0, "a"), Some(0), "wrapping back to the row itself when it is the only one (All Photos; never a header)");
        assert_eq!(type_ahead(&rows, 2, "tt"), Some(4), "the same letter typed again, still within the second, moves on too");
        assert_eq!(type_ahead(&rows, 4, "ttt"), Some(7));
        assert_eq!(type_ahead(&rows, 0, "zz"), None);
        assert_eq!(type_ahead(&rows, 0, ""), None);
        assert_eq!(type_ahead(&[], 0, "t"), None);
    }
}
