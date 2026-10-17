//! Retained review probe freshness regressions extracted from the existing #332 stack.
use super::*;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Fixture(PathBuf);
impl Fixture {
    fn new(tag: &str) -> TestResult<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("lc-import-run1-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(width: usize, seed: u8) -> TestResult<Vec<u8>> {
    let image = lightcraft_raster::Rgba8::from_fn(width, 1, |_, _| [seed, 64, 128, 255]);
    Ok(lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default())?)
}

// Equal-sized valid PNGs, padded with an ancillary tEXt chunk before IEND.
fn padded_png(width: usize, seed: u8) -> TestResult<Vec<u8>> {
    let mut bytes = png(width, seed)?;
    let end = bytes.len().checked_sub(12).ok_or("missing PNG end chunk")?;
    let iend = bytes.split_off(end);
    let payload_len = 4096usize.checked_sub(bytes.len() + iend.len() + 12).ok_or("PNG too large")?;
    let mut payload = b"Comment\0".to_vec();
    if payload_len < payload.len() {
        return Err("PNG padding too small".into());
    }
    payload.resize(payload_len, b'x');
    let mut chunk = b"tEXt".to_vec();
    chunk.extend(payload);
    let mut crc = !0u32;
    for byte in &chunk {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb88320 } else { 0 };
        }
    }
    bytes.extend(u32::try_from(payload_len)?.to_be_bytes());
    bytes.extend(chunk);
    bytes.extend((!crc).to_be_bytes());
    bytes.extend(iend);
    Ok(bytes)
}

fn count_probes(s: &mut Session) -> TestResult<Arc<AtomicUsize>> {
    let native = s.media.file_probe.clone().ok_or("native probe missing")?;
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    s.media.file_probe = Some(Arc::new(move |path| {
        counted.fetch_add(1, Ordering::Relaxed);
        native(path)
    }));
    Ok(calls)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> TestResult {
    let modified = std::fs::metadata(path)?.modified()?;
    let next = path.with_extension("replacement");
    std::fs::write(&next, bytes)?;
    std::fs::File::options().write(true).open(&next)?.set_times(std::fs::FileTimes::new().set_modified(modified))?;
    std::fs::rename(next, path)?;
    Ok(())
}

#[test]
fn native_atomic_replacement_reprobes_add_move_and_refuses_copy() -> TestResult {
    use std::os::unix::fs::MetadataExt;
    for mode in [ImportMode::Add, ImportMode::Move, ImportMode::Copy] {
        let fixture = Fixture::new(&format!("stamp-{mode:?}"))?;
        let path = fixture.0.join("photo.png");
        let old = padded_png(1, 16)?;
        let replacement = padded_png(2, 48)?;
        assert_eq!(old.len(), replacement.len());
        std::fs::write(&path, &old)?;
        let mut s = Session::new().with_fs();
        let calls = count_probes(&mut s)?;
        let paths = vec![path.to_string_lossy().into_owned()];
        scan(&mut s, &paths);
        scan(&mut s, &paths);
        assert_eq!(calls.load(Ordering::Relaxed), 1, "unchanged review reuses its probe");
        let cached = s.import_probes.get(&paths[0]).unwrap().clone();
        let inode = std::fs::metadata(&path)?.ino();
        atomic_replace(&path, &replacement)?;
        assert_ne!(std::fs::metadata(&path)?.ino(), inode);
        assert!(!cached.cache_is_current(&paths[0]));
        let dest = fixture.0.join("destination");
        let opts = ImportOptions { mode, destination: Some(dest.to_string_lossy().into()), ..Default::default() };
        let result = import_with(&mut s, &paths, &opts)?;
        if mode == ImportMode::Copy {
            assert_eq!(result.failed.len(), 1);
            assert!(result.failed[0].1.contains("source changed meanwhile"));
            assert_eq!(s.catalog.len(), 0);
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_eq!(std::fs::read(&path)?, replacement);
            assert!(!dest.exists());
        } else {
            assert_eq!(result.imported.len(), 1);
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            let photo = s.catalog.photo(PhotoId(result.imported[0])).unwrap();
            assert_eq!((photo.width, photo.height), (2, 1));
            assert_ne!(photo.content_hash, cached.content_hash);
            let Source::File { path: stored } = &photo.source else { panic!("native file expected") };
            assert_eq!(std::fs::read(stored)?, replacement);
            assert_eq!(path.exists(), mode == ImportMode::Add);
        }
    }
    Ok(())
}

#[test]
fn review_rescan_rejects_replaced_inode_and_virtual_custom_probes_keep_their_contract() -> TestResult {
    let fixture = Fixture::new("rescan")?;
    let path = fixture.0.join("photo.png");
    std::fs::write(&path, padded_png(1, 16)?)?;
    let mut s = Session::new().with_fs();
    let calls = count_probes(&mut s)?;
    let paths = vec![path.to_string_lossy().into_owned()];
    scan(&mut s, &paths);
    atomic_replace(&path, &padded_png(2, 48)?)?;
    let candidates = scan(&mut s, &paths);
    assert_eq!(candidates[0].width, 2);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert!(ProbeInfo::default().cache_is_current(&fixture.0.join("virtual.png").to_string_lossy()));
    let custom = ProbeInfo { file_size: std::fs::metadata(&path)?.len(), ..Default::default() };
    assert!(custom.cache_is_current(&paths[0]));
    let mut virtual_session = Session::new();
    virtual_session.media.file_probe = Some(Arc::new(|_| Ok(ProbeInfo { width: 7, height: 3, format: "PNG".into(), ..Default::default() })));
    let virtual_path = fixture.0.join("not-on-disk.png").to_string_lossy().into_owned();
    scan(&mut virtual_session, std::slice::from_ref(&virtual_path));
    let report = import_with(&mut virtual_session, &[virtual_path], &Default::default())?;
    assert_eq!(report.imported.len(), 1);
    Ok(())
}

#[test]
fn completed_ui_scans_allow_first_deliberate_copy_retry_after_source_change() -> TestResult {
    let fixture = Fixture::new("completed-ui-copy-retry")?;
    let path = fixture.0.join("photo.png");
    let old = padded_png(1, 16)?;
    let replacement = padded_png(2, 48)?;
    assert_eq!(old.len(), replacement.len());
    std::fs::write(&path, &old)?;
    let mut s = Session::new().with_fs();
    let calls = count_probes(&mut s)?;
    let sources = vec![fixture.0.to_string_lossy().into_owned()];
    let paths = vec![path.to_string_lossy().into_owned()];
    for _ in 0..2 {
        let (input, sources) = ScanInput::new(&mut s, &sources);
        let out = scan_with(input, &sources, &ScanProgress::default());
        assert_eq!(out.candidates.len(), 1);
        assert!(out.candidates[0].error.is_none());
        assert_eq!(out.candidates[0].path, paths[0]);
        assert_eq!((out.candidates[0].width, out.candidates[0].height), (1, 1));
        // Completed UI poll_scan assigns directly; scan() would clear the retained cache.
        s.import_probes = out.probes;
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1, "both completed scans reuse the native probe");
    let cached = s.import_probes.get(&paths[0]).unwrap().clone();
    assert!(cached.file_stamp.is_some());
    assert_eq!(s.import_probe_cache.as_ref().unwrap().get(&paths[0]).unwrap().file_stamp, cached.file_stamp);
    atomic_replace(&path, &replacement)?;
    assert!(!cached.cache_is_current(&paths[0]));
    let dest = fixture.0.join("destination");
    let opts = ImportOptions { mode: ImportMode::Copy, destination: Some(dest.to_string_lossy().into()), ..Default::default() };
    let first = import_with(&mut s, &paths, &opts)?;
    assert_eq!(first.failed.len(), 1);
    assert!(first.failed[0].1.contains("source changed meanwhile"));
    assert!(first.imported.is_empty());
    assert_eq!(s.catalog.len(), 0);
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the first stale Copy refuses before probing again");
    assert_eq!(std::fs::read(&path)?, replacement);
    assert!(!dest.exists());

    let retry = import_with(&mut s, &paths, &opts)?;
    assert_eq!(retry.imported.len(), 1, "the first deliberate Copy retry must reprobe and succeed: {:?}", retry.failed);
    assert!(retry.failed.is_empty());
    assert_eq!(calls.load(Ordering::Relaxed), 2, "the retry performs one fresh native probe");
    assert_eq!(s.catalog.len(), 1);
    let photo = s.catalog.photo(PhotoId(retry.imported[0])).unwrap();
    assert_eq!((photo.width, photo.height), (2, 1));
    assert_ne!(photo.content_hash, cached.content_hash);
    let Source::File { path: stored } = &photo.source else { panic!("native file expected") };
    assert!(Path::new(stored).starts_with(&dest));
    assert_eq!(std::fs::read(stored)?, replacement);
    assert_eq!(std::fs::read(&path)?, replacement);
    Ok(())
}
