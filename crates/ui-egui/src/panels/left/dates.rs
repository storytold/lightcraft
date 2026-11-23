use super::rows::{INDENT_STEP, Row, tree_open, tree_toggle};
use super::{Icon, LibrarySource, LightcraftApp, json};

/// By Date: year → month → day; a click filters by that prefix, the triangle opens a level.
fn native_date_rows(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let groups = app.caches.date_groups(&app.session.catalog);
    for g in groups.iter() {
        if date_row(app, ui, &g.year, &crate::i18n::date_group_label(&g.year, true), g.count, 0.0) {
            for (m, n) in &g.months {
                let label = crate::i18n::date_group_label(m, true);
                if date_row(app, ui, m, &label, *n, INDENT_STEP) {
                    for (d, n) in g.days.iter().filter(|(d, _)| d.starts_with(m.as_str())) {
                        let label = crate::i18n::date_group_label(d, true);
                        date_row(app, ui, d, &label, *n, 2.0 * INDENT_STEP);
                    }
                }
            }
        }
    }
}

/// One By Date row (`key`: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`); returns whether it is open.
fn date_row(app: &mut LightcraftApp, ui: &mut egui::Ui, key: &str, label: &str, count: usize, indent: f32) -> bool {
    let open_id = egui::Id::unique(("date-open", key.to_string()));
    let mut open = tree_open(ui, open_id, false);
    let sel = app.session.filter.date.as_deref() == Some(key);
    let resp = Row::new(&format!("date:{key}"), Icon::Clock, label).count(Some(count)).selected(sel).indent(indent).show(app, ui);
    if key.len() < 10 {
        // (the name carries the year, so two Marches in different years differ in a screen reader's list of triangles)
        tree_toggle(ui, &resp, indent, open_id, &mut open, format!("dateToggle:{key}"), &crate::i18n::date_group_label(key, false));
    }
    if resp.clicked() {
        let v = if sel { serde_json::Value::Null } else { json!(key) };
        browse_all_photos(app, !sel);
        let _ = app.run("library.filter", json!({"date": v}));
    }
    open
}

/// By Date and Keywords count every photo in the library, so choosing a row shows those photos
/// from All Photos, not from whatever album or folder happened to be open, where they could be
/// missing (issue #341). Only when choosing (`on`), not when clearing the row again.
pub(crate) fn browse_all_photos(app: &mut LightcraftApp, on: bool) {
    if on && app.session.source != LibrarySource::All {
        let _ = app.run("library.source", json!({"kind": "all"}));
    }
}

/// The By Date tree, on the framework's collection lifecycle.
pub(super) fn date_rows(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if craft::ui::runtime::measure::active(ui.ctx()) && !ui.is_sizing_pass() {
        // A context-only measure supplies a supported non-publishing sizing UI,
        // rather than registering live collection input in the measured pass.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
        sdk_date_rows(app, &mut child);
    } else {
        sdk_date_rows(app, ui);
    }
}

#[derive(Hash)]
struct DateItem {
    key: String,
    label: String,
    count: usize,
    depth: usize,
    group: bool,
    open: bool,
}
struct DateItems {
    shown: Vec<DateItem>,
    index: std::collections::HashMap<String, usize>,
    stamp: u64,
}
impl craft::behavior::DisplayOrder<String> for DateItems {
    fn len(&self) -> usize {
        self.shown.len()
    }
    fn key(&self, at: usize) -> String {
        self.shown[at].key.clone()
    }
    fn index_of(&self, key: &String) -> Option<usize> {
        self.index.get(key).copied()
    }
    fn depth(&self, at: usize) -> usize {
        self.shown[at].depth
    }
    fn has_children(&self, at: usize) -> bool {
        self.shown[at].group
    }
    fn open(&self, at: usize) -> bool {
        self.shown[at].open
    }
    fn text(&self, at: usize) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(&self.shown[at].label))
    }
    fn revision(&self) -> Option<u64> {
        Some(self.stamp)
    }
}
impl craft::behavior::ItemSource<String> for DateItems {}
impl DateItems {
    fn from_groups(ui: &egui::Ui, groups: &[lightcraft_catalog::DateGroup]) -> Self {
        use std::hash::{Hash as _, Hasher as _};
        let mut shown = Vec::new();
        let item = |key: &str, count: usize, depth: usize| DateItem {
            key: key.into(),
            label: crate::i18n::date_group_label(key, true),
            count,
            depth,
            group: key.len() < 10,
            open: tree_open(ui, egui::Id::unique(("date-open", key.to_owned())), false),
        };
        for group in groups {
            let year = item(&group.year, group.count, 0);
            let opened = year.open;
            shown.push(year);
            if !opened {
                continue;
            }
            for (month, count) in &group.months {
                let row = item(month, *count, 1);
                let opened = row.open;
                shown.push(row);
                if opened {
                    for (day, count) in group.days.iter().filter(|(day, _)| day.starts_with(month.as_str())) {
                        shown.push(item(day, *count, 2));
                    }
                }
            }
        }
        let index = shown.iter().enumerate().map(|(at, row)| (row.key.clone(), at)).collect();
        let mut hasher = std::hash::DefaultHasher::new();
        shown.hash(&mut hasher);
        Self { shown, index, stamp: hasher.finish() }
    }
}

fn sdk_date_rows(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use craft::behavior::{ColumnKey, ColumnWidth, PartKey, Selection};
    use craft::ui::contract::{ActivateOn, Bind, CollectionEvent, ColumnSpec, Props as _, SelectMode, ShowCollection as _};
    use craft::ui::list::{List, ListRow};
    use craft::ui::primitives::IconSpec;
    const NAME: ColumnKey = ColumnKey::new("by-date-caption");
    const COUNT: ColumnKey = ColumnKey::new("by-date-count");
    let groups = app.caches.date_groups(&app.session.catalog);
    let source = DateItems::from_groups(ui, groups.as_ref());
    if source.shown.is_empty() {
        return;
    }
    let show_counts = app.ui.show_counts;
    let mut columns = vec![ColumnSpec::new(NAME, "", ColumnWidth::fill(1.0))];
    if show_counts {
        columns.push(ColumnSpec::new(COUNT, "", ColumnWidth::Fixed(40.0)));
    }
    // The collection's lead is transient navigation, not the photo filter. Only
    // its real Activate event executes the original source/filter commands.
    let mut selected = app.session.filter.date.clone().map(Selection::single).unwrap_or_default();
    let out = List::<String>::new(crate::i18n::tr("By Date").to_owned())
        .id_salt("by-date-tree")
        .header(false)
        .tree(true)
        .natural_height(true)
        .select(SelectMode::Single)
        .activate(ActivateOn::Click)
        .selection(Bind::bound(&mut selected))
        .columns(columns)
        .enabled(ui.is_enabled())
        .show_items(ui, &source, &mut |key, icx| {
            let item = &source.shown[source.index[key]];
            let mut row =
                ListRow::new(item.label.clone()).depth(item.depth).cells(icx.cells()).icon(IconSpec::svg(craft::behavior::IconId::new("clock")));
            if item.group {
                row = row.group(item.open);
                // Public ItemCx/CollectionResponse opener API returns the real
                // declared disclosure-part response; no invented hit geometry.
                icx.part_opener(PartKey::new("disclosure"));
            }
            if show_counts {
                row = row.cell(COUNT, item.count.to_string());
            }
            row
        });
    let live = ui.is_enabled()
        && ui.is_visible()
        && !ui.is_sizing_pass()
        && !craft::ui::runtime::measure::active(ui.ctx())
        && out.response.enabled()
        && out.response.rect.intersects(ui.clip_rect());
    if !live {
        return;
    }
    for row in &out.shown {
        if !row.rect.intersects(ui.clip_rect()) {
            continue;
        }
        super::register(ui, format!("source:date:{}", row.key), row.rect);
        if app.session.filter.date.as_ref() == Some(&row.key) {
            super::register(ui, format!("highlight:date:{}", row.key), row.rect);
        }
        for (column, rect) in &row.cells {
            if *column == NAME {
                super::register(ui, format!("label:date:{}", row.key), *rect);
            }
            if *column == COUNT && show_counts {
                super::register(ui, format!("count:date:{}", row.key), *rect);
            }
        }
        for (_, response) in &row.openers {
            crate::widgets::register_response(format!("dateToggle:{}", row.key), response.rect, response);
        }
    }
    for event in &out.events {
        match event {
            CollectionEvent::Open(change) if source.index.contains_key(&change.key) && change.key.len() < 10 => {
                // The legacy disclosure changes this exact group, including on
                // Alt-click; it never rewrites the other groups' remembered state.
                super::rows::set_tree_open(ui, egui::Id::unique(("date-open", change.key.clone())), change.open);
            }
            CollectionEvent::Activate(key) if source.index.contains_key(key) => {
                let sel = app.session.filter.date.as_deref() == Some(key.as_str());
                let v = if sel { serde_json::Value::Null } else { json!(key) };
                browse_all_photos(app, !sel);
                let _ = app.run("library.filter", json!({"date":v}));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "date_filter_flow_tests.rs"]
mod date_filter_port_tests;
