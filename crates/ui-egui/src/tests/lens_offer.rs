//! The lens database is offered once, on the first launch, and downloaded only after a click: Download in the offer
//! or in Settings. Not Now downloads nothing and the next start doesn't ask; an installed database and headless
//! hosts (CLI, MCP) never ask (`crate::lens_db`, `lightcraft_engine::lensfun_download::fetch_on_start`).

use lightcraft_engine::lensfun_download::{self, Offer, Start};
use lightcraft_engine::{Session, lens_profiles};

use crate::state::Dialog;
use crate::{LightcraftApp, Services};

/// A desktop-like app (allowed to ask) and a folder of this test's own for the database.
fn desktop(name: &str) -> (LightcraftApp, std::path::PathBuf) {
    let mut session = Session::with_demo();
    session.lens_auto_download = true;
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("lc-lens-offer-{name}-{}-{unique}", std::process::id()));
    (app, root)
}

/// Nothing listens on port 1: a download started in a test fails at once, without the network.
fn offline() -> Vec<String> {
    vec!["http://127.0.0.1:1/db".into()]
}

fn wait_idle(app: &LightcraftApp) {
    let t0 = std::time::Instant::now();
    while app.session.lensfun.running() && t0.elapsed() < std::time::Duration::from_secs(60) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The first start shows the offer and fetches nothing; Download starts the download (an activity row) and is
/// remembered as the user's yes.
#[test]
fn the_first_start_offers_the_database_and_fetches_nothing_until_download() {
    let (mut app, root) = desktop("first");
    let dir = root.join("lensfun");
    let before = lens_profiles::host_dir();
    crate::lens_db::begin(&mut app, lensfun_download::decide(true, false, Some(dir.clone()), false, Offer::Ask), offline());
    let Some(dlg @ Dialog::LensDatabase { .. }) = app.ui.dialog.clone() else { panic!("no offer: {:?}", app.ui.dialog) };
    assert_eq!(app.session.lensfun.state(), lensfun_download::State::Idle, "nothing fetched while it asks");
    assert!(app.session.activity.list().is_empty());
    crate::panels::dialogs::confirm_dialog(&mut app, &dlg).expect("Download");
    assert_ne!(app.session.lensfun.state(), lensfun_download::State::Idle, "Download starts it");
    assert_eq!(app.ui.settings.lens_database, Offer::Accepted);
    wait_idle(&app);
    lens_profiles::use_dir(before);
    let _ = std::fs::remove_dir_all(&root);
}

/// Not Now (or Esc: closing the offer any way but Download) downloads nothing, is saved with the settings, and the
/// next start doesn't ask.
#[test]
fn not_now_is_remembered_and_the_next_start_does_not_ask() {
    let (mut app, root) = desktop("notnow");
    crate::lens_db::begin(&mut app, Start::Ask(root.join("lensfun")), offline());
    assert!(matches!(app.ui.dialog, Some(Dialog::LensDatabase { .. })));
    app.ui.dialog = None; // Not Now
    assert_eq!(app.session.lensfun.state(), lensfun_download::State::Idle, "nothing fetched");
    assert_eq!(app.ui.settings.lens_database, Offer::Declined);
    // saved and read back with the app's settings
    let saved = serde_json::to_value(&app.ui.settings).expect("settings save");
    let read: crate::state::AppSettings = serde_json::from_value(saved).expect("settings read");
    assert_eq!(read.lens_database, Offer::Declined);
    assert_eq!(lensfun_download::decide(true, false, Some(root.join("lensfun")), false, read.lens_database), Start::Nothing, "not asked again");
    // a damaged value asks again instead of failing to read the settings
    let mut damaged = serde_json::to_value(&app.ui.settings).expect("settings save");
    damaged["lensDatabase"] = serde_json::json!(42);
    let read: crate::state::AppSettings = serde_json::from_value(damaged).expect("a bad value doesn't spoil the settings");
    assert_eq!(read.lens_database, Offer::Ask);
}

/// Settings' Download Lens Database downloads it (after a Not Now, too) and remembers the yes.
#[test]
fn the_settings_button_downloads_the_database() {
    let (mut app, root) = desktop("settings");
    let before = lens_profiles::host_dir();
    app.ui.settings.lens_database = Offer::Declined;
    crate::lens_db::download(&mut app, &root.join("lensfun"), offline()).expect("started");
    assert_ne!(app.session.lensfun.state(), lensfun_download::State::Idle, "the download ran");
    assert_eq!(app.ui.settings.lens_database, Offer::Accepted);
    assert_eq!(lens_profiles::host_dir(), Some(root.join("lensfun")), "and is read once it is in place");
    wait_idle(&app);
    lens_profiles::use_dir(before);
    let _ = std::fs::remove_dir_all(&root);
}

/// An installed database means no offer and no download.
#[test]
fn an_installed_database_is_not_offered() {
    let (mut app, root) = desktop("installed");
    for offer in [Offer::Ask, Offer::Accepted, Offer::Declined] {
        crate::lens_db::begin(&mut app, lensfun_download::decide(true, false, Some(root.join("lensfun")), true, offer), offline());
        assert!(app.ui.dialog.is_none(), "{offer:?}");
        assert_eq!(app.session.lensfun.state(), lensfun_download::State::Idle);
    }
}

/// A headless app (CLI snapshot, MCP) never shows the offer and never fetches, whatever the settings say.
#[test]
fn a_headless_app_never_offers_or_fetches() {
    let mut app = LightcraftApp::new(Session::with_demo(), Services { png: None, ..Default::default() });
    assert!(!app.session.lens_auto_download);
    for offer in [Offer::Ask, Offer::Accepted] {
        app.ui.settings.lens_database = offer;
        app.ui.lens_download_checked = false;
        crate::lens_db::tick(&mut app);
        assert!(app.ui.dialog.is_none());
        assert_eq!(app.session.lensfun.state(), lensfun_download::State::Idle);
    }
}

/// A download that can't start keeps the offer open with a toast saying why (Download again or Not Now), from its
/// Download button and from `ui.dialog.confirm`, and isn't remembered as a yes.
#[test]
fn a_download_that_cannot_start_keeps_the_offer_open_and_says_why() {
    use crate::headless::Headless;
    let t = std::time::Duration::from_secs(20);
    let app = LightcraftApp::new(Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    let root = std::env::temp_dir().join(format!("lc-lens-offer-fail-{}", std::process::id()));
    // no place to download from: the start is refused
    let refused = Dialog::LensDatabase { dir: root.join("lensfun").to_string_lossy().into_owned(), mirrors: Vec::new() };
    let offer_open = |h: &Headless| matches!(h.app.ui.dialog, Some(Dialog::LensDatabase { .. }));

    h.app.ui.settings.lens_database = Offer::Declined;
    h.app.ui.dialog = Some(refused.clone());
    h.settle(std::time::Duration::from_secs(10));
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "button:dialogOk"}), t);
    assert_eq!(r["ok"], true, "{r}");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("no download location"), "Download says why it didn't start: {toast:?}");
    assert!(offer_open(&h), "the offer stays open: {:?}", h.app.ui.dialog);
    assert_eq!(h.app.session.lensfun.state(), lensfun_download::State::Idle);
    assert_eq!(h.app.ui.settings.lens_database, Offer::Declined, "a start that failed isn't a yes");

    // the control channel's confirm keeps it open too
    let r = h.request("ui.dialog.confirm", serde_json::json!({}), t);
    assert_eq!(r["ok"], false, "{r}");
    assert!(offer_open(&h), "the offer stays open after ui.dialog.confirm: {:?}", h.app.ui.dialog);
    assert_eq!(h.request("ui.dialog.cancel", serde_json::json!({}), t)["ok"], true);
    assert!(h.app.ui.dialog.is_none(), "Not Now still closes it");
}
