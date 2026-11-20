//! Format detection from leading bytes.

#[cfg(test)]
use crate::exif::Tiff;
use serde::{Deserialize, Serialize};

/// Image container formats LightCraft recognises.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    Jpeg,
    Png,
    /// Ordinary (non-camera-raw) TIFF, incl. BigTIFF.
    Tiff,
    WebP,
    Avif,
    /// HEIC / HEIF (ISO-BMFF with HEVC).
    Heif,
    /// JPEG XL (bare codestream or ISO-BMFF container).
    Jxl,
    Gif,
    Bmp,
    /// Photoshop PSD / PSB (merged composite).
    Psd,
    /// TIFF-structured camera raw (DNG, CR2, NEF, ARW, PEF, ORF, RW2, SRW, 3FR, IIQ, ERF, …):
    /// route to `lightcraft-raw`.
    RawTiffLike,
    /// Non-TIFF camera raw containers (CR3, RAF, CRW, MRW, X3F): route to `lightcraft-raw`.
    RawOther,
}

impl Format {
    /// Canonical lowercase file extension.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::Png => "png",
            Format::Tiff => "tif",
            Format::WebP => "webp",
            Format::Avif => "avif",
            Format::Heif => "heic",
            Format::Jxl => "jxl",
            Format::Gif => "gif",
            Format::Bmp => "bmp",
            Format::Psd => "psd",
            Format::RawTiffLike => "dng",
            Format::RawOther => "raw",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Jpeg => "image/jpeg",
            Format::Png => "image/png",
            Format::Tiff => "image/tiff",
            Format::WebP => "image/webp",
            Format::Avif => "image/avif",
            Format::Heif => "image/heic",
            Format::Jxl => "image/jxl",
            Format::Gif => "image/gif",
            Format::Bmp => "image/bmp",
            Format::Psd => "image/vnd.adobe.photoshop",
            Format::RawTiffLike | Format::RawOther => "image/x-raw",
        }
    }

    /// Whether [`crate::decode`] can decode this format in this build.
    pub fn can_decode(self) -> bool {
        self.not_decodable().is_none()
    }

    /// Why [`crate::decode`] can't decode this format in this build (`None`: it can). The
    /// [`crate::Error::Unsupported`] a decode returns carries the same reason.
    pub fn not_decodable(self) -> Option<&'static str> {
        match self {
            #[cfg(not(feature = "jxl"))]
            Format::Jxl => Some("built without the `jxl` feature"),
            #[cfg(not(feature = "heif"))]
            Format::Heif => Some(crate::heif::NOT_IN_BUILD),
            #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
            Format::Avif => Some("AVIF decoding is not included in this build"),
            Format::RawTiffLike | Format::RawOther => Some("camera raw: decode with lightcraft-raw"),
            _ => None,
        }
    }

    /// Camera raw formats (decode with `lightcraft-raw`).
    pub fn is_raw(self) -> bool {
        matches!(self, Format::RawTiffLike | Format::RawOther)
    }
}

/// Detect the format from the first bytes of a file (a few KiB suffices; TIFF raw detection walks IFDs
/// when more of the file is available).
pub fn sniff(b: &[u8]) -> Option<Format> {
    if b.len() < 4 {
        return None;
    }
    if b.starts_with(b"8BPS") && b.len() >= 6 && matches!(u16::from_be_bytes([b[4], b[5]]), 1 | 2) {
        return Some(Format::Psd);
    }
    if b.starts_with(b"FUJIFILMCCD-RAW") || b.starts_with(b"\0MRM") || b.starts_with(b"FOVb") {
        return Some(Format::RawOther);
    }
    if b.len() >= 14 && &b[0..2] == b"II" && &b[6..14] == b"HEAPCCDR" {
        return Some(Format::RawOther);
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        // A JPEG comment can put these bytes at this offset. Check the leading JPEG signature first.
        return match craft_image_codecs::detect(b) {
            Some(craft_image_codecs::Format::Jpeg | craft_image_codecs::Format::JpegLs) => Some(Format::Jpeg),
            _ => sniff_bmff(b),
        };
    }
    if b.starts_with(b"IIRO") || b.starts_with(b"IIRS") || b.starts_with(b"MMOR") || b.starts_with(b"IIU\0") {
        // Olympus ORF / Panasonic RW2: TIFF-structured with a private magic.
        return Some(Format::RawTiffLike);
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        return Some(if craft_image_codecs::legacy_tiff_raw_hint(b) { Format::RawTiffLike } else { Format::Tiff });
    }
    if craft_image_codecs::bmp_probe::is_supported_header(b) {
        return Some(Format::Bmp);
    }
    match craft_image_codecs::detect(b) {
        Some(craft_image_codecs::Format::Jpeg | craft_image_codecs::Format::JpegLs) => Some(Format::Jpeg),
        Some(craft_image_codecs::Format::Png) => Some(Format::Png),
        Some(craft_image_codecs::Format::Gif) => Some(Format::Gif),
        Some(craft_image_codecs::Format::WebP) => Some(Format::WebP),
        Some(craft_image_codecs::Format::Jxl) => Some(Format::Jxl),
        Some(craft_image_codecs::Format::Tiff) => Some(Format::Tiff),
        _ => None,
    }
}

fn sniff_bmff(b: &[u8]) -> Option<Format> {
    let size = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let end = size.clamp(12, b.len().min(4096));
    let mut brands: Vec<&[u8]> = vec![&b[8..12]];
    let mut p = 16;
    while p + 4 <= end {
        brands.push(&b[p..p + 4]);
        p += 4;
    }
    let has = |x: &[u8]| brands.contains(&x);
    if has(b"crx ") {
        return Some(Format::RawOther);
    }
    if has(b"jxl ") {
        return Some(Format::Jxl);
    }
    if has(b"avif") || has(b"avis") {
        return Some(Format::Avif);
    }
    if [&b"heic"[..], b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1", b"msf1"].iter().any(|x| has(x)) {
        return Some(Format::Heif);
    }
    None
}

/// Heuristic camera-raw detection for classic TIFF: DNGVersion, the CR2 signature, CFA/LinearRaw
/// photometric or raw-only compressions in IFD0 or its SubIFDs, or a camera Make with 10–16-bit data
/// and no ordinary photometric.
#[cfg(test)]
fn tiff_is_raw(b: &[u8]) -> bool {
    if b.len() >= 10 && &b[8..10] == b"CR" {
        return true;
    }
    let Some(t) = Tiff::new(b) else { return false };
    let Some(ifd0) = t.first_ifd() else { return false };
    let sony = t.ifd(ifd0).is_some_and(|(entries, _)| {
        entries.iter().any(|e| e.tag == 0x010f && t.bytes(e).and_then(|v| v.get(..4)).is_some_and(|v| v.eq_ignore_ascii_case(b"SONY")))
    });
    let mut stack = vec![ifd0];
    let mut seen = 0;
    while let Some(pos) = stack.pop() {
        seen += 1;
        if seen > 16 {
            break;
        }
        let Some((entries, _)) = t.ifd(pos) else { continue };
        // Sony M/S lossless ARWs retain a dummy CFA pattern, but store linear YCbCr.
        // Requiring the raw pattern plus this layout keeps ordinary Sony TIFFs as TIFFs.
        let uint = |tag| entries.iter().find(|e| e.tag == tag).and_then(|e| t.uint(e));
        if sony && entries.iter().any(|e| e.tag == 0x828e) && uint(0x0103) == Some(7) && uint(0x0106) == Some(6) && uint(0x0115) == Some(3) {
            return true;
        }
        for e in &entries {
            match e.tag {
                0xC612 => return true, // DNGVersion
                0x0106 if matches!(t.uint(e), Some(32803 | 34892)) => return true,
                0x0103 if matches!(t.uint(e), Some(32767 | 32769 | 32770 | 34316 | 34713 | 65000 | 65535)) => return true,
                0x014A => {
                    // SubIFDs (LONG or IFD type): walk them.
                    let n = e.count.min(8) as usize;
                    if n == 1 {
                        if let Some(p) = t.uint(e) {
                            stack.push(p as usize);
                        }
                    } else if let Some(off) = t.u32_at(e.value_pos) {
                        for i in 0..n {
                            if let Some(p) = t.u32_at(off as usize + 4 * i) {
                                stack.push(p as usize);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_routing_retains_supported_dib_and_truncation_rules() {
        let mut b = [0_u8; 26];
        b[..2].copy_from_slice(b"BM");
        for dib in [12_u32, 16, 40, 52, 56, 64, 108, 124] {
            b[14..18].copy_from_slice(&dib.to_le_bytes());
            assert_eq!(sniff(&b), Some(Format::Bmp));
            assert_eq!(sniff(&b[..25]), None);
        }
        for dib in [0_u32, 13, 39, 125, u32::MAX] {
            b[14..18].copy_from_slice(&dib.to_le_bytes());
            assert_eq!(sniff(&b), None);
        }
    }

    #[test]
    fn raw_tiff_classifier_matches_legacy_on_mutations() {
        let fixtures: &[&[u8]] = &[
            b"II*\0\x08\0\0\0\x01\0\x12\xc6\x01\0\x04\0\0\0\x01\x04\0\0\0\0\0\0",
            b"II*\0\x08\0\0\0\x01\0\x03\x01\x03\0\x01\0\0\0\x09\x80\0\0\0\0\0\0",
            b"II*\0\x10\0\0\0CR\x02\0",
        ];
        for fixture in fixtures {
            for end in 0..=fixture.len() {
                let bytes = &fixture[..end];
                assert_eq!(craft_image_codecs::legacy_tiff_raw_hint(bytes), tiff_is_raw(bytes), "truncation {end}");
                for i in 0..bytes.len() {
                    for value in [0, 255] {
                        let mut changed = bytes.to_vec();
                        changed[i] = value;
                        assert_eq!(craft_image_codecs::legacy_tiff_raw_hint(&changed), tiff_is_raw(&changed), "byte {i} value {value} length {end}");
                    }
                }
            }
        }
    }

    #[test]
    fn magic_numbers() {
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0\0\x10JFIF"), Some(Format::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(Format::Png));
        assert_eq!(sniff(b"GIF89a......"), Some(Format::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::WebP));
        assert_eq!(sniff(b"\0\0\0\x1cftypavif\0\0\0\0avifmif1miaf"), Some(Format::Avif));
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"), Some(Format::Heif));
        assert_eq!(sniff(b"\0\0\0\x18ftypcrx \0\0\0\x01crx isom"), Some(Format::RawOther));
        assert_eq!(sniff(&[0xFF, 0x0A, 0xFA, 0x7F]), Some(Format::Jxl));
        assert_eq!(sniff(b"8BPS\0\x01\0\0\0\0\0\0"), Some(Format::Psd));
        assert_eq!(sniff(b"FUJIFILMCCD-RAW 0201"), Some(Format::RawOther));
        assert_eq!(sniff(b"II*\0\x08\0\0\0\0\0"), Some(Format::Tiff));
        assert_eq!(sniff(b"II*\0\x10\0\0\0CR\x02\0"), Some(Format::RawTiffLike));
        assert_eq!(sniff(b"IIRO\x08\0\0\0"), Some(Format::RawTiffLike));
        assert_eq!(sniff(b"nope"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn shared_detection_keeps_bigtiff_and_jpeg_ls_routing() {
        for header in [b"II+\0".as_slice(), b"MM\0+".as_slice()] {
            assert_eq!(sniff(header), Some(Format::Tiff));
        }
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xf7, 0, 2]), Some(Format::Jpeg));
    }

    #[test]
    fn jpeg_comments_do_not_override_the_format() {
        use crate::encode::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};

        let pixels = [32, 83, 170].repeat(12);
        let image = EncodeImage::new(4, 3, 3, Samples::U8(&pixels));
        let jpeg = encode_jpeg(&image, 85, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let header = crate::read_header(&jpeg).unwrap();
        assert_eq!((header.format, header.width, header.height), (Format::Jpeg, 4, 3));
        let expected = crate::decode(&jpeg, crate::DecodeOptions::default()).unwrap().to_srgb8();

        for apparent_brand in [b"avif", b"test"] {
            // The two-byte comment length and the start of its payload form the apparent ftyp.
            let mut comment = vec![0; usize::from(u16::from_be_bytes(*b"ft")) - 2];
            comment[..2].copy_from_slice(b"yp");
            comment[2..6].copy_from_slice(apparent_brand);

            let mut commented = jpeg[..2].to_vec();
            commented.extend_from_slice(b"\xff\xfeft");
            commented.extend_from_slice(&comment);
            commented.extend_from_slice(&jpeg[2..]);

            assert_eq!(sniff(&commented), Some(Format::Jpeg), "{apparent_brand:?}");
            assert_eq!(crate::read_header(&commented).unwrap(), header);
            assert_eq!(crate::decode(&commented, crate::DecodeOptions::default()).unwrap().to_srgb8().data, expected.data);
        }
    }

    #[test]
    fn dng_detected() {
        // II*, IFD at 8 with a single DNGVersion entry.
        let mut b = b"II*\0\x08\0\0\0".to_vec();
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&0xC612u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&[1, 4, 0, 0]);
        b.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(sniff(&b), Some(Format::RawTiffLike));
    }

    #[test]
    fn sony_linear_ycbcr_subifd_is_raw_but_ordinary_tiff_is_not() {
        fn fixture(compression: u32, pattern: bool) -> Vec<u8> {
            let mut b = b"II*\0\x08\0\0\0".to_vec();
            let entry = |b: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
                b.extend_from_slice(&tag.to_le_bytes());
                b.extend_from_slice(&typ.to_le_bytes());
                b.extend_from_slice(&count.to_le_bytes());
                b.extend_from_slice(&value.to_le_bytes());
            };
            b.extend_from_slice(&2u16.to_le_bytes());
            entry(&mut b, 0x010f, 2, 5, 38);
            entry(&mut b, 0x014a, 4, 1, 44);
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(b"SONY\0\0");
            b.extend_from_slice(&(if pattern { 4u16 } else { 3 }).to_le_bytes());
            entry(&mut b, 0x0103, 3, 1, compression);
            entry(&mut b, 0x0106, 3, 1, 6);
            entry(&mut b, 0x0115, 3, 1, 3);
            if pattern {
                entry(&mut b, 0x828e, 1, 4, u32::MAX);
            }
            b.extend_from_slice(&0u32.to_le_bytes());
            b
        }
        let mut raw = fixture(7, true);
        assert_eq!(sniff(&raw), Some(Format::RawTiffLike));
        assert_eq!(sniff(&fixture(7, false)), Some(Format::Tiff));
        assert_eq!(sniff(&fixture(1, true)), Some(Format::Tiff));
        assert_eq!(sniff(&raw[..38]), Some(Format::Tiff));
        raw[38..42].copy_from_slice(b"TEST");
        assert_eq!(sniff(&raw), Some(Format::Tiff));
    }

    fn ftyp_box(size: Option<u32>, major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let box_len = size.unwrap_or((16 + compatible.len() * 4) as u32);
        let mut b = Vec::with_capacity(16 + compatible.len() * 4);
        b.extend_from_slice(&box_len.to_be_bytes());
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(major);
        b.extend_from_slice(&0_u32.to_be_bytes());
        for brand in compatible {
            b.extend_from_slice(*brand);
        }
        b
    }

    #[test]
    fn bmff_probe_precedence_cr3_jxl_avif_heif() {
        // CR3 (`crx `) wins over JXL, AVIF, and HEIF regardless of brand position.
        assert_eq!(sniff(&ftyp_box(None, b"crx ", &[b"jxl ", b"avif", b"heic", b"mif1"])), Some(Format::RawOther));
        assert_eq!(sniff(&ftyp_box(None, b"mif1", &[b"heic", b"avif", b"jxl ", b"crx "])), Some(Format::RawOther));
        assert_eq!(sniff(&ftyp_box(None, b"avif", &[b"crx "])), Some(Format::RawOther));

        // JXL (`jxl `) wins over AVIF and HEIF regardless of brand position.
        assert_eq!(sniff(&ftyp_box(None, b"jxl ", &[b"avif", b"avis", b"heic", b"mif1"])), Some(Format::Jxl));
        assert_eq!(sniff(&ftyp_box(None, b"avif", &[b"jxl "])), Some(Format::Jxl));
        assert_eq!(sniff(&ftyp_box(None, b"mif1", &[b"jxl "])), Some(Format::Jxl));

        // AVIF (`avif` / `avis`) wins over all HEIF brands regardless of brand position.
        let heif_brands: [&[u8; 4]; 10] = [b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1", b"msf1"];
        for heif in heif_brands {
            assert_eq!(sniff(&ftyp_box(None, b"avif", &[heif])), Some(Format::Avif));
            assert_eq!(sniff(&ftyp_box(None, b"avis", &[heif])), Some(Format::Avif));
            assert_eq!(sniff(&ftyp_box(None, heif, &[b"avif"])), Some(Format::Avif));
            assert_eq!(sniff(&ftyp_box(None, heif, &[b"avis"])), Some(Format::Avif));

            // Without higher-precedence brands, every HEIF brand sniffs as Heif (major or compatible).
            assert_eq!(sniff(&ftyp_box(None, heif, &[b"miaf"])), Some(Format::Heif));
            assert_eq!(sniff(&ftyp_box(None, b"isom", &[heif])), Some(Format::Heif));
        }

        // Unrecognised BMFF brands return None rather than falling through.
        assert_eq!(sniff(&ftyp_box(None, b"isom", &[b"iso2", b"mp41"])), None);
    }

    #[test]
    fn bmff_ftyp_box_bounds_and_truncation() {
        // Box size stops compatible-brand scanning at the ftyp boundary.
        let bounded = ftyp_box(Some(16), b"heic", &[b"crx ", b"avif"]);
        assert_eq!(sniff(&bounded), Some(Format::Heif));

        // Box sizes smaller than 12 clamp to 12: major brand is still read, trailing bytes are not.
        for small in [0_u32, 8, 11] {
            let clamped = ftyp_box(Some(small), b"avif", &[b"crx "]);
            assert_eq!(sniff(&clamped), Some(Format::Avif));
        }

        // Declared box size larger than the buffer still scans available compatible brands.
        let truncated_with_compat = ftyp_box(Some(64), b"mif1", &[b"avif"]);
        assert_eq!(sniff(&truncated_with_compat), Some(Format::Avif));

        // Truncated before the 12-byte ftyp major brand returns None.
        let too_short = ftyp_box(None, b"heic", &[]);
        assert_eq!(sniff(&too_short[..11]), None);
    }

    #[test]
    fn jxl_and_detector_fallback_sniffing() {
        // JXL ISO-BMFF signature box (`JXL ` box, not `ftyp`) falls through to shared detector.
        let mut jxl_container = b"\x00\x00\x00\x0CJXL \r\n\x87\n".to_vec();
        assert_eq!(sniff(&jxl_container), Some(Format::Jxl));
        jxl_container.extend_from_slice(&ftyp_box(None, b"jxl ", &[b"jxl "]));
        assert_eq!(sniff(&jxl_container), Some(Format::Jxl));

        // Bare JXL codestream and standalone `ftypjxl ` box both sniff as Jxl.
        assert_eq!(sniff(&[0xFF, 0x0A, 0x00, 0x00]), Some(Format::Jxl));
        assert_eq!(sniff(&ftyp_box(None, b"jxl ", &[])), Some(Format::Jxl));

        // Shared detector fallback for standard raster containers and non-raw TIFF.
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0\0\x10JFIF"), Some(Format::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), Some(Format::Png));
        assert_eq!(sniff(b"GIF87a\x01\0\x01\0\0\0\0"), Some(Format::Gif));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Some(Format::WebP));
        assert_eq!(sniff(b"II*\0\x08\0\0\0\0\0"), Some(Format::Tiff));
        assert_eq!(sniff(b"MM\0*\0\0\0\x08\0\0"), Some(Format::Tiff));
        assert_eq!(sniff(b"unknown-header"), None);
    }
}
