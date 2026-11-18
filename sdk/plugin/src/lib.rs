//! Guest-side SDK for the app's WebAssembly plug-ins (ABI v1, see `docs/plugins.md`).
//!
//! A plug-in is a `cdylib` built for `wasm32-unknown-unknown`:
//!
//! ```ignore
//! use lcc_plugin_sdk::{export_plugin, host, Request};
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
//! `export_plugin!(package: MANIFEST, handle)` takes the manifest's `version`, `description`,
//! `author`, `homepage`, `repository` and `license` from the crate's `Cargo.toml` where the JSON
//! doesn't set them, so they can't drift apart. Other manifest fields go in the JSON as they are,
//! such as `"updateUrl"` (the plug-in's update feed; `"requires": ["updates"]`): see the host's
//! `docs/plugins.md`.
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
    /// Buffers handed to the host by `lcc_alloc`, by address.
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
    /// `command`, `command.prepare`, `settings.prepare`, `export`, `export.destination`, `metadata`,
    /// `publish.upload`, `publish.delete`, `program.end`.
    pub fn hook(&self) -> &str {
        self.0.get("hook").and_then(Value::as_str).unwrap_or("")
    }
    /// The command id (`hook == "command"` or `"command.prepare"`).
    pub fn command(&self) -> &str {
        self.0.get("command").and_then(Value::as_str).unwrap_or("")
    }
    /// A dialog value or argument.
    pub fn arg(&self, key: &str) -> &Value {
        self.0.get("args").and_then(|a| a.get(key)).unwrap_or(&Value::Null)
    }
    /// Selected photo ids (`hook == "command"` or `"command.prepare"`).
    pub fn selection(&self) -> Vec<String> {
        self.0
            .get("selection")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    }
    /// The Library source the command was started from (`hook == "command"` or
    /// `"command.prepare"`): `{kind: "album" | "smartAlbum", id, name, quick?}`, `{kind: "folder",
    /// path}`, `{kind: "allPhotos"}` or `{kind: "other", name}`; null from an older app.
    pub fn source(&self) -> &Value {
        self.get("source")
    }
    /// The album the command was started from, when it can hold photos (`source` is an `album`):
    /// its id, for `host::library_add`.
    pub fn source_album(&self) -> Option<&str> {
        let s = self.source();
        (s.get("kind").and_then(Value::as_str) == Some("album")).then(|| s.get("id").and_then(Value::as_str)).flatten()
    }
    pub fn get(&self, key: &str) -> &Value {
        self.0.get(key).unwrap_or(&Value::Null)
    }
    /// A job's state from its last reply (`hook == "command"` of a job, see [`job::continue_with`]):
    /// null on its first call.
    pub fn state(&self) -> &Value {
        self.get("state")
    }
    /// The run a `program.start` returned (`hook == "program.end"`).
    pub fn run(&self) -> &str {
        self.0.get("run").and_then(Value::as_str).unwrap_or("")
    }
    /// The program's id in the manifest (`hook == "program.end"`).
    pub fn program(&self) -> &str {
        self.0.get("program").and_then(Value::as_str).unwrap_or("")
    }
    /// How the run ended: `exited`, `cancelled` (the user stopped waiting; the program may still
    /// run) or `failed` (`hook == "program.end"`).
    pub fn status(&self) -> &str {
        self.0.get("status").and_then(Value::as_str).unwrap_or("")
    }
    /// The program's exit code, when the app knows it (`hook == "program.end"`, status `exited`):
    /// `None` on macOS, where the app starts programs through `open`, and for a program ended by a
    /// signal.
    pub fn code(&self) -> Option<i64> {
        self.0.get("code").and_then(Value::as_i64)
    }
    /// The run's folder, lent for reading (`hook == "program.end"`).
    pub fn tmp_dir(&self) -> &str {
        self.0.get("tmpDir").and_then(Value::as_str).unwrap_or("")
    }
    /// The export destination id (`hook == "export.destination"`).
    pub fn destination(&self) -> &str {
        self.0.get("destination").and_then(Value::as_str).unwrap_or("")
    }
    /// Exported files `(path, photo id)` (`hook == "export"` or `"export.destination"`).
    pub fn files(&self) -> Vec<(String, String)> {
        self.0
            .get("files")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        let path = f.get("path")?.as_str()?.to_string();
                        let photo = match f.get("photo") {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Number(n)) => n.to_string(),
                            _ => String::new(),
                        };
                        Some((path, photo))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A photo's pixels from [`host::photo_pixels`]: `width × height`, 8-bit sRGB, interleaved RGB
/// rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// Standard base64 (with padding), as `fs.readBytes` / `fs.writeBytes` / `bodyBase64` use.
pub mod base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const INVALID: u8 = 0xff;
    /// Each byte's value in [`ALPHABET`] ([`INVALID`] for others): a table, not a search, as
    /// plug-ins decode megabytes (`photo.pixels`, `fs.readBytes`) in an interpreter.
    const DECODE: [u8; 256] = {
        let mut t = [INVALID; 256];
        let mut i = 0;
        while i < 64 {
            t[ALPHABET[i] as usize] = i as u8;
            i += 1;
        }
        t
    };

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                out.push(if i <= chunk.len() { char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]) } else { '=' });
            }
        }
        out
    }

    pub fn decode(text: &str) -> Result<Vec<u8>, String> {
        decode_bytes(text.as_bytes())
    }

    /// [`decode`] of base64 bytes, a whole group of four at a time.
    pub fn decode_bytes(s: &[u8]) -> Result<Vec<u8>, String> {
        if !s.len().is_multiple_of(4) {
            return Err("base64 length must be a multiple of 4".into());
        }
        let Some((body, last)) = s.len().checked_sub(4).map(|n| s.split_at(n)) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        for c in body.as_chunks::<4>().0 {
            let (a, b, x, y) = (DECODE[usize::from(c[0])], DECODE[usize::from(c[1])], DECODE[usize::from(c[2])], DECODE[usize::from(c[3])]);
            // every valid value fits in 6 bits; INVALID (also `=` before the end) doesn't
            if (a | b | x | y) & 0xc0 != 0 {
                return Err(if c.contains(&b'=') { "bad base64 padding" } else { "bad base64 character" }.into());
            }
            let n = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(x) << 6) | u32::from(y);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
            out.push(n as u8);
        }
        let pad = last.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 {
            return Err("bad base64 padding".into());
        }
        let mut n = 0u32;
        for &b in &last[..4 - pad] {
            let v = DECODE[usize::from(b)];
            if v == INVALID {
                return Err("bad base64 character".into());
            }
            n = (n << 6) | u32::from(v);
        }
        n <<= 6 * pad as u32;
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8][..3 - pad]);
        Ok(out)
    }
}

/// Calls into the host.
pub mod host {
    use super::*;

    /// Sends `{method, params}`; the host's reply or its error message.
    pub fn call(method: &str, params: Value) -> Result<Value, String> {
        parse_reply(&call_raw(method, params)?)
    }

    /// Sends `{method, params}`; the host's reply as it came, unparsed.
    fn call_raw(method: &str, params: Value) -> Result<Vec<u8>, String> {
        let req = serde_json::to_vec(&json!({"method": method, "params": params})).map_err(|e| e.to_string())?;
        let n = host_call(addr(&req), req.len() as i32);
        if n < 0 {
            return Err("no host".into());
        }
        let mut buf = vec![0u8; n as usize];
        host_response(buf.as_mut_ptr() as usize as i32);
        // the host wrote the reply into `buf`; read it through a black box so it isn't assumed zero
        Ok(std::hint::black_box(buf))
    }

    /// A host reply's value, or its error message.
    fn parse_reply(buf: &[u8]) -> Result<Value, String> {
        let mut reply: Value = serde_json::from_slice(buf).map_err(|e| format!("bad host reply: {e}"))?;
        if let Some(e) = reply.get("error") {
            return Err(e.as_str().map_or_else(|| e.to_string(), str::to_string));
        }
        Ok(reply.get_mut("ok").map(Value::take).unwrap_or(Value::Null))
    }

    /// A line in the plug-in's log (Plug-in Manager → Log).
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

    /// This plug-in's settings (`settings` in the manifest): every key, defaults for those never set.
    pub fn settings() -> Result<Value, String> {
        call("settings.get", json!({}))
    }

    /// One setting (its default when never set, `Value::Null` when there is no host or no such key).
    pub fn setting(key: &str) -> Value {
        settings().ok().and_then(|mut s| s.get_mut(key).map(Value::take)).unwrap_or(Value::Null)
    }

    /// Changes a setting (`Value::Null`: back to its default); the host checks it against its
    /// field and returns the value kept (a number clamped to its range).
    pub fn set_setting(key: &str, value: Value) -> Result<Value, String> {
        call("settings.set", json!({"key": key, "value": value}))
    }

    pub fn read_text(path: &str) -> Result<String, String> {
        call("fs.readText", json!({"path": path})).map(|v| v.as_str().unwrap_or_default().to_string())
    }

    pub fn write_text(path: &str, text: &str) -> Result<(), String> {
        call("fs.writeText", json!({"path": path, "text": text})).map(|_| ())
    }

    /// A binary file (needs a readable root, or a file lent to this call such as an exported file).
    pub fn read_bytes(path: &str) -> Result<Vec<u8>, String> {
        call("fs.readBytes", json!({"path": path})).and_then(|v| crate::base64::decode(v.as_str().unwrap_or_default()))
    }

    /// Writes a binary file (needs a writable root, or a file lent to this call).
    pub fn write_bytes(path: &str, bytes: &[u8]) -> Result<(), String> {
        call("fs.writeBytes", json!({"path": path, "base64": crate::base64::encode(bytes)})).map(|_| ())
    }

    /// Removes a file below a writable root.
    pub fn remove(path: &str) -> Result<(), String> {
        call("fs.remove", json!({"path": path})).map(|_| ())
    }

    /// Does `path` exist (needs a readable root)?
    pub fn exists(path: &str) -> Result<bool, String> {
        call("fs.exists", json!({"path": path})).map(|v| v.as_bool().unwrap_or(false))
    }

    /// An HTTP request with a binary body (e.g. a WebDAV `PUT` of an exported file).
    pub fn http_bytes(method: &str, url: &str, headers: Value, content_type: &str, body: &[u8]) -> Result<Value, String> {
        call(
            "http.request",
            json!({"method": method, "url": url, "headers": headers, "contentType": content_type, "bodyBase64": crate::base64::encode(body)}),
        )
    }

    /// The programs this plug-in declares: `[{program, name, granted, installed: [{bundleId?, path?,
    /// version?}]}]`. Needs nothing; tells the user what to install or allow. `path` only once the
    /// program is granted.
    pub fn program_list() -> Result<Value, String> {
        call("program.list", json!({}))
    }

    /// Starts a granted program (`programs` permission): `files` (`{name: text}`) are written into
    /// a fresh folder for the run first, and `{tmpDir}` in `args` becomes its path. → `{run,
    /// tmpDir}`; the plug-in's `program.end` hook is called when it exits or the user stops
    /// waiting. `bundle_id` picks one of the entry's versions (macOS).
    pub fn program_start(program: &str, args: &[&str], files: Value, bundle_id: Option<&str>) -> Result<Value, String> {
        call("program.start", json!({"program": program, "args": args, "files": files, "bundleId": bundle_id}))
    }

    /// Adds the file at `path`, made from photo `from`, to the library (`libraryAdd` permission).
    /// `path` must be in the folder of `from`'s original or below it. With `stack`, it is stacked
    /// on `from`; with `album`, it goes in that album, which must be a regular album holding
    /// `from`. A file the library already has is read again if it changed. → `{photo, added,
    /// reloaded}`
    pub fn library_add(path: &str, from: &str, stack: bool, album: Option<&str>) -> Result<Value, String> {
        call("library.add", json!({"path": path, "from": from, "stack": stack, "album": album}))
    }

    /// A downsized copy of photo `photo` (`pixels` permission): the whole frame as the crop tool
    /// shows it before cropping (orientation, flips, lens and perspective; no crop, no
    /// straighten), long edge at most `long_edge` (the host keeps it within 64..=1536), 8-bit sRGB.
    /// A point's position in it, normalized, is where [`set_crop`] puts that point.
    pub fn photo_pixels(photo: &str, long_edge: u32) -> Result<crate::Pixels, String> {
        let mut buf = call_raw("photo.pixels", json!({"photo": photo, "longEdge": long_edge}))?;
        // the base64 is decoded straight from the reply and cut out of it before the rest is
        // parsed: parsing a megabyte into a JSON string first would double the work, which an
        // interpreted plug-in feels (base64 never needs escaping, so its string ends at a quote)
        const KEY: &[u8] = b"\"data\":\"";
        let data = buf
            .windows(KEY.len())
            .position(|w| w == KEY)
            .map(|i| i + KEY.len())
            .and_then(|start| buf[start..].iter().position(|&b| b == b'"').map(|len| start..start + len));
        let rgb = match data {
            Some(r) => {
                let rgb = crate::base64::decode_bytes(&buf[r.clone()])?;
                buf.drain(r);
                Some(rgb)
            }
            None => None,
        };
        let v = parse_reply(&buf)?;
        let dim = |k: &str| v[k].as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| format!("photo.pixels: no `{k}`"));
        let (width, height) = (dim("width")?, dim("height")?);
        let rgb = match rgb {
            Some(rgb) => rgb,
            None => crate::base64::decode(v["data"].as_str().unwrap_or_default())?,
        };
        // checked: on wasm32 a usize product of two reply numbers can overflow
        if width.checked_mul(height).and_then(|n| n.checked_mul(3)) != Some(rgb.len()) {
            return Err(format!("photo.pixels: {} bytes for {width} × {height}", rgb.len()));
        }
        Ok(crate::Pixels { width, height, rgb })
    }

    /// Makes a virtual copy of photo `photo` (`virtualCopy` permission): a new catalog entry
    /// sharing its file, with a copy of its settings, stacked after it, named `name` or "Copy N".
    /// → the copy's id. One History step; the selection stays as it was.
    pub fn virtual_copy(photo: &str, name: Option<&str>) -> Result<String, String> {
        let v = call("photo.virtualCopy", json!({"photo": photo, "name": name}))?;
        v["photo"].as_str().map(str::to_string).ok_or_else(|| "photo.virtualCopy: no `photo` in the reply".into())
    }

    /// Sets photo `photo`'s crop (`crop` permission): `rect` `[x0, y0, x1, y1]`, normalized (0..1)
    /// in the frame [`photo_pixels`] gives turned by `angle` (degrees, clockwise; `None` is 0) about
    /// its centre, so a box found in those pixels, turned to level them, is set as found. One
    /// History step; the user adjusts it in the crop tool like any crop. → `{rect, angle}` as set
    /// (the host fits it inside the photo).
    pub fn set_crop(photo: &str, rect: [f64; 4], angle: Option<f64>) -> Result<Value, String> {
        call("develop.setCrop", json!({"photo": photo, "rect": rect, "angle": angle}))
    }

    /// An HTTP request (needs the URL's host in the `network` permission) →
    /// `{status, contentType, location, body}`.
    pub fn http(method: &str, url: &str, headers: Value, body: Option<&str>) -> Result<Value, String> {
        call("http.request", json!({"method": method, "url": url, "headers": headers, "body": body}))
    }
}

/// A job's calls (a command with `"job": true`; needs `"jobs"` in `requires`): its row in the
/// activity stack and its cancel. Outside a job they fail (`cancelled()` is then false).
///
/// A job keeps each call short and goes on with [`continue_with`]:
///
/// ```ignore
/// fn handle(req: &Request) -> Result<Value, String> {
///     let photos = req.selection();
///     let mut next = req.state().as_u64().unwrap_or(0) as usize;
///     for photo in photos.iter().skip(next).take(20) {
///         if job::cancelled() {
///             return Ok(json!({"message": format!("Stopped after {next} photos")}));
///         }
///         job::detail(photo);
///         // … work on the photo …
///         job::item_done();
///         next += 1;
///     }
///     job::progress(next as u64, photos.len() as u64);
///     if next < photos.len() { Ok(job::continue_with(json!(next))) } else { Ok(json!({"message": "Done"})) }
/// }
/// ```
pub mod job {
    use super::*;

    /// `done` of `total` things (0 total: not known, the bar sweeps).
    pub fn progress(done: u64, total: u64) {
        let _ = host::call("job.progress", json!({"done": done, "total": total}));
    }

    /// [`progress`] in `unit`: `"count"`, `"bytes"` ("12 of 340 MB") or `"percent"`.
    pub fn progress_in(done: u64, total: u64, unit: &str) -> Result<(), String> {
        host::call("job.progress", json!({"done": done, "total": total, "unit": unit})).map(|_| ())
    }

    /// What the job is working on: a name (a file, a photo), not a sentence.
    pub fn detail(text: &str) {
        let _ = host::call("job.detail", json!({"text": text}));
    }

    /// One photo done: the row counts photos from now on, with the time left and a summary at the end.
    pub fn item_done() {
        let _ = host::call("job.itemDone", json!({}));
    }

    /// One photo failed; `why` names it and gives the reason (kept for the summary).
    pub fn item_failed(why: &str) {
        let _ = host::call("job.itemFailed", json!({"why": why}));
    }

    /// Whether the user stopped the job (✕). From then on every other host call fails with
    /// "cancelled", and a reply asking to continue is not called again: stop, and say how far it got.
    pub fn cancelled() -> bool {
        host::call("job.cancelled", json!({})).ok().and_then(|v| v.as_bool()).unwrap_or(false)
    }

    /// The reply that calls the job again, in a fresh instance with a fresh budget, with `state` as
    /// [`Request::state`](crate::Request::state). The next request must still fit in a message.
    pub fn continue_with(state: Value) -> Value {
        json!({"continue": state})
    }
}

/// The plug-in crate's own `Cargo.toml` package fields, as [`package!`] reads them at compile
/// time (an unset field is empty).
#[derive(Clone, Copy, Debug, Default)]
pub struct Package {
    pub version: &'static str,
    pub description: &'static str,
    /// `authors`, `:`-separated as Cargo gives them.
    pub authors: &'static str,
    pub homepage: &'static str,
    pub repository: &'static str,
    pub license: &'static str,
}

/// The [`Package`] of the crate this is written in (it expands there, so `env!` reads that
/// crate's `Cargo.toml`).
#[macro_export]
macro_rules! package {
    () => {
        $crate::Package {
            version: env!("CARGO_PKG_VERSION"),
            description: env!("CARGO_PKG_DESCRIPTION"),
            authors: env!("CARGO_PKG_AUTHORS"),
            homepage: env!("CARGO_PKG_HOMEPAGE"),
            repository: env!("CARGO_PKG_REPOSITORY"),
            license: env!("CARGO_PKG_LICENSE"),
        }
    };
}

/// Manifest JSON `manifest` with `version`, `description`, `author`, `homepage`, `repository` and
/// `license` filled from `package` where the JSON doesn't set them; a field `package` leaves empty
/// stays out (`author` joins Cargo's `authors` with ", "). Manifest text that isn't a JSON object
/// is returned as it is, for the host to report.
pub fn with_package(manifest: &str, package: &Package) -> String {
    let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(manifest) else { return manifest.to_string() };
    let authors = package.authors.split(':').map(str::trim).filter(|a| !a.is_empty()).collect::<Vec<_>>().join(", ");
    let fields = [
        ("version", package.version),
        ("description", package.description),
        ("author", authors.as_str()),
        ("homepage", package.homepage),
        ("repository", package.repository),
        ("license", package.license),
    ];
    for (key, value) in fields {
        let value = value.trim();
        if !value.is_empty() && !m.contains_key(key) {
            m.insert(key.into(), Value::from(value));
        }
    }
    Value::Object(m).to_string()
}

/// Exports the ABI: `export_plugin!(MANIFEST_JSON, handler)` with
/// `fn handler(&Request) -> Result<Value, String>`. `export_plugin!(package: MANIFEST_JSON,
/// handler)` fills the manifest's `version`, `description`, `author`, `homepage`, `repository` and
/// `license` from the plug-in crate's `Cargo.toml` where the JSON leaves them out ([`with_package`]).
#[macro_export]
macro_rules! export_plugin {
    (package: $manifest:expr, $handler:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn lcc_abi_version() -> i32 {
            $crate::ABI_VERSION
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn lcc_manifest() -> i64 {
            $crate::__manifest(&$crate::with_package($manifest, &$crate::package!()))
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn lcc_alloc(len: i32) -> i32 {
            $crate::__alloc(len)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn lcc_handle(ptr: i32, len: i32) -> i64 {
            $crate::__handle(ptr, len, $handler)
        }
    };
    ($manifest:expr, $handler:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn lcc_abi_version() -> i32 {
            $crate::ABI_VERSION
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn lcc_manifest() -> i64 {
            $crate::__manifest($manifest)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn lcc_alloc(len: i32) -> i32 {
            $crate::__alloc(len)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn lcc_handle(ptr: i32, len: i32) -> i64 {
            $crate::__handle(ptr, len, $handler)
        }
    };
}

#[cfg(test)]
mod tests_package {
    use super::{Package, with_package};
    use serde_json::{Value, json};

    #[test]
    fn package_fields_fill_only_what_the_manifest_leaves_out() {
        let p = Package {
            version: "1.2.3",
            description: "From Cargo",
            authors: "Ann <a@x.org>:Bo",
            homepage: "",
            repository: "https://github.com/me/geo",
            license: "MIT",
        };
        let m: Value = serde_json::from_str(&with_package(r#"{"id": "x", "name": "X", "description": "Mine"}"#, &p)).unwrap();
        assert_eq!(
            m,
            json!({"id": "x", "name": "X", "description": "Mine", "version": "1.2.3", "author": "Ann <a@x.org>, Bo", "repository": "https://github.com/me/geo", "license": "MIT"})
        );
        assert_eq!(with_package("not json", &p), "not json");
        assert_eq!(with_package("[1]", &p), "[1]");
    }
}

#[cfg(test)]
mod tests_base64 {
    use super::base64::{decode, encode};

    #[test]
    fn base64_round_trips_and_refuses_what_is_not_base64() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(decode(&encode(&bytes)).unwrap(), bytes, "length {len}");
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&all)).unwrap(), all);
        assert_eq!(decode("TWFu").unwrap(), b"Man");
        assert_eq!(decode("TWE=").unwrap(), b"Ma");
        for bad in ["TWF", "TW!u", "T===", "TQ==TWFu", "TWFu\n"] {
            assert!(decode(bad).is_err(), "{bad:?}");
        }
    }
}
