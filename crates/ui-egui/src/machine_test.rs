//! "Test my machine" in the app (Settings ▸ Performance): the benchmark
//! ([`lightcraft_engine::machine_test`]) runs on a worker thread with a row in the activity stack
//! (✕ stops it); when it ends, its choice (photos side by side, the GPU for exports, exports
//! sharing the GPU) goes into the app's settings, which belong to this install, not a library,
//! and its report is shown under the button.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::machine_test::{MachineReport, TestOptions};

use crate::LightcraftApp;
use crate::state::AppSettings;

/// The test while it runs.
pub struct MachineTask {
    rx: Receiver<Result<MachineReport, String>>,
    /// Its row in the activity stack (its ✕ stops the test).
    _guard: TaskGuard,
}

/// Make the export settings take effect (lanes, the GPU for exports, the GPU's share).
pub fn apply(st: &AppSettings) {
    lightcraft_engine::export::set_export_lanes(Some(st.export_lanes as usize).filter(|n| *n > 0));
    lightcraft_engine::export::set_export_gpu(!st.export_cpu_only);
    lightcraft_engine::gpu::set_batch_limits(
        Some(u64::from(st.gpu_export_mb) << 20).filter(|b| *b > 0),
        Some(st.gpu_export_jobs as usize).filter(|j| *j > 0),
    );
}

/// Store what a finished test chose in `st` (the user's own choices are replaced by it).
pub fn adopt(st: &mut AppSettings, r: &MachineReport) {
    st.export_lanes = r.lanes.min(u32::MAX as usize) as u32;
    st.export_cpu_only = !r.use_gpu && r.gpu.is_some();
    st.gpu_export_jobs = r.gpu_jobs.min(u32::MAX as usize) as u32;
    st.gpu_export_mb = 0;
    st.machine_test = Some(r.clone());
}

/// Raw photos of the library to test with (the newest few that are on disk): the user's own
/// camera's decode is part of an export. Empty: the test uses the demo photos.
fn sample_paths(app: &LightcraftApp, n: usize) -> Vec<String> {
    let mut v: Vec<String> = app
        .session
        .catalog
        .photos()
        .filter(|p| p.kind == lightcraft_engine::catalog::MediaKind::Raw)
        .filter_map(|p| match &p.source {
            lightcraft_engine::catalog::Source::File { path } if std::path::Path::new(path).is_file() => Some(path.clone()),
            _ => None,
        })
        .collect();
    let keep = v.len().saturating_sub(n);
    v.drain(..keep);
    v
}

/// Whether the test is running.
pub fn running(app: &LightcraftApp) -> bool {
    app.machine.is_some()
}

/// Start the test (nothing when it already runs or an export is running: it would measure the
/// export too).
pub fn start(app: &mut LightcraftApp) -> Result<(), String> {
    if app.machine.is_some() {
        return Err(crate::i18n::tr("The test is already running").to_string());
    }
    if app.export.is_some() {
        return Err(crate::i18n::tr("Wait for the export to finish, then test this computer").to_string());
    }
    let paths = sample_paths(app, 4);
    let guard = app.session.activity.start("benchmark", "Testing this computer", Cancel::Yes);
    let cancel = guard.cancel_flag();
    let row = guard.handle();
    let (tx, rx) = channel();
    let work = move || {
        let r = run(paths, &mut |done, total, what| {
            row.progress(done as u64, total as u64);
            row.detail(what);
            !cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
        });
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("machine-test".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    app.machine = Some(MachineTask { rx, _guard: guard });
    Ok(())
}

fn run(paths: Vec<String>, progress: &mut dyn FnMut(usize, usize, &str) -> bool) -> Result<MachineReport, String> {
    let what = || "the test".to_string();
    lightcraft_engine::guard::catch(&what(), || lightcraft_engine::machine_test::run_test(&TestOptions { paths, ..Default::default() }, progress))
        .and_then(|r| r)
}

/// When the test has ended: keep and use its choice, and say what it found.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = &app.machine else { return };
    let r = match task.rx.try_recv() {
        Ok(r) => r,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the test stopped unexpectedly".into()),
    };
    app.machine = None;
    match r {
        Ok(report) => {
            adopt(&mut app.ui.settings, &report);
            apply(&app.ui.settings);
            app.toast_for(ctx, crate::i18n::tr(&report.summary).to_string(), 12.0);
        }
        Err(e) if e == "cancelled" => app.toast(ctx, crate::i18n::tr("Test stopped: the settings are as they were.").to_string()),
        Err(e) => app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Test my machine failed"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_becomes_the_install_settings() {
        let mut st = AppSettings::default();
        let r = MachineReport {
            lanes: 8,
            use_gpu: false,
            gpu: Some("Card".into()),
            gpu_jobs: 2,
            summary: "Exports about 9 photos a minute".into(),
            ..Default::default()
        };
        adopt(&mut st, &r);
        assert_eq!((st.export_lanes, st.export_cpu_only, st.gpu_export_jobs, st.gpu_export_mb), (8, true, 2, 0));
        assert_eq!(st.machine_test.as_ref().map(|m| m.summary.as_str()), Some("Exports about 9 photos a minute"));
        // no GPU at all: not "processor only", which would read as a choice
        adopt(&mut st, &MachineReport { lanes: 4, use_gpu: false, gpu: None, ..Default::default() });
        assert!(!st.export_cpu_only);
        // kept across restarts with the other app settings, and older files without them read
        let json = serde_json::to_value(&st).unwrap();
        let back: AppSettings = serde_json::from_value(json).unwrap();
        assert_eq!(back.export_lanes, 4);
        let old: AppSettings = serde_json::from_value(serde_json::json!({"gpu": true, "machineTest": "garbage", "exportLanes": "x"})).unwrap();
        assert_eq!((old.export_lanes, old.machine_test.is_none()), (0, true));
    }
}
