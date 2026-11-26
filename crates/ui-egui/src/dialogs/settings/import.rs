use craft::ui::buttons::Button;
use craft::ui::choice::Checkbox;
use craft::ui::contract::{Bind, Props as _, Show as _};

use super::general::{cameras, preset_combo};
use super::widgets::{button_width, check, choices, field, heading, hint, path_label, row};
use super::{LightcraftApp, RichText, Tokens, json};

fn auto_import_measuring(ui: &egui::Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

fn auto_import_button(ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
    let shown = Button::new(crate::i18n::tr(label).to_owned()).id_salt(id).enabled(enabled).show(ui);
    if auto_import_measuring(ui) {
        return false;
    }
    crate::widgets::register_response(id, shown.response.rect, &shown.response);
    ui.is_enabled() && enabled && shown.clicked()
}

fn auto_import_copy(ui: &mut egui::Ui, value: bool) -> Option<bool> {
    let shown = Checkbox::new(Bind::given(value))
        .id_salt("check:settingsAutoCopy")
        .label(crate::i18n::tr("Copy into the library (else use the files where they are)").to_owned())
        .show(ui);
    if auto_import_measuring(ui) {
        return None;
    }
    crate::widgets::register_response("check:settingsAutoCopy", shown.response.rect, &shown.response);
    shown.events.committed().copied().filter(|next| ui.is_enabled() && *next != value)
}

/// The choices of a camera's default, the same for every camera: the raw default (`raw_default`), LightCraft's
/// (`None`), then the presets under their group headings. Kept until the presets or the language change, since a
/// library can hold thousands of presets.
fn camera_entries(app: &LightcraftApp, ui: &egui::Ui, raw_default: &str) -> std::sync::Arc<Vec<craft::ui::contract::Entry<Option<String>>>> {
    use craft::ui::contract::Entry;
    use std::sync::Arc;
    type Cached = (u64, crate::i18n::Locale, Arc<Vec<Entry<Option<String>>>>);
    let generation = app.session.presets.generation();
    let language = crate::i18n::language();
    let cache = egui::Id::unique("settings-camera-entries");
    if let Some((_, _, entries)) = ui.data(|data| data.get_temp::<Cached>(cache)).filter(|(old, locale, _)| *old == generation && *locale == language)
    {
        return entries;
    }
    let mut entries = vec![
        Entry::new(Some(raw_default.to_owned()), crate::i18n::tr("Same as raw default").to_owned()),
        Entry::new(None, crate::i18n::tr("LightCraft Default").to_owned()),
    ];
    let mut group = None;
    for preset in app.session.presets.iter() {
        if group != Some(preset.group.as_str()) {
            group = Some(preset.group.as_str());
            entries.push(Entry::heading(crate::i18n::builtin_label(&preset.group, preset.builtin).to_owned()));
        }
        entries.push(Entry::new(Some(preset.id.clone()), crate::i18n::builtin_label(&preset.name, preset.builtin).to_owned()));
    }
    let entries = Arc::new(entries);
    ui.data_mut(|data| data.insert_temp(cache, (generation, language, Arc::clone(&entries))));
    entries
}

pub(super) fn import_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let d = app.session.import_defaults.clone();
    heading(ui, t, crate::i18n::tr("Raw defaults"));
    hint(ui, t, crate::i18n::tr("Settings new raw photos start from. Changing them doesn't touch photos already in the library."));
    row(ui, t, crate::i18n::tr("Raw photos"), |ui| {
        if let Some(v) = preset_combo(app, ui, "settingsRawPreset", crate::i18n::tr("Raw default"), d.raw_preset.as_deref(), "LightCraft Default") {
            let _ = app.run("library.preferences", json!({"import": {"rawPreset": v}}));
        }
    });
    let mut per = d.per_camera;
    row(ui, t, "", |ui| {
        if check(ui, "settings.perCamera", &mut per, "Use camera-specific defaults") {
            let _ = app.run("library.preferences", json!({"import": {"perCamera": per}}));
        }
    });
    if per {
        let cams = cameras(app);
        if cams.is_empty() {
            hint(ui, t, crate::i18n::tr("No raw photos yet: cameras appear here once their photos are in the library."));
        }
        egui::ScrollArea::vertical().max_height(150.0).id_salt("settingsCameras").show(ui, |ui| {
            for (i, cam) in cams.iter().enumerate() {
                let entry = d.cameras.iter().find(|c| c.camera.eq_ignore_ascii_case(cam));
                row(ui, t, cam, |ui| {
                    // "Raw default" = no entry; otherwise the entry's preset (None = LightCraft Default)
                    const RAW_DEFAULT: &str = "\u{1}raw";
                    let current = match entry {
                        None => Some(RAW_DEFAULT),
                        Some(e) => e.preset.as_deref(),
                    };
                    // The camera's own default is the raw default (no entry), LightCraft's, or a preset; a preset that
                    // has gone keeps its id as the caption until another is chosen.
                    let missing = current.filter(|pid| *pid != RAW_DEFAULT && !app.session.presets.iter().any(|p| p.id == *pid));
                    let entries = camera_entries(app, ui, RAW_DEFAULT);
                    let id = format!("settingsCamera-{i}");
                    let picked = crate::widgets::select(
                        ui,
                        &format!("combo:{id}"),
                        crate::i18n::tr("Camera"),
                        current.map(str::to_owned),
                        entries.as_slice(),
                        missing.map(str::to_owned),
                        Some(240.0),
                    );
                    let pick = picked.map(|next| match next.as_deref() {
                        Some(RAW_DEFAULT) => json!({"camera": cam, "remove": true}),
                        None => json!({"camera": cam, "preset": null}),
                        Some(pid) => json!({"camera": cam, "preset": pid}),
                    });
                    if let Some(c) = pick {
                        let _ = app.run("library.preferences", json!({"camera": c}));
                    }
                });
            }
        });
    }
    heading(ui, t, crate::i18n::tr("Other images (JPEG, PNG, TIFF, HEIC…)"));
    row(ui, t, crate::i18n::tr("Non-raw photos"), |ui| {
        if let Some(v) = preset_combo(app, ui, "settingsOtherPreset", crate::i18n::tr("Other preset"), d.other_preset.as_deref(), "None") {
            let _ = app.run("library.preferences", json!({"import": {"otherPreset": v}}));
        }
    });
    heading(ui, t, crate::i18n::tr("Metadata"));
    hint(ui, t, crate::i18n::tr("Added to photos you import that don't already have it."));
    for (key, label, hint_text, value) in
        [("copyright", "Copyright", "© 2026 Your Name", d.copyright.clone()), ("creator", "Creator", "Your Name", d.creator.clone())]
    {
        row(ui, t, label, |ui| {
            let id = egui::Id::unique(("settingsMeta", key));
            let mut text: String = ui.data(|m| m.get_temp(id)).unwrap_or(value.clone());
            // (the project's text field: Esc gives the text back and stays in Settings, and with
            // the text back the line below has nothing to save)
            let widget = format!("field:settings.{key}");
            let shown = field(ui, crate::text_field::TextField::singleline(&widget, &mut text).hint(hint_text).width(240.0));
            if shown.committed() && text.trim() != value {
                let _ = app.run("library.preferences", json!({"import": {key: text.trim()}}));
            }
            if shown.editing {
                ui.data_mut(|m| m.insert_temp(id, text));
            } else {
                ui.data_mut(|m| m.remove::<String>(id));
            }
        });
    }
    if !app.session.metadata_presets.is_empty() {
        row(ui, t, crate::i18n::tr("Metadata preset"), |ui| {
            // "" is no metadata preset, as the command takes it
            let cur = d.metadata_preset.clone().unwrap_or_default();
            let mut entries = vec![craft::ui::contract::Entry::new(String::new(), crate::i18n::tr("None").to_owned())];
            entries.extend(app.session.metadata_presets.iter().map(|m| craft::ui::contract::Entry::new(m.name.clone(), m.name.clone())));
            let pick = crate::widgets::select(
                ui,
                "combo:settingsMetaPreset",
                crate::i18n::tr("Metadata preset"),
                cur.clone(),
                &entries,
                Some(cur),
                Some(240.0),
            );
            if let Some(n) = pick {
                let _ = app.run("library.preferences", json!({"import": {"metadataPreset": n}}));
            }
        });
    }
    heading(ui, t, crate::i18n::tr("XMP sidecars"));
    let mut xmp = app.session.xmp;
    if check(ui, "settings.autoWriteXmp", &mut xmp.auto_write, "Automatically write changes into XMP sidecars") {
        let _ = app.run("library.xmpPreferences", json!({"autoWrite": xmp.auto_write}));
    }
    row(ui, t, crate::i18n::tr("Sidecar names"), |ui| {
        use lightcraft_engine::sidecar::SidecarNaming as N;
        let mut n = xmp.naming;
        if choices(ui, "settingsXmpNaming", &[(N::Stem, "IMG_1.xmp"), (N::Full, "IMG_1.CR3.xmp")], &mut n) {
            let naming = if n == N::Full { "full" } else { "stem" };
            let _ = app.run("library.xmpPreferences", json!({"naming": naming}));
        }
    });
    if !cfg!(target_arch = "wasm32") {
        heading(ui, t, crate::i18n::tr("Dropped Files"));
        check(ui, "settings.importDropped", &mut app.ui.settings.import_dropped_directly, "Import dropped files without review");
        heading(ui, t, crate::i18n::tr("Cameras and Cards"));
        let mut on_card = d.show_on_card;
        if check(ui, "settings.importOnCard", &mut on_card, "Show the import review when a camera or card is connected") {
            let _ = app.run("library.preferences", json!({"import": {"showOnCard": on_card}}));
        }
        heading(ui, t, crate::i18n::tr("Auto Import"));
        hint(ui, t, crate::i18n::tr("Photos that arrive in this folder (tethering, a scanner, a sync app) are added as soon as they're complete."));
        row(ui, t, crate::i18n::tr("Watched folder"), |ui| {
            match &d.auto_folder {
                Some(folder) => path_label(
                    ui,
                    "settingsAutoFolder",
                    folder,
                    egui::TextStyle::Body.resolve(ui.style()),
                    t.text,
                    button_width(ui, crate::i18n::tr("Choose…")) + button_width(ui, crate::i18n::tr("Turn Off")),
                ),
                None => drop(ui.label(RichText::new(crate::i18n::tr("Off")).color(t.text))),
            }
            let can = app.services.pick_folder.is_some();
            if auto_import_button(ui, "button:settingsAutoFolder", "Choose…", can)
                && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
                && let Err(e) = app.run("library.autoImport", json!({"folder": f}))
            {
                app.toast(ui.ctx(), e);
            }
            if d.auto_folder.is_some() && auto_import_button(ui, "button:settingsAutoOff", "Turn Off", true) {
                let _ = app.run("library.autoImport", json!({"folder": null}));
            }
        });
        // the last listing of the watched folder failed: say why (it reads "OS error: reason")
        if let (Some(folder), Some((failed, e))) = (&d.auto_folder, &app.ui.auto_import_error)
            && folder == failed
        {
            let reason = e.strip_prefix(folder.as_str()).and_then(|r| r.strip_prefix(": ")).unwrap_or(e);
            row(ui, t, "", |ui| {
                ui.label(RichText::new(crate::i18n::tr_format!("Can't read this folder: {}", reason)).size(11.0).color(t.caution));
            });
        }
        if d.auto_folder.is_some() {
            row(ui, t, "", |ui| {
                if let Some(copy) = auto_import_copy(ui, d.auto_copy) {
                    let _ = app.run("library.autoImport", json!({"copy": copy}));
                }
            });
            row(ui, t, crate::i18n::tr("Album"), |ui| {
                // (per library: a name left here by one library is not another's)
                let id = egui::Id::unique(("auto-album", app.session.library.as_ref().map(|l| l.dir.clone())));
                let mut name = ui.data(|m| m.get_temp::<String>(id)).unwrap_or_else(|| d.auto_album.clone().unwrap_or_default());
                let shown = field(
                    ui,
                    crate::text_field::TextField::singleline("field:settingsAutoAlbum", &mut name).hint(crate::i18n::tr("None")).width(180.0),
                );
                if shown.committed() {
                    let _ = app.run("library.autoImport", json!({"album": name.trim()}));
                }
                ui.data_mut(|m| m.insert_temp(id, name));
            });
        }
    }
    if app.session.library.is_none() {
        hint(ui, t, crate::i18n::tr("In-memory session: these settings last until LightCraft quits."));
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
#[path = "auto_import_controls_port_tests.rs"]
mod auto_import_controls_port_tests;
