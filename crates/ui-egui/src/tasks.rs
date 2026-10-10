//! Background tasks for commands whose file-system work can take long on a slow or offline drive
//! (Find Missing Photos walking a folder, listing the auto-import folder…): the work runs on a
//! worker thread and its result is applied on the UI thread between frames, so the window keeps
//! answering. Imports, scans, exports and preview builds have their own tasks with progress. A task the user started
//! (Find Missing Photos) shows in the activity stack while it runs; quiet ones (the auto-import listing) don't.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use crate::DacApp;

/// Applies a task's result to the app (on the UI thread).
type Finish = Box<dyn FnOnce(&mut DacApp, &egui::Context) + Send>;

struct Task {
    label: String,
    rx: Receiver<Finish>,
    /// Its row in the activity stack (none for quiet tasks); goes with the task.
    _guard: Option<dac_engine::activity::TaskGuard>,
}

/// The background tasks in flight.
#[derive(Default)]
pub struct Tasks {
    running: Vec<Task>,
    /// Repaints the window when a task finishes (set every frame).
    pub(crate) repaint: Option<egui::Context>,
}

impl Tasks {
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// What is running (`ui.inspect` → `tasks`).
    pub fn labels(&self) -> Vec<String> {
        self.running.iter().map(|t| t.label.clone()).collect()
    }

    pub fn is_running(&self, label: &str) -> bool {
        self.running.iter().any(|t| t.label == label)
    }
}

/// Run `work` on a worker thread, then `done(app, ctx, result)` on the UI thread (in the browser
/// build, which has no threads, `work` runs at once and `done` on the next frame). With `kind`, the
/// activity stack shows `label` meanwhile, without a count and without ✕ (the work can't be stopped).
pub fn spawn<T: Send + 'static>(
    app: &mut DacApp,
    label: &str,
    kind: Option<&'static str>,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&mut DacApp, &egui::Context, T) + Send + 'static,
) -> Result<(), String> {
    let (tx, rx) = channel::<Finish>();
    let repaint = app.tasks.repaint.clone();
    let job = move || {
        let t = work();
        let _ = tx.send(Box::new(move |app: &mut DacApp, ctx: &egui::Context| done(app, ctx, t)));
        if let Some(c) = repaint {
            c.request_repaint();
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name(format!("lc-task-{label}")).spawn(job).map_err(|e| format!("{label}: could not start: {e}"))?;
    #[cfg(target_arch = "wasm32")]
    job();
    let guard = kind.map(|k| app.session.activity.start(k, label, dac_engine::activity::Cancel::No));
    app.tasks.running.push(Task { label: label.to_string(), rx, _guard: guard });
    Ok(())
}

/// Apply finished tasks (called every frame).
pub fn poll(app: &mut DacApp, ctx: &egui::Context) {
    app.tasks.repaint = Some(ctx.clone());
    let mut i = 0;
    while i < app.tasks.running.len() {
        match app.tasks.running[i].rx.try_recv() {
            Ok(finish) => {
                app.tasks.running.remove(i);
                finish(app, ctx);
            }
            Err(TryRecvError::Empty) => i += 1,
            Err(TryRecvError::Disconnected) => {
                // the worker died (a panic, already logged)
                let t = app.tasks.running.remove(i);
                app.toast(ctx, crate::i18n::tr_format!("{} failed", t.label));
            }
        }
    }
    if !app.tasks.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Wait for every task and apply it (tests, and commands asked to `wait`). `false` on timeout.
pub fn wait(app: &mut DacApp, ctx: &egui::Context, timeout: std::time::Duration) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let t0 = std::time::Instant::now();
        loop {
            poll(app, ctx);
            if app.tasks.is_empty() {
                return true;
            }
            if t0.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        // tasks ran inline: their results are already waiting
        let _ = timeout;
        poll(app, ctx);
        app.tasks.is_empty()
    }
}
