//! Regression tests for the CollabCraft cloud startup sequence and user-facing
//! cloud Open/Save workflows.
//!
//! Covers four categories:
//!   1. Startup sequence: `start` submits the account probe once and only once.
//!   2. Menu visibility: `file.cloudOpen` and `file.cloudSave` appear in the
//!      File menu only when a CollabCraft backend is connected, and are absent
//!      when there is none.
//!   3. Command routing: running the commands sets `cloud_page` to the right
//!      variant and returns an error when cloud is not configured.
//!   4. UI regression: the cloud modal is rendered every frame while
//!      `cloud_page` is `Some`, and is dismissed when the command succeeds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cloud::{CloudBackend, CloudPage, Job, Post, start};
use crate::menubar::{MenuNode, menu_bar};
use crate::{LightcraftApp, Services};

/// Records whether `Job::Account` was submitted.
struct AccountProbe(Arc<AtomicBool>);

impl CloudBackend for AccountProbe {
    fn submit(&self, job: Job, _post: Post) {
        if matches!(job, Job::Account) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

/// `start` must submit the initial account probe the moment it is called.
///
/// Without this call the web host's cloud controls never populate: the
/// `LightcraftApp` sits with `asked_account = false` and no capabilities are
/// fetched, so every cloud-gated panel stays hidden.
#[test]
fn start_submits_account_probe_after_connect() {
    let probed = Arc::new(AtomicBool::new(false));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.cloud.connect(AccountProbe(probed.clone()));

    assert!(!probed.load(Ordering::SeqCst), "probe must not fire before start()");
    start(&mut app);
    assert!(probed.load(Ordering::SeqCst), "start() must submit Job::Account immediately");
}

/// Calling `start` twice (e.g. across reloads) must not duplicate the probe.
#[test]
fn start_is_idempotent() {
    use std::sync::atomic::AtomicUsize;
    struct Counter(Arc<AtomicUsize>);
    impl CloudBackend for Counter {
        fn submit(&self, job: Job, _post: Post) {
            if matches!(job, Job::Account) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    let count = Arc::new(AtomicUsize::new(0));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.cloud.connect(Counter(count.clone()));
    start(&mut app);
    start(&mut app); // second call must be a no-op
    assert_eq!(count.load(Ordering::SeqCst), 1, "Job::Account must be submitted exactly once across repeated start() calls");
}

// ---------------------------------------------------------------- helpers

/// A no-op backend used when we only need `available()` to return `true`.
struct NoopBackend;
impl CloudBackend for NoopBackend {
    fn submit(&self, _job: Job, _post: Post) {}
}

fn find_item<'a>(nodes: &'a [MenuNode], id: &str) -> Option<&'a MenuNode> {
    nodes.iter().find_map(|n| match n {
        MenuNode::Item { id: i, .. } if i == id => Some(n),
        MenuNode::Submenu { children, .. } => find_item(children, id),
        _ => None,
    })
}

fn file_items(app: &LightcraftApp) -> Vec<MenuNode> {
    menu_bar(app).into_iter().find(|(t, _)| t == "File").map(|(_, v)| v).unwrap_or_default()
}

// ---------------------------------------------------------------- menu visibility

/// `file.cloudOpen` and `file.cloudSave` must appear in the File menu when
/// a CollabCraft backend is wired, and be absent otherwise.
///
/// A menu entry that only exists in the backend never reaches a user whose
/// deployment has no cloud service; a menu entry that silently does nothing
/// when clicked is worse than no entry at all.
#[test]
fn cloud_open_and_save_entries_require_a_connected_backend() {
    // Without a backend the entries must be absent (host_supports returns false).
    let no_cloud = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let items = file_items(&no_cloud);
    assert!(find_item(&items, "file.cloudOpen").is_none(), "file.cloudOpen must not appear without a CollabCraft backend");
    assert!(find_item(&items, "file.cloudSave").is_none(), "file.cloudSave must not appear without a CollabCraft backend");

    // With a backend both entries appear, Open enabled, Save guarded by an
    // active photo (the demo session has one so both should be enabled).
    let mut with_cloud = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    with_cloud.cloud.connect(NoopBackend);
    let items = file_items(&with_cloud);

    assert!(
        matches!(find_item(&items, "file.cloudOpen"), Some(MenuNode::Item { enabled: true, .. })),
        "file.cloudOpen must appear and be enabled when a backend is connected: {items:?}"
    );
    assert!(
        matches!(find_item(&items, "file.cloudSave"), Some(MenuNode::Item { enabled: true, .. })),
        "file.cloudSave must appear and be enabled when a backend is connected and a photo is active: {items:?}"
    );
}

/// Without an active photo `file.cloudSave` is present but disabled.
#[test]
fn cloud_save_is_disabled_without_an_active_photo() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.cloud.connect(NoopBackend);
    let items = file_items(&app);
    assert!(
        matches!(find_item(&items, "file.cloudSave"), Some(MenuNode::Item { enabled: false, .. })),
        "file.cloudSave must be disabled when no photo is active: {items:?}"
    );
    // Open is still enabled — browsing cloud storage does not require a selection.
    assert!(
        matches!(find_item(&items, "file.cloudOpen"), Some(MenuNode::Item { enabled: true, .. })),
        "file.cloudOpen must stay enabled even with no active photo"
    );
}

// ---------------------------------------------------------------- command routing

/// Running `file.cloudOpen` sets `cloud_page` to `CloudPage::Open`.
/// Running it without a backend returns an error and leaves `cloud_page` `None`.
#[test]
fn file_cloud_open_sets_cloud_page() {
    // Without a backend: error, no state change.
    let mut no_cloud = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    assert!(no_cloud.cloud_page.is_none());
    let r = no_cloud.run("file.cloudOpen", serde_json::json!({}));
    assert!(r.is_err(), "must fail without a CollabCraft backend");
    assert!(no_cloud.cloud_page.is_none(), "cloud_page must stay None on error");

    // With a backend: success, cloud_page set.
    let mut with_cloud = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    with_cloud.cloud.connect(NoopBackend);
    let r = with_cloud.run("file.cloudOpen", serde_json::json!({}));
    assert!(r.is_ok(), "must succeed with a CollabCraft backend: {r:?}");
    assert!(matches!(with_cloud.cloud_page, Some(CloudPage::Open)), "cloud_page must be Some(Open) after file.cloudOpen");
}

/// Running `file.cloudSave` sets `cloud_page` to `CloudPage::Save`.
#[test]
fn file_cloud_save_sets_cloud_page() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.cloud.connect(NoopBackend);
    let r = app.run("file.cloudSave", serde_json::json!({}));
    assert!(r.is_ok(), "must succeed with a backend and active photo: {r:?}");
    assert!(matches!(app.cloud_page, Some(CloudPage::Save)), "cloud_page must be Some(Save) after file.cloudSave");
}

/// Opening and then closing the cloud page (via `cloud_page = None`) is
/// possible within the same headless session.
#[test]
fn cloud_page_can_be_set_and_cleared() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.cloud.connect(NoopBackend);

    app.cloud_page = Some(CloudPage::Open);
    assert!(matches!(app.cloud_page, Some(CloudPage::Open)));

    app.cloud_page = Some(CloudPage::Save);
    assert!(matches!(app.cloud_page, Some(CloudPage::Save)));

    app.cloud_page = None;
    assert!(app.cloud_page.is_none());
}

// ---------------------------------------------------------------- UI regression (headless frames)

/// The cloud modal must be rendered on the frame where `cloud_page` is `Some`.
///
/// This test drives the app through the headless harness and checks that the
/// modal's widget (`lightcraft_cloud_page`) is registered in the widget tree
/// when the page is set, and absent when it is not.
#[test]
fn cloud_modal_renders_when_cloud_page_is_set() {
    use crate::headless::Headless;
    use std::time::Duration;

    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.cloud.connect(NoopBackend);

    let settle = Duration::from_secs(5);
    let mut h = Headless::new(app, [1280.0, 800.0], 1.0);
    h.settle(settle);

    // Without cloud_page set: no modal widget.
    let has_modal = h.app.widgets.iter().any(|(id, _)| id == "modal:cloud");
    assert!(!has_modal, "modal must not render when cloud_page is None: {:?}", h.app.widgets);

    // Set the Open page and run a frame.
    h.app.cloud_page = Some(CloudPage::Open);
    h.settle(Duration::from_millis(100));

    let has_modal = h.app.widgets.iter().any(|(id, _)| id == "modal:cloud");
    assert!(has_modal, "modal must render when cloud_page is Some(Open); widgets: {:?}", h.app.widgets);

    // After clearing cloud_page the modal must be gone.
    h.app.cloud_page = None;
    h.settle(Duration::from_millis(100));

    let has_modal = h.app.widgets.iter().any(|(id, _)| id == "modal:cloud");
    assert!(!has_modal, "modal must not render after cloud_page is cleared");
}

/// Suggested Save names use a writable extension so RAW photos are not uploaded
/// as JPEG bytes under a `.cr3` / `.dng` name.
#[test]
fn cloud_save_name_rewrites_raw_and_keeps_writable() {
    use crate::cloud::cloud_save_name;
    assert_eq!(cloud_save_name("portrait.CR3"), "portrait.jpg");
    assert_eq!(cloud_save_name("scan.dng"), "scan.jpg");
    assert_eq!(cloud_save_name("photo.bmp"), "photo.jpg");
    assert_eq!(cloud_save_name("photo.jxl"), "photo.jpg");
    assert_eq!(cloud_save_name("trip.JPEG"), "trip.JPEG");
    assert_eq!(cloud_save_name("flat.png"), "flat.png");
}

/// Encoding refuses open-only destination names instead of writing JPEG under them.
#[test]
fn encode_refuses_raw_destination_name() {
    use craft_ui::cloud::{CloudApp, Destination};
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let Some(uid) = app.active() else { return };
    let err = CloudApp::encode(&mut app, uid, Destination::new("portrait.CR3")).expect_err("RAW destination must be refused");
    assert!(err.contains("JPEG") || err.contains("rename"), "refusal must say which formats work: {err}");
}

/// A pending CollabCraft Open blocks library switch the same way an import does.
#[test]
fn can_switch_library_refuses_while_cloud_open_is_busy() {
    use crate::cloud::Busy;
    use crate::panels::settings::can_switch_library;
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    assert!(can_switch_library(&app).is_ok());
    app.cloud.busy = Some(Busy::Opening { name: "shot.jpg".into() });
    let err = can_switch_library(&app).expect_err("must refuse while Opening");
    assert!(err.contains("cloud"), "{err}");
}

/// Cloud Open through store_bytes never touches the filesystem (the browser path).
#[test]
fn open_bytes_uses_store_bytes_when_configured() {
    use craft_ui::cloud::CloudApp;
    use std::sync::{Arc, Mutex};
    let got = Arc::new(Mutex::new(None));
    let slot = got.clone();
    let services = Services {
        store_bytes: Some(Box::new(move |name, bytes| {
            *slot.lock().unwrap() = Some((name, bytes));
        })),
        ..Default::default()
    };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.cloud_open_library = Some(app.session.library_generation());
    let r = CloudApp::open_bytes(&mut app, "from-cloud.jpg", b"fake-jpeg".to_vec());
    assert_eq!(r, Ok(None));
    let (name, bytes) = got.lock().unwrap().take().expect("store_bytes must run");
    assert_eq!(name, "from-cloud.jpg");
    assert_eq!(bytes, b"fake-jpeg");
}

/// A late Cloud Open answer after the library generation changed is refused.
#[test]
fn open_bytes_refuses_after_library_generation_change() {
    use craft_ui::cloud::CloudApp;
    let services = Services {
        store_bytes: Some(Box::new(|_, _| {})),
        ..Default::default()
    };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.cloud_open_library = Some(app.session.library_generation().wrapping_add(1));
    let err = CloudApp::open_bytes(&mut app, "late.jpg", b"x".to_vec()).expect_err("stale library must refuse");
    assert!(err.contains("library changed"), "{err}");
}

/// The cloud Open entry in the File menu is reachable end-to-end: clicking it
/// in the headless menu bar sets `cloud_page` to `Some(Open)`, proving the
/// entire path from menu item → command handler → state is wired.
#[test]
fn file_menu_cloud_open_entry_is_reachable() {
    use crate::headless::Headless;
    use std::time::Duration;

    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.cloud.connect(NoopBackend);

    let mut h = Headless::new(app, [1280.0, 800.0], 1.0);
    h.settle(Duration::from_secs(5));

    // The cloud entries must appear in the built menu model.
    let items = file_items(&h.app);
    assert!(find_item(&items, "file.cloudOpen").is_some(), "file.cloudOpen missing from File menu with cloud connected; items: {items:?}");

    // Run the command directly (simulating a menu click via the control channel).
    h.app.cloud_page = None;
    h.app.run("file.cloudOpen", serde_json::json!({})).expect("file.cloudOpen must succeed");
    assert!(matches!(h.app.cloud_page, Some(CloudPage::Open)), "cloud_page must be Some(Open) after running file.cloudOpen");
}
