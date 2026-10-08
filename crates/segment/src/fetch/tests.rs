//! The downloader against a local HTTP server (std `TcpListener`; never the internet): mirror
//! fallback, redirects, resuming, chunked bodies, stalls, hash mismatches, size caps, cancel.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;

/// The served file: 300 kB of a deterministic pattern.
fn payload() -> Vec<u8> {
    (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect()
}

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|b| format!("{b:02x}")).collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn pinned() -> FileSpec {
    let p = payload();
    FileSpec { name: "model.bin", size: Some(p.len() as u64), sha256: Some(leak(sha_hex(&p))), max: p.len() as u64 }
}

struct Server {
    base: String,
    hits: Arc<AtomicUsize>,
}

/// Serve on 127.0.0.1; the first path segment picks the behaviour.
fn server() -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let h = h.clone();
            std::thread::spawn(move || handle(s, &h));
        }
    });
    Server { base, hits }
}

fn handle(mut s: TcpStream, hits: &AtomicUsize) {
    let n = hits.fetch_add(1, Ordering::SeqCst);
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut first = String::new();
    if r.read_line(&mut first).is_err() {
        return;
    }
    let mut range: Option<u64> = None;
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
            break;
        }
        if let Some(v) = l.to_ascii_lowercase().strip_prefix("range: bytes=") {
            range = v.trim().trim_end_matches('-').parse().ok();
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
    let mode = path.trim_start_matches('/').split('/').next().unwrap_or("").to_string();
    let body = payload();
    let send = |s: &mut TcpStream, status: &str, headers: &str, data: &[u8]| {
        let _ = s.write_all(format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n").as_bytes());
        let _ = s.write_all(data);
    };
    match mode.as_str() {
        "missing" => send(&mut s, "404 Not Found", "Content-Length: 9\r\n", b"not found"),
        "error" => send(&mut s, "500 Oops", "Content-Length: 25\r\n", b"<html>server error</html>"),
        "redirect" => send(&mut s, "302 Found", &format!("Location: {}\r\nContent-Length: 0\r\n", path.replacen("/redirect", "/good", 1)), b""),
        "wrong" => {
            let mut b = body.clone();
            b[1000] ^= 0xff;
            send(&mut s, "200 OK", &format!("Content-Length: {}\r\n", b.len()), &b)
        }
        "toolong" => {
            // no length announced, more bytes than the file can have
            let mut b = body.clone();
            b.extend_from_slice(&[7u8; 5000]);
            send(&mut s, "200 OK", "", &b)
        }
        "chunked" => {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
            for c in body.chunks(70_001) {
                let _ = s.write_all(format!("{:x};ext=1\r\n", c.len()).as_bytes());
                let _ = s.write_all(c);
                let _ = s.write_all(b"\r\n");
            }
            let _ = s.write_all(b"0\r\nX-Trailer: 1\r\n\r\n");
        }
        "stall" => {
            // half the file, then nothing
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
            let _ = s.write_all(&body[..body.len() / 2]);
            std::thread::sleep(Duration::from_secs(5));
        }
        "slow" => {
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
            for c in body.chunks(1000) {
                if s.write_all(c).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        "flaky" if n.is_multiple_of(2) && range.is_none() => {
            // the first try breaks off after a third
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
            let _ = s.write_all(&body[..body.len() / 3]);
        }
        "range416" if range.is_some() => send(&mut s, "416 Range Not Satisfiable", "Content-Length: 0\r\n", b""),
        "norange" | "range416" => send(&mut s, "200 OK", &format!("Content-Length: {}\r\n", body.len()), &body),
        _ => match range {
            Some(from) if from as usize >= body.len() => send(&mut s, "416 Range Not Satisfiable", "Content-Length: 0\r\n", b""),
            Some(from) => send(
                &mut s,
                "206 Partial Content",
                &format!("Content-Range: bytes {from}-{}/{}\r\nContent-Length: {}\r\n", body.len() - 1, body.len(), body.len() - from as usize),
                &body[from as usize..],
            ),
            None => send(&mut s, "200 OK", &format!("Content-Length: {}\r\n", body.len()), &body),
        },
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-sam3-fetch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn quick() -> Options {
    Options { connect_timeout: Duration::from_secs(2), stall_timeout: Duration::from_millis(600), attempts: 2 }
}

fn run(files: &[FileSpec], mirrors: &[String], dir: &Path, cancel: &AtomicBool) -> (Result<(), DownloadError>, Vec<Progress>) {
    let mut seen = Vec::new();
    let r = download(files, mirrors, dir, &quick(), cancel, &mut |p| seen.push(p.clone()));
    (r, seen)
}

#[test]
fn falls_back_to_the_next_mirror_and_verifies() {
    let srv = server();
    let dir = tmp("fallback");
    let mirrors = vec![
        format!("{}/missing", srv.base),
        format!("{}/error", srv.base),
        "http://127.0.0.1:1/unreachable".to_string(),
        format!("{}/redirect", srv.base),
    ];
    let (r, seen) = run(&[pinned()], &mirrors, &dir, &AtomicBool::new(false));
    r.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    assert!(!part_path(&dir, "model.bin").exists());
    let last = seen.last().unwrap();
    assert_eq!((last.done, last.total), (300_000, 300_000));
    // an error page never ends up in the file: nothing left over from the failed mirrors
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    // installed: nothing is fetched again
    let hits = srv.hits.load(Ordering::SeqCst);
    run(&[pinned()], &mirrors, &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(srv.hits.load(Ordering::SeqCst), hits);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_hash_mismatch_is_deleted_and_the_next_mirror_used() {
    let srv = server();
    let dir = tmp("hash");
    let (r, _) = run(&[pinned()], &[format!("{}/wrong", srv.base)], &dir, &AtomicBool::new(false));
    let e = r.unwrap_err().to_string();
    assert!(e.contains("SHA-256"), "{e}");
    assert!(!dir.join("model.bin").exists() && !part_path(&dir, "model.bin").exists(), "a damaged file is never kept");
    run(&[pinned()], &[format!("{}/wrong", srv.base), format!("{}/good", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resumes_partial_files_and_restarts_bad_ones() {
    let srv = server();
    let dir = tmp("resume");
    std::fs::create_dir_all(&dir).unwrap();
    // a third already there
    std::fs::write(part_path(&dir, "model.bin"), &payload()[..100_000]).unwrap();
    run(&[pinned()], &[format!("{}/good", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    // a partial file with the wrong bytes: the hash fails, it's deleted, the next try starts over
    std::fs::remove_file(dir.join("model.bin")).unwrap();
    std::fs::write(part_path(&dir, "model.bin"), vec![0u8; 100_000]).unwrap();
    run(&[pinned()], &[format!("{}/good", srv.base), format!("{}/good", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    // a server ignoring Range: the whole file again, not appended
    std::fs::remove_file(dir.join("model.bin")).unwrap();
    std::fs::write(part_path(&dir, "model.bin"), &payload()[..5]).unwrap();
    run(&[pinned()], &[format!("{}/norange", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    // a connection breaking off is resumed on the same mirror
    std::fs::remove_file(dir.join("model.bin")).unwrap();
    run(&[pinned()], &[format!("{}/flaky", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_416_or_a_bad_complete_part_starts_over() {
    let srv = server();
    let dir = tmp("416");
    std::fs::create_dir_all(&dir).unwrap();
    // the server refuses the range: the part is dropped and the file fetched whole (no loop)
    std::fs::write(part_path(&dir, "model.bin"), &payload()[..100]).unwrap();
    run(&[pinned()], &[format!("{}/range416", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    // as long as the file but wrong: hash fails → deleted → downloaded again from the same mirror
    std::fs::remove_file(dir.join("model.bin")).unwrap();
    std::fs::write(part_path(&dir, "model.bin"), vec![1u8; 300_000]).unwrap();
    run(&[pinned()], &[format!("{}/good", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn chunked_bodies_and_unpinned_files() {
    let srv = server();
    let dir = tmp("chunked");
    let f = FileSpec { name: "vocab.json", size: None, sha256: None, max: 1 << 20 };
    run(std::slice::from_ref(&f), &[format!("{}/chunked", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("vocab.json")).unwrap(), payload());
    // more than the cap: refused, nothing kept
    let small = FileSpec { max: 1000, ..f };
    let dir2 = tmp("cap");
    let e = run(&[small], &[format!("{}/good", srv.base)], &dir2, &AtomicBool::new(false)).0.unwrap_err().to_string();
    assert!(e.contains("wrong size") || e.contains("more than"), "{e}");
    assert!(!dir2.join("vocab.json").exists() && !part_path(&dir2, "vocab.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn a_server_sending_too_much_is_cut_off() {
    let srv = server();
    let dir = tmp("toolong");
    let e = run(&[pinned()], &[format!("{}/toolong", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap_err().to_string();
    assert!(e.contains("more than"), "{e}");
    assert!(!dir.join("model.bin").exists() && !part_path(&dir, "model.bin").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stalled_server_times_out_and_the_next_mirror_finishes() {
    let srv = server();
    let dir = tmp("stall");
    let t = Instant::now();
    let (r, _) = run(&[pinned()], &[format!("{}/stall", srv.base), format!("{}/good", srv.base)], &dir, &AtomicBool::new(false));
    r.unwrap();
    assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_stops_quickly_and_keeps_the_part_for_later() {
    let srv = server();
    let dir = tmp("cancel");
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        c.store(true, Ordering::SeqCst);
    });
    let t = Instant::now();
    let (r, _) = run(&[pinned()], &[format!("{}/slow", srv.base)], &dir, &cancel);
    assert_eq!(r, Err(DownloadError::Cancelled));
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    assert!(!dir.join("model.bin").exists());
    let kept = std::fs::metadata(part_path(&dir, "model.bin")).map(|m| m.len()).unwrap_or(0);
    assert!(kept > 0 && kept < 300_000, "{kept}");
    // and the next run resumes it
    run(&[pinned()], &[format!("{}/good", srv.base)], &dir, &AtomicBool::new(false)).0.unwrap();
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), payload());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_mirrors_is_a_clear_error_and_mirror_lists_are_parsed() {
    let dir = tmp("nomirror");
    let e = run(&[pinned()], &[], &dir, &AtomicBool::new(false)).0.unwrap_err();
    assert_eq!(e, DownloadError::NoMirrors);
    assert!(e.to_string().contains(MIRRORS_ENV));
    let file = std::env::temp_dir().join(format!("lc-sam3-mirrors-{}.txt", std::process::id()));
    std::fs::write(&file, "# mine\nhttps://b.example/m/ # second\n\nnot a url\nhttps://a.example/x\n").unwrap();
    let m = mirrors(Some("https://a.example/x/, ftp://no"), Some(&file));
    assert_eq!(m[..2], ["https://a.example/x".to_string(), "https://b.example/m".to_string()]);
    assert_eq!(m.len(), 2 + DEFAULT_MIRRORS.len());
    assert_eq!(mirrors(None, Some(Path::new("/nonexistent/mirrors.txt"))).len(), DEFAULT_MIRRORS.len());
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn content_ranges() {
    assert_eq!(parse_content_range("bytes 100-299/300"), Some((100, Some(300))));
    assert_eq!(parse_content_range("bytes 0-0/*"), Some((0, None)));
    assert_eq!(parse_content_range("bytes 9-1/300"), None);
    assert_eq!(parse_content_range("items 1-2/3"), None);
}
