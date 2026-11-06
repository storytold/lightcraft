//! Which commands may run while automation roots are in force, and which of their parameters are
//! paths. [`check_command`] runs in [`crate::Session::execute`] before any command, so a path an
//! agent gives is checked even where the command itself has no check of its own, and a command
//! nobody has looked at yet is refused instead of being trusted. The tests below walk the command
//! registry: a new command doesn't pass until it has a line here.

use serde_json::Value;

use super::{Access, check_read, check_write, current};
use Access::{Read, Write};

/// What a command's parameters mean to the automation roots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// No parameter names a file or folder. What the command reaches through the library (a photo's
    /// original, its sidecar) is checked where it is opened; the app's own folders are not confined
    /// (`docs/mcp.md`).
    NoPaths,
    /// These parameters name files or folders. A dot goes into an object; a value may be one string
    /// or a list of them, at any depth.
    Paths(&'static [(&'static str, Access)]),
    /// Refused while roots are in force, for this reason.
    Refused(&'static str),
}

/// The commands with paths among their parameters (`app.export` is the MCP server's own).
const PATHS: &[(&str, &[(&str, Access)])] = &[
    ("app.export", &[("path", Write), ("dir", Write), ("watermark.image", Read)]),
    ("curve.exportPresets", &[("path", Write)]),
    ("curve.importPresets", &[("path", Read), ("paths", Read)]),
    ("denoise.models.inspect", &[("path", Read)]),
    ("denoise.models.install", &[("path", Read)]),
    ("export.checkTarget", &[("path", Write)]),
    ("export.contactSheet", &[("path", Write)]),
    ("faces.models.inspect", &[("path", Read)]),
    ("faces.models.install", &[("path", Read)]),
    ("folder.create", &[("parent", Write)]),
    ("folder.label", &[("path", Read)]),
    ("folder.move", &[("path", Write), ("into", Write)]),
    ("folder.rename", &[("path", Write)]),
    ("folder.scanChanges", &[("path", Read)]),
    ("folder.synchronize", &[("path", Read)]),
    ("keyword.export", &[("path", Write)]),
    ("keyword.import", &[("path", Read)]),
    ("library.autoImport", &[("folder", Read)]),
    ("library.autoImportScan", &[("listing", Read)]),
    ("library.browse", &[("path", Read)]),
    ("library.findMissing", &[("folder", Read)]),
    ("library.import", &[("paths", Read), ("destination", Write)]),
    ("library.importLightroom", &[("path", Read)]),
    ("library.importPreview", &[("paths", Read)]),
    ("library.inspectLightroom", &[("path", Read)]),
    ("library.removeFolder", &[("path", Read)]),
    ("library.smartPreviewsLocation", &[("path", Write)]),
    ("library.source", &[("path", Read)]),
    ("mask.exportEffects", &[("path", Write)]),
    ("mask.importEffects", &[("path", Read), ("paths", Read)]),
    ("merge.hdr", &[("previewPath", Write)]),
    ("merge.hdrPanorama", &[("previewPath", Write)]),
    ("merge.panorama", &[("previewPath", Write)]),
    ("metadata.downloadPlaceNames", &[("dataset", Write)]),
    ("metadata.placeNamesStatus", &[("dataset", Read)]),
    ("metadata.reverseGeocode", &[("dataset", Read)]),
    ("photo.autoTagTracklog", &[("path", Read)]),
    ("photo.editExternal", &[("dir", Write)]),
    ("photo.moveToFolder", &[("folder", Write)]),
    ("photo.relink", &[("path", Read)]),
    ("preset.export", &[("path", Write)]),
    ("preset.import", &[("paths", Read)]),
    ("print.exportPdf", &[("path", Write)]),
    ("print.set", &[("job.color.profile.path", Read)]),
    ("print.template.export", &[("path", Write)]),
    ("print.template.import", &[("path", Read)]),
    ("profile.import", &[("paths", Read)]),
    ("slideshow.export", &[("path", Write)]),
    ("slideshow.overlays", &[("identityPlate.image", Read)]),
    ("slideshow.preview", &[("path", Write)]),
    ("slideshow.templates.export", &[("path", Write)]),
    ("slideshow.templates.import", &[("path", Read)]),
];

const REFUSED: &[(&str, &str)] =
    &[("export.addToPhotos", "hands files to another application"), ("library.devices", "lists the volumes mounted on this computer")];

const NO_PATHS: &[&str] = &[
    "activity.cancel",
    "activity.list",
    "album.addPhotos",
    "album.clearQuick",
    "album.create",
    "album.createSmart",
    "album.delete",
    "album.move",
    "album.removePhotos",
    "album.rename",
    "album.reorder",
    "album.ruleFields",
    "album.setCover",
    "album.setRules",
    "album.setTarget",
    "album.sort",
    "album.toggleTarget",
    "albums.list",
    "app.gpu",
    "app.hotPixelLimit",
    "app.memoryBudget",
    "beforeAfter.copyAfterToBefore",
    "beforeAfter.copyBeforeToAfter",
    "beforeAfter.resetBefore",
    "beforeAfter.setBefore",
    "beforeAfter.swap",
    "catalog.query",
    "catalog.stats",
    "crop.aspect",
    "crop.autoStraighten",
    "crop.drag",
    "crop.reset",
    "crop.rotateAspect",
    "crop.set",
    "crop.straighten",
    "curve.applyPreset",
    "curve.deletePreset",
    "curve.presets",
    "curve.reset",
    "curve.savePreset",
    "denoise.cancel",
    "denoise.clear",
    "denoise.models.download",
    "denoise.models.downloadCancel",
    "denoise.models.downloads",
    "denoise.models.list",
    "denoise.models.remove",
    "denoise.models.select",
    "denoise.models.test",
    "denoise.pump",
    "denoise.queue",
    "denoise.settings",
    "denoise.status",
    "denoise.toggle",
    "develop.adjust",
    "develop.auto",
    "develop.autoBwMix",
    "develop.autoSync",
    "develop.beginInteraction",
    "develop.cancelInteraction",
    "develop.controls",
    "develop.copy",
    "develop.curve",
    "develop.endInteraction",
    "develop.get",
    "develop.hdr",
    "develop.matchExposure",
    "develop.merge",
    "develop.paste",
    "develop.pastePrevious",
    "develop.profile",
    "develop.quickAdjust",
    "develop.reset",
    "develop.resetControl",
    "develop.resetSection",
    "develop.sectionEnabled",
    "develop.set",
    "develop.sync",
    "develop.targeted",
    "develop.treatment",
    "develop.updateProcess",
    "develop.wb",
    "develop.wbPick",
    "edit.redo",
    "edit.undo",
    "export.deletePreset",
    "export.photosImports",
    "export.presets",
    "export.savePreset",
    "faces.detect",
    "faces.enable",
    "faces.evaluate",
    "faces.gpu",
    "faces.index",
    "faces.models.download",
    "faces.models.downloadCancel",
    "faces.models.downloads",
    "faces.models.list",
    "faces.models.remove",
    "faces.models.select",
    "faces.models.test",
    "faces.nameFaces",
    "faces.person",
    "faces.pump",
    "faces.renamePerson",
    "faces.setName",
    "faces.suggest",
    "faces.unnamed",
    "filter.applyPreset",
    "filter.deletePreset",
    "filter.presets",
    "filter.savePreset",
    "focus.pump",
    "geometry.guides",
    "geometry.upright",
    "history.clear",
    "history.list",
    "history.next",
    "history.previous",
    "history.restore",
    "journal.list",
    "keyword.create",
    "keyword.delete",
    "keyword.deleteSet",
    "keyword.edit",
    "keyword.info",
    "keyword.list",
    "keyword.merge",
    "keyword.move",
    "keyword.nextSet",
    "keyword.previousSet",
    "keyword.purgeUnused",
    "keyword.rename",
    "keyword.saveSet",
    "keyword.setDefaultParent",
    "keyword.setShortcut",
    "keyword.sets",
    "keyword.shortcut",
    "keyword.suggest",
    "keyword.toggleFromSet",
    "keyword.toggleShortcut",
    "keyword.useSet",
    "label.applySet",
    "label.deleteSet",
    "label.names",
    "label.saveSet",
    "label.setNames",
    "label.sets",
    "lens.databaseStatus",
    "lens.downloadDatabase",
    "library.buildPreviews",
    "library.cancelPreviews",
    "library.clearFilter",
    "library.clearPreviews",
    "library.compact",
    "library.down",
    "library.emptyRecentlyDeleted",
    "library.filter",
    "library.findSimilar",
    "library.folders",
    "library.forgetLocal",
    "library.groups",
    "library.holdView",
    "library.info",
    "library.memory",
    "library.missing",
    "library.next",
    "library.preferences",
    "library.previewProgress",
    "library.previous",
    "library.recheckPreviewOnly",
    "library.refresh",
    "library.refreshLensProfiles",
    "library.retrySidecars",
    "library.select",
    "library.selectAll",
    "library.selectBy",
    "library.selectNone",
    "library.shuffle",
    "library.smartPreviews",
    "library.sort",
    "library.state",
    "library.toggleAutoWriteXmp",
    "library.toggleFilters",
    "library.up",
    "library.xmpPreferences",
    "mask.add",
    "mask.addComponent",
    "mask.adjust",
    "mask.applyEffect",
    "mask.brushStroke",
    "mask.component",
    "mask.delete",
    "mask.deleteAll",
    "mask.deleteEffect",
    "mask.duplicate",
    "mask.duplicateEffect",
    "mask.effects",
    "mask.favoriteEffect",
    "mask.invert",
    "mask.move",
    "mask.objectPoint",
    "mask.refine",
    "mask.refineDetail",
    "mask.rename",
    "mask.renameEffect",
    "mask.renameEffectGroup",
    "mask.sampleColor",
    "mask.saveEffect",
    "mask.select",
    "mask.setEffectGroup",
    "mask.toolEffect",
    "mask.update",
    "mask.updateEffect",
    "mask.visible",
    "merge.hdrBatch",
    "metadata.applyPreset",
    "metadata.deletePreset",
    "metadata.presets",
    "metadata.savePreset",
    "photo.addToLibrary",
    "photo.allMetadata",
    "photo.analyze",
    "photo.clearLabel",
    "photo.convertToDng",
    "photo.copyMetadata",
    "photo.cycleFlag",
    "photo.decreaseFlag",
    "photo.decreaseRating",
    "photo.delete",
    "photo.deletePermanently",
    "photo.duplicate",
    "photo.flag",
    "photo.flipHorizontal",
    "photo.flipVertical",
    "photo.increaseFlag",
    "photo.increaseRating",
    "photo.inspect",
    "photo.label",
    "photo.pasteMetadata",
    "photo.pick",
    "photo.rate",
    "photo.readFocusPoints",
    "photo.readMetadataFromFile",
    "photo.reject",
    "photo.reload",
    "photo.removeRegion",
    "photo.rename",
    "photo.renamePreview",
    "photo.renameTokens",
    "photo.restore",
    "photo.rotateLeft",
    "photo.rotateRight",
    "photo.saveMetadataToFile",
    "photo.setCaptureTime",
    "photo.setMeta",
    "photo.setRegion",
    "photo.smartPreview",
    "photo.unflag",
    "photo.virtualCopy",
    "pointColor.delete",
    "pointColor.pick",
    "preset.apply",
    "preset.create",
    "preset.delete",
    "preset.favorite",
    "preset.move",
    "preset.rename",
    "preset.saveRecipe",
    "preset.update",
    "presets.list",
    "print.cell.add",
    "print.cell.delete",
    "print.cell.move",
    "print.cell.page",
    "print.cell.photo",
    "print.cell.resize",
    "print.cell.stack",
    "print.get",
    "print.layout.autoLayout",
    "print.layout.clear",
    "print.layout.newPage",
    "print.reset",
    "print.template.apply",
    "prefs.backgroundPrep",
    "prefs.setBackgroundPrep",
    "print.template.delete",
    "print.template.list",
    "print.template.save",
    "profile.deleteCamera",
    "profile.deleteImported",
    "profile.favorite",
    "profiles.camera",
    "profiles.list",
    "profiles.menu",
    "redeye.add",
    "redeye.catchlight",
    "redeye.delete",
    "segment.model.cancel",
    "segment.model.download",
    "segment.model.status",
    "segment.prepare",
    "slideshow.exportStatus",
    "slideshow.layout",
    "slideshow.playback",
    "slideshow.templates.apply",
    "slideshow.templates.delete",
    "slideshow.templates.list",
    "slideshow.templates.rename",
    "slideshow.templates.save",
    "slideshow.templates.update",
    "slideshow.titles",
    "spot.add",
    "spot.delete",
    "spot.findBlemishes",
    "spot.findDust",
    "spot.refreshSource",
    "spot.select",
    "spot.update",
    "stack.auto",
    "stack.collapseAll",
    "stack.expandAll",
    "stack.group",
    "stack.moveDown",
    "stack.moveUp",
    "stack.remove",
    "stack.setTop",
    "stack.split",
    "stack.toggle",
    "stack.ungroup",
    "version.create",
    "version.delete",
    "version.rename",
    "version.restore",
    "version.update",
];

/// How deep a path parameter's lists may nest; deeper is refused, not skipped.
const MAX_DEPTH: usize = 8;

/// The policy of command `id`; `None` for a command that isn't classified, which is refused.
pub fn policy(id: &str) -> Option<Policy> {
    if let Some((_, params)) = PATHS.iter().find(|(c, _)| *c == id) {
        return Some(Policy::Paths(params));
    }
    if let Some((_, why)) = REFUSED.iter().find(|(c, _)| *c == id) {
        return Some(Policy::Refused(why));
    }
    NO_PATHS.contains(&id).then_some(Policy::NoPaths)
}

/// May command `id` run with `params` here? Always, without roots in force.
pub fn check_command(id: &str, params: &Value) -> Result<(), String> {
    if current().is_none() {
        return Ok(());
    }
    match policy(id) {
        None => Err(format!("automation command refused: `{id}` is not known to the automation roots")),
        Some(Policy::Refused(why)) => Err(format!("automation command refused: `{id}` {why}")),
        Some(Policy::NoPaths) => Ok(()),
        Some(Policy::Paths(paths)) => {
            for (name, access) in paths {
                let mut at = Some(params);
                for part in name.split('.') {
                    at = at.and_then(|v| v.get(part));
                }
                if let Some(value) = at {
                    check_value(value, *access, MAX_DEPTH)?;
                }
            }
            Ok(())
        }
    }
}

/// Check every string in `value`. Commands trim some of their paths, so the path a command would
/// open is checked as well as the text it was given. An empty string means "none" to the commands;
/// all blanks is a file name to those that don't trim, so only an empty string goes unchecked.
fn check_value(value: &Value, access: Access, depth: usize) -> Result<(), String> {
    match value {
        Value::String(path) if path.is_empty() => Ok(()),
        Value::String(path) => {
            let check = |p: &str| match access {
                Read => check_read(p),
                Write => check_write(p),
            };
            check(path)?;
            let trimmed = path.trim();
            if trimmed != path && !trimmed.is_empty() { check(trimmed) } else { Ok(()) }
        }
        Value::Array(_) if depth == 0 => Err("automation command refused: a path parameter nests too deep".into()),
        Value::Array(items) => items.iter().try_for_each(|v| check_value(v, access, depth - 1)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::json;

    use super::super::{AutomationRoots, confine};
    use super::*;
    use crate::cmd::command_specs;

    /// Parameters the documentation calls by a path's name that aren't paths: an album's
    /// `folder?: bool`, a keyword's `into`, the `{folder}` token of a rename template.
    const NOT_PATHS: &[&str] = &["album.create", "keyword.merge", "photo.rename", "photo.renamePreview"];

    /// The input half of a command's parameter text names a path parameter.
    fn documents_a_path(params: &str) -> bool {
        let input = params.split('→').next().unwrap_or_default();
        let input = input.split(" — ").next().unwrap_or_default();
        input.split(['{', ',', '|']).any(|part| {
            let name = part.trim().split([':', '?', '}', ' ']).next().unwrap_or_default();
            matches!(name, "path" | "paths" | "dir" | "dataset" | "previewPath" | "destination" | "folder" | "file" | "files" | "into")
        })
    }

    /// Every registered command is classified once.
    #[test]
    fn every_registered_command_is_classified_once() {
        let unclassified: Vec<&str> = command_specs().iter().map(|c| c.id).filter(|id| policy(id).is_none()).collect();
        assert!(unclassified.is_empty(), "add these to file_access/commands.rs: {unclassified:?}");
        let registered: HashSet<&str> = command_specs().iter().map(|c| c.id).collect();
        let listed = PATHS.iter().map(|(id, _)| *id).chain(REFUSED.iter().map(|(id, _)| *id)).chain(NO_PATHS.iter().copied());
        let mut seen = HashSet::new();
        for id in listed {
            assert!(seen.insert(id), "{id} is listed twice");
            assert!(id == "app.export" || registered.contains(id), "{id} is listed but no such command exists");
        }
    }

    /// Every command documenting a path declares it.
    #[test]
    fn every_command_documenting_a_path_declares_it() {
        let undeclared: Vec<&str> = command_specs()
            .iter()
            .filter(|c| documents_a_path(c.params) && !NOT_PATHS.contains(&c.id) && policy(c.id) == Some(Policy::NoPaths))
            .map(|c| c.id)
            .collect();
        assert!(undeclared.is_empty(), "these document a path parameter: {undeclared:?}");
    }

    /// A temporary folder with `in`, `out` and `outside`, and roots on the first two.
    fn roots(tag: &str) -> (std::path::PathBuf, Arc<AutomationRoots>) {
        let base = std::env::temp_dir().join(format!("lc-file-access-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("in")).unwrap();
        std::fs::create_dir_all(base.join("out")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        let roots = AutomationRoots::new(Some(&base.join("in")), Some(&base.join("out"))).unwrap().unwrap();
        (base, roots)
    }

    /// `params` with the value at the dotted `name` set to `value`.
    fn with_param(name: &str, value: Value) -> Value {
        name.rsplit('.').fold(value, |inner, part| json!({ part: inner }))
    }

    /// Every declared path is refused outside and allowed inside.
    #[test]
    fn every_declared_path_is_refused_outside_and_allowed_inside() {
        let (base, roots) = roots("declared");
        let outside = base.join("outside/x").to_string_lossy().into_owned();
        confine(Some(&roots), || {
            for (id, params) in PATHS {
                for (name, access) in *params {
                    let inside = base.join(if *access == Read { "in/x" } else { "out/x" }).to_string_lossy().into_owned();
                    assert!(check_command(id, &with_param(name, json!(inside))).is_ok(), "{id} {name} inside");
                    let refused = with_param(name, json!(outside));
                    assert!(check_command(id, &refused).unwrap_err().contains("automation"), "{id} {name}");
                    // also in a list, and with the blanks a command trims away
                    assert!(check_command(id, &with_param(name, json!([[outside]]))).is_err(), "{id} {name} in a list");
                    assert!(check_command(id, &with_param(name, json!(format!(" {outside}")))).is_err(), "{id} {name} with a blank");
                }
            }
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Unknown and refused commands fail closed.
    #[test]
    fn unknown_and_refused_commands_fail_closed() {
        let (base, roots) = roots("closed");
        confine(Some(&roots), || {
            assert!(check_command("no.such.command", &json!({})).unwrap_err().contains("not known"));
            assert!(check_command("library.devices", &json!({})).unwrap_err().contains("refused"));
            assert!(check_command("library.state", &json!({})).is_ok());
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    /// All blanks names a file in the working folder, so it is checked; only an empty string means none.
    #[test]
    fn a_blank_path_is_checked_and_an_empty_one_is_not() {
        let (base, roots) = roots("blank");
        confine(Some(&roots), || {
            for blank in ["   ", "\t", " \n "] {
                let refused = check_command("library.inspectLightroom", &json!({"path": blank})).unwrap_err();
                assert!(refused.contains("automation"), "{blank:?}: {refused}");
            }
            assert!(check_command("library.inspectLightroom", &json!({"path": ""})).is_ok());
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Too deep a list is refused not skipped.
    #[test]
    fn too_deep_a_list_is_refused_not_skipped() {
        let (base, roots) = roots("deep");
        let inside = base.join("in/x").to_string_lossy().into_owned();
        let deep = (0..=MAX_DEPTH).fold(json!(inside), |v, _| json!([v]));
        confine(Some(&roots), || assert!(check_command("library.importPreview", &json!({"paths": deep})).is_err()));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Source files that touch the file system without going through `file_access`, each because all
    /// it opens is the app's own (the library folder, the settings and model folders, the caches)
    /// or a path `PATHS` has already checked. A new file that opens files elsewhere needs a check, not a line here.
    const OWN_FOLDERS: &[&str] = &[
        "camera_profiles.rs",
        "cmd/denoise.rs",
        "cmd/face_detect.rs",
        "cmd/face_models.rs",
        "cmd/face_recognize.rs",
        "cmd/geocode.rs",
        "config.rs",
        "denoise/builtin.rs",
        "denoise/gpu.rs",
        "denoise/products.rs",
        "denoise/session.rs",
        "denoise/settings.rs",
        "devices.rs",
        "face_download.rs",
        "faces_index.rs",
        "guard.rs",
        "import/commit.rs",
        "import/files.rs",
        "lens_profiles.rs",
        "lensfun_download.rs",
        "library.rs",
        "lightroom_archive.rs",
        "lightroom_job.rs",
        "lightroom_sqlite.rs",
        "memory.rs",
        "model_download.rs",
        "preset_import.rs",
        "slide_template.rs",
    ];

    /// Does this line of source open a file?
    fn opens_files(code: &str) -> bool {
        ["std::fs::", "File::open", "File::create", "OpenOptions::new", "safe_file::write"].iter().any(|api| code.contains(api))
    }

    /// Every Rust source file under `dir`.
    fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if entry.file_name() != "tests" {
                    sources(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "rs") && !entry.file_name().to_string_lossy().contains("test") {
                out.push(path);
            }
        }
    }

    /// File access outside the checks is accounted for.
    #[test]
    fn file_access_outside_the_checks_is_accounted_for() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&src, &mut files);
        let mut unchecked = Vec::new();
        for file in files {
            // (a Windows checkout may have CRLF line ends)
            let text = std::fs::read_to_string(&file).unwrap().replace("\r\n", "\n");
            let code = text.split("\n#[cfg(test)]\nmod ").next().unwrap_or_default();
            let name = file.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
            let checked = code.contains("file_access::") || name.starts_with("file_access");
            match (opens_files(code) && !checked, OWN_FOLDERS.contains(&name.as_str())) {
                (true, false) => unchecked.push(name),
                (false, true) => panic!("{name} is listed in OWN_FOLDERS but needs no line there"),
                _ => {}
            }
        }
        assert!(unchecked.is_empty(), "these open files with no automation root check: {unchecked:?}");
    }
}
