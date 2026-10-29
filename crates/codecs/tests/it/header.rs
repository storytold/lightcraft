//! `read_header` (import probes) agrees with a decode on dimensions and orientation, refuses what a
//! decode refuses (truncated files included), and returns errors — never panics — on hostile input.

use lightcraft_codecs::exif::minimal_exif;
use lightcraft_codecs::*;
use lightcraft_raster::Rgba8;

fn pixels(w: usize, h: usize) -> Rgba8 {
    Rgba8::from_fn(w, h, |x, y| [(x * 37 % 256) as u8, (y * 53 % 256) as u8, ((x ^ y) * 9 % 256) as u8, (128 + x % 100) as u8])
}

fn meta(exif: &Option<Vec<u8>>) -> EncodeMeta<'_> {
    EncodeMeta { exif: exif.as_deref(), xmp: Some("<x/>"), ..Default::default() }
}

/// A TIFF with an Orientation tag (`encode_tiff` writes none), via the `tiff` crate's encoder.
fn tiff_oriented(img: &Rgba8, orientation: u16) -> Vec<u8> {
    use tiff::encoder::{TiffEncoder, colortype};
    let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut cur = std::io::Cursor::new(Vec::new());
    let mut enc = TiffEncoder::new(&mut cur).unwrap();
    let mut im = enc.new_image::<colortype::RGB8>(img.width as u32, img.height as u32).unwrap();
    im.encoder().write_tag(tiff::tags::Tag::Orientation, orientation).unwrap();
    im.write_data(&rgb).unwrap();
    cur.into_inner()
}

/// A minimal 8-bit RGB PSD (raw or PackBits-literal merged data) with an EXIF resource.
fn psd(w: usize, h: usize, orientation: Option<u16>, rle: bool) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"8BPS");
    b.extend_from_slice(&1u16.to_be_bytes());
    b.extend_from_slice(&[0; 6]);
    b.extend_from_slice(&3u16.to_be_bytes());
    b.extend_from_slice(&(h as u32).to_be_bytes());
    b.extend_from_slice(&(w as u32).to_be_bytes());
    b.extend_from_slice(&8u16.to_be_bytes());
    b.extend_from_slice(&3u16.to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    let mut res = Vec::new();
    if let Some(o) = orientation {
        let exif = minimal_exif(o);
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&1058u16.to_be_bytes());
        res.extend_from_slice(&[0, 0]); // empty pascal name, padded
        res.extend_from_slice(&(exif.len() as u32).to_be_bytes());
        res.extend_from_slice(&exif);
        if exif.len() % 2 == 1 {
            res.push(0);
        }
    }
    b.extend_from_slice(&(res.len() as u32).to_be_bytes());
    b.extend_from_slice(&res);
    b.extend_from_slice(&0u32.to_be_bytes()); // layer and mask info
    b.extend_from_slice(&(rle as u16).to_be_bytes());
    let plane = |c: usize| (0..w * h).map(move |i| ((i * (c + 3) * 11) % 256) as u8);
    if rle {
        // every row as literal runs of at most 128 bytes (PackBits' longest)
        let row_len = w + w.div_ceil(128);
        for _ in 0..3 * h {
            b.extend_from_slice(&(row_len as u16).to_be_bytes());
        }
        for c in 0..3 {
            let p: Vec<u8> = plane(c).collect();
            for row in p.chunks(w) {
                for run in row.chunks(128) {
                    b.push(run.len() as u8 - 1);
                    b.extend_from_slice(run);
                }
            }
        }
    } else {
        for c in 0..3 {
            b.extend(plane(c));
        }
    }
    b
}

/// Generated files of every format with a header reader, plus a fallback one (BMP via `image`).
fn fixtures(w: usize, h: usize) -> Vec<(String, Vec<u8>)> {
    let img = pixels(w, h);
    let e = EncodeImage::rgba8(&img);
    let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let rgb_e = EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&rgb));
    let u16s: Vec<u16> = (0..w * h * 3).map(|i| (i * 331) as u16).collect();
    let mut out = Vec::new();
    for o in [None, Some(1), Some(6), Some(8)] {
        let exif = o.map(minimal_exif);
        let tag = format!("o{}", o.unwrap_or(0));
        out.push((format!("jpeg 420 {tag}"), encode_jpeg(&rgb_e, 85, ChromaSubsampling::S420, &meta(&exif)).unwrap()));
        out.push((format!("jpeg 444 {tag}"), encode_jpeg(&rgb_e, 85, ChromaSubsampling::S444, &meta(&exif)).unwrap()));
        out.push((format!("png rgba {tag}"), encode_png(&e, &meta(&exif)).unwrap()));
        out.push((format!("png 16-bit {tag}"), encode_png(&EncodeImage::new(w as u32, h as u32, 3, Samples::U16(&u16s)), &meta(&exif)).unwrap()));
        out.push((format!("webp {tag}"), encode_webp_lossless(&e, &meta(&exif)).unwrap()));
        out.push((format!("psd {tag}"), psd(w, h, o, false)));
        out.push((format!("psd rle {tag}"), psd(w, h, o, true)));
        out.push((format!("tiff {tag}"), tiff_oriented(&img, o.unwrap_or(1))));
    }
    out.push(("tiff lzw".into(), encode_tiff(&e, TiffCompression::Lzw, &EncodeMeta::default()).unwrap()));
    out.push(("tiff packbits".into(), encode_tiff(&e, TiffCompression::PackBits, &EncodeMeta::default()).unwrap()));
    let mut bmp = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut bmp).encode(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8).unwrap();
    out.push(("bmp".into(), bmp));
    out
}

/// What the import probe learnt before it read headers: a small decode.
fn decoded(bytes: &[u8]) -> Result<Header> {
    let d = decode(bytes, DecodeOptions::fit(64, 64))?;
    Ok(Header { format: d.format, width: d.source_width, height: d.source_height, orientation: d.orientation })
}

#[test]
fn header_matches_decode() {
    for (w, h) in [(160, 96), (96, 160), (7, 5)] {
        for (name, bytes) in fixtures(w, h) {
            let header = read_header(&bytes).unwrap_or_else(|e| panic!("{name} {w}×{h}: {e}"));
            assert_eq!(header, decoded(&bytes).unwrap(), "{name} {w}×{h}");
            assert_eq!((header.width, header.height), (w as u32, h as u32), "{name}");
        }
    }
    let o = |name: &str| fixtures(16, 8).into_iter().find(|f| f.0 == name).map(|f| read_header(&f.1).unwrap().orientation).unwrap();
    assert_eq!((o("jpeg 420 o6"), o("png rgba o8"), o("webp o6"), o("psd o8"), o("tiff o6"), o("jpeg 444 o0")), (6, 8, 6, 8, 6, 1));
}

/// Whenever the decode the probe used to run accepts a file (here: every cut of every fixture),
/// the header agrees with it: reading headers never refuses a file that decodes. The reverse
/// is checked for truncation, which the header readers detect.
#[test]
fn truncated_files_agree_with_decode() {
    // 160 px wide: large JPEGs take the DCT-scaled (strict) decode path the probe took for photos
    for (name, bytes) in fixtures(160, 96) {
        let n = bytes.len();
        let mut cuts: Vec<usize> = (0..100).map(|i| n * i / 100).collect();
        cuts.extend(n.saturating_sub(64)..n);
        for cut in cuts {
            let part = &bytes[..cut];
            if let Ok(d) = decoded(part) {
                assert_eq!(read_header(part).ok(), Some(d), "{name} cut at {cut}/{n}");
            }
        }
        for cut in [n / 4, n / 2, n * 9 / 10] {
            assert!(read_header(&bytes[..cut]).is_err(), "{name}: cut at {cut}/{n} accepted");
        }
    }
}

#[test]
fn truncation_details() {
    let f = fixtures(160, 96);
    let get = |name: &str| f.iter().find(|x| x.0 == name).map(|x| x.1.clone()).unwrap();
    let jpeg = get("jpeg 420 o6");
    assert!(read_header(&jpeg[..jpeg.len() - 2]).is_err(), "a JPEG without its EOI is truncated");
    let mut trailing = jpeg.clone();
    trailing.extend_from_slice(&[0; 100]);
    assert_eq!(read_header(&trailing).unwrap().orientation, 6, "bytes after the EOI are fine");
    let png = get("png rgba o6");
    assert!(read_header(&png[..png.len() - 2]).is_ok(), "a damaged IEND is ignored, as by a decode");
    assert!(read_header(&png[..png.len() - 12]).is_err(), "nothing after the image data: truncated");
}

/// eXIf after the image data counts, as it does for a decode (which reads the whole file).
#[test]
fn png_exif_after_idat() {
    let img = pixels(9, 4);
    let plain = encode_png(&EncodeImage::rgba8(&img), &EncodeMeta::default()).unwrap();
    let exif = minimal_exif(6);
    let mut chunk = (exif.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(b"eXIf");
    chunk.extend_from_slice(&exif);
    chunk.extend_from_slice(&[0; 4]); // CRC: not checked by either reader
    let iend = plain.len() - 12;
    let mut b = plain[..iend].to_vec();
    b.extend_from_slice(&chunk);
    b.extend_from_slice(&plain[iend..]);
    assert_eq!(read_header(&b).unwrap().orientation, 6);
}

fn jpeg_segments(sof: u8, w: u16, h: u16, comps: u8) -> Vec<u8> {
    let mut b = vec![0xFF, 0xD8, 0xFF, sof, 0, 8 + 3 * comps as u16 as u8, 8];
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&w.to_be_bytes());
    b.push(comps);
    for c in 0..comps {
        b.extend_from_slice(&[c + 1, 0x11, 0]);
    }
    b.extend_from_slice(&[0xFF, 0xDA, 0, 8, 1, 1, 0, 0, 63, 0]);
    b.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]);
    b.extend_from_slice(&[0xFF, 0xD9]);
    b
}

fn png_ihdr(w: u32, h: u32, depth: u8, color: u8) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[depth, color, 0, 0, 0, 0, 0, 0, 0]);
    b
}

fn png_chunk(b: &mut Vec<u8>, kind: &[u8; 4], len: u32, data: &[u8]) {
    b.extend_from_slice(&len.to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(data);
    b.extend_from_slice(&[0; 4]);
}

#[test]
fn hostile_headers_are_errors() {
    // a well-formed synthetic JPEG header is read
    assert_eq!(read_header(&jpeg_segments(0xC0, 33, 21, 3)).unwrap(), Header { format: Format::Jpeg, width: 33, height: 21, orientation: 1 });
    let jpeg_bad = [
        jpeg_segments(0xC0, 0, 21, 3),        // zero width
        jpeg_segments(0xC0, 33, 0, 3),        // zero height (DNL)
        jpeg_segments(0xC0, 65535, 65535, 3), // 4.3 Gpx
        jpeg_segments(0xC9, 33, 21, 3),       // arithmetic coding
        jpeg_segments(0xC5, 33, 21, 3),       // hierarchical
        jpeg_segments(0xC0, 33, 21, 2),       // two components
        jpeg_segments(0xC0, 33, 21, 3)[..30].to_vec(),
        vec![0xFF, 0xD8, 0xFF, 0xE1, 0, 0],       // segment length 0
        vec![0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF], // segment past the end
        vec![0xFF, 0xD8, 0xFF, 0xD9],
    ];
    for (i, b) in jpeg_bad.iter().enumerate() {
        assert!(read_header(b).is_err(), "JPEG case {i}");
    }
    let mut no_sos = jpeg_segments(0xC0, 33, 21, 3);
    no_sos.truncate(2 + 2 + 17);
    no_sos.extend_from_slice(&[0xFF, 0xD9]);
    assert!(read_header(&no_sos).is_err(), "JPEG without a scan");

    let mut ok = png_ihdr(5, 3, 8, 2);
    png_chunk(&mut ok, b"IDAT", 1, &[0]);
    png_chunk(&mut ok, b"IEND", 0, &[]);
    assert_eq!(read_header(&ok).unwrap(), Header { format: Format::Png, width: 5, height: 3, orientation: 1 });
    let png_case = |ihdr: Vec<u8>, chunks: &[(&[u8; 4], u32, &[u8])]| {
        let mut b = ihdr;
        for (k, l, d) in chunks {
            png_chunk(&mut b, k, *l, d);
        }
        b
    };
    let idat: (&[u8; 4], u32, &[u8]) = (b"IDAT", 1, &[0]);
    let iend: (&[u8; 4], u32, &[u8]) = (b"IEND", 0, &[]);
    let png_bad = [
        png_case(png_ihdr(0, 3, 8, 2), &[idat, iend]),
        png_case(png_ihdr(5, 0, 8, 2), &[idat, iend]),
        png_case(png_ihdr(0x8000_0000, 1, 8, 2), &[idat, iend]),
        png_case(png_ihdr(65536, 65536, 8, 2), &[idat, iend]),
        png_case(png_ihdr(5, 3, 4, 2), &[idat, iend]), // RGB at 4 bits
        png_case(png_ihdr(5, 3, 8, 5), &[idat, iend]), // no colour type 5
        png_case(png_ihdr(5, 3, 8, 3), &[idat, iend]), // indexed without PLTE
        png_case(png_ihdr(5, 3, 8, 2), &[iend]),       // no IDAT
        png_case(png_ihdr(5, 3, 8, 2), &[idat]),       // nothing after the image data
        png_case(png_ihdr(5, 3, 8, 2), &[(b"IDAT", 0xFFFF_FFFF, &[0]), iend]),
        png_case(png_ihdr(5, 3, 8, 2), &[(b"tEXt", 0x7FFF_FFFF, &[0]), idat, iend]),
        png_ihdr(5, 3, 8, 2)[..20].to_vec(),
        b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDX\0\0\0\x05\0\0\0\x03\x08\x02\0\0\0".to_vec(),
    ];
    for (i, b) in png_bad.iter().enumerate() {
        assert!(read_header(b).is_err(), "PNG case {i}");
    }

    let tiff = encode_tiff(&EncodeImage::rgba8(&pixels(6, 4)), TiffCompression::None, &EncodeMeta::default()).unwrap();
    let ifd = u32::from_le_bytes([tiff[4], tiff[5], tiff[6], tiff[7]]) as usize;
    let mut tiff_bad = vec![b"II*\0\xff\xff\xff\x7f".to_vec(), b"II*\0\x08\0\0\0".to_vec(), b"II*\0\x04\0\0\0".to_vec()];
    // cut short after the IFD
    assert!(ifd < tiff.len() / 2, "the IFD comes before the image data");
    tiff_bad.push(tiff[..tiff.len() - 10].to_vec());
    for (i, b) in tiff_bad.iter().enumerate() {
        assert!(read_header(b).is_err(), "TIFF case {i}");
    }
    // a strip offset pointing past the end of the file
    let t = |b: &[u8], p: usize| u16::from_le_bytes([b[p], b[p + 1]]);
    let n = t(&tiff, ifd) as usize;
    let mut far = tiff.clone();
    for e in 0..n {
        let p = ifd + 2 + e * 12;
        if t(&far, p) == 273 && t(&far, p + 2) == 4 && u32::from_le_bytes([far[p + 4], far[p + 5], far[p + 6], far[p + 7]]) == 1 {
            far[p + 8..p + 12].copy_from_slice(&0x7FFF_0000u32.to_le_bytes());
        }
    }
    assert_ne!(far, tiff, "the strip offset was found");
    assert!(read_header(&far).is_err(), "strip past the end");

    let webp_bad = [
        b"RIFF\0\0\0\0WEBP".to_vec(),
        b"RIFF\0\0\0\0WEBPVP8L\xff\xff\xff\xff\x2f".to_vec(),
        b"RIFF\0\0\0\0WEBPXXXX\xff\xff\xff\xff".to_vec(),
        b"RIFF\0\0\0\0WEBPVP8 \x04\0\0\0\0\0\0\0".to_vec(),
    ];
    for (i, b) in webp_bad.iter().enumerate() {
        assert!(read_header(b).is_err(), "WebP case {i}");
    }

    let good = psd(4, 3, Some(6), true);
    assert_eq!(read_header(&good).unwrap().orientation, 6);
    let set = |pos: usize, v: &[u8]| {
        let mut b = good.clone();
        b[pos..pos + v.len()].copy_from_slice(v);
        b
    };
    let res_len = u32::from_be_bytes([good[30], good[31], good[32], good[33]]) as usize;
    let compression = 34 + res_len + 4;
    let mut psd_bad = vec![
        set(12, &0u16.to_be_bytes()),           // no channels
        set(12, &57u16.to_be_bytes()),          // too many
        set(12, &2u16.to_be_bytes()),           // too few for RGB
        set(14, &0x7FFF_FFFFu32.to_be_bytes()), // absurd height
        set(18, &0u32.to_be_bytes()),           // zero width
        set(22, &1u16.to_be_bytes()),           // 1-bit
        set(22, &7u16.to_be_bytes()),           // bad depth
        set(24, &9u16.to_be_bytes()),           // Lab
        set(24, &77u16.to_be_bytes()),          // unknown mode
        set(24, &2u16.to_be_bytes()),           // indexed without a palette
        set(compression, &2u16.to_be_bytes()),  // ZIP
        set(26, &0xFFFF_FFFFu32.to_be_bytes()), // colour mode data past the end
        set(30, &0xFFFF_FFF0u32.to_be_bytes()), // resources past the end
    ];
    // a 1 × 2^30 RLE image declared in a tiny file: refused without reading 2^30 row counts
    let mut tall = good.clone();
    tall[14..18].copy_from_slice(&(1u32 << 30).to_be_bytes());
    tall[18..22].copy_from_slice(&1u32.to_be_bytes());
    psd_bad.push(tall);
    psd_bad.push(good[..good.len() - 1].to_vec());
    for (i, b) in psd_bad.iter().enumerate() {
        assert!(read_header(b).is_err(), "PSD case {i}");
    }

    for b in [&b""[..], b"\xFF\xD8\xFF", b"\x89PNG\r\n\x1a\n", b"II*\0", b"MM\0*", b"8BPS\0\x01", b"GIF89a", b"not an image at all"] {
        assert!(read_header(b).is_err(), "{b:?}");
    }
}

/// The same with a few random bytes changed: whenever the decode accepts the damaged file, the
/// header agrees with it (deterministic seed).
#[test]
fn mutated_files_agree_with_decode() {
    let mut s: u64 = 0x1234_5678_9abc_def1;
    let mut rnd = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let (mut decodable, mut disagreements) = (0, Vec::new());
    for (name, bytes) in fixtures(160, 96) {
        for _ in 0..300 {
            let mut b = bytes.clone();
            for _ in 0..1 + rnd() % 3 {
                let i = rnd() as usize % b.len();
                b[i] = rnd() as u8;
            }
            if let Ok(d) = decoded(&b) {
                decodable += 1;
                let h = read_header(&b);
                if h.as_ref().ok() != Some(&d) {
                    disagreements.push(format!("{name}: decode {d:?}, header {h:?}"));
                }
            }
        }
    }
    assert!(decodable > 1000, "{decodable}");
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}
