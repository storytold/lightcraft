//! Geometry probes for sidecars. They never hash pixels or build import metadata.

use std::io::{Read, Seek, SeekFrom};

use lightcraft_raw::sparse::{HeaderPlan, Ranges};

use super::{Orientation, ProbeInfo};

const MAX_FRAME_BYTES: u64 = 512 * 1024 * 1024;
const PREFIX: u64 = 64 * 1024;

pub fn fs_frame_probe() -> crate::media::FileProbe {
    std::sync::Arc::new(|path| {
        let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let total = file.metadata().map_err(|e| format!("{path}: {e}"))?.len();
        let _permit = crate::memory::work_gate().acquire(usize::try_from(total).unwrap_or(usize::MAX));
        if total > MAX_FRAME_BYTES {
            // Preserve the existing geometry path for unusually large images. The sparse
            // planner's buffer cap limits its allocation, not which photos can be cropped.
            let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            return frame_bytes(&bytes).map_err(|e| format!("{path}: {e}"));
        }
        read_frame(&mut file, total).map(|(info, _)| info).map_err(|e| format!("{path}: {e}"))
    })
}

/// Leave sample ranges unread where the raw reader can prove that its headers suffice. Some
/// formats need samples to determine their developed frame, so they retain a whole-file fallback.
/// The byte count lets synthetic tests check the actual I/O, rather than elapsed time.
fn read_frame(reader: &mut (impl Read + Seek), total: u64) -> Result<(ProbeInfo, u64), String> {
    if total > MAX_FRAME_BYTES {
        return Err("file too large for a sidecar geometry probe".into());
    }
    let len = usize::try_from(total).map_err(|_| "file too large")?;
    let mut bytes = vec![0u8; len];
    let mut loaded = Ranges::new();
    read_ranges(reader, &mut bytes, &mut loaded, &[(0, total.min(PREFIX))])?;
    let mut preview = lightcraft_raw::probe(&bytes).is_some_and(|f| !f.is_supported());
    if lightcraft_raw::probe(&bytes).is_some() || bytes.starts_with(b"II") || bytes.starts_with(b"MM") {
        for _ in 0..128 {
            let plan =
                if preview { lightcraft_raw::sparse::preview_plan(&bytes, &loaded) } else { lightcraft_raw::sparse::header_plan(&bytes, &loaded) };
            match plan {
                HeaderPlan::Need(ranges) => read_ranges(reader, &mut bytes, &mut loaded, ranges.spans())?,
                HeaderPlan::Ready => match frame_bytes(&bytes) {
                    Ok(info) => return Ok((info, loaded.total())),
                    Err(_) if !preview => preview = true,
                    Err(_) => break,
                },
                HeaderPlan::Whole(_) => break,
            }
        }
    }
    let missing = loaded.gaps(0, total);
    read_ranges(reader, &mut bytes, &mut loaded, missing.spans())?;
    frame_bytes(&bytes).map(|info| (info, loaded.total()))
}

fn read_ranges(reader: &mut (impl Read + Seek), bytes: &mut [u8], loaded: &mut Ranges, spans: &[(u64, u64)]) -> Result<(), String> {
    for &(start, end) in spans {
        let (a, b) = (usize::try_from(start).map_err(|_| "header offset too large")?, usize::try_from(end).map_err(|_| "header offset too large")?);
        let dst = bytes.get_mut(a..b).ok_or("header range outside file")?;
        reader.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        reader.read_exact(dst).map_err(|e| e.to_string())?;
        loaded.insert(start, end);
    }
    Ok(())
}

fn frame_bytes(bytes: &[u8]) -> Result<ProbeInfo, String> {
    let (mut width, mut height, orientation) = if lightcraft_raw::probe(bytes).is_some() {
        match lightcraft_raw::probe_info(bytes).map_err(|e| super::probe::preview_reason(bytes, e)) {
            Ok(raw) => {
                let (w, h) = raw.developed_size();
                (w.max(1) as u32, h.max(1) as u32, raw.orientation)
            }
            Err(Ok(_)) => {
                let (width, height, orientation) = super::preview::embedded_preview_frame(bytes).ok_or("raw without a usable preview")?;
                return Ok(ProbeInfo { width, height, orientation: Some(orientation), ..Default::default() });
            }
            Err(Err(e)) => return Err(e),
        }
    } else {
        let header = lightcraft_codecs::read_header(bytes).map_err(|e| e.to_string())?;
        (header.width, header.height, Orientation::from_exif(header.orientation))
    };
    if orientation.swaps_axes() {
        std::mem::swap(&mut width, &mut height);
    }
    Ok(ProbeInfo { width, height, orientation: Some(orientation), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_raw_frame_probe_skips_samples_and_hashing() {
        let mut seed = 42u32;
        let pixels = (0..800_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 16) & 0x3fff) as u16
            })
            .collect();
        let file = crate::tests_xmp::synthetic_dng_of(1000, 800, "RGGB", pixels, None, Default::default());
        let whole = crate::files::probe_bytes("frame.dng", &file).unwrap();
        let (frame, read) = read_frame(&mut std::io::Cursor::new(&file), file.len() as u64).unwrap();
        assert_eq!(frame.stored_frame(), whole.stored_frame());
        assert!(frame.content_hash.is_none());
        assert!(read < file.len() as u64 / 4, "read {read} of {} bytes", file.len());
    }
}
