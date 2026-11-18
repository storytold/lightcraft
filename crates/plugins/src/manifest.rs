//! The plug-in manifest (`lcc_manifest`, UTF-8 JSON): identity, requested capabilities and what
//! the plug-in contributes (commands with declarative dialogs, hooks, a publish service, settings).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::about::About;
use crate::permissions::Permissions;
use crate::{Error, Result};

/// Longest manifest the host reads.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Most commands one plug-in may declare.
pub const MAX_COMMANDS: usize = 64;
/// Most fields one dialog may have.
pub const MAX_FIELDS: usize = 32;
/// Most export destinations one plug-in may declare.
pub const MAX_DESTINATIONS: usize = 16;
/// Most custom metadata fields one plug-in may declare.
pub const MAX_METADATA_FIELDS: usize = 32;
/// Most commands one plug-in may put in the quick settings bar (`quickAction`).
pub const MAX_QUICK_ACTIONS: usize = 4;
/// Most host features one plug-in may require.
pub const MAX_REQUIRES: usize = 64;
/// Longest `version` the host reads.
pub const MAX_VERSION_BYTES: usize = 64;
/// Most options a choice field may have.
pub const MAX_OPTIONS: usize = 64;
/// Longest option of a choice field filled at run time.
pub const MAX_OPTION_BYTES: usize = 1024;
/// Longest text value a field takes.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The host features this version of the app has, by name. A plug-in lists the ones it uses in
/// `requires` and isn't installed (or loaded) where one is missing. Every manifest field,
/// permission group or host call added from now on gets a name here; what the host had before
/// `requires` existed is the baseline and has none.
pub const HOST_FEATURES: &[&str] = &[
    // `programs` in the permissions, `program.list` / `program.start` and the `program.end` hook
    "programs",
    // `settings` in the manifest, `settings.get` / `settings.set`, dialogs that remember their
    // values (`remember`), and the `command.prepare` / `settings.prepare` hooks
    "settings",
    // `libraryAdd` in the permissions and the `library.add` host call; `source` in the `command`
    // and `command.prepare` requests
    "libraryAdd",
    // `job` on a command and the `job.*` host calls (progress, cancel, `{continue: state}`)
    "jobs",
    // `pixels` in the permissions and the `photo.pixels` host call
    "pixels",
    // `crop` in the permissions and the `develop.setCrop` host call
    "crop",
    // `virtualCopy` in the permissions and the `photo.virtualCopy` host call
    "virtualCopy",
    // `icon` and `header` in the manifest, shown by the Plug-in Manager (`plugin.picture`)
    "pictures",
    // `homepage`, `repository`, `changelog` and `license` in the manifest, shown on the About tab
    // (`plugin.list` → `about`)
    "about",
    // `quickAction` on a command: a button in the quick settings bar that runs it
    "quickActions",
];

/// The manifest a plug-in returns from `lcc_manifest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Stable id, `[A-Za-z0-9._-]{1,64}` (reverse-DNS style: `org.example.geotag`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Semver `MAJOR.MINOR.PATCH` (checked on install, see [`Manifest::semver`]).
    #[serde(default)]
    pub version: String,
    /// Host features the plug-in uses ([`HOST_FEATURES`]): a minimum, never a maximum.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
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
    /// Export destinations: after the app writes the files, the plug-in delivers them
    /// (`{"hook": "export.destination"}`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub export_destinations: Vec<DestinationDecl>,
    /// Custom metadata fields shown in the Metadata panel; their values live in the plug-in's
    /// own namespace (the key is the field key).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_fields: Vec<MetadataField>,
    /// Settings, shown on the plug-in's page and kept for the whole app (`settings.get`); a field
    /// with `prepare` has its options filled by the `settings.prepare` hook.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Field>,
    /// Fields of the manifest JSON the host doesn't know, by path ([`ignored_fields`]): a typo, or
    /// a field from a newer host. Set by [`Manifest::parse`].
    #[serde(skip)]
    pub unknown_fields: Vec<String>,
    /// A square icon the host may show for the plug-in (`"icon": "<svg…>"`, drawn on the host's
    /// own background, so it suits dark and light themes), as a pixel filter's. Read by
    /// [`Manifest::parse`]; a picture that isn't an SVG document of at most [`MAX_PICTURE_BYTES`]
    /// is dropped (the host draws its own icon).
    #[serde(skip)]
    pub icon: Option<String>,
    /// A header picture the host may show in place of the plug-in's name (`"header": "<svg…>"`, or
    /// `{"dark": "<svg…>", "light": "<svg…>"}` for dark and light interface themes), as a pixel
    /// filter's. Read by [`Manifest::parse`]; a picture that isn't an SVG document of at most
    /// [`MAX_PICTURE_BYTES`] is dropped (no header).
    #[serde(skip)]
    pub header: Option<Header>,
    /// Where the plug-in comes from (`homepage`, `repository`, `changelog`, `license`). Read by
    /// [`Manifest::parse`]; a value the host can't use is dropped and named in [`Manifest::dropped`].
    #[serde(skip)]
    pub about: About,
    /// Manifest values [`Manifest::parse`] dropped, each with why (`"homepage: not an https://
    /// link"`): the install puts them in the plug-in's log.
    #[serde(skip)]
    pub dropped: Vec<String>,
}

/// The largest picture (`icon`, each `header` theme) a manifest may carry: a logo drawn as paths
/// is a few kilobytes. The manifest as a whole stays within [`MAX_MANIFEST_BYTES`].
pub const MAX_PICTURE_BYTES: usize = 24 * 1024;

/// A plug-in's header picture (see [`Manifest::header`]): SVG documents for dark and light
/// interface themes, at least one of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub dark: Option<String>,
    pub light: Option<String>,
}

/// `v` as an SVG document of at most [`MAX_PICTURE_BYTES`], or `None`.
fn svg_document(v: Option<&serde_json::Value>) -> Option<String> {
    let t = v?.as_str()?.trim();
    (t.len() <= MAX_PICTURE_BYTES && (t.starts_with("<svg") || t.starts_with("<?xml")) && t.contains("</svg>")).then(|| t.to_string())
}

impl Header {
    fn parse(v: &serde_json::Value) -> Option<Header> {
        let h = match v {
            serde_json::Value::String(_) => Header { dark: svg_document(Some(v)), light: None },
            _ => Header { dark: svg_document(v.get("dark")), light: svg_document(v.get("light")) },
        };
        (h.dark.is_some() || h.light.is_some()).then_some(h)
    }

    /// The picture for a dark (`true`) or light interface theme, or the other one when the
    /// plug-in gives only one.
    pub fn for_theme(&self, dark: bool) -> &str {
        let (want, other) = if dark { (&self.dark, &self.light) } else { (&self.light, &self.dark) };
        want.as_deref().or(other.as_deref()).unwrap_or_default()
    }
}

/// An export destination ("Upload to my server").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationDecl {
    /// `[A-Za-z0-9._-]{1,64}`, unique within the plug-in.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Settings asked in the Export dialog; their values arrive as `args`.
    #[serde(default)]
    pub dialog: Vec<Field>,
}

/// A custom metadata field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataField {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: FieldKind,
    /// Shown but not editable by the user (the plug-in fills it).
    #[serde(default)]
    pub read_only: bool,
}

impl MetadataField {
    /// The field as a dialog field (same checks, same coercion).
    pub fn as_field(&self) -> Field {
        Field { key: self.key.clone(), label: self.label.clone(), kind: self.kind.clone(), remember: true, prepare: false }
    }
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
    /// Ask the plug-in first (`{"hook": "command.prepare"}`): it shows the dialog with options
    /// and defaults of its own, runs the command without it, or explains why it can't run.
    #[serde(default, skip_serializing_if = "is_false")]
    pub prepare: bool,
    /// Runs as a job (host feature `jobs`): on a worker, with a row in the activity stack, its
    /// progress reported through the `job.*` host calls, and continued with `{continue: state}`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub job: bool,
    /// Also a button in the quick settings bar (host feature `quickActions`; an older host ignores
    /// it), drawn with the plug-in's icon and named by `label`; at most [`MAX_QUICK_ACTIONS`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub quick_action: bool,
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

/// One dialog or settings field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: FieldKind,
    /// A dialog field opens with the value it was last run with (`false`: always its default).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub remember: bool,
    /// A settings field whose options and default the `settings.prepare` hook fills.
    #[serde(default, skip_serializing_if = "is_false")]
    pub prepare: bool,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
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
    /// `options` may be empty in a `prepare` command's dialog or a `prepare` settings field: the
    /// plug-in gives them at run time.
    Choice {
        #[serde(default)]
        options: Vec<String>,
        #[serde(default)]
        default: Option<String>,
    },
}

/// A plug-in version as semver (`MAJOR.MINOR.PATCH`, optionally `-pre` and `+build`), or why it isn't one.
pub fn parse_version(v: &str) -> std::result::Result<semver::Version, String> {
    let form = "it must be MAJOR.MINOR.PATCH, e.g. 1.4.0";
    if v.is_empty() {
        return Err(format!("no version: {form}"));
    }
    if v.len() > MAX_VERSION_BYTES {
        return Err(format!("version is {} bytes (limit {MAX_VERSION_BYTES}): {form}", v.len()));
    }
    semver::Version::parse(v).map_err(|e| format!("version {v:?} is not semver ({e}): {form}"))
}

/// At most 64 characters of `s`, for messages about hostile input.
fn short(s: &str) -> String {
    s.chars().take(64).collect()
}

/// Most fields [`ignored_fields`] reports, most it tests (known fields left out of the
/// serialised manifest at their defaults are candidates too, so it looks past 32), and how deep.
const MAX_IGNORED: usize = 32;
const MAX_CANDIDATES: usize = 256;
const MAX_IGNORED_DEPTH: usize = 16;

/// A step of a path into JSON.
#[derive(Clone)]
enum Step {
    Key(String),
    Index(usize),
}

/// The fields of manifest JSON `raw` that `parse` ignores, by path (`commands[2].lable`), at most
/// 32. A field is ignored when the manifest parsed from `raw` doesn't serialise it, and setting it
/// to a value no field takes (`[[[]]]`) still parses to the same manifest; so a known field at its
/// default isn't reported. `lenient`: paths (`*` for any key) of fields that drop a value they
/// can't use instead of failing, which that test can't tell from unknown ones; never reported.
/// Plug-in and pixel-filter manifests both use it (`serde(deny_unknown_fields)` doesn't work with
/// their `serde(flatten)`ed fields).
pub fn ignored_fields<T: Serialize + PartialEq>(raw: &Value, lenient: &[&str], parse: impl Fn(&Value) -> Option<T>) -> Vec<String> {
    let Some(base) = parse(raw) else { return Vec::new() };
    let Ok(ser) = serde_json::to_value(&base) else { return Vec::new() };
    let mut candidates = Vec::new();
    unserialised(raw, &ser, &mut Vec::new(), &mut candidates);
    candidates
        .into_iter()
        .filter(|path| !lenient.iter().any(|l| path_matches(l, path)))
        .filter(|path| {
            let mut probe = raw.clone();
            match value_at(&mut probe, path) {
                Some(slot) => *slot = json!([[[]]]),
                None => return false,
            }
            parse(&probe).is_some_and(|m| m == base)
        })
        .take(MAX_IGNORED)
        .map(|path| show_path(&path))
        .collect()
}

/// Paths of `raw`'s object keys that `ser` (the parsed manifest, serialised) lacks.
fn unserialised(raw: &Value, ser: &Value, path: &mut Vec<Step>, out: &mut Vec<Vec<Step>>) {
    if path.len() >= MAX_IGNORED_DEPTH {
        return;
    }
    match (raw, ser) {
        (Value::Object(r), Value::Object(s)) => {
            for (k, v) in r {
                if out.len() >= MAX_CANDIDATES {
                    return;
                }
                path.push(Step::Key(k.clone()));
                match s.get(k) {
                    Some(sv) => unserialised(v, sv, path, out),
                    None => out.push(path.clone()),
                }
                path.pop();
            }
        }
        (Value::Array(r), Value::Array(s)) if r.len() == s.len() => {
            for (i, (rv, sv)) in r.iter().zip(s).enumerate() {
                path.push(Step::Index(i));
                unserialised(rv, sv, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn value_at<'a>(v: &'a mut Value, path: &[Step]) -> Option<&'a mut Value> {
    path.iter().try_fold(v, |v, step| match step {
        Step::Key(k) => v.get_mut(k.as_str()),
        Step::Index(i) => v.get_mut(*i),
    })
}

/// Whether `pattern` (`params.*.label`; `*` is any key or index) names `path`.
fn path_matches(pattern: &str, path: &[Step]) -> bool {
    let parts: Vec<&str> = pattern.split('.').collect();
    parts.len() == path.len()
        && parts.iter().zip(path).all(|(p, step)| match step {
            Step::Key(k) => *p == "*" || p == k,
            Step::Index(_) => *p == "*",
        })
}

fn show_path(path: &[Step]) -> String {
    let mut s = String::new();
    for step in path {
        match step {
            Step::Key(k) if s.is_empty() => s.push_str(&short(k)),
            Step::Key(k) => {
                s.push('.');
                s.push_str(&short(k));
            }
            Step::Index(i) => s.push_str(&format!("[{i}]")),
        }
    }
    s
}

pub(crate) fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Plug-in manifest fields read leniently (a picture the host can't use is dropped, not an
/// error): never reported as unknown.
const LENIENT: &[&str] = &["icon", "header", "homepage", "repository", "changelog", "license"];

impl Manifest {
    /// Parses and validates a manifest.
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Manifest(format!("{} bytes (limit {MAX_MANIFEST_BYTES})", bytes.len())));
        }
        // the struct first: it refuses a key given twice, which a `Value` would keep the last of
        let mut m: Manifest = serde_json::from_slice(bytes).map_err(|e| Error::Manifest(e.to_string()))?;
        let raw: Value = serde_json::from_slice(bytes).map_err(|e| Error::Manifest(e.to_string()))?;
        // pictures are optional: one the host can't use is dropped, never an error
        m.icon = svg_document(raw.get("icon"));
        m.header = raw.get("header").and_then(Header::parse);
        (m.about, m.dropped) = About::parse(&raw);
        m.validate()?;
        m.unknown_fields = ignored_fields(&raw, LENIENT, |v| serde_json::from_value::<Manifest>(v.clone()).ok());
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
        if self.requires.len() > MAX_REQUIRES {
            return bad(format!("at most {MAX_REQUIRES} names in requires"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for r in &self.requires {
            if !valid_id(r) || !seen.insert(r.as_str()) {
                return bad(format!("requires: {:?} is invalid or repeated (names are 1-64 characters of A-Z a-z 0-9 . _ -)", short(r)));
            }
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
            if c.job && !self.requires.iter().any(|r| r == "jobs") {
                return bad(format!("command {:?}: a job needs \"jobs\" in `requires`", c.id));
            }
            for f in &c.dialog {
                f.validate(c.prepare).map_err(|e| Error::Manifest(format!("command {:?}: {e}", c.id)))?;
                if f.prepare {
                    return bad(format!("command {:?}: field {:?}: `prepare` is for settings fields (a command takes `prepare: true`)", c.id, f.key));
                }
            }
        }
        if self.commands.iter().filter(|c| c.quick_action).count() > MAX_QUICK_ACTIONS {
            return bad(format!("at most {MAX_QUICK_ACTIONS} commands with `quickAction`"));
        }
        if let Some(p) = &self.publish
            && (!valid_id(&p.id) || p.name.trim().is_empty())
        {
            return bad("publish service needs an id and a name".into());
        }
        if self.export_destinations.len() > MAX_DESTINATIONS {
            return bad(format!("at most {MAX_DESTINATIONS} export destinations"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for d in &self.export_destinations {
            if !valid_id(&d.id) || !seen.insert(d.id.as_str()) || d.name.trim().is_empty() || d.name.len() > 128 {
                return bad(format!("export destination {:?}: needs a unique id and a 1-128 byte name", d.id));
            }
            if d.dialog.len() > MAX_FIELDS {
                return bad(format!("export destination {:?}: at most {MAX_FIELDS} dialog fields", d.id));
            }
            for f in &d.dialog {
                f.validate(false).map_err(|e| Error::Manifest(format!("export destination {:?}: {e}", d.id)))?;
                if f.prepare {
                    return bad(format!("export destination {:?}: field {:?}: `prepare` is for settings fields", d.id, f.key));
                }
            }
        }
        if self.metadata_fields.len() > MAX_METADATA_FIELDS {
            return bad(format!("at most {MAX_METADATA_FIELDS} metadata fields"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in &self.metadata_fields {
            if !seen.insert(f.key.as_str()) || f.label.trim().is_empty() {
                return bad(format!("metadata field {:?}: needs a unique key and a label", f.key));
            }
            f.as_field().validate(false).map_err(|e| Error::Manifest(format!("metadata field: {e}")))?;
        }
        if self.settings.len() > MAX_FIELDS {
            return bad(format!("at most {MAX_FIELDS} settings"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in &self.settings {
            if !seen.insert(f.key.as_str()) || f.label.trim().is_empty() {
                return bad(format!("setting {:?}: needs a unique key and a label", short(&f.key)));
            }
            f.validate(f.prepare).map_err(|e| Error::Manifest(format!("setting: {e}")))?;
        }
        Ok(())
    }

    /// The features in `requires` that `known` lacks, in order.
    pub fn missing_features(&self, known: &[impl AsRef<str>]) -> Vec<String> {
        self.requires.iter().filter(|r| !known.iter().any(|k| k.as_ref() == r.as_str())).cloned().collect()
    }

    /// `version` as semver, or why it isn't one.
    pub fn semver(&self) -> std::result::Result<semver::Version, String> {
        parse_version(&self.version)
    }

    pub fn command(&self, id: &str) -> Option<&CommandDecl> {
        self.commands.iter().find(|c| c.id == id)
    }

    pub fn destination(&self, id: &str) -> Option<&DestinationDecl> {
        self.export_destinations.iter().find(|d| d.id == id)
    }

    pub fn metadata_field(&self, key: &str) -> Option<&MetadataField> {
        self.metadata_fields.iter().find(|f| f.key == key)
    }

    pub fn setting(&self, key: &str) -> Option<&Field> {
        self.settings.iter().find(|f| f.key == key)
    }
}

impl Field {
    /// `run_time`: the plug-in may give a choice's options at run time, so it may declare none.
    fn validate(&self, run_time: bool) -> std::result::Result<(), String> {
        if !valid_id(&self.key) || self.label.len() > 128 {
            return Err(format!("field {:?}: bad key or label", short(&self.key)));
        }
        match &self.kind {
            FieldKind::Number { min, max, default } if !(min.is_finite() && max.is_finite() && min <= max && (min..=max).contains(&default)) => {
                Err(format!("field {:?}: needs finite min <= default <= max", self.key))
            }
            FieldKind::Choice { options, .. } if options.is_empty() && !run_time => {
                Err(format!("field {:?}: 1-{MAX_OPTIONS} options (none only when the plug-in gives them at run time: `prepare`)", self.key))
            }
            FieldKind::Choice { options, default } if options.len() > MAX_OPTIONS || default.as_ref().is_some_and(|d| !options.contains(d)) => {
                Err(format!("field {:?}: at most {MAX_OPTIONS} options, default among them", self.key))
            }
            FieldKind::Text { default } if default.len() > 4096 => Err(format!("field {:?}: default too long", self.key)),
            _ => Ok(()),
        }
    }

    /// A choice whose options the plug-in gives at run time (it declares none).
    pub fn options_at_run_time(&self) -> bool {
        matches!(&self.kind, FieldKind::Choice { options, .. } if options.is_empty())
    }

    /// The field with the options and default a `prepare` hook gave (`{options?, default?}`), or
    /// why they don't fit it: options only for a choice (1-64 non-empty texts of at most 1024
    /// bytes), a default the field (with those options) takes.
    pub fn prepared(&self, o: &Value) -> std::result::Result<Field, String> {
        let o = o.as_object().ok_or_else(|| format!("`{}`: expected {{options?, default?}}", self.key))?;
        if let Some(k) = o.keys().find(|k| !matches!(k.as_str(), "options" | "default")) {
            return Err(format!("`{}`: unknown key {:?}", self.key, short(k)));
        }
        if self.options_at_run_time() && !o.contains_key("options") {
            return Err(format!("`{}`: no options given", self.key));
        }
        let mut f = self.clone();
        if let Some(opts) = o.get("options") {
            let FieldKind::Choice { options, default } = &mut f.kind else {
                return Err(format!("`{}`: only a choice field takes options", self.key));
            };
            let list = opts.as_array().filter(|a| (1..=MAX_OPTIONS).contains(&a.len()));
            let list = list.ok_or_else(|| format!("`{}`: options must be a list of 1-{MAX_OPTIONS} texts", self.key))?;
            let mut new = Vec::with_capacity(list.len());
            for v in list {
                let t = v.as_str().filter(|t| !t.is_empty() && t.len() <= MAX_OPTION_BYTES);
                new.push(t.ok_or_else(|| format!("`{}`: each option must be a text of 1-{MAX_OPTION_BYTES} bytes", self.key))?.to_string());
            }
            // the declared default stays only if it is still offered
            if default.as_ref().is_some_and(|d| !new.contains(d)) {
                *default = None;
            }
            *options = new;
        }
        if let Some(d) = o.get("default") {
            let v = f.coerce(d)?;
            match &mut f.kind {
                FieldKind::Text { default } => {
                    let t = v.as_str().unwrap_or_default();
                    // the same cap as a declared default
                    if t.len() > 4096 {
                        return Err(format!("`{}`: default too long", self.key));
                    }
                    *default = t.to_string();
                }
                FieldKind::Number { default, .. } => *default = v.as_f64().unwrap_or(*default),
                FieldKind::Bool { default } => *default = v.as_bool().unwrap_or(*default),
                FieldKind::Choice { default, .. } => *default = v.as_str().map(str::to_string),
            }
        }
        Ok(f)
    }

    pub fn default_value(&self) -> Value {
        match &self.kind {
            FieldKind::Text { default } => Value::from(default.clone()),
            FieldKind::Number { default, .. } => Value::from(*default),
            FieldKind::Bool { default } => Value::from(*default),
            FieldKind::Choice { options, default } => Value::from(default.clone().or_else(|| options.first().cloned()).unwrap_or_default()),
        }
    }

    /// A value written as text (an XMP sidecar's `lcpf:value`) read as this field's type, then
    /// checked like [`Field::coerce`].
    pub fn from_text(&self, t: &str) -> std::result::Result<Value, String> {
        let t = t.trim();
        let v = match &self.kind {
            FieldKind::Number { .. } => t.parse::<f64>().ok().filter(|x| x.is_finite()).map(Value::from).unwrap_or(Value::Null),
            FieldKind::Bool { .. } => match t.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Value::Bool(true),
                "false" | "0" | "no" => Value::Bool(false),
                _ => Value::Null,
            },
            _ => Value::from(t),
        };
        self.coerce(&v)
    }

    /// `v` checked against the field (numbers clamped), or an error naming the field.
    pub fn coerce(&self, v: &Value) -> std::result::Result<Value, String> {
        let wrong = || format!("`{}` must be {}", self.key, self.kind_name());
        match &self.kind {
            FieldKind::Text { .. } => v.as_str().filter(|s| s.len() <= MAX_TEXT_BYTES).map(Value::from).ok_or_else(wrong),
            FieldKind::Number { min, max, .. } => v.as_f64().filter(|x| x.is_finite()).map(|x| Value::from(x.clamp(*min, *max))).ok_or_else(wrong),
            FieldKind::Bool { .. } => v.as_bool().map(Value::from).ok_or_else(wrong),
            // options given at run time: any option the plug-in could have given (it checks them)
            FieldKind::Choice { options, .. } if options.is_empty() => {
                v.as_str().filter(|s| !s.is_empty() && s.len() <= MAX_OPTION_BYTES).map(Value::from).ok_or_else(wrong)
            }
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

    /// An icon and a header (one picture, or one per interface theme) are kept; a picture that
    /// isn't an SVG document, or is too large, is dropped without failing the manifest.
    #[test]
    fn pictures_are_kept_or_dropped() {
        let svg = "<svg xmlns='http://www.w3.org/2000/svg'><rect width='1' height='1'/></svg>";
        let with = |extra: serde_json::Value| {
            let mut m = json!({"id": "org.example.pic", "name": "Pic", "version": "1.0.0"});
            m.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            Manifest::parse(m.to_string().as_bytes()).unwrap()
        };
        let none = with(json!({}));
        assert!(none.icon.is_none() && none.header.is_none());
        let m = with(json!({"icon": svg, "header": svg}));
        assert_eq!(m.icon.as_deref(), Some(svg));
        let h = m.header.unwrap();
        assert_eq!((h.for_theme(true), h.for_theme(false)), (svg, svg), "one picture serves both themes");
        let m = with(json!({"header": {"light": svg}}));
        assert_eq!(m.header.as_ref().map(|h| h.for_theme(true)), Some(svg));
        for bad in [json!({"icon": "<html></html>"}), json!({"icon": 7}), json!({"header": {"dark": "png bytes"}}), json!({"header": [svg]})] {
            let m = with(bad.clone());
            assert!(m.icon.is_none() && m.header.is_none(), "{bad}");
        }
        let huge = format!("<svg>{}</svg>", "x".repeat(MAX_PICTURE_BYTES));
        assert!(with(json!({"icon": huge})).icon.is_none());
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

    #[test]
    fn destinations_and_metadata_fields() {
        let m = Manifest::parse(
            json!({"id": "a", "name": "A",
                "exportDestinations": [{"id": "srv", "name": "My Server", "dialog": [{"key": "album", "label": "Album", "type": "text"}]}],
                "metadataFields": [{"key": "job", "label": "Job", "type": "text"},
                    {"key": "score", "label": "Score", "type": "number", "min": 0, "max": 10, "default": 0, "readOnly": true}]})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(m.destination("srv").unwrap().dialog.len(), 1);
        assert!(m.metadata_field("score").unwrap().read_only);
        assert_eq!(m.metadata_field("score").unwrap().as_field().coerce(&json!(99)).unwrap(), json!(10.0));
        for bad in [
            json!({"id": "a", "name": "A", "exportDestinations": [{"id": "x", "name": "X"}, {"id": "x", "name": "Y"}]}),
            json!({"id": "a", "name": "A", "exportDestinations": [{"id": "bad id", "name": "X"}]}),
            json!({"id": "a", "name": "A", "metadataFields": [{"key": "k", "label": "K", "type": "text"}, {"key": "k", "label": "L", "type": "text"}]}),
            json!({"id": "a", "name": "A", "metadataFields": [{"key": "k k", "label": "K", "type": "text"}]}),
            json!({"id": "a", "name": "A", "metadataFields": [{"key": "c", "label": "C", "type": "choice", "options": []}]}),
        ] {
            assert!(Manifest::parse(bad.to_string().as_bytes()).is_err(), "{bad}");
        }
    }

    /// A command may also be a quick settings bar button (`quickAction`), a few per plug-in; a
    /// manifest without it reads as before, and serialises without it.
    #[test]
    fn quick_actions_are_flagged_commands_and_capped() {
        let m = Manifest::parse(
            json!({"id": "a", "name": "A", "commands": [{"id": "go", "label": "Go", "quickAction": true}, {"id": "menu", "label": "Menu"}]})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        assert!(m.command("go").unwrap().quick_action && !m.command("menu").unwrap().quick_action);
        assert!(m.unknown_fields.is_empty(), "{:?}", m.unknown_fields);
        let menu = serde_json::to_value(m.command("menu").unwrap()).unwrap();
        assert!(menu.get("quickAction").is_none(), "{menu}");
        let many: Vec<Value> = (0..=MAX_QUICK_ACTIONS).map(|i| json!({"id": format!("c{i}"), "label": "C", "quickAction": true})).collect();
        let err = Manifest::parse(json!({"id": "a", "name": "A", "commands": many}).to_string().as_bytes()).unwrap_err();
        assert!(err.to_string().contains("quickAction"), "{err}");
        assert!(HOST_FEATURES.contains(&"quickActions"));
    }

    #[test]
    fn settings_prepare_and_remember() {
        let m = Manifest::parse(
            json!({"id": "a", "name": "A",
                "commands": [{"id": "go", "label": "Go", "prepare": true, "dialog": [
                    {"key": "v", "label": "V", "type": "choice", "options": []},
                    {"key": "once", "label": "Once", "type": "bool", "remember": false}]}],
                "settings": [{"key": "v", "label": "V", "type": "choice", "prepare": true},
                    {"key": "n", "label": "N", "type": "number", "min": 0, "max": 1, "default": 0}]})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let go = m.command("go").unwrap();
        assert!(go.prepare && go.dialog[0].remember && !go.dialog[1].remember);
        let v = m.setting("v").unwrap();
        assert!(v.prepare && v.options_at_run_time());
        assert_eq!(Manifest::parse(&serde_json::to_vec(&m).unwrap()).unwrap(), m, "written back as read (plugin.list)");
        // until the plug-in gives the options, any option it could give
        assert_eq!(v.coerce(&json!("4.2")).unwrap(), "4.2");
        assert!(v.coerce(&json!("")).is_err() && v.coerce(&json!("x".repeat(MAX_OPTION_BYTES + 1))).is_err());
        // what a prepare step may give
        let p = v.prepared(&json!({"options": ["1", "2"], "default": "2"})).unwrap();
        assert_eq!(p.default_value(), "2");
        assert!(p.coerce(&json!("3")).is_err(), "only what it offered");
        for bad in [json!({"options": []}), json!({"options": [""]}), json!({"options": ["1"], "default": "3"}), json!({"extra": 1}), json!("1")] {
            assert!(v.prepared(&bad).is_err(), "{bad}");
        }
        let n = m.setting("n").unwrap();
        assert!(n.prepared(&json!({"options": ["a"]})).is_err(), "options only for a choice");
        assert_eq!(n.prepared(&json!({"default": 5})).unwrap().default_value(), json!(1.0), "clamped");
        let t: Field = serde_json::from_value(json!({"key": "t", "label": "T", "type": "text"})).unwrap();
        assert_eq!(t.prepared(&json!({"default": "x".repeat(4096)})).unwrap().default_value(), json!("x".repeat(4096)));
        assert!(t.prepared(&json!({"default": "x".repeat(4097)})).is_err(), "no longer than a declared default");

        let choice = |options: Value| json!({"key": "c", "label": "C", "type": "choice", "options": options});
        for bad in [
            json!({"id": "a", "name": "A", "commands": [{"id": "go", "label": "Go", "dialog": [choice(json!([]))]}]}),
            json!({"id": "a", "name": "A", "settings": [choice(json!([]))]}),
            json!({"id": "a", "name": "A", "commands": [{"id": "go", "label": "Go", "prepare": true, "dialog": [{"key": "c", "label": "C", "type": "text", "prepare": true}]}]}),
            json!({"id": "a", "name": "A", "exportDestinations": [{"id": "d", "name": "D", "dialog": [choice(json!([]))]}]}),
            json!({"id": "a", "name": "A", "settings": [choice(json!(["x"])), choice(json!(["y"]))]}),
            json!({"id": "a", "name": "A", "settings": [{"key": "t", "label": " ", "type": "text"}]}),
            json!({"id": "a", "name": "A", "settings": (0..=MAX_FIELDS).map(|i| json!({"key": format!("k{i}"), "label": "K", "type": "bool"})).collect::<Vec<_>>()}),
        ] {
            assert!(Manifest::parse(bad.to_string().as_bytes()).is_err(), "{bad}");
        }
    }
}
