//! A text field that behaves the same everywhere in the app: right-click for Cut, Copy, Paste and
//! Select All; Esc gives back the text from before the edit; Return or leaving the field ends the
//! edit; optionally everything is selected when it takes focus, for fields that are retyped whole.
//!
//! It knows nothing of the app's documents or commands, only the UI's plumbing (the automation
//! registry, translations, shortcut labels). The caller owns the text and learns how an edit
//! ended from [`TextFieldResponse::ending`]; what an ending means (save, apply, close) is the
//! caller's. Each field's widget id is unique on screen: its state is keyed by it.
//!
//! An edit ends exactly once, and every way of leaving it except Esc saves, including the field no
//! longer being drawn (egui reports a lost focus only to a widget that is drawn). A field that
//! saves when its edit ends declares the save with [`TextField::saves_with`]; the app runs what is
//! left recorded when a pass ends, and when the window closes (`LightcraftApp::finish_ui_pass`,
//! `LightcraftApp::save_unfinished_edits`).
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

/// The command an edit saves when it ends, as a field declares it ([`TextField::saves_with`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Save {
    pub command: &'static str,
    pub params: serde_json::Value,
}

pub struct TextFieldResponse {
    pub response: Response,
    /// Set on the frame an edit ends.
    pub ending: Option<Ending>,
    /// An edit is under way: the field has the focus, or its menu does.
    pub editing: bool,
    /// What the field declared with [`TextField::saves_with`], for the frame the edit ended with
    /// the text kept (Return or leaving the field): run it, rather than deciding again from
    /// `committed()`. `None` on every other frame, and when the text needs no save.
    pub save: Option<Save>,
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
    label: Option<String>,
    width: Option<f32>,
    select_on_focus: bool,
    begin_edit: bool,
    saves: Option<SaveWith<'a>>,
    target: u64,
    frame: bool,
    font: Option<egui::FontId>,
    color: Option<egui::Color32>,
    align: Option<egui::Align>,
    margin: Option<egui::Margin>,
    rows: Option<usize>,
}

/// The egui id of the field registered as `widget`, to give it the focus (⌘F to search).
pub fn id(widget: &str) -> egui::Id {
    egui::Id::unique("lc-text-field").with(widget)
}

/// Whether the field had an unfinished edit on the immediately preceding egui pass.
pub(crate) fn was_editing(ctx: &egui::Context, widget: &str) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<Memo>(id(widget).with("memo")).is_some_and(|memo| memo.before.is_some() && memo.pass.checked_add(1) == Some(pass)))
}

/// A field's declaration of its save ([`TextField::saves_with`]).
type SaveWith<'a> = Box<dyn FnOnce(&str) -> Option<Save> + 'a>;

/// The saves of edits under way, in the order they were recorded: the field's id, what its text is
/// for ([`TextField::saves_with`]), the pass it was last drawn in, and what it saves.
#[derive(Clone, Default)]
struct Unfinished(Vec<(egui::Id, u64, u64, Save)>);

/// The save of an edit taken from the record to be run ([`take_left_behind`], [`take_all`]): once it
/// succeeds, [`end_edits`] tells the field its edit is over.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pending {
    pub(crate) id: egui::Id,
    pub(crate) save: Save,
}

/// The edits of `done` (the fields of saves that ran and succeeded) are over: each field, if drawn
/// again showing the text its save was made from, ends its edit there, so Esc has nothing left to
/// give up and leaving does not save a second time. Text typed since stays an edit under way.
pub(crate) fn end_edits(ctx: &egui::Context, done: impl IntoIterator<Item = egui::Id>) {
    for id in done {
        let key = id.with("memo");
        if let Some(mut memo) = ctx.data(|d| d.get_temp::<Memo>(key)) {
            memo.flushed = true;
            ctx.data_mut(|d| d.insert_temp(key, memo));
        }
    }
}

fn unfinished_id() -> egui::Id {
    egui::Id::unique("lc-text-field-unfinished")
}

/// A key for what a field's text is for ([`TextField::saves_with`]).
pub(crate) fn target_key(target: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    target.hash(&mut hasher);
    hasher.finish()
}

/// Record (or, with `None`, forget) the save of the field `id` for `target`, drawn in `pass`. Only
/// an entry from the pass just before is forgotten: an older one means the field was not drawn
/// since (a pass egui discarded, a panel switched away), so it is for the sweep to run, not for
/// the field to drop. An entry for another target of the same field is left alone: that edit was
/// for something else, and it is the sweep's.
fn record(ctx: &egui::Context, id: egui::Id, target: u64, pass: u64, save: Option<Save>) {
    ctx.data_mut(|d| {
        let list = &mut d.get_temp_mut_or_default::<Unfinished>(unfinished_id()).0;
        match (list.iter().position(|(other, t, ..)| *other == id && *t == target), save) {
            (Some(at), Some(save)) => list[at] = (id, target, pass, save),
            (None, Some(save)) => list.push((id, target, pass, save)),
            (Some(at), None) => {
                if list[at].2.checked_add(1) == Some(pass) {
                    list.remove(at);
                }
            }
            (None, None) => {}
        }
    });
}

/// Take the saves of the edits whose field was not drawn in the pass that is ending, or was drawn
/// for another target: each went away mid-edit, and what is taken is no longer recorded.
pub(crate) fn take_left_behind(ctx: &egui::Context) -> Vec<Pending> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let list = &mut d.get_temp_mut_or_default::<Unfinished>(unfinished_id()).0;
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(list).into_iter().partition(|(_, _, at, _)| *at != pass);
        *list = kept;
        gone.into_iter().map(|(id, _, _, save)| Pending { id, save }).collect()
    })
}

/// Whether any edit with a declared save is under way (or was left behind and not swept yet).
pub(crate) fn any_unfinished(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Unfinished>(unfinished_id()).is_some_and(|u| !u.0.is_empty()))
}

/// Take every save of an edit under way, drawn or not (the window is closing).
pub(crate) fn take_all(ctx: &egui::Context) -> Vec<Pending> {
    ctx.data_mut(|d| {
        let list = std::mem::take(&mut d.get_temp_mut_or_default::<Unfinished>(unfinished_id()).0);
        list.into_iter().map(|(id, _, _, save)| Pending { id, save }).collect()
    })
}

/// What a box shows while its edit is under way: the text typed, kept between frames for the
/// `target` it was typed for (the library, the photo, the overlay). Another target is another edit:
/// the box then shows `value`, as when no edit is under way.
pub(crate) fn draft(ui: &Ui, key: impl std::hash::Hash + std::fmt::Debug, target: &impl std::hash::Hash, value: &str) -> String {
    let at = target_key(target);
    let draft = ui.data(|d| d.get_temp::<(u64, String)>(egui::Id::unique(("lc-text-field-draft", &key))));
    draft.filter(|(t, _)| *t == at).map_or_else(|| value.to_owned(), |(_, text)| text)
}

/// Keep the text of an edit under way as the [`draft`] for `target` (or, once no edit is under
/// way, forget it).
pub(crate) fn keep_draft(ui: &Ui, key: impl std::hash::Hash + std::fmt::Debug, target: &impl std::hash::Hash, editing: bool, text: &str) {
    let id = egui::Id::unique(("lc-text-field-draft", &key));
    if editing {
        ui.data_mut(|d| d.insert_temp(id, (target_key(target), text.to_owned())));
    } else {
        ui.data_mut(|d| d.remove::<(u64, String)>(id));
    }
}

impl<'a> TextField<'a> {
    /// A one-line field (Return ends the edit) registered as `widget`.
    pub fn singleline(widget: &'a str, text: &'a mut String) -> Self {
        TextField {
            widget,
            text,
            multiline: false,
            hint: None,
            label: None,
            width: None,
            select_on_focus: false,
            begin_edit: false,
            saves: None,
            target: 0,
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

    /// Declare what the field saves when its edit ends, from the text then: `None` when there is
    /// nothing to save (unchanged, or the text is not meant). The rule: an edit ends exactly once,
    /// and every way of leaving it except Esc saves, including the field no longer being drawn
    /// (a tab, panel or dialog that replaces it mid-edit, a command, Quit). While the edit is
    /// under way the save is recorded; the frame the edit ends in the field shows it as
    /// [`TextFieldResponse::save`] (and forgets it), and one that is still recorded when the field
    /// was not drawn is run by [`take_left_behind`] / [`take_all`]. Leave it off where Return
    /// means OK and Cancel must not save (a dialog's own fields). A save the sweep runs after the
    /// view changed must not depend on the view: its command is `always` enabled or it saves to
    /// the ids captured when the edit started; a new opt-in keeps to that. A save that ran from
    /// outside the field (the window's close) ends the edit of a field still drawn ([`end_edits`]).
    ///
    /// `target` is what the text is for: the library, the photos, the overlay, anything that
    /// makes the same text another edit when it differs. A field drawn for another target than
    /// its edit under way ends that edit there; the pass did not record it again, so the sweep
    /// runs its save, made for the first target, and the field starts afresh on the new one. The
    /// save names its target too (ids, an index with the text it had), and the engine refuses one
    /// that is gone or changed rather than write to the wrong one or bring it back.
    pub fn saves_with(mut self, target: impl std::hash::Hash, save: impl FnOnce(&str) -> Option<Save> + 'a) -> Self {
        self.target = target_key(&target);
        self.saves = Some(Box::new(save));
        self
    }

    /// Grey text shown while the field is empty.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The name a screen reader announces for the field (its visible caption); the hint
    /// stands in when there is none.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
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

    /// Initialize a newly revealed value editor before its first input is processed.
    pub(crate) fn begin_edit(mut self, on: bool) -> Self {
        self.begin_edit = on;
        self
    }

    /// The field's remembered state at the start of a pass: a field not drawn on the previous pass starts afresh,
    /// a begun edit or a choice made from the menu last frame is carried out, and a field focused by the previous
    /// pass selects its text when it asks to.
    fn prepare(&self, ui: &mut Ui, id: egui::Id) -> Memo {
        let mut memo: Memo = ui.data_mut(|d| d.get_temp(id.with("memo"))).unwrap_or_default();
        // a field not drawn last frame (its dialog or panel closed) starts afresh: an edit, a menu
        // or a choice from it that was under way is gone with it
        let pass = ui.ctx().cumulative_pass_nr();
        if memo.pass.checked_add(1) != Some(pass) {
            memo = Memo::default();
        }
        memo.pass = pass;
        if self.begin_edit {
            memo.before = Some(self.text.clone());
            memo.select_on_release = false;
            ui.memory_mut(|m| m.request_focus(id));
            select_all(ui.ctx(), id, self.text);
        }
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
        // A field focused by the previous pass (a dialog asks after drawing it) must already
        // select its text on the pass that carries the first keystroke: egui processes text
        // inside `ui.add`, before the after-add branch below could select, leaving the new text
        // inserted into the old. A press may go on to drag a selection, so it keeps the
        // after-add path; an edit already under way keeps its pre-edit text for Esc.
        let focused_at_start = ui.memory(|m| m.has_focus(id));
        if focused_at_start && memo.before.is_none() && self.select_on_focus && !ui.input(|i| i.pointer.any_down()) {
            memo.before = Some(self.text.clone());
            select_all(ui.ctx(), id, self.text);
        }
        memo
    }

    /// Draw the field and return its response, any ending this frame, and whether an edit is
    /// under way. Per-frame state keeps the pre-edit text for Esc, the selection across the
    /// context menu, and select-on-focus; single-line Enter ends the edit, and focus loss ends it
    /// except when the focus went to the field's own menu (a choice from the menu resumes the
    /// edit). The menu offers cut/copy/paste/select-all (paste is omitted on the web; egui also
    /// handles their keyboard shortcuts), and a field not drawn on the previous pass starts clean.
    pub fn show(self, ui: &mut Ui) -> TextFieldResponse {
        let id = id(self.widget);
        let pass = ui.ctx().cumulative_pass_nr();
        let mut memo = self.prepare(ui, id);
        let name = self.label.clone().or_else(|| self.hint.clone());
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
            edit = edit.align(egui::Align2([align, egui::Align::Min]));
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
        // screen readers: the field by its caption, even once it holds text (egui names it by
        // nothing but its contents)
        if let Some(name) = name.filter(|n| !n.trim().is_empty()) {
            ui.ctx().accesskit_node_builder(response.id, |node| node.set_label(name));
        }
        // The menu may take focus on a primary-button release. egui collapses a
        // TextEdit selection when focus leaves; retain the range our own menu acts on.
        if right && response.contains_pointer() || memo.menu_open && !ui.memory(|m| m.has_focus(id)) {
            let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            state.cursor.set_char_range(selection);
            state.store(ui.ctx(), id);
        }
        crate::widgets::register(ui, self.widget, response.rect);
        // egui's own focus: `Response::has_focus` is also false while the window is in the
        // background (another app in front), which neither ends nor restarts an edit
        let focused = ui.memory(|m| m.has_focus(id));
        // The opening press may place a cursor; keep the original selected, never replacement text.
        if self.begin_edit && focused && !response.changed() {
            select_all(ui.ctx(), id, self.text);
        }

        // an edit that is over before this frame (its save ran while the field stayed drawn: the window's
        // close, a library switch that did not happen) or was for another target (the field now shows
        // another photo's text) ends here, before anything below can end it a second time; the sweep
        // runs the save of one for another target, which the pass did not record again
        let ended_here = memo.end_if_over(self.text, pass, focused, self.saves.is_some().then_some(self.target));
        track_edit_start(&mut memo, ui, id, self.text, self.select_on_focus, focused, response.lost_focus());
        let menu_was_open = memo.menu_open;
        // a click on a greyed-out item does nothing, as in a native menu: only a choice or a click
        // outside closes it (not the right-click that moves the menu within the field, #1877)
        memo.menu_open = egui::Popup::context_menu(&response)
            .close_behavior(crate::widgets::context_menu_close(&response, egui::PopupCloseBehavior::CloseOnClickOutside))
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
        let editing = !ended_here && ending.is_none() && (focused || memo.away || memo.menu_open || memo.action.is_some());
        // the save the field declares from its text now: recorded while the edit is under way (to
        // be run if the field is not drawn again), handed over once, with the text kept, when it ends
        let kept = matches!(ending, Some(Ending::Return | Ending::Away));
        let declared = self.saves.filter(|_| editing || kept).and_then(|saves| saves(self.text));
        memo.recorded = recorded_text(declared.as_ref(), editing, self.text);
        ui.data_mut(|d| d.insert_temp(id.with("memo"), memo));
        let save = if kept { declared.clone() } else { None };
        record(ui.ctx(), id, self.target, pass, declared.filter(|_| editing));
        TextFieldResponse { response, ending, editing, save }
    }
}

/// An edit not under way (none, or one cut short when the field went away) is forgotten; the start of an edit
/// (not the focus coming back from the menu) keeps the text for Esc and selects it when the field asks to.
fn track_edit_start(memo: &mut Memo, ui: &Ui, id: egui::Id, text: &str, select_on_focus: bool, focused: bool, lost_focus: bool) {
    let in_menu = memo.away || memo.menu_open || memo.action.is_some();
    if !focused && !lost_focus && !in_menu {
        memo.before = None;
    }
    let pressing = ui.input(|i| i.pointer.any_down());
    if focused && memo.before.is_none() {
        memo.before = Some(text.to_owned());
        if select_on_focus {
            // a press that took the focus may go on to drag a selection: wait for its release
            if pressing {
                memo.select_on_release = true;
            } else {
                select_all(ui.ctx(), id, text);
            }
        }
    }
    if memo.select_on_release && !pressing {
        memo.select_on_release = false;
        // a plain click selects everything; a drag keeps what it selected
        if focused && !has_selection(ui.ctx(), id) {
            select_all(ui.ctx(), id, text);
        }
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
    /// The text its declared save was made from on the last frame of an edit under way.
    recorded: Option<String>,
    /// The save ran from outside the field ([`end_edits`]): the edit ends on the next frame that
    /// shows `recorded` again.
    flushed: bool,
    /// What the text was for, the last frame it was drawn ([`TextField::saves_with`]).
    target: Option<u64>,
}

impl Memo {
    /// Whether the edit under way is over before this frame: its save ran from outside and the field
    /// still shows the text that was saved ([`end_edits`]), or the field is now for another
    /// `target` (another photo, library, overlay) than the edit was. The memo then starts afresh.
    /// A field that keeps the focus goes on from the text it shows as a new edit (Esc gives back
    /// that text; it is not a new focus, so nothing is selected).
    fn end_if_over(&mut self, text: &str, pass: u64, focused: bool, target: Option<u64>) -> bool {
        let saved = std::mem::take(&mut self.flushed) && self.recorded.as_deref() == Some(text);
        let moved = self.before.is_some() && self.target.is_some() && target.is_some() && self.target != target;
        if saved || moved {
            *self = Memo { pass, before: focused.then(|| text.to_owned()), target, ..Memo::default() };
        }
        self.target = target;
        saved || moved
    }
}

/// The text to remember for [`Memo::recorded`]: the field's, while an edit with a declared save is under way.
fn recorded_text(declared: Option<&Save>, editing: bool, text: &str) -> Option<String> {
    declared.filter(|_| editing).map(|_| text.to_owned())
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
        crate::widgets::register(ui, format!("{widget}:{key}"), r.rect);
        if r.clicked() {
            *action = Some(a);
            ui.close();
        }
    }
}

#[cfg(test)]
mod tests;
