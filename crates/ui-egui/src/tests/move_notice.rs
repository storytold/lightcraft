//! What the app says when the library opens and finds moves an earlier run cut short
//! (`lightcraft_engine::move_intent`): its own window ("Unfinished moves"), not a settings problem.

use std::time::Duration;

use lightcraft_engine::move_intent::{MoveProblem, Recovered};
use serde_json::json;

use crate::headless::Headless;
use crate::panels::notices::recovered_text;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);

#[test]
fn nothing_to_say_when_nothing_was_left() {
    assert_eq!(recovered_text(&Recovered::default()), None);
}

#[test]
fn the_notice_says_what_was_followed_what_waits_and_what_went_wrong() {
    let r = Recovered {
        relinked: 2,
        waiting: 1,
        problems: vec![MoveProblem::BothPlaces { from: "/a/x.jpg".into(), to: "/b/x.jpg".into() }],
        ..Default::default()
    };
    let text = recovered_text(&r).expect("something to say");
    assert!(text.contains("Photos being moved when LightCraft last closed: 2."), "{text}");
    assert!(text.contains("can't be reached now (a disk not connected?): 1."), "{text}");
    assert!(text.contains("/b/x.jpg was being moved and is now at both places: the library keeps /a/x.jpg."), "{text}");
}

#[test]
fn the_notice_tells_removed_copies_and_a_library_left_alone_for_want_of_its_lock() {
    let copies = recovered_text(&Recovered { copies_removed: 2, ..Default::default() }).expect("removed copies are told");
    assert!(copies.contains("Copies removed from the new place, because the original was still where the library has it: 2."), "{copies}");
    let alone = recovered_text(&Recovered { problems: vec![MoveProblem::Unprotected], ..Default::default() }).expect("said");
    assert!(alone.contains("open without its lock") && alone.contains("Find Missing Photos"), "{alone}");
}

#[test]
fn a_library_that_followed_moves_shows_the_notice_once_and_ok_closes_it() {
    let dir = std::env::temp_dir().join(format!("lc-move-notice-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut session = lightcraft_engine::Session::new();
    session.open_library(&dir, false).unwrap();
    session.library.as_mut().unwrap().recovered_moves = Some(Recovered { relinked: 3, ..Default::default() });
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for _ in 0..4 {
        h.step();
    }
    let notice = h.app.move_notice.clone().expect("the notice is taken from the library");
    assert!(notice.contains("Photos being moved when LightCraft last closed: 3."), "{notice}");
    assert_eq!(crate::control::inspect(&h.app, &h.view.ctx)["moveNotice"], json!(notice));
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:moveNoticeOk"), "the window has its OK");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:moveNoticeOk"}), T)["ok"], true);
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.app.move_notice, None);
    assert!(h.app.session.take_recovered_moves().is_none(), "said once");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_notice_of_a_library_does_not_outlive_a_switch_to_another() {
    let dir = std::env::temp_dir().join(format!("lc-move-notice-switch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (first, second) = (dir.join("first"), dir.join("second"));
    let mut other = lightcraft_engine::Session::new();
    other.open_library(&second, false).unwrap();
    drop(other);
    let mut session = lightcraft_engine::Session::new();
    session.open_library(&first, false).unwrap();
    session.library.as_mut().unwrap().recovered_moves = Some(Recovered { relinked: 3, ..Default::default() });
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for _ in 0..4 {
        h.step();
    }
    assert!(h.app.move_notice.is_some(), "the first library's notice is up");
    crate::panels::settings::open_library(&mut h.app, &json!({"path": second.to_string_lossy()})).expect("the switch goes through");
    assert_eq!(h.app.move_notice, None, "it said something about the library that was left");
    let _ = std::fs::remove_dir_all(&dir);
}
