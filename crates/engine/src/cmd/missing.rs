//! Missing originals: photos whose file is no longer where the library expects it (moved,
//! renamed, on an unplugged drive), and relinking them — one at a time (`photo.relink`) or by
//! searching a folder for files with the same name and size, or the same content
//! (`library.findMissing`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use lightcraft_catalog::{Catalog, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{Result, Session};

/// The file Missing Photos checks for `p`, if it is in scope: a library photo (not in Recently
/// Deleted, not a Local-only record seen while browsing a folder) whose original is a file.
///
/// Local browse records are out of scope on purpose: browsing can leave tens of thousands of
/// them in the catalog, they never show in library views (Missing Photos included), and a
/// folder that is gone is simply not browsed again. The sidebar count, the Missing Photos view,
/// `library.missing` and Find Missing Photos all use this one rule.
pub fn checked_path(p: &Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } if p.in_library() => Some(path),
        _ => None,
    }
}

/// Every in-scope photo's file ([`checked_path`]), without touching the disk.
pub fn candidates(cat: &Catalog) -> Vec<String> {
    cat.photos().filter_map(|p| checked_path(p).map(str::to_string)).collect()
}

/// [`missing`] with the existence check supplied (tests count the file-system calls).
pub fn missing_with(cat: &Catalog, mut exists: impl FnMut(&str) -> bool) -> Vec<(PhotoId, String)> {
    cat.photos().filter_map(|p| checked_path(p).filter(|f| !exists(f)).map(|f| (p.id, f.to_string()))).collect()
}

/// Library photos whose original file can't be found: (id, path).
pub fn missing(s: &Session) -> Vec<(PhotoId, String)> {
    if cfg!(target_arch = "wasm32") {
        return Vec::new();
    }
    let exists = s.media.availability.probe();
    missing_with(&s.catalog, |f| exists(f))
}

/// Every in-scope photo with its file ([`checked_path`]), without touching the disk: what a
/// sweep ([`sweep`]) goes through, so a caller can take the list on its own thread and check
/// the files on another.
pub fn checked(cat: &Catalog) -> Vec<(PhotoId, String)> {
    cat.photos().filter_map(|p| checked_path(p).map(|f| (p.id, f.to_string()))).collect()
}

fn fingerprint_of<'a>(entries: impl Iterator<Item = (PhotoId, &'a str)>) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    entries.fold(0u64, |sum, entry| {
        let mut h = DefaultHasher::new();
        entry.hash(&mut h);
        sum.wrapping_add(h.finish())
    })
}

/// What [`checked`] lists, as one number (the photos in scope and the file each one points at,
/// whatever their order), without building the list. A caller that took `checked` and checked its
/// files on another thread compares it with [`fingerprint_of_checked`] to know whether the photos
/// changed meanwhile.
pub fn fingerprint(cat: &Catalog) -> u64 {
    fingerprint_of(cat.photos().filter_map(|p| checked_path(p).map(|f| (p.id, f))))
}

/// [`fingerprint`] of the list [`checked`] gave.
pub fn fingerprint_of_checked(files: &[(PhotoId, String)]) -> u64 {
    fingerprint_of(files.iter().map(|(id, f)| (*id, f.as_str())))
}

/// What of `gone` (a sweep's answer, of the catalog as it was) still holds in `cat` as it is now: the
/// photos still in scope that still point at the file that was not found. A photo removed, moved to
/// Recently Deleted or relinked since is not reported as missing; one added since was not checked.
pub fn still_missing(cat: &Catalog, gone: Vec<(PhotoId, String)>) -> Vec<(PhotoId, String)> {
    gone.into_iter().filter(|(id, file)| cat.photo(*id).and_then(|p| checked_path(p)) == Some(file.as_str())).collect()
}

/// How many files [`sweep`] checks between two reports of its progress.
const PROGRESS_EVERY: usize = 64;

/// The files among `files` that `exists` doesn't find, in order: [`missing`] for a caller that
/// checks on a thread of its own. `stop` is asked before every check (`None`: it said yes, and
/// the rest was not looked at); `progress` is told how many files are done every few dozen.
pub fn sweep(
    files: Vec<(PhotoId, String)>,
    exists: impl Fn(&str) -> bool,
    stop: impl Fn() -> bool,
    progress: impl Fn(usize),
) -> Option<Vec<(PhotoId, String)>> {
    let mut gone = Vec::new();
    for (done, (id, file)) in files.into_iter().enumerate() {
        if stop() {
            return None;
        }
        if done % PROGRESS_EVERY == 0 {
            progress(done);
        }
        if !exists(&file) {
            gone.push((id, file));
        }
    }
    // (a stop that came during the last check is honoured too)
    (!stop()).then_some(gone)
}

/// `library.missing`'s answer: `[{id, path}]`.
pub fn missing_json(gone: Vec<(PhotoId, String)>) -> Value {
    Value::Array(gone.into_iter().map(|(id, path)| json!({"id": id.0, "path": path})).collect())
}

/// Whether photo `id` is in scope and its file is gone (the Missing Photos view checks only
/// the photos its query already narrowed to). In the app `avail` answers from its cache (files
/// not checked yet count as present until the background check finds them gone).
pub fn is_missing(cat: &Catalog, avail: &crate::availability::Availability, id: PhotoId) -> bool {
    !cfg!(target_arch = "wasm32") && cat.photo(id).and_then(|p| checked_path(p)).is_some_and(|f| avail.is_offline(f))
}

pub(crate) fn relink_op(id: PhotoId, path: &str) -> Op {
    let file_name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    Op::Relink { id, file_name, source: Source::File { path: path.to_string() }, format: None }
}

fn relink(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.relink";
    let id = super::photo_id(p, C)?.or(s.active()).ok_or_else(|| bad(C, "no photo (give `id`)"))?;
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let abs = std::path::absolute(path).map_err(|e| bad(C, e.to_string()))?;
    if !abs.is_file() {
        return Err(bad(C, format!("{path}: no such file")));
    }
    if !matches!(s.catalog.photo(id).map(|p| &p.source), Some(Source::File { .. })) {
        return Err(bad(C, "only photos from files can be relinked"));
    }
    let abs = abs.to_string_lossy().to_string();
    // the file may differ from the one imported (another export, a raw that decodes now or not):
    // its size, dimensions and preview-only state follow it
    let mut ops = vec![relink_op(id, &abs)];
    if s.media.file_probe.is_some()
        && let (Some(Ok(info)), Some(ph)) = (crate::import::probe_paths(s, std::slice::from_ref(&abs)).pop(), s.catalog.photo(id))
    {
        ops.extend(crate::cmd::convert::content_op(id, ph, info));
    }
    let op = match <[Op; 1]>::try_from(ops) {
        Ok([op]) => op,
        Err(ops) => Op::Batch { ops },
    };
    s.commit("Relink Photo", op)?;
    s.media.forget(id);
    Ok(json!({"id": id.0, "path": abs}))
}

/// The stored content hash of a photo, without the `:dupN` suffix duplicates get.
fn stored_hash(p: &Photo) -> Option<&str> {
    p.content_hash.as_deref().map(|h| h.split(':').next().unwrap_or(h)).filter(|h| !h.is_empty())
}

/// The content hash of the file at `path` (as import computes it), if it can be read. Read in
/// pieces: the file is whatever is on the disk, of any size.
fn file_hash(path: &str) -> Option<String> {
    use std::io::Read;
    // the opened file must be regular (a FIFO or device would never end), and no more than its size at open is read
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok().filter(|m| m.is_file())?;
    hash_reader(file.take(meta.len()))
}

/// The content hash of what `r` gives, read in pieces (the hash import computes of the whole).
fn hash_reader(mut r: impl std::io::Read) -> Option<String> {
    let mut hasher = lightcraft_preview::Hasher128::new();
    let mut piece = vec![0u8; 1 << 16];
    loop {
        match r.read(&mut piece) {
            Ok(0) => return Some(hasher.finish().to_string()),
            Ok(n) => {
                hasher.update(piece.get(..n)?);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that gives its bytes a few at a time, and an interruption in between.
    struct Trickle<'a>(&'a [u8], bool);

    impl std::io::Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1 = !self.1;
            if self.1 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let n = self.0.len().min(buf.len()).min(7);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn a_hash_read_in_pieces_is_the_hash_of_the_whole() {
        // longer than one piece, so the read loop runs more than once
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let whole = lightcraft_preview::hash_bytes(&bytes).to_string();
        assert_eq!(hash_reader(bytes.as_slice()).as_deref(), Some(whole.as_str()));
        assert_eq!(hash_reader(Trickle(&bytes[..1000], false)).as_deref(), Some(lightcraft_preview::hash_bytes(&bytes[..1000]).to_string().as_str()));
        assert_eq!(hash_reader(std::io::empty()).as_deref(), Some(lightcraft_preview::hash_bytes(&[]).to_string().as_str()));
    }

    /// A scratch folder that goes away with the test, however it ends.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_hash_is_the_import_hash_and_a_missing_file_has_none() {
        let dir = Scratch(std::env::temp_dir().join(format!("lc-file-hash-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&dir.0);
        std::fs::create_dir_all(&dir.0).unwrap();
        let f = dir.0.join("a.bin");
        std::fs::write(&f, b"pixels").unwrap();
        assert_eq!(file_hash(f.to_str().unwrap()), Some(lightcraft_preview::hash_bytes(b"pixels").to_string()));
        assert_eq!(file_hash(dir.0.join("nope").to_str().unwrap()), None);
        assert_eq!(file_hash(dir.0.to_str().unwrap()), None, "a folder has no content hash");
        #[cfg(unix)]
        assert_eq!(file_hash("/dev/zero"), None, "a device never ends");
    }
}

/// An in-scope photo ([`checked_path`]) as Find Missing Photos sees it: its file, the size and
/// content hash the library knows (0 / `None` = unknown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindCandidate {
    pub id: PhotoId,
    pub path: String,
    pub size: u64,
    pub hash: Option<String>,
    /// When the photo was taken, as the library knows it (from the file, or a Lightroom catalog).
    pub captured: Option<String>,
}

/// Every in-scope photo, for [`plan_find_missing`]: the missing ones are looked for, and no
/// photo's file is used for another. No file-system calls, so the app takes this snapshot on the
/// UI thread and plans on a worker without holding the session.
pub fn find_candidates(cat: &Catalog) -> Vec<FindCandidate> {
    cat.photos()
        .filter_map(|p| {
            checked_path(p).map(|f| FindCandidate {
                id: p.id,
                path: f.to_string(),
                size: p.file_size,
                hash: stored_hash(p).map(str::to_string),
                captured: p.captured.clone(),
            })
        })
        .collect()
}

/// How a missing photo's file was recognised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchBy {
    /// Same name and size (and content, when the library knows its hash).
    Name,
    /// Renamed: same content hash (and size, when the library knows it).
    Content,
    /// The same path on another drive ([`plan_other_drives`]).
    Drive,
}

impl MatchBy {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchBy::Name => "name",
            MatchBy::Content => "content",
            MatchBy::Drive => "drive",
        }
    }
}

/// A missing photo and the file found for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundFile {
    pub id: PhotoId,
    pub from: String,
    pub to: String,
    pub by: MatchBy,
}

/// A missing photo with several look-alike files (same name and size, no hash to tell them
/// apart): skipped, never guessed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmbiguousFile {
    pub id: PhotoId,
    pub from: String,
    pub candidates: Vec<String>,
}

/// What Find Missing Photos found ([`plan_find_missing`]), before relinking.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindPlan {
    pub found: Vec<FoundFile>,
    /// In-scope photos that stay missing (the ambiguous ones included).
    pub missing: usize,
    pub ambiguous: Vec<AmbiguousFile>,
}

impl FindPlan {
    /// `{found: [{id, from, to, by}], missing, ambiguous: [{id, from, candidates}]}` — the
    /// command's result, and what `library.findMissing {found, …}` takes back.
    pub fn to_json(&self) -> Value {
        let found: Vec<Value> = self.found.iter().map(|f| json!({"id": f.id.0, "from": f.from, "to": f.to, "by": f.by.as_str()})).collect();
        let ambiguous: Vec<Value> = self.ambiguous.iter().map(|a| json!({"id": a.id.0, "from": a.from, "candidates": a.candidates})).collect();
        json!({"found": found, "missing": self.missing, "ambiguous": ambiguous})
    }

    /// The inverse of [`FindPlan::to_json`] (entries it can't read are dropped).
    pub fn from_json(v: &Value) -> FindPlan {
        let list = |k: &str| v.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        let found = list("found")
            .iter()
            .filter_map(|f| {
                let by = match f.get("by").and_then(Value::as_str) {
                    Some("content") => MatchBy::Content,
                    Some("drive") => MatchBy::Drive,
                    _ => MatchBy::Name,
                };
                Some(FoundFile {
                    id: PhotoId(f.get("id")?.as_u64()?),
                    from: str_param(f, "from")?.to_string(),
                    to: str_param(f, "to")?.to_string(),
                    by,
                })
            })
            .collect();
        let ambiguous = list("ambiguous")
            .iter()
            .filter_map(|a| {
                let candidates = a.get("candidates")?.as_array()?.iter().filter_map(|c| c.as_str().map(str::to_string)).collect();
                Some(AmbiguousFile { id: PhotoId(a.get("id")?.as_u64()?), from: str_param(a, "from")?.to_string(), candidates })
            })
            .collect();
        let missing = v.get("missing").and_then(Value::as_u64).map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
        FindPlan { found, missing, ambiguous }
    }
}

/// How Find Missing compares file names: the part after the last `/` or `\`, in lower case. A
/// library made on Windows keeps Windows paths, whose `\` isn't a separator to `Path` on macOS or
/// Linux. Files on disk are named by the same rule, so a macOS or Linux file name that holds a
/// `\` (`a\b.jpg`) still meets its photo, and a `b.jpg` beside it makes that match ambiguous
/// rather than a guess. With no `a\b.jpg` there, such a photo may be found as `b.jpg`, if the
/// size on record agrees.
fn name_key(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or_default().to_lowercase()
}

/// The file-system half of Find Missing Photos: which `candidates` are gone, and which file in
/// `folder` (recursively) is each one's. Touches only the disk — never the session — so the app
/// runs it on a worker thread (walking a big folder on a network share and hashing look-alikes
/// takes a while) and relinks the result with `library.findMissing {found, missing, ambiguous}`.
///
/// Matching, per photo:
/// 1. a file with the same name ([`name_key`]: any letter case), the same size and the same
///    content hash;
/// 2. renamed: a file with the same content hash (and the same size, when one is on record; with
///    no size on record every file is read, but the hash alone confirms identity);
/// 3. without a hash to confirm (or the file was edited in place): the one file with the same
///    name and size. Several such files are ambiguous: the photo is skipped and reported in
///    `ambiguous`, never guessed (two cameras' `IMG_0001.CR3` of one size are different photos).
///
/// Files that are already some photo's original, or that another photo was just matched with,
/// are not used.
pub fn plan_find_missing(candidates: &[FindCandidate], folder: &str) -> std::result::Result<FindPlan, String> {
    if !Path::new(folder).is_dir() {
        return Err(format!("{folder}: not a folder"));
    }
    if cfg!(target_arch = "wasm32") {
        return Ok(FindPlan::default());
    }
    let lost: Vec<&FindCandidate> = candidates.iter().filter(|c| !Path::new(&c.path).exists()).collect();
    if lost.is_empty() {
        return Ok(FindPlan::default());
    }
    let in_use: HashSet<&str> = candidates.iter().map(|c| c.path.as_str()).collect();
    // file name (lower case) → candidate paths; size → paths (filled lazily)
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    let files: Vec<String> = crate::import::expand(&[folder.to_string()], None).into_iter().filter(|f| !in_use.contains(f.as_str())).collect();
    for f in &files {
        let name = name_key(f);
        if !name.is_empty() {
            by_name.entry(name).or_default().push(f.clone());
        }
    }
    let mut by_size: Option<HashMap<u64, Vec<String>>> = None;
    let mut hashes: HashMap<String, Option<String>> = HashMap::new();
    let mut hash_of = |f: &str| hashes.entry(f.to_string()).or_insert_with(|| file_hash(f)).clone();
    let size_of = |f: &str| std::fs::metadata(f).map(|m| m.len()).ok();
    let mut used: HashSet<String> = HashSet::new();
    let mut plan = FindPlan::default();
    for c in &lost {
        let name = name_key(&c.path);
        let size = c.size;
        let hash = c.hash.as_deref();
        let named: Vec<&String> = by_name
            .get(&name)
            .map(|v| v.iter().filter(|f| !used.contains(*f) && (size == 0 || size_of(f) == Some(size))).collect())
            .unwrap_or_default();
        // 1. same name and size, confirmed by the content hash
        let mut hit: Option<(String, MatchBy)> =
            hash.and_then(|h| named.iter().find(|f| hash_of(f).as_deref() == Some(h))).map(|f| (f.to_string(), MatchBy::Name));
        // 2. renamed: same content, and same size when one is on record
        if hit.is_none()
            && let Some(h) = hash
        {
            if size > 0 {
                let sizes = by_size.get_or_insert_with(|| {
                    let mut m: HashMap<u64, Vec<String>> = HashMap::new();
                    for f in &files {
                        if let Some(n) = size_of(f) {
                            m.entry(n).or_default().push(f.clone());
                        }
                    }
                    m
                });
                hit = sizes
                    .get(&size)
                    .and_then(|v| v.iter().find(|f| !used.contains(*f) && hash_of(f).as_deref() == Some(h)))
                    .map(|f| (f.clone(), MatchBy::Content));
            } else {
                hit = files.iter().find(|f| !used.contains(*f) && hash_of(f).as_deref() == Some(h)).map(|f| (f.clone(), MatchBy::Content));
            }
        }
        // 3. same name and size only (no hash, or the file was edited in place): one candidate
        //    is taken, several are ambiguous and skipped rather than guessed
        if hit.is_none() {
            match named.as_slice() {
                [one] => hit = Some(((*one).clone(), MatchBy::Name)),
                [] => {}
                _ => {
                    plan.ambiguous.push(AmbiguousFile { id: c.id, from: c.path.clone(), candidates: named.iter().map(|f| (*f).clone()).collect() });
                    continue;
                }
            }
        }
        let Some((path, by)) = hit else { continue };
        used.insert(path.clone());
        plan.found.push(FoundFile { id: c.id, from: c.path.clone(), to: path, by });
    }
    plan.missing = lost.len() - plan.found.len();
    Ok(plan)
}

/// Where `path` starts on its drive, and the rest of it: a Windows drive (`D:\`, `D:/`), or a mount point
/// (`/Volumes/<name>`, `/mnt/<name>`, `/media/<name>`, `/media/<user>/<name>` and `/run/media/<user>/<name>`,
/// `user` being the account's name), whether or not that drive is there now. `None` for other paths, and for a
/// drive root itself (no rest).
pub fn split_volume<'a>(path: &'a str, user: Option<&str>) -> Option<(&'a str, &'a str)> {
    let b = path.as_bytes();
    if b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        return Some((&path[..3], &path[3..]));
    }
    let parts: Vec<&str> = path.split('/').collect();
    // the number of leading components (after the empty one before the first `/`) that make the mount point
    let depth = match parts.get(1).copied() {
        Some("Volumes" | "mnt") => 2,
        Some("media") if user.is_some_and(|u| parts.get(2) == Some(&u)) => 3,
        Some("media") => 2,
        Some("run") if parts.get(2) == Some(&"media") => 4,
        _ => return None,
    };
    if !parts.first().is_some_and(|p| p.is_empty()) || parts.len() <= depth + 1 || parts[1..=depth].iter().any(|p| p.is_empty()) {
        return None;
    }
    let root_len: usize = parts[..=depth].iter().map(|p| p.len() + 1).sum::<usize>() - 1;
    let rest = &path[root_len + 1..];
    (!rest.is_empty()).then_some((&path[..root_len], rest))
}

/// The drives there are now, to look for missing files on ([`plan_other_drives`]): on Windows the drive letters
/// C: to Z: that exist; on macOS the volumes under `/Volumes`; elsewhere the mounts under `/media/<user>`,
/// `/run/media/<user>`, `/media` and `/mnt`.
pub fn drive_roots() -> Vec<String> {
    let mut roots = Vec::new();
    if cfg!(windows) {
        for letter in 'C'..='Z' {
            let root = format!("{letter}:\\");
            if Path::new(&root).is_dir() {
                roots.push(root);
            }
        }
        return roots;
    }
    let user = std::env::var("USER").ok().filter(|u| !u.is_empty());
    let parents: Vec<String> = if cfg!(target_os = "macos") {
        vec!["/Volumes".into()]
    } else {
        let mut v = Vec::new();
        if let Some(u) = &user {
            v.push(format!("/media/{u}"));
            v.push(format!("/run/media/{u}"));
        }
        v.extend(["/media".to_string(), "/mnt".to_string()]);
        v
    };
    for parent in parents {
        let Ok(dir) = std::fs::read_dir(&parent) else { continue };
        for entry in dir.flatten() {
            let path = entry.path();
            // `/media/<user>` is a parent of mounts, not a mount
            let user_dir = parent == "/media" && user.as_deref().is_some_and(|u| entry.file_name() == u);
            if path.is_dir() && !user_dir {
                roots.push(path.to_string_lossy().to_string());
            }
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// `rest` (a path below a drive, as [`split_volume`] gives it) on drive `root`, with the separators `root` uses.
fn on_drive(root: &str, rest: &str) -> String {
    let windows = root.len() >= 2 && root.as_bytes()[1] == b':' && root.as_bytes()[0].is_ascii_alphabetic();
    let rest = if windows { rest.replace('/', "\\") } else { rest.replace('\\', "/") };
    let sep = if windows { "\\" } else { "/" };
    format!("{}{sep}{rest}", root.trim_end_matches(['\\', '/']))
}

/// Whether `path` starts with a Windows drive (`D:\`, `d:/`).
fn windows_drive(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/')
}

/// Whether drive roots `a` and `b` are the same drive: Windows drive letters in any case, mount points exactly
/// (`/media/Photos` and `/media/photos` are two disks).
fn same_root(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_end_matches(['\\', '/']), b.trim_end_matches(['\\', '/']));
    if windows_drive(a) && windows_drive(b) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// The key two spellings of one file share: a Windows path (`E:/Photos/a.ARW`, `e:\photos\a.arw`) in one case
/// with `\`; any other path as it is.
pub fn path_key(path: &str) -> String {
    if windows_drive(path) { path.replace('/', "\\").to_lowercase() } else { path.to_string() }
}

/// The seconds of a capture time (`2024-05-01T12:34:56.78`, `2024:05:01 12:34:56`…) as its 14 digits; `None` when it
/// doesn't hold a date and a time to the second.
fn capture_key(t: &str) -> Option<String> {
    let digits: String = t.chars().filter(char::is_ascii_digit).take(14).collect();
    (digits.len() == 14).then_some(digits)
}

/// How many bytes of a file [`file_capture_time`] reads: a raw's or JPEG's Exif sits at its start.
const CAPTURE_PREFIX: u64 = 512 * 1024;

/// When the file at `path` was taken, from the Exif at its start ([`CAPTURE_PREFIX`] bytes are read, never the
/// whole file); `None` when it can't be read there.
pub fn file_capture_time(path: &str) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(CAPTURE_PREFIX).read_to_end(&mut bytes).ok()?;
    let m = match lightcraft_meta::embedded(&bytes).exif {
        Some(e) => lightcraft_meta::read_exif(&e),
        None => lightcraft_meta::read_exif(&bytes),
    };
    m.capture_time.map(|d| d.to_iso())
}

/// What [`plan_other_drives_with`] asks of the disk.
pub struct DriveIo<'a> {
    pub exists: &'a dyn Fn(&str) -> bool,
    pub is_file: &'a dyn Fn(&str) -> bool,
    pub size: &'a dyn Fn(&str) -> Option<u64>,
    pub captured: &'a dyn Fn(&str) -> Option<String>,
    pub hash: &'a dyn Fn(&str) -> Option<String>,
}

/// Find Missing Photos on other drives: each missing photo's file looked for at the same path on every other
/// drive in `roots` ([`split_volume`], [`drive_roots`]), so a library whose drive letter or mount changed, or that
/// has its photos on a copy of the drive, finds them without walking any folder. A file found there is another
/// photo, and skipped, when
/// 1. its size isn't the one on record (when there is one);
/// 2. the time it was taken, read from its first [`CAPTURE_PREFIX`] bytes, isn't the one on record, to the second
///    (when both are known);
/// 3. with `verify_hash`, its content hash isn't the one on record (when the library knows one; the whole file is
///    read).
///
/// What remains is taken only on positive evidence that it is this photo: the content hash confirmed, or a size
/// on record and a capture time known on both sides that both match. A file without that evidence (no size or
/// capture time to compare, no hash) is reported in `ambiguous` for the user to confirm, never relinked by itself.
/// Found on several drives, a file whose hash confirms it everywhere is taken from the first; otherwise the photo
/// is ambiguous too.
///
/// Files that are already some photo's original (in any spelling, [`path_key`]), or that another photo was just
/// matched with, are not used. With `only` (keys of [`path_key`]), just the missing photos at those paths are
/// looked for (after a Lightroom import, the originals it couldn't find); the files every photo uses still count.
/// Touches only the disk, so the app runs it on a worker thread and relinks with `library.findMissing {found, …}`.
pub fn plan_other_drives(candidates: &[FindCandidate], roots: &[String], verify_hash: bool, only: Option<&HashSet<String>>) -> FindPlan {
    if cfg!(target_arch = "wasm32") {
        return FindPlan::default();
    }
    let user = std::env::var("USER").ok().filter(|u| !u.is_empty());
    let io = DriveIo {
        exists: &|f| Path::new(f).exists(),
        is_file: &|f| Path::new(f).is_file(),
        size: &|f| std::fs::metadata(f).map(|m| m.len()).ok(),
        captured: &file_capture_time,
        hash: &file_hash,
    };
    plan_other_drives_with(candidates, roots, verify_hash, only, user.as_deref(), &io)
}

/// [`plan_other_drives`] with the disk and the account's name supplied (tests use a pretend disk).
pub fn plan_other_drives_with(
    candidates: &[FindCandidate],
    roots: &[String],
    verify_hash: bool,
    only: Option<&HashSet<String>>,
    user: Option<&str>,
    io: &DriveIo<'_>,
) -> FindPlan {
    let lost: Vec<&FindCandidate> =
        candidates.iter().filter(|c| only.is_none_or(|o| o.contains(&path_key(&c.path))) && !(io.exists)(&c.path)).collect();
    let in_use: HashSet<String> = candidates.iter().map(|c| path_key(&c.path)).collect();
    let mut used: HashSet<String> = HashSet::new();
    let mut plan = FindPlan::default();
    for c in &lost {
        let Some((root, rest)) = split_volume(&c.path, user) else { continue };
        let want_time = c.captured.as_deref().and_then(capture_key);
        let hash = c.hash.as_deref().filter(|_| verify_hash);
        let mut hits: Vec<Hit> = Vec::new();
        for r in roots {
            if same_root(r, root) {
                continue;
            }
            let file = on_drive(r, rest);
            let key = path_key(&file);
            if in_use.contains(&key) || used.contains(&key) || !(io.is_file)(&file) {
                continue;
            }
            // 1. a size on record must match
            let size_known = c.size > 0;
            if size_known && (io.size)(&file) != Some(c.size) {
                continue;
            }
            // 2. a capture time known on both sides must match, to the second
            let time_matches = match (&want_time, (io.captured)(&file).as_deref().and_then(capture_key)) {
                (Some(want), Some(got)) if &got != want => continue,
                (Some(_), Some(_)) => true,
                _ => false,
            };
            // 3. the content hash on record, when asked for and known, must match
            let hashed = match hash {
                Some(h) if (io.hash)(&file).as_deref() != Some(h) => continue,
                Some(_) => true,
                None => false,
            };
            hits.push(Hit { file, proven: hashed || (size_known && time_matches), hashed });
        }
        let pick = match hits.as_slice() {
            [] => None,
            [one] if one.proven => Some(one.file.clone()),
            // identical copies on several drives: the hash confirms each, take the first
            [first, ..] if hits.len() > 1 && hits.iter().all(|h| h.hashed) => Some(first.file.clone()),
            // without positive evidence, or several unconfirmed: for the user to confirm, never guessed
            _ => {
                plan.ambiguous.push(AmbiguousFile { id: c.id, from: c.path.clone(), candidates: hits.into_iter().map(|h| h.file).collect() });
                continue;
            }
        };
        if let Some(to) = pick {
            used.insert(path_key(&to));
            plan.found.push(FoundFile { id: c.id, from: c.path.clone(), to, by: MatchBy::Drive });
        }
    }
    plan.missing = lost.len() - plan.found.len();
    plan
}

/// A file at a missing photo's path on another drive that passed every check it could be put to: `proven` when
/// the hash, or both a size and a capture time, say it is this photo; `hashed` when the hash did.
struct Hit {
    file: String,
    proven: bool,
    hashed: bool,
}

/// The `paths` of a `findMissingOnOtherDrives` request, as [`path_key`]s (`None`: every missing photo).
pub fn only_paths(p: &Value) -> Option<HashSet<String>> {
    p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(path_key).collect())
}

/// `library.findMissingOnOtherDrives {hash?, paths?}`: [`plan_other_drives`] over [`drive_roots`], relinked in one undo step.
fn find_missing_on_other_drives(s: &mut Session, p: &Value) -> Result<Value> {
    let verify_hash = p.get("hash").and_then(Value::as_bool).unwrap_or(true);
    let plan = plan_other_drives(&find_candidates(&s.catalog), &drive_roots(), verify_hash, only_paths(p).as_ref());
    commit_find(s, plan)
}

/// Relink what [`plan_find_missing`] found, in one undoable step, re-checked against the
/// catalog as it is now (the search may have run on a worker while the library changed): a
/// photo that no longer points at the path it was missing from (relinked, removed) is left
/// alone, and a file that is now some photo's original — or that the plan names twice — is not
/// used (that photo stays missing). `missing` is the plan's count, corrected for what changed
/// among the photos the plan names.
fn commit_find(s: &mut Session, plan: FindPlan) -> Result<Value> {
    let still_at = |cat: &Catalog, id: PhotoId, from: &str| cat.photo(id).and_then(|p| checked_path(p)) == Some(from);
    let mut in_use: HashSet<String> = candidates(&s.catalog).iter().map(|c| path_key(c)).collect();
    let mut found = Vec::new();
    let mut missing = plan.missing;
    for f in plan.found {
        if !still_at(&s.catalog, f.id, &f.from) {
            continue;
        }
        if in_use.contains(&path_key(&f.to)) {
            missing += 1;
            continue;
        }
        in_use.insert(path_key(&f.to));
        found.push(f);
    }
    let planned = plan.ambiguous.len();
    let ambiguous: Vec<AmbiguousFile> = plan.ambiguous.into_iter().filter(|a| still_at(&s.catalog, a.id, &a.from)).collect();
    // ambiguous photos relinked meanwhile aren't missing any more
    missing = missing.saturating_sub(planned - ambiguous.len());
    if !found.is_empty() {
        s.commit("Find Missing Photos", Op::Batch { ops: found.iter().map(|f| relink_op(f.id, &f.to)).collect() })?;
        for f in &found {
            s.media.forget(f.id);
        }
    }
    Ok(FindPlan { found, missing, ambiguous }.to_json())
}

/// Search `folder` (recursively) for each missing photo's file ([`plan_find_missing`]) and
/// relink every match in one undoable step. With `found` instead (what a search on a worker
/// thread returned, as `{found, missing, ambiguous}`), only the relinking is done.
fn find_missing(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.findMissing";
    if p.get("found").is_some_and(Value::is_array) {
        return commit_find(s, FindPlan::from_json(p));
    }
    let folder = str_param(p, "folder").ok_or_else(|| bad(C, "missing `folder`"))?;
    let plan = plan_find_missing(&find_candidates(&s.catalog), folder).map_err(|e| bad(C, e))?;
    commit_find(s, plan)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "library.missing", "Missing Photos", [], None, "{} → [{id, path}] — photos whose original file isn't where the library expects it (every file is looked at: on a network share that takes minutes, so the desktop app, asked over its control channel, checks on a worker thread, with a row in the activity stack that can stop it, and answers when it is done)", always, |s, _| {
            Ok(missing_json(missing(s)))
        }),
        cmd!(
            "photo.relink",
            "Locate Photo",
            [],
            None,
            "{id?, path} — point a photo at its file's new location (undo never moves files)",
            always,
            relink
        ),
        cmd!(
            "library.findMissingOnOtherDrives",
            "Find Missing Photos on Other Drives",
            [],
            None,
            "{hash?, paths?} — relink every missing photo (with paths, only those at these paths) whose file is at the same path on another drive (another drive letter, or another mount under /Volumes, /media, /run/media or /mnt), when its size, and the capture time in its Exif, are the ones on record; with hash (default true) also its content hash, when the library knows one (the whole file is read) → {found: [{id, from, to, by: drive}], missing, ambiguous}",
            always,
            find_missing_on_other_drives
        ),
        cmd!(
            "library.findMissing",
            "Find Missing Photos",
            [],
            None,
            "{folder} — relink every missing photo whose file is somewhere in the folder: same name and size (and content, when the library knows its hash), or — renamed — same size and content; or {found, missing, ambiguous} — relink what a search already found (the app searches on a worker thread; photos relinked meanwhile and files now in use are skipped) → {found: [{id, from, to, by: name|content}], missing, ambiguous: [{id, from, candidates}] (several same-name candidates and no hash to tell them apart: skipped)}",
            always,
            find_missing
        ),
    ]
}

#[cfg(test)]
mod drive_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn a_path_splits_into_its_drive_and_the_rest() {
        assert_eq!(split_volume(r"D:\Photos\2020\a.ARW", None), Some((r"D:\", r"Photos\2020\a.ARW")));
        assert_eq!(split_volume("e:/Photos/a.ARW", None), Some(("e:/", "Photos/a.ARW")));
        assert_eq!(split_volume("/Volumes/Archive/2020/a.ARW", None), Some(("/Volumes/Archive", "2020/a.ARW")));
        assert_eq!(split_volume("/mnt/backup/2020/a.ARW", None), Some(("/mnt/backup", "2020/a.ARW")));
        assert_eq!(split_volume("/media/john/Disk/2020/a.ARW", Some("john")), Some(("/media/john/Disk", "2020/a.ARW")));
        assert_eq!(split_volume("/media/Disk/2020/a.ARW", Some("john")), Some(("/media/Disk", "2020/a.ARW")));
        assert_eq!(split_volume("/run/media/john/Disk/a.ARW", Some("john")), Some(("/run/media/john/Disk", "a.ARW")));
        // not on a drive we know how to swap, or no file below the drive
        assert_eq!(split_volume("/home/john/a.ARW", Some("john")), None);
        assert_eq!(split_volume(r"D:\", None), None);
        assert_eq!(split_volume("/mnt/backup", None), None);
        assert_eq!(split_volume("relative/a.ARW", None), None);
    }

    #[test]
    fn the_rest_takes_the_new_drives_separators() {
        assert_eq!(on_drive(r"E:\", r"Photos\2020\a.ARW"), r"E:\Photos\2020\a.ARW");
        assert_eq!(on_drive("/media/john/Disk", r"Photos\2020\a.ARW"), "/media/john/Disk/Photos/2020/a.ARW");
        assert_eq!(on_drive(r"F:\", "Photos/a.ARW"), r"F:\Photos\a.ARW");
    }

    /// A pretend disk: files with a size, a capture time and a hash.
    struct Disk(HashMap<String, (u64, Option<&'static str>, &'static str)>);

    impl Disk {
        fn plan(&self, candidates: &[FindCandidate], roots: &[&str], verify_hash: bool) -> FindPlan {
            self.plan_only(candidates, roots, verify_hash, None)
        }

        fn plan_only(&self, candidates: &[FindCandidate], roots: &[&str], verify_hash: bool, only: Option<&[&str]>) -> FindPlan {
            let roots: Vec<String> = roots.iter().map(|r| r.to_string()).collect();
            let only: Option<HashSet<String>> = only.map(|o| o.iter().map(|p| path_key(p)).collect());
            let io = DriveIo {
                exists: &|f| self.0.contains_key(f),
                is_file: &|f| self.0.contains_key(f),
                size: &|f| self.0.get(f).map(|e| e.0),
                captured: &|f| self.0.get(f).and_then(|e| e.1.map(str::to_string)),
                hash: &|f| self.0.get(f).map(|e| e.2.to_string()),
            };
            plan_other_drives_with(candidates, &roots, verify_hash, only.as_ref(), Some("john"), &io)
        }
    }

    fn photo(id: u64, path: &str, size: u64, captured: Option<&str>, hash: Option<&str>) -> FindCandidate {
        FindCandidate { id: PhotoId(id), path: path.into(), size, hash: hash.map(str::to_string), captured: captured.map(str::to_string) }
    }

    #[test]
    fn a_missing_photo_is_found_at_the_same_path_on_another_drive() {
        let disk = Disk(HashMap::from([
            (r"E:\Photos\2020\a.ARW".to_string(), (100, Some("2020:05:01 10:00:00"), "h1")),
            (r"E:\Photos\2020\b.ARW".to_string(), (100, None, "h2")),
        ]));
        let lost =
            [photo(1, r"D:\Photos\2020\a.ARW", 100, Some("2020-05-01T10:00:00.12"), None), photo(2, r"D:\Photos\2020\b.ARW", 100, None, Some("h2"))];
        let plan = disk.plan(&lost, &[r"C:\", r"E:\"], true);
        assert_eq!(
            plan.found.iter().map(|f| (f.id.0, f.to.as_str(), f.by)).collect::<Vec<_>>(),
            [(1, r"E:\Photos\2020\a.ARW", MatchBy::Drive), (2, r"E:\Photos\2020\b.ARW", MatchBy::Drive)]
        );
        assert_eq!(plan.missing, 0);
    }

    #[test]
    fn the_search_can_be_limited_to_some_photos_and_still_respects_every_photos_files() {
        let disk = Disk(HashMap::from([
            (r"E:\Import\a.ARW".to_string(), (100, None, "ha")),
            (r"E:\Old\b.ARW".to_string(), (100, None, "hb")),
            (r"E:\Import\c.ARW".to_string(), (100, None, "hc")),
        ]));
        let photos = [
            photo(1, r"D:\Import\a.ARW", 100, None, Some("ha")),
            // missing before the import: not in the scope, so left alone
            photo(2, r"D:\Old\b.ARW", 100, None, Some("hb")),
            // in the scope, but its file on E: is another photo's original (outside the scope)
            photo(3, r"D:\Import\c.ARW", 100, None, Some("hc")),
            photo(4, r"E:\Import\c.ARW", 100, None, None),
        ];
        let plan = disk.plan_only(&photos, &[r"E:\"], true, Some(&[r"d:/import/a.arw", r"D:\Import\c.ARW"]));
        assert_eq!(plan.found.iter().map(|f| f.id.0).collect::<Vec<_>>(), [1]);
        assert_eq!(plan.missing, 1, "photo 3 stays missing; photo 2 wasn't looked for");
        assert_eq!(disk.plan_only(&photos, &[r"E:\"], true, Some(&[])).missing, 0, "an empty scope looks for nothing");
    }

    #[test]
    fn mount_points_differing_in_case_are_different_disks() {
        let disk = Disk(HashMap::from([("/media/john/photos/2020/a.ARW".to_string(), (100, None, "h1"))]));
        let lost = [photo(1, "/media/john/Photos/2020/a.ARW", 100, None, Some("h1"))];
        let plan = disk.plan(&lost, &["/media/john/photos"], true);
        assert_eq!(plan.found.iter().map(|f| f.to.as_str()).collect::<Vec<_>>(), ["/media/john/photos/2020/a.ARW"]);
        // a Windows drive letter in another case is the same drive, not another one
        let disk = Disk(HashMap::from([(r"d:\2020\a.ARW".to_string(), (100, None, "h1"))]));
        let plan = disk.plan(&[photo(1, r"D:\2020\a.ARW", 100, None, Some("h1"))], &[r"d:\"], true);
        assert!(plan.found.is_empty(), "{:?}", plan.found);
    }

    #[test]
    fn a_file_in_use_under_another_spelling_is_not_taken() {
        let disk = Disk(HashMap::from([(r"E:\Photos\a.ARW".to_string(), (100, None, "h1"))]));
        // photo 2 has the same file, written with forward slashes and in lower case
        let photos = [photo(1, r"D:\Photos\a.ARW", 100, None, Some("h1")), photo(2, "e:/photos/a.arw", 100, None, None)];
        let plan = disk.plan(&photos, &[r"E:\"], true);
        assert!(plan.found.is_empty(), "{:?}", plan.found);
    }

    #[test]
    fn an_unverified_file_at_the_same_path_stays_missing() {
        // nothing contradicts these files, but nothing proves they are the photos either: no hash, and a size or
        // a capture time missing on one side
        let disk = Disk(HashMap::from([
            (r"E:\Import\a.ARW".to_string(), (100, None, "h1")),
            (r"E:\Import\b.ARW".to_string(), (100, Some("2020:05:01 10:00:00"), "h2")),
            (r"E:\Import\c.ARW".to_string(), (100, None, "h3")),
        ]));
        let lost = [
            photo(1, r"D:\Import\a.ARW", 100, None, None),
            // no size on record
            photo(2, r"D:\Import\b.ARW", 0, Some("2020:05:01 10:00:00"), None),
            // the file has no capture time to compare
            photo(3, r"D:\Import\c.ARW", 100, Some("2020:05:01 10:00:00"), None),
        ];
        for verify_hash in [true, false] {
            let plan = disk.plan(&lost, &[r"E:\"], verify_hash);
            assert!(plan.found.is_empty(), "{:?}", plan.found);
            assert_eq!(plan.missing, 3);
            // offered for the user to confirm, with the file found
            assert_eq!(
                plan.ambiguous.iter().map(|a| (a.id.0, a.candidates.clone())).collect::<Vec<_>>(),
                [(1, vec![r"E:\Import\a.ARW".to_string()]), (2, vec![r"E:\Import\b.ARW".to_string()]), (3, vec![r"E:\Import\c.ARW".to_string()])]
            );
        }
    }

    #[test]
    fn another_photo_at_the_same_path_is_left_alone() {
        // the same name, folder and size, but taken at another time, or with another content
        let disk = Disk(HashMap::from([
            (r"E:\Import\DSC00001.ARW".to_string(), (100, Some("2019:01:01 09:00:00"), "other")),
            (r"E:\Import\DSC00002.ARW".to_string(), (100, Some("2020:05:01 10:00:02"), "other")),
            (r"E:\Import\DSC00003.ARW".to_string(), (101, None, "h3")),
        ]));
        let lost = [
            photo(1, r"D:\Import\DSC00001.ARW", 100, Some("2020:05:01 10:00:01"), None),
            photo(2, r"D:\Import\DSC00002.ARW", 100, Some("2020:05:01 10:00:02"), Some("h2")),
            photo(3, r"D:\Import\DSC00003.ARW", 100, None, None),
        ];
        let plan = disk.plan(&lost, &[r"E:\"], true);
        assert!(plan.found.is_empty(), "{:?}", plan.found);
        assert_eq!(plan.missing, 3);
        // with the full check off, the hash isn't read: only the size and the time decide
        let plan = disk.plan(&lost, &[r"E:\"], false);
        assert_eq!(plan.found.iter().map(|f| f.id.0).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn copies_on_several_drives_are_taken_only_when_the_hash_confirms_them() {
        let disk = Disk(HashMap::from([
            (r"E:\Photos\a.ARW".to_string(), (100, None, "h1")),
            (r"F:\Photos\a.ARW".to_string(), (100, None, "h1")),
            (r"E:\Photos\b.ARW".to_string(), (100, None, "x")),
            (r"F:\Photos\b.ARW".to_string(), (100, None, "y")),
        ]));
        let lost = [photo(1, r"D:\Photos\a.ARW", 100, None, Some("h1")), photo(2, r"D:\Photos\b.ARW", 100, None, None)];
        let plan = disk.plan(&lost, &[r"E:\", r"F:\"], true);
        assert_eq!(plan.found.iter().map(|f| (f.id.0, f.to.as_str())).collect::<Vec<_>>(), [(1, r"E:\Photos\a.ARW")]);
        assert_eq!(plan.ambiguous.iter().map(|a| a.id.0).collect::<Vec<_>>(), [2], "two unconfirmed look-alikes: skipped, never guessed");
        assert_eq!(plan.missing, 1);
    }

    #[test]
    fn a_library_from_windows_finds_its_photos_on_a_linux_mount() {
        let disk = Disk(HashMap::from([("/media/john/Photos/2020/a.ARW".to_string(), (100, None, "h"))]));
        let plan = disk.plan(&[photo(1, r"D:\2020\a.ARW", 100, None, Some("h"))], &["/media/john/Photos"], true);
        assert_eq!(plan.found.iter().map(|f| f.to.as_str()).collect::<Vec<_>>(), ["/media/john/Photos/2020/a.ARW"]);
    }

    #[test]
    fn photos_still_there_and_files_in_use_are_not_touched() {
        let disk = Disk(HashMap::from([
            (r"D:\Photos\here.ARW".to_string(), (100, None, "h")),
            (r"E:\Photos\here.ARW".to_string(), (100, None, "h")),
            (r"E:\Photos\taken.ARW".to_string(), (100, None, "t")),
        ]));
        let photos = [
            photo(1, r"D:\Photos\here.ARW", 100, None, None),
            // another photo already points at E:'s file
            photo(2, r"E:\Photos\taken.ARW", 100, None, None),
            photo(3, r"D:\Photos\taken.ARW", 100, None, Some("t")),
        ];
        let plan = disk.plan(&photos, &[r"D:\", r"E:\"], true);
        assert!(plan.found.is_empty(), "{:?}", plan.found);
        assert_eq!(plan.missing, 1);
    }

    #[test]
    fn a_drive_match_round_trips_through_the_plan_the_app_hands_back() {
        let plan = FindPlan {
            found: vec![FoundFile { id: PhotoId(7), from: r"D:\a.ARW".into(), to: r"E:\a.ARW".into(), by: MatchBy::Drive }],
            missing: 2,
            ambiguous: Vec::new(),
        };
        assert_eq!(FindPlan::from_json(&plan.to_json()), plan);
    }

    #[test]
    fn the_capture_time_is_read_from_the_files_start() {
        // a real file: a TIFF-based raw's Exif, followed by more data than is read
        let dir = std::env::temp_dir().join(format!("lc-drives-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.ARW");
        let mut bytes = lightcraft_meta::write_exif(&lightcraft_meta::Metadata {
            capture_time: lightcraft_meta::DateTime::parse_iso("2021-06-07T08:09:10"),
            ..Default::default()
        });
        bytes.resize(bytes.len() + 2 * CAPTURE_PREFIX as usize, 0);
        std::fs::write(&path, &bytes).unwrap();
        let t = file_capture_time(&path.to_string_lossy());
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(t.as_deref().and_then(capture_key).as_deref(), Some("20210607080910"), "{t:?}");
    }
}
