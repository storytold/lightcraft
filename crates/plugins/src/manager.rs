//! The Plugin Manager: installed plug-ins, their grants, enabled state, logs and data.
//!
//! On disk (a folder, usually `<settings>/plugins`): `<id>.wasm` per plug-in, `plugins.json` with
//! each plug-in's `{enabled, grant}`, and `data/<id>.json`, its metadata namespace. A manager
//! without a folder keeps everything in memory (tests, the web build).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::{CallContext, HostApi};
use crate::manifest::dialog_args;
use crate::permissions::Permissions;
use crate::runtime::{Limits, Plugin};
use crate::store::{NamespaceStore, write_atomic};
use crate::{Error, Result};

/// Log lines kept per plug-in.
const MAX_LOG: usize = 500;
/// Most plug-ins loaded from a folder.
const MAX_PLUGINS: usize = 256;
/// Most photos / files one hook request carries.
const MAX_BATCH: usize = 10_000;

/// One installed plug-in.
#[derive(Debug)]
pub struct Installed {
    pub plugin: Arc<Plugin>,
    pub enabled: bool,
    /// What the user granted (the effective set is this ∩ the manifest's request).
    pub grant: Permissions,
    pub log: VecDeque<String>,
    store: Option<NamespaceStore>,
}

impl Installed {
    /// The capabilities calls actually get.
    pub fn effective(&self) -> Permissions {
        self.grant.intersect(&self.plugin.manifest().permissions)
    }

    fn push_log(&mut self, line: String) {
        if self.log.len() >= MAX_LOG {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    /// The plug-in as JSON for lists (`plugin.list`).
    pub fn info(&self) -> Value {
        let m = self.plugin.manifest();
        json!({
            "id": m.id, "name": m.name, "version": m.version, "description": m.description, "author": m.author,
            "enabled": self.enabled, "size": self.plugin.size(),
            "requested": m.permissions, "granted": self.effective(),
            "commands": m.commands, "hooks": m.hooks, "publish": m.publish,
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    plugins: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    enabled: bool,
    grant: Permissions,
}

/// The set of installed plug-ins.
#[derive(Debug, Default)]
pub struct Manager {
    dir: Option<PathBuf>,
    limits: Limits,
    plugins: BTreeMap<String, Installed>,
    /// `(file, error)` for modules in the folder that failed to load.
    pub failed: Vec<(String, String)>,
}

impl Manager {
    /// A manager that keeps nothing on disk.
    pub fn in_memory() -> Manager {
        Manager::default()
    }

    /// Opens the plug-in folder `dir` (created when missing) and loads every installed module;
    /// a module that fails to load is listed in [`Manager::failed`] and skipped.
    pub fn open(dir: &Path) -> Result<Manager> {
        let io = |e: std::io::Error| Error::Io(format!("{}: {e}", dir.display()));
        std::fs::create_dir_all(dir).map_err(io)?;
        let state: State = match std::fs::read(dir.join("plugins.json")) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                log::warn!("plugins.json is unreadable ({e}); plug-ins load disabled");
                State::default()
            }),
            Err(_) => State::default(),
        };
        let mut m = Manager { dir: Some(dir.to_path_buf()), ..Manager::default() };
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(io)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wasm")))
            .collect();
        files.sort();
        files.truncate(MAX_PLUGINS);
        for f in files {
            match read_module(&f, &m.limits).and_then(|b| Plugin::load(&b, m.limits.clone())) {
                Ok(p) => {
                    let id = p.id().to_string();
                    // not in plugins.json (copied in by hand): disabled, nothing granted
                    let (enabled, grant) = state.plugins.get(&id).map_or((false, Permissions::default()), |e| (e.enabled, e.grant.clone()));
                    m.plugins.insert(id, Installed { plugin: Arc::new(p), enabled, grant, log: VecDeque::new(), store: None });
                }
                Err(e) => m.failed.push((f.display().to_string(), e.to_string())),
            }
        }
        Ok(m)
    }

    /// Limits for plug-ins installed from now on.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Installs (or updates) a module. `grant` = what the user approved, one by one, on the
    /// install dialog. `None` grants nothing new: a fresh install gets no permissions, an update
    /// keeps the previous grant (cut back to what the new version still asks for), so an update
    /// can never gain a capability without the user approving it. The plug-in starts enabled.
    pub fn install_bytes(&mut self, bytes: &[u8], grant: Option<Permissions>) -> Result<&Installed> {
        let plugin = Plugin::load(bytes, self.limits.clone())?;
        let id = plugin.id().to_string();
        let previous = self.plugins.get(&id).map(|i| i.grant.clone()).unwrap_or_default();
        let grant = grant.unwrap_or(previous).intersect(&plugin.manifest().permissions);
        if let Some(dir) = &self.dir {
            write_atomic(&dir.join(format!("{id}.wasm")), bytes)?;
        }
        let log = self.plugins.remove(&id).map(|i| i.log).unwrap_or_default();
        self.plugins.insert(id.clone(), Installed { plugin: Arc::new(plugin), enabled: true, grant, log, store: None });
        self.save()?;
        self.plugins
            .get_mut(&id)
            .map(|i| {
                i.push_log(format!("installed version {}", i.plugin.manifest().version));
                &*i
            })
            .ok_or(Error::NotFound(id))
    }

    /// Reads a `.wasm` file and installs it.
    pub fn install_file(&mut self, path: &Path, grant: Option<Permissions>) -> Result<&Installed> {
        let bytes = read_module(path, &self.limits)?;
        self.install_bytes(&bytes, grant)
    }

    /// Removes a plug-in, its module and its data.
    pub fn uninstall(&mut self, id: &str) -> Result<()> {
        self.plugins.remove(id).ok_or_else(|| Error::NotFound(id.into()))?;
        if let Some(dir) = &self.dir {
            for f in [dir.join(format!("{id}.wasm")), dir.join("data").join(format!("{id}.json"))] {
                match std::fs::remove_file(&f) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::Io(format!("{}: {e}", f.display()))),
                }
            }
        }
        self.save()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<()> {
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        p.enabled = enabled;
        p.push_log(if enabled { "enabled".into() } else { "disabled".into() });
        self.save()
    }

    /// Replaces the grant (revoking what it leaves out); it can never exceed the request.
    pub fn set_grant(&mut self, id: &str, grant: Permissions) -> Result<Permissions> {
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        p.grant = grant.intersect(&p.plugin.manifest().permissions);
        let e = p.effective();
        p.push_log(format!("permissions set to {}", serde_json::to_string(&e).unwrap_or_default()));
        self.save()?;
        Ok(e)
    }

    pub fn get(&self, id: &str) -> Option<&Installed> {
        self.plugins.get(id)
    }

    /// Installed plug-ins, by id.
    pub fn list(&self) -> impl Iterator<Item = &Installed> {
        self.plugins.values()
    }

    fn save(&self) -> Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let state =
            State { plugins: self.plugins.iter().map(|(id, p)| (id.clone(), Entry { enabled: p.enabled, grant: p.grant.clone() })).collect() };
        let bytes = serde_json::to_vec_pretty(&state).map_err(|e| Error::Io(e.to_string()))?;
        write_atomic(&dir.join("plugins.json"), &bytes)
    }

    /// Sends `request` to plug-in `id` with its effective capabilities, `lent` files added for this
    /// call. Logs what it printed and how the call ended.
    pub fn invoke(&mut self, id: &str, request: &Value, host: &mut dyn HostApi, lent: &[PathBuf]) -> Result<Value> {
        let dir = self.dir.clone();
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        if !p.enabled {
            return Err(Error::Disabled(id.into()));
        }
        if p.store.is_none() {
            p.store = Some(match &dir {
                Some(d) => NamespaceStore::open(d.join("data").join(format!("{id}.json")))?,
                None => NamespaceStore::in_memory(),
            });
        }
        let perms = p.effective();
        let plugin = p.plugin.clone();
        let mut store = p.store.take().unwrap_or_default();
        let outcome = {
            let mut ctx = CallContext { perms: &perms, lent, host, store: &mut store };
            plugin.invoke(request, &mut ctx)
        };
        let saved = store.save();
        p.store = Some(store);
        let what = request.get("hook").and_then(Value::as_str).unwrap_or("call");
        for l in outcome.logs {
            p.push_log(l);
        }
        if let Err(e) = &outcome.value {
            p.push_log(format!("{what} failed: {e}"));
        }
        saved?;
        outcome.value
    }

    /// Runs a plug-in's menu command: `args` are checked against its dialog, `selection` are the
    /// selected photo ids.
    pub fn run_command(&mut self, id: &str, command: &str, args: &Value, selection: &[String], host: &mut dyn HostApi) -> Result<Value> {
        let p = self.plugins.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let decl = p.plugin.manifest().command(command).ok_or_else(|| Error::Params(format!("plug-in {id} has no command {command:?}")))?;
        let args = dialog_args(&decl.dialog, args)?;
        let req = json!({"hook": "command", "command": command, "args": args, "selection": cap(selection)});
        self.invoke(id, &req, host, &[])
    }

    /// Runs every enabled export hook on the exported files `[{path, photo?}]`. Each hook may read
    /// and rewrite those files (lent for the call). Returns one result per plug-in; a failing
    /// hook doesn't stop the others.
    pub fn post_export(&mut self, files: &[Value], host: &mut dyn HostApi) -> Vec<Value> {
        let lent: Vec<PathBuf> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).map(PathBuf::from).take(MAX_BATCH).collect();
        let ids = self.with_hook(|m| m.hooks.export);
        let req = json!({"hook": "export", "files": files.iter().take(MAX_BATCH).collect::<Vec<_>>()});
        ids.into_iter().map(|id| outcome(&id, self.invoke(&id, &req, host, &lent))).collect()
    }

    /// Asks every enabled metadata provider about `photos` (photo JSON, as `photo.inspect`
    /// returns it). Each result is `{plugin, ok: {suggestions: [{photo, fields?, keywords?}]}}`
    /// or `{plugin, error}`; nothing is written.
    pub fn suggest_metadata(&mut self, photos: &[Value], host: &mut dyn HostApi) -> Vec<Value> {
        let ids = self.with_hook(|m| m.hooks.metadata);
        let req = json!({"hook": "metadata", "photos": photos.iter().take(MAX_BATCH).collect::<Vec<_>>()});
        ids.into_iter().map(|id| outcome(&id, self.invoke(&id, &req, host, &[]))).collect()
    }

    fn with_hook(&self, f: impl Fn(&crate::Manifest) -> bool) -> Vec<String> {
        self.plugins.values().filter(|p| p.enabled && f(p.plugin.manifest())).map(|p| p.plugin.id().to_string()).collect()
    }

    /// Enabled plug-ins that provide a publish service: `(plugin id, service)`.
    pub fn publish_services(&self) -> Vec<(String, crate::PublishDecl)> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .filter_map(|p| p.plugin.manifest().publish.clone().map(|s| (p.plugin.id().to_string(), s)))
            .collect()
    }
}

fn cap(ids: &[String]) -> &[String] {
    ids.get(..MAX_BATCH).unwrap_or(ids)
}

fn outcome(id: &str, r: Result<Value>) -> Value {
    match r {
        Ok(v) => json!({"plugin": id, "ok": v}),
        Err(e) => json!({"plugin": id, "error": e.to_string()}),
    }
}

/// Reads a module file: a regular file within the size limit.
fn read_module(path: &Path, limits: &Limits) -> Result<Vec<u8>> {
    use std::io::Read;
    let io = |e: std::io::Error| Error::Io(format!("{}: {e}", path.display()));
    let meta = std::fs::metadata(path).map_err(io)?;
    if !meta.is_file() {
        return Err(Error::Io(format!("{} is not a file", path.display())));
    }
    if meta.len() > limits.max_module_bytes as u64 {
        return Err(Error::Module(format!("{} is {} bytes (limit {})", path.display(), meta.len(), limits.max_module_bytes)));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(io)?.take(limits.max_module_bytes as u64 + 1).read_to_end(&mut bytes).map_err(io)?;
    Ok(bytes)
}
