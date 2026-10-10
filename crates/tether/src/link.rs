//! The app's connected camera (one at a time): connect by device id, fire, download new shots
//! into a folder (the studio session's watched folder, so the usual scan imports them), delete
//! them from the card when asked, and the tether bar's readouts. Process-wide, behind a mutex.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::ptp::{Camera, PtpError, sim::SimCamera};

/// The id of the built-in simulated camera.
pub const SIM: &str = "sim";

struct Link {
    camera: Camera,
    id: String,
    /// Delete each shot from the card once downloaded.
    delete_from_card: bool,
    /// Shots downloaded in this connection.
    downloaded: usize,
    /// The simulated camera (to press its shutter from tests and the demo).
    sim: Option<SimCamera>,
}

static LINK: Mutex<Option<Link>> = Mutex::new(None);

fn link() -> std::sync::MutexGuard<'static, Option<Link>> {
    LINK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What to say when native tethering can't be used.
pub fn fallback_message(e: &PtpError) -> String {
    format!(
        "{e}. This camera can't be tethered natively here; use studio capture instead: let the camera's own app save into a folder and choose that folder in File ▸ Tethered Capture."
    )
}

/// The cameras that can be connected: USB PTP cameras (and the simulated one when `simulated`).
pub fn cameras(simulated: bool) -> Value {
    let mut out = Vec::new();
    let mut error = None;
    #[cfg(not(target_arch = "wasm32"))]
    match crate::ptp::usb::list() {
        Ok(list) => out.extend(list.into_iter().map(|c| json!({"id": c.id, "name": c.name, "vendor": c.vendor, "kind": "usb"}))),
        Err(e) => error = Some(e.to_string()),
    }
    if simulated {
        out.push(json!({"id": SIM, "name": "Simulated PTP Camera", "vendor": "other", "kind": "simulated"}));
    }
    json!({"cameras": out, "error": error})
}

/// Connect `device`: `sim`, `sim-nikon` (with live view), `usb`, `usb:<bus>:<addr>`,
/// `ptpip:<host>[:port]`. `make` makes simulated shots' bytes.
pub fn connect(device: &str, delete_from_card: bool, make: Option<crate::ptp::sim::ShotMaker>) -> Result<Value, PtpError> {
    disconnect();
    let device = device.trim();
    let (camera, sim) = if device == SIM || device == "sim-nikon" {
        let s = SimCamera::new(make, device == "sim-nikon");
        (Camera::open(s.transport(), None)?, Some(s))
    } else if let Some(host) = device.strip_prefix("ptpip:") {
        let t = crate::ptp::ptpip::PtpIpTransport::connect(host, *b"dac-tether-host!", "Photo library")?;
        (Camera::open(Box::new(t), None)?, None)
    } else if device == "usb" || device.starts_with("usb:") {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (t, vid) = crate::ptp::usb::open(device)?;
            (Camera::open(t, Some(vid))?, None)
        }
        #[cfg(target_arch = "wasm32")]
        return Err(PtpError::Unsupported("USB cameras in the browser".into()));
    } else {
        return Err(PtpError::Unsupported(format!("unknown device `{device}` (sim, usb, usb:<bus>:<addr>, ptpip:<host>)")));
    };
    let mut g = link();
    *g = Some(Link { camera, id: device.to_string(), delete_from_card, downloaded: 0, sim });
    drop(g);
    Ok(status())
}

pub fn disconnect() {
    if let Some(l) = link().take() {
        l.camera.close();
    }
}

pub fn connected() -> bool {
    link().is_some()
}

pub fn set_delete_from_card(on: bool) {
    if let Some(l) = link().as_mut() {
        l.delete_from_card = on;
    }
}

/// The camera, its readouts and what it can do (`null` when none is connected).
pub fn status() -> Value {
    let mut g = link();
    let Some(l) = g.as_mut() else { return Value::Null };
    let readouts = l.camera.readouts();
    json!({
        "id": l.id,
        "name": l.camera.name(),
        "vendor": l.camera.vendor,
        "link": l.camera.describe(),
        "canCapture": l.camera.can_capture(),
        "liveView": l.camera.has_live_view(),
        "deleteFromCard": l.delete_from_card,
        "downloaded": l.downloaded,
        "readouts": readouts,
        "note": l.camera.vendor.note(),
    })
}

/// Set a readout (`shutter`, `aperture`, `iso`, `wb`, `ev`) to a label or raw value.
pub fn set(name: &str, value: &str) -> Result<Value, PtpError> {
    {
        let mut g = link();
        let l = g.as_mut().ok_or_else(|| PtpError::Unsupported("no camera connected".into()))?;
        l.camera.set_readout(name, value)?;
    }
    Ok(status())
}

/// A file name in `dir` for `name` that doesn't overwrite anything.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let clean: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') || c.is_control() { '_' } else { c }).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    let clean = if clean.is_empty() { "shot".to_string() } else { clean };
    let p = dir.join(&clean);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match clean.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (clean.clone(), String::new()),
    };
    (1..10_000).map(|i| dir.join(format!("{stem}-{i}{ext}"))).find(|p| !p.exists()).unwrap_or_else(|| dir.join(format!("{stem}-x{ext}")))
}

fn download_into(l: &mut Link, handles: &[u32], dir: &Path) -> Result<Vec<String>, PtpError> {
    let mut out = Vec::new();
    for h in handles {
        let info = l.camera.object_info(*h)?;
        if Camera::is_folder(&info) {
            continue;
        }
        let bytes = l.camera.download(*h)?;
        let path = free_name(dir, &info.filename);
        // written under a hidden name, then renamed: the folder scan never sees half a file
        let tmp = dir.join(format!(".{}.part", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
        std::fs::write(&tmp, &bytes).map_err(|e| PtpError::Io(format!("{}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &path).map_err(|e| PtpError::Io(format!("{}: {e}", path.display())))?;
        if l.delete_from_card {
            l.camera.delete(*h)?;
        }
        l.downloaded = l.downloaded.saturating_add(1);
        out.push(path.to_string_lossy().to_string());
    }
    Ok(out)
}

/// Download shots announced since the last call (taken with the camera's own shutter button)
/// into `dir`. Nothing connected: nothing to do.
pub fn poll(dir: &Path) -> Result<Vec<String>, PtpError> {
    let mut g = link();
    let Some(l) = g.as_mut() else { return Ok(Vec::new()) };
    let events = l.camera.poll_events(Duration::from_millis(5))?;
    let added: Vec<u32> = events.iter().filter(|e| e.code == crate::ptp::ev::OBJECT_ADDED).filter_map(|e| e.params.first().copied()).collect();
    download_into(l, &added, dir)
}

/// Fire the shutter and download what it made into `dir`.
pub fn capture(dir: &Path) -> Result<Vec<String>, PtpError> {
    let mut g = link();
    let l = g.as_mut().ok_or_else(|| PtpError::Unsupported("no camera connected".into()))?;
    // shots announced earlier are downloaded too
    let earlier: Vec<u32> = l
        .camera
        .poll_events(Duration::from_millis(1))?
        .iter()
        .filter(|e| e.code == crate::ptp::ev::OBJECT_ADDED)
        .filter_map(|e| e.params.first().copied())
        .collect();
    l.camera.capture()?;
    let mut handles = earlier;
    handles.extend(l.camera.wait_capture(Duration::from_secs(30))?);
    if handles.is_empty() {
        return Err(PtpError::Timeout);
    }
    download_into(l, &handles, dir)
}

/// Press the simulated camera's own shutter (tests, demo).
pub fn sim_shoot() -> Option<u32> {
    link().as_ref().and_then(|l| l.sim.as_ref()).map(SimCamera::shoot)
}

/// One live view frame (JPEG bytes).
pub fn live_view_frame() -> Result<Vec<u8>, PtpError> {
    let mut g = link();
    let l = g.as_mut().ok_or_else(|| PtpError::Unsupported("no camera connected".into()))?;
    l.camera.live_view_frame()
}

pub fn stop_live_view() {
    if let Some(l) = link().as_mut() {
        l.camera.stop_live_view();
    }
}
