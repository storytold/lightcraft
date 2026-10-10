//! P6.2 never-crash harnesses: print settings and templates (files, agents) and what a printer
//! answers over IPP (decode, HTTP framing, printer attributes) are untrusted.

use dac_layout::render::PhotoSource;
use dac_raster::Rgba8;

use crate::PrintSettings;
use crate::ipp::*;
use crate::job::builtin_templates;
use crate::output::{PdfOptions, needed_sizes, to_pdf};
use crate::text::ShapedText;

struct Tiny;

impl PhotoSource for Tiny {
    fn image(&self, _photo: &str, _long: usize) -> Result<Rgba8, String> {
        Ok(Rgba8::from_fn(6, 4, |x, y| [x as u8 * 40, y as u8 * 50, 90, 255]))
    }
}

#[test]
fn hostile_print_settings_never_panic() {
    let seeds: Vec<String> = builtin_templates().iter().filter_map(|(_, s)| s.to_json().ok()).collect();
    let refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    let photos: Vec<String> = (0..5).map(|i| i.to_string()).collect();
    let text = ShapedText::new();
    let mut n = 0usize;
    dac_fuzzkit::run_json("print.settings", &refs, 1500, |s| {
        let parsed = PrintSettings::from_json(s).ok().or_else(|| serde_json::from_str::<PrintSettings>(s).ok());
        let Some(mut st) = parsed else { return };
        let _ = st.validate();
        let _ = st.info_line();
        let Ok(doc) = st.build(&photos) else { return };
        let _ = needed_sizes(&doc, st.job.dpi);
        n += 1;
        // a PDF now and then, at a low resolution (the cost is the raster, not the parsing)
        if n.is_multiple_of(20) {
            st.job.dpi = 36.0;
            let _ = to_pdf(&doc, &st, &Tiny, &text, &PdfOptions::default(), &mut |_, _| true);
        }
    });
    assert!(n > 100, "only {n} mutants got as far as a layout");
}

fn printer_response() -> Vec<u8> {
    let col = Value::Collection(vec![
        (
            "media-size".into(),
            vec![Value::Collection(vec![("x-dimension".into(), vec![Value::Int(21000)]), ("y-dimension".into(), vec![Value::Int(29700)])])],
        ),
        ("media-source".into(), vec![Value::Text("auto".into())]),
    ]);
    let mut m = Message::request(0, 1, "ipp://x/");
    m.code = 0;
    m.groups.push(Group {
        tag: 0x04,
        attrs: vec![
            Attribute { tag: 0x42, name: "printer-name".into(), values: vec![Value::Text("PDF".into())] },
            Attribute { tag: 0x23, name: "printer-state".into(), values: vec![Value::Int(3)] },
            Attribute {
                tag: 0x49,
                name: "document-format-supported".into(),
                values: vec![Value::Text("application/pdf".into()), Value::Text("image/jpeg".into())],
            },
            Attribute {
                tag: 0x44,
                name: "media-supported".into(),
                values: vec![Value::Text("na_letter_8.5x11in".into()), Value::Text("iso_a4_210x297mm".into())],
            },
            Attribute { tag: 0x21, name: "media-left-margin-supported".into(), values: vec![Value::Int(635), Value::Int(0)] },
            Attribute { tag: 0x32, name: "printer-resolution-supported".into(), values: vec![Value::Resolution(600, 600, 3)] },
            Attribute { tag: 0x33, name: "copies-supported".into(), values: vec![Value::Range(1, 99)] },
            Attribute { tag: 0x34, name: "media-col-database".into(), values: vec![col] },
        ],
    });
    m.encode()
}

#[test]
fn hostile_ipp_answers_never_panic() {
    let body = printer_response();
    assert!(Message::decode(&body).is_ok());
    let mut http = format!("HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    http.extend_from_slice(&body);
    let chunked =
        [b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".as_slice(), format!("{:x}\r\n", body.len()).as_bytes(), &body, b"\r\n0\r\n\r\n"]
            .concat();
    dac_fuzzkit::run("print.ipp", &[&body, &http, &chunked], 5000, |b| {
        for msg in [Message::decode(b), http_body(b).and_then(|x| Message::decode(&x))] {
            if let Ok(m) = msg {
                let info = parse_printer_info(&m);
                let _ = info.accepts("application/pdf");
                for media in &info.media {
                    let _ = pwg_media_size(&media.name);
                }
                let _ = m.encode();
            }
        }
    });
}

#[test]
fn hostile_printer_uris_and_media_names_never_panic() {
    dac_fuzzkit::run_str(
        "print.uri",
        &["ipp://printer.local:631/printers/PDF", "ipps://[::1]/ipp/print", "na_letter_8.5x11in", "custom_min_1x1mm"],
        5000,
        |s| {
            let _ = IppUri::parse(s);
            let _ = pwg_media_size(s);
        },
    );
}
