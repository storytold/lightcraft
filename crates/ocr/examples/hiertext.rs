//! The HierText word-spotting benchmark for the PP-OCRv6 sets: how well LightCraft reads the text in photos.
//!
//! ```text
//! cargo run --release -p lightcraft-ocr --example hiertext -- \
//!     --data DIR_WITH_validation.jsonl.gz_AND_validation/ --models MODELS_DIR [--model pp-ocrv6-small] \
//!     [--limit 500 --seed 55] [--shard K/N] [--download --accept-licence]
//! ```
//!
//! HierText (Long et al., Google; <https://github.com/google-research-datasets/hiertext>) has 1,724 validation photos with
//! every word boxed and transcribed. Its annotations (`gt/validation.jsonl.gz`) are CC BY-SA 4.0 and its photos
//! (`validation.tgz`, from the Open Images bucket) are CC BY 2.0 per image; both are for evaluation here and are never
//! shipped with LightCraft. Unpack the photos to `DIR/validation/<image_id>.jpg` and put the annotations at
//! `DIR/validation.jsonl.gz`.
//!
//! The reader runs as the app would run it (PaddleOCR 3's pipeline, the shipped minimum confidence), on the CPU. The
//! ground truth is words and the reader gives lines, so each line is cut into the words between its spaces, each word's
//! box its share of the line's quad ([`words_of`]). A word counts as right when its text is the same as the annotation's
//! and its box overlaps the annotation's by IoU >= 0.5 (each annotation matched once, best overlaps first); illegible
//! annotations are left out. It prints the word precision, recall and F1, and the seconds per photo (reading only, one
//! photo at a time; decoding the JPEG is not counted). `--limit N` takes N photos spread evenly, or with `--seed S` a
//! seeded random subset (the same photos on every platform). `--shard K/N` reads only every N-th of the chosen photos,
//! starting at the K-th (K from 0), so N processes can share a run; the output carries the word counts to add up.
//!
//! Models: `--models DIR` holds one folder per set (`DIR/pp-ocrv6-small/…`). `--download --accept-licence` fetches a
//! missing set from its pinned addresses first, which also says you have read its licence (Apache-2.0).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use candle_core::Device;
use lightcraft_ocr::image::Rgb;
use lightcraft_ocr::known;
use lightcraft_ocr::reader::{Options, Reader, SHIPPED_MIN_SCORE};

/// A word box `[x0, y0, x1, y1]` with its text.
#[derive(Clone, Debug, PartialEq)]
struct Word {
    bbox: [f64; 4],
    text: String,
}

/// The words of a line read as `text` inside `quad` (corners clockwise from the top left, in pixels): each word is the
/// text between spaces, in the box its characters take of the line (an ASCII character counts 1, any other 2: CJK glyphs
/// are about twice as wide), on the line's top and bottom edges.
fn words_of(text: &str, quad: &[[f32; 2]; 4]) -> Vec<Word> {
    let width = |c: char| if c.is_ascii() { 1.0 } else { 2.0 };
    let total: f64 = text.chars().map(width).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let lerp = |a: [f32; 2], b: [f32; 2], t: f64| {
        [f64::from(a[0]) + (f64::from(b[0]) - f64::from(a[0])) * t, f64::from(a[1]) + (f64::from(b[1]) - f64::from(a[1])) * t]
    };
    let mut out = Vec::new();
    let (mut at, mut start, mut word) = (0.0f64, 0.0f64, String::new());
    let mut flush = |word: &mut String, start: f64, end: f64| {
        if word.is_empty() {
            return;
        }
        let corners = [
            lerp(quad[0], quad[1], start / total),
            lerp(quad[0], quad[1], end / total),
            lerp(quad[3], quad[2], end / total),
            lerp(quad[3], quad[2], start / total),
        ];
        let x = corners.iter().map(|p| p[0]);
        let y = corners.iter().map(|p| p[1]);
        let bbox = [
            x.clone().fold(f64::INFINITY, f64::min),
            y.clone().fold(f64::INFINITY, f64::min),
            x.fold(f64::NEG_INFINITY, f64::max),
            y.fold(f64::NEG_INFINITY, f64::max),
        ];
        out.push(Word { bbox, text: std::mem::take(word) });
    };
    for c in text.chars() {
        if c.is_whitespace() {
            flush(&mut word, start, at);
            at += 1.0;
            continue;
        }
        if word.is_empty() {
            start = at;
        }
        word.push(c);
        at += width(c);
    }
    flush(&mut word, start, at);
    out
}

fn iou(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let iw = a[2].min(b[2]) - a[0].max(b[0]);
    let ih = a[3].min(b[3]) - a[1].max(b[1]);
    if iw <= 0.0 || ih <= 0.0 {
        return 0.0;
    }
    let inter = iw * ih;
    let union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// Matched words on one photo: a predicted word is right when it overlaps an unmatched ground-truth word by IoU >= 0.5
/// and has the same text; one-to-one, best overlaps first.
fn matched(gt: &[Word], pred: &[Word]) -> usize {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (i, p) in pred.iter().enumerate() {
        for (j, g) in gt.iter().enumerate() {
            let o = iou(&p.bbox, &g.bbox);
            if o >= 0.5 && p.text == g.text {
                pairs.push((o, i, j));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (mut pu, mut gu) = (vec![false; pred.len()], vec![false; gt.len()]);
    let mut tp = 0;
    for (_, i, j) in pairs {
        if let (Some(p), Some(g)) = (pu.get_mut(i), gu.get_mut(j))
            && !*p
            && !*g
        {
            (*p, *g) = (true, true);
            tp += 1;
        }
    }
    tp
}

/// `(precision, recall, f1)` from the counts.
fn prf(tp: usize, predicted: usize, truth: usize) -> (f64, f64, f64) {
    let p = if predicted == 0 { 0.0 } else { tp as f64 / predicted as f64 };
    let r = if truth == 0 { 0.0 } else { tp as f64 / truth as f64 };
    let f = if p + r == 0.0 { 0.0 } else { 2.0 * p * r / (p + r) };
    (p, r, f)
}

/// SplitMix64: small, seedable and the same on every platform.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce5_e4b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (0 when `n` is 0).
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }
}

/// The indices of the `k` of `n` photos to read: a seeded random subset (a partial Fisher-Yates shuffle), or with no
/// seed `k` spread evenly.
fn pick(n: usize, k: usize, seed: Option<u64>) -> Vec<usize> {
    if k >= n {
        return (0..n).collect();
    }
    match seed {
        None => (0..k).map(|i| i * n / k).collect(),
        Some(seed) => {
            let mut rng = Rng(seed);
            let mut all: Vec<usize> = (0..n).collect();
            for i in 0..k {
                let j = i + rng.below(n - i);
                all.swap(i, j);
            }
            all.truncate(k);
            all.sort_unstable();
            all
        }
    }
}

/// The legible words of every photo of the validation annotations, with the photo's file.
fn load_ground_truth(dir: &Path) -> Result<Vec<(PathBuf, Vec<Word>)>, String> {
    let file = dir.join("validation.jsonl.gz");
    let f = std::fs::File::open(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut bytes = Vec::new();
    flate2::read::MultiGzDecoder::new(f).take(1 << 30).read_to_end(&mut bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut out = Vec::new();
    for a in v["annotations"].as_array().ok_or("validation.jsonl.gz: no annotations")? {
        let Some(id) = a["image_id"].as_str().filter(|id| id.chars().all(|c| c.is_ascii_alphanumeric())) else { continue };
        let mut words = Vec::new();
        for para in a["paragraphs"].as_array().into_iter().flatten() {
            for line in para["lines"].as_array().into_iter().flatten() {
                for w in line["words"].as_array().into_iter().flatten() {
                    if w["legible"] == false {
                        continue;
                    }
                    let pts: Vec<(f64, f64)> =
                        w["vertices"].as_array().into_iter().flatten().filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?))).collect();
                    if pts.is_empty() {
                        continue;
                    }
                    let (xs, ys) = (pts.iter().map(|p| p.0), pts.iter().map(|p| p.1));
                    let bbox = [
                        xs.clone().fold(f64::INFINITY, f64::min),
                        ys.clone().fold(f64::INFINITY, f64::min),
                        xs.fold(f64::NEG_INFINITY, f64::max),
                        ys.fold(f64::NEG_INFINITY, f64::max),
                    ];
                    words.push(Word { bbox, text: w["text"].as_str().unwrap_or("").to_string() });
                }
            }
        }
        out.push((dir.join("validation").join(format!("{id}.jpg")), words));
    }
    Ok(out)
}

struct Args {
    data: PathBuf,
    models: PathBuf,
    model: String,
    limit: Option<usize>,
    seed: Option<u64>,
    shard: (usize, usize),
    download: bool,
    accept: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        data: PathBuf::new(),
        models: PathBuf::new(),
        model: known::RECOMMENDED.to_string(),
        limit: None,
        seed: None,
        shard: (0, 1),
        download: false,
        accept: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--data" => a.data = PathBuf::from(value("--data")?),
            "--models" => a.models = PathBuf::from(value("--models")?),
            "--model" => a.model = value("--model")?,
            "--limit" => a.limit = Some(value("--limit")?.parse().map_err(|e| format!("--limit: {e}"))?),
            "--seed" => a.seed = Some(value("--seed")?.parse().map_err(|e| format!("--seed: {e}"))?),
            "--shard" => {
                let v = value("--shard")?;
                let parsed = v.split_once('/').and_then(|(k, n)| Some((k.parse::<usize>().ok()?, n.parse::<usize>().ok()?)));
                a.shard = parsed.filter(|&(k, n)| k < n).ok_or("--shard needs K/N with K < N")?;
            }
            "--download" => a.download = true,
            "--accept-licence" => a.accept = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.data.as_os_str().is_empty() || a.models.as_os_str().is_empty() {
        return Err("usage: hiertext --data DIR --models DIR [--model ID] [--limit N] [--seed S] [--shard K/N] [--download --accept-licence]".into());
    }
    Ok(a)
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let ids = || known::all().iter().map(|m| m.id).collect::<Vec<_>>().join(", ");
    let set = known::find(&args.model).ok_or_else(|| format!("unknown model {} (one of {})", args.model, ids()))?;
    let dir = args.models.join(set.id);
    if args.download && !set.installed_in(&dir) {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut last = 0u64;
        known::download(set, &dir, args.accept, &cancel, &mut |p| {
            if p.done / (4 << 20) != last {
                last = p.done / (4 << 20);
                eprintln!("{}: {} / {} bytes", p.file, p.done, p.total);
            }
        })
        .map_err(|e| e.to_string())?;
    }
    let reader = Reader::load(set, &dir, &Device::Cpu).map_err(|e| format!("{}: {e}", set.id))?;
    let data = load_ground_truth(&args.data)?;
    let chosen = match args.limit {
        Some(k) => pick(data.len(), k, args.seed),
        None => (0..data.len()).collect(),
    };
    let chosen: Vec<usize> = chosen.into_iter().enumerate().filter(|(n, _)| n % args.shard.1 == args.shard.0).map(|(_, i)| i).collect();
    let options = Options { min_score: SHIPPED_MIN_SCORE, ..Options::default() };
    let (mut tp, mut npred, mut ngt, mut reading) = (0, 0, 0, 0.0f64);
    for (n, &i) in chosen.iter().enumerate() {
        let Some((path, truth)) = data.get(i) else { continue };
        let photo = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.to_rgb8();
        let (w, h) = (photo.width() as usize, photo.height() as usize);
        let rgb = Rgb::new(w, h, photo.into_raw()).map_err(|e| format!("{}: {e}", path.display()))?;
        let started = Instant::now();
        let lines = reader.read(&rgb, &options).map_err(|e| format!("{}: {e}", path.display()))?;
        reading += started.elapsed().as_secs_f64();
        let pred: Vec<Word> = lines.iter().flat_map(|l| words_of(&l.text, &l.quad)).collect();
        tp += matched(truth, &pred);
        npred += pred.len();
        ngt += truth.len();
        if (n + 1) % 25 == 0 {
            eprintln!("{} / {} photos, word F1 so far {:.2}", n + 1, chosen.len(), prf(tp, npred, ngt).2 * 100.0);
        }
    }
    let (p, r, f) = prf(tp, npred, ngt);
    println!(
        "{{\"model\": \"{}\", \"device\": \"cpu\", \"photos\": {}, \"seed\": {}, \"word_precision\": {:.2}, \"word_recall\": {:.2}, \"word_f1\": {:.2}, \"seconds_per_photo\": {:.3}, \"words_matched\": {}, \"words_predicted\": {}, \"words_truth\": {}, \"seconds\": {:.1}}}",
        set.id,
        chosen.len(),
        args.seed.map_or("null".to_string(), |s| s.to_string()),
        p * 100.0,
        r * 100.0,
        f * 100.0,
        if chosen.is_empty() { 0.0 } else { reading / chosen.len() as f64 },
        tp,
        npred,
        ngt,
        reading
    );
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("hiertext: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_cut_into_words_by_the_share_of_its_width() {
        // "AB CD" (5 wide) on a 50 x 10 box at (100, 20): AB is 0-2, CD is 3-5 of it
        let quad = [[100.0, 20.0], [150.0, 20.0], [150.0, 30.0], [100.0, 30.0]];
        let w = words_of("AB CD", &quad);
        assert_eq!(w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["AB", "CD"]);
        assert_eq!(w[0].bbox, [100.0, 20.0, 120.0, 30.0]);
        assert_eq!(w[1].bbox, [130.0, 20.0, 150.0, 30.0]);
        assert!(words_of("   ", &quad).is_empty() && words_of("", &quad).is_empty());
    }

    #[test]
    fn words_match_by_text_and_overlap_once() {
        let w = |x: f64, t: &str| Word { bbox: [x, 0.0, x + 10.0, 10.0], text: t.into() };
        let gt = [w(0.0, "a"), w(20.0, "b"), w(40.0, "c")];
        // right, right text but moved off its box, right box but wrong text, and a second copy of a matched word
        let pred = [w(0.0, "a"), w(26.0, "b"), w(40.0, "x"), w(1.0, "a")];
        assert_eq!(matched(&gt, &pred), 1);
        assert_eq!(prf(1, 4, 3), (0.25, 1.0 / 3.0, 2.0 * 0.25 * (1.0 / 3.0) / (0.25 + 1.0 / 3.0)));
        assert_eq!(prf(0, 0, 0), (0.0, 0.0, 0.0));
    }

    #[test]
    fn a_seeded_subset_is_stable_and_sorted() {
        assert_eq!(pick(10, 20, Some(1)), (0..10).collect::<Vec<_>>());
        assert_eq!(pick(100, 5, None), [0, 20, 40, 60, 80]);
        let a = pick(1724, 500, Some(55));
        assert_eq!((a.len(), a == pick(1724, 500, Some(55)), a.windows(2).all(|w| w[0] < w[1])), (500, true, true));
        assert_ne!(a, pick(1724, 500, Some(56)));
    }
}
