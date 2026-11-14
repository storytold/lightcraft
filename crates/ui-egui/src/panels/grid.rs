//! Photo Grid (justified rows) and Square Grid. Virtualized: only visible cells request thumbnails.

/// A photo's exposure as a cell shows it: shutter · aperture · ISO · focal length, the parts the
/// file has (`None`: none of them).
pub(crate) fn exposure_line(m: &lcc_catalog::Meta) -> Option<String> {
    let parts: Vec<String> = [
        // whole seconds read "8 s"; the camera notation (1/250, 1.3″) already carries its unit
        (!m.shutter.is_empty())
            .then(|| if m.shutter.contains('/') || m.shutter.ends_with(['s', '″']) { m.shutter.clone() } else { format!("{} s", m.shutter) }),
        m.aperture.map(|a| format!("f/{a:.1}").replace(".0", "")),
        m.iso.map(|i| format!("ISO {i}")),
        m.focal_mm.map(|f| format!("{f:.0} mm")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
#[path = "grid_header_tests.rs"]
mod header_tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lcc_catalog::{Catalog, ColorLabel, DateRun, Flag, GroupBy, PhotoId, SortKey, StackId};
use serde_json::json;

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::render::Slot;
use crate::state::ViewMode;
use crate::theme::Tokens;
use crate::widgets::register;

/// Thumbnail render size (pixels, long edge) for a cell of `pts` points.
fn thumb_px(pts: f32, ppp: f32) -> usize {
    let px = (pts * ppp).ceil() as usize;
    if px <= 256 {
        256
    } else if px <= 384 {
        384
    } else {
        512
    }
}

/// Counters of the grid's work, cumulative over frames (benchmarks and tests: an unchanged frame
/// must not rebuild the view, its layout or its indexes, and visits only the cells near the screen).
#[derive(Clone, Debug, Default)]
pub struct GridStats {
    /// Frames the grid was drawn.
    pub frames: u64,
    /// Cells looked at (tested against the viewport or drawn).
    pub cells_visited: u64,
    /// Layouts computed.
    pub layout_builds: u64,
    /// Per-view data (date runs, row index) built.
    pub view_builds: u64,
    /// Photos whose date and shape were read from the catalog (every photo of a list read whole;
    /// only the photos an edit touched of one followed: [`GridCache`]).
    pub photos_read: u64,
    /// Stack indexes built.
    pub stack_index_builds: u64,
    /// Wall-clock time of the last [`show`].
    pub last_show: web_time::Duration,
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let t0 = web_time::Instant::now();
    // the filmstrip runs under the Grid too, as in Lightroom Classic (the fork's)
    let full = ui.max_rect();
    let film_h = app.ui.film_height();
    if crate::module::edge_visible(app, crate::module::Edge::Bottom) && !app.ui.fullscreen && full.height() > film_h * 2.0 {
        let grid = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - film_h));
        ui.scope_builder(egui::UiBuilder::new().max_rect(grid), |ui| show_inner(app, ui));
        super::detail::filmstrip(app, ui, Rect::from_min_max(pos2(full.left(), grid.bottom()), full.max));
    } else {
        show_inner(app, ui);
    }
    app.caches.grid_stats.frames += 1;
    app.caches.grid_stats.last_show = t0.elapsed();
}

/// Photo → (stack, position in the stack).
type StackIndex = HashMap<PhotoId, (StackId, usize)>;

/// What the date runs depend on: the visible list's generation, the date the grid groups by
/// (`None`: no headers) and the grouping.
type RunsKey = (u64, Option<SortKey>, GroupBy);

/// The most photos an edit may touch for the grid to read only those again (each costs a lookup
/// and a date parse; past this, reading the whole list is as quick).
const MAX_FOLLOWED: usize = 4096;

/// What the grid derives from the visible photos, kept across frames. Everything is keyed by the
/// session's visible-list generation ([`lcc_engine::Session::visible_shared`]), which
/// changes whenever the list is recomputed (any catalog change: crops, orientation, dates, stacks;
/// or another source, filter or sort) — so an unchanged frame neither copies nor hashes the ids.
#[derive(Default)]
pub struct GridCache {
    runs: Option<(RunsKey, Arc<Vec<DateRun>>)>,
    /// Layout by (runs key, width, thumbnail size, square).
    layout: Option<((RunsKey, u32, u32, u32, bool), Arc<GridLayout>)>,
    /// Stack index by generation.
    stacks: Option<(u64, Arc<StackIndex>)>,
    /// How many of the photos are browsed (Local) ones, by generation.
    local: Option<(u64, usize)>,
    /// Collections (plain albums) holding each photo, by generation (the collection badge).
    albums: Option<(u64, Arc<HashMap<PhotoId, usize>>)>,
    /// A published collection's sections (new / modified / published), by generation.
    publish: Option<(u64, Option<Arc<Vec<DateRun>>>)>,
    /// What was read of each photo of the list last shown.
    read: Option<ListRead>,
}

/// Header keys of a published collection's sections, in the order the engine sorts them.
const PUBLISH_SECTIONS: [&str; 4] = ["publish:new", "publish:modified", "publish:published", "publish:remove"];

/// The header of a published collection's section, as Lightroom Classic names them.
fn publish_section_label(key: &str) -> Option<&'static str> {
    Some(match key {
        "publish:new" => "New Photos to Publish",
        "publish:modified" => "Modified Photos to Re-Publish",
        "publish:published" => "Published Photos",
        "publish:remove" => "Deleted Photos to Remove",
        _ => return None,
    })
}

/// A published collection's photos as runs of one section each (`ids` in the engine's order:
/// new, modified, published, to remove); photos in no section are left out.
pub fn publish_runs(sections: &HashMap<PhotoId, u8>, ids: &[PhotoId]) -> Vec<DateRun> {
    let mut runs: Vec<DateRun> = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let Some(key) = sections.get(id).and_then(|r| PUBLISH_SECTIONS.get(usize::from(*r))) else { continue };
        match runs.last_mut() {
            Some(last) if last.key == *key && last.start + last.count == i => last.count += 1,
            _ => runs.push(DateRun { key: (*key).to_string(), label: String::new(), start: i, count: 1 }),
        }
    }
    runs
}

/// What the grid read of each photo of a list, position by position: its own date group (a
/// stack's members take their first photo's when the runs are made) and its cell's shape. A new
/// list that differs from this one by the photos an edit touched (an import's batch, a capture
/// time, a crop) is read again only for those: at 180,000 photos, reading every photo again took
/// most of a 100 ms frame for each batch an import added.
struct ListRead {
    ids: Arc<[PhotoId]>,
    /// The catalog revision it was read at.
    revision: u64,
    /// The date and grouping of the groups (`None`: the list has no date runs, no group is read).
    dates: Option<(SortKey, GroupBy)>,
    /// Each photo's group, an index into `names`.
    groups: Vec<u32>,
    names: Vec<String>,
    named: HashMap<String, u32>,
    /// Each photo's shape; `None` for the square grid, whose cells are all alike.
    shapes: Option<Vec<f32>>,
}

impl ListRead {
    fn new(ids: Arc<[PhotoId]>, revision: u64, dates: Option<(SortKey, GroupBy)>, shaped: bool) -> ListRead {
        let n = ids.len();
        let groups = Vec::with_capacity(if dates.is_some() { n } else { 0 });
        ListRead { ids, revision, dates, groups, names: Vec::new(), named: HashMap::new(), shapes: shaped.then(|| Vec::with_capacity(n)) }
    }

    /// Read photo `id` from the catalog and put it next.
    fn read(&mut self, cat: &Catalog, id: PhotoId) {
        if let Some((key, by)) = self.dates {
            let name = cat.date_group(id, key, by);
            let next = u32::try_from(self.names.len()).unwrap_or(u32::MAX);
            let g = match self.named.get(&name) {
                Some(g) => *g,
                None => {
                    self.named.insert(name.clone(), next);
                    self.names.push(name);
                    next
                }
            };
            self.groups.push(g);
        }
        if let Some(shapes) = self.shapes.as_mut() {
            shapes.push(shape_of(cat, id));
        }
    }

    /// Read every photo of `ids`.
    fn whole(stats: &mut GridStats, cat: &Catalog, ids: &Arc<[PhotoId]>, dates: Option<(SortKey, GroupBy)>, shaped: bool) -> ListRead {
        let mut r = ListRead::new(ids.clone(), cat.revision, dates, shaped);
        if r.dates.is_some() || shaped {
            for id in ids.iter() {
                r.read(cat, *id);
            }
            stats.photos_read += ids.len() as u64;
        }
        r
    }

    /// `ids` from what was read of the list before it: the photos the catalog says changed since
    /// are read again, the others are taken over. `None` when the list is not this one with
    /// those photos changed, added or removed (another view, order or grouping; an edit to more
    /// than photos of its own, such as a stack): read it whole then.
    fn follow(self, stats: &mut GridStats, cat: &Catalog, ids: &Arc<[PhotoId]>, dates: Option<(SortKey, GroupBy)>, shaped: bool) -> Option<ListRead> {
        if self.dates != dates || self.shapes.is_some() != shaped {
            // (a switch between the photo grid and the square grid reads every photo once, on
            // purpose: the other mode's data was never read, so there is nothing to follow)
            return None;
        }
        let touched: HashSet<PhotoId> = cat.touched_since(self.revision)?.into_iter().collect();
        if touched.len() > MAX_FOLLOWED {
            return None;
        }
        let mut r = ListRead::new(ids.clone(), cat.revision, dates, shaped);
        // (the groups' names go on, unpruned: pruning would renumber every photo's group; they are
        // only reset by a whole read, and grow by a short string per day or month an edit puts a
        // photo in that no photo was in before)
        r.names = self.names;
        r.named = self.named;
        let mut was = 0;
        let mut fresh = 0u64;
        for id in ids.iter() {
            if touched.contains(id) {
                r.read(cat, *id);
                fresh += 1;
                continue;
            }
            // (the photos touched are read again wherever they are now)
            while self.ids.get(was).is_some_and(|w| touched.contains(w)) {
                was += 1;
            }
            // an untouched photo is where it was, among the others: else this is another list
            if self.ids.get(was) != Some(id) {
                return None;
            }
            if dates.is_some() {
                r.groups.push(*self.groups.get(was)?);
            }
            if let (Some(to), Some(from)) = (r.shapes.as_mut(), self.shapes.as_ref()) {
                to.push(*from.get(was)?);
            }
            was += 1;
        }
        // no untouched photo left the list
        if self.ids.get(was..).is_some_and(|rest| rest.iter().any(|w| !touched.contains(w))) {
            return None;
        }
        stats.photos_read += fresh;
        Some(r)
    }
}

impl GridCache {
    /// The date runs of `ids`, reading what has to be read of them (`square`: the cells' shapes
    /// are not wanted).
    fn runs(&mut self, stats: &mut GridStats, cat: &Catalog, ids: &Arc<[PhotoId]>, key: RunsKey, square: bool) -> Arc<Vec<DateRun>> {
        let shaped = !square;
        match &self.runs {
            Some((k, r)) if *k == key && self.read.as_ref().is_some_and(|read| read.shapes.is_some() == shaped) => r.clone(),
            _ => {
                stats.view_builds += 1;
                let dates = key.1.map(|sort| (sort, key.2)).filter(|(sort, by)| lcc_catalog::dates::groups_by_date(*sort, *by));
                let read = match self.read.take() {
                    // (the same list, read at this revision)
                    Some(r) if Arc::ptr_eq(&r.ids, ids) && r.revision == cat.revision && r.dates == dates && r.shapes.is_some() == shaped => r,
                    Some(r) => r.follow(stats, cat, ids, dates, shaped).unwrap_or_else(|| ListRead::whole(stats, cat, ids, dates, shaped)),
                    None => ListRead::whole(stats, cat, ids, dates, shaped),
                };
                let r = Arc::new(match dates {
                    Some(_) if !ids.is_empty() => {
                        let stacks = self.stacks(stats, cat, key.0);
                        let group = |i: usize, _: PhotoId| read.groups.get(i).and_then(|g| read.names.get(*g as usize)).map_or("", String::as_str);
                        lcc_catalog::dates::date_runs_of(ids, &stacks, group)
                    }
                    _ => Vec::new(),
                });
                self.read = Some(read);
                self.runs = Some((key, r.clone()));
                r
            }
        }
    }

    fn publish(&mut self, session: &lcc_engine::Session, ids: &[PhotoId], generation: u64) -> Option<Arc<Vec<DateRun>>> {
        match &self.publish {
            Some((g, r)) if *g == generation => r.clone(),
            _ => {
                let r = match session.source {
                    #[cfg(not(target_arch = "wasm32"))]
                    lcc_engine::LibrarySource::Album(a) => lcc_engine::cmd::publish::sections(session, a).map(|m| Arc::new(publish_runs(&m, ids))),
                    _ => None,
                };
                self.publish = Some((generation, r.clone()));
                r
            }
        }
    }

    /// The shape of each cell of the list last shown (width / height), in list order; empty for
    /// the square grid.
    fn shapes(&self) -> &[f32] {
        self.read.as_ref().and_then(|r| r.shapes.as_deref()).unwrap_or(&[])
    }

    /// The date runs and the cell shapes the grid shows now (tests compare them with a grid made
    /// afresh).
    #[cfg(test)]
    pub(crate) fn runs_and_shapes(&self) -> Option<(Arc<Vec<DateRun>>, Vec<f32>)> {
        Some((self.runs.as_ref()?.1.clone(), self.shapes().to_vec()))
    }

    fn stacks(&mut self, stats: &mut GridStats, cat: &Catalog, generation: u64) -> Arc<StackIndex> {
        match &self.stacks {
            Some((g, s)) if *g == generation => s.clone(),
            _ => {
                stats.stack_index_builds += 1;
                let s = Arc::new(cat.stack_index());
                self.stacks = Some((generation, s.clone()));
                s
            }
        }
    }

    fn albums(&mut self, cat: &Catalog, generation: u64) -> Arc<HashMap<PhotoId, usize>> {
        match &self.albums {
            Some((g, m)) if *g == generation => m.clone(),
            _ => {
                let mut m: HashMap<PhotoId, usize> = HashMap::new();
                for al in cat.albums().filter(|a| !a.is_smart() && !a.folder) {
                    for p in &al.photos {
                        *m.entry(*p).or_default() += 1;
                    }
                }
                let m = Arc::new(m);
                self.albums = Some((generation, m.clone()));
                m
            }
        }
    }

    fn local(&mut self, cat: &Catalog, ids: &[PhotoId], generation: u64) -> usize {
        match self.local {
            Some((g, n)) if g == generation => n,
            _ => {
                let n = ids.iter().filter(|id| cat.photo(**id).is_some_and(|p| p.local)).count();
                self.local = Some((generation, n));
                n
            }
        }
    }
}

/// Width / height of a photo's cell: its cropped, oriented shape.
pub(crate) fn shape_of(cat: &Catalog, id: PhotoId) -> f32 {
    let p = cat.photo(id);
    let (w, h) = p.map(|p| (p.width.max(1) as f32, p.height.max(1) as f32)).unwrap_or((3.0, 2.0));
    let swap = p.is_some_and(|p| p.develop.orientation.swaps_axes());
    let crop = p.map(|p| p.develop.crop.geometry.rect).unwrap_or(lcc_geom::Rect::UNIT);
    let (w, h) = if swap { (h, w) } else { (w, h) };
    (w * crop.width() as f32) / (h * crop.height() as f32).max(1e-3)
}

/// The rows (of a layout's `rows`, top to bottom) that meet the y range `top..=bottom`, found by
/// binary search.
pub fn rows_between(rows: &[GridRow], top: f32, bottom: f32) -> &[GridRow] {
    let first = rows.partition_point(|r| r.bottom < top);
    let rest = rows.get(first..).unwrap_or(&[]);
    let n = rest.partition_point(|r| r.top <= bottom);
    rest.get(..n).unwrap_or(&[])
}

/// The photo the grid keeps in place on screen while its layout reflows: the active photo when
/// it is on screen, otherwise the one in the middle of the view. Picked again only when the user
/// scrolls or the active photo changes, never by a reflow, so a run of small resizes does not
/// drift from photo to photo.
#[derive(Clone, Debug, PartialEq)]
pub struct GridAnchor {
    /// Which layout it was last seen in (changes when the layout is rebuilt).
    pub lay: (usize, usize, u32),
    pub photo: PhotoId,
    /// Its index in the photo list last seen (a hint; looked up again when it no longer matches).
    pub idx: usize,
    /// The photo's top minus the view's top, in points.
    pub dy: f32,
    pub seen_top: f32,
    pub active: Option<PhotoId>,
}

impl GridAnchor {
    fn index_in(&self, ids: &[PhotoId]) -> Option<usize> {
        if ids.get(self.idx) == Some(&self.photo) { Some(self.idx) } else { ids.iter().position(|p| *p == self.photo) }
    }

    /// The scroll offset that keeps the anchor where it was, when the layout changed since.
    pub fn reflow_offset(&self, lay_id: (usize, usize, u32), ids: &[PhotoId], lay: &GridLayout) -> Option<f32> {
        // a grid scrolled to the very top stays there (new photos arriving above show)
        if self.lay == lay_id || self.seen_top <= 0.5 {
            return None;
        }
        let cell = lay.cells.get(self.index_in(ids)?)?;
        let y = cell.top() - self.dy;
        y.is_finite().then_some(y.max(0.0))
    }

    /// The anchor after this frame (`viewport`: the grid's visible part, in layout coordinates).
    pub fn follow(
        last: Option<GridAnchor>,
        lay_id: (usize, usize, u32),
        ids: &[PhotoId],
        lay: &GridLayout,
        viewport: Rect,
        active: Option<PhotoId>,
    ) -> Option<GridAnchor> {
        let top = viewport.top();
        if let Some(a) = last {
            let reflowed = a.lay != lay_id;
            let user_moved = !reflowed && (a.seen_top != top || a.active != active);
            if !user_moved && let Some(idx) = a.index_in(ids) {
                return Some(GridAnchor { lay: lay_id, idx, seen_top: top, ..a });
            }
        }
        let on_screen = |i: usize| lay.cells.get(i).is_some_and(|c| c.intersects(viewport));
        let idx = active.and_then(|p| ids.iter().position(|x| *x == p)).filter(|i| on_screen(*i)).or_else(|| {
            let mid = viewport.center().y;
            let row = rows_between(&lay.rows, mid, mid).first().or_else(|| rows_between(&lay.rows, top, viewport.bottom()).first())?;
            Some(row.start + (row.end.saturating_sub(row.start)) / 2)
        })?;
        let (photo, cell) = (*ids.get(idx)?, lay.cells.get(idx)?);
        Some(GridAnchor { lay: lay_id, photo, idx, dy: cell.top() - top, seen_top: top, active })
    }
}

fn show_inner(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let (generation, ids) = app.session.visible_shared();
    // header: source title + count
    let sel_n = app.session.selection.ids.len();
    let chips = lcc_engine::filter_chips(&app.session.filter, &app.session.catalog);
    let total = app.session.source_total();
    let counted = super::chips::count_text(ids.len(), total, !chips.is_empty());
    let cnt = if sel_n > 0 { crate::i18n::tr_format!("{sel_n} selected · {counted}", counted = counted, sel_n = sel_n) } else { counted };
    match app.session.browse.clone().filter(|_| app.session.source == lcc_engine::LibrarySource::Folder) {
        Some(b) => {
            let local = app.caches.grid.local(&app.session.catalog, &ids, generation);
            folder_header(app, ui, &b, &ids, local, &cnt);
        }
        None => {
            let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
            ui.painter().rect_filled(hr, 0.0, t.canvas);
            let full = crate::i18n::source_title(&app.session);
            // a count wider than all but the title's ellipsis (a narrow centre pane) is cut, as a Local folder's is,
            // and reads in full on hover
            let mut bw_taken = 0.0f32;
            let mut count_w =
                ui.painter().layout_no_wrap(cnt.clone(), t.font(12.5), t.text_dim).size().x.ceil().min((hr.width() - 96.0).max(0.0).floor());
            // Recently Deleted: an Empty button left of the count (it asks first)
            if app.session.source == lcc_engine::LibrarySource::RecentlyDeleted && !ids.is_empty() {
                let label = crate::i18n::tr("Empty Recently Deleted");
                let bw = ui.painter().layout_no_wrap(label.to_string(), t.semibold(11.5), t.text).size().x + 18.0;
                let br = egui::Rect::from_center_size(pos2(hr.right() - 30.0 - count_w - bw / 2.0, hr.center().y), vec2(bw, 24.0));
                let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(br));
                if crate::widgets::text_button(&mut bar, "emptyRecentlyDeleted", "Empty Recently Deleted", false).clicked() {
                    let _ = crate::menubar::run_item(app, "library.emptyRecentlyDeleted", serde_json::json!({}));
                }
                count_w += bw + 10.0;
                bw_taken = bw + 10.0;
            }
            // the title gives way to the count: a long album or folder name is trimmed, in full on hover
            let room = (hr.width() - 20.0 - count_w - 36.0).max(0.0);
            let font = t.semibold(17.0);
            let title = crate::widgets::elide_head(&full, room, |s| ui.painter().layout_no_wrap(s.to_string(), font.clone(), t.text).size().x);
            let title_rect = ui.painter().text(pos2(hr.left() + 20.0, hr.center().y), Align2::LEFT_CENTER, &title, font, t.text);
            let count_x = hr.right() - 20.0;
            let count_box = Rect::from_min_max(pos2(count_x - (count_w - bw_taken).max(0.0), hr.top()), pos2(count_x, hr.bottom()));
            let label =
                egui::Label::new(egui::RichText::new(cnt.as_str()).font(t.font(12.5)).color(t.text_dim)).truncate().show_tooltip_when_elided(false);
            let count = ui
                .scope_builder(egui::UiBuilder::new().max_rect(count_box).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
                    ui.add(label)
                })
                .inner;
            let count_rect = count.rect;
            count.on_hover_text(cnt.as_str());
            crate::widgets::register(ui.ctx(), "grid:title", title_rect);
            crate::widgets::register(ui.ctx(), "grid:count", count_rect);
            if title != full {
                ui.interact(title_rect, ui.id().with("grid-title"), Sense::hover()).on_hover_text(full);
            }
        }
    }
    super::chips::show(app, ui, &chips);
    crate::libtools::end_stroke(app, ui);
    crate::libtools::painter_bar(app, ui);
    if app.ui.filter_bar {
        super::filterbar::show(app, ui);
    }
    app.canvas_rect = Some(ui.max_rect());
    if ids.is_empty() {
        if !chips.is_empty() {
            let body = match total {
                Some(n) if n > 0 => {
                    crate::i18n::tr_format!("{n} photos in {} are hidden by the filters above", crate::i18n::source_title(&app.session), n = n)
                }
                _ => "Remove a filter above, or choose Clear all".to_string(),
            };
            super::empty_message(ui, ui.max_rect(), "No photos match the active filters", &body);
        } else if app.session.filter != Default::default() {
            super::empty_message(
                ui,
                ui.max_rect(),
                "No matching photos",
                "A filter is hiding this view's photos: change it, or clear it (View → Clear Filters)",
            );
        } else if app.session.source == lcc_engine::LibrarySource::Folder {
            super::empty_message(ui, ui.max_rect(), "No photos in this folder", "Turn on Include subfolders, or pick another folder under Favorites");
        } else if super::empty_library::shown(&app.session) {
            super::empty_library::show(app, ui, ui.max_rect());
        } else {
            super::empty_message(ui, ui.max_rect(), "No photos", "Import photos with File → Import Photos… (Cmd+Shift+I), or drop them here");
        }
        return;
    }
    let ppp = ui.ctx().pixels_per_point();
    let square = app.ui.view == ViewMode::SquareGrid;
    let target = app.ui.thumb_size;
    let avail_w = ui.available_width() - 8.0;
    let by = resolve_group(app.session.sort.group, target);
    let group_key = match app.session.source {
        lcc_engine::LibrarySource::RecentlyDeleted => None,
        lcc_engine::LibrarySource::RecentlyAdded => Some(SortKey::ImportDate),
        _ => Some(app.session.sort.key),
    };
    let runs_key = (generation, group_key, by);
    // Compact: the culling marks sit in a strip under each picture, which grows every cell
    let footer = if app.compact() { BADGE_STRIP } else { 0.0 };
    let stats = &mut app.caches.grid_stats;
    // (read even for a published collection: the cell shapes the layout uses come with the runs)
    let date_runs = app.caches.grid.runs(stats, &app.session.catalog, &ids, runs_key, square);
    let runs = app.caches.grid.publish(&app.session, &ids, generation).unwrap_or(date_runs);
    // the layout only changes with the photos (and their shapes), the grouping, the width and
    // the thumbnail size
    let lay_key = (runs_key, avail_w.to_bits(), target.to_bits(), footer.to_bits(), square);
    let lay = match &app.caches.grid.layout {
        Some((k, l)) if *k == lay_key => l.clone(),
        _ => {
            stats.layout_builds += 1;
            let ones;
            let shapes = if square {
                ones = vec![1.0; ids.len()];
                &ones[..]
            } else {
                app.caches.grid.shapes()
            };
            let spans: Vec<(usize, usize)> = runs.iter().map(|r| (r.start, r.count)).collect();
            let l = Arc::new(layout_with_footer(shapes, &spans, avail_w, target, square, footer));
            app.caches.grid.layout = Some((lay_key, l.clone()));
            l
        }
    };
    let stacks = app.caches.grid.stacks(stats, &app.session.catalog, generation);
    let albums = if app.ui.lib.cell_badges { Some(app.caches.grid.albums(&app.session.catalog, generation)) } else { None };
    let total_h = lay.height + 12.0;
    let active = app.session.selection.active;
    // bring the active photo into view when it changes (keyboard, click, command) or the grid
    // comes back on screen; otherwise the scroll position is the user's
    let scroll_to: Option<Rect> = if follow_active(ui.ctx(), egui::Id::new("grid-follow-active"), active, app.session.source) {
        active.and_then(|a| ids.iter().position(|x| *x == a)).and_then(|i| lay.cells.get(i).copied())
    } else {
        None
    };
    let mut visible_ids = HashSet::new();
    let mut visited = 0u64;
    // a wheel notch scrolls a third of the grid's height
    let scroll_rect = ui.available_rect_before_wrap();
    register(ui.ctx(), "gridArea", scroll_rect);
    crate::widgets::register(ui.ctx(), "grid:scroll", scroll_rect);
    let wheel = wheel_step(ui, egui::Id::new("grid-wheel"), scroll_rect, scroll_rect.height(), false, app.ui.settings.wheel_scroll_speed);
    // (one notch is `step` points of wheel, positive = up; Scroll By Row counts notches, not points)
    let by_row = app.ui.settings.scroll_by_row;
    let step = (scroll_rect.height() * crate::theme::WHEEL_STRIP_SHARE * app.ui.settings.wheel_scroll_speed.clamp(0.25, 3.0)).max(1.0);
    // keep a photo where it is on screen when the layout reflows (a side panel or the window
    // resized, the thumbnail size or the filter changed): the pixel offset alone would land elsewhere
    let anchor_id = egui::Id::new("grid-anchor");
    let lay_id = (Arc::as_ptr(&lay) as usize, ids.len(), lay.height.to_bits());
    let last: Option<GridAnchor> = ui.ctx().data(|d| d.get_temp(anchor_id));
    // a bar that can be seen and grabbed, its handle always drawn: it says where the view is in the shoot
    visible_scroll_bar(ui);
    let mut area_ui = egui::ScrollArea::vertical()
        .id_salt("grid-scroll")
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
    let mut viewport_seen = Rect::NOTHING;
    if scroll_to.is_none()
        && let Some(y) = last.as_ref().and_then(|a| a.reflow_offset(lay_id, &ids, &lay))
    {
        area_ui = area_ui.vertical_scroll_offset(y);
    }
    area_ui.show_viewport(ui, |ui, viewport| {
        viewport_seen = viewport;
        let next = GridAnchor::follow(last, lay_id, &ids, &lay, viewport, active);
        ui.ctx().data_mut(|d| match next {
            Some(a) => {
                d.insert_temp(anchor_id, a);
            }
            None => {
                d.remove::<GridAnchor>(anchor_id);
            }
        });
        if wheel != egui::Vec2::ZERO {
            // Scroll By Row: the notches become whole rows, ending on a row's top
            let wheel = match (by_row, crate::view_state::row_scroll(&lay.rows, viewport.top(), -wheel.y / step)) {
                (true, Some(to)) => egui::vec2(0.0, viewport.top() - to),
                _ => wheel,
            };
            ui.scroll_with_delta_animation(wheel, egui::style::ScrollAnimation::none());
        }
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), total_h), Sense::hover());
        let origin = area.min;
        app.grid_scroll = Some(viewport.top());
        if let Some(r) = scroll_to {
            ui.scroll_to_rect(r.translate(origin.to_vec2()), None);
        }
        // only the rows on screen and a screen above and below it (their thumbnails are prefetched)
        let prefetch = viewport.expand2(vec2(0.0, viewport.height()));
        for row in rows_between(&lay.rows, prefetch.top(), prefetch.bottom()) {
            for i in row.start..row.end {
                let (Some(&id), Some(&rect)) = (ids.get(i), lay.cells.get(i)) else { continue };
                visited += 1;
                if !rect.intersects(prefetch) {
                    continue;
                }
                visible_ids.insert(id);
                let r = rect.translate(origin.to_vec2());
                let onscreen = rect.intersects(viewport);
                cell(app, ui, id, r, square, onscreen, ppp, i);
                if onscreen {
                    super::cells::extras(app, ui, id, r, square, i, albums.as_deref());
                }
                if let Some((sid, pos)) = stacks.get(&id) {
                    stack_badge(app, ui, id, *sid, *pos, r, square);
                }
            }
        }
        // date headers (top to bottom); the current group's header sticks to the top while its
        // photos scroll by
        let first = lay.headers.partition_point(|h| h.bottom() < viewport.top());
        for (g, hr) in lay.headers.iter().enumerate().skip(first) {
            if hr.top() > viewport.bottom() {
                break;
            }
            if let Some(run) = runs.get(g) {
                group_header(app, ui, run, &ids, hr.translate(origin.to_vec2()), false);
            }
        }
        let sticky = lay.headers.partition_point(|h| h.top() <= viewport.top()).checked_sub(1);
        if let Some(g) = sticky
            && let Some(head) = lay.headers.get(g)
            && head.top() < viewport.top()
            && let Some(run) = runs.get(g)
        {
            // pushed up by the next header as it arrives
            let next_top = lay.headers.get(g + 1).map(|h| h.top()).unwrap_or(f32::INFINITY);
            let y = viewport.top().min(next_top - HEADER_H);
            let hr = Rect::from_min_size(pos2(0.0, y), vec2(area.width(), HEADER_H)).translate(origin.to_vec2());
            group_header(app, ui, run, &ids, hr, true);
        }
    });
    // where the view is in the shoot: "1,234 / 2,233" by the scroll bar while it is dragged or the view scrolls
    scroll_position_label(app, ui, scroll_rect, viewport_seen, total_h, &ids, &lay);
    app.caches.grid_stats.cells_visited += visited;
    app.renderer.evict_thumbs(&visible_ids, 600);
    let _ = (Color32::BLACK, StrokeKind::Inside, Stroke::NONE);
}

/// Should a scrolling photo strip (`key`: the grid, the filmstrip) bring the active photo into
/// view on this pass? Yes when the active photo changed since the strip was last drawn, or when
/// the strip was not drawn on the previous pass (view switch, panel shown again). Otherwise the
/// scroll position is the user's: time passing, finished renders, edits to the photo or a released
/// scroll bar never move it (issue #11).
pub(crate) fn follow_active(ctx: &egui::Context, key: egui::Id, active: Option<PhotoId>, source: lcc_engine::LibrarySource) -> bool {
    let pass = ctx.cumulative_pass_nr();
    let last: Option<(Option<PhotoId>, lcc_engine::LibrarySource, u64)> = ctx.data(|d| d.get_temp(key));
    ctx.data_mut(|d| d.insert_temp(key, (active, source, pass)));
    match last {
        Some((was, where_, drawn)) => follow_changed(was, where_, drawn, active, source, pass),
        None => true,
    }
}

/// Scroll when the active photo changes, the strip was not drawn last pass, or the same photo
/// sits in a different library source (another album) (#1142).
pub(crate) fn follow_changed(
    was: Option<PhotoId>,
    where_was: lcc_engine::LibrarySource,
    drawn: u64,
    active: Option<PhotoId>,
    source: lcc_engine::LibrarySource,
    pass: u64,
) -> bool {
    was != active || where_was != source || drawn + 1 < pass
}

/// How far a mouse wheel scrolls a photo strip this frame (`key`: the grid, the filmstrip; `area`: its
/// rect on screen; `span`: its visible height, or width when `sideways`), for `Ui::scroll_with_delta_animation`
/// inside its `ScrollArea`. A notch scrolls [`crate::theme::WHEEL_STRIP_SHARE`] (a third) of `span`, as in
/// Lightroom (about two rows of a six-row grid), instead of the [`crate::theme::WHEEL_LINE_POINTS`] a side
/// panel scrolls. Not whole rows: the photos land somewhere else after each notch, so the move shows. It
/// jumps at once instead of gliding, so the photos stay sharp while scrolling; egui's glide of the notch is
/// swallowed until it ends. Trackpads report points and scroll as before. A vertical wheel scrolls a
/// `sideways` strip. `speed` scales the step (the Wheel scroll speed preference; clamped, 1 when not finite).
pub(crate) fn wheel_step(ui: &egui::Ui, key: egui::Id, area: Rect, span: f32, sideways: bool, speed: f32) -> egui::Vec2 {
    if !ui.rect_contains_pointer(area) || !span.is_finite() {
        ui.data_mut(|d| d.remove_temp::<bool>(key));
        return egui::Vec2::ZERO;
    }
    let (notches, gliding) = ui.input(|i| {
        let notches = i.events.iter().fold(egui::Vec2::ZERO, |sum, e| match e {
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta, modifiers, .. }
                if *modifiers == egui::Modifiers::NONE && delta.is_finite() =>
            {
                sum + *delta
            }
            _ => sum,
        });
        (notches, i.smooth_scroll_delta != egui::Vec2::ZERO)
    });
    if notches != egui::Vec2::ZERO {
        ui.data_mut(|d| d.insert_temp(key, true));
    } else if !gliding {
        ui.data_mut(|d| d.remove_temp::<bool>(key));
    }
    if !ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) {
        return egui::Vec2::ZERO;
    }
    ui.ctx().input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    let speed = if speed.is_finite() { speed.clamp(0.25, 3.0) } else { 1.0 };
    let step = span * crate::theme::WHEEL_STRIP_SHARE * speed;
    if sideways { egui::vec2((notches.x + notches.y) * step, 0.0) } else { egui::vec2(0.0, notches.y * step) }
}

/// Height of a date header row (points).
pub const HEADER_H: f32 = 40.0;

/// Cell rectangles (one per photo, in order), date-header rectangles (one per group, top to
/// bottom), the rows of cells (top to bottom, for finding the visible ones by binary search) and
/// the total height of the grid content.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridLayout {
    pub cells: Vec<Rect>,
    pub headers: Vec<Rect>,
    pub rows: Vec<GridRow>,
    pub height: f32,
}

/// The cell straight above (`down` false) or below cell `i`: in the adjacent row, the one whose
/// centre is nearest horizontally (justified rows don't line up in columns). None in the first /
/// last row or for an index outside the layout.
pub fn vertical_neighbor(lay: &GridLayout, i: usize, down: bool) -> Option<usize> {
    let row = lay.rows.iter().position(|r| (r.start..r.end).contains(&i))?;
    let next = if down { row.checked_add(1)? } else { row.checked_sub(1)? };
    let target = lay.rows.get(next)?;
    let x = lay.cells.get(i)?.center().x;
    (target.start..target.end).filter_map(|k| Some((k, (lay.cells.get(k)?.center().x - x).abs()))).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(k, _)| k)
}

/// Up / Down in the grids: how many photos `library.up` / `library.down` move to reach the cell
/// above / below the active one in the grid as last laid out; None when the grid isn't laid out
/// for the current photos (the filmstrip then steps one photo).
pub fn row_distance(app: &mut LccApp, down: bool) -> Option<u64> {
    if !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
        return None;
    }
    let active = app.session.selection.active?;
    let (generation, _) = app.session.visible_shared();
    let i = app.session.visible_position(active)?;
    let (key, lay) = app.caches.grid.layout.as_ref()?;
    if key.0.0 != generation {
        return None;
    }
    // at the top / bottom row there's no cell to go to: stay put rather than step sideways
    let j = vertical_neighbor(lay, i, down).unwrap_or(i);
    Some(j.abs_diff(i) as u64)
}

/// One row of cells: `cells[start..end]`, between `top` and `bottom`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridRow {
    pub top: f32,
    pub bottom: f32,
    pub start: usize,
    pub end: usize,
}

/// The grouping actually shown: `Auto` is days with large thumbnails, months with medium ones and
/// years when zoomed far out.
pub fn resolve_group(by: GroupBy, thumb_size: f32) -> GroupBy {
    match by {
        GroupBy::Auto if thumb_size >= 150.0 => GroupBy::Day,
        GroupBy::Auto if thumb_size >= 110.0 => GroupBy::Month,
        GroupBy::Auto => GroupBy::Year,
        other => other,
    }
}

/// Lay out the grid: `aspects` (width / height per photo; ignored for the square grid) in
/// `groups` (`(start, count)` runs; empty = one group without a header), `avail_w` wide, rows
/// about `target` high (square cells about `target` wide). Every group starts on a new row
/// below its header. Justified rows fill the width exactly except a group's last row, which keeps
/// the target height.
pub fn layout(aspects: &[f32], groups: &[(usize, usize)], avail_w: f32, target: f32, square: bool) -> GridLayout {
    layout_with_footer(aspects, groups, avail_w, target, square, 0.0)
}

/// [`layout`] with a `footer` strip (points) under every cell's picture: the ratings, flags and
/// labels the Compact layout shows under the thumbnails rather than over them. The pictures keep
/// their sizes; each cell grows by the footer.
pub fn layout_with_footer(aspects: &[f32], groups: &[(usize, usize)], avail_w: f32, target: f32, square: bool, footer: f32) -> GridLayout {
    let footer = if footer.is_finite() { footer.max(0.0) } else { 0.0 };
    let n = aspects.len();
    let whole = [(0, n)];
    let (groups, headed) = if groups.is_empty() { (&whole[..], false) } else { (groups, true) };
    let gap = if square { 1.0 } else { 6.0 };
    let mut out = GridLayout { cells: vec![Rect::NOTHING; n], headers: Vec::with_capacity(groups.len()), rows: Vec::new(), height: 0.0 };
    let mut y = 4.0f32;
    for &(start, count) in groups {
        let end = (start + count).min(n);
        if headed {
            out.headers.push(Rect::from_min_size(pos2(0.0, y), vec2(avail_w + 8.0, HEADER_H)));
            y += HEADER_H;
        }
        if start >= end {
            continue;
        }
        if square {
            let cols = ((avail_w + gap) / (target + gap)).floor().max(1.0);
            let cw = (avail_w - gap * (cols - 1.0)) / cols;
            for i in start..end {
                let k = (i - start) as f32;
                let (c, r) = (k % cols, (k / cols).floor());
                out.cells[i] = Rect::from_min_size(pos2(4.0 + c * (cw + gap), y + r * (cw + footer + gap)), vec2(cw, cw + footer));
            }
            let per_row = (cols as usize).max(1);
            for (r, row_start) in (start..end).step_by(per_row).enumerate() {
                let top = y + r as f32 * (cw + footer + gap);
                out.rows.push(GridRow { top, bottom: top + cw + footer, start: row_start, end: (row_start + per_row).min(end) });
            }
            let rows = ((end - start) as f32 / cols).ceil();
            y += rows * (cw + footer + gap);
        } else {
            // justified rows: accumulate aspect ratios until the row is full
            let mut i = start;
            while i < end {
                let mut sum = 0.0;
                let mut j = i;
                while j < end {
                    sum += aspects[j].max(0.05);
                    let h = (avail_w - gap * (j - i) as f32) / sum;
                    j += 1;
                    if h <= target {
                        break;
                    }
                }
                let row_h = (avail_w - gap * (j - i - 1) as f32) / sum;
                let h = if j < end || row_h <= target { row_h } else { target };
                let mut x = 4.0;
                for k in i..j {
                    let w = aspects[k].max(0.05) * h;
                    out.cells[k] = Rect::from_min_size(pos2(x, y), vec2(w, h + footer));
                    x += w + gap;
                }
                out.rows.push(GridRow { top: y, bottom: y + h + footer, start: i, end: j });
                y += h + footer + gap;
                i = j;
            }
        }
        if headed {
            y += 8.0;
        }
    }
    out.height = y;
    out
}

/// The ring round a selected photo, drawn outside `photo`. The active one is 3 pt in
/// [`Tokens::ring_active`] with a 1 pt line in [`Tokens::ring_inner`] over its inner edge, against
/// the photo (so the ring reads on a white photo and on a black one). The others are a single 1.5 pt line in
/// [`Tokens::ring_selected`]; the two are told apart by weight, not by lightness alone.
pub(crate) fn selection_ring(p: &egui::Painter, t: &Tokens, photo: Rect, state: lcc_engine::SelectionState) {
    use lcc_engine::SelectionState;
    match state {
        SelectionState::Active => {
            // both on the same rect, so no seam opens between them at a fractional scale
            p.rect_stroke(photo, 0.0, Stroke::new(3.0, t.ring_active), StrokeKind::Outside);
            p.rect_stroke(photo, 0.0, Stroke::new(1.0, t.ring_inner), StrokeKind::Outside);
        }
        SelectionState::Selected => {
            p.rect_stroke(photo, 0.0, Stroke::new(1.5, t.ring_selected), StrokeKind::Outside);
        }
        SelectionState::NotSelected => {}
    }
}

/// A date header: "Wednesday, 30 September 2026 · 12 photos". Clicking it selects the group
/// (Cmd/Shift: adds to the selection).
fn group_header(app: &mut LccApp, ui: &mut egui::Ui, run: &DateRun, ids: &[PhotoId], r: Rect, pinned: bool) {
    let t = Tokens::get(ui.ctx());
    let resp = ui.interact(r, egui::Id::new(("group-header", run.start, pinned)), Sense::click());
    crate::widgets::announce(&resp, if run.key.is_empty() { crate::i18n::tr("Unknown date").to_string() } else { run.key.clone() });
    if !pinned {
        register(ui.ctx(), format!("group:{}", if run.key.is_empty() { "unknown" } else { &run.key }), r);
    }
    let p = ui.painter();
    if pinned {
        p.rect_filled(r, 0.0, t.canvas);
        p.hline(r.x_range(), r.bottom(), Stroke::new(1.0, t.divider));
    }
    let label = match publish_section_label(&run.key) {
        Some(l) => crate::i18n::tr(l).to_string(),
        None => crate::i18n::date_group_label(&run.key, false),
    };
    let g = p.layout_no_wrap(label, t.semibold(15.0), if resp.hovered() { t.text } else { t.text_label });
    let x = r.left() + 8.0;
    let gw = g.size().x;
    p.galley(pos2(x, r.center().y - g.size().y / 2.0), g, t.text);
    let n = run.count;
    p.text(
        pos2(x + gw + 10.0, r.center().y),
        Align2::LEFT_CENTER,
        crate::i18n::tr_format!("· {n} photo{}", if n == 1 { "" } else { "s" }, n = n),
        t.font(12.5),
        t.text_dim,
    );
    if resp.clicked() {
        let group: Vec<u64> = ids[run.start..(run.start + run.count).min(ids.len())].iter().map(|p| p.0).collect();
        let m = ui.input(|i| i.modifiers);
        let mode = if m.command || m.shift { "add" } else { "replace" };
        let _ = app.run("library.select", json!({"ids": group, "mode": mode}));
    }
}

/// Request a grid/filmstrip thumbnail at `priority`. An unedited raw without a thumbnail texture
/// gets its embedded camera preview, and the render of the raw behind it follows in the background
/// — except where that render would only redraw what the embedded JPEG already shows (see
/// [`lcc_engine::media::Session::embedded_thumb_is_final`]), which for a folder of raws on a
/// network share is the difference between reading every file whole and reading a preview each.
pub fn request_thumb(app: &mut LccApp, id: PhotoId, size: usize, priority: u32) {
    // A slider drag changes the photos it edits every frame (the active one, the others of a
    // multi-photo edit, and those Auto Sync carries along). Under the loupe their thumbnails follow
    // once the drag ends, so their renders (a raw decode the first time) don't compete with the
    // loupe's drafts. In a grid the thumbnails are what the user watches: they follow live, one
    // render at a time per photo (the newest settings when the last one is in).
    if drag_edits(app.session.interaction.as_ref(), id) {
        let loupe = matches!(app.ui.view, ViewMode::Detail | ViewMode::Compare | ViewMode::Reference);
        if drag_waits(loupe, app.renderer.thumb(id).is_some(), app.renderer.is_pending(Slot::Thumb(id))) {
            return;
        }
    }
    // painting on the loupe: the other photos a stroke reached (Auto Sync) keep the thumbnail they have until the
    // strokes pause, so their renders don't take the workers from the photo on screen (ported from OneSlips,
    // NOTES-1028 note 29); one without any thumbnail still gets one
    if app.paint_hold && app.session.active() != Some(id) && app.renderer.thumb(id).is_some() {
        return;
    }
    // never less than the photo has or is getting: a smaller ask (the filmstrip beside a larger view) moving the
    // job up would otherwise swap it for a smaller one, and the larger view swap it back
    retry_found_original(app, id);
    let size = app.session.catalog.photo(id).and_then(|p| app.renderer.thumb_bucket_kept(p)).map_or(size, |kept| kept.max(size));
    let bucket = lcc_engine::media::thumb_bucket(size);
    if let Some(photo) = app.session.catalog.photo(id)
        && (app.renderer.thumb_current(photo, bucket, priority) || app.renderer.thumb_covers(photo, bucket))
    {
        return;
    }
    // The camera's embedded preview is this photo's thumbnail (unedited raw): once its quick render
    // has arrived there is nothing left to ask for. Without this the grid would rebuild the render
    // job — and its cache key — on every frame, which is what the embedded-thumbnail record settles.
    if let Some(photo) = app.session.catalog.photo(id)
        && (app.renderer.thumb_quick_is_final(photo, bucket, priority) || app.renderer.thumb_quick_pending(photo, bucket, priority))
    {
        return;
    }
    let Some(job) = app.session.thumb_job(id, size) else { return };
    #[cfg(test)]
    {
        app.renderer.thumb_jobs_built += 1;
    }
    // An edited raw's camera JPEG is only a stand-in: for a cell with no picture at all. One that
    // shows a picture already (the look before this edit) keeps it until the real render arrives.
    let shows_stand_in = app.renderer.textures.contains_key(&Slot::ThumbQuick(id))
        && app.session.catalog.photo(id).is_some_and(|p| !app.session.embedded_thumb_is_final(p));
    let quick = if app.renderer.textures.contains_key(&Slot::Thumb(id)) || shows_stand_in { None } else { app.session.quick_thumb_job(&job) };
    // A cached render is a stand-in for whatever comes next; an embedded camera preview of an
    // unedited raw is the picture itself, so no sensor decode is scheduled behind it.
    let embedded_is_final =
        quick.is_some() && app.session.catalog.photo(id).is_some_and(|p| skips_the_real_render(app.session.embedded_thumb_is_final(p), true));
    if let Some(photo) = app.session.catalog.photo(id) {
        if embedded_is_final {
            app.renderer.remember_embedded_thumb(photo, bucket, job.key);
        } else {
            app.renderer.remember_thumb(photo, bucket, job.key, quick.is_some());
        }
    }
    match quick {
        Some(q) => {
            app.renderer.request_quick(Slot::ThumbQuick(id), q, stand_in_priority(priority));
            // (the real thumbnail behind the stand-in is background work: not while a cell on
            // screen waits for its first picture)
            if !embedded_is_final && !app.renderer.is_pending(Slot::ThumbQuick(id)) && !app.pictures_busy(app.renderer.frame()) {
                app.renderer.request(Slot::Thumb(id), job, BACKGROUND_THUMB_PRIORITY);
            }
        }
        // (the render behind a stand-in on screen is background work, as above)
        None if shows_stand_in => {
            if !app.pictures_busy(app.renderer.frame()) {
                app.renderer.request(Slot::Thumb(id), job, BACKGROUND_THUMB_PRIORITY);
            }
        }
        None => app.renderer.request(Slot::Thumb(id), job, priority),
    }
}

/// A cell that shows photo `id`'s kept thumbnail ([`lcc_engine::media::QuickSource::Kept`]) or a failure asks
/// whether its original is there (a cached answer, checked off the UI thread), and asks for its thumbnail again
/// once the original is back: a drive mounted while the app runs ([`crate::render::Renderer::retry_found_original`]).
/// Other cells ask nothing.
fn retry_found_original(app: &mut LccApp, id: PhotoId) {
    let kept = app.renderer.thumb(id).is_some_and(|t| t.quick == Some(lcc_engine::media::QuickSource::Kept));
    if !kept && app.renderer.thumb_failure(id).is_none() {
        return;
    }
    let Some(lcc_catalog::Source::File { path }) = app.session.catalog.photo(id).map(|p| &p.source) else { return };
    let availability = &app.session.media.availability;
    match availability.exists(path) {
        Some(false) => app.renderer.note_missing_original(id),
        Some(true) => app.renderer.retry_found_original(id, kept, availability.gone_generation()),
        None => {}
    }
}

/// A grid or filmstrip cell of photo `id` is on screen: while its first picture is on its way,
/// every background job waits ([`LccApp::pictures_missing`]; not for a cell whose picture failed).
pub(crate) fn note_cell(app: &mut LccApp, id: PhotoId) {
    if app.renderer.thumb(id).is_none() && (app.renderer.is_pending(Slot::ThumbQuick(id)) || app.renderer.is_pending(Slot::Thumb(id))) {
        app.pictures_missing(app.renderer.frame());
    }
}

/// The stand-in (embedded JPEG or cached thumbnail) of a cell on screen: the cheapest picture for
/// the whole view comes before anything bigger but the loupe's own stand-in (110), so before every
/// render ("lowest and fastest resolution first, then the next cheap one, then anything bigger").
pub const STAND_IN_ON_SCREEN: u32 = 97;

/// The priority of a cell's stand-in when the cell asks at `priority` (10: on screen).
pub(crate) fn stand_in_priority(priority: u32) -> u32 {
    if (10..STAND_IN_ON_SCREEN).contains(&priority) { STAND_IN_ON_SCREEN } else { priority.saturating_add(1) }
}

/// Thickness (points) of the grid's and the filmstrip's scroll bars.
pub const SCROLL_BAR_W: f32 = 7.5;

/// A scroll bar that can be seen and grabbed: solid, [`SCROLL_BAR_W`] wide, its handle drawn in the
/// theme's text colour (egui's default floats thin over the photos and fades out).
pub(crate) fn visible_scroll_bar(ui: &mut egui::Ui) {
    let s = &mut ui.style_mut().spacing.scroll;
    *s = egui::style::ScrollStyle::solid();
    s.bar_width = SCROLL_BAR_W;
    s.handle_min_length = 32.0;
    s.bar_inner_margin = 2.0;
    s.foreground_color = true;
    s.dormant_background_opacity = 0.25;
    s.active_background_opacity = 0.35;
    s.interact_background_opacity = 0.45;
    s.dormant_handle_opacity = 0.55;
    s.active_handle_opacity = 0.8;
    s.interact_handle_opacity = 1.0;
}

/// A count with its thousands grouped ("2,233"; a no-break space outside English).
pub(crate) fn group_digits(n: usize) -> String {
    let sep = if crate::i18n::language() == crate::i18n::Locale::En { ',' } else { '\u{a0}' };
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// While the grid scrolls (or its bar is dragged): a label by the bar saying where the view is in
/// the shoot ("1,234 / 2,233" and the capture time of the photos there), in the theme's colours.
fn scroll_position_label(app: &LccApp, ui: &mut egui::Ui, area: Rect, viewport: Rect, total_h: f32, ids: &[PhotoId], lay: &GridLayout) {
    if !viewport.is_positive() || total_h <= viewport.height() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let moving_id = egui::Id::new("grid-scroll-moving");
    let now = ui.input(|i| i.time);
    let last_top: Option<(f32, f64)> = ui.data(|d| d.get_temp(moving_id));
    let moved = last_top.is_none_or(|(y, _)| (y - viewport.top()).abs() > 0.5);
    let since = if moved { now } else { last_top.map_or(now, |(_, at)| at) };
    ui.data_mut(|d| d.insert_temp(moving_id, (viewport.top(), since)));
    let dragging = ui.input(|i| i.pointer.primary_down())
        && ui.input(|i| i.pointer.press_origin()).is_some_and(|p| p.x > area.right() - 16.0 && area.contains(p));
    if last_top.is_none() || (now - since > 0.9 && !dragging) {
        return;
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    let Some(row) = rows_between(&lay.rows, viewport.center().y, viewport.bottom()).first() else { return };
    let Some(id) = ids.get(row.start) else { return };
    let mut text = format!("{} / {}", group_digits(row.start.saturating_add(1)), group_digits(ids.len()));
    if let Some(date) = app.session.catalog.photo(*id).and_then(|p| p.captured.clone()) {
        text.push_str(" · ");
        text.push_str(&crate::i18n::display_time(&date));
    }
    let frac = (viewport.top() / (total_h - viewport.height()).max(1.0)).clamp(0.0, 1.0);
    let y = area.top() + 18.0 + frac * (area.height() - 36.0).max(0.0);
    let g = ui.painter().layout_no_wrap(text, t.semibold(12.0), t.text);
    let r = Rect::from_min_size(pos2(area.right() - 22.0 - g.size().x - 16.0, y - (g.size().y + 8.0) / 2.0), g.size() + vec2(16.0, 8.0));
    let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("grid-scroll-label")));
    p.rect(r, 6.0, t.chrome, Stroke::new(1.0, t.divider), StrokeKind::Inside);
    p.galley(r.min + vec2(8.0, 4.0), g, t.text);
    register(ui.ctx(), "grid:scrollPosition", r);
}

/// The `library.select` mode of a click on a photo cell, as in Lightroom Classic: Shift = range, ⌘/Ctrl = toggle; a
/// plain click on the picture of a selected photo makes it the active one and keeps the selection, a plain click on
/// the frame around the picture selects that photo alone.
pub(crate) fn click_mode(m: egui::Modifiers, on_picture: bool) -> &'static str {
    if m.shift {
        "range"
    } else if m.command {
        "toggle"
    } else if on_picture {
        "click"
    } else {
        "replace"
    }
}

/// Whether the running interaction (a slider drag, a turning dial) edits photo `id`: the active
/// photo, the others of a multi-photo edit, or one Auto Sync moves along with it.
pub(crate) fn drag_edits(interaction: Option<&lcc_engine::Interaction>, id: PhotoId) -> bool {
    interaction.is_some_and(|i| i.photo == id || i.others.iter().chain(&i.along).any(|(x, _)| *x == id))
}

/// During a slider drag, whether a photo it edits skips asking for a new thumbnail this frame:
/// under the loupe (`loupe`) while it has one, anywhere while one is being rendered (`pending`);
/// in a grid it follows the drag.
pub(crate) fn drag_waits(loupe: bool, has_thumb: bool, pending: bool) -> bool {
    (loupe && has_thumb) || pending
}

/// Whether a photo's grid thumbnail is already the picture it should show, so the render of the raw
/// behind it is skipped. `embedded_is_final` is the engine's answer
/// ([`lcc_engine::media::Session::embedded_thumb_is_final`]: the photo is an unedited raw
/// whose camera JPEG the viewer is showing) and `from_embedded` says the stand-in actually came from
/// the embedded preview rather than a cached render of the file.
///
/// Skipping matters where the file is expensive to read: decoding a 56 MB Fuji RAF to draw a 512 px
/// thumbnail — which the 4 416 × 2 944 embedded JPEG already covers — cost 45 s a photo over a
/// 1.2 MB/s share, and the queue it built delayed the render the user was waiting for.
pub(crate) fn skips_the_real_render(embedded_is_final: bool, from_embedded: bool) -> bool {
    embedded_is_final && from_embedded
}

/// Rendered thumbnails replacing embedded previews: after everything on screen.
pub const BACKGROUND_THUMB_PRIORITY: u32 = 3;

#[allow(clippy::too_many_arguments)]
/// A quiet cue on a thumbnail's placeholder while the picture is still on its way (an unreadable one
/// says "!" instead): three small dots in the middle, in the icon color (3:1 on every theme's placeholder). It stands still, so
/// Reduce motion has nothing to stop. It is left off tiles too small to hold it: judged by the
/// `tile` (the placeholder's cell, smaller than 32 pt either way) and by the `rect` (the
/// placeholder, which must have room for the dots). Registered as `{prefix}{id}` (the grid's
/// `badge:loading:{id}`, the filmstrip's `film:badge:loading:{id}`).
pub(crate) fn paint_loading_cue(p: &egui::Painter, t: &Tokens, tile: Rect, rect: Rect, prefix: &str, id: u64) {
    const RADIUS: f32 = 1.75;
    const GAP: f32 = 7.0;
    const CUE_W: f32 = 2.0 * (GAP + RADIUS);
    const CUE_H: f32 = 2.0 * RADIUS;
    if tile.width().min(tile.height()) < 32.0 || rect.width() < CUE_W || rect.height() < CUE_H {
        return;
    }
    let middle = rect.center();
    for dx in [-GAP, 0.0, GAP] {
        p.circle_filled(middle + vec2(dx, 0.0), RADIUS, t.icon);
    }
    register(p.ctx(), format!("{prefix}{id}"), Rect::from_center_size(middle, vec2(CUE_W, CUE_H)));
}

fn cell(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, r: Rect, square: bool, onscreen: bool, ppp: f32, index: usize) {
    let t = Tokens::get(ui.ctx());
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    let resp = ui.interact(r, egui::Id::new(("cell", id.0)), Sense::click_and_drag());
    if resp.hovered() {
        // the secondary window's Live loupe follows the pointer
        app.ui.hovered_photo = Some(id.0);
    }
    register(ui.ctx(), format!("thumb:{}", id.0), r);
    let state = app.session.selection.state_of(id);
    let selected = state != lcc_engine::SelectionState::NotSelected;
    // screen readers: the file, then rating / flag / label
    let mut spoken = photo.file_name.clone();
    if photo.rating > 0 {
        spoken.push_str(&crate::i18n::tr_format!(", {} star{}", photo.rating, if photo.rating == 1 { "" } else { "s" }));
    }
    match photo.flag {
        lcc_catalog::Flag::Pick => spoken.push_str(crate::i18n::tr(", picked")),
        lcc_catalog::Flag::Reject => spoken.push_str(crate::i18n::tr(", rejected")),
        lcc_catalog::Flag::None => {}
    }
    if let Some(l) = photo.label {
        spoken.push_str(&crate::i18n::tr_format!(", {} label", crate::i18n::color_label(&app.session.catalog, l)));
    }
    spoken.push_str(&crate::video_ui::spoken_suffix(&photo));
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &spoken));
    let p = ui.painter();
    // Compact: ratings, flags and labels in a strip under the picture (they cover nothing), the
    // cell's level telling unselected, picked, rejected, selected and active apart
    let under = app.compact();
    let strip = if under { BADGE_STRIP.min(r.height() * 0.4) } else { 0.0 };
    let pic = Rect::from_min_max(r.min, pos2(r.right(), r.bottom() - strip));
    let bar = Rect::from_min_max(pos2(r.left(), pic.bottom()), r.max);
    if under {
        let base = if square { t.cell } else { t.canvas };
        let fill = crate::theme::label_background(cell_fill(&t, state, photo.flag, resp.hovered(), base), photo.label, selected);
        p.rect_filled(if square { r } else { bar }, 0.0, fill);
        // the flag's glow behind the picture: it shows around and under it, never over it
        flag_glow(p, &t, r, photo.flag);
    }
    let img_rect = if square {
        if !under {
            let base = if selected { t.cell_selected } else { t.cell };
            p.rect_filled(r, 0.0, crate::theme::label_background(base, photo.label, selected));
        }
        super::cells::image_rect(app, pic)
    } else {
        pic
    };
    // thumbnail
    let size = thumb_px(img_rect.width().max(img_rect.height()), ppp);
    request_thumb(app, id, size, if onscreen { 10 } else { 5 });
    if onscreen {
        note_cell(app, id);
    }
    if let Some(tex) = app.renderer.thumb(id) {
        let [tw, th] = tex.size;
        // Preserve the decoded preview's ratio when it differs from the catalog dimensions.
        let fit = super::detail::fit_texture_rect(img_rect, [tw, th]);
        p.image(tex.id(), fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        selection_ring(p, &t, fit, state);
    } else {
        let ph = img_rect.shrink(if square { 20.0 } else { 0.0 });
        p.rect_filled(ph, 0.0, t.skeleton);
        // not drawn yet (or unreadable): the selection is still visible
        selection_ring(p, &t, ph, state);
        if app.renderer.thumb_failure(id).is_some() {
            // unreadable / missing file
            p.text(ph.center(), Align2::CENTER_CENTER, "!", t.semibold(18.0), t.text);
        } else {
            paint_loading_cue(p, &t, img_rect, ph, "badge:loading:", id.0);
        }
    }
    crate::video_ui::grid_badge(ui, img_rect, &photo);
    // labels and badges
    if square && app.ui.lib.cell_style == crate::libtools::CellStyle::Expanded {
        super::cells::expanded_header(app, ui, &photo, r, index);
    } else if square && app.ui.show_filenames {
        let m = &photo.meta;
        let name = match app.ui.grid_info.as_str() {
            "exposure" => exposure_line(m).unwrap_or_default(),
            "date" => photo.captured.as_deref().map(crate::i18n::display_time).unwrap_or_else(|| crate::i18n::tr("No date").into()),
            _ => photo.file_name.rsplit_once('.').map(|(n, _)| n.to_string()).unwrap_or(photo.file_name.clone()),
        };
        p.text(pos2(r.left() + 8.0, r.top() + 12.0), Align2::LEFT_CENTER, name, t.font(10.5), t.text_dim);
        let fmt = photo.format.clone();
        let g = p.layout_no_wrap(fmt, t.semibold(9.0), t.text_label);
        let br = Rect::from_min_size(pos2(r.right() - g.size().x - 14.0, r.top() + 5.0), g.size() + vec2(8.0, 3.0));
        p.rect_filled(br, 2.0, Color32::from_gray(26));
        p.galley(br.min + vec2(4.0, 1.5), g, t.text_label);
    }
    // the Import tab: what each frame was shot at, in a strip over the photo's top edge (the
    // cull of a card reads shutter, aperture, ISO and focal length at a glance)
    if crate::import_tab::showing(app)
        && !(square && app.ui.show_filenames && app.ui.grid_info == "exposure")
        && let Some(line) = exposure_line(&photo.meta)
    {
        let strip = Rect::from_min_max(img_rect.left_top(), pos2(img_rect.right(), (img_rect.top() + 17.0).min(img_rect.bottom())));
        if strip.width() >= 60.0 {
            p.rect_filled(strip, 0.0, Color32::from_black_alpha(140));
            let g = crate::widgets::one_line(p, &line, t.font(10.5), Color32::from_gray(225), strip.width() - 10.0);
            p.galley(pos2(strip.left() + 5.0, strip.center().y - g.size().y / 2.0), g, Color32::from_gray(225));
            register(ui.ctx(), format!("cellInfo:{}", id.0), strip);
        }
    }
    let expanded = square && app.ui.lib.cell_style == crate::libtools::CellStyle::Expanded;
    let show_badges = expanded
        || match app.ui.settings.grid_badges {
            crate::state::GridBadges::Auto => resp.hovered() || selected || photo.rating > 0 || photo.flag != Flag::None || photo.label.is_some(),
            crate::state::GridBadges::Always => true,
            crate::state::GridBadges::Never => false,
        };
    if under {
        let marks = bar.shrink2(vec2(if square { 8.0 } else { 3.0 }, 1.0));
        register(ui.ctx(), format!("cellmarks:{}", id.0), marks);
        if show_badges || photo.label.is_some() {
            paint_badges(p, &t, marks, &photo, false, app.session.catalog.is_vignetting_reference(photo.id));
        }
    }
    let badge_bar = Rect::from_min_max(pos2(img_rect.left(), img_rect.bottom() - 24.0), img_rect.right_bottom());
    if !under && !square && photo.label.is_some() {
        // Justified photos fill the whole cell, so the label shows as a translucent tinted footer
        // over the photo's bottom edge (opaque, it would hide 24 px of every labelled photo).
        p.rect_filled(badge_bar, 0.0, crate::theme::label_background(t.cell, photo.label, selected).gamma_multiply(0.7));
    }
    if show_badges && !under {
        let bar = badge_bar;
        if (resp.hovered() || selected) && (square || photo.label.is_none()) {
            p.rect_filled(bar, 0.0, Color32::from_black_alpha(120));
        }
        let mut x = bar.left() + 6.0;
        x += crate::rating::paint_rating(p, pos2(x, bar.center().y), photo.rating, crate::rating::RatingSize::Medium, t.star) + 4.0;
        match photo.flag {
            Flag::Pick => paint(p, Rect::from_min_size(pos2(x, bar.center().y - 7.0), vec2(14.0, 14.0)), Icon::FlagPickFilled, t.pick),
            Flag::Reject => paint(p, Rect::from_min_size(pos2(x, bar.center().y - 7.0), vec2(14.0, 14.0)), Icon::FlagReject, t.reject),
            Flag::None => {}
        }
        let mut right = bar.right();
        if photo.is_edited() {
            paint(p, Rect::from_min_size(pos2(right - 20.0, bar.center().y - 7.0), vec2(14.0, 14.0)), Icon::Sliders, t.text_label);
            right -= 20.0;
        }
        if photo.exported.is_some() {
            paint(p, Rect::from_min_size(pos2(right - 20.0, bar.center().y - 7.0), vec2(14.0, 14.0)), Icon::Share, t.text_label);
            right -= 20.0;
        }
        if let Some(l) = photo.label {
            p.circle(pos2(right - 12.0, bar.center().y), 5.0, label_color(l), Stroke::new(1.0, Color32::from_black_alpha(120)));
        }
    }
    if let Some(name) = photo.virtual_copy_name() {
        // virtual copy: a folded-corner tag at the top right
        let g = p.layout_no_wrap(name.to_string(), t.semibold(10.5), Color32::WHITE);
        let tr = if square { img_rect.right_top() + vec2(-6.0, 6.0) } else { img_rect.right_top() + vec2(-5.0, 5.0) };
        let br = Rect::from_min_max(pos2(tr.x - g.size().x - 26.0, tr.y), pos2(tr.x, tr.y + 20.0));
        p.rect_filled(br, 10.0, Color32::from_black_alpha(165));
        let c = pos2(br.left() + 13.0, br.center().y);
        let s = 5.0;
        p.add(egui::Shape::convex_polygon(
            vec![c + vec2(-s, -s), c + vec2(s * 0.3, -s), c + vec2(s, -s * 0.3), c + vec2(s, s), c + vec2(-s, s)],
            Color32::TRANSPARENT,
            Stroke::new(1.2, Color32::WHITE),
        ));
        p.line_segment([c + vec2(s * 0.3, -s), c + vec2(s * 0.3, -s * 0.3)], Stroke::new(1.2, Color32::WHITE));
        p.line_segment([c + vec2(s * 0.3, -s * 0.3), c + vec2(s, -s * 0.3)], Stroke::new(1.2, Color32::WHITE));
        p.galley(pos2(br.left() + 22.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
    }
    if let Some(why) = &photo.preview_only {
        // a raw shown from its embedded JPEG: a small amber "Preview" pill at the image's bottom
        // left, above the badge bar (always shown: it changes what editing does)
        let g = p.layout_no_wrap(crate::i18n::tr("Preview").into(), t.semibold(9.5), Color32::WHITE);
        let at = pos2(img_rect.left() + 6.0, img_rect.bottom() - 30.0 - 16.0);
        let br = Rect::from_min_size(at, vec2(g.size().x + 24.0, 16.0));
        p.rect_filled(br, 8.0, Color32::from_black_alpha(170));
        paint(p, Rect::from_center_size(pos2(br.left() + 9.0, br.center().y), vec2(12.0, 12.0)), Icon::Info, t.caution);
        p.galley(pos2(br.left() + 17.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
        register(ui.ctx(), format!("badge:previewOnly:{}", id.0), br);
        ui.interact(br, egui::Id::new(("preview-only-badge", id.0)), Sense::hover())
            .on_hover_text(crate::i18n::tr_format!("Preview only: {}", crate::widgets::preview_only_explanation(why)));
    }
    #[cfg(not(target_arch = "wasm32"))]
    super::connections::grid_badge(app, ui, id, img_rect);
    if photo.flag == Flag::Reject && !under {
        p.rect_filled(img_rect, 0.0, Color32::from_black_alpha(110));
    }
    // interaction
    // painting: a click or a drag sprays the painter's value instead of selecting
    if crate::libtools::paint_cell(app, ui, &resp, id) {
        return;
    }
    if resp.clicked() {
        let m = ui.input(|i| i.modifiers);
        // on the picture: this photo becomes the active one and a selection stays; on the frame: this photo alone
        let on_picture = resp.interact_pointer_pos().is_none_or(|at| img_rect.contains(at));
        let _ = app.run("library.select", json!({"ids": [id.0], "mode": click_mode(m, on_picture)}));
    }
    if resp.double_clicked() {
        open_photo(app, id);
    }
    // drag the selection (this photo joins it, or replaces it when it wasn't selected)
    if resp.drag_started() {
        if !app.session.selection.contains(id) {
            let _ = app.run("library.select", json!({"ids": [id.0]}));
        }
        app.ui.dragging_photos = Some(app.session.selection.ids.iter().map(|p| p.0).collect());
    } else if let Some(dragged) = app.ui.dragging_photos.clone() {
        reorder_drop(app, ui, id, r, &dragged);
    }
    crate::widgets::context_menu(&resp, |ui| context_menu(app, ui, id));
}

/// Height (points) of the strip under each thumbnail that holds its rating, flag and label in the
/// Compact layout.
pub const BADGE_STRIP: f32 = 16.0;

/// Inset (points) of a picked or rejected cell's glow from the cell's sides and bottom edge, so a
/// run of marked cells reads as separate cells rather than one border between rows.
pub const FLAG_GLOW_INSET: f32 = 3.0;

/// A Compact thumbnail cell's background (grid and filmstrip): one set of levels, as in
/// Lightroom: unselected (a pick a touch lighter, a reject darker) < hovered < selected < the
/// active photo, each step plainly visible in every theme, so a selection of picks still reads as
/// selected. `base` is the unselected cell's colour. The levels step towards the theme's text
/// colour, so a light theme darkens where a dark one brightens.
pub fn cell_fill(t: &Tokens, state: lcc_engine::SelectionState, flag: Flag, hovered: bool, base: Color32) -> Color32 {
    use lcc_engine::SelectionState;
    let selected = t.cell.lerp_to_gamma(t.text, 0.24);
    match state {
        SelectionState::Active => t.cell.lerp_to_gamma(t.text, 0.38),
        SelectionState::Selected => selected,
        SelectionState::NotSelected if hovered => base.lerp_to_gamma(selected, 0.4),
        SelectionState::NotSelected => flag_tint(base, flag),
    }
}

/// A cell's background with its flag in it: a pick slightly lighter, a reject darker, both well
/// short of the selection's level ([`cell_fill`]).
pub fn flag_tint(base: Color32, flag: Flag) -> Color32 {
    match flag {
        Flag::Pick => base.lerp_to_gamma(Color32::WHITE, 0.05),
        Flag::Reject => base.lerp_to_gamma(Color32::BLACK, 0.35),
        Flag::None => base,
    }
}

/// Where a flag's glow sits in `cell`: inset from its sides and bottom edge, rising about a third
/// of the cell (12–56 points). `None` for a cell too small to hold one.
pub fn flag_glow_rect(cell: Rect) -> Option<Rect> {
    let inner = cell.shrink2(vec2(FLAG_GLOW_INSET, 0.0));
    let bottom = cell.bottom() - FLAG_GLOW_INSET;
    let h = (cell.height() * 0.33).clamp(12.0, 56.0).min(bottom - cell.top());
    (inner.width() > 0.0 && h > 0.0).then(|| Rect::from_min_max(pos2(inner.left(), bottom - h), pos2(inner.right(), bottom)))
}

/// The flag's mark in a Compact `cell`: a glow rising from just inside the bottom edge and fading
/// out upwards, white for a pick and red for a reject, none for an unflagged photo. Drawn before
/// the thumbnail, so it sits behind it, soft enough that the white pick flag over it stays
/// readable.
pub fn flag_glow(p: &egui::Painter, t: &Tokens, cell: Rect, flag: Flag) {
    let (colour, alpha) = match flag {
        Flag::Pick => (Color32::WHITE, 0.42),
        Flag::Reject => (t.reject, 0.6),
        Flag::None => return,
    };
    let Some(g) = flag_glow_rect(cell) else { return };
    let mut mesh = egui::Mesh::default();
    // a fade in two steps (bright at the edge, quickly softer, then gone), so it reads as a glow
    // from the bottom rather than a band
    let mid_y = g.bottom() - g.height() * 0.35;
    let rows = [(g.top(), Color32::TRANSPARENT), (mid_y, colour.gamma_multiply(alpha * 0.35)), (g.bottom(), colour.gamma_multiply(alpha))];
    for (y, c) in rows {
        mesh.colored_vertex(pos2(g.left(), y), c);
        mesh.colored_vertex(pos2(g.right(), y), c);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    mesh.add_triangle(2, 3, 4);
    mesh.add_triangle(3, 5, 4);
    p.add(egui::Shape::mesh(mesh));
}

/// The flag a Compact cell's strip shows (OneSlips LR-FLAGS cff75fcf5): one flag shape, solid for a pick, solid with a
/// bold cross for a reject, so the two differ in shape as well as colour; none for an unflagged photo.
pub fn flag_mark_icon(flag: Flag) -> Option<Icon> {
    match flag {
        Flag::Pick => Some(Icon::FlagMarkPick),
        Flag::Reject => Some(Icon::FlagMarkReject),
        Flag::None => None,
    }
}

/// The colour of a reject's mark in the Compact strip: the theme's reject red, lightened so it
/// stands out from the red glow ([`flag_glow`]) the strip sits in.
pub fn reject_mark_colour(t: &Tokens) -> Color32 {
    t.reject.lerp_to_gamma(Color32::WHITE, 0.55)
}

/// `icon` in `colour` with a one-point dark outline round it, so it reads on a background of a
/// similar hue.
fn paint_outlined(p: &egui::Painter, rect: Rect, icon: Icon, colour: Color32) {
    let outline = Color32::from_black_alpha(210);
    for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (-0.7, -0.7), (0.7, -0.7), (-0.7, 0.7), (0.7, 0.7)] {
        paint(p, rect.translate(vec2(dx, dy)), icon, outline);
    }
    paint(p, rect, icon, colour);
}

/// A photo's culling marks along `bar` (the strip under a Compact thumbnail): its flag and stars
/// from the left, its colour label and edited mark from the right, sized to the bar's height so
/// they read at a glance. `on_photo`: drawn over the picture (white marks) rather than on the panel.
pub fn paint_badges(p: &egui::Painter, t: &Tokens, bar: Rect, photo: &lcc_catalog::Photo, on_photo: bool, reference: bool) {
    let h = bar.height().clamp(10.0, 40.0);
    let y = bar.center().y;
    let star = (h * 0.62).round().max(8.0);
    let flag = (h * 0.8).round().max(10.0);
    // the pick flag is white over a picture whatever the theme (the theme's colour is for panels)
    let pick = if on_photo { Color32::WHITE } else { t.pick };
    let mut x = bar.left() + 2.0;
    if let Some(icon) = flag_mark_icon(photo.flag) {
        let at = Rect::from_min_size(pos2(x, y - flag / 2.0), vec2(flag, flag));
        if photo.flag == Flag::Reject {
            // the reject's strip glows red (`flag_glow`), so its red mark sat red on red: a lighter red
            // with a dark outline reads on it (and over a picture)
            paint_outlined(p, at, icon, reject_mark_colour(t));
        } else {
            paint(p, at, icon, pick);
        }
    }
    if photo.flag != Flag::None {
        x += flag + 4.0;
    }
    let room = (bar.right() - x - h * 1.6).max(0.0);
    // every star where they fit; else one star and the number
    if photo.rating > 0 && f32::from(photo.rating) * (star + 1.0) <= room {
        for i in 0..photo.rating {
            paint(p, Rect::from_min_size(pos2(x + f32::from(i) * (star + 1.0), y - star / 2.0), vec2(star, star)), Icon::StarFilled, t.star);
        }
    } else if photo.rating > 0 {
        paint(p, Rect::from_min_size(pos2(x, y - star / 2.0), vec2(star, star)), Icon::StarFilled, t.star);
        p.text(pos2(x + star + 2.0, y), Align2::LEFT_CENTER, photo.rating.to_string(), t.semibold(h * 0.46), t.star);
    }
    let mut right = bar.right() - 2.0;
    if let Some(l) = photo.label {
        let rad = (h * 0.22).max(4.0);
        p.circle(pos2(right - rad, y), rad, label_color(l), Stroke::new(1.0, Color32::from_black_alpha(120)));
        right -= rad * 2.0 + 5.0;
    }
    if photo.is_edited() {
        let s = (h * 0.5).round();
        let colour = if on_photo { Color32::from_gray(220) } else { t.text_label };
        paint(p, Rect::from_min_size(pos2(right - s, y - s / 2.0), vec2(s, s)), Icon::Sliders, colour);
        right -= s + 4.0;
    }
    if photo.exported.is_some() {
        let s = (h * 0.5).round();
        let colour = if on_photo { Color32::from_gray(220) } else { t.text_label };
        paint(p, Rect::from_min_size(pos2(right - s, y - s / 2.0), vec2(s, s)), Icon::Share, colour);
        right -= s + 4.0;
    }
    // a Vignetting Calibration reference in use
    if reference {
        let s = (h * 0.5).round();
        let colour = if on_photo { Color32::from_gray(220) } else { t.text_label };
        paint(p, Rect::from_min_size(pos2(right - s, y - s / 2.0), vec2(s, s)), Icon::Radial, colour);
    }
}

/// A double-click on a grid photo: select it and show it in the loupe, or in Develop when the
/// user chose that in Preferences → Interface (issue #144).
pub fn open_photo(app: &mut LccApp, id: PhotoId) {
    let _ = app.run("library.select", json!({"ids": [id.0]}));
    let cmd = if app.ui.settings.double_click_develop { "module.develop" } else { "view.detail" };
    let _ = app.run(cmd, json!({}));
}

/// The regular album the library shows: the one whose photos can be put in Custom Order.
pub fn custom_order_album(app: &LccApp) -> Option<lcc_catalog::AlbumId> {
    match app.session.source {
        lcc_engine::LibrarySource::Album(a) if app.session.catalog.has_custom_order(a) => Some(a),
        _ => None,
    }
}

/// Photos dragged over a thumbnail of a regular album: a line on the side they would land
/// (before this photo, or after it), and on release they move there in Custom Order (switching the
/// sort to it, as Lightroom Classic does).
pub(crate) fn reorder_drop(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, r: Rect, dragged: &[u64]) {
    let Some(album) = custom_order_album(app).filter(|_| !dragged.contains(&id.0)) else { return };
    let (pos, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released()));
    let Some(pos) = pos.filter(|p| r.contains(*p) && ui.clip_rect().contains(*p)) else { return };
    let t = Tokens::get(ui.ctx());
    let left = pos.x < r.center().x;
    let x = if left { r.left() + 1.5 } else { r.right() - 1.5 };
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("reorder-drop"))).with_clip_rect(ui.clip_rect());
    painter.line_segment([egui::pos2(x, r.top()), egui::pos2(x, r.bottom())], Stroke::new(3.0, t.accent));
    if !released {
        return;
    }
    let before = if left {
        Some(id)
    } else {
        let vis = app.session.visible_cloned();
        vis.iter().position(|v| *v == id).and_then(|i| vis.get(i + 1).copied())
    };
    app.ui.dragging_photos = None;
    if let Err(e) = app.run("album.orderPhotos", json!({"album": album.0, "ids": dragged, "before": before.map(|b| b.0)})) {
        app.toast(ui.ctx(), e);
    }
}

/// The header of a Local folder view: the path as a breadcrumb (each part opens that folder),
/// Include subfolders, Add to My Photos.
fn folder_header(app: &mut LccApp, ui: &mut egui::Ui, b: &lcc_engine::Browse, ids: &[PhotoId], local_n: usize, cnt: &str) -> Rect {
    let t = Tokens::get(ui.ctx());
    // The path/count and actions need independent rows. The frame reserves the actual height,
    // including any wrapped actions, so the grid below never paints under the header.
    let header = egui::Frame::NONE.fill(t.canvas).inner_margin(egui::Margin::symmetric(16, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let count_w =
                ui.painter().layout_no_wrap(cnt.to_owned(), t.font(12.5), t.text_dim).size().x.ceil().min((ui.available_width() * 0.45).floor());
            let path_w = (ui.available_width() - count_w - 8.0).max(0.0).floor();
            ui.allocate_ui_with_layout(vec2(path_w, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                folder_breadcrumbs(app, ui, &b.path);
            });
            let count = ui.add_sized(vec2(count_w, 24.0), egui::Label::new(egui::RichText::new(cnt).size(12.5).color(t.text_dim)).truncate());
            register(ui.ctx(), "grid:count", count.rect);
            count.on_hover_text(cnt);
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            let mut sub = b.subfolders;
            let c = ui.checkbox(&mut sub, crate::i18n::tr("Include subfolders"));
            register(ui.ctx(), "check:includeSubfolders", c.rect);
            if c.changed() {
                let _ = app.run("library.browse", json!({"path": b.path, "subfolders": sub}));
            }
            // Counted once per view (cached); the ids are only gathered on a click.
            if local_n > 0 {
                let label = crate::i18n::tr_format!("Add {local_n} to My Photos", local_n = local_n);
                let button =
                    ui.add(egui::Button::new(egui::RichText::new(label).font(t.semibold(11.5)).color(t.text)).wrap().min_size(vec2(37.0, 24.0)));
                register(ui.ctx(), "button:addToLibrary", button.rect);
                if button.clicked() {
                    let local: Vec<u64> = ids.iter().filter(|id| app.session.catalog.photo(**id).is_some_and(|p| p.local)).map(|id| id.0).collect();
                    let n = local.len();
                    match app.run("photo.addToLibrary", json!({"ids": local})) {
                        Ok(_) => app.toast(ui.ctx(), crate::i18n::tr_format!("Added {n} photo{} to My Photos", if n == 1 { "" } else { "s" }, n = n)),
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
            }
        });
    });
    register(ui.ctx(), "grid:folderHeader", header.response.rect);
    header.response.rect
}

/// The folders a breadcrumb shows for `path`: each name with the path that opens it, spelled as
/// in `path` (its separators, a leading `/` or a UNC `\\server\share`; issue #538). A bare drive
/// (`C:`) gets its separator back: on its own it means the current folder on that drive.
pub(crate) fn crumbs(path: &str) -> Vec<(&str, String)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in path.char_indices().chain(std::iter::once((path.len(), '/'))) {
        if c != '/' && c != '\\' {
            continue;
        }
        if let Some(part) = path.get(start..i).filter(|p| !p.is_empty()) {
            let mut to = path.get(..i).unwrap_or(path).to_string();
            if out.is_empty() && part.ends_with(':') && to == part {
                to.push(if path.contains('\\') { '\\' } else { '/' });
            }
            out.push((part, to));
        }
        start = i + c.len_utf8();
    }
    out
}

fn folder_breadcrumbs(app: &mut LccApp, ui: &mut egui::Ui, path: &str) {
    let t = Tokens::get(ui.ctx());
    ui.spacing_mut().item_spacing.x = 4.0;
    let crumbs = crumbs(path);
    let parts: Vec<&str> = crumbs.iter().map(|(name, _)| *name).collect();
    // With all sidebars open even four ellipses and their separators can exceed the row.
    // Prefer the current folder, showing more ancestors as space permits.
    let visible = ((ui.available_width() / 80.0) as usize).clamp(1, 4);
    let skip = parts.len().saturating_sub(visible);
    let separator = |ui: &mut egui::Ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 24.0), Sense::hover());
        paint(ui.painter(), Rect::from_center_size(r.center(), vec2(16.0, 16.0)), Icon::ChevronRight, t.text_dim);
    };
    if skip > 0 {
        // the folders "…" stands for: a click lists them, each one opens
        let dots = ui.add(egui::Label::new(egui::RichText::new("…").size(13.0).color(t.text_dim)).sense(Sense::click()));
        register(ui.ctx(), "crumb:more", dots.rect);
        if dots.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let dots = dots.on_hover_text(path);
        egui::Popup::menu(&dots).show(|ui| {
            for (i, (name, to)) in crumbs.iter().enumerate().take(skip) {
                let item = ui.button(*name);
                register(ui.ctx(), format!("crumb:{i}"), item.rect);
                if item.on_hover_text(to).clicked() {
                    let _ = app.run("library.browse", json!({"path": to}));
                    ui.close();
                }
            }
        });
        separator(ui);
    }
    let separator_w = 16.0 + 2.0 * ui.spacing().item_spacing.x;
    for (i, part) in parts.iter().enumerate().skip(skip) {
        let remaining = parts.len() - i;
        let last = remaining == 1;
        // Share the bounded row among the remaining crumbs, leaving room for every separator.
        let width = ((ui.available_width() - separator_w * (remaining - 1) as f32) / remaining as f32).max(0.0).floor();
        let text = egui::RichText::new(*part).size(13.0).color(if last { t.text } else { t.text_label });
        let r = ui.add_sized(vec2(width, 24.0), egui::Label::new(if last { text.strong() } else { text }).truncate().sense(Sense::click()));
        register(ui.ctx(), format!("crumb:{i}"), r.rect);
        if !last {
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if r.clicked()
                && let Some((_, to)) = crumbs.get(i)
            {
                let _ = app.run("library.browse", json!({"path": to}));
            }
            separator(ui);
        }
        r.on_hover_text(path);
    }
}

/// While photos are dragged: a badge at the pointer; the drag ends when the button is up
/// (drop targets act on the release frame, before this runs).
/// What the label following dragged photos says this frame instead of how many there are: where a
/// drop would put them (set by the drop target under the pointer, read by [`drag_feedback`]).
pub fn drag_hint(ctx: &egui::Context, text: String) {
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("drag-photos-hint"), (frame, text)));
}

pub fn drag_feedback(app: &mut LccApp, ctx: &egui::Context) {
    let Some(ids) = &app.ui.dragging_photos else { return };
    let (released, down, pos) = ctx.input(|i| (i.pointer.any_released(), i.pointer.any_down(), i.pointer.latest_pos()));
    if released || !down {
        app.ui.dragging_photos = None;
        return;
    }
    let Some(pos) = pos else { return };
    let t = Tokens::get(ctx);
    let n = ids.len();
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    // a drop target under the pointer says where they would go (only this frame's)
    let frame = ctx.cumulative_frame_nr();
    let hint = ctx.data(|d| d.get_temp::<(u64, String)>(egui::Id::new("drag-photos-hint"))).filter(|(f, _)| *f == frame).map(|(_, text)| text);
    let text = hint.unwrap_or_else(|| crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n));
    egui::Area::new(egui::Id::new("drag-photos")).order(egui::Order::Tooltip).interactable(false).fixed_pos(pos + vec2(14.0, 10.0)).show(ctx, |ui| {
        egui::Frame::NONE.fill(t.accent).corner_radius(10.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
            let label = ui.add(egui::Label::new(egui::RichText::new(text).color(Color32::WHITE).font(t.semibold(12.0))).extend());
            crate::widgets::register(ui.ctx(), "dragPhotos", label.rect);
        });
    });
}

pub use super::filterbar::label_color;

/// Albums hold library photos (issue #684): true when `ids` are photos and all of them are
/// browsed (Local) ones, which album views never show.
pub fn local_only(app: &LccApp, ids: &[PhotoId]) -> bool {
    !ids.is_empty() && ids.iter().all(|id| app.session.catalog.photo(*id).is_some_and(|p| p.local))
}

/// Why an album action is off for Local photos.
pub fn albums_hint() -> String {
    crate::i18n::tr("Albums hold catalog photos: Add to My Photos first").to_string()
}

/// Tell the user when an album command skipped Local photos of the selection (`skipped` in its
/// result).
pub fn toast_skipped(app: &mut LccApp, ctx: &egui::Context, r: &serde_json::Value) {
    if let Some(n) = r.get("skipped").and_then(serde_json::Value::as_u64).filter(|n| *n > 0) {
        app.toast(
            ctx,
            crate::i18n::tr_format!(
                "{n} Local photo{} skipped: collections hold catalog photos (Add to My Photos first)",
                if n == 1 { "" } else { "s" },
                n = n
            ),
        );
    }
}

/// "Set Color Label" items for the active photo, shared by context menus.
pub fn label_menu(app: &mut LccApp, ui: &mut egui::Ui) {
    let current = app.session.active().and_then(|id| app.session.catalog.photo(id)).and_then(|p| p.label);
    if let Some(l) = label_items(app, ui, current, "photoLabel") {
        let _ = crate::menubar::run_item(app, "photo.label", json!({"label": label_param(l)}));
        ui.close();
    }
}

/// The colour label items of a menu (coloured dot + the label's name, then None and Edit Label
/// Names…), `current` checked; each colour is widget `<widget>:<colour>` (`<widget>:none`) for
/// the control channel. Returns the choice (`Some(None)`: None).
pub fn label_items(app: &mut LccApp, ui: &mut egui::Ui, current: Option<ColorLabel>, widget: &str) -> Option<Option<ColorLabel>> {
    let mut chosen = None;
    for l in ColorLabel::ALL {
        let name = crate::i18n::color_label(&app.session.catalog, l);
        let resp = ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, label_color(l));
            ui.selectable_label(current == Some(l), name)
        });
        register(ui.ctx(), format!("{widget}:{}", label_param(Some(l))), resp.inner.rect);
        if resp.inner.clicked() {
            chosen = Some(Some(l));
        }
    }
    let none = ui.selectable_label(current.is_none(), crate::i18n::tr("None"));
    register(ui.ctx(), format!("{widget}:none"), none.rect);
    if none.clicked() {
        chosen = Some(None);
    }
    ui.separator();
    if ui.button(crate::i18n::tr("Edit Label Names…")).clicked() {
        let _ = app.run("dialog.labelNames", json!({}));
    }
    chosen
}

/// A label as the `label` parameter of `photo.label` / `folder.label` names it.
pub fn label_param(l: Option<ColorLabel>) -> String {
    l.map_or_else(|| "none".to_string(), |l| format!("{l:?}").to_lowercase())
}

/// Stack badge at the cell's top-left: the photo count on a collapsed stack's top, `i/n` on the
/// members of an expanded stack. Clicking or double-clicking it expands/collapses the stack.
pub fn stack_badge(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId, sid: lcc_catalog::StackId, pos: usize, r: Rect, square: bool) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.catalog.stack(sid) else { return };
    let n = st.photos.len();
    let collapsed = st.collapsed;
    let text = if collapsed { n.to_string() } else { format!("{}/{n}", pos + 1) };
    let p = ui.painter();
    let g = p.layout_no_wrap(text, t.semibold(11.0), Color32::WHITE);
    let origin = if square { r.min + vec2(10.0, 26.0) } else { r.min + vec2(5.0, 5.0) };
    let br = Rect::from_min_size(origin, vec2(g.size().x + 30.0, 20.0));
    let resp = ui.interact(br, egui::Id::new(("stack-badge", id.0)), Sense::click()).on_hover_text(if collapsed {
        "Stack: click to expand (S)"
    } else {
        "Stack: click to collapse (S)"
    });
    crate::widgets::announce(&resp, crate::i18n::tr(if collapsed { "Expand Stack" } else { "Collapse Stack" }));
    register(ui.ctx(), format!("stack:{}", id.0), br);
    let fill = if resp.hovered() { Color32::from_black_alpha(220) } else { Color32::from_black_alpha(165) };
    p.rect_filled(br, 10.0, fill);
    if pos == 0 && !collapsed {
        p.rect_stroke(br, 10.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
    }
    paint(p, Rect::from_min_size(br.min + vec2(6.0, 3.0), vec2(14.0, 14.0)), Icon::Stack, Color32::WHITE);
    p.galley(pos2(br.min.x + 24.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
    // a double-click (Lightroom's way) toggles once: its second click would undo the first
    if resp.clicked() && !resp.double_clicked() {
        toggle_stack(app, ui.ctx(), id);
    }
}

/// A click on a stack badge (grid or filmstrip): expand / collapse that photo's stack, selected or
/// not. The selection stays as it is (Compare keeps its pair).
pub fn toggle_stack(app: &mut LccApp, ctx: &egui::Context, id: PhotoId) {
    if let Err(e) = app.run("stack.toggle", json!({"ids": [id.0]})) {
        app.toast(ctx, e);
    }
}

pub fn context_menu(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    if !app.session.selection.contains(id) {
        let _ = app.run("library.select", json!({"ids": [id.0]}));
    }
    if ui.button(crate::i18n::tr("Open in Detail")).clicked() {
        let _ = app.run("view.detail", json!({}));
    }
    // a photo that doesn't work: the frames around it in its folder, imported or not
    let alt = ui.button(crate::i18n::tr("Show Alternatives"));
    register(ui.ctx(), "menu:showAlternatives", alt.rect);
    if alt.clicked()
        && let Err(e) = app.run("photo.showAlternatives", json!({"id": id.0}))
    {
        app.toast(ui.ctx(), e);
    }
    // Photo ▸ Edit In: a copy with this photo's edits, opened in PhotoCraft or another editor
    // (desktop only: the web can't start programs)
    ui.add_enabled_ui(crate::menus::ui_enabled(app, "photo.editInPhotoCraft"), |ui| {
        crate::widgets::menu_button(ui, crate::i18n::tr("Edit In"), |ui| {
            if ui.button(crate::i18n::tr("Edit in PhotoCraft")).clicked() {
                let _ = app.run("photo.editInPhotoCraft", json!({}));
            }
            if ui.button(crate::i18n::tr("Edit in Other Editor")).clicked() {
                let _ = app.run("photo.editInExternal", json!({}));
            }
            // the raw itself, unrendered: for a raw developer set as the other editor
            if ui
                .add_enabled(
                    crate::menus::ui_enabled(app, "photo.openOriginalInExternal"),
                    egui::Button::new(crate::i18n::tr("Open Original in Other Editor (RAW, no copy)")),
                )
                .clicked()
            {
                let _ = app.run("photo.openOriginalInExternal", json!({}));
            }
        });
    });
    ui.separator();
    crate::widgets::menu_button(ui, crate::i18n::tr("Set Flag"), |ui| {
        for (l, f) in [("Pick", "pick"), ("Reject", "reject"), ("Unflagged", "none")] {
            if ui.button(crate::i18n::tr(l)).clicked() {
                let _ = crate::menubar::run_item(app, "photo.flag", json!({"flag": f}));
            }
        }
    });
    crate::widgets::menu_button(ui, crate::i18n::tr("Set Rating"), |ui| {
        for r in 0..=5 {
            if ui.button(if r == 0 { crate::i18n::tr("No Stars").to_string() } else { "★".repeat(r) }).clicked() {
                let _ = crate::menubar::run_item(app, "photo.rate", json!({"rating": r}));
            }
        }
    });
    crate::widgets::menu_button(ui, crate::i18n::tr("Set Color Label"), |ui| label_menu(app, ui));
    // the keyword shortcut (Set Keyword Shortcut…), given to the selected photos
    let keywords = app.session.keyword_shortcut.clone();
    if !keywords.is_empty() {
        let names = keywords.iter().map(|k| k.replace('|', " › ")).collect::<Vec<_>>().join(", ");
        let add = ui.button(crate::i18n::tr_format!("Add Keyword “{keywords}”", keywords = names));
        register(ui.ctx(), "button:contextAddKeywordShortcut", add.rect);
        if add.clicked() {
            let _ = app.run("photo.setMeta", json!({"addKeywords": keywords}));
            ui.close();
        }
    }
    ui.separator();
    quick_collection_item(app, ui);
    if local_only(app, &app.session.selected()) {
        // albums hold library photos (issue #684): nothing here to add until they join My Photos
        let add = ui.add_enabled(false, egui::Button::new(crate::i18n::tr("Add to Collection"))).on_disabled_hover_text(albums_hint());
        register(ui.ctx(), "button:contextAddToAlbum", add.rect);
    } else {
        let add = crate::widgets::menu_button(ui, crate::i18n::tr("Add to Collection"), |ui| {
            let albums: Vec<_> = app.session.catalog.albums().filter(|a| !a.folder && !a.is_smart()).map(|a| (a.id.0, a.name.clone())).collect();
            let any = !albums.is_empty();
            for (aid, name) in albums {
                let b = ui.button(name);
                register(ui.ctx(), format!("button:contextAlbum:{aid}"), b.rect);
                if b.clicked() {
                    match app.run("album.addPhotos", json!({"id": aid})) {
                        Ok(r) => toast_skipped(app, ui.ctx(), &r),
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
            }
            if any {
                ui.separator();
            }
            // as in the Photo menu: a new album holding the selection, also when there is none yet
            // to pick (issue #676: the submenu came up empty)
            let new = ui.button(crate::i18n::tr("New Collection…"));
            register(ui.ctx(), "button:contextNewAlbum", new.rect);
            if new.clicked() {
                let _ = app.run("dialog.newAlbum", json!({}));
            }
            // in a (non-smart) album: take the selection out of it, or make this photo its cover
            if let lcc_engine::LibrarySource::Album(aid) = app.session.source
                && app.session.catalog.album(aid).is_some_and(|a| !a.folder && !a.is_smart())
            {
                if ui.button(crate::i18n::tr("Remove from Collection")).clicked() {
                    let _ = app.run("album.removePhotos", json!({"id": aid.0}));
                }
                if ui.button(crate::i18n::tr("Set as Collection Cover")).clicked() {
                    let _ = app.run("album.setCover", json!({"id": aid.0, "photo": id.0}));
                }
            }
        });
        register(ui.ctx(), "button:contextAddToAlbum", add.response.rect);
    }
    crate::widgets::menu_button(ui, crate::i18n::tr("Virtual Copy & Version"), |ui| {
        if ui.button(crate::i18n::tr("Create Virtual Copy")).clicked() {
            let _ = app.run("photo.virtualCopy", json!({}));
        }
        if ui.button(crate::i18n::tr("Create Version")).clicked() {
            let _ = app.run("version.create", json!({}));
        }
    });
    crate::widgets::menu_button(ui, crate::i18n::tr("Stack"), |ui| {
        let stacked = app.session.catalog.stack_of(id).is_some();
        let several = app.session.selection.ids.len() > 1;
        for (label, cmd, on) in [
            ("Group into Stack", "stack.group", several),
            ("Ungroup Stack", "stack.ungroup", stacked),
            ("Remove from Stack", "stack.remove", stacked),
            ("Set as Top of Stack", "stack.setTop", stacked),
            ("Move Up in Stack", "stack.moveUp", stacked),
            ("Move Down in Stack", "stack.moveDown", stacked),
            ("Split Stack", "stack.split", stacked),
            ("Expand/Collapse Stack", "stack.toggle", stacked),
        ] {
            if ui.add_enabled(on, egui::Button::new(crate::i18n::tr(label))).clicked() {
                let _ = app.run(cmd, json!({}));
            }
        }
        ui.separator();
        if ui.button(crate::i18n::tr("Auto-Stack by Capture Time…")).clicked() {
            let _ = app.run("dialog.autoStack", json!({}));
        }
    });
    ui.separator();
    if ui.button(crate::i18n::tr("Copy Edit Settings")).clicked() {
        let _ = app.run("develop.copy", json!({}));
    }
    if ui.add_enabled(app.session.clipboard.is_some(), egui::Button::new(crate::i18n::tr("Paste Edit Settings"))).clicked() {
        let _ = app.run("develop.paste", json!({}));
    }
    crate::widgets::menu_button(ui, crate::i18n::tr("Develop Settings"), |ui| {
        if ui.add_enabled(app.session.clipboard.is_some(), egui::Button::new(crate::i18n::tr("Paste Selected Settings…"))).clicked() {
            let _ = app.run("dialog.pasteSettings", json!({}));
        }
        if ui.button(crate::i18n::tr("Reset Edits")).clicked() {
            let _ = app.run("develop.reset", json!({}));
        }
        let live = super::compare::live(app, id);
        let set_reference = ui
            .add_enabled(live, egui::Button::new(crate::i18n::tr("Set as Reference Photo")))
            .on_disabled_hover_text(crate::i18n::tr("Restore the photo to use it as the reference"));
        if set_reference.clicked()
            && let Err(e) = app.run("photo.setReference", json!({"id": id.0}))
        {
            app.toast(ui.ctx(), e);
        }
    });
    crate::widgets::menu_button(ui, crate::i18n::tr("Photo Merge"), |ui| {
        let n = app.session.selected().len();
        for (id, label) in [
            ("dialog.mergeHdr", "HDR…"),
            ("dialog.mergePanorama", "Panorama…"),
            ("dialog.mergeHdrPanorama", "HDR Panorama…"),
            ("dialog.mergeFocusStack", "Focus Stack…"),
        ] {
            if ui.add_enabled(n >= 2, egui::Button::new(crate::i18n::tr(label))).clicked() {
                let _ = app.run(id, json!({}));
            }
        }
    });
    ui.separator();
    crate::widgets::menu_button(ui, crate::i18n::tr("Rotate"), |ui| {
        if ui.button(crate::i18n::tr("Rotate Left")).clicked() {
            let _ = app.run("photo.rotateLeft", json!({}));
        }
        if ui.button(crate::i18n::tr("Rotate Right")).clicked() {
            let _ = app.run("photo.rotateRight", json!({}));
        }
    });
    crate::widgets::menu_button(ui, crate::i18n::tr("File"), |ui| {
        if ui.button(crate::i18n::tr("Rename…")).clicked() {
            let _ = app.run("dialog.rename", json!({}));
        }
        let can_pick = app.services.pick_folder.is_some() || app.services.picker.is_some();
        if can_pick && ui.button(crate::i18n::tr("Move to Folder…")).clicked() {
            // the folder dialog goes through the shared picker (off the UI thread where the host has
            // one: then photo.moveToFolder runs when it closes)
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("Move to Folder…"));
            let picked =
                crate::pick::ask(app, "photo.moveToFolder", &json!({}), "folder", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x]));
            if let crate::pick::Picked::Now(v) = picked
                && let Some(into) = v.into_iter().next()
            {
                match app.run("photo.moveToFolder", json!({"folder": into})) {
                    Ok(r) => app.toast(ui.ctx(), crate::i18n::tr_format!("Moved {} photo(s)", r["moved"])),
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
        }
        if ui.button(crate::i18n::tr("Reload from Disk")).clicked() {
            // the files are read in the background (activity row `reload`)
            if let Err(e) = app.run("photo.reload", json!({})) {
                app.toast_error(ui.ctx(), e);
            }
        }
        // the original moved or its drive is gone: point the photo at the file again
        // (a cached answer, checked off the UI thread)
        let missing = matches!(&app.session.catalog.photo(id).map(|p| p.source.clone()), Some(lcc_catalog::Source::File { path }) if app.session.media.availability.is_offline(path));
        if missing && ui.button(crate::i18n::tr("Locate Missing File…")).clicked() {
            let _ = app.run("photo.locate", json!({}));
        }
        if ui
            .add_enabled(crate::menus::ui_enabled(app, "app.showInFinder"), egui::Button::new(crate::i18n::tr(crate::menus::reveal_label())))
            .clicked()
        {
            let _ = app.run("app.showInFinder", json!({}));
        }
        // the photo's folder in the sidebar's Folders section, as Lightroom's Go to Folder in Library
        // (the photo right-clicked, which a multi-selection may leave other than the active one)
        if ui.add_enabled(crate::menus::show_in_folders_enabled(app, Some(id)), egui::Button::new(crate::i18n::tr("Show in Folders"))).clicked() {
            let _ = app.run("app.showInFolders", json!({"id": id.0}));
        }
    });
    if ui.button(crate::i18n::tr("Find Similar Photos")).clicked() {
        match app.run("library.findSimilar", json!({"id": id.0})) {
            Ok(r) => {
                let n = r["photos"].as_array().map_or(0, Vec::len);
                app.ui.view = crate::state::ViewMode::PhotoGrid;
                app.toast(
                    ui.ctx(),
                    crate::i18n::tr_format!("{n} similar photo{} · View ▸ Clear Filters to see all", if n == 1 { "" } else { "s" }, n = n),
                );
            }
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
    ui.separator();
    if ui.button(crate::i18n::tr("Export…")).clicked() {
        let _ = app.run("file.export", json!({}));
    }
    crate::widgets::menu_button(ui, crate::i18n::tr("Export with Preset"), |ui| {
        for (p, _) in app.session.all_export_presets() {
            if ui.button(&p.name).clicked() {
                match app.run("app.export", json!({"preset": p.name, "background": true})) {
                    Ok(r) if r.get("background").is_some() => {}
                    Ok(r) => {
                        let n = r["files"].as_array().map_or(0, Vec::len);
                        app.toast(ui.ctx(), crate::i18n::tr_format!("Exported {n} photo{}", if n == 1 { "" } else { "s" }, n = n));
                    }
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
        }
    });
    ui.separator();
    if crate::menubar::selection_deleted(app) {
        if ui.button(crate::i18n::tr("Restore")).clicked() {
            let _ = app.run("photo.restore", json!({}));
        }
        if ui.button(crate::i18n::tr("Delete Permanently")).clicked() {
            let _ = crate::menubar::run_item(app, "photo.deletePermanently", json!({}));
        }
    } else {
        let delete = ui.button(crate::i18n::tr("Delete Photo"));
        register(ui.ctx(), "button:contextDeletePhoto", delete.rect);
        if delete.clicked() {
            let _ = crate::menubar::run_item(app, "photo.delete", json!({}));
        }
    }
}

/// Add to / Remove from Quick Collection (B), as Lightroom Classic's photo menu has it: the target
/// album when one is set, labelled by whether the whole selection is already in it.
fn quick_collection_item(app: &mut LccApp, ui: &mut egui::Ui) {
    let s = &app.session;
    let target = s.target_album.filter(|a| s.catalog.album(*a).is_some_and(|al| !al.is_smart() && !al.folder));
    let quick = target.is_none();
    let album = target.or_else(|| s.catalog.quick_collection()).and_then(|a| s.catalog.album(a));
    let sel = s.selected();
    let all_in = !sel.is_empty() && album.is_some_and(|al| sel.iter().all(|t| al.photos.contains(t)));
    let text = match (all_in, quick) {
        (false, true) => "Add to Quick Collection",
        (true, true) => "Remove from Quick Collection",
        (false, false) => "Add to Target Collection",
        (true, false) => "Remove from Target Collection",
    };
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    let mut b = egui::Button::new(crate::i18n::tr(text));
    if let Some(sc) = crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "album.toggleTarget") {
        b = b.shortcut_text(crate::menubar::shortcut_text(sc, mac));
    }
    let r = ui.add_enabled(crate::menus::ui_enabled(app, "album.toggleTarget"), b);
    register(ui.ctx(), "button:contextQuickCollection", r.rect);
    if r.clicked() {
        match app.run("album.toggleTarget", json!({})) {
            Ok(r) => toast_skipped(app, ui.ctx(), &r),
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_exposure_line_names_the_parts_a_file_has() {
        let mut m = lcc_catalog::Meta { shutter: "1/250".into(), aperture: Some(2.8), iso: Some(400), focal_mm: Some(85.0), ..Default::default() };
        assert_eq!(super::exposure_line(&m).as_deref(), Some("1/250 · f/2.8 · ISO 400 · 85 mm"));
        m.shutter = "8".into();
        m.aperture = Some(4.0);
        m.iso = None;
        assert_eq!(super::exposure_line(&m).as_deref(), Some("8 s · f/4 · 85 mm"));
        assert_eq!(super::exposure_line(&lcc_catalog::Meta::default()), None);
    }

    use super::*;

    /// A grid's thumbnail follows a slider drag (one render at a time); under the loupe it waits
    /// for the drag to end.
    #[test]
    fn counts_group_their_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1234), "1,234");
        assert_eq!(group_digits(2_233_000), "2,233,000");
    }

    /// Every photo a drag edits follows it the same way, not only the active one: the others of a
    /// multi-photo edit and the photos Auto Sync carries along wait under the loupe and follow live
    /// in a grid, one render at a time.
    #[test]
    fn every_photo_a_drag_edits_follows_it() {
        let d = std::sync::Arc::new(lcc_develop::DevelopSettings::default());
        let i = lcc_engine::Interaction {
            label: "Exposure".into(),
            photo: PhotoId(1),
            original: d.clone(),
            crop_edited: false,
            others: vec![(PhotoId(2), d.clone())],
            along: vec![(PhotoId(3), d)],
        };
        for id in [1, 2, 3] {
            assert!(drag_edits(Some(&i), PhotoId(id)), "photo {id} is edited by the drag");
        }
        assert!(!drag_edits(Some(&i), PhotoId(4)), "a photo the drag doesn't edit");
        assert!(!drag_edits(None, PhotoId(1)), "no drag");
    }

    #[test]
    fn grid_thumbnails_follow_a_drag() {
        assert!(!drag_waits(false, true, false), "a grid asks for the new picture");
        assert!(drag_waits(false, true, true), "one render at a time");
        assert!(drag_waits(true, true, false), "under the loupe it waits");
        assert!(!drag_waits(true, false, false), "a photo without a thumbnail gets one");
    }

    #[test]
    fn a_photo_keeps_its_place_through_a_run_of_small_resizes() {
        let ids: Vec<PhotoId> = (1..=600).map(PhotoId).collect();
        let aspects: Vec<f32> = (0..600).map(|i| [1.5, 0.67, 1.0, 1.78][i % 4]).collect();
        let view_h = 800.0;
        let mut w = 900.0;
        let lay = layout(&aspects, &[(0, 600)], w, 160.0, false);
        let active = ids.get(250).copied();
        let top0 = lay.cells.get(250).unwrap().top() - 300.0;
        let mut anchor = GridAnchor::follow(None, (0, 600, 0), &ids, &lay, Rect::from_min_size(pos2(0.0, top0), vec2(w, view_h)), active);
        let start_dy = anchor.as_ref().unwrap().dy;
        for step in 1..=20 {
            w *= 1.05;
            let lay = layout(&aspects, &[(0, 600)], w, 160.0, false);
            let id = (step, 600, 0);
            let top = anchor.as_ref().unwrap().reflow_offset(id, &ids, &lay).unwrap();
            let view = Rect::from_min_size(pos2(0.0, top), vec2(w, view_h));
            anchor = GridAnchor::follow(anchor, id, &ids, &lay, view, active);
            let a = anchor.as_ref().unwrap();
            assert_eq!(a.photo, PhotoId(251), "step {step}: the selected photo stays the anchor");
            let on_screen = lay.cells.get(250).unwrap().top() - top;
            assert!((on_screen - start_dy).abs() < 0.5, "step {step}: moved from {start_dy} to {on_screen}");
        }
    }

    #[test]
    fn a_filter_keeps_the_anchor_photo_in_place_and_survives_it_leaving() {
        let ids: Vec<PhotoId> = (1..=400).map(PhotoId).collect();
        let lay = layout(&[1.5; 400], &[(0, 400)], 1000.0, 160.0, false);
        let view = Rect::from_min_size(pos2(0.0, lay.cells.get(200).unwrap().top() - 100.0), vec2(1000.0, 600.0));
        let a = GridAnchor::follow(None, (0, 400, 0), &ids, &lay, view, None).unwrap();
        // every other photo filtered out
        let kept: Vec<PhotoId> = ids.iter().copied().filter(|p| p.0 % 2 == a.photo.0 % 2).collect();
        let lay2 = layout(&vec![1.5; kept.len()], &[(0, kept.len())], 1000.0, 160.0, false);
        let y = a.reflow_offset((1, kept.len(), 0), &kept, &lay2).unwrap();
        let i = kept.iter().position(|p| *p == a.photo).unwrap();
        assert!((lay2.cells.get(i).unwrap().top() - y - a.dy).abs() < 0.5);
        // anchor filtered out: no jump, and a new one is picked
        let none: Vec<PhotoId> = ids.iter().copied().filter(|p| *p != a.photo).collect();
        assert!(a.reflow_offset((2, none.len(), 0), &none, &lay).is_none());
        assert!(GridAnchor::follow(Some(a), (2, none.len(), 0), &none, &lay, view, None).is_some());
        assert!(GridAnchor::follow(None, (3, 0, 0), &[], &layout(&[], &[], 1000.0, 160.0, false), view, None).is_none());
    }

    #[test]
    fn the_same_photo_in_another_source_scrolls_into_view() {
        let id = PhotoId(3);
        let all = lcc_engine::LibrarySource::All;
        let album = lcc_engine::LibrarySource::Album(lcc_catalog::AlbumId(9));
        assert!(!follow_changed(Some(id), all, 4, Some(id), all, 5));
        assert!(follow_changed(Some(id), all, 4, Some(id), album, 5));
    }

    #[test]
    fn ungrouped_justified_rows_fill_the_width() {
        let aspects = [1.5f32, 1.5, 0.66, 1.0, 1.5, 1.5, 1.5];
        let l = layout(&aspects, &[], 600.0, 150.0, false);
        assert!(l.headers.is_empty());
        assert_eq!(l.cells.len(), aspects.len());
        // full rows end at the right edge; rows are at most the target height
        let first_row: Vec<&Rect> = l.cells.iter().filter(|c| (c.top() - l.cells[0].top()).abs() < 0.01).collect();
        assert!(first_row.len() >= 2);
        assert!((first_row.last().unwrap().right() - 604.0).abs() < 0.5, "{first_row:?}");
        assert!(first_row.iter().all(|c| c.height() <= 150.0 + 0.01));
        let last = l.cells.last().unwrap();
        assert!(last.right() <= 604.0 + 0.5);
        assert!((l.height - (last.bottom() + 6.0)).abs() < 0.01);
    }

    #[test]
    fn groups_start_new_rows_below_their_headers() {
        let aspects = [1.5f32; 7];
        let l = layout(&aspects, &[(0, 2), (2, 5)], 1000.0, 200.0, false);
        assert_eq!(l.headers.len(), 2);
        assert_eq!(l.headers[0].top(), 4.0);
        assert_eq!(l.headers[0].height(), HEADER_H);
        // group 1: two photos in a short row below its header (not stretched)
        assert_eq!(l.cells[0].top(), l.headers[0].bottom());
        assert!((l.cells[0].height() - 200.0).abs() < 0.01);
        assert_eq!(l.cells[1].top(), l.cells[0].top());
        // group 2 starts on a new row below its own header, after group 1's photos
        assert!(l.headers[1].top() >= l.cells[1].bottom());
        assert_eq!(l.cells[2].top(), l.headers[1].bottom());
        assert!((l.cells[2].left() - 4.0).abs() < 0.01, "a group's first photo starts the row");
        for c in &l.cells[2..] {
            assert!(c.top() >= l.headers[1].bottom());
        }
        // every photo has a cell; nothing overlaps
        for (i, a) in l.cells.iter().enumerate() {
            assert!(a.width() > 0.0);
            for b in &l.cells[i + 1..] {
                assert!(!a.shrink(0.5).intersects(b.shrink(0.5)), "{a:?} {b:?}");
            }
            for h in &l.headers {
                assert!(!a.shrink(0.5).intersects(*h));
            }
        }
    }

    /// The row index covers every cell once, in order and top to bottom, so the visible cells
    /// are found by binary search.
    #[test]
    fn rows_index_every_cell_top_to_bottom() {
        let aspects: Vec<f32> = (0..500).map(|i| [1.5f32, 0.66, 1.0, 1.78][i % 4]).collect();
        let groups = [(0, 7), (7, 200), (207, 1), (208, 292)];
        for square in [false, true] {
            let l = layout(&aspects, &groups, 900.0, 140.0, square);
            let mut next = 0;
            for (k, row) in l.rows.iter().enumerate() {
                assert_eq!(row.start, next, "rows are contiguous");
                assert!(row.end > row.start);
                for c in &l.cells[row.start..row.end] {
                    assert!((c.top() - row.top).abs() < 0.01 && (c.bottom() - row.bottom).abs() < 0.01, "{c:?} {row:?}");
                }
                if let Some(prev) = k.checked_sub(1).map(|p| l.rows[p]) {
                    assert!(row.top > prev.bottom, "{prev:?} {row:?}");
                }
                next = row.end;
            }
            assert_eq!(next, aspects.len());
            // the binary search finds exactly the cells a linear scan does
            for (top, bottom) in [(0.0, 300.0), (1234.0, 2100.0), (l.height - 50.0, l.height + 900.0), (-500.0, -1.0)] {
                let band = Rect::from_min_max(pos2(0.0, top), pos2(2000.0, bottom));
                let want: Vec<usize> = (0..aspects.len()).filter(|i| l.cells[*i].intersects(band)).collect();
                let got: Vec<usize> = rows_between(&l.rows, top, bottom).iter().flat_map(|r| r.start..r.end).collect();
                assert_eq!(got, want, "square {square} {top}..{bottom}");
            }
        }
    }

    #[test]
    fn square_grid_groups_and_auto_levels() {
        let l = layout(&[1.0; 5], &[(0, 3), (3, 2)], 400.0, 100.0, true);
        // 4 columns: group 1 fills 3 cells of a row, group 2 starts a new row
        assert_eq!(l.cells[0].top(), l.cells[2].top());
        assert_eq!(l.cells[3].left(), 4.0);
        assert!(l.cells[3].top() > l.cells[0].bottom() + HEADER_H - 1.0);
        assert!((l.cells[0].width() - l.cells[0].height()).abs() < 0.01);
        assert_eq!(resolve_group(GroupBy::Auto, 220.0), GroupBy::Day);
        assert_eq!(resolve_group(GroupBy::Auto, 120.0), GroupBy::Month);
        assert_eq!(resolve_group(GroupBy::Auto, 90.0), GroupBy::Year);
        assert_eq!(resolve_group(GroupBy::None, 220.0), GroupBy::None);
        assert_eq!(resolve_group(GroupBy::Year, 400.0), GroupBy::Year);
    }

    /// The Compact strip under the thumbnails adds the same height to every cell and keeps the
    /// pictures' widths: the marks sit under the picture, never over it.
    #[test]
    fn a_footer_grows_each_cell_under_its_picture() {
        let aspects = [1.5, 0.66, 1.0, 1.5, 1.5];
        for square in [false, true] {
            let plain = layout(&aspects, &[], 600.0, 150.0, square);
            let under = layout_with_footer(&aspects, &[], 600.0, 150.0, square, BADGE_STRIP);
            for (a, b) in plain.cells.iter().zip(&under.cells) {
                assert!((b.width() - a.width()).abs() < 1e-3);
                assert!((b.height() - a.height() - BADGE_STRIP).abs() < 1e-3);
            }
            for w in under.rows.windows(2) {
                assert!(w[1].top >= w[0].bottom, "rows don't overlap");
            }
            assert_eq!(layout_with_footer(&aspects, &[], 600.0, 150.0, square, f32::NAN).cells, plain.cells);
        }
    }

    /// In every theme the Compact cell levels are unselected < pick < selected < active (a
    /// selection of picks still reads as selected), each step plainly visible.
    #[test]
    fn cell_levels_keep_picks_below_the_selection_in_every_theme() {
        use crate::theme::{ThemeKind, Tokens};
        use lcc_engine::SelectionState as S;
        for kind in ThemeKind::ALL {
            let t = Tokens::for_kind(kind);
            // distance from the unselected cell, whichever way the theme brightens
            let d = |c: Color32| (i32::from(c.r()) - i32::from(t.cell.r())).abs();
            let pick = d(cell_fill(&t, S::NotSelected, Flag::Pick, false, t.cell));
            let selected = d(cell_fill(&t, S::Selected, Flag::Pick, false, t.cell));
            let active = d(cell_fill(&t, S::Active, Flag::None, false, t.cell));
            assert!(pick < 16, "{kind:?}: a pick is only a touch lighter ({pick})");
            assert!(selected >= pick + 20, "{kind:?}: selected ({selected}) is well above a pick ({pick})");
            assert!(active >= selected + 16, "{kind:?}: active ({active}) is above selected ({selected})");
        }
    }

    /// A cell too small for a glow gets none; any other gets one inside it.
    /// A rejected photo's mark sat red on its red-glowing strip and hardly showed: it is a lighter
    /// red with a dark outline now, at least 3:1 against the strip, with the outline darker still.
    #[test]
    fn a_rejected_photos_mark_reads_on_its_red_strip() {
        use serde_json::json;
        let t = std::time::Duration::from_secs(20);
        let app = crate::LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        h.settle(std::time::Duration::from_secs(120));
        for (method, params) in
            [("engine.execute", json!({"command": "view.compactLayout", "params": {"on": true}})), ("ui.set", json!({"view": "photoGrid"}))]
        {
            let r = h.request(method, params, t);
            assert_eq!(r["ok"], true, "{method}: {r}");
        }
        h.settle(std::time::Duration::from_secs(120));
        h.step();
        // a photo in the grid's top row that isn't the active one (its strip in plain view)
        let active = h.app.session.active().map(|p| p.0);
        let (id, _) = h
            .app
            .widgets
            .iter()
            .filter_map(|(w, r)| w.strip_prefix("cellmarks:").and_then(|n| n.parse::<u64>().ok()).map(|n| (n, *r)))
            .filter(|(n, _)| Some(*n) != active)
            .min_by(|a, b| (a.1.top(), a.1.left()).partial_cmp(&(b.1.top(), b.1.left())).unwrap_or(std::cmp::Ordering::Equal))
            .expect("a photo's strip");
        let r = h.request("engine.execute", json!({"command": "photo.flag", "params": {"flag": "reject", "ids": [id]}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(std::time::Duration::from_secs(120));
        h.step();
        let marks = h.app.widgets.iter().find(|(w, _)| *w == format!("cellmarks:{id}")).map(|(_, r)| *r).expect("the rejected photo's strip");
        let image = h.paint();
        let w = image.size[0];
        let px = |x: f32, y: f32| image.pixels[y.round() as usize * w + x.round() as usize];
        // relative luminance (WCAG)
        let lum = |c: Color32| {
            let lin = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
        };
        let hgt = marks.height().clamp(10.0, 40.0);
        let flag = (hgt * 0.8).round().max(10.0);
        let (x0, y) = (marks.left() + 2.0, marks.center().y);
        let bg = lum(px(x0 + flag + 2.5, y));
        let (mut bright, mut dark) = (0.0f32, 1.0f32);
        for yy in (y - flag / 2.0).round() as i32..(y + flag / 2.0).round() as i32 {
            for xx in (x0 - 1.0).round() as i32..(x0 + flag + 1.0).round() as i32 {
                let l = lum(px(xx as f32, yy as f32));
                bright = bright.max(l);
                dark = dark.min(l);
            }
        }
        let contrast = (bright + 0.05) / (bg + 0.05);
        assert!(contrast >= 3.0, "the reject mark stands out from its strip ({contrast:.2}:1)");
        assert!(dark < bg * 0.6, "a dark outline round it ({dark:.3} vs strip {bg:.3})");
    }

    #[test]
    fn the_flag_glow_stays_inside_its_cell() {
        let cell = Rect::from_min_size(pos2(10.0, 10.0), vec2(120.0, 90.0));
        let g = flag_glow_rect(cell).expect("a glow");
        assert!(cell.contains_rect(g));
        assert!(g.bottom() < cell.bottom() && g.left() > cell.left() && g.right() < cell.right());
        assert!(flag_glow_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(4.0, 2.0))).is_none());
    }

    /// The Compact strip's flags are one shape (OneSlips LR-FLAGS): a pick is a solid flag, a reject the same solid
    /// flag with a bold cross below its banner, so they differ in shape, not only in colour; both are bold enough to
    /// read at a glance (the reject no longer a thin struck-through outline).
    #[test]
    fn compact_pick_and_reject_marks_differ_in_shape() {
        // lit pixels of the mark drawn white at 40 points: the whole icon, and its lower right quarter (under the banner)
        let ink = |icon: Icon| {
            let mut view = crate::headless::HeadlessView::new();
            let raw = crate::headless::HeadlessView::raw_input(vec2(40.0, 40.0), 1.0, 0.0, vec![]);
            view.run(raw, |ui| {
                ui.painter().rect_filled(ui.max_rect(), 0.0, Color32::BLACK);
                paint(ui.painter(), Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 40.0)), icon, Color32::WHITE);
            });
            let image = view.paint(&Default::default());
            let w = image.size[0];
            let lit = |p: &Color32| p.r() > 100;
            let all = image.pixels.iter().filter(|p| lit(p)).count();
            let corner = image.pixels.iter().enumerate().filter(|(i, p)| i % w >= w / 2 && i / w >= image.size[1] * 6 / 10 && lit(p)).count();
            // the middle of the banner (grid point 9, 6.4 of 20, at 2 pixels a point)
            let middle = image.pixels.get(13 * w + 18).is_some_and(lit);
            (all, corner, middle)
        };
        let pick = flag_mark_icon(Flag::Pick).expect("a pick has a mark");
        let reject = flag_mark_icon(Flag::Reject).expect("a reject has a mark");
        assert!(flag_mark_icon(Flag::None).is_none(), "an unflagged photo has no mark in the strip");
        let (pick_all, pick_corner, pick_middle) = ink(pick);
        let (reject_all, reject_corner, reject_middle) = ink(reject);
        assert!(pick_corner < 5, "nothing under the pick's banner ({pick_corner})");
        assert!(reject_corner > 30, "the reject's cross sits under its banner ({reject_corner})");
        // the banner is solid in both, where an outline is hollow
        let (outline, _, outline_middle) = ink(Icon::FlagMarkNone);
        assert!(pick_middle && reject_middle && !outline_middle, "solid banners, a hollow outline");
        assert!(pick_all > outline, "the pick's banner is filled ({pick_all} vs outline {outline})");
        assert!(reject_all > pick_all, "the reject adds its cross to the solid flag");
    }
}
