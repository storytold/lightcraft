use super::import::xmp_sidecars;
use super::performance::local_folders;
#[cfg(not(target_arch = "wasm32"))]
use super::performance::smart_previews;
use super::widgets::{check, choices, heading, hint, path_label, row};
use super::{LightcraftApp, RichText, StartupView, Tokens, json, register};

// ------------------------------------------------------------------------------------- General

/// Library: the library itself. Where it is and opening another ([`library_section`]), where
/// its smart previews are kept ([`smart_previews`], for a library on disk), and what leaves its
/// catalog by itself ([`local_folders`]).
pub(super) fn library_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    library_section(app, ui, t);
    #[cfg(not(target_arch = "wasm32"))]
    smart_previews(app, ui, t);
    local_folders(app, ui, t);
    in_memory_note(app, ui, t);
}

/// Files: what LightCraft writes beside the photo files ([`xmp_sidecars`]) and what it opens
/// them in ([`external_editing`]). External Editing is saved with the interface settings, so it
/// comes first and the in-memory note, which is about the library's settings, ends the tab.
pub(super) fn files_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    external_editing(app, ui, t);
    xmp_sidecars(app, ui, t);
    in_memory_note(app, ui, t);
}

/// What is saved with the library lasts only as long as a library that is not on disk does:
/// said at the foot of a tab whose last settings are such (Import says it at its foot itself).
/// It must not follow settings saved elsewhere (those in ui.json last).
pub(super) fn in_memory_note(app: &LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.session.library.is_none() {
        hint(ui, t, "In-memory session: the library's settings last until LightCraft quits.");
    }
}

pub(super) fn general_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    row(ui, t, crate::i18n::tr("Language"), |ui| {
        let languages: Vec<_> = crate::i18n::Locale::for_picker().into_iter().map(|language| (language, language.name())).collect();
        // Thirteen language names do not fit one line in a narrow window: wrap them.
        ui.horizontal_wrapped(|ui| {
            choices(ui, "settingsLanguage", &languages, &mut app.ui.language);
        });
        crate::i18n::set_language(app.ui.language);
    });
    startup_and_culling(app, ui, t);
}

/// Library: where the open library is, and opening or making another.
fn library_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, "Library");
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
}

/// Startup and Culling: what LightCraft opens in, and how rating and deleting behave.
fn startup_and_culling(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, "Startup");
    row(ui, t, crate::i18n::tr("Open in"), |ui| {
        choices(
            ui,
            "settingsStartup",
            &[(StartupView::Last, "Last view"), (StartupView::Grid, "Photo Grid"), (StartupView::Detail, "Detail View")],
            &mut app.ui.settings.startup_view,
        );
    });
    heading(ui, t, "Culling");
    check(ui, "settings.autoAdvance", &mut app.ui.auto_advance, "Auto Advance: move to the next photo after rating or flagging");
    check(ui, "settings.confirmDelete", &mut app.ui.settings.confirm_delete, "Confirm before moving photos to Recently Deleted");
    check(ui, "settings.askBeforeMove", &mut app.ui.settings.ask_before_move, "Confirm before moving photos to another folder on disk");
    heading(ui, t, "Dialogs");
    check(ui, "settings.closeDialogsOutside", &mut app.ui.settings.close_dialogs_outside, "Close a dialog by clicking outside it");
}

/// External Editing: the editors a photo is opened in, and the copy they are given.
fn external_editing(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, "External Editing");
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
