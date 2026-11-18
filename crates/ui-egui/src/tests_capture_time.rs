//! Edit Capture Time on a photo whose own capture time doesn't fully read (a Lightroom catalog's
//! `captureTime` is copied verbatim): the preview says so instead of asking for a date the user
//! never typed, and OK leaves the photo as it was and says why. A capture time of a year or a month
//! alone reads, shows at its own precision and is kept as typed, but has no time to shift.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);

fn set_captured(h: &mut Headless, id: lcc_catalog::PhotoId, t: &str) {
    h.app.session.catalog.apply(lcc_catalog::Op::SetCaptured { id, captured: Some(t.into()) }).unwrap();
}

fn painted(h: &Headless, part: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(part))
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default()
}

/// Select `ids` with `active` and open Edit Capture Time.
fn open(h: &mut Headless, ids: &[lcc_catalog::PhotoId], active: lcc_catalog::PhotoId) {
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids, "active": active.0}}), T);
    let r = h.request("ui.menu.invoke", json!({"id": "dialog.captureTime"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

fn demo() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    Headless::new(app, [1300.0, 900.0], 1.0)
}

/// Between the steps of the one session: no dialog left open, no message left showing.
fn fresh(h: &mut Headless) {
    h.app.ui.dialog = None;
    h.app.ui.toasts.clear();
    h.step();
    h.step();
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn edit_capture_time_handles_unreadable_and_partial_capture_times() {
    let mut h = demo();
    // shift preview says the photos capture time cant be read
    {
        let id = h.app.session.visible_cloned()[0];
        set_captured(&mut h, id, "2026-01-01T10:00:00");
        open(&mut h, &[id], id);
        match &mut h.app.ui.dialog {
            Some(Dialog::CaptureTime { mode, hours, .. }) => {
                *mode = "shift".into();
                *hours = 1;
            }
            other => panic!("Edit Capture Time is not open: {other:?}"),
        }
        h.settle(SETTLE);
        assert!(painted(&h, "2026-01-01 10:00:00 → 2026-01-01 11:00:00"), "a readable time previews its shift: {:?}", h.painted_text());

        let bad = "2026-01-01T10:00:00 and more";
        set_captured(&mut h, id, bad);
        h.settle(SETTLE);
        assert!(painted(&h, "can't be read"), "the preview says the photo's time can't be read: {:?}", h.painted_text());
        assert!(!painted(&h, "Enter a date"), "no date was typed, so the preview doesn't ask for one: {:?}", h.painted_text());

        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        // (the toast is read as it appears: it goes after a moment, and settling a busy machine
        // can take longer than that)
        h.step_until(SETTLE, |h| !toast(h).is_empty());
        assert!(h.app.ui.dialog.is_none(), "OK closes the dialog");
        let toast = toast(&h);
        assert!(toast.contains("can't be read"), "OK says the photo was left as it was: {toast:?}");
        assert_eq!(h.app.session.catalog.photo(id).unwrap().captured.as_deref(), Some(bad), "the unreadable time is left as it was");
    }
    // set mode refuses moving others by an unreadable time and repairs it alone
    {
        fresh(&mut h);
        let ids = h.app.session.visible_cloned();
        let (bad, other) = (ids[0], ids[1]);
        let broken = "2026-01-01T10:00:00 and more";
        set_captured(&mut h, bad, broken);
        set_captured(&mut h, other, "2026-01-01T12:00:00");
        let open_set = |h: &mut Headless, select: &[lcc_catalog::PhotoId]| {
            open(h, select, bad);
            match &mut h.app.ui.dialog {
                Some(Dialog::CaptureTime { mode, time, .. }) => {
                    *mode = "set".into();
                    *time = "2026-01-01 11:00:00".into();
                }
                other => panic!("Edit Capture Time is not open: {other:?}"),
            }
            h.settle(SETTLE);
        };

        // with another photo selected, OK can't move it by the unreadable time's change: it says so
        // and stays open
        open_set(&mut h, &[bad, other]);
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        // (the toast is read as it appears: it goes after a moment, and settling a busy machine
        // can take longer than that)
        h.step_until(SETTLE, |h| !toast(h).is_empty());
        let toast = toast(&h);
        assert!(toast.contains("can't be read"), "OK says why nothing changed: {toast:?}");
        assert!(matches!(h.app.ui.dialog, Some(Dialog::CaptureTime { .. })), "the dialog stays open after the refusal");
        let captured = |h: &Headless, id| h.app.session.catalog.photo(id).unwrap().captured.clone();
        assert_eq!(captured(&h, bad).as_deref(), Some(broken));
        assert_eq!(captured(&h, other).as_deref(), Some("2026-01-01T12:00:00"), "the other photo doesn't move");
        h.request("ui.dialog.cancel", json!({}), T);

        // alone, the preview shows the time it will get, and OK sets it
        open_set(&mut h, &[bad]);
        let preview = h.painted_text().into_iter().find(|t| t.contains(" → ")).unwrap_or_default();
        assert!(preview.ends_with(" → 2026-01-01 11:00:00"), "the preview shows the time it will get, nothing after: {preview:?}");
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.ui.dialog.is_none(), "OK closes the dialog");
        assert_eq!(captured(&h, bad).as_deref(), Some("2026-01-01T11:00:00"), "alone, it gets the time typed");
    }
    // errors are shown and the dialog stays open
    // Errors from the command say why and keep the dialog (and what was typed) open, from its OK
    // button and from the control channel alike; they used to close it without a word.
    {
        fresh(&mut h);
        let id = h.app.session.visible_cloned()[0];
        set_captured(&mut h, id, "9999-12-31T12:00:00");
        open(&mut h, &[id], id);
        match &mut h.app.ui.dialog {
            Some(Dialog::CaptureTime { time, .. }) => *time = "yesterday".into(),
            other => panic!("Edit Capture Time is not open: {other:?}"),
        }
        h.settle(SETTLE);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T)["ok"], true);
        h.settle(SETTLE);
        assert!(toast(&h).contains("not a date"), "OK says the typed time isn't a date: {:?}", toast(&h));
        assert!(
            matches!(&h.app.ui.dialog, Some(Dialog::CaptureTime { time, .. }) if time == "yesterday"),
            "the dialog stays open with what was typed"
        );

        // a shift past year 9999, confirmed through the control channel
        match &mut h.app.ui.dialog {
            Some(Dialog::CaptureTime { mode, days, .. }) => {
                *mode = "shift".into();
                *days = 1;
            }
            other => panic!("Edit Capture Time is not open: {other:?}"),
        }
        h.settle(SETTLE);
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_ne!(r["ok"], true, "the shift is refused: {r}");
        assert!(r.to_string().contains("9999"), "{r}");
        assert!(matches!(h.app.ui.dialog, Some(Dialog::CaptureTime { .. })), "ui.dialog.confirm keeps it open too, as the button does");
        assert_eq!(h.app.session.catalog.photo(id).unwrap().captured.as_deref(), Some("9999-12-31T12:00:00"));
    }
    // set mode does not start from an unreadable time
    // A capture time that doesn't fully read isn't offered as the time to set: `2026-01-01T10:0`
    // would read as midnight. The field starts empty and the preview asks for a date.
    {
        fresh(&mut h);
        let id = h.app.session.visible_cloned()[0];
        set_captured(&mut h, id, "2026-01-01T10:00:00");
        open(&mut h, &[id], id);
        assert!(
            matches!(&h.app.ui.dialog, Some(Dialog::CaptureTime { time, .. }) if time == "2026-01-01 10:00:00"),
            "a readable time is offered: {:?}",
            h.app.ui.dialog
        );
        h.request("ui.dialog.cancel", json!({}), T);

        set_captured(&mut h, id, "2026-01-01T10:0");
        open(&mut h, &[id], id);
        assert!(
            matches!(&h.app.ui.dialog, Some(Dialog::CaptureTime { time, .. }) if time.is_empty()),
            "an unreadable time isn't offered: {:?}",
            h.app.ui.dialog
        );
        assert!(painted(&h, "Enter a date"), "alone, the photo will get the time typed, so the preview asks for one: {:?}", h.painted_text());
    }
    // edit capture time shows and keeps a year or month alone
    // A year or a month alone (`1983-04`, a scanned print's date) shows at its own precision in Edit
    // Capture Time, whose preview takes a year alone as typed, and OK keeps it as typed. (The grid's
    // date line and the Info panel show it through `i18n::display_time`, tested with the languages.)
    {
        fresh(&mut h);
        let id = h.app.session.visible_cloned()[0];
        set_captured(&mut h, id, "1983-04");
        open(&mut h, &[id], id);
        match &mut h.app.ui.dialog {
            Some(Dialog::CaptureTime { mode, time, .. }) => {
                *mode = "set".into();
                *time = "1983".into();
            }
            other => panic!("Edit Capture Time is not open: {other:?}"),
        }
        h.settle(SETTLE);
        assert!(painted(&h, "April 1983 → 1983"), "the preview shows both at their precision: {:?}", h.painted_text());
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.session.catalog.photo(id).unwrap().captured.as_deref(), Some("1983"));
    }
    // edit capture time starts from a partial date and gives one to every photo
    // Edit Capture Time opened on a photo dated to a year or a month alone starts from that date, as
    // stated; typed with several photos selected, a year or a month goes to each of them.
    {
        fresh(&mut h);
        let ids = h.app.session.visible_cloned();
        let (a, b) = (ids[0], ids[1]);
        set_captured(&mut h, a, "1983-04");
        set_captured(&mut h, b, "2026-01-01T10:00:00");
        open(&mut h, &[a], a);
        match &h.app.ui.dialog {
            Some(Dialog::CaptureTime { time, .. }) => assert_eq!(time, "1983-04", "starts from the date as stated"),
            other => panic!("Edit Capture Time is not open: {other:?}"),
        }
        h.app.ui.dialog = None;
        open(&mut h, &[a, b], a);
        match &mut h.app.ui.dialog {
            Some(Dialog::CaptureTime { mode, time, .. }) => {
                *mode = "set".into();
                *time = "1983".into();
            }
            other => panic!("Edit Capture Time is not open: {other:?}"),
        }
        h.settle(SETTLE);
        assert!(painted(&h, "All 2 selected photos get this date."), "{:?}", h.painted_text());
        assert!(painted(&h, "April 1983 → 1983"), "{:?}", h.painted_text());
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        for id in [a, b] {
            assert_eq!(h.app.session.catalog.photo(id).unwrap().captured.as_deref(), Some("1983"));
        }
    }
    // shifting a partial date says it has no time
    // A shift with a photo dated to a year or a month alone: the preview and the result say it has no
    // time to shift (not that it can't be read), and it is left as it was; a photo whose time can't be
    // read is named in the same result.
    {
        fresh(&mut h);
        let ids = h.app.session.visible_cloned();
        let (a, b) = (ids[0], ids[1]);
        set_captured(&mut h, a, "1983-04");
        set_captured(&mut h, b, "2026-01-01T10:00:00");
        let broken = "2026-01-01T10:00:00 and more";
        set_captured(&mut h, ids[2], broken);
        open(&mut h, &[a, b, ids[2]], a);
        match &mut h.app.ui.dialog {
            Some(Dialog::CaptureTime { mode, hours, .. }) => {
                *mode = "shift".into();
                *hours = 1;
            }
            other => panic!("Edit Capture Time is not open: {other:?}"),
        }
        h.settle(SETTLE);
        assert!(painted(&h, "year or month alone, with no time to shift"), "{:?}", h.painted_text());
        assert!(!painted(&h, "can't be read"), "{:?}", h.painted_text());
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        let toast = toast(&h);
        assert!(toast.contains("no time to shift") && toast.contains("can't be read"), "both are named: {toast:?}");
        assert_eq!(h.app.session.catalog.photo(ids[2]).unwrap().captured.as_deref(), Some(broken));
        assert_eq!(h.app.session.catalog.photo(a).unwrap().captured.as_deref(), Some("1983-04"));
        assert_eq!(h.app.session.catalog.photo(b).unwrap().captured.as_deref(), Some("2026-01-01T11:00:00"));
    }

    // Issue #4205: with several photos selected, Set date & time moves them all by the active photo's change (as
    // Lightroom does), and "Give every selected photo this time" gives each of them the time typed instead.
    {
        fresh(&mut h);
        let ids: Vec<_> = h.app.session.visible_cloned().into_iter().take(3).collect();
        let times = ["2026-01-01T10:00:00", "2026-01-02T08:30:00", "2026-02-10T18:45:00"];
        let run = |h: &mut Headless, each: bool| {
            for (id, t) in ids.iter().zip(times) {
                set_captured(h, *id, t);
            }
            open(h, &ids, ids[0]);
            match &mut h.app.ui.dialog {
                Some(Dialog::CaptureTime { mode, time, .. }) => {
                    *mode = "set".into();
                    *time = "2026-05-05 12:00:00".into();
                }
                other => panic!("Edit Capture Time is not open: {other:?}"),
            }
            h.settle(SETTLE);
            assert!(painted(h, "All 3 selected photos move by the same amount."), "{:?}", h.painted_text());
            if each {
                let r = h.request("ui.clickWidget", json!({"id": "captureTimeEach"}), T);
                assert_eq!(r["ok"], true, "the option is offered with several photos: {r}");
                h.settle(SETTLE);
                assert!(painted(h, "All 3 selected photos get this time."), "{:?}", h.painted_text());
            }
            let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
            assert_eq!(r["ok"], true, "{r}");
            h.settle(SETTLE);
            ids.iter().map(|id| h.app.session.catalog.photo(*id).and_then(|p| p.captured.clone()).unwrap_or_default()).collect::<Vec<_>>()
        };
        assert_eq!(run(&mut h, false), ["2026-05-05T12:00:00", "2026-05-06T10:30:00", "2026-06-14T20:45:00"]);
        assert_eq!(run(&mut h, true), ["2026-05-05T12:00:00"; 3]);
    }
}
