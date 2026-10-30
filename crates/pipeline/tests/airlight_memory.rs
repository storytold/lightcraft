#![allow(unsafe_code)] // Test-only allocation accounting, never product code.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
struct Counting;
fn added(n: usize) {
    let live = LIVE.fetch_add(n, Ordering::SeqCst) + n;
    PEAK.fetch_max(live, Ordering::SeqCst);
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            added(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            added(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, n: usize) -> *mut u8 {
        let out = unsafe { System.realloc(p, layout, n) };
        if !out.is_null() {
            if n >= layout.size() {
                added(n - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - n, Ordering::SeqCst);
            }
        }
        out
    }
}
#[global_allocator]
static ALLOC: Counting = Counting;

#[test]
fn exact_preparation_estimate_covers_measured_live_allocations() {
    use lightcraft_pipeline::{RenderRequest, SourceInfo, airlight};
    let src = lightcraft_raster::Rgb32f::from_fn(1201, 803, |x, y| {
        let v = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 120.;
        [v, v * 0.82, v * 1.15]
    });
    let info = SourceInfo::default();
    let mut s = lightcraft_develop::DevelopSettings::default();
    s.effects.dehaze = 15.;
    // Start Rayon and its worker-local allocations before measuring preparation.
    let warm = lightcraft_pipeline::plan(&src, &info, &s, &RenderRequest::fit(1201, 803));
    airlight::compute(&src, &info, &warm).unwrap();
    for (lum, col, defringe, heal) in [
        (0., 0., false, false),
        (30., 0., false, false),
        (0., 25., false, false),
        (30., 25., false, false),
        (0., 0., true, false),
        (0., 0., false, true),
    ] {
        s.detail.nr_luminance = lum;
        s.detail.nr_color = col;
        s.optics.defringe_purple_amount = if defringe { 50. } else { 0. };
        s.spots = if heal {
            vec![lightcraft_develop::Spot {
                mode: lightcraft_develop::SpotMode::Heal,
                points: vec![lightcraft_geom::Point::new(0.5, 0.5)],
                source_offset: Some(lightcraft_geom::Point::new(0.2, 0.0)),
                size: 0.2,
                ..Default::default()
            }]
        } else {
            Vec::new()
        };
        let plan = lightcraft_pipeline::plan(&src, &info, &s, &RenderRequest::fit(1201, 803));
        let baseline = LIVE.load(Ordering::SeqCst);
        PEAK.store(baseline, Ordering::SeqCst);
        airlight::compute(&src, &info, &plan).unwrap();
        let peak = PEAK.load(Ordering::SeqCst).saturating_sub(baseline) as u64;
        let estimate = airlight::prepare_bytes(&plan, None, false);
        eprintln!("NR luminance={lum} colour={col}, defringe={defringe}, heal={heal}: measured={peak} estimated={estimate}");
        assert!(peak <= estimate, "measured={peak}, estimate={estimate}");
    }
}
