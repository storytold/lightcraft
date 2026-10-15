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
}
