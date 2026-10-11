//! When a library is backed up without being asked: by itself while it is open, and when it is
//! closed. Two choices of their own, both off unless made, saved with the library
//! (`library.backupPreferences`). The backup itself is `library.backup`'s
//! ([`crate::cmd::backup`]).

use serde::{Deserialize, Serialize};

/// How often a library is backed up by itself while it is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupEvery {
    #[default]
    Never,
    Day,
    Week,
    Month,
}

impl BackupEvery {
    pub const ALL: [BackupEvery; 4] = [BackupEvery::Never, BackupEvery::Day, BackupEvery::Week, BackupEvery::Month];

    /// Its id in commands and in the preferences file.
    pub const fn id(self) -> &'static str {
        match self {
            BackupEvery::Never => "never",
            BackupEvery::Day => "day",
            BackupEvery::Week => "week",
            BackupEvery::Month => "month",
        }
    }

    pub fn from_id(id: &str) -> Option<BackupEvery> {
        BackupEvery::ALL.into_iter().find(|e| e.id() == id)
    }

    /// The time from one backup to the next, in seconds (a month is thirty days). `None`: never.
    pub const fn seconds(self) -> Option<i64> {
        const DAY: i64 = 24 * 60 * 60;
        match self {
            BackupEvery::Never => None,
            BackupEvery::Day => Some(DAY),
            BackupEvery::Week => Some(7 * DAY),
            BackupEvery::Month => Some(30 * DAY),
        }
    }
}

/// Whether a library is backed up when it is closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupOnClose {
    #[default]
    Never,
    /// The person is asked each time.
    Ask,
    Always,
}

impl BackupOnClose {
    pub const ALL: [BackupOnClose; 3] = [BackupOnClose::Never, BackupOnClose::Ask, BackupOnClose::Always];

    pub const fn id(self) -> &'static str {
        match self {
            BackupOnClose::Never => "never",
            BackupOnClose::Ask => "ask",
            BackupOnClose::Always => "always",
        }
    }

    pub fn from_id(id: &str) -> Option<BackupOnClose> {
        BackupOnClose::ALL.into_iter().find(|c| c.id() == id)
    }
}

/// A library's backup schedule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackupSchedule {
    pub every: BackupEvery,
    pub on_close: BackupOnClose,
}

impl BackupSchedule {
    /// Nothing was chosen (and nothing needs saving).
    pub fn is_off(&self) -> bool {
        *self == BackupSchedule::default()
    }
}

/// How long a backup by itself that failed is left before it is tried again.
pub const RETRY_SECONDS: i64 = 60 * 60;

/// Whether a backup is due at `now`: the library was last backed up at `last` (never: `None`),
/// and a backup by itself was last tried at `tried` (not in this session: `None`). Times are the
/// session clock's (ISO 8601); one that can't be read is taken for long ago.
pub fn due(every: BackupEvery, last: Option<&str>, tried: Option<&str>, now: &str) -> bool {
    let Some(period) = every.seconds() else { return false };
    // (`now` has reached `from` + `seconds`; a time that can't be shifted is long ago)
    let reached = |from: &str, seconds: i64| lightcraft_catalog::dates::shift_iso(from, seconds).is_none_or(|then| now >= then.as_str());
    last.is_none_or(|last| reached(last, period)) && tried.is_none_or(|tried| reached(tried, RETRY_SECONDS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_is_due_a_period_after_the_last_and_an_hour_after_a_try() {
        let t = "2026-10-11T09:00:00";
        assert!(!due(BackupEvery::Never, None, None, t));
        assert!(due(BackupEvery::Day, None, None, t));
        assert!(!due(BackupEvery::Day, Some(t), None, "2026-10-12T08:59:59"));
        assert!(due(BackupEvery::Day, Some(t), None, "2026-10-12T09:00:00"));
        assert!(!due(BackupEvery::Week, Some(t), None, "2026-10-17T23:59:59"));
        assert!(due(BackupEvery::Month, Some(t), None, "2026-11-10T09:00:00"));
        // tried and failed: an hour's rest
        assert!(!due(BackupEvery::Day, None, Some(t), "2026-10-11T09:59:59"));
        assert!(due(BackupEvery::Day, None, Some(t), "2026-10-11T10:00:00"));
        // a time that can't be read is long ago, not never
        assert!(due(BackupEvery::Day, Some("yesterday"), None, t));
        // the clock set back: not due, and no panic
        assert!(!due(BackupEvery::Day, Some("2027-01-01T00:00:00"), None, t));
    }

    #[test]
    fn ids_read_back() {
        for e in BackupEvery::ALL {
            assert_eq!(BackupEvery::from_id(e.id()), Some(e));
            assert_eq!(serde_json::to_value(e).unwrap(), e.id());
        }
        for c in BackupOnClose::ALL {
            assert_eq!(BackupOnClose::from_id(c.id()), Some(c));
            assert_eq!(serde_json::to_value(c).unwrap(), c.id());
        }
    }
}
