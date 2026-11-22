//! Installed pixel-filter plug-ins (PhotoCraft ABI, [`lcc_filters`]): a process-wide registry
//! beside the general plug-in manager, in the same folder (`cmd::plugins::plugins_dir`). The
//! `plugin.*` commands route modules that [`lcc_filters::is_filter_module`] recognises here, and
//! every render (preview, loupe, export, CLI, MCP) resolves a photo's filter stack through
//! [`EngineFilterHost`], the pipeline's default host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use lcc_filters::{Filter, Limits, Registry};
use serde_json::{Value, json};

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Most filter modules loaded from the folder.
const MAX_FILTERS: usize = 1000;

/// Filter modules in the folder that didn't load because an earlier one (by file name) has their
/// id: file → (id, why).
static DUPLICATES: Mutex<BTreeMap<String, (String, String)>> = Mutex::new(BTreeMap::new());

fn duplicates() -> std::sync::MutexGuard<'static, BTreeMap<String, (String, String)>> {
    DUPLICATES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A filter module that didn't load because another module has its id: `(id, why)`.
pub fn duplicate(file: &str) -> Option<(String, String)> {
    duplicates().get(file).cloned()
}

/// Notes that filter module `file` (filter `id`) didn't load because `first` has its id.
pub(crate) fn note_duplicate(file: &str, id: &str, first: &str) {
    duplicates().insert(file.to_string(), (id.to_string(), duplicate_error(first)));
}

/// Why a module didn't load when `first` has its id, in the plug-in host's words (the web build has
/// no plug-in host, nor a plug-ins folder to find duplicates in).
fn duplicate_error(first: &str) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    let why = lcc_plugins::duplicate_error(first);
    #[cfg(target_arch = "wasm32")]
    let why = format!("another module has this id: {first} (the first by file name loads)");
    why
}

/// Forgets the duplicate filter module `file` (removed).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn forget_duplicate(file: &str) {
    duplicates().remove(file);
}

/// The process-wide filter registry, loaded from the plug-ins folder on first use (kept in
/// memory in unit tests), with the pipeline's default host pointed at it.
pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| {
        let r = Registry::new(Limits::default());
        if !cfg!(test)
            && let Some(dir) = crate::cmd::plugins::plugins_dir()
        {
            load_dir(&r, &dir);
        }
        lcc_pipeline::plugin_filters::set_default_host(Some(Arc::new(EngineFilterHost)));
        r
    })
}

/// Installs every filter module in `dir` (others are the general manager's business); of two with
/// one id, the first by file name.
pub(crate) fn load_dir(r: &Registry, dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<PathBuf> =
        rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wasm"))).collect();
    files.sort();
    files.truncate(MAX_FILTERS);
    for f in files {
        let Ok(bytes) = read_limited(&f) else { continue };
        if !lcc_filters::is_filter_module(&bytes) {
            continue;
        }
        let file = f.display().to_string();
        match Filter::load(&bytes, Limits::default()) {
            Ok(filter) => match r.get(filter.id()).and_then(|first| first.source().map(str::to_string)) {
                Some(first) => note_duplicate(&file, filter.id(), &first),
                None => {
                    r.install(filter.with_source(file));
                }
            },
            Err(e) => log::warn!("filter plug-in {file}: {e}"),
        }
    }
}

/// Plug-ins and pixel filters share the folder, and each loader only knows its own kind: of a
/// plug-in and a filter with one id, the module first by file name stays loaded, and the other is
/// listed as a module that didn't load.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn settle_shared_ids(m: &mut lcc_plugins::Manager, r: &Registry) {
    let both: Vec<(String, PathBuf, String)> = m
        .list()
        .filter_map(|p| {
            let id = p.plugin.id().to_string();
            let source = r.get(&id)?.source()?.to_string();
            Some((id, p.path()?.to_path_buf(), source))
        })
        .collect();
    for (id, plugin, filter) in both {
        let plugin_file = plugin.display().to_string();
        if Path::new(&filter) < plugin.as_path() {
            m.demote(&id, duplicate_error(&filter));
        } else {
            r.remove(&id);
            note_duplicate(&filter, &id, &plugin_file);
        }
    }
}

fn read_limited(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let max = Limits::default().max_module_bytes as u64;
    if !std::fs::metadata(path)?.is_file() {
        return Err(std::io::Error::other("not a file"));
    }
    let mut b = Vec::new();
    std::fs::File::open(path)?.take(max + 1).read_to_end(&mut b)?;
    Ok(b)
}

/// Resolves stack entries to the installed filters.
pub struct EngineFilterHost;

impl lcc_pipeline::plugin_filters::FilterHost for EngineFilterHost {
    fn resolve(&self, plugin: &str, _module_hash: &str) -> Result<Arc<Filter>, String> {
        registry().get(plugin).ok_or_else(|| format!("plug-in {plugin:?} is not installed"))
    }
}

/// What a module file is to the filter registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterFile {
    /// Not a pixel filter (or unreadable).
    No,
    /// A pixel filter that loads.
    Loads,
    /// A pixel filter the registry refuses, and why.
    Refused(String),
}

/// What `path` is to the filter registry, remembered while the file's size and modification time
/// stay the same: `plugin.list` sorts the modules that didn't load by kind and gives a filter's own
/// error, and the Plug-in Manager lists them every frame.
pub fn filter_file_cached(path: &Path) -> FilterFile {
    type Seen = std::collections::HashMap<PathBuf, (u64, Option<std::time::SystemTime>, FilterFile)>;
    static SEEN: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
    let Ok(meta) = std::fs::metadata(path) else { return FilterFile::No };
    let stamp = (meta.len(), meta.modified().ok());
    let mut g = SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let seen = g.get_or_insert_with(Seen::new);
    if let Some((len, modified, what)) = seen.get(path)
        && (*len, *modified) == stamp
    {
        return what.clone();
    }
    let what = match read_limited(path) {
        Ok(bytes) if lcc_filters::is_filter_module(&bytes) => match Filter::load(&bytes, Limits::default()) {
            Ok(_) => FilterFile::Loads,
            Err(e) => FilterFile::Refused(e.to_string()),
        },
        _ => FilterFile::No,
    };
    if seen.len() >= 1000 {
        seen.clear();
    }
    seen.insert(path.to_path_buf(), (stamp.0, stamp.1, what.clone()));
    what
}

/// Whether filter version `new` is older than `installed`. Filter versions are free text: both
/// semver, they compare by semver precedence; both dotted numbers (`1`, `1.2`, `1.2.0.3`), part by
/// part, a missing part counting as 0. Anything else can't be compared, and isn't a downgrade.
#[cfg(not(target_arch = "wasm32"))]
pub fn is_older(new: &str, installed: &str) -> bool {
    use lcc_plugins::manifest::parse_version;
    if let (Ok(n), Ok(i)) = (parse_version(new), parse_version(installed)) {
        return n.cmp_precedence(&i) == std::cmp::Ordering::Less;
    }
    let parts = |v: &str| -> Option<Vec<u64>> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() > 8 {
            return None;
        }
        parts.iter().map(|p| p.parse::<u64>().ok()).collect()
    };
    let (Some(mut n), Some(mut i)) = (parts(new), parts(installed)) else { return false };
    let len = n.len().max(i.len());
    n.resize(len, 0);
    i.resize(len, 0);
    n < i
}

/// Filter manifest fields read leniently (a value the host can't use is dropped, not an error):
/// never reported as unknown.
#[cfg(not(target_arch = "wasm32"))]
const LENIENT: &[&str] = &[
    "stage",
    "analyzeArea",
    "baseTone",
    "header",
    "icon",
    "sections",
    "shortcut",
    "params.*.label",
    "params.*.groups",
    "homepage",
    "repository",
    "changelog",
    "license",
];

/// Fields of `f`'s manifest the host doesn't know, by path ([`lcc_plugins::manifest::ignored_fields`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn unknown_fields(f: &Filter) -> Vec<String> {
    let Ok(raw) = serde_json::from_str::<Value>(f.manifest_json()) else { return Vec::new() };
    lcc_plugins::manifest::ignored_fields(&raw, LENIENT, |v| lcc_filters::Manifest::parse(v.to_string().as_bytes()).ok())
}

/// Copies a filter module loaded from `bytes` into the plug-ins folder (when there is one) and
/// installs it.
pub fn install_loaded(filter: Filter, bytes: &[u8]) -> Result<Arc<Filter>, String> {
    let r = registry();
    let filter = match (!cfg!(test)).then(crate::cmd::plugins::plugins_dir).flatten() {
        Some(dir) => {
            let installed = r.get(filter.id());
            write_into(&dir, filter, bytes, installed.as_deref())?
        }
        None => filter,
    };
    Ok(r.install(filter))
}

/// Writes `filter`'s module (`bytes`) to `<dir>/<id>.wasm`, replacing `installed`, the filter with
/// its id now installed: when that one was copied in under another name, its file goes too, or it
/// would load first (by file name) in place of this one at the next start.
pub(crate) fn write_into(dir: &Path, filter: Filter, bytes: &[u8], installed: Option<&Filter>) -> Result<Filter, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dest = dir.join(format!("{}.wasm", filter.id()));
    let tmp = dir.join(format!(".{}.wasm.tmp", filter.id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let old = installed.and_then(|f| f.source().map(PathBuf::from)).filter(|p| *p != dest);
    if let Some(old) = old {
        match std::fs::remove_file(&old) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!(
                "pixel filter {}: the old module {} couldn't be removed ({e}); remove it by hand, or it may load again",
                filter.id(),
                old.display()
            ),
        }
    }
    Ok(filter.with_source(dest.display().to_string()))
}

/// Tests only: installs a filter module from `bytes` into the registry, with no file in the
/// plug-ins folder. Compiled into this crate's tests and, through the `test-support` feature, other
/// crates' tests; no app build has it, and no command, control request or MCP tool reaches it.
#[cfg(any(test, feature = "test-support"))]
pub fn install_in_memory(bytes: &[u8]) -> Result<Arc<Filter>, String> {
    let filter = Filter::load(bytes, Limits::default()).map_err(|e| e.to_string())?;
    Ok(registry().install(filter))
}

/// Uninstalls filter `id` and deletes its module file; false when no such filter is installed.
pub fn uninstall(id: &str) -> Result<bool, String> {
    let r = registry();
    let Some(f) = r.get(id) else { return Ok(false) };
    r.remove(id);
    if let Some(src) = f.source() {
        match std::fs::remove_file(src) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{src}: {e}")),
        }
    }
    Ok(true)
}

/// Plug-ins known to want the Source stage that don't declare `stage` in their manifest yet.
const STAGE_HINTS: &[(&str, lcc_develop::FilterStage)] = &[
    ("io.github.smyrgeorge.film-lab.convert", lcc_develop::FilterStage::Source),
    ("io.github.smyrgeorge.film-lab.convert-advanced", lcc_develop::FilterStage::Source),
];

/// The stage a new entry of `f` starts at: the manifest's `stage`, else the built-in hint, else Display.
pub fn preferred_stage(f: &Filter) -> lcc_develop::FilterStage {
    use lcc_develop::FilterStage;
    match f.manifest().stage {
        Some(lcc_filters::Stage::Source) => FilterStage::Source,
        Some(lcc_filters::Stage::Display) => FilterStage::Display,
        None => STAGE_HINTS.iter().find(|(id, _)| *id == f.id()).map(|(_, s)| *s).unwrap_or_default(),
    }
}

/// How `plugin.list` shows a filter plug-in. `params` is an object (no key order in JSON), so
/// `paramOrder` lists its keys in the manifest's order; each spec carries its display `label`.
pub fn info(f: &Filter) -> Value {
    let m = f.manifest();
    let mut params = serde_json::Map::new();
    for (k, spec) in &m.params {
        let mut v = serde_json::to_value(spec).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.insert("label".into(), Value::from(m.param_label(k)));
            if let Some(g) = m.param_groups(k) {
                o.insert("groups".into(), Value::from(g.to_vec()));
            }
        }
        params.insert(k.clone(), v);
    }
    let stage = match preferred_stage(f) {
        lcc_develop::FilterStage::Source => "source",
        lcc_develop::FilterStage::Display => "display",
    };
    json!({
        "id": m.id,
        "name": m.name,
        "version": m.version,
        "kind": "filter",
        "description": m.description,
        "author": m.author,
        "params": params,
        "paramOrder": m.params.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        "stage": stage,
        "overlap": m.overlap,
        "moduleHash": f.module_hash(),
        "analyze": f.has_analyze(),
        "merge": f.has_merge(),
        "sections": m.sections,
        "shortcut": m.shortcut,
        "analyzeArea": m.analyze_area,
        "baseTone": m.base_tone,
        "size": f.size(),
        "header": m.header.is_some(),
        "icon": m.icon.is_some(),
        "about": m.about.info(),
        "enabled": true,
    })
}

/// Module files of installed filters (so the general manager's load failures for them, which
/// are expected, aren't listed).
pub fn sources() -> Vec<String> {
    registry().list().iter().filter_map(|f| f.source().map(str::to_string)).collect()
}

/// The values last set for the parameters filters remember (manifest `"remember": true`), by
/// plug-in id: a newly added copy of the filter starts with them. Kept in the settings folder for
/// the whole app (in memory only in tests, and when there is no settings folder).
static REMEMBERED: Mutex<Option<serde_json::Map<String, Value>>> = Mutex::new(None);

/// Most plug-ins whose values are kept, and most bytes the file may have.
const MAX_REMEMBERED_PLUGINS: usize = 256;
const MAX_REMEMBERED_BYTES: u64 = 1024 * 1024;

fn remembered_path() -> Option<PathBuf> {
    if cfg!(test) { None } else { crate::config::config_dir().map(|d| d.join("filter-remembered.json")) }
}

fn remembered_all() -> std::sync::MutexGuard<'static, Option<serde_json::Map<String, Value>>> {
    let mut g = REMEMBERED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if g.is_none() {
        // (a missing, oversized or unreadable file remembers nothing)
        let read = remembered_path()
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= MAX_REMEMBERED_BYTES))
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        *g = Some(match read {
            Some(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        });
    }
    g
}

/// The values plug-in `plugin`'s filter last had for the parameters it remembers.
pub fn remembered(plugin: &str) -> serde_json::Map<String, Value> {
    match remembered_all().as_ref().and_then(|m| m.get(plugin)) {
        Some(Value::Object(v)) => v.clone(),
        _ => serde_json::Map::new(),
    }
}

/// Keeps those of `values` (already checked against the manifest) that are parameters plug-in
/// `plugin` remembers (`keys`, its manifest's list), beside what it remembered before, and drops
/// what it no longer remembers; written to the settings folder when there is one (a write that
/// fails is logged: the values are still kept for this run).
pub fn remember(plugin: &str, keys: &[String], values: &serde_json::Map<String, Value>) {
    let new: Vec<(String, Value)> = values.iter().filter(|(k, _)| keys.contains(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    if new.is_empty() {
        return;
    }
    let mut g = remembered_all();
    let Some(all) = g.as_mut() else { return };
    if !all.contains_key(plugin) && all.len() >= MAX_REMEMBERED_PLUGINS {
        return;
    }
    let entry = all.entry(plugin.to_string()).or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let Some(m) = entry.as_object_mut() else { return };
    let mut changed = false;
    for (k, v) in new {
        if m.get(&k) != Some(&v) {
            m.insert(k, v);
            changed = true;
        }
    }
    // the parameters the plug-in no longer remembers go
    let before = m.len();
    m.retain(|k, _| keys.contains(k));
    changed |= m.len() != before;
    if !changed {
        return;
    }
    if let (Some(path), Ok(bytes)) = (remembered_path(), serde_json::to_vec_pretty(&Value::Object(all.clone()))) {
        let part = path.with_extension("part");
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&part, &bytes))
            .and_then(|()| std::fs::rename(&part, &path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&part);
            log::warn!("remembering filter settings in {}: {e}", path.display());
        }
    }
}
