//! Diagnostics on a real bracket (not run by default): how far apart the frames still are after the
//! HDR merge's alignment, tile by tile.
//!
//! `LIGHTCRAFT_BRACKET=a.ARW,b.ARW,c.ARW [LIGHTCRAFT_REGION=x0,y0,x1,y1] cargo test --release -p lightcraft-merge
//! --test real_bracket -- --ignored --nocapture`

use lightcraft_geom::Homography;
use lightcraft_merge::hdr::{Deghost, HdrOptions, merge_hdr};
use lightcraft_merge::{align, flow, load_frame, metrics, no_progress, refine};
use lightcraft_raster::Plane;

fn crop(p: &Plane, r: [usize; 4]) -> Plane {
    let [x0, y0, x1, y1] = r;
    Plane::from_fn(x1 - x0, y1 - y0, |x, y| p.get(x + x0, y + y0))
}

#[test]
#[ignore]
fn residual_after_alignment() {
    let Ok(paths) = std::env::var("LIGHTCRAFT_BRACKET") else { return };
    let frames: Vec<_> = paths.split(',').map(|p| load_frame(&std::fs::read(p).unwrap(), None, true).unwrap()).collect();
    let r = merge_hdr(frames.clone(), &HdrOptions { align: true, deghost: Deghost::None }, &no_progress).unwrap();
    let (w, h) = (frames[0].width(), frames[0].height());
    let region = std::env::var("LIGHTCRAFT_REGION")
        .ok()
        .and_then(|s| s.split(',').map(|v| v.parse().ok()).collect::<Option<Vec<usize>>>())
        .and_then(|v| v.try_into().ok())
        .unwrap_or([0, 0, w, h]);
    let gain = |i: usize| 2f32.powf(-r.ev[i] as f32) * 2.0;
    let rf = r.reference;
    let ref_luma = refine::luma(&frames[rf].image, gain(rf), frames[rf].clip);
    eprintln!("{w}×{h}, reference {rf}, ev {:?}", r.ev);
    for i in (0..frames.len()).filter(|&i| i != rf) {
        let a = &r.alignments[i];
        let raw = refine::luma(&frames[i].image, gain(i), frames[i].clip);
        let (global, _) = align::warp(&frames[i].image, &a.h);
        let lg = refine::luma(&global, gain(i), frames[i].clip);
        let fl = flow::estimate(&ref_luma, &raw, &a.h, flow::RADIUS);
        let (warped, _) = flow::warp(&frames[i].image, &a.h, &fl);
        let l = refine::luma(&warped, gain(i), frames[i].clip);
        eprintln!(
            "frame {i}: global only: whole {:.2?}, region {:.2?}; local shifts up to {:.2} px ({} of {} tiles measured), merge reported {:.2}",
            metrics::residual(&ref_luma, &lg),
            metrics::residual(&crop(&ref_luma, region), &crop(&lg, region)),
            fl.max(),
            fl.measured,
            fl.dx.width * fl.dx.height,
            r.local_shift[i]
        );
        let whole = metrics::residual(&ref_luma, &l);
        let (rr, rl) = (crop(&ref_luma, region), crop(&l, region));
        let local = metrics::residual(&rr, &rl);
        eprintln!("frame {i}: {} ({} inliers, rms {:.2}); residual whole {whole:.2?}; region {region:?} {local:.2?}", a.model, a.inliers, a.rms);
        if std::env::var("LIGHTCRAFT_TILES").is_ok() {
            for s in refine::sites(&rr, 5) {
                if let Some(m) = refine::match_patch(&rr, s, &rl, &Homography::IDENTITY, 3) {
                    eprintln!(
                        "    at ({:5.0}, {:5.0}) shift ({:+.2}, {:+.2}) score {:.2}",
                        m.reference.x + region[0] as f64,
                        m.reference.y + region[1] as f64,
                        m.offset.x,
                        m.offset.y,
                        m.score
                    );
                }
            }
        }
    }
}
