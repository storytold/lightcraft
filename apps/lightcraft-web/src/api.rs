//! `window.lightcraft`: the page command API (`craft-web-api`, storytold/craft-libs#413) over the
//! app's own control channel, the one the desktop control server drives (`docs/control-protocol.md`).
//!
//! The crate owns the JavaScript side (`request`, `execute`, `commands`, `uiInspect`, `screenshot`, as
//! promises with a timeout; LightCraft has no `document.inspect`, so no `docInspect`). This module is LightCraft's part: a [`Request`] that
//! queues a [`ControlRequest`] for the app, which answers it on its next frame, and waits for the
//! reply. A call the page gave up on is released ([`Pending`]), so the app drops it unrun instead of
//! applying a command the page was told had failed.

use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::mpsc::{Receiver, TryRecvError};

use lightcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};

/// How often a waiting call looks for the app's reply, in milliseconds (about a frame).
#[cfg(target_arch = "wasm32")]
const POLL_MS: i32 = 16;

/// A queued control request whose reply has not arrived. Dropping it before the reply (the page's
/// timeout fired, or the call was abandoned) releases the request: the app skips it unrun.
struct Pending {
    reply: Receiver<Value>,
    state: Arc<AtomicU8>,
    answered: bool,
}

impl Pending {
    /// `Some` once the app has answered or gone away; `None` while the request is still waiting.
    fn poll(&mut self) -> Option<Value> {
        match self.reply.try_recv() {
            Ok(reply) => {
                self.answered = true;
                Some(reply)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.answered = true;
                Some(json!({"ok": false, "error": "the app dropped the request"}))
            }
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.answered {
            ControlRequest::abandon(&self.state);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::install;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::rc::Rc;
    use std::sync::mpsc::{Receiver, TrySendError, sync_channel};

    use craft_web_api::methods::Methods;
    use craft_web_api::page::{PageApi, Request};
    use lightcraft_ui_egui::ControlRequest;
    use serde_json::json;
    use wasm_bindgen_futures::JsFuture;

    use super::{POLL_MS, Pending};

    /// The app id in `window.<app>`.
    const APP: &str = "lightcraft";

    /// How many control requests may wait for the app at once. `craft-web-api` already refuses calls past its own
    /// in-flight limit; this bounds the app's queue too, so a flood of calls can't grow it without limit.
    const QUEUE: usize = 64;

    /// Put the crate's functions on `window.lightcraft` and return the queue the app drains each
    /// frame (hand it to `LightcraftApp::with_control`). An existing `window.lightcraft` keeps the
    /// functions it already has (`command`, for one).
    pub fn install(ctx: egui::Context) -> Receiver<ControlRequest> {
        let (queue, requests) = sync_channel::<ControlRequest>(QUEUE);
        let request: Request = Rc::new(move |method, params| {
            let (control, reply) = ControlRequest::new(method, params);
            let mut pending = Pending { reply, state: control.state.clone(), answered: false };
            if let Err(refused) = queue.try_send(control) {
                pending.answered = true;
                let error = match refused {
                    TrySendError::Full(_) => format!("lightcraft is busy: {QUEUE} requests are already waiting for the app"),
                    TrySendError::Disconnected(_) => "the app is not running".to_string(),
                };
                return Box::pin(async move { json!({"ok": false, "error": error}) });
            }
            ctx.request_repaint();
            Box::pin(async move {
                loop {
                    if let Some(reply) = pending.poll() {
                        return reply;
                    }
                    if !sleep(POLL_MS).await {
                        return json!({"ok": false, "error": "this page has no timer to wait for the app with"});
                    }
                }
            })
        });
        let methods = Methods { doc_inspect: None, ..Methods::default() };
        if let Err(e) = Rc::new(PageApi::new(APP, methods, request)).install() {
            log::warn!("couldn't install window.{APP}: {e:?}");
        }
        requests
    }

    async fn sleep(ms: i32) -> bool {
        // `false` when this page has no timer to wait with (no window, or the timer is refused): the caller ends the
        // call with an error instead of going round again at once.
        let timer = std::cell::Cell::new(false);
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            let set = web_sys::window().is_some_and(|w| w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms).is_ok());
            timer.set(set);
            if !set {
                let _ = resolve.call0(&wasm_bindgen::JsValue::UNDEFINED);
            }
        });
        let _ = JsFuture::from(promise).await;
        timer.get()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;

    /// `ControlRequest`'s `state` values (`lightcraft_ui_egui::control`): the app has started it, or its
    /// sender gave up.
    const TAKEN: u8 = 1;
    const ABANDONED: u8 = 2;

    fn pending() -> (Pending, ControlRequest) {
        let (control, reply) = ControlRequest::new("document.inspect", json!({}));
        (Pending { reply, state: control.state.clone(), answered: false }, control)
    }

    #[test]
    fn a_call_the_page_gave_up_on_is_released_so_the_app_skips_it() {
        let (waiting, control) = pending();
        drop(waiting);
        assert_eq!(control.state.load(Ordering::SeqCst), ABANDONED);
        assert!(!control.take(), "the app must not run a request the page was told had failed");
    }

    #[test]
    fn an_answered_call_is_not_released() {
        let (mut waiting, control) = pending();
        assert!(control.take());
        control.reply.send(json!({"ok": true, "result": 1})).unwrap_or_default();
        assert_eq!(waiting.poll(), Some(json!({"ok": true, "result": 1})));
        drop(waiting);
        assert_eq!(control.state.load(Ordering::SeqCst), TAKEN);
    }

    #[test]
    fn a_request_the_app_never_runs_waits_until_it_is_dropped() {
        let (mut waiting, control) = pending();
        assert_eq!(waiting.poll(), None);
        drop(control);
        assert_eq!(waiting.poll().and_then(|r| r["ok"].as_bool()), Some(false), "the app went away");
    }
}
