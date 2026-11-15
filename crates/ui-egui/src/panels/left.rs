//! The left panel: the module's Classic column (Library's and Develop's, `classic_left.rs`), or
//! upstream's "My Photos" sidebar (library sources, albums tree, date groups), which Develop keeps
//! as one of its panels.

use egui::{Align2, Rect, Sense, pos2, vec2};
use lcc_catalog::{Album, AlbumId, FolderNode, KeywordNode};
use lcc_engine::LibrarySource;
use serde_json::json;

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::sidebar::SidebarSection;
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// Library's and Develop's Classic left columns (fork-owned).
#[path = "classic_left.rs"]
pub(crate) mod classic;

/// How wide the sidebar's content needs to be, from the widest row of the last frame: rows are
/// drawn at this width (or the panel's, if wider), and the sidebar scrolls sideways when it
/// exceeds the panel. One frame behind, which needs no second layout pass.
pub(crate) fn content_width(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(egui::Id::new("left-content-width"))).unwrap_or(0.0)
}

/// The most a row asks for its name: longer ones are cut when there is no more room.
const MAX_NAME_NEED: f32 = 140.0;

/// Height of a sidebar row.
const ROW_HEIGHT: f32 = 29.0;

/// Temp-data key of the row (its widget id after `source:`, lower-case) the sidebar scrolls to
/// this frame: set before the sections are drawn, cleared after.
const SCROLL_TO_ROW: &str = "left-scroll-to-row";

/// Temp-data key of the Folders choice whose rows above were last opened ([`reveal_chosen`]):
/// removed, they open again.
pub(super) const LIBFOLDER_REVEALED: &str = "libfolder-revealed";

/// A row says how wide it needs to be.
fn note_width(ui: &egui::Ui, w: f32) {
    ui.data_mut(|d| {
        let m = d.get_temp_mut_or_insert_with::<f32>(egui::Id::new("left-content-width-next"), || 0.0);
        *m = m.max(w);
    });
}

/// One level of a sidebar tree when the panel has room: callers pass `indent` in these steps.
pub(crate) const INDENT_STEP: f32 = 16.0;

/// The narrowest a level is drawn: the chevron still steps visibly right of its parent's.
pub(crate) const MIN_INDENT_STEP: f32 = 8.0;

/// What a row keeps for its name when the step is chosen: the deepest row's name is not cut to
/// less than this while a narrower step would still keep it.
const MIN_NAME_ROOM: f32 = 80.0;

/// Room kept for a count at the end of the deepest row (three digits and their gap).
const COUNT_ROOM: f32 = 40.0;

/// The step a level is drawn with this frame: [`INDENT_STEP`] while the deepest open row fits the
/// panel, narrower when it would not, never below [`MIN_INDENT_STEP`]. From the last frame.
pub(crate) fn indent_step(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(egui::Id::new("left-indent-step"))).unwrap_or(INDENT_STEP)
}

/// How far right a row at `indent` (in [`INDENT_STEP`]s) is drawn this frame.
fn indent_x(ctx: &egui::Context, indent: f32) -> f32 {
    indent / INDENT_STEP * indent_step(ctx)
}

/// [`indent_x`] for a row being laid out, which also notes how deep the tree goes, for next
/// frame's step.
fn drawn_indent(ui: &egui::Ui, indent: f32) -> f32 {
    ui.data_mut(|d| {
        let m = d.get_temp_mut_or_insert_with::<f32>(egui::Id::new("left-deepest-indent-next"), || 0.0);
        *m = m.max(indent);
    });
    indent_x(ui.ctx(), indent)
}

/// The widest step (16, 14, 12, 10 or 8 pt) at which a row `deepest` deep (in [`INDENT_STEP`]s)
/// keeps [`MIN_NAME_ROOM`] for its name and its count in a panel `panel_w` wide. In steps, so a
/// row does not slide sideways at every point the panel is dragged; when even the narrowest step
/// does not fit, the sidebar scrolls sideways as before.
pub(crate) fn step_for(panel_w: f32, deepest: f32) -> f32 {
    let levels = deepest / INDENT_STEP;
    if levels < 1.0 {
        return INDENT_STEP;
    }
    let room = panel_w - 42.0 - MIN_NAME_ROOM - 18.0 - COUNT_ROOM;
    [16.0, 14.0, 12.0, 10.0].into_iter().find(|step| levels * step <= room).unwrap_or(MIN_INDENT_STEP)
}

fn row(app: &mut LccApp, ui: &mut egui::Ui, id: &str, icon: Icon, label: &str, count: Option<usize>, selected: bool, indent: f32) -> egui::Response {
    row_named(app, ui, id, icon, label, None, count, selected, indent)
}

/// [`row`] whose spoken name is `spoken` when the painted `label` is a shortened form of it.
fn row_named(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    id: &str,
    icon: Icon,
    label: &str,
    spoken: Option<&str>,
    count: Option<usize>,
    selected: bool,
    indent: f32,
) -> egui::Response {
    row_sensed(app, ui, id, icon, label, spoken, count, selected, indent, Sense::click(), None)
}

/// [`row_named`] with the given `sense` (rows that can be dragged add `drag`) and a colour
/// label's dot (`mark`) before the count.
#[allow(clippy::too_many_arguments)]
fn row_sensed(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    id: &str,
    icon: Icon,
    label: &str,
    spoken: Option<&str>,
    count: Option<usize>,
    selected: bool,
    indent: f32,
    sense: Sense,
    mark: Option<egui::Color32>,
) -> egui::Response {
    let label = if matches!(id, "all" | "recentlyAdded" | "previousImport" | "quickCollection" | "picks" | "missing" | "recentlyDeleted") {
        crate::i18n::tr(label)
    } else {
        label
    };
    // a name is drawn on one line: one holding line breaks (a whole keyword list read as one
    // keyword) would spill over the rows around it
    let one_line = lcc_catalog::keywords::one_line(label);
    let label: &str = &one_line;
    let t = Tokens::get(ui.ctx());
    let indent = drawn_indent(ui, indent);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), sense);
    if scroll_asked(ui, id) {
        ui.scroll_to_rect_animation(r, None, egui::style::ScrollAnimation::none());
    }
    register(ui.ctx(), format!("source:{id}"), r);
    let mark_w = if mark.is_some() { MARK_W } else { 0.0 };
    let said = spoken.unwrap_or(label);
    let name = match count {
        Some(n) => crate::i18n::tr_format!("{label}, {n} photos", label = said, n = n),
        None => said.to_string(),
    };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &name));
    // what is visible of the row: when the sidebar is scrolled sideways its bars and counts end at
    // the panel's edge, not at the end of the (wider) content
    let visible_right: f32 = ui.data(|d| d.get_temp(egui::Id::new("left-visible-right"))).unwrap_or(f32::MAX);
    let edge = r.right().min(visible_right);
    let inner = Rect::from_min_max(r.min + vec2(8.0, 0.0), pos2((r.right() - 8.0).min(edge - 8.0).max(r.left() + 8.0), r.bottom()));
    if selected {
        ui.painter().rect_filled(inner, 4.0, t.canvas);
        register(ui.ctx(), format!("highlight:{id}"), inner);
    } else if resp.hovered() {
        ui.painter().rect_filled(inner, 4.0, t.hover.gamma_multiply(0.6));
    }
    paint(
        ui.painter(),
        Rect::from_min_size(pos2(r.left() + 18.0 + indent, r.center().y - 8.0), vec2(16.0, 16.0)),
        icon,
        if selected { t.text } else { t.icon },
    );
    let font = t.font(13.5);
    let color = if selected { t.text } else { t.text_label };
    // a count of nothing is there, but fainter: the rows that hold photos stand out
    let count_color = |n: usize| if n == 0 { t.text_dim.gamma_multiply(0.55) } else { t.text_dim };
    let count_galley = count.filter(|_| app.ui.show_counts).map(|n| ui.painter().layout_no_wrap(n.to_string(), t.font(12.5), count_color(n)));
    let label_left = r.left() + 42.0 + indent;
    let count_left = count_galley.as_ref().map_or(edge - 18.0, |g| edge - 18.0 - g.size().x);
    // a label's dot sits just before the count
    if let Some(c) = mark {
        let dot = Rect::from_center_size(pos2(count_left - MARK_W / 2.0, r.center().y), vec2(8.0, 8.0));
        ui.painter().circle_filled(dot.center(), 4.0, c);
        register(ui.ctx(), format!("labelMark:{id}"), dot);
    }
    // the name gives way to the count: cut with an ellipsis, in full on hover
    let room = count_left - mark_w - 8.0 - label_left;
    let measure = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), color).size().x;
    let full_w = measure(label);
    let shown = if full_w <= room { label.to_string() } else { crate::widgets::elide_head(label, room.max(0.0), measure) };
    let label_rect = ui.painter().text(pos2(label_left, r.center().y), Align2::LEFT_CENTER, &shown, font.clone(), color);
    register(ui.ctx(), format!("label:{id}"), label_rect);
    let resp = if shown != label { resp.on_hover_text(label) } else { resp };
    note_width(ui, row_need(indent, full_w, mark_w, count_galley.as_ref().map(|g| g.size().x)));
    if let Some(galley) = count_galley {
        let rect = Rect::from_min_size(pos2(edge - 18.0 - galley.size().x, r.center().y - galley.size().y / 2.0), galley.size());
        register(ui.ctx(), format!("count:{id}"), rect);
        ui.painter().galley(rect.min, galley, t.text_dim);
    }
    resp
}

/// Whether the row `id` about to be drawn is out of sight: then it only takes its room, is listed
/// for the control channel and asks for its width (so the content does not widen and narrow as
/// rows scroll by), and the caller skips it. A library can have thousands of keywords or folders,
/// and drawing them all every frame slows the whole app down (issue #1011). A row the sidebar is
/// asked to scroll to is drawn. `label`, `count` and `mark` as for [`row_sensed`].
fn out_of_sight(app: &LccApp, ui: &mut egui::Ui, id: &str, label: &str, count: Option<usize>, indent: f32, mark: bool) -> bool {
    let r = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), ROW_HEIGHT));
    if ui.is_rect_visible(r) || scroll_asked(ui, id) {
        return false;
    }
    ui.allocate_space(r.size());
    register(ui.ctx(), format!("source:{id}"), r);
    let indent = drawn_indent(ui, indent);
    let t = Tokens::get(ui.ctx());
    let painter = ui.painter();
    let name_w = painter.layout_no_wrap(label.to_string(), t.font(13.5), t.text_label).size().x;
    let count_w = count.filter(|_| app.ui.show_counts).map(|n| painter.layout_no_wrap(n.to_string(), t.font(12.5), t.text_dim).size().x);
    note_width(ui, row_need(indent, name_w, if mark { MARK_W } else { 0.0 }, count_w));
    true
}

/// Whether the sidebar is asked to scroll to the row `id` this frame ([`SCROLL_TO_ROW`]).
fn scroll_asked(ui: &egui::Ui, id: &str) -> bool {
    ui.data(|d| d.get_temp::<String>(egui::Id::new(SCROLL_TO_ROW))).is_some_and(|want| want == id.to_lowercase())
}

/// How wide a row needs to be, from its indent, name, colour dot and count widths. It asks for
/// room for its name up to a share of a panel, so one very long name does not make everything
/// scroll: depth does that.
fn row_need(indent: f32, name_w: f32, mark_w: f32, count_w: Option<f32>) -> f32 {
    42.0 + indent + name_w.min(MAX_NAME_NEED) + 18.0 + mark_w + count_w.map_or(0.0, |w| w + 16.0)
}

/// The disclosure chevron at the left of a tree row (`indent` is the row's): right-pointing
/// when closed, down when open, registered as widget `widget` for the control channel. Returns
/// its response: the caller flips the open state on `clicked()`.
fn disclosure_chevron(ui: &mut egui::Ui, row: &egui::Response, indent: f32, open: bool, id: egui::Id, widget: String) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let c = pos2(row.rect.left() + 10.0 + indent_x(ui.ctx(), indent), row.rect.center().y);
    let tri = Rect::from_center_size(c, vec2(16.0, 16.0));
    let tr = ui.interact(tri, id, Sense::click());
    register(ui.ctx(), widget, tri);
    let col = if tr.hovered() { t.text } else { t.text_dim };
    paint(ui.painter(), Rect::from_center_size(c, vec2(16.0, 16.0)), if open { Icon::ChevronDown } else { Icon::ChevronRight }, col);
    tr
}

/// Whether the tree row `key` is open: what the user last chose this session, else `default`.
/// Whether the tree row `key` is open: what the user last chose (persisted across sessions in
/// `app.ui.tree_open`, issue #774), falling back to in-memory UI data, else `default`.
pub(crate) fn tree_open(app: &LccApp, ui: &egui::Ui, key: &str, open_id: egui::Id, default: bool) -> bool {
    app.ui.tree_open.get(key).copied().or_else(|| ui.data(|d| d.get_temp(open_id))).unwrap_or(default)
}

/// Open or close the tree row `key` from now on.
pub(crate) fn set_tree_open(app: &mut LccApp, ui: &egui::Ui, key: &str, open_id: egui::Id, open: bool) {
    app.ui.tree_open.insert(key.to_string(), open);
    ui.data_mut(|d| d.insert_temp(open_id, open));
}

/// An Option / Alt-click on a triangle, being applied to the rows drawn after it that sit deeper
/// than it (the rows inside it). Rows that only appear once their parent opens are reached on the
/// next pass; the sweep ends after a pass that changed nothing. Folding keeps the clicked row open
/// until then, so the rows inside it are still drawn to fold.
#[derive(Clone, Default)]
struct BranchSweep {
    root: String,
    indent: f32,
    open: bool,
    pass: u64,
    changed: bool,
    active: bool,
}

fn sweep_id() -> egui::Id {
    egui::Id::new("lc-branch-sweep")
}

fn branch_sweep(app: &mut LccApp, ui: &egui::Ui, key: &str, indent: f32, open_id: egui::Id, open: &mut bool) {
    let Some(mut s) = ui.data(|d| d.get_temp::<BranchSweep>(sweep_id())) else { return };
    let pass = ui.ctx().cumulative_pass_nr();
    if s.pass != pass {
        if !s.changed {
            // nothing moved in the last pass: the sweep is done (a folded branch closes its root now)
            ui.data_mut(|d| d.remove_temp::<BranchSweep>(sweep_id()));
            if !s.open {
                app.ui.set_tree_open(&s.root, false);
                if s.root == key {
                    *open = false;
                    ui.data_mut(|d| d.insert_temp(open_id, false));
                }
            }
            return;
        }
        s.pass = pass;
        s.changed = false;
        s.active = false;
    }
    if s.root == key {
        s.active = true;
    } else if s.active {
        if indent > s.indent {
            if *open != s.open {
                *open = s.open;
                set_tree_open(app, ui, key, open_id, s.open);
                s.changed = true;
            }
        } else {
            s.active = false;
        }
    }
    ui.ctx().request_repaint();
    ui.data_mut(|d| d.insert_temp(sweep_id(), s));
}

/// The chevron of a tree row that has rows inside it (Albums' folders, Local, By Date, Folders,
/// Keywords): a click flips `open` and remembers it under `key`. Returns the chevron's response
/// (it sits on the row and takes its clicks, so a row's context menu is attached to it too).
fn tree_toggle(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    row: &egui::Response,
    indent: f32,
    key: &str,
    open_id: egui::Id,
    open: &mut bool,
    widget: String,
) -> egui::Response {
    let tri = disclosure_chevron(ui, row, indent, *open, open_id.with("triangle"), widget);
    branch_sweep(app, ui, key, indent, open_id, open);
    if tri.clicked() {
        if ui.input(|i| i.modifiers.alt) {
            // Option / Alt-click: the row and everything inside it open, or all fold
            let open_all = !*open;
            let pass = ui.ctx().cumulative_pass_nr();
            ui.data_mut(|d| {
                d.insert_temp(sweep_id(), BranchSweep { root: key.to_string(), indent, open: open_all, pass, changed: true, active: true })
            });
            if open_all {
                *open = true;
                set_tree_open(app, ui, key, open_id, true);
            }
            ui.ctx().request_repaint();
        } else {
            *open = !*open;
            set_tree_open(app, ui, key, open_id, *open);
        }
    }
    let label = if *open { crate::i18n::tr("Collapse") } else { crate::i18n::tr("Expand") };
    tri.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    tri
}

/// Space above a section's header.
const SECTION_GAP: f32 = 10.0;
/// Height of a section's header.
const SECTION_HEADER: f32 = 34.0;

/// One sidebar section: the gap above it, its header, and (while it is open) the rows `body`
/// draws. Every section goes through here, so they all fold, space and announce themselves alike.
/// `rows`: how many rows the section lists at its top level (a folded header says so).
fn section(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    section: SidebarSection,
    rows: usize,
    body: impl FnOnce(&mut LccApp, &mut egui::Ui),
) -> PlacedSection {
    ui.add_space(SECTION_GAP);
    let (header, _) = ui.allocate_exact_size(vec2(ui.available_width(), SECTION_HEADER), Sense::hover());
    // a hairline in the gap above tells the sections apart
    let t = Tokens::get(ui.ctx());
    let edge = visible_right(ui).min(header.right());
    let y = header.top() - SECTION_GAP / 2.0;
    ui.painter().hline(header.left() + 18.0..=(edge - 18.0).max(header.left() + 18.0), y, egui::Stroke::new(1.0, t.divider));
    let show = egui::Id::new("left-show-section");
    if let Some((asked, frame)) = ui.data(|d| d.get_temp::<(SidebarSection, u64)>(show))
        && asked == section
    {
        ui.data_mut(|d| d.remove::<(SidebarSection, u64)>(show));
        // asked for by the frame before (a request the section was not drawn for is forgotten),
        // and up or down only: the sidebar stays where it was scrolled sideways
        if ui.ctx().cumulative_frame_nr().saturating_sub(frame) <= 2 {
            let view_top: f32 = ui.data(|d| d.get_temp(egui::Id::new("left-view-top"))).unwrap_or(header.top());
            let delta = vec2(0.0, view_top - header.top());
            ui.scroll_with_delta_animation(delta, egui::style::ScrollAnimation::none());
        }
    }
    let open = section_header(app, ui, section, header, rows, false).open;
    if open {
        let rows_from = ui.cursor().top();
        body(app, ui);
        if ui.cursor().top() <= rows_from {
            empty_hint(ui, section);
        }
    }
    PlacedSection { section, top: header.top(), bottom: ui.cursor().top(), open, rows }
}

/// Where what the panel shows of the sidebar's content ends (screen x): the content is wider
/// than the panel while the sidebar scrolls sideways.
fn visible_right(ui: &egui::Ui) -> f32 {
    ui.data(|d| d.get_temp(egui::Id::new("left-visible-right"))).unwrap_or(f32::MAX)
}

/// The header of the open section whose own header has scrolled out of sight stays at the top of
/// the sidebar (widget `sidebarSectionPinned:<id>`), so it is always clear which section the rows
/// belong to, and the section can be folded, moved or hidden from anywhere in a long list. The
/// next section's header pushes it out as it arrives. `view_top`: where the visible part starts.
fn pinned_header(app: &mut LccApp, ui: &mut egui::Ui, placed: &[PlacedSection], view_top: f32) {
    let pinned = placed.iter().find(|p| p.open && p.top < view_top && p.bottom > view_top);
    let rect = pinned.map(|p| {
        let top = view_top.min(p.bottom - SECTION_HEADER);
        Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + SECTION_HEADER)
    });
    // rows ask where it is (next frame: they are laid out before it)
    ui.data_mut(|d| d.insert_temp(egui::Id::new("left-pinned-rect"), rect));
    let (Some(p), Some(r)) = (pinned, rect) else { return };
    let t = Tokens::get(ui.ctx());
    let view = ui.clip_rect();
    // over the rows scrolling beneath it
    ui.painter().rect_filled(Rect::from_x_y_ranges(view.x_range(), r.y_range()), 0.0, t.chrome);
    ui.painter().hline(view.x_range(), r.bottom(), egui::Stroke::new(1.0, t.divider));
    if section_header(app, ui, p.section, r, p.rows, true).toggled {
        // folded from here: its own header comes into view, not whatever was below the section
        // (next frame, when the section is laid out folded: see `section`)
        let frame = ui.ctx().cumulative_frame_nr();
        ui.data_mut(|d| d.insert_temp(egui::Id::new("left-show-section"), (p.section, frame)));
        ui.ctx().request_repaint();
    }
}

/// What an open section with nothing to list says in place of its rows (widget
/// `sidebarEmpty:<id>`), so the section stays where it is instead of disappearing.
fn empty_hint(ui: &mut egui::Ui, section: SidebarSection) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());
    register(ui.ctx(), format!("sidebarEmpty:{}", section.id()), r);
    let text = crate::i18n::tr(section.empty_hint());
    let galley = ui.painter().layout_no_wrap(text.to_string(), t.font(12.5), t.text_dim);
    note_width(ui, 42.0 + galley.size().x + 18.0);
    ui.painter().galley(pos2(r.left() + 42.0, r.center().y - galley.size().y / 2.0), galley, t.text_dim);
}

/// Where a section was laid out this frame (screen y): from its header's top to the end of its rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlacedSection {
    pub section: SidebarSection,
    pub top: f32,
    pub bottom: f32,
    /// Not folded: its rows (or its note that there are none) are drawn.
    pub open: bool,
    /// How many rows it lists at its top level.
    pub rows: usize,
}

/// What a section's header says happened to it this frame.
struct HeaderOutcome {
    open: bool,
    /// A click folded or unfolded the section.
    toggled: bool,
}

/// Where a section dragged to `y` would go among the sections drawn: above the first one whose
/// middle is below `y` (`Some`), or after them all (`None`).
pub(crate) fn section_drop_before(placed: &[PlacedSection], y: f32) -> Option<SidebarSection> {
    placed.iter().find(|p| y < (p.top + p.bottom) / 2.0).map(|p| p.section)
}

/// While a section is dragged by its header: a line shows where it would go, releasing over the
/// sidebar moves it there (`view.sidebarMoveSection`), releasing elsewhere or Esc leaves it.
fn section_drag(app: &mut LccApp, ui: &mut egui::Ui, placed: &[PlacedSection]) {
    let Some(dragged) = app.ui.dragging_section else { return };
    let (released, pos, esc) = ui.input(|i| (i.pointer.primary_released(), i.pointer.latest_pos(), i.key_pressed(egui::Key::Escape)));
    if esc {
        // the drag's Esc, not also a dialog's or a view's
        crate::widgets::take_escape(ui.ctx());
        app.ui.dragging_section = None;
        return;
    }
    let view = ui.clip_rect();
    let Some(pos) = pos.filter(|p| view.contains(*p)) else {
        if released {
            app.ui.dragging_section = None;
        }
        return;
    };
    let before = section_drop_before(placed, pos.y);
    if released {
        app.ui.dragging_section = None;
        let before = before.map_or(serde_json::Value::Null, |b| json!(b.id()));
        if let Err(e) = app.run("view.sidebarMoveSection", json!({"section": dragged.id(), "before": before})) {
            app.toast(ui.ctx(), e);
        }
        return;
    }
    // the line: in the gap above the section it would go before, or below the last one
    let y = match before {
        Some(b) => placed.iter().find(|p| p.section == b).map(|p| p.top - SECTION_GAP / 2.0),
        None => placed.last().map(|p| p.bottom + SECTION_GAP / 2.0),
    };
    if let Some(y) = y {
        let t = Tokens::get(ui.ctx());
        let line = Rect::from_min_max(pos2(view.left() + 8.0, y - 1.0), pos2(visible_right(ui).min(view.right()) - 8.0, y + 1.0));
        ui.painter().rect_filled(line, 1.0, t.accent);
        register(ui.ctx(), "sidebarSectionDrop", line);
    }
}

/// The dragged section's name follows the pointer; a drag that ended anywhere (also outside the
/// sidebar, or with the sidebar closed) is over.
pub fn section_drag_feedback(app: &mut LccApp, ctx: &egui::Context) {
    let Some(section) = app.ui.dragging_section else { return };
    let (down, pos) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
    if !down || !app.ui.left_panel {
        app.ui.dragging_section = None;
        return;
    }
    let Some(pos) = pos else { return };
    let t = Tokens::get(ctx);
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    egui::Area::new(egui::Id::new("drag-section")).order(egui::Order::Tooltip).interactable(false).fixed_pos(pos + vec2(14.0, 10.0)).show(
        ctx,
        |ui| {
            egui::Frame::NONE.fill(t.accent).corner_radius(10.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
                ui.label(egui::RichText::new(crate::i18n::tr(section.title())).color(egui::Color32::WHITE).font(t.semibold(12.0)));
            });
        },
    );
}

/// A section's header in `r`: a disclosure chevron in the gutter (where tree rows have their
/// chevron), then the bold title; a click folds or unfolds the section (kept in the UI state, so
/// it survives restarts), and what the section adds to its header (its button, see `header_button`) sits at
/// the visible edge. A folded section says how many rows it holds (`rows`, with View ▸ Show Photo
/// Counts). `pinned`: this is the copy kept at the top of the sidebar (see `pinned_header`).
fn section_header(app: &mut LccApp, ui: &mut egui::Ui, section: SidebarSection, r: Rect, rows: usize, pinned: bool) -> HeaderOutcome {
    let t = Tokens::get(ui.ctx());
    let id = section.id();
    let title = crate::i18n::tr(section.title());
    let (interact, widget) = if pinned { ("sidebar-section-pinned", "sidebarSectionPinned") } else { ("sidebar-section", "sidebarSection") };
    let resp = ui.interact(r, ui.id().with((interact, id)), Sense::click_and_drag());
    register(ui.ctx(), format!("{widget}:{id}"), r);
    let toggled = resp.clicked();
    if toggled {
        app.ui.sidebar.toggle_collapsed(section);
    }
    // dragged by its header, a section moves among the others (see `section_drag`); a press on
    // the header's button is the button's, also when it turns into a drag
    let on_button = header_button(section).is_some() && ui.input(|i| i.pointer.press_origin()).is_some_and(|p| header_button_rect(ui, r).contains(p));
    if resp.drag_started_by(egui::PointerButton::Primary) && !on_button {
        // picked up by the pinned header, the drag starts inside the top edge (see `drag_auto_scroll`)
        ui.data_mut(|d| d.insert_temp(egui::Id::new("left-section-drag-from-pinned"), pinned));
        app.ui.dragging_section = Some(section);
    }
    crate::widgets::context_menu(&resp, |ui| sections_menu(app, ui, Some(section)));
    let open = !app.ui.sidebar.is_collapsed(section);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    // what is visible of the header: its bar and count end at the panel's edge, like a row's
    let edge = r.right().min(visible_right(ui));
    let hovered = resp.hovered() && app.ui.dragging_section.is_none();
    if hovered {
        let bar = Rect::from_min_max(r.min + vec2(8.0, 2.0), pos2((edge - 8.0).max(r.left() + 8.0), r.bottom() - 2.0));
        ui.painter().rect_filled(bar, 4.0, t.hover.gamma_multiply(0.6));
    }
    let c = pos2(r.left() + 10.0, r.center().y);
    register(ui.ctx(), format!("{widget}Chevron:{id}"), Rect::from_center_size(c, vec2(16.0, 16.0)));
    let col = if hovered { t.text } else { t.text_dim };
    paint(ui.painter(), Rect::from_center_size(c, vec2(16.0, 16.0)), if open { Icon::ChevronDown } else { Icon::ChevronRight }, col);
    ui.painter().text(pos2(r.left() + 18.0, r.center().y), Align2::LEFT_CENTER, title, t.semibold(13.5), t.text_label);
    let button = header_button(section);
    // (an album being dragged puts "Top Level" in the Albums header)
    let room = !(section == SidebarSection::Albums && app.ui.dragging_album.is_some());
    if !open && rows > 0 && app.ui.show_counts && room {
        // folded: how much is in there (left of the header's own button)
        let right = edge - 18.0 - if button.is_some() { HEADER_BUTTON + 4.0 } else { 0.0 };
        let count = ui.painter().text(pos2(right, r.center().y), Align2::RIGHT_CENTER, rows.to_string(), t.font(12.5), t.text_dim);
        register(ui.ctx(), format!("{widget}Count:{id}"), count);
    }
    if section == SidebarSection::Albums {
        top_level_drop_target(app, ui, r);
    }
    if let Some(button) = button {
        show_header_button(app, ui, r, &button);
    }
    HeaderOutcome { open, toggled }
}

/// Side of a header's button.
const HEADER_BUTTON: f32 = 26.0;

/// A button at the end of a section's header (see [`header_button`]).
struct HeaderButton {
    /// Its widget is `icon:<id>`.
    id: &'static str,
    icon: Icon,
    /// What it does, in English: its tooltip and spoken name.
    tooltip: &'static str,
    action: HeaderAction,
}

/// What a header's button does when clicked.
enum HeaderAction {
    /// Does it at once (usually: opens a dialog).
    Run(fn(&mut LccApp, &egui::Context)),
    /// Opens a menu with these items.
    Menu(fn(&mut LccApp, &mut egui::Ui)),
}

/// The button a section has at the end of its header, if it has one. This is the one place that
/// says so: the header (its own and the pinned copy) draws the button at the visible edge, keeps
/// its clicks from folding the section and leaves it room beside a folded section's count.
fn header_button(section: SidebarSection) -> Option<HeaderButton> {
    match section {
        SidebarSection::Albums => {
            Some(HeaderButton { id: "albumNew", icon: Icon::Plus, tooltip: "Create Collection", action: HeaderAction::Menu(new_album_menu) })
        }
        SidebarSection::Keywords => {
            Some(HeaderButton { id: "keywordNew", icon: Icon::Plus, tooltip: "Create Keyword Tag", action: HeaderAction::Run(new_keyword) })
        }
        SidebarSection::Local | SidebarSection::ByDate | SidebarSection::Folders => None,
    }
}

/// The end of `header` kept for its button: at the visible edge, also when the sidebar is
/// scrolled sideways.
fn header_button_rect(ui: &egui::Ui, header: Rect) -> Rect {
    let right = visible_right(ui).min(header.right());
    Rect::from_min_max(pos2(right - 50.0, header.top()), pos2(right, header.bottom()))
}

/// Draw `button` at the end of `header` and act on it.
fn show_header_button(app: &mut LccApp, ui: &mut egui::Ui, header: Rect, button: &HeaderButton) {
    let mut hdr =
        ui.new_child(egui::UiBuilder::new().max_rect(header_button_rect(ui, header)).layout(egui::Layout::right_to_left(egui::Align::Center)));
    let resp = icon_button(&mut hdr, button.id, button.icon, vec2(HEADER_BUTTON, HEADER_BUTTON), false, true, button.tooltip);
    match button.action {
        HeaderAction::Run(run) => {
            if resp.clicked() {
                run(app, ui.ctx());
            }
        }
        HeaderAction::Menu(items) => {
            egui::Popup::menu(&resp).show(|ui| items(app, ui));
        }
    }
}

/// Keywords' +: Create Keyword Tag, for a keyword at the usual place for new ones (not inside
/// whatever is picked in the Keyword List, which may not even be on screen).
/// Made from here, the keyword is shown here: Keywords opens down to it (`reveal_keyword`).
fn new_keyword(app: &mut LccApp, _ctx: &egui::Context) {
    let mut dialog = crate::panels::keyword_list::create_dialog_inside(app, None);
    if let crate::state::Dialog::KeywordTag { reveal, .. } = &mut dialog {
        *reveal = true;
    }
    app.ui.dialog = Some(dialog);
}

/// The menu of a section's header (`on`) and of the My Photos title (`None`): hide this section,
/// show or hide any section, and go back to the usual arrangement. Its items are widgets
/// `sidebarMenu:hide`, `sidebarMenu:section:<id>` and `sidebarMenu:reset`.
fn sections_menu(app: &mut LccApp, ui: &mut egui::Ui, on: Option<SidebarSection>) {
    let mut run = None;
    if let Some(section) = on {
        let hide = ui.button(crate::i18n::tr("Hide This Section"));
        register(ui.ctx(), "sidebarMenu:hide", hide.rect);
        if hide.clicked() {
            run = Some(("view.sidebarSection", json!({"section": section.id(), "show": false})));
        }
        for (up, label, widget) in [(true, "Move Up", "sidebarMenu:moveUp"), (false, "Move Down", "sidebarMenu:moveDown")] {
            let item = ui.add_enabled(app.ui.sidebar.can_move(section, up), egui::Button::new(crate::i18n::tr(label)));
            register(ui.ctx(), widget, item.rect);
            if item.clicked() {
                run = Some(("view.sidebarMoveSection", json!({"section": section.id(), "direction": if up { "up" } else { "down" }})));
            }
        }
        ui.separator();
    }
    for s in app.ui.sidebar.sections().iter().filter(|s| s.id.available()) {
        let shown = ui.selectable_label(!s.hidden, crate::i18n::tr(s.id.title()));
        register(ui.ctx(), format!("sidebarMenu:section:{}", s.id.id()), shown.rect);
        if shown.clicked() {
            run = Some(("view.sidebarSection", json!({"section": s.id.id(), "show": s.hidden})));
        }
    }
    ui.separator();
    let reset = ui.add_enabled(!app.ui.sidebar.is_default(), egui::Button::new(crate::i18n::tr("Reset Sidebar Sections")));
    register(ui.ctx(), "sidebarMenu:reset", reset.rect);
    if reset.clicked() {
        run = Some(("view.sidebarReset", json!({})));
    }
    if let Some((command, params)) = run {
        if let Err(e) = app.run(command, params) {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    }
}

/// The menu of Albums' +: Create Album, Smart Album, Folder…
fn new_album_menu(app: &mut LccApp, ui: &mut egui::Ui) {
    if ui.button(crate::i18n::tr("Create Collection…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: true });
    }
    if ui.button(crate::i18n::tr("Create Smart Collection…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::SmartRules {
            id: None,
            name: String::new(),
            rules: lcc_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
            parent: None,
        });
    }
    if ui.button(crate::i18n::tr("Create Smart Collection from Filter…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::NewSmartAlbum { name: String::new(), parent: None });
    }
    if ui.button(crate::i18n::tr("Create Collection Set…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: true, parent: None, include_selected: false });
    }
    super::collections::import_item(app, ui, None);
    // only once the albums were put in an order by hand
    if app.session.catalog.album_children_are_ordered(None) {
        ui.separator();
        if ui.button(crate::i18n::tr("Sort Collections A–Z")).on_hover_text(crate::i18n::tr("Go back to listing them by name")).clicked() {
            let _ = app.run("album.sort", json!({}));
        }
    }
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider));
    frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // The module's Classic column, in `classic_left.rs` (fork-owned): Library's Navigator,
        // Catalog, Folders, Collections and Publish Services, or Develop's Navigator, Presets,
        // Versions, History and Collections. What is left below is upstream's My Photos column,
        // which Develop offers as its hidden "My Photos" panel (`classic_left::columns`).
        match app.ui.module {
            crate::module::ModuleId::Library => return classic::show(app, ui, crate::module::LIBRARY_LEFT),
            crate::module::ModuleId::Develop => return classic::show(app, ui, crate::module::DEVELOP_LEFT),
            _ => {}
        }
        my_photos_column(app, ui);
    });
}

/// Upstream's "My Photos" column: its title, then the sources and the sections in a scroll area.
fn my_photos_column(app: &mut LccApp, ui: &mut egui::Ui) {
    my_photos_title(app, ui);
    egui::ScrollArea::both().id_salt("left-scroll").auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        drag_auto_scroll(app, ui);
        // where the visible part of the content starts (screen y), for the pinned header
        let view_top = ui.cursor().top() + viewport.min.y;
        ui.data_mut(|d| d.insert_temp(egui::Id::new("left-view-top"), view_top));
        // rows are as wide as the widest one needs (last frame), at least the panel
        let wide = viewport.width().max(content_width(ui.ctx()));
        ui.set_min_width(wide);
        ui.set_max_width(wide);
        ui.data_mut(|d| {
            d.insert_temp(egui::Id::new("left-content-width-next"), 0.0f32);
            d.insert_temp(egui::Id::new("left-deepest-indent-next"), 0.0f32);
            // where the visible part of the content ends (screen x), for what stays at the edge
            d.insert_temp(egui::Id::new("left-visible-right"), ui.cursor().left() + viewport.max.x);
        });
        my_photos(app, ui, view_top);
        crate::widgets::follow_focus(ui);
        // what the rows asked for becomes next frame's width
        let next = ui.data(|d| d.get_temp::<f32>(egui::Id::new("left-content-width-next"))).unwrap_or(0.0);
        if (next - content_width(ui.ctx())).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(egui::Id::new("left-content-width"), next));
            ui.ctx().request_repaint();
        }
        // …and the deepest row drawn (or passed over out of sight) sets next frame's step
        let deepest = ui.data(|d| d.get_temp::<f32>(egui::Id::new("left-deepest-indent-next"))).unwrap_or(0.0);
        let step = step_for(viewport.width(), deepest);
        if step != indent_step(ui.ctx()) {
            ui.data_mut(|d| d.insert_temp(egui::Id::new("left-indent-step"), step));
            ui.ctx().request_repaint();
        }
    });
}

/// Upstream's "My Photos" title row (widget `sidebarTitle`): it offers the sections' menu too —
/// where hidden sections come back from — and the open library's name under it, so nobody edits in
/// the wrong one (issue #902).
fn my_photos_title(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let (hr, title) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    crate::widgets::announce(&title, crate::i18n::tr("My Photos"));
    register(ui.ctx(), "sidebarTitle", hr);
    crate::widgets::context_menu(&title, |ui| sections_menu(app, ui, None));
    let library = crate::panels::settings::open_library_name(app);
    let title_y = if library.is_some() { hr.center().y - 7.0 } else { hr.center().y };
    ui.painter().text(pos2(hr.left() + 18.0, title_y), Align2::LEFT_CENTER, crate::i18n::tr("My Photos"), t.semibold(15.0), t.text);
    if let Some(name) = library {
        ui.painter().with_clip_rect(hr.shrink2(vec2(18.0, 0.0))).text(
            pos2(hr.left() + 18.0, hr.center().y + 10.0),
            Align2::LEFT_CENTER,
            name,
            t.font(11.0),
            t.text_dim,
        );
        crate::widgets::register(ui.ctx(), "label:libraryName", hr);
        if let Some(l) = app.session.library.as_ref() {
            title.on_hover_text(l.dir.display().to_string());
        }
    }
}

/// Upstream's My Photos rows and sections: the catalog's sources, Recently Deleted with them
/// (upstream's grouping), then Albums, Local, By Date, Folders and Keywords as the user arranged
/// them. `view_top`: where the visible part starts (screen y) — the open section whose header has
/// scrolled past it is the one pinned at the top. [`my_photos_column`] passes the top of its scroll
/// area; Develop's column, drawing this as one of its panels, the top of the visible column.
fn my_photos(app: &mut LccApp, ui: &mut egui::Ui, view_top: f32) {
    let counts = app.caches.counts(&app.session.catalog);
    let (total, picks, deleted) = (counts.total, counts.picks, counts.deleted);
    // My Photos: the catalog's sources, Recently Deleted with them (upstream's grouping)
    classic::catalog_section(app, ui, total, picks, deleted, true);
    // the album just made: the folders down to it open, once (also when the section is shut)
    let reveal = app.ui.reveal_album.take();
    if reveal.is_some() {
        // …and Albums itself is shown and opened, so the new album can be seen
        app.ui.sidebar.reveal(SidebarSection::Albums);
    }
    // the keyword just made from Keywords' header: the section and the keywords above it open
    if let Some(path) = app.ui.reveal_keyword.take() {
        app.ui.sidebar.reveal(SidebarSection::Keywords);
        let mut above = String::new();
        let mut names = path.split('|').peekable();
        while let Some(name) = names.next().filter(|_| names.peek().is_some()) {
            if !above.is_empty() {
                above.push('|');
            }
            above.push_str(name);
            let key = format!("keyword:{}", above.to_lowercase());
            let open_id = egui::Id::new(("kw-open", above.to_lowercase()));
            set_tree_open(app, ui, &key, open_id, true);
        }
        // …and the sidebar scrolls to it
        ui.data_mut(|d| d.insert_temp(egui::Id::new(SCROLL_TO_ROW), format!("keyword:{path}").to_lowercase()));
    }
    // the folder Show in Folders went to: Folders is shown and opened, and so are the
    // folders above it, also when it was already the one shown and one was folded by hand…
    if let Some(path) = app.ui.reveal_folder.take() {
        app.ui.sidebar.reveal(SidebarSection::Folders);
        ui.data_mut(|d| d.remove::<Option<String>>(egui::Id::new(LIBFOLDER_REVEALED)));
        // …and the sidebar scrolls to it
        ui.data_mut(|d| d.insert_temp(egui::Id::new(SCROLL_TO_ROW), format!("libfolder:{path}").to_lowercase()));
    }
    // Local keeps track of the folder being browsed whether or not it is drawn
    let local = local_now(app, ui);
    let shown: Vec<SidebarSection> = app.ui.sidebar.shown().collect();
    let mut placed: Vec<PlacedSection> = Vec::with_capacity(shown.len());
    for s in shown {
        let at = match s {
            SidebarSection::Albums => {
                let rows = app.session.catalog.albums().filter(|a| a.parent.is_none()).count();
                section(app, ui, s, rows, |app, ui| albums_rows(app, ui, reveal))
            }
            SidebarSection::Local => {
                let Some(local) = &local else { continue };
                section(app, ui, s, local.places.places.len(), |app, ui| local_rows(app, ui, local))
            }
            SidebarSection::ByDate => {
                let rows = app.caches.date_groups(&app.session.catalog).len();
                section(app, ui, s, rows, date_rows)
            }
            SidebarSection::Folders => {
                let tree = app.caches.folder_tree(&app.session.catalog);
                section(app, ui, s, tree.len(), |app, ui| {
                    reveal_chosen(app, ui, &tree);
                    folder_rows(app, ui, &tree, 0.0);
                })
            }
            SidebarSection::Keywords => {
                let tree = app.caches.keyword_tree(&app.session.catalog);
                section(app, ui, s, tree.len(), |app, ui| keyword_rows(app, ui, &tree, 0.0))
            }
        };
        placed.push(at);
    }
    ui.data_mut(|d| d.remove::<String>(egui::Id::new(SCROLL_TO_ROW)));
    // room below the last section (a section dragged to the end is dropped here)
    ui.add_space(SECTION_GAP * 2.0);
    pinned_header(app, ui, &placed, view_top);
    section_drag(app, ui, &placed);
}

/// Albums: the tree of albums and the folders they are sorted into. `reveal`: an album just made,
/// whose folders open down to it.
fn albums_rows(app: &mut LccApp, ui: &mut egui::Ui, reveal: Option<u64>) {
    let kids: AlbumKids = app.session.catalog.album_children_by_parent().into_iter().map(|(k, v)| (k, v.into_iter().cloned().collect())).collect();
    let cat = &app.session.catalog;
    let mut open_to = Vec::new();
    let mut cur = reveal.map(AlbumId).and_then(|id| cat.album(id)).and_then(|a| a.parent);
    while let Some(p) = cur.filter(|p| !open_to.contains(p) && open_to.len() < 64) {
        open_to.push(p);
        cur = cat.album(p).and_then(|a| a.parent);
    }
    albums_tree(app, ui, &kids, None, 0.0, &open_to);
}

/// By Date: year → month → day; a click filters by that prefix, the triangle opens a level.
fn date_rows(app: &mut LccApp, ui: &mut egui::Ui) {
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

/// How many library photos have no file (Local browse records are not checked; see
/// `cmd::missing::checked_path`). Checking stats every file, which on a network share takes
/// seconds, so it runs on a worker thread: the count shown is the last finished one, refreshed
/// at most every 5 s, and at once (after the running check) when the catalog changed. The first
/// count and a recount after a catalog change are pending sidebar work ([`fs_cached_running`]):
/// the Missing Photos row they add or take away moves every row below it. The timed refresh is
/// not: it rarely changes the count, and on a share where counting takes seconds, a headless
/// run's fast simulated clock would start the next one before settling.
fn missing_count(app: &mut LccApp, ui: &mut egui::Ui) -> usize {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::AcqRel, Ordering::Relaxed};
    #[derive(Clone, Default)]
    struct Job {
        n: std::sync::Arc<AtomicUsize>,
        running: std::sync::Arc<AtomicBool>,
        at: f64,
        rev: u64,
    }
    let id = egui::Id::new("missing-count");
    let now = ui.input(|i| i.time);
    let rev = app.session.catalog.revision;
    let mut job: Job = ui.data(|d| d.get_temp(id)).unwrap_or(Job { rev: u64::MAX, at: f64::MIN, ..Default::default() });
    let fresh = now - job.at < 5.0 && job.rev == rev;
    if !fresh && !job.running.load(Relaxed) {
        // the same scope as the Missing Photos view: library photos only, never Local browse records
        let paths = lcc_engine::cmd::missing::candidates(&app.session.catalog);
        let pending = (job.rev != rev).then(|| fs_running_counter(ui.ctx()));
        if let Some(p) = &pending {
            p.fetch_add(1, AcqRel);
        }
        job.running.store(true, Relaxed);
        job.at = now;
        job.rev = rev;
        #[cfg(test)]
        let test_delay = app.missing_count_test_delay;
        let (n, running, ctx, done) = (job.n.clone(), job.running.clone(), ui.ctx().clone(), pending.clone());
        let work = move || {
            #[cfg(test)]
            if let Some(d) = test_delay {
                std::thread::sleep(d);
            }
            let missing = if cfg!(target_arch = "wasm32") { 0 } else { paths.iter().filter(|p| !std::path::Path::new(p).exists()).count() };
            n.store(missing, Relaxed);
            running.store(false, Relaxed);
            // after the count is stored: a frame that sees the pending count drop also sees the row
            if let Some(d) = done {
                d.fetch_sub(1, AcqRel);
            }
            ctx.request_repaint();
        };
        #[cfg(not(target_arch = "wasm32"))]
        if std::thread::Builder::new().name("lc-missing-count".into()).spawn(work).is_err() {
            // no thread to spare: keep the last count and try again after the next 5 s
            job.running.store(false, Relaxed);
            if let Some(p) = &pending {
                p.fetch_sub(1, AcqRel);
            }
        }
        #[cfg(target_arch = "wasm32")]
        work();
        ui.data_mut(|d| d.insert_temp(id, job.clone()));
    }
    job.n.load(Relaxed)
}

/// Local's folders as they stand this frame, and the folder being browsed among them.
struct LocalNow {
    places: LocalPlaces,
    current: Option<String>,
    /// The places removed from Local, as (name, path), in the order they were hidden.
    hidden: Vec<(String, String)>,
}

/// Folders on this computer to browse without adding (Lightroom's Local): Pictures, Desktop,
/// Downloads, the home folder, the folder being browsed, and the folders kept with Browse
/// Folder…. Worked out every frame the sidebar shows, drawn or not: the row listed for this
/// session only (`local_browse_root`) follows the browsing. `None` where there is no Local (web).
fn local_now(app: &mut LccApp, ui: &egui::Ui) -> Option<LocalNow> {
    if !SidebarSection::Local.available() {
        return None;
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    let mut builtin: Vec<(String, String)> = Vec::new();
    if !home.is_empty() {
        for (name, sub) in [("Pictures", "Pictures"), ("Desktop", "Desktop"), ("Downloads", "Downloads"), ("Home", "")] {
            // joined with the platform's separator, like the paths browsing gives back
            let p = if sub.is_empty() { home.clone() } else { std::path::Path::new(&home).join(sub).to_string_lossy().to_string() };
            // (checked off the UI thread: a home folder can be on a network share)
            if fs_cached(ui, "is-dir", &p, 5.0, |p| std::path::Path::new(p).is_dir()) == Some(true) {
                builtin.push((crate::i18n::tr(name).to_string(), p));
            }
        }
    }
    let browsing = app.session.browse.clone().filter(|_| app.session.source == LibrarySource::Folder);
    let current = browsing.as_ref().map(|b| b.path.clone());
    // a hidden built-in place keeps its name (Home, not the user's folder name)
    let name_of = |path: &str| builtin.iter().find(|(_, p)| same_folder(p, path)).map_or_else(|| folder_label(path), |(n, _)| n.clone());
    let hidden = app.ui.hidden_locations.iter().map(|h| (name_of(h), h.clone())).collect();
    let places = local_places(builtin, &app.ui.local_roots, current.as_deref(), app.ui.local_browse_root.as_deref(), &app.ui.hidden_locations);
    if current.is_some() {
        app.ui.local_browse_root = places.browse_root.clone();
    }
    Some(LocalNow { places, current, hidden })
}

/// Local's rows: each place as a folder tree, Browse Folder…, and the way back for hidden places.
fn local_rows(app: &mut LccApp, ui: &mut egui::Ui, now: &LocalNow) {
    let (local, current) = (&now.places, now.current.as_deref());
    for (i, (name, path)) in local.places.iter().enumerate() {
        let transient = local.browse_root.as_deref() == Some(path.as_str());
        let reveal = local.owner == Some(i);
        folder_tree(app, ui, path, name, path, 0.0, current, reveal, transient);
    }
    if app.services.pick_folder.is_some() && row(app, ui, "local:browse", Icon::Plus, crate::i18n::tr("Browse Folder…"), None, false, 0.0).clicked()
    {
        let picked = app.services.pick_folder.as_mut().and_then(|f| f());
        if let Some(path) = picked {
            match app.run("library.browse", json!({"path": path, "show": "all"})) {
                // the picked folder stays in Local (and comes back if it was hidden)
                Ok(r) => {
                    let dir = r["path"].as_str().unwrap_or(&path).to_string();
                    let _ = app.run("local.addRoot", json!({"path": dir}));
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
    }
    // the way back for hidden places: one comes back from the row itself; several are listed, so
    // each comes back on its own, or all at once (issue #2114: the row used to bring every one back)
    let hidden = now.hidden.len();
    if hidden > 0 {
        let label = crate::i18n::tr_format!("Show {hidden} hidden location{}", if hidden == 1 { "" } else { "s" }, hidden = hidden);
        let resp = row(app, ui, "local:restoreHidden", Icon::Folder, &label, None, false, 0.0)
            .on_hover_text(crate::i18n::tr("Put the folders you removed from Favorites back (no files change)"));
        if let [(_, only)] = now.hidden.as_slice() {
            if resp.clicked() {
                let _ = app.run("local.restoreHidden", json!({"path": only}));
            }
        } else {
            egui::Popup::menu(&resp).show(|ui| hidden_menu(app, ui, &now.hidden));
        }
    }
}

/// The list behind "Show N hidden locations": each place by name (its path on hover), then
/// Restore All.
fn hidden_menu(app: &mut LccApp, ui: &mut egui::Ui, hidden: &[(String, String)]) {
    for (name, path) in hidden {
        let r = ui.button(name).on_hover_text(path);
        register(ui.ctx(), format!("local:restoreHidden:{path}"), r.rect);
        if r.clicked() {
            let _ = app.run("local.restoreHidden", json!({"path": path}));
            ui.close();
        }
    }
    ui.separator();
    let r = ui.button(crate::i18n::tr("Restore All"));
    register(ui.ctx(), "local:restoreHidden:all", r.rect);
    if r.clicked() {
        let _ = app.run("local.restoreHidden", json!({}));
        ui.close();
    }
}

/// Whether two paths name the same folder, however they are spelled (separators, trailing
/// slash, `.`/`..`, drive-letter case; see `lcc_catalog::query::folder_key`).
pub(crate) fn same_folder(a: &str, b: &str) -> bool {
    a == b || lcc_catalog::query::folder_key(a) == lcc_catalog::query::folder_key(b)
}

/// Local's top-level folders and how the folder being browsed sits among them.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LocalPlaces {
    /// (label, path) of each top-level folder, in order.
    pub places: Vec<(String, String)>,
    /// The top-level folder the browsed folder lies in (the innermost one): its tree opens on
    /// the way down to it. None when that way passes through a hidden folder.
    pub owner: Option<usize>,
    /// A folder listed only for this session because the browsed folder is in no saved
    /// location (browsed from a breadcrumb, the CLI…); it stays while browsing below it.
    pub browse_root: Option<String>,
}

fn folder_label(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

/// Local's top-level folders, in order: the built-in places, the folders kept with Browse
/// Folder… / Keep in Local (`saved`), then — only when the browsed folder lies in none of those —
/// a session root for it: the previous one (`browse_root`) while browsing stays below it, else
/// the browsed folder itself. A folder inside a listed one is shown inside that one's tree (the
/// root stays; its siblings stay reachable), never as a root of its own. Each folder is listed
/// once however its path is spelled (the first spelling wins), and hidden ones are left out —
/// they stay reachable through breadcrumbs and Browse Folder….
pub(crate) fn local_places(
    builtin: Vec<(String, String)>,
    saved: &[String],
    browsing: Option<&str>,
    browse_root: Option<&str>,
    hidden: &[String],
) -> LocalPlaces {
    use lcc_catalog::query::{folder_key, folder_within};
    let is_hidden = |p: &str| hidden.iter().any(|h| same_folder(h, p));
    let mut places = builtin;
    for path in saved {
        if !places.iter().any(|(_, p)| same_folder(p, path)) {
            places.push((folder_label(path), path.clone()));
        }
    }
    places.retain(|(_, p)| !is_hidden(p));
    let mut out = LocalPlaces::default();
    if let Some(c) = browsing
        && !places.iter().any(|(_, p)| folder_within(c, p))
    {
        let root = browse_root.filter(|r| folder_within(c, r)).unwrap_or(c);
        if !is_hidden(root) {
            places.push((folder_label(root), root.to_string()));
            out.browse_root = Some(root.to_string());
        }
    }
    if let Some(c) = browsing {
        // A tree never opens on the way down through a hidden folder: hiding a kept folder
        // beneath Home would otherwise reveal it again inside Home's (possibly huge) tree.
        let through_hidden = |p: &str| hidden.iter().any(|h| folder_within(c, h) && folder_within(h, p));
        out.owner = places
            .iter()
            .enumerate()
            .filter(|(_, (_, p))| folder_within(c, p) && !through_hidden(p))
            .max_by_key(|(_, (_, p))| folder_key(p).len())
            .map(|(i, _)| i);
    }
    out.places = places;
    out
}

/// The subfolders of `path` (not hidden ones), sorted; listed on a worker thread at most every
/// 2 s per folder (empty until the first listing).
fn subfolders(ui: &egui::Ui, path: &str) -> Vec<(String, String)> {
    fs_cached(ui, "subfolders", path, 2.0, list_subfolders).unwrap_or_default()
}

fn list_subfolders(path: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(path)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && !hidden_attribute(e))
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    (!name.starts_with('.')).then(|| (name, e.path().to_string_lossy().to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|(n, _)| n.to_lowercase());
    v
}

/// On Windows a folder is hidden by an attribute (AppData in the home folder is), not by a
/// leading dot.
#[cfg(windows)]
fn hidden_attribute(e: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    e.metadata().is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
}

#[cfg(not(windows))]
fn hidden_attribute(_: &std::fs::DirEntry) -> bool {
    false
}

/// How many [`fs_cached`] answers for `ctx` are being worked out right now. Rows appear (and
/// the sidebar below them moves) when they land, so the headless driver counts them as pending
/// work and waits for them before acting on widget positions.
pub(crate) fn fs_cached_running(ctx: &egui::Context) -> usize {
    fs_running_counter(ctx).load(std::sync::atomic::Ordering::Acquire)
}

fn fs_running_counter(ctx: &egui::Context) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<std::sync::Arc<std::sync::atomic::AtomicUsize>>(egui::Id::new("fs-cached-running")).clone())
}

/// A file-system answer for `path` (`f(path)`), kept per `kind` and path and refreshed on a worker
/// thread at most every `every` seconds: a folder on a sleeping NAS, a dropped share or a
/// spinning-up drive never blocks a frame. `None` until the first answer arrives.
pub(crate) fn fs_cached<T: Clone + Send + 'static>(ui: &egui::Ui, kind: &'static str, path: &str, every: f64, f: fn(&str) -> T) -> Option<T> {
    struct Entry<T> {
        value: Option<T>,
        at: Option<f64>,
        running: bool,
    }
    type Cell<T> = std::sync::Arc<std::sync::Mutex<Entry<T>>>;
    let id = egui::Id::new(("fs-cached", kind, path.to_string()));
    let now = ui.input(|i| i.time);
    let cell: Cell<T> = match ui.data(|d| d.get_temp::<Cell<T>>(id)) {
        Some(c) => c,
        None => {
            let c: Cell<T> = std::sync::Arc::new(std::sync::Mutex::new(Entry { value: None, at: None, running: false }));
            ui.data_mut(|d| d.insert_temp(id, c.clone()));
            c
        }
    };
    let lock = |c: &Cell<T>| c.lock().unwrap_or_else(std::sync::PoisonError::into_inner).value.clone();
    let start = {
        let mut e = cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let due = !e.running && e.at.is_none_or(|at| now - at >= every || now < at);
        if due {
            e.running = true;
            e.at = Some(now);
        }
        due
    };
    if start {
        let running = fs_running_counter(ui.ctx());
        running.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        // (settled when the work ends however it ends: a listing that panics must not stay running, nor keep
        // the count up, which `Headless::busy` waits on)
        struct Settle<T> {
            cell: Cell<T>,
            count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            repaint: egui::Context,
        }
        impl<T> Drop for Settle<T> {
            fn drop(&mut self) {
                self.cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner).running = false;
                // after the answer is stored: a frame that sees the count drop also sees the answer
                self.count.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                self.repaint.request_repaint();
            }
        }
        let (path, settle) = (path.to_string(), Settle { cell: cell.clone(), count: running.clone(), repaint: ui.ctx().clone() });
        let work = move || {
            let v = f(&path);
            settle.cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner).value = Some(v);
            drop(settle);
        };
        #[cfg(not(target_arch = "wasm32"))]
        // (no thread to be had: the closure is dropped unrun, which settles it)
        let _ = std::thread::Builder::new().name("lc-fs-list".into()).spawn(work);
        #[cfg(target_arch = "wasm32")]
        work();
    }
    lock(&cell)
}

/// A folder on disk with a disclosure triangle: click browses it, the triangle lists its
/// subfolders. In the tree that holds the folder being browsed (`reveal`), the folders on the
/// way down to it open whenever the browsed folder changes, so it shows highlighted in place.
/// `transient`: a top-level row listed for this session only (it offers Keep in Local).
/// `place`: the top-level folder this tree hangs from. A folder can be listed in more than one
/// tree (Pictures on its own and inside Home): each row opens and closes by itself, and their
/// triangles have ids of their own, else the copy drawn last takes the other's clicks (#1093).
#[allow(clippy::too_many_arguments)]
pub(crate) fn folder_tree(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    place: &str,
    name: &str,
    path: &str,
    indent: f32,
    current: Option<&str>,
    reveal: bool,
    transient: bool,
) {
    let key = format!("local:{place}:{path}");
    let open_id = egui::Id::new(("folder-open", place.to_string(), path.to_string()));
    let sel = current.is_some_and(|c| same_folder(c, path));
    let on_the_way = !sel && current.is_some_and(|c| lcc_catalog::query::folder_within(c, path));
    let mut open = tree_open(app, ui, &key, open_id, false);
    if reveal && on_the_way {
        // opened once per browsed folder: collapsing it again afterwards sticks
        let revealed_id = egui::Id::new(("folder-revealed", place.to_string(), path.to_string()));
        let target = current.map(str::to_string);
        if ui.data(|d| d.get_temp::<Option<String>>(revealed_id)) != Some(target.clone()) {
            open = true;
            set_tree_open(app, ui, &key, open_id, true);
            ui.data_mut(|d| d.insert_temp(revealed_id, target));
        }
    }
    // at the tree's own indent, as in Albums and Folders: the triangle sits in the gutter and the
    // icon lines up with the rows above
    // a poll's "N new" (issue #123), as on the Folders rows
    let badged = lcc_engine::folder_watch::new_in(&app.session, path).map(|k| format!("{name} · {k} {}", crate::i18n::tr("new")));
    let shown = badged.as_deref().unwrap_or(name);
    let resp = row(app, ui, &format!("local:{path}"), Icon::Folder, shown, None, sel, indent).on_hover_text(path);
    let tr = tree_toggle(app, ui, &resp, indent, &key, open_id, &mut open, format!("folderToggle:{path}"));
    if !tr.clicked()
        && resp.clicked()
        // every photo of the folder, library ones too: the Import tab's Show Not Imported is
        // that tab's alone, and kept it would hide a library folder's photos here (#881)
        && let Err(e) = app.run("library.browse", json!({"path": path, "show": "all"}))
    {
        app.toast(ui.ctx(), e);
    }
    disk_folder_menu(app, &resp, FolderRow::Local { top: indent == 0.0, transient }, path, None);
    if open && indent < INDENT_STEP * 8.0 {
        for (n, p) in subfolders(ui, path) {
            folder_tree(app, ui, place, &n, &p, indent + INDENT_STEP, current, reveal, false);
        }
    }
}

/// One By Date row (`key`: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`); returns whether it is open.
fn date_row(app: &mut LccApp, ui: &mut egui::Ui, key: &str, label: &str, count: usize, indent: f32) -> bool {
    let str_key = format!("date:{key}");
    let open_id = egui::Id::new(("date-open", key.to_string()));
    let mut open = tree_open(app, ui, &str_key, open_id, false);
    let sel = app.session.filter.date.as_deref() == Some(key);
    let resp = row(app, ui, &format!("date:{key}"), Icon::Clock, label, Some(count), sel, indent);
    if key.len() < 10 {
        tree_toggle(app, ui, &resp, indent, &str_key, open_id, &mut open, format!("dateToggle:{key}"));
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
pub(crate) fn browse_all_photos(app: &mut LccApp, on: bool) {
    if on && app.session.source != LibrarySource::All {
        let _ = app.run("library.source", json!({"kind": "all"}));
    }
}

/// The albums inside each folder (`None`: the top level), in the order they are listed.
type AlbumKids = std::collections::HashMap<Option<AlbumId>, Vec<Album>>;

/// `open_to`: folders forced open this frame (the way down to an album that was just made).
fn albums_tree(app: &mut LccApp, ui: &mut egui::Ui, all: &AlbumKids, parent: Option<AlbumId>, indent: f32, open_to: &[AlbumId]) {
    // 64 folders deep at most (16 points a level): a catalog that loops must not overflow the stack
    if indent > 64.0 * INDENT_STEP {
        return;
    }
    let Some(kids) = all.get(&parent) else { return };
    for a in kids {
        if a.folder {
            let key = format!("album:{}", a.id.0);
            let open_id = egui::Id::new(("folder-open", a.id.0));
            let mut open = tree_open(app, ui, &key, open_id, true);
            if open_to.contains(&a.id) && !open {
                open = true;
                set_tree_open(app, ui, &key, open_id, true);
            }
            let resp =
                row_sensed(app, ui, &format!("folder:{}", a.id.0), Icon::Folder, &a.name, None, None, false, indent, Sense::click_and_drag(), None);
            // a folder is no source, so its row folds it too; the triangle is the same click, aimed
            let has_children = all.get(&Some(a.id)).is_some_and(|v| !v.is_empty());
            if resp.drag_started() {
                app.ui.dragging_album = Some(a.id.0);
            }
            if album_drag_over(app, ui, &resp, a, has_children.then_some(&mut open), indent) {
                set_tree_open(app, ui, &key, open_id, true);
            }
            // what this frame draws; a click shows from the next one
            let draw_open = open;
            let mut toggled = false;
            if has_children {
                let tri = tree_toggle(app, ui, &resp, indent, &key, open_id, &mut open, format!("albumToggle:{}", a.id.0));
                toggled = tri.clicked();
                // the triangle sits on the row and takes its clicks: the menu opens from it too
                folder_menu(app, &tri, a);
            }
            // one click, one toggle: the triangle's is not the row's as well
            if resp.clicked() && !toggled {
                set_tree_open(app, ui, &key, open_id, !open);
            }
            folder_menu(app, &resp, a);
            if draw_open {
                albums_tree(app, ui, all, Some(a.id), indent + INDENT_STEP, open_to);
            }
        } else {
            let sel = app.session.source == LibrarySource::Album(a.id);
            let icon = crate::creations_ui::icon(a).unwrap_or(if a.is_smart() { Icon::SmartAlbum } else { Icon::Album });
            // cached: a smart album's count scans the catalog (in the background: no number until it is known)
            let now = (app.session.clock)();
            let n = app.caches.album_counts(&app.session.catalog, app.session.library_generation(), &now, ui.ctx()).get(&a.id).copied();
            // the album B adds to is marked "+"
            let target =
                app.session.target_album.filter(|t| app.session.catalog.album(*t).is_some()).or_else(|| app.session.catalog.quick_collection());
            let label = if target == Some(a.id) { format!("{} +", a.name) } else { a.name.clone() };
            // a smart album whose rules no longer check (an album they test was deleted) says so
            let problems = app.caches.smart_album_problems(&app.session.catalog).get(&a.id).cloned().unwrap_or_default();
            let label = if problems.is_empty() { label } else { format!("⚠ {label}") };
            let mut resp = row_sensed(app, ui, &format!("album:{}", a.id.0), icon, &label, None, n, sel, indent, Sense::click_and_drag(), None);
            if !problems.is_empty() {
                crate::widgets::register(ui.ctx(), format!("albumProblem:{}", a.id.0), resp.rect);
            }
            if resp.drag_started() {
                app.ui.dragging_album = Some(a.id.0);
            }
            album_drag_over(app, ui, &resp, a, None, indent);
            // the device-sync pill, left of the photo count
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(pill) = app.session.sync_collection_pill(a.id) {
                let size = super::sync_pill::size(ui, &pill);
                let at = egui::pos2(resp.rect.right() - 44.0 - size.x, resp.rect.center().y - size.y / 2.0);
                super::sync_pill::paint(ui, at, &pill, &format!("badge:syncCollection:{}", a.id.0));
            }
            if !a.is_smart() {
                drop_target(app, ui, &resp, a);
            }
            if let Some(rules) = &a.smart {
                // one tooltip: the rules, then what needs fixing in them
                let mut tip = crate::i18n::tr_format!("Smart collection: {}", crate::i18n::filter_label(rules, &app.session.catalog));
                if !problems.is_empty() {
                    tip.push_str("\n\n");
                    tip.push_str(crate::i18n::tr("Some rules need fixing (Edit Smart Collection…):"));
                    for p in &problems {
                        tip.push('\n');
                        tip.push_str(&crate::i18n::problem_line(p));
                    }
                }
                resp = resp.on_hover_text(tip);
            }
            if resp.clicked() {
                let _ = app.run("library.source", json!({"kind": "album", "id": a.id.0}));
            }
            if resp.double_clicked() {
                crate::creations_ui::double_clicked(app, ui.ctx(), a);
            }
            folder_menu(app, &resp, a);
        }
    }
}

/// Whether the pointer is over `rect` and over what the panel shows of it (a row scrolled out of
/// sight is not a drop target).
fn pointer_over(ui: &egui::Ui, rect: Rect) -> bool {
    crate::docking::drop_target(ui, rect, ui.input(|i| i.pointer.latest_pos()))
}

/// [`pointer_over`] for a row: a row scrolled beneath the pinned header is covered by it, so the
/// pointer there is over the header, not over the row.
fn pointer_over_row(ui: &egui::Ui, rect: Rect) -> bool {
    pointer_over(ui, rect) && !pointer_on_pinned_header(ui)
}

/// Where the pinned header was drawn last frame, if one was (rows are laid out before it).
fn pinned_rect(ui: &egui::Ui) -> Option<Rect> {
    ui.data(|d| d.get_temp::<Option<Rect>>(egui::Id::new("left-pinned-rect"))).flatten()
}

fn pointer_on_pinned_header(ui: &egui::Ui) -> bool {
    let pos = ui.input(|i| i.pointer.latest_pos());
    pinned_rect(ui).zip(pos).is_some_and(|(r, p)| r.contains(p))
}

/// How close to the top or bottom edge of the sidebar (points) a dragged album starts scrolling it.
const SCROLL_EDGE: f32 = 36.0;
/// The fastest the sidebar scrolls for a drag (points per second).
const SCROLL_MAX_SPEED: f32 = 700.0;

/// How fast the sidebar scrolls (points per second, positive = down) for a dragged item with the
/// pointer at `y`, the visible part being `top..bottom`: none in the middle, faster the nearer to
/// the edge, the top speed at and past it.
pub fn auto_scroll_speed(y: f32, top: f32, bottom: f32) -> f32 {
    let depth = |dist: f32| ((SCROLL_EDGE - dist) / SCROLL_EDGE).clamp(0.0, 1.0);
    SCROLL_MAX_SPEED * (depth(bottom - y) - depth(y - top))
}

/// While an album, or photos from the grid, are dragged over the sidebar: scrolls it when the
/// pointer is near its top or bottom edge, so rows beyond the visible part can be reached. Call
/// inside the scroll area.
fn drag_auto_scroll(app: &LccApp, ui: &egui::Ui) {
    if app.ui.dragging_album.is_none() && app.ui.dragging_photos.is_none() && app.ui.dragging_section.is_none() {
        return;
    }
    let view = ui.clip_rect();
    let Some(p) = ui.input(|i| i.pointer.latest_pos()).filter(|p| p.x >= view.left() && p.x <= view.right()) else { return };
    // a section picked up by its pinned header starts inside the top edge: it scrolls once it has
    // left the header. Any other drag scrolls there as at any edge, to reach what is above
    let from_pinned = egui::Id::new("left-section-drag-from-pinned");
    if app.ui.dragging_section.is_some() && ui.data(|d| d.get_temp::<bool>(from_pinned)) == Some(true) {
        if pointer_on_pinned_header(ui) {
            return;
        }
        ui.data_mut(|d| d.insert_temp(from_pinned, false));
    }
    let speed = auto_scroll_speed(p.y, view.top(), view.bottom());
    if speed != 0.0 {
        // a positive delta moves the content down: the view goes up
        ui.scroll_with_delta_animation(vec2(0.0, -speed * ui.input(|i| i.stable_dt).min(0.1)), egui::style::ScrollAnimation::none());
        ui.ctx().request_repaint();
    }
}

/// Room a colour label's dot takes before a row's count.
const MARK_W: f32 = 12.0;

/// How long a dragged album must rest on a closed folder before it opens.
const HOVER_OPEN_SECS: f64 = 0.6;

/// Whether the album `dragged` may be dropped into the folder `target` (`None`: the top level):
/// a folder other than where it already is, and not itself or something inside it.
fn can_drop_album(app: &LccApp, dragged: AlbumId, target: Option<AlbumId>) -> bool {
    let Some(d) = app.session.catalog.album(dragged) else { return false };
    match target {
        None => d.parent.is_some(),
        Some(t) => app.session.catalog.album(t).is_some_and(|f| f.folder) && d.parent != Some(t) && !is_within(app, t, dragged),
    }
}

/// Ends a drag that ran `result`: the folders down to the album open so it shows in its new place.
fn finish_drop(app: &mut LccApp, ui: &egui::Ui, dragged: AlbumId, result: Result<serde_json::Value, String>) {
    forget_hover(app, ui.ctx());
    match result {
        Ok(_) => {
            app.ui.reveal_album = Some(dragged.0);
            ui.ctx().request_repaint();
        }
        Err(e) => app.toast(ui.ctx(), e),
    }
    app.ui.dragging_album = None;
}

/// Forgets how long the pointer rested on each folder (a new drag starts counting again).
fn forget_hover(app: &LccApp, ctx: &egui::Context) {
    let ids: Vec<u64> = app.session.catalog.albums().filter(|a| a.folder).map(|a| a.id.0).collect();
    ctx.data_mut(|d| {
        for id in ids {
            d.remove_temp::<f64>(egui::Id::new(("album-hover", id)));
        }
    });
}

/// Moves the dragged album into a folder (`album.move`, one undo step; `None`: the top level).
fn drop_album(app: &mut LccApp, ui: &egui::Ui, dragged: AlbumId, target: Option<AlbumId>) {
    let r = app.run("album.move", json!({"id": dragged.0, "parent": target.map(|t| t.0)}));
    finish_drop(app, ui, dragged, r);
}

/// Places the dragged album in `parent` before the sibling `before` (`None`: last of its kind), by
/// hand (`album.reorder`, one undo step).
fn place_album(app: &mut LccApp, ui: &egui::Ui, dragged: AlbumId, parent: Option<AlbumId>, before: Option<AlbumId>) {
    let r = app.run("album.reorder", json!({"id": dragged.0, "parent": parent.map(|p| p.0), "before": before.map(|b| b.0)}));
    finish_drop(app, ui, dragged, r);
}

/// What dropping the dragged album on a row would do.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AlbumDrop {
    /// Into the folder.
    Into(AlbumId),
    /// Beside the row: in `parent`, before `before` (`None`: last of its kind); the line is drawn
    /// above the row (`above`) or below it.
    Place { parent: Option<AlbumId>, before: Option<AlbumId>, above: bool },
}

/// What dropping `dragged` at `frac` (0 = top edge, 1 = bottom edge) of the row of `target` does,
/// if anything. An album over a folder goes inside it; a folder over a folder goes inside by its
/// middle and beside it by its edges; an album over an album goes before it by its top half and
/// after it by its bottom half; a folder over an album does nothing. Placing where it already
/// is, or inside itself, is nothing too. `expanded`: `target` is a folder shown open.
fn album_drop_at(app: &LccApp, dragged: AlbumId, target: &Album, frac: f32, expanded: bool) -> Option<AlbumDrop> {
    let cat = &app.session.catalog;
    let d = cat.album(dragged)?;
    let into = || can_drop_album(app, dragged, Some(target.id)).then_some(AlbumDrop::Into(target.id));
    if d.folder != target.folder {
        return if target.folder { into() } else { None };
    }
    if target.id == dragged {
        return None;
    }
    let after = frac >= if target.folder { 0.7 } else { 0.5 };
    // the middle of a folder, and the bottom of an open one (its first child is next), mean inside
    if target.folder && frac >= 0.3 && (!after || expanded) {
        return into();
    }
    let parent = target.parent;
    if parent.is_some_and(|p| is_within(app, p, dragged)) {
        return None;
    }
    let sibs = cat.album_children(parent);
    let at = sibs.iter().position(|a| a.id == target.id)?;
    let was = sibs.iter().position(|a| a.id == dragged);
    if !after {
        (was.map(|w| w + 1) != Some(at)).then_some(AlbumDrop::Place { parent, before: Some(target.id), above: true })
    } else {
        let next = sibs.iter().skip(at + 1).find(|a| a.id != dragged).filter(|a| a.folder == target.folder).map(|a| a.id);
        (was != Some(at + 1)).then_some(AlbumDrop::Place { parent, before: next, above: false })
    }
}

/// A row while an album is dragged: a folder outlined under the pointer when the drop would go
/// inside it, a line above or below the row when it would go beside it, and the move or placement
/// done on release. A closed folder with something inside (`open`) opens once the pointer rests on
/// it; returns whether it did.
fn album_drag_over(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, row: &Album, open: Option<&mut bool>, indent: f32) -> bool {
    let hover_id = egui::Id::new(("album-hover", row.id.0));
    let Some(dragged) = app.ui.dragging_album.map(AlbumId) else { return false };
    let pos = ui.input(|i| i.pointer.latest_pos()).filter(|_| pointer_over_row(ui, resp.rect));
    let Some(pos) = pos else {
        if ui.data(|d| d.get_temp::<f64>(hover_id)).is_some() {
            ui.data_mut(|d| d.remove_temp::<f64>(hover_id));
        }
        return false;
    };
    let expanded = open.as_deref().copied().unwrap_or(false);
    let frac = ((pos.y - resp.rect.top()) / resp.rect.height().max(1.0)).clamp(0.0, 1.0);
    let drop = album_drop_at(app, dragged, row, frac, expanded);
    let mut opened = false;
    // only a drop that goes inside opens the folder: on its edges the rows would shift under the pointer
    let inside = matches!(drop, Some(AlbumDrop::Into(_)));
    if !inside && ui.data(|d| d.get_temp::<f64>(hover_id)).is_some() {
        ui.data_mut(|d| d.remove_temp::<f64>(hover_id));
    }
    if inside
        && let Some(open) = open
        && !*open
    {
        let now = ui.input(|i| i.time);
        let since = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(hover_id, || now));
        if now - since >= HOVER_OPEN_SECS {
            *open = true;
            opened = true;
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let t = Tokens::get(ui.ctx());
    let released = ui.input(|i| i.pointer.primary_released());
    match drop {
        Some(AlbumDrop::Into(folder)) => {
            ui.painter().rect_stroke(resp.rect.shrink2(vec2(8.0, 1.0)), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
            if released {
                drop_album(app, ui, dragged, Some(folder));
            }
        }
        Some(AlbumDrop::Place { parent, before, above }) => {
            let y = if above { resp.rect.top() + 0.5 } else { resp.rect.bottom() - 0.5 };
            let (x0, x1) = (resp.rect.left() + 18.0 + indent_x(ui.ctx(), indent), ui.clip_rect().right() - 8.0);
            ui.painter().circle_stroke(pos2(x0, y), 3.0, egui::Stroke::new(1.5, t.accent));
            ui.painter().line_segment([pos2(x0 + 3.0, y), pos2(x1.max(x0 + 3.0), y)], egui::Stroke::new(2.0, t.accent));
            if released {
                place_album(app, ui, dragged, parent, before);
            }
        }
        None => {}
    }
    opened
}

/// While an album that sits in a folder is dragged: the "Collections" header takes it back to the top
/// level (outlined, with a hint). The header stays in view however long the tree is, and nothing
/// moves when it appears.
fn top_level_drop_target(app: &mut LccApp, ui: &mut egui::Ui, header: Rect) {
    let Some(dragged) = app.ui.dragging_album.map(AlbumId) else { return };
    if !can_drop_album(app, dragged, None) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    register(ui.ctx(), "albumDrop:top", header);
    let over = pointer_over(ui, header);
    let stroke = if over { egui::Stroke::new(1.5, t.accent) } else { egui::Stroke::new(1.0, t.text_dim.gamma_multiply(0.5)) };
    ui.painter().rect_stroke(header.shrink2(vec2(8.0, 3.0)), 4.0, stroke, egui::StrokeKind::Inside);
    // right-aligned, left of the + button
    let edge = header.right().min(ui.clip_rect().right());
    ui.painter().text(pos2(edge - 56.0, header.center().y), Align2::RIGHT_CENTER, crate::i18n::tr("Top Level"), t.font(12.5), t.text_dim);
    if over && ui.input(|i| i.pointer.primary_released()) {
        drop_album(app, ui, dragged, None);
    }
}

/// While an album is dragged in the sidebar: its name at the pointer; the drag ends when the
/// button is up (the drop targets act on the release frame, before this runs).
pub fn album_drag_feedback(app: &mut LccApp, ctx: &egui::Context) {
    let Some(id) = app.ui.dragging_album else { return };
    let (released, down, pos, esc) =
        ctx.input(|i| (i.pointer.primary_released(), i.pointer.primary_down(), i.pointer.latest_pos(), i.key_pressed(egui::Key::Escape)));
    if released || !down || esc {
        app.ui.dragging_album = None;
        forget_hover(app, ctx);
        return;
    }
    let (Some(pos), Some(name)) = (pos, app.session.catalog.album(AlbumId(id)).map(|a| a.name.clone())) else { return };
    let t = Tokens::get(ctx);
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    egui::Area::new(egui::Id::new("drag-album")).order(egui::Order::Tooltip).interactable(false).fixed_pos(pos + vec2(14.0, 10.0)).show(ctx, |ui| {
        egui::Frame::NONE.fill(t.accent).corner_radius(10.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
            ui.label(egui::RichText::new(name).color(egui::Color32::WHITE).font(t.semibold(12.0)));
        });
    });
}

/// An album row while photos are dragged from the grid: highlighted under the pointer; a
/// release there adds them.
fn drop_target(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, a: &Album) {
    let Some(ids) = app.ui.dragging_photos.clone() else { return };
    // only where the row can be seen: not scrolled out of the sidebar or beneath the pinned header
    let over = pointer_over_row(ui, resp.rect);
    if !over {
        return;
    }
    let released = ui.input(|i| i.pointer.any_released());
    // albums hold library photos (issue #684): Local ones can't be dropped in; the row doesn't
    // light up, and a release says why
    let photos: Vec<lcc_catalog::PhotoId> = ids.iter().map(|id| lcc_catalog::PhotoId(*id)).collect();
    if super::grid::local_only(app, &photos) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        if released {
            app.toast(ui.ctx(), super::grid::albums_hint());
            app.ui.dragging_photos = None;
        }
        return;
    }
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_stroke(resp.rect.shrink2(vec2(8.0, 1.0)), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    if released {
        match app.run("album.addPhotos", json!({"id": a.id.0, "ids": ids})) {
            Ok(r) => {
                let n = r.get("added").and_then(serde_json::Value::as_u64).unwrap_or(0);
                let skipped = r.get("skipped").and_then(serde_json::Value::as_u64).unwrap_or(0);
                let mut msg = crate::i18n::tr_format!("Added {n} photo{} to “{}”", if n == 1 { "" } else { "s" }, a.name, n = n);
                if skipped > 0 {
                    msg.push_str(" · ");
                    msg.push_str(&crate::i18n::tr_format!(
                        "{n} Local photo{} skipped: collections hold catalog photos (Add to My Photos first)",
                        if skipped == 1 { "" } else { "s" },
                        n = skipped
                    ));
                }
                app.toast(ui.ctx(), msg);
            }
            Err(e) => app.toast(ui.ctx(), e),
        }
        app.ui.dragging_photos = None;
    }
}

fn folder_menu(app: &mut LccApp, resp: &egui::Response, a: &Album) {
    crate::widgets::context_menu(resp, |ui| {
        if a.folder {
            // the same commands as File ▸ New…, aimed at this folder
            crate::widgets::menu_button(ui, crate::i18n::tr("New"), |ui| {
                for (label, command) in [
                    ("Create Collection…", "dialog.newAlbum"),
                    ("Create Smart Collection…", "dialog.smartAlbum"),
                    ("Create Smart Collection from Filter…", "dialog.newSmartAlbum"),
                    ("Create Collection Set…", "dialog.newFolder"),
                ] {
                    if ui.button(crate::i18n::tr(label)).clicked() {
                        let _ = app.run(command, json!({"parent": a.id.0}));
                        ui.close();
                    }
                }
            });
            if app.session.catalog.album_children_are_ordered(Some(a.id))
                && ui.button(crate::i18n::tr("Sort Contents A–Z")).on_hover_text(crate::i18n::tr("Go back to listing them by name")).clicked()
            {
                let _ = app.run("album.sort", json!({"parent": a.id.0}));
                ui.close();
            }
            ui.separator();
        }
        if !a.folder && !a.is_smart() {
            let local_only = super::grid::local_only(app, &app.session.selected());
            let b = ui
                .add_enabled(!local_only, egui::Button::new(crate::i18n::tr("Add Selected Photos")))
                .on_disabled_hover_text(super::grid::albums_hint());
            if b.clicked() {
                match app.run("album.addPhotos", json!({"id": a.id.0})) {
                    Ok(r) => super::grid::toast_skipped(app, ui.ctx(), &r),
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
            // File ▸ Import's ways in; the import review opens with this album chosen
            let menu = crate::widgets::menu_button(ui, crate::i18n::tr("Import"), |ui| {
                let mut entry = |ui: &mut egui::Ui, label: String, widget: String, command: &str, params: serde_json::Value| {
                    let r = ui.button(label);
                    crate::widgets::register(ui.ctx(), widget, r.rect);
                    if r.clicked() {
                        if let Err(e) = app.run(command, params) {
                            app.toast(ui.ctx(), e);
                        }
                        ui.close();
                    }
                };
                for (label, command, widget) in [
                    ("Import Photos…", "file.addPhotos", "button:albumImportPhotos"),
                    ("Import from Folder…", "file.addFolder", "button:albumImportFolder"),
                ] {
                    entry(ui, crate::i18n::tr(label).into(), widget.into(), command, json!({"album": a.id.0}));
                }
                // like File ▸ Import from Device: one entry per camera or card found
                let device = crate::widgets::menu_button(ui, crate::i18n::tr("Import from Device"), |ui| {
                    let devices = lcc_engine::devices::devices();
                    if devices.is_empty() {
                        ui.add_enabled(false, egui::Button::new(crate::i18n::tr("No Camera or Card Found")));
                    }
                    for (i, d) in devices.into_iter().enumerate() {
                        let params = json!({"path": d.path, "album": a.id.0});
                        entry(ui, format!("{}…", d.name), format!("button:albumImportDevice:{i}"), "file.addFromDevice", params);
                    }
                });
                crate::widgets::register(ui.ctx(), "button:albumImportDevice", device.response.rect);
            });
            crate::widgets::register(ui.ctx(), "button:albumImport", menu.response.rect);
        }
        if !a.folder
            && !a.is_smart()
            && !a.quick
            && ui
                .button(crate::i18n::tr("Duplicate Collection"))
                .on_hover_text(crate::i18n::tr("Another collection with the same photos, to arrange in its own order"))
                .clicked()
        {
            match app.run("album.duplicate", json!({"id": a.id.0})) {
                Ok(r) => {
                    if let Some(n) = r["id"].as_u64() {
                        let _ = app.run("library.source", json!({"kind": "album", "id": n}));
                    }
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
            ui.close();
        }
        if !a.folder && !a.is_smart() {
            let is_target = app.session.target_album == Some(a.id) || (app.session.target_album.is_none() && a.quick);
            if !is_target && ui.button(crate::i18n::tr("Set as Target Collection (B adds to it)")).clicked() {
                let _ = app.run("album.setTarget", json!({"id": if a.quick { serde_json::Value::Null } else { json!(a.id.0) }}));
            }
            if is_target && !a.quick && ui.button(crate::i18n::tr("Stop Using as Target Collection")).clicked() {
                let _ = app.run("album.setTarget", json!({"id": null}));
            }
        }
        if a.quick && ui.button(crate::i18n::tr("Clear Quick Collection")).clicked() {
            let _ = app.run("album.clearQuick", json!({}));
        }
        if a.is_smart() && ui.button(crate::i18n::tr("Edit Smart Collection…")).clicked() {
            // the same editor as `dialog.smartAlbum` (older smart albums keep their filter fields;
            // it works on the rule set, upgraded)
            let _ = app.run("dialog.smartAlbum", json!({"id": a.id.0}));
        }
        if a.is_smart() && ui.button(crate::i18n::tr("Update Rules from Current Filter")).clicked() {
            // refused when the view would make the album test itself: say why
            if let Err(e) = app.run("album.setRules", json!({"id": a.id.0, "fromView": true})) {
                app.toast(ui.ctx(), e);
            }
        }
        if !a.folder {
            // export: show the album, select its photos, then the dialog / a preset
            let show_all = |app: &mut LccApp| {
                let _ = app.run("library.source", json!({"kind": "album", "id": a.id.0}));
                let _ = app.run("library.selectAll", json!({}));
            };
            let has_photos = app.session.catalog.album_count(a.id) > 0;
            if ui.add_enabled(has_photos, egui::Button::new(crate::i18n::tr("Export Collection…"))).clicked() {
                show_all(app);
                let _ = app.run("file.export", json!({}));
            }
            ui.add_enabled_ui(has_photos, |ui| {
                crate::widgets::menu_button(ui, crate::i18n::tr("Export Collection with Preset"), |ui| {
                    for (p, _) in app.session.all_export_presets() {
                        if ui.button(&p.name).clicked() {
                            show_all(app);
                            if let Err(e) = app.run("app.export", json!({"preset": p.name, "background": true})) {
                                app.toast(ui.ctx(), e);
                            }
                        }
                    }
                });
            });
            ui.separator();
        }
        // move into another folder (not into itself or one of its own subfolders)
        let mut folders: Vec<(u64, String)> =
            app.session.catalog.albums().filter(|f| f.folder && !is_within(app, f.id, a.id)).map(|f| (f.id.0, f.name.clone())).collect();
        folders.sort_by_key(|(_, n)| n.to_lowercase());
        super::collections::export_item(app, ui, a.id.0, &a.name);
        // how this collection syncs with other devices: Settings ▸ Sync
        #[cfg(not(target_arch = "wasm32"))]
        if a.smart.is_none() && !a.quick {
            let b = ui.button(crate::i18n::tr("Sync…"));
            crate::widgets::register(ui.ctx(), format!("menu:albumSync:{}", a.id.0), b.rect);
            if b.clicked() {
                let _ = app.run("app.settings", json!({"tab": "sync"}));
                ui.close();
            }
        }
        if a.folder {
            super::collections::import_item(app, ui, Some(a.id.0));
        }
        crate::widgets::menu_button(ui, crate::i18n::tr("Move to"), |ui| {
            if ui.add_enabled(a.parent.is_some(), egui::Button::new(crate::i18n::tr("Top Level"))).clicked() {
                let _ = app.run("album.move", json!({"id": a.id.0, "parent": null}));
            }
            for (fid, name) in &folders {
                if ui.add_enabled(a.parent.map(|p| p.0) != Some(*fid), egui::Button::new(name)).clicked() {
                    let _ = app.run("album.move", json!({"id": a.id.0, "parent": fid}));
                }
            }
        });
        if ui.button(crate::i18n::tr("Rename…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::RenameAlbum { id: a.id.0, name: a.name.clone() });
        }
        let delete = ui.button(crate::i18n::tr("Delete"));
        register(ui.ctx(), format!("albumDelete:{}", a.id.0), delete.rect);
        let delete =
            delete.on_hover_text(crate::i18n::tr("Photos stay in All Photos. Deleting a folder also deletes its albums. You can undo this."));
        if delete.clicked() {
            if let Err(e) = app.run("album.delete", json!({"id": a.id.0})) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
    });
}

/// Whether `id` is `ancestor` or lies inside it.
fn is_within(app: &LccApp, id: lcc_catalog::AlbumId, ancestor: lcc_catalog::AlbumId) -> bool {
    let mut cur = Some(id);
    let mut guard = 0;
    while let Some(c) = cur {
        if c == ancestor {
            return true;
        }
        cur = app.session.catalog.album(c).and_then(|x| x.parent);
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    false
}

// "Folders" (drawn in `show`): where on disk the library's photos were imported from, with photo counts (see
// `lcc_catalog::folders`). A click makes that folder the source, like an album or a Local
// folder: its photos and those of the folders inside it fill the grid. The triangle opens a
// level. Only folders holding imported photos are listed; every folder on disk is under Local.

/// A plain collapsible section header (Classic's panels): the title and its disclosure triangle;
/// a click folds it. Returns the header's rect and whether the section is open.
fn sidebar_section_header(app: &mut LccApp, ui: &mut egui::Ui, id: &str, title: &str) -> (Rect, bool) {
    let t = Tokens::get(ui.ctx());
    let title = crate::i18n::tr(title);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    register(ui.ctx(), format!("sidebarSection:{id}"), r);
    let section = crate::sidebar::SidebarSection::from_id(id);
    if resp.clicked()
        && let Some(s) = section
    {
        app.ui.sidebar.toggle_collapsed(s);
    }
    let open = !app.ui.sidebar_section_collapsed(id);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    let text = ui.painter().text(pos2(r.left() + 18.0, r.center().y), Align2::LEFT_CENTER, title, t.semibold(13.5), t.text_label);
    let c = pos2(text.right() + 10.0, r.center().y);
    let col = if resp.hovered() { t.text } else { t.text_dim };
    let pts = if open {
        vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
    } else {
        vec![c + vec2(-2.0, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 4.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
    (r, open)
}

/// Classic's Local panel: the folders on disk, shown in place without being imported, under a line saying so.
pub(crate) fn local_body(app: &mut LccApp, ui: &mut egui::Ui) {
    let Some(now) = local_now(app, ui) else { return };
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.add_space(18.0);
        let note = crate::i18n::tr("Shown in place, not in the catalog.");
        ui.add(egui::Label::new(egui::RichText::new(note).color(t.text_dim).size(11.5)).wrap());
    });
    ui.add_space(4.0);
    local_rows(app, ui, &now);
    ui.add_space(6.0);
}

/// Whenever the shown folder changes (a click, an agent, a rename or its undo), open the rows
/// above it so it is on screen; folding one by hand afterwards sticks until the choice changes.
pub(crate) fn reveal_chosen(app: &mut LccApp, ui: &egui::Ui, tree: &[FolderNode]) {
    let shown = app.session.library_folder.clone().filter(|_| app.session.source == LibrarySource::LibraryFolder);
    let chosen = shown.filter(|c| !lcc_catalog::query::folder_key(c).is_empty());
    let seen = egui::Id::new(LIBFOLDER_REVEALED);
    let now = chosen.as_deref().map(lcc_catalog::query::folder_key);
    if ui.data(|d| d.get_temp::<Option<String>>(seen)) == Some(now.clone()) {
        return;
    }
    ui.data_mut(|d| d.insert_temp(seen, now));
    let Some(chosen) = chosen else { return };
    fn open_above(app: &mut LccApp, ui: &egui::Ui, nodes: &[FolderNode], chosen: &str) {
        for n in nodes {
            if lcc_catalog::query::folder_within(chosen, &n.path) && !same_folder(chosen, &n.path) {
                let folder_k = lcc_catalog::query::folder_key(&n.path);
                let key = format!("libfolder:{folder_k}");
                let open_id = egui::Id::new(("libfolder-open", folder_k));
                set_tree_open(app, ui, &key, open_id, true);
                open_above(app, ui, &n.children, chosen);
            }
        }
    }
    open_above(app, ui, tree, &chosen);
}

/// The Folders tree as the sidebar shows it: a folder that holds no photo itself, has no colour
/// label and leads to one folder only (`Users` → `me` → `Pictures` on the way to an imported
/// folder) gives its row to that folder, so an imported folder is listed under its own name
/// instead of under every folder from the disk down to it (the full path is the row's hover
/// text). Folders listed side by side with the same name say which folder each is in:
/// `trip (2024)`, `trip (2025)`. Disk rows stay as they are.
/// A folder in `shown` (folder keys, added by Show Parent Folder) always keeps its own row.
pub(crate) fn sidebar_folders(nodes: Vec<FolderNode>, shown: &std::collections::BTreeSet<String>) -> Vec<FolderNode> {
    shape_folders(nodes, true, shown)
}

/// Only the chain right below a disk is lifted (`lift`): once a folder is listed, every folder
/// inside it keeps its own row, so `2026` → `January` → `31st` nests as it does on disk.
fn shape_folders(mut nodes: Vec<FolderNode>, lift: bool, shown: &std::collections::BTreeSet<String>) -> Vec<FolderNode> {
    let shape = |mut n: FolderNode| {
        while lift
            && !shown.contains(&lcc_catalog::query::folder_key(&n.path))
            && !n.volume
            && n.own == 0
            && n.label.is_none()
            && n.children.len() == 1
        {
            let Some(child) = n.children.pop() else { break };
            n = child;
        }
        n.children = shape_folders(std::mem::take(&mut n.children), n.volume, shown);
        n
    };
    nodes = nodes.into_iter().map(shape).collect();
    disambiguate(&mut nodes);
    nodes.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    nodes
}

/// Rows side by side with the same name (ignoring case) add the name of the folder each is in;
/// if that is the same too, the whole path.
fn disambiguate(nodes: &mut [FolderNode]) {
    let clash = |nodes: &[FolderNode], i: usize| {
        let n = &nodes[i];
        !n.volume && nodes.iter().enumerate().any(|(j, m)| j != i && !m.volume && m.name.to_lowercase() == n.name.to_lowercase())
    };
    let clashing: Vec<usize> = (0..nodes.len()).filter(|&i| clash(nodes, i)).collect();
    for &i in &clashing {
        let parent = std::path::Path::new(&nodes[i].path).parent().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string());
        if let Some(parent) = parent {
            nodes[i].name = format!("{} ({parent})", nodes[i].name);
        }
    }
    let still: Vec<usize> = clashing.into_iter().filter(|&i| clash(nodes, i)).collect();
    for i in still {
        nodes[i].name = nodes[i].path.clone();
    }
}

/// The Folders row that stands for folder `dir`, however it is spelled. `None` when no row is that
/// folder.
pub(crate) fn folder_row_of<'a>(tree: &'a [FolderNode], dir: &str) -> Option<&'a FolderNode> {
    // only the rows that hold `dir` are opened (the startup disk's holds the other disks' paths
    // too, so more than one may), and the tree is at most `MAX_DEPTH` deep
    tree.iter()
        .filter(|n| lcc_catalog::query::folder_within(dir, &n.path))
        .find_map(|n| if same_folder(&n.path, dir) { Some(n) } else { folder_row_of(&n.children, dir) })
}

fn folder_rows(app: &mut LccApp, ui: &mut egui::Ui, nodes: &[FolderNode], indent: f32) {
    for n in nodes {
        let folder_k = lcc_catalog::query::folder_key(&n.path);
        let key = format!("libfolder:{folder_k}");
        let open_id = egui::Id::new(("libfolder-open", folder_k));
        // a disk starts open, and so does a folder that holds nothing itself and leads to one
        // folder (`Users` → `me`): the first row where the library branches is what you look for
        let leads_on = n.own == 0 && n.children.len() == 1;
        let mut open = tree_open(app, ui, &key, open_id, n.volume || leads_on);
        // a row whose path would cover other disks' photos too only opens and closes
        let name = if n.selectable { n.name.clone() } else { crate::i18n::tr("This Computer").to_string() };
        let id = format!("libfolder:{}", n.path);
        if !out_of_sight(app, ui, &id, &name, Some(n.count), indent, n.label.is_some()) {
            folder_row(app, ui, n, &id, &name, indent, &key, open_id, &mut open);
        }
        if open && !n.children.is_empty() {
            folder_rows(app, ui, &n.children, indent + INDENT_STEP);
        }
    }
}

/// One drawn row of [`folder_rows`], shown as `name`; `open_id` and `open` are its triangle's.
#[allow(clippy::too_many_arguments)]
fn folder_row(app: &mut LccApp, ui: &mut egui::Ui, n: &FolderNode, id: &str, name: &str, indent: f32, key: &str, open_id: egui::Id, open: &mut bool) {
    let sel = n.selectable
        && app.session.source == LibrarySource::LibraryFolder
        && app.session.library_folder.as_deref().is_some_and(|f| same_folder(f, &n.path));
    // the label is said with the name, as it is shown beside it
    let spoken = match n.label {
        Some(l) => format!("{name}, {}", crate::i18n::color_label(&app.session.catalog, l)),
        None => name.to_string(),
    };
    let mark = n.label.map(crate::theme::label_color);
    // a poll found files the library doesn't have yet (new files: ask): "N new" beside the name
    let badged = lcc_engine::folder_watch::new_in(&app.session, &n.path).map(|k| format!("{name} · {k} {}", crate::i18n::tr("new")));
    let (name, spoken) = match &badged {
        Some(b) => (b.as_str(), format!("{spoken}, {b}")),
        None => (name, spoken),
    };
    let resp = row_sensed(app, ui, id, Icon::Folder, name, Some(&spoken), Some(n.count), sel, indent, Sense::click(), mark);
    folder_drop_target(app, ui, &resp, n, key, open_id, open);
    let mut toggled = false;
    if !n.children.is_empty() {
        let tr = tree_toggle(app, ui, &resp, indent, key, open_id, open, format!("libraryFolderToggle:{}", n.path));
        toggled = tr.clicked();
        // the triangle sits on the row and takes its clicks: the menu opens from it too
        row_menu(app, &tr, n);
    }
    let tip = if n.children.is_empty() { n.path.clone() } else { format!("{} ({})", n.path, crate::i18n::tr("includes the folders inside it")) };
    let resp = resp.on_hover_text(tip);
    if resp.clicked() && !toggled {
        if n.selectable {
            // a source like an album or a Local folder: it replaces what the grid showed
            let _ = app.run("library.source", json!({"kind": "libraryFolder", "path": n.path}));
        } else {
            *open = !*open;
            set_tree_open(app, ui, key, open_id, *open);
        }
    }
    row_menu(app, &resp, n);
}

/// How long photos dragged over a closed Folders row wait before it opens, so a folder inside it
/// can be reached.
const SPRING_OPEN_SECONDS: f64 = 0.7;

/// A Folders row while photos are dragged from the grid: a release moves their files there on
/// disk (`photo.moveToFolder`, one undo step). The row lights up and the dragged label says where
/// they will go, as the same gesture onto an album only adds them to it. Held over a closed row,
/// it opens. Photos that can't go there leave it dark (a row covering several disks; Local photos,
/// not in the library) and a release says why; photos already there say so.
fn folder_drop_target(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, n: &FolderNode, key: &str, open_id: egui::Id, open: &mut bool) {
    let Some(ids) = app.ui.dragging_photos.clone() else { return };
    // only where the row can be seen: not scrolled out of the sidebar or beneath the pinned header
    if !pointer_over_row(ui, resp.rect) {
        return;
    }
    spring_open(app, ui, n, key, open_id, open);
    let name = std::path::Path::new(&n.path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| n.name.clone());
    let photos: Vec<_> = ids.iter().filter_map(|id| app.session.catalog.photo(lcc_catalog::PhotoId(*id))).collect();
    let refused = if !n.selectable {
        Some(crate::i18n::tr("This row covers more than one disk: drop on a folder inside it"))
    } else if photos.iter().any(|p| p.local) {
        Some(crate::i18n::tr("Local photos aren't in the catalog yet: Add to My Photos first, then move them"))
    } else {
        None
    };
    let here = |p: &lcc_catalog::Photo| match &p.source {
        lcc_catalog::Source::File { path } => std::path::Path::new(path).parent().is_some_and(|d| same_folder(&d.to_string_lossy(), &n.path)),
        _ => false,
    };
    let all_here = !photos.is_empty() && photos.iter().all(|p| here(p));
    let count = photos.len();
    let released = ui.input(|i| i.pointer.any_released());
    let hint = match (&refused, all_here) {
        (Some(_), _) => crate::i18n::tr("Can't be moved here").to_string(),
        (None, true) => crate::i18n::tr_format!("Already in “{}”", name),
        (None, false) => crate::i18n::tr_format!("Move {n} photo{} to “{}”", if count == 1 { "" } else { "s" }, name, n = count),
    };
    super::grid::drag_hint(ui.ctx(), hint);
    if let Some(why) = refused {
        ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        if released {
            app.toast(ui.ctx(), why);
            app.ui.dragging_photos = None;
        }
        return;
    }
    if all_here {
        if released {
            app.ui.dragging_photos = None;
        }
        return;
    }
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_stroke(resp.rect.shrink2(vec2(8.0, 1.0)), 4.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
    if released {
        app.ui.dragging_photos = None;
        // a drop moves files on disk: asked first (as Lightroom Classic does), unless the person
        // said not to; the menu's Move Selected Photos Here is asked for by name and goes at once
        if app.ui.settings.ask_before_move {
            let library = app.session.library_generation();
            app.ui.dialog = Some(crate::state::Dialog::MovePhotos { ids, folder: n.path.clone(), name, dont_ask: false, library });
        } else {
            move_photos_here(app, ui.ctx(), Some(&ids), &n.path, &name);
        }
    }
}

/// Photos dragged over the closed row `n` for [`SPRING_OPEN_SECONDS`] open it (a row with folders
/// inside it). Restarts on each row the pointer comes to, and when it comes back to one after
/// leaving it (or after another drag).
fn spring_open(app: &mut LccApp, ui: &egui::Ui, n: &FolderNode, key: &str, open_id: egui::Id, open: &mut bool) {
    if *open || n.children.is_empty() {
        return;
    }
    let spring_key = egui::Id::new("folder-spring-open");
    let (now, frame) = (ui.input(|i| i.time), ui.ctx().cumulative_frame_nr());
    // held without a break: over this row last frame too
    let since = match ui.data(|d| d.get_temp::<(String, f64, u64)>(spring_key)) {
        Some((path, since, seen)) if path == n.path && seen.saturating_add(1) >= frame => since,
        _ => now,
    };
    ui.data_mut(|d| d.insert_temp(spring_key, (n.path.clone(), since, frame)));
    if now - since >= SPRING_OPEN_SECONDS {
        *open = true;
        set_tree_open(app, ui, key, open_id, true);
        ui.data_mut(|d| d.remove::<(String, f64, u64)>(spring_key));
    } else {
        // keep the clock running while the pointer rests
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Move photos (`ids`, else the selection) into the folder `path` on disk (`photo.moveToFolder`).
/// Photos that leave the view leave the selection, so nothing done next (Delete, an edit) reaches
/// photos no longer on screen. How many moved, or why none did.
pub(crate) fn move_photos(app: &mut LccApp, ids: Option<&[u64]>, path: &str) -> Result<u64, String> {
    let mut params = json!({"folder": path});
    if let Some(ids) = ids {
        params["ids"] = json!(ids);
    }
    let r = app.run("photo.moveToFolder", params)?;
    let visible: std::collections::HashSet<lcc_catalog::PhotoId> = app.session.visible_cloned().into_iter().collect();
    let sel = &mut app.session.selection;
    sel.ids.retain(|id| visible.contains(id));
    if sel.active.is_some_and(|a| !visible.contains(&a)) {
        sel.active = sel.ids.first().copied();
    }
    Ok(r["moved"].as_u64().unwrap_or(0))
}

/// What a move of `n` photos into the folder `name` says: how many moved and that Undo moves them
/// back, or that they were there already.
pub(crate) fn moved_text(n: u64, name: &str) -> String {
    if n == 0 {
        crate::i18n::tr_format!("Already in “{}”", name)
    } else {
        crate::i18n::tr_format!("Moved {n} photo{} to “{}”. Edit → Undo moves them back.", if n == 1 { "" } else { "s" }, name, n = n)
    }
}

/// [`move_photos`], saying how it went in a toast.
fn move_photos_here(app: &mut LccApp, ctx: &egui::Context, ids: Option<&[u64]>, path: &str, name: &str) {
    match move_photos(app, ids, path) {
        Ok(n) => app.toast(ctx, moved_text(n, name)),
        Err(e) => app.toast(ctx, e),
    }
}

/// The context menu of a Folders row: a folder's own, a disk's, none for the startup disk.
fn row_menu(app: &mut LccApp, resp: &egui::Response, n: &FolderNode) {
    if !n.volume {
        disk_folder_menu(app, resp, FolderRow::Library, &n.path, Some(n));
    } else if n.path != "/" {
        disk_folder_menu(app, resp, FolderRow::Disk, &n.path, Some(n));
    }
}

/// Locate Missing Files… of a Folders row: choose where this folder moved, then let the existing
/// background finder relink only this folder's photos (including the folders inside it).
fn locate_missing_item(app: &mut LccApp, ui: &mut egui::Ui, n: &FolderNode) {
    let r = ui.button(crate::i18n::tr("Locate Missing Files…"));
    register(ui.ctx(), "folderLocateMissing", r.rect);
    if r.clicked() {
        if let Err(e) = app.run("file.findMissing", json!({"within": n.path})) {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    }
}

/// The node Synchronize Folder… of a Local folder `path` works on, with the name its dialog shows:
/// the Folders row for that very folder, else (a folder above rows) the folder itself, as a folder
/// holding other folders. `None` when no library photo lies in `path` or inside it. Reads the
/// cached Folders tree only; the disk is not touched.
pub(crate) fn library_node_for(tree: &[FolderNode], path: &str) -> Option<(FolderNode, String)> {
    use lcc_catalog::query::folder_within;
    fn exact<'a>(tree: &'a [FolderNode], path: &str) -> Option<&'a FolderNode> {
        tree.iter().find_map(|n| if same_folder(&n.path, path) { Some(n) } else { exact(&n.children, path) })
    }
    fn any_inside(tree: &[FolderNode], path: &str) -> bool {
        tree.iter().any(|n| (n.count > 0 && folder_within(&n.path, path)) || any_inside(&n.children, path))
    }
    if let Some(n) = exact(tree, path).filter(|n| n.count > 0) {
        return Some((n.clone(), lcc_catalog::folders::folder_label(&n.path)));
    }
    if !any_inside(tree, path) {
        return None;
    }
    let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let node = FolderNode {
        name: name.clone(),
        path: path.to_string(),
        count: 0,
        own: 0,
        volume: false,
        selectable: false,
        label: None,
        children: Vec::new(),
    };
    Some((node, name))
}

/// A poll's "N new" (issue #123) on a folder's menu: adds them with Synchronize Folder, which
/// shows them first (widget `folderAddNew`). Nothing when the folder has none.
fn add_new_item(app: &mut LccApp, ui: &mut egui::Ui, n: &FolderNode, name: &str) {
    if let Some(k) = lcc_engine::folder_watch::new_in(&app.session, &n.path) {
        let r = ui.button(crate::i18n::tr_format!("Add {n} New Photos…", n = k));
        crate::widgets::register(ui.ctx(), "folderAddNew", r.rect);
        if r.clicked() {
            if let Err(e) = crate::sync::open(app, &n.path, name, n.volume || !n.selectable) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
    }
}

/// Synchronize Folder… of a folder's menu (widget `folderSynchronize`): opens its dialog, which
/// scans the folder in the background. `target`: the Folders node it works on and the name its
/// dialog shows; `None` (a Local folder holding no library photo) draws it grayed out.
fn synchronize_item(app: &mut LccApp, ui: &mut egui::Ui, target: Option<(&FolderNode, &str)>) {
    let r = ui
        .add_enabled(target.is_some(), egui::Button::new(crate::i18n::tr("Synchronize Folder…")))
        .on_hover_text(crate::i18n::tr("Find photos added to or missing from this folder on disk, and XMP sidecars changed by other apps"))
        .on_disabled_hover_text(crate::i18n::tr("No photo in this folder is in the catalog yet"));
    crate::widgets::register(ui.ctx(), "folderSynchronize", r.rect);
    if r.clicked()
        && let Some((n, name)) = target
    {
        // a disk, or a folder holding disks, was chosen as such
        if let Err(e) = crate::sync::open(app, &n.path, name, n.volume || !n.selectable) {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    }
}

/// Why a Folders row can't be exported as it is shown, `None` when it can: it holds no photo, or
/// choosing it would show the photos of other disks too (see [`FolderNode::selectable`]), so
/// the grid could not be made to hold exactly this folder's photos.
pub(crate) fn folder_export_blocker(n: &FolderNode) -> Option<&'static str> {
    if !n.selectable {
        Some("This row covers more than one disk: export a folder inside it")
    } else if n.count == 0 {
        Some("No photo in this folder is in the catalog yet")
    } else {
        None
    }
}

/// Export Folder… and Export Folder with Preset ▸ of a Folders row (`with_preset`: the second):
/// like an album's, they show the folder as the Folders row does (its photos and those of the
/// folders inside it), select them all, then open the Export dialog or run the preset in the
/// background. Grayed out, with the reason, for a row that can't be shown as a source or holds no
/// photo. Widgets `folderExport`, `folderExportPresetMenu` and `folderExportPreset:<name>`.
fn export_folder_item(app: &mut LccApp, ui: &mut egui::Ui, n: &FolderNode, with_preset: bool) {
    let blocker = folder_export_blocker(n);
    let show_all = |app: &mut LccApp| -> Result<(), String> {
        app.run("library.source", json!({"kind": "libraryFolder", "path": n.path}))?;
        app.run("library.selectAll", json!({})).map(|_| ())
    };
    if !with_preset {
        let mut r = ui
            .add_enabled(blocker.is_none(), egui::Button::new(crate::i18n::tr("Export Folder…")))
            .on_hover_text(crate::i18n::tr("Exports this folder's photos and those in the folders inside it"));
        if let Some(why) = blocker {
            r = r.on_disabled_hover_text(crate::i18n::tr(why));
        }
        register(ui.ctx(), "folderExport", r.rect);
        if r.clicked() {
            if let Err(e) = show_all(app).and_then(|()| app.run("file.export", json!({})).map(|_| ())) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
        return;
    }
    let menu = ui.add_enabled_ui(blocker.is_none(), |ui| {
        crate::widgets::menu_button(ui, crate::i18n::tr("Export Folder with Preset"), |ui| {
            for (p, _) in app.session.all_export_presets() {
                let r = ui.button(&p.name);
                register(ui.ctx(), format!("folderExportPreset:{}", p.name), r.rect);
                if r.clicked() {
                    if let Err(e) = show_all(app).and_then(|()| app.run("app.export", json!({"preset": p.name, "background": true})).map(|_| ())) {
                        app.toast(ui.ctx(), e);
                    }
                    ui.close();
                }
            }
        })
    });
    let mut r = menu.inner.response;
    if let Some(why) = blocker {
        r = r.on_disabled_hover_text(crate::i18n::tr(why));
    }
    register(ui.ctx(), "folderExportPresetMenu", r.rect);
}

/// Set Color Label ▸ of a Folders row (`folder.label`; the submenu is widget `folderLabelMenu`,
/// its colours `folderLabel:<colour>`).
fn folder_label_menu(app: &mut LccApp, ui: &mut egui::Ui, n: &FolderNode) {
    let menu = crate::widgets::menu_button(ui, crate::i18n::tr("Set Color Label"), |ui| {
        if let Some(l) = crate::panels::grid::label_items(app, ui, n.label, "folderLabel") {
            if let Err(e) = app.run("folder.label", json!({"path": n.path, "label": crate::panels::grid::label_param(l)})) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
    });
    register(ui.ctx(), "folderLabelMenu", menu.response.rect);
}

/// Which row a folder's context menu is opened on; it decides the menu's groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderRow {
    /// A folder in the Folders tree.
    Library,
    /// A disk's row in the Folders tree.
    Disk,
    /// A Local row: `top` when it is a place at the top of Local, `transient` when that place is
    /// listed for this session only.
    Local { top: bool, transient: bool },
}

/// One row of a folder's context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderItem {
    ImportHere,
    AddToMyPhotos,
    AddNew,
    Synchronize,
    LocateMissing,
    UpdateLocation,
    ExportFolder,
    ExportFolderWithPreset,
    NewFolder,
    Rename,
    Move,
    MovePhotosHere,
    ColorLabel,
    Reveal,
    CopyPath,
    ShowParent,
    KeepInLocal,
    RemoveFromLocal,
    RemoveFromLibrary,
    RemoveDisk,
}

/// The groups of a folder's context menu, in the order they are drawn with a separator between
/// two: what it does with the library's photos, how it reorganizes the disk, how the row looks,
/// the way out to the file manager and the tree, and last what lists or removes it. Every row of
/// a folder takes the same order; a row leaves out only the groups that can't apply to it.
pub(crate) fn folder_menu_groups(row: FolderRow) -> &'static [&'static [FolderItem]] {
    use FolderItem::*;
    match row {
        FolderRow::Library => &[
            &[ImportHere, AddNew, Synchronize, LocateMissing, UpdateLocation, ExportFolder, ExportFolderWithPreset],
            &[NewFolder, Rename, Move, MovePhotosHere],
            &[ColorLabel],
            &[Reveal, CopyPath, ShowParent],
            &[RemoveFromLibrary],
        ],
        FolderRow::Disk => &[
            &[AddNew, Synchronize, LocateMissing, ExportFolder, ExportFolderWithPreset],
            &[NewFolder],
            &[ColorLabel],
            &[Reveal, CopyPath],
            &[RemoveDisk],
        ],
        FolderRow::Local { .. } => &[&[AddToMyPhotos, Synchronize], &[NewFolder, Rename, Move], &[Reveal, CopyPath], &[KeepInLocal, RemoveFromLocal]],
    }
}

/// The context menu of the folder at `path`, laid out by [`folder_menu_groups`]. `node`: its
/// Folders node (`None` on a Local row). A row that depends on the moment (nothing selected to
/// move here, no library photo to synchronize) is grayed out with the reason rather than left
/// out, so the menu keeps its shape; only what this host can't do (no folder picker, no file
/// manager) or what doesn't exist (no new photos found, no parent) is left out.
fn disk_folder_menu(app: &mut LccApp, resp: &egui::Response, row: FolderRow, path: &str, node: Option<&FolderNode>) {
    let groups = folder_menu_groups(row);
    crate::widgets::context_menu(resp, |ui| {
        let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        // what dialogs call it: the last two names, so same-named folders are told apart
        let label = lcc_catalog::folders::folder_label(path);
        let mut drawn = false;
        for group in groups {
            if !group.iter().any(|i| folder_item_available(app, *i, path, node)) {
                continue;
            }
            if drawn {
                ui.separator();
            }
            drawn = true;
            for &item in group.iter() {
                if folder_item_available(app, item, path, node) {
                    folder_item(app, ui, row, item, path, node, (&name, &label));
                }
            }
        }
        if row == FolderRow::Library {
            if drawn {
                ui.separator();
            }
            super::folders::subfolder_photos_item(app, ui);
        }
    });
}

/// Whether `item` is drawn at all: what this host can't do, a row that needs a Folders node when
/// it has none, or one with nothing to offer (no new photos, no parent folder) is left out.
fn folder_item_available(app: &LccApp, item: FolderItem, path: &str, node: Option<&FolderNode>) -> bool {
    match item {
        FolderItem::Move => app.services.pick_folder.is_some() || app.services.picker.is_some(),
        FolderItem::ImportHere => app.services.pick_folder.is_some(),
        FolderItem::UpdateLocation => app.services.pick_folder.is_some() && node.is_some(),
        FolderItem::Reveal => app.services.reveal.is_some(),
        FolderItem::AddNew => node.is_some() && lcc_engine::folder_watch::new_in(&app.session, path).is_some(),
        FolderItem::ShowParent => node.is_some() && parent_folder(path).is_some(),
        FolderItem::LocateMissing
        | FolderItem::ExportFolder
        | FolderItem::ExportFolderWithPreset
        | FolderItem::ColorLabel
        | FolderItem::RemoveFromLibrary
        | FolderItem::RemoveDisk => node.is_some(),
        _ => true,
    }
}

/// `path` as the OS spells it: its separators native (a path may carry `/` on Windows), and a
/// bare drive (`D:`, a disk's row) as the drive's root.
pub(crate) fn native_path(path: &str, sep: char) -> String {
    let mut spelled: String = path.chars().map(|c| if c == '/' { sep } else { c }).collect();
    if matches!(spelled.as_bytes(), [d, b':'] if d.is_ascii_alphabetic()) {
        spelled.push(sep);
    }
    spelled
}

/// The folder `path` is in, when it has one.
fn parent_folder(path: &str) -> Option<String> {
    std::path::Path::new(path).parent().map(|p| p.to_string_lossy().to_string()).filter(|p| !p.is_empty())
}

/// Draws one row of [`disk_folder_menu`] and does what it says when clicked. `names`: the
/// folder's own name on disk, and what dialogs call it.
fn folder_item(app: &mut LccApp, ui: &mut egui::Ui, row: FolderRow, item: FolderItem, path: &str, node: Option<&FolderNode>, names: (&str, &str)) {
    let (name, label) = names;
    // a disk is called by its own name, as in Synchronize and Remove Disk
    let label = match (row, node) {
        (FolderRow::Disk, Some(n)) => n.name.as_str(),
        _ => label,
    };
    match item {
        FolderItem::ImportHere => {
            let r = ui
                .add_enabled(crate::import::can_open(app), egui::Button::new(crate::i18n::tr("Import to This Folder…")))
                .on_hover_text(crate::i18n::tr("Copies the photos of a folder you choose into this folder; the review lets you change that"))
                .on_disabled_hover_text(crate::i18n::tr("An import is running"));
            register(ui.ctx(), "folderImportHere", r.rect);
            if r.clicked() {
                // the folder picker, then the Import review copying into this folder
                if let Err(e) = app.run("file.addFolder", json!({"destination": path})) {
                    app.toast(ui.ctx(), e);
                }
                ui.close();
            }
        }
        FolderItem::AddToMyPhotos => {
            let r = ui
                .add_enabled(crate::import::can_open(app), egui::Button::new(crate::i18n::tr("Add to My Photos…")))
                .on_hover_text(crate::i18n::tr("Reviews the photos in this folder, then adds them to the catalog where they are; no file is touched"))
                .on_disabled_hover_text(crate::i18n::tr("An import is running"));
            register(ui.ctx(), "folderAddToMyPhotos", r.rect);
            if r.clicked() {
                if let Err(e) = app.run("file.addFolder", json!({"path": path})) {
                    app.toast(ui.ctx(), e);
                }
                ui.close();
            }
        }
        FolderItem::AddNew => {
            if let Some(n) = node {
                add_new_item(app, ui, n, if row == FolderRow::Disk { &n.name } else { label });
            }
        }
        FolderItem::Synchronize => match (row, node) {
            (FolderRow::Disk, Some(n)) => synchronize_item(app, ui, Some((n, &n.name))),
            (_, Some(n)) => synchronize_item(app, ui, Some((n, label))),
            // a Local row: the Folders node for this folder, or for the library folders inside it
            (_, None) => {
                let library = app.caches.folder_tree(&app.session.catalog);
                let held = library_node_for(&library, path);
                synchronize_item(app, ui, held.as_ref().map(|(n, name)| (n, name.as_str())));
            }
        },
        FolderItem::LocateMissing => {
            if let Some(n) = node {
                locate_missing_item(app, ui, n);
            }
        }
        FolderItem::ExportFolder | FolderItem::ExportFolderWithPreset => {
            if let Some(n) = node {
                export_folder_item(app, ui, n, item == FolderItem::ExportFolderWithPreset);
            }
        }
        FolderItem::UpdateLocation => {
            let r = ui
                .button(crate::i18n::tr("Update Folder Location…"))
                .on_hover_text(crate::i18n::tr("The folder was moved or renamed outside the app: point its photos at the new place"));
            register(ui.ctx(), "folderUpdateLocation", r.rect);
            if r.clicked() {
                let picked = app.services.pick_folder.as_mut().and_then(|f| f());
                if let Some(to) = picked {
                    match app.run("folder.relocate", json!({"path": path, "to": to})) {
                        Ok(r) => app.toast(ui.ctx(), crate::i18n::tr_format!("{n} photos relinked", n = r["relinked"].as_u64().unwrap_or(0))),
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
                ui.close();
            }
        }
        FolderItem::NewFolder => {
            let r = ui.button(crate::i18n::tr("New Folder on Disk…"));
            register(ui.ctx(), "folderNewFolder", r.rect);
            if r.clicked() {
                app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                    title: crate::i18n::tr_format!("New Folder in “{name}”", name = label),
                    hint: "Folder name (created on disk)".into(),
                    value: String::new(),
                    command: "folder.create".into(),
                    params: json!({"parent": path}),
                    key: "name".into(),
                });
                ui.close();
            }
        }
        FolderItem::Rename => {
            let r = ui.button(crate::i18n::tr("Rename Folder…"));
            register(ui.ctx(), "folderRename", r.rect);
            if r.clicked() {
                app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                    title: crate::i18n::tr_format!("Rename “{name}”", name = label),
                    hint: "Folder name (renamed on disk; its photos follow)".into(),
                    value: name.to_string(),
                    command: "folder.rename".into(),
                    params: json!({"path": path}),
                    key: "name".into(),
                });
                ui.close();
            }
        }
        FolderItem::Move => {
            let r = ui.button(crate::i18n::tr("Move Folder To…"));
            let id = if matches!(row, FolderRow::Local { .. }) { format!("localFolderMove:{path}") } else { format!("folderMove:{path}") };
            register(ui.ctx(), id, r.rect);
            if r.clicked() {
                // the dialog runs off the UI thread where the host has a picker (upstream #3325); `folder.move`
                // then runs when it closes (`pick::poll`)
                let req = crate::pick::PickRequest::folder(crate::i18n::tr("Move Folder To…"));
                let params = json!({"path": path});
                let picked =
                    crate::pick::ask(app, "folder.move", &params, "into", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x]));
                if let crate::pick::Picked::Now(v) = picked
                    && let Some(into) = v.into_iter().next()
                {
                    match app.run("folder.move", json!({"path": path, "into": into})) {
                        Ok(r) => app.toast(ui.ctx(), crate::pick::answered_message("folder.move", &r).unwrap_or_default()),
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
                ui.close();
            }
        }
        FolderItem::MovePhotosHere => {
            let r = ui
                .add_enabled(!app.session.selection.ids.is_empty(), egui::Button::new(crate::i18n::tr("Move Selected Photos Here")))
                .on_hover_text(crate::i18n::tr("Moves the files and their XMP sidecars into this folder; never overwrites"))
                .on_disabled_hover_text(crate::i18n::tr("Select the photos to move in the grid first"));
            register(ui.ctx(), "folderMovePhotosHere", r.rect);
            if r.clicked() {
                move_photos_here(app, ui.ctx(), None, path, name);
                ui.close();
            }
        }
        FolderItem::ColorLabel => {
            if let Some(n) = node {
                folder_label_menu(app, ui, n);
            }
        }
        FolderItem::Reveal => {
            let r = ui.button(crate::i18n::tr(crate::menus::reveal_label()));
            register(ui.ctx(), "folderReveal", r.rect);
            if r.clicked() {
                if let Some(Err(e)) = app.services.reveal.as_mut().map(|f| f(path)) {
                    app.toast(ui.ctx(), e);
                }
                ui.close();
            }
        }
        FolderItem::CopyPath => {
            let r = ui.button(crate::i18n::tr("Copy Path"));
            register(ui.ctx(), "folderCopyPath", r.rect);
            if r.clicked() {
                ui.ctx().copy_text(native_path(path, std::path::MAIN_SEPARATOR));
                app.toast(ui.ctx(), crate::i18n::tr("Path copied"));
                ui.close();
            }
        }
        FolderItem::ShowParent => {
            let r = ui.button(crate::i18n::tr("Show Parent Folder"));
            register(ui.ctx(), "folderShowParent", r.rect);
            if r.clicked()
                && let Some(parent) = parent_folder(path)
            {
                // the parent gets a row of its own in the Folders list even when it holds no
                // photo itself; choosing it then opens the rows above it
                app.caches.show_folder(&parent);
                if let Err(e) = app.run("library.source", json!({"kind": "libraryFolder", "path": parent})) {
                    app.toast(ui.ctx(), e);
                }
                ui.close();
            }
        }
        FolderItem::KeepInLocal => {
            let (top, transient) = match row {
                FolderRow::Local { top, transient } => (top, transient),
                _ => (false, false),
            };
            // a place already kept at the top of Local has nothing to add
            let r = ui
                .add_enabled(!top || transient, egui::Button::new(crate::i18n::tr(if top { "Keep in Favorites" } else { "Add to Favorites" })))
                .on_hover_text(crate::i18n::tr("List this folder in Favorites from now on"))
                .on_disabled_hover_text(crate::i18n::tr("This folder is kept in Favorites already"));
            register(ui.ctx(), "folderKeepInLocal", r.rect);
            if r.clicked() {
                if let Err(e) = app.run("local.addRoot", json!({"path": path})) {
                    app.toast(ui.ctx(), e);
                }
                ui.close();
            }
        }
        FolderItem::RemoveFromLocal => {
            let top = matches!(row, FolderRow::Local { top: true, .. });
            let r = ui
                .add_enabled(top, egui::Button::new(crate::i18n::tr("Remove from Favorites")))
                .on_hover_text(crate::i18n::tr("Hides this shortcut only; the folder and its photos stay as they are"))
                .on_disabled_hover_text(crate::i18n::tr("Only a folder at the top of Favorites can be removed from it"));
            register(ui.ctx(), "folderRemoveFromLocal", r.rect);
            if r.clicked() {
                if let Err(e) = app.run("local.hide", json!({"path": path})) {
                    app.toast(ui.ctx(), e);
                }
                ui.close();
            }
        }
        FolderItem::RemoveFromLibrary | FolderItem::RemoveDisk => {
            let Some(n) = node else { return };
            let disk = item == FolderItem::RemoveDisk;
            let (text, hint) = if disk {
                ("Remove Disk from Catalog…", "Moves every photo imported from this disk to Recently Deleted; no file is touched")
            } else {
                ("Remove from Catalog…", "Moves the photos imported from this folder to Recently Deleted; no file is touched")
            };
            let r = ui.button(crate::i18n::tr(text)).on_hover_text(crate::i18n::tr(hint));
            register(ui.ctx(), "folderRemove", r.rect);
            if r.clicked() {
                let name = if disk { n.name.clone() } else { label.to_string() };
                app.ui.dialog = Some(crate::state::Dialog::RemoveFolder { path: path.to_string(), name, count: n.count, disk });
                ui.close();
            }
        }
    }
}

/// "Keywords": the library's keyword tree with photo counts (`a|b|c` keywords nest). A click
/// filters the grid by the keyword (children included), the triangle opens a level, and the
/// context menu renames, merges or deletes the keyword across the library.
fn keyword_rows(app: &mut LccApp, ui: &mut egui::Ui, nodes: &[KeywordNode], indent: f32) {
    for n in nodes {
        // leaves (most keywords) have no open state to look up
        let (str_key, open_id) = if !n.children.is_empty() {
            (Some(format!("keyword:{}", n.path.to_lowercase())), Some(egui::Id::new(("kw-open", n.path.to_lowercase()))))
        } else {
            (None, None)
        };
        let mut open = str_key.as_deref().zip(open_id).is_some_and(|(k, id)| tree_open(app, ui, k, id, false));
        let id = format!("keyword:{}", n.path);
        if !out_of_sight(app, ui, &id, &n.name, Some(n.count), indent, false) {
            keyword_row(app, ui, n, &id, indent, str_key.as_deref(), open_id, &mut open);
        }
        if open {
            keyword_rows(app, ui, &n.children, indent + INDENT_STEP);
        }
    }
}

/// One drawn row of [`keyword_rows`]; `open_id` and `open` are its triangle's when it has rows inside it.
fn keyword_row(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    n: &KeywordNode,
    id: &str,
    indent: f32,
    str_key: Option<&str>,
    open_id: Option<egui::Id>,
    open: &mut bool,
) {
    // chosen here or among the filter bar's keywords
    let f = &app.session.filter;
    let sel = f.keyword.iter().chain(&f.keywords).any(|k| k.eq_ignore_ascii_case(&n.path));
    let resp = row(app, ui, id, Icon::Tag, &n.name, Some(n.count), sel, indent);
    if let (Some(k), Some(id)) = (str_key, open_id) {
        tree_toggle(app, ui, &resp, indent, k, id, open, format!("keywordToggle:{}", n.path));
    }
    let resp = resp.on_hover_text(if n.children.is_empty() { n.path.clone() } else { format!("{} (includes the keywords below it)", n.path) });
    if resp.clicked() {
        let v = if sel { serde_json::Value::Null } else { json!(n.path) };
        browse_all_photos(app, !sel);
        // the Keyword List shows one keyword: it replaces the filter bar's several
        let _ = app.run("library.filter", json!({"keyword": v, "keywords": []}));
    }
    crate::widgets::context_menu(&resp, |ui| {
        let has_sel = app.session.active().is_some();
        if ui.add_enabled(has_sel, egui::Button::new(crate::i18n::tr("Add to Selected Photos"))).clicked() {
            let _ = app.run("photo.setMeta", json!({"addKeywords": [n.path]}));
        }
        if ui.add_enabled(has_sel, egui::Button::new(crate::i18n::tr("Remove from Selected Photos"))).clicked() {
            let _ = app.run("photo.setMeta", json!({"removeKeywords": [n.path]}));
        }
        ui.separator();
        if ui.button(crate::i18n::tr("Rename Keyword…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::RenameKeyword { from: n.path.clone(), to: n.path.clone() });
        }
        if ui.button(crate::i18n::tr("Merge into…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::MergeKeywords { from: vec![n.path.clone()], into: String::new() });
        }
        if ui.button(crate::i18n::tr("Delete Keyword")).clicked() {
            let _ = app.run("keyword.delete", json!({"keyword": n.path}));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::local_places;
    use crate::widgets::elide_head;

    /// Width = characters, so a limit of 12 is "12 characters".
    fn fit(text: &str, max: usize) -> String {
        elide_head(text, max as f32, |s| s.chars().count() as f32)
    }

    /// A label that fits is left alone; a long path loses its leading folders, not its end; a
    /// single name too long is cut at the end, on a character boundary.
    #[test]
    fn a_long_label_loses_its_leading_folders_not_its_end() {
        for (text, max, want) in [
            ("/Users/me/Pictures", 18, "/Users/me/Pictures"),
            ("/Users/me/Pictures/Lightroom", 20, "…/Pictures/Lightroom"),
            ("/Users/me/Pictures/Lightroom", 12, "…/Lightroom"),
            ("2024-summer-holiday", 8, "2024-su…"),
            ("2024-summer-holiday", 1, "…"),
            ("2024-summer-holiday", 0, "…"),
            ("/写真/夏休み旅行の記録", 6, "夏休み旅行…"),
        ] {
            assert_eq!(fit(text, max), want, "{text:?} in {max}");
        }
    }

    fn names(v: &[(String, String)]) -> Vec<&str> {
        v.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// A built-in location browsed (or picked) under another spelling of its path is the same
    /// row, not a second root — for every built-in, not just named ones.
    #[test]
    fn equivalent_paths_are_one_local_location() {
        for (home, sep) in [("D:\\Example", '\\'), ("D:/Example", '/'), ("/home/example", '/')] {
            let builtin: Vec<(String, String)> =
                ["Pictures", "Desktop", "Downloads"].iter().map(|s| (s.to_string(), format!("{home}{sep}{s}"))).collect();
            for (_, p) in builtin.clone() {
                // the browsed spelling: other separators, a trailing one, a different drive-letter case
                let flipped = p.replace(['/', '\\'], if sep == '/' { "\\" } else { "/" });
                for browsing in [flipped.clone(), format!("{p}{sep}"), p.replacen("D:", "d:", 1)] {
                    let l = local_places(builtin.clone(), std::slice::from_ref(&browsing), Some(&browsing), None, &[]);
                    assert_eq!(names(&l.places), ["Pictures", "Desktop", "Downloads"], "{p} browsed as {browsing}");
                    assert_eq!(l.places[l.owner.unwrap()].1, p, "the built-in row is the one highlighted");
                }
                // hiding under one spelling hides the other
                let l = local_places(builtin.clone(), &[], None, None, std::slice::from_ref(&flipped));
                assert_eq!(l.places.len(), 2, "hidden {flipped}");
            }
        }
        // a different folder is still added
        let l = local_places(vec![("Pictures".into(), "/home/example/Pictures".into())], &[], Some("/home/example/Pictures2"), None, &[]);
        assert_eq!(names(&l.places), ["Pictures", "Pictures2"]);
    }

    /// A folder inside a kept root is shown inside that root's tree: the root stays (its other
    /// subfolders stay reachable) and no second root appears; other kept roots stay too.
    #[test]
    fn browsing_below_a_kept_root_keeps_the_root() {
        let photos = "/data/Photos".to_string();
        let other = "/data/Scans".to_string();
        let saved = [photos.clone(), other.clone()];
        let builtin = || vec![("Home".to_string(), "/home/example".to_string())];
        for browsing in ["/data/Photos/2026/20260101", "/data/Photos/2026", "/data/Photos", "D:\\x"] {
            let l = local_places(builtin(), &saved, Some(browsing), None, &[]);
            let roots: Vec<&str> = l.places.iter().map(|(_, p)| p.as_str()).collect();
            if browsing.starts_with("/data") {
                assert_eq!(roots, ["/home/example", "/data/Photos", "/data/Scans"], "{browsing}");
                assert_eq!(l.owner, Some(1), "the Photos tree opens down to {browsing}");
                assert_eq!(l.browse_root, None);
            } else {
                assert_eq!(roots, ["/home/example", "/data/Photos", "/data/Scans", "D:\\x"], "a folder outside them gets a row");
            }
        }
        // the innermost containing root is the one that opens
        let l = local_places(builtin(), &["/home/example/Pictures".into()], Some("/home/example/Pictures/Trip"), None, &[]);
        assert_eq!(l.owner, Some(1));
    }

    /// A browsed folder outside every kept root gets a session row, which stays while browsing
    /// below it and gives way when browsing moves elsewhere.
    #[test]
    fn session_root_stays_while_browsing_below_it() {
        let l = local_places(Vec::new(), &[], Some("/t/base"), None, &[]);
        assert_eq!(l.browse_root.as_deref(), Some("/t/base"));
        let l = local_places(Vec::new(), &[], Some("/t/base/Trip/Day 1"), l.browse_root.as_deref(), &[]);
        assert_eq!((l.places.len(), l.browse_root.as_deref(), l.owner), (1, Some("/t/base"), Some(0)), "{l:?}");
        let l = local_places(Vec::new(), &[], Some("/elsewhere"), l.browse_root.as_deref(), &[]);
        assert_eq!(l.browse_root.as_deref(), Some("/elsewhere"));
        // hidden: no row at all
        let l = local_places(Vec::new(), &[], Some("/t/base"), None, &["/t/base/".into()]);
        assert!(l.places.is_empty() && l.browse_root.is_none());
    }

    /// A hidden folder inside a listed one (a kept folder beneath Home) is not revealed in that
    /// one's tree while it is browsed, nor are the folders below it; hiding Home itself still
    /// lets Pictures open down to a folder browsed inside it.
    #[test]
    fn hidden_folder_is_not_revealed_in_an_outer_tree() {
        let builtin = || vec![("Pictures".to_string(), "/home/example/Pictures".to_string()), ("Home".to_string(), "/home/example".to_string())];
        let kept = ["/home/example/AppData/Temp/lc".to_string()];
        let l = local_places(builtin(), &kept, Some("/home/example/AppData/Temp/lc"), None, &[]);
        assert_eq!(l.owner, Some(2), "shown as its own kept row");
        for browsing in ["/home/example/AppData/Temp/lc", "/home/example/AppData/Temp/lc/Day 1"] {
            let l = local_places(builtin(), &kept, Some(browsing), None, &["/home/example/AppData/Temp/lc/".into()]);
            assert_eq!(names(&l.places), ["Pictures", "Home"], "{browsing}");
            assert_eq!((l.owner, l.browse_root.as_deref()), (None, None), "Home does not open down to {browsing}");
        }
        let l = local_places(builtin(), &[], Some("/home/example/Pictures/Trip"), None, &["/home/example".into()]);
        assert_eq!((names(&l.places), l.owner), (vec!["Pictures"], Some(0)));
    }

    /// A folder tree leaves out hidden subfolders: a leading dot everywhere, and on Windows the
    /// hidden attribute, so Home's tree does not list AppData and open down into its temp folder.
    #[test]
    fn subfolders_leave_out_hidden_folders() {
        let dir = lcc_testkit::TempDir::new("lc-ui-hidden-subfolders");
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["Shown", ".dot", "Hidden"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        let expected = if cfg!(windows) {
            let status = std::process::Command::new("attrib").arg("+h").arg(dir.join("Hidden")).status().unwrap();
            assert!(status.success(), "attrib +h: {status}");
            vec!["Shown"]
        } else {
            vec!["Hidden", "Shown"]
        };
        let listed = super::list_subfolders(&dir.to_string_lossy());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(listed.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), expected);
    }
}
