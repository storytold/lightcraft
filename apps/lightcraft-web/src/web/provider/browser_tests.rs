//! One real browser fixture owns the page API and the temporary JavaScript fault injection.
//! It drives the actual WebApp admission/publisher paths, without WebRunner, a GPU or workers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::Cell;
use std::future::{Future, poll_fn};
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;

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
export function lightcraftProviderUnloadForTest() {
    const event = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(event);
    return event.defaultPrevented;
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
    #[wasm_bindgen(catch, js_name = lightcraftProviderUnloadForTest)]
    fn unload_prompted() -> Result<bool, JsValue>;
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
    let mut app = LightcraftApp::new(
        session,
        super::super::services(originals.clone(), Some(backend.clone()), files.clone(), frozen.clone(), ctx.clone(), None),
    );
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
        bridge: None,
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
    let old_activity = app.app.session.activity.clone();
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
            assert!(old_activity.list().is_empty(), "the replaced session must not retain its open row");
            assert!(app.app.session.activity.list().is_empty());
            return Ok(());
        }
        next_turn().await?;
    }
    Err(error("bounded real ZIP admission did not finish"))
}

fn refuse_replacement_during_drop(app: &mut WebApp, ctx: &Context, host: &Host, events: &PublishedEvents, zip: &[u8]) -> Result<(), JsValue> {
    assert_idle_input(ctx);
    assert!(app.app.can_admit_provider_library(ctx).is_ok(), "the import owner must be the admission barrier");
    assert!(!app.frozen.get() && !app.flushing.get() && !app.files.is_dirty());
    let document = app.app.file_provider_document;
    let generation = app.app.session.library_generation();
    let library = app.app.session.library.as_ref().unwrap().dir.clone();
    let selection = app.app.session.selection.clone();
    let snapshot = app.files.snapshot().map_err(error)?;
    let request = host.request_open("incoming").map_err(error)?;
    crate::file_providers::publish(host, ctx);
    let since = events.len();
    host.open(request.clone(), "Incoming.zip".into(), zip.to_vec()).map_err(error)?;
    app.provider_tick(ctx);
    let refused = events.matching(since, "openFailed", Some(&request));
    assert!(
        app.provider.as_ref().is_some_and(|bridge| !bridge.loading && bridge.waiting.is_none())
            && refused.len() == 1
            && text(&refused[0], "resourceId").as_deref() == Some("incoming")
            && text(&refused[0], "error").as_deref() == Some("Finish the current import/save before opening a provider library")
            && events.matching(since, "opened", Some(&request)).is_empty(),
        "deferred dropped bytes must keep provider admission closed"
    );
    assert_eq!(app.app.file_provider_document, document);
    assert_eq!(app.app.session.library_generation(), generation);
    assert_eq!(app.app.session.library.as_ref().unwrap().dir, library);
    assert_eq!(app.app.session.selection, selection);
    assert_eq!(app.files.snapshot().map_err(error)?, snapshot);
    assert!(!host.busy());
    assert_eq!(
        crate::safety::take_notices(),
        vec!["Finish the current import/save before opening a provider library".to_owned()],
        "each refused replacement must report only its expected admission notice"
    );
    Ok(())
}

async fn deferred_drop_lifetimes(app: &mut WebApp, ctx: &Context, host: &Host, events: &PublishedEvents, zip: &[u8]) -> Result<String, JsValue> {
    assert_eq!(super::super::IMPORT_READS.with(Cell::get), 0);
    assert!(!app.originals.imports_waiting());
    assert!(crate::safety::take_notices().is_empty());
    hidden_idle_frame(ctx);
    // Cancel both before the first poll and while the production byte-read task is suspended.
    for polled in [false, true] {
        let mut cancelled = Box::pin(super::super::read_dropped_file(
            app.originals.clone(),
            app.backend.clone(),
            "cancelled-drop.png".into(),
            std::future::pending::<Result<Vec<u8>, String>>(),
            ctx.clone(),
        ));
        refuse_replacement_during_drop(app, ctx, host, events, zip)?;
        if polled {
            poll_fn(|cx| {
                assert!(cancelled.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            refuse_replacement_during_drop(app, ctx, host, events, zip)?;
        }
        assert_eq!(super::super::IMPORT_READS.with(Cell::get), 1);
        drop(cancelled);
        assert_eq!(super::super::IMPORT_READS.with(Cell::get), 0);
        assert!(!app.originals.imports_waiting());
    }
    let png = crate::store::png_bytes(6, 3);
    let hash = content_hash(&png);
    let stored_key = storage_key(&hash);
    for success in [false, true] {
        let mut resolve = None;
        let mut reject = None;
        let promise = js_sys::Promise::new(&mut |yes, no| {
            resolve = Some(yes);
            reject = Some(no);
        });
        let bytes = async move {
            JsFuture::from(promise).await.map(|value| Uint8Array::new(&value).to_vec()).map_err(|_| "fixture deferred read failure".to_owned())
        };
        let mut read =
            Box::pin(super::super::read_dropped_file(app.originals.clone(), app.backend.clone(), "deferred-drop.png".into(), bytes, ctx.clone()));
        // First before spawn_local could poll, then while the actual byte future is pending.
        refuse_replacement_during_drop(app, ctx, host, events, zip)?;
        poll_fn(|cx| {
            assert!(read.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        refuse_replacement_during_drop(app, ctx, host, events, zip)?;
        assert!(!app.originals.imports_waiting(), "no stored original can mask the pending byte read");
        assert_eq!(super::super::IMPORT_READS.with(Cell::get), 1);
        if success {
            resolve.ok_or_else(|| error("missing deferred resolve"))?.call1(&JsValue::UNDEFINED, &Uint8Array::from(png.as_slice()))?;
        } else {
            reject.ok_or_else(|| error("missing deferred reject"))?.call1(&JsValue::UNDEFINED, &"fixture deferred read failure".into())?;
        }
        read.await;
        assert_eq!(super::super::IMPORT_READS.with(Cell::get), 0);
        if success {
            assert!(app.originals.imports_waiting());
            refuse_replacement_during_drop(app, ctx, host, events, zip)?;
            let backend = app.backend.as_ref().ok_or_else(|| error("missing drop storage"))?;
            assert_eq!(backend.read_capped(&stored_key, 64 * 1024).await.map_err(error)?, Some(png.clone()));
            let count = app.app.session.catalog.len();
            let document = app.app.file_provider_document;
            app.import_dropped(ctx);
            assert!(!app.originals.imports_waiting());
            assert_eq!(app.app.session.catalog.len(), count + 1, "the actual drop consumer imported into the original library");
            assert_eq!(app.app.file_provider_document, document);
            assert_eq!(app.lib_dir, CURRENT_DIR);
            let path = crate::store::original_path(&hash, "deferred-drop.png");
            assert!(
                app.app
                    .session
                    .catalog
                    .photos()
                    .any(|photo| matches!(&photo.source, lightcraft_engine::catalog::Source::File { path: source } if source == &path))
            );
            assert_eq!(app.originals.get(&path).unwrap().as_ref(), png.as_slice());
            settle_writes(app).await?;
        } else {
            assert!(!app.originals.imports_waiting());
            assert_eq!(
                crate::safety::take_notices(),
                vec![lightcraft_ui_egui::i18n::dropped_file_unreadable("deferred-drop.png", "fixture deferred read failure")]
            );
        }
    }
    assert!(crate::safety::take_notices().is_empty());
    Ok(stored_key)
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

fn start_tracked_save(app: &mut WebApp, host: &Host, ctx: &Context, events: &PublishedEvents, name: &str) -> Result<String, JsValue> {
    let since = events.len();
    host.choose(app.app.file_provider_document, app.files.generation().map_err(error)?, "Tracked.zip".into(), false).map_err(error)?;
    crate::file_providers::publish(host, ctx);
    let dialogs = events.matching(since, "saveDialog", None);
    assert_eq!(dialogs.len(), 1);
    let request = text(&dialogs[0], "requestId").ok_or_else(|| error("tracked save request missing"))?;
    host.resolve_dialog(&request, "provider", name).map_err(error)?;
    app.provider_tick(ctx);
    Ok(request)
}

async fn wait_provider_completion(app: &mut WebApp, ctx: &Context) -> Result<(), JsValue> {
    for _ in 0..2000 {
        if !app.provider.as_ref().unwrap().completed.borrow().is_empty() {
            return Ok(());
        }
        app.provider_tick(ctx);
        next_turn().await?;
    }
    Err(error("bounded provider worker did not finish"))
}

fn embedding_command(canvas: &web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
    let detail = Object::new();
    Reflect::set(&detail, &"id".into(), &"library.info".into())?;
    Reflect::set(&detail, &"params".into(), &"{}".into())?;
    let init = web_sys::CustomEventInit::new();
    init.set_detail(&detail);
    let event: web_sys::Event = web_sys::CustomEvent::new_with_event_init_dict("lightcraft:cmd", &init)?.into();
    canvas.dispatch_event(&event)?;
    Ok(())
}

async fn provider_activity_lifetimes(app: &mut WebApp, ctx: &Context, host: &Host, events: &PublishedEvents) -> Result<(), JsValue> {
    let activity = app.app.session.activity.clone();
    assert!(activity.list().is_empty());
    let saved_before = app.provider.as_ref().unwrap().saved.clone();
    let since = events.len();
    let refused = start_tracked_save(app, host, ctx, events, "Not-a-backup.txt")?;
    assert!(activity.list().is_empty(), "synchronous capture refusal must remove its row");
    assert_eq!(events.matching(since, "saveFailed", Some(&refused)).len(), 1);
    assert_eq!(crate::safety::take_notices(), vec!["Library backups require a .zip filename".to_owned()]);
    assert!(!host.busy());

    let request = start_tracked_save(app, host, ctx, events, "Tracked.zip")?;
    let rows = activity.list();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].detail.as_str(), rows[0].total), ("providerSave", "Exporting", "Tracked.zip", 0));
    assert!(!rows[0].cancellable && activity.cancel(rows[0].id).is_err());
    wait_provider_completion(app, ctx).await?;
    assert_eq!(activity.list().len(), 1, "completion retains the guard until the application consumes it");
    app.provider_tick(ctx);
    assert!(activity.list().is_empty());
    assert!(host.pending_for(app.app.file_provider_document), "local task completion is not a remote-save acknowledgement");
    host.result(request, "cancel".into(), String::new()).map_err(error)?;
    app.provider_tick(ctx);
    assert!(!host.busy() && activity.list().is_empty());
    assert_eq!(app.provider.as_ref().unwrap().saved, saved_before);

    settle_writes(app).await?;
    let canvas = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| error("fixture document missing"))?
        .create_element("canvas")?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    let embedding = crate::host::Bridge::for_page(&canvas);
    embedding.listen(ctx.clone());
    let rejected = Rc::new(Cell::new(0u32));
    let count = rejected.clone();
    let response = Closure::<dyn Fn(web_sys::Event)>::new(move |event: web_sys::Event| {
        let custom = event.dyn_ref::<web_sys::CustomEvent>().unwrap();
        if Reflect::get(&custom.detail(), &"ok".into()).unwrap().as_bool() == Some(false) {
            count.set(count.get() + 1);
        }
    });
    canvas.add_event_listener_with_callback("lightcraft:response", response.as_ref().unchecked_ref())?;
    app.bridge = Some(embedding.clone());
    embedding_command(&canvas)?;
    assert!(embedding.has_incoming());
    let request = host.request_open("incoming").map_err(error)?;
    host.open(request, "Queued-embedding.zip".into(), vec![0; 32]).map_err(error)?;
    app.provider_tick(ctx);
    assert!(activity.list().is_empty() && !host.busy(), "queued embedding work must keep provider admission closed");
    assert_eq!(crate::safety::take_notices(), vec!["Finish the current import/save before opening a provider library".to_owned()]);
    app.run_host(ctx);
    assert!(!embedding.has_incoming());

    let request = host.request_open("incoming").map_err(error)?;
    host.open(request, "Waiting.zip".into(), vec![0; 32]).map_err(error)?;
    app.flushing.set(true);
    app.provider_tick(ctx);
    let rows = activity.list();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].detail.as_str()), ("providerOpen", "Importing", "Waiting.zip"));
    assert!(!rows[0].cancellable && activity.cancel(rows[0].id).is_err());
    assert!(app.provider.as_ref().unwrap().waiting.is_some());
    embedding_command(&canvas)?;
    assert_eq!(rejected.get(), 1, "embedding commands must be refused while the provider is switching libraries");
    assert!(!embedding.has_incoming(), "the refused embedding command must not reach the replacement library");
    app.files.set_error(Some("injected local flush failure".into()));
    app.flushing.set(false);
    app.provider_tick(ctx);
    assert!(activity.list().is_empty(), "a failed flush must release the waiting open's guard");
    assert!(!host.busy() && !app.provider.as_ref().unwrap().loading);
    assert!(!super::super::LIBRARY_SWITCHING.with(Cell::get));
    app.files.set_error(None);
    settle_writes(app).await?;

    let since = events.len();
    let request = host.request_open("incoming").map_err(error)?;
    host.open(request.clone(), "Invalid.zip".into(), vec![0; 32]).map_err(error)?;
    app.provider_tick(ctx);
    assert_eq!(activity.list().len(), 1);
    wait_provider_completion(app, ctx).await?;
    assert_eq!(activity.list().len(), 1);
    app.provider_tick(ctx);
    assert!(activity.list().is_empty(), "an asynchronous ZIP error must remove its row");
    assert_eq!(events.matching(since, "openFailed", Some(&request)).len(), 1);
    assert!(!host.busy() && !app.provider.as_ref().unwrap().loading);
    assert!(!super::super::LIBRARY_SWITCHING.with(Cell::get));
    assert_eq!(crate::safety::take_notices().len(), 1);
    assert_eq!(app.provider.as_ref().unwrap().saved, saved_before);
    canvas.remove_event_listener_with_callback("lightcraft:response", response.as_ref().unchecked_ref())?;
    app.bridge = None;
    Ok(())
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
    let installed = InstalledApi { window: window.clone(), value: Reflect::get(&window, &API.into())?, host: host.clone() };
    let register_provider = Reflect::get(&installed.value, &"registerProvider".into())?.dyn_into::<Function>()?;
    let unregister_provider = Reflect::get(&installed.value, &"unregisterProvider".into())?.dyn_into::<Function>()?;
    let register = || {
        register_provider
            .call3(&installed.value, &"fixture".into(), &"Fixture files".into(), &"fixture-account".into())?
            .as_string()
            .ok_or_else(|| error("registerProvider did not return a connection ID"))
    };
    assert!(!host.enabled() && !host.connected());
    let events = PublishedEvents::listen(&window)?;
    let current = tiny_library(&backend, CURRENT_DIR, "current.png", 4).await?;
    let current_original = current.original_key.clone();
    let mut app = real_web_app(current, backend.clone(), host.clone(), &ctx);
    settle_writes(&mut app).await?;
    app.provider_publish(&ctx);
    assert!(!unload_prompted()?, "publishing the API alone must not change unload behavior");
    let connection = register()?;
    assert_eq!(host.connection_id().as_deref(), Some(connection.as_str()));
    assert!(!host.busy());
    assert!(unload_prompted()?, "a new connection must protect unload before its first publication");
    host.set_resources(&connection, vec![Resource { id: "incoming".into(), name: "Incoming.zip".into(), updated_at: String::new() }])
        .map_err(error)?;
    app.provider_publish(&ctx);
    assert!(!host.busy());
    assert!(unload_prompted()?, "a registered unsaved library needs unload protection");
    host.set_enabled(false).map_err(error)?;
    app.provider_publish(&ctx);
    assert!(!host.busy());
    assert!(unload_prompted()?, "pausing the provider must keep unsaved revision protection");
    host.set_enabled(true).map_err(error)?;
    backend.write(ACTIVE_LIBRARY, CURRENT_DIR.as_bytes()).await.map_err(error)?;
    let incoming = tiny_library(&backend, "library-browser-incoming-fixture", "incoming.png", 5).await?;
    let incoming_original = incoming.original_key.clone();
    let zip = crate::safety::provider_backup(incoming.files.snapshot().map_err(error)?, backend.clone(), names(&incoming.session).map_err(error)?)
        .await
        .map_err(error)?;
    assert!(zip.len() < 64 * 1024);
    provider_activity_lifetimes(&mut app, &ctx, &host, &events).await?;
    let dropped_original = deferred_drop_lifetimes(&mut app, &ctx, &host, &events, &zip).await?;
    assert_eq!(backend.read_capped(ACTIVE_LIBRARY, 128).await.map_err(error)?, Some(CURRENT_DIR.as_bytes().to_vec()));
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
    assert_eq!(app.provider.as_ref().unwrap().saved, Some((connection.clone(), retry_capture.revision)));
    assert_eq!(host.filename(retry_capture.doc).as_deref(), Some("Retry.zip"));
    app.provider_publish(&ctx);
    assert!(!host.busy());
    assert!(!unload_prompted()?, "a durably acknowledged unchanged revision is clean");
    assert_eq!(register()?, connection, "idempotent registration must retain its connection");
    assert!(!host.busy());
    assert!(!unload_prompted()?, "idempotent clean registration must retain its published clean state");
    assert_eq!(unregister_provider.call0(&installed.value)?.as_bool(), Some(true));
    assert!(!unload_prompted()?, "disconnect must immediately restore ordinary unload behavior");
    // No provider publication occurs between this clean connection's disconnect and reconnect.
    let reconnected = register()?;
    assert_ne!(reconnected, connection);
    assert!(!host.busy());
    assert!(unload_prompted()?, "a reconnected provider must not inherit the previous connection's clean publication");
    app.provider_publish(&ctx);
    assert!(unload_prompted()?, "the previous connection's acknowledgement cannot clean the new connection");
    let (reconnect_request, reconnect_capture) = capture_for_dispatch(&app, &host, &ctx, &events, captured.clone())?;
    let since = events.len();
    crate::file_providers::publish(&host, &ctx);
    assert_eq!(events.matching(since, "save", Some(&reconnect_request)).len(), 1);
    host.result(reconnect_request, "saved".into(), "reconnected-resource".into()).map_err(error)?;
    app.provider_tick(&ctx);
    assert_eq!(app.provider.as_ref().unwrap().saved, Some((reconnected, reconnect_capture.revision)));
    assert!(!host.busy());
    assert!(!unload_prompted()?, "only the new connection's acknowledged unchanged revision is clean");
    host.set_enabled(false).map_err(error)?;
    app.provider_publish(&ctx);
    assert!(!unload_prompted()?, "a paused clean provider must not add an unload prompt");
    app.app.run("photo.rate", json!({"rating":1})).map_err(error)?;
    app.capture_provider_ui().map_err(error)?;
    settle_writes(&mut app).await?;
    app.provider_publish(&ctx);
    assert!(!host.busy());
    assert!(unload_prompted()?, "edits made while paused still need unload protection");
    host.disconnect().map_err(error)?;
    app.provider_publish(&ctx);
    assert!(!unload_prompted()?, "disconnect must restore ordinary unload behavior");
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
    backend.remove(&dropped_original).await.map_err(error)?;
    assert_eq!(key_count(&backend).await?, 0);
    Ok(())
}
