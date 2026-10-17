//! The Settings tabs are grouped by what a person comes to do: the library itself (where it is,
//! what leaves it by itself) under Library; what is written beside and opened from the photo
//! files under Files; and the tabs for the monitor and for how LightCraft looks named for that.
//!
//! Before, General held the library's location beside the language and the external editors,
//! XMP sidecar writing (which governs every later edit) sat under Import, and forgetting unchanged
//! photos (catalog housekeeping) under Performance. "Display" was the monitor's colour profile and
//! "Interface" was how the app looks.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::settings::TABS;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1200.0, 1000.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn show(h: &mut Headless, tab: &str) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    for _ in 0..4 {
        h.step();
    }
}

/// Every section heading of the open page, top to bottom (`heading:{its English name}`).
fn headings(h: &Headless) -> Vec<String> {
    let mut found: Vec<(f32, String)> =
        h.app.widgets.iter().filter_map(|(id, r)| Some((r.top(), id.strip_prefix("heading:")?.to_string()))).collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.into_iter().map(|(_, text)| text).collect()
}

#[test]
fn the_tabs_are_named_and_ordered_for_what_they_hold() {
    let names: Vec<(&str, &str)> = TABS.iter().take(10).map(|(id, label)| (*id, crate::panels::settings::tab_label(id, label))).collect();
    assert_eq!(
        names,
        [
            ("general", "General"),
            ("library", "Library"),
            ("import", "Import"),
            ("files", "Files"),
            ("performance", "Performance"),
            ("display", "Monitor"),
            ("interface", "Appearance"),
            ("slideshow", "Slideshow"),
            ("faces", "Faces"),
            ("denoise", "AI Denoise"),
        ]
    );
}

#[test]
fn each_section_is_under_the_tab_a_person_would_look_in() {
    let mut h = demo();
    // (every heading a page has, so a section that turns up on a tab it does not belong to is seen)
    let mut under = |tab: &str| {
        show(&mut h, tab);
        headings(&h)
    };
    // General is about the app as a whole
    assert_eq!(under("general"), ["Startup", "Culling"]);
    // Library is the library itself: where it is, and what leaves it by itself
    assert_eq!(under("library"), ["Library", "Local folders"]);
    // Import is what happens to photos as they come in, from a camera or card or a watched folder too
    assert_eq!(under("import"), ["Raw defaults", "Other images (JPEG, PNG, TIFF, HEIC…)", "Metadata", "Cameras and Cards", "Auto Import"]);
    // Files is what is written beside the photo files and what they are opened in
    assert_eq!(under("files"), ["XMP sidecars", "External Editing"]);
    // Performance is speed and the cache that buys it
    assert_eq!(under("performance"), ["Rendering", "Thumbnail cache"]);
    assert_eq!(under("display"), ["Monitor profile"]);
    assert_eq!(under("interface"), ["Appearance", "Filmstrip", "Grid", "Keywords", "Detail"]);
    // Slideshow is what the slideshow looks like and how it plays, next to how LightCraft looks
    assert_eq!(under("slideshow"), ["Preview", "Templates", "Playback", "Layout", "Options", "Backdrop", "Overlays", "Titles", "Export"]);
    // Faces and AI Denoise are as they were (here without a way to download models)
    assert_eq!(under("faces"), ["Face recognition"]);
    assert_eq!(under("denoise"), ["AI Denoise"]);
    // And every tab there is was looked at
    assert_eq!(TABS.len(), 10, "a new tab needs its sections listed here");
}

#[test]
fn what_is_saved_with_a_library_in_memory_says_it_will_not_last() {
    let mut h = demo();
    let note = "In-memory session: these settings last until LightCraft quits.";
    for tab in ["library", "import", "files"] {
        show(&mut h, tab);
        assert!(h.painted_text().iter().any(|text| text == note), "{tab} says so");
    }
    show(&mut h, "general");
    assert!(!h.painted_text().iter().any(|text| text == note), "General is saved on this computer");
}

#[test]
fn every_tab_id_from_before_still_opens_its_tab() {
    let mut h = demo();
    // Given the ids agents, the docs and the rest of the app have used so far, and the two new ones
    for id in ["general", "import", "performance", "display", "interface", "slideshow", "faces", "denoise", "library", "files"] {
        show(&mut h, id);
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: id.into() }), "{id}");
    }
    // And an id that names no tab is refused, saying which there are
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "nowhere"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("library") && r.to_string().contains("files"), "the refusal lists the tabs: {r}");
}

#[test]
fn controls_keep_their_names_where_they_moved_to() {
    let mut h = demo();
    let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
    // (what scripts and agents click is found by the same name on its new tab)
    show(&mut h, "library");
    assert!(has(&h, "button:settingsOpenLibrary") && has(&h, "text:settingsLocation") && has(&h, "button:settingsForgetLocal-0"));
    show(&mut h, "files");
    assert!(has(&h, "check:settings.autoWriteXmp") && has(&h, "field:externalEditor") && has(&h, "field:photocraftPath"));
    show(&mut h, "general");
    assert!(has(&h, "button:settingsLanguage-0") && has(&h, "check:settings.autoAdvance"));
    assert!(!has(&h, "button:settingsOpenLibrary") && !has(&h, "field:externalEditor"), "and no longer on General");
    show(&mut h, "import");
    assert!(has(&h, "field:settings.copyright") && !has(&h, "check:settings.autoWriteXmp"));
    show(&mut h, "performance");
    assert!(has(&h, "button:settingsClearCache") && !has(&h, "button:settingsForgetLocal-0"));
}

#[test]
fn a_library_on_disk_has_its_smart_previews_folder_under_library() {
    let dir = std::env::temp_dir().join(format!("lc-ui-settings-groups-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&dir, true).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1200.0, 1000.0], 1.0);
    h.settle(T);
    show(&mut h, "performance");
    let on_performance = headings(&h);
    show(&mut h, "library");
    let on_library = headings(&h);
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
    // (where a library keeps its files is the library's; Performance keeps rendering and the cache)
    assert_eq!(on_library, ["Library", "Smart previews", "Local folders"]);
    assert_eq!(on_performance, ["Rendering", "Thumbnail cache"]);
}

#[test]
fn the_tab_bar_shows_the_names_people_read() {
    let mut h = demo();
    show(&mut h, "general");
    let texts = h.painted_text();
    for name in ["General", "Library", "Import", "Files", "Performance", "Monitor", "Appearance", "Slideshow", "Faces", "AI Denoise"] {
        assert!(texts.iter().any(|text| text == name), "the tab bar has {name}");
    }
    assert!(!texts.iter().any(|text| text == "Display" || text == "Interface"), "and not the names from before");
}
