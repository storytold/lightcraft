//! Test tooling shared by the workspace's tests. A dev-dependency only: `cargo xtask layers`
//! refuses it as a normal dependency. Std only, so every crate can use it at no build cost.
//!
//! **Skips.** libtest has no "skipped" status and hides the output of passing tests, so a test that
//! needs a GPU, a corpus file, the craft-fonts faces or a model, and returns early without them,
//! passes silently: "all steps passed" could mean the GPU tests never ran. Such a test calls
//! [`skip!`] before it returns:
//!
//! ```no_run
//! # let gpu_available = false;
//! if !gpu_available {
//!     lcc_testkit::skip!(Gpu, "no GPU adapter");
//!     return;
//! }
//! ```
//!
//! It prints the note to stderr as before and, when the [`skip_log_env`] variable names a file
//! (`cargo xtask ci` sets it), appends one JSON line there:
//! `{"need":"gpu","reason":"no GPU adapter","test":"equivalence::camera_tone","binary":"it","location":"crates/gpu/tests/it/equivalence.rs:22"}`.
//! `cargo xtask ci` lists the skips by need and reason in its summary and in
//! `target/ci/report.json`. A test that runs part of its checks (e.g. only the CPU half) records
//! the part it could not run the same way.
//!
//! **Temporary folders.** A test that writes files takes a [`TempDir`]: a folder of its own in the
//! system temp directory that is removed with everything in it when the test ends, passed or failed,
//! so test runs don't pile folders up in Temp.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

/// A test's own folder in the system temp directory, `<temp>/<name>-<process id>`: made empty when
/// it is made (an earlier run's is removed first) and removed with everything in it when dropped,
/// which a failed assert's unwinding does too. Derefs to the folder's [`Path`].
///
/// ```no_run
/// let dir = lcc_testkit::TempDir::new("lcc-example-out");
/// std::fs::write(dir.join("a.txt"), "x").unwrap();
/// // the folder is gone once `dir` goes out of scope
/// ```
#[derive(Debug)]
pub struct TempDir(std::path::PathBuf);

impl TempDir {
    /// The folder `<temp>/<name>-<process id>`, empty. A folder that can't be made shows in the
    /// test's first use of it.
    pub fn new(name: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::create_dir_all(&path);
        TempDir(path)
    }

    /// The folder.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        remove_dir_retrying(&self.0);
    }
}

/// How many times [`remove_dir_retrying`] tries.
pub const REMOVE_TRIES: u32 = 10;
/// The pause between two of [`remove_dir_retrying`]'s tries.
pub const REMOVE_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

/// Remove `dir` and everything in it. When that fails while the folder is still there, try again
/// up to [`REMOVE_TRIES`] times, [`REMOVE_PAUSE`] apart, then give up quietly. On Windows, a file
/// that a background thread of the test's app still has open (a library a test opened, for
/// example) can't be removed until that thread lets go, which is usually a moment after the app
/// itself is dropped.
pub fn remove_dir_retrying(dir: &Path) {
    for attempt in 1..=REMOVE_TRIES {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        if attempt < REMOVE_TRIES {
            std::thread::sleep(REMOVE_PAUSE);
        }
    }
}

/// How long [`remove_dir_settled`] waits for a folder to stay gone.
pub const SETTLE_QUIET: std::time::Duration = std::time::Duration::from_millis(500);
/// The longest [`remove_dir_settled`] keeps at it.
pub const SETTLE_MAX: std::time::Duration = std::time::Duration::from_secs(5);

/// Remove `dir` (as [`remove_dir_retrying`]), then keep watching it: if something writes it again
/// (a background job of an app the test has already dropped, such as a thumbnail render or a
/// cache record for a library the test opened), remove it again. Returns once the folder has
/// stayed away for [`SETTLE_QUIET`], or after [`SETTLE_MAX`] in all. For tests that open a library
/// in a temporary folder; costs [`SETTLE_QUIET`], so plain folders use [`TempDir`]'s drop.
pub fn remove_dir_settled(dir: &Path) {
    let start = std::time::Instant::now();
    let mut gone_since: Option<std::time::Instant> = None;
    while start.elapsed() < SETTLE_MAX {
        if dir.exists() {
            gone_since = None;
            let _ = std::fs::remove_dir_all(dir);
        } else {
            let since = *gone_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= SETTLE_QUIET {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// A file's path in a [`TempDir`] of its own, `<temp>/<folder>-<process id>/<file>`, for a test
/// that needs one file: the folder goes when this is dropped. Derefs to the file's [`Path`].
#[derive(Debug)]
pub struct TempFile {
    dir: TempDir,
    path: std::path::PathBuf,
}

impl TempFile {
    /// `file` in the empty folder `<temp>/<folder>-<process id>` (not created: the test writes it).
    pub fn new(folder: &str, file: &str) -> TempFile {
        let dir = TempDir::new(folder);
        let path = dir.join(file);
        TempFile { dir, path }
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder it is in.
    pub fn dir(&self) -> &TempDir {
        &self.dir
    }
}

impl std::ops::Deref for TempFile {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempFile {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for TempFile {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

/// The environment variable naming the file skips are appended to (JSON lines).
/// The skip-log variable's name: `<PREFIX>_SKIP_LOG` (`env_prefix` in `brand.toml`).
pub fn skip_log_env() -> String {
    lcc_brand::env_var("SKIP_LOG")
}

/// What a skipped test needed and didn't find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// A usable GPU adapter (`lcc_gpu::available()`).
    Gpu,
    /// A file from the gitignored `corpus/` (`cargo xtask corpus --download`) or a private corpus.
    Corpus,
    /// Faces from craft-fonts, embedded only in builds with `CRAFT_FONTS_DIR`.
    Fonts,
    /// A model file named by an environment variable (SAM 3, YuNet, …).
    Model,
    /// An input file named by an environment variable (opt-in tests).
    Input,
    /// Something the operating system provides (system ICC profiles, …).
    System,
    /// Support this build doesn't have yet (a decoder that refuses a file variant, an optional
    /// cargo feature that is off).
    Feature,
}

impl Need {
    /// The name in the skip log and the CI summary.
    pub fn as_str(self) -> &'static str {
        match self {
            Need::Gpu => "gpu",
            Need::Corpus => "corpus",
            Need::Fonts => "fonts",
            Need::Model => "model",
            Need::Input => "input",
            Need::System => "system",
            Need::Feature => "feature",
        }
    }
}

/// Record that this test (or part of it) can't run here: `skip!(Gpu, "no GPU adapter ({why})")`.
/// The first argument names a [`Need`] variant; the rest is a `format!` message. Doesn't return
/// from the test: follow it with `return` (or `continue`) as the test needs.
#[macro_export]
macro_rules! skip {
    ($need:ident, $($arg:tt)+) => {
        $crate::record($crate::Need::$need, &::std::format!($($arg)+), ::std::file!(), ::std::line!())
    };
}

/// What [`skip!`] calls: print the note and append it to the skip log, if one is set. Never fails
/// the test: a log that can't be written loses the entry, not the run.
pub fn record(need: Need, reason: &str, file: &str, line: u32) {
    eprintln!("skipped ({}): {reason}", need.as_str());
    let log = std::env::var_os(skip_log_env()).filter(|p| !p.is_empty());
    if let Some(log) = log {
        let thread = std::thread::current();
        let binary = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| binary_name(&s.to_string_lossy()).to_owned()));
        let entry = entry(need, reason, thread.name().unwrap_or(""), binary.as_deref().unwrap_or(""), file, line);
        append(Path::new(&log), &entry);
    }
}

/// Append one line in a single write, so lines from parallel tests don't interleave.
fn append(path: &Path, line: &str) {
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path);
    if let Err(e) = file.and_then(|mut f| f.write_all(line.as_bytes())) {
        eprintln!("{}: can't append to {}: {e}", skip_log_env(), path.display());
    }
}

/// A test binary's name without the hash cargo appends (`equivalence-0123abcd` → `equivalence`).
fn binary_name(stem: &str) -> &str {
    match stem.rsplit_once('-') {
        Some((name, hash)) if !name.is_empty() && hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => name,
        _ => stem,
    }
}

/// One skip-log line (JSON, newline-terminated).
fn entry(need: Need, reason: &str, test: &str, binary: &str, file: &str, line: u32) -> String {
    let mut s = String::from("{");
    for (i, (key, value)) in [("need", need.as_str()), ("reason", reason), ("test", test), ("binary", binary)].into_iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "\"{key}\":\"{}\"", escape(value));
    }
    let _ = writeln!(s, ",\"location\":\"{}:{line}\"}}", escape(&file.replace('\\', "/")));
    s
}

/// A JSON string's contents.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// A little-endian, uncompressed, 16-bit TIFF of `w`×`h` pixels with `spp` samples each (`px`
/// gives a pixel's samples), a `Software` tag of `software`, and `ExtraSamples` only when given.
pub fn tiff16(w: u32, h: u32, spp: u16, software: &str, extra_samples: Option<u16>, px: impl Fn(u32, u32) -> Vec<u16>) -> Vec<u8> {
    let mut soft = software.as_bytes().to_vec();
    soft.push(0);
    let mut entries: Vec<(u16, u16, u32, u32)> = vec![
        (256, 4, 1, w),
        (257, 4, 1, h),
        (258, 3, u32::from(spp), 0),
        (259, 3, 1, 1),
        (262, 3, 1, 2),
        (273, 4, 1, 0),
        (277, 3, 1, u32::from(spp)),
        (278, 4, 1, h),
        (279, 4, 1, w * h * u32::from(spp) * 2),
        (284, 3, 1, 1),
        (305, 2, soft.len() as u32, 0),
    ];
    if let Some(e) = extra_samples {
        entries.push((338, 3, 1, u32::from(e)));
    }
    entries.sort_by_key(|e| e.0);
    let ifd_len = 2 + entries.len() * 12 + 4;
    let bps_at = 8 + ifd_len as u32;
    let soft_at = bps_at + 2 * u32::from(spp);
    let strip_at = soft_at + soft.len() as u32;
    let mut b = b"II*\0\x08\0\0\0".to_vec();
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, kind, count, value) in entries {
        let value = match tag {
            258 => bps_at,
            273 => strip_at,
            305 => soft_at,
            _ => value,
        };
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&kind.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&value.to_le_bytes());
    }
    b.extend_from_slice(&[0; 4]);
    for _ in 0..spp {
        b.extend_from_slice(&16u16.to_le_bytes());
    }
    b.extend_from_slice(&soft);
    for y in 0..h {
        for x in 0..w {
            for v in px(x, y).iter().take(spp as usize) {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_one_json_line_with_its_text_escaped() {
        let line = entry(Need::Corpus, "skip: C:\\corpus\\raw \"absent\"\n\u{1}", "corpus::decodes", "it", "crates\\raw\\tests\\it\\corpus.rs", 28);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1, "{line}");
        assert_eq!(
            line,
            "{\"need\":\"corpus\",\"reason\":\"skip: C:\\\\corpus\\\\raw \\\"absent\\\"\\n\\u0001\",\"test\":\"corpus::decodes\",\
             \"binary\":\"it\",\"location\":\"crates/raw/tests/it/corpus.rs:28\"}\n"
        );
    }

    #[test]
    fn a_temp_dir_goes_with_its_contents_and_a_missing_one_is_no_error() {
        let dir = TempDir::new("lcc-testkit-tempdir");
        let path = dir.to_path_buf();
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("a").join("b").join("c.txt"), b"x").unwrap();
        drop(dir);
        assert!(!path.exists(), "removed with what is inside");
        let t0 = std::time::Instant::now();
        remove_dir_retrying(&path);
        assert!(t0.elapsed() < REMOVE_PAUSE, "a folder that is already gone takes no retries");
    }

    /// A file still open when the folder is dropped (Windows can't remove it then): the removal
    /// keeps trying, and the folder goes once the file is closed a moment later.
    #[test]
    fn removal_waits_for_a_file_closed_a_moment_later() {
        let dir = TempDir::new("lcc-testkit-tempdir-open");
        let path = dir.to_path_buf();
        let file = std::fs::File::create(dir.join("held.bin")).unwrap();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(REMOVE_PAUSE * 2);
            drop(file);
        });
        drop(dir);
        closer.join().unwrap();
        // on systems that remove open files at once the folder is gone anyway
        assert!(!path.exists());
    }

    /// A late writer (a background job of an app the test dropped) writes the folder again after
    /// it was removed: the settled removal removes it again and returns with it gone.
    #[test]
    fn a_settled_removal_outlasts_a_late_writer() {
        let dir = std::env::temp_dir().join(format!("lcc-testkit-settled-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("thumbs")).unwrap();
        let late = dir.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = std::fs::create_dir_all(late.join("thumbs").join("36"));
            let _ = std::fs::write(late.join("thumbs").join("36").join("x.lcthumb"), b"x");
        });
        remove_dir_settled(&dir);
        writer.join().unwrap();
        assert!(!dir.exists(), "written again after the first removal, and removed again");
    }

    #[test]
    fn binary_names_lose_cargos_hash_only() {
        assert_eq!(binary_name("equivalence-0f3a9c2b1d4e5f60"), "equivalence");
        assert_eq!(binary_name("lcc_engine-0f3a9c2b1d4e5f60"), "lcc_engine");
        assert_eq!(binary_name("sparse_corpus"), "sparse_corpus");
        assert_eq!(binary_name("cr3-corpus"), "cr3-corpus");
        assert_eq!(binary_name("-0f3a9c2b1d4e5f60"), "-0f3a9c2b1d4e5f60");
        assert_eq!(binary_name("tool-cafe"), "tool-cafe");
    }

    #[test]
    fn appended_lines_accumulate() {
        // Exclusive, private: the pid-derived name in a shared /tmp can be planted by another account
        // (CWE-59), so an existing directory fails the test instead of it symlinking inside.
        let dir = std::env::temp_dir().join(format!("lcc-testkit-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let log = dir.join("skips.jsonl");
        if std::fs::symlink_metadata(&log).is_ok() {
            std::fs::remove_file(&log).unwrap();
        }
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let log = log.clone();
                std::thread::spawn(move || append(&log, &entry(Need::Gpu, &format!("no GPU {i}"), "t", "b", "f.rs", i)))
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 8);
        assert!(text.lines().all(|l| l.starts_with("{\"need\":\"gpu\"") && l.ends_with('}')));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_log_directory_does_not_fail_the_test() {
        append(Path::new("/nonexistent-app-dir/skips.jsonl"), "{}\n");
    }
}
