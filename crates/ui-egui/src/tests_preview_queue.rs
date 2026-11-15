//! PREVIEW-QUEUE: the Previews menu's builds stay offered while one runs (they wait for their
//! turn behind it), Stop covers the waiting ones too, and the full-screen preview keeps saying
//! what runs — quietly, top right, where no other activity surface is on the screen.

use std::time::Duration;

use lcc_engine::activity::Cancel;
use lcc_engine::cmd::previews::{PreviewBuild, QueuedPreview};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::menubar::{MenuNode, menu_bar};
use crate::{LccApp, Services};

// generous: renders run on worker threads that a busy machine (a full workspace test run) slows a lot
const T: Duration = Duration::from_secs(60);

/// A demo app, like the menu tests' (the Import tab off, so the menu bar is the window's own).
fn app() -> LccApp {
    let mut session = lcc_engine::Session::with_demo();
    session.selection = lcc_engine::Selection::single(session.visible_cloned()[0]);
    let mut a = LccApp::new(session, Services { png: None, ..Default::default() });
    a.ui.settings.import_as_tab = false;
    a
}

/// The Library ▸ Previews submenu of the menu bar.
fn previews_menu(app: &LccApp) -> Vec<MenuNode> {
    for (title, nodes) in menu_bar(app) {
        if title != "Library" {
            continue;
        }
        for n in nodes {
            if let MenuNode::Submenu { label, children } = n
                && label == "Previews"
            {
                return children;
            }
        }
    }
    panic!("no Library ▸ Previews submenu");
}

/// Whether the Previews menu offers the item running `id` with `params` (it is there).
fn offered(app: &LccApp, id: &str, params: &Value) -> bool {
    previews_menu(app)
        .into_iter()
        .find_map(|n| match n {
            MenuNode::Item { id: i, params: p, enabled, .. } if i == id && p == *params => Some(enabled),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{id} with {params} is not in the Previews menu"))
}

/// A build that runs (never started: its lanes would wait on nothing).
fn running_build() -> std::sync::Arc<PreviewBuild> {
    let mut b = PreviewBuild::default();
    b.total = 3;
    std::sync::Arc::new(b)
}

/// While a build runs, its menu's builds stay offered (they wait for their turn); the destructive
/// discards wait, and Stop covers the waiting builds too — also with nothing running but a queue.
#[test]
fn previews_menu_offers_builds_while_one_runs_and_stop_covers_the_queue() {
    let mut a = app();
    let (standard, full) = (json!({"size": "standard", "edge": a.ui.settings.standard_preview_edge()}), json!({"size": "full"}));
    // nothing runs, nothing waits: everything is offered, Stop is not
    assert!(offered(&a, "library.buildPreviews", &standard));
    assert!(offered(&a, "library.buildPreviews", &full));
    assert!(offered(&a, "library.discardPreviews", &json!({"size": "full"})));
    assert!(!offered(&a, "library.cancelPreviews", &Value::Null), "Stop with nothing to stop");

    // a build runs: the builds queue behind it; the discards wait for its end
    a.session.preview_build = Some(running_build());
    assert!(offered(&a, "library.buildPreviews", &standard), "a second build queues behind the running one");
    assert!(offered(&a, "library.buildPreviews", &full), "a 1:1 build queues too");
    assert!(!offered(&a, "library.discardPreviews", &json!({"size": "full"})), "a discard waits for the build's end");
    assert!(offered(&a, "library.cancelPreviews", &Value::Null), "Stop stops the running one");

    // builds waiting and nothing running: Stop drops them
    a.session.preview_build = None;
    a.session.preview_queue.push_back(QueuedPreview { command: "library.buildPreviews", params: full.clone() });
    assert!(offered(&a, "library.cancelPreviews", &Value::Null), "Stop drops the waiting ones");
    assert!(offered(&a, "library.buildPreviews", &full));
    assert!(offered(&a, "library.discardPreviews", &json!({"size": "full"})), "nothing runs");

    // both: Stop stops the running one and drops the waiting ones
    a.session.preview_build = Some(running_build());
    assert!(offered(&a, "library.cancelPreviews", &Value::Null));
    assert!(!offered(&a, "library.discardPreviews", &json!({"size": "full"})));
}

/// Asking for a build while one runs says it waits, as a toast, and the engine's answer is the
/// queue's (the running build keeps its row; no second one starts).
#[test]
fn asking_for_a_build_while_one_runs_says_it_waits() {
    let mut a = app();
    let running = running_build();
    a.session.preview_build = Some(running.clone());
    let r = a.run("library.buildPreviews", json!({"size": "full"})).unwrap();
    assert_eq!(r["queued"], true, "{r}");
    assert_eq!(a.session.preview_queue.len(), 1);
    assert!(a.session.preview_build.as_ref().is_some_and(|b| std::sync::Arc::ptr_eq(b, &running)), "the running build carries on");
    let toast = a.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
    assert_eq!(toast, "Queued: it starts when the running preview build finishes");
    // the same again: one waiting, the toast says the same
    a.ui.toasts.clear();
    let r = a.run("library.buildPreviews", json!({"size": "full"})).unwrap();
    assert_eq!(r["position"], 1, "{r}");
    assert_eq!(a.session.preview_queue.len(), 1);
}

/// The full-screen preview keeps saying what runs: a quiet card top right with the task's name
/// and a thin bar (or "N operations in progress…" over a bar each), and nothing while idle.
#[test]
fn full_screen_shows_the_running_tasks_top_right() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    let r = h.request("ui.set", json!({"fullscreen": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.fullscreen, "the full-screen preview");
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    assert!(!has(&h, "activity:fullscreen"), "nothing runs, nothing shows");

    let g = h.app.session.activity.start("export", "Exporting", Cancel::Yes);
    g.progress(1, 4);
    std::thread::sleep(Duration::from_millis(600));
    h.step();
    h.step();
    assert!(has(&h, "activity:fullscreen"), "the running task shows");
    assert!(has(&h, &format!("activity:fullscreen:bar:{}", g.id())), "with its bar");
    let card = h.app.widgets.iter().find(|(w, _)| w == "activity:fullscreen").map(|(_, r)| *r).unwrap();
    let screen = h.view.ctx.content_rect();
    assert!(card.right() <= screen.right() + 0.5 && card.left() > screen.width() * 0.6, "top right: {card:?} of {screen:?}");
    assert!(card.top() <= screen.top() + 60.0, "at the top: {card:?}");
    assert!(h.painted_text().iter().any(|t| t == "Exporting"), "{:?}", h.painted_text());

    // several: a bar each under "N operations in progress…" (a task shows once it has run
    // SHOW_AFTER_MS, as the first did above)
    let other = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    std::thread::sleep(Duration::from_millis(600));
    h.step();
    h.step();
    assert!(h.painted_text().iter().any(|t| t.contains("2 operations in progress")), "{:?}", h.painted_text());

    // the tasks end: the card goes
    drop(g);
    drop(other);
    h.step();
    h.step();
    assert!(!has(&h, "activity:fullscreen"), "gone with the last task");
}
