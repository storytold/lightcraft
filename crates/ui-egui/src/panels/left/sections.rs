use super::albums::top_level_drop_target;
use super::rows::{ROW_HEIGHT, note_width};
use super::*;

/// Space above a section's header.
pub(super) const SECTION_GAP: f32 = 10.0;

/// Height of a section's header.
const SECTION_HEADER: f32 = 34.0;

/// One sidebar section: the gap above it, its header, and (while it is open) the rows `body`
/// draws. Every section goes through here, so they all fold, space and announce themselves alike.
/// `rows`: how many rows the section lists at its top level (a folded header says so).
pub(super) fn section(
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
    let show = egui::Id::unique("left-show-section");
    if let Some((asked, frame)) = ui.data(|d| d.get_temp::<(SidebarSection, u64)>(show))
        && asked == section
    {
        ui.data_mut(|d| d.remove::<(SidebarSection, u64)>(show));
        // asked for by the frame before (a request the section was not drawn for is forgotten),
        // and up or down only: the sidebar stays where it was scrolled sideways
        if ui.ctx().cumulative_frame_nr().saturating_sub(frame) <= 2 {
            let view_top: f32 = ui.data(|d| d.get_temp(egui::Id::unique("left-view-top"))).unwrap_or(header.top());
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
    ui.data(|d| d.get_temp(egui::Id::unique("left-visible-right"))).unwrap_or(f32::MAX)
}

/// The header of the open section whose own header has scrolled out of sight stays at the top of
/// the sidebar (widget `sidebarSectionPinned:<id>`), so it is always clear which section the rows
/// belong to, and the section can be folded, moved or hidden from anywhere in a long list. The
/// next section's header pushes it out as it arrives. `view_top`: where the visible part starts.
pub(super) fn pinned_header(app: &mut LightcraftApp, ui: &mut egui::Ui, placed: &[PlacedSection], view_top: f32) {
    let pinned = placed.iter().find(|p| p.open && p.top < view_top && p.bottom > view_top);
    let rect = pinned.map(|p| {
        let top = view_top.min(p.bottom - SECTION_HEADER);
        Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + SECTION_HEADER)
    });
    // rows ask where it is (next frame: they are laid out before it)
    ui.data_mut(|d| d.insert_temp(egui::Id::unique("left-pinned-rect"), rect));
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
        ui.data_mut(|d| d.insert_temp(egui::Id::unique("left-show-section"), (p.section, frame)));
        ui.ctx().request_repaint();
    }
}

/// What an open section with nothing to list says in place of its rows (widget
/// `sidebarEmpty:<id>`), so the section stays where it is instead of disappearing.
fn empty_hint(ui: &mut egui::Ui, section: SidebarSection) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());
    register(ui, format!("sidebarEmpty:{}", section.id()), r);
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
pub(super) fn section_drag(app: &mut LightcraftApp, ui: &mut egui::Ui, placed: &[PlacedSection]) {
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
        register(ui, "sidebarSectionDrop", line);
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
    egui::Area::new(egui::Id::unique("drag-section")).order(egui::Order::Tooltip).interactable(false).fixed_pos(pos + vec2(14.0, 10.0)).show(
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
    let resp = ui.interact(r, ui.scope_id().with((interact, id)), Sense::click_and_drag());
    register(ui, format!("{widget}:{id}"), r);
    let toggled = resp.clicked();
    if toggled {
        app.ui.sidebar.toggle_collapsed(section);
    }
    // dragged by its header, a section moves among the others (see `section_drag`); a press on
    // the header's button is the button's, also when it turns into a drag
    let on_button = header_button(section).is_some() && ui.input(|i| i.pointer.press_origin()).is_some_and(|p| header_button_rect(ui, r).contains(p));
    if resp.drag_started_by(egui::PointerButton::Primary) && !on_button {
        // picked up by the pinned header, the drag starts inside the top edge (see `drag_auto_scroll`)
        ui.data_mut(|d| d.insert_temp(egui::Id::unique("left-section-drag-from-pinned"), pinned));
        app.ui.dragging_section = Some(section);
    }
    crate::widgets::context_menu(&resp, |ui| sections_menu(app, ui, Some(section)));
    let open = !app.ui.sidebar.is_collapsed(section);
    crate::widgets::announce_disclosure(&resp, true, open, title);
    // what is visible of the header: its bar and count end at the panel's edge, like a row's
    let edge = r.right().min(visible_right(ui));
    let hovered = resp.hovered() && app.ui.dragging_section.is_none();
    if hovered {
        let bar = Rect::from_min_max(r.min + vec2(8.0, 2.0), pos2((edge - 8.0).max(r.left() + 8.0), r.bottom() - 2.0));
        ui.painter().rect_filled(bar, 4.0, t.hover.gamma_multiply(0.6));
    }
    let c = pos2(r.left() + 10.0, r.center().y);
    register(ui, format!("{widget}Chevron:{id}"), Rect::from_center_size(c, vec2(16.0, 16.0)));
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
        register(ui, format!("{widget}Count:{id}"), count);
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
pub(super) fn sections_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, on: Option<SidebarSection>) {
    let mut run = None;
    if let Some(section) = on {
        let hide = ui.button(crate::i18n::tr("Hide This Section"));
        register(ui, "sidebarMenu:hide", hide.rect);
        if hide.clicked() {
            run = Some(("view.sidebarSection", json!({"section": section.id(), "show": false})));
        }
        for (up, label, widget) in [(true, "Move Up", "sidebarMenu:moveUp"), (false, "Move Down", "sidebarMenu:moveDown")] {
            let item = ui.add_enabled(app.ui.sidebar.can_move(section, up), egui::Button::new(crate::i18n::tr(label)));
            register(ui, widget, item.rect);
            if item.clicked() {
                run = Some(("view.sidebarMoveSection", json!({"section": section.id(), "direction": if up { "up" } else { "down" }})));
            }
        }
        ui.separator();
    }
    for s in app.ui.sidebar.sections().iter().filter(|s| s.id.available()) {
        let shown = ui.selectable_label(!s.hidden, crate::i18n::tr(s.id.title()));
        register(ui, format!("sidebarMenu:section:{}", s.id.id()), shown.rect);
        if shown.clicked() {
            run = Some(("view.sidebarSection", json!({"section": s.id.id(), "show": s.hidden})));
        }
    }
    ui.separator();
    let reset = ui.add_enabled(!app.ui.sidebar.is_default(), egui::Button::new(crate::i18n::tr("Reset Sidebar Sections")));
    register(ui, "sidebarMenu:reset", reset.rect);
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

/// How many library photos have no file (Local browse records are not checked; see
/// `cmd::missing::checked_path`). Checking stats every file, which on a network share takes
/// seconds, so it runs on a worker thread: the count shown is the last finished one, refreshed
/// at most every 5 s, and at once (after the running check) when the catalog changed.
pub(super) fn missing_count(app: &mut LightcraftApp, ui: &mut egui::Ui) -> usize {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
    #[derive(Clone, Default)]
    struct Job {
        n: std::sync::Arc<AtomicUsize>,
        running: std::sync::Arc<AtomicBool>,
        at: f64,
        rev: u64,
    }
    let id = egui::Id::unique("missing-count");
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
