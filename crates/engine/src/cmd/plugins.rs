//! Plug-in commands (P4.3): the Plugin Manager ([`lcc_plugins::Manager`]) behind engine commands,
//! so the UI, CLI, control channel and MCP drive plug-ins the same way. See docs/plugins.md.
//!
//! The manager is process-wide (plug-ins are installed per user, not per catalog) and lives in
//! `<PREFIX>_PLUGINS` or `<settings>/plugins`; unit tests keep it in memory.
//!
//! A plug-in reaches the catalog only through [`SessionHost`]: read-only queries, and
//! `photo.setMeta` for standard fields, each behind its permission (checked by `lcc-plugins`).

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use lcc_plugins::{HostApi, Manager, Permissions};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);

/// Where plug-ins are installed: `<PREFIX>_PLUGINS`, else `<settings>/plugins`.
pub fn plugins_dir() -> Option<PathBuf> {
    lcc_brand::env_os("PLUGINS").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| crate::config::config_dir().map(|d| d.join("plugins")))
}

/// The process-wide manager, opened on first use. A folder that can't be opened leaves an
/// in-memory manager with the reason in its `failed` list.
pub fn manager() -> MutexGuard<'static, Option<Manager>> {
    let mut g = manager_slot();
    if g.is_none() {
        let dir = if cfg!(test) { None } else { plugins_dir() };
        *g = Some(match dir {
            Some(d) => Manager::open(&d).unwrap_or_else(|e| {
                let mut m = Manager::in_memory();
                m.failed.push((d.display().to_string(), e.to_string()));
                m
            }),
            None => Manager::in_memory(),
        });
    }
    g
}

/// Keeps plug-ins in memory for the rest of the process instead of the settings folder's
/// `plugins` (unless the manager already opened). For tests of other crates, so installing a test
/// plug-in never touches the real user's settings.
pub fn use_in_memory_manager() {
    manager_slot().get_or_insert_with(Manager::in_memory);
}

fn manager_slot() -> MutexGuard<'static, Option<Manager>> {
    MANAGER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The manager pointed at `library` (a session's [`Session::plugin_library`]): plug-ins' data is
/// kept per library, and sessions with different libraries share the process-wide manager.
fn manager_for(library: &str) -> MutexGuard<'static, Option<Manager>> {
    let mut g = manager();
    if let Some(m) = g.as_mut()
        && m.library() != library
    {
        m.set_library(library);
    }
    g
}

fn with_manager<R>(f: impl FnOnce(&mut Manager) -> Result<R>) -> Result<R> {
    let mut g = manager();
    let m = g.get_or_insert_with(Manager::in_memory);
    f(m)
}

/// [`with_manager`] with the manager pointed at `library` (see [`manager_for`]).
fn with_library<R>(library: &str, f: impl FnOnce(&mut Manager) -> Result<R>) -> Result<R> {
    let mut g = manager_for(library);
    let m = g.get_or_insert_with(Manager::in_memory);
    f(m)
}

/// The catalog as plug-ins see it.
pub struct SessionHost<'a>(pub &'a mut Session);

/// Standard fields a plug-in may write with `metadataWrite`.
const STANDARD_FIELDS: &[&str] = &[
    "title",
    "caption",
    "altText",
    "extendedDescription",
    "copyright",
    "creator",
    "location",
    "city",
    "state",
    "country",
    "gps",
    "keywords",
    "addKeywords",
    "removeKeywords",
];

/// One photo id from a plug-in: a number, or the decimal string the SDK hands out in `selection`.
fn photo_id(v: &Value) -> std::result::Result<Value, String> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok())).map(Value::from).ok_or_else(|| format!("{v} is not a photo id"))
}

/// `id` / `ids` as the engine reads them (numbers). Plug-ins get ids as strings, and an id the
/// engine can't read must be an error: it would otherwise fall back to the app's current selection.
fn engine_ids(params: &Value) -> std::result::Result<Value, String> {
    let mut p = params.clone();
    if let Some(id) = params.get("id") {
        p["id"] = photo_id(id)?;
    }
    if let Some(ids) = params.get("ids") {
        let a = ids.as_array().ok_or("`ids` must be a list")?;
        p["ids"] = Value::Array(a.iter().map(photo_id).collect::<std::result::Result<_, _>>()?);
    }
    Ok(p)
}

impl HostApi for SessionHost<'_> {
    fn catalog(&mut self, method: &str, params: &Value) -> std::result::Result<Value, String> {
        // lcc-plugins only forwards the read-only methods; check again: this is the boundary
        if !matches!(method, "catalog.query" | "photo.inspect" | "catalog.stats" | "keyword.list") {
            return Err(format!("{method} is not a catalog read"));
        }
        let params = if params.is_object() { engine_ids(params)? } else { params.clone() };
        self.0.execute(method, &params).map_err(|e| e.to_string())
    }

    fn set_metadata(&mut self, params: &Value) -> std::result::Result<Value, String> {
        let params = &engine_ids(params)?;
        let ids = params.get("ids").and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or("metadata.setStandard needs `ids`")?;
        let mut p = serde_json::Map::new();
        p.insert("ids".into(), Value::Array(ids.clone()));
        for (k, v) in params.as_object().into_iter().flatten() {
            if STANDARD_FIELDS.contains(&k.as_str()) {
                p.insert(k.clone(), v.clone());
            } else if k != "ids" {
                return Err(format!("metadata.setStandard: `{k}` is not a standard field"));
            }
        }
        self.0.execute("photo.setMeta", &Value::Object(p)).map_err(|e| e.to_string())
    }
}

fn plugin_err(cmd: &str, e: lcc_plugins::Error) -> crate::EngineError {
    bad(cmd, e.to_string())
}

fn id_param<'a>(p: &'a Value, cmd: &str) -> Result<&'a str> {
    str_param(p, "id").or_else(|| str_param(p, "plugin")).ok_or_else(|| bad(cmd, "missing `id`"))
}

fn grant_param(p: &Value, cmd: &str) -> Result<Option<Permissions>> {
    match p.get("grant") {
        None | Some(Value::Null) => Ok(None),
        Some(g) => serde_json::from_value(g.clone()).map(Some).map_err(|e| bad(cmd, format!("grant: {e}"))),
    }
}

fn list(_: &mut Session, _: &Value) -> Result<Value> {
    with_manager(|m| {
        let mut plugins: Vec<Value> = m.list().map(|p| p.info()).collect();
        // pixel-filter plug-ins (kind "filter"); the manager can't load their modules
        plugins.extend(crate::filter_plugins::registry().list().iter().map(|f| crate::filter_plugins::info(f)));
        let filter_files = crate::filter_plugins::sources();
        let failed: Vec<Value> = m.failed.iter().filter(|(f, _)| !filter_files.contains(f)).map(|(f, e)| json!({"file": f, "error": e})).collect();
        Ok(json!({"dir": m.dir(), "plugins": plugins, "failed": failed}))
    })
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.install";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path` (a .wasm file)"))?;
    let grant = grant_param(p, C)?;
    if crate::filter_plugins::is_filter_file(std::path::Path::new(path)) {
        // a pixel filter: no capabilities to grant, nothing for the manager
        let f = crate::filter_plugins::install_file(std::path::Path::new(path)).map_err(|e| bad(C, e))?;
        return Ok(crate::filter_plugins::info(&f));
    }
    let lib = s.plugin_library.clone();
    let r = with_library(&lib, |m| m.install_file(std::path::Path::new(path), grant).map(|i| i.info()).map_err(|e| plugin_err(C, e)))?;
    sync_fields(s);
    Ok(r)
}

fn inspect_file(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.inspectFile";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let bytes = std::fs::read(path).map_err(|e| bad(C, format!("{path}: {e}")))?;
    if lcc_filters::is_filter_module(&bytes) {
        let f = lcc_filters::Filter::load(&bytes, lcc_filters::Limits::default()).map_err(|e| bad(C, e.to_string()))?;
        let installed =
            crate::filter_plugins::registry().get(f.id()).map(|i| json!({"version": i.manifest().version, "moduleHash": i.module_hash()}));
        return Ok(json!({"manifest": crate::filter_plugins::info(&f), "installed": installed}));
    }
    let plugin = lcc_plugins::Plugin::load(&bytes, lcc_plugins::Limits::default()).map_err(|e| plugin_err(C, e))?;
    let manifest = serde_json::to_value(plugin.manifest()).map_err(|e| bad(C, e.to_string()))?;
    // an update: what is granted now, so the dialog can show what the new version adds
    let installed = with_manager(|m| Ok(m.get(plugin.id()).map(|i| json!({"version": i.plugin.manifest().version, "granted": i.effective()}))))?;
    Ok(json!({"manifest": manifest, "installed": installed}))
}

fn uninstall(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.uninstall";
    let id = id_param(p, C)?;
    if crate::filter_plugins::uninstall(id).map_err(|e| bad(C, e))? {
        return Ok(Value::Null);
    }
    let lib = s.plugin_library.clone();
    with_library(&lib, |m| m.uninstall(id).map(|_| Value::Null).map_err(|e| plugin_err(C, e)))?;
    sync_fields(s);
    Ok(Value::Null)
}

fn enable(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.enable";
    let id = id_param(p, C)?;
    let on = bool_or(p, "enabled", true);
    let lib = s.plugin_library.clone();
    let r = with_library(&lib, |m| m.set_enabled(id, on).map(|_| json!({"id": id, "enabled": on})).map_err(|e| plugin_err(C, e)))?;
    sync_fields(s);
    Ok(r)
}

fn set_grant(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.grant";
    let id = id_param(p, C)?;
    let grant = grant_param(p, C)?.ok_or_else(|| bad(C, "missing `grant`"))?;
    with_manager(|m| m.set_grant(id, grant).map(|g| json!({"id": id, "granted": g})).map_err(|e| plugin_err(C, e)))
}

fn log(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.log";
    let id = id_param(p, C)?;
    with_manager(|m| m.get(id).map(|i| json!(i.log)).ok_or_else(|| bad(C, format!("no plug-in {id:?} is installed"))))
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.run";
    let id = id_param(p, C)?;
    let command = str_param(p, "command").ok_or_else(|| bad(C, "missing `command`"))?;
    let args = p.get("args").cloned().unwrap_or(Value::Null);
    let selection: Vec<String> = s.targets(p)?.iter().map(|i| i.0.to_string()).collect();
    let lib = s.plugin_library.clone();
    let mut host = SessionHost(s);
    let r = with_library(&lib, |m| m.run_command(id, command, &args, &selection, &mut host).map_err(|e| plugin_err(C, e)));
    // the command may have written the plug-in's own fields
    sync_fields(s);
    r
}

fn commands(_: &mut Session, _: &Value) -> Result<Value> {
    with_manager(|m| {
        let v: Vec<Value> = m
            .list()
            .filter(|p| p.enabled)
            .flat_map(|p| {
                let pm = p.plugin.manifest();
                pm.commands.iter().map(
                    move |c| json!({"plugin": pm.id, "pluginName": pm.name, "command": c.id, "label": c.label, "menu": c.menu, "dialog": c.dialog}),
                )
            })
            .collect();
        Ok(Value::Array(v))
    })
}

fn suggest(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.suggestMetadata";
    let ids = s.targets(p)?;
    let mut photos = Vec::new();
    for id in ids.iter().take(1000) {
        let mut v = s.execute("photo.inspect", &json!({"id": id.0})).map_err(|e| bad(C, e.to_string()))?;
        if v.get("id").is_none() {
            v["id"] = json!(id.0.to_string());
        }
        photos.push(v);
    }
    let lib = s.plugin_library.clone();
    let mut host = SessionHost(s);
    with_library(&lib, |m| Ok(Value::Array(m.suggest_metadata(&photos, &mut host))))
}

fn post_export(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.postExport";
    let files = p.get("files").and_then(Value::as_array).ok_or_else(|| bad(C, "missing `files: [{path, photo?}]`"))?;
    let lib = s.plugin_library.clone();
    let mut host = SessionHost(s);
    let r = with_library(&lib, |m| Ok(Value::Array(m.post_export(files, &mut host))));
    sync_fields(s);
    r
}

fn services(_: &mut Session, _: &Value) -> Result<Value> {
    with_manager(|m| {
        Ok(Value::Array(
            m.publish_services()
                .into_iter()
                .map(|(plugin, s)| json!({"kind": format!("{KIND_PREFIX}{plugin}"), "plugin": plugin, "id": s.id, "name": s.name}))
                .collect(),
        ))
    })
}

/// Publish-service kinds of plug-ins: `plugin:<plugin id>`.
pub const KIND_PREFIX: &str = "plugin:";

/// Is `kind` an installed, enabled plug-in publish service?
pub fn is_plugin_kind(kind: &str) -> bool {
    kind.strip_prefix(KIND_PREFIX).is_some_and(|id| manager().as_ref().is_some_and(|m| m.publish_services().iter().any(|(p, _)| p == id)))
}

/// Opens a `plugin:<id>` service; `None` for other kinds (the built-in ones).
pub fn open_plugin_service(
    svc: &lcc_publish::ServiceConfig,
) -> Option<std::result::Result<Box<dyn lcc_publish::PublishService>, lcc_publish::PublishError>> {
    let id = svc.kind.strip_prefix(KIND_PREFIX)?;
    if !is_plugin_kind(&svc.kind) {
        return Some(Err(lcc_publish::PublishError::Config(format!("plug-in {id} is not installed, enabled, or a publish service"))));
    }
    let dir =
        std::env::temp_dir().join(format!("plugin-publish-{}-{}", std::process::id(), svc.id.replace(|c: char| !c.is_ascii_alphanumeric(), "_")));
    Some(Ok(Box::new(PluginService { plugin: id.to_string(), dir })))
}

/// A plug-in's publish service as a [`lcc_publish::PublishService`]: each rendered photo is
/// written to a private temporary file, lent to the plug-in's `publish.upload` call, then deleted.
pub struct PluginService {
    plugin: String,
    dir: PathBuf,
}

fn pub_err(e: lcc_plugins::Error) -> lcc_publish::PublishError {
    lcc_publish::PublishError::Service(e.to_string())
}

impl lcc_publish::PublishService for PluginService {
    fn kind(&self) -> &'static str {
        "plugin"
    }

    fn publish(&mut self, up: &lcc_publish::Upload<'_>) -> std::result::Result<lcc_publish::Published, lcc_publish::PublishError> {
        use lcc_plugins::publish::{PluginPublisher, PublishItem, PublishProvider};
        let io = |e: std::io::Error| lcc_publish::PublishError::Io(e.to_string());
        std::fs::create_dir_all(&self.dir).map_err(io)?;
        // only the file name, never a path the export settings could smuggle in
        let name: String = up.file_name.rsplit(['/', '\\']).next().unwrap_or("photo").chars().filter(|c| !c.is_control()).take(200).collect();
        let name = if name.is_empty() || name == "." || name == ".." { "photo".to_string() } else { name };
        let path = self.dir.join(name);
        std::fs::write(&path, up.bytes).map_err(io)?;
        let item = PublishItem {
            path: path.display().to_string(),
            photo: up.photo.0.to_string(),
            remote_id: up.previous.map(str::to_string),
            ..Default::default()
        };
        let r = {
            let mut g = manager();
            let m = g.get_or_insert_with(Manager::in_memory);
            PluginPublisher::new(m, &self.plugin).and_then(|mut p| p.upload(&mut lcc_plugins::NoHost, &item))
        };
        let _ = std::fs::remove_file(&path);
        r.map(|r| lcc_publish::Published { remote_id: r.remote_id }).map_err(pub_err)
    }

    fn remove(&mut self, remote_id: &str) -> std::result::Result<(), lcc_publish::PublishError> {
        use lcc_plugins::publish::{PluginPublisher, PublishProvider};
        let mut g = manager();
        let m = g.get_or_insert_with(Manager::in_memory);
        PluginPublisher::new(m, &self.plugin).and_then(|mut p| p.delete(&mut lcc_plugins::NoHost, remote_id)).map_err(pub_err)
    }
}

impl Drop for PluginService {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// After an export finished: runs the enabled export hooks on the written files (`[{path, …}]`),
/// then, when the export named one (`pluginDestination: {plugin, destination, args?}`), hands
/// the files to that plug-in export destination (its result is the last entry, with
/// `destination` set). `None` when nothing ran. Runs here, on the caller's thread; the app runs
/// the same calls on a worker ([`export_hooks`], [`ExportHooks::run`]).
pub fn after_export(s: &mut Session, files: &[Value], destination: Option<&Value>) -> Option<Vec<Value>> {
    let hooks = export_hooks(&s.plugin_library, files, destination)?;
    let r = hooks.run(&mut SessionHost(s), &|| false, &mut |_, _| {});
    // hooks may have written their own fields (a "delivered" mark)
    sync_fields(s);
    Some(r)
}

/// The plug-in calls an export makes ([`after_export`]), planned under the manager's lock and run
/// without it, so a long upload neither blocks the manager nor (on a worker) the window.
pub struct ExportHooks {
    /// The library of the session the export ran in (see [`Session::plugin_library`]).
    library: String,
    #[allow(clippy::type_complexity)]
    calls: Vec<(String, Option<String>, std::result::Result<(Value, Vec<PathBuf>), lcc_plugins::Error>)>,
}

/// The plug-in calls after an export of `files` to `destination` from a session with `library`
/// open ([`Session::plugin_library`], see [`after_export`]); `None` when there are none.
pub fn export_hooks(library: &str, files: &[Value], destination: Option<&Value>) -> Option<ExportHooks> {
    let files: Vec<Value> = files.iter().filter(|f| f.get("path").and_then(Value::as_str).is_some()).cloned().collect();
    let destination = destination.filter(|d| d.is_object());
    if files.is_empty() {
        return None;
    }
    let dest = destination
        .map(|d| (d["plugin"].as_str().unwrap_or_default(), d["destination"].as_str().unwrap_or_default(), d.get("args").unwrap_or(&Value::Null)));
    let calls = manager_for(library).as_ref().map(|m| m.export_calls(&files, dest)).unwrap_or_default();
    (!calls.is_empty()).then_some(ExportHooks { library: library.to_string(), calls })
}

impl ExportHooks {
    /// How many plug-in calls there are.
    pub fn len(&self) -> usize {
        self.calls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Runs the calls in order, the manager's lock held only to hand each one out and back.
    /// `cancelled` is asked before each call (a running plug-in call can't be interrupted): the
    /// rest are reported as cancelled. `progress(done, plug-in id)` before each. One result per
    /// call: `{plugin, ok | error}`, the destination's with `destination` too.
    pub fn run(self, host: &mut dyn HostApi, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(usize, &str)) -> Vec<Value> {
        let mut out = Vec::with_capacity(self.calls.len());
        for (k, (id, dest, call)) in self.calls.into_iter().enumerate() {
            progress(k, &id);
            let r = if cancelled() {
                Err("cancelled".to_string())
            } else {
                call.map_err(|e| e.to_string()).and_then(|(req, lent)| {
                    let mut pending =
                        with_library(&self.library, |m| m.begin(&id, req, lent).map_err(|e| plugin_err("plugin", e))).map_err(|e| e.to_string())?;
                    let outcome = pending.run(host);
                    let mut g = manager_for(&self.library);
                    let m = g.get_or_insert_with(Manager::in_memory);
                    m.end(pending, Some(outcome)).map_err(|e| e.to_string())
                })
            };
            let mut v = match r {
                Ok(v) => json!({"plugin": id, "ok": v}),
                Err(e) => json!({"plugin": id, "error": e}),
            };
            if let Some(d) = dest {
                v["destination"] = json!(d);
            }
            out.push(v);
        }
        out
    }
}

/// A catalog request a plug-in made from a worker thread ([`relay_host`]), answered on the thread that owns
/// the session with [`HostRequest::answer`].
pub struct HostRequest {
    /// `None`: `metadata.setStandard`; else the catalog read.
    method: Option<String>,
    params: Value,
    reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
}

impl HostRequest {
    /// Answers the request from `s`, with the same checks as a call on the session's thread.
    pub fn answer(self, s: &mut Session) {
        let mut host = SessionHost(s);
        let r = match &self.method {
            Some(m) => host.catalog(m, &self.params),
            None => host.set_metadata(&self.params),
        };
        let _ = self.reply.send(r);
    }
}

/// What plug-ins running on a worker see of the catalog: each request goes to the session's thread
/// (see [`relay_host`]) and waits for its answer.
pub struct RelayHost(std::sync::mpsc::Sender<HostRequest>);

impl RelayHost {
    fn ask(&self, method: Option<String>, params: &Value) -> std::result::Result<Value, String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.0.send(HostRequest { method, params: params.clone(), reply }).map_err(|_| "the catalog is closed".to_string())?;
        rx.recv().map_err(|_| "the catalog is closed".to_string())?
    }
}

impl HostApi for RelayHost {
    fn catalog(&mut self, method: &str, params: &Value) -> std::result::Result<Value, String> {
        self.ask(Some(method.to_string()), params)
    }

    fn set_metadata(&mut self, params: &Value) -> std::result::Result<Value, String> {
        self.ask(None, params)
    }
}

/// A host for plug-in calls on a worker, and where its requests arrive for the session's thread to answer.
pub fn relay_host() -> (RelayHost, std::sync::mpsc::Receiver<HostRequest>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (RelayHost(tx), rx)
}

fn destinations(_: &mut Session, _: &Value) -> Result<Value> {
    with_manager(|m| {
        let names: std::collections::BTreeMap<String, String> =
            m.list().map(|p| (p.plugin.id().to_string(), p.plugin.manifest().name.clone())).collect();
        Ok(Value::Array(
            m.export_destinations()
                .into_iter()
                .map(|(plugin, d)| json!({"plugin": plugin, "pluginName": names.get(&plugin), "destination": d.id, "name": d.name, "dialog": d.dialog}))
                .collect(),
        ))
    })
}

fn export_to(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.exportTo";
    let id = id_param(p, C)?;
    let dest = str_param(p, "destination").ok_or_else(|| bad(C, "missing `destination`"))?;
    let files = p.get("files").and_then(Value::as_array).ok_or_else(|| bad(C, "missing `files: [{path, photo?}]`"))?;
    let args = p.get("args").cloned().unwrap_or(Value::Null);
    let lib = s.plugin_library.clone();
    let mut host = SessionHost(s);
    let r = with_library(&lib, |m| m.export_to(id, dest, &args, files, &mut host).map_err(|e| plugin_err(C, e)));
    sync_fields(s);
    r
}

/// The photo a field command is about: `id`, else the active photo.
fn field_photo(s: &Session, p: &Value, cmd: &str) -> Result<String> {
    match p.get("id").or_else(|| p.get("photo")) {
        Some(v) => photo_id(v).map(|v| v.to_string()).map_err(|e| bad(cmd, e)),
        None => s.active().map(|i| i.0.to_string()).ok_or_else(|| bad(cmd, "no photo (pass `id`)")),
    }
}

/// Most photos one field command reads or writes.
const MAX_FIELD_PHOTOS: usize = 10_000;

/// The photos a field command is about: `ids` (numbers or the decimal strings plug-ins get),
/// else `id`, else the active photo.
fn field_photos(s: &Session, p: &Value, cmd: &str) -> Result<Vec<String>> {
    match p.get("ids").and_then(Value::as_array) {
        Some(a) if !a.is_empty() => a.iter().take(MAX_FIELD_PHOTOS).map(|v| photo_id(v).map(|v| v.to_string()).map_err(|e| bad(cmd, e))).collect(),
        _ => field_photo(s, p, cmd).map(|v| vec![v]),
    }
}

fn metadata_fields(s: &mut Session, p: &Value) -> Result<Value> {
    let photos = field_photos(s, p, "plugin.metadataFields")?;
    let lib = s.plugin_library.clone();
    with_library(&lib, |m| {
        let mut merged: Vec<Value> = Vec::new();
        for (n, photo) in photos.iter().enumerate() {
            for (k, plugin) in m.metadata_fields(photo).into_iter().enumerate() {
                let Some(into) = merged.get_mut(k) else {
                    merged.push(plugin);
                    continue;
                };
                // a field whose value differs between the photos is mixed (no value)
                let (Some(have), Some(new)) = (into.get_mut("fields").and_then(Value::as_array_mut), plugin["fields"].as_array()) else { continue };
                for (f, g) in have.iter_mut().zip(new) {
                    if n > 0 && f["value"] != g["value"] {
                        f["value"] = Value::Null;
                        f["mixed"] = json!(true);
                    }
                }
            }
        }
        let shown = if photos.len() == 1 { json!(photos[0]) } else { json!(photos) };
        Ok(json!({"photo": shown, "count": photos.len(), "plugins": merged}))
    })
}

fn set_field(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.setField";
    let photos = field_photos(s, p, C)?;
    let plugin = str_param(p, "plugin").ok_or_else(|| bad(C, "missing `plugin`"))?;
    let key = str_param(p, "field").ok_or_else(|| bad(C, "missing `field`"))?;
    let value = p.get("value").cloned().unwrap_or(Value::Null);
    let lib = s.plugin_library.clone();
    let set = with_library(&lib, |m| {
        let mut v = Value::Null;
        for photo in &photos {
            v = m.set_metadata_field(plugin, photo, key, &value).map_err(|e| plugin_err(C, e))?;
        }
        Ok(v)
    })?;
    sync_fields(s);
    let shown = if photos.len() == 1 { json!(photos[0]) } else { json!(photos) };
    Ok(json!({"photo": shown, "count": photos.len(), "field": key, "value": set}))
}

/// Plug-in metadata fields read from XMP sidecars (`(photo, [(plug-in id, key, value text)])`) go
/// back into the plug-ins' own data, the source, so a library rebuilt or moved to another machine
/// keeps them. Plug-ins that aren't installed or enabled, unknown fields and values a field doesn't
/// take are skipped (logged); read-only fields are restored too. Returns how many values changed.
pub fn restore_fields(s: &mut Session, items: &[(lcc_catalog::PhotoId, Vec<(String, String, String)>)]) -> usize {
    if items.iter().all(|(_, f)| f.is_empty()) {
        return 0;
    }
    let mut n = 0;
    {
        let mut g = manager_for(&s.plugin_library);
        let Some(m) = g.as_mut() else { return 0 };
        for (id, fields) in items {
            if s.catalog.photo(*id).is_none() {
                continue;
            }
            let photo = id.0.to_string();
            for (plugin, key, text) in fields {
                match m.restore_metadata_field(plugin, &photo, key, text) {
                    Ok(true) => n += 1,
                    Ok(false) => {}
                    Err(e) => log::warn!("plug-in field {plugin}/{key} from XMP: {e}"),
                }
            }
        }
    }
    // the sidecars already say this: only the index is rebuilt (also when the plug-in's data
    // already held the values: these photo records may be new)
    sync_fields_with(s, false);
    n
}

/// The plug-in metadata fields rules can test: `(rule field id, "Plug-in ▸ Field")` of every enabled plug-in.
pub fn rule_fields() -> Vec<(String, String)> {
    let g = manager();
    let Some(m) = g.as_ref() else { return Vec::new() };
    m.list()
        .filter(|p| p.enabled)
        .flat_map(|p| {
            let man = p.plugin.manifest();
            man.metadata_fields
                .iter()
                .map(|f| (lcc_catalog::rules::plugin_field_id(&man.id, &f.key), format!("{} ▸ {}", man.name, f.label)))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Bring the catalog's in-memory index of the plug-ins' metadata fields ([`lcc_catalog::Photo::plugin_fields`],
/// what rules search and sidecars carry) in line with the plug-ins' own data, the source. Nothing is
/// saved with the catalog; with XMP auto-write on, the changed photos' sidecars are written. A plug-in
/// whose data can't be read now keeps its entries. Returns how many photos changed.
pub fn sync_fields(s: &mut Session) -> usize {
    sync_fields_with(s, true)
}

/// [`sync_fields`]; `write_sidecars: false` only builds the index (a library just opened: its
/// sidecars already have what the plug-ins hold).
pub(crate) fn sync_fields_with(s: &mut Session, write_sidecars: bool) -> usize {
    let (values, skipped) = {
        let mut g = manager_for(&s.plugin_library);
        // rules compare number fields as numbers
        if let Some(m) = g.as_ref() {
            lcc_catalog::rules::set_numeric_plugin_fields(m.list().filter(|p| p.enabled).flat_map(|p| {
                let man = p.plugin.manifest();
                man.metadata_fields
                    .iter()
                    .filter(|f| matches!(f.kind, lcc_plugins::FieldKind::Number { .. }))
                    .map(|f| format!("{}/{}", man.id, f.key))
                    .collect::<Vec<_>>()
            }));
        }
        g.as_mut().map(|m| m.field_values()).unwrap_or_default()
    };
    let kept = |k: &str| skipped.iter().any(|p| k.split_once('/').is_some_and(|(id, _)| id == p));
    let mut changes = Vec::new();
    for p in s.catalog.photos() {
        let want_new = values.get(&p.id.0.to_string());
        if p.plugin_fields.is_empty() && want_new.is_none() {
            continue;
        }
        let mut want: std::collections::BTreeMap<String, Value> = want_new.cloned().unwrap_or_default();
        for (k, v) in &p.plugin_fields {
            if kept(k) {
                want.insert(k.clone(), v.clone());
            }
        }
        if want != p.plugin_fields {
            changes.push((p.id, want));
        }
    }
    let ids: Vec<lcc_catalog::PhotoId> = changes.iter().map(|(id, _)| *id).collect();
    for (id, fields) in changes {
        if let Err(e) = s.catalog.set_plugin_fields(id, fields) {
            log::warn!("plug-in fields: {e}");
        }
    }
    let n = ids.len();
    if write_sidecars && s.xmp.auto_write && s.interaction.is_none() {
        s.auto_write_sidecars_of(ids);
    }
    n
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "plugin.list", "Plug-ins", [], None, "{} → {dir, plugins: [{id, name, version, enabled, requested, granted, commands, hooks, publish}], failed}", always, list),
        cmd!(query "plugin.inspectFile", "Inspect Plug-in File", [], None, "{path} — loads a .wasm without installing it → {manifest, installed: {version, granted} | null}; the install dialog shows each requested permission for approval", always, inspect_file),
        cmd!(query "plugin.install", "Install Plug-in", [], None, "{path: .wasm, grant?: {catalog, metadataWrite, network: [hosts], fs: [{path, write}]}} — installs or updates with the permissions the user approved (each one shown first, see plugin.inspectFile); without `grant` nothing new is granted: none on a first install, the previous grant on an update → plugin", always, install),
        cmd!(query "plugin.uninstall", "Remove Plug-in", [], None, "{id} — removes the module and its data", always, uninstall),
        cmd!(query "plugin.enable", "Enable Plug-in", [], None, "{id, enabled?: bool = true}", always, enable),
        cmd!(query "plugin.grant", "Set Plug-in Permissions", [], None, "{id, grant} — replaces the grant (revokes what it leaves out; never more than requested) → granted", always, set_grant),
        cmd!(query "plugin.log", "Plug-in Log", [], None, "{id} → [lines]", always, log),
        cmd!(query "plugin.commands", "Plug-in Commands", [], None, "{} → [{plugin, pluginName, command, label, menu, dialog}] of enabled plug-ins", always, commands),
        cmd!(query "plugin.run", "Run Plug-in Command", [], None, "{plugin, command, args?: dialog values, ids?} — runs a plug-in menu command on ids / the selection → its result", always, run),
        cmd!(query "plugin.suggestMetadata", "Suggest Metadata (Plug-ins)", [], None, "{ids?} — asks metadata providers about the photos → [{plugin, ok: {suggestions}} | {plugin, error}]; writes nothing", always, suggest),
        cmd!(query "plugin.postExport", "Run Export Hooks", [], None, "{files: [{path, photo?}]} — runs plug-in export post-process hooks on exported files → [{plugin, ok|error}]", always, post_export),
        cmd!(query "plugin.exportDestinations", "Plug-in Export Destinations", [], None, "{} → [{plugin, pluginName, destination, name, dialog}] of enabled plug-ins; pass one as app.export {pluginDestination: {plugin, destination, args}}", always, destinations),
        cmd!(query "plugin.exportTo", "Send to Plug-in Destination", [], None, "{plugin, destination, files: [{path, photo?}], args?: dialog values} — hands exported files to a plug-in export destination → its result", always, export_to),
        cmd!(query "plugin.metadataFields", "Plug-in Metadata Fields", [], None, "{ids?: photos | id?: photo (default the active one)} → {photo, count, plugins: [{plugin, pluginName, fields: [{key, label, type, readOnly, value, mixed?}]} | {plugin, error}]}; with several photos a field whose values differ is `mixed` (value null)", always, metadata_fields),
        cmd!(query "plugin.setField", "Set Plug-in Metadata Field", [], None, "{plugin, field, value (null clears), ids?: photos | id?: photo (default the active one)} — read-only fields and values the field doesn't take are refused → {photo, count, field, value}; the catalog's copy (rules `plugin:<plugin>/<field>`, Any Searchable Text, XMP) follows", always, set_field),
        cmd!(query "plugin.publishServices", "Plug-in Publish Services", [], None, "{} → [{kind: plugin:<id> (a publish.addService kind), plugin, id, name}]", always, services),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manager is process-wide: tests that install plug-ins take turns, so one test's export
    /// hooks never hold another's plug-in busy.
    static TURN: Mutex<()> = Mutex::new(());

    fn turn() -> MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A plug-in publish service whose upload answers `{remoteId: "r1"}` and whose export hook
    /// answers `{seen: true}`.
    fn publisher_module() -> Vec<u8> {
        let manifest = r#"{"id":"test.pub","name":"Pub","hooks":{"export":true},"publish":{"id":"p","name":"P"},"exportDestinations":[{"id":"d","name":"D"}],"metadataFields":[{"key":"job","label":"Job","type":"text"},{"key":"ro","label":"RO","type":"bool","readOnly":true}]}"#;
        let reply = r#"{"ok":{"remoteId":"r1","seen":true}}"#;
        let esc = |s: &str| s.replace('"', "\\\"");
        let src = format!(
            r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const {rl}) (i64.const 32)) (i64.const 8192))))"#,
            m = esc(manifest),
            r = esc(reply),
            ml = manifest.len(),
            rl = reply.len()
        );
        wat::parse_str(&src).unwrap()
    }

    #[test]
    fn plugin_publish_service_and_export_hook() {
        let _turn = turn();
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&publisher_module(), None).unwrap();
        assert!(is_plugin_kind("plugin:test.pub"));
        assert!(!is_plugin_kind("plugin:nope") && !is_plugin_kind("hardDrive"));
        let svc: lcc_publish::ServiceConfig = serde_json::from_value(json!({"id": "svc-9", "kind": "plugin:test.pub", "name": "P"})).unwrap();
        let mut service = open_plugin_service(&svc).unwrap().unwrap();
        let up = lcc_publish::Upload { photo: lcc_catalog::PhotoId(7), file_name: "../evil/a.jpg", bytes: b"jpeg", sidecars: &[], previous: None };
        assert_eq!(service.publish(&up).unwrap().remote_id, "r1");
        service.remove("r1").unwrap();
        let mut s = Session::with_demo();
        let r = after_export(&mut s, &[json!({"path": "/tmp/x.jpg"})], None).unwrap();
        assert!(r.iter().any(|v| v["plugin"] == "test.pub" && v["ok"]["seen"] == true), "{r:?}");
        assert!(after_export(&mut s, &[json!({"error": "failed"})], None).is_none(), "no written files, no hooks");
        // an export destination named by the export runs after the hooks
        let d = json!({"plugin": "test.pub", "destination": "d"});
        let r = after_export(&mut s, &[json!({"path": "/tmp/x.jpg"})], Some(&d)).unwrap();
        assert_eq!(r.last().unwrap()["destination"], "d", "{r:?}");
        let bad = json!({"plugin": "test.pub", "destination": "nope"});
        assert!(after_export(&mut s, &[json!({"path": "/tmp/x.jpg"})], Some(&bad)).unwrap().last().unwrap()["error"].is_string());
        let l = s.execute("plugin.exportDestinations", &json!({})).unwrap();
        assert!(l.as_array().unwrap().iter().any(|x| x["plugin"] == "test.pub" && x["destination"] == "d" && x["pluginName"] == "Pub"), "{l}");
        assert!(s.execute("plugin.exportTo", &json!({"plugin": "test.pub", "destination": "d"})).is_err(), "files required");
        // export validation accepts a well-formed destination only
        assert!(crate::export::ExportOptions::validate("app.export", &json!({"pluginDestination": d})).is_ok());
        assert!(crate::export::ExportOptions::validate("app.export", &json!({"pluginDestination": "d"})).is_err());

        // custom metadata fields: listed for a photo, edited, read-only refused
        let id = s.visible_cloned()[0].0;
        let field = |s: &mut Session, id: Value| {
            let f = s.execute("plugin.metadataFields", &json!({"id": id})).unwrap();
            f["plugins"].as_array().unwrap().iter().find(|p| p["plugin"] == "test.pub").cloned().unwrap()["fields"][0].clone()
        };
        assert_eq!(field(&mut s, json!(id))["key"], "job");
        s.execute("plugin.setField", &json!({"plugin": "test.pub", "field": "job", "value": "J-9", "id": id})).unwrap();
        assert_eq!(field(&mut s, json!(id.to_string()))["value"], "J-9");
        assert!(s.execute("plugin.setField", &json!({"plugin": "test.pub", "field": "ro", "value": true, "id": id})).is_err());
        assert!(s.execute("plugin.setField", &json!({"plugin": "test.pub", "field": "job", "value": 5, "id": id})).is_err());
        assert!(s.execute("plugin.setField", &json!({"plugin": "test.pub", "field": "job", "value": "x", "id": "bogus"})).is_err());
        manager().get_or_insert_with(Manager::in_memory).uninstall("test.pub").unwrap();
    }

    /// An export hook plug-in `id` that answers `{seen: true}` (no catalog access).
    fn hook_module(id: &str) -> Vec<u8> {
        let manifest = format!(r#"{{"id":"{id}","name":"Hook","hooks":{{"export":true}},"exportDestinations":[{{"id":"d","name":"D"}}]}}"#);
        let reply = r#"{"ok":{"seen":true}}"#;
        let esc = |s: &str| s.replace('"', "\\\"");
        let src = format!(
            r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const {rl}) (i64.const 32)) (i64.const 8192))))"#,
            m = esc(&manifest),
            r = esc(reply),
            ml = manifest.len(),
            rl = reply.len()
        );
        wat::parse_str(&src).unwrap()
    }

    /// Export hooks planned on one thread run on another, the manager's lock free between the
    /// calls; the catalog reaches them through the relay; cancelling skips the calls not yet made.
    #[test]
    fn export_hooks_run_on_a_worker_and_relay_catalog_requests() {
        let _turn = turn();
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&hook_module("test.relay"), None).unwrap();
        let files = [json!({"path": "/tmp/x.jpg"})];
        let d = json!({"plugin": "test.relay", "destination": "d"});
        let hooks = export_hooks("", &files, Some(&d)).unwrap();
        let (mut host, requests) = relay_host();
        let worker = std::thread::spawn(move || {
            // a catalog read from the worker waits for the session's thread to answer it
            let stats = host.catalog("catalog.stats", &json!({}));
            let r = hooks.run(&mut host, &|| false, &mut |_, _| {});
            (stats, r)
        });
        let mut s = Session::with_demo();
        requests.recv_timeout(std::time::Duration::from_secs(20)).unwrap().answer(&mut s);
        let (stats, r) = worker.join().unwrap();
        assert!(stats.unwrap().is_object());
        let mine: Vec<&Value> = r.iter().filter(|v| v["plugin"] == "test.relay").collect();
        assert!(mine.iter().any(|v| v["ok"]["seen"] == true && v.get("destination").is_none()), "{r:?}");
        assert_eq!(mine.last().unwrap()["destination"], "d", "{r:?}");
        // the relay with nobody answering: an error, not a hang
        let (mut host, requests) = relay_host();
        drop(requests);
        assert!(host.catalog("catalog.stats", &json!({})).is_err());
        // cancelled before they start: no plug-in runs, none stays busy
        let r = export_hooks("", &files, None).unwrap().run(&mut lcc_plugins::NoHost, &|| true, &mut |_, _| {});
        assert!(r.iter().all(|v| v["error"] == "cancelled"), "{r:?}");
        // a plug-in whose call is out (on a worker) is busy for others until it comes back
        let call = with_manager(|m| m.begin("test.relay", json!({"hook": "export", "files": []}), vec![]).map_err(|e| plugin_err("t", e))).unwrap();
        let busy = after_export(&mut s, &files, None).unwrap();
        assert!(busy.iter().any(|v| v["plugin"] == "test.relay" && v["error"].as_str().is_some_and(|e| e.contains("busy"))), "{busy:?}");
        assert!(with_manager(|m| m.end(call, None).map_err(|e| plugin_err("t", e))).is_err(), "never ran: reported as cancelled");
        let again = after_export(&mut s, &files, None).unwrap();
        assert!(again.iter().any(|v| v["plugin"] == "test.relay" && v["ok"]["seen"] == true), "{again:?}");
        manager().get_or_insert_with(Manager::in_memory).uninstall("test.relay").unwrap();
    }

    /// A plug-in with custom metadata fields `job` (text), `ok` (bool) and `n` (number) and no code to speak of.
    fn fields_module(id: &str) -> Vec<u8> {
        let manifest = format!(
            r#"{{"id":"{id}","name":"Jobs","metadataFields":[{{"key":"job","label":"Job","type":"text"}},{{"key":"ok","label":"OK","type":"bool"}},{{"key":"n","label":"N","type":"number","min":0,"max":1000,"default":0}}]}}"#
        );
        let reply = r#"{"ok":{}}"#;
        let esc = |s: &str| s.replace('"', "\\\"");
        let src = format!(
            r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const {rl}) (i64.const 32)) (i64.const 8192))))"#,
            m = esc(&manifest),
            r = esc(reply),
            ml = manifest.len(),
            rl = reply.len()
        );
        wat::parse_str(&src).unwrap()
    }

    /// G19: a field is set on several photos at once, shows as mixed where they differ, and its
    /// catalog copy is searchable by rules and goes into the photo's sidecar packet.
    #[test]
    fn plugin_fields_edit_the_selection_search_and_reach_xmp() {
        let _turn = turn();
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&fields_module("test.jobs"), None).unwrap();
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.visible_cloned().iter().take(3).map(|p| p.0).collect();
        let r = s.execute("plugin.setField", &json!({"plugin": "test.jobs", "field": "job", "value": "J-77", "ids": [ids[0], ids[1]]})).unwrap();
        assert_eq!(r["count"], 2);
        let field = |s: &mut Session, ids: Value| {
            let f = s.execute("plugin.metadataFields", &json!({"ids": ids})).unwrap();
            f["plugins"].as_array().unwrap().iter().find(|p| p["plugin"] == "test.jobs").cloned().unwrap()["fields"][0].clone()
        };
        let both = field(&mut s, json!([ids[0], ids[1]]));
        assert_eq!((both["value"].clone(), both.get("mixed").cloned()), (json!("J-77"), None), "the same value: shown");
        let mixed = field(&mut s, json!([ids[0], ids[2]]));
        assert_eq!((mixed["value"].clone(), mixed["mixed"].clone()), (Value::Null, json!(true)), "{mixed}");
        // the catalog's copy: rules and Any Searchable Text find it
        let p0 = s.catalog.photo(lcc_catalog::PhotoId(ids[0])).unwrap().clone();
        assert_eq!(p0.plugin_fields.get("test.jobs/job"), Some(&json!("J-77")));
        let rule: lcc_catalog::rules::RuleSet =
            serde_json::from_value(json!({"rules": [{"field": "plugin:test.jobs/job", "op": "contains", "value": "j-77"}]})).unwrap();
        let mut found: Vec<u64> = s.catalog.photos().filter(|p| rule.matches(p, &s.catalog)).map(|p| p.id.0).collect();
        found.sort_unstable();
        let mut want = vec![ids[0], ids[1]];
        want.sort_unstable();
        assert_eq!(found, want);
        // the sidecar packet carries it under the plug-in fields namespace
        let packet = crate::sidecar::sidecar_packet(&p0, &s.catalog);
        assert!(
            packet.contains(&lcc_meta::plugin_fields_ns())
                && packet.contains("<lcpf:plugin>test.jobs</lcpf:plugin>")
                && packet.contains("<lcpf:value>J-77</lcpf:value>"),
            "{packet}"
        );
        // an index in memory only: the catalog's saved form (format 7) doesn't carry it
        assert!(!s.catalog.to_snapshot().contains("J-77"), "not saved with the catalog");
        // a library opened again rebuilds it from the plug-in's data
        let dir = std::env::temp_dir().join(format!("plugin-fields-lib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut lib = Session::new();
        lib.open_library(&dir, true).unwrap();
        let first = lib.visible_cloned()[0].0;
        lib.execute("plugin.setField", &json!({"plugin": "test.jobs", "field": "job", "value": "K-1", "id": first})).unwrap();
        drop(lib);
        let mut again = Session::new();
        again.open_library(&dir, false).unwrap();
        assert_eq!(again.catalog.photo(lcc_catalog::PhotoId(first)).unwrap().plugin_fields.get("test.jobs/job"), Some(&json!("K-1")));
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
        // clearing it clears the copy
        s.execute("plugin.setField", &json!({"plugin": "test.jobs", "field": "job", "value": null, "ids": [ids[0], ids[1]]})).unwrap();
        assert!(s.catalog.photo(lcc_catalog::PhotoId(ids[0])).unwrap().plugin_fields.is_empty());
        // removing the plug-in removes its fields from the catalog
        s.execute("plugin.setField", &json!({"plugin": "test.jobs", "field": "ok", "value": true, "id": ids[2]})).unwrap();
        assert!(!s.catalog.photo(lcc_catalog::PhotoId(ids[2])).unwrap().plugin_fields.is_empty());
        s.execute("plugin.uninstall", &json!({"id": "test.jobs"})).unwrap();
        assert!(s.catalog.photo(lcc_catalog::PhotoId(ids[2])).unwrap().plugin_fields.is_empty());
    }

    /// G19: plug-in fields written to an XMP sidecar come back from it: Read Metadata from File and
    /// an import into another library put them into the plug-in's data again; hostile values and
    /// unknown plug-ins are skipped.
    #[test]
    fn plugin_fields_read_back_from_xmp() {
        let _turn = turn();
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&fields_module("test.back"), None).unwrap();
        let src = crate::tests_xmp::temp_dir("pf-src");
        let lib = crate::tests_xmp::temp_dir("pf-lib");
        let lib2 = crate::tests_xmp::temp_dir("pf-lib2");
        let file = src.join("job.png");
        crate::tests_xmp::write_png(&file, 3);
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        let id = s.catalog.photos().next().unwrap().id;
        s.execute("plugin.setField", &json!({"plugin": "test.back", "field": "job", "value": "J-5", "id": id.0})).unwrap();
        s.execute("plugin.setField", &json!({"plugin": "test.back", "field": "ok", "value": true, "id": id.0})).unwrap();
        s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
        // the plug-in loses its data; reading the sidecar brings it back
        s.execute("plugin.setField", &json!({"plugin": "test.back", "field": "job", "value": null, "id": id.0})).unwrap();
        s.execute("plugin.setField", &json!({"plugin": "test.back", "field": "ok", "value": null, "id": id.0})).unwrap();
        assert!(s.catalog.photo(id).unwrap().plugin_fields.is_empty());
        let r = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
        assert_eq!(r["pluginFields"], 2, "{r}");
        let p = s.catalog.photo(id).unwrap();
        assert_eq!((p.plugin_fields.get("test.back/job"), p.plugin_fields.get("test.back/ok")), (Some(&json!("J-5")), Some(&json!(true))));
        // an import into another library restores them for the new photo record
        let mut t = Session::new().with_fs();
        t.open_library(&lib2, false).unwrap();
        t.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        let id2 = t.catalog.photos().next().unwrap().id;
        assert_eq!(t.catalog.photo(id2).unwrap().plugin_fields.get("test.back/job"), Some(&json!("J-5")));
        // hostile sidecar: a bool that isn't one, an unknown field and plug-in, a huge value: skipped, no panic
        let ns = lcc_meta::plugin_fields_ns();
        let big = "x".repeat(70 * 1024);
        let xmp = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:lcpf="{ns}"><lcpf:fields><rdf:Bag>
<rdf:li rdf:parseType="Resource"><lcpf:plugin>test.back</lcpf:plugin><lcpf:key>ok</lcpf:key><lcpf:value>maybe</lcpf:value></rdf:li>
<rdf:li rdf:parseType="Resource"><lcpf:plugin>test.back</lcpf:plugin><lcpf:key>nope</lcpf:key><lcpf:value>1</lcpf:value></rdf:li>
<rdf:li rdf:parseType="Resource"><lcpf:plugin>no.such</lcpf:plugin><lcpf:key>job</lcpf:key><lcpf:value>1</lcpf:value></rdf:li>
<rdf:li rdf:parseType="Resource"><lcpf:plugin>test.back</lcpf:plugin><lcpf:key>job</lcpf:key><lcpf:value>{big}</lcpf:value></rdf:li>
</rdf:Bag></lcpf:fields></rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        let sc = crate::sidecar::parse_sidecar(&xmp, crate::crs::Target::Rendered).unwrap();
        assert_eq!(sc.plugin_fields.len(), 4);
        assert_eq!(restore_fields(&mut t, &[(id2, sc.plugin_fields)]), 0);
        let p = t.catalog.photo(id2).unwrap();
        assert_eq!((p.plugin_fields.get("test.back/job"), p.plugin_fields.get("test.back/ok")), (Some(&json!("J-5")), Some(&json!(true))));
        drop((s, t));
        for d in [src, lib, lib2] {
            let _ = std::fs::remove_dir_all(d);
        }
        manager().get_or_insert_with(Manager::in_memory).uninstall("test.back").unwrap();
    }

    /// G19: plug-in data is kept per library: photo 1 of one library never shows (or is searched
    /// by) the value set on photo 1 of another, also after reopening; number fields compare as
    /// numbers in rules.
    #[test]
    fn plugin_fields_are_kept_per_library() {
        let _turn = turn();
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&fields_module("test.libs"), None).unwrap();
        let a = crate::tests_xmp::temp_dir("pf-lib-a");
        let b = crate::tests_xmp::temp_dir("pf-lib-b");
        let job = |s: &Session, id: u64| s.catalog.photo(lcc_catalog::PhotoId(id)).and_then(|p| p.plugin_fields.get("test.libs/job").cloned());
        let mut s = Session::new();
        s.open_library(&a, true).unwrap();
        let first = s.visible_cloned()[0].0;
        s.execute("plugin.setField", &json!({"plugin": "test.libs", "field": "job", "value": "A-1", "id": first})).unwrap();
        s.execute("plugin.setField", &json!({"plugin": "test.libs", "field": "n", "value": 10, "id": first})).unwrap();
        let second = s.visible_cloned()[1].0;
        s.execute("plugin.setField", &json!({"plugin": "test.libs", "field": "n", "value": 9, "id": second})).unwrap();
        let rule: lcc_catalog::rules::RuleSet =
            serde_json::from_value(json!({"rules": [{"field": "plugin:test.libs/n", "op": "gt", "value": 9.5}]})).unwrap();
        assert!(rule.check(&s.catalog).is_empty());
        let found: Vec<u64> = s.catalog.photos().filter(|p| rule.matches(p, &s.catalog)).map(|p| p.id.0).collect();
        assert_eq!(found, vec![first], "10 > 9.5 > 9, as numbers");
        drop(s);
        let mut t = Session::new();
        t.open_library(&b, true).unwrap();
        assert!(t.catalog.photo(lcc_catalog::PhotoId(first)).is_some(), "the same photo id in both libraries");
        assert_eq!(job(&t, first), None, "library B doesn't get library A's value");
        let f = t.execute("plugin.metadataFields", &json!({"id": first})).unwrap();
        let mine = f["plugins"].as_array().unwrap().iter().find(|p| p["plugin"] == "test.libs").cloned().unwrap();
        assert!(mine["fields"][0]["value"].is_null(), "{mine}");
        t.execute("plugin.setField", &json!({"plugin": "test.libs", "field": "job", "value": "B-1", "id": first})).unwrap();
        drop(t);
        let mut s = Session::new();
        s.open_library(&a, false).unwrap();
        assert_eq!(job(&s, first), Some(json!("A-1")), "library A keeps its own");
        drop(s);
        let mut t = Session::new();
        t.open_library(&b, false).unwrap();
        assert_eq!(job(&t, first), Some(json!("B-1")));
        drop(t);
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
        manager().get_or_insert_with(Manager::in_memory).uninstall("test.libs").unwrap();
    }

    #[test]
    fn plugin_commands_answer_without_plugins() {
        let mut s = Session::with_demo();
        let l = s.execute("plugin.list", &json!({})).unwrap();
        assert!(l["plugins"].is_array());
        assert!(s.execute("plugin.run", &json!({"plugin": "nope", "command": "x"})).is_err());
        assert!(s.execute("plugin.install", &json!({"path": "/no/such.wasm"})).is_err());
        assert!(s.execute("plugin.grant", &json!({"id": "nope", "grant": {"bogus": 1}})).is_err());
        assert!(s.execute("plugin.postExport", &json!({"files": []})).unwrap().is_array());
    }

    #[test]
    fn host_reads_catalog_and_guards_writes() {
        let mut s = Session::with_demo();
        let mut h = SessionHost(&mut s);
        let stats = h.catalog("catalog.stats", &json!({})).unwrap();
        assert!(stats.is_object());
        assert!(h.catalog("photo.delete", &json!({})).is_err());
        assert!(h.set_metadata(&json!({"title": "x"})).is_err(), "ids required");
        assert!(h.set_metadata(&json!({"ids": [1], "rating": 5})).is_err(), "only standard fields");
    }

    #[test]
    fn host_takes_the_string_ids_plugins_get() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.visible_cloned().iter().take(2).map(|p| p.0).collect();
        // the app's selection is another photo: a string id must not fall back to it
        s.execute("library.select", &json!({"ids": [ids[1]]})).unwrap();
        let mut h = SessionHost(&mut s);
        let v = h.catalog("photo.inspect", &json!({"id": ids[0].to_string()})).unwrap();
        assert_eq!(v["id"], ids[0]);
        h.set_metadata(&json!({"ids": [ids[0].to_string()], "title": "From a plug-in"})).unwrap();
        assert!(h.catalog("photo.inspect", &json!({"id": "not-an-id"})).is_err());
        assert!(h.set_metadata(&json!({"ids": ["x"], "title": "y"})).is_err());
        let a = s.execute("photo.inspect", &json!({"id": ids[0]})).unwrap();
        let b = s.execute("photo.inspect", &json!({"id": ids[1]})).unwrap();
        assert_eq!(a["meta"]["title"], "From a plug-in");
        assert_ne!(b["meta"]["title"], "From a plug-in");
    }
}
