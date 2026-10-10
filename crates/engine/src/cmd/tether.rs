//! Tethered capture, first stage: studio capture (LRC-LIB-TETHER; model in `dac-tether`). A
//! watched folder written by the camera's app or a vendor tool; every new shot is imported with
//! the session's develop preset, metadata preset, keywords, naming and collection, and becomes the
//! selection so the loupe shows it. The app calls `tether.scan` every couple of seconds while a
//! session is active.

use std::path::{Path, PathBuf};

use dac_catalog::PhotoId;
use dac_tether::StudioSession;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session, view::Selection};

fn lib_dir(s: &Session, c: &str) -> Result<PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()).ok_or_else(|| bad(c, "studio capture needs a library on disk"))
}

fn load(s: &Session, c: &str) -> Result<(PathBuf, Option<StudioSession>)> {
    let dir = lib_dir(s, c)?;
    let ses = StudioSession::load(&dir).map_err(|e| bad(c, e))?;
    Ok((dir, ses))
}

fn opt_str(p: &Value, key: &str) -> Option<Option<String>> {
    let v = p.get(key)?;
    Some(v.as_str().map(str::trim).filter(|x| !x.is_empty()).map(str::to_string))
}

/// Apply the settings in `p` to a session, checking presets exist.
fn configure(s: &Session, ses: &mut StudioSession, p: &Value, c: &str) -> Result<()> {
    if let Some(n) = str_param(p, "session").map(str::trim).filter(|n| !n.is_empty()) {
        ses.name = dac_tether::safe_session_name(n);
    }
    if let Some(c2) = p.get("copy").and_then(Value::as_bool) {
        ses.copy = c2;
    }
    if let Some(x) = opt_str(p, "preset") {
        if let Some(id) = &x
            && !s.presets.iter().any(|q| &q.id == id)
        {
            return Err(bad(c, format!("unknown develop preset `{id}`")));
        }
        ses.preset = x;
    }
    if let Some(x) = opt_str(p, "metadataPreset") {
        if let Some(n) = &x
            && !s.metadata_presets.iter().any(|m| m.name.eq_ignore_ascii_case(n))
        {
            return Err(bad(c, format!("unknown metadata preset `{n}`")));
        }
        ses.metadata_preset = x;
    }
    if let Some(k) = p.get("keywords") {
        ses.keywords = match k {
            Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect(),
            Value::String(t) => t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect(),
            _ => Vec::new(),
        };
    }
    if let Some(x) = opt_str(p, "naming") {
        ses.naming = x;
    }
    if let Some(x) = opt_str(p, "collection") {
        ses.collection = x;
    }
    Ok(())
}

fn start(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "tether.start";
    let (dir, old) = load(s, C)?;
    let folder =
        str_param(p, "folder").map(str::trim).filter(|f| !f.is_empty()).map(str::to_string).or_else(|| old.as_ref().map(|o| o.folder.clone()));
    let folder = folder.ok_or_else(|| bad(C, "missing `folder` (the folder the camera software writes to)"))?;
    if !Path::new(&folder).is_dir() {
        return Err(bad(C, format!("`{folder}` is not a folder")));
    }
    // a new session unless this continues the stopped one (same folder, no new name)
    let named = str_param(p, "session").map(dac_tether::safe_session_name);
    let resume = old.as_ref().is_some_and(|o| o.folder == folder && named.as_ref().is_none_or(|n| *n == o.name));
    let mut ses = match old {
        Some(o) if resume => o,
        _ => {
            let mut n = StudioSession::new(str_param(p, "session").unwrap_or("Studio Session"), &folder);
            n.collection = Some(n.name.clone());
            n
        }
    };
    configure(s, &mut ses, p, C)?;
    ses.active = true;
    if !resume && bool_or(p, "skipExisting", true) {
        let listing = dac_tether::list_folder(Path::new(&folder)).map_err(|e| bad(C, e))?;
        ses.skip_existing(&listing);
    }
    StudioSession::save(Some(&ses), &dir).map_err(|e| bad(C, e))?;
    Ok(ses.to_json())
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "tether.settings";
    let (dir, ses) = load(s, C)?;
    let mut ses = ses.ok_or_else(|| bad(C, "no studio session (tether.start)"))?;
    configure(s, &mut ses, p, C)?;
    StudioSession::save(Some(&ses), &dir).map_err(|e| bad(C, e))?;
    Ok(ses.to_json())
}

fn stop(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "tether.stop";
    let (dir, ses) = load(s, C)?;
    let Some(mut ses) = ses else { return Ok(Value::Null) };
    if bool_or(p, "end", false) {
        StudioSession::save(None, &dir).map_err(|e| bad(C, e))?;
        return Ok(Value::Null);
    }
    ses.active = false;
    StudioSession::save(Some(&ses), &dir).map_err(|e| bad(C, e))?;
    Ok(ses.to_json())
}

fn scan(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "tether.scan";
    let (dir, ses) = load(s, C)?;
    let Some(mut ses) = ses.filter(|x| x.active) else { return Ok(json!({"active": false, "imported": []})) };
    let listing = match p.get("listing").and_then(Value::as_array) {
        Some(l) => l.iter().filter_map(|e| Some((e.get(0)?.as_str()?.to_string(), e.get(1)?.as_u64()?))).collect(),
        None => match dac_tether::list_folder(Path::new(&ses.folder)) {
            Ok(l) => l,
            // the card or share went away: say so, keep the session
            Err(e) => return Ok(json!({"active": true, "imported": [], "error": e})),
        },
    };
    let fresh = ses.fresh(&listing);
    let mut imported: Vec<u64> = Vec::new();
    let mut error = None;
    if !fresh.is_empty() {
        let originals = s.library.as_ref().map(|l| l.originals_dir());
        let params = ses.import_params(&fresh, originals.as_deref());
        match s.execute("library.import", &params) {
            Ok(r) => {
                imported = ["imported", "restored"].iter().flat_map(|k| r[*k].as_array().into_iter().flatten().filter_map(Value::as_u64)).collect();
            }
            Err(e) => error = Some(e.to_string()),
        }
        ses.imported(&imported);
        if let Some(n) = ses.newest.filter(|_| !imported.is_empty()).map(PhotoId)
            && s.catalog.photo(n).is_some()
        {
            // the newest shot is what the loupe shows
            s.selection = Selection::single(n);
        }
    }
    // the session file holds what was taken: saved every scan that saw something change
    StudioSession::save(Some(&ses), &dir).map_err(|e| bad(C, e))?;
    Ok(json!({"active": true, "imported": imported, "newest": ses.newest, "shots": ses.shots, "error": error}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "tether.start",
            "Start Tethered Capture…",
            ["File", "Tethered Capture"],
            None,
            "{folder: the folder the camera software writes to, session?: name (default \"Studio Session\"; a new name starts a new session), copy?: bool (copy into Originals/<session>), preset?: develop preset id, metadataPreset?: name, keywords?: [..] | \"a, b\", naming?: template with {session} and rename tokens ({seq:4}, …; copy only), collection?: album name (default: the session name), skipExisting?: bool (default true)} → the session",
            always,
            start
        ),
        cmd!(
            "tether.settings",
            "Tethered Capture Settings",
            [],
            None,
            "{session?, copy?, preset?, metadataPreset?, keywords?, naming?, collection?} (null clears) → the session",
            always,
            set
        ),
        cmd!(
            "tether.stop",
            "Stop Tethered Capture",
            ["File", "Tethered Capture"],
            None,
            "{end?: bool (forget the session)} → the session (inactive) or null",
            always,
            stop
        ),
        cmd!(query "tether.status", "Tethered Capture Status", [], None,
            "{} → the session {name, folder, copy, preset, metadataPreset, keywords, naming, collection, nextSeq, active, newest, shots} or null",
            always, |s, _| Ok(load(s, "tether.status")?.1.map(|x| x.to_json()).unwrap_or(Value::Null))),
        cmd!(
            "tether.scan",
            "Import New Shots",
            [],
            None,
            "{listing?: [[path, size]]} — imports the watched folder's new shots (a file whose size held still since the last scan); the newest becomes the selection → {active, imported: ids, newest, shots, error?}",
            always,
            scan
        ),
    ]
}
