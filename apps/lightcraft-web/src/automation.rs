//! The page's automation entry point, `lightcraft.command(id, params)` (see `web.rs`): what the
//! params text means. Plain Rust, so it is tested natively.

use serde_json::Value;

/// The command parameters in `text`: a JSON object, or `{}` when the text is empty or blank (or
/// `null`). Anything else is an error, never `{}`: a command run without its params acts on the
/// selection or with defaults, so a typo such as `{ids: [3]}` must not delete or reset the
/// selected photos. The control channel and MCP refuse such params the same way.
pub fn params(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Null) => Ok(Value::Object(Default::default())),
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => Err("params must be a JSON object, e.g. {\"ids\": [3]}".into()),
        Err(e) => Err(format!("params are not valid JSON ({e}); nothing was run")),
    }
}

/// Whether command `id` keeps its place behind an Original export that is still waiting for its
/// videos to load: anything that can change the library (it could remove a photo the export
/// copies), but not a query, which runs at once. UI commands and unknown ids keep their order too.
pub fn waits_behind_export(id: &str) -> bool {
    if id == "web.stats" {
        return false;
    }
    lightcraft_engine::find_command(id).is_none_or(|spec| spec.journal)
}

/// How long an Original export waits for its videos before it runs anyway (and reports the one
/// still loading), so a storage read that never settles can't hold the commands behind it forever.
pub const EXPORT_WAIT_MS: f64 = 30_000.0;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn library_changes_wait_behind_a_waiting_export_and_queries_do_not() {
        for id in ["photo.deletePermanently", "photo.delete", "dialog.allMetadata", "no.such.command"] {
            assert!(waits_behind_export(id), "{id}");
        }
        for id in ["library.info", "photo.allMetadata", "web.stats"] {
            assert!(!waits_behind_export(id), "{id}");
        }
    }

    #[test]
    fn blank_or_null_params_are_an_empty_object() {
        for text in ["", "   ", "\n", "null", " null "] {
            assert_eq!(params(text), Ok(json!({})), "{text:?}");
        }
    }

    #[test]
    fn an_object_is_passed_through() {
        assert_eq!(params(r#"{"ids": [3], "rating": 5}"#), Ok(json!({"ids": [3], "rating": 5})));
    }

    /// Before, any of these ran the command with `{}`: `photo.delete` then removed the selection.
    #[test]
    fn invalid_json_or_a_non_object_is_refused() {
        for text in ["{ids: [3]}", "{\"ids\": [3]", "ids=3", "[3]", "3", "\"{}\"", "true"] {
            assert!(params(text).is_err(), "{text:?} must not run the command");
        }
        assert!(params("{ids: [3]}").unwrap_err().contains("nothing was run"));
    }
}
