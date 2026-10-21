//! Where the camera focused: the autofocus point a camera records in its maker notes, as a box on the
//! upright photo ([`FocusPoint`]). Sony only for now.
//!
//! **Sony** (the MakerNote IFD: header-less in ARWs, after a `SONY DSC` header in JPEGs). Tag identities and value names come from the public ExifTool
//! tag-name documentation (Sony table); every layout below was confirmed on real ILCE-1 (α1) and
//! ILCE-7SM3 (A7S III) ARWs, compared with ExifTool's output as a black box. No third-party code was read.
//!
//! | Tag | Layout (confirmed) | Use |
//! |---|---|---|
//! | `0x2027` FocusLocation | 4 × int16u: the width and height of the frame it is measured in, then the focus centre x, y in pixels of that frame (the sensor's landscape frame: a portrait shot still says 8640 × 5760) | the point |
//! | `0x2037` FocusFrameSize | 6 bytes: int16u width, int16u height of the frame the camera showed, then 2 bytes that are zero when there is no frame (manual focus) | the box size |
//! | `0x201b` FocusMode | int8u: 0 Manual, 2 AF-S, 3 AF-C, 4 AF-A, 6 DMF, 7 AF-D | manual = no point |
//! | `0x201c` AFAreaModeSetting | int8u: 0 Wide, 1 Center, 3 Flexible Spot, 11 Zone, 12 Expanded Flexible Spot, 13 Custom AF Area | shown |
//! | `0x2021` AFTracking | int8u: 0 Off, 1 Face tracking, 2 Lock On AF | shown |
//! | `0x9402` (enciphered block) byte 0x16, 0x17 | FocusMode (low 7 bits) and the AF area mode actually used: 0 Multi, 1 Center, 2 Spot, 3 Flexible Spot, 11 Zone, 12 Expanded Flexible Spot, 13 Custom AF Area, 14 Tracking, 15 Face Tracking, 20 Animal Eye Tracking, 21 Human Eye Tracking, 255 Manual | shown; read only when its focus mode agrees with `0x201b` |
//!
//! The `0x94xx` blocks are enciphered byte by byte (the documentation calls it "a simple substitution
//! cipher"). On the sample files each plain byte `p < 249` is stored as `p³ mod 249` and bytes 249–255
//! as themselves (e.g. a stored `0x20` reads as AFAreaMode 20, stored `0x77` as `0x83`, FocusMode AF-C
//! with its top bit set); the map is a bijection on 0..249 (cubing is invertible mod 3 and mod 83), so
//! it is undone by a table of its inverse.
//!
//! **No real point** (nothing is stored, so nothing is drawn and Z zooms as before):
//! - a body the layout hasn't been confirmed on ([`SONY_MODELS`]), a missing or malformed tag, a zero
//!   frame size, or a centre outside its frame;
//! - manual focus (`0x201b` = 0, or the block's area mode says Manual): the camera still records where
//!   the AF frame was parked, not where anything was in focus (on the samples these are off-centre);
//! - the exact frame centre while the area used was Multi (Wide with nothing tracked): on the samples
//!   the wide area reports the centre when it picked no particular spot. A centred Spot, Zone or
//!   Flexible Spot frame is a real choice and is kept.

pub use crate::focus_read::{HEADER_PREFIX, HeaderRead, MAX_HEADER_BYTES, read_header};
use crate::{Orientation, Rect};
use lightcraft_tiff::{ByteOrder, Ifd, Tiff, makernote, tags as t};
use serde::{Deserialize, Serialize};

/// Where the camera focused, on the upright photo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FocusPoint {
    /// The focus frame, normalized to the photo's full, EXIF-oriented frame (y down, 0..1 on both
    /// axes, clipped to it) — the frame face regions use, so it follows Rotate, flips and crops.
    pub rect: Rect,
    /// Focus mode as the camera names it (`AF-C`, `AF-S`, `DMF`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The AF area chosen on the camera (`Wide`, `Zone`, `Flexible Spot`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// What the camera followed, when it says (`Animal Eye Tracking`, `Face tracking`, `Lock On AF`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<String>,
}

impl FocusPoint {
    /// The box's centre (normalized, upright).
    pub fn center(&self) -> (f64, f64) {
        ((self.rect.x0 + self.rect.x1) / 2.0, (self.rect.y0 + self.rect.y1) / 2.0)
    }
    /// The words that describe it, in order (mode, area, tracking), for "AF-C · Wide · Animal Eye Tracking".
    pub fn words(&self) -> Vec<&str> {
        [&self.mode, &self.area, &self.tracking].into_iter().filter_map(|s| s.as_deref()).filter(|s| !s.is_empty()).collect()
    }
    /// The box clipped to the frame and checked: `None` when it is not a usable box (non-finite,
    /// empty, or entirely outside the photo).
    pub fn sanitized(mut self) -> Option<FocusPoint> {
        let r = self.rect;
        if ![r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) {
            return None;
        }
        let (x0, x1) = (r.x0.min(r.x1).clamp(0.0, 1.0), r.x0.max(r.x1).clamp(0.0, 1.0));
        let (y0, y1) = (r.y0.min(r.y1).clamp(0.0, 1.0), r.y0.max(r.y1).clamp(0.0, 1.0));
        if x1 - x0 <= 0.0 || y1 - y0 <= 0.0 {
            return None;
        }
        self.rect = Rect { x0, y0, x1, y1 };
        Some(self)
    }
}

/// Sony bodies whose focus tags have been checked against real files. Another body may store the same
/// tags differently (or not mean the same by them): it shows no focus point until its files are checked.
pub const SONY_MODELS: &[&str] = &["ILCE-1", "ILCE-7SM3"];

/// Whether a camera as the catalog names it (`"SONY ILCE-1"`: the Exif Make, a space, the Model) is
/// one whose files can carry a focus point, so a photo from any other camera is never opened to look.
pub fn camera_supported(camera: &str) -> bool {
    let camera = camera.trim();
    camera.get(..4).is_some_and(|m| m.eq_ignore_ascii_case("SONY")) && camera.rsplit(' ').next().is_some_and(|model| SONY_MODELS.contains(&model))
}

/// Names what [`read`] decodes (its bodies), so a record of "looked, found nothing" made by this
/// decoder is not trusted by one that knows more bodies.
pub fn decoder_tag() -> String {
    format!("sony-v1:{}", SONY_MODELS.join(","))
}

const FOCUS_MODE: u16 = 0x201b;
const AF_AREA_SETTING: u16 = 0x201c;
const AF_TRACKING: u16 = 0x2021;
const FOCUS_LOCATION: u16 = 0x2027;
const FOCUS_FRAME_SIZE: u16 = 0x2037;
const TAG_9402: u16 = 0x9402;

/// Side of the box drawn when the camera gave a point but no frame size, as a fraction of the frame's
/// short side (about the size of the α1's smallest frames).
const DEFAULT_SIDE: f64 = 0.03;

/// The focus point a file's maker note records, or `None` (see the module notes for when).
/// `data` is the TIFF stream the maker note's offsets refer to (a whole raw, or an Exif block).
pub fn read(data: &[u8], tiff: &Tiff, make: Option<&str>, model: Option<&str>, orientation: Option<Orientation>) -> Option<FocusPoint> {
    let make = make?;
    if !make.to_ascii_uppercase().starts_with("SONY") || !SONY_MODELS.contains(&model?.trim()) {
        return None;
    }
    let e = tiff.exif()?.get(t::MAKER_NOTE)?;
    let mn = makernote::parse_makernote(data, e.offset, e.count() as u64, tiff.order, make)?;
    // ARWs from these bodies carry a header-less IFD; their JPEGs the `SONY DSC` header
    if !matches!(mn.kind, makernote::MakerNoteKind::Sony | makernote::MakerNoteKind::Plain) {
        return None;
    }
    sony(&mn.ifd, mn.order, orientation.unwrap_or_default())
}

fn byte(ifd: &Ifd, tag: u16) -> Option<u8> {
    ifd.value(tag)?.get_u64(0).and_then(|v| u8::try_from(v).ok())
}

/// Decode the Sony tags of a parsed maker-note IFD.
pub(crate) fn sony(ifd: &Ifd, order: ByteOrder, orientation: Orientation) -> Option<FocusPoint> {
    let loc = ifd.value(FOCUS_LOCATION)?;
    let [w, h, x, y] = [0, 1, 2, 3].map(|i| loc.get_u64(i));
    let (w, h, x, y) = (w? as f64, h? as f64, x? as f64, y? as f64);
    if w < 1.0 || h < 1.0 || x > w || y > h {
        return None;
    }
    let mode = byte(ifd, FOCUS_MODE);
    if mode == Some(0) {
        return None;
    }
    // the enciphered block: its focus mode must agree with the plain one, else its layout is not the one we know
    let block = ifd.bytes(TAG_9402).and_then(|b| {
        let (m, area) = (decipher(*b.get(0x16)?) & 0x7f, decipher(*b.get(0x17)?));
        (Some(m) == mode).then_some(area)
    });
    if block == Some(255) {
        return None;
    }
    let setting = byte(ifd, AF_AREA_SETTING);
    let tracking = byte(ifd, AF_TRACKING);
    let wide_untracked = match block {
        Some(a) => a == 0,
        None => setting == Some(0) && tracking.is_none_or(|t| t == 0),
    };
    if wide_untracked && x * 2.0 == w && y * 2.0 == h {
        return None;
    }
    // frame size: two int16u in the note's byte order, then a non-zero marker when there is a frame
    let size = ifd.bytes(FOCUS_FRAME_SIZE).and_then(|b| {
        let (fw, fh) = (order.read_u16(b, 0)? as f64, order.read_u16(b, 2)? as f64);
        let shown = b.get(4..6).is_none_or(|m| m.iter().any(|&v| v != 0));
        (shown && fw >= 1.0 && fh >= 1.0).then_some((fw, fh))
    });
    let (fw, fh) = size.unwrap_or_else(|| {
        let side = w.min(h) * DEFAULT_SIDE;
        (side, side)
    });
    let sensor = Rect { x0: (x - fw / 2.0) / w, y0: (y - fh / 2.0) / h, x1: (x + fw / 2.0) / w, y1: (y + fh / 2.0) / h };
    let name = |v: Option<u8>, f: fn(u8) -> Option<&'static str>| v.and_then(f).map(str::to_string);
    let area = name(setting, area_setting_name);
    let used = name(block, area_used_name);
    // what it followed: the area mode used when it says more than the setting (Animal Eye Tracking on
    // Wide), else the tracking setting when it is on
    let tracking = used.filter(|u| Some(u) != area.as_ref() && u != "Multi").or_else(|| name(tracking, tracking_name).filter(|t| t != "Off"));
    FocusPoint { rect: orientation.map_norm_rect(sensor), mode: name(mode, focus_mode_name), area, tracking }.sanitized()
}

/// A minimal little-endian TIFF laid out as an α1 ARW's metadata is (IFD0 with Make `SONY` and Model `ILCE-1` →
/// Exif IFD → a header-less maker note with the focus tags: AF-C or the given FocusMode byte, Wide, Face tracking,
/// Animal Eye Tracking at (3226, 1980) of 8640 × 5760), followed by `samples` bytes standing in for sensor data. For
/// tests of the readers and the backfill in other crates; not a real raw (it has no image).
#[doc(hidden)]
pub fn synthetic_arw(mode: u8, samples: usize) -> Vec<u8> {
    fn entry(f: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]) {
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&typ.to_le_bytes());
        f.extend_from_slice(&count.to_le_bytes());
        f.extend_from_slice(&value);
    }
    let encipher = |p: u8| if p >= 249 { p } else { (u32::from(p).pow(3) % 249) as u8 };
    let mut f = b"II*\0".to_vec();
    f.extend_from_slice(&8u32.to_le_bytes());
    // IFD0 at 8: Make (at 62), Model (at 68), Orientation, the Exif IFD (at 76)
    f.extend_from_slice(&4u16.to_le_bytes());
    entry(&mut f, t::MAKE, 2, 5, 62u32.to_le_bytes());
    entry(&mut f, t::MODEL, 2, 7, 68u32.to_le_bytes());
    entry(&mut f, t::ORIENTATION, 3, 1, [1, 0, 0, 0]);
    entry(&mut f, t::EXIF_IFD, 4, 1, 76u32.to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    f.extend_from_slice(b"SONY\0\0ILCE-1\0\0");
    // the Exif IFD: the maker note at 94, 156 bytes
    f.extend_from_slice(&1u16.to_le_bytes());
    entry(&mut f, t::MAKER_NOTE, 7, 156, 94u32.to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    // the maker note's IFD, its values from 172
    f.extend_from_slice(&6u16.to_le_bytes());
    entry(&mut f, FOCUS_MODE, 1, 1, [mode, 0, 0, 0]);
    entry(&mut f, AF_AREA_SETTING, 1, 1, [0, 0, 0, 0]);
    entry(&mut f, AF_TRACKING, 1, 1, [1, 0, 0, 0]);
    entry(&mut f, FOCUS_LOCATION, 3, 4, 172u32.to_le_bytes());
    entry(&mut f, FOCUS_FRAME_SIZE, 7, 6, 180u32.to_le_bytes());
    entry(&mut f, TAG_9402, 7, 64, 186u32.to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    for v in [8640u16, 5760, 3226, 1980] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.extend_from_slice(&[0xaf, 0, 0xb4, 0, 1, 1]);
    let mut block = [0u8; 64];
    block[0x16] = encipher(0x80 | mode);
    block[0x17] = encipher(20);
    f.extend_from_slice(&block);
    f.extend((0..samples).map(|i| (i % 251) as u8));
    f
}

/// Undo the `0x94xx` byte substitution: the stored byte is `p³ mod 249` for `p < 249`.
fn decipher(b: u8) -> u8 {
    if b >= 249 {
        return b;
    }
    (0..249u32).find(|p| (p * p * p) % 249 == u32::from(b)).and_then(|p| u8::try_from(p).ok()).unwrap_or(b)
}

fn focus_mode_name(v: u8) -> Option<&'static str> {
    Some(match v {
        0 => "Manual",
        2 => "AF-S",
        3 => "AF-C",
        4 => "AF-A",
        6 => "DMF",
        7 => "AF-D",
        _ => return None,
    })
}

fn area_setting_name(v: u8) -> Option<&'static str> {
    Some(match v {
        0 => "Wide",
        1 | 9 => "Center",
        3 | 4 => "Flexible Spot",
        11 => "Zone",
        12 => "Expanded Flexible Spot",
        13 => "Custom AF Area",
        _ => return None,
    })
}

fn tracking_name(v: u8) -> Option<&'static str> {
    Some(match v {
        0 => "Off",
        1 => "Face tracking",
        2 => "Lock On AF",
        _ => return None,
    })
}

fn area_used_name(v: u8) -> Option<&'static str> {
    Some(match v {
        0 => "Multi",
        1 => "Center",
        2 => "Spot",
        3 => "Flexible Spot",
        11 => "Zone",
        12 => "Expanded Flexible Spot",
        13 => "Custom AF Area",
        14 => "Tracking",
        15 => "Face Tracking",
        20 => "Animal Eye Tracking",
        21 => "Human Eye Tracking",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{Entry, IfdBuilder, TiffWriter, Value};

    fn encipher(p: u8) -> u8 {
        if p >= 249 { p } else { ((u32::from(p).pow(3)) % 249) as u8 }
    }

    fn entry(tag: u16, value: Value) -> Entry {
        Entry { tag, value, offset: 0 }
    }

    /// The α1 sample `_DSC6954.ARW`: AF-C, Wide, Face tracking, Animal Eye Tracking, 8640×5760 at (3226, 1980), 175×180.
    fn alpha1() -> Ifd {
        let mut block = vec![0u8; 0x40];
        block[0x16] = encipher(0x83);
        block[0x17] = encipher(20);
        Ifd {
            entries: vec![
                entry(FOCUS_MODE, Value::Byte(vec![3])),
                entry(AF_AREA_SETTING, Value::Byte(vec![0])),
                entry(AF_TRACKING, Value::Byte(vec![1])),
                entry(FOCUS_LOCATION, Value::Short(vec![8640, 5760, 3226, 1980])),
                entry(FOCUS_FRAME_SIZE, Value::Undefined(vec![0xaf, 0, 0xb4, 0, 1, 1])),
                entry(TAG_9402, Value::Undefined(block)),
            ],
            ..Default::default()
        }
    }

    fn set(ifd: &mut Ifd, tag: u16, value: Value) {
        ifd.entries.retain(|e| e.tag != tag);
        ifd.entries.push(entry(tag, value));
        // IFD entries are kept sorted by tag (lookups rely on it)
        ifd.entries.sort_by_key(|e| e.tag);
    }

    #[test]
    fn the_cipher_matches_the_bytes_seen_in_files() {
        // stored → plain, as read from an α1 file's 0x9402 block next to ExifTool's decoded values
        assert_eq!(decipher(0x20), 20);
        assert_eq!(decipher(0x77), 0x83);
        assert_eq!(decipher(0xff), 0xff);
        for p in 0..=255u8 {
            assert_eq!(decipher(encipher(p)), p);
        }
    }

    #[test]
    fn reads_the_alpha1_sample() {
        let f = sony(&alpha1(), ByteOrder::Little, Orientation::Normal).unwrap();
        assert!((f.rect.x0 - (3226.0 - 87.5) / 8640.0).abs() < 1e-12);
        assert!((f.rect.y1 - (1980.0 + 90.0) / 5760.0).abs() < 1e-12);
        assert_eq!(f.words(), vec!["AF-C", "Wide", "Animal Eye Tracking"]);
        let (cx, cy) = f.center();
        assert!((cx - 3226.0 / 8640.0).abs() < 1e-12 && (cy - 1980.0 / 5760.0).abs() < 1e-12);
    }

    #[test]
    fn big_endian_frame_size() {
        let mut ifd = alpha1();
        set(&mut ifd, FOCUS_FRAME_SIZE, Value::Undefined(vec![0, 0xaf, 0, 0xb4, 1, 1]));
        let f = sony(&ifd, ByteOrder::Big, Orientation::Normal).unwrap();
        assert!(((f.rect.x1 - f.rect.x0) * 8640.0 - 175.0).abs() < 1e-9);
    }

    #[test]
    fn follows_the_exif_orientation() {
        // a portrait shot (Rotate 270 CW, EXIF 8) still records landscape sensor coordinates
        let mut ifd = alpha1();
        set(&mut ifd, FOCUS_LOCATION, Value::Short(vec![8640, 5760, 864, 576]));
        let f = sony(&ifd, ByteOrder::Little, Orientation::from_exif(8)).unwrap();
        let (cx, cy) = f.center();
        // left of the sensor (x 0.1) is the bottom of the upright photo; its top (y 0.1) the left
        assert!((cx - 0.1).abs() < 1e-9 && (cy - 0.9).abs() < 1e-9, "{cx} {cy}");
        let f = sony(&ifd, ByteOrder::Little, Orientation::from_exif(6)).unwrap();
        let (cx, cy) = f.center();
        assert!((cx - 0.9).abs() < 1e-9 && (cy - 0.1).abs() < 1e-9, "{cx} {cy}");
    }

    #[test]
    fn no_real_point() {
        // manual focus
        let mut ifd = alpha1();
        set(&mut ifd, FOCUS_MODE, Value::Byte(vec![0]));
        assert!(sony(&ifd, ByteOrder::Little, Orientation::Normal).is_none());
        // the block says Manual (its focus mode agreeing)
        let mut ifd = alpha1();
        let mut block = vec![0u8; 0x18];
        block[0x16] = encipher(3);
        block[0x17] = encipher(255);
        set(&mut ifd, TAG_9402, Value::Undefined(block));
        assert!(sony(&ifd, ByteOrder::Little, Orientation::Normal).is_none());
        // Multi at the exact centre
        let mut ifd = alpha1();
        let mut block = vec![0u8; 0x18];
        block[0x16] = encipher(3);
        block[0x17] = encipher(0);
        set(&mut ifd, TAG_9402, Value::Undefined(block));
        set(&mut ifd, FOCUS_LOCATION, Value::Short(vec![8640, 5760, 4320, 2880]));
        assert!(sony(&ifd, ByteOrder::Little, Orientation::Normal).is_none());
        // …and without the block: Wide, tracking off
        ifd.entries.retain(|e| e.tag != TAG_9402);
        set(&mut ifd, AF_TRACKING, Value::Byte(vec![0]));
        assert!(sony(&ifd, ByteOrder::Little, Orientation::Normal).is_none());
        // missing location, zero frame, centre outside it
        let mut ifd = alpha1();
        ifd.entries.retain(|e| e.tag != FOCUS_LOCATION);
        assert!(sony(&ifd, ByteOrder::Little, Orientation::Normal).is_none());
        for loc in [vec![0, 5760, 0, 0], vec![8640, 0, 1, 0], vec![8640, 5760, 9000, 10], vec![8640, 5760, 10, 6000], vec![8640, 5760, 10]] {
            let mut ifd = alpha1();
            set(&mut ifd, FOCUS_LOCATION, Value::Short(loc.clone()));
            assert!(sony(&ifd, ByteOrder::Little, Orientation::Normal).is_none(), "{loc:?}");
        }
    }

    #[test]
    fn a_centred_spot_is_a_real_point() {
        let mut ifd = alpha1();
        set(&mut ifd, AF_AREA_SETTING, Value::Byte(vec![3]));
        set(&mut ifd, AF_TRACKING, Value::Byte(vec![0]));
        let mut block = vec![0u8; 0x18];
        block[0x16] = encipher(2);
        block[0x17] = encipher(3);
        set(&mut ifd, TAG_9402, Value::Undefined(block));
        set(&mut ifd, FOCUS_MODE, Value::Byte(vec![2]));
        set(&mut ifd, FOCUS_LOCATION, Value::Short(vec![8640, 5760, 4320, 2880]));
        let f = sony(&ifd, ByteOrder::Little, Orientation::Normal).unwrap();
        assert_eq!(f.words(), vec!["AF-S", "Flexible Spot"]);
        let (cx, cy) = f.center();
        assert!((cx - 0.5).abs() < 1e-12 && (cy - 0.5).abs() < 1e-12);
    }

    #[test]
    fn a_block_that_disagrees_is_ignored() {
        // another body's block layout: its "focus mode" is not the plain tag's, so its area isn't trusted
        let mut ifd = alpha1();
        let mut block = vec![0u8; 0x18];
        block[0x16] = encipher(7);
        block[0x17] = encipher(255);
        set(&mut ifd, TAG_9402, Value::Undefined(block));
        let f = sony(&ifd, ByteOrder::Little, Orientation::Normal).unwrap();
        assert_eq!(f.words(), vec!["AF-C", "Wide", "Face tracking"]);
        // a block too short to hold the bytes is ignored the same way
        set(&mut ifd, TAG_9402, Value::Undefined(vec![1, 2, 3]));
        assert!(sony(&ifd, ByteOrder::Little, Orientation::Normal).is_some());
    }

    #[test]
    fn missing_or_garbage_frame_size_gets_a_small_box_and_edges_are_clipped() {
        for fs in [
            Value::Undefined(vec![0, 0, 0, 0, 0, 0]),
            Value::Undefined(vec![1]),
            Value::Short(vec![5]),
            Value::Undefined(vec![0xaf, 0, 0xb4, 0, 0, 0]),
        ] {
            let mut ifd = alpha1();
            set(&mut ifd, FOCUS_FRAME_SIZE, fs);
            let f = sony(&ifd, ByteOrder::Little, Orientation::Normal).unwrap();
            assert!(((f.rect.y1 - f.rect.y0) - DEFAULT_SIDE).abs() < 1e-9);
        }
        // a frame reaching past the corner is clipped to the photo
        let mut ifd = alpha1();
        set(&mut ifd, FOCUS_LOCATION, Value::Short(vec![8640, 5760, 0, 5760]));
        set(&mut ifd, FOCUS_FRAME_SIZE, Value::Undefined(vec![0xff, 0xff, 0xff, 0xff, 1, 1]));
        let f = sony(&ifd, ByteOrder::Little, Orientation::Normal).unwrap();
        assert_eq!(f.rect, Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 });
        let f = sony(&ifd, ByteOrder::Little, Orientation::Normal).unwrap();
        assert!(f.rect.x0 >= 0.0 && f.rect.y1 <= 1.0);
    }

    #[test]
    fn wrong_types_and_extreme_values_never_panic() {
        let values = [
            Value::Ascii("x".into()),
            Value::Byte(vec![]),
            Value::SShort(vec![-1, -1, -1, -1]),
            Value::Long(vec![u32::MAX; 4]),
            Value::Double(vec![f64::NAN; 4]),
            Value::Undefined(vec![0xff; 300]),
        ];
        for v in &values {
            for tag in [FOCUS_MODE, AF_AREA_SETTING, AF_TRACKING, FOCUS_LOCATION, FOCUS_FRAME_SIZE, TAG_9402] {
                let mut ifd = alpha1();
                set(&mut ifd, tag, v.clone());
                if let Some(f) = sony(&ifd, ByteOrder::Little, Orientation::Normal) {
                    assert!(f.rect.x0 >= 0.0 && f.rect.x1 <= 1.0 && f.rect.x0 < f.rect.x1);
                }
            }
        }
        assert!(FocusPoint { rect: Rect { x0: f64::NAN, y0: 0.0, x1: 1.0, y1: 1.0 }, mode: None, area: None, tracking: None }.sanitized().is_none());
        assert!(FocusPoint { rect: Rect { x0: 2.0, y0: 2.0, x1: 3.0, y1: 3.0 }, mode: None, area: None, tracking: None }.sanitized().is_none());
    }

    /// A whole TIFF with a `SONY DSC` maker note holding `ifd`'s entries, written for real.
    fn sony_file(make: &str, model: &str, orientation: u16, note_entries: &[(u16, Value)]) -> Vec<u8> {
        sony_file_with(make, model, orientation, note_entries, None)
    }

    /// [`sony_file`], with the `0x9402` block's value placed `far` bytes past the end of the TIFF
    /// (the gap filled with "sample data") when `far` is given.
    fn sony_file_with(make: &str, model: &str, orientation: u16, note_entries: &[(u16, Value)], far: Option<usize>) -> Vec<u8> {
        let order = ByteOrder::Little;
        // the note: 12-byte header, then an IFD whose out-of-line values are offset from the TIFF header,
        // which we only know after writing; write once to learn where the note lands, then rebuild
        let build = |note_at: u64, end: u64| -> (Vec<u8>, Vec<u8>) {
            let mut note = b"SONY DSC \0\0\0".to_vec();
            let ifd_len = 2 + note_entries.len() * 12 + 4;
            let mut tail: Vec<u8> = Vec::new();
            let mut outside: Vec<u8> = Vec::new();
            order.put_u16(&mut note, note_entries.len() as u16);
            for (tag, v) in note_entries {
                let raw = lightcraft_tiff::writer::encode(order, v);
                order.put_u16(&mut note, *tag);
                order.put_u16(&mut note, v.field_type() as u16);
                order.put_u32(&mut note, v.count() as u32);
                if raw.len() <= 4 {
                    let mut r = raw.clone();
                    r.resize(4, 0);
                    note.extend(r);
                } else if let (Some(gap), TAG_9402) = (far, *tag) {
                    order.put_u32(&mut note, (end + gap as u64) as u32);
                    outside = raw;
                } else {
                    order.put_u32(&mut note, (note_at + 12 + ifd_len as u64 + tail.len() as u64) as u32);
                    tail.extend(raw);
                    if tail.len() % 2 == 1 {
                        tail.push(0);
                    }
                }
            }
            order.put_u32(&mut note, 0);
            note.extend(tail);
            let mut exif = IfdBuilder::new();
            exif.set(t::MAKER_NOTE, Value::Undefined(note));
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii(make.into()));
            ifd0.set(t::MODEL, Value::Ascii(model.into()));
            ifd0.set(t::ORIENTATION, Value::Short(vec![orientation]));
            ifd0.set_child(t::EXIF_IFD, exif);
            (TiffWriter { order, bigtiff: false }.write(&[ifd0]).unwrap(), outside)
        };
        let (first, _) = build(0, 0);
        let at = Tiff::parse(&first).unwrap().exif().unwrap().get(t::MAKER_NOTE).unwrap().offset;
        let (mut file, outside) = build(at, first.len() as u64);
        assert_eq!(file.len(), first.len());
        assert_eq!(Tiff::parse(&file).unwrap().exif().unwrap().get(t::MAKER_NOTE).unwrap().offset, at);
        if let Some(gap) = far {
            file.extend((0..gap).map(|i| (i * 13 % 241) as u8));
            file.extend(outside);
        }
        file
    }

    fn alpha1_entries() -> Vec<(u16, Value)> {
        alpha1().entries.into_iter().map(|e| (e.tag, e.value)).collect()
    }

    #[test]
    fn read_exif_finds_it_through_the_maker_note_and_orientation() {
        let file = sony_file("SONY", "ILCE-1", 8, &alpha1_entries());
        let m = crate::read_exif(&file);
        let f = m.focus.expect("focus point");
        assert_eq!(f.words(), vec!["AF-C", "Wide", "Animal Eye Tracking"]);
        // EXIF 8: sensor x becomes 1 - upright y
        let (cx, cy) = f.center();
        assert!((cy - (1.0 - 3226.0 / 8640.0)).abs() < 1e-9 && (cx - 1980.0 / 5760.0).abs() < 1e-9, "{cx} {cy}");
        // an unconfirmed body or another maker: nothing
        assert!(crate::read_exif(&sony_file("SONY", "ILCE-7M4", 1, &alpha1_entries())).focus.is_none());
        assert!(crate::read_exif(&sony_file("Canon", "ILCE-1", 1, &alpha1_entries())).focus.is_none());
    }

    #[test]
    fn truncated_and_garbage_maker_notes_never_panic() {
        let file = sony_file("SONY", "ILCE-1", 1, &alpha1_entries());
        let note_at = Tiff::parse(&file).unwrap().exif().unwrap().get(t::MAKER_NOTE).unwrap().offset as usize;
        for cut in (0..file.len()).step_by(7) {
            let _ = crate::read_exif(&file[..cut]);
        }
        let mut seed = 0x2545_f491_u32;
        for _ in 0..300 {
            let mut f = file.clone();
            for _ in 0..8 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = note_at + (seed as usize % (file.len() - note_at));
                f[i] = (seed >> 8) as u8;
            }
            if let Some(fp) = crate::read_exif(&f).focus {
                assert!(fp.rect.x0 >= 0.0 && fp.rect.x1 <= 1.0 && fp.rect.y0 >= 0.0 && fp.rect.y1 <= 1.0);
            }
        }
    }

    /// A whole-file read and a header-only read of the same file agree, and the header read reads a
    /// small part of it (the sample data after the metadata is never touched).
    #[test]
    fn header_read_matches_the_whole_file_and_reads_little() {
        let read = |file: &[u8]| {
            let mut asked = 0u64;
            let r = read_header(file.len() as u64, &mut |off, buf: &mut [u8]| {
                asked += buf.len() as u64;
                let s = off as usize;
                buf.copy_from_slice(&file[s..s + buf.len()]);
                Ok(())
            })
            .unwrap();
            assert_eq!(r.bytes_read, asked);
            r
        };
        // a raw: the metadata, then 8 MB of "samples"
        let mut file = sony_file("SONY", "ILCE-1", 8, &alpha1_entries());
        let meta_len = file.len();
        file.extend((0..8_000_000u32).map(|i| (i * 31 % 251) as u8));
        let whole = crate::extract(&file).focus;
        assert!(whole.is_some());
        let r = read(&file);
        assert_eq!(r.focus, whole);
        assert!(r.bytes_read <= HEADER_PREFIX && r.bytes_read < file.len() as u64 / 100, "{} of {}", r.bytes_read, file.len());
        assert!(meta_len < HEADER_PREFIX as usize);
        // the area-mode block placed 3 MB further on: found in a further round, still a small part
        let mut far = sony_file_with("SONY", "ILCE-1", 1, &alpha1_entries(), Some(3_000_000));
        far.extend((0..4_000_000u32).map(|i| (i * 7 % 253) as u8));
        let whole = crate::extract(&far).focus;
        assert!(whole.is_some());
        let r = read(&far);
        assert_eq!(r.focus, whole);
        assert!(r.bytes_read < 64 * 1024, "{}", r.bytes_read);
        // a JPEG: the Exif segment after another segment
        let exif = sony_file("SONY", "ILCE-1", 6, &alpha1_entries());
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0, 16];
        jpeg.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        jpeg.extend_from_slice(&[0xff, 0xe1]);
        jpeg.extend_from_slice(&((exif.len() + 8) as u16).to_be_bytes());
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend_from_slice(&exif);
        jpeg.extend_from_slice(&[0xff, 0xda, 0, 2]);
        jpeg.extend(std::iter::repeat_n(0x55u8, 3_000_000));
        let whole = crate::extract(&jpeg).focus;
        assert!(whole.is_some());
        let r = read(&jpeg);
        assert_eq!(r.focus, whole);
        assert!(r.bytes_read <= HEADER_PREFIX);
        // unsupported bodies agree too (no point either way)
        let other = sony_file("SONY", "ILCE-7M4", 1, &alpha1_entries());
        assert_eq!(read(&other).focus, None);
    }

    /// Every truncation and corruption of a file gives the header read the whole-file read's answer
    /// (or an error when the file is shorter than it said), never a panic.
    #[test]
    fn header_read_of_damaged_files_agrees_or_fails_cleanly() {
        let file = sony_file("SONY", "ILCE-1", 1, &alpha1_entries());
        for cut in (0..file.len()).step_by(5) {
            let part = &file[..cut];
            let r = read_header(part.len() as u64, &mut |off, buf: &mut [u8]| {
                let s = off as usize;
                buf.copy_from_slice(part.get(s..s + buf.len()).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?);
                Ok(())
            })
            .unwrap();
            assert_eq!(r.focus, crate::extract(part).focus, "cut at {cut}");
        }
        let mut seed = 0x9e37_79b9_u32;
        for _ in 0..400 {
            let mut f = file.clone();
            for _ in 0..6 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = seed as usize % f.len();
                f[i] = (seed >> 8) as u8;
            }
            let r = read_header(f.len() as u64, &mut |off, buf: &mut [u8]| {
                let s = off as usize;
                buf.copy_from_slice(&f[s..s + buf.len()]);
                Ok(())
            })
            .unwrap();
            assert_eq!(r.focus, crate::extract(&f).focus);
        }
        // a reader told the file is longer than it is (replaced since it was measured): an error
        let r = read_header(file.len() as u64 + 100_000, &mut |off, buf: &mut [u8]| {
            let s = off as usize;
            buf.copy_from_slice(file.get(s..s + buf.len()).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?);
            Ok(())
        });
        assert!(r.is_err());
    }

    #[test]
    fn the_synthetic_arw_is_laid_out_as_described() {
        let f = synthetic_arw(3, 10);
        assert_eq!(f.len(), 260);
        let p = crate::extract(&f).focus.expect("a point");
        assert_eq!(p.words(), vec!["AF-C", "Wide", "Animal Eye Tracking"]);
        assert!(crate::extract(&synthetic_arw(0, 0)).focus.is_none(), "manual focus");
    }

    #[test]
    fn cameras_that_can_have_a_point() {
        assert!(camera_supported("SONY ILCE-1") && camera_supported("Sony ILCE-7SM3 "));
        for c in ["SONY ILCE-7M4", "Canon EOS R5", "", "SONY", "ILCE-1", "SON"] {
            assert!(!camera_supported(c), "{c}");
        }
    }

    /// Opt-in: decode real α1 / A7S III files and compare with the values ExifTool (run as a black box)
    /// reports for them. `LIGHTCRAFT_FOCUS_SAMPLES=<dir>` is the photo folder the paths below are relative
    /// to; skipped without it. No user file is part of the repository.
    #[test]
    fn real_sony_files_match_exiftool() {
        let Ok(dir) = std::env::var("LIGHTCRAFT_FOCUS_SAMPLES") else { return };
        let dir = std::path::Path::new(&dir);
        type Want = Option<([f64; 4], [f64; 2], &'static [&'static str])>;
        // (file, EXIF orientation, ExifTool's FocusLocation and FocusFrameSize, the words we show; None = no point)
        let cases: [(&str, u16, Want); 8] = [
            // α1, AF-C, Wide, Face tracking, AFAreaMode Animal Eye Tracking
            (
                "2023/11-november/17th/_DSC6954.ARW",
                1,
                Some(([8640.0, 5760.0, 3226.0, 1980.0], [175.0, 180.0], &["AF-C", "Wide", "Animal Eye Tracking"])),
            ),
            (
                "2023/11-november/17th/_DSC6894.ARW",
                1,
                Some(([8640.0, 5760.0, 4725.0, 1860.0], [135.0, 132.0], &["AF-C", "Wide", "Animal Eye Tracking"])),
            ),
            // α1, manual focus (the location 4441 3060 is where the frame was parked)
            ("2023/11-november/15th/_DSC6613.ARW", 1, None),
            // α1 portrait, AF-S, Wide, Multi at the exact centre 4320 2880
            ("2022/01-january/9th/_DSC6079.ARW", 8, None),
            // α1 APS-C crop mode portrait, AF-C, Wide, Lock On AF, AFAreaMode Tracking
            ("2022/12-december/16th/_DSC1575.ARW", 8, Some(([5616.0, 3744.0, 4071.0, 1848.0], [280.0, 280.0], &["AF-C", "Wide", "Tracking"]))),
            // A7S III portrait, AF-A, Wide, Face tracking, Animal Eye Tracking
            (
                "2020/10-october/31st/DSC00065.ARW",
                8,
                Some(([4240.0, 2832.0, 1192.0, 1675.0], [278.0, 277.0], &["AF-A", "Wide", "Animal Eye Tracking"])),
            ),
            // A7S III upside down, AF-C, a centred Flexible Spot: a real point
            ("2021/11-november/7th/DSC05975.ARW", 3, Some(([4240.0, 2832.0, 2120.0, 1416.0], [251.0, 224.0], &["AF-C", "Flexible Spot"]))),
            // A7S III, manual focus
            ("2021/06-june/12th/DSC06806.ARW", 1, None),
        ];
        for (name, exif_orientation, want) in cases {
            let bytes = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let got = crate::extract(&bytes).focus;
            match (want, got) {
                (None, None) => {}
                (Some((loc, size, words)), Some(f)) => {
                    let [w, h, x, y] = loc;
                    let sensor =
                        Rect { x0: (x - size[0] / 2.0) / w, y0: (y - size[1] / 2.0) / h, x1: (x + size[0] / 2.0) / w, y1: (y + size[1] / 2.0) / h };
                    let r = Orientation::from_exif(exif_orientation).map_norm_rect(sensor);
                    for (a, b) in [(f.rect.x0, r.x0), (f.rect.y0, r.y0), (f.rect.x1, r.x1), (f.rect.y1, r.y1)] {
                        assert!((a - b).abs() < 1e-9, "{name}: {:?} vs {r:?}", f.rect);
                    }
                    assert_eq!(f.words(), words, "{name}");
                }
                (w, g) => panic!("{name}: want {w:?}, got {g:?}"),
            }
        }
    }
}
