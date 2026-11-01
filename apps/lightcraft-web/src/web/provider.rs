//! Web library adapter: immutable native backups, correlated reads, and no change to the
//! catalog's ordinary local autosave. Provider acknowledgements describe backup revisions.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lightcraft_engine::Session;
use lightcraft_engine::catalog::Source;
use lightcraft_engine::library::LibraryStores;
use lightcraft_ui_egui::file_providers::{Capture, Command, Host};
use serde_json::json;
use wasm_bindgen::{JsCast, closure::Closure};

use super::WebApp;
use crate::backend::Backend;
use crate::files::{Files, LIBRARY_FILES, Snapshot};
use crate::store::Originals;

struct Loaded {
    session: Session,
    files: Files,
    dir: String,
    source_generation: u64,
}

enum Completion {
    Save { request: String, capture: Capture, destination: String, result: Result<Vec<u8>, String> },
    Open { request: String, name: String, document: u64, result: Result<Box<Loaded>, String> },
}

pub(super) struct Bridge {
    host: Host,
    saved: Option<(String, u64)>,
    loading: bool,
    waiting: Option<PendingOpen>,
    completed: Rc<RefCell<Vec<Completion>>>,
    leave: Rc<Cell<bool>>,
}

impl Bridge {
    pub(super) fn new(host: Host) -> Self {
        let leave = Rc::new(Cell::new(false));
        let flag = leave.clone();
        let provider = host.clone();
        let before = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            if provider.connected() && (flag.get() || provider.busy()) {
                event.prevent_default();
                let _ = js_sys::Reflect::set(event.as_ref(), &"returnValue".into(), &"".into());
            }
        });
        if let Some(window) = web_sys::window() {
            if window.add_event_listener_with_callback("beforeunload", before.as_ref().unchecked_ref()).is_ok() {
                before.forget();
            }
        }
        Self { host, saved: None, loading: false, waiting: None, completed: Rc::default(), leave }
    }

    pub(super) fn loading(&self) -> bool {
        self.loading
    }

    fn connection(&self) -> String {
        self.host.connection_id().unwrap_or_default()
    }

    fn note_saved(&mut self, revision: u64) {
        self.saved = Some((self.connection(), revision));
    }

    fn dirty(&self, revision: &Result<u64, String>) -> bool {
        let connection = self.connection();
        !matches!((&self.saved, revision), (Some((saved_connection, saved)), Ok(current))
            if *saved_connection == connection && saved == current)
    }

    fn stop_loading(&mut self) {
        self.loading = false;
        super::LIBRARY_SWITCHING.with(|flag| flag.set(false));
    }
}

fn names(session: &Session) -> Result<HashMap<String, String>, String> {
    let mut names = HashMap::new();
    for photo in session.catalog.photos() {
        if let Source::File { path } = &photo.source {
            let hash = crate::store::hash_of_path(path)
                .filter(|hash| crate::backup::valid_hash(hash))
                .ok_or("This library references an original outside browser storage; import it before saving a provider copy")?;
            names.insert(hash.to_string(), photo.file_name.clone());
        }
    }
    Ok(names)
}

fn archive_hashes(bytes: &[u8]) -> Result<HashSet<String>, String> {
    let len = bytes.len() as u64;
    let tail = crate::backup::tail_len(len) as usize;
    let (start, size) = crate::backup::find_central(&bytes[bytes.len() - tail..])?;
    let end = start.checked_add(size).ok_or("Invalid backup directory range")?;
    let directory = bytes
        .get(usize::try_from(start).map_err(|_| "Backup offset overflow")?..usize::try_from(end).map_err(|_| "Backup offset overflow")?)
        .ok_or("Backup directory is outside the supplied file")?;
    Ok(crate::backup::parse_central(directory)?
        .into_iter()
        .filter_map(|entry| {
            let rest = entry.name.strip_prefix("originals/")?;
            let (hash, _) = rest.split_once('/')?;
            Some(hash.to_string())
        })
        .collect())
}

async fn load(name: &str, bytes: Vec<u8>, backend: Backend, originals: Originals) -> Result<Loaded, String> {
    let available = archive_hashes(&bytes)?;
    let parts = js_sys::Array::new();
    parts.push(&js_sys::Uint8Array::from(bytes.as_slice()));
    let file = web_sys::File::new_with_u8_array_sequence(&parts, name).map_err(|e| format!("{e:?}"))?;
    let dir = crate::safety::restore_staged(file, backend.clone()).await?;
    let result: Result<Loaded, String> = async {
        let files = Files::default();
        let mut total = 0usize;
        for entry in LIBRARY_FILES {
            if let Some(bytes) = backend.read_capped(&super::library_key(&dir, entry), (80 * 1024 * 1024usize).saturating_sub(total)).await? {
                total = total.checked_add(bytes.len()).ok_or("Backup metadata size overflow")?;
                files.preload(entry, bytes);
            }
        }
        let source_generation = files.generation()?;
        let mut session = Session::new();
        originals.install(&mut session);
        session
            .open_library_in(
                LibraryStores {
                    dir: format!("browser:{}/{dir}", backend.kind()).into(),
                    catalog: Box::new(files.store()),
                    files: Box::new(files.store()),
                    on_disk: false,
                },
                false,
            )
            .map_err(|e| e.to_string())?;
        for hash in names(&session)?.keys() {
            if !available.contains(hash) {
                return Err("The backup omits an original referenced by its catalog".into());
            }
        }
        session.media.preview_capacity = 3;
        // Input is blocked during this asynchronous switch. Activate only a validated catalog,
        // after the old library's pending local writes have settled.
        backend.write(crate::backup::ACTIVE_LIBRARY, dir.as_bytes()).await?;
        Ok(Loaded { session, files, dir: dir.clone(), source_generation })
    }
    .await;
    match result {
        Ok(loaded) => Ok(loaded),
        Err(error) => Err(crate::safety::failed_staged_restore(&backend, &dir, error).await),
    }
}

struct SaveChoice {
    request: String,
    doc: u64,
    destination: String,
    name: String,
    save_as: bool,
}

struct CapturedLibrary {
    capture: Capture,
    snapshot: Snapshot,
    backend: Backend,
    originals: HashMap<String, String>,
}

struct PendingOpen {
    request: String,
    name: String,
    bytes: Vec<u8>,
    document: u64,
}

struct OpenedLibrary {
    request: String,
    name: String,
    document: u64,
    loaded: Box<Loaded>,
}

fn reject_queued_commands() {
    super::LIBRARY_SWITCHING.with(|flag| flag.set(true));
    let queued = super::INBOX.with(|inbox| std::mem::take(&mut *inbox.borrow_mut()));
    for (_, _, (_, reject)) in queued {
        let _ = reject.call1(&wasm_bindgen::JsValue::NULL, &"The original library is being replaced; queued commands were cancelled".into());
    }
}

impl WebApp {
    pub(super) fn provider_tick(&mut self, ctx: &egui::Context) {
        let Some(mut bridge) = self.provider.take() else { return };
        let completed = std::mem::take(&mut *bridge.completed.borrow_mut());
        for result in completed {
            self.complete_provider_operation(&mut bridge, result, ctx);
        }
        for command in bridge.host.take_commands() {
            match command {
                Command::Save { request, doc, destination, name, save_as } => {
                    self.start_provider_save(&mut bridge, SaveChoice { request, doc, destination, name, save_as }, ctx);
                }
                Command::Result { capture, resource, status } => {
                    if status == "saved" && capture.doc == self.app.file_provider_document {
                        bridge.host.bind(&capture, resource);
                        bridge.note_saved(capture.revision);
                    }
                }
                Command::Open { request, name, bytes, .. } => {
                    if let Err(error) = self.begin_provider_open(&mut bridge, &request, name, bytes, ctx) {
                        bridge.host.opened(&request, None, &error);
                        crate::safety::notice(error);
                    }
                }
            }
        }
        self.resume_provider_open(&mut bridge, ctx);
        self.provider = Some(bridge);
        self.provider_publish();
    }

    fn complete_provider_operation(&mut self, bridge: &mut Bridge, completion: Completion, ctx: &egui::Context) {
        match completion {
            Completion::Save { request, capture, destination, result } => {
                let result = result.and_then(|bytes| {
                    if destination == "device" {
                        super::download(&capture.name, &bytes)?;
                        bridge.host.finish_device(&request);
                        Ok(())
                    } else {
                        bridge.host.capture(&request, capture, bytes)
                    }
                });
                if let Err(error) = result {
                    bridge.host.failed(&request, &error);
                    crate::safety::notice(error);
                }
            }
            Completion::Open { request, name, document, result } => {
                bridge.stop_loading();
                match result {
                    Ok(loaded) => self.admit_provider_library(bridge, OpenedLibrary { request, name, document, loaded }, ctx),
                    Err(error) => {
                        bridge.host.opened(&request, None, &error);
                        crate::safety::notice(error);
                    }
                }
            }
        }
    }

    fn admit_provider_library(&mut self, bridge: &mut Bridge, opened: OpenedLibrary, ctx: &egui::Context) {
        let OpenedLibrary { request, name, document, loaded } = opened;
        let Loaded { mut session, files, dir, source_generation } = *loaded;
        session.media.rendered = self.app.session.media.rendered.clone();
        self.app.replace_provider_library(session);
        self.restore_provider_ui(&files);
        self.files = files;
        self.lib_dir = dir;
        self.frozen = Rc::new(Cell::new(false));
        self.flushing = Rc::new(Cell::new(false));
        self.flush_failed_at = Rc::new(Cell::new(0.0));
        self.ui_written.clear();
        self.last_save = 0.0;
        self.app.services = super::services(self.originals.clone(), self.backend.clone(), self.files.clone(), self.frozen.clone(), ctx.clone());
        self.app.file_provider_document = document;
        bridge.note_saved(source_generation);
        let revision = self.files.generation().unwrap_or(u64::MAX);
        bridge.host.opened(&request, Some(Capture { doc: document, revision, name, save_as: false }), "");
    }

    fn restore_provider_ui(&mut self, files: &Files) {
        let Some(bytes) = files.get("ui.json") else { return };
        let mut saved = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default();
        if let Some(fields) = saved.as_object_mut() {
            lightcraft_ui_egui::docking::drop_outdated(fields);
        }
        if let Ok(ui) = serde_json::from_value::<lightcraft_ui_egui::UiState>(saved) {
            self.app.ui = ui.sanitized();
        }
    }

    fn capture_provider_ui(&mut self) -> Result<(), String> {
        self.app.session.end_interaction().map_err(|error| error.to_string())?;
        self.app.session.save_view();
        let ui = serde_json::to_vec_pretty(&self.app.ui).map_err(|error| error.to_string())?;
        self.files.write("ui.json", &ui);
        self.ui_written = ui;
        Ok(())
    }

    fn capture_provider_library(&mut self, bridge: &Bridge, choice: &SaveChoice) -> Result<CapturedLibrary, String> {
        if bridge.loading || choice.doc != self.app.file_provider_document || self.frozen.get() {
            return Err("The selected library is no longer available for saving".into());
        }
        if !choice.name.to_ascii_lowercase().ends_with(".zip") {
            return Err("Library backups require a .zip filename".into());
        }
        if let Some(why) = crate::backup::unsaved_refusal(self.app.session.unsaved()) {
            return Err(why);
        }
        self.capture_provider_ui()?;
        if let Some(why) = crate::backup::unsaved_refusal(self.app.session.unsaved()) {
            return Err(why);
        }
        let snapshot = self.files.snapshot()?;
        let backend = self.backend.clone().ok_or("Provider backups require persistent browser storage for the original photos")?;
        let originals = names(&self.app.session)?;
        let capture = Capture { doc: choice.doc, revision: snapshot.generation, name: choice.name.clone(), save_as: choice.save_as };
        Ok(CapturedLibrary { capture, snapshot, backend, originals })
    }

    fn start_provider_save(&mut self, bridge: &mut Bridge, choice: SaveChoice, ctx: &egui::Context) {
        let work = match self.capture_provider_library(bridge, &choice) {
            Ok(work) => work,
            Err(error) => {
                bridge.host.failed(&choice.request, &error);
                crate::safety::notice(error);
                return;
            }
        };
        let CapturedLibrary { capture, snapshot, backend, originals } = work;
        let SaveChoice { request, destination, .. } = choice;
        let done = bridge.completed.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::safety::provider_backup(snapshot, backend, originals).await;
            done.borrow_mut().push(Completion::Save { request, capture, destination, result });
            ctx.request_repaint();
        });
    }

    fn begin_provider_open(&mut self, bridge: &mut Bridge, request: &str, name: String, bytes: Vec<u8>, ctx: &egui::Context) -> Result<(), String> {
        self.app.can_admit_provider_library(ctx)?;
        if bridge.loading
            || bridge.host.pending_for(self.app.file_provider_document)
            || self.frozen.get()
            || super::IMPORT_READS.with(|n| n.get() != 0)
            || self.originals.imports_waiting()
        {
            return Err("Finish the current import/save before opening a provider library".into());
        }
        if self.backend.is_none() || !name.to_ascii_lowercase().ends_with(".zip") {
            return Err("Select a native library ZIP backup in a browser with persistent storage".into());
        }
        let document = self.app.file_provider_document.checked_add(1).ok_or("Library instance counter exhausted")?;
        self.capture_provider_ui()?;
        self.save();
        reject_queued_commands();
        bridge.waiting = Some(PendingOpen { request: request.into(), name, bytes, document });
        bridge.loading = true;
        Ok(())
    }

    fn resume_provider_open(&mut self, bridge: &mut Bridge, ctx: &egui::Context) {
        if bridge.waiting.is_none() || self.flushing.get() {
            return;
        }
        if self.files.error().is_some() || self.files.is_dirty() {
            if let Some(pending) = bridge.waiting.take() {
                bridge.stop_loading();
                bridge.host.opened(&pending.request, None, "The current library could not be saved locally; it remains open");
            }
            return;
        }
        let Some(pending) = bridge.waiting.take() else { return };
        let Some(backend) = self.backend.clone() else {
            bridge.stop_loading();
            bridge.host.opened(&pending.request, None, "Persistent browser storage is unavailable; the current library remains open");
            return;
        };
        let originals = self.originals.clone();
        let done = bridge.completed.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = load(&pending.name, pending.bytes, backend, originals).await.map(Box::new);
            done.borrow_mut().push(Completion::Open { request: pending.request, name: pending.name, document: pending.document, result });
            ctx.request_repaint();
        });
    }

    pub(super) fn provider_publish(&mut self) {
        let Some(bridge) = &self.provider else { return };
        let revision = self.files.generation();
        // A checkpoint belongs to a provider connection as well as a library generation.
        // Reconnecting another account cannot inherit an old account's clean status.
        let dirty = bridge.dirty(&revision);
        self.app.file_provider_revision = revision.unwrap_or(u64::MAX);
        let doc = self.app.file_provider_document;
        bridge.host.publish_documents(
            json!([{"documentId":doc.to_string(),"revision":self.app.file_provider_revision,
            "name":bridge.host.filename(doc).unwrap_or_else(||"LightCraft library.zip".into()),"dirty":dirty,
            "pending":bridge.host.pending_for(doc),"loading":bridge.loading}]),
            &[doc],
        );
        bridge.leave.set(bridge.host.enabled() && dirty);
        crate::file_providers::publish(&bridge.host);
    }
}
