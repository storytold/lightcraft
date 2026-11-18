//! File ▸ Email Photo…: the dialog (long edge and quality of the copies), the background export of
//! the selection into a temporary folder, and the hand-off of the files to the system's mail
//! program ([`lcc_engine::email`]). The export is the ordinary one ([`crate::export_task`]: activity
//! row, ✕, one photo at a time in the order shown); the hand-off runs on its worker when the files
//! are written. With no mail program to ask, the files stay in their folder, which is shown, and
//! the message says where they are.
//!
//! Sources: own design.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::LccApp;
use crate::state::Dialog;

/// UI commands: (id, label, shortcut, menu).
#[cfg(not(target_arch = "wasm32"))]
pub const COMMANDS: &[crate::menus::UiCommand] = &[
    ("dialog.emailPhoto", "Email Photo…", None, "File"),
    // {ids?, longEdge?, quality?}: exports the photos small and opens a new message with them attached
    ("app.emailPhoto", "Email Photo", None, ""),
];
#[cfg(target_arch = "wasm32")]
pub const COMMANDS: &[crate::menus::UiCommand] = &[];

/// Default and range of the copies' long edge (pixels), and the long edges the dialog offers.
pub const DEFAULT_EDGE: u32 = 1600;
pub const EDGE_RANGE: (u32, u32) = (100, 8000);
pub const EDGES: &[u32] = &[640, 1024, 1600, 2048, 3000];
/// Default JPEG quality.
pub const DEFAULT_QUALITY: u8 = 80;

/// The dialog's state, default first.
pub fn new_dialog() -> Dialog {
    Dialog::EmailPhoto { long_edge: DEFAULT_EDGE, quality: DEFAULT_QUALITY }
}

/// The dialog's `app.emailPhoto` params.
pub fn dialog_params(d: &Dialog) -> Option<Value> {
    let Dialog::EmailPhoto { long_edge, quality } = d else { return None };
    Some(json!({"longEdge": long_edge, "quality": quality}))
}

pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    COMMANDS.iter().any(|c| c.0 == id).then(|| !app.session.selected().is_empty() && app.export.is_none() && app.services.write_shared.is_some())
}

pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "dialog.emailPhoto" if COMMANDS.iter().any(|c| c.0 == id) => {
            app.ui.dialog = Some(new_dialog());
            Ok(Value::Null)
        }
        "app.emailPhoto" if COMMANDS.iter().any(|c| c.0 == id) => start(app, p),
        _ => return None,
    })
}

fn number(p: &Value, key: &str, default: u64, range: (u64, u64)) -> Result<u64, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| (range.0..=range.1).contains(n))
            .ok_or_else(|| format!("{key} must be a whole number from {} to {}", range.0, range.1)),
    }
}

/// Export the photos (`ids`, else the selection) as small JPEGs into a new temporary folder in the
/// background, then hand them to the mail program → `{background: true, total}`.
pub fn start(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    use lcc_engine::email::{self, Os, Outcome};
    use lcc_engine::export::{Destination, ExportOptions};
    if app.export.is_some() {
        return Err("an export is already running".into());
    }
    let edge = number(p, "longEdge", u64::from(DEFAULT_EDGE), (u64::from(EDGE_RANGE.0), u64::from(EDGE_RANGE.1)))?;
    let quality = number(p, "quality", u64::from(DEFAULT_QUALITY), (1, 100))?;
    let ids = app.session.targets(p).map_err(|e| e.to_string())?;
    if ids.is_empty() {
        return Err("no photo selected".into());
    }
    if ids.len() > email::MAX_FILES {
        return Err(format!("a message takes at most {} attachments: select fewer photos", email::MAX_FILES));
    }
    // sRGB JPEGs; a photo's location is not sent along
    let opts =
        ExportOptions::from_params(&json!({"format": "jpeg", "quality": quality, "longEdge": edge, "colorSpace": "srgb", "removeLocation": true}))
            .map_err(|e| e.to_string())?;
    let after = app.session.after_export(&opts)?;
    let items = lcc_engine::export::prepare_batch(&mut app.session, &ids, &opts)?;
    let dir = email::temp_dir(lcc_brand::BINARY)?;
    let to = Destination { dir: dir.to_string_lossy().into_owned(), exact: None };
    let desktop = app.session.desktop.clone();
    let then: crate::export_task::Then = Box::new(move |files: &[Value]| {
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).map(PathBuf::from).collect();
        if paths.is_empty() {
            return None;
        }
        let shown = match email::hand_off(&*desktop, Os::current(), &paths, "", &email::find_program) {
            Ok(Outcome::Opened { program }) => json!({"opened": program, "count": paths.len(), "dir": dir}),
            Ok(Outcome::Folder { dir, why }) => json!({"folder": dir, "why": why, "count": paths.len()}),
            Err(why) => json!({"folder": dir, "why": why, "count": paths.len()}),
        };
        Some(json!({"email": shown}))
    });
    crate::export_task::start_then(app, items, opts, to, Some(after), Some(then))
}

/// The toast's account of the hand-off (`email` of the batch's last result).
pub fn note(e: &Value) -> String {
    match e.get("folder").and_then(Value::as_str) {
        Some(dir) => {
            let why = e.get("why").and_then(Value::as_str).unwrap_or("");
            crate::i18n::tr_format!("Not emailed: {why}. The photos are in {dir}", why = why, dir = dir)
        }
        None => crate::i18n::tr_format!("New email opened · {n} attached", n = e.get("count").and_then(Value::as_u64).unwrap_or(0)),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use lcc_engine::desktop::{Recorder, Request};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn app_with(rec: Arc<Recorder>) -> LccApp {
        let services = crate::Services { write_shared: Some(Arc::new(lcc_engine::export::write_file)), ..Default::default() };
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
        app.session.desktop = rec;
        app
    }

    fn finish(app: &mut LccApp) {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(300);
        while app.export.is_some() {
            crate::export_task::poll(app, &ctx);
            assert!(Instant::now() < deadline, "the export did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The menu item opens the dialog; confirming exports the selection small into a temp folder and
    /// asks the desktop for a mail program (recorded, never opened). With no mail program the files stay in
    /// their folder and the folder is shown. (Either way the files are what was asked for.)
    #[test]
    fn the_selection_is_exported_small_and_handed_to_the_mail_program() {
        let rec = Arc::new(Recorder::default());
        let mut app = app_with(rec.clone());
        assert!(crate::menus::menu_entries(&app).iter().any(|e| e.id == "dialog.emailPhoto" && e.menu == ["File"]));
        let ids: Vec<u64> = app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
        app.run("library.select", json!({"ids": ids, "active": ids[0]})).unwrap();
        app.run("dialog.emailPhoto", json!({})).unwrap();
        let Some(Dialog::EmailPhoto { long_edge, .. }) = app.ui.dialog.as_mut() else { panic!("no email dialog") };
        *long_edge = 320;
        let params = dialog_params(app.ui.dialog.as_ref().unwrap()).unwrap();
        app.ui.dialog = None;
        app.run("app.emailPhoto", params).unwrap();
        assert!(app.export.is_some(), "the export runs in the background");
        assert_eq!(app.session.activity.list().len(), 1, "with its row in the activity stack");
        finish(&mut app);
        let result = app.last_export_result.clone().expect("the batch reports");
        let files: Vec<&str> = result["files"].as_array().unwrap().iter().filter_map(|f| f["path"].as_str()).collect();
        assert_eq!(files.len(), 2, "{result}");
        let mail = result["files"].as_array().unwrap().last().unwrap()["email"].clone();
        assert!(mail.is_object(), "the hand-off is reported: {result}");
        for f in &files {
            let bytes = std::fs::read(f).unwrap();
            assert!(bytes.starts_with(&[0xFF, 0xD8]), "{f} is a JPEG");
            let head = lcc_codecs::read_header(&bytes).unwrap();
            assert!(head.width.max(head.height) <= 320, "{f}: {}x{}", head.width, head.height);
        }
        // either a mail program was launched with every file, or the folder was shown instead
        let requests = rec.requests();
        match (mail["opened"].is_string(), mail["folder"].is_string()) {
            (true, _) => {
                assert!(matches!(requests.as_slice(), [Request::Launch(line)] if files.iter().all(|f| line.iter().any(|a| a == f))), "{requests:?}")
            }
            (_, true) => assert!(matches!(requests.as_slice(), [Request::Reveal(_)]), "{requests:?}"),
            _ => panic!("neither opened nor shown: {mail}"),
        }
        assert!(app.session.activity.list().is_empty());
        if let Some(dir) = files.first().and_then(|f| std::path::Path::new(f).parent()) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn bad_params_and_a_busy_export_are_errors_not_panics() {
        let mut app = app_with(Arc::new(Recorder::default()));
        for bad in [json!({"longEdge": 5}), json!({"longEdge": "x"}), json!({"quality": 0}), json!({"quality": 500}), json!({"ids": [1.5]})] {
            assert!(start(&mut app, &bad).is_err(), "{bad}");
        }
        assert!(app.export.is_none());
        assert!(note(&json!({"folder": "/tmp/x", "why": "no mail program"})).contains("/tmp/x"));
    }
}
