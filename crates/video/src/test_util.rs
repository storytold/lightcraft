//! Synthetic clips for tests in other crates (feature `test-util`, dev-only): never media on disk
//! in the repository, a clip is encoded in the test that needs it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::Path;

use filmcraft_h264enc::{Encoder, EncoderConfig, PacketFormat, RateControl, YuvFrame};
use filmcraft_isobmff::{AvcConfig, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

/// Write a `w × h` 30 fps H.264 MP4 of `frames` flat grey frames (frame k's luma `luma(k)`,
/// chroma neutral) with udta `metadata`. Panics on failure: test code only.
pub fn write_clip(path: &Path, w: u32, h: u32, frames: usize, luma: impl Fn(usize) -> u8, metadata: &[(&str, &str)]) {
    let mut cfg = EncoderConfig::new(w, h, 30, 1);
    cfg.format = PacketFormat::LengthPrefixed;
    cfg.bframes = 0;
    cfg.keyint = 250;
    cfg.scenecut = false;
    cfg.rate = RateControl::Crf(12.0);
    cfg.threads = 1;
    let mut enc = Encoder::new(cfg).unwrap();
    let (wu, hu) = (w as usize, h as usize);
    let mut packets = Vec::new();
    for k in 0..frames {
        let (y, u, v) = (vec![luma(k); wu * hu], vec![128u8; wu * hu / 4], vec![128u8; wu * hu / 4]);
        let f = YuvFrame { y: &y, u: &u, v: &v, y_stride: wu, uv_stride: wu / 2 };
        packets.extend(enc.encode(&f, k as i64).unwrap());
    }
    packets.extend(enc.flush());
    let avcc = AvcConfig::parse(&enc.avcc()).unwrap();
    let mut opts = WriterOptions::new(Brand::Mp4);
    opts.metadata = metadata.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), opts).unwrap();
    let t = mux.add_track(TrackConfig::new(SampleEntry::avc(avcc, w as u16, h as u16), 30)).unwrap();
    for (k, p) in packets.iter().enumerate() {
        let off = (p.pts - k as i64) as i32;
        mux.write_sample(t, WriteSample { data: &p.data, duration: 1, composition_offset: off, is_sync: p.keyframe }).unwrap();
    }
    std::fs::write(path, mux.finish().unwrap().into_inner()).unwrap();
}

/// Mean of a frame's linear RGB.
pub fn mean(img: &lcc_raster::Rgb32f) -> f64 {
    let n = img.data.len().max(1) as f64;
    img.data.iter().map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])).sum::<f64>() / (3.0 * n)
}
