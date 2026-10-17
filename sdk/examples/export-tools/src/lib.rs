//! Example plug-in: export hooks. Build: `cargo build --release --target wasm32-unknown-unknown`,
//! then install `target/wasm32-unknown-unknown/release/export_tools.wasm` from the Plug-in Manager.
//!
//! - **Export post-process hook**: after every export, reads each written file (lent to the call)
//!   and records its CRC-32 in the plug-in's own namespace and log. A real hook could rewrite the
//!   file with `host::write_bytes` (the exported files are lent for writing too).
//! - **Export destination** "WebDAV Upload": the Export dialog offers it under *Then send to*;
//!   after the files are written, each one is `PUT` to `<url>/<file name>` with HTTP basic auth.
//!   The manifest asks for the network hosts it may reach (`*.example.com` here: change it to
//!   your server before building). `dryRun` lists the uploads without sending anything.

use lcc_plugin_sdk::serde_json::{Value, json};
use lcc_plugin_sdk::{Request, base64, export_plugin, host};

const MANIFEST: &str = r#"{
  "id": "org.example.export-tools",
  "name": "Export Tools (example)",
  "version": "0.1.0",
  "description": "Checksums after export, and a WebDAV export destination",
  "author": "the app's authors",
  "permissions": {"network": ["*.example.com"]},
  "hooks": {"export": true},
  "exportDestinations": [
    {"id": "webdav", "name": "WebDAV Upload", "dialog": [
      {"key": "url", "label": "Folder URL", "type": "text", "default": "https://dav.example.com/photos"},
      {"key": "user", "label": "User", "type": "text"},
      {"key": "password", "label": "Password", "type": "text"},
      {"key": "dryRun", "label": "Dry run", "type": "bool", "default": false}
    ]}
  ]
}"#;

fn handle(req: &Request) -> Result<Value, String> {
    match req.hook() {
        "export" => {
            let mut sums = Vec::new();
            for (path, photo) in req.files() {
                let crc = format!("{:08x}", crc32(&host::read_bytes(&path)?));
                if !photo.is_empty() {
                    host::ns_set(&photo, "exportCrc32", json!(crc))?;
                }
                host::log(&format!("{crc}  {path}"));
                sums.push(json!({"path": path, "crc32": crc}));
            }
            Ok(json!({"files": sums.len(), "checksums": sums}))
        }
        "export.destination" if req.destination() == "webdav" => webdav(req),
        other => Err(format!("unsupported hook {other:?}")),
    }
}

fn webdav(req: &Request) -> Result<Value, String> {
    let base = req.arg("url").as_str().unwrap_or_default().trim_end_matches('/').to_string();
    if !base.starts_with("https://") && !base.starts_with("http://") {
        return Err("Folder URL must start with https:// or http://".into());
    }
    let user = req.arg("user").as_str().unwrap_or_default();
    let password = req.arg("password").as_str().unwrap_or_default();
    let dry = req.arg("dryRun").as_bool().unwrap_or(false);
    let headers = if user.is_empty() {
        json!({})
    } else {
        json!({"Authorization": format!("Basic {}", base64::encode(format!("{user}:{password}").as_bytes()))})
    };
    let mut uploaded = Vec::new();
    for (path, _) in req.files() {
        let name = path.rsplit(['/', '\\']).next().unwrap_or("photo");
        let url = format!("{base}/{}", encode_segment(name));
        if dry {
            uploaded.push(json!({"file": path, "url": url}));
            continue;
        }
        let bytes = host::read_bytes(&path)?;
        let r = host::http_bytes("PUT", &url, headers.clone(), content_type(name), &bytes)?;
        let status = r.get("status").and_then(Value::as_u64).unwrap_or(0);
        if !(200..300).contains(&status) {
            return Err(format!("{url}: the server answered {status}"));
        }
        host::log(&format!("uploaded {url}"));
        uploaded.push(json!({"file": path, "url": url, "status": status}));
    }
    Ok(json!({"message": format!("{} files sent to {base}", uploaded.len()), "uploaded": uploaded}))
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("tif" | "tiff") => "image/tiff",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        _ => "application/octet-stream",
    }
}

/// Percent-encodes a URL path segment (everything but unreserved characters).
fn encode_segment(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { char::from(b).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// CRC-32 (IEEE 802.3, reflected polynomial 0xEDB88320).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

export_plugin!(MANIFEST, handle);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_urls() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(encode_segment("My photo #1.jpg"), "My%20photo%20%231.jpg");
        assert_eq!(content_type("A.JPG"), "image/jpeg");
    }
}
