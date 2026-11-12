//! `lightcraft-cli mcp --automation-read-root <dir> --automation-write-root <dir>`: through the MCP
//! protocol, every file a command reads comes from inside the read root and every file it writes
//! goes inside the write root. Paths that leave a root (`..`, another folder, a sibling with the
//! same prefix, a link) are refused, nothing from a refused file reaches the library, and nothing
//! is written outside.
//!
//! Lives in the crate so it can use the test-only thread-local roots. Those are not a public API.

use std::path::{Path, PathBuf};

use lightcraft_engine::Session;
use lightcraft_engine::file_access::AutomationRoots;
use serde_json::{Value, json};

use crate::{Headless, Server};

const SECRET: &str = "TOP SECRET PHOTO CONTENT";

/// Write a small PNG at `path` and a sidecar holding `text`, to catch a leak of it.
fn create_test_image(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let img = lightcraft_raster::Rgba8::from_fn(64, 64, |x, y| [x as u8, y as u8, 128, 255]);
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
    std::fs::write(path, png).unwrap();
    // Also write an xmp sidecar with the text to check leakage
    let xmp = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{text}</rdf:li></rdf:Alt></dc:description></rdf:Description></rdf:RDF></x:xmpmeta>"
    );
    std::fs::write(path.with_extension("png.xmp"), xmp).unwrap();
}

/// A server confined to `in` and `out` of a new temporary folder, as asked.
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
    let session = Session::new().with_fs().with_default_denoise_models().with_system_clock().with_default_face_models();
    let headless = Headless::new(session.with_default_sam3_model().with_default_slide_templates()).with_scoped_automation_roots(roots);
    let server = Server::new(Box::new(headless));
    (server, base)
}

/// A path as text.
fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Call a tool: (is it an error, its text, the whole reply).
fn call(server: &mut Server, name: &str, args: Value) -> (bool, String, Value) {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string();
    let reply: Value = serde_json::from_str(&server.handle_line(&line).unwrap()).unwrap();
    let result = &reply["result"];
    let text =
        result["content"].as_array().map(|arr| arr.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    (result["isError"] == true, text, reply)
}

/// Call a tool that has to succeed; its text.
fn ok(server: &mut Server, name: &str, args: Value) -> String {
    let (error, text, _) = call(server, name, args.clone());
    assert!(!error, "{name} {args}: {text}");
    text
}

/// Call a tool that has to be refused for the reason `why`.
fn refused(server: &mut Server, name: &str, args: Value, why: &str) {
    let (error, text, _) = call(server, name, args.clone());
    assert!(error && text.contains(why), "{name} {args} should be refused ({why}): {text}");
    assert!(!text.contains(SECRET), "{name}: {text} leaked secret!");
}

/// A preset's watermark is held to the roots like one in the call: the export is refused, not
/// written without it.
#[test]
fn export_preset_with_a_watermark_outside_is_refused() {
    let (mut server, base) = confined("preset_mark", true, true);
    ok(&mut server, "import", json!({"paths": [s(&base.join("in/photo.png"))]}));
    let watermark = json!({"image": s(&base.join("outside/secret.png"))});
    ok(&mut server, "run_command", json!({"command": "export.savePreset", "params": {"name": "marked", "params": {"watermark": watermark}}}));
    let out = base.join("out/marked.jpg");
    refused(&mut server, "export", json!({"path": s(&out), "format": "jpeg", "preset": "marked"}), "outside");
    assert!(!out.exists(), "nothing is written");
    let _ = std::fs::remove_dir_all(base);
}

/// An export can't hand its files to Apple Photos, asked for in the call or by a preset.
#[test]
fn export_to_apple_photos_is_refused() {
    let (mut server, base) = confined("to_photos", true, true);
    ok(&mut server, "import", json!({"paths": [s(&base.join("in/photo.png"))]}));
    let out = base.join("out/photos.jpg");
    let mut asks = vec![json!({"addToPhotos": true})];
    // only a Mac gets as far as the roots; elsewhere the option itself is invalid
    let why = if cfg!(target_os = "macos") {
        ok(&mut server, "run_command", json!({"command": "export.savePreset", "params": {"name": "photos", "params": {"addToPhotos": true}}}));
        asks.push(json!({"preset": "photos"}));
        "another application"
    } else {
        "only available on macOS"
    };
    for asked in asks {
        let mut args = json!({"path": s(&out), "format": "jpeg"});
        args.as_object_mut().unwrap().extend(asked.as_object().unwrap().clone());
        refused(&mut server, "export", args, why);
        assert!(!out.exists(), "nothing is written");
    }
    let _ = std::fs::remove_dir_all(base);
}

/// Imports only from inside read root.
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

/// Exports only to inside write root.
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

/// Denied write does not truncate existing file.
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

/// Read only root permits reads but denies all writes.
#[test]
fn read_only_root_permits_reads_but_denies_all_writes() {
    let (mut server, base) = confined("readonly", true, false);
    let valid_path = s(&base.join("in/photo.png"));
    ok(&mut server, "import", json!({"paths": [valid_path]}));

    let out_file = s(&base.join("out/exported.jpg"));
    refused(&mut server, "export", json!({"path": out_file, "format": "jpeg"}), "write authority is absent");

    let _ = std::fs::remove_dir_all(base);
}

/// Write only root permits writes and reads under write root.
#[test]
fn write_only_root_permits_writes_and_reads_under_write_root() {
    let (mut server, base) = confined("writeonly", false, true);
    let in_path = s(&base.join("in/photo.png"));
    refused(&mut server, "import", json!({"paths": [in_path]}), "outside the read root");

    // Reading under the write root is permitted
    create_test_image(&base.join("out/photo.png"), "Output photo");
    let out_path = s(&base.join("out/photo.png"));
    ok(&mut server, "import", json!({"paths": [out_path]}));

    let _ = std::fs::remove_dir_all(base);
}

/// Batch operations stop and do not leak on confined errors.
#[test]
fn batch_operations_stop_and_do_not_leak_on_confined_errors() {
    let (mut server, base) = confined("batch", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    let secret_path = s(&base.join("outside/secret.png"));

    // Batch with valid and invalid paths fails without leaking secret content
    refused(&mut server, "import", json!({"paths": [valid_path, secret_path]}), "outside the read root");

    let _ = std::fs::remove_dir_all(base);
}

/// Sidecar write refused outside write root.
#[test]
fn sidecar_write_refused_outside_write_root() {
    let (mut server, base) = confined("sidecar", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    ok(&mut server, "import", json!({"paths": [valid_path]}));

    let sidecar = base.join("in/photo.png.xmp");
    let before = std::fs::read(&sidecar).unwrap();
    let (_, text, reply) = call(&mut server, "run_command", json!({"command": "photo.saveMetadataToFile", "params": {}}));
    assert!(text.contains("outside the write root") || reply.to_string().contains("outside the write root"), "{text}");
    assert_eq!(std::fs::read(&sidecar).unwrap(), before, "Denied sidecar write must not modify the existing sidecar");
    assert!(!base.join("in/photo.xmp").exists(), "Denied sidecar write must not create file");

    let _ = std::fs::remove_dir_all(base);
}

/// Folder browse refused outside read root.
#[test]
fn folder_browse_refused_outside_read_root() {
    let (mut server, base) = confined("browse", true, true);
    let inside_dir = s(&base.join("in"));
    let outside_dir = s(&base.join("outside"));

    // Browsing folder inside read root succeeds
    ok(&mut server, "run_command", json!({"command": "library.browse", "params": {"path": inside_dir}}));

    // Browsing folder outside read root is refused
    refused(&mut server, "run_command", json!({"command": "library.browse", "params": {"path": outside_dir}}), "outside the read root");

    let _ = std::fs::remove_dir_all(base);
}

/// Render photo in memory succeeds under confinement.
#[test]
fn render_photo_in_memory_succeeds_under_confinement() {
    let (mut server, base) = confined("render", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    ok(&mut server, "import", json!({"paths": [valid_path]}));

    // Calling render_photo without a path stays in memory and succeeds
    let (error, text, reply) = call(&mut server, "render_photo", json!({}));
    assert!(!error, "render_photo in memory must succeed under confinement: {text}");
    let image = reply["result"]["content"].as_array().unwrap().iter().find(|c| c["type"] == "image").expect("an image block");
    let png = crate::base64_decode(image["data"].as_str().unwrap()).unwrap();
    assert!(lightcraft_codecs::decode(&png, Default::default()).is_ok(), "the image decodes");
    assert!(!text.contains("\"data\""), "the text doesn't repeat the image: {text}");

    // saved to a file, the text doesn't carry it either
    let saved = base.join("out/render.png");
    let (error, text, _) = call(&mut server, "render_photo", json!({"path": s(&saved)}));
    assert!(!error, "{text}");
    assert!(saved.is_file());
    assert!(!text.contains("\"data\""), "the text doesn't repeat the image: {text}");

    let _ = std::fs::remove_dir_all(base);
}

/// Symlink directory outside read root is not traversed.
#[test]
fn symlink_directory_outside_read_root_is_not_traversed() {
    let (mut server, base) = confined("symlink_dir", true, true);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(base.join("outside"), base.join("in/link_outside")).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(base.join("outside"), base.join("in/link_outside")).is_ok();
    if made {
        let in_dir = s(&base.join("in"));
        let (error, text, reply) = call(&mut server, "import", json!({"paths": [in_dir]}));
        assert!(!error, "import {in_dir}: {text}");
        let imported = reply["result"]["structuredContent"]["imported"].as_array().expect("import response must contain imported photo ids");
        assert_eq!(imported.len(), 1, "Must not import photo through symlink directory: {reply}");
    }
    let _ = std::fs::remove_dir_all(base);
}

/// Mask import effects symlink directory outside read root is not traversed.
#[test]
fn mask_import_effects_symlink_directory_outside_read_root_is_not_traversed() {
    let (mut server, base) = confined("mask_symlink_dir", true, true);
    std::fs::create_dir_all(base.join("in/sub")).unwrap();
    std::fs::write(base.join("outside/secret.lceffect"), "{}").unwrap();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(base.join("outside"), base.join("in/sub/link_outside")).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(base.join("outside"), base.join("in/sub/link_outside")).is_ok();
    if made {
        let in_dir = s(&base.join("in"));
        let (error, text, reply) = call(&mut server, "cmd_mask_importEffects", json!({"paths": [in_dir]}));
        assert!(!error, "mask.importEffects {in_dir}: {text}");
        let full_response = reply.to_string();
        assert!(!full_response.contains("secret.lceffect"), "Must not disclose outside directory entries through nested symlink: {reply}");
    }
    let _ = std::fs::remove_dir_all(base);
}

/// Folder create refused outside write root.
#[test]
fn folder_create_refused_outside_write_root() {
    let (mut server, base) = confined("folder_create", true, true);
    let in_dir = s(&base.join("in"));
    let outside_dir = s(&base.join("outside"));
    let out_dir = s(&base.join("out"));

    // Attempting to create a folder in the read root is refused
    refused(
        &mut server,
        "run_command",
        json!({"command": "folder.create", "params": {"parent": in_dir, "name": "subfolder"}}),
        "outside the write root",
    );
    assert!(!base.join("in/subfolder").exists(), "Denied folder.create must not create folder in read root");

    // Attempting to create a folder in an outside directory is refused
    refused(
        &mut server,
        "run_command",
        json!({"command": "folder.create", "params": {"parent": outside_dir, "name": "subfolder"}}),
        "outside the write root",
    );
    assert!(!base.join("outside/subfolder").exists(), "Denied folder.create must not create folder outside");

    // Creating folder inside write root succeeds
    ok(&mut server, "run_command", json!({"command": "folder.create", "params": {"parent": out_dir, "name": "valid_subfolder"}}));
    assert!(base.join("out/valid_subfolder").exists(), "Valid folder.create inside write root must succeed");

    let _ = std::fs::remove_dir_all(base);
}

/// Folder rename and move refused outside write root.
#[test]
fn folder_rename_and_move_refused_outside_write_root() {
    let (mut server, base) = confined("folder_ops", true, true);
    std::fs::create_dir_all(base.join("in/my_folder")).unwrap();
    std::fs::create_dir_all(base.join("out/my_folder")).unwrap();

    let in_folder = s(&base.join("in/my_folder"));
    let out_folder = s(&base.join("out/my_folder"));
    let in_dir = s(&base.join("in"));
    let out_dir = s(&base.join("out"));

    // Renaming folder in read root is refused
    refused(
        &mut server,
        "run_command",
        json!({"command": "folder.rename", "params": {"path": in_folder, "name": "renamed_folder"}}),
        "outside the write root",
    );
    assert!(base.join("in/my_folder").exists(), "Folder in read root must remain unmodified");
    assert!(!base.join("in/renamed_folder").exists(), "Denied folder rename must not create new folder");

    // Moving folder from read root into write root is refused (source cannot be modified)
    refused(&mut server, "run_command", json!({"command": "folder.move", "params": {"path": in_folder, "into": out_dir}}), "outside the write root");
    assert!(base.join("in/my_folder").exists(), "Denied folder move must keep source intact");

    // Moving folder from write root into read root is refused (destination cannot be outside write root)
    refused(&mut server, "run_command", json!({"command": "folder.move", "params": {"path": out_folder, "into": in_dir}}), "outside the write root");
    assert!(base.join("out/my_folder").exists(), "Folder in write root must remain in place");

    let _ = std::fs::remove_dir_all(base);
}

/// Photo move to folder refused outside write root.
#[test]
fn photo_move_to_folder_refused_outside_write_root() {
    let (mut server, base) = confined("move_photo", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    let (error, text, reply) = call(&mut server, "import", json!({"paths": [valid_path]}));
    assert!(!error, "import {valid_path}: {text}");
    let id = reply["result"]["structuredContent"]["imported"][0].as_u64().unwrap();

    // Target folder in read root or outside is refused
    let in_dir = s(&base.join("in"));
    let outside_dir = s(&base.join("outside"));

    refused(
        &mut server,
        "run_command",
        json!({"command": "photo.moveToFolder", "params": {"folder": outside_dir, "ids": [id]}}),
        "outside the write root",
    );
    assert!(base.join("in/photo.png").exists(), "Photo source must not move");

    refused(
        &mut server,
        "run_command",
        json!({"command": "photo.moveToFolder", "params": {"folder": in_dir, "ids": [id]}}),
        "outside the write root",
    );
    assert!(base.join("in/photo.png").exists(), "Photo source must not move");

    let _ = std::fs::remove_dir_all(base);
}

/// Photo rename refused outside write root.
#[test]
fn photo_rename_refused_outside_write_root() {
    let (mut server, base) = confined("photo_rename", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    let (error, text, reply) = call(&mut server, "import", json!({"paths": [valid_path]}));
    assert!(!error, "import {valid_path}: {text}");
    let id = reply["result"]["structuredContent"]["imported"][0].as_u64().unwrap();

    // Photo is in `in/photo.png` (read root, not write root)
    // Renaming it on disk requires write authority on `in/photo.png` and `in/renamed_photo.png`
    refused(
        &mut server,
        "run_command",
        json!({"command": "photo.rename", "params": {"template": "New_{name}", "ids": [id]}}),
        "outside the write root",
    );
    assert!(base.join("in/photo.png").exists(), "Photo must not be renamed on disk");
    assert!(!base.join("in/New_photo.png").exists(), "New photo path must not be created");

    let _ = std::fs::remove_dir_all(base);
}

/// Import mode move refused without source write authority.
#[test]
fn import_mode_move_refused_without_source_write_authority() {
    let (mut server, base) = confined("import_move", true, true);
    let photo = base.join("in/photo.png");
    let valid_path = s(&photo);
    let out_dir = s(&base.join("out"));

    // Attempting to import with mode=move from read root (without write authority on source) is refused
    refused(&mut server, "import", json!({"paths": [valid_path], "mode": "move", "destination": out_dir}), "outside the write root");
    assert!(photo.exists(), "Source photo in read root must not be removed by move import");
    assert!(base.join("in/photo.png.xmp").exists(), "Sidecar in read root must not be removed by move import");

    let _ = std::fs::remove_dir_all(base);
}

/// Lightroom commands refused outside read root.
#[test]
fn lightroom_commands_refused_outside_read_root() {
    let (mut server, base) = confined("lrcat", true, true);
    let lrcat_outside = s(&base.join("outside/catalog.lrcat"));
    std::fs::write(base.join("outside/catalog.lrcat"), b"not a real catalog").unwrap();

    refused(&mut server, "run_command", json!({"command": "library.inspectLightroom", "params": {"path": lrcat_outside}}), "outside the read root");

    refused(&mut server, "run_command", json!({"command": "library.importLightroom", "params": {"path": lrcat_outside}}), "outside the read root");

    let _ = std::fs::remove_dir_all(base);
}

/// Lightroom wal symlink outside read root is refused.
#[test]
fn lightroom_wal_symlink_outside_read_root_is_refused() {
    let (mut server, base) = confined("lrcat_wal", true, true);
    let lrcat_in = base.join("in/catalog.lrcat");
    let wal_in = base.join("in/catalog.lrcat-wal");
    let outside_secret = base.join("outside/secret.txt");
    std::fs::write(&outside_secret, b"secret wal data").unwrap();
    std::fs::write(&lrcat_in, b"SQLite format 3\0").unwrap();

    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside_secret, &wal_in).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&outside_secret, &wal_in).is_ok();

    if made {
        refused(
            &mut server,
            "run_command",
            json!({"command": "library.inspectLightroom", "params": {"path": s(&lrcat_in)}}),
            "outside the read root",
        );
        refused(&mut server, "run_command", json!({"command": "library.importLightroom", "params": {"path": s(&lrcat_in)}}), "outside the read root");
    }
    let _ = std::fs::remove_dir_all(base);
}

/// Library import preview refused outside read root.
#[test]
fn library_import_preview_refused_outside_read_root() {
    let (mut server, base) = confined("preview", true, true);
    let outside_path = s(&base.join("outside/secret.png"));

    refused(&mut server, "run_command", json!({"command": "library.importPreview", "params": {"paths": [outside_path]}}), "outside the read root");

    let _ = std::fs::remove_dir_all(base);
}

/// [`confined`] with a library on disk, for commands that need one.
fn confined_with_library(tag: &str, read: bool, write: bool) -> (Server, PathBuf) {
    let base = std::env::temp_dir().join(format!("lc-mcp-roots-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for d in ["in", "out", "outside", "lib"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    create_test_image(&base.join("in/photo.png"), "Inside photo");
    create_test_image(&base.join("outside/secret.png"), SECRET);

    let (in_dir, out_dir) = (base.join("in"), base.join("out"));
    let roots = AutomationRoots::new(read.then_some(in_dir.as_path()), write.then_some(out_dir.as_path())).unwrap();
    let mut session = Session::new().with_fs().with_default_denoise_models().with_system_clock().with_default_face_models();
    session.open_library(base.join("lib"), false).unwrap();
    let headless = Headless::new(session.with_default_sam3_model().with_default_slide_templates()).with_scoped_automation_roots(roots);
    let server = Server::new(Box::new(headless));
    (server, base)
}

/// Photo duplicate refused outside write root and in read only root.
#[test]
fn photo_duplicate_refused_outside_write_root_and_in_read_only_root() {
    // 1. With write root = out/, source photo is in in/photo.png (outside write root).
    // Duplicating copies next to original into in/photo-copy.png.
    let (mut server, base) = confined("dup_write", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    let (error, text, reply) = call(&mut server, "import", json!({"paths": [valid_path]}));
    assert!(!error, "import {valid_path}: {text}");
    let id = reply["result"]["structuredContent"]["imported"][0].as_u64().unwrap();

    refused(&mut server, "run_command", json!({"command": "photo.duplicate", "params": {"ids": [id]}}), "outside the write root");
    assert!(!base.join("in/photo-copy.png").exists(), "Duplicate file must not be created outside write root");
    let _ = std::fs::remove_dir_all(base);

    // 2. In read-only root: write authority is absent.
    let (mut server, base) = confined("dup_ro", true, false);
    let valid_path = s(&base.join("in/photo.png"));
    let (error, text, reply) = call(&mut server, "import", json!({"paths": [valid_path]}));
    assert!(!error, "import {valid_path}: {text}");
    let id = reply["result"]["structuredContent"]["imported"][0].as_u64().unwrap();

    refused(&mut server, "run_command", json!({"command": "photo.duplicate", "params": {"ids": [id]}}), "write authority is absent");
    assert!(!base.join("in/photo-copy.png").exists(), "Duplicate file must not be created with read-only root");
    let _ = std::fs::remove_dir_all(base);
}

/// Photo convert to dng refused outside write root and in read only root.
#[test]
fn photo_convert_to_dng_refused_outside_write_root_and_in_read_only_root() {
    // 1. With write root = out/, attempting to convert a photo in in/ is refused write into in/.
    let (mut server, base) = confined("dng_write", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    let (error, text, reply) = call(&mut server, "import", json!({"paths": [valid_path]}));
    assert!(!error, "import {valid_path}: {text}");
    let id = reply["result"]["structuredContent"]["imported"][0].as_u64().unwrap();

    refused(&mut server, "run_command", json!({"command": "photo.convertToDng", "params": {"ids": [id]}}), "outside the write root");
    assert!(!base.join("in/photo.dng").exists(), "DNG must not be created outside write root");
    let _ = std::fs::remove_dir_all(base);

    // 2. In read-only root: write authority is absent.
    let (mut server, base) = confined("dng_ro", true, false);
    let valid_path = s(&base.join("in/photo.png"));
    let (error, text, reply) = call(&mut server, "import", json!({"paths": [valid_path]}));
    assert!(!error, "import {valid_path}: {text}");
    let id = reply["result"]["structuredContent"]["imported"][0].as_u64().unwrap();

    refused(&mut server, "run_command", json!({"command": "photo.convertToDng", "params": {"ids": [id]}}), "write authority is absent");
    assert!(!base.join("in/photo.dng").exists(), "DNG must not be created with read-only root");
    let _ = std::fs::remove_dir_all(base);
}

/// Library smart previews location refused outside write root and in read only root.
#[test]
fn library_smart_previews_location_refused_outside_write_root_and_in_read_only_root() {
    // 1. With write root = out/, moving or creating smart previews outside write root is refused.
    let (mut server, base) = confined_with_library("smart_write", true, true);
    let outside_smart = s(&base.join("outside/smart_previews"));

    refused(
        &mut server,
        "run_command",
        json!({"command": "library.smartPreviewsLocation", "params": {"path": outside_smart}}),
        "outside the write root",
    );
    assert!(!base.join("outside/smart_previews").exists(), "Smart previews dir must not be created outside write root");
    let _ = std::fs::remove_dir_all(base);

    // 2. In read-only root: write authority is absent.
    let (mut server, base) = confined_with_library("smart_ro", true, false);
    let out_smart = s(&base.join("out/smart_previews"));

    refused(
        &mut server,
        "run_command",
        json!({"command": "library.smartPreviewsLocation", "params": {"path": out_smart}}),
        "write authority is absent",
    );
    assert!(!base.join("out/smart_previews").exists(), "Smart previews dir must not be created with read-only root");
    let _ = std::fs::remove_dir_all(base);
}

/// Photo relink refused outside read root.
#[test]
fn photo_relink_refused_outside_read_root() {
    let (mut server, base) = confined("relink", true, true);
    let valid_path = s(&base.join("in/photo.png"));
    let (error, text, reply) = call(&mut server, "import", json!({"paths": [valid_path]}));
    assert!(!error, "import {valid_path}: {text}");
    let id = reply["result"]["structuredContent"]["imported"][0].as_u64().unwrap();

    let outside_path = s(&base.join("outside/secret.png"));
    refused(&mut server, "run_command", json!({"command": "photo.relink", "params": {"id": id, "path": outside_path}}), "outside the read root");
    let _ = std::fs::remove_dir_all(base);
}

/// Merge refused when the file would land outside the write root.
#[test]
fn merge_refused_when_the_file_would_land_outside_the_write_root() {
    let (mut server, base) = confined("merge", true, true);
    // other pixels: the same bytes would be imported as the same photo
    let img = lightcraft_raster::Rgba8::from_fn(64, 64, |x, y| [y as u8, x as u8, 32, 255]);
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
    std::fs::write(base.join("in/second.png"), png).unwrap();
    ok(&mut server, "import", json!({"paths": [s(&base.join("in/photo.png")), s(&base.join("in/second.png"))]}));
    ok(&mut server, "run_command", json!({"command": "library.selectAll"}));
    for id in ["merge.hdr", "merge.panorama"] {
        refused(&mut server, "run_command", json!({"command": id}), "outside the write root");
    }
    let names: Vec<_> = std::fs::read_dir(base.join("in")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(!names.iter().any(|n| n.to_string_lossy().ends_with(".dng")), "{names:?}");
    let _ = std::fs::remove_dir_all(base);
}

/// Smart previews stay put unless their folder can be written.
#[test]
fn smart_previews_stay_put_unless_their_folder_can_be_written() {
    let (mut server, base) = confined_with_library("smart-existing", true, true);
    let new_dir = s(&base.join("out/smart"));
    for existing in ["move", "discard"] {
        refused(
            &mut server,
            "run_command",
            json!({"command": "library.smartPreviewsLocation", "params": {"path": new_dir, "existing": existing}}),
            "outside the write root",
        );
    }
    ok(&mut server, "run_command", json!({"command": "library.smartPreviewsLocation", "params": {"path": new_dir, "existing": "leave"}}));
    let _ = std::fs::remove_dir_all(base);
}
