//! A misbehaving IPP printer on loopback (completion plan 2.13). The well-behaved virtual printer
//! ([`crate::testprinter`]) covers the happy path; this one answers Get-Printer-Attributes normally and then
//! does something wrong with the Print-Job: an IPP error status, an HTTP error, a closed connection, a reply
//! that is not IPP, an oversized reply. Every case is an error naming what happened, never a panic or a hang.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::ipp::{Attribute, Group, Message, Value};
use crate::job::builtin_templates;
use crate::tests::{Gradient, send_opts};
use crate::text::ShapedText;

const GET_PRINTER_ATTRIBUTES: u16 = 0x000B;

fn ipp_reply(code: u16, groups: Vec<Group>) -> Vec<u8> {
    let mut m = Message::request(0, 1, "ipp://x/");
    m.code = code;
    m.groups.extend(groups);
    m.encode()
}

fn printer_attributes() -> Vec<u8> {
    ipp_reply(
        0,
        vec![Group {
            tag: 0x04,
            attrs: vec![
                Attribute { tag: 0x42, name: "printer-name".into(), values: vec![Value::Text("Mock".into())] },
                Attribute { tag: 0x23, name: "printer-state".into(), values: vec![Value::Int(3)] },
                Attribute { tag: 0x49, name: "document-format-supported".into(), values: vec![Value::Text("application/pdf".into())] },
                Attribute { tag: 0x44, name: "media-supported".into(), values: vec![Value::Text("iso_a4_210x297mm".into())] },
            ],
        }],
    )
}

fn http(status: &str, body: &[u8]) -> Vec<u8> {
    let mut v =
        format!("HTTP/1.1 {status}\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    v.extend_from_slice(body);
    v
}

/// A printer whose Print-Job answer is `job` (raw HTTP bytes; `None` = close without a word). Returns its URI
/// and how many Print-Jobs arrived.
fn mock_printer(job: Option<Vec<u8>>) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let uri = format!("ipp://127.0.0.1:{}/printers/mock", l.local_addr().unwrap().port());
    let jobs = Arc::new(AtomicUsize::new(0));
    let j = jobs.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (job, j) = (job.clone(), j.clone());
            std::thread::spawn(move || answer(s, job.as_deref(), &j));
        }
    });
    (uri, jobs)
}

fn answer(mut s: TcpStream, job: Option<&[u8]>, jobs: &AtomicUsize) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))).unwrap_or(0);
    while buf.len() < head_end + len {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let op = buf.get(head_end + 2..head_end + 4).map(|b| u16::from_be_bytes([b[0], b[1]])).unwrap_or(0);
    let out = if op == GET_PRINTER_ATTRIBUTES {
        http("200 OK", &printer_attributes())
    } else {
        jobs.fetch_add(1, Ordering::SeqCst);
        match job {
            Some(b) => b.to_vec(),
            None => return, // dropped: the connection closes
        }
    };
    let _ = s.write_all(&out);
    let _ = s.flush();
}

fn print_to(uri: &str) -> crate::Result<crate::send::Sent> {
    let s = builtin_templates().into_iter().find(|(n, _)| n == "2×2 Cells").map(|(_, s)| s).unwrap_or_default();
    let ids: Vec<String> = (0..2).map(|i| format!("IMG_{i:04}")).collect();
    let doc = s.build(&ids).unwrap();
    crate::send::send_to_printer(uri, &doc, &s, &Gradient, &ShapedText::new(), &send_opts(), &mut |_, _| true)
}

#[test]
fn a_misbehaving_printer_is_an_error_naming_what_happened() {
    // a reply far over the read cap, claiming its whole length
    let huge = 20 * 1024 * 1024;
    let big = [
        format!(
            "HTTP/1.1 200 OK
Content-Type: application/ipp
Content-Length: {huge}

"
        )
        .into_bytes(),
        vec![0u8; huge],
    ]
    .concat();
    let cases: Vec<(&str, Option<Vec<u8>>, &str)> = vec![
        ("format refused", Some(http("200 OK", &ipp_reply(0x040A, vec![]))), "0x040a"),
        ("busy", Some(http("200 OK", &ipp_reply(0x0507, vec![]))), "0x0507"),
        ("http 500", Some(http("500 Internal Server Error", b"oops")), ""),
        ("http 401", Some(http("401 Unauthorized", b"")), ""),
        ("closed", None, ""),
        ("not ipp", Some(http("200 OK", b"<html>printer web page</html>")), ""),
        ("no http", Some(b"\0\0garbage".to_vec()), ""),
        ("truncated", Some(b"HTTP/1.1 200 OK\r\nContent-Length: 400\r\n\r\n\x01\x01".to_vec()), ""),
        ("oversized", Some(big), ""),
    ];
    for (what, job, want) in cases {
        let (uri, jobs) = mock_printer(job);
        let e = print_to(&uri).expect_err(what);
        let msg = e.to_string();
        assert!(!msg.is_empty() && msg.to_ascii_lowercase().contains(want), "{what}: {msg}");
        assert_eq!(jobs.load(Ordering::SeqCst), 1, "{what}: the job was sent once, not retried");
    }
}

/// A printer that accepts the job without saying its id is not followed (nothing to ask about) but is not an
/// error either: the job was taken.
#[test]
fn a_job_accepted_without_details_is_sent() {
    let (uri, jobs) = mock_printer(Some(http("200 OK", &ipp_reply(0, vec![]))));
    let r = print_to(&uri);
    assert_eq!(jobs.load(Ordering::SeqCst), 1);
    if let Ok(sent) = r {
        assert_eq!(sent.format, "application/pdf");
    }
}
