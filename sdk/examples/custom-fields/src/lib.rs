//! Example plug-in: custom metadata fields. Build: `cargo build --release --target wasm32-unknown-unknown`,
//! then install `target/wasm32-unknown-unknown/release/custom_fields.wasm` from the Plug-in Manager.
//!
//! - **Metadata fields** Client, Job, Usage (editable in the Metadata panel's plug-in section) and
//!   Delivered (read-only: the plug-in sets it). Values live in the plug-in's own namespace, never
//!   in the catalog, and need no permission.
//! - **Set Job Fields…** (Plug-in Extras, with a dialog): fills Client / Job / Usage for the
//!   selected photos.
//! - **Export hook**: marks every exported photo Delivered.
//! - **Metadata provider**: suggests the client as a keyword.

use lcc_plugin_sdk::serde_json::{Value, json};
use lcc_plugin_sdk::{Request, export_plugin, host};

const MANIFEST: &str = r#"{
  "id": "org.example.custom-fields",
  "name": "Job Fields (example)",
  "version": "0.1.0",
  "description": "Client, job and usage fields for commissioned work",
  "author": "the app's authors",
  "commands": [
    {"id": "set", "label": "Set Job Fields", "dialog": [
      {"key": "client", "label": "Client", "type": "text"},
      {"key": "job", "label": "Job", "type": "text"},
      {"key": "usage", "label": "Usage", "type": "choice", "options": ["Editorial", "Commercial", "Personal"]}
    ]}
  ],
  "hooks": {"export": true, "metadata": true},
  "metadataFields": [
    {"key": "client", "label": "Client", "type": "text"},
    {"key": "job", "label": "Job", "type": "text"},
    {"key": "usage", "label": "Usage", "type": "choice", "options": ["Editorial", "Commercial", "Personal"]},
    {"key": "delivered", "label": "Delivered", "type": "bool", "readOnly": true}
  ]
}"#;

fn handle(req: &Request) -> Result<Value, String> {
    match req.hook() {
        "command" if req.command() == "set" => {
            let ids = req.selection();
            if ids.is_empty() {
                return Err("select photos first".into());
            }
            for id in &ids {
                for key in ["client", "job", "usage"] {
                    let v = req.arg(key);
                    // an empty text leaves the field as it is
                    if v.as_str().is_some_and(|s| !s.trim().is_empty()) {
                        host::ns_set(id, key, json!(v.as_str().unwrap_or_default().trim()))?;
                    }
                }
            }
            Ok(json!({"message": format!("job fields set on {} photos", ids.len()), "changed": ids.len()}))
        }
        "export" => {
            let mut n = 0;
            for (_, photo) in req.files() {
                if !photo.is_empty() {
                    host::ns_set(&photo, "delivered", json!(true))?;
                    n += 1;
                }
            }
            Ok(json!({"delivered": n}))
        }
        "metadata" => {
            let photos = req.get("photos").as_array().cloned().unwrap_or_default();
            let mut suggestions = Vec::new();
            for p in &photos {
                let id = match p.get("id") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => continue,
                };
                if let Some(client) = host::ns_get(&id)?.get("client").and_then(Value::as_str).filter(|c| !c.is_empty()) {
                    suggestions.push(json!({"photo": id, "keywords": [client]}));
                }
            }
            Ok(json!({"suggestions": suggestions}))
        }
        other => Err(format!("unsupported hook {other:?}")),
    }
}

export_plugin!(MANIFEST, handle);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_json() {
        let m: Value = lcc_plugin_sdk::serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(m["metadataFields"].as_array().map(Vec::len), Some(4));
    }
}
