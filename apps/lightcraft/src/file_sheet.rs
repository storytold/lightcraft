//! The commands' file dialogs as sheets of the window on macOS, made on the UI thread (#3735).
//!
//! rfd makes its `NSOpenPanel` on the main thread, and called from any other thread it sends that
//! work to the main queue with a blocking `dispatch_sync`. The dialogs of
//! [`lightcraft_ui_egui::pick`] (#191) were started from a thread of their own, so every one of
//! them went that way. On macOS 26.6 none came up: the reporter's `sample` has the main thread
//! inside `+[NSOpenPanel openPanel]`, which does not return, and the picker thread waiting in
//! `dispatch_sync` for it, and the window never answered again.
//!
//! [`start`] makes the dialog on the thread that asks instead, the UI thread, from inside a frame.
//! With rfd's async dialog and the window as its parent the dialog is a sheet of the window:
//! making it neither blocks nor sends anything to the main queue, and a thread of its own only
//! waits for the answer. A sheet is the window's own, so the UI shows one at a time
//! (`Services::picker_one_at_a_time`).

use lightcraft_ui_egui::pick::PickRequest;
use std::pin::Pin;
use std::sync::mpsc::{Receiver, channel};
use std::task::{Context, Poll, Waker};

/// A dialog that is up, as the future of its answer: the chosen paths, none when it was canceled.
pub(crate) type Answer = Pin<Box<dyn Future<Output = Vec<String>> + Send>>;

/// What making a dialog gave.
enum Shown {
    /// On screen; the answer is still to come.
    Up(Answer),
    /// Answered already.
    Done(Vec<String>),
}

/// Show `request` on this thread, through `present`, and answer through the channel of a
/// [`Picker`](lightcraft_ui_egui::pick::Picker). `present` makes the dialog and returns the
/// future of its answer; `repaint` wakes the UI when that is in.
///
/// Making the dialog is what has to happen here: it is where rfd sends work to the main queue
/// from any other thread. So a thread of its own only awaits the answer, and the future gets its
/// first poll here as well, which is the call that would make a dialog that waited for one.
/// Nothing but Rust frames lies between `present` and this function, so a panic while the panel
/// is made (a nil `NSOpenPanel` from AppKit, #1996) unwinds to here and ends as a canceled dialog
/// and a logged error, not an abort. `Err` is `present` refusing: showing the dialog some other
/// way is the caller's.
pub(crate) fn start(
    repaint: &egui::Context,
    request: PickRequest,
    present: impl FnOnce(&PickRequest) -> Result<Answer, String>,
) -> Result<Receiver<Vec<String>>, String> {
    let (tx, rx) = channel();
    let shown = lightcraft_engine::guard::catch("the file dialog", || -> Result<Shown, String> {
        let mut answer = present(&request)?;
        Ok(match answer.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(paths) => Shown::Done(paths),
            Poll::Pending => Shown::Up(answer),
        })
    });
    match shown {
        Ok(Ok(Shown::Up(answer))) => {
            let waiting = repaint.clone();
            let thread = std::thread::Builder::new().name("file-dialog".into()).spawn(move || {
                let paths = pollster::block_on(answer);
                // the app may have quit meanwhile: nobody to tell
                let _ = tx.send(paths);
                waiting.request_repaint();
            });
            if let Err(e) = thread {
                // the sheet stays up with nobody to hear it and the dropped sender ends the request, so the next
                // waiting request can show over it: a known limit, for a thread that can't be started
                log::error!("could not start the thread that waits for the file dialog: {e}");
            }
        }
        Ok(Ok(Shown::Done(paths))) => {
            let _ = tx.send(paths);
            repaint.request_repaint();
        }
        Ok(Err(refused)) => return Err(refused),
        Err(_panicked) => {
            // `catch` has logged it
            let _ = tx.send(Vec::new());
            repaint.request_repaint();
        }
    }
    Ok(rx)
}

/// The dialogs as rfd's sheets of one window.
#[cfg(target_os = "macos")]
pub(crate) struct Sheets {
    /// An async dialog with the window as its parent; each request gets a copy.
    window: rfd::AsyncFileDialog,
}

#[cfg(target_os = "macos")]
impl Sheets {
    /// `window`: an async dialog whose parent is the app's window.
    pub(crate) fn new(window: rfd::AsyncFileDialog) -> Self {
        Sheets { window }
    }

    /// The host's [`Picker`](lightcraft_ui_egui::pick::Picker): each request is a sheet.
    pub(crate) fn picker(self, repaint: egui::Context) -> lightcraft_ui_egui::pick::Picker {
        Box::new(move |request| start(&repaint, request, |request| self.present(request)))
    }

    /// Show the sheet for `request` and return the future of its answer. rfd's async dialogs are
    /// plain functions, not `async fn`s: the sheet is up when they return. On any thread but the
    /// main one rfd would hand its work to the main queue with the blocking dispatch that hangs
    /// on macOS 26.6, so this refuses.
    fn present(&self, request: &PickRequest) -> Result<Answer, String> {
        use crate::dialog_filter::FileDialogExt as _;
        use lightcraft_ui_egui::pick::PickKind;
        if objc2::MainThreadMarker::new().is_none() {
            return Err("a file dialog can only be shown from the main thread".into());
        }
        let mut dialog = self.window.clone();
        if let Some(title) = &request.title {
            dialog = dialog.set_title(title);
        }
        if let Some((name, extensions)) = &request.filter {
            dialog = dialog.add_filter_nocase(name.clone(), extensions);
        }
        if let Some(name) = &request.file_name {
            dialog = dialog.set_file_name(name);
        }
        let text = |file: &rfd::FileHandle| file.path().to_string_lossy().to_string();
        let answer: Answer = match request.kind {
            PickKind::Files => {
                let sheet = dialog.pick_files();
                Box::pin(async move { sheet.await.unwrap_or_default().iter().map(text).collect::<Vec<String>>() })
            }
            PickKind::File => {
                let sheet = dialog.pick_file();
                Box::pin(async move { sheet.await.iter().map(text).collect::<Vec<String>>() })
            }
            PickKind::Folder => {
                let sheet = dialog.pick_folder();
                Box::pin(async move { sheet.await.iter().map(text).collect::<Vec<String>>() })
            }
            PickKind::Save => {
                let sheet = dialog.save_file();
                Box::pin(async move { sheet.await.iter().map(text).collect::<Vec<String>>() })
            }
        };
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, ThreadId};
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(10);

    /// A dialog that stays up until the test closes it, and notes which threads made and polled it.
    #[derive(Default)]
    struct Sheet {
        state: Mutex<SheetState>,
    }

    #[derive(Default)]
    struct SheetState {
        answer: Option<Vec<String>>,
        waker: Option<Waker>,
        made_on: Option<ThreadId>,
        polled_on: Vec<ThreadId>,
    }

    impl Sheet {
        /// The user closes the sheet with `paths` chosen (none: canceled).
        fn close(&self, paths: &[&str]) {
            let mut state = self.state.lock().unwrap();
            state.answer = Some(paths.iter().map(|p| p.to_string()).collect());
            if let Some(waker) = state.waker.take() {
                waker.wake();
            }
        }

        fn made_on(&self) -> Option<ThreadId> {
            self.state.lock().unwrap().made_on
        }

        fn polled_on(&self) -> Vec<ThreadId> {
            self.state.lock().unwrap().polled_on.clone()
        }
    }

    /// What a held `Sheet` answers with.
    struct Answering(Arc<Sheet>);

    impl Future for Answering {
        type Output = Vec<String>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Vec<String>> {
            let mut state = self.0.state.lock().unwrap();
            state.polled_on.push(thread::current().id());
            match state.answer.take() {
                Some(paths) => Poll::Ready(paths),
                None => {
                    state.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
    }

    /// A context that counts the repaints asked of it: a fresh one starts out wanting a frame, so
    /// its own state says nothing about whether the host asked.
    fn counting() -> (egui::Context, Arc<AtomicUsize>) {
        let ctx = egui::Context::default();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        ctx.set_repaint_observer(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        (ctx, count)
    }

    /// A dialog started through `start`, held up by a `Sheet`.
    fn held(ctx: &egui::Context) -> (Arc<Sheet>, Receiver<Vec<String>>) {
        let sheet = Arc::new(Sheet::default());
        let made = sheet.clone();
        let rx = start(ctx, PickRequest::folder("Open Library"), move |_| {
            made.state.lock().unwrap().made_on = Some(thread::current().id());
            Ok(Box::pin(Answering(made.clone())))
        })
        .unwrap();
        (sheet, rx)
    }

    /// The UI is told to look once the answer is in: the waiting thread repaints after it sent.
    fn told_to_repaint(count: &AtomicUsize) -> bool {
        (0..1000).any(|_| {
            thread::sleep(Duration::from_millis(10));
            count.load(Ordering::SeqCst) > 0
        })
    }

    #[test]
    fn the_dialog_is_made_and_first_polled_where_it_is_asked_for_and_awaited_by_another_thread() {
        let (ctx, _) = counting();
        let asking = thread::current().id();
        let (sheet, rx) = held(&ctx);
        assert_eq!(sheet.made_on(), Some(asking), "made here");
        assert_eq!(sheet.polled_on().first(), Some(&asking), "its first poll, which would make a lazy dialog, is here too");
        // up, and the asking thread is free: nothing has answered
        assert!(rx.try_recv().is_err());
        sheet.close(&["/photos/Sunset"]);
        assert_eq!(rx.recv_timeout(WAIT), Ok(vec!["/photos/Sunset".to_string()]));
        let polls = sheet.polled_on();
        assert!(polls.len() >= 2 && polls[1..].iter().all(|t| *t != asking), "the rest is awaited elsewhere: {polls:?}");
    }

    #[test]
    fn what_is_chosen_arrives_whole_and_wakes_the_ui() {
        let (ctx, repaints) = counting();
        let (sheet, rx) = held(&ctx);
        assert_eq!(repaints.load(Ordering::SeqCst), 0, "no frame is asked for while the sheet is up");
        sheet.close(&["/photos/a.nef", "/photos/b.nef"]);
        assert_eq!(rx.recv_timeout(WAIT), Ok(vec!["/photos/a.nef".to_string(), "/photos/b.nef".to_string()]));
        assert!(told_to_repaint(&repaints));
    }

    #[test]
    fn a_canceled_dialog_answers_with_nothing() {
        let (ctx, repaints) = counting();
        let (sheet, rx) = held(&ctx);
        sheet.close(&[]);
        assert_eq!(rx.recv_timeout(WAIT), Ok(Vec::new()));
        assert!(told_to_repaint(&repaints));
    }

    #[test]
    fn a_panic_while_the_panel_is_made_is_a_canceled_dialog_not_an_abort() {
        let (ctx, repaints) = counting();
        let rx = start(&ctx, PickRequest::folder("Open Library"), |_| -> Result<Answer, String> {
            panic!("unexpected NULL returned from +[NSOpenPanel openPanel]")
        })
        .unwrap();
        assert_eq!(rx.try_recv(), Ok(Vec::new()), "answered at once: canceled");
        assert_eq!(repaints.load(Ordering::SeqCst), 1, "the UI is told, so the request is forgotten");
    }

    #[test]
    fn a_dialog_the_host_refuses_is_left_to_the_caller() {
        let (ctx, repaints) = counting();
        let refused = start(&ctx, PickRequest::folder("Open Library"), |_| Err("not here".to_string()));
        assert_eq!(refused.err(), Some("not here".to_string()));
        assert_eq!(repaints.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_answer_that_is_in_when_the_dialog_is_made_is_delivered_at_once_by_the_asking_thread() {
        let (ctx, repaints) = counting();
        let sheet = Arc::new(Sheet::default());
        sheet.close(&["/photos"]);
        let made = sheet.clone();
        let rx = start(&ctx, PickRequest::folder("Open Library"), move |_| Ok(Box::pin(Answering(made)))).unwrap();
        assert_eq!(rx.try_recv(), Ok(vec!["/photos".to_string()]));
        assert_eq!(repaints.load(Ordering::SeqCst), 1);
        assert_eq!(sheet.polled_on(), [thread::current().id()], "polled once, here: no thread waits for an answer that is in");
    }

    #[test]
    fn a_request_nobody_waits_for_any_more_ends_quietly() {
        let (ctx, repaints) = counting();
        let (sheet, rx) = held(&ctx);
        drop(rx);
        sheet.close(&["/photos"]);
        // the waiting thread got as far as telling the UI, so sending to nobody did not stop it
        assert!(told_to_repaint(&repaints));
    }

    /// A test thread is not the main thread: rfd would send its panel to the main queue with a
    /// blocking dispatch, which is the hang. The call runs on a thread of its own so that a
    /// regression fails the test instead of hanging it.
    #[cfg(target_os = "macos")]
    #[test]
    fn rfd_is_not_asked_off_the_main_thread() {
        let (tx, rx) = channel();
        thread::spawn(move || {
            let sheets = Sheets::new(rfd::AsyncFileDialog::new());
            let _ = tx.send(sheets.present(&PickRequest::folder("Open Library")).err());
        });
        let refused = rx.recv_timeout(Duration::from_secs(5)).expect("the call came back instead of waiting on the main queue");
        assert!(refused.is_some_and(|why| why.contains("main thread")), "refused");
    }
}
