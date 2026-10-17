//! Native audio decode: the first decodable audio track of the clip's container (MP4/MOV,
//! Matroska/WebM, MPEG-TS/PS, MXF), decoded with FilmCraft's AAC-LC, Opus and AC-3 / E-AC-3
//! decoders, `puremp3` for MP3 (MIT OR CC0), and in-crate integer / float PCM unpacking.
//!
//! Times count from the clip's first shown video frame, like [`crate::FrameReader`]'s, so a block
//! at `time` plays with the frame at `time`.

use crate::VideoError;
use crate::audio::{AudioBlock, AudioInfo, MAX_CHANNELS};
use crate::demux::{self, Clip, Container};
use crate::native::guarded;

/// Packets decoded (and dropped) before the first needed one: AAC and AC-3 overlap their MDCT
/// windows with the previous frame; Opus (CELT) needs a few for its pre-roll.
const PREROLL: usize = 3;
/// Timestamps further than 1/RESYNC_DIV s from the running sample count move it (gaps, drops).
const RESYNC_DIV: u32 = 50;
/// Consecutive undecodable packets before a read gives up.
const MAX_BAD_PACKETS: usize = 32;
/// Samples per channel one packet may decode to (E-AC-3 has 1536, Opus at most 5760).
const MAX_PACKET_SAMPLES: usize = 8192;
/// Sample frames one PCM packet may unpack to (an MP4 chunk of PCM is often a second or more).
const MAX_PCM_FRAMES: usize = 1 << 20;
/// Sample frames per synthetic packet of MXF sound (MXF stores sound as sample ranges).
const MXF_BLOCK: u64 = 4096;

enum Track {
    Mp4 {
        track: usize,
    },
    Mkv {
        track: usize,
    },
    Ts {
        stream: usize,
    },
    /// `first`: stored sample frame shown with the first video frame; `frames`: frames from it.
    Mxf {
        track: usize,
        first: u64,
        frames: u64,
    },
}

/// Uncompressed PCM layout of one packet.
#[derive(Clone, Copy, Debug)]
struct Pcm {
    /// Bytes per sample (the container width: 20-bit samples sit left-justified in 3 bytes).
    bytes: usize,
    float: bool,
    big_endian: bool,
    signed: bool,
    channels: usize,
    /// Channels stored per frame (Blu-ray LPCM pads odd counts with an empty one).
    coded_channels: usize,
    /// Header bytes before the samples of each packet (4 for Blu-ray LPCM).
    skip: usize,
}

impl Pcm {
    fn new(bits: u32, float: bool, big_endian: bool, signed: bool, channels: usize) -> Option<Pcm> {
        let bytes = bits.div_ceil(8) as usize;
        let ok = if float { bytes == 4 || bytes == 8 } else { (1..=4).contains(&bytes) };
        (ok && (1..=MAX_CHANNELS).contains(&channels)).then_some(Pcm {
            bytes,
            float,
            big_endian,
            signed,
            channels,
            coded_channels: channels,
            skip: 0,
        })
    }

    fn sample(&self, b: &[u8]) -> f32 {
        let mut v = 0u64;
        for i in 0..b.len() {
            let byte = if self.big_endian { b.get(i) } else { b.get(b.len() - 1 - i) };
            v = (v << 8) | u64::from(byte.copied().unwrap_or(0));
        }
        if self.float {
            return if b.len() == 4 { f32::from_bits(v as u32) } else { f64::from_bits(v) as f32 };
        }
        let w = 8 * b.len() as u32;
        let half = (1u64 << (w - 1)) as f64;
        let x = if self.signed { ((v << (64 - w)) as i64 >> (64 - w)) as f64 } else { v as f64 - half };
        (x / half) as f32
    }

    /// Planar samples of one packet (a trailing partial frame is dropped).
    fn decode(&self, data: &[u8]) -> Vec<Vec<f32>> {
        let body = data.get(self.skip..).unwrap_or(&[]);
        let frame = self.bytes.saturating_mul(self.coded_channels).max(1);
        let n = (body.len() / frame).min(MAX_PCM_FRAMES);
        let mut out = vec![Vec::with_capacity(n); self.channels];
        for f in body.chunks_exact(frame).take(n) {
            for (c, o) in out.iter_mut().enumerate() {
                let s = f.get(c * self.bytes..(c + 1) * self.bytes).unwrap_or(&[]);
                let x = self.sample(s);
                // NaN / inf in float PCM would poison mixing and resampling
                o.push(if x.is_finite() { x.clamp(-1.0, 1.0) } else { 0.0 });
            }
        }
        out
    }
}

/// Blu-ray / AVCHD LPCM from the 4-byte header of a PES payload: (layout, sample rate).
fn bluray_lpcm(h: &[u8]) -> Option<(Pcm, u32)> {
    let (&b2, &b3) = (h.get(2)?, h.get(3)?);
    let channels = match b2 >> 4 {
        1 => 1,
        3 => 2,
        4 | 5 => 3,
        6 | 7 => 4,
        8 => 5,
        9 | 10 => 6,
        11 => 8,
        _ => return None,
    };
    let rate = match b2 & 0x0F {
        1 => 48_000,
        4 => 96_000,
        5 => 192_000,
        _ => return None,
    };
    let bits = match b3 >> 6 {
        1 => 16,
        2 | 3 => 24,
        _ => return None,
    };
    let mut p = Pcm::new(bits, false, true, true, channels)?;
    p.coded_channels = channels + channels % 2;
    p.skip = 4;
    Some((p, rate))
}

/// MP3 (MPEG-1/2/2.5 Layer III); each packet is one frame, fed through the decoder's reader so
/// the bit reservoir and overlap carry across packets.
struct Mp3(puremp3::Mp3Decoder<std::io::Cursor<Vec<u8>>>);

impl Mp3 {
    fn new() -> Mp3 {
        Mp3(puremp3::Mp3Decoder::new(std::io::Cursor::new(Vec::new())))
    }

    /// Planar samples and the frame's (sample rate, channels).
    fn decode(&mut self, data: &[u8]) -> Result<(Vec<Vec<f32>>, u32, usize), String> {
        *self.0.get_mut() = std::io::Cursor::new(data.to_vec());
        let f = self.0.next_frame().map_err(|e| format!("MP3: {e}"))?;
        if f.header.layer != puremp3::MpegLayer::Layer3 {
            return Err("MPEG audio layer I / II is not decoded".into());
        }
        let ch = if f.header.channels == puremp3::Channels::Mono { 1 } else { 2 };
        let n = f.num_samples.min(1152);
        let planar = f.samples.iter().take(ch).map(|s| s.get(..n).unwrap_or(&[]).to_vec()).collect();
        Ok((planar, f.header.sample_rate.hz(), ch))
    }
}

enum Dec {
    /// ADTS framing (MPEG-TS) is stripped before decoding.
    Aac {
        dec: filmcraft_aac::Decoder,
        adts: bool,
    },
    Opus(filmcraft_opus::Decoder),
    Ac3(Box<filmcraft_ac3::Decoder>),
    Pcm(Pcm),
    Mp3(Box<Mp3>),
}

impl Dec {
    fn reset(&mut self) {
        match self {
            Dec::Aac { dec, .. } => dec.reset(),
            Dec::Opus(d) => d.reset(),
            Dec::Ac3(d) => d.reset(),
            Dec::Pcm(_) => {}
            Dec::Mp3(d) => **d = Mp3::new(),
        }
    }

    /// Planar samples of one packet; `Ok(None)` for a packet that yields nothing (a dependent
    /// E-AC-3 substream).
    fn decode(&mut self, data: &[u8]) -> Result<Option<Vec<Vec<f32>>>, String> {
        match self {
            Dec::Aac { dec, adts } => {
                let au = if *adts {
                    let h = filmcraft_aac::AdtsHeader::parse(data).map_err(|e| e.to_string())?;
                    let end = h.frame_length.min(data.len());
                    data.get(h.header_length..end).ok_or("short ADTS frame")?
                } else {
                    data
                };
                dec.decode(au).map(Some).map_err(|e| e.to_string())
            }
            Dec::Opus(d) => d.decode(Some(data)).map(Some).map_err(|e| e.to_string()),
            Dec::Ac3(d) => {
                let out = d.decode(data).map_err(|e| e.to_string())?;
                Ok((!out.channels.is_empty()).then_some(out.channels))
            }
            Dec::Pcm(p) => Ok(Some(p.decode(data))),
            Dec::Mp3(d) => d.decode(data).map(|(p, ..)| Some(p)),
        }
    }

    /// Samples per channel one packet may decode to.
    fn max_samples(&self) -> usize {
        if matches!(self, Dec::Pcm(_)) { MAX_PCM_FRAMES } else { MAX_PACKET_SAMPLES }
    }
}

pub struct Reader {
    clip: Clip,
    track: Track,
    dec: Dec,
    info: AudioInfo,
    /// Start time of each packet in seconds from the video origin; None where the container has
    /// none (TS audio frames between PES timestamps).
    times: Vec<Option<f64>>,
    /// Next packet to decode.
    next: usize,
    /// Presentation time (samples at the source rate from the origin) of the next decoded sample.
    cursor: i64,
    /// Samples before this one are dropped (the target of the last seek).
    from: i64,
}

fn unsupported(why: impl Into<String>) -> VideoError {
    VideoError::Unsupported(why.into())
}

impl Reader {
    pub fn open(path: &std::path::Path) -> Result<Self, VideoError> {
        let clip = guarded("demux", || demux::open(path))?;
        guarded("audio", || Self::from_clip(clip))
    }

    fn from_clip(clip: Clip) -> Result<Self, VideoError> {
        let (track, dec, codec, times, rate, channels) = match &clip.container {
            Container::Mp4 { file, track: vt } => {
                use filmcraft_isobmff::{CodecConfig, TrackKind};
                let origin = file.tracks.get(*vt).and_then(|t| {
                    let tb = f64::from(t.timescale.max(1));
                    (0..t.samples.len()).filter_map(|i| t.presentation_pts(i)).min().map(|p| p as f64 / tb)
                });
                let mut found = None;
                for (i, t) in file.tracks.iter().enumerate() {
                    if t.kind != TrackKind::Audio || t.samples.is_empty() || t.timescale == 0 {
                        continue;
                    }
                    let a = t.audio().cloned().unwrap_or_default();
                    let dec = match t.codec() {
                        Some(CodecConfig::Aac(c)) => filmcraft_aac::Decoder::new(&c.asc).ok().map(|dec| (Dec::Aac { dec, adts: false }, "aac")),
                        Some(CodecConfig::Opus(c)) => opus_from_mp4(c, a.channels).map(|d| (Dec::Opus(d), "opus")),
                        Some(CodecConfig::Ac3 { .. }) => Some((Dec::Ac3(Box::default()), "ac3")),
                        Some(CodecConfig::Eac3 { .. }) => Some((Dec::Ac3(Box::default()), "eac3")),
                        Some(CodecConfig::Mp3) => Some((Dec::Mp3(Box::new(Mp3::new())), "mp3")),
                        Some(CodecConfig::Pcm(p)) => {
                            let ch = if p.channels > 0 { p.channels } else { a.channels } as usize;
                            Pcm::new(u32::from(p.bits), p.float, p.big_endian, p.signed, ch).map(|p| (Dec::Pcm(p), "pcm"))
                        }
                        _ => None,
                    };
                    if let Some((dec, name)) = dec {
                        let tb = f64::from(t.timescale);
                        let o = origin.unwrap_or(0.0);
                        let times = (0..t.samples.len()).map(|k| t.presentation_pts(k).map(|p| p as f64 / tb - o)).collect();
                        found = Some((Track::Mp4 { track: i }, dec, name, times, a.sample_rate, a.channels as usize));
                        break;
                    }
                }
                found.ok_or_else(|| unsupported("no decodable audio track (AAC, Opus, AC-3, MP3, PCM)"))?
            }
            Container::Mkv { file, track: vt } => {
                use filmcraft_matroska::{Codec, TrackKind};
                let secs = |t: &filmcraft_matroska::Track, p: i64| {
                    let (n, d) = t.timebase;
                    if d == 0 { 0.0 } else { p as f64 * n as f64 / d as f64 }
                };
                let origin = file.tracks.get(*vt).and_then(|t| t.samples.iter().map(|s| s.pts).min().map(|p| secs(t, p))).unwrap_or(0.0);
                let mut found = None;
                for (i, t) in file.tracks.iter().enumerate() {
                    if t.kind != TrackKind::Audio || t.samples.is_empty() {
                        continue;
                    }
                    let a = t.audio.clone().unwrap_or_default();
                    let dec = match &t.codec {
                        Codec::Aac { asc } => filmcraft_aac::Decoder::new(asc).ok().map(|dec| (Dec::Aac { dec, adts: false }, "aac")),
                        Codec::Opus { head } => filmcraft_opus::Decoder::new(head).ok().map(|mut d| {
                            // the codec delay is applied through the timestamps below
                            d.set_trim_pre_skip(false);
                            (Dec::Opus(d), "opus")
                        }),
                        Codec::Ac3 => Some((Dec::Ac3(Box::default()), "ac3")),
                        Codec::Mp3 => Some((Dec::Mp3(Box::new(Mp3::new())), "mp3")),
                        // Matroska: 8-bit A_PCM/INT/LIT is unsigned (as in WAVE)
                        Codec::Pcm { float, big_endian, bits } => {
                            let bits = if *bits > 0 { u32::from(*bits) } else { u32::try_from(a.bit_depth.unwrap_or(16)).unwrap_or(0) };
                            Pcm::new(bits, *float, *big_endian, *float || *big_endian || bits > 8, a.channels as usize).map(|p| (Dec::Pcm(p), "pcm"))
                        }
                        _ => None,
                    };
                    if let Some((dec, name)) = dec {
                        let delay = t.codec_delay_ns as f64 / 1e9;
                        let times = t.samples.iter().map(|s| Some(secs(t, s.pts) - delay - origin)).collect();
                        let rate = a.output_sampling_frequency.unwrap_or(a.sampling_frequency);
                        found = Some((Track::Mkv { track: i }, dec, name, times, rate, a.channels as usize));
                        break;
                    }
                }
                found.ok_or_else(|| unsupported("no decodable audio track (AAC, Opus, AC-3, MP3, PCM)"))?
            }
            Container::Ts { file, stream: vs, units } => {
                use filmcraft_mpegts::{Codec as T, Kind};
                let mut lpcm_rate = 0u32;
                let origin = file
                    .streams
                    .get(*vs)
                    .and_then(|s| units.first().and_then(|&u| s.units.get(u)))
                    .and_then(|u| u.pts)
                    .map_or(0.0, |p| p as f64 / 90_000.0);
                let stream = file
                    .find(Kind::Audio, |c| matches!(c, T::AacAdts | T::Ac3 | T::Eac3 | T::MpegAudio | T::LpcmBluray | T::LpcmDvd))
                    .ok_or_else(|| unsupported("no decodable audio stream (ADTS AAC, AC-3, MP3, LPCM)"))?;
                let st = file.streams.get(stream).ok_or_else(|| unsupported("no audio stream"))?;
                let (dec, name) = match st.codec {
                    T::AacAdts => {
                        let first = file.read_unit(&clip.src, stream, 0).map_err(|e| VideoError::Invalid(e.to_string()))?;
                        let h = filmcraft_aac::AdtsHeader::parse(&first).map_err(|e| VideoError::Invalid(format!("ADTS: {e}")))?;
                        let dec = filmcraft_aac::Decoder::from_adts(&h).map_err(|e| VideoError::Unsupported(format!("AAC: {e}")))?;
                        (Dec::Aac { dec, adts: true }, "aac")
                    }
                    T::Eac3 => (Dec::Ac3(Box::default()), "eac3"),
                    T::MpegAudio => (Dec::Mp3(Box::new(Mp3::new())), "mp3"),
                    T::LpcmBluray => {
                        let first = file.read_unit(&clip.src, stream, 0).map_err(|e| VideoError::Invalid(e.to_string()))?;
                        let (p, rate) = bluray_lpcm(&first).ok_or_else(|| unsupported("Blu-ray LPCM format"))?;
                        lpcm_rate = rate;
                        (Dec::Pcm(p), "pcm")
                    }
                    T::LpcmDvd => {
                        // 20- and 24-bit DVD LPCM interleave sample groups; only 16-bit is plain
                        let f = st.lpcm.filter(|f| f.bits == 16).ok_or_else(|| unsupported("DVD LPCM other than 16-bit"))?;
                        lpcm_rate = f.sample_rate;
                        let p = Pcm::new(16, false, true, true, f.channels as usize).ok_or_else(|| unsupported("DVD LPCM channels"))?;
                        (Dec::Pcm(p), "pcm")
                    }
                    _ => (Dec::Ac3(Box::default()), "ac3"),
                };
                let times = st.units.iter().map(|u| u.pts.map(|p| p as f64 / 90_000.0 - origin)).collect();
                // rate and channels come from the first decoded packet (LPCM: its header)
                (Track::Ts { stream }, dec, name, times, f64::from(lpcm_rate), 0)
            }
            Container::Mxf { file, .. } => {
                use filmcraft_mxf::{SoundFormat, TrackKind};
                let a = file
                    .tracks
                    .iter()
                    .position(|t| t.kind == TrackKind::Sound && !t.chunks.is_empty() && t.sound_format != SoundFormat::Other && t.sound.is_some())
                    .ok_or_else(|| unsupported("no decodable audio track (PCM, AES3)"))?;
                let t = file.tracks.get(a).ok_or_else(|| unsupported("no audio track"))?;
                let si = t.sound.clone().unwrap_or_default();
                let sr = si.sample_rate;
                let rate = if sr.num > 0 && sr.den > 0 { f64::from(sr.num) / f64::from(sr.den) } else { 0.0 };
                // the zero point of the sound track lines up with the video track's
                let (n, d) = t.samples_per_edit_unit();
                let first = u64::try_from(t.first_edit_unit().saturating_mul(n) / d.max(1)).unwrap_or(0);
                let frames = t.stored_sample_frames().saturating_sub(first);
                let blocks = frames.div_ceil(MXF_BLOCK).min(1 << 24);
                let times = (0..blocks).map(|k| Some((k * MXF_BLOCK) as f64 / rate.max(1.0))).collect();
                // blocks arrive as planar f32 re-packed little-endian
                let p = Pcm::new(32, true, false, true, si.channels as usize)
                    .ok_or_else(|| unsupported(format!("{} MXF audio channels", si.channels)))?;
                (Track::Mxf { track: a, first, frames }, Dec::Pcm(p), "pcm", times, rate, si.channels as usize)
            }
            Container::Es { .. } => return Err(unsupported("audio in this container is not decoded")),
        };
        let mut r =
            Reader { clip, track, dec, info: AudioInfo { codec: codec.into(), sample_rate: 0, channels: 0 }, times, next: 0, cursor: 0, from: 0 };
        r.probe_format(rate, channels)?;
        r.seek(0.0);
        Ok(r)
    }

    /// Fix the source rate and channel count: the decoder's when it knows them up front, else the
    /// first packet that decodes (TS, AC-3, rates the container got wrong).
    fn probe_format(&mut self, container_rate: f64, container_channels: usize) -> Result<(), VideoError> {
        let (rate, channels) = match &self.dec {
            Dec::Aac { dec, .. } => (dec.sample_rate(), dec.channels()),
            Dec::Opus(d) => (d.sample_rate(), d.channels()),
            Dec::Pcm(p) => (container_rate.round() as u32, p.channels),
            Dec::Mp3(_) => {
                let mut found = None;
                for i in 0..self.times.len().min(MAX_BAD_PACKETS) {
                    let Ok(data) = self.read(i) else { continue };
                    if let Ok((_, rate, ch)) = guarded("audio", || Mp3::new().decode(&data).map_err(VideoError::Decode)) {
                        found = Some((rate, ch));
                        break;
                    }
                }
                found.unwrap_or((container_rate as u32, container_channels))
            }
            Dec::Ac3(_) => {
                let mut found = None;
                for i in 0..self.times.len().min(MAX_BAD_PACKETS) {
                    let Ok(data) = self.read(i) else { continue };
                    if let Ok(h) = filmcraft_ac3::parse_header(&data) {
                        found = Some((h.sample_rate, h.channels()));
                        break;
                    }
                }
                found.unwrap_or((container_rate as u32, container_channels))
            }
        };
        if !(8_000..=192_000).contains(&rate) {
            return Err(unsupported(format!("audio sample rate {rate} Hz")));
        }
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(unsupported(format!("{channels} audio channels")));
        }
        self.info.sample_rate = rate;
        self.info.channels = channels;
        Ok(())
    }

    pub fn info(&self) -> &AudioInfo {
        &self.info
    }

    fn read(&self, i: usize) -> Result<Vec<u8>, VideoError> {
        let invalid = |e: String| VideoError::Invalid(e);
        match (&self.track, &self.clip.container) {
            (Track::Mp4 { track }, Container::Mp4 { file, .. }) => file.read_sample(&self.clip.src, *track, i).map_err(demux::demux_error),
            (Track::Mkv { track }, Container::Mkv { file, .. }) => file.read_sample(&self.clip.src, *track, i).map_err(|e| invalid(e.to_string())),
            (Track::Ts { stream }, Container::Ts { file, .. }) => file.read_unit(&self.clip.src, *stream, i).map_err(|e| invalid(e.to_string())),
            (Track::Mxf { track, first, frames }, Container::Mxf { file, .. }) => {
                let at = (i as u64).saturating_mul(MXF_BLOCK);
                let n = frames.saturating_sub(at).min(MXF_BLOCK) as usize;
                let planar = file.read_pcm(&self.clip.src, *track, first.saturating_add(at), n).map_err(|e| invalid(e.to_string()))?;
                let mut bytes = Vec::with_capacity(n * planar.len() * 4);
                for k in 0..n {
                    for c in &planar {
                        bytes.extend_from_slice(&c.get(k).copied().unwrap_or(0.0).to_le_bytes());
                    }
                }
                Ok(bytes)
            }
            _ => Err(invalid("audio track mismatch".into())),
        }
    }

    fn to_samples(&self, t: f64) -> i64 {
        let s = (t * f64::from(self.info.sample_rate)).round();
        if s.is_finite() { s.clamp(-(i64::MAX as f64) / 4.0, i64::MAX as f64 / 4.0) as i64 } else { 0 }
    }

    /// Position before `t` seconds: the next block starts at (or just before) `t`.
    pub fn seek(&mut self, t: f64) {
        let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
        // last packet starting at or before t (times are increasing; unknown ones are skipped)
        let k = self.times.partition_point(|x| x.is_none_or(|s| s <= t));
        let target = k.saturating_sub(1);
        let start = target.saturating_sub(PREROLL);
        // the nearest known time at or before the start packet anchors the cursor
        self.next = (0..=start).rev().find(|&i| self.times.get(i).copied().flatten().is_some()).unwrap_or(0);
        self.cursor = self.times.get(self.next).copied().flatten().map_or(0, |s| self.to_samples(s));
        self.from = self.to_samples(t);
        self.dec.reset();
    }

    /// The next decoded block at or after the seek target; None at the end of the track.
    pub fn next_block(&mut self) -> Result<Option<AudioBlock>, VideoError> {
        let mut bad = 0usize;
        loop {
            let Some(&known) = self.times.get(self.next) else { return Ok(None) };
            let i = self.next;
            self.next = i.saturating_add(1);
            if let Some(s) = known {
                // follow container timestamps only past a jitter tolerance: Matroska rounds to
                // its timestamp scale (usually 1 ms), so packets keep the running sample count
                let at = self.to_samples(s);
                if at.abs_diff(self.cursor) > u64::from(self.info.sample_rate / RESYNC_DIV) {
                    self.cursor = at;
                }
            }
            let decoded = match self.read(i) {
                Ok(data) => {
                    let dec = &mut self.dec;
                    guarded("audio decode", || dec.decode(&data).map_err(VideoError::Decode))
                }
                Err(e) => Err(e),
            };
            let planar = match decoded {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(e) => {
                    bad = bad.saturating_add(1);
                    if bad >= MAX_BAD_PACKETS {
                        return Err(e);
                    }
                    continue;
                }
            };
            bad = 0;
            let ch = self.info.channels;
            let n = planar.iter().map(Vec::len).min().unwrap_or(0).min(self.dec.max_samples());
            let start = self.cursor;
            self.cursor = self.cursor.saturating_add(n as i64);
            if n == 0 || self.cursor <= self.from {
                continue;
            }
            let skip = usize::try_from(self.from.saturating_sub(start)).unwrap_or(0).min(n);
            let mut samples = Vec::with_capacity((n - skip) * ch);
            for k in skip..n {
                for c in 0..ch {
                    // a packet with fewer channels than the stream (AC-3 mode change): silence
                    samples.push(planar.get(c).and_then(|p| p.get(k)).copied().unwrap_or(0.0));
                }
            }
            let first = start.saturating_add(skip as i64);
            self.from = self.cursor;
            return Ok(Some(AudioBlock { time: first as f64 / f64::from(self.info.sample_rate), samples }));
        }
    }
}

/// An Opus decoder from an MP4 `dOps` box (pre-skip comes through the edit list).
fn opus_from_mp4(c: &filmcraft_isobmff::OpusConfig, entry_channels: u32) -> Option<filmcraft_opus::Decoder> {
    let channels = if c.output_channels > 0 { usize::from(c.output_channels) } else { entry_channels as usize };
    let mut head = filmcraft_opus::OpusHead::simple(channels.clamp(1, 2)).ok()?;
    if c.channel_mapping_family != 0 {
        let (&streams, rest) = c.channel_mapping.split_first()?;
        let (&coupled, mapping) = rest.split_first()?;
        head.channels = channels;
        head.mapping_family = c.channel_mapping_family;
        head.streams = usize::from(streams);
        head.coupled_streams = usize::from(coupled);
        head.mapping = mapping.to_vec();
    }
    head.pre_skip = c.pre_skip;
    head.output_gain = c.output_gain;
    head.input_sample_rate = c.input_sample_rate;
    let mut d = filmcraft_opus::Decoder::from_head(head, 48_000).ok()?;
    d.set_trim_pre_skip(false);
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluray_lpcm_header_and_padding() {
        // 3 channels (3/0), 48 kHz, 16-bit: stored as 4 channels
        let (p, rate) = bluray_lpcm(&[0, 0, 0x41, 0x40]).unwrap();
        assert_eq!((rate, p.channels, p.coded_channels, p.bytes, p.skip), (48_000, 3, 4, 2, 4));
        let mut data = vec![0, 0, 0x41, 0x40];
        data.extend([0x40, 0x00, 0xC0, 0x00, 0x00, 0x01, 0x7F, 0xFF, 0x12]); // one frame + a stray byte
        let out = p.decode(&data);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], [0.5]);
        assert_eq!(out[1], [-0.5]);
        assert!(bluray_lpcm(&[0, 0, 0x02, 0x40]).is_none());
        assert!(bluray_lpcm(&[0, 0]).is_none());
    }

    #[test]
    fn unsigned_8_bit_and_bad_layouts() {
        let p = Pcm::new(8, false, false, false, 1).unwrap();
        assert_eq!(p.decode(&[128, 0, 255]), [vec![0.0, -1.0, 127.0 / 128.0]]);
        assert!(Pcm::new(0, false, false, true, 2).is_none());
        assert!(Pcm::new(16, true, false, true, 2).is_none());
        assert!(Pcm::new(16, false, false, true, 0).is_none());
        let f = Pcm::new(32, true, false, true, 1).unwrap();
        assert_eq!(f.decode(&f32::NAN.to_le_bytes()), [vec![0.0]]);
    }

    #[test]
    fn garbage_mp3_is_an_error() {
        assert!(Mp3::new().decode(&[0xFF, 0xFB, 0x94]).is_err());
        assert!(Mp3::new().decode(&[]).is_err());
    }
}
