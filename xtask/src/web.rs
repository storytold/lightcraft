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
const CHINESE_FONT_FILE: &str = "lightcraft_zh_hans.otf";
const CHINESE_FONT_LICENSE_FILE: &str = "OFL-noto-sans-cjk-sc.txt";

/// The faces embedded by `crates/engine/build.rs` on wasm32. Keep their notices beside the
/// module even when the site is served directly, without `packaging/web/package.sh`.
/// The last field matches the build script's required-face checks; Georgian is optional.
const EMBEDDED_FONT_LICENCES: &[(&str, &str, bool)] = &[
    ("BIZ UDPGothic", "OFL-biz-ud-pgothic.txt", true),
    ("Noto Sans Thai", "OFL-noto-sans-thai.txt", true),
    ("Noto Sans Georgian", "OFL-noto-sans-georgian.txt", false),
    ("NanumGothic", "OFL-nanum-gothic.txt", true),
];

/// Files copied from `apps/lightcraft-web/` into the bundle as they are.
const STATIC_FILES: [&str; 2] = ["index.html", "worker.js"];

/// Bundle files that get precompressed copies.
const COMPRESSED: [&str; 4] = ["index.html", "worker.js", "lightcraft_web.js", "lightcraft_web_bg.wasm"];

/// The Noto face is shipped beside the WASM rather than embedded in it: hosts such as Cloudflare
/// Pages cap individual files at 25 MiB, while the complete site can contain larger assets.
fn chinese_font_in_manifest(manifest: &str) -> Option<(&str, &str)> {
    manifest.lines().find_map(|line| {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() >= 6 && fields[0] == "Noto Sans CJK SC" && fields[1] == "Regular" && !fields[2].is_empty() && !fields[5].is_empty() {
            Some((fields[2], fields[5]))
        } else {
            None
        }
    })
}

fn copy_chinese_font(out: &Path) -> Result<bool, String> {
    // A previous font-enabled build must not leave an unlicensed/stale asset in a font-free one.
    for name in [CHINESE_FONT_FILE, CHINESE_FONT_LICENSE_FILE, "lightcraft_zh_hans.otf.gz", "lightcraft_zh_hans.otf.br"] {
        let path = out.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    let Some(dir) = std::env::var_os("CRAFT_FONTS_DIR") else {
        if std::env::var_os("CRAFT_FONTS_REQUIRED").is_some() {
            return Err("CRAFT_FONTS_REQUIRED is set but CRAFT_FONTS_DIR is missing".into());
        }
        return Ok(false);
    };
    let dir = PathBuf::from(dir);
    let dir = if dir.is_absolute() { dir } else { root().join(dir) };
    let manifest_path = dir.join("fonts/manifest.txt");
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let (font, licence) =
        chinese_font_in_manifest(&manifest).ok_or_else(|| format!("{}: Noto Sans CJK SC Regular is missing", manifest_path.display()))?;
    let licence = dir.join(licence);
    let licence_target = out.join(CHINESE_FONT_LICENSE_FILE);
    std::fs::copy(&licence, &licence_target).map_err(|e| format!("{} → {}: {e}", licence.display(), licence_target.display()))?;
    let source = dir.join(font);
    let target = out.join(CHINESE_FONT_FILE);
    std::fs::copy(&source, &target).map_err(|e| format!("{} → {}: {e}", source.display(), target.display()))?;
    println!("web Chinese font: {} → {}", source.display(), target.display());
    Ok(true)
}

fn embedded_font_licence<'a>(manifest: &'a str, family: &str) -> Result<Option<&'a str>, String> {
    for line in manifest.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        let [found_family, style, ..] = fields.as_slice() else { continue };
        if *found_family != family || *style != "Regular" {
            continue;
        }
        let [_, _, _, _, licence, file, ..] = fields.as_slice() else {
            return Err(format!("{family} Regular has no licence entry in fonts/manifest.txt"));
        };
        if *licence != "OFL-1.1" || file.is_empty() {
            return Err(format!("{family} Regular needs an OFL-1.1 licence file in fonts/manifest.txt"));
        }
        return Ok(Some(file));
    }
    Ok(None)
}

/// Read first, then write: a missing notice must fail the build, not quietly reuse an old
/// notice from a previous bundle. Copy the bytes verbatim, including the copyright notice.
fn copy_embedded_font_licences(out: &Path, repo: &Path, craft: Option<&Path>, required: bool) -> Result<(), String> {
    let inter = repo.join("assets/fonts/OFL-Inter.txt");
    let inter_bytes = std::fs::read(&inter).map_err(|e| format!("{}: {e}", inter.display()))?;
    if inter_bytes.is_empty() {
        return Err(format!("{}: font licence is empty", inter.display()));
    }
    let mut notices = vec![("OFL-Inter.txt", inter_bytes)];
    if let Some(craft) = craft {
        let craft = craft.canonicalize().map_err(|e| format!("{}: {e}", craft.display()))?;
        let manifest_path = craft.join("fonts/manifest.txt");
        let manifest = std::fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        for &(family, name, mandatory) in EMBEDDED_FONT_LICENCES {
            let Some(file) = embedded_font_licence(&manifest, family)? else {
                if required && mandatory {
                    return Err(format!("required web font {family} Regular is missing from {}", manifest_path.display()));
                }
                continue;
            };
            let source = craft.join(file).canonicalize().map_err(|e| format!("{family} licence {file}: {e}"))?;
            if !source.starts_with(&craft) {
                return Err(format!("{family} licence {file} is outside CRAFT_FONTS_DIR"));
            }
            let bytes = std::fs::read(&source).map_err(|e| format!("{}: {e}", source.display()))?;
            if bytes.is_empty() {
                return Err(format!("{}: font licence is empty", source.display()));
            }
            notices.push((name, bytes));
        }
    } else if required {
        return Err("CRAFT_FONTS_REQUIRED is set but CRAFT_FONTS_DIR is missing".into());
    }
    for (name, bytes) in notices {
        let target = out.join(name);
        std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    Ok(())
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
    let craft = std::env::var_os("CRAFT_FONTS_DIR").map(PathBuf::from).map(|dir| if dir.is_absolute() { dir } else { root().join(dir) });
    copy_embedded_font_licences(&out, &root(), craft.as_deref(), std::env::var_os("CRAFT_FONTS_REQUIRED").is_some())?;
    let chinese_font = copy_chinese_font(&out)?;

    // precompressed copies (skipped for --dev: they'd only slow the edit loop down)
    println!("\n{:<26} {:>12} {:>12} {:>12}", "file", "bytes", "gzip -9", "brotli -11");
    for f in COMPRESSED.into_iter().chain(chinese_font.then_some(CHINESE_FONT_FILE)) {
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
        println!("{f:<26} {:>12} {:>12} {:>12}", raw.len(), show(gz), show(br));
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
        assert_eq!(mime(Path::new(CHINESE_FONT_FILE)), "font/otf");
    }

    #[test]
    fn web_bundle_selects_the_chinese_face_from_the_font_manifest() {
        let manifest = "# family | style | path | scripts | licence | licence file\n\
            BIZ UDPGothic | Regular | fonts/biz.ttf | Jpan,Latn | OFL-1.1 | fonts/biz/OFL.txt\n\
            Noto Sans CJK SC | Regular | fonts/noto/NotoSansCJKsc-Regular.otf | Hans,Latn | OFL-1.1 | fonts/noto/OFL.txt\n";
        assert_eq!(chinese_font_in_manifest(manifest), Some(("fonts/noto/NotoSansCJKsc-Regular.otf", "fonts/noto/OFL.txt")));
        assert_eq!(chinese_font_in_manifest("BIZ UDPGothic | Regular | fonts/biz.ttf | Jpan,Latn | OFL-1.1 | fonts/biz/OFL.txt"), None);
    }

    fn font_notice_fixture(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, String) {
        let dir = tempfile::Builder::new().prefix(&format!("lightcraft-web-font-notice-{name}-")).tempdir().unwrap();
        let repo = dir.path().join("repo");
        let craft = dir.path().join("craft");
        let out = dir.path().join("site");
        std::fs::create_dir_all(repo.join("assets/fonts")).unwrap();
        std::fs::create_dir_all(craft.join("fonts")).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(repo.join("assets/fonts/OFL-Inter.txt"), b"Inter copyright\r\nOFL text\r\n").unwrap();
        let manifest = EMBEDDED_FONT_LICENCES
            .iter()
            .map(|(family, name, _)| format!("{family} | Regular | unused.ttf | Kore | OFL-1.1 | fonts/{name}\n"))
            .collect::<String>();
        std::fs::write(craft.join("fonts/manifest.txt"), &manifest).unwrap();
        (dir, repo, craft, out, manifest)
    }

    #[test]
    fn font_notice_fixtures_do_not_inherit_inputs_or_outputs() {
        let (old_dir, _, old_craft, old_out, _) = font_notice_fixture("isolation");
        std::fs::write(old_craft.join("fonts/OFL-nanum-gothic.txt"), b"previous source notice").unwrap();
        std::fs::write(old_out.join("OFL-Inter.txt"), b"previous bundle notice").unwrap();

        let (fresh_dir, _, fresh_craft, fresh_out, _) = font_notice_fixture("isolation");
        assert_ne!(old_dir.path(), fresh_dir.path());
        assert!(!fresh_craft.join("fonts/OFL-nanum-gothic.txt").exists(), "a fresh fixture must not inherit source notices");
        assert!(!fresh_out.join("OFL-Inter.txt").exists(), "a fresh fixture must not inherit bundle output");

        let old_path = old_dir.path().to_path_buf();
        let fresh_path = fresh_dir.path().to_path_buf();
        drop(old_dir);
        assert!(!old_path.exists(), "the guard cleans up its own fixture");
        assert!(fresh_path.is_dir(), "another live fixture must remain intact");
        drop(fresh_dir);
        assert!(!fresh_path.exists(), "a fixture must not persist after its guard is dropped");
    }

    #[test]
    fn web_bundle_keeps_embedded_font_copyright_and_licence_bytes() {
        let (_dir, repo, craft, out, _) = font_notice_fixture("verbatim");
        for &(family, name, _) in EMBEDDED_FONT_LICENCES {
            let bytes = format!("Copyright {family}\r\nSIL OFL 1.1\r\nReserved names remain unchanged.\r\n");
            std::fs::write(craft.join("fonts").join(name), bytes).unwrap();
        }
        copy_embedded_font_licences(&out, &repo, Some(&craft), true).unwrap();
        assert_eq!(std::fs::read(out.join("OFL-Inter.txt")).unwrap(), std::fs::read(repo.join("assets/fonts/OFL-Inter.txt")).unwrap());
        for &(_, name, _) in EMBEDDED_FONT_LICENCES {
            assert_eq!(std::fs::read(out.join(name)).unwrap(), std::fs::read(craft.join("fonts").join(name)).unwrap(), "{name}");
        }
    }

    #[test]
    fn a_missing_korean_notice_fails_instead_of_reusing_the_previous_bundles_notice() {
        let (_dir, repo, craft, out, _) = font_notice_fixture("missing-korean");
        for &(family, name, _) in EMBEDDED_FONT_LICENCES {
            if family != "NanumGothic" {
                std::fs::write(craft.join("fonts").join(name), b"copyright and licence").unwrap();
            }
        }
        std::fs::write(out.join("OFL-nanum-gothic.txt"), b"old bundle notice").unwrap();
        // A selected font needs its notice even in an optional-font build.
        for required in [false, true] {
            let error = copy_embedded_font_licences(&out, &repo, Some(&craft), required).unwrap_err();
            assert!(error.contains("NanumGothic licence"), "{error}");
        }
        std::fs::write(craft.join("fonts/OFL-nanum-gothic.txt"), b"").unwrap();
        assert!(copy_embedded_font_licences(&out, &repo, Some(&craft), true).unwrap_err().contains("font licence is empty"));
        assert_eq!(std::fs::read(out.join("OFL-nanum-gothic.txt")).unwrap(), b"old bundle notice");
        assert!(!out.join("OFL-Inter.txt").exists(), "validation happens before notices are published");
    }

    #[test]
    fn required_font_notices_cannot_be_silently_omitted() {
        let (_dir, repo, craft, out, manifest) = font_notice_fixture("required");
        let without_korean = manifest.lines().filter(|line| !line.starts_with("NanumGothic | ")).collect::<Vec<_>>().join("\n");
        std::fs::write(craft.join("fonts/manifest.txt"), without_korean).unwrap();
        for &(family, name, _) in EMBEDDED_FONT_LICENCES {
            if family != "NanumGothic" {
                std::fs::write(craft.join("fonts").join(name), b"copyright and licence").unwrap();
            }
        }
        let error = copy_embedded_font_licences(&out, &repo, Some(&craft), true).unwrap_err();
        assert!(error.contains("required web font NanumGothic Regular is missing"), "{error}");
        assert!(copy_embedded_font_licences(&out, &repo, None, true).unwrap_err().contains("CRAFT_FONTS_DIR is missing"));
        copy_embedded_font_licences(&out, &repo, None, false).unwrap();
        assert!(out.join("OFL-Inter.txt").is_file(), "font-free bundles still embed Inter");
        for line in ["NanumGothic | Regular | font.ttf | Kore", "NanumGothic | Regular | font.ttf | Kore | OFL-1.1 | "] {
            assert!(embedded_font_licence(line, "NanumGothic").is_err(), "{line}");
        }
        // Keep the engine build script's existing optional Georgian-face behavior.
        let without_georgian = manifest.lines().filter(|line| !line.starts_with("Noto Sans Georgian | ")).collect::<Vec<_>>().join("\n");
        std::fs::write(craft.join("fonts/manifest.txt"), without_georgian).unwrap();
        std::fs::write(craft.join("fonts/OFL-nanum-gothic.txt"), b"Korean copyright and licence").unwrap();
        copy_embedded_font_licences(&out, &repo, Some(&craft), true).unwrap();
        assert!(out.join("OFL-nanum-gothic.txt").is_file());
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
