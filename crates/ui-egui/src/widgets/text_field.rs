//! A text field that behaves the same everywhere in the app, drawn by the framework's `TextField`: the
//! framework owns typing, selection, its Cut / Copy / Paste / Select All menu, focus and Tab. Esc gives back
//! the text from before the edit; Return (one-line fields) or leaving the field ends the edit; optionally
//! everything is selected when it takes focus, for fields that are retyped whole.
//!
//! It knows nothing of the app's documents or commands, only the UI's plumbing (the automation
//! registry, translations). The caller owns the text and learns how an edit ended from
//! [`TextFieldResponse::ending`]; what an ending means (save, apply, close) is the caller's. Each
//! field's widget id is unique on screen: its state is keyed by it.
//!
//! New text boxes use it rather than `egui::TextEdit`.

use craft::ui::contract::{Bind, Props as _, Show as _, Sizing};
use craft::ui::runtime::focus::FocusTarget;
use egui::{Rect, Response, Ui};

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
    /// An edit is under way: the field has the focus.
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
    label: Option<String>,
    width: Option<f32>,
    select_on_focus: bool,
    begin_edit: bool,
    rows: Option<usize>,
}

/// The egui id of the field registered as `widget`, to give it the focus (⌘F to search).
pub fn id(widget: &str) -> egui::Id {
    egui::Id::unique("lc-text-field").with(widget)
}

/// Whether the keyboard focus is on something that types: a framework text part or shortcut recorder, or an app's own
/// egui text widget outside the framework. A focused framework button, slider or list does not count: the runtime
/// already keeps the keys it uses, so the app's shortcuts still get the rest.
pub(crate) fn typing(ctx: &egui::Context) -> bool {
    match craft::ui::runtime::focus::focused_rec(ctx) {
        // (a part of the stop, such as a number field's text part, holding egui's focus is typing too)
        Some(rec) => {
            rec.stop.text
                || craft::ui::runtime::focus::takes_letters(&rec.stop.keys)
                || ctx.memory(|m| m.focused()).is_some_and(|focused| focused != rec.stop.root && focused != rec.stop.id)
        }
        None => ctx.egui_wants_keyboard_input(),
    }
}

/// Whether the field registered as `widget` has the keyboard focus now.
pub(crate) fn focused(ctx: &egui::Context, widget: &str) -> bool {
    has_focus(ctx, id(widget))
}

/// Gives the field registered as `widget` the keyboard focus.
pub(crate) fn focus(ctx: &egui::Context, widget: &str) {
    focus_id(ctx, id(widget));
}

/// Gives the field whose id is `id` ([`id`]) the keyboard focus.
pub(crate) fn focus_id(ctx: &egui::Context, id: egui::Id) {
    craft::ui::runtime::focus::request(ctx, FocusTarget::Id(id));
}

/// Takes the keyboard focus from the field registered as `widget`, if it has it.
pub(crate) fn blur(ctx: &egui::Context, widget: &str) {
    blur_id(ctx, id(widget));
}

/// Takes the keyboard focus from the field whose id is `id`, if it has it.
pub(crate) fn blur_id(ctx: &egui::Context, id: egui::Id) {
    if let Some(rec) = craft::ui::runtime::focus::focused_rec(ctx).filter(|rec| rec.stop.id == id || rec.stop.root == id) {
        ctx.memory_mut(|m| m.surrender_focus(rec.stop.root));
        if let Some(now) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(now));
        }
    }
}

/// Whether the field whose id is `id` has the keyboard focus.
pub(crate) fn has_focus_id(ctx: &egui::Context, id: egui::Id) -> bool {
    has_focus(ctx, id)
}

/// The egui id of the field's text part, which holds the keyboard focus while the field is edited.
pub(crate) fn text_id(id: egui::Id) -> egui::Id {
    craft::ui::runtime::memory::sub(id, ("text", craft::behavior::PartKey::new("text")))
}

fn has_focus(ctx: &egui::Context, id: egui::Id) -> bool {
    craft::ui::runtime::focus::focused_rec(ctx).is_some_and(|rec| rec.stop.id == id || rec.stop.root == id) || ctx.memory(|m| m.has_focus(id) || m.has_focus(text_id(id)))
}

/// What a field remembers between passes: the pass it was last drawn in and whether an edit was under way.
#[derive(Clone, Copy, Default)]
struct Memo {
    editing: bool,
    pass: u64,
}

/// Whether the field had an unfinished edit on the immediately preceding egui pass.
pub(crate) fn was_editing(ctx: &egui::Context, widget: &str) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<Memo>(id(widget).with("memo")).is_some_and(|memo| memo.editing && memo.pass.checked_add(1) == Some(pass)))
}

/// Registers the field's real placed rect for automation, and its edit menu's items (`{widget}:cut`, `:copy`,
/// `:paste`, `:selectAll`) while that menu is open. SDK hit responses are clipped and become a zero-height rect on
/// the clip edge when scrolled away: the placed geometry is kept for visibility, scrolling and automation.
pub(crate) fn register_field(ui: &egui::Ui, widget: &str, response: &egui::Response, record: Option<&craft::ui::runtime::inspect::Recorded>) -> Rect {
    if craft::ui::runtime::measure::active(ui.ctx()) || ui.is_sizing_pass() {
        return response.rect;
    }
    let rect = record
        .and_then(|record| record.elements.iter().find(|element| element.id == response.id && element.part.is_none()))
        .map_or(response.rect, |element| element.rect);
    if let Some(record) = record {
        for element in &record.elements {
            let Some(facts) = &element.facts else { continue };
            let action = [("Cut", "cut"), ("Copy", "copy"), ("Paste", "paste"), ("Select All", "selectAll")]
                .into_iter()
                .find(|(label, _)| facts.name.as_deref() == Some(crate::i18n::tr(label)));
            if element.component == "MenuItem"
                && let Some((_, action)) = action
                && let Some(response) = ui.ctx().read_response(element.id)
                // (a menu body still fading out after it closed publishes nothing)
                && egui::AreaState::load(ui.ctx(), response.layer_id.id).is_none_or(|state| state.interactable)
            {
                crate::widgets::register_response(format!("{widget}:{action}"), response.rect, &response);
            }
        }
    }
    // The keyboard focus sits on the field's text part: register that response, so a focused id names the widget.
    let text_part = ui.ctx().read_response(text_id(response.id));
    if response.interact_rect.is_positive() {
        crate::widgets::register_response(widget, rect, text_part.as_ref().unwrap_or(response));
    } else {
        crate::widgets::register_geometry(ui.ctx(), widget, rect);
    }
    rect
}

/// Whether the field's own edit menu (its Cut, Copy, Paste and Select All rows) is shown in this pass, open or still
/// fading out after Esc closed it (the focus comes back to the field once it has gone).
fn edit_menu_open(_ctx: &egui::Context, record: Option<&craft::ui::runtime::inspect::Recorded>) -> bool {
    record.is_some_and(|record| {
        record.elements.iter().any(|element| {
            element.component == "MenuItem" && element.facts.as_ref().is_some_and(|facts| facts.name.as_deref() == Some(crate::i18n::tr("Copy")))
        })
    })
}

impl<'a> TextField<'a> {
    /// A one-line field (Return ends the edit) registered as `widget`.
    pub fn singleline(widget: &'a str, text: &'a mut String) -> Self {
        TextField { widget, text, multiline: false, hint: None, label: None, width: None, select_on_focus: false, begin_edit: false, rows: None }
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

    /// The name a screen reader announces for the field (its visible caption); the hint
    /// stands in when there is none.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The field's width; `f32::INFINITY` fills the row.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// How many lines a multi-line field shows.
    pub fn rows(mut self, rows: usize) -> Self {
        self.rows = Some(rows);
        self
    }

    /// Select all the text when the field takes focus, so typing replaces it (names, numbers).
    pub fn select_on_focus(mut self, on: bool) -> Self {
        self.select_on_focus = on;
        self
    }

    /// Give a newly revealed value editor the focus before its first input is processed.
    pub(crate) fn begin_edit(mut self, on: bool) -> Self {
        self.begin_edit = on;
        self
    }

    /// Draw the field and return its response, any ending this pass, and whether an edit is under way.
    pub fn show(self, ui: &mut Ui) -> TextFieldResponse {
        let id = id(self.widget);
        let pass = ui.ctx().cumulative_pass_nr();
        let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
        if self.begin_edit && real {
            craft::ui::runtime::focus::request(ui.ctx(), FocusTarget::Id(id));
        }
        let name = self.label.clone().or_else(|| self.hint.clone()).unwrap_or_default();
        let widget = self.widget;
        let multiline = self.multiline;
        let (shown, record) = craft::ui::runtime::inspect::record(ui, id.with("inspect"), |ui| {
            let mut field = craft::ui::text::TextField::new(name, Bind::bound(&mut *self.text)).id(id).limit(None);
            if let Some(hint) = self.hint.clone() {
                field = field.placeholder(hint);
            }
            if multiline {
                field = field.lines(self.rows.unwrap_or(3));
            } else {
                field = field.submits();
            }
            if self.select_on_focus {
                field = field.select_on_focus();
            }
            // while photos are carried a right-click opens no edit menu: that is not what the person is doing
            if crate::widgets::carrying(ui.ctx()) {
                field = field.own_context_menu();
            }
            match self.width {
                Some(w) if w.is_finite() => {
                    let lines = if multiline { self.rows.unwrap_or(3).max(1) as f32 } else { 1.0 };
                    let h = ui.spacing().interact_size.y.max(24.0) + (lines - 1.0) * ui.text_style_height(&egui::TextStyle::Body);
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(w.max(1.0), h), egui::Sense::hover());
                    field.show_at(ui, rect)
                }
                Some(_) => field.sizing(Sizing::FillWidth).show(ui),
                None => field.show(ui),
            }
        });
        register_field(ui, widget, &shown.response, record.as_deref());
        let committed = shown.events.iter().any(|event| matches!(event, craft::behavior::EditEvent::Commit { .. }));
        let ending = if !real {
            None
        } else if shown.escaped {
            Some(Ending::Escape)
        } else if shown.submitted && !multiline {
            Some(Ending::Return)
        } else if committed || shown.left_by_tab.is_some() {
            Some(Ending::Away)
        } else {
            None
        };
        // Return and Escape end the edit and give up the keyboard, as the app's fields always have: the next key is a
        // shortcut again.
        if matches!(ending, Some(Ending::Return | Ending::Escape)) {
            blur_id(ui.ctx(), id);
        }
        // The field's own Cut / Copy / Paste menu takes the focus while it is open, but the edit goes on under it: the
        // typing stays, and Esc closes only the menu.
        let editing = ending.is_none()
            && (has_focus(ui.ctx(), id) || (real && was_editing(ui.ctx(), widget) && edit_menu_open(ui.ctx(), record.as_deref())));
        if real {
            ui.data_mut(|d| d.insert_temp(id.with("memo"), Memo { editing, pass }));
        }
        TextFieldResponse { response: shown.response, ending, editing }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_filter_tests;
