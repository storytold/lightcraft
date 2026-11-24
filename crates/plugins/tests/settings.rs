//! Settings kept for the whole app, dialogs that remember their values, and the `prepare` step,
//! against hand-written modules: what the host keeps, checks and runs, and what it does when a
//! prepare step fails.

use std::path::PathBuf;

use lcc_plugins::{Begun, Error, Limits, Manager, NoHost, Prepared};
use serde_json::{Value, json};

/// How a test module answers `lcc_handle`.
enum Answer<'a> {
    /// `command.prepare` and `settings.prepare` get these replies (`{"ok": …}` / `{"error": …}`);
    /// a command gets its own request back as `{"ok": <request>}`, so a test sees its arguments.
    Prepare { command: &'a str, settings: &'a str },
    /// The prepare steps trap (`unreachable`); a command gets its request back.
    Trap,
    /// The prepare steps never return (the budget stops them); a command gets its request back.
    Spin,
    /// Sends the host request (`{"method", "params"}`) and returns the host's reply.
    Forward(&'a str),
}

/// A module with `manifest` answering as `answer` says.
///
/// Requests are serialized with sorted keys: a command's starts `{"args"`, `command.prepare`'s
/// `{"command"`, `settings.prepare`'s `{"hook"`; the third byte tells them apart.
fn module(manifest: &Value, answer: Answer) -> Vec<u8> {
    let manifest = manifest.to_string();
    let esc = |s: &str| s.bytes().map(|b| format!("\\{b:02x}")).collect::<String>();
    // a command's request echoed at 32768: `{"ok":` + request + `}`
    let echo = "(memory.copy (i32.const 32774) (local.get $ptr) (local.get $len))
        (i32.store8 (i32.add (i32.const 32774) (local.get $len)) (i32.const 125))
        (i64.or (i64.shl (i64.extend_i32_u (i32.add (local.get $len) (i32.const 7))) (i64.const 32)) (i64.const 32768))";
    let (command, settings) = match &answer {
        Answer::Prepare { command, settings } => (*command, *settings),
        _ => ("", ""),
    };
    let reply = |at: u32, r: &str| format!("(return (i64.or (i64.shl (i64.const {}) (i64.const 32)) (i64.const {at})))", r.len());
    let prepare = match &answer {
        Answer::Prepare { .. } => format!(
            "(if (i32.eq (i32.load8_u (i32.add (local.get $ptr) (i32.const 2))) (i32.const 99)) (then {}))
             (if (i32.eq (i32.load8_u (i32.add (local.get $ptr) (i32.const 2))) (i32.const 104)) (then {}))",
            reply(16384, command),
            reply(24576, settings)
        ),
        Answer::Trap => "(if (i32.ne (i32.load8_u (i32.add (local.get $ptr) (i32.const 2))) (i32.const 97)) (then unreachable))".into(),
        Answer::Spin => "(if (i32.ne (i32.load8_u (i32.add (local.get $ptr) (i32.const 2))) (i32.const 97)) (then (loop $l (br $l))))".into(),
        Answer::Forward(_) => String::new(),
    };
    let body = match &answer {
        Answer::Forward(req) => format!(
            "(local.set $n (call $call (i32.const 8192) (i32.const {})))
             (call $response (i32.const 40960))
             (i64.or (i64.shl (i64.extend_i32_u (local.get $n)) (i64.const 32)) (i64.const 40960))",
            req.len()
        ),
        _ => format!("{prepare} {echo}"),
    };
    let request = match &answer {
        Answer::Forward(r) => *r,
        _ => "",
    };
    let src = format!(
        r#"(module
  (import "host" "call" (func $call (param i32 i32) (result i32)))
  (import "host" "response" (func $response (param i32)))
  (import "host" "log" (func $log (param i32 i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}")
  (data (i32.const 8192) "{rq}")
  (data (i32.const 16384) "{c}")
  (data (i32.const 24576) "{s}")
  (data (i32.const 32768) "{{\"ok\":")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {mlen}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1)) (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "lcc_handle") (param $ptr i32) (param $len i32) (result i64) (local $n i32)
    {body})
)"#,
        m = esc(&manifest),
        rq = esc(request),
        c = esc(command),
        s = esc(settings),
        mlen = manifest.len(),
    );
    wat::parse_str(&src).unwrap_or_else(|e| panic!("bad test WAT: {e}\n{src}"))
}

fn temp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugin-settings-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn settings_manifest(version: &str, settings: Value) -> Value {
    json!({"id": "test.settings", "name": "Settings", "version": version, "settings": settings, "commands": [{"id": "go", "label": "Go"}]})
}

fn values(m: &mut Manager, id: &str) -> Value {
    Value::Object(m.settings(id, &mut NoHost).unwrap().values)
}

/// Settings come back after a restart and whatever library is open; `set_settings` checks every
/// value against its field and keeps none of them when one doesn't fit.
#[test]
fn settings_are_kept_for_the_whole_app_and_checked() {
    let dir = temp("app");
    let fields = json!([
        {"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 3},
        {"key": "mode", "label": "Mode", "type": "choice", "options": ["fast", "best"]},
        {"key": "name", "label": "Name", "type": "text", "default": "x"}]);
    let id = "test.settings";
    let mut m = Manager::open(&dir).unwrap();
    m.set_library("library-A");
    m.install_bytes(&module(&settings_manifest("1.0.0", fields.clone()), Answer::Trap), None, false).unwrap();
    assert_eq!(values(&mut m, id), json!({"n": 3.0, "mode": "fast", "name": "x"}), "defaults for what was never set");
    let now = m.set_settings(id, &json!({"n": 50, "mode": "best"}), &mut NoHost).unwrap();
    assert_eq!(Value::Object(now), json!({"n": 10.0, "mode": "best", "name": "x"}), "numbers are clamped to their field");
    for bad in [json!({"mode": "slow"}), json!({"nope": 1}), json!({"name": "y", "n": "ten"}), json!(["n"])] {
        assert!(m.set_settings(id, &bad, &mut NoHost).is_err(), "{bad}");
    }
    assert_eq!(values(&mut m, id), json!({"n": 10.0, "mode": "best", "name": "x"}), "a refused change keeps none of its values");
    m.set_settings(id, &json!({"mode": null}), &mut NoHost).unwrap();
    assert_eq!(values(&mut m, id)["mode"], "fast", "null is back to the default");
    drop(m);

    let mut m = Manager::open(&dir).unwrap();
    m.set_library("library-B");
    assert_eq!(values(&mut m, id), json!({"n": 10.0, "mode": "fast", "name": "x"}), "after a restart, in another library");
}

/// The plug-in reads and writes its settings with `settings.get` / `settings.set`, checked against
/// its fields.
#[test]
fn the_plugin_reads_and_writes_its_settings() {
    let fields = json!([{"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 3}]);
    let id = "test.settings";
    let run = |req: &str, m: &mut Manager| {
        m.install_bytes(&module(&settings_manifest("1.0.0", fields.clone()), Answer::Forward(req)), None, false).unwrap();
        m.run_command(id, "go", &Value::Null, &[], &mut NoHost)
    };
    let mut m = Manager::in_memory();
    assert_eq!(run(r#"{"method":"settings.get","params":{}}"#, &mut m).unwrap(), json!({"n": 3.0}));
    assert_eq!(run(r#"{"method":"settings.set","params":{"key":"n","value":99}}"#, &mut m).unwrap(), json!(10.0), "clamped");
    assert_eq!(values(&mut m, id), json!({"n": 10.0}));
    assert_eq!(run(r#"{"method":"settings.get","params":{}}"#, &mut m).unwrap(), json!({"n": 10.0}), "what was set");
    assert!(run(r#"{"method":"settings.set","params":{"key":"n","value":"x"}}"#, &mut m).is_err());
    assert!(run(r#"{"method":"settings.set","params":{"key":"other","value":1}}"#, &mut m).is_err());
    assert_eq!(values(&mut m, id), json!({"n": 10.0}));
    assert_eq!(run(r#"{"method":"settings.set","params":{"key":"n","value":null}}"#, &mut m).unwrap(), json!(3.0), "back to the default");
}

/// An update drops the settings it no longer declares and starts new ones at their defaults;
/// removing the plug-in removes its settings file.
#[test]
fn updates_drop_old_settings_and_removing_deletes_them() {
    let dir = temp("update");
    let id = "test.settings";
    let text = |k: &str| json!({"key": k, "label": k, "type": "text", "default": format!("{k}-default")});
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&module(&settings_manifest("1.0.0", json!([text("a"), text("b")])), Answer::Trap), None, false).unwrap();
    m.set_settings(id, &json!({"a": "A", "b": "B"}), &mut NoHost).unwrap();
    m.install_bytes(&module(&settings_manifest("1.1.0", json!([text("b"), text("c")])), Answer::Trap), None, false).unwrap();
    assert_eq!(values(&mut m, id), json!({"b": "B", "c": "c-default"}));
    let file = dir.join("settings").join(format!("{id}.json"));
    let saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(saved["settings"], json!({"b": "B"}), "`a` is gone from the file");
    m.uninstall(id).unwrap();
    assert!(!file.exists(), "removing the plug-in removes its settings");
}

/// A damaged settings file gives the defaults and a line in the plug-in's log; the app goes on.
#[test]
fn a_damaged_settings_file_gives_defaults_and_a_log_line() {
    let dir = temp("damaged");
    let id = "test.settings";
    let fields = json!([{"key": "b", "label": "B", "type": "bool", "default": true}]);
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&module(&settings_manifest("1.0.0", fields), Answer::Trap), None, false).unwrap();
    m.set_settings(id, &json!({"b": false}), &mut NoHost).unwrap();
    drop(m);
    std::fs::write(dir.join("settings").join(format!("{id}.json")), b"\xff\xfe not json").unwrap();
    let mut m = Manager::open(&dir).unwrap();
    assert_eq!(values(&mut m, id), json!({"b": true}));
    assert!(m.get(id).unwrap().log.iter().any(|l| l.contains("settings") && l.contains("using the defaults")), "{:?}", m.get(id).unwrap().log);
    m.set_settings(id, &json!({"b": false}), &mut NoHost).unwrap();
    assert!(dir.join("settings").join(format!("{id}.json.bad")).exists(), "the damaged file is kept aside");
}

fn dialog_manifest(version: &str, pick: Value) -> Value {
    json!({"id": "test.dialog", "name": "Dialog", "version": version,
        "commands": [{"id": "go", "label": "Go", "dialog": [
            {"key": "keep", "label": "Keep", "type": "text", "default": "k0"},
            {"key": "once", "label": "Once", "type": "bool", "remember": false},
            {"key": "pick", "label": "Pick", "type": "choice", "options": pick}]}],
        "exportDestinations": [{"id": "d", "name": "D", "dialog": [{"key": "album", "label": "Album", "type": "text"}]}]})
}

/// A command's and a destination's dialog open with the values they last ran with, for the whole
/// app; a field with `remember: false`, or a remembered value the field no longer takes, starts at
/// its default.
#[test]
fn dialogs_open_with_their_last_run_values() {
    let dir = temp("remember");
    let id = "test.dialog";
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&module(&dialog_manifest("1.0.0", json!(["x", "y"])), Answer::Trap), None, false).unwrap();
    let fields = |m: &Manager| m.get(id).unwrap().plugin.manifest().command("go").unwrap().dialog.clone();
    assert_eq!(Value::Object(m.command_values(id, "go", &fields(&m))), json!({"keep": "k0", "once": false, "pick": "x"}));
    m.run_command(id, "go", &json!({"keep": "k1", "once": true, "pick": "y"}), &[], &mut NoHost).unwrap();
    assert_eq!(Value::Object(m.command_values(id, "go", &fields(&m))), json!({"keep": "k1", "once": false, "pick": "y"}));
    m.export_to(id, "d", &json!({"album": "Trip"}), &[json!({"path": "/tmp/x.jpg"})], &mut NoHost).unwrap();
    assert_eq!(Value::Object(m.destination_values(id, "d")), json!({"album": "Trip"}));
    // a run without arguments (an agent taking the defaults) doesn't change what is remembered
    m.run_command(id, "go", &Value::Null, &[], &mut NoHost).unwrap();
    assert_eq!(m.command_values(id, "go", &fields(&m))["keep"], "k1");
    drop(m);

    let mut m = Manager::open(&dir).unwrap();
    m.set_library("another");
    assert_eq!(Value::Object(m.command_values(id, "go", &fields(&m))), json!({"keep": "k1", "once": false, "pick": "y"}), "after a restart");
    assert_eq!(Value::Object(m.destination_values(id, "d")), json!({"album": "Trip"}));
    // `y` is no longer offered: the dialog starts at the default
    m.install_bytes(&module(&dialog_manifest("1.1.0", json!(["x", "z"])), Answer::Trap), None, false).unwrap();
    assert_eq!(Value::Object(m.command_values(id, "go", &fields(&m))), json!({"keep": "k1", "once": false, "pick": "x"}));
}

fn prepare_manifest(versions: Value) -> Value {
    json!({"id": "test.prepare", "name": "Prepare", "version": "1.0.0",
        "commands": [{"id": "go", "label": "Go", "prepare": true, "dialog": [
            {"key": "version", "label": "Version", "type": "choice", "options": versions},
            {"key": "stack", "label": "Stack", "type": "bool"}]}],
        "settings": [{"key": "version", "label": "Version", "type": "choice", "options": [], "prepare": true},
            {"key": "keep", "label": "Keep", "type": "bool"}]})
}

fn prepared(command: &str, settings: &str) -> (Manager, &'static str) {
    let mut m = Manager::in_memory();
    m.install_bytes(&module(&prepare_manifest(json!([])), Answer::Prepare { command, settings }), None, false).unwrap();
    (m, "test.prepare")
}

/// The run arguments a command got (its request comes back from the test module).
fn ran_with(r: lcc_plugins::Result<Value>) -> Value {
    let r = r.unwrap();
    assert_eq!(r["hook"], "command", "the command ran: {r}");
    r["args"].clone()
}

/// `command.prepare` answers with a dialog of its own options, a run without the dialog, or a
/// message and no run; `settings.prepare` fills settings the same way.
#[test]
fn prepare_shows_a_dialog_runs_at_once_or_explains() {
    let (mut m, id) = prepared(
        r#"{"ok":{"dialog":{"version":{"options":["2.0","3.1"],"default":"3.1"}}}}"#,
        r#"{"ok":{"fields":{"version":{"options":["2.0","3.1"]}}}}"#,
    );
    let sel = ["7".to_string()];
    let Prepared::Dialog { fields, error: None } = m.prepare_command(id, "go", &sel, &mut NoHost).unwrap() else { panic!("a dialog") };
    let version = fields.iter().find(|f| f.key == "version").unwrap();
    assert_eq!(serde_json::to_value(version).unwrap()["options"], json!(["2.0", "3.1"]));
    assert_eq!(Value::Object(m.command_values(id, "go", &fields)), json!({"version": "3.1", "stack": false}), "its default");
    // the dialog's values are checked against the options it was given
    assert_eq!(
        ran_with(m.run_command(id, "go", &json!({"version": "2.0", "stack": true}), &sel, &mut NoHost)),
        json!({"version": "2.0", "stack": true})
    );
    assert!(m.run_command(id, "go", &json!({"version": "9.9"}), &sel, &mut NoHost).is_err());
    assert_eq!(m.command_values(id, "go", &fields)["version"], "2.0", "remembered, and still offered");
    // settings prepared the same way, and checked against those options
    let lcc_plugins::SettingsView { fields, values, error } = m.settings(id, &mut NoHost).unwrap();
    assert!(error.is_none());
    assert_eq!(serde_json::to_value(&fields[0]).unwrap()["options"], json!(["2.0", "3.1"]));
    assert_eq!(values["version"], "2.0", "the first option");
    m.set_settings(id, &json!({"version": "3.1"}), &mut NoHost).unwrap();
    assert!(m.set_settings(id, &json!({"version": "4.0"}), &mut NoHost).is_err());

    let (mut m, id) = prepared(r#"{"ok":{"run":{"version":"3.1"}}}"#, "{}");
    assert_eq!(m.prepare_command(id, "go", &sel, &mut NoHost).unwrap(), Prepared::Run(json!({"version": "3.1", "stack": false})));
    assert_eq!(ran_with(m.run_command(id, "go", &Value::Null, &sel, &mut NoHost)), json!({"version": "3.1", "stack": false}), "no dialog");

    let (mut m, id) = prepared(r#"{"ok":{"message":"No version of the tool is installed."}}"#, "{}");
    assert_eq!(m.prepare_command(id, "go", &sel, &mut NoHost).unwrap(), Prepared::Message("No version of the tool is installed.".into()));
    let r = m.run_command(id, "go", &json!({"version": "3.1"}), &sel, &mut NoHost).unwrap();
    assert_eq!(r, json!({"message": "No version of the tool is installed.", "ran": false}), "nothing ran");
}

/// A prepare step that traps, runs out of budget or answers something invalid is logged: the
/// dialog comes up with its declared options when it has some, else the command fails, and the
/// command itself never runs on an answer it didn't get.
#[test]
fn failing_prepare_steps_never_run_the_command_on_a_bad_answer() {
    let sel = ["7".to_string()];
    let invalid = [
        r#"{"ok":"run"}"#,
        r#"{"ok":{"run":{"version":"2.0"},"message":"both"}}"#,
        r#"{"ok":{"dialog":{"nope":{"default":"x"}}}}"#,
        r#"{"ok":{"dialog":{"stack":{"options":["a"]}}}}"#,
        r#"{"ok":{"dialog":{"version":{"options":[]}}}}"#,
        r#"{"ok":{"dialog":{"version":{"default":"x"}}}}"#,
        r#"{"ok":{"fields":{"version":{"default":"x"}}}}"#,
        r#"{"ok":{"dialog":{"version":{"options":["2.0"],"default":"3.1"}}}}"#,
        r#"{"ok":{"run":"now"}}"#,
        r#"{"ok":{"run":{"stack":"yes"}}}"#,
        r#"{"ok":{"message":7}}"#,
        r#"{"error":"the tool's folder can't be read"}"#,
    ];
    let limits = Limits { wall_time_ms: 200, fuel: 5_000_000, ..Limits::default() };
    let mut modules: Vec<(String, Vec<u8>, Vec<u8>)> = invalid
        .iter()
        .map(|a| {
            let p = Answer::Prepare { command: a, settings: a };
            let q = Answer::Prepare { command: a, settings: a };
            (a.to_string(), module(&prepare_manifest(json!(["2.0"])), p), module(&prepare_manifest(json!([])), q))
        })
        .collect();
    modules.push(("trap".into(), module(&prepare_manifest(json!(["2.0"])), Answer::Trap), module(&prepare_manifest(json!([])), Answer::Trap)));
    modules.push(("spin".into(), module(&prepare_manifest(json!(["2.0"])), Answer::Spin), module(&prepare_manifest(json!([])), Answer::Spin)));
    for (what, declared, run_time) in modules {
        let id = "test.prepare";
        // declared options: the dialog comes up with them, and the error
        let mut m = Manager::in_memory();
        m.set_limits(limits.clone());
        m.install_bytes(&declared, None, false).unwrap();
        let Ok(Prepared::Dialog { fields, error: Some(e) }) = m.prepare_command(id, "go", &sel, &mut NoHost) else {
            panic!("{what}: the declared dialog")
        };
        assert_eq!(serde_json::to_value(&fields[0]).unwrap()["options"], json!(["2.0"]), "{what}");
        assert!(m.get(id).unwrap().log.iter().any(|l| l.contains("prepare")), "{what}: logged: {e}");
        assert_eq!(
            ran_with(m.run_command(id, "go", &json!({"version": "2.0"}), &sel, &mut NoHost)),
            json!({"version": "2.0", "stack": false}),
            "{what}"
        );
        assert!(m.run_command(id, "go", &Value::Null, &sel, &mut NoHost).is_err(), "{what}: never runs on the defaults alone");
        let lcc_plugins::SettingsView { values, error, .. } = m.settings(id, &mut NoHost).unwrap();
        assert!(error.is_some() && values.contains_key("version"), "{what}: settings fall back to the declared fields");
        // the page still saves: the unprepared choice stays unset, the others are kept
        let saved = m.set_settings(id, &json!({"version": values["version"], "keep": true}), &mut NoHost).unwrap();
        assert_eq!(saved["keep"], true, "{what}");
        // options only the plug-in can give: an error, and the command doesn't run
        let mut m = Manager::in_memory();
        m.set_limits(limits.clone());
        m.install_bytes(&run_time, None, false).unwrap();
        assert!(m.prepare_command(id, "go", &sel, &mut NoHost).is_err(), "{what}");
        assert!(m.run_command(id, "go", &json!({"version": "2.0"}), &sel, &mut NoHost).is_err(), "{what}: no run");
        assert!(m.run_command(id, "go", &Value::Null, &sel, &mut NoHost).is_err(), "{what}: no run");
    }
}

/// Records metadata writes.
#[derive(Default)]
struct Writes(Vec<Value>);

impl lcc_plugins::HostApi for Writes {
    fn set_metadata(&mut self, params: &Value) -> Result<Value, String> {
        self.0.push(params.clone());
        Ok(json!({}))
    }

    fn library_add(&mut self, params: &Value) -> Result<Value, String> {
        self.0.push(params.clone());
        Ok(json!({"photo": "2", "added": true, "reloaded": false}))
    }

    fn set_crop(&mut self, photo: &str, rect: [f64; 4], angle: Option<f64>) -> Result<Value, String> {
        self.0.push(json!({"photo": photo, "rect": rect, "angle": angle}));
        Ok(json!({"rect": rect, "angle": angle.unwrap_or(0.0)}))
    }

    fn virtual_copy(&mut self, photo: &str, name: Option<&str>) -> Result<Value, String> {
        self.0.push(json!({"photo": photo, "name": name}));
        Ok(json!({"photo": "2"}))
    }
}

/// A prepare step only reads: whatever it is granted, a host call that writes (metadata, the library,
/// its data,
/// its settings, files, an HTTP request other than GET or HEAD) is refused there, and the same call
/// from the command itself goes through.
#[test]
fn prepare_steps_can_only_read() {
    let manifest = json!({"id": "test.readonly", "name": "Read only", "version": "1.0.0", "requires": ["settings", "libraryAdd", "crop", "virtualCopy"],
        "permissions": {"metadataWrite": true, "libraryAdd": true, "crop": true, "virtualCopy": true, "network": ["example.invalid"]},
        "settings": [{"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 1, "prepare": true}],
        "commands": [{"id": "go", "label": "Go", "prepare": true, "dialog": [{"key": "b", "label": "B", "type": "bool"}]}]});
    let grant = lcc_plugins::Permissions {
        metadata_write: true,
        library_add: true,
        crop: true,
        virtual_copy: true,
        network: vec!["example.invalid".into()],
        ..Default::default()
    };
    let id = "test.readonly";
    for (req, runs) in [
        (r#"{"method":"metadata.setStandard","params":{"ids":["1"],"title":"t"}}"#, true),
        (r#"{"method":"library.add","params":{"path":"/x/a.tif","from":"1"}}"#, true),
        (r#"{"method":"develop.setCrop","params":{"photo":"1","rect":[0.1,0.1,0.9,0.9]}}"#, true),
        (r#"{"method":"photo.virtualCopy","params":{"photo":"1"}}"#, true),
        (r#"{"method":"ns.set","params":{"photo":"1","key":"k","value":1}}"#, true),
        (r#"{"method":"settings.set","params":{"key":"n","value":5}}"#, true),
        (r#"{"method":"http.request","params":{"method":"POST","url":"http://example.invalid/x","body":"{}"}}"#, false),
    ] {
        let mut m = Manager::in_memory();
        m.install_bytes_by_user(&module(&manifest, Answer::Forward(req)), Some(grant.clone()), false).unwrap();
        let mut host = Writes::default();
        let Ok(Prepared::Dialog { error: Some(e), .. }) = m.prepare_command(id, "go", &["1".into()], &mut host) else {
            panic!("{req}: the declared dialog, with the refusal")
        };
        assert!(e.contains("can only read"), "{req}: {e}");
        let page = m.settings(id, &mut host).unwrap();
        assert!(page.error.is_some_and(|e| e.contains("can only read")), "{req}: settings.prepare refused too");
        assert_eq!(page.values["n"], 1.0, "{req}: unchanged");
        assert!(host.0.is_empty(), "{req}: nothing written");
        if runs {
            // the command itself may write
            m.run_command(id, "go", &json!({"b": true}), &["1".into()], &mut host).unwrap();
        }
    }
    let mut m = Manager::in_memory();
    m.install_bytes_by_user(
        &module(&manifest, Answer::Forward(r#"{"method":"metadata.setStandard","params":{"ids":["1"],"title":"t"}}"#)),
        Some(grant),
        false,
    )
    .unwrap();
    let mut host = Writes::default();
    m.run_command(id, "go", &json!({"b": true}), &["1".into()], &mut host).unwrap();
    assert_eq!(host.0.len(), 1, "written by the command");
}

/// Removing a plug-in can keep its settings and per-photo data for the next install of its id;
/// its grant goes either way, and a call still running when it was removed can't write its
/// settings over the kept file.
#[test]
fn removing_a_plugin_can_keep_its_data_and_settings() {
    let dir = temp("keep");
    let id = "test.settings";
    let fields = json!([{"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 3}]);
    let mut manifest = settings_manifest("1.0.0", fields);
    manifest["metadataFields"] = json!([{"key": "job", "label": "Job", "type": "text"}]);
    manifest["permissions"] = json!({"catalog": true});
    let bytes = module(&manifest, Answer::Forward(r#"{"method":"settings.set","params":{"key":"n","value":9}}"#));
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&bytes, None, false).unwrap();
    m.set_grant_by_user(id, lcc_plugins::Permissions { catalog: true, ..Default::default() }).unwrap();
    m.set_settings(id, &json!({"n": 5}), &mut NoHost).unwrap();
    m.set_metadata_field(id, "7", "job", &json!("J-1")).unwrap();
    let settings = dir.join("settings").join(format!("{id}.json"));
    let data = dir.join("data").join(format!("{id}.json"));
    let kept = std::fs::read(&settings).unwrap();

    // a call begun before the removal and run after it
    let Begun::Call(mut call) = m.begin_command(id, "go", &Value::Null, &[], &Value::Null, &mut NoHost).unwrap() else { panic!("a call") };
    m.uninstall_with(id, true).unwrap();
    let outcome = call.run(&mut NoHost);
    let _ = m.end(*call, Some(outcome));
    assert!(m.get(id).is_none() && !dir.join(format!("{id}.wasm")).exists(), "the module is gone");
    let state: Value = serde_json::from_slice(&std::fs::read(dir.join("plugins.json")).unwrap()).unwrap();
    assert!(state["plugins"].get(id).is_none(), "and its entry: {state}");
    assert_eq!(std::fs::read(&settings).unwrap(), kept, "the running call didn't write over the kept settings");
    assert!(data.exists(), "its data is kept");

    // installed again: its settings and data are back, its grant isn't
    m.install_bytes(&bytes, None, false).unwrap();
    assert_eq!(values(&mut m, id), json!({"n": 5.0}));
    let job = m.field_values().0.get("7").and_then(|v| v.get(&format!("{id}/job"))).cloned();
    assert_eq!(job, Some(json!("J-1")), "its per-photo data");
    assert!(!m.get(id).unwrap().effective().catalog, "nothing granted");

    // without keeping them, both go
    m.uninstall_with(id, false).unwrap();
    assert!(!settings.exists() && !data.exists());
}

/// A call begun before its plug-in was removed with its data, and ended after, writes no data file
/// back; its outcome still reaches the caller, and an install of the same id afterwards finds none
/// of the earlier data.
#[test]
fn a_call_ended_after_removal_writes_no_data_back() {
    let dir = temp("data-gone");
    let id = "test.settings";
    let fields = json!([{"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 3}]);
    let set = r#"{"method":"ns.set","params":{"photo":"p1","key":"k","value":1}}"#;
    let get = r#"{"method":"ns.get","params":{"photo":"p1","key":"k"}}"#;
    let data = dir.join("data").join(format!("{id}.json"));
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&module(&settings_manifest("1.0.0", fields.clone()), Answer::Forward(set)), None, false).unwrap();

    // a call that runs after the removal
    let Begun::Call(mut call) = m.begin_command(id, "go", &Value::Null, &[], &Value::Null, &mut NoHost).unwrap() else { panic!("a call") };
    m.uninstall(id).unwrap();
    let outcome = call.run(&mut NoHost);
    let back = m.end(*call, Some(outcome));
    assert!(back.is_ok(), "the outcome still goes back to the caller: {back:?}");
    assert!(!data.exists(), "the call wrote no data back");

    // installed again: nothing from the earlier install
    m.install_bytes(&module(&settings_manifest("1.0.0", fields), Answer::Forward(get)), None, false).unwrap();
    let got = m.run_command(id, "go", &Value::Null, &[], &mut NoHost).unwrap();
    assert_eq!(got, json!({}), "photo p1 has no data from the earlier install");
}

/// With its data kept, a call that ends after the removal saves its data as usual, and the next
/// install of the id finds it.
#[test]
fn a_call_ended_after_a_kept_removal_saves_its_data() {
    let dir = temp("data-kept");
    let id = "test.settings";
    let fields = json!([{"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 3}]);
    let set = r#"{"method":"ns.set","params":{"photo":"p1","key":"k","value":1}}"#;
    let get = r#"{"method":"ns.get","params":{"photo":"p1"}}"#;
    let data = dir.join("data").join(format!("{id}.json"));
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&module(&settings_manifest("1.0.0", fields.clone()), Answer::Forward(set)), None, false).unwrap();

    let Begun::Call(mut call) = m.begin_command(id, "go", &Value::Null, &[], &Value::Null, &mut NoHost).unwrap() else { panic!("a call") };
    m.uninstall_with(id, true).unwrap();
    let outcome = call.run(&mut NoHost);
    let back = m.end(*call, Some(outcome));
    assert!(back.is_ok(), "the outcome goes back to the caller: {back:?}");
    assert!(data.exists(), "its data was kept, so the call saves it");

    m.install_bytes(&module(&settings_manifest("1.0.0", fields), Answer::Forward(get)), None, false).unwrap();
    assert_eq!(m.run_command(id, "go", &Value::Null, &[], &mut NoHost).unwrap(), json!({"k": 1}), "the kept data is back");
}

/// A call whose data can't be written ends with that error, while the plug-in stays installed
/// and its outcome is still the call's own.
#[cfg(unix)]
#[test]
fn a_call_whose_data_cant_be_saved_reports_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp("data-locked");
    let id = "test.settings";
    let fields = json!([{"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 3}]);
    let set = r#"{"method":"ns.set","params":{"photo":"p1","key":"k","value":1}}"#;
    let mut m = Manager::open(&dir).unwrap();
    m.install_bytes(&module(&settings_manifest("1.0.0", fields), Answer::Forward(set)), None, false).unwrap();
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let Begun::Call(mut call) = m.begin_command(id, "go", &Value::Null, &[], &Value::Null, &mut NoHost).unwrap() else { panic!("a call") };
    std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let outcome = call.run(&mut NoHost);
    let back = m.end(*call, Some(outcome));
    std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(back, Err(Error::Io(_))), "the save error reaches the caller: {back:?}");
    assert!(m.get(id).is_some(), "still installed");
    assert!(!data_dir.join(format!("{id}.json")).exists(), "nothing written");
}
