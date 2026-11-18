//! Plug-in jobs: a command declared `"job": true` runs on a worker of its own with a row in the
//! activity stack ("{command} ({plugin})"), so the window stays responsive. Its `job.*` host calls
//! move the row's bar, count and detail line; counting photos makes the row a batch, with the time
//! left and a summary at the end. The row's ✕ (or `activity.cancel`) stops the job between calls:
//! `job.cancelled` says so, every other host call fails with "cancelled", and a reply
//! `{continue: state}` is not called again.
//!
//! A job may read the catalog, never change it (`lcc_plugins` refuses the writes). Its reads are
//! relayed to the session's thread like a `program.end` hook's
//! ([`super::plugin_programs::answer_requests`]); a session nobody answers for (headless MCP or
//! CLI) refuses them at once, so those run a job with `wait: true`, on the caller's thread.
//!
//! When a job ends, how it went waits in [`take_finished`] for the app to show it (the last
//! reply's `message`, or the error, which the plug-in's log has too).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use lcc_plugins::{HostApi, JobReporter, JobUnit, Manager, PendingCall, StartRequest};
use serde_json::{Value, json};

use super::plugin_programs::{answered, relay_sender};
use super::plugins::{RelayHost, Relayed, SessionHost, manager_for};
use crate::Session;
use crate::activity::{Activity, Cancel, TaskGuard, Unit};

/// How long a relayed catalog read waits for the session's thread.
const ANSWER_WAIT: Duration = Duration::from_secs(60);
/// Ended jobs kept for the app to show ([`take_finished`]); a session that never asks keeps the newest.
const MAX_FINISHED: usize = 20;
/// The row's label: both names are the plug-in's, which no catalog translates.
const LABEL: &str = "{command} ({plugin})";

/// Jobs that ended since their session last looked, oldest first, with the activity registry of
/// the session that started each.
static FINISHED: Mutex<VecDeque<(Activity, Value)>> = Mutex::new(VecDeque::new());
/// Jobs running on a worker now.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Whether a job runs on a worker (the app then answers its requests often).
pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed) > 0
}

/// The jobs session `s` started that ended since the last call, oldest first: `{job, plugin, pluginName, command,
/// label, ok: the last reply | error | cancelled: true}`. Their plug-ins' own fields are synced
/// into `s`'s catalog first (a job may have set them).
pub fn take_finished(s: &mut Session) -> Vec<Value> {
    let ended: Vec<Value> = {
        let mut f = FINISHED.lock().unwrap_or_else(PoisonError::into_inner);
        let (mine, others): (VecDeque<_>, VecDeque<_>) = f.drain(..).partition(|(a, _)| a.is(&s.activity));
        *f = others;
        mine.into_iter().map(|(_, v)| v).collect()
    };
    if !ended.is_empty() {
        super::plugins::sync_fields(s);
    }
    ended
}

/// The job's row, as its `job.*` calls see it.
struct Row {
    guard: TaskGuard,
    /// The job counted items: its row is a batch, with a summary at the end.
    counted: AtomicBool,
}

impl JobReporter for Row {
    fn progress(&self, done: u64, total: u64, unit: JobUnit) {
        self.guard.set_unit(match unit {
            JobUnit::Count => Unit::Count,
            JobUnit::Bytes => Unit::Bytes,
            JobUnit::Percent => Unit::Percent,
        });
        self.guard.progress(done, total);
    }
    fn detail(&self, text: &str) {
        self.guard.detail(text);
    }
    fn item_done(&self) {
        self.counted.store(true, Ordering::Relaxed);
        self.guard.item_done();
    }
    fn item_failed(&self, why: &str) {
        self.counted.store(true, Ordering::Relaxed);
        self.guard.item_failed(why);
    }
    fn cancelled(&self) -> bool {
        self.guard.is_cancelled()
    }
}

impl Row {
    fn new(s: &Session, call: &PendingCall) -> Row {
        let guard = s.activity.start_batch("pluginJob", LABEL, Cancel::Yes);
        guard.name("command", call.job_label().unwrap_or_default());
        guard.name("plugin", call.plugin_name());
        Row { guard, counted: AtomicBool::new(false) }
    }

    /// The job ended: a batch summary only when it counted items (its message says the rest).
    fn end(self) {
        if !self.counted.load(Ordering::Relaxed) {
            self.guard.reports_itself();
        }
    }
}

/// What a job on a worker sees: catalog reads relayed to the session's thread when the app answers
/// them (refused at once otherwise), the session's desktop to look programs up. Writes and starts
/// are refused (`lcc_plugins` refuses them first in a job).
struct JobHost(RelayHost);

impl JobHost {
    fn reachable(&self) -> Result<(), String> {
        if answered() { Ok(()) } else { Err("the catalog can't be reached from a job in this session; run the job with wait: true".into()) }
    }
}

const NO_WRITES: &str = "writes to the catalog aren't available in a job yet";

impl HostApi for JobHost {
    fn catalog(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        self.reachable()?;
        self.0.ask_within(Relayed::Catalog(method.to_string()), params, ANSWER_WAIT)
    }
    fn set_metadata(&mut self, _: &Value) -> Result<Value, String> {
        Err(format!("metadata.setStandard: {NO_WRITES}"))
    }
    fn library_add(&mut self, _: &Value) -> Result<Value, String> {
        Err(format!("library.add: {NO_WRITES}"))
    }
    fn photo_pixels(&mut self, photo: &str, long_edge: u32) -> Result<Value, String> {
        self.reachable()?;
        self.0.pixels_within(photo, long_edge, Some(ANSWER_WAIT))
    }
    fn set_crop(&mut self, _: &str, _: [f64; 4], _: Option<f64>) -> Result<Value, String> {
        Err(format!("develop.setCrop: {NO_WRITES}"))
    }
    fn virtual_copy(&mut self, _: &str, _: Option<&str>) -> Result<Value, String> {
        Err(format!("photo.virtualCopy: {NO_WRITES}"))
    }
    fn find_program(&mut self, program: &lcc_plugins::ProgramDecl) -> Vec<lcc_plugins::Found> {
        self.0.find_program(program)
    }
    fn start_program(&mut self, _: StartRequest) -> Result<(), String> {
        Err("program.start is not available in a job".into())
    }
}

/// How a job ended, for [`take_finished`] and `plugin.run {wait}`.
fn report(id: u64, plugin: &str, plugin_name: &str, command: &str, label: &str, r: &Result<Value, lcc_plugins::Error>) -> Value {
    let mut v = json!({"job": id, "plugin": plugin, "pluginName": plugin_name, "command": command, "label": label});
    match r {
        Ok(ok) => v["ok"] = ok.clone(),
        Err(lcc_plugins::Error::Cancelled) => v["cancelled"] = json!(true),
        Err(e) => v["error"] = json!(e.to_string()),
    }
    v
}

/// Takes a running job off [`RUNNING`] when dropped.
struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Runs `call` to its end; a panic in the host ends it as a failure, so the call is still ended
/// (the plug-in is not left busy) and its row goes.
fn run_guarded(call: &mut PendingCall, host: &mut dyn HostApi, row: &Row) -> lcc_plugins::Outcome {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call.run_with(host, row))).unwrap_or_else(|_| lcc_plugins::Outcome {
        value: Err(lcc_plugins::Error::Failed("the job stopped on an internal error".into())),
        // the log lines of its earlier calls unwound with `run_with`
        logs: Vec::new(),
    })
}

/// Runs job `call` of session `s`, begun with [`Manager::begin_command`]. On a worker by default:
/// returns `{job: its activity id, running: true, plugin, command}` at once. With `wait`, here, to the end: its
/// last reply, or its error.
pub(super) fn run(s: &mut Session, mut call: PendingCall, wait: bool) -> Result<Value, lcc_plugins::Error> {
    let row = Row::new(s, &call);
    let id = row.guard.id();
    let library = s.plugin_library.clone();
    let activity = s.activity.clone();
    let names = (
        call.id().to_string(),
        call.plugin_name().to_string(),
        call.command().unwrap_or_default().to_string(),
        call.job_label().unwrap_or_default().to_string(),
    );
    if wait {
        let outcome = run_guarded(&mut call, &mut SessionHost(s), &row);
        let r = manager_for(&library).get_or_insert_with(Manager::in_memory).end(call, Some(outcome));
        row.end();
        super::plugins::sync_fields(s);
        return r;
    }
    let host = JobHost(RelayHost::new(relay_sender(), s.desktop.clone()));
    let started = json!({"job": id, "running": true, "plugin": names.0, "command": names.2});
    // the call and its row are moved in; a thread that can't start hands them back
    let (tx, rx) = std::sync::mpsc::channel::<(PendingCall, Row)>();
    // counted before the thread can end, so the count never goes below zero
    RUNNING.fetch_add(1, Ordering::Relaxed);
    let spawned = std::thread::Builder::new().name("plugin-job".into()).spawn(move || {
        // uncounted however the thread ends, a panic included
        let _running = Running;
        let Ok((mut call, row)) = rx.recv() else { return };
        let mut host = host;
        let outcome = run_guarded(&mut call, &mut host, &row);
        let r = manager_for(&library).get_or_insert_with(Manager::in_memory).end(call, Some(outcome));
        let (plugin, plugin_name, command, label) = &names;
        let ended = report(id, plugin, plugin_name, command, label, &r);
        // the row goes before the report, so whoever reads the report sees the row gone
        row.end();
        {
            let mut f = FINISHED.lock().unwrap_or_else(PoisonError::into_inner);
            if f.len() >= MAX_FINISHED {
                f.pop_front();
            }
            f.push_back((activity, ended));
        }
    });
    match spawned {
        Ok(_) => {
            if let Err(back) = tx.send((call, row)) {
                // the thread is gone before it took the job (its `Running` has uncounted it): it never ran
                let (call, _row) = back.0;
                let _ = manager_for(&s.plugin_library).get_or_insert_with(Manager::in_memory).end(call, None);
                return Err(lcc_plugins::Error::Failed("the job's worker stopped before it started".into()));
            }
            Ok(started)
        }
        Err(e) => {
            RUNNING.fetch_sub(1, Ordering::Relaxed);
            let _ = manager_for(&s.plugin_library).get_or_insert_with(Manager::in_memory).end(call, None);
            Err(lcc_plugins::Error::Failed(format!("no thread to run the job on ({e})")))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::cmd::plugins::manager;
    use crate::cmd::plugins::tests::turn;

    /// A job `go` ("Go Far") of plug-in `id` that sends each of `requests`, then replies `reply`.
    fn job_module(id: &str, requests: &[&str], reply: &str) -> Vec<u8> {
        let manifest = format!(
            r#"{{"id":"{id}","name":"Job Test","version":"1.0.0","requires":["jobs"],"permissions":{{"catalog":true}},"commands":[{{"id":"go","label":"Go Far","job":true}}]}}"#
        );
        let esc = |s: &str| s.replace('"', "\\\"");
        let data: String = requests.iter().enumerate().map(|(k, r)| format!("(data (i32.const {}) \"{}\")", 8192 + 1024 * k, esc(r))).collect();
        let calls: String = requests
            .iter()
            .enumerate()
            .map(|(k, r)| format!("(drop (call $call (i32.const {}) (i32.const {})))", 8192 + 1024 * k, r.len()))
            .collect();
        let src = format!(
            r#"(module (import "host" "call" (func $call (param i32 i32) (result i32))) (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 4096) "{r}") {data}
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) {calls} (i64.or (i64.shl (i64.const {rl}) (i64.const 32)) (i64.const 4096))))"#,
            m = esc(&manifest),
            r = esc(reply),
            ml = manifest.len(),
            rl = reply.len()
        );
        wat::parse_str(&src).unwrap()
    }

    fn install(bytes: &[u8]) {
        manager().get_or_insert_with(Manager::in_memory).install_bytes(bytes, None, false).unwrap();
    }

    /// Waits up to 10 s for `f` to give something.
    fn within<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let t = Instant::now();
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "{what}: not within 10 s");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_job_runs_on_a_worker_with_a_row_that_stops_it() {
        let _turn = turn();
        // reports progress, then asks to go on: it runs until it is stopped
        install(&job_module(
            "test.spin",
            &[r#"{"method":"job.progress","params":{"done":1,"total":3}}"#, r#"{"method":"job.detail","params":{"text":"a.jpg"}}"#],
            r#"{"ok":{"continue":1}}"#,
        ));
        let mut s = Session::with_demo();
        let started = s.execute("plugin.run", &json!({"plugin": "test.spin", "command": "go"})).unwrap();
        let id = started["job"].as_u64().expect("plugin.run of a job returns its activity id at once");
        assert_eq!((started["plugin"].as_str(), started["command"].as_str()), (Some("test.spin"), Some("go")));
        let row = within("the job's row with its progress", || {
            let list = s.execute("activity.list", &json!({})).unwrap();
            list["tasks"].as_array()?.iter().find(|t| t["id"] == id && t["detail"] == "a.jpg").cloned()
        });
        assert_eq!(row["label"], "Go Far (Job Test)", "named after the command and the plug-in: {row}");
        assert_eq!((row["done"].as_u64(), row["total"].as_u64()), (Some(1), Some(3)), "{row}");
        assert_eq!(row["cancellable"], true, "{row}");
        // the plug-in is busy with it: another call names the job
        let e = s.execute("plugin.run", &json!({"plugin": "test.spin", "command": "go"})).unwrap_err().to_string();
        assert!(e.contains("Go Far"), "{e}");
        s.execute("activity.cancel", &json!({"id": id})).unwrap();
        let ended = within("the stopped job", || take_finished(&mut s).into_iter().find(|v| v["job"] == id));
        assert_eq!(ended["cancelled"], true, "{ended}");
        assert!(!s.activity.list().iter().any(|t| t.id == id), "its row goes");
        // and the plug-in takes calls again
        let again = s.execute("plugin.run", &json!({"plugin": "test.spin", "command": "go"})).unwrap();
        s.execute("activity.cancel", &json!({"id": again["job"]})).unwrap();
        within("the second job", || take_finished(&mut s).into_iter().find(|v| v["job"] == again["job"]));
    }

    #[test]
    fn a_job_that_counts_photos_is_a_batch_wait_returns_its_result_and_a_failed_job_says_why() {
        // a job that counts photos is a batch and wait returns its result
        {
            let _turn = turn();
            install(&job_module(
                "test.count",
                &[r#"{"method":"job.itemDone","params":{}}"#, r#"{"method":"job.itemFailed","params":{"why":"b.jpg: broken"}}"#],
                r#"{"ok":{"message":"Counted"}}"#,
            ));
            let mut s = Session::with_demo();
            let r = s.execute("plugin.run", &json!({"plugin": "test.count", "command": "go", "wait": true})).unwrap();
            assert_eq!(r, json!({"message": "Counted"}), "wait: the job's own result");
            let summary = s.activity.finished().into_iter().find(|b| b.kind == "pluginJob").expect("a batch summary");
            assert_eq!((summary.done, summary.failed, summary.reported), (2, 1, false), "{summary:?}");
            assert!(summary.reasons.iter().any(|r| r.contains("b.jpg")), "{summary:?}");
            // in the background, how it ended waits for the app
            let started = s.execute("plugin.run", &json!({"plugin": "test.count", "command": "go"})).unwrap();
            let ended = within("the job's end", || take_finished(&mut s).into_iter().find(|v| v["job"] == started["job"]));
            assert_eq!(ended["ok"]["message"], "Counted", "{ended}");
            assert_eq!(ended["label"], "Go Far", "{ended}");
        }
        // a failed job says why and logs it
        {
            let _turn = turn();
            install(&job_module("test.fail", &[], r#"{"error":"the disk is full"}"#));
            let mut s = Session::with_demo();
            let e = s.execute("plugin.run", &json!({"plugin": "test.fail", "command": "go", "wait": true})).unwrap_err().to_string();
            assert!(e.contains("the disk is full"), "{e}");
            let started = s.execute("plugin.run", &json!({"plugin": "test.fail", "command": "go"})).unwrap();
            let ended = within("the job's end", || take_finished(&mut s).into_iter().find(|v| v["job"] == started["job"]));
            assert!(ended["error"].as_str().is_some_and(|e| e.contains("the disk is full")), "{ended}");
            let log = s.execute("plugin.log", &json!({"id": "test.fail"})).unwrap();
            assert!(log.as_array().unwrap().iter().any(|l| l.as_str().is_some_and(|l| l.contains("the disk is full"))), "{log}");
        }
    }

    /// A host that breaks on a catalog read.
    struct Boom;

    impl HostApi for Boom {
        fn catalog(&mut self, _: &str, _: &Value) -> Result<Value, String> {
            panic!("simulated host panic")
        }
    }

    #[test]
    fn a_job_whose_host_panics_fails_and_frees_its_plugin() {
        let _turn = turn();
        let bytes = job_module("test.boom", &[r#"{"method":"catalog.query","params":{}}"#], r#"{"ok":{"message":"never"}}"#);
        let read = lcc_plugins::Permissions { catalog: true, ..Default::default() };
        manager().get_or_insert_with(Manager::in_memory).install_bytes_by_user(&bytes, Some(read), false).unwrap();
        let mut s = Session::with_demo();
        let begin =
            |s: &mut Session| manager().as_mut().unwrap().begin_command("test.boom", "go", &json!({}), &[], &Value::Null, &mut SessionHost(s));
        let Ok(lcc_plugins::Begun::Call(mut call)) = begin(&mut s) else { panic!("the job begins") };
        let row = Row::new(&s, &call);
        let outcome = run_guarded(&mut call, &mut Boom, &row);
        let e = manager().as_mut().unwrap().end(*call, Some(outcome)).unwrap_err().to_string();
        row.end();
        assert!(e.contains("internal error"), "a panic ends the job as a failure: {e}");
        assert!(matches!(begin(&mut s), Ok(lcc_plugins::Begun::Call(_))), "and the plug-in isn't left busy");
    }
}
