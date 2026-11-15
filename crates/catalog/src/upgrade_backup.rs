//! Copy first, then upgrade: before a library's catalog is rewritten in a newer format (the v3 →
//! v4 migration, or a v4 store marked with this build's format version), the files an older
//! build reads are copied to `backups/before-v<N> <time>/` and checked byte for byte. Only then
//! is the library itself upgraded, so the version that made it can still open the copy, and the
//! user can go back by opening that folder with it (or copying its files back).
//!
//! The copy is made in a hidden `.before-v<N> <time>.partial` folder that is renamed into place
//! only once every file is copied, synced and verified. A failure on the way (a full disk, an
//! unwritable folder, a source that changed while it was read) removes the partial folder and
//! returns an error that says the library was left unchanged and what to do; a crash leaves only
//! the partial folder, which the next upgrade removes. Nothing in the library is written before
//! the copy is complete.
//!
//! Sources: own design (after OneSlips' "upgraded copy" of an older library).

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::library::{BACKUPS_DIR, SETTINGS_FILE, find_entry, now_secs};
use crate::{CatalogError, Result};

/// The file a kept copy carries to say what it is and how to go back.
pub const README: &str = "README.txt";
/// Copy buffer and compare chunk.
const CHUNK: usize = 1 << 20;
/// Leftover partial folders removed per upgrade (bounded: a hostile backups folder stays cheap).
const MAX_SCAN: usize = 10_000;

/// Test-only fault injection: what the next copy runs into.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Fault {
    /// The disk fills up once this many bytes of the copy are written.
    DiskFull { after: u64 },
    /// The process dies after the first file is copied: the partial folder stays behind.
    Crash,
}

#[cfg(test)]
thread_local! {
    pub(crate) static FAULT: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn fault() -> Option<Fault> {
    FAULT.with(std::cell::Cell::get)
}

/// Copy `files` of the library in `dir` (names relative to it; the ones that don't exist are
/// skipped), plus its entry point and catalog settings, to a new verified backup folder before the
/// catalog is upgraded from format `from` to `to`. Returns the folder.
pub(crate) fn keep_copy(dir: &Path, from: u32, to: u32, files: &[&str]) -> Result<PathBuf> {
    keep_copy_inner(dir, from, to, files).map_err(|e| {
        let full = e.kind() == io::ErrorKind::StorageFull || e.raw_os_error() == Some(28);
        let fix = if full { "free some disk space" } else { "make sure the library's folder can be written to and has free space" };
        CatalogError::Io(format!(
            "can't keep a copy of the library before upgrading its catalog from format v{from} to v{to} ({e}). The library was left \
             unchanged and still opens in the version that made it: {fix}, then open it again"
        ))
    })
}

fn keep_copy_inner(dir: &Path, from: u32, to: u32, files: &[&str]) -> io::Result<PathBuf> {
    let root = dir.join(BACKUPS_DIR);
    std::fs::create_dir_all(&root)?;
    remove_partials(&root);
    let stamp = crate::library::stamp(now_secs());
    let mut name = format!("before-v{to} {stamp}");
    for n in 2..1000 {
        if !root.join(&name).exists() && !root.join(format!(".{name}.partial")).exists() {
            break;
        }
        name = format!("before-v{to} {stamp}-{n}");
    }
    let partial = root.join(format!(".{name}.partial"));
    let done = root.join(&name);
    std::fs::create_dir(&partial)?;
    match fill(dir, &partial, from, to, files) {
        Ok(()) => {}
        // a crash: nothing runs after it, the partial folder stays
        Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(e),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(e);
        }
    }
    if let Err(e) = std::fs::rename(&partial, &done) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    crate::safe_file::sync_dir(&root);
    log::info!("catalog: kept a copy of {} (format v{from}) in {} before upgrading it to v{to}", dir.display(), done.display());
    Ok(done)
}

/// Copy and verify every file into `to_dir`, then write the README. Byte progress goes to
/// [`crate::progress`] while a catalog operation that shows it is running.
fn fill(dir: &Path, to_dir: &Path, from: u32, to: u32, files: &[&str]) -> io::Result<()> {
    let mut names: Vec<String> = files.iter().map(|f| (*f).to_string()).collect();
    if let Some(entry) = find_entry(dir).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())) {
        names.push(entry);
    }
    names.push(SETTINGS_FILE.to_string());
    let mut written = 0u64;
    let total = names.iter().map(|n| dir.join(n)).filter(|p| p.is_file()).map(|p| p.metadata().map(|m| m.len()).unwrap_or(0)).sum();
    for (i, name) in names.iter().enumerate() {
        let src = dir.join(name);
        if !src.is_file() {
            continue;
        }
        copy_verified(&src, &to_dir.join(name), written, total, &mut written)?;
        #[cfg(test)]
        if i == 0 && fault() == Some(Fault::Crash) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "simulated crash"));
        }
        let _ = i;
    }
    let text = format!(
        "This folder keeps the library's catalog as it was before it was upgraded from format v{from} to v{to}.\n\
         The version of the app that made the library can open this folder as a library.\n\
         To go back: quit the app, then copy these files over the ones in the library's folder\n\
         (or open this folder in the older version). Edits made after the upgrade are not in it.\n"
    );
    let mut f = File::create(to_dir.join(README))?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    crate::safe_file::sync_dir(to_dir);
    Ok(())
}

/// Copy `src` to `dst`, sync it, then read both back and compare. `written`/`total` say how many
/// of the copy's bytes are done (reported to [`crate::progress`] while something is running, so
/// the interface can show a moving bar over a store of gigabytes).
fn copy_verified(src: &Path, dst: &Path, done: u64, total: u64, written: &mut u64) -> io::Result<()> {
    let report = crate::progress::active();
    let mut input = File::open(src)?;
    let mut out = File::create(dst)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = buf.get(..n).unwrap_or_default();
        #[cfg(test)]
        if let Some(Fault::DiskFull { after }) = fault()
            && written.saturating_add(n as u64) > after
        {
            let room = usize::try_from(after.saturating_sub(*written)).unwrap_or(0).min(n);
            out.write_all(chunk.get(..room).unwrap_or_default())?;
            return Err(io::Error::new(io::ErrorKind::StorageFull, "No space left on device (simulated)"));
        }
        out.write_all(chunk)?;
        *written = written.saturating_add(n as u64);
        if report {
            crate::progress::bytes(done.saturating_add(*written), total);
        }
    }
    out.sync_all()?;
    drop(out);
    if !same_bytes(src, dst)? {
        return Err(io::Error::other(format!("the copy of {} doesn't match it (was it changed meanwhile?)", src.display())));
    }
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba.get(..n) != bb.get(..m) {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let Some(rest) = buf.get_mut(n..) else { break };
        let k = f.read(rest)?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

/// Remove the partial copies an earlier upgrade left when it was cut short.
fn remove_partials(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.take(MAX_SCAN).flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(".before-v") && name.ends_with(".partial") && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}
