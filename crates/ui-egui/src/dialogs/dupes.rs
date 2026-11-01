//! Photo ▸ Find Duplicate Photos…: the review window. It starts the engine's finder (`library.findDuplicates`, with
//! the view and selection left alone), shows the run's progress, then every group with a thumbnail per photo, the
//! finder's keeper ticked and the others unticked with why they lose. The confirm, "Delete N Photos", moves the
//! unticked photos to Recently Deleted through `photo.delete` (one undo step); any tick can be changed first.

use egui::{Color32, Rect, Sense, pos2, vec2};
use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::{Dialog, DuplicateGroup, DuplicatePhoto};
use crate::theme::Tokens;

/// A card's thumbnail box.
const THUMB_W: f32 = 150.0;
const THUMB_H: f32 = 100.0;
/// A whole card (thumbnail, the Keep tick, three lines): cards off screen take this much room and lay out nothing.
const CARD_H: f32 = 190.0;
/// Thumbnail requests from the window come before the grid's off-screen prefetch.
const THUMB_PRIORITY: u32 = 20;

/// `dialog.findDuplicates`: open the window and start the finder.
pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    let mut dlg = Dialog::Duplicates { groups: Vec::new(), done: false, photos: 0, error: None };
    match app.run("library.findDuplicates", json!({"filter": false, "selectExtras": false})) {
        // (nothing to decode and no worker: the result came at once)
        Ok(r) => {
            if let Some(last) = r.get("last").filter(|l| !l.is_null()) {
                take_result(&mut dlg, last);
            }
        }
        Err(e) => {
            if let Dialog::Duplicates { done, error, .. } = &mut dlg {
                *done = true;
                *error = Some(e);
            }
        }
    }
    app.ui.dialog = Some(dlg);
    Ok(Value::Null)
}

/// Fill the window from a finished run's result (`last` of `library.findDuplicates` / `duplicates.pump`).
pub fn take_result(dlg: &mut Dialog, last: &Value) {
    let Dialog::Duplicates { groups, done, photos, error } = dlg else { return };
    // a run stopped early (✕ in the activity stack, a cancel from the control channel) found nothing because it
    // never looked: not the same as "no duplicates"
    if last["cancelled"].as_bool() == Some(true) {
        *error = Some(crate::i18n::tr("Finding duplicate photos was stopped before it finished.").to_string());
    }
    let group_of = |g: &Value| DuplicateGroup {
        exact: g["exact"].as_bool().unwrap_or(false),
        max_distance: g["maxDistance"].as_u64().and_then(|d| u32::try_from(d).ok()).unwrap_or(0),
        photos: g["photos"]
            .as_array()
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| {
                        Some(DuplicatePhoto {
                            id: p["id"].as_u64()?,
                            keep: p["keep"].as_bool().unwrap_or(false),
                            why: p["why"].as_str().unwrap_or_default().to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    *groups = last["groups"].as_array().map(|gs| gs.iter().map(group_of).collect()).unwrap_or_default();
    *photos = last["photos"].as_u64().unwrap_or(0);
    *done = true;
}

/// The photos the confirm deletes: every unticked one.
pub fn delete_ids(groups: &[DuplicateGroup]) -> Vec<u64> {
    groups.iter().flat_map(|g| g.photos.iter().filter(|p| !p.keep).map(|p| p.id)).collect()
}

/// The confirm: move the unticked photos to Recently Deleted (nothing unticked: nothing happens).
pub fn confirm(app: &mut LightcraftApp, groups: &[DuplicateGroup]) -> Result<Value, String> {
    let ids = delete_ids(groups);
    if ids.is_empty() {
        return Ok(Value::Null);
    }
    app.run("photo.delete", json!({"ids": ids}))
}

/// The window's body.
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    ui.set_min_width(720.0);
    if matches!(dlg, Dialog::Duplicates { done: false, .. }) {
        progress(app, ui, t, dlg);
        return;
    }
    let Dialog::Duplicates { groups, photos, error, .. } = dlg else { return };
    if let Some(e) = error {
        ui.label(egui::RichText::new(e.as_str()).color(t.text));
        return;
    }
    if groups.is_empty() {
        ui.label(
            egui::RichText::new(crate::i18n::tr_format!("No duplicate photos found among {photos} photos.", photos = *photos)).color(t.text_label),
        );
        return;
    }
    let delete = delete_ids(groups).len();
    ui.label(
        egui::RichText::new(crate::i18n::tr_format!(
            "{groups} groups among {photos} photos · {delete} to delete",
            groups = groups.len(),
            photos = *photos,
            delete = delete
        ))
        .color(t.text_label),
    );
    ui.label(
        egui::RichText::new(crate::i18n::tr(
            "The best copy of each group is ticked: a raw over a rendered file, lossless over lossy, more pixels, a larger file. Untick a photo to delete it instead, or tick more to keep them too.",
        ))
        .color(t.text_dim),
    );
    ui.add_space(6.0);
    egui::ScrollArea::vertical().max_height(super::list_height(ui, 520.0)).auto_shrink([false, true]).show(ui, |ui| {
        for (n, g) in groups.iter_mut().enumerate() {
            // (how far apart the hashes are says how sure a visual match is: 0–2 bits is the same picture
            // at another size or compression, the top of the range deserves a look)
            let kind = if g.exact {
                crate::i18n::tr("byte-identical").to_string()
            } else {
                crate::i18n::tr_format!("visually identical, {bits} bits apart", bits = g.max_distance)
            };
            ui.label(
                egui::RichText::new(crate::i18n::tr_format!("Group {n} · {count} photos · {kind}", n = n + 1, count = g.photos.len(), kind = kind))
                    .font(t.semibold(12.5))
                    .color(t.text),
            );
            ui.horizontal_wrapped(|ui| {
                for p in &mut g.photos {
                    card(app, ui, t, p);
                }
            });
            ui.add_space(8.0);
        }
    });
}

/// While the finder runs: its progress, from the engine's pump (the frame loop drives the job at full pace while
/// this window is open; see `dupes_scan`).
fn progress(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let v = app.session.execute("duplicates.pump", &json!({"pace": "normal"})).unwrap_or_default();
    if v["active"] != true {
        match v.get("last").filter(|l| !l.is_null()) {
            Some(last) => take_result(dlg, last),
            // no run and no result: the library changed under the window (the switch drops the run and its
            // result), so there is nothing to wait for
            None => {
                if let Dialog::Duplicates { done, error, .. } = dlg {
                    *done = true;
                    *error = Some(crate::i18n::tr("Finding duplicate photos was stopped before it finished.").to_string());
                }
            }
        }
        return;
    }
    ui.label(egui::RichText::new(crate::i18n::tr("Looking for duplicate photos…")).color(t.text_label));
    let (done, total) = (v["running"]["done"].as_u64().unwrap_or(0), v["running"]["total"].as_u64().unwrap_or(0));
    let fraction = if total == 0 { 0.0 } else { done as f32 / total as f32 };
    ui.add(egui::ProgressBar::new(fraction).text(crate::i18n::tr_format!("Hashed {done} of {total} photos", done = done, total = total)));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
}

/// One photo of a group: its thumbnail, the Keep tick, its name, format, size and the finder's reason.
fn card(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, p: &mut DuplicatePhoto) {
    let id = PhotoId(p.id);
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    // a card the scroll position hides takes its room and nothing else: no thumbnail request, no text layout
    // (a library can have thousands of photos in groups)
    let footprint = Rect::from_min_size(ui.cursor().min, vec2(THUMB_W + 8.0, CARD_H));
    if !ui.clip_rect().intersects(footprint) {
        ui.allocate_exact_size(footprint.size(), Sense::hover());
        return;
    }
    crate::panels::grid::request_thumb(app, id, THUMB_W as usize, THUMB_PRIORITY);
    ui.vertical(|ui| {
        ui.set_width(THUMB_W + 8.0);
        ui.set_min_height(CARD_H);
        let (rect, _) = ui.allocate_exact_size(vec2(THUMB_W, THUMB_H), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, t.skeleton);
        match app.renderer.thumb(id) {
            Some(tex) => {
                painter.image(tex.tex.id(), fit(rect, tex.size), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)),
        }
        let r = ui.checkbox(&mut p.keep, crate::i18n::tr("Keep"));
        crate::widgets::register(ui, format!("check:dupeKeep:{}", p.id), r.rect);
        ui.label(egui::RichText::new(photo.file_name.as_str()).font(t.font(12.0)).color(t.text));
        ui.label(
            egui::RichText::new(format!("{} · {}×{} · {}", photo.format, photo.width, photo.height, human_size(photo.file_size)))
                .font(t.font(11.5))
                .color(t.text_dim),
        );
        if !p.why.is_empty() {
            ui.label(egui::RichText::new(p.why.as_str()).font(t.font(11.5)).color(t.text_dim));
        }
    });
}

/// Where a picture of `size` pixels sits inside `rect`, its aspect kept.
fn fit(rect: Rect, size: [usize; 2]) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let s = (rect.width() / w).min(rect.height() / h);
    Rect::from_center_size(rect.center(), vec2(w * s, h * s))
}

/// "8.2 MB", "640 KB", "12 B" (also the grid's Dimensions & File Size caption).
pub(crate) fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::time::Duration;

    use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    use serde_json::json;

    use crate::headless::Headless;
    use crate::state::Dialog;
    use crate::{LightcraftApp, Services};

    const T: Duration = Duration::from_secs(20);
    const SETTLE: Duration = Duration::from_secs(120);

    /// A smooth procedural picture of `seed`, drawn in normalized coordinates so every size shows the same picture.
    fn write_jpeg(path: &Path, w: usize, h: usize, seed: u32, quality: u8) {
        let s = f64::from(seed);
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f64 / w as f64, y as f64 / h as f64);
                let r = 0.5 + 0.5 * (std::f64::consts::TAU * (u * (1.0 + s) + s * 0.3)).sin();
                let g = 0.5 + 0.5 * (std::f64::consts::TAU * (v * (2.0 + s * 0.5))).cos();
                let b = 0.5 + 0.5 * (std::f64::consts::TAU * ((u + v) * 1.5 + s)).sin();
                let block = (u - 0.2 - 0.15 * s).abs() < 0.12 && (v - 0.6 + 0.1 * s).abs() < 0.15;
                let (r, g, b) = if block { (0.95, 0.9, 0.2) } else { (r, g, b) };
                px.extend_from_slice(&[(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]);
            }
        }
        let bytes = encode_jpeg(&EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&px)), quality, ChromaSubsampling::S444, &EncodeMeta::default())
            .expect("jpeg");
        std::fs::write(path, bytes).expect("write");
    }

    /// An app on a library of picture 0 at 300 × 200, the same at 150 × 100 and re-saved at quality 35, and picture 1.
    fn app_with_duplicates(tag: &str) -> (Headless, HashMap<String, u64>) {
        let dir = std::env::temp_dir().join(format!("lc-ui-dupes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        write_jpeg(&dir.join("a.jpg"), 300, 200, 0, 90);
        write_jpeg(&dir.join("a_small.jpg"), 150, 100, 0, 90);
        write_jpeg(&dir.join("a_resaved.jpg"), 300, 200, 0, 35);
        write_jpeg(&dir.join("b.jpg"), 300, 200, 1, 90);
        let mut session = lightcraft_engine::Session::new().with_fs();
        let r = session.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).expect("import");
        assert_eq!(r["imported"].as_array().map(Vec::len), Some(4), "{r}");
        let ids = session.catalog.photos().map(|p| (p.file_name.clone(), p.id.0)).collect();
        let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
        app.ui.settings.gpu = false;
        let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
        h.step();
        (h, ids)
    }

    fn groups(h: &Headless) -> Vec<crate::state::DuplicateGroup> {
        match &h.app.ui.dialog {
            Some(Dialog::Duplicates { groups, done: true, .. }) => groups.clone(),
            d => panic!("{d:?}"),
        }
    }

    fn open_and_wait(h: &mut Headless) {
        h.app.run("dialog.findDuplicates", json!({})).expect("opens");
        assert!(h.step_until(SETTLE, |h| matches!(&h.app.ui.dialog, Some(Dialog::Duplicates { done: true, .. }))), "{:?}", h.app.ui.dialog);
        h.step();
        h.step();
    }

    /// The window lists the group with the finder's keeper ticked, the confirm counts the unticked photos, a tick
    /// changes the count, and the confirm deletes exactly the unticked ones.
    #[test]
    fn the_window_lists_the_groups_and_deletes_the_unticked_copies() {
        let (mut h, ids) = app_with_duplicates("delete");
        open_and_wait(&mut h);
        let gs = groups(&h);
        assert_eq!(gs.len(), 1, "{gs:?}");
        assert_eq!((gs[0].photos[0].id, gs[0].photos[0].keep), (ids["a.jpg"], true));
        assert!(gs[0].photos.iter().skip(1).all(|p| !p.keep && !p.why.is_empty()), "{gs:?}");
        let text = h.painted_text();
        assert!(text.iter().any(|t| t == "Delete 2 Photos"), "{text:?}");
        assert!(text.iter().any(|t| t.starts_with("1 groups among 4 photos")), "{text:?}");
        // keep the small copy too
        let r = h.request("ui.clickWidget", json!({"id": format!("check:dupeKeep:{}", ids["a_small.jpg"])}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        let gs = groups(&h);
        assert!(gs[0].photos.iter().find(|p| p.id == ids["a_small.jpg"]).is_some_and(|p| p.keep));
        assert!(h.painted_text().iter().any(|t| t == "Delete 1 Photo"));
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.ui.dialog.is_none());
        let left: Vec<u64> = h.app.session.catalog.photos().filter(|p| p.in_library()).map(|p| p.id.0).collect();
        assert!(!left.contains(&ids["a_resaved.jpg"]), "{left:?}");
        for name in ["a.jpg", "a_small.jpg", "b.jpg"] {
            assert!(left.contains(&ids[name]), "{name} stays: {left:?}");
        }
        assert_eq!(h.app.session.visible_cloned().len(), 3, "the view was never filtered by the window");
    }

    /// A run that was stopped says so instead of "no duplicates", and offers nothing to delete.
    #[test]
    fn a_stopped_run_is_reported_not_taken_for_no_duplicates() {
        let mut dlg = Dialog::Duplicates { groups: Vec::new(), done: false, photos: 0, error: None };
        super::take_result(&mut dlg, &json!({"groups": [], "photos": 12, "cancelled": true}));
        match &dlg {
            Dialog::Duplicates { done: true, error: Some(e), groups, .. } => {
                assert!(e.contains("stopped"), "{e}");
                assert!(groups.is_empty());
            }
            d => panic!("{d:?}"),
        }
        let mut dlg = Dialog::Duplicates { groups: Vec::new(), done: false, photos: 0, error: None };
        super::take_result(&mut dlg, &json!({"groups": [], "photos": 12, "cancelled": false}));
        assert!(matches!(&dlg, Dialog::Duplicates { done: true, error: None, .. }), "{dlg:?}");
    }

    /// A window left waiting with no run and no result (the library was switched under it, which drops both)
    /// ends as a stopped run instead of showing progress forever.
    #[test]
    fn a_window_whose_run_went_with_the_library_stops_waiting() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
        h.step();
        h.app.ui.dialog = Some(Dialog::Duplicates { groups: Vec::new(), done: false, photos: 0, error: None });
        h.step();
        h.step();
        match &h.app.ui.dialog {
            Some(Dialog::Duplicates { done: true, error: Some(e), .. }) => assert!(e.contains("stopped"), "{e}"),
            d => panic!("{d:?}"),
        }
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.ui.dialog.is_none());
    }

    /// Without duplicates the window says so and its confirm deletes nothing.
    #[test]
    fn without_duplicates_the_window_says_so() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
        h.step();
        let before = h.app.session.catalog.len();
        open_and_wait(&mut h);
        assert!(groups(&h).is_empty());
        let text = h.painted_text();
        assert!(text.iter().any(|t| t.starts_with("No duplicate photos found")), "{text:?}");
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.ui.dialog.is_none());
        assert_eq!(h.app.session.catalog.photos().filter(|p| p.in_library()).count(), before);
    }
}
