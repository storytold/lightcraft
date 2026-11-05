use super::general::{cameras, preset_combo};
use super::widgets::{button_width, check, choices, field, heading, hint, path_label, row};
use super::{LightcraftApp, RichText, Tokens, json, register};

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
                    let mut pick = None;
                    let label = match current {
                        Some(RAW_DEFAULT) => crate::i18n::tr("Same as raw default").to_string(),
                        None => crate::i18n::tr("LightCraft Default").to_string(),
                        Some(pid) => app
                            .session
                            .presets
                            .iter()
                            .find(|p| p.id == pid)
                            .map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string())
                            .unwrap_or_else(|| pid.to_string()),
                    };
                    let id = format!("settingsCamera-{i}");
                    let r = crate::widgets::combo_box(&id).width(240.0).selected_text(label).show_ui(ui, |ui| {
                        if ui.selectable_label(current == Some(RAW_DEFAULT), crate::i18n::tr("Same as raw default")).clicked() {
                            pick = Some(json!({"camera": cam, "remove": true}));
                        }
                        if ui.selectable_label(current.is_none(), crate::i18n::tr("LightCraft Default")).clicked() {
                            pick = Some(json!({"camera": cam, "preset": null}));
                        }
                        if let Some(pid) = crate::panels::presets::picker_rows(ui, &id, &app.session.presets, current) {
                            pick = Some(json!({"camera": cam, "preset": pid}));
                        }
                    });
                    crate::widgets::name_combo(&r.response, crate::i18n::tr("Camera"));
                    register(ui, format!("combo:{id}"), r.response.rect);
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
            let cur = d.metadata_preset.clone();
            let mut pick = None;
            let r = crate::widgets::combo_box("settingsMetaPreset")
                .width(240.0)
                .selected_text(cur.clone().unwrap_or_else(|| crate::i18n::tr("None").into()))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(cur.is_none(), crate::i18n::tr("None")).clicked() {
                        pick = Some(String::new());
                    }
                    for m in &app.session.metadata_presets {
                        if ui.selectable_label(cur.as_deref() == Some(m.name.as_str()), &m.name).clicked() {
                            pick = Some(m.name.clone());
                        }
                    }
                });
            crate::widgets::name_combo(&r.response, crate::i18n::tr("Metadata preset"));
            register(ui, "combo:settingsMetaPreset", r.response.rect);
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
            let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Choose…")));
            register(ui, "button:settingsAutoFolder", r.rect);
            if r.clicked()
                && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
                && let Err(e) = app.run("library.autoImport", json!({"folder": f}))
            {
                app.toast(ui.ctx(), e);
            }
            if d.auto_folder.is_some() && ui.button(crate::i18n::tr("Turn Off")).clicked() {
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
                let mut copy = d.auto_copy;
                if ui.checkbox(&mut copy, crate::i18n::tr("Copy into the library (else use the files where they are)")).changed() {
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
