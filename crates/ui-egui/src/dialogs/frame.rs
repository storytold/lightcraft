use super::export::LABEL_W;
use super::{BACKDROP_SHOWN, Dialog, LightcraftApp, Tokens};

/// A click on the dimmed backdrop around the dialog: it closes the dialog like Esc. Pressed and
/// released outside the window, so a slider dragged past its edge doesn't close it; a press while a
/// popup is open (a dropdown, a date picker's calendar) only closes the popup: `popup_was_open` is
/// read before the window is drawn, since drawing it already closes the popup. Only a press made
/// while the dialog is shown counts: in the frame a dialog appears, its input is the click that
/// opened it (a quick click on a slow frame presses and releases in that one frame), so it never
/// closes it. (Read from the raw events: egui forgets the press origin on release.)
pub(super) fn clicked_backdrop(ctx: &egui::Context, window: egui::Rect, title_bar: Option<egui::Rect>, popup_was_open: bool) -> bool {
    let press_id = egui::Id::unique("dialog-backdrop-press");
    let frame = ctx.cumulative_frame_nr();
    // the same frame too: a second pass of it gets no input
    let shown = ctx.data(|d| d.get_temp::<u64>(egui::Id::unique(BACKDROP_SHOWN))).is_some_and(|f| f + 1 >= frame);
    if !shown {
        ctx.data_mut(|d| d.remove_temp::<bool>(press_id));
        return false;
    }
    let (press, release, clicked) = ctx.input(|i| {
        let at = |down: bool| {
            i.events.iter().rev().find_map(|e| match e {
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, .. } if *pressed == down => Some(*pos),
                _ => None,
            })
        };
        (at(true), at(false), i.pointer.primary_clicked())
    });
    if let Some(p) = press {
        let outside = !window.contains(p) && !title_bar.is_some_and(|b| b.contains(p)) && !popup_was_open;
        ctx.data_mut(|d| d.insert_temp(press_id, outside));
    }
    let Some(release) = release else { return false };
    let pressed_outside = ctx.data_mut(|d| d.remove_temp::<bool>(press_id)).unwrap_or(false);
    clicked && pressed_outside && !window.contains(release) && !title_bar.is_some_and(|b| b.contains(release))
}

/// Room kept free above and below a dialog (#781).
pub(super) const DIALOG_SCREEN_MARGIN: f32 = 12.0;

/// A dialog's title bar, frame and button row before the first frame measures them.
pub(super) const DIALOG_CHROME_GUESS: f32 = 100.0;

/// A dialog's options never get less room than this, however short the window.
pub(super) const DIALOG_MIN_BODY: f32 = 80.0;

/// A list that scrolls on its own never gets shorter than this to make its dialog fit.
const DIALOG_MIN_LIST: f32 = 120.0;

fn list_shrink_id() -> egui::Id {
    egui::Id::unique("dialog-list-shrink")
}

fn list_applied_id() -> egui::Id {
    egui::Id::unique("dialog-list-applied")
}

/// The height for a list in a dialog that scrolls on its own (What's New, All Metadata, the
/// shortcuts, smart-album rules): `preferred`, less what the dialog needs to fit the window, so
/// on a short window the list gets shorter rather than scrolling inside a scrolling dialog.
pub fn list_height(ui: &egui::Ui, preferred: f32) -> f32 {
    let shrink = ui.data(|d| d.get_temp::<f32>(list_shrink_id())).unwrap_or(0.0);
    let height = (preferred - shrink).max(preferred.min(DIALOG_MIN_LIST));
    ui.data_mut(|d| *d.get_temp_mut_or_default::<f32>(list_applied_id()) += preferred - height);
    height
}

/// A dialog's options: when `fit` names the dialog, the lists in it that scroll on their own give up the room it
/// needs to fit `max_height`, as measured on the previous frame (so on a short window a list gets shorter rather than
/// scrolling inside the scrolling dialog: the framework dialog scrolls what is still too tall). Drawn directly
/// otherwise. Measuring runs only draw.
pub(super) fn fit_lists(ui: &mut egui::Ui, fit: bool, max_height: f32, body: impl FnOnce(&mut egui::Ui)) {
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    if !real {
        body(ui);
        return;
    }
    if !fit {
        ui.data_mut(|d| d.remove_temp::<f32>(list_shrink_id()));
        body(ui);
        return;
    }
    ui.data_mut(|d| d.insert_temp(list_applied_id(), 0.0_f32));
    let top = ui.cursor().top();
    body(ui);
    // what the lists that scroll on their own gave up this frame, and what they should next frame
    let applied = ui.data(|d| d.get_temp::<f32>(list_applied_id())).unwrap_or(0.0);
    let was = ui.data(|d| d.get_temp::<f32>(list_shrink_id())).unwrap_or(0.0);
    let shrink = (ui.min_rect().bottom() - top + applied - max_height).max(0.0);
    if (shrink - was).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(list_shrink_id(), shrink));
        ui.ctx().request_repaint();
    }
}

/// Open a one-field dialog that runs `command` with `params` + `{key: typed value}`.
pub fn prompt(app: &mut LightcraftApp, title: &str, hint: &str, value: &str, command: &str, params: serde_json::Value, key: &str) {
    app.ui.dialog =
        Some(Dialog::TextPrompt { title: title.into(), hint: hint.into(), value: value.into(), command: command.into(), params, key: key.into() });
}

pub(super) fn field<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(LABEL_W, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.label(egui::RichText::new(crate::i18n::tr(label)).color(t.text_label));
        });
        add(ui)
    })
    .inner
}

/// The rest of a [`field`] row, laid out right to left: `add` places its trailing button(s) first,
/// then a text field taking exactly the width that's left. (Subtracting a guessed button width
/// instead made the auto-sized dialog grow every frame when the real button was wider, #8.)
pub(super) fn trailing_button_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let h = ui.spacing().interact_size.y.max(24.0);
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), h), egui::Layout::right_to_left(egui::Align::Center), add);
}

/// Dialogs that Return confirms with nothing focused (those with text fields confirm from the
/// field: `dialog_field`).
pub(super) fn return_confirms(dlg: &Dialog) -> bool {
    matches!(dlg, Dialog::CopySettings { .. } | Dialog::PasteSettings { .. })
}

/// A dialog's one text field: it takes the focus when the dialog opens, and opens with its text
/// selected, so typing replaces it. It takes the focus back only while nothing else has it: taking
/// it back every frame would keep Tab from ever reaching the dialog's buttons and check boxes.
/// True when Return confirms the dialog; Esc cancels the dialog (`show`).
pub(super) fn dialog_field(ui: &mut egui::Ui, widget: &str, text: &mut String, hint: &str) -> bool {
    let r = crate::text_field::TextField::singleline(widget, text).hint(crate::i18n::tr(hint)).width(f32::INFINITY).select_on_focus(true).show(ui);
    // The field itself, the stop the framework focuses, names the widget too, beside the text part that holds egui's
    // keyboard focus.
    if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        crate::widgets::add_hit(widget, &r.response);
    }
    // (and takes it back when nothing has it: after a click on the dialog's background or a check box,
    // or when Return gave it up on a refusal; a control Tab reached keeps it). A disabled dialog (a
    // prompt or the blocking library window owns input) must not request the focus at all: egui's
    // text edit would otherwise keep editing the draft from host text behind that window.
    if ui.is_enabled() && (just_opened(ui, widget) || (!r.editing && r.ending.is_none() && ui.memory(|m| m.focused().is_none()))) {
        crate::text_field::focus_id(ui.ctx(), r.response.id);
    }
    // Escape in a dialog's one field cancels the dialog, its draft with it (the framework convention); the field takes
    // the key to give its text back, so the dialog reads it from here (`escaped_field`).
    if r.cancelled() {
        field_escape_closes(ui);
    }
    r.ending == Some(crate::text_field::Ending::Return)
}

/// A field in a dialog's body took this pass's Escape (`TextFieldResponse::cancelled`) and the dialog closes on it,
/// as it would on an Escape nothing took (read back by `escaped_field`).
pub(crate) fn field_escape_closes(ui: &egui::Ui) {
    if ui.is_enabled() && !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("lightcraft.dialog.field-escaped"), pass));
    }
}

/// Whether a dialog's one field (`dialog_field`) took an Escape in this pass.
pub(super) fn escaped_field(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(egui::Id::unique("lightcraft.dialog.field-escaped"))) == Some(ctx.cumulative_pass_nr())
}

/// The dialog part `key` is drawn this frame but wasn't the frame before: the dialog has just
/// opened (to give its first field the focus once).
pub(super) fn just_opened(ui: &egui::Ui, key: &str) -> bool {
    // A measuring run of the dialog's body (the framework measures it before it places the layer) is not a showing:
    // it must neither take the opening nor use it up for the real run of the same pass.
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return false;
    }
    let id = egui::Id::unique(("dialog-shown", key));
    let pass = ui.ctx().cumulative_pass_nr();
    let last: Option<u64> = ui.data_mut(|d| d.get_temp(id));
    ui.data_mut(|d| d.insert_temp(id, pass));
    last.and_then(|l| l.checked_add(1)) != Some(pass)
}

/// How a dialog's confirm button looks: [`ConfirmStyle::Primary`] (the accent's fill) when it
/// makes or brings in something, plain when it deletes or removes (never the eye-catching
/// default) or when the dialog only shows something (its Close).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmStyle {
    Primary,
    Plain,
}

pub fn confirm_style(dlg: &Dialog) -> ConfirmStyle {
    match dlg {
        Dialog::ConfirmDelete { .. } | Dialog::DeleteKeyword { .. } | Dialog::DeleteEffects { .. } | Dialog::RemoveFolder { .. } => {
            ConfirmStyle::Plain
        }
        _ if only_shows(dlg) => ConfirmStyle::Plain,
        _ => ConfirmStyle::Primary,
    }
}

/// A dialog that only shows something: one Close, nothing to confirm or cancel. (Synchronize
/// Folder is one when its folder turned out up to date.)
pub(super) fn only_shows(dlg: &Dialog) -> bool {
    matches!(
        dlg,
        Dialog::About
            | Dialog::Shortcuts
            | Dialog::Settings { .. }
            | Dialog::SystemInfo { .. }
            | Dialog::AllMetadata { .. }
            | Dialog::WhatsNew
            | Dialog::Welcome { .. }
            | Dialog::ManageEffects { .. }
            | Dialog::ViewOptions
    ) || matches!(dlg, Dialog::SynchronizeFolder { counts: Some(c), .. } if c.up_to_date())
}
