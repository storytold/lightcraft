//! Jobs (`"job": true` commands) against hand-written modules: the `job.*` host calls reach the
//! reporter, items make a batch, a cancel stops the job between calls, `{continue: state}` calls
//! it again with a fresh budget, catalog writes are refused in a job, and a revoke made while a
//! job runs reaches its next call.

use std::cell::{Cell, RefCell};

use lcc_plugins::{Begun, Error, HostApi, JobReporter, JobUnit, Limits, Manager, Manifest, NoHost, Permissions};
use serde_json::{Value, json};

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn manifest(job: bool) -> String {
    let requires = if job { r#""requires":["jobs"],"# } else { "" };
    format!(
        r#"{{"id":"test.job","name":"Job Test","version":"1.0.0",{requires}"permissions":{{"catalog":true,"metadataWrite":true,"libraryAdd":true}},"commands":[{{"id":"go","label":"Go Far","job":{job}}}]}}"#
    )
}

/// A module whose `lcc_handle` is `body`, with `data` placed at the given offsets.
fn module(manifest: &str, data: &[(u32, &str)], body: &str) -> Vec<u8> {
    let segments: String = data.iter().map(|(at, text)| format!("(data (i32.const {at}) \"{}\")\n", esc(text))).collect();
    let src = format!(
        r#"(module
  (import "host" "call" (func $call (param i32 i32) (result i32)))
  (import "host" "response" (func $response (param i32)))
  (import "host" "log" (func $log (param i32 i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}")
  {segments}
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64)
    (i64.or (i64.shl (i64.const {mlen}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1))
      (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  ;; whether the `m` bytes at `q` occur in the `n` bytes at `p`
  (func $has (param $p i32) (param $n i32) (param $q i32) (param $m i32) (result i32) (local $i i32) (local $j i32)
    (block $no
      (loop $outer
        (br_if $no (i32.gt_s (i32.add (local.get $i) (local.get $m)) (local.get $n)))
        (local.set $j (i32.const 0))
        (block $miss
          (loop $inner
            (if (i32.eq (local.get $j) (local.get $m)) (then (return (i32.const 1))))
            (br_if $miss (i32.ne (i32.load8_u (i32.add (local.get $p) (i32.add (local.get $i) (local.get $j))))
                                 (i32.load8_u (i32.add (local.get $q) (local.get $j)))))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $inner)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $outer)))
    (i32.const 0))
  (func (export "lcc_handle") (param $ptr i32) (param $len i32) (result i64) (local $n i32)
    {body})
)"#,
        m = esc(manifest),
        mlen = manifest.len()
    );
    wat::parse_str(&src).unwrap_or_else(|e| panic!("bad test WAT: {e}\n{src}"))
}

fn ret(at: u32, text: &str) -> String {
    format!("(i64.or (i64.shl (i64.const {}) (i64.const 32)) (i64.const {at}))", text.len())
}

/// Sends each of `requests` in turn, then replies `reply`; `None`: replies with the host's answer
/// to the last request (its `{"ok"|"error"}` envelope).
fn sequence(job: bool, requests: &[&str], reply: Option<&str>) -> Vec<u8> {
    let m = manifest(job);
    let mut data: Vec<(u32, &str)> = requests.iter().enumerate().map(|(k, r)| (8192 + 1024 * k as u32, *r)).collect();
    let mut body: String = data.iter().map(|(at, r)| format!("(local.set $n (call $call (i32.const {at}) (i32.const {})))\n", r.len())).collect();
    match reply {
        Some(r) => {
            data.push((4096, r));
            body.push_str(&ret(4096, r));
        }
        None => {
            body.push_str("(call $response (i32.const 32768)) (i64.or (i64.shl (i64.extend_i32_u (local.get $n)) (i64.const 32)) (i64.const 32768))")
        }
    }
    module(&m, &data, &body)
}

/// Each call counts an item, then replies by the request's `state`: none → `{continue: 1}`,
/// 1 → `{continue: 2}`, 2 → `{message: "all"}`.
fn three_steps() -> Vec<u8> {
    let (item, s1, s2) = (r#"{"method":"job.itemDone","params":{}}"#, r#""state":1"#, r#""state":2"#);
    let (c1, c2, fin) = (r#"{"ok":{"continue":1}}"#, r#"{"ok":{"continue":2}}"#, r#"{"ok":{"message":"all"}}"#);
    let body = format!(
        "(drop (call $call (i32.const 8192) (i32.const {il})))
         (if (call $has (local.get $ptr) (local.get $len) (i32.const 7168) (i32.const {l2})) (then (return {fin})))
         (if (call $has (local.get $ptr) (local.get $len) (i32.const 7424) (i32.const {l1})) (then (return {c2})))
         {c1}",
        il = item.len(),
        l1 = s1.len(),
        l2 = s2.len(),
        fin = ret(4096, fin),
        c2 = ret(5120, c2),
        c1 = ret(6144, c1),
    );
    module(&manifest(true), &[(8192, item), (7168, s2), (7424, s1), (4096, fin), (5120, c2), (6144, c1)], &body)
}

/// Records what the job reported; cancelled once `cancel_after` items were counted.
#[derive(Default)]
struct Rec {
    events: RefCell<Vec<String>>,
    items: Cell<u32>,
    cancel_after: Option<u32>,
}

impl JobReporter for Rec {
    fn progress(&self, done: u64, total: u64, unit: JobUnit) {
        self.events.borrow_mut().push(format!("progress {done}/{total} {unit:?}"));
    }
    fn detail(&self, text: &str) {
        self.events.borrow_mut().push(format!("detail {text}"));
    }
    fn item_done(&self) {
        self.items.set(self.items.get() + 1);
        self.events.borrow_mut().push("item".into());
    }
    fn item_failed(&self, why: &str) {
        self.items.set(self.items.get() + 1);
        self.events.borrow_mut().push(format!("failed {why}"));
    }
    fn cancelled(&self) -> bool {
        self.cancel_after.is_some_and(|n| self.items.get() >= n)
    }
}

/// Counts the catalog calls that reach it.
#[derive(Default)]
struct Host {
    calls: u32,
}

impl HostApi for Host {
    fn catalog(&mut self, _: &str, _: &Value) -> Result<Value, String> {
        self.calls += 1;
        Ok(json!({"photos": 3}))
    }
    fn set_metadata(&mut self, _: &Value) -> Result<Value, String> {
        self.calls += 1;
        Ok(Value::Null)
    }
    fn library_add(&mut self, _: &Value) -> Result<Value, String> {
        self.calls += 1;
        Ok(Value::Null)
    }
}

fn installed(bytes: &[u8], limits: Limits) -> Manager {
    let mut m = Manager::in_memory();
    m.set_limits(limits);
    let all = Permissions { catalog: true, metadata_write: true, library_add: true, ..Default::default() };
    m.install_bytes_by_user(bytes, Some(all), false).unwrap();
    m
}

/// Runs the job's command to its end with `rep` watching.
fn run_job(m: &mut Manager, host: &mut dyn HostApi, rep: &dyn JobReporter) -> lcc_plugins::Result<Value> {
    let Begun::Call(mut call) = m.begin_command("test.job", "go", &json!({}), &[], &Value::Null, host).unwrap() else {
        panic!("the command has no prepare step: it must run")
    };
    assert!(call.is_job(), "a command with `job` is begun as a job");
    let outcome = call.run_with(host, rep);
    m.end(*call, Some(outcome))
}

#[test]
fn a_job_needs_jobs_in_requires() {
    let m = json!({"id": "t.j", "name": "J", "version": "1.0.0", "commands": [{"id": "go", "label": "Go", "job": true}]});
    let e = Manifest::parse(m.to_string().as_bytes()).unwrap_err();
    assert!(matches!(&e, Error::Manifest(t) if t.contains("jobs")), "{e}");
    let ok = json!({"id": "t.j", "name": "J", "version": "1.0.0", "requires": ["jobs"], "commands": [{"id": "go", "label": "Go", "job": true}]});
    assert!(Manifest::parse(ok.to_string().as_bytes()).unwrap().command("go").unwrap().job);
}

#[test]
fn the_job_calls_move_the_row() {
    let bytes = sequence(
        true,
        &[
            r#"{"method":"job.progress","params":{"done":2,"total":5,"unit":"bytes"}}"#,
            r#"{"method":"job.progress","params":{"done":0,"total":0}}"#,
            r#"{"method":"job.detail","params":{"text":"a.jpg"}}"#,
            r#"{"method":"job.itemDone","params":{}}"#,
            r#"{"method":"job.itemFailed","params":{"why":"b.jpg: broken"}}"#,
        ],
        Some(r#"{"ok":{"message":"Done"}}"#),
    );
    let mut m = installed(&bytes, Limits::default());
    let rep = Rec::default();
    assert_eq!(run_job(&mut m, &mut NoHost, &rep).unwrap(), json!({"message": "Done"}));
    assert_eq!(
        *rep.events.borrow(),
        vec!["progress 2/5 Bytes", "progress 0/0 Count", "detail a.jpg", "item", "failed b.jpg: broken"],
        "each job call reaches the row, in order"
    );
}

#[test]
fn a_bad_job_call_is_an_error_for_the_plugin() {
    for (req, needle) in [
        (r#"{"method":"job.progress","params":{"done":1,"total":2,"unit":"furlongs"}}"#, "unit"),
        (r#"{"method":"job.progress","params":{"done":-1,"total":2}}"#, "`done`"),
        (r#"{"method":"job.detail","params":{}}"#, "text"),
        (r#"{"method":"job.nope","params":{}}"#, "unknown host method"),
    ] {
        let mut m = installed(&sequence(true, &[req], None), Limits::default());
        let rep = Rec::default();
        let e = run_job(&mut m, &mut NoHost, &rep).unwrap_err().to_string();
        assert!(e.contains(needle), "{req}: {e}");
        assert!(rep.events.borrow().is_empty(), "{req}: nothing reported");
    }
}

#[test]
fn outside_a_job_the_job_calls_fail() {
    for req in [
        r#"{"method":"job.progress","params":{"done":1,"total":2}}"#,
        r#"{"method":"job.detail","params":{"text":"x"}}"#,
        r#"{"method":"job.itemDone","params":{}}"#,
        r#"{"method":"job.cancelled","params":{}}"#,
    ] {
        let mut m = installed(&sequence(false, &[req], None), Limits::default());
        let e = m.run_command("test.job", "go", &json!({}), &[], &mut NoHost).unwrap_err().to_string();
        assert!(e.contains("only a command declared as a job"), "{req}: {e}");
    }
}

#[test]
fn each_call_of_a_job_has_its_own_budget() {
    // one host call per call, three calls: more than one call's budget, within each one's
    let limits = Limits { max_host_calls: 2, ..Limits::default() };
    let mut m = installed(&three_steps(), limits);
    let rep = Rec::default();
    assert_eq!(run_job(&mut m, &mut NoHost, &rep).unwrap(), json!({"message": "all"}));
    assert_eq!(rep.items.get(), 3);
}

#[test]
fn a_cancel_stops_the_job_between_calls() {
    let mut m = installed(&three_steps(), Limits::default());
    let rep = Rec { cancel_after: Some(1), ..Rec::default() };
    let e = run_job(&mut m, &mut NoHost, &rep).unwrap_err();
    assert_eq!(e, Error::Cancelled);
    assert_eq!(rep.items.get(), 1, "no call after the cancel, though the reply asked to continue");
    assert!(m.get("test.job").unwrap().log.iter().any(|l| l == "command cancelled"), "{:?}", m.get("test.job").unwrap().log);
}

#[test]
fn after_a_cancel_only_job_cancelled_answers() {
    let rep = Rec { cancel_after: Some(0), ..Rec::default() };
    // job.cancelled says so without failing: a plug-in can stop cleanly, with a message of its own
    let mut m = installed(&sequence(true, &[r#"{"method":"job.cancelled","params":{}}"#], None), Limits::default());
    assert_eq!(run_job(&mut m, &mut NoHost, &rep).unwrap(), json!(true));
    for req in [r#"{"method":"catalog.stats","params":{}}"#, r#"{"method":"job.detail","params":{"text":"x"}}"#] {
        let mut m = installed(&sequence(true, &[req], None), Limits::default());
        let mut host = Host::default();
        assert_eq!(run_job(&mut m, &mut host, &rep).unwrap_err(), Error::Cancelled, "{req}");
        assert_eq!(host.calls, 0, "{req}: refused before it reached the host");
        assert!(rep.events.borrow().is_empty(), "{req}: nothing reported");
    }
    // the reply the plug-in got
    let mut m =
        installed(&sequence(true, &[r#"{"method":"catalog.stats","params":{}}"#], Some(r#"{"ok":{"message":"stopped"}}"#)), Limits::default());
    assert_eq!(run_job(&mut m, &mut Host::default(), &rep).unwrap(), json!({"message": "stopped"}), "its own final reply stands");
}

#[test]
fn a_job_reads_the_catalog_but_never_writes_it() {
    let mut m = installed(&sequence(true, &[r#"{"method":"catalog.stats","params":{}}"#], None), Limits::default());
    let mut host = Host::default();
    assert_eq!(run_job(&mut m, &mut host, &Rec::default()).unwrap(), json!({"photos": 3}));
    for req in [
        r#"{"method":"metadata.setStandard","params":{"ids":["1"],"title":"x"}}"#,
        r#"{"method":"library.add","params":{"path":"/x/a.tif","from":"1"}}"#,
        r#"{"method":"develop.setCrop","params":{"photo":"1","rect":[0.1,0.1,0.9,0.9]}}"#,
        r#"{"method":"photo.virtualCopy","params":{"photo":"1"}}"#,
        r#"{"method":"program.start","params":{"program":"p"}}"#,
    ] {
        let mut m = installed(&sequence(true, &[req], None), Limits::default());
        let mut host = Host::default();
        let e = run_job(&mut m, &mut host, &Rec::default()).unwrap_err().to_string();
        assert!(e.contains("in a job"), "{req}: {e}");
        assert_eq!(host.calls, 0, "{req}: refused before it reached the host");
    }
    // outside a job the same write goes through
    let mut m = installed(&sequence(false, &[r#"{"method":"metadata.setStandard","params":{"ids":["1"],"title":"x"}}"#], None), Limits::default());
    let mut host = Host::default();
    m.run_command("test.job", "go", &json!({}), &[], &mut host).unwrap();
    assert_eq!(host.calls, 1);
}

#[test]
fn a_continue_state_too_large_for_a_message_fails_the_job() {
    let state = "x".repeat(250);
    let reply = format!(r#"{{"ok":{{"continue":"{state}"}}}}"#);
    let limits = Limits { max_message_bytes: 300, ..Limits::default() };
    let mut m = installed(&sequence(true, &[], Some(&reply)), limits);
    let e = run_job(&mut m, &mut NoHost, &Rec::default()).unwrap_err();
    assert!(matches!(&e, Error::Limit(t) if t.contains("continue state")), "{e}");
}

#[test]
fn a_running_job_makes_its_plugin_busy_and_names_the_job() {
    let mut m = installed(&sequence(true, &[], Some(r#"{"ok":{}}"#)), Limits::default());
    let Begun::Call(mut call) = m.begin_command("test.job", "go", &json!({}), &[], &Value::Null, &mut NoHost).unwrap() else { panic!() };
    let e = m.run_command("test.job", "go", &json!({}), &[], &mut NoHost).unwrap_err();
    assert!(matches!(&e, Error::Busy(id, why) if id == "test.job" && why.contains("Go Far")), "{e}");
    assert!(e.to_string().contains("job \"Go Far\""), "{e}");
    let outcome = call.run_with(&mut NoHost, &Rec::default());
    m.end(*call, Some(outcome)).unwrap();
    // run as a plain call, a job runs to its end with nobody watching
    assert_eq!(m.run_command("test.job", "go", &json!({}), &[], &mut NoHost).unwrap(), json!({}));
}

/// Looks at the plug-in's data file each time the job sets its detail line, and asks to stop
/// after two photos.
struct Peek {
    file: std::path::PathBuf,
    seen: RefCell<Vec<bool>>,
    items: Cell<u32>,
}

impl JobReporter for Peek {
    fn progress(&self, _: u64, _: u64, _: JobUnit) {}
    fn detail(&self, _: &str) {
        let on_disk = std::fs::read_to_string(&self.file).unwrap_or_default();
        self.seen.borrow_mut().push(on_disk.contains("checked"));
    }
    fn item_done(&self) {
        self.items.set(self.items.get() + 1);
    }
    fn item_failed(&self, _: &str) {}
    fn cancelled(&self) -> bool {
        self.items.get() >= 2
    }
}

#[test]
fn what_a_job_wrote_is_saved_before_its_next_call() {
    let dir = std::env::temp_dir().join(format!("lcc-plugins-jobs-saved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut m = Manager::open_with_features(&dir, &["jobs"]).unwrap();
    // each call writes to its namespace, looks at the file, counts a photo and asks to go on
    let bytes = sequence(
        true,
        &[
            r#"{"method":"ns.set","params":{"photo":"p1","key":"checked","value":true}}"#,
            r#"{"method":"job.detail","params":{"text":"p1"}}"#,
            r#"{"method":"job.itemDone","params":{}}"#,
        ],
        Some(r#"{"ok":{"continue":1}}"#),
    );
    m.install_bytes_by_user(&bytes, Some(Permissions { catalog: true, ..Default::default() }), false).unwrap();
    let peek = Peek { file: dir.join("data").join("test.job.json"), seen: RefCell::default(), items: Cell::new(0) };
    let e = run_job(&mut m, &mut NoHost, &peek).unwrap_err();
    assert_eq!(e, Error::Cancelled);
    assert_eq!(*peek.seen.borrow(), vec![false, true], "the first call's write is on disk before the second call runs");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A change made to the manager while a job runs.
type Change<'a> = Box<dyn FnMut(&mut Manager) + 'a>;

/// Answers catalog calls, making `change` to the manager at the first (a revoke, a disable, …).
struct Changer<'a> {
    m: &'a mut Manager,
    change: Change<'a>,
    calls: u32,
}

impl HostApi for Changer<'_> {
    fn catalog(&mut self, _: &str, _: &Value) -> Result<Value, String> {
        self.calls += 1;
        if self.calls == 1 {
            (self.change)(self.m);
        }
        Ok(json!({"photos": 3}))
    }
}

/// A job with `manifest` whose calls each read the catalog: the first asks to go on, the second
/// replies with what it got.
fn reads_twice(manifest: &str) -> Vec<u8> {
    let (stats, s1, c1) = (r#"{"method":"catalog.stats","params":{}}"#, r#""state":1"#, r#"{"ok":{"continue":1}}"#);
    let body = format!(
        "(local.set $n (call $call (i32.const 8192) (i32.const {sl})))
         (if (call $has (local.get $ptr) (local.get $len) (i32.const 7424) (i32.const {l1}))
           (then (call $response (i32.const 32768))
                 (return (i64.or (i64.shl (i64.extend_i32_u (local.get $n)) (i64.const 32)) (i64.const 32768)))))
         {c1}",
        sl = stats.len(),
        l1 = s1.len(),
        c1 = ret(6144, c1),
    );
    module(manifest, &[(8192, stats), (7424, s1), (6144, c1)], &body)
}

#[test]
fn a_change_made_while_a_job_runs_reaches_its_next_call() {
    let bytes = reads_twice(&manifest(true));
    let less = Permissions { metadata_write: true, ..Default::default() };
    let changes: Vec<(&str, Change)> = vec![
        (
            "revoked",
            Box::new(|m: &mut Manager| {
                m.set_grant("test.job", less.clone()).unwrap();
            }),
        ),
        ("disabled", Box::new(|m: &mut Manager| m.set_enabled("test.job", false).unwrap())),
        ("uninstalled", Box::new(|m: &mut Manager| m.uninstall("test.job").unwrap())),
        (
            "updated with less granted",
            Box::new(|m: &mut Manager| {
                m.install_bytes_by_user(&bytes, Some(less.clone()), false).unwrap();
            }),
        ),
    ];
    for (what, change) in changes {
        let mut m = installed(&bytes, Limits::default());
        let Begun::Call(mut call) = m.begin_command("test.job", "go", &json!({}), &[], &Value::Null, &mut NoHost).unwrap() else { panic!() };
        let mut host = Changer { m: &mut m, change, calls: 0 };
        let outcome = call.run_with(&mut host, &Rec::default());
        assert_eq!(host.calls, 1, "{what}: the second call's catalog read never reached the host");
        drop(host);
        let e = m.end(*call, Some(outcome)).unwrap_err().to_string();
        assert!(e.contains("not granted"), "{what} during the first call: the job's next call has no catalog permission: {e}");
    }
}

/// Going back to the previous version while a job runs keeps the job's permissions (those the old
/// version asks for), as an update does: its next call still reads the catalog.
#[test]
fn a_job_running_through_a_rollback_keeps_its_permissions() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugin-jobs-rollback-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut m = Manager::open(&dir).unwrap();
    let all = Permissions { catalog: true, metadata_write: true, library_add: true, ..Default::default() };
    m.install_bytes_by_user(&reads_twice(&manifest(true).replace("1.0.0", "0.9.0")), Some(all), false).unwrap();
    m.install_bytes(&reads_twice(&manifest(true)), None, false).unwrap();
    let Begun::Call(mut call) = m.begin_command("test.job", "go", &json!({}), &[], &Value::Null, &mut NoHost).unwrap() else { panic!() };
    let change: Change = Box::new(|m: &mut Manager| {
        m.rollback("test.job").unwrap();
    });
    let mut host = Changer { m: &mut m, change, calls: 0 };
    let outcome = call.run_with(&mut host, &Rec::default());
    assert_eq!(host.calls, 2, "the second call read the catalog too");
    drop(host);
    assert_eq!(m.end(*call, Some(outcome)).unwrap(), json!({"photos": 3}));
    assert_eq!(m.get("test.job").unwrap().plugin.manifest().version, "0.9.0");
}
