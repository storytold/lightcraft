use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};

use craft::behavior::{ColumnKey, ColumnWidth, DisplayOrder, IconId, ItemSource, PartKey, Selection, SizeHint};
use craft::ui::contract::overlay::Anchor;
use craft::ui::contract::{
    ActivateOn, Bind, CollectionEvent, CollectionResponse, ColumnSpec, Props as _, SelectMode, Show as _, ShowCollection as _, ShowContainer as _, Trigger,
};
use craft::ui::display::Divider;
use craft::ui::list::{List, ListRow, TreeLeaves};
use craft::ui::menu::{Menu, MenuItem};
use craft::ui::primitives::IconSpec;
use egui::{Pos2, Rect};

use super::{pos2, register, vec2};

/// Temp-data key of the row (its widget id after `source:`, lower-case) the sidebar scrolls to
/// this frame: set before the sections are drawn, cleared after.
pub(super) const SCROLL_TO_ROW: &str = "left-scroll-to-row";

/// Temp-data key of the Folders choice whose rows above were last opened ([`reveal_chosen`]):
/// removed, they open again.
pub(super) const LIBFOLDER_REVEALED: &str = "libfolder-revealed";

/// Height a sidebar row is taken to have until the list has shown one (and for the room an empty
/// section's note takes).
pub(super) const ROW_HEIGHT: f32 = 29.0;

/// The deepest a tree of folders is followed: a catalog or a disk that loops must not run away.
pub(super) const MAX_DEPTH: usize = 64;

/// A list of this many rows or fewer is built whole; a longer one (a library can have thousands of
/// keywords or folders, issue #1011) builds the rows in view and a row below, and stands a filler
/// as tall as the rest in for the others.
const WINDOW_FROM: usize = 80;

/// Rows built below the visible ones.
const WINDOW_MARGIN: usize = 1;

/// One level of a sidebar tree at the framework's default steps, kept for the tests that name it.
#[cfg(test)]
pub(crate) const INDENT_STEP: f32 = 16.0;

/// The narrowest a level was drawn before the framework's tree took over the indentation.
#[cfg(test)]
pub(crate) const MIN_INDENT_STEP: f32 = 8.0;

/// How wide the sidebar's content needs to be. The tree rows size themselves now, so nothing asks
/// for more than the panel: always 0.
#[cfg(test)]
pub(crate) fn content_width(_ctx: &egui::Context) -> f32 {
    0.0
}

/// The step a level is drawn with: the framework's own, which does not narrow.
#[cfg(test)]
pub(crate) fn indent_step(_ctx: &egui::Context) -> f32 {
    INDENT_STEP
}

/// The widest step (16, 14, 12, 10 or 8 pt) at which a row `deepest` deep (in [`INDENT_STEP`]s)
/// keeps room for its name and count in a panel `panel_w` wide.
#[cfg(test)]
pub(crate) fn step_for(panel_w: f32, deepest: f32) -> f32 {
    let levels = deepest / INDENT_STEP;
    if levels < 1.0 {
        return INDENT_STEP;
    }
    let room = panel_w - 42.0 - 80.0 - 18.0 - 40.0;
    [16.0, 14.0, 12.0, 10.0].into_iter().find(|step| levels * step <= room).unwrap_or(MIN_INDENT_STEP)
}

/// What a disclosure triangle is called: "Expand Travel" / "Collapse Travel".
pub(crate) fn disclosure_name(open: bool, name: &str) -> String {
    if open { crate::i18n::tr_format!("Collapse {name}", name = name) } else { crate::i18n::tr_format!("Expand {name}", name = name) }
}

/// Whether the tree row `key` is open: what the user last chose this session, else `default`.
pub(super) fn tree_open(ui: &egui::Ui, key: egui::Id, default: bool) -> bool {
    ui.data(|d| d.get_temp(key)).unwrap_or(default)
}

/// Open or close the tree row `key` from now on.
pub(super) fn set_tree_open(ui: &egui::Ui, key: egui::Id, open: bool) {
    ui.data_mut(|d| d.insert_temp(key, open));
}

/// The row (its widget id, lower-case) the sidebar is asked to scroll to this frame
/// ([`SCROLL_TO_ROW`]).
fn scroll_target(ui: &egui::Ui) -> Option<String> {
    ui.data(|d| d.get_temp::<String>(egui::Id::unique(SCROLL_TO_ROW)))
}

/// Whether a layout is being measured or sized rather than shown: nothing is registered, run or
/// opened then.
pub(super) fn measuring(ui: &egui::Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

// ------------------------------------------------------------------------------ the list

const NAME: ColumnKey = ColumnKey::new("side-caption");
const COUNT: ColumnKey = ColumnKey::new("side-count");

/// One row of a sidebar list, as the framework's list shows it.
#[derive(Clone, Debug, Hash)]
pub(super) struct SideRow {
    /// The collection's key: unique in its list.
    pub key: String,
    /// What the control channel calls the row: `source:<id>`, `label:<id>`, `count:<id>`...
    /// (several rows can share one, as a folder listed under two places does).
    pub id: String,
    pub label: String,
    /// What a screen reader says when it is not the label (a folder's colour label).
    pub spoken: Option<String>,
    pub icon: Option<&'static str>,
    pub count: Option<usize>,
    pub depth: usize,
    pub group: bool,
    pub open: bool,
    /// The label in the strong weight (a section's title).
    pub strong: bool,
    /// Text for the count's column that is not a count of photos (a folded section's rows).
    pub note: Option<String>,
    /// A second, smaller line under the label.
    pub detail: Option<String>,
    /// A colour label's dot, before the count.
    pub mark: Option<egui::Color32>,
    /// What resting on the row says.
    pub tip: Option<String>,
    /// The control-channel name of the row's disclosure triangle.
    pub toggle: Option<String>,
    /// List the row under the usual `source:`, `label:`, `count:` names.
    pub register: bool,
    /// A filler this many points tall, standing in for rows that are not built.
    fill: Option<u32>,
}

impl SideRow {
    pub(super) fn new(id: impl Into<String>, icon: &'static str, label: impl Into<String>) -> Self {
        let id = id.into();
        SideRow {
            key: id.clone(),
            id,
            label: label.into(),
            spoken: None,
            icon: Some(icon),
            count: None,
            depth: 0,
            group: false,
            open: false,
            strong: false,
            note: None,
            detail: None,
            mark: None,
            tip: None,
            toggle: None,
            register: true,
            fill: None,
        }
    }

    pub(super) fn key(mut self, key: impl Into<String>) -> Self {
        self.key = key.into();
        self
    }

    pub(super) fn spoken(mut self, spoken: impl Into<String>) -> Self {
        self.spoken = Some(spoken.into());
        self
    }

    pub(super) fn no_icon(mut self) -> Self {
        self.icon = None;
        self
    }

    pub(super) fn count(mut self, count: Option<usize>) -> Self {
        self.count = count;
        self
    }

    pub(super) fn depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// The row has rows inside it, `open` or not, and a triangle that says so.
    pub(super) fn group(mut self, open: bool, toggle: impl Into<String>) -> Self {
        self.group = true;
        self.open = open;
        self.toggle = Some(toggle.into());
        self
    }

    pub(super) fn strong(mut self) -> Self {
        self.strong = true;
        self
    }

    pub(super) fn note(mut self, note: Option<String>) -> Self {
        self.note = note;
        self
    }

    pub(super) fn detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    pub(super) fn mark(mut self, mark: Option<egui::Color32>) -> Self {
        self.mark = mark;
        self
    }

    pub(super) fn tip(mut self, tip: impl Into<String>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    pub(super) fn unregistered(mut self) -> Self {
        self.register = false;
        self
    }

    fn filler(key: &str, height: f32) -> Self {
        let mut row = SideRow::new(key, "folder", "").no_icon().unregistered();
        row.fill = Some(height.max(1.0).round() as u32);
        row
    }
}

/// The rows a sidebar list shows, as the framework's item source.
struct SideRows {
    rows: Vec<SideRow>,
    index: HashMap<String, usize>,
    stamp: u64,
}

impl SideRows {
    fn new(rows: Vec<SideRow>) -> Self {
        let index = rows.iter().enumerate().map(|(at, row)| (row.key.clone(), at)).collect();
        let mut hasher = std::hash::DefaultHasher::new();
        rows.hash(&mut hasher);
        SideRows { rows, index, stamp: hasher.finish() }
    }
}

impl DisplayOrder<String> for SideRows {
    fn len(&self) -> usize {
        self.rows.len()
    }
    fn key(&self, at: usize) -> String {
        self.rows[at].key.clone()
    }
    fn index_of(&self, key: &String) -> Option<usize> {
        self.index.get(key).copied()
    }
    fn depth(&self, at: usize) -> usize {
        self.rows[at].depth
    }
    fn has_children(&self, at: usize) -> bool {
        self.rows[at].group
    }
    fn open(&self, at: usize) -> bool {
        self.rows[at].open
    }
    fn enabled(&self, at: usize) -> bool {
        self.rows[at].fill.is_none()
    }
    fn text(&self, at: usize) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(&self.rows[at].label))
    }
    fn revision(&self) -> Option<u64> {
        Some(self.stamp)
    }
}

impl ItemSource<String> for SideRows {
    fn size_hint(&self, at: usize) -> SizeHint {
        match self.rows[at].fill {
            Some(height) => SizeHint::Height(height as f32),
            None => SizeHint::Auto,
        }
    }
}

/// How a sidebar list is shown.
pub(super) struct SideSpec<'a> {
    /// Keeps the list's id apart from the others.
    pub salt: &'a str,
    /// The list's accessible name.
    pub name: &'a str,
    /// The key of the row to show as the chosen one.
    pub selected: Option<&'a str>,
    pub show_counts: bool,
    /// The rows nest: indented by depth, with triangles for the groups.
    pub tree: bool,
    /// Build every row, whatever is in view (a short list that is never windowed).
    pub build_all: bool,
}

impl<'a> SideSpec<'a> {
    pub(super) fn new(salt: &'a str, name: &'a str, show_counts: bool) -> Self {
        SideSpec { salt, name, selected: None, show_counts, tree: true, build_all: false }
    }

    pub(super) fn selected(mut self, key: Option<&'a str>) -> Self {
        self.selected = key;
        self
    }

    pub(super) fn flat(mut self) -> Self {
        self.tree = false;
        self
    }

    pub(super) fn build_all(mut self) -> Self {
        self.build_all = true;
        self
    }
}

/// A row the list showed: where, and where its parts are.
#[derive(Clone, Debug)]
pub(super) struct SideShown {
    pub key: String,
    pub rect: Rect,
    pub label: Option<Rect>,
    pub count: Option<Rect>,
}

/// What a sidebar list reports.
pub(super) enum SideEvent {
    /// A click or Enter on a row.
    Activate(String),
    /// A triangle (or a key) opened or closed a group.
    Open { key: String, open: bool },
    /// The row asked for its menu, at the pointer.
    Menu { key: String, at: Pos2 },
}

#[derive(Default)]
pub(super) struct SideOut {
    pub shown: Vec<SideShown>,
    pub events: Vec<SideEvent>,
    /// The list's own response, which a menu hangs from.
    pub response: Option<egui::Response>,
}

impl SideOut {
    /// The rect of the row `key`, if it was built this frame.
    pub(super) fn rect_of(&self, key: &str) -> Option<Rect> {
        self.shown.iter().find(|row| row.key == key).map(|row| row.rect)
    }
}

/// A sidebar list on the framework's collection: its rows, their triangles, the click that chooses
/// one and the request for a row's menu come from the list; the control-channel names of the rows
/// and what they hold are listed from where the list put them.
pub(super) fn side_list(ui: &mut egui::Ui, spec: SideSpec<'_>, rows: Vec<SideRow>) -> SideOut {
    if craft::ui::runtime::measure::active(ui.ctx()) && !ui.is_sizing_pass() {
        // A context-only measure supplies a supported non-publishing sizing UI,
        // rather than registering live collection input in the measured pass.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
        side_list_in(&mut child, spec, rows)
    } else {
        side_list_in(ui, spec, rows)
    }
}

/// The rows to build for a list of `total` that starts at screen `top` with rows `height` tall: the
/// ones in view and a row below, plus `reveal`'s neighbourhood.
fn window(ui: &egui::Ui, total: usize, top: f32, height: f32, reveal: Option<usize>) -> std::ops::Range<usize> {
    let view = ui.clip_rect();
    let first = ((view.top() - top) / height).floor().max(0.0) as usize;
    let last = ((view.bottom() - top) / height).ceil().max(0.0) as usize;
    let mut from = first.min(total);
    let mut to = (last + WINDOW_MARGIN).min(total);
    if let Some(at) = reveal.filter(|at| *at < total) {
        from = from.min(at.saturating_sub(2));
        to = to.max((at + 3).min(total));
    }
    from..to.max(from)
}

/// The rows a list builds: all of them, or (for a long list) the ones in view with a filler as tall
/// as the rest standing in for the others. The rows left out keep their `source:` names for the
/// control channel, at where they would be.
fn built_rows(ui: &egui::Ui, rows: Vec<SideRow>, salt: &str, want: Option<&String>) -> Vec<SideRow> {
    let total = rows.len();
    let height_id = egui::Id::unique(("side-row-height", salt.to_owned()));
    let height = ui.data(|d| d.get_temp::<f32>(height_id)).unwrap_or(ROW_HEIGHT).max(1.0);
    let top = ui.cursor().top();
    let reveal = want.and_then(|want| rows.iter().position(|row| row.id.to_lowercase() == *want));
    let built = window(ui, total, top, height, reveal);
    let mut shown = Vec::with_capacity(built.len() + 2);
    if built.start > 0 {
        shown.push(SideRow::filler("\u{0}above", built.start as f32 * height));
    }
    shown.extend(rows[built.clone()].iter().cloned());
    if built.end < total {
        shown.push(SideRow::filler("\u{0}below", (total - built.end) as f32 * height));
    }
    let (left, width) = (ui.cursor().left(), ui.available_width());
    for (at, row) in rows.iter().enumerate().filter(|(at, row)| !built.contains(at) && row.register) {
        register(ui, format!("source:{}", row.id), Rect::from_min_size(pos2(left, top + at as f32 * height), vec2(width, height)));
    }
    shown
}

/// The list row of `item`: its icon, label, count and the rest, in the columns `cells`.
fn list_row(item: &SideRow, cells: &[(ColumnKey, Rect)], tree: bool, show_counts: bool) -> ListRow<'static> {
    let mut row = ListRow::new(item.label.clone()).cells(cells);
    if let Some(icon) = item.icon {
        row = row.icon(IconSpec::svg(IconId::new(icon)));
    }
    if tree {
        row = row.depth(item.depth);
    }
    if item.strong {
        row = row.strong(true);
    }
    if let Some(detail) = &item.detail {
        row = row.detail(detail.clone());
    }
    let name = item.spoken.clone().unwrap_or_else(|| item.label.clone());
    row = match item.count {
        Some(n) => row.accessible_name(crate::i18n::tr_format!("{label}, {n} photos", label = name, n = n)),
        None => row.accessible_name(name),
    };
    if item.group {
        row = row.group(item.open);
    }
    if let Some(note) = &item.note {
        row = row.cell(COUNT, note.clone());
    } else if show_counts && let Some(n) = item.count {
        row = row.cell(COUNT, n.to_string());
        if n == 0 {
            // a count of nothing is there, but fainter: the rows that hold photos stand out
            row = row.cell_muted(COUNT);
        }
    }
    row
}

fn side_list_in(ui: &mut egui::Ui, spec: SideSpec<'_>, rows: Vec<SideRow>) -> SideOut {
    if rows.is_empty() {
        return SideOut::default();
    }
    let SideSpec { salt, name, selected, show_counts, tree, build_all } = spec;
    let want = scroll_target(ui);
    let rows = if !build_all && rows.len() > WINDOW_FROM { built_rows(ui, rows, salt, want.as_ref()) } else { rows };
    let has_notes = rows.iter().any(|row| row.note.is_some());
    let source = SideRows::new(rows);
    const DISCLOSURE: PartKey = PartKey::new("disclosure");
    let mut columns = vec![ColumnSpec::new(NAME, "", ColumnWidth::fill(1.0))];
    if show_counts || has_notes {
        columns.push(ColumnSpec::new(COUNT, "", ColumnWidth::Fixed(40.0)));
    }
    // The collection's lead is transient navigation, not the sidebar's choice: only its real
    // Activate event runs the row's command.
    let mut chosen = selected.map(|key| Selection::single(key.to_owned())).unwrap_or_default();
    let mut list = List::<String>::new(name.to_owned())
        .id_salt(("left-list", salt))
        .header(false)
        .natural_height(true)
        .select(SelectMode::Single)
        .activate(ActivateOn::Click)
        .selection(Bind::bound(&mut chosen))
        .columns(columns)
        .enabled(ui.is_enabled());
    if tree {
        list = list.tree(true).tree_leaves(TreeLeaves::Aligned);
    }
    let out = list.show_items(ui, &source, &mut |key, icx| {
        let item = &source.rows[source.index[key]];
        if item.fill.is_some() {
            return ListRow::spacer();
        }
        if item.group {
            // Public ItemCx/CollectionResponse opener API returns the real
            // declared disclosure-part response; no invented hit geometry.
            icx.part_opener(DISCLOSURE);
        }
        if let Some(tip) = &item.tip {
            let tip = tip.clone();
            icx.tip(move |ui| {
                ui.label(tip);
            });
        }
        list_row(item, icx.cells(), tree, show_counts)
    });
    let live = !measuring(ui) && ui.is_enabled() && ui.is_visible() && out.response.enabled();
    if !live {
        return SideOut::default();
    }
    if let Some(first) = out.shown.iter().find(|row| source.rows[source.index[&row.key]].fill.is_none()) {
        let height_id = egui::Id::unique(("side-row-height", salt.to_owned()));
        ui.data_mut(|d| d.insert_temp(height_id, first.rect.height()));
    }
    let shown = list_shown(ui, &out, &source, selected, want.as_ref());
    SideOut { shown, events: list_events(&out, &source), response: Some(out.response.clone()) }
}

/// The rows the list showed, listed under the names the control channel knows them by (and the
/// colour label's dot painted); a row the sidebar was asked to scroll to is scrolled to.
fn list_shown<E>(ui: &mut egui::Ui, out: &CollectionResponse<String, E>, source: &SideRows, selected: Option<&str>, want: Option<&String>) -> Vec<SideShown> {
    let mut shown = Vec::new();
    for row in &out.shown {
        let item = &source.rows[source.index[&row.key]];
        if item.fill.is_some() {
            continue;
        }
        let label = row.cells.iter().find(|(column, _)| *column == NAME).map(|(_, rect)| *rect);
        let count = row.cells.iter().find(|(column, _)| *column == COUNT).map(|(_, rect)| *rect);
        if item.register {
            register_row(ui, item, row.rect, (label, count), selected == Some(item.key.as_str()));
        }
        if let Some(toggle) = &item.toggle {
            for (_, response) in &row.openers {
                crate::widgets::register_response(toggle.clone(), response.rect, response);
            }
        }
        if want.is_some_and(|want| item.id.to_lowercase() == *want) {
            ui.scroll_to_rect_animation(row.rect, None, egui::style::ScrollAnimation::none());
        }
        shown.push(SideShown { key: row.key.clone(), rect: row.rect, label, count });
    }
    shown
}

/// Lists a row under `source:<id>`, `highlight:<id>` (the chosen one), `label:<id>` and
/// `count:<id>`, and paints its colour label's dot.
fn register_row(ui: &egui::Ui, item: &SideRow, rect: Rect, (label, count): (Option<Rect>, Option<Rect>), chosen: bool) {
    let id = &item.id;
    register(ui, format!("source:{id}"), rect);
    if chosen {
        register(ui, format!("highlight:{id}"), rect);
    }
    if let Some(label) = label {
        register(ui, format!("label:{id}"), label);
    }
    if item.count.is_some()
        && let Some(count) = count
    {
        register(ui, format!("count:{id}"), count);
    }
    if let Some(color) = item.mark {
        // a label's dot sits just before the count, or at the row's end without one
        let x = count.map_or(rect.right() - 18.0, |count| count.left() - 6.0);
        let dot = Rect::from_center_size(pos2(x, rect.center().y), vec2(8.0, 8.0));
        ui.painter().circle_filled(dot.center(), 4.0, color);
        register(ui, format!("labelMark:{id}"), dot);
    }
}

/// What the list reported that the sidebar acts on: a click, a triangle, a request for a menu.
fn list_events<E>(out: &CollectionResponse<String, E>, source: &SideRows) -> Vec<SideEvent> {
    let known = |key: &String| source.index.get(key).is_some_and(|at| source.rows[*at].fill.is_none());
    let mut events = Vec::new();
    for event in &out.events {
        match event {
            CollectionEvent::Open(change) if known(&change.key) && source.rows[source.index[&change.key]].group => {
                events.push(SideEvent::Open { key: change.key.clone(), open: change.open });
            }
            CollectionEvent::Activate(key) if known(key) => events.push(SideEvent::Activate(key.clone())),
            CollectionEvent::ContextMenu(request) if known(&request.key) => {
                events.push(SideEvent::Menu { key: request.key.clone(), at: request.at });
            }
            _ => {}
        }
    }
    events
}

// ------------------------------------------------------------------------------ drags

/// The key of the row a drag of the primary button has just started on, from where the press began
/// and the rows' places: once for each press (`pinned`: the rows are the pinned header's). Rows are the sidebar's own, so a press that began
/// elsewhere (the photo grid) is none of this.
pub(super) fn drag_started_on(ui: &egui::Ui, rows: &[SideShown], pinned: bool) -> Option<String> {
    let (down, dragging, origin, since) =
        ui.input(|i| (i.pointer.primary_down(), i.pointer.is_decidedly_dragging(), i.pointer.press_origin(), i.pointer.press_start_time()));
    if !(down && dragging) {
        return None;
    }
    let (origin, since) = (origin?, since?);
    // a press on the header pinned over the rows is the header's, not the rows beneath it
    let over_pinned = ui.data(|d| d.get_temp::<Option<Rect>>(egui::Id::unique("left-pinned-rect"))).flatten().is_some_and(|r| r.contains(origin));
    if over_pinned && !pinned {
        return None;
    }
    let latch = egui::Id::unique("left-drag-latch");
    if ui.data(|d| d.get_temp::<f64>(latch)) == Some(since) {
        return None;
    }
    let hit = rows.iter().find(|row| row.rect.contains(origin))?;
    ui.data_mut(|d| d.insert_temp(latch, since));
    Some(hit.key.clone())
}

// ------------------------------------------------------------------------------ menus

/// Where a row's menu hangs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum MenuAt {
    Point(Pos2),
    Rect(Rect),
}

/// The menu a sidebar row asked for: remembered from the request until the menu closes, and shown
/// each frame meanwhile with `body` (given the row's key). `slot` tells this list's menu from the
/// other lists'; `response` is the list's.
pub(super) fn host_menu(
    ui: &mut egui::Ui,
    slot: &str,
    name: &str,
    response: Option<&egui::Response>,
    request: Option<(String, MenuAt)>,
    mut body: impl FnMut(&mut egui::Ui, &str),
) {
    if measuring(ui) {
        return;
    }
    let stored = egui::Id::unique(("left-menu-target", slot.to_owned()));
    // (a menu is not opened while photos are being carried over it)
    if let Some(request) = request.filter(|_| ui.is_enabled() && !crate::widgets::carrying(ui.ctx())) {
        ui.data_mut(|d| d.insert_temp(stored, request));
    }
    let Some((key, at)) = ui.data(|d| d.get_temp::<(String, MenuAt)>(stored)) else { return };
    let Some(response) = response else {
        ui.data_mut(|d| d.remove::<(String, MenuAt)>(stored));
        return;
    };
    let anchor = match at {
        MenuAt::Point(point) => Anchor::Point(point),
        MenuAt::Rect(rect) => Anchor::Rect(rect),
    };
    let mut open = true;
    let shown = Menu::new(name.to_owned())
        .id_salt(("left-menu", slot))
        .opened_by(response, Trigger::Manual)
        .anchor(anchor)
        .open(Bind::bound(&mut open))
        .show(ui, |ui, _| body(ui, &key));
    if shown.output.closed.is_some() || !open {
        ui.data_mut(|d| d.remove::<(String, MenuAt)>(stored));
    }
}

/// A row of a menu: build it with [`entry`] and the setters, then [`Entry::show`] it. It says
/// whether it was run, which is never while the menu is only being measured.
pub(super) struct Entry<'a> {
    salt: &'a str,
    label: &'a str,
    enabled: bool,
    widget: Option<String>,
    tip: Option<&'a str>,
    checked: Option<bool>,
}

pub(super) fn entry<'a>(salt: &'a str, label: &'a str) -> Entry<'a> {
    Entry { salt, label, enabled: true, widget: None, tip: None, checked: None }
}

impl<'a> Entry<'a> {
    pub(super) fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    /// The row is widget `name` for the control channel.
    pub(super) fn widget(mut self, name: impl Into<String>) -> Self {
        self.widget = Some(name.into());
        self
    }

    pub(super) fn tip(mut self, tip: &'a str) -> Self {
        self.tip = Some(tip);
        self
    }

    /// The row carries a check mark while `on`.
    pub(super) fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }

    pub(super) fn show(self, ui: &mut egui::Ui) -> bool {
        let live = !measuring(ui);
        let mut item = MenuItem::new(self.label.to_owned()).id_salt(("left-menu-item", self.salt, self.label)).enabled(self.enabled);
        if let Some(on) = self.checked {
            item = item.check(on);
        }
        if let Some(tip) = self.tip {
            item = item.tooltip(tip.to_owned());
        }
        let shown = item.show(ui);
        if live && let Some(widget) = self.widget {
            crate::widgets::register_response(widget, shown.response.rect, &shown.response);
        }
        live && self.enabled && shown.ran
    }
}

/// A rule between groups of a menu's rows.
pub(super) fn rule(ui: &mut egui::Ui) {
    Divider::horizontal().show(ui);
}

/// A row that opens a submenu with the rows `body` shows (`enabled`: it has some to run); `widget`
/// names it for the control channel.
pub(super) fn submenu(
    ui: &mut egui::Ui,
    label: &str,
    widget: Option<&str>,
    enabled: bool,
    body: impl FnMut(&mut egui::Ui, &mut craft::ui::contract::BodyCx<'_>),
) {
    let head = Menu::submenu_row_enabled(ui, label.to_owned(), enabled, body);
    if !measuring(ui)
        && let Some(widget) = widget
    {
        crate::widgets::register_response(widget.to_owned(), head.response.rect, &head.response);
    }
}
