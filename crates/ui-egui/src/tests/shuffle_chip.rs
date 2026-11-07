//! A shuffled view says so in the grid header, with Reshuffle beside it (LR-FILT-SORT), and the
//! sort button names which sort is chosen.

use std::time::Duration;

use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::headless::Headless;

const T: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(120);

fn demo(size: [f32; 2], view: &str) -> Headless {
    let services = crate::Services { png: None, ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), size, 1.0);
    let r = h.request("ui.set", json!({"view": view, "leftPanel": false, "right": "none"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn exec(h: &mut Headless, command: &str, params: Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command} {params}: {r}");
    h.settle(SETTLE);
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn tooltip_shown(h: &Headless) -> bool {
    h.view.ctx.memory(|m| m.areas().visible_layer_ids().into_iter().any(|l| l.order == egui::Order::Tooltip))
}

const RESHUFFLE: &str = "button:reshuffle";
const SHUFFLED: &str = "grid:shuffled";

/// The chip shows for a shuffle of a source that follows the sort, in both grids, and not for any
/// other order.
#[test]
fn the_header_says_shuffled_only_under_random_in_a_source_that_follows_the_sort() {
    for view in ["photoGrid", "squareGrid"] {
        let mut h = demo([1200.0, 800.0], view);
        assert!(widget(&h, RESHUFFLE).is_none() && widget(&h, SHUFFLED).is_none(), "{view}: sorted by date: no chip");
        exec(&mut h, "library.sort", json!({"key": "fileName"}));
        assert!(widget(&h, SHUFFLED).is_none(), "{view}: another key: no chip");
        exec(&mut h, "library.sort", json!({"key": "random"}));
        let (chip, count) = (widget(&h, SHUFFLED).unwrap_or_else(|| panic!("{view}: no chip")), widget(&h, "grid:count").unwrap());
        let button = widget(&h, RESHUFFLE).unwrap_or_else(|| panic!("{view}: no Reshuffle"));
        assert!(h.painted_text().iter().any(|t| t == "Shuffled"), "{view}: the chip reads Shuffled");
        assert!(
            chip.right() <= button.left() + 0.5 && button.right() <= count.left(),
            "{view}: chip {chip:?}, button {button:?} sit before the count {count:?}"
        );
        // Recently Added ignores the sort: it says nothing about it
        exec(&mut h, "library.source", json!({"kind": "recentlyAdded"}));
        assert!(widget(&h, SHUFFLED).is_none() && widget(&h, RESHUFFLE).is_none(), "{view}: Recently Added ignores the sort: no chip");
        // Recently Deleted is sorted like any other view (and empty it has nothing to shuffle)
        exec(&mut h, "library.source", json!({"kind": "recentlyDeleted"}));
        assert!(widget(&h, SHUFFLED).is_none(), "{view}: nothing deleted: no chip");
        exec(&mut h, "library.source", json!({"kind": "all"}));
        let eight: Vec<u64> = h.app.session.visible_cloned().iter().take(8).map(|p| p.0).collect();
        exec(&mut h, "library.select", json!({"ids": eight}));
        exec(&mut h, "photo.delete", json!({}));
        exec(&mut h, "library.source", json!({"kind": "recentlyDeleted"}));
        assert_eq!(h.app.session.visible_cloned().len(), 8);
        assert!(widget(&h, SHUFFLED).is_some() && widget(&h, RESHUFFLE).is_some(), "{view}: Recently Deleted is shuffled: chip");
        let seed = h.app.session.sort.seed;
        let r = h.request("ui.clickWidget", json!({"id": RESHUFFLE}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_ne!(h.app.session.sort.seed, seed, "{view}: Reshuffle reshuffles Recently Deleted");
        exec(&mut h, "library.source", json!({"kind": "all"}));
        assert!(widget(&h, SHUFFLED).is_some(), "{view}: back to All Photos: the chip is back");
        exec(&mut h, "library.sort", json!({"key": "captureDate"}));
        assert!(widget(&h, SHUFFLED).is_none() && widget(&h, RESHUFFLE).is_none(), "{view}: sorted again: no chip");
    }
}

/// "Shuffled" is the state (quiet text) and Reshuffle the action: the button shows its border at
/// rest, in every theme, and keeps a gap from the label, so the two don't read as one phrase.
#[test]
fn reshuffle_reads_as_a_button_beside_the_label() {
    for theme in ["charcoal", "midnight", "paper", "silver"] {
        let mut h = demo([1200.0, 800.0], "photoGrid");
        exec(&mut h, "view.theme", json!({"theme": theme}));
        exec(&mut h, "library.sort", json!({"key": "random"}));
        let (word, button) = (widget(&h, SHUFFLED).expect("the label"), widget(&h, RESHUFFLE).expect("the button"));
        let outlines = |of: egui::Rect| {
            h.view
                .painted_shapes()
                .iter()
                .filter(|s| matches!(s, egui::epaint::Shape::Rect(r) if r.stroke.width > 0.0 && r.rect.expand(0.5).contains_rect(of) && of.expand(0.5).contains_rect(r.rect)))
                .count()
        };
        assert_eq!(outlines(button), 1, "{theme}: Reshuffle shows its border at rest");
        assert_eq!(outlines(word), 0, "{theme}: the label stays plain text");
        assert!(button.left() - word.right() >= 8.0, "{theme}: a gap between the state and the action: {word:?} {button:?}");
    }
}

/// One click on the chip's Reshuffle is a new shuffle, and the chip stays.
#[test]
fn reshuffle_in_the_header_gives_a_new_order() {
    let mut h = demo([1200.0, 800.0], "photoGrid");
    exec(&mut h, "library.sort", json!({"key": "random"}));
    let (seed, order) = (h.app.session.sort.seed, h.app.session.visible_cloned());
    let r = h.request("ui.clickWidget", json!({"id": RESHUFFLE}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_ne!(h.app.session.sort.seed, seed, "a new seed");
    assert_ne!(h.app.session.visible_cloned(), order, "a new order");
    assert_eq!(h.app.session.sort.key, lightcraft_catalog::SortKey::Random, "still a shuffle");
    assert!(widget(&h, RESHUFFLE).is_some(), "the chip stays");
}

/// The Local folder header has its own layout: the chip is there too, and its count stays.
#[test]
fn a_shuffled_local_folder_says_so_too() {
    let dir = std::env::temp_dir().join(format!("lc-ui-shuffle-chip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    for i in 0..3u8 {
        let img = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 40 * i, 255]);
        std::fs::write(dir.join(format!("p{i}.png")), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    }
    let mut h = demo([1200.0, 800.0], "photoGrid");
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert!(widget(&h, RESHUFFLE).is_none(), "sorted: no chip");
    exec(&mut h, "library.sort", json!({"key": "random"}));
    let (button, count) = (widget(&h, RESHUFFLE).expect("Reshuffle in the folder header"), widget(&h, "grid:count").unwrap());
    assert!(button.right() <= count.left(), "{button:?} before {count:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// However narrow the window, the count stays on screen, Reshuffle stays reachable and nothing
/// overlaps.
#[test]
fn the_chip_never_pushes_the_count_off_a_narrow_header() {
    for width in [520.0f32, 400.0, 340.0] {
        let mut h = demo([width, 700.0], "photoGrid");
        exec(&mut h, "library.sort", json!({"key": "random"}));
        let (title, count) = (widget(&h, "grid:title").unwrap(), widget(&h, "grid:count").unwrap());
        let button = widget(&h, RESHUFFLE).unwrap_or_else(|| panic!("{width}: no Reshuffle"));
        assert!(count.right() <= width && count.left() >= 0.0 && button.left() >= 0.0, "{width}: on screen: {count:?} {button:?}");
        assert!(button.right() <= count.left() + 0.5, "{width}: {button:?} left of {count:?}");
        assert!(title.right() <= button.left() + 0.5, "{width}: title {title:?} left of {button:?}");
        if let Some(chip) = widget(&h, SHUFFLED) {
            assert!(title.right() <= chip.left() + 0.5 && chip.right() <= button.left() + 0.5, "{width}: {title:?} {chip:?} {button:?}");
        }
    }
}

/// A long count (a selection) leaves the title no room beside the chip: the chip gives way, and
/// never covers the title or the count. A little wider, the chip is back.
#[test]
fn a_long_count_makes_the_chip_give_way_rather_than_overlap() {
    // (a Local folder or the library, the narrowest window that still has the chip, one narrower)
    for (folder, shown, dropped) in [(false, 420.0f32, 340.0f32), (true, 440.0, 330.0)] {
        for width in [shown, dropped] {
            let mut h = demo([width, 700.0], "photoGrid");
            exec(&mut h, "library.sort", json!({"key": "random"}));
            if folder {
                let dir = std::env::temp_dir().join(format!("lc-ui-shuffle-chip-narrow-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
                for i in 0..12u8 {
                    let img = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 20 * i, 255]);
                    std::fs::write(dir.join(format!("p{i}.png")), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
                }
                exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
            }
            let all: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
            exec(&mut h, "library.select", json!({"ids": all}));
            let count = widget(&h, "grid:count").expect("the count");
            assert!(count.left() >= 0.0 && count.right() <= width, "folder {folder}, {width}: the count stays on screen: {count:?}");
            if width == shown {
                let button = widget(&h, RESHUFFLE).unwrap_or_else(|| panic!("folder {folder}, {width}: the chip fits"));
                assert!(button.left() >= 0.0 && button.right() <= count.left() + 0.5, "folder {folder}, {width}: {button:?} before {count:?}");
            } else {
                assert!(
                    widget(&h, RESHUFFLE).is_none() && widget(&h, SHUFFLED).is_none(),
                    "folder {folder}, {width}: no room beside the title: no chip"
                );
            }
            if folder {
                let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("lc-ui-shuffle-chip-narrow-{}", std::process::id())));
            }
        }
    }
}

/// The sort button's tooltip names the sort setting, as the menu words it.
#[test]
fn the_sort_button_names_the_current_sort() {
    let mut h = demo([1200.0, 800.0], "photoGrid");
    for (params, said) in [
        (json!({"key": "captureDate", "ascending": false}), "Sort: Capture Date, Descending"),
        (json!({"key": "fileName", "ascending": true}), "Sort: File Name, Ascending"),
        (json!({"key": "random"}), "Sort: Random"),
    ] {
        exec(&mut h, "library.sort", params);
        let c = widget(&h, "icon:sort").expect("the sort button").center();
        let r = h.request("ui.move", json!({"x": c.x, "y": c.y}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.step_until(Duration::from_secs(5), tooltip_shown), "{said}: a tooltip appears");
        // (the tooltip fades in over a few frames)
        assert!(h.step_until(Duration::from_secs(5), |h| h.painted_text().iter().any(|t| t == said)), "{said}: not in {:?}", h.painted_text());
        let r = h.request("ui.move", json!({"x": 5.0, "y": 5.0}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
}
