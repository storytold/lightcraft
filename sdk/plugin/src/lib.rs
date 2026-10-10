//! Guest-side SDK for the app's WebAssembly plug-ins (ABI v1, see `docs/plugins.md`).
//!
//! A plug-in is a `cdylib` built for `wasm32-unknown-unknown`:
//!
//! ```ignore
//! use dac_plugin_sdk::{export_plugin, host, Request};
//! use serde_json::{json, Value};
//!
//! const MANIFEST: &str = r#"{"id": "org.example.hello", "name": "Hello",
//!   "permissions": {"catalog": true},
//!   "commands": [{"id": "count", "label": "Count Photos"}]}"#;
//!
//! fn handle(req: &Request) -> Result<Value, String> {
//!     match req.hook() {
//!         "command" => Ok(json!({"message": format!("{}", host::call("catalog.stats", json!({}))?)})),
//!         other => Err(format!("unsupported hook {other}")),
//!     }
//! }
//! export_plugin!(MANIFEST, handle);
//! ```
//!
//! The SDK itself has no `unsafe` blocks: buffers the host fills are ordinary `Vec`s kept in a
//! table by address; the host imports are declared `safe`. The only `unsafe` token is the
//! `#[unsafe(no_mangle)]` the [`export_plugin!`] macro puts on the four ABI exports.

use std::cell::RefCell;
use std::collections::BTreeMap;

pub use serde_json;
use serde_json::{Value, json};

/// The ABI version this SDK implements.
pub const ABI_VERSION: i32 = 1;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "host")]
unsafe extern "C" {
    #[link_name = "call"]
    safe fn host_call(ptr: i32, len: i32) -> i32;
    #[link_name = "response"]
    safe fn host_response(ptr: i32);
    #[link_name = "log"]
    safe fn host_log(ptr: i32, len: i32);
}

// Off wasm (unit tests of a plug-in's logic): no host.
#[cfg(not(target_arch = "wasm32"))]
fn host_call(_: i32, _: i32) -> i32 {
    -1
}
#[cfg(not(target_arch = "wasm32"))]
fn host_response(_: i32) {}
#[cfg(not(target_arch = "wasm32"))]
fn host_log(_: i32, _: i32) {}

thread_local! {
    /// Buffers handed to the host by `dac_alloc`, by address.
    static BUFFERS: RefCell<BTreeMap<usize, Vec<u8>>> = const { RefCell::new(BTreeMap::new()) };
    /// The last reply, kept alive until the next call.
    static REPLY: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static MANIFEST: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn addr(v: &[u8]) -> i32 {
    v.as_ptr() as usize as i32
}

fn pack(v: &[u8]) -> i64 {
    ((v.len() as i64) << 32) | (addr(v) as u32 as i64)
}

#[doc(hidden)]
pub fn __alloc(len: i32) -> i32 {
    let v = vec![0u8; len.max(1) as usize];
    let p = addr(&v);
    BUFFERS.with(|b| b.borrow_mut().insert(p as u32 as usize, v));
    p
}

#[doc(hidden)]
pub fn __manifest(text: &str) -> i64 {
    MANIFEST.with(|m| {
        let mut m = m.borrow_mut();
        *m = text.as_bytes().to_vec();
        pack(&m)
    })
}

#[doc(hidden)]
pub fn __handle(ptr: i32, len: i32, handler: fn(&Request) -> Result<Value, String>) -> i64 {
    let bytes = BUFFERS.with(|b| b.borrow_mut().remove(&(ptr as u32 as usize))).unwrap_or_default();
    let bytes = bytes.get(..len.max(0) as usize).unwrap_or(&bytes);
    let reply = match serde_json::from_slice::<Value>(bytes) {
        Ok(v) => match handler(&Request(v)) {
            Ok(v) => json!({"ok": v}),
            Err(e) => json!({"error": e}),
        },
        Err(e) => json!({"error": format!("bad request: {e}")}),
    };
    REPLY.with(|r| {
        let mut r = r.borrow_mut();
        *r = serde_json::to_vec(&reply).unwrap_or_else(|_| br#"{"error":"reply not serializable"}"#.to_vec());
        pack(&r)
    })
}

/// A request from the host.
#[derive(Clone, Debug)]
pub struct Request(pub Value);

impl Request {
    /// `command`, `export`, `metadata`, `publish.upload`, `publish.delete`.
    pub fn hook(&self) -> &str {
        self.0.get("hook").and_then(Value::as_str).unwrap_or("")
    }
    /// The command id (`hook == "command"`).
    pub fn command(&self) -> &str {
        self.0.get("command").and_then(Value::as_str).unwrap_or("")
    }
    /// A dialog value or argument.
    pub fn arg(&self, key: &str) -> &Value {
        self.0.get("args").and_then(|a| a.get(key)).unwrap_or(&Value::Null)
    }
    /// Selected photo ids (`hook == "command"`).
    pub fn selection(&self) -> Vec<String> {
        self.0
            .get("selection")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    }
    pub fn get(&self, key: &str) -> &Value {
        self.0.get(key).unwrap_or(&Value::Null)
    }
}

/// Calls into the host.
pub mod host {
    use super::*;

    /// Sends `{method, params}`; the host's reply or its error message.
    pub fn call(method: &str, params: Value) -> Result<Value, String> {
        let req = serde_json::to_vec(&json!({"method": method, "params": params})).map_err(|e| e.to_string())?;
        let n = host_call(addr(&req), req.len() as i32);
        if n < 0 {
            return Err("no host".into());
        }
        let mut buf = vec![0u8; n as usize];
        host_response(buf.as_mut_ptr() as usize as i32);
        // the host wrote the reply into `buf`; read it through a black box so it isn't assumed zero
        let buf = std::hint::black_box(buf);
        let mut reply: Value = serde_json::from_slice(&buf).map_err(|e| format!("bad host reply: {e}"))?;
        if let Some(e) = reply.get("error") {
            return Err(e.as_str().map_or_else(|| e.to_string(), str::to_string));
        }
        Ok(reply.get_mut("ok").map(Value::take).unwrap_or(Value::Null))
    }

    /// A line in the plug-in's log (Plugin Manager → Log).
    pub fn log(line: &str) {
        host_log(addr(line.as_bytes()), line.len() as i32);
    }

    /// `catalog.query` (needs the `catalog` permission).
    pub fn query(params: Value) -> Result<Value, String> {
        call("catalog.query", params)
    }

    /// This plug-in's own metadata for `photo`.
    pub fn ns_get(photo: &str) -> Result<Value, String> {
        call("ns.get", json!({"photo": photo}))
    }

    /// Sets (or with `Value::Null`, removes) a key in this plug-in's own metadata for `photo`.
    pub fn ns_set(photo: &str, key: &str, value: Value) -> Result<(), String> {
        call("ns.set", json!({"photo": photo, "key": key, "value": value})).map(|_| ())
    }

    pub fn read_text(path: &str) -> Result<String, String> {
        call("fs.readText", json!({"path": path})).map(|v| v.as_str().unwrap_or_default().to_string())
    }

    pub fn write_text(path: &str, text: &str) -> Result<(), String> {
        call("fs.writeText", json!({"path": path, "text": text})).map(|_| ())
    }

    /// An HTTP request (needs the URL's host in the `network` permission) →
    /// `{status, contentType, location, body}`.
    pub fn http(method: &str, url: &str, headers: Value, body: Option<&str>) -> Result<Value, String> {
        call("http.request", json!({"method": method, "url": url, "headers": headers, "body": body}))
    }
}

/// Exports the ABI: `export_plugin!(MANIFEST_JSON, handler)` with
/// `fn handler(&Request) -> Result<Value, String>`.
#[macro_export]
macro_rules! export_plugin {
    ($manifest:expr, $handler:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn dac_abi_version() -> i32 {
            $crate::ABI_VERSION
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn dac_manifest() -> i64 {
            $crate::__manifest($manifest)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn dac_alloc(len: i32) -> i32 {
            $crate::__alloc(len)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn dac_handle(ptr: i32, len: i32) -> i64 {
            $crate::__handle(ptr, len, $handler)
        }
    };
}
