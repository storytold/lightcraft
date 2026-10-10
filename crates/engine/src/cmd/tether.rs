//! Tethered capture, first stage: studio capture (LRC-LIB-TETHER; model in `dac-tether`). A
//! watched folder written by the camera's app or a vendor tool; every new shot is imported with
//! the session's develop preset, metadata preset, keywords, naming and collection, and becomes the
//! selection so the loupe shows it. The app calls `tether.scan` every couple of seconds while a
//! session is active.
//!
//! Native tethering (`tether.connect`, `tether.capture`, `tether.camera*`, `tether.liveView`): a
//! PTP camera over USB or PTP/IP (`dac_tether::link`). Its shots download into the session's
//! watched folder, so the scan imports them like any other; connecting without a session starts
//! one in `<library>/Tethered/<session>`.

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
    if let Some(b) = p.get("sameAsPrevious").and_then(Value::as_bool) {
        ses.same_as_previous = b;
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
    // a connected camera: shots taken on its body download into the watched folder first
    let mut camera_error = None;
    if dac_tether::link::connected()
        && let Err(e) = dac_tether::link::poll(Path::new(&ses.folder))
    {
        camera_error = Some(format!("camera: {e}"));
    }
    let listing = match p.get("listing").and_then(Value::as_array) {
        Some(l) => l.iter().filter_map(|e| Some((e.get(0)?.as_str()?.to_string(), e.get(1)?.as_u64()?))).collect(),
        None => match dac_tether::list_folder(Path::new(&ses.folder)) {
            Ok(l) => l,
            // the card or share went away: say so, keep the session
            Err(e) => return Ok(json!({"active": true, "imported": [], "error": e})),
        },
    };
    let previous = ses.newest.map(PhotoId);
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
        if ses.same_as_previous
            && let Some(prev) = previous
        {
            same_as_previous(s, prev, &imported);
        }
        if let Some(n) = ses.newest.filter(|_| !imported.is_empty()).map(PhotoId)
            && s.catalog.photo(n).is_some()
        {
            // the newest shot is what the loupe shows
            s.selection = Selection::single(n);
        }
    }
    // the session file holds what was taken: saved every scan that saw something change
    StudioSession::save(Some(&ses), &dir).map_err(|e| bad(C, e))?;
    let error = error.or(camera_error);
    Ok(json!({"active": true, "imported": imported, "newest": ses.newest, "shots": ses.shots, "error": error}))
}

/// Give the new shots `ids` the develop settings (the copy groups) of shot `prev`.
fn same_as_previous(s: &mut Session, prev: PhotoId, ids: &[u64]) {
    let Some(from) = s.develop_of(prev).filter(|_| s.catalog.photo(prev).is_some_and(|p| !p.deleted)) else { return };
    let partial = dac_develop::extract_groups(&from, &s.copy_groups);
    let ops: Vec<_> = ids
        .iter()
        .map(|i| PhotoId(*i))
        .filter(|id| *id != prev)
        .filter_map(|id| s.develop_of(id).map(|d| (id, d)))
        .filter_map(|(id, d)| s.develop_op(id, dac_develop::apply_partial(&d, &partial, 1.0), "Same as Previous"))
        .collect();
    if !ops.is_empty()
        && let Err(e) = s.commit("Same as Previous", dac_catalog::Op::Batch { ops })
    {
        log::warn!("same as previous: {e}");
    }
}

/// The folder captures download into: the active session's, or a new session's in
/// `<library>/Tethered/<session>`.
fn capture_folder(s: &mut Session, p: &Value, c: &str) -> Result<PathBuf> {
    if let (_, Some(ses)) = load(s, c)?
        && ses.active
    {
        return Ok(PathBuf::from(ses.folder));
    }
    let dir = lib_dir(s, c)?;
    let name = dac_tether::safe_session_name(str_param(p, "session").unwrap_or("Tethered Session"));
    let folder = dir.join("Tethered").join(&name);
    std::fs::create_dir_all(&folder).map_err(|e| bad(c, format!("{}: {e}", folder.display())))?;
    let mut sp = json!({"folder": folder.to_string_lossy(), "session": name});
    for k in ["preset", "metadataPreset", "keywords", "collection", "sameAsPrevious"] {
        if let Some(v) = p.get(k) {
            sp[k] = v.clone();
        }
    }
    s.execute("tether.start", &sp)?;
    Ok(folder)
}

/// A simulated shot: a small JPEG, different for each shot.
fn sim_shot(n: u32) -> Vec<u8> {
    let (w, h) = (96usize, 64usize);
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 2) as u8, (i / w * 3) as u8, (n.wrapping_mul(40) % 256) as u8]).collect();
    let img = dac_codecs::EncodeImage::new(w as u32, h as u32, 3, dac_codecs::Samples::U8(&px));
    dac_codecs::encode_jpeg(&img, 85, dac_codecs::ChromaSubsampling::S420, &dac_codecs::EncodeMeta::default()).unwrap_or_default()
}

fn connect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "tether.connect";
    let device = str_param(p, "device").map(str::trim).filter(|d| !d.is_empty()).unwrap_or("usb");
    // a library on disk first: shots need somewhere to land
    lib_dir(s, C)?;
    let make: Option<dac_tether::ptp::sim::ShotMaker> = Some(Box::new(sim_shot));
    let cam =
        dac_tether::link::connect(device, bool_or(p, "deleteFromCard", false), make).map_err(|e| bad(C, dac_tether::link::fallback_message(&e)))?;
    if let Err(e) = capture_folder(s, p, C) {
        dac_tether::link::disconnect();
        return Err(e);
    }
    Ok(cam)
}

fn capture(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "tether.capture";
    if !dac_tether::link::connected() {
        return Err(bad(C, "no camera connected (tether.connect); with studio capture, fire the camera from its own app"));
    }
    let folder = capture_folder(s, p, C)?;
    let files = dac_tether::link::capture(&folder).map_err(|e| bad(C, e.to_string()))?;
    // downloaded files are complete: two scans import them now
    s.execute("tether.scan", &json!({}))?;
    let r = s.execute("tether.scan", &json!({}))?;
    Ok(json!({"files": files, "imported": r["imported"], "newest": r["newest"], "error": r["error"]}))
}

fn set_camera(_s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "tether.camera.set";
    if let Some(b) = p.get("deleteFromCard").and_then(Value::as_bool) {
        dac_tether::link::set_delete_from_card(b);
    }
    let Some(name) = str_param(p, "setting") else { return Ok(dac_tether::link::status()) };
    let value = match p.get("value") {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(bad(C, "missing `value`")),
    };
    dac_tether::link::set(name, &value).map_err(|e| bad(C, e.to_string()))
}

/// One live view frame, decoded (RGBA, fitted within `max_edge`).
pub fn live_view_rgba(max_edge: u32) -> std::result::Result<dac_raster::Rgba8, String> {
    let jpeg = dac_tether::link::live_view_frame().map_err(|e| e.to_string())?;
    dac_codecs::decode_thumbnail(&jpeg, max_edge).map(|t| t.image).map_err(|e| e.to_string())
}

fn live_view(_s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "tether.liveView";
    if bool_or(p, "stop", false) {
        dac_tether::link::stop_live_view();
        return Ok(json!({"live": false}));
    }
    let img = live_view_rgba(1024).map_err(|e| bad(C, e))?;
    Ok(json!({"live": true, "width": img.width, "height": img.height}))
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
            "{session?, copy?, preset?, metadataPreset?, keywords?, naming?, collection?, sameAsPrevious?: bool} (null clears) → the session",
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
            "tether.connect",
            "Connect Camera…",
            ["File", "Tethered Capture"],
            None,
            "{device?: \"usb\" (first PTP camera, default) | \"usb:<bus>:<addr>\" | \"ptpip:<host>[:port]\" (Wi-Fi) | \"sim\" | \"sim-nikon\" (simulated), deleteFromCard?: bool, session?, preset?, metadataPreset?, keywords?, collection?, sameAsPrevious?} — shots download into the studio session's folder (a new session in <library>/Tethered/<session> when none is active) → the camera {name, vendor, link, canCapture, liveView, readouts}. A camera that can't be tethered natively is an error that says to use studio capture",
            always,
            connect
        ),
        cmd!("tether.disconnect", "Disconnect Camera", ["File", "Tethered Capture"], None, "{} → null", always, |_s, _p| {
            dac_tether::link::disconnect();
            Ok(Value::Null)
        }),
        cmd!(query "tether.cameras", "Tethered Cameras", [], None,
            "{simulated?: bool} → {cameras: [{id, name, vendor, kind}], error?}",
            always, |_s, p| Ok(dac_tether::link::cameras(bool_or(p, "simulated", false)))),
        cmd!(query "tether.camera", "Tethered Camera Status", [], None,
            "{} → the connected camera {id, name, vendor, link, canCapture, liveView, deleteFromCard, downloaded, readouts: [{name: shutter|aperture|iso|wb|ev|battery, value, label, writable, choices}], note} or null",
            always, |_s, _p| Ok(dac_tether::link::status())),
        cmd!(
            "tether.camera.set",
            "Set Camera Setting",
            [],
            None,
            "{setting: shutter|aperture|iso|wb|ev, value: a label from the readout's choices (\"1/250\", \"f/8\", \"ISO 400\", \"Daylight\", \"+0.3 EV\") or the raw PTP value} | {deleteFromCard: bool} → the camera",
            always,
            set_camera
        ),
        cmd!(
            "tether.capture",
            "Capture",
            ["File", "Tethered Capture"],
            None,
            "{} — fires the connected camera, downloads the shot into the session folder and imports it (the newest becomes the selection) → {files, imported, newest, error?}",
            always,
            capture
        ),
        cmd!(
            "tether.liveView",
            "Live View Frame",
            [],
            None,
            "{stop?: bool} — grabs one live view frame where the camera supports it → {live, width, height}",
            always,
            live_view
        ),
        cmd!(
            "tether.simShoot",
            "Press Simulated Shutter",
            [],
            None,
            "{} — the simulated camera (device \"sim\") takes a shot on its own, as if its shutter button was pressed → {handle} or null",
            always,
            |_s, _p| Ok(dac_tether::link::sim_shoot().map(|h| json!({"handle": h})).unwrap_or(Value::Null))
        ),
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

#[cfg(test)]
#[path = "tether_tests.rs"]
mod tests;
