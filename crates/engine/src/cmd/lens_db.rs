//! Settings ▸ Lens profiles as commands: the lens database's status, its folder, downloading lensfun's database,
//! removing it, and looking photos' lenses up again after a change ([`crate::lens_download`]).

use std::path::PathBuf;

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};

fn status(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(s.lens_db.status())
}

fn set_dir(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "lens.db.setDir";
    let dir = match p.get("dir") {
        None => return Err(bad(C, "missing dir (a folder of lensfun-format XML files, or null for none)")),
        Some(Value::Null) => None,
        Some(Value::String(d)) if d.trim().is_empty() => None,
        Some(Value::String(d)) => Some(PathBuf::from(d)),
        Some(_) => return Err(bad(C, "dir must be a folder path or null")),
    };
    if let Some(d) = &dir {
        if !d.is_dir() {
            return Err(bad(C, format!("{} is not a folder", d.display())));
        }
        if crate::lens_profiles::load(d).is_none() {
            return Err(bad(C, format!("{} holds no lens profiles: choose the folder with lensfun's .xml files (its data/db)", d.display())));
        }
    }
    s.lens_db.set_dir(dir).map_err(|e| bad(C, e))?;
    refresh_active(s);
    Ok(s.lens_db.status())
}

fn download(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "lens.db.download";
    let activity = s.activity.clone();
    s.lens_db.start_download(&activity).map_err(|e| bad(C, e))?;
    // a one-shot caller (the CLI) waits for the outcome instead of polling
    if p.get("wait").and_then(Value::as_bool) == Some(true) {
        while s.lens_db.running() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if let Some(crate::lens_download::State::Failed(e)) = s.lens_db.download_state() {
            return Err(bad(C, e));
        }
        refresh_active(s);
    }
    Ok(s.lens_db.status())
}

fn remove(s: &mut Session, _: &Value) -> Result<Value> {
    let deleted = s.lens_db.remove().map_err(|e| bad("lens.db.remove", e))?;
    refresh_active(s);
    let mut v = s.lens_db.status();
    v["deleted"] = json!(deleted);
    Ok(v)
}

/// The active photo picks the change up at once (the others when they are next opened, [`refresh`]).
fn refresh_active(s: &mut Session) {
    if let Some(id) = s.selection.active {
        let _ = s.refresh_lens_profiles(&[id]);
    }
}

fn refresh(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = s.targets(p)?;
    let changed = s.refresh_lens_profiles(&ids)?;
    Ok(json!({"changed": changed.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// A lens given in params `p` (`lens: {maker?, model, focal?, aperture?}` or `lens: "model"`); `None` without one.
pub(crate) fn given_lens(c: &str, p: &Value) -> Result<Option<crate::lens_profiles::GivenLens>> {
    use crate::lens_profiles::GivenLens;
    let num = |o: &serde_json::Map<String, Value>, k: &str| -> Result<Option<f64>> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_f64().map(Some).ok_or_else(|| bad(c, format!("lens.{k} must be a number"))),
        }
    };
    let text = |o: &serde_json::Map<String, Value>, k: &str| -> Result<Option<String>> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(t)) => Ok(Some(t.clone())),
            Some(_) => Err(bad(c, format!("lens.{k} must be a string"))),
        }
    };
    let g = match p.get("lens") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(m)) => GivenLens { model: m.clone(), ..Default::default() },
        Some(Value::Object(o)) => GivenLens {
            maker: text(o, "maker")?,
            model: text(o, "model")?.ok_or_else(|| bad(c, "lens.model is missing"))?,
            focal: num(o, "focal")?,
            aperture: num(o, "aperture")?,
        },
        Some(_) => return Err(bad(c, "lens must be {maker?, model, focal?, aperture?} or a model name")),
    };
    g.validate().map_err(|e| bad(c, e))?;
    Ok(Some(g))
}

/// What Develop ▸ Optics says about the photo's lens, in the order corrections apply: `embedded` (the file's own
/// correction), `chosen` (the `lens` given, found in the database), `found` (matched by the metadata's lens name),
/// or why there is none: `noDatabase`, `notRaw`, `noLensName`, `notFound` (with the lens name).
fn photo_status(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "lens.db.photo";
    let given = given_lens(C, p)?;
    let id = s.targets(p)?.first().copied().or(s.selection.active).ok_or_else(|| bad(C, "no photo"))?;
    let ph = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?;
    use crate::lens_profiles::Match;
    Ok(match s.lens_match_with(ph, given.as_ref()) {
        None => json!({"state": "embedded"}),
        Some(Match::NoDatabase) => json!({"state": "noDatabase"}),
        Some(Match::NoLensName) => json!({"state": "noLensName"}),
        Some(Match::NotRaw) => json!({"state": "notRaw"}),
        Some(Match::NotFound(l)) => json!({"state": "notFound", "lens": l}),
        Some(Match::Found(l)) => json!({"state": "found", "lens": l}),
        Some(Match::Chosen(l)) => json!({"state": "chosen", "lens": l}),
    })
}

/// Most lenses one `lens.db.list` returns (`limit`); the default is [`LIST_DEFAULT`].
pub const LIST_MAX: usize = 500;
/// Lenses `lens.db.list` returns without a `limit`.
pub const LIST_DEFAULT: usize = 100;

/// The database's lenses for a lens picker, filtered by camera mount, crop factor and words.
fn list(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "lens.db.list";
    let _ = s;
    let text = |k: &str| -> Result<Option<String>> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(t)) if t.chars().count() <= crate::lens_profiles::MAX_NAME => Ok(Some(t.clone())),
            Some(Value::String(_)) => Err(bad(C, format!("{k} is at most {} characters", crate::lens_profiles::MAX_NAME))),
            Some(_) => Err(bad(C, format!("{k} must be a string"))),
        }
    };
    let count = |k: &str, default: usize| -> Result<usize> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => v.as_u64().map(|n| usize::try_from(n).unwrap_or(usize::MAX)).ok_or_else(|| bad(C, format!("{k} must be a whole number ≥ 0"))),
        }
    };
    let crop = match p.get("crop") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_f64().filter(|c| c.is_finite() && *c > 0.0 && *c <= 20.0).ok_or_else(|| bad(C, "crop must be a crop factor (0–20)"))?),
    };
    let filter = crate::lens_profiles::ListFilter { mount: text("mount")?, crop, query: text("query")? };
    let limit = count("limit", LIST_DEFAULT)?.min(LIST_MAX);
    let offset = count("offset", 0)?;
    let Some(db) = crate::lens_profiles::database() else {
        return Ok(json!({"total": 0, "lenses": [], "database": false}));
    };
    let (total, lenses) = crate::lens_profiles::list(&db, &filter, offset, limit);
    let more = offset.saturating_add(lenses.len()) < total;
    Ok(json!({"total": total, "offset": offset, "lenses": lenses, "more": more, "database": true}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "lens.db.status", "Lens Database Status", [], None, "{} → {dir, origin: env | folder | null, env, configured, downloaded, lenses, cameras, updated: YYYY-MM-DD, download: {state: running | done | failed | cancelled, bytes, total, lenses, error}, canDownload, attribution} — the lensfun-format database lens corrections fall back to (the environment variable `env` overrides the Settings folder)", always, status),
        cmd!(query "lens.db.setDir", "Set Lens Database Folder", [], None, "{dir: folder of lensfun-format .xml files | null} — used at once and remembered in the app settings → status", always, set_dir),
        cmd!(query "lens.db.download", "Download Lens Database", [], None, "{wait?} — fetch lensfun's database (CC-BY-SA 3.0) into the settings folder in the background (activity stack, cancellable) and use it; again to update → status (poll lens.db.status; `wait: true` blocks until it is in, for one-shot callers)", always, download),
        cmd!(query "lens.db.downloadCancel", "Cancel Lens Database Download", [], None, "{} → {cancelled}", always, |s, _| Ok(json!({"cancelled": s.lens_db.cancel_download()}))),
        cmd!(query "lens.db.remove", "Remove Lens Database", [], None, "{} — stop using the database (a downloaded one is deleted) → status + {deleted}", always, remove),
        cmd!(
            "lens.db.refresh",
            "Look Up Lens Profiles Again",
            [],
            None,
            "{ids?} — re-read the targets' lens data after the lens database changed → {changed: [ids]}",
            always,
            refresh
        ),
        cmd!(query "lens.db.photo", "Lens Profile for Photo", [], None, "{id?, lens?: {maker?, model, focal?, aperture?} | model} → {state: embedded | chosen | found | noDatabase | notRaw | noLensName | notFound, lens} — which correction applies, in order: the file's own, the given lens, the match by the metadata's lens name", always, photo_status),
        cmd!(query "lens.db.list", "List Lens Profiles", [], None, "{mount?, crop?, query?, limit? (default 100, at most 500), offset?} → {total, offset, more, database, lenses: [{maker, model, models, mounts, cropFactor, focal: [min, max], distortion, vignetting, tca}]} — the database's lenses for a camera's mount and crop factor (a lens calibrated on a larger frame fits a smaller one), sorted by maker and name; pass `model` back as lens.db.photo's lens", always, list),
    ]
}
