//! Edit in External Editor, the desktop half: open the copy `photo.editExternal` made and bring
//! the editor's saves back without the app having to lose and regain the focus.
//!
//! - **PhotoCraft** (the editor setting names a PhotoCraft binary or app): asked once on a worker
//!   thread whether it takes `--wait` (`--version --json`, never `--help`). When it does, it runs
//!   there with `--wait` (and `--single-instance` when supported, so repeated edits open tabs in
//!   one window); the activity stack shows the edit until it exits, then the copy reloads. An older
//!   PhotoCraft (a plain `--version` line) opens like any other editor.
//! - **Any editor**: opened through the host's `open_with`; the copy reloads when the app regains
//!   the focus (as before), and a cheap modification-time poll of the files out for editing (once
//!   a second) catches saves while the app keeps the focus.
//!
//! Sources: own design.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use lightcraft_engine::cmd::external_edit::editor::{self, Watched};
use serde_json::json;

use crate::LightcraftApp;
use crate::i18n::tr;

/// What the editor threads report.
#[derive(Debug)]
enum Msg {
    /// The editor started with `--wait` exited.
    Exited(PathBuf),
    /// The editor doesn't take `--wait`: open the file the usual way (`open_with`).
    OpenWith(PathBuf, String),
    Error(String),
}

/// Files out for editing and the editor threads' mailbox.
struct RoundTrip {
    watched: Watched,
    tx: std::sync::mpsc::Sender<Msg>,
    rx: std::sync::mpsc::Receiver<Msg>,
    last_poll: f64,
}

impl Default for RoundTrip {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        RoundTrip { watched: Watched::default(), tx, rx, last_poll: 0.0 }
    }
}

thread_local! {
    static ROUND_TRIP: RefCell<RoundTrip> = RefCell::new(RoundTrip::default());
}

/// Track `path` (out for editing) for photo `id`: it reloads when the file changes, and when the
/// app regains the focus.
pub(crate) fn watch(app: &mut LightcraftApp, path: &Path, id: u64) {
    if !app.ui.external_edits.contains(&id) {
        app.ui.external_edits.push(id);
    }
    ROUND_TRIP.with_borrow_mut(|rt| rt.watched.add(path, &[id]));
}

/// Open `path` in `editor` (the setting, or the system default when empty). `exe` is the program
/// file the host located for it (PhotoCraft found where its installers put it); a PhotoCraft
/// program, located or named, is asked whether it takes `--wait`.
pub(crate) fn open(app: &mut LightcraftApp, path: &Path, editor: &str, exe: Option<&str>) -> Result<(), String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let tool = exe.filter(|e| editor::is_photocraft(e)).unwrap_or(editor);
        if editor::is_photocraft(tool) && app.services.open_with.is_some() {
            return start_photocraft(app, tool.to_string(), editor.to_string(), path.to_path_buf());
        }
    }
    let _ = exe;
    open_with(path, editor, app.services.open_with.as_mut())
}

/// Hand `path` to the host's opener. The copy is LightCraft's own render, but it goes through the
/// same gate as any other path handed to the system opener (#2135). A host with no opener is not an error.
fn open_with(path: &Path, editor: &str, opener: Option<&mut crate::OpenWithFn>) -> Result<(), String> {
    let absolute = lightcraft_engine::open_path::openable(&path.to_string_lossy(), "")?;
    match opener {
        Some(f) => f(&absolute, editor),
        None => Ok(()),
    }
}

/// Ask PhotoCraft what it supports and run it with `--wait` on a worker thread (an activity row
/// meanwhile), or hand the file back to [`open_with`] when it can't wait.
#[cfg(not(target_arch = "wasm32"))]
fn start_photocraft(app: &mut LightcraftApp, tool: String, editor: String, path: PathBuf) -> Result<(), String> {
    let tx = ROUND_TRIP.with_borrow(|rt| rt.tx.clone());
    let activity = app.session.activity.clone();
    let spawned = std::thread::Builder::new().name("external-editor".into()).spawn(move || {
        let caps = editor::capabilities(&tool);
        if !caps.wait {
            let _ = tx.send(Msg::OpenWith(path, editor));
            return;
        }
        let line = editor::wait_command_line(&tool, &path, caps);
        let task = activity.start("externalEdit", "Editing in the external editor", lightcraft_engine::activity::Cancel::No);
        task.detail(&path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        let r = editor::run_and_wait(&line);
        drop(task);
        let _ = tx.send(match r {
            Ok(_) => Msg::Exited(path),
            Err(e) => Msg::Error(e),
        });
    });
    spawned.map(|_| ()).map_err(|e| e.to_string())
}

fn reload(app: &mut LightcraftApp, ctx: &egui::Context, ids: Vec<u64>) {
    if ids.is_empty() {
        return;
    }
    match app.session.execute("photo.reload", &json!({"ids": ids})) {
        Ok(r) if r["reloaded"].as_array().is_some_and(|a| !a.is_empty()) => app.toast(ctx, tr("Updated the edits saved in the external editor")),
        Ok(_) => {}
        Err(e) => app.ui.status = e.to_string(),
    }
}

/// Every frame: the editor threads' news, and once a second the modification times of the files
/// out for editing.
pub(crate) fn pump(app: &mut LightcraftApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let (msgs, changed, busy) = ROUND_TRIP.with_borrow_mut(|rt| {
        let msgs: Vec<Msg> = rt.rx.try_iter().take(256).collect();
        let changed = if !rt.watched.is_empty() && (now - rt.last_poll >= 1.0 || now < rt.last_poll) {
            rt.last_poll = now;
            rt.watched.changed()
        } else {
            Vec::new()
        };
        (msgs, changed, !rt.watched.is_empty())
    });
    for m in msgs {
        match m {
            Msg::Exited(p) => {
                let ids = ROUND_TRIP.with_borrow_mut(|rt| {
                    let ids = rt.watched.ids(&p);
                    rt.watched.mark_seen(&p);
                    rt.watched.retire(&p);
                    ids
                });
                reload(app, ctx, ids);
            }
            Msg::OpenWith(p, tool) => {
                if let Err(e) = open_with(&p, &tool, app.services.open_with.as_mut()) {
                    app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e));
                }
            }
            Msg::Error(e) => app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e)),
        }
    }
    reload(app, ctx, changed);
    if busy {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::headless::Headless;

    fn png(path: &Path, w: usize, h: usize, seed: u8) {
        let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, seed, 90, 255]).collect();
        let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
        std::fs::write(path, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-ui-extrt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn opened() -> (Services, std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>) {
        let opened: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
        let o = opened.clone();
        let services = Services {
            png: None,
            open_with: Some(Box::new(move |path: &str, app: &str| {
                o.lock().unwrap().push((path.to_string(), app.to_string()));
                Ok(())
            })),
            ..Default::default()
        };
        (services, opened)
    }

    /// A save to a file out for editing is picked up by the modification-time poll, without the
    /// app losing and regaining the focus.
    #[test]
    fn a_save_is_picked_up_without_focus() {
        let dir = temp("poll");
        let src = dir.join("b.png");
        png(&src, 20, 12, 1);
        let (services, _) = opened();
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), services), [900.0, 600.0], 1.0);
        h.app.session.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let id = h.app.session.active().unwrap();
        watch(&mut h.app, &src, id.0);
        h.step();
        png(&src, 30, 18, 200);
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3);
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(later).unwrap();
        let t0 = std::time::Instant::now();
        while h.app.session.catalog.photo(id).unwrap().width != 30 && t0.elapsed() < std::time::Duration::from_secs(20) {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(h.app.session.catalog.photo(id).unwrap().width, 30, "reloaded by the poll");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A PhotoCraft that takes `--wait` (a stand-in script answering `--version --json`) runs on a
    /// worker thread with an activity row; when it exits the copy reloads. `open_with` isn't used.
    #[cfg(unix)]
    #[test]
    fn photocraft_with_wait_reloads_when_it_exits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("wait");
        let src = dir.join("c.png");
        png(&src, 20, 12, 1);
        let gate = dir.join("go");
        let editor = dir.join("photocraft");
        let saved = dir.join("saved.tif");
        let px = vec![90u8; 14 * 6 * 3];
        let img = lightcraft_codecs::EncodeImage::new(14, 6, 3, lightcraft_codecs::Samples::U8(&px));
        std::fs::write(&saved, lightcraft_codecs::encode_tiff(&img, lightcraft_codecs::TiffCompression::None, &Default::default()).unwrap()).unwrap();
        // waits until the test says "save", then writes the file and exits
        std::fs::write(
            &editor,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '{{\"app\":\"photocraft\",\"features\":[\"wait\"]}}'; exit 0; fi\nfor f; do :; done\nwhile [ ! -e '{g}' ]; do sleep 0.05; done\ncp '{s}' \"$f\"\n",
                g = gate.display(),
                s = saved.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (services, opened) = opened();
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), services), [900.0, 600.0], 1.0);
        h.app.session.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        h.app.ui.settings.external_editor = editor.to_string_lossy().to_string();
        let r = h.request("engine.execute", json!({"command": "photo.editInExternal", "params": {}}), std::time::Duration::from_secs(10));
        assert_eq!(r["ok"], true, "{r}");
        let id = lightcraft_catalog::PhotoId(h.app.session.active().unwrap().0);
        let t0 = std::time::Instant::now();
        while !h.app.session.activity.list().iter().any(|t| t.kind == "externalEdit") && t0.elapsed() < std::time::Duration::from_secs(10) {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(h.app.session.activity.list().iter().any(|t| t.kind == "externalEdit" && !t.cancellable), "the edit shows in the activity stack");
        std::fs::write(&gate, b"").unwrap();
        while h.app.session.catalog.photo(id).unwrap().width != 14 && t0.elapsed() < std::time::Duration::from_secs(20) {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(h.app.session.catalog.photo(id).unwrap().width, 14, "reloaded when the editor exited");
        assert!(h.app.session.activity.list().is_empty(), "the row is gone");
        assert!(opened.lock().unwrap().is_empty(), "started by us, not by open_with");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
