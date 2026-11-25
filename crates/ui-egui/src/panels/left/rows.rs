use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};

use craft::behavior::{ColumnKey, ColumnWidth, DisplayOrder, IconId, ItemSource, PartKey, Selection, SizeHint};
use craft::ui::contract::overlay::Anchor;
use craft::ui::contract::{
    ActivateOn, Bind, CollectionEvent, CollectionResponse, ColumnAlign, ColumnSpec, ListDrag, Props as _, SelectMode, Show as _, ShowCollection as _,
    ShowContainer as _,
};
use craft::ui::display::Divider;
use craft::ui::list::{List, ListRow, TreeLeaves};
use craft::ui::menu::{Menu, MenuItem};
use craft::ui::primitives::IconSpec;
use craft::ui::style::TypeRole;
use egui::{Pos2, Rect};

use super::{SidebarSection, Tokens, pos2, register, vec2};

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
/// The room between the name and the count, where a colour label's dot sits.
const GAP: ColumnKey = ColumnKey::new("side-gap");

/// Width of [`GAP`] without and with a colour label's dot.
const GAP_PLAIN: f32 = 4.0;
const GAP_MARK: f32 = 16.0;

/// How far one level of a tree steps in, as far as the sidebar plans its width: the deepest row
/// asks for this many points a level, and the sidebar scrolls sideways when that exceeds the panel.
const LEVEL_WIDTH: f32 = 16.0;

/// What a row has before its name: the triangle's column and the icon.
const LABEL_LEAD: f32 = 42.0;

/// What a row needs besides its levels: the icon and triangle, a name's share, the count.
const ROW_ROOM: f32 = 42.0 + 140.0 + 40.0;

/// The room a count keeps at the right of the visible edge it is pinned to.
const COUNT_EDGE_ROOM: f32 = 18.0;

/// The least of its name a row shows before its count gives way (about three letters and an
/// ellipsis), and the room between the name and the count's slot.
const MIN_COUNT_NAME_ROOM: f32 = 36.0;
const NAME_GAP: f32 = 8.0;

/// The room a count's cell keeps around its text: the framework insets a cell's text at each side
/// (`control.pad_x`), and a cell narrower than its text would cut a digit off.
const COUNT_CELL_PAD: f32 = 14.0;

/// How wide `text` lays out in `font`: how a row's name and its count are measured.
fn text_width(ctx: &egui::Context, text: &str, font: egui::FontId) -> f32 {
    ctx.fonts_mut(|fonts| fonts.layout(text.to_owned(), font, egui::Color32::PLACEHOLDER, f32::INFINITY).size().x)
}

/// Where a row's name starts, from the row's start: the triangle's column and the icon, then one
/// step for each level.
fn labels_start(item: &SideRow) -> f32 {
    LABEL_LEAD + item.depth as f32 * LEVEL_WIDTH
}

/// How wide the sidebar's content needs to be, from the deepest row of the last frame: the panel's
/// width, or more when the levels leave too little room for the names. One frame behind, which
/// needs no second layout pass. 0 until the first frame has been drawn.
pub(super) fn content_width(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(egui::Id::unique("left-content-width"))).unwrap_or(0.0)
}

/// A row `depth` levels deep asks the sidebar for its width.
fn note_depth(ui: &egui::Ui, depth: usize) {
    let need = depth as f32 * LEVEL_WIDTH + ROW_ROOM;
    ui.data_mut(|d| {
        let m = d.get_temp_mut_or_insert_with::<f32>(egui::Id::unique("left-content-need"), || 0.0);
        *m = m.max(need);
    });
}

/// Called once a frame after the lists: the width the deepest row asked for becomes next frame's.
/// Returns whether it changed.
pub(super) fn settle_width(ctx: &egui::Context, panel: f32) -> bool {
    let need = ctx.data_mut(|d| d.remove_temp::<f32>(egui::Id::unique("left-content-need"))).unwrap_or(0.0);
    let wide = need.max(panel);
    let changed = (wide - content_width(ctx)).abs() > 0.5;
    if changed {
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("left-content-width"), wide));
        ctx.request_repaint();
    }
    changed
}

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
    /// The type role of the label, when it is not the list's own (the My Photos title).
    pub role: Option<TypeRole>,
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
            role: None,
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

    pub(super) fn role(mut self, role: TypeRole) -> Self {
        self.role = Some(role);
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

}

/// What a row's count column shows: whether photo counts are on, and whether the list has a count
/// column at all (counts shown, or a row with a note of its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Counts {
    shown: bool,
    column: bool,
}

impl Counts {
    /// The text a row puts in its count column: its note, or its count of photos.
    fn text(self, item: &SideRow) -> Option<String> {
        item.note.clone().or_else(|| item.count.filter(|_| self.shown).map(|n| n.to_string()))
    }
}

/// A row's cells and what it shows in them, with its count pinned at the sidebar's visible right
/// edge as the old sidebar drew it: the counts keep one column at the edge while the content
/// scrolls sideways under it, and the name is cut just before it, so a count never covers a name.
/// A row too deep to hold both keeps its count (and its colour label's dot) wholly past the edge,
/// where a sideways scroll reaches it, rather than being cut in two by the edge. A list that fits
/// the panel needs no pinning: its rows end at the edge, and the count's own column already cuts
/// the name before it.
struct Pinned {
    /// The row's cells, with [`COUNT`] and [`GAP`] moved to where the pin keeps them.
    cells: Vec<(ColumnKey, Rect)>,
    /// The name as drawn: cut with an ellipsis where it does not fit before the count.
    label: String,
    /// Whether the name had to be cut.
    cut: bool,
    /// The name as far as it is drawn, from the row's start.
    label_rect: Rect,
    /// The count's text where it sits (only for a row that shows one).
    count: Option<Rect>,
    /// A colour label's dot where it sits.
    dot: Option<Rect>,
}

impl Pinned {
    /// The row `item` in `rect` with the cells `cells` the list laid out for it, and what its
    /// count column shows (`counts`).
    fn of(ctx: &egui::Context, rect: Rect, cells: &[(ColumnKey, Rect)], item: &SideRow, counts: Counts) -> Self {
        let font = Tokens::get(ctx).font(13.5);
        let measure = |text: &str| text_width(ctx, text, font.clone());
        let lead = labels_start(item);
        let count_text = counts.text(item);
        let name_at = cells.iter().position(|(column, _)| *column == NAME);
        let count_at = cells.iter().position(|(column, _)| *column == COUNT);
        let gap_at = cells.iter().position(|(column, _)| *column == GAP);
        let Some(name_cell) = name_at.and_then(|at| cells.get(at)).map(|(_, rect)| *rect) else {
            // a list without columns: the row is all label
            return Pinned { cells: cells.to_vec(), label: item.label.clone(), cut: false, label_rect: rect, count: None, dot: None };
        };
        let name_w = measure(&item.label);
        let mark_w = if item.mark.is_some() { GAP_MARK } else { 0.0 };
        let label_left = name_cell.left() + lead;
        let visible = super::sections::visible_right_of(ctx);
        // The count's text starts here: `COUNT_EDGE_ROOM` before the visible edge while the row
        // reaches past it, and right after the name instead when there is no room for both.
        let pinned = (counts.column && rect.right() > visible + 0.5).then(|| {
            let text = count_text.as_deref().map_or(0.0, &measure);
            let after_name = label_left + name_w.min(MIN_COUNT_NAME_ROOM) + NAME_GAP + mark_w + text + COUNT_EDGE_ROOM;
            if after_name > visible && (count_text.is_some() || mark_w > 0.0) {
                // The count gives way: wholly past the edge, with the name's least room before it.
                (after_name - COUNT_EDGE_ROOM - text).max(visible + 4.0 + mark_w)
            } else {
                visible - COUNT_EDGE_ROOM - text
            }
        });
        let mut cells = cells.to_vec();
        let mut label = item.label.clone();
        let mut cut = false;
        let mut drawn = name_w;
        if let Some(count_left) = pinned {
            let room = (count_left - mark_w - NAME_GAP - label_left).max(0.0);
            if name_w > room {
                label = crate::widgets::elide_head(&item.label, room, &measure);
                cut = true;
            }
            drawn = measure(&label).min(room);
            // (the name's column stays as the list laid it out: its room decides the indent the
            // row draws, and the name is already cut where the count's slot begins)
            // a colour label's dot sits in its slot before the count
            if let Some(at) = gap_at {
                let width = cells[at].1.width();
                cells[at].1 = Rect::from_center_size(pos2(count_left - mark_w / 2.0, rect.center().y), vec2(width, rect.height()));
            }
            // the count's cell is as wide as its text and the room its text keeps at each side
            if let (Some(at), Some(text)) = (count_at, count_text.as_deref()) {
                let width = measure(text);
                let (left, right) = (count_left - COUNT_CELL_PAD, count_left + width + COUNT_CELL_PAD);
                cells[at].1 = Rect::from_min_max(pos2(left, rect.top()), pos2(right, rect.bottom()));
            }
        }
        let count = count_at.and_then(|at| match pinned {
            Some(count_left) => {
                let width = count_text.as_deref().map_or(0.0, &measure);
                count_text.as_ref().map(|_| Rect::from_x_y_ranges(count_left..=(count_left + width), rect.y_range()))
            }
            None => Some(cells[at].1),
        });
        let dot = item.mark.map(|_| {
            let x = pinned.map_or_else(
                || gap_at.map_or_else(|| count.map_or(rect.right() - 18.0, |count| count.left() - 6.0), |at| cells[at].1.center().x),
                |count_left| count_left - mark_w / 2.0,
            );
            Rect::from_center_size(pos2(x, rect.center().y), vec2(8.0, 8.0))
        });
        let label_rect = Rect::from_min_size(name_cell.min, vec2((lead + drawn).min(name_cell.width()), name_cell.height()));
        Pinned { cells, label, cut, label_rect, count, dot }
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

/// What dragging a section's header carries through the framework's drag session.
#[derive(Clone, Copy, Debug)]
pub(super) struct SectionDrag {
    pub section: SidebarSection,
    /// The drag began on the header pinned at the top of the sidebar.
    pub pinned: bool,
}

/// What dragging an album's (or folder's) row carries: its id.
#[derive(Clone, Copy, Debug)]
pub(super) struct AlbumDrag(pub u64);

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
    /// A press on a row puts the keyboard focus on the list. A header that is dragged or given up
    /// with Escape keeps the focus where it was, so the Escape reaches the sidebar.
    pub focus_on_press: bool,
    /// Dragging the list's row drags this section (and says it began on the pinned header or not).
    pub section_drag: Option<(SidebarSection, bool)>,
    /// Dragging a row drags the album or folder whose id ends its key.
    pub album_drag: bool,
}

impl<'a> SideSpec<'a> {
    pub(super) fn new(salt: &'a str, name: &'a str, show_counts: bool) -> Self {
        SideSpec { salt, name, selected: None, show_counts, tree: true, build_all: false, focus_on_press: true, section_drag: None, album_drag: false }
    }

    pub(super) fn selected(mut self, key: Option<&'a str>) -> Self {
        self.selected = key;
        self
    }

    /// The row drags `section`: the drag is the framework's, so Escape and a lost press end it.
    pub(super) fn section_drag(mut self, section: SidebarSection, pinned: bool) -> Self {
        self.section_drag = Some((section, pinned));
        self
    }

    /// The rows drag albums and folders.
    pub(super) fn album_drag(mut self) -> Self {
        self.album_drag = true;
        self
    }

    /// A press on a row leaves the keyboard focus where it was.
    pub(super) fn keep_focus(mut self) -> Self {
        self.focus_on_press = false;
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
fn built_rows(ui: &egui::Ui, rows: Vec<SideRow>, salt: &str, want: Option<&String>) -> (Vec<SideRow>, f32, f32) {
    let total = rows.len();
    let height_id = egui::Id::unique(("side-row-height", salt.to_owned()));
    let height = ui.data(|d| d.get_temp::<f32>(height_id)).unwrap_or(ROW_HEIGHT).max(1.0);
    let top = ui.cursor().top();
    let reveal = want.and_then(|want| rows.iter().position(|row| row.id.to_lowercase() == *want));
    let built = window(ui, total, top, height, reveal);
    let shown: Vec<SideRow> = rows[built.clone()].to_vec();
    // The rows left out are plain space above and below the list, not rows of it: a list that measured their whole
    // height would pass the framework's size guard (32,768 points) with a few thousand keywords and stop showing.
    let (above, below) = (built.start as f32 * height, (total - built.end) as f32 * height);
    let (left, width) = (ui.cursor().left(), ui.available_width());
    for (at, row) in rows.iter().enumerate().filter(|(at, row)| !built.contains(at) && row.register) {
        register(ui, format!("source:{}", row.id), Rect::from_min_size(pos2(left, top + at as f32 * height), vec2(width, height)));
    }
    (shown, above, below)
}

/// The list row of `item`: its icon, label, count and the rest, in the columns `pinned` places
/// (the name already cut where the pin keeps the count).
fn list_row(item: &SideRow, pinned: &Pinned, tree: bool, show_counts: bool) -> ListRow<'static> {
    let mut row = ListRow::new(pinned.label.clone()).cells(&pinned.cells);
    if let Some(icon) = item.icon {
        row = row.icon(IconSpec::svg(IconId::new(icon)));
    }
    if tree {
        row = row.depth(item.depth);
    }
    if item.strong {
        row = row.strong(true).role(item.role.unwrap_or(TypeRole::BodyStrong));
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
        // counts read quieter than the names
        row = row.cell(COUNT, n.to_string()).cell_muted(COUNT);
    }
    row
}

fn side_list_in(ui: &mut egui::Ui, spec: SideSpec<'_>, rows: Vec<SideRow>) -> SideOut {
    if rows.is_empty() {
        return SideOut::default();
    }
    let SideSpec { salt, name, selected, show_counts, tree, build_all, focus_on_press, section_drag, album_drag } = spec;
    let want = scroll_target(ui);
    let (rows, above, below) = if !build_all && rows.len() > WINDOW_FROM { built_rows(ui, rows, salt, want.as_ref()) } else { (rows, 0.0, 0.0) };
    let has_notes = rows.iter().any(|row| row.note.is_some());
    let has_marks = rows.iter().any(|row| row.mark.is_some());
    if tree && let Some(deepest) = rows.iter().map(|row| row.depth).max() {
        note_depth(ui, deepest);
    }
    let source = SideRows::new(rows);
    const DISCLOSURE: PartKey = PartKey::new("disclosure");
    let mut columns = vec![ColumnSpec::new(NAME, "", ColumnWidth::fill(1.0))];
    // (the pin moves both cells row by row: the column's own rect only decides what a row that
    // fits the panel shows, and right-aligned counts keep one end where the pin holds them)
    let counts = Counts { shown: show_counts, column: show_counts || has_notes };
    if counts.column {
        columns.push(ColumnSpec::new(GAP, "", ColumnWidth::Fixed(if has_marks { GAP_MARK } else { GAP_PLAIN })));
        columns.push(ColumnSpec::new(COUNT, "", ColumnWidth::Fixed(40.0)).align(ColumnAlign::End));
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
        .focus_on_press(focus_on_press)
        .enabled(ui.is_enabled());
    if tree {
        list = list.tree(true).tree_leaves(TreeLeaves::Aligned);
    }
    if let Some((section, pinned)) = section_drag {
        list = list.drag_payload(move |_drag: &ListDrag<String>| Some(SectionDrag { section, pinned }));
    }
    if album_drag {
        list = list.drag_payload(|drag: &ListDrag<String>| drag.grabbed.rsplit(':').next().and_then(|id| id.parse::<u64>().ok()).map(AlbumDrag));
    }
    if above > 0.0 {
        ui.add_space(above);
    }
    let ctx = ui.ctx().clone();
    let out = list.show_items(ui, &source, &mut |key, icx| {
        let item = &source.rows[source.index[key]];
        if item.fill.is_some() {
            return ListRow::spacer();
        }
        let pinned = Pinned::of(&ctx, icx.rect, icx.cells(), item, counts);
        if item.group {
            // Public ItemCx/CollectionResponse opener API returns the real
            // declared disclosure-part response; no invented hit geometry.
            icx.part_opener(DISCLOSURE);
        }
        if pinned.cut || item.tip.is_some() {
            // the whole name where the pin cut it short, then what resting on the row says
            let (full, cut, tip) = (item.label.clone(), pinned.cut, item.tip.clone());
            icx.tip(move |ui| {
                if cut {
                    ui.label(full);
                }
                if let Some(tip) = &tip {
                    ui.label(tip.as_str());
                }
            });
        }
        list_row(item, &pinned, tree, show_counts)
    });
    if below > 0.0 {
        ui.add_space(below);
    }
    let live = !measuring(ui) && ui.is_enabled() && ui.is_visible() && out.response.enabled();
    if !live {
        return SideOut::default();
    }
    if let Some(first) = out.shown.iter().find(|row| source.rows[source.index[&row.key]].fill.is_none()) {
        let height_id = egui::Id::unique(("side-row-height", salt.to_owned()));
        ui.data_mut(|d| d.insert_temp(height_id, first.rect.height()));
    }
    let shown = list_shown(ui, &out, &source, selected, want.as_ref(), counts);
    let mut events = list_events(&out, &source);
    // the menu opens from a row's triangle too (the list reports a request for the row itself only)
    let secondary = ui.input(|i| if i.pointer.button_clicked(egui::PointerButton::Secondary) { i.pointer.interact_pos() } else { None });
    if let Some(at) = secondary {
        for row in out.shown.iter().filter(|row| row.openers.iter().any(|(_, response)| response.rect.contains(at))) {
            if !events.iter().any(|event| matches!(event, SideEvent::Menu { key, .. } if *key == row.key)) {
                events.push(SideEvent::Menu { key: row.key.clone(), at });
            }
        }
    }
    SideOut { shown, events, response: Some(out.response.clone()) }
}

/// The rows the list showed, listed under the names the control channel knows them by (and the
/// colour label's dot painted); a row the sidebar was asked to scroll to is scrolled to.
fn list_shown<E>(
    ui: &mut egui::Ui,
    out: &CollectionResponse<String, E>,
    source: &SideRows,
    selected: Option<&str>,
    want: Option<&String>,
    counts: Counts,
) -> Vec<SideShown> {
    let ctx = ui.ctx().clone();
    let mut shown = Vec::new();
    for row in &out.shown {
        let item = &source.rows[source.index[&row.key]];
        if item.fill.is_some() {
            continue;
        }
        // the row's cells as the item closure placed them: the same pin, from the same numbers
        let pinned = Pinned::of(&ctx, row.rect, &row.cells, item, counts);
        if item.register {
            register_row(ui, item, row.rect, &pinned, selected == Some(item.key.as_str()));
        }
        if let Some(toggle) = &item.toggle {
            for (_, response) in &row.openers {
                crate::widgets::register_response(toggle.clone(), response.rect, response);
            }
        }
        if want.is_some_and(|want| item.id.to_lowercase() == *want) {
            ui.scroll_to_rect_animation(row.rect, None, egui::style::ScrollAnimation::none());
        }
        shown.push(SideShown { key: row.key.clone(), rect: row.rect, label: Some(pinned.label_rect), count: pinned.count });
    }
    shown
}

/// Lists a row under `source:<id>`, `highlight:<id>` (the chosen one), `label:<id>` and
/// `count:<id>`, and paints its colour label's dot.
fn register_row(ui: &egui::Ui, item: &SideRow, rect: Rect, pinned: &Pinned, chosen: bool) {
    let id = &item.id;
    // what is visible of the row: a press on the centre of a row that reaches past the panel's edge
    // (the sidebar scrolls sideways) must land on the panel
    let visible = rect.intersect(ui.clip_rect());
    let visible = if visible.is_positive() { visible } else { rect };
    register(ui, format!("source:{id}"), visible);
    if chosen {
        register(ui, format!("highlight:{id}"), visible);
    }
    // the name as far as it is drawn, and the count where the pin keeps it
    register(ui, format!("label:{id}"), pinned.label_rect);
    if item.count.is_some()
        && let Some(count) = pinned.count
    {
        register(ui, format!("count:{id}"), count);
    }
    if let (Some(color), Some(dot)) = (item.mark, pinned.dot) {
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
    let shown = Menu::context(name.to_owned(), response)
        .id_salt(("left-menu", slot))
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
