//! The device's camera roll as an import source (Android): `file.addCameraRoll` asks the host
//! ([`crate::Services::camera_roll`]) for the photos in DCIM/Camera, which it lists through the
//! system's media store (asking for the photo permission first), and opens the import review on
//! them, as Import from Folder… does. On the first start a window asks whether to add it; the
//! answer is kept in the UI settings (`camera_roll_asked`), and Settings ▸ Folders offers it
//! again.
//!
//! Added for keeps — the prompt's Add Camera Roll, or Settings ▸ Folders — the roll is watched
//! (`camera_roll_watch`): it is listed again when the library opens, when the app comes to the
//! front and every few minutes, and photos new to the library are imported without the review
//! (issue #1024), as Auto Import adds a watched folder's. The watch shares the host's listing
//! with the review, in a pending slot of its own, and says nothing when there is no news.

use egui::{Align2, RichText, vec2};
use serde_json::Value;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use web_time::{Duration, Instant};

use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// `file.addCameraRoll`: starts listing the camera roll; the import review opens when it answers.
pub fn start(app: &mut LccApp) -> Result<Value, String> {
    app.ui.camera_roll_asked = true;
    if app.pending_camera_roll.is_some() {
        return Ok(Value::Null);
    }
    let Some(list) = app.services.camera_roll.as_mut() else {
        return Err("there is no camera roll on this platform".into());
    };
    app.pending_camera_roll = Some(list()?);
    Ok(Value::Null)
}

/// Adding the camera roll for keeps — the first-start prompt's Add Camera Roll, Settings ▸
/// Folders ▸ Camera roll ▸ Import…: it is watched from now on (`camera_roll_watch`), so photos
/// taken later join the library on their own.
pub fn add_and_watch(app: &mut LccApp) -> Result<Value, String> {
    app.ui.camera_roll_watch = true;
    start(app)
}

/// Settings ▸ Folders' "Watch for new photos" on the camera roll. Turning it off drops a listing
/// in flight and forgets when the last one ran, so turning it on lists at once.
pub fn set_watch(app: &mut LccApp, on: bool) -> Value {
    app.ui.camera_roll_watch = on;
    if !on {
        app.pending_camera_roll_watch = None;
        app.camera_roll_watch_at = None;
    }
    Value::Null
}

/// Each frame: the host's answer opens the import review, or says why there is nothing to import.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    let Some(rx) = &app.pending_camera_roll else { return };
    let answer = match rx.try_recv() {
        Ok(a) => a,
        Err(TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        Err(TryRecvError::Disconnected) => Err(crate::i18n::tr("the camera roll could not be read").to_string()),
    };
    app.pending_camera_roll = None;
    match answer {
        Ok(paths) if paths.is_empty() => app.toast(ctx, crate::i18n::tr("There are no photos in the camera roll")),
        Ok(paths) => match crate::import::open(app, paths) {
            Ok(_) => {
                if let Some(scan) = app.scan.as_mut() {
                    scan.camera_roll = true;
                }
            }
            Err(e) => app.toast_error(ctx, e),
        },
        Err(e) => app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Couldn't add the camera roll"))),
    }
}

/// Seconds between watch listings (folder_poll's cadence: at the front and on open at once).
const WATCH_INTERVAL_SECS: u64 = 5 * 60;

/// The watch (called every frame, as [`crate::folder_poll::tick`]; `came_forward`: the app was
/// just brought to the front): take in what a listing found, then list again when it is due —
/// the first tick of a library, at the front, and otherwise a few minutes after the last one.
/// Nothing runs while the library has a problem or an import, a scan, a review or a folder
/// synchronize does: a listing's news is dropped then, and the next one picks it up.
pub fn watch_tick(app: &mut LccApp, ctx: &egui::Context, came_forward: bool) {
    watch_collect(app, ctx);
    if !app.ui.camera_roll_watch || app.services.camera_roll.is_none() {
        return;
    }
    if app.library_problem.is_some()
        || crate::import::running(app)
        || app.sync_run.is_some()
        || app.lightroom.is_some()
        || app.pending_camera_roll.is_some()
    {
        return;
    }
    let library = lcc_engine::folder_watch::library_token(&app.session);
    let due = match &app.camera_roll_watch_at {
        // the library just opened (or another one did)
        None => true,
        Some((_, l)) if !Arc::ptr_eq(l, &library) => true,
        Some((at, _)) => came_forward || at.elapsed() >= Duration::from_secs(WATCH_INTERVAL_SECS),
    };
    if due {
        watch_list(app);
    } else {
        // (wake up for the next listing)
        ctx.request_repaint_after(Duration::from_secs(30));
    }
}

/// List the roll for the watch: the same host service as `file.addCameraRoll`, noted in a pending
/// slot of its own so it never fights the review's. The time is kept whatever the answer, so a
/// roll that can't be read is retried a full interval later, not every frame.
fn watch_list(app: &mut LccApp) {
    if app.pending_camera_roll_watch.is_some() {
        return;
    }
    let Some(list) = app.services.camera_roll.as_mut() else { return };
    app.camera_roll_watch_at = Some((Instant::now(), lcc_engine::folder_watch::library_token(&app.session)));
    match list() {
        Ok(rx) => app.pending_camera_roll_watch = Some(rx),
        Err(e) => log::warn!("camera roll watch: {e}"),
    }
}

/// Each frame: the watch's listing answers, and its new photos are handed to an import in place.
fn watch_collect(app: &mut LccApp, ctx: &egui::Context) {
    let Some(rx) = &app.pending_camera_roll_watch else { return };
    let answer = match rx.try_recv() {
        Ok(a) => a,
        Err(TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        Err(TryRecvError::Disconnected) => Err(crate::i18n::tr("the camera roll could not be read").to_string()),
    };
    app.pending_camera_roll_watch = None;
    match answer {
        Ok(paths) => {
            // device sync's camera-roll collection learns where the platform keeps the roll
            let _ = app.session.execute("sync.cameraRollSeen", &serde_json::json!({"paths": paths.iter().take(16).collect::<Vec<_>>()}));
            import_new(app, paths)
        }
        // a watch that can't read the roll says so in the log, not in a toast every few minutes
        Err(e) => log::warn!("camera roll watch: {e}"),
    }
}

/// Hand the listed paths the library doesn't have to an import in place, without the review. The
/// engine skips files it already has by path and by content anyway, but filtering first by path
/// (a photo in Recently Deleted counts as had) costs nothing when there is no news.
fn import_new(app: &mut LccApp, paths: Vec<String>) {
    let known: std::collections::HashSet<&str> = app
        .session
        .catalog
        .photos()
        .filter_map(|p| match &p.source {
            lcc_catalog::Source::File { path } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    let new: Vec<String> = paths.into_iter().filter(|p| !known.contains(p.as_str())).collect();
    if new.is_empty() {
        return;
    }
    if crate::import::running(app) || app.sync_run.is_some() || app.lightroom.is_some() {
        return;
    }
    if let Err(e) = crate::import::start_camera_roll(app, new) {
        log::warn!("camera roll watch: {e}");
    }
}

/// Whether the first-start question is due.
pub fn prompt_due(app: &LccApp) -> bool {
    app.services.camera_roll.is_some() && !app.ui.camera_roll_asked && app.library_problem.is_none() && app.pending_camera_roll.is_none()
}

/// The first-start question: add the camera roll to the library?
pub fn prompt(app: &mut LccApp, ctx: &egui::Context) {
    if !prompt_due(app) {
        return;
    }
    let t = Tokens::get(ctx);
    let mut choice = None;
    let frame = crate::dialog_frame::card_frame(ctx);
    egui::Window::new(crate::i18n::tr("Add Your Camera Roll?"))
        .id(egui::Id::new("camera-roll-prompt"))
        .title_bar(false)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(240.0, 400.0));
            ui.spacing_mut().item_spacing.y = 8.0;
            crate::dialog_frame::card_header(ui, crate::i18n::tr("Add Your Camera Roll?"), None);
            ui.add(
                egui::Label::new(
                    RichText::new(crate::i18n::tr(
                        "{app} can add the photos in your camera roll (DCIM/Camera) to the catalog. Android asks you to allow access to your photos first.",
                    ))
                    .color(t.text),
                )
                .wrap(),
            );
            ui.label(RichText::new(crate::i18n::tr("You can add it later from Settings ▸ Folders.")).color(t.text_dim));
            ui.horizontal(|ui| {
                for (id, label, add) in [("cameraRollAdd", "Add Camera Roll", true), ("cameraRollNotNow", "Not Now", false)] {
                    let r = ui.add(egui::Button::new(crate::i18n::tr(label)).min_size(vec2(0.0, 26.0)));
                    register(ui.ctx(), format!("button:{id}"), r.rect);
                    if r.clicked() {
                        choice = Some(add);
                    }
                }
            });
        });
    match choice {
        Some(true) => {
            if let Err(e) = add_and_watch(app) {
                app.toast_error(ctx, e);
            }
        }
        Some(false) => app.ui.camera_roll_asked = true,
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Arc, Mutex};

    type Answer = Arc<Mutex<Option<Sender<Result<Vec<String>, String>>>>>;

    fn app_with_roll() -> (LccApp, Answer) {
        let answer: Answer = Default::default();
        let a = answer.clone();
        let services = crate::Services {
            camera_roll: Some(Box::new(move || {
                let (tx, rx) = channel();
                *a.lock().unwrap() = Some(tx);
                Ok(rx)
            })),
            ..Default::default()
        };
        (LccApp::new(lcc_engine::Session::new(), services), answer)
    }

    /// A photo on disk for the listings to name.
    fn photo_in(dir: &std::path::Path, name: &str) -> String {
        let img = lcc_raster::Rgba8::from_fn(8, 6, |x, y| [(x * 20) as u8, (y * 30) as u8, 9, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        let path = dir.join(name);
        std::fs::write(&path, lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn without_a_camera_roll_nothing_is_asked_and_the_command_says_why() {
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        assert!(!prompt_due(&app));
        assert!(app.run("file.addCameraRoll", json!({})).is_err());
    }

    #[test]
    fn the_first_start_asks_once_and_the_answer_is_remembered() {
        let (mut app, _) = app_with_roll();
        assert!(prompt_due(&app));
        app.run("file.addCameraRoll", json!({})).unwrap();
        assert!(app.ui.camera_roll_asked);
        assert!(!prompt_due(&app));
        // the answer survives the settings file
        let saved: crate::UiState = serde_json::from_value(serde_json::to_value(&app.ui).unwrap()).unwrap();
        assert!(saved.camera_roll_asked);
        // an old settings file without it asks
        let old: crate::UiState = serde_json::from_value(json!({})).unwrap();
        assert!(!old.camera_roll_asked);
    }

    #[test]
    fn a_refused_permission_is_reported_and_the_roll_can_be_asked_for_again() {
        let ctx = egui::Context::default();
        let (mut app, answer) = app_with_roll();
        app.run("file.addCameraRoll", json!({})).unwrap();
        answer.lock().unwrap().take().unwrap().send(Err("permission denied".into())).unwrap();
        poll(&mut app, &ctx);
        assert!(app.pending_camera_roll.is_none());
        // asking again starts a new listing
        app.run("file.addCameraRoll", json!({})).unwrap();
        assert!(app.pending_camera_roll.is_some());
        // a host that went away without answering is an error too, never a hang
        drop(answer.lock().unwrap().take());
        poll(&mut app, &ctx);
        assert!(app.pending_camera_roll.is_none());
    }

    #[test]
    fn the_camera_roll_opens_the_import_review() {
        let ctx = egui::Context::default();
        let (mut app, answer) = app_with_roll();
        app.run("file.addCameraRoll", json!({})).unwrap();
        let dir = lcc_testkit::TempDir::new("lc-camera-roll");
        std::fs::create_dir_all(&dir).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 6, |x, y| [(x * 20) as u8, (y * 30) as u8, 9, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        let photo = dir.join("IMG_1.png");
        std::fs::write(&photo, lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        answer.lock().unwrap().take().unwrap().send(Ok(vec![photo.to_string_lossy().to_string()])).unwrap();
        poll(&mut app, &ctx);
        assert!(app.pending_camera_roll.is_none());
        assert!(app.scan.is_some(), "the import review's scan started");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Adding the roll for keeps (the prompt's Add Camera Roll, Settings ▸ Folders' Import…)
    /// watches it; the one-off `file.addCameraRoll` (Import Camera Roll…) does not. The choice
    /// survives the settings file; an old file starts with the watch off.
    #[test]
    fn adding_for_keeps_watches_and_a_one_off_import_does_not() {
        let (mut app, _) = app_with_roll();
        assert!(add_and_watch(&mut app).is_ok());
        assert!(app.ui.camera_roll_asked && app.ui.camera_roll_watch);
        let saved: crate::UiState = serde_json::from_value(serde_json::to_value(&app.ui).unwrap()).unwrap();
        assert!(saved.camera_roll_watch);
        let old: crate::UiState = serde_json::from_value(json!({})).unwrap();
        assert!(!old.camera_roll_watch);
        // the toggle turns it off again
        set_watch(&mut app, false);
        assert!(app.ui.camera_roll_asked && !app.ui.camera_roll_watch);
        // a one-off import never watches
        let (mut app, _) = app_with_roll();
        app.run("file.addCameraRoll", json!({})).unwrap();
        assert!(app.ui.camera_roll_asked && !app.ui.camera_roll_watch);
    }

    /// The watch lists when the library is ready (its first tick), and a listing's new photos go
    /// straight to an import in place: no scan, no review.
    #[test]
    fn the_watch_lists_on_open_and_imports_new_photos_without_the_review() {
        let ctx = egui::Context::default();
        let (mut app, answer) = app_with_roll();
        app.ui.camera_roll_watch = true;
        let dir = lcc_testkit::TempDir::new("lc-camera-roll-watch");
        std::fs::create_dir_all(&dir).unwrap();
        let photo = photo_in(&dir, "IMG_1.png");
        watch_tick(&mut app, &ctx, false);
        assert!(app.pending_camera_roll_watch.is_some(), "the first tick lists");
        answer.lock().unwrap().take().unwrap().send(Ok(vec![photo])).unwrap();
        watch_tick(&mut app, &ctx, false);
        assert!(app.pending_camera_roll_watch.is_none());
        assert!(app.import.is_some(), "the import task started");
        assert!(app.scan.is_none() && app.review_pending.is_none(), "no review opened");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Photos the library already has (a photo of theirs in Recently Deleted included) are
    /// skipped silently: nothing is imported, nothing said.
    #[test]
    fn photos_the_library_has_are_not_imported_again() {
        let ctx = egui::Context::default();
        let (mut app, answer) = app_with_roll();
        app.ui.camera_roll_watch = true;
        let dir = lcc_testkit::TempDir::new("lc-camera-roll-known");
        std::fs::create_dir_all(&dir).unwrap();
        let photo = photo_in(&dir, "IMG_1.png");
        app.session.execute("library.import", &json!({"paths": [photo.clone()]})).unwrap();
        watch_tick(&mut app, &ctx, false);
        answer.lock().unwrap().take().unwrap().send(Ok(vec![photo])).unwrap();
        watch_tick(&mut app, &ctx, false);
        assert!(app.import.is_none(), "nothing new, nothing imported");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without the service, with the watch off, or while an import (or any scan) runs, nothing is
    /// listed; a busy moment's news waits for the next listing.
    #[test]
    fn the_watch_lists_nothing_when_off_without_a_service_or_while_busy() {
        let ctx = egui::Context::default();
        // no service on this platform
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        app.ui.camera_roll_watch = true;
        watch_tick(&mut app, &ctx, false);
        assert!(app.pending_camera_roll_watch.is_none() && app.import.is_none());
        // the watch off (the default)
        let (mut app, _) = app_with_roll();
        watch_tick(&mut app, &ctx, true);
        assert!(app.pending_camera_roll_watch.is_none());
        // an import scan running: no listing starts
        let (mut app, _) = app_with_roll();
        app.ui.camera_roll_watch = true;
        let keep = crate::import::stall_scan(&mut app);
        watch_tick(&mut app, &ctx, true);
        assert!(app.pending_camera_roll_watch.is_none());
        drop(keep);
    }

    /// The folders list (Settings ▸ Folders', `app.folders`) shows the watch on the camera roll's
    /// row, as the panel's toggle and agents see it.
    #[test]
    fn the_folders_list_shows_the_watch() {
        let (mut app, _) = app_with_roll();
        let watch = |app: &mut LccApp| {
            app.run("app.folders", json!({})).unwrap().as_array().unwrap().iter().find(|f| f["id"] == "cameraRoll").unwrap()["watch"].clone()
        };
        assert_eq!(watch(&mut app), json!(false));
        set_watch(&mut app, true);
        assert_eq!(watch(&mut app), json!(true));
        // without a camera roll there is no row, so no watch either
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        assert!(!app.run("app.folders", json!({})).unwrap().as_array().unwrap().iter().any(|f| f["id"] == "cameraRoll"));
    }
}
