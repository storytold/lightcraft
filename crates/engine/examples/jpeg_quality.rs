//! JPEG rate–quality harness: file size at equal perceptual quality (SSIMULACRA2) for LightCraft's
//! encoder and, when installed, reference encoders, so encoder changes are measured, not guessed.
//!
//! `cargo run --release -p lightcraft-engine --example jpeg_quality -- [options] [files or dirs…]`
//!
//! Images: the procedural demo scenes (default when no files are given) and any files LightCraft
//! opens (raws, TIFF, PNG, JPEG…; directories are searched one level deep), each developed with
//! default settings as an export would see it and fitted to `--max-edge` (default 1536). Every
//! encoder runs a quality sweep on the same 8-bit sRGB pixels; each result is decoded with
//! zune-jpeg and scored against those pixels with SSIMULACRA2 (rust-av `ssimulacra2` crate, a port
//! of libjxl's tool; higher is better, ~90 is visually lossless, ~70 high quality, ~50 medium).
//!
//! Encoders (`--enc`, comma-separated; default: all that are available):
//! - `lightcraft`: `lightcraft_codecs::jpeg_par`, the export encoder.
//! - `cjpegli`: jpegli's `cjpegli` with its defaults (progressive, adaptive quantization).
//! - `cjpegli-seq`: `cjpegli -p 0`, baseline sequential like ours: the gap without progressive.
//! - `cjpegli-noaq`: `cjpegli -p 0 --noadaptive_quantization`: what jpegli's tables and quality
//!   curve alone give, without adaptive quantization.
//! - `cjpeg`: libjpeg-turbo / mozjpeg `cjpeg -optimize` (whichever `cjpeg` is installed).
//!
//! External tools are found on `PATH` or through `CJPEGLI` / `CJPEG`, and are skipped when missing;
//! they are research references, never product dependencies. Times are taken while the images run
//! in parallel, and external tools' include process start and file I/O: compare sizes and scores
//! here, and speed with `lightcraft-codecs`' `jpeg_bench` example and `cargo xtask bench`.
//!
//! Options: `--sub 444|420`, `--max-edge N`, `--qualities 50,60,…`, `--range LO:HI` (score range
//! for BD-rate, default 60:90), `--scenes` (also the demo scenes when files are given), `--csv
//! FILE` (every encode: image, encoder, quality, bytes, bpp, score, ms), `--keep DIR` (keep each
//! reference as `DIR/<image>/in.ppm` and every encode next to it, e.g. to look at artefacts or
//! cross-check scores with libjxl's `ssimulacra2` / `butteraugli_main`).
//!
//! Output: per image, BD-rate of each encoder against `lightcraft` (the average difference in
//! file size at equal score over the range; negative = smaller files than ours), then file size
//! at a few target scores summed over all images, then score and bpp at each quality setting.
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use lightcraft_codecs::ChromaSubsampling;
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::media::develop;
use lightcraft_pipeline::{RenderRequest, SourceInfo};
use rayon::prelude::*;

const DEFAULT_QUALITIES: &[u8] = &[30, 40, 50, 60, 70, 75, 80, 85, 90, 93, 95, 97];
const TARGETS: &[f64] = &[50.0, 60.0, 70.0, 80.0, 85.0, 90.0];

struct Opts {
    sub: ChromaSubsampling,
    max_edge: usize,
    qualities: Vec<u8>,
    range: (f64, f64),
    scenes: bool,
    encoders: Vec<Encoder>,
    csv: Option<PathBuf>,
    keep: Option<PathBuf>,
    inputs: Vec<PathBuf>,
}

#[derive(Clone)]
enum Encoder {
    Lightcraft,
    /// An external tool: display name, which tool and settings, program path.
    External {
        name: &'static str,
        kind: ExtKind,
        program: PathBuf,
    },
}

#[derive(Clone, Copy)]
enum ExtKind {
    Cjpegli,
    CjpegliSeq,
    CjpegliNoAq,
    Cjpeg,
}

impl Encoder {
    fn name(&self) -> &'static str {
        match self {
            Encoder::Lightcraft => "lightcraft",
            Encoder::External { name, .. } => name,
        }
    }
}

/// One test image: 8-bit sRGB, 3 channels.
struct Image {
    name: String,
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

/// One encode of one image.
#[derive(Clone)]
struct Point {
    quality: u8,
    bytes: usize,
    score: f64,
    ms: f64,
}

fn find_tool(env: &str, names: &[&str]) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env) {
        return Some(PathBuf::from(p));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

fn available_encoders() -> Vec<Encoder> {
    let mut v = vec![Encoder::Lightcraft];
    if let Some(p) = find_tool("CJPEGLI", &["cjpegli"]) {
        v.push(Encoder::External { name: "cjpegli", kind: ExtKind::Cjpegli, program: p.clone() });
        v.push(Encoder::External { name: "cjpegli-seq", kind: ExtKind::CjpegliSeq, program: p.clone() });
        v.push(Encoder::External { name: "cjpegli-noaq", kind: ExtKind::CjpegliNoAq, program: p });
    }
    if let Some(p) = find_tool("CJPEG", &["cjpeg"]) {
        v.push(Encoder::External { name: "cjpeg", kind: ExtKind::Cjpeg, program: p });
    }
    v
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        sub: ChromaSubsampling::S444,
        max_edge: 1536,
        qualities: DEFAULT_QUALITIES.to_vec(),
        range: (60.0, 90.0),
        scenes: false,
        encoders: available_encoders(),
        csv: None,
        keep: None,
        inputs: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |what: &str| args.next().ok_or(format!("{what} needs a value"));
        match a.as_str() {
            "--sub" => {
                o.sub = match val("--sub")?.as_str() {
                    "444" => ChromaSubsampling::S444,
                    "420" => ChromaSubsampling::S420,
                    s => return Err(format!("--sub {s}: expected 444 or 420")),
                }
            }
            "--max-edge" => o.max_edge = val("--max-edge")?.parse().map_err(|_| "--max-edge needs a number")?,
            "--qualities" => {
                o.qualities = val("--qualities")?
                    .split(',')
                    .map(|q| q.trim().parse::<u8>().map_err(|_| format!("bad quality `{q}`")))
                    .collect::<Result<_, _>>()?;
                if o.qualities.iter().any(|&q| q == 0 || q > 100) {
                    return Err("qualities are 1–100".into());
                }
            }
            "--range" => {
                let v = val("--range")?;
                let (lo, hi) = v.split_once(':').ok_or("--range needs LO:HI")?;
                o.range = (lo.parse().map_err(|_| "bad --range")?, hi.parse().map_err(|_| "bad --range")?);
            }
            "--scenes" => o.scenes = true,
            "--csv" => o.csv = Some(val("--csv")?.into()),
            "--keep" => o.keep = Some(val("--keep")?.into()),
            "--enc" => {
                let all = available_encoders();
                let want = val("--enc")?;
                o.encoders = Vec::new();
                for n in want.split(',') {
                    let e = all.iter().find(|e| e.name() == n).ok_or(format!("encoder `{n}` is unknown or not installed"))?;
                    o.encoders.push(e.clone());
                }
            }
            "-h" | "--help" => return Err(USAGE.into()),
            s if s.starts_with("--") => return Err(format!("unknown option `{s}`\n{USAGE}")),
            s => o.inputs.push(s.into()),
        }
    }
    if o.inputs.is_empty() {
        o.scenes = true;
    }
    o.qualities.sort_unstable();
    o.qualities.dedup();
    Ok(o)
}

const USAGE: &str = "usage: jpeg_quality [--sub 444|420] [--max-edge N] [--qualities a,b,…] [--range LO:HI] \
                     [--enc lightcraft,cjpegli,cjpegli-seq,cjpegli-noaq,cjpeg] [--scenes] [--csv FILE] [--keep DIR] [files or dirs…]";

fn to_rgb(img: &lightcraft_raster::Rgba8) -> Vec<u8> {
    img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

fn load_images(o: &Opts) -> Result<Vec<Image>, String> {
    let req = RenderRequest::fit(o.max_edge, o.max_edge);
    let settings = DevelopSettings::default();
    let mut out = Vec::new();
    if o.scenes {
        for sc in lightcraft_scenes::demo_library() {
            let src = Arc::new(sc.render_fit(o.max_edge));
            let img = develop(&src, &SourceInfo { raw: true, ..Default::default() }, &settings, &req, None, false).image;
            out.push(Image { name: format!("scene-{}", sc.name.to_lowercase().replace(' ', "-")), w: img.width, h: img.height, rgb: to_rgb(&img) });
        }
    }
    let mut files = Vec::new();
    for p in &o.inputs {
        if p.is_dir() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(p)
                .map_err(|e| format!("{}: {e}", p.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file())
                .collect();
            v.sort();
            files.extend(v);
        } else {
            files.push(p.clone());
        }
    }
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else {
            eprintln!("skipped {}: unreadable", f.display());
            continue;
        };
        // a size well above the output, so the develop step does the final resampling
        let (src, info) = match lightcraft_engine::files::load_bytes(&bytes, o.max_edge.saturating_mul(2)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("skipped {}: {e}", f.display());
                continue;
            }
        };
        let img = develop(&Arc::new(src), &info, &settings, &req, None, false).image;
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        out.push(Image { name, w: img.width, h: img.height, rgb: to_rgb(&img) });
    }
    Ok(out)
}

fn write_ppm(path: &Path, img: &Image) -> Result<(), String> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?);
    write!(f, "P6\n{} {}\n255\n", img.w, img.h).map_err(|e| e.to_string())?;
    f.write_all(&img.rgb).map_err(|e| e.to_string())?;
    f.flush().map_err(|e| e.to_string())
}

fn encode(enc: &Encoder, img: &Image, ppm: &Path, q: u8, sub: ChromaSubsampling, tmp: &Path) -> Result<(Vec<u8>, f64), String> {
    let t = Instant::now();
    let bytes = match enc {
        Encoder::Lightcraft => lightcraft_codecs::jpeg_par::encode(&img.rgb, img.w, img.h, 3, q, sub, &[]),
        Encoder::External { name, kind, program } => {
            let out = tmp.join(format!("{name}-q{q}.jpg"));
            let mut c = Command::new(program);
            let s420 = matches!(sub, ChromaSubsampling::S420);
            match kind {
                ExtKind::Cjpegli | ExtKind::CjpegliSeq | ExtKind::CjpegliNoAq => {
                    c.arg(ppm).arg(&out).arg("-q").arg(q.to_string()).arg("--quiet");
                    c.arg(format!("--chroma_subsampling={}", if s420 { "420" } else { "444" }));
                    match kind {
                        ExtKind::CjpegliSeq => drop(c.args(["-p", "0"])),
                        ExtKind::CjpegliNoAq => drop(c.args(["-p", "0", "--noadaptive_quantization"])),
                        _ => {}
                    }
                }
                ExtKind::Cjpeg => {
                    c.args(["-quality", &q.to_string(), "-optimize", "-sample", if s420 { "2x2" } else { "1x1" }, "-outfile"]).arg(&out).arg(ppm);
                }
            }
            let r = c.output().map_err(|e| format!("{name}: {e}"))?;
            if !r.status.success() {
                return Err(format!("{name} q{q} failed: {}", String::from_utf8_lossy(&r.stderr).trim()));
            }
            std::fs::read(&out).map_err(|e| format!("{name}: {e}"))?
        }
    };
    Ok((bytes, t.elapsed().as_secs_f64() * 1e3))
}

fn decode_rgb(jpg: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    let opts = zune_core::options::DecoderOptions::default().jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(zune_core::bytestream::ZCursor::new(jpg), opts);
    let px = d.decode().map_err(|e| format!("decode: {e:?}"))?;
    let (w, h) = d.dimensions().ok_or("decode: no dimensions")?;
    Ok((px, w, h))
}

fn ssim_rgb(rgb: &[u8], w: usize, h: usize) -> Result<ssimulacra2::Rgb, String> {
    let data = rgb.as_chunks::<3>().0.iter().map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]).collect();
    ssimulacra2::Rgb::new(data, w, h, ssimulacra2::TransferCharacteristic::SRGB, ssimulacra2::ColorPrimaries::BT709).map_err(|e| format!("{e:?}"))
}

fn score(reference: &Image, jpg: &[u8]) -> Result<f64, String> {
    let (px, w, h) = decode_rgb(jpg)?;
    if (w, h) != (reference.w, reference.h) {
        return Err(format!("decoded {w}×{h}, expected {}×{}", reference.w, reference.h));
    }
    let a = ssim_rgb(&reference.rgb, w, h)?;
    let b = ssim_rgb(&px, w, h)?;
    ssimulacra2::compute_frame_ssimulacra2(a, b).map_err(|e| format!("ssimulacra2: {e:?}"))
}

/// The directory for references and encodes: `--keep DIR` (created if needed, kept), or a new private directory in
/// the system temp folder, created exclusively (never one that already exists, mode 0700 on Unix) and removed when
/// this is dropped, also when a step fails. (An interrupted run leaves it behind.)
struct Scratch {
    path: PathBuf,
    remove: bool,
}

impl Scratch {
    fn new(keep: Option<&Path>) -> Result<Scratch, String> {
        if let Some(dir) = keep {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            return Ok(Scratch { path: dir.to_path_buf(), remove: false });
        }
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        for attempt in 0..100u32 {
            let path = std::env::temp_dir().join(format!("lightcraft-jpeg-quality-{}-{nanos:08x}-{attempt}", std::process::id()));
            match builder.create(&path) {
                Ok(()) => return Ok(Scratch { path, remove: true }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        Err("no free scratch directory name in the temp folder".into())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.remove {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// ln(bytes) at `score`, interpolated linearly between the sweep's points (None outside them).
fn log_rate_at(points: &[Point], s: f64) -> Option<f64> {
    let mut p: Vec<(f64, f64)> = points.iter().map(|p| (p.score, (p.bytes as f64).ln())).collect();
    p.sort_by(|a, b| a.0.total_cmp(&b.0));
    p.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
    let i = p.iter().position(|q| q.0 >= s)?;
    let hi = p.get(i)?;
    if i == 0 {
        return ((hi.0 - s).abs() < 1e-9).then_some(hi.1);
    }
    let lo = p.get(i - 1)?;
    let t = (s - lo.0) / (hi.0 - lo.0);
    Some(lo.1 + t * (hi.1 - lo.1))
}

fn score_span(points: &[Point]) -> (f64, f64) {
    points.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.score), hi.max(p.score)))
}

/// Bjøntegaard-style rate difference of `test` against `base` (piecewise-linear log-rate over
/// score, averaged over the overlap of both curves and `range`): -0.1 = 10 % smaller at equal score.
fn bd_rate(base: &[Point], test: &[Point], range: (f64, f64)) -> Option<f64> {
    let (a, b) = (score_span(base), score_span(test));
    let lo = a.0.max(b.0).max(range.0);
    let hi = a.1.min(b.1).min(range.1);
    if hi - lo < 1.0 {
        return None;
    }
    let n = 200;
    let mut sum = 0.0;
    for k in 0..=n {
        // (rounding can put the last sample an ulp above `hi`, outside both curves, when `hi - lo` is inexact)
        let s = (lo + (hi - lo) * k as f64 / n as f64).min(hi);
        sum += log_rate_at(test, s)? - log_rate_at(base, s)?;
    }
    Some((sum / (n + 1) as f64).exp() - 1.0)
}

fn pct(v: Option<f64>) -> String {
    v.map(|v| format!("{:+.1}%", v * 100.0)).unwrap_or_else(|| "—".into())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let o = parse_args()?;
    let t = Instant::now();
    let images = load_images(&o)?;
    if images.is_empty() {
        return Err("no images".into());
    }
    let names: Vec<&str> = o.encoders.iter().map(Encoder::name).collect();
    eprintln!(
        "{} images fitted to {} px ({:.1} s to develop), {:?}, encoders: {}, qualities: {:?}",
        images.len(),
        o.max_edge,
        t.elapsed().as_secs_f64(),
        o.sub,
        names.join(", "),
        o.qualities
    );
    let scratch = Scratch::new(o.keep.as_deref())?;
    let tmp = &scratch.path;

    // results[image][encoder] = sweep
    let t = Instant::now();
    let results: Vec<Vec<Vec<Point>>> = images
        .par_iter()
        .enumerate()
        .map(|(i, img)| -> Result<Vec<Vec<Point>>, String> {
            let dir = tmp.join(format!("{i:02}-{}", img.name));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let ppm = dir.join("in.ppm");
            write_ppm(&ppm, img)?;
            let mut per_enc = Vec::new();
            for enc in &o.encoders {
                let mut pts = Vec::new();
                for &q in &o.qualities {
                    let (jpg, ms) = encode(enc, img, &ppm, q, o.sub, &dir)?;
                    if o.keep.is_some() && matches!(enc, Encoder::Lightcraft) {
                        let path = dir.join(format!("lightcraft-q{q}.jpg"));
                        std::fs::write(&path, &jpg).map_err(|e| format!("{}: {e}", path.display()))?;
                    }
                    let s = score(img, &jpg).map_err(|e| format!("{} {} q{q}: {e}", img.name, enc.name()))?;
                    pts.push(Point { quality: q, bytes: jpg.len(), score: s, ms });
                }
                per_enc.push(pts);
            }
            eprintln!("  {}", img.name);
            Ok(per_enc)
        })
        .collect::<Result<_, _>>()?;
    drop(scratch);
    eprintln!("encoded and scored in {:.1} s", t.elapsed().as_secs_f64());

    if let Some(path) = &o.csv {
        let mut f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        writeln!(f, "image,width,height,encoder,subsampling,quality,bytes,bpp,ssimulacra2,ms").map_err(|e| e.to_string())?;
        for (img, per_enc) in images.iter().zip(&results) {
            for (enc, pts) in o.encoders.iter().zip(per_enc) {
                for p in pts {
                    let bpp = p.bytes as f64 * 8.0 / (img.w * img.h) as f64;
                    writeln!(
                        f,
                        "{},{},{},{},{:?},{},{},{bpp:.4},{:.3},{:.1}",
                        img.name,
                        img.w,
                        img.h,
                        enc.name(),
                        o.sub,
                        p.quality,
                        p.bytes,
                        p.score,
                        p.ms
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    let base = 0; // `lightcraft` when present, else the first encoder
    let base_name = names.first().copied().unwrap_or("?");
    let width = images.iter().map(|i| i.name.len()).max().unwrap_or(5).max(10);

    println!("\nBD-rate vs {base_name} over SSIMULACRA2 {}–{} (negative = smaller files at equal score)", o.range.0, o.range.1);
    print!("{:<width$}", "image");
    for n in names.iter().skip(1) {
        print!(" {n:>12}");
    }
    println!();
    let mut logs: Vec<Vec<f64>> = vec![Vec::new(); names.len()];
    for (img, per_enc) in images.iter().zip(&results) {
        print!("{:<width$}", img.name);
        for (e, pts) in per_enc.iter().enumerate().skip(1) {
            let r = bd_rate(&per_enc[base], pts, o.range);
            if let Some(r) = r {
                logs[e].push((1.0 + r).ln());
            }
            print!(" {:>12}", pct(r));
        }
        println!();
    }
    print!("{:<width$}", "mean");
    for l in logs.iter().skip(1) {
        let m = (!l.is_empty()).then(|| (l.iter().sum::<f64>() / l.len() as f64).exp() - 1.0);
        print!(" {:>12}", pct(m));
    }
    println!();

    println!("\nTotal size at equal SSIMULACRA2 (all images, bits per pixel; images outside an encoder's sweep left out)");
    print!("{:<8}", "score");
    for n in &names {
        print!(" {n:>12}");
    }
    println!("  images");
    let px: Vec<f64> = images.iter().map(|i| (i.w * i.h) as f64).collect();
    for &s in TARGETS {
        // images where every encoder reaches the score within its sweep
        let rows: Vec<(usize, Vec<f64>)> = results
            .iter()
            .enumerate()
            .filter_map(|(i, per_enc)| per_enc.iter().map(|pts| log_rate_at(pts, s).map(f64::exp)).collect::<Option<Vec<f64>>>().map(|v| (i, v)))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let total_px: f64 = rows.iter().map(|(i, _)| px[*i]).sum();
        print!("{s:<8}");
        for e in 0..names.len() {
            let bytes: f64 = rows.iter().map(|(_, v)| v[e]).sum();
            print!(" {:>12.3}", bytes * 8.0 / total_px);
        }
        println!("  {}", rows.len());
    }

    println!("\nMean SSIMULACRA2 / bits per pixel / ms per MP at each quality setting");
    print!("{:<8}", "quality");
    for n in &names {
        print!(" {:>24}", n);
    }
    println!();
    let total_px: f64 = px.iter().sum();
    for (k, q) in o.qualities.iter().enumerate() {
        print!("{q:<8}");
        for e in 0..names.len() {
            let mut agg: BTreeMap<&str, f64> = BTreeMap::new();
            for per_enc in &results {
                if let Some(p) = per_enc.get(e).and_then(|pts| pts.get(k)) {
                    *agg.entry("score").or_default() += p.score;
                    *agg.entry("bytes").or_default() += p.bytes as f64;
                    *agg.entry("ms").or_default() += p.ms;
                }
            }
            let n = results.len() as f64;
            print!(
                " {:>8.2} {:>7.3} {:>7.1}",
                agg.get("score").copied().unwrap_or(0.0) / n,
                agg.get("bytes").copied().unwrap_or(0.0) * 8.0 / total_px,
                agg.get("ms").copied().unwrap_or(0.0) / (total_px / 1e6)
            );
        }
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sweep whose ln(bytes) is linear in score: bytes = scale · e^(score / 10).
    fn sweep(scale: f64) -> Vec<Point> {
        (0..8).map(|k| 50.0 + 6.0 * k as f64).map(|s| Point { quality: 0, bytes: (scale * (s / 10.0).exp()) as usize, score: s, ms: 0.0 }).collect()
    }

    #[test]
    fn bd_rate_measures_a_constant_size_ratio() {
        let r = bd_rate(&sweep(1000.0), &sweep(800.0), (60.0, 90.0)).expect("overlap");
        assert!((r + 0.2).abs() < 1e-3, "{r}");
        let r = bd_rate(&sweep(1000.0), &sweep(1000.0), (60.0, 90.0)).expect("overlap");
        assert!(r.abs() < 1e-3, "{r}");
    }

    /// Over a wide score span `lo + (hi - lo)` rounds above `hi`; the last sample must still land on both curves
    /// when `hi` is their top score (review of #3450).
    #[test]
    fn bd_rate_samples_stay_inside_a_wide_span() {
        let (lo, hi): (f64, f64) = (15.803_672_619_522_928, 57.287_717_255_928_19);
        assert!(lo + (hi - lo) * 200.0 / 200.0 > hi, "the case this test needs");
        let curve = |scale: f64| -> Vec<Point> {
            [lo, 30.0, 45.0, hi].iter().map(|&s| Point { quality: 0, bytes: (scale * (s / 10.0).exp()) as usize, score: s, ms: 0.0 }).collect()
        };
        let r = bd_rate(&curve(1000.0), &curve(1000.0), (0.0, 100.0)).expect("the curves overlap from lo to hi");
        assert!(r.abs() < 1e-2, "{r}");
    }

    #[test]
    fn bd_rate_needs_overlapping_curves() {
        let low: Vec<Point> = sweep(1000.0).into_iter().filter(|p| p.score < 65.0).collect();
        let high: Vec<Point> = sweep(1000.0).into_iter().filter(|p| p.score > 75.0).collect();
        assert_eq!(bd_rate(&low, &high, (0.0, 100.0)), None);
    }

    #[test]
    fn log_rate_interpolates_inside_the_sweep_only() {
        let s = sweep(1000.0);
        let mid = log_rate_at(&s, 59.0).expect("inside");
        assert!((mid - (1000.0f64.ln() + 5.9)).abs() < 1e-2, "{mid}");
        assert_eq!(log_rate_at(&s, 40.0), None);
        assert_eq!(log_rate_at(&s, 99.0), None);
    }
}
