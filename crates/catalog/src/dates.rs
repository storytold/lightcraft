//! Calendar helpers for ISO 8601 local times (`2026-09-30T14:05:00`): seconds ↔ civil date,
//! weekday and month names, shifting a time by an offset, and grouping a photo list into runs of
//! the same day / month / year (the date headers of the photo grid).

use chrono::Datelike;
use serde::{Deserialize, Serialize};

pub use crate::stacks::iso_seconds;
use crate::{Catalog, PhotoId, SortKey};

pub const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
pub const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Unix seconds → `YYYY-MM-DDTHH:MM:SS` (UTC, proleptic Gregorian). A time outside chrono's year
/// range still converts: the calendar repeats every 400 years, so the instant is moved into range
/// by whole cycles and only the year is carried (matching the old `civil_from_days`), rather than
/// resetting to the epoch. The callers here stay well inside years 0000–9999.
pub fn civil(secs: i64) -> String {
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (cycles, rest) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    // `rest + 719_163` (1970-01-01 is CE day 719_163) is always a valid CE day, and `sod` < 86_400.
    let date = i32::try_from(rest + 719_163).ok().and_then(chrono::NaiveDate::from_num_days_from_ce_opt);
    let time = u32::try_from(sod).ok().and_then(|s| chrono::NaiveTime::from_num_seconds_from_midnight_opt(s, 0));
    match (date, time) {
        (Some(d), Some(t)) => {
            let y = i128::from(d.year()) + i128::from(cycles) * 400;
            let y = i64::try_from(y).unwrap_or(if y < 0 { i64::MIN } else { i64::MAX });
            // `{:04}` rather than `%Y`, which writes a `+` before years past 9999 (the old `{y:04}` did not).
            format!("{y:04}-{}", d.and_time(t).format("%m-%dT%H:%M:%S"))
        }
        _ => "1970-01-01T00:00:00".to_string(), // unreachable
    }
}

/// Weekday name of an ISO date (`None` if it doesn't parse).
pub fn weekday(iso: &str) -> Option<&'static str> {
    let days = iso_seconds(iso.get(..10)?)?.div_euclid(86_400);
    let date = chrono::NaiveDate::from_num_days_from_ce_opt(i32::try_from(days.checked_add(719_163)?).ok()?)?;
    Some(WEEKDAYS[date.weekday().num_days_from_monday() as usize])
}

/// Shift an ISO time by `secs`, keeping anything after the seconds (fractions, zone) as it was.
/// A date without a time gets one. `None` if it doesn't parse, or if the result leaves years
/// 0000–9999 (an ISO date here has four year digits, so it couldn't be read back); `secs` may be
/// anything, it never overflows.
pub fn shift_iso(iso: &str, secs: i64) -> Option<String> {
    let base = iso_seconds(iso)?;
    // reject impossible dates that the arithmetic silently rolls over (2026-02-30)
    if civil(base).get(..10) != iso.trim().get(..10) {
        return None;
    }
    let t = base.checked_add(secs)?;
    let (first, last) = (iso_seconds("0000-01-01T00:00:00")?, iso_seconds("9999-12-31T23:59:59")?);
    if !(first..=last).contains(&t) {
        return None;
    }
    let iso = iso.trim();
    // the seconds are optional (`10:00+02:00`): without them the tail starts right after the minutes
    let tail = iso.get(if iso.get(16..17) == Some(":") { 19 } else { 16 }..).unwrap_or("");
    Some(format!("{}{tail}", civil(t)))
}

/// Normalize user input to `YYYY-MM-DDTHH:MM:SS` (accepts a space instead of `T`, missing
/// seconds or time). `None` if it isn't a valid date.
pub fn normalize_iso(s: &str) -> Option<String> {
    let t = iso_seconds(s)?;
    let out = civil(t);
    // reject impossible dates that the arithmetic silently rolls over (2026-02-30)
    if out.get(..10) != s.trim().get(..10) {
        return None;
    }
    Some(out)
}

/// Granularity of the grid's date headers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GroupBy {
    /// Day headers, or months/years when the thumbnails are small (decided by the UI).
    #[default]
    Auto,
    None,
    Day,
    Month,
    Year,
}

impl GroupBy {
    pub fn parse(s: &str) -> Option<GroupBy> {
        Some(match s.to_ascii_lowercase().as_str() {
            "auto" => GroupBy::Auto,
            "none" | "off" => GroupBy::None,
            "day" => GroupBy::Day,
            "month" => GroupBy::Month,
            "year" => GroupBy::Year,
            _ => return None,
        })
    }
    /// Length of the ISO prefix that identifies a group.
    fn prefix(self) -> usize {
        match self {
            GroupBy::Year => 4,
            GroupBy::Month => 7,
            _ => 10,
        }
    }
}

/// A run of consecutive photos with the same date (one header in the grid).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRun {
    /// `2026-09-30` / `2026-09` / `2026`, or empty for photos without a usable date.
    pub key: String,
    /// "Wednesday, 30 September 2026" / "September 2026" / "2026" / "Unknown Date".
    pub label: String,
    /// Index of the first photo in the list.
    pub start: usize,
    pub count: usize,
}

/// A capture time for display: `2022-03-30T22:11:11` → "March 30, 2022 at 10:11:11 PM" (the
/// date alone when there is no time; the input unchanged when it doesn't parse).
pub fn display_time(iso: &str) -> String {
    let num = |r: std::ops::Range<usize>| iso.get(r).and_then(|v| v.parse::<u32>().ok());
    let (Some(y), Some(mo), Some(d)) = (num(0..4), num(5..7), num(8..10)) else { return iso.to_string() };
    let Some(month) = MONTHS.get((mo as usize).wrapping_sub(1)) else { return iso.to_string() };
    let date = format!("{month} {d}, {y}");
    match (num(11..13), num(14..16), num(17..19)) {
        (Some(h), Some(mi), s) => {
            let (h12, ampm) = match h {
                0 => (12, "AM"),
                1..=11 => (h, "AM"),
                12 => (12, "PM"),
                _ => (h - 12, "PM"),
            };
            format!("{date} at {h12}:{mi:02}:{:02} {ampm}", s.unwrap_or(0))
        }
        _ => date,
    }
}

/// Header text for a group key.
pub fn group_label(key: &str) -> String {
    let month = |k: &str| k.get(5..7).and_then(|m| m.parse::<usize>().ok()).and_then(|m| MONTHS.get(m.wrapping_sub(1)));
    // `get` rather than `[..]`: a non-ASCII key must not split a character
    let year = key.get(..4).unwrap_or(key);
    match key.len() {
        10 => match (weekday(key), month(key), key.get(8..10).and_then(|d| d.parse::<u32>().ok())) {
            (Some(w), Some(m), Some(d)) => format!("{w}, {d} {m} {year}"),
            _ => key.to_string(),
        },
        7 => month(key).map(|m| format!("{m} {year}")).unwrap_or_else(|| key.to_string()),
        4 => key.to_string(),
        _ => "Unknown Date".into(),
    }
}

impl Catalog {
    /// Split `ids` (the grid order) into runs of the same day / month / year for `key`'s date.
    /// Members of a stack share the group of the stack's first photo in the list, so a stack is
    /// never split by a header. Empty for sort keys without a date and for [`GroupBy::None`]
    /// ([`GroupBy::Auto`] means days here).
    pub fn date_runs(&self, ids: &[PhotoId], key: SortKey, by: GroupBy) -> Vec<DateRun> {
        if !groups_by_date(key, by) || ids.is_empty() {
            return Vec::new();
        }
        date_runs_of(ids, &self.stack_index(), |_, id| self.date_group(id, key, by))
    }

    /// The date group of photo `id` on its own for [`Catalog::date_runs`] (`2026-09-30` /
    /// `2026-09` / `2026`; empty without a usable date): a stack's members take the group of its
    /// first photo in the list there.
    pub fn date_group(&self, id: PhotoId, key: SortKey, by: GroupBy) -> String {
        let Some(p) = self.photo(id) else { return String::new() };
        let d = match key {
            SortKey::CaptureDate => p.captured.as_deref(),
            SortKey::ImportDate => Some(p.imported.as_str()),
            SortKey::EditDate => p.edited.as_deref(),
            _ => None,
        };
        d.filter(|d| iso_seconds(d.get(..10).unwrap_or("")).is_some()).and_then(|d| d.get(..by.prefix())).unwrap_or("").to_string()
    }
}

/// Whether [`Catalog::date_runs`] has runs for sort key `key` grouped `by`.
pub fn groups_by_date(key: SortKey, by: GroupBy) -> bool {
    by != GroupBy::None && matches!(key, SortKey::CaptureDate | SortKey::ImportDate | SortKey::EditDate)
}

/// [`Catalog::date_runs`] from each photo's own group (`group(position, id)`, see
/// [`Catalog::date_group`]) and the stack index ([`Catalog::stack_index`]).
pub fn date_runs_of<K: AsRef<str>>(
    ids: &[PhotoId],
    stacks: &std::collections::HashMap<PhotoId, (crate::StackId, usize)>,
    mut group: impl FnMut(usize, PhotoId) -> K,
) -> Vec<DateRun> {
    let mut runs: Vec<DateRun> = Vec::new();
    let mut stack_key: std::collections::HashMap<crate::StackId, String> = Default::default();
    for (i, id) in ids.iter().enumerate() {
        let own;
        let k: &str = match stacks.get(id) {
            Some((sid, _)) => stack_key.entry(*sid).or_insert_with(|| group(i, *id).as_ref().to_string()),
            None => {
                own = group(i, *id);
                own.as_ref()
            }
        };
        match runs.last_mut() {
            Some(r) if r.key == k => r.count += 1,
            _ => runs.push(DateRun { label: group_label(k), key: k.to_string(), start: i, count: 1 }),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Op, Photo, Source};

    #[test]
    fn calendar() {
        assert_eq!(civil(0), "1970-01-01T00:00:00");
        assert_eq!(civil(951_782_400), "2000-02-29T00:00:00");
        // A year past 9999 keeps the plain zero-padded year (no `+`), as the old `{y:04}` did.
        assert_eq!(civil(253_402_300_800), "10000-01-01T00:00:00");
        // Beyond chrono's year range the old `civil_from_days` still converted; pin that output
        // (never the epoch) in both directions and at the i64 extremes.
        assert_eq!(civil(10_000_000_000_000), "318857-05-20T17:46:40");
        assert_eq!(civil(-10_000_000_000_000), "-314918-08-13T06:13:20");
        assert_eq!(civil(i64::MAX), "292277026596-12-04T15:30:07");
        assert_eq!(civil(i64::MIN), "-292277022657-01-27T08:29:52");
        assert_eq!(weekday("2026-09-30"), Some("Wednesday"));
        assert_eq!(weekday("2026-10-01T09:00:00"), Some("Thursday"));
        assert_eq!(weekday("2000-02-29"), Some("Tuesday"));
        assert_eq!(group_label("2026-09-29"), "Tuesday, 29 September 2026");
        assert_eq!(group_label("2026-02"), "February 2026");
        assert_eq!(group_label("2026"), "2026");
        assert_eq!(group_label(""), "Unknown Date");
        // non-ASCII keys of a date's length used to be sliced mid-character
        assert_eq!(group_label("2024-01é0"), "2024-01é0");
        assert_eq!(group_label("202é01"), "January 202é01");
        assert_eq!(shift_iso("2026-12-31T23:30:00", 3600).as_deref(), Some("2027-01-01T00:30:00"));
        assert_eq!(shift_iso("2026-03-01T00:00:00.25+02:00", -1).as_deref(), Some("2026-02-28T23:59:59.25+02:00"));
        assert_eq!(shift_iso("nope", 5), None);
        // hostile shifts never overflow, and a result outside years 0000–9999 (which no ISO date
        // here can read back) is refused rather than stored
        for secs in [i64::MAX, i64::MIN, i64::MAX / 2, -(i64::MAX / 2)] {
            assert_eq!(shift_iso("2026-01-01T00:00:00", secs), None, "{secs}");
        }
        assert_eq!(shift_iso("9999-12-31T23:59:59", 1), None);
        assert_eq!(shift_iso("0000-01-01T00:00:00", -1), None);
        assert_eq!(shift_iso("9999-12-31T23:59:58", 1).as_deref(), Some("9999-12-31T23:59:59"));
        assert_eq!(normalize_iso("2026-04-01 10:05").as_deref(), Some("2026-04-01T10:05:00"));
        assert_eq!(normalize_iso("2026-02-30"), None);
    }

    #[test]
    fn shift_refuses_impossible_dates() {
        assert_eq!(shift_iso("2026-02-30T10:00:00", 60), None);
        assert_eq!(shift_iso("2026-02-29T10:00:00", 0), None);
        assert_eq!(shift_iso("2026-04-31T10:00:00", 0), None);
        assert_eq!(shift_iso("2024-02-29T10:00:00", 0).as_deref(), Some("2024-02-29T10:00:00"));
    }

    #[test]
    fn shift_keeps_the_zone_of_a_time_without_seconds() {
        assert_eq!(shift_iso("2026-01-01T10:00+02:00", 3600).as_deref(), Some("2026-01-01T11:00:00+02:00"));
        assert_eq!(shift_iso("2026-01-01T10:00Z", 60).as_deref(), Some("2026-01-01T10:01:00Z"));
        assert_eq!(shift_iso("2026-01-01T10:00", 3600).as_deref(), Some("2026-01-01T11:00:00"));
        assert_eq!(shift_iso("2026-01-01T10:00:00.5-05:00", 3600).as_deref(), Some("2026-01-01T11:00:00.5-05:00"));
        assert_eq!(iso_seconds("2026-01-01T10:00+02:00"), iso_seconds("2026-01-01T10:00"));
    }

    #[test]
    fn impossible_times_are_rejected_not_rolled_over() {
        for bad in ["2026-04-01T10:75:00", "2026-04-01 10:00:75", "2026-04-01T10:60", "2026-04-01T24:00:00", "2026-04-01T10:59:60"] {
            assert_eq!(normalize_iso(bad), None, "{bad}");
            assert_eq!(shift_iso(bad, 5), None, "{bad}");
        }
        assert_eq!(normalize_iso("2026-04-01T23:59:59").as_deref(), Some("2026-04-01T23:59:59"));
    }

    #[test]
    fn runs_by_day_month_year_keep_stacks_together() {
        let mut c = Catalog::new();
        let times = ["2026-09-30T18:00:00", "2026-09-30T09:00:00", "2026-09-29T23:59:00", "2026-08-01T10:00:00", "2025-12-31T10:00:00", ""];
        let mut ids = Vec::new();
        for t in times {
            let id = c.alloc_photo_id();
            let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a.jpg", "JPEG", 3, 2, "2026-10-01T00:00:00");
            p.captured = (!t.is_empty()).then(|| t.to_string());
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            ids.push(id);
        }
        let shape = |r: &[DateRun]| r.iter().map(|r| (r.key.clone(), r.start, r.count)).collect::<Vec<_>>();
        let day = c.date_runs(&ids, SortKey::CaptureDate, GroupBy::Day);
        // the undated photo is grouped under Unknown Date
        assert_eq!(
            shape(&day),
            vec![
                ("2026-09-30".into(), 0, 2),
                ("2026-09-29".into(), 2, 1),
                ("2026-08-01".into(), 3, 1),
                ("2025-12-31".into(), 4, 1),
                (String::new(), 5, 1)
            ]
        );
        assert_eq!(day[0].label, "Wednesday, 30 September 2026");
        assert_eq!(day[4].label, "Unknown Date");
        assert_eq!(c.date_runs(&ids, SortKey::CaptureDate, GroupBy::Auto), day);
        assert_eq!(
            shape(&c.date_runs(&ids, SortKey::CaptureDate, GroupBy::Month)),
            vec![("2026-09".into(), 0, 3), ("2026-08".into(), 3, 1), ("2025-12".into(), 4, 1), (String::new(), 5, 1)]
        );
        assert_eq!(
            shape(&c.date_runs(&ids, SortKey::CaptureDate, GroupBy::Year)),
            vec![("2026".into(), 0, 4), ("2025".into(), 4, 1), (String::new(), 5, 1)]
        );
        assert!(c.date_runs(&ids, SortKey::FileName, GroupBy::Day).is_empty());
        assert!(c.date_runs(&ids, SortKey::CaptureDate, GroupBy::None).is_empty());
        // edit date: unedited photos have no date
        let r = c.date_runs(&ids, SortKey::EditDate, GroupBy::Day);
        assert_eq!(shape(&r), vec![(String::new(), 0, 6)]);
        assert_eq!(r[0].label, "Unknown Date");
        // a stack spanning midnight stays in its first member's group
        let op = c.group_ops(ids[1], &[ids[1], ids[2]], false).unwrap();
        c.apply(op).unwrap();
        let arranged = c.arrange_stacks(&ids[..5]);
        assert_eq!(
            shape(&c.date_runs(&arranged, SortKey::CaptureDate, GroupBy::Day))[..2],
            [("2026-09-30".into(), 0, 3), ("2026-08-01".into(), 3, 1)]
        );
    }
}
