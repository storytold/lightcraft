//! Filtering, search and sorting.

use serde::{Deserialize, Serialize};

use crate::{AlbumId, Catalog, ColorLabel, Flag, MediaKind, Photo, PhotoId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RatingOp {
    #[default]
    AtLeast,
    Exactly,
    AtMost,
}

/// What the grid shows. Empty/None fields don't filter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Filter {
    /// Free-text search: every token must match filename, title, caption, keywords, camera, lens,
    /// location or format. Tokens like `rating:3`, `flag:pick`, `iso:>800`, `camera:x2` are fielded.
    pub text: String,
    pub rating: u8,
    pub rating_op: RatingOp,
    pub flag: Option<Flag>,
    pub label: Option<ColorLabel>,
    /// Only these photos (Find Similar results…); empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<PhotoId>,
    /// Any of these labels (the filter bar's multi-select); empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<ColorLabel>,
    pub kind: Option<MediaKind>,
    pub edited: Option<bool>,
    pub album: Option<AlbumId>,
    /// Show "Recently Deleted" instead of the library.
    pub deleted: bool,
    /// Capture date prefix (`2026`, `2026-04`, `2026-04-12`).
    pub date: Option<String>,
    /// A folder the library's photos were imported from: those photos and the ones imported from
    /// the folders inside it (see [`crate::folders`]). Photos only browsed in Local are never part
    /// of it, unlike [`Filter::folder`].
    pub library_folder: Option<String>,
    /// A keyword; hierarchical keywords match their children too (`travel` finds `travel|italy`).
    pub keyword: Option<String>,
    /// A person: photos with a named face region of this name (case-insensitive), as read from XMP.
    pub person: Option<String>,
    pub camera: Option<String>,
    /// Lens (case-insensitive substring).
    pub lens: Option<String>,
    /// Capture date range, inclusive: `dateFrom` is compared as a lower bound (`2026-04-01`),
    /// `dateTo` as a prefix upper bound (`2026-04` includes all of April).
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    /// Import date prefix.
    pub imported: Option<String>,
    /// Imported at or after this time (ISO; Recently Added).
    pub imported_from: Option<String>,
    /// Photo Merge results: `hdr`, `panorama`, `hdrPanorama` or `any` (see [`merged_kind`]).
    pub merged: Option<String>,
    /// Immich link state: `linked`, `notLinked` (no confirmed link) or `probable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub immich: Option<String>,
    /// A folder on disk: its files only (browsed ones too); `subfolders` includes everything below.
    pub folder: Option<String>,
    pub subfolders: bool,
    /// Smart-album rules (all / any / none, nested groups; see [`crate::rules`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_set: Option<crate::RuleSet>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortKey {
    #[default]
    CaptureDate,
    ImportDate,
    EditDate,
    FileName,
    Rating,
    FileSize,
    /// A shuffle fixed by [`Sort::seed`]: the same seed always gives the same order, and photos
    /// added or edited later never reshuffle the others. Pick a new seed to reshuffle.
    Random,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub key: SortKey,
    pub ascending: bool,
    /// Date headers in the grid (date sort keys only).
    pub group: crate::GroupBy,
    /// Which shuffle [`SortKey::Random`] gives (ignored by the other keys).
    pub seed: u64,
}

impl Default for Sort {
    fn default() -> Self {
        Sort { key: SortKey::CaptureDate, ascending: false, group: crate::GroupBy::Auto, seed: 0 }
    }
}

/// splitmix64 finaliser: a stateless, platform-independent mix (no RNG state, no `rand` version
/// to drift), so a seed names the same shuffle on every machine.
pub fn mix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Libraries this large are filtered on several threads.
const PARALLEL_FROM: usize = 20_000;

/// `v` filtered by `keep`, in order, on up to 16 threads (one on the web).
fn par_filter<'a>(v: &[&'a Photo], keep: &(dyn Fn(&&Photo) -> bool + Sync)) -> Vec<&'a Photo> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
        let chunk = v.len().div_ceil(threads).max(1);
        let parts: Option<Vec<Vec<&'a Photo>>> = std::thread::scope(|s| {
            let jobs: Vec<_> = v.chunks(chunk).map(|c| s.spawn(move || c.iter().copied().filter(|p| keep(p)).collect::<Vec<_>>())).collect();
            jobs.into_iter().map(|j| j.join().ok()).collect()
        });
        if let Some(parts) = parts {
            return parts.concat();
        }
        // a worker panicked (it can't): filter here instead
    }
    v.iter().copied().filter(|p| keep(p)).collect()
}

/// `f(position, item)` over `v`, on up to 16 threads (one on the web).
fn par_each<'a>(v: &[&'a Photo], f: &(dyn Fn(usize, &&'a Photo) + Sync)) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
        let chunk = v.len().div_ceil(threads).max(1);
        let done: Option<Vec<()>> = std::thread::scope(|s| {
            let jobs: Vec<_> =
                v.chunks(chunk).enumerate().map(|(k, c)| s.spawn(move || c.iter().enumerate().for_each(|(i, p)| f(k * chunk + i, p)))).collect();
            jobs.into_iter().map(|j| j.join().ok()).collect()
        });
        if done.is_some() {
            return;
        }
        // a worker panicked (it can't): run here instead (marking twice is harmless)
    }
    v.iter().enumerate().for_each(|(i, p)| f(i, p));
}

/// One bit per rank, set from several threads.
struct RankMarks(Vec<std::sync::atomic::AtomicU64>);

impl RankMarks {
    fn new(n: usize) -> RankMarks {
        RankMarks((0..n.div_ceil(64)).map(|_| std::sync::atomic::AtomicU64::new(0)).collect())
    }
    fn set(&self, r: usize) {
        if let Some(w) = self.0.get(r / 64) {
            w.fetch_or(1 << (r % 64), std::sync::atomic::Ordering::Relaxed);
        }
    }
    fn get(&self, r: usize) -> bool {
        self.0.get(r / 64).is_some_and(|w| w.load(std::sync::atomic::Ordering::Relaxed) & (1 << (r % 64)) != 0)
    }
}

/// The whole catalog in one sort order, ascending (descending is its exact reverse: ids break
/// every tie).
pub(crate) struct Sorted {
    key: SortKey,
    seed: u64,
    structural: u64,
    other: u64,
    len: usize,
    /// Photo ids by rank.
    order: Vec<PhotoId>,
    /// Rank of each photo, by its position in the catalog (id order).
    rank: Vec<u32>,
}

/// Sort orders of the whole catalog kept between queries, so a query only filters: the grid's
/// default (all photos by capture date) no longer sorts 500 000 photos on every refresh. Ops that
/// change a sort key bump an epoch ([`Catalog::apply`]), which retires the orders built before.
/// Capture date, import date and random orders survive everything but added or removed photos and
/// capture-time changes, so editing never re-sorts them. At most two orders are kept (about 12
/// bytes a photo each).
#[derive(Default)]
pub(crate) struct SortCache {
    /// Photos added or removed, or a capture time changed.
    structural: u64,
    /// Any other sort key changed (rating, edit time, file name, file size).
    other: u64,
    entries: std::sync::Mutex<Vec<std::sync::Arc<Sorted>>>,
}

impl Clone for SortCache {
    fn clone(&self) -> Self {
        SortCache::default()
    }
}

impl PartialEq for SortCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for SortCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SortCache")
    }
}

/// Orders worth keeping: below this a sort is cheaper than the bookkeeping.
const CACHE_SORT_FROM: usize = 2_000;

impl SortCache {
    /// An op is about to change sort keys: `structural` = which photos exist or a capture time.
    pub(crate) fn bump(&mut self, structural: bool) {
        if structural {
            self.structural = self.structural.wrapping_add(1);
        } else {
            self.other = self.other.wrapping_add(1);
        }
    }

    fn valid(&self, e: &Sorted, key: SortKey, seed: u64, len: usize) -> bool {
        let only_structural = matches!(key, SortKey::CaptureDate | SortKey::ImportDate | SortKey::Random);
        e.key == key
            && (key != SortKey::Random || e.seed == seed)
            && e.len == len
            && e.structural == self.structural
            && (only_structural || e.other == self.other)
    }

    /// The order of `all` (the catalog's photos in id order) by `sort`, built when missing. `None`
    /// for small catalogs (sorted directly).
    fn sorted(&self, sort: &Sort, all: &[&Photo]) -> Option<std::sync::Arc<Sorted>> {
        if all.len() < CACHE_SORT_FROM || u32::try_from(all.len()).is_err() {
            return None;
        }
        {
            let entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(e) = entries.iter().find(|e| self.valid(e, sort.key, sort.seed, all.len())) {
                return Some(e.clone());
            }
        }
        let order = sort_photos(all.to_vec(), &Sort { ascending: true, ..*sort });
        // ranks back in id order, which is `all`'s
        let mut by_id: Vec<(PhotoId, u32)> = order.iter().enumerate().map(|(r, id)| (*id, r as u32)).collect();
        by_id.sort_unstable();
        if by_id.len() != all.len() || by_id.iter().zip(all).any(|((id, _), p)| *id != p.id) {
            return None;
        }
        let rank: Vec<u32> = by_id.into_iter().map(|(_, r)| r).collect();
        let e = std::sync::Arc::new(Sorted {
            key: sort.key,
            seed: sort.seed,
            structural: self.structural,
            other: self.other,
            len: all.len(),
            order,
            rank,
        });
        let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|x| self.valid(x, x.key, x.seed, all.len()) && !(x.key == e.key && x.seed == e.seed));
        entries.insert(0, e.clone());
        entries.truncate(2);
        Some(e)
    }
}

/// A string's sort key, 24 bytes: its first 23 bytes (zero-padded, big-endian) and a tag, 0 for
/// no string, else 1 + its length capped at 24. Keys order exactly like the strings except that
/// two strings longer than 23 bytes with the same start (tag 25 both) must be compared as
/// strings.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct SKey(u64, u64, u64);

impl SKey {
    fn of(s: Option<&str>) -> SKey {
        let Some(s) = s else { return SKey::default() };
        let mut b = [0u8; 24];
        for (d, s) in b.iter_mut().zip(s.bytes().take(23)) {
            *d = s;
        }
        b[23] = 1 + s.len().min(24) as u8;
        let w = |i: usize| {
            let mut x = [0u8; 8];
            x.copy_from_slice(b.get(i..i + 8).unwrap_or(&[0; 8]));
            u64::from_be_bytes(x)
        };
        SKey(w(0), w(8), w(16))
    }
    /// Compare, falling back to the strings only when the keys can't tell.
    fn cmp_or(&self, other: &SKey, a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
        let o = self.cmp(other);
        if o.is_eq() && self.2 & 0xff == 25 { a.cmp(&b) } else { o }
    }
}

/// Order photos by `sort` (ties broken by id, as always). Keys are computed once per photo and
/// held inline, so comparisons rarely follow a pointer (one `to_lowercase` per file name, not one
/// per comparison).
fn sort_photos<'a>(v: Vec<&'a Photo>, sort: &Sort) -> Vec<PhotoId> {
    type Get = fn(&Photo) -> Option<&str>;
    struct E<'a> {
        k1: SKey,
        k2: SKey,
        num: u64,
        id: u64,
        p: &'a Photo,
    }
    fn none(_: &Photo) -> Option<&str> {
        None
    }
    let asc = sort.ascending;
    let seed = sort.seed;
    let (s1, s2, num): (Get, Get, fn(&Photo, u64) -> u64) = match sort.key {
        SortKey::CaptureDate => (|p| p.captured.as_deref(), |p| Some(p.imported.as_str()), |_, _| 0),
        SortKey::ImportDate => (|p| Some(p.imported.as_str()), none, |_, _| 0),
        SortKey::EditDate => (|p| p.edited.as_deref(), none, |_, _| 0),
        SortKey::Rating => (none, none, |p, _| u64::from(p.rating)),
        SortKey::FileSize => (none, none, |p, _| p.file_size),
        SortKey::Random => (none, none, |p, seed| shuffle_rank(seed, p.id)),
        SortKey::FileName => {
            let mut keyed: Vec<(String, u64)> = v.into_iter().map(|p| (p.file_name.to_lowercase(), p.id.0)).collect();
            keyed.sort_unstable_by(|a, b| {
                let o = a.cmp(b);
                if asc { o } else { o.reverse() }
            });
            return keyed.into_iter().map(|(_, id)| PhotoId(id)).collect();
        }
    };
    // reading every photo's strings is mostly waiting for memory: done on several threads
    let make = |p: &&'a Photo| E { k1: SKey::of(s1(p)), k2: SKey::of(s2(p)), num: num(p, seed), id: p.id.0, p };
    let keyed: Vec<E> = par_map_photos(&v, &make);
    let cmp = |a: &E, b: &E| {
        let o = a.k1.cmp_or(&b.k1, s1(a.p), s1(b.p)).then_with(|| a.k2.cmp_or(&b.k2, s2(a.p), s2(b.p))).then(a.num.cmp(&b.num)).then(a.id.cmp(&b.id));
        if asc { o } else { o.reverse() }
    };
    let keyed = par_sort(keyed, &cmp);
    keyed.into_iter().map(|e| PhotoId(e.id)).collect()
}

/// `f` over `v`, in order, on several threads when large.
fn par_map_photos<'a, R: Send>(v: &[&'a Photo], f: &(dyn Fn(&&'a Photo) -> R + Sync)) -> Vec<R> {
    #[cfg(not(target_arch = "wasm32"))]
    if v.len() >= PARALLEL_FROM {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
        let chunk = v.len().div_ceil(threads).max(1);
        let parts: Option<Vec<Vec<R>>> = std::thread::scope(|s| {
            let jobs: Vec<_> = v.chunks(chunk).map(|c| s.spawn(move || c.iter().map(f).collect::<Vec<R>>())).collect();
            jobs.into_iter().map(|j| j.join().ok()).collect()
        });
        if let Some(parts) = parts {
            return parts.into_iter().flatten().collect();
        }
    }
    v.iter().map(f).collect()
}

type Cmp<'c, T> = &'c (dyn Fn(&T, &T) -> std::cmp::Ordering + Sync);

/// Sort, on several threads when large: chunks sorted in parallel, then merged pairwise in
/// parallel. The order is total (ids break ties), so the result is the one a single sort gives.
fn par_sort<T: Send>(mut v: Vec<T>, cmp: Cmp<'_, T>) -> Vec<T> {
    #[cfg(not(target_arch = "wasm32"))]
    if v.len() >= PARALLEL_FROM {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
        let chunk = v.len().div_ceil(threads).max(1);
        let mut runs = Vec::with_capacity(threads);
        while v.len() > chunk {
            let tail = v.split_off(v.len() - chunk);
            runs.push(tail);
        }
        runs.push(v);
        runs.reverse();
        let sorted: Option<Vec<Vec<T>>> = std::thread::scope(|s| {
            let jobs: Vec<_> = runs
                .into_iter()
                .map(|mut r| {
                    s.spawn(move || {
                        r.sort_unstable_by(cmp);
                        r
                    })
                })
                .collect();
            jobs.into_iter().map(|j| j.join().ok()).collect()
        });
        let Some(mut runs) = sorted else { return Vec::new() };
        while runs.len() > 1 {
            let mut pairs = Vec::with_capacity(runs.len() / 2 + 1);
            let mut it = runs.into_iter();
            while let Some(a) = it.next() {
                pairs.push((a, it.next()));
            }
            let merged: Option<Vec<Vec<T>>> = std::thread::scope(|s| {
                let jobs: Vec<_> = pairs
                    .into_iter()
                    .map(|(a, b)| {
                        s.spawn(move || match b {
                            Some(b) => merge(a, b, cmp),
                            None => a,
                        })
                    })
                    .collect();
                jobs.into_iter().map(|j| j.join().ok()).collect()
            });
            let Some(m) = merged else { return Vec::new() };
            runs = m;
        }
        return runs.pop().unwrap_or_default();
    }
    v.sort_unstable_by(cmp);
    v
}

/// Merge two sorted runs (stable: `a` first on ties).
#[cfg(not(target_arch = "wasm32"))]
fn merge<T>(a: Vec<T>, b: Vec<T>, cmp: Cmp<'_, T>) -> Vec<T> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let mut a = a.into_iter().peekable();
    let mut b = b.into_iter().peekable();
    loop {
        let take_b = match (a.peek(), b.peek()) {
            (Some(x), Some(y)) => cmp(y, x).is_lt(),
            (Some(_), None) => false,
            (None, Some(_)) => true,
            (None, None) => break,
        };
        out.extend(if take_b { b.next() } else { a.next() });
    }
    out
}

/// Whether photo `id`'s Immich link state is `want` (`linked`, `probable`, `none` or
/// `notLinked`: anything but a confirmed link). Unknown values match nothing.
pub fn immich_state_is(cat: &Catalog, id: PhotoId, want: &str) -> bool {
    let s = cat.remote_links().link_state(id, "immich");
    match want {
        "notLinked" | "not linked" | "unlinked" => s != "linked",
        "none" => s == "none",
        w => s == w,
    }
}

/// A photo's place in the shuffle for `seed`.
fn shuffle_rank(seed: u64, id: PhotoId) -> u64 {
    mix64(mix64(seed) ^ id.0)
}

fn token_matches(p: &Photo, tok: &str) -> bool {
    let t = tok.to_lowercase();
    if let Some((field, val)) = t.split_once(':') {
        let num = |s: &str| -> Option<(char, f64)> {
            let (op, rest) = match s.chars().next()? {
                c @ ('>' | '<' | '=') => (c, &s[1..]),
                _ => ('=', s),
            };
            rest.parse().ok().map(|v| (op, v))
        };
        let cmp = |x: f64, s: &str| match num(s) {
            Some(('>', v)) => x > v,
            Some(('<', v)) => x < v,
            Some((_, v)) => (x - v).abs() < 1e-9,
            None => false,
        };
        return match field {
            "rating" | "stars" => cmp(p.rating as f64, val),
            "flag" => Flag::parse(val) == Some(p.flag),
            "label" | "color" => p.label.is_some_and(|l| format!("{l:?}").eq_ignore_ascii_case(val)),
            "iso" => p.meta.iso.is_some_and(|i| cmp(i as f64, val)),
            "f" | "aperture" => p.meta.aperture.is_some_and(|a| cmp(a as f64, val)),
            "focal" => p.meta.focal_mm.is_some_and(|a| cmp(a as f64, val)),
            "camera" => p.meta.camera.to_lowercase().contains(val),
            "lens" => p.meta.lens.to_lowercase().contains(val),
            "keyword" | "kw" => p.meta.keywords.iter().any(|k| crate::keywords::is_under(k, val)),
            "person" | "who" => has_person(p, val),
            "type" | "kind" => format!("{:?}", p.kind).eq_ignore_ascii_case(val),
            "edited" => (val == "true" || val == "yes") == p.is_edited(),
            "date" => p.captured.as_deref().is_some_and(|c| c.starts_with(val)),
            "copy" | "virtual" => (val == "true" || val == "yes") == p.copy_of.is_some(),
            "name" | "file" => p.file_name.to_lowercase().contains(val),
            _ => false,
        };
    }
    let hay = [&p.file_name, &p.meta.title, &p.meta.caption, &p.meta.camera, &p.meta.lens, &p.meta.location, &p.format];
    hay.iter().any(|h| h.to_lowercase().contains(&t)) || p.meta.keywords.iter().any(|k| k.to_lowercase().contains(&t))
}

/// Whether `p` has a named face region called `name` (case-insensitive).
fn has_person(p: &Photo, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    p.meta.regions.iter().any(|r| r.kind == dac_meta::RegionKind::Face && r.name.as_deref().is_some_and(|n| n.to_lowercase() == name))
}

impl Filter {
    /// Human-readable summary of the active rules (smart album tooltips, `album.list`).
    pub fn describe(&self) -> String {
        let mut v: Vec<String> = Vec::new();
        if self.rating > 0 {
            let op = match self.rating_op {
                RatingOp::AtLeast => "≥",
                RatingOp::Exactly => "=",
                RatingOp::AtMost => "≤",
            };
            v.push(format!("rating {op} {}", self.rating));
        }
        if let Some(f) = self.flag {
            v.push(format!("flag {}", format!("{f:?}").to_lowercase()));
        }
        if let Some(l) = self.label {
            v.push(format!("label {}", format!("{l:?}").to_lowercase()));
        }
        if !self.labels.is_empty() {
            v.push(format!("label {}", self.labels.iter().map(|l| format!("{l:?}").to_lowercase()).collect::<Vec<_>>().join(" or ")));
        }
        if let Some(k) = self.kind {
            v.push(format!("kind {}", format!("{k:?}").to_lowercase()));
        }
        if let Some(e) = self.edited {
            v.push(if e { "edited".into() } else { "unedited".into() });
        }
        for (name, val) in [
            ("keyword", &self.keyword),
            ("person", &self.person),
            ("camera", &self.camera),
            ("lens", &self.lens),
            ("date", &self.date),
            ("folder", &self.library_folder),
            ("imported", &self.imported),
        ] {
            if let Some(x) = val {
                v.push(format!("{name} {x}"));
            }
        }
        match (&self.date_from, &self.date_to) {
            (Some(a), Some(b)) => v.push(format!("captured {a} – {b}")),
            (Some(a), None) => v.push(format!("captured from {a}")),
            (None, Some(b)) => v.push(format!("captured until {b}")),
            _ => {}
        }
        if !self.text.trim().is_empty() {
            v.push(format!("“{}”", self.text.trim()));
        }
        if let Some(rs) = self.rule_set.as_ref().filter(|r| !r.rules.is_empty()) {
            v.push(rs.describe());
        }
        if v.is_empty() { "all photos".into() } else { v.join(", ") }
    }

    /// Whether matches depend on the clock (only "in the last…" rules do).
    pub fn depends_on_now(&self) -> bool {
        self.rule_set.as_ref().is_some_and(crate::RuleSet::depends_on_now)
    }

    pub fn matches(&self, p: &Photo, cat: &Catalog) -> bool {
        self.matches_in(p, cat, self.library_root().as_deref())
    }

    /// [`Filter::library_folder`] as a folder identity ([`folder_key`]); `None` without a choice.
    fn library_root(&self) -> Option<String> {
        // as given: a folder may be named with spaces at its edge, and trimming would make it
        // another folder
        self.library_folder.as_deref().filter(|d| !d.trim().is_empty()).map(folder_key)
    }

    /// [`Filter::matches`] with the library folder already turned into its identity, so a query
    /// over a big library does that once, not once per photo.
    fn matches_in(&self, p: &Photo, cat: &Catalog, root: Option<&str>) -> bool {
        if p.deleted != self.deleted {
            return false;
        }
        match &self.folder {
            // browsed photos only show in folder views
            None if p.local => return false,
            None => {}
            Some(dir) => {
                let crate::Source::File { path } = &p.source else { return false };
                if !in_folder(path, dir, self.subfolders) {
                    return false;
                }
            }
        }
        if self.rating > 0 {
            let ok = match self.rating_op {
                RatingOp::AtLeast => p.rating >= self.rating,
                RatingOp::Exactly => p.rating == self.rating,
                RatingOp::AtMost => p.rating <= self.rating,
            };
            if !ok {
                return false;
            }
        }
        if self.flag.is_some_and(|f| f != p.flag) || self.label.is_some_and(|l| Some(l) != p.label) || self.kind.is_some_and(|k| k != p.kind) {
            return false;
        }
        if self.edited.is_some_and(|e| e != p.is_edited()) {
            return false;
        }
        if let Some(rs) = &self.rule_set
            && !rs.matches(p, cat)
        {
            return false;
        }
        if !self.labels.is_empty() && !p.label.is_some_and(|l| self.labels.contains(&l)) {
            return false;
        }
        if !self.only.is_empty() && !self.only.contains(&p.id) {
            return false;
        }
        if let Some(want) = &self.immich
            && !immich_state_is(cat, p.id, want)
        {
            return false;
        }
        if let Some(want) = &self.merged {
            match merged_kind(&p.file_name) {
                Some(k) if want == "any" || want == k => {}
                _ => return false,
            }
        }
        if let Some(a) = self.album
            && !cat.album_contains(a, p)
        {
            return false;
        }
        if let Some(from) = &self.date_from
            && match p.captured.as_deref() {
                Some(c) => c < from.as_str(),
                None => true,
            }
        {
            return false;
        }
        if let Some(to) = &self.date_to
            && match p.captured.as_deref() {
                Some(c) => c.get(..to.len()).unwrap_or(c) > to.as_str(),
                None => true,
            }
        {
            return false;
        }
        if let Some(l) = &self.lens
            && !p.meta.lens.to_lowercase().contains(&l.to_lowercase())
        {
            return false;
        }
        if let Some(d) = &self.date
            && !p.captured.as_deref().is_some_and(|c| c.starts_with(d.as_str()))
        {
            return false;
        }
        if let Some(root) = root
            && !photo_in_root(p, root)
        {
            return false;
        }
        if let Some(d) = &self.imported
            && !p.imported.starts_with(d.as_str())
        {
            return false;
        }
        if let Some(d) = &self.imported_from
            && p.imported.as_str() < d.as_str()
        {
            return false;
        }
        if let Some(k) = &self.keyword
            && !p.meta.keywords.iter().any(|x| crate::keywords::is_under(x, k))
        {
            return false;
        }
        if let Some(n) = &self.person
            && !has_person(p, n)
        {
            return false;
        }
        if let Some(c) = &self.camera
            && !p.meta.camera.eq_ignore_ascii_case(c)
        {
            return false;
        }
        self.text.split_whitespace().all(|tok| token_matches(p, tok))
    }
}

impl Catalog {
    /// Photos matching `filter`, in `sort` order (ties broken by id for stability).
    pub fn query(&self, filter: &Filter, sort: &Sort) -> Vec<PhotoId> {
        self.query_with(filter, sort, true)
    }

    /// [`Self::query`], with or without the kept sort orders (tests compare both).
    pub(crate) fn query_with(&self, filter: &Filter, sort: &Sort, cached: bool) -> Vec<PhotoId> {
        let root = filter.library_root();
        let keep = |p: &&Photo| filter.matches_in(p, self, root.as_deref());
        let all: Vec<&Photo> = self.photos().map(|p| p.as_ref()).collect();
        if cached && let Some(sorted) = self.sort_cache.sorted(sort, &all) {
            // the whole catalog's order is known: mark the matches by rank, read them off in order
            let marks = RankMarks::new(all.len());
            let mark = |pos: usize, p: &&Photo| {
                if keep(p)
                    && let Some(r) = sorted.rank.get(pos)
                {
                    marks.set(*r as usize);
                }
            };
            if all.len() >= PARALLEL_FROM && !filter.depends_on_now() {
                par_each(&all, &mark);
            } else {
                all.iter().enumerate().for_each(|(i, p)| mark(i, p));
            }
            let pick = |(r, id): (usize, &PhotoId)| marks.get(r).then_some(*id);
            return if sort.ascending {
                sorted.order.iter().enumerate().filter_map(pick).collect()
            } else {
                sorted.order.iter().enumerate().rev().filter_map(pick).collect()
            };
        }
        // large libraries: filter on several threads (not when a rule reads the thread's clock)
        let v: Vec<&Photo> =
            if all.len() >= PARALLEL_FROM && !filter.depends_on_now() { par_filter(&all, &keep) } else { all.into_iter().filter(keep).collect() };
        sort_photos(v, sort)
    }

    /// Year → month → day counts for the "By Date" section (newest first).
    pub fn date_groups(&self) -> Vec<DateGroup> {
        use std::collections::BTreeMap;
        let mut years: BTreeMap<&str, (BTreeMap<&str, usize>, BTreeMap<&str, usize>)> = BTreeMap::new();
        for p in self.photos().filter(|p| p.in_library()) {
            let Some(d) = p.captured.as_deref() else { continue };
            if d.len() >= 10 {
                let (months, days) = years.entry(&d[..4]).or_default();
                *months.entry(&d[..7]).or_default() += 1;
                *days.entry(&d[..10]).or_default() += 1;
            }
        }
        let newest_first = |m: BTreeMap<&str, usize>| m.into_iter().rev().map(|(k, n)| (k.to_string(), n)).collect::<Vec<_>>();
        years
            .into_iter()
            .rev()
            .map(|(year, (months, days))| DateGroup {
                year: year.to_string(),
                count: months.values().sum(),
                months: newest_first(months),
                days: newest_first(days),
            })
            .collect()
    }

    /// All keywords with usage counts, sorted by name.
    pub fn keywords(&self) -> Vec<(String, usize)> {
        let mut m: std::collections::BTreeMap<String, usize> = Default::default();
        for p in self.photos().filter(|p| p.in_library()) {
            for k in &p.meta.keywords {
                *m.entry(k.clone()).or_default() += 1;
            }
        }
        m.into_iter().collect()
    }

    /// The people named on faces in the library (MWG regions read from XMP): how many photos each
    /// appears in and the photo showing their largest face (for a card's picture). Most photos first,
    /// then by name. Names that differ only in case are one person, shown as first seen; a person
    /// twice in one photo counts once.
    pub fn people(&self) -> Vec<Person> {
        self.people_in(&Filter::default())
    }

    /// [`Self::people`] among the photos `filter` lets through (its `person` is ignored): what the
    /// People view offers while other filters (a date, a rating, an album…) are active, so picking
    /// a person never ends in an empty grid.
    pub fn people_in(&self, filter: &Filter) -> Vec<Person> {
        let filter = Filter { person: None, ..filter.clone() };
        let mut m: std::collections::HashMap<String, (Person, f64)> = Default::default();
        let root = filter.library_root();
        for p in self.photos().filter(|p| filter.matches_in(p, self, root.as_deref())) {
            let mut seen: Vec<String> = Vec::new();
            for r in p.meta.regions.iter().filter(|r| r.kind == dac_meta::RegionKind::Face) {
                let Some(name) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
                let key = name.to_lowercase();
                // the face's size in pixels of the photo
                let area = (r.rect.x1 - r.rect.x0) * (r.rect.y1 - r.rect.y0) * p.width as f64 * p.height as f64;
                let (person, best) =
                    m.entry(key.clone()).or_insert_with(|| (Person { name: name.to_string(), count: 0, photo: p.id, face: r.rect }, area));
                if !seen.contains(&key) {
                    person.count += 1;
                    seen.push(key);
                }
                if area > *best || (area == *best && p.id < person.photo) {
                    (person.photo, person.face, *best) = (p.id, r.rect, area);
                }
            }
        }
        let mut v: Vec<Person> = m.into_values().map(|(p, _)| p).collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }
}

/// A person named on faces in the library ([`Catalog::people`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub name: String,
    /// Photos they appear in.
    pub count: usize,
    /// The photo with their largest face, and that face (normalized, in the photo's upright frame).
    pub photo: PhotoId,
    pub face: dac_meta::Rect,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DateGroup {
    pub year: String,
    pub count: usize,
    /// `YYYY-MM` and counts, newest first.
    pub months: Vec<(String, usize)>,
    /// `YYYY-MM-DD` and counts, newest first.
    pub days: Vec<(String, usize)>,
}

/// Whether file `path` is directly in `dir` (or anywhere below it with `deep`). Both `/` and `\\`
/// separate.
pub fn in_folder(path: &str, dir: &str, deep: bool) -> bool {
    let dir = dir.trim_end_matches(['/', '\\']);
    let Some(rest) = path.strip_prefix(dir) else { return false };
    let Some(rest) = rest.strip_prefix(['/', '\\']) else { return false };
    !rest.is_empty() && (deep || !rest.contains(['/', '\\']))
}

/// A path read the way [`folder_key`] reads it: the part `..` can't climb above (`//server/share`,
/// `c:`; empty for `/` and relative paths), the names below it, and whether it is absolute.
pub(crate) struct SplitPath {
    pub prefix: String,
    pub parts: Vec<String>,
    pub absolute: bool,
}

/// Both separators, repeated separators (also between a server and its share), `.` and a
/// trailing separator dropped, `..` resolved lexically, a Windows verbatim prefix removed;
/// `lower_drive` lower-cases a drive letter (the identity does, a display name wants it as is).
pub(crate) fn split_path(path: &str, lower_drive: bool) -> SplitPath {
    let mut s = path.replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/").or_else(|| s.strip_prefix("//./")) {
        s = match rest.strip_prefix("UNC/") {
            Some(unc) => format!("//{unc}"),
            None => rest.to_string(),
        };
    }
    let (prefix, rest) = if let Some(unc) = s.strip_prefix("//").filter(|u| !u.is_empty() && !u.starts_with('/')) {
        let mut segs = unc.split('/').filter(|x| !x.is_empty());
        let (server, share) = (segs.next().unwrap_or(""), segs.next().unwrap_or(""));
        (format!("//{server}/{share}"), segs.collect::<Vec<_>>().join("/"))
    } else if let (true, Some(drive), Some(rest)) = (matches!(s.as_bytes(), [a, b':', ..] if a.is_ascii_alphabetic()), s.get(..2), s.get(2..)) {
        (if lower_drive { drive.to_ascii_lowercase() } else { drive.to_string() }, rest.to_string())
    } else {
        (String::new(), s.clone())
    };
    let absolute = rest.starts_with('/') || !prefix.is_empty();
    let mut parts: Vec<String> = Vec::new();
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| p != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            c => parts.push(c.to_string()),
        }
    }
    SplitPath { prefix, parts, absolute }
}

/// A folder path's identity, for telling whether two spellings name the same folder: `/` and
/// `\\` are both separators, repeated separators, `.` and a trailing separator are dropped,
/// `..` is resolved lexically, a Windows verbatim prefix (`\\?\`) is removed and the drive
/// letter lower-cased; on Windows (case-insensitive file names) the whole path is lower-cased.
/// File names are otherwise compared as written, so on a case-insensitive disk elsewhere
/// (macOS, a Samba share) two spellings that differ in case are two folders here.
/// No file-system access: the paths compared should already be absolute.
pub fn folder_key(path: &str) -> String {
    let SplitPath { prefix, parts, absolute } = split_path(path, true);
    let key = if absolute { format!("{prefix}/{}", parts.join("/")) } else { parts.join("/") };
    if cfg!(windows) { key.to_lowercase() } else { key }
}

/// Whether `path` is folder `root` or lies somewhere inside it, however either is spelled
/// (see [`folder_key`]).
pub fn folder_within(path: &str, root: &str) -> bool {
    key_within(&folder_key(path), &folder_key(root))
}

/// The names below folder `root` that lead to `path`, as the caller spelled them (`None` when
/// `path` is not at or inside `root`; empty when it is `root`). Compared by [`folder_key`], but
/// the names keep their case, which the key lowers on Windows.
pub fn folder_rest(path: &str, root: &str) -> Option<Vec<String>> {
    if !key_within(&folder_key(path), &folder_key(root)) {
        return None;
    }
    let depth = split_path(root, false).parts.len();
    Some(split_path(path, false).parts.into_iter().skip(depth).collect())
}

/// Whether a library photo (not one only browsed in Local) lies in the folder whose
/// [`folder_key`] is `root`. A plain POSIX path (no `\`, `.`, `..`, repeated or trailing
/// separators) is its own identity, so it is read without building a key for it: the check runs
/// for every photo of the library on each refresh, and this keeps it cheap.
pub(crate) fn photo_in_root(p: &Photo, root: &str) -> bool {
    if p.local {
        return false;
    }
    let crate::Source::File { path } = &p.source else { return false };
    let plain = path.starts_with('/')
        && !path.starts_with("//")
        && !path.ends_with('/')
        && !path.contains(['\\'])
        && !path.contains("//")
        && !path.contains("/.");
    if !cfg!(windows) && plain && root.starts_with('/') && !root.starts_with("//") {
        let r = root.trim_end_matches('/');
        return path.strip_prefix(r).is_some_and(|rest| rest.starts_with('/'));
    }
    crate::local::folder_of(p).is_some_and(|f| key_within(&f, root))
}

/// [`folder_within`] for two paths already turned into their [`folder_key`].
pub(crate) fn key_within(p: &str, r: &str) -> bool {
    // `.` or `a/..` name no folder: not "everything"
    if r.is_empty() {
        return false;
    }
    // a disk's root key ends in `/` (`c:/`, `//srv/share/`), a folder's does not: same folder
    let (p, r) = (p.trim_end_matches('/'), r.trim_end_matches('/'));
    p == r || p.strip_prefix(r).is_some_and(|rest| rest.starts_with('/'))
}

/// The kind of Photo Merge result a file is, from the name merges give it (`IMG_1-HDR.dng`,
/// `IMG_1-Pano.dng`, `IMG_1-HDR-Pano.dng` — also the names other raw editors use):
/// `hdr`, `panorama` or `hdrPanorama`.
pub fn merged_kind(file_name: &str) -> Option<&'static str> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s).to_ascii_lowercase();
    let stem = stem.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-' || c == ' ');
    if stem.ends_with("-hdr-pano") {
        Some("hdrPanorama")
    } else if stem.ends_with("-hdr") {
        Some("hdr")
    } else if stem.ends_with("-pano") {
        Some("panorama")
    } else {
        None
    }
}
