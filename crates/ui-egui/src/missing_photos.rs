//! File ▸ Missing Photos…: every photo whose original isn't where the library expects it, in one window, with
//! the path it may be at now (part of issue #2356). Replace turns the beginning of the old paths into a new one
//! (another drive letter, a Windows library opened on Linux); each row's new path can also be typed. The paths
//! proposed are checked on a worker thread (`check_relinks`: is a file there, of the size on record, not another
//! photo's), and Relink relinks the ticked ones with `library.findMissing {found}`, one undo step.
//!
//! Over the control channel the same is `library.missing`, `library.checkRelinks {from, to}` and
//! `library.findMissing {found}`; `file.missingPhotos` opens the window.

use std::path::Path;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::cmd::missing::{FindCandidate, RelinkCheck, RelinkStatus, check_relinks, find_candidates, real_relink_io, replace_prefix};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::{Dialog, MissingRow, MissingStatus};

/// The row in the activity stack while the window looks at every photo's file.
const LIST: &str = "Missing Photos";
/// … and while it checks the paths proposed.
const CHECK: &str = "Check Missing Photos";

/// Open the window and look for the missing photos on a worker thread (every file is looked at: on a network
/// share that takes a while).
pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    if app.tasks.is_running(LIST) {
        return Err("Missing Photos is already looking for missing photos".into());
    }
    app.ui.dialog = Some(Dialog::MissingPhotos { rows: None, from: String::new(), to: String::new() });
    let candidates = find_candidates(&app.session.catalog);
    let library = app.session.library_generation();
    let work = move || candidates.into_iter().filter(|c| !Path::new(&c.path).is_file()).collect::<Vec<_>>();
    crate::tasks::spawn(app, LIST, Some("missingPhotos"), work, move |app, _ctx, missing| listed(app, library, &missing))?;
    Ok(json!({"open": true}))
}

/// The missing photos found: the window's rows, and Replace's beginning suggested from them.
fn listed(app: &mut LightcraftApp, library: u64, missing: &[FindCandidate]) {
    if app.session.library_generation() != library {
        return;
    }
    let Some(Dialog::MissingPhotos { rows, from, .. }) = app.ui.dialog.as_mut() else { return };
    *rows = Some(missing.iter().map(|c| MissingRow { id: c.id.0, path: c.path.clone(), ..Default::default() }).collect());
    if from.is_empty() {
        *from = common_folder(missing.iter().map(|c| c.path.as_str()));
    }
}

/// The deepest folder all `paths` are in (`D:\Photos` for `D:\Photos\2020\a.jpg` and `D:\Photos\b.jpg`), or ""
/// when they share none; written with the first path's separator.
pub fn common_folder<'a>(paths: impl IntoIterator<Item = &'a str>) -> String {
    let mut paths = paths.into_iter();
    let Some(first) = paths.next() else { return String::new() };
    let folders = |p: &'a str| -> Vec<&'a str> {
        let mut parts: Vec<&str> = p.split(['/', '\\']).collect();
        parts.pop();
        parts
    };
    let mut common = folders(first);
    for p in paths {
        let other = folders(p);
        let same = common.iter().zip(&other).take_while(|(a, b)| a == b || (a.ends_with(':') && a.eq_ignore_ascii_case(b))).count();
        common.truncate(same);
    }
    let sep = if first.contains('\\') && !first.contains('/') { "\\" } else { "/" };
    match common.as_slice() {
        [] | [""] => String::new(),
        _ => common.join(sep),
    }
}

/// Replace: the new path of every row that begins with the window's `from` becomes `to` and what follows; then
/// they are checked.
pub fn apply_replace(app: &mut LightcraftApp) -> Result<usize, String> {
    let Some(Dialog::MissingPhotos { rows: Some(rows), from, to }) = app.ui.dialog.as_mut() else { return Err("Missing Photos isn't open".into()) };
    let (from, to) = (from.trim().to_string(), to.trim().to_string());
    if from.is_empty() || to.is_empty() {
        return Err("Say which beginning of the paths to replace, and with what".into());
    }
    let mut changed = 0;
    for r in rows.iter_mut() {
        if let Some(new) = replace_prefix(&r.path, &from, &to) {
            (r.new_path, r.status, r.chosen) = (new, None, false);
            changed += 1;
        }
    }
    if changed == 0 {
        return Err(crate::i18n::tr_format!("No missing photo's path begins with {from}", from = from));
    }
    check(app)?;
    Ok(changed)
}

/// Check every row whose new path isn't checked yet, on a worker thread.
pub fn check(app: &mut LightcraftApp) -> Result<(), String> {
    if app.tasks.is_running(CHECK) {
        return Err("Missing Photos is already checking".into());
    }
    let Some(Dialog::MissingPhotos { rows: Some(rows), .. }) = app.ui.dialog.as_mut() else { return Err("Missing Photos isn't open".into()) };
    let mut proposed = Vec::new();
    for r in rows.iter_mut().filter(|r| r.status.is_none() && !r.new_path.trim().is_empty()) {
        r.new_path = r.new_path.trim().to_string();
        r.status = Some(MissingStatus::Checking);
        proposed.push((PhotoId(r.id), r.new_path.clone()));
    }
    if proposed.is_empty() {
        return Ok(());
    }
    let candidates = find_candidates(&app.session.catalog);
    let library = app.session.library_generation();
    let work = move || check_relinks(&candidates, &proposed, &real_relink_io());
    crate::tasks::spawn(app, CHECK, None, work, move |app, ctx, checks| checked(app, ctx, library, &checks))
}

/// A check's answers, onto the rows whose new path is still the one checked (an edit meanwhile waits for the
/// next check); found ones are ticked.
fn checked(app: &mut LightcraftApp, ctx: &egui::Context, library: u64, checks: &[RelinkCheck]) {
    if app.session.library_generation() != library {
        return;
    }
    let Some(Dialog::MissingPhotos { rows: Some(rows), .. }) = app.ui.dialog.as_mut() else { return };
    for c in checks {
        if let Some(r) = rows.iter_mut().find(|r| r.id == c.id.0 && r.new_path == c.to) {
            r.status = Some(status(c.status));
            r.chosen = c.status == RelinkStatus::Found;
        }
    }
    // rows left checking had their photo removed meanwhile
    for r in rows.iter_mut().filter(|r| r.status == Some(MissingStatus::Checking) && checks.iter().all(|c| c.id.0 != r.id)) {
        r.status = None;
    }
    let found = checks.iter().filter(|c| c.status == RelinkStatus::Found).count();
    let message = crate::i18n::tr_format!("Missing Photos: {found} of {count} found", found = found, count = checks.len());
    app.toast_for(ctx, message, 4.0);
}

fn status(s: RelinkStatus) -> MissingStatus {
    match s {
        RelinkStatus::Found => MissingStatus::Found,
        RelinkStatus::Absent => MissingStatus::Absent,
        RelinkStatus::OtherSize => MissingStatus::OtherSize,
        RelinkStatus::InUse => MissingStatus::InUse,
        RelinkStatus::NotMissing => MissingStatus::NotMissing,
    }
}

/// Whether a row can be ticked: a file is there that isn't another photo's (one of another size only on purpose).
fn can_relink(r: &MissingRow) -> bool {
    matches!(r.status, Some(MissingStatus::Found | MissingStatus::OtherSize))
}

/// The ticked rows the window's Relink would relink.
pub fn chosen(dlg: &Dialog) -> usize {
    match dlg {
        Dialog::MissingPhotos { rows: Some(rows), .. } => rows.iter().filter(|r| r.chosen && can_relink(r)).count(),
        _ => 0,
    }
}

/// Relink: the ticked rows, as one undo step (`library.findMissing` checks again that each photo is still missing
/// and its new file isn't another photo's).
pub fn confirm(app: &mut LightcraftApp, dlg: &Dialog) -> Result<Value, String> {
    let Dialog::MissingPhotos { rows: Some(rows), .. } = dlg else { return Err("Missing Photos is still looking".into()) };
    let found: Vec<Value> =
        rows.iter().filter(|r| r.chosen && can_relink(r)).map(|r| json!({"id": r.id, "from": r.path, "to": r.new_path, "by": "chosen"})).collect();
    if found.is_empty() {
        return Err("Tick the photos to relink".into());
    }
    app.run("library.findMissing", json!({"found": found, "missing": 0, "ambiguous": []}))
}

/// What the window's Relink says once done.
pub fn done_message(r: &Value) -> String {
    let n = r.get("found").and_then(Value::as_array).map_or(0, Vec::len);
    if n == 1 { crate::i18n::tr("Relinked 1 photo").to_string() } else { crate::i18n::tr_format!("Relinked {count} photos", count = n) }
}

/// The text a row's status shows (always beside its colour, which isn't the only sign).
fn status_text(s: Option<MissingStatus>) -> &'static str {
    match s {
        None => "",
        Some(MissingStatus::Checking) => "Checking…",
        Some(MissingStatus::Found) => "Found",
        Some(MissingStatus::Absent) => "Not there",
        Some(MissingStatus::OtherSize) => "Other size",
        Some(MissingStatus::InUse) => "Another photo's",
        Some(MissingStatus::NotMissing) => "Back in place",
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn folder(path: &str) -> &str {
    let name = file_name(path);
    path[..path.len() - name.len()].trim_end_matches(['/', '\\'])
}

const ROW_H: f32 = 24.0;

pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, dlg: &mut Dialog) {
    let tr = crate::i18n::tr;
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut replace = false;
    let mut recheck = false;
    {
        let Dialog::MissingPhotos { rows, from, to } = dlg else { return };
        let Some(rows) = rows else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr("Looking for missing photos…"));
            });
            return;
        };
        if rows.is_empty() {
            ui.label(tr("No photos are missing: every original is where the library expects it."));
            return;
        }
        let n = rows.len();
        ui.add(
            egui::Label::new(crate::i18n::tr_format!(
                "{count} photo{} can't be found where the library expects them. Replace the beginning of their paths (another drive letter, another computer), or type a photo's new path, then relink the ones found.",
                if n == 1 { "" } else { "s" },
                count = n
            ))
            .wrap(),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr("Replace")).color(t.text_label));
            let w = ((ui.available_width() - 180.0) / 2.0).max(120.0);
            crate::text_field::TextField::singleline("field:missingPhotosFrom", from).label(tr("Replace")).width(w).show(ui);
            ui.label(egui::RichText::new(tr("with")).color(t.text_label));
            let r = crate::text_field::TextField::singleline("field:missingPhotosTo", to).label(tr("with")).width(w).show(ui);
            let ready = !from.trim().is_empty() && !to.trim().is_empty();
            let enter = r.ending == Some(crate::text_field::Ending::Return);
            let apply = ui.add_enabled_ui(ready, |ui| crate::widgets::text_button(ui, "missingPhotosApply", tr("Apply"), false)).inner;
            replace = ready && (apply.clicked() || enter);
        });
        ui.add_space(6.0);
        // the table: a header, then the rows (only the visible ones are laid out: a library can miss thousands)
        let name_w = 170.0;
        let status_w = 110.0;
        let path_w = ((ui.available_width() - name_w - status_w - 24.0) / 2.0).max(140.0);
        let header = |ui: &mut egui::Ui, text: &str, w: f32| {
            ui.allocate_ui_with_layout(egui::vec2(w, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_width(w);
                ui.label(egui::RichText::new(text).color(t.text_label).strong());
            });
        };
        ui.horizontal(|ui| {
            header(ui, tr("Photo"), name_w);
            header(ui, tr("Was in"), path_w);
            header(ui, tr("New path"), path_w);
            header(ui, tr("Status"), status_w);
        });
        egui::ScrollArea::vertical().id_salt("missingPhotosRows").max_height(360.0).auto_shrink([false, true]).show_rows(
            ui,
            ROW_H,
            n,
            |ui, range| {
                for r in &mut rows[range] {
                    ui.horizontal(|ui| {
                        ui.set_height(ROW_H);
                        let name = file_name(&r.path).to_string();
                        ui.allocate_ui_with_layout(egui::vec2(name_w, ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(name_w);
                            let c = ui.add_enabled(can_relink(r), egui::Checkbox::new(&mut r.chosen, name.as_str()));
                            crate::widgets::register(ui, format!("check:missingPhoto-{}", r.id), c.rect);
                        });
                        ui.allocate_ui_with_layout(egui::vec2(path_w, ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(path_w);
                            ui.add(egui::Label::new(egui::RichText::new(folder(&r.path)).color(t.text_dim)).truncate())
                                .on_hover_text(r.path.as_str());
                        });
                        let widget = format!("field:missingPhotoPath-{}", r.id);
                        let before = r.new_path.clone();
                        crate::text_field::TextField::singleline(&widget, &mut r.new_path)
                            .label(crate::i18n::tr_format!("New path, {name}", name = name))
                            .hint(tr("Not given"))
                            .width(path_w)
                            .show(ui);
                        if r.new_path != before {
                            (r.status, r.chosen) = (None, false);
                        }
                        let (text, colour) = match r.status {
                            Some(MissingStatus::Found) => (status_text(r.status), t.pick),
                            Some(MissingStatus::Absent | MissingStatus::InUse) => (status_text(r.status), t.reject),
                            Some(MissingStatus::OtherSize) => (status_text(r.status), t.caution),
                            None if !r.new_path.trim().is_empty() => ("Not checked", t.text_dim),
                            s => (status_text(s), t.text_dim),
                        };
                        ui.label(egui::RichText::new(tr(text)).color(colour));
                    });
                }
            },
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let unchecked = rows.iter().any(|r| r.status.is_none() && !r.new_path.trim().is_empty());
            recheck = ui.add_enabled_ui(unchecked, |ui| crate::widgets::text_button(ui, "missingPhotosCheck", tr("Check"), false)).inner.clicked();
            let ticked = rows.iter().filter(|r| r.chosen && can_relink(r)).count();
            ui.label(
                egui::RichText::new(crate::i18n::tr_format!("{ticked} of {count} ticked to relink", ticked = ticked, count = n)).color(t.text_dim),
            );
        });
    }
    // (outside the borrow of the window's state: these put it back into the app first)
    if replace || recheck {
        app.ui.dialog = Some(dlg.clone());
        let r = if replace { apply_replace(app).map(|_| ()) } else { check(app) };
        if let Err(e) = r {
            app.toast_for(ui.ctx(), crate::i18n::tr(&e).to_string(), 6.0);
        }
        if let Some(d) = app.ui.dialog.clone() {
            *dlg = d;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_is_suggested_from_the_folder_the_missing_photos_share() {
        assert_eq!(common_folder([r"D:\Photos\2020\a.jpg", r"D:\Photos\b.jpg", r"d:\Photos\2021\x\c.jpg"]), r"D:\Photos");
        assert_eq!(common_folder(["/Volumes/Disk/2020/a.jpg", "/Volumes/Disk/2020/b.jpg"]), "/Volumes/Disk/2020");
        assert_eq!(common_folder([r"C:\a.jpg", r"D:\b.jpg"]), "");
        assert_eq!(common_folder(["/a.jpg", "/b.jpg"]), "");
        assert_eq!(common_folder(std::iter::empty()), "");
    }
}
