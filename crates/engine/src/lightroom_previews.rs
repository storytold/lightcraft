//! Lightroom Classic's own previews of a catalog's photos, as stand-ins for photos whose originals
//! can't be read cheaply: online-only in a cloud folder ([`crate::cloud_files`]) or offline. The
//! grid then shows the photo as Lightroom last rendered it, without downloading its original.
//!
//! What the preview store looks like was observed on catalogs of our own (Lightroom Classic 13,
//! store version 13), black-box: nothing here comes from Adobe code or documentation.
//!
//! - The store is the folder `<catalog name> Previews.lrdata` beside the `.lrcat`.
//! - Its `previews.db` (SQLite) has `ImageCacheEntry (imageId, uuid, digest, orientation)`, where
//!   `imageId` is the catalog's `Adobe_images.id_local`; `Pyramid (uuid, digest, colorProfile, …)`
//!   with `AdobeRGB` or `sRGB`; and `PyramidLevel (uuid, digest, level, longDimension, …)`, one
//!   row per size kept.
//! - Each size is a plain baseline JPEG at `<uuid[0]>/<uuid[0..4]>/<uuid>-<digest>_<longDimension>`,
//!   without an ICC profile (its space is the pyramid's `colorProfile`), cropped as the photo is
//!   but in the stored image's orientation: `ImageCacheEntry.orientation` (Lightroom's corner code,
//!   [`crate::lightroom_catalog::lightroom_orientation`]) turns it upright.
//!
//! Everything is optional: no store, an unreadable one, or one laid out otherwise gives no
//! stand-ins and the import goes on as without them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::PhotoId;
use lightcraft_geom::Orientation;
use lightcraft_preview::{Hash128, PreviewCache};
use lightcraft_raster::Rgba8;

use crate::lightroom_sqlite::{Database, FileSource, Value};

/// Largest preview file read (a 1:1 preview of a 100 MP photo is well under this).
const MAX_PREVIEW_BYTES: u64 = 64 << 20;
/// Largest preview database read into memory pages: the store's index, not its images.
const MAX_INDEX_ROWS: usize = 4_000_000;
/// The long edge a stand-in is decoded to: the largest grid thumbnail. Only grid thumbnails are
/// seeded: Lightroom's standard previews (most are 1024 px) are smaller than the loupe draws, and
/// loupe-sized copies of thousands of photos would push the thumbnails out of the disk cache.
const MAX_EDGE: usize = crate::media::THUMB_SIZES[crate::media::THUMB_SIZES.len() - 1];

/// One photo's Lightroom preview: its largest size on disk.
#[derive(Clone, Debug, PartialEq)]
pub struct StandIn {
    pub path: PathBuf,
    pub adobe_rgb: bool,
    pub orientation: Orientation,
}

/// The preview store of `catalog` (a `.lrcat`), when there is one.
pub fn store_of(catalog: &Path) -> Option<PathBuf> {
    if !catalog.extension().is_some_and(|e| e.eq_ignore_ascii_case("lrcat")) {
        return None;
    }
    let stem = catalog.file_stem()?.to_str()?;
    let store = catalog.with_file_name(format!("{stem} Previews.lrdata"));
    store.join("previews.db").is_file().then_some(store)
}

fn text(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::Text(s)) => Some(s),
        _ => None,
    }
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Integer(n)) => Some(*n as f64),
        Some(Value::Real(n)) if n.is_finite() => Some(*n),
        _ => None,
    }
}

/// A Lightroom id as it is used in a file name: the canonical 36-character UUID.
fn valid_uuid(s: &str) -> bool {
    s.len() == 36 && s.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() })
}

/// A pyramid digest as it is used in a file name: hexadecimal.
fn valid_digest(s: &str) -> bool {
    (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A table of the store's database as rows of `columns` (missing columns are `Null`).
fn rows(db: &Database, name: &str, wanted: &[&str]) -> Result<Vec<Vec<Value>>, String> {
    let tables = db.live_tables()?;
    let table = tables.iter().find(|t| t.name == name).ok_or_else(|| format!("no {name} table"))?;
    let columns = table.column_names.as_ref().ok_or_else(|| format!("cannot read {name} columns"))?;
    let at: Vec<Option<usize>> = wanted.iter().map(|w| columns.iter().position(|c| c == w)).collect();
    let records = db.read_table(table.rootpage, columns.len()).map_err(|e| format!("{name}: {e}"))?;
    if records.len() > MAX_INDEX_ROWS {
        return Err(format!("{name}: too many records"));
    }
    Ok(records.into_iter().map(|r| at.iter().map(|i| i.and_then(|i| r.values.get(i).cloned()).unwrap_or(Value::Null)).collect()).collect())
}

fn open(store: &Path) -> Result<Database, String> {
    let main_path = store.join("previews.db");
    let main = FileSource::open(&main_path)?;
    let wal_path = store.join("previews.db-wal");
    let wal = if wal_path.is_file() { crate::lightroom_catalog::bytes(&wal_path)? } else { Vec::new() };
    let wal_len = crate::lightroom_catalog::committed_wal_len(main.header(), &wal)?;
    Database::open_file_with_wal(main, wal.get(..wal_len).unwrap_or(&[]))
}

/// The stand-ins in `store` for the catalog images `wanted` (by `Adobe_images.id_local`): each
/// one's largest preview whose file is there. Stops early, with what it has, when `cancel` is set.
pub fn read(store: &Path, wanted: &HashSet<i64>, cancel: &AtomicBool) -> Result<HashMap<i64, StandIn>, String> {
    let mut out = HashMap::new();
    if wanted.is_empty() {
        return Ok(out);
    }
    let db = open(store)?;
    let entries = rows(&db, "ImageCacheEntry", &["imageId", "uuid", "digest", "orientation"])?;
    let mut profiles: HashMap<(String, String), bool> = HashMap::new();
    for r in rows(&db, "Pyramid", &["uuid", "digest", "colorProfile"])? {
        if let (Some(u), Some(d)) = (text(r.first()), text(r.get(1))) {
            let adobe = text(r.get(2)).is_some_and(|p| p.eq_ignore_ascii_case("AdobeRGB"));
            profiles.insert((u.to_string(), d.to_string()), adobe);
        }
    }
    let mut levels: HashMap<(String, String), Vec<u32>> = HashMap::new();
    for r in rows(&db, "PyramidLevel", &["uuid", "digest", "longDimension"])? {
        let (Some(u), Some(d), Some(edge)) = (text(r.first()), text(r.get(1)), number(r.get(2))) else { continue };
        if edge >= 1.0 && edge <= f64::from(u32::MAX) {
            levels.entry((u.to_string(), d.to_string())).or_default().push(edge as u32);
        }
    }
    for r in entries {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(image) = number(r.first()).map(|n| n as i64).filter(|id| wanted.contains(id)) else { continue };
        let (Some(uuid), Some(digest)) = (text(r.get(1)), text(r.get(2))) else { continue };
        // (names that would leave the store are never joined to it)
        if !valid_uuid(uuid) || !valid_digest(digest) {
            continue;
        }
        let key = (uuid.to_string(), digest.to_string());
        let Some(sizes) = levels.get_mut(&key) else { continue };
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        let (Some(first), Some(shard)) = (uuid.get(..1), uuid.get(..4)) else { continue };
        let folder = store.join(first).join(shard);
        let Some(path) = sizes.iter().map(|edge| folder.join(format!("{uuid}-{digest}_{edge}"))).find(|p| p.is_file()) else { continue };
        let orientation = text(r.get(3)).and_then(crate::lightroom_catalog::lightroom_orientation).unwrap_or_default();
        let adobe_rgb = profiles.get(&key).copied().unwrap_or(false);
        out.insert(image, StandIn { path, adobe_rgb, orientation });
    }
    Ok(out)
}

/// The stand-in's picture: display sRGB, upright, at most [`MAX_EDGE`] on its long side.
pub fn decode(stand_in: &StandIn) -> Result<Rgba8, String> {
    let bytes = crate::read_file::read_regular(&stand_in.path, MAX_PREVIEW_BYTES).map_err(|e| format!("{}: {e}", stand_in.path.display()))?;
    let space = if stand_in.adobe_rgb { lightcraft_codecs::NamedSpace::AdobeRgb } else { lightcraft_codecs::NamedSpace::Srgb };
    let edge = MAX_EDGE as u32;
    let decoded = lightcraft_codecs::decode_jpeg_with_fallback(&bytes, lightcraft_codecs::DecodeOptions::fit(edge, edge), space)
        .map_err(|e| format!("{}: {e}", stand_in.path.display()))?;
    Ok(decoded.to_srgb8().into_oriented(stand_in.orientation))
}

/// One photo to seed: its stand-in and the cache keys its thumbnails take, by size.
struct Seed {
    stand_in: StandIn,
    thumbs: Vec<(usize, Hash128)>,
}

/// Stand-ins to put in the thumbnail cache, made on the owner thread after the import committed
/// ([`PreviewSeed::plan`]) and run on a worker ([`PreviewSeed::run`]). A key the cache already has
/// is left alone: LightCraft's own render always wins over Lightroom's.
pub struct PreviewSeed {
    cache: Arc<PreviewCache>,
    generation: u64,
    seeds: Vec<Seed>,
}

/// What [`PreviewSeed::run`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Photos whose thumbnails were stored on disk, where they last.
    pub seeded: usize,
    /// Photos whose preview could not be decoded, or whose thumbnails the disk cache could not write.
    pub failed: usize,
}

impl PreviewSeed {
    /// The keys `photos` (each with its stand-in) take in `s`'s thumbnail cache now: every grid
    /// thumbnail size. `None` when there is nothing to seed.
    pub fn plan(s: &mut crate::Session, photos: Vec<(PhotoId, StandIn)>) -> Option<Self> {
        if photos.is_empty() {
            return None;
        }
        // the disk budget grows with the library: sized for the photos just imported, not for the
        // (perhaps empty) library that was opened, it would drop thumbnails as fast as they are kept
        s.fit_disk_cache();
        let mut seeds = Vec::new();
        let mut cache = None;
        for (id, stand_in) in photos {
            let mut thumbs = Vec::new();
            for size in crate::media::THUMB_SIZES {
                if let Some((c, key)) = s.thumb_job(id, size).and_then(|j| j.cache) {
                    thumbs.push((size, key));
                    cache.get_or_insert(c);
                }
            }
            if !thumbs.is_empty() {
                seeds.push(Seed { stand_in, thumbs });
            }
        }
        let cache = cache?;
        let generation = cache.generation();
        Some(Self { cache, generation, seeds })
    }

    pub fn len(&self) -> usize {
        self.seeds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seeds.is_empty()
    }

    /// Decode each stand-in and keep it under its keys, counting each photo in `task`.
    pub fn run(self, cancel: &AtomicBool, task: Option<&crate::activity::TaskGuard>) -> SeedReport {
        let mut report = SeedReport::default();
        let total = self.seeds.len() as u64;
        for (n, seed) in self.seeds.into_iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if let Some(task) = task {
                task.progress(n as u64, total);
            }
            let wanted: Vec<_> = seed.thumbs.iter().filter(|(_, k)| self.cache.stored_dims(*k).is_none()).copied().collect();
            if wanted.is_empty() {
                continue;
            }
            let image = match crate::guard::catch("a Lightroom preview", || decode(&seed.stand_in)) {
                Ok(Ok(image)) if image.width > 0 && image.height > 0 => image,
                Ok(Ok(_)) => {
                    report.failed += 1;
                    continue;
                }
                Ok(Err(e)) | Err(e) => {
                    log::warn!("Lightroom preview: {e}");
                    report.failed += 1;
                    continue;
                }
            };
            let mut written = true;
            for (size, key) in wanted {
                let thumb = if image.width.max(image.height) > size { crate::media::fit_for_display(&image, size, size) } else { image.clone() };
                if !self.cache.put_at(self.generation, key, Arc::new(thumb)) {
                    if self.cache.generation() != self.generation {
                        // the preview cache was discarded meanwhile: it stays discarded
                        return report;
                    }
                    written = false;
                }
            }
            // (a thumbnail the disk cache could not write shows for this session only: the photo isn't seeded)
            if written {
                report.seeded += 1;
            } else {
                report.failed += 1;
            }
            crate::memory::release();
        }
        if let Some(task) = task {
            task.progress(total, total);
        }
        report
    }

    /// [`Self::run`] on a thread of its own, shown in the activity stack. `None` when the thread
    /// could not start (nothing is seeded; the photos render as usual).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn spawn(self, activity: &crate::activity::Activity) -> Option<std::thread::JoinHandle<SeedReport>> {
        let task = activity.start("lightroom", "Using Lightroom previews", crate::activity::Cancel::Yes);
        std::thread::Builder::new()
            .name("lc-lightroom-previews".into())
            .spawn(move || {
                let cancel = task.cancel_flag().unwrap_or_default();
                self.run(&cancel, Some(&task))
            })
            .ok()
    }
}

#[cfg(test)]
mod tests;
