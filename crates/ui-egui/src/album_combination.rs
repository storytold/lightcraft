//! Live set operations over albums, expressed as the catalog's existing smart-album rules.
//! Membership updates are reflected immediately, without duplicating or moving photos.

use crate::LightcraftApp;
use lightcraft_catalog::rules::Match;
use lightcraft_catalog::{AlbumId, Rule, RuleSet};
use lightcraft_engine::LibrarySource;
use serde_json::{Value, json};

pub const OPERATIONS: &[(&str, &str)] =
    &[("union", "OR: any album"), ("intersection", "AND: every album"), ("difference", "First minus others"), ("exclusive", "Only one album")];

/// Build one catalog album-membership predicate.
fn field(id: u64, included: bool) -> Rule {
    Rule::Field { field: "album".into(), op: if included { "is" } else { "isNot" }.into(), value: json!(id) }
}

/// Translate a named set operation into the catalog's existing live rules.
fn rules(ids: &[u64], operation: &str) -> Result<RuleSet, String> {
    let (mode, rules) = match operation {
        "union" => (Match::Any, ids.iter().map(|id| field(*id, true)).collect()),
        "intersection" => (Match::All, ids.iter().map(|id| field(*id, true)).collect()),
        "difference" => (Match::All, ids.iter().enumerate().map(|(i, id)| field(*id, i == 0)).collect()),
        // For two albums this is XOR. With more albums, keep photos belonging to exactly one.
        "exclusive" => (
            Match::Any,
            ids.iter()
                .map(|id| Rule::Group { group: RuleSet { mode: Match::All, rules: ids.iter().map(|other| field(*other, id == other)).collect() } })
                .collect(),
        ),
        _ => return Err("view.combineAlbums: unknown operation (union|intersection|difference|exclusive)".into()),
    };
    Ok(RuleSet { mode, rules })
}

/// Remove only the rules installed by this view, preserving unrelated filters.
pub fn clear(app: &mut LightcraftApp) {
    if let Some((ids, operation)) = app.ui.album_combination.take()
        && let Ok(set) = rules(&ids, &operation)
    {
        app.session.clear_view_rules(&set);
    }
}

/// Whether the stored combination still exactly describes the current live view.
pub fn active(app: &LightcraftApp) -> bool {
    app.ui.album_combination.as_ref().is_some_and(|(ids, op)| {
        (2..=8).contains(&ids.len())
            && app.session.source == LibrarySource::All
            && rules(ids, op).is_ok_and(|set| app.session.filter.rule_set.as_ref() == Some(&set))
    })
}

/// Validate and apply `view.combineAlbums`, returning the visible photo count.
pub fn command(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    let values = params.get("ids").and_then(Value::as_array).ok_or("view.combineAlbums: ids must be an array")?;
    if !(2..=8).contains(&values.len()) {
        return Err("view.combineAlbums: choose 2 to 8 albums".into());
    }
    let mut ids = Vec::new();
    for value in values {
        let id = value.as_u64().ok_or("view.combineAlbums: invalid album id")?;
        if app.session.catalog.album(AlbumId(id)).is_none_or(|a| a.folder) {
            return Err(format!("view.combineAlbums: no album {id}"));
        }
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.len() < 2 {
        return Err("view.combineAlbums: choose two distinct albums".into());
    }
    let operation = match params.get("operation") {
        None => "union",
        Some(value) => value.as_str().ok_or("view.combineAlbums: operation must be a string")?,
    };
    let set = rules(&ids, operation)?;
    let problems = set.check(&app.session.catalog);
    if !problems.is_empty() {
        return Err(problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "));
    }
    let preserve_filters = active(app) && app.ui.album_combination.as_ref().is_some_and(|(current, _)| current == &ids);
    let filter = if preserve_filters {
        // Changing only the operation keeps filters applied after the combination was opened.
        json!({"ruleSet": set})
    } else {
        // A source choice starts a fresh view; ordinary filters can be applied to the result next.
        let filter = lightcraft_catalog::Filter { rule_set: Some(set), ..Default::default() };
        let mut filter = serde_json::to_value(filter).map_err(|e| e.to_string())?;
        let fields = filter.as_object_mut().ok_or("view.combineAlbums: couldn't serialize the filter")?;
        // These defaults are normally omitted by Filter's serializer. library.filter merges a
        // partial payload, so spell them out to keep a previous Find Similar or multi-select
        // filter from narrowing the new source view.
        fields.insert("flags".into(), json!([]));
        fields.insert("labels".into(), json!([]));
        fields.insert("only".into(), json!([]));
        fields.insert("onlyNone".into(), json!(false));
        filter
    };
    app.session.source = LibrarySource::All;
    app.session.execute("library.filter", &filter).map_err(|e| e.to_string())?;
    if !preserve_filters {
        app.ui.search.clear();
    }
    app.ui.album_combination = Some((ids, operation.into()));
    app.session.keep_active_in_view();
    Ok(json!({"count": app.session.visible().len(), "operation": operation}))
}

/// A modifier-click adds or removes an album; a plain click still shows just that album.
pub fn toggle(app: &mut LightcraftApp, id: u64) {
    let (mut ids, operation) = if active(app) {
        app.ui.album_combination.clone().unwrap_or_default()
    } else {
        let ids = match app.session.source {
            LibrarySource::Album(a) => vec![a.0],
            _ => Vec::new(),
        };
        (ids, "union".into())
    };
    if ids.contains(&id) {
        ids.retain(|a| *a != id);
    } else {
        ids.push(id);
    }
    let result = if ids.len() >= 2 {
        app.run("view.combineAlbums", json!({"ids": ids, "operation": operation}))
    } else if let Some(id) = ids.first() {
        app.run("library.source", json!({"kind": "album", "id": id}))
    } else {
        app.run("library.source", json!({"kind": "all"}))
    };
    if let Err(e) = result {
        app.toast(&app.tasks.repaint.clone().unwrap_or_default(), e);
    }
}

/// Draw operation choices and the ordered names of the albums being combined.
pub fn controls(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if !active(app) {
        return;
    }
    let Some((ids, mut operation)) = app.ui.album_combination.clone() else { return };
    let before = operation.clone();
    ui.label(crate::i18n::tr("Combine Albums"));
    ui.horizontal_wrapped(|ui| {
        for (id, label) in OPERATIONS {
            let response = ui.selectable_value(&mut operation, (*id).into(), crate::i18n::tr(label));
            crate::widgets::register(ui, format!("albumOperation:{id}"), response.rect);
        }
    });
    let names: Vec<_> = ids.iter().filter_map(|id| app.session.catalog.album(AlbumId(*id))).map(|a| a.name.as_str()).collect();
    ui.label(names.join(" → "));
    ui.label(crate::i18n::tr("Cmd/Ctrl-click albums to combine. A plain click shows one album."));
    if before != operation
        && let Err(e) = app.run("view.combineAlbums", json!({"ids": ids, "operation": operation}))
    {
        app.toast(ui.ctx(), e);
    }
    ui.separator();
}
