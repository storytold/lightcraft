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

/// How a metadata filter's multi-select (the filter bar's pickers, the Metadata columns) combines its values
/// (issue #683): `Any` matches photos with at least one of them (OR, the default), `All` only photos with
/// every one of them (AND).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchOp {
    #[default]
    Any,
    All,
}

/// What the grid shows. Empty/None fields don't filter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Filter {
    /// Free-text search (plug-in metadata fields too; `key:value` for one): every token must match filename, title, caption, keywords, camera, lens,
    /// location or format. Tokens like `rating:3`, `flag:pick`, `iso:>800`, `camera:x2` are fielded.
    pub text: String,
    pub rating: u8,
    pub rating_op: RatingOp,
    pub flag: Option<Flag>,
    /// Any of these flags (Hide Rejected Photos shows picks and unflagged ones); empty = no
    /// constraint. ANDs with `flag`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<Flag>,
    pub label: Option<ColorLabel>,
    /// Only these photos (Find Similar results…); empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<PhotoId>,
    /// Show nothing: an explicit empty result (Find Similar with no matches), since an empty
    /// `only` means "no constraint".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub only_none: bool,
    /// Natural-language (semantic) search query (`LR-FILT-SEARCH-AI`). The matching photo IDs
    /// (in descending cosine-similarity order) are stored in `only` / `only_none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<String>,
    /// Any of these labels (the filter bar's multi-select); empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<ColorLabel>,
    /// Only photos with no colour label (the filter bar's hollow swatch, the Classic menu's "No
    /// Label"); ANDs with `label` / `labels`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub labels_none: bool,
    pub kind: Option<MediaKind>,
    /// Virtual copies only (`Some(true)`) or master photos only (`Some(false)`); Classic's Filter by Kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<bool>,
    pub edited: Option<bool>,
    pub album: Option<AlbumId>,
    /// Show "Recently Deleted" instead of the library.
    pub deleted: bool,
    /// Keep only the library's photos (`Some(true)`) or only the ones outside it — browsed
    /// (Local), deleted or demo photos (`Some(false)`).
    pub in_library: Option<bool>,
    /// Capture date prefix (`2026`, `2026-04`, `2026-04-12`).
    pub date: Option<String>,
    /// A folder the library's photos were imported from: those photos and the ones imported from
    /// the folders inside it (see [`crate::folders`]). Photos only browsed in Local are never part
    /// of it, unlike [`Filter::folder`].
    pub library_folder: Option<String>,
    /// With [`Filter::library_folder`]: only the photos imported from that very folder, not from
    /// the folders inside it (the Folders panel's Show Photos in Subfolders turned off).
    pub library_folder_only: bool,
    /// A keyword; hierarchical keywords match their children too (`travel` finds `travel|italy`).
    pub keyword: Option<String>,
    /// A person: photos with a named face region of this name (case-insensitive), as read from XMP.
    pub person: Option<String>,
    pub camera: Option<String>,
    /// Lens (case-insensitive substring).
    pub lens: Option<String>,
    /// Any of these keywords, each matched as [`Filter::keyword`] (the filter bar's multi-select); empty = no
    /// constraint. Together with `keyword`, both must match.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Any of these people, each matched as [`Filter::person`]; empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<String>,
    /// Any of these cameras, each matched as [`Filter::camera`]; empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<String>,
    /// Any of these lenses, each matched as [`Filter::lens`]; empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lenses: Vec<String>,
    /// How [`Filter::keywords`] combine (issue #683): `Any` of them (OR, the default) or `All` of them (AND).
    pub keyword_op: MatchOp,
    /// How [`Filter::people`] combine (issue #683): `Any` of them (OR, the default) or `All` of them (AND).
    pub person_op: MatchOp,
    /// How [`Filter::lenses`] combine (issue #683): `Any` of them (OR, the default) or `All` of them (AND).
    pub lens_op: MatchOp,
    /// Capture date range, inclusive: `dateFrom` is compared as a lower bound (`2026-04-01`),
    /// `dateTo` as a prefix upper bound (`2026-04` includes all of April).
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    /// Import date prefix.
    pub imported: Option<String>,
    /// Imported at or after this time (ISO; Recently Added).
    pub imported_from: Option<String>,
    /// Photo Merge results: `hdr`, `panorama`, `hdrPanorama`, `focusStack` or `any` (see [`merged_kind`]).
    pub merged: Option<String>,
    /// Immich link state: `linked`, `notLinked` (no confirmed link) or `probable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub immich: Option<String>,
    /// Vignetting Calibration reference ([`vignetting_reference_is`]): `reference` (in use) or `none` (not a
    /// reference, or a withdrawn one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vignetting_reference: Option<String>,
    /// The Image Quality Scanner's verdict (issue #1313): `accepted`, `pending`, `rejected`, or
    /// `unscanned` for the photos no scan has scored. `None` = no constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// The quality score's floor (`qualityMin`) and ceiling (`qualityMax`), inclusive, 0..100: the
    /// filter bar's slider. A photo no scan has scored matches neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_min: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_max: Option<f32>,
    /// A folder on disk: its files only (browsed ones too); `subfolders` includes everything below.
    pub folder: Option<String>,
    pub subfolders: bool,
    /// Detected face count filter: exact count ("0", "1", "2"), minimum ("3+", ">=2", ">0"),
    /// maximum ("<=2", "<3"), or range ("1-5", "2..4"). Accepts strings or numbers in JSON.
    #[serde(default, deserialize_with = "deserialize_faces", skip_serializing_if = "Option::is_none")]
    pub faces: Option<String>,
    /// Smart-album rules (all / any / none, nested groups; see [`crate::rules`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_set: Option<crate::RuleSet>,
    /// Metadata column picks on fields without a filter field of their own (file format, IPTC text, aspect,
    /// preset, AI tools), and "no value" picks on every field (issue #777); each must let the photo through.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<crate::FacetPick>,
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
    /// Custom Order: the order set by hand in a regular album ([`Catalog::album_photo_order`]).
    /// Anywhere else (no album, a smart album) it sorts by capture date.
    Custom,
    /// The Image Quality Scanner's score (issue #1313): photos it has not scored go first in
    /// ascending order (and last descending), as they carry no number to sort by.
    Quality,
}

impl SortKey {
    /// Whether photos that tie on this key go by file name before id: every key but the name itself and the
    /// shuffle (whose ranks come from ids and never tie).
    fn ties_by_name(self) -> bool {
        !matches!(self, SortKey::FileName | SortKey::Random)
    }
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

impl Sort {
    /// Which of two photos comes first in this order: the order [`Catalog::query`] gives (never `Equal` for two
    /// photos: the id decides last). Photos that tie on the key (one import batch, a burst within a second,
    /// equal ratings) go by file name, the order cameras number frames, so the order doesn't depend on how ids
    /// were given out. [`SortKey::Custom`] orders as the capture date here, as it does where there is no album's
    /// own order; an album's Custom Order is the album's, not the photos'.
    pub fn order(&self, a: &Photo, b: &Photo) -> std::cmp::Ordering {
        let o = match self.key {
            SortKey::CaptureDate | SortKey::Custom => a.captured.as_deref().cmp(&b.captured.as_deref()).then_with(|| a.imported.cmp(&b.imported)),
            SortKey::ImportDate => a.imported.cmp(&b.imported),
            SortKey::EditDate => a.edited.as_deref().cmp(&b.edited.as_deref()),
            // (as `sort_photos` keys them: the name lower-cased as a whole)
            SortKey::FileName => a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()),
            SortKey::Rating => a.rating.cmp(&b.rating),
            SortKey::FileSize => a.file_size.cmp(&b.file_size),
            SortKey::Random => shuffle_rank(self.seed, a.id).cmp(&shuffle_rank(self.seed, b.id)),
            SortKey::Quality => {
                let qa = a.analysis.and_then(|x| x.quality).map(|q| q.score);
                let qb = b.analysis.and_then(|x| x.quality).map(|q| q.score);
                match (qa, qb) {
                    (Some(x), Some(y)) => x.total_cmp(&y),
                    (None, None) => std::cmp::Ordering::Equal,
                    // unscanned photos have no place among scores: first when ascending
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (Some(_), None) => std::cmp::Ordering::Greater,
                }
            }
        }
        .then_with(|| if self.key.ties_by_name() { cmp_name(&a.file_name, &b.file_name) } else { std::cmp::Ordering::Equal })
        .then(a.id.cmp(&b.id));
        if self.ascending { o } else { o.reverse() }
    }
}

impl Default for Sort {
    /// Capture date, oldest first: Lightroom Classic's default sort, in one grid without date sections as its Grid
    /// (the fork's: grouping is a choice in the Sort menu).
    fn default() -> Self {
        Sort { key: SortKey::CaptureDate, ascending: true, group: crate::GroupBy::None, seed: 0 }
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

/// The whole catalog in one sort order, ascending (descending is its exact reverse: file names,
/// then ids, break every tie).
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
/// Capture date, import date and random orders survive everything but added or removed photos,
/// capture-time changes and renames (file names break their ties), so editing never re-sorts
/// them. At most two orders are kept (about 12 bytes a photo each).
#[derive(Default)]
pub(crate) struct SortCache {
    /// Photos added or removed, a capture time changed, or a file renamed.
    structural: u64,
    /// Any other sort key changed (rating, edit time, file size).
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
    /// An op is about to change sort keys: `structural` = which photos exist, a capture time or a file name.
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
    /// [`SKey::of`] the string in lower case ([`cmp_name`]'s order: UTF-8 bytes order as the characters do),
    /// without making the lower-case string.
    fn of_lower(s: &str) -> SKey {
        let mut b = [0u8; 24];
        let mut len = 0usize;
        for c in s.chars().flat_map(char::to_lowercase) {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                if let Some(d) = b.get_mut(len).filter(|_| len < 23) {
                    *d = byte;
                }
                len += 1;
            }
            if len >= 24 {
                break;
            }
        }
        b[23] = 1 + len.min(24) as u8;
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

/// Order photos by `sort`: [`Sort::order`] (ties broken by file name, then id). Keys are computed
/// once per photo and held inline, so comparisons rarely follow a pointer (one `to_lowercase` per
/// file name, not one per comparison).
fn sort_photos<'a>(v: Vec<&'a Photo>, sort: &Sort) -> Vec<PhotoId> {
    type Get = fn(&Photo) -> Option<&str>;
    struct E<'a> {
        k1: SKey,
        k2: SKey,
        num: u64,
        /// The file name in lower case, for ties (empty when the key doesn't tie by name).
        name: SKey,
        id: u64,
        p: &'a Photo,
    }
    fn none(_: &Photo) -> Option<&str> {
        None
    }
    let asc = sort.ascending;
    let seed = sort.seed;
    let (s1, s2, num): (Get, Get, fn(&Photo, u64) -> u64) = match sort.key {
        SortKey::CaptureDate | SortKey::Custom => (|p| p.captured.as_deref(), |p| Some(p.imported.as_str()), |_, _| 0),
        SortKey::ImportDate => (|p| Some(p.imported.as_str()), none, |_, _| 0),
        SortKey::EditDate => (|p| p.edited.as_deref(), none, |_, _| 0),
        SortKey::Rating => (none, none, |p, _| u64::from(p.rating)),
        SortKey::FileSize => (none, none, |p, _| p.file_size),
        SortKey::Random => (none, none, |p, seed| shuffle_rank(seed, p.id)),
        // Unscanned photos rank below every score (first ascending, last descending), as
        // `Sort::order` puts them; the score is kept to a thousandth (the stored score's
        // precision) and one higher, so a measured 0 stays above "no score at all".
        SortKey::Quality => (none, none, |p, _| match p.analysis.and_then(|a| a.quality) {
            Some(q) if q.score.is_finite() => (q.score.clamp(0.0, 100.0) * 1_000.0).round() as u64 + 1,
            _ => 0,
        }),
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
    let by_name = sort.key.ties_by_name();
    let make = |p: &&'a Photo| E {
        k1: SKey::of(s1(p)),
        k2: SKey::of(s2(p)),
        num: num(p, seed),
        name: if by_name { SKey::of_lower(&p.file_name) } else { SKey::default() },
        id: p.id.0,
        p,
    };
    let keyed: Vec<E> = par_map_photos(&v, &make);
    let cmp = |a: &E, b: &E| {
        let o =
            a.k1.cmp_or(&b.k1, s1(a.p), s1(b.p))
                .then_with(|| a.k2.cmp_or(&b.k2, s2(a.p), s2(b.p)))
                .then(a.num.cmp(&b.num))
                .then_with(|| {
                    let o = a.name.cmp(&b.name);
                    if o.is_eq() && a.name.2 & 0xff == 25 { cmp_name(&a.p.file_name, &b.p.file_name) } else { o }
                })
                .then(a.id.cmp(&b.id));
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

/// Whether photo `id` is what `want` says to Vignetting Calibration: `reference` (of a map in use) or `none` (not a
/// reference; a withdrawn reference counts as none). Derived from the catalog's map table
/// ([`Catalog::is_vignetting_reference`]), so it can't be stale. Unknown values match nothing.
pub fn vignetting_reference_is(cat: &Catalog, id: PhotoId, want: &str) -> bool {
    match want {
        "reference" => cat.is_vignetting_reference(id),
        "none" => !cat.is_vignetting_reference(id),
        _ => false,
    }
}

/// What [`Filter::prepare`] works out once for a pass over many photos.
pub(crate) struct Prepared {
    root: Option<String>,
    only: Option<std::collections::HashSet<PhotoId>>,
    /// A pass over many photos: the catalog's kept search text is made when missing.
    whole: bool,
}

/// A photo's place in the shuffle for `seed`.
fn shuffle_rank(seed: u64, id: PhotoId) -> u64 {
    mix64(mix64(seed) ^ id.0)
}

/// Reads `faces` as a count (`2`) or a spec string, rejecting a spec [`parse_face_spec`] can't read
/// so a typo is an error the caller sees rather than an empty grid.
fn deserialize_faces<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum FaceCountVal {
        Num(u64),
        Str(String),
    }

    let spec = match Option::<FaceCountVal>::deserialize(deserializer)? {
        None => return Ok(None),
        Some(FaceCountVal::Num(n)) => n.to_string(),
        Some(FaceCountVal::Str(s)) => s.trim().to_string(),
    };
    if spec.is_empty() {
        return Ok(None);
    }
    parse_face_spec(&spec).map_err(serde::de::Error::custom)?;
    Ok(Some(spec))
}

/// A face-count condition: at least `min`, at most `max` (inclusive) when set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaceSpec {
    pub min: usize,
    pub max: Option<usize>,
}

impl FaceSpec {
    pub fn matches(self, count: usize) -> bool {
        count >= self.min && self.max.is_none_or(|max| count <= max)
    }
}

/// Parses a face-count spec:
/// - exact: `"0"`, `"2"`, `"=2"`, `"none"` (0)
/// - minimum: `"3+"`, `"+3"`, `">=3"`, `">2"`, `"any"` (1 or more)
/// - maximum: `"<=2"`, `"<3"`
/// - range: `"1-10"`, `"1–10"`, `"1..10"`, `"1..=10"` (both ends included; `"5-2"` means 2–5)
///
/// Anything else, and `"<0"` (which nothing matches), is an error naming the accepted forms.
pub fn parse_face_spec(spec: &str) -> Result<FaceSpec, String> {
    let s = spec.trim();
    let num = |t: &str| t.trim().parse::<usize>().ok();
    let exact = |v: usize| FaceSpec { min: v, max: Some(v) };
    let at_least = |v: usize| FaceSpec { min: v, max: None };
    let parsed = if s.eq_ignore_ascii_case("none") {
        Some(exact(0))
    } else if s.eq_ignore_ascii_case("any") {
        Some(at_least(1))
    } else if let Some(v) = s.strip_suffix('+').and_then(num).or_else(|| s.strip_prefix('+').and_then(num)) {
        Some(at_least(v))
    } else if let Some(rest) = s.strip_prefix(">=") {
        num(rest).map(at_least)
    } else if let Some(rest) = s.strip_prefix("<=") {
        num(rest).map(|v| FaceSpec { min: 0, max: Some(v) })
    } else if let Some(rest) = s.strip_prefix('>') {
        num(rest).and_then(|v| v.checked_add(1)).map(at_least)
    } else if let Some(rest) = s.strip_prefix('<') {
        num(rest).and_then(|v| v.checked_sub(1)).map(|v| FaceSpec { min: 0, max: Some(v) })
    } else if let Some(rest) = s.strip_prefix('=') {
        num(rest).map(exact)
    } else if let Some((a, b)) = ["..=", "..", "–", "-"].iter().find_map(|sep| s.split_once(sep)) {
        match (num(a), num(b)) {
            (Some(a), Some(b)) => Some(FaceSpec { min: a.min(b), max: Some(a.max(b)) }),
            _ => None,
        }
    } else {
        num(s).map(exact)
    };
    parsed.ok_or_else(|| {
        format!("faces: can't read {spec:?}; use a count (2), a minimum (3+, >=3, >2), a maximum (<=2, <3), a range (1-5) or none / any")
    })
}

/// Whether a photo's detected face count matches `spec` (see [`parse_face_spec`]); a spec that
/// can't be read matches nothing.
pub fn matches_face_spec(count: usize, spec: &str) -> bool {
    parse_face_spec(spec).is_ok_and(|f| f.matches(count))
}

/// The tokens of a search text: split at whitespace, except inside double quotes, so
/// `person:"Jane Doe"` is one token (issue #1449). An unclosed quote runs to the end of the text; a
/// backslash is an ordinary character. One pass over the text, nothing allocated.
fn search_tokens(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        let mut quoted = false;
        let end = rest
            .char_indices()
            .find(|&(_, c)| {
                quoted ^= c == '"';
                !quoted && c.is_whitespace()
            })
            .map_or(rest.len(), |(i, _)| i);
        let (tok, tail) = rest.split_at(end);
        rest = tail;
        Some(tok)
    })
}

/// `s` without the double quotes around it (a missing closing quote is allowed).
fn unquote(s: &str) -> &str {
    s.strip_prefix('"').map_or(s, |inner| inner.strip_suffix('"').unwrap_or(inner))
}

fn token_matches(p: &Photo, tok: &str, kept: Option<&[Box<str>]>) -> bool {
    let t = tok.to_lowercase();
    // a token that starts with a quote is a phrase, even with a colon in it
    if let Some((field, val)) = t.split_once(':').filter(|_| !t.starts_with('"')) {
        let val = unquote(val);
        let num = |s: &str| -> Option<(&str, f64)> {
            // `>=` / `<=` before `>` / `<`: `rating:>=1` is at least one star
            let op = [">=", "<=", ">", "<", "="].into_iter().find(|op| s.starts_with(op)).unwrap_or("=");
            let rest = s.strip_prefix(op).unwrap_or(s);
            rest.parse().ok().map(|v| (op, v))
        };
        let cmp = |x: f64, s: &str| match num(s) {
            Some((">=", v)) => x >= v - 1e-9,
            Some(("<=", v)) => x <= v + 1e-9,
            Some((">", v)) => x > v,
            Some(("<", v)) => x < v,
            Some((_, v)) => (x - v).abs() < 1e-9,
            None => false,
        };
        // Fields stored as `f32` compare at f32 precision: 2.8f32 widened to f64 is not 2.8.
        let cmp32 = |x: f32, s: &str| match num(s) {
            Some((">=", v)) => x >= v as f32,
            Some(("<=", v)) => x <= v as f32,
            Some((">", v)) => x > v as f32,
            Some(("<", v)) => x < v as f32,
            Some((_, v)) => x == v as f32,
            None => false,
        };
        // only the words the field is documented with; any other value matches nothing
        let boolean = |s: &str| -> Option<bool> {
            match s {
                "true" | "yes" => Some(true),
                "false" | "no" => Some(false),
                _ => None,
            }
        };
        return match field {
            "rating" | "stars" => cmp(p.rating as f64, val),
            "flag" => Flag::parse(val) == Some(p.flag),
            "label" | "color" => {
                if val.eq_ignore_ascii_case("none") || val.eq_ignore_ascii_case("no") {
                    p.label.is_none()
                } else {
                    p.label.is_some_and(|l| format!("{l:?}").eq_ignore_ascii_case(val))
                }
            }
            "iso" => p.meta.iso.is_some_and(|i| cmp(i as f64, val)),
            "f" | "aperture" => p.meta.aperture.is_some_and(|a| cmp32(a, val)),
            "focal" => p.meta.focal_mm.is_some_and(|a| cmp32(a, val)),
            "camera" => p.meta.camera.to_lowercase().contains(val),
            "lens" => p.meta.lens.to_lowercase().contains(val),
            "keyword" | "kw" => p.meta.keywords.iter().any(|k| crate::keywords::is_under(k, val)),
            "person" | "who" => has_person(p, val),
            "faces" | "face" | "facecount" => matches_face_spec(p.face_count(), val),
            "type" | "kind" => format!("{:?}", p.kind).eq_ignore_ascii_case(val),
            "video" => boolean(val).is_some_and(|video| video == (p.kind == MediaKind::Video)),
            "edited" => boolean(val).is_some_and(|edited| edited == p.is_edited()),
            "date" => p.captured.as_deref().is_some_and(|c| {
                // the token is lower-cased, the stored time has an upper-case `T`
                crate::dates::capture_start(c).get(..val.len()).is_some_and(|head| head.eq_ignore_ascii_case(val))
            }),
            "copy" | "virtual" => boolean(val).is_some_and(|copy| copy == p.copy_of.is_some()),
            "name" | "file" => p.file_name.to_lowercase().contains(val),
            // a plug-in metadata field by its key (`job:J-77`), whichever plug-in has it; and, as
            // it isn't a known field, a plain word with the colon in it ("10:30", "Re:")
            key => {
                p.plugin_fields.iter().any(|(k, v)| {
                    k.rsplit_once('/').is_some_and(|(_, f)| f.eq_ignore_ascii_case(key))
                        && crate::rules::plugin_value_text(v).to_lowercase().contains(val)
                }) || text_matches(p, &t, kept)
            }
        };
    }
    text_matches(p, unquote(&t), kept)
}

/// The lower-cased fields the free-text search looks through ([`text_matches`]), each on its own.
pub(crate) fn search_fields_of(p: &Photo) -> Box<[Box<str>]> {
    let fields = [&p.file_name, &p.meta.title, &p.meta.caption, &p.meta.camera, &p.meta.lens, &p.meta.location, &p.format];
    fields
        .into_iter()
        .map(|f| f.to_lowercase())
        .chain(p.meta.keywords.iter().map(|k| k.to_lowercase()))
        .chain(p.people().iter().map(|n| n.to_lowercase()))
        .chain(p.plugin_fields.values().map(|v| crate::rules::plugin_value_text(v).to_lowercase()))
        .filter(|f| !f.is_empty())
        .map(String::into_boxed_str)
        .collect()
}

/// Whether the lower-cased word `t` is in the file name, title, caption, camera, lens, location,
/// format, a keyword, a person's name or a plug-in field value of `p` (`kept`: those fields,
/// lower-cased already by the catalog).
fn text_matches(p: &Photo, t: &str, kept: Option<&[Box<str>]>) -> bool {
    if let Some(fields) = kept {
        return t.is_empty() || fields.iter().any(|f| f.contains(t));
    }
    let hay = [&p.file_name, &p.meta.title, &p.meta.caption, &p.meta.camera, &p.meta.lens, &p.meta.location, &p.format];
    hay.iter().any(|h| h.to_lowercase().contains(t))
        || p.meta.keywords.iter().any(|k| k.to_lowercase().contains(t))
        || p.people().iter().any(|n| n.to_lowercase().contains(t))
        || p.plugin_fields.values().any(|v| crate::rules::plugin_value_text(v).to_lowercase().contains(t))
}

/// Whether `p` has a named face region called `name` (case-insensitive).
fn has_person(p: &Photo, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    p.people().iter().any(|n| n.to_lowercase() == name)
}

/// How many photos of a view carry each culling mark: the filter bar's live counts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tally {
    pub total: usize,
    pub picks: usize,
    pub rejects: usize,
    pub unflagged: usize,
    /// Photos with exactly 0, 1, … 5 stars.
    pub stars: [usize; 6],
    /// Photos per colour label, in [`ColorLabel::ALL`] order.
    pub labels: [usize; 5],
    pub unlabeled: usize,
    /// The Image Quality Scanner's verdicts (issue #1313): accepted, pending and rejected photos,
    /// and how many no scan has scored.
    pub quality_accepted: usize,
    pub quality_pending: usize,
    pub quality_rejected: usize,
    pub quality_unscanned: usize,
}

impl Tally {
    /// Photos with at least `n` stars.
    pub fn at_least(&self, n: u8) -> usize {
        self.stars.iter().skip(usize::from(n.min(5))).sum()
    }

    /// Photos with `label`.
    pub fn label(&self, label: ColorLabel) -> usize {
        ColorLabel::ALL.iter().position(|x| *x == label).and_then(|i| self.labels.get(i).copied()).unwrap_or(0)
    }

    /// Photos with `flag`.
    pub fn flag(&self, flag: Flag) -> usize {
        match flag {
            Flag::Pick => self.picks,
            Flag::Reject => self.rejects,
            Flag::None => self.unflagged,
        }
    }
}

impl Filter {
    /// This filter without the quick culling filters (stars, flags, colour labels): what the filter
    /// bar counts its photos in, so "Picked 47" stays readable while only the picks are shown.
    pub fn without_quick(&self) -> Filter {
        Filter {
            rating: 0,
            rating_op: RatingOp::default(),
            flag: None,
            flags: Vec::new(),
            label: None,
            labels: Vec::new(),
            labels_none: false,
            ..self.clone()
        }
    }

    /// The flags that `flag` and `flags` let through together; `None` when neither is set.
    pub fn flags_shown(&self) -> Option<Vec<Flag>> {
        match (self.flag, self.flags.is_empty()) {
            (None, true) => None,
            (Some(f), true) => Some(vec![f]),
            (f, false) => Some(self.flags.iter().copied().filter(|x| f.is_none_or(|f| f == *x)).collect()),
        }
    }

    /// The labels that `label` and `labels` let through together (they AND), in `labels`' order;
    /// `None` when neither is set, empty when they exclude each other. The chip and the filter
    /// bar's buttons show this, so they match what is filtered.
    pub fn labels_shown(&self) -> Option<Vec<ColorLabel>> {
        match (self.label, self.labels.is_empty()) {
            (None, true) => None,
            (Some(l), true) => Some(vec![l]),
            (l, false) => Some(self.labels.iter().copied().filter(|x| l.is_none_or(|l| l == *x)).collect()),
        }
    }

    /// Human-readable summary of the active rules (smart album tooltips, `album.list`).
    pub fn describe(&self) -> String {
        self.describe_in(None)
    }

    /// [`Filter::describe`] naming the albums its rules test (`RuleSet::describe_with`).
    pub fn describe_with(&self, cat: &crate::Catalog) -> String {
        self.describe_in(Some(cat))
    }

    /// Whether the rating rule narrows the view. Only "at least no stars" lets every photo through:
    /// "exactly" or "at most" no stars shows the unrated photos, as in Lightroom's filter bar.
    pub fn rating_filters(&self) -> bool {
        self.rating > 0 || self.rating_op != RatingOp::AtLeast
    }

    fn describe_in(&self, cat: Option<&crate::Catalog>) -> String {
        let mut v: Vec<String> = Vec::new();
        if self.rating_filters() {
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
        if !self.flags.is_empty() {
            v.push(format!("flag {}", self.flags.iter().map(|f| format!("{f:?}").to_lowercase()).collect::<Vec<_>>().join(" or ")));
        }
        if let Some(l) = self.label {
            v.push(format!("label {}", format!("{l:?}").to_lowercase()));
        }
        if !self.labels.is_empty() {
            v.push(format!("label {}", self.labels.iter().map(|l| format!("{l:?}").to_lowercase()).collect::<Vec<_>>().join(" or ")));
        }
        if self.labels_none {
            v.push("label none".into());
        }
        if let Some(k) = self.kind {
            v.push(format!("kind {}", format!("{k:?}").to_lowercase()));
        }
        if let Some(c) = self.copy {
            v.push(if c { "virtual copies".into() } else { "master photos".into() });
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
        for (name, list, op) in [
            ("keyword", &self.keywords, self.keyword_op),
            ("person", &self.people, self.person_op),
            // a photo has one camera: several of them can only mean any of them
            ("camera", &self.cameras, MatchOp::Any),
            ("lens", &self.lenses, self.lens_op),
        ] {
            if !list.is_empty() {
                v.push(format!("{name} {}", list.join(if op == MatchOp::All { " and " } else { " or " })));
            }
        }
        match (&self.date_from, &self.date_to) {
            (Some(a), Some(b)) => v.push(format!("captured {a} – {b}")),
            (Some(a), None) => v.push(format!("captured from {a}")),
            (None, Some(b)) => v.push(format!("captured until {b}")),
            _ => {}
        }
        for f in self.facets.iter().filter(|f| !f.is_empty()) {
            let mut any: Vec<&str> = f.values.iter().map(String::as_str).collect();
            if f.none {
                any.push("none");
            }
            v.push(format!("{} {}", f.field, any.join(" or ")));
        }
        if let Some(fcs) = &self.faces {
            v.push(format!("faces {fcs}"));
        }
        if let Some(r) = &self.vignetting_reference {
            v.push(match r.as_str() {
                "reference" => "vignetting reference".to_string(),
                "none" => "not a vignetting reference".to_string(),
                other => format!("vignetting reference {other}"),
            });
        }
        if let Some(q) = &self.quality {
            v.push(match q.as_str() {
                "unscanned" => "not quality scanned".to_string(),
                other => format!("quality {other}"),
            });
        }
        match (self.quality_min, self.quality_max) {
            (Some(a), Some(b)) => v.push(format!("quality score {a:.0}–{b:.0}")),
            (Some(a), None) => v.push(format!("quality score at least {a:.0}")),
            (None, Some(b)) => v.push(format!("quality score at most {b:.0}")),
            (None, None) => {}
        }
        if !self.text.trim().is_empty() {
            v.push(format!("“{}”", self.text.trim()));
        }
        if let Some(q) = self.semantic.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            v.push(format!("AI search “{q}”"));
        }
        if let Some(rs) = self.rule_set.as_ref().filter(|r| !r.rules.is_empty()) {
            v.push(cat.map_or_else(|| rs.describe(), |c| rs.describe_with(c)));
        }
        if v.is_empty() { "all photos".into() } else { v.join(", ") }
    }

    /// The albums this filter tests: its album field and its rules' Album rules.
    pub fn albums_tested(&self) -> Vec<crate::AlbumId> {
        let mut out: Vec<crate::AlbumId> = self.album.into_iter().collect();
        out.extend(self.rule_set.as_ref().map(crate::RuleSet::albums_tested).unwrap_or_default());
        out
    }

    /// Whether matches depend on the clock (only "in the last…" rules do).
    pub fn depends_on_now(&self) -> bool {
        self.rule_set.as_ref().is_some_and(crate::RuleSet::depends_on_now)
    }

    pub fn matches(&self, p: &Photo, cat: &Catalog) -> bool {
        self.matches_in(p, cat, &self.prepare_one())
    }

    /// [`Filter::prepare`] for testing one photo (the `only` ids are looked through, not hashed).
    pub(crate) fn prepare_one(&self) -> Prepared {
        Prepared { root: self.library_root(), only: None, whole: false }
    }

    /// What [`Filter::matches_in`] needs worked out once for a pass over many photos rather than
    /// once per photo: the library folder's identity and the `only` ids as a set.
    pub(crate) fn prepare(&self) -> Prepared {
        Prepared { root: self.library_root(), only: (!self.only.is_empty()).then(|| self.only.iter().copied().collect()), whole: true }
    }

    /// [`Filter::library_folder`] as a folder identity ([`folder_key`]); `None` without a choice.
    fn library_root(&self) -> Option<String> {
        // as given: a folder may be named with spaces at its edge, and trimming would make it
        // another folder
        self.library_folder.as_deref().filter(|d| !d.trim().is_empty()).map(folder_key)
    }

    /// [`Filter::matches`] with what [`Filter::prepare`] works out done already, so a query over a
    /// big library does that once, not once per photo.
    pub(crate) fn matches_in(&self, p: &Photo, cat: &Catalog, prepared: &Prepared) -> bool {
        if p.deleted != self.deleted {
            return false;
        }
        if self.in_library.is_some_and(|want| p.in_library() != want) {
            return false;
        }
        match &self.folder {
            // browsed photos only show in folder views, and in Recently Deleted once deleted (#1430)
            None if p.local && !p.deleted => return false,
            None => {}
            Some(dir) => {
                let crate::Source::File { path } = &p.source else { return false };
                if !in_folder(path, dir, self.subfolders) {
                    return false;
                }
            }
        }
        if self.rating_filters() {
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
        if self.copy.is_some_and(|c| c != p.copy_of.is_some()) {
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
        if !self.flags.is_empty() && !self.flags.contains(&p.flag) {
            return false;
        }
        if !self.labels.is_empty() && !p.label.is_some_and(|l| self.labels.contains(&l)) {
            return false;
        }
        if self.labels_none && p.label.is_some() {
            return false;
        }
        if !self.facets.iter().all(|f| f.matches(p, cat)) {
            return false;
        }
        if self.only_none {
            return false;
        }
        if !self.only.is_empty() && !prepared.only.as_ref().map_or_else(|| self.only.contains(&p.id), |only| only.contains(&p.id)) {
            return false;
        }
        if let Some(want) = &self.immich
            && !immich_state_is(cat, p.id, want)
        {
            return false;
        }
        if let Some(want) = &self.vignetting_reference
            && !vignetting_reference_is(cat, p.id, want)
        {
            return false;
        }
        if let Some(want) = &self.quality {
            let scan = p.analysis.and_then(|a| a.quality);
            let ok = match want.as_str() {
                "unscanned" => scan.is_none(),
                other => scan.is_some_and(|q| q.verdict.as_str() == other),
            };
            if !ok {
                return false;
            }
        }
        if self.quality_min.is_some() || self.quality_max.is_some() {
            let Some(score) = p.analysis.and_then(|a| a.quality).map(|q| q.score) else { return false };
            if self.quality_min.is_some_and(|min| score < min) || self.quality_max.is_some_and(|max| score > max) {
                return false;
            }
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
        // a partial capture time (`1983-04`) stands for the start of its period
        if let Some(from) = &self.date_from
            && match p.captured.as_deref().map(crate::dates::capture_start) {
                Some(c) => *c < *from.as_str(),
                None => true,
            }
        {
            return false;
        }
        if let Some(to) = &self.date_to
            && match p.captured.as_deref().map(crate::dates::capture_start) {
                Some(c) => c.get(..to.len()).unwrap_or(&c) > to.as_str(),
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
            && !p.captured.as_deref().is_some_and(|c| crate::dates::capture_start(c).starts_with(d.as_str()))
        {
            return false;
        }
        if let Some(root) = prepared.root.as_deref()
            && !(photo_in_root(p, root) && (!self.library_folder_only || photo_directly_in(p, root)))
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
        if let Some(fcs) = &self.faces
            && !matches_face_spec(p.face_count(), fcs)
        {
            return false;
        }
        // the multi-selects: each list's values combine by its operator (any = OR, all = AND, issue #683), and
        // every list must let the photo through
        let list_matches = |list: &[String], op: MatchOp, one: &dyn Fn(&str) -> bool| {
            list.is_empty()
                || match op {
                    MatchOp::Any => list.iter().any(|v| one(v)),
                    MatchOp::All => list.iter().all(|v| one(v)),
                }
        };
        if !list_matches(&self.keywords, self.keyword_op, &|k| p.meta.keywords.iter().any(|x| crate::keywords::is_under(x, k)))
            || !list_matches(&self.people, self.person_op, &|n| has_person(p, n))
            || !list_matches(&self.cameras, MatchOp::Any, &|c| p.meta.camera.eq_ignore_ascii_case(c))
            || !list_matches(&self.lenses, self.lens_op, &|l| p.meta.lens.to_lowercase().contains(&l.to_lowercase()))
        {
            return false;
        }
        let mut tokens = search_tokens(&self.text).peekable();
        if tokens.peek().is_none() {
            return true;
        }
        let kept = cat.search_fields(p, prepared.whole);
        tokens.all(|tok| token_matches(p, tok, kept))
    }
}

/// Case-insensitive file name order without allocating (compares per-character lowercase via `char::to_lowercase`).
pub fn cmp_name(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

impl Catalog {
    /// Photos matching `filter`, in `sort` order (ties broken by file name, then id).
    pub fn query(&self, filter: &Filter, sort: &Sort) -> Vec<PhotoId> {
        self.query_with(filter, sort, true)
    }

    /// [`Catalog::query`], and with them the photos of `keep` that still match `within`, each at its place in `sort`
    /// order: a view that holds the photos it showed when an edit makes them stop matching `filter` (a smart
    /// album's rules, the filter bar), as long as they are still where the view looks (`within`: in the library,
    /// the album, the folder; not deleted).
    pub fn query_keeping(&self, filter: &Filter, sort: &Sort, keep: &std::collections::HashSet<PhotoId>, within: &Filter) -> Vec<PhotoId> {
        self.query_inner(filter, sort, true, Some((keep, within)))
    }

    /// The list `was` (a [`Catalog::query`] of `filter` in `sort` order, or a [`Catalog::query_keeping`]), after ops
    /// that touched only the photos `touched` (sorted by id, each once: [`Catalog::touched_since`]): those photos
    /// taken out, and the ones that match `filter` now put back at their place in the order. With `within`, a
    /// touched photo that was in the list and still matches `within` stays, as `query_keeping` would keep it. What
    /// the query would give, without asking every photo again.
    pub fn follow(&self, was: &[PhotoId], touched: &[PhotoId], filter: &Filter, sort: &Sort, within: Option<&Filter>) -> Vec<PhotoId> {
        if sort.key == SortKey::Custom && filter.album.is_some_and(|a| self.has_custom_order(a)) {
            // an album's own order is the album's, not the photos': asked whole, keeping what the list held (the
            // untouched photos still match what they matched, or the ops would not have been followed)
            let keep: std::collections::HashSet<PhotoId> = was.iter().copied().collect();
            return self.query_keeping(filter, sort, &keep, within.unwrap_or(filter));
        }
        let listed: std::collections::HashSet<PhotoId> = was.iter().copied().filter(|id| touched.binary_search(id).is_ok()).collect();
        let mut ids: Vec<PhotoId> = was.iter().copied().filter(|id| !listed.contains(id)).collect();
        let (prepared, within_prepared) = (filter.prepare_one(), within.map(Filter::prepare_one));
        for id in touched {
            let Some(p) = self.photo(*id) else { continue };
            let kept = within.zip(within_prepared.as_ref()).is_some_and(|(w, wp)| listed.contains(id) && w.matches_in(p, self, wp));
            if filter.matches_in(p, self, &prepared) || kept {
                // (the list is in order without the touched photos, and stays so as each is put back)
                let at = ids.partition_point(|other| self.photo(*other).is_some_and(|o| sort.order(o, p).is_lt()));
                ids.insert(at, *id);
            }
        }
        ids
    }

    /// How many of the photos matching `filter` carry each culling mark (the filter bar's counts).
    pub fn tally(&self, filter: &Filter) -> Tally {
        let prepared = filter.prepare();
        let mut t = Tally::default();
        for p in self.photos().filter(|p| filter.matches_in(p, self, &prepared)) {
            t.total += 1;
            match p.flag {
                Flag::Pick => t.picks += 1,
                Flag::Reject => t.rejects += 1,
                Flag::None => t.unflagged += 1,
            }
            if let Some(n) = t.stars.get_mut(usize::from(p.rating.min(5))) {
                *n += 1;
            }
            match p.label.and_then(|l| ColorLabel::ALL.iter().position(|x| *x == l)) {
                Some(i) => {
                    if let Some(n) = t.labels.get_mut(i) {
                        *n += 1;
                    }
                }
                None => t.unlabeled += 1,
            }
            match p.analysis.and_then(|a| a.quality).map(|q| q.verdict) {
                Some(crate::QualityVerdict::Accepted) => t.quality_accepted += 1,
                Some(crate::QualityVerdict::Rejected) => t.quality_rejected += 1,
                Some(crate::QualityVerdict::Pending) => t.quality_pending += 1,
                None => t.quality_unscanned += 1,
            }
        }
        t
    }

    /// How many photos the Image Quality Scanner has scored (issue #1313).
    pub fn quality_scanned(&self) -> usize {
        self.photos().filter(|p| p.analysis.is_some_and(|a| a.quality.is_some())).count()
    }

    /// Whether any photo carries a quality score (issue #1313): what the filter bar asks each time
    /// the catalog changes, and stops at the first one.
    pub fn any_quality_scanned(&self) -> bool {
        self.photos().any(|p| p.analysis.is_some_and(|a| a.quality.is_some()))
    }

    /// How many photos match `filter`: [`Catalog::query`]'s length without its sorting.
    pub fn count(&self, filter: &Filter) -> usize {
        let prepared = filter.prepare();
        self.photos().filter(|p| filter.matches_in(p, self, &prepared)).count()
    }

    /// [`Self::query`], with or without the kept sort orders (tests compare both).
    pub(crate) fn query_with(&self, filter: &Filter, sort: &Sort, cached: bool) -> Vec<PhotoId> {
        self.query_inner(filter, sort, cached, None)
    }

    /// [`Self::query`], or [`Self::query_keeping`] with `keeping`.
    fn query_inner(
        &self,
        filter: &Filter,
        sort: &Sort,
        cached: bool,
        keeping: Option<(&std::collections::HashSet<PhotoId>, &Filter)>,
    ) -> Vec<PhotoId> {
        let prepared = filter.prepare();
        // `within` only decides for the few kept photos: worked out per photo, not hashed
        let within_prepared = keeping.map(|(_, within)| within.prepare_one());
        let keep = |p: &&Photo| {
            filter.matches_in(p, self, &prepared)
                || keeping.zip(within_prepared.as_ref()).is_some_and(|((kept, within), wp)| kept.contains(&p.id) && within.matches_in(p, self, wp))
        };
        // (a rule that reads the clock is not asked on other threads)
        let clocked = filter.depends_on_now() || keeping.is_some_and(|(_, within)| within.depends_on_now());
        if sort.key == SortKey::Custom {
            if let Some(a) = filter.album.filter(|a| self.has_custom_order(*a)) {
                let rank: std::collections::HashMap<PhotoId, usize> =
                    self.album_photo_order(a).into_iter().enumerate().map(|(i, p)| (p, i)).collect();
                let mut v: Vec<(usize, PhotoId)> =
                    self.photos().map(|p| p.as_ref()).filter(keep).map(|p| (rank.get(&p.id).copied().unwrap_or(usize::MAX), p.id)).collect();
                v.sort_unstable();
                if !sort.ascending {
                    v.reverse();
                }
                return v.into_iter().map(|(_, id)| id).collect();
            }
            // no regular album: by capture date
            return self.query_inner(filter, &Sort { key: SortKey::CaptureDate, ..*sort }, cached, keeping);
        }
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
            if all.len() >= PARALLEL_FROM && !clocked {
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
        let v: Vec<&Photo> = if all.len() >= PARALLEL_FROM && !clocked { par_filter(&all, &keep) } else { all.into_iter().filter(keep).collect() };
        sort_photos(v, sort)
    }

    /// Whether the album can have a Custom Order (a regular album, not a smart album or folder).
    pub fn has_custom_order(&self, id: AlbumId) -> bool {
        self.album(id).is_some_and(|a| !a.folder && a.smart.is_none())
    }

    /// The album's photos in Custom Order: [`crate::Album::photo_order`] (members only), then the
    /// photos it doesn't list in the order they were added. Empty for smart albums and folders.
    pub fn album_photo_order(&self, id: AlbumId) -> Vec<PhotoId> {
        let Some(a) = self.album(id).filter(|_| self.has_custom_order(id)) else { return Vec::new() };
        let members: std::collections::HashSet<PhotoId> = a.photos.iter().copied().collect();
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<PhotoId> = a.photo_order.iter().copied().filter(|p| members.contains(p) && seen.insert(*p)).collect();
        out.extend(a.photos.iter().copied().filter(|p| seen.insert(*p)));
        out
    }

    /// Year → month → day counts for the "By Date" section (newest first).
    pub fn date_groups(&self) -> Vec<DateGroup> {
        use std::borrow::Cow;
        use std::collections::BTreeMap;
        let mut years: BTreeMap<Cow<str>, (BTreeMap<Cow<str>, usize>, BTreeMap<Cow<str>, usize>)> = BTreeMap::new();
        for p in self.photos().filter(|p| p.in_library()) {
            let Some(d) = p.captured.as_deref().map(crate::dates::capture_start) else { continue };
            // `get`, not `[..]`: a corrupt or foreign `captured` (a Lightroom catalog's
            // captureTime is copied verbatim) may not be ASCII, and slicing it on a byte index
            // would panic. A partial time (`1983-04`) counts at the start of its period, so only
            // it owns its keys.
            let part = |n: usize| match &d {
                Cow::Borrowed(d) => d.get(..n).map(Cow::Borrowed),
                Cow::Owned(d) => d.get(..n).map(|k| Cow::Owned(k.to_string())),
            };
            if let (Some(year), Some(month), Some(day)) = (part(4), part(7), part(10)) {
                let (months, days) = years.entry(year).or_default();
                *months.entry(month).or_default() += 1;
                *days.entry(day).or_default() += 1;
            }
        }
        let newest_first = |m: BTreeMap<Cow<str>, usize>| m.into_iter().rev().map(|(k, n)| (k.to_string(), n)).collect::<Vec<_>>();
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

    /// [`Self::people`] among the photos `filter` lets through (its `person` and `people` are ignored): what the
    /// People view offers while other filters (a date, a rating, an album…) are active, so picking
    /// a person never ends in an empty grid.
    pub fn people_in(&self, filter: &Filter) -> Vec<Person> {
        let filter = Filter { person: None, people: Vec::new(), ..filter.clone() };
        let mut m: std::collections::HashMap<String, (Person, f64)> = Default::default();
        let prepared = filter.prepare();
        for p in self.photos().filter(|p| filter.matches_in(p, self, &prepared)) {
            let mut seen: Vec<String> = Vec::new();
            for r in p.meta.regions.iter().filter(|r| r.kind == lcc_meta::RegionKind::Face) {
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
    pub face: lcc_meta::Rect,
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

/// Whether `p` lies in the folder `root` itself (a [`folder_key`]), not in a folder inside it.
pub(crate) fn photo_directly_in(p: &Photo, root: &str) -> bool {
    crate::local::folder_of(p).is_some_and(|f| f.trim_end_matches('/') == root.trim_end_matches('/'))
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
/// `IMG_1-Pano.dng`, `IMG_1-HDR-Pano.dng` — also the names other raw editors use — and
/// `IMG_1-Focus.dng`): `hdr`, `panorama`, `hdrPanorama` or `focusStack`.
pub fn merged_kind(file_name: &str) -> Option<&'static str> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s).to_ascii_lowercase();
    let stem = stem.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-' || c == ' ');
    if stem.ends_with("-hdr-pano") {
        Some("hdrPanorama")
    } else if stem.ends_with("-hdr") {
        Some("hdr")
    } else if stem.ends_with("-pano") {
        Some("panorama")
    } else if stem.ends_with("-focus") {
        Some("focusStack")
    } else {
        None
    }
}
