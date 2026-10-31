use super::frame::{just_opened, list_height};
use super::{LightcraftApp, Tokens, json};

/// When a backup was made, as its row says it: the time it says itself (UTC), else its
/// folder's name (which is the time too, unless the folder was renamed).
fn backup_when(b: &serde_json::Value) -> String {
    match b["made"].as_str().filter(|made| made.len() == 19 && made.contains('T')) {
        Some(made) => format!("{} UTC", made.replace('T', " ")),
        None => b["name"].as_str().unwrap_or_default().to_string(),
    }
}

/// Restore Library from Backup: the backups in `dir`, newest first, one of them chosen. True
/// when Choose Folder… was pressed (for a backup, or a folder of them, that is elsewhere).
pub(super) fn restore_backup_body(
    app: &LightcraftApp,
    ui: &mut egui::Ui,
    t: &Tokens,
    dir: &str,
    backups: &[serde_json::Value],
    selected: &mut Option<usize>,
) -> bool {
    ui.set_min_width(460.0);
    let opened = just_opened(ui, "restore-backup");
    ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr_format!("Backups in {}", dir)).color(t.text_dim)).wrap());
    ui.add_space(4.0);
    if backups.is_empty() {
        ui.label(crate::i18n::tr("There are no backups in this folder."));
    }
    egui::ScrollArea::vertical().max_height(list_height(ui, 260.0)).auto_shrink([false, true]).show(ui, |ui| {
        // (rows as wide as the list, their text at the left)
        let rows = egui::Layout::top_down_justified(egui::Align::LEFT);
        ui.with_layout(rows, |ui| {
            for (i, b) in backups.iter().enumerate() {
                let mut text = format!(
                    "{}   ·   {}   ·   {}",
                    backup_when(b),
                    crate::i18n::tr_format!("{} photos", b["photos"].as_u64().unwrap_or(0)),
                    crate::panels::right::human_size(b["bytes"].as_u64().unwrap_or(0))
                );
                if b["ofThisLibrary"] != serde_json::Value::Bool(true) {
                    text = format!("{text}   ·   {}", crate::i18n::tr("another library"));
                }
                let r = ui.add(egui::Button::selectable(*selected == Some(i), text).min_size(egui::vec2(0.0, 22.0)));
                // (the backup that Restore would take is in sight when the list opens)
                if opened && *selected == Some(i) {
                    r.scroll_to_me(Some(egui::Align::Center));
                }
                crate::widgets::register(ui, format!("row:restoreBackup-{i}"), r.rect);
                if r.clicked() {
                    *selected = Some(i);
                }
            }
        });
    });
    ui.add_space(6.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(crate::i18n::tr(
                "The library becomes what it was when the backup was made. What it holds now is kept in Replaced in the library folder.",
            ))
            .color(t.text_dim),
        )
        .wrap(),
    );
    let mut choose = false;
    // (a backup kept elsewhere: where a folder can be chosen)
    if app.services.pick_folder.is_some() || app.services.picker.is_some() {
        ui.add_space(4.0);
        let r = ui.button(crate::i18n::tr("Choose Folder…"));
        crate::widgets::register(ui, "button:restoreChooseFolder", r.rect);
        choose = r.clicked();
    }
    choose
}

/// Run `library.restore` for the folder chosen with Choose Folder…: whether the dialog closes
/// (a backup was chosen and its restore has begun).
pub(super) fn choose_backup_folder(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    match app.run("library.restore", json!({"choose": true})) {
        // (nothing chosen, or a folder of backups, which are now the ones listed)
        Ok(serde_json::Value::Null) => false,
        // a backup was chosen: its restore has begun
        Ok(_) => true,
        Err(e) => {
            app.toast(ctx, e);
            false
        }
    }
}

/// The list of backups after a restore was refused: refreshed, as the folder now is (the
/// backup may be gone since), with the chosen backup kept chosen where it is still there.
pub(super) fn refresh_backups(app: &mut LightcraftApp, dlg: &mut crate::state::Dialog) {
    if let crate::state::Dialog::RestoreBackup { dir, backups, selected } = dlg
        && let Ok(listed) = app.session.execute("library.backups", &json!({"dir": dir.as_str()}))
    {
        let chosen = selected.and_then(|i| backups.get(i)).map(|b| b["dir"].clone());
        *backups = crate::menus::backups_to_choose_from(&listed);
        *selected = chosen.and_then(|dir| backups.iter().position(|b| b["dir"] == dir));
    }
}
