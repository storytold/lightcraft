//! The settings a print is made from, and (with the saved-template commands) what a print
//! template stores.
use super::PrintJob;
use super::compose::{ImageSettings, PageStyle};
use super::layout::{Guides, Layout};
use serde::{Deserialize, Serialize};

/// Layout, image settings, page, guides and job: the whole Print module state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PrintSettings {
    pub layout: Layout,
    pub image: ImageSettings,
    pub page: PageStyle,
    pub guides: Guides,
    pub job: PrintJob,
}

impl PrintSettings {
    /// Checks every part; run on anything that did not come from the setters.
    pub fn validate(&mut self) -> Result<(), String> {
        self.layout.validate()?;
        self.image.validate()?;
        self.guides.validate()?;
        self.job.validate()
    }
}

/// Longest template name, characters.
pub const MAX_NAME: usize = 80;
/// Most templates kept.
pub const MAX_TEMPLATES: usize = 200;
/// File extension of an exported print template (JSON).
pub const FILE_EXT: &str = "lcprint";
/// `format` tag of an exported print template file.
const FILE_FORMAT: &str = "lightcraft.printTemplate";
/// Version of the template file this build writes and the newest it reads.
const FILE_VERSION: u64 = 1;
/// Largest template file read, bytes.
pub const MAX_FILE: u64 = 256 * 1024;

/// A saved print template: the layout, image settings, page and job of a print, without the
/// photos assigned to cells and without the guides (a view preference). The settings are kept as
/// JSON so a template written by another version never makes the preferences file unreadable;
/// [`PrintTemplate::settings`] checks them when they are used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrintTemplate {
    pub name: String,
    pub settings: serde_json::Value,
}

/// A template name: trimmed, 1 to [`MAX_NAME`] characters, no control characters.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(format!("a template name is 1 to {MAX_NAME} characters without control characters"));
    }
    Ok(name.to_string())
}

impl PrintTemplate {
    /// A template of `settings` (validated first) named `name`.
    pub fn new(name: &str, settings: &PrintSettings) -> Result<Self, String> {
        let mut s = settings.clone();
        s.validate()?;
        for c in &mut s.layout.cells {
            c.photo = None;
        }
        let mut value = serde_json::to_value(&s).map_err(|e| e.to_string())?;
        if let Some(o) = value.as_object_mut() {
            o.remove("guides");
        }
        Ok(Self { name: clean_name(name)?, settings: value })
    }

    /// The settings, checked: unknown fields, hostile numbers and inconsistent layouts are errors.
    /// `guides` are those of the print the template is applied to.
    pub fn settings(&self, guides: &Guides) -> Result<PrintSettings, String> {
        let mut s: PrintSettings = serde_json::from_value(self.settings.clone()).map_err(|e| format!("template `{}`: {e}", self.name))?;
        s.guides = *guides;
        s.validate().map_err(|e| format!("template `{}`: {e}", self.name))?;
        Ok(s)
    }

    /// The document of an exported template: `{format, version, name, settings}`.
    pub fn to_file(&self) -> serde_json::Value {
        serde_json::json!({"format": FILE_FORMAT, "version": FILE_VERSION, "name": self.name, "settings": self.settings})
    }

    /// A template read from an exported file. Refused: more than [`MAX_FILE`] bytes, not JSON,
    /// another format or a newer version, a bad name, and settings that fail the checks a saved
    /// template's settings get. Photos assigned to cells and guides in the file are dropped.
    pub fn from_file(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_FILE {
            return Err(format!("{} bytes is too large for a print template (at most {} KB)", bytes.len(), MAX_FILE / 1024));
        }
        let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("not a print template file: {e}"))?;
        if v.get("format").and_then(serde_json::Value::as_str) != Some(FILE_FORMAT) {
            return Err(format!("not a print template file (`format` is not \"{FILE_FORMAT}\")"));
        }
        match v.get("version").and_then(serde_json::Value::as_u64) {
            Some(1..=FILE_VERSION) => {}
            Some(n) => return Err(format!("print template file version {n} is not one this version of LightCraft reads (1 to {FILE_VERSION})")),
            None => return Err("a print template file needs a `version` number".into()),
        }
        let name = clean_name(v.get("name").and_then(serde_json::Value::as_str).ok_or("a print template file needs a `name`")?)?;
        let settings = v.get("settings").cloned().ok_or("a print template file needs `settings`")?;
        let checked = PrintTemplate { name: name.clone(), settings }.settings(&Guides::default())?;
        Self::new(&name, &checked)
    }
}

/// `name` if no template in `taken` has it (in any case), else the first free `name (2)`,
/// `name (3)`…, with `name` shortened to keep the result within [`MAX_NAME`] characters.
pub fn unique_name(name: &str, taken: &[PrintTemplate]) -> String {
    let free = |n: &str| !taken.iter().any(|t| t.name.eq_ignore_ascii_case(n));
    if free(name) {
        return name.to_string();
    }
    let mut n = 2usize;
    loop {
        let suffix = format!(" ({n})");
        let keep = MAX_NAME.saturating_sub(suffix.chars().count());
        let base: String = name.chars().take(keep).collect();
        let candidate = format!("{}{suffix}", base.trim_end());
        if free(&candidate) || n > taken.len() {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::layout::Style;
    use serde_json::json;

    #[test]
    fn a_template_keeps_the_look_not_the_photos_or_guides() {
        let mut s = PrintSettings::default();
        s.layout.style = Style::CustomPackage;
        let id = s.layout.add_cell(3.0, 2.0).unwrap();
        s.layout.set_cell_photo(id, Some(77)).unwrap();
        s.image.zoom_to_fill = true;
        s.page.background = [10, 20, 30];
        s.job.ppi = 300.0;
        s.guides.grid_size = 1.0;
        let t = PrintTemplate::new("  Proof sheet ", &s).unwrap();
        assert_eq!(t.name, "Proof sheet");
        assert!(t.settings.get("guides").is_none());
        let back = t.settings(&Guides::default()).unwrap();
        assert_eq!(back.layout.cells.len(), 1);
        assert_eq!(back.layout.cells[0].photo, None);
        assert!(back.image.zoom_to_fill);
        assert_eq!((back.page.background, back.job.ppi), ([10, 20, 30], 300.0));
        assert_eq!(back.guides, Guides::default(), "the guides are the caller's");
    }

    #[test]
    fn hostile_templates_are_rejected() {
        for name in ["", "   ", "a\nb", &"x".repeat(MAX_NAME + 1)] {
            assert!(PrintTemplate::new(name, &PrintSettings::default()).is_err(), "{name:?}");
        }
        let bad = |v: serde_json::Value| PrintTemplate { name: "t".into(), settings: v }.settings(&Guides::default()).is_err();
        assert!(bad(json!("text")));
        assert!(bad(json!({"surprise": 1})));
        assert!(bad(json!({"job": {"ppi": 1e30}})));
        assert!(bad(json!({"layout": {"pages": 0}})));
        assert!(bad(json!({"layout": {"page": {"paper": "custom", "customWidth": -3}}})));
        assert!(bad(json!({"layout": {"cells": [{"id": 1, "rect": {"x": 0, "y": 0, "w": 1e9, "h": 1}}]}})));
        assert!(bad(json!({"image": {"borderWidth": -1}})));
        assert!(!bad(json!({})));
        let mut s = PrintSettings::default();
        s.job.ppi = f32::NAN;
        assert!(PrintTemplate::new("t", &s).is_err());
    }

    #[test]
    fn a_template_file_reads_back_as_the_template() {
        let mut s = PrintSettings::default();
        s.layout.style = Style::ContactSheet;
        s.page.background = [1, 2, 3];
        let t = PrintTemplate::new("Proofs", &s).unwrap();
        let bytes = serde_json::to_vec(&t.to_file()).unwrap();
        assert_eq!(PrintTemplate::from_file(&bytes).unwrap(), t);
        // a hand-written file: photos and guides are not taken in
        let doc = json!({"format": FILE_FORMAT, "version": 1, "name": " Hand ", "settings": {
            "layout": {"style": "customPackage", "cells": [{"id": 1, "rect": {"x": 1, "y": 1, "w": 2, "h": 2}, "photo": 9}]},
            "guides": {"gridSize": 1.0}}});
        let t = PrintTemplate::from_file(&serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!(t.name, "Hand");
        assert!(t.settings.get("guides").is_none());
        assert!(t.settings.pointer("/layout/cells/0/rect").is_some());
        assert_eq!(t.settings.pointer("/layout/cells/0/photo"), None);
    }

    #[test]
    fn hostile_template_files_are_refused() {
        let file = |v: serde_json::Value| PrintTemplate::from_file(&serde_json::to_vec(&v).unwrap()).unwrap_err();
        let ok = |extra: serde_json::Value| {
            let mut v = json!({"format": FILE_FORMAT, "version": 1, "name": "t", "settings": {}});
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            v
        };
        assert!(PrintTemplate::from_file(b"not json").unwrap_err().contains("not a print template file"));
        assert!(PrintTemplate::from_file(&vec![b' '; MAX_FILE as usize + 1]).unwrap_err().contains("too large"));
        assert!(file(json!([1, 2])).contains("format"));
        assert!(file(ok(json!({"format": "lightcraft.slideshowTemplate"}))).contains("format"));
        assert!(file(ok(json!({"version": 2}))).contains("version 2"));
        assert!(file(ok(json!({"version": 0}))).contains("version 0"));
        assert!(file(ok(json!({"version": "1"}))).contains("`version` number"));
        assert!(file(json!({"format": FILE_FORMAT, "name": "t", "settings": {}})).contains("`version` number"));
        assert!(file(ok(json!({"name": "a\nb"}))).contains("template name"));
        assert!(file(ok(json!({"name": 5}))).contains("`name`"));
        assert!(file(json!({"format": FILE_FORMAT, "version": 1, "name": "t"})).contains("`settings`"));
        assert!(file(ok(json!({"settings": {"job": {"ppi": 1e30}}}))).contains("template `t`"));
        assert!(file(ok(json!({"settings": {"surprise": 1}}))).contains("template `t`"));
        assert!(PrintTemplate::from_file(&serde_json::to_vec(&ok(json!({}))).unwrap()).is_ok());
    }

    #[test]
    fn an_imported_name_never_takes_an_existing_one() {
        let t = |n: &str| PrintTemplate::new(n, &PrintSettings::default()).unwrap();
        assert_eq!(unique_name("Proofs", &[]), "Proofs");
        assert_eq!(unique_name("Proofs", &[t("proofs")]), "Proofs (2)");
        assert_eq!(unique_name("Proofs", &[t("Proofs"), t("PROOFS (2)")]), "Proofs (3)");
        let long = "x".repeat(MAX_NAME);
        let named = unique_name(&long, &[t(&long)]);
        assert_eq!(named.chars().count(), MAX_NAME);
        assert!(named.ends_with(" (2)"));
        let many: Vec<PrintTemplate> = std::iter::once(t("A")).chain((2..=MAX_TEMPLATES).map(|n| t(&format!("A ({n})")))).collect();
        let named = unique_name("A", &many);
        assert!(!many.iter().any(|x| x.name.eq_ignore_ascii_case(&named)), "{named}");
    }
}
