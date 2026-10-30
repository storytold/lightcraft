use super::{GOTO, LABEL_W, LANDED, LANDED_FOR, RichText, Tokens, register};
use crate::widgets::register_geometry;

/// `path` shortened to fit `max` (as `width` measures it) by dropping leading folders, after
/// either kind of separator, so that its end stays (`…/Pictures/Library`,
/// `…\Pictures\Library`): the end says which folder it is. A last folder too long by itself
/// loses its end ([`crate::widgets::elide_head`]).
pub(super) fn elide_path(path: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    if width(path) <= max {
        return path.to_string();
    }
    // (a separator at the very end is not where a name starts: `…/` alone would say nothing)
    let end = path.trim_end_matches(['/', '\\']).len();
    let mut rest = path;
    while let Some(i) = rest.find(['/', '\\']).filter(|i| path.len() - rest.len() + i + 1 < end) {
        let separator = if rest.get(i..).is_some_and(|r| r.starts_with('\\')) { '\\' } else { '/' };
        rest = rest.get(i + 1..).unwrap_or("");
        let shorter = format!("…{separator}{rest}");
        if width(&shorter) <= max {
            return shorter;
        }
    }
    let name = rest.trim_end_matches(['/', '\\']);
    // (nothing but separators: never an empty label)
    if name.is_empty() { "…".to_string() } else { crate::widgets::elide_head(name, max, width) }
}

/// A folder or file path on one line in the room left in its row (`text:{id}`): shortened from
/// the front when it is too long ([`elide_path`]), with all of it in a tooltip then. `after` is
/// the width of what follows it on the line (a button, a note) and its spacing: the path leaves
/// that room, so those never wrap onto a second line. Every path is registered as `path:{id}` too,
/// relative ones also (what the filter leaves out of what it looks through).
pub(super) fn path_label(ui: &mut egui::Ui, id: &str, path: &str, font: egui::FontId, color: egui::Color32, after: f32) {
    let rect = elided_label(ui, id, path, font, color, after);
    register(ui, format!("path:{id}"), rect);
}

/// [`path_label`] for words that stand where a path would, with no folder to show ("In memory"):
/// the filter looks through them.
pub(super) fn path_stand_in(ui: &mut egui::Ui, id: &str, text: &str, font: egui::FontId, color: egui::Color32, after: f32) {
    elided_label(ui, id, text, font, color, after);
}

/// `text` on one line (`text:{id}`), shortened from the front when it is too long, with all of it in a tooltip then.
fn elided_label(ui: &mut egui::Ui, id: &str, text: &str, font: egui::FontId, color: egui::Color32, after: f32) -> egui::Rect {
    let shown =
        elide_path(text, (ui.available_width() - after).max(0.0), |s| ui.painter().layout_no_wrap(s.to_string(), font.clone(), color).size().x);
    // (`truncate` is the last resort, for a room narrower than "…")
    let r = ui.add(egui::Label::new(RichText::new(&shown).font(font).color(color)).truncate().show_tooltip_when_elided(false));
    register(ui, format!("text:{id}"), r.rect);
    let rect = r.rect;
    if shown != text {
        r.on_hover_text(text);
    }
    rect
}

/// The width of `text` in the style `style`, and the gap before it: what a label takes in a row.
pub(super) fn label_width(ui: &egui::Ui, text: &str, style: egui::TextStyle) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), style.resolve(ui.style()), egui::Color32::WHITE).size().x + ui.spacing().item_spacing.x
}

/// The same for a button, which has padding either side of its text.
pub(super) fn button_width(ui: &egui::Ui, text: &str) -> f32 {
    label_width(ui, text, egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.x
}

/// What [`ask_first`] makes of a button: [`Asked::clicked`] once the person has said yes.
pub(crate) struct Asked {
    yes: bool,
}

impl Asked {
    /// The person pressed the button and then said yes to its question.
    pub(crate) fn clicked(&self) -> bool {
        self.yes
    }
}

/// A button whose action cannot be taken back, or takes long to make good again, asks before it
/// acts. Pressing `button` (registered by the caller as `id`) opens a small panel under it with
/// `question`, which says what the action costs, and two buttons: `yes` (`{id}:confirm`) and
/// Keep (`{id}:keep`), which has the focus when the panel opens. A click elsewhere or Esc answers no. What comes back stands in for the
/// button: its `clicked` is true on the frame the answer was yes.
pub(crate) fn ask_first(t: &Tokens, id: &str, button: egui::Response, question: &str, yes: &str) -> Asked {
    let mut answer = false;
    let opened = button.clicked();
    let mut ids = None;
    egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_max_width(280.0);
        legible_controls(ui, t);
        ui.add(egui::Label::new(RichText::new(crate::i18n::tr(question)).color(t.text_label)).wrap());
        ui.horizontal(|ui| {
            let r = ui.button(crate::i18n::tr(yes));
            let confirm = r.id;
            register(ui, format!("{id}:confirm"), r.rect);
            if r.clicked() {
                answer = true;
                ui.close();
            }
            let r = ui.button(crate::i18n::tr("Keep"));
            register(ui, format!("{id}:keep"), r.rect);
            // (the question opens with the focus on Keep, so Return or Space alone changes nothing)
            if opened {
                r.request_focus();
            }
            if r.clicked() {
                ui.close();
            }
            ids = Some((confirm, r.id));
        });
    });
    // However the question closed (yes, Keep, Esc, a click elsewhere), the focus goes back to the
    // button it was about, unless the click elsewhere put it on another widget. Only in the frame
    // after the question was last drawn: a button that was not drawn since (a model's Remove while
    // the model was gone) left its entry behind, and the button coming back is not that question's.
    let ctx = button.ctx.clone();
    let was_open = egui::Id::unique(("ask-first", id));
    let frame = ctx.cumulative_frame_nr();
    if let Some((confirm, keep)) = ids {
        ctx.data_mut(|d| d.insert_temp(was_open, (confirm, keep, frame)));
    } else if let Some((confirm, keep, shown)) = ctx.data(|d| d.get_temp::<(egui::Id, egui::Id, u64)>(was_open)) {
        ctx.data_mut(|d| d.remove::<(egui::Id, egui::Id, u64)>(was_open));
        let focused = ctx.memory(|m| m.focused());
        if frame.saturating_sub(shown) <= 1 && (focused.is_none() || focused == Some(keep) || focused == Some(confirm)) {
            button.request_focus();
        }
    }
    Asked { yes: answer }
}

/// One look for "the focus is here", whatever the control, for everything drawn in `ui` from here
/// on: a line 2 pt wide in the text colour round it, as round a tab or a choice
/// ([`crate::widgets::focus_ring`]). egui draws a button, checkbox, radio button or list that has
/// the focus (or is being pressed) with its "active" outline, and a text box in use with the
/// selection's.
pub(super) fn focus_style(ui: &mut egui::Ui, t: &Tokens) {
    let visuals = ui.visuals_mut();
    visuals.widgets.active.bg_stroke = egui::Stroke::new(2.0, t.text);
    visuals.selection.stroke = egui::Stroke::new(2.0, t.text);
}

pub(crate) fn heading(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add_space(4.0);
    // `text` is the heading's English name (its message key). It is shown translated and
    // registered as `heading:{text}` whatever the language, where a section starts: for the
    // filter, scripts and tests that look for one.
    let behind = ui.painter().add(egui::Shape::Noop);
    let r = ui.label(RichText::new(crate::i18n::tr(text)).font(t.semibold(12.5)).color(t.text));
    let name = text;
    register(ui, format!("heading:{name}"), r.rect);
    // (chosen from what the filter found: the section is brought to the top of the page, and its
    // heading is marked for a moment, so that the one chosen is told from its neighbours)
    let now = ui.input(|i| i.time);
    let landed_id = egui::Id::unique(LANDED);
    if ui.data(|d| d.get_temp::<(&'static str, String)>(egui::Id::unique(GOTO))).is_some_and(|(_, section)| section == name) {
        r.scroll_to_me(Some(egui::Align::TOP));
        ui.data_mut(|d| d.insert_temp(landed_id, (name.to_string(), now)));
    }
    if let Some((_, since)) = ui.data(|d| d.get_temp::<(String, f64)>(landed_id)).filter(|(landed, _)| *landed == name) {
        // (full for the first half of its time, then fading out)
        let left = (1.0 - (now - since) / LANDED_FOR).clamp(0.0, 1.0) as f32;
        if left > 0.0 {
            let strength = (left * 2.0).min(1.0);
            let around = r.rect.expand2(egui::vec2(6.0, 2.0));
            let outline = egui::Stroke::new(1.0, t.legible(t.accent, t.chrome, 3.0).gamma_multiply(strength));
            ui.painter().set(behind, egui::Shape::rect_filled(around, egui::CornerRadius::same(4), t.hover.gamma_multiply(strength)));
            ui.painter().rect_stroke(around, egui::CornerRadius::same(4), outline, egui::StrokeKind::Inside);
            register_geometry(ui.ctx(), format!("settingsLanded:{name}"), around);
            ui.ctx().request_repaint();
        } else {
            ui.data_mut(|d| d.remove::<(String, f64)>(landed_id));
        }
    }
}

pub(crate) fn row<R>(ui: &mut egui::Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(LABEL_W, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            // (a label longer than its column wraps in it; it does not push the controls along)
            ui.add(egui::Label::new(RichText::new(crate::i18n::tr(label)).color(t.text_label)).wrap());
        });
        // The controls wrap in the room beside the label, each further line starting under the
        // first control, so a row with many choices grows down and never widens the window.
        let wrapped = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 24.0), wrapped, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            add(ui)
        })
        .inner
    })
    .inner
}

/// What says where a control is, and what a text box says while it is empty, made legible for
/// everything drawn in `ui` from here on: the border of a button, checkbox, radio button, list or
/// text box stands 3:1 against the window and against a theme card's fill, a text box in use is
/// outlined in an accent that does too, and placeholder text stands 4.5:1 in its box.
pub(super) fn legible_controls(ui: &mut egui::Ui, t: &Tokens) {
    // (a little over 3:1 as a colour: a radio button's ring is one antialiased pixel wide, and
    // what is drawn of it is fainter than the colour it is drawn in)
    let border = t.legible(t.legible(t.button_border, t.chrome, 3.0), t.inset, 3.6);
    let visuals = ui.visuals_mut();
    visuals.widgets.inactive.bg_stroke.color = border;
    visuals.selection.stroke.color = t.legible(t.accent, t.chrome, 3.0);
    visuals.weak_text_color = Some(t.legible(t.text_dim, t.field, 4.5));
}

pub(crate) fn hint(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    // (an explanation is small text: the dim colour where it reads, else as near it as reads)
    ui.label(RichText::new(crate::i18n::tr(text)).size(11.0).color(t.legible(t.text_dim, t.chrome, 4.5)));
}

/// A checkbox addressable as `check:{id}`; true when toggled.
pub(crate) fn check(ui: &mut egui::Ui, id: &str, value: &mut bool, label: &str) -> bool {
    let r = ui.checkbox(value, crate::i18n::tr(label));
    register(ui, format!("check:{id}"), r.rect);
    // (ringed round the whole of it while it has the focus, as a choice or a tab is)
    crate::widgets::focus_ring(ui, &Tokens::get(ui.ctx()), &r, r.rect.expand2(egui::vec2(3.0, 1.0)));
    r.changed()
}

/// Mutually exclusive buttons (`button:{id}-{index}`).
pub(crate) fn choices<V: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, options: &[(V, &str)], value: &mut V) -> bool {
    let mut changed = false;
    ui.spacing_mut().item_spacing.x = 4.0;
    // (the row is one stop for Tab, at the choice that holds; the arrow keys go along it)
    let mut group = crate::widgets::Roving::new(ui, id, options.iter().position(|(v, _)| *v == *value));
    for (i, (v, l)) in options.iter().enumerate() {
        if crate::widgets::choice_button_in(ui, &mut group, &format!("{id}-{i}"), l, *value == *v).clicked() && *value != *v {
            *value = *v;
            changed = true;
        }
    }
    group.end(ui);
    changed
}

#[cfg(test)]
mod path_tests {
    use super::elide_path;

    /// Width = characters, so a limit of 20 is "20 characters".
    fn fit(path: &str, max: usize) -> String {
        elide_path(path, max as f32, |s| s.chars().count() as f32)
    }

    #[test]
    fn a_path_that_fits_is_left_alone() {
        assert_eq!(fit("/Users/me/Pictures", 18), "/Users/me/Pictures");
        assert_eq!(fit("", 5), "");
    }

    #[test]
    fn leading_folders_go_first_and_the_end_stays() {
        assert_eq!(fit("/Users/me/Pictures/Library", 20), "…/Pictures/Library");
        assert_eq!(fit("/Users/me/Pictures/Library", 10), "…/Library");
    }

    /// Windows paths: `Path::display` gives backslashes there.
    #[test]
    fn a_path_with_backslashes_keeps_its_end_too() {
        assert_eq!(fit(r"C:\Users\me\Pictures\Library", 20), r"…\Pictures\Library");
        assert_eq!(fit(r"C:\Users\me/Pictures\Library", 10), r"…\Library");
    }

    #[test]
    fn a_last_folder_too_long_by_itself_loses_its_end() {
        assert_eq!(fit("/Users/me/a-very-long-folder-name", 8), "a-very-…");
        assert_eq!(fit("no-separator-at-all", 8), "no-sepa…");
        assert_eq!(fit("/Users/Фотографии-из-отпуска", 8), "Фотогра…");
    }

    #[test]
    fn a_separator_at_the_end_is_not_where_the_name_starts() {
        assert_eq!(fit("/Users/me/Pictures/Library/", 11), "…/Library/");
        assert_eq!(fit("/Users/me/a-very-long-folder-name/", 8), "a-very-…");
        assert_eq!(fit(r"C:\Users\me\Library\", 11), r"…\Library\");
        assert_eq!(fit("///", 2), "…");
        assert_eq!(fit(r"\\server\share\folder", 15), r"…\share\folder");
    }
}
