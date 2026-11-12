//! Library commands: view source, filter/sort, selection, ratings/flags/labels, rotate, delete,
//! metadata, albums, import.

use lightcraft_catalog::{Album, AlbumId, ColorLabel, CopyrightStatus, Filter, Flag, GroupBy, Op, PhotoId, Sort, SortKey};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, has_active, has_library_selection, has_selection, ok, str_param};
use crate::{LibrarySource, Result, Selection, Session};

mod filters;
mod gps;
mod import;
mod selection;
mod specs;

pub(crate) use filters::covers_other_disks;
pub use import::{ImportRequest, import_batch_done, import_params, list_auto_import_folder};
pub fn specs() -> Vec<CommandSpec> {
    let mut v = vec![
        cmd!("library.nextUnrated", "Next Unrated Photo", [], Some("Cmd+Right"), "{}", always, |s, _| step_unrated(s, 1)),
        cmd!("library.prevUnrated", "Previous Unrated Photo", [], Some("Cmd+Left"), "{}", always, |s, _| step_unrated(s, -1)),
    ];
    v.extend(specs::specs());
    v
}

fn step_unrated(s: &mut Session, d: isize) -> Result<serde_json::Value> {
    let vis = s.visible_cloned();
    if vis.is_empty() {
        return ok();
    }
    let is_unrated = |id: &PhotoId| s.catalog.photo(*id).is_some_and(|p| p.rating == 0);
    if !vis.iter().any(&is_unrated) {
        return Ok(json!({"allRated": true}));
    }
    let active = s.active();
    let next = active
        .and_then(|a| {
            let cur = vis.iter().position(|x| *x == a)?;
            if d >= 0 {
                vis.iter()
                    .enumerate()
                    .find(|(pos, id)| *pos > cur && is_unrated(id))
                    .map(|(_, id)| *id)
                    .or_else(|| vis.iter().enumerate().find(|(pos, id)| *pos <= cur && is_unrated(id)).map(|(_, id)| *id))
            } else {
                vis.iter()
                    .enumerate()
                    .rev()
                    .find(|(pos, id)| *pos < cur && is_unrated(id))
                    .map(|(_, id)| *id)
                    .or_else(|| vis.iter().enumerate().rev().find(|(pos, id)| *pos >= cur && is_unrated(id)).map(|(_, id)| *id))
            }
        })
        .or_else(|| if d >= 0 { vis.iter().find(|id| is_unrated(id)).copied() } else { vis.iter().rev().find(|id| is_unrated(id)).copied() });
    let Some(id) = next else {
        return Ok(json!({"allRated": true}));
    };
    if active == Some(id) {
        return Ok(json!({"active": id.0}));
    }
    s.end_interaction()?;
    if s.selection.ids.len() > 1 {
        s.selection.active = Some(id);
    } else {
        s.selection = Selection::single(id);
    }
    s.active_mask = None;
    s.active_spot = None;
    Ok(json!({"active": id.0}))
}

#[cfg(test)]
#[path = "library/tests_relative_ratings.rs"]
mod relative_rating_tests;

#[cfg(test)]
#[path = "library/tests_flag_cycle.rs"]
mod flag_cycle_tests;

#[cfg(test)]
#[path = "library/tests_rated_out.rs"]
mod rated_out_tests;

#[cfg(test)]
#[path = "library/tests_clear_label.rs"]
mod clear_label_tests;

#[cfg(test)]
#[path = "library/tests_enable_filters.rs"]
mod enable_filters_tests;

#[cfg(test)]
#[path = "library/tests_source_album_filter.rs"]
mod source_album_filter_tests;

#[cfg(test)]
mod next_unrated_tests {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    use serde_json::json;

    use crate::{Result, Session};

    fn session() -> Result<Session> {
        let mut s = Session::new();
        for (id, rating) in [(1, 5), (2, 1), (3, 0), (4, 0)] {
            let mut photo = Photo::new(
                PhotoId(id),
                Source::File { path: format!("/lightcraft-next-unrated/{id}.jpg") },
                &format!("{id}.jpg"),
                "JPEG",
                40,
                30,
                "2026-10-09",
            );
            photo.rating = rating;
            s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
        }
        s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
        s.execute("library.select", &json!({"ids": [1]}))?;
        Ok(s)
    }

    #[test]
    fn next_unrated_jumps_to_next_unrated_photo() {
        let mut s = session().unwrap();
        assert_eq!(s.active(), Some(PhotoId(1)));
        // The command that moves the active photo to the next unrated photo in view order.
        let result = s.execute("library.nextUnrated", &json!({}));
        assert!(result.is_ok(), "library.nextUnrated command should exist");
        assert_eq!(s.active(), Some(PhotoId(3)), "next unrated photo in view order");
    }

    #[test]
    fn next_unrated_all_rated_returns_distinct_result() {
        let mut s = session().unwrap();
        s.execute("photo.rate", &json!({"ids": [3, 4], "rating": 5})).unwrap();
        let result = s.execute("library.nextUnrated", &json!({})).unwrap();
        assert_eq!(result, json!({"allRated": true}));
        assert_eq!(s.active(), Some(PhotoId(1)));
    }

    #[test]
    fn next_unrated_preserves_multi_selection_when_moving() {
        let mut s = session().unwrap();
        s.execute("library.select", &json!({"ids": [1, 2, 4], "active": 1})).unwrap();
        let result = s.execute("library.nextUnrated", &json!({})).unwrap();
        assert_eq!(result, json!({"active": 3}));
        assert_eq!(s.active(), Some(PhotoId(3)));
        assert_eq!(s.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(4)]);
    }

    #[test]
    fn next_unrated_preserves_selection_and_mask_spot_when_staying() {
        let mut s = session().unwrap();
        s.execute("photo.rate", &json!({"ids": [4], "rating": 5})).unwrap();
        s.execute("library.select", &json!({"ids": [1, 2, 3], "active": 3})).unwrap();
        s.active_mask = Some(7);
        s.active_spot = Some(2);
        let result = s.execute("library.nextUnrated", &json!({}));
        assert!(result.is_ok(), "library.nextUnrated command should exist");
        assert_eq!(s.active(), Some(PhotoId(3)), "the only unrated photo stays active");
        assert_eq!(s.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(3)], "staying must preserve the multi-photo selection");
        assert_eq!(s.active_mask, Some(7), "staying must preserve the mask state");
        assert_eq!(s.active_spot, Some(2), "staying must preserve the spot state");
    }

    #[test]
    fn prev_unrated_jumps_to_previous_unrated_photo() {
        let mut s = session().unwrap();
        s.execute("library.select", &json!({"ids": [4]})).unwrap();
        let result = s.execute("library.prevUnrated", &json!({}));
        assert!(result.is_ok(), "library.prevUnrated command should exist");
        assert_eq!(s.active(), Some(PhotoId(3)), "previous unrated photo in view order");
    }

    #[test]
    fn prev_unrated_wraps_to_last_unrated_photo() {
        let mut s = session().unwrap();
        assert_eq!(s.active(), Some(PhotoId(1)));
        let result = s.execute("library.prevUnrated", &json!({})).unwrap();
        assert_eq!(result, json!({"active": 4}));
        assert_eq!(s.active(), Some(PhotoId(4)), "wraps to the last unrated photo in view order");
    }

    #[test]
    fn prev_unrated_all_rated_returns_distinct_result() {
        let mut s = session().unwrap();
        s.execute("photo.rate", &json!({"ids": [3, 4], "rating": 5})).unwrap();
        let result = s.execute("library.prevUnrated", &json!({})).unwrap();
        assert_eq!(result, json!({"allRated": true}));
        assert_eq!(s.active(), Some(PhotoId(1)));
    }

    #[test]
    fn prev_unrated_preserves_multi_selection_when_moving() {
        let mut s = session().unwrap();
        s.execute("library.select", &json!({"ids": [1, 2, 4], "active": 4})).unwrap();
        let result = s.execute("library.prevUnrated", &json!({})).unwrap();
        assert_eq!(result, json!({"active": 3}));
        assert_eq!(s.active(), Some(PhotoId(3)));
        assert_eq!(s.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(4)]);
    }

    #[test]
    fn prev_unrated_preserves_selection_and_mask_spot_when_staying() {
        let mut s = session().unwrap();
        s.execute("photo.rate", &json!({"ids": [4], "rating": 5})).unwrap();
        s.execute("library.select", &json!({"ids": [1, 2, 3], "active": 3})).unwrap();
        s.active_mask = Some(7);
        s.active_spot = Some(2);
        let result = s.execute("library.prevUnrated", &json!({}));
        assert!(result.is_ok(), "library.prevUnrated command should exist");
        assert_eq!(s.active(), Some(PhotoId(3)), "the only unrated photo stays active");
        assert_eq!(s.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(3)], "staying must preserve the multi-photo selection");
        assert_eq!(s.active_mask, Some(7), "staying must preserve the mask state");
        assert_eq!(s.active_spot, Some(2), "staying must preserve the spot state");
    }

    #[test]
    fn next_unrated_jumps_to_next_unrated_photo_review_regression_1() {
        let mut s = session().unwrap();
        // The active photo is still in the catalog but no longer in the view (here: moved to
        // Recently Deleted while it was the active photo), so there is no view position to step from.
        s.catalog.apply(Op::SetDeleted { id: PhotoId(1), deleted: true }).unwrap();
        assert_eq!(s.active(), Some(PhotoId(1)), "the photo stays active while it is in the catalog");
        let result = s.execute("library.nextUnrated", &json!({})).unwrap();
        assert_eq!(result, json!({"active": 3}), "next enters the view at its first unrated photo");
        // Stepping backwards from the same photo enters the view at its other end: the last unrated photo.
        s.selection.ids = vec![PhotoId(1)];
        s.selection.active = Some(PhotoId(1));
        let result = s.execute("library.prevUnrated", &json!({})).unwrap();
        assert_eq!(result, json!({"active": 4}), "previous enters the view at its last unrated photo");
    }
}
