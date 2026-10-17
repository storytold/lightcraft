//! Example plug-in: a publish service. Build: `cargo build --release --target wasm32-unknown-unknown`,
//! then install `target/wasm32-unknown-unknown/release/publish_folder.wasm` from the Plug-in Manager.
//!
//! The service "Local Gallery" appears in the Publish Services panel (kind
//! `plugin:org.example.publish-folder`). Publishing copies each rendered photo into the gallery
//! folder `/tmp/lcc-gallery` (the one filesystem root the manifest asks for; change it before
//! building) and keeps an `index.json` of what is there. Re-publishing replaces the file;
//! removing a photo from the collection deletes it.

use lcc_plugin_sdk::serde_json::{Map, Value, json};
use lcc_plugin_sdk::{Request, export_plugin, host};

const ROOT: &str = "/tmp/lcc-gallery";

const MANIFEST: &str = r#"{
  "id": "org.example.publish-folder",
  "name": "Local Gallery (example)",
  "version": "0.1.0",
  "description": "Publishes photos into a gallery folder with an index",
  "author": "the app's authors",
  "permissions": {"fs": [{"path": "/tmp/lcc-gallery", "write": true}]},
  "publish": {"id": "gallery", "name": "Local Gallery"}
}"#;

fn handle(req: &Request) -> Result<Value, String> {
    match req.hook() {
        "publish.upload" => {
            let item = req.get("item");
            let path = item.get("path").and_then(Value::as_str).ok_or("item.path missing")?;
            let photo = item.get("photo").and_then(Value::as_str).unwrap_or("photo");
            let name = path.rsplit(['/', '\\']).next().unwrap_or("photo.jpg");
            // a stable remote id: the same photo always lands in the same file
            let remote = safe_name(&format!("{photo}-{name}"));
            if let Some(old) = item.get("remoteId").and_then(Value::as_str).filter(|o| *o != remote) {
                let _ = host::remove(&format!("{ROOT}/{}", safe_name(old)));
            }
            let target = format!("{ROOT}/{remote}");
            host::write_bytes(&target, &host::read_bytes(path)?)?;
            let mut index = read_index();
            index.insert(remote.clone(), json!({"photo": photo, "title": item.get("title"), "caption": item.get("caption")}));
            write_index(&index)?;
            host::log(&format!("published {target}"));
            Ok(json!({"remoteId": remote, "url": format!("file://{target}")}))
        }
        "publish.delete" => {
            let remote = safe_name(req.get("remoteId").as_str().unwrap_or_default());
            if remote.is_empty() {
                return Err("remoteId missing".into());
            }
            let target = format!("{ROOT}/{remote}");
            if host::exists(&target)? {
                host::remove(&target)?;
            }
            let mut index = read_index();
            index.remove(&remote);
            write_index(&index)?;
            Ok(Value::Null)
        }
        other => Err(format!("unsupported hook {other:?}")),
    }
}

fn read_index() -> Map<String, Value> {
    host::read_text(&format!("{ROOT}/index.json"))
        .ok()
        .and_then(|t| lcc_plugin_sdk::serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_index(index: &Map<String, Value>) -> Result<(), String> {
    let text = lcc_plugin_sdk::serde_json::to_string_pretty(index).map_err(|e| e.to_string())?;
    host::write_text(&format!("{ROOT}/index.json"), &text)
}

/// A file name without separators or `..`, so a remote id can't leave the folder.
fn safe_name(s: &str) -> String {
    let n: String = s.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' }).take(200).collect();
    if n.trim_matches('.').is_empty() || n == "index.json" { String::new() } else { n }
}

export_plugin!(MANIFEST, handle);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_in_the_folder() {
        assert_eq!(safe_name("7-a b.jpg"), "7-a_b.jpg");
        assert_eq!(safe_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_name(".."), "");
        assert_eq!(safe_name("index.json"), "");
    }
}
