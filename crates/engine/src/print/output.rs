//! Writes composed pages as a print-ready PDF, or as JPEG files ([`write_jpegs`]): each page is converted to the job's colour target
//! (a printer / paper profile, or sRGB / Adobe RGB when the printer manages colour) with its
//! intent, black point compensation and print adjustment ([`crate::print_color`]), then embedded
//! covering the paper as an ICC-tagged, losslessly compressed 16-bit (or 8-bit) image. A printer
//! profile is also named as the document's output intent ([`crate::print_pdf`]).
use super::{MAX_JOB_PAGES, PageRaster, PrintJob};
use crate::originals::OriginalGuard;
use crate::print_color::PrintColorJob;
use crate::print_pdf::ImageData;
use crate::slideshow_export::{PartFile, file_name_prefix};
use lightcraft_color::transfer::encode_srgb8;
use lightcraft_color::{REC2020, SRGB};
use lightcraft_pipeline::OutputSpace;
use lightcraft_raster::Rgba8;
use std::io::Write as _;
use std::path::Path;

/// Largest PDF produced, bytes.
const MAX_PDF_BYTES: usize = 256 * 1024 * 1024;

/// A Rec. 2020 page as 8-bit sRGB (out-of-gamut colours are clipped).
pub fn page_to_srgb8(page: &PageRaster) -> Result<Rgba8, String> {
    let (w, h) = (page.width as usize, page.height as usize);
    let n = w.checked_mul(h).ok_or("page too large")?;
    if n == 0 || page.rgb.len() != n.checked_mul(3).ok_or("page too large")? {
        return Err("a page's pixel data does not match its size".into());
    }
    let trc = OutputSpace::Rec2020.trc();
    // 16-bit Rec. 2020 code → linear light, once per code value.
    let lut: Vec<f32> = (0..=u16::MAX).map(|v| trc.decode(f32::from(v) / 65535.0)).collect();
    let m = REC2020.to_space(&SRGB).to_f32();
    let mut out = Rgba8::new(w, h);
    for (dst, src) in out.data.iter_mut().zip(page.rgb.as_chunks::<3>().0) {
        let l = src.map(|v| lut.get(usize::from(v)).copied().unwrap_or(0.0));
        let row = |r: usize| m.get(r).map_or(0.0, |m| m[0] * l[0] + m[1] * l[1] + m[2] * l[2]);
        *dst = [encode_srgb8(row(0)), encode_srgb8(row(1)), encode_srgb8(row(2)), 255];
    }
    Ok(out)
}

/// The PDF for `pages` (all the same size): one page per raster, its MediaBox the paper size in
/// points (`pixels / ppi * 72`) with the image covering it. Each page is converted and compressed
/// in turn, so memory holds one page's device samples at a time.
pub fn write(pages: &[PageRaster], job: &PrintJob, title: &str) -> Result<Vec<u8>, String> {
    let first = pages.first().ok_or("nothing to print")?;
    if pages.len() > MAX_JOB_PAGES {
        return Err(format!("a print is limited to {MAX_JOB_PAGES} pages"));
    }
    if !(first.ppi.is_finite() && first.ppi > 0.0) {
        return Err("a page has no resolution".into());
    }
    let points = |px: u32| f64::from(px) / f64::from(first.ppi) * 72.0;
    let size = (points(first.width), points(first.height));
    let color = PrintColorJob::new(&job.color)?;
    let mut out = Vec::with_capacity(pages.len());
    let mut bytes = 0usize;
    for page in pages {
        if (page.width, page.height) != (first.width, first.height) {
            return Err("all pages of a PDF must be the same size".into());
        }
        let image = color.page_image(page.width, page.height, &page.rgb)?.deflated()?;
        if let ImageData::Flate { data, .. } = &image.data {
            bytes = bytes.saturating_add(data.len());
        }
        if bytes > MAX_PDF_BYTES {
            return Err("the PDF would exceed 256 MiB; lower the print resolution, use 8 bits or print fewer pages".into());
        }
        out.push((size, image));
    }
    color.pdf(out, title)
}

/// The name of a print's JPEG files (`<name>-001.jpg`…): `name`, else the folder's own name when it
/// can name files, else "Print". `name` follows the slideshow's rule for its JPEG files.
pub fn jpeg_name(name: Option<&str>, folder: &str) -> Result<String, String> {
    match name {
        Some(n) => file_name_prefix(n),
        None => Ok(Path::new(folder).file_name().and_then(|n| file_name_prefix(&n.to_string_lossy()).ok()).unwrap_or_else(|| "Print".into())),
    }
}

/// Writes `pages` as JPEG files `<name>-001.jpg`, `<name>-002.jpg`… in `folder` (#4675), each in
/// the job's colour target at its JPEG quality, with the profile and resolution embedded. Every
/// file is written as `.part` first and they take their names together once all are written: a
/// failure while writing, or `cancelled`, leaves no file behind, and a failure while renaming
/// removes the rest. `guard` keeps an original from being replaced.
/// → the files written and their total bytes.
pub fn write_jpegs(
    pages: &[PageRaster],
    job: &PrintJob,
    folder: &Path,
    name: &str,
    guard: &OriginalGuard,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<String>, usize), String> {
    if pages.is_empty() {
        return Err("nothing to print".into());
    }
    if pages.len() > MAX_JOB_PAGES {
        return Err(format!("a print is limited to {MAX_JOB_PAGES} pages"));
    }
    let color = PrintColorJob::new(&job.color)?;
    color.jpeg_ready()?;
    let digits = pages.len().to_string().len().max(3);
    let mut parts = Vec::with_capacity(pages.len());
    let mut bytes = 0usize;
    for (i, page) in pages.iter().enumerate() {
        if cancelled() {
            return Err("Print cancelled".into());
        }
        let jpg = color.jpeg(page, job.jpeg_quality)?;
        let target = folder.join(format!("{name}-{:0digits$}.jpg", i + 1));
        guard.check(&target)?;
        let (part, mut f) = PartFile::new(&target, guard)?;
        f.write_all(&jpg).map_err(|e| format!("{}: {e}", part.part.display()))?;
        f.sync_all().map_err(|e| format!("{}: {e}", part.part.display()))?;
        bytes = bytes.saturating_add(jpg.len());
        parts.push(part);
    }
    if cancelled() {
        return Err("Print cancelled".into());
    }
    let files = parts.iter().map(|p| p.target.to_string_lossy().into_owned()).collect();
    for part in parts {
        part.commit(guard)?;
    }
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::compose::rec2020_from_srgb8;

    fn raster(w: u32, h: u32, srgb: [u8; 3]) -> PageRaster {
        let c = rec2020_from_srgb8(srgb);
        PageRaster { width: w, height: h, ppi: 100.0, rgb: (0..w * h).flat_map(|_| c).collect() }
    }

    #[test]
    fn colours_survive_the_round_trip() {
        for c in [[255u8, 255, 255], [0, 0, 0], [128, 128, 128], [200, 30, 30], [10, 200, 90]] {
            let px = page_to_srgb8(&raster(2, 2, c)).unwrap().data[0];
            for k in 0..3 {
                assert!(px[k].abs_diff(c[k]) <= 1, "{c:?} -> {px:?}");
            }
        }
        assert!(page_to_srgb8(&PageRaster { width: 2, height: 2, ppi: 100.0, rgb: vec![0; 5] }).is_err());
        assert!(page_to_srgb8(&PageRaster { width: 0, height: 2, ppi: 100.0, rgb: vec![] }).is_err());
    }

    #[test]
    fn pdf_pages_have_the_paper_size() {
        let pages = [raster(425, 550, [255, 255, 255]), raster(425, 550, [0, 0, 255])];
        let bytes = write(&pages, &PrintJob::default(), "t").unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("%PDF-"));
        assert!(text.contains("/Count 2"));
        let at = text.find("/MediaBox [").unwrap() + "/MediaBox [".len();
        let nums: Vec<f64> = text[at..].split(']').next().unwrap().split_whitespace().map(|v| v.parse().unwrap()).collect();
        assert_eq!((nums[2], nums[3]), (306.0, 396.0), "4.25 x 5.5 in is 306 x 396 pt");
        assert!(text.contains("/Width 425 /Height 550"));
        // managed by the printer: sRGB, 16 bits, tagged, no output intent
        assert!(text.contains("/ICCBased") && text.contains("/BitsPerComponent 16") && !text.contains("/OutputIntents"));
        assert!(write(&[], &PrintJob::default(), "t").is_err());
        let mixed = [raster(10, 10, [0; 3]), raster(10, 12, [0; 3])];
        assert!(write(&mixed, &PrintJob::default(), "t").is_err());
        let bad = PageRaster { ppi: f32::NAN, ..raster(4, 4, [0; 3]) };
        assert!(write(&[bad], &PrintJob::default(), "t").is_err());
        let zero = PageRaster { ppi: 0.0, ..raster(4, 4, [0; 3]) };
        assert!(write(&[zero], &PrintJob::default(), "t").is_err());
    }

    #[test]
    fn a_printer_profile_is_converted_to_and_named_as_the_output_intent() {
        use crate::print_color::{PrintColorSettings, PrintIntent, PrintProfile};
        let dir = std::env::temp_dir().join(format!("lc-print-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let icc = dir.join("Lustre Paper.icc");
        std::fs::write(&icc, lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::AdobeRgb)).unwrap();
        let color =
            PrintColorSettings { profile: PrintProfile::Icc { path: icc }, intent: PrintIntent::Relative, sixteen_bit: false, ..Default::default() };
        let job = PrintJob { color, ..Default::default() };
        let bytes = write(&[raster(20, 30, [200, 30, 30])], &job, "Lustre").unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/OutputIntents") && text.contains("/DestOutputProfile"), "{}", &text[..text.len().min(500)]);
        assert!(text.contains("/BitsPerComponent 8") && text.contains("/Title (Lustre)"));
        // a profile that isn't one fails the print and says which file
        std::fs::write(dir.join("Junk.icc"), [7u8; 200]).unwrap();
        let junk = PrintJob {
            color: PrintColorSettings { profile: PrintProfile::Icc { path: dir.join("Junk.icc") }, ..Default::default() },
            ..Default::default()
        };
        let err = write(&[raster(2, 2, [0; 3])], &junk, "t").unwrap_err();
        assert!(err.contains("Junk"), "{err}");
    }

    /// The width and height in a JPEG's frame header, and its JFIF density in dots per inch.
    fn jpeg_size_and_ppi(jpg: &[u8]) -> ((u16, u16), u16) {
        let be = |i: usize| u16::from_be_bytes([jpg[i], jpg[i + 1]]);
        let sof = jpg.windows(2).position(|w| w == [0xFF, 0xC0]).expect("a baseline frame header");
        let jfif = jpg.windows(5).position(|w| w == b"JFIF\0").expect("a JFIF segment");
        assert_eq!(jpg[jfif + 7], 1, "density in dots per inch");
        ((be(sof + 7), be(sof + 5)), be(jfif + 8))
    }

    /// Print to JPEG files (#4675): one file per page, `<name>-001.jpg`…, at the page's pixels and
    /// resolution, with the colour target's profile; nothing is left behind when it's cancelled.
    #[test]
    fn pages_become_numbered_jpeg_files() {
        let dir = std::env::temp_dir().join(format!("lc-print-jpeg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pages = [raster(425, 550, [255, 255, 255]), raster(425, 550, [200, 30, 30])];
        let job = PrintJob { jpeg_quality: 80, ..PrintJob::default() };
        let guard = OriginalGuard::default();
        let (files, bytes) = write_jpegs(&pages, &job, &dir, "Proofs", &guard, &|| false).unwrap();
        assert_eq!(files, [dir.join("Proofs-001.jpg"), dir.join("Proofs-002.jpg")].map(|p| p.to_string_lossy().into_owned()));
        let mut total = 0;
        for file in &files {
            let jpg = std::fs::read(file).unwrap();
            total += jpg.len();
            assert!(jpg.starts_with(&[0xFF, 0xD8]), "{file}");
            assert_eq!(jpeg_size_and_ppi(&jpg), ((425, 550), 100), "{file}");
            assert!(jpg.windows(12).any(|w| w == b"ICC_PROFILE\0"), "{file}: the sRGB profile is embedded");
        }
        assert_eq!(bytes, total);
        assert!(std::fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part")));
        // cancelled after the first page: no file and no part file stays
        let again = dir.join("again");
        let calls = std::cell::Cell::new(0);
        let cancel = || {
            calls.set(calls.get() + 1);
            calls.get() > 1
        };
        assert!(write_jpegs(&pages, &job, &again, "Proofs", &guard, &cancel).is_err());
        assert_eq!(std::fs::read_dir(&again).map(|d| d.count()).unwrap_or(0), 0, "nothing left behind");
        assert!(write_jpegs(&[], &job, &again, "Proofs", &guard, &|| false).is_err());
        // a name is a plain file name: no path, drive or reserved character; a folder whose name
        // can't name files gives "Print"
        for bad in ["a/b", "a\\b", "D:lab", "", "x\u{0}y"] {
            assert!(jpeg_name(Some(bad), "/tmp/out").is_err(), "{bad:?}");
        }
        assert_eq!(jpeg_name(Some(" Lab order "), "/tmp/out").unwrap(), "Lab order");
        // a prefix only starts file names: ".." gives "..-001.jpg" in the folder, not its parent
        assert_eq!(jpeg_name(Some(".."), "/tmp/out").unwrap(), "..");
        assert_eq!(jpeg_name(None, "/tmp/Proofs").unwrap(), "Proofs");
        assert_eq!(jpeg_name(None, "/tmp/a:b").unwrap(), "Print");
        assert_eq!(jpeg_name(None, "/").unwrap(), "Print");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The staging name `<file>.part` keeps main's rule (#3673): a leftover there, or a link
    /// planted there, is removed and a new file made, and what a link points to is left alone. A
    /// catalogued photo whose file is that entry refuses the print instead (#4675).
    #[test]
    fn a_staging_name_never_removes_an_original() {
        let dir = std::env::temp_dir().join(format!("lc-print-jpeg-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pages = [raster(40, 50, [255, 255, 255])];
        let job = PrintJob::default();
        // a leftover from an earlier print is replaced
        std::fs::write(dir.join("Proofs-001.jpg.part"), b"leftover").unwrap();
        let (files, _) = write_jpegs(&pages, &job, &dir, "Proofs", &OriginalGuard::default(), &|| false).unwrap();
        assert!(std::fs::read(&files[0]).unwrap().starts_with(&[0xFF, 0xD8]));
        assert!(!dir.join("Proofs-001.jpg.part").exists());
        // the file of a catalogued photo is never removed: the print is refused
        std::fs::write(dir.join("Proofs-001.jpg.part"), b"a photo").unwrap();
        let mut guard = OriginalGuard::default();
        guard.add(&dir.join("Proofs-001.jpg.part"), "Proofs-001.jpg.part");
        let err = write_jpegs(&pages, &job, &dir, "Proofs", &guard, &|| false).unwrap_err();
        assert!(err.contains("never writes over an original"), "{err}");
        assert_eq!(std::fs::read(dir.join("Proofs-001.jpg.part")).unwrap(), b"a photo");
        #[cfg(unix)]
        {
            // nor when the catalogue reaches it through a link with another name
            std::os::unix::fs::symlink(dir.join("Proofs-001.jpg.part"), dir.join("IMG_1.jpg")).unwrap();
            let mut guard = OriginalGuard::default();
            guard.add(&dir.join("IMG_1.jpg"), "IMG_1.jpg");
            let err = write_jpegs(&pages, &job, &dir, "Proofs", &guard, &|| false).unwrap_err();
            assert!(err.contains("never writes over an original"), "{err}");
            assert_eq!(std::fs::read(dir.join("Proofs-001.jpg.part")).unwrap(), b"a photo");
            // a link planted at the staging name is removed; the photo it points to is untouched
            std::fs::remove_file(dir.join("IMG_1.jpg")).unwrap();
            std::fs::rename(dir.join("Proofs-001.jpg.part"), dir.join("IMG_1.jpg")).unwrap();
            std::os::unix::fs::symlink(dir.join("IMG_1.jpg"), dir.join("Proofs-001.jpg.part")).unwrap();
            write_jpegs(&pages, &job, &dir, "Proofs", &guard, &|| false).unwrap();
            assert_eq!(std::fs::read(dir.join("IMG_1.jpg")).unwrap(), b"a photo");
            assert!(std::fs::symlink_metadata(dir.join("Proofs-001.jpg.part")).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
