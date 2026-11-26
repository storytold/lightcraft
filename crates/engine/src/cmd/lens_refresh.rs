//! Lens profiles looked up again when the lens database appears or changes. A photo's lens data is
//! stored when it is imported ([`Photo::embedded_lens`]), so photos imported before the database was there say "no
//! profile" until something reads their file again. When the database arrives (downloaded on the user's request) or is updated,
//! and when "Enable Lens Corrections" is ticked on a photo without a profile, every library raw that has no profile
//! but whose lens the database knows by name is probed again and gets what Reload from Disk would give it: the
//! profile, and the correction switched on where its settings are still what import gave it (an edited photo keeps
//! its settings).
//!
//! Split like [`super::recheck`] so the app probes on a worker thread: [`missing`] lists what to probe (no
//! file-system calls), [`probe`] reads the files without the session, [`apply`] applies the results as LightCraft's
//! own bookkeeping (journaled, not an undo step). Agents and tests use `library.refreshLensProfiles`.

use lightcraft_catalog::{Catalog, Op, Photo, PhotoId};
use lightcraft_develop::LensSource;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::media::{FileProbe, ProbeInfo};
use crate::{Result, Session};

/// A photo to probe again, and its file when listed (a result for a photo moved meanwhile is dropped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub id: PhotoId,
    pub path: String,
}

/// Library raws with a file, no lens profile and a lens the database `db` knows by name (`ids`: only these; virtual
/// copies follow the photo they were copied from). With `again` (the database's files were replaced, or an agent
/// asked for every photo), also the raws whose profile came from the database (the new data may give another one, or
/// none) and the raws whose file named no lens when they were imported (a newer LightCraft may read their lens).
/// No file-system calls.
pub fn missing(cat: &Catalog, db: &lightcraft_lensdb::LensDb, ids: Option<&[PhotoId]>, again: bool) -> Vec<Missing> {
    // a photo and its virtual copies list the photo once, in library order
    let mut seen = std::collections::HashSet::new();
    let from_database = |p: &Photo| p.embedded_lens.is_some_and(|l| l.source == LensSource::Database);
    cat.photos()
        .filter(|p| ids.is_none_or(|ids| ids.contains(&p.id) || p.copy_of.is_some_and(|c| ids.contains(&c))))
        .map(|p| p.copy_of.and_then(|c| cat.photo(c)).map_or(&**p, |orig| &**orig))
        .filter(|p| p.develops_raw())
        .filter(|p| {
            let lens = p.meta.lens.trim();
            match &p.embedded_lens {
                None if lens.is_empty() => again,
                None => crate::lens_profiles::profile_name(db, &p.meta.camera, lens).is_some(),
                Some(_) => again && from_database(p),
            }
        })
        .filter_map(|p| crate::cmd::missing::checked_path(p).map(|path| Missing { id: p.id, path: path.to_string() }))
        .filter(|m| seen.insert(m.id))
        .collect()
}

/// Probe each listed photo's file as Reload would. Doesn't touch the session.
pub fn probe(probe: &FileProbe, list: Vec<Missing>) -> Vec<(Missing, std::result::Result<ProbeInfo, String>)> {
    let paths: Vec<String> = list.iter().map(|x| x.path.clone()).collect();
    list.into_iter().zip(crate::import::probe_with(probe, &paths)).collect()
}

/// Apply [`probe`]'s results: each photo and its virtual copies get the lens data their file gives now, in one batch
/// that is journaled but isn't an undo step: a profile found (or a different one from a replaced database), a
/// database profile the database no longer gives removed, and the lens's name where the catalog had none. Returns
/// the photos that gained or changed a profile.
pub fn apply(s: &mut Session, results: Vec<(Missing, std::result::Result<ProbeInfo, String>)>) -> Result<Vec<PhotoId>> {
    let mut ops = Vec::new();
    let mut found = Vec::new();
    let mut changed = Vec::new();
    // each photo's virtual copies, gathered once for the whole batch (the catalog doesn't change until it's applied)
    let mut copies: std::collections::HashMap<PhotoId, Vec<PhotoId>> = std::collections::HashMap::new();
    for p in s.catalog.photos() {
        if let Some(original) = p.copy_of {
            copies.entry(original).or_default().push(p.id);
        }
    }
    for (item, info) in results {
        let Ok(info) = info else { continue };
        // the photo moved or left the library while its file was read: the result is stale, for its copies too
        if s.catalog.photo(item.id).and_then(|p| crate::cmd::missing::checked_path(p)) != Some(item.path.as_str()) {
            continue;
        }
        let family = std::iter::once(item.id).chain(copies.get(&item.id).into_iter().flatten().copied());
        for id in family {
            let Some(p) = s.catalog.photo(id) else { continue };
            // nothing found for a photo without a database profile: a file's own correction is Reload's business
            let from_database = p.embedded_lens.is_some_and(|l| l.source == LensSource::Database);
            if info.embedded_lens.is_some() || from_database {
                let mine = crate::cmd::convert::lens_ops(id, p, &info);
                if !mine.is_empty() {
                    ops.extend(mine);
                    changed.push(id);
                    if info.embedded_lens.is_some() {
                        found.push(id);
                    }
                }
            }
            let named = info.meta.lens.trim();
            if p.meta.lens.trim().is_empty() && !named.is_empty() {
                let mut meta = p.meta.clone();
                meta.lens = named.to_string();
                ops.push(Op::SetMeta { id, meta: Box::new(meta) });
            }
        }
    }
    if !ops.is_empty() {
        s.apply_system(Op::Batch { ops })?;
    }
    for id in &changed {
        s.media.forget(*id);
    }
    Ok(found)
}

/// Why a photo has no lens profile, in words for the Optics panel: `None` when it has one.
pub fn no_profile_reason(p: &Photo) -> Option<String> {
    if p.embedded_lens.is_some() {
        return None;
    }
    let lens = p.meta.lens.trim();
    Some(if lens.is_empty() { "This file names no lens.".to_string() } else { format!("No lens profile found for {lens}.") })
}

fn refresh(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.refreshLensProfiles";
    let fp = super::recheck::file_probe(s).ok_or_else(|| bad(C, "this session has no file access"))?;
    let ids: Option<Vec<PhotoId>> = match p.get("ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a.iter().map(|v| v.as_u64().map(PhotoId).ok_or_else(|| bad(C, "ids are photo ids"))).collect::<Result<_>>()?),
        Some(_) => return Err(bad(C, "ids is a list of photo ids")),
    };
    let again = match p.get("all") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad(C, "all is true or false")),
    };
    let Some(db) = crate::lens_profiles::database() else {
        return Ok(json!({"checked": 0, "found": [], "database": false}));
    };
    let listed = missing(&s.catalog, &db, ids.as_deref(), again);
    let n = listed.len();
    let found = apply(s, probe(&fp, listed))?;
    Ok(json!({"checked": n, "found": found.iter().map(|i| i.0).collect::<Vec<_>>(), "database": true}))
}

fn download(s: &mut Session, _: &Value) -> Result<Value> {
    const C: &str = "lens.downloadDatabase";
    let dir = crate::lensfun_download::default_dir().ok_or_else(|| bad(C, "this session has no data folder"))?;
    start_download(s, &dir, crate::lensfun_download::mirrors(), crate::lensfun_download::FILES).map_err(|e| bad(C, e))?;
    Ok(json!({"started": true, "dir": dir.to_string_lossy()}))
}

/// Fetch the database into `dir` and select it, so this session reads it once it is in place (a session that
/// selected no database, e.g. a bare headless one, would otherwise download it and go on without it; the user's
/// `LIGHTCRAFT_LENSFUN_DIR` still wins). Until the download is whole the folder has no marker and isn't read.
pub fn start_download(
    s: &mut Session,
    dir: &std::path::Path,
    mirrors: Vec<String>,
    files: &'static [crate::lensfun_download::Pin],
) -> std::result::Result<(), String> {
    let activity = s.activity.clone();
    s.lensfun.start(dir, mirrors, files, &activity)?;
    if crate::lens_profiles::host_dir().as_deref() != Some(dir) {
        crate::lens_profiles::use_dir(Some(dir.to_path_buf()));
    }
    Ok(())
}

fn status(s: &mut Session, _: &Value) -> Result<Value> {
    use crate::lensfun_download::State;
    let state = match s.lensfun.state() {
        State::Idle => json!({"state": "idle"}),
        State::Running { bytes, total } => json!({"state": "running", "bytes": bytes, "total": total}),
        State::Done { files } => json!({"state": "done", "files": files}),
        State::Failed(e) => json!({"state": "failed", "error": e}),
        State::Cancelled => json!({"state": "cancelled"}),
    };
    let dir = crate::lens_profiles::dir();
    let lenses = crate::lens_profiles::database().map_or(0, |db| db.lenses.len());
    Ok(json!({"download": state, "dir": dir.map(|d| d.to_string_lossy().to_string()), "lenses": lenses}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "library.refreshLensProfiles",
            "Look Up Lens Profiles Again",
            [],
            None,
            "{ids?: [photo ids], all?: bool} — library raws without a lens profile whose lens the lens database knows are read again and get their profile (the correction is switched on where the photo's settings are still as imported); with all, also raws whose profile came from the database (it may give another, or none) and raws whose file named no lens at import; the app does this by itself when the database arrives (all: when its files were replaced) and when Enable Lens Corrections is ticked; not an undo step → {checked, found: [ids], database: bool}",
            always,
            refresh
        ),
        cmd!(
            "lens.downloadDatabase",
            "Download the Lens Database",
            [],
            None,
            "{} — fetch lensfun's lens database (CC BY-SA 3.0, pinned to one commit, checked file by file) into the app's data folder in the background, with an activity row, and read lens profiles from it once it is in place (unless LIGHTCRAFT_LENSFUN_DIR names another folder); the desktop app offers it on its first launch and in Settings. LIGHTCRAFT_LENSFUN_URL lists other places to fetch the same files from → {started, dir}",
            always,
            download
        ),
        cmd!(
            "lens.databaseStatus",
            "Lens Database Status",
            [],
            None,
            "{} → {download: {state: idle|running|done|failed|cancelled, …}, dir, lenses (in the database in use)}",
            always,
            status
        ),
    ]
}
