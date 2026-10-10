//! Progress of a long catalog operation that runs while a library opens: the v3 → v4 migration
//! (see [`crate::library`]). Process-wide, so the app's activity area and the CLI can show it
//! from another thread while the opening thread works; `None` when nothing is running.

use std::sync::{Mutex, PoisonError};

/// Where a migration is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MigrationPhase {
    /// Copying the v3 files to `backups/before-v4 <time>/`.
    Backup,
    /// Reading the v3 library (the JSON snapshot and the log).
    Reading,
    /// Encoding the photo records for the store.
    Encoding,
    /// Writing the records into the store (`done` of `total` photos).
    Writing,
    /// Committing the store and putting it in place.
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
    pub phase: MigrationPhase,
    /// Photos written so far (Writing), else 0.
    pub done: u64,
    /// Photos in the library (0 until known).
    pub total: u64,
}

impl MigrationProgress {
    /// 0..=1 over the whole migration (reading and writing are most of it).
    pub fn fraction(&self) -> f32 {
        let within = if self.total == 0 { 0.0 } else { (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32 };
        match self.phase {
            MigrationPhase::Backup => 0.0,
            MigrationPhase::Reading => 0.05,
            MigrationPhase::Encoding => 0.45,
            MigrationPhase::Writing => 0.6 + 0.35 * within,
            MigrationPhase::Finishing => 0.95,
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

/// Whether a migration is running (only then do checkpoints report their writes).
pub(crate) fn active() -> bool {
    CURRENT.lock().unwrap_or_else(PoisonError::into_inner).is_some()
}

/// Clears the progress when the migration ends, however it ends.
pub(crate) struct Running;

impl Running {
    pub(crate) fn start(library: String) -> Running {
        set(Some(MigrationProgress { library, phase: MigrationPhase::Backup, done: 0, total: 0 }));
        Running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        set(None);
    }
}
