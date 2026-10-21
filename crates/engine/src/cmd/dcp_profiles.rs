//! Imported `.dcp` camera profiles (personal fork): one active profile per camera model replaces
//! the colour model of that model's raws (`camera_profiles::apply_dcp`). Imported files are kept
//! in the camera-profiles folder's `dcp/` library; the active one for a model is copied next to
//! the JSON profiles. Like those, the active set is read when LightCraft starts.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::camera_profiles as cp;
use crate::{Result, Session};

/// `(name, bytes)` of every `.dcp` in `paths` (folders searched, zips opened), and failures.
fn dcp_files(paths: &[String]) -> (Vec<(String, Vec<u8>)>, Vec<Value>) {
    fn name(f: &Path) -> String {
        f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }
    fn is_dcp(f: &Path) -> bool {
        f.extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp"))
    }
    fn read(f: &Path, out: &mut Vec<(String, Vec<u8>)>, failed: &mut Vec<Value>) {
        match std::fs::metadata(f) {
            Ok(m) if m.len() > lightcraft_raw::dcp::MAX_FILE as u64 => failed.push(json!([name(f), "file too large for a camera profile"])),
            _ => match std::fs::read(f) {
                Ok(b) => out.push((name(f), b)),
                Err(e) => failed.push(json!([name(f), e.to_string()])),
            },
        }
    }
    let mut out = Vec::new();
    let mut failed = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            for f in crate::walk::files_in(path, None, crate::walk::Limits::default(), is_dcp).files {
                read(&f, &mut out, &mut failed);
            }
        } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")) {
            match std::fs::read(path) {
                Ok(b) => {
                    for (inner, data) in crate::preset_import::read_zip(&b).unwrap_or_default() {
                        if is_dcp(Path::new(&inner)) {
                            out.push((name(Path::new(&inner)), data));
                        }
                    }
                }
                Err(e) => failed.push(json!([name(path), e.to_string()])),
            }
        } else {
            read(path, &mut out, &mut failed);
        }
    }
    (out, failed)
}

fn dirs(c: &str) -> Result<(PathBuf, PathBuf)> {
    let active = cp::dir().ok_or_else(|| bad(c, "no camera-profiles folder (no configuration folder on this system)"))?;
    let library = cp::dcp_library_dir().ok_or_else(|| bad(c, "no camera-profiles folder"))?;
    std::fs::create_dir_all(&library).map_err(|e| bad(c, format!("{}: {e}", library.display())))?;
    Ok((active, library))
}

/// Copy `from` to `to` through a temporary file.
fn copy_atomic(c: &str, from: &Path, to: &Path) -> Result<()> {
    let tmp = to.with_extension("dcp.tmp");
    std::fs::copy(from, &tmp).map_err(|e| bad(c, format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, to).map_err(|e| bad(c, format!("{}: {e}", to.display())))?;
    Ok(())
}

/// Make the library file `file` the active profile for its camera.
fn activate(c: &str, active_dir: &Path, file: &Path) -> Result<lightcraft_raw::dcp::Dcp> {
    let dcp = cp::load_dcp(file).map_err(|e| bad(c, e))?;
    copy_atomic(c, file, &active_dir.join(cp::dcp_file_name(&dcp.model)))?;
    Ok(dcp)
}

fn import(_s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "camera.importDcp";
    let paths: Vec<String> =
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if paths.is_empty() {
        return Err(bad(C, "no paths"));
    }
    let activate_new = p.get("activate").and_then(Value::as_bool);
    let (active_dir, library) = dirs(C)?;
    let (files, mut failed) = dcp_files(&paths);
    let mut imported = Vec::new();
    let mut defaults_changed = false;
    for (name, bytes) in files {
        let dcp = match lightcraft_raw::dcp::parse(&bytes) {
            Ok(d) => d,
            Err(e) => {
                failed.push(json!([name, e]));
                continue;
            }
        };
        let label = if dcp.name.is_empty() { name.trim_end_matches(".dcp").to_string() } else { dcp.name.clone() };
        let slug = crate::presets::slug(&format!("{}-{label}", dcp.model));
        let file = library.join(format!("{slug}.dcp"));
        let tmp = file.with_extension("dcp.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| bad(C, format!("{}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &file).map_err(|e| bad(C, format!("{}: {e}", file.display())))?;
        // the first profile for a camera becomes its default; the others are chosen per photo
        let make_default = activate_new.unwrap_or_else(|| !active_dir.join(cp::dcp_file_name(&dcp.model)).is_file());
        if make_default {
            activate(C, &active_dir, &file)?;
        }
        imported
            .push(json!({"id": format!("dcp:{slug}"), "model": dcp.model, "name": label, "file": file.to_string_lossy(), "active": make_default}));
        defaults_changed |= make_default;
    }
    // imported profiles can be chosen per photo straight away (the camera's default changes on the next start)
    lightcraft_raw::dcp::install_library(cp::library_dcps());
    Ok(json!({"imported": imported, "failed": failed, "restart": defaults_changed}))
}

/// The library, each entry with whether it is the active profile for its model.
fn list(_s: &mut Session, _p: &Value) -> Result<Value> {
    let active: Vec<(PathBuf, lightcraft_raw::dcp::Dcp)> = cp::active_dcps();
    let same = |a: &Path, b: &Path| std::fs::read(a).ok().zip(std::fs::read(b).ok()).is_some_and(|(x, y)| x == y);
    let mut out = Vec::new();
    if let Some(lib) = cp::dcp_library_dir()
        && let Ok(entries) = std::fs::read_dir(&lib)
    {
        let mut files: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|f| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp"))).collect();
        files.sort();
        for f in files {
            match cp::load_dcp(&f) {
                Ok(d) => {
                    let is_active = active.iter().any(|(a, ad)| ad.model == d.model && same(a, &f));
                    let id = format!("dcp:{}", f.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                    out.push(json!({"id": id, "model": d.model, "name": d.name, "copyright": d.copyright, "file": f.to_string_lossy(), "active": is_active}));
                }
                Err(e) => out.push(json!({"file": f.to_string_lossy(), "error": e})),
            }
        }
    }
    let in_use: Vec<Value> = lightcraft_raw::dcp::installed().iter().map(|d| json!({"model": d.model, "name": d.name})).collect();
    Ok(json!({"profiles": out, "inUse": in_use}))
}

fn use_dcp(_s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "camera.useDcp";
    let file = str_param(p, "file").ok_or_else(|| bad(C, "missing file"))?;
    let (active_dir, library) = dirs(C)?;
    let path = PathBuf::from(file);
    // only files in the library (what `camera.listDcp` names)
    if path.parent().map(Path::to_path_buf) != Some(library.clone()) {
        return Err(bad(C, format!("`{file}` is not an imported camera profile")));
    }
    let dcp = activate(C, &active_dir, &path)?;
    Ok(json!({"model": dcp.model, "name": dcp.name, "restart": true}))
}

fn remove_dcp(_s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "camera.removeDcp";
    let model = str_param(p, "model").ok_or_else(|| bad(C, "missing model"))?;
    let dir = cp::dir().ok_or_else(|| bad(C, "no camera-profiles folder"))?;
    let f = dir.join(cp::dcp_file_name(model));
    if !f.is_file() {
        return Err(bad(C, format!("no active camera profile for `{model}`")));
    }
    std::fs::remove_file(&f).map_err(|e| bad(C, format!("{}: {e}", f.display())))?;
    Ok(json!({"model": model, "restart": true}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "camera.importDcp",
            "Import Camera Profiles (.dcp)",
            [],
            None,
            "{paths: [.dcp file, folder or .zip], activate?: bool} — DNG camera profiles you own, chosen per photo (develop.cameraProfile); the first for a camera model (or each, with activate: true) also becomes that camera's default from the next start → {imported: [{id, model, name, file, active}], failed, restart}",
            always,
            import
        ),
        cmd!(query "camera.listDcp", "Camera Profiles (.dcp)", [], None, "{} → {profiles: [{id, model, name, copyright, file, active}], inUse: [{model, name}]}", always, list),
        cmd!(
            "camera.useDcp",
            "Use Camera Profile",
            [],
            None,
            "{file: library file from camera.listDcp} — active for its model from the next start",
            always,
            use_dcp
        ),
        cmd!(
            "camera.removeDcp",
            "Stop Using Camera Profile",
            [],
            None,
            "{model} — the model's raws go back to LightCraft's own colour from the next start",
            always,
            remove_dcp
        ),
    ]
}
