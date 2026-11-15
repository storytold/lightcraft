//! Submenus whose rows come from the current state, with a check mark on the choice in force
//! (Classic's Filter by Flag, Keyword Set, Map Style, Content, Targeted Adjustment…).
//!
//! To add one:
//! 1. put `@Label` (or `@Label=Name`) in a slot table of [`crate::lrc_menus`]; `Name` is the string
//!    [`crate::menubar::expanded`] is asked for;
//! 2. add a `"Name" => …` arm to [`expanded`] below that builds its rows with [`choices`]: each row is a
//!    [`Choice`] (label, the parameters its command runs with, whether it is the one in force). Extra
//!    rows (a separator, "Edit…") are pushed after;
//! 3. if the labels are the user's own words (preset names, keywords), list the command in
//!    `menubar::display_item_label` so they are not looked up as messages.
//!
//! A row is an ordinary `command{params}` item, so the menu, the control channel and MCP all run it the
//! same way, and a test needs only the node's id and parameters.

use serde_json::{Value, json};

use crate::LccApp;
use crate::menubar::{MenuNode, item};

/// One row of a state-driven submenu.
pub(crate) struct Choice {
    pub label: String,
    pub params: Value,
    /// `Some(true)` for the choice in force (it gets the check mark); `None` for a row that starts
    /// something instead of being a state.
    pub checked: Option<bool>,
    pub enabled: bool,
    pub shortcut: Option<String>,
}

impl Choice {
    pub fn new(label: impl Into<String>, params: Value, checked: bool) -> Self {
        Self { label: label.into(), params, checked: Some(checked), enabled: true, shortcut: None }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// An action row: no check slot.
    pub fn plain(mut self) -> Self {
        self.checked = None;
        self
    }

    pub fn key(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }
}

/// The rows as menu items that all run command `id`.
pub(crate) fn choices(id: &str, rows: impl IntoIterator<Item = Choice>) -> Vec<MenuNode> {
    rows.into_iter().map(|c| item(id, c.params, c.label, c.shortcut.as_deref(), c.enabled, c.checked)).collect()
}

/// The rows of the submenu `name`, if it is one of these.
pub(crate) fn expanded(app: &LccApp, name: &str) -> Option<Vec<MenuNode>> {
    Some(match name {
        "Filter by Flag" => filter_by_flag(app),
        "Filter by Color Label" => filter_by_label(app),
        "Filter by Preset" => filter_by_preset(app),
        "Filter by Kind" => filter_by_kind(app),
        "Filter by Metadata" => filter_by_metadata(app),
        "Set Keyword" => set_keyword(app),
        "Keyword Set" => keyword_set(app),
        "Location Filter" => location_filter(app),
        "Map Style" => map_style(app),
        "Slideshow Content" => slideshow_content(app),
        "Target Adjustment" => target_adjustment(app),
        "Select Landscape" => select_landscape(app),
        "Add to Mask with" => mask_with(app, "add"),
        "Subtract from Mask with" => mask_with(app, "subtract"),
        "Intersect Mask with" => mask_with(app, "intersect"),
        _ => return None,
    })
}

/// The filter bar's flag buttons: one flag at a time, the same one again clears it.
fn filter_by_flag(app: &LccApp) -> Vec<MenuNode> {
    use lcc_catalog::Flag;
    let f = &app.session.filter;
    let shown = f.flags_shown();
    let only = |flag: Flag| shown.as_deref() == Some(&[flag][..]);
    let mut v = choices(
        "library.filter",
        [("pick", "Flagged", Flag::Pick), ("none", "Unflagged", Flag::None), ("reject", "Rejected", Flag::Reject)].map(|(key, label, flag)| {
            let params = if only(flag) { json!({"flag": null, "flags": []}) } else { json!({"flag": key, "flags": []}) };
            Choice::new(label, params, shown.as_ref().is_some_and(|s| s.contains(&flag)))
        }),
    );
    v.push(MenuNode::Separator);
    v.extend(choices("library.filter", [Choice::new("None", json!({"flag": null, "flags": []}), false).enabled(shown.is_some())]));
    v
}

/// The filter bar's colour labels: several at once (any of them matches), each toggling.
fn filter_by_label(app: &LccApp) -> Vec<MenuNode> {
    use lcc_catalog::ColorLabel;
    let shown: Vec<ColorLabel> = app.session.filter.labels_shown().unwrap_or_default();
    let names = |labels: &[ColorLabel]| -> Vec<String> { labels.iter().map(|l| format!("{l:?}").to_lowercase()).collect() };
    let mut v = choices(
        "library.filter",
        ColorLabel::ALL.iter().map(|l| {
            let on = shown.contains(l);
            let mut next: Vec<ColorLabel> = shown.iter().copied().filter(|x| x != l).collect();
            if !on {
                next.push(*l);
            }
            next.sort();
            Choice::new(crate::i18n::color_label(&app.session.catalog, *l), json!({"label": null, "labels": names(&next)}), on)
        }),
    );
    v.push(MenuNode::Separator);
    let any = app.session.filter.labels_shown().is_some();
    v.extend(choices("library.filter", [Choice::new("None", json!({"label": null, "labels": []}), false).enabled(any)]));
    v
}

/// Classic's Filter by Kind: master photos, virtual copies and videos, each on its own (the same row again clears it).
fn filter_by_kind(app: &LccApp) -> Vec<MenuNode> {
    use lcc_catalog::MediaKind;
    let f = &app.session.filter;
    let row = |label: &str, on: bool, set: Value, clear: Value| Choice::new(label, if on { clear } else { set }, on);
    let mut v = choices(
        "library.filter",
        [
            row("Master Photos", f.copy == Some(false), json!({"copy": false}), json!({"copy": null})),
            row("Virtual Copies", f.copy == Some(true), json!({"copy": true}), json!({"copy": null})),
            row("Videos", f.kind == Some(MediaKind::Video), json!({"kind": "video"}), json!({"kind": null})),
        ],
    );
    v.push(MenuNode::Separator);
    let any = f.copy.is_some() || f.kind == Some(MediaKind::Video);
    v.extend(choices("library.filter", [Choice::new("None", json!({"copy": null, "kind": null}), false).enabled(any)]));
    v
}

/// The Metadata filter's columns: one row per field, checked while the filter bar lists it.
fn filter_by_metadata(app: &LccApp) -> Vec<MenuNode> {
    use crate::panels::filterbar::COLUMN_FIELDS;
    let shown = &app.ui.filter_columns;
    choices(
        "view.filterColumns",
        COLUMN_FIELDS.iter().map(|(field, label)| Choice::new(*label, json!({"toggle": field}), shown.iter().any(|c| c == field))),
    )
}

/// The saved filter presets; the one equal to the filter now is checked.
fn filter_by_preset(app: &LccApp) -> Vec<MenuNode> {
    let s = &app.session;
    if s.filter_presets.is_empty() {
        return vec![item("filter.applyPreset", Value::Null, "None", None, false, None)];
    }
    choices("filter.applyPreset", s.filter_presets.iter().map(|p| Choice::new(p.name.clone(), json!({"name": p.name}), p.filter == s.filter)))
}

/// The current keyword set's nine keywords (⌥1–⌥9); on when every target photo has it.
fn set_keyword(app: &LccApp) -> Vec<MenuNode> {
    let sets = lcc_engine::cmd::keywords::keyword_sets_json(&app.session);
    let keywords: Vec<String> = sets["keywords"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
    let targets = app.session.selected();
    let has = |k: &str| {
        !targets.is_empty()
            && targets.iter().all(|id| {
                app.session.catalog.photo(*id).is_some_and(|p| p.meta.keywords.iter().any(|x| lcc_catalog::keywords::same(x.trim(), k.trim())))
            })
    };
    choices(
        "keyword.toggleFromSet",
        (1..=9usize).map(|i| {
            let k = keywords.get(i - 1).map(|k| k.trim()).unwrap_or_default();
            // (an empty slot does nothing: shown, greyed)
            let label = if k.is_empty() { format!("({i})") } else { k.to_string() };
            Choice::new(label, json!({"index": i}), !k.is_empty() && has(k)).enabled(!k.is_empty() && !targets.is_empty()).key(format!("Alt+{i}"))
        }),
    )
}

/// The keyword sets (Recent Keywords first) with the one in use checked, then Edit Set….
fn keyword_set(app: &LccApp) -> Vec<MenuNode> {
    let sets = lcc_engine::cmd::keywords::keyword_sets_json(&app.session);
    let current = sets["current"].as_str().unwrap_or_default().to_string();
    let mut v = choices(
        "keyword.useSet",
        sets["sets"].as_array().into_iter().flatten().map(|set| {
            let name = set["name"].as_str().unwrap_or_default();
            let builtin = set["builtin"].as_bool().unwrap_or(false);
            Choice::new(crate::i18n::builtin_label(name, builtin), json!({"name": name}), name == current)
        }),
    );
    v.push(MenuNode::Separator);
    v.push(item("dialog.keywordSet", Value::Null, "Edit Set…", None, true, None));
    v
}

/// Map ▸ Location Filter: None, Visible on Map, Tagged, Untagged, then the saved locations.
fn location_filter(app: &LccApp) -> Vec<MenuNode> {
    let now = app.map.filter.key();
    let mut v = choices(
        "map.filter",
        [("all", "None"), ("visible", "Visible on Map"), ("tagged", "Tagged"), ("untagged", "Untagged")]
            .map(|(key, label)| Choice::new(label, json!({"filter": key}), now == key)),
    );
    let places: Vec<String> = app.session.catalog.saved_locations().map(|l| l.name.clone()).collect();
    if !places.is_empty() {
        v.push(MenuNode::Separator);
        v.extend(choices(
            "map.filter",
            places.into_iter().map(|n| {
                let key = format!("location:{n}");
                Choice::new(n, json!({"filter": key}), now == key)
            }),
        ));
    }
    v
}

/// Map ▸ Map Style: the tile servers (built-in and added), the one in use checked.
fn map_style(app: &LccApp) -> Vec<MenuNode> {
    let now = app.map.server().id;
    choices("map.style", app.map.servers().into_iter().map(|s| Choice::new(s.name.clone(), json!({"id": s.id}), s.id == now)))
}

/// Play ▸ Content: which photos the slideshow plays.
fn slideshow_content(app: &LccApp) -> Vec<MenuNode> {
    use crate::slideshow_ui::UsePhotos;
    let now = app.ui.slides.use_photos;
    choices(
        "slideshow.set",
        [(UsePhotos::All, "All Filmstrip Photos"), (UsePhotos::Selected, "Selected Photos"), (UsePhotos::Flagged, "Flagged Photos")]
            .map(|(u, label)| Choice::new(label, json!({"usePhotos": u}), now == u)),
    )
}

/// Tools ▸ Target Adjustment: the targeted-adjustment tool in use is checked; None puts it down.
fn target_adjustment(app: &LccApp) -> Vec<MenuNode> {
    let tool = app.ui.tool.strip_prefix("tat:").unwrap_or_default();
    let bw = app.session.active().and_then(|id| app.session.develop_of(id)).is_some_and(|d| crate::is_bw(&d));
    // a B&W photo mixes its greys with Luminance's drag only
    let rows: &[(&str, &str)] = if bw {
        &[("curve", "Tone Curve"), ("lum", "B&W Mix")]
    } else {
        &[("curve", "Tone Curve"), ("hue", "Hue"), ("sat", "Saturation"), ("lum", "Luminance")]
    };
    let on = app.session.active().is_some();
    let mut v = choices("tool.tat", rows.iter().map(|(key, label)| Choice::new(*label, json!({"target": key}), tool == *key).enabled(on)));
    v.push(MenuNode::Separator);
    v.extend(choices("tool.tat", [Choice::new("None", json!({"target": null}), tool.is_empty())]));
    v
}

/// Add / Subtract / Intersect Mask with: every kind Create New Mask offers, in its order (painting
/// only adds or erases, so Intersect has no Brush). Greyed while no mask is selected.
/// Create New Mask ▸ Select Landscape: one row per class `mask.add {kind: "landscape"}` takes. The first three work from the
/// photo analysis; the rest need the EoMT scene model (their error offers its download).
fn select_landscape(app: &LccApp) -> Vec<MenuNode> {
    let on = app.session.active().is_some();
    let row = |label: &str, class: &str| Choice::new(label, json!({"kind": "landscape", "class": class}), false).enabled(on).plain();
    let mut v = choices("mask.add", [row("Sky", "sky"), row("Water", "water"), row("Vegetation", "vegetation")]);
    v.push(MenuNode::Separator);
    v.extend(choices(
        "mask.add",
        [
            row("Mountains", "mountains"),
            row("Architecture", "architecture"),
            row("Natural Ground", "natural ground"),
            row("Artificial Ground", "artificial ground"),
        ],
    ));
    v
}

fn mask_with(app: &LccApp, op: &str) -> Vec<MenuNode> {
    let on = app.session.active_mask.is_some() && app.session.active().is_some();
    choices(
        "mask.startComponent",
        crate::panels::masking::MASK_KINDS.iter().filter(|(kind, _)| !(*kind == "brush" && op == "intersect")).map(|(kind, label)| {
            let label = match *kind {
                "prompt" => "Describe…",
                "linear" => "Linear Gradient",
                "radial" => "Radial Gradient",
                "luminanceRange" => "Luminance Range",
                "colorRange" => "Color Range",
                "depthRange" => "Depth Range",
                "people" => "People…",
                "concept" => "Remembered…",
                _ => label,
            };
            Choice::new(label, json!({"kind": kind, "op": op}), false).enabled(on).plain()
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menubar::{menu_bar, run_item};

    fn app() -> LccApp {
        let mut session = lcc_engine::Session::with_demo();
        session.selection = lcc_engine::Selection::single(session.visible_cloned()[0]);
        LccApp::new(session, Default::default())
    }

    /// (label, params, checked, enabled) of each row of the submenu `name`.
    fn rows(app: &LccApp, name: &str) -> Vec<(String, Value, Option<bool>, bool)> {
        expanded(app, name)
            .unwrap_or_else(|| panic!("{name} is not a choice submenu"))
            .into_iter()
            .filter_map(|n| match n {
                MenuNode::Item { label, params, checked, enabled, .. } => Some((label, params, checked, enabled)),
                _ => None,
            })
            .collect()
    }

    fn checked(app: &LccApp, name: &str) -> Vec<String> {
        rows(app, name).into_iter().filter(|r| r.2 == Some(true)).map(|r| r.0).collect()
    }

    /// Run the row labelled `label` as the menu does.
    fn pick(app: &mut LccApp, name: &str, label: &str) {
        let node = expanded(app, name).unwrap_or_default().into_iter().find(|n| matches!(n, MenuNode::Item { label: l, .. } if l == label));
        let Some(MenuNode::Item { id, params, enabled, .. }) = node else { panic!("{name} has no row {label}") };
        assert!(enabled, "{name} / {label} is greyed");
        run_item(app, &id, params).unwrap_or_else(|e| panic!("{name} / {label}: {e}"));
    }

    #[test]
    fn filter_by_flag_follows_the_filter_and_a_second_pick_clears_it() {
        let mut a = app();
        assert!(checked(&a, "Filter by Flag").is_empty());
        pick(&mut a, "Filter by Flag", "Rejected");
        assert_eq!(a.session.filter.flags_shown(), Some(vec![lcc_catalog::Flag::Reject]));
        assert_eq!(checked(&a, "Filter by Flag"), ["Rejected"]);
        // set in the filter bar's way too: the menu shows what is filtered
        a.run("library.filter", json!({"flag": "pick"})).unwrap();
        assert_eq!(checked(&a, "Filter by Flag"), ["Flagged"]);
        pick(&mut a, "Filter by Flag", "Flagged");
        assert!(a.session.filter.flags_shown().is_none());
        pick(&mut a, "Filter by Flag", "Unflagged");
        pick(&mut a, "Filter by Flag", "None");
        assert!(a.session.filter.flags_shown().is_none());
    }

    #[test]
    fn filter_by_kind_picks_masters_virtual_copies_and_videos() {
        let mut a = app();
        assert!(checked(&a, "Filter by Kind").is_empty());
        pick(&mut a, "Filter by Kind", "Virtual Copies");
        assert_eq!(a.session.filter.copy, Some(true));
        assert_eq!(checked(&a, "Filter by Kind"), ["Virtual Copies"]);
        pick(&mut a, "Filter by Kind", "Master Photos");
        assert_eq!(checked(&a, "Filter by Kind"), ["Master Photos"]);
        // every photo shown is a master
        assert!(a.session.visible_cloned().iter().all(|id| a.session.catalog.photo(*id).is_some_and(|p| p.copy_of.is_none())));
        pick(&mut a, "Filter by Kind", "Master Photos");
        assert_eq!(a.session.filter.copy, None);
        pick(&mut a, "Filter by Kind", "Videos");
        assert_eq!(a.session.filter.kind, Some(lcc_catalog::MediaKind::Video));
        pick(&mut a, "Filter by Kind", "None");
        assert_eq!(a.session.filter.kind, None);
    }

    #[test]
    fn filter_by_metadata_adds_and_removes_columns() {
        let mut a = app();
        assert!(checked(&a, "Filter by Metadata").is_empty());
        pick(&mut a, "Filter by Metadata", "Camera");
        pick(&mut a, "Filter by Metadata", "ISO");
        assert_eq!(checked(&a, "Filter by Metadata"), ["Camera", "ISO"]);
        assert!(a.ui.filter_bar);
        pick(&mut a, "Filter by Metadata", "Camera");
        assert_eq!(checked(&a, "Filter by Metadata"), ["ISO"]);
        pick(&mut a, "Filter by Metadata", "ISO");
        assert!(a.ui.filter_columns.is_empty());
    }

    #[test]
    fn filter_by_color_label_toggles_labels_together() {
        use lcc_catalog::ColorLabel;
        let mut a = app();
        pick(&mut a, "Filter by Color Label", "Red");
        pick(&mut a, "Filter by Color Label", "Blue");
        assert_eq!(a.session.filter.labels_shown(), Some(vec![ColorLabel::Red, ColorLabel::Blue]));
        assert_eq!(checked(&a, "Filter by Color Label"), ["Red", "Blue"]);
        pick(&mut a, "Filter by Color Label", "Red");
        assert_eq!(a.session.filter.labels_shown(), Some(vec![ColorLabel::Blue]));
        pick(&mut a, "Filter by Color Label", "None");
        assert!(a.session.filter.labels_shown().is_none());
        assert!(!rows(&a, "Filter by Color Label").last().is_some_and(|r| r.3), "None is greyed with nothing to clear");
    }

    #[test]
    fn filter_by_preset_lists_the_saved_presets_and_applies_one() {
        let mut a = app();
        // nothing saved: one greyed row
        assert!(matches!(rows(&a, "Filter by Preset").as_slice(), [(_, _, _, false)]));
        a.run("library.filter", json!({"rating": 3})).unwrap();
        a.run("filter.savePreset", json!({"name": "Good"})).unwrap();
        a.run("library.filter", json!({"rating": 0, "ratingOp": "atLeast"})).unwrap();
        assert!(checked(&a, "Filter by Preset").is_empty());
        pick(&mut a, "Filter by Preset", "Good");
        assert_eq!(a.session.filter.rating, 3);
        assert_eq!(checked(&a, "Filter by Preset"), ["Good"]);
    }

    #[test]
    fn set_keyword_toggles_the_current_sets_keywords_on_the_photo() {
        let mut a = app();
        a.run("keyword.useSet", json!({"name": "Outdoor Photography"})).unwrap();
        let r = rows(&a, "Set Keyword");
        assert_eq!(r.len(), 9);
        let first = r[0].0.clone();
        assert!(!first.is_empty() && r[0].3, "{r:?}");
        let id = a.session.selected()[0];
        let has = |a: &LccApp| a.session.catalog.photo(id).is_some_and(|p| p.meta.keywords.iter().any(|k| lcc_catalog::keywords::same(k, &first)));
        // (the demo photos carry some keywords already: the check follows the photo either way)
        let start = has(&a);
        assert_eq!(checked(&a, "Set Keyword").contains(&first), start);
        pick(&mut a, "Set Keyword", &first);
        assert_eq!(has(&a), !start);
        assert_eq!(checked(&a, "Set Keyword").contains(&first), !start);
        pick(&mut a, "Set Keyword", &first);
        assert_eq!(has(&a), start);
        // with nothing selected the rows are greyed
        a.session.selection = lcc_engine::Selection::default();
        assert!(rows(&a, "Set Keyword").iter().all(|r| !r.3));
    }

    #[test]
    fn keyword_set_chooses_the_set_and_edit_opens_the_editor() {
        let mut a = app();
        assert_eq!(checked(&a, "Keyword Set"), ["Recent Keywords"]);
        pick(&mut a, "Keyword Set", "Outdoor Photography");
        assert_eq!(checked(&a, "Keyword Set"), ["Outdoor Photography"]);
        assert_eq!(a.session.keyword_set.as_deref(), Some("Outdoor Photography"));
        let edit = expanded(&a, "Keyword Set").unwrap_or_default().into_iter().find_map(|n| match n {
            MenuNode::Item { id, params, .. } if id == "dialog.keywordSet" => Some((id, params)),
            _ => None,
        });
        let (id, params) = edit.expect("Edit Set row");
        run_item(&mut a, &id, params).unwrap();
        assert!(
            matches!(&a.ui.dialog, Some(crate::state::Dialog::KeywordSet { replaces: Some(n), slots, .. }) if n == "Outdoor Photography" && slots.len() == 9)
        );
    }

    #[test]
    fn map_location_filter_and_style_follow_the_map() {
        let mut a = app();
        assert_eq!(checked(&a, "Location Filter"), ["None"]);
        pick(&mut a, "Location Filter", "Tagged");
        assert_eq!(a.map.filter.key(), "tagged");
        assert_eq!(checked(&a, "Location Filter"), ["Tagged"]);
        let styles = rows(&a, "Map Style");
        assert!(styles.len() > 1 && styles.iter().filter(|r| r.2 == Some(true)).count() == 1, "{styles:?}");
        let other = styles.iter().find(|r| r.2 == Some(false)).expect("another style").clone();
        pick(&mut a, "Map Style", &other.0);
        assert_eq!(checked(&a, "Map Style"), [other.0]);
    }

    #[test]
    fn slideshow_content_picks_the_photos_to_play() {
        let mut a = app();
        assert_eq!(checked(&a, "Slideshow Content"), ["All Filmstrip Photos"]);
        pick(&mut a, "Slideshow Content", "Flagged Photos");
        assert_eq!(a.ui.slides.use_photos, crate::slideshow_ui::UsePhotos::Flagged);
        assert_eq!(checked(&a, "Slideshow Content"), ["Flagged Photos"]);
    }

    #[test]
    fn target_adjustment_arms_the_tool_where_its_drag_acts() {
        let mut a = app();
        pick(&mut a, "Target Adjustment", "Tone Curve");
        assert_eq!(a.ui.tool, "tat:curve");
        assert!(a.ui.section_open("curve") && a.ui.right == crate::state::RightPanel::Edit);
        assert_eq!(checked(&a, "Target Adjustment"), ["Tone Curve"]);
        pick(&mut a, "Target Adjustment", "Saturation");
        assert_eq!(a.ui.tool, "tat:sat");
        assert_eq!(a.ui.mixer_group, "sat", "the Mixer shows the group the drag changes");
        // the Mixer would put a tool of a hidden group away
        crate::panels::edit::drop_hidden_mixer_tool(&mut a);
        assert_eq!(a.ui.tool, "tat:sat");
        pick(&mut a, "Target Adjustment", "None");
        assert!(a.ui.tool.is_empty());
        assert_eq!(checked(&a, "Target Adjustment"), ["None"]);
        // bad input is an error, not a panic
        assert!(run_item(&mut a, "tool.tat", json!({"target": "bogus"})).is_err());
        assert!(a.ui.tool.is_empty());
    }

    #[test]
    fn select_landscape_lists_every_class_as_a_landscape_mask() {
        let a = app();
        let r = rows(&a, "Select Landscape");
        assert!(r.len() >= 7 && r.iter().all(|r| r.1["kind"] == "landscape" && r.3), "{r:?}");
        let mut a = a;
        // a class from the analysis works at once
        let sky = r.iter().find(|r| r.1["class"] == "sky").expect("sky row");
        run_item(&mut a, "mask.add", sky.1.clone()).unwrap();
        let d = a.session.active().and_then(|id| a.session.develop_of(id)).expect("develop");
        assert!(d.masks.iter().any(|m| matches!(&m.components[0].shape, lcc_develop::MaskShape::Landscape { class } if class == "sky")));
    }

    #[test]
    fn mask_with_combines_a_new_component_into_the_selected_mask() {
        let mut a = app();
        // no mask: every row is greyed and the command says why
        assert!(rows(&a, "Add to Mask with").iter().all(|r| !r.3));
        assert!(run_item(&mut a, "mask.startComponent", json!({"kind": "linear", "op": "add"})).is_err());
        a.run("mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.2})).unwrap();
        let components = |a: &LccApp| {
            let d = a.session.active().and_then(|id| a.session.develop_of(id)).expect("develop");
            d.masks.iter().find(|m| Some(m.id) == a.session.active_mask).map_or(0, |m| m.components.len())
        };
        assert_eq!(components(&a), 1);
        // one row per kind Create New Mask offers; painting only adds or erases
        let kinds = crate::panels::masking::MASK_KINDS.len();
        assert_eq!(rows(&a, "Add to Mask with").len(), kinds);
        assert_eq!(rows(&a, "Subtract from Mask with").len(), kinds);
        assert_eq!(rows(&a, "Intersect Mask with").len(), kinds - 1);
        assert!(rows(&a, "Add to Mask with").iter().all(|r| r.3 && r.2.is_none()));
        pick(&mut a, "Subtract from Mask with", "Linear Gradient");
        assert_eq!(components(&a), 2);
        assert!(run_item(&mut a, "mask.startComponent", json!({"kind": "nope", "op": "add"})).is_err());
        assert!(run_item(&mut a, "mask.startComponent", json!({"kind": "linear", "op": "xor"})).is_err());
        assert!(run_item(&mut a, "mask.startComponent", json!({})).is_err());
        assert_eq!(components(&a), 2);
    }

    #[test]
    fn the_menus_carry_the_submenus_not_coming_soon_rows() {
        use crate::module::ModuleId;
        let want = [
            (ModuleId::Library, "Filter by Flag"),
            (ModuleId::Library, "Filter by Color Label"),
            (ModuleId::Library, "Filter by Preset"),
            (ModuleId::Library, "Set Keyword"),
            (ModuleId::Library, "Keyword Set"),
            (ModuleId::Map, "Location Filter"),
            (ModuleId::Map, "Map Style"),
            (ModuleId::Slideshow, "Content"),
            (ModuleId::Develop, "Target Adjustment"),
            (ModuleId::Develop, "Add to Mask with"),
            (ModuleId::Develop, "Subtract from Mask with"),
            (ModuleId::Develop, "Intersect Mask with"),
        ];
        for (module, label) in want {
            let mut a = app();
            a.ui.module = module;
            let found = menu_bar(&a)
                .into_iter()
                .flat_map(|(_, v)| v)
                .any(|n| matches!(n, MenuNode::Submenu { label: l, children } if l == label && !children.is_empty()));
            assert!(found, "{module:?}: no {label} submenu");
        }
    }
}
