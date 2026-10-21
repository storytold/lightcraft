//! Bounded, names-only diagnostics for saved settings that were not carried over.

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const MAX_NAMES: usize = 256;
pub const MAX_NAME_BYTES: usize = 256;
// Valid names do not contain JSON escape characters (quotes, backslashes, controls).
pub const MAX_JSON_BYTES: usize = MAX_NAMES * (MAX_NAME_BYTES + 3) + 1;
pub const INVALID_DIAGNOSTICS: &str = "InvalidSavedEditDiagnostics";

pub fn name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.trim() == name
        && name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/' | ' ' | '(' | ')' | '?'))
}

fn mark_invalid(fields: &mut Vec<String>) {
    if !fields.iter().any(|name| name == INVALID_DIAGNOSTICS) {
        if fields.len() >= MAX_NAMES {
            fields.pop();
        }
        fields.push(INVALID_DIAGNOSTICS.into());
    }
}

/// Keep valid identifiers, deduplicate, and retain an explicit diagnostic if input was invalid
/// or distinct names exceeded the cap. Existing names should come first when merging partial edits.
pub fn bounded_names<'a>(fields: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut invalid = false;
    for name in fields {
        if !name_valid(name) {
            invalid = true;
        } else if out.iter().any(|other| other == name) {
            continue;
        } else if out.len() < MAX_NAMES {
            out.push(name.to_string());
        } else {
            invalid = true;
        }
    }
    if invalid {
        mark_invalid(&mut out);
    }
    out.sort_unstable();
    out
}

struct DiagnosticName(Option<String>);

impl<'de> Deserialize<'de> for DiagnosticName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NameVisitor;
        impl<'de> Visitor<'de> for NameVisitor {
            type Value = DiagnosticName;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a saved-setting field name")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(DiagnosticName(name_valid(value).then(|| value.to_string())))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(DiagnosticName(None))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(DiagnosticName(None))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(DiagnosticName(None))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(DiagnosticName(None))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(DiagnosticName(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(DiagnosticName(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(DiagnosticName(None))
            }
        }
        deserializer.deserialize_any(NameVisitor)
    }
}

/// Missing fields use serde's default. Present invalid values are consumed without building
/// arbitrary value trees, while retained names remain bounded even on native library loads.
pub fn deserialize_names<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    struct NamesVisitor;
    impl<'de> Visitor<'de> for NamesVisitor {
        type Value = Vec<String>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a list of saved-setting field names")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut fields = Vec::new();
            let mut invalid = false;
            for _ in 0..MAX_NAMES {
                let Some(DiagnosticName(name)) = seq.next_element()? else {
                    if invalid {
                        mark_invalid(&mut fields);
                    }
                    fields.sort_unstable();
                    return Ok(fields);
                };
                match name {
                    Some(name) if !fields.contains(&name) => fields.push(name),
                    Some(_) => {}
                    None => invalid = true,
                }
            }
            // Never retain more names or recursively materialize nested values after the cap.
            while seq.next_element::<IgnoredAny>()?.is_some() {
                invalid = true;
            }
            if invalid {
                mark_invalid(&mut fields);
            }
            fields.sort_unstable();
            Ok(fields)
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(vec![INVALID_DIAGNOSTICS.into()])
        }
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(vec![INVALID_DIAGNOSTICS.into()])
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(vec![INVALID_DIAGNOSTICS.into()])
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(vec![INVALID_DIAGNOSTICS.into()])
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(vec![INVALID_DIAGNOSTICS.into()])
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(vec![INVALID_DIAGNOSTICS.into()])
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(vec![INVALID_DIAGNOSTICS.into()])
        }
    }
    deserializer.deserialize_any(NamesVisitor)
}

/// Also bound direct in-memory constructors before native serialization.
pub fn serialize_names<S: Serializer>(fields: &[String], serializer: S) -> Result<S::Ok, S::Error> {
    bounded_names(fields.iter().map(String::as_str)).serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Diagnostics {
        #[serde(default, deserialize_with = "deserialize_names", serialize_with = "serialize_names")]
        unsupported_edits: Vec<String>,
    }

    fn diagnostics(fields: Option<Value>) -> Diagnostics {
        let mut value = json!({});
        if let Some(fields) = fields {
            value["unsupported_edits"] = fields;
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn old_native_diagnostics_default_empty_and_invalid_values_remain_visible() {
        assert!(diagnostics(None).unsupported_edits.is_empty());
        for invalid in [
            Value::Null,
            json!({"OwnPayload":"discard me"}),
            json!(42),
            json!(["FilterList", "{not a field}", 7]),
            json!([{"OwnPayload": ["discard me", {"nested":true}]}, "FilterList"]),
        ] {
            let p = diagnostics(Some(invalid));
            assert!(p.unsupported_edits.iter().any(|name| name == INVALID_DIAGNOSTICS));
            assert!(p.unsupported_edits.iter().all(|name| name_valid(name)));
            let encoded = serde_json::to_string(&p).unwrap();
            assert!(!encoded.contains("discard me") && !encoded.contains("not a field"));
            assert_eq!(serde_json::from_str::<Diagnostics>(&encoded).unwrap(), p);
        }
    }

    #[test]
    fn maximal_valid_names_round_trip_and_oversized_names_have_a_marker() {
        let names: Vec<String> = (0..MAX_NAMES).map(|i| format!("Field{i:03}{}", "A".repeat(MAX_NAME_BYTES - 8))).collect();
        let p = diagnostics(Some(json!(names)));
        assert_eq!(p.unsupported_edits, names);
        assert_eq!(serde_json::from_str::<Diagnostics>(&serde_json::to_string(&p).unwrap()).unwrap(), p);
        let too_many: Vec<String> = (0..MAX_NAMES + 20).map(|i| format!("Field{i:03}")).collect();
        for fields in [json!(too_many), json!(["A".repeat(MAX_NAME_BYTES + 1)])] {
            let p = diagnostics(Some(fields));
            assert!(p.unsupported_edits.len() <= MAX_NAMES);
            assert!(p.unsupported_edits.iter().any(|name| name == INVALID_DIAGNOSTICS));
        }
    }

    #[test]
    fn union_deduplicates_without_false_overflow_and_serialization_is_bounded() {
        let names: Vec<String> = (0..MAX_NAMES).map(|i| format!("Field{i:03}")).collect();
        let merged = bounded_names(names.iter().chain(&names).map(String::as_str));
        assert_eq!(merged, names);
        let mut p = diagnostics(None);
        p.unsupported_edits = vec!["FilterList".into(), "{payload}".into(), "X".repeat(MAX_NAME_BYTES + 1)];
        let read: Diagnostics = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(read.unsupported_edits, ["FilterList", INVALID_DIAGNOSTICS]);
    }
}
