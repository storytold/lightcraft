//! Video: AV1 encoding of RGBA frames (`rav1e`, native targets with the `avif` feature) and two
//! small muxers of our own, written from the public specifications only:
//!
//! - **MP4** (ISO/IEC 14496-12 ISO base media file format + the AOM "AV1 Codec ISO Media File
//!   Format Binding"): `ftyp`, one `mdat` (64-bit size, patched at the end) holding the samples,
//!   then `moov` with one video track whose sample entry is `av01` + `av1C` + `colr` (`nclx`).
//! - **WebM** (Matroska / EBML as restricted by WebM, codec mapping `V_AV1`): EBML header,
//!   Segment (size patched at the end) with SeekHead, Info, Tracks, Clusters of SimpleBlocks (the
//!   last frame a BlockGroup with its duration) and Cues.
//!
//! Frames may last several frame periods: a picture held on screen is one sample with a long
//! duration (variable frame rate), so a still slide costs one encoded frame however long it
//! stays up. Durations are counted in frame periods of the nominal rate.
//!
//! Samples are AV1 temporal units without temporal delimiter OBUs (both bindings ask for that);
//! the configuration record carries the sequence header OBU. No audio track.

use std::io::{Seek, SeekFrom, Write};

use crate::{Error, Result};

/// The container a video is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    WebM,
}

impl Container {
    pub fn id(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::WebM => "webm",
        }
    }

    pub fn parse(s: &str) -> Option<Container> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mp4" => Some(Container::Mp4),
            "webm" => Some(Container::WebM),
            _ => None,
        }
    }
}

/// One encoded picture: an AV1 temporal unit (no temporal delimiter), whether it is a key frame
/// (a sync sample), and how many frame periods it stays on screen (≥ 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub data: Vec<u8>,
    pub key: bool,
    pub duration: u64,
}

/// The longest video the muxers write, in frame periods (keeps every timestamp in range).
pub const MAX_PERIODS: u64 = 60 * 60 * 24 * 120;

fn io(e: std::io::Error) -> Error {
    Error::Encode(format!("video: {e}"))
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Encode(format!("video: {}", msg.into()))
}

// ---------------------------------------------------------------- AV1 OBUs

/// OBU types (AV1 specification §6.2.2).
pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_PADDING: u8 = 15;

/// Read a LEB128 value at `pos` (at most 8 bytes); returns (value, bytes read).
fn leb128(data: &[u8], pos: usize) -> Result<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..8 {
        let b = *data.get(pos + i).ok_or_else(|| bad("truncated OBU size"))?;
        v |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Err(bad("OBU size longer than 8 bytes"))
}

/// The OBUs of a temporal unit: (type, the whole OBU's byte range). Every OBU but the last must
/// carry its size (as the low-overhead bitstream format requires).
pub fn obus(data: &[u8]) -> Result<Vec<(u8, std::ops::Range<usize>)>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(&header) = data.get(pos) {
        if header & 0x80 != 0 {
            return Err(bad("OBU forbidden bit set"));
        }
        let kind = (header >> 3) & 0x0f;
        let ext = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        let mut p = pos + 1 + usize::from(ext);
        let end = if has_size {
            let (size, n) = leb128(data, p)?;
            p += n;
            let size = usize::try_from(size).map_err(|_| bad("OBU too large"))?;
            p.checked_add(size).filter(|e| *e <= data.len()).ok_or_else(|| bad("OBU runs past the end of the packet"))?
        } else {
            data.len()
        };
        if p > end {
            return Err(bad("truncated OBU header"));
        }
        out.push((kind, pos..end));
        pos = end;
    }
    Ok(out)
}

/// A temporal unit as a sample: temporal delimiters and padding dropped. Also returns its
/// sequence header OBU, if it has one.
pub fn to_sample_data(tu: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
    let mut data = Vec::with_capacity(tu.len());
    let mut seq = None;
    for (kind, r) in obus(tu)? {
        let Some(bytes) = tu.get(r) else { continue };
        match kind {
            OBU_TEMPORAL_DELIMITER | OBU_PADDING => {}
            _ => {
                if kind == OBU_SEQUENCE_HEADER && seq.is_none() {
                    seq = Some(bytes.to_vec());
                }
                data.extend_from_slice(bytes);
            }
        }
    }
    Ok((data, seq))
}

/// The `av1C` configuration record (AV1-ISOBMFF §2.3) for 8-bit 4:2:0 main profile, followed by
/// `seq_header` (the sequence header OBU, may be empty). Matroska's `CodecPrivate` is the same.
pub fn av1_config(seq_header: &[u8]) -> Vec<u8> {
    // marker 1, version 1 | profile 0, level 31 (unconstrained) | tier 0, 8-bit, not mono,
    // subsampling x and y, chroma position 0 | no initial presentation delay
    let mut v = vec![0x81, 31, 0b0000_1100, 0];
    v.extend_from_slice(seq_header);
    v
}

// ---------------------------------------------------------------- MP4

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Result<Vec<u8>> {
    let size = u32::try_from(body.len().checked_add(8).ok_or_else(|| bad("box too large"))?).map_err(|_| bad("box too large"))?;
    let mut v = Vec::with_capacity(body.len() + 8);
    v.extend_from_slice(&size.to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    Ok(v)
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Result<Vec<u8>> {
    let mut b = Vec::with_capacity(body.len() + 4);
    b.push(version);
    b.extend_from_slice(&flags.to_be_bytes()[1..]);
    b.extend_from_slice(body);
    mp4_box(kind, &b)
}

/// The unity transformation matrix of `mvhd` / `tkhd`.
fn matrix() -> Vec<u8> {
    let mut m = Vec::with_capacity(36);
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        m.extend_from_slice(&v.to_be_bytes());
    }
    m
}

/// An MP4 writer: [`Mp4Writer::add`] streams each sample into `mdat`; [`Mp4Writer::finish`]
/// patches its size and appends `moov`.
pub struct Mp4Writer<W: Write + Seek> {
    w: W,
    width: u16,
    height: u16,
    fps: u32,
    /// Where `mdat`'s 64-bit size is, and where its data starts.
    mdat_size_at: u64,
    data_start: u64,
    written: u64,
    sizes: Vec<u32>,
    durations: Vec<u64>,
    keys: Vec<u32>,
    config: Option<Vec<u8>>,
}

/// Track ticks per frame period (ffmpeg's habit: 512 per frame).
const TICKS: u64 = 512;

impl<W: Write + Seek> Mp4Writer<W> {
    pub fn new(mut w: W, width: u32, height: u32, fps: u32) -> Result<Self> {
        let (width, height) = (u16::try_from(width).map_err(|_| bad("too wide"))?, u16::try_from(height).map_err(|_| bad("too tall"))?);
        if width == 0 || height == 0 || !(1..=240).contains(&fps) {
            return Err(bad("bad size or frame rate"));
        }
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        ftyp.extend_from_slice(&0x200u32.to_be_bytes());
        for brand in [b"isom", b"iso6", b"av01", b"mp41"] {
            ftyp.extend_from_slice(brand);
        }
        w.write_all(&mp4_box(b"ftyp", &ftyp)?).map_err(io)?;
        let start = w.stream_position().map_err(io)?;
        // size 1 = a 64-bit size follows (patched in finish)
        w.write_all(&1u32.to_be_bytes()).map_err(io)?;
        w.write_all(b"mdat").map_err(io)?;
        w.write_all(&0u64.to_be_bytes()).map_err(io)?;
        Ok(Mp4Writer {
            w,
            width,
            height,
            fps,
            mdat_size_at: start + 8,
            data_start: start + 16,
            written: 0,
            sizes: Vec::new(),
            durations: Vec::new(),
            keys: Vec::new(),
            config: None,
        })
    }

    /// The `av1C` record ([`av1_config`]); needed before [`Self::finish`].
    pub fn set_config(&mut self, config: Vec<u8>) {
        self.config = Some(config);
    }

    pub fn add(&mut self, s: &Sample) -> Result<()> {
        let size = u32::try_from(s.data.len()).map_err(|_| bad("sample too large"))?;
        if s.duration == 0 {
            return Err(bad("a sample lasts at least one frame"));
        }
        if self.sizes.is_empty() && !s.key {
            return Err(bad("the first sample must be a key frame"));
        }
        self.w.write_all(&s.data).map_err(io)?;
        self.written = self.written.saturating_add(u64::from(size));
        self.sizes.push(size);
        self.durations.push(s.duration);
        if s.key {
            let n = u32::try_from(self.sizes.len()).map_err(|_| bad("too many samples"))?;
            self.keys.push(n);
        }
        Ok(())
    }

    /// Frame periods written so far.
    pub fn periods(&self) -> u64 {
        self.durations.iter().sum()
    }

    pub fn finish(mut self) -> Result<W> {
        if self.sizes.is_empty() {
            return Err(bad("no frames"));
        }
        let config = self.config.take().ok_or_else(|| bad("no AV1 configuration"))?;
        let periods = self.periods();
        if periods > MAX_PERIODS {
            return Err(bad("video too long"));
        }
        let end = self.w.stream_position().map_err(io)?;
        self.w.seek(SeekFrom::Start(self.mdat_size_at)).map_err(io)?;
        self.w.write_all(&(end - (self.mdat_size_at - 8)).to_be_bytes()).map_err(io)?;
        self.w.seek(SeekFrom::Start(end)).map_err(io)?;
        let moov = self.moov(&config, periods)?;
        self.w.write_all(&moov).map_err(io)?;
        self.w.flush().map_err(io)?;
        Ok(self.w)
    }

    fn moov(&self, config: &[u8], periods: u64) -> Result<Vec<u8>> {
        let n = u32::try_from(self.sizes.len()).map_err(|_| bad("too many samples"))?;
        let timescale = u32::try_from(u64::from(self.fps) * TICKS).map_err(|_| bad("bad frame rate"))?;
        let ticks = periods.checked_mul(TICKS).ok_or_else(|| bad("video too long"))?;
        // the movie's own timescale: milliseconds
        let movie_ms = periods.saturating_mul(1000) / u64::from(self.fps);
        let u32_of = |v: u64| u32::try_from(v).map_err(|_| bad("video too long"));
        let (track_ticks, movie_ms) = (u32_of(ticks)?, u32_of(movie_ms)?);

        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0; 8]); // creation, modification
        mvhd.extend_from_slice(&1000u32.to_be_bytes());
        mvhd.extend_from_slice(&movie_ms.to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        mvhd.extend_from_slice(&[0; 10]);
        mvhd.extend_from_slice(&matrix());
        mvhd.extend_from_slice(&[0; 24]); // pre_defined
        mvhd.extend_from_slice(&2u32.to_be_bytes()); // next track id

        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&[0; 8]);
        tkhd.extend_from_slice(&1u32.to_be_bytes()); // track id
        tkhd.extend_from_slice(&[0; 4]);
        tkhd.extend_from_slice(&movie_ms.to_be_bytes());
        tkhd.extend_from_slice(&[0; 8]);
        tkhd.extend_from_slice(&[0; 2]); // layer
        tkhd.extend_from_slice(&[0; 2]); // alternate group
        tkhd.extend_from_slice(&[0; 2]); // volume (video: 0)
        tkhd.extend_from_slice(&[0; 2]);
        tkhd.extend_from_slice(&matrix());
        tkhd.extend_from_slice(&(u32::from(self.width) << 16).to_be_bytes());
        tkhd.extend_from_slice(&(u32::from(self.height) << 16).to_be_bytes());

        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&[0; 8]);
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&track_ticks.to_be_bytes());
        mdhd.extend_from_slice(&0x55c4u16.to_be_bytes()); // language "und"
        mdhd.extend_from_slice(&[0; 2]);

        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&[0; 4]);
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0; 12]);
        hdlr.extend_from_slice(b"LightCraft video\0");

        let mut vmhd = Vec::new();
        vmhd.extend_from_slice(&[0; 8]); // graphics mode, opcolor
        let url = full_box(b"url ", 0, 1, &[])?;
        let mut dref = 1u32.to_be_bytes().to_vec();
        dref.extend_from_slice(&url);
        let dinf = mp4_box(b"dinf", &full_box(b"dref", 0, 0, &dref)?)?;

        // av01 visual sample entry
        let mut av01 = Vec::new();
        av01.extend_from_slice(&[0; 6]);
        av01.extend_from_slice(&1u16.to_be_bytes()); // data reference index
        av01.extend_from_slice(&[0; 16]); // pre_defined, reserved
        av01.extend_from_slice(&self.width.to_be_bytes());
        av01.extend_from_slice(&self.height.to_be_bytes());
        av01.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
        av01.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        av01.extend_from_slice(&[0; 4]);
        av01.extend_from_slice(&1u16.to_be_bytes()); // frame count
        let mut name = [0u8; 32];
        let label = b"AOM Coding";
        name[0] = label.len() as u8;
        name[1..=label.len()].copy_from_slice(label);
        av01.extend_from_slice(&name);
        av01.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
        av01.extend_from_slice(&(-1i16).to_be_bytes());
        av01.extend_from_slice(&mp4_box(b"av1C", config)?);
        // BT.709 primaries, transfer and matrix, limited range
        let mut colr = b"nclx".to_vec();
        colr.extend_from_slice(&[0, 1, 0, 1, 0, 1, 0]);
        av01.extend_from_slice(&mp4_box(b"colr", &colr)?);
        let mut pasp = 1u32.to_be_bytes().to_vec();
        pasp.extend_from_slice(&1u32.to_be_bytes());
        av01.extend_from_slice(&mp4_box(b"pasp", &pasp)?);
        let mut stsd = 1u32.to_be_bytes().to_vec();
        stsd.extend_from_slice(&mp4_box(b"av01", &av01)?);

        // decoding times: runs of equal durations
        let mut runs: Vec<(u32, u64)> = Vec::new();
        for d in &self.durations {
            match runs.last_mut() {
                Some((count, delta)) if *delta == *d => *count += 1,
                _ => runs.push((1, *d)),
            }
        }
        let mut stts = u32::try_from(runs.len()).map_err(|_| bad("too many samples"))?.to_be_bytes().to_vec();
        for (count, delta) in runs {
            stts.extend_from_slice(&count.to_be_bytes());
            stts.extend_from_slice(&u32_of(delta.saturating_mul(TICKS))?.to_be_bytes());
        }
        let mut stsc = 1u32.to_be_bytes().to_vec();
        for v in [1u32, n, 1] {
            stsc.extend_from_slice(&v.to_be_bytes());
        }
        let mut stsz = 0u32.to_be_bytes().to_vec();
        stsz.extend_from_slice(&n.to_be_bytes());
        for s in &self.sizes {
            stsz.extend_from_slice(&s.to_be_bytes());
        }
        // one chunk holding every sample, at the start of mdat's data
        let chunk_offset = match u32::try_from(self.data_start) {
            Ok(o) => full_box(b"stco", 0, 0, &[1u32.to_be_bytes(), o.to_be_bytes()].concat())?,
            Err(_) => {
                let mut b = 1u32.to_be_bytes().to_vec();
                b.extend_from_slice(&self.data_start.to_be_bytes());
                full_box(b"co64", 0, 0, &b)?
            }
        };
        let mut stbl = Vec::new();
        stbl.extend_from_slice(&full_box(b"stsd", 0, 0, &stsd)?);
        stbl.extend_from_slice(&full_box(b"stts", 0, 0, &stts)?);
        if self.keys.len() < self.sizes.len() {
            let mut stss = u32::try_from(self.keys.len()).map_err(|_| bad("too many samples"))?.to_be_bytes().to_vec();
            for k in &self.keys {
                stss.extend_from_slice(&k.to_be_bytes());
            }
            stbl.extend_from_slice(&full_box(b"stss", 0, 0, &stss)?);
        }
        stbl.extend_from_slice(&full_box(b"stsc", 0, 0, &stsc)?);
        stbl.extend_from_slice(&full_box(b"stsz", 0, 0, &stsz)?);
        stbl.extend_from_slice(&chunk_offset);

        let mut minf = full_box(b"vmhd", 0, 1, &vmhd)?;
        minf.extend_from_slice(&dinf);
        minf.extend_from_slice(&mp4_box(b"stbl", &stbl)?);
        let mut mdia = full_box(b"mdhd", 0, 0, &mdhd)?;
        mdia.extend_from_slice(&full_box(b"hdlr", 0, 0, &hdlr)?);
        mdia.extend_from_slice(&mp4_box(b"minf", &minf)?);
        let mut trak = full_box(b"tkhd", 0, 3, &tkhd)?;
        trak.extend_from_slice(&mp4_box(b"mdia", &mdia)?);
        let mut moov = full_box(b"mvhd", 0, 0, &mvhd)?;
        moov.extend_from_slice(&mp4_box(b"trak", &trak)?);
        mp4_box(b"moov", &moov)
    }
}

// ---------------------------------------------------------------- WebM

/// An EBML element id with its length marker, as written (e.g. `0x1A45DFA3`).
fn ebml_id(id: u32) -> Vec<u8> {
    let b = id.to_be_bytes();
    let skip = b.iter().take_while(|x| **x == 0).count().min(3);
    b[skip..].to_vec()
}

/// An EBML size, in the fewest bytes (all-ones values are reserved for "unknown").
fn ebml_size(n: u64) -> Vec<u8> {
    for len in 1..=8u32 {
        if n < (1u64 << (7 * len)) - 1 {
            let mut v = Vec::with_capacity(len as usize);
            let marked = n | (1u64 << (7 * len));
            let bytes = marked.to_be_bytes();
            v.extend_from_slice(&bytes[(8 - len as usize)..]);
            return v;
        }
    }
    vec![0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]
}

/// An 8-byte EBML size (patchable in place).
fn ebml_size8(n: u64) -> [u8; 8] {
    let mut b = (n & 0x00ff_ffff_ffff_ffff).to_be_bytes();
    b[0] = 0x01;
    b
}

fn el(id: u32, body: &[u8]) -> Vec<u8> {
    let mut v = ebml_id(id);
    v.extend_from_slice(&ebml_size(body.len() as u64));
    v.extend_from_slice(body);
    v
}

fn el_uint(id: u32, n: u64) -> Vec<u8> {
    let b = n.to_be_bytes();
    let skip = b.iter().take_while(|x| **x == 0).count().min(7);
    el(id, &b[skip..])
}

fn el_float(id: u32, f: f64) -> Vec<u8> {
    el(id, &f.to_be_bytes())
}

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const VOID: u32 = 0xEC;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const CLUSTER: u32 = 0x1F43_B675;
const TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const BLOCK_DURATION: u32 = 0x9B;
const REFERENCE_BLOCK: u32 = 0xFB;
const CUES: u32 = 0x1C53_BB6B;

/// Room kept after the Segment header for the SeekHead written at the end.
const SEEK_ROOM: usize = 96;

/// A WebM writer (see the module docs). Timestamps are in milliseconds.
pub struct WebmWriter<W: Write + Seek> {
    w: W,
    width: u32,
    height: u32,
    fps: u32,
    segment_size_at: u64,
    segment_start: u64,
    duration_at: u64,
    info_at: u64,
    tracks_at: Option<u64>,
    config: Option<Vec<u8>>,
    /// Frame periods before the pending sample.
    periods: u64,
    /// The sample not written yet (the last one becomes a BlockGroup with its duration).
    pending: Option<(Sample, u64)>,
    cluster: Vec<u8>,
    cluster_ts: u64,
    last_block_ts: u64,
    cues: Vec<(u64, u64)>,
}

impl<W: Write + Seek> WebmWriter<W> {
    pub fn new(mut w: W, width: u32, height: u32, fps: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > 65_535 || height > 65_535 || !(1..=240).contains(&fps) {
            return Err(bad("bad size or frame rate"));
        }
        let mut header = Vec::new();
        header.extend_from_slice(&el_uint(0x4286, 1)); // EBMLVersion
        header.extend_from_slice(&el_uint(0x42F7, 1)); // EBMLReadVersion
        header.extend_from_slice(&el_uint(0x42F2, 4)); // EBMLMaxIDLength
        header.extend_from_slice(&el_uint(0x42F3, 8)); // EBMLMaxSizeLength
        header.extend_from_slice(&el(0x4282, b"webm")); // DocType
        header.extend_from_slice(&el_uint(0x4287, 4)); // DocTypeVersion
        header.extend_from_slice(&el_uint(0x4285, 2)); // DocTypeReadVersion
        w.write_all(&el(EBML, &header)).map_err(io)?;
        w.write_all(&ebml_id(SEGMENT)).map_err(io)?;
        let segment_size_at = w.stream_position().map_err(io)?;
        w.write_all(&ebml_size8(0)).map_err(io)?;
        let segment_start = w.stream_position().map_err(io)?;
        // a Void the SeekHead replaces at the end
        w.write_all(&el(VOID, &[0; SEEK_ROOM - 2])).map_err(io)?;
        let info_at = w.stream_position().map_err(io)? - segment_start;
        let mut info = el_uint(TIMESTAMP_SCALE, 1_000_000);
        let duration_offset = info.len() + 3; // after Duration's id (2 bytes) and size (1 byte)
        info.extend_from_slice(&el_float(DURATION, 0.0));
        info.extend_from_slice(&el(0x4D80, b"LightCraft")); // MuxingApp
        info.extend_from_slice(&el(0x5741, b"LightCraft")); // WritingApp
        let info_el = el(INFO, &info);
        let duration_at = segment_start + info_at + (info_el.len() - info.len()) as u64 + duration_offset as u64;
        w.write_all(&info_el).map_err(io)?;
        Ok(WebmWriter {
            w,
            width,
            height,
            fps,
            segment_size_at,
            segment_start,
            duration_at,
            info_at,
            tracks_at: None,
            config: None,
            periods: 0,
            pending: None,
            cluster: Vec::new(),
            cluster_ts: 0,
            last_block_ts: 0,
            cues: Vec::new(),
        })
    }

    /// The `CodecPrivate` (the `av1C` record, [`av1_config`]); needed before the first sample.
    pub fn set_config(&mut self, config: Vec<u8>) {
        self.config = Some(config);
    }

    fn ms(&self, periods: u64) -> u64 {
        periods.saturating_mul(1000) / u64::from(self.fps)
    }

    fn write_tracks(&mut self) -> Result<()> {
        let config = self.config.clone().ok_or_else(|| bad("no AV1 configuration"))?;
        let mut video = el_uint(0xB0, u64::from(self.width)); // PixelWidth
        video.extend_from_slice(&el_uint(0xBA, u64::from(self.height))); // PixelHeight
        let mut entry = el_uint(0xD7, 1); // TrackNumber
        entry.extend_from_slice(&el_uint(0x73C5, 1)); // TrackUID
        entry.extend_from_slice(&el_uint(0x83, 1)); // TrackType video
        entry.extend_from_slice(&el_uint(0x9C, 0)); // FlagLacing
        entry.extend_from_slice(&el(0x86, b"V_AV1")); // CodecID
        entry.extend_from_slice(&el(0x63A2, &config)); // CodecPrivate
        entry.extend_from_slice(&el_uint(0x23E383, 1_000_000_000 / u64::from(self.fps))); // DefaultDuration (ns)
        entry.extend_from_slice(&el(0xE0, &video));
        let at = self.w.stream_position().map_err(io)? - self.segment_start;
        self.w.write_all(&el(TRACKS, &el(TRACK_ENTRY, &entry))).map_err(io)?;
        self.tracks_at = Some(at);
        Ok(())
    }

    fn flush_cluster(&mut self) -> Result<()> {
        if self.cluster.is_empty() {
            return Ok(());
        }
        let mut body = el_uint(TIMESTAMP, self.cluster_ts);
        body.extend_from_slice(&self.cluster);
        let at = self.w.stream_position().map_err(io)? - self.segment_start;
        self.w.write_all(&el(CLUSTER, &body)).map_err(io)?;
        self.cues.push((self.cluster_ts, at));
        self.cluster.clear();
        Ok(())
    }

    /// The block header: track 1, timestamp relative to the cluster, flags.
    fn block(&self, data: &[u8], ts: u64, flags: u8) -> Result<Vec<u8>> {
        let rel = i16::try_from(ts.saturating_sub(self.cluster_ts)).map_err(|_| bad("block timestamp out of range"))?;
        let mut b = Vec::with_capacity(data.len() + 4);
        b.push(0x81);
        b.extend_from_slice(&rel.to_be_bytes());
        b.push(flags);
        b.extend_from_slice(data);
        Ok(b)
    }

    /// Start a new cluster at a key frame or before the relative timestamp could overflow.
    fn place(&mut self, key: bool, ts: u64) -> Result<()> {
        if !self.cluster.is_empty() && (key || ts.saturating_sub(self.cluster_ts) > 30_000) {
            self.flush_cluster()?;
        }
        if self.cluster.is_empty() {
            self.cluster_ts = ts;
        }
        Ok(())
    }

    fn write_simple(&mut self, s: &Sample, ts: u64) -> Result<()> {
        self.place(s.key, ts)?;
        let b = self.block(&s.data, ts, if s.key { 0x80 } else { 0 })?;
        self.cluster.extend_from_slice(&el(SIMPLE_BLOCK, &b));
        self.last_block_ts = ts;
        Ok(())
    }

    pub fn add(&mut self, s: &Sample) -> Result<()> {
        if s.duration == 0 {
            return Err(bad("a sample lasts at least one frame"));
        }
        if self.tracks_at.is_none() {
            if !s.key {
                return Err(bad("the first sample must be a key frame"));
            }
            self.write_tracks()?;
        }
        if let Some((p, start)) = self.pending.take() {
            let ts = self.ms(start);
            self.write_simple(&p, ts)?;
        }
        let start = self.periods;
        self.periods = self.periods.saturating_add(s.duration);
        if self.periods > MAX_PERIODS {
            return Err(bad("video too long"));
        }
        self.pending = Some((s.clone(), start));
        Ok(())
    }

    pub fn periods(&self) -> u64 {
        self.periods
    }

    pub fn finish(mut self) -> Result<W> {
        let (last, start) = self.pending.take().ok_or_else(|| bad("no frames"))?;
        let ts = self.ms(start);
        self.place(last.key, ts)?;
        let mut group = el(BLOCK, &self.block(&last.data, ts, 0)?);
        group.extend_from_slice(&el_uint(BLOCK_DURATION, self.ms(self.periods).saturating_sub(ts).max(1)));
        if !last.key {
            // the previous block, as a (negative) offset from this one
            let back = i64::try_from(ts.saturating_sub(self.last_block_ts)).unwrap_or(i64::MAX).max(1);
            el_int_into(&mut group, REFERENCE_BLOCK, -back);
        }
        self.cluster.extend_from_slice(&el(BLOCK_GROUP, &group));
        self.flush_cluster()?;
        // cues: one per cluster (each starts with a key frame or carries on from one)
        let cues_at = self.w.stream_position().map_err(io)? - self.segment_start;
        let mut cues = Vec::new();
        for (t, pos) in &self.cues {
            let mut positions = el_uint(0xF7, 1); // CueTrack
            positions.extend_from_slice(&el_uint(0xF1, *pos)); // CueClusterPosition
            let mut point = el_uint(0xB3, *t); // CueTime
            point.extend_from_slice(&el(0xB7, &positions)); // CueTrackPositions
            cues.extend_from_slice(&el(0xBB, &point)); // CuePoint
        }
        self.w.write_all(&el(CUES, &cues)).map_err(io)?;
        let end = self.w.stream_position().map_err(io)?;
        // SeekHead in the room kept for it, the rest stays a Void
        let tracks_at = self.tracks_at.ok_or_else(|| bad("no tracks"))?;
        let mut seeks = Vec::new();
        for (id, at) in [(INFO, self.info_at), (TRACKS, tracks_at), (CUES, cues_at)] {
            let mut seek = el(SEEK_ID, &ebml_id(id));
            seek.extend_from_slice(&el(SEEK_POSITION, &at.to_be_bytes()));
            seeks.extend_from_slice(&el(SEEK, &seek));
        }
        let head = el(SEEK_HEAD, &seeks);
        let rest = SEEK_ROOM.checked_sub(head.len()).filter(|r| *r >= 2).ok_or_else(|| bad("seek head too large"))?;
        let mut room = head;
        room.extend_from_slice(&el(VOID, &vec![0; rest - 2]));
        self.w.seek(SeekFrom::Start(self.segment_start)).map_err(io)?;
        self.w.write_all(&room).map_err(io)?;
        self.w.seek(SeekFrom::Start(self.duration_at)).map_err(io)?;
        self.w.write_all(&(self.ms(self.periods) as f64).to_be_bytes()).map_err(io)?;
        self.w.seek(SeekFrom::Start(self.segment_size_at)).map_err(io)?;
        self.w.write_all(&ebml_size8(end - self.segment_start)).map_err(io)?;
        self.w.seek(SeekFrom::Start(end)).map_err(io)?;
        self.w.flush().map_err(io)?;
        Ok(self.w)
    }
}

fn el_int_into(out: &mut Vec<u8>, id: u32, v: i64) {
    let b = v.to_be_bytes();
    // the fewest bytes that keep the sign
    let mut skip = 0;
    while skip < 7 && ((b[skip] == 0 && b[skip + 1] & 0x80 == 0) || (b[skip] == 0xff && b[skip + 1] & 0x80 != 0)) {
        skip += 1;
    }
    out.extend_from_slice(&el(id, &b[skip..]));
}

// ---------------------------------------------------------------- either container

/// An MP4 or WebM writer.
pub enum VideoWriter<W: Write + Seek> {
    Mp4(Mp4Writer<W>),
    WebM(WebmWriter<W>),
}

impl<W: Write + Seek> VideoWriter<W> {
    pub fn new(w: W, container: Container, width: u32, height: u32, fps: u32) -> Result<Self> {
        Ok(match container {
            Container::Mp4 => VideoWriter::Mp4(Mp4Writer::new(w, width, height, fps)?),
            Container::WebM => VideoWriter::WebM(WebmWriter::new(w, width, height, fps)?),
        })
    }

    pub fn set_config(&mut self, config: Vec<u8>) {
        match self {
            VideoWriter::Mp4(m) => m.set_config(config),
            VideoWriter::WebM(m) => m.set_config(config),
        }
    }

    pub fn add(&mut self, s: &Sample) -> Result<()> {
        match self {
            VideoWriter::Mp4(m) => m.add(s),
            VideoWriter::WebM(m) => m.add(s),
        }
    }

    pub fn periods(&self) -> u64 {
        match self {
            VideoWriter::Mp4(m) => m.periods(),
            VideoWriter::WebM(m) => m.periods(),
        }
    }

    pub fn finish(self) -> Result<W> {
        match self {
            VideoWriter::Mp4(m) => m.finish(),
            VideoWriter::WebM(m) => m.finish(),
        }
    }
}

// ---------------------------------------------------------------- AV1 encoder

/// How to encode: frame size (even numbers; odd ones are rounded down), nominal frame rate,
/// quality 1–100 and rav1e speed 1 (slow) – 10 (fast).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub quality: u8,
    pub speed: u8,
}

/// Is AV1 video encoding in this build (native, `avif` feature)?
pub const AV1_AVAILABLE: bool = cfg!(all(feature = "avif", not(target_arch = "wasm32")));

/// BT.709 limited-range 4:2:0 planes of an sRGB RGBA image (alpha ignored), `w × h` (even).
pub fn yuv420(rgba: &[[u8; 4]], w: usize, h: usize) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) || w.checked_mul(h).is_none_or(|n| rgba.len() < n) {
        return Err(bad("frame size"));
    }
    const KR: f32 = 0.2126;
    const KB: f32 = 0.0722;
    const KG: f32 = 1.0 - KR - KB;
    let mut y = vec![0u8; w * h];
    let (cw, ch) = (w / 2, h / 2);
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    let luma = |p: [u8; 4]| KR * f32::from(p[0]) + KG * f32::from(p[1]) + KB * f32::from(p[2]);
    for (i, out) in y.iter_mut().enumerate() {
        let p = rgba.get(i).copied().unwrap_or([0; 4]);
        *out = (16.0 + luma(p) * (219.0 / 255.0)).round().clamp(0.0, 255.0) as u8;
    }
    for cy in 0..ch {
        for cx in 0..cw {
            let mut acc = [0f32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = rgba.get((cy * 2 + dy) * w + cx * 2 + dx).copied().unwrap_or([0; 4]);
                for (a, c) in acc.iter_mut().zip(p) {
                    *a += f32::from(c) / 4.0;
                }
            }
            let l = KR * acc[0] + KG * acc[1] + KB * acc[2];
            let cb = (acc[2] - l) / (2.0 * (1.0 - KB));
            let cr = (acc[0] - l) / (2.0 * (1.0 - KR));
            let i = cy * cw + cx;
            if let Some(o) = u.get_mut(i) {
                *o = (128.0 + cb * (224.0 / 255.0)).round().clamp(0.0, 255.0) as u8;
            }
            if let Some(o) = v.get_mut(i) {
                *o = (128.0 + cr * (224.0 / 255.0)).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok((y, u, v))
}

/// The quantizer for `quality` (ravif's curve, so qualities match the AVIF export).
pub fn quantizer(quality: u8) -> usize {
    let q = f32::from(quality.clamp(1, 100)) / 100.0;
    let x = if q >= 0.82 {
        (1.0 - q) * 2.6
    } else if q > 0.25 {
        1.0 - 0.125 - q * 0.5
    } else {
        1.0 - q
    };
    (x * 255.0).round().clamp(0.0, 255.0) as usize
}

/// An AV1 encoder fed RGBA frames, each with how many frame periods it stays up; it hands back
/// the samples as they come out (in order: no frame reordering).
pub struct Av1Encoder {
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    ctx: rav1e::Context<u8>,
    /// Each frame sent so far: its duration (by input frame number).
    #[cfg_attr(not(all(feature = "avif", not(target_arch = "wasm32"))), allow(dead_code))]
    durations: Vec<u64>,
    width: usize,
    height: usize,
    seq_header: Option<Vec<u8>>,
}

impl Av1Encoder {
    pub fn new(c: &VideoConfig) -> Result<Self> {
        let (w, h) = ((c.width & !1) as usize, (c.height & !1) as usize);
        if w < 16 || h < 16 || w > 8192 || h > 8192 || !(1..=240).contains(&c.fps) {
            return Err(bad("unsupported video size or frame rate"));
        }
        #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
        {
            use rav1e::prelude::*;
            let mut ec = EncoderConfig::with_speed_preset(c.speed.clamp(1, 10));
            ec.width = w;
            ec.height = h;
            ec.time_base = Rational::new(1, u64::from(c.fps));
            ec.bit_depth = 8;
            ec.chroma_sampling = ChromaSampling::Cs420;
            ec.pixel_range = PixelRange::Limited;
            ec.color_description = Some(ColorDescription {
                color_primaries: ColorPrimaries::BT709,
                transfer_characteristics: TransferCharacteristics::BT709,
                matrix_coefficients: MatrixCoefficients::BT709,
            });
            // in order, one shown frame per packet: samples map 1:1 to the frames sent
            ec.low_latency = true;
            let q = quantizer(c.quality);
            ec.quantizer = q;
            ec.min_quantizer = q.min(255) as u8;
            ec.min_key_frame_interval = 1;
            ec.max_key_frame_interval = 120;
            // tiles are what rav1e encodes in parallel (16 measured best for 1080p on 32 threads)
            ec.tiles = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16);
            let ctx: Context<u8> = Config::new().with_encoder_config(ec).new_context().map_err(|e| bad(format!("AV1: {e}")))?;
            Ok(Av1Encoder { ctx, durations: Vec::new(), width: w, height: h, seq_header: None })
        }
        #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
        {
            let _ = (w, h);
            Err(bad("AV1 video encoding is not available in this build"))
        }
    }

    /// The frame size actually encoded (even numbers).
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The configuration record for the container ([`av1_config`]), once the first sample (with
    /// the sequence header) has come out.
    pub fn config_record(&self) -> Option<Vec<u8>> {
        self.seq_header.as_ref().map(|s| av1_config(s))
    }

    /// Encode `rgba` (`width × height` of [`Self::size`]) shown for `periods` frame periods;
    /// `key` forces a key frame (a picture that doesn't lean on earlier ones: a held slide is
    /// then clean of the slide before it, and seeking lands on it).
    pub fn push(&mut self, rgba: &[[u8; 4]], periods: u64, key: bool) -> Result<Vec<Sample>> {
        let (y, u, v) = yuv420(rgba, self.width, self.height)?;
        #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
        {
            let mut frame = self.ctx.new_frame();
            for (plane, (data, w)) in frame.planes.iter_mut().zip([(&y, self.width), (&u, self.width / 2), (&v, self.width / 2)]) {
                plane.copy_from_raw_u8(data, w, 1);
            }
            let params = rav1e::prelude::FrameParameters {
                frame_type_override: if key { rav1e::prelude::FrameTypeOverride::Key } else { rav1e::prelude::FrameTypeOverride::No },
                ..Default::default()
            };
            self.ctx.send_frame((frame, params)).map_err(|e| bad(format!("AV1: {e}")))?;
            self.durations.push(periods.max(1));
            self.drain()
        }
        #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
        {
            let _ = (y, u, v, periods, key);
            Err(bad("AV1 video encoding is not available in this build"))
        }
    }

    /// Flush the encoder: the samples still inside.
    pub fn finish(&mut self) -> Result<Vec<Sample>> {
        #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
        {
            self.ctx.flush();
            self.drain()
        }
        #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
        Err(bad("AV1 video encoding is not available in this build"))
    }

    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    fn drain(&mut self) -> Result<Vec<Sample>> {
        use rav1e::prelude::*;
        let mut out = Vec::new();
        loop {
            match self.ctx.receive_packet() {
                Ok(p) => {
                    let (data, seq) = to_sample_data(&p.data)?;
                    if self.seq_header.is_none() {
                        self.seq_header = seq;
                    }
                    let duration = usize::try_from(p.input_frameno).ok().and_then(|i| self.durations.get(i)).copied().unwrap_or(1);
                    out.push(Sample { data, key: p.frame_type == FrameType::KEY, duration });
                }
                Err(EncoderStatus::Encoded) => {}
                Err(EncoderStatus::NeedMoreData | EncoderStatus::LimitReached) => break,
                Err(e) => return Err(bad(format!("AV1: {e}"))),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The boxes of an MP4 byte range: (type, body range).
    pub fn boxes(d: &[u8], range: std::ops::Range<usize>) -> Vec<([u8; 4], std::ops::Range<usize>)> {
        let mut out = Vec::new();
        let mut p = range.start;
        while p + 8 <= range.end {
            let size32 = u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
            let (hdr, size) = if size32 == 1 { (16, u64::from_be_bytes(d[p + 8..p + 16].try_into().unwrap()) as usize) } else { (8, size32) };
            assert!(size >= hdr && p + size <= range.end, "box {} overruns", String::from_utf8_lossy(&kind));
            out.push((kind, p + hdr..p + size));
            p += size;
        }
        assert_eq!(p, range.end, "trailing bytes");
        out
    }

    fn find(d: &[u8], r: std::ops::Range<usize>, path: &[&[u8; 4]]) -> std::ops::Range<usize> {
        let mut r = r;
        for k in path {
            r = boxes(d, r).into_iter().find(|(t, _)| t == *k).unwrap_or_else(|| panic!("no {}", String::from_utf8_lossy(*k))).1;
        }
        r
    }

    fn samples() -> Vec<Sample> {
        // fake AV1 data: an OBU_FRAME (type 6) with a size field
        let frame = |b: u8| vec![0x32, 2, b, b];
        vec![
            Sample { data: frame(1), key: true, duration: 30 },
            Sample { data: frame(2), key: false, duration: 1 },
            Sample { data: frame(3), key: false, duration: 1 },
            Sample { data: frame(4), key: true, duration: 58 },
        ]
    }

    #[test]
    fn obus_split_and_temporal_delimiters_go() {
        // TD, sequence header (3 bytes), frame (2 bytes)
        let tu = [0x12, 0, 0x0a, 3, 1, 2, 3, 0x32, 2, 9, 9];
        let o = obus(&tu).unwrap();
        assert_eq!(o.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![2, 1, 6]);
        let (data, seq) = to_sample_data(&tu).unwrap();
        assert_eq!(data, &tu[2..]);
        assert_eq!(seq.unwrap(), &tu[2..7]);
        // hostile: sizes past the end, forbidden bit, endless LEB128
        assert!(obus(&[0x32, 9, 1]).is_err());
        assert!(obus(&[0x80]).is_err());
        assert!(obus(&[0x32, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(obus(&[0x36]).is_err(), "extension byte missing");
        assert_eq!(av1_config(&[1, 2])[..], [0x81, 31, 0x0c, 0, 1, 2]);
    }

    #[test]
    fn mp4_box_tree_samples_and_durations() {
        let mut m = Mp4Writer::new(Cursor::new(Vec::new()), 64, 36, 30).unwrap();
        assert!(m.add(&Sample { data: vec![1], key: false, duration: 1 }).is_err(), "must start on a key frame");
        for s in samples() {
            m.add(&s).unwrap();
        }
        assert!(Mp4Writer::new(Cursor::new(Vec::new()), 64, 36, 30).unwrap().finish().is_err(), "no frames");
        m.set_config(av1_config(&[0x0a, 1, 0]));
        let d = m.finish().unwrap().into_inner();
        let top = boxes(&d, 0..d.len());
        assert_eq!(top.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![*b"ftyp", *b"mdat", *b"moov"]);
        assert_eq!(&d[top[1].1.clone()], &[0x32, 2, 1, 1, 0x32, 2, 2, 2, 0x32, 2, 3, 3, 0x32, 2, 4, 4]);
        let stbl = find(&d, 0..d.len(), &[b"moov", b"trak", b"mdia", b"minf", b"stbl"]);
        let kinds: Vec<_> = boxes(&d, stbl.clone()).into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, vec![*b"stsd", *b"stts", *b"stss", *b"stsc", *b"stsz", *b"stco"]);
        let be = |r: usize| u32::from_be_bytes(d[r..r + 4].try_into().unwrap());
        let stsz = find(&d, stbl.clone(), &[b"stsz"]);
        assert_eq!(be(stsz.start + 8), 4, "sample count");
        let stts = find(&d, stbl.clone(), &[b"stts"]);
        assert_eq!(be(stts.start + 4), 3, "runs: 30, 1×2, 58");
        let total: u32 = (0..3).map(|i| be(stts.start + 8 + i * 8) * be(stts.start + 12 + i * 8)).sum();
        assert_eq!(total, 90 * 512);
        let stss = find(&d, stbl.clone(), &[b"stss"]);
        assert_eq!((be(stss.start + 4), be(stss.start + 8), be(stss.start + 12)), (2, 1, 4));
        let stco = find(&d, stbl.clone(), &[b"stco"]);
        assert_eq!(be(stco.start + 8) as usize, top[1].1.start, "chunk offset = mdat data");
        let stsd = find(&d, stbl, &[b"stsd"]);
        let av01 = boxes(&d, stsd.start + 8..stsd.end);
        assert_eq!(av01[0].0, *b"av01");
        let entry = av01[0].1.clone();
        assert_eq!(u16::from_be_bytes([d[entry.start + 24], d[entry.start + 25]]), 64);
        let inner: Vec<_> = boxes(&d, entry.start + 78..entry.end).into_iter().map(|(k, _)| k).collect();
        assert_eq!(inner, vec![*b"av1C", *b"colr", *b"pasp"]);
        let mdhd = find(&d, 0..d.len(), &[b"moov", b"trak", b"mdia", b"mdhd"]);
        assert_eq!((be(mdhd.start + 12), be(mdhd.start + 16)), (30 * 512, 90 * 512));
        let mvhd = find(&d, 0..d.len(), &[b"moov", b"mvhd"]);
        assert_eq!(be(mvhd.start + 16), 3000, "3 s in ms");
    }

    /// The EBML elements of a byte range: (id, body range).
    fn elements(d: &[u8], range: std::ops::Range<usize>) -> Vec<(u32, std::ops::Range<usize>)> {
        let vint = |p: usize| -> (u64, usize) {
            let len = d[p].leading_zeros() as usize + 1;
            let mut v = u64::from(d[p]) & (0xff >> len);
            for i in 1..len {
                v = (v << 8) | u64::from(d[p + i]);
            }
            (v, len)
        };
        let mut out = Vec::new();
        let mut p = range.start;
        while p < range.end {
            let idlen = d[p].leading_zeros() as usize + 1;
            let id = d[p..p + idlen].iter().fold(0u32, |a, b| (a << 8) | u32::from(*b));
            let (size, n) = vint(p + idlen);
            let start = p + idlen + n;
            let end = start + size as usize;
            assert!(end <= range.end, "element {id:x} overruns");
            out.push((id, start..end));
            p = end;
        }
        out
    }

    #[test]
    fn webm_element_tree_blocks_and_duration() {
        let mut m = WebmWriter::new(Cursor::new(Vec::new()), 64, 36, 30).unwrap();
        assert!(m.add(&Sample { data: vec![0x32, 0], key: true, duration: 1 }).is_err(), "configuration first");
        let mut m = WebmWriter::new(Cursor::new(Vec::new()), 64, 36, 30).unwrap();
        m.set_config(av1_config(&[0x0a, 1, 0]));
        for s in samples() {
            m.add(&s).unwrap();
        }
        let d = m.finish().unwrap().into_inner();
        let top = elements(&d, 0..d.len());
        assert_eq!(top.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![EBML, SEGMENT]);
        let seg = elements(&d, top[1].1.clone());
        let ids: Vec<_> = seg.iter().map(|(i, _)| *i).collect();
        assert_eq!(ids, vec![SEEK_HEAD, VOID, INFO, TRACKS, CLUSTER, CLUSTER, CUES]);
        let info = elements(&d, seg[2].1.clone());
        let dur = info.iter().find(|(i, _)| *i == DURATION).unwrap().1.clone();
        assert_eq!(f64::from_be_bytes(d[dur].try_into().unwrap()), 3000.0);
        let track = elements(&d, elements(&d, seg[3].1.clone())[0].1.clone());
        let codec = track.iter().find(|(i, _)| *i == 0x86).unwrap().1.clone();
        assert_eq!(&d[codec], b"V_AV1");
        let first: Vec<_> = elements(&d, seg[4].1.clone()).into_iter().map(|(i, _)| i).collect();
        assert_eq!(first, vec![TIMESTAMP, SIMPLE_BLOCK, SIMPLE_BLOCK, SIMPLE_BLOCK]);
        let second = elements(&d, seg[5].1.clone());
        assert_eq!(d[second[0].1.clone()], [0x04, 0x2a], "the second cluster starts at 1066 ms (32 periods)");
        let group = elements(&d, second[1].1.clone());
        assert_eq!(group.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![BLOCK, BLOCK_DURATION]);
        // the SeekHead points at Info, Tracks and Cues
        let seg_start = top[1].1.start;
        for seek in elements(&d, seg[0].1.clone()) {
            let e = elements(&d, seek.1);
            let pos = d[e[1].1.clone()].iter().fold(0usize, |a, b| (a << 8) | usize::from(*b));
            let id = d[e[0].1.clone()].iter().fold(0u32, |a, b| (a << 8) | u32::from(*b));
            assert!(seg.iter().any(|(i, r)| *i == id && r.start > seg_start + pos), "{id:x}");
        }
        let mut neg = Vec::new();
        el_int_into(&mut neg, REFERENCE_BLOCK, -33);
        assert_eq!(neg, vec![0xfb, 0x81, 0xdf]);
        assert_eq!(ebml_size(126), vec![0xfe]);
        assert_eq!(ebml_size(127), vec![0x40, 0x7f]);
    }

    #[test]
    fn hostile_sizes_are_refused() {
        assert!(Mp4Writer::new(Cursor::new(Vec::new()), 0, 10, 30).is_err());
        assert!(Mp4Writer::new(Cursor::new(Vec::new()), 70_000, 10, 30).is_err());
        assert!(WebmWriter::new(Cursor::new(Vec::new()), 10, 10, 0).is_err());
        assert!(yuv420(&[[0; 4]; 3], 2, 2).is_err());
        assert!(yuv420(&[[0; 4]; 9], 3, 3).is_err());
        let (y, u, v) = yuv420(&[[255; 4]; 4], 2, 2).unwrap();
        assert_eq!((y, u, v), (vec![235; 4], vec![128], vec![128]));
        assert!(quantizer(100) < quantizer(50) && quantizer(50) < quantizer(1));
        assert!(Av1Encoder::new(&VideoConfig { width: 8, height: 8, fps: 30, quality: 50, speed: 10 }).is_err());
    }

    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    #[test]
    fn encodes_av1_into_a_playable_mp4() {
        let c = VideoConfig { width: 64, height: 48, fps: 24, quality: 60, speed: 10 };
        let mut e = Av1Encoder::new(&c).unwrap();
        let mut out = Vec::new();
        for (i, periods) in [(0u8, 24u64), (80, 1), (160, 1), (240, 30)] {
            let img = vec![[i, 255 - i, 128, 255]; 64 * 48];
            out.extend(e.push(&img, periods, periods > 1).unwrap());
        }
        out.extend(e.finish().unwrap());
        assert_eq!(out.len(), 4, "one sample per frame sent");
        assert!(out[0].key);
        assert_eq!(out.iter().map(|s| s.duration).sum::<u64>(), 56);
        for s in &out {
            assert!(obus(&s.data).unwrap().iter().all(|(k, _)| *k != OBU_TEMPORAL_DELIMITER));
        }
        let config = e.config_record().unwrap();
        assert_eq!(obus(&config[4..]).unwrap()[0].0, OBU_SEQUENCE_HEADER);
        let mut m = VideoWriter::new(Cursor::new(Vec::new()), Container::Mp4, 64, 48, 24).unwrap();
        m.set_config(config);
        for s in &out {
            m.add(s).unwrap();
        }
        let d = m.finish().unwrap().into_inner();
        assert_eq!(boxes(&d, 0..d.len()).len(), 3);
    }
}
