//! Native file dialogs for commands, off the UI thread (#191). A synchronous dialog blocks the
//! thread that shows it, and on the desktop that was the UI thread: the window stopped answering
//! the compositor (GNOME marked the app "not responding" and offered to kill it), the menu the
//! command came from stayed drawn, and the control channel went quiet until the dialog closed.
//!
//! A host that can run a dialog elsewhere installs a [`Picker`] in `Services`: it starts the
//! dialog and answers through a channel. A command that needs a path the caller didn't give
//! [`ask`]s for one and returns at once; when the dialog closes, [`poll`] runs the command again
//! with the answer filled in under the parameter it was missing. Hosts without a `Picker` (the
//! web, where browser pickers are asynchronous already; tests) answer through the synchronous
//! services as before.

use crate::{DacApp, Services};
use serde_json::{Value, json};
use std::sync::mpsc::{Receiver, TryRecvError};

/// What a dialog chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    /// Any number of files.
    Files,
    /// One file.
    File,
    /// One folder.
    Folder,
    /// Where to save one file.
    Save,
}

/// A dialog to show. Texts are translated already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickRequest {
    pub kind: PickKind,
    /// The dialog's title; the host's default when `None`.
    pub title: Option<String>,
    /// One file-type filter: its name and the extensions it admits (without dots).
    pub filter: Option<(String, &'static [&'static str])>,
    /// [`PickKind::Save`]: the suggested file name.
    pub file_name: Option<String>,
}

impl PickRequest {
    pub fn files(title: Option<String>, filter: impl Into<String>, extensions: &'static [&'static str]) -> Self {
        PickRequest { kind: PickKind::Files, title, filter: Some((filter.into(), extensions)), file_name: None }
    }

    pub fn file(title: impl Into<String>, filter: impl Into<String>, extensions: &'static [&'static str]) -> Self {
        PickRequest { kind: PickKind::File, title: Some(title.into()), filter: Some((filter.into(), extensions)), file_name: None }
    }

    pub fn folder(title: impl Into<String>) -> Self {
        PickRequest { kind: PickKind::Folder, title: Some(title.into()), filter: None, file_name: None }
    }

    pub fn save(title: impl Into<String>, filter: impl Into<String>, extensions: &'static [&'static str], file_name: impl Into<String>) -> Self {
        PickRequest { kind: PickKind::Save, title: Some(title.into()), filter: Some((filter.into(), extensions)), file_name: Some(file_name.into()) }
    }
}

/// What File ▸ Import Profiles & Presets… opens.
pub const PRESET_EXTENSIONS: &[&str] = &["lcpreset", "xmp", "lrtemplate", "zip", "dng", "lmp", "mplumpack", "cube"];
/// What Import Point Curve Presets… opens.
pub const CURVE_PRESET_EXTENSIONS: &[&str] = &["lccurve", "json"];

/// The host's dialog starter: shows the request without blocking and answers through the
/// channel with the chosen paths (none = cancelled). `Err` when no dialog could be started.
pub type Picker = Box<dyn FnMut(PickRequest) -> Result<Receiver<Vec<String>>, String>>;

/// A dialog that is up for a command. When it closes, the command runs again with the answer
/// under `key`.
pub struct Pending {
    pub command: String,
    params: Value,
    key: &'static str,
    kind: PickKind,
    rx: Receiver<Vec<String>>,
}

/// How a command's request for a path was answered.
pub enum Picked {
    /// A synchronous dialog answered (an empty list: cancelled).
    Now(Vec<String>),
    /// The dialog is up (or was already); the command runs again when it closes.
    Later,
    /// This host has no such dialog.
    Unavailable,
}

/// Ask for a path for `command`, which was run with `params` but without `key`. With a [`Picker`]
/// the dialog runs off the UI thread and the command comes back through [`poll`] with `key` filled
/// in; a dialog already up for the command is not opened twice. Without one, `now` asks the
/// synchronous service.
pub fn ask(
    app: &mut DacApp,
    command: &str,
    params: &Value,
    key: &'static str,
    request: PickRequest,
    now: impl FnOnce(&mut Services) -> Option<Vec<String>>,
) -> Picked {
    if app.pending_picks.iter().any(|p| p.command == command) {
        return Picked::Later;
    }
    if let Some(start) = app.services.picker.as_mut() {
        let kind = request.kind;
        match start(request) {
            Ok(rx) => {
                app.pending_picks.push(Pending { command: command.to_string(), params: params.clone(), key, kind, rx });
                return Picked::Later;
            }
            Err(e) => log::warn!("{command}: no background file dialog ({e}); asking on the UI thread"),
        }
    }
    match now(&mut app.services) {
        Some(paths) => Picked::Now(paths),
        None => Picked::Unavailable,
    }
}

/// `params` with the dialog's answer under `key`: the whole list for a `paths` key, the first
/// path otherwise.
pub fn with_answer(params: &Value, key: &str, kind: PickKind, paths: &[String]) -> Value {
    let mut p = params.as_object().cloned().unwrap_or_default();
    let answer = match kind {
        PickKind::Files => json!(paths),
        _ => json!(paths.first().cloned().unwrap_or_default()),
    };
    p.insert(key.to_string(), answer);
    Value::Object(p)
}

/// Each frame: run the commands whose dialogs have closed with something chosen; forget the
/// cancelled ones and those whose dialog went away without an answer.
pub fn poll(app: &mut DacApp, ctx: &egui::Context) {
    if app.pending_picks.is_empty() {
        return;
    }
    let mut answered = Vec::new();
    let mut waiting = Vec::new();
    for p in std::mem::take(&mut app.pending_picks) {
        match p.rx.try_recv() {
            Ok(paths) if paths.is_empty() => {}
            Ok(paths) => answered.push((p, paths)),
            Err(TryRecvError::Empty) => waiting.push(p),
            Err(TryRecvError::Disconnected) => log::warn!("{}: the file dialog ended without an answer", p.command),
        }
    }
    if !waiting.is_empty() {
        // the host repaints when a dialog closes; this is the net under it
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    app.pending_picks = waiting;
    for (p, paths) in answered {
        let params = with_answer(&p.params, p.key, p.kind, &paths);
        if let Err(e) = app.run(&p.command, params) {
            app.toast_error(ctx, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::Headless;
    use crate::state::Dialog;
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const SETTLE: Duration = Duration::from_secs(20);

    type Asked = Arc<Mutex<Vec<(PickRequest, Sender<Vec<String>>)>>>;

    /// A windowless app whose host shows dialogs "elsewhere": each request is recorded with the
    /// sender that answers it. Frames run through the headless harness, so [`poll`] and the import
    /// scan happen as in the app.
    fn app_with_picker() -> (Headless, Asked) {
        let mut app = DacApp::new(dac_engine::Session::with_demo(), Default::default());
        let asked: Asked = Default::default();
        let a = asked.clone();
        app.services.picker = Some(Box::new(move |req| {
            let (tx, rx) = channel();
            a.lock().unwrap().push((req, tx));
            Ok(rx)
        }));
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        h.settle(SETTLE);
        (h, asked)
    }

    fn photo_in(dir: &std::path::Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let img = dac_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 50, 255]);
        let o = dac_engine::export::ExportOptions { format: dac_engine::export::ExportFormat::Png, ..Default::default() };
        let path = dir.join("one.png");
        std::fs::write(&path, dac_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        path.to_string_lossy().to_string()
    }

    /// The import review's sources once frames have run (the review opens after a background
    /// scan of the chosen paths).
    fn import_sources(h: &mut Headless) -> Option<Vec<String>> {
        h.settle(SETTLE);
        match &h.app.ui.dialog {
            Some(Dialog::Import { opts }) => Some(opts.sources.clone()),
            _ => None,
        }
    }

    #[test]
    fn import_photos_waits_for_its_dialog_and_runs_with_the_answer() {
        let dir = std::env::temp_dir().join(format!("lc-pick-{}", std::process::id()));
        let photo = photo_in(&dir);
        let (mut h, asked) = app_with_picker();
        // the command returns at once: no review of its own yet, the native dialog is up
        assert_eq!(h.app.run("file.addPhotos", json!({})).unwrap(), Value::Null);
        assert!(import_sources(&mut h).is_none());
        assert_eq!(h.app.pending_picks.len(), 1);
        {
            let asked = asked.lock().unwrap();
            assert_eq!(asked.len(), 1);
            assert_eq!(asked[0].0.kind, PickKind::Files);
            assert_eq!(asked[0].0.filter.as_ref().map(|f| f.1), Some(dac_engine::import::EXTENSIONS));
        }
        // asking again while it is up opens no second dialog
        assert_eq!(h.app.run("file.addPhotos", json!({})).unwrap(), Value::Null);
        h.settle(SETTLE);
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert_eq!(h.app.pending_picks.len(), 1, "still waiting");
        // the user chooses a photo: the command runs again with it and opens the import review
        asked.lock().unwrap()[0].1.send(vec![photo.clone()]).unwrap();
        assert_eq!(import_sources(&mut h), Some(vec![photo]));
        assert!(h.app.pending_picks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_or_vanished_dialog_runs_nothing() {
        let (mut h, asked) = app_with_picker();
        h.app.run("file.addPhotos", json!({})).unwrap();
        asked.lock().unwrap()[0].1.send(vec![]).unwrap();
        assert!(import_sources(&mut h).is_none(), "cancelled: no import review");
        assert!(h.app.pending_picks.is_empty());
        // the dialog thread ends without answering (the host dropped the sender)
        h.app.run("file.addFolder", json!({})).unwrap();
        assert_eq!(h.app.pending_picks.len(), 1);
        asked.lock().unwrap().pop();
        assert!(import_sources(&mut h).is_none());
        assert!(h.app.pending_picks.is_empty());
    }

    #[test]
    fn a_folder_dialog_fills_in_one_path_and_a_save_dialog_names_the_file() {
        let dir = std::env::temp_dir().join(format!("lc-pick-folder-{}", std::process::id()));
        let _photo = photo_in(&dir);
        let (mut h, asked) = app_with_picker();
        h.app.run("file.addFolder", json!({})).unwrap();
        assert_eq!(asked.lock().unwrap()[0].0.kind, PickKind::Folder);
        asked.lock().unwrap()[0].1.send(vec![dir.to_string_lossy().to_string()]).unwrap();
        assert_eq!(import_sources(&mut h), Some(vec![dir.to_string_lossy().to_string()]));
        // a save dialog carries the suggested name; the command's other parameters survive
        h.app.run("file.exportPresets", json!({"group": "Mine"})).unwrap();
        let req = asked.lock().unwrap().last().map(|(r, _)| r.clone()).unwrap();
        assert_eq!(req.kind, PickKind::Save);
        assert_eq!(req.file_name.as_deref(), Some(format!("Mine.{}", dac_brand::PRESET_EXT).as_str()));
        let p = with_answer(&json!({"group": "Mine"}), "path", PickKind::Save, &["/tmp/Mine.lcpreset".to_string(), "/tmp/other".to_string()]);
        assert_eq!(p, json!({"group": "Mine", "path": "/tmp/Mine.lcpreset"}));
        assert_eq!(with_answer(&Value::Null, "paths", PickKind::Files, &["a".to_string(), "b".to_string()]), json!({"paths": ["a", "b"]}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_picker_the_synchronous_dialog_answers_at_once() {
        let dir = std::env::temp_dir().join(format!("lc-pick-sync-{}", std::process::id()));
        let photo = photo_in(&dir);
        let mut app = DacApp::new(dac_engine::Session::with_demo(), Default::default());
        let p = photo.clone();
        app.services.pick_files = Some(Box::new(move || vec![p.clone()]));
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        h.app.run("file.addPhotos", json!({})).unwrap();
        assert!(h.app.pending_picks.is_empty());
        assert_eq!(import_sources(&mut h), Some(vec![photo]));
        // and a host with neither shows nothing, as before
        let mut bare = DacApp::new(dac_engine::Session::with_demo(), Default::default());
        assert_eq!(bare.run("file.addPhotos", json!({})).unwrap(), Value::Null);
        assert!(bare.run("file.importPresets", json!({})).is_err(), "no dialog on this platform");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
