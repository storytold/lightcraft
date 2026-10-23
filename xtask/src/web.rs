//! `cargo xtask web`: build `apps/lightcraft-web` for wasm32 and bundle it with `wasm-bindgen`
//! into `<target>/web-site/` (index.html + worker.js + lightcraft_web.js + lightcraft_web_bg.wasm),
//! with gzip and brotli precompressed copies (`*.gz`, `*.br`) next to each file.
//! `--serve [port]` then serves that folder with a tiny static HTTP server (std only) that sends
//! the cross-origin isolation headers (COOP/COEP) and the precompressed files when the browser
//! accepts them.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{cargo, metadata, root, run as step};

const TARGET: &str = "wasm32-unknown-unknown";
/// Cargo profile for the optimized (`--dev` off) browser build; see `[profile.web]` in `Cargo.toml`.
const WEB_PROFILE: &str = "web";
/// Directory under `<target>` that receives the bundle. It must **not** be a Cargo profile name:
/// cross-compiling with `--target wasm32-unknown-unknown` makes Cargo write the selected profile's
/// *host* artifacts (build scripts, proc-macros, `deps/`, `build/`, `.fingerprint/`) to
/// `<target>/<profile>/`, and `packaging/web/package.sh` zips the whole bundle directory (issue #793).
const SITE_DIR: &str = "web-site";

/// Files copied from `apps/lightcraft-web/` into the bundle as they are.
const STATIC_FILES: [&str; 2] = ["index.html", "worker.js"];

/// Bundle files that get precompressed copies (the web fonts get them too).
const COMPRESSED: [&str; 4] = ["index.html", "worker.js", "lightcraft_web.js", "lightcraft_web_bg.wasm"];

/// Cloudflare Pages' hard limit per file (25 MiB): a release bundle with a larger file can't be deployed there.
const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;

/// Where the web fonts go in the bundle; `index.html` reads `fonts/manifest.json` from here.
const FONTS_DIR: &str = "fonts";

/// The craft-fonts faces the web build ships, as (family, style, required). They go beside the
/// WASM rather than into it: Cloudflare Pages serves at most 25 MiB per file and the module alone
/// is close to that, while the site as a whole may be larger. `index.html` fetches them and hands
/// them to the app before it starts. A face that is not required ships when the craft-fonts
/// checkout has it.
const WEB_FACES: [(&str, &str, bool); 4] = [
    ("BIZ UDPGothic", "Regular", true),
    ("Noto Sans CJK SC", "Regular", true),
    ("Noto Sans Thai", "Regular", true),
    ("Noto Sans Georgian", "Regular", false),
];

/// A [`WEB_FACES`] entry as the craft-fonts manifest lists it.
#[derive(Debug, PartialEq)]
struct WebFace<'a> {
    family: &'a str,
    style: &'a str,
    file: &'a str,
    scripts: Vec<&'a str>,
    licence: &'a str,
}

/// The [`WEB_FACES`] in a craft-fonts `fonts/manifest.txt`, in that order; a missing required face
/// is an error.
fn web_faces_in_manifest(manifest: &str) -> Result<Vec<WebFace<'_>>, String> {
    let lines: Vec<Vec<&str>> =
        manifest.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(|l| l.split('|').map(str::trim).collect()).collect();
    let mut faces = Vec::new();
    for (family, style, required) in WEB_FACES {
        let found = lines.iter().find(|f| f.len() >= 6 && f[0] == family && f[1] == style && !f[2].is_empty() && !f[5].is_empty());
        match found {
            Some(f) => {
                for path in [f[2], f[5]] {
                    if !Path::new(path).starts_with(FONTS_DIR) || path.starts_with('/') || path.split('/').any(|c| c == "..") {
                        return Err(format!("{family} {style}: {path} leaves the craft-fonts checkout"));
                    }
                }
                faces.push(WebFace { family, style, file: f[2], scripts: f[3].split(',').map(str::trim).collect(), licence: f[5] });
            }
            None if required => return Err(format!("{family} {style} is missing")),
            None => println!("web fonts: {family} {style} is not in this craft-fonts checkout; skipped"),
        }
    }
    Ok(faces)
}

/// Copy the [`WEB_FACES`] and their licences into `out/fonts/` (craft-fonts' own layout) and write
/// `out/fonts/manifest.json`; returns the font files' bundle paths. Without `CRAFT_FONTS_DIR`
/// nothing is shipped and the web build runs without them.
fn copy_web_fonts(out: &Path) -> Result<Vec<String>, String> {
    // A previous font-enabled build must not leave an unlicensed/stale asset in a font-free one
    // (`lightcraft_zh_hans.otf` and its licence: the layout before `fonts/`).
    let fonts_dir = out.join(FONTS_DIR);
    match std::fs::remove_dir_all(&fonts_dir) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(format!("{}: {e}", fonts_dir.display())),
    }
    for name in ["lightcraft_zh_hans.otf", "lightcraft_zh_hans.otf.gz", "lightcraft_zh_hans.otf.br", "OFL-noto-sans-cjk-sc.txt"] {
        let _ = std::fs::remove_file(out.join(name));
    }
    let Some(dir) = std::env::var_os("CRAFT_FONTS_DIR") else {
        if std::env::var_os("CRAFT_FONTS_REQUIRED").is_some() {
            return Err("CRAFT_FONTS_REQUIRED is set but CRAFT_FONTS_DIR is missing".into());
        }
        println!("web fonts: no CRAFT_FONTS_DIR, so CJK, Thai and Georgian text will show as boxes");
        return Ok(Vec::new());
    };
    let dir = PathBuf::from(dir);
    let dir = if dir.is_absolute() { dir } else { root().join(dir) };
    let manifest_path = dir.join("fonts/manifest.txt");
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let faces = web_faces_in_manifest(&manifest).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let mut list = Vec::new();
    for face in &faces {
        for path in [face.file, face.licence] {
            let (source, target) = (dir.join(path), out.join(path));
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::copy(&source, &target).map_err(|e| format!("{} → {}: {e}", source.display(), target.display()))?;
        }
        println!("web font: {} {} → {}", face.family, face.style, out.join(face.file).display());
        list.push(serde_json::json!({"family": face.family, "style": face.style, "scripts": face.scripts, "file": face.file}));
    }
    std::fs::create_dir_all(&fonts_dir).map_err(|e| format!("{}: {e}", fonts_dir.display()))?;
    let json = fonts_dir.join("manifest.json");
    std::fs::write(&json, serde_json::to_string_pretty(&list).unwrap_or_default()).map_err(|e| format!("{}: {e}", json.display()))?;
    Ok(faces.iter().map(|face| face.file.to_string()).collect())
}

/// The `wasm-bindgen` version pinned in Cargo.lock (the CLI must match it exactly).
fn locked_bindgen_version() -> Result<String, String> {
    let lock = std::fs::read_to_string(root().join("Cargo.lock")).map_err(|e| format!("Cargo.lock: {e}"))?;
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "name = \"wasm-bindgen\""
            && let Some(v) = lines.next().and_then(|v| v.trim().strip_prefix("version = \""))
        {
            return Ok(v.trim_end_matches('"').to_string());
        }
    }
    Err("wasm-bindgen not found in Cargo.lock".into())
}

fn check_bindgen(want: &str) -> Result<(), String> {
    let hint = format!("install it with:\n    cargo install wasm-bindgen-cli --version {want} --locked");
    let out = Command::new("wasm-bindgen").arg("--version").output().map_err(|_| format!("`wasm-bindgen` CLI not found; {hint}"))?;
    let have = String::from_utf8_lossy(&out.stdout);
    let have = have.split_whitespace().nth(1).unwrap_or("?");
    if have != want {
        return Err(format!("wasm-bindgen CLI is {have} but Cargo.lock has {want}; {hint}"));
    }
    Ok(())
}

pub fn run(args: &[&str]) -> Result<(), String> {
    let dev = args.contains(&"--dev");
    let serve = args.iter().position(|a| *a == "--serve").map(|i| args.get(i + 1).and_then(|p| p.parse::<u16>().ok()).unwrap_or(8080));

    let want = locked_bindgen_version()?;
    check_bindgen(&want)?;
    let meta = metadata()?;
    let target_dir = PathBuf::from(meta["target_directory"].as_str().ok_or("cargo metadata: no target_directory")?);
    let profile = if dev { "dev" } else { WEB_PROFILE };

    let mut c = cargo();
    c.args(["build", "-p", "lightcraft-web", "--lib", "--target", TARGET, "--profile", profile]);
    step(c, &format!("cargo build -p lightcraft-web --target {TARGET} --profile {profile}"))?;

    let wasm = target_dir.join(TARGET).join(if dev { "debug" } else { profile }).join("lightcraft_web.wasm");
    let out = target_dir.join(SITE_DIR);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut b = Command::new("wasm-bindgen");
    b.arg(&wasm).args(["--target", "web", "--no-typescript", "--out-name", "lightcraft_web", "--out-dir"]).arg(&out);
    if dev {
        b.arg("--debug");
    }
    step(b, &format!("wasm-bindgen {} → {}", wasm.display(), out.display()))?;

    // optional size pass when binaryen is installed
    let bg = out.join("lightcraft_web_bg.wasm");
    if !dev && Command::new("wasm-opt").arg("--version").output().is_ok() {
        let mut o = Command::new("wasm-opt");
        o.args(["-O2", "--enable-bulk-memory", "--enable-nontrapping-float-to-int", "--enable-sign-ext", "--enable-mutable-globals"])
            .arg(&bg)
            .arg("-o")
            .arg(&bg);
        if step(o, "wasm-opt -O2").is_err() {
            eprintln!("(wasm-opt failed; keeping the unoptimized module)");
        }
    }
    for f in STATIC_FILES {
        std::fs::copy(root().join("apps/lightcraft-web").join(f), out.join(f)).map_err(|e| format!("copy {f}: {e}"))?;
    }
    let fonts = copy_web_fonts(&out)?;

    // precompressed copies (skipped for --dev: they'd only slow the edit loop down)
    println!("\n{:<50} {:>12} {:>12} {:>12}", "file", "bytes", "gzip -9", "brotli -11");
    for f in COMPRESSED.into_iter().chain(fonts.iter().map(String::as_str)) {
        let p = out.join(f);
        let raw = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let (gz, br) = if dev {
            for ext in ["gz", "br"] {
                let _ = std::fs::remove_file(out.join(format!("{f}.{ext}")));
            }
            (None, None)
        } else {
            let gz = gzip(&raw)?;
            let br = brotli(&raw)?;
            std::fs::write(out.join(format!("{f}.gz")), &gz).map_err(|e| format!("{f}.gz: {e}"))?;
            std::fs::write(out.join(format!("{f}.br")), &br).map_err(|e| format!("{f}.br: {e}"))?;
            (Some(gz.len()), Some(br.len()))
        };
        let show = |n: Option<usize>| n.map_or("-".to_string(), |n| n.to_string());
        println!("{f:<50} {:>12} {:>12} {:>12}", raw.len(), show(gz), show(br));
        if !dev && raw.len() > MAX_FILE_BYTES {
            return Err(format!(
                "{f} is {} bytes, over the {MAX_FILE_BYTES}-byte (25 MiB) per-file limit of Cloudflare Pages (docs/web.md)",
                raw.len()
            ));
        }
    }
    let size = std::fs::metadata(&bg).map(|m| m.len()).unwrap_or(0);
    println!("\nweb build ready: {} ({:.1} MB wasm)", out.display(), size as f64 / 1e6);

    match serve {
        Some(port) => serve_dir(&out, port),
        None => {
            println!("serve it with `cargo xtask web --serve` (or any static HTTP server; see docs/web.md) and open http://127.0.0.1:8080/");
            Ok(())
        }
    }
}

fn gzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).and_then(|_| e.finish()).map_err(|e| format!("gzip: {e}"))
}

fn brotli(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let params = ::brotli::enc::BrotliEncoderParams { quality: 11, lgwin: 24, ..Default::default() };
    ::brotli::BrotliCompress(&mut &data[..], &mut out, &params).map_err(|e| format!("brotli: {e}"))?;
    Ok(out)
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "otf" => "font/otf",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// Response headers every file gets. `Cross-Origin-Opener-Policy: same-origin` +
/// `Cross-Origin-Embedder-Policy: require-corp` make the page cross-origin isolated
/// (`crossOriginIsolated == true`): required for `SharedArrayBuffer` (a wasm-threads build) and
/// for full-resolution `performance.now()`. The current worker design (separate wasm instances)
/// works without them; they are sent so the dev server matches the recommended deployment.
pub const ISOLATION_HEADERS: [(&str, &str); 3] = [
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("Cross-Origin-Embedder-Policy", "require-corp"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
];

/// Pick the precompressed variant to send: (`Content-Encoding`, file suffix).
fn negotiate(accept_encoding: &str, has: impl Fn(&str) -> bool) -> Option<(&'static str, &'static str)> {
    let accepts = |enc: &str| accept_encoding.split(',').any(|e| e.split(';').next().is_some_and(|n| n.trim().eq_ignore_ascii_case(enc)));
    [("br", ".br"), ("gzip", ".gz")].into_iter().find(|(enc, suffix)| accepts(enc) && has(suffix))
}

/// Serve `dir` on 127.0.0.1:`port` (GET/HEAD only, no directory listing). Blocks.
fn serve_dir(dir: &Path, port: u16) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    println!("serving {} at http://127.0.0.1:{port}/  (Ctrl+C to stop)", dir.display());
    for stream in listener.incoming().flatten() {
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, &dir) {
                eprintln!("http: {e}");
            }
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, dir: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut accept_encoding = String::new();
    let mut h = String::new();
    while reader.read_line(&mut h)? > 2 {
        if let Some((k, v)) = h.split_once(':')
            && k.trim().eq_ignore_ascii_case("accept-encoding")
        {
            accept_encoding = v.trim().to_string();
        }
        h.clear();
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let file = dir.join(rel);
    let safe = !rel.split('/').any(|c| c == ".." || c.contains('\\'));
    let encoding = if safe { negotiate(&accept_encoding, |suffix| dir.join(format!("{rel}{suffix}")).is_file()) } else { None };
    let read = match encoding {
        Some((_, suffix)) => std::fs::read(dir.join(format!("{rel}{suffix}"))),
        None => std::fs::read(&file),
    };
    let (status, body, ctype) = match (method, safe, read) {
        ("GET" | "HEAD", true, Ok(b)) => ("200 OK", b, mime(&file)),
        ("GET" | "HEAD", _, _) => ("404 Not Found", b"not found".to_vec(), "text/plain"),
        _ => ("405 Method Not Allowed", Vec::new(), "text/plain"),
    };
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nVary: Accept-Encoding\r\n",
        body.len()
    );
    if status.starts_with("200")
        && let Some((enc, _)) = encoding
    {
        head.push_str(&format!("Content-Encoding: {enc}\r\n"));
    }
    for (k, v) in ISOLATION_HEADERS {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    if method != "HEAD" {
        stream.write_all(&body)?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindgen_version_is_locked() {
        let v = locked_bindgen_version().unwrap();
        assert!(v.starts_with("0.2."), "{v}");
    }

    #[test]
    fn site_dir_is_not_a_cargo_profile() {
        // Cross-compiling writes the selected profile's *host* artifacts (build scripts,
        // proc-macros, `deps/`, `build/`, `.fingerprint/`) to `<target>/<profile>/`, so the bundle
        // must not live in a profile directory or `packaging/web/package.sh` would zip them
        // (issue #793).
        let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
        let is_profile = |name: &str| {
            let header = format!("[profile.{name}]");
            manifest.lines().any(|l| l.trim() == header)
        };
        assert!(is_profile(WEB_PROFILE), "`{WEB_PROFILE}` is no longer a profile in Cargo.toml");
        assert!(!is_profile(SITE_DIR), "`{SITE_DIR}` is a Cargo profile name; pick a different bundle directory");
    }

    #[test]
    fn wasm_mime() {
        assert_eq!(mime(Path::new("a/lightcraft_web_bg.wasm")), "application/wasm");
        assert_eq!(mime(Path::new("index.html")), "text/html; charset=utf-8");
        assert_eq!(mime(Path::new("worker.js")), "text/javascript; charset=utf-8");
        assert_eq!(mime(Path::new("fonts/noto-sans-cjk-sc/NotoSansCJKsc-Regular.otf")), "font/otf");
        assert_eq!(mime(Path::new("fonts/noto-sans-georgian/NotoSansGeorgian.ttf")), "font/ttf");
        assert_eq!(mime(Path::new("fonts/manifest.json")), "application/json");
    }

    #[test]
    fn web_bundle_selects_its_faces_from_the_font_manifest() {
        let manifest = "# family | style | path | scripts | licence | licence file\n\
            BIZ UDPGothic | Bold | fonts/biz/Bold.ttf | Jpan,Latn | OFL-1.1 | fonts/biz/OFL.txt\n\
            Noto Sans Thai | Regular | fonts/thai/NotoSansThai.ttf | Thai | OFL-1.1 | fonts/thai/OFL.txt\n\
            BIZ UDPGothic | Regular | fonts/biz/Regular.ttf | Jpan,Latn | OFL-1.1 | fonts/biz/OFL.txt\n\
            Noto Sans CJK SC | Regular | fonts/noto/NotoSansCJKsc-Regular.otf | Hans,Latn | OFL-1.1 | fonts/noto/OFL.txt\n";
        let faces = web_faces_in_manifest(manifest).unwrap();
        let files: Vec<_> = faces.iter().map(|f| f.file).collect();
        // In WEB_FACES order; Georgian is optional (craft-fonts before a71c20e3 has none).
        assert_eq!(files, ["fonts/biz/Regular.ttf", "fonts/noto/NotoSansCJKsc-Regular.otf", "fonts/thai/NotoSansThai.ttf"]);
        assert_eq!(faces[0].scripts, ["Jpan", "Latn"]);
        assert_eq!(faces[0].licence, "fonts/biz/OFL.txt");
        let georgian = format!("{manifest}Noto Sans Georgian | Regular | fonts/geo/NotoSansGeorgian.ttf | Geor | OFL-1.1 | fonts/geo/OFL.txt\n");
        assert_eq!(
            web_faces_in_manifest(&georgian).unwrap().last().map(|f| (f.family, f.scripts.clone())),
            Some(("Noto Sans Georgian", vec!["Geor"]))
        );
        // A required face missing, or a path out of the checkout, fails the build.
        let no_thai: String = manifest.lines().filter(|l| !l.contains("Thai")).map(|l| format!("{l}\n")).collect();
        assert!(web_faces_in_manifest(&no_thai).unwrap_err().contains("Noto Sans Thai Regular is missing"));
        let escape = manifest.replace("fonts/thai/NotoSansThai.ttf", "../NotoSansThai.ttf");
        assert!(web_faces_in_manifest(&escape).is_err());
        let root = manifest.replace("fonts/thai/NotoSansThai.ttf", "index.html");
        assert!(web_faces_in_manifest(&root).is_err());
    }

    /// The page fetches the same manifest name the bundle writes, and passes it to `start`.
    #[test]
    fn index_html_loads_the_web_fonts() {
        let html = std::fs::read_to_string(root().join("apps/lightcraft-web/index.html")).unwrap();
        assert!(html.contains(&format!("./{FONTS_DIR}/manifest.json")), "index.html must fetch {FONTS_DIR}/manifest.json");
        assert!(html.contains("start(fontManifest, fontFiles)"));
    }

    #[test]
    fn encoding_negotiation() {
        let all = |_: &str| true;
        assert_eq!(negotiate("gzip, deflate, br, zstd", all), Some(("br", ".br")));
        assert_eq!(negotiate("gzip;q=1.0", all), Some(("gzip", ".gz")));
        assert_eq!(negotiate("gzip, br", |s| s == ".gz"), Some(("gzip", ".gz")));
        assert_eq!(negotiate("", all), None);
        assert_eq!(negotiate("identity", all), None);
    }

    #[test]
    fn compressors_round_trip_sizes() {
        let data: Vec<u8> = (0..20_000u32).flat_map(|i| (i % 97).to_le_bytes()).collect();
        let gz = gzip(&data).unwrap();
        let br = brotli(&data).unwrap();
        assert!(gz.len() < data.len() / 4 && br.len() < data.len() / 4, "{} {} {}", data.len(), gz.len(), br.len());
        assert_eq!(&gz[..2], &[0x1f, 0x8b], "gzip magic");
        let mut back = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&gz[..]), &mut back).unwrap();
        assert_eq!(back, data);
    }
}
