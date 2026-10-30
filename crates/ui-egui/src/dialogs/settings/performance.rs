use super::widgets::{ask_first, check, choices, heading, hint, row};
#[cfg(not(target_arch = "wasm32"))]
use super::widgets::{label_width, path_label};
use super::{CACHE_SIZES, LABEL_W, LightcraftApp, PREVIEW_LIMITS, RichText, Tokens, Value, json, register};

// --------------------------------------------------------------------------------- Performance

/// "GPU: <adapter>", or "GPU: starting…" (in the language shown) while the adapter is not known yet.
pub(crate) fn gpu_line(adapter: Option<String>) -> String {
    format!("GPU: {}", adapter.unwrap_or_else(|| crate::i18n::tr("starting…").to_string()))
}

pub(super) fn performance_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use lightcraft_engine::gpu;
    heading(ui, t, "Rendering");
    check(ui, "settings.gpu", &mut app.ui.settings.gpu, "Use the GPU for rendering");
    let status = if !gpu::available() {
        match gpu::unavailable_reason() {
            Some(why) => crate::i18n::tr_format!("Rendering on the CPU: {why}", why = why),
            None => "No usable GPU found: rendering on the CPU".to_string(),
        }
    } else {
        gpu_line(gpu::adapter_name())
    };
    // One line, the whole text on hover: the reason can be a long driver message (no adapter
    // among the backends …), and the GPU state is process-wide, so it may change between frames;
    // a hint that wrapped would move every control below it under the pointer.
    ui.add(egui::Label::new(RichText::new(crate::i18n::tr(&status)).size(11.0).color(t.legible(t.text_dim, t.chrome, 4.5))).truncate())
        .on_hover_text(&status);
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
    heading(ui, t, "Thumbnail cache");
    let cur = (app.session.cache_bytes() >> 20) as u32;
    row(ui, t, crate::i18n::tr("Size limit"), |ui| {
        let opts = [(512u32, "512 MB"), (1024, "1 GB"), (2048, "2 GB"), (4096, "4 GB"), (8192, "8 GB")];
        let mut v = if CACHE_SIZES.contains(&cur) { cur } else { 2048 };
        if choices(ui, "settingsCache", &opts, &mut v) {
            let _ = app.run("library.preferences", json!({"cacheMb": v}));
        }
    });
    let used = app.session.media.rendered.disk().map(|d| d.size());
    row(ui, t, crate::i18n::tr("In use"), |ui| {
        ui.label(
            RichText::new(used.map(|b| format!("{:.1} MB", b as f64 / 1048576.0)).unwrap_or_else(|| crate::i18n::tr("memory only").to_string()))
                .color(t.text),
        );
        let r = ui.button(crate::i18n::tr("Clear Cache"));
        register(ui, "button:settingsClearCache", r.rect);
        let r = ask_first(
            t,
            "button:settingsClearCache",
            r,
            "Thumbnails and previews are made again as you browse, which takes a while in a large library. Your photos and edits are not touched.",
            "Clear Cache",
        );
        if r.clicked() {
            let _ = app.run("library.clearPreviews", json!({}));
        }
    });
    heading(ui, t, "Local folders");
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

/// What the smart previews row says after the path of a folder the library was not given: "(library
/// default)", in the language shown. (The catalogs have that message with the two spaces that
/// follow a path.)
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn smart_folder_default() -> String {
    crate::i18n::tr("  (library default)").trim().to_string()
}

/// What the Smart previews section ([`smart_previews`]) keeps between frames: what is chosen for the previews already
/// built (0 move, 1 leave, 2 delete) and a change of folder that waits for a yes because it would
/// delete them, with the frame, library and folder they were chosen and asked in.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Default)]
struct SmartAsk {
    pass: u64,
    library: String,
    folder: String,
    mode: u8,
    waiting: Option<Value>,
}

/// Where this library keeps its smart previews (the offline-editing proxies, which can be large):
/// the effective folder, what is in it, and choosing another one.
#[cfg(not(target_arch = "wasm32"))]
fn smart_previews(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let cache = egui::Id::unique("smart-location");
    let Some(loc) = smart_location(app, ui, cache) else { return };
    heading(ui, t, "Smart previews");
    let path = loc["path"].as_str().unwrap_or_default().to_string();
    let custom = loc["custom"].as_bool().unwrap_or(false);

    smart_folder_rows(ui, t, &loc);
    let (count, bytes) = (loc["count"].as_u64().unwrap_or(0), loc["bytes"].as_u64().unwrap_or(0));
    // what happens to the previews already built when the folder changes
    // That choice, and a change waiting for an answer, are kept from one frame of this section to
    // the next only, and only for this library and this folder: with Settings closed, another
    // tab or library opened, or the folder changed some other way, nothing is left chosen or asked.
    let ask_id = egui::Id::unique("smart-ask");
    // (counted in frames: the first pass of a frame that opens a prompt draws the prompt alone, so
    // passes skip a number while Settings is still the same window)
    let pass = ui.ctx().cumulative_frame_nr();
    let library = app.session.library.as_ref().map(|l| l.dir.display().to_string()).unwrap_or_default();
    let kept: Option<SmartAsk> = ui.data(|d| d.get_temp(ask_id));
    let mut ask = kept.filter(|k| pass.saturating_sub(k.pass) <= 1 && k.library == library && k.folder == path).unwrap_or_default();
    let mut mode = ask.mode;
    if count > 0 {
        row(ui, t, crate::i18n::tr("Existing previews"), |ui| {
            choices(ui, "settingsSmartExisting", &[(0u8, "Move them"), (1, "Leave them"), (2, "Delete them")], &mut mode);
        });
    }
    let mut existing = ["move", "leave", "discard"][mode.min(2) as usize];
    // Deleting the previews already built is asked about once more, when the folder they would
    // be deleted for is known: until then the change (the command's parameters) waits here.
    // (asked for "Delete them" only: choosing otherwise withdraws the question)
    let mut waiting: Option<Value> = ask.waiting.take().filter(|_| existing == "discard");
    let mut change = None;
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
        if app.services.pick_folder.is_some() {
            let r = ui.button(crate::i18n::tr("Choose Folder…"));
            register(ui, "button:settingsSmartChoose", r.rect);
            if r.clicked()
                && let Some(dir) = app.services.pick_folder.as_mut().and_then(|f| f())
            {
                change = Some(json!({"path": dir, "existing": existing}));
            }
        }
        let r = ui.add_enabled(custom, egui::Button::new(crate::i18n::tr("Use Library Folder")));
        register(ui, "button:settingsSmartReset", r.rect);
        if r.clicked() {
            change = Some(json!({"reset": true, "existing": existing}));
        }
    });
    if existing == "discard" && count > 0 && change.is_some() {
        waiting = change.take();
    }
    let mut answered = false;
    if let Some(params) = &waiting {
        // (said with the count line above and two plain sentences, so no new format string is needed)
        let how_many =
            crate::i18n::tr_format!("{count} smart preview{} · {:.1} MB", if count == 1 { "" } else { "s" }, bytes as f64 / 1048576.0, count = count);
        let question = format!(
            "{} {how_many}. {}",
            crate::i18n::tr("Delete the smart previews in the folder used until now?"),
            crate::i18n::tr(
                "Photos whose originals are not connected are shown and edited from their smart previews; new ones can only be made from originals that are connected."
            ),
        );
        ui.add(egui::Label::new(RichText::new(question).color(t.legible(t.caution, t.chrome, 4.5))).wrap());
        ui.horizontal(|ui| {
            let r = ui.button(crate::i18n::tr("Delete and Change Folder"));
            register(ui, "button:settingsSmartDelete", r.rect);
            if r.clicked() {
                // (what the section shows is up to two seconds old: the folder is asked for
                // again, and a question about a folder that is no longer the one is dropped)
                let still = app.run("library.smartPreviewsLocation", json!({})).is_ok_and(|now| now["path"].as_str() == Some(path.as_str()));
                if still {
                    change = Some(params.clone());
                    existing = "discard";
                } else {
                    ui.data_mut(|d| d.remove::<(f64, Value)>(cache));
                }
                answered = true;
            }
            let r = ui.button(crate::i18n::tr("Cancel"));
            register(ui, "button:settingsSmartCancel", r.rect);
            answered |= r.clicked();
        });
    }
    if answered || change.is_some() {
        // (the choice goes back to "Move them": deleting is never left set for the next time)
        waiting = None;
        mode = 0;
    }
    ui.data_mut(|d| d.insert_temp(ask_id, SmartAsk { pass, library, folder: path.clone(), mode, waiting }));
    let result = change.map(|params| app.run("library.smartPreviewsLocation", params));
    if let Some(r) = result {
        ui.data_mut(|d| d.remove::<(f64, Value)>(cache));
        smart_change_toast(app, ui, r, existing);
    }
    hint(ui, t, crate::i18n::tr("Keep it on a drive with room: smart previews are about 1 MB per photo. The thumbnail cache stays in the library."));
}

/// The smart previews' location, listed at most every 2 s (listing a big folder every frame would be slow)
/// and again after a change (`cache` is dropped then); `None` when the library has none.
#[cfg(not(target_arch = "wasm32"))]
fn smart_location(app: &mut LightcraftApp, ui: &egui::Ui, cache: egui::Id) -> Option<Value> {
    let now = ui.input(|i| i.time);
    let loc: Value = match ui.data(|d| d.get_temp::<(f64, Value)>(cache)) {
        Some((at, v)) if now - at < 2.0 => v,
        _ => {
            let v = app.run("library.smartPreviewsLocation", json!({})).unwrap_or(Value::Null);
            ui.data_mut(|d| d.insert_temp(cache, (now, v.clone())));
            v
        }
    };
    (!loc.is_null()).then_some(loc)
}

/// The Folder row and the line under it: how many previews are there, or that the folder is not available.
#[cfg(not(target_arch = "wasm32"))]
fn smart_folder_rows(ui: &mut egui::Ui, t: &Tokens, loc: &Value) {
    let path = loc["path"].as_str().unwrap_or_default();
    let available = loc["available"].as_bool().unwrap_or(true);
    let custom = loc["custom"].as_bool().unwrap_or(false);
    row(ui, t, crate::i18n::tr("Folder"), |ui| {
        let default = smart_folder_default();
        path_label(
            ui,
            "settingsSmartFolder",
            path,
            egui::TextStyle::Body.resolve(ui.style()),
            if available { t.text } else { t.legible(t.reject, t.chrome, 4.5) },
            if custom { 0.0 } else { label_width(ui, &default, egui::TextStyle::Body) },
        );
        if !custom {
            let r = ui.label(RichText::new(default).color(t.text));
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
                crate::i18n::plural_s(count as usize),
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
}

/// Says what a folder change did: how many previews were moved or deleted, and how many failed.
#[cfg(not(target_arch = "wasm32"))]
fn smart_change_toast(app: &mut LightcraftApp, ui: &egui::Ui, result: Result<Value, String>, existing: &str) {
    match result {
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
