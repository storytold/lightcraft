//! The `postMessage` bridge to a same-origin parent page (`?host=parent`), for pages that embed
//! LightCraft in an `<iframe>` (such as a file manager) to edit one photo:
//!
//! - `lightcraft:ready` (to the parent): `{ version }`, the app is listening; send a photo now.
//! - `lightcraft:open` (from the parent): `{ name, bytes }`, bytes an `ArrayBuffer` or
//!   `Uint8Array` of any format LightCraft imports (JPEG, PNG, TIFF, WebP, DNG and camera raws…).
//!   It is imported like a dropped file, selected and shown in the Detail (edit) view.
//! - `lightcraft:save` (to the parent): `{ name, bytes }` (a transferred `ArrayBuffer`) for every
//!   file LightCraft would otherwise download: Export (`<photo name>.jpg` by default, or what the
//!   Export dialog chose), Export with Previous and the other exports. An export marks the photo
//!   clean.
//! - `lightcraft:dirty` (to the parent): `{ dirty }` whenever the opened photo's edit settings
//!   differ from those it was opened or last exported with.
//! - `lightcraft:failed` (to the parent): `{ error }` when the app couldn't start, or when a
//!   `lightcraft:open` file is over the size cap ([`crate::backup::MAX_BYTES`]).
//!
//! Only the parent window on the page's own origin is heard or answered. In host mode the library
//! lives in memory only (no browser storage, no demo photos, no one-tab lock) and the parent guards
//! leaving the page. LightCraft has no user name, so `?author=` is ignored.

use std::cell::{Cell, RefCell};

use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

struct Bridge {
    parent: web_sys::Window,
    origin: String,
}

thread_local! {
    static BRIDGE: RefCell<Option<Bridge>> = const { RefCell::new(None) };
    /// Photos sent by the parent, imported on the next frame.
    static INBOX: RefCell<Vec<(String, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    /// The unsaved-changes state last reported to the parent.
    static REPORTED: Cell<Option<bool>> = const { Cell::new(None) };
    /// A file went to the parent since the last [`take_saved`].
    static SAVED: Cell<bool> = const { Cell::new(false) };
}

/// Connect to the parent when the page is framed; false when it isn't.
pub fn connect() -> bool {
    let Some(window) = web_sys::window() else { return false };
    let Some(parent) = window.parent().ok().flatten().filter(|p| !js_sys::Object::is(p, &window)) else { return false };
    let Ok(origin) = window.location().origin() else { return false };
    listen(&window, parent.clone(), origin.clone());
    BRIDGE.with(|b| *b.borrow_mut() = Some(Bridge { parent, origin }));
    true
}

pub fn is_hosted() -> bool {
    BRIDGE.with(|b| b.borrow().is_some())
}

/// In host mode: hand `bytes` to the parent instead of downloading them.
pub fn save(name: &str, bytes: &[u8]) -> Option<Result<(), String>> {
    BRIDGE.with(|b| {
        let b = b.borrow();
        let b = b.as_ref()?;
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let r = match message(&[("type", "lightcraft:save".into()), ("name", name.into()), ("bytes", buffer.clone().into())]) {
            Some(m) => b.parent.post_message_with_transfer(&m, &b.origin, &js_sys::Array::of1(&buffer)).map_err(|e| format!("couldn't hand {name} to the page: {e:?}")),
            None => Err("couldn't build the message".into()),
        };
        if r.is_ok() {
            SAVED.with(|s| s.set(true));
        }
        Some(r)
    })
}

/// Whether a file went to the parent since the last call.
pub fn take_saved() -> bool {
    SAVED.with(|s| s.replace(false))
}

/// Photos the parent sent since the last call.
pub fn take_opened() -> Vec<(String, Vec<u8>)> {
    INBOX.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

pub fn ready() {
    send(&[("type", "lightcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into())]);
}

pub fn failed(error: &str) {
    send(&[("type", "lightcraft:failed".into()), ("error", error.into())]);
}

/// Report the unsaved-changes state when it changed (called every frame).
pub fn dirty(dirty: bool) {
    if REPORTED.with(Cell::get) != Some(dirty) && send(&[("type", "lightcraft:dirty".into()), ("dirty", dirty.into())]) {
        REPORTED.with(|r| r.set(Some(dirty)));
    }
}

/// Post a message to the parent; false if it couldn't be sent (or there is none).
fn send(fields: &[(&str, JsValue)]) -> bool {
    BRIDGE.with(|b| b.borrow().as_ref().is_some_and(|b| message(fields).is_some_and(|m| b.parent.post_message(&m, &b.origin).is_ok())))
}

fn listen(window: &web_sys::Window, parent: web_sys::Window, origin: String) {
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
        if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
            return;
        }
        let data = event.data();
        if string(&data, "type").as_deref() != Some("lightcraft:open") {
            return;
        }
        let name = string(&data, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Photo.jpg".to_string());
        let Some(bytes) = bytes(&data) else { return };
        let Ok(bytes) = bytes else { return };
        INBOX.with(|q| q.borrow_mut().push((name, bytes)));
        crate::web::request_repaint();
    });
    if window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
        // The listener lives as long as the page.
        on_message.forget();
    }
}

fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
    let m = js_sys::Object::new();
    for (key, value) in fields {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
    }
    Some(m)
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}

/// `bytes` as an `ArrayBuffer` or `Uint8Array`, copied only when within the size cap; an
/// oversize file is reported to the parent (`lightcraft:failed`) and yields `Err`.
fn bytes(data: &JsValue) -> Option<Result<Vec<u8>, ()>> {
    let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    let array = if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
        array.clone()
    } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
        js_sys::Uint8Array::new(&bytes)
    } else {
        return None;
    };
    if let Some(error) = crate::backup::open_size_error(u64::from(array.byte_length())) {
        failed(&error);
        return Some(Err(()));
    }
    Some(Ok(array.to_vec()))
}
