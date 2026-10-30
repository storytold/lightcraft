#![allow(unsafe_code)] // Test-only allocator instrumentation, never product code.
// Test-only allocator instrumentation. Reject a single allocation above 1 GiB before
// requesting any pages, so a virtual-frame probe cannot exhaust the shared machine.
use std::alloc::{GlobalAlloc, Layout, System};
struct Guard;
unsafe impl GlobalAlloc for Guard {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() > 1 << 30 {
            eprintln!("REJECTED_ALLOCATION bytes={} limit=1073741824", layout.size());
            std::process::exit(77);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.size() > 1 << 30 {
            eprintln!("REJECTED_ALLOCATION bytes={} limit=1073741824", layout.size());
            std::process::exit(77);
        }
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, n: usize) -> *mut u8 {
        if n > 1 << 30 {
            eprintln!("REJECTED_ALLOCATION bytes={n} limit=1073741824");
            std::process::exit(77);
        }
        unsafe { System.realloc(p, layout, n) }
    }
}
#[global_allocator]
static ALLOC: Guard = Guard;
#[test]
fn window_render_stays_below_the_allocation_guard() {
    use lightcraft_pipeline::{PixelWindow, RenderRequest, SourceInfo};
    let src = lightcraft_raster::Rgb32f::from_fn(600, 400, |x, y| {
        let v = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 120.;
        [v, v * 0.82, v * 1.15]
    });
    let mut s = lightcraft_develop::DevelopSettings::default();
    s.effects.dehaze = 100.;
    let req = RenderRequest { window: Some(PixelWindow { x: 20000, y: 12000, w: 400, h: 300 }), ..RenderRequest::fit(48000, 32000) };
    let p = lightcraft_pipeline::plan(&src, &SourceInfo::default(), &s, &req);
    println!("PLAN work={}x{} requested=400x300 virtual=48000x32000", p.w, p.h);
    let result = lightcraft_pipeline::render(&src, &SourceInfo::default(), &s, &req);
    let rendered = result.unwrap();
    assert_eq!((rendered.image.width, rendered.image.height), (400, 300));
}
