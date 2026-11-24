//! The Export screen's preview of the first photo to be exported ([`crate::export_preview`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lcc_catalog::{Op, Photo, PhotoId, Source};
use lcc_engine::export::{ExportFormat, ExportOptions, Resize};
use serde_json::{Value, json};

use crate::export_preview::{Hdr, Info, NOTE, WIDGET};
use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(10);

fn demo(size: [f32; 2]) -> Headless {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    Headless::new(app, size, 1.0)
}

fn open_export(h: &mut Headless) {
    h.app.ui.dialog = None;
    h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), T);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Export { .. })), "the Export screen is open");
}

fn set_opts(h: &mut Headless, f: impl FnOnce(&mut ExportOptions, &mut bool, &mut Resize)) {
    if let Some(crate::state::Dialog::Export { opts, full_size, resize, .. }) = &mut h.app.ui.dialog {
        f(opts, full_size, resize);
    }
}

fn widget(h: &mut Headless, id: &str) -> Option<Value> {
    h.request("ui.widgets", json!({"filter": id}), T)["result"].as_array().and_then(|a| a.first().cloned())
}

fn info(h: &Headless) -> Option<Info> {
    crate::export_preview::info(&h.view.ctx)
}

/// Steps until `done` (the preview renders on its own threads).
fn wait(h: &mut Headless, done: impl Fn(&Headless) -> bool) -> bool {
    let t0 = Instant::now();
    while !done(h) && t0.elapsed() < Duration::from_secs(180) {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    done(h)
}

/// Steps until the picture asked for is on screen; a few frames more, so nothing is mid-change.
fn settled(h: &mut Headless) -> Info {
    // (a frame with the settings as they are now: the last frame's news is the one before)
    h.step();
    assert!(wait(h, |h| info(h).is_some_and(|i| !i.running && i.shown.is_some_and(|s| s.current))), "the preview settles: {:?}", info(h));
    for _ in 0..4 {
        h.step();
    }
    info(h).expect("the preview is on screen")
}

fn selected(h: &Headless) -> Vec<PhotoId> {
    h.app.session.selected()
}

/// The preview shows the first photo to be exported as its file: rendered by the export's own
/// path at the export's size, the selected photo, and the picture changes with the settings that
/// change the file (size, watermark) but not with those that don't (where it goes, its name).
#[test]
fn export_screen_previews_the_first_photo_as_its_file() {
    let mut h = demo([1600.0, 1000.0]);
    h.settle(Duration::from_secs(120));
    open_export(&mut h);
    let first = selected(&h).into_iter().next().expect("a photo is selected");
    let i = settled(&mut h);
    assert_eq!(i.shown.map(|s| s.photo), Some(first), "{i:?}");
    assert!(matches!(i.hdr, Hdr::Not(_)) && i.line.is_none(), "SDR mode on an SDR file has nothing to explain: {i:?}");
    let [w, hh] = i.shown.map(|s| s.of).expect("a picture");
    // (the Export screen opens on its saved size: here the dialog's own)
    let (opts, full_size, resize) = match &h.app.ui.dialog {
        Some(crate::state::Dialog::Export { opts, full_size, resize, .. }) => (opts.clone(), *full_size, *resize),
        _ => panic!("the Export screen is open"),
    };
    let asked = ExportOptions { resize: (!full_size).then_some(resize), ..opts };
    let photo = h.app.session.catalog.photo(first).expect("the photo");
    let (fw, fh) = lcc_engine::export::output_size(photo, &asked);
    assert_eq!([w, hh], [fw, fh], "rendered at the export's size");
    assert!(w.max(hh) > 512, "the export's render, not a thumbnail");
    // the picture and its choice are on screen, addressable
    let r = widget(&mut h, WIDGET).expect("the picture is registered");
    assert!(r["rect"][2].as_f64().unwrap_or(0.0) > 100.0 && r["rect"][3].as_f64().unwrap_or(0.0) > 100.0, "{r}");
    let x = r["rect"][0].as_f64().unwrap_or(0.0);
    let settings = widget(&mut h, "combo:exportDestinationMode").expect("the settings");
    assert!(x > settings["rect"][0].as_f64().unwrap_or(0.0) + 200.0, "wide enough, the preview sits beside the settings");
    assert!(widget(&mut h, "button:exportPreviewMode-0").is_some() && widget(&mut h, "button:exportPreviewMode-1").is_some());
    assert!(widget(&mut h, "button:exportPreviewMode-0").is_some(), "the choice is its two buttons");

    // an odd size: the file's size to the pixel
    set_opts(&mut h, |_, full_size, resize| {
        *full_size = false;
        *resize = Resize { mode: lcc_engine::export::ResizeMode::LongEdge, value: 1001.0, ..Resize::default() };
    });
    let i = settled(&mut h);
    let s = i.shown.expect("a picture");
    assert_eq!(s.of[0].max(s.of[1]), 1001, "{s:?}");
    assert!(s.at[0] <= s.of[0] && s.at[1] <= s.of[1], "fitted to its place: {s:?}");
    let renders = i.renders;

    // where the files go and how they are named don't change the picture
    set_opts(&mut h, |o, _, _| {
        o.destination_mode = lcc_engine::export::DestinationMode::SameAsOriginal;
        o.naming = "holiday_{seq}".into();
        o.metadata = lcc_engine::export::MetadataPolicy::None;
        o.remove_location = true;
    });
    for _ in 0..8 {
        h.step();
    }
    assert_eq!(info(&h).map(|i| i.renders), Some(renders), "no render for the destination, naming or metadata");

    // the watermark does
    set_opts(&mut h, |o, _, _| o.watermark = Some(lcc_engine::export::Watermark { text: "© Example".into(), ..Default::default() }));
    let i = settled(&mut h);
    assert!(i.renders > renders, "the watermark re-renders the preview");
    let renders = i.renders;
    // so does the size
    set_opts(&mut h, |_, _, resize| resize.value = 777.0);
    let i = settled(&mut h);
    assert!(i.renders > renders && i.shown.is_some_and(|s| s.of[0].max(s.of[1]) == 777), "{i:?}");

    // the selected photo
    let other = h.app.session.visible_cloned().into_iter().find(|id| *id != first).expect("another photo");
    h.app.session.selection = lcc_engine::Selection::single(other);
    let i = settled(&mut h);
    assert_eq!(i.shown.map(|s| s.photo), Some(other));
    // the activity stack had a row for the render
    assert_eq!(info(&h).map(|i| i.running), Some(false));
}

/// In a narrow window (no room beside the settings) the preview is the settings page's last
/// section, with the settings where they were; wide enough it sits beside them.
#[test]
fn export_screen_previews_after_the_settings_when_narrow() {
    let mut h = demo([700.0, 800.0]);
    h.settle(Duration::from_secs(120));
    // (the settings start where they did: Export Location is on the first screen)
    let loc_before = {
        let mut plain = demo([700.0, 800.0]);
        plain.settle(Duration::from_secs(120));
        open_export(&mut plain);
        plain.settle(Duration::from_secs(120));
        widget(&mut plain, "section:exportLocation").expect("Export Location")["rect"][1].as_f64().unwrap_or(0.0)
    };
    open_export(&mut h);
    let _ = settled(&mut h);
    // (the page scrolls to the preview only when asked: its section is the rail's to jump to)
    let loc = widget(&mut h, "section:exportLocation").expect("Export Location");
    assert!((loc["rect"][1].as_f64().unwrap_or(0.0) - loc_before).abs() < 2.0, "the settings are where they were: {loc}");
    assert!(widget(&mut h, "export:group:Preview").is_some(), "the rail lists it");
    h.request("ui.clickWidget", json!({"id": "export:group:Preview"}), T);
    h.settle(Duration::from_secs(120));
    let pic = widget(&mut h, WIDGET).expect("the picture");
    let y = pic["rect"][1].as_f64().unwrap_or(0.0);
    let hgt = pic["rect"][3].as_f64().unwrap_or(0.0);
    assert!(y > loc["rect"][1].as_f64().unwrap_or(0.0) && hgt > 40.0, "the preview follows the settings: {pic} {loc}");
    // wide enough, the settings make room for it beside them
    let mut wide = demo([1400.0, 900.0]);
    wide.settle(Duration::from_secs(120));
    open_export(&mut wide);
    settled(&mut wide);
    let pic = widget(&mut wide, WIDGET).expect("the picture");
    let loc = widget(&mut wide, "section:exportLocation").expect("Export Location");
    assert!(pic["rect"][0].as_f64().unwrap_or(0.0) > loc["rect"][0].as_f64().unwrap_or(0.0) + 300.0, "beside the settings: {pic} {loc}");
}

/// HDR mode without an HDR display doesn't pretend: the picture stays the SDR rendition, a line
/// says why HDR isn't shown and what to turn on, and nothing is uploaded. The command toggles it.
#[test]
fn export_preview_hdr_mode_is_honest_without_an_hdr_display() {
    let mut h = demo([1600.0, 1000.0]);
    h.settle(Duration::from_secs(120));
    h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true}}), T);
    open_export(&mut h);
    set_opts(&mut h, |o, _, _| o.hdr = true);
    settled(&mut h);
    assert!(!h.app.ui.export_preview_hdr, "SDR to begin with");
    h.request("ui.clickWidget", json!({"id": "button:exportPreviewMode-1"}), T);
    let i = settled(&mut h);
    assert!(h.app.ui.export_preview_hdr);
    let line = i.line.expect("a line says why");
    assert!(line.contains("isn't showing HDR"), "{line}");
    assert!(h.painted_text().iter().any(|t| t.contains("isn't showing HDR")), "the line is on screen: {:?}", h.painted_text());
    assert!(widget(&mut h, NOTE).is_some());
    assert!(i.shown.is_some_and(|s| !s.hdr), "the SDR rendition");

    // the command toggles it back, and reports
    let r = h.request("engine.execute", json!({"command": "export.previewMode", "params": {}}), T);
    assert_eq!(r["result"]["hdr"], json!(false));
    assert_eq!(r["result"]["hdrDisplay"], json!(false));
    assert!(!h.app.ui.export_preview_hdr);
    let i = settled(&mut h);
    assert!(i.line.is_none(), "{i:?}");

    // a file that carries no HDR: the line says which setting or format
    let why = |h: &mut Headless| {
        h.request("engine.execute", json!({"command": "export.previewMode", "params": {"hdr": true}}), T);
        for _ in 0..4 {
            h.step();
        }
        info(h).and_then(|i| i.line)
    };
    set_opts(&mut h, |o, _, _| o.hdr = false);
    assert_eq!(why(&mut h).as_deref(), Some("HDR output is off: turn it on under File Settings to write HDR files."));
    set_opts(&mut h, |o, _, _| {
        o.hdr = true;
        o.format = ExportFormat::Tiff;
    });
    assert_eq!(why(&mut h).as_deref(), Some("This format can't carry HDR: JPEG, AVIF, PNG, JPEG XL or 32-bit TIFF can."));
    set_opts(&mut h, |o, _, _| o.format = ExportFormat::Jpeg);
    h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": false}}), T);
    let _ = settled(&mut h);
    h.request("engine.execute", json!({"command": "export.previewMode", "params": {"hdr": true}}), T);
    assert_eq!(why(&mut h).as_deref(), Some("This photo has no HDR edit."));

    // an HDR display that is switched off, or HDR Preview off: named
    h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true}}), T);
    h.app.ui.settings.hdr_display = false;
    assert_eq!(why(&mut h).as_deref(), Some("HDR display is off: turn it on under Settings ▸ Display (it applies at the next launch)."));
    h.app.ui.settings.hdr_display = true;
    h.app.ui.hdr_preview = false;
    // (the display's headroom is read as the frame begins: a frame with the output in place)
    h.app.services.hdr_output = Some(Box::new(Display(None)));
    h.step();
    assert!(h.painted_text().iter().any(|t| t.contains("isn't showing HDR")), "an SDR display: {:?}", h.painted_text());
    h.app.services.hdr_output = Some(Box::new(Display(Some(4.0))));
    h.step();
    assert_eq!(why(&mut h).as_deref(), Some("HDR Preview is off: turn it on under View ▸ Loupe Info ▸ HDR Preview."));
}

/// An HDR output whose display shows HDR now (`Some`) or not (`None`); nothing may be uploaded.
struct Display(Option<f32>);
impl crate::HdrOutput for Display {
    fn headroom(&mut self) -> Option<f32> {
        self.0
    }
    fn upload(&mut self, _: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
        panic!("nothing is uploaded without HDR")
    }
}

/// On an HDR display, HDR mode shows an HDR export's HDR rendition through the HDR output (a
/// texture of the preview's own, with highlights above SDR white); SDR mode and a closed
/// Export screen upload nothing and free the texture.
#[test]
fn export_preview_shows_hdr_on_an_hdr_display() {
    #[derive(Default)]
    struct Log {
        uploads: Vec<(u8, usize, usize, f32)>,
        released: Vec<u8>,
    }
    struct Display(Rc<RefCell<Log>>);
    impl crate::HdrOutput for Display {
        fn headroom(&mut self) -> Option<f32> {
            Some(4.0)
        }
        fn upload(&mut self, slot: u8, w: usize, h: usize, rgb: &[f32]) -> Result<egui::TextureId, String> {
            assert_eq!(rgb.len(), w * h * 3);
            let peak = rgb.iter().copied().filter(|v| v.is_finite()).fold(0.0, f32::max);
            self.0.borrow_mut().uploads.push((slot, w, h, peak));
            Ok(egui::TextureId::User(700 + u64::from(slot)))
        }
        fn release(&mut self, slot: u8) {
            self.0.borrow_mut().released.push(slot);
        }
    }
    let log = Rc::new(RefCell::new(Log::default()));
    let services = Services { png: None, hdr_output: Some(Box::new(Display(Rc::clone(&log)))), ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.settle(Duration::from_secs(120));
    h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true}}), T);
    h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"light.exposure": 2.0}}}), T);
    open_export(&mut h);
    set_opts(&mut h, |o, _, _| o.hdr = true);
    let slot = crate::export_preview::HDR_SLOT;
    let first = selected(&h).into_iter().next().expect("a photo");
    let _ = first;

    // SDR: the gain map's base image, nothing uploaded
    let i = settled(&mut h);
    assert!(i.hdr == Hdr::Shown && i.shown.is_some_and(|s| !s.hdr), "SDR mode shows SDR: {i:?}");
    assert!(log.borrow().uploads.is_empty(), "{:?}", log.borrow().uploads);

    // HDR: the HDR rendition, above SDR white
    h.request("ui.clickWidget", json!({"id": "button:exportPreviewMode-1"}), T);
    let i = settled(&mut h);
    let s = i.shown.expect("a picture");
    assert!(s.hdr && i.line.is_none(), "{i:?}");
    let ups = log.borrow().uploads.clone();
    let (sl, w, hh, peak) = *ups.last().expect("the HDR rendition was uploaded");
    assert_eq!(sl, slot);
    assert!(peak > 1.0, "highlights above SDR white: {ups:?}");
    assert_eq!([w, hh], s.at, "uploaded at the size it is shown");
    assert!(h.app.export_preview_hdr.is_some());

    // back to SDR: the texture is freed, the picture is SDR again
    h.request("ui.clickWidget", json!({"id": "button:exportPreviewMode-0"}), T);
    let i = settled(&mut h);
    assert!(i.shown.is_some_and(|s| !s.hdr) && h.app.export_preview_hdr.is_none(), "{i:?}");
    assert_eq!(log.borrow().released.last(), Some(&slot));

    // HDR again, then the screen closes: its texture goes with it
    h.request("engine.execute", json!({"command": "export.previewMode", "params": {"hdr": true}}), T);
    let i = settled(&mut h);
    assert!(i.shown.is_some_and(|s| s.hdr), "{i:?}");
    h.request("ui.dialog.cancel", json!({}), T);
    for _ in 0..3 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none() && h.app.export_preview_hdr.is_none());
    assert!(info(&h).is_none(), "the preview is forgotten");
    assert!(log.borrow().released.iter().filter(|s| **s == slot).count() >= 2);
}

/// A photo that cannot be read, or a size of nothing, is said in the preview: never a panic, and
/// the Export screen stays up.
#[test]
fn export_preview_says_what_is_wrong() {
    let mut session = lcc_engine::Session::new();
    session.media.file_loader = Some(Arc::new(|path, _, _| Err(format!("{}: not an image", path))));
    let photo = Photo::new(PhotoId(1), Source::File { path: "broken.jpg".into() }, "broken.jpg", "JPEG", 3000, 2000, "2026-10-22");
    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).expect("the photo is added");
    session.selection = lcc_engine::Selection::single(PhotoId(1));
    let mut app = LccApp::new(session, Services { png: None, ..Default::default() });
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    open_export(&mut h);
    // the loader reports a file it can't read (the error is the preview's line)
    let t0 = Instant::now();
    let line = loop {
        h.step();
        if let Some(i) = info(&h)
            && !i.running
        {
            break i.line;
        }
        assert!(t0.elapsed() < Duration::from_secs(120), "no answer");
        std::thread::sleep(Duration::from_millis(2));
    };
    // (a photo the loader cannot read may fail at the source's probe or at the decode: either way it is said)
    assert!(line.is_some_and(|l| !l.is_empty()), "the preview says what is wrong");
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Export { .. })), "the Export screen stays up");
    assert!(widget(&mut h, NOTE).is_some(), "the line is on screen");

    // a size of nothing makes a picture of one pixel, not an error
    set_opts(&mut h, |_, full_size, resize| {
        *full_size = false;
        *resize = Resize { mode: lcc_engine::export::ResizeMode::LongEdge, value: 0.0, ..Resize::default() };
    });
    for _ in 0..30 {
        h.step();
    }
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Export { .. })));
}

/// The Export button answers at once while the preview renders; with the screen closed the
/// preview stops.
#[test]
fn export_button_works_while_the_preview_renders() {
    let picks = Rc::new(RefCell::new(0usize));
    let n = Rc::clone(&picks);
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.services.pick_folder = Some(Box::new(move || {
        *n.borrow_mut() += 1;
        None
    }));
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.settle(Duration::from_secs(120));
    open_export(&mut h);
    // (a full-size render of a demo photo takes a moment: long enough to press Export meanwhile)
    set_opts(&mut h, |_, full_size, _| *full_size = true);
    // (the settings rest a moment, then its row is there while it renders)
    let row = |h: &Headless| h.app.session.activity.list().into_iter().find(|t| t.kind == crate::export_preview::KIND);
    assert!(wait(&mut h, |h| row(h).is_some()), "the preview is rendering: {:?}", info(&h));
    assert!(info(&h).is_some_and(|i| i.running), "on its way, not a picture of the frame before: {:?}", info(&h));
    let row = row(&h).expect("the row");
    assert!(!row.cancellable, "the screen's closing stops it: it has no ✕ of its own");
    assert!(widget(&mut h, "button:dialogOk").is_some(), "Export is on screen");
    h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(*picks.borrow(), 1, "Export was pressed at once: it asked where to put the files");
    // the screen is gone (no folder was chosen), and with it the preview and its row
    // (Export without a folder leaves the screen up: Back closes it)
    if h.app.ui.dialog.is_some() {
        h.request("ui.dialog.cancel", json!({}), T);
    }
    for _ in 0..3 {
        h.step();
    }
    assert!(info(&h).is_none(), "the preview is forgotten");
    assert!(wait(&mut h, |h| h.app.session.activity.list().iter().all(|t| t.kind != crate::export_preview::KIND)), "its render stopped");
}

/// The mean colour of each cell of a `grid × grid` tiling of `img` (0..255).
fn tile_means(img: &lcc_raster::Rgba8, grid: usize) -> Vec<[f32; 3]> {
    let (grid, (w, h)) = (grid.max(1), (img.width.max(1), img.height.max(1)));
    let mut sums = vec![[0f64; 4]; grid * grid];
    for y in 0..h {
        for x in 0..w {
            let cell = (y * grid / h).saturating_mul(grid).saturating_add(x * grid / w).min(sums.len().saturating_sub(1));
            if let Some(p) = img.data.get(y.saturating_mul(w).saturating_add(x))
                && let Some(s) = sums.get_mut(cell)
            {
                s[3] += 1.0;
                for c in 0..3 {
                    s[c] += f64::from(p[c]);
                }
            }
        }
    }
    sums.iter().map(|s| [0, 1, 2].map(|c| (s[c] / s[3].max(1.0)) as f32)).collect()
}

/// A picture larger than its place is shrunk for the screen without its pixels changing places:
/// each part of the picture shows what that part of the export shows (a transposed or scrambled
/// pass showed streaks where the photo was).
#[test]
fn export_preview_shrinks_the_export_faithfully() {
    let mut h = demo([1600.0, 1000.0]);
    h.settle(Duration::from_secs(120));
    open_export(&mut h);
    set_opts(&mut h, |_, full_size, resize| {
        *full_size = false;
        *resize = Resize { mode: lcc_engine::export::ResizeMode::LongEdge, value: 700.0, ..Resize::default() };
    });
    let i = settled(&mut h);
    let s = i.shown.expect("a picture");
    assert_eq!(s.of[0].max(s.of[1]), 700, "{s:?}");
    assert!(s.at[0] < s.of[0] && s.at[1] < s.of[1], "shrunk to its place: {s:?}");
    let shown =
        h.view.ctx.data(|d| d.get_temp::<std::sync::Arc<crate::export_preview::Shown>>(egui::Id::new("export-preview-shown"))).expect("shown");
    // the export itself, shrunk by plain averaging for comparison
    let (opts, full_size, resize) = match &h.app.ui.dialog {
        Some(crate::state::Dialog::Export { opts, full_size, resize, .. }) => (opts.clone(), *full_size, *resize),
        _ => panic!("the Export screen is open"),
    };
    let o = ExportOptions { resize: (!full_size).then_some(resize), ..opts };
    let id = s.photo;
    let mut small = lcc_raster::Rgba8::new(s.at[0], s.at[1]);
    let full = lcc_engine::export::prepare_export(&mut h.app.session, id, &o, 1)
        .expect("the export is set up")
        .preview_until(&|| false)
        .expect("rendered")
        .to_display()
        .0;
    assert_eq!((full.width, full.height), (s.of[0], s.of[1]));
    for y in 0..small.height {
        for x in 0..small.width {
            let (x0, y0) = (x * full.width / small.width.max(1), y * full.height / small.height.max(1));
            let (x1, y1) = ((x + 1) * full.width / small.width.max(1), (y + 1) * full.height / small.height.max(1));
            let mut sum = [0f64; 3];
            let mut n = 0u64;
            for yy in y0..y1.max(y0 + 1) {
                for xx in x0..x1.max(x0 + 1) {
                    if let Some(p) = full.data.get(yy.min(full.height - 1).saturating_mul(full.width).saturating_add(xx.min(full.width - 1))) {
                        for c in 0..3 {
                            sum[c] += f64::from(p[c]);
                        }
                        n += 1;
                    }
                }
            }
            if let Some(p) = small.data.get_mut(y.saturating_mul(small.width).saturating_add(x)) {
                let v = [0, 1, 2].map(|c| (sum[c] / n.max(1) as f64).round() as u8);
                *p = [v[0], v[1], v[2], 255];
            }
        }
    }
    let (a, b) = (tile_means(&shown.fitted.sdr, 4), tile_means(&small, 4));
    for (k, (a, b)) in a.iter().zip(&b).enumerate() {
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() < 6.0, "cell {k}, channel {c}: the preview {a:?} against the export {b:?}");
        }
    }
}
