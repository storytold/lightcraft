//! Plug-in commands (P4.3): the Plugin Manager ([`dac_plugins::Manager`]) behind engine commands,
//! so the UI, CLI, control channel and MCP drive plug-ins the same way. See docs/plugins.md.
//!
//! The manager is process-wide (plug-ins are installed per user, not per catalog) and lives in
//! `<PREFIX>_PLUGINS` or `<settings>/plugins`; unit tests keep it in memory.
//!
//! A plug-in reaches the catalog only through [`SessionHost`]: read-only queries, and
//! `photo.setMeta` for standard fields, each behind its permission (checked by `dac-plugins`).

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use dac_plugins::{HostApi, Manager, Permissions};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);

/// Where plug-ins are installed: `<PREFIX>_PLUGINS`, else `<settings>/plugins`.
pub fn plugins_dir() -> Option<PathBuf> {
    dac_brand::env_os("PLUGINS").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| crate::config::config_dir().map(|d| d.join("plugins")))
}

/// The process-wide manager, opened on first use. A folder that can't be opened leaves an
/// in-memory manager with the reason in its `failed` list.
pub fn manager() -> MutexGuard<'static, Option<Manager>> {
    let mut g = MANAGER.lock().unwrap_or_else(PoisonError::into_inner);
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

fn with_manager<R>(f: impl FnOnce(&mut Manager) -> Result<R>) -> Result<R> {
    let mut g = manager();
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

impl HostApi for SessionHost<'_> {
    fn catalog(&mut self, method: &str, params: &Value) -> std::result::Result<Value, String> {
        // dac-plugins only forwards the read-only methods; check again: this is the boundary
        if !matches!(method, "catalog.query" | "photo.inspect" | "catalog.stats" | "keyword.list") {
            return Err(format!("{method} is not a catalog read"));
        }
        self.0.execute(method, params).map_err(|e| e.to_string())
    }

    fn set_metadata(&mut self, params: &Value) -> std::result::Result<Value, String> {
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

fn plugin_err(cmd: &str, e: dac_plugins::Error) -> crate::EngineError {
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
        let plugins: Vec<Value> = m.list().map(|p| p.info()).collect();
        let failed: Vec<Value> = m.failed.iter().map(|(f, e)| json!({"file": f, "error": e})).collect();
        Ok(json!({"dir": m.dir(), "plugins": plugins, "failed": failed}))
    })
}

fn install(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.install";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path` (a .wasm file)"))?;
    let grant = grant_param(p, C)?;
    with_manager(|m| m.install_file(std::path::Path::new(path), grant).map(|i| i.info()).map_err(|e| plugin_err(C, e)))
}

fn inspect_file(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.inspectFile";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let bytes = std::fs::read(path).map_err(|e| bad(C, format!("{path}: {e}")))?;
    let plugin = dac_plugins::Plugin::load(&bytes, dac_plugins::Limits::default()).map_err(|e| plugin_err(C, e))?;
    serde_json::to_value(plugin.manifest()).map_err(|e| bad(C, e.to_string()))
}

fn uninstall(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.uninstall";
    let id = id_param(p, C)?;
    with_manager(|m| m.uninstall(id).map(|_| Value::Null).map_err(|e| plugin_err(C, e)))
}

fn enable(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.enable";
    let id = id_param(p, C)?;
    let on = bool_or(p, "enabled", true);
    with_manager(|m| m.set_enabled(id, on).map(|_| json!({"id": id, "enabled": on})).map_err(|e| plugin_err(C, e)))
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
    let selection: Vec<String> = s.targets(p).iter().map(|i| i.0.to_string()).collect();
    let mut host = SessionHost(s);
    with_manager(|m| m.run_command(id, command, &args, &selection, &mut host).map_err(|e| plugin_err(C, e)))
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
    let ids = s.targets(p);
    let mut photos = Vec::new();
    for id in ids.iter().take(1000) {
        let mut v = s.execute("photo.inspect", &json!({"id": id.0})).map_err(|e| bad(C, e.to_string()))?;
        if v.get("id").is_none() {
            v["id"] = json!(id.0.to_string());
        }
        photos.push(v);
    }
    let mut host = SessionHost(s);
    with_manager(|m| Ok(Value::Array(m.suggest_metadata(&photos, &mut host))))
}

fn post_export(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.postExport";
    let files = p.get("files").and_then(Value::as_array).ok_or_else(|| bad(C, "missing `files: [{path, photo?}]`"))?;
    let mut host = SessionHost(s);
    with_manager(|m| Ok(Value::Array(m.post_export(files, &mut host))))
}

fn services(_: &mut Session, _: &Value) -> Result<Value> {
    with_manager(|m| {
        Ok(Value::Array(m.publish_services().into_iter().map(|(plugin, s)| json!({"plugin": plugin, "id": s.id, "name": s.name})).collect()))
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "plugin.list", "Plug-ins", [], None, "{} → {dir, plugins: [{id, name, version, enabled, requested, granted, commands, hooks, publish}], failed}", always, list),
        cmd!(query "plugin.inspectFile", "Inspect Plug-in File", [], None, "{path} — loads a .wasm without installing it → its manifest (the install dialog shows the requested permissions)", always, inspect_file),
        cmd!(query "plugin.install", "Install Plug-in", [], None, "{path: .wasm, grant?: {catalog, metadataWrite, network: [hosts], fs: [{path, write}]}} — installs or updates; grant defaults to everything requested → plugin", always, install),
        cmd!(query "plugin.uninstall", "Remove Plug-in", [], None, "{id} — removes the module and its data", always, uninstall),
        cmd!(query "plugin.enable", "Enable Plug-in", [], None, "{id, enabled?: bool = true}", always, enable),
        cmd!(query "plugin.grant", "Set Plug-in Permissions", [], None, "{id, grant} — replaces the grant (revokes what it leaves out; never more than requested) → granted", always, set_grant),
        cmd!(query "plugin.log", "Plug-in Log", [], None, "{id} → [lines]", always, log),
        cmd!(query "plugin.commands", "Plug-in Commands", [], None, "{} → [{plugin, pluginName, command, label, menu, dialog}] of enabled plug-ins", always, commands),
        cmd!(query "plugin.run", "Run Plug-in Command", [], None, "{plugin, command, args?: dialog values, ids?} — runs a plug-in menu command on ids / the selection → its result", always, run),
        cmd!(query "plugin.suggestMetadata", "Suggest Metadata (Plug-ins)", [], None, "{ids?} — asks metadata providers about the photos → [{plugin, ok: {suggestions}} | {plugin, error}]; writes nothing", always, suggest),
        cmd!(query "plugin.postExport", "Run Export Hooks", [], None, "{files: [{path, photo?}]} — runs plug-in export post-process hooks on exported files → [{plugin, ok|error}]", always, post_export),
        cmd!(query "plugin.publishServices", "Plug-in Publish Services", [], None, "{} → [{plugin, id, name}]", always, services),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_commands_answer_without_plugins() {
        let mut s = Session::with_demo();
        let l = s.execute("plugin.list", &json!({})).unwrap();
        assert!(l["plugins"].is_array());
        assert!(s.execute("plugin.run", &json!({"plugin": "nope", "command": "x"})).is_err());
        assert!(s.execute("plugin.install", &json!({"path": "/no/such.wasm"})).is_err());
        assert!(s.execute("plugin.grant", &json!({"id": "nope", "grant": {"bogus": 1}})).is_err());
        assert!(s.execute("plugin.postExport", &json!({"files": []})).unwrap().as_array().is_some_and(|a| a.is_empty()));
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
}
