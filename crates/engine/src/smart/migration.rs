//! Copy and verify first. The caller persists the location before asking for source cleanup.

use super::{Existing, Format};
use crate::activity::TaskHandle;
use std::path::{Path, PathBuf};

pub(crate) struct Prepared {
    files: Vec<(PathBuf, PathBuf)>,
    /// Sources that do not decode (cut short, say): left in the old folder and reported.
    skipped: Vec<String>,
    what: Existing,
    #[cfg(test)]
    from: PathBuf,
}

impl Prepared {
    /// Only called after the location is committed. A cleanup failure leaves a redundant
    /// source, never a missing destination. Retrying the move can remove redundant copies.
    pub(crate) fn cleanup(self, task: &TaskHandle) -> (usize, Vec<String>) {
        let copied = self.files.len();
        let total = copied + self.skipped.len();
        let (mut done, mut failed) = (0, self.skipped);
        for (index, (src, dst)) in self.files.into_iter().enumerate() {
            #[cfg(test)]
            super::tests_round3::checkpoint(&self.from, copied + index);
            #[cfg(not(test))]
            let _ = index;
            task.detail(&src.file_name().unwrap_or_default().to_string_lossy());
            let result = if self.what == Existing::Move && !super::is_usable(&dst) {
                Err(format!("{}: destination could not be verified; source retained", dst.display()))
            } else {
                std::fs::remove_file(&src).map_err(|e| format!("{}: {e}", src.display()))
            };
            match result {
                Ok(()) => done += 1,
                Err(e) => failed.push(e),
            }
            task.progress((done + failed.len()) as u64, total as u64);
        }
        (done, failed)
    }
}

fn destination(to: &Path, src: &Path) -> Result<PathBuf, String> {
    let name = src.file_name().ok_or("smart preview has no file name")?;
    Ok(name
        .to_str()
        .and_then(|n| n.strip_suffix(".lcsp"))
        .filter(|stem| stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        .map_or_else(|| to.join(name), |stem| to.join(stem.get(..2).unwrap_or_default()).join(name)))
}

/// No source is deleted in this phase, including on cancellation, error or process exit.
/// A retry reuses valid copies. Until the caller persists the new location, the old folder
/// remains complete. This deliberately avoids rename, even on the same volume.
pub(crate) fn prepare(from: &Path, to: &Path, what: Option<Existing>, task: &TaskHandle) -> Result<Option<Prepared>, String> {
    super::check_writable(to)?;
    if let Ok(old) = from.canonicalize() {
        let new = to.canonicalize().map_err(|e| e.to_string())?;
        if new.starts_with(&old) || old.starts_with(&new) {
            return Err("the smart preview folders overlap".into());
        }
    }
    let files = super::try_files(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let what = what
        .or_else(|| files.is_empty().then_some(Existing::Leave))
        .ok_or_else(|| format!("{} smart previews are in {}: pass existing = move, leave or discard", files.len(), from.display()))?;
    let mut prepared = Prepared {
        files: Vec::new(),
        skipped: Vec::new(),
        what,
        #[cfg(test)]
        from: from.to_path_buf(),
    };
    if what == Existing::Leave {
        return Ok(Some(prepared));
    }
    task.progress(0, files.len() as u64);
    for (index, src) in files.iter().enumerate() {
        #[cfg(test)]
        super::tests_round3::checkpoint(from, index);
        if task.is_cancelled() {
            return Ok(None);
        }
        task.detail(&src.file_name().unwrap_or_default().to_string_lossy());
        let dst = destination(to, src)?;
        if what == Existing::Move {
            if src == &dst {
                return Err("the smart preview folders overlap".into());
            }
            let parent = dst.parent().ok_or("smart preview has no parent folder")?;
            if !parent.is_dir() {
                std::fs::create_dir(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let keep = super::is_usable(&dst)
                && (!super::is_usable(src)
                    || super::header(&dst).map(|h| h.0) == super::header(src).map(|h| h.0)
                    || super::header(&dst).is_some_and(|h| h.0 == Format::Lossless));
            if !keep {
                let bytes = super::read(src)?;
                // A source that does not decode (cut short, say) can't be verified at the new
                // place. It stays in the old folder, which is no longer used, and is named in the
                // result; Build Smart Previews rebuilds it in the new folder from its original.
                if let Err(e) = super::decode(&bytes) {
                    prepared.skipped.push(format!("{}: {e}; left in the old folder", src.display()));
                    task.progress((index + 1) as u64, files.len() as u64);
                    continue;
                }
                lightcraft_catalog::safe_file::write_atomic(&dst, &bytes).map_err(|e| format!("{}: {e}", dst.display()))?;
                super::load(&dst)?;
            }
        }
        prepared.files.push((src.clone(), dst));
        task.progress((index + 1) as u64, files.len() as u64);
    }
    if task.is_cancelled() { Ok(None) } else { Ok(Some(prepared)) }
}
