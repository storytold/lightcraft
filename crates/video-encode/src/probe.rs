//! Test helper (`test-util`): decode the luma planes of an H.264 MP4 a [`crate::SequenceWriter`]
//! wrote, in presentation order, so tests can count frames and compare their brightness.

use std::path::Path;

use crate::{Result, VideoError};

/// One decoded frame's luma plane (8-bit limited-range codes, cropped, tightly packed).
#[derive(Clone, Debug, PartialEq)]
pub struct Luma {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Luma {
    /// Mean code value.
    pub fn mean(&self) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data.iter().map(|&v| f32::from(v)).sum::<f32>() / self.data.len() as f32
    }

    /// Code value at (x, y), 0 outside.
    pub fn at(&self, x: usize, y: usize) -> u8 {
        if x >= self.width {
            return 0;
        }
        self.data.get(y * self.width + x).copied().unwrap_or(0)
    }
}

fn bad(e: impl std::fmt::Display) -> VideoError {
    VideoError::Io(e.to_string())
}

/// Every frame of the first H.264 video track of the MP4 at `path`.
pub fn h264_lumas(path: &Path) -> Result<Vec<Luma>> {
    let bytes = std::fs::read(path).map_err(bad)?;
    let file = filmcraft_isobmff::open(bytes.as_slice()).map_err(bad)?;
    let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).ok_or_else(|| bad("no video track"))?;
    let track = file.tracks.get(t).ok_or_else(|| bad("no video track"))?;
    let Some(filmcraft_isobmff::CodecConfig::Avc(avcc)) = track.codec() else { return Err(bad("not H.264")) };
    let mut dec = filmcraft_h264::Decoder::from_avcc(&avcc.to_bytes()).map_err(bad)?;
    let mut pics = Vec::new();
    for (i, s) in track.samples.iter().enumerate() {
        pics.extend(dec.decode(&file.read_sample(bytes.as_slice(), t, i).map_err(bad)?, s.pts).map_err(bad)?);
    }
    pics.extend(dec.flush());
    pics.sort_by_key(|p| p.pts);
    Ok(pics
        .iter()
        .map(|p| {
            let (w, h) = (p.width as usize, p.height as usize);
            let data = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| p.y.get(y * p.y_stride + x).copied().unwrap_or(0)).collect();
            Luma { width: w, height: h, data }
        })
        .collect())
}
