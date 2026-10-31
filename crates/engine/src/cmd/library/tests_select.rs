//! Selecting as in Lightroom: Shift / ⌘-click keep the active photo, a click inside the selection
//! keeps it, and Select All keeps up with a view that grows.
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Result, Session};

fn add(s: &mut Session, id: u64) -> Result<()> {
    let photo = Photo::new(
        PhotoId(id),
        Source::File { path: format!("/lightcraft-select/{id:03}.jpg") },
        &format!("{id:03}.jpg"),
        "JPEG",
        40,
        30,
        "2026-10-22",
    );
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    Ok(())
}

fn session(n: u64) -> Result<Session> {
    let mut s = Session::new();
    for id in 1..=n {
        add(&mut s, id)?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    Ok(s)
}

#[test]
fn shift_and_ctrl_clicks_keep_the_active_photo_and_a_click_inside_keeps_the_selection() -> Result<()> {
    let mut s = session(10)?;
    s.execute("library.select", &json!({"ids": [3], "mode": "click"}))?;
    s.execute("library.select", &json!({"ids": [7], "mode": "range"}))?;
    assert_eq!(s.selection.active, Some(PhotoId(3)), "Shift-click: the loupe stays on 3");
    assert_eq!(s.selection.ids.len(), 5);
    s.execute("library.select", &json!({"ids": [9], "mode": "toggle"}))?;
    assert_eq!(s.selection.active, Some(PhotoId(3)), "⌘-click: the loupe stays on 3");
    assert_eq!(s.selection.ids.len(), 6);
    s.execute("library.select", &json!({"ids": [5], "mode": "click"}))?;
    assert_eq!((s.selection.active, s.selection.ids.len()), (Some(PhotoId(5)), 6), "a click inside: 5 active, the set stays");
    s.execute("library.select", &json!({"ids": [10], "mode": "click"}))?;
    assert_eq!((s.selection.active, s.selection.ids.len()), (Some(PhotoId(10)), 1), "a click outside replaces it");
    Ok(())
}

#[test]
fn select_all_takes_in_the_photos_that_join_the_view() -> Result<()> {
    let mut s = session(5)?;
    s.execute("library.selectAll", &json!({}))?;
    assert_eq!(s.selection.ids.len(), 5);
    // an import still running adds photos to the view
    for id in 6..=9 {
        add(&mut s, id)?;
    }
    assert_eq!(s.visible().len(), 9);
    assert_eq!(s.selection.ids.len(), 9, "every photo of the view stays selected");
    // a selection changed by hand is the user's: new photos don't join it
    s.execute("library.select", &json!({"ids": [2], "mode": "toggle"}))?;
    add(&mut s, 10)?;
    assert_eq!(s.visible().len(), 10);
    assert_eq!(s.selection.ids.len(), 8);
    // nor do they in another view
    s.execute("library.selectAll", &json!({}))?;
    s.execute("library.filter", &json!({"text": "00"}))?;
    add(&mut s, 11)?;
    let _ = s.visible();
    assert_eq!(s.selection.ids.len(), 10);
    Ok(())
}

#[test]
fn select_all_ends_with_another_view_and_does_not_come_back_with_its_view() -> Result<()> {
    let mut s = session(2)?;
    s.execute("library.selectAll", &json!({}))?;
    // narrowed by hand to one photo, then a view that shows only that photo
    s.execute("library.select", &json!({"ids": [1]}))?;
    s.execute("library.filter", &json!({"text": "001"}))?;
    assert_eq!(s.visible().len(), 1);
    assert_eq!(s.selection.ids, vec![PhotoId(1)]);
    // back to the first view: photo 2 stays deselected (the old Select All is over)
    s.execute("library.filter", &json!({"text": ""}))?;
    assert_eq!(s.visible().len(), 2);
    assert_eq!(s.selection.ids, vec![PhotoId(1)]);
    Ok(())
}

#[test]
fn a_click_that_changes_nothing_keeps_select_all_and_select_by_ends_it() -> Result<()> {
    let mut s = session(3)?;
    s.execute("library.selectAll", &json!({}))?;
    let active = s.selection.active.map(|a| a.0);
    // a plain click on the active photo of the selection changes nothing: still "every photo of the view"
    s.execute("library.select", &json!({"ids": [active], "mode": "click"}))?;
    add(&mut s, 4)?;
    assert_eq!(s.visible().len(), 4);
    assert_eq!(s.selection.ids.len(), 4, "the new photo joins Select All");
    // narrowed by flag, then every photo picked again by flag: chosen by hand, new photos don't join
    s.execute("library.selectBy", &json!({"flag": "pick"}))?;
    s.execute("library.selectBy", &json!({"flag": "none"}))?;
    assert_eq!(s.selection.ids.len(), 4);
    add(&mut s, 5)?;
    assert_eq!(s.visible().len(), 5);
    assert_eq!(s.selection.ids.len(), 4, "Select All ended with the selection by flag");
    Ok(())
}

#[test]
fn a_click_that_only_moves_the_active_photo_keeps_select_all() -> Result<()> {
    let mut s = session(3)?;
    s.execute("library.selectAll", &json!({}))?;
    let other = s.selection.ids.iter().find(|id| Some(**id) != s.selection.active).map(|id| id.0);
    // a plain click on another photo of the selection (grid or filmstrip): it becomes active, the set stays
    s.execute("library.select", &json!({"ids": [other], "mode": "click"}))?;
    assert_eq!(s.selection.active.map(|a| a.0), other);
    add(&mut s, 4)?;
    assert_eq!(s.visible().len(), 4);
    assert_eq!(s.selection.ids.len(), 4, "the new photo joins Select All");
    Ok(())
}

/// Missing Photos fills in as the background checks find files gone: a photo found gone after Select All joins the
/// selection (the checks are not another view).
#[test]
fn select_all_in_missing_photos_takes_in_a_photo_the_checks_find_gone() -> Result<()> {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let mut s = session(2)?;
    // every file is gone (none of these paths exist); answered by a worker, as in the app
    s.media.availability.set_probe(Arc::new(|_: &str| false));
    s.media.availability.run_in_background(Arc::new(|| {}));
    s.source = crate::LibrarySource::Missing;
    let settle = |s: &mut Session, n: usize| {
        let until = Instant::now() + Duration::from_secs(10);
        while s.visible().len() < n && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        s.visible().len()
    };
    assert_eq!(settle(&mut s, 2), 2);
    s.execute("library.selectAll", &json!({}))?;
    add(&mut s, 3)?;
    assert_eq!(settle(&mut s, 3), 3, "the check finds photo 3 gone");
    assert_eq!(s.selection.ids.len(), 3, "photo 3 joins Select All");
    Ok(())
}

/// An arrow key after Select All moves the selection on: Select All is over, and a photo that leaves the view and
/// comes back is not selected again (CodeRabbit on #3030).
#[test]
fn select_all_ends_when_an_arrow_key_moves_the_selection() -> Result<()> {
    let mut s = session(2)?;
    s.execute("library.filter", &json!({"flag": "none"}))?;
    s.execute("library.selectAll", &json!({}))?;
    s.execute("library.select", &json!({"ids": [1], "active": 1, "mode": "click"}))?;
    s.execute("library.next", &json!({}))?;
    assert_eq!(s.selection.ids, vec![PhotoId(2)]);
    // photo 1 leaves the view (rejected), then comes back
    s.execute("photo.flag", &json!({"flag": "reject", "ids": [1]}))?;
    assert_eq!(s.visible().len(), 1);
    s.execute("photo.flag", &json!({"flag": "none", "ids": [1]}))?;
    assert_eq!(s.visible().len(), 2);
    assert_eq!(s.selection.ids, vec![PhotoId(2)], "photo 1 is not selected again");
    Ok(())
}

/// Photos that joined the view after Select All are selected before the next command acts, with no view refresh
/// between: a command acts on them too, and a Shift-click on a selected photo changes nothing, so Select All goes on
/// (CodeRabbit on #3030).
#[test]
fn select_all_takes_in_new_photos_before_the_next_command() -> Result<()> {
    let mut s = session(3)?;
    s.execute("library.selectAll", &json!({}))?;
    add(&mut s, 4)?;
    s.execute("photo.rate", &json!({"rating": 2}))?;
    assert_eq!(s.catalog.photo(PhotoId(4)).map(|p| p.rating), Some(2), "the new photo is rated with the others");
    add(&mut s, 5)?;
    s.execute("library.select", &json!({"ids": [2], "mode": "range"}))?;
    assert_eq!(s.selection.ids.len(), 5);
    add(&mut s, 6)?;
    assert_eq!(s.visible().len(), 6);
    assert_eq!(s.selection.ids.len(), 6, "Select All goes on after a Shift-click that changed nothing");
    Ok(())
}
