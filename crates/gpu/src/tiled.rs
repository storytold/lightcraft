//! Tiled rendering on the GPU: the CPU's tile plan ([`lightcraft_pipeline::tiled::TilePlan`]: the
//! tiles, their halos, the whole-image guided-filter grids and the dehaze airlight) driven with the
//! same kernels as a whole render ([`crate::render`]). Each tile's region is resampled on the CPU
//! (no full-size source or oriented copy on the device) and uploaded; every device buffer is the
//! size of one region. So an output over the device's storage-buffer limit renders on the GPU,
//! and a full-size export within a memory cap needs neither a full-size source on the device nor
//! the CPU's slower per-tile work.

use std::borrow::Cow;

use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::finish::FinishParams;
use lightcraft_pipeline::tiled::{GridView, Rect, TileOptions, TilePlan};
use lightcraft_pipeline::{Plan, RenderRequest, Rendered, SourceInfo, local};
use lightcraft_raster::{Histogram, Rgb32f, Rgba8};

use crate::ctx::{Buf, FailKind, Gpu, fail};
use crate::params::{Present, finish_block};
use crate::render::{
    Cx, Host, Prep, finish_pass, gaussian, guided_fast, linear, map, masks, median_hv, pack_taps, profiling, rgb_words, tone_base, unwritten,
};

/// Pixels of a tile's region when tiling only because the output exceeds the device's buffer
/// limit (the largest that fits, up to this): big tiles overlap less.
const MAX_REGION_PX: usize = 4 << 20;

/// Region size for rendering `s` in tiles on `gpu` because the whole output is over its limit.
pub(crate) fn options_for_limit(gpu: &Gpu, s: &DevelopSettings) -> TileOptions {
    // the widest buffer per pixel: the masks (one plane each, then the blurred chromaticity), or RGB
    let masks = s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).count();
    let words = (masks + 3).max(3);
    let fit = (gpu.limit() as usize / 4 / words) / 10 * 9;
    TileOptions { region_px: fit.min(MAX_REGION_PX), core: None }
}

/// A whole-image guided-filter grid on the device: its coefficients, interleaved (a, b).
struct Grid<'g> {
    ab: Buf,
    lw: usize,
    lh: usize,
    view: GridView<'g>,
}

/// The plan of region `r` of `tp`'s output: buffers of the region's size, the whole output's
/// frame and radii, red eyes moved into the region.
fn region_plan<'p>(tp: &'p TilePlan<'_>, r: Rect) -> Plan<'p> {
    let p = &tp.plan;
    let (dx, dy) = (r.x0 as f32, r.y0 as f32);
    let eyes = p
        .eyes
        .iter()
        .map(|e| lightcraft_pipeline::redeye::EyeK {
            cx: e.cx - dx,
            cy: e.cy - dy,
            catchlight: e.catchlight.map(|(x, y, r)| (x - dx, y - dy, r)),
            ..*e
        })
        .collect();
    Plan {
        settings: Cow::Borrowed(&*p.settings),
        frame: tp.frame_of(r),
        w: r.w,
        h: r.h,
        px_per_long: p.px_per_long,
        src_long: p.src_long,
        detail_scale: p.detail_scale,
        geo: 0,
        lin_key: 0,
        pre_nr_key: 0,
        eyes,
        // the tiled path places the airlight itself (from the whole frame's samples)
        fixed_air: p.fixed_air,
        keep: None,
        mattes: p.mattes.clone(),
    }
}

/// The scene-linear image of a region on the device from its resampled source `img` (see
/// [`resampled`]): white balance, red eye (defringe on the CPU) and noise reduction as in a whole
/// render.
fn region_linear(cx: &mut Cx<'_>, img: Rgb32f, info: &SourceInfo, rp: &Plan<'_>, host: &mut Host) -> Buf {
    let sampled = cx.gpu.upload(rgb_words(&img));
    host.sampled = Some(img);
    linear(cx, &sampled, info, rp, host)
}

/// `f` with the source resampled into each of `regions` in turn (`Frame::sample` of its tile
/// frame), resampled on another thread one region ahead: the CPU resamples the next region while
/// the device works on this one.
fn resampled<R>(tp: &TilePlan<'_>, src: &Rgb32f, regions: &[Rect], f: impl FnOnce(&mut dyn Iterator<Item = Rgb32f>) -> R) -> R {
    let frames: Vec<_> = regions.iter().map(|r| (tp.frame_of(*r), *r)).collect();
    resampled_frames(frames, src, f)
}

/// [`resampled`] for regions with their frames.
fn resampled_frames<R>(
    frames: Vec<(lightcraft_pipeline::geometry::Frame, Rect)>,
    src: &Rgb32f,
    f: impl FnOnce(&mut dyn Iterator<Item = Rgb32f>) -> R,
) -> R {
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Rgb32f>(1);
        scope.spawn(move || {
            for (frame, r) in frames {
                if tx.send(frame.sample(src, r.w, r.h)).is_err() {
                    return; // the render stopped early
                }
            }
        });
        f(&mut rx.into_iter())
    })
}

/// The base or clarity plane over the region of `rp` from a whole-image grid: the coefficients
/// upsampled with the region's share of the bilinear taps, then `a·log_l + b`.
fn grid_plane(cx: &mut Cx<'_>, g: &Grid<'_>, log_l: &Buf, r: Rect) -> Buf {
    let tx = cx.gpu.upload(&pack_taps(&g.view.up_x[r.x0..r.x0 + r.w]));
    let tmp = cx.gpu.buffer(r.w * g.lh * 2);
    let p = [g.lw as u32, g.lh as u32, r.w as u32, 2];
    cx.run("resize_h", &p, &[Some(&g.ab), Some(&tx), Some(&tmp)], crate::ctx::groups2(r.w, g.lh, [16, 16]));
    let ty = cx.gpu.upload(&pack_taps(&g.view.up_y[r.y0..r.y0 + r.h]));
    let up = cx.gpu.buffer(r.w * r.h * 2);
    let p = [r.w as u32, g.lh as u32, r.h as u32, 2];
    cx.run("resize_v", &p, &[Some(&tmp), Some(&ty), Some(&up)], crate::ctx::groups2(r.w, r.h, [16, 16]));
    let q = cx.gpu.buffer(r.w * r.h);
    map(cx, "guided_apply", r.w * r.h, &[], [Some(log_l), Some(&up), None], &q);
    q
}

/// The spatial planes of region `r` (`local::prepare`): per region, except the planes from
/// whole-image grids and the airlight, which come from the first pass.
fn region_prepare(cx: &mut Cx<'_>, tp: &TilePlan<'_>, grids: &[Option<Grid<'_>>; 2], lin: &Buf, r: Rect) -> Prep {
    let (w, h, n) = (r.w, r.h, r.w * r.h);
    let sig = tp.sig;
    let l = cx.gpu.buffer(n);
    map(cx, "log_lum_k", n, &[], [Some(lin), None, None], &l);
    let plane = |cx: &mut Cx<'_>, sigma: Option<f32>, grid: &Option<Grid<'_>>, eps: f32| match (sigma, grid) {
        (Some(_), Some(g)) => Some(std::sync::Arc::new(grid_plane(cx, g, &l, r))),
        (Some(sg), None) => Some(std::sync::Arc::new(guided_fast(cx, &l, w, h, sg, eps))),
        (None, _) => None,
    };
    // process V2: the base filters the median of log luminance (local::tone_base)
    let base = match (sig.base, &grids[0]) {
        (Some(_), Some(g)) if sig.base_detail > 0 => {
            let m = median_hv(cx, &l, w, h, sig.base_detail);
            Some(std::sync::Arc::new(grid_plane(cx, g, &m, r)))
        }
        (Some(sg), None) => Some(std::sync::Arc::new(tone_base(cx, &l, w, h, sg, sig.base_detail))),
        _ => plane(cx, sig.base, &grids[0], local::BASE_EPS),
    };
    let clarity = plane(cx, sig.clarity, &grids[1], local::CLARITY_EPS);
    let texture = sig.texture.map(|sg| std::sync::Arc::new(gaussian(cx, &l, w, h, 1, sg)));
    let dark = sig.dark.map(|sg| {
        let d0 = cx.gpu.buffer(n);
        map(cx, "dark_k", n, &[], [Some(lin), None, None], &d0);
        std::sync::Arc::new(gaussian(cx, &d0, w, h, 1, sg))
    });
    let chroma = sig.chroma.map(|sg| {
        let ch = cx.gpu.buffer(n * 3);
        map(cx, "chroma_k", n, &[], [Some(lin), None, None], &ch);
        std::sync::Arc::new(gaussian(cx, &ch, w, h, 3, sg))
    });
    let log_l = std::sync::Arc::new(l);
    let base = base.unwrap_or_else(|| log_l.clone());
    Prep { log_l, base, clarity, texture, dark, chroma, air: tp.air }
}

/// Render tile by tile on `gpu` (see the module docs). `None`: the render can't be tiled (the
/// caller decides what then; nothing is recorded), or it failed (recorded with [`fail`]).
pub(crate) fn render_tiled(
    gpu: &Gpu,
    src: &Rgb32f,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    opts: &TileOptions,
    fault: Option<crate::Fault>,
) -> Option<Rendered> {
    let t0 = profiling().then(std::time::Instant::now);
    let mut tp = TilePlan::new(src, info, s, req, opts)?;
    let (gw, gh) = (tp.gw, tp.gh);
    let regions: Vec<Rect> = tp.cores.iter().map(|c| tp.region(*c)).collect();
    let biggest = regions.iter().map(|r| r.w * r.h).max().unwrap_or(0);
    let nmask = tp.plan.settings.masks.iter().filter(|m| m.visible && !m.components.is_empty()).count();
    if !gpu.fits(biggest * (nmask + 3).max(3)) {
        fail(FailKind::Limit, format!("tiles of {biggest} pixels with {nmask} masks are over the device's {} MiB buffer limit", gpu.limit() >> 20));
        return None;
    }

    // pass A: the whole-image values, from each region's log luminance and dark channel
    if tp.needs_gather() {
        let frames: Vec<_> = regions.iter().map(|r| (tp.frame_of(*r), *r)).collect();
        let cores = tp.cores.clone();
        resampled_frames(frames, src, |imgs| {
            for ((c, r), img) in cores.into_iter().zip(regions.iter().copied()).zip(imgs) {
                let mut cx = Cx::new(gpu);
                let rp = region_plan(&tp, r);
                let mut host = Host::default();
                let lin = region_linear(&mut cx, img, info, &rp, &mut host);
                let n = r.w * r.h;
                let l = cx.gpu.buffer(n);
                map(&mut cx, "log_lum_k", n, &[], [Some(&lin), None, None], &l);
                let dark = tp.sig.dark.map(|sg| {
                    let d0 = cx.gpu.buffer(n);
                    map(&mut cx, "dark_k", n, &[], [Some(&lin), None, None], &d0);
                    gaussian(&mut cx, &d0, r.w, r.h, 1, sg)
                });
                // process V2's base filters the median of log luminance, taken over the whole region
                let detail = tp.sig.base_detail;
                let med = (tp.sig.base.is_some() && detail > 0).then(|| median_hv(&mut cx, &l, r.w, r.h, detail));
                // only the core's rows are gathered
                let rows = Rect { y0: c.y0, h: c.h, ..r };
                let (off, len) = ((c.y0 - r.y0) * r.w, c.h * r.w);
                let l = cx.slice(&l, off, len);
                let med = med.map(|m| cx.slice(&m, off, len));
                let dark = dark.map(|d| cx.slice(&d, off, len));
                let log_l = cx.read_plane(&l, r.w, c.h);
                let base_l = med.map(|m| cx.read_plane(&m, r.w, c.h));
                let dark = dark.map(|d| cx.read_plane(&d, r.w, c.h));
                if crate::ctx::failed() {
                    return None;
                }
                tp.gather(c, rows, &log_l, base_l.as_ref(), dark.as_ref());
            }
            Some(())
        })?;
        tp.solve();
    }
    let t_gather = t0.map(|t| t.elapsed());

    // the grids' coefficients, uploaded once
    let grids: [Option<Grid<'_>>; 2] = [false, true].map(|clarity| {
        tp.grid(clarity).map(|view| {
            let ab: Vec<f32> = view.a.data.iter().zip(&view.b.data).flat_map(|(a, b)| [*a, *b]).collect();
            Grid { ab: gpu.upload(&ab), lw: view.a.width, lh: view.a.height, view }
        })
    });

    // pass B: each tile rendered over its region, its core read back into the output
    let s = &*tp.plan.settings;
    let mut image = Rgba8::new(gw, gh);
    resampled(&tp, src, &regions, |imgs| {
        for ((&c, &r), img) in tp.cores.iter().zip(&regions).zip(imgs) {
            let mut cx = Cx::new(gpu);
            let rp = region_plan(&tp, r);
            let mut host = Host::default();
            let lin = region_linear(&mut cx, img, info, &rp, &mut host);
            let prep = region_prepare(&mut cx, &tp, &grids, &lin, r);
            let (masks, terms) = masks(&mut cx, &lin, &prep, &rp, &mut host);
            let mut fp = FinishParams::new(s, &rp.frame, info, gw, gh, rp.px_per_long, prep.air, req.space);
            (fp.x0, fp.y0) = (r.x0, r.y0);
            let present = Present {
                clarity: prep.clarity.is_some(),
                texture: prep.texture.is_some(),
                dark: prep.dark.is_some(),
                chroma: masks.is_some() && prep.chroma.is_some(),
            };
            let out = finish_pass(&mut cx, &lin, &prep, masks.as_ref(), finish_block(&fp, &terms, &present, (r.w, r.h)), (r.w, r.h), fault);
            // the core's rows only
            let (ox, oy) = (c.x0 - r.x0, c.y0 - r.y0);
            let rows = cx.slice(&out, oy * r.w, c.h * r.w);
            let data: Vec<[u8; 4]> = cx.read(&rows, c.h * r.w);
            if crate::ctx::failed() {
                return None;
            }
            let core: Vec<[u8; 4]> = data.chunks(r.w).flat_map(|row| row[ox..ox + c.w].iter().copied()).collect();
            let missing = unwritten(&core);
            if missing > 0 {
                fail(FailKind::Fatal, format!("the GPU returned an incomplete tile ({missing} of {} pixels unwritten)", c.w * c.h));
                return None;
            }
            lightcraft_pipeline::tiled::put(&mut image.data, &core, gw, c, 1);
        }
        Some(())
    })?;
    let histogram = Histogram::of_srgb8(&image);
    if gw * gh >= 4096 && [&histogram.r, &histogram.g, &histogram.b].iter().all(|ch| ch.first() == Some(&histogram.total)) {
        fail(FailKind::Blank, "the GPU result is entirely black; rendering it on the CPU to be sure".into());
        return None;
    }
    if let (Some(t0), Some(ta)) = (t0, t_gather) {
        eprintln!("  gpu tiled: {} tiles, gather {:.1} ms, total {:.1} ms", tp.cores.len(), ta.as_secs_f64() * 1e3, t0.elapsed().as_secs_f64() * 1e3);
    }
    Some(Rendered { image, histogram, deep: None })
}
