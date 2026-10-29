use super::{LABEL_W, RichText, Tokens, register};

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
/// that room, so those never wrap onto a second line.
pub(super) fn path_label(ui: &mut egui::Ui, id: &str, path: &str, font: egui::FontId, color: egui::Color32, after: f32) {
    let shown =
        elide_path(path, (ui.available_width() - after).max(0.0), |s| ui.painter().layout_no_wrap(s.to_string(), font.clone(), color).size().x);
    // (`truncate` is the last resort, for a room narrower than "…")
    let r = ui.add(egui::Label::new(RichText::new(&shown).font(font).color(color)).truncate().show_tooltip_when_elided(false));
    register(ui, format!("text:{id}"), r.rect);
    if shown != path {
        r.on_hover_text(path);
    }
}

/// The width of `text` in the style `style`, and the gap before it: what a label takes in a row.
pub(super) fn label_width(ui: &egui::Ui, text: &str, style: egui::TextStyle) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), style.resolve(ui.style()), egui::Color32::WHITE).size().x + ui.spacing().item_spacing.x
}

/// The same for a button, which has padding either side of its text.
pub(super) fn button_width(ui: &egui::Ui, text: &str) -> f32 {
    label_width(ui, text, egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.x
}

pub(crate) fn heading(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(crate::i18n::tr(text)).font(t.semibold(12.5)).color(t.text));
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

pub(crate) fn hint(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.label(RichText::new(crate::i18n::tr(text)).size(11.0).color(t.text_dim));
}

/// A checkbox addressable as `check:{id}`; true when toggled.
pub(crate) fn check(ui: &mut egui::Ui, id: &str, value: &mut bool, label: &str) -> bool {
    let r = ui.checkbox(value, crate::i18n::tr(label));
    register(ui, format!("check:{id}"), r.rect);
    r.changed()
}

/// Mutually exclusive buttons (`button:{id}-{index}`).
pub(crate) fn choices<V: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, options: &[(V, &str)], value: &mut V) -> bool {
    let mut changed = false;
    ui.spacing_mut().item_spacing.x = 4.0;
    for (i, (v, l)) in options.iter().enumerate() {
        if crate::widgets::text_button(ui, &format!("{id}-{i}"), l, *value == *v).clicked() && *value != *v {
            *value = *v;
            changed = true;
        }
    }
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
