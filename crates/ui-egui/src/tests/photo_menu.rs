//! The photo context menu as a person uses it: right-click a thumbnail in the grid or the
//! filmstrip, open a submenu, click a row.

use std::time::Duration;

use lightcraft_catalog::{Op, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo(view: &str) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": view}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn right_click(h: &mut Headless, id: &str) {
    let c = widget(h, id).unwrap_or_else(|| panic!("no {id}")).center();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

fn go(h: &mut Headless, method: &str, id: &str) {
    let r = h.request(method, json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{method} {id}: {r}");
    h.step();
    h.step();
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

/// Give a demo photo a file on disk (the path is only read, never opened, by these rows).
fn on_disk(h: &mut Headless, id: PhotoId, path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    h.app.session.catalog.apply(Op::SetFile { id, file_name: name, source: Source::File { path: path.into() } }).unwrap();
}

/// In Detail the filmstrip's menu has no "Open in Detail" (the photo is already there), and
/// Copy File Path puts the right-clicked photo's path on the clipboard.
#[test]
fn the_filmstrip_menu_copies_the_file_path() {
    let mut h = demo("detail");
    let p = h.app.session.visible_cloned()[2];
    on_disk(&mut h, p, "/pics/trip/a.jpg");
    h.step();
    right_click(&mut h, &format!("film:{}", p.0));
    assert_eq!(h.app.session.selected(), vec![p], "the right-click selects the photo");
    assert!(widget(&h, "photomenu:view.detail").is_none(), "no Open in Detail while in Detail");
    assert!(widget(&h, "photomenu:photo.rotateLeft").is_none(), "Rotate sits in Rotate & Flip");
    go(&mut h, "ui.clickWidget", "photomenu:photo.copyPath");
    h.step();
    assert_eq!(h.app.copied.as_deref(), Some("/pics/trip/a.jpg"));
    assert!(toast(&h).contains("Copied"), "{}", toast(&h));
}

/// A row whose command refuses says why, in the command's own words, instead of doing nothing.
#[test]
fn a_refused_row_says_why() {
    let mut h = demo("photoGrid");
    let p = h.app.session.visible_cloned()[0];
    h.app.session.execute("library.select", &json!({"ids": [p.0]})).unwrap();
    let expected = h.app.run("photo.duplicate", json!({})).expect_err("a demo photo has no file to duplicate");
    h.app.ui.toast = None;
    right_click(&mut h, &format!("thumb:{}", p.0));
    go(&mut h, "ui.clickWidget", "photomenu:photo.duplicate");
    assert_eq!(toast(&h), expected);
}

/// Metadata ▸ Copy Metadata on one photo and Metadata ▸ Paste Metadata on another, through the
/// submenu, as a person does it.
#[test]
fn metadata_is_copied_and_pasted_from_the_submenu() {
    let mut h = demo("photoGrid");
    let v = h.app.session.visible_cloned();
    let (from, to) = (v[0], v[1]);
    h.app.run("photo.setMeta", json!({"ids": [from.0], "title": "Harbor at dawn"})).unwrap();
    h.step();
    right_click(&mut h, &format!("thumb:{}", from.0));
    go(&mut h, "ui.hoverWidget", "photomenusub:Metadata");
    go(&mut h, "ui.clickWidget", "photomenu:photo.copyMetadata");
    right_click(&mut h, &format!("thumb:{}", to.0));
    go(&mut h, "ui.hoverWidget", "photomenusub:Metadata");
    go(&mut h, "ui.clickWidget", "photomenu:photo.pasteMetadata");
    assert_eq!(h.app.session.catalog.photo(to).unwrap().meta.title, "Harbor at dawn");
}

/// Rotate & Flip ▸ Flip Horizontal acts on the selection, like Photo ▸ Flip Horizontal.
#[test]
fn rotate_and_flip_flips_the_photo() {
    let mut h = demo("photoGrid");
    let p = h.app.session.visible_cloned()[0];
    let before = h.app.session.catalog.photo(p).unwrap().develop.clone();
    right_click(&mut h, &format!("thumb:{}", p.0));
    go(&mut h, "ui.hoverWidget", "photomenusub:Rotate & Flip");
    go(&mut h, "ui.clickWidget", "photomenu:photo.flipHorizontal");
    assert_ne!(h.app.session.catalog.photo(p).unwrap().develop, before, "the photo is flipped");
}

/// In a short window the menu stays inside it and scrolls (like the menu bar's menus) instead of
/// running past its bottom edge, where the last rows, Delete among them, couldn't be reached: in
/// the grid, and in the loupe and Compare, whose own rows (Zoom; Swap) sit in the same menu.
#[test]
fn the_menu_scrolls_in_a_short_window() {
    for (view, n, target) in [("photoGrid", 1, "thumb"), ("detail", 1, "canvas:image"), ("compare", 2, "cull")] {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1200.0, 420.0], 1.0);
        let v = h.app.session.visible_cloned();
        h.app.session.execute("library.select", &json!({"ids": v[..n].iter().map(|p| p.0).collect::<Vec<_>>()})).unwrap();
        let r = h.request("ui.set", json!({"view": view}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let id = if target == "canvas:image" { target.to_string() } else { format!("{target}:{}", v[0].0) };
        right_click(&mut h, &id);
        h.step();
        let level = widget(&h, "menu-level:1").unwrap_or_else(|| panic!("{view}: the menu is a bounded level"));
        assert!(level.top() >= -0.5 && level.bottom() <= 420.5, "{view}: the menu {level:?} runs past the 420 pt window");
        let rows: Vec<egui::Rect> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("photomenu")).map(|(_, r)| *r).collect();
        assert!(rows.iter().any(|r| !level.contains_rect(*r)), "{view}: some rows are scrolled out of view, none drawn past the window");
    }
}

/// Browsing a folder (every chosen photo Local), the photo's menu offers Add to My Photos, and
/// the row adds the photo to the library.
#[test]
fn local_photos_are_offered_add_to_my_photos() {
    let mut h = demo("photoGrid");
    let dir = std::env::temp_dir().join(format!("lc-photo-menu-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(dir.join("a.png"), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let local = h.app.session.visible_cloned()[0];
    assert!(h.app.session.catalog.photo(local).unwrap().local);
    h.app.session.execute("library.select", &json!({"ids": [local.0]})).unwrap();
    h.step();
    right_click(&mut h, &format!("thumb:{}", local.0));
    go(&mut h, "ui.clickWidget", "photomenu:photo.addToLibrary");
    h.settle(SETTLE);
    assert_eq!(h.app.session.catalog.photo(local).map(|p| p.local), Some(false), "the photo joined My Photos");
    let _ = std::fs::remove_dir_all(&dir);
}

/// In Survey, rating from the menu of a photo that wasn't the active one rates that photo (the
/// one whose rating the menu checks), not the one active before the right-click.
#[test]
fn survey_rates_the_right_clicked_photo() {
    let mut h = demo("photoGrid");
    let v = h.app.session.visible_cloned();
    let (active, other) = (v[0], v[1]);
    h.app.session.execute("library.select", &json!({"ids": [active.0, other.0]})).unwrap();
    let r = h.request("ui.set", json!({"view": "survey"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.active(), Some(active));
    let before = h.app.session.catalog.photo(active).unwrap().rating;
    right_click(&mut h, &format!("cull:{}", other.0));
    go(&mut h, "ui.hoverWidget", "photomenusub:Set Rating");
    let three = h
        .app
        .widgets
        .iter()
        .map(|(w, _)| w.clone())
        .find(|w| w.starts_with("photomenu:photo.rate|") && w.contains("\"rating\":3"))
        .expect("the three-star row");
    go(&mut h, "ui.clickWidget", &three);
    assert_eq!(h.app.session.catalog.photo(other).unwrap().rating, 3, "the right-clicked photo is rated");
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, before, "the active photo is left alone");
}

/// Find Similar Photos from the filmstrip looks for photos like the right-clicked one and shows
/// them in the grid, saying how many it found.
#[test]
fn find_similar_shows_the_grid_and_says_how_many() {
    let mut h = demo("detail");
    let p = h.app.session.visible_cloned()[1];
    right_click(&mut h, &format!("film:{}", p.0));
    let row = h.app.widgets.iter().map(|(w, _)| w.clone()).find(|w| w.starts_with("photomenu:library.findSimilar|")).expect("the row");
    assert!(row.contains(&format!("\"id\":{}", p.0)), "the row names the right-clicked photo: {row}");
    go(&mut h, "ui.clickWidget", &row);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
    assert!(toast(&h).contains("similar photo"), "{}", toast(&h));
}

/// Compare's own rows open in the same menu as the photo's: Swap there swaps the two photos.
/// The loupe's Zoom rows sit in a Zoom submenu of the same menu.
#[test]
fn compare_and_the_loupe_add_their_rows_to_the_menu() {
    let mut h = demo("photoGrid");
    let v = h.app.session.visible_cloned();
    h.app.session.execute("library.select", &json!({"ids": [v[0].0, v[1].0]})).unwrap();
    let r = h.request("ui.set", json!({"view": "compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let before = crate::panels::compare::compare_pair(&mut h.app).expect("two photos to compare");
    right_click(&mut h, &format!("cull:{}", before.0.0));
    assert!(widget(&h, "photomenu:compare.makeSelect").is_some(), "Make Candidate the Select is in the menu");
    assert!(widget(&h, "photomenu:photo.delete").is_some(), "with the photo's own rows");
    go(&mut h, "ui.clickWidget", "photomenu:compare.swap");
    assert_eq!(crate::panels::compare::compare_pair(&mut h.app), Some((before.1, before.0)), "Swap swapped the two photos");

    let mut h = demo("detail");
    right_click(&mut h, "canvas:image");
    go(&mut h, "ui.hoverWidget", "photomenusub:Zoom");
    for z in ["view.zoomFit", "view.zoom100", "view.zoomIn", "view.zoomOut"] {
        assert!(widget(&h, &format!("photomenu:{z}")).is_some(), "no {z} row");
    }
    assert!(widget(&h, "photomenu:photo.delete").is_some(), "the photo's own rows follow");
}

/// Right-clicking a selected photo that isn't the active one makes it the active one and keeps
/// the selection (as a click does in Survey), so the rows that act on the active photo act on
/// it; with Auto Advance on, rating it from its menu moves on from it, as the keys do. A rating
/// aimed at another photo by `ids` (a script's) still leaves the active photo where it is.
#[test]
fn a_right_click_makes_the_photo_active_and_rating_it_moves_on() {
    let mut h = demo("photoGrid");
    let v = h.app.session.visible_cloned();
    h.app.session.execute("library.select", &json!({"ids": [v[0].0, v[1].0, v[2].0]})).unwrap();
    let r = h.request("ui.set", json!({"view": "survey"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h.app.ui.auto_advance = true;
    assert_eq!(h.app.session.active(), Some(v[0]));
    right_click(&mut h, &format!("cull:{}", v[1].0));
    assert_eq!(h.app.session.active(), Some(v[1]), "the right-clicked photo is the active one");
    assert_eq!(h.app.session.selection.ids.len(), 3, "the selection stays");
    go(&mut h, "ui.hoverWidget", "photomenusub:Set Rating");
    let row =
        h.app.widgets.iter().map(|(w, _)| w.clone()).find(|w| w.starts_with("photomenu:photo.rate|") && w.contains("\"rating\":2")).expect("the row");
    go(&mut h, "ui.clickWidget", &row);
    assert_eq!(h.app.session.catalog.photo(v[1]).unwrap().rating, 2);
    assert_eq!(h.app.session.active(), Some(v[2]), "rating the photo moved on from it");
    crate::menubar::run_item(&mut h.app, "photo.rate", json!({"ids": [v[0].0], "rating": 4})).unwrap();
    assert_eq!(h.app.session.catalog.photo(v[0]).unwrap().rating, 4);
    assert_eq!(h.app.session.active(), Some(v[2]), "a rating aimed at another photo leaves the active one");
}

/// In the grid, right-clicking one of several selected photos makes it the active one and keeps
/// the selection, so Copy Edit Settings copies that photo's settings.
#[test]
fn a_right_click_in_the_grid_keeps_the_selection() {
    let mut h = demo("photoGrid");
    let v = h.app.session.visible_cloned();
    h.app.session.execute("library.select", &json!({"ids": [v[0].0, v[1].0, v[2].0]})).unwrap();
    h.app.session.execute("develop.set", &json!({"control": "light.exposure", "value": 1.5, "ids": [v[1].0]})).unwrap();
    h.step();
    assert_eq!(h.app.session.active(), Some(v[0]));
    right_click(&mut h, &format!("thumb:{}", v[1].0));
    assert_eq!(h.app.session.active(), Some(v[1]), "the right-clicked photo is the active one");
    assert_eq!(h.app.session.selection.ids.len(), 3, "the selection stays");
    go(&mut h, "ui.hoverWidget", "photomenusub:Edit Settings");
    go(&mut h, "ui.clickWidget", "photomenu:develop.copy");
    let copied = h.app.session.clipboard.as_ref().and_then(|c| c["light"]["exposure"].as_f64());
    assert_eq!(copied, Some(1.5), "Copy Edit Settings copied the right-clicked photo's settings");
}

/// In Compare, a selected filmstrip photo that neither pane shows stays as it is on a
/// right-click: the active photo (the one the rating keys and the Edit panel act on) stays on
/// screen (#744).
#[test]
fn a_right_click_in_compare_keeps_the_active_photo_on_screen() {
    let mut h = demo("photoGrid");
    let v = h.app.session.visible_cloned();
    h.app.session.execute("library.select", &json!({"ids": [v[0].0, v[1].0]})).unwrap();
    let r = h.request("ui.set", json!({"view": "compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let active = h.app.session.active();
    let (a, b) = crate::panels::compare::compare_pair(&mut h.app).expect("two photos to compare");
    let extra = v.iter().copied().find(|p| *p != a && *p != b).expect("a third photo");
    let ids: Vec<u64> = [a, b, extra].iter().map(|p| p.0).collect();
    h.app.session.execute("library.select", &json!({"ids": ids, "active": active.map(|p| p.0)})).unwrap();
    h.settle(SETTLE);
    assert!(active == Some(a) || active == Some(b), "the active photo is a pane's");
    right_click(&mut h, &format!("film:{}", extra.0));
    assert!(widget(&h, "photomenu:photo.delete").is_some(), "the menu opened");
    assert_eq!(h.app.session.active(), active, "the active photo is still one on screen");
    assert_eq!(crate::panels::compare::compare_pair(&mut h.app), Some((a, b)), "the panes are as they were");
}

/// The menu as drawn lists the presets too, and follows a change to them: a preset starred
/// while the app runs puts a Favorites group at the top of Apply Preset.
#[test]
fn the_drawn_menu_follows_the_presets() {
    let mut h = demo("photoGrid");
    let p = h.app.session.visible_cloned()[0];
    let open = |h: &mut Headless| {
        right_click(h, &format!("thumb:{}", p.0));
        go(h, "ui.hoverWidget", "photomenusub:Edit Settings");
        go(h, "ui.hoverWidget", "photomenusub:Apply Preset");
    };
    open(&mut h);
    assert!(
        h.app.widgets.iter().any(|(w, _)| w.starts_with("photomenusub:")
            && w != "photomenusub:Edit Settings"
            && w != "photomenusub:Apply Preset"
            && h.app.session.presets.iter().any(|x| format!("photomenusub:{}", x.group) == *w)),
        "a preset group's submenu is drawn"
    );
    assert!(widget(&h, "photomenusub:Favorites").is_none());
    let first = h.app.session.presets[0].id.clone();
    h.app.run("preset.favorite", json!({"id": first})).unwrap();
    open(&mut h);
    go(&mut h, "ui.hoverWidget", "photomenusub:Favorites");
    assert!(widget(&h, &format!("photomenu:preset.apply|{}", json!({"id": first}))).is_some(), "the starred preset is under Favorites");
}

/// A preset group of hundreds of presets opens as a scrolling list that only draws the rows in
/// view (an imported collection can put thousands in one group). Scrolled down, a row far down
/// the list is drawn where it belongs, on one line though its name is the longest, and applies
/// its preset.
#[test]
fn a_large_preset_group_draws_only_the_rows_in_view() {
    let mut h = demo("photoGrid");
    let base = h.app.session.presets[0].clone();
    for i in 0..600 {
        let mut p = base.clone();
        p.id = format!("big-{i}");
        p.name = if i == 450 { "Big 450, with a name much longer than any other in this group".into() } else { format!("Big {i}") };
        p.group = "Big".into();
        p.builtin = false;
        h.app.session.presets.push(p);
    }
    let photo = h.app.session.visible_cloned()[0];
    right_click(&mut h, &format!("thumb:{}", photo.0));
    go(&mut h, "ui.hoverWidget", "photomenusub:Edit Settings");
    go(&mut h, "ui.hoverWidget", "photomenusub:Apply Preset");
    go(&mut h, "ui.hoverWidget", "photomenusub:Big");
    let row = |i: usize| format!("photomenu:preset.apply|{}", json!({"id": format!("big-{i}")}));
    let drawn = h.app.widgets.iter().filter(|(w, _)| w.starts_with("photomenu:preset.apply|") && w.contains("big-")).count();
    assert!(drawn > 5 && drawn < 100, "{drawn} of 600 rows drawn");
    let first = widget(&h, &row(0)).expect("the first row is drawn");
    let pitch = widget(&h, &row(1)).expect("the second row is drawn").top() - first.top();
    let level = h
        .app
        .widgets
        .iter()
        .filter(|(w, r)| w.starts_with("menu-level:") && r.contains(first.center()))
        .map(|(_, r)| *r)
        .next()
        .expect("the group's level");
    let r = h.request("ui.move", json!({"x": level.center().x, "y": level.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    // scroll a little at a time, letting each scroll ease out, until rows 450 and 451 are both
    // drawn inside the level
    let settle = |h: &mut Headless| {
        let mut at = widget(h, &row(450));
        for _ in 0..200 {
            h.step();
            let now = widget(h, &row(450));
            if now == at {
                break;
            }
            at = now;
        }
    };
    let mut pair = None;
    for _ in 0..400 {
        let r = h.request("ui.scroll", json!({"dy": -100.0}), T);
        assert_eq!(r["ok"], true, "{r}");
        settle(&mut h);
        if let (Some(far), Some(next)) = (widget(&h, &row(450)), widget(&h, &row(451)))
            && level.contains_rect(next)
        {
            pair = Some((far, next));
            break;
        }
    }
    let (far, next) = pair.expect("rows 450 and 451 come into view");
    assert!(far.left() >= level.left() - 0.5 && far.right() <= level.right() + 0.5, "row 450 {far:?} fits its level's width {level:?}");
    assert!((far.height() - first.height()).abs() < 0.5, "row 450 is one line high: {far:?} against {first:?}");
    assert!((next.top() - far.top() - pitch).abs() < 1.0, "rows keep their pitch: {next:?} after {far:?}, pitch {pitch}");
    go(&mut h, "ui.clickWidget", &row(450));
    let label = h.app.session.undo.last().map(|e| e.label.clone());
    assert_eq!(label.as_deref(), Some("Preset: Big 450, with a name much longer than any other in this group"));
}

/// A preset applied from a photo's menu is marked in the Presets panel, whose Amount slider
/// then adjusts it, as after a click in the panel.
#[test]
fn a_preset_from_the_menu_gets_the_amount_slider() {
    let mut h = demo("detail");
    let r = h.request("engine.execute", json!({"command": "panel.presets"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(widget(&h, "slider:presetAmount").is_none(), "no slider before a preset is applied");
    let preset = h.app.session.presets.iter().find(|p| p.id == "lc.warm-glow").expect("the stock Warm Glow").clone();
    right_click(&mut h, "canvas:image");
    go(&mut h, "ui.hoverWidget", "photomenusub:Edit Settings");
    go(&mut h, "ui.hoverWidget", "photomenusub:Apply Preset");
    go(&mut h, "ui.hoverWidget", &format!("photomenusub:{}", crate::i18n::builtin_label(&preset.group, true)));
    go(&mut h, "ui.clickWidget", &format!("photomenu:preset.apply|{}", json!({"id": preset.id})));
    h.settle(SETTLE);
    h.step();
    assert!(widget(&h, "slider:presetAmount").is_some(), "the Amount slider is offered");
    let mark = widget(&h, "presetApplied").expect("the panel marks the preset");
    assert_eq!(widget(&h, "preset:lc.warm-glow"), Some(mark), "on the preset applied");
}
