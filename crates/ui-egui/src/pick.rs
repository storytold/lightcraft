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

use crate::{LccApp, Services};
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
    /// Narrower file-type filters offered after `filter`, in order ([`PickRequest::with_filter`]); a host
    /// that offers one filter only (Android) shows `filter`, so it admits every extension these do.
    pub more_filters: Vec<(String, &'static [&'static str])>,
    /// [`PickKind::Save`]: the suggested file name.
    pub file_name: Option<String>,
}

impl PickRequest {
    pub fn files(title: Option<String>, filter: impl Into<String>, extensions: &'static [&'static str]) -> Self {
        PickRequest { kind: PickKind::Files, title, filter: Some((filter.into(), extensions)), file_name: None, more_filters: Vec::new() }
    }

    pub fn file(title: impl Into<String>, filter: impl Into<String>, extensions: &'static [&'static str]) -> Self {
        PickRequest {
            kind: PickKind::File,
            title: Some(title.into()),
            filter: Some((filter.into(), extensions)),
            file_name: None,
            more_filters: Vec::new(),
        }
    }

    pub fn folder(title: impl Into<String>) -> Self {
        PickRequest { kind: PickKind::Folder, title: Some(title.into()), filter: None, file_name: None, more_filters: Vec::new() }
    }

    /// Offer one more file-type filter after the ones already there.
    pub fn with_filter(mut self, filter: impl Into<String>, extensions: &'static [&'static str]) -> Self {
        self.more_filters.push((filter.into(), extensions));
        self
    }

    /// Every file-type filter, `filter` first.
    pub fn filters(&self) -> impl Iterator<Item = &(String, &'static [&'static str])> {
        self.filter.iter().chain(self.more_filters.iter())
    }

    pub fn save(title: impl Into<String>, filter: impl Into<String>, extensions: &'static [&'static str], file_name: impl Into<String>) -> Self {
        PickRequest {
            kind: PickKind::Save,
            title: Some(title.into()),
            filter: Some((filter.into(), extensions)),
            file_name: Some(file_name.into()),
            more_filters: Vec::new(),
        }
    }
}

/// What File ▸ Import Profiles & Presets… opens.
pub const PRESET_EXTENSIONS: &[&str] =
    &["lcpreset", "xmp", "lrtemplate", "zip", "dng", "lmp", "mplumpack", "cube", "dcp", "lceffect", "lcmeta", "lcexport"];
/// What Import Point Curve Presets… opens.
pub const CURVE_PRESET_EXTENSIONS: &[&str] = &["lccurve", "json"];

/// The chosen paths, an empty successful answer for cancellation, or a dialog failure.
pub type PickResult = Result<Vec<String>, String>;

/// The host's dialog starter: shows the request without blocking and answers through the
/// channel. The outer `Err` means no dialog could be started; the channel can report failures
/// that happen after it starts.
pub type Picker = Box<dyn FnMut(PickRequest) -> Result<Receiver<PickResult>, String>>;

/// A command's pending dialog response, including a failed start. A successful choice runs the
/// command again with the answer under `key`.
pub struct Pending {
    pub command: String,
    params: Value,
    key: &'static str,
    kind: PickKind,
    // A failed start is reported on the next UI frame, just like a failed worker response.
    rx: Result<Receiver<PickResult>, String>,
}

/// How a command's request for a path was answered.
pub enum Picked {
    /// A synchronous dialog answered (an empty list: cancelled).
    Now(Vec<String>),
    /// The request is pending (or was already); polling runs the command again if paths are chosen.
    Later,
    /// This host has no such dialog.
    Unavailable,
}

/// Ask for a path for `command`, which was run with `params` but without `key`. With a [`Picker`]
/// the dialog runs off the UI thread and the command comes back through [`poll`] with `key` filled
/// in; a dialog already up for the command is not opened twice. Without one, `now` asks the
/// synchronous service.
pub fn ask(
    app: &mut LccApp,
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
        let rx = start(request);
        app.pending_picks.push(Pending { command: command.to_string(), params: params.clone(), key, kind, rx });
        // A failure must never retry the same native dialog synchronously (#1996).
        return Picked::Later;
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

fn failed(app: &mut LccApp, ctx: &egui::Context, error: &str) {
    log::error!("File chooser: {error}");
    app.toast_error(
        ctx,
        format!(
            "{} {error}\n{}",
            crate::i18n::tr("Couldn't open the file chooser:"),
            crate::i18n::tr("Try starting {app} from your desktop or terminal.")
        ),
    );
}

/// Each frame: run the commands whose dialogs have closed with something chosen; forget the
/// cancelled ones and report failures without retrying or changing the session.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    let errors: Vec<_> = app.services.dialog_errors.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
    for error in errors {
        failed(app, ctx, &error);
    }
    if app.pending_picks.is_empty() {
        return;
    }
    let mut answered = Vec::new();
    let mut waiting = Vec::new();
    for p in std::mem::take(&mut app.pending_picks) {
        let result = match &p.rx {
            Ok(rx) => rx.try_recv(),
            Err(error) => {
                failed(app, ctx, error);
                continue;
            }
        };
        match result {
            Ok(Ok(paths)) if paths.is_empty() => {}
            Ok(Ok(paths)) => answered.push((p, paths)),
            Ok(Err(error)) => failed(app, ctx, &error),
            Err(TryRecvError::Empty) => waiting.push(p),
            Err(TryRecvError::Disconnected) => failed(app, ctx, "the file dialog ended without an answer"),
        }
    }
    if !waiting.is_empty() {
        // the host repaints when a dialog closes; this is the net under it
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    app.pending_picks = waiting;
    for (p, paths) in answered {
        let params = with_answer(&p.params, p.key, p.kind, &paths);
        match app.run(&p.command, params) {
            Ok(r) => {
                if let Some(text) = answered_message(&p.command, &r) {
                    app.toast(ctx, text);
                }
            }
            Err(e) => app.toast_error(ctx, e),
        }
    }
}

/// What `command`, run with a dialog's answer, says when it worked (`None`: its result shows
/// for itself). The same words whether the dialog answered at once or later.
pub fn answered_message(command: &str, result: &Value) -> Option<String> {
    match command {
        "folder.move" => Some(crate::i18n::tr_format!("Moved; {} photo(s) relinked", result["relinked"])),
        _ => None,
    }
}

/// A dialog's answer that arrived after the app was restarted (a phone or tablet stops apps in
/// the background to free memory, and the system picker puts the app in the background): the
/// command that asked is gone. Photos are imported as Add Photos would have; anything else can't
/// be finished, and the user is told to choose again. `paths` is `Err` when the host could not get
/// the chosen files.
pub fn answer_after_restart(app: &mut LccApp, ctx: &egui::Context, kind: PickKind, paths: Result<Vec<String>, String>) {
    let paths = match paths {
        Ok(p) => p,
        Err(e) => {
            app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Couldn't get the chosen files")));
            return;
        }
    };
    if paths.is_empty() {
        return;
    }
    let photos = matches!(kind, PickKind::Files | PickKind::File) && paths.iter().all(|p| lcc_engine::import::is_supported(std::path::Path::new(p)));
    if !photos {
        log::warn!("a {kind:?} dialog answered after a restart: {} path(s) not used", paths.len());
        app.toast_error(ctx, crate::i18n::tr("The app was restarted while the file dialog was open: please choose again"));
        return;
    }
    log::info!("importing {} photo(s) chosen before the app was restarted", paths.len());
    if let Err(e) = app.run("file.addPhotos", json!({ "paths": paths })) {
        app.toast_error(ctx, e);
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

    type Asked = Arc<Mutex<Vec<(PickRequest, Sender<PickResult>)>>>;

    /// A windowless app whose host shows dialogs "elsewhere": each request is recorded with the
    /// sender that answers it. Frames run through the headless harness, so [`poll`] and the import
    /// scan happen as in the app.
    fn app_with_picker() -> (Headless, Asked) {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), Default::default());
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
        let img = lcc_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 50, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        let path = dir.join("one.png");
        std::fs::write(&path, lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
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
        let dir = lcc_testkit::TempDir::new("lc-pick");
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
            assert_eq!(asked[0].0.filter.as_ref().map(|f| f.1), Some(lcc_engine::import::EXTENSIONS));
        }
        // asking again while it is up opens no second dialog
        assert_eq!(h.app.run("file.addPhotos", json!({})).unwrap(), Value::Null);
        h.settle(SETTLE);
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert_eq!(h.app.pending_picks.len(), 1, "still waiting");
        // the user chooses a photo: the command runs again with it and opens the import review
        asked.lock().unwrap()[0].1.send(Ok(vec![photo.clone()])).unwrap();
        assert_eq!(import_sources(&mut h), Some(vec![photo]));
        assert!(h.app.pending_picks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn photos_chosen_before_a_restart_still_open_the_import_review() {
        let dir = lcc_testkit::TempDir::new("lc-pick-restart");
        let photo = photo_in(&dir);
        let (mut h, _) = app_with_picker();
        let ctx = h.view.ctx.clone();
        answer_after_restart(&mut h.app, &ctx, PickKind::Files, Ok(vec![photo.clone()]));
        assert_eq!(import_sources(&mut h), Some(vec![photo]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_or_unusable_answer_after_a_restart_is_reported() {
        let (mut h, _) = app_with_picker();
        let ctx = h.view.ctx.clone();
        answer_after_restart(&mut h.app, &ctx, PickKind::Files, Err("disk full".into()));
        assert!(h.app.ui.take_toast().is_some_and(|t| t.0.contains("disk full")), "a failure is shown");
        answer_after_restart(&mut h.app, &ctx, PickKind::Folder, Ok(vec!["/nowhere".into()]));
        assert!(h.app.ui.take_toast().is_some(), "a folder's command is gone: the user is told");
        answer_after_restart(&mut h.app, &ctx, PickKind::Files, Ok(vec![]));
        assert!(h.app.ui.toast().is_none(), "cancelled: nothing to say");
        assert!(import_sources(&mut h).is_none());
    }

    #[test]
    fn move_folder_asks_off_the_ui_thread_and_runs_when_the_dialog_closes() {
        let (mut h, asked) = app_with_picker();
        let params = json!({"path": "/pics/trip"});
        let req = PickRequest::folder("Move Folder To…");
        let picked = ask(&mut h.app, "folder.move", &params, "into", req, |_| None);
        assert!(matches!(picked, Picked::Later), "the window is not held up");
        assert_eq!(asked.lock().unwrap()[0].0.kind, PickKind::Folder);
        asked.lock().unwrap()[0].1.send(Ok(vec!["/pics/elsewhere".to_string()])).unwrap();
        let ctx = h.view.ctx.clone();
        poll(&mut h.app, &ctx);
        assert!(h.app.pending_picks.is_empty());
        assert!(h.app.ui.take_toast().is_some(), "the move ran with the chosen folder and said how it went");
    }

    #[test]
    fn a_cancelled_or_vanished_dialog_runs_nothing() {
        let (mut h, asked) = app_with_picker();
        h.app.run("file.addPhotos", json!({})).unwrap();
        asked.lock().unwrap()[0].1.send(Ok(vec![])).unwrap();
        assert!(import_sources(&mut h).is_none(), "cancelled: no import review");
        assert!(h.app.pending_picks.is_empty());
        assert!(h.app.ui.toast().is_none(), "cancellation is not an error");
        // the dialog thread ends without answering (the host dropped the sender)
        assert!(h.app.ui.toast().is_none(), "cancelled: nothing to say");
        h.app.run("file.addFolder", json!({})).unwrap();
        assert_eq!(h.app.pending_picks.len(), 1);
        asked.lock().unwrap().pop();
        assert!(import_sources(&mut h).is_none());
        assert!(h.app.pending_picks.is_empty());
        // the user chose something that never arrived: they are told
        assert!(h.app.ui.toast().is_some(), "a dialog that ended without an answer is reported");
    }

    #[test]
    fn a_folder_dialog_fills_in_one_path_and_a_save_dialog_names_the_file() {
        let dir = lcc_testkit::TempDir::new("lc-pick-folder");
        let _photo = photo_in(&dir);
        let (mut h, asked) = app_with_picker();
        // a host with only background dialogs (Android has no synchronous folder picker) still
        // offers File ▸ Import from Folder…
        assert!(h.app.services.pick_folder.is_none());
        assert!(crate::menus::ui_enabled(&h.app, "file.addFolder"), "Import from Folder… is enabled with a background picker");
        h.app.run("file.addFolder", json!({})).unwrap();
        assert_eq!(asked.lock().unwrap()[0].0.kind, PickKind::Folder);
        asked.lock().unwrap()[0].1.send(Ok(vec![dir.to_string_lossy().to_string()])).unwrap();
        assert_eq!(import_sources(&mut h), Some(vec![dir.to_string_lossy().to_string()]));
        // a save dialog carries the suggested name; the command's other parameters survive
        h.app.run("file.exportPresets", json!({"group": "Mine"})).unwrap();
        let req = asked.lock().unwrap().last().map(|(r, _)| r.clone()).unwrap();
        assert_eq!(req.kind, PickKind::Save);
        assert_eq!(req.file_name.as_deref(), Some(format!("Mine.{}", lcc_brand::PRESET_EXT).as_str()));
        let p = with_answer(&json!({"group": "Mine"}), "path", PickKind::Save, &["/tmp/Mine.lcpreset".to_string(), "/tmp/other".to_string()]);
        assert_eq!(p, json!({"group": "Mine", "path": "/tmp/Mine.lcpreset"}));
        assert_eq!(with_answer(&Value::Null, "paths", PickKind::Files, &["a".to_string(), "b".to_string()]), json!({"paths": ["a", "b"]}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_dialog_start_never_retries_the_synchronous_picker() {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        app.services.picker = Some(Box::new(|_| Err("worker unavailable".into())));
        app.services.pick_files = Some(Box::new(|| panic!("must not retry the native dialog")));
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        h.settle(SETTLE);
        h.app.run("file.addPhotos", json!({})).unwrap();
        h.step();
        h.step();
        assert!(h.app.pending_picks.is_empty());
        assert!(h.painted_text().iter().any(|text| text.contains("worker unavailable")));
        assert!(!matches!(h.app.ui.dialog, Some(Dialog::Import { .. })));
        assert!(h.app.run("view.squareGrid", json!({})).is_ok());
    }

    #[test]
    fn a_failed_worker_reports_the_error_and_keeps_the_session_usable() {
        for fullscreen in [false, true] {
            let (mut h, asked) = app_with_picker();
            h.app.services.pick_files = Some(Box::new(|| panic!("must not retry the native dialog")));
            h.app.run("develop.set", json!({"values": {"light.exposure": 0.75}})).unwrap();
            let catalog = h.app.session.catalog.to_snapshot();
            let selected = h.app.session.selected();
            let active = h.app.session.active();
            if fullscreen {
                h.app.run("view.fullScreenPreview", json!({})).unwrap();
            }
            h.app.ui.dialog = Some(Dialog::Rename { template: "keep this draft".into(), start: 7, texts: Default::default() });
            h.app.run("file.addPhotos", json!({})).unwrap();
            asked.lock().unwrap()[0].1.send(Err("macOS refused the file chooser".into())).unwrap();
            h.step();
            h.step();
            assert!(h.app.pending_picks.is_empty());
            assert!(h.painted_text().iter().any(|text| text.contains("macOS refused the file chooser")));
            assert!(h.painted_text().iter().any(|text| text.contains("Try starting")));
            assert_eq!(h.app.session.catalog.to_snapshot(), catalog);
            assert_eq!(h.app.session.selected(), selected);
            assert_eq!(h.app.session.active(), active);
            assert!(matches!(&h.app.ui.dialog, Some(Dialog::Rename { template, start: 7, .. }) if template == "keep this draft"));
            assert_eq!(asked.lock().unwrap().len(), 1, "no automatic retry");
            let response = h.request("engine.execute", json!({"command": "photo.rate", "params": {"rating": 4}}), SETTLE);
            assert_eq!(response["ok"], true, "the control channel still answers commands: {response}");
            let photo = h.app.run("photo.inspect", json!({})).unwrap();
            assert_eq!(photo["rating"], 4);
            assert_eq!(photo["develop"]["light"]["exposure"], 0.75);
        }
    }

    #[test]
    fn a_legacy_dialog_failure_is_visible_without_replacing_the_current_dialog() {
        for fullscreen in [false, true] {
            let (mut h, _) = app_with_picker();
            let (tx, rx) = channel();
            h.app.services.dialog_errors = Some(rx);
            if fullscreen {
                h.app.run("view.fullScreenPreview", json!({})).unwrap();
            }
            h.app.ui.dialog = Some(Dialog::Rename { template: "keep this draft".into(), start: 7, texts: Default::default() });
            let catalog = h.app.session.catalog.to_snapshot();
            tx.send("macOS refused the folder chooser".into()).unwrap();
            h.step();
            h.step();
            assert!(h.painted_text().iter().any(|text| text.contains("macOS refused the folder chooser")));
            assert!(matches!(&h.app.ui.dialog, Some(Dialog::Rename { template, start: 7, .. }) if template == "keep this draft"));
            assert_eq!(h.app.session.catalog.to_snapshot(), catalog);
            assert!(h.app.pending_picks.is_empty());
        }
    }

    #[test]
    fn without_a_picker_the_synchronous_dialog_answers_at_once() {
        let dir = lcc_testkit::TempDir::new("lc-pick-sync");
        let photo = photo_in(&dir);
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        let p = photo.clone();
        app.services.pick_files = Some(Box::new(move || vec![p.clone()]));
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        h.app.run("file.addPhotos", json!({})).unwrap();
        assert!(h.app.pending_picks.is_empty());
        assert_eq!(import_sources(&mut h), Some(vec![photo]));
        // and a host with neither shows nothing, as before
        let mut bare = LccApp::new(lcc_engine::Session::with_demo(), Default::default());
        assert_eq!(bare.run("file.addPhotos", json!({})).unwrap(), Value::Null);
        assert!(bare.run("file.importPresets", json!({})).is_err(), "no dialog on this platform");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
