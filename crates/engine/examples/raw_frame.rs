//! Measure where a camera's own JPEG sits inside its raw sensor data, and which edge bands of that data hold no
//! picture: `cargo xtask raw-frame [--json] FILE…` (or `cargo run --release -p lightcraft-engine --example raw_frame --
//! [--json] FILE…`). It checks the crop any decoder applies against the camera's own framing, and measures the entries
//! of Nikon `MEASURED_BODIES` (`crates/raw/src/vendor/nef.rs`) for bodies that record
//! no crop. It works for any colour-filter raw that embeds a full-size camera JPEG (checked on Nikon, Sony, Canon CR2 /
//! CR3, Pentax and Samsung files, where it finds the decoder's crop); bodies with a smaller embedded preview (most
//! Fujifilm, Panasonic and Olympus files) get the edge bands only.
//!
//! **Frame.** For each raw it aligns the largest embedded JPEG with the sensor data: both are reduced to half-size
//! luminance (the raw by summing each 2×2 colour-filter block), and every offset that keeps the JPEG inside the data is
//! scored by the correlation of their gradient magnitudes. The best offset is refined to sub-pixel precision; a peak
//! that is weak, ambiguous or between two pixels is flagged. Files are grouped by model and data size (bit depths
//! pooled: both read out the same area); when 3 clean files agree, the group prints its table entries (siblings of the
//! same readout that agree on the same frame count towards the 3; see `MIN_FILES`). Use several
//! files with detail across the frame: a blank sky, fog or a lens cap has nothing to align on (low score). The JPEG
//! must be the camera's full-size one, unrotated and without in-camera geometry corrections (distortion control off);
//! a smaller preview is not aligned.
//!
//! **Edge bands.** From each edge inwards (at most 128 lines) it counts lines that hold no picture: *empty or dark*
//! (mean below 1% of the range over black) or *masked* (flat, patternless and above black: even and odd samples
//! alike within 4 standard errors or within 0.4% of white, since some filler holds a small fixed offset between the
//! two, as on the D90 and D5000; spread no more than the line's own neighbour noise and under 0.4% of white). A
//! band that the decoder's crop still includes is developed as picture: the coloured bars of issue #976. This is a
//! survey aid: a dark band can also be deep shadow, so read it with the frame.
use std::collections::BTreeMap;

use lightcraft_codecs::DecodeOptions;
use lightcraft_raw::RawData;

/// Smallest correlation peak worth trusting, and how far it must stand above the best offset outside its 3×3
/// neighbourhood (measured: 0.75–0.88 on detailed D5100 / D7000 frames, runners-up ≥ 0.05 lower).
const MIN_SCORE: f64 = 0.5;
const MIN_MARGIN: f64 = 0.02;
/// Largest sub-pixel distance from a whole pixel accepted as a clean alignment.
const MAX_FRACTION: f64 = 0.35;
/// Search at most this many half-size offsets in each direction (384 px of margin around the JPEG: Canon bodies keep
/// up to ~280 px of masked border).
const MAX_SEARCH: usize = 192;
/// Windows of up to this many offsets are searched exhaustively (every Nikon body); larger ones coarse to fine.
const FULL_SEARCH: usize = 2048;
/// Edge bands are counted up to this many lines from each edge.
const MAX_BAND: usize = 128;
/// Clean, agreeing files an entry needs: of one model and data size (bit depths pooled: both read out the same area),
/// or, with at least one of its own, together with sibling models of the same readout (data and JPEG size) whose own
/// files agree on the same frame.
const MIN_FILES: usize = 3;

struct Frame {
    jpeg: (usize, usize),
    /// Sub-pixel offset of the JPEG's top left corner in sensor pixels.
    at: (f64, f64),
    score: f64,
    margin: f64,
}

impl Frame {
    fn whole(&self) -> (usize, usize) {
        (self.at.0.round().max(0.0) as usize, self.at.1.round().max(0.0) as usize)
    }
    fn clean(&self) -> bool {
        let frac = |v: f64| (v - v.round()).abs();
        self.score >= MIN_SCORE && self.margin >= MIN_MARGIN && frac(self.at.0) <= MAX_FRACTION && frac(self.at.1) <= MAX_FRACTION
    }
}

/// Lines without picture at one edge: empty or dark ones, then masked ones (or the other way round), from the edge.
#[derive(Clone, Copy, Default)]
struct Band {
    dark: usize,
    masked: usize,
}

impl Band {
    fn total(self) -> usize {
        self.dark + self.masked
    }
}

struct Measured {
    model: String,
    size: (usize, usize),
    bits: u32,
    black: f64,
    white: f64,
    /// The crop the decoder applies, in sensor-data coordinates: x, y, width, height.
    crop: (usize, usize, usize, usize),
    /// Left, right, top, bottom.
    bands: [Band; 4],
    frame: Result<Frame, String>,
    /// With a clean frame: what the crop holds beyond it.
    beyond: Option<[Beyond; 4]>,
}

impl Measured {
    /// Lines of each edge band (left, right, top, bottom) inside the decoder's crop: developed as picture.
    fn in_crop(&self) -> [usize; 4] {
        let (w, h) = self.size;
        let (x, y, cw, ch) = self.crop;
        let [l, r, t, b] = self.bands.map(Band::total);
        [l.saturating_sub(x), (x + cw).saturating_sub(w - r.min(w)), t.saturating_sub(y), (y + ch).saturating_sub(h - b.min(h))]
    }
}

/// What the decoder's crop holds outside the camera's JPEG frame, per side (left, right, top, bottom): lines that are
/// masked (flat above black: a coloured bar), dark (empty or optical black: a dark edge) or picture (trimmed by the
/// camera, harmless).
#[derive(Clone, Copy, Default)]
struct Beyond {
    masked: usize,
    dark: usize,
    picture: usize,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let files: Vec<&String> = args.iter().filter(|a| *a != "--json").collect();
    if files.is_empty() {
        eprintln!("usage: raw_frame [--json] FILE… (raw files whose largest embedded JPEG is the camera's full-size one)");
        std::process::exit(2);
    }
    // model, data size, JPEG size -> (bit depth, frame) of each file
    let mut groups: BTreeMap<(String, (usize, usize), (usize, usize)), Vec<(u32, Frame)>> = BTreeMap::new();
    for f in files {
        let m = match measure(f) {
            Ok(m) => m,
            Err(e) => {
                if json {
                    println!("{}", serde_json::json!({ "file": f, "error": e }));
                } else {
                    println!("{f}: skipped: {e}");
                }
                continue;
            }
        };
        let in_crop = m.in_crop();
        if json {
            let band = |b: Band| serde_json::json!({ "dark": b.dark, "masked": b.masked });
            let frame = match &m.frame {
                Ok(fr) => serde_json::json!({
                    "jpeg": [fr.jpeg.0, fr.jpeg.1], "at": [fr.at.0, fr.at.1], "score": fr.score, "margin": fr.margin, "clean": fr.clean()
                }),
                Err(e) => serde_json::json!({ "error": e }),
            };
            println!(
                "{}",
                serde_json::json!({
                    "file": f, "model": m.model, "size": [m.size.0, m.size.1], "bits": m.bits, "black": m.black, "white": m.white,
                    "crop": [m.crop.0, m.crop.1, m.crop.2, m.crop.3],
                    "bands": { "left": band(m.bands[0]), "right": band(m.bands[1]), "top": band(m.bands[2]), "bottom": band(m.bands[3]) },
                    "in_crop": { "left": in_crop[0], "right": in_crop[1], "top": in_crop[2], "bottom": in_crop[3] },
                    "beyond_frame": m.beyond.map(|b| {
                        let side = |b: Beyond| serde_json::json!({ "masked": b.masked, "dark": b.dark, "picture": b.picture });
                        serde_json::json!({ "left": side(b[0]), "right": side(b[1]), "top": side(b[2]), "bottom": side(b[3]) })
                    }),
                    "frame": frame,
                })
            );
        } else {
            let frame = match &m.frame {
                Ok(fr) => format!(
                    "JPEG {}×{} at ({:.2}, {:.2}), score {:.3}, margin {:.3}{}",
                    fr.jpeg.0,
                    fr.jpeg.1,
                    fr.at.0,
                    fr.at.1,
                    fr.score,
                    fr.margin,
                    if fr.clean() { "" } else { " (not clean: weak, ambiguous or between pixels)" }
                ),
                Err(e) => format!("no frame: {e}"),
            };
            println!("{f}: {} {}×{} {}-bit, {frame}", m.model, m.size.0, m.size.1, m.bits);
            let side = |name: &str, b: Band| match (b.dark, b.masked) {
                (0, 0) => None,
                (d, 0) => Some(format!("{name} {d} dark")),
                (0, k) => Some(format!("{name} {k} masked")),
                (d, k) => Some(format!("{name} {d} dark + {k} masked")),
            };
            let bands: Vec<String> = ["left", "right", "top", "bottom"].iter().zip(m.bands).filter_map(|(n, b)| side(n, b)).collect();
            let shown: Vec<String> =
                ["left", "right", "top", "bottom"].iter().zip(in_crop).filter(|(_, n)| *n > 0).map(|(s, n)| format!("{s} {n}")).collect();
            println!(
                "    edge bands: {}; decoder crop ({}, {}, {}, {}){}",
                if bands.is_empty() { "none".to_string() } else { bands.join(", ") },
                m.crop.0,
                m.crop.1,
                m.crop.2,
                m.crop.3,
                match m.beyond {
                    Some(b) => {
                        let parts: Vec<String> = ["left", "right", "top", "bottom"]
                            .iter()
                            .zip(b)
                            .filter(|(_, b)| b.masked + b.dark + b.picture > 0)
                            .map(|(s, b)| format!("{s} {} masked / {} dark / {} picture", b.masked, b.dark, b.picture))
                            .collect();
                        if parts.is_empty() {
                            "; nothing beyond the camera frame".to_string()
                        } else {
                            format!("; beyond the camera frame: {}", parts.join(", "))
                        }
                    }
                    None if shown.is_empty() => String::new(),
                    None => format!(" still includes edge bands: {} lines", shown.join(", ")),
                }
            );
        }
        if let Ok(fr) = m.frame {
            groups.entry((m.model.clone(), m.size, fr.jpeg)).or_default().push((m.bits, fr));
        }
    }
    if json {
        return;
    }
    println!();
    // each group's agreed frame: (model, data size, JPEG size) -> (offset, clean files), when its clean files agree
    let agreed: BTreeMap<&(String, (usize, usize), (usize, usize)), ((usize, usize), usize)> = groups
        .iter()
        .filter_map(|(key, frames)| {
            let offsets: Vec<(usize, usize)> = frames.iter().filter(|(_, f)| f.clean()).map(|(_, f)| f.whole()).collect();
            let first = *offsets.first()?;
            offsets.iter().all(|o| *o == first).then_some((key, (first, offsets.len())))
        })
        .collect();
    for (key, frames) in &groups {
        let (model, size, jpeg) = key;
        let clean = frames.iter().filter(|(_, f)| f.clean()).count();
        let mut depths: Vec<u32> = frames.iter().map(|(b, _)| *b).collect();
        depths.sort_unstable();
        depths.dedup();
        let label = format!("{model} {}×{}", size.0, size.1);
        let Some(&((x, y), _)) = agreed.get(key) else {
            if clean == 0 {
                println!("{label}: no clean alignment in {} files; no entry", frames.len());
            } else {
                let offsets: Vec<(usize, usize)> = frames.iter().filter(|(_, f)| f.clean()).map(|(_, f)| f.whole()).collect();
                println!("{label}: files disagree: {offsets:?}; no entry");
            }
            continue;
        };
        let note = if clean < frames.len() { format!(", {} not clean", frames.len() - clean) } else { String::new() };
        println!("{label}: {clean} of {} files agree on ({x}, {y}){note}", frames.len());
        // siblings: other models whose sensor readout matches (same data and JPEG size) and whose own files agree on
        // the same frame
        let siblings: Vec<(&str, usize)> = agreed
            .iter()
            .filter(|((m, s, j), (o, _))| m != model && s == size && j == jpeg && *o == (x, y))
            .map(|((m, _, _), (_, n))| (m.as_str(), *n))
            .collect();
        let pooled = clean + siblings.iter().map(|(_, n)| n).sum::<usize>();
        if clean < MIN_FILES {
            if pooled < MIN_FILES {
                println!("    pending: {} more clean file(s) needed for an entry", MIN_FILES - pooled);
                continue;
            }
            let names: Vec<String> = siblings.iter().map(|(m, n)| format!("{m} ({n})")).collect();
            println!("    with siblings of the same readout and frame: {}", names.join(", "));
        }
        for bits in depths {
            println!(
                "    MeasuredBody {{ model: {model:?}, size: ({}, {}), bits: {bits}, crop: ({x}, {y}, {}, {}), white: None }},",
                size.0, size.1, jpeg.0, jpeg.1
            );
        }
    }
}

fn measure(path: &str) -> Result<Measured, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let raw = lightcraft_raw::decode(&bytes).map_err(|e| e.to_string())?;
    let RawData::U16(d) = &raw.data else { return Err("not 16-bit integer sensor data".into()) };
    if raw.cpp != 1 || raw.cfa.is_none() {
        return Err("not colour-filter sensor data".into());
    }
    let (w, h) = (raw.width, raw.height);
    if w < 4 || h < 4 || d.len() < w.saturating_mul(h) {
        return Err("sensor data shorter than its size".into());
    }
    let black = f64::from(raw.black.values.iter().sum::<f32>() / raw.black.values.len().max(1) as f32).max(0.0);
    let white = f64::from(raw.white.first().copied().unwrap_or(((1u32 << raw.bits.min(16)) - 1) as f32)).max(black + 1.0);
    let at = |x: usize, y: usize| f64::from(d.get(y * w + x).copied().unwrap_or(0));
    let bands = [
        band((0..w.min(MAX_BAND)).map(|x| line(h, |i| at(x, i))), black, white),
        band((0..w.min(MAX_BAND)).map(|k| line(h, |i| at(w - 1 - k, i))), black, white),
        band((0..h.min(MAX_BAND)).map(|y| line(w, |i| at(i, y))), black, white),
        band((0..h.min(MAX_BAND)).map(|k| line(w, |i| at(i, h - 1 - k))), black, white),
    ];
    // the decoder's crop is relative to the active area: in sensor-data coordinates here
    let a = raw.active_area;
    let c = lightcraft_raw::Rect::new(a.x + raw.crop.x, a.y + raw.crop.y, raw.crop.width, raw.crop.height);
    let frame = frame(&bytes, d, w, h);
    let beyond = frame.as_ref().ok().filter(|f| f.clean()).map(|f| {
        let ((fx, fy), (fw, fh)) = (f.whole(), f.jpeg);
        let kinds = |lines: Vec<Line>| {
            lines.iter().fold(Beyond::default(), |mut b, l| {
                match kind(l, black, white) {
                    Kind::Dark => b.dark += 1,
                    Kind::Masked => b.masked += 1,
                    Kind::Picture => b.picture += 1,
                }
                b
            })
        };
        let col = |x: usize| line(h, |i| at(x, i));
        let row = |y: usize| line(w, |i| at(i, y));
        [
            kinds((c.x..fx.min(c.x + c.width)).map(col).collect()),
            kinds(((fx + fw).max(c.x)..c.x + c.width).map(col).collect()),
            kinds((c.y..fy.min(c.y + c.height)).map(row).collect()),
            kinds(((fy + fh).max(c.y)..c.y + c.height).map(row).collect()),
        ]
    });
    Ok(Measured {
        model: raw.metadata.model.clone().unwrap_or_default().trim().to_string(),
        size: (w, h),
        bits: raw.bits,
        black,
        white,
        crop: (c.x, c.y, c.width, c.height),
        bands,
        frame,
        beyond,
    })
}

/// Statistics of one line of sensor samples: mean, |even − odd| mean, spread, neighbour noise (same colour, 2 apart)
/// and the number of sample pairs.
struct Line {
    mean: f64,
    pattern: f64,
    spread: f64,
    noise: f64,
    pairs: f64,
}

fn line(n: usize, at: impl Fn(usize) -> f64) -> Line {
    let stride = ((n / 512).max(1)).div_ceil(2) * 2;
    let (mut k, mut even, mut odd, mut var, mut noise) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let idx: Vec<usize> = (0..n.saturating_sub(2)).step_by(stride).collect();
    for &i in &idx {
        even += at(i);
        odd += at(i + 1);
        k += 1.0;
    }
    let (even, odd) = (even / k.max(1.0), odd / k.max(1.0));
    for &i in &idx {
        var += (at(i) - even).powi(2);
        noise += (at(i + 2) - at(i)).powi(2) / 2.0;
    }
    Line { mean: (even + odd) / 2.0, pattern: (even - odd).abs(), spread: (var / k.max(1.0)).sqrt(), noise: (noise / k.max(1.0)).sqrt(), pairs: k }
}

enum Kind {
    Dark,
    Masked,
    Picture,
}

fn kind(l: &Line, black: f64, white: f64) -> Kind {
    let range = white - black;
    let slack = white / 4096.0;
    if l.mean < black + range * 0.01 {
        Kind::Dark
    } else if l.mean > black + range * 0.02
        && l.mean < white * 0.25
        // patternless within the noise, or with a small fixed offset between the CFA phases (D90 / D5000 filler)
        && (l.pattern < 4.0 * l.noise * (2.0 / l.pairs.max(1.0)).sqrt() + slack || l.pattern < white * 0.004)
        && l.spread < 1.5 * l.noise + slack
        && l.spread < white * 0.004
    {
        Kind::Masked
    } else {
        Kind::Picture
    }
}

fn band(lines: impl Iterator<Item = Line>, black: f64, white: f64) -> Band {
    let mut b = Band::default();
    for l in lines {
        match kind(&l, black, white) {
            Kind::Dark => b.dark += 1,
            Kind::Masked => b.masked += 1,
            Kind::Picture => break,
        }
    }
    b
}

fn frame(bytes: &[u8], d: &[u16], w: usize, h: usize) -> Result<Frame, String> {
    let jpeg = lightcraft_raw::embedded_preview(bytes).ok_or("no embedded preview")?;
    let jpeg = lightcraft_codecs::decode(&jpeg, DecodeOptions::default()).map_err(|e| e.to_string())?;
    let (jw, jh) = (jpeg.image.width, jpeg.image.height);
    if jw > w || jh > h || jw * 10 < w * 9 || jh * 10 < h * 9 {
        return Err(format!("the largest preview ({jw}×{jh}) is not a full-size JPEG of the {w}×{h} data"));
    }
    // half-size luminance, square-rooted so the linear raw and the JPEG weigh their tones alike
    let (rw, rh) = (w / 2, h / 2);
    let raw_y: Vec<f64> = (0..rh * rw)
        .map(|i| {
            let (x, y) = (2 * (i % rw), 2 * (i / rw));
            let s: f64 =
                [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)].iter().map(|&(x, y)| f64::from(d.get(y * w + x).copied().unwrap_or(0))).sum();
            s.sqrt()
        })
        .collect();
    let (pw, ph) = (jw / 2, jh / 2);
    let px = &jpeg.image.data;
    let jpg_y: Vec<f64> = (0..ph * pw)
        .map(|i| {
            let (x, y) = (2 * (i % pw), 2 * (i / pw));
            let s: f64 = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)]
                .iter()
                .map(|&(x, y)| px.get(y * jw + x).map_or(0.0, |p| f64::from(0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).max(0.0)))
                .sum();
            s.sqrt()
        })
        .collect();
    let (sx, sy) = ((rw - pw).min(MAX_SEARCH), (rh - ph).min(MAX_SEARCH));
    let grad = |img: &[f64], stride: usize, x: usize, y: usize| {
        let at = |x: usize, y: usize| img.get(y * stride + x).copied().unwrap_or(0.0);
        (at(x + 1, y) - at(x, y)).hypot(at(x, y + 1) - at(x, y))
    };
    let normalise = |v: Vec<f64>| {
        let n = v.len().max(1) as f64;
        let mean = v.iter().sum::<f64>() / n;
        let sd = (v.iter().map(|a| (a - mean).powi(2)).sum::<f64>() / n).sqrt().max(1e-12);
        v.into_iter().map(|a| (a - mean) / sd).collect::<Vec<f64>>()
    };
    // gradient magnitudes on every `step`-th half-size pixel, normalised, and the correlation at one offset
    let sample = |step: usize| {
        let points: Vec<(usize, usize)> =
            (0..ph.saturating_sub(1)).step_by(step).flat_map(|y| (0..pw.saturating_sub(1)).step_by(step).map(move |x| (x, y))).collect();
        let gj = normalise(points.iter().map(|&(x, y)| grad(&jpg_y, pw, x, y)).collect());
        (points, gj)
    };
    let score = |(points, gj): &(Vec<(usize, usize)>, Vec<f64>), dx: usize, dy: usize| {
        let gr = normalise(points.iter().map(|&(x, y)| grad(&raw_y, rw, x + dx, y + dy)).collect());
        gr.iter().zip(gj).map(|(a, b)| a * b).sum::<f64>() / gj.len().max(1) as f64
    };
    let fine = sample(2);
    // every offset when the window is small (every Nikon body); else every 4th on sparser points, then each offset
    // around the best of those
    let mut scores = vec![vec![f64::NEG_INFINITY; sx + 1]; sy + 1];
    let mut coarse_best = f64::NEG_INFINITY;
    let (mut cx, mut cy) = (0, 0);
    let window = if (sx + 1) * (sy + 1) <= FULL_SEARCH {
        (0..=sx, 0..=sy)
    } else {
        let coarse = sample(8);
        for dy in (0..=sy).step_by(4) {
            for dx in (0..=sx).step_by(4) {
                let v = score(&coarse, dx, dy);
                if let Some(s) = scores.get_mut(dy).and_then(|r| r.get_mut(dx)) {
                    *s = v;
                }
                if v > coarse_best {
                    (coarse_best, cx, cy) = (v, dx, dy);
                }
            }
        }
        (cx.saturating_sub(5)..=(cx + 5).min(sx), cy.saturating_sub(5)..=(cy + 5).min(sy))
    };
    for dy in window.1.clone() {
        for dx in window.0.clone() {
            if let Some(s) = scores.get_mut(dy).and_then(|r| r.get_mut(dx)) {
                *s = score(&fine, dx, dy);
            }
        }
    }
    let at = |dx: usize, dy: usize| scores.get(dy).and_then(|r| r.get(dx)).copied().unwrap_or(f64::NEG_INFINITY);
    let (mut bx, mut by) = (0, 0);
    for dy in 0..=sy {
        for dx in 0..=sx {
            if at(dx, dy) > at(bx, by) {
                (bx, by) = (dx, dy);
            }
        }
    }
    let best = at(bx, by);
    let runner_up = (0..=sy)
        .flat_map(|dy| (0..=sx).map(move |dx| (dx, dy)))
        .filter(|&(dx, dy)| dx.abs_diff(bx) > 1 || dy.abs_diff(by) > 1)
        .map(|(dx, dy)| at(dx, dy))
        .fold(f64::NEG_INFINITY, f64::max);
    // parabola through the peak and its neighbours (none at the window's edge)
    let refine = |a: f64, b: f64, c: f64| {
        let k = a - 2.0 * b + c;
        if a.is_finite() && c.is_finite() && k < 0.0 { (0.5 * (a - c) / k).clamp(-0.5, 0.5) } else { 0.0 }
    };
    let fx = if bx > 0 { refine(at(bx - 1, by), best, at(bx + 1, by)) } else { 0.0 };
    let fy = if by > 0 { refine(at(bx, by - 1), best, at(bx, by + 1)) } else { 0.0 };
    Ok(Frame {
        jpeg: (jw, jh),
        at: (2.0 * (bx as f64 + fx), 2.0 * (by as f64 + fy)),
        score: best,
        margin: if runner_up.is_finite() { best - runner_up } else { best },
    })
}
