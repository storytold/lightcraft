//! Example plug-in. Build: `cargo build --release --target wasm32-unknown-unknown`, then install
//! `target/wasm32-unknown-unknown/release/catalog_tools.wasm` from the Plug-in Manager.
//!
//! - **Count Photos** (menu command with a dialog): reads the catalog and reports the count.
//! - **Mark Selected** (menu command): records a flag in the plug-in's own metadata namespace.
//! - **Metadata provider**: suggests keywords from the words in each photo's file name.
//! - **Export hook**: remembers where each photo was last exported (own namespace).
//! - **Publish service** "Example Folder": pretends to upload, returning the file as the remote.

use dac_plugin_sdk::serde_json::{Value, json};
use dac_plugin_sdk::{Request, export_plugin, host};

const MANIFEST: &str = r#"{
  "id": "org.example.catalog-tools",
  "name": "Catalog Tools (example)",
  "version": "0.1.0",
  "description": "Example plug-in for the SDK",
  "author": "the app's authors",
  "permissions": {"catalog": true},
  "commands": [
    {"id": "count", "label": "Count Photos", "menu": "Examples",
     "dialog": [{"key": "label", "label": "Label", "type": "text", "default": "Photos"}]},
    {"id": "mark", "label": "Mark Selected", "menu": "Examples"}
  ],
  "hooks": {"export": true, "metadata": true},
  "publish": {"id": "folder", "name": "Example Folder"}
}"#;

fn handle(req: &Request) -> Result<Value, String> {
    match req.hook() {
        "command" => match req.command() {
            "count" => {
                let stats = host::call("catalog.stats", json!({}))?;
                let label = req.arg("label").as_str().unwrap_or("Photos").to_string();
                host::log(&format!("counted photos for {label}"));
                Ok(json!({"message": format!("{label}: {}", stats.get("photos").cloned().unwrap_or(Value::Null)), "stats": stats}))
            }
            "mark" => {
                let ids = req.selection();
                for id in &ids {
                    host::ns_set(id, "marked", json!(true))?;
                }
                Ok(json!({"message": format!("marked {} photos", ids.len()), "marked": ids.len()}))
            }
            other => Err(format!("unknown command {other}")),
        },
        "metadata" => {
            let photos = req.get("photos").as_array().cloned().unwrap_or_default();
            let suggestions: Vec<Value> = photos.iter().map(|p| json!({"photo": p.get("id"), "keywords": keywords(p)})).collect();
            Ok(json!({"suggestions": suggestions}))
        }
        "export" => {
            let files = req.get("files").as_array().cloned().unwrap_or_default();
            for f in &files {
                if let (Some(photo), Some(path)) = (f.get("photo").and_then(Value::as_str), f.get("path").and_then(Value::as_str)) {
                    host::ns_set(photo, "lastExport", json!(path))?;
                }
            }
            host::log(&format!("export hook saw {} files", files.len()));
            Ok(json!({"files": files.len()}))
        }
        "publish.upload" => {
            let item = req.get("item");
            let path = item.get("path").and_then(Value::as_str).unwrap_or_default();
            Ok(json!({"remoteId": item.get("photo"), "url": format!("file://{path}")}))
        }
        "publish.delete" => Ok(Value::Null),
        other => Err(format!("unsupported hook {other:?}")),
    }
}

/// Lower-case words of 3+ letters in the photo's file name.
fn keywords(photo: &Value) -> Vec<String> {
    let name = ["fileName", "name", "path", "file"].iter().find_map(|k| photo.get(*k).and_then(Value::as_str)).unwrap_or_default();
    let stem = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
    let mut out: Vec<String> = stem.split(|c: char| !c.is_alphabetic()).filter(|w| w.chars().count() >= 3).map(str::to_lowercase).collect();
    out.dedup();
    out
}

export_plugin!(MANIFEST, handle);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_from_file_names() {
        assert_eq!(keywords(&json!({"fileName": "/x/Beach_sunset-01.jpg"})), vec!["beach", "sunset"]);
    }
}
