//! Catalog-level commands: settings, backup, integrity test, optimise, Export as Catalog and
//! Import from Another Catalog (`docs/catalog.md`).

use std::path::{Path, PathBuf};

use dac_catalog::library::{BackupSchedule, CatalogSettings};
use dac_catalog::transfer::{self, ConflictRule, ExportOptions};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

fn on_disk(s: &Session) -> std::result::Result<(), String> {
    s.catalog_dir().map(|_| ()).ok_or_else(|| "no catalog on disk is open".into())
}

fn schedule_name(b: BackupSchedule) -> &'static str {
    match b {
        BackupSchedule::Never => "never",
        BackupSchedule::EveryExit => "everyExit",
        BackupSchedule::Daily => "daily",
        BackupSchedule::Weekly => "weekly",
        BackupSchedule::Monthly => "monthly",
    }
}

pub fn parse_schedule(s: &str) -> Option<BackupSchedule> {
    Some(match s {
        "never" => BackupSchedule::Never,
        "everyExit" | "exit" => BackupSchedule::EveryExit,
        "daily" => BackupSchedule::Daily,
        "weekly" => BackupSchedule::Weekly,
        "monthly" => BackupSchedule::Monthly,
        _ => return None,
    })
}

fn settings_json(s: &Session, c: &CatalogSettings) -> Value {
    let dir = s.catalog_dir().map(Path::to_path_buf).unwrap_or_default();
    json!({
        "backup": schedule_name(c.backup),
        "backupDir": c.backup_root(&dir).display().to_string(),
        "customBackupDir": c.backup_dir.is_some(),
        "defaultBackupDir": dir.join(dac_catalog::library::BACKUPS_DIR).display().to_string(),
        "keepBackups": c.keep_backups,
        "lastBackup": c.last_backup,
        "testIntegrity": c.test_integrity,
        "optimize": c.optimize,
        "previews": s.preview_prefs.json(),
        "backupDue": c.backup_due(dac_catalog::library::now_secs()),
    })
}

fn info(s: &mut Session, _: &Value) -> Result<Value> {
    let dir = s.catalog_dir().map(Path::to_path_buf);
    let entry = dir.as_deref().and_then(dac_catalog::library::find_entry);
    let c = s.catalog_settings();
    Ok(json!({
        "dir": dir.map(|d| d.display().to_string()),
        "entry": entry.map(|e| e.display().to_string()),
        "photos": s.catalog.len(),
        "onDisk": s.catalog_dir().is_some(),
        "settings": settings_json(s, &c),
    }))
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "catalog.settings";
    let mut c = s.catalog_settings();
    let before = c.clone();
    if let Some(b) = str_param(p, "backup") {
        c.backup = parse_schedule(b).ok_or_else(|| bad(C, format!("unknown backup schedule `{b}` (never|everyExit|daily|weekly|monthly)")))?;
    }
    if let Some(d) = p.get("backupDir") {
        c.backup_dir = d.as_str().map(str::trim).filter(|d| !d.is_empty()).map(PathBuf::from);
    }
    if let Some(n) = p.get("keepBackups").and_then(Value::as_u64) {
        c.keep_backups = usize::try_from(n.min(10_000)).unwrap_or(10_000);
    }
    if let Some(b) = p.get("testIntegrity").and_then(Value::as_bool) {
        c.test_integrity = b;
    }
    if let Some(b) = p.get("optimize").and_then(Value::as_bool) {
        c.optimize = b;
    }
    if c != before {
        s.set_catalog_settings(&c)?;
    }
    if let Some(prev) = p.get("previews").filter(|v| v.is_object()) {
        s.execute("library.previewSettings", prev)?;
    }
    let c = s.catalog_settings();
    Ok(settings_json(s, &c))
}

fn backup(s: &mut Session, p: &Value) -> Result<Value> {
    let root = str_param(p, "dir").map(str::trim).filter(|d| !d.is_empty()).map(PathBuf::from);
    let out = s.backup_catalog(root.as_deref())?;
    Ok(json!({"backup": out.display().to_string()}))
}

fn backup_if_due(s: &mut Session, _: &Value) -> Result<Value> {
    let out = s.backup_catalog_if_due()?;
    Ok(json!({"backup": out.map(|o| o.display().to_string())}))
}

fn check(s: &mut Session, _: &Value) -> Result<Value> {
    let r = s.check_catalog_integrity()?;
    let ok = r.ok();
    let mut v = serde_json::to_value(r).unwrap_or_default();
    v["ok"] = json!(ok);
    Ok(v)
}

fn optimize(s: &mut Session, _: &Value) -> Result<Value> {
    let r = s.optimize_catalog()?;
    Ok(serde_json::to_value(r).unwrap_or_default())
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "catalog.export";
    let parent = str_param(p, "parent")
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .ok_or_else(|| bad(C, "missing `parent` (the folder to create the catalog in)"))?;
    let name = str_param(p, "name").map(str::trim).filter(|d| !d.is_empty()).ok_or_else(|| bad(C, "missing `name`"))?;
    let photos = match str_param(p, "scope") {
        Some("all") => s.catalog.photos().filter(|p| p.in_library()).map(|p| p.id).collect(),
        Some("visible") => s.visible_cloned(),
        _ => s.targets(p),
    };
    if photos.is_empty() {
        return Err(bad(C, "no photos to export"));
    }
    let opts = ExportOptions { photos, include_originals: bool_or(p, "originals", false), include_previews: bool_or(p, "previews", false) };
    let r = transfer::export_catalog(&s.catalog, &opts, Path::new(parent), name)?;
    Ok(serde_json::to_value(r).unwrap_or_default())
}

fn rule_param(p: &Value) -> Result<ConflictRule> {
    match str_param(p, "rule") {
        None => Ok(ConflictRule::Keep),
        Some(r) => serde_json::from_value(json!(r))
            .map_err(|_| bad("catalog.import", format!("unknown rule `{r}` (keep|replaceSettings|replaceMetadata|replaceSettingsAndMetadata)"))),
    }
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "catalog.import";
    let path = str_param(p, "path").map(str::trim).filter(|d| !d.is_empty()).ok_or_else(|| bad(C, "missing `path` (the other catalog)"))?;
    let rule = rule_param(p)?;
    if let Some(dir) = s.catalog_dir()
        && dac_catalog::library::resolve(Path::new(path)).is_ok_and(|o| same(&o, dir))
    {
        return Err(bad(C, "that is the catalog that is open"));
    }
    let other = transfer::load_readonly(Path::new(path))?;
    let plan = transfer::plan_import(&s.catalog, &other);
    let mut out = serde_json::to_value(&plan).unwrap_or_default();
    out["changedSettings"] = json!(plan.changed.iter().filter(|c| c.settings_differ).count());
    out["changedMetadata"] = json!(plan.changed.iter().filter(|c| c.metadata_differ).count());
    if bool_or(p, "preview", false) {
        return Ok(out);
    }
    if plan.new_photos.is_empty()
        && plan.new_albums.is_empty()
        && plan.extended_albums.is_empty()
        && (plan.changed.is_empty() || rule == ConflictRule::Keep)
    {
        out["imported"] = json!(false);
        return Ok(out);
    }
    let op = plan.ops(&mut s.catalog, &other, rule);
    s.commit("Import from Catalog", op)?;
    out["imported"] = json!(true);
    Ok(out)
}

fn same(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "catalog.info", "Catalog Info", [], None, "{} → {dir, entry, photos, onDisk, settings}", always, info),
        cmd!(
            "catalog.settings",
            "Catalog Settings",
            [],
            None,
            "{backup?: never|everyExit|daily|weekly|monthly, backupDir?: path (\"\" = backups/ in the catalog), keepBackups?: n (0 = all), testIntegrity?: bool, optimize?: bool, previews?: library.previewSettings params} → the settings (saved in the catalog's catalog-settings.json)",
            on_disk,
            settings
        ),
        cmd!(query "catalog.backup", "Back Up Catalog", ["File"], None, "{dir?} — checkpoint, copy the catalog into <dir or backup folder>/<time>/, test the copy; old backups pruned → {backup}", on_disk, backup),
        cmd!(query "catalog.backupIfDue", "Back Up Catalog If Due", [], None, "{} — the exit-time backup: only when the schedule says so → {backup: path|null}", always, backup_if_due),
        cmd!(query "catalog.checkIntegrity", "Test Catalog Integrity", ["File"], None, "{} → {ok, storeOk, storedPhotos, photos, danglingRefs}", on_disk, check),
        cmd!(query "catalog.optimize", "Optimize Catalog", ["File"], None, "{} — rewrite the store, rebuild indexes, compact → {bytesBefore, bytesAfter, ms}", on_disk, optimize),
        cmd!(
            query "catalog.export",
            "Export as Catalog",
            [],
            None,
            "{parent, name, ids? | scope?: selected|visible|all, originals?: bool, previews?: bool} — a new catalog folder parent/name with these photos, their albums and stacks; originals copied into its Originals/ → {entry, photos, albums, stacks, originalsCopied, originalsFailed}",
            always,
            export
        ),
        cmd!(
            "catalog.import",
            "Import from Another Catalog",
            [],
            None,
            "{path, rule?: keep|replaceSettings|replaceMetadata|replaceSettingsAndMetadata, preview?: bool} — photos, albums and stacks from another catalog (read-only), one undo step; with preview only the change preview → {newPhotos, changed, unchanged, newAlbums, extendedAlbums, changedSettings, changedMetadata, imported}",
            always,
            import
        ),
    ]
}
