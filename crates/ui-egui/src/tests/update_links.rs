//! Issue #3689: the update prompt's What's New link (and its Download button) open through
//! `egui::Context::open_url`, which the desktop build's eframe can't act on (no egui link opener
//! compiled in). The app hands those links to the host's `open_url`, as it does its own.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const SETTLE: Duration = Duration::from_secs(60);
const RELEASE: &str = "https://github.com/storytold/lightcraft/releases/tag/v99.0.0";

fn text_rect(h: &Headless, text: &str) -> Option<egui::Rect> {
    h.view.painted_shapes().into_iter().find_map(|s| match s {
        egui::epaint::Shape::Text(t) if t.galley.job.text == text => Some(t.visual_bounding_rect()),
        _ => None,
    })
}

/// A headless app whose host `open_url` records each URL and answers with `answer`, showing the
/// update prompt for [`RELEASE`]; then a click on its What's new link.
fn click_whats_new(answer: Result<(), String>) -> (Headless, Arc<Mutex<Vec<String>>>) {
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = opened.clone();
    let services = Services {
        png: None,
        open_url: Some(Box::new(move |url: &str| {
            seen.lock().unwrap().push(url.to_string());
            answer.clone()
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), services), [1200.0, 800.0], 1.0);
    h.settle(SETTLE);
    h.app.updates_rt.offer(craft_update::Release {
        tag: "v99.0.0".into(),
        version: craft_update::parse_version("99.0.0"),
        name: "LightCraft 99.0.0".into(),
        html_url: RELEASE.into(),
        ..Default::default()
    });
    for _ in 0..4 {
        h.step();
    }
    let link = text_rect(&h, "What's new").expect("the update prompt shows its What's new link");
    let at = link.center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    for _ in 0..3 {
        h.step();
    }
    (h, opened)
}

#[test]
fn the_update_prompts_whats_new_opens_through_the_host() {
    let (_h, opened) = click_whats_new(Ok(()));
    assert_eq!(*opened.lock().unwrap(), vec![RELEASE.to_string()], "the link went to the host's open_url, once");
}

/// A link the host couldn't open says so, with the link and the host's reason.
#[test]
fn a_link_the_host_cant_open_shows_an_error() {
    let (h, opened) = click_whats_new(Err("no browser".into()));
    assert_eq!(opened.lock().unwrap().len(), 1);
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains(RELEASE) && toast.contains("no browser"), "{toast:?}");
}
