//! When the browser host may consume queued drops. Kept out of the wasm-only `web` module so the
//! rule that keeps stored drops out of a library behind a foreground window is testable natively.

use lightcraft_ui_egui::LightcraftApp;

/// The browser host's two drop queues: file handles not yet read, and originals whose asynchronous
/// storage has finished and that wait to be imported. Taking either only through this trait keeps
/// the gate and the drain together.
pub trait QueuedDrops {
    type Dropped;
    type Pending;
    /// Whether a prompt or the blocking library window owns input right now.
    fn drops_wait(&self) -> bool;
    /// Record that all currently unread handles are held by this foreground barrier.
    fn defer_dropped(&mut self);
    fn take_dropped(&mut self) -> Vec<Self::Dropped>;
    /// Take fresh arrivals only, retaining handles protected by an earlier foreground barrier.
    fn take_unprotected_dropped(&mut self) -> Vec<Self::Dropped>;
    fn take_pending(&mut self) -> Vec<Self::Pending>;
}

/// A prompt or the blocking library window (over an already open library, or its own New Library
/// name prompt or asynchronous folder picker) owns input: leaves the dropped-file queue and the
/// originals still being stored for a later UI pass, instead of importing them behind the window.
/// With no library open and no dialog in front, the existing drop path is unchanged.
pub fn drops_wait(app: &LightcraftApp) -> bool {
    lightcraft_ui_egui::panels::notices::blocks_input(app) || lightcraft_ui_egui::panels::library_problem::blocks_file_drops(app)
}

/// One UI pass's queues, drained by [`drain_ready`].
pub struct Drained<D, P> {
    pub dropped: Vec<D>,
    /// `None` when new file handles were refused behind a dialog: the originals already stored
    /// stay queued for a later pass (as before), rather than importing behind that dialog.
    pub pending: Option<Vec<P>>,
}

/// The actual queue drain `WebApp` runs each UI pass: while [`drops_wait`] holds, neither queue is
/// touched. With an ordinary dialog open, only fresh handles are taken for refusal; earlier
/// barrier-held handles remain queued. If fresh handles are refused, stored originals are left
/// queued as well. Otherwise the existing stored-original drain policy is unchanged.
pub fn drain_ready<Q: QueuedDrops>(queues: &mut Q, dialog_open: bool) -> Option<Drained<Q::Dropped, Q::Pending>> {
    if queues.drops_wait() {
        queues.defer_dropped();
        return None;
    }
    let dropped = if dialog_open { queues.take_unprotected_dropped() } else { queues.take_dropped() };
    let pending = if !dropped.is_empty() && dialog_open { None } else { Some(queues.take_pending()) };
    Some(Drained { dropped, pending })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{QueuedDrops, drain_ready, drops_wait};
    use lightcraft_engine::Session;
    use lightcraft_ui_egui::panels::library_problem::LibraryProblem;
    use lightcraft_ui_egui::state::Dialog;
    use lightcraft_ui_egui::{LightcraftApp, Services};

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    type Asked = std::sync::Arc<std::sync::Mutex<Vec<std::sync::mpsc::Sender<Vec<String>>>>>;

    fn app_with_library(tag: &str) -> Result<(Scratch, LightcraftApp), String> {
        let dir = std::env::temp_dir().join(format!("lc-web-drop-gate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let scratch = Scratch(dir);
        let mut session = Session::new().with_fs();
        session.open_library(&scratch.0, false).map_err(|error| error.to_string())?;
        Ok((scratch, LightcraftApp::new(session, Services::default())))
    }

    /// Stands in for `WebApp`: the same shared orchestration over two real queues, so a test sees
    /// that a held window leaves both queues untouched, that the ready path drains both, and that
    /// a refused new drop keeps the already stored originals queued.
    struct FakeQueues {
        app: LightcraftApp,
        dropped: Vec<String>,
        protected: usize,
        pending: Vec<String>,
    }

    impl FakeQueues {
        fn new(app: LightcraftApp) -> Self {
            Self { app, dropped: vec!["dropped-photo".into()], protected: 0, pending: vec!["web/hash/photo.png".into()] }
        }
    }

    impl QueuedDrops for FakeQueues {
        type Dropped = String;
        type Pending = String;

        fn drops_wait(&self) -> bool {
            drops_wait(&self.app)
        }

        fn defer_dropped(&mut self) {
            self.protected = self.dropped.len();
        }

        fn take_dropped(&mut self) -> Vec<Self::Dropped> {
            self.protected = 0;
            std::mem::take(&mut self.dropped)
        }

        fn take_unprotected_dropped(&mut self) -> Vec<Self::Dropped> {
            self.dropped.split_off(self.protected.min(self.dropped.len()))
        }

        fn take_pending(&mut self) -> Vec<Self::Pending> {
            std::mem::take(&mut self.pending)
        }
    }

    #[test]
    fn stored_drops_wait_through_the_choosers_new_library_prompt_and_drain_after_it() {
        let (_dir, mut app) = app_with_library("chooser").expect("create chooser test library");
        let mut problem = LibraryProblem::choose_later(&app.ui.settings);
        if let Some(c) = problem.chooser.as_mut() {
            c.over = Some(app.session.library_generation());
        }
        app.library_problem = Some(problem);
        let mut queues = FakeQueues::new(app);

        // The list's own New Library name prompt hides the list but still owns the library.
        queues.app.ui.dialog = Some(Dialog::TextPrompt {
            title: "New Library".into(),
            hint: "Library name".into(),
            value: String::new(),
            command: "app.newLibrary".into(),
            params: serde_json::json!({}),
            key: "name".into(),
        });
        assert!(drain_ready(&mut queues, false).is_none(), "the name prompt holds both queues");
        assert_eq!((queues.dropped.len(), queues.pending.len()), (1, 1), "the queued drop survived the prompt");

        // Cancelling the prompt brings the list back; it still owns the drop until it goes.
        queues.app.ui.dialog = None;
        assert!(drain_ready(&mut queues, false).is_none(), "the list is back and still holds the queue");
        queues.app.library_problem = None;
        let got = drain_ready(&mut queues, false).expect("the queues should be ready");
        assert_eq!(got.dropped, vec!["dropped-photo".to_string()]);
        assert_eq!(got.pending, Some(vec!["web/hash/photo.png".to_string()]));
        assert!(queues.dropped.is_empty() && queues.pending.is_empty(), "both queues drained once the window was gone");
    }

    #[test]
    fn a_refused_new_drop_behind_a_dialog_keeps_stored_originals_queued() {
        let (_dir, app) = app_with_library("refused").expect("create refused-drop test library");
        // not held by the gate: the dialog alone refuses the newly dropped handles
        let mut queues = FakeQueues::new(app);
        let got = drain_ready(&mut queues, true).expect("the queues should be ready");
        assert_eq!(got.dropped, vec!["dropped-photo".to_string()]);
        assert_eq!(got.pending, None, "the refused drop must not drain originals behind the dialog");
        assert_eq!(queues.pending.len(), 1, "the earlier originals stayed queued");

        // Nothing new was dropped: originals read from an earlier drop still finish, as before.
        let got = drain_ready(&mut queues, true).expect("the queues should be ready");
        assert!(got.dropped.is_empty());
        assert_eq!(got.pending, Some(vec!["web/hash/photo.png".to_string()]));
        assert!(queues.pending.is_empty());
    }

    #[test]
    fn a_no_library_library_picker_holds_both_queues() {
        let mut app = LightcraftApp::new(Session::new().with_fs(), Services::default());
        let asked: Asked = Default::default();
        app.services.picker = Some(Box::new(move |_| {
            let (tx, rx) = std::sync::mpsc::channel();
            asked.lock().unwrap().push(tx);
            Ok(rx)
        }));
        // New Library's name prompt was confirmed; the asynchronous place picker is still open.
        assert_eq!(app.run("app.newLibrary", serde_json::json!({"name": "Queued"})).unwrap(), serde_json::Value::Null);
        assert!(app.session.library.is_none());
        app.library_problem = Some(LibraryProblem::choose_later(&app.ui.settings));
        let mut queues = FakeQueues::new(app);
        assert!(drain_ready(&mut queues, false).is_none(), "a library picker that will open one holds both queues");
        assert_eq!((queues.dropped.len(), queues.pending.len()), (1, 1), "nothing was drained while the picker is up");
    }

    #[test]
    fn a_no_library_window_lets_the_existing_drop_path_consume_the_queues() {
        let mut app = LightcraftApp::new(Session::with_demo(), Services::default());
        app.library_problem = Some(LibraryProblem::new("/no/such/library", "missing"));
        let mut queues = FakeQueues::new(app);
        assert!(!drops_wait(&queues.app), "with no library open the existing drop path is unchanged");
        let got = drain_ready(&mut queues, false).expect("the queues should be ready");
        assert_eq!(got.dropped, vec!["dropped-photo".to_string()]);
        assert_eq!(got.pending, Some(vec!["web/hash/photo.png".to_string()]));
        assert!(queues.dropped.is_empty() && queues.pending.is_empty(), "the host consumed both queues");

        queues.app.notices.push("The settings file could not be read and was reset.".into());
        queues.dropped.push("late-drop".into());
        queues.pending.push("web/hash/late.png".into());
        assert!(drain_ready(&mut queues, false).is_none(), "a prompt still owns input");
        assert_eq!((queues.dropped.len(), queues.pending.len()), (1, 1), "the prompt left both queues alone");
    }

    /// Uses the product's actual raw-handle queue and provenance APIs. Only the stored-original
    /// vector is host-owned; there is no test copy of protected-prefix bookkeeping or policy.
    struct RealAppQueues {
        h: lightcraft_ui_egui::headless::Headless,
        pending: Vec<String>,
        pending_takes: usize,
    }

    impl QueuedDrops for RealAppQueues {
        type Dropped = egui::DroppedFileHandle;
        type Pending = String;

        fn drops_wait(&self) -> bool {
            drops_wait(&self.h.app)
        }

        fn defer_dropped(&mut self) {
            self.h.app.defer_dropped_files();
        }

        fn take_dropped(&mut self) -> Vec<Self::Dropped> {
            self.h.app.take_dropped_files()
        }

        fn take_unprotected_dropped(&mut self) -> Vec<Self::Dropped> {
            self.h.app.take_unprotected_dropped_files()
        }

        fn take_pending(&mut self) -> Vec<Self::Pending> {
            self.pending_takes += 1;
            std::mem::take(&mut self.pending)
        }
    }

    #[derive(Debug)]
    struct UnreadDrop {
        path: std::path::PathBuf,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl egui::DroppedFile for UnreadDrop {
        fn path(&self) -> &std::path::Path {
            &self.path
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("the queue gate must not read dropped bytes".into())
        }
    }

    fn unread(path: &str) -> (egui::DroppedFileHandle, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handle = std::sync::Arc::new(UnreadDrop { path: path.into(), reads: reads.clone() });
        (handle, reads)
    }

    fn queue_raw(queues: &mut RealAppQueues, handles: &[egui::DroppedFileHandle]) {
        let mut raw = egui::RawInput { dropped_files: handles.to_vec(), ..Default::default() };
        queues.h.app.raw_input_hook(&queues.h.view.ctx, &mut raw);
        assert!(raw.dropped_files.is_empty(), "the product takes raw ownership once");
    }

    fn hold_library(queues: &mut RealAppQueues) -> Result<(), String> {
        let mut problem = LibraryProblem::choose_later(&queues.h.app.ui.settings);
        problem.chooser.as_mut().ok_or("the launch list has no chooser")?.over = Some(queues.h.app.session.library_generation());
        queues.h.app.library_problem = Some(problem);
        Ok(())
    }

    fn real_queues(tag: &str) -> Result<(Scratch, RealAppQueues), String> {
        let (dir, app) = app_with_library(tag)?;
        let h = lightcraft_ui_egui::headless::Headless::new(app, [1300.0, 900.0], 1.0);
        Ok((dir, RealAppQueues { h, pending: vec!["web/hash/stored.png".into()], pending_takes: 0 }))
    }

    fn protect(queues: &mut RealAppQueues, handle: &egui::DroppedFileHandle) -> Result<(), String> {
        hold_library(queues)?;
        queue_raw(queues, std::slice::from_ref(handle));
        // WebApp's gate runs before App.ui, including the cold first frame. The gate
        // itself must establish provenance; a warmed native UI pass cannot stand in for it.
        assert_eq!(queues.h.frames(), 0, "this starts before the first native UI pass");
        assert!(drops_wait(&queues.h.app));
        assert!(drain_ready(queues, false).is_none());
        assert_eq!(queues.h.frames(), 0, "the host gate protects ownership without drawing");
        assert_eq!(queues.pending_takes, 0);
        assert!(queues.h.app.scan.is_none() && queues.h.app.import.is_none());
        assert_eq!(queues.h.app.session.catalog.len(), 0);
        Ok(())
    }

    #[test]
    fn real_unread_barrier_drop_survives_a_dialog_while_stored_originals_finish() {
        let (_dir, mut queues) = real_queues("real-held-dialog").expect("create real-queue library");
        let (a, reads) = unread("A.png");
        protect(&mut queues, &a).expect("hold the library barrier");
        queues.h.app.library_problem = None;
        queues.h.app.ui.dialog = Some(Dialog::WhatsNew);
        // WebApp drains BEFORE App.ui. A native UI pass here would conceal the host-order bug.
        let got = drain_ready(&mut queues, true).expect("only the ordinary dialog remains");
        assert!(got.dropped.is_empty(), "the earlier unread A must not be drained for dialog refusal");
        assert_eq!(
            got.pending,
            Some(vec!["web/hash/stored.png".into()]),
            "with no fresh refusal, earlier stored originals retain their existing completion policy"
        );
        assert_eq!(queues.pending_takes, 1);
        let again = drain_ready(&mut queues, true).unwrap();
        assert!(again.dropped.is_empty(), "A stays protected across another host pass");
        queues.h.app.ui.dialog = None;
        let released = drain_ready(&mut queues, false).unwrap();
        assert_eq!(released.dropped.len(), 1);
        assert!(std::sync::Arc::ptr_eq(&released.dropped[0], &a), "the original A handle is released");
        assert!(drain_ready(&mut queues, false).unwrap().dropped.is_empty(), "A transfers exactly once");
        assert!(queues.h.app.take_dropped_files().is_empty());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn real_fresh_dialog_drop_is_refused_without_consuming_protected_or_stored_files() {
        let (_dir, mut queues) = real_queues("real-mixed-dialog").expect("create real-queue library");
        let (a, reads_a) = unread("A.png");
        let (b, reads_b) = unread("B.png");
        protect(&mut queues, &a).expect("hold the library barrier");
        queues.h.app.library_problem = None;
        queues.h.app.ui.dialog = Some(Dialog::WhatsNew);
        queue_raw(&mut queues, std::slice::from_ref(&b));
        let got = drain_ready(&mut queues, true).unwrap();
        assert_eq!(got.dropped.len(), 1, "only fresh B is offered to the browser's existing refusal branch");
        assert!(std::sync::Arc::ptr_eq(&got.dropped[0], &b));
        assert_eq!(got.pending, None, "refusing B must not consume stored originals");
        assert_eq!(queues.pending_takes, 0);
        assert_eq!(queues.pending, vec!["web/hash/stored.png".to_string()]);
        assert!(matches!(queues.h.app.ui.dialog, Some(Dialog::WhatsNew)));
        queues.h.app.ui.dialog = None;
        let released = drain_ready(&mut queues, false).unwrap();
        assert_eq!(released.dropped.len(), 1, "protected A survived B's refusal");
        assert!(std::sync::Arc::ptr_eq(&released.dropped[0], &a));
        assert_eq!(released.pending, Some(vec!["web/hash/stored.png".into()]));
        let empty = drain_ready(&mut queues, false).unwrap();
        assert!(empty.dropped.is_empty());
        assert_eq!(empty.pending, Some(Vec::<String>::new()));
        assert_eq!(reads_a.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(reads_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        // Consuming A clears its protection: a later ordinary drop is still fresh.
        let (c, reads_c) = unread("C.png");
        queues.h.app.ui.dialog = Some(Dialog::WhatsNew);
        queue_raw(&mut queues, std::slice::from_ref(&c));
        let fresh = drain_ready(&mut queues, true).unwrap();
        assert_eq!(fresh.dropped.len(), 1);
        assert!(std::sync::Arc::ptr_eq(&fresh.dropped[0], &c));
        assert_eq!(fresh.pending, None);
        queues.h.app.ui.dialog = None;
        assert!(drain_ready(&mut queues, false).unwrap().dropped.is_empty());
        assert_eq!(reads_c.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn real_protected_drops_keep_order_through_barrier_reentry_and_transfer_once() {
        let (_dir, mut queues) = real_queues("real-reentered").expect("create real-queue library");
        let (a, reads_a) = unread("A.png");
        let (b, reads_b) = unread("B.png");
        protect(&mut queues, &a).expect("hold the library barrier");
        queues.h.app.library_problem = None;
        queues.h.app.ui.dialog = Some(Dialog::WhatsNew);
        assert!(drain_ready(&mut queues, true).unwrap().dropped.is_empty());
        queues.h.app.ui.dialog = None;
        hold_library(&mut queues).expect("reenter the library barrier"); // Before a pass can consume A.
        queue_raw(&mut queues, std::slice::from_ref(&b));
        assert!(drain_ready(&mut queues, false).is_none(), "the reentered host gate protects B before native UI");
        assert_eq!(queues.h.frames(), 0, "neither protected arrival depends on font readiness or a completed UI pass");
        queues.h.app.library_problem = None;
        queues.h.app.ui.dialog = Some(Dialog::WhatsNew);
        assert!(drain_ready(&mut queues, true).unwrap().dropped.is_empty(), "both real barrier arrivals remain protected");
        queues.h.app.ui.dialog = None;
        let released = drain_ready(&mut queues, false).unwrap();
        assert_eq!(released.dropped.len(), 2);
        assert!(std::sync::Arc::ptr_eq(&released.dropped[0], &a));
        assert!(std::sync::Arc::ptr_eq(&released.dropped[1], &b));
        assert!(drain_ready(&mut queues, false).unwrap().dropped.is_empty());
        assert!(queues.h.app.take_dropped_files().is_empty());
        assert_eq!(reads_a.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(reads_b.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
