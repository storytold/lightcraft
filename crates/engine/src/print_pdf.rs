//! Print-page policy and adaptation to shared PDF raster serialization.
//! Layout, rendering, profile choice and job limits stay in the app.
pub use craft_pdf_print::write_raster::print::{ImageData, OutputIntent, PdfImage, PdfPage, Placement};
#[cfg(test)]
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfInfo {
    pub title: Option<String>,
    pub output_intent: Option<OutputIntent>,
}

pub fn write(pages: &[PdfPage], info: &PdfInfo) -> Result<Vec<u8>, String> {
    craft_pdf_print::write_raster::print::write(
        pages,
        &craft_pdf_print::write_raster::print::PdfInfo {
            title: info.title.clone(),
            output_intent: info.output_intent.clone(),
            producer: "LightCraft".into(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_pdf_print::write_raster::print::MAX_PAGES;
    use lightcraft_codecs::NamedSpace;

    /// The body of object `id` (dictionary and raw stream bytes), found through the xref table.
    fn object(pdf: &[u8], id: usize) -> (String, Vec<u8>) {
        // byte offsets: the streams are binary, so search the bytes, not a lossy string
        let start = pdf.windows(10).rposition(|w| w == b"startxref\n").unwrap() + 10;
        let tail = std::str::from_utf8(&pdf[start..]).unwrap();
        let xref: usize = tail.lines().next().unwrap().trim().parse().unwrap();
        let table = std::str::from_utf8(&pdf[xref..]).unwrap();
        let line = table.lines().nth(2 + id).unwrap();
        let offset: usize = line[..10].parse().unwrap();
        let head = format!("{id} 0 obj\n");
        assert_eq!(&pdf[offset..offset + head.len()], head.as_bytes(), "xref entry for {id}");
        let rest = &pdf[offset + head.len()..];
        let end = rest.windows(7).position(|w| w == b"\nendobj").unwrap();
        let body = &rest[..end];
        match body.windows(8).position(|w| w == b"\nstream\n") {
            Some(p) => {
                let data = &body[p + 8..body.len() - "\nendstream".len()];
                (String::from_utf8_lossy(&body[..p]).into_owned(), data.to_vec())
            }
            None => (String::from_utf8_lossy(body).into_owned(), vec![]),
        }
    }

    fn inflate(data: &[u8]) -> Vec<u8> {
        miniz_oxide::inflate::decompress_to_vec_zlib(data).unwrap()
    }

    /// Object ids of every image XObject, in file order.
    fn images(pdf: &[u8]) -> Vec<usize> {
        let s = String::from_utf8_lossy(pdf);
        s.match_indices(" 0 obj\n<< /Type /XObject /Subtype /Image").map(|(i, _)| s[..i].rsplit('\n').next().unwrap().parse().unwrap()).collect()
    }

    fn reference(dict: &str, key: &str) -> usize {
        let at = dict.find(key).unwrap() + key.len();
        dict[at..].trim_start().split(' ').next().unwrap().parse().unwrap()
    }

    fn srgb() -> Arc<Vec<u8>> {
        Arc::new(lightcraft_codecs::icc::write_named(NamedSpace::Srgb))
    }

    fn page(images: Vec<(PdfImage, Placement)>) -> PdfPage {
        PdfPage { width: 612.0, height: 792.0, images }
    }

    fn full(w: f64, h: f64) -> Placement {
        Placement { x: 0.0, y: 0.0, width: w, height: h }
    }

    #[test]
    fn a_16_bit_icc_image_round_trips() {
        let samples: Vec<u16> = (0..2 * 3 * 3).map(|i| (i * 3851) as u16).collect();
        let img = PdfImage::from_u16(3, 2, 3, &samples, Some(srgb()));
        let pdf = write(&[page(vec![(img, full(612.0, 792.0))])], &PdfInfo::default()).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.6\n"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        let ids = images(&pdf);
        assert_eq!(ids.len(), 1);
        let (dict, data) = object(&pdf, ids[0]);
        assert!(dict.contains("/Width 3 /Height 2") && dict.contains("/BitsPerComponent 16") && dict.contains("/FlateDecode"), "{dict}");
        let back: Vec<u16> = inflate(&data).chunks(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
        assert_eq!(back, samples);
        let (profile, icc) = object(&pdf, reference(&dict, "/ICCBased"));
        assert!(profile.contains("/N 3") && profile.contains("/Alternate /DeviceRGB"), "{profile}");
        assert_eq!(inflate(&icc), *srgb());
        assert_eq!(lightcraft_codecs::icc::parse(&inflate(&icc)).and_then(|i| i.named), Some(NamedSpace::Srgb));
    }

    #[test]
    fn pages_sizes_placements_and_a_shared_profile() {
        let px = |v: u16| PdfImage::from_u16(1, 1, 3, &[v, v, v], Some(srgb()));
        let at = Placement { x: 36.0, y: 72.5, width: 288.0, height: 216.0 };
        let pages = [page(vec![(px(1), at), (px(2), full(612.0, 792.0))]), PdfPage { width: 841.89, height: 595.28, images: vec![(px(3), at)] }];
        let pdf = write(&pages, &PdfInfo::default()).unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("/Type /Pages /Count 2"), "two pages");
        assert!(s.contains("/MediaBox [0 0 612 792]") && s.contains("/MediaBox [0 0 841.89 595.28]"));
        // the sRGB profile is written once for all three images
        assert_eq!(s.matches("/N 3 /Alternate").count(), 1);
        let first: Vec<_> = images(&pdf).iter().map(|&id| reference(&object(&pdf, id).0, "/ICCBased")).collect();
        assert!(first.windows(2).all(|w| w[0] == w[1]), "{first:?}");
        // placements reach the content streams
        let contents: Vec<String> = s
            .match_indices("/Contents ")
            .map(|(i, _)| s[i + 10..].split(' ').next().unwrap().parse::<usize>().unwrap())
            .map(|id| String::from_utf8(inflate(&object(&pdf, id).1)).unwrap())
            .collect();
        assert_eq!(contents[0], "q 288 0 0 216 36 72.5 cm /Im0 Do Q\nq 612 0 0 792 0 0 cm /Im1 Do Q\n");
        assert_eq!(write(&pages, &PdfInfo::default()).unwrap(), pdf, "deterministic");
    }

    #[test]
    fn output_intent_and_title() {
        let cmyk_like = srgb();
        let info = PdfInfo {
            title: Some("Garden – prints (2)".into()),
            output_intent: Some(OutputIntent { icc: cmyk_like.clone(), channels: 3, condition: "Glossy (Photo) \\ test".into() }),
        };
        let img = PdfImage::from_u16(1, 1, 3, &[0, 0, 0], Some(cmyk_like));
        let pdf = write(&[page(vec![(img, full(10.0, 10.0))])], &info).unwrap();
        let (catalog, _) = object(&pdf, 1);
        assert!(catalog.contains("/OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX"), "{catalog}");
        assert!(catalog.contains("/Info (Glossy \\(Photo\\) \\\\ test)"), "{catalog}");
        // the intent and the image share one profile object
        let dest = reference(&catalog, "/DestOutputProfile");
        assert_eq!(reference(&object(&pdf, images(&pdf)[0]).0, "/ICCBased"), dest);
        let (info_dict, _) = object(&pdf, 3);
        assert!(info_dict.contains("/Title <FEFF"), "non-ASCII titles are UTF-16: {info_dict}");
        assert!(info_dict.contains("2013"), "the en dash: {info_dict}");
    }

    #[test]
    fn cmyk_grey_jpeg_and_device_images() {
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xD9];
        let imgs = vec![
            (
                PdfImage { width: 1, height: 1, channels: 4, data: ImageData::Raw { bits: 8, samples: vec![0, 10, 20, 30] }, icc: None },
                full(1.0, 1.0),
            ),
            (PdfImage { width: 2, height: 1, channels: 1, data: ImageData::Raw { bits: 8, samples: vec![0, 255] }, icc: None }, full(1.0, 1.0)),
            (PdfImage { width: 4, height: 4, channels: 3, data: ImageData::Jpeg(jpeg.clone()), icc: None }, full(1.0, 1.0)),
        ];
        let pdf = write(&[page(imgs)], &PdfInfo::default()).unwrap();
        let ids = images(&pdf);
        let dicts: Vec<_> = ids.iter().map(|&id| object(&pdf, id)).collect();
        assert!(dicts[0].0.contains("/ColorSpace /DeviceCMYK") && dicts[0].0.contains("/BitsPerComponent 8"));
        assert!(dicts[1].0.contains("/ColorSpace /DeviceGray"));
        assert!(dicts[2].0.contains("/DCTDecode") && dicts[2].0.contains("/DeviceRGB"));
        assert_eq!(dicts[2].1, jpeg, "JPEG streams are copied as they are");
    }

    #[test]
    fn compressing_early_writes_the_same_file() {
        let samples: Vec<u16> = (0..4 * 3 * 3).map(|i| (i * 977) as u16).collect();
        let raw = PdfImage::from_u16(4, 3, 3, &samples, Some(srgb()));
        let early = raw.clone().deflated().unwrap();
        assert!(matches!(early.data, ImageData::Flate { bits: 16, .. }));
        let at = full(100.0, 75.0);
        let a = write(&[page(vec![(raw, at)])], &PdfInfo::default()).unwrap();
        let b = write(&[page(vec![(early, at)])], &PdfInfo::default()).unwrap();
        assert_eq!(a, b);
        let mut short = PdfImage::from_u16(4, 3, 3, &samples, None);
        short.width = 5;
        assert!(short.deflated().is_err(), "checked before compressing");
    }

    #[test]
    fn hostile_documents_are_errors() {
        let ok = || PdfImage::from_u16(1, 1, 3, &[1, 2, 3], None);
        let at = full(10.0, 10.0);
        let bad = |pages: Vec<PdfPage>| write(&pages, &PdfInfo::default()).is_err();
        assert!(bad(vec![]));
        assert!(bad(vec![PdfPage { width: f64::NAN, height: 10.0, images: vec![] }]));
        assert!(bad(vec![PdfPage { width: -1.0, height: 10.0, images: vec![] }]));
        assert!(bad(vec![PdfPage { width: 1e9, height: 10.0, images: vec![] }]));
        assert!(bad(vec![page(vec![(ok(), Placement { x: f64::INFINITY, ..at })])]));
        assert!(bad(vec![page(vec![(ok(), Placement { width: 0.0, ..at })])]));
        let mut short = ok();
        short.data = ImageData::Raw { bits: 16, samples: vec![0; 5] };
        assert!(bad(vec![page(vec![(short, at)])]));
        let mut empty = ok();
        empty.width = 0;
        assert!(bad(vec![page(vec![(empty, at)])]));
        let mut odd = ok();
        odd.channels = 2;
        assert!(bad(vec![page(vec![(odd, at)])]));
        let mut not_jpeg = ok();
        not_jpeg.data = ImageData::Jpeg(vec![1, 2, 3]);
        assert!(bad(vec![page(vec![(not_jpeg, at)])]));
        let too_many = (0..MAX_PAGES + 1).map(|_| page(vec![])).collect();
        assert!(bad(too_many));
        let info = PdfInfo { title: None, output_intent: Some(OutputIntent { icc: srgb(), channels: 2, condition: String::new() }) };
        assert!(write(&[page(vec![])], &info).is_err());
    }
}
