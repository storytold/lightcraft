//! Nikon pixel-shift series: reading the frames' headers and checking that a set of files is one complete series.
//!
//! A pixel-shift capture is 4, 8, 16 or 32 ordinary NEFs shot in a burst with the sensor moved between them.
//! What the maker note records about it was measured from one series of each mode and from lone frames of one body
//! (a Z 8, FX 3:2 and 1:1 image area), and checked on every series of a larger test set of all four modes and lone
//! frames; no other tool's source or tag table was consulted. Every meaning
//! below is a hypothesis that those files bore out:
//!
//! | maker note tag | content |
//! |---|---|
//! | `0x005e` (28 bytes, byte 4) | the mode: 0 = not pixel shift (a lone frame), 1, 2, 3, 4 = a series of 4, 8, 16, 32 shots |
//! | `0x0056` (16 bytes, byte 8) | the frame's number in the series, 1-based |
//! | `0x005e` (byte 5) | the frame's placement slot: the frame number, except in an 8-shot series where frames 5 to 8 have 17 to 20 (number + 12) |
//! | `0x0034` (Short) | 16 in every mode and for lone frames: **not** the shot count, only reported |
//! | `0x00a7` (Long) | a per-exposure counter that rises by one from frame to frame |
//! | `0x0054` (20 bytes: version, int32 at 4, 4 bytes, int32 at 12, 4 bytes) | the sensor position of the frame (x, y). It follows the real image shift to about 0.1 px and is **only** reported, never used to place a frame |
//! | `0x001b` (7 Shorts), `0x0045` (4 Shorts) | where the stored frame sits on the sensor, and the image area inside it |
//!
//! There is no series or group id: a series is told by its mode, its frame numbers (1..N in order), the counter and the
//! time between exposures (about 0.12 s). Only the 16-shot merge is built; the other modes are recognised so that they
//! can be refused by name.

use lcc_tiff::{Tiff, makernote, tags as t};

/// Maker note tags (own names; see the module documentation).
pub mod tag {
    pub const SHOT_COUNT: u16 = 0x0034;
    pub const FRAME_INDEX_A: u16 = 0x0056;
    pub const FRAME_INDEX_B: u16 = 0x005e;
    pub const POSITION: u16 = 0x0054;
    pub const EXPOSURE_COUNTER: u16 = 0x00a7;
    pub const SENSOR_REGION: u16 = 0x001b;
    pub const IMAGE_AREA: u16 = 0x0045;
    pub const WHITE_BALANCE: u16 = 0x000c;
}

/// Byte of `0x0056` that holds the frame number, and bytes of `0x005e` that hold the mode and the placement slot.
const NUMBER_BYTE: usize = 8;
const MODE_BYTE: usize = 4;
const SLOT_BYTE: usize = 5;
/// Spacing may differ from the series' median interval by half of it, but by at least this much (timing jitter
/// and the camera's sub-second time resolution).
const MIN_JITTER_MS: i64 = 250;

/// What one frame says about itself, read without decoding its pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameHeader {
    pub camera: String,
    pub lens: String,
    /// Stored raw size (width, height).
    pub raw_size: (u32, u32),
    /// Maker note `0x0045`: x, y, width, height of the image area inside the stored raw.
    pub image_area: Option<[u16; 4]>,
    /// Maker note `0x001b`, as stored.
    pub sensor_region: Option<[u16; 7]>,
    pub exposure_time: f64,
    pub f_number: f64,
    pub iso: u32,
    pub exposure_bias: f64,
    /// Maker note `0x000c` as shot (R, B relative to G, and two more values).
    pub white_balance: Option<[f64; 4]>,
    /// Shots in the series, when the frame says it is part of one.
    pub shot_count: Option<u16>,
    /// 1-based position in the series.
    pub index: Option<u16>,
    pub counter: Option<u32>,
    /// Capture time in milliseconds (date and sub-second together; only differences matter).
    pub time_ms: Option<i64>,
    /// Maker note `0x0054`: diagnostic only.
    pub position: Option<(i32, i32)>,
}

/// Why a file is not a pixel-shift frame.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("not a TIFF-based raw file")]
    NotTiff,
    #[error("no Nikon maker note")]
    NoMakerNote,
    #[error("not a pixel-shift frame (the maker note has no pixel-shift mode and frame number)")]
    NotPixelShift,
    /// A pixel-shift mode this reader does not know (the mode byte is none of 1 to 4).
    #[error("not a pixel-shift mode this reader knows ({0})")]
    UnknownMode(u8),
}

fn rational(ifd: &lcc_tiff::Ifd, tag: u16) -> f64 {
    ifd.f64(tag).filter(|v| v.is_finite()).unwrap_or(0.0)
}

/// Milliseconds since an arbitrary epoch from `YYYY:MM:DD HH:MM:SS` and the sub-second digits.
fn time_ms(date: &str, sub: &str) -> Option<i64> {
    let d: Vec<i64> = date.trim().split([':', ' ']).map(|p| p.parse::<i64>().ok()).collect::<Option<_>>()?;
    let [y, mo, da, h, mi, s] = d[..] else { return None };
    // (a date from a file: bounded, so the arithmetic below cannot overflow)
    if !(1..=9999).contains(&y)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&da)
        || !(0..=24).contains(&h)
        || !(0..=59).contains(&mi)
        || !(0..=60).contains(&s)
    {
        return None;
    }
    // days from civil (Hinnant), good enough for differences
    let (yy, m) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * m + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe;
    let secs = days * 86_400 + h * 3600 + mi * 60 + s;
    let digits: String = sub.trim().chars().filter(char::is_ascii_digit).take(3).collect();
    let frac = if digits.is_empty() { 0 } else { digits.parse::<i64>().ok()? * 10i64.pow(3 - digits.len() as u32) };
    Some(secs * 1000 + frac)
}

fn le_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The placement slot a frame of a series of `shots` has: its number, except for frames 5 to 8 of an 8-shot series, which
/// have slots 17 to 20.
fn expected_slot(shots: u16, number: u16) -> u16 {
    if shots == 8 && number > 4 { number.saturating_add(12) } else { number }
}

/// Read the header of a NEF (the bytes may be the whole file or its first few hundred kilobytes: the maker note
/// ends about 260 KB in). `Err(NotPixelShift)` for an ordinary frame (mode 0 or no pixel-shift tags) and
/// `Err(UnknownMode)` for a mode this reader does not know.
pub fn read_header(bytes: &[u8]) -> Result<FrameHeader, HeaderError> {
    let tiff = Tiff::parse(bytes).map_err(|_| HeaderError::NotTiff)?;
    let ifd0 = tiff.ifds.first().ok_or(HeaderError::NotTiff)?;
    let exif = tiff.exif().ok_or(HeaderError::NoMakerNote)?;
    let make = ifd0.string(t::MAKE).unwrap_or_default();
    let model = ifd0.string(t::MODEL).unwrap_or_default();
    let entry = exif.get(t::MAKER_NOTE).ok_or(HeaderError::NoMakerNote)?;
    let note = makernote::parse_makernote(bytes, entry.offset, entry.count() as u64, tiff.order, &make).ok_or(HeaderError::NoMakerNote)?;
    let mn = &note.ifd;

    // (`0x0034` says 16 in every mode: it is not the shot count)
    let number = mn.bytes(tag::FRAME_INDEX_A).and_then(|b| b.get(NUMBER_BYTE)).copied();
    let mode = mn.bytes(tag::FRAME_INDEX_B).and_then(|b| b.get(MODE_BYTE)).copied();
    let slot = mn.bytes(tag::FRAME_INDEX_B).and_then(|b| b.get(SLOT_BYTE)).copied();
    let (Some(number), Some(mode), Some(slot)) = (number, mode, slot) else { return Err(HeaderError::NotPixelShift) };
    let shots = match mode {
        0 => return Err(HeaderError::NotPixelShift),
        1 => 4,
        2 => 8,
        3 => 16,
        4 => 32,
        other => return Err(HeaderError::UnknownMode(other)),
    };
    // the frame number counts from 1 to the shot count, and the slot must be the one the mode gives that number; a
    // frame where they don't agree is reported with no number (detection refuses it)
    let index = ((1..=shots).contains(&u16::from(number)) && u16::from(slot) == expected_slot(shots, u16::from(number))).then_some(u16::from(number));
    let shot_count = Some(shots);
    // the raw is the largest image in the file
    let raw_size = tiff
        .all_ifds()
        .iter()
        .filter_map(|i| Some((i.u32(t::IMAGE_WIDTH)?, i.u32(t::IMAGE_LENGTH)?)))
        .max_by_key(|(w, h)| u64::from(*w) * u64::from(*h))
        .unwrap_or((0, 0));
    let shorts = |tag: u16| -> Vec<u16> { mn.u64s(tag).unwrap_or_default().into_iter().map(|v| v as u16).collect() };
    let image_area = <[u16; 4]>::try_from(shorts(tag::IMAGE_AREA)).ok();
    let sensor_region = <[u16; 7]>::try_from(shorts(tag::SENSOR_REGION)).ok();
    let white_balance = mn.f64s(tag::WHITE_BALANCE).and_then(|v| <[f64; 4]>::try_from(v).ok());
    let position = mn.bytes(tag::POSITION).and_then(|b| Some((le_i32(b, 4)?, le_i32(b, 12)?)));
    let date = exif.string(t::DATE_TIME_ORIGINAL).unwrap_or_default();
    let sub = exif.string(0x9291).unwrap_or_default();
    Ok(FrameHeader {
        camera: format!("{make} {model}").trim().to_string(),
        lens: exif.string(0xa434).unwrap_or_default(),
        raw_size,
        image_area,
        sensor_region,
        exposure_time: rational(exif, t::EXPOSURE_TIME),
        f_number: rational(exif, t::F_NUMBER),
        iso: exif.u32(0x8827).unwrap_or(0),
        exposure_bias: rational(exif, t::EXPOSURE_BIAS),
        white_balance,
        shot_count,
        index,
        counter: mn.u64(tag::EXPOSURE_COUNTER).map(|v| v as u32),
        time_ms: time_ms(&date, &sub),
        position,
    })
}

/// How much of a file's start is normally enough for [`read_header`] (the maker note ends about 260 KB in).
pub const HEADER_PREFIX_BYTES: usize = 512 * 1024;

/// [`read_header`] on the start of a file (`prefix`, of a file `file_len` bytes long), falling back to the whole
/// file from `full` when the start doesn't hold the tags (a note stored further in, another camera's layout). `full`
/// is not called when the prefix already is the whole file.
pub fn read_header_with(prefix: &[u8], file_len: u64, full: impl FnOnce() -> Option<Vec<u8>>) -> Result<FrameHeader, HeaderError> {
    match read_header(prefix) {
        Ok(h) => Ok(h),
        Err(first) if (prefix.len() as u64) < file_len => match full() {
            Some(all) => read_header(&all),
            None => Err(first),
        },
        Err(first) => Err(first),
    }
}

/// Why a set of files is not one complete pixel-shift series.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SeriesError {
    #[error("no photos to merge")]
    Empty,
    #[error("photo {0} has no (consistent) pixel-shift frame number")]
    NoIndex(usize),
    #[error("the photos are from series with different shot counts")]
    MixedCount(usize),
    #[error("the series has {count} shots but {have} photos were given")]
    Incomplete { have: usize, count: usize },
    #[error("frame {0} of the series appears more than once")]
    Duplicate(u16),
    #[error("frame {0} of the series is missing")]
    Missing(u16),
    #[error("the photos are from different cameras or lenses")]
    MixedCamera,
    #[error("the photos have different exposure settings (shutter, aperture, ISO, exposure compensation or white balance)")]
    MixedExposure,
    #[error("the photos have different raw sizes or image areas")]
    MixedGeometry,
    #[error("the frames' exposure counters don't follow one another: not one burst")]
    CounterGap,
    #[error("the frames were not shot one right after the other")]
    TimeGap,
}

/// A checked series: the input positions of its frames in series order (frame 01 first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Series {
    pub count: u16,
    pub order: Vec<usize>,
}

/// The shot count of the series `headers` are frames of: the mode all of them are in (4, 8, 16 or 32). Frames of
/// different modes are [`SeriesError::MixedCount`]. This judges the mode only, so that a series that can not be merged
/// (whichever of its frames are chosen) is told apart from one that is incomplete; [`detect`] checks the rest.
pub fn series_shot_count(headers: &[FrameHeader]) -> Result<u16, SeriesError> {
    let first = headers.first().ok_or(SeriesError::Empty)?;
    let count = first.shot_count.ok_or(SeriesError::NoIndex(1))?;
    match headers.iter().position(|h| h.shot_count != Some(count)) {
        Some(_) => Err(SeriesError::MixedCount(headers.len())),
        None => Ok(count),
    }
}

/// Check that `headers` (in any order) are one complete series and put them in series order.
pub fn detect(headers: &[FrameHeader]) -> Result<Series, SeriesError> {
    let first = headers.first().ok_or(SeriesError::Empty)?;
    for (i, h) in headers.iter().enumerate() {
        if h.index.is_none_or(|v| v == 0) || h.shot_count.is_none() {
            return Err(SeriesError::NoIndex(i + 1));
        }
    }
    let count = first.shot_count.unwrap_or(0);
    if headers.iter().any(|h| h.shot_count != Some(count)) {
        return Err(SeriesError::MixedCount(headers.len()));
    }
    if headers.iter().any(|h| h.camera != first.camera || h.lens != first.lens) {
        return Err(SeriesError::MixedCamera);
    }
    let same_exposure = |h: &FrameHeader| {
        h.exposure_time == first.exposure_time
            && h.f_number == first.f_number
            && h.iso == first.iso
            && h.exposure_bias == first.exposure_bias
            && h.white_balance == first.white_balance
    };
    if !headers.iter().all(same_exposure) {
        return Err(SeriesError::MixedExposure);
    }
    if headers.iter().any(|h| h.raw_size != first.raw_size || h.image_area != first.image_area || h.sensor_region != first.sensor_region) {
        return Err(SeriesError::MixedGeometry);
    }
    // by frame number
    let mut order: Vec<usize> = (0..headers.len()).collect();
    order.sort_by_key(|&i| headers[i].index);
    for w in order.windows(2) {
        let (a, b) = (headers[w[0]].index.unwrap_or(0), headers[w[1]].index.unwrap_or(0));
        if a == b {
            return Err(SeriesError::Duplicate(a));
        }
    }
    if headers.len() != usize::from(count) {
        // name the first missing frame when the numbers are the problem, else report the count
        for n in 1..=count {
            if !headers.iter().any(|h| h.index == Some(n)) {
                return Err(SeriesError::Missing(n));
            }
        }
        return Err(SeriesError::Incomplete { have: headers.len(), count: usize::from(count) });
    }
    if headers[order[0]].index != Some(1) || headers[order[headers.len() - 1]].index != Some(count) {
        return Err(SeriesError::Missing(headers[order[0]].index.filter(|v| *v != 1).map_or(count, |_| 1)));
    }
    for w in order.windows(2) {
        let (a, b) = (&headers[w[0]], &headers[w[1]]);
        if a.counter.zip(b.counter).is_none_or(|(x, y)| y != x.wrapping_add(1)) {
            return Err(SeriesError::CounterGap);
        }
    }
    // Even spacing, judged against the series' own median interval: the camera's interval setting runs from
    // none (about 0.12 s between exposures) to many seconds, so no absolute limit would fit every series.
    let times: Option<Vec<i64>> = order.iter().map(|&i| headers[i].time_ms).collect();
    let Some(times) = times else { return Err(SeriesError::TimeGap) };
    let mut gaps: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    if gaps.iter().any(|g| *g < 0) {
        return Err(SeriesError::TimeGap);
    }
    let mut sorted = gaps.clone();
    sorted.sort_unstable();
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
    let tolerance = (median / 2).max(MIN_JITTER_MS);
    if gaps.drain(..).any(|g| (g - median).abs() > tolerance) {
        return Err(SeriesError::TimeGap);
    }
    Ok(Series { count, order })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};

    fn hdr(index: u16, count: u16) -> FrameHeader {
        FrameHeader {
            camera: "NIKON CORPORATION NIKON Z 8".into(),
            lens: "NIKKOR Z 50mm f/1.8 S".into(),
            raw_size: (80, 60),
            image_area: Some([4, 4, 72, 52]),
            sensor_region: Some([11, 80, 60, 80, 60, 0, 0]),
            exposure_time: 0.01,
            f_number: 5.6,
            iso: 64,
            exposure_bias: 0.0,
            white_balance: Some([1.8, 1.4, 1.0, 1.0]),
            shot_count: Some(count),
            index: Some(index),
            counter: Some(1000 + u32::from(index)),
            time_ms: Some(5_000 + 120 * i64::from(index)),
            position: Some((0, 0)),
        }
    }

    fn series(n: u16) -> Vec<FrameHeader> {
        (1..=n).map(|i| hdr(i, n)).collect()
    }

    #[test]
    fn a_complete_series_is_accepted_in_any_input_order() {
        let mut v = series(16);
        v.reverse();
        let s = detect(&v).unwrap();
        assert_eq!(s.count, 16);
        // frame 01 is the last input photo now
        assert_eq!(s.order[0], 15);
        assert_eq!(s.order[15], 0);
        for (n, &i) in s.order.iter().enumerate() {
            assert_eq!(v[i].index, Some(n as u16 + 1));
        }
    }

    #[test]
    fn incomplete_duplicate_and_wrong_start_are_refused() {
        let mut v = series(16);
        v.remove(5);
        assert_eq!(detect(&v), Err(SeriesError::Missing(6)));
        let mut v = series(16);
        v.pop();
        assert_eq!(detect(&v), Err(SeriesError::Missing(16)));
        let mut v = series(16);
        v[3] = v[2].clone();
        assert_eq!(detect(&v), Err(SeriesError::Duplicate(3)));
        let v = series(16)[1..].to_vec();
        assert_eq!(detect(&v), Err(SeriesError::Missing(1)));
        assert_eq!(detect(&[]), Err(SeriesError::Empty));
        // fewer photos than the shot count, all present numbers consecutive from 1
        assert_eq!(detect(&series(16)[..4]), Err(SeriesError::Missing(5)));
    }

    #[test]
    fn mixed_series_are_refused() {
        let mut v = series(16);
        v[7].exposure_time = 0.02;
        assert_eq!(detect(&v), Err(SeriesError::MixedExposure));
        let mut v = series(16);
        v[7].iso = 100;
        assert_eq!(detect(&v), Err(SeriesError::MixedExposure));
        let mut v = series(16);
        v[7].exposure_bias = 0.3;
        assert_eq!(detect(&v), Err(SeriesError::MixedExposure));
        let mut v = series(16);
        v[2].raw_size = (60, 60);
        assert_eq!(detect(&v), Err(SeriesError::MixedGeometry));
        let mut v = series(16);
        v[2].image_area = Some([4, 4, 52, 52]);
        assert_eq!(detect(&v), Err(SeriesError::MixedGeometry));
        let mut v = series(16);
        v[9].camera = "NIKON CORPORATION NIKON Z 9".into();
        assert_eq!(detect(&v), Err(SeriesError::MixedCamera));
        let mut v = series(16);
        v[9].shot_count = Some(8);
        assert_eq!(detect(&v), Err(SeriesError::MixedCount(16)));
    }

    #[test]
    fn counters_and_times_must_follow() {
        let mut v = series(16);
        v[10].counter = Some(5);
        assert_eq!(detect(&v), Err(SeriesError::CounterGap));
        let mut v = series(16);
        for h in v.iter_mut().skip(8) {
            h.time_ms = h.time_ms.map(|t| t + 60_000);
        }
        assert_eq!(detect(&v), Err(SeriesError::TimeGap));
        let mut v = series(16);
        v[4].time_ms = None;
        assert_eq!(detect(&v), Err(SeriesError::TimeGap));
        // an ordinary frame in the set
        let mut v = series(16);
        v[4].index = None;
        assert_eq!(detect(&v), Err(SeriesError::NoIndex(5)));
    }

    #[test]
    fn times_are_read_from_date_and_sub_second() {
        let a = time_ms("2030:01:31 23:59:59", "9").unwrap();
        let b = time_ms("2030:02:01 00:00:00", "15").unwrap();
        assert_eq!(b - a, 1000 - 900 + 150);
        assert_eq!(time_ms("garbage", "1"), None);
        // a date that is not one, from a hostile file, is no time (and no overflow)
        assert_eq!(time_ms("9223372036854775807:01:01 00:00:00", "1"), None);
        assert_eq!(time_ms("2030:01:01 99:00:00", "1"), None);
    }

    /// A synthetic NEF with the maker note of a pixel-shift frame (all values invented).
    fn synthetic(index: u8, count: u16, tags: bool) -> Vec<u8> {
        let mode = match count {
            4 => 1,
            8 => 2,
            16 => 3,
            _ => 4,
        };
        let slot = if count == 8 && index > 4 { index + 12 } else { index };
        synthetic_with(index, mode, slot, 16, tags)
    }

    /// The same with the mode, the slot and tag `0x0034` as given (the Z 8 writes 16 in `0x0034` in every mode).
    fn synthetic_with(index: u8, mode: u8, slot: u8, t34: u16, tags: bool) -> Vec<u8> {
        let mut note = IfdBuilder::new();
        if tags {
            note.set(tag::SHOT_COUNT, Value::Short(vec![t34]));
            let mut a = vec![0u8; 16];
            a[NUMBER_BYTE] = index;
            note.set(tag::FRAME_INDEX_A, Value::Undefined(a));
            let mut b = vec![0u8; 28];
            b[MODE_BYTE] = mode;
            b[SLOT_BYTE] = slot;
            note.set(tag::FRAME_INDEX_B, Value::Undefined(b));
        }
        note.set(tag::EXPOSURE_COUNTER, Value::Long(vec![700 + u32::from(index)]));
        let mut pos = vec![0u8; 20];
        pos[4..8].copy_from_slice(&(-250i32).to_le_bytes());
        pos[12..16].copy_from_slice(&7i32.to_le_bytes());
        note.set(tag::POSITION, Value::Undefined(pos));
        note.set(tag::IMAGE_AREA, Value::Short(vec![4, 4, 72, 52]));
        note.set(tag::SENSOR_REGION, Value::Short(vec![11, 80, 60, 80, 60, 0, 0]));
        let mut maker = b"Nikon\0\x02\0\0\0".to_vec();
        maker.extend(TiffWriter::new(ByteOrder::Little, false).write(&[note]).unwrap());
        let mut exif = IfdBuilder::new();
        exif.set(t::MAKER_NOTE, Value::Undefined(maker));
        exif.set(t::EXPOSURE_TIME, Value::Rational(vec![lcc_tiff::writer::rational(0.01)]));
        exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii("2030:05:06 07:08:09".into()));
        exif.set(0x9291, Value::Ascii("12".into()));
        exif.set(0xa434, Value::Ascii("TEST LENS".into()));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("NIKON CORPORATION".into()));
        ifd0.set(t::MODEL, Value::Ascii("NIKON Z 8".into()));
        let mut raw = IfdBuilder::new();
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![80]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![60]));
        ifd0.add_sub_ifd(raw);
        ifd0.set_child(t::EXIF_IFD, exif);
        TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn a_header_is_read_from_a_synthetic_maker_note() {
        let h = read_header(&synthetic(7, 16, true)).unwrap();
        assert_eq!((h.shot_count, h.index, h.counter), (Some(16), Some(7), Some(707)));
        assert_eq!(h.raw_size, (80, 60));
        assert_eq!(h.image_area, Some([4, 4, 72, 52]));
        assert_eq!(h.sensor_region, Some([11, 80, 60, 80, 60, 0, 0]));
        assert_eq!(h.position, Some((-250, 7)));
        assert_eq!(h.camera, "NIKON CORPORATION NIKON Z 8");
        assert_eq!(h.lens, "TEST LENS");
        assert!((h.exposure_time - 0.01).abs() < 1e-6);
        assert!(h.time_ms.is_some());
        assert_eq!(read_header(&synthetic(1, 16, false)), Err(HeaderError::NotPixelShift));
        assert_eq!(read_header(b"not a tiff"), Err(HeaderError::NotTiff));
    }

    /// The shot count is the mode byte (`0x005e` byte 4), whatever `0x0034` says (16 in every mode on the Z 8); a lone
    /// frame (mode 0) is no pixel-shift frame and an unknown mode is refused, not guessed.
    #[test]
    fn the_shot_count_is_the_mode_byte_and_not_tag_0x0034() {
        for (mode, shots) in [(1u8, 4u16), (2, 8), (3, 16), (4, 32)] {
            for t34 in [16u16, 4, 99, 0] {
                let h = read_header(&synthetic_with(1, mode, 1, t34, true)).unwrap();
                assert_eq!((h.shot_count, h.index), (Some(shots), Some(1)), "mode {mode}, 0x0034 = {t34}");
            }
        }
        assert_eq!(read_header(&synthetic_with(0, 0, 0, 16, true)), Err(HeaderError::NotPixelShift), "a lone frame");
        for mode in [5u8, 9, 255] {
            assert_eq!(read_header(&synthetic_with(1, mode, 1, 16, true)), Err(HeaderError::UnknownMode(mode)));
        }
    }

    /// The placement slot must be the one the mode gives the frame number: the number itself, except 17 to 20 for frames
    /// 5 to 8 of an 8-shot series. A frame that breaks the rule, or whose number is outside the series, has no number.
    #[test]
    fn the_slot_must_match_the_frame_number_of_the_mode() {
        let number = |mode: u8, n: u8, slot: u8| read_header(&synthetic_with(n, mode, slot, 16, true)).unwrap().index;
        for n in 1..=4u8 {
            assert_eq!(number(1, n, n), Some(u16::from(n)), "4 shots");
        }
        for n in 1..=8u8 {
            assert_eq!(number(2, n, if n > 4 { n + 12 } else { n }), Some(u16::from(n)), "8 shots, frame {n}");
        }
        for n in 1..=16u8 {
            assert_eq!(number(3, n, n), Some(u16::from(n)), "16 shots");
        }
        for n in 1..=32u8 {
            assert_eq!(number(4, n, n), Some(u16::from(n)), "32 shots");
        }
        // the slot of another mode's rule, a slot out of step, numbers outside the series
        assert_eq!(number(2, 5, 5), None, "8 shots, frame 5 in slot 5");
        assert_eq!(number(2, 2, 14), None, "8 shots, frame 2 in slot 14");
        assert_eq!(number(3, 3, 4), None, "16 shots, slot 4 for frame 3");
        assert_eq!(number(4, 20, 8), None, "32 shots, slot 8 for frame 20");
        assert_eq!(number(1, 1, 17), None, "4 shots, slot 17");
        assert_eq!(number(1, 5, 5), None, "4 shots, frame 5");
        assert_eq!(number(2, 9, 21), None, "8 shots, frame 9");
        assert_eq!(number(4, 33, 33), None, "32 shots, frame 33");
        assert_eq!(number(3, 0, 0), None, "frame 0");
    }

    /// Frames of different modes are not one series, and a series that can not be merged is told by its mode alone.
    #[test]
    fn the_mode_of_a_set_of_frames_is_judged_before_its_completeness() {
        let of = |shots: u16, n: u16| FrameHeader { shot_count: Some(shots), ..hdr(n, shots) };
        let first_half_of_32: Vec<FrameHeader> = (1..=16).map(|n| of(32, n)).collect();
        assert_eq!(series_shot_count(&first_half_of_32), Ok(32), "any frames of a 32-shot series say 32");
        assert_eq!(series_shot_count(&(1..=4).map(|n| of(8, n)).collect::<Vec<_>>()), Ok(8));
        assert_eq!(series_shot_count(&(1..=16).map(|n| of(16, n)).collect::<Vec<_>>()), Ok(16));
        let mixed = vec![of(4, 1), of(4, 2), of(16, 1)];
        assert_eq!(series_shot_count(&mixed), Err(SeriesError::MixedCount(3)));
        assert_eq!(series_shot_count(&[]), Err(SeriesError::Empty));
    }

    /// The series' times for an interval of `step` ms, with `jitter(i)` added to frame i.
    fn spaced(step: i64, jitter: impl Fn(i64) -> i64) -> Vec<FrameHeader> {
        (1..=16u16)
            .map(|i| {
                let mut h = hdr(i, 16);
                h.time_ms = Some(5_000 + step * i64::from(i) + jitter(i64::from(i)));
                h
            })
            .collect()
    }

    #[test]
    fn spacing_is_judged_against_the_series_own_interval() {
        // no interval set: about 0.12 s; and exactly 0 (same timestamp)
        assert!(detect(&spaced(120, |i| (i % 3) * 10)).is_ok());
        assert!(detect(&spaced(0, |_| 0)).is_ok());
        // user-set intervals of several seconds, with jitter of a few hundred ms
        assert!(detect(&spaced(3_000, |i| (i % 4 - 1) * 200)).is_ok());
        assert!(detect(&spaced(10_000, |i| (i % 5 - 2) * 400)).is_ok());
        // one interval far longer than the rest is not one series, at any speed
        let mut v = spaced(3_000, |_| 0);
        for h in v.iter_mut().skip(9) {
            h.time_ms = h.time_ms.map(|t| t + 20_000);
        }
        assert_eq!(detect(&v), Err(SeriesError::TimeGap));
        let mut v = spaced(120, |_| 0);
        for h in v.iter_mut().skip(9) {
            h.time_ms = h.time_ms.map(|t| t + 5_000);
        }
        assert_eq!(detect(&v), Err(SeriesError::TimeGap));
        // frames going back in time
        let mut v = spaced(3_000, |_| 0);
        v.swap(4, 5);
        assert!(detect(&v).is_ok(), "input order does not matter");
        let mut v = spaced(3_000, |_| 0);
        v[5].time_ms = Some(0);
        assert_eq!(detect(&v), Err(SeriesError::TimeGap));
    }

    #[test]
    fn the_whole_file_is_read_when_the_start_does_not_hold_the_tags() {
        use std::cell::Cell;
        let file = synthetic(3, 16, true);
        let calls = Cell::new(0);
        let full = || {
            calls.set(calls.get() + 1);
            Some(file.clone())
        };
        // the prefix is enough: no second read
        let h = read_header_with(&file, file.len() as u64, full).unwrap();
        assert_eq!(h.index, Some(3));
        assert_eq!(calls.get(), 0);
        // a prefix that ends before the maker note: the whole file is read
        let h = read_header_with(&file[..40], file.len() as u64, full).unwrap();
        assert_eq!((h.index, h.shot_count), (Some(3), Some(16)));
        assert_eq!(calls.get(), 1);
        // the whole file is an ordinary frame: refused, and no second read of the same bytes
        let plain = synthetic(1, 16, false);
        let again = || {
            calls.set(calls.get() + 1);
            Some(plain.clone())
        };
        assert_eq!(read_header_with(&plain, plain.len() as u64, again), Err(HeaderError::NotPixelShift));
        assert_eq!(calls.get(), 1);
        // the file can't be read in full: the first error stands
        assert!(read_header_with(&file[..40], file.len() as u64, || None).is_err());
    }
}
