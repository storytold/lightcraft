//! The tracked print behind the network destination.
//!
//! One press of Print is one `craft_print::PrintJob` (the lifecycle record of the whole print,
//! stable across retries), owned by a worker thread that calls `PrintJob::advance(&Registry)` when it
//! is due (at most one bounded request per call, the library's `Staleness` defaults) and publishes
//! a `JobView` plus the few facts a status line needs. The UI thread only reads that publication
//! and sends commands (cancel, retry, reconcile, refresh, adopt a found job); it never blocks.
//!
//! craft-ui's own job model (`printing::network`) is fed from the same worker: `Accepted` when the
//! printer has the document, `Status` as the printer reports, and the result when the print settles
//! (ended, needs a person, or accepted with nothing to follow). What happens afterwards (retry,
//! reconcile) is shown by the status line below the dialog's section and, with the dialog closed,
//! by the small status row.
//!
//! The sentences are catalog messages with `{name}` holes ([`fill`]); no address, URL or id ever reaches one.

use std::sync::{Arc, Mutex, PoisonError, mpsc};

use crate::i18n::tr;
use std::time::{Duration, Instant};

use craft_ui::printing::network::{Failure, FailureKind, JobHandle, JobUpdate, Outcome, Progress, Report, Stage};
use lightcraft_print::network::Station;
use lightcraft_print::network::{
    AttemptError, CancelToken, ErrorKind, JobState, JobView, LifecycleState, Phase, PrintJob, PrintOutcome, RetryAdvice, RetrySafety, SimpleState,
    Snapshot, StaleReason, TrackedReconciled,
};

/// What the person can ask of a running or settled print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Cancel,
    Retry,
    Reconcile,
    /// Read the status now (resumes following a print that went quiet).
    Refresh,
    /// Adopt the job that a reconcile found as this print's job.
    AcceptFound,
}

/// What the worker publishes after each step.
#[derive(Clone)]
pub(crate) struct TrackedView {
    pub view: JobView,
    pub lifecycle: LifecycleState,
    pub advice: RetryAdvice,
    pub reconciled: Option<TrackedReconciled>,
    pub stale: Option<StaleReason>,
    /// The last thing a command answered ("Retry refused: ...", what a reconcile found).
    pub note: Option<String>,
    /// What the printer calls itself (empty before a query read it). Never its address.
    pub name: String,
    /// The WS-Eventing listener, when `LIGHTCRAFT_TEST_EVENTS_BIND (or the legacy PDFCRAFT_TEST_EVENTS_BIND)` turned it on.
    pub events: Option<String>,
    pub token: CancelToken,
}

/// The UI side of one tracked print.
pub(crate) struct Tracked {
    shared: Arc<Mutex<Option<TrackedView>>>,
    commands: mpsc::Sender<Command>,
}

impl Tracked {
    pub(crate) fn view(&self) -> Option<TrackedView> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub(crate) fn send(&self, command: Command) {
        if command == Command::Cancel
            && let Some(view) = self.view()
        {
            // Wakes a send that is blocked in the backend at its next checkpoint.
            view.token.cancel();
        }
        let _ = self.commands.send(command);
    }
}

/// The slot the dialog and the status row read; a new print replaces (and so stops) the old one.
pub(crate) type Slot = Arc<Mutex<Option<Tracked>>>;

/// A message with its `{name}` holes filled in one pass (a value that looks like a hole is left as it is).
pub(crate) fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(rest.get(..start).unwrap_or(""));
        let tail = rest.get(start..).unwrap_or("");
        match tail.find('}') {
            Some(end) => {
                let key = tail.get(1..end).unwrap_or("");
                match args.iter().find(|(k, _)| *k == key) {
                    Some((_, value)) => out.push_str(value),
                    None => out.push_str(tail.get(..=end).unwrap_or("")),
                }
                rest = tail.get(end + 1..).unwrap_or("");
            }
            None => {
                out.push_str(tail);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// What the status lines call the printer: its display name, else "the printer". The name is what the
/// printer reported about itself; an address, a URL or an id-looking text is never used (it reads as no name).
pub(crate) fn printer_label(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim();
    if name.is_empty() || name.split_whitespace().any(looks_like_address) { "the printer".to_owned() } else { name.to_owned() }
}

/// Is the label a real name, rather than the fallback?
fn is_named(label: &str) -> bool {
    label != "the printer"
}

/// A word that is an address, a URL or a device id: not for the screen.
fn looks_like_address(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if lower.contains("://") || lower.contains("urn:") || lower.contains("uuid:") {
        return true;
    }
    let w = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let groups: Vec<usize> = w.split('-').map(str::len).collect();
    if groups == [8, 4, 4, 4, 12] && w.chars().all(|c| c == '-' || c.is_ascii_hexdigit()) {
        return true;
    }
    let host = w.split(':').next().unwrap_or(w);
    host.split('.').count() == 4 && host.split('.').all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// A line the library or the backend wrote, with every address, URL and id in it replaced by the printer's label.
pub(crate) fn scrubbed(text: &str, label: &str) -> String {
    text.split(' ').map(|word| if looks_like_address(word) { label } else { word }).collect::<Vec<_>>().join(" ")
}

/// How the print is going, in a sentence, from the `JobView` alone. `name`: the printer's display name
/// (empty when none is known); the sentence says which printer it is about.
pub(crate) fn status_text(view: &JobView, name: &str) -> String {
    let remote = view.id.backend() != "local";
    let who = printer_label(name);
    let named = is_named(&who);
    let with = |template: &str| fill(tr(template), &[("printer", &who)]);
    let mut text = match view.state {
        JobState::Pending if !remote && view.attempts > 1 => {
            fill(tr("Sending to {printer} (attempt {attempt})"), &[("printer", &who), ("attempt", &view.attempts.to_string())])
        }
        JobState::Pending if !remote => with("Sending to {printer}"),
        JobState::Pending => with("Accepted by {printer}: waiting in its queue"),
        JobState::Processing => with("Printing on {printer}"),
        JobState::Held => with("Held by {printer}"),
        JobState::Completed if view.reasons.is_empty() => with("Printed on {printer}"),
        JobState::Completed => with("Printed on {printer}, with warnings"),
        JobState::CompletedWithErrors if named => with("{printer} finished with errors"),
        JobState::CompletedWithErrors => tr("The printer finished with errors").to_owned(),
        JobState::Cancelled => with("Cancelled on {printer}"),
        JobState::Aborted if named => with("{printer}: the printer or the send failed"),
        JobState::Aborted => tr("The printer or the send failed").to_owned(),
        JobState::Abandoned if named => with("No outcome was heard from {printer}; it may still print it"),
        JobState::Abandoned => tr("No outcome was heard; the printer may still print it").to_owned(),
        JobState::NeedsAttention => with("Needs attention on {printer}"),
        _ if named => with("Sent to {printer}; it cannot report progress"),
        _ => tr("Sent; the printer cannot report progress").to_owned(),
    };
    if let Some(progress) = view.progress {
        match progress.total {
            Some(total) if total > 0 => {
                let (done, total) = (progress.done.min(total).to_string(), total.to_string());
                text.push(' ');
                text.push_str(&fill(tr("({done} of {total} sheets)"), &[("done", &done), ("total", &total)]));
            }
            _ if progress.done > 0 => {
                text.push(' ');
                text.push_str(&fill(tr("({done} sheets so far)"), &[("done", &progress.done.to_string())]));
            }
            _ => {}
        }
    }
    if !view.reasons.is_empty() {
        text.push_str(&format!(" [{}]", view.reasons.join(", ")));
    }
    if let Some(error) = &view.last_error
        && !matches!(view.state, JobState::Completed)
    {
        text.push_str(&format!(": {}", scrubbed(&error.message, &who)));
    }
    text
}

/// What to do next, in words, from the retry advice. `name`: the printer's display name.
pub(crate) fn advice_text(advice: RetryAdvice, lifecycle: LifecycleState, name: &str) -> Option<String> {
    let in_flight = matches!(
        lifecycle,
        LifecycleState::Preparing | LifecycleState::Submitting | LifecycleState::Watching(_) | LifecycleState::Terminal(PrintOutcome::Completed)
    );
    if in_flight {
        return None;
    }
    let who = printer_label(name);
    match advice {
        RetryAdvice::RetryNow => Some(fill(tr("Nothing was printed on {printer}; it is safe to retry."), &[("printer", &who)])),
        RetryAdvice::RetryAfter(wait) => {
            Some(fill(tr("Sending to {printer} again in {seconds} s."), &[("printer", &who), ("seconds", &wait.as_secs().max(1).to_string())]))
        }
        RetryAdvice::CheckDeviceFirst => {
            Some(fill(tr("Check {printer} before sending again; it may have printed or may still print."), &[("printer", &who)]))
        }
        _ => None,
    }
}

/// The line for a print that went quiet or stalled, naming the printer; `None` when it has not.
pub(crate) fn stale_text(stale: Option<StaleReason>, name: &str) -> Option<String> {
    let who = printer_label(name);
    let template = match stale? {
        StaleReason::NoNews => "Waiting for {printer}: it has not answered for a while. Use Check now.",
        StaleReason::Stalled => "Waiting for {printer}: it keeps reporting the same thing. Use Check now.",
        StaleReason::Abandoned => "No more news from {printer}; it may still print this.",
        _ => "Waiting for {printer}.",
    };
    Some(fill(tr(template), &[("printer", &who)]))
}

/// Which buttons make sense for this state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Buttons {
    pub cancel: bool,
    pub retry: bool,
    pub reconcile: bool,
    pub refresh: bool,
    pub accept: bool,
}

pub(crate) fn buttons(view: &TrackedView) -> Buttons {
    use PrintOutcome as P;
    let ended = matches!(view.lifecycle, LifecycleState::Terminal(_));
    Buttons {
        cancel: !ended,
        retry: matches!(
            view.lifecycle,
            LifecycleState::NeedsAttention
                | LifecycleState::RetryScheduled { .. }
                | LifecycleState::Terminal(P::Failed | P::Aborted | P::Cancelled | P::Abandoned)
        ),
        reconcile: view.lifecycle == LifecycleState::NeedsAttention && view.stale.is_none(),
        refresh: matches!(view.lifecycle, LifecycleState::Accepted | LifecycleState::Watching(_))
            || (view.lifecycle == LifecycleState::NeedsAttention && view.stale.is_some()),
        accept: matches!(view.reconciled, Some(TrackedReconciled::Found(_))) && view.lifecycle == LifecycleState::NeedsAttention,
    }
}

fn progress_of(s: &Snapshot) -> Progress {
    Progress {
        stage: match s.simple {
            SimpleState::Queued => Stage::Waiting,
            SimpleState::Held => Stage::Held,
            SimpleState::Printing => Stage::Printing,
            SimpleState::PrinterNeedsAttention => Stage::Stopped,
            _ => Stage::Accepted,
        },
        sheets_done: s.progress.sheets_done,
        sheets_total: s.progress.sheets_total,
        notes: s.reasons.clone(),
    }
}

/// The failure kind craft-ui words, from the typed attempt error.
pub(crate) fn failure_of(e: &AttemptError, open: bool) -> Failure {
    failure_named(e, open, "the printer")
}

/// [`failure_of`], with the printer's label in place of any address the message carries.
fn failure_named(e: &AttemptError, open: bool, label: &str) -> Failure {
    let kind = if e.native.is_some_and(|n| n.domain == "wsd-fault") {
        FailureKind::Fault
    } else if e.phase == Some(Phase::Prepare) && e.kind == ErrorKind::Failed {
        FailureKind::Encode
    } else {
        match e.kind {
            ErrorKind::InvalidRequest if open => FailureKind::Policy,
            ErrorKind::InvalidRequest | ErrorKind::ConsentRequired => FailureKind::Invalid,
            ErrorKind::UnsupportedFormat => FailureKind::NoCommonFormat,
            ErrorKind::Unsupported => FailureKind::Unsupported,
            ErrorKind::Unavailable | ErrorKind::Timeout | ErrorKind::Busy | ErrorKind::Io => FailureKind::Transport,
            ErrorKind::Failed => FailureKind::Protocol,
            ErrorKind::Rejected | ErrorKind::NotFound => FailureKind::Fault,
            ErrorKind::Cancelled => FailureKind::Stopped,
            _ => FailureKind::Other,
        }
    };
    Failure::new(kind, scrubbed(&e.message, label)).with_outcome_unknown(e.safety == RetrySafety::OutcomeUnknown)
}

/// craft-ui's result for a print that settled; `None` while it goes on by itself.
fn result_of(s: &Snapshot, printer: &str) -> Option<Result<Report, Failure>> {
    if !s.settled {
        return None;
    }
    let mut notes = s.reasons.clone();
    notes.extend(s.warnings.iter().cloned());
    let report = |outcome| {
        Some(Ok(Report { outcome, printer: printer.to_owned(), sheets: s.progress.sheets_done.map_or(0, u64::from), notes: notes.clone() }))
    };
    match s.state {
        LifecycleState::Terminal(PrintOutcome::Completed) => report(Outcome::Completed),
        LifecycleState::Terminal(PrintOutcome::CompletedWithWarnings) => report(Outcome::CompletedWithWarnings),
        LifecycleState::Terminal(PrintOutcome::Cancelled) => report(Outcome::Canceled),
        LifecycleState::Terminal(PrintOutcome::Aborted) => report(Outcome::Aborted),
        LifecycleState::Terminal(PrintOutcome::Abandoned) => report(Outcome::StillRunning),
        LifecycleState::Terminal(PrintOutcome::Failed) if s.view().state == JobState::CompletedWithErrors => report(Outcome::CompletedWithErrors),
        LifecycleState::Terminal(PrintOutcome::Failed) => Some(Err(match &s.last_error {
            Some(error) => failure_named(error, false, printer),
            None => Failure::new(FailureKind::Other, "the print failed"),
        })),
        LifecycleState::NeedsAttention if s.stale.is_some() => report(Outcome::StillRunning),
        LifecycleState::NeedsAttention => {
            let detail = s.last_error.as_ref().map_or_else(|| "the outcome of the send is not known".to_owned(), |e| e.message.clone());
            Some(Err(Failure::new(FailureKind::Transport, scrubbed(&detail, printer)).with_outcome_unknown(true)))
        }
        LifecycleState::Accepted => report(Outcome::AcceptedUnknown),
        _ => None,
    }
}

/// Start the worker for `job` on `station` and return craft-ui's handle for it. The tracked side
/// (view and commands) is stored in `slot`, replacing the previous print's, whose worker then stops.
pub(crate) fn spawn(station: Arc<Station>, job: PrintJob, slot: &Slot, ctx: Option<egui::Context>) -> JobHandle {
    let (result_tx, result_rx) = mpsc::channel();
    let (update_tx, update_rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel();
    let shared = Arc::new(Mutex::new(None));
    let tracked = Tracked { shared: Arc::clone(&shared), commands: command_tx.clone() };
    let token = job.cancel_token();
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(tracked);
    let failed = result_tx.clone();
    let spawned = std::thread::Builder::new()
        .name("print-job".into())
        .spawn(move || run(&station, job, &shared, &command_rx, &update_tx, &result_tx, ctx.as_ref()));
    if let Err(e) = spawned {
        // Nothing was sent: the dialog shows the failure, and no status row is left for a print that never ran.
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let _ = failed.send(Err(Failure::new(FailureKind::Other, e.to_string())));
    }
    JobHandle::new(
        result_rx,
        Box::new(move || {
            token.cancel();
            let _ = command_tx.send(Command::Cancel);
        }),
    )
    .with_updates(update_rx)
}

/// The longest the worker sleeps between looks at its command queue.
const TICK: Duration = Duration::from_millis(100);

fn publish(job: &PrintJob, station: &Station, shared: &Mutex<Option<TrackedView>>, note: Option<String>) -> Snapshot {
    let snapshot = job.snapshot();
    let mut slot = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let note = note.or_else(|| slot.as_ref().and_then(|v| v.note.clone()));
    *slot = Some(TrackedView {
        view: snapshot.view(),
        lifecycle: snapshot.state,
        advice: snapshot.advice,
        reconciled: snapshot.reconciled.clone(),
        stale: snapshot.stale,
        note,
        name: station.display_name(),
        events: station.events_summary(),
        token: job.cancel_token(),
    });
    snapshot
}

fn run(
    station: &Station,
    mut job: PrintJob,
    shared: &Mutex<Option<TrackedView>>,
    commands: &mpsc::Receiver<Command>,
    updates: &mpsc::Sender<JobUpdate>,
    result: &mpsc::Sender<Result<Report, Failure>>,
    ctx: Option<&egui::Context>,
) {
    let registry = station.registry();
    let repaint = || {
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    };
    let mut due = Instant::now();
    let (mut accepted_sent, mut reported) = (false, false);
    let mut last_progress: Option<Progress> = None;
    publish(&job, station, shared, None);
    loop {
        let mut note = None;
        let mut touched = false;
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    touched = true;
                    note = Some(apply(&mut job, registry, command, &station.display_name()));
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // The UI dropped this print (a new one replaced it, or the app is closing).
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        if touched {
            due = Instant::now();
        }
        let mut changed = touched;
        if Instant::now() >= due {
            let advance = job.advance(registry);
            changed |= advance.changed;
            due = Instant::now() + advance.next_in.unwrap_or(Duration::from_secs(3600 * 24));
            if advance.next_in.is_none() && !job.is_settled() {
                due = Instant::now() + TICK;
            }
        }
        if changed || note.is_some() {
            let snapshot = publish(&job, station, shared, note);
            if !reported {
                if snapshot.remote.is_some() && !accepted_sent {
                    accepted_sent = true;
                    let _ = updates.send(JobUpdate::Accepted(Progress { notes: snapshot.warnings.clone(), ..Progress::default() }));
                }
                if accepted_sent {
                    let progress = progress_of(&snapshot);
                    if last_progress.as_ref() != Some(&progress) {
                        let _ = updates.send(JobUpdate::Status(progress.clone()));
                        last_progress = Some(progress);
                    }
                }
                if let Some(outcome) = result_of(&snapshot, &printer_label(&station.display_name())) {
                    reported = true;
                    let _ = result.send(outcome);
                }
            }
            repaint();
        }
        std::thread::sleep(TICK.min(due.saturating_duration_since(Instant::now())).max(Duration::from_millis(20)));
    }
}

/// Carry out a command on the job; the answer is a line for the status row.
fn apply(job: &mut PrintJob, registry: &lightcraft_print::network::Registry, command: Command, name: &str) -> String {
    let who = printer_label(name);
    let say = |template: &str| fill(tr(template), &[("printer", &who)]);
    let say_why = |template: &str, why: &str| fill(tr(template), &[("printer", &who), ("why", &scrubbed(why, &who))]);
    match command {
        Command::Cancel => match job.cancel(registry) {
            Ok(()) => say("Cancel requested on {printer}."),
            Err(e) => say_why("Cancel on {printer} failed: {why}", e.message()),
        },
        Command::Retry => match job.retry() {
            Ok(()) => say("Sending again to {printer} (a new attempt of the same print)."),
            Err(e) => say_why("Retry to {printer} refused: {why}", e.message()),
        },
        Command::Reconcile => match job.reconcile(registry) {
            Ok(TrackedReconciled::NotFound) => say("Reconcile: {printer} lists no such job."),
            Ok(TrackedReconciled::Found(_)) => say("Reconcile: {printer} has this job. Accept it to follow it."),
            Ok(TrackedReconciled::Ambiguous(found)) => {
                fill(tr("Reconcile: {count} jobs on {printer} match; check the printer."), &[("count", &found.len().to_string()), ("printer", &who)])
            }
            Ok(_) => say("Reconcile: no answer from {printer}."),
            Err(e) => say_why("Reconcile on {printer} failed: {why}", e.message()),
        },
        Command::Refresh => {
            job.refresh(registry);
            say("Status read from {printer}.")
        }
        Command::AcceptFound => match job.snapshot().reconciled {
            Some(TrackedReconciled::Found(found)) => match job.accept(registry, &found.id) {
                Ok(()) => say("Following the job {printer} already had."),
                Err(e) => say_why("Accept on {printer} failed: {why}", e.message()),
            },
            _ => tr("Nothing to accept.").to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_print::network::{JobId, JobKind};

    #[test]
    fn status_text_reads_the_view() {
        let local = JobView::new(JobId::new("local", "print-1"), JobKind::Print, "t", JobState::Pending);
        assert_eq!(status_text(&local, ""), "Sending to the printer");
        let mut queued = JobView::new(JobId::new("wsd", "http://x#1"), JobKind::Print, "t", JobState::Processing);
        queued.progress = Some(lightcraft_print::network::Progress::of(1, 3));
        assert_eq!(status_text(&queued, ""), "Printing on the printer (1 of 3 sheets)");
        queued.progress = Some(lightcraft_print::network::Progress::new(2, None));
        assert_eq!(status_text(&queued, ""), "Printing on the printer (2 sheets so far)");
    }

    #[test]
    fn advice_is_silent_while_the_print_goes_on() {
        assert_eq!(advice_text(RetryAdvice::DoNotRetry, LifecycleState::Submitting, ""), None);
        assert!(advice_text(RetryAdvice::CheckDeviceFirst, LifecycleState::NeedsAttention, "").is_some());
        assert!(advice_text(RetryAdvice::RetryNow, LifecycleState::Terminal(PrintOutcome::Failed), "").is_some());
    }

    const NAME: &str = "Office Laser";

    fn view(backend: &str, state: JobState) -> JobView {
        JobView::new(JobId::new(backend, "http://192.168.1.50:5357/service#9"), JobKind::Print, "t", state)
    }

    #[test]
    fn every_status_line_names_the_printer() {
        for state in [
            JobState::Pending,
            JobState::Processing,
            JobState::Held,
            JobState::Completed,
            JobState::CompletedWithErrors,
            JobState::Cancelled,
            JobState::Aborted,
            JobState::Abandoned,
            JobState::NeedsAttention,
        ] {
            for backend in ["local", "wsd"] {
                let text = status_text(&view(backend, state), NAME);
                assert!(text.contains(NAME), "{state:?}: {text}");
            }
        }
        assert_eq!(status_text(&view("local", JobState::Pending), NAME), "Sending to Office Laser");
        assert_eq!(status_text(&view("wsd", JobState::Pending), NAME), "Accepted by Office Laser: waiting in its queue");
        assert_eq!(status_text(&view("wsd", JobState::Completed), NAME), "Printed on Office Laser");
        assert_eq!(status_text(&view("wsd", JobState::Aborted), NAME), "Office Laser: the printer or the send failed");
        assert_eq!(status_text(&view("wsd", JobState::Cancelled), NAME), "Cancelled on Office Laser");
        assert_eq!(status_text(&view("wsd", JobState::Held), NAME), "Held by Office Laser");
        assert_eq!(status_text(&view("wsd", JobState::NeedsAttention), NAME), "Needs attention on Office Laser");
        assert_eq!(status_text(&view("wsd", JobState::Abandoned), NAME), "No outcome was heard from Office Laser; it may still print it");
        assert_eq!(status_text(&view("wsd", JobState::CompletedWithErrors), NAME), "Office Laser finished with errors");
        let mut warned = view("wsd", JobState::Completed);
        warned.reasons = vec!["toner-low".to_owned()];
        assert_eq!(status_text(&warned, NAME), "Printed on Office Laser, with warnings [toner-low]");
    }

    #[test]
    fn status_lines_without_a_name_say_the_printer() {
        for name in ["", "   ", "\u{7}"] {
            assert_eq!(status_text(&view("local", JobState::Pending), name), "Sending to the printer");
            assert_eq!(status_text(&view("wsd", JobState::Completed), name), "Printed on the printer");
            assert_eq!(status_text(&view("wsd", JobState::Aborted), name), "The printer or the send failed");
            assert_eq!(status_text(&view("wsd", JobState::Abandoned), name), "No outcome was heard; the printer may still print it");
            assert_eq!(status_text(&view("wsd", JobState::NeedsAttention), name), "Needs attention on the printer");
        }
    }

    #[test]
    fn advice_and_stale_lines_name_the_printer() {
        let done = LifecycleState::Terminal(PrintOutcome::Failed);
        assert_eq!(advice_text(RetryAdvice::RetryNow, done, NAME).unwrap(), "Nothing was printed on Office Laser; it is safe to retry.");
        assert_eq!(
            advice_text(RetryAdvice::CheckDeviceFirst, LifecycleState::NeedsAttention, NAME).unwrap(),
            "Check Office Laser before sending again; it may have printed or may still print."
        );
        assert_eq!(advice_text(RetryAdvice::RetryAfter(Duration::from_secs(4)), done, "").unwrap(), "Sending to the printer again in 4 s.");
        assert_eq!(stale_text(Some(StaleReason::NoNews), NAME).unwrap(), "Waiting for Office Laser: it has not answered for a while. Use Check now.");
        assert!(stale_text(Some(StaleReason::Stalled), NAME).unwrap().contains(NAME));
        assert!(stale_text(Some(StaleReason::Abandoned), "").unwrap().contains("the printer"));
        assert_eq!(stale_text(None, NAME), None);
    }

    #[test]
    fn no_address_or_id_reaches_the_text() {
        let uuid = "urn:uuid:3f2504e0-4f89-11d3-9a0c-0305e82c3301";
        let message = format!("connect to http://192.168.1.50:5357/service ({uuid}) at 10.0.0.7:631 failed");
        // The detail appended to a status line, and the failure the dialog shows, are scrubbed the same way.
        let failure =
            failure_named(&AttemptError::from(&lightcraft_print::network::PrintError::new(ErrorKind::Failed, message.clone())), false, NAME);
        assert_eq!(failure.detail, "connect to Office Laser Office Laser at Office Laser failed");
        assert_eq!(scrubbed(&message, "the printer"), "connect to the printer the printer at the printer failed");
        // Names that are themselves addresses, URLs or ids read as no name.
        for name in ["http://192.168.1.50:5357/service", "192.168.1.50", uuid, "3f2504e0-4f89-11d3-9a0c-0305e82c3301"] {
            assert_eq!(printer_label(name), "the printer", "{name}");
            let text = status_text(&view("wsd", JobState::Completed), name);
            for bad in ["http", "://", "192.168", "uuid", "3f2504e0", "5357"] {
                assert!(!text.contains(bad), "{name:?} -> {text}");
            }
        }
        assert_eq!(printer_label("192.168.1.50"), "the printer");
        assert_eq!(printer_label(" Office  Laser "), "Office  Laser");
    }
}
