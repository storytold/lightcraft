//! The GPU memory one export job holds at its peak, by source and output size: the table the
//! export gate's estimate (`lightcraft_gpu::gate::job_bytes`) is fitted to.
//!
//! `cargo run --release -p lightcraft-engine --example vram_peak -- [--out DIR] [--sdr] [--two] [RAW…]`
//!
//! - Sources: procedural scenes of 24, 45 and 61 MP (16-bit TIFFs written to `--out`, default the
//!   system's temp folder), plus any raw files given.
//! - Outputs: full size, 6000 px and 2048 px on the long edge; JPEG with an HDR gain map (`--sdr`:
//!   plain JPEG) and the radial mask of `wedding_export_bench`.
//! - For each: the device pool is emptied, then one photo is exported on the GPU, one lane; the
//!   peak of the renderer's buffers above what existed before is the job's peak. `--two` also
//!   exports two at once (two lanes, two GPU jobs) for the peak of a pair.
//! - Prints one tab-separated line per case: source MP, output MP, peak MB (one job), peak MB
//!   (two jobs, `--two`), the gate's estimate MB.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskShape};
use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::export::{Destination, ExportOptions, export_batch};
use lightcraft_geom::Point;

fn settings(hdr: bool) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.35;
    s.light.contrast = 12.0;
    s.light.highlights = -45.0;
    s.light.shadows = 35.0;
    s.effects.clarity = 8.0;
    s.effects.texture = 5.0;
    s.effects.dehaze = 4.0;
    s.detail.sharpen_amount = 40.0;
    s.detail.nr_luminance = 15.0;
    s.detail.nr_color = 25.0;
    s.hdr.enabled = hdr;
    let mut m = Mask {
        id: 1,
        name: "mask 1".into(),
        visible: true,
        components: vec![MaskComponent {
            name: None,
            op: Default::default(),
            invert: false,
            shape: MaskShape::Radial { center: Point::new(0.5, 0.45), rx: 0.35, ry: 0.3, angle: 0.0, feather: 60.0, invert: true },
        }],
        ..Default::default()
    };
    m.adjust.exposure = -0.3;
    s.masks = vec![m];
    s
}

/// A procedural `w × h` photo as an uncompressed 16-bit TIFF.
fn scene_tiff(dir: &std::path::Path, w: usize, h: usize) -> String {
    let path = dir.join(format!("vram-peak-{w}x{h}.tif"));
    if !path.is_file() {
        let img = lightcraft_scenes::demo_library()[0].render(w, h);
        let px: Vec<u16> = img.data.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 65535.0) as u16)).collect();
        drop(img);
        let e = lightcraft_codecs::EncodeImage::new(w as u32, h as u32, 3, lightcraft_codecs::Samples::U16(&px));
        let bytes = lightcraft_codecs::encode_tiff(&e, lightcraft_codecs::TiffCompression::None, &Default::default()).expect("tiff");
        std::fs::write(&path, bytes).expect("write");
    }
    path.to_string_lossy().into_owned()
}

/// The peak of the renderer's buffers above what existed before, exporting `ids` side by side.
fn peak(session: &mut lightcraft_engine::Session, ids: &[PhotoId], o: &ExportOptions) -> (u64, (u32, u32)) {
    let _ = lightcraft_engine::gpu::wait_idle(std::time::Duration::from_secs(10));
    lightcraft_engine::gpu::trim_pool(0);
    let before = lightcraft_engine::gpu::memory().allocated;
    lightcraft_engine::gpu::reset_memory_peak();
    let files = export_batch(session, ids, o, &Destination::default(), &mut |_, _| Ok(()), &|_| false).expect("export");
    for f in &files {
        assert!(f.get("gpuFallback").is_none(), "fell back to the CPU: {f}");
        assert!(f.get("error").is_none(), "{f}");
    }
    let size = files.first().map(|f| (f["width"].as_u64().unwrap_or(0) as u32, f["height"].as_u64().unwrap_or(0) as u32)).unwrap_or_default();
    (lightcraft_engine::gpu::memory().peak.saturating_sub(before), size)
}

fn main() {
    lightcraft_engine::memory::set_ram_probe(lightcraft_sysmem::total_physical_memory);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)).map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let raws: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && args.get(i.wrapping_sub(1)).is_none_or(|p| p != "--out"))
        .map(|(_, a)| a.clone())
        .collect();
    let hdr = !flag("--sdr");
    lightcraft_engine::gpu::set_enabled(true);
    assert!(lightcraft_engine::gpu::available(), "no GPU");
    println!("# {}, {}", lightcraft_engine::gpu::adapter_name().unwrap_or_default(), if hdr { "HDR gain map" } else { "SDR" });
    println!("source_mp\tout_mp\tone_mb\ttwo_mb\testimate_mb");
    let mut sources = raws;
    for (w, h) in [(6000, 4000), (8256, 5504), (9504, 6336)] {
        sources.push(scene_tiff(&out, w, h));
    }
    for src in &sources {
        for edge in [None, Some(6000u32), Some(2048)] {
            let mut session = lightcraft_engine::Session::new().with_fs();
            let r = session.execute("library.import", &serde_json::json!({"paths": [src]})).expect("import");
            let id = PhotoId(r["imported"][0].as_u64().expect("id"));
            session.set_develop(id, settings(hdr), "bench").expect("settings");
            let src_px = session.catalog.photo(id).map(|p| p.width as u64 * p.height as u64).unwrap_or(0);
            let mut o = ExportOptions { hdr, quality: 90, ..ExportOptions::default() };
            o.resize = edge.map(lightcraft_engine::export::Resize::long_edge);
            lightcraft_engine::export::set_export_lanes(Some(1));
            lightcraft_engine::gpu::set_batch_limits(Some(u64::MAX >> 1), Some(1));
            let (one, (w, h)) = peak(&mut session, &[id], &o);
            let out_px = w as u64 * h as u64;
            let two = if flag("--two") {
                lightcraft_engine::export::set_export_lanes(Some(2));
                lightcraft_engine::gpu::set_batch_limits(Some(u64::MAX >> 1), Some(2));
                peak(&mut session, &[id, id], &o).0
            } else {
                0
            };
            let estimate = lightcraft_engine::gpu::export_job_bytes(src_px as usize, out_px as usize);
            println!("{:.1}\t{:.1}\t{}\t{}\t{}", src_px as f64 / 1e6, out_px as f64 / 1e6, one >> 20, two >> 20, estimate >> 20);
            lightcraft_engine::gpu::set_batch_limits(None, None);
        }
    }
}
