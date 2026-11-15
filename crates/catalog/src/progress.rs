//! Progress of a long catalog operation that runs while a library opens: the v3 → v4 migration,
//! or a v4 store's format bump after an update (see [`crate::library`]). Process-wide, so the
//! app's activity area and the CLI can show it from another thread while the opening thread
//! works; `None` when nothing is running.

use std::sync::{Mutex, PoisonError};

/// Which long operation is running: they spend their time on different things, so their phases
/// weigh differently in [`MigrationProgress::fraction`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MigrationKind {
    /// The v3 → v4 migration: reading the JSON library and writing the store is most of it.
    V3ToV4,
    /// A v4 store's format bump after an update: the byte-verified backup copy is nearly all of
    /// it.
    FormatBump,
}

/// Where a migration is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MigrationPhase {
    /// Copying the catalog's files to `backups/before-v<N> <time>/` (`done` of `total` bytes
    /// copied, both 0 until the sizes are known).
    Backup,
    /// Reading the v3 library (the JSON snapshot and the log).
    Reading,
    /// Encoding the photo records for the store.
    Encoding,
    /// Writing the records into the store (`done` of `total` photos).
    Writing,
    /// Committing the store, or marking it with this build's format.
    Finishing,
}

impl MigrationPhase {
    /// What the phase does, for people.
    pub fn label(self) -> &'static str {
        match self {
            MigrationPhase::Backup => "Backing up the catalog",
            MigrationPhase::Reading => "Reading the catalog",
            MigrationPhase::Encoding => "Preparing photo records",
            MigrationPhase::Writing => "Writing photo records",
            MigrationPhase::Finishing => "Finishing",
        }
    }
}

/// A snapshot of a running migration.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationProgress {
    /// The catalog folder being upgraded.
    pub library: String,
    /// Which operation is running.
    pub kind: MigrationKind,
    pub phase: MigrationPhase,
    /// How far the phase is: bytes copied (Backup) or photos written (Writing); 0 otherwise.
    pub done: u64,
    /// The phase's whole: bytes to copy (Backup) or photos in the library (Writing); 0 until
    /// known.
    pub total: u64,
}

impl MigrationProgress {
    /// 0..=1 over the whole operation.
    pub fn fraction(&self) -> f32 {
        let within = if self.total == 0 { 0.0 } else { (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32 };
        match self.kind {
            MigrationKind::V3ToV4 => match self.phase {
                MigrationPhase::Backup => 0.05 * within,
                MigrationPhase::Reading => 0.05,
                MigrationPhase::Encoding => 0.45,
                MigrationPhase::Writing => 0.6 + 0.35 * within,
                MigrationPhase::Finishing => 0.95,
            },
            // the backup copy is nearly the whole bump; the version mark after it is quick
            MigrationKind::FormatBump => match self.phase {
                MigrationPhase::Backup => 0.95 * within,
                _ => 0.95,
            },
        }
    }

    /// One line: `Writing photo records… 120000 of 500000 (78 %)`.
    pub fn describe(&self) -> String {
        let pct = (self.fraction() * 100.0).round();
        match self.phase {
            MigrationPhase::Writing => format!("{}… {} of {} ({pct} %)", self.phase.label(), self.done, self.total),
            _ => format!("{}… ({pct} %)", self.phase.label()),
        }
    }
}

static CURRENT: Mutex<Option<MigrationProgress>> = Mutex::new(None);

/// The migration running in this process, if any.
pub fn migration() -> Option<MigrationProgress> {
    CURRENT.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

pub(crate) fn set(p: Option<MigrationProgress>) {
    *CURRENT.lock().unwrap_or_else(PoisonError::into_inner) = p;
}

/// Move the running migration (if any) to `phase`.
pub(crate) fn phase(phase: MigrationPhase, done: u64, total: u64) {
    let mut cur = CURRENT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(p) = cur.as_mut() {
        p.phase = phase;
        p.done = done;
        p.total = total;
    }
}

/// How far the backup copy is (`done` of `total` bytes), while one runs.
pub(crate) fn bytes(done: u64, total: u64) {
    let mut cur = CURRENT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(p) = cur.as_mut()
        && p.phase == MigrationPhase::Backup
    {
        p.done = done;
        p.total = total;
    }
}

/// Whether a migration is running (only then do checkpoints and copies report their progress).
pub(crate) fn active() -> bool {
    CURRENT.lock().unwrap_or_else(PoisonError::into_inner).is_some()
}

/// Clears the progress when the migration ends, however it ends.
pub(crate) struct Running;

impl Running {
    pub(crate) fn start(kind: MigrationKind, library: String) -> Running {
        set(Some(MigrationProgress { library, kind, phase: MigrationPhase::Backup, done: 0, total: 0 }));
        Running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        set(None);
    }
}
