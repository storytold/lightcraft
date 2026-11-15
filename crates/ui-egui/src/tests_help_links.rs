//! The Settings window's help links (`crate::help_link`): every `?` names a topic the manual has,
//! the significant groups have one, and a click opens the manual at that topic.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

/// The help links Settings shows on `tab`.
fn links_on(h: &mut Headless, tab: &str) -> BTreeSet<String> {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    for _ in 0..3 {
        h.step();
    }
    h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("help:")).map(str::to_string).collect()
}

#[test]
fn settings_help_links_name_known_topics_and_open_the_manual() {
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let o = opened.clone();
    let services = Services {
        open_url: Some(Box::new(move |u: &str| {
            o.lock().unwrap().push(u.to_string());
            Ok(())
        })),
        ..Services::default()
    };
    let mut h = Headless::new(LccApp::new(lcc_engine::Session::with_demo(), services), [1600.0, 1000.0], 1.0);
    h.settle(T);

    // Given every tab of the Settings window
    let mut shown = BTreeSet::new();
    for (tab, _) in crate::panels::settings::shown_tabs() {
        let on_tab = links_on(&mut h, tab);
        // Then each help link names a topic of the table (which a unit test checks against the manual)
        for topic in &on_tab {
            assert!(crate::help_link::target(topic).is_some(), "Settings ▸ {tab} shows a help link to unknown topic {topic}");
        }
        shown.extend(on_tab);
    }
    // And the significant groups have one (a few depend on what this build or computer has)
    let conditional =
        ["autoImport", "folderPolling", "xmpSidecars", "importMetadata", "smartPreviews", "sam3", "semanticSearch", "faceScan", "denoiseModels"];
    for (topic, _) in crate::help_link::TOPICS {
        if !conditional.contains(topic) {
            assert!(shown.contains(*topic), "no Settings page shows the help link {topic}; shown: {shown:?}");
        }
    }

    // When a help link is clicked
    links_on(&mut h, "general");
    let r = h.request("ui.clickWidget", json!({"id": "help:catalog"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
    // Then the manual opens at that topic
    assert_eq!(opened.lock().unwrap().as_slice(), [crate::help_link::url("catalog").unwrap()]);
}
