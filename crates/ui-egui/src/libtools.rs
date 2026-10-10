//! Library tools that live outside a single panel: the Painter (spray keywords, labels, flags,
//! ratings, a metadata preset, develop settings, a rotation or the target collection onto photos
//! in the grid), the grid cell style (compact / expanded, index numbers, extra badges) and the
//! Metadata panel's field preset.
//!
//! Everything here is reached through UI commands (`tool.painter`, `view.gridCellStyle`,
//! `metadata.panelPreset`); painting itself dispatches the engine's ordinary commands
//! (`photo.setMeta`, `photo.label`, …), one undo step per photo.

use std::collections::HashSet;

use dac_catalog::{ColorLabel, Flag, Photo, PhotoId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::DacApp;

/// What the painter sprays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PaintKind {
    /// `value`: comma-separated keywords (`a|b` for hierarchy).
    Keywords,
    /// `value`: red | yellow | green | blue | purple | none.
    Label,
    /// `value`: pick | reject | none.
    Flag,
    /// `value`: 0..5.
    Rating,
    /// `value`: a metadata preset's name.
    MetadataPreset,
    /// `value`: a develop preset's id.
    Settings,
    /// `value`: left | right | flipHorizontal | flipVertical.
    Rotation,
    /// Adds to (or erases from) the target collection; `value` unused.
    TargetCollection,
}

impl PaintKind {
    pub const ALL: [PaintKind; 8] = [
        PaintKind::Keywords,
        PaintKind::Label,
        PaintKind::Flag,
        PaintKind::Rating,
        PaintKind::MetadataPreset,
        PaintKind::Settings,
        PaintKind::Rotation,
        PaintKind::TargetCollection,
    ];

    pub fn id(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }

    pub fn parse(s: &str) -> Option<PaintKind> {
        serde_json::from_value(json!(s)).ok()
    }

    pub fn label(self) -> &'static str {
        match self {
            PaintKind::Keywords => "Keywords",
            PaintKind::Label => "Label",
            PaintKind::Flag => "Flag",
            PaintKind::Rating => "Rating",
            PaintKind::MetadataPreset => "Metadata",
            PaintKind::Settings => "Settings",
            PaintKind::Rotation => "Rotation",
            PaintKind::TargetCollection => "Target Collection",
        }
    }

    /// The value a freshly chosen kind starts with.
    pub fn default_value(self) -> &'static str {
        match self {
            PaintKind::Label => "red",
            PaintKind::Flag => "pick",
            PaintKind::Rating => "3",
            PaintKind::Rotation => "right",
            _ => "",
        }
    }
}

/// The painter: what it sprays and with which value.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Painter {
    pub kind: PaintKind,
    pub value: String,
}

impl Painter {
    /// Checks the value for its kind (`None` = nothing to paint, with why).
    pub fn new(kind: PaintKind, value: &str) -> Result<Painter, String> {
        let value = value.trim();
        let ok = match kind {
            PaintKind::Keywords => !keywords(value).is_empty(),
            PaintKind::Label => value == "none" || ColorLabel::parse(value).is_some(),
            PaintKind::Flag => Flag::parse(value).is_some(),
            PaintKind::Rating => value.parse::<u8>().is_ok_and(|r| r <= 5),
            PaintKind::MetadataPreset | PaintKind::Settings => !value.is_empty(),
            PaintKind::Rotation => matches!(value, "left" | "right" | "flipHorizontal" | "flipVertical"),
            PaintKind::TargetCollection => true,
        };
        if !ok {
            return Err(format!("tool.painter: `{value}` is not a value for {}", kind.id()));
        }
        Ok(Painter { kind, value: value.to_string() })
    }

    /// Does the photo already carry what this painter sprays? (Then a stroke started on it erases.)
    pub fn has(&self, p: &Photo, in_target: bool) -> bool {
        match self.kind {
            PaintKind::Keywords => keywords(&self.value).iter().all(|k| p.meta.keywords.iter().any(|x| x.eq_ignore_ascii_case(k))),
            PaintKind::Label => p.label == ColorLabel::parse(&self.value),
            PaintKind::Flag => Flag::parse(&self.value) == Some(p.flag),
            PaintKind::Rating => self.value.parse::<u8>().ok() == Some(p.rating),
            PaintKind::TargetCollection => in_target,
            // applying again is harmless (presets) or meant (rotation): never erase
            PaintKind::MetadataPreset | PaintKind::Settings | PaintKind::Rotation => false,
        }
    }

    /// The command that paints (or with `erase`, takes away) this painter's value on one photo;
    /// `None` when there is nothing to do.
    pub fn command(&self, p: &Photo, erase: bool, in_target: bool) -> Option<(&'static str, Value)> {
        let ids = json!([p.id.0]);
        let has = self.has(p, in_target);
        if erase && !has && self.kind != PaintKind::Keywords {
            return None;
        }
        Some(match self.kind {
            PaintKind::Keywords => {
                let kws = keywords(&self.value);
                let missing: Vec<&String> = kws.iter().filter(|k| !p.meta.keywords.iter().any(|x| x.eq_ignore_ascii_case(k))).collect();
                if erase {
                    if missing.len() == kws.len() {
                        return None;
                    }
                    ("photo.setMeta", json!({"ids": ids, "removeKeywords": kws}))
                } else {
                    if missing.is_empty() {
                        return None;
                    }
                    ("photo.setMeta", json!({"ids": ids, "addKeywords": missing}))
                }
            }
            _ if has && !erase && self.kind != PaintKind::Rotation => return None,
            PaintKind::Label => ("photo.label", json!({"ids": ids, "label": if erase { "none" } else { &self.value }})),
            PaintKind::Flag => ("photo.flag", json!({"ids": ids, "flag": if erase { "none" } else { &self.value }})),
            PaintKind::Rating => ("photo.rate", json!({"ids": ids, "rating": if erase { 0 } else { self.value.parse::<u8>().unwrap_or(0) }})),
            PaintKind::MetadataPreset => ("metadata.applyPreset", json!({"ids": ids, "name": self.value})),
            PaintKind::Settings => ("preset.apply", json!({"ids": ids, "id": self.value})),
            PaintKind::Rotation => (
                match self.value.as_str() {
                    "left" => "photo.rotateLeft",
                    "flipHorizontal" => "photo.flipHorizontal",
                    "flipVertical" => "photo.flipVertical",
                    _ => "photo.rotateRight",
                },
                json!({"ids": ids}),
            ),
            // toggles: adds when out, removes when in (checked above)
            PaintKind::TargetCollection => ("album.toggleTarget", json!({"ids": ids})),
        })
    }
}

fn keywords(value: &str) -> Vec<String> {
    value.split(',').map(str::trim).filter(|k| !k.is_empty()).map(str::to_string).collect()
}

/// Grid cell style (the Square Grid's cells; the Photo Grid always shows photos edge to edge).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CellStyle {
    /// One caption line.
    #[default]
    Compact,
    /// Two header lines (index · name · size, then the exposure) and a footer with rating, flag and label.
    Expanded,
}

/// Library tool state kept in [`crate::state::UiState`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LibTools {
    /// The Metadata panel's field preset ([`crate::panels::metadata::PRESETS`]).
    pub metadata_preset: String,
    pub cell_style: CellStyle,
    /// Index numbers (the photo's position in the view) in the cells.
    pub cell_index: bool,
    /// Thumbnail badges: keywords, collections, metadata conflict, remote (Immich) link.
    pub cell_badges: bool,
    /// The running painter.
    #[serde(skip)]
    pub painter: Option<Painter>,
    /// The kind and value the painter bar shows (kept while the painter is off).
    #[serde(skip)]
    pub last_painter: Option<Painter>,
    /// The stroke in progress: erasing?, photos already painted.
    #[serde(skip)]
    pub stroke: Option<(bool, HashSet<PhotoId>)>,
}

impl Default for LibTools {
    fn default() -> Self {
        LibTools {
            metadata_preset: "default".into(),
            cell_style: CellStyle::Compact,
            cell_index: false,
            cell_badges: true,
            painter: None,
            last_painter: None,
            stroke: None,
        }
    }
}

/// UI commands of this module; `None`: not one of them.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "tool.painter" => painter_command(app, p),
        "tool.keywordPainter" => {
            // the older single-keyword painter: `{keyword?}`
            let k = p.get("keyword").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty());
            painter_command(app, &json!({"kind": "keywords", "value": k, "stop": k.is_none()}))
        }
        "view.gridCellStyle" => {
            let t = &mut app.ui.lib;
            match p.get("style").and_then(Value::as_str) {
                Some("compact") => t.cell_style = CellStyle::Compact,
                Some("expanded") => t.cell_style = CellStyle::Expanded,
                Some(other) => return Some(Err(format!("view.gridCellStyle: unknown style `{other}` (compact|expanded)"))),
                None if p.get("index").is_none() && p.get("badges").is_none() => {
                    t.cell_style = if t.cell_style == CellStyle::Compact { CellStyle::Expanded } else { CellStyle::Compact };
                }
                None => {}
            }
            if let Some(b) = p.get("index").and_then(Value::as_bool) {
                t.cell_index = b;
            }
            if let Some(b) = p.get("badges").and_then(Value::as_bool) {
                t.cell_badges = b;
            }
            Ok(json!({"style": t.cell_style, "index": t.cell_index, "badges": t.cell_badges}))
        }
        "metadata.panelPreset" => {
            let Some(name) = p.get("preset").and_then(Value::as_str) else {
                return Some(Ok(json!({"preset": app.ui.lib.metadata_preset, "presets": crate::panels::metadata::preset_ids()})));
            };
            if !crate::panels::metadata::PRESETS.iter().any(|(k, _)| *k == name) {
                return Some(Err(format!("metadata.panelPreset: unknown preset `{name}` ({})", crate::panels::metadata::preset_ids().join(", "))));
            }
            app.ui.lib.metadata_preset = name.to_string();
            Ok(json!({"preset": name}))
        }
        _ => return None,
    })
}

/// `tool.painter {kind?, value?, stop?}`: start (or retarget) the painter; `{}` toggles it with the
/// last kind and value; `stop: true` stops.
fn painter_command(app: &mut DacApp, p: &Value) -> Result<Value, String> {
    let stop = p.get("stop").and_then(Value::as_bool).unwrap_or(false);
    let kind = p.get("kind").and_then(Value::as_str);
    if stop || (kind.is_none() && app.ui.lib.painter.is_some()) {
        app.ui.lib.painter = None;
        app.ui.lib.stroke = None;
        return Ok(json!({"painter": Value::Null}));
    }
    let painter = match kind {
        Some(k) => {
            let kind = PaintKind::parse(k).ok_or_else(|| {
                format!("tool.painter: unknown kind `{k}` ({})", PaintKind::ALL.iter().map(|k| k.id()).collect::<Vec<_>>().join(", "))
            })?;
            let value = p.get("value").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).filter(|v| v != "null");
            Painter::new(kind, value.as_deref().unwrap_or(kind.default_value()))?
        }
        None => app.ui.lib.last_painter.clone().ok_or("tool.painter: choose what to paint (kind, value)")?,
    };
    match painter.kind {
        PaintKind::MetadataPreset if !app.session.metadata_presets.iter().any(|m| m.name.eq_ignore_ascii_case(&painter.value)) => {
            return Err(format!("tool.painter: no metadata preset `{}`", painter.value));
        }
        PaintKind::Settings if !app.session.presets.iter().any(|m| m.id == painter.value) => {
            return Err(format!("tool.painter: no develop preset `{}`", painter.value));
        }
        _ => {}
    }
    if !matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid) {
        app.ui.view = crate::state::ViewMode::PhotoGrid;
    }
    app.ui.lib.last_painter = Some(painter.clone());
    app.ui.lib.painter = Some(painter.clone());
    app.ui.lib.stroke = None;
    Ok(json!({"painter": painter, "keyword": (painter.kind == PaintKind::Keywords).then_some(&painter.value)}))
}

/// Is the photo in the target collection (the Quick Collection unless one is set)?
pub fn in_target(app: &DacApp, id: PhotoId) -> bool {
    let cat = &app.session.catalog;
    let target = app.session.target_album.filter(|a| cat.album(*a).is_some_and(|al| !al.is_smart() && !al.folder)).or_else(|| cat.quick_collection());
    target.and_then(|a| cat.album(a)).is_some_and(|al| al.photos.contains(&id))
}

/// Once per grid frame: a finished stroke ends (kept on the release frame so the click that ends
/// it doesn't paint twice).
pub fn end_stroke(app: &mut DacApp, ui: &egui::Ui) {
    if !ui.input(|i| i.pointer.primary_down() || i.pointer.primary_released()) {
        app.ui.lib.stroke = None;
    }
}

/// While the painter runs, a grid cell paints instead of selecting: pressing on a photo starts a
/// stroke (erasing when the photo already has the value, or with Alt), dragging over more photos
/// sprays them. Returns `true` when the painter handled the cell.
pub fn paint_cell(app: &mut DacApp, ui: &egui::Ui, resp: &egui::Response, id: PhotoId) -> bool {
    let Some(painter) = app.ui.lib.painter.clone() else { return false };
    if resp.contains_pointer() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let down = resp.contains_pointer() && ui.input(|i| i.pointer.primary_down());
    let clicked = resp.clicked();
    if !down && !clicked {
        return true;
    }
    if app.ui.lib.stroke.as_ref().is_some_and(|(_, seen)| seen.contains(&id)) {
        return true;
    }
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return true };
    let target = in_target(app, id);
    let alt = ui.input(|i| i.modifiers.alt);
    let erase = match &app.ui.lib.stroke {
        Some((erase, _)) => *erase,
        None => alt || painter.has(&photo, target),
    };
    let stroke = app.ui.lib.stroke.get_or_insert_with(|| (erase, HashSet::new()));
    stroke.1.insert(id);
    if let Some((cmd, params)) = painter.command(&photo, erase, target)
        && let Err(e) = app.run(cmd, params)
    {
        app.toast(ui.ctx(), e);
    }
    true
}

/// The painter bar over the grid while painting: what to spray, its value, Done.
pub fn painter_bar(app: &mut DacApp, ui: &mut egui::Ui) {
    let Some(painter) = app.ui.lib.painter.clone() else { return };
    let t = crate::theme::Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(16, 6));
    let r = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            crate::icons::paint(
                ui.painter(),
                egui::Rect::from_min_size(ui.cursor().min + egui::vec2(0.0, 3.0), egui::vec2(16.0, 16.0)),
                crate::icons::Icon::Brush,
                t.accent,
            );
            ui.add_space(20.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Paint:")).color(t.text_label));
            let mut next: Option<(PaintKind, String)> = None;
            let combo = egui::ComboBox::from_id_salt("painter-kind").selected_text(crate::i18n::tr(painter.kind.label())).show_ui(ui, |ui| {
                for k in PaintKind::ALL {
                    if ui.selectable_label(k == painter.kind, crate::i18n::tr(k.label())).clicked() && k != painter.kind {
                        let v = match k {
                            PaintKind::MetadataPreset => app.session.metadata_presets.first().map(|m| m.name.clone()).unwrap_or_default(),
                            PaintKind::Settings => app.session.presets.first().map(|m| m.id.clone()).unwrap_or_default(),
                            _ => k.default_value().to_string(),
                        };
                        next = Some((k, v));
                    }
                }
            });
            crate::widgets::register(ui.ctx(), "painter:kind", combo.response.rect);
            value_editor(app, ui, &painter, &mut next);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::text_button(ui, "painterDone", crate::i18n::tr("Done"), false).clicked() {
                    let _ = app.run("tool.painter", json!({"stop": true}));
                }
                ui.label(egui::RichText::new(crate::i18n::tr("Click or drag over photos · Alt erases · Esc stops")).size(11.5).color(t.text_dim));
            });
            if let Some((k, v)) = next
                && let Err(e) = app.run("tool.painter", json!({"kind": k.id(), "value": v}))
            {
                app.toast(ui.ctx(), e);
            }
        });
    });
    crate::widgets::register(ui.ctx(), "painter:bar", r.response.rect);
}

/// The value control for the painter's kind; a change goes to `next`.
fn value_editor(app: &mut DacApp, ui: &mut egui::Ui, painter: &Painter, next: &mut Option<(PaintKind, String)>) {
    let kind = painter.kind;
    let mut choose = |ui: &mut egui::Ui, items: Vec<(String, String)>| {
        let current = items.iter().find(|(v, _)| *v == painter.value).map(|(_, l)| l.clone()).unwrap_or_else(|| painter.value.clone());
        let r = egui::ComboBox::from_id_salt("painter-value").selected_text(current).show_ui(ui, |ui| {
            for (v, l) in items {
                if ui.selectable_label(v == painter.value, l).clicked() {
                    *next = Some((kind, v));
                }
            }
        });
        crate::widgets::register(ui.ctx(), "painter:value", r.response.rect);
    };
    match kind {
        PaintKind::Keywords => {
            let id = egui::Id::new("painter-keywords");
            let mut text: String = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| painter.value.clone());
            let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text(crate::i18n::tr("keyword, keyword")).desired_width(200.0));
            crate::widgets::register(ui.ctx(), "painter:value", r.rect);
            if r.has_focus() {
                ui.data_mut(|d| d.insert_temp(id, text.clone()));
            } else {
                ui.data_mut(|d| d.remove::<String>(id));
            }
            if r.lost_focus() && text.trim() != painter.value && !text.trim().is_empty() {
                *next = Some((kind, text.trim().to_string()));
            }
        }
        PaintKind::Label => {
            let mut items: Vec<(String, String)> = ColorLabel::ALL
                .iter()
                .map(|l| (format!("{l:?}").to_lowercase(), crate::i18n::color_label(&app.session.catalog, *l).to_string()))
                .collect();
            items.push(("none".into(), crate::i18n::tr("None").into()));
            choose(ui, items);
        }
        PaintKind::Flag => choose(
            ui,
            [("pick", "Flagged"), ("reject", "Rejected"), ("none", "Unflagged")]
                .iter()
                .map(|(v, l)| (v.to_string(), crate::i18n::tr(l).to_string()))
                .collect(),
        ),
        PaintKind::Rating => choose(
            ui,
            (0..=5u8).map(|r| (r.to_string(), if r == 0 { crate::i18n::tr("No stars").to_string() } else { "★".repeat(r as usize) })).collect(),
        ),
        PaintKind::MetadataPreset => {
            let items: Vec<(String, String)> = app.session.metadata_presets.iter().map(|m| (m.name.clone(), m.name.clone())).collect();
            if items.is_empty() {
                ui.label(crate::i18n::tr("No metadata presets yet (Metadata panel ▸ Preset ▸ Save)"));
            } else {
                choose(ui, items);
            }
        }
        PaintKind::Settings => choose(ui, app.session.presets.iter().map(|m| (m.id.clone(), m.name.clone())).collect()),
        PaintKind::Rotation => choose(
            ui,
            [("left", "Rotate Left"), ("right", "Rotate Right"), ("flipHorizontal", "Flip Horizontal"), ("flipVertical", "Flip Vertical")]
                .iter()
                .map(|(v, l)| (v.to_string(), crate::i18n::tr(l).to_string()))
                .collect(),
        ),
        PaintKind::TargetCollection => {
            let cat = &app.session.catalog;
            let name = app
                .session
                .target_album
                .and_then(|a| cat.album(a))
                .filter(|al| !al.is_smart() && !al.folder)
                .map(|al| al.name.clone())
                .unwrap_or_else(|| crate::i18n::tr("Quick Collection").to_string());
            ui.label(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dac_catalog::Source;

    fn photo() -> Photo {
        Photo::new(PhotoId(7), Source::Demo { scene: 0 }, "a.jpg", "JPEG", 10, 10, "2026-01-01T00:00:00")
    }

    #[test]
    fn painter_values_are_checked() {
        assert!(Painter::new(PaintKind::Rating, "6").is_err());
        assert!(Painter::new(PaintKind::Rating, "x").is_err());
        assert!(Painter::new(PaintKind::Label, "pink").is_err());
        assert!(Painter::new(PaintKind::Keywords, " , ").is_err());
        assert!(Painter::new(PaintKind::Rotation, "up").is_err());
        assert!(Painter::new(PaintKind::Flag, "pick").is_ok());
        assert_eq!(PaintKind::parse("targetCollection"), Some(PaintKind::TargetCollection));
        assert_eq!(PaintKind::parse("nope"), None);
    }

    #[test]
    fn painter_commands_paint_and_erase() {
        let mut p = photo();
        let kw = Painter::new(PaintKind::Keywords, "sea, sky").unwrap_or_else(|e| panic!("{e}"));
        assert!(!kw.has(&p, false));
        let (c, v) = kw.command(&p, false, false).unwrap();
        assert_eq!((c, v["addKeywords"].clone()), ("photo.setMeta", json!(["sea", "sky"])));
        assert!(kw.command(&p, true, false).is_none(), "nothing to erase");
        p.meta.keywords = vec!["Sea".into(), "sky".into()];
        assert!(kw.has(&p, false));
        assert!(kw.command(&p, false, false).is_none(), "already there");
        assert_eq!(kw.command(&p, true, false).unwrap().1["removeKeywords"], json!(["sea", "sky"]));

        let r = Painter::new(PaintKind::Rating, "4").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.command(&p, false, false).unwrap().1["rating"], 4);
        assert!(r.command(&p, true, false).is_none());
        p.rating = 4;
        assert_eq!(r.command(&p, true, false).unwrap().1["rating"], 0);

        let l = Painter::new(PaintKind::Label, "green").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(l.command(&p, false, false).unwrap(), ("photo.label", json!({"ids": [7], "label": "green"})));

        let tc = Painter::new(PaintKind::TargetCollection, "").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tc.command(&p, false, false).unwrap().0, "album.toggleTarget");
        assert!(tc.command(&p, false, true).is_none(), "already in the target");
        assert!(tc.command(&p, true, false).is_none(), "nothing to take out");

        let rot = Painter::new(PaintKind::Rotation, "left").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rot.command(&p, false, false).unwrap().0, "photo.rotateLeft");
        assert_eq!(rot.command(&p, false, false).unwrap().0, "photo.rotateLeft", "rotation always applies");
    }
}
