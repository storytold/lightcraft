//! One real browser fixture owns the page API and the temporary JavaScript fault injection.
//! It drives the actual WebApp admission/publisher paths, without WebRunner, a GPU or workers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use egui::{Context, Event, Id, Key, Modifiers, PointerButton, Pos2, Rect, Vec2};
use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use lightcraft_engine::{Selection, Session, catalog::PhotoId, library::LibraryStores};
use lightcraft_ui_egui::{
    LightcraftApp,
    file_providers::{Capture, Command, Host, Resource},
    widgets,
};
use serde_json::json;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

use super::{Bridge, WebApp, names};
use crate::backend::Backend;
use crate::backup::ACTIVE_LIBRARY;
use crate::files::Files;
use crate::store::{Originals, content_hash, storage_key};

wasm_bindgen_test_configure!(run_in_browser);

const API: &str = "lightcraftFileProviders";
const EVENT: &str = "lightcraft-file-provider";
const CURRENT_DIR: &str = "library-browser-draft-fixture";
const DRAFT_ERROR: &str = "Finish or cancel the current text edit, gesture or preview before opening a provider library";

// Only test code replaces Uint8Array.from. The real production catch binding is still called.
// Restore all overwritten descriptors in JS finally, including an escaping Rust/JS error.
#[wasm_bindgen(inline_js = r#"
export function lightcraftProviderBrowserTurnForTest() {
    return new Promise(resolve => setTimeout(resolve, 0));
}
export function lightcraftProviderDatabaseCountForTest(db) {
    return new Promise((resolve, reject) => {
        const request = db.transaction('files', 'readonly').objectStore('files').count();
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
    });
}
export function lightcraftProviderCopyFailureForTest(callback) {
    const fromDescriptor = Object.getOwnPropertyDescriptor(Uint8Array, 'from');
    const originalFrom = Uint8Array.from;
    const errorDescriptor = Object.getOwnPropertyDescriptor(console, 'error');
    const originalError = console.error;
    const warnDescriptor = Object.getOwnPropertyDescriptor(console, 'warn');
    const originalWarn = console.warn;
    let calls = 0;
    const logged = [];
    try {
        Object.defineProperty(Uint8Array, 'from', {
            configurable: true, writable: true,
            value() { calls += 1; throw new RangeError('injected LightCraft provider copy failure'); }
        });
        Object.defineProperty(console, 'error', {
            configurable: true, writable: true,
            value(...args) {
                logged.push(args.map(value => String(value)).join(' '));
                return originalError.apply(console, args);
            }
        });
        // The pinned eframe WebLogger emits Error records through console.warn with ERROR:.
        Object.defineProperty(console, 'warn', {
            configurable: true, writable: true,
            value(...args) {
                logged.push(args.map(value => String(value)).join(' '));
                return originalWarn.apply(console, args);
            }
        });
        callback();
    } finally {
        if (fromDescriptor) Object.defineProperty(Uint8Array, 'from', fromDescriptor);
        else Reflect.deleteProperty(Uint8Array, 'from');
        if (errorDescriptor) Object.defineProperty(console, 'error', errorDescriptor);
        else Reflect.deleteProperty(console, 'error');
        if (warnDescriptor) Object.defineProperty(console, 'warn', warnDescriptor);
        else Reflect.deleteProperty(console, 'warn');
    }
    return { calls, logged, restored: Uint8Array.from === originalFrom && console.error === originalError && console.warn === originalWarn };
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = lightcraftProviderBrowserTurnForTest)]
    fn browser_turn() -> js_sys::Promise;
    #[wasm_bindgen(js_name = lightcraftProviderDatabaseCountForTest)]
    fn database_count(db: &web_sys::IdbDatabase) -> js_sys::Promise;
    #[wasm_bindgen(catch, js_name = lightcraftProviderCopyFailureForTest)]
    fn inject_copy_failure(callback: &Function) -> Result<JsValue, JsValue>;
}

fn error(message: impl ToString) -> JsValue {
    JsValue::from_str(&message.to_string())
}

fn text(value: &JsValue, key: &str) -> Option<String> {
    Reflect::get(value, &key.into()).ok()?.as_string()
}

struct InstalledApi {
    window: web_sys::Window,
    value: JsValue,
    host: Host,
}

impl Drop for InstalledApi {
    fn drop(&mut self) {
        let _ = self.host.disconnect();
        if Reflect::get(&self.window, &API.into()).is_ok_and(|current| Object::is(&current, &self.value)) {
            let _ = Reflect::delete_property(self.window.unchecked_ref::<Object>(), &API.into());
        }
    }
}

struct PublishedEvents {
    window: web_sys::Window,
    callback: Closure<dyn FnMut(web_sys::Event)>,
    events: Array,
}

impl PublishedEvents {
    fn listen(window: &web_sys::Window) -> Result<Self, JsValue> {
        let events = Array::new();
        let recorded = events.clone();
        let callback = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            recorded.push(event.as_ref());
        });
        window.add_event_listener_with_callback(EVENT, callback.as_ref().unchecked_ref())?;
        Ok(Self { window: window.clone(), callback, events })
    }

    fn len(&self) -> u32 {
        self.events.length()
    }

    fn matching(&self, since: u32, kind: &str, request: Option<&str>) -> Vec<JsValue> {
        self.events
            .iter()
            .skip(since as usize)
            .filter_map(|value| {
                let event = value.dyn_into::<web_sys::CustomEvent>().ok()?;
                let detail = event.detail();
                (event.type_() == EVENT
                    && text(&detail, "type").as_deref() == Some(kind)
                    && request.is_none_or(|request| text(&detail, "requestId").as_deref() == Some(request)))
                .then_some(detail)
            })
            .collect()
    }
}

impl Drop for PublishedEvents {
    fn drop(&mut self) {
        let _ = self.window.remove_event_listener_with_callback(EVENT, self.callback.as_ref().unchecked_ref());
    }
}

async fn next_turn() -> Result<(), JsValue> {
    JsFuture::from(browser_turn()).await.map(|_| ())
}

async fn key_count(backend: &Backend) -> Result<u32, JsValue> {
    let Backend::Idb(db) = backend else {
        return Err(error("fixture requires real IndexedDB"));
    };
    let count = JsFuture::from(database_count(db)).await?.as_f64().ok_or_else(|| error("IndexedDB count was not numeric"))?;
    assert!(count.is_finite() && (0.0..=128.0).contains(&count));
    Ok(count as u32)
}

struct TinyLibrary {
    session: Session,
    files: Files,
    originals: Originals,
    original_key: String,
}

async fn tiny_library(backend: &Backend, dir: &str, filename: &str, width: usize) -> Result<TinyLibrary, JsValue> {
    let files = Files::default();
    let originals = Originals::default();
    let mut session = Session::new();
    originals.install(&mut session);
    session
        .open_library_in(
            LibraryStores {
                dir: format!("browser:idb/{dir}").into(),
                catalog: Box::new(files.store()),
                files: Box::new(files.store()),
                on_disk: false,
            },
            false, // Never seed the large procedural demo library.
        )
        .map_err(error)?;
    let bytes = crate::store::png_bytes(width, 3);
    let hash = content_hash(&bytes);
    let original_key = storage_key(&hash);
    backend.write(&original_key, &bytes).await.map_err(error)?;
    let path = originals.added(filename, &hash, Arc::from(bytes));
    let pending = originals.take_pending();
    assert_eq!(pending, vec![path]);
    let imported = session.execute("library.import", &json!({"paths":pending})).map_err(error)?;
    let id = PhotoId(imported["imported"][0].as_u64().ok_or_else(|| error("tiny PNG was not imported"))?);
    session.selection = Selection::single(id);
    session.save_view();
    session.persist().map_err(error)?;
    assert_eq!(session.catalog.len(), 1);
    assert!(!originals.imports_waiting());
    Ok(TinyLibrary { session, files, originals, original_key })
}

fn real_web_app(library: TinyLibrary, backend: Backend, host: Host, ctx: &Context) -> WebApp {
    let TinyLibrary { session, files, originals, .. } = library;
    let frozen = Rc::new(Cell::new(false));
    let mut app =
        LightcraftApp::new(session, super::super::services(originals.clone(), Some(backend.clone()), files.clone(), frozen.clone(), ctx.clone()));
    app.file_provider = Some(host.clone());
    app.file_provider_document = 1;
    // Every field belongs to the actual private production type; no mocked WebApp/method or eframe context.
    WebApp {
        provider: Some(Bridge::new(host)),
        app,
        originals,
        files,
        backend: Some(backend),
        workers: None,
        flushing: Rc::new(Cell::new(false)),
        flush_failed_at: Rc::new(Cell::new(0.0)),
        frozen,
        lib_dir: CURRENT_DIR.into(),
        bench: None,
        first_frame_logged: false,
        title: String::new(),
        last_save: 0.0,
        ui_written: Vec::new(),
    }
}

async fn settle_writes(app: &mut WebApp) -> Result<(), JsValue> {
    for _ in 0..2000 {
        app.save();
        if let Some(why) = app.files.error() {
            return Err(error(why));
        }
        if !app.flushing.get() && !app.files.is_dirty() {
            return Ok(());
        }
        next_turn().await?;
    }
    Err(error("bounded tiny-library flush did not settle"))
}

fn pointer(at: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

fn key(key: Key, pressed: bool) -> Event {
    Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }
}

fn slider_frame(app: &mut LightcraftApp, ctx: &Context, events: Vec<Event>) -> Id {
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 120.0))),
        time: Some(ctx.cumulative_pass_nr() as f64 / 10.0),
        events,
        ..Default::default()
    };
    let spec = lightcraft_engine::develop::controls::find("light.exposure").unwrap();
    let mut draft = Id::unique("unrendered-provider-browser-draft");
    // These are actual egui inputs/widgets, not a hand-inserted draft or raw-input barrier.
    // DOM keyboard routing and the full eframe UI pass are outside this fixture's scope.
    ctx.run_ui(raw, |ui| {
        draft = ui.scope_id().with(spec.id).with("typing");
        let settings = app.session.develop_of(app.session.active().unwrap()).unwrap();
        let value = lightcraft_engine::develop::controls::get(&settings, spec.id).unwrap();
        if let Some(value) = widgets::slider(ui, spec, value, true, None).value {
            app.run("develop.set", json!({"control":spec.id,"value":value})).unwrap();
        }
    })
    .textures_delta
    .clear();
    draft
}

fn enter_draft(app: &mut LightcraftApp, ctx: &Context) -> Id {
    slider_frame(app, ctx, vec![]);
    slider_frame(app, ctx, vec![]);
    let at = widgets::take_registry(ctx).into_iter().rev().find(|(name, _)| name == "sliderValue:light.exposure").unwrap().1.center();
    slider_frame(app, ctx, vec![Event::PointerMoved(at), pointer(at, true)]);
    slider_frame(app, ctx, vec![pointer(at, false)]);
    slider_frame(app, ctx, vec![]);
    let draft = slider_frame(app, ctx, vec![Event::Text("not-a-number".into())]);
    assert_eq!(ctx.data(|data| data.get_temp::<(String, u8, bool)>(draft)).unwrap().0, "not-a-number");
    draft
}

fn assert_idle_input(ctx: &Context) {
    assert!(ctx.input(|input| input.events.is_empty() && input.keys_down.is_empty() && !input.pointer.any_down()));
}

fn hidden_idle_frame(ctx: &Context) {
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 120.0))),
        time: Some(ctx.cumulative_pass_nr() as f64 / 10.0),
        ..Default::default()
    };
    // A parked editor is not drawn: drawing its unfocused text field would retire the draft.
    ctx.run_ui(raw, |_| {}).textures_delta.clear();
    assert_idle_input(ctx);
}

async fn admit_valid_zip(app: &mut WebApp, ctx: &Context, host: &Host, events: &PublishedEvents, zip: Vec<u8>) -> Result<(), JsValue> {
    assert!(app.app.can_admit_provider_library(ctx).is_ok());
    let request = host.request_open("incoming").map_err(error)?;
    let since = events.len();
    host.open(request.clone(), "Incoming.zip".into(), zip).map_err(error)?;
    let old_document = app.app.file_provider_document;
    for _ in 0..2000 {
        app.provider_tick(ctx);
        let failures = events.matching(since, "openFailed", Some(&request));
        assert!(
            failures.is_empty(),
            "valid ZIP failed after explicit draft cancellation: {:?}",
            failures.first().and_then(|value| text(value, "error"))
        );
        let opened = events.matching(since, "opened", Some(&request));
        if !opened.is_empty() {
            assert_eq!(opened.len(), 1);
            assert_eq!(text(&opened[0], "resourceId").as_deref(), Some("incoming"));
            assert_eq!(app.app.file_provider_document, old_document + 1);
            assert!(host.bound(app.app.file_provider_document));
            assert!(!host.busy());
            return Ok(());
        }
        next_turn().await?;
    }
    Err(error("bounded real ZIP admission did not finish"))
}

fn capture_for_dispatch(app: &WebApp, host: &Host, ctx: &Context, events: &PublishedEvents, bytes: Vec<u8>) -> Result<(String, Capture), JsValue> {
    let document = app.app.file_provider_document;
    let revision = app.files.generation().map_err(error)?;
    let since = events.len();
    host.choose(document, revision, "Retry.zip".into(), false).map_err(error)?;
    crate::file_providers::publish(host, ctx);
    let dialogs = events.matching(since, "saveDialog", None);
    assert_eq!(dialogs.len(), 1);
    let request = text(&dialogs[0], "requestId").ok_or_else(|| error("save dialog request missing"))?;
    host.resolve_dialog(&request, "provider", "Retry.zip").map_err(error)?;
    let commands = host.take_commands();
    assert!(matches!(commands.as_slice(), [Command::Save { request: queued, doc, destination, name, .. }]
        if queued == &request && *doc == document && destination == "provider" && name == "Retry.zip"));
    let capture = Capture { doc: document, revision, name: "Retry.zip".into(), save_as: false };
    host.capture(&request, capture.clone(), bytes).map_err(error)?;
    Ok((request, capture))
}

#[wasm_bindgen_test]
pub async fn page_local_provider_boundary_preserves_live_draft_and_recovers_failed_dispatch() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| error("real browser window required"))?;
    assert!(!Reflect::has(&window, &API.into())?, "fixture requires its own page, without editor startup");
    let backend = Backend::open(true).await.map_err(error)?;
    assert_eq!(key_count(&backend).await?, 0, "fixture requires a fresh isolated browser profile/database; never clear user data");
    let ctx = Context::default();
    lightcraft_ui_egui::theme::install_fonts(&ctx);
    let host = Host::default();
    crate::file_providers::install(host.clone(), ctx.clone())?;
    let _installed = InstalledApi { window: window.clone(), value: Reflect::get(&window, &API.into())?, host: host.clone() };
    assert!(!host.enabled() && !host.connected());
    let connection = host.connect("fixture".into(), "Fixture files".into(), "fixture-account".into()).map_err(error)?;
    host.set_resources(&connection, vec![Resource { id: "incoming".into(), name: "Incoming.zip".into(), updated_at: String::new() }])
        .map_err(error)?;
    let events = PublishedEvents::listen(&window)?;
    let current = tiny_library(&backend, CURRENT_DIR, "current.png", 4).await?;
    let current_original = current.original_key.clone();
    let mut app = real_web_app(current, backend.clone(), host.clone(), &ctx);
    settle_writes(&mut app).await?;
    backend.write(ACTIVE_LIBRARY, CURRENT_DIR.as_bytes()).await.map_err(error)?;
    let incoming = tiny_library(&backend, "library-browser-incoming-fixture", "incoming.png", 5).await?;
    let incoming_original = incoming.original_key.clone();
    let zip = crate::safety::provider_backup(incoming.files.snapshot().map_err(error)?, backend.clone(), names(&incoming.session).map_err(error)?)
        .await
        .map_err(error)?;
    assert!(zip.len() < 64 * 1024);
    let backing_count = key_count(&backend).await?;
    for parked in [false, true] {
        settle_writes(&mut app).await?;
        assert!(app.app.can_admit_provider_library(&ctx).is_ok(), "request must start while the editor is idle");
        let request = host.request_open("incoming").map_err(error)?;
        crate::file_providers::publish(&host, &ctx);
        let draft = enter_draft(&mut app.app, &ctx);
        slider_frame(&mut app.app, &ctx, vec![]);
        assert_idle_input(&ctx);
        assert!(ctx.text_edit_focused(), "the focused draft remains after its input has settled");
        if parked {
            ctx.memory_mut(|memory| memory.surrender_focus(widgets::text_field::id("sliderField:light.exposure")));
            hidden_idle_frame(&ctx);
            assert!(!ctx.text_edit_focused());
        }
        let draft_before = ctx.data(|data| data.get_temp::<(String, u8, bool)>(draft)).unwrap();
        let selection = app.app.session.selection.clone();
        let active = app.app.session.active().unwrap();
        let settings = app.app.session.develop_of(active).unwrap();
        let generation = app.app.session.library_generation();
        let library = app.app.session.library.as_ref().unwrap().dir.clone();
        let snapshot = app.files.snapshot().map_err(error)?;
        let document = app.app.file_provider_document;
        assert!(app.backend.is_some() && !app.frozen.get() && !app.flushing.get() && !app.files.is_dirty());
        assert!(!app.originals.imports_waiting() && !host.pending_for(document));
        assert_eq!(super::super::IMPORT_READS.with(Cell::get), 0);
        assert!(app.app.session.interaction.is_none() && app.app.session.unsaved().is_none());
        assert!(!app.provider.as_ref().unwrap().loading && app.provider.as_ref().unwrap().waiting.is_none());
        assert_eq!(app.app.can_admit_provider_library(&ctx).unwrap_err(), DRAFT_ERROR);
        let since = events.len();
        host.open(request.clone(), "Incoming.zip".into(), zip.clone()).map_err(error)?;
        // Do not inspect/drain Host commands: this is the actual WebApp dispatch under test.
        app.provider_tick(&ctx);
        let refused = events.matching(since, "openFailed", Some(&request));
        // The guard-deletion mutation must fail HERE, before any decoder/setup assertion.
        assert!(
            app.provider.as_ref().is_some_and(|bridge| !bridge.loading && bridge.waiting.is_none())
                && refused.len() == 1
                && text(&refused[0], "resourceId").as_deref() == Some("incoming")
                && text(&refused[0], "error").as_deref() == Some(DRAFT_ERROR)
                && events.matching(since, "opened", Some(&request)).is_empty(),
            "live draft must keep provider admission closed"
        );
        assert_eq!(app.app.file_provider_document, document);
        assert_eq!(app.lib_dir, CURRENT_DIR);
        assert_eq!(app.app.session.library.as_ref().unwrap().dir, library);
        assert_eq!(app.app.session.library_generation(), generation);
        assert_eq!(app.app.session.selection, selection);
        assert_eq!(app.app.session.develop_of(active).unwrap(), settings);
        assert_eq!(app.files.snapshot().map_err(error)?, snapshot);
        assert_eq!(ctx.data(|data| data.get_temp::<(String, u8, bool)>(draft)), Some(draft_before));
        assert!(!host.busy());
        assert_eq!(backend.read_capped(ACTIVE_LIBRARY, 128).await.map_err(error)?, Some(CURRENT_DIR.as_bytes().to_vec()));
        assert_eq!(key_count(&backend).await?, backing_count, "refused admission must create no staging files");
        ctx.memory_mut(|memory| memory.request_focus(widgets::text_field::id("sliderField:light.exposure")));
        slider_frame(&mut app.app, &ctx, vec![key(Key::Escape, true)]);
        assert!(ctx.data(|data| data.get_temp::<(String, u8, bool)>(draft)).is_none());
        slider_frame(&mut app.app, &ctx, vec![key(Key::Escape, false)]);
        slider_frame(&mut app.app, &ctx, vec![]);
        assert_idle_input(&ctx);
        assert!(app.app.can_admit_provider_library(&ctx).is_ok());
    }
    admit_valid_zip(&mut app, &ctx, &host, &events, zip).await?;
    assert_ne!(app.lib_dir, CURRENT_DIR);
    assert!(app.lib_dir.starts_with("library-restored-"));
    assert_eq!(app.app.session.catalog.photo(app.app.session.active().unwrap()).unwrap().file_name, "incoming.png");
    assert_eq!(backend.read_capped(ACTIVE_LIBRARY, 128).await.map_err(error)?, Some(app.lib_dir.as_bytes().to_vec()));
    let saved_before = app.provider.as_ref().unwrap().saved.clone();
    let bound_before = host.filename(app.app.file_provider_document);
    app.app.run("photo.rate", json!({"rating":4})).map_err(error)?;
    app.capture_provider_ui().map_err(error)?;
    settle_writes(&mut app).await?;
    assert_ne!(saved_before.as_ref().unwrap().1, app.files.generation().map_err(error)?);
    let captured = crate::safety::provider_backup(app.files.snapshot().map_err(error)?, backend.clone(), names(&app.app.session).map_err(error)?)
        .await
        .map_err(error)?;
    // The publisher boundary owns a real admitted Host capture, with correctly encoded snapshot bytes.
    // Its failed Result remains queued for actual provider_tick; no test-only dispatch replacement.
    let (failed_request, failed_capture) = capture_for_dispatch(&app, &host, &ctx, &events, captured.clone())?;
    let later = host.request_open("incoming").map_err(error)?;
    host.cancel_open(later.clone(), "later terminal survived".into()).map_err(error)?;
    let since = events.len();
    let dispatch_host = host.clone();
    let dispatch_ctx = ctx.clone();
    let dispatch = Closure::<dyn FnMut()>::new(move || crate::file_providers::publish(&dispatch_host, &dispatch_ctx));
    eframe::WebLogger::init(log::LevelFilter::Error).ok();
    let injected = inject_copy_failure(dispatch.as_ref().unchecked_ref())?;
    assert_eq!(Reflect::get(&injected, &"calls".into())?.as_f64(), Some(1.0));
    assert_eq!(Reflect::get(&injected, &"restored".into())?.as_bool(), Some(true));
    let logged = Reflect::get(&injected, &"logged".into())?.dyn_into::<Array>()?;
    assert!(
        logged
            .iter()
            .any(|value| { value.as_string().is_some_and(|line| line.contains("ERROR:") && line.contains("file-provider event delivery failed")) })
    );
    assert!(events.matching(since, "save", Some(&failed_request)).is_empty(), "copy failed before the window received bytes");
    let later_events = events.matching(since, "openFailed", Some(&later));
    assert_eq!(later_events.len(), 1);
    assert_eq!(text(&later_events[0], "resourceId").as_deref(), Some("incoming"));
    assert_eq!(text(&later_events[0], "error").as_deref(), Some("later terminal survived"));
    assert!(
        host.result(failed_request.clone(), "saved".into(), "late-ack".into()).is_err(),
        "the failed capture cannot accept a durable acknowledgement"
    );
    assert!(host.pending_for(failed_capture.doc), "the correlated failure waits for its actual application tick");
    app.provider_tick(&ctx);
    assert!(!host.busy() && !host.pending_for(failed_capture.doc));
    assert_eq!(app.provider.as_ref().unwrap().saved, saved_before);
    assert_eq!(host.filename(failed_capture.doc), bound_before);
    let (retry, retry_capture) = capture_for_dispatch(&app, &host, &ctx, &events, captured.clone())?;
    let since = events.len();
    crate::file_providers::publish(&host, &ctx);
    let delivered = events.matching(since, "save", Some(&retry));
    assert_eq!(delivered.len(), 1);
    let bytes = Reflect::get(&delivered[0], &"bytes".into())?.dyn_into::<Uint8Array>()?;
    assert_eq!(bytes.to_vec(), captured);
    assert_eq!(Reflect::get(&delivered[0], &"revision".into())?.as_f64(), Some(retry_capture.revision as f64));
    assert!(host.pending_for(retry_capture.doc));
    assert_eq!(app.provider.as_ref().unwrap().saved, saved_before, "delivery alone is not persistence");
    host.result(retry, "saved".into(), "retry-resource".into()).map_err(error)?;
    app.provider_tick(&ctx);
    assert_eq!(app.provider.as_ref().unwrap().saved, Some((connection, retry_capture.revision)));
    assert_eq!(host.filename(retry_capture.doc).as_deref(), Some("Retry.zip"));
    assert!(!host.busy());
    host.disconnect().map_err(error)?;
    // Only keys created in the verified-empty isolated database are removed; never clear a store.
    let admitted = app.lib_dir.clone();
    assert_eq!(backend.read_capped(ACTIVE_LIBRARY, 128).await.map_err(error)?, Some(admitted.as_bytes().to_vec()));
    backend.remove(ACTIVE_LIBRARY).await.map_err(error)?;
    for dir in [CURRENT_DIR, admitted.as_str()] {
        for filename in crate::files::LIBRARY_FILES {
            backend.remove(&format!("{dir}/{filename}")).await.map_err(error)?;
        }
    }
    backend.remove(&current_original).await.map_err(error)?;
    backend.remove(&incoming_original).await.map_err(error)?;
    assert_eq!(key_count(&backend).await?, 0);
    Ok(())
}
