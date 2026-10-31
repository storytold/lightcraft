use super::widgets::{check, choices, heading, hint, row};
#[cfg(not(target_arch = "wasm32"))]
use super::widgets::{label_width, path_label};
use super::{CACHE_SIZES, LABEL_W, LightcraftApp, PREVIEW_LIMITS, RichText, Tokens, Value, json, register};

// --------------------------------------------------------------------------------- Performance

/// Exports: photos side by side, the GPU for exports, and "Test my machine", which measures both and
/// sets them (its last report below the button).
fn exports_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Exports"));
    row(ui, t, crate::i18n::tr("Photos at once"), |ui| {
        // (what Automatic is here: what Test my machine found, else half the processor's threads, up to 12)
        let tested = app.ui.settings.machine_test.as_ref().map(|m| m.lanes).filter(|n| *n > 0);
        let auto = match tested {
            Some(n) => format!("{} ({n}, {})", crate::i18n::tr("Automatic"), crate::i18n::tr("tested")),
            None => {
                let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
                format!("{} ({})", crate::i18n::tr("Automatic"), (cores / 2).clamp(1, 12))
            }
        };
        let mut opts: Vec<(u32, String)> = vec![(0, auto)];
        let cur = app.ui.settings.export_lanes;
        for n in [2u32, 4, 6, 8, 12, 16, 24] {
            opts.push((n, n.to_string()));
        }
        if cur > 0 && !opts.iter().any(|(v, _)| *v == cur) {
            opts.push((cur, cur.to_string()));
            opts.sort_by_key(|(v, _)| *v);
        }
        let opts: Vec<(u32, &str)> = opts.iter().map(|(v, l)| (*v, l.as_str())).collect();
        choices(ui, "settingsExportLanes", &opts, &mut app.ui.settings.export_lanes);
    });
    let mut gpu_exports = !app.ui.settings.export_cpu_only;
    if check(ui, "settings.exportGpu", &mut gpu_exports, "Use the GPU for exports") {
        app.ui.settings.export_cpu_only = !gpu_exports;
    }
    row(ui, t, crate::i18n::tr("Test my machine"), |ui| {
        let running = crate::machine_test::running(app);
        let label = if running { crate::i18n::tr("Testing…") } else { crate::i18n::tr("Test my machine") };
        let r = ui.add_enabled(!running, egui::Button::new(label));
        register(ui, "button:settingsTestMachine", r.rect);
        if r.clicked()
            && let Err(e) = crate::machine_test::start(app)
        {
            app.toast_error(ui.ctx(), e);
        }
    });
    let text = match &app.ui.settings.machine_test {
        Some(m) => {
            let when = m.tested.get(..10).unwrap_or(&m.tested);
            format!("{} {when}: {}", crate::i18n::tr("Tested"), crate::i18n::tr(&m.summary))
        }
        None => crate::i18n::tr(
            "Exports a few of your photos (or the demo photos) for a few minutes at several settings, then uses the fastest. Run it again after changing the computer or its graphics driver.",
        )
        .to_string(),
    };
    hint(ui, t, &text);
}

/// The graphics cards to choose from for editing: Automatic (the screen's card), each card that can
/// render, and a saved card that isn't there (kept: decision 4, 31 Oct 2026), as (setting, label).
pub(crate) fn gpu_card_choices(cards: &[lightcraft_engine::gpu::CardInfo], saved: &str) -> Vec<(String, String)> {
    let usable: Vec<&lightcraft_engine::gpu::CardInfo> = cards.iter().filter(|c| c.usable).collect();
    let auto = match usable.iter().find(|c| c.drives_screen) {
        Some(c) => format!("{} ({} — {})", crate::i18n::tr("Automatic"), c.name, crate::i18n::tr("your screen's card")),
        None => crate::i18n::tr("Automatic").to_string(),
    };
    let mut opts = vec![(String::new(), auto)];
    for c in &usable {
        // identical cards: numbered
        let twins = usable.iter().filter(|o| o.name == c.name).count();
        let n = usable.iter().take_while(|o| o.key != c.key).filter(|o| o.name == c.name).count() + 1;
        opts.push((c.key.clone(), if twins > 1 { format!("{} ({n})", c.name) } else { c.name.clone() }));
    }
    if !saved.is_empty() && !opts.iter().any(|(k, _)| k == saved) {
        let name = lightcraft_engine::gpu::card_name(saved).unwrap_or_else(|| saved.to_string());
        opts.push((saved.to_string(), format!("{name} ({})", crate::i18n::tr("not found"))));
    }
    opts
}

/// Settings ▸ Performance ▸ Graphics card for editing: shown when there are cards to choose from
/// (two or more, or a saved one).
fn gpu_card_row(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let cards = lightcraft_engine::gpu::adapters().unwrap_or_default();
    let saved = app.ui.settings.gpu_card.clone();
    if cards.iter().filter(|c| c.usable).count() < 2 && saved.is_empty() {
        return;
    }
    let opts = gpu_card_choices(&cards, &saved);
    let mut at = opts.iter().position(|(k, _)| *k == saved).unwrap_or(0);
    row(ui, t, crate::i18n::tr("Graphics card for editing"), |ui| {
        ui.horizontal_wrapped(|ui| {
            let labels: Vec<(usize, &str)> = opts.iter().enumerate().map(|(i, (_, l))| (i, l.as_str())).collect();
            if choices(ui, "settingsGpuCard", &labels, &mut at)
                && let Some((key, _)) = opts.get(at)
            {
                app.ui.settings.gpu_card = key.clone();
            }
        });
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "The card that renders the photo you're working on. Automatic uses the card your screen is connected to, so photos appear without being copied between cards.",
        ),
    );
}


/// The editing card among `cards`: the one chosen (`saved`, when it is there), else the screen's,
/// else the one named `in_use`.
pub(crate) fn editing_card_key(cards: &[lightcraft_engine::gpu::CardInfo], saved: &str, in_use: Option<&str>) -> Option<String> {
    let usable = || cards.iter().filter(|c| c.usable);
    usable()
        .find(|c| !saved.is_empty() && c.key == saved)
        .or_else(|| usable().find(|c| c.drives_screen))
        .or_else(|| usable().find(|c| Some(c.name.as_str()) == in_use))
        .map(|c| c.key.clone())
}

/// The choices for the second graphics card: Automatic (naming the card it uses), Don't use, each
/// other card that can render, and a saved card that isn't there, as (setting, label).
pub(crate) fn gpu_helper_choices(cards: &[lightcraft_engine::gpu::CardInfo], editing: Option<&str>, saved: &str) -> Vec<(String, String)> {
    let others: Vec<&lightcraft_engine::gpu::CardInfo> = cards.iter().filter(|c| c.usable && Some(c.key.as_str()) != editing).collect();
    let auto = others.iter().find(|c| c.kind == "dedicated").or_else(|| others.iter().find(|c| c.kind != "virtual"));
    let auto = match auto {
        // (what the card does is in the hint below: a longer label is one button too wide for a
        // small Settings window, which no wrapping can fix)
        Some(c) => format!("{} ({})", crate::i18n::tr("Automatic"), c.name),
        None => format!("{} ({})", crate::i18n::tr("Automatic"), crate::i18n::tr("no other card found")),
    };
    let mut opts = vec![(String::new(), auto), ("off".to_string(), crate::i18n::tr("Don't use").to_string())];
    for c in &others {
        let twins = others.iter().filter(|o| o.name == c.name).count();
        let n = others.iter().take_while(|o| o.key != c.key).filter(|o| o.name == c.name).count() + 1;
        opts.push((c.key.clone(), if twins > 1 { format!("{} ({n})", c.name) } else { c.name.clone() }));
    }
    if !saved.is_empty() && !opts.iter().any(|(k, _)| k == saved) {
        let name = lightcraft_engine::gpu::card_name(saved).unwrap_or_else(|| saved.to_string());
        opts.push((saved.to_string(), format!("{name} ({})", crate::i18n::tr("not found"))));
    }
    opts
}

/// What the second card is doing, in plain words, for the line under its row.
pub(crate) fn gpu_helper_line(st: &lightcraft_engine::gpu::HelperStatus) -> String {
    let name = st.name.clone().unwrap_or_default();
    let why = st.reason.clone().unwrap_or_default();
    match st.state {
        "ready" => format!("{} {name}", crate::i18n::tr("Helping now:")),
        "starting" | "notStarted" => crate::i18n::tr("Getting the second card ready…").to_string(),
        "lost" => format!("{} {why}", crate::i18n::tr("The second card stopped responding; LightCraft restarts it:")),
        "failed" => format!("{} {why}", crate::i18n::tr("The second card can't render:")),
        "off" => crate::i18n::tr("Not used").to_string(),
        _ => match st.reason.as_deref() {
            Some(r) => format!("{} {r}", crate::i18n::tr("Not used:")),
            None => crate::i18n::tr("Not used").to_string(),
        },
    }
}

/// Settings ▸ Performance ▸ Second graphics card: shown when there are two or more cards (or a
/// saved choice).
fn gpu_helper_row(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use lightcraft_engine::gpu;
    let cards = gpu::adapters().unwrap_or_default();
    let saved = app.ui.settings.gpu_helper.clone();
    if cards.iter().filter(|c| c.usable).count() < 2 && saved.is_empty() {
        return;
    }
    let in_use = gpu::card_in_use();
    let editing = editing_card_key(&cards, &app.ui.settings.gpu_card, in_use.as_deref());
    let opts = gpu_helper_choices(&cards, editing.as_deref(), &saved);
    let mut at = opts.iter().position(|(k, _)| *k == saved).unwrap_or(0);
    row(ui, t, crate::i18n::tr("Second graphics card"), |ui| {
        ui.horizontal_wrapped(|ui| {
            let labels: Vec<(usize, &str)> = opts.iter().enumerate().map(|(i, (_, l))| (i, l.as_str())).collect();
            if choices(ui, "settingsGpuHelper", &labels, &mut at)
                && let Some((key, _)) = opts.get(at)
            {
                app.ui.settings.gpu_helper = key.clone();
            }
        });
    });
    let line = gpu_helper_line(&gpu::helper_status());
    ui.add(egui::Label::new(RichText::new(&line).size(11.0).color(t.text_dim)).truncate()).on_hover_text(&line);
    hint(
        ui,
        t,
        crate::i18n::tr("Exports, previews and AI masks also run on this card, so the card for editing stays free for the photo you're working on."),
    );
}


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
    } else if gpu::switching_card() {
        format!("GPU: {} — {}", gpu::adapter_name().unwrap_or_default(), crate::i18n::tr("moving to the newly chosen card…"))
    } else {
        format!("GPU: {}", gpu::adapter_name().unwrap_or_else(|| "starting…".into()))
    };
    // One line, the whole text on hover: the reason can be a long driver message (no adapter
    // among the backends …), and the GPU state is process-wide, so it may change between frames;
    // a hint that wrapped would move every control below it under the pointer.
    ui.add(egui::Label::new(RichText::new(crate::i18n::tr(&status)).size(11.0).color(t.text_dim)).truncate()).on_hover_text(&status);
    gpu_card_row(app, ui, t);
    gpu_helper_row(app, ui, t);
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
    exports_section(app, ui, t);
    heading(ui, t, crate::i18n::tr("Thumbnail cache"));
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
        if r.clicked() {
            let _ = app.run("library.clearPreviews", json!({}));
        }
    });
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

#[cfg(test)]
mod tests {
    fn card(key: &str, name: &str, screen: bool, usable: bool) -> lightcraft_engine::gpu::CardInfo {
        lightcraft_engine::gpu::CardInfo {
            key: key.into(),
            name: name.into(),
            kind: "dedicated",
            memory: Some(12 << 30),
            backend: "dx12".into(),
            drives_screen: screen,
            usable,
            why_not: (!usable).then(|| "a software renderer".into()),
        }
    }

    /// Graphics card for editing: Automatic names the screen's card, then every card that can
    /// render (identical ones numbered); a saved card that isn't there stays a choice, marked.
    #[test]
    fn graphics_card_choices_name_the_cards() {
        let cards = [
            card("dx12:10de:2f04:NVIDIA GeForce RTX 5070#1", "NVIDIA GeForce RTX 5070", true, true),
            card("dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1", "NVIDIA GeForce RTX 4070 SUPER", false, true),
            card("dx12:1414:008c:Microsoft Basic Render Driver#1", "Microsoft Basic Render Driver", false, false),
        ];
        let o = super::gpu_card_choices(&cards, "");
        let labels: Vec<&str> = o.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, ["Automatic (NVIDIA GeForce RTX 5070 — your screen's card)", "NVIDIA GeForce RTX 5070", "NVIDIA GeForce RTX 4070 SUPER"]);
        assert_eq!(o[0].0, "", "Automatic saves as empty");
        assert_eq!(o[2].0, "dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1");
        // a saved card that isn't there
        let gone = "dx12:10de:2b85:NVIDIA GeForce RTX 5090#1";
        let o = super::gpu_card_choices(&cards, gone);
        assert_eq!(o.last().map(|(k, l)| (k.as_str(), l.as_str())), Some((gone, "NVIDIA GeForce RTX 5090 (not found)")));
        // identical cards, and no screen known (Automatic alone)
        let twins = [
            card("dx12:10de:2684:NVIDIA GeForce RTX 4090#1", "NVIDIA GeForce RTX 4090", false, true),
            card("dx12:10de:2684:NVIDIA GeForce RTX 4090#2", "NVIDIA GeForce RTX 4090", false, true),
        ];
        let labels: Vec<String> = super::gpu_card_choices(&twins, "").into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["Automatic", "NVIDIA GeForce RTX 4090 (1)", "NVIDIA GeForce RTX 4090 (2)"]);
        assert_eq!(super::gpu_card_choices(&[], "").len(), 1);
    }

    // The second card's choices leave out the editing card and name the card Automatic uses
    #[test]
    fn second_graphics_card_choices_leave_out_the_editing_card() {
        let cards = [
            card("dx12:10de:2f04:NVIDIA GeForce RTX 5070#1", "NVIDIA GeForce RTX 5070", true, true),
            card("dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1", "NVIDIA GeForce RTX 4070 SUPER", false, true),
            card("dx12:1414:008c:Microsoft Basic Render Driver#1", "Microsoft Basic Render Driver", false, false),
        ];
        let editing = super::editing_card_key(&cards, "", None);
        assert_eq!(editing.as_deref(), Some("dx12:10de:2f04:NVIDIA GeForce RTX 5070#1"), "Automatic edits on the screen's card");
        let o = super::gpu_helper_choices(&cards, editing.as_deref(), "");
        let labels: Vec<&str> = o.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, ["Automatic (NVIDIA GeForce RTX 4070 SUPER)", "Don't use", "NVIDIA GeForce RTX 4070 SUPER"]);
        assert_eq!((o[0].0.as_str(), o[1].0.as_str()), ("", "off"));
        // editing on the 4070: the 5070 helps
        let editing = super::editing_card_key(&cards, "dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1", None);
        let o = super::gpu_helper_choices(&cards, editing.as_deref(), "");
        assert_eq!(o[0].1, "Automatic (NVIDIA GeForce RTX 5070)");
        // one card: Automatic finds none
        let o = super::gpu_helper_choices(&cards[..1], Some("dx12:10de:2f04:NVIDIA GeForce RTX 5070#1"), "");
        assert_eq!(o[0].1, "Automatic (no other card found)");
        let ready = lightcraft_engine::gpu::HelperStatus { state: "ready", name: Some("NVIDIA GeForce RTX 4070 SUPER".into()), reason: None };
        assert_eq!(super::gpu_helper_line(&ready), "Helping now: NVIDIA GeForce RTX 4070 SUPER");
    }

    /// Every graphics-card choice fits beside the label column of the smallest Settings window
    /// (640×480: a 582-point page): one button can't wrap, so a longer one widened the
    /// Performance tab past the window on a machine with two cards.
    #[test]
    fn graphics_card_choices_fit_a_small_settings_window() {
        let cards = [
            card("dx12:10de:2f04:NVIDIA GeForce RTX 5070#1", "NVIDIA GeForce RTX 5070", true, true),
            card("dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1", "NVIDIA GeForce RTX 4070 SUPER", false, true),
        ];
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions(&[]));
        crate::theme::apply(&ctx);
        let mut labels: Vec<String> = Vec::new();
        for editing in [&cards[0].key, &cards[1].key] {
            labels.extend(super::gpu_helper_choices(&cards, Some(editing.as_str()), "").into_iter().map(|(_, l)| l));
        }
        labels.extend(super::gpu_card_choices(&cards, "").into_iter().map(|(_, l)| l));
        let page = 582.0f32;
        let controls = page - super::LABEL_W.min((page * 0.32).floor()).max(90.0);
        // (a first frame loads the fonts)
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let mut widest = 0.0f32;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            for l in &labels {
                let w = super::super::widgets::button_width(ui, l);
                widest = widest.max(w);
                assert!(w <= controls, "\"{l}\" is {w:.0} points wide, the controls {controls:.0}");
            }
        });
        out.textures_delta.clear();
        assert!(widest > 100.0, "the labels were measured: {widest}");
    }
}
