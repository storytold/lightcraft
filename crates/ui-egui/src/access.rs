//! Accessibility helpers (P6.3): give custom-painted widgets an AccessKit role and name, so a
//! screen reader announces them. egui's own buttons, checkboxes and sliders name themselves from
//! their text; widgets drawn with `ui.interact` / `allocate_*` and text fields with only a hint do
//! not, and these helpers fill the gap. Every name goes through `i18n::tr`.

use egui::{Response, WidgetInfo, WidgetType};

use crate::i18n::tr;

/// A clickable area that acts as a button, named `label`.
pub fn button(resp: &Response, label: &str) {
    let label = tr(label).to_string();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, resp.enabled(), &label));
}

/// A clickable area that is one choice among several (a tab, a module, a template), named `label`.
pub fn choice(resp: &Response, label: &str, selected: bool) {
    let label = tr(label).to_string();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, resp.enabled(), selected, &label));
}

/// An already-translated or data name (a photo's file name, a template's own name).
pub fn named(resp: &Response, typ: WidgetType, name: &str) {
    resp.widget_info(|| WidgetInfo::labeled(typ, resp.enabled(), name));
}

/// Name a widget that keeps its own role and value (a text field, a colour well): only the label
/// is set, so the field still reports what is typed in it.
pub fn label(resp: &Response, label: &str) {
    let label = tr(label).to_string();
    resp.ctx.accesskit_node_builder(resp.id, |node| node.set_label(label));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_name_their_widgets() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut found = Vec::new();
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let r = ui.allocate_response(egui::vec2(20.0, 20.0), egui::Sense::click());
                button(&r, "Zoom In");
                let r = ui.allocate_response(egui::vec2(20.0, 20.0), egui::Sense::click());
                choice(&r, "Map", true);
                let mut text = String::from("x");
                let r = ui.text_edit_singleline(&mut text);
                label(&r, "Search");
            });
            out.textures_delta.clear();
            if let Some(update) = out.platform_output.accesskit_update.take() {
                found = update.nodes.iter().map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string())).collect();
            }
        }
        let has = |role: &str, label: &str| found.iter().any(|(r, l)| r == role && l == label);
        assert!(has("Button", "Zoom In"), "{found:?}");
        assert!(found.iter().any(|(_, l)| l == "Map"), "{found:?}");
        assert!(has("TextInput", "Search"), "{found:?}");
    }
}
