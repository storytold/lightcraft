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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
