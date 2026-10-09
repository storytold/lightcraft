//! `cargo xtask immich up|down|seed`: a pinned local Immich server for integration tests (plan/immich.md).
//!
//! - `up` starts `xtask/immich/compose.yml` with `docker compose` and waits until `GET /api/server/ping` answers.
//! - `down [--volumes]` stops it (`--volumes` also deletes its uploads and database).
//! - `seed` creates the admin user (first run only), logs in, creates an API key, uploads a small fixture set of
//!   PNGs generated here (our own work, CC0) into an album, and writes the key to `target/immich/api-key`.
//!
//! The server URL defaults to [`DEFAULT_URL`]; `IMMICH_URL` overrides it (for an already running test server).
//! Only `docker` and `curl` are invoked, through `std::process::Command`; nothing here talks to a real library.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const COMPOSE: &str = "xtask/immich/compose.yml";
pub const DEFAULT_URL: &str = "http://127.0.0.1:2284";
/// Test-only admin account of the throw-away server.
const ADMIN_EMAIL: &str = "admin@example.invalid";
const ADMIN_PASSWORD: &str = "xtask-immich-admin";
const ADMIN_NAME: &str = "Test Admin";
const ALBUM: &str = "xtask fixtures";
const FIXTURES: usize = 8;
const WAIT: Duration = Duration::from_secs(300);

const USAGE: &str = "usage: cargo xtask immich up | down [--volumes] | seed";

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let url = base_url(std::env::var("IMMICH_URL").ok().as_deref());
    match args.first().copied() {
        Some("up") => up(root, &url),
        Some("down") => down(root, args.contains(&"--volumes")),
        Some("seed") => seed(root, &url),
        _ => Err(USAGE.into()),
    }
}

/// The server URL without a trailing slash (`IMMICH_URL` or the compose default).
pub fn base_url(env: Option<&str>) -> String {
    env.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(DEFAULT_URL).trim_end_matches('/').to_string()
}

fn compose(root: &Path) -> Command {
    let mut c = Command::new("docker");
    c.current_dir(root).arg("compose").arg("-f").arg(root.join(COMPOSE));
    c
}

fn status_ok(mut c: Command, what: &str) -> Result<(), String> {
    let s = c.status().map_err(|e| format!("{what}: {e} (is docker installed?)"))?;
    if s.success() { Ok(()) } else { Err(format!("{what} failed ({s})")) }
}

fn up(root: &Path, url: &str) -> Result<(), String> {
    let mut c = compose(root);
    c.args(["up", "-d"]);
    status_ok(c, "docker compose up")?;
    println!("waiting for {url}/api/server/ping (first start downloads the images and can take minutes)…");
    let start = Instant::now();
    loop {
        if let Ok((200, body)) = request(url, "GET", "/api/server/ping", &Auth::None, None)
            && body["res"] == "pong"
        {
            println!("Immich is up at {url} ({} s)", start.elapsed().as_secs());
            return Ok(());
        }
        if start.elapsed() > WAIT {
            return Err(format!("Immich did not answer {url}/api/server/ping within {} s (see `docker compose logs`)", WAIT.as_secs()));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn down(root: &Path, volumes: bool) -> Result<(), String> {
    let mut c = compose(root);
    c.arg("down");
    if volumes {
        c.arg("--volumes");
    }
    status_ok(c, "docker compose down")
}

enum Auth {
    None,
    Bearer(String),
    Key(String),
}

impl Auth {
    fn header(&self) -> Option<String> {
        match self {
            Auth::None => None,
            Auth::Bearer(t) => Some(format!("Authorization: Bearer {t}")),
            Auth::Key(k) => Some(format!("x-api-key: {k}")),
        }
    }
}

/// Marker curl prints after the body (`-w`), so the status code can be split off.
const STATUS_MARK: &str = "\n@@xtask-http-status:";

/// Split curl's output (`<body>` + [`STATUS_MARK`] + `<code>`) into status and JSON body (`Null` when empty or
/// not JSON).
pub fn split_status(out: &str) -> Option<(u16, Value)> {
    let (body, code) = out.rsplit_once(STATUS_MARK)?;
    let code = code.trim().parse().ok()?;
    Some((code, serde_json::from_str(body).unwrap_or(Value::Null)))
}

fn curl(url: &str, method: &str, path: &str, auth: &Auth) -> Command {
    let mut c = Command::new("curl");
    c.args(["-sS", "--max-time", "60", "-X", method, "-H", "Accept: application/json", "-w"]).arg(format!("{STATUS_MARK}%{{http_code}}"));
    if let Some(h) = auth.header() {
        c.arg("-H").arg(h);
    }
    c.arg(format!("{url}{path}"));
    c
}

fn finish(mut c: Command, what: &str) -> Result<(u16, Value), String> {
    let out = c.output().map_err(|e| format!("{what}: curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("{what}: curl failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    split_status(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| format!("{what}: unexpected curl output"))
}

fn request(url: &str, method: &str, path: &str, auth: &Auth, body: Option<&Value>) -> Result<(u16, Value), String> {
    let mut c = curl(url, method, path, auth);
    if let Some(b) = body {
        c.args(["-H", "Content-Type: application/json", "--data-binary"]).arg(b.to_string());
    }
    finish(c, &format!("{method} {path}"))
}

/// A 2xx response's body, or an error with the server's message.
fn expect_ok(what: &str, (code, body): (u16, Value)) -> Result<Value, String> {
    if (200..300).contains(&code) { Ok(body) } else { Err(format!("{what}: HTTP {code}: {body}")) }
}

fn seed(root: &Path, url: &str) -> Result<(), String> {
    let (code, body) =
        request(url, "GET", "/api/server/ping", &Auth::None, None).map_err(|e| format!("{e}\nis the server running? `cargo xtask immich up`"))?;
    if code != 200 || body["res"] != "pong" {
        return Err(format!("{url} is not an Immich server (ping: HTTP {code}: {body})"));
    }
    let version = expect_ok("server version", request(url, "GET", "/api/server/version", &Auth::None, None)?)?;
    println!("Immich {}.{}.{} at {url}", version["major"], version["minor"], version["patch"]);

    // the first sign-up creates the admin; afterwards the endpoint refuses, which is fine
    let signup = json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD, "name": ADMIN_NAME });
    match request(url, "POST", "/api/auth/admin-sign-up", &Auth::None, Some(&signup))? {
        (c, _) if (200..300).contains(&c) => println!("created admin {ADMIN_EMAIL}"),
        (c, b) => println!("admin sign-up skipped (HTTP {c}: {}); logging in", b["message"]),
    }
    let login = json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD });
    let session = expect_ok("login", request(url, "POST", "/api/auth/login", &Auth::None, Some(&login))?)?;
    let token = session["accessToken"].as_str().ok_or("login: no accessToken in the response")?.to_string();
    let bearer = Auth::Bearer(token);

    let key_req = json!({ "name": "xtask seed", "permissions": ["all"] });
    let key = expect_ok("create API key", request(url, "POST", "/api/api-keys", &bearer, Some(&key_req))?)?;
    let secret = key["secret"].as_str().ok_or("create API key: no secret in the response")?.to_string();
    let out_dir = root.join("target/immich");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    let key_path = out_dir.join("api-key");
    write_secret(&key_path, &secret)?;
    println!("API key written to {}", key_path.display());
    let auth = Auth::Key(secret);

    let fixtures = write_fixtures(&out_dir.join("fixtures"))?;
    let mut ids = Vec::new();
    for (i, path) in fixtures.iter().enumerate() {
        let (code, body) = upload(url, &auth, path, &fixture_time(i))?;
        let body = expect_ok(&format!("upload {}", path.display()), (code, body))?;
        if let Some(id) = body["id"].as_str() {
            ids.push(id.to_string());
        }
        println!("  {} → {} ({})", file_name(path), body["id"], body["status"]);
    }

    let albums = expect_ok("list albums", request(url, "GET", "/api/albums", &auth, None)?)?;
    let existing = albums.as_array().into_iter().flatten().find(|a| a["albumName"] == ALBUM).and_then(|a| a["id"].as_str()).map(str::to_string);
    let album_id = match existing {
        Some(id) => id,
        None => {
            let a = expect_ok("create album", request(url, "POST", "/api/albums", &auth, Some(&json!({ "albumName": ALBUM })))?)?;
            a["id"].as_str().ok_or("create album: no id in the response")?.to_string()
        }
    };
    expect_ok("add to album", request(url, "PUT", &format!("/api/albums/{album_id}/assets"), &auth, Some(&json!({ "ids": ids })))?)?;
    println!("seeded {} fixture(s) into album \"{ALBUM}\"", ids.len());
    Ok(())
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Write the key readable by the owner only (where the platform supports it).
fn write_secret(path: &Path, secret: &str) -> Result<(), String> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| format!("write {}: {e}", path.display()))?;
    writeln!(f, "{secret}").map_err(|e| format!("write {}: {e}", path.display()))
}

/// Multipart upload (`POST /api/assets`); a duplicate (same checksum) is answered with `status: duplicate`.
fn upload(url: &str, auth: &Auth, path: &Path, time: &str) -> Result<(u16, Value), String> {
    let mut c = curl(url, "POST", "/api/assets", auth);
    c.arg("-F").arg(format!("assetData=@{};type=image/png", path.display()));
    c.arg("-F").arg(format!("fileCreatedAt={time}"));
    c.arg("-F").arg(format!("fileModifiedAt={time}"));
    c.arg("-F").arg(format!("filename={}", file_name(path)));
    finish(c, &format!("upload {}", path.display()))
}

/// Capture time of fixture `i`: one per day from 2024-01-01, so the timeline has distinct buckets.
pub fn fixture_time(i: usize) -> String {
    format!("2024-01-{:02}T12:00:00.000Z", (i % 28) + 1)
}

fn write_fixtures(dir: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    (0..FIXTURES)
        .map(|i| {
            let path = dir.join(format!("fixture-{i:02}.png"));
            std::fs::write(&path, fixture_png(i)?).map_err(|e| format!("write {}: {e}", path.display()))?;
            Ok(path)
        })
        .collect()
}

/// Fixture `i`: a 64×48 RGB gradient with its own hue, so every file has a distinct checksum.
pub fn fixture_png(i: usize) -> Result<Vec<u8>, String> {
    let (w, h) = (64u32, 48u32);
    let hue = (i as f32 * 360.0 / FIXTURES as f32) % 360.0;
    let base = hue_rgb(hue);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = 0.35 + 0.65 * (x as f32 / (w - 1) as f32);
            let s = 0.5 + 0.5 * (y as f32 / (h - 1) as f32);
            for c in base {
                rgb.push((c * t * s * 255.0).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    png_rgb8(w, h, &rgb)
}

fn hue_rgb(hue: f32) -> [f32; 3] {
    let f = |n: f32| {
        let k = (n + hue / 60.0) % 6.0;
        1.0 - (k.min(4.0 - k).clamp(0.0, 1.0))
    };
    [f(5.0), f(3.0), f(1.0)]
}

/// CRC-32 (ISO-HDLC, as PNG uses), bitwise.
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for b in parts.iter().flat_map(|p| p.iter()) {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), String> {
    let len = u32::try_from(data.len()).map_err(|_| "PNG chunk too large".to_string())?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    Ok(())
}

/// Minimal 8-bit RGB PNG (filter 0 on every row, zlib from `flate2`).
pub fn png_rgb8(w: u32, h: u32, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let row = w as usize * 3;
    if w == 0 || h == 0 || rgb.len() != row * h as usize {
        return Err(format!("png: {} bytes for {w}×{h} RGB", rgb.len()));
    }
    let mut raw = Vec::with_capacity((row + 1) * h as usize);
    for line in rgb.chunks_exact(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(&raw).map_err(|e| format!("png: {e}"))?;
    let idat = z.finish().map_err(|e| format!("png: {e}"))?;
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour, deflate, adaptive filtering, no interlace
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr)?;
    chunk(&mut out, b"IDAT", &idat)?;
    chunk(&mut out, b"IEND", &[])?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    #[test]
    fn base_url_defaults_and_trims() {
        assert_eq!(base_url(None), DEFAULT_URL);
        assert_eq!(base_url(Some("  ")), DEFAULT_URL);
        assert_eq!(base_url(Some("https://photos.example.invalid/")), "https://photos.example.invalid");
    }

    #[test]
    fn splits_status_from_body() {
        let (code, body) = split_status(&format!("{{\"res\":\"pong\"}}{STATUS_MARK}200")).unwrap();
        assert_eq!(code, 200);
        assert_eq!(body["res"], "pong");
        let (code, body) = split_status(&format!("{STATUS_MARK}204")).unwrap();
        assert_eq!((code, body), (204, Value::Null));
        assert_eq!(split_status("<html>").map(|r| r.0), None);
        assert_eq!(split_status(&format!("x{STATUS_MARK}abc")).map(|r| r.0), None);
    }

    #[test]
    fn crc_matches_the_reference_value() {
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082);
    }

    #[test]
    fn png_is_well_formed_and_round_trips() {
        let png = fixture_png(3).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 64);
        assert_eq!(u32::from_be_bytes([png[20], png[21], png[22], png[23]]), 48);
        assert!(png.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]));
        // IDAT inflates to 48 rows of filter byte + 64 RGB pixels
        let idat_len = u32::from_be_bytes([png[33], png[34], png[35], png[36]]) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&png[41..41 + idat_len]).read_to_end(&mut raw).unwrap();
        assert_eq!(raw.len(), 48 * (1 + 64 * 3));
        assert!(raw.chunks(1 + 64 * 3).all(|r| r[0] == 0));
    }

    #[test]
    fn fixtures_are_distinct() {
        let all: Vec<Vec<u8>> = (0..FIXTURES).map(|i| fixture_png(i).unwrap()).collect();
        for (i, a) in all.iter().enumerate() {
            assert!(all.iter().skip(i + 1).all(|b| b != a), "fixture {i} repeats");
        }
        assert!(png_rgb8(2, 2, &[0; 3]).is_err());
    }

    #[test]
    fn fixture_times_are_valid_dates() {
        assert_eq!(fixture_time(0), "2024-01-01T12:00:00.000Z");
        assert_eq!(fixture_time(27), "2024-01-28T12:00:00.000Z");
        assert_eq!(fixture_time(28), "2024-01-01T12:00:00.000Z");
    }
}
