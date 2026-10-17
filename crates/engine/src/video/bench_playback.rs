//! Playback cost per frame (M14.4 perf): decode at playback size, CPU LUT grade, and the RGBA
//! conversion the loupe does before a texture upload. `#[ignore]`d: run with
//! `cargo test -p lcc-engine --release video::bench_playback -- --ignored --nocapture`.
//! Corpus clips (`corpus/video`, `cargo xtask corpus --download`) are measured when present; a
//! synthetic 4K H.264 clip is made in-test.

use std::path::{Path, PathBuf};
use std::time::Instant;

use lcc_pipeline::Quality;
use lcc_pipeline::grade::GradeLut;

use super::VideoBackend;
use super::native::LccVideo;
use super::playback::PLAY_MAX_SIDE;

const FRAMES: usize = 48;

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    lut: lcc_gpu::video_lut::VideoLut,
    target: Option<lcc_gpu::video_lut::LutTarget>,
}

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let lut = lcc_gpu::video_lut::VideoLut::new(&device).ok()?;
    Some(Gpu { device, queue, lut, target: None })
}

/// Upload + LUT shader, waited for (the display path doesn't wait; this bounds its cost).
fn gpu_grade(g: &mut Gpu, img: &lcc_raster::Rgb32f, lut: &GradeLut) -> f64 {
    let t = Instant::now();
    if g.target.as_ref().is_none_or(|t| t.size() != [img.width, img.height]) {
        g.target = lcc_gpu::video_lut::LutTarget::new(&g.device, img.width, img.height).ok();
    }
    let Some(target) = g.target.as_ref() else { return f64::NAN };
    if g.lut.grade(&g.device, &g.queue, img, 1, lut, target).is_err() {
        return f64::NAN;
    }
    let _ = g.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    ms(t.elapsed())
}

fn measure(path: &Path, lut: &GradeLut, mut gpu: Option<&mut Gpu>) {
    let b = LccVideo::default();
    let Ok(mut it) = b.frames_from(path, 0.0, Some(PLAY_MAX_SIDE)) else {
        println!("{}: can't open", path.display());
        return;
    };
    let (mut dec, mut grade, mut conv, mut gsum, mut n) = (0.0, 0.0, 0.0, 0.0, 0usize);
    let mut size = (0, 0);
    while n < FRAMES {
        let t0 = Instant::now();
        let Some(Ok((_, img))) = it.next() else { break };
        let t1 = Instant::now();
        let g = lut.apply(&img, Quality::Full);
        let t2 = Instant::now();
        // what `render::color_image` does before `load_texture`
        let px: Vec<u32> = g.data.iter().map(|p| u32::from_le_bytes(*p)).collect();
        std::hint::black_box(&px);
        let t3 = Instant::now();
        let gt = gpu.as_deref_mut().map(|g| gpu_grade(g, &img, lut));
        if n > 0 {
            dec += ms(t1 - t0);
            grade += ms(t2 - t1);
            conv += ms(t3 - t2);
            gsum += gt.unwrap_or(0.0);
        }
        size = (img.width, img.height);
        n += 1;
    }
    let k = n.saturating_sub(1).max(1) as f64;
    let gpu = if gpu.is_some() { format!("  gpu-lut {:5.2} ms", gsum / k) } else { String::new() };
    println!(
        "{:<34} {}x{}  decode {:6.2} ms  cpu-lut {:6.2} ms  rgba {:5.2} ms{}",
        path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
        size.0,
        size.1,
        dec / k,
        grade / k,
        conv / k,
        gpu
    );
}

#[test]
#[ignore = "benchmark"]
fn playback_cost_per_frame() {
    let mut s = lcc_develop::DevelopSettings::default();
    s.light.exposure = 0.3;
    s.light.contrast = 20.0;
    s.color.vibrance = 15.0;
    let lut = GradeLut::bake(&s).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/video");
    let mut clips: Vec<PathBuf> = std::fs::read_dir(&root).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    clips.sort();
    let dir = std::env::temp_dir().join(format!("lcc-bench-playback-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let uhd = dir.join("synthetic-h264-2160p.mp4");
    lcc_video::test_util::write_clip(&uhd, 3840, 2160, FRAMES + 1, |k| (16 + 3 * k).min(235) as u8, &[]);
    clips.push(uhd);
    let mut g = gpu();
    for c in &clips {
        measure(c, &lut, g.as_mut());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `start_linear` hands frames over ungraded, with the LUT; graded they match `start`'s.
#[test]
fn linear_playback_frames_carry_the_lut() {
    use super::playback::{FramePixels, Playback};
    let dir = std::env::temp_dir().join(format!("lcc-linear-playback-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("c.mp4");
    lcc_video::test_util::write_clip(&clip, 64, 48, 4, |k| (40 + 20 * k) as u8, &[]);
    let mut s = lcc_develop::DevelopSettings::default();
    s.light.exposure = 0.5;
    let b: std::sync::Arc<dyn VideoBackend> = std::sync::Arc::new(LccVideo::default());
    let first = |mut p: Playback| {
        let t = Instant::now();
        loop {
            // at t = 0 only the first frame is due, whatever the worker has decoded since
            if let Some(f) = p.poll(0.0, 0.0).frame {
                return f;
            }
            assert!(t.elapsed().as_secs() < 30, "no frame");
            std::thread::yield_now();
        }
    };
    let lin = first(Playback::start_linear(b.clone(), clip.clone(), 0.0, 64, &s).unwrap());
    let cpu = first(Playback::start(b, clip, 0.0, 64, &s).unwrap());
    assert!(matches!(lin.pixels, FramePixels::Linear { .. }));
    assert!(matches!(cpu.pixels, FramePixels::Graded(_)));
    assert_eq!(lin.pixels.graded(), cpu.pixels.graded());
    let _ = std::fs::remove_dir_all(&dir);
}
