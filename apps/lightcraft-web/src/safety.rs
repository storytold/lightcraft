//! Keeping a library in the browser safe: notices the user must see (a failed save, storage the
//! browser may evict, a file that couldn't be stored), one tab at a time per library, a message
//! instead of a dead page after a panic, and backup / restore of the whole library
//! ([`crate::backup`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::backend::Backend;
use crate::backup::{
    ACTIVE_LIBRARY, ProviderZip, README, ZipWriter, data_offset, find_central, original_entry, parse_central, restore_key, tail_len, valid_hash,
    verify, verify_original, verify_original_bytes,
};
use crate::files::{Files, LIBRARY_FILES, Snapshot};
use crate::store::storage_key;

thread_local! {
    /// Messages for the user, shown by the app on its next frame.
    static NOTICES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static RESTORE_SERIAL: RefCell<u64> = const { RefCell::new(0) };
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
        match backend.read(&storage_key(&hash)).await {
            Ok(Some(data)) => {
                let name = names.get(&hash).map_or("original", String::as_str);
                add(&mut zip, &original_entry(&hash, name), &data)?;
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

/// A complete native project capture. Only originals referenced by the captured catalog's
/// `names` are read; unrelated originals left in browser storage are never uploaded.
pub async fn provider_backup(snapshot: Snapshot, backend: Backend, names: HashMap<String, String>) -> Result<Vec<u8>, String> {
    let mut zip = ProviderZip::default();
    let mut seen = HashSet::new();
    if !snapshot.entries.iter().any(|(name, _)| matches!(name.as_str(), "catalog.snap" | "catalog.log")) {
        return Err("the captured library has no catalog file".into());
    }
    if names.len() > usize::from(u16::MAX) {
        return Err("too many referenced originals for one library backup".into());
    }
    for (hash, name) in &names {
        if !valid_hash(hash) || name.len().saturating_add("originals/".len() + hash.len() + 1) > usize::from(u16::MAX) {
            return Err("invalid referenced original hash or filename".into());
        }
    }
    zip.entry("README.txt", README.as_bytes())?;
    for (name, bytes) in snapshot.entries {
        if !LIBRARY_FILES.contains(&name.as_str()) || !seen.insert(name.clone()) {
            return Err("invalid or duplicate captured library file".into());
        }
        zip.entry(&format!("library/{name}"), &bytes)?;
    }
    let mut originals: Vec<_> = names.into_iter().collect();
    originals.sort_by(|a, b| a.0.cmp(&b.0));
    for (hash, name) in originals {
        let name = original_entry(&hash, &name);
        let remaining = zip.remaining_for(&name)?;
        let bytes = backend
            .read_capped(&storage_key(&hash), remaining)
            .await?
            .ok_or_else(|| format!("{name}: the referenced original is missing from browser storage"))?;
        verify_original_bytes(&name, &hash, &bytes)?;
        zip.entry(&name, &bytes)?;
    }
    zip.finish()
}

/// Bytes `start..end` of a picked file.
async fn read_range(file: &web_sys::File, start: u64, end: u64) -> Result<Vec<u8>, String> {
    let size = file.size();
    if !size.is_finite() || size < 0.0 || start > end || end as f64 > size {
        return Err("damaged backup: file range is outside the ZIP".into());
    }
    let blob = file.slice_with_f64_and_f64(start as f64, end as f64).map_err(|e| format!("{e:?}"))?;
    let buf = JsFuture::from(blob.array_buffer()).await.map_err(|e| format!("{e:?}"))?;
    Ok(Uint8Array::new(&buf).to_vec())
}

/// Restore into a new folder, without changing ACTIVE_LIBRARY or admitting a catalog. The
/// caller can validate the staged catalog before activating it; current work stays in use.
pub async fn restore_staged(file: web_sys::File, backend: Backend) -> Result<String, String> {
    let len = file.size() as u64;
    let tail = read_range(&file, len - tail_len(len), len).await?;
    let (cd_offset, cd_size) = find_central(&tail)?;
    let cd_end = cd_offset.checked_add(cd_size).ok_or("damaged ZIP directory size")?;
    let cd = read_range(&file, cd_offset, cd_end).await?;
    let entries = parse_central(&cd)?;
    if !entries.iter().any(|e| e.name == "library/catalog.snap" || e.name == "library/catalog.log") {
        return Err("this isn't a LightCraft library backup (no library/catalog files)".into());
    }
    let dir = new_restore_dir(&backend).await?;
    let mut targets = HashSet::new();
    for entry in &entries {
        if let Some(key) = restore_key(&entry.name, &dir)
            && !targets.insert(key)
        {
            return Err("backup contains duplicate restored files".into());
        }
    }
    let mut written = 0usize;
    for entry in &entries {
        match restore_entry(&file, &backend, &dir, entry, cd_offset).await {
            Ok(true) => written += 1,
            Ok(false) => {}
            Err(error) => return Err(failed_staged_restore(&backend, &dir, error).await),
        }
    }
    log::info!("lightcraft: restored {written} files into {dir}");
    Ok(dir)
}

/// A failed validation must not leave the new library's private files occupying browser quota.
/// The pointer check also preserves a library whose activation committed before an I/O error.
pub(crate) async fn cleanup_staged(backend: &Backend, dir: &str) -> Result<(), String> {
    let active = backend.read_capped(ACTIVE_LIBRARY, 128).await?;
    crate::backup::discard_staged_library(dir, active.as_deref(), |key| {
        let backend = backend.clone();
        async move { backend.remove(&key).await }
    })
    .await
}

pub(crate) async fn failed_staged_restore(backend: &Backend, dir: &str, error: String) -> String {
    match cleanup_staged(backend, dir).await {
        Ok(()) => error,
        Err(cleanup) => format!("{error}; could not remove staged library files: {cleanup}"),
    }
}

async fn restore_entry(file: &web_sys::File, backend: &Backend, dir: &str, entry: &crate::backup::ZipEntry, cd_offset: u64) -> Result<bool, String> {
    let Some(key) = restore_key(&entry.name, dir) else { return Ok(false) };
    let header_end = entry.header.checked_add(30).ok_or("damaged ZIP entry offset")?;
    let local = read_range(file, entry.header, header_end).await?;
    let start = data_offset(entry, &local)?;
    let end = start.checked_add(entry.size).filter(|end| *end <= cd_offset).ok_or("damaged ZIP entry range")?;
    let data = read_range(file, start, end).await?;
    verify(entry, &data)?;
    if let Some(hash) = key.strip_prefix("originals/") {
        verify_original(entry, hash, &data)?;
        // Existing content-addressed bytes are not overwritten. Verify both the archive
        // and stored copy rather than silently trusting a corrupted existing original.
        let max = usize::try_from(entry.size).map_err(|_| "original is too large for this browser")?;
        if let Some(existing) = backend.read_capped(&key, max).await? {
            verify_original_bytes(&entry.name, hash, &existing)?;
            return Ok(false);
        }
    }
    backend.write(&key, &data).await.map_err(|error| format!("storing {}: {error}", entry.name))?;
    Ok(true)
}

async fn new_restore_dir(backend: &Backend) -> Result<String, String> {
    let active = backend.read_capped(ACTIVE_LIBRARY, 128).await?;
    for _ in 0..64 {
        let serial = RESTORE_SERIAL.with(|counter| {
            let mut value = counter.borrow_mut();
            *value = value.checked_add(1).ok_or("restore folder counter exhausted")?;
            Ok::<_, String>(*value)
        })?;
        // A nonce avoids sharing a staging folder with another tab/restoration realm even
        // when clocks and counters coincide. The counter also prevents same-realm reuse.
        let crypto = Reflect::get(&js_sys::global(), &"crypto".into()).map_err(|error| format!("restore nonce: {error:?}"))?;
        let random = Reflect::get(&crypto, &"getRandomValues".into())
            .map_err(|error| format!("restore nonce: {error:?}"))?
            .dyn_into::<js_sys::Function>()
            .map_err(|_| "secure randomness is unavailable for staging a restore")?;
        let nonce = Uint8Array::new_with_length(16);
        random.call1(&crypto, &nonce).map_err(|error| format!("restore nonce: {error:?}"))?;
        let nonce: String = nonce.to_vec().iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = format!("library-restored-{nonce}-{serial}");
        if active.as_deref() == Some(dir.as_bytes()) {
            continue;
        }
        let mut exists = false;
        for name in LIBRARY_FILES {
            if backend.exists(&format!("{dir}/{name}")).await? {
                exists = true;
                break;
            }
        }
        if !exists {
            return Ok(dir);
        }
    }
    Err("could not choose an unused restored-library folder".into())
}

/// Official Restore keeps its activation behavior, after the verified staging succeeds.
pub async fn restore(file: web_sys::File, backend: Backend) -> Result<String, String> {
    let dir = restore_staged(file, backend.clone()).await?;
    if let Err(error) = backend.write(ACTIVE_LIBRARY, dir.as_bytes()).await {
        return Err(failed_staged_restore(&backend, &dir, error).await);
    }
    Ok(dir)
}

/// File ▸ Restore Library from Backup…: confirm, pick the zip, restore, reload.
pub fn pick_and_restore(backend: Backend, before_reload: impl FnOnce() + 'static) {
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
        notice(format!("Restoring {}…", file.name()));
        wasm_bindgen_futures::spawn_local(async move {
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
