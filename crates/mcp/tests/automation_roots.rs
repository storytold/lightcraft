//! `lightcraft-cli mcp --automation-read-root <dir> --automation-write-root <dir>`: through the MCP
//! protocol, every file a command reads comes from inside the read root and every file it writes
//! goes inside the write root. Paths that leave a root (`..`, another folder, a sibling with the
//! same prefix, a link) are refused, nothing from a refused file reaches the library, and nothing
//! is written outside.
// Integration tests: unwrapping and panicking on failure is fine here, unlike in shipped code (AGENTS.md).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightcraft_engine::Session;
use lightcraft_engine::file_access::AutomationRoots;
use lightcraft_mcp::{Headless, Server};
use serde_json::{Value, json};

const SECRET: &str = "TOP SECRET PHOTO CONTENT";

fn create_test_image(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let img = lightcraft_raster::Rgba8::from_fn(64, 64, |x, y| [x as u8, y as u8, 128, 255]);
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
    std::fs::write(path, png).unwrap();
    // Also write an xmp sidecar with the text to check leakage
    let xmp = format!("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{text}</rdf:li></rdf:Alt></dc:description></rdf:Description></rdf:RDF></x:xmpmeta>");
    std::fs::write(path.with_extension("png.xmp"), xmp).unwrap();
}

fn confined(tag: &str, read: bool, write: bool) -> (Server, PathBuf) {
    let base = std::env::temp_dir().join(format!("lc-mcp-roots-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for d in ["in", "out", "outside", "in2"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    create_test_image(&base.join("in/photo.png"), "Inside photo");
    create_test_image(&base.join("outside/secret.png"), SECRET);
    create_test_image(&base.join("in2/sibling.png"), "Sibling photo");

    let (in_dir, out_dir) = (base.join("in"), base.join("out"));
    let roots = AutomationRoots::new(read.then_some(in_dir.as_path()), write.then_some(out_dir.as_path())).unwrap();
    let mut session = Session::new().with_fs().with_default_denoise_models().with_system_clock().with_default_face_models();
    let headless = Headless::new(session.with_default_sam3_model().with_default_slide_templates()).with_automation_roots(roots);
    let server = Server::new(Box::new(headless));
    (server, base)
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn call(server: &mut Server, name: &str, args: Value) -> (bool, String, Value) {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string();
    let reply: Value = serde_json::from_str(&server.handle_line(&line).unwrap()).unwrap();
    let result = &reply["result"];
    let text = result["content"].as_array().map(|arr| {
        arr.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
    }).unwrap_or_default();
    (result["isError"] == true, text, reply)
}

fn ok(server: &mut Server, name: &str, args: Value) -> String {
    let (error, text, _) = call(server, name, args.clone());
    assert!(!error, "{name} {args}: {text}");
    text
}

fn refused(server: &mut Server, name: &str, args: Value, why: &str) {
    let (error, text, _) = call(server, name, args.clone());
    assert!(error && text.contains(why), "{name} {args} should be refused ({why}): {text}");
    assert!(!text.contains(SECRET), "{name}: {text} leaked secret!");
}

#[test]
fn imports_only_from_inside_read_root() {
    let (mut server, base) = confined("import", true, true);
    let sep = std::path::MAIN_SEPARATOR;

    // Importing valid photo inside read root succeeds
    let valid_path = s(&base.join("in/photo.png"));
    ok(&mut server, "import", json!({"paths": [valid_path]}));

    // Importing outside read root is refused
    let outside_path = s(&base.join("outside/secret.png"));
    refused(&mut server, "import", json!({"paths": [outside_path]}), "outside the read root");

    // Traversal escape is refused
    let escape = format!("{}{sep}..{sep}outside{sep}secret.png", s(&base.join("in")));
    refused(&mut server, "import", json!({"paths": [escape]}), "outside the read root");

    // Sibling prefix escape is refused
    let sibling = s(&base.join("in2/sibling.png"));
    refused(&mut server, "import", json!({"paths": [sibling]}), "outside the read root");

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn exports_only_to_inside_write_root() {
    let (mut server, base) = confined("export", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    ok(&mut server, "import", json!({"paths": [valid_path]}));

    // Export to write root succeeds
    let out_file = s(&base.join("out/exported.jpg"));
    ok(&mut server, "export", json!({"path": out_file, "format": "jpeg"}));
    assert!(Path::new(&out_file).exists());

    // Export outside write root is refused
    let outside_file = s(&base.join("outside/leak.jpg"));
    refused(&mut server, "export", json!({"path": outside_file, "format": "jpeg"}), "outside the write root");
    assert!(!Path::new(&outside_file).exists(), "Denied export must not create file");

    // Export into read root is refused (write root != read root)
    let in_file = s(&base.join("in/leak.jpg"));
    refused(&mut server, "export", json!({"path": in_file, "format": "jpeg"}), "outside the write root");
    assert!(!Path::new(&in_file).exists(), "Denied export must not create file in read root");

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn denied_write_does_not_truncate_existing_file() {
    let (mut server, base) = confined("trunc", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    ok(&mut server, "import", json!({"paths": [valid_path]}));

    let protected = base.join("outside/important.jpg");
    std::fs::write(&protected, b"IMPORTANT EXISTING DATA").unwrap();

    refused(&mut server, "export", json!({"path": s(&protected), "format": "jpeg"}), "outside the write root");
    assert_eq!(std::fs::read(&protected).unwrap(), b"IMPORTANT EXISTING DATA", "Existing file must not be modified or truncated");

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn read_only_root_permits_reads_but_denies_all_writes() {
    let (mut server, base) = confined("readonly", true, false);
    let valid_path = s(&base.join("in/photo.png"));
    ok(&mut server, "import", json!({"paths": [valid_path]}));

    let out_file = s(&base.join("out/exported.jpg"));
    refused(&mut server, "export", json!({"path": out_file, "format": "jpeg"}), "outside the write root");

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn write_only_root_permits_writes_but_denies_all_reads() {
    let (mut server, base) = confined("writeonly", false, true);
    let valid_path = s(&base.join("in/photo.png"));
    refused(&mut server, "import", json!({"paths": [valid_path]}), "read authority is absent");

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn batch_operations_stop_and_do_not_leak_on_confined_errors() {
    let (mut server, base) = confined("batch", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    let secret_path = s(&base.join("outside/secret.png"));

    // Batch with valid and invalid paths fails without leaking secret content
    refused(&mut server, "import", json!({"paths": [valid_path, secret_path]}), "outside the read root");

    let _ = std::fs::remove_dir_all(base);
}
