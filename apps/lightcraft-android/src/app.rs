#![forbid(unsafe_code)]
use super::platform::Bridge;
use eframe::egui;
use lightcraft_catalog::{FsStore, Store};
use lightcraft_engine::{
    Session,
    import::{ImportJob, ImportOptions, Prepared, commit_prepared},
};
use lightcraft_ui_egui::{LightcraftApp, Services, UiState};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};

use super::sources::Names;
struct ImportResult {
    prepared: Prepared,
    options: ImportOptions,
    now: String,
}

struct App {
    inner: LightcraftApp,
    bridge: Arc<Bridge>,
    store: FsStore,
    saved: Vec<u8>,
    last_save: Instant,
    last_poll: Instant,
    recursive: bool,
    names: Names,
    importing: Option<Receiver<ImportResult>>,
    cancel: Arc<AtomicBool>,
    gesture: super::gestures::ViewerGesture,
    touches: usize,
    original_before: Option<lightcraft_ui_egui::state::BeforeAfter>,
}

pub fn run(android: android_activity::AndroidApp) -> Result<(), String> {
    let dir = android.internal_data_path().ok_or("Android internal data directory is unavailable")?;
    let bridge = Bridge::new(android.clone())?;
    lightcraft_engine::memory::set_budget(512 << 20);
    let mut session = Session::new().with_system_clock();
    session.open_library(dir.join("library"), false).map_err(|e| e.to_string())?;
    let names: Names = Arc::default();
    let b = bridge.clone();
    super::sources::configure(&mut session, Arc::new(move |uri| b.read(uri)), names.clone());
    let b = bridge.clone();
    session.media.availability.set_probe(Arc::new(move |uri| {
        b.call(json!({"op":"exists", "uri":uri})).ok().and_then(|v| v.get("exists").and_then(Value::as_bool)).unwrap_or(false)
    }));
    let b = bridge.clone();
    let staging = dir.join("exports");
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let sequence = AtomicU64::new(0);
    let writer: lightcraft_ui_egui::SharedWrite = Arc::new(move |path, bytes| {
        let name = super::storage::export_name(path)?;
        let file = staging.join(format!("export-{}", sequence.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
        let result = b.call(json!({"op":"export", "path":file, "name":name})).map(|_| ());
        let _ = std::fs::remove_file(file);
        result
    });
    let sync_writer = writer.clone();
    let b = bridge.clone();
    let destination_bridge = bridge.clone();
    let exists_bridge = bridge.clone();
    let services = Services {
        export_exists: Some(Arc::new(move |path| {
            exists_bridge
                .call(json!({"op":"exportExists", "name":super::storage::export_name(path).unwrap_or("")}))
                .ok()
                .and_then(|v| v.get("exists").and_then(Value::as_bool))
                .unwrap_or(false)
        })),
        pick_export_folder: Some(Box::new(move || {
            let _ = destination_bridge.call(json!({"op":"destination"}));
            None
        })),
        write_shared: Some(writer),
        write: Some(Box::new(move |p, bytes| sync_writer(p, bytes))),
        pick_files: Some(Box::new(move || {
            let _ = b.call(json!({"op":"open"}));
            vec![]
        })),
        ..Default::default()
    };
    let mut inner = LightcraftApp::new(session, services);
    inner.renderer = lightcraft_ui_egui::render::Renderer::with_worker_threads(2);
    let mut store = FsStore::open(&dir).map_err(|e| e.to_string())?;
    let saved = store.read("ui.json").map_err(|e| e.to_string())?.unwrap_or_default();
    if !saved.is_empty() {
        match serde_json::from_slice::<UiState>(&saved) {
            Ok(ui) => inner.ui = ui.sanitized(),
            Err(e) => {
                store.write_atomic("ui.corrupt.json", &saved).map_err(|e| e.to_string())?;
                inner.notices.push(format!("Saved layout could not be read; preserved ui.corrupt.json: {e}"));
            }
        }
    } else {
        inner.ui.settings.memory_mb = 512;
        inner.ui.settings.preview_edge = 2048;
    }
    let mut options = eframe::NativeOptions { android_app: Some(android), ..Default::default() };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.device_descriptor = Arc::new(|adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("LightCraft Android UI"),
            required_limits: eframe::wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        });
    }
    bridge.call(json!({"op":"refresh", "recursive":false}))?;
    eframe::run_native(
        "LightCraft",
        options,
        Box::new(move |cc| {
            #[cfg(debug_assertions)]
            {
                inner = inner.with_control(crate::control_server::start(7980, cc.egui_ctx.clone()));
            }
            let ctx = cc.egui_ctx.clone();
            inner.session.media.availability.run_in_background(Arc::new(move || ctx.request_repaint()));
            Ok(Box::new(App {
                inner,
                bridge,
                store,
                saved,
                last_save: Instant::now(),
                last_poll: Instant::now(),
                recursive: false,
                names,
                importing: None,
                cancel: Arc::new(AtomicBool::new(false)),
                gesture: Default::default(),
                touches: 0,
                original_before: None,
            }))
        }),
    )
    .map_err(|e| e.to_string())
}

impl App {
    fn save(&mut self) -> Result<(), String> {
        self.inner.session.persist().map_err(|e| e.to_string())?;
        self.inner.session.save_view();
        let bytes = serde_json::to_vec(&self.inner.ui).map_err(|e| e.to_string())?;
        if bytes != self.saved {
            self.store.write_atomic("ui.json", &bytes).map_err(|e| e.to_string())?;
            self.saved = bytes;
        }
        self.last_save = Instant::now();
        Ok(())
    }
    fn request(&mut self, ctx: &egui::Context, value: Value) {
        if let Err(e) = self.bridge.call(value) {
            self.inner.toast_error(ctx, e);
        }
    }
    fn import(&mut self, files: &[Value], ctx: &egui::Context) -> Result<(), String> {
        if self.importing.is_some() {
            return Err("An import is already in progress. Cancel it or wait before rescanning.".into());
        }
        let mut paths = Vec::new();
        {
            let mut names = self.names.lock().map_err(|_| "Document index lock failed")?;
            for file in files {
                let uri = file.get("uri").and_then(Value::as_str).ok_or("Missing document URI")?;
                super::storage::validate_uri(uri)?;
                let name = file.get("name").and_then(Value::as_str).ok_or("Missing document name")?;
                names.insert(uri.into(), name.into());
                paths.push(uri.to_owned());
            }
        }
        let options = ImportOptions::default();
        let mut job = ImportJob::new(&mut self.inner.session, options.clone()).map_err(|e| e.to_string())?;
        let now = job.now().to_owned();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("saf-import".into())
            .spawn(move || {
                let prepared = job.prepare_files(paths, &cancel);
                let _ = tx.send(ImportResult { prepared, options, now });
                ctx.request_repaint();
            })
            .map_err(|e| e.to_string())?;
        self.importing = Some(rx);
        Ok(())
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if self.last_poll.elapsed() < Duration::from_millis(100) {
            return;
        }
        self.last_poll = Instant::now();
        match self.bridge.call(json!({"op":"poll"})) {
            Ok(reply) => {
                for event in reply.get("events").and_then(Value::as_array).into_iter().flatten() {
                    match event.get("kind").and_then(Value::as_str).unwrap_or_default() {
                        "files" => {
                            if let Some(files) = event.get("files").and_then(Value::as_array) {
                                if let Err(e) = self.import(files, ctx) {
                                    self.inner.toast_error(ctx, e);
                                }
                            }
                        }
                        "save" => {
                            if let Err(e) = self.save() {
                                self.inner.toast_error(ctx, e);
                            }
                        }
                        "memory" => self.inner.session.media.clear_sources(),
                        "back" => {
                            let _ = self.inner.run("view.back", json!({}));
                        }
                        "destination" => {
                            if let Some(lightcraft_ui_egui::state::Dialog::Export { dir, .. }) = &mut self.inner.ui.dialog {
                                *dir = event.get("value").and_then(Value::as_str).unwrap_or_default().into();
                            }
                        }
                        _ => self.inner.toast(ctx, event.get("value").and_then(Value::as_str).unwrap_or_default()),
                    }
                }
            }
            Err(e) => self.inner.toast_error(ctx, e),
        }
        if let Some(result) = self.importing.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.importing = None;
            match commit_prepared(&mut self.inner.session, &result.options, &result.now, result.prepared) {
                Ok(report) => {
                    // URI remains the identity; provider display names are catalog metadata.
                    let names = self.names.lock().map(|g| g.clone()).unwrap_or_default();
                    let ops: Vec<_> = self
                        .inner
                        .session
                        .catalog
                        .photos()
                        .filter_map(|p| {
                            let lightcraft_catalog::Source::File { path } = &p.source else { return None };
                            let name = names.get(path)?;
                            (name != &p.file_name).then(|| lightcraft_catalog::Op::Relink {
                                id: p.id,
                                file_name: name.clone(),
                                source: p.source.clone(),
                                format: None,
                            })
                        })
                        .collect();
                    for op in ops {
                        if let Err(e) = self.inner.session.commit("Document name", op) {
                            self.inner.toast_error(ctx, e.to_string());
                        }
                    }
                    self.inner.toast(ctx, format!("Imported {} photos; {} failed", report.imported.len(), report.failed.len()));
                    for (_, error) in report.failed {
                        self.inner.notices.push(error);
                    }
                    if let Err(e) = self.save() {
                        self.inner.toast_error(ctx, e);
                    }
                }
                Err(e) => self.inner.toast_error(ctx, e.to_string()),
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll(ctx);
        let original = self.gesture.original(ctx.input(|i| i.time));
        if original && self.original_before.is_none() {
            self.original_before = Some(self.inner.ui.before_after);
            self.inner.ui.before_after = lightcraft_ui_egui::state::BeforeAfter::Original;
        } else if !original && let Some(before) = self.original_before.take() {
            self.inner.ui.before_after = before;
        }
        self.inner.logic(ctx);
        if self.last_save.elapsed() >= Duration::from_secs(1) {
            if let Err(e) = self.save() {
                self.inner.toast_error(ctx, e);
                self.last_save = Instant::now();
            }
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        for event in &raw.events {
            if let egui::Event::Touch { phase, pos, .. } = event {
                match phase {
                    egui::TouchPhase::Start => {
                        self.touches = self.touches.saturating_add(1);
                        let viewer = self.inner.ui.tool.is_empty()
                            && self.inner.ui.view == lightcraft_ui_egui::state::ViewMode::Detail
                            && !matches!(
                                self.inner.ui.right,
                                lightcraft_ui_egui::state::RightPanel::Crop | lightcraft_ui_egui::state::RightPanel::Masking
                            )
                            && self.inner.image_rect.is_some_and(|r| r.contains(*pos));
                        if self.touches == 1 {
                            self.gesture.start(raw.time.unwrap_or_else(|| ctx.input(|i| i.time)), [pos.x, pos.y], viewer);
                        } else {
                            self.gesture.cancel();
                        }
                    }
                    egui::TouchPhase::Move => self.gesture.move_to([pos.x, pos.y]),
                    egui::TouchPhase::End => {
                        self.touches = self.touches.saturating_sub(1);
                        let direction = self.gesture.end([pos.x, pos.y], matches!(self.inner.ui.zoom, lightcraft_ui_egui::state::Zoom::Fit));
                        if direction != 0 {
                            let _ = self.inner.run(if direction > 0 { "library.next" } else { "library.previous" }, json!({}));
                        }
                    }
                    egui::TouchPhase::Cancel => {
                        self.touches = self.touches.saturating_sub(1);
                        self.gesture.cancel();
                    }
                }
            }
        }
        self.inner.raw_input_hook(raw);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("android-storage").show(ui, |ui| {
            // NativeActivity may composite the status bar over the surface even
            // when decor fitting is enabled. Reserve its typical inset here so
            // the toolbar is visible and receives taps at the same coordinates.
            ui.add_space(28.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open Folder").clicked() {
                    self.request(&ctx, json!({"op":"open", "recursive":self.recursive}));
                }
                if ui.button("Refresh").clicked() {
                    self.request(&ctx, json!({"op":"refresh", "recursive":self.recursive}));
                }
                ui.checkbox(&mut self.recursive, "Subfolders");
                if ui.button("Export Folder").clicked() {
                    self.request(&ctx, json!({"op":"destination"}));
                }
                if ui.button("Use Source Folder").clicked() {
                    self.request(&ctx, json!({"op":"sourceDestination"}));
                }
                if ui.button("Use Save As").clicked() {
                    self.request(&ctx, json!({"op":"documentMode"}));
                }
                if ui.button("Share Export").clicked() {
                    self.request(&ctx, json!({"op":"share"}));
                }
                if ui.button(if self.inner.ui.fullscreen { "Restore Panels" } else { "Fullscreen" }).clicked() {
                    self.inner.ui.fullscreen = !self.inner.ui.fullscreen;
                }
                if self.importing.is_some() {
                    ui.spinner();
                    if ui.button("Cancel Import").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                }
            });
        });
        self.inner.ui(ui);
    }
    fn on_exit(&mut self) {
        if let Err(e) = self.save() {
            log::error!("{e}");
        }
        if let Err(e) = self.inner.session.close_library() {
            log::error!("{e}");
        }
    }
}
