use craft::ui::contract::{Props as _, ShowContainer as _, Trigger};
use craft::ui::menu::Menu;

use super::albums::top_level_drop_target;
use super::rows::{
    AlbumDrag, MenuAt, ROW_HEIGHT, SectionDrag, SideEvent, SideRow, SideSpec, entry, host_menu, measuring, rule, side_list,
};
use super::{Icon, LightcraftApp, Rect, SidebarSection, Tokens, icon_button, json, pos2, register, vec2};

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
    let (_, header) = ui.allocate_space(vec2(ui.available_width(), SECTION_HEADER));
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
pub(super) fn visible_right(ui: &egui::Ui) -> f32 {
    visible_right_of(ui.ctx())
}

/// [`visible_right`] for a caller that has the context and not the `Ui` (a row's item closure, which
/// runs inside the list that owns the `Ui`).
pub(super) fn visible_right_of(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp(egui::Id::unique("left-visible-right"))).unwrap_or(f32::MAX)
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
    let (_, r) = ui.allocate_space(vec2(ui.available_width(), ROW_HEIGHT));
    register(ui, format!("sidebarEmpty:{}", section.id()), r);
    let text = crate::i18n::tr(section.empty_hint());
    let galley = ui.painter().layout_no_wrap(text.to_string(), t.font(12.5), t.text_dim);
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

/// The drags of sections and albums are the framework's drag sessions (so Escape and a lost press end
/// them): the app's own `dragging_section` and `dragging_album` follow the session, which the drop
/// targets, the auto-scroll and the tests read. Called first thing, before the lists are drawn.
pub(super) fn adopt_drags(app: &mut LightcraftApp, ui: &egui::Ui) {
    let ctx = ui.ctx();
    let down = ui.input(|i| i.pointer.primary_down());
    let (section, album) = (craft::ui::runtime::drag::active::<SectionDrag>(ctx), craft::ui::runtime::drag::active::<AlbumDrag>(ctx));
    let (section_flag, album_flag) = (egui::Id::unique("left-section-from-session"), egui::Id::unique("left-album-from-session"));
    let adopted = |flag: egui::Id| ui.data(|d| d.get_temp::<bool>(flag)).unwrap_or(false);
    match (section, app.ui.dragging_section) {
        (Some(p), None) if down => {
            app.ui.dragging_section = Some(p.section);
            // picked up by the pinned header, the drag starts inside the top edge (see `drag_auto_scroll`)
            ui.data_mut(|d| {
                d.insert_temp(egui::Id::unique("left-section-drag-from-pinned"), p.pinned);
                d.insert_temp(section_flag, true);
            });
        }
        (None, Some(_)) if adopted(section_flag) => {
            app.ui.dragging_section = None;
            ui.data_mut(|d| d.insert_temp(section_flag, false));
        }
        (None, None) => ui.data_mut(|d| {
            d.insert_temp(section_flag, false);
        }),
        _ => {}
    }
    match (album, app.ui.dragging_album) {
        (Some(p), None) if down => {
            app.ui.dragging_album = Some(p.0);
            ui.data_mut(|d| d.insert_temp(album_flag, true));
        }
        (None, Some(_)) if adopted(album_flag) => {
            app.ui.dragging_album = None;
            ui.data_mut(|d| d.insert_temp(album_flag, false));
        }
        (None, None) => ui.data_mut(|d| {
            d.insert_temp(album_flag, false);
        }),
        _ => {}
    }
}

/// While a section is dragged by its header: a line shows where it would go, releasing over the
/// sidebar moves it there (`view.sidebarMoveSection`), releasing elsewhere or Esc leaves it.
pub(super) fn section_drag(app: &mut LightcraftApp, ui: &mut egui::Ui, placed: &[PlacedSection]) {
    let Some(dragged) = app.ui.dragging_section else { return };
    let (released, pos) = ui.input(|i| (i.pointer.primary_released(), i.pointer.latest_pos()));
    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
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

/// A drag that ended anywhere (also outside the sidebar, or with the sidebar closed) is over. The
/// dragged section's name follows the pointer as the framework's drag ghost.
pub fn section_drag_feedback(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.ui.dragging_section.is_none() {
        return;
    }
    if !ctx.input(|i| i.pointer.primary_down()) || !app.ui.left_panel {
        app.ui.dragging_section = None;
        return;
    }
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
}

/// A section's header in `r`: a framework list row with a disclosure triangle in the gutter (where
/// tree rows have theirs), then the bold title; a click folds or unfolds the section (kept in the
/// UI state, so it survives restarts), and what the section adds to its header (its button, see
/// `header_button`) sits at the visible edge. A folded section says how many rows it holds (`rows`,
/// with View ▸ Show Photo Counts). `pinned`: this is the copy kept at the top of the sidebar (see
/// `pinned_header`). Dragged by its header, a section moves among the others.
fn section_header(app: &mut LightcraftApp, ui: &mut egui::Ui, section: SidebarSection, r: Rect, rows: usize, pinned: bool) -> HeaderOutcome {
    let id = section.id();
    let title = crate::i18n::tr(section.title());
    let widget = if pinned { "sidebarSectionPinned" } else { "sidebarSection" };
    let button = header_button(section);
    // the list takes the header but for the room the button keeps at the visible edge
    let edge = r.right().min(visible_right(ui));
    let room = if button.is_some() { header_button_rect(ui, r).width() } else { 0.0 };
    let list = Rect::from_min_max(r.min, pos2((edge - room).max(r.left() + 40.0), r.bottom()));
    let open = !app.ui.sidebar.is_collapsed(section);
    // (an album being dragged puts "Top Level" in the Albums header)
    let room_for_count = !(section == SidebarSection::Albums && app.ui.dragging_album.is_some());
    let note = (!open && rows > 0 && app.ui.show_counts && room_for_count).then(|| rows.to_string());
    let row = SideRow::new(format!("section:{id}"), "folder", title)
        .no_icon()
        .strong()
        .note(note)
        .group(open, format!("{widget}Chevron:{id}"))
        .unregistered();
    let salt = format!("section-{widget}-{id}");
    let mut header = ui.new_child(egui::UiBuilder::new().max_rect(list));
    let out = side_list(&mut header, SideSpec::new(&salt, crate::i18n::tr("Sidebar section"), false).keep_focus().section_drag(section, pinned), vec![row]);
    let live = !measuring(ui) && ui.is_enabled();
    if live {
        register(ui, format!("{widget}:{id}"), r);
        if let Some(count) = out.shown.first().and_then(|row| row.count) {
            register(ui, format!("{widget}Count:{id}"), count);
        }
    }
    // the triangle or the row folds or unfolds it, once
    let mut toggled = false;
    let mut request = None;
    for event in &out.events {
        match event {
            SideEvent::Open { .. } | SideEvent::Activate(_) => toggled = true,
            SideEvent::Menu { key, at } => request = Some((key.clone(), MenuAt::Point(*at))),
        }
    }
    if toggled {
        app.ui.sidebar.toggle_collapsed(section);
    }
    host_menu(ui, &salt, crate::i18n::tr(section.title()), out.response.as_ref(), request, |ui, _| sections_menu(app, ui, Some(section)));
    let open = !app.ui.sidebar.is_collapsed(section);
    if section == SidebarSection::Albums {
        top_level_drop_target(app, ui, r);
    }
    if let Some(button) = button {
        show_header_button(app, ui, r, &button, pinned);
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
fn show_header_button(app: &mut LightcraftApp, ui: &mut egui::Ui, header: Rect, button: &HeaderButton, pinned: bool) {
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
            Menu::new(crate::i18n::tr(button.tooltip).to_owned())
                .id_salt(("left-header-menu", button.id, pinned))
                .opened_by(&resp.response, Trigger::Click)
                .show(&mut hdr, |ui, _| items(app, ui));
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
        if entry("sidebar", crate::i18n::tr("Hide This Section")).widget("sidebarMenu:hide").show(ui) {
            run = Some(("view.sidebarSection", json!({"section": section.id(), "show": false})));
        }
        for (up, label, widget) in [(true, "Move Up", "sidebarMenu:moveUp"), (false, "Move Down", "sidebarMenu:moveDown")] {
            if entry("sidebar", crate::i18n::tr(label)).enabled(app.ui.sidebar.can_move(section, up)).widget(widget).show(ui) {
                run = Some(("view.sidebarMoveSection", json!({"section": section.id(), "direction": if up { "up" } else { "down" }})));
            }
        }
        rule(ui);
    }
    for s in app.ui.sidebar.sections().iter().filter(|s| s.id.available()) {
        let shown = entry("sidebar-section", crate::i18n::tr(s.id.title())).checked(!s.hidden).widget(format!("sidebarMenu:section:{}", s.id.id())).show(ui);
        if shown {
            run = Some(("view.sidebarSection", json!({"section": s.id.id(), "show": s.hidden})));
        }
    }
    rule(ui);
    if entry("sidebar", crate::i18n::tr("Reset Sidebar Sections")).enabled(!app.ui.sidebar.is_default()).widget("sidebarMenu:reset").show(ui) {
        run = Some(("view.sidebarReset", json!({})));
    }
    if let Some((command, params)) = run
        && let Err(e) = app.run(command, params)
    {
        app.toast(ui.ctx(), e);
    }
}

/// The menu of Albums' +: Create Album, Smart Album, Folder…
fn new_album_menu(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if entry("new-album", crate::i18n::tr("Create Album…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: true });
    }
    if entry("new-album", crate::i18n::tr("Create Smart Album…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::SmartRules {
            id: None,
            name: String::new(),
            rules: lightcraft_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
            parent: None,
        });
    }
    if entry("new-album", crate::i18n::tr("Create Smart Album from Filter…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::NewSmartAlbum { name: String::new(), parent: None });
    }
    if entry("new-album", crate::i18n::tr("Create Folder…")).show(ui) {
        app.ui.dialog = Some(crate::state::Dialog::NewAlbum { name: String::new(), folder: true, parent: None, include_selected: false });
    }
    // only once the albums were put in an order by hand
    if app.session.catalog.album_children_are_ordered(None) {
        rule(ui);
        if entry("new-album", crate::i18n::tr("Sort Albums A–Z")).tip(crate::i18n::tr("Go back to listing them by name")).show(ui) {
            let _ = app.run("album.sort", json!({}));
        }
    }
}

/// The My Photos title and, under it, the name of the open library (so nobody edits in the wrong
/// one, issue #902): a row of the framework's list. Its menu offers the sections' menu too: where
/// hidden sections come back from.
pub(super) fn sidebar_title(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let (_, hr) = ui.allocate_space(vec2(ui.available_width(), TITLE_HEIGHT));
    let library = crate::panels::settings::open_library_name(app);
    let mut row = SideRow::new("sidebarTitle", "folder", crate::i18n::tr("My Photos")).no_icon().strong().role(craft::ui::style::TypeRole::Title).unregistered().detail(library.clone());
    if library.is_some()
        && let Some(l) = app.session.library.as_ref()
    {
        row = row.tip(l.dir.display().to_string());
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(hr).layout(egui::Layout::top_down(egui::Align::Min)));
    let out = side_list(&mut child, SideSpec::new("sidebar-title", crate::i18n::tr("My Photos"), false).flat().keep_focus(), vec![row]);
    if !measuring(ui) && ui.is_enabled() {
        register(ui, "sidebarTitle", hr);
        if library.is_some() {
            register(ui, "label:libraryName", hr);
        }
    }
    let request = out.events.iter().find_map(|event| match event {
        SideEvent::Menu { key, at } => Some((key.clone(), MenuAt::Point(*at))),
        _ => None,
    });
    host_menu(ui, "sidebar-title", crate::i18n::tr("My Photos"), out.response.as_ref(), request, |ui, _| sections_menu(app, ui, None));
}

/// Height of the My Photos title.
const TITLE_HEIGHT: f32 = 44.0;

/// How many library photos have no file (Local browse records are not checked; see
/// `cmd::missing::checked_path`). Checking stats every file, which on a network share takes
/// seconds, so it runs on a worker thread: the count shown is the last finished one, refreshed
/// at most every 5 s, and at once (after the running check) when the catalog changed. The first
/// count and a recount after a catalog change are pending sidebar work
/// ([`super::fs_cached_running`]): the Missing Photos row they add or take away moves every row
/// below it. The timed refresh is not: it rarely changes the count, and on a share where counting
/// takes seconds, a headless run's fast simulated clock would start the next one before settling.
/// A catalog change during a timed refresh is pending from the frame it is seen, although its
/// recount starts only when the refresh ends.
pub(super) fn missing_count(app: &mut LightcraftApp, ui: &mut egui::Ui) -> usize {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::AcqRel, Ordering::Relaxed};
    #[derive(Clone, Default)]
    struct Job {
        n: std::sync::Arc<AtomicUsize>,
        running: std::sync::Arc<AtomicBool>,
        at: f64,
        rev: u64,
        /// A catalog change arrived while a count was running: its recount is pending work
        /// already, held until the recount stores its result.
        owed: bool,
    }
    let id = egui::Id::unique("missing-count");
    let now = ui.input(|i| i.time);
    let rev = app.session.catalog.revision;
    let mut job: Job = ui.data(|d| d.get_temp(id)).unwrap_or(Job { rev: u64::MAX, at: f64::MIN, ..Default::default() });
    let running = job.running.load(Relaxed);
    if running && job.rev != rev && !job.owed {
        // the recount waits for the running count (a timed refresh, not waited for), but settling
        // must wait for it from now on
        super::local::fs_running_counter(ui.ctx()).fetch_add(1, AcqRel);
        job.owed = true;
        ui.data_mut(|d| d.insert_temp(id, job.clone()));
    }
    let fresh = now - job.at < 5.0 && job.rev == rev;
    if !fresh && !running {
        // the same scope as the Missing Photos view: library photos only, never Local browse records
        let paths = lightcraft_engine::cmd::missing::candidates(&app.session.catalog);
        let pending = (job.rev != rev).then(|| super::local::fs_running_counter(ui.ctx()));
        if let Some(p) = &pending
            && !job.owed
        {
            p.fetch_add(1, AcqRel);
        }
        // the recount takes over the entry held while it waited
        job.owed = false;
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
