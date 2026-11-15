//! Plug-in commands (P4.3): [`lcc_plugins::Manager`] behind engine commands,
//! so the UI, CLI, control channel and MCP drive plug-ins the same way. See docs/plugins.md.
//!
//! The manager is process-wide (plug-ins are installed per user, not per catalog) and lives in
//! `<PREFIX>_PLUGINS` or `<settings>/plugins`; unit tests keep it in memory.
//!
//! A plug-in reaches the catalog only through [`SessionHost`]: read-only queries, and
//! `photo.setMeta` for standard fields, each behind its permission (checked by `lcc-plugins`).

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use lcc_plugins::{Begun, HostApi, Manager, Permissions};
use serde_json::{Value, json};

/// The host of a link in a plug-in's or filter's About (`plugin.list` → `about`), lower-cased, as
/// the Plug-in Manager names the link; `None` for a link the host doesn't show.
pub use lcc_plugins::about::link_host;

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
            Some(d) => match Manager::open(&d) {
                Ok(mut m) => {
                    // no plug-in may be granted a folder holding the settings (its own grant)
                    m.set_protected(crate::config::config_dir().into_iter().collect());
                    crate::filter_plugins::settle_shared_ids(&mut m, crate::filter_plugins::registry());
                    m
                }
                Err(e) => {
                    let mut m = Manager::in_memory();
                    m.failed.push(lcc_plugins::Failed::new(d.display().to_string(), e.to_string()));
                    m
                }
            },
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
pub(super) fn manager_for(library: &str) -> MutexGuard<'static, Option<Manager>> {
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

    fn library_add(&mut self, params: &Value) -> std::result::Result<Value, String> {
        super::plugin_library::add(self.0, params)
    }

    fn photo_pixels(&mut self, photo: &str, long_edge: u32) -> std::result::Result<Value, String> {
        super::plugin_develop::pixels(self.0, photo, long_edge)
    }

    fn set_crop(&mut self, photo: &str, rect: [f64; 4], angle: Option<f64>) -> std::result::Result<Value, String> {
        super::plugin_develop::set_crop(self.0, photo, rect, angle)
    }

    fn virtual_copy(&mut self, photo: &str, name: Option<&str>) -> std::result::Result<Value, String> {
        super::plugin_develop::virtual_copy(self.0, photo, name)
    }

    fn find_program(&mut self, program: &lcc_plugins::ProgramDecl) -> Vec<lcc_plugins::Found> {
        self.0.desktop.find_program(program)
    }

    fn start_program(&mut self, run: lcc_plugins::StartRequest) -> std::result::Result<(), String> {
        super::plugin_programs::start(self.0, run)
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
        let mut plugins: Vec<Value> = m
            .list()
            .map(|p| {
                let mut v = p.info();
                v["previousVersion"] = json!(m.previous_version(&p.plugin.manifest().id));
                v
            })
            .collect();
        // pixel-filter plug-ins (kind "filter"); the manager can't load their modules
        plugins.extend(crate::filter_plugins::registry().list().iter().map(|f| crate::filter_plugins::info(f)));
        let filter_files = crate::filter_plugins::sources();
        let failed: Vec<Value> = m
            .failed
            .iter()
            .filter(|f| !filter_files.contains(&f.file))
            .map(|f| {
                // its kind, so the Plug-in Manager lists it in its section: a module the filter
                // registry recognises is a pixel filter, and the registry's error says why it
                // didn't load (the plug-in manager's would say it isn't a plug-in)
                use crate::filter_plugins::{FilterFile, filter_file_cached};
                let path = f.path.clone().or_else(|| m.dir().map(|d| d.join(&f.file)));
                let what = path.map_or(FilterFile::No, |p| filter_file_cached(&p));
                let error = match &what {
                    FilterFile::Refused(e) => e.as_str(),
                    _ => f.error.as_str(),
                };
                let kind = if what == FilterFile::No { "plugin" } else { "filter" };
                // a filter module another one with its id stands in for: its own id, and why
                let (id, error) = match crate::filter_plugins::duplicate(&f.file) {
                    Some((id, why)) => (id, why),
                    None => (f.id.clone(), error.to_string()),
                };
                let mut v = json!({"file": f.file, "error": error, "kind": kind});
                if !f.missing.is_empty() {
                    v["missing"] = json!(f.missing);
                }
                if !id.is_empty() {
                    v["id"] = json!(id);
                }
                v
            })
            .collect();
        let host = json!({"abi": lcc_plugins::ABI_VERSION, "features": m.features()});
        // past the most the folder loads: neither kind looked at them
        let skipped = m.skipped.iter().filter(|p| !filter_files.contains(&p.display().to_string())).count();
        Ok(json!({"dir": m.dir(), "host": host, "plugins": plugins, "failed": failed, "skipped": skipped}))
    })
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    install_as(s, p, false)
}

/// Installs a plug-in from the Plug-in Manager's install dialog (`{path, allowDowngrade?}`, as
/// `plugin.install`): an update keeps the programs the user allowed even when its code differs,
/// which a command's install drops. The UI's install button only; never registered as a command.
pub fn install_by_user(s: &mut Session, p: &Value) -> Result<Value> {
    install_as(s, p, true)
}

/// A permission a plug-in command the user started needs and the user was never asked about
/// ([`Session::plugin_asks`]): the UI asks, Allow ([`allow_by_user`]) runs `rerun` again, Deny
/// ([`deny_by_user`]) remembers the answer.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginAsk {
    pub plugin: String,
    pub plugin_name: String,
    pub request: lcc_plugins::Request,
    /// The `plugin.run` params to run again once allowed.
    pub rerun: Value,
    /// Asked by the command's prepare step (`plugin.prepare`): once allowed, the command starts
    /// again as from its menu item (its dialog may open) instead of running with `rerun`.
    pub prepare: bool,
}

/// `{id, group, value?}` → the request it names.
fn request_param(p: &Value, cmd: &str) -> Result<lcc_plugins::Request> {
    let group = str_param(p, "group").ok_or_else(|| bad(cmd, "missing `group`"))?;
    let group = lcc_plugins::Group::parse(group).ok_or_else(|| {
        let names: Vec<&str> = lcc_plugins::Group::ALL.iter().map(|g| g.name()).collect();
        bad(cmd, format!("unknown group {group:?} (one of {})", names.join(", ")))
    })?;
    match p.get("value") {
        None | Some(Value::Null) => Ok(lcc_plugins::Request::group(group)),
        Some(Value::String(v)) if group.has_values() => Ok(lcc_plugins::Request::value(group, v.clone())),
        Some(Value::String(_)) => Err(bad(cmd, format!("`{}` takes no value", group.name()))),
        Some(_) => Err(bad(cmd, "`value` must be a string")),
    }
}

/// The user allowed a request of a plug-in (`{id, group, value?}`): when the plug-in asked, or on
/// its Permissions tab. The UI's Allow buttons only (and tests); never registered as a command.
pub fn allow_by_user(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.allow";
    let id = id_param(p, C)?;
    let r = request_param(p, C)?;
    with_manager(|m| m.allow_by_user(id, &r).map(|g| json!({"id": id, "granted": g})).map_err(|e| plugin_err(C, e)))
}

/// The user said no to a request of a plug-in (`{id, group, value?}`) when it asked: remembered
/// with today's date. The UI's Deny button only (and tests); never registered as a command.
pub fn deny_by_user(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.deny";
    let id = id_param(p, C)?;
    let r = request_param(p, C)?;
    let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
    with_manager(|m| m.deny_by_user(id, &r, &today).map(|()| json!({"id": id})).map_err(|e| plugin_err(C, e)))
}

fn revoke(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.revoke";
    let id = id_param(p, C)?;
    let r = request_param(p, C)?;
    with_manager(|m| m.revoke(id, &r).map(|g| json!({"id": id, "granted": g})).map_err(|e| plugin_err(C, e)))
}

/// Sets a plug-in's whole grant at once (`{id, grant}`, as `plugin.grant`, without its check that
/// nothing is added). Tests only: never registered as a command, and the UI allows one request at
/// a time ([`allow_by_user`]).
#[cfg(test)]
pub fn grant_by_user(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.grant";
    let id = id_param(p, C)?;
    let grant = grant_param(p, C)?.ok_or_else(|| bad(C, "missing `grant`"))?;
    with_manager(|m| m.set_grant_by_user(id, grant).map(|g| json!({"id": id, "granted": g})).map_err(|e| plugin_err(C, e)))
}

/// Where a plug-in's declared program (a `programs` entry, as `plugin.list` gives it) is
/// installed, as the session's desktop finds it: `[{bundleId?, path, version?}]` (the Plugin
/// Manager shows it next to the permission). An entry that isn't valid finds nothing.
///
/// It doesn't wait (the UI thread asks every frame): `None` while the first lookup runs in the
/// background ([`crate::desktop::Desktop::find_program_soon`]).
pub fn find_program(s: &Session, program: &Value) -> Option<Vec<Value>> {
    let Ok(decl) = serde_json::from_value::<lcc_plugins::ProgramDecl>(program.clone()) else { return Some(Vec::new()) };
    if decl.validate().is_err() {
        return Some(Vec::new());
    }
    Some(s.desktop.find_program_soon(&decl)?.iter().filter_map(|f| serde_json::to_value(f).ok()).collect())
}

fn install_as(s: &mut Session, p: &Value, by_user: bool) -> Result<Value> {
    const C: &str = "plugin.install";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path` (a .wasm file)"))?;
    // the install dialog grants nothing: the user allows each permission when it's asked for
    let grant = if by_user { None } else { grant_param(p, C)? };
    let allow_downgrade = bool_or(p, "allowDowngrade", false);
    let bytes = lcc_plugins::read_module(std::path::Path::new(path), &lcc_plugins::Limits::default()).map_err(|e| plugin_err(C, e))?;
    if lcc_filters::is_filter_module(&bytes) {
        // a pixel filter: no capabilities to grant, nothing for the manager
        let f = lcc_filters::Filter::load(&bytes, lcc_filters::Limits::default()).map_err(|e| bad(C, e.to_string()))?;
        // checked and installed under the manager's lock, so a plug-in install can't take the id meanwhile
        let (f, unknown) = with_manager(|m| {
            if let Some(e) = plugin_has_id(m, f.id()) {
                return Err(bad(C, e));
            }
            if let Some(e) = filter_downgrade(&f)
                && !allow_downgrade
            {
                return Err(bad(C, e.to_string()));
            }
            let unknown = crate::filter_plugins::unknown_fields(&f);
            if !unknown.is_empty() {
                // a filter has no log of its own
                log::warn!("pixel filter {}: unknown manifest fields: {}", f.id(), unknown.join(", "));
            }
            for d in &f.manifest().dropped {
                log::warn!("pixel filter {}: manifest value dropped: {d}", f.id());
            }
            Ok((crate::filter_plugins::install_loaded(f, &bytes).map_err(|e| bad(C, e))?, unknown))
        })?;
        let mut info = crate::filter_plugins::info(&f);
        info["unknownFields"] = json!(unknown);
        return Ok(info);
    }
    let lib = s.plugin_library.clone();
    let r = with_library(&lib, |m| {
        let plugin = m.load(&bytes).map_err(|e| plugin_err(C, e))?;
        if let Some(e) = filter_has_id(plugin.id()) {
            return Err(bad(C, e));
        }
        let installed = if by_user {
            m.install_loaded_by_user(plugin, &bytes, grant, allow_downgrade)
        } else {
            m.install_loaded(plugin, &bytes, grant, allow_downgrade)
        };
        installed.map(|i| i.info()).map_err(|e| plugin_err(C, e))
    })?;
    sync_fields(s);
    Ok(r)
}

/// Plug-ins and pixel filters share the folder's `<id>.wasm` names: why a filter with `id` can't be
/// installed while plug-in `id` is (loaded, or a plug-in module in the folder that didn't load:
/// it needs features this version lacks, can't be read, or a filter with its id came first).
fn plugin_has_id(m: &Manager, id: &str) -> Option<String> {
    use crate::filter_plugins::{FilterFile, filter_file_cached};
    let failed = || m.failed.iter().any(|f| f.id == id && f.path.as_deref().is_some_and(|p| filter_file_cached(p) == FilterFile::No));
    let name = match (m.get(id), m.unloaded(id).is_some() || failed()) {
        (Some(p), _) => p.plugin.manifest().name.clone(),
        (None, true) => id.to_string(),
        (None, false) => return None,
    };
    Some(format!("{id:?} is the id of the installed plug-in \"{name}\"; a pixel filter can't share it (remove the plug-in first)"))
}

/// [`plugin_has_id`] the other way round: why a plug-in with `id` can't be installed.
fn filter_has_id(id: &str) -> Option<String> {
    let f = crate::filter_plugins::registry().get(id)?;
    Some(format!("{id:?} is the id of the installed pixel filter \"{}\"; a plug-in can't share it (remove the filter first)", f.manifest().name))
}

/// Filter `f` is older than the installed filter with its id: the error installing it without
/// `allowDowngrade` gives (the same as a plug-in's).
fn filter_downgrade(f: &lcc_filters::Filter) -> Option<lcc_plugins::Error> {
    let installed = crate::filter_plugins::registry().get(f.id())?;
    let (new, old) = (&f.manifest().version, &installed.manifest().version);
    crate::filter_plugins::is_older(new, old).then(|| lcc_plugins::Error::Downgrade { installed: old.clone(), new: new.clone() })
}

fn inspect_file(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.inspectFile";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    // as install reads it: a file over the size limit is refused before it is read
    let bytes = lcc_plugins::read_module(std::path::Path::new(path), &lcc_plugins::Limits::default()).map_err(|e| plugin_err(C, e))?;
    if lcc_filters::is_filter_module(&bytes) {
        let f = lcc_filters::Filter::load(&bytes, lcc_filters::Limits::default()).map_err(|e| bad(C, e.to_string()))?;
        let installed = crate::filter_plugins::registry().get(f.id()).map(|i| {
            let same = i.manifest().version == f.manifest().version && i.module_hash() == f.module_hash();
            json!({"version": i.manifest().version, "moduleHash": i.module_hash(), "same": same})
        });
        let conflict = with_manager(|m| Ok(plugin_has_id(m, f.id())))?;
        let downgrade = filter_downgrade(&f).is_some();
        let unknown = crate::filter_plugins::unknown_fields(&f);
        return Ok(json!({
            "manifest": crate::filter_plugins::info(&f), "installed": installed, "downgrade": downgrade, "conflict": conflict,
            "unknownFields": unknown, "dropped": f.manifest().dropped,
        }));
    }
    let plugin = lcc_plugins::Plugin::load(&bytes, lcc_plugins::Limits::default()).map_err(|e| plugin_err(C, e))?;
    let mut manifest = serde_json::to_value(plugin.manifest()).map_err(|e| bad(C, e.to_string()))?;
    if let Some(o) = manifest.as_object_mut() {
        o.insert("about".into(), plugin.manifest().about.info());
    }
    // an update: what is granted now, so the dialog can show what the new version adds; and what
    // would stop the install, so the dialog can say so before the user approves anything
    with_manager(|m| {
        // one that stopped loading counts too: installing over it replaces it
        let installed = m
            .get(plugin.id())
            .map(|i| {
                let same = i.plugin.manifest().version == plugin.manifest().version && i.plugin.digest() == plugin.digest();
                json!({"version": i.plugin.manifest().version, "granted": i.effective(), "same": same})
            })
            .or_else(|| m.unloaded(plugin.id()).map(|(version, grant)| json!({"version": version, "granted": grant, "loaded": false})));
        let (missing, version_error, downgrade) = m.install_check(plugin.manifest());
        let conflict = filter_has_id(plugin.id());
        let (unknown, dropped) = (&plugin.manifest().unknown_fields, &plugin.manifest().dropped);
        Ok(json!({
            "manifest": manifest, "installed": installed, "missing": missing, "versionError": version_error,
            "downgrade": downgrade.is_some(), "conflict": conflict, "unknownFields": unknown, "dropped": dropped,
        }))
    })
}

fn uninstall(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.uninstall";
    let keep_data = bool_or(p, "keepData", false);
    if let Some(file) = str_param(p, "file") {
        return uninstall_file(s, file, keep_data);
    }
    let id = id_param(p, C)?;
    if crate::filter_plugins::uninstall(id).map_err(|e| bad(C, e))? {
        return Ok(Value::Null);
    }
    let lib = s.plugin_library.clone();
    let filter_files = crate::filter_plugins::sources();
    with_library(&lib, |m| {
        // a pixel filter's module fails to load here too: it's only removed through the filter registry
        if m.failed.iter().any(|f| f.id == id && filter_files.contains(&f.file)) {
            return Err(bad(C, format!("{id:?} is a pixel filter module, not a plug-in; remove it by its filter id")));
        }
        // nor a filter module that didn't load (a duplicate): removing by id would delete it too
        let filter_row = m.failed.iter().find(|f| {
            use crate::filter_plugins::{FilterFile, filter_file_cached};
            f.id == id && f.path.as_deref().is_some_and(|p| filter_file_cached(p) != FilterFile::No)
        });
        if let Some(f) = filter_row {
            return Err(bad(C, format!("{} is a pixel filter module with the id {id:?}; remove that file first (by `file`)", f.file)));
        }
        m.uninstall_with(id, keep_data).map(|_| Value::Null).map_err(|e| plugin_err(C, e))
    })?;
    sync_fields(s);
    Ok(Value::Null)
}

/// `plugin.uninstall {file}`: a module that didn't load. Another module with its id loaded (or
/// stands in for it): only this file goes. Otherwise it is the plug-in's only module, and the
/// plug-in is removed as by its id, data and all.
fn uninstall_file(s: &mut Session, file: &str, keep_data: bool) -> Result<Value> {
    const C: &str = "plugin.uninstall";
    let lib = s.plugin_library.clone();
    with_library(&lib, |m| {
        let Some(row) = m.failed.iter().find(|f| f.file == file).cloned() else {
            return Err(bad(C, format!("{file}: no module that didn't load is listed with this file")));
        };
        let id = crate::filter_plugins::duplicate(file).map_or(row.id.clone(), |(id, _)| id);
        let others =
            m.get(&id).is_some() || crate::filter_plugins::registry().get(&id).is_some() || m.failed.iter().any(|f| f.file != file && f.id == id);
        // a pixel filter module that didn't load has no plug-in data: only its file goes
        let filter = row.path.as_deref().is_some_and(|p| crate::filter_plugins::filter_file_cached(p) != crate::filter_plugins::FilterFile::No);
        if others || filter || crate::filter_plugins::duplicate(file).is_some() {
            m.remove_failed_module(file).map_err(|e| plugin_err(C, e))?;
            crate::filter_plugins::forget_duplicate(file);
            return Ok(());
        }
        m.uninstall_with(&id, keep_data).map_err(|e| plugin_err(C, e))
    })?;
    sync_fields(s);
    Ok(Value::Null)
}

fn rollback(s: &mut Session, p: &Value) -> Result<Value> {
    rollback_as(s, p, false)
}

/// Goes back to a plug-in's previous version as the user asked on its page (`{id}`, as
/// `plugin.rollback`): it keeps the programs granted, which a command's rollback drops. The
/// plug-in page's button only; never registered as a command.
pub fn rollback_by_user(s: &mut Session, p: &Value) -> Result<Value> {
    rollback_as(s, p, true)
}

fn rollback_as(s: &mut Session, p: &Value, by_user: bool) -> Result<Value> {
    const C: &str = "plugin.rollback";
    let id = id_param(p, C)?;
    if crate::filter_plugins::registry().get(id).is_some() {
        return Err(bad(C, format!("{id:?} is a pixel filter: pixel filters keep no previous version")));
    }
    let lib = s.plugin_library.clone();
    let r = with_library(&lib, |m| {
        let back = if by_user { m.rollback_by_user(id) } else { m.rollback(id) };
        back.map(|i| i.info()).map_err(|e| plugin_err(C, e))
    })?;
    sync_fields(s);
    Ok(r)
}

fn enable(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.enable";
    let id = id_param(p, C)?;
    let on = bool_or(p, "enabled", true);
    if crate::filter_plugins::registry().get(id).is_some() {
        return Err(bad(C, format!("{id:?} is a pixel filter: pixel filters can't be turned off, only removed")));
    }
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

fn picture(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.picture";
    let id = id_param(p, C)?;
    let dark = match str_param(p, "theme") {
        None | Some("dark") => true,
        Some("light") => false,
        Some(t) => return Err(bad(C, format!("`theme` must be dark or light, not {t:?}"))),
    };
    let header = match str_param(p, "picture") {
        None | Some("header") => true,
        Some("icon") => false,
        Some(k) => return Err(bad(C, format!("`picture` must be header or icon, not {k:?}"))),
    };
    // a pixel filter's, else a plug-in's: both manifests carry the same two pictures
    let svg = if let Some(f) = crate::filter_plugins::registry().get(id) {
        let m = f.manifest();
        if header { m.header.as_ref().map(|h| h.for_theme(dark).to_string()) } else { m.icon.clone() }
    } else {
        with_manager(|m| {
            let i = m.get(id).ok_or_else(|| bad(C, format!("no plug-in {id:?} is installed")))?;
            let m = i.plugin.manifest();
            Ok(if header { m.header.as_ref().map(|h| h.for_theme(dark).to_string()) } else { m.icon.clone() })
        })?
    };
    Ok(json!({"svg": svg}))
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.run";
    let id = id_param(p, C)?;
    let command = str_param(p, "command").ok_or_else(|| bad(C, "missing `command`"))?;
    let args = p.get("args").cloned().unwrap_or(Value::Null);
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let ids: Vec<u64> = s.targets(p)?.iter().map(|i| i.0).collect();
    let selection: Vec<String> = ids.iter().map(u64::to_string).collect();
    let source = source_param(s, p, C)?;
    let lib = s.plugin_library.clone();
    let begun = {
        let mut host = SessionHost(s);
        with_library(&lib, |m| m.begin_command(id, command, &args, &selection, &source, &mut host).map_err(|e| plugin_err(C, e)))?
    };
    let r = match begun {
        Begun::Message(m) => return Ok(json!({"message": m, "ran": false})),
        Begun::Call(call) if call.is_job() => return super::plugin_jobs::run(s, *call, wait).map_err(|e| plugin_err(C, e)),
        Begun::Call(mut call) => {
            if s.plugin_ask_user {
                call.ask_user();
            }
            let outcome = call.run(&mut SessionHost(s));
            let needs = call.take_needs();
            let name = call.plugin_name().to_string();
            let r = with_library(&lib, |m| m.end(*call, Some(outcome)).map_err(|e| plugin_err(C, e)));
            // the user is asked about each, once the command has ended (Allow runs it again)
            for request in needs {
                let mut rerun = json!({"plugin": id, "command": command, "args": args, "source": source});
                // the same photos (the selection may have changed by then)
                if !ids.is_empty() {
                    rerun["ids"] = json!(ids);
                }
                let ask = PluginAsk { plugin: id.to_string(), plugin_name: name.clone(), request, rerun, prepare: false };
                if !s.plugin_asks.contains(&ask) {
                    s.plugin_asks.push(ask);
                }
            }
            r
        }
    };
    // the command may have written the plug-in's own fields
    sync_fields(s);
    r
}

/// The `source` a command gets: the one in `p`, checked, else the Library source `s` shows.
fn source_param(s: &Session, p: &Value, cmd: &str) -> Result<Value> {
    match p.get("source") {
        None | Some(Value::Null) => Ok(super::plugin_library::source(s)),
        Some(v) => super::plugin_library::checked_source(v).map_err(|e| bad(cmd, e)),
    }
}

fn commands(_: &mut Session, _: &Value) -> Result<Value> {
    with_manager(|m| {
        let m: &Manager = m;
        let v: Vec<Value> = m
            .list()
            .filter(|p| p.enabled)
            .flat_map(|p| {
                let pm = p.plugin.manifest();
                pm.commands.iter().map(move |c| {
                    let values = m.command_values(&pm.id, &c.id, &c.dialog);
                    json!({"plugin": pm.id, "pluginName": pm.name, "command": c.id, "label": c.label, "menu": c.menu, "dialog": c.dialog,
                        "prepare": c.prepare, "quickAction": c.quick_action, "values": values})
                })
            })
            .collect();
        Ok(Value::Array(v))
    })
}

/// Fields as JSON, each with its `value` from `values`.
fn fields_with_values(fields: &[lcc_plugins::Field], values: &serde_json::Map<String, Value>) -> Vec<Value> {
    fields
        .iter()
        .map(|f| {
            let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
            v["value"] = values.get(&f.key).cloned().unwrap_or(Value::Null);
            v
        })
        .collect()
}

fn prepare(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.prepare";
    let id = id_param(p, C)?;
    let command = str_param(p, "command").ok_or_else(|| bad(C, "missing `command`"))?;
    let selection: Vec<String> = s.targets(p)?.iter().map(|i| i.0.to_string()).collect();
    let source = source_param(s, p, C)?;
    let lib = s.plugin_library.clone();
    let asking = s.plugin_ask_user;
    let mut needs = Vec::new();
    let mut name = String::new();
    let r = {
        let mut host = SessionHost(s);
        with_library(&lib, |m| {
            let prepared = if asking {
                m.prepare_command_asking(id, command, &selection, &source, &mut host, &mut needs)
            } else {
                m.prepare_command_from(id, command, &selection, &source, &mut host)
            };
            if let Some(p) = m.get(id) {
                name.clone_from(&p.plugin.manifest().name);
            }
            Ok(match prepared.map_err(|e| plugin_err(C, e))? {
                lcc_plugins::Prepared::Dialog { fields, error } => {
                    let values = m.command_values(id, command, &fields);
                    json!({"dialog": fields_with_values(&fields, &values), "error": error})
                }
                lcc_plugins::Prepared::Run(args) => json!({"run": args}),
                lcc_plugins::Prepared::Message(message) => json!({"message": message}),
            })
        })
    };
    // the user is asked about each, whether the prepare step failed or not (Allow starts the
    // command again)
    for request in needs {
        let ask =
            PluginAsk { plugin: id.to_string(), plugin_name: name.clone(), request, rerun: json!({"plugin": id, "command": command}), prepare: true };
        if !s.plugin_asks.contains(&ask) {
            s.plugin_asks.push(ask);
        }
    }
    r
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.settings";
    let id = id_param(p, C)?;
    let lib = s.plugin_library.clone();
    let mut host = SessionHost(s);
    with_library(&lib, |m| {
        let lcc_plugins::SettingsView { fields, values, error } = m.settings(id, &mut host).map_err(|e| plugin_err(C, e))?;
        Ok(json!({"id": id, "fields": fields_with_values(&fields, &values), "values": values, "error": error}))
    })
}

fn set_settings(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.setSettings";
    let id = id_param(p, C)?;
    let values = p.get("values").ok_or_else(|| bad(C, "missing `values: {key: value}`"))?;
    let lib = s.plugin_library.clone();
    let mut host = SessionHost(s);
    with_library(&lib, |m| m.set_settings(id, values, &mut host).map(|v| json!({"id": id, "values": v})).map_err(|e| plugin_err(C, e)))
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
    let mut host = HookSession(SessionHost(s));
    with_library(&lib, |m| Ok(Value::Array(m.suggest_metadata(&photos, &mut host))))
}

fn post_export(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "plugin.postExport";
    let files = p.get("files").and_then(Value::as_array).ok_or_else(|| bad(C, "missing `files: [{path, photo?}]`"))?;
    let lib = s.plugin_library.clone();
    let mut host = HookSession(SessionHost(s));
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
    let r = hooks.run(&mut HookSession(SessionHost(s)), &|| false, &mut |_, _| {});
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
    // the destination's arguments are remembered for its dialog
    let calls = manager_for(library).as_mut().map(|m| m.export_calls(&files, dest)).unwrap_or_default();
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
    /// The host call: a catalog read, `metadata.setStandard`, `library.add`, `photo.pixels`,
    /// `develop.setCrop` or `photo.virtualCopy`.
    method: Relayed,
    params: Value,
    reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
    /// Who got to the request first: the session's thread taking it (`TAKEN`) or a caller
    /// that gave up waiting (`ABANDONED`), so a request is never run after its caller was
    /// told it failed.
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

/// A host call [`RelayHost`] hands to the session's thread.
#[derive(Clone, Debug)]
pub(super) enum Relayed {
    /// A catalog read (`catalog.query`, …).
    Catalog(String),
    /// `metadata.setStandard`.
    SetMetadata,
    /// `library.add`.
    LibraryAdd,
    /// `photo.pixels`: `{photo, longEdge}`, already checked. The session's thread only finds the
    /// source and leaves it here; the caller decodes it on its own thread.
    Pixels(std::sync::Arc<std::sync::Mutex<Option<super::plugin_develop::PixelsSource>>>),
    /// `develop.setCrop`: `{photo, rect, angle}`, already checked.
    SetCrop,
    /// `photo.virtualCopy`: `{photo, name}`, already checked.
    VirtualCopy,
}

/// [`Relayed::Pixels`]'s parameters, and back.
pub(super) fn pixels_params(photo: &str, long_edge: u32) -> Value {
    json!({"photo": photo, "longEdge": long_edge})
}

/// [`Relayed::SetCrop`]'s parameters, and back.
pub(super) fn crop_params(photo: &str, rect: [f64; 4], angle: Option<f64>) -> Value {
    json!({"photo": photo, "rect": rect, "angle": angle})
}

/// [`Relayed::VirtualCopy`]'s parameters, and back.
pub(super) fn copy_params(photo: &str, name: Option<&str>) -> Value {
    json!({"photo": photo, "name": name})
}

fn relayed_copy(host: &mut SessionHost<'_>, p: &Value) -> std::result::Result<Value, String> {
    let photo = p["photo"].as_str().ok_or("photo.virtualCopy: missing photo")?;
    host.virtual_copy(photo, p["name"].as_str())
}

fn relayed_pixels(
    s: &mut Session,
    p: &Value,
    slot: &std::sync::Mutex<Option<super::plugin_develop::PixelsSource>>,
) -> std::result::Result<Value, String> {
    let photo = p["photo"].as_str().ok_or("photo.pixels: missing photo")?;
    let edge = p["longEdge"].as_u64().and_then(|e| u32::try_from(e).ok()).ok_or("photo.pixels: missing longEdge")?;
    let source = super::plugin_develop::pixels_source(s, photo, edge)?;
    *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(source);
    Ok(Value::Null)
}

fn relayed_crop(host: &mut SessionHost<'_>, p: &Value) -> std::result::Result<Value, String> {
    let photo = p["photo"].as_str().ok_or("develop.setCrop: missing photo")?;
    let v: Vec<f64> = p["rect"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
    let rect: [f64; 4] = v.try_into().map_err(|_| "develop.setCrop: missing rect")?;
    host.set_crop(photo, rect, p["angle"].as_f64())
}

const WAITING: u8 = 0;
const TAKEN: u8 = 1;
const ABANDONED: u8 = 2;

impl HostRequest {
    /// Answers the request from `s`, with the same checks as a call on the session's thread.
    /// A request its caller gave up on is skipped.
    pub fn answer(self, s: &mut Session) {
        use std::sync::atomic::Ordering;
        if self.state.compare_exchange(WAITING, TAKEN, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        let mut host = SessionHost(s);
        let r = match &self.method {
            Relayed::Catalog(m) => host.catalog(m, &self.params),
            Relayed::SetMetadata => host.set_metadata(&self.params),
            Relayed::LibraryAdd => host.library_add(&self.params),
            Relayed::Pixels(slot) => relayed_pixels(host.0, &self.params, slot),
            Relayed::SetCrop => relayed_crop(&mut host, &self.params),
            Relayed::VirtualCopy => relayed_copy(&mut host, &self.params),
        };
        let _ = self.reply.send(r);
    }
}

/// Why a hook can't start a program: a run starts from a plug-in command, which the user chose.
pub(super) const HOOK_NO_START: &str = "program.start is not available in hooks or export destinations; start the program from a command";

/// What plug-ins running on a worker see of the catalog: each request goes to the session's thread
/// (see [`relay_host`]) and waits for its answer. Programs are looked up through the session's
/// desktop; none is started ([`HOOK_NO_START`]).
pub struct RelayHost(std::sync::mpsc::Sender<HostRequest>, std::sync::Arc<dyn crate::desktop::Desktop>);

impl RelayHost {
    pub(super) fn new(tx: std::sync::mpsc::Sender<HostRequest>, desktop: std::sync::Arc<dyn crate::desktop::Desktop>) -> RelayHost {
        RelayHost(tx, desktop)
    }

    fn ask(&self, method: Relayed, params: &Value) -> std::result::Result<Value, String> {
        let (reply, rx) = std::sync::mpsc::channel();
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(WAITING));
        self.0.send(HostRequest { method, params: params.clone(), reply, state }).map_err(|_| "the catalog is closed".to_string())?;
        rx.recv().map_err(|_| "the catalog is closed".to_string())?
    }

    /// `photo.pixels`: the session's thread finds the source ([`Relayed::Pixels`]), and it is
    /// decoded here, on the caller's thread. With a `limit`, as [`RelayHost::ask_within`].
    pub(super) fn pixels_within(&self, photo: &str, long_edge: u32, limit: Option<std::time::Duration>) -> std::result::Result<Value, String> {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let method = Relayed::Pixels(slot.clone());
        let params = pixels_params(photo, long_edge);
        match limit {
            Some(limit) => self.ask_within(method, &params, limit)?,
            None => self.ask(method, &params)?,
        };
        let source = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        source.ok_or_else(|| "photo.pixels: the catalog gave no source".to_string())?.decode()
    }

    /// [`RelayHost::ask`], giving up after `limit` unless the session's thread has already taken
    /// the request (then its answer is awaited: it may have written already).
    pub(super) fn ask_within(&self, method: Relayed, params: &Value, limit: std::time::Duration) -> std::result::Result<Value, String> {
        use std::sync::atomic::Ordering;
        let (reply, rx) = std::sync::mpsc::channel();
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(WAITING));
        let request = HostRequest { method, params: params.clone(), reply, state: state.clone() };
        self.0.send(request).map_err(|_| "the catalog is closed".to_string())?;
        match rx.recv_timeout(limit) {
            Ok(r) => r,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the catalog is closed".into()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if state.compare_exchange(WAITING, ABANDONED, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    return Err("the catalog didn't answer".into());
                }
                rx.recv().map_err(|_| "the catalog is closed".to_string())?
            }
        }
    }
}

impl HostApi for RelayHost {
    fn catalog(&mut self, method: &str, params: &Value) -> std::result::Result<Value, String> {
        self.ask(Relayed::Catalog(method.to_string()), params)
    }

    fn set_metadata(&mut self, params: &Value) -> std::result::Result<Value, String> {
        self.ask(Relayed::SetMetadata, params)
    }

    fn library_add(&mut self, params: &Value) -> std::result::Result<Value, String> {
        self.ask(Relayed::LibraryAdd, params)
    }

    fn photo_pixels(&mut self, photo: &str, long_edge: u32) -> std::result::Result<Value, String> {
        self.pixels_within(photo, long_edge, None)
    }

    fn set_crop(&mut self, photo: &str, rect: [f64; 4], angle: Option<f64>) -> std::result::Result<Value, String> {
        self.ask(Relayed::SetCrop, &crop_params(photo, rect, angle))
    }

    fn virtual_copy(&mut self, photo: &str, name: Option<&str>) -> std::result::Result<Value, String> {
        self.ask(Relayed::VirtualCopy, &copy_params(photo, name))
    }

    fn find_program(&mut self, program: &lcc_plugins::ProgramDecl) -> Vec<lcc_plugins::Found> {
        self.1.find_program(program)
    }

    fn start_program(&mut self, _: lcc_plugins::StartRequest) -> std::result::Result<(), String> {
        Err(HOOK_NO_START.into())
    }
}

/// A host for plug-in calls of session `s` on a worker, and where its requests arrive for the
/// session's thread to answer.
pub fn relay_host(s: &Session) -> (RelayHost, std::sync::mpsc::Receiver<HostRequest>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (RelayHost(tx, s.desktop.clone()), rx)
}

/// [`SessionHost`] for hooks and export destinations: the same catalog and lookups, no starts
/// ([`HOOK_NO_START`]), wherever the hook runs.
struct HookSession<'a>(SessionHost<'a>);

impl HostApi for HookSession<'_> {
    fn catalog(&mut self, method: &str, params: &Value) -> std::result::Result<Value, String> {
        self.0.catalog(method, params)
    }

    fn set_metadata(&mut self, params: &Value) -> std::result::Result<Value, String> {
        self.0.set_metadata(params)
    }

    fn library_add(&mut self, params: &Value) -> std::result::Result<Value, String> {
        self.0.library_add(params)
    }

    fn photo_pixels(&mut self, photo: &str, long_edge: u32) -> std::result::Result<Value, String> {
        self.0.photo_pixels(photo, long_edge)
    }

    fn set_crop(&mut self, photo: &str, rect: [f64; 4], angle: Option<f64>) -> std::result::Result<Value, String> {
        self.0.set_crop(photo, rect, angle)
    }

    fn virtual_copy(&mut self, photo: &str, name: Option<&str>) -> std::result::Result<Value, String> {
        self.0.virtual_copy(photo, name)
    }

    fn find_program(&mut self, program: &lcc_plugins::ProgramDecl) -> Vec<lcc_plugins::Found> {
        self.0.find_program(program)
    }

    fn start_program(&mut self, _: lcc_plugins::StartRequest) -> std::result::Result<(), String> {
        Err(HOOK_NO_START.into())
    }
}

fn destinations(_: &mut Session, _: &Value) -> Result<Value> {
    with_manager(|m| {
        let names: std::collections::BTreeMap<String, String> =
            m.list().map(|p| (p.plugin.id().to_string(), p.plugin.manifest().name.clone())).collect();
        Ok(Value::Array(
            m.export_destinations()
                .into_iter()
                .map(|(plugin, d)| {
                    let values = m.destination_values(&plugin, &d.id);
                    json!({"plugin": plugin, "pluginName": names.get(&plugin), "destination": d.id, "name": d.name, "dialog": d.dialog, "values": values})
                })
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
    let mut host = HookSession(SessionHost(s));
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
        cmd!(query "plugin.list", "Plug-ins", [], None, "{} → {dir, host: {abi, features}, plugins: [{id, name, version, requires, enabled, requested, granted, requests: [{group, value?: host, folder or program id, write?: (fs), state: allowed|denied|never|refused, deniedOn?: YYYY-MM-DD, reason?: why a folder can't be granted}], commands, hooks, publish, about: {homepage?, repository?, changelog?, license?, issues?: the GitHub repository's issues page}, moduleHash: sha256 of the module, settingsRevision: changes when a setting does, previousVersion: the version plugin.rollback goes back to | null}], failed: [{file, error, id?: to remove it with plugin.uninstall, missing?: features this version lacks}], skipped: modules in the folder past the most it loads (256 plug-ins counting every module, 1,000 pixel filters), not looked at}", always, list),
        cmd!(query "plugin.inspectFile", "Inspect Plug-in File", [], None, "{path}: loads a .wasm without installing it → {manifest, installed: {version, granted, same: the installed version and code are this file's (installing it changes nothing), loaded?: false (it needs features this version lacks)} | null, missing: [features this version lacks], versionError: why `version` isn't semver | null, downgrade: older than the installed version (pixel filters too), conflict: why a module of the other kind (plug-in or pixel filter) with this id stops it | null, unknownFields: manifest fields this version doesn't know, by path (they're ignored; installing still goes ahead), dropped: manifest values this version can't use, each with why (`homepage: not an https:// link`; left out, installing still goes ahead)}; manifest.about: {homepage?, repository?, changelog?, license?, issues?}; plugin.install refuses each of these (a downgrade only without allowDowngrade). A file over the install size limit is refused as install refuses it. The install dialog lists the permissions it may ask for", always, inspect_file),
        cmd!(query "plugin.install", "Install Plug-in", [], None, "{path: .wasm, grant?: {catalog, metadataWrite, network: [hosts], fs: [{path, write}], programs: [entries as plugin.list gives them]}, allowDowngrade?: bool = false}: installs or updates. Nothing is granted: a fresh install has no permissions, and the plug-in asks the user for each as it needs it; an update keeps what the user allowed (cut back to what the new version asks for) and what they denied. `grant` may repeat or leave out what the installed version has, never add to it (an error, nothing installed): only the user allows. A module whose code differs from the installed one keeps none of its programs (its log says so). Refused: features in `requires` this version lacks, a `version` that isn't semver, an older version than the installed one unless allowDowngrade (pixel filters too), an id an installed module of the other kind (plug-in or pixel filter) has → plugin", always, install),
        cmd!(query "plugin.uninstall", "Remove Plug-in", [], None, "{id} | {file}, keepData?: bool = false: removes the module and its data (with keepData its data and settings stay, for a later install of the same id; a pixel filter keeps none); also a plug-in listed under plugin.list → failed (by its id there). With `file` (a plugin.list → failed row's): only that module file when another module has its id (the first by file name loads), else the plug-in as by its id", always, uninstall),
        cmd!(query "plugin.rollback", "Go Back to Previous Plug-in Version", [], None, "{id}: goes back to the version the plug-in's last update replaced (plugin.list → previousVersion), with its settings as they were before that update; one level deep. Its grant is kept, cut back to what that version asks for, without programs (only the user keeps those, from the plug-in's page). An error when no version is kept; nothing changes when the old module can't be written → the plug-in, as plugin.list lists it", always, rollback),
        cmd!(query "plugin.enable", "Enable Plug-in", [], None, "{id, enabled?: bool = true}", always, enable),
        cmd!(query "plugin.grant", "Set Plug-in Permissions", [], None, "{id, grant}: replaces the grant with one that may repeat or leave out what is granted (revoking what it leaves out), never add to it: a permission, host, folder (or its write), program or wider program entry the plug-in doesn't have is an error and changes nothing. Only the user allows, when the plug-in asks or in the Plug-in Manager → granted", always, set_grant),
        cmd!(query "plugin.revoke", "Revoke Plug-in Permission", [], None, "{id, group: catalog|metadataWrite|libraryAdd|pixels|crop|virtualCopy|network|fs|programs, value?: the host, folder or program id (none: all of the group)}: takes back one permission; the plug-in may ask for it again → granted", always, revoke),
        cmd!(query "plugin.picture", "Plug-in Picture", [], None, "{id, picture?: header|icon (header), theme?: dark|light (dark)}: a plug-in's or pixel filter's own picture: its header for that interface theme, or its icon (the manifest's `header` and `icon`) → {svg: SVG text | null}", always, picture),
        cmd!(query "plugin.log", "Plug-in Log", [], None, "{id} → [lines]", always, log),
        cmd!(query "plugin.commands", "Plug-in Commands", [], None, "{} → [{plugin, pluginName, command, label, menu, dialog, prepare, quickAction, values: what the dialog opens with (the last-run values of fields that remember, else defaults)}] of enabled plug-ins", always, commands),
        cmd!(query "plugin.prepare", "Prepare Plug-in Command", [], None, "{plugin, command, ids?, source?: {kind: album|smartAlbum|folder|allPhotos|other, id?, name?, path?} (default: the Library source shown)}: asks a command with `prepare` how to run on ids / the selection (others: their dialog as declared) → {dialog: [field + value], error: why the plug-in's own options are missing | null} | {run: args} | {message}; runs nothing", always, prepare),
        cmd!(query "plugin.run", "Run Plug-in Command", [], None, "{plugin, command, args?: dialog values, ids?, source?: as plugin.prepare, wait?: a job runs to its end}: runs a plug-in menu command on ids / the selection, telling it the Library source it was started from (`source`, default: the one shown) → its result; args given are remembered for its dialog. A command with `prepare` is asked first: a message → {message, ran: false} and nothing runs; its own args are used when `args` is missing; args are checked against its dialog as prepared. A job (a command with `job`) runs on a worker with a row in the activity stack and returns at once → {job: its activity id (activity.list, activity.cancel), running: true, plugin, command}; with `wait: true` it runs here to its end → its result (a cancelled job is an error). Headless sessions answer a job's catalog reads only with `wait`", always, run),
        cmd!(query "plugin.settings", "Plug-in Settings", [], None, "{id} → {id, fields: [field + value], values: {key: value}, error: why the plug-in's own options are missing | null}; kept for the whole app, defaults for what was never set", always, settings),
        cmd!(query "plugin.setSettings", "Set Plug-in Settings", [], None, "{id, values: {key: value (null: the default)}}: each value checked against its field (numbers clamped); an unknown key or a value that doesn't fit changes nothing → {id, values}", always, set_settings),
        cmd!(query "plugin.suggestMetadata", "Suggest Metadata (Plug-ins)", [], None, "{ids?}: asks metadata providers about the photos → [{plugin, ok: {suggestions}} | {plugin, error}]; writes nothing", always, suggest),
        cmd!(query "plugin.postExport", "Run Export Hooks", [], None, "{files: [{path, photo?}]}: runs plug-in export post-process hooks on exported files → [{plugin, ok|error}]", always, post_export),
        cmd!(query "plugin.exportDestinations", "Plug-in Export Destinations", [], None, "{} → [{plugin, pluginName, destination, name, dialog, values: what its dialog opens with}] of enabled plug-ins; pass one as app.export {pluginDestination: {plugin, destination, args}} (args are remembered)", always, destinations),
        cmd!(query "plugin.exportTo", "Send to Plug-in Destination", [], None, "{plugin, destination, files: [{path, photo?}], args?: dialog values}: hands exported files to a plug-in export destination → its result", always, export_to),
        cmd!(query "plugin.metadataFields", "Plug-in Metadata Fields", [], None, "{ids?: photos | id?: photo (default the active one)} → {photo, count, plugins: [{plugin, pluginName, fields: [{key, label, type, readOnly, value, mixed?}]} | {plugin, error}]}; with several photos a field whose values differ is `mixed` (value null)", always, metadata_fields),
        cmd!(query "plugin.setField", "Set Plug-in Metadata Field", [], None, "{plugin, field, value (null clears), ids?: photos | id?: photo (default the active one)}: read-only fields and values the field doesn't take are refused → {photo, count, field, value}; the catalog's copy (rules `plugin:<plugin>/<field>`, Any Searchable Text, XMP) follows", always, set_field),
        cmd!(query "plugin.publishServices", "Plug-in Publish Services", [], None, "{} → [{kind: plugin:<id> (a publish.addService kind), plugin, id, name}]", always, services),
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The manager is process-wide: tests that install plug-ins take turns, so one test's export
    /// hooks never hold another's plug-in busy.
    static TURN: Mutex<()> = Mutex::new(());

    pub(crate) fn turn() -> MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A plug-in publish service whose upload answers `{remoteId: "r1"}` and whose export hook
    /// answers `{seen: true}`.
    fn publisher_module() -> Vec<u8> {
        let manifest = r#"{"id":"test.pub","name":"Pub","version":"1.0.0","hooks":{"export":true},"publish":{"id":"p","name":"P"},"exportDestinations":[{"id":"d","name":"D"}],"metadataFields":[{"key":"job","label":"Job","type":"text"},{"key":"ro","label":"RO","type":"bool","readOnly":true}]}"#;
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
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&publisher_module(), None, false).unwrap();
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
        let manifest =
            format!(r#"{{"id":"{id}","name":"Hook","version":"1.0.0","hooks":{{"export":true}},"exportDestinations":[{{"id":"d","name":"D"}}]}}"#);
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
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&hook_module("test.relay"), None, false).unwrap();
        let files = [json!({"path": "/tmp/x.jpg"})];
        let d = json!({"plugin": "test.relay", "destination": "d"});
        let hooks = export_hooks("", &files, Some(&d)).unwrap();
        let mut s = Session::with_demo();
        let (mut host, requests) = relay_host(&s);
        let worker = std::thread::spawn(move || {
            // a catalog read from the worker waits for the session's thread to answer it
            let stats = host.catalog("catalog.stats", &json!({}));
            let r = hooks.run(&mut host, &|| false, &mut |_, _| {});
            (stats, r)
        });
        requests.recv_timeout(std::time::Duration::from_secs(20)).unwrap().answer(&mut s);
        let (stats, r) = worker.join().unwrap();
        assert!(stats.unwrap().is_object());
        let mine: Vec<&Value> = r.iter().filter(|v| v["plugin"] == "test.relay").collect();
        assert!(mine.iter().any(|v| v["ok"]["seen"] == true && v.get("destination").is_none()), "{r:?}");
        assert_eq!(mine.last().unwrap()["destination"], "d", "{r:?}");
        // the relay with nobody answering: an error, not a hang
        let (mut host, requests) = relay_host(&s);
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
            r#"{{"id":"{id}","name":"Jobs","version":"1.0.0","metadataFields":[{{"key":"job","label":"Job","type":"text"}},{{"key":"ok","label":"OK","type":"bool"}},{{"key":"n","label":"N","type":"number","min":0,"max":1000,"default":0}}]}}"#
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
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&fields_module("test.jobs"), None, false).unwrap();
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
        let dir = lcc_testkit::TempDir::new("plugin-fields-lib");
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
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&fields_module("test.back"), None, false).unwrap();
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
        manager().get_or_insert_with(Manager::in_memory).install_bytes(&fields_module("test.libs"), None, false).unwrap();
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

    /// A plug-in with `manifest` that answers every call with `{}`, written to `<name>.wasm` in a
    /// Temp folder of its own, removed when the returned guard is dropped.
    pub(crate) fn module_file(name: &str, manifest: &Value) -> lcc_testkit::TempFile {
        let manifest = manifest.to_string();
        let esc = |s: &str| s.replace('"', "\\\"");
        let src = format!(
            r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 8192))))"#,
            m = esc(&manifest),
            ml = manifest.len()
        );
        let file = lcc_testkit::TempFile::new(&format!("lcc-engine-plugin-compat-{name}"), &format!("{name}.wasm"));
        std::fs::write(&file, wat::parse_str(&src).unwrap()).unwrap();
        file
    }

    /// A plug-in's own icon and header, as a pixel filter's: listed, and served by `plugin.picture`.
    #[test]
    fn a_plugins_pictures_are_listed_and_served() {
        let _turn = turn();
        let mut s = Session::with_demo();
        let svg = "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 4 4'><rect width='4' height='4'/></svg>";
        let light = "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 2 2'><rect width='1' height='1'/></svg>";
        let manifest = json!({"id": "test.pictures", "name": "Pictures", "version": "1.0.0", "icon": svg, "header": {"dark": svg, "light": light}});
        let pictures = module_file("pictures", &manifest);
        let info = s.execute("plugin.install", &json!({"path": pictures.path()})).unwrap();
        assert_eq!((info["icon"].as_bool(), info["header"].as_bool()), (Some(true), Some(true)));
        let listed = s.execute("plugin.list", &json!({})).unwrap();
        let row = listed["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "test.pictures").unwrap().clone();
        assert_eq!(row["icon"], true);
        assert_eq!(s.execute("plugin.picture", &json!({"id": "test.pictures", "picture": "icon"})).unwrap()["svg"], svg);
        assert_eq!(s.execute("plugin.picture", &json!({"id": "test.pictures"})).unwrap()["svg"], svg, "the header, dark by default");
        assert_eq!(s.execute("plugin.picture", &json!({"id": "test.pictures", "theme": "light"})).unwrap()["svg"], light);
        // without pictures: listed as absent, none served
        let plain_file = module_file("plain", &json!({"id": "test.plain", "name": "Plain", "version": "1.0.0"}));
        let plain = s.execute("plugin.install", &json!({"path": plain_file.path()})).unwrap();
        assert_eq!((plain["icon"].as_bool(), plain["header"].as_bool()), (Some(false), Some(false)));
        assert_eq!(s.execute("plugin.picture", &json!({"id": "test.plain", "picture": "icon"})).unwrap()["svg"], Value::Null);
        assert!(s.execute("plugin.picture", &json!({"id": "test.nowhere"})).is_err());
        for id in ["test.pictures", "test.plain"] {
            s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
        }
    }

    #[test]
    fn install_checks_requires_and_versions_and_inspect_says_when_a_file_is_what_is_installed() {
        // `requires`, the version rule and downgrades through the commands the install dialog and
        // agents use; `plugin.list` says what the host has.
        {
            let _turn = turn();
            let mut s = Session::with_demo();
            let list = s.execute("plugin.list", &json!({})).unwrap();
            assert_eq!(list["host"]["abi"], lcc_plugins::ABI_VERSION);
            assert_eq!(list["host"]["features"], json!(lcc_plugins::HOST_FEATURES));

            // a feature no host has: the dialog learns why, the install is refused
            let needs = module_file("needs", &json!({"id": "test.needs", "name": "N", "version": "1.0.0", "requires": ["test-unknown-feature"]}));
            let info = s.execute("plugin.inspectFile", &json!({"path": needs.path()})).unwrap();
            assert_eq!(info["missing"], json!(["test-unknown-feature"]));
            let e = s.execute("plugin.install", &json!({"path": needs.path()})).unwrap_err().to_string();
            assert!(e.contains("test-unknown-feature"), "{e}");
            let listed = |s: &mut Session, id: &str| {
                let l = s.execute("plugin.list", &json!({})).unwrap();
                l["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).cloned()
            };
            assert!(listed(&mut s, "test.needs").is_none());

            // a feature this host has (set for the test): installs, and the list shows what it requires
            manager().get_or_insert_with(Manager::in_memory).set_features(&["test-known-feature"]);
            let known = module_file("known", &json!({"id": "test.known", "name": "K", "version": "1.0.0", "requires": ["test-known-feature"]}));
            let r = s.execute("plugin.install", &json!({"path": known.path()}));
            manager().get_or_insert_with(Manager::in_memory).set_features(lcc_plugins::HOST_FEATURES);
            r.unwrap();
            assert_eq!(listed(&mut s, "test.known").unwrap()["requires"], json!(["test-known-feature"]));
            s.execute("plugin.uninstall", &json!({"id": "test.known"})).unwrap();

            // the version: semver, and a downgrade only when confirmed
            let bad = module_file("bad", &json!({"id": "test.ver", "name": "V", "version": "1.2"}));
            assert!(s.execute("plugin.inspectFile", &json!({"path": bad.path()})).unwrap()["versionError"].is_string());
            assert!(s.execute("plugin.install", &json!({"path": bad.path()})).unwrap_err().to_string().contains("MAJOR.MINOR.PATCH"));
            let v12 = module_file("v12", &json!({"id": "test.ver", "name": "V", "version": "1.2.0"}));
            let v11 = module_file("v11", &json!({"id": "test.ver", "name": "V", "version": "1.1.0"}));
            s.execute("plugin.install", &json!({"path": v12.path()})).unwrap();
            let info = s.execute("plugin.inspectFile", &json!({"path": v11.path()})).unwrap();
            assert_eq!((info["downgrade"].clone(), info["installed"]["version"].clone()), (json!(true), json!("1.2.0")));
            assert_eq!(s.execute("plugin.inspectFile", &json!({"path": v12.path()})).unwrap()["downgrade"], false, "the same version is a repair");
            let e = s.execute("plugin.install", &json!({"path": v11.path()})).unwrap_err().to_string();
            assert!(e.contains("1.1.0") && e.contains("1.2.0"), "{e}");
            assert_eq!(listed(&mut s, "test.ver").unwrap()["version"], "1.2.0");
            let r = s.execute("plugin.install", &json!({"path": v11.path(), "allowDowngrade": true})).unwrap();
            assert_eq!(r["version"], "1.1.0");
            let log = s.execute("plugin.log", &json!({"id": "test.ver"})).unwrap();
            assert!(log.as_array().unwrap().iter().any(|l| l == "downgraded from 1.2.0 to 1.1.0"), "{log}");
            s.execute("plugin.uninstall", &json!({"id": "test.ver"})).unwrap();
        }
        // The install dialog learns whether a file is exactly what is installed (`installed.same`):
        // the same version and code, not a repair (other code) or an update; and `plugin.list` gives
        // what a page of the plug-in needs to notice it changed (`moduleHash`, `settingsRevision`).
        {
            let _turn = turn();
            let mut s = Session::with_demo();
            let settings = json!([{"key": "level", "label": "Level", "type": "number", "min": 0, "max": 10, "default": 1}]);
            let v1 = module_file("same-v1", &json!({"id": "test.same", "name": "S", "version": "1.0.0", "settings": settings}));
            let repair = module_file("same-v1b", &json!({"id": "test.same", "name": "S again", "version": "1.0.0", "settings": settings}));
            let v2 = module_file("same-v2", &json!({"id": "test.same", "name": "S", "version": "2.0.0", "settings": settings}));
            let same = |s: &mut Session, path: &std::path::Path| {
                s.execute("plugin.inspectFile", &json!({"path": path})).unwrap()["installed"]["same"].clone()
            };
            assert_eq!(same(&mut s, &v1), Value::Null, "nothing installed yet");
            s.execute("plugin.install", &json!({"path": v1.path()})).unwrap();
            assert_eq!(same(&mut s, &v1), true, "the installed module again");
            assert_eq!(same(&mut s, &repair), false, "the same version with other code is a repair");
            assert_eq!(same(&mut s, &v2), false, "a newer version is an update");

            let listed = |s: &mut Session| {
                let l = s.execute("plugin.list", &json!({})).unwrap();
                l["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "test.same").cloned().unwrap()
            };
            let before = listed(&mut s);
            assert!(before["moduleHash"].as_str().is_some_and(|h| h.len() == 64), "{before}");
            s.execute("plugin.setSettings", &json!({"id": "test.same", "values": {"level": 5}})).unwrap();
            let after = listed(&mut s);
            assert_ne!(before["settingsRevision"], after["settingsRevision"], "a saved setting changes the revision");
            s.execute("plugin.install", &json!({"path": v2.path()})).unwrap();
            assert_ne!(after["moduleHash"], listed(&mut s)["moduleHash"], "an update changes the module hash");
            s.execute("plugin.uninstall", &json!({"id": "test.same"})).unwrap();
        }
    }

    #[test]
    fn modules_that_did_not_load_are_listed_with_their_kind_and_a_filter_module_is_never_uninstalled_as_one() {
        // A module that didn't load is listed with its kind: a pixel filter's module as a filter, anything
        // else as a plug-in, so the Plug-in Manager shows it in its section.
        {
            let _turn = turn();
            let mut s = Session::with_demo();
            let dir = lcc_testkit::TempDir::new("lcc-engine-failed-kind");
            let filter = dir.join("test.failedkind.filter.wasm");
            std::fs::write(&filter, crate::tests_plugin_filters::invert_module("test.failedkind.filter")).unwrap();
            // a filter module the registry refuses (its manifest isn't JSON)
            let refused = dir.join("test.failedkind.refused.wasm");
            let refused_bytes = crate::tests_plugin_filters::module("{ not a manifest", None);
            let why = lcc_filters::Filter::load(&refused_bytes, lcc_filters::Limits::default()).expect_err("the filter is refused").to_string();
            std::fs::write(&refused, &refused_bytes).unwrap();
            let other = dir.join("test.failedkind.plugin.wasm");
            std::fs::write(&other, b"\0asm and then nothing a module needs").unwrap();
            let row = |path: &std::path::Path, id: &str| lcc_plugins::Failed {
                file: path.file_name().unwrap().to_string_lossy().into_owned(),
                error: "didn't load".into(),
                missing: vec![],
                id: id.into(),
                path: Some(path.to_path_buf()),
            };
            let mut g = manager();
            let m = g.get_or_insert_with(Manager::in_memory);
            m.failed.push(row(&filter, "test.failedkind.filter"));
            m.failed.push(row(&other, "test.failedkind.plugin"));
            m.failed.push(row(&refused, "test.failedkind.refused"));
            // a file that's gone isn't a filter
            m.failed.push(row(&dir.join("test.failedkind.gone.wasm"), "test.failedkind.gone"));
            drop(g);
            let listed = s.execute("plugin.list", &json!({}));
            manager().as_mut().unwrap().failed.retain(|f| !f.id.starts_with("test.failedkind."));
            let _ = std::fs::remove_dir_all(&dir);
            let listed = listed.unwrap();
            let kind = |id: &str| listed["failed"].as_array().unwrap().iter().find(|f| f["id"] == id).map(|f| f["kind"].clone());
            assert_eq!(kind("test.failedkind.filter"), Some(json!("filter")));
            assert_eq!(kind("test.failedkind.plugin"), Some(json!("plugin")));
            assert_eq!(kind("test.failedkind.gone"), Some(json!("plugin")));
            // a refused filter: listed as one, with the filter registry's reason
            assert_eq!(kind("test.failedkind.refused"), Some(json!("filter")));
            let error = |id: &str| listed["failed"].as_array().unwrap().iter().find(|f| f["id"] == id).map(|f| f["error"].clone());
            assert_eq!(error("test.failedkind.refused"), Some(json!(why)), "the filter's own reason");
            assert_eq!(error("test.failedkind.plugin"), Some(json!("didn't load")), "a plug-in keeps the manager's");
        }
        // A pixel filter's module also fails to load in the plug-in manager; `plugin.uninstall` by its
        // file name never removes it as a failed plug-in.
        {
            let _turn = turn();
            let mut s = Session::with_demo();
            let file = std::env::temp_dir().join("lcc-engine-guard").join("copied-in.wasm").display().to_string();
            let bytes = crate::tests_plugin_filters::invert_module("test.guard.filter");
            let filter = lcc_filters::Filter::load(&bytes, lcc_filters::Limits::default()).unwrap().with_source(file.clone());
            crate::filter_plugins::registry().install(filter);
            let row = lcc_plugins::Failed { file: file.clone(), error: "not a plug-in".into(), missing: vec![], id: "copied-in".into(), path: None };
            manager().get_or_insert_with(Manager::in_memory).failed.push(row);
            let r = s.execute("plugin.uninstall", &json!({"id": "copied-in"}));
            let kept = manager().as_ref().unwrap().failed.iter().any(|f| f.id == "copied-in");
            manager().as_mut().unwrap().failed.retain(|f| f.id != "copied-in");
            crate::filter_plugins::registry().remove("test.guard.filter");
            assert!(r.unwrap_err().to_string().contains("pixel filter"));
            assert!(kept, "the filter's row is untouched");
        }
    }

    /// A plug-in with `manifest` that answers every call with `reply`, written to `<name>.wasm` in a
    /// Temp folder of its own, removed when the returned guard is dropped.
    fn replying_module_file(name: &str, manifest: &Value, reply: &str) -> lcc_testkit::TempFile {
        let manifest = manifest.to_string();
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
        let file = lcc_testkit::TempFile::new(&format!("lcc-engine-plugin-settings-{name}"), &format!("{name}.wasm"));
        std::fs::write(&file, wat::parse_str(&src).unwrap()).unwrap();
        file
    }

    /// Agents read and change a plug-in's settings with `plugin.settings` / `plugin.setSettings`,
    /// checked like the plug-in's own calls; `plugin.prepare` and `plugin.commands` give what a
    /// command's dialog opens with, and a prepare step's message stops the run.
    #[test]
    fn settings_and_prepare_through_engine_commands() {
        let _turn = turn();
        let mut s = Session::with_demo();
        let manifest = json!({"id": "test.engset", "name": "S", "version": "1.0.0", "requires": ["settings"],
            "settings": [{"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 3},
                {"key": "mode", "label": "Mode", "type": "choice", "options": ["fast", "best"]}],
            "commands": [{"id": "go", "label": "Go", "dialog": [{"key": "who", "label": "Who", "type": "text", "default": "me"}]},
                {"id": "ask", "label": "Ask", "prepare": true, "dialog": [{"key": "v", "label": "V", "type": "choice", "options": []}]}]});
        let path = replying_module_file("engset", &manifest, r#"{"ok":{"message":"Nothing to do here."}}"#);
        s.execute("plugin.install", &json!({"path": path.path()})).unwrap();

        let got = s.execute("plugin.settings", &json!({"id": "test.engset"})).unwrap();
        assert_eq!(got["values"], json!({"n": 3.0, "mode": "fast"}));
        assert_eq!(got["fields"][1]["value"], "fast");
        let set = s.execute("plugin.setSettings", &json!({"id": "test.engset", "values": {"n": 12, "mode": "best"}})).unwrap();
        assert_eq!(set["values"], json!({"n": 10.0, "mode": "best"}));
        for bad in [json!({"mode": "slow"}), json!({"other": 1}), json!({"n": 1, "mode": 2})] {
            assert!(s.execute("plugin.setSettings", &json!({"id": "test.engset", "values": bad})).is_err(), "{bad}");
        }
        assert!(s.execute("plugin.setSettings", &json!({"id": "test.engset"})).is_err(), "values required");
        assert_eq!(s.execute("plugin.settings", &json!({"id": "test.engset"})).unwrap()["values"], json!({"n": 10.0, "mode": "best"}));

        // a dialog opens with what it last ran with
        let start = |s: &mut Session| {
            let c = s.execute("plugin.commands", &json!({})).unwrap();
            c.as_array().unwrap().iter().find(|c| c["plugin"] == "test.engset" && c["command"] == "go").unwrap()["values"].clone()
        };
        assert_eq!(start(&mut s), json!({"who": "me"}));
        s.execute("plugin.run", &json!({"plugin": "test.engset", "command": "go", "args": {"who": "you"}})).unwrap();
        assert_eq!(start(&mut s), json!({"who": "you"}));
        let p = s.execute("plugin.prepare", &json!({"plugin": "test.engset", "command": "go"})).unwrap();
        assert_eq!(p["dialog"][0]["value"], "you");
        // a prepare step that explains: nothing runs
        let p = s.execute("plugin.prepare", &json!({"plugin": "test.engset", "command": "ask"})).unwrap();
        assert_eq!(p, json!({"message": "Nothing to do here."}));
        let r = s.execute("plugin.run", &json!({"plugin": "test.engset", "command": "ask", "args": {"v": "x"}})).unwrap();
        assert_eq!(r, json!({"message": "Nothing to do here.", "ran": false}));
        s.execute("plugin.uninstall", &json!({"id": "test.engset"})).unwrap();
        assert!(s.execute("plugin.settings", &json!({"id": "test.engset"})).is_err());
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn host_reads_the_catalog_guards_writes_and_takes_string_ids() {
        // the host reads the catalog and guards writes
        {
            let mut s = Session::with_demo();
            let mut h = SessionHost(&mut s);
            let stats = h.catalog("catalog.stats", &json!({})).unwrap();
            assert!(stats.is_object());
            assert!(h.catalog("photo.delete", &json!({})).is_err());
            assert!(h.set_metadata(&json!({"title": "x"})).is_err(), "ids required");
            assert!(h.set_metadata(&json!({"ids": [1], "rating": 5})).is_err(), "only standard fields");
        }
        // the host takes the string ids plug-ins get
        {
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

    /// A plug-in with the program `conv` whose every call sends `program.start` for it. Its
    /// `program.end` hook fails (the run is still active then, so the start is refused as already
    /// running) and logs that once per run, which the tests count.
    fn program_module() -> Vec<u8> {
        program_module_with(r#""commands":[{"id":"go","label":"Go"}]"#)
    }

    /// [`program_module`] with `more` in its manifest (commands, hooks, destinations).
    fn program_module_with(more: &str) -> Vec<u8> {
        let manifest = format!(
            r#"{{"id":"test.prog","name":"Prog","version":"1.0.0","requires":["programs"],"permissions":{{"programs":[{{"id":"conv","name":"Converter","macBundleIds":["org.example.conv"],"windowsExe":"conv.exe","linuxExe":"conv"}}]}},{more}}}"#
        );
        let request = r#"{"method":"program.start","params":{"program":"conv","args":["{tmpDir}/job.txt"],"files":{"job.txt":"1\n"}}}"#;
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let src = format!(
            r#"(module
  (import "host" "call" (func $call (param i32 i32) (result i32)))
  (import "host" "response" (func $response (param i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (local $n i32)
    (local.set $n (call $call (i32.const 8192) (i32.const {rl})))
    (call $response (i32.const 16384))
    (i64.or (i64.shl (i64.extend_i32_u (local.get $n)) (i64.const 32)) (i64.const 16384))))"#,
            m = esc(&manifest),
            r = esc(request),
            ml = manifest.len(),
            rl = request.len()
        );
        wat::parse_str(&src).unwrap()
    }

    /// Waits up to 20 s for `done`.
    fn eventually(what: &str, done: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(start.elapsed() < std::time::Duration::from_secs(20), "timed out waiting: {what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn end_failures() -> usize {
        manager()
            .get_or_insert_with(Manager::in_memory)
            .get("test.prog")
            .map_or(0, |p| p.log.iter().filter(|l| l.starts_with("program.end failed")).count())
    }

    /// A run: recorded start with its folder, a row in the activity stack, ✕ ends it without
    /// stopping the program (the folder stays), `program.end` once per run, the folder removed
    /// once the program has exited.
    #[test]
    fn a_plugin_program_run_is_watched_and_cleaned_up() {
        let _turn = turn();
        let mut s = Session::with_demo();
        let desktop = std::sync::Arc::new(crate::desktop::Recorder::default());
        s.desktop = desktop.clone();
        let decl = json!({"programs": [{"id": "conv", "name": "Converter", "linuxExe": "conv", "windowsExe": "conv.exe", "macBundleIds": ["org.example.conv"]}]});
        manager()
            .get_or_insert_with(Manager::in_memory)
            .install_bytes_by_user(&program_module(), Some(serde_json::from_value(decl).unwrap()), false)
            .unwrap();

        let first = s.execute("plugin.run", &json!({"plugin": "test.prog", "command": "go"})).unwrap();
        let folder = PathBuf::from(first["tmpDir"].as_str().unwrap());
        let job = folder.join("job.txt");
        assert_eq!(std::fs::read_to_string(&job).unwrap(), "1\n", "the files are written before the start");
        let started = desktop.requests();
        assert!(
            matches!(started.as_slice(), [crate::desktop::Request::StartProgram { args, tmp, .. }] if args == &[format!("{}/job.txt", folder.display())] && tmp == &folder),
            "{started:?}"
        );
        let rows: Vec<_> = s.activity.list().into_iter().filter(|t| t.kind == "pluginProgram").collect();
        assert_eq!(rows.len(), 1, "one row for the run");
        // the row names the program and the plug-in, says when the results come back, and warns that stopping
        // throws them away (agents see the same in `activity.list`)
        assert_eq!(rows[0].label, "Waiting for Converter to finish (Prog)");
        assert_eq!(rows[0].detail, "The results come back when Converter closes");
        let listed = s.execute("activity.list", &json!({})).unwrap();
        let listed = listed["tasks"].as_array().into_iter().flatten().find(|t| t["kind"] == "pluginProgram").cloned().unwrap_or_default();
        assert!(listed["stopWarning"].as_str().is_some_and(|w| w.contains("Converter keeps running")), "{listed}");
        assert_eq!(listed["label"], "Waiting for Converter to finish (Prog)", "{listed}");

        // activity.cancel (what ✕ was; the app now asks first): the run ends at once (its hook runs once), the
        // program and its folder stay
        s.activity.cancel(rows[0].id).unwrap();
        eventually("the cancelled run's hook", || end_failures() == 1);
        eventually("the cancelled run's row to go", || s.activity.list().iter().all(|t| t.kind != "pluginProgram"));
        // the watcher removes the folder right after the row if it doesn't wait for the program
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(folder.exists(), "the program may still use its folder");

        // the run is over, so a new start works
        let second = s.execute("plugin.run", &json!({"plugin": "test.prog", "command": "go"})).unwrap();
        let second_folder = PathBuf::from(second["tmpDir"].as_str().unwrap());
        assert_ne!(second_folder, folder);

        // both programs exit: the second run's hook runs once, and both folders go
        desktop.finish_programs();
        eventually("both folders to go", || !folder.exists() && !second_folder.exists());
        assert_eq!(end_failures(), 2, "program.end once per run");
        assert!(s.activity.list().iter().all(|t| t.kind != "pluginProgram"));
        manager().get_or_insert_with(Manager::in_memory).uninstall("test.prog").unwrap();
    }

    /// A desktop whose programs all fail as they start.
    struct FailsAtOnce;
    struct Failed;
    impl lcc_plugins::Running for Failed {
        fn try_wait(&mut self) -> std::result::Result<Option<lcc_plugins::Exit>, String> {
            Ok(Some(lcc_plugins::Exit { success: false, code: Some(1) }))
        }
    }
    impl crate::desktop::Desktop for FailsAtOnce {
        fn reveal(&self, _: &std::path::Path) -> std::result::Result<(), String> {
            Ok(())
        }
        fn launch(&self, _: &[String]) -> std::result::Result<(), String> {
            Ok(())
        }
        fn run_and_wait(&self, _: &[String]) -> std::result::Result<bool, String> {
            Ok(true)
        }
        fn find_program(&self, p: &lcc_plugins::ProgramDecl) -> Vec<lcc_plugins::Found> {
            crate::desktop::Recorder::default().find_program(p)
        }
        fn start_program(
            &self,
            _: &lcc_plugins::Found,
            _: &[String],
            _: &std::path::Path,
        ) -> std::result::Result<Box<dyn lcc_plugins::Running>, String> {
            Ok(Box::new(Failed))
        }
    }

    /// A program that exits with an error as it starts: the start is an error (in the plug-in's
    /// log), no row is shown, the run folder is gone and the program can be started again.
    #[test]
    fn a_program_that_fails_as_it_starts_leaves_nothing_behind() {
        let _turn = turn();
        let root = lcc_testkit::TempDir::new("lcc-engine-runs");
        let mut s = Session::with_demo();
        s.desktop = std::sync::Arc::new(FailsAtOnce);
        let decl = json!({"programs": [{"id": "conv", "name": "Converter", "linuxExe": "conv", "windowsExe": "conv.exe", "macBundleIds": ["org.example.conv"]}]});
        {
            let mut g = manager();
            let m = g.get_or_insert_with(Manager::in_memory);
            m.set_runs_root(Some(root.to_path_buf()));
            m.install_bytes_by_user(&program_module(), Some(serde_json::from_value(decl).unwrap()), false).unwrap();
        }
        for _ in 0..2 {
            let _ = s.execute("plugin.run", &json!({"plugin": "test.prog", "command": "go"}));
        }
        let mut g = manager();
        let m = g.get_or_insert_with(Manager::in_memory);
        let failed: Vec<String> = m.get("test.prog").unwrap().log.iter().filter(|l| l.starts_with("program.start failed")).cloned().collect();
        assert_eq!(failed.len(), 2, "each start fails, and the second isn't refused as already running: {failed:?}");
        assert!(failed[0].contains("exited at once"), "{failed:?}");
        assert!(s.activity.list().iter().all(|t| t.kind != "pluginProgram"), "no row");
        let left: Vec<_> = std::fs::read_dir(&root).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert!(left.is_empty(), "the run folders and their locks are gone: {left:?}");
        m.set_runs_root(None);
        m.uninstall("test.prog").unwrap();
        drop(g);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Hooks (`program.end`, export hooks and destinations, on a worker or the session's thread)
    /// find programs through the session's desktop and can't start one.
    #[test]
    fn hooks_look_programs_up_but_start_none() {
        use lcc_plugins::HostApi;
        let desktop = std::sync::Arc::new(crate::desktop::Recorder::default());
        let mut s = Session::with_demo();
        s.desktop = desktop.clone();
        let decl: lcc_plugins::ProgramDecl = serde_json::from_value(
            json!({"id": "conv", "name": "Converter", "linuxExe": "conv", "windowsExe": "conv.exe", "macBundleIds": ["org.example.conv"]}),
        )
        .unwrap();
        let root = lcc_testkit::TempDir::new("lcc-engine-hook-runs");
        let check = |host: &mut dyn HostApi, what: &str| {
            assert_eq!(host.find_program(&decl).len(), 1, "{what}: found through the desktop");
            let dir = lcc_plugins::RunDir::create(&root).unwrap();
            let folder = dir.path().to_path_buf();
            let found = host.find_program(&decl).remove(0);
            let reservation = lcc_plugins::Reservation::take("test.hook", "conv").unwrap();
            let req = lcc_plugins::StartRequest {
                plugin: "test.hook".into(),
                plugin_name: "Test hook".into(),
                program: decl.clone(),
                found,
                args: vec![],
                dir,
                reservation,
            };
            let e = host.start_program(req).unwrap_err();
            assert!(e.contains("from a command"), "{what}: {e}");
            assert!(!folder.exists(), "{what}: the refused run's folder is gone");
        };
        let (relay, _requests) = relay_host(&s);
        check(&mut super::super::plugin_programs::hook_host(relay), "program.end");
        let (mut relay, _requests) = relay_host(&s);
        check(&mut relay, "a worker's export hooks");
        check(&mut HookSession(SessionHost(&mut s)), "hooks on the session's thread");
        assert!(desktop.requests().is_empty(), "nothing started");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepare_steps_export_hooks_and_destinations_start_no_program() {
        // A prepare step only reads, so it starts no program: opening a command's dialog or the
        // plug-in's settings never runs one.
        {
            let _turn = turn();
            let mut s = Session::with_demo();
            let desktop = std::sync::Arc::new(crate::desktop::Recorder::default());
            s.desktop = desktop.clone();
            let decl = json!({"programs": [{"id": "conv", "name": "Converter", "linuxExe": "conv", "windowsExe": "conv.exe", "macBundleIds": ["org.example.conv"]}]});
            manager()
                .get_or_insert_with(Manager::in_memory)
                .install_bytes_by_user(
                    &program_module_with(
                        r#""commands":[{"id":"go","label":"Go","prepare":true}],"settings":[{"key":"n","label":"N","type":"number","min":0,"max":10,"default":1,"prepare":true}]"#,
                    ),
                    Some(serde_json::from_value(decl).unwrap()),
                    false,
                )
                .unwrap();
            let id = s.visible_cloned()[0].0;
            let prepared = s.execute("plugin.prepare", &json!({"id": "test.prog", "command": "go", "ids": [id]}));
            let settings = s.execute("plugin.settings", &json!({"id": "test.prog"}));
            let started = desktop.requests();
            // a program started by mistake exits, so its run ends and its folder goes
            desktop.finish_programs();
            for r in &started {
                if let crate::desktop::Request::StartProgram { tmp, .. } = r {
                    eventually("a mistaken run's folder to go", || !tmp.exists());
                }
            }
            manager().get_or_insert_with(Manager::in_memory).uninstall("test.prog").unwrap();
            assert!(started.is_empty(), "nothing started: {started:?}\nprepare: {prepared:?}\nsettings: {settings:?}");
            for (what, reply) in [("the dialog's prepare step", &prepared), ("the settings' prepare step", &settings)] {
                let error = reply.as_ref().ok().and_then(|r| r["error"].as_str()).unwrap_or_default();
                assert!(error.contains("can only read"), "{what} is refused as read-only: {reply:?}");
            }
        }
        // The hook commands run on the session's thread start nothing either.
        {
            let _turn = turn();
            let mut s = Session::with_demo();
            let desktop = std::sync::Arc::new(crate::desktop::Recorder::default());
            s.desktop = desktop.clone();
            let decl = json!({"programs": [{"id": "conv", "name": "Converter", "linuxExe": "conv", "windowsExe": "conv.exe", "macBundleIds": ["org.example.conv"]}]});
            manager()
                .get_or_insert_with(Manager::in_memory)
                .install_bytes_by_user(
                    &program_module_with(r#""hooks":{"export":true,"metadata":true},"exportDestinations":[{"id":"d","name":"D"}]"#),
                    Some(serde_json::from_value(decl).unwrap()),
                    false,
                )
                .unwrap();
            let file = json!([{"path": "/tmp/x.jpg"}]);
            let _ = s.execute("plugin.postExport", &json!({"files": file}));
            let _ = s.execute("plugin.exportTo", &json!({"plugin": "test.prog", "destination": "d", "files": file}));
            let _ = s.execute("plugin.suggestMetadata", &json!({}));
            let _ = after_export(&mut s, &[json!({"path": "/tmp/x.jpg"})], Some(&json!({"plugin": "test.prog", "destination": "d"})));
            assert!(desktop.requests().is_empty(), "nothing started");
            let mut g = manager();
            let m = g.get_or_insert_with(Manager::in_memory);
            let refused = m
                .get("test.prog")
                .unwrap()
                .log
                .iter()
                .filter(|l| l.starts_with("program.start failed") && l.contains("start the program from a command"))
                .count();
            assert_eq!(refused, 5, "each hook's start is refused: {:?}", m.get("test.prog").unwrap().log);
            m.uninstall("test.prog").unwrap();
        }
    }

    /// A relayed write its caller gave up on is never made later; one the session's thread has
    /// taken is waited for.
    #[test]
    fn a_relayed_request_given_up_on_is_never_answered_late() {
        let mut s = Session::with_demo();
        let id = s.visible_cloned()[0].0;
        let (host, requests) = relay_host(&s);
        let late = json!({"ids": [id.to_string()], "title": "Too late"});
        let e = host.ask_within(Relayed::SetMetadata, &late, std::time::Duration::from_millis(20)).unwrap_err();
        assert!(e.contains("didn't answer"), "{e}");
        requests.recv().unwrap().answer(&mut s);
        let title = s.execute("photo.inspect", &json!({"id": id})).unwrap()["meta"]["title"].clone();
        assert_ne!(title, "Too late", "the abandoned write was not made");

        // taken in time: the answer comes back even when it arrives after the limit
        let worker =
            std::thread::spawn(move || host.ask_within(Relayed::Catalog("catalog.stats".into()), &json!({}), std::time::Duration::from_millis(500)));
        let taken = requests.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        use std::sync::atomic::Ordering;
        assert!(taken.state.compare_exchange(WAITING, TAKEN, Ordering::AcqRel, Ordering::Acquire).is_ok(), "taken before the limit");
        std::thread::sleep(std::time::Duration::from_millis(800));
        taken.reply.send(Ok(json!({"late": true}))).unwrap();
        assert_eq!(worker.join().unwrap().unwrap()["late"], true, "the answer after the limit is the one returned");
    }

    /// A plug-in `id` whose command `go` sends `catalog.stats` and returns the reply.
    fn catalog_module(id: &str) -> Vec<u8> {
        catalog_module_with(id, r#"{"id":"go","label":"Go"}"#)
    }

    /// A plug-in `id` asking for `catalog` and `pixels` whose every call (a command, a prepare
    /// step) reads `catalog.stats`, with `command` its one command.
    fn catalog_module_with(id: &str, command: &str) -> Vec<u8> {
        let manifest =
            format!(r#"{{"id":"{id}","name":"Asks","version":"1.0.0","permissions":{{"catalog":true,"pixels":true}},"commands":[{command}]}}"#);
        let request = r#"{"method":"catalog.stats","params":{}}"#;
        let esc = |s: &str| s.replace('"', "\\\"");
        let src = format!(
            r#"(module
  (import "host" "call" (func $call (param i32 i32) (result i32)))
  (import "host" "response" (func $response (param i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (local $n i32)
    (local.set $n (call $call (i32.const 8192) (i32.const {rl})))
    (call $response (i32.const 16384))
    (i64.or (i64.shl (i64.extend_i32_u (local.get $n)) (i64.const 32)) (i64.const 16384))))"#,
            m = esc(&manifest),
            r = esc(request),
            ml = manifest.len(),
            rl = request.len()
        );
        wat::parse_str(&src).unwrap()
    }

    fn request_state(s: &mut Session, id: &str, group: &str) -> Value {
        let list = s.execute("plugin.list", &json!({})).unwrap();
        let p = list["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).cloned().unwrap();
        p["requests"].as_array().unwrap().iter().find(|r| r["group"] == group).cloned().unwrap()
    }

    #[test]
    fn commands_never_grant_and_a_run_or_prepare_step_the_user_started_leaves_an_ask() {
        // commands never grant, and a run the user started leaves an ask
        {
            let id = "test.asks";
            let mut s = Session::with_demo();
            let dir = lcc_testkit::TempDir::new("lcc-engine-asks");
            let file = dir.join("asks.wasm");
            std::fs::write(&file, catalog_module(id)).unwrap();
            // a command can't install with a grant, nor grant afterwards
            let e = s.execute("plugin.install", &json!({"path": file, "grant": {"catalog": true}})).unwrap_err().to_string();
            assert!(e.contains("only be allowed by the user"), "{e}");
            s.execute("plugin.install", &json!({"path": file})).unwrap();
            let e = s.execute("plugin.grant", &json!({"id": id, "grant": {"catalog": true}})).unwrap_err().to_string();
            assert!(e.contains("only be allowed by the user"), "{e}");
            assert_eq!(request_state(&mut s, id, "catalog")["state"], "never");
            // run by a request (an agent, a script): told where to allow it, nothing to ask
            let e = s.execute("plugin.run", &json!({"plugin": id, "command": "go"})).unwrap_err().to_string();
            assert!(e.contains("allow it in the Plug-in Manager"), "{e}");
            assert!(s.plugin_asks.is_empty(), "nobody is asked");
            // started by the user in the UI: the usual error, and an ask for the UI
            s.plugin_ask_user = true;
            let e = s.execute("plugin.run", &json!({"plugin": id, "command": "go"})).unwrap_err().to_string();
            s.plugin_ask_user = false;
            assert!(e.contains("permission `catalog` not granted"), "{e}");
            let ask = s.plugin_asks.pop().expect("an ask");
            assert!(s.plugin_asks.is_empty(), "one ask");
            assert_eq!(
                (ask.plugin.as_str(), ask.plugin_name.as_str(), ask.request.clone()),
                (id, "Asks", lcc_plugins::Request::group(lcc_plugins::Group::Catalog))
            );
            assert_eq!((ask.rerun["plugin"].as_str(), ask.rerun["command"].as_str()), (Some(id), Some("go")));
            // Allow, then the same run again
            allow_by_user(&mut s, &json!({"id": id, "group": "catalog"})).unwrap();
            let r = s.execute("plugin.run", &ask.rerun).unwrap();
            assert!(r.is_object(), "it ran: {r}");
            assert_eq!(request_state(&mut s, id, "catalog")["state"], "allowed");
            // a command revokes; the plug-in may ask again
            let g = s.execute("plugin.revoke", &json!({"id": id, "group": "catalog"})).unwrap();
            assert_eq!(g["granted"]["catalog"], false);
            assert_eq!(request_state(&mut s, id, "catalog")["state"], "never");
            assert!(s.execute("plugin.revoke", &json!({"id": id, "group": "nope"})).is_err(), "an unknown group");
            assert!(s.execute("plugin.revoke", &json!({"id": id, "group": "catalog", "value": "x"})).is_err(), "catalog takes no value");
            // Deny: remembered with a date, and a later run asks nothing
            deny_by_user(&mut s, &json!({"id": id, "group": "catalog"})).unwrap();
            let state = request_state(&mut s, id, "catalog");
            assert_eq!(state["state"], "denied");
            assert!(state["deniedOn"].as_str().is_some_and(|d| d.len() == 10), "{state}");
            s.plugin_ask_user = true;
            let e = s.execute("plugin.run", &json!({"plugin": id, "command": "go"})).unwrap_err().to_string();
            s.plugin_ask_user = false;
            assert!(e.contains("permission `catalog` not granted"), "{e}");
            assert!(s.plugin_asks.is_empty(), "a denied permission isn't asked again");
            s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }
        // A prepare step the user started asks too; Allow starts the command again (`prepare`), and
        // one with nobody to ask is told where to allow it.
        // a prepare step
        {
            let id = "test.asks.prepare";
            let mut s = Session::with_demo();
            let dir = lcc_testkit::TempDir::new("lcc-engine-asks-prepare");
            let file = dir.join("asks.wasm");
            std::fs::write(&file, catalog_module_with(id, r#"{"id":"go","label":"Go","prepare":true}"#)).unwrap();
            s.execute("plugin.install", &json!({"path": file})).unwrap();
            let prepare = json!({"plugin": id, "command": "go"});
            let r = s.execute("plugin.prepare", &prepare).unwrap();
            assert!(r["error"].as_str().is_some_and(|e| e.contains("allow it in the Plug-in Manager")), "{r}");
            assert!(s.plugin_asks.is_empty(), "nobody is asked");
            s.plugin_ask_user = true;
            let r = s.execute("plugin.prepare", &prepare).unwrap();
            s.plugin_ask_user = false;
            assert!(r["error"].as_str().is_some_and(|e| e.contains("permission `catalog` not granted")), "{r}");
            let ask = s.plugin_asks.pop().expect("an ask");
            assert!(s.plugin_asks.is_empty(), "one ask");
            assert_eq!(ask.request, lcc_plugins::Request::group(lcc_plugins::Group::Catalog));
            assert!(ask.prepare, "Allow starts the command again, as from its menu");
            assert_eq!((ask.rerun["plugin"].as_str(), ask.rerun["command"].as_str()), (Some(id), Some("go")));
            // denied: the next prepare step asks nothing
            deny_by_user(&mut s, &json!({"id": id, "group": "catalog"})).unwrap();
            s.plugin_ask_user = true;
            s.execute("plugin.prepare", &prepare).unwrap();
            s.plugin_ask_user = false;
            assert!(s.plugin_asks.is_empty(), "a denied permission isn't asked again");
            s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }
        // agents cannot grant programs
        {
            let _turn = turn();
            let mut s = Session::with_demo();
            let grant = json!({"programs": [{"id": "conv", "name": "Converter", "linuxExe": "conv", "windowsExe": "conv.exe", "macBundleIds": ["org.example.conv"]}]});
            let _ = manager().get_or_insert_with(Manager::in_memory).uninstall("test.prog");
            manager().get_or_insert_with(Manager::in_memory).install_bytes(&program_module(), None, false).unwrap();
            let e = s.execute("plugin.grant", &json!({"id": "test.prog", "grant": grant})).unwrap_err().to_string();
            assert!(e.contains("Plug-in Manager"), "{e}");
            // the Plug-in Manager's path grants it
            let g = grant_by_user(&mut s, &json!({"id": "test.prog", "grant": grant})).unwrap();
            assert_eq!(g["granted"]["programs"][0]["id"], "conv", "{g}");
            // an agent may repeat it (kept) or leave it out (revoked)
            let kept = s.execute("plugin.grant", &json!({"id": "test.prog", "grant": grant})).unwrap();
            assert_eq!(kept["granted"]["programs"][0]["id"], "conv", "{kept}");
            let revoked = s.execute("plugin.grant", &json!({"id": "test.prog", "grant": {}})).unwrap();
            assert!(revoked["granted"]["programs"].as_array().is_none_or(Vec::is_empty), "{revoked}");
            manager().get_or_insert_with(Manager::in_memory).uninstall("test.prog").unwrap();
        }
    }
}
