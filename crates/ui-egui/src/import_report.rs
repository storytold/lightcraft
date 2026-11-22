//! What an import couldn't carry over from other editors' XMP sidecars, in a window instead of a
//! toast: a large folder from Lightroom gave one toast line per photo, thousands of them, gone in
//! eight seconds. The window groups the warnings by reason with counts, selects the photos of a
//! group, and copies or saves the full list; File ▸ More Imports ▸ Import Warnings… opens it again.
//! Fork-owned.
#![allow(clippy::indexing_slicing, reason = "not yet converted to checked indexing: tracked in docs/gaps.md (indexing lint)")]

use std::collections::{BTreeMap, BTreeSet};

use egui::{RichText, vec2};
use lcc_engine::import::{ImportUprightMode, ImportWarning, ImportWarningCode};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::tr;
use crate::menus::UiCommand;
use crate::pick::{PickRequest, Picked};
use crate::theme::Tokens;
use crate::widgets::register;

pub const OPEN: &str = "dialog.importWarnings";
pub const SAVE: &str = "file.saveImportWarnings";

pub const COMMANDS: &[UiCommand] = &[(OPEN, "Import Warnings…", None, "File>More Imports"), (SAVE, "Save Import Warnings…", None, "")];

/// The last import's warnings, kept until the next import that has any.
#[derive(Default)]
pub struct Report {
    pub open: bool,
    /// photos the import added or restored
    pub imported: usize,
    pub groups: Vec<Group>,
}

/// The warnings that share one reason.
pub struct Group {
    pub title: String,
    pub note: String,
    /// (path, photo), one per photo, in import order
    pub photos: Vec<(String, Option<u64>)>,
}

impl Report {
    pub fn new(imported: usize, warnings: &[ImportWarning]) -> Self {
        // (kind, detail) keeps the groups of one kind together; BTreeMap gives a stable order
        let mut by: BTreeMap<(u8, String), (Vec<(String, Option<u64>)>, BTreeMap<String, usize>, Option<ImportUprightMode>)> = BTreeMap::new();
        for w in warnings {
            let key = match w.code {
                ImportWarningCode::CameraProfile => (0, w.profile.clone().unwrap_or_default()),
                ImportWarningCode::UprightGeometry => (1, format!("{:?}", w.mode.unwrap_or(ImportUprightMode::Unknown))),
                ImportWarningCode::UnmappedXmp => (2, String::new()),
            };
            let entry = by.entry(key).or_default();
            if !entry.0.iter().any(|(p, _)| *p == w.path) {
                entry.0.push((w.path.clone(), w.photo));
            }
            for f in &w.fields {
                *entry.1.entry(f.clone()).or_default() += 1;
            }
            entry.2 = w.mode;
        }
        let mut groups: Vec<Group> = by
            .into_iter()
            .map(|((kind, detail), (photos, fields, mode))| {
                let (title, note) = match kind {
                    0 => (
                        format!("{}: {detail}", tr("Profile not applied")),
                        tr("These photos start from the camera's own look instead, so colours may differ from the other editor.").to_string(),
                    ),
                    1 => upright_text(mode.unwrap_or(ImportUprightMode::Unknown)),
                    _ => {
                        let mut fields: Vec<(String, usize)> = fields.into_iter().collect();
                        fields.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                        let list = fields.iter().map(|(f, n)| format!("{f} ({n})")).collect::<Vec<_>>().join(", ");
                        (tr("Settings with no equivalent here").to_string(), format!("{}: {list}.", tr("Left out")))
                    }
                };
                Group { title, note, photos }
            })
            .collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.photos.len()));
        Report { open: false, imported, groups }
    }

    /// Photos with at least one warning.
    pub fn photos(&self) -> usize {
        self.groups.iter().flat_map(|g| g.photos.iter().map(|(p, _)| p.as_str())).collect::<BTreeSet<_>>().len()
    }

    /// One line for a toast.
    pub fn summary(&self) -> String {
        format!("{}: {}", tr("Photos with settings that didn't carry over"), self.photos())
    }

    /// The whole report as plain text (Copy, Save, the log).
    pub fn text(&self) -> String {
        let mut out = format!("{}\n", self.intro());
        for g in &self.groups {
            out.push_str(&format!("\n{} ({})\n{}\n", g.title, g.photos.len(), g.note));
            for (path, _) in &g.photos {
                out.push_str(&format!("  {path}\n"));
            }
        }
        out
    }

    fn intro(&self) -> String {
        let n = self.photos();
        format!(
            "{}: {}\n{}: {n}\n{}",
            tr("Photos imported"),
            self.imported.max(n),
            tr("With settings that didn't carry over"),
            tr(
                "These photos have develop settings in an XMP sidecar from another editor that can't be applied here. The photos were imported and the sidecars are unchanged; only these settings were left out."
            ),
        )
    }
}

/// The end of a path, which tells photos apart: its file and two folders above it (`…/2026/10-09/DSC_0675.NEF`).
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    match parts.len() {
        0..=3 => path.to_string(),
        n => format!("…/{}", parts[n - 3..].join("/")),
    }
}

fn upright_text(mode: ImportUprightMode) -> (String, String) {
    let name = match mode {
        ImportUprightMode::Auto => Some("Auto"),
        ImportUprightMode::Level => Some("Level"),
        ImportUprightMode::Vertical => Some("Vertical"),
        ImportUprightMode::Full => Some("Full"),
        _ => None,
    };
    match (mode, name) {
        (_, Some(name)) => (
            format!("{} ({})", tr("Upright transform not kept"), tr(name)),
            tr("Upright is worked out again here, so the correction may differ slightly.").to_string(),
        ),
        (ImportUprightMode::Guided, _) => {
            (tr("Guided Upright not kept").to_string(), tr("Guided Upright isn't supported; straighten these photos again in Transform.").to_string())
        }
        _ => (
            tr("Upright transform not kept").to_string(),
            tr("The saved Upright transform isn't supported; straighten these photos again in Transform.").to_string(),
        ),
    }
}

/// An import finished with `warnings`: keep them, say so in the log, and open the window unless
/// the import ran unasked (Auto Import, browsing a folder), which only gets the toast.
/// Returns the toast's line.
pub fn record(app: &mut LccApp, imported: usize, warnings: &[ImportWarning], show: bool) -> String {
    let mut report = Report::new(imported, warnings);
    log::warn!("import: {}", report.summary());
    log::info!("import warnings:\n{}", report.text());
    report.open = show;
    let line = report.summary();
    app.import_report = Some(report);
    line
}

pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    matches!(id, OPEN | SAVE).then(|| app.import_report.is_some())
}

pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    match id {
        OPEN => Some(match app.import_report.as_mut() {
            Some(r) => {
                r.open = true;
                Ok(
                    json!({"photos": r.photos(), "groups": r.groups.iter().map(|g| json!({"title": g.title, "photos": g.photos.len()})).collect::<Vec<_>>()}),
                )
            }
            None => Err(tr("No import warnings to show").to_string()),
        }),
        SAVE => Some(save(app, p)),
        _ => None,
    }
}

fn save(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let Some(text) = app.import_report.as_ref().map(Report::text) else { return Err(tr("No import warnings to save").to_string()) };
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => x.to_string(),
        None => {
            let req = PickRequest::save(tr("Save Import Warnings"), tr("Text"), &["txt"], "Import Warnings.txt");
            match crate::pick::ask(app, SAVE, p, "path", req, |_| None) {
                Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => x,
                    None => return Ok(Value::Null),
                },
                Picked::Later => return Ok(Value::Null),
                Picked::Unavailable => return Err("no file dialog on this platform: pass {path}".into()),
            }
        }
    };
    std::fs::write(&path, text).map_err(|e| format!("{path}: {e}"))?;
    Ok(json!({"path": path}))
}

/// Select a group's photos in All Photos (they may be anywhere in the library).
fn select(app: &mut LccApp, ids: Vec<u64>) {
    let ids: Vec<u64> = ids.into_iter().filter(|&id| app.session.catalog.photo(lcc_catalog::PhotoId(id)).is_some()).collect();
    if ids.is_empty() || app.run("library.source", json!({"kind": "all"})).is_err() {
        return;
    }
    let _ = app.run("library.select", json!({"ids": ids}));
}

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    let Some(report) = app.import_report.as_mut().filter(|r| r.open) else { return };
    let t = Tokens::get(ctx);
    let id = egui::Id::new("import-warnings");
    let mut open = true;
    let mut chosen: Option<Vec<u64>> = None;
    let (mut copy, mut save, mut close) = (false, false, false);
    let mut closed = false;
    let shown = egui::Window::new(tr("Import Warnings"))
        .id(id)
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .default_width(560.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .max_height(crate::dialog_frame::max_window_height(ctx))
        .show(ctx, |ui| {
            closed = crate::dialog_frame::card_header(ui, tr("Import Warnings"), Some("import-warnings:close"));
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, id);
            ui.add(egui::Label::new(RichText::new(report.intro()).color(t.text_label)).wrap());
            ui.add_space(6.0);
            layout.body(ui, |ui| {
                for (i, g) in report.groups.iter().enumerate() {
                    let header = format!("{} · {}", g.title, g.photos.len());
                    egui::CollapsingHeader::new(RichText::new(header).strong()).id_salt(("import-warning-group", i)).default_open(i == 0).show(
                        ui,
                        |ui| {
                            ui.add(egui::Label::new(RichText::new(&g.note).color(t.text_dim)).wrap());
                            let ids: Vec<u64> = g.photos.iter().filter_map(|(_, id)| *id).collect();
                            let r = ui.add_enabled(!ids.is_empty(), egui::Button::new(tr("Select These Photos")));
                            register(ui.ctx(), format!("button:importWarnings.select.{i}"), r.rect);
                            if r.clicked() {
                                chosen = Some(ids);
                            }
                            let row = ui.text_style_height(&egui::TextStyle::Body);
                            egui::ScrollArea::vertical().id_salt(("import-warning-paths", i)).max_height(180.0).auto_shrink([false, true]).show_rows(
                                ui,
                                row,
                                g.photos.len(),
                                |ui, range| {
                                    for (path, _) in &g.photos[range] {
                                        ui.add(egui::Label::new(RichText::new(short_path(path)).color(t.text_dim).small()).truncate())
                                            .on_hover_text(path);
                                    }
                                },
                            );
                        },
                    );
                }
            });
            layout.footer(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    for (bid, label, flag) in [
                        ("importWarnings.copy", "Copy Report", &mut copy),
                        ("importWarnings.save", "Save Report…", &mut save),
                        ("importWarnings.close", "Close", &mut close),
                    ] {
                        let r = ui.add(egui::Button::new(tr(label)).min_size(vec2(0.0, 26.0)));
                        register(ui.ctx(), format!("button:{bid}"), r.rect);
                        *flag |= r.clicked();
                    }
                });
            });
        });
    crate::dialog_frame::ModalLayout::finish(ctx, id, shown.as_ref().map(|w| w.response.rect));
    if closed {
        open = false;
    }
    if copy {
        ctx.copy_text(report.text());
    }
    if !open || close {
        report.open = false;
    }
    if let Some(ids) = chosen {
        select(app, ids);
    }
    if save {
        let _ = app.run(SAVE, json!({}));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_paths_keep_the_file_and_its_folders() {
        assert_eq!(super::short_path("/Volumes/Photography/Photo Container/2026/10-09 - Duluth/DSC_0675.NEF"), "…/2026/10-09 - Duluth/DSC_0675.NEF");
        assert_eq!(super::short_path(r"D:\Photos\2026\a.nef"), "…/Photos/2026/a.nef");
        assert_eq!(super::short_path("/a/b.png"), "/a/b.png");
    }
}
