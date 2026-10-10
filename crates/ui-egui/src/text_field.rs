//! A text field that behaves the same everywhere in the app: right-click for Cut, Copy, Paste and
//! Select All; Esc gives back the text from before the edit; Return or leaving the field ends the
//! edit; optionally everything is selected when it takes focus, for fields that are retyped whole.
//!
//! It knows nothing of the app's documents or commands, only the UI's plumbing (the automation
//! registry, translations, shortcut labels). The caller owns the text and learns how an edit
//! ended from [`TextFieldResponse::ending`]; what an ending means (save, apply, close) is the
//! caller's. Each field's widget id is unique on screen: its state is keyed by it.
//!
//! New text boxes use it rather than `egui::TextEdit`.
//!
//! The keyboard's ⌘X / ⌘C / ⌘V / ⌘A and undo are egui's own, and the app's shortcuts step aside
//! while a field has focus (`shortcuts::handle`, the native menu's `yields_to_text`).

use egui::{Response, Ui};

/// How an edit ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// Return in a one-line field: the text is meant.
    Return,
    /// The focus went elsewhere (a click outside, Tab): the text is kept.
    Away,
    /// Esc: the text is back to what it was before the edit.
    Escape,
}

pub struct TextFieldResponse {
    pub response: Response,
    /// Set on the frame an edit ends.
    pub ending: Option<Ending>,
    /// An edit is under way: the field has the focus, or its menu does.
    pub editing: bool,
}

impl TextFieldResponse {
    /// The edit ended with the text kept (Return or leaving the field).
    pub fn committed(&self) -> bool {
        matches!(self.ending, Some(Ending::Return | Ending::Away))
    }

    /// Esc ended the edit and the text is back as it was.
    pub fn cancelled(&self) -> bool {
        self.ending == Some(Ending::Escape)
    }
}

/// A one-line or multi-line text field, registered under a widget id for the control channel.
pub struct TextField<'a> {
    widget: &'a str,
    text: &'a mut String,
    multiline: bool,
    hint: Option<String>,
    width: Option<f32>,
    select_on_focus: bool,
    frame: bool,
    font: Option<egui::FontId>,
    color: Option<egui::Color32>,
    align: Option<egui::Align>,
    margin: Option<egui::Margin>,
    rows: Option<usize>,
}

/// The egui id of the field registered as `widget`, to give it the focus (⌘F to search).
pub fn id(widget: &str) -> egui::Id {
    egui::Id::new("lc-text-field").with(widget)
}

impl<'a> TextField<'a> {
    /// A one-line field (Return ends the edit) registered as `widget`.
    pub fn singleline(widget: &'a str, text: &'a mut String) -> Self {
        TextField {
            widget,
            text,
            multiline: false,
            hint: None,
            width: None,
            select_on_focus: false,
            frame: true,
            font: None,
            color: None,
            align: None,
            margin: None,
            rows: None,
        }
    }

    /// A multi-line field: Return starts a new line; leaving the field ends the edit.
    pub fn multiline(widget: &'a str, text: &'a mut String) -> Self {
        TextField { multiline: true, ..Self::singleline(widget, text) }
    }

    /// Grey text shown while the field is empty.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// How many lines a multi-line field shows.
    pub fn rows(mut self, rows: usize) -> Self {
        self.rows = Some(rows);
        self
    }

    /// Draw the field's own box (on by default); off where the caller draws one around it.
    pub fn frame(mut self, on: bool) -> Self {
        self.frame = on;
        self
    }

    pub fn font(mut self, font: egui::FontId) -> Self {
        self.font = Some(font);
        self
    }

    pub fn text_color(mut self, color: egui::Color32) -> Self {
        self.color = Some(color);
        self
    }

    pub fn horizontal_align(mut self, align: egui::Align) -> Self {
        self.align = Some(align);
        self
    }

    pub fn margin(mut self, margin: egui::Margin) -> Self {
        self.margin = Some(margin);
        self
    }

    /// Select all the text when the field takes focus, so typing replaces it (names, numbers).
    pub fn select_on_focus(mut self, on: bool) -> Self {
        self.select_on_focus = on;
        self
    }

    pub fn show(self, ui: &mut Ui) -> TextFieldResponse {
        let id = id(self.widget);
        let mut memo: Memo = ui.data_mut(|d| d.get_temp(id.with("memo"))).unwrap_or_default();
        // a field not drawn last frame (its dialog or panel closed) starts afresh: an edit, a menu
        // or a choice from it that was under way is gone with it
        let pass = ui.ctx().cumulative_pass_nr();
        if memo.pass.checked_add(1) != Some(pass) {
            memo = Memo::default();
        }
        memo.pass = pass;
        // a choice from the menu last frame: the field takes the focus back and does it, with
        // egui's own handling of the clipboard events (a password is never copied)
        if let Some(action) = memo.action.take() {
            ui.memory_mut(|m| m.request_focus(id));
            match action {
                Action::Cut => ui.input_mut(|i| i.events.push(egui::Event::Cut)),
                Action::Copy => ui.input_mut(|i| i.events.push(egui::Event::Copy)),
                // only the host can read the clipboard: it answers with a paste event
                Action::Paste => ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste),
                Action::SelectAll => select_all(ui.ctx(), id, self.text),
                Action::Refocus => {}
            }
        }
        let mut edit = if self.multiline { egui::TextEdit::multiline(self.text) } else { egui::TextEdit::singleline(self.text) };
        edit = edit.id(id);
        if let Some(hint) = self.hint {
            edit = edit.hint_text(hint);
        }
        if let Some(w) = self.width {
            edit = edit.desired_width(w);
        }
        if !self.frame {
            edit = edit.frame(egui::Frame::NONE);
        }
        if let Some(font) = self.font {
            edit = edit.font(font);
        }
        if let Some(color) = self.color {
            edit = edit.text_color(color);
        }
        if let Some(align) = self.align {
            edit = edit.horizontal_align(align);
        }
        if let Some(margin) = self.margin {
            edit = edit.margin(margin);
        }
        if let Some(rows) = self.rows {
            edit = edit.desired_rows(rows);
        }
        let (escape, enter) = ui.input(|i| (i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Enter)));
        // a right-click places the cursor as a left one does: the selection the menu acts on
        // would be gone
        let right = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary) || i.pointer.button_released(egui::PointerButton::Secondary));
        let selection = egui::text_edit::TextEditState::load(ui.ctx(), id).and_then(|s| s.cursor.char_range());
        let mut response = ui.add(edit);
        if right && response.contains_pointer() {
            let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            state.cursor.set_char_range(selection);
            state.store(ui.ctx(), id);
        }
        crate::widgets::register(ui.ctx(), self.widget, response.rect);
        // egui's own focus: `Response::has_focus` is also false while the window is in the
        // background (another app in front), which neither ends nor restarts an edit
        let focused = ui.memory(|m| m.has_focus(id));

        // an edit not under way (none, or one cut short when the field went away) is forgotten
        let in_menu = memo.away || memo.menu_open || memo.action.is_some();
        if !focused && !response.lost_focus() && !in_menu {
            memo.before = None;
        }
        // the start of an edit (not the focus coming back from the menu)
        let pressing = ui.input(|i| i.pointer.any_down());
        if focused && memo.before.is_none() {
            memo.before = Some(self.text.clone());
            if self.select_on_focus {
                // a press that took the focus may go on to drag a selection: wait for its release
                if pressing {
                    memo.select_on_release = true;
                } else {
                    select_all(ui.ctx(), id, self.text);
                }
            }
        }
        if memo.select_on_release && !pressing {
            memo.select_on_release = false;
            // a plain click selects everything; a drag keeps what it selected
            if focused && !has_selection(ui.ctx(), id) {
                select_all(ui.ctx(), id, self.text);
            }
        }
        let menu_was_open = memo.menu_open;
        // a click on a greyed-out item does nothing, as in a native menu: only a choice or a click
        // outside closes it
        memo.menu_open = egui::Popup::context_menu(&response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| menu(ui, self.widget, id, self.text, &mut memo.action))
            .is_some();

        let mut ending = None;
        let close_menu = |ui: &Ui| egui::Popup::close_id(ui.ctx(), egui::Popup::default_response_id(&response));
        // egui reports a lost focus on two frames: only an edit under way ends
        if response.lost_focus() && memo.before.is_some() {
            if escape && menu_was_open {
                // Esc closes only the menu, as in native menus: the field takes the focus back
                memo.action = Some(Action::Refocus);
            } else if escape {
                if let Some(before) = memo.before.take()
                    && *self.text != before
                {
                    *self.text = before;
                    response.mark_changed();
                }
                ending = Some(Ending::Escape);
            } else if enter && !self.multiline {
                ending = Some(Ending::Return);
                close_menu(ui);
            } else if menu_was_open || memo.menu_open || memo.action.is_some() {
                // the click went to the menu: the edit goes on once the menu is done with
                memo.away = true;
            } else {
                ending = Some(Ending::Away);
            }
        } else if memo.away && escape && memo.action.is_none() {
            // Esc with the focus in the menu (Tab took it there) closes only the menu
            memo.action = Some(Action::Refocus);
            memo.away = false;
        } else if memo.away && !memo.menu_open && memo.action.is_none() && !focused {
            // the menu closed without a choice: the click outside it left the field
            ending = Some(Ending::Away);
        }
        if focused {
            memo.away = false;
        }
        if ending.is_some() {
            memo = Memo { pass, ..Memo::default() };
        }
        let editing = ending.is_none() && (focused || memo.away || memo.menu_open || memo.action.is_some());
        ui.data_mut(|d| d.insert_temp(id.with("memo"), memo));
        TextFieldResponse { response, ending, editing }
    }
}

/// What a field remembers between frames.
#[derive(Clone, Default)]
struct Memo {
    /// The text when the edit started, for Esc.
    before: Option<String>,
    /// The context menu was open at the end of the frame.
    menu_open: bool,
    /// The menu's choice, done next frame.
    action: Option<Action>,
    /// The focus left for the menu: the edit ends if the menu closes without a choice.
    away: bool,
    /// The field took the focus with select-on-focus under a press: select all on its release,
    /// unless it dragged a selection.
    select_on_release: bool,
    /// The frame (egui pass) the field was last drawn in.
    pass: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Cut,
    Copy,
    Paste,
    SelectAll,
    /// Only take the focus back (Esc closed the menu).
    Refocus,
}

fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    let end = egui::text::CCursor::new(text.chars().count());
    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
    state.store(ctx, id);
}

/// Some text is selected in the field.
fn has_selection(ctx: &egui::Context, id: egui::Id) -> bool {
    egui::text_edit::TextEditState::load(ctx, id).and_then(|s| s.cursor.char_range()).is_some_and(|r| !r.is_empty())
}

/// The field's context menu: Cut, Copy, Paste, Select All, with the keyboard's shortcuts beside them.
fn menu(ui: &mut Ui, widget: &str, id: egui::Id, text: &str, action: &mut Option<Action>) {
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    let selected = has_selection(ui.ctx(), id);
    let items = [
        (Action::Cut, "cut", "Cut", "Cmd+X", selected),
        (Action::Copy, "copy", "Copy", "Cmd+C", selected),
        // a browser only pastes from its own paste event, never on request
        (Action::Paste, "paste", "Paste", "Cmd+V", !cfg!(target_arch = "wasm32")),
        (Action::SelectAll, "selectAll", "Select All", "Cmd+A", !text.is_empty()),
    ];
    for (a, key, label, shortcut, enabled) in items {
        if a == Action::Paste && cfg!(target_arch = "wasm32") {
            continue;
        }
        if a == Action::SelectAll {
            ui.separator();
        }
        let button = egui::Button::new(crate::i18n::tr(label)).shortcut_text(crate::menubar::shortcut_text(shortcut, mac));
        let r = ui.add_enabled(enabled, button);
        crate::widgets::register(ui.ctx(), format!("{widget}:{key}"), r.rect);
        if r.clicked() {
            *action = Some(a);
            ui.close();
        }
    }
}

#[cfg(test)]
mod tests {
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
}
