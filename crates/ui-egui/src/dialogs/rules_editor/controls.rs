//! SDK controls mounted inside the rule editor's native, content-sized rows.
use craft::ui::buttons::Button;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Trigger};
use craft::ui::controls::disclosure::DisclosureGlyph;
use craft::ui::menu::{Menu, MenuItem};
use craft::ui::text::{NumberField, TextField};

pub(super) fn measuring(ui: &egui::Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

pub(super) fn register(ui: &egui::Ui, id: impl Into<String>, response: &egui::Response) {
    if !measuring(ui) {
        crate::widgets::register_response(id, response.rect, response);
    }
}

pub(super) fn rect(ui: &mut egui::Ui, width: f32) -> egui::Rect {
    // Do not shrink against available_width: the dialog must grow around a long date pair.
    ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::hover()).0
}

pub(super) fn button(ui: &mut egui::Ui, id: &str, glyph: &str, label: &str, width: f32) -> bool {
    let rect = rect(ui, width);
    let shown = Button::new(crate::i18n::tr(label).to_owned()).painted(crate::i18n::tr(glyph).to_owned()).id_salt(id).show_at(ui, rect);
    let clicked = !measuring(ui) && shown.clicked();
    let response = shown.response.on_hover_text(crate::i18n::tr(label));
    register(ui, id, &response);
    clicked
}

pub(super) fn text(ui: &mut egui::Ui, id: &str, value: &mut String, width: f32, hint: &str) {
    let rect = rect(ui, width);
    let shown = TextField::new(crate::i18n::tr("Value").to_owned(), Bind::bound(value))
        .id_salt(id)
        .placeholder(crate::i18n::tr(hint).to_owned())
        .limit(None)
        .show_at(ui, rect);
    register(ui, id, &shown.response);
}

pub(super) fn number(ui: &mut egui::Ui, id: &str, label: &str, value: f64, lo: f64, hi: f64, step: f64, whole: bool) -> Option<f64> {
    let mut next = Some(value);
    let rect = rect(ui, 72.0);
    let field = NumberField::new(crate::i18n::tr(label).to_owned(), Bind::bound(&mut next))
        .id_salt(id)
        .range(lo..=hi)
        .step(if whole { step.max(1.0) } else { step })
        .commit_decimals(if whole { 0 } else { 2 });
    let field = if whole { field.fixed_decimals(0) } else { field };
    let shown = field.show_at(ui, rect);
    register(ui, id, &shown.response);
    // A malformed or out-of-range stored rule remains visible and invalid until a real edit.
    next.filter(|next| !measuring(ui) && ui.is_enabled() && next.is_finite() && *next != value)
}

pub(super) fn face(ui: &mut egui::Ui, id: &str, label: &str, shown: &str, width: f32) -> egui::Response {
    let rect = rect(ui, width);
    let face = Button::new(crate::i18n::tr(label).to_owned())
        .painted(shown.to_owned())
        .trailing_icon(DisclosureGlyph::dropdown())
        .id_salt(id)
        .show_at(ui, rect);
    register(ui, id, &face.response);
    face.response
}

pub(super) fn item(ui: &mut egui::Ui, id: &str, label: &str, checked: bool) -> bool {
    let row = MenuItem::new(label.to_owned()).id_salt(id).entry_checked(checked).show(ui);
    register(ui, id, &row.response);
    !measuring(ui) && row.ran
}

pub(super) fn select(ui: &mut egui::Ui, id: &str, label: &str, value: &str, options: &[(String, String)], width: f32) -> Option<String> {
    let shown = options.iter().find(|o| o.0 == value).map_or(value, |o| o.1.as_str());
    let face = face(ui, id, label, shown, width);
    let mut picked = None;
    Menu::new(crate::i18n::tr(label).to_owned()).id_salt((id, "menu")).values().opened_by(&face, Trigger::Click).show(ui, |ui, _| {
        for (key, label) in options {
            if item(ui, &format!("{id}:item:{key}"), label, value == key) {
                picked = Some(key.clone());
            }
        }
    });
    picked
}
