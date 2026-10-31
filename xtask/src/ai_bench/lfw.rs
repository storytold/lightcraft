//! LFW face verification ("view 2"): 6,000 pairs of photos in ten folds, half of them the same person. Every photo goes
//! through the code Photo ▸ Detect/Recognise Faces uses (the YuNet detector, alignment to the ArcFace template from its
//! landmarks, the installed recognition model) and a pair scores the cosine of its two embeddings.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use lightcraft_faces::align::{FaceCrop, Rgb, align_to_template, crop_box};
use lightcraft_faces::matching::cosine;
use lightcraft_faces::runtime::Embedder;
use lightcraft_faces::yunet::{Detector, Options};

use super::metrics;
use super::wider::load_rgb;

/// How much wider than the guessed face box the fallback crop is (what the app does for a box without landmarks).
const BOX_PAD: f32 = 1.5;
/// Half the side of the fallback box, as a fraction of the shorter side of the photo (LFW centres the face).
const FALLBACK_HALF: f32 = 0.22;

/// Largest `pairs.txt` accepted (the real file is under 200 KB).
const MAX_PAIRS_BYTES: u64 = 1 << 20;

/// Refuse a `limit` that leaves some fold without a same-person and a different-person pair to evaluate and train on.
fn check_limit(limit: Option<usize>, pairs: &[Pair]) -> Result<(), String> {
    let folds = pairs.iter().map(|p| p.fold).max().map_or(0, |last| last.saturating_add(1));
    let minimum = folds.saturating_mul(2);
    match limit {
        Some(k) if k < minimum => Err(format!(
            "--limit {k} is too small to evaluate {folds} folds: use at least {minimum} pairs (a same-person and a different-person pair per fold)"
        )),
        _ => Ok(()),
    }
}

/// One line of `pairs.txt`.
#[derive(Debug, PartialEq)]
pub struct Pair {
    pub a: PathBuf,
    pub b: PathBuf,
    pub same: bool,
    pub fold: usize,
}

/// Read `dir/pairs.txt` (LFW's view 2) and check that every photo it names is under `dir/lfw`. The file starts with
/// `<folds> <pairs per half fold>`; each fold then lists that many same-person lines (`name n m`) followed by as
/// many different-person lines (`name n other m`).
pub fn load_pairs(dir: &Path) -> Result<Vec<Pair>, String> {
    let list = dir.join("pairs.txt");
    let unreadable = |e: std::io::Error| format!("{}: {e} (run `cargo xtask ai-bench fetch --set lfw`)", list.display());
    let file = std::fs::File::open(&list).map_err(unreadable)?;
    let size = file.metadata().map_err(unreadable)?.len();
    if size > MAX_PAIRS_BYTES {
        return Err(format!("{}: is {size} bytes, more than the {MAX_PAIRS_BYTES} a pairs file can be", list.display()));
    }
    // read one byte past the cap too, in case the file grows while it is read
    let mut text = String::new();
    file.take(MAX_PAIRS_BYTES + 1).read_to_string(&mut text).map_err(unreadable)?;
    if text.len() as u64 > MAX_PAIRS_BYTES {
        return Err(format!("{}: grew past {MAX_PAIRS_BYTES} bytes while being read", list.display()));
    }
    let photos = dir.join("lfw");
    let photo = |name: &str, number: &str| -> Result<PathBuf, String> {
        let number: usize = number.parse().map_err(|_| format!("{}: `{number}` is not a photo number", list.display()))?;
        if name.is_empty() || name.contains(['/', '\\', ':']) || name.contains("..") {
            return Err(format!("{}: `{name}` is not a person's folder name", list.display()));
        }
        Ok(photos.join(name).join(format!("{name}_{number:04}.jpg")))
    };
    let mut lines = text.lines();
    let header: Vec<usize> = lines.next().unwrap_or("").split_whitespace().map_while(|t| t.parse().ok()).collect();
    let [folds, half] = header.as_slice() else {
        return Err(format!("{}: the first line must be `<folds> <pairs per half fold>`", list.display()));
    };
    // the header is data from the file: a damaged one must not overflow
    let too_big = || format!("{}: the header `{folds} {half}` is too large", list.display());
    let per_fold = half.checked_mul(2).filter(|n| *n > 0).ok_or_else(too_big)?;
    let expected = folds.checked_mul(per_fold).ok_or_else(too_big)?;
    let mut pairs = Vec::new();
    for (i, line) in lines.filter(|l| !l.trim().is_empty()).enumerate() {
        let fold = i / per_fold;
        let words: Vec<&str> = line.split_whitespace().collect();
        let (a, b, same) = match words.as_slice() {
            [name, n, m] => (photo(name, n)?, photo(name, m)?, true),
            [name, n, other, m] => (photo(name, n)?, photo(other, m)?, false),
            _ => return Err(format!("{}: pair {} is malformed: `{line}`", list.display(), i + 1)),
        };
        if same != (i % per_fold < *half) {
            let (place, kind) = if same { ("different-person", "three fields") } else { ("same-person", "four fields") };
            return Err(format!("{}: pair {} is in the {place} half of fold {} but has {kind}: `{line}`", list.display(), i + 1, fold + 1));
        }
        pairs.push(Pair { a, b, same, fold });
    }
    if pairs.len() != expected {
        return Err(format!("{}: lists {} pairs, its header says {expected}", list.display(), pairs.len()));
    }
    if let Some(missing) = pairs.iter().flat_map(|p| [&p.a, &p.b]).find(|p| !p.is_file()) {
        return Err(format!("{}: no such photo (run `cargo xtask ai-bench fetch --set lfw`)", missing.display()));
    }
    Ok(pairs)
}

/// Run `work(0..n)` on `threads` threads and give the results in order.
fn parallel<T: Send>(n: usize, threads: usize, work: impl Fn(usize) -> T + Sync) -> Result<Vec<T>, String> {
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let started = Instant::now();
    let step = (n / 20).max(1);
    let joined = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.clamp(1, 64))
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        if at >= n {
                            break;
                        }
                        done.push((at, work(at)));
                        let count = finished.fetch_add(1, Ordering::Relaxed) + 1;
                        if count.is_multiple_of(step) {
                            eprintln!("  {count}/{n} ({:.1}/s)", count as f64 / started.elapsed().as_secs_f64().max(1e-9));
                        }
                    }
                    done
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().map_err(|_| "a worker stopped unexpectedly".to_string())).collect::<Result<Vec<_>, _>>()
    })?;
    let mut all: Vec<(usize, T)> = joined.into_iter().flatten().collect();
    all.sort_by_key(|(at, _)| *at);
    Ok(all.into_iter().map(|(_, value)| value).collect())
}

/// The face of one photo as the recogniser wants it, and whether the detector found none, or its landmarks could not be aligned (so the centre was cut out).
/// LFW photos are centred on the person, so with several faces the one nearest the centre is the one.
fn face(detector: &Detector, path: &Path, size: (usize, usize)) -> Result<(Vec<u8>, bool), String> {
    let (rgb, w, h) = load_rgb(path)?;
    let faces = detector.detect(&rgb, w, h, &Options::default()).map_err(|e| format!("{}: {e}", path.display()))?;
    let picture = Rgb { data: &rgb, width: w, height: h };
    let (fw, fh) = (w as f32, h as f32);
    let distance = |f: &lightcraft_faces::yunet::Face| ((f.x0 + f.x1) / 2.0 - 0.5).hypot((f.y0 + f.y1) / 2.0 - 0.5);
    let aligned = faces
        .iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .and_then(|f| align_to_template(&picture, &f.landmarks.map(|(x, y)| (x * fw, y * fh)), size.0, size.1));
    if let Some(crop) = aligned {
        return Ok((crop, false));
    }
    let half = fw.min(fh) * FALLBACK_HALF;
    let bounds = (fw / 2.0 - half, fh / 2.0 - half, fw / 2.0 + half, fh / 2.0 + half);
    let crop = crop_box(&picture, FaceCrop { bounds, padding: BOX_PAD, output_size: size })
        .ok_or_else(|| format!("{}: no face could be cut out", path.display()))?;
    Ok((crop, true))
}

/// A pair's score: the cosine of its two photos' embeddings; a pair with a photo that could not be embedded (or whose
/// embeddings cannot be compared) has none, so it is counted as wrong instead of being given a made-up score.
fn scores(pairs: &[Pair], embeddings: &HashMap<&Path, Vec<f32>>) -> Vec<(Option<f64>, bool, usize)> {
    pairs
        .iter()
        .map(|p| {
            let score = match (embeddings.get(p.a.as_path()), embeddings.get(p.b.as_path())) {
                (Some(a), Some(b)) => cosine(a, b).map(f64::from),
                _ => None,
            };
            (score, p.same, p.fold)
        })
        .collect()
}

pub struct Run {
    pub pairs: usize,
    pub photos: usize,
    /// Photos where the detector found no face, or its landmarks could not be aligned, and the centre was cut out.
    pub fallbacks: usize,
    /// Photos that could not be read or embedded.
    pub failed: usize,
    /// Pairs with such a photo: they have no score, take no part in choosing a cut-off and count as wrong.
    pub failed_pairs: usize,
    pub seconds: f64,
    /// Mean and standard deviation of the ten folds' accuracy, in percent.
    pub accuracy: (f64, f64),
}

/// Verify `embedder` on the pairs under `dir` (or `limit` of them, spread evenly) on `threads` threads.
pub fn run(dir: &Path, detector: &Detector, embedder: &Embedder, limit: Option<usize>, threads: usize) -> Result<Run, String> {
    let mut pairs = load_pairs(dir)?;
    check_limit(limit, &pairs)?;
    if let Some(k) = limit.filter(|k| *k > 0 && *k < pairs.len()) {
        let n = pairs.len();
        let keep: Vec<usize> = (0..k).map(|i| i * n / k).collect();
        pairs = pairs.into_iter().enumerate().filter(|(i, _)| keep.contains(i)).map(|(_, p)| p).collect();
    }
    let mut photos: Vec<&Path> = pairs.iter().flat_map(|p| [p.a.as_path(), p.b.as_path()]).collect();
    photos.sort();
    photos.dedup();
    let started = Instant::now();
    let size = embedder.input_size();
    let found = parallel(photos.len(), threads, |i| {
        let path = photos.get(i).ok_or("no such photo")?;
        let (crop, fallback) = face(detector, path, size)?;
        let embedding = embedder.embed(&crop).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok::<_, String>((embedding, fallback))
    })?;
    let (mut embeddings, mut fallbacks, mut failed) = (HashMap::new(), 0, 0);
    for (path, result) in photos.iter().zip(found) {
        match result {
            Ok((embedding, fallback)) => {
                fallbacks += usize::from(fallback);
                embeddings.insert(*path, embedding);
            }
            Err(e) => {
                if failed < 5 {
                    eprintln!("  warning: {e}");
                }
                failed += 1;
            }
        }
    }
    let scored = scores(&pairs, &embeddings);
    let failed_pairs = scored.iter().filter(|p| p.0.is_none()).count();
    let (mean, std) = metrics::fold_accuracy(&scored);
    Ok(Run {
        pairs: pairs.len(),
        failed_pairs,
        photos: photos.len(),
        fallbacks,
        failed,
        seconds: started.elapsed().as_secs_f64(),
        accuracy: (mean * 100.0, std * 100.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that is removed afterwards.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("ai-bench-lfw-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            assert!(std::fs::create_dir_all(&dir).is_ok());
            Scratch(dir)
        }

        fn pairs(&self, text: &str) {
            assert!(std::fs::write(self.0.join("pairs.txt"), text).is_ok());
        }

        fn photo(&self, name: &str, number: usize) {
            let folder = self.0.join("lfw").join(name);
            assert!(std::fs::create_dir_all(&folder).is_ok());
            assert!(std::fs::write(folder.join(format!("{name}_{number:04}.jpg")), b"x").is_ok());
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Two folds of one same-person and one different-person pair each.
    const TWO_FOLDS: &str = "2 1\nAnn\t1\t2\nAnn\t1\tBob\t1\nBob\t1\t2\nBob\t2\tAnn\t2\n";

    fn two_folds() -> Scratch {
        let s = Scratch::new(&format!("two-{:?}", std::thread::current().id()).replace(['(', ')'], ""));
        for (name, n) in [("Ann", 1), ("Ann", 2), ("Bob", 1), ("Bob", 2)] {
            s.photo(name, n);
        }
        s.pairs(TWO_FOLDS);
        s
    }

    #[test]
    fn pairs_read_into_folds_of_same_then_different() {
        let s = two_folds();
        let pairs = load_pairs(&s.0).unwrap();
        let lfw = s.0.join("lfw");
        assert_eq!(pairs.len(), 4);
        assert_eq!(pairs[0], Pair { a: lfw.join("Ann/Ann_0001.jpg"), b: lfw.join("Ann/Ann_0002.jpg"), same: true, fold: 0 });
        assert_eq!(pairs[1], Pair { a: lfw.join("Ann/Ann_0001.jpg"), b: lfw.join("Bob/Bob_0001.jpg"), same: false, fold: 0 });
        assert_eq!((pairs[2].same, pairs[2].fold, pairs[3].same, pairs[3].fold), (true, 1, false, 1));
    }

    #[test]
    fn a_pairs_file_over_the_size_cap_is_refused_with_its_size() {
        let s = Scratch::new("big");
        let size = (MAX_PAIRS_BYTES + 1) as usize;
        s.pairs(&" ".repeat(size));
        let e = load_pairs(&s.0).err().unwrap_or_default();
        assert!(e.contains(&size.to_string()) && e.contains(&MAX_PAIRS_BYTES.to_string()), "{e:?}");
    }

    #[test]
    fn a_pair_of_the_wrong_kind_for_its_place_in_the_fold_is_an_error() {
        let s = two_folds();
        for (text, why) in [
            ("2 1\nAnn 1 Bob 1\nAnn 1 2\nBob 1 2\nBob 2 Ann 2\n", "different-person pair in the same-person half"),
            ("2 1\nAnn 1 2\nAnn 1 2\nBob 1 2\nBob 2 Ann 2\n", "same-person pair in the different-person half"),
            ("2 1\nAnn 1 2\nAnn 1 Bob 1\nBob 2 Ann 2\nBob 1 2\n", "halves swapped in the second fold"),
        ] {
            s.pairs(text);
            let e = load_pairs(&s.0).err().unwrap_or_default();
            assert!(e.contains("pair "), "{why}: {e:?}");
        }
    }

    #[test]
    fn a_limit_too_small_for_ten_fold_evaluation_is_refused_and_states_the_minimum() {
        let s = Scratch::new("limit");
        let pair = |fold| Pair { a: s.0.join("a"), b: s.0.join("b"), same: true, fold };
        let pairs: Vec<Pair> = (0..10).map(pair).collect();
        for k in [1, 5, 19] {
            let e = check_limit(Some(k), &pairs).err().unwrap_or_default();
            assert!(e.contains("20") && e.contains("10 folds"), "{k}: {e:?}");
        }
        assert!(check_limit(Some(20), &pairs).is_ok() && check_limit(None, &pairs).is_ok());
    }

    #[test]
    fn a_missing_pairs_file_or_photo_is_an_error_that_says_how_to_fetch_it() {
        let s = Scratch::new("missing");
        assert!(load_pairs(&s.0).err().unwrap().contains("ai-bench fetch"));
        s.pairs(TWO_FOLDS);
        s.photo("Ann", 1);
        let e = load_pairs(&s.0).err().unwrap();
        assert!(e.contains("Ann_0002.jpg") && e.contains("ai-bench fetch"), "{e}");
    }

    #[test]
    fn malformed_pairs_files_are_errors_not_panics() {
        let s = two_folds();
        for (text, why) in [
            ("", "empty"),
            ("garbage\n", "no header"),
            ("2\nAnn 1 2\n", "one-number header"),
            ("18446744073709551615 2\nAnn 1 2\n", "a header that overflows the pair count"),
            ("2 9223372036854775807\nAnn 1 2\n", "a header that overflows the fold size"),
            ("0 0\n", "empty folds"),
            ("2 1\nAnn 1 2\nAnn 1 Bob 1\nBob 1 2\n", "a pair short"),
            ("2 1\nAnn 1 2\nAnn 1 Bob 1\nBob 1 2\nBob 2 Ann 2\nAnn 1 2\n", "a pair too many"),
            ("2 1\nAnn 1\nAnn 1 Bob 1\nBob 1 2\nBob 2 Ann 2\n", "a line of two words"),
            ("2 1\nAnn x 2\nAnn 1 Bob 1\nBob 1 2\nBob 2 Ann 2\n", "a photo number that is not a number"),
            ("2 1\nAnn -1 2\nAnn 1 Bob 1\nBob 1 2\nBob 2 Ann 2\n", "a negative photo number"),
            ("2 1\n../Ann 1 2\nAnn 1 Bob 1\nBob 1 2\nBob 2 Ann 2\n", "a name that climbs out"),
            ("2 1\nC:Ann 1 2\nAnn 1 Bob 1\nBob 1 2\nBob 2 Ann 2\n", "a drive in the name"),
        ] {
            s.pairs(text);
            assert!(load_pairs(&s.0).is_err(), "{why}");
        }
        // a pairs file that is not text
        assert!(std::fs::write(s.0.join("pairs.txt"), [0xff, 0xfe, 0x00]).is_ok());
        assert!(load_pairs(&s.0).is_err());
    }

    #[test]
    fn a_pair_scores_the_cosine_of_its_embeddings_and_a_missing_one_scores_lowest() {
        let s = two_folds();
        let pairs = load_pairs(&s.0).unwrap();
        let lfw = s.0.join("lfw");
        let path = |n: &str| lfw.join(n.split('_').next().unwrap_or("")).join(format!("{n}.jpg"));
        let (a1, a2, b1) = (path("Ann_0001"), path("Ann_0002"), path("Bob_0001"));
        let mut embeddings: HashMap<&Path, Vec<f32>> = HashMap::new();
        embeddings.insert(a1.as_path(), vec![1.0, 0.0]);
        embeddings.insert(a2.as_path(), vec![0.6, 0.8]);
        embeddings.insert(b1.as_path(), vec![0.0, 1.0]);
        let scored = scores(&pairs, &embeddings);
        assert_eq!(scored.len(), 4);
        assert!((scored[0].0.unwrap_or(f64::NAN) - 0.6).abs() < 1e-6 && scored[0].1 && scored[0].2 == 0);
        assert!(scored[1].0.unwrap_or(f64::NAN).abs() < 1e-6 && !scored[1].1);
        // Bob_0002 has no embedding: no score at all, not a low one
        assert_eq!((scored[2].0, scored[3].0), (None, None));
    }
}
