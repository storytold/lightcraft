//! Preview-only raws rechecked after an update (issue #662): a raw imported while its format or
//! variant couldn't be decoded keeps [`lightcraft_catalog::Photo::preview_only`] until something re-reads its file.
//! Each flag is stamped with the app version that set it ([`lightcraft_catalog::Photo::preview_checked`]);
//! when a library opens with another version, its stale preview-only photos are probed again and get
//! exactly what Reload from Disk would give them: a file that decodes now loses the flag, one that
//! still doesn't gets the new reason and a fresh stamp, so it isn't probed again.
//! ORF records are rechecked even when previously sensor-decodable: compressed sensor support
//! was withdrawn after #3775. Supported uncompressed ORFs also keep a stamp to avoid repeat work.
//!
//! The work is split so the app can probe on a worker thread: [`stale`] lists what to probe (no
//! file-system calls), [`probe`] reads the files without the session, and [`apply`] applies the
//! results as LightCraft's own bookkeeping (journaled, not an undo step). Agents and tests use
//! `library.recheckPreviewOnly`, which does all three at once.

use lightcraft_catalog::{Op, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::media::{FileProbe, ProbeInfo};
use crate::{Result, Session};

/// A grouped source read, with Reload's membership and freshness checks.
pub use super::reload::Item as Stale;

/// The library's preview-only photos and ORFs whose support another app version checked (or
/// every such photo, with `all`), grouped by family and path under a new source-read ticket. Every
/// same-file member retains its ticket so a copy can use the result if its representative relinks.
/// Local browsing records are included. No file-system calls.
pub fn stale(s: &mut Session, all: bool) -> Vec<Stale> {
    let ids = s
        .catalog
        .photos()
        .filter(|p| p.preview_only_stale() || (all && (p.preview_only.is_some() || p.format.eq_ignore_ascii_case("orf"))))
        .map(|p| p.id)
        .collect();
    super::reload::plan_ids(s, ids)
}

/// The session's file probe, to [`probe`] with on a worker thread; `None` when the session has no
/// file access (a demo or browser session), where nothing can be rechecked.
pub fn file_probe(s: &Session) -> Option<FileProbe> {
    s.media.file_probe.clone()
}

/// Probe each listed photo's file as Reload would. Doesn't touch the session.
pub fn probe(probe: &FileProbe, stale: Vec<Stale>) -> Vec<(Stale, std::result::Result<ProbeInfo, String>)> {
    let paths: Vec<String> = stale.iter().map(|x| x.path.clone()).collect();
    stale.into_iter().zip(crate::import::probe_with(probe, &paths)).collect()
}

/// What a recheck changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rechecked {
    /// Photos brought up to date (virtual copies included).
    pub updated: Vec<PhotoId>,
    /// Of those, the ones that develop from their raw data now.
    pub cleared: Vec<PhotoId>,
}

impl Rechecked {
    pub fn to_json(&self) -> Value {
        json!({"updated": self.updated.iter().map(|i| i.0).collect::<Vec<_>>(), "cleared": self.cleared.iter().map(|i| i.0).collect::<Vec<_>>()})
    }
}

/// Apply [`probe`]'s results: each photo gets the ops Reload would give it, all in one batch that is
/// journaled but isn't an undo step. A file that can't be read (missing, offline) changes nothing,
/// so it is tried again next time. Relink, removal and source history invalidate pending reads;
/// a completed older read leaves newer pending reads usable.
pub fn apply(s: &mut Session, results: Vec<(Stale, std::result::Result<ProbeInfo, String>)>) -> Result<Rechecked> {
    let mut ops = Vec::new();
    let mut out = Rechecked::default();
    for (mut item, info) in results {
        let Ok(info) = info else { continue };
        let Some(id) = item.accept(s) else { continue };
        let first = out.updated.len();
        let selected: std::collections::HashSet<_> = item.metadata_targets.into_iter().collect();
        crate::cmd::convert::reload_ops(&s.catalog, id, &info, &mut ops, &mut out.updated, item.source_targets, |id| selected.contains(&id));
        if info.preview_only.is_none() {
            out.cleared.extend(out.updated[first..].iter().copied().filter(|id| s.catalog.photo(*id).is_some_and(|p| p.preview_only.is_some())));
        }
    }
    if !ops.is_empty() {
        s.apply_system(Op::Batch { ops })?;
    }
    out.cleared.retain(|id| s.catalog.photo(*id).is_some_and(|p| p.develops_raw()));
    for id in &out.updated {
        s.media.forget(*id);
    }
    Ok(out)
}

fn recheck(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.recheckPreviewOnly";
    let all = p.get("all").and_then(Value::as_bool).unwrap_or(false);
    // without file access every probe "succeeds" with nothing known, which would clear every flag
    let fp = file_probe(s).ok_or_else(|| bad(C, "this session has no file access"))?;
    let listed = stale(s, all);
    let n = listed.len();
    let mut r = apply(s, probe(&fp, listed))?.to_json();
    r["checked"] = json!(n);
    Ok(r)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "library.recheckPreviewOnly",
        "Recheck Preview-Only Raws",
        [],
        None,
        "{all?: bool} — probe again preview-only raws and ORFs whose support another app version checked (the app does this in the background when a library opens; `all` rechecks every preview-only raw and ORF), and bring each up to date as Reload from Disk would; not an undo step, develop settings and history are kept; files that can't be read are left for next time → {checked, updated: [ids], cleared: [ids] (develop from their raw data now)}",
        always,
        recheck
    )]
}
