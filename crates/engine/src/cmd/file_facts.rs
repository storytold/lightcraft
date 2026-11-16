//! File facts recorded for photos already in the library (issue #4367). Some things about a file are written to
//! the catalog when it is imported (which segmentation mattes it carries, `Meta::mattes`), so photos imported
//! before LightCraft read them say "not looked at" until something reads their file again. This pass reads those
//! files once and records what is missing, and nothing else: no edit, no user metadata, none of the size, hash or
//! lens bookkeeping Reload from Disk redoes.
//!
//! Only a fact that can say "not looked at" apart from "looked at, none" can be listed here, or the pass would
//! never be done. Today that is the mattes of the formats the probe looks into ([`LOOKED_INTO`]); other facts join
//! as they gain that state.
//!
//! Split like [`super::lens_refresh`] so the app probes on a worker thread: [`missing`] lists what to probe (no
//! file-system calls), [`probe`] reads the files without the session, [`apply`] applies the results as LightCraft's
//! own bookkeeping (journaled, not an undo step). Nothing runs by itself: the desktop app offers the pass, and agents
//! and tests use `library.recordFileFacts`.

use lightcraft_catalog::{Catalog, Op, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::media::{FileProbe, ProbeInfo};
use crate::{Result, Session};

/// The formats whose mattes the import probe records (`crate::files`): only these can be "not looked at" and then
/// looked at. A format added to the probe is added here.
pub const LOOKED_INTO: [&str; 1] = ["DNG"];

/// A photo to probe, and its file when listed (a result for a photo moved meanwhile is dropped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub id: PhotoId,
    pub path: String,
}

/// Library photos with a file whose facts were never looked at (`ids`: only these; a virtual copy lists the photo
/// it was copied from, once). No file-system calls.
pub fn missing(cat: &Catalog, ids: Option<&[PhotoId]>) -> Vec<Missing> {
    let mut seen = std::collections::HashSet::new();
    cat.photos()
        .filter(|p| ids.is_none_or(|ids| ids.contains(&p.id) || p.copy_of.is_some_and(|c| ids.contains(&c))))
        .map(|p| p.copy_of.and_then(|c| cat.photo(c)).map_or(&**p, |orig| &**orig))
        .filter(|p| p.meta.mattes.is_none() && LOOKED_INTO.iter().any(|f| p.format.eq_ignore_ascii_case(f)))
        .filter_map(|p| super::missing::checked_path(p).map(|path| Missing { id: p.id, path: path.to_string() }))
        .filter(|m| seen.insert(m.id))
        .collect()
}

/// Probe each listed photo's file as import would. Doesn't touch the session.
pub fn probe(probe: &FileProbe, list: Vec<Missing>) -> Vec<(Missing, std::result::Result<ProbeInfo, String>)> {
    let paths: Vec<String> = list.iter().map(|x| x.path.clone()).collect();
    list.into_iter().zip(crate::import::probe_with(probe, &paths)).collect()
}

/// [`probe`] for a worker that reports progress and can stop (`progress(done)` while it runs; once `cancel` is set
/// no further file is started, and the files not reached come back as errors, left pending by [`apply`]).
pub fn probe_tracked(
    probe: &FileProbe,
    list: Vec<Missing>,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &dyn Fn(usize),
) -> Vec<(Missing, std::result::Result<ProbeInfo, String>)> {
    let items: Vec<super::reload::Item> = list.iter().map(|m| super::reload::Item { id: m.id, path: m.path.clone(), ticket: 0 }).collect();
    let results = super::reload::probe(Some(probe), &items, cancel, progress);
    list.into_iter().zip(results).collect()
}

/// Apply [`probe`]'s results: each photo and its virtual copies get the facts their file gives that the catalog
/// lacked, in one batch that is journaled but isn't an undo step. A file that couldn't be read (offline) changes
/// nothing and stays listed for the next pass. Returns the photos that were recorded.
pub fn apply(s: &mut Session, results: Vec<(Missing, std::result::Result<ProbeInfo, String>)>) -> Result<Vec<PhotoId>> {
    let mut ops = Vec::new();
    let mut recorded = Vec::new();
    let mut copies: std::collections::HashMap<PhotoId, Vec<PhotoId>> = std::collections::HashMap::new();
    for p in s.catalog.photos() {
        if let Some(original) = p.copy_of {
            copies.entry(original).or_default().push(p.id);
        }
    }
    for (item, info) in results {
        let Ok(info) = info else { continue };
        // the photo moved or left the library while its file was read: the result is stale, for its copies too
        if s.catalog.photo(item.id).and_then(|p| super::missing::checked_path(p)) != Some(item.path.as_str()) {
            continue;
        }
        let Some(mattes) = info.meta.mattes else { continue };
        let family = std::iter::once(item.id).chain(copies.get(&item.id).into_iter().flatten().copied());
        for id in family {
            let Some(p) = s.catalog.photo(id) else { continue };
            if p.meta.mattes.is_some() {
                continue;
            }
            let mut meta = p.meta.clone();
            meta.mattes = Some(mattes.clone());
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
            recorded.push(id);
        }
    }
    if !ops.is_empty() {
        s.apply_system(Op::Batch { ops })?;
    }
    Ok(recorded)
}

fn ids_param(p: &Value, c: &str) -> Result<Option<Vec<PhotoId>>> {
    match p.get("ids") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => {
            Ok(Some(a.iter().map(|v| v.as_u64().map(PhotoId).ok_or_else(|| bad(c, "ids are photo ids"))).collect::<Result<_>>()?))
        }
        Some(_) => Err(bad(c, "ids is a list of photo ids")),
    }
}

fn record(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.recordFileFacts";
    let fp = super::recheck::file_probe(s).ok_or_else(|| bad(C, "this session has no file access"))?;
    let ids = ids_param(p, C)?;
    let listed = missing(&s.catalog, ids.as_deref());
    let n = listed.len();
    let recorded = apply(s, probe(&fp, listed))?;
    let left = missing(&s.catalog, ids.as_deref()).len();
    Ok(json!({"checked": n, "recorded": recorded.iter().map(|i| i.0).collect::<Vec<_>>(), "pending": left}))
}

fn pending(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = ids_param(p, "library.fileFactsPending")?;
    Ok(json!({"pending": missing(&s.catalog, ids.as_deref()).len()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "library.recordFileFacts",
            "Update Photos Imported Earlier",
            [],
            None,
            "{ids?: [photo ids]} — library photos imported before LightCraft recorded what their file carries (the segmentation mattes of a DNG) are read again and get it recorded; nothing else about them changes (no edit, no metadata, no size or hash), and files that can't be read stay pending; the desktop app offers this after an update and in Settings, and runs it in the background; not an undo step → {checked, recorded: [ids], pending}",
            always,
            record
        ),
        cmd!(
            "library.fileFactsPending",
            "Photos Waiting to Be Updated",
            [],
            None,
            "{ids?: [photo ids]} — read-only: how many library photos `library.recordFileFacts` would read → {pending}",
            always,
            pending
        ),
    ]
}
