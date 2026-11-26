//! `window.lightcraft`'s control functions: `request`, `execute`, `commands` and `uiInspect` from
//! `craft-web-api` (storytold/craft-libs#413), the same in every Craft web build, over the app's
//! control channel (`docs/control-protocol.md`, the methods the desktop control server and MCP use).
//!
//! ```js
//! await lightcraft.request("ui.inspect", {})              // any control method → its result
//! await lightcraft.execute("library.info", {})           // engine command → its result
//! await lightcraft.commands()                             // engine and UI commands
//! await lightcraft.uiInspect()                            // ui.inspect
//! ```
//!
//! They are added to the `window.lightcraft` the page set up (`index.html`: `{command}`). Until
//! the app runs, calls reject with "LightCraft is not running yet".

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use craft_web_api::methods::Methods;
use craft_web_api::page::{PageApi, Request};
use lightcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

thread_local! {
    /// The running app's control channel, and its context to wake it for a request.
    static CONTROL: RefCell<Option<(Sender<ControlRequest>, egui::Context)>> = const { RefCell::new(None) };
}

/// How often a waiting call looks for the app's reply, in milliseconds (about a frame).
const POLL_MS: i32 = 16;

/// Add the control functions to `window.lightcraft`. LightCraft has no `document.inspect`, and
/// its `ui.screenshot` with no `path` answers only the size (no inline PNG), so `docInspect` and
/// `screenshot` are left out; `request("ui.screenshot", …)` still reaches it.
pub fn install() -> Result<(), JsValue> {
    let methods = Methods { doc_inspect: None, screenshot: None, ..Methods::default() };
    let request: Request = Rc::new(|method, params| send(&method, params));
    Rc::new(PageApi::new("lightcraft", methods, request)).install()
}

/// Hand the running app's control channel to the page's functions.
pub fn connect(tx: Sender<ControlRequest>, ctx: egui::Context) {
    CONTROL.with(|c| *c.borrow_mut() = Some((tx, ctx)));
}

/// A queued control request whose reply has not arrived. Dropping it before the reply (the page's
/// timeout fired) abandons the request if the app has not started it, so the app never runs it
/// late (issue #718).
struct Waiting {
    reply: Receiver<Value>,
    state: Arc<AtomicU8>,
    answered: bool,
}

impl Waiting {
    /// The app's reply once it has answered or gone away; `None` while the request is waiting.
    fn poll(&mut self) -> Option<Value> {
        match self.reply.try_recv() {
            Ok(reply) => {
                self.answered = true;
                Some(reply)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.answered = true;
                Some(json!({"ok": false, "error": "request dropped"}))
            }
        }
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if !self.answered {
            ControlRequest::abandon(&self.state);
        }
    }
}

/// Queue a control request for the app and wait for its reply, as the protocol's `{ok, result}` /
/// `{ok: false, error}` object.
fn send(method: &str, params: Value) -> Pin<Box<dyn Future<Output = Value>>> {
    let (req, rx) = ControlRequest::new(method, params);
    let state = req.state.clone();
    let sent = CONTROL.with(|c| {
        c.borrow().as_ref().is_some_and(|(tx, ctx)| {
            let sent = tx.send(req).is_ok();
            ctx.request_repaint();
            sent
        })
    });
    if !sent {
        return Box::pin(async { json!({"ok": false, "error": "LightCraft is not running yet"}) });
    }
    let mut waiting = Waiting { reply: rx, state, answered: false };
    Box::pin(async move {
        loop {
            if let Some(reply) = waiting.poll() {
                return reply;
            }
            sleep(POLL_MS).await;
        }
    })
}

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}
