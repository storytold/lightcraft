// Feature: the History panel greys out the steps after the cursor
#[test]
fn history_rows_after_the_cursor_are_greyed() {
    use super::{HistoryRow, history_row};
    let rows: Vec<_> = (0..4).map(|i| history_row(i, Some(1))).collect();
    assert_eq!(rows, [HistoryRow::Done, HistoryRow::Current, HistoryRow::Undone, HistoryRow::Undone]);
    assert_eq!(history_row(3, Some(3)), HistoryRow::Current, "at the latest step nothing is greyed");
    assert_eq!(history_row(0, None), HistoryRow::Done);
}

// Feature: the aspect button tells what is locked
#[test]
fn aspect_label_names_presets_in_either_orientation() {
    assert_eq!(super::aspect_label(None, Some(1.5)), "Free");
    assert_eq!(super::aspect_label(Some((1600, 900)), None), "16 × 9");
    assert_eq!(super::aspect_label(Some((900, 1600)), None), "16 × 9", "portrait 16:9");
    assert_eq!(super::aspect_label(Some((400, 500)), None), "4 × 5 / 8 × 10");
}

#[test]
fn aspect_label_prefers_original_over_a_coinciding_preset_and_never_shows_pixel_sizes() {
    assert_eq!(super::aspect_label(Some((300, 200)), Some(1.5)), "Original");
    assert_eq!(super::aspect_label(Some((600_000, 400_000)), Some(1.5)), "Original");
    assert_eq!(super::aspect_label(Some((137, 100)), Some(1.5)), "1.37 : 1");
    assert_eq!(super::aspect_label(Some((0, 5)), None), "Free");
}

#[test]
fn short_times() {
    assert_eq!(super::short_time("2026-09-30T12:00:05"), "Sep 30, 2026, 12:00 PM");
    assert_eq!(super::short_time("2026-01-02"), "Jan 2, 2026");
}

/// Versions on the 24-hour clock (issue #3479): the same short date, minutes without AM/PM,
/// wall-clock digits kept; a value it can't read is shown as it is, not cut up.
#[test]
fn short_times_on_a_24_hour_clock() {
    let cases = [
        ("2026-09-30T12:00:05", "Sep 30, 2026, 12:00"),
        ("2026-09-30T00:05:00", "Sep 30, 2026, 00:05"),
        ("2026-09-30T23:59:59Z", "Sep 30, 2026, 23:59"),
        ("2026-09-30T23:59:59.75+02:00", "Sep 30, 2026, 23:59"),
        ("2026-09-30T18:30+02:00", "Sep 30, 2026, 18:30"),
        ("2026-09-30 18:30", "Sep 30, 2026, 18:30"),
        ("2026-01-02", "Jan 2, 2026"),
        ("2026-02-30 22:00", "2026-02-30 22:00"),
        ("2026-09-30T22", "2026-09-30T22"),
        ("2026-09-30+01:00", "2026-09-30+01:00"),
        ("", ""),
        ("über", "über"),
    ];
    for (iso, want) in cases {
        assert_eq!(super::short_time_with(iso, true), want, "{iso:?}");
    }
    assert_eq!(super::short_time_with("2026-09-30T12:00:05", false), "Sep 30, 2026, 12:00 PM");
    assert_eq!(super::short_time_with("2026-09-30T00:05:00", false), "Sep 30, 2026, 12:05 AM");
    assert_eq!(super::short_time_with("2026-09-30T18:30:00", false), "Sep 30, 2026, 6:30 PM");
}

/// …and the Versions list reads the setting (`i18n::set_24_hour_clock`, as each frame applies it).
#[test]
fn short_time_follows_the_24_hour_clock_setting() {
    crate::i18n::set_language(crate::i18n::Locale::En);
    crate::i18n::set_24_hour_clock(true);
    assert_eq!(super::short_time("2026-09-30T18:30:00"), "Sep 30, 2026, 18:30");
    crate::i18n::set_24_hour_clock(false);
    assert_eq!(super::short_time("2026-09-30T18:30:00"), "Sep 30, 2026, 6:30 PM");
}
