//! P6.2: never crash on what a server (or a URL typed by the user) sends.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

#[test]
fn hostile_urls_never_panic() {
    let seeds = ["https://photos.example:2283/api/x?y=1#f", "http://[::1]:80/", "http://user:pw@h/", "https://ü.example/ä"];
    dac_fuzzkit::run_str("net.url", &seeds, 5000, |s| {
        if let Ok(u) = Url::parse(s) {
            let _ = (u.host_header(), u.origin());
            for loc in ["/a", "//other/b", "../c", s, "https://x/y", "?q", ""] {
                let _ = u.join(loc);
            }
        }
        let _ = Fingerprint::parse(s);
    });
}

#[test]
fn hostile_response_heads_never_panic() {
    let head = "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nTransfer-Encoding: chunked\r\nLocation: /x\r\nContent-Encoding: gzip\r\n\r\n";
    dac_fuzzkit::run_str("net.head", &[head, "HTTP/1.0 301 Moved\r\n\r\n"], 5000, |s| {
        if let Ok(h) = http::read_head(&mut s.as_bytes()) {
            let _ = h.header("content-length");
        }
    });
}

/// Skip the request (head and any body up to a blank line is enough for GETs).
fn drain_request(s: impl Read) {
    let mut r = BufReader::new(s);
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
            return;
        }
    }
}

/// Whole responses (status, headers, chunked and gzip bodies) from a server that answers with
/// garbage: errors, never a panic, a hang or an unbounded allocation.
#[test]
fn hostile_responses_through_the_client_never_panic() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(br#"{"version":"1.2.3"}"#).unwrap();
    let gz = gz.finish().unwrap();
    let mut zipped = format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", gz.len()).into_bytes();
    zipped.extend_from_slice(&gz);
    let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"a\":[1,2,3]}".to_vec();
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n".to_vec();
    let redirect = b"HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\nContent-Length: 0\r\n\r\n".to_vec();
    let seeds: Vec<&[u8]> = vec![&plain, &zipped, &chunked, &redirect];
    let answer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let a2 = answer.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            drain_request(&mut s);
            let out = a2.lock().unwrap().clone();
            let _ = s.write_all(&out);
        }
    });
    let c = Client::new(ClientConfig { stall_timeout: Duration::from_millis(500), ..ClientConfig::default() }).unwrap();
    let url = format!("http://127.0.0.1:{port}/x");
    // a loopback round trip per iteration: keep it short (DAC_FUZZ_ITERS raises it)
    dac_fuzzkit::run("net.response", &seeds, 150, |b| {
        *answer.lock().unwrap() = b.to_vec();
        if let Ok(r) = c.get(&url).send() {
            let _ = r.bytes();
        }
    });
}
