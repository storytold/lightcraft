//! XMP sidecar commands: save/read metadata to/from files, sidecar preferences.

use lightcraft_catalog::Op;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_selection, str_param};
use crate::sidecar::SidecarNaming;
use crate::{Result, Session};

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    let mut written = Vec::new();
    let mut merged = Vec::new();
    let mut backups = Vec::new();
    let mut failed = Vec::new();
    let ids = s.targets(p)?;
    let owners = crate::sidecar::StemOwners::among(&s.catalog, &ids);
    for id in ids {
        match s.save_sidecar_with(id, &owners) {
            Ok(r) => {
                let path = r.path.display().to_string();
                if r.merged {
                    merged.push(path.clone());
                }
                if let Some(b) = r.backup {
                    backups.push(json!({"path": path, "backup": b.display().to_string()}));
                }
                written.push(path);
            }
            Err(e) => failed.push(json!({"id": id.0, "error": e.to_string()})),
        }
    }
    Ok(json!({"written": written, "merged": merged, "backups": backups, "failed": failed}))
}

/// Whether applying `op` (from [`Session::read_sidecar_op_with_warnings`]) would change its photo.
fn changes_photo(s: &Session, op: &Op) -> bool {
    let differs = |id: &lightcraft_catalog::PhotoId, f: &dyn Fn(&lightcraft_catalog::Photo) -> bool| s.catalog.photo(*id).is_some_and(|p| f(p));
    match op {
        Op::Batch { ops } => ops.iter().any(|o| changes_photo(s, o)),
        Op::SetRating { id, rating } => differs(id, &|p| p.rating != *rating),
        Op::SetFlag { id, flag } => differs(id, &|p| p.flag != *flag),
        Op::SetLabel { id, label } => differs(id, &|p| p.label != *label),
        Op::SetMeta { id, meta } => differs(id, &|p| p.meta != **meta),
        Op::SetCaptured { id, captured } => differs(id, &|p| p.captured != *captured),
        Op::SetDevelop { id, settings, .. } => differs(id, &|p| *p.develop != **settings),
        // the history step only comes with a changed `SetDevelop`
        Op::PushHistory { .. } => false,
        // an op this doesn't know yet counts as a change, so nothing a sidecar says is skipped
        _ => true,
    }
}

fn read(s: &mut Session, p: &Value) -> Result<Value> {
    // `onlyChanged` is for a caller that re-reads sidecars other apps may have touched: photos whose
    // sidecar says what the catalog already has get no op, so no undo step and no log record.
    let only_changed = p.get("onlyChanged").and_then(Value::as_bool).unwrap_or(false);
    let mut ops = Vec::new();
    let mut read = Vec::new();
    let mut unchanged = Vec::new();
    let mut failed = Vec::new();
    let mut warnings = Vec::new();
    for id in s.targets(p)? {
        match s.read_sidecar_op_with_warnings(id) {
            Ok(Some((op, from, mut photo_warnings))) => {
                if only_changed && !changes_photo(s, &op) {
                    unchanged.push(json!({"id": id.0, "from": from.display().to_string()}));
                    continue;
                }
                ops.push(op);
                read.push(json!({"id": id.0, "from": from.display().to_string()}));
                warnings.append(&mut photo_warnings);
            }
            Ok(None) => failed.push(json!({"id": id.0, "error": "no XMP sidecar or embedded XMP"})),
            Err(e) => failed.push(json!({"id": id.0, "error": e.to_string()})),
        }
    }
    if !ops.is_empty() {
        s.commit("Read Metadata from File", Op::Batch { ops })?;
        // Don't auto-write the sidecars just read: that would add our `lc:settings` to one that had
        // only another app's `crs:` settings, and later reads prefer `lc:settings`, so that app's
        // next edits would be ignored (issue #934). The next edit, or Save Metadata, writes them.
        s.skip_auto_write = true;
    }
    let mut result = json!({"read": read, "failed": failed});
    if only_changed {
        result["unchanged"] = json!(unchanged);
    }
    if !warnings.is_empty() {
        result["warnings"] = serde_json::to_value(warnings).unwrap_or_default();
    }
    Ok(result)
}

fn prefs(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(a) = p.get("autoWrite").and_then(Value::as_bool) {
        s.xmp.auto_write = a;
    }
    if let Some(n) = str_param(p, "naming") {
        s.xmp.naming = match n {
            "stem" => SidecarNaming::Stem,
            "full" => SidecarNaming::Full,
            other => return Err(bad("library.xmpPreferences", format!("unknown naming `{other}` (stem|full)"))),
        };
    }
    if p.as_object().is_some_and(|o| !o.is_empty()) {
        s.save_prefs()?;
    }
    Ok(serde_json::to_value(s.xmp).unwrap_or_default())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "photo.saveMetadataToFile",
            "Save Metadata to File",
            ["Photo"],
            Some("Cmd+S"),
            "{ids?} — writes each photo's XMP sidecar (metadata + develop settings) next to the original, merged into an existing one (other apps' data kept; an unreadable one is backed up first) → {written: [path], merged: [path], backups: [{path, backup}], failed}",
            has_selection,
            save
        ),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.readMetadataFromFile",
                "Read Metadata from File",
                ["Photo"],
                None,
                "{ids?, onlyChanged?: bool} — reads each photo's XMP sidecar, or embedded XMP in raw/DNG and JPEG files when no sidecar exists: metadata and develop settings, one undo step; never writes the sidecar, even with auto-write on; `onlyChanged: true` skips photos whose sidecar changes nothing (no undo step or log record for them) and lists them in `unchanged`; with explicit `ids` it needs no selection; JPEG uses the first complete standard XMP APP1 before SOS/EOI, within 16 MiB of headers and 4096 marker/fill steps; an unreadable sidecar fails without embedded fallback → {read, failed, unchanged?: [{id, from}], warnings?: [{path, code, profile?, mode?, fields?}]}",
                has_selection,
                read
            )
        },
        cmd!(
            "library.xmpPreferences",
            "XMP Sidecar Preferences",
            [],
            None,
            "{autoWrite?: bool, naming?: stem (IMG_1.xmp, default) | full (IMG_1.CR3.xmp)} → current preferences (saved with the library)",
            always,
            prefs
        ),
        cmd!(
            "library.retrySidecars",
            "Write Waiting XMP Sidecars",
            [],
            None,
            "{} — writes again the automatic XMP sidecars that failed (folder offline, read-only or full) whose originals are reachable now → {written, waiting}",
            always,
            |s, _| {
                let written = s.retry_unsaved_sidecars();
                Ok(json!({"written": written, "waiting": s.unsaved_sidecars()}))
            }
        ),
        cmd!("library.toggleAutoWriteXmp", "Automatically Write Changes into XMP", ["File"], None, "{} → {autoWrite}", always, |s, _| {
            let on = !s.xmp.auto_write;
            prefs(s, &json!({"autoWrite": on}))
        }),
    ]
}
