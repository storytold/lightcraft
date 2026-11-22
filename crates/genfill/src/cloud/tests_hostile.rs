//! Provider clients against a scripted, misbehaving HTTP server on loopback (completion plan 2.13): error
//! statuses, oversized and truncated replies, replies that are not PNG. Every case must end in a [`GenError`]
//! the user can read, never a panic, a hang or a huge allocation. The friendly mock ([`super::mock`]) covers the
//! happy paths; this one only answers what each test scripts.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::util::{b64_encode, png_encode};
use super::*;

const KEY: &str = "test-key-not-real-0002";

/// What one connection gets back: raw bytes, or a streamed body of `n` zero bytes after `head`.
enum Answer {
    Raw(Vec<u8>),
    Stream { head: Vec<u8>, n: usize },
}

fn http(status: &str, ctype: &str, body: &[u8]) -> Answer {
    let mut v = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    v.extend_from_slice(body);
    Answer::Raw(v)
}

/// A loopback server answering each request with `script(method, path, base)`. Lives as long as the test.
fn serve(script: impl Fn(&str, &str, &str) -> Answer + Send + Sync + 'static) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let script = Arc::new(script);
    let b = base.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (script, b) = (script.clone(), b.clone());
            std::thread::spawn(move || answer(s, &*script, &b));
        }
    });
    base
}

fn answer(mut s: TcpStream, script: &dyn Fn(&str, &str, &str) -> Answer, base: &str) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let len: usize =
        head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))).unwrap_or(0);
    while buf.len() < head_end + len {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let mut first = head.split_whitespace();
    let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
    match script(&method, &path, base) {
        Answer::Raw(b) => {
            let _ = s.write_all(&b);
        }
        Answer::Stream { head, n } => {
            if s.write_all(&head).is_err() {
                return;
            }
            let zeros = vec![0u8; 1 << 20];
            let mut left = n;
            while left > 0 {
                let k = left.min(zeros.len());
                if s.write_all(&zeros[..k]).is_err() {
                    break; // the client stopped reading: expected
                }
                left -= k;
            }
        }
    }
    let _ = s.flush();
}

fn request() -> Request {
    let (w, h) = (32, 32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
    let mask: Vec<u8> = (0..w * h).map(|i| if i % w > w / 2 { 255 } else { 0 }).collect();
    Request { width: w, height: h, rgb, mask, prompt: String::new(), variations: 1, seed: 1 }
}

fn run(p: Provider, base: &str) -> Result<Vec<Image>, GenError> {
    let cancel = AtomicBool::new(false);
    let ctl = Control { cancel: &cancel, stall: Duration::from_secs(10), deadline: Duration::from_secs(30), accepted: None };
    generate(p, KEY, &Endpoint::mock(base).unwrap(), &request(), &ctl)
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// A PNG signature, a valid IHDR claiming `w × h` RGB pixels and an empty IDAT: the header reads, the pixels don't.
fn png_claiming(w: u32, h: u32) -> Vec<u8> {
    let mut chunk = b"IHDR".to_vec();
    chunk.extend_from_slice(&w.to_be_bytes());
    chunk.extend_from_slice(&h.to_be_bytes());
    chunk.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(&chunk);
    v.extend_from_slice(&crc32(&chunk).to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(b"IDAT");
    v.extend_from_slice(&crc32(b"IDAT").to_be_bytes());
    v
}

/// Stability answers `/v2beta/...` with JSON `{"image": <b64>}`; the reply under test goes there.
fn stability_image(bytes: Vec<u8>) -> Answer {
    http("200 OK", "application/json", json!({"image": b64_encode(&bytes), "finish_reason": "SUCCESS"}).to_string().as_bytes())
}

#[test]
fn error_statuses_map_to_what_the_user_can_do() {
    let cases: Vec<(&str, Vec<u8>, fn(&GenError) -> bool)> = vec![
        ("401 Unauthorized", br#"{"message":"bad key"}"#.to_vec(), |e| matches!(e, GenError::Unauthorized(_))),
        ("403 Forbidden", b"nope".to_vec(), |e| matches!(e, GenError::Unauthorized(_))),
        (
            "402 Payment Required",
            br#"{"error":{"message":"out of credits"}}"#.to_vec(),
            |e| matches!(e, GenError::Payment(m) if m.contains("credits")),
        ),
        ("429 Too Many Requests", br#"{"detail":"slow down"}"#.to_vec(), |e| matches!(e, GenError::RateLimited(m) if m.contains("slow"))),
        ("400 Bad Request", br#"{"errors":[{"msg":"blocked by moderation"}]}"#.to_vec(), |e| matches!(e, GenError::Refused(_))),
        ("500 Internal Server Error", b"<html>oops</html>".to_vec(), |e| matches!(e, GenError::Provider(m) if m.starts_with("500"))),
        ("503 Service Unavailable", Vec::new(), |e| matches!(e, GenError::Provider(m) if m.starts_with("503"))),
        // a status line that is no status at all
        ("abc nonsense", Vec::new(), |e| matches!(e, GenError::Network(_) | GenError::Provider(_))),
    ];
    for (status, body, want) in cases {
        let (st, b) = (status.to_string(), body.clone());
        let base = serve(move |_, _, _| http(&st, "application/json", &b));
        let e = run(Provider::Stability, &base).unwrap_err();
        assert!(want(&e), "{status}: {e:?}");
        assert!(!e.to_string().is_empty());
    }
}

#[test]
fn a_key_echoed_in_an_error_is_redacted() {
    let base =
        serve(|_, _, _| http("401 Unauthorized", "application/json", json!({"message": format!("key {KEY} is invalid")}).to_string().as_bytes()));
    for p in ALL {
        let e = run(p, &base).unwrap_err();
        assert!(!e.to_string().contains(KEY) && !e.to_string().contains(&KEY[..12]), "{p:?}: {e}");
    }
}

#[test]
fn replies_that_are_not_png_are_named() {
    let jpeg = b"\xff\xd8\xff\xe0\0\x10JFIF\0".to_vec();
    let cases: Vec<(Answer, &str)> = vec![
        (stability_image(jpeg), "JPEG"),
        (stability_image(b"GIF89a....".to_vec()), "not a PNG"),
        (stability_image(Vec::new()), "not a PNG"),
        (http("200 OK", "application/json", br#"{"image": "***not base64***"}"#), "unreadable image data"),
        (http("200 OK", "application/json", br#"{"artifacts": []}"#), "no image"),
        (http("200 OK", "text/html", b"<html>maintenance</html>"), "unreadable reply"),
        (stability_image(png_claiming(100_000, 100_000)), "100000"),
        (stability_image(png_claiming(0, 7)), "PNG"),
        (stability_image(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR garbage".to_vec()), "PNG"),
    ];
    for (i, (a, want)) in cases.into_iter().enumerate() {
        let a = std::sync::Mutex::new(Some(a));
        let base = serve(move |_, _, _| a.lock().unwrap().take().unwrap_or_else(|| http("500 Gone", "text/plain", b"once")));
        let e = run(Provider::Stability, &base).unwrap_err();
        assert!(matches!(&e, GenError::Provider(m) if m.contains(want)), "case {i}: want {want:?}, got {e:?}");
    }
    // a truncated real PNG
    let png = png_encode(8, 8, 3, &[90; 8 * 8 * 3]).unwrap();
    let cut = png[..png.len() / 2].to_vec();
    let base = serve(move |_, _, _| stability_image(cut.clone()));
    assert!(matches!(run(Provider::Stability, &base), Err(GenError::Provider(_))));
}

/// Black Forest Labs downloads its result from an address it hands back: the raw bytes there are checked too.
#[test]
fn a_downloaded_result_that_is_not_png_or_missing_is_an_error() {
    for (sample, want) in [(http("200 OK", "image/jpeg", b"\xff\xd8\xff\xdb rest"), "JPEG"), (http("404 Not Found", "text/plain", b"expired"), "404")]
    {
        let sample = std::sync::Mutex::new(Some(sample));
        let base = serve(move |method, path, base| match (method, path) {
            ("POST", _) => {
                http("200 OK", "application/json", json!({"id": "1", "polling_url": format!("{base}/v1/get_result?id=1")}).to_string().as_bytes())
            }
            ("GET", p) if p.starts_with("/v1/get_result") => http(
                "200 OK",
                "application/json",
                json!({"status": "Ready", "result": {"sample": format!("{base}/sample.png")}}).to_string().as_bytes(),
            ),
            _ => sample.lock().unwrap().take().unwrap_or_else(|| http("410 Gone", "text/plain", b"once")),
        });
        let e = run(Provider::Bfl, &base).unwrap_err();
        assert!(matches!(&e, GenError::Provider(m) if m.contains(want)), "{want}: {e:?}");
    }
    // a result address elsewhere is not followed
    let base = serve(|method, _, _| match method {
        "POST" => http("200 OK", "application/json", br#"{"polling_url": "http://203.0.113.9/steal"}"#),
        _ => http("500 No", "text/plain", b"unexpected"),
    });
    assert!(matches!(run(Provider::Bfl, &base), Err(GenError::Provider(m)) if m.contains("unexpected address")));
}

/// A reply over the read cap stops the read with an error, instead of buffering it all.
#[test]
fn an_oversized_reply_is_refused() {
    let n = MAX_RESPONSE + (4 << 20);
    let base = serve(move |_, _, _| Answer::Stream {
        head: format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n").into_bytes(),
        n,
    });
    let e = run(Provider::Stability, &base).unwrap_err();
    assert!(matches!(&e, GenError::Network(m) if m.contains("MB")), "{e:?}");
    // the same without a length (read to close)
    let base =
        serve(move |_, _, _| Answer::Stream { head: b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n".to_vec(), n });
    assert!(matches!(run(Provider::Stability, &base), Err(GenError::Network(m)) if m.contains("MB")));
}

#[test]
fn a_truncated_or_silent_reply_is_a_network_error() {
    let base = serve(|_, _, _| Answer::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\n\r\n{\"image\":".to_vec()));
    assert!(matches!(run(Provider::Stability, &base), Err(GenError::Network(_))));
    // the connection closes without a word
    let base = serve(|_, _, _| Answer::Raw(Vec::new()));
    assert!(matches!(run(Provider::OpenAi, &base), Err(GenError::Network(_))));
}
