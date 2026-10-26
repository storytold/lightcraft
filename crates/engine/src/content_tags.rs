//! Auto-tags, the data side: the tags' embeddings (cached per model), scoring every indexed photo against every tag,
//! and the suggestions the user dismissed.
//!
//! - **The vocabulary** ([`lightcraft_vision::vocab`]) is the built-in English list or the user's own
//!   (`vocabulary.txt` in the content models folder: it belongs to the user, not to a library).
//! - **Its embeddings** depend only on the model and the vocabulary, not on the library, so they are cached once per
//!   model in the content models folder: `content-vocab-<model id>.bin`, records keyed by each tag's phrases, so an
//!   edited vocabulary only embeds the tags that changed. A file made by another model (or another version of it, or
//!   other prompt templates) or a damaged one is ignored and rebuilt.
//! - **Tags are computed, not stored**: every indexed photo's embedding against every tag's (a matrix product,
//!   [`tag_rows`]); the best [`TOP_K`] scores of each photo above [`FLOOR`] are kept in memory, so a cut-off moved by the
//!   user applies at once. A photo's tags are those at or above the cut-off, at most [`MAX_SHOWN`], and every level above
//!   them is implied (`animal|bird|owl` → `animal|bird`, `animal`).
//! - **Dismissed suggestions** are remembered per library in `content-dismissed.bin` beside the catalog: `"LCCD"`,
//!   version 1, then records of `photo u64` and the tag's path hash `u64` ([`vocab::path_hash`]). A damaged file is
//!   ignored (and rewritten with the next dismissal).
//!
//! None of it is in the catalog: an accepted tag becomes an ordinary keyword (`content.acceptTags`).

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use half::f16;
use half::slice::HalfFloatSliceExt;
use lightcraft_vision::Scoring;
use lightcraft_vision::vocab::{self, Vocabulary};
use rayon::prelude::*;

use crate::content_index::Hash;

/// Scores kept per photo (before the cut-off).
pub(crate) const TOP_K: usize = 16;
/// Scores below this are never kept (far below any useful cut-off).
pub(crate) const FLOOR: f32 = 1e-4;
/// Most tags a photo shows.
pub(crate) const MAX_SHOWN: usize = 8;
/// The user's vocabulary, in the content models folder.
pub(crate) const VOCAB_FILE: &str = "vocabulary.txt";
/// Photos scored in one matrix product.
const CHUNK_ROWS: usize = 2048;

/// A photo's best tags: (tag index in the vocabulary, score), best first.
pub(crate) type Scores = Vec<(u32, f32)>;

/// The embeddings of a vocabulary's tags (`terms × dim`, each normalised), for one model.
pub(crate) struct Matrix {
    /// `<model id>@<sha256>`.
    pub tag: String,
    /// [`Vocabulary::hash`] of the vocabulary it was made for.
    pub vocab: u64,
    pub dim: usize,
    pub data: Vec<f32>,
    pub scoring: Scoring,
}

impl Matrix {
    pub fn terms(&self) -> usize {
        self.data.len() / self.dim.max(1)
    }
}

/// Score `n` photos (`rows`: `n × dim` f16, normalised) against every tag of `m` and keep each photo's best [`TOP_K`]
/// at or above `floor`. Runs the matrix products a chunk of photos at a time (memory stays at a few tens of MB) and
/// the selection in parallel.
pub(crate) fn tag_rows(rows: &[f16], m: &Matrix, floor: f32) -> Vec<Scores> {
    let dim = m.dim;
    let terms = m.terms();
    if dim == 0 || terms == 0 || !rows.len().is_multiple_of(dim) {
        return Vec::new();
    }
    let cos_floor = m.scoring.cosine_for(floor);
    let mut out = Vec::with_capacity(rows.len() / dim);
    for chunk in rows.chunks(CHUNK_ROWS * dim) {
        let n = chunk.len() / dim;
        let mut a = vec![0.0f32; chunk.len()];
        chunk.convert_to_f32_slice(&mut a);
        let sims = lightcraft_vision::similarities(&a, n, &m.data, terms, dim);
        if sims.len() != n * terms {
            out.extend((0..n).map(|_| Vec::new()));
            continue;
        }
        let picked: Vec<Scores> = sims
            .par_chunks(terms)
            .map(|row| {
                let mut best: Vec<(u32, f32)> =
                    row.iter().enumerate().filter(|(_, c)| c.is_finite() && **c >= cos_floor).map(|(j, c)| (j as u32, *c)).collect();
                if best.len() > TOP_K {
                    best.select_nth_unstable_by(TOP_K - 1, |a, b| b.1.total_cmp(&a.1));
                    best.truncate(TOP_K);
                }
                best.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                best.into_iter().map(|(j, c)| (j, m.scoring.score(c))).filter(|(_, s)| s.is_finite() && *s >= floor).collect()
            })
            .collect();
        out.extend(picked);
    }
    out
}

// ---------------------------------------------------------------- the vocabulary's embeddings, cached per model

const VOCAB_MAGIC: &[u8; 4] = b"LCCV";
const VOCAB_VERSION: u8 = 1;
/// Most records and longest key a vocabulary cache holds, and its largest size.
const MAX_CACHE_RECORDS: usize = 2 * vocab::MAX_TERMS;
const MAX_KEY: usize = 4096;
const MAX_CACHE_BYTES: u64 = 64 << 20;

/// The cache of model `id`'s tag embeddings in the content models folder `dir`.
pub(crate) fn vocab_cache_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("content-vocab-{id}.bin"))
}

/// A digest of the prompt templates (embeddings made with other templates are not reused).
fn templates_hash() -> u64 {
    let mut h = vocab::Fnv::new();
    for t in vocab::TEMPLATES {
        h.write(t.as_bytes());
        h.write(&[0]);
    }
    h.finish()
}

/// What a vocabulary cache holds: how the model scores, and embeddings by tag key ([`vocab::Term::key`]).
#[derive(Default)]
pub(crate) struct VocabCache {
    pub scoring: Option<Scoring>,
    pub vectors: HashMap<String, Vec<f32>>,
    /// Why the file was not used, when it exists and was not.
    pub note: Option<String>,
}

/// Read model `tag`'s cache (`dim` numbers per tag); a missing, foreign or damaged file gives an empty one.
pub(crate) fn read_vocab_cache(path: &Path, tag: &str, dim: usize) -> VocabCache {
    let mut c = VocabCache::default();
    let Ok(meta) = std::fs::metadata(path) else { return c };
    if meta.len() > MAX_CACHE_BYTES {
        c.note = Some("the tag cache is too large: it is rebuilt".into());
        return c;
    }
    let mut bytes = Vec::new();
    if std::fs::File::open(path).and_then(|f| f.take(MAX_CACHE_BYTES).read_to_end(&mut bytes)).is_err() {
        c.note = Some("the tag cache cannot be read: it is rebuilt".into());
        return c;
    }
    match parse_vocab_cache(&bytes, tag, dim) {
        Ok(v) => v,
        Err(why) => {
            c.note = Some(format!("the tag cache {why}: it is rebuilt"));
            c
        }
    }
}

/// Reads little-endian values from a byte slice, never past its end.
struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

fn parse_vocab_cache(b: &[u8], tag: &str, dim: usize) -> Result<VocabCache, &'static str> {
    let mut r = Cursor { b, at: 0 };
    if r.take(4) != Some(VOCAB_MAGIC.as_slice()) {
        return Err("is not a LightCraft tag cache");
    }
    if r.u8() != Some(VOCAB_VERSION) {
        return Err("is of another version");
    }
    let tag_len = usize::from(r.u16().ok_or("is damaged")?);
    let file_tag = r.take(tag_len).ok_or("is damaged")?;
    let templates = r.u64().ok_or("is damaged")?;
    let file_dim = r.u32().ok_or("is damaged")? as usize;
    if file_tag != tag.as_bytes() || file_dim != dim || templates != templates_hash() {
        return Err("was made by another model or for other prompts");
    }
    let kind = r.u8().ok_or("is damaged")?;
    let (scale, bias) = (r.f32().ok_or("is damaged")?, r.f32().ok_or("is damaged")?);
    let scoring = match kind {
        0 => None,
        1 | 2 if scale.is_finite() && bias.is_finite() => Some(Scoring { scale, bias, sigmoid: kind == 2 }),
        _ => return Err("is damaged"),
    };
    let count = r.u32().ok_or("is damaged")? as usize;
    if count > MAX_CACHE_RECORDS {
        return Err("holds too many tags");
    }
    let mut vectors = HashMap::with_capacity(count.min(1024));
    let mut row = vec![0.0f32; dim];
    for _ in 0..count {
        let len = usize::from(r.u16().ok_or("is damaged")?);
        if len > MAX_KEY {
            return Err("is damaged");
        }
        let key = std::str::from_utf8(r.take(len).ok_or("is damaged")?).map_err(|_| "is damaged")?.to_string();
        let values = r.take(dim * 2).ok_or("is damaged")?;
        for (dst, src) in row.iter_mut().zip(values.as_chunks::<2>().0) {
            *dst = f16::from_le_bytes(*src).to_f32();
        }
        if row.iter().all(|v| v.is_finite()) {
            vectors.insert(key, row.clone());
        }
    }
    Ok(VocabCache { scoring, vectors, note: None })
}

/// Write model `tag`'s cache (atomically: `.part` + rename).
pub(crate) fn write_vocab_cache<'a>(
    path: &Path,
    tag: &str,
    dim: usize,
    scoring: Option<Scoring>,
    entries: impl Iterator<Item = (&'a str, &'a [f32])>,
) -> std::io::Result<()> {
    let mut out = Vec::new();
    out.extend_from_slice(VOCAB_MAGIC);
    out.push(VOCAB_VERSION);
    let tag = tag.as_bytes().get(..tag.len().min(usize::from(u16::MAX))).unwrap_or_default();
    out.extend_from_slice(&(tag.len() as u16).to_le_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(&templates_hash().to_le_bytes());
    out.extend_from_slice(&u32::try_from(dim).unwrap_or(0).to_le_bytes());
    let s = scoring.unwrap_or(Scoring::COSINE);
    out.push(match scoring {
        None => 0,
        Some(s) if s.sigmoid => 2,
        Some(_) => 1,
    });
    out.extend_from_slice(&s.scale.to_le_bytes());
    out.extend_from_slice(&s.bias.to_le_bytes());
    let count_at = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    let mut count = 0u32;
    for (key, v) in entries.take(MAX_CACHE_RECORDS) {
        if key.len() > MAX_KEY || v.len() != dim {
            continue;
        }
        out.extend_from_slice(&(key.len() as u16).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        for x in v {
            out.extend_from_slice(&f16::from_f32(*x).to_le_bytes());
        }
        count += 1;
    }
    if let Some(slot) = out.get_mut(count_at..count_at + 4) {
        slot.copy_from_slice(&count.to_le_bytes());
    }
    let part = path.with_extension("bin.part");
    std::fs::write(&part, &out)?;
    std::fs::rename(&part, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

// ---------------------------------------------------------------- dismissed suggestions, per library

const DISMISSED_MAGIC: &[u8; 4] = b"LCCD";
const DISMISSED_VERSION: u8 = 1;
/// Most dismissals remembered (16 bytes each).
const MAX_DISMISSED: usize = 4_000_000;

/// `content-dismissed.bin` in the library folder `dir`.
pub(crate) fn dismissed_path(dir: &Path) -> PathBuf {
    dir.join("content-dismissed.bin")
}

/// The suggestions dismissed in one library: (photo, [`vocab::path_hash`] of the tag).
#[derive(Default)]
pub(crate) struct Dismissed {
    pub set: HashSet<(u64, u64)>,
    pub path: Option<PathBuf>,
    pub note: Option<String>,
    /// Bumped by every change.
    pub generation: u64,
}

impl Dismissed {
    /// The dismissals kept at `path` (`None`: in memory only); a missing or damaged file is none.
    pub fn open(path: Option<PathBuf>) -> Dismissed {
        let mut d = Dismissed { path: path.clone(), ..Default::default() };
        let Some(p) = path else { return d };
        let Ok(meta) = std::fs::metadata(&p) else { return d };
        if meta.len() > 5 + (MAX_DISMISSED as u64) * 16 {
            d.note = Some("the dismissed-tags file is too large: it is ignored".into());
            return d;
        }
        let mut b = Vec::new();
        if std::fs::File::open(&p).and_then(|f| f.take(meta.len()).read_to_end(&mut b)).is_err() {
            d.note = Some("the dismissed-tags file cannot be read: it is ignored".into());
            return d;
        }
        if b.get(..4) != Some(DISMISSED_MAGIC.as_slice()) || b.get(4) != Some(&DISMISSED_VERSION) {
            d.note = Some("the dismissed-tags file is damaged: it is ignored".into());
            return d;
        }
        let body = b.get(5..).unwrap_or_default();
        for rec in body.as_chunks::<16>().0 {
            let (photo, tag) = rec.split_at(8);
            d.set.insert((u64::from_le_bytes(photo.try_into().unwrap_or([0; 8])), u64::from_le_bytes(tag.try_into().unwrap_or([0; 8]))));
        }
        if body.len() % 16 != 0 {
            d.note = Some("the dismissed-tags file ended in a torn record (it is ignored)".into());
        }
        d
    }

    #[cfg(test)]
    pub fn contains(&self, photo: u64, path: &str) -> bool {
        self.set.contains(&(photo, vocab::path_hash(path)))
    }

    /// Remember these dismissals and write the file. Returns how many were new.
    pub fn add(&mut self, items: impl Iterator<Item = (u64, u64)>) -> Result<usize, String> {
        let mut n = 0;
        for it in items {
            if self.set.len() >= MAX_DISMISSED {
                break;
            }
            if self.set.insert(it) {
                n += 1;
            }
        }
        if n > 0 {
            self.generation = self.generation.wrapping_add(1);
            self.save()?;
        }
        Ok(n)
    }

    fn save(&mut self) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        let mut out = Vec::with_capacity(5 + self.set.len() * 16);
        out.extend_from_slice(DISMISSED_MAGIC);
        out.push(DISMISSED_VERSION);
        let mut all: Vec<&(u64, u64)> = self.set.iter().collect();
        all.sort();
        for (p, t) in all {
            out.extend_from_slice(&p.to_le_bytes());
            out.extend_from_slice(&t.to_le_bytes());
        }
        let part = path.with_extension("bin.part");
        std::fs::write(&part, &out).and_then(|()| std::fs::rename(&part, path)).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            format!("could not save the dismissed tags: {e}")
        })?;
        self.note = None;
        Ok(())
    }
}

// ---------------------------------------------------------------- the tags of the library, with the cut-off applied

/// The library's tags as shown: each photo's tags at or above the cut-off, and the photos under each tag (implied
/// levels included). Made from the scores when the scores, the cut-off or the dismissals change.
#[derive(Default)]
pub(crate) struct View {
    /// Photo → its tags (vocabulary index, score), best first.
    pub photo: HashMap<u64, Scores>,
    /// Tag path in lower case → the photos under it (its own and its levels below), and the path as written.
    pub under: HashMap<String, (String, HashSet<u64>)>,
}

impl View {
    /// Apply the cut-off: `cutoff(term)` is a tag's own (`base` unless it has one), `dismissed(photo, term)` hides
    /// one. A specific tag that reaches `base` but not its own, higher cut-off (a bird species, a dog breed) counts
    /// for the nearest tag above it that its score reaches (`animal|mammal|dog|spaniel` → `animal|mammal|dog`): the
    /// model is often surer of the specific word, and the general one is still right.
    pub fn build(
        scores: &HashMap<u64, (Hash, Scores)>,
        vocab: &Vocabulary,
        base: f32,
        cutoff: impl Fn(usize) -> f32,
        dismissed: impl Fn(u64, usize) -> bool,
    ) -> View {
        let index: HashMap<String, usize> = vocab.terms.iter().enumerate().map(|(i, t)| (t.path.to_lowercase(), i)).collect();
        // each tag's tags above it, nearest first
        let above: Vec<Vec<usize>> =
            vocab.terms.iter().map(|t| vocab::ancestors(&t.path).filter_map(|a| index.get(&a.to_lowercase()).copied()).collect()).collect();
        let mut v = View::default();
        for (photo, (_, s)) in scores {
            let mut shown: Scores = Vec::new();
            for (j, sc) in s {
                let j = *j as usize;
                let chosen = if *sc >= cutoff(j) {
                    Some(j)
                } else if *sc >= base {
                    above.get(j).and_then(|a| a.iter().copied().find(|a| *sc >= cutoff(*a)))
                } else {
                    None
                };
                let Some(c) = chosen else { continue };
                if dismissed(*photo, c) || shown.iter().any(|(x, _)| *x as usize == c) {
                    continue;
                }
                shown.push((c as u32, *sc));
                if shown.len() >= MAX_SHOWN {
                    break;
                }
            }
            if shown.is_empty() {
                continue;
            }
            for (j, _) in &shown {
                let Some(t) = vocab.terms.get(*j as usize) else { continue };
                for p in std::iter::once(t.path.as_str()).chain(vocab::ancestors(&t.path)) {
                    v.under.entry(p.to_lowercase()).or_insert_with(|| (p.to_string(), HashSet::new())).1.insert(*photo);
                }
            }
            v.photo.insert(*photo, shown);
        }
        v
    }

    /// Whether `photo` has tag `path` or one below it.
    pub fn has(&self, photo: u64, path: &str) -> bool {
        self.under.get(&path.trim().to_lowercase()).is_some_and(|(_, s)| s.contains(&photo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let mut v = v.to_vec();
        lightcraft_vision::normalize(&mut v);
        v
    }

    fn matrix(terms: &[&[f32]]) -> Matrix {
        let dim = terms[0].len();
        Matrix { tag: "m".into(), vocab: 1, dim, data: terms.iter().flat_map(|t| unit(t)).collect(), scoring: Scoring::COSINE }
    }

    fn rows(r: &[&[f32]]) -> Vec<f16> {
        r.iter().flat_map(|v| unit(v)).map(f16::from_f32).collect()
    }

    #[test]
    fn each_photo_keeps_its_best_tags_above_the_floor() {
        let m = matrix(&[&[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0], &[0.0, 0.0, 1.0], &[1.0, 1.0, 0.0]]);
        let t = tag_rows(&rows(&[&[1.0, 0.1, 0.0], &[0.0, 0.0, 1.0], &[-1.0, -1.0, -1.0]]), &m, 0.2);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].iter().map(|x| x.0).collect::<Vec<_>>(), [0, 3], "best first, the weak ones left out");
        assert_eq!(t[1].iter().map(|x| x.0).collect::<Vec<_>>(), [2]);
        assert!(t[2].is_empty());
        // at most TOP_K
        let many: Vec<Vec<f32>> = (0..40).map(|i| vec![1.0, i as f32 * 0.01]).collect();
        let refs: Vec<&[f32]> = many.iter().map(Vec::as_slice).collect();
        let t = tag_rows(&rows(&[&[1.0, 0.0]]), &matrix(&refs), 0.0);
        assert_eq!(t[0].len(), TOP_K);
        assert_eq!(t[0][0].0, 0, "the closest first");
        // sizes that do not fit
        assert!(tag_rows(&rows(&[&[1.0, 0.0]]), &m, 0.0).is_empty());
    }

    #[test]
    fn the_vocabulary_cache_round_trips_and_refuses_other_models_and_damage() {
        let d = std::env::temp_dir().join(format!("lc-vocabcache-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = vocab_cache_path(&d, "m");
        let s = Some(Scoring { scale: 100.0, bias: -10.0, sigmoid: true });
        let (a, b) = (vec![0.6f32, 0.8], vec![1.0f32, 0.0]);
        write_vocab_cache(&p, "m@1", 2, s, [("dog", a.as_slice()), ("cat\npuppy", b.as_slice())].into_iter()).unwrap();
        let c = read_vocab_cache(&p, "m@1", 2);
        assert_eq!(c.scoring, s);
        assert_eq!(c.vectors.len(), 2);
        assert!((c.vectors["dog"][1] - 0.8).abs() < 1e-3);
        assert!(c.note.is_none());
        // another model, another size: rebuilt
        assert!(read_vocab_cache(&p, "m@2", 2).vectors.is_empty() && read_vocab_cache(&p, "m@2", 2).note.is_some());
        assert!(read_vocab_cache(&p, "m@1", 3).vectors.is_empty());
        // every truncation and some flipped bytes: never a panic, never a wrong-sized vector
        let bytes = std::fs::read(&p).unwrap();
        for cut in 0..bytes.len() {
            std::fs::write(&p, &bytes[..cut]).unwrap();
            let c = read_vocab_cache(&p, "m@1", 2);
            assert!(c.vectors.values().all(|v| v.len() == 2));
        }
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0xff;
            std::fs::write(&p, &b).unwrap();
            let c = read_vocab_cache(&p, "m@1", 2);
            assert!(c.vectors.values().all(|v| v.len() == 2 && v.iter().all(|x| x.is_finite())));
        }
        assert!(read_vocab_cache(&d.join("missing.bin"), "m@1", 2).note.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dismissals_persist_and_a_damaged_file_is_ignored() {
        let d = std::env::temp_dir().join(format!("lc-dismissed-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = dismissed_path(&d);
        let mut x = Dismissed::open(Some(p.clone()));
        assert_eq!(x.add([(1, vocab::path_hash("animal|dog")), (1, vocab::path_hash("animal|dog")), (2, 5)].into_iter()).unwrap(), 2);
        let again = Dismissed::open(Some(p.clone()));
        assert!(again.contains(1, "Animal|Dog") && !again.contains(2, "animal|dog") && again.set.contains(&(2, 5)));
        // torn tail: the whole records are kept
        let mut b = std::fs::read(&p).unwrap();
        b.extend_from_slice(&[1, 2, 3]);
        std::fs::write(&p, &b).unwrap();
        let torn = Dismissed::open(Some(p.clone()));
        assert_eq!(torn.set.len(), 2);
        assert!(torn.note.is_some());
        // damaged: ignored, and written whole with the next dismissal
        std::fs::write(&p, b"garbage").unwrap();
        let mut bad = Dismissed::open(Some(p.clone()));
        assert!(bad.set.is_empty() && bad.note.is_some());
        bad.add([(3, 4)].into_iter()).unwrap();
        assert_eq!(Dismissed::open(Some(p.clone())).set.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_view_applies_the_cut_off_dismissals_and_implies_the_levels_above() {
        let v = vocab::parse("animal|bird|owl\nanimal|mammal|dog\nscene|beach @0.03\nanimal|mammal|dog|spaniel @0.1");
        let mut scores = HashMap::new();
        scores.insert(1u64, ([0u8; 16], vec![(0u32, 0.5f32), (2, 0.04), (1, 0.001)]));
        scores.insert(2u64, ([0u8; 16], vec![(1u32, 0.3f32)]));
        scores.insert(3u64, ([0u8; 16], vec![(2u32, 0.01f32)]));
        // a breed under its own cut-off counts for "dog" (once), one above it is shown itself
        scores.insert(4u64, ([0u8; 16], vec![(3u32, 0.05f32), (1, 0.03)]));
        scores.insert(5u64, ([0u8; 16], vec![(3u32, 0.2f32)]));
        let cut = |j: usize| v.terms[j].threshold.unwrap_or(0.02);
        let view = View::build(&scores, &v, 0.02, cut, |p, j| p == 2 && j == 1);
        assert_eq!(view.photo[&4], vec![(1, 0.05)]);
        assert_eq!(view.photo[&5], vec![(3, 0.2)]);
        assert!(view.has(5, "animal|mammal|dog"));
        assert_eq!(view.photo[&1].iter().map(|x| x.0).collect::<Vec<_>>(), [0, 2]);
        assert!(!view.photo.contains_key(&2), "dismissed");
        assert!(!view.photo.contains_key(&3), "below its tag's own cut-off");
        assert!(view.has(1, "animal") && view.has(1, "Animal|Bird") && view.has(1, "animal|bird|owl") && view.has(1, "scene"));
        assert!(!view.has(1, "animal|mammal") && !view.has(2, "animal"));
        assert_eq!(view.under["animal"].1.len(), 3);
    }
}
