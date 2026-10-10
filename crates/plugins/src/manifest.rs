//! The plug-in manifest (`dac_manifest`, UTF-8 JSON): identity, requested capabilities and what
//! the plug-in contributes (commands with declarative dialogs, hooks, a publish service).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::permissions::Permissions;
use crate::{Error, Result};

/// Longest manifest the host reads.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Most commands one plug-in may declare.
pub const MAX_COMMANDS: usize = 64;
/// Most fields one dialog may have.
pub const MAX_FIELDS: usize = 32;

/// The manifest a plug-in returns from `dac_manifest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Stable id, `[A-Za-z0-9._-]{1,64}` (reverse-DNS style: `org.example.geotag`).
    pub id: String,
    /// Display name.
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// Capabilities the plug-in asks for; the user grants them on install.
    #[serde(default)]
    pub permissions: Permissions,
    /// Menu commands.
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    #[serde(default)]
    pub hooks: Hooks,
    /// A publish service this plug-in provides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<PublishDecl>,
}

/// A menu command. It runs as the engine command `plugin.run {plugin, command, args}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDecl {
    /// `[A-Za-z0-9._-]{1,64}`, unique within the plug-in.
    pub id: String,
    pub label: String,
    /// Where the UI lists it, `>`-separated under the Plug-in Extras menu (`""` = top level).
    #[serde(default)]
    pub menu: String,
    /// A dialog asked before the command runs; its values arrive as `args`.
    #[serde(default)]
    pub dialog: Vec<Field>,
}

/// Which hooks the plug-in implements.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hooks {
    /// Post-process exported files (`{"hook": "export"}`).
    #[serde(default)]
    pub export: bool,
    /// Suggest metadata and keywords for photos (`{"hook": "metadata"}`).
    #[serde(default)]
    pub metadata: bool,
}

/// A publish service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishDecl {
    /// Service id, unique among the plug-in's services.
    pub id: String,
    /// Display name ("WebDAV").
    pub name: String,
}

/// One dialog field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: FieldKind,
}

/// A field's type and default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FieldKind {
    Text {
        #[serde(default)]
        default: String,
    },
    Number {
        min: f64,
        max: f64,
        default: f64,
    },
    Bool {
        #[serde(default)]
        default: bool,
    },
    Choice {
        options: Vec<String>,
        #[serde(default)]
        default: Option<String>,
    },
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Manifest {
    /// Parses and validates a manifest.
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Manifest(format!("{} bytes (limit {MAX_MANIFEST_BYTES})", bytes.len())));
        }
        let m: Manifest = serde_json::from_slice(bytes).map_err(|e| Error::Manifest(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(Error::Manifest(m));
        if !valid_id(&self.id) {
            return bad(format!("id {:?} must be 1-64 characters of A-Z a-z 0-9 . _ -", self.id));
        }
        if self.name.trim().is_empty() || self.name.len() > 128 {
            return bad("name must be 1-128 bytes".into());
        }
        self.permissions.validate().map_err(Error::Manifest)?;
        if self.commands.len() > MAX_COMMANDS {
            return bad(format!("at most {MAX_COMMANDS} commands"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for c in &self.commands {
            if !valid_id(&c.id) || !seen.insert(c.id.as_str()) {
                return bad(format!("command id {:?} is invalid or repeated", c.id));
            }
            if c.label.trim().is_empty() || c.label.len() > 128 || c.menu.len() > 256 {
                return bad(format!("command {:?}: label must be 1-128 bytes, menu at most 256", c.id));
            }
            if c.dialog.len() > MAX_FIELDS {
                return bad(format!("command {:?}: at most {MAX_FIELDS} dialog fields", c.id));
            }
            for f in &c.dialog {
                f.validate().map_err(|e| Error::Manifest(format!("command {:?}: {e}", c.id)))?;
            }
        }
        if let Some(p) = &self.publish
            && (!valid_id(&p.id) || p.name.trim().is_empty())
        {
            return bad("publish service needs an id and a name".into());
        }
        Ok(())
    }

    pub fn command(&self, id: &str) -> Option<&CommandDecl> {
        self.commands.iter().find(|c| c.id == id)
    }
}

impl Field {
    fn validate(&self) -> std::result::Result<(), String> {
        if !valid_id(&self.key) || self.label.len() > 128 {
            return Err(format!("field {:?}: bad key or label", self.key));
        }
        match &self.kind {
            FieldKind::Number { min, max, default } if !(min.is_finite() && max.is_finite() && min <= max && (min..=max).contains(&default)) => {
                Err(format!("field {:?}: needs finite min <= default <= max", self.key))
            }
            FieldKind::Choice { options, default }
                if options.is_empty() || options.len() > 64 || default.as_ref().is_some_and(|d| !options.contains(d)) =>
            {
                Err(format!("field {:?}: 1-64 options, default among them", self.key))
            }
            FieldKind::Text { default } if default.len() > 4096 => Err(format!("field {:?}: default too long", self.key)),
            _ => Ok(()),
        }
    }

    pub fn default_value(&self) -> Value {
        match &self.kind {
            FieldKind::Text { default } => Value::from(default.clone()),
            FieldKind::Number { default, .. } => Value::from(*default),
            FieldKind::Bool { default } => Value::from(*default),
            FieldKind::Choice { options, default } => Value::from(default.clone().or_else(|| options.first().cloned()).unwrap_or_default()),
        }
    }

    /// `v` checked against the field (numbers clamped), or an error naming the field.
    pub fn coerce(&self, v: &Value) -> std::result::Result<Value, String> {
        let wrong = || format!("`{}` must be {}", self.key, self.kind_name());
        match &self.kind {
            FieldKind::Text { .. } => v.as_str().filter(|s| s.len() <= 64 * 1024).map(Value::from).ok_or_else(wrong),
            FieldKind::Number { min, max, .. } => v.as_f64().filter(|x| x.is_finite()).map(|x| Value::from(x.clamp(*min, *max))).ok_or_else(wrong),
            FieldKind::Bool { .. } => v.as_bool().map(Value::from).ok_or_else(wrong),
            FieldKind::Choice { options, .. } => v.as_str().filter(|s| options.iter().any(|o| o == s)).map(Value::from).ok_or_else(wrong),
        }
    }

    fn kind_name(&self) -> &'static str {
        match self.kind {
            FieldKind::Text { .. } => "text",
            FieldKind::Number { .. } => "a number",
            FieldKind::Bool { .. } => "true or false",
            FieldKind::Choice { .. } => "one of the options",
        }
    }
}

/// The dialog values for `fields` from `args`: defaults for missing keys, checked values, unknown
/// keys passed through (a command may take extra arguments from agents).
pub fn dialog_args(fields: &[Field], args: &Value) -> Result<Value> {
    let mut out = match args {
        Value::Object(m) => m.clone(),
        Value::Null => Map::new(),
        _ => return Err(Error::Params("args must be an object".into())),
    };
    for f in fields {
        let v = match out.get(&f.key) {
            Some(v) => f.coerce(v).map_err(Error::Params)?,
            None => f.default_value(),
        };
        out.insert(f.key.clone(), v);
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_full_manifest() {
        let m = Manifest::parse(
            json!({
                "id": "org.example.geo", "name": "Geo", "version": "1.0",
                "permissions": {"catalog": true, "network": ["api.example.com"]},
                "commands": [{"id": "lookup", "label": "Look Up Places", "menu": "Metadata",
                    "dialog": [{"key": "radius", "label": "Radius", "type": "number", "min": 0, "max": 10, "default": 1},
                               {"key": "lang", "label": "Language", "type": "choice", "options": ["en", "de"]}]}],
                "hooks": {"metadata": true},
                "publish": {"id": "webdav", "name": "WebDAV"}
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert!(m.permissions.catalog && m.hooks.metadata && !m.hooks.export);
        let c = m.command("lookup").unwrap();
        let a = dialog_args(&c.dialog, &json!({"radius": 50})).unwrap();
        assert_eq!(a, json!({"radius": 10.0, "lang": "en"}));
        assert!(dialog_args(&c.dialog, &json!({"lang": "fr"})).is_err());
    }

    #[test]
    fn rejects_bad_manifests() {
        for bad in [
            json!({"id": "", "name": "x"}),
            json!({"id": "a b", "name": "x"}),
            json!({"id": "a", "name": " "}),
            json!({"id": "a", "name": "x", "commands": [{"id": "c", "label": "C"}, {"id": "c", "label": "D"}]}),
            json!({"id": "a", "name": "x", "permissions": {"network": ["no spaces allowed"]}}),
            json!({"id": "a", "name": "x", "commands": [{"id": "c", "label": "C", "dialog": [{"key": "n", "label": "N", "type": "number", "min": 2, "max": 1, "default": 1}]}]}),
        ] {
            assert!(Manifest::parse(bad.to_string().as_bytes()).is_err(), "{bad}");
        }
        assert!(Manifest::parse(b"not json").is_err());
    }
}
