use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_develop::{BrushStroke, DevelopSettings, Mask, MaskComponent, MaskShape};
use lightcraft_engine::Session;
use lightcraft_engine::media::{RenderJob, RenderResult};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    headless::Headless,
    panels::grid::request_thumb,
    render::{RenderOffload, Slot, Tex},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

/// Counts allocations made by the current thread (the UI thread is the one measured).
struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

fn count(size: usize) {
    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
    let _ = BYTES.try_with(|n| n.set(n.get() + size as u64));
}

// SAFETY: every call is forwarded unchanged to the system allocator; the counters are plain
// thread-locals that never allocate. Dev-only example.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count(l.size());
        // SAFETY: same contract as ours.
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count(l.size());
        // SAFETY: same contract as ours.
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        count(new);
        // SAFETY: same contract as ours.
        unsafe { System.realloc(p, l, new) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: same contract as ours.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn allocs() -> (u64, u64) {
    (ALLOCS.with(Cell::get), BYTES.with(Cell::get))
}

struct Blocked;
impl RenderOffload for Blocked {
    fn try_start(&mut self, _: Slot, job: RenderJob) -> Option<RenderJob> {
        Some(job)
    }
    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
        vec![]
    }
}
fn settings(points: usize) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    if points > 0 {
        let stroke = BrushStroke {
            points: (0..points).map(|i| lightcraft_geom::Point::new((i % 100) as f64 / 100.0, (i / 100) as f64 / 100.0)).collect(),
            ..Default::default()
        };
        s.masks.push(Mask {
            components: vec![MaskComponent { name: None, op: Default::default(), invert: false, shape: MaskShape::Brush { strokes: vec![stroke] } }],
            ..Default::default()
        });
    }
    s
}
fn summary(name: &str, mut times: Vec<f64>) {
    times.sort_by(f64::total_cmp);
    println!("{name},median_ms={:.6},p95_ms={:.6}", times[times.len() / 2], times[times.len() * 95 / 100]);
}
fn frames() -> usize {
    std::env::var("FRAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(300).max(1)
}

fn micro(points: usize, pending: bool) {
    let frames = frames();
    let n = 200;
    let mut session = Session::new();
    for i in 1..=n {
        let mut p =
            Photo::new(PhotoId(i), Source::File { path: format!("N:/audit-only/IMG_{i}.jpg") }, "sample.jpg", "JPEG", 6000, 4000, "2026-01-01");
        p.develop = Arc::new(settings(points));
        p.content_hash = Some(format!("{i:032x}"));
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut app = LightcraftApp::new(session, Services::default());
    app.renderer.set_offload(Box::new(Blocked));
    let ctx = egui::Context::default();
    let texture = ctx.load_texture("audit", egui::ColorImage::new([1, 1], vec![egui::Color32::GRAY]), Default::default());
    for i in 1..=n {
        let id = PhotoId(i);
        if !pending {
            let key = app.session.thumb_job(id, 384).unwrap().key;
            app.renderer.textures.insert(
                Slot::Thumb(id),
                Tex { key, photo: id, tex: texture.clone(), size: [1, 1], histogram: None, ms: 0.0, quick: None, pixels: None, hdr: None },
            );
        }
        request_thumb(&mut app, id, 384, 10);
    }
    let mut times = Vec::with_capacity(frames);
    let (a0, b0) = allocs();
    for _ in 0..frames {
        let t = Instant::now();
        for i in 1..=n {
            request_thumb(black_box(&mut app), PhotoId(i), 384, 10);
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let (a1, b1) = allocs();
    println!("micro,points={points},pending={pending},allocs_per_frame={},bytes_per_frame={}", (a1 - a0) / frames as u64, (b1 - b0) / frames as u64);
    summary(&format!("micro,points={points},pending={pending},cells={n}"), times);
    // The draw fast path must notice a develop edit and a size-bucket change.
    let mut edit = settings(points);
    edit.light.exposure = 1.0;
    app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "audit edit".into(), edited: None }).unwrap();
    request_thumb(&mut app, PhotoId(1), 384, 10);
    assert_eq!(app.renderer.wanted(Slot::Thumb(PhotoId(1))), Some(app.session.thumb_job(PhotoId(1), 384).unwrap().key));
    request_thumb(&mut app, PhotoId(1), 512, 10);
    assert_eq!(app.renderer.wanted(Slot::Thumb(PhotoId(1))), Some(app.session.thumb_job(PhotoId(1), 512).unwrap().key));
}
fn collect(root: &std::path::Path, ext: &str, paths: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect(&p, ext, paths);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case(ext)) {
            paths.push(p);
        }
    }
}
fn verify_texture(h: &Headless, id: PhotoId) {
    let p = h.app.session.catalog.photo(id).unwrap();
    let tex = h.app.renderer.thumb(id).unwrap();
    // Justified rows can choose different buckets even with the same slider value.
    // Verify the actual displayed resolution independently of candidate request/hash helpers.
    let edge = tex.size[0].max(tex.size[1]);
    assert!([128, 256, 384, 512].contains(&edge), "thumbnail must use a supported size bucket");
    let Source::File { path } = &p.source else { unreachable!() };
    let (source, info) = h.app.session.media.file_loader.as_ref().unwrap()(path, 512).unwrap();
    println!("decode,path={path},raw={},preview_only={:?}", info.raw, p.preview_only);
    if std::env::var_os("REQUIRE_RAW").is_some() {
        assert!(p.preview_only.is_none(), "embedded-preview fallback does not validate RAW decoding");
        assert!(info.raw, "fresh loader must return decoded RAW data");
    }
    let req = lightcraft_engine::pipeline::RenderRequest { apply_crop: true, ..lightcraft_engine::pipeline::RenderRequest::fit(edge, edge) };
    // Fresh direct CPU render: no candidate job/hash/source/preview cache helpers.
    let expected = lightcraft_engine::pipeline::render(&source, &info, &p.develop, &req).image;
    assert_eq!(tex.size, [expected.width, expected.height]);
    assert_eq!(tex.pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(), expected.as_bytes());
}

fn real() {
    let frames = frames();
    let mut paths = vec![];
    let ext = std::env::var("PHOTO_EXT").unwrap_or_else(|_| "jpg".into());
    let Ok(root) = std::env::var("PHOTO_ROOT") else {
        eprintln!("REAL_PHOTOS needs PHOTO_ROOT, a folder of photos to sample");
        return;
    };
    collect(std::path::Path::new(&root), &ext, &mut paths);
    paths.sort();
    let offset = std::env::var("PHOTO_OFFSET").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
    let limit = std::env::var("PHOTO_LIMIT").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(4).clamp(1, 4);
    paths = paths.into_iter().skip(offset).take(limit).collect();
    assert!(!paths.is_empty(), "selected photo batch is empty");
    println!("batch,extension={ext},offset={offset},limit={limit},photos={}", paths.len());
    let mut session = Session::new().with_fs();
    for (i, path) in paths.iter().enumerate() {
        let path = path.to_string_lossy().into_owned();
        let probe = session.media.file_probe.as_ref().unwrap()(&path).unwrap();
        let mut p =
            Photo::new(PhotoId(i as u64 + 1), Source::File { path: path.clone() }, "photo", &probe.format, probe.width, probe.height, "2026-01-01");
        p.file_size = probe.file_size;
        p.kind = probe.kind;
        p.meta = probe.meta;
        p.as_shot_wb = probe.as_shot_wb;
        p.embedded_lens = probe.embedded_lens;
        p.content_hash = probe.content_hash;
        p.preview_only = probe.preview_only;
        lightcraft_engine::import::apply_import_defaults(&session, &mut p);
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1400.0, 900.0], 1.0);
    h.app.renderer = lightcraft_ui_egui::render::Renderer::with_worker_threads(1);
    h.app.renderer.keep_pixels = true;
    h.app.ui.view = lightcraft_ui_egui::state::ViewMode::PhotoGrid;
    h.app.ui.thumb_size = if ext == "rw2" { 160.0 } else { 100.0 };
    let mut loading_frame = vec![];
    let mut loading_grid = vec![];
    let mut busy = 0;
    for _ in 0..120 {
        let t = Instant::now();
        h.step();
        loading_frame.push(t.elapsed().as_secs_f64() * 1000.0);
        loading_grid.push(h.app.caches.grid_stats.last_show.as_secs_f64() * 1000.0);
        busy += usize::from(h.app.renderer.in_flight() > 0);
    }
    summary("loading,frame", loading_frame);
    summary("loading,grid", loading_grid);
    println!("loading,frames_with_pending={busy}");
    assert!(h.settle(Duration::from_secs(120)), "real library did not settle");
    for i in 1..=paths.len() {
        request_thumb(&mut h.app, PhotoId(i as u64), 256, 20);
    }
    assert!(h.settle(Duration::from_secs(120)));
    println!("real,photos={},textures={},in_flight={}", paths.len(), h.app.renderer.thumb_textures(), h.app.renderer.in_flight());
    let stats = h.app.caches.grid_stats.clone();
    let mut frame = Vec::with_capacity(frames);
    let mut grid = Vec::with_capacity(frames);
    let (a0, b0) = allocs();
    for _ in 0..frames {
        let t = Instant::now();
        h.step();
        frame.push(t.elapsed().as_secs_f64() * 1000.0);
        grid.push(h.app.caches.grid_stats.last_show.as_secs_f64() * 1000.0);
    }
    let (a1, b1) = allocs();
    println!("real,allocs_per_frame={},bytes_per_frame={}", (a1 - a0) / frames as u64, (b1 - b0) / frames as u64);
    summary("real,frame", frame);
    summary("real,grid", grid);
    println!("real,cells_per_frame={}", (h.app.caches.grid_stats.cells_visited - stats.cells_visited) / frames as u64);
    assert!(h.settle(Duration::from_secs(120)), "grid's displayed thumbnail bucket must settle before verification");
    let mut checks: Vec<_> = h
        .app
        .renderer
        .textures
        .iter()
        .filter_map(|(slot, t)| {
            if let Slot::Thumb(id) = slot {
                let mut hash = 0xcbf29ce484222325u64;
                for color in &t.pixels.as_ref().unwrap().pixels {
                    for b in color.to_array() {
                        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
                    }
                }
                Some((id.0, t.key, t.size, hash))
            } else {
                None
            }
        })
        .collect();
    checks.sort_by_key(|x| x.0);
    let mut hash = 0xcbf29ce484222325u64;
    for b in format!("{checks:?}").bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    println!("real,verified_textures={},keys_sizes_pixels_checksum={hash:016x}", checks.len());
    assert_eq!(checks.len(), paths.len(), "every original must render successfully");
    if std::env::var_os("VERIFY_PIXELS").is_some() {
        for i in 1..=paths.len() {
            verify_texture(&h, PhotoId(i as u64));
        }
        println!("oracle,exact_fresh_pixel_matches={}", paths.len());
    }
    let mut scroll_frame = vec![];
    let mut scroll_grid = vec![];
    let start = h.app.grid_scroll;
    let mut max_scroll = 0.0f32;
    for i in 0..frames {
        h.app.synthetic.push(egui::Event::PointerMoved(egui::pos2(800.0, 400.0)));
        h.app.synthetic.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, if (i / 45) % 2 == 0 { -12.0 } else { 12.0 }),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
        });
        let t = Instant::now();
        h.step();
        scroll_frame.push(t.elapsed().as_secs_f64() * 1000.0);
        scroll_grid.push(h.app.caches.grid_stats.last_show.as_secs_f64() * 1000.0);
        max_scroll = max_scroll.max(h.app.grid_scroll.unwrap_or(0.0));
    }
    summary("scroll,frame", scroll_frame);
    summary("scroll,grid", scroll_grid);
    println!("scroll,start={start:?},end={:?},max={max_scroll}", h.app.grid_scroll);
    if max_scroll == 0.0 {
        println!("scroll,not_applicable=bounded_batch_fits_view");
    }
    let id = PhotoId(1);
    let old = h.app.renderer.thumb(id).unwrap().pixels.as_ref().unwrap().clone();
    let mut edit = (*h.app.session.catalog.photo(id).unwrap().develop).clone();
    edit.light.exposure += 0.5;
    let undo = h.app.session.catalog.apply(Op::SetDevelop { id, settings: Arc::new(edit), label: "audit".into(), edited: None }).unwrap();
    request_thumb(&mut h.app, id, 256, 20);
    assert!(h.settle(Duration::from_secs(120)));
    let edited = h.app.renderer.thumb(id).unwrap().pixels.as_ref().unwrap().clone();
    assert_ne!(old.pixels, edited.pixels, "edit must change actual rendered pixels");
    if std::env::var_os("VERIFY_PIXELS").is_some() {
        verify_texture(&h, id);
    }
    let mut hash = 0xcbf29ce484222325u64;
    for c in &edited.pixels {
        for b in c.to_array() {
            hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    println!("edit,exposure_plus_half_stop,pixels_checksum={hash:016x}");
    h.app.session.catalog.apply(undo).unwrap();
    request_thumb(&mut h.app, id, 256, 20);
    assert!(h.settle(Duration::from_secs(120)));
    assert_eq!(old.pixels, h.app.renderer.thumb(id).unwrap().pixels.as_ref().unwrap().pixels, "undo restores pixels");
    if std::env::var_os("VERIFY_PIXELS").is_some() {
        verify_texture(&h, id);
        println!("oracle,edit_and_undo_match_fresh_pixels=true");
    }
    println!("undo,pixels_restored=true");
}
fn main() {
    println!("mode={}", std::env::var("THUMB_AUDIT_MODE").unwrap_or_else(|_| "baseline".into()));
    if std::env::var_os("SKIP_MICRO").is_none() {
        for points in [0, 200, 1000] {
            for pending in [false, true] {
                micro(points, pending);
            }
        }
    }
    if std::env::var_os("REAL_PHOTOS").is_some() {
        real();
    }
}
