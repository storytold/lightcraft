use super::widgets::{check, choices, heading, hint, path_label, row};
use super::{LightcraftApp, RichText, StartupView, Tokens, json, register};

// ------------------------------------------------------------------------------------- General

pub(super) fn general_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    row(ui, t, crate::i18n::tr("Language"), |ui| {
        let languages: Vec<_> = crate::i18n::Locale::for_picker().into_iter().map(|language| (language, language.name())).collect();
        // Thirteen language names do not fit one line in a narrow window: wrap them.
        ui.horizontal_wrapped(|ui| {
            choices(ui, "settingsLanguage", &languages, &mut app.ui.language);
        });
        crate::i18n::set_language(app.ui.language);
    });
    heading(ui, t, crate::i18n::tr("Library"));
    let location = match &app.session.library {
        Some(l) => l.dir.display().to_string(),
        None => crate::i18n::tr("In memory — nothing is saved").to_string(),
    };
    row(ui, t, crate::i18n::tr("Location"), |ui| {
        path_label(ui, "settingsLocation", &location, egui::TextStyle::Body.resolve(ui.style()), t.text, 0.0);
    });
    row(ui, t, "", |ui| {
        let can = app.services.pick_folder.is_some();
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Open Library…")));
        register(ui, "button:settingsOpenLibrary", r.rect);
        if r.clicked() {
            let _ = app.run("app.openLibrary", json!({}));
        }
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("New Library…")));
        register(ui, "button:settingsNewLibrary", r.rect);
        if r.clicked() {
            let _ = app.run("app.newLibrary", json!({}));
        }
        if !can {
            hint(ui, t, crate::i18n::tr("not available here"));
        }
    });
    // where libraries are folders to choose from (not in the browser, which keeps one)
    if app.services.pick_folder.is_some() {
        use crate::state::LaunchLibrary;
        row(ui, t, crate::i18n::tr("At launch"), |ui| {
            choices(
                ui,
                "settingsAtLaunch",
                &[(LaunchLibrary::Last, "Open the last library"), (LaunchLibrary::Ask, "Ask me")],
                &mut app.ui.settings.at_launch,
            );
        });
        hint(ui, t, crate::i18n::tr("Ask me lists your recent libraries at launch. Holding Alt (Option on a Mac) as the window appears asks once."));
    }
    // File ▸ Import Lightroom Catalog… (#2672): the window that asks where a catalog goes, or not
    ui.label(RichText::new(crate::i18n::tr("When importing a Lightroom catalog")).color(t.text_label));
    row(ui, t, "", |ui| {
        use crate::state::LightroomImport;
        choices(
            ui,
            "settingsLightroomImport",
            &[(LightroomImport::Ask, "Ask where to import it"), (LightroomImport::AlwaysMerge, "Always merge into the open library")],
            &mut app.ui.settings.lightroom_import,
        );
    });
    backups(app, ui, t);
    heading(ui, t, crate::i18n::tr("Startup"));
    row(ui, t, crate::i18n::tr("Open in"), |ui| {
        choices(
            ui,
            "settingsStartup",
            &[(StartupView::Last, "Last view"), (StartupView::Grid, "Photo Grid"), (StartupView::Detail, "Detail View")],
            &mut app.ui.settings.startup_view,
        );
    });
    heading(ui, t, crate::i18n::tr("Culling"));
    check(ui, "settings.autoAdvance", &mut app.ui.auto_advance, "Auto Advance: move to the next photo after rating or flagging");
    check(ui, "settings.confirmDelete", &mut app.ui.settings.confirm_delete, "Confirm before moving photos to Recently Deleted");
    check(ui, "settings.askBeforeMove", &mut app.ui.settings.ask_before_move, "Confirm before moving photos to another folder on disk");
    heading(ui, t, crate::i18n::tr("Dialogs"));
    check(ui, "settings.closeDialogsOutside", &mut app.ui.settings.close_dialogs_outside, "Close a dialog by clicking outside it");
    heading(ui, t, crate::i18n::tr("External Editing"));
    row(ui, t, crate::i18n::tr("PhotoCraft"), |ui| {
        crate::text_field::TextField::singleline("field:photocraftPath", &mut app.ui.settings.photocraft_path)
            .hint(crate::i18n::tr("Found automatically"))
            .width(220.0)
            .show(ui);
    });
    edit_file_rows(ui, t, "photocraftFile", &mut app.ui.settings.photocraft_file);
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photo ▸ Edit In ▸ Edit in PhotoCraft (⇧⌘E, or right-click a photo) renders a copy with your edits, stacks it with the original and opens it in PhotoCraft. Save there and the copy updates here. As PSD, PhotoCraft saves straight back to it. Leave the location empty unless you use a portable copy.",
        ),
    );
    row(ui, t, crate::i18n::tr("Other editor"), |ui| {
        crate::text_field::TextField::singleline("field:externalEditor", &mut app.ui.settings.external_editor)
            .hint(crate::i18n::tr("System default"))
            .width(220.0)
            .show(ui);
    });
    edit_file_rows(ui, t, "externalFile", &mut app.ui.settings.external_file);
    hint(ui, t, crate::i18n::tr("Edit in Other Editor (⌥⌘E) opens the copy in this application (an app name on macOS, a program path elsewhere)."));
    hint(
        ui,
        t,
        crate::i18n::tr(
            "To hand this editor the original file itself instead (the raw, without a copy: DxO PhotoLab, for example), right-click a photo → Edit In → Open Original in Other Editor (RAW, no copy).",
        ),
    );
}

/// What Settings found out about the folder backups go to, and what it was found out for (the
/// library, the folder, the last backup and whether one is under way) on which frame.
#[derive(Clone)]
struct BackupLook {
    pass: u64,
    of: (String, Option<String>, String, Option<String>, bool),
    shares_the_drive: Option<bool>,
    /// Whether the folder is there: `None` when that can't be told (a folder that can't be read
    /// is not a folder that is not there).
    there: Option<bool>,
    holds_a_backup: bool,
    /// The newest backup of this library in the folder: when it says it was made, or its name.
    newest: Option<String>,
}

/// Where the library's backups go (`library.backupPreferences`): shown, chosen with a folder
/// dialog, and put back to Backups in the library folder. Only for a library that is a folder on
/// disk: nothing else is backed up to a folder.
fn backups(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    const ID: &str = "library.backupPreferences";
    // (read, not asked with the command: this is every frame, and a command is journaled)
    let Some((folder, chosen)) = lightcraft_engine::cmd::backup::backup_folder(&app.session) else { return };
    let folder = folder.display().to_string();
    heading(ui, t, crate::i18n::tr("Backups"));
    // What the folder is like is read from the file system: what volume it is on, whether it is
    // there, whether it holds a backup of this library. It is looked at when this section is
    // shown anew, when the library or the folder changes and when a backup starts or ends, not
    // on a timer: a sleeping or missing drive is not asked again and again.
    let running = lightcraft_engine::cmd::backup::status(&app.session).is_some_and(|job| !job.finished.load(std::sync::atomic::Ordering::Relaxed));
    let look_id = egui::Id::unique("backup-folder-look");
    let pass = ui.ctx().cumulative_pass_nr();
    let library = app.session.library.as_ref().map(|l| l.dir.display().to_string()).unwrap_or_default();
    // (the library's id too: a restore puts another library where this one was, and a backup is
    // of the open library by its id)
    let of = (library, app.session.library_id.clone(), folder.clone(), app.session.last_backup.clone(), running);
    let kept: Option<BackupLook> = ui.data(|d| d.get_temp(look_id));
    let mut look = match kept.filter(|k| k.pass.checked_add(1) == Some(pass) && k.of == of) {
        Some(kept) => kept,
        None => {
            // refusal expected: a folder that cannot be listed holds no backup we know of
            // (the list is newest first)
            let newest = app.run("library.backups", json!({})).ok().and_then(|list| {
                let of_this = list["backups"].as_array()?.iter().find(|b| b["ofThisLibrary"] == true)?;
                Some(
                    of_this["made"]
                        .as_str()
                        .map(|made| format!("{} UTC", crate::i18n::display_time(made)))
                        .or_else(|| of_this["name"].as_str().map(str::to_string)),
                )
            });
            BackupLook {
                shares_the_drive: lightcraft_engine::cmd::backup::backup_shares_the_drive(&app.session),
                there: std::path::Path::new(&folder).try_exists().ok(),
                holds_a_backup: newest.is_some(),
                newest: newest.flatten(),
                of,
                pass,
            }
        }
    };
    look.pass = pass;
    ui.data_mut(|d| d.insert_temp(look_id, look.clone()));
    // (a chosen folder that is not there is said to be just that: nothing is known of its drive)
    let away = chosen && look.there == Some(false);
    let mut result = None;
    // Where the library stands comes first, in the size and colour of what a row says, with a
    // sign before it (so it is not told by a colour alone) and Back Up Now beside it: no backup
    // of this library in the folder and nothing set to make one, a folder that is not there, a
    // backup being made, or when the last one was made.
    let schedule = app.session.backup_schedule;
    let unset = schedule.every == lightcraft_engine::backup_schedule::BackupEvery::Never
        && schedule.on_close == lightcraft_engine::backup_schedule::BackupOnClose::Never;
    // (the tick is for a backup that is in the folder now, not for one that was made once: a
    // folder moved away or emptied since holds none, whatever the session remembers)
    let last = crate::i18n::tr("Last backup");
    let (sign, says) = if running {
        (None, crate::i18n::tr("Backing up…").to_string())
    } else if away {
        (
            Some(false),
            crate::i18n::tr("This folder is not there now. If it is on a drive that is not connected, connect it before backing up.").to_string(),
        )
    } else if look.holds_a_backup {
        // (when the newest one says it was made, by the session's clock, which is UTC; a backup
        // from before backups said so is told by its name)
        (Some(true), look.newest.as_ref().map_or_else(|| last.to_string(), |when| format!("{last}: {when}")))
    } else if unset {
        (Some(false), crate::i18n::tr("No backup of this library is in this folder, and nothing is set to make one.").to_string())
    } else if app.session.last_backup.is_some() {
        // (one was made and is gone; the next is made when it is due)
        (Some(false), crate::i18n::tr("No backup of this library is in this folder.").to_string())
    } else {
        (None, format!("{last}: {}", crate::i18n::tr("Not yet")))
    };
    // (why a backup cannot be made right now, if it cannot: one or a restore is under way)
    let refused = lightcraft_engine::cmd::backup::can_back_up(&app.session).err();
    ui.horizontal(|ui| {
        let (at, _) = ui.allocate_exact_size(egui::vec2(16.0, 20.0), egui::Sense::hover());
        let at = egui::Rect::from_center_size(at.center(), egui::vec2(16.0, 16.0));
        match sign {
            Some(false) => {
                crate::icons::paint(ui.painter(), at, crate::icons::Icon::Info, t.caution);
                register(ui, "icon:settingsBackupStatus:caution", at);
            }
            Some(true) => {
                crate::icons::paint(ui.painter(), at, crate::icons::Icon::Check, t.text_label);
                register(ui, "icon:settingsBackupStatus:ok", at);
            }
            None => {}
        }
        // (the button keeps its place at the end of the line; the words wrap in what is left)
        let button = crate::i18n::tr("Back Up Now");
        let button_w = ui.painter().layout_no_wrap(button.to_string(), egui::TextStyle::Button.resolve(ui.style()), t.text).size().x
            + 2.0 * ui.spacing().button_padding.x;
        let words = (ui.available_width() - button_w - ui.spacing().item_spacing.x).max(80.0);
        let r = ui
            .allocate_ui_with_layout(egui::vec2(words, 20.0), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                ui.add(egui::Label::new(RichText::new(says).color(t.text)).wrap())
            })
            .inner;
        register(ui, "text:settingsBackupStatus", r.rect);
        // (not on offer while it would be refused)
        let r = ui.add_enabled(refused.is_none(), egui::Button::new(button));
        register(ui, "button:settingsBackupNow", r.rect);
        if r.clicked() {
            result = Some(app.run("library.backup", json!({"background": true})));
        }
    });
    if let Some(why) = refused.as_ref().filter(|_| !running) {
        hint(ui, t, why);
    }
    row(ui, t, crate::i18n::tr("Folder"), |ui| {
        path_label(ui, "settingsBackupFolder", &folder, egui::TextStyle::Body.resolve(ui.style()), t.text, 0.0);
    });
    if !chosen {
        row(ui, t, "", |ui| hint(ui, t, crate::i18n::tr("In the library folder")));
    }
    // A backup on the library's own drive goes with it when that drive fails, which is the day a
    // backup is for. The usual folder is such a place, and is said to be in the grey of a hint;
    // a folder someone chose that turns out to be on the same drive is said in the caution colour.
    let aside = |ui: &mut egui::Ui, text: &str, color: egui::Color32| {
        row(ui, t, "", |ui| {
            ui.add(egui::Label::new(RichText::new(crate::i18n::tr(text)).size(11.0).color(color)).wrap());
        });
    };
    if !away && look.shares_the_drive == Some(true) {
        let text = "On the same drive as the library: a backup here is lost with the library if that drive fails.";
        aside(ui, text, if chosen { t.caution } else { t.text_dim });
    }
    row(ui, t, "", |ui| {
        let can = app.services.picker.is_some() || app.services.pick_folder.is_some();
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Choose Folder…")));
        register(ui, "button:settingsBackupChoose", r.rect);
        if r.clicked() {
            result = Some(app.run(ID, json!({"choose": true})));
        }
        let r = ui.add_enabled(chosen, egui::Button::new(crate::i18n::tr("Use Library Folder")));
        register(ui, "button:settingsBackupReset", r.rect);
        if r.clicked() {
            result = Some(app.run(ID, json!({"dir": null})));
        }
        if !can {
            hint(ui, t, crate::i18n::tr("not available here"));
        }
    });
    // when the library is backed up without being asked: two choices of their own
    use lightcraft_engine::backup_schedule::{BackupEvery, BackupOnClose};
    let mut schedule = schedule;
    row(ui, t, crate::i18n::tr("While open"), |ui| {
        let every = [
            (BackupEvery::Never, "Never"),
            (BackupEvery::Day, "Once a day"),
            (BackupEvery::Week, "Once a week"),
            (BackupEvery::Month, "Once a month"),
        ];
        if choices(ui, "settingsBackupEvery", &every, &mut schedule.every) {
            result = Some(app.run(ID, json!({"every": schedule.every.id()})));
        }
    });
    row(ui, t, crate::i18n::tr("When quitting"), |ui| {
        let on_close = [(BackupOnClose::Never, "Never"), (BackupOnClose::Ask, "Ask"), (BackupOnClose::Always, "Always")];
        if choices(ui, "settingsBackupOnClose", &on_close, &mut schedule.on_close) {
            result = Some(app.run(ID, json!({"onClose": schedule.on_close.id()})));
        }
    });
    // how long backups are kept: forever unless chosen (an agent may have set another time)
    let mut keep = app.session.backup_keep_days;
    row(ui, t, crate::i18n::tr("Keep backups"), |ui| {
        let ages = [(None, "Forever"), (Some(30), "1 month"), (Some(90), "3 months"), (Some(180), "6 months"), (Some(365), "1 year")];
        if choices(ui, "settingsBackupKeep", &ages, &mut keep) {
            result = Some(app.run(ID, json!({"keepDays": keep})));
        }
        if let Some(days) = keep.filter(|days| !ages.iter().any(|(age, _)| *age == Some(*days))) {
            hint(ui, t, &format!("{}: {days}", crate::i18n::tr("Days")));
        }
    });

    // (a folder or a choice that was refused: why. What was set shows in the rows above.)
    if let Some(Err(e)) = result {
        app.toast(ui.ctx(), crate::i18n::refusal_text(&e));
    }
    hint(
        ui,
        t,
        crate::i18n::tr("A backup holds the catalog, not the photos. Keep it on another drive than the library, so that it outlives that drive."),
    );
    if keep.is_some() {
        hint(ui, t, crate::i18n::tr("Older backups of this library are removed after a new one is made. Its newest three are always kept."));
        hint(ui, t, crate::i18n::tr("What restores replaced (Replaced in the library folder) goes by the same rule."));
    }
}

/// The file Edit In hands an editor: format, colour space and bit depth (`button:{id}Format-…`).
fn edit_file_rows(ui: &mut egui::Ui, t: &Tokens, id: &str, file: &mut crate::state::EditFile) {
    use crate::state::{EditFormat, EditSpace};
    row(ui, t, "", |ui| {
        choices(ui, &format!("{id}Format"), &[(EditFormat::Psd, "PSD"), (EditFormat::Tiff, "TIFF")], &mut file.format);
        ui.separator();
        choices(ui, &format!("{id}Depth"), &[(16u8, "16-bit"), (8, "8-bit")], &mut file.bit_depth);
    });
    row(ui, t, "", |ui| {
        choices(
            ui,
            &format!("{id}Space"),
            &[(EditSpace::AdobeRgb, "Adobe RGB"), (EditSpace::ProPhoto, "ProPhoto"), (EditSpace::DisplayP3, "P3"), (EditSpace::Srgb, "sRGB")],
            &mut file.color_space,
        );
    });
}

// -------------------------------------------------------------------------------------- Import

/// A preset picker: `None` = `none_label`. Returns the new choice when it changed.
pub(super) fn preset_combo(
    app: &LightcraftApp,
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    current: Option<&str>,
    none_label: &str,
) -> Option<Option<String>> {
    let name = |pid: &str| {
        app.session
            .presets
            .iter()
            .find(|p| p.id == pid)
            .map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string())
            .unwrap_or_else(|| crate::i18n::tr_format!("{pid} (missing)", pid = pid))
    };
    let text = current.map(name).unwrap_or_else(|| crate::i18n::tr(none_label).to_string());
    let mut out = None;
    let r = crate::widgets::combo_box(id).width(240.0).selected_text(text).show_ui(ui, |ui| {
        if ui.selectable_label(current.is_none(), crate::i18n::tr(none_label)).clicked() {
            out = Some(None);
        }
        if let Some(pid) = crate::panels::presets::picker_rows(ui, id, &app.session.presets, current) {
            out = Some(Some(pid));
        }
    });
    crate::widgets::name_combo(&r.response, label);
    register(ui, format!("combo:{id}"), r.response.rect);
    out.filter(|v| v.as_deref() != current)
}

/// Cameras of the raws in the library plus those with a stored default, sorted.
pub(super) fn cameras(app: &LightcraftApp) -> Vec<String> {
    let mut v: Vec<String> = app
        .session
        .catalog
        .photos()
        .filter(|p| p.kind == lightcraft_catalog::MediaKind::Raw && !p.meta.camera.is_empty())
        .map(|p| p.meta.camera.clone())
        .chain(app.session.import_defaults.cameras.iter().map(|c| c.camera.clone()))
        .collect();
    v.sort_by_key(|c| c.to_lowercase());
    v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    v
}
