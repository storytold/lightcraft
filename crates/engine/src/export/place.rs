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

fn join_path(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { format!("{}/{b}", a.trim_end_matches('/')) }
}

/// A subfolder that stays inside the destination folder (issue #2571): `\`, `:` and NUL become `_`
/// (so no drive, root or Windows separator), and empty, `.` and `..` parts are dropped; `a/b` still
/// nests. Applied where the folder is used, so presets and the Export dialog's field are covered too.
pub fn clean_subfolder(s: &str) -> String {
    let s = s.trim().replace(['\\', ':', '\0'], "_");
    s.split('/').map(str::trim).filter(|p| !p.is_empty() && *p != "." && *p != "..").collect::<Vec<_>>().join("/")
}

/// Clean an album name so it is safe to use as a directory or path component.
///
/// Characters `/ \ : * ? " < > | \0` and control characters become `_`, and surrounding
/// whitespace and dots are trimmed. Windows-reserved device names (`CON`, `PRN`, `AUX`, `NUL`,
/// `COM1`–`COM9`, `LPT1`–`LPT9`, plus superscript forms `COM¹`–`COM³`, `LPT¹`–`LPT³`, case-insensitive)
/// get `_` inserted after the base component so they remain usable directory components. The result
/// may be empty if the album name consisted entirely of dots or whitespace.
pub fn clean_album_name(album: &str) -> String {
    let mut s: String = album
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0') || c.is_control() { '_' } else { c })
        .collect();
    loop {
        let trimmed = s.trim().trim_matches('.');
        if trimmed == s {
            break;
        }
        s = trimmed.to_string();
    }
    let base = s.split('.').next().unwrap_or(&s);
    let upper = base.to_uppercase();
    if matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
            | "COM¹"
            | "COM²"
            | "COM³"
            | "LPT¹"
            | "LPT²"
            | "LPT³"
    ) {
        s.insert(base.len(), '_');
    }
    s
}

/// Expand `{album}`, `{date}`, `{hour}` variables in a subfolder template, and clean the result
/// so it stays safely inside the destination folder.
///
/// `{date}` and `{hour}` are expanded before `{album}`, so an album name that contains token-like
/// text (such as `{date}` or `{hour}`) is never double-expanded.
///
/// - `album`: the active album name, cleaned with [`clean_album_name`]; `None` expands `{album}` to `""`.
/// - `now`: the UTC timestamp for `{date}` (YYYYMMDD) and `{hour}` (HHMM); `None` uses current UTC time.
/// - Unknown tokens are left unchanged. The result passes through [`clean_subfolder`].
///
/// E.g. `"{album}/{date}_{hour}"` with album `"Vacation"` produces `"Vacation/20261107_0230"`.
///
/// Returns immediately without timestamp formatting or album lookup if `template` is empty or has no `{`.
pub fn expand_subfolder(template: &str, album: Option<&str>, now: Option<chrono::NaiveDateTime>) -> String {
    if template.is_empty() || !template.contains('{') {
        return clean_subfolder(template);
    }
    let now = now.unwrap_or_else(|| {
        let since = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default();
        let secs = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
        chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.naive_utc()).unwrap_or_default()
    });
    let date_str = now.format("%Y%m%d").to_string();
    let hour_str = now.format("%H%M").to_string();
    let album_str = album.map(clean_album_name).unwrap_or_default();

    let res = template.replace("{date}", &date_str).replace("{hour}", &hour_str).replace("{album}", &album_str);

    clean_subfolder(&res)
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
    fn folder_for(&self, name: &str, source_dir: Option<&str>, subfolder: Option<&str>) -> Result<String, String> {
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
        let sub = subfolder.map(clean_subfolder).unwrap_or_else(|| expand_subfolder(&self.o.subfolder, None, None));
        Ok(if sub.is_empty() { base } else { join_path(&base, &sub) })
    }

    /// Place photo `photo`'s export result (file `name`, its original in `source_dir`): write it
    /// and record the outcome. `Err` only when it failed and `stop_on_error` is set.
    pub(super) fn place(
        &mut self,
        photo: lightcraft_catalog::PhotoId,
        name: String,
        source_dir: Option<&str>,
        subfolder: Option<&str>,
        guard: &crate::originals::OriginalGuard,
        e: Result<Exported, String>,
    ) -> Result<(), String> {
        use serde_json::json;
        // an exact file path needs no folder
        let dir = if self.to.exact.is_some() && self.single { Ok(String::new()) } else { self.folder_for(&name, source_dir, subfolder) };
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
