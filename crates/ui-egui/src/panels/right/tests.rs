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
