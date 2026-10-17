//! `ipps://` (IPP over TLS) and the printer's own options, against the virtual printer: a TLS
//! front (rustls, a test-only certificate) relays to the plain virtual IPP printer.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::PrintError;
use crate::ipp::{self, DriverOptions};
use crate::testprinter::{TestPrinter, TestPrinterConfig};
use crate::text::ShapedText;

/// A test CA's `localhost` / 127.0.0.1 leaf and its key (P-256, valid until 2126), generated
/// with openssl for tests only (the same pair `lcc-net`'s TLS tests use).
const LEAF_B64: &str = "MIIBqTCCAU6gAwIBAgIUHPx6OQjNVD01xDDk464Uj1xwmzcwCgYIKoZIzj0EAwIwEjEQMA4GA1UEAwwHVGVzdCBDQTAgFw0yNjEwMTAwMDA1NDlaGA8yMTI2MDkxNjAwMDU0OVowFDESMBAGA1UEAwwJbG9jYWxob3N0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEFPSyEHSnzWNFWHjo6Fsccp8IUISLOIv2JcW4dwim6VlrvmcgH4fSRtMLq9JDbrs7eGp5J/7qj5VL3iqt4kGTI6N+MHwwGgYDVR0RBBMwEYIJbG9jYWxob3N0hwR/AAABMAkGA1UdEwQCMAAwEwYDVR0lBAwwCgYIKwYBBQUHAwEwHQYDVR0OBBYEFIkl10ciEZxIBMwplQXH2M/FHL6EMB8GA1UdIwQYMBaAFKqM68Y6JJb+RsT9hW35DdCWbtsTMAoGCCqGSM49BAMCA0kAMEYCIQCGcYFLw3UXrn8ea/Hfn4plLzgTQLoQzYhaA86WzbiexQIhAM/ozbpH7BFWxXjrbJfUwlFZ5A6k670RBOEwjjudfnO4";
const KEY_B64: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgP7on22FsY1CzXydImw0X41brmtJ52CZx4n9+5T6HDV+hRANCAAQU9LIQdKfNY0VYeOjoWxxynwhQhIs4i/Ylxbh3CKbpWWu+ZyAfh9JG0wur0kNuuzt4ankn/uqPlUveKq3iQZMj";

fn b64(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = T.iter().position(|t| *t == c).unwrap() as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// The leaf certificate's fingerprint as `lcc-net` writes it.
fn leaf_fingerprint() -> String {
    lcc_net::Fingerprint::of(&b64(LEAF_B64)).to_string()
}

/// A TLS front for `n` connections that relays each request to `plain` (the virtual printer's
/// `host:port`) and the answer back.
fn tls_front(plain: String, n: usize) -> (u16, JoinHandle<()>) {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(b64(LEAF_B64))], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(b64(KEY_B64))))
        .unwrap();
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        for _ in 0..n {
            let Ok((s, _)) = listener.accept() else { return };
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let conn = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, s);
            // the request: head, then Content-Length bytes
            let mut got = Vec::new();
            let mut buf = [0u8; 16 * 1024];
            let want = loop {
                let Ok(k) = tls.read(&mut buf) else { break None };
                if k == 0 {
                    break None;
                }
                got.extend_from_slice(&buf[..k]);
                if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&got[..end]).to_lowercase();
                    let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).map(|v| v.trim().parse().unwrap()).unwrap_or(0);
                    break Some(end + 4 + len);
                }
            };
            let Some(want) = want else { continue };
            while got.len() < want {
                let Ok(k) = tls.read(&mut buf) else { break };
                if k == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..k]);
            }
            let mut p = TcpStream::connect(&plain).unwrap();
            p.write_all(&got).unwrap();
            let mut resp = Vec::new();
            p.read_to_end(&mut resp).unwrap();
            let _ = tls.write_all(&resp);
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
    });
    (port, h)
}

fn plain_hostport(p: &TestPrinter) -> String {
    p.uri().trim_start_matches("ipp://").split('/').next().unwrap().to_string()
}

/// One test, so the process-wide trust settings it changes are not raced by another.
#[test]
fn ipps_untrusted_then_trusted_by_fingerprint_and_on_first_use() {
    let printer = TestPrinter::start(TestPrinterConfig::default()).unwrap();
    let (port, h) = tls_front(plain_hostport(&printer), 5);
    let uri = format!("ipps://localhost:{port}/printers/Test");
    ipp::set_trust_on_first_use(false);

    // an unknown, self-made certificate: refused, with the fingerprint to check and what to do
    let e = ipp::printer_info(&uri).unwrap_err();
    match &e {
        PrintError::UntrustedCertificate { printer, fingerprint } => {
            assert_eq!(printer, &format!("localhost:{port}"));
            assert_eq!(fingerprint, &leaf_fingerprint());
        }
        other => panic!("{other:?}"),
    }
    let msg = e.to_string();
    assert!(msg.contains(&leaf_fingerprint()) && msg.contains("Trust Certificate"), "{msg}");

    // trusted by fingerprint: attributes and a job go through TLS
    ipp::trust_certificate(&uri, &leaf_fingerprint()).unwrap();
    let info = ipp::printer_info(&uri).unwrap();
    assert_eq!(info.name, "Test");
    let id = ipp::print_job(&uri, "tester", "secure", "application/pdf", 1, b"%PDF-1.4 test").unwrap();
    assert_eq!(id, 1);
    assert_eq!(printer.jobs()[0].name, "secure");

    // a wrong fingerprint is refused as a fingerprint, and a bad one as input
    assert!(ipp::trust_certificate(&uri, "12:34").is_err());

    // trust on first use: a printer with nothing trusted yet is trusted and remembered
    ipp::set_trusted_certificates(&[]);
    ipp::set_trust_on_first_use(true);
    assert!(ipp::printer_info(&uri).is_ok());
    assert_eq!(ipp::trusted_certificates(), vec![(format!("localhost:{port}"), leaf_fingerprint())]);
    // a changed certificate is not trusted on "first use" again
    ipp::set_trusted_certificates(&[(format!("localhost:{port}"), "00".repeat(32))]);
    assert!(matches!(ipp::printer_info(&uri), Err(PrintError::UntrustedCertificate { .. })));
    ipp::set_trust_on_first_use(false);
    ipp::set_trusted_certificates(&[]);
    drop(printer);
    let _ = h.join();
}

#[test]
fn printer_options_are_read_and_sent_with_the_job() {
    let printer = TestPrinter::start(TestPrinterConfig::default()).unwrap();
    let info = ipp::printer_info(printer.uri()).unwrap();
    let o = &info.options;
    assert_eq!(o.media_types, vec!["stationery", "photographic-glossy", "photographic-matte"]);
    assert_eq!(o.media_type_default.as_deref(), Some("stationery"));
    assert_eq!(o.media_sources.len(), 4);
    assert_eq!(o.media_source_default.as_deref(), Some("auto"));
    assert_eq!((o.qualities.clone(), o.quality_default), (vec![4, 3, 5], Some(4)));
    assert_eq!(o.sides_default.as_deref(), Some("one-sided"));

    let mut s = crate::job::builtin_templates().into_iter().find(|(n, _)| n == "2×2 Cells").unwrap().1;
    s.job.driver = DriverOptions {
        media_type: Some("photographic-glossy".into()),
        media_source: Some("photo".into()),
        print_quality: Some(5),
        sides: Some("two-sided-long-edge".into()),
    };
    let doc = s.build(&(0..4).map(|i| format!("p{i}")).collect::<Vec<_>>()).unwrap();
    let sent = crate::send::send_to_printer(
        printer.uri(),
        &doc,
        &s,
        &super::tests::Gradient,
        &ShapedText::new(),
        &super::tests::send_opts(),
        &mut |_, _| true,
    )
    .unwrap();
    assert!(sent.warnings.iter().all(|w| !w.contains("offers no")), "{:?}", sent.warnings);
    let j = &printer.jobs()[0];
    assert_eq!(j.media_type.as_deref(), Some("photographic-glossy"));
    assert_eq!(j.media_source.as_deref(), Some("photo"));
    assert_eq!(j.print_quality, Some(5));
    assert_eq!(j.sides.as_deref(), Some("two-sided-long-edge"));

    // an option the printer doesn't offer is left to its default, with a warning
    s.job.driver = DriverOptions { media_type: Some("canvas".into()), ..Default::default() };
    let sent = crate::send::send_to_printer(
        printer.uri(),
        &doc,
        &s,
        &super::tests::Gradient,
        &ShapedText::new(),
        &super::tests::send_opts(),
        &mut |_, _| true,
    )
    .unwrap();
    assert!(sent.warnings.iter().any(|w| w.contains("canvas")), "{:?}", sent.warnings);
    assert_eq!(printer.jobs()[1].media_type, None);
}

#[test]
fn driver_options_survive_settings_round_trip_and_hostile_keywords_are_dropped() {
    let mut s = crate::PrintSettings::default();
    s.job.driver.print_quality = Some(3);
    s.job.driver.sides = Some("one-sided".into());
    let back: crate::PrintSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back.job.driver, s.job.driver);
    // a job with a keyword containing a line break or a space is not sent that keyword
    let printer = TestPrinter::start(TestPrinterConfig::default()).unwrap();
    let d = DriverOptions { media_type: Some("bad\r\nkey".into()), sides: Some("one sided".into()), print_quality: Some(42), ..Default::default() };
    ipp::print_job_with(printer.uri(), "t", "j", "application/pdf", 1, &d, b"%PDF").unwrap();
    let j = &printer.jobs()[0];
    assert_eq!((j.media_type.clone(), j.sides.clone(), j.print_quality), (None, None, None));
}
