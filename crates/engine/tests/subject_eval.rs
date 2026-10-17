//! BiRefNet's Subject selection scored on the labelled demo scenes (like
//! `crates/pipeline/src/tests_mask_eval.rs` scores the photo analysis): soft IoU of the model's
//! matte against the scenes' Subject share, and CPU time per photo.
//! `{ENV_PREFIX}_BIREFNET_DIR=<model dir> cargo test --release -p lcc-engine --features sam --test subject_eval -- --nocapture`.
//! Skipped (passes) without the model.
#![cfg(feature = "sam")]

use lcc_scenes::Label;

const EDGE: usize = 384;

#[test]
fn birefnet_subject_iou_on_demo_scenes() {
    let Some(dir) = lcc_brand::env_os("BIREFNET_DIR") else {
        eprintln!("skipped: set {}", lcc_brand::env_var("BIREFNET_DIR"));
        return;
    };
    let model = lcc_segment::birefnet::BiRefNet::load(std::path::Path::new(&dir)).unwrap();
    let (mut scores, mut times) = (Vec::new(), Vec::new());
    for scene in lcc_scenes::demo_library() {
        let img = scene.render_fit(EDGE);
        let (w, h) = (img.width, img.height);
        let truth = scene.labels(w, h);
        let total: f64 = truth.iter().map(|t| t[Label::Subject as usize] as f64).sum();
        if total <= 0.002 * truth.len() as f64 {
            continue;
        }
        let big = scene.render_fit(lcc_pipeline::auto_mask::MODEL_EDGE);
        let rgb = lcc_pipeline::auto_mask::model_input(&big);
        let t = std::time::Instant::now();
        let m = model.predict(&rgb, big.width, big.height).unwrap();
        times.push(t.elapsed().as_secs_f64());
        // the square matte, stretched back over the frame (nearest cell)
        let (mut inter, mut uni) = (0.0f64, 0.0f64);
        for y in 0..h {
            for x in 0..w {
                let (mx, my) = ((x * m.side) / w, (y * m.side) / h);
                let a = m.data[my * m.side + mx] as f64;
                let t = truth[y * w + x][Label::Subject as usize] as f64;
                inter += a.min(t);
                uni += a.max(t);
            }
        }
        if let Ok(out) = std::env::var("MASK_EVAL_DUMP") {
            // photo | matte | reference, side by side
            let mut buf = format!("P6\n{} {}\n255\n", 3 * w, h).into_bytes();
            let rgb_small = lcc_pipeline::auto_mask::model_input(&img);
            for y in 0..h {
                buf.extend_from_slice(&rgb_small[y * w * 3..(y + 1) * w * 3]);
                for x in 0..w {
                    let v = (m.data[(y * m.side / h) * m.side + x * m.side / w] * 255.0) as u8;
                    buf.extend_from_slice(&[v; 3]);
                }
                for x in 0..w {
                    buf.extend_from_slice(&[(truth[y * w + x][Label::Subject as usize] * 255.0) as u8; 3]);
                }
            }
            std::fs::write(format!("{out}/subject-{:?}-{}.ppm", scene.kind, scene.seed), buf).unwrap();
        }
        let s = inter / uni.max(1e-9);
        println!("Subject {:?} seed {}: IoU {s:.3} ({:.2} s)", scene.kind, scene.seed, times.last().unwrap());
        scores.push(s);
    }
    let mean = scores.iter().sum::<f64>() / scores.len().max(1) as f64;
    let time = times.iter().sum::<f64>() / times.len().max(1) as f64;
    println!("BiRefNet Subject: mean IoU {mean:.3} over {} scenes, {time:.2} s per photo", scores.len());
    assert!(mean > 0.4, "{mean}");
}
