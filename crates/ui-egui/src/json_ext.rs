//! Checked `serde_json::Value` field access for the UI (docs/gaps.md gap 40).
//!
//! `value["key"]` never panics on a read (a missing key or a non-object gives `Null`), but the
//! `indexing_slicing` lint can't tell that apart from slice indexing, and `value["key"] = x`
//! *does* panic when `value` is neither an object nor `Null`. [`JsonExt::field`] is the read
//! with the same `Null` default; [`JsonExt::field_mut`] is the write, which turns a non-object
//! into an empty object instead of panicking.

use serde_json::{Map, Value};

static NULL: Value = Value::Null;

/// Field access on JSON values that never panics.
pub(crate) trait JsonExt {
    /// The value under `key`, or `Null` when it is missing or `self` isn't an object
    /// (what `self[key]` returns).
    fn field(&self, key: &str) -> &Value;
    /// The value under `key`, inserted as `Null` when missing; a `self` that isn't an object
    /// becomes an empty object first (where `self[key]` on a non-object would panic).
    fn field_mut(&mut self, key: &str) -> &mut Value;
}

impl JsonExt for Value {
    fn field(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }

    fn field_mut(&mut self, key: &str) -> &mut Value {
        if !self.is_object() {
            *self = Value::Object(Map::new());
        }
        #[allow(clippy::indexing_slicing, reason = "IndexMut on an object inserts the key; it can't panic")]
        &mut self[key]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn field_reads_like_index() {
        let v = json!({"a": {"b": 1}, "s": "x"});
        assert_eq!(v.field("a").field("b"), &json!(1));
        assert!(v.field("missing").is_null());
        assert!(v.field("s").field("b").is_null());
        assert!(json!([1, 2]).field("a").is_null());
    }

    #[test]
    fn field_mut_writes_without_panicking() {
        let mut v = Value::Null;
        *v.field_mut("a").field_mut("b") = json!(2);
        assert_eq!(v, json!({"a": {"b": 2}}));
        let mut s = json!("text");
        *s.field_mut("k") = json!(true);
        assert_eq!(s, json!({"k": true}));
    }
}
