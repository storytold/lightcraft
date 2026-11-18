//! Plug-ins: the `plugin.*` commands: list, install (a file, base64 data, a GitHub repository or a
//! catalog's package), update, remove and run plug-ins, and the catalogs they come from. The
//! runtime and the rules a run follows are in [`crate::plugin`] (RFC-0046).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::plugin::{self, Request};
use crate::{Result, Session};

/// Run `f` with an HTTPS client for plug-in downloads; when `network`, listed in the activity
/// stack while it runs (and cancellable there).
fn with_fetch<T>(s: &Session, network: bool, f: impl FnOnce(&mut dyn plugin::craft_plugin::Fetch) -> T) -> T {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _task = network.then(|| s.activity.start(plugin::DOWNLOAD_KIND, plugin::DOWNLOAD_LABEL, crate::activity::Cancel::Flag(cancel.clone())));
    let mut client = plugin::plugin_fetch(s.plugins.network, cancel);
    f(client.as_mut())
}

/// Whether `url` is read over the network (not a `file://` catalog).
fn remote(url: &str) -> bool {
    !url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("file://"))
}

fn list(s: &mut Session, _p: &Value) -> Result<Value> {
    let plugins: Vec<Value> = s.plugins.list().iter().map(plugin::InstalledPlugin::describe).collect();
    let last = s.plugins.last.as_ref().map(|(id, command, _)| json!({"id": id, "command": command}));
    Ok(json!({
        "plugins": plugins,
        "problems": s.plugins.problems(),
        "catalogs": s.plugins.prefs.catalogs,
        "stored": s.plugins.has_store(),
        "last": last,
    }))
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "plugin.install";
    let request = plugin::install_request(p);
    let wanted = str_param(p, "sha256").map(str::to_string);
    let c = match wanted.as_deref().and_then(|h| plugin::take_held(s, h, &request)) {
        Some(c) => c,
        None => plugin::obtain(s, cmd, &Request::parse(cmd, p)?)?,
    };
    let (_, mut info) = plugin::review(s, cmd, &c)?;
    let sha256 = info["sha256"].as_str().unwrap_or_default().to_string();
    if let Some(w) = &wanted
        && !w.eq_ignore_ascii_case(&sha256)
    {
        return Err(bad(cmd, format!("the bundle's SHA-256 is {sha256}, not the {w} that was reviewed: review it again")));
    }
    if bool_or(p, "review", false) {
        plugin::hold(s, request, c);
        info["installed"] = json!(false);
        return Ok(info);
    }
    plugin::install(s, cmd, c)?;
    info["installed"] = json!(true);
    Ok(info)
}

fn update(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "plugin.update";
    let only = str_param(p, "id");
    if let Some(id) = only
        && s.plugins.get(id).is_none()
    {
        return Err(bad(cmd, format!("no plug-in `{id}` is installed")));
    }
    let apply = bool_or(p, "apply", false);
    let targets: Vec<plugin::InstalledPlugin> =
        s.plugins.list().iter().filter(|x| only.is_none_or(|id| x.manifest.id.eq_ignore_ascii_case(id))).cloned().collect();
    let (mut updates, mut updated, mut errors) = (vec![], vec![], vec![]);
    for old in targets {
        let id = old.manifest.id.clone();
        let network = match &old.source {
            plugin::craft_plugin::Source::Catalog { catalog, .. } => remote(catalog),
            plugin::craft_plugin::Source::Github { .. } => true,
            _ => continue,
        };
        let (params, available) = match with_fetch(s, network, |f| plugin::update_for(f, &old)) {
            Ok(Some(found)) => found,
            Ok(None) => continue,
            Err(e) => {
                errors.push(json!({"id": id, "error": e}));
                continue;
            }
        };
        let mut entry = json!({"id": id, "installed": old.manifest.version, "available": available, "install": params});
        if apply {
            match apply_update(s, cmd, &params) {
                Ok(true) => updated.push(json!(id)),
                Ok(false) => entry["permissionsChanged"] = json!(true),
                Err(e) => errors.push(json!({"id": id, "error": e.to_string()})),
            }
        }
        updates.push(entry);
    }
    Ok(json!({"updates": updates, "updated": updated, "errors": errors}))
}

/// Install the update `params` name, unless it asks for other permissions (then it waits for its
/// confirmation, `plugin.install`): whether it was installed.
fn apply_update(s: &mut Session, cmd: &str, params: &Value) -> Result<bool> {
    let c = plugin::obtain(s, cmd, &Request::parse(cmd, params)?)?;
    let (_, info) = plugin::review(s, cmd, &c)?;
    if info["permissionsChanged"].as_bool().unwrap_or(true) {
        return Ok(false);
    }
    plugin::install(s, cmd, c).map(|_| true)
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "plugin.remove";
    let id = str_param(p, "id").ok_or_else(|| bad(cmd, "`id` required (see plugin.list)"))?.to_string();
    if !plugin::remove(s, cmd, &id)? {
        return Err(bad(cmd, format!("no plug-in `{id}` is installed")));
    }
    Ok(json!({"removed": id}))
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let id = str_param(p, "id").ok_or_else(|| bad("plugin.run", "`id` required (see plugin.list)"))?.to_string();
    let command = str_param(p, "command").map(str::to_string);
    let fuel = p.get("fuel").and_then(Value::as_u64);
    plugin::run(s, &id, command.as_deref(), p.get("params").unwrap_or(&Value::Null), fuel)
}

fn catalog_list(s: &mut Session, p: &Value) -> Result<Value> {
    let urls = match str_param(p, "url") {
        Some(url) => vec![url.to_string()],
        None => s.plugins.prefs.catalogs.clone(),
    };
    let (mut packages, mut warnings) = (vec![], vec![]);
    for url in &urls {
        match with_fetch(s, remote(url), |f| plugin::catalog_packages(s, f, url)) {
            Ok((found, w)) => {
                packages.extend(found);
                warnings.extend(w.into_iter().map(|w| format!("{url}: {w}")));
            }
            Err(e) => warnings.push(format!("{url}: {e}")),
        }
    }
    Ok(json!({"catalogs": urls, "packages": packages, "warnings": warnings}))
}

fn catalog_add(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "plugin.catalog.add";
    let url = str_param(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or_else(|| bad(cmd, "`url` required"))?.to_string();
    if bool_or(p, "remove", false) {
        s.plugins.prefs.catalogs.retain(|u| *u != url);
    } else if !s.plugins.prefs.catalogs.contains(&url) {
        plugin::check_catalog_url(&url).map_err(|e| bad(cmd, e))?;
        if s.plugins.prefs.catalogs.len() >= plugin::MAX_CATALOGS {
            return Err(bad(cmd, format!("at most {} catalogs", plugin::MAX_CATALOGS)));
        }
        s.plugins.prefs.catalogs.push(url);
    }
    s.plugins.save_prefs();
    Ok(json!({"catalogs": s.plugins.prefs.catalogs}))
}

/// None of these is an undo step by itself: `plugin.run` records one for the commands it runs.
/// Only `plugin.run` is journaled (an install's `data` can be megabytes).
pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "plugin.list", "Plug-ins", [], None, "{} → {plugins: [{id, name, version, kind, permissions, commands, source, sha256, stored}], problems, catalogs, stored (kept in the Plug-ins folder; else this session only), last}", always, list),
        cmd!(query "plugin.install", "Install Plug-in", [], None, "{path, review?, sha256?} | {data: base64 .craftplugin, name?, review?, sha256?} | {repo: owner/repo or owner/repo@tag (its release's .craftplugin, checked against SHA256SUMS), review?, sha256?} | {catalog: index.json URL, id, version? (default: the newest for LightCraft), review?, sha256?} — review: check and describe it without installing (the confirmation); sha256: install only that bundle (after a review, without downloading it again) → {id, name, version, kind, permissions, commands, source, sha256, inCatalog, replaces, permissionsChanged, stored, installed}", always, install),
        cmd!(query "plugin.update", "Update Plug-ins", [], None, "{id?, apply?: bool (install the updates that ask for no other permissions)} — checks each plug-in's catalog or repository → {updates: [{id, installed, available, install (plugin.install params), permissionsChanged?}], updated, errors}", always, update),
        cmd!(query "plugin.remove", "Remove Plug-in", [], None, "{id} — removes it and its settings → {removed}", always, remove),
        cmd!(
            "plugin.run",
            "Run Plug-in",
            [],
            None,
            "{id, command? (default: the first), params? (the command's parameters), fuel? (a lower instruction budget)} — runs the commands it calls as one undo step named after the plug-in, rolled back when it fails; it acts on the photos it names or the selection (`selection`, `active` in its input) → {plugin, command, output, notes, logs, calls, fuel, panel}",
            always,
            run
        ),
        cmd!(query "plugin.catalog.list", "Plug-in Catalogs", [], None, "{url?} — the packages LightCraft can run in that catalog, or in every catalog added → {catalogs, packages: [{catalog, id, name, version, author, description, license, repo, kind, sha256, installed}], warnings}", always, catalog_list),
        cmd!(query "plugin.catalog.add", "Add Plug-in Catalog", [], None, "{url: https:// on GitHub or file:// index.json, remove?: bool} → {catalogs}", always, catalog_add),
    ]
}
