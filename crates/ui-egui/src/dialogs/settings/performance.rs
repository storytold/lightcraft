use super::widgets::{check, choices, heading, hint, row};
#[cfg(not(target_arch = "wasm32"))]
use super::widgets::{label_width, path_label};
use super::{CACHE_SIZES, LABEL_W, LightcraftApp, PREVIEW_LIMITS, RichText, Tokens, Value, json, register};

// --------------------------------------------------------------------------------- Performance

pub(super) fn performance_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use lightcraft_engine::gpu;
    heading(ui, t, crate::i18n::tr("Rendering"));
    #[cfg(target_os = "windows")]
    check(ui, "settings.composeWindow", &mut app.ui.settings.compose_window, "Prevent G-SYNC/FreeSync flicker (applies at next launch)");
    check(ui, "settings.gpu", &mut app.ui.settings.gpu, "Use the GPU for rendering");
    // `available` would wait for a device still being created (issue #136)
    let status = if gpu::enabled() && !gpu::ready() {
        // the device is created on another thread, which doesn't wake the UI when it's done;
        // make sure that thread runs (cheap and idempotent: a start lost to an off/on switch
        // can't leave "starting" for good)
        gpu::warm_up();
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        "GPU: starting…".to_string()
    } else if !gpu::available() {
        match gpu::unavailable_reason() {
            Some(why) => crate::i18n::tr_format!("Rendering on the CPU: {why}", why = why),
            None => "No usable GPU found: rendering on the CPU".to_string(),
        }
    } else {
        format!("GPU: {}", gpu::adapter_name().unwrap_or_else(|| "starting…".into()))
    };
    // One line, the whole text on hover: the reason can be a long driver message (no adapter
    // among the backends …), and the GPU state is process-wide, so it may change between frames;
    // a hint that wrapped would move every control below it under the pointer.
    ui.add(egui::Label::new(RichText::new(crate::i18n::tr(&status)).size(11.0).color(t.text_dim)).truncate()).on_hover_text(&status);
    row(ui, t, crate::i18n::tr("Preview size"), |ui| {
        let opts: Vec<(u32, String)> =
            PREVIEW_LIMITS.iter().map(|e| (*e, if *e == 0 { crate::i18n::tr("Automatic").to_string() } else { format!("{e} px") })).collect();
        let opts: Vec<(u32, &str)> = opts.iter().map(|(e, l)| (*e, l.as_str())).collect();
        choices(ui, "settingsPreview", &opts, &mut app.ui.settings.preview_limit);
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Automatic renders the Detail view at the size it is shown, up to the photo's own pixels. Choose a size to cap it: smaller is faster.",
        ),
    );
    row(ui, t, crate::i18n::tr("Memory for caches"), |ui| {
        let auto = crate::i18n::tr_format!("Automatic ({} MB)", lightcraft_engine::memory::default_budget() >> 20);
        let opts = [(0u32, auto.as_str()), (512, "512 MB"), (1024, "1 GB"), (2048, "2 GB"), (4096, "4 GB")];
        choices(ui, "settingsMemory", &opts, &mut app.ui.settings.memory_mb);
    });
    heading(ui, t, crate::i18n::tr("Preview cache"));
    let cur = app.session.cache_mb;
    row(ui, t, crate::i18n::tr("Size limit"), |ui| {
        let gb = lightcraft_engine::media::default_disk_cache_bytes(app.session.catalog.len()).div_ceil(1 << 30);
        let auto = format!("{} ({gb} GB)", crate::i18n::tr("Automatic"));
        let opts = [
            (512u32, "512 MB"),
            (1024, "1 GB"),
            (2048, "2 GB"),
            (4096, "4 GB"),
            (8192, "8 GB"),
            (51_200, "50 GB"),
            (204_800, "200 GB"),
            (0, auto.as_str()),
        ];
        let mut v = if CACHE_SIZES.contains(&cur) { cur } else { 0 };
        if choices(ui, "settingsCache", &opts, &mut v) {
            let _ = app.run("library.preferences", json!({"cacheMb": v}));
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Grid thumbnails and the previews the loupe shows, kept on disk so photos open at once next time. Automatic makes room for every photo in this library (about 1.5 MB a photo). 50 GB: normal · 200 GB: better, for several weddings or large shoots.",
        ),
    );
    let used = app.session.media.rendered.disk().map(|d| d.size());
    row(ui, t, crate::i18n::tr("In use"), |ui| {
        ui.label(
            RichText::new(used.map(|b| format!("{:.1} MB", b as f64 / 1048576.0)).unwrap_or_else(|| crate::i18n::tr("memory only").to_string()))
                .color(t.text),
        );
        let r = ui.button(crate::i18n::tr("Clear Cache"));
        register(ui, "button:settingsClearCache", r.rect);
        if r.clicked() {
            let _ = app.run("library.clearPreviews", json!({}));
        }
    });
    background_prep(app, ui, t);
    heading(ui, t, crate::i18n::tr("Local folders"));
    row(ui, t, crate::i18n::tr("Forget unchanged photos"), |ui| {
        let opts = [(0u32, "Never"), (7, "After 7 days"), (30, "After 30 days"), (90, "After 90 days"), (365, "After a year")];
        let mut v = app.session.forget_local_days;
        if choices(ui, "settingsForgetLocal", &opts, &mut v) {
            let _ = app.run("library.preferences", json!({"forgetLocalDays": v}));
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photos seen in Local but never added or changed leave the catalog when their folder hasn't been browsed for this long (checked when the library opens). Files stay on disk; browsing the folder shows them again.",
        ),
    );
    #[cfg(not(target_arch = "wasm32"))]
    smart_previews(app, ui, t);
}

/// Settings ▸ Performance ▸ Prepare photos in the background (notes 47 and 48): per mode, what
/// is made ready before the user gets to it (`prefs.backgroundPrep`), app-wide or for this library.
fn background_prep(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use lightcraft_engine::prep::{Mode, Scope};
    heading(ui, t, crate::i18n::tr("Prepare photos in the background"));
    hint(
        ui,
        t,
        crate::i18n::tr(
            "While you go through photos in the loupe, the next ones are made ready before you reach them, and when the computer is otherwise idle the previews of every photo in view are built. The photo on screen always comes first; the work shows in the activity list, where it can be stopped.",
        ),
    );
    let own = app.session.prep.library.is_some();
    let lib_open = app.session.library.is_some();
    if lib_open {
        let mut v = own;
        if check(ui, "settings.prepOwn", &mut v, "Use different settings for this library") {
            let r = if v {
                let cur = app.session.prep_settings();
                app.run("prefs.setBackgroundPrep", json!({"library": true, "settings": cur}))
            } else {
                app.run("prefs.setBackgroundPrep", json!({"reset": true}))
            };
            if let Err(e) = r {
                log::warn!("background preparation: {e}");
            }
        }
        hint(ui, t, crate::i18n::tr("For example, a timelapse library can prepare nothing while a wedding library prepares everything."));
    }
    let cur = app.session.prep_settings();
    let mut patch: Option<Value> = None;
    let scopes = [(Scope::All, "Every photo"), (Scope::Marked, "Picks, rated and labelled"), (Scope::OnScreen, "Only the photo on screen")];
    for (mode, key, title) in [(Mode::Culling, "culling", "Culling (a folder before import)"), (Mode::Library, "library", "Library")] {
        let m = cur.mode(mode);
        ui.add_space(4.0);
        ui.label(RichText::new(crate::i18n::tr(title)).font(t.semibold(12.0)).color(t.text));
        let mut previews = m.previews;
        row(ui, t, crate::i18n::tr("Previews"), |ui| {
            if check(ui, &format!("settings.prep.{key}.previews"), &mut previews, "Build previews for every photo in view") {
                patch = Some(json!({key: {"previews": previews}}));
            }
        });
        hint(ui, t, crate::i18n::tr("Starts once you step through a few photos. Uses about 1.5 MB of disk a photo, and the computer's spare time."));
        let mut ahead = m.ahead;
        row(ui, t, crate::i18n::tr("Photos ahead"), |ui| {
            let opts = [(0u8, "Off"), (3, "3"), (5, "5"), (10, "10"), (20, "20")];
            if choices(ui, &format!("settingsPrepAhead-{key}"), &opts, &mut ahead) {
                patch = Some(json!({key: {"ahead": ahead}}));
            }
        });
        let mut scope = m.scope;
        row(ui, t, crate::i18n::tr("Which photos"), |ui| {
            if choices(ui, &format!("settingsPrepScope-{key}"), &scopes, &mut scope) {
                patch = Some(json!({key: {"scope": scope}}));
            }
        });
    }
    ui.add_space(4.0);
    ui.label(RichText::new(crate::i18n::tr("Editing")).font(t.semibold(12.0)).color(t.text));
    let mut ahead = cur.develop.ahead;
    row(ui, t, crate::i18n::tr("Photos ahead"), |ui| {
        let opts = [(0u8, "Off"), (1, "1"), (2, "2"), (3, "3"), (5, "5")];
        if choices(ui, "settingsPrepAhead-develop", &opts, &mut ahead) {
            patch = Some(json!({"develop": {"ahead": ahead}}));
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "The next photos are opened and kept ready to edit, so the first slider move on them is quick. Uses memory: a few hundred MB a photo.",
        ),
    );
    let mut scope = cur.develop.scope;
    row(ui, t, crate::i18n::tr("Which photos"), |ui| {
        if choices(ui, "settingsPrepScope-develop", &scopes, &mut scope) {
            patch = Some(json!({"develop": {"scope": scope}}));
        }
    });
    if let Some(patch) = patch
        && let Err(e) = app.run("prefs.setBackgroundPrep", json!({"library": own, "settings": patch}))
    {
        log::warn!("background preparation: {e}");
    }
}

/// Where this library keeps its smart previews (the offline-editing proxies, which can be large):
/// the effective folder, what is in it, and choosing another one.
#[cfg(not(target_arch = "wasm32"))]
fn smart_previews(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    // listing a big folder every frame would be slow: refresh every 2 s and after a change
    let cache = egui::Id::unique("smart-location");
    let now = ui.input(|i| i.time);
    let loc: Value = match ui.data(|d| d.get_temp::<(f64, Value)>(cache)) {
        Some((at, v)) if now - at < 2.0 => v,
        _ => {
            let v = app.run("library.smartPreviewsLocation", json!({})).unwrap_or(Value::Null);
            ui.data_mut(|d| d.insert_temp(cache, (now, v.clone())));
            v
        }
    };
    if loc.is_null() {
        return;
    }
    heading(ui, t, crate::i18n::tr("Smart previews"));
    let path = loc["path"].as_str().unwrap_or_default().to_string();
    let available = loc["available"].as_bool().unwrap_or(true);
    let custom = loc["custom"].as_bool().unwrap_or(false);
    row(ui, t, crate::i18n::tr("Folder"), |ui| {
        path_label(
            ui,
            "settingsSmartFolder",
            &path,
            egui::TextStyle::Body.resolve(ui.style()),
            if available { t.text } else { t.reject },
            if custom { 0.0 } else { label_width(ui, "(library default)", egui::TextStyle::Body) },
        );
        if !custom {
            let r = ui.label(RichText::new("(library default)").color(t.text));
            register(ui, "text:settingsSmartDefault", r.rect);
        }
    });
    let (count, bytes) = (loc["count"].as_u64().unwrap_or(0), loc["bytes"].as_u64().unwrap_or(0));
    if available {
        hint(
            ui,
            t,
            &crate::i18n::tr_format!(
                "{count} smart preview{} · {:.1} MB",
                if count == 1 { "" } else { "s" },
                bytes as f64 / 1048576.0,
                count = count
            ),
        );
    } else {
        hint(
            ui,
            t,
            crate::i18n::tr(
                "This folder is not available (drive disconnected?). Smart previews are not built or used until it is back or you choose another folder.",
            ),
        );
    }
    // what happens to the previews already built when the folder changes
    let mode_id = egui::Id::unique("smart-existing");
    let mut mode: u8 = ui.data(|d| d.get_temp(mode_id)).unwrap_or(0);
    if count > 0 {
        row(ui, t, crate::i18n::tr("Existing previews"), |ui| {
            if choices(ui, "settingsSmartExisting", &[(0u8, "Move them"), (1, "Leave them"), (2, "Delete them")], &mut mode) {
                ui.data_mut(|d| d.insert_temp(mode_id, mode));
            }
        });
    }
    let existing = ["move", "leave", "discard"][mode.min(2) as usize];
    let mut result = None;
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
        if app.services.pick_folder.is_some() {
            let r = ui.button(crate::i18n::tr("Choose Folder…"));
            register(ui, "button:settingsSmartChoose", r.rect);
            if r.clicked()
                && let Some(dir) = app.services.pick_folder.as_mut().and_then(|f| f())
            {
                result = Some(app.run("library.smartPreviewsLocation", json!({"path": dir, "existing": existing})));
            }
        }
        let r = ui.add_enabled(custom, egui::Button::new(crate::i18n::tr("Use Library Folder")));
        register(ui, "button:settingsSmartReset", r.rect);
        if r.clicked() {
            result = Some(app.run("library.smartPreviewsLocation", json!({"reset": true, "existing": existing})));
        }
    });
    if let Some(r) = result {
        ui.data_mut(|d| d.remove::<(f64, Value)>(cache));
        match r {
            Ok(v) => {
                let failed = v["failed"].as_array().map_or(0, Vec::len);
                let msg = match (existing, v["handled"].as_u64().unwrap_or(0)) {
                    (_, 0) => "Smart previews folder changed".to_string(),
                    ("move", n) => format!("Smart previews folder changed; moved {n}"),
                    ("discard", n) => format!("Smart previews folder changed; deleted {n}"),
                    _ => "Smart previews folder changed".to_string(),
                };
                app.toast(ui.ctx(), if failed > 0 { crate::i18n::tr_format!("{msg} ({failed} failed)", failed = failed, msg = msg) } else { msg });
            }
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
    hint(ui, t, crate::i18n::tr("Keep it on a drive with room: smart previews are about 1 MB per photo. The thumbnail cache stays in the library."));
}
