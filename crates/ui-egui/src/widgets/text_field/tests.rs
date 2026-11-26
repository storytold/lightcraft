use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};

use super::*;
use crate::headless::HeadlessView;

const FIELD: &str = "cityField";

/// A field alone in a window, with a blank area beside it to click away to.
struct Rig {
    view: HeadlessView,
    text: String,
    select_on_focus: bool,
    /// The field is on screen (a dialog that holds it may close).
    shown: bool,
    /// The app's window has the system's focus (the user may switch to another app).
    window_focused: bool,
    /// The field said an edit is under way, last frame.
    editing: bool,
    endings: Vec<Ending>,
    widgets: Vec<(String, Rect)>,
    time: f64,
}

impl Rig {
    fn new(text: &str) -> Self {
        let mut rig = Rig {
            view: HeadlessView::new(),
            text: text.to_string(),
            select_on_focus: false,
            shown: true,
            window_focused: true,
            editing: false,
            endings: vec![],
            widgets: vec![],
            time: 0.0,
        };
        rig.frame(vec![]);
        rig
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.frame_with(events, false);
    }

    /// A frame that asks for the field's focus after drawing it, as a dialog does, so the
    /// focus takes effect on the next frame.
    fn frame_with(&mut self, events: Vec<Event>, focus_after: bool) {
        self.time += 1.0 / 60.0;
        let mut raw = HeadlessView::raw_input(egui::vec2(400.0, 300.0), 1.0, self.time, events);
        raw.focused = self.window_focused;
        let (text, select, shown, endings, editing) = (&mut self.text, self.select_on_focus, self.shown, &mut self.endings, &mut self.editing);
        self.view.run(raw, |ui| {
            ui.add_space(20.0);
            if !shown {
                return;
            }
            let r = TextField::singleline(FIELD, text).width(200.0).select_on_focus(select).show(ui);
            endings.extend(r.ending);
            *editing = r.editing;
            if focus_after {
                ui.memory_mut(|m| m.request_focus(id(FIELD)));
            }
        });
        self.widgets = crate::widgets::take_registry(&self.view.ctx);
    }

    /// A few frames, for menus to open and requests to come back.
    fn settle(&mut self) {
        for _ in 0..4 {
            self.frame(vec![]);
        }
    }

    fn rect(&self, id: &str) -> Option<Rect> {
        self.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
    }

    fn click_at(&mut self, at: Pos2, button: PointerButton) {
        // far apart in time from the last click: two clicks never make a double-click
        self.time += 1.0;
        let press = |pressed| Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE };
        self.frame(vec![Event::PointerMoved(at), press(true)]);
        self.frame(vec![press(false)]);
        self.settle();
    }

    fn click(&mut self, id: &str, button: PointerButton) {
        let r = self.rect(id).unwrap_or_else(|| panic!("no {id} on screen: {:?}", self.widgets));
        self.click_at(r.center(), button);
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        let ev = |pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
        self.frame(vec![ev(true), ev(false)]);
        self.settle();
    }

    fn type_text(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.to_string())]);
        self.settle();
    }

    fn focused(&self) -> bool {
        self.view.ctx.memory(|m| m.focused()).is_some()
    }

    /// Click into the field and select all its text with ⌘A.
    fn select_all_by_keys(&mut self) {
        self.click(FIELD, PointerButton::Primary);
        self.key(Key::A, Modifiers::COMMAND);
    }

    /// Right-click the field, then pick `item` from its menu.
    fn menu(&mut self, item: &str) {
        self.click(FIELD, PointerButton::Secondary);
        self.click(&format!("{FIELD}:{item}"), PointerButton::Primary);
    }
}

/// Right-click offers Cut, Copy, Paste and Select All.
#[test]
fn right_click_offers_cut_copy_paste_and_select_all() {
    let mut rig = Rig::new("Lisbon");
    rig.click(FIELD, PointerButton::Secondary);
    for item in ["cut", "copy", "paste", "selectAll"] {
        assert!(rig.rect(&format!("{FIELD}:{item}")).is_some(), "{item}: {:?}", rig.widgets);
    }
}

/// Copy in the menu puts the selected text on the clipboard and leaves the field as it was,
/// still being edited.
#[test]
fn copy_in_the_menu_copies_the_selection() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.menu("copy");
    assert_eq!(rig.view.clipboard, "Lisbon");
    assert_eq!(rig.text, "Lisbon");
    assert!(rig.focused(), "still editing");
    assert_eq!(rig.endings, vec![], "using the menu is not leaving the field");
}

/// Cut in the menu puts the selected text on the clipboard and takes it out of the field.
#[test]
fn cut_in_the_menu_moves_the_selection_to_the_clipboard() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.menu("cut");
    assert_eq!(rig.view.clipboard, "Lisbon");
    assert_eq!(rig.text, "");
}

/// Paste in the menu puts the clipboard's text in place of the selection.
#[test]
fn paste_in_the_menu_replaces_the_selection() {
    let mut rig = Rig::new("Lisbon");
    rig.view.clipboard = "Porto".into();
    rig.select_all_by_keys();
    rig.menu("paste");
    assert_eq!(rig.text, "Porto");
}

/// Select All in the menu selects the whole text: what is typed next replaces it.
#[test]
fn select_all_in_the_menu_selects_the_whole_text() {
    let mut rig = Rig::new("Lisbon");
    rig.click(FIELD, PointerButton::Primary);
    rig.menu("selectAll");
    rig.type_text("Kyoto");
    assert_eq!(rig.text, "Kyoto");
}

/// #1877: a right-click elsewhere in the field while its menu is open moves the menu there
/// (it flashed at the new spot and closed).
#[test]
fn right_clicking_again_moves_the_menu() {
    let mut rig = Rig::new("Lisbon");
    rig.click(FIELD, PointerButton::Secondary);
    let before = rig.rect(&format!("{FIELD}:cut")).expect("the menu");
    let rect = rig.rect(FIELD).unwrap();
    let at = egui::pos2(rect.left() + 10.0, rect.center().y);
    rig.click_at(at, PointerButton::Secondary);
    let after = rig.rect(&format!("{FIELD}:cut")).unwrap_or_else(|| panic!("the menu is still open: {:?}", rig.widgets));
    assert!(after.left() < before.left(), "the menu moved to the pointer: {after:?} (was {before:?})");
    assert!(rig.endings.is_empty(), "the edit goes on");
}

#[test]
fn clicking_back_into_field_dismisses_its_menu_and_places_a_new_caret() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.click(FIELD, PointerButton::Secondary);
    let rect = rig.rect(FIELD).unwrap();
    rig.click_at(egui::pos2(rect.left() + 2.0, rect.center().y), PointerButton::Primary);
    let range = egui::text_edit::TextEditState::load(&rig.view.ctx, id(FIELD)).unwrap().cursor.char_range().unwrap();
    assert!(range.is_empty(), "a click back in the field places a caret: {range:?}");
    rig.type_text("X");
    assert_eq!(rig.text, "XLisbon");
    assert!(rig.endings.is_empty(), "dismissing into the field keeps the edit open");
}

/// With nothing selected, Cut and Copy are greyed out: clicking one does nothing, as in a native
/// menu, and neither closes the menu nor ends the edit.
#[test]
fn greyed_out_items_do_nothing() {
    let mut rig = Rig::new("Lisbon");
    rig.click(FIELD, PointerButton::Primary);
    rig.click(FIELD, PointerButton::Secondary);
    for item in ["cut", "copy"] {
        rig.click(&format!("{FIELD}:{item}"), PointerButton::Primary);
        assert!(rig.rect(&format!("{FIELD}:{item}")).is_some(), "{item}: the menu is still open");
    }
    assert_eq!((rig.text.as_str(), rig.view.clipboard.as_str()), ("Lisbon", ""));
    assert_eq!(rig.endings, vec![], "still editing");
    rig.menu("selectAll");
    rig.type_text("Kyoto");
    assert_eq!(rig.text, "Kyoto", "and the edit goes on");
}

/// Esc gives back the text from before the edit and says the edit was cancelled.
#[test]
fn escape_gives_back_the_text_from_before_the_edit() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    assert_eq!(rig.text, "Rome");
    rig.key(Key::Escape, Modifiers::NONE);
    assert_eq!(rig.text, "Lisbon");
    assert_eq!(rig.endings, vec![Ending::Escape]);
}

/// Return ends the edit and keeps the text.
#[test]
fn return_ends_the_edit() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    rig.key(Key::Enter, Modifiers::NONE);
    assert_eq!(rig.text, "Rome");
    assert_eq!(rig.endings, vec![Ending::Return]);
}

/// Return with the menu open ends the edit as Return does, and closes the menu.
#[test]
fn return_with_the_menu_open_ends_the_edit() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    rig.click(FIELD, PointerButton::Secondary);
    rig.key(Key::Enter, Modifiers::NONE);
    assert_eq!(rig.endings, vec![Ending::Return]);
    assert_eq!(rig.text, "Rome");
    assert!(rig.rect(&format!("{FIELD}:cut")).is_none(), "the menu closed");
}

/// Esc with the menu open closes only the menu, as in native menus: the edit goes on, and a
/// second Esc gives back the text.
#[test]
fn escape_with_the_menu_open_closes_only_the_menu() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    rig.click(FIELD, PointerButton::Secondary);
    rig.key(Key::Escape, Modifiers::NONE);
    assert!(rig.rect(&format!("{FIELD}:cut")).is_none(), "the menu closed");
    assert_eq!((rig.text.as_str(), rig.endings.clone()), ("Rome", vec![]), "the edit goes on");
    assert!(rig.focused());
    rig.key(Key::Escape, Modifiers::NONE);
    assert_eq!((rig.text.as_str(), rig.endings.clone()), ("Lisbon", vec![Ending::Escape]));
}

/// Esc after Tab took the focus into the open menu also closes only the menu.
#[test]
fn escape_from_inside_the_menu_closes_only_the_menu() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    rig.click(FIELD, PointerButton::Secondary);
    rig.key(Key::Tab, Modifiers::NONE);
    rig.key(Key::Escape, Modifiers::NONE);
    assert_eq!((rig.text.as_str(), rig.endings.clone()), ("Rome", vec![]), "the edit goes on");
    assert!(rig.focused());
}

/// Clicking elsewhere ends the edit and keeps the text.
#[test]
fn clicking_away_ends_the_edit() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    rig.click_at(egui::pos2(300.0, 250.0), PointerButton::Primary);
    assert_eq!(rig.text, "Rome");
    assert_eq!(rig.endings, vec![Ending::Away]);
}

/// Opening the menu and closing it without a choice (a click outside) leaves the field.
#[test]
fn dismissing_the_menu_leaves_the_field() {
    let mut rig = Rig::new("Lisbon");
    rig.click(FIELD, PointerButton::Primary);
    rig.click(FIELD, PointerButton::Secondary);
    rig.click_at(egui::pos2(300.0, 250.0), PointerButton::Primary);
    assert!(!rig.focused());
    assert_eq!(rig.endings, vec![Ending::Away]);
}

/// A field that selects on focus: clicking into it selects everything, so typing replaces it.
#[test]
fn a_field_that_selects_on_focus_is_retyped_whole() {
    let mut rig = Rig::new("Lisbon");
    rig.select_on_focus = true;
    rig.click(FIELD, PointerButton::Primary);
    rig.type_text("Kyoto");
    assert_eq!(rig.text, "Kyoto");
}

/// A field focused by the pass before (a dialog asks for focus after drawing it) selects its
/// text before egui handles the first keystroke of the pass the focus takes effect, so that
/// keystroke replaces the text instead of being inserted into it.
#[test]
fn text_typed_on_the_first_focused_pass_replaces_the_whole_text() {
    let mut rig = Rig::new("LightCraft Library");
    rig.select_on_focus = true;
    // the dialog draws the field, then asks for its focus: effective on the next pass
    rig.frame_with(vec![], true);
    assert!(rig.view.ctx.memory(|m| m.has_focus(id(FIELD))), "focused for the next pass");
    // the pass the focus takes effect already carries the text
    rig.frame(vec![Event::Text("Fresh".into())]);
    assert_eq!(rig.text, "Fresh", "the first keystroke replaced the selected text");
    // the text from before the edit is still what Esc gives back
    rig.key(Key::Escape, Modifiers::NONE);
    assert_eq!(rig.text, "LightCraft Library");
    assert_eq!(rig.endings, vec![Ending::Escape]);
}

/// Esc after using the menu still gives back the text from before the whole edit.
#[test]
fn escape_after_the_menu_gives_back_the_original_text() {
    let mut rig = Rig::new("Lisbon");
    rig.view.clipboard = "Porto".into();
    rig.select_all_by_keys();
    rig.menu("paste");
    assert_eq!(rig.text, "Porto");
    rig.key(Key::Escape, Modifiers::NONE);
    assert_eq!(rig.text, "Lisbon");
}

/// A field that goes away in the middle of an edit (its dialog closed) starts afresh when it
/// comes back: Esc then gives back the text it came back with.
#[test]
fn an_edit_cut_short_is_forgotten() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    rig.shown = false;
    rig.settle();
    rig.text = "Porto".into();
    rig.shown = true;
    rig.settle();
    rig.select_all_by_keys();
    rig.type_text("Kyoto");
    rig.key(Key::Escape, Modifiers::NONE);
    assert_eq!(rig.text, "Porto");
}

/// Switching to another app and back in the middle of an edit carries on with it: the field
/// is still being edited meanwhile, and Esc still gives back the text from before.
#[test]
fn switching_apps_carries_on_with_the_edit() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    rig.window_focused = false;
    rig.settle();
    assert!(rig.editing, "still being edited while the app is in the background");
    assert_eq!(rig.endings, vec![]);
    rig.window_focused = true;
    rig.settle();
    rig.key(Key::Escape, Modifiers::NONE);
    assert_eq!((rig.text.as_str(), rig.endings.clone()), ("Lisbon", vec![Ending::Escape]));
}

/// A field that goes away while its menu is open starts afresh too when it comes back.
#[test]
fn a_field_gone_with_its_menu_open_starts_afresh() {
    let mut rig = Rig::new("Lisbon");
    rig.select_all_by_keys();
    rig.type_text("Rome");
    rig.click(FIELD, PointerButton::Secondary);
    // a click on the greyed-out Cut takes the focus into the menu
    rig.click(&format!("{FIELD}:cut"), PointerButton::Primary);
    rig.shown = false;
    rig.settle();
    rig.text = "Porto".into();
    rig.shown = true;
    rig.settle();
    assert!(!rig.editing, "no edit under way");
    assert_eq!(rig.endings, vec![], "and none ended: it was cut short");
    rig.select_all_by_keys();
    rig.type_text("Kyoto");
    rig.key(Key::Escape, Modifiers::NONE);
    assert_eq!(rig.text, "Porto");
}

/// A field that selects on focus still lets a drag into it select part of the text, as a
/// browser's address bar does: only a plain click selects everything.
#[test]
fn a_drag_into_a_field_that_selects_on_focus_selects_what_it_covers() {
    let mut rig = Rig::new("Lisbon Portugal Europe Atlantic Ocean");
    rig.select_on_focus = true;
    let r = rig.rect(FIELD).expect("the field");
    let (from, to) = (r.center(), egui::pos2(r.right() + 40.0, r.center().y));
    let press = |at, pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    rig.frame(vec![Event::PointerMoved(from), press(from, true)]);
    for i in 1..=5 {
        rig.frame(vec![Event::PointerMoved(from + (to - from) * (i as f32 / 5.0))]);
    }
    rig.frame(vec![press(to, false)]);
    rig.settle();
    rig.type_text("X");
    assert!(rig.text.starts_with("Lisbon") && rig.text.ends_with('X') && rig.text != "X", "{:?}", rig.text);
}
