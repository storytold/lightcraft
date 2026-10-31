//! Slideshow templates (Lightroom Classic's Template Browser): a named snapshot of how a
//! slideshow looks and plays — the Playback settings, the slide's Layout / Options / Backdrop,
//! the Overlays and the Titles — as one JSON document with a format version.
//!
//! Built-in templates are our own designs ([`builtins`], JSON files beside this module). User
//! templates are files in the settings folder, `<config>/slideshow-templates/<name>.json`
//! ([`Session::slide_templates_dir`]); they are shared by every library on the machine, like
//! Lightroom's templates. Every section is read through the settings' own `merged()` (so a
//! hand-edited or hostile file can never set a value the panels couldn't), files are capped in
//! size and number, a file that can't be used is reported and skipped, and file names are made
//! from the template's name so they can never leave the templates folder.
//!
//! Applying a template replaces the four settings in one step and saves them with the library.
//! Library preferences have no undo, so the settings it replaced are kept for the session as
//! "Previous settings" ([`PREVIOUS`]): applying that puts them back (and applying it again goes
//! forward). "Prepare previews" is a machine preference, not part of a look: templates neither
//! save nor change it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Session;
use crate::slide::SlideSettings;
use crate::slide_overlay::OverlaySettings;
use crate::slide_title::TitleSettings;
use crate::slideshow::PlaybackSettings;

/// The `format` of a template document.
pub const FORMAT: &str = "lightcraft.slideshowTemplate";
/// The newest document version this build reads (and the one it writes).
pub const VERSION: u64 = 1;
/// Largest template file read (bytes).
pub const MAX_FILE: u64 = 256 * 1024;
/// Most user templates listed (the rest of a folder that holds more are reported, not read).
pub const MAX_TEMPLATES: usize = 1000;
/// Directory entries looked at before giving up on a huge folder.
const MAX_SCAN: usize = 20_000;
/// Longest template name (characters).
pub const NAME_MAX: usize = 80;
/// Longest description (characters).
pub const DESCRIPTION_MAX: usize = 500;
/// Longest file stem made from a name (characters).
const STEM_MAX: usize = 64;
/// The folder under the settings folder user templates live in.
pub const DIR_NAME: &str = "slideshow-templates";
/// Id prefix of built-in templates.
pub const BUILTIN: &str = "builtin:";
/// Id prefix of user templates (followed by the file stem).
pub const USER: &str = "user:";
/// Id of the settings before the last template was applied.
pub const PREVIOUS: &str = "previous";

/// Everything a template sets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlideLook {
    pub playback: PlaybackSettings,
    pub layout: SlideSettings,
    pub overlays: OverlaySettings,
    pub titles: TitleSettings,
}

impl SlideLook {
    /// The library's current slideshow settings.
    pub fn of(s: &Session) -> SlideLook {
        SlideLook {
            playback: s.slideshow.clone(),
            layout: s.slide_layout.clone(),
            overlays: s.slide_overlays.clone(),
            titles: s.slide_titles.clone(),
        }
    }

    /// The four sections as JSON (playback without `preparePreviews`, which isn't part of a look).
    pub fn sections(&self) -> Map<String, Value> {
        let mut playback = self.playback.to_json();
        if let Some(o) = playback.as_object_mut() {
            o.remove("preparePreviews");
        }
        let mut m = Map::new();
        m.insert("playback".into(), playback);
        m.insert("layout".into(), self.layout.to_json());
        m.insert("overlays".into(), self.overlays.to_json());
        m.insert("titles".into(), self.titles.to_json());
        m
    }

    /// Read the sections of a template document: each one merged onto the defaults by the
    /// settings' own validation (a missing section is the defaults; a wrong type, an unknown key
    /// or a non-finite number is an error).
    fn from_sections(doc: &Map<String, Value>) -> Result<SlideLook, String> {
        fn section<T>(doc: &Map<String, Value>, key: &str, merge: impl Fn(&Value) -> Result<T, String>, default: T) -> Result<T, String> {
            match doc.get(key) {
                None | Some(Value::Null) => Ok(default),
                Some(v @ Value::Object(_)) => merge(v).map_err(|e| format!("{key}: {e}")),
                Some(v) => Err(format!("`{key}` must be an object, not {v}")),
            }
        }
        let mut playback = section(doc, "playback", |v| PlaybackSettings::default().merged(v), PlaybackSettings::default())?;
        playback.prepare_previews = PlaybackSettings::default().prepare_previews;
        Ok(SlideLook {
            playback,
            layout: section(doc, "layout", |v| SlideSettings::default().merged(v), SlideSettings::default())?,
            overlays: section(doc, "overlays", |v| OverlaySettings::default().merged(v), OverlaySettings::default())?,
            titles: section(doc, "titles", |v| TitleSettings::default().merged(v), TitleSettings::default())?,
        })
    }

    /// What doesn't work with this look in `s`'s library: a background photo that isn't in it,
    /// an identity plate or watermark image file that can't be read, unknown tokens, a title
    /// screen with nothing to show. The look still applies; these draw nothing.
    pub fn warnings(&self, s: &Session) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(id) = self.layout.background_image
            && s.catalog.photo(lightcraft_catalog::PhotoId(id)).is_none()
        {
            out.push(format!("the background image (photo {id}) isn't in this library; the backdrop colour shows instead"));
        }
        out.extend(self.overlays.warnings());
        let w = &self.overlays.watermark;
        if self.overlays.show_watermark && !w.image.trim().is_empty() && crate::export::watermark_logo(w.image.trim()).is_none() {
            out.push(format!("watermark image {} can't be read; nothing is drawn", w.image.trim()));
        }
        for t in self.titles.warnings(&self.overlays) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }
}

/// A template: built in or a user's file.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    /// `builtin:<slug>` or `user:<file stem>`.
    pub id: String,
    pub name: String,
    pub description: String,
    pub builtin: bool,
    /// The file a user template is read from.
    pub path: Option<PathBuf>,
    pub look: SlideLook,
}

impl Template {
    /// The template as a document (what a user template's file holds, and what export writes).
    pub fn to_doc(&self) -> Value {
        doc(&self.name, &self.description, &self.look)
    }

    /// The template for listings (no settings).
    pub fn info(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "name": self.name,
            "builtin": self.builtin,
            "group": if self.builtin { "builtin" } else { "user" },
        });
        if !self.description.is_empty() {
            v["description"] = json!(self.description);
        }
        if let Some(p) = &self.path {
            v["path"] = json!(p.to_string_lossy());
        }
        v
    }
}

/// A template document for `look`.
pub fn doc(name: &str, description: &str, look: &SlideLook) -> Value {
    let mut m = Map::new();
    m.insert("format".into(), json!(FORMAT));
    m.insert("version".into(), json!(VERSION));
    m.insert("name".into(), json!(name));
    if !description.is_empty() {
        m.insert("description".into(), json!(description));
    }
    m.extend(look.sections());
    Value::Object(m)
}

/// A template document read and checked: its name (`None` when it has none), description and look.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub name: Option<String>,
    pub description: String,
    pub look: SlideLook,
}

/// Read a template document. Refused: more than [`MAX_FILE`] bytes, not JSON, another format, a
/// version from a newer LightCraft, a bad name, unknown keys and any setting its section's
/// validation refuses.
pub fn parse(bytes: &[u8]) -> Result<Parsed, String> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(format!("{} bytes is too large for a slideshow template (at most {} KB)", bytes.len(), MAX_FILE / 1024));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "not a slideshow template (not UTF-8 text)".to_string())?;
    let text = text.trim_start_matches('\u{feff}');
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not a slideshow template: {e}"))?;
    let Value::Object(o) = v else { return Err("not a slideshow template (not a JSON object)".into()) };
    match o.get("format").and_then(Value::as_str) {
        Some(FORMAT) => {}
        Some(other) => return Err(format!("not a slideshow template (format `{other}`, expected `{FORMAT}`)")),
        None => return Err(format!("not a slideshow template (no `format`: \"{FORMAT}\")")),
    }
    match o.get("version") {
        Some(v) => match v.as_u64() {
            Some(n) if n > VERSION => {
                return Err(format!(
                    "made by a newer LightCraft (template version {n}; this one reads version {VERSION}): update LightCraft to use it"
                ));
            }
            Some(n) if n >= 1 => {}
            _ => return Err(format!("`version` must be a whole number from 1, not {v}")),
        },
        None => return Err("no `version`".into()),
    }
    for k in o.keys() {
        if !matches!(k.as_str(), "format" | "version" | "name" | "description" | "playback" | "layout" | "overlays" | "titles") {
            return Err(format!("unknown key `{k}` (format, version, name, description, playback, layout, overlays, titles)"));
        }
    }
    let name = match o.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) if n.trim().is_empty() => None,
        Some(Value::String(n)) => Some(valid_name(n)?),
        Some(v) => return Err(format!("`name` must be text, not {v}")),
    };
    let description = match o.get("description") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(d)) => clean_text(d, DESCRIPTION_MAX),
        Some(v) => return Err(format!("`description` must be text, not {v}")),
    };
    Ok(Parsed { name, description, look: SlideLook::from_sections(&o)? })
}

/// `s` without control characters, trimmed, at most `max` characters.
fn clean_text(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string()
}

/// A template name as stored: trimmed, 1–[`NAME_MAX`] characters, no control characters.
pub fn valid_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("a template needs a name".into());
    }
    if n.chars().any(char::is_control) {
        return Err("a template name can't contain control characters (line breaks, tabs…)".into());
    }
    if n.chars().count() > NAME_MAX {
        return Err(format!("a template name has at most {NAME_MAX} characters"));
    }
    Ok(n.to_string())
}

/// The file stem a template called `name` is saved as: letters, digits, spaces, `-`, `_`, `(`
/// and `)` kept, anything else (path separators, dots, `:`, `*`…) `_`; trimmed; at most 64
/// characters; Windows' reserved device names get a `_`. `None` when nothing usable is left.
pub fn file_stem(name: &str) -> Option<String> {
    let s: String =
        name.trim().chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '(' | ')') { c } else { '_' }).take(STEM_MAX).collect();
    let s = s.trim().to_string();
    if s.is_empty() || s.chars().all(|c| c == '_') {
        return None;
    }
    let upper = s.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT")) && upper.len() == 4 && upper.ends_with(|c: char| c.is_ascii_digit()));
    Some(if reserved { format!("{s}_") } else { s })
}

/// The stem of a user template id, when it is one this module could have made (so it can't
/// name a file outside the templates folder).
fn user_stem(id: &str) -> Option<&str> {
    let stem = id.strip_prefix(USER)?;
    (file_stem(stem).as_deref() == Some(stem)).then_some(stem)
}

/// The built-in templates: our own designs (no Lightroom template, name or design copied).
const BUILTINS: &[(&str, &str)] = &[
    ("plain-black", include_str!("slideshow_templates/plain-black.json")),
    ("gallery-wall", include_str!("slideshow_templates/gallery-wall.json")),
    ("edge-to-edge", include_str!("slideshow_templates/edge-to-edge.json")),
    ("cinema-wide", include_str!("slideshow_templates/cinema-wide.json")),
    ("shooting-data", include_str!("slideshow_templates/shooting-data.json")),
];

/// The built-in templates (one that doesn't parse — a bug the tests catch — is left out).
pub fn builtins() -> Vec<Template> {
    BUILTINS
        .iter()
        .filter_map(|(slug, json)| {
            let p = parse(json.as_bytes()).inspect_err(|e| eprintln!("lightcraft: built-in slideshow template {slug}: {e}")).ok()?;
            Some(Template {
                id: format!("{BUILTIN}{slug}"),
                name: p.name.unwrap_or_else(|| (*slug).to_string()),
                description: p.description,
                builtin: true,
                path: None,
                look: p.look,
            })
        })
        .collect()
}

/// Every template, and what couldn't be read.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    /// Built-ins first (in their order), then user templates by name.
    pub templates: Vec<Arc<Template>>,
    /// Files skipped, each with why.
    pub problems: Vec<String>,
}

impl Listing {
    pub fn get(&self, id: &str) -> Option<&Arc<Template>> {
        self.templates.iter().find(|t| t.id == id)
    }

    /// A template by name (case-insensitive); a user template before a built-in.
    pub fn by_name(&self, name: &str) -> Option<&Arc<Template>> {
        let n = name.trim().to_lowercase();
        let mut hits = self.templates.iter().filter(|t| t.name.to_lowercase() == n);
        let first = hits.next()?;
        Some(hits.find(|t| !t.builtin).unwrap_or(first))
    }
}

/// When the templates folder last changed (the listing is read again when this does).
type Stamp = (Option<PathBuf>, Option<std::time::SystemTime>);

/// The cached listing and the folder stamp it was read at.
pub type Cache = Option<(Stamp, Arc<Listing>)>;

fn stamp(dir: Option<&Path>) -> Stamp {
    (dir.map(Path::to_path_buf), dir.and_then(|d| std::fs::metadata(d).ok()).and_then(|m| m.modified().ok()))
}

/// Read the user templates in `dir`: `*.json` regular files, at most [`MAX_TEMPLATES`], each
/// parsed and checked; anything else reported.
fn read_dir(dir: &Path, out: &mut Listing) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            out.problems.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.take(MAX_SCAN).flatten() {
        let path = entry.path();
        let is_json = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if !is_json {
            continue;
        }
        let file = entry.file_name().to_string_lossy().to_string();
        files.push((file, path));
    }
    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    if files.len() > MAX_TEMPLATES {
        out.problems.push(format!("{}: {} template files; only the first {MAX_TEMPLATES} are listed", dir.display(), files.len()));
        files.truncate(MAX_TEMPLATES);
    }
    let mut user = Vec::new();
    for (file, path) in files {
        match read_user(&file, &path) {
            Ok(t) => user.push(Arc::new(t)),
            Err(e) => out.problems.push(format!("{file}: {e}")),
        }
    }
    user.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    out.templates.extend(user);
}

/// One user template file.
fn read_user(file: &str, path: &Path) -> Result<Template, String> {
    let stem = file.get(..file.len().saturating_sub(5)).unwrap_or_default();
    if file_stem(stem).as_deref() != Some(stem) {
        return Err("the file name isn't one LightCraft makes (letters, digits, spaces, - _ ( ) only); rename the file".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() {
        return Err("not a regular file (a link or a folder)".into());
    }
    if meta.len() > MAX_FILE {
        return Err(format!("{} bytes is too large for a slideshow template (at most {} KB)", meta.len(), MAX_FILE / 1024));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let p = parse(&bytes)?;
    Ok(Template {
        id: format!("{USER}{stem}"),
        name: p.name.unwrap_or_else(|| stem.to_string()),
        description: p.description,
        builtin: false,
        path: Some(path.to_path_buf()),
        look: p.look,
    })
}

/// Read a template file the user picked (import): size-capped, then [`parse`]d. Its name is the
/// document's, else the file's.
pub fn read_file(path: &Path) -> Result<Parsed, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    if meta.len() > MAX_FILE {
        return Err(format!("{}: {} bytes is too large for a slideshow template (at most {} KB)", path.display(), meta.len(), MAX_FILE / 1024));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut p = parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if p.name.is_none() {
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        p.name = valid_name(&clean_text(&stem, NAME_MAX)).ok();
    }
    Ok(p)
}

fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    lightcraft_catalog::safe_file::write_atomic(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))
}

impl Session {
    /// Keep user slideshow templates in `<config>/slideshow-templates` (hosts with a file system).
    pub fn with_default_slide_templates(mut self) -> Self {
        self.slide_templates_dir = crate::config::config_dir().map(|d| d.join(DIR_NAME));
        self
    }

    /// Every template (built-in, then the user's), read again when the templates folder changed.
    pub fn slide_templates(&mut self) -> Arc<Listing> {
        let now = stamp(self.slide_templates_dir.as_deref());
        if let Some((st, l)) = &self.slide_templates_cache
            && *st == now
        {
            return l.clone();
        }
        let mut l = Listing { templates: builtins().into_iter().map(Arc::new).collect(), problems: Vec::new() };
        if let Some(dir) = self.slide_templates_dir.clone() {
            read_dir(&dir, &mut l);
        }
        let l = Arc::new(l);
        self.slide_templates_cache = Some((now, l.clone()));
        l
    }

    /// Forget the cached listing (after a write: a folder's time may not change within its
    /// resolution).
    fn templates_changed(&mut self) {
        self.slide_templates_cache = None;
    }

    /// The template `id` (or [`PREVIOUS`], the settings before the last apply).
    pub fn slide_template(&mut self, id: &str) -> Result<Arc<Template>, String> {
        if id == PREVIOUS {
            let look = self.slide_templates_previous.clone().ok_or("no previous settings (no template has been applied this session)")?;
            return Ok(Arc::new(Template {
                id: PREVIOUS.into(),
                name: "Previous settings".into(),
                description: String::new(),
                builtin: false,
                path: None,
                look,
            }));
        }
        if id.starts_with(USER) && user_stem(id).is_none() {
            return Err(format!("`{id}` isn't a template id (user templates are `user:` and a file name LightCraft made)"));
        }
        self.slide_templates().get(id).cloned().ok_or_else(|| format!("no slideshow template `{id}`"))
    }

    /// The template a command names: `id`, or `name` (case-insensitive).
    pub fn slide_template_named(&mut self, id: Option<&str>, name: Option<&str>) -> Result<Arc<Template>, String> {
        match (id.map(str::trim).filter(|s| !s.is_empty()), name.map(str::trim).filter(|s| !s.is_empty())) {
            (Some(id), _) => self.slide_template(id),
            (None, Some(name)) => self.slide_templates().by_name(name).cloned().ok_or_else(|| format!("no slideshow template named \"{name}\"")),
            (None, None) => Err("name a template: `id` or `name`".into()),
        }
    }

    /// Replace the slideshow settings with `look` (prepare previews kept) and save them; the
    /// settings it replaced become [`PREVIOUS`]. Returns the warnings for this library.
    pub fn apply_slide_look(&mut self, look: &SlideLook) -> crate::Result<Vec<String>> {
        let before = SlideLook::of(self);
        let mut next = look.clone();
        next.playback.prepare_previews = self.slideshow.prepare_previews;
        let next = SlideLook {
            playback: next.playback.sanitized(),
            layout: next.layout.sanitized(),
            overlays: next.overlays.sanitized(),
            titles: next.titles.sanitized(),
        };
        let warnings = next.warnings(self);
        if next != before {
            (self.slideshow, self.slide_layout, self.slide_overlays, self.slide_titles) = (next.playback, next.layout, next.overlays, next.titles);
            if let Err(e) = self.save_prefs() {
                (self.slideshow, self.slide_layout, self.slide_overlays, self.slide_titles) =
                    (before.playback, before.layout, before.overlays, before.titles);
                return Err(e);
            }
            self.slide_templates_previous = Some(before);
        }
        Ok(warnings)
    }

    fn templates_dir(&self) -> Result<PathBuf, String> {
        self.slide_templates_dir.clone().ok_or_else(|| {
            "user templates need a settings folder, and this LightCraft has none (the web build, or LIGHTCRAFT_CONFIG_DIR is set empty)".into()
        })
    }

    /// Is a user template file called `stem` there, or one whose name differs only in case?
    fn stem_taken(&mut self, dir: &Path, stem: &str, except: Option<&str>) -> bool {
        let lower = stem.to_lowercase();
        let other = |s: &str| except.is_none_or(|e| e.to_lowercase() != s.to_lowercase());
        (dir.join(format!("{stem}.json")).exists() && other(stem))
            || self.slide_templates().templates.iter().filter_map(|t| t.id.strip_prefix(USER)).any(|s| s.to_lowercase() == lower && other(s))
    }

    fn builtin_name(name: &str) -> bool {
        builtins().iter().any(|b| b.name.to_lowercase() == name.to_lowercase())
    }

    /// Save the current settings as user template `name` (`replace`: over one of that name).
    pub fn save_slide_template(&mut self, name: &str, replace: bool) -> Result<Arc<Template>, String> {
        let name = valid_name(name)?;
        if Self::builtin_name(&name) {
            return Err(format!("\"{name}\" is the name of a built-in template; pick another name"));
        }
        let stem = file_stem(&name).ok_or_else(|| format!("\"{name}\" has no letters or digits to make a file name from"))?;
        let dir = self.templates_dir()?;
        if !replace && self.stem_taken(&dir, &stem, None) {
            return Err(format!("a template named \"{name}\" (file {stem}.json) already exists: update it, or save under another name"));
        }
        let look = SlideLook::of(self);
        write_json(&dir.join(format!("{stem}.json")), &doc(&name, "", &look))?;
        self.templates_changed();
        self.slide_template(&format!("{USER}{stem}"))
    }

    /// A user template (not a built-in).
    fn user_template(&mut self, id: &str) -> Result<(Arc<Template>, PathBuf), String> {
        if id.starts_with(BUILTIN) {
            return Err(format!("`{id}` is a built-in template and can't be changed (save the settings as a template of your own)"));
        }
        if id == PREVIOUS {
            return Err("the previous settings aren't a saved template".into());
        }
        let t = self.slide_template(id)?;
        let path = t.path.clone().ok_or_else(|| format!("`{id}` isn't a user template"))?;
        Ok((t, path))
    }

    /// Replace user template `id`'s settings with the current ones (its name kept).
    pub fn update_slide_template(&mut self, id: &str) -> Result<Arc<Template>, String> {
        let (t, path) = self.user_template(id)?;
        write_json(&path, &doc(&t.name, &t.description, &SlideLook::of(self)))?;
        self.templates_changed();
        self.slide_template(id)
    }

    /// Rename user template `id` (its file follows the name).
    pub fn rename_slide_template(&mut self, id: &str, name: &str) -> Result<Arc<Template>, String> {
        let (t, path) = self.user_template(id)?;
        let name = valid_name(name)?;
        if Self::builtin_name(&name) {
            return Err(format!("\"{name}\" is the name of a built-in template; pick another name"));
        }
        let stem = file_stem(&name).ok_or_else(|| format!("\"{name}\" has no letters or digits to make a file name from"))?;
        let dir = self.templates_dir()?;
        let old = id.strip_prefix(USER).unwrap_or_default().to_string();
        if self.stem_taken(&dir, &stem, Some(&old)) {
            return Err(format!("a template named \"{name}\" (file {stem}.json) already exists"));
        }
        // the new name into the file, then the file to its new name (one rename: never two copies
        // or none, and a change of case works on case-insensitive file systems too)
        write_json(&path, &doc(&name, &t.description, &t.look))?;
        let to = dir.join(format!("{stem}.json"));
        if to != path {
            std::fs::rename(&path, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
        self.templates_changed();
        self.slide_template(&format!("{USER}{stem}"))
    }

    /// Delete user template `id`'s file.
    pub fn delete_slide_template(&mut self, id: &str) -> Result<Arc<Template>, String> {
        let (t, path) = self.user_template(id)?;
        std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.templates_changed();
        Ok(t)
    }

    /// Import the template file at `path` as a user template. A name already taken gets " (2)",
    /// " (3)"…; returns the template and whether it was renamed.
    pub fn import_slide_template(&mut self, path: &Path) -> Result<(Arc<Template>, bool), String> {
        let p = read_file(path)?;
        let dir = self.templates_dir()?;
        let base = p.name.clone().ok_or_else(|| format!("{}: the template has no name, nor a file name to take one from", path.display()))?;
        let mut name = base.clone();
        let mut n = 1;
        loop {
            let free = !Self::builtin_name(&name) && file_stem(&name).is_some_and(|stem| !self.stem_taken(&dir, &stem, None));
            if free {
                break;
            }
            n += 1;
            if n > 999 {
                return Err(format!("too many templates named \"{base}\""));
            }
            let suffix = format!(" ({n})");
            let keep: String = base.chars().take(NAME_MAX.saturating_sub(suffix.chars().count())).collect();
            name = format!("{}{suffix}", keep.trim_end());
        }
        let stem = file_stem(&name).ok_or_else(|| format!("\"{name}\" has no letters or digits to make a file name from"))?;
        write_json(&dir.join(format!("{stem}.json")), &doc(&name, &p.description, &p.look))?;
        self.templates_changed();
        Ok((self.slide_template(&format!("{USER}{stem}"))?, name != base))
    }
}

#[cfg(test)]
mod tests;
