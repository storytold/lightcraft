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

/// Whether the field registered as `widget` has the keyboard focus now.
pub(crate) fn focused(ctx: &egui::Context, widget: &str) -> bool {
    has_focus(ctx, id(widget))
}

/// Gives the field registered as `widget` the keyboard focus.
pub(crate) fn focus(ctx: &egui::Context, widget: &str) {
    craft::ui::runtime::focus::request(ctx, FocusTarget::Id(id(widget)));
}

fn has_focus(ctx: &egui::Context, id: egui::Id) -> bool {
    craft::ui::runtime::focus::focused_rec(ctx).is_some_and(|rec| rec.stop.id == id || rec.stop.root == id) || ctx.memory(|m| m.has_focus(id))
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
            {
                crate::widgets::register_response(format!("{widget}:{action}"), response.rect, &response);
            }
        }
    }
    if response.interact_rect.is_positive() {
        crate::widgets::register_response(widget, rect, response);
    } else {
        crate::widgets::register_geometry(ui.ctx(), widget, rect);
    }
    rect
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
        let editing = ending.is_none() && has_focus(ui.ctx(), id);
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


#[cfg(test)]
mod native_popup_preclose_tests {
    use super::*;
    use crate::headless::HeadlessView;
    use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};

    const FIRST: &str = "preclose-first";
    const SECOND: &str = "preclose-second";

    struct Rig {
        view: HeadlessView,
        text: String,
        sibling: String,
        widgets: Vec<(String, Rect)>,
        endings: Vec<Ending>,
        sibling_open_after_first: bool,
        first_escape_taken: bool,
        enabled: bool,
        measuring: bool,
        time: f64,
    }

    impl Rig {
        fn new() -> Self {
            let mut rig = Self {
                view: HeadlessView::new(), text: "Lisbon".into(), sibling: "Porto".into(),
                widgets: Vec::new(), endings: Vec::new(), sibling_open_after_first: false, first_escape_taken: false,
                enabled: true, measuring: false, time: 0.0,
            };
            rig.settle();
            rig
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.time += 1.0 / 60.0;
            let raw = HeadlessView::raw_input(egui::vec2(500.0, 320.0), 1.0, self.time, events);
            let Self { view, text, sibling, endings, sibling_open_after_first, first_escape_taken, enabled, measuring, .. } = self;
            view.run(raw, |ui| {
                let ctx = ui.ctx().clone();
                let mut draw = |ui: &mut Ui| {
                    ui.add_space(20.0);
                    ui.add_enabled_ui(*enabled, |ui| {
                        let field = TextField::singleline(FIRST, text).width(210.0).show(ui);
                        endings.extend(field.ending);
                        *first_escape_taken = crate::widgets::escape_taken(&ctx);
                    });
                    // Observe the actual sibling owner between the two fields' shows.
                    *sibling_open_after_first = egui::Popup::is_id_open(&ctx, id(SECOND).with("popup"));
                    ui.add_space(80.0);
                    TextField::singleline(SECOND, sibling).width(210.0).show(ui);
                };
                if *measuring {
                    craft::ui::runtime::measure::run(&ctx, || draw(ui));
                } else {
                    draw(ui);
                }
            });
            self.widgets = crate::widgets::take_registry(&self.view.ctx);
        }

        fn settle(&mut self) {
            for _ in 0..4 { self.frame(Vec::new()); }
        }

        fn rect(&self, name: &str) -> Rect {
            self.widgets.iter().rev().find(|(key, _)| key == name).map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("missing {name}: {:?}", self.widgets))
        }

        fn click_without_settling(&mut self, at: Pos2, button: PointerButton) {
            self.time += 1.0;
            let event = |pressed| Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE };
            self.frame(vec![Event::PointerMoved(at), event(true)]);
            self.frame(vec![event(false)]);
        }

        fn click(&mut self, name: &str, button: PointerButton) {
            self.click_without_settling(self.rect(name).center(), button);
            self.settle();
        }

        fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
            Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            self.frame(vec![Self::key_event(key, true, modifiers), Self::key_event(key, false, modifiers)]);
            self.settle();
        }

        fn popup_open(&self, name: &str) -> bool {
            egui::Popup::is_id_open(&self.view.ctx, id(name).with("popup"))
        }

        fn selection(&self) -> Option<egui::text::CCursorRange> {
            egui::text_edit::TextEditState::load(&self.view.ctx, id(FIRST)).and_then(|state| state.cursor.char_range())
        }
    }

    #[test]
    fn escape_omits_the_closing_proxy_before_immediate_secondary_reopen() {
        let mut rig = Rig::new();
        rig.click(FIRST, PointerButton::Primary);
        rig.click(FIRST, PointerButton::Secondary);
        assert!(rig.popup_open(FIRST));
        let at = rig.rect(FIRST).center();
        rig.frame(vec![Rig::key_event(Key::Escape, true, Modifiers::NONE), Rig::key_event(Key::Escape, false, Modifiers::NONE)]);
        assert!(!rig.popup_open(FIRST));
        assert!(rig.widgets.iter().all(|(name, _)| name != "preclose-first:cut"), "closing pass must not draw a menu proxy");
        assert!(rig.first_escape_taken, "the closing pass owns Escape before the frame-local marker expires");
        // The very next pass is the secondary press: no empty/settle frame in between.
        rig.click_without_settling(at, PointerButton::Secondary);
        assert!(rig.popup_open(FIRST), "the first real secondary click must reopen the field's menu");
        assert!(rig.endings.is_empty());
        assert_eq!(rig.text, "Lisbon");
    }

    #[test]
    fn menu_escape_keeps_draft_selection_and_existing_second_escape_cancel() {
        let mut rig = Rig::new();
        rig.click(FIRST, PointerButton::Primary);
        rig.key(Key::A, Modifiers::COMMAND);
        rig.frame(vec![Event::Text("Rome".into())]);
        rig.key(Key::A, Modifiers::COMMAND);
        rig.click(FIRST, PointerButton::Secondary);
        let selected = rig.selection();
        assert!(selected.is_some_and(|range| !range.is_empty()));
        rig.key(Key::Escape, Modifiers::NONE);
        assert_eq!(rig.text, "Rome");
        assert_eq!(rig.selection(), selected);
        assert!(rig.endings.is_empty());
        assert!(rig.view.ctx.memory(|memory| memory.has_focus(id(FIRST))));
        rig.key(Key::Escape, Modifiers::NONE);
        assert_eq!(rig.text, "Lisbon");
        assert_eq!(rig.endings, vec![Ending::Escape]);
    }

    #[test]
    fn earlier_field_does_not_preclose_its_siblings_popup() {
        let mut rig = Rig::new();
        rig.click(SECOND, PointerButton::Primary);
        rig.click(SECOND, PointerButton::Secondary);
        assert!(rig.popup_open(SECOND));
        rig.frame(vec![Rig::key_event(Key::Escape, true, Modifiers::NONE), Rig::key_event(Key::Escape, false, Modifiers::NONE)]);
        assert!(rig.sibling_open_after_first, "the first field must leave the second field's own popup to its owner");
        assert!(!rig.popup_open(SECOND), "the sibling closes its own menu");
        assert!(rig.endings.is_empty());
    }

    #[test]
    fn preclose_does_not_claim_disabled_or_measuring_escape() {
        for measuring in [false, true] {
            let mut rig = Rig::new();
            rig.click(FIRST, PointerButton::Secondary);
            assert!(rig.popup_open(FIRST));
            rig.enabled = measuring;
            rig.measuring = measuring;
            rig.frame(vec![Rig::key_event(Key::Escape, true, Modifiers::NONE), Rig::key_event(Key::Escape, false, Modifiers::NONE)]);
            assert!(!rig.first_escape_taken, "the new preclose must not claim disabled/measurement input during the pass");
            // This checks the new early-close effect, not egui's pre-existing late-close behavior.
        }
    }
}
