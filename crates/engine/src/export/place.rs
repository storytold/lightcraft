use super::ExportOptions;
use super::Ran;
use super::options::{Conflict, DestinationMode};
use super::prepare::Destination;

/// Write an exported or rendered file on disk: its folder is created if needed, and the file is
/// replaced atomically ([`lightcraft_catalog::safe_file::write_atomic_nosync`]: a temp file
/// renamed into place), so a failure part-way leaves any previous file intact and no truncated
/// one. Not synced to disk (issue #134): an export can always be made again from the original.
/// The native writer behind exports, renders and screenshots (app, CLI, MCP).
pub fn write_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_with(path, bytes, false)
}

/// Copy an Original video atomically with bounded memory, checking location on the same open
/// file when requested. There is no clip-size limit on this destination-backed path.
pub fn copy_original(source: &str, destination: &str, remove_location: bool) -> Result<u64, String> {
    use std::io::{Read, Seek, SeekFrom};
    use std::path::Path;
    if lightcraft_catalog::safe_file::same_file(Path::new(source), Path::new(destination)) {
        return Err(format!("{destination}: won't overwrite the original"));
    }
    let mut file = std::fs::File::open(source).map_err(|e| format!("{source}: {e}"))?;
    let before = file.metadata().map_err(|e| format!("{source}: {e}"))?;
    if remove_location {
        let metadata = crate::files::read_video_metadata_from(&mut file, before.len(), source)?;
        lightcraft_meta::verify_video_location_absent(&metadata).map_err(|e| format!("{source}: {e}"))?;
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("{source}: {e}"))?;
    let target = Path::new(destination);
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    lightcraft_catalog::safe_file::write_atomic_nosync_with_check(
        target,
        &mut |out| {
            let n = std::io::copy(&mut (&mut file).take(before.len()), out)?;
            let after = file.metadata()?;
            if n != before.len() || after.len() != before.len() || after.modified()? != before.modified()? {
                return Err(std::io::Error::other("original changed during export; retry when the file is no longer being written"));
            }
            Ok(())
        },
        // The check sees the bytes that would be published, not just the source: a file that
        // gained a location between the first check and the copy never reaches its destination.
        &mut |tmp| {
            if remove_location {
                let mut copied = std::fs::File::open(tmp)?;
                let len = copied.metadata()?.len();
                let metadata = crate::files::read_video_metadata_from(&mut copied, len, source).map_err(std::io::Error::other)?;
                lightcraft_meta::verify_video_location_absent(&metadata).map_err(|e| std::io::Error::other(format!("{destination}: {e}")))?;
            }
            Ok(())
        },
    )
    .map_err(|e| format!("{destination}: {e}"))
}

/// Host copier for an unchanged Original: source path, destination path, location-removal policy.
pub type OriginalCopier<'a> = dyn FnMut(&str, &str, bool) -> Result<u64, String> + 'a;

/// Thread-safe host copier for background Original exports.
pub type SharedOriginalCopier = std::sync::Arc<dyn Fn(&str, &str, bool) -> Result<u64, String> + Send + Sync>;

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

/// How large a previous sidecar may be to keep a copy for restoring after a failed export.
const MAX_SIDECAR_BACKUP: u64 = 16 * 1024 * 1024;

/// Put the sidecars a failed export replaced back, or remove the ones it created.
fn restore_sidecars(write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>, previous: &[(String, Option<Vec<u8>>)]) {
    for (path, bytes) in previous {
        match bytes {
            Some(bytes) => {
                let _ = write(path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
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
    pub(super) copy: Option<&'a mut OriginalCopier<'a>>,
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
        Placer { o, to, single: false, taken: Default::default(), out: Vec::new(), write, exists, stop_on_error, copy: None }
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
        Ok(if self.o.subfolder.is_empty() { base } else { join_path(&base, &self.o.subfolder) })
    }

    /// Place photo `photo`'s export result (file `name`, its original in `source_dir`): write it
    /// and record the outcome. `Err` only when it failed and `stop_on_error` is set.
    pub(super) fn place(
        &mut self,
        photo: lightcraft_catalog::PhotoId,
        name: String,
        source_dir: Option<&str>,
        guard: &crate::originals::OriginalGuard,
        e: Result<Ran, String>,
    ) -> Result<(), String> {
        use serde_json::json;
        // a photo the batch leaves out (a video in a rendered format) still reports on its entry
        let mut original = None;
        let e = match e {
            Ok(Ran::Exported(e)) => Ok(e),
            Ok(Ran::Original { exported, source, remove_location }) => {
                original = Some((source, remove_location));
                Ok(exported)
            }
            Ok(Ran::Skipped(why)) => {
                self.out.push(json!({"photo": photo.0, "file": name, "warning": why}));
                return Ok(());
            }
            Err(err) => Err(err),
        };
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
        let copy = &mut self.copy;
        let mut byte_count = e.bytes.len() as u64;
        let written = path.and_then(|path| {
            let files = group(&path);
            // never over an original, whatever the conflict policy or the exact path said
            for f in &files {
                guard.check(std::path::Path::new(f))?;
            }
            // Keep the previous sidecars in memory so a failure after replacing some (or after
            // replacing the clip) can put the group back together instead of pairing a new clip
            // with old metadata, or the reverse.
            let previous: Vec<(String, Option<Vec<u8>>)> = files
                .iter()
                .skip(1)
                .filter_map(|sc| match std::fs::metadata(sc) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some((sc.clone(), None)),
                    // Only a small file is backed up; anything else is left as it is rather than
                    // removed by a later restore that cannot put it back.
                    Ok(m) if m.is_file() && m.len() <= MAX_SIDECAR_BACKUP => std::fs::read(sc).ok().map(|bytes| (sc.clone(), Some(bytes))),
                    _ => None,
                })
                .collect();
            // Sidecars first: a metadata write failure leaves the previous clip untouched.
            let mut sidecars = Vec::new();
            for ((_, bytes), sc) in e.sidecars.iter().zip(files.iter().skip(1)) {
                if let Err(e) = write(sc, bytes) {
                    restore_sidecars(&mut *write, &previous);
                    return Err(e);
                }
                sidecars.push(sc.clone());
            }
            let main: Result<(), String> = if let Some((source, remove_location)) = &original {
                let copier = copy.as_mut().ok_or("this destination cannot stream an Original video")?;
                copier(source, &path, *remove_location).map(|n| byte_count = n)
            } else {
                write(&path, &e.bytes)
            };
            if let Err(e) = main {
                restore_sidecars(&mut *write, &previous);
                return Err(e);
            }
            Ok((path, files, sidecars))
        });
        match written {
            Ok((path, files, sidecars)) => {
                self.taken.extend(files);
                let mut entry = json!({"path": path, "width": e.width, "height": e.height, "bytes": byte_count, "sidecars": sidecars});
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
