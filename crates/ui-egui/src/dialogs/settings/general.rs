use super::widgets::{check, choices, field, heading, hint, path_label, row};
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
    library_launch_section(app, ui, t);
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
        field(
            ui,
            crate::text_field::TextField::singleline("field:photocraftPath", &mut app.ui.settings.photocraft_path)
                .hint(crate::i18n::tr("Found automatically"))
                .width(220.0),
        );
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
        field(
            ui,
            crate::text_field::TextField::singleline("field:externalEditor", &mut app.ui.settings.external_editor)
                .hint(crate::i18n::tr("System default"))
                .width(220.0),
        );
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
    #[cfg(not(target_arch = "wasm32"))]
    lens_profiles(app, ui, t);
}

/// The live General Library/launch section. Host UI state still owns the launch preference.
fn library_launch_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
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
        if library_launch_button(ui, "button:settingsOpenLibrary", "Open Library…", can) {
            let _ = app.run("app.openLibrary", json!({}));
        }
        if library_launch_button(ui, "button:settingsNewLibrary", "New Library…", can) {
            let _ = app.run("app.newLibrary", json!({}));
        }
        if !can {
            hint(ui, t, crate::i18n::tr("not available here"));
        }
    });
    // Preserve the host's existing capability rule, including the whole native Settings route.
    if app.services.pick_folder.is_some() {
        use crate::state::LaunchLibrary;
        row(ui, t, crate::i18n::tr("At launch"), |ui| {
            let mut at_launch = app.ui.settings.at_launch;
            // Measurement/sizing may consume a native button result, but cannot change host UI state.
            if choices(
                ui,
                "settingsAtLaunch",
                &[(LaunchLibrary::Last, "Open the last library"), (LaunchLibrary::Ask, "Ask me")],
                &mut at_launch,
            ) && library_launch_input(ui)
            {
                app.ui.settings.at_launch = at_launch;
            }
        });
        hint(ui, t, crate::i18n::tr("Ask me lists your recent libraries at launch. Holding Alt (Option on a Mac) as the window appears asks once."));
    }
}

fn library_launch_input(ui: &egui::Ui) -> bool {
    ui.is_enabled() && !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx())
}

/// Only General's Open/New controls; their command and picker owners stay unchanged.
fn library_launch_button(ui: &mut egui::Ui, alias: &str, label: &str, can: bool) -> bool {
    use craft::ui::contract::{Props as _, Show as _};
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let native = super::native_controls_active(ui.ctx());
    let (response, clicked) = if native {
        let response = ui.add_enabled(can && real, egui::Button::new(crate::i18n::tr(label)));
        let clicked = response.clicked();
        (response, clicked)
    } else {
        let shown = craft::ui::buttons::Button::new(crate::i18n::tr(label).to_owned())
            .id_salt(alias)
            .enabled(can && real)
            .show(ui);
        let clicked = shown.clicked();
        (shown.response, clicked)
    };
    if real {
        crate::widgets::register_response(alias, response.rect, &response);
    }
    #[cfg(test)]
    general_library_launch_flow_tests::observe(ui, alias, &response, native, clicked);
    can && library_launch_input(ui) && clicked
}

#[cfg(test)]
#[path = "general_library_launch_flow_tests.rs"]
mod general_library_launch_flow_tests;

/// Lens profiles: whether the lens database is here, a button that downloads it (nothing is fetched without a
/// click: the first launch offers it once, [`crate::lens_db`]), and the startup offer turned back on or off.
#[cfg(not(target_arch = "wasm32"))]
fn lens_profiles(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use lightcraft_engine::lensfun_download::{self, Offer};
    heading(ui, t, crate::i18n::tr("Lens profiles"));
    let running = app.session.lensfun.running();
    // (the user's own folder, LIGHTCRAFT_LENSFUN_DIR, is read instead and never replaced)
    let own = std::env::var_os(lightcraft_engine::lens_profiles::ENV).is_some_and(|v| !v.is_empty());
    let installed = lensfun_download::default_dir().is_some_and(|d| lensfun_download::installed(&d));
    let status = if own {
        crate::i18n::tr("Lens profiles are read from the folder LIGHTCRAFT_LENSFUN_DIR names.").to_string()
    } else if running {
        crate::i18n::tr("Downloading the lens database…").to_string()
    } else if installed {
        crate::i18n::tr("The lens database is installed.").to_string()
    } else {
        crate::i18n::tr_format!("The lens database is not installed (about {size} MB).", size = lensfun_download::download_megabytes())
    };
    row(ui, t, crate::i18n::tr("Lens database"), |ui| {
        ui.label(RichText::new(status).color(t.text));
        let r = ui.add_enabled(!own && !running && !installed, egui::Button::new(crate::i18n::tr("Download Lens Database")));
        register(ui, "button:settingsLensDatabase", r.rect);
        if r.clicked()
            && let Err(e) = crate::lens_db::download_default(app)
        {
            app.toast(ui.ctx(), e);
        }
    });
    let mut offer = app.ui.settings.lens_database != Offer::Declined;
    if check(ui, "settings.lensDatabaseOffer", &mut offer, "Offer the lens database download when LightCraft starts") {
        app.ui.settings.lens_database = if offer { Offer::Ask } else { Offer::Declined };
    }
    hint(
        ui,
        t,
        crate::i18n::tr(
            "lensfun's open lens database (CC BY-SA 3.0) corrects distortion and vignetting for many lenses. It is only downloaded when you choose to.",
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
    if super::native_controls_active(ui.ctx()) {
        // Settings remains a native subtree until SDK/native traversal is supported.
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
        return out.filter(|v| v.as_deref() != current);
    }

    use craft::ui::contract::{Bind, Entry, Props as _, Show as _};
    use craft::ui::pickers::Select;
    use std::sync::Arc;

    let (rect, _) = ui.allocate_exact_size(egui::vec2(240.0, 24.0), egui::Sense::hover());
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return None;
    }
    // Preserve the virtualized picker's input order and headers at each group transition.
    // Imported collections can contain thousands of presets: cache captions by generation
    // and language, rather than allocating the entire entry list on every idle frame.
    type Cached = (u64, crate::i18n::Locale, String, Arc<Vec<Entry<Option<String>>>>);
    let generation = app.session.presets.generation();
    let language = crate::i18n::language();
    let none = crate::i18n::tr(none_label).to_owned();
    let cache = ui.scope_id().with(("settings-preset-entries", id));
    let entries = ui
        .data(|data| data.get_temp::<Cached>(cache))
        .filter(|(old, locale, caption, _)| *old == generation && *locale == language && *caption == none)
        .map(|(_, _, _, entries)| entries)
        .unwrap_or_else(|| {
            let mut entries = vec![Entry::new(None, none.clone())];
            let mut group = None;
            for preset in app.session.presets.iter() {
                if group != Some(preset.group.as_str()) {
                    group = Some(preset.group.as_str());
                    entries.push(Entry::heading(crate::i18n::builtin_label(&preset.group, preset.builtin).to_owned()));
                }
                entries.push(Entry::new(Some(preset.id.clone()), crate::i18n::builtin_label(&preset.name, preset.builtin).to_owned()));
            }
            let entries = Arc::new(entries);
            ui.data_mut(|data| data.insert_temp(cache, (generation, language, none, Arc::clone(&entries))));
            entries
        });
    // Option<String> is the actual value (including explicit None), not a mixed binding.
    // A missing id stays visible as the original "{id} (missing)" caption until chosen over.
    let shown = Select::new(label.to_owned(), Bind::given(current.map(str::to_owned)), entries.as_slice())
        .id_salt(("settings-preset", id))
        .placeholder(text)
        .show_at(ui, rect);
    crate::widgets::register_response(format!("combo:{id}"), rect, &shown.response);
    if ui.is_enabled() { shown.events.committed().cloned().filter(|next| next.as_deref() != current) } else { None }
}

#[cfg(test)]
#[path = "import_default_preset_flow_tests.rs"]
mod import_default_preset_flow_tests;

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
