//! Pre-import culling: a cursor and large preview over the same checked candidates as the grid.

use egui::{Key, Modifiers, Sense, vec2};
use serde_json::{Value, json};

use super::ImportDialog;
use crate::{LightcraftApp, render::Slot, theme::Tokens, widgets};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GridClick {
    index: usize,
    checks: Vec<bool>,
    anchor: Option<usize>,
}

/// A single click retains the grid's toggle/range behavior. A double-click opens the preview
/// with the choices from before its first click, including a heterogeneous Shift-click range.
pub(super) fn grid_click(d: &mut ImportDialog, i: usize, shift: bool, double: bool) {
    if double {
        if let Some(before) = d.last_grid_click.take().filter(|before| before.index == i) {
            d.checked = before.checks;
            d.last_clicked = before.anchor;
        }
        let _ = command(d, &json!({"action": "preview", "index": i}));
    } else {
        let before = GridClick { index: i, checks: d.checked.clone(), anchor: d.last_clicked };
        let _ = command(d, &json!({"action": "select", "index": i}));
        d.click(i, shift);
        d.last_grid_click = Some(before);
    }
}

impl ImportDialog {
    pub(super) fn active_index(&self) -> Option<usize> {
        self.active.filter(|i| *i < self.candidates.len()).or_else(|| (!self.candidates.is_empty()).then_some(0))
    }
}

/// Shared dispatcher for buttons, contextual keys and the `import.review` UI command. A query
/// does not mutate anything. Validate arguments before changing either the cursor or checks.
pub(crate) fn command(d: &mut ImportDialog, p: &Value) -> Result<Value, String> {
    let action = match p.get("action") {
        None => "get",
        Some(v) => v.as_str().ok_or("import.review: action must be a string")?,
    };
    if !matches!(action, "get" | "grid" | "preview" | "select" | "next" | "previous" | "include" | "exclude" | "toggle") {
        return Err(format!("import.review: unknown action `{action}`"));
    }
    let index = match p.get("index") {
        None => d.active_index(),
        Some(v) => {
            let i = v.as_u64().and_then(|n| usize::try_from(n).ok()).filter(|i| *i < d.candidates.len());
            Some(i.ok_or("import.review: index must name a candidate in this review")?)
        }
    };
    if action == "select" && p.get("index").is_none() {
        return Err("import.review: select needs an index".into());
    }
    if !matches!(action, "get" | "grid") {
        let i = index.ok_or("import.review: there are no candidates to review")?;
        if matches!(action, "include" | "exclude" | "toggle") {
            if !d.importable(i) {
                return Err("import.review: this candidate is unreadable or already in the library".into());
            }
            let checked = d.checked.get_mut(i).ok_or("import.review: candidate has no import checkbox")?;
            *checked = match action {
                "include" => true,
                "exclude" => false,
                _ => !*checked,
            };
        }
        let i = match action {
            "next" => i.saturating_add(1).min(d.candidates.len().saturating_sub(1)),
            "previous" => i.saturating_sub(1),
            _ => i,
        };
        d.active = Some(i);
        d.scroll_to_active = true;
    }
    match action {
        "preview" => d.preview = true,
        "grid" => {
            d.preview = false;
            d.scroll_to_active = true;
        }
        _ => {}
    }
    if action != "get" {
        d.last_grid_click = None;
    }
    let active = d.active_index();
    Ok(json!({
        "active": active, "preview": d.preview, "total": d.candidates.len(),
        "path": active.and_then(|i| d.candidates.get(i)).map(|c| &c.path),
        "checked": active.and_then(|i| d.checked.get(i)),
        "importable": active.is_some_and(|i| d.importable(i)),
        "selected": d.selected_paths().len(),
    }))
}

fn act(d: &mut ImportDialog, action: &str) {
    // Button availability and keyboard guards use the same candidate eligibility as the command.
    let _ = command(d, &json!({"action": action}));
}

/// Text fields, sliders, tab-focused buttons, open popups and covering prompts keep their keys.
pub(super) fn keyboard(ui: &egui::Ui, d: &mut ImportDialog) {
    let ctx = ui.ctx();
    if !ui.is_enabled() || egui::Popup::is_any_open(ctx) || widgets::escape_taken(ctx) {
        return;
    }
    if ctx.egui_wants_keyboard_input() {
        // Return to the grid even after a preview control was reached with Tab.
        if d.preview && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            act(d, "grid");
            widgets::take_escape(ctx);
        }
        return;
    }
    // Several keys can arrive between frames. Preserve their order: X then Right excludes the
    // photo being left, whereas Right then X excludes the next photo. Keep repeated arrows too.
    let mut escaped = false;
    let mut preview = d.preview;
    let actions = ctx.input_mut(|input| {
        let mut actions = Vec::new();
        input.events.retain(|event| {
            let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { return true };
            if *modifiers != Modifiers::NONE {
                return true;
            }
            let action = match key {
                Key::ArrowRight => "next",
                Key::ArrowLeft => "previous",
                Key::D | Key::E => {
                    preview = true;
                    "preview"
                }
                Key::G => {
                    preview = false;
                    "grid"
                }
                Key::Escape if preview => {
                    preview = false;
                    escaped = true;
                    "grid"
                }
                Key::P => "include",
                Key::X => "exclude",
                Key::Space => "toggle",
                _ => return true,
            };
            actions.push(action);
            false
        });
        actions
    });
    for action in actions {
        act(d, action);
    }
    if escaped {
        widgets::take_escape(ctx);
    }
}

pub(super) fn toolbar(ui: &mut egui::Ui, d: &mut ImportDialog) {
    ui.horizontal_wrapped(|ui| {
        for (id, label, action, selected) in [("importGrid", "Grid", "grid", !d.preview), ("importPreview", "Preview", "preview", d.preview)] {
            if ui.add_enabled_ui(d.active_index().is_some(), |ui| widgets::text_button(ui, id, crate::i18n::tr(label), selected)).inner.clicked() {
                act(d, action);
            }
        }
        let active = d.active_index();
        for (id, label, action, enabled) in [
            ("importPrevious", "Previous", "previous", active.is_some_and(|i| i > 0)),
            ("importNext", "Next", "next", active.is_some_and(|i| i.saturating_add(1) < d.candidates.len())),
            ("importInclude", "Include", "include", active.is_some_and(|i| d.importable(i))),
            ("importExclude", "Exclude", "exclude", active.is_some_and(|i| d.importable(i))),
        ] {
            if ui.add_enabled_ui(enabled, |ui| widgets::text_button(ui, id, crate::i18n::tr(label), false)).inner.clicked() {
                act(d, action);
            }
        }
    });
    ui.label(crate::i18n::tr("← / →: previous / next · P: include · X: exclude · Space: toggle · E: preview · G: grid"));
}

/// One candidate at a time: larger density-aware previews, bounded to 2048 px and released on
/// navigation. This never probes, imports or changes any source file on the UI thread.
pub(super) fn preview(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &ImportDialog) {
    let Some(i) = d.active_index() else { return };
    let Some(c) = d.candidates.get(i) else { return };
    let Ok(index) = u32::try_from(i) else { return };
    let t = Tokens::get(ui.ctx());
    let included = d.checked.get(i).copied().unwrap_or(false) && d.importable(i);
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("{} / {} · {}", i.saturating_add(1), d.candidates.len(), c.name)).on_hover_text(&c.path);
        ui.label(crate::i18n::tr(if included { "Included" } else { "Excluded" }));
        ui.label(format!("{} × {} · {}", c.width, c.height, c.format));
    });
    let height = (ui.available_height() - 48.0).clamp(160.0, 1200.0);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(1.0), height), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, ui.is_enabled(), &c.name));
    widgets::register(ui, "import:preview", rect);
    let pixels = rect.width().max(rect.height()) * ui.ctx().pixels_per_point();
    let edge = [512, 1024, 1536, 2048].into_iter().find(|edge| *edge as f32 >= pixels).unwrap_or(2048);
    app.renderer.import_preview(&mut app.session, c, index, edge);
    let slot = Slot::Import(index);
    ui.painter().rect_filled(rect, 3.0, t.skeleton);
    if let Some(tex) = app.renderer.textures.get(&slot) {
        let [w, h] = tex.size;
        let scale = ((rect.width() - 8.0).max(1.0) / w.max(1) as f32).min((rect.height() - 8.0).max(1.0) / h.max(1) as f32);
        let fit = egui::Rect::from_center_size(rect.center(), vec2(w as f32 * scale, h as f32 * scale));
        ui.painter().image(tex.tex.id(), fit, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    } else {
        let message = if c.error.is_some() || app.renderer.failure(slot).is_some() { "Preview unavailable" } else { "Loading preview…" };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, crate::i18n::tr(message), t.font(14.0), t.text_dim);
    }
    if let Some(reason) = c.error.as_deref().or_else(|| app.renderer.failure(slot)) {
        ui.colored_label(t.caution, reason);
    } else if let Some(reason) = c.preview_only.as_deref() {
        ui.label(crate::i18n::tr("Preview only")).on_hover_text(reason);
    } else if c.duplicate.is_some() && !d.is_trashed(i) {
        ui.label(crate::i18n::tr("Duplicate"));
    }
    if !ui.is_sizing_pass() {
        app.renderer.retain_imports(&std::collections::HashSet::from([index]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::import::ImportCandidate;

    #[test]
    fn review_commands_validate_before_mutating_and_keep_import_eligibility() {
        let candidates = vec![
            ImportCandidate { path: "a.jpg".into(), ..Default::default() },
            ImportCandidate { path: "duplicate.jpg".into(), duplicate: Some("content".into()), ..Default::default() },
            ImportCandidate { path: "unreadable.jpg".into(), error: Some("bad header".into()), ..Default::default() },
            ImportCandidate { path: "deleted.jpg".into(), duplicate: Some("path".into()), ..Default::default() },
        ];
        let mut d = ImportDialog::new(candidates);
        d.set_trashed(vec![false, false, false, true]);
        d.set_on_deleted("");
        assert_eq!(command(&mut d, &json!({})).unwrap()["active"], 0);
        for bad in [json!({"action": 3}), json!({"action": "wat"}), json!({"action": "select"})] {
            let before = d.clone();
            assert!(command(&mut d, &bad).is_err());
            assert_eq!(d, before);
        }
        for index in [json!(-1), json!(4), json!(u64::MAX), json!(0.5), json!("0")] {
            let before = d.clone();
            assert!(command(&mut d, &json!({"action": "include", "index": index})).is_err());
            assert_eq!(d, before);
        }
        for i in [1, 2] {
            assert!(command(&mut d, &json!({"action": "preview", "index": i})).is_ok());
            let before = d.clone();
            assert!(command(&mut d, &json!({"action": "include"})).is_err());
            assert_eq!(d, before);
        }
        // Recently Deleted with Leave them is still eligible when explicitly included by hand.
        command(&mut d, &json!({"action": "include", "index": 3})).unwrap();
        command(&mut d, &json!({"action": "next"})).unwrap();
        assert_eq!(d.active, Some(3));
        assert_eq!(d.selected_paths(), vec!["a.jpg", "deleted.jpg"]);
        command(&mut d, &json!({"action": "select", "index": 0})).unwrap();
        command(&mut d, &json!({"action": "previous"})).unwrap();
        assert_eq!(d.active, Some(0));
    }

    #[test]
    fn empty_and_deserialized_reviews_have_safe_cursor_defaults() {
        let mut empty = ImportDialog::default();
        assert_eq!(command(&mut empty, &json!({})).unwrap()["active"], Value::Null);
        assert!(command(&mut empty, &json!({"action": "preview"})).is_err());
        assert!(command(&mut empty, &json!({"action": "grid"})).is_ok());
        let mut d: ImportDialog = serde_json::from_value(json!({"candidates": [{"path": "a.jpg"}], "checked": []})).unwrap();
        assert_eq!(d.active_index(), Some(0));
        let before = d.clone();
        assert!(command(&mut d, &json!({"action": "include"})).is_err());
        assert_eq!(d, before);
    }

    #[test]
    fn double_click_restores_mixed_shift_ranges_and_keeps_import_options() {
        let mut d = ImportDialog::new((0..5).map(|_| ImportCandidate::default()).collect());
        d.checked = vec![true, false, true, false, true];
        d.last_clicked = Some(0);
        d.copy = true;
        d.move_files = true;
        d.destination = "chosen-folder".into();
        d.preset = "chosen-preset".into();
        d.keywords = "wedding, delivery".into();
        let before = d.clone();
        grid_click(&mut d, 4, true, false);
        assert_eq!(d.checked, vec![false; 5]);
        grid_click(&mut d, 4, true, true);
        assert!(d.preview);
        assert_eq!(d.checked, before.checked);
        assert_eq!(d.last_clicked, before.last_clicked);
        assert_eq!(
            (d.copy, d.move_files, d.destination, d.preset, d.keywords),
            (before.copy, before.move_files, before.destination, before.preset, before.keywords)
        );
    }
}
