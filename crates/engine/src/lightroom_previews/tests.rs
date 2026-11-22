use super::*;
use crate::lightroom_catalog::tests::{native_catalog, native_catalog_rows, photo_tables};
use crate::lightroom_sqlite::Value as SqlValue;
use std::sync::atomic::AtomicUsize;

const UUID: &str = "0014C12B-C71A-4A0F-BB56-C937D0A60F0B";
const DIGEST: &str = "782559885e84487d43e82d4a4b65d5c1";

/// A test's folder, removed when it is dropped, after a failed assertion as well.
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(name: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!("lightcraft-lr-previews-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// An 8 × 4 JPEG, its left half red and its right half blue.
fn two_colour_jpeg() -> Vec<u8> {
    let (w, h) = (8usize, 4usize);
    let pixels: Vec<u8> = (0..w * h).flat_map(|i| if i % w < w / 2 { [230u8, 20, 20] } else { [20, 20, 230] }).collect();
    let image = lightcraft_codecs::EncodeImage::new(w as u32, h as u32, 3, lightcraft_codecs::Samples::U8(&pixels));
    lightcraft_codecs::encode_jpeg(&image, 95, lightcraft_codecs::ChromaSubsampling::S444, &Default::default()).unwrap()
}

/// A preview store as Lightroom Classic 13 lays one out: `previews.db` and one JPEG per size kept.
/// `entries` are `(imageId, uuid, orientation)`; every uuid has levels 1024 and 320, and only the
/// sizes in `on_disk` are written.
fn store(dir: &Path, catalog_stem: &str, entries: &[(i64, &str, &str)], on_disk: &[u32]) -> PathBuf {
    use SqlValue::{Integer as I, Text as T};
    let store = dir.join(format!("{catalog_stem} Previews.lrdata"));
    std::fs::create_dir_all(&store).unwrap();
    let cache_rows = entries.iter().map(|(id, uuid, o)| vec![I(*id), T((*uuid).into()), T(DIGEST.into()), T((*o).into())]).collect();
    let pyramids = entries.iter().map(|(_, uuid, _)| vec![T((*uuid).into()), T(DIGEST.into()), T("AdobeRGB".into())]).collect();
    let levels = entries.iter().flat_map(|(_, uuid, _)| [1024, 320].map(|edge| vec![T((*uuid).into()), T(DIGEST.into()), I(1), I(edge)])).collect();
    let db = native_catalog_rows(
        &[
            ("ImageCacheEntry", "imageId NOT NULL, uuid NOT NULL, digest NOT NULL, orientation", cache_rows),
            ("Pyramid", "uuid NOT NULL, digest NOT NULL, colorProfile NOT NULL", pyramids),
            ("PyramidLevel", "uuid NOT NULL, digest NOT NULL, level NOT NULL, longDimension NOT NULL", levels),
        ],
        None,
    );
    std::fs::write(store.join("previews.db"), db).unwrap();
    for (_, uuid, _) in entries {
        let (Some(first), Some(shard)) = (uuid.get(..1), uuid.get(..4)) else { continue };
        if !valid_uuid(uuid) {
            continue;
        }
        let folder = store.join(first).join(shard);
        std::fs::create_dir_all(&folder).unwrap();
        for edge in on_disk {
            std::fs::write(folder.join(format!("{uuid}-{DIGEST}_{edge}")), two_colour_jpeg()).unwrap();
        }
    }
    store
}

#[test]
fn the_store_is_found_beside_a_catalog_only() {
    let dir = temp_dir("store-of");
    let catalog = dir.join("My Catalog.lrcat");
    assert_eq!(store_of(&catalog), None, "no store");
    let made = store(&dir, "My Catalog", &[], &[]);
    assert_eq!(store_of(&catalog), Some(made));
    assert_eq!(store_of(&dir.join("My Catalog.zip")), None, "a backup has no store beside it");
}

#[test]
fn reads_the_largest_preview_on_disk_and_never_a_name_that_leaves_the_store() {
    let dir = temp_dir("read");
    let other = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
    let s = store(&dir, "Cat", &[(1, UUID, "BC"), (2, "../../../../../../../../etc/passwd-0000", "AB"), (3, other, "AB")], &[320]);
    // image 3's pyramid has no file at all
    std::fs::remove_dir_all(s.join("A")).unwrap();
    let wanted = [1, 2, 3, 4].into_iter().collect();
    let found = read(&s, &wanted, &AtomicBool::new(false)).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let one = &found[&1];
    // 1024 is listed but not on disk: the 320 is used
    assert_eq!(one.path, s.join("0").join("0014").join(format!("{UUID}-{DIGEST}_320")));
    assert!(one.adobe_rgb);
    assert_eq!(one.orientation, Orientation::Rotate90);
    assert!(read(&s, &HashSet::new(), &AtomicBool::new(false)).unwrap().is_empty(), "nothing wanted, nothing read");
}

#[test]
fn a_broken_store_is_an_error_not_a_panic() {
    let dir = temp_dir("broken");
    let s = dir.join("Cat Previews.lrdata");
    std::fs::create_dir_all(&s).unwrap();
    std::fs::write(s.join("previews.db"), b"SQLite format 3\0 but not really").unwrap();
    assert!(read(&s, &[1].into_iter().collect(), &AtomicBool::new(false)).is_err());
    std::fs::write(s.join("previews.db"), native_catalog(&[("Other", "a", vec![SqlValue::Integer(1)])])).unwrap();
    assert!(read(&s, &[1].into_iter().collect(), &AtomicBool::new(false)).is_err(), "no ImageCacheEntry table");
}

#[test]
fn a_stand_in_decodes_upright() {
    let dir = temp_dir("decode");
    let s = store(&dir, "Cat", &[(1, UUID, "BC")], &[320]);
    let found = read(&s, &[1].into_iter().collect(), &AtomicBool::new(false)).unwrap();
    let image = decode(&found[&1]).unwrap();
    // a quarter turn clockwise: the stored left half (red) is the top
    assert_eq!((image.width, image.height), (4, 8));
    let top = image.data[image.width + 1];
    let bottom = image.data[image.width * 6 + 1];
    assert!(top[0] > top[2] && bottom[2] > bottom[0], "top {top:?}, bottom {bottom:?}");
}

/// A catalog whose one photo has `test.ARW` as its original, beside a store with its preview.
fn catalog_with_preview(name: &str, original: bool) -> (TempDir, PathBuf) {
    let dir = temp_dir(name);
    std::fs::create_dir_all(dir.join("Photos")).unwrap();
    if original {
        std::fs::write(dir.join("Photos/test.ARW"), b"raw bytes the import must not read").unwrap();
    }
    let source = dir.join("source.lrcat");
    std::fs::write(&source, native_catalog(&photo_tables((dir.to_str().unwrap(), "")))).unwrap();
    store(&dir, "source", &[(1, UUID, "AB")], &[1024, 320]);
    (dir, source)
}

fn counting_probe(s: &mut crate::Session) -> Arc<AtomicUsize> {
    let probed = Arc::new(AtomicUsize::new(0));
    let count = probed.clone();
    s.media.file_probe = Some(Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(crate::media::ProbeInfo { format: "ARW".into(), width: 20, height: 10, ..Default::default() })
    }));
    probed
}

/// An online-only original (a Dropbox placeholder) is not read: the photo comes from the catalog's
/// record and its grid thumbnails from Lightroom's preview, and it is not reported missing.
#[test]
fn an_online_only_original_is_not_read_and_shows_lightroom_preview() {
    let (_dir, source) = catalog_with_preview("online-only", true);
    let mut s = crate::Session::new();
    let probed = counting_probe(&mut s);
    let mut job = crate::lightroom_job::LightroomJob::new(&mut s, source, false).unwrap().with_placeholder_check(|_| true);
    let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
    let completion = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap();
    let r = completion.report;
    assert_eq!(probed.load(Ordering::Relaxed), 0, "the original was read: {r}");
    assert_eq!((r["cloudOnly"].as_u64(), r["lightroomPreviews"].as_u64()), (Some(1), Some(1)), "{r}");
    assert_eq!(r["missing"].as_array().map(Vec::len), Some(0), "not missing: {r}");
    let id = PhotoId(r["mapping"]["1"].as_u64().unwrap());
    assert_eq!(s.catalog.photo(id).unwrap().rating, 5, "the catalog's record: {r}");
    let seed = completion.previews.unwrap();
    assert_eq!(seed.run(&AtomicBool::new(false), None), SeedReport { seeded: 1, failed: 0 });
    for size in crate::media::THUMB_SIZES {
        let (cache, key) = s.thumb_job(id, size).unwrap().cache.unwrap();
        let thumb = cache.get(key).unwrap_or_else(|| panic!("no {size} px thumbnail"));
        assert_eq!((thumb.width, thumb.height), (8, 4), "the preview, never enlarged");
    }
    let view = s.view_key(s.catalog.photo(id).unwrap(), true);
    assert!(s.media.rendered.get(view).is_none(), "the loupe renders the original: a 1024 px preview is too small for it");
}

/// A thumbnail LightCraft rendered itself is never replaced by Lightroom's.
#[test]
fn a_seed_never_replaces_a_render() {
    let (_dir, source) = catalog_with_preview("keeps-render", true);
    let mut s = crate::Session::new();
    counting_probe(&mut s);
    let mut job = crate::lightroom_job::LightroomJob::new(&mut s, source, false).unwrap().with_placeholder_check(|_| true);
    let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
    let completion = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap();
    let id = PhotoId(completion.report["mapping"]["1"].as_u64().unwrap());
    let (cache, key) = s.thumb_job(id, 128).unwrap().cache.unwrap();
    let own = Arc::new(Rgba8 { width: 3, height: 3, data: vec![[1, 2, 3, 255]; 9] });
    cache.put(key, own.clone());
    completion.previews.unwrap().run(&AtomicBool::new(false), None);
    assert_eq!(cache.get(key).unwrap().data, own.data);
}

/// A missing original (an unplugged drive) gets Lightroom's preview as well (#2613); an original
/// that is here and downloaded is read as before, and needs none.
#[test]
fn a_missing_original_gets_the_preview_and_a_local_one_is_read() {
    let (_missing, source) = catalog_with_preview("missing", false);
    let mut s = crate::Session::new();
    counting_probe(&mut s);
    let r = crate::lightroom_catalog::import(&mut s, &source, false).unwrap();
    assert_eq!(r["missing"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!((r["lightroomPreviews"].as_u64(), r["lightroomPreviewsUsed"].as_u64()), (Some(1), Some(1)), "{r}");

    let (_local, source) = catalog_with_preview("local", true);
    let mut s = crate::Session::new();
    let probed = counting_probe(&mut s);
    let r = crate::lightroom_catalog::import(&mut s, &source, false).unwrap();
    assert_eq!(probed.load(Ordering::Relaxed), 1, "{r}");
    assert_eq!((r["cloudOnly"].as_u64(), r["lightroomPreviews"].as_u64()), (Some(0), Some(0)), "{r}");
    assert!(r.get("lightroomPreviewsUsed").is_none(), "{r}");
}

/// A photo that kept its LightCraft edit on a re-import (Update existing off) is not shown as Lightroom
/// rendered it: its thumbnails are of the edit it has (CodeRabbit on #4552). With Update existing on,
/// it takes Lightroom's edit, and Lightroom's preview with it.
#[test]
fn a_photo_that_kept_its_edit_gets_no_preview() {
    let (_dir, source) = catalog_with_preview("kept-edit", true);
    let mut s = crate::Session::new();
    counting_probe(&mut s);
    let import = |s: &mut crate::Session, update_existing| {
        let mut job = crate::lightroom_job::LightroomJob::new(s, source.clone(), update_existing).unwrap().with_placeholder_check(|_| true);
        let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
        crate::lightroom_job::commit_prepared(s, prepared).unwrap()
    };
    let first = import(&mut s, false);
    let id = PhotoId(first.report["mapping"]["1"].as_u64().unwrap());
    let mut settings = lightcraft_develop::DevelopSettings::default();
    settings.light.exposure = 0.75;
    s.set_develop(id, settings, "Personal edit").unwrap();

    let again = import(&mut s, false);
    let r = &again.report;
    assert_eq!(r["preservedExistingEdits"], 1, "{r}");
    assert_eq!(r["lightroomPreviews"], 0, "{r}");
    assert!(again.previews.is_none());

    let updated = import(&mut s, true);
    assert_eq!(updated.report["lightroomPreviews"], 1, "{}", updated.report);
}

/// A thumbnail the disk cache can't write is kept in memory for this session only, so its photo is
/// not counted as seeded (CodeRabbit on #4552).
#[test]
fn a_thumbnail_the_disk_cannot_keep_is_not_counted_as_seeded() {
    let (dir, source) = catalog_with_preview("unwritable", true);
    let mut s = crate::Session::new();
    counting_probe(&mut s);
    let mut job = crate::lightroom_job::LightroomJob::new(&mut s, source, false).unwrap().with_placeholder_check(|_| true);
    let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
    let mut seed = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap().previews.unwrap();
    // a file where the cache folder goes: every write fails
    let blocked = dir.join("blocked");
    std::fs::write(&blocked, b"not a folder").unwrap();
    seed.cache = Arc::new(PreviewCache::with_disk(1 << 20, &blocked, 1 << 20));
    seed.generation = seed.cache.generation();
    assert_eq!(seed.run(&AtomicBool::new(false), None), SeedReport { seeded: 0, failed: 1 });
}
