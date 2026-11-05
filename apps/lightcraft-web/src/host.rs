//! The bridge to a host page (issue #2270): a same-origin page embeds LightCraft in an `<iframe>`
//! (`?host=parent`) or in its own document (`data-host="page"` on the canvas). The protocol is
//! the one PdfCraft, GridCraft and DeckCraft speak, with `lightcraft:` message types:
//!
//! - `lightcraft:ready` (to the parent): `{ version }`, the app is listening.
//! - `lightcraft:failed` (to the parent): `{ error }`, the app couldn't start, or it refused a
//!   `lightcraft:open` (a file larger than a library backup can hold, [`crate::backup::MAX_BYTES`]).
//! - `lightcraft:open` (from the parent): `{ name, bytes }`, bytes an `ArrayBuffer` or
//!   `Uint8Array`. The photo is added to the library, as one dropped on the window is; `name`
//!   gives the format (its extension), `photo.jpg` when missing.
//! - `lightcraft:save` (to the parent): `{ name, bytes }` (a transferred `ArrayBuffer`) for every
//!   file LightCraft would otherwise download: exports and the library backup (the preset, keyword
//!   and curve file dialogs have no web implementation yet). The parent stores it and reports
//!   failures itself.
//! - `lightcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away.
//!   On the web every change is written to browser storage a moment after it is made, so `dirty`
//!   means "closing the page right now would lose something": a write still queued or running,
//!   or changes that storage refused (quota).
//! - `lightcraft:cmd` (from the parent): `{ id, params }`, params a JSON string (`{}` when
//!   missing, empty or `null`; anything but a JSON object is refused unrun, see [`crate::automation::params`]).
//!   Runs the command by id, like `lightcraft.command(..)` from the page itself, and answers with
//!   `lightcraft:response` `{ ok, value }` (the JSON result, or the error text).
//!   Responses carry no request id (none of the sibling bridges' do): they come back in the order
//!   the commands were sent, one per `cmd` that had an id; a `cmd` without one gets no response.
//!
//! Only the parent window on the page's own origin is heard or answered. A page whose origin is
//! opaque (`"null"`: a sandboxed iframe, a `data:` page) gets no bridge at all, since every other
//! opaque-origin window would then pass as the parent. A parent on another origin gets no bridge
//! either: its `location` cannot be read, and exports keep downloading. There is no `docClosed`: LightCraft has a
//! library, not documents that close.
//!
//! ## `data-host="page"` — the page's own document
//!
//! A page can also run LightCraft in its own document, no iframe: the canvas opts in with
//! `data-host="page"`, and the same messages travel as DOM `CustomEvent`s on the canvas instead
//! of `postMessage` — outward with the message's fields as the event's `detail`, inward as
//! `lightcraft:open` and `lightcraft:cmd` events the page dispatches on the canvas. Events don't
//! bubble, so two editors on one page can't hear each other, and the canvas is the only target
//! the bridge listens on.
//!
//! The message layer (what a message means, what is sent back) is plain Rust, tested natively;
//! [`Bridge`] (wasm32 only) moves it over `postMessage`, or over DOM events in page mode.

/// A message from the parent page, once understood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    /// `lightcraft:open`: add this photo to the library.
    Open { name: String, bytes: Vec<u8> },
    /// `lightcraft:cmd`: run this command (params as JSON text).
    Cmd { id: String, params: String },
    /// `lightcraft:open` with a file too large to keep: tell the parent why (`lightcraft:failed`).
    Refused(String),
}

/// The fields of a message from the parent, as read off the event (each may be missing).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    pub kind: Option<String>,
    pub name: Option<String>,
    pub bytes: Option<Vec<u8>>,
    pub id: Option<String>,
    pub params: Option<String>,
    /// The size of `bytes` when it was too large to copy (`bytes` is then `None`).
    pub oversize: Option<u64>,
}

/// Why a photo of `len` bytes sent with `lightcraft:open` is refused, or `None` when it fits: one
/// larger than a library backup can hold ([`crate::backup::MAX_BYTES`]) is never copied in.
pub fn open_size_error(len: u64) -> Option<String> {
    let max = crate::backup::MAX_BYTES;
    (len > max).then(|| format!("the file is too large to open ({} MB; the limit is {} MB)", len >> 20, max >> 20))
}

/// The name a photo sent without one is stored under (its extension picks the decoder).
pub const DEFAULT_OPEN_NAME: &str = "photo.jpg";

impl Incoming {
    /// What the parent asked for; `None` for a message type that isn't ours, an `open` without
    /// bytes or a `cmd` without an id (dropped, the page's other messages are none of our business).
    pub fn parse(raw: Raw) -> Option<Incoming> {
        match raw.kind.as_deref() {
            Some("lightcraft:open") => {
                if let Some(error) = raw.oversize.and_then(open_size_error) {
                    return Some(Incoming::Refused(error));
                }
                let name = raw.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| DEFAULT_OPEN_NAME.to_string());
                Some(Incoming::Open { name, bytes: raw.bytes? })
            }
            Some("lightcraft:cmd") => {
                let id = raw.id.filter(|i| !i.trim().is_empty())?;
                Some(Incoming::Cmd { id, params: raw.params.filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "{}".to_string()) })
            }
            _ => None,
        }
    }
}

/// A field of a message to the parent.
#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Str(String),
    Bool(bool),
}

/// A message to the parent, without bytes (`lightcraft:save` carries a transferred buffer and is
/// built by the bridge itself).
#[derive(Clone, Debug, PartialEq)]
pub enum Outgoing {
    Ready,
    Failed(String),
    Dirty(bool),
    /// How a `lightcraft:cmd` ended: the JSON result, or the error text.
    Response {
        ok: bool,
        value: String,
    },
}

impl Outgoing {
    /// The message's fields, `type` first.
    pub fn fields(&self) -> Vec<(&'static str, Field)> {
        match self {
            Outgoing::Ready => vec![("type", Field::Str("lightcraft:ready".into())), ("version", Field::Str(env!("CARGO_PKG_VERSION").into()))],
            Outgoing::Failed(error) => vec![("type", Field::Str("lightcraft:failed".into())), ("error", Field::Str(error.clone()))],
            Outgoing::Dirty(dirty) => vec![("type", Field::Str("lightcraft:dirty".into())), ("dirty", Field::Bool(*dirty))],
            Outgoing::Response { ok, value } => {
                vec![("type", Field::Str("lightcraft:response".into())), ("ok", Field::Bool(*ok)), ("value", Field::Str(value.clone()))]
            }
        }
    }
}

/// The unsaved-changes state as the parent last heard it: `lightcraft:dirty` goes out only when
/// it changes (not every frame), and a message that couldn't be sent is sent again next time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirtyReport {
    reported: Option<bool>,
}

impl DirtyReport {
    /// `Some(message)` when `dirty` differs from what the parent knows; call [`DirtyReport::sent`]
    /// once it went out.
    pub fn change(&self, dirty: bool) -> Option<Outgoing> {
        (self.reported != Some(dirty)).then_some(Outgoing::Dirty(dirty))
    }

    pub fn sent(&mut self, dirty: bool) {
        self.reported = Some(dirty);
    }

    /// Record `dirty` before the send. A page host can report another state while the event is
    /// still dispatching, and that later report wins. Returns the message to send and the state
    /// to put back if this send fails and nothing else recorded one.
    pub fn prepare(&mut self, dirty: bool) -> (Option<Outgoing>, Option<bool>) {
        let previous = self.reported;
        let message = self.change(dirty);
        if message.is_some() {
            self.sent(dirty);
        }
        (message, previous)
    }

    /// Forget a send that failed, unless a nested report already recorded a different state.
    pub fn restore_if_unchanged(&mut self, dirty: bool, previous: Option<bool>) {
        if self.reported == Some(dirty) {
            self.reported = previous;
        }
    }
}

/// Whether a page origin can tell the parent apart from any other window: an opaque origin
/// (`"null"`, as a sandboxed iframe or a `data:` page has) cannot, so such a page gets no bridge.
pub fn origin_is_usable(origin: &str) -> bool {
    let o = origin.trim();
    !o.is_empty() && o != "null"
}

/// The name a saved file is handed over under: the file name of the path the export chose.
pub fn save_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| path.to_string())
}

/// The one `data-host` value that opts a page into page host mode: the exact string `"page"`.
pub const PAGE_HOST: &str = "page";

/// How a host page reaches the app. The modes behave the same; they differ only in how an event
/// goes out ([`Bridge::post`] and `save`, wasm32) and how one comes in ([`Bridge::listen`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `?host=parent`: the page framed LightCraft in an `<iframe>`; `postMessage`.
    Parent,
    /// `data-host="page"`: the page runs LightCraft in its own document; DOM `CustomEvent`s.
    Page,
}

/// The mode the page asked for. `?host=parent` wins when the page asks for both, the same order
/// as the other apps. Page mode is the exact `data-host="page"` ([`PAGE_HOST`]). `None` with
/// neither, so that URL stays the normal app with downloads.
pub fn transport_for(data_host: Option<&str>, query: &str) -> Option<Mode> {
    if host_parent_requested(query) {
        Some(Mode::Parent)
    } else if data_host == Some(PAGE_HOST) {
        Some(Mode::Page)
    } else {
        None
    }
}

/// The message type. A page-mode `CustomEvent` is named `lightcraft:open` or `lightcraft:cmd`
/// and its `detail` is only the fields, so the event name is the type. A parent `postMessage`
/// carries `type` in the object and has no event name.
pub fn incoming_kind(event_name: Option<&str>, detail_type: Option<String>) -> Option<String> {
    event_name.map(str::to_string).or(detail_type)
}

/// Does the query ask for the parent bridge? The same reading of `?host=parent` the URL options
/// always used: the first `host` parameter whose value is exactly `parent`.
pub fn host_parent_requested(query: &str) -> bool {
    query.trim_start_matches('?').split('&').find_map(|p| {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        (k == "host").then_some(v == "parent")
    }) == Some(true)
}

#[cfg(target_arch = "wasm32")]
pub use bridge::Bridge;

#[cfg(target_arch = "wasm32")]
mod bridge {
    use std::cell::RefCell;
    use std::rc::Rc;

    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::JsValue;
    use wasm_bindgen::closure::Closure;

    use super::{DirtyReport, Field, Incoming, Mode, Outgoing, Raw, open_size_error, origin_is_usable, save_name};

    /// How an event goes to the host page, per mode ([`Mode`]).
    #[derive(Clone)]
    enum Transport {
        /// `?host=parent`: `postMessage` between same-origin windows.
        Parent { parent: web_sys::Window, origin: String },
        /// `data-host="page"`: DOM `CustomEvent`s on the editor's own canvas.
        Page { canvas: web_sys::HtmlCanvasElement },
    }

    impl Transport {
        /// Hand an event to the host. Parent mode: a `postMessage` whose `type` field is `event`
        /// (the same message as before, transferring `transfer` when given). Page mode: a
        /// non-bubbling `CustomEvent` of that name on the canvas, whose `detail` holds the
        /// fields; nothing is transferred there, the buffer just rides inside `detail`.
        fn send(&self, event: &str, fields: &[(&str, JsValue)], transfer: Option<&js_sys::ArrayBuffer>) -> Result<(), String> {
            match self {
                Transport::Parent { parent, origin } => {
                    let mut all = Vec::with_capacity(fields.len() + 1);
                    all.push(("type", JsValue::from_str(event)));
                    all.extend_from_slice(fields);
                    let message = message(&all).ok_or_else(|| "couldn't build the message".to_string())?;
                    let sent = match transfer {
                        Some(buffer) => parent.post_message_with_transfer(&message, origin, &js_sys::Array::of1(buffer)),
                        None => parent.post_message(&message, origin),
                    };
                    sent.map_err(|e| js_error(&e, "couldn't post the message"))
                }
                Transport::Page { canvas } => {
                    let detail = message(fields).ok_or_else(|| "couldn't build the event detail".to_string())?;
                    let init = web_sys::CustomEventInit::new();
                    init.set_bubbles(false);
                    init.set_detail(&detail);
                    let custom =
                        web_sys::CustomEvent::new_with_event_init_dict(event, &init).map_err(|e| js_error(&e, "couldn't create the event"))?;
                    canvas.dispatch_event(&custom).map_err(|e| js_error(&e, "couldn't dispatch the event"))?;
                    Ok(())
                }
            }
        }
    }

    /// The host page's end of the bridge (cheap to clone; one per page).
    #[derive(Clone)]
    pub struct Bridge {
        transport: Transport,
        /// What the host sent since the last frame, oldest first.
        incoming: Rc<RefCell<Vec<Incoming>>>,
        dirty: Rc<RefCell<DirtyReport>>,
    }

    impl Bridge {
        /// The parent bridge for `?host=parent`. `None` when the page isn't framed, its origin is
        /// opaque, or the parent is on another origin (exports then keep downloading).
        pub fn new() -> Option<Self> {
            let window = web_sys::window()?;
            let parent = window.parent().ok().flatten()?;
            if js_sys::Object::is(&parent, &window) {
                return None;
            }
            // the document's own origin (`window.origin`), not the URL's: a sandboxed iframe keeps
            // its URL but runs with the opaque origin "null"
            let origin = window.origin();
            if !origin_is_usable(&origin) {
                return None;
            }
            // Reading another origin's location throws, so this is `None` for a cross-origin parent.
            if parent.location().origin().ok().as_deref() != Some(origin.as_str()) {
                return None;
            }
            Some(Self { transport: Transport::Parent { parent, origin }, incoming: Rc::default(), dirty: Rc::default() })
        }

        /// The page bridge for `data-host="page"`: the host page runs LightCraft in its own
        /// document and hears the app's DOM `CustomEvent`s on this canvas.
        pub fn for_page(canvas: &web_sys::HtmlCanvasElement) -> Self {
            Self { transport: Transport::Page { canvas: canvas.clone() }, incoming: Rc::default(), dirty: Rc::default() }
        }

        /// The bridge for the mode the page opted into (`host::transport_for`): page mode needs
        /// its canvas, and parent mode falls back to `None` as [`Bridge::new`] always did.
        pub fn for_mode(mode: Mode, canvas: Option<&web_sys::HtmlCanvasElement>) -> Option<Self> {
            match mode {
                Mode::Page => canvas.map(Self::for_page),
                Mode::Parent => Self::new(),
            }
        }

        /// Hear the host from now on; a message wakes the app for the next frame.
        pub fn listen(&self, ctx: egui::Context) {
            match &self.transport {
                Transport::Parent { parent, origin } => {
                    let parent = parent.clone();
                    let origin = origin.clone();
                    let incoming = self.incoming.clone();
                    let on_message = Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                        if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                            return;
                        }
                        deliver(&incoming, &ctx, &event.data(), None);
                    });
                    if web_sys::window().is_some_and(|w| w.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok()) {
                        // the listener lives as long as the page
                        on_message.forget();
                    }
                }
                Transport::Page { canvas } => {
                    // non-bubbling events: the page dispatches on the canvas, the canvas listens
                    for kind in ["lightcraft:open", "lightcraft:cmd"] {
                        let incoming = self.incoming.clone();
                        let ctx = ctx.clone();
                        let event_type = kind.to_string();
                        // `Fn`, not `FnMut`: a page event is dispatched synchronously, and a host
                        // that answers one event with another would re-enter an `FnMut` listener.
                        let on_custom = Closure::<dyn Fn(web_sys::Event)>::new(move |event: web_sys::Event| {
                            let Some(custom) = event.dyn_ref::<web_sys::CustomEvent>() else { return };
                            deliver(&incoming, &ctx, &custom.detail(), Some(event_type.as_str()));
                        });
                        if canvas.add_event_listener_with_callback(kind, on_custom.as_ref().unchecked_ref()).is_ok() {
                            // the listener lives as long as the page
                            on_custom.forget();
                        }
                    }
                }
            }
        }

        /// What the parent sent since the last frame, oldest first.
        pub fn take_incoming(&self) -> Vec<Incoming> {
            std::mem::take(&mut *self.incoming.borrow_mut())
        }

        /// Post a message; false if it couldn't be sent.
        pub fn post(&self, m: &Outgoing) -> bool {
            let fields = m.fields();
            // every message names its type first (tested natively); the rest go to the transport
            let Some((_, Field::Str(event))) = fields.first() else { return false };
            let event = event.clone();
            let rest: Vec<(&str, JsValue)> = fields.into_iter().skip(1).map(|(k, v)| (k, field(&v))).collect();
            self.transport.send(&event, &rest, None).is_ok()
        }

        /// Tell the parent whether closing now would lose something, when that changed.
        /// The new state is recorded before the send: page mode dispatches synchronously, and a
        /// host that reports another state from that event wins. A failed send is forgotten
        /// only when nothing else recorded one, so the next frame retries it.
        pub fn report_dirty(&self, dirty: bool) {
            let (message, previous) = self.dirty.borrow_mut().prepare(dirty);
            if let Some(m) = message
                && !self.post(&m)
            {
                self.dirty.borrow_mut().restore_if_unchanged(dirty, previous);
            }
        }

        /// Hand a file to the parent instead of downloading it (`lightcraft:save`).
        pub fn save(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
            let name = save_name(path);
            let buffer = js_sys::Uint8Array::from(bytes).buffer();
            self.transfer(&name, buffer)
        }

        /// [`Bridge::save`] for a blob built outside the wasm heap (the library backup).
        pub async fn save_blob(&self, name: String, blob: web_sys::Blob) -> Result<(), String> {
            let buffer = wasm_bindgen_futures::JsFuture::from(blob.array_buffer()).await.map_err(|e| format!("{e:?}"))?;
            self.transfer(&name, buffer.unchecked_into::<js_sys::ArrayBuffer>())
        }

        fn transfer(&self, name: &str, buffer: js_sys::ArrayBuffer) -> Result<(), String> {
            self.transport.send("lightcraft:save", &[("name", name.into()), ("bytes", buffer.clone().into())], Some(&buffer))?;
            log::info!("handed {name} to the page");
            Ok(())
        }
    }

    /// A message from the host (the `data` of a window message, or the `detail` of a page-mode
    /// `CustomEvent`), read with the same field code both transports share, and pushed for the
    /// next frame.
    fn deliver(incoming: &Rc<RefCell<Vec<Incoming>>>, ctx: &egui::Context, data: &JsValue, event_type: Option<&str>) {
        // moved, not cloned: an accepted photo can be hundreds of MB
        let (bytes, oversize) = match bytes(data) {
            Some(Ok(b)) => (Some(b), None),
            Some(Err(len)) => (None, Some(len)),
            None => (None, None),
        };
        let raw = Raw {
            kind: super::incoming_kind(event_type, string(data, "type")),
            name: string(data, "name"),
            bytes,
            id: string(data, "id"),
            params: string(data, "params"),
            oversize,
        };
        if let Some(m) = Incoming::parse(raw) {
            incoming.borrow_mut().push(m);
            ctx.request_repaint();
        }
    }

    fn field(v: &Field) -> JsValue {
        match v {
            Field::Str(s) => s.as_str().into(),
            Field::Bool(b) => (*b).into(),
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

    /// `bytes` as an `ArrayBuffer` or `Uint8Array`.
    /// `bytes` as an `ArrayBuffer` or `Uint8Array`, copied only when within the size cap
    /// ([`open_size_error`]); `Err(len)` for an oversize one, never copied into the app's memory.
    fn bytes(data: &JsValue) -> Option<Result<Vec<u8>, u64>> {
        let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
        let array = if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
            array.clone()
        } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
            js_sys::Uint8Array::new(&bytes)
        } else {
            return None;
        };
        let len = u64::from(array.byte_length());
        Some(if open_size_error(len).is_some() { Err(len) } else { Ok(array.to_vec()) })
    }

    /// A `JsValue` error as a string, after `what` went wrong.
    fn js_error(e: &JsValue, what: &str) -> String {
        format!("{what}: {}", e.as_string().unwrap_or_else(|| format!("{e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: &str) -> Raw {
        Raw { kind: Some(kind.into()), ..Default::default() }
    }

    #[test]
    fn open_needs_bytes_and_names_a_nameless_photo() {
        let mut m = raw("lightcraft:open");
        assert_eq!(Incoming::parse(m.clone()), None, "no bytes, nothing to add");
        m.bytes = Some(vec![1, 2, 3]);
        assert_eq!(Incoming::parse(m.clone()), Some(Incoming::Open { name: DEFAULT_OPEN_NAME.into(), bytes: vec![1, 2, 3] }));
        m.name = Some("  ".into());
        assert_eq!(Incoming::parse(m.clone()), Some(Incoming::Open { name: DEFAULT_OPEN_NAME.into(), bytes: vec![1, 2, 3] }));
        m.name = Some("IMG_0001.CR2".into());
        assert_eq!(Incoming::parse(m), Some(Incoming::Open { name: "IMG_0001.CR2".into(), bytes: vec![1, 2, 3] }));
    }

    #[test]
    fn cmd_needs_an_id_and_defaults_its_params() {
        let mut m = raw("lightcraft:cmd");
        assert_eq!(Incoming::parse(m.clone()), None);
        m.id = Some("library.info".into());
        assert_eq!(Incoming::parse(m.clone()), Some(Incoming::Cmd { id: "library.info".into(), params: "{}".into() }));
        m.params = Some(r#"{"rating": 5}"#.into());
        assert_eq!(Incoming::parse(m), Some(Incoming::Cmd { id: "library.info".into(), params: r#"{"rating": 5}"#.into() }));
    }

    #[test]
    fn an_oversize_open_is_refused_not_copied() {
        let max = crate::backup::MAX_BYTES;
        assert!(open_size_error(max).is_none());
        assert!(open_size_error(max + 1).is_some_and(|e| e.contains("too large")));
        let m = Raw { name: Some("big.tif".into()), oversize: Some(max + 1), ..raw("lightcraft:open") };
        assert!(matches!(Incoming::parse(m), Some(Incoming::Refused(e)) if e.contains("too large")));
    }

    #[test]
    fn other_pages_messages_are_not_ours() {
        for kind in ["deckcraft:open", "lightcraft:save", "lightcraft:ready", ""] {
            let m = Raw { bytes: Some(vec![1]), id: Some("x".into()), ..raw(kind) };
            assert_eq!(Incoming::parse(m), None, "{kind:?}");
        }
        assert_eq!(Incoming::parse(Raw::default()), None);
    }

    #[test]
    fn outgoing_messages_name_their_type_first() {
        let ready = Outgoing::Ready.fields();
        assert_eq!(ready[0], ("type", Field::Str("lightcraft:ready".into())));
        assert_eq!(ready[1].0, "version");
        assert_eq!(Outgoing::Failed("boom".into()).fields()[1], ("error", Field::Str("boom".into())));
        assert_eq!(Outgoing::Dirty(true).fields(), vec![("type", Field::Str("lightcraft:dirty".into())), ("dirty", Field::Bool(true))]);
        let r = Outgoing::Response { ok: false, value: "no such command".into() }.fields();
        assert_eq!(r[1], ("ok", Field::Bool(false)));
        assert_eq!(r[2], ("value", Field::Str("no such command".into())));
    }

    /// The parent hears `dirty` when it changes, not every frame, and again if a send failed.
    #[test]
    fn dirty_is_reported_on_change_only() {
        let mut d = DirtyReport::default();
        assert_eq!(d.change(false), Some(Outgoing::Dirty(false)), "the first state is news");
        d.sent(false);
        assert_eq!(d.change(false), None);
        assert_eq!(d.change(true), Some(Outgoing::Dirty(true)));
        // not marked sent (the post failed): still owed
        assert_eq!(d.change(true), Some(Outgoing::Dirty(true)));
        d.sent(true);
        assert_eq!(d.change(true), None);
        assert_eq!(d.change(false), Some(Outgoing::Dirty(false)));
    }

    /// A failed send is retried. A report that lands during the send (page mode dispatches
    /// synchronously) is the one that stays.
    #[test]
    fn a_failed_dirty_send_is_retried_unless_a_later_report_landed() {
        let mut d = DirtyReport::default();
        let (message, previous) = d.prepare(true);
        assert_eq!(message, Some(Outgoing::Dirty(true)));
        d.restore_if_unchanged(true, previous);
        assert_eq!(d.change(true), Some(Outgoing::Dirty(true)));

        let (message, previous) = d.prepare(true);
        assert!(message.is_some());
        let (later, _) = d.prepare(false);
        assert_eq!(later, Some(Outgoing::Dirty(false)));
        d.restore_if_unchanged(true, previous);
        assert_eq!(d.change(false), None);
    }

    /// A canvas event is named for its type; the detail does not have to repeat it.
    #[test]
    fn a_page_event_is_named_for_its_type() {
        assert_eq!(incoming_kind(Some("lightcraft:open"), None).as_deref(), Some("lightcraft:open"));
        assert_eq!(incoming_kind(Some("lightcraft:cmd"), Some("lightcraft:open".into())).as_deref(), Some("lightcraft:cmd"));
        assert_eq!(incoming_kind(None, Some("lightcraft:open".into())).as_deref(), Some("lightcraft:open"));
        assert_eq!(incoming_kind(None, None), None);
    }

    /// A sandboxed iframe or a `data:` page has the opaque origin `"null"`, which any other such
    /// window shares: no bridge there, so the origin check keeps meaning something.
    #[test]
    fn an_opaque_origin_gets_no_bridge() {
        assert!(origin_is_usable("https://photos.example"));
        assert!(origin_is_usable("http://localhost:8080"));
        assert!(!origin_is_usable("null"));
        assert!(!origin_is_usable(" null "));
        assert!(!origin_is_usable(""));
    }

    #[test]
    fn saved_files_are_handed_over_by_file_name() {
        assert_eq!(save_name("exports/2026/IMG_0001.jpg"), "IMG_0001.jpg");
        assert_eq!(save_name("IMG_0001.jpg"), "IMG_0001.jpg");
        assert_eq!(save_name("lightcraft-library-backup-2026-10-19.zip"), "lightcraft-library-backup-2026-10-19.zip");
    }

    /// Page mode is opt-in by the exact attribute value, parent mode by the query, and parent
    /// mode wins when a page asks for both.
    #[test]
    fn page_mode_is_opt_in_and_parent_mode_wins_when_both_are_set() {
        assert_eq!(transport_for(Some("page"), ""), Some(Mode::Page));
        assert_eq!(transport_for(Some("page"), "?host=parent"), Some(Mode::Parent), "parent mode wins");
        // only the exact value opts in; anything else falls back to the query, or to standalone
        for not_page in ["Page", " page", "page ", "parent", "host"] {
            assert_eq!(transport_for(Some(not_page), "?host=parent"), Some(Mode::Parent), "{not_page:?}");
            assert_eq!(transport_for(Some(not_page), ""), None, "{not_page:?}");
        }
        assert_eq!(transport_for(None, ""), None);
        assert_eq!(transport_for(None, "?store=idb"), None);
        assert_eq!(transport_for(None, "?host="), None);
        assert_eq!(transport_for(None, "?host=Parent"), None);
        assert_eq!(transport_for(None, "?xhost=parent"), None);
    }

    /// The query is read the way the URL options always read it: the first `host` parameter wins,
    /// and only a value of exactly `parent` counts.
    #[test]
    fn the_query_asks_for_the_parent_bridge() {
        assert!(host_parent_requested("?host=parent"));
        assert!(host_parent_requested("host=parent"));
        assert!(host_parent_requested("?host=parent&store=idb"));
        assert!(host_parent_requested("?store=idb&host=parent"));
        assert!(!host_parent_requested(""));
        assert!(!host_parent_requested("?"));
        assert!(!host_parent_requested("?host"));
        assert!(!host_parent_requested("?host="));
        assert!(host_parent_requested("?host=parent&host=id"), "the first host parameter wins");
        assert!(!host_parent_requested("?host=id&host=parent"));
        assert!(!host_parent_requested("?hosts=parent"));
    }
}
