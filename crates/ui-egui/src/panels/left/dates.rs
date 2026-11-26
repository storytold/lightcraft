use super::rows::tree_open;
use super::{LibrarySource, LightcraftApp, json};

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
    /// The source of `shown`: each row's key at its place, and a revision over the rows as they are
    /// now, so the framework's memory of their heights and labels follows a window that moved.
    fn of(shown: Vec<DateItem>) -> Self {
        use std::hash::{Hash as _, Hasher as _};
        let index = shown.iter().enumerate().map(|(at, row)| (row.key.clone(), at)).collect();
        let mut hasher = std::hash::DefaultHasher::new();
        shown.hash(&mut hasher);
        Self { shown, index, stamp: hasher.finish() }
    }

    /// The rows `built` of the tree, as the list's own source: it builds only these, the rows left
    /// out standing as the space above and below them (see `rows::rows_in_view`).
    fn windowed(mut self, built: std::ops::Range<usize>) -> Self {
        let shown = self.shown.drain(built).collect();
        Self::of(shown)
    }

    fn from_groups(ui: &egui::Ui, groups: &[lightcraft_catalog::DateGroup]) -> Self {
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
        Self::of(shown)
    }
}

fn sdk_date_rows(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use craft::behavior::{ColumnKey, ColumnWidth, PartKey, Selection};
    use craft::ui::contract::{ActivateOn, Bind, CollectionEvent, ColumnSpec, Props as _, SelectMode, ShowCollection as _};
    use craft::ui::list::{List, ListRow};
    use craft::ui::primitives::IconSpec;
    const NAME: ColumnKey = ColumnKey::new("by-date-caption");
    const COUNT: ColumnKey = ColumnKey::new("by-date-count");
    // tells this list's row height from the sidebar's other lists
    const SALT: &str = "by-date";
    let groups = app.caches.date_groups(&app.session.catalog);
    let all = DateItems::from_groups(ui, groups.as_ref());
    if all.shown.is_empty() {
        return;
    }
    // Every month of every expanded year can show its days: well over a thousand rows, more than the framework's
    // size guard (32,768 points) has room for, and a list that measured them all would stop showing (see
    // `rows::rows_in_view`). The rows in view are built, the rest stand as the space above and below them.
    let height = super::rows::row_height(ui, SALT);
    let (built, above, below) = super::rows::rows_in_view(ui, all.shown.len(), height, None);
    let (left, top, width) = (ui.cursor().left(), ui.cursor().top(), ui.available_width());
    for (at, row) in all.shown.iter().enumerate().filter(|(at, _)| !built.contains(at)) {
        // a row left out keeps its name for the control channel, at where it would be
        let place = egui::Rect::from_min_size(egui::pos2(left, top + at as f32 * height), egui::vec2(width, height));
        super::register(ui, format!("source:date:{}", row.key), place);
    }
    let source = all.windowed(built);
    let show_counts = app.ui.show_counts;
    let mut columns = vec![ColumnSpec::new(NAME, "", ColumnWidth::fill(1.0))];
    if show_counts {
        columns.push(ColumnSpec::new(COUNT, "", ColumnWidth::Fixed(40.0)));
    }
    // The collection's lead is transient navigation, not the photo filter. Only
    // its real Activate event executes the original source/filter commands.
    let mut selected = app.session.filter.date.clone().map(Selection::single).unwrap_or_default();
    if above > 0.0 {
        ui.add_space(above);
    }
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
            if item.depth == 1 {
                // the same month of two years is told apart: its triangle reads "Expand March 2025"
                row = row.accessible_name(format!("{} {}", item.label, &item.key[..4]));
            }
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
    if below > 0.0 {
        ui.add_space(below);
    }
    let live = ui.is_enabled()
        && ui.is_visible()
        && !ui.is_sizing_pass()
        && !craft::ui::runtime::measure::active(ui.ctx())
        && out.response.enabled()
        && out.response.rect.intersects(ui.clip_rect());
    if !live {
        return;
    }
    if let Some(first) = out.shown.first() {
        super::rows::note_row_height(ui, SALT, first.rect.height());
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
