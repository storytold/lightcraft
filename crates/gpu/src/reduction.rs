//! SDR histogram and result validation, without downloading pixels. The reduction is separate
//! from presentation: today's CPU-readable renders append its 4108 bytes to their pixel readback;
//! a future native-texture path (#1228) can read just `Reduction::buffer`.

use lightcraft_pipeline::PixelWindow;
use lightcraft_raster::Histogram;

use crate::ctx::{Buf, FailKind, fail};
use crate::render::Cx;

const HIST_WORDS: usize = 4 * Histogram::BINS + 1;
pub(crate) const WORDS: usize = HIST_WORDS + 2;

pub(crate) struct Summary {
    pub histogram: Histogram,
    pub unwritten: u32,
}

pub(crate) struct Reduction {
    pub buffer: Buf,
    pixels: u32,
    samples: u32,
}

impl Reduction {
    /// Record two bounded dispatches using only baseline WGSL workgroup atomics. Histogram
    /// sampling matches `Histogram::of_srgb8` of the kept region; validation visits ALL pixels,
    /// including context outside it and pixels the histogram's sampling skips.
    pub fn encode(cx: &mut Cx<'_>, image: &Buf, w: usize, h: usize, keep: Option<PixelWindow>) -> Option<Self> {
        let keep = keep.unwrap_or(PixelWindow { x: 0, y: 0, w, h });
        let n = w.checked_mul(h);
        let area = keep.w.checked_mul(keep.h);
        let valid = n.zip(area).filter(|(n, _)| {
            *n <= image.len
                && u32::try_from(*n).is_ok()
                && keep.x.checked_add(keep.w).is_some_and(|x| x <= w)
                && keep.y.checked_add(keep.h).is_some_and(|y| y <= h)
                && u32::try_from(w).is_ok()
                && u32::try_from(h).is_ok()
        });
        let Some((n, area)) = valid else {
            fail(FailKind::Fatal, "invalid GPU histogram dimensions".into());
            return None;
        };
        let step = ((area as f64 / 1_000_000.0).sqrt().ceil() as usize).max(1);
        let samples = keep.w.div_ceil(step).checked_mul(keep.h.div_ceil(step)).and_then(|s| u32::try_from(s).ok())?;
        // Bound temporary storage to ~1 MiB, independent of source size. Groups process strided
        // batches, so neighbouring lanes read neighbouring pixels even for large images.
        let groups = n.div_ceil(4096).clamp(1, 256);
        let partials = cx.zeroed(groups * WORDS);
        let buffer = cx.zeroed(WORDS);
        let p = [w as u32, n as u32, keep.x as u32, keep.y as u32, keep.w as u32, keep.h as u32, step as u32, groups as u32];
        let buffers = [Some(image), Some(&partials), Some(&buffer)];
        cx.run("summary_partial", &p, &buffers, [groups as u32, 1, 1]);
        cx.run("summary_reduce", &p, &buffers, [WORDS.div_ceil(256) as u32, 1, 1]);
        Some(Self { buffer, pixels: n as u32, samples })
    }

    /// Reject a missing/partial reduction as well as inconsistent bins. Cleared buffers plus
    /// the visited and sampled totals prevent a dropped dispatch from looking like a black photo.
    pub fn decode(&self, words: &[u32]) -> Result<Summary, String> {
        decode(words, self.pixels, self.samples)
    }
}

/// Decode exactly one summary, requiring complete pixel coverage and the expected sampled
/// total in every channel. Invalid lengths, counters or bin sums return an error.
fn decode(words: &[u32], pixels: u32, samples: u32) -> Result<Summary, String> {
    let malformed = || "incomplete or inconsistent histogram/validation reduction".to_string();
    if words.len() != WORDS {
        return Err(malformed());
    }
    let histogram = words.get(..HIST_WORDS).and_then(Histogram::from_words).ok_or_else(malformed)?;
    let unwritten = words.get(HIST_WORDS).copied().ok_or_else(malformed)?;
    let visited = words.get(HIST_WORDS + 1).copied().ok_or_else(malformed)?;
    let valid_bins = [&histogram.r, &histogram.g, &histogram.b, &histogram.luma]
        .iter()
        .all(|bins| bins.iter().map(|&n| u64::from(n)).sum::<u64>() == u64::from(samples));
    if visited != pixels || unwritten > pixels || histogram.total != samples || !valid_bins {
        return Err(malformed());
    }
    Ok(Summary { histogram, unwritten })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{ErrorScopes, RenderScope};
    use lightcraft_raster::Rgba8;

    /// Reject truncated, dropped and corrupt summaries without overflowing bin totals.
    #[test]
    fn malformed_or_dropped_summaries_are_rejected() {
        assert!(decode(&[], 5, 5).is_err());
        assert!(decode(&vec![0; WORDS], 5, 5).is_err());
        let mut words = Histogram::of_srgb8(&Rgba8::filled(5, 1, [0, 0, 0, 255])).to_words();
        words.truncate(HIST_WORDS);
        words.extend([0, 5]);
        assert!(decode(&words, 5, 5).is_ok());
        words[HIST_WORDS + 1] = 4;
        assert!(decode(&words, 5, 5).is_err());
        words[HIST_WORDS + 1] = 5;
        words[0] = u32::MAX; // corrupt bin: sum must not overflow or pass validation
        assert!(decode(&words, 5, 5).is_err());
        words[0] = 5;
        words[HIST_WORDS] = 6;
        assert!(decode(&words, 5, 5).is_err());
    }

    /// Compare compact or paired readback with CPU histogram sampling and full-image alpha counts.
    fn check(cx: &mut Cx<'_>, image: &Rgba8, keep: Option<PixelWindow>, pixels_too: bool) {
        let source = cx.gpu.upload(&image.data);
        let reduction = Reduction::encode(cx, &source, image.width, image.height, keep).expect("valid dimensions");
        let words = if pixels_too {
            let (pixels, summary): (Vec<[u8; 4]>, _) = cx.read_with_tail(&source, image.len(), &reduction.buffer);
            assert_eq!(pixels, image.data);
            summary
        } else {
            cx.read(&reduction.buffer, WORDS)
        };
        assert_eq!(words.len() * 4, 4108, "compact readback size independent of image dimensions");
        let summary = reduction.decode(&words).expect("complete reduction");
        let kept = keep.map(|k| image.crop(k.x, k.y, k.w, k.h));
        assert_eq!(summary.histogram, Histogram::of_srgb8(kept.as_ref().unwrap_or(image)));
        assert_eq!(summary.unwritten as usize, image.data.iter().filter(|p| p[3] != 255).count());
    }

    /// Exercise sampling thresholds, offset crops, constant colours and unsampled invalid pixels.
    #[test]
    fn gpu_reduction_matches_cpu_sampling_and_validates_every_pixel() {
        let Some(gpu) = crate::device() else {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        let _scope = RenderScope::new(gpu);
        let errors = ErrorScopes::push(gpu);
        println!("reduction adapter: {} ({:?}); driver: {} {}", gpu.info.name, gpu.info.backend, gpu.info.driver, gpu.info.driver_info);
        let mut cx = Cx::new(gpu);
        for (w, h) in [(1, 1), (17, 19), (1000, 1000), (1001, 1000), (2101, 2003)] {
            let image = Rgba8::from_fn(w, h, |x, y| [(x % 256) as u8, (y % 256) as u8, ((x + 3 * y) % 256) as u8, 255]);
            check(&mut cx, &image, None, false);
            if w > 1 {
                check(&mut cx, &image, Some(PixelWindow { x: 1, y: 1, w: w - 1, h: h - 1 }), true);
            }
        }
        for rgb in [[0, 0, 0, 255], [255; 4], [12, 128, 240, 255]] {
            check(&mut cx, &Rgba8::filled(1201, 1001, rgb), None, false);
        }
        let mut partial = Rgba8::filled(1201, 1001, [8, 9, 10, 255]);
        // One unsampled pixel and one pixel outside the histogram's kept region are still checked.
        partial.set(1, 1, [0; 4]);
        partial.set(1200, 1000, [255, 255, 255, 254]);
        check(&mut cx, &partial, None, false);
        check(&mut cx, &partial, Some(PixelWindow { x: 3, y: 5, w: 1101, h: 991 }), false);
        assert!(errors.pop().is_none(), "GPU errors");
    }

    /// A readback longer than its source must record a failure instead of panicking.
    #[test]
    fn malformed_readback_returns_an_error_instead_of_panicking() {
        let Some(gpu) = crate::device() else {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        let _scope = RenderScope::new(gpu);
        let source = gpu.upload(&[123u32]);
        assert!(gpu.finish_and_read::<u32>(gpu.encoder(), &source, 2).is_empty());
        assert!(crate::ctx::take_failure().is_some());
    }

    /// Repeatable microbenchmark: input upload/creation and shader compilation are outside the
    /// measurement. The CPU scan is timed separately; end-to-end process CPU time comes from
    /// `cargo xtask bench`. No corpus, model or UI is required.
    #[test]
    #[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
    fn bench_result_reduction() {
        use std::hint::black_box;
        use std::time::Instant;
        let Some(gpu) = crate::device() else {
            lightcraft_testkit::skip!(Gpu, "no GPU device");
            return;
        };
        println!("adapter: {} ({:?})", gpu.info.name, gpu.info.backend);
        let _scope = RenderScope::new(gpu);
        let errors = ErrorScopes::push(gpu);
        let mut cx = Cx::new(gpu);
        let iterations = std::env::var("N").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(20).clamp(1, 1000);
        /// Warm up once and report the minimum wall-clock latency over `n` measured iterations.
        fn best(n: usize, mut f: impl FnMut()) -> f64 {
            f(); // warm up pipeline and pool
            (0..n)
                .map(|_| {
                    let start = Instant::now();
                    f();
                    start.elapsed().as_secs_f64() * 1e3
                })
                .fold(f64::INFINITY, f64::min)
        }
        for (w, h) in [(256, 256), (1920, 1280), (3072, 2304), (6000, 4000)] {
            let image = Rgba8::from_fn(w, h, |x, y| [(x % 256) as u8, (y % 256) as u8, ((x + 3 * y) % 256) as u8, 255]);
            let source = gpu.upload(&image.data);
            let scan = |image: &Rgba8| {
                black_box(Histogram::of_srgb8(image));
                // The previous result-validation scan, including its fast opaque-chunk path.
                black_box(
                    image
                        .data
                        .chunks(4096)
                        .filter(|c| c.iter().fold(255u8, |a, p| a & p[3]) != 255)
                        .map(|c| c.iter().filter(|p| p[3] != 255).count())
                        .sum::<usize>(),
                );
            };
            let cpu_scan = best(iterations, || scan(&image));
            let old = best(iterations, || {
                let data = cx.read(&source, w * h);
                scan(&Rgba8 { width: w, height: h, data });
            });
            let compact = best(iterations, || {
                let r = Reduction::encode(&mut cx, &source, w, h, None).expect("reduction");
                let words = cx.read(&r.buffer, WORDS);
                black_box(r.decode(&words).expect("summary"));
            });
            let combined = best(iterations, || {
                let r = Reduction::encode(&mut cx, &source, w, h, None).expect("reduction");
                let (pixels, words): (Vec<[u8; 4]>, _) = cx.read_with_tail(&source, w * h, &r.buffer);
                black_box(pixels);
                black_box(r.decode(&words).expect("summary"));
            });
            println!(
                "{w}x{h}: CPU scan {cpu_scan:.3} ms; old readback + CPU {old:.3} ms; GPU summary only {compact:.3} ms; GPU summary + pixels {combined:.3} ms"
            );
            println!("  readback bytes: old {}; summary {}; combined {}", w * h * 4, WORDS * 4, w * h * 4 + WORDS * 4);
        }
        assert!(errors.pop().is_none(), "GPU errors");
    }
}
