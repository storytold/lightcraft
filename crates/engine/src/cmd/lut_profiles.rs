//! Imported profiles: `.cube` 3D LUTs used as creative profiles (Amount 0–200 % like the
//! built-in ones). A library on disk keeps a copy of each file in its `Profiles/` folder; the
//! list lives in the library's prefs and the LUTs are registered with the pipeline when the
//! library opens.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{Result, Session};

/// One imported LUT profile.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LutProfile {
    /// `lut:<slug>`.
    pub id: String,
    pub name: String,
    pub group: String,
    /// The library's copy of the `.cube` file (empty without a library on disk).
    pub file: String,
}

fn profiles_dir(s: &Session) -> Option<PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("Profiles"))
}

/// Register every imported profile's LUT with the pipeline (missing files are skipped).
pub fn register_all(s: &Session) {
    for p in &s.lut_profiles {
        if let Some(l) = std::fs::read_to_string(&p.file).ok().and_then(|t| lightcraft_pipeline::lut::Lut3d::parse_cube(&t).ok()) {
            lightcraft_pipeline::lut::register(&p.id, l);
        }
    }
}

/// `.cube` files in `paths` (folders searched, zip bundles opened), and the files that couldn't
/// be read with the reason (`[name, reason]`).
fn cube_files(paths: &[String]) -> (Vec<(String, String, Option<String>)>, Vec<Value>) {
    // (name, text, group)
    let mut out = Vec::new();
    let mut failed = Vec::new();
    let stem = |f: &Path| f.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            let cube = |f: &Path| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("cube"));
            for ep in crate::walk::files_in(path, None, crate::walk::Limits::default(), cube).files {
                match std::fs::read_to_string(&ep) {
                    Ok(t) => {
                        let group = ep.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string());
                        out.push((stem(&ep), t, group));
                    }
                    Err(e) => failed.push(json!([stem(&ep), e.to_string()])),
                }
            }
        } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")) {
            let b = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    failed.push(json!([stem(path), e.to_string()]));
                    continue;
                }
            };
            for (inner, data) in crate::preset_import::read_zip(&b).unwrap_or_default() {
                if inner.to_ascii_lowercase().ends_with(".cube") {
                    let ip = Path::new(&inner);
                    let group = ip
                        .parent()
                        .and_then(|d| d.file_name())
                        .map(|n| n.to_string_lossy().to_string())
                        .or_else(|| path.file_stem().map(|n| n.to_string_lossy().to_string()));
                    out.push((
                        ip.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        String::from_utf8_lossy(&data).to_string(),
                        group,
                    ));
                }
            }
        } else {
            match std::fs::read_to_string(path) {
                Ok(t) => out.push((stem(path), t, None)),
                Err(e) => failed.push(json!([stem(path), e.to_string()])),
            }
        }
    }
    (out, failed)
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.import";
    let paths: Vec<String> =
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if paths.is_empty() {
        return Err(bad(C, "no paths"));
    }
    let dir = profiles_dir(s);
    if let Some(d) = &dir {
        std::fs::create_dir_all(d).map_err(|e| bad(C, format!("{}: {e}", d.display())))?;
    }
    let (files, mut failed) = cube_files(&paths);
    let mut imported = Vec::new();
    for (name, text, group) in files {
        let lut = match lightcraft_pipeline::lut::Lut3d::parse_cube(&text) {
            Ok(l) => l,
            Err(e) => {
                failed.push(json!([name, e]));
                continue;
            }
        };
        let display = lut.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| name.clone());
        let group = super::super::preset_import::group_from_dir(&group.unwrap_or_default()).unwrap_or_else(|| "Imported".into());
        let slug = crate::presets::slug(&format!("{group}-{display}"));
        let mut id = format!("lut:{slug}");
        let mut n = 2;
        while s.lut_profiles.iter().any(|x| x.id == id) {
            id = format!("lut:{slug}-{n}");
            n += 1;
        }
        let file = match &dir {
            Some(d) => {
                let f = d.join(format!("{}.cube", id.trim_start_matches("lut:")));
                std::fs::write(&f, &text).map_err(|e| bad(C, format!("{}: {e}", f.display())))?;
                f.to_string_lossy().to_string()
            }
            // no library on disk: the registered LUT is all this session needs (a copy would only
            // be read back when a library opens, and that takes its own list from its prefs)
            None => String::new(),
        };
        lightcraft_pipeline::lut::register(&id, lut);
        s.lut_profiles.push(LutProfile { id: id.clone(), name: display.clone(), group: group.clone(), file });
        imported.push(json!({"id": id, "name": display, "group": group}));
    }
    s.save_prefs()?;
    // presets that name these profiles use them now
    crate::profile_refs::refresh(s);
    Ok(json!({"imported": imported, "failed": failed}))
}

/// `profile.exportLut`: a photo's look as a `.cube` file (see [`crate::lut_export`]).
fn export_lut(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.exportLut";
    let path = str_param(p, "path").filter(|x| !x.trim().is_empty()).ok_or_else(|| bad(C, "missing path"))?;
    // always a .cube: `look` and `My.Look` become `look.cube` and `My.Look.cube`, and another
    // extension is kept in the name rather than written over (`look.zip` → `look.zip.cube`), so
    // addProfile never reads cube text back as a zip bundle or a folder
    let path = match Path::new(path).extension() {
        Some(x) if x.eq_ignore_ascii_case("cube") => PathBuf::from(path),
        _ => PathBuf::from(format!("{path}.cube")),
    };
    let size = match p.get("size").filter(|v| !v.is_null()) {
        None => crate::lut_export::DEFAULT_SIZE,
        Some(v) => v
            .as_u64()
            .filter(|n| (2..=129).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| bad(C, "`size` must be an integer 2..129 (17, 33 or 65 are usual)"))?,
    };
    let id = match p.get("id").filter(|v| !v.is_null()) {
        None => s.active().ok_or_else(|| bad(C, "no photo (select one, or pass id)"))?,
        Some(v) => lightcraft_catalog::PhotoId(v.as_u64().ok_or_else(|| bad(C, "`id` must be a photo id"))?),
    };
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, format!("no photo {}", id.0)))?;
    let stem = |f: &str| Path::new(f).file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let title = str_param(p, "title").map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| stem(&path.to_string_lossy()));
    let settings = photo.develop.clone();
    let source = photo.file_name.clone();
    let (_, skipped) = crate::lut_export::lut_settings(&settings);
    let mut lut = crate::lut_export::bake(&settings, size).map_err(|e| bad(C, e))?;
    lut.title = Some(title.clone());
    let mut notes = vec![format!("Made with LightCraft from the look of {source}"), "Input and output: sRGB (display-encoded)".to_string()];
    if !skipped.is_empty() {
        notes.push(format!("Not included (a LUT can't hold them): {}", skipped.join(", ")));
    }
    let target = path.to_string_lossy().to_string();
    s.check_write_target(&target).map_err(|e| bad(C, e))?;
    crate::export::write_file_durable(&target, lut.to_cube(&notes).as_bytes()).map_err(crate::EngineError::Other)?;
    let mut out = json!({"path": target, "size": lut.size, "title": title, "skipped": skipped});
    // also a profile in this library, ready to use on other photos
    if p.get("addProfile").and_then(Value::as_bool).unwrap_or(false) {
        let r = import(s, &json!({"paths": [target]}))?;
        out["profile"] = r["imported"].get(0).cloned().unwrap_or(Value::Null);
    }
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "profile.import",
            "Import Profiles",
            [],
            None,
            "{paths: [.cube file, folder or .zip]} — 3D LUTs become creative profiles (Amount 0–200 %; rendered on the CPU) in the profile browser's groups (folder / zip name, else Imported) → {imported: [{id, name, group}], failed}",
            always,
            import
        ),
        cmd!(
            "profile.exportLut",
            "Export Look as LUT",
            [],
            None,
            "{path, id?: photo (default: the active one), size?: 2..129 entries an axis (default 33; 17 small, 65 finest), title?, addProfile?: also import it as a profile} — the photo's global colour and tone (profile, white balance, exposure, contrast, whites, blacks, curves, colour, mixer, point colour, B&W mix, grading, calibration) as a 3D LUT .cube file, sRGB in and out, for video editors and other apps; local settings (masks, highlights & shadows, texture / clarity / dehaze, sharpening, noise reduction, vignette, grain, crop, lens corrections…) can't be held by a LUT and are listed in `skipped` → {path, size, title, skipped: [names], profile?: {id, name, group}}",
            always,
            export_lut
        ),
        cmd!("profile.deleteImported", "Delete Imported Profile", [], None, "{id: lut:…}", always, |s, p| {
            let id = str_param(p, "id").ok_or_else(|| bad("profile.deleteImported", "missing id"))?.to_string();
            let Some(i) = s.lut_profiles.iter().position(|x| x.id == id) else {
                return Err(bad("profile.deleteImported", format!("no imported profile `{id}`")));
            };
            let gone = s.lut_profiles.remove(i);
            if profiles_dir(s).is_some_and(|d| Path::new(&gone.file).starts_with(d)) {
                let _ = std::fs::remove_file(&gone.file);
            }
            lightcraft_pipeline::lut::unregister(&id);
            s.save_prefs()?;
            crate::profile_refs::refresh(s);
            Ok(json!({"deleted": id}))
        }),
    ]
}
