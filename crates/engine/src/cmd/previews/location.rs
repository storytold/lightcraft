//! The background folder change keeps the old location until all copies verify.

use super::*;
use crate::smart::{Existing, migration::Prepared};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

pub(crate) struct Migration {
    from: PathBuf,
    to: PathBuf,
    custom: Option<PathBuf>,
    task: Option<Arc<TaskGuard>>,
    phase: Phase,
}

enum Phase {
    Copying(Receiver<std::result::Result<Option<Prepared>, String>>),
    Cleaning(Receiver<Value>),
}

impl Drop for Migration {
    fn drop(&mut self) {
        if let Some(flag) = self.task.as_ref().and_then(|task| task.cancel_flag()) {
            flag.store(true, Ordering::Release);
        }
    }
}

pub(crate) fn busy(s: &Session) -> bool {
    s.activity.list().iter().any(|t| matches!(t.kind, "smartPreviews" | "smartRefresh" | "smartMigration"))
}

pub(super) fn start(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.smartPreviewsLocation";
    poll(s);
    let current = s.media.smart_dir.clone().ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let reset = p.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let chosen = str_param(p, "path").filter(|x| !x.trim().is_empty());
    if !reset && chosen.is_none() {
        let mut result = super::location_json(s);
        result["running"] = json!(s.smart_migration.is_some());
        if let Some(outcome) = &s.smart_migration_result {
            result["migration"] = match outcome {
                Ok(value) => value.clone(),
                Err(error) => json!({"error": error}),
            };
        }
        return Ok(result);
    }
    if busy(s) {
        return Err(bad(C, "smart preview work is already running; wait for it to finish before changing folders"));
    }
    let lib_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone()).ok_or_else(|| bad(C, "smart previews need a library on disk"))?;
    let default = crate::smart::dir(&lib_dir);
    let (to, custom) = match chosen {
        Some(x) if !reset => {
            let dir = PathBuf::from(x.trim());
            if !dir.is_absolute() {
                return Err(bad(C, "path must be an absolute folder path"));
            }
            (dir.clone(), (dir != default).then_some(dir))
        }
        _ => (default, None),
    };
    if to == current {
        return Ok(super::location_json(s));
    }
    let existing =
        str_param(p, "existing").map(|x| Existing::parse(x).ok_or_else(|| bad(C, "existing must be move, leave or discard"))).transpose()?;
    let task = Arc::new(s.activity.start("smartMigration", "Changing smart preview folder", Cancel::Yes));
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false);
    s.smart_migration_result = None;
    if !background {
        let prepared = crate::smart::migration::prepare(&current, &to, existing, &task.handle()).map_err(|e| bad(C, e))?;
        let Some(prepared) = prepared else {
            return Ok(json!({"cancelled": true, "path": current}));
        };
        if !commit(s, &to, custom, &task).map_err(|e| bad(C, e))? {
            return Ok(json!({"cancelled": true, "path": current}));
        }
        let (handled, failed) = prepared.cleanup(&task.handle());
        let mut result = super::location_json(s);
        result["handled"] = json!(handled);
        result["failed"] = json!(failed);
        return Ok(result);
    }
    let (tx, rx) = mpsc::channel();
    let (from_worker, to_worker) = (current.clone(), to.clone());
    // The task guard stays with the session through commit and is moved to cleanup afterward.
    let worker_task = task.clone();
    std::thread::Builder::new()
        .name("lc-smart-move".into())
        .spawn(move || {
            let result = crate::guard::catch("moving smart previews", || {
                crate::smart::migration::prepare(&from_worker, &to_worker, existing, &worker_task.handle())
            })
            .unwrap_or_else(Err);
            let _ = tx.send(result);
        })
        .map_err(|e| bad(C, format!("could not start: {e}")))?;
    s.smart_migration = Some(Migration { from: current.clone(), to, custom, task: Some(task), phase: Phase::Copying(rx) });
    Ok(json!({"running": true, "path": current}))
}

fn commit(s: &mut Session, to: &std::path::Path, custom: Option<PathBuf>, task: &TaskGuard) -> std::result::Result<bool, String> {
    // Cancellation is accepted through copying. Once the preference is saved, cleanup must
    // finish; an interrupted cleanup only leaves redundant files in the old folder.
    task.set_cancellable(false);
    if task.is_cancelled() {
        return Ok(false);
    }
    let previous_generation = s.smart_previews_cache_generation;
    let next_generation = previous_generation.checked_add(1).ok_or("thumbnail cache generation is exhausted")?;
    let previous = std::mem::replace(&mut s.smart_previews_dir, custom);
    s.smart_previews_cache_generation = next_generation;
    if let Err(error) = s.save_prefs() {
        s.smart_previews_dir = previous;
        s.smart_previews_cache_generation = previous_generation;
        return Err(error.to_string());
    }
    // The durable folder and namespace change together. Even an exit inside save_prefs
    // reopens with different thumbnail keys. Replacing this cache does no file scan or
    // deletion. Cache identity rejects old results; their disk writes use the old namespace.
    if let Some(lib) = s.library.as_ref() {
        s.media.attach_disk_cache_namespace(&lib.thumbs_dir(), s.cache_bytes(), next_generation);
    }
    s.media.smart_dir = Some(to.to_path_buf());
    s.media.clear_sources();
    Ok(true)
}

pub(crate) fn poll(s: &mut Session) {
    let Some(mut migration) = s.smart_migration.take() else { return };
    let result = match &migration.phase {
        Phase::Copying(rx) => match rx.try_recv() {
            Err(TryRecvError::Empty) => {
                s.smart_migration = Some(migration);
                return;
            }
            Err(_) => Err("the smart preview folder worker stopped unexpectedly".to_string()),
            Ok(result) => result,
        },
        Phase::Cleaning(rx) => {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {
                    s.smart_migration = Some(migration);
                    return;
                }
                Err(_) => s.smart_migration_result = Some(Err("smart preview cleanup stopped; the new folder remains in use".into())),
                Ok(result) => s.smart_migration_result = Some(Ok(result)),
            }
            return;
        }
    };
    let Some(task) = migration.task.take() else { return };
    let prepared = match result {
        Err(error) => {
            s.smart_migration_result = Some(Err(error));
            return;
        }
        Ok(prepared) => prepared,
    };
    if task.is_cancelled() || prepared.is_none() {
        s.smart_migration_result = Some(Ok(json!({"cancelled": true, "path": migration.from})));
        return;
    }
    let Some(prepared) = prepared else { return };
    match commit(s, &migration.to, migration.custom.clone(), &task) {
        Err(error) => {
            s.smart_migration_result = Some(Err(error));
            return;
        }
        Ok(false) => {
            s.smart_migration_result = Some(Ok(json!({"cancelled": true, "path": migration.from})));
            return;
        }
        Ok(true) => {}
    }
    let to = migration.to.clone();
    let (tx, rx) = mpsc::channel();
    let result = spawn_cleanup(&migration.to, move || {
        let (handled, failed) = prepared.cleanup(&task.handle());
        let _ = tx.send(json!({"path": to, "handled": handled, "failed": failed}));
    });
    match result {
        Ok(_) => {
            migration.phase = Phase::Cleaning(rx);
            s.smart_migration = Some(migration);
        }
        Err(error) => s.smart_migration_result = Some(Err(format!("the new folder is in use; source cleanup could not start: {error}"))),
    }
}

fn spawn_cleanup(_to: &std::path::Path, work: impl FnOnce() + Send + 'static) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    if crate::smart::tests_round4::fail_cleanup_spawn(_to) {
        return Err(std::io::Error::other("injected cleanup spawn failure"));
    }
    std::thread::Builder::new().name("lc-smart-move-cleanup".into()).spawn(work)
}
