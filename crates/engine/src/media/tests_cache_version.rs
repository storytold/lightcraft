//! Native cache compatibility after independently prepared pipeline changes.
use super::*;
use lightcraft_catalog::Op;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

// Literal historical key version: using the production constant would hide the collision.
const LEGACY_V22: u64 = 22;
const EDGE: usize = 128;

struct RuntimeDir(PathBuf);
impl RuntimeDir {
    fn new() -> TestResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = std::env::temp_dir().join(format!("lc-cache-v22-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        // Never reuse or remove a pre-existing directory.
        std::fs::create_dir(&dir)?;
        Ok(Self(dir))
    }
}
impl Drop for RuntimeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn native_fixture() -> TestResult<(RuntimeDir, crate::Session, PhotoId)> {
    let dir = RuntimeDir::new()?;
    let image = Rgba8::from_fn(EDGE, 96, |x, y| {
        let tile = if (x / 7 + y / 5) % 2 == 0 { 30 } else { 205 };
        let texture = ((x * 17 + y * 29) % 31) as u8;
        [tile + texture, (x * 255 / (EDGE - 1)) as u8, (y * 235 / 95 + 10) as u8, 255]
    });
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default())?;
    let path = dir.0.join("textured.png");
    std::fs::write(&path, &bytes)?;
    let id = PhotoId(1);
    let mut photo = Photo::new(id, Source::File { path: path.to_string_lossy().into_owned() }, "textured.png", "PNG", EDGE as u32, 96, "2026-10-09");
    photo.file_size = u64::try_from(bytes.len())?;
    photo.content_hash = Some(lightcraft_preview::hash_bytes(&bytes).to_string());
    let mut settings = DevelopSettings::default();
    settings.profile.id = "lc.landscape".into();
    settings.profile.amount = 200.0;
    settings.set_section_enabled("effects", false);
    photo.develop = Arc::new(settings);
    let mut session = crate::Session::new().with_fs();
    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    session.media.attach_disk_cache(&dir.0.join("thumbs"), 2 << 20);
    Ok((dir, session, id))
}

// The exact v22 thumbnail, variant and view formulas, independent of today's version.
fn legacy_thumb_key(photo: &Photo, camera_profiles: u64) -> Hash128 {
    Hasher128::new().str(&content_key(photo)).u64(photo.develop.hash64()).u64(thumb_bucket(EDGE) as u64).u64(LEGACY_V22).u64(camera_profiles).finish()
}
fn legacy_variant_key(photo: &Photo, camera_profiles: u64) -> Hash128 {
    Hasher128::new()
        .str(&content_key(photo))
        .str("variant")
        .u64(photo.develop.hash64())
        .u64(EDGE as u64)
        .u64(LEGACY_V22)
        .u64(camera_profiles)
        .finish()
}
fn legacy_view_key(photo: &Photo, camera_profiles: u64) -> Hash128 {
    Hasher128::new()
        .str(&content_key(photo))
        .str("view")
        .u64(photo.develop.hash64())
        .u64(1) // apply_crop
        .u64(LEGACY_V22)
        .u64(camera_profiles)
        .finish()
}

#[test]
fn legacy_v22_disk_previews_do_not_bypass_corrected_landscape_rendering() -> TestResult {
    let (dir, mut session, id) = native_fixture()?;
    let photo = session.catalog.photo(id).ok_or("fixture photo missing")?.clone();
    let saved = photo.develop.clone();
    let camera_profiles = crate::camera_profiles::cache_key();
    let old_thumb = legacy_thumb_key(&photo, camera_profiles);
    let old_variant = legacy_variant_key(&photo, camera_profiles);
    let old_view = legacy_view_key(&photo, camera_profiles);
    let initial = session.thumb_job(id, EDGE).ok_or("thumbnail job missing")?;
    let decoded = initial.source.load_source()?;
    let info = decoded.info_or(initial.info.clone());

    // With no user Effects edits, switching the panel off must keep the profile's look.
    let mut profile_only = (*saved).clone();
    profile_only.set_section_enabled("effects", true);
    let expected = lightcraft_pipeline::render(&decoded.image, &info, &profile_only, &initial.request);
    // A distinct valid stale render makes a key collision observable after reopening. The
    // historical key formulas use literal v22, independently of the current version.
    let legacy =
        Rgba8::from_fn(
            expected.image.width,
            expected.image.height,
            |x, y| {
                if (x / 7 + y / 5) % 2 == 0 { [255, 0, 255, 255] } else { [0, 255, 0, 255] }
            },
        );
    assert_ne!(legacy, expected.image);

    let writer = session.media.rendered.clone();
    for key in [old_thumb, old_variant, old_view] {
        writer.put(key, Arc::new(legacy.clone()));
    }
    assert_eq!(writer.disk().ok_or("disk cache missing")?.writes.load(Ordering::Relaxed), 3, "legacy JPEGs really reached disk");
    // Reopen the same directory through the production cache-attachment path, with an empty LRU.
    session.media.attach_disk_cache(&dir.0.join("thumbs"), 2 << 20);
    drop(initial);
    drop(writer);
    let reopened = session.media.rendered.clone();
    assert_eq!(reopened.mem_usage().0, 0);
    let disk = reopened.disk().ok_or("reopened disk cache missing")?;
    for key in [old_thumb, old_variant, old_view] {
        assert!(disk.get(key).is_some(), "valid historical entries survive reopening");
    }

    let job = session.thumb_job(id, EDGE).ok_or("reopened thumbnail job missing")?;
    assert!(job.cache.is_some(), "thumbnail jobs force CPU rendering");
    let new_thumb = job.cache.as_ref().ok_or("thumbnail cache missing")?.1;
    let result = job.run();
    assert!(result.loaded.is_some(), "a legacy v22 entry must miss and load the native source");
    let rendered = result.rendered?;
    assert_eq!(rendered.image, expected.image, "a cached old look cannot replace corrected CPU output");
    assert_eq!(rendered.histogram, expected.histogram);
    assert_ne!(new_thumb, old_thumb);
    assert!(reopened.get(new_thumb).is_some(), "the fresh result warms its new key");

    let variant = session.variant_job(id, &saved, EDGE).ok_or("variant job missing")?;
    let new_variant = variant.cache.as_ref().ok_or("variant cache missing")?.1;
    assert_ne!(new_variant, old_variant);
    assert!(reopened.get(new_variant).is_none(), "the legacy variant cannot satisfy the current key");
    assert_eq!(variant.run().rendered?.image, expected.image);

    let view = session.loupe_job(id, EDGE, EDGE, true).ok_or("view job missing")?;
    let new_view = view.view_cache.as_ref().ok_or("view cache missing")?.1;
    assert_ne!(new_view, old_view);
    assert!(reopened.get(new_view).is_none(), "the legacy view cannot satisfy the current key");
    let quick = session.quick_view_job(id, EDGE, true).ok_or("quick view job missing")?;
    assert_eq!(quick.cached.first().ok_or("quick view cache missing")?.1, new_view);
    assert_eq!(crate::camera_profiles::cache_key(), camera_profiles);
    let after = session.catalog.photo(id).ok_or("fixture photo disappeared")?;
    assert_eq!(after.as_ref(), photo.as_ref(), "rendering preserves all stored source and edit facts");
    assert!(Arc::ptr_eq(&saved, &after.develop));
    Ok(())
}
