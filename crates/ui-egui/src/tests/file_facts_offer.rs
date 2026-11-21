//! Issue #4367: photos imported before LightCraft recorded what their files carry are updated only on request. The
//! desktop app offers it once per version when the library holds such photos; Update Now reads them in the
//! background and records only what was missing; Not Now reads nothing; Settings can run it any time; headless
//! hosts never offer (`crate::file_facts`, `lightcraft_engine::cmd::file_facts`).

use std::sync::Arc;
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::Session;
use lightcraft_engine::media::ProbeInfo;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A desktop-like app (allowed to offer) whose library holds `older` DNGs never looked at; their files "carry" a
/// skin matte.
fn desktop(older: usize) -> (LightcraftApp, Vec<PhotoId>) {
    let mut session = Session::with_demo().with_file_facts_offer();
    let template = Photo::clone(session.catalog.photos().next().unwrap());
    let mut ids = Vec::new();
    for i in 0..older {
        let mut p = template.clone();
        p.id = session.catalog.alloc_photo_id();
        p.source = Source::File { path: format!("/old/{i}.dng") };
        p.format = "DNG".into();
        p.meta.mattes = None;
        ids.push(p.id);
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    session.media.file_probe = Some(Arc::new(|_: &str| {
        let mut info = ProbeInfo { width: 1, height: 1, ..Default::default() };
        info.meta.mattes = Some(vec!["skin".to_string()]);
        Ok(info)
    }));
    (LightcraftApp::new(session, Services { png: None, ..Default::default() }), ids)
}

fn looked_at(app: &LightcraftApp, ids: &[PhotoId]) -> usize {
    ids.iter().filter(|id| app.session.catalog.photo(**id).is_some_and(|p| p.meta.mattes.is_some())).count()
}

/// The start offers with the count and reads nothing; Update Now reads the files in the background (an activity
/// row) and records them; this version doesn't ask again.
#[test]
fn the_start_offers_and_update_now_records_in_the_background() {
    let (app, ids) = desktop(3);
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    let Some(dlg @ Dialog::FileFacts { count: 3 }) = h.app.ui.dialog.clone() else { panic!("no offer: {:?}", h.app.ui.dialog) };
    assert_eq!(looked_at(&h.app, &ids), 0, "nothing read while it asks");
    assert_eq!(h.app.ui.settings.file_facts_asked.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).expect("Update Now");
    h.app.ui.dialog = None;
    assert!(h.step_until(T, |h| looked_at(&h.app, &ids) == 3), "the pass never finished");
    assert_eq!(crate::file_facts::pending(&h.app), 0);
    // the next start of this version: nothing left, and not asked again even if there were
    h.app.ui.file_facts_checked = false;
    h.step();
    assert!(h.app.ui.dialog.is_none());
}

/// Not Now reads nothing and this version doesn't ask again; a later version does; Settings' button runs it.
#[test]
fn not_now_reads_nothing_and_settings_can_run_it_later() {
    let (app, ids) = desktop(2);
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::FileFacts { count: 2 })));
    h.app.ui.dialog = None; // Not Now
    h.app.ui.file_facts_checked = false;
    h.step();
    assert!(h.app.ui.dialog.is_none(), "this version asked already");
    assert_eq!(looked_at(&h.app, &ids), 0);
    // saved and read back with the app's settings; an older file without the keys offers
    let saved = serde_json::to_value(&h.app.ui.settings).expect("settings save");
    let read: crate::state::AppSettings = serde_json::from_value(saved).expect("settings read");
    assert_eq!(read.file_facts_asked.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    let fresh: crate::state::AppSettings = serde_json::from_value(serde_json::json!({})).expect("older settings");
    assert!(fresh.file_facts_offer && fresh.file_facts_asked.is_none());
    // a later version of the app asks once more
    h.app.ui.settings.file_facts_asked = Some("0.0.1".into());
    h.app.ui.file_facts_checked = false;
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::FileFacts { count: 2 })));
    h.app.ui.dialog = None;
    // Settings' Update Now
    assert_eq!(crate::file_facts::start(&mut h.app), Ok(2));
    assert!(crate::file_facts::start(&mut h.app).is_err(), "not twice at once");
    assert!(h.step_until(T, |h| looked_at(&h.app, &ids) == 2));
}

/// Nothing to update, the offer switched off, or a host without a person to ask: no offer, nothing read.
#[test]
fn no_offer_without_older_photos_when_switched_off_or_headless() {
    let (app, _) = desktop(0);
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(h.app.ui.settings.file_facts_asked, None, "not asked: a library opened later may hold some");

    let (mut app, ids) = desktop(2);
    app.ui.settings.file_facts_offer = false;
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(looked_at(&h.app, &ids), 0);

    let (mut app, ids) = desktop(2);
    app.session.file_facts_offer = false; // a CLI snapshot, MCP
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(looked_at(&h.app, &ids), 0);
    // the command still works there, in the background
    let r = h.app.run("library.recordFileFacts", serde_json::json!({})).unwrap();
    assert_eq!((r["background"].clone(), r["count"].clone()), (serde_json::json!(true), serde_json::json!(2)));
    assert!(h.step_until(T, |h| looked_at(&h.app, &ids) == 2));
}

/// Settings ▸ General has the row: how many photos are waiting, Update Now and the switch for the offer. Update Now
/// there runs the pass, and the row then says every photo is up to date.
#[test]
fn settings_shows_how_many_wait_and_runs_the_update() {
    let (mut app, ids) = desktop(2);
    app.session.file_facts_offer = false; // no offer in the way
    let mut h = Headless::new(app, [1300.0, 1000.0], 1.0);
    h.app.run("app.settings", serde_json::json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    assert!(has(&h, "button:settingsFileFacts"), "{:?}", h.app.ui.dialog);
    assert!(h.painted_text().iter().any(|t| t.contains("2 photos were imported before")), "{:?}", h.painted_text());
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "button:settingsFileFacts"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| looked_at(&h.app, &ids) == 2), "Update Now never finished");
    for _ in 0..3 {
        h.step();
    }
    assert!(h.painted_text().iter().any(|t| t.contains("Every photo is up to date")), "{:?}", h.painted_text());
}
