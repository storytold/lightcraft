//! Imported camera profiles: the user's own DNG Camera Profiles (`.dcp`), chosen like any other
//! profile (`develop.profile {id: "dcp:…"}`). Unlike the built-in looks and LUT profiles, which
//! work on developed colour, a camera profile replaces the raw file's colour model when it is
//! decoded (its matrices and hue/saturation map; see [`lightcraft_raw::dcp`]), so the photo's
//! source is decoded again when it changes. Other photos (JPEG, TIFF…) ignore it.
//!
//! A library on disk keeps a copy of each file in its `Profiles/` folder; the list lives in the
//! library's prefs and the profiles are parsed again when the library opens.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightcraft_raw::dcp::DcpProfile;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{Result, Session};

/// Profile id prefix of imported camera profiles.
pub const DCP_PREFIX: &str = "dcp:";
/// Profile browser group of imported camera profiles.
pub const DCP_GROUP: &str = "Camera Profiles";

/// One imported camera profile.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DcpEntry {
    /// `dcp:<slug>`.
    pub id: String,
    /// `ProfileName`, else the file name.
    pub name: String,
    /// The camera it was made for (`UniqueCameraModel`).
    #[serde(default)]
    pub camera_model: String,
    /// The `.dcp` file (the library's copy when there is a library on disk).
    pub file: String,
}

/// Is `id` an imported camera profile id?
pub fn is_dcp_id(id: &str) -> bool {
    id.starts_with(DCP_PREFIX)
}

fn profiles_dir(s: &Session) -> Option<PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("Profiles"))
}

/// Parse every imported camera profile again and hand them to the decoder (missing or damaged
/// files are skipped: photos using them develop with the file's own colour).
pub fn register_all(s: &mut Session) {
    let loaded: Vec<(String, Arc<DcpProfile>)> = s
        .dcp_profiles
        .iter()
        .filter_map(|p| std::fs::read(&p.file).ok().and_then(|b| lightcraft_raw::dcp::parse_dcp(&b).ok()).map(|d| (p.id.clone(), Arc::new(d))))
        .collect();
    s.media.camera_profiles = loaded.into_iter().collect();
}

/// `.dcp` files under `paths` (folders searched).
pub fn dcp_files(paths: &[String]) -> Vec<PathBuf> {
    let is_dcp = |f: &Path| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp"));
    let mut out = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            out.extend(crate::walk::files_in(path, None, crate::walk::Limits::default(), is_dcp).files);
        } else if is_dcp(path) {
            out.push(path.to_path_buf());
        }
    }
    out
}

/// Import the `.dcp` files under `paths` → (imported `[{id, name, cameraModel}]`, failed `[[file, error]]`).
/// A profile already imported (same name, camera and bytes) is not added twice.
pub fn import_files(s: &mut Session, paths: &[String]) -> Result<(Vec<Value>, Vec<Value>)> {
    const C: &str = "profile.import";
    let dir = profiles_dir(s);
    if let Some(d) = &dir {
        std::fs::create_dir_all(d).map_err(|e| bad(C, format!("{}: {e}", d.display())))?;
    }
    let (mut imported, mut failed) = (Vec::new(), Vec::new());
    for f in dcp_files(paths) {
        let shown = f.display().to_string();
        let bytes = match std::fs::read(&f) {
            Ok(b) => b,
            Err(e) => {
                failed.push(json!([shown, e.to_string()]));
                continue;
            }
        };
        let profile = match lightcraft_raw::dcp::parse_dcp(&bytes) {
            Ok(p) => p,
            Err(e) => {
                failed.push(json!([shown, e.to_string()]));
                continue;
            }
        };
        let stem = f.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Camera Profile".into());
        let name = if profile.name.trim().is_empty() { stem.clone() } else { profile.name.trim().to_string() };
        // the same profile imported again: keep the one there is
        let same = s
            .dcp_profiles
            .iter()
            .find(|p| p.name == name && p.camera_model == profile.camera_model && std::fs::read(&p.file).is_ok_and(|b| b == bytes));
        if let Some(p) = same {
            imported.push(json!({"id": p.id, "name": p.name, "cameraModel": p.camera_model, "existing": true}));
            continue;
        }
        // (the file name keeps profiles with the same ProfileName apart, e.g. per-body copies)
        let slug = crate::presets::slug(&format!("{}-{stem}", profile.camera_model));
        let mut id = format!("{DCP_PREFIX}{slug}");
        let mut n = 2;
        while s.dcp_profiles.iter().any(|x| x.id == id) {
            id = format!("{DCP_PREFIX}{slug}-{n}");
            n += 1;
        }
        let file = match &dir {
            Some(d) => {
                let to = d.join(format!("{}.dcp", id.trim_start_matches(DCP_PREFIX)));
                std::fs::write(&to, &bytes).map_err(|e| bad(C, format!("{}: {e}", to.display())))?;
                to.to_string_lossy().to_string()
            }
            // no library on disk: remember the original (the in-memory session ends with the app)
            None => f.to_string_lossy().to_string(),
        };
        s.media.camera_profiles.insert(id.clone(), Arc::new(profile.clone()));
        s.dcp_profiles.push(DcpEntry { id: id.clone(), name: name.clone(), camera_model: profile.camera_model.clone(), file });
        imported.push(json!({"id": id, "name": name, "cameraModel": profile.camera_model}));
    }
    Ok((imported, failed))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.deleteCamera";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing id"))?.to_string();
    let Some(i) = s.dcp_profiles.iter().position(|x| x.id == id) else {
        return Err(bad(C, format!("no imported camera profile `{id}`")));
    };
    let entry = s.dcp_profiles.remove(i);
    s.media.camera_profiles.remove(&id);
    // the library's copy goes; a file outside the library is the user's and stays
    if let Some(d) = profiles_dir(s)
        && Path::new(&entry.file).starts_with(&d)
    {
        let _ = std::fs::remove_file(&entry.file);
    }
    s.profile_favorites.retain(|f| f != &id);
    s.profile_recent.retain(|f| f != &id);
    s.save_prefs()?;
    Ok(json!({"deleted": id}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "profiles.camera",
            "Imported Camera Profiles",
            [],
            None,
            "{} → {profiles: [{id, name, cameraModel, file}]}: the imported .dcp camera profiles (use with develop.profile)",
            always,
            |s, _| Ok(json!({"profiles": s.dcp_profiles}))
        ),
        cmd!(
            "profile.deleteCamera",
            "Delete Imported Camera Profile",
            [],
            None,
            "{id: dcp:…} — photos using it develop with their file's own colour again",
            always,
            delete
        ),
    ]
}
