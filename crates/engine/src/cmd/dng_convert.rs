//! Adobe DNG Converter for Nikon High Efficiency NEF (issue #3921; see [`crate::dng_converter`]).
//!
//! `photo.convertHighEfficiency` is split like Reload from Disk so the app can convert on a worker thread: [`plan`]
//! lists the targeted HE NEFs shown from their preview (no file-system calls), [`run`] converts them and reads their
//! files again without the session, and [`apply`] commits what came of it as Reload does (one undo step). The command
//! itself (agents, CLI, MCP) does all three at once.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use lightcraft_catalog::{PhotoId, Source};
use serde_json::{Value, json};

use super::reload::{self, Item};
use super::{CommandSpec, always, bad, cmd, has_selection};
use crate::dng_converter as dc;
use crate::media::{FileProbe, ProbeInfo};
use crate::{Result, Session};

const CONVERT: &str = "photo.convertHighEfficiency";

/// Whether the photo is a Nikon High Efficiency NEF shown from its embedded preview, with the content hash its DNG is
/// cached under.
pub fn is_candidate(p: &lightcraft_catalog::Photo) -> bool {
    p.copy_of.is_none()
        && p.preview_only.as_deref().is_some_and(lightcraft_raw::is_nef_high_efficiency)
        && p.content_hash.as_deref().is_some_and(|h| !h.is_empty())
        && matches!(&p.source, Source::File { path } if dc::is_nef_path(Path::new(path)))
}

/// A photo to convert: what Reload needs, and the content hash its DNG is cached under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub item: Item,
    pub content_hash: String,
}

/// The photos `p` targets (`ids`, else the selection) that are High Efficiency NEFs shown from their preview. No
/// file-system calls.
pub fn plan(s: &mut Session, p: &Value) -> Result<Vec<Planned>> {
    let items = reload::plan(s, p)?;
    Ok(items
        .into_iter()
        .filter_map(|item| {
            let ph = s.catalog.photo(item.id).filter(|ph| is_candidate(ph))?;
            let content_hash = ph.content_hash.clone()?;
            Some(Planned { item, content_hash })
        })
        .collect())
}

/// What [`run`] did: per planned photo, the conversion's result and the file read again afterwards.
pub struct Outcome {
    pub converted: Vec<std::result::Result<PathBuf, String>>,
    pub probed: Vec<std::result::Result<ProbeInfo, String>>,
}

/// Convert each planned file with `converter` and read the converted files again as Reload does. Doesn't touch the
/// session. `progress(done)` counts conversions.
pub fn run(converter: &Path, probe: Option<&FileProbe>, planned: &[Planned], cancel: &AtomicBool, progress: &dyn Fn(usize)) -> Outcome {
    let jobs: Vec<dc::Job> = planned.iter().map(|p| dc::Job { path: PathBuf::from(&p.item.path), content_hash: p.content_hash.clone() }).collect();
    let converted = dc::convert_all(converter, &jobs, dc::settings().timeout, cancel, progress);
    let ok: Vec<Item> = planned.iter().zip(&converted).filter(|(_, r)| r.is_ok()).map(|(p, _)| p.item.clone()).collect();
    let mut read = reload::probe(probe, &ok, cancel, &|_| {}).into_iter();
    let probed = converted.iter().map(|r| match r {
        Ok(_) => read.next().unwrap_or_else(|| Err("not read".into())),
        Err(e) => Err(e.clone()),
    });
    Outcome { probed: probed.collect(), converted }
}

/// Remove photo `id`'s smart preview: one made from its embedded JPEG doesn't stand for the photo developed from its
/// DNG, nor the other way round (it is made again from the file when asked for).
fn drop_smart_preview(s: &Session, id: PhotoId) {
    if let (Some(dir), Some(p)) = (&s.media.smart_dir, s.catalog.photo(id)) {
        let _ = std::fs::remove_file(crate::smart::sharded_file_path(dir, p));
        let _ = std::fs::remove_file(dir.join(crate::smart::file_name(p)));
    }
}

/// Commit [`run`]'s results as Reload from Disk does → `{converted: [ids], failed: [{id, error}], reloaded: [ids]}`.
/// The photos that now develop from their DNG get new render keys (`media::quick::decoder_key`) and lose their smart
/// preview.
pub fn apply(s: &mut Session, planned: Vec<Planned>, outcome: Outcome) -> Result<Value> {
    let mut converted = Vec::new();
    let mut failed = Vec::new();
    for (p, r) in planned.iter().zip(&outcome.converted) {
        match r {
            Ok(_) => converted.push(p.item.id.0),
            Err(e) => failed.push(json!({"id": p.item.id.0, "error": e})),
        }
    }
    let items: Vec<Item> = planned.into_iter().map(|p| p.item).collect();
    let reloaded = reload::apply(s, items, outcome.probed)?;
    for id in &reloaded {
        drop_smart_preview(s, *id);
    }
    Ok(json!({"converted": converted, "failed": failed, "reloaded": reloaded.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// The converter to run, or why there is none.
pub fn converter_or_err() -> Result<PathBuf> {
    dc::converter().ok_or_else(|| {
        bad(
            CONVERT,
            match dc::settings().path {
                Some(p) => format!("Adobe DNG Converter was not found at {}", p.display()),
                None => "Adobe DNG Converter is not installed (it is a free download from Adobe); or set its path with app.dngConverter".into(),
            },
        )
    })
}

fn convert(s: &mut Session, p: &Value) -> Result<Value> {
    let planned = plan(s, p)?;
    if planned.is_empty() {
        return Err(bad(CONVERT, "none of the targeted photos is a Nikon High Efficiency NEF shown from its embedded preview"));
    }
    let converter = converter_or_err()?;
    let probe = reload::file_probe(s);
    let outcome = run(&converter, probe.as_ref(), &planned, &AtomicBool::new(false), &|_| {});
    apply(s, planned, outcome)
}

/// `app.dngConverter` → the settings, where the converter is and what the cache holds.
pub fn status_json() -> Value {
    let st = dc::settings();
    let (files, bytes) = dc::cache_usage();
    json!({
        "auto": st.auto,
        "path": st.path.as_ref().map(|p| p.to_string_lossy().to_string()),
        "timeoutSeconds": st.timeout.as_secs(),
        "found": dc::converter().map(|p| p.to_string_lossy().to_string()),
        "defaultPaths": dc::default_locations().iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "cacheDir": dc::cache_dir().map(|p| p.to_string_lossy().to_string()),
        "cachedFiles": files,
        "cachedBytes": bytes,
    })
}

fn settings_cmd(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "app.dngConverter";
    let mut st = dc::settings();
    if let Some(v) = p.get("auto").filter(|v| !v.is_null()) {
        st.auto = v.as_bool().ok_or_else(|| bad(C, "auto must be true or false"))?;
    }
    if let Some(v) = p.get("path") {
        st.path = match v {
            Value::Null => None,
            Value::String(t) if t.trim().is_empty() => None,
            Value::String(t) => Some(PathBuf::from(t.trim())),
            _ => return Err(bad(C, "path must be a string or null")),
        };
    }
    if let Some(v) = p.get("timeoutSeconds").filter(|v| !v.is_null()) {
        let secs = v.as_u64().filter(|n| (5..=3600).contains(n)).ok_or_else(|| bad(C, "timeoutSeconds must be a whole number in 5..3600"))?;
        st.timeout = std::time::Duration::from_secs(secs);
    }
    dc::set_settings(st);
    Ok(status_json())
}

/// The library's photos that develop from a converted DNG (the catalog's record; no file is read).
pub fn converted_photos(s: &Session) -> Vec<PhotoId> {
    s.catalog.photos().filter(|p| p.copy_of.is_none() && p.converted_dng()).map(|p| p.id).collect()
}

fn clear_cache(s: &mut Session, p: &Value) -> Result<Value> {
    // the photos developed from a cached DNG go back to their embedded preview: read again once it's gone (the app
    // passes `reload: false` and reloads them in the background itself)
    let ids = converted_photos(s);
    let (files, bytes) = dc::clear_cache();
    let mut reloaded: Vec<PhotoId> = Vec::new();
    if !ids.is_empty() && p.get("reload").and_then(Value::as_bool) != Some(false) {
        let items = reload::plan(s, &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()}))?;
        let probe = reload::file_probe(s);
        let results = reload::probe(probe.as_ref(), &items, &AtomicBool::new(false), &|_| {});
        reloaded = reload::apply(s, items, results)?;
        for id in &reloaded {
            drop_smart_preview(s, *id);
        }
    }
    Ok(json!({
        "deletedFiles": files,
        "freedBytes": bytes,
        "converted": ids.iter().map(|i| i.0).collect::<Vec<_>>(),
        "reloaded": reloaded.iter().map(|i| i.0).collect::<Vec<_>>(),
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "photo.convertHighEfficiency",
            "Convert with Adobe DNG Converter",
            [],
            None,
            "{ids?, wait?} — Nikon High Efficiency (HE / HE★) NEFs shown from their embedded preview are converted to DNG by Adobe DNG Converter, when the user installed it (a separate free program; LightCraft has no licence for the codec), and then developed from that DNG; the DNG is kept in LightCraft's cache (`app.dngConverter` → cacheDir), the NEF stays the photo's file and is never changed; in the app the conversion runs in the background (activity row `dngConvert`, cancellable) and the reply is {background: true, count} unless `wait: true` → {converted: [ids], failed: [{id, error}], reloaded: [ids]}",
            has_selection,
            convert
        ),
        cmd!(
            query "app.dngConverter",
            "Adobe DNG Converter",
            [],
            None,
            "{auto?: bool, path?: string|null, timeoutSeconds?: number} — the Adobe DNG Converter fallback for Nikon High Efficiency NEF: `auto` (default false) converts such a photo in the app when it is opened in Edit, `path` is the converter's program (or macOS .app) when it isn't in the default place (null: the default), `timeoutSeconds` (5..3600, default 120) stops one conversion; returns {auto, path, timeoutSeconds, found (the program that will run, or null), defaultPaths, cacheDir, cachedFiles, cachedBytes}; the settings belong to this process (a headless CLI or MCP process starts with the defaults)",
            always,
            settings_cmd
        ),
        cmd!(
            "library.clearDngCache",
            "Delete Converted DNGs",
            [],
            None,
            "{reload?: bool} — delete the DNGs Adobe DNG Converter made for Nikon High Efficiency NEFs (the cache is shared by every library on this computer); this library's photos that developed from them are read again (unless `reload: false`) and show their embedded preview until converted again → {deletedFiles, freedBytes, converted: [ids that developed from a DNG], reloaded: [ids]}",
            always,
            clear_cache
        ),
    ]
}
