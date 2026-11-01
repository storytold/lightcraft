use super::ExportOptions;
use super::options::{Conflict, DestinationMode};
use super::prepare::{Destination, Exported};

/// Write an exported or rendered file on disk: its folder is created if needed, and the file is
/// replaced atomically ([`lightcraft_catalog::safe_file::write_atomic_nosync`]: a temp file
/// renamed into place), so a failure part-way leaves any previous file intact and no truncated
/// one. Not synced to disk (issue #134): an export can always be made again from the original.
/// The native writer behind exports, renders and screenshots (app, CLI, MCP).
pub fn write_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_with(path, bytes, false)
}

/// [`write_file`], synced to disk: for a written file that becomes a library photo (Edit a Copy)
/// and so can't simply be exported again once the catalog points at it.
pub fn write_file_durable(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_with(path, bytes, true)
}

/// Write `path` only when nothing exists there: the destination is prepared as a complete temp
/// file and published with create-new semantics, so a file that appears after a caller's check is
/// never replaced. Screenshots of an initially absent path use this instead of the overwriting
/// writer.
pub fn write_file_new(path: &str, bytes: &[u8]) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    lightcraft_catalog::safe_file::write_new(p, bytes).map_err(|e| format!("{path}: {e}"))
}

fn write_with(path: &str, bytes: &[u8], durable: bool) -> Result<(), String> {
    use lightcraft_catalog::safe_file::{write_atomic, write_atomic_nosync};
    let p = std::path::Path::new(path);
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if durable { write_atomic(p, bytes) } else { write_atomic_nosync(p, bytes) }.map_err(|e| format!("{path}: {e}"))
}

/// The path of the sidecar with extension `ext` of the exported file `main`.
fn sidecar_path(main: &str, ext: &str) -> String {
    std::path::Path::new(main).with_extension(ext).to_string_lossy().to_string()
}

/// Where a batch's files go: each photo's path (destination folder, subfolder, conflict policy,
/// sidecars, never an original) and its write, in batch order.
pub(super) struct Placer<'a> {
    o: &'a ExportOptions,
    to: &'a Destination,
    pub(super) single: bool,
    taken: std::collections::HashSet<String>,
    pub(super) out: Vec<serde_json::Value>,
    write: &'a mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &'a dyn Fn(&str) -> bool,
    stop_on_error: bool,
}

/// Why `subfolder` would lead out of the export folder (a `..` component, also as Windows reads
/// `...` or `.. .`, an absolute path or drive, or a Windows device name such as `CON` or `NUL`),
/// `None` when it stays inside. Nested subfolders (`a/b`) are fine. Checked when the params come
/// in and again before writing, since a saved preset is read without the checks.
pub(super) fn subfolder_escapes(subfolder: &str) -> Option<&'static str> {
    let s = subfolder.trim();
    if s.starts_with(['/', '\\']) || (s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) && s.as_bytes().get(1) == Some(&b':')) {
        return Some("is an absolute path; give a folder inside the export folder");
    }
    let components = || s.split(['/', '\\']);
    // Windows drops a name's trailing dots and spaces, so `...` and `.. .` are `..` there
    if components().any(|c| c.contains("..") && c.trim_matches([' ', '.']).is_empty()) {
        return Some("can't contain `..`; give a folder inside the export folder");
    }
    if components().any(is_device_name) {
        return Some("names a Windows device (CON, PRN, AUX, NUL, COM1–9, LPT1–9); give another folder name");
    }
    // the platform's own reading of the path, for any form the checks above don't know
    if !plain_relative(std::path::Path::new(s)) {
        return Some("is not a folder inside the export folder");
    }
    None
}

/// A Windows device name, with or without an extension (`nul.txt`): Windows opens the device
/// instead of a folder of that name.
fn is_device_name(component: &str) -> bool {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let base = component.trim_end_matches([' ', '.']);
    let base = base.split('.').next().unwrap_or(base).trim_end();
    let numbered = |prefix: &str| {
        base.len() == 4
            && base.get(..3).is_some_and(|p| p.eq_ignore_ascii_case(prefix))
            && base.as_bytes().get(3).is_some_and(|d| (b'1'..=b'9').contains(d))
    };
    DEVICES.iter().any(|d| base.eq_ignore_ascii_case(d)) || numbered("COM") || numbered("LPT")
}

/// Only folder names (and `.`), as the platform reads the path: no root, prefix or `..`.
fn plain_relative(path: &std::path::Path) -> bool {
    use std::path::Component;
    path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Does `path` (built from `base`) still lie inside `base`? The second line behind
/// [`subfolder_escapes`], on the folder actually written to.
fn stays_inside(base: &str, path: &str) -> bool {
    std::path::Path::new(path).strip_prefix(base).is_ok_and(plain_relative)
}

fn join_path(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { format!("{}/{b}", a.trim_end_matches('/')) }
}

impl<'a> Placer<'a> {
    pub(super) fn new(
        o: &'a ExportOptions,
        to: &'a Destination,
        write: &'a mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
        exists: &'a dyn Fn(&str) -> bool,
        stop_on_error: bool,
    ) -> Placer<'a> {
        Placer { o, to, single: false, taken: Default::default(), out: Vec::new(), write, exists, stop_on_error }
    }

    /// The folder photo `name` (its original in `source_dir`) is written to: the batch's folder or
    /// the photo's own ([`DestinationMode`]), plus the subfolder. An error for a photo with no
    /// folder of its own, or whose folder is gone: never a file written somewhere else instead.
    fn folder_for(&self, name: &str, source_dir: Option<&str>) -> Result<String, String> {
        let base = match self.o.destination_mode {
            DestinationMode::SpecificFolder => self.to.dir.clone(),
            DestinationMode::SameAsOriginal => {
                let Some(d) = source_dir else {
                    return Err(format!(
                        "{name}: the photo has no folder on disk (a demo photo or one in the browser's storage), so it can't be exported to the same folder as its original; choose a specific folder"
                    ));
                };
                if !(self.exists)(d) {
                    return Err(format!("{name}: the original's folder is missing ({d}); reconnect it or choose a specific folder"));
                }
                d.to_string()
            }
        };
        if let Some(why) = subfolder_escapes(&self.o.subfolder) {
            return Err(format!("{name}: the subfolder `{}` {why}", self.o.subfolder));
        }
        if self.o.subfolder.is_empty() {
            return Ok(base);
        }
        let folder = join_path(&base, &self.o.subfolder);
        if !stays_inside(&base, &folder) {
            return Err(format!("{name}: the subfolder `{}` is not a folder inside the export folder", self.o.subfolder));
        }
        Ok(folder)
    }

    /// Place photo `photo`'s export result (file `name`, its original in `source_dir`): write it
    /// and record the outcome. `Err` only when it failed and `stop_on_error` is set.
    pub(super) fn place(
        &mut self,
        photo: lightcraft_catalog::PhotoId,
        name: String,
        source_dir: Option<&str>,
        guard: &crate::originals::OriginalGuard,
        e: Result<Exported, String>,
    ) -> Result<(), String> {
        use serde_json::json;
        // an exact file path needs no folder
        let dir = if self.to.exact.is_some() && self.single { Ok(String::new()) } else { self.folder_for(&name, source_dir) };
        let (o, exists) = (self.o, self.exists);
        let (dir, e) = match dir.and_then(|dir| e.map(|e| (dir, e))) {
            Ok(v) => v,
            Err(err) if self.stop_on_error => return Err(err),
            Err(err) => {
                self.out.push(json!({"photo": photo.0, "file": name, "error": err}));
                return Ok(());
            }
        };
        let dir = dir.as_str();
        // the exported file and its sidecars
        let group = |main: &str| std::iter::once(main.to_string()).chain(e.sidecars.iter().map(|(x, _)| sidecar_path(main, x))).collect::<Vec<_>>();
        let path = match self.to.exact.as_deref().filter(|_| self.single) {
            Some(p) => Ok(p.to_string()),
            None => {
                let path = join_path(dir, &e.file_name);
                let taken = &self.taken;
                let busy = |main: &str| group(main).iter().any(|p| taken.contains(p) || exists(p));
                if !busy(&path) {
                    Ok(path)
                } else {
                    match o.conflict {
                        Conflict::Overwrite => Ok(path),
                        Conflict::Skip => {
                            self.out.push(json!({"skipped": path}));
                            return Ok(());
                        }
                        Conflict::Unique => {
                            let (stem, ext) = e.file_name.rsplit_once('.').map_or((e.file_name.as_str(), None), |(a, b)| (a, Some(b)));
                            let name = |n: usize| join_path(dir, &ext.map_or(format!("{stem}-{n}"), |x| format!("{stem}-{n}.{x}")));
                            (2..1_000_000).map(name).find(|p| !busy(p)).ok_or_else(|| format!("{path}: no free file name"))
                        }
                    }
                }
            }
        };
        let file = path.clone().unwrap_or_else(|_| name.clone());
        let write = &mut *self.write;
        let written = path.and_then(|path| {
            let files = group(&path);
            // never over an original, whatever the conflict policy or the exact path said
            for f in &files {
                guard.check(std::path::Path::new(f))?;
            }
            write(&path, &e.bytes)?;
            let mut sidecars = Vec::new();
            for ((_, bytes), sc) in e.sidecars.iter().zip(files.iter().skip(1)) {
                write(sc, bytes)?;
                sidecars.push(sc.clone());
            }
            Ok((path, files, sidecars))
        });
        match written {
            Ok((path, files, sidecars)) => {
                self.taken.extend(files);
                let mut entry = json!({"path": path, "width": e.width, "height": e.height, "bytes": e.bytes.len(), "sidecars": sidecars});
                if let Some(why) = &e.gpu_fallback {
                    entry["gpuFallback"] = json!(why);
                }
                self.out.push(entry);
            }
            Err(err) if self.stop_on_error => return Err(err),
            Err(err) => self.out.push(json!({"photo": photo.0, "file": file, "error": err})),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{is_device_name, stays_inside};

    #[test]
    fn device_names_are_known_with_any_case_and_extension() {
        for name in ["CON", "nul", "Aux", "PRN ", "nul.txt", "COM1", "lpt9", "com3.jpg", "CON."] {
            assert!(is_device_name(name), "{name}");
        }
        for name in ["Console", "COM0", "COM10", "LPT", "connect", "a.nul", ""] {
            assert!(!is_device_name(name), "{name}");
        }
    }

    #[test]
    fn the_built_folder_is_checked_against_the_export_folder() {
        assert!(stays_inside("out", "out/a/b"));
        assert!(stays_inside("/home/me/out", "/home/me/out/./a"));
        assert!(!stays_inside("out", "out/../elsewhere"));
        assert!(!stays_inside("out", "other/a"));
        assert!(!stays_inside("/home/me/out", "/home/me/outside"));
    }
}
