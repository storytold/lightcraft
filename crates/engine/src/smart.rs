//! Smart previews: a compact proxy of each photo (its decoded preview-size source, about 1 MB)
//! kept in the library, so photos stay editable — and exportable at proxy size — while their
//! originals are offline (an unplugged drive). The proxy is the scene-linear source scaled into
//! 0..1 by a stored factor, sRGB-encoded and saved as a JPEG after a one-line header. The header
//! also keeps the decoder's file-local camera tone curve (Sony ARW camera look), which is applied
//! at render time and is not baked into the pixels, and the file's other decoder facts (its colour
//! model, gain table map and semantic mattes) ride in a tail after the JPEG, so offline edits
//! interpret the stored pixels the same way the online source did.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightcraft_catalog::Photo;
use lightcraft_color::transfer::linear_to_srgb;
use lightcraft_pipeline::SourceInfo;
use lightcraft_pipeline::masks::{MatteKind, Mattes};
use lightcraft_pipeline::tone::CameraTone;
use lightcraft_raster::{Image, Rgb32f, Rgba8};

const MAGIC: &[u8] = b"LCSP1\n";

/// The proxy's file name for a photo (by its content, so copies share one).
pub fn file_name(p: &Photo) -> String {
    let h = lightcraft_preview::Hasher128::new().str(&crate::media::content_key(p)).finish();
    format!("{:032x}.lcsp", h.0)
}

/// The default sharded location of a proxy. A two-character hex prefix bounds each
/// folder to about 1/256 of the library's smart previews.
pub fn sharded_file_path(dir: &Path, p: &Photo) -> PathBuf {
    let name = file_name(p);
    match name.get(..2) {
        Some(shard) => dir.join(shard).join(&name),
        None => dir.join(&name),
    }
}

/// Read an existing proxy in either layout. New proxies use the sharded location; previews
/// made by older releases still load and can be moved with smartPreviewsLocation.
pub fn file_path(dir: &Path, p: &Photo) -> PathBuf {
    existing_file_path(dir, sharded_file_path(dir, p))
}

/// Resolve a sharded path to an older flat file when present. Call only off the UI thread:
/// checking a removable drive synchronously could stall drawing.
pub fn existing_file_path(dir: &Path, sharded: PathBuf) -> PathBuf {
    if sharded.exists() {
        return sharded;
    }
    let Some(name) = sharded.file_name() else { return sharded };
    let legacy = dir.join(name);
    if legacy.exists() { legacy } else { sharded }
}

/// The most header [`is_valid`] reads (the JSON line before the JPEG).
const HEADER_MAX: u64 = 4096;

/// The header field that records which version of the camera-look fit (`camera_preview::LOOK_VERSION`)
/// wrote the proxy's pixels (colour matrix and profile are baked into them) and stored tone curve.
const LOOK_FIELD: &str = "look_version";

/// The JPEG quality a smart preview's pixels are stored at. The original 92 left the 8×8 DCT's
/// quantised DC — each block's own mean, luma and chroma — visible as block steps across the frame
/// and as a small constant colour shift; that is the largest part of what a proxy gets wrong
/// against the original, and the only part a parameter can reach. 97 shrinks those quantisers to
/// roughly 1/2.6 of their steps, which is what removes the steps, at about twice the file size
/// (~1 MB per preview at 2560 px). What is left is the 8-bit sRGB quantisation itself, which only
/// a lossless proxy format can close. (ICED: the measured change is printed by the fidelity test
/// below; drop this to 95 for ~1.4× the size and roughly half the gain.)
const JPEG_QUALITY: u8 = 97;

/// The header field that records which version of this module's own encoding wrote a proxy's pixels
/// (the source mapping above and [`JPEG_QUALITY`]). Not to be confused with [`LOOK_FIELD`], which
/// tracks the camera-look fit that is baked into those pixels.
const ENC_FIELD: &str = "enc";

/// The value of [`ENC_FIELD`] this build writes. Bump it whenever [`encode`]'s mapping, JPEG
/// settings or stored facts change, so proxies an older build wrote are rebuilt: a missing field
/// reads as 0, which makes every proxy from before this version stale. Version 2 is the first to
/// record the field at all (a version 1 proxy is stored at JPEG quality 92, which this build
/// raises to [`JPEG_QUALITY`]); version 3 is the first to store the decoder facts in the tail. A
/// stale proxy whose original is offline is kept as it is (see [`is_stale`]).
/// Crate-visible so the look-marker test pins the marker format to it instead of to a literal.
pub(crate) const ENCODE_VERSION: u64 = 3;

/// The decoder facts a render interprets its settings against, stored with the proxy's pixels so
/// an offline original edits like it did online (the catalog's header facts cannot carry them).
/// In the proxy's tail, not its header: a real file's gain table map runs far past any header
/// budget (up to 12 336 gains).
#[derive(serde::Serialize, serde::Deserialize)]
struct SmartFacts {
    raw: bool,
    as_shot_temp: f64,
    as_shot_tint: f64,
    relative_wb: bool,
    legacy_relative_wb: bool,
    lens: Option<lightcraft_develop::EmbeddedLens>,
    camera_color: Option<lightcraft_pipeline::CameraColor>,
    local_tone: Option<lightcraft_pipeline::local_tone::LocalTone>,
    /// The semantic mattes whose bytes follow the facts, in this order.
    #[serde(default)]
    mattes: Vec<StoredMatte>,
}

/// One stored matte's shape; its `len` bytes of 0..255 alpha follow the facts.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredMatte {
    kind: u8,
    w: usize,
    h: usize,
    len: usize,
}

/// The facts and mattes a proxy's tail holds: the length-prefixed JSON, then one byte plane per
/// stored matte. `None` for an old proxy or a malformed tail; like a missing curve, the offline
/// render then keeps its header facts.
fn stored_facts(head: &serde_json::Value, bytes: &[u8]) -> Option<(SmartFacts, Mattes)> {
    let tail = usize::try_from(head.get("tail")?.as_u64()?).ok()?;
    if tail == 0 || tail > bytes.len() {
        return None;
    }
    let t = bytes.get(bytes.len() - tail..)?;
    let n = u32::from_le_bytes(t.get(..4)?.try_into().ok()?) as usize;
    let facts: SmartFacts = serde_json::from_slice(t.get(4..4 + n)?).ok()?;
    let mut mattes = Mattes::default();
    let mut at = 4 + n;
    for m in &facts.mattes {
        let kind = MatteKind::from_code(m.kind)?;
        if m.w == 0 || m.h == 0 || m.w.checked_mul(m.h)? != m.len {
            return None;
        }
        mattes.push(kind, Image { width: m.w, height: m.h, data: t.get(at..at.checked_add(m.len)?)?.to_vec() });
        at += m.len;
    }
    if at != t.len() {
        return None;
    }
    Some((facts, mattes))
}

/// Encode a source image and its decoder facts (the camera tone curve and the [`SourceInfo`] the
/// render interprets settings against, if given) as a smart preview.
pub fn encode(img: &Rgb32f, info: Option<&SourceInfo>) -> Result<Vec<u8>, String> {
    // scale so all but the brightest 0.05 % fit into 0..1
    let mut lum: Vec<f32> = img.data.iter().map(|c| c[0].max(c[1]).max(c[2])).filter(|v| v.is_finite()).collect();
    let scale = if lum.is_empty() {
        1.0
    } else {
        let k = ((lum.len() as f64) * 0.9995) as usize;
        let k = k.min(lum.len() - 1);
        let (_, v, _) = lum.select_nth_unstable_by(k, f32::total_cmp);
        v.max(1.0)
    };
    let data: Vec<[u8; 4]> = img
        .data
        .iter()
        .map(|c| {
            let e = |v: f32| (linear_to_srgb((v / scale).clamp(0.0, 1.0)) * 255.0).round() as u8;
            [e(c[0]), e(c[1]), e(c[2]), 255]
        })
        .collect();
    let rgba = Rgba8 { width: img.width, height: img.height, data };
    let jpg = lightcraft_codecs::encode_jpeg(
        &lightcraft_codecs::EncodeImage::rgba8(&rgba),
        JPEG_QUALITY,
        lightcraft_codecs::ChromaSubsampling::S444,
        &Default::default(),
    )
    .map_err(|e| e.to_string())?;
    let mut out = MAGIC.to_vec();
    let mut head = serde_json::json!({
        "w": img.width,
        "h": img.height,
        "scale": scale,
        LOOK_FIELD: crate::camera_preview::LOOK_VERSION,
        ENC_FIELD: ENCODE_VERSION,
    });
    // The tail: the decoder facts (and the file's semantic mattes) after the JPEG, so the header
    // stays small. `tail` records its byte count from the end of the file, so a header rewritten
    // in place (an edited look version) keeps pointing at it.
    let mut tail: Vec<u8> = Vec::new();
    if let Some(i) = info {
        if let Some(t) = &i.camera_tone {
            head["tone"] = serde_json::to_value(t).map_err(|e| e.to_string())?;
        }
        let matte_list: Vec<StoredMatte> = i
            .mattes
            .as_deref()
            .map(|m| m.iter().map(|(kind, a)| StoredMatte { kind: kind.code(), w: a.width, h: a.height, len: a.data.len() }).collect())
            .unwrap_or_default();
        let facts = SmartFacts {
            raw: i.raw,
            as_shot_temp: i.as_shot_temp,
            as_shot_tint: i.as_shot_tint,
            relative_wb: i.relative_wb,
            legacy_relative_wb: i.legacy_relative_wb,
            lens: i.lens,
            camera_color: i.camera_color.as_deref().cloned(),
            local_tone: i.local_tone.as_deref().cloned(),
            mattes: matte_list,
        };
        let facts = serde_json::to_vec(&facts).map_err(|e| e.to_string())?;
        let n = u32::try_from(facts.len()).map_err(|_| "smart preview facts too large")?;
        tail.extend_from_slice(&n.to_le_bytes());
        tail.extend_from_slice(&facts);
        if let Some(m) = i.mattes.as_deref() {
            for (_, a) in m.iter() {
                tail.extend_from_slice(&a.data);
            }
        }
        head["tail"] = serde_json::json!(tail.len());
    }
    out.extend_from_slice(head.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&jpg);
    out.extend_from_slice(&tail);
    Ok(out)
}

/// Decode a smart preview back into a source image, its stored camera tone curve and the stored
/// decoder facts (an invalid curve or tail is ignored, like a missing one).
pub fn decode(bytes: &[u8]) -> Result<(Rgb32f, Option<CameraTone>, Option<SourceInfo>), String> {
    let rest = bytes.strip_prefix(MAGIC).ok_or("not a smart preview")?;
    let nl = rest.iter().position(|b| *b == b'\n').ok_or("bad smart preview")?;
    let head: serde_json::Value = serde_json::from_slice(&rest[..nl]).map_err(|e| e.to_string())?;
    let scale = head["scale"].as_f64().unwrap_or(1.0) as f32;
    let tone = head.get("tone").and_then(|t| serde_json::from_value::<CameraTone>(t.clone()).ok());
    let tail = head.get("tail").and_then(serde_json::Value::as_u64).and_then(|t| usize::try_from(t).ok()).unwrap_or(0);
    let start = MAGIC.len() + nl + 1;
    let end = bytes.len().saturating_sub(tail);
    if end <= start {
        return Err("bad smart preview".into());
    }
    let d = lightcraft_codecs::decode(bytes.get(start..end).ok_or("bad smart preview")?, Default::default()).map_err(|e| e.to_string())?;
    // the decoder undoes the sRGB encoding; the values are the source's own primaries
    let mut img = d.image;
    img.data.iter_mut().for_each(|c| *c = c.map(|v| v * scale));
    let info = stored_facts(&head, bytes).map(|(f, mattes)| SourceInfo {
        raw: f.raw,
        as_shot_temp: f.as_shot_temp,
        as_shot_tint: f.as_shot_tint,
        relative_wb: f.relative_wb,
        legacy_relative_wb: f.legacy_relative_wb,
        lens: f.lens,
        camera_color: f.camera_color.map(Arc::new),
        camera_tone: tone,
        mattes: (!mattes.is_empty()).then(|| Arc::new(mattes)),
        local_tone: f.local_tone.map(Arc::new),
    });
    Ok((img, tone, info))
}

/// Where a library keeps its smart previews.
pub fn dir(library: &Path) -> PathBuf {
    library.join("Smart Previews")
}

/// Check that smart previews can be written to `dir`: it must be (or, when its parent exists,
/// become) a directory that accepts a new file. Never creates missing parents, so an unplugged
/// drive's mount point is not silently recreated on the system drive.
pub fn check_writable(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        let parent_ok = dir.parent().is_some_and(|p| p.is_dir());
        if !parent_ok {
            return Err(format!("{} is not available — reconnect the drive or choose another folder", dir.display()));
        }
        std::fs::create_dir(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let probe = dir.join(format!(".lightcraft-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"x").map_err(|e| format!("{} is not writable (is the drive full or read-only?): {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// The owned proxy files in both layouts. Do not follow symlinked directories or
/// traverse unrelated folders inside a user-selected smart preview directory.
fn files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found = Vec::new();
    for e in entries.flatten() {
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_file() && e.path().extension().is_some_and(|ext| ext == "lcsp") {
            found.push(e.path()); // legacy layout
        } else if kind.is_dir() {
            let shard_name = e.file_name();
            let Some(shard) = shard_name.to_str() else { continue };
            if shard.len() != 2 || !shard.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                continue;
            }
            let Ok(children) = std::fs::read_dir(e.path()) else { continue };
            for child in children.flatten() {
                let name = child.file_name();
                let Some(name) = name.to_str() else { continue };
                let Some(stem) = name.strip_suffix(".lcsp") else { continue };
                if stem.len() == 32
                    && stem.starts_with(shard)
                    && stem.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    && child.file_type().is_ok_and(|kind| kind.is_file())
                {
                    found.push(child.path());
                }
            }
        }
    }
    found
}

/// The number of smart previews in `dir` and their total size in bytes.
pub fn stats(dir: &Path) -> (usize, u64) {
    files(dir).into_iter().fold((0usize, 0u64), |(count, bytes), path| {
        let size = std::fs::metadata(path).map_or(0, |m| m.len());
        (count.saturating_add(1), bytes.saturating_add(size))
    })
}

/// What to do with the smart previews in the old folder when the location changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existing {
    /// Move them to the new folder (copy and delete across drives).
    Move,
    /// Leave them where they are (they stop being used; build again in the new folder).
    Leave,
    /// Delete them.
    Discard,
}

impl Existing {
    pub fn parse(s: &str) -> Option<Existing> {
        match s {
            "move" => Some(Existing::Move),
            "leave" => Some(Existing::Leave),
            "discard" => Some(Existing::Discard),
            _ => None,
        }
    }
}

/// Apply `what` to the smart previews in `from` for the new folder `to` → (moved or removed, failed).
pub fn migrate(from: &Path, to: &Path, what: Existing) -> (usize, Vec<String>) {
    let (mut done, mut failed) = (0, Vec::new());
    if what == Existing::Leave {
        return (done, failed);
    }
    for src in files(from) {
        let Some(name) = src.file_name() else { continue };
        let dst = name
            .to_str()
            .and_then(|n| n.strip_suffix(".lcsp"))
            .filter(|stem| stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
            .map_or_else(|| to.join(name), |stem| to.join(&stem[..2]).join(name));
        let r = match what {
            Existing::Discard => std::fs::remove_file(&src),
            _ => {
                let parent = dst.parent().unwrap_or(to);
                // Create only the shard, never missing parent directories on an unplugged drive.
                let prepare = if parent.is_dir() { Ok(()) } else { std::fs::create_dir(parent) };
                prepare.and_then(|()| {
                    if is_valid(&dst) {
                        // named by content: the complete copy already there is the same preview
                        std::fs::remove_file(&src)
                    } else {
                        // across drives: copied atomically (never a partial proxy at the new place)
                        std::fs::rename(&src, &dst).or_else(|_| {
                            std::fs::read(&src)
                                .and_then(|b| lightcraft_catalog::safe_file::write_atomic(&dst, &b))
                                .and_then(|()| std::fs::remove_file(&src))
                        })
                    }
                })
            }
        };
        match r {
            Ok(()) => done += 1,
            Err(err) => failed.push(format!("{}: {err}", src.display())),
        }
    }
    (done, failed)
}

/// A cheap check that the smart preview at `path` is complete: our header, then a JPEG from its
/// start marker to its end marker. A file cut short by a crash, a full drive or an unplugged one
/// fails it (and is then rebuilt by Build Smart Previews, not counted as there).
pub fn is_valid(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = Vec::with_capacity(1024);
    if (&mut f).take(HEADER_MAX).read_to_end(&mut head).is_err() {
        return false;
    }
    let Some(rest) = head.strip_prefix(MAGIC) else { return false };
    let Some(nl) = rest.iter().position(|b| *b == b'\n') else { return false };
    let json = rest.get(..nl).and_then(|h| serde_json::from_slice::<serde_json::Value>(h).ok());
    let size_ok = json.as_ref().is_some_and(|h| h["w"].as_u64().is_some_and(|w| w > 0) && h["h"].as_u64().is_some_and(|h| h > 0));
    if !size_ok || rest.get(nl + 1..nl + 3) != Some(&[0xFF, 0xD8][..]) {
        return false;
    }
    let tail = json.and_then(|h| h.get("tail").and_then(serde_json::Value::as_u64)).and_then(|t| usize::try_from(t).ok()).unwrap_or(0);
    if tail == 0 {
        // no tail: the file ends at the JPEG's end marker
        let mut end = [0u8; 2];
        return f.seek(SeekFrom::End(-2)).is_ok() && f.read_exact(&mut end).is_ok() && end == [0xFF, 0xD9];
    }
    // with a tail, the JPEG must end right before it and the tail must be there in full
    let Ok(meta) = f.metadata() else { return false };
    let Ok(file_len) = usize::try_from(meta.len()) else { return false };
    let Some(jpeg_end) = file_len.checked_sub(tail) else { return false };
    if jpeg_end <= MAGIC.len() + nl + 3 {
        return false;
    }
    let mut end = [0u8; 2];
    f.seek(SeekFrom::Start((jpeg_end - 2) as u64)).is_ok() && f.read_exact(&mut end).is_ok() && end == [0xFF, 0xD9]
}

/// The fit version a header records: a missing, negative, fractional or otherwise malformed value
/// counts as 0 (older than any real version).
fn header_look_version(head: &serde_json::Value) -> u64 {
    head.get(LOOK_FIELD).and_then(serde_json::Value::as_u64).unwrap_or(0)
}

/// The encode version a header records: a missing, negative, fractional or otherwise malformed
/// value counts as 0 (older than any real version), like [`header_look_version`].
fn header_encode_version(head: &serde_json::Value) -> u64 {
    head.get(ENC_FIELD).and_then(serde_json::Value::as_u64).unwrap_or(0)
}

/// Whether the complete proxy at `path` is out of date for this build: written by an older
/// camera-look fit than this build's (so its stored curve, and the colour baked into its pixels, are
/// out of date), or by an older encoding of its own pixels ([`ENCODE_VERSION`], i.e. a lower JPEG
/// quality). A proxy from a newer build is left alone (rebuilding it here would only undo that
/// build's work), as is one that can't be read: those are [`is_valid`]'s business.
pub fn is_stale(path: &Path) -> bool {
    use std::io::Read;
    if !is_valid(path) {
        return false;
    }
    let Ok(f) = std::fs::File::open(path) else { return false };
    let mut head = Vec::with_capacity(1024);
    if f.take(HEADER_MAX).read_to_end(&mut head).is_err() {
        return false;
    }
    let Some(rest) = head.strip_prefix(MAGIC) else { return false };
    let Some(nl) = rest.iter().position(|b| *b == b'\n') else { return false };
    rest.get(..nl)
        .and_then(|h| serde_json::from_slice::<serde_json::Value>(h).ok())
        .is_some_and(|h| header_look_version(&h) < u64::from(crate::camera_preview::LOOK_VERSION) || header_encode_version(&h) < ENCODE_VERSION)
}

/// The marker file in a smart previews folder: the camera-look fit version and encode version its
/// proxies were last checked at ([`look_checked`]).
const LOOK_MARKER: &str = "look-version";

/// Whether the smart previews folder `dir` has been checked for out-of-date proxies at this build's
/// camera-look fit version and encode version (or newer ones). A missing or unreadable marker counts
/// as not, and so does one written before the encode version was recorded — it only says the look
/// was checked, so this build scans the folder once more and rebuilds the proxies [`is_stale`]
/// finds.
pub fn look_checked(dir: &Path) -> bool {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(dir.join(LOOK_MARKER)) else { return false };
    let mut text = String::new();
    if f.take(64).read_to_string(&mut text).is_err() {
        return false;
    }
    let mut versions = text.split_whitespace();
    let (Some(look), Some(enc)) = (versions.next(), versions.next()) else { return false };
    match (look.parse::<u64>(), enc.parse::<u64>()) {
        (Ok(look), Ok(enc)) => look >= u64::from(crate::camera_preview::LOOK_VERSION) && enc >= ENCODE_VERSION,
        _ => false,
    }
}

/// Record that the proxies in `dir` were checked at this build's camera-look fit version and encode
/// version.
pub fn mark_look_checked(dir: &Path) -> Result<(), String> {
    let path = dir.join(LOOK_MARKER);
    let text = format!("{} {}\n", crate::camera_preview::LOOK_VERSION, ENCODE_VERSION);
    lightcraft_catalog::safe_file::write_atomic(&path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Load the proxy at `path`.
pub fn load(path: &Path) -> Result<crate::media::DecodedSource, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode(&b).map(|(image, camera_tone, info)| crate::media::DecodedSource { image: Arc::new(image), info, camera_tone, denoised: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-smart-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writable_check_does_not_create_missing_parents() {
        let base = temp("writable");
        assert!(check_writable(&base.join("new")).is_ok(), "one missing level is created");
        assert!(base.join("new").is_dir());
        let err = check_writable(&base.join("gone/deeper")).unwrap_err();
        assert!(err.contains("not available"), "{err}");
        assert!(!base.join("gone").exists(), "no fallback onto the current drive");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn migrate_moves_leaves_or_discards() {
        let base = temp("migrate");
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("1.lcsp"), b"one").unwrap();
        std::fs::write(a.join("note.txt"), b"keep").unwrap();
        assert_eq!(stats(&a), (1, 3));
        assert_eq!(migrate(&a, &b, Existing::Leave).0, 0);
        assert_eq!(stats(&a).0, 1);
        assert_eq!(migrate(&a, &b, Existing::Move).0, 1);
        assert_eq!((stats(&a).0, stats(&b).0), (0, 1));
        assert!(a.join("note.txt").exists(), "only smart previews are touched");
        assert_eq!(migrate(&b, &a, Existing::Discard).0, 1);
        assert_eq!(stats(&b).0, 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn sharded_and_legacy_proxies_are_counted_and_moved() {
        let base = temp("shards");
        let from = base.join("from");
        let to = base.join("to");
        std::fs::create_dir_all(from.join("a1")).unwrap();
        std::fs::create_dir(&to).unwrap();
        let old = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2.lcsp";
        let new = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1.lcsp";
        std::fs::write(from.join(old), b"legacy").unwrap();
        std::fs::write(from.join("a1").join(new), b"sharded").unwrap();
        std::fs::write(from.join("a1").join("notes.txt"), b"user file").unwrap();
        std::fs::write(from.join("notes.lcsp.txt"), b"keep").unwrap();
        assert_eq!(stats(&from), (2, 13));
        let (moved, failed) = migrate(&from, &to, Existing::Move);
        assert_eq!((moved, failed), (2, Vec::<String>::new()));
        assert_eq!(stats(&from), (0, 0));
        assert_eq!(stats(&to), (2, 13));
        assert_eq!(std::fs::read(to.join("b2").join(old)).unwrap(), b"legacy");
        assert_eq!(std::fs::read(to.join("a1").join(new)).unwrap(), b"sharded");
        assert!(from.join("a1").join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn damaged_proxies_are_invalid_and_never_kept_over_good_ones() {
        let base = temp("valid");
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let img = lightcraft_scenes::demo_library()[0].render(64, 40);
        let good = encode(&img, None).unwrap();
        std::fs::write(a.join("1.lcsp"), &good).unwrap();
        assert!(is_valid(&a.join("1.lcsp")));
        for cut in [good.len() - 1, good.len() / 2, 10, 0] {
            std::fs::write(b.join("1.lcsp"), &good[..cut]).unwrap();
            assert!(!is_valid(&b.join("1.lcsp")), "cut at {cut}");
        }
        assert!(!is_valid(&b.join("missing.lcsp")));
        // moving onto a damaged copy replaces it instead of deleting the good one
        assert_eq!(migrate(&a, &b, Existing::Move).0, 1);
        assert_eq!(std::fs::read(b.join("1.lcsp")).unwrap(), good);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn camera_tone_travels_with_the_proxy() {
        let tone = CameraTone::new(std::array::from_fn(|i| {
            let x = 0.004 * 1.17f32.powi(i as i32);
            [x, 0.95 * (1.0 - (-2.7 * x).exp())]
        }))
        .unwrap();
        let img = lightcraft_scenes::demo_library()[0].render(64, 40);
        let info = lightcraft_pipeline::SourceInfo { raw: true, camera_tone: Some(tone), ..Default::default() };
        let bytes = encode(&img, Some(&info)).unwrap();
        assert_eq!(decode(&bytes).unwrap().1, Some(tone));
        let dir = temp("tone");
        let path = dir.join("t.lcsp");
        std::fs::write(&path, &bytes).unwrap();
        // the longer header is still read by the validity check, and the curve by `load`
        assert!(is_valid(&path));
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.camera_tone, Some(tone));
        let header = lightcraft_pipeline::SourceInfo { raw: true, ..Default::default() };
        assert_eq!(loaded.info_or(header).camera_tone, Some(tone));
        // a hostile curve (reversing knots) is ignored, not trusted
        let nl = MAGIC.len() + bytes[MAGIC.len()..].iter().position(|b| *b == b'\n').unwrap();
        let mut head: serde_json::Value = serde_json::from_slice(&bytes[MAGIC.len()..nl]).unwrap();
        let mut knots: Vec<[f32; 2]> = serde_json::from_value(head["tone"]["knots"].clone()).unwrap();
        knots.swap(3, 4);
        head["tone"]["knots"] = serde_json::json!(knots);
        let mut hostile = MAGIC.to_vec();
        hostile.extend_from_slice(head.to_string().as_bytes());
        hostile.extend_from_slice(&bytes[nl..]);
        let (pixels, tone, _) = decode(&hostile).unwrap();
        assert_eq!((pixels.width, tone), (img.width, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn facts_and_mattes_travel_with_the_proxy() {
        let img = lightcraft_scenes::demo_library()[0].render(32, 20);
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Sky, Image { width: 16, height: 10, data: vec![255; 160] });
        let info = SourceInfo {
            raw: true,
            as_shot_temp: 4200.0,
            as_shot_tint: -5.0,
            legacy_relative_wb: true,
            mattes: Some(std::sync::Arc::new(mattes)),
            ..Default::default()
        };
        let bytes = encode(&img, Some(&info)).unwrap();
        let (_, _, back) = decode(&bytes).unwrap();
        let back = back.expect("the facts are read back");
        assert_eq!((back.as_shot_temp, back.as_shot_tint, back.legacy_relative_wb), (4200.0, -5.0, true));
        assert_eq!(back.mattes.expect("the mattes are read back").bytes(), 160);
        // a proxy cut inside its tail is incomplete (rebuilt, not taken for a source of facts)
        let dir = temp("facts");
        let path = dir.join("f.lcsp");
        std::fs::write(&path, &bytes[..bytes.len() - 8]).unwrap();
        assert!(!is_valid(&path));
        let _ = std::fs::remove_dir_all(&dir);
        // an old proxy (no tail) has no facts, and neither has one with a malformed tail
        assert!(decode(&encode(&img, None).unwrap()).unwrap().2.is_none());
        let nl = MAGIC.len() + bytes[MAGIC.len()..].iter().position(|b| *b == b'\n').unwrap();
        let mut head: serde_json::Value = serde_json::from_slice(&bytes[MAGIC.len()..nl]).unwrap();
        head["tail"] = serde_json::json!(bytes.len());
        let mut lying = MAGIC.to_vec();
        lying.extend_from_slice(head.to_string().as_bytes());
        lying.extend_from_slice(&bytes[nl..]);
        assert!(decode(&lying).map(|(_, _, i)| i.is_none()).unwrap_or(true), "an over-large tail is not trusted");
    }

    #[test]
    fn roundtrip_keeps_the_picture() {
        let img = lightcraft_scenes::demo_library()[0].render(160, 100);
        let (back, tone, _) = decode(&encode(&img, None).unwrap()).unwrap();
        assert_eq!((back.width, back.height), (img.width, img.height));
        assert!(tone.is_none());
        let mut err = 0.0f64;
        for (a, b) in img.data.iter().zip(&back.data) {
            for k in 0..3 {
                let (x, y) = (linear_to_srgb(a[k].clamp(0.0, 1.0)), linear_to_srgb(b[k].clamp(0.0, 1.0)));
                err += ((x - y) as f64).abs();
            }
        }
        let mean = err / (img.data.len() * 3) as f64;
        assert!(mean < 0.02, "mean encoded error {mean}");
        assert!(decode(b"nope").is_err());
    }

    /// A smooth, deterministic stand-in for a photo: a bright, near-neutral "sky" crossing over into
    /// a saturated colour field, every value inside 0..1 so the proxy's 0.05 % highlight clip never
    /// enters what is measured. Built from a rendered demo scene so the dimensions and the flat
    /// row-major layout are the real ones.
    fn smooth_scene(w: usize, h: usize) -> Rgb32f {
        let mut img = lightcraft_scenes::demo_library()[0].render(w, h);
        let (w, h) = (img.width.max(1), img.height.max(1));
        for (i, px) in img.data.iter_mut().enumerate() {
            let u = (i % w) as f32 / (w - 1).max(1) as f32;
            let v = (i / w) as f32 / (h - 1).max(1) as f32;
            let t = (v * v * (3.0 - 2.0 * v)).clamp(0.0, 1.0); // smoothstep: a fade, not a step
            let sky = [0.10 + 0.70 * v, 0.14 + 0.72 * v, 0.30 + 0.62 * v];
            let field = [0.86 - 0.26 * u, 0.18 + 0.62 * u, 0.12 + 0.30 * (1.0 - u)];
            *px = std::array::from_fn(|k| (sky[k] * (1.0 - t) + field[k] * t).clamp(0.0, 1.0));
        }
        img
    }

    /// CIE L*a*b* (D65) of a scene-linear colour: the perceptual yardstick a proxy is held to. The
    /// input is clamped into 0..1, the only range a proxy ever stores.
    fn lab(c: [f32; 3]) -> [f64; 3] {
        let (r, g, b) = (c[0].clamp(0.0, 1.0) as f64, c[1].clamp(0.0, 1.0) as f64, c[2].clamp(0.0, 1.0) as f64);
        let (x, y, z) = (
            (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047,
            0.2126 * r + 0.7152 * g + 0.0722 * b,
            (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883,
        );
        let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (841.0 / 108.0) * t + 4.0 / 29.0 };
        let (fx, fy, fz) = (f(x), f(y), f(z));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    /// The mean CIE76 ΔE between two same-sized scene-linear images: monotone in CIEDE2000 for a
    /// low-contrast, near-neutral difference like a proxy's, and enough to rank two encoders.
    fn mean_delta_e(a: &Rgb32f, b: &Rgb32f) -> f64 {
        let n = a.data.len().min(b.data.len());
        if n == 0 {
            return 0.0;
        }
        (0..n)
            .map(|i| {
                let (p, q) = (lab(a.data[i]), lab(b.data[i]));
                ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
            })
            .sum::<f64>()
            / n as f64
    }

    /// A deliberately frozen copy of the pixel mapping and container this module used before the
    /// quality raise: the 99.95 % highlight scale, the sRGB 8-bit mapping, JPEG quality 92, and the
    /// version-1 header (width, height, scale and camera-look fit — no encode version). A copy on
    /// purpose: it must not follow `encode` as that changes, so the fidelity test measures what the
    /// quality raise bought instead of trusting a written-down number.
    fn encode_at_q92(img: &Rgb32f) -> Vec<u8> {
        let mut lum: Vec<f32> = img.data.iter().map(|c| c[0].max(c[1]).max(c[2])).filter(|v| v.is_finite()).collect();
        let scale = if lum.is_empty() {
            1.0
        } else {
            let k = ((lum.len() as f64) * 0.9995) as usize;
            let k = k.min(lum.len() - 1);
            let (_, v, _) = lum.select_nth_unstable_by(k, f32::total_cmp);
            v.max(1.0)
        };
        let data: Vec<[u8; 4]> = img
            .data
            .iter()
            .map(|c| {
                let e = |v: f32| (linear_to_srgb((v / scale).clamp(0.0, 1.0)) * 255.0).round() as u8;
                [e(c[0]), e(c[1]), e(c[2]), 255]
            })
            .collect();
        let rgba = Rgba8 { width: img.width, height: img.height, data };
        let jpg = lightcraft_codecs::encode_jpeg(
            &lightcraft_codecs::EncodeImage::rgba8(&rgba),
            92,
            lightcraft_codecs::ChromaSubsampling::S444,
            &Default::default(),
        )
        .unwrap();
        // the version-1 container the decoder needs: magic, header, JPEG — and no `enc` field
        let mut out = MAGIC.to_vec();
        let head = serde_json::json!({"w": img.width, "h": img.height, "scale": scale, LOOK_FIELD: crate::camera_preview::LOOK_VERSION});
        out.extend_from_slice(head.to_string().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(&jpg);
        out
    }

    /// The mean CIE76 ΔE between two same-sized scene-linear images, each reduced first to one
    /// sample per 8×8 block — the block's mean colour. JPEG stores a block's DC coefficient, its
    /// mean, and rebuilds the whole block from it, so a proxy differs from the original by a
    /// constant per block: the "8×8 block pattern" a preview shows. Averaging a block's 64 pixels
    /// keeps that constant while shrinking the 8-bit sRGB rounding the two encoders share
    /// eightfold, which is what a per-pixel ΔE drowns in.
    fn mean_block_delta_e(a: &Rgb32f, b: &Rgb32f) -> f64 {
        let w = a.width.min(b.width).max(1);
        let h = a.height.min(b.height).max(1);
        let (mut sum, mut n) = (0.0f64, 0usize);
        for by in (0..h).step_by(8) {
            for bx in (0..w).step_by(8) {
                let (mut sa, mut sb, mut count) = ([0.0f64; 3], [0.0f64; 3], 0u32);
                for y in by..(by + 8).min(h) {
                    for x in bx..(bx + 8).min(w) {
                        if let (Some(ca), Some(cb)) = (a.data.get(y * a.width + x), b.data.get(y * b.width + x)) {
                            for k in 0..3 {
                                sa[k] += f64::from(ca[k]);
                                sb[k] += f64::from(cb[k]);
                            }
                            count += 1;
                        }
                    }
                }
                let d = f64::from(count.max(1));
                let (pa, pb) = (
                    lab([(sa[0] / d) as f32, (sa[1] / d) as f32, (sa[2] / d) as f32]),
                    lab([(sb[0] / d) as f32, (sb[1] / d) as f32, (sb[2] / d) as f32]),
                );
                sum += ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                n += 1;
            }
        }
        sum / n.max(1) as f64
    }

    /// for [`a_smooth_proxy_keeps_the_scene_closer_than_the_old_quality`] to pass. The shared 8-bit
    /// sRGB quantization floor both encoders share caps the achievable ratio at 0.632 on this scene,
    /// a no-op encoder scores 1.000, and this is their geometric midpoint: it fails the moment the
    /// quality raise is reverted and never trips on the honest result (measured 0.683).
    const PROXY_BLOCK_GAIN: f64 = 0.8;

    #[test]
    fn a_smooth_proxy_keeps_the_scene_closer_than_the_old_quality() {
        let img = smooth_scene(256, 160);
        let ours = decode(&encode(&img, None).unwrap()).unwrap().0;
        let before = decode(&encode_at_q92(&img)).unwrap().0;
        let block = (mean_block_delta_e(&img, &ours), mean_block_delta_e(&img, &before));
        let pixel = (mean_delta_e(&img, &ours), mean_delta_e(&img, &before));
        println!(
            "smooth-scene proxy round-trip: block-mean ΔE76 {:.3} at q{JPEG_QUALITY} vs {:.3} at q92 \
             (per-pixel {:.3} vs {:.3})",
            block.0, block.1, pixel.0, pixel.1
        );
        assert!(block.1 > 0.0, "the old encoder must measure something to compare against");
        assert!(
            block.0 <= PROXY_BLOCK_GAIN * block.1,
            "q{JPEG_QUALITY} leaves a block-mean ΔE of {:.3}, not the ≤{:.3} the quality raise should buy over q92's {:.3} \
             (the 8-bit sRGB floor caps the achievable ratio at ~0.63 on this scene)",
            block.0,
            PROXY_BLOCK_GAIN * block.1,
            block.1
        );
    }

    #[test]
    fn a_proxy_from_an_older_encode_is_stale() {
        let dir = temp("encode-version");
        let img = lightcraft_scenes::demo_library()[0].render(64, 40);
        let bytes = encode(&img, None).unwrap();
        let path = dir.join("p.lcsp");
        std::fs::write(&path, &bytes).unwrap();
        assert!(!is_stale(&path), "this build's own proxy is up to date");
        // one written before the encode version was recorded is not
        let nl = MAGIC.len() + bytes[MAGIC.len()..].iter().position(|b| *b == b'\n').unwrap();
        let head: serde_json::Value = serde_json::from_slice(&bytes[MAGIC.len()..nl]).unwrap();
        assert_eq!(head[ENC_FIELD].as_u64(), Some(ENCODE_VERSION));
        let mut older = head.as_object().cloned().unwrap_or_default();
        older.remove(ENC_FIELD);
        let mut old = MAGIC.to_vec();
        old.extend_from_slice(serde_json::Value::Object(older).to_string().as_bytes());
        old.extend_from_slice(&bytes[nl..]);
        std::fs::write(&path, &old).unwrap();
        assert!(is_valid(&path), "losing the field keeps the file readable");
        assert!(is_stale(&path), "an older encoding is rebuilt");
        // the marker records both versions, and one from before the encode version forces a scan
        assert!(!look_checked(&dir));
        mark_look_checked(&dir).unwrap();
        assert!(look_checked(&dir));
        std::fs::write(dir.join(LOOK_MARKER), "99\n").unwrap();
        assert!(!look_checked(&dir), "a look-only marker is not enough to skip the rebuild");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
