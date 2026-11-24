//! Import-local SDK controls. Native picker and import jobs stay in their original owners.
use craft::ui::buttons::Button;
use craft::ui::choice::Checkbox;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Trigger};
use craft::ui::menu::{Menu, MenuItem};
use craft::ui::text::{NumberField, TextField};
use craft::ui::value_drag::Slider;

pub(super) fn measuring(ui: &egui::Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

pub(super) fn register(ui: &egui::Ui, id: impl Into<String>, rect: egui::Rect) {
    if !measuring(ui) {
        crate::widgets::register(ui, id, rect);
    }
}

fn response(ui: &egui::Ui, id: impl Into<String>, response: &egui::Response) {
    if !measuring(ui) {
        crate::widgets::register_response(id, response.rect, response);
    }
}

pub(super) struct ButtonHit {
    response: egui::Response,
    activated: bool,
}
impl ButtonHit {
    pub fn clicked(&self) -> bool {
        self.activated
    }
    pub fn on_hover_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.response = self.response.on_hover_text(text);
        self
    }
}

pub(super) fn button(ui: &mut egui::Ui, id: &str, label: &str, active: bool) -> ButtonHit {
    let shown = Button::new(crate::i18n::tr(label).to_owned()).id_salt(id).selected(active).show(ui);
    response(ui, format!("button:{id}"), &shown.response);
    ButtonHit { activated: !measuring(ui) && shown.clicked(), response: shown.response }
}

pub(super) fn check(ui: &mut egui::Ui, id: &str, label: &str, value: &mut bool) {
    let shown = Checkbox::new(Bind::bound(value)).id_salt(id).label(crate::i18n::tr(label).to_owned()).show(ui);
    response(ui, id, &shown.response);
}

pub(super) fn text(ui: &mut egui::Ui, id: &str, label: &str, value: &mut String, hint: &str, width: f32) {
    // This is an egui row, not an SDK Stack: mount inside its actual remaining width.
    let width = width.min(ui.available_width()).max(1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::hover());
    let shown = TextField::new(crate::i18n::tr(label).to_owned(), Bind::bound(value))
        .id_salt(id)
        .placeholder(crate::i18n::tr(hint).to_owned())
        .limit(None)
        .submits()
        .show_at(ui, rect);
    response(ui, id, &shown.response);
}

/// A value menu preserves stable option IDs, unlike position-indexed picker entries.
pub(super) fn select(ui: &mut egui::Ui, id: &str, label: &str, value: &str, options: &[(&str, &str)], localize: bool) -> Option<String> {
    let shown = options.iter().find(|o| o.0 == value).map_or(options[0].1, |o| o.1);
    let translated = |label: &str| if localize { crate::i18n::tr(label).to_owned() } else { label.to_owned() };
    let face = Button::new(crate::i18n::tr(label).to_owned()).painted(translated(shown)).id_salt(id).show(ui);
    response(ui, format!("combo:{id}"), &face.response);
    let mut picked = None;
    Menu::new(crate::i18n::tr(label).to_owned()).id_salt((id, "menu")).values().opened_by(&face.response, Trigger::Click).show(ui, |ui, _| {
        for &(key, label) in options {
            let row = MenuItem::new(translated(label)).id_salt((id, key)).entry_checked(value == key).show(ui);
            response(ui, format!("button:{id}-{key}"), &row.response);
            if !measuring(ui) && row.ran {
                picked = Some(key.to_owned());
            }
        }
    });
    picked
}

/// Slider and exact-value keyboard field retain existing IDs and double-click reset, without adding buttons.
pub(super) fn number(ui: &mut egui::Ui, spec: &lightcraft_develop::controls::ControlSpec, value: f64) -> Option<f64> {
    let mut current = value;
    ui.horizontal(|ui| {
        let label = ui.label(crate::i18n::tr(spec.label));
        // Main also resets by double-clicking the painted label. Keep that native-row
        // compatibility hit only in a real pass; the SDK track owns its own reset.
        if !measuring(ui)
            && crate::panels::detail::host::target(
                ui,
                ("import-caption", spec.id),
                label.rect,
                crate::panels::detail::host::Target::new(crate::i18n::tr(spec.label)).pointer_only(),
            )
            .double_clicked()
        {
            current = spec.default;
        }
        // The review's thumbnail size takes the row up to its value box, as main's full-width row did; the recipe's
        // rows keep their compact track.
        let room = (ui.available_width() - 64.0 - 2.0 * ui.spacing().item_spacing.x).floor();
        let width = if spec.id == "importThumbSize" && room.is_finite() { room.max(40.0) } else { (ui.available_width() - 72.0).clamp(40.0, 180.0) };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::hover());
        let shown = Slider::new(Bind::bound(&mut current).reset(spec.default), spec.min..=spec.max)
            .id_salt(("import-slider", spec.id))
            .label(crate::i18n::tr(spec.label))
            .step(spec.step)
            .show_at(ui, rect);
        response(ui, format!("slider:{}", spec.id), &shown.response);
        let mut exact = Some(current);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 24.0), egui::Sense::hover());
        let shown = NumberField::new(crate::i18n::tr(spec.label), Bind::bound(&mut exact))
            .id_salt(("import-number", spec.id))
            .range(spec.min..=spec.max)
            .step(spec.step)
            .snap_typed(true)
            .commit_decimals(spec.decimals as usize)
            .fixed_decimals(spec.decimals as usize)
            .show_at(ui, rect);
        response(ui, format!("sliderValue:{}", spec.id), &shown.response);
        response(ui, format!("sliderField:{}", spec.id), &shown.response);
        if let Some(exact) = exact.filter(|v| v.is_finite()) {
            current = spec.clamp(exact);
        }
    });
    (!measuring(ui) && current != value).then_some(current)
}
