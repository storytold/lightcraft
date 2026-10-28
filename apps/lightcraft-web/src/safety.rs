//! Keeping a library in the browser safe: notices the user must see (a failed save, storage the
//! browser may evict, a file that couldn't be stored), one tab at a time per library, a message
//! instead of a dead page after a panic, and backup / restore of the whole library
//! ([`crate::backup`]).

use std::cell::RefCell;
use std::collections::HashMap;

use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::backend::Backend;
use crate::backup::{
    ACTIVE_LIBRARY, README, ZipWriter, data_offset, find_central, original_entry, parse_central, restore_key, restored_dir_name, tail_len,
    valid_hash, verify, verify_checksum,
};
use crate::files::{Files, LIBRARY_FILES};
use crate::store::storage_key;

thread_local! {
    /// Messages for the user, shown by the app on its next frame.
    static NOTICES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Queue a message for the user (shown for several seconds; also logged).
pub fn notice(text: impl Into<String>) {
    let text = text.into();
    log::warn!("{text}");
    NOTICES.with(|n| n.borrow_mut().push(text));
    crate::web::request_repaint();
}

/// Messages queued since the last call.
pub fn take_notices() -> Vec<String> {
    NOTICES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

/// Ask the user (the browser's own confirm box). `false` when it can't be shown.
pub fn confirm(text: &str) -> bool {
    web_sys::window().and_then(|w| w.confirm_with_message(text).ok()).unwrap_or(false)
}

/// Cover the page with a message (the app can't continue, or must not start).
pub fn show_blocking(text: &str) {
    let Some(doc) = document() else { return };
    let el = match doc.get_element_by_id("lightcraft_blocking") {
        Some(el) => el,
        None => {
            let Ok(el) = doc.create_element("div") else { return };
            el.set_id("lightcraft_blocking");
            let _ = el.set_attribute(
                "style",
                "position:fixed;inset:0;z-index:10;display:flex;align-items:center;justify-content:center;padding:32px;\
                 background:#1c1c1c;color:#e0e0e0;font:15px/1.5 system-ui,-apple-system,'Segoe UI',sans-serif;text-align:center;white-space:pre-line",
            );
            if let Some(body) = doc.body() {
                let _ = body.append_child(&el);
            }
            el
        }
    };
    el.set_text_content(Some(text));
    if let Some(loading) = doc.get_element_by_id("lightcraft_loading") {
        loading.remove();
    }
}

/// `panic = "abort"` on wasm: a panic ends the module. Say so on the page instead of leaving a
/// frozen canvas, and say that the stored library is intact.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = match (info.payload().downcast_ref::<&str>(), info.payload().downcast_ref::<String>()) {
            (Some(s), _) => (*s).to_string(),
            (_, Some(s)) => s.clone(),
            _ => "unknown error".into(),
        };
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        log::error!("LightCraft panicked{at}: {msg}");
        show_blocking(&format!(
            "LightCraft stopped after an internal error:\n{msg}\n\nThe library saved in this browser is kept. Reload the page to continue.\n\
             If it happens again right away, a photo may be the cause: reload with ?workers=0 or report the error."
        ));
    }));
}

/// The name of the lock that keeps a library to one tab.
const TAB_LOCK: &str = "lightcraft-library";

/// Hold the library lock for as long as this page lives (Web Locks API). `Some(true)`: ours;
/// `Some(false)`: another tab of this site has it; `None`: the browser can't tell (no Web Locks, or
/// it refused the request).
pub async fn acquire_tab_lock() -> Option<bool> {
    let nav = Reflect::get(&js_sys::global(), &"navigator".into()).ok()?;
    let locks = Reflect::get(&nav, &"locks".into()).ok().filter(|l| !l.is_undefined() && !l.is_null())?;
    let request: js_sys::Function = Reflect::get(&locks, &"request".into()).ok()?.dyn_into().ok()?;
    let opts = js_sys::Object::new();
    Reflect::set(&opts, &"ifAvailable".into(), &true.into()).ok()?;
    let granted = js_sys::Promise::new(&mut |resolve, reject| {
        let cb = Closure::once_into_js(move |lock: JsValue| -> JsValue {
            let ours = !lock.is_null() && !lock.is_undefined();
            let _ = resolve.call1(&JsValue::NULL, &ours.into());
            // keep the lock until the page goes away: a promise that never settles
            if ours { js_sys::Promise::new(&mut |_, _| {}).into() } else { JsValue::NULL }
        });
        // a refused request never calls `cb`: settle with its error instead of waiting forever
        match request.call3(&locks, &TAB_LOCK.into(), &opts, &cb) {
            Ok(pending) => {
                let catch = Reflect::get(&pending, &"catch".into()).ok().and_then(|c| c.dyn_into::<js_sys::Function>().ok());
                if let Some(catch) = catch {
                    let _ = catch.call1(&pending, &reject);
                }
            }
            Err(e) => {
                let _ = reject.call1(&JsValue::NULL, &e);
            }
        }
    });
    match JsFuture::from(granted).await {
        Ok(v) => Some(v.is_truthy()),
        Err(e) => {
            log::warn!("Web Locks request failed, starting without the one-tab guard: {e:?}");
            None
        }
    }
}

/// Offer a blob as a download named `name`.
pub fn download_blob(name: &str, blob: &web_sys::Blob) -> Result<(), String> {
    let e = |e: JsValue| format!("download failed: {e:?}");
    let url = web_sys::Url::create_object_url_with_blob(blob).map_err(e)?;
    let doc = document().ok_or("no document")?;
    let a: web_sys::HtmlAnchorElement = doc.create_element("a").map_err(e)?.unchecked_into();
    a.set_href(&url);
    a.set_download(name);
    a.click();
    // revoke once the download has started
    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    if let Some(w) = web_sys::window() {
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 60_000);
    }
    Ok(())
}

/// File ▸ Back Up Library…: a zip of the library files (as the app has them, saved or not) and
/// every original in storage, built entry by entry into the browser's memory (outside the wasm
/// heap) and downloaded. `names`: content hash → the photo's file name.
pub async fn backup(files: Files, backend: Backend, names: HashMap<String, String>) -> Result<(usize, f64), String> {
    let parts = js_sys::Array::new();
    let mut zip = ZipWriter::default();
    let mut bytes = 0f64;
    let mut add = |zip: &mut ZipWriter, name: &str, data: &[u8]| -> Result<(), String> {
        let header = zip.entry(name, data)?;
        parts.push(&Uint8Array::from(&header[..]));
        parts.push(&Uint8Array::from(data));
        bytes += (header.len() + data.len()) as f64;
        Ok(())
    };
    add(&mut zip, "README.txt", README.as_bytes())?;
    for name in LIBRARY_FILES {
        if let Some(d) = files.get(name) {
            add(&mut zip, &format!("library/{name}"), &d)?;
        }
    }
    let mut originals = 0;
    for hash in backend.list("originals").await? {
        if !valid_hash(&hash) {
            continue;
        }
        match backend.read_original_blob(&storage_key(&hash)).await {
            Ok(Some(data)) => {
                let name = names.get(&hash).map_or("original", String::as_str);
                let size = data.size() as u64;
                if size > crate::backup::MAX_BYTES {
                    return Err(format!("{name}: the library is too large for one backup file (4 GB)"));
                }
                let mut crc = crc32fast::Hasher::new();
                let mut at = 0u64;
                while at < size {
                    let end = at.saturating_add(1 << 20).min(size);
                    crc.update(&read_range(&data, at, end).await?);
                    at = end;
                }
                let header = zip.entry_with_crc(&original_entry(&hash, name), size, crc.finalize())?;
                parts.push(&Uint8Array::from(&header[..]));
                parts.push(&data);
                bytes += header.len() as f64 + size as f64;
                originals += 1;
            }
            Ok(None) => {}
            Err(e) => return Err(format!("reading an original from browser storage: {e}")),
        }
    }
    let tail = zip.finish()?;
    parts.push(&Uint8Array::from(&tail[..]));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/zip");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(|e| format!("{e:?}"))?;
    let day: String = String::from(js_sys::Date::new_0().to_iso_string()).chars().take(10).collect();
    download_blob(&format!("lightcraft-library-backup-{day}.zip"), &blob)?;
    Ok((originals, bytes + tail.len() as f64))
}

/// Bytes `start..end` of a picked file.
async fn read_range(file: &web_sys::Blob, start: u64, end: u64) -> Result<Vec<u8>, String> {
    let len = end.checked_sub(start).ok_or("invalid backup byte range")?;
    lightcraft_engine::files::check_buffered_original_size(len)?;
    if end as f64 > file.size() {
        return Err("backup is truncated".into());
    }
    let blob = file.slice_with_f64_and_f64(start as f64, end as f64).map_err(|e| format!("{e:?}"))?;
    let buf = JsFuture::from(blob.array_buffer()).await.map_err(|e| format!("{e:?}"))?;
    let bytes = crate::store::copy_browser_bytes(&Uint8Array::new(&buf))?;
    if bytes.len() as u64 != len {
        return Err("backup changed or was truncated while reading".into());
    }
    Ok(bytes)
}

/// Restore a backup into a new library folder (nothing in storage is deleted or overwritten),
/// then point [`ACTIVE_LIBRARY`] at it. Returns the folder's name; the caller reloads the page.
pub async fn restore(file: web_sys::File, backend: Backend) -> Result<String, String> {
    let len = file.size() as u64;
    let tail = read_range(&file, len - tail_len(len), len).await?;
    let (cd_offset, cd_size) = find_central(&tail)?;
    let cd = read_range(&file, cd_offset, cd_offset + cd_size).await?;
    let entries = parse_central(&cd)?;
    if !entries.iter().any(|e| e.name == "library/catalog.snap" || e.name == "library/catalog.log") {
        return Err("this isn't a LightCraft library backup (no library/catalog files)".into());
    }
    let dir = restored_dir_name(js_sys::Date::now());
    let mut written = 0usize;
    for e in &entries {
        let Some(key) = restore_key(&e.name, &dir) else { continue };
        // originals are named by their content: one already stored is the same file
        if key.starts_with("originals/") && backend.exists(&key).await? {
            continue;
        }
        let local = read_range(&file, e.header, e.header + 30).await?;
        let start = data_offset(e, &local)?;
        let end = start.checked_add(e.size).filter(|end| *end <= len).ok_or("backup entry extends past the file")?;
        if let Some(expected_hash) = key.strip_prefix("originals/") {
            let mut crc = crc32fast::Hasher::new();
            let mut hash = lightcraft_preview::Hasher128::new();
            let mut at = start;
            while at < end {
                let next = at.saturating_add(1 << 20).min(end);
                let chunk = read_range(&file, at, next).await?;
                crc.update(&chunk);
                hash.update(&chunk);
                at = next;
            }
            verify_checksum(e, e.size, crc.finalize())?;
            if hash.finish().to_string() != expected_hash {
                return Err(format!("{}: the file doesn't match its content hash (damaged or altered backup)", e.name));
            }
            let blob = file.slice_with_f64_and_f64(start as f64, end as f64).map_err(|e| format!("{e:?}"))?;
            backend.write_original_blob(&key, &blob).await.map_err(|err| format!("storing {}: {err}", e.name))?;
            written += 1;
            continue;
        }
        let data = read_range(&file, start, end).await?;
        verify(e, &data)?;
        backend.write(&key, &data).await.map_err(|err| format!("storing {}: {err}", e.name))?;
        written += 1;
    }
    backend.write(ACTIVE_LIBRARY, dir.as_bytes()).await?;
    log::info!("lightcraft: restored {written} files into {dir}");
    Ok(dir)
}

/// File ▸ Restore Library from Backup…: confirm, pick the zip, restore, reload.
pub fn pick_and_restore(backend: Backend, activity: lightcraft_engine::activity::Activity, before_reload: impl FnOnce() + 'static) {
    let ok = confirm(
        "Restore a LightCraft library backup?\n\nThe backup becomes the library in this browser and the page reloads. \
         The current library is not deleted: it stays in browser storage.",
    );
    if !ok {
        return;
    }
    let Some(doc) = document() else { return };
    let Ok(input) = doc.create_element("input").map(|e| e.unchecked_into::<web_sys::HtmlInputElement>()) else { return };
    input.set_type("file");
    input.set_accept(".zip,application/zip");
    let inp = input.clone();
    let mut before_reload = Some(before_reload);
    let on_change = Closure::<dyn FnMut()>::new(move || {
        let Some(file) = inp.files().and_then(|f| f.get(0)) else { return };
        let backend = backend.clone();
        let before = before_reload.take();
        let guard = activity.start("restore", "Restoring library", lightcraft_engine::activity::Cancel::No);
        guard.detail(&file.name());
        wasm_bindgen_futures::spawn_local(async move {
            let _guard = guard;
            match restore(file, backend).await {
                Ok(_) => {
                    if let Some(f) = before {
                        f();
                    }
                    if let Some(w) = web_sys::window() {
                        let _ = w.location().reload();
                    }
                }
                Err(e) => notice(format!("Restore failed: {e}. Nothing was changed: the current library is still in use.")),
            }
        });
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();
    input.click();
}
