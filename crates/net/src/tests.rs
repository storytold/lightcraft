//! End-to-end tests against in-process servers on 127.0.0.1 (no external network).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::*;

#[derive(Debug, Clone)]
struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, n: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_str())
    }
}

fn read_req(s: impl Read) -> Option<Req> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut it = line.split_whitespace();
    let (method, target) = (it.next()?.to_string(), it.next()?.to_string());
    let mut headers = Vec::new();
    loop {
        let mut l = String::new();
        r.read_line(&mut l).ok()?;
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Req { method, target, headers, body })
}

fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    out.extend_from_slice(body);
    out
}

type Handler = Arc<dyn Fn(Req) -> Vec<u8> + Send + Sync>;

/// Serve `n` connections with `handler`; the requests seen are collected.
fn serve(n: usize, handler: impl Fn(Req) -> Vec<u8> + Send + Sync + 'static) -> (u16, Arc<Mutex<Vec<Req>>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let handler: Handler = Arc::new(handler);
    let h = std::thread::spawn(move || {
        for _ in 0..n {
            let Ok((mut s, _)) = listener.accept() else { return };
            let Some(req) = read_req(&mut s) else { continue };
            seen2.lock().unwrap().push(req.clone());
            let _ = s.write_all(&handler(req));
        }
    });
    (port, seen, h)
}

fn client() -> Client {
    Client::new(ClientConfig { stall_timeout: Duration::from_secs(5), ..ClientConfig::default() }).unwrap()
}

#[test]
fn get_json_gzip_and_user_agent() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(br#"{"version":"1.2.3"}"#).unwrap();
    let gz = gz.finish().unwrap();
    let (port, seen, h) = serve(1, move |_| response("200 OK", &[("Content-Encoding", "gzip"), ("Content-Type", "application/json")], &gz));
    #[derive(serde::Deserialize)]
    struct V {
        version: String,
    }
    let v: V = client().get(&format!("http://127.0.0.1:{port}/api/server/version")).send().unwrap().error_for_status().unwrap().json().unwrap();
    assert_eq!(v.version, "1.2.3");
    h.join().unwrap();
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req.header("user-agent"), Some(user_agent().as_str()));
    assert!(user_agent().starts_with(dac_brand::BINARY));
    assert_eq!(req.header("accept-encoding"), Some("gzip"));
}

#[test]
fn methods_and_bodies() {
    let (port, seen, h) = serve(4, |r| response("200 OK", &[], &r.body));
    let c = client();
    let base = format!("http://127.0.0.1:{port}/x");
    assert_eq!(c.post(&base).json(&serde_json::json!({"a": 1})).send().unwrap().text().unwrap(), r#"{"a":1}"#);
    assert_eq!(c.put(&base).body("text/plain", b"put".to_vec()).send().unwrap().bytes().unwrap(), b"put");
    assert_eq!(c.patch(&base).json(&[1, 2]).send().unwrap().text().unwrap(), "[1,2]");
    let r = c.delete(&base).send().unwrap();
    assert!(r.is_success());
    h.join().unwrap();
    let seen = seen.lock().unwrap();
    let methods: Vec<&str> = seen.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(methods, ["POST", "PUT", "PATCH", "DELETE"]);
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
}

#[test]
fn error_status_and_bad_headers() {
    let (port, _, h) = serve(1, |_| response("404 Not Found", &[], b"nope"));
    let e = client().get(&format!("http://127.0.0.1:{port}/")).send().unwrap().error_for_status().unwrap_err();
    assert_eq!(e, NetError::Status { code: 404, reason: "Not Found".into() });
    h.join().unwrap();
    let e = client().get("http://127.0.0.1:1/").header("X", "a\r\nInjected: 1").send().unwrap_err();
    assert!(matches!(e, NetError::BadHeader(_)));
    assert!(!e.to_string().contains("Injected"));
}

#[test]
fn multipart_upload_streams_with_progress() {
    let dir = tempdir("mp");
    let file = dir.join("photo.jpg");
    std::fs::write(&file, vec![0xAB; 300_000]).unwrap();
    let (port, seen, h) = serve(1, |r| response("201 Created", &[], format!("{}", r.body.len()).as_bytes()));
    let form = Multipart::new().text("deviceAssetId", "abc").file("assetData", "photo.jpg", "image/jpeg", &file).unwrap();
    let expected = form.len();
    let mut calls = 0u32;
    let mut last = (0, None);
    let mut progress = |s: u64, t: Option<u64>| {
        calls += 1;
        last = (s, t);
    };
    let r = client().post(&format!("http://127.0.0.1:{port}/api/assets")).multipart(form).progress(&mut progress).send().unwrap();
    assert_eq!(r.status, 201);
    assert_eq!(r.text().unwrap(), expected.to_string());
    assert!(calls > 1);
    assert_eq!(last, (expected, Some(expected)));
    h.join().unwrap();
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.header("content-type").unwrap().starts_with("multipart/form-data; boundary="));
    let body = String::from_utf8_lossy(&req.body);
    assert!(body.contains("name=\"assetData\"; filename=\"photo.jpg\""));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn redirects_keep_credentials_on_the_origin_only() {
    let (other, other_seen, h2) = serve(1, |_| response("200 OK", &[], b"elsewhere"));
    let (port, seen, h1) = serve(2, move |r| {
        if r.target == "/a" {
            response("302 Found", &[("Location", "/b")], b"")
        } else {
            response("307 Temporary Redirect", &[("Location", &format!("http://localhost:{other}/c"))], b"")
        }
    });
    let r = client().post(&format!("http://127.0.0.1:{port}/a")).bearer("s3cret-token").header("X-Api-Key", "k3y").json(&1).send().unwrap();
    assert_eq!(r.text().unwrap(), "elsewhere");
    h1.join().unwrap();
    h2.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[1].method, "GET"); // 302 after POST becomes GET
    assert!(seen[1].header("authorization").is_some());
    let other_seen = other_seen.lock().unwrap();
    assert_eq!(other_seen[0].target, "/c");
    assert!(other_seen[0].header("authorization").is_none());
    assert!(other_seen[0].header("x-api-key").is_none());
}

#[test]
fn too_many_redirects() {
    let (port, _, h) = serve(3, |_| response("302 Found", &[("Location", "/loop")], b""));
    let c = Client::new(ClientConfig { max_redirects: 2, ..ClientConfig::default() }).unwrap();
    assert!(matches!(c.get(&format!("http://127.0.0.1:{port}/")).send(), Err(NetError::Redirect(_))));
    h.join().unwrap();
}

#[test]
fn stall_timeout_and_cancel() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let mut held = Vec::new();
        for _ in 0..2 {
            if let Ok((s, _)) = listener.accept() {
                held.push(s); // never answer
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    });
    let c = Client::new(ClientConfig { stall_timeout: Duration::from_millis(600), ..ClientConfig::default() }).unwrap();
    let t = std::time::Instant::now();
    assert_eq!(c.get(&format!("http://127.0.0.1:{port}/")).send().unwrap_err(), NetError::Stalled);
    assert!(t.elapsed() < Duration::from_secs(3));

    let c = Client::new(ClientConfig { stall_timeout: Duration::from_secs(60), ..ClientConfig::default() }).unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        f2.store(true, Ordering::Relaxed);
    });
    let t = std::time::Instant::now();
    assert_eq!(c.get(&format!("http://127.0.0.1:{port}/")).cancel(flag).send().unwrap_err(), NetError::Cancelled);
    assert!(t.elapsed() < Duration::from_secs(5));
    h.join().unwrap();
}

#[test]
fn connect_failure_is_an_error() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port(); // closed again
    assert!(matches!(client().get(&format!("http://127.0.0.1:{port}/")).send(), Err(NetError::Connect(_))));
}

#[test]
fn http_proxy_gets_absolute_form() {
    let (port, seen, h) = serve(1, |_| response("200 OK", &[], b"via proxy"));
    let c = Client::new(ClientConfig { proxy: Some(format!("http://127.0.0.1:{port}")), ..ClientConfig::default() }).unwrap();
    assert_eq!(c.get("http://photos.invalid:2283/api/x?k=v").send().unwrap().text().unwrap(), "via proxy");
    h.join().unwrap();
    assert_eq!(seen.lock().unwrap()[0].target, "http://photos.invalid:2283/api/x?k=v");
    assert!(Client::new(ClientConfig { proxy: Some("https://p".into()), ..ClientConfig::default() }).is_err());
}

#[test]
fn chunked_and_truncated_bodies() {
    let (port, _, h) = serve(2, |r| {
        if r.target == "/chunked" {
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n".to_vec()
        } else {
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec()
        }
    });
    assert_eq!(client().get(&format!("http://127.0.0.1:{port}/chunked")).send().unwrap().text().unwrap(), "abcde");
    assert!(client().get(&format!("http://127.0.0.1:{port}/short")).send().unwrap().bytes().is_err());
    h.join().unwrap();
}

#[test]
fn body_cap() {
    let (port, _, h) = serve(1, |_| response("200 OK", &[], &[1u8; 5000]));
    let c = Client::new(ClientConfig { max_body: 1000, ..ClientConfig::default() }).unwrap();
    assert_eq!(c.get(&format!("http://127.0.0.1:{port}/")).send().unwrap().bytes().unwrap_err(), NetError::TooLarge(1000));
    h.join().unwrap();
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("dac-net-test-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn range_server(data: Vec<u8>, n: usize) -> (u16, Arc<Mutex<Vec<Req>>>, JoinHandle<()>) {
    serve(n, move |r| {
        match r.header("range").and_then(|v| v.strip_prefix("bytes=")).and_then(|v| v.strip_suffix('-')).and_then(|v| v.parse::<usize>().ok()) {
            Some(start) if start < data.len() => {
                let cr = format!("bytes {start}-{}/{}", data.len() - 1, data.len());
                response("206 Partial Content", &[("Content-Range", &cr)], &data[start..])
            }
            _ => response("200 OK", &[], &data),
        }
    })
}

#[test]
fn download_resumes_and_verifies() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let sha = sha256_hex(&data);
    let dir = tempdir("dl");
    let dest = dir.join("model.bin");
    std::fs::write(dir.join("model.bin.part"), &data[..80_000]).unwrap();
    let (port, seen, h) = range_server(data.clone(), 1);
    let mut last = 0;
    let mut progress = |s: u64, _t: Option<u64>| last = s;
    let opts =
        DownloadOptions { size: Some(data.len() as u64), sha256: Some(sha.to_uppercase()), progress: Some(&mut progress), ..Default::default() };
    let got = client().download(&format!("http://127.0.0.1:{port}/m"), &dest, opts).unwrap();
    h.join().unwrap();
    assert_eq!(got, Downloaded { bytes: data.len() as u64, sha256: sha.clone() });
    assert_eq!(last, data.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(!dir.join("model.bin.part").exists());
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].header("range"), Some("bytes=80000-"));
    assert_eq!(seen[0].header("accept-encoding"), Some("identity"));

    // a wrong hash deletes the part file and keeps no file under the real name
    let dest2 = dir.join("bad.bin");
    let (port, _, h) = range_server(data.clone(), 1);
    let opts = DownloadOptions { sha256: Some("00".repeat(32)), ..Default::default() };
    assert!(matches!(client().download(&format!("http://127.0.0.1:{port}/m"), &dest2, opts), Err(NetError::Verify(_))));
    h.join().unwrap();
    assert!(!dest2.exists() && !dir.join("bad.bin.part").exists());

    // a cap smaller than the file
    let (port, _, h) = range_server(data, 1);
    let opts = DownloadOptions { max_size: 1000, ..Default::default() };
    assert!(matches!(client().download(&format!("http://127.0.0.1:{port}/m"), &dir.join("big.bin"), opts), Err(NetError::TooLarge(_))));
    h.join().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

// --- TLS -------------------------------------------------------------------------------------

/// A test CA and a `localhost`/127.0.0.1 leaf it signed (P-256, valid until 2126), generated with
/// openssl for these tests only.
const CA_B64: &str = "MIIBjDCCATGgAwIBAgIUabPpTeBrc750uF10VHKviz4AX94wCgYIKoZIzj0EAwIwEjEQMA4GA1UEAwwHVGVzdCBDQTAgFw0yNjEwMTAwMDA1NDlaGA8yMTI2MDkxNjAwMDU0OVowEjEQMA4GA1UEAwwHVGVzdCBDQTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABB+i1UqTwg54S6Ef3QlGtBCju6tkmrJoeDPLw4AYwvfr07fhEP2asbQew8dGE8EspL4KyYwnTtsBEcocsbAah4ujYzBhMB0GA1UdDgQWBBSqjOvGOiSW/kbE/YVt+Q3Qlm7bEzAfBgNVHSMEGDAWgBSqjOvGOiSW/kbE/YVt+Q3Qlm7bEzAPBgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwICBDAKBggqhkjOPQQDAgNJADBGAiEAzHuuueM9+8Laj3rzm9LfkZuBS5cDoVwxlYPlHsfUvmYCIQDGh047dSWnC7wIfN+bryPuQ7wg023H9UdDlMBMsHDV+w==";
const LEAF_B64: &str = "MIIBqTCCAU6gAwIBAgIUHPx6OQjNVD01xDDk464Uj1xwmzcwCgYIKoZIzj0EAwIwEjEQMA4GA1UEAwwHVGVzdCBDQTAgFw0yNjEwMTAwMDA1NDlaGA8yMTI2MDkxNjAwMDU0OVowFDESMBAGA1UEAwwJbG9jYWxob3N0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEFPSyEHSnzWNFWHjo6Fsccp8IUISLOIv2JcW4dwim6VlrvmcgH4fSRtMLq9JDbrs7eGp5J/7qj5VL3iqt4kGTI6N+MHwwGgYDVR0RBBMwEYIJbG9jYWxob3N0hwR/AAABMAkGA1UdEwQCMAAwEwYDVR0lBAwwCgYIKwYBBQUHAwEwHQYDVR0OBBYEFIkl10ciEZxIBMwplQXH2M/FHL6EMB8GA1UdIwQYMBaAFKqM68Y6JJb+RsT9hW35DdCWbtsTMAoGCCqGSM49BAMCA0kAMEYCIQCGcYFLw3UXrn8ea/Hfn4plLzgTQLoQzYhaA86WzbiexQIhAM/ozbpH7BFWxXjrbJfUwlFZ5A6k670RBOEwjjudfnO4";
const KEY_B64: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgP7on22FsY1CzXydImw0X41brmtJ52CZx4n9+5T6HDV+hRANCAAQU9LIQdKfNY0VYeOjoWxxynwhQhIs4i/Ylxbh3CKbpWWu+ZyAfh9JG0wur0kNuuzt4ankn/uqPlUveKq3iQZMj";

fn b64(s: &str) -> Vec<u8> {
    tls::base64_decode(s).unwrap()
}

/// An HTTPS server for `n` connections answering "secure".
fn tls_serve(n: usize) -> (u16, JoinHandle<()>) {
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
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let conn = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, s);
            if read_req(&mut tls).is_some() {
                let _ = tls.write_all(&response("200 OK", &[], b"secure"));
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        }
    });
    (port, h)
}

fn get_tls(trust: Trust, port: u16) -> Result<String, NetError> {
    let c = Client::new(ClientConfig { trust, stall_timeout: Duration::from_secs(5), ..ClientConfig::default() })?;
    c.get(&format!("https://localhost:{port}/")).send()?.text()
}

#[test]
fn tls_trust_ca_pin_and_tofu() {
    let leaf_fp = Fingerprint::of(&b64(LEAF_B64));
    let (port, h) = tls_serve(5);

    // untrusted: the error names the fingerprint
    match get_tls(Trust::new(), port) {
        Err(NetError::UntrustedCertificate { host, fingerprint }) => {
            assert_eq!(host, "localhost");
            assert_eq!(fingerprint, leaf_fp.to_string());
        }
        other => panic!("{other:?}"),
    }
    // the connection's own CA
    assert_eq!(get_tls(Trust::new().with_ca(&b64(CA_B64)).unwrap(), port).unwrap(), "secure");
    // a pinned fingerprint
    assert_eq!(get_tls(Trust::new().with_pinned(leaf_fp), port).unwrap(), "secure");
    // trust on first use: declined, then accepted (and pinned)
    assert!(matches!(get_tls(Trust::new().with_tofu(Arc::new(|_| false)), port), Err(NetError::UntrustedCertificate { .. })));
    let seen = Arc::new(Mutex::new(None));
    let s2 = seen.clone();
    let trust = Trust::new().with_tofu(Arc::new(move |info: &CertInfo| {
        *s2.lock().unwrap() = Some(info.fingerprint);
        true
    }));
    assert_eq!(get_tls(trust.clone(), port).unwrap(), "secure");
    assert_eq!(*seen.lock().unwrap(), Some(leaf_fp));
    assert_eq!(trust.pinned(), vec![leaf_fp]);
    h.join().unwrap();
}

#[test]
fn ca_pem_is_accepted() {
    let pem = format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", CA_B64);
    assert!(Trust::new().with_ca(pem.as_bytes()).is_ok());
}

// --- logs never carry secrets -----------------------------------------------------------------

struct Capture;
static LOGS: Mutex<String> = Mutex::new(String::new());

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        LOGS.lock().unwrap().push_str(&format!("{} {}\n", r.target(), r.args()));
    }
    fn flush(&self) {}
}

#[test]
fn secrets_never_reach_the_log_or_debug_output() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = log::set_logger(&Capture);
        log::set_max_level(log::LevelFilter::Trace);
    });
    let (port, _, h) = serve(1, |_| response("401 Unauthorized", &[("Set-Cookie", "session=c00kie-secret")], b""));
    let r = client()
        .get(&format!("http://127.0.0.1:{port}/api/x?key=share-secret"))
        .bearer("bearer-secret")
        .header("x-api-key", "apikey-secret")
        .send()
        .unwrap();
    let debug = format!("{r:?}");
    let err = r.error_for_status().unwrap_err().to_string();
    h.join().unwrap();
    let logs = LOGS.lock().unwrap().clone();
    assert!(logs.contains("net: GET"), "logging works: {logs}");
    for text in [&logs, &debug, &err] {
        for secret in ["bearer-secret", "apikey-secret", "share-secret", "c00kie-secret"] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }
}
