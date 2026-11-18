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

/// Where imported profile files are kept: the app store's `Profiles/`, else the library's.
fn profiles_dir(s: &Session) -> Option<PathBuf> {
    s.profiles_dir()
}

/// Largest `.cube` file read back from a library: a 129³ LUT, the largest size in common use, is
/// about 60 MB of text.
const MAX_CUBE_BYTES: u64 = 64 << 20;

/// The library's own copy of `p`: `<library>/Profiles/<its file name>`, a `.cube` that is a regular
/// file (not a link, a FIFO or a device). `prefs.json` is a file anyone with the library folder can
/// edit, so the path it records is never followed as written: not `..` out of `Profiles/`, an
/// absolute path elsewhere, `/dev/zero` or a network share (issue #2123). Only the name counts, so
/// a library moved since keeps its profiles.
fn own_copy(profiles: &Path, p: &LutProfile) -> Option<PathBuf> {
    let name = Path::new(&p.file).file_name()?;
    if !Path::new(name).extension().is_some_and(|x| x.eq_ignore_ascii_case("cube")) {
        return None;
    }
    let f = profiles.join(name);
    std::fs::symlink_metadata(&f).ok().filter(std::fs::Metadata::is_file).map(|_| f)
}

/// A `.cube` or look file's text, for an import: a regular file of at most [`MAX_CUBE_BYTES`].
fn read_text(f: &Path) -> std::result::Result<String, String> {
    let b = crate::bounded_read::read_bounded(f, MAX_CUBE_BYTES).map_err(|e| e.to_string())?;
    String::from_utf8(b).map_err(|e| e.to_string())
}

/// `f` as text, when it is at most [`MAX_CUBE_BYTES`].
fn read_cube(f: &Path) -> Option<String> {
    use std::io::Read;
    let mut b = Vec::new();
    std::fs::File::open(f).ok()?.take(MAX_CUBE_BYTES + 1).read_to_end(&mut b).ok()?;
    if b.len() as u64 > MAX_CUBE_BYTES {
        return None;
    }
    String::from_utf8(b).ok()
}

/// Register every imported profile's LUT with the pipeline, from the copies in `profiles` (the
/// `Profiles/` folder of the library being opened, which isn't `s.library` yet; `None` when it
/// isn't on disk) ([`own_copy`]); missing, oversized or unreadable files are skipped.
pub fn register_all(s: &Session, profiles: Option<&Path>) {
    let Some(profiles) = profiles else { return };
    register_profiles(profiles, &s.lut_profiles, MAX_REGISTERED_LUT_BYTES);
}

/// Most bytes of table data held for one library's profiles (about ten 256-point tables).
const MAX_REGISTERED_LUT_BYTES: usize = 2 << 30;

/// Register `list` from the copies in `profiles`: profiles naming one copy share one parsed table, and
/// tables are registered while their distinct data fits `budget` bytes (a profile past it is skipped).
fn register_profiles(profiles: &Path, list: &[LutProfile], budget: usize) {
    use std::collections::HashMap;
    type Table = Option<std::sync::Arc<lcc_pipeline::lut::Lut3d>>;
    let mut by_file: HashMap<PathBuf, Table> = HashMap::new();
    let mut held = 0usize;
    for p in list {
        let Some(f) = own_copy(profiles, p) else { continue };
        let lut = by_file.entry(f.clone()).or_insert_with(|| {
            let l = read_cube(&f).and_then(|t| lcc_pipeline::lut::Lut3d::parse_cube(&t).ok())?;
            let bytes = l.data.len().saturating_mul(12);
            if held.saturating_add(bytes) > budget {
                log::warn!("LUT profile {}: the profiles' tables together pass {} MiB; this one is not loaded", p.id, budget >> 20);
                return None;
            }
            held += bytes;
            Some(std::sync::Arc::new(l))
        });
        if let Some(l) = lut {
            lcc_pipeline::lut::register_shared(&p.id, l.clone());
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
    // an XMP look with a colour table, as `.cube` text (its own name and group when it has them)
    let look = |name: String, text: &str, group: Option<String>, out: &mut Vec<(String, String, Option<String>)>, failed: &mut Vec<Value>| {
        match crate::xmp_look::parse(text, &name) {
            Ok(l) => out.push((l.name, l.lut.to_cube(), l.group.or(group))),
            Err(e) => failed.push(json!([name, e])),
        }
    };
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            let cube = |f: &Path| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("cube")) || crate::xmp_look::is_look_file(f);
            for ep in crate::walk::files_in(path, None, crate::walk::Limits::default(), cube).files {
                match read_text(&ep) {
                    Ok(t) => {
                        let group = ep.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string());
                        if ep.extension().is_some_and(|x| x.eq_ignore_ascii_case("xmp")) {
                            look(stem(&ep), &t, group, &mut out, &mut failed);
                        } else {
                            out.push((stem(&ep), t, group));
                        }
                    }
                    Err(e) => failed.push(json!([stem(&ep), e.to_string()])),
                }
            }
        } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")) {
            let b = match crate::bounded_read::read_bounded(path, crate::preset_import::MAX_ZIP_TOTAL as u64).map_err(|e| e.to_string()) {
                Ok(b) => b,
                Err(e) => {
                    failed.push(json!([stem(path), e.to_string()]));
                    continue;
                }
            };
            for (inner, data) in crate::preset_import::read_zip(&b).unwrap_or_default() {
                let text = String::from_utf8_lossy(&data);
                if inner.to_ascii_lowercase().ends_with(".xmp") && crate::xmp_look::is_look(&text) {
                    let ip = Path::new(&inner);
                    let group = ip.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string());
                    look(stem(ip), &text, group, &mut out, &mut failed);
                } else if inner.to_ascii_lowercase().ends_with(".cube") {
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
            let ext = path.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            // a camera profile is imported by `camera.importDcp` (see `import`): only .cube files are
            // read as LUTs, so a .dcp is not also reported as an unreadable LUT (OneSlips)
            if ext == "dcp" {
                continue;
            }
            match read_text(path) {
                Ok(t) if ext == "xmp" => look(stem(path), &t, None, &mut out, &mut failed),
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
    // the ids taken and the next suffix per slug: a bundle of same-titled LUTs doesn't probe the
    // whole list from 2 for each one
    let mut taken: std::collections::HashSet<String> = s.lut_profiles.iter().map(|x| x.id.clone()).collect();
    let mut next_suffix: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for (name, text, group) in files {
        let lut = match lcc_pipeline::lut::Lut3d::parse_cube(&text) {
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
        let n = next_suffix.entry(slug.clone()).or_insert(2);
        while taken.contains(&id) {
            id = format!("lut:{slug}-{n}");
            *n = n.saturating_add(1);
        }
        taken.insert(id.clone());
        let file = match &dir {
            Some(d) => {
                let f = d.join(format!("{}.cube", id.trim_start_matches("lut:")));
                // replaced, never written through a link at the name (issue #2122)
                lcc_catalog::safe_file::write_atomic(&f, text.as_bytes()).map_err(|e| bad(C, format!("{}: {e}", f.display())))?;
                f.to_string_lossy().to_string()
            }
            // no library on disk: the registered LUT is all this session needs (a copy would only
            // be read back when a library opens, and that takes its own list from its prefs)
            None => String::new(),
        };
        lcc_pipeline::lut::register(&id, lut);
        s.lut_profiles.push(LutProfile { id: id.clone(), name: display.clone(), group: group.clone(), file });
        imported.push(json!({"id": id, "name": display, "group": group}));
    }
    s.save_prefs()?;
    // presets that name these profiles use them now
    crate::profile_refs::refresh(s);
    // the user's own DNG camera profiles (.dcp) among the paths go to the camera-profile library
    // (OneSlips: File ▸ Import Profiles and drag and drop take them too)
    let dcps: Vec<&String> = paths.iter().filter(|p| Path::new(p).extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp"))).collect();
    let mut camera = Vec::new();
    if !dcps.is_empty() {
        match super::dcp_profiles::import(s, &json!({"paths": dcps})) {
            Ok(r) => {
                camera = r.get("imported").and_then(Value::as_array).cloned().unwrap_or_default();
                failed.extend(r.get("failed").and_then(Value::as_array).cloned().unwrap_or_default());
            }
            Err(e) => failed.extend(dcps.iter().map(|p| json!([p, e.to_string()]))),
        }
    }
    Ok(json!({"imported": imported, "camera": camera, "failed": failed}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "profile.import",
            "Import Profiles",
            [],
            None,
            "{paths: [.cube file, XMP look (.xmp with crs:RGBTable), .dcp camera profile, folder or .zip]}: 3D LUTs and XMP looks become creative profiles; .dcp files go to the camera profiles (as camera.importDcp) (Amount 0–200 %; rendered on the CPU) in the profile browser's groups (folder / zip name, else Imported) → {imported: [{id, name, group}], camera: [camera.importDcp's imported], failed}",
            always,
            import
        ),
        cmd!("profile.deleteImported", "Delete Imported Profile", [], None, "{id: lut:…}", always, |s, p| {
            let id = str_param(p, "id").ok_or_else(|| bad("profile.deleteImported", "missing id"))?.to_string();
            let Some(i) = s.lut_profiles.iter().position(|x| x.id == id) else {
                return Err(bad("profile.deleteImported", format!("no imported profile `{id}`")));
            };
            let gone = s.lut_profiles.remove(i);
            // only the library's own copy, and only when no other profile is read from it
            if let Some(dir) = profiles_dir(s)
                && let Some(f) = own_copy(&dir, &gone)
                && !s.lut_profiles.iter().any(|q| own_copy(&dir, q).as_ref() == Some(&f))
            {
                let _ = std::fs::remove_file(&f);
            }
            lcc_pipeline::lut::unregister(&id);
            s.save_prefs()?;
            crate::profile_refs::refresh(s);
            Ok(json!({"deleted": id}))
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single .dcp handed to Import Profiles is a camera profile only: it is not also read as a
    /// .cube LUT and reported as failed (OneSlips).
    #[test]
    fn a_dcp_file_is_not_read_as_a_lut() {
        let dir = std::env::temp_dir().join(format!("lut-dcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("mine.dcp");
        std::fs::write(&f, [0x49u8, 0x49, 0x52, 0x43, 8, 0, 0, 0]).unwrap();
        let (luts, failed) = cube_files(&[f.to_string_lossy().to_string()]);
        assert!(luts.is_empty() && failed.is_empty(), "{luts:?} {failed:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod register_tests {
    use super::*;

    fn profile(id: &str, file: &Path) -> LutProfile {
        LutProfile { id: id.into(), name: id.into(), group: String::new(), file: file.to_string_lossy().into_owned() }
    }

    #[test]
    fn profiles_naming_one_file_share_a_table_and_the_total_is_bounded() {
        let dir = std::env::temp_dir().join(format!("lc-lut-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cube = "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let (a, b) = (dir.join("a.cube"), dir.join("b.cube"));
        std::fs::write(&a, cube).unwrap();
        std::fs::write(&b, cube).unwrap();
        let ps = [profile("lut:share-1", &a), profile("lut:share-2", &a), profile("lut:share-3", &b)];
        // room for one 2x2x2 table (96 bytes): the alias of `a` shares it, the second file does not fit
        register_profiles(&dir, &ps, 100);
        let (one, two) = (lcc_pipeline::lut::get("lut:share-1").unwrap(), lcc_pipeline::lut::get("lut:share-2").unwrap());
        assert!(std::sync::Arc::ptr_eq(&one, &two));
        assert!(lcc_pipeline::lut::get("lut:share-3").is_none());
        for p in &ps {
            lcc_pipeline::lut::unregister(&p.id);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
