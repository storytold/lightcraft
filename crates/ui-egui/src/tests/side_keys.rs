//! The core of Keyboard in Sidebar (`panels::side_keys`, LR-BEHAV-SIDEBAR-KEYS), with no sidebar of the
//! app drawn: a small stand-in sidebar of rows is drawn the way the real one calls the core (`begin`,
//! then `note_row` / `note_open` / `note_unseen` / `chosen` / `ring` for each row), and the keys are
//! pressed on it frame by frame.
//!
//! - Off: no cursor, and every key is left to whoever read it before.
//! - On: a cursor starts on the row of what is shown; the arrow keys, Home, End and Page Up / Down
//!   move it, Enter and Space choose its row as a click does, Left / Right fold, unfold, and step
//!   in and out, typing goes to the next row that starts so, F2 opens the row's rename dialog.
//! - It ends on Esc and Tab, on a click outside the sidebar, when a field takes the keys, and when
//!   the sidebar is no longer drawn; keys with Cmd / Ctrl / Alt are not the sidebar's.

use std::collections::{HashMap, HashSet};

use egui::{Event, Key, Modifiers, Pos2, Rect, Sense, vec2};

use crate::panels::side_keys::{begin, chosen, forget_rows, note_open, note_row, note_unseen, owns_keys, ring};
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const ROW: f32 = 24.0;
/// What a header pinned over the top rows covers (`left::SECTION_HEADER`, which the core clears).
const PINNED: f32 = 34.0;
const FIELD: &str = "side-keys-test-field";

/// One row of the stand-in sidebar.
#[derive(Clone)]
struct Row {
    id: String,
    label: String,
    indent: f32,
    header: bool,
    /// Has a triangle (a folder of albums): Left / Right fold it. A header folds too.
    foldable: bool,
    /// Out of sight and so not drawn, as `left::out_of_sight` leaves rows (unless the keys ask for it).
    unseen: bool,
}

fn row(id: &str, label: &str) -> Row {
    Row { id: id.into(), label: label.into(), indent: 0.0, header: false, foldable: false, unseen: false }
}

impl Row {
    fn at(mut self, indent: f32) -> Self {
        self.indent = indent;
        self
    }
    fn header(mut self) -> Self {
        self.header = true;
        self.foldable = true;
        self
    }
    fn fold(mut self) -> Self {
        self.foldable = true;
        self
    }
    fn unseen(mut self) -> Self {
        self.unseen = true;
        self
    }
}

/// The rows the tests use unless they say otherwise (index: row).
/// 0 all, 1 recentlyAdded, 2 picks, 3 recentlyDeleted, 4 Albums (header), 5 Travel (a folder), 6 Lisbon and 7 Tokyo
/// inside it, 8 Weddings, 9 Keywords (header), 10 travel, 11 zebra (out of sight).
fn standard() -> Vec<Row> {
    vec![
        row("all", "All Photos"),
        row("recentlyAdded", "Recently Added"),
        row("picks", "Picks"),
        row("recentlyDeleted", "Recently Deleted"),
        row("sidebarSection:albums", "Albums").header(),
        row("album:1", "Travel").fold(),
        row("album:2", "Lisbon").at(16.0),
        row("album:3", "Tokyo").at(16.0),
        row("album:4", "Weddings"),
        row("sidebarSection:keywords", "Keywords").header(),
        row("keyword:travel", "travel"),
        row("keyword:zebra", "zebra").unseen(),
    ]
}

/// The stand-in sidebar, 200 pt wide at the left of a 600 pt window, with the app state the keys use.
struct Sidebar {
    ctx: egui::Context,
    app: LightcraftApp,
    rows: Vec<Row>,
    /// The id of the row that is shown (what the grid shows).
    shown: String,
    /// Foldable rows that are folded shut.
    closed: HashSet<String>,
    visible: bool,
    /// A text field outside the sidebar, drawn when set.
    field: bool,
    field_text: String,
    view_h: f32,
    queued: Vec<Event>,
    time: f64,
    // what the last frame showed
    owned: bool,
    drawn: Vec<String>,
    rects: HashMap<String, Rect>,
    /// The ids of the rows whose choosing was asked for.
    chosen: Vec<String>,
    /// The row ringed, by id and by where it is.
    ring: Option<(String, Rect)>,
    left_keys: Vec<Key>,
    left_text: Vec<String>,
    offset: f32,
    /// What the scroll area shows of its rows.
    inner: Rect,
}

fn panel() -> Rect {
    Rect::from_min_size(Pos2::ZERO, vec2(200.0, 600.0))
}

impl Sidebar {
    fn new(rows: Vec<Row>) -> Self {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut s = Self {
            ctx: egui::Context::default(),
            app,
            rows,
            shown: "all".into(),
            closed: HashSet::new(),
            visible: true,
            field: false,
            field_text: String::new(),
            view_h: 600.0,
            queued: Vec::new(),
            time: 1.0,
            owned: false,
            drawn: Vec::new(),
            rects: HashMap::new(),
            chosen: Vec::new(),
            ring: None,
            left_keys: Vec::new(),
            left_text: Vec::new(),
            offset: 0.0,
            inner: Rect::NOTHING,
        };
        s.frame();
        s
    }

    /// One frame, in the order the app has: the shortcut pass asks who owns the keys, then the sidebar is drawn.
    fn frame(&mut self) {
        let raw = egui::RawInput {
            events: std::mem::take(&mut self.queued),
            time: Some(self.time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 700.0))),
            ..Default::default()
        };
        self.time += 0.05;
        let Self {
            ctx,
            app,
            rows,
            shown,
            closed,
            visible,
            field,
            field_text,
            view_h,
            owned,
            drawn,
            rects,
            chosen: chose,
            ring: ringed,
            left_keys,
            left_text,
            offset,
            inner,
            ..
        } = self;
        drawn.clear();
        rects.clear();
        left_keys.clear();
        left_text.clear();
        *ringed = None;
        let mut out = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            ui.spacing_mut().item_spacing.y = 0.0;
            *owned = owns_keys(app, &ctx);
            if *visible {
                begin(app, ui, panel());
                let mut in_view = Rect::NOTHING;
                let out = egui::ScrollArea::vertical().id_salt("stand-in").max_height(*view_h).auto_shrink(false).show(ui, |ui| {
                    in_view = ui.clip_rect();
                    let mut skip: Option<(f32, bool)> = None;
                    for r in rows.iter() {
                        if let Some((indent, header)) = skip {
                            if !r.header && (header || r.indent > indent) {
                                continue;
                            }
                            skip = None;
                        }
                        let open = !closed.contains(&r.id);
                        let (_, rect) = ui.allocate_space(vec2(190.0, ROW));
                        if r.unseen && !note_unseen(app, ui, &r.id, &r.label, r.indent, || *shown == r.id) {
                            continue;
                        }
                        let resp = ui.interact(rect, ui.scope_id().with(&r.id), Sense::click());
                        let noted = note_row(app, ui, &r.id, &r.label, r.indent, r.header, r.header.then_some(open), *shown == r.id, &resp);
                        let flip = |closed: &mut HashSet<String>| {
                            if !closed.remove(&r.id) {
                                closed.insert(r.id.clone());
                            }
                        };
                        // (as `left::tree_toggle`, and as a header or a folder row is chosen)
                        let mut toggled = false;
                        if r.foldable && !r.header && note_open(ui, open) {
                            flip(closed);
                            toggled = true;
                        }
                        if r.header {
                            if noted.chosen || resp.clicked() {
                                flip(closed);
                            }
                        } else if chosen(ui, &resp) {
                            chose.push(r.id.clone());
                            if r.foldable && !toggled {
                                flip(closed);
                            }
                        }
                        if noted.cursor {
                            ring(ui, rect);
                            *ringed = Some((r.id.clone(), rect));
                        }
                        drawn.push(r.id.clone());
                        rects.insert(r.id.clone(), rect);
                        // (a header's rows follow its state now; a folder's, as in `albums_tree`, the state it was drawn with)
                        let hides = if r.header { closed.contains(&r.id) } else { !open };
                        if r.foldable && hides {
                            skip = Some((r.indent, r.header));
                        }
                    }
                });
                *offset = out.state.clamped_offset().y;
                *inner = in_view;
            }
            if *field {
                ui.add(egui::TextEdit::singleline(field_text).id(egui::Id::unique(FIELD)));
            }
            ui.input(|i| {
                for e in &i.events {
                    match e {
                        Event::Key { key, pressed: true, .. } => left_keys.push(*key),
                        Event::Text(t) => left_text.push(t.clone()),
                        _ => {}
                    }
                }
            });
        });
        out.textures_delta.clear();
    }

    /// What `view.sidebarKeys` (turning the mode on) does to the app's state.
    fn turn_on(&mut self) {
        self.app.ui.sidebar_keys = true;
        self.app.ui.sidebar_cursor = None;
        self.app.ui.sidebar_cursor_moved = true;
        self.app.ui.sidebar_keys_grace = 4;
    }

    /// The mode turned on, then two frames.
    fn enter(&mut self) {
        self.turn_on();
        self.frame();
        self.frame();
    }

    fn press(&mut self, key: Key) {
        self.queue_key(key, Modifiers::NONE);
        self.frame();
    }

    /// The modifier keys held from now on.
    fn hold(&mut self, mods: Modifiers) {
        self.queued.push(Event::ModifiersChanged(mods));
        self.frame();
    }

    fn queue_key(&mut self, key: Key, modifiers: Modifiers) {
        self.queued.push(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
    }

    fn text(&mut self, t: &str) {
        self.queued.push(Event::Text(t.into()));
        self.frame();
    }

    /// A click at `pos`: pressed in one frame, released in the next.
    fn click(&mut self, pos: Pos2) {
        self.queued.push(Event::PointerMoved(pos));
        self.queued.push(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        self.frame();
        self.queued.push(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        self.frame();
    }

    fn click_row(&mut self, id: &str) {
        let at = self.rects.get(id).unwrap_or_else(|| panic!("{id} is not drawn")).center();
        self.click(at);
    }

    /// Frames for a scroll the cursor asked for to be done (the scroll area takes a request in the frame after it).
    fn settle(&mut self) {
        self.frame();
        self.frame();
    }

    /// The id of the row the ring is round.
    fn at(&self) -> Option<&str> {
        self.ring.as_ref().map(|(id, _)| id.as_str())
    }

    fn on(&self) -> bool {
        self.app.ui.sidebar_keys
    }

    fn said(&self) -> String {
        self.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
    }

    fn open_rename_of(&mut self, id: &str) -> Option<Dialog> {
        self.app.ui.sidebar_cursor = Some(id.into());
        self.frame();
        self.press(Key::F2);
        let dialog = self.app.ui.dialog.take();
        self.frame();
        dialog
    }
}

/// Given the mode was never asked for, then no row is ringed, no key is taken, and a click on a
/// row chooses it as ever without taking the keys.
#[test]
fn off_there_is_no_cursor_and_every_key_is_left_alone() {
    let mut s = Sidebar::new(standard());
    s.frame();
    s.press(Key::ArrowDown);
    assert_eq!(s.left_keys, [Key::ArrowDown], "the arrow was not taken");
    s.press(Key::Enter);
    assert_eq!(s.left_keys, [Key::Enter]);
    s.text("p");
    assert_eq!(s.left_text, ["p"], "the typed letter was not taken");
    for key in [Key::Escape, Key::Tab] {
        s.press(key);
        assert_eq!(s.left_keys, [key], "{key:?} was not taken: it closes a dialog, or goes on to the next control");
    }
    assert!(!s.owned, "no key is the sidebar's");
    assert_eq!((s.at(), s.app.ui.sidebar_cursor.as_deref()), (None, None));
    assert!(s.chosen.is_empty());
    s.click_row("picks");
    assert_eq!(s.chosen, ["picks"], "a click chooses");
    assert!(!s.on(), "a click on a row does not take the keys");
    assert_eq!(s.app.ui.sidebar_cursor, None);
    assert_eq!(s.said(), "", "and nothing is said");
}

/// Given the mode is turned on, then the cursor is on the row of what is shown and the mode says how it ends;
/// Down and Up move it and choose nothing; Enter and Space choose the row as a click does; Home, End
/// and the page keys go far; the top and the bottom stop it.
#[test]
fn the_cursor_starts_on_the_shown_row_and_the_keys_move_and_choose() {
    let mut s = Sidebar::new(standard());
    s.shown = "picks".into();
    s.enter();
    assert!(s.on());
    assert_eq!(s.at(), Some("picks"), "it starts where the grid is");
    assert!(s.said().contains("Esc"), "the mode says how it ends: {:?}", s.said());
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("recentlyDeleted"));
    assert!(s.chosen.is_empty(), "moving the cursor chooses nothing");
    s.press(Key::ArrowUp);
    s.press(Key::Enter);
    assert_eq!(s.chosen, ["picks"], "Enter is a click on the row");
    s.press(Key::ArrowUp);
    s.press(Key::Space);
    assert_eq!(s.chosen, ["picks", "recentlyAdded"], "so is Space");
    assert!(s.left_keys.is_empty(), "the keys were the sidebar's");
    s.press(Key::Home);
    assert_eq!(s.at(), Some("all"));
    s.press(Key::ArrowUp);
    assert_eq!(s.at(), Some("all"), "the top is the top");
    s.press(Key::PageDown);
    assert_eq!(s.at(), Some("keyword:travel"), "a page is ten rows");
    s.press(Key::PageDown);
    assert_eq!(s.at(), Some("keyword:zebra"), "and stops at the last row, which is drawn when the cursor is on it though it was out of sight");
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("keyword:zebra"), "the bottom is the bottom");
    s.press(Key::PageUp);
    assert_eq!(s.at(), Some("recentlyAdded"));
    s.press(Key::End);
    assert_eq!(s.at(), Some("keyword:zebra"));
    s.press(Key::Home);
    assert_eq!(s.at(), Some("all"));
    let (_, ring) = s.ring.clone().expect("a ring");
    assert_eq!(
        crate::widgets::take_registry(&s.ctx).iter().rfind(|(w, _)| w == "sidebarCursor").map(|(_, r)| *r),
        Some(ring),
        "the ring is registered by its widget id"
    );
}

/// Left and Right fold and unfold what the cursor is on (a section by its header, a folder of
/// albums by its row) and step into and out of it.
#[test]
fn left_and_right_fold_unfold_and_step_in_and_out() {
    let mut s = Sidebar::new(standard());
    s.shown = "album:3".into();
    s.enter();
    assert_eq!(s.at(), Some("album:3"));
    s.press(Key::ArrowLeft);
    assert_eq!(s.at(), Some("album:1"), "Left steps out to the folder it is in");
    s.press(Key::ArrowLeft);
    s.frame();
    assert!(!s.drawn.contains(&"album:2".to_string()), "Left on an open folder folds it");
    assert_eq!(s.at(), Some("album:1"));
    assert!(s.chosen.is_empty(), "folding is a click on the triangle, not on the row");
    s.press(Key::ArrowRight);
    s.frame();
    assert!(s.drawn.contains(&"album:2".to_string()), "Right unfolds it");
    s.press(Key::ArrowRight);
    assert_eq!(s.at(), Some("album:2"), "Right on an open folder steps in");
    s.press(Key::ArrowRight);
    assert_eq!(s.at(), Some("album:2"), "a row with nothing in it stays");
    s.press(Key::ArrowLeft);
    s.press(Key::ArrowLeft);
    s.press(Key::ArrowLeft);
    assert_eq!(s.at(), Some("sidebarSection:albums"), "out of the folder is the section's header");
    assert!(s.drawn.contains(&"album:4".to_string()));
    s.press(Key::ArrowLeft);
    assert!(!s.drawn.contains(&"album:4".to_string()), "Left on a header folds the section");
    s.press(Key::ArrowLeft);
    assert_eq!(s.at(), Some("sidebarSection:albums"), "a header is inside nothing");
    s.press(Key::Enter);
    assert!(s.drawn.contains(&"album:4".to_string()), "Enter on a header is a click on it");
    s.press(Key::ArrowRight);
    assert_eq!(s.at(), Some("album:1"), "Right on an open header steps in");
    s.press(Key::Enter);
    assert_eq!(s.chosen, ["album:1"], "Enter on a folder's row chooses it");
    s.frame();
    assert!(s.drawn.contains(&"album:2".to_string()), "and unfolds it (it was folded), as a click on it does");
}

/// A smart album whose rules need fixing is drawn as "⚠ name": typing its name finds it, and the mark does not.
#[test]
fn typing_finds_a_smart_album_flagged_with_a_warning_by_its_name() {
    let mut rows = standard();
    rows[8] = row("album:4", "⚠ Weddings");
    let mut s = Sidebar::new(rows);
    s.enter();
    s.text("w");
    assert_eq!(s.at(), Some("album:4"), "Weddings, whatever mark is drawn before it");
    s.time += 2.0;
    s.text("⚠");
    assert_eq!(s.at(), Some("album:4"), "the mark is not a name: the cursor stays");
    // a space typed inside a flagged name is part of the word, as in an unflagged one
    let mut rows = standard();
    rows[8] = row("album:4", "⚠ Blue hour");
    let mut s = Sidebar::new(rows);
    s.enter();
    s.text("blue");
    assert_eq!(s.at(), Some("album:4"));
    s.press(Key::Space);
    assert!(s.chosen.is_empty(), "the space went on with the word: {:?}", s.chosen);
    s.text("h");
    assert_eq!(s.at(), Some("album:4"));
}

/// A Local folder listed twice and out of sight is two rows to the keys, as when it is drawn: the arrows pass over each.
#[test]
fn a_local_folder_listed_twice_out_of_sight_is_two_rows() {
    let mut s = Sidebar::new(vec![row("all", "All Photos"), row("local:/x", "x").unseen(), row("local:/x", "x").unseen(), row("picks", "Picks")]);
    s.enter();
    assert_eq!(s.at(), Some("all"));
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("local:/x"));
    let first = s.app.ui.sidebar_cursor.clone();
    s.press(Key::ArrowDown);
    let second = s.app.ui.sidebar_cursor.clone();
    assert_ne!(second, first, "the second row is a row of its own");
    assert!(second.as_deref().is_some_and(|id| id.starts_with("local:/x")), "{second:?}");
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("picks"));
    s.press(Key::ArrowUp);
    assert_eq!(s.app.ui.sidebar_cursor, second, "and Up comes back to it");
}

/// Typing goes to the next row whose name starts so, the same letter again to the one after;
/// a pause starts a new word; the letters are the sidebar's, with a modifier they are not.
#[test]
fn typing_finds_a_row_and_the_same_letter_goes_on() {
    let mut s = Sidebar::new(standard());
    s.enter();
    s.text("t");
    assert_eq!(s.at(), Some("album:1"), "Travel");
    assert!(s.left_text.is_empty(), "the letter was the sidebar's");
    s.text("t");
    assert_eq!(s.at(), Some("album:3"), "the same letter again: Tokyo");
    s.text("t");
    assert_eq!(s.at(), Some("keyword:travel"), "then the keyword, whatever its case");
    s.text("t");
    assert_eq!(s.at(), Some("album:1"), "and round");
    s.time += 2.0;
    s.text("t");
    assert_eq!(s.at(), Some("album:3"), "after a pause a letter is a new word");
    s.text("o");
    assert_eq!(s.at(), Some("album:3"), "a second letter narrows: \"to\" is Tokyo");
    s.time += 2.0;
    s.text("w");
    assert_eq!(s.at(), Some("album:4"), "after a pause \"w\" is a new word (\"tow\" starts no row): Weddings");
    s.time += 2.0;
    s.text("z");
    assert_eq!(s.at(), Some("keyword:zebra"), "a row out of sight is found too");
    s.time += 2.0;
    s.text("q");
    assert_eq!(s.at(), Some("keyword:zebra"), "no row starts so: the cursor stays");
    assert!(s.owned, "unmodified keys are the sidebar's");
    for mods in [Modifiers::COMMAND, Modifiers::CTRL, Modifiers::ALT] {
        s.hold(mods);
        assert!(!s.owned, "{mods:?} keys stay shortcuts");
        s.text("t");
        assert_eq!(s.left_text, ["t"], "{mods:?} T was not taken");
        assert_eq!(s.at(), Some("keyword:zebra"));
    }
    s.hold(Modifiers::NONE);
    assert!(s.owned);
}

/// A word of more than one letter narrows from the row the cursor is on: after T went to Travel, R stays there ("tr"),
/// where another T would have gone on.
#[test]
fn a_longer_word_narrows_from_the_row_the_cursor_is_on() {
    let mut s = Sidebar::new(standard());
    s.enter();
    s.text("t");
    assert_eq!(s.at(), Some("album:1"), "Travel");
    s.text("r");
    assert_eq!(s.at(), Some("album:1"), "\"tr\" starts the row the cursor is on, so it stays (not the keyword below)");
    s.time += 2.0;
    s.text("t");
    assert_eq!(s.at(), Some("album:3"), "after a pause the next T is a new word and goes on: Tokyo");
}

/// Shift+Enter is not read by the sidebar (it would otherwise choose a range, which the keys
/// do not offer), nor is Cmd+Down; a space while a word is being typed belongs to the word, and after the first
/// letter of a row it chooses.
#[test]
fn shift_is_left_alone_and_a_space_inside_a_word_is_typed() {
    let mut s = Sidebar::new(standard());
    s.shown = "picks".into();
    s.enter();
    s.queue_key(Key::Enter, Modifiers::SHIFT);
    s.frame();
    assert_eq!(s.left_keys, [Key::Enter], "Shift+Enter was left alone");
    assert!(s.chosen.is_empty());
    s.queue_key(Key::ArrowDown, Modifiers::COMMAND);
    s.frame();
    assert_eq!(s.left_keys, [Key::ArrowDown], "so was Cmd+Down");
    assert_eq!(s.at(), Some("picks"));
    s.press(Key::Home);
    s.text("r");
    assert_eq!(s.at(), Some("recentlyAdded"));
    s.text("r");
    assert_eq!(s.at(), Some("recentlyDeleted"), "the same letter again moves on");
    s.press(Key::End);
    s.text("all");
    assert_eq!(s.at(), Some("all"));
    s.press(Key::Space);
    assert!(s.chosen.is_empty(), "a space inside \"all\" and the row \"All Photos\" is typed, not a choice");
    s.text("p");
    assert_eq!(s.at(), Some("all"), "\"all p\" is All Photos");
    s.press(Key::Home);
    s.text("p");
    s.press(Key::Space);
    assert_eq!(s.chosen, ["picks"], "a space after a letter no row goes on with chooses");
}

/// Keys that come in one frame act one after the other: Down, Down, Enter chooses the row the cursor
/// got to, not the one it was on.
#[test]
fn keys_of_one_frame_act_in_order() {
    let mut s = Sidebar::new(standard());
    s.enter();
    for k in [Key::ArrowDown, Key::ArrowDown, Key::Enter] {
        s.queue_key(k, Modifiers::NONE);
    }
    s.frame();
    assert_eq!(s.at(), Some("picks"));
    assert_eq!(s.chosen, ["picks"], "Enter chose where the cursor had got to");
}

/// Esc and Tab (also with Shift) give the keys back and are not passed on; the ring goes.
#[test]
fn esc_and_tab_give_the_keys_back() {
    for (key, mods) in [(Key::Escape, Modifiers::NONE), (Key::Tab, Modifiers::NONE), (Key::Tab, Modifiers::SHIFT)] {
        let mut s = Sidebar::new(standard());
        s.enter();
        s.press(Key::ArrowDown);
        s.queue_key(key, mods);
        s.frame();
        assert!(!s.on(), "{key:?} {mods:?}");
        assert!(s.left_keys.is_empty(), "{key:?} was taken");
        s.frame();
        // (egui moves the focus on a Tab by itself: it must not land on a control, or the keys are not the photos')
        assert!(s.ctx.memory(|m| m.focused()).is_none(), "{key:?} {mods:?} left the focus on a control");
        assert_eq!(s.at(), None, "the ring goes");
        assert!(!s.owned);
        s.press(Key::ArrowDown);
        assert_eq!(s.left_keys, [Key::ArrowDown], "the arrows are the photos' again");
    }
}

/// The same with no row to act on (an empty sidebar): Tab and Shift+Tab leave and hand the focus to nothing.
#[test]
fn tab_with_no_rows_gives_the_keys_back_without_moving_the_focus() {
    for mods in [Modifiers::NONE, Modifiers::SHIFT] {
        let mut s = Sidebar::new(Vec::new());
        s.enter();
        s.field = true;
        s.frame();
        s.queue_key(Key::Tab, mods);
        s.frame();
        s.frame();
        assert!(!s.on(), "{mods:?}");
        assert!(s.ctx.memory(|m| m.focused()).is_none(), "{mods:?} left the focus on a control");
    }
}

/// While an album is dragged the sidebar's keys wait, and the Esc that gives the drag up is the drag's:
/// the frame the drag ends in does not take it either, so it is not also the sidebar's.
#[test]
fn esc_during_a_drag_is_the_drags() {
    let mut s = Sidebar::new(standard());
    s.enter();
    s.app.ui.dragging_album = Some(1);
    s.press(Key::ArrowDown);
    assert_eq!(s.left_keys, [Key::ArrowDown], "no key is taken during a drag");
    assert_eq!(s.at(), Some("all"));
    s.app.ui.dragging_album = None;
    s.press(Key::Escape);
    assert!(s.on(), "the Esc that ended the drag does not end the mode");
    assert_eq!(s.left_keys, [Key::Escape], "it is left to the drag");
    s.press(Key::Escape);
    assert!(!s.on(), "the next one is the sidebar's");
}

/// The frame the mode comes on in has seen no rows and takes no key, on every entry and not only the first.
#[test]
fn the_frame_the_mode_comes_on_in_takes_no_key() {
    let mut s = Sidebar::new(standard());
    s.enter();
    s.press(Key::Escape);
    s.frame();
    s.turn_on();
    s.press(Key::Escape);
    assert!(s.on(), "the entry frame did not act on rows of nothing");
    assert_eq!(s.left_keys, [Key::Escape], "and left the key alone");
    s.press(Key::Escape);
    assert!(!s.on(), "the next frame acts on its rows");
}

/// A key pressed in the first frame that has rows acts from the shown row, not on nothing.
#[test]
fn a_key_in_the_second_frame_acts_from_the_shown_row() {
    let mut s = Sidebar::new(standard());
    s.shown = "picks".into();
    s.turn_on();
    s.frame();
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("recentlyDeleted"), "Down went on from the row that is shown");
    assert!(s.left_keys.is_empty());
}

/// A sidebar that was hidden meanwhile starts afresh: the mode asked for again puts the cursor on the row that is
/// shown now, not on the one that was shown when the sidebar went.
#[test]
fn a_sidebar_hidden_meanwhile_starts_afresh() {
    let mut s = Sidebar::new(standard());
    s.shown = "picks".into();
    s.enter();
    s.visible = false;
    for _ in 0..4 {
        s.frame();
    }
    assert!(!s.on(), "the mode ended with the sidebar");
    s.shown = "album:4".into();
    s.turn_on();
    s.visible = true;
    s.press(Key::Escape);
    assert!(s.on(), "the frame it comes on in takes no key");
    assert_eq!(s.left_keys, [Key::Escape]);
    s.frame();
    assert_eq!(s.at(), Some("album:4"), "the row shown now");
}

/// A dialog, an open menu and a dragged section have the keys meanwhile: the sidebar takes none, and has them again after.
#[test]
fn a_dialog_a_menu_and_a_drag_have_the_keys_meanwhile() {
    let mut s = Sidebar::new(standard());
    s.enter();
    s.app.ui.dialog = Some(Dialog::RenameKeyword { from: "a".into(), to: "a".into() });
    s.press(Key::ArrowDown);
    assert_eq!(s.left_keys, [Key::ArrowDown], "a dialog has the keys");
    s.app.ui.dialog = None;
    s.frame();
    egui::Popup::open_id(&s.ctx, egui::Id::unique("stand-in-menu"));
    assert!(egui::Popup::is_any_open(&s.ctx));
    s.press(Key::ArrowDown);
    assert_eq!(s.left_keys, [Key::ArrowDown], "an open menu has the keys");
    egui::Popup::close_all(&s.ctx);
    s.frame();
    s.app.ui.dragging_section = Some(crate::sidebar::SidebarSection::Albums);
    s.press(Key::ArrowDown);
    assert_eq!(s.left_keys, [Key::ArrowDown], "a dragged section has the keys");
    s.app.ui.dragging_section = None;
    s.frame();
    s.frame();
    assert_eq!(s.at(), Some("all"), "nothing moved meanwhile");
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("recentlyAdded"), "and the sidebar has them again");
    assert!(s.left_keys.is_empty());
}

/// Right on an open section with no row in it stays on its header (it does not step on to the next section's).
#[test]
fn right_on_an_open_section_with_nothing_in_it_stays() {
    let mut s = Sidebar::new(vec![
        row("all", "All Photos"),
        row("sidebarSection:albums", "Albums").header(),
        row("sidebarSection:keywords", "Keywords").header(),
        row("keyword:travel", "travel"),
    ]);
    s.enter();
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("sidebarSection:albums"));
    s.press(Key::ArrowRight);
    assert_eq!(s.at(), Some("sidebarSection:albums"), "no row inside it to step to");
}

/// With no row drawn to act on, the arrows do nothing and Esc and Tab still leave.
#[test]
fn with_no_row_to_act_on_esc_and_tab_still_leave() {
    for key in [Key::Escape, Key::Tab] {
        let mut s = Sidebar::new(Vec::new());
        s.enter();
        s.press(Key::ArrowDown);
        assert_eq!(s.left_keys, [Key::ArrowDown], "no row, no move");
        assert!(s.on());
        s.press(key);
        assert!(!s.on(), "{key:?}");
        assert!(s.left_keys.is_empty(), "{key:?} was taken");
    }
}

/// A click outside the sidebar gives the keys to what was clicked; a click on a row of it keeps them
/// and puts the cursor there; with a dialog or a menu of the sidebar open, a click elsewhere does not end the mode.
#[test]
fn a_click_elsewhere_gives_the_keys_back_and_a_click_on_a_row_moves_the_cursor() {
    let mut s = Sidebar::new(standard());
    s.enter();
    s.click_row("keyword:travel");
    assert!(s.on(), "a click in the sidebar keeps the keys there");
    assert_eq!(s.at(), Some("keyword:travel"), "and the cursor goes where the click went");
    assert_eq!(s.chosen, ["keyword:travel"]);
    s.click(Pos2::new(400.0, 300.0));
    assert!(!s.on(), "a click on the photos gives them the keys");
    s.enter();
    s.app.ui.dialog = Some(Dialog::RenameKeyword { from: "a".into(), to: "a".into() });
    s.click(Pos2::new(400.0, 300.0));
    assert!(s.on(), "a dialog has the keys meanwhile: the click is in its world");
}

/// The mode explains itself once: the first time the keys come in it says what they do; later the ring says it.
#[test]
fn the_first_time_says_what_the_keys_do() {
    let mut s = Sidebar::new(standard());
    s.enter();
    assert!(s.said().contains("Esc"), "{:?}", s.said());
    s.app.ui.toast = None;
    s.press(Key::Escape);
    s.frame();
    s.enter();
    assert!(s.on());
    assert!(s.app.ui.toast.is_none(), "the second time the ring says it");
}

/// Keys are never left with something out of sight: when the sidebar is not drawn, the mode ends after the frames
/// a hidden sidebar had to come back in; asked for while it is hidden, it has a few frames to appear.
#[test]
fn the_mode_ends_with_the_sidebar_and_a_hidden_sidebar_has_a_few_frames() {
    let mut s = Sidebar::new(standard());
    s.enter();
    s.visible = false;
    s.frame();
    s.frame();
    assert!(s.on(), "a frame or two without it is not the end");
    s.frame();
    s.frame();
    assert!(!s.on(), "no sidebar, no sidebar keys");
    assert!(!s.owned);
    // asked for while hidden
    let mut s = Sidebar::new(standard());
    s.visible = false;
    for _ in 0..8 {
        s.frame();
    }
    s.enter();
    assert!(s.on());
    s.frame();
    assert!(s.on(), "a few frames to appear in");
    assert!(!s.owned, "and no key is the sidebar's until it does");
    s.visible = true;
    s.frame();
    s.frame();
    assert!(s.on() && s.owned, "it appeared in time");
    assert_eq!(s.at(), Some("all"));
}

/// A field that takes the focus (Find, a box that was clicked) takes the keys: the mode ends and what is typed
/// goes to the field. What a dialog leaves focused as it closes is no such field: it is let go and the keys stay.
#[test]
fn a_field_that_takes_the_focus_takes_the_keys() {
    let mut s = Sidebar::new(standard());
    s.field = true;
    s.enter();
    s.ctx.memory_mut(|m| m.request_focus(egui::Id::unique(FIELD)));
    s.frame();
    s.frame();
    assert!(!s.on(), "the field has the keys");
    s.text("pets");
    assert_eq!(s.left_text, ["pets"], "what was typed was left to the field");
    assert_eq!(s.at(), None);
    // a dialog that closes leaving its field focused
    let mut s = Sidebar::new(standard());
    s.field = true;
    s.enter();
    s.app.ui.dialog = Some(Dialog::RenameKeyword { from: "a".into(), to: "a".into() });
    s.frame();
    s.ctx.memory_mut(|m| m.request_focus(egui::Id::unique(FIELD)));
    s.frame();
    s.frame();
    assert!(s.on(), "the dialog has the keys meanwhile");
    s.app.ui.dialog = None;
    s.frame();
    assert!(s.on(), "the dialog's field is not a field that took the keys");
    assert!(s.ctx.memory(|m| m.focused()).is_none(), "its focus was let go");
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("recentlyAdded"), "and the keys work");
}

/// The keys pass over rows that are out of sight and are not drawn; a row asked for is drawn after all.
#[test]
fn the_keys_reach_rows_that_are_out_of_sight() {
    let mut s = Sidebar::new(standard());
    s.enter();
    assert!(!s.drawn.contains(&"keyword:zebra".to_string()), "the last keyword is out of sight and not drawn");
    s.press(Key::End);
    assert_eq!(s.at(), Some("keyword:zebra"), "the cursor is on it, and it is drawn, ringed");
    s.press(Key::Enter);
    assert_eq!(s.chosen, ["keyword:zebra"], "and chosen, though it was out of sight");
    s.press(Key::ArrowUp);
    assert!(!s.drawn.contains(&"keyword:zebra".to_string()), "off it, it is out of sight again");
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("keyword:zebra"), "the arrows went over it");
}

/// A cursor that moves asks the scroll area to show its row (End scrolls the list), and moving up leaves the row clear
/// of the header pinned over the top rows.
#[test]
fn the_cursor_stays_in_view_and_never_under_the_pinned_header() {
    let mut s = Sidebar::new(standard());
    s.view_h = 100.0;
    s.enter();
    s.press(Key::End);
    s.settle();
    assert!(s.offset > 0.0, "End scrolled the list");
    let mut checked = 0;
    for _ in 0..8 {
        s.press(Key::ArrowUp);
        s.settle();
        let (id, ring) = s.ring.clone().expect("a cursor");
        if s.offset > 0.0 && !id.starts_with("sidebarSection:") {
            assert!(ring.top() >= s.inner.top() + PINNED - 0.5, "the ring {ring:?} is under the pinned header in {:?}", s.inner);
            checked += 1;
        }
    }
    assert!(checked > 3, "the list was scrolled while moving up: {checked}");
}

/// F2 opens the rename dialog of what the row is (an album, a folder of albums, a keyword, a library
/// folder, a Local folder) and says so for a row with no name to change.
#[test]
fn f2_renames_the_row_under_the_cursor() {
    let probe = Sidebar::new(Vec::new());
    let albums: Vec<_> = probe.app.session.catalog.albums().map(|a| (a.id.0, a.name.clone(), a.folder, a.parent.is_none())).collect();
    let (album, album_name) = albums.iter().find(|a| !a.2 && a.3).map(|a| (a.0, a.1.clone())).expect("a demo album");
    let (folder, folder_name) = albums.iter().find(|a| a.2 && a.3).map(|a| (a.0, a.1.clone())).expect("a demo album folder");
    let local = std::env::temp_dir().join("Day#2").to_string_lossy().to_string();
    let rows = vec![
        row("all", "All Photos"),
        row(&format!("album:{album}"), &album_name),
        row(&format!("folder:{folder}"), &folder_name).fold(),
        row("keyword:pets/cats", "cats"),
        row("libfolder:/photos/Trips", "Trips"),
        row("libfolder:/", "This Computer"),
        row(&format!("local:{local}"), "Day #2"),
        row(&format!("local:{local}"), "Day #2"),
        row("local:relative", "relative"),
    ];
    let mut s = Sidebar::new(rows);
    s.enter();
    s.press(Key::F2);
    assert_eq!(s.app.ui.dialog, None, "All Photos has no name to change");
    assert!(s.said().contains("no name"), "{:?}", s.said());
    assert_eq!(s.open_rename_of(&format!("album:{album}")), Some(Dialog::RenameAlbum { id: album, name: album_name }));
    assert_eq!(s.open_rename_of(&format!("folder:{folder}")), Some(Dialog::RenameAlbum { id: folder, name: folder_name }));
    assert_eq!(s.open_rename_of("keyword:pets/cats"), Some(Dialog::RenameKeyword { from: "pets/cats".into(), to: "pets/cats".into() }));
    match s.open_rename_of("libfolder:/photos/Trips") {
        Some(Dialog::TextPrompt { value, command, params, .. }) => {
            assert_eq!((value.as_str(), command.as_str()), ("Trips", "folder.rename"));
            assert_eq!(params["path"], "/photos/Trips");
        }
        other => panic!("no rename dialog: {other:?}"),
    }
    #[cfg(unix)]
    {
        s.app.ui.toast = None;
        assert_eq!(s.open_rename_of("libfolder:/"), None, "a disk's root is not renamed");
        assert!(s.said().contains("no name"), "{:?}", s.said());
    }
    // the same folder drawn twice is two rows to the keys, and F2 on either is the one folder
    s.app.ui.sidebar_cursor = Some(format!("local:{local}"));
    s.frame();
    let below = s.rects["local:relative"].top();
    let (id, ring) = s.ring.clone().expect("a cursor");
    assert_eq!(id, format!("local:{local}"));
    assert_eq!(ring.top() + 2.0 * ROW, below, "the cursor is on the first of the twins");
    s.press(Key::ArrowDown);
    let (_, ring) = s.ring.clone().expect("a cursor");
    assert_eq!(ring.top() + ROW, below, "Down goes to the second of the twins, which is a row of its own");
    s.press(Key::F2);
    match s.app.ui.dialog.take() {
        Some(Dialog::TextPrompt { value, params, .. }) => {
            assert_eq!(value, "Day#2", "the keys' own mark of a row drawn twice is no part of a name");
            assert_eq!(params["path"], local);
        }
        other => panic!("no rename dialog: {other:?}"),
    }
    s.frame();
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("local:relative"), "and Down goes on past them");
    assert_eq!(s.open_rename_of("local:relative"), None, "a Local row that is no folder has no name to change");
}

/// A cursor whose row is gone starts again at the shown row; the rows of another library are forgotten
/// ([`forget_rows`]), so a key pressed before the new sidebar is drawn acts on nothing of the old one.
#[test]
fn a_cursor_with_no_row_starts_at_the_shown_row_and_a_new_library_forgets_the_rows() {
    let mut s = Sidebar::new(standard());
    s.shown = "picks".into();
    s.enter();
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("recentlyDeleted"));
    s.rows.retain(|r| r.id != "recentlyDeleted");
    s.frame();
    s.frame();
    assert_eq!(s.at(), Some("picks"), "its row is gone: the shown row it is");
    // another library is opened: the old rows are not acted on, and the next frame starts afresh
    forget_rows(&s.ctx);
    s.app.ui.sidebar_cursor = None;
    s.rows = vec![row("all", "All Photos"), row("picks", "Picks"), row("album:9", "Other")];
    s.press(Key::ArrowDown);
    assert_eq!(s.left_keys, [Key::ArrowDown], "no row of the old library took the key");
    assert_eq!(s.app.ui.sidebar_cursor, None);
    s.frame();
    assert_eq!(s.at(), Some("picks"), "the cursor starts from the new rows");
    s.press(Key::ArrowDown);
    assert_eq!(s.at(), Some("album:9"));
}
