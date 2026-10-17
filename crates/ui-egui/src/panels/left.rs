//! The left "My Photos" panel: library sources, albums tree, and date groups.

use egui::{Align2, Rect, Sense, pos2, vec2};
use lightcraft_catalog::{Album, AlbumId, FolderNode, KeywordNode};
use lightcraft_engine::LibrarySource;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::sidebar::SidebarSection;
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

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

/// A row says how wide it needs to be.
fn note_width(ui: &egui::Ui, w: f32) {
    ui.data_mut(|d| {
        let m = d.get_temp_mut_or_insert_with::<f32>(egui::Id::new("left-content-width-next"), || 0.0);
        *m = m.max(w);
    });
}

fn row(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    id: &str,
    icon: Icon,
    label: &str,
    count: Option<usize>,
    selected: bool,
    indent: f32,
) -> egui::Response {
    row_named(app, ui, id, icon, label, None, count, selected, indent)
}

/// [`row`] whose spoken name is `spoken` when the painted `label` is a shortened form of it.
fn row_named(
    app: &mut LightcraftApp,
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
    app: &mut LightcraftApp,
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
    let label = if matches!(id, "all" | "recentlyAdded" | "picks" | "missing" | "recentlyDeleted") { crate::i18n::tr(label) } else { label };
    let t = Tokens::get(ui.ctx());
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
fn out_of_sight(app: &LightcraftApp, ui: &mut egui::Ui, id: &str, label: &str, count: Option<usize>, indent: f32, mark: bool) -> bool {
    let r = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), ROW_HEIGHT));
    if ui.is_rect_visible(r) || scroll_asked(ui, id) {
        return false;
    }
    ui.allocate_space(r.size());
    register(ui.ctx(), format!("source:{id}"), r);
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
    let c = pos2(row.rect.left() + 10.0 + indent, row.rect.center().y);
    let tri = Rect::from_center_size(c, vec2(16.0, 16.0));
    let tr = ui.interact(tri, id, Sense::click());
    register(ui.ctx(), widget, tri);
    let col = if tr.hovered() { t.text } else { t.text_dim };
    paint(ui.painter(), Rect::from_center_size(c, vec2(16.0, 16.0)), if open { Icon::ChevronDown } else { Icon::ChevronRight }, col);
    tr
}

/// Whether the tree row `key` is open: what the user last chose this session, else `default`.
fn tree_open(ui: &egui::Ui, key: egui::Id, default: bool) -> bool {
    ui.data(|d| d.get_temp(key)).unwrap_or(default)
}

/// Open or close the tree row `key` from now on.
fn set_tree_open(ui: &egui::Ui, key: egui::Id, open: bool) {
    ui.data_mut(|d| d.insert_temp(key, open));
}

/// The chevron of a tree row that has rows inside it (Albums' folders, Local, By Date, Folders,
/// Keywords): a click flips `open` and remembers it under `key`. Returns the chevron's response
/// (it sits on the row and takes its clicks, so a row's context menu is attached to it too).
fn tree_toggle(ui: &mut egui::Ui, row: &egui::Response, indent: f32, key: egui::Id, open: &mut bool, widget: String) -> egui::Response {
    let tri = disclosure_chevron(ui, row, indent, *open, key.with("triangle"), widget);
    if tri.clicked() {
        *open = !*open;
        set_tree_open(ui, key, *open);
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
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    section: SidebarSection,
    rows: usize,
    body: impl FnOnce(&mut LightcraftApp, &mut egui::Ui),
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
fn pinned_header(app: &mut LightcraftApp, ui: &mut egui::Ui, placed: &[PlacedSection], view_top: f32) {
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
fn section_drag(app: &mut LightcraftApp, ui: &mut egui::Ui, placed: &[PlacedSection]) {
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
pub fn section_drag_feedback(app: &mut LightcraftApp, ctx: &egui::Context) {
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
fn section_header(app: &mut LightcraftApp, ui: &mut egui::Ui, section: SidebarSection, r: Rect, rows: usize, pinned: bool) -> HeaderOutcome {
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
    resp.context_menu(|ui| sections_menu(app, ui, Some(section)));
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
    Run(fn(&mut LightcraftApp, &egui::Context)),
    /// Opens a menu with these items.
    Menu(fn(&mut LightcraftApp, &mut egui::Ui)),
}

/// The button a section has at the end of its header, if it has one. This is the one place that
/// says so: the header (its own and the pinned copy) draws the button at the visible edge, keeps
/// its clicks from folding the section and leaves it room beside a folded section's count.
fn header_button(section: SidebarSection) -> Option<HeaderButton> {
    match section {
        SidebarSection::Albums => {
            Some(HeaderButton { id: "albumNew", icon: Icon::Plus, tooltip: "Create Album", action: HeaderAction::Menu(new_album_menu) })
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
fn show_header_button(app: &mut LightcraftApp, ui: &mut egui::Ui, header: Rect, button: &HeaderButton) {
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
fn new_keyword(app: &mut LightcraftApp, _ctx: &egui::Context) {
    let mut dialog = crate::panels::keyword_list::create_dialog_inside(app, None);
    if let crate::state::Dialog::KeywordTag { reveal, .. } = &mut dialog {
        *reveal = true;
    }
    app.ui.dialog = Some(dialog);
}

/// The menu of a section's header (`on`) and of the My Photos title (`None`): hide this section,
/// show or hide any section, and go back to the usual arrangement. Its items are widgets
/// `sidebarMenu:hide`, `sidebarMenu:section:<id>` and `sidebarMenu:reset`.
fn sections_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, on: Option<SidebarSection>) {
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
fn new_album_menu(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if ui.button(crate::i18n::tr("Create Album…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: true });
    }
    if ui.button(crate::i18n::tr("Create Smart Album…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::SmartRules {
            id: None,
            name: String::new(),
            rules: lightcraft_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
            parent: None,
        });
    }
    if ui.button(crate::i18n::tr("Create Smart Album from Filter…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::NewSmartAlbum { name: String::new(), parent: None });
    }
    if ui.button(crate::i18n::tr("Create Folder…")).clicked() {
        app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: true, parent: None, include_selected: false });
    }
    // only once the albums were put in an order by hand
    if app.session.catalog.album_children_are_ordered(None) {
        ui.separator();
        if ui.button(crate::i18n::tr("Sort Albums A–Z")).on_hover_text(crate::i18n::tr("Go back to listing them by name")).clicked() {
            let _ = app.run("album.sort", json!({}));
        }
    }
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider));
    frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // the title offers the sections' menu too: where hidden sections come back from
        let (hr, title) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
        crate::widgets::announce(&title, crate::i18n::tr("My Photos"));
        register(ui.ctx(), "sidebarTitle", hr);
        title.context_menu(|ui| sections_menu(app, ui, None));
        // the open library's name under the title, so nobody edits in the wrong one (issue #902)
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
        let counts = app.caches.counts(&app.session.catalog);
        let (total, picks, deleted) = (counts.total, counts.picks, counts.deleted);
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
                // where the visible part of the content ends (screen x), for what stays at the edge
                d.insert_temp(egui::Id::new("left-visible-right"), ui.cursor().left() + viewport.max.x);
            });
            let src = app.session.source;
            for (id, icon, label, count, s) in [
                ("all", Icon::Photos, "All Photos", Some(total), LibrarySource::All),
                ("recentlyAdded", Icon::Clock, "Recently Added", None, LibrarySource::RecentlyAdded),
                ("picks", Icon::FlagPick, "Picks", Some(picks), LibrarySource::Picks),
            ] {
                if row(app, ui, id, icon, label, count, src == s, 0.0).clicked() {
                    let _ = app.run("library.source", json!({"kind": id}));
                }
            }
            // photos whose files can't be found (checked every few seconds, not every frame)
            let missing = missing_count(app, ui);
            if (missing > 0 || src == LibrarySource::Missing)
                && row(app, ui, "missing", Icon::Folder, "Missing Photos", Some(missing), src == LibrarySource::Missing, 0.0).clicked()
            {
                let _ = app.run("library.source", json!({"kind": "missing"}));
            }
            if row(app, ui, "recentlyDeleted", Icon::Trash, "Recently Deleted", Some(deleted), src == LibrarySource::RecentlyDeleted, 0.0).clicked() {
                let _ = app.run("library.source", json!({"kind": "recentlyDeleted"}));
            }
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
                    set_tree_open(ui, egui::Id::new(("kw-open", above.to_lowercase())), true);
                }
                // …and the sidebar scrolls to it
                ui.data_mut(|d| d.insert_temp(egui::Id::new(SCROLL_TO_ROW), format!("keyword:{path}").to_lowercase()));
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
            // what the rows asked for becomes next frame's width
            let next = ui.data(|d| d.get_temp::<f32>(egui::Id::new("left-content-width-next"))).unwrap_or(0.0);
            if (next - content_width(ui.ctx())).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(egui::Id::new("left-content-width"), next));
                ui.ctx().request_repaint();
            }
        });
    });
}

/// Albums: the tree of albums and the folders they are sorted into. `reveal`: an album just made,
/// whose folders open down to it.
fn albums_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, reveal: Option<u64>) {
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
fn date_rows(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let groups = app.caches.date_groups(&app.session.catalog);
    for g in groups.iter() {
        if date_row(app, ui, &g.year, &crate::i18n::date_group_label(&g.year, true), g.count, 0.0) {
            for (m, n) in &g.months {
                let label = crate::i18n::date_group_label(m, true);
                if date_row(app, ui, m, &label, *n, 16.0) {
                    for (d, n) in g.days.iter().filter(|(d, _)| d.starts_with(m.as_str())) {
                        let label = crate::i18n::date_group_label(d, true);
                        date_row(app, ui, d, &label, *n, 32.0);
                    }
                }
            }
        }
    }
}

/// How many library photos have no file (Local browse records are not checked; see
/// `cmd::missing::checked_path`). Checking stats every file, which on a network share takes
/// seconds, so it runs on a worker thread: the count shown is the last finished one, refreshed
/// at most every 5 s, and at once (after the running check) when the catalog changed.
fn missing_count(app: &mut LightcraftApp, ui: &mut egui::Ui) -> usize {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
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
        let paths = lightcraft_engine::cmd::missing::candidates(&app.session.catalog);
        job.running.store(true, Relaxed);
        job.at = now;
        job.rev = rev;
        let (n, running, ctx) = (job.n.clone(), job.running.clone(), ui.ctx().clone());
        let work = move || {
            let missing = if cfg!(target_arch = "wasm32") { 0 } else { paths.iter().filter(|p| !std::path::Path::new(p).exists()).count() };
            n.store(missing, Relaxed);
            running.store(false, Relaxed);
            ctx.request_repaint();
        };
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(work);
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
}

/// Folders on this computer to browse without adding (Lightroom's Local): Pictures, Desktop,
/// Downloads, the home folder, the folder being browsed, and the folders kept with Browse
/// Folder…. Worked out every frame the sidebar shows, drawn or not: the row listed for this
/// session only (`local_browse_root`) follows the browsing. `None` where there is no Local (web).
fn local_now(app: &mut LightcraftApp, ui: &egui::Ui) -> Option<LocalNow> {
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
    let places = local_places(builtin, &app.ui.local_roots, current.as_deref(), app.ui.local_browse_root.as_deref(), &app.ui.hidden_locations);
    if current.is_some() {
        app.ui.local_browse_root = places.browse_root.clone();
    }
    Some(LocalNow { places, current })
}

/// Local's rows: each place as a folder tree, Browse Folder…, and the way back for hidden places.
fn local_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, now: &LocalNow) {
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
            match app.run("library.browse", json!({"path": path})) {
                // the picked folder stays in Local (and comes back if it was hidden)
                Ok(r) => {
                    let dir = r["path"].as_str().unwrap_or(&path).to_string();
                    let _ = app.run("local.addRoot", json!({"path": dir}));
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
    }
    let hidden = app.ui.hidden_locations.len();
    if hidden > 0 {
        let label = crate::i18n::tr_format!("Show {hidden} hidden location{}", if hidden == 1 { "" } else { "s" }, hidden = hidden);
        if row(app, ui, "local:restoreHidden", Icon::Folder, &label, None, false, 0.0)
            .on_hover_text(crate::i18n::tr("Put the locations you removed from Local back (no files change)"))
            .clicked()
        {
            let _ = app.run("local.restoreHidden", json!({}));
        }
    }
}

/// Whether two paths name the same folder, however they are spelled (separators, trailing
/// slash, `.`/`..`, drive-letter case; see `lightcraft_catalog::query::folder_key`).
pub(crate) fn same_folder(a: &str, b: &str) -> bool {
    a == b || lightcraft_catalog::query::folder_key(a) == lightcraft_catalog::query::folder_key(b)
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
    use lightcraft_catalog::query::{folder_key, folder_within};
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
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
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

/// Whether `path` is a folder, when the file system answers within a moment; `None` when it is slow
/// to (a network share busy listing, a sleeping NAS): the caller goes on without blocking the window,
/// and the work that reads the folder finds out. The look goes on in the background meanwhile.
pub(crate) fn is_dir_quickly(path: &std::path::Path) -> Option<bool> {
    #[cfg(target_arch = "wasm32")]
    {
        Some(path.is_dir())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.to_path_buf();
        std::thread::Builder::new()
            .name("lc-is-dir".into())
            .spawn(move || {
                let _ = tx.send(p.is_dir());
            })
            .ok()?;
        rx.recv_timeout(std::time::Duration::from_millis(150)).ok()
    }
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
        use std::sync::atomic::Ordering;
        let running = fs_running_counter(ui.ctx());
        running.fetch_add(1, Ordering::AcqRel);
        let (out, path, repaint, done) = (cell.clone(), path.to_string(), ui.ctx().clone(), running.clone());
        let work = move || {
            let v = f(&path);
            let mut e = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            e.value = Some(v);
            e.running = false;
            drop(e);
            // after the answer is stored: a frame that sees the count drop also sees the answer
            done.fetch_sub(1, Ordering::AcqRel);
            repaint.request_repaint();
        };
        #[cfg(not(target_arch = "wasm32"))]
        if std::thread::Builder::new().name("lc-fs-list".into()).spawn(work).is_err() {
            cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner).running = false;
            running.fetch_sub(1, Ordering::AcqRel);
        }
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
fn folder_tree(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    place: &str,
    name: &str,
    path: &str,
    indent: f32,
    current: Option<&str>,
    reveal: bool,
    transient: bool,
) {
    let open_id = egui::Id::new(("folder-open", place.to_string(), path.to_string()));
    let sel = current.is_some_and(|c| same_folder(c, path));
    let on_the_way = !sel && current.is_some_and(|c| lightcraft_catalog::query::folder_within(c, path));
    let mut open = tree_open(ui, open_id, false);
    if reveal && on_the_way {
        // opened once per browsed folder: collapsing it again afterwards sticks
        let revealed_id = egui::Id::new(("folder-revealed", place.to_string(), path.to_string()));
        let target = current.map(str::to_string);
        if ui.data(|d| d.get_temp::<Option<String>>(revealed_id)) != Some(target.clone()) {
            open = true;
            set_tree_open(ui, open_id, true);
            ui.data_mut(|d| d.insert_temp(revealed_id, target));
        }
    }
    // at the tree's own indent, as in Albums and Folders: the triangle sits in the gutter and the
    // icon lines up with the rows above
    let resp = row(app, ui, &format!("local:{path}"), Icon::Folder, name, None, sel, indent).on_hover_text(path);
    let tr = tree_toggle(ui, &resp, indent, open_id, &mut open, format!("folderToggle:{path}"));
    if !tr.clicked()
        && resp.clicked()
        && let Err(e) = app.run("library.browse", json!({"path": path}))
    {
        app.toast(ui.ctx(), e);
    }
    resp.context_menu(|ui| {
        // the library actions of the Folders tree, when this folder (or one inside it) holds library photos
        let library = app.caches.folder_tree(&app.session.catalog);
        let held = library_node_for(&library, path);
        if (transient || indent > 0.0)
            && ui
                .button(crate::i18n::tr(if transient { "Keep in Local" } else { "Add to Local" }))
                .on_hover_text(crate::i18n::tr("List this folder in Local from now on"))
                .clicked()
        {
            let _ = app.run("local.addRoot", json!({"path": path}));
            ui.close();
        }
        if indent == 0.0
            && ui
                .button(crate::i18n::tr("Remove from Local"))
                .on_hover_text(crate::i18n::tr("Hides this shortcut only; the folder and its photos stay as they are"))
                .clicked()
        {
            let _ = app.run("local.hide", json!({"path": path}));
            ui.close();
        }
        if ui.button(crate::i18n::tr("Rename Folder…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                title: crate::i18n::tr_format!("Rename “{name}”", name = name),
                hint: "Folder name (renamed on disk; its photos follow)".into(),
                value: name.to_string(),
                command: "folder.rename".into(),
                params: json!({"path": path}),
                key: "name".into(),
            });
        }
        if app.services.pick_folder.is_some() && ui.button(crate::i18n::tr("Move Folder To…")).clicked() {
            let into = app.services.pick_folder.as_mut().and_then(|f| f());
            if let Some(into) = into {
                match app.run("folder.move", json!({"path": path, "into": into})) {
                    Ok(r) => app.toast(ui.ctx(), crate::i18n::tr_format!("Moved; {} photo(s) relinked", r["relinked"])),
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
        }
        if app.services.reveal.is_some()
            && ui.button(crate::i18n::tr(crate::menus::reveal_label())).clicked()
            && let Some(f) = app.services.reveal.as_mut()
        {
            let _ = f(path);
        }
        if let Some(held) = &held {
            synchronize_item(app, ui, &held.0, &held.1);
        }
    });
    if open && indent < 16.0 * 8.0 {
        for (n, p) in subfolders(ui, path) {
            folder_tree(app, ui, place, &n, &p, indent + 16.0, current, reveal, false);
        }
    }
}

/// One By Date row (`key`: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`); returns whether it is open.
fn date_row(app: &mut LightcraftApp, ui: &mut egui::Ui, key: &str, label: &str, count: usize, indent: f32) -> bool {
    let open_id = egui::Id::new(("date-open", key.to_string()));
    let mut open = tree_open(ui, open_id, false);
    let sel = app.session.filter.date.as_deref() == Some(key);
    let resp = row(app, ui, &format!("date:{key}"), Icon::Clock, label, Some(count), sel, indent);
    if key.len() < 10 {
        tree_toggle(ui, &resp, indent, open_id, &mut open, format!("dateToggle:{key}"));
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

/// The albums inside each folder (`None`: the top level), in the order they are listed.
type AlbumKids = std::collections::HashMap<Option<AlbumId>, Vec<Album>>;

/// `open_to`: folders forced open this frame (the way down to an album that was just made).
fn albums_tree(app: &mut LightcraftApp, ui: &mut egui::Ui, all: &AlbumKids, parent: Option<AlbumId>, indent: f32, open_to: &[AlbumId]) {
    // 64 folders deep at most (16 points a level): a catalog that loops must not overflow the stack
    if indent > 64.0 * 16.0 {
        return;
    }
    let Some(kids) = all.get(&parent) else { return };
    for a in kids {
        if a.folder {
            let open_id = egui::Id::new(("folder-open", a.id.0));
            let mut open = tree_open(ui, open_id, true);
            if open_to.contains(&a.id) && !open {
                open = true;
                set_tree_open(ui, open_id, true);
            }
            let resp =
                row_sensed(app, ui, &format!("folder:{}", a.id.0), Icon::Folder, &a.name, None, None, false, indent, Sense::click_and_drag(), None);
            // a folder is no source, so its row folds it too; the triangle is the same click, aimed
            let has_children = all.get(&Some(a.id)).is_some_and(|v| !v.is_empty());
            if resp.drag_started() {
                app.ui.dragging_album = Some(a.id.0);
            }
            if album_drag_over(app, ui, &resp, a, has_children.then_some(&mut open), indent) {
                set_tree_open(ui, open_id, true);
            }
            // what this frame draws; a click shows from the next one
            let draw_open = open;
            let mut toggled = false;
            if has_children {
                let tri = tree_toggle(ui, &resp, indent, open_id, &mut open, format!("albumToggle:{}", a.id.0));
                toggled = tri.clicked();
                // the triangle sits on the row and takes its clicks: the menu opens from it too
                folder_menu(app, &tri, a);
            }
            // one click, one toggle: the triangle's is not the row's as well
            if resp.clicked() && !toggled {
                set_tree_open(ui, open_id, !open);
            }
            folder_menu(app, &resp, a);
            if draw_open {
                albums_tree(app, ui, all, Some(a.id), indent + 16.0, open_to);
            }
        } else {
            let sel = app.session.source == LibrarySource::Album(a.id);
            let icon = if a.is_smart() { Icon::SmartAlbum } else { Icon::Album };
            // cached: a smart album's count scans the catalog
            let now = (app.session.clock)();
            let n = app.caches.album_counts(&app.session.catalog, &now).get(&a.id).copied().unwrap_or(0);
            // the album B adds to is marked "+"
            let target =
                app.session.target_album.filter(|t| app.session.catalog.album(*t).is_some()).or_else(|| app.session.catalog.quick_collection());
            let label = if target == Some(a.id) { format!("{} +", a.name) } else { a.name.clone() };
            // a smart album whose rules no longer check (an album they test was deleted) says so
            let problems = app.caches.smart_album_problems(&app.session.catalog).get(&a.id).cloned().unwrap_or_default();
            let label = if problems.is_empty() { label } else { format!("⚠ {label}") };
            let mut resp = row_sensed(app, ui, &format!("album:{}", a.id.0), icon, &label, None, Some(n), sel, indent, Sense::click_and_drag(), None);
            if !problems.is_empty() {
                crate::widgets::register(ui.ctx(), format!("albumProblem:{}", a.id.0), resp.rect);
            }
            if resp.drag_started() {
                app.ui.dragging_album = Some(a.id.0);
            }
            album_drag_over(app, ui, &resp, a, None, indent);
            if !a.is_smart() {
                drop_target(app, ui, &resp, a);
            }
            if let Some(rules) = &a.smart {
                // one tooltip: the rules, then what needs fixing in them
                let mut tip = crate::i18n::tr_format!("Smart album: {}", crate::i18n::filter_label(rules, &app.session.catalog));
                if !problems.is_empty() {
                    tip.push_str("\n\n");
                    tip.push_str(crate::i18n::tr("Some rules need fixing (Edit Smart Album…):"));
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
fn drag_auto_scroll(app: &LightcraftApp, ui: &egui::Ui) {
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
fn can_drop_album(app: &LightcraftApp, dragged: AlbumId, target: Option<AlbumId>) -> bool {
    let Some(d) = app.session.catalog.album(dragged) else { return false };
    match target {
        None => d.parent.is_some(),
        Some(t) => app.session.catalog.album(t).is_some_and(|f| f.folder) && d.parent != Some(t) && !is_within(app, t, dragged),
    }
}

/// Ends a drag that ran `result`: the folders down to the album open so it shows in its new place.
fn finish_drop(app: &mut LightcraftApp, ui: &egui::Ui, dragged: AlbumId, result: Result<serde_json::Value, String>) {
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
fn forget_hover(app: &LightcraftApp, ctx: &egui::Context) {
    let ids: Vec<u64> = app.session.catalog.albums().filter(|a| a.folder).map(|a| a.id.0).collect();
    ctx.data_mut(|d| {
        for id in ids {
            d.remove_temp::<f64>(egui::Id::new(("album-hover", id)));
        }
    });
}

/// Moves the dragged album into a folder (`album.move`, one undo step; `None`: the top level).
fn drop_album(app: &mut LightcraftApp, ui: &egui::Ui, dragged: AlbumId, target: Option<AlbumId>) {
    let r = app.run("album.move", json!({"id": dragged.0, "parent": target.map(|t| t.0)}));
    finish_drop(app, ui, dragged, r);
}

/// Places the dragged album in `parent` before the sibling `before` (`None`: last of its kind), by
/// hand (`album.reorder`, one undo step).
fn place_album(app: &mut LightcraftApp, ui: &egui::Ui, dragged: AlbumId, parent: Option<AlbumId>, before: Option<AlbumId>) {
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
fn album_drop_at(app: &LightcraftApp, dragged: AlbumId, target: &Album, frac: f32, expanded: bool) -> Option<AlbumDrop> {
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
fn album_drag_over(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, row: &Album, open: Option<&mut bool>, indent: f32) -> bool {
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
            let (x0, x1) = (resp.rect.left() + 18.0 + indent, ui.clip_rect().right() - 8.0);
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

/// While an album that sits in a folder is dragged: the "Albums" header takes it back to the top
/// level (outlined, with a hint). The header stays in view however long the tree is, and nothing
/// moves when it appears.
fn top_level_drop_target(app: &mut LightcraftApp, ui: &mut egui::Ui, header: Rect) {
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
pub fn album_drag_feedback(app: &mut LightcraftApp, ctx: &egui::Context) {
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
fn drop_target(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, a: &Album) {
    let Some(ids) = app.ui.dragging_photos.clone() else { return };
    // only where the row can be seen: not scrolled out of the sidebar or beneath the pinned header
    let over = pointer_over_row(ui, resp.rect);
    if !over {
        return;
    }
    let released = ui.input(|i| i.pointer.any_released());
    // albums hold library photos (issue #684): Local ones can't be dropped in; the row doesn't
    // light up, and a release says why
    let photos: Vec<lightcraft_catalog::PhotoId> = ids.iter().map(|id| lightcraft_catalog::PhotoId(*id)).collect();
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
                        "{n} Local photo{} skipped: albums hold library photos (Add to My Photos first)",
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

fn folder_menu(app: &mut LightcraftApp, resp: &egui::Response, a: &Album) {
    resp.context_menu(|ui| {
        if a.folder {
            // the same commands as File ▸ New…, aimed at this folder
            crate::widgets::menu_button(ui, crate::i18n::tr("New"), |ui| {
                for (label, command) in [
                    ("Create Album…", "dialog.newAlbum"),
                    ("Create Smart Album…", "dialog.smartAlbum"),
                    ("Create Smart Album from Filter…", "dialog.newSmartAlbum"),
                    ("Create Folder…", "dialog.newFolder"),
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
                    let devices = lightcraft_engine::devices::devices();
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
        if !a.folder && !a.is_smart() {
            let is_target = app.session.target_album == Some(a.id) || (app.session.target_album.is_none() && a.quick);
            if !is_target && ui.button(crate::i18n::tr("Set as Target Album (B adds to it)")).clicked() {
                let _ = app.run("album.setTarget", json!({"id": if a.quick { serde_json::Value::Null } else { json!(a.id.0) }}));
            }
            if is_target && !a.quick && ui.button(crate::i18n::tr("Stop Using as Target Album")).clicked() {
                let _ = app.run("album.setTarget", json!({"id": null}));
            }
        }
        if a.quick && ui.button(crate::i18n::tr("Clear Quick Collection")).clicked() {
            let _ = app.run("album.clearQuick", json!({}));
        }
        if a.is_smart() && ui.button(crate::i18n::tr("Edit Smart Album…")).clicked() {
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
            let show_all = |app: &mut LightcraftApp| {
                let _ = app.run("library.source", json!({"kind": "album", "id": a.id.0}));
                let _ = app.run("library.selectAll", json!({}));
            };
            let has_photos = app.session.catalog.album_count(a.id) > 0;
            if ui.add_enabled(has_photos, egui::Button::new(crate::i18n::tr("Export Album…"))).clicked() {
                show_all(app);
                let _ = app.run("dialog.export", json!({}));
            }
            ui.add_enabled_ui(has_photos, |ui| {
                crate::widgets::menu_button(ui, crate::i18n::tr("Export Album with Preset"), |ui| {
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
fn is_within(app: &LightcraftApp, id: lightcraft_catalog::AlbumId, ancestor: lightcraft_catalog::AlbumId) -> bool {
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
// `lightcraft_catalog::folders`). A click makes that folder the source, like an album or a Local
// folder: its photos and those of the folders inside it fill the grid. The triangle opens a
// level. Only folders holding imported photos are listed; every folder on disk is under Local.

/// Whenever the shown folder changes (a click, an agent, a rename or its undo), open the rows
/// above it so it is on screen; folding one by hand afterwards sticks until the choice changes.
fn reveal_chosen(app: &LightcraftApp, ui: &egui::Ui, tree: &[FolderNode]) {
    let shown = app.session.library_folder.clone().filter(|_| app.session.source == LibrarySource::LibraryFolder);
    let chosen = shown.filter(|c| !lightcraft_catalog::query::folder_key(c).is_empty());
    let seen = egui::Id::new("libfolder-revealed");
    let now = chosen.as_deref().map(lightcraft_catalog::query::folder_key);
    if ui.data(|d| d.get_temp::<Option<String>>(seen)) == Some(now.clone()) {
        return;
    }
    ui.data_mut(|d| d.insert_temp(seen, now));
    let Some(chosen) = chosen else { return };
    fn open_above(ui: &egui::Ui, nodes: &[FolderNode], chosen: &str) {
        for n in nodes {
            if lightcraft_catalog::query::folder_within(chosen, &n.path) && !same_folder(chosen, &n.path) {
                let key = lightcraft_catalog::query::folder_key(&n.path);
                ui.data_mut(|d| d.insert_temp(egui::Id::new(("libfolder-open", key)), true));
                open_above(ui, &n.children, chosen);
            }
        }
    }
    open_above(ui, tree, &chosen);
}

fn folder_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, nodes: &[FolderNode], indent: f32) {
    for n in nodes {
        let key = lightcraft_catalog::query::folder_key(&n.path);
        let open_id = egui::Id::new(("libfolder-open", key.clone()));
        // a disk starts open, and so does a folder that holds nothing itself and leads to one
        // folder (`Users` → `me`): the first row where the library branches is what you look for
        let leads_on = n.own == 0 && n.children.len() == 1;
        let mut open = tree_open(ui, open_id, n.volume || leads_on);
        // a row whose path would cover other disks' photos too only opens and closes
        let name = if n.selectable { n.name.clone() } else { crate::i18n::tr("This Computer").to_string() };
        let id = format!("libfolder:{}", n.path);
        if !out_of_sight(app, ui, &id, &name, Some(n.count), indent, n.label.is_some()) {
            folder_row(app, ui, n, &id, &name, indent, open_id, &mut open);
        }
        if open && !n.children.is_empty() {
            folder_rows(app, ui, &n.children, indent + 16.0);
        }
    }
}

/// One drawn row of [`folder_rows`], shown as `name`; `open_id` and `open` are its triangle's.
#[allow(clippy::too_many_arguments)]
fn folder_row(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode, id: &str, name: &str, indent: f32, open_id: egui::Id, open: &mut bool) {
    let sel = n.selectable
        && app.session.source == LibrarySource::LibraryFolder
        && app.session.library_folder.as_deref().is_some_and(|f| same_folder(f, &n.path));
    // the label is said with the name, as it is shown beside it
    let spoken = match n.label {
        Some(l) => format!("{name}, {}", crate::i18n::color_label(&app.session.catalog, l)),
        None => name.to_string(),
    };
    let mark = n.label.map(crate::theme::label_color);
    let resp = row_sensed(app, ui, id, Icon::Folder, name, Some(&spoken), Some(n.count), sel, indent, Sense::click(), mark);
    folder_drop_target(app, ui, &resp, n, open_id, open);
    let mut toggled = false;
    if !n.children.is_empty() {
        let tr = tree_toggle(ui, &resp, indent, open_id, open, format!("libraryFolderToggle:{}", n.path));
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
            set_tree_open(ui, open_id, *open);
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
fn folder_drop_target(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, n: &FolderNode, open_id: egui::Id, open: &mut bool) {
    let Some(ids) = app.ui.dragging_photos.clone() else { return };
    // only where the row can be seen: not scrolled out of the sidebar or beneath the pinned header
    if !pointer_over_row(ui, resp.rect) {
        return;
    }
    spring_open(ui, n, open_id, open);
    let name = std::path::Path::new(&n.path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| n.name.clone());
    let photos: Vec<_> = ids.iter().filter_map(|id| app.session.catalog.photo(lightcraft_catalog::PhotoId(*id))).collect();
    let refused = if !n.selectable {
        Some(crate::i18n::tr("This row covers more than one disk: drop on a folder inside it"))
    } else if photos.iter().any(|p| p.local) {
        Some(crate::i18n::tr("Local photos aren't in the library yet: Add to My Photos first, then move them"))
    } else {
        None
    };
    let here = |p: &lightcraft_catalog::Photo| match &p.source {
        lightcraft_catalog::Source::File { path } => std::path::Path::new(path).parent().is_some_and(|d| same_folder(&d.to_string_lossy(), &n.path)),
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
fn spring_open(ui: &egui::Ui, n: &FolderNode, open_id: egui::Id, open: &mut bool) {
    if *open || n.children.is_empty() {
        return;
    }
    let key = egui::Id::new("folder-spring-open");
    let (now, frame) = (ui.input(|i| i.time), ui.ctx().cumulative_frame_nr());
    // held without a break: over this row last frame too
    let since = match ui.data(|d| d.get_temp::<(String, f64, u64)>(key)) {
        Some((path, since, seen)) if path == n.path && seen.saturating_add(1) >= frame => since,
        _ => now,
    };
    ui.data_mut(|d| d.insert_temp(key, (n.path.clone(), since, frame)));
    if now - since >= SPRING_OPEN_SECONDS {
        *open = true;
        set_tree_open(ui, open_id, true);
        ui.data_mut(|d| d.remove::<(String, f64, u64)>(key));
    } else {
        // keep the clock running while the pointer rests
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Move photos (`ids`, else the selection) into the folder `path` on disk (`photo.moveToFolder`).
/// Photos that leave the view leave the selection, so nothing done next (Delete, an edit) reaches
/// photos no longer on screen. How many moved, or why none did.
pub(crate) fn move_photos(app: &mut LightcraftApp, ids: Option<&[u64]>, path: &str) -> Result<u64, String> {
    let mut params = json!({"folder": path});
    if let Some(ids) = ids {
        params["ids"] = json!(ids);
    }
    let r = app.run("photo.moveToFolder", params)?;
    let visible: std::collections::HashSet<lightcraft_catalog::PhotoId> = app.session.visible_cloned().into_iter().collect();
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
fn move_photos_here(app: &mut LightcraftApp, ctx: &egui::Context, ids: Option<&[u64]>, path: &str, name: &str) {
    match move_photos(app, ids, path) {
        Ok(n) => app.toast(ctx, moved_text(n, name)),
        Err(e) => app.toast(ctx, e),
    }
}

/// The context menu of a Folders row: a folder's own, a disk's (remove it), none for the
/// startup disk.
fn row_menu(app: &mut LightcraftApp, resp: &egui::Response, n: &FolderNode) {
    if !n.volume {
        folder_menu_for_library(app, resp, n);
    } else if n.path != "/" {
        resp.context_menu(|ui| {
            folder_label_menu(app, ui, n);
            locate_missing_item(app, ui, n);
            synchronize_item(app, ui, n, &n.name);
            ui.separator();
            if ui
                .button(crate::i18n::tr("Remove Disk from Library…"))
                .on_hover_text(crate::i18n::tr("Moves every photo imported from this disk to Recently Deleted; no file is touched"))
                .clicked()
            {
                app.ui.dialog = Some(crate::state::Dialog::RemoveFolder { path: n.path.clone(), name: n.name.clone(), count: n.count, disk: true });
                ui.close();
            }
        });
    }
}

/// Locate Missing Files… of a Folders row: choose where this folder moved, then let the existing
/// background finder relink only this folder's photos (including the folders inside it).
fn locate_missing_item(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode) {
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
fn library_node_for(tree: &[FolderNode], path: &str) -> Option<(FolderNode, String)> {
    use lightcraft_catalog::query::folder_within;
    fn exact<'a>(tree: &'a [FolderNode], path: &str) -> Option<&'a FolderNode> {
        tree.iter().find_map(|n| if same_folder(&n.path, path) { Some(n) } else { exact(&n.children, path) })
    }
    fn any_inside(tree: &[FolderNode], path: &str) -> bool {
        tree.iter().any(|n| (n.count > 0 && folder_within(&n.path, path)) || any_inside(&n.children, path))
    }
    if let Some(n) = exact(tree, path).filter(|n| n.count > 0) {
        return Some((n.clone(), lightcraft_catalog::folders::folder_label(&n.path)));
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

/// Synchronize Folder… of a Folders row (widget `folderSynchronize`): opens its dialog, which
/// scans the folder in the background.
fn synchronize_item(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode, name: &str) {
    let r = ui
        .button(crate::i18n::tr("Synchronize Folder…"))
        .on_hover_text(crate::i18n::tr("Find photos added to or missing from this folder on disk, and XMP sidecars changed by other apps"));
    crate::widgets::register(ui.ctx(), "folderSynchronize", r.rect);
    if r.clicked() {
        // a disk, or a folder holding disks, was chosen as such
        if let Err(e) = crate::sync::open(app, &n.path, name, n.volume || !n.selectable) {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    }
}

/// Set Color Label ▸ of a Folders row (`folder.label`; the submenu is widget `folderLabelMenu`,
/// its colours `folderLabel:<colour>`).
fn folder_label_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &FolderNode) {
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

/// The context menu of a folder row: the folder's disk actions (the same as Local's, photos
/// follow) and taking its photos out of the library.
fn folder_menu_for_library(app: &mut LightcraftApp, resp: &egui::Response, n: &FolderNode) {
    let path = n.path.as_str();
    let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    // what dialogs call it: the last two names, so same-named folders are told apart
    let label = lightcraft_catalog::folders::folder_label(path);
    resp.context_menu(|ui| {
        if ui.button(crate::i18n::tr("Rename Folder…")).clicked() {
            app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                title: crate::i18n::tr_format!("Rename “{name}”", name = label),
                hint: "Folder name (renamed on disk; its photos follow)".into(),
                value: name.clone(),
                command: "folder.rename".into(),
                params: json!({"path": path}),
                key: "name".into(),
            });
            ui.close();
        }
        if app.services.pick_folder.is_some() && ui.button(crate::i18n::tr("Move Folder To…")).clicked() {
            let into = app.services.pick_folder.as_mut().and_then(|f| f());
            if let Some(into) = into {
                match app.run("folder.move", json!({"path": path, "into": into})) {
                    Ok(r) => app.toast(ui.ctx(), format!("Moved; {} photo(s) relinked", r["relinked"])),
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
            ui.close();
        }
        if ui.button(crate::i18n::tr("New Folder on Disk…")).clicked() {
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
        let selected = app.session.selection.ids.len();
        if selected > 0
            && ui
                .button(crate::i18n::tr("Move Selected Photos Here"))
                .on_hover_text(crate::i18n::tr("Moves the files and their XMP sidecars into this folder; never overwrites"))
                .clicked()
        {
            move_photos_here(app, ui.ctx(), None, path, &name);
            ui.close();
        }
        if app.services.reveal.is_some()
            && ui.button(crate::i18n::tr(crate::menus::reveal_label())).clicked()
            && let Some(f) = app.services.reveal.as_mut()
        {
            let _ = f(path);
            ui.close();
        }
        folder_label_menu(app, ui, n);
        locate_missing_item(app, ui, n);
        synchronize_item(app, ui, n, &label);
        ui.separator();
        if ui
            .button(crate::i18n::tr("Remove from Library…"))
            .on_hover_text(crate::i18n::tr("Moves the photos imported from this folder to Recently Deleted; no file is touched"))
            .clicked()
        {
            app.ui.dialog = Some(crate::state::Dialog::RemoveFolder { path: path.to_string(), name: label.clone(), count: n.count, disk: false });
            ui.close();
        }
    });
}

/// "Keywords": the library's keyword tree with photo counts (`a|b|c` keywords nest). A click
/// filters the grid by the keyword (children included), the triangle opens a level, and the
/// context menu renames, merges or deletes the keyword across the library.
fn keyword_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, nodes: &[KeywordNode], indent: f32) {
    for n in nodes {
        // leaves (most keywords) have no open state to look up
        let open_id = (!n.children.is_empty()).then(|| egui::Id::new(("kw-open", n.path.to_lowercase())));
        let mut open = open_id.is_some_and(|k| tree_open(ui, k, false));
        let id = format!("keyword:{}", n.path);
        if !out_of_sight(app, ui, &id, &n.name, Some(n.count), indent, false) {
            keyword_row(app, ui, n, &id, indent, open_id, &mut open);
        }
        if open {
            keyword_rows(app, ui, &n.children, indent + 16.0);
        }
    }
}

/// One drawn row of [`keyword_rows`]; `open_id` and `open` are its triangle's when it has rows inside it.
fn keyword_row(app: &mut LightcraftApp, ui: &mut egui::Ui, n: &KeywordNode, id: &str, indent: f32, open_id: Option<egui::Id>, open: &mut bool) {
    let sel = app.session.filter.keyword.as_deref().is_some_and(|k| k.eq_ignore_ascii_case(&n.path));
    let resp = row(app, ui, id, Icon::Tag, &n.name, Some(n.count), sel, indent);
    if let Some(k) = open_id {
        tree_toggle(ui, &resp, indent, k, open, format!("keywordToggle:{}", n.path));
    }
    let resp = resp.on_hover_text(if n.children.is_empty() { n.path.clone() } else { format!("{} (includes the keywords below it)", n.path) });
    if resp.clicked() {
        let v = if sel { serde_json::Value::Null } else { json!(n.path) };
        browse_all_photos(app, !sel);
        let _ = app.run("library.filter", json!({"keyword": v}));
    }
    resp.context_menu(|ui| {
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

    /// A file system that answers at once is asked as before: a folder is one, a file or a missing
    /// path is not (a slow one gives `None`, and opening it goes on without blocking the window).
    #[test]
    fn is_dir_quickly_answers_for_a_local_disk() {
        let dir = std::env::temp_dir();
        assert_eq!(super::is_dir_quickly(&dir), Some(true));
        assert_eq!(super::is_dir_quickly(&dir.join("lc-surely-not-here-1f3a")), Some(false));
    }

    /// Width = characters, so a limit of 12 is "12 characters".
    fn fit(text: &str, max: usize) -> String {
        elide_head(text, max as f32, |s| s.chars().count() as f32)
    }

    #[test]
    fn a_label_that_fits_is_left_alone() {
        assert_eq!(fit("/Users/me/Pictures", 18), "/Users/me/Pictures");
    }

    #[test]
    fn a_long_path_loses_its_leading_folders_not_its_end() {
        assert_eq!(fit("/Users/me/Pictures/Lightroom", 20), "…/Pictures/Lightroom");
        assert_eq!(fit("/Users/me/Pictures/Lightroom", 12), "…/Lightroom");
    }

    #[test]
    fn a_single_name_too_long_is_cut_at_the_end() {
        assert_eq!(fit("2024-summer-holiday", 8), "2024-su…");
        assert_eq!(fit("2024-summer-holiday", 1), "…");
        assert_eq!(fit("2024-summer-holiday", 0), "…");
    }

    #[test]
    fn multibyte_names_are_cut_on_character_boundaries() {
        assert_eq!(fit("/写真/夏休み旅行の記録", 6), "夏休み旅行…");
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

    #[test]
    fn a_local_folder_lines_up_with_the_rows_above_and_its_triangle_stays_left_of_the_icon() {
        use crate::Services;
        use crate::headless::HeadlessView;
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        let mut view = HeadlessView::new();
        let path = "/lightcraft-test/Pictures";
        let mut widgets = vec![];
        for frame in 0..3 {
            let raw = HeadlessView::raw_input(egui::vec2(300.0, 200.0), 1.0, frame as f64 / 60.0, vec![]);
            view.run(raw, |ui| {
                super::row(&mut app, ui, "all", super::Icon::Photos, "All Photos", None, false, 0.0);
                super::folder_tree(&mut app, ui, path, "Pictures", path, 0.0, None, false, false);
            });
            widgets = crate::widgets::take_registry(&view.ctx);
        }
        let rect = |id: &str| widgets.iter().find(|(w, _)| w == id).unwrap_or_else(|| panic!("{id} not registered")).1;
        let (all, local) = (rect("label:all"), rect(&format!("label:local:{path}")));
        assert_eq!(local.left(), all.left(), "a top-level Local folder's name starts where All Photos' does");
        let triangle = rect(&format!("folderToggle:{path}"));
        // the icon is 16 points wide and ends 8 points before the name
        assert!(triangle.right() <= local.left() - 24.0, "triangle {triangle:?} overlaps the icon before {local:?}");
    }
}
