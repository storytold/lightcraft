//! The Keywording box (the top of the right panel's Keywords): the keywords of the selected photos,
//! not only the active one's, as Lightroom Classic shows them. A keyword only some of the selected
//! photos have is marked as such.

use lightcraft_catalog::{Catalog, PhotoId};
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// The Keywording box's view: the keywords (chips), them with the keywords containing them, or
/// what exported files will carry.
pub fn view_switch(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use crate::state::KeywordingView as V;
    ui.horizontal_wrapped(|ui| {
        for (view, id, label) in
            [(V::Keywords, "keywords", "Keywords"), (V::Containing, "containing", "& Containing"), (V::WillExport, "willExport", "Will Export")]
        {
            let r = ui.selectable_label(app.ui.keywording_view == view, crate::i18n::tr(label));
            register(ui.ctx(), format!("keywordView:{id}"), r.rect);
            if r.clicked() {
                app.ui.keywording_view = view;
            }
        }
    });
}

/// A read-only view of the selection's keywords: with the keywords containing them, or what
/// exported files will carry. One only some of the photos have is marked with an asterisk.
pub fn names_row(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let selection = app.session.selected();
    let export = app.ui.keywording_view == crate::state::KeywordingView::WillExport;
    let names = app.caches.keyword_names(&app.session.catalog, &selection, export);
    if names.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr(if export { "No keywords are exported" } else { "No keywords yet" })).color(t.text_dim));
        return;
    }
    let prefix = if export { "keywordExport" } else { "keywordContaining" };
    ui.horizontal_wrapped(|ui| {
        for n in names.iter() {
            let (body, _) = chip(ui, &n.path, &n.path, !n.on_all(), false, ui.max_rect().width());
            register(ui.ctx(), format!("{prefix}:{}", n.path), body.rect);
            if n.on_all() {
                body.on_hover_text(&n.path);
            } else {
                let count = crate::i18n::tr_format!("On {have} of {of} selected photos", have = n.have, of = n.of);
                body.on_hover_text(format!("{}\n{count}", n.path));
            }
        }
    });
}

/// Chips on a line of the grouped box are this far apart.
const GAP: f32 = 6.0;
/// The × at a chip's end is a target this wide, and as tall as the chip.
const CROSS_W: f32 = 22.0;

/// The room a chip needs besides its name: the space before it and, with a ×, the ×'s target (the
/// × sits in the middle of it, which leaves the name its space), else the space after it.
fn chip_edges(cross: bool) -> f32 {
    8.0 + if cross { CROSS_W } else { 8.0 }
}

/// What a chip shows of `name` in a row `row_w` wide (cut short to fit it), and the chip's width.
fn chip_size(ui: &egui::Ui, name: &str, partial: bool, cross: bool, row_w: f32) -> (String, f32) {
    let font = Tokens::get(ui.ctx()).font(13.0);
    let room = (row_w - chip_edges(cross)).max(24.0);
    let measure = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), egui::Color32::WHITE).size().x;
    let shown = chip_label(name, partial, room, measure);
    let w = measure(&shown) + chip_edges(cross);
    (shown, w)
}

/// One chip of the keyword `path`, measured before it is placed so that a wrapping row moves it
/// whole to the next line: `name` (the path, or what the grouped box shows of it; cut short to fit
/// a row `row_w` wide) and, with `cross`, a × at its end. A `partial` one (only some of the
/// selected photos) is dimmed and marked with an asterisk. Returns the name's response and the ×'s.
fn chip(ui: &mut egui::Ui, path: &str, name: &str, partial: bool, cross: bool, row_w: f32) -> (egui::Response, Option<egui::Response>) {
    use egui::{Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
    let t = Tokens::get(ui.ctx());
    let font = t.font(13.0);
    let color = if partial { t.text_dim } else { t.text_label };
    let cross_w = if cross { CROSS_W } else { 0.0 };
    let (shown, w) = chip_size(ui, name, partial, cross, row_w);
    // the row places it; its name and × answer under ids of their own keyword, so an open menu
    // stays with its keyword when the chips change
    let (rect, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
    let id = egui::Id::new(("keyword-chip", path.to_lowercase(), cross));
    let body = Rect::from_min_max(rect.min, pos2(rect.right() - cross_w, rect.bottom()));
    let full = path.replace('|', " › ");
    let resp = ui.interact(body, id, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &full));
    // the ×: the whole end of the chip, so that it is hard to miss and easy to tell from the name
    let x = cross.then(|| {
        let xr = Rect::from_min_max(pos2(rect.right() - cross_w, rect.top()), rect.max);
        let xresp = ui.interact(xr, id.with("remove"), Sense::click());
        let said = crate::i18n::tr_format!("Remove “{name}”", name = full);
        xresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &said));
        xresp
    });
    let p = ui.painter();
    p.rect_stroke(rect, 10.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    if resp.hovered() {
        p.rect_filled(rect.shrink(1.0), 10.0, t.hover.gamma_multiply(0.5));
    }
    p.text(pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, &shown, font, color);
    if let Some(x) = &x {
        // under the pointer the × lights up on its own: this click removes, the name's doesn't
        if x.hovered() {
            p.rect_filled(x.rect.shrink(1.0), CornerRadius { nw: 0, sw: 0, ne: 10, se: 10 }, t.hover);
        }
        let (c, arm) = (x.rect.center(), 3.5);
        let stroke = Stroke::new(1.5, if x.hovered() || x.has_focus() { t.text } else { t.text_dim });
        p.line_segment([c + vec2(-arm, -arm), c + vec2(arm, arm)], stroke);
        p.line_segment([c + vec2(-arm, arm), c + vec2(arm, -arm)], stroke);
        if x.has_focus() {
            p.rect_stroke(x.rect, CornerRadius { nw: 4, sw: 4, ne: 10, se: 10 }, Stroke::new(1.5, t.accent), StrokeKind::Inside);
        }
    }
    // reached with Tab, a chip shows where the keyboard is
    if resp.has_focus() {
        p.rect_stroke(rect, 10.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
    }
    (resp, x)
}

/// Take the chip's keyword off the selected photos. Several photos losing it at once is said, with
/// the way back (one photo's chip going is enough); a refusal is said too.
fn remove(app: &mut LightcraftApp, ctx: &egui::Context, chip: &Chip) {
    match app.run("photo.setMeta", json!({"removeKeywords": [chip.path]})) {
        Ok(_) if chip.have > 1 => {
            let name = chip.path.replace('|', " › ");
            app.toast_for(
                ctx,
                crate::i18n::tr_format!("Removed “{name}” from {n} photos. Edit ▸ Undo puts it back.", name = name, n = chip.have),
                6.0,
            );
        }
        Ok(_) => {}
        Err(e) => app.toast_error(ctx, e),
    }
}

/// A chip's menu: it acts on the selected photos (deleting a keyword from the whole library is the
/// Keyword List's).
fn menu(app: &mut LightcraftApp, ui: &mut egui::Ui, chip: &Chip) {
    let item = |ui: &mut egui::Ui, id: &str, label: &str| {
        let r = ui.button(label);
        register(ui.ctx(), format!("keywordChipMenu:{id}"), r.rect);
        r.clicked()
    };
    if !chip.on_all() && item(ui, "add", crate::i18n::tr("Add to All Selected Photos")) {
        let _ = app.run("photo.setMeta", json!({"addKeywords": [chip.path]}));
    }
    if item(ui, "remove", crate::i18n::tr("Remove from Selected Photos")) {
        remove(app, ui.ctx(), chip);
    }
    if item(ui, "show", crate::i18n::tr("Show Photos with Keyword")) {
        super::left::browse_all_photos(app, true);
        let _ = app.run("library.filter", json!({"keyword": chip.path}));
    }
    ui.separator();
    if item(ui, "edit", crate::i18n::tr("Edit Keyword Tag…")) {
        app.ui.dialog = Some(super::keyword_list::edit_dialog(app, &chip.path));
    }
}

/// With several photos selected, how many the box's keywords are of (`keywordScope`): what a ×
/// or a keyword typed here acts on.
pub fn scope(app: &mut LightcraftApp, ui: &mut egui::Ui, selection: &[PhotoId]) {
    let photos = app.caches.selected_photos(&app.session.catalog, selection);
    if photos < 2 {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let said = crate::i18n::tr_format!("Keywords of {n} selected photos", n = photos);
    let r = ui.label(egui::RichText::new(said).size(11.5).color(t.text_label));
    register(ui.ctx(), "keywordScope", r.rect);
    ui.add_space(4.0);
}

/// The selection's keywords as chips: the name (right-click for its menu), and × to take it off
/// every selected photo. One only some of them have is marked with an asterisk. Whole names, or
/// grouped by the keyword they are inside (Settings ▸ Interface).
pub fn chip_row(app: &mut LightcraftApp, ui: &mut egui::Ui, selection: &[PhotoId]) {
    let chips = app.caches.keyword_chips(&app.session.catalog, selection);
    if chips.is_empty() {
        let t = Tokens::get(ui.ctx());
        let r = ui.label(egui::RichText::new(crate::i18n::tr("No keywords yet")).color(t.text_dim));
        register(ui.ctx(), "keywordChipsEmpty", r.rect);
        return;
    }
    if app.ui.settings.keyword_chips == crate::state::KeywordChips::Grouped {
        let groups = app.caches.keyword_groups(&app.session.catalog, selection);
        chip_groups(app, ui, &groups, crate::key_of(selection));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for c in chips.iter() {
            let (body, x) = chip(ui, &c.path, &c.path, !c.on_all(), true, ui.max_rect().width());
            chip_acts(app, ui, c, body, x);
        }
    });
}

/// A drawn chip does its work: its × takes the keyword off the selected photos, its name has the
/// tooltip and the menu.
fn chip_acts(app: &mut LightcraftApp, ui: &egui::Ui, c: &Chip, body: egui::Response, x: Option<egui::Response>) {
    register(ui.ctx(), format!("keywordChip:{}", c.path), body.rect);
    if let Some(x) = x {
        register(ui.ctx(), format!("keywordChipRemove:{}", c.path), x.rect);
        if x.on_hover_text(crate::i18n::tr("Remove from Selected Photos")).clicked() {
            remove(app, ui.ctx(), c);
        }
    }
    // one tooltip: the whole name (it may be cut short), and how many have it
    let full = c.path.replace('|', " › ");
    let body = if c.on_all() {
        body.on_hover_text(full)
    } else {
        register(ui.ctx(), format!("keywordChipPartial:{}", c.path), body.rect);
        let n = crate::i18n::tr_format!("On {have} of {of} selected photos", have = c.have, of = c.of);
        body.on_hover_text(format!("{full}\n{n}"))
    };
    let c = c.clone();
    crate::widgets::context_menu(&body, |ui| menu(app, ui, &c));
}

/// When a list is shortened, only this many chips are measured for it: more than the lines it
/// can show hold in rows `avail` wide, however short their names are. The others are planned a
/// line each, past the lines shown.
pub(crate) fn measured(avail: f32) -> usize {
    // the narrowest chip: its edges and a letter
    let narrowest = chip_edges(true) + 4.0;
    let row = if avail.is_finite() && avail > narrowest { ((avail / narrowest) as usize).min(4096) } else { 1 };
    row.saturating_add(1).saturating_mul(LINES + SLACK + 1)
}

/// The grouped box: each group's parent on a line of its own, its chips under it side by side,
/// and "+N more" where a long list stops.
fn chip_groups(app: &mut LightcraftApp, ui: &mut egui::Ui, groups: &[Group], selection: u64) {
    use egui::{Align2, Sense, vec2};
    let t = Tokens::get(ui.ctx());
    let avail = ui.available_width();
    // the whole list only for the selection it was asked of
    let all = app.ui.keyword_chips_all == Some(selection);
    let limit = measured(avail);
    // the widths the lines are planned with (past what a shortened list can show: a line each)
    let mut measured = 0_usize;
    let sizes: Vec<(bool, Vec<f32>)> = groups
        .iter()
        .map(|g| {
            let widths = g
                .chips
                .iter()
                .map(|(name, c)| {
                    measured = measured.saturating_add(1);
                    if all || measured <= limit { chip_size(ui, name, !c.on_all(), true, avail).1 } else { avail }
                })
                .collect();
            (!g.parent.is_empty(), widths)
        })
        .collect();
    let small = t.font(11.5);
    for (i, line) in lines(&sizes, avail, GAP, all).into_iter().enumerate() {
        match line {
            Line::Heading(g) => {
                let Some(group) = groups.get(g) else { continue };
                ui.add_space(if i == 0 { 0.0 } else { 8.0 });
                let (r, resp) = ui.allocate_exact_size(vec2(avail, 18.0), Sense::hover());
                let measure = |s: &str| ui.painter().layout_no_wrap(s.to_string(), small.clone(), t.text_label).size().x;
                let shown = chip_label(&group.parent, false, avail, measure);
                let full = group.parent.replace('|', " › ");
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &full));
                ui.painter().text(r.left_center(), Align2::LEFT_CENTER, &shown, small.clone(), t.text_label);
                // (cut short, it says its whole name on hover, as a chip does)
                if shown != full {
                    resp.on_hover_text(&full);
                }
                register(ui.ctx(), format!("keywordGroup:{}", group.parent), r);
            }
            Line::Chips(g, row) => {
                let Some(group) = groups.get(g) else { continue };
                ui.add_space(if i == 0 { 0.0 } else { 4.0 });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for (name, c) in group.chips.get(row).into_iter().flatten() {
                        let (body, x) = chip(ui, &c.path, name, !c.on_all(), true, avail);
                        chip_acts(app, ui, c, body, x);
                    }
                });
            }
            Line::More(n) => {
                ui.add_space(6.0);
                let label = crate::i18n::tr_format!("+{n} more", n = n);
                let r = ui.add(egui::Button::new(egui::RichText::new(label).color(t.text_label)).corner_radius(10.0));
                register(ui.ctx(), "keywordChipsMore", r.rect);
                if r.on_hover_text(crate::i18n::tr("Show every keyword of the selected photos")).clicked() {
                    app.ui.keyword_chips_all = Some(selection);
                }
            }
            Line::Fewer => {
                ui.add_space(6.0);
                let r = ui.add(egui::Button::new(egui::RichText::new(crate::i18n::tr("Show fewer")).color(t.text_label)).corner_radius(10.0));
                register(ui.ctx(), "keywordChipsFewer", r.rect);
                if r.clicked() {
                    app.ui.keyword_chips_all = None;
                }
            }
        }
    }
}

/// What the grouped box's suggestions say of the keywords `paths`: each one's own name, or its
/// whole path (" › " between its levels) when another of them has the same name.
pub(crate) fn suggestion_names(paths: &[String]) -> Vec<String> {
    use lightcraft_catalog::keywords::SEP;
    let leaf = |p: &str| p.rsplit(SEP).next().unwrap_or(p).to_lowercase();
    let mut named: std::collections::HashMap<String, usize> = Default::default();
    for p in paths {
        *named.entry(leaf(p)).or_default() += 1;
    }
    paths
        .iter()
        .map(|p| if named.get(&leaf(p)).is_some_and(|n| *n > 1) { p.replace(SEP, " › ") } else { p.rsplit(SEP).next().unwrap_or(p).to_string() })
        .collect()
}

/// A chip's label within `room` (as `measure` measures text): the keyword's path with " › "
/// between its levels, and an asterisk when it is `partial`. Too long, it is cut from the front so
/// that the keyword's own name and the asterisk stay ("…Lisbon › Belém tower *").
pub(crate) fn chip_label(path: &str, partial: bool, room: f32, measure: impl Fn(&str) -> f32) -> String {
    let full = path.replace('|', " › ");
    let marker = if partial { " *" } else { "" };
    let whole = format!("{full}{marker}");
    if measure(&whole) <= room {
        return whole;
    }
    // the longest end of the path that fits after "…"
    let chars: Vec<char> = full.chars().collect();
    let mut best = format!("…{marker}");
    for start in (0..chars.len()).rev() {
        let tail: String = chars.get(start..).map(|c| c.iter().collect()).unwrap_or_default();
        let candidate = format!("…{}{marker}", tail.trim_start());
        if measure(&candidate) > room {
            break;
        }
        best = candidate;
    }
    best
}

/// The Add keyword box offers at most this many of the library's keywords.
pub(crate) const OFFERS: usize = 8;

/// What the Add keyword box offers under itself for the text typed in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Offers {
    /// The library's keywords matching the text (those starting with it first, then the most
    /// used), without the ones every selected photo has.
    pub keywords: Vec<String>,
    /// The keyword Return would create, when the library has none of that name: where it would go
    /// (inside the default parent, if there is one).
    pub create: Option<String>,
}

/// The text being typed in the Add keyword box: what follows its last comma.
pub(crate) fn typing(text: &str) -> &str {
    text.rsplit(',').next().unwrap_or("").trim()
}

/// The keywords typed before the last comma, each where the library would put it.
pub(crate) fn typed_before(catalog: &Catalog, text: &str) -> Vec<String> {
    let Some((before, _)) = text.rsplit_once(',') else { return Vec::new() };
    before.split(',').map(|s| catalog.typed_keyword(s)).filter(|s| !s.is_empty()).collect()
}

/// What the box offers for `typed`, for photos that all have the `current` keywords.
pub(crate) fn offers(catalog: &Catalog, current: &[String], typed: &str) -> Offers {
    use lightcraft_catalog::keywords::{SEP, clean};
    let name = clean(typed);
    if name.is_empty() {
        return Offers::default();
    }
    // (a keyword two photos spell differently is one offer, the first spelling)
    let mut seen = std::collections::HashSet::new();
    let mut keywords = catalog.keyword_suggestions(current, typed, OFFERS);
    keywords.retain(|k| seen.insert(k.to_lowercase()));
    // where Return would put a name the library lacks, as `Catalog::typed_keyword` does, without
    // looking through the library a second time
    let create = (!catalog.has_keyword(&name)).then(|| match catalog.default_keyword_parent() {
        Some(parent) if !name.contains(SEP) => format!("{parent}{SEP}{name}"),
        _ => name,
    });
    Offers { keywords, create }
}

/// What was chosen from the offers under the Add keyword box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Picked {
    /// One of the library's keywords.
    Keyword(String),
    /// The new keyword, as typed.
    Typed,
}

/// The offers under the Add keyword box (`below` is the box): the library's keywords matching
/// what is typed, each by its own name with the keyword it is inside after it, the one at `at`
/// highlighted (↑ / ↓); then, for a name the library doesn't have, the keyword Return creates.
/// They go above the box where the window has no room for them under it. Returns the offer
/// clicked and where the offers were drawn.
pub(crate) fn offers_below(ui: &egui::Ui, below: egui::Rect, offers: &Offers, at: Option<&str>) -> (Option<Picked>, egui::Rect) {
    use egui::{Align2, Sense, pos2, vec2};
    use lightcraft_catalog::keywords::SEP;
    let t = Tokens::get(ui.ctx());
    let mut picked = None;
    let width = (below.width() - 10.0).max(60.0);
    // under the box, or over it where the window has more room there; as many lines as fit that
    // side, so that the offers never cover the box (the highlighted one is always among them)
    let window = ui.ctx().content_rect();
    let create = usize::from(offers.create.is_some());
    let fits = |room: f32| {
        let lines = (room - 2.0 - if create > 0 && !offers.keywords.is_empty() { 17.0 } else { 10.0 }) / 24.0;
        if lines.is_finite() && lines >= 1.0 { lines as usize } else { 0 }
    };
    let wanted = offers.keywords.len().saturating_add(create);
    let (under, above) = (fits(window.bottom() - below.bottom()), fits(below.top() - window.top()));
    let over = under < wanted && above > under;
    let room = (if over { above } else { under }).clamp(1, wanted.max(1));
    // the keywords shown: those up to the highlighted one when not all fit
    let count = room.saturating_sub(create).min(offers.keywords.len());
    let lit = at.and_then(|k| offers.keywords.iter().position(|x| x == k));
    let first = lit.map_or(0, |a| a.saturating_add(1).saturating_sub(count)).min(offers.keywords.len().saturating_sub(count));
    let listed = offers.keywords.iter().skip(first).take(count);
    let area = egui::Area::new(egui::Id::new("keyword-offers")).order(egui::Order::Foreground).constrain(true);
    let area = if over {
        area.pivot(Align2::LEFT_BOTTOM).fixed_pos(below.left_top() - vec2(0.0, 2.0))
    } else {
        area.fixed_pos(below.left_bottom() + vec2(0.0, 2.0))
    };
    let shown = area.show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).inner_margin(4.0).show(ui, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            // a line: the keyword's own name, then (dimmer, cut from the front) where it is
            let line = |ui: &mut egui::Ui, id: egui::Id, name: &str, place: &str, said: &str, lit: bool| {
                let (r, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
                let resp = ui.interact(r, id, Sense::click()).union(resp);
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, lit, said));
                if lit || resp.hovered() {
                    ui.painter().rect_filled(r, 4.0, if lit { t.pressed } else { t.hover });
                }
                let font = t.font(13.0);
                let measure = |s: &str, f: &egui::FontId| ui.painter().layout_no_wrap(s.to_string(), f.clone(), t.text).size().x;
                let name = chip_label(name, false, width - 16.0, |s| measure(s, &font));
                let end = r.left() + 8.0 + measure(&name, &font);
                ui.painter().text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, &name, font, t.text);
                let small = t.font(11.5);
                let room = r.right() - 8.0 - (end + 10.0);
                if !place.is_empty() && room >= 30.0 {
                    let place = chip_label(place, false, room, |s| measure(s, &small));
                    ui.painter().text(pos2(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, place, small, t.text_label);
                }
                resp
            };
            for k in listed {
                let (place, name) = k.rsplit_once(SEP).unwrap_or(("", k));
                let whole = k.replace(SEP, " › ");
                let lit = at == Some(k.as_str());
                let r = line(ui, egui::Id::new(("keyword-offer", k.to_lowercase())), name, place, &whole, lit);
                register(ui.ctx(), format!("kwComplete:{k}"), r.rect);
                if lit {
                    register(ui.ctx(), format!("kwCompleteAt:{k}"), r.rect);
                }
                if r.clicked() {
                    picked = Some(Picked::Keyword(k.clone()));
                }
            }
            if let Some(new) = &offers.create {
                if count > 0 {
                    ui.add_space(3.0);
                    let (r, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
                    ui.painter().rect_filled(r, 0.0, t.field_border);
                    ui.add_space(3.0);
                }
                let (place, name) = new.rsplit_once(SEP).unwrap_or(("", new));
                let whole = new.replace(SEP, " › ");
                let (place, said) = if place.is_empty() {
                    (crate::i18n::tr("new keyword").to_string(), crate::i18n::tr_format!("Create the keyword “{name}”", name = whole))
                } else {
                    let inside = place.replace(SEP, " › ");
                    (
                        crate::i18n::tr_format!("new, inside {parent}", parent = inside),
                        crate::i18n::tr_format!("Create the keyword “{name}”", name = whole),
                    )
                };
                // (its place is said in words: not a path to cut from the front)
                let r = line(ui, egui::Id::new("keyword-offer-create"), name, "", &said, false);
                let small = t.font(11.5);
                ui.painter().text(pos2(r.rect.right() - 8.0, r.rect.center().y), Align2::RIGHT_CENTER, place, small, t.text_label);
                register(ui.ctx(), "kwCompleteCreate", r.rect);
                // (Return gives the highlighted offer instead, while there is one)
                let r = if at.is_none() {
                    r.on_hover_text(crate::i18n::tr("Return creates this keyword and gives it to the selected photos"))
                } else {
                    r
                };
                if r.clicked() {
                    picked = Some(Picked::Typed);
                }
            }
        });
    });
    (picked, shown.response.rect)
}

/// Keywords of the selection that are inside the same keyword, as the grouped box shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Group {
    /// The keyword they are inside, spelled as the first of them spells it; empty for the keywords
    /// that share none with another.
    pub parent: String,
    /// Each keyword's name below `parent`, and its chip.
    pub chips: Vec<(String, Chip)>,
}

/// The chips grouped by the keyword they are inside: the deepest one a chip shares with another
/// chip ("Places|Italy" for "Places|Italy|Rome" next to "Places|Italy|Venice"; "Transport" for
/// "Transport|Rail|Night Train" next to "Transport|Ferry"). Those that share none come first, in a group
/// without a parent; then the groups by name, each in the chips' order.
pub(crate) fn grouped(chips: &[Chip]) -> Vec<Group> {
    use lightcraft_catalog::keywords::SEP;
    let sep = SEP.to_string();
    // a path's first `depth` levels, and the rest
    let head = |levels: &[&str], depth: usize| levels.get(..depth).unwrap_or_default().join(&sep);
    let tail = |levels: &[&str], depth: usize| levels.get(depth..).unwrap_or_default().join(&sep);
    // how many chips are inside each keyword (by its lower-case path)
    let mut inside: std::collections::HashMap<String, usize> = Default::default();
    for c in chips {
        let levels: Vec<&str> = c.path.split(SEP).collect();
        for depth in 1..levels.len() {
            *inside.entry(head(&levels, depth).to_lowercase()).or_default() += 1;
        }
    }
    let mut groups: std::collections::BTreeMap<String, Group> = Default::default();
    for c in chips {
        let levels: Vec<&str> = c.path.split(SEP).collect();
        let shared = |depth: &usize| inside.get(&head(&levels, *depth).to_lowercase()).is_some_and(|n| *n > 1);
        let depth = (1..levels.len()).rev().find(shared).unwrap_or(0);
        let parent = head(&levels, depth);
        // (ordered level by level: "Port › Docks" follows "Port", before "Portugal")
        let key = parent.to_lowercase().replace(SEP, "\u{1}");
        groups.entry(key).or_insert_with(|| Group { parent, chips: Vec::new() }).chips.push((tail(&levels, depth), c.clone()));
    }
    groups.into_values().collect()
}

/// Items `widths` wide laid out left to right in rows `avail` wide, `gap` apart: the items of each
/// row. An item wider than a row has a row to itself.
pub(crate) fn rows_of(widths: &[f32], avail: f32, gap: f32) -> Vec<std::ops::Range<usize>> {
    let mut rows = Vec::new();
    let (mut start, mut used) = (0, 0.0_f32);
    for (i, w) in widths.iter().enumerate() {
        let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
        if i > start && used + gap + w > avail {
            rows.push(start..i);
            (start, used) = (i, w);
        } else {
            used = if i == start { w } else { used + gap + w };
        }
    }
    if start < widths.len() {
        rows.push(start..widths.len());
    }
    rows
}

/// A line of the grouped box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Line {
    /// The name of a group's parent.
    Heading(usize),
    /// Chips of a group (by their place in it).
    Chips(usize, std::ops::Range<usize>),
    /// "+N more": this many chips are not shown.
    More(usize),
    /// "Show fewer", under a list shown whole that would otherwise be shortened.
    Fewer,
}

/// The grouped box shows this many lines of a long list (then "+N more")…
pub(crate) const LINES: usize = 8;
/// …but a list only this many lines longer is shown whole: "+1 more" saves nothing.
pub(crate) const SLACK: usize = 2;

/// The lines of the grouped box: for each group whether it has a heading and how wide its chips
/// are, in rows `avail` wide with chips `gap` apart. A list longer than [`LINES`] + [`SLACK`]
/// lines stops after [`LINES`] and says how many chips are left, unless `all` of it is asked for.
pub(crate) fn lines(groups: &[(bool, Vec<f32>)], avail: f32, gap: f32, all: bool) -> Vec<Line> {
    let mut out = Vec::new();
    for (g, (heading, widths)) in groups.iter().enumerate() {
        if widths.is_empty() {
            continue;
        }
        if *heading {
            out.push(Line::Heading(g));
        }
        out.extend(rows_of(widths, avail, gap).into_iter().map(|row| Line::Chips(g, row)));
    }
    if out.len() <= LINES + SLACK {
        return out;
    }
    if all {
        out.push(Line::Fewer);
        return out;
    }
    out.truncate(LINES);
    if matches!(out.last(), Some(Line::Heading(_))) {
        out.pop();
    }
    let count = |lines: &[Line]| lines.iter().map(|l| if let Line::Chips(_, row) = l { row.len() } else { 0 }).sum::<usize>();
    let total: usize = groups.iter().map(|(_, widths)| widths.len()).sum();
    let left = total.saturating_sub(count(&out));
    out.push(Line::More(left));
    out
}

/// A keyword of the selection, as a chip shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chip {
    /// The keyword, spelled as the library spells it.
    pub path: String,
    /// Selected photos that have it.
    pub have: usize,
    /// Selected photos.
    pub of: usize,
}

impl Chip {
    /// Every selected photo has it.
    pub fn on_all(&self) -> bool {
        self.have >= self.of
    }
}

/// "Will Export" (Lightroom Classic's Keyword Tags ▸ Will Export): the names exported files carry
/// for the selected `photos`, as the keyword tag options say, each with how many of them carry it;
/// by name.
pub(crate) fn will_export(catalog: &Catalog, photos: &[PhotoId]) -> Vec<Chip> {
    let photos = distinct(catalog, photos);
    let mut have: std::collections::BTreeMap<String, (String, usize)> = Default::default();
    for p in &photos {
        for name in catalog.export_keywords(&p.meta.keywords).flat {
            have.entry(name.to_lowercase()).or_insert_with(|| (name.clone(), 0)).1 += 1;
        }
    }
    have.into_values().map(|(path, have)| Chip { path, have, of: photos.len() }).collect()
}

/// "Keywords & Containing Keywords": the names of the selection's keywords and of the keywords
/// containing them, each with how many of the selected photos have it (itself or below it); by
/// name.
pub(crate) fn with_containing(catalog: &Catalog, photos: &[PhotoId]) -> Vec<Chip> {
    use lightcraft_catalog::keywords::{SEP, clean};
    let photos = distinct(catalog, photos);
    let mut have: std::collections::BTreeMap<String, (String, usize)> = Default::default();
    let mut seen = std::collections::HashSet::new();
    for p in &photos {
        seen.clear();
        for k in &p.meta.keywords {
            for name in clean(k).split(SEP) {
                if !name.is_empty() && seen.insert(name.to_lowercase()) {
                    have.entry(name.to_lowercase()).or_insert_with(|| (name.to_string(), 0)).1 += 1;
                }
            }
        }
    }
    have.into_values().map(|(path, have)| Chip { path, have, of: photos.len() }).collect()
}

/// The selected photos, each once, those the library has: what "N of M" counts.
pub(crate) fn distinct<'a>(catalog: &'a Catalog, photos: &[PhotoId]) -> Vec<&'a lightcraft_catalog::Photo> {
    let mut seen = std::collections::HashSet::new();
    photos.iter().filter(|id| seen.insert(**id)).filter_map(|id| catalog.photo(*id).map(|p| &**p)).collect()
}

/// The keywords of the selected `photos`, each once whatever its case, with how many of them have
/// it; by name.
pub(crate) fn chips(catalog: &Catalog, photos: &[PhotoId]) -> Vec<Chip> {
    use lightcraft_catalog::keywords::clean;
    let photos = distinct(catalog, photos);
    // by lower-case keyword: as first seen, and the photos with it (each once)
    let mut have: std::collections::BTreeMap<String, (String, usize)> = Default::default();
    let mut seen = std::collections::HashSet::new();
    for p in &photos {
        seen.clear();
        for k in &p.meta.keywords {
            let k = clean(k);
            if k.is_empty() || !seen.insert(k.to_lowercase()) {
                continue;
            }
            have.entry(k.to_lowercase()).or_insert_with(|| (k.clone(), 0)).1 += 1;
        }
    }
    // spelled as the keyword list spells it, else as first seen (no scan of the library)
    have.into_values().map(|(k, n)| Chip { path: catalog.listed_path(&k).map(str::to_string).unwrap_or(k), have: n, of: photos.len() }).collect()
}

#[cfg(test)]
mod tests {
    use lightcraft_catalog::{Op, Photo, Source};

    use super::*;

    fn library(keywords: &[&[&str]]) -> (Catalog, Vec<PhotoId>) {
        let mut c = Catalog::new();
        let mut ids = Vec::new();
        for k in keywords {
            let id = c.alloc_photo_id();
            let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a.jpg", "JPEG", 3, 2, "2026-01-01");
            p.meta.keywords = k.iter().map(|s| s.to_string()).collect();
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            ids.push(id);
        }
        (c, ids)
    }

    fn shown(chips: &[Chip]) -> Vec<(&str, usize, usize)> {
        chips.iter().map(|c| (c.path.as_str(), c.have, c.of)).collect()
    }

    /// The chips are the keywords of every selected photo, by name, each saying how many of the
    /// selected photos have it.
    #[test]
    fn the_chips_are_the_selections_keywords() {
        let (c, ids) = library(&[&["travel|Italy", "beach"], &["beach", "Weddings"], &["sea"]]);
        assert_eq!(shown(&chips(&c, &ids[..2])), [("beach", 2, 2), ("travel|Italy", 1, 2), ("Weddings", 1, 2)]);
        assert!(chips(&c, &ids[..2])[0].on_all() && !chips(&c, &ids[..2])[1].on_all());
        assert_eq!(shown(&chips(&c, &ids[2..])), [("sea", 1, 1)]);
        assert!(chips(&c, &[]).is_empty());
    }

    /// A keyword two photos spell differently is one chip, spelled as the library spells it.
    #[test]
    fn a_keyword_spelled_two_ways_is_one_chip() {
        let (c, ids) = library(&[&["Beach"], &["beach"], &["BEACH", "beach"]]);
        assert_eq!(shown(&chips(&c, &ids)), [("Beach", 3, 3)]);
    }

    /// "Will Export": the names exported files carry for the selection, as the keyword tag options
    /// say (a keyword left out goes, its parents and synonyms come), each with how many of the
    /// selected photos carry it.
    #[test]
    fn will_export_shows_what_exported_files_carry() {
        use lightcraft_catalog::keywords::KeywordInfo;
        let (mut c, ids) = library(&[&["Places|Lisbon", "draft"], &["Places|Lisbon"]]);
        let out = KeywordInfo { include_on_export: false, ..KeywordInfo::default() };
        c.apply(Op::SetKeyword { path: "Places".into(), info: Some(out.clone()) }).unwrap();
        c.apply(Op::SetKeyword { path: "draft".into(), info: Some(out) }).unwrap();
        c.apply(Op::SetKeyword {
            path: "Places|Lisbon".into(),
            info: Some(KeywordInfo { synonyms: vec!["Lisboa".into()], ..KeywordInfo::default() }),
        })
        .unwrap();
        assert_eq!(shown(&will_export(&c, &ids)), [("Lisboa", 2, 2), ("Lisbon", 2, 2)]);
        assert!(will_export(&c, &[]).is_empty());
    }

    /// A chip too long for its row is cut from the front: the keyword's own name and the asterisk
    /// of a partial one stay.
    #[test]
    fn a_long_chip_keeps_its_name_and_asterisk() {
        let chars = |s: &str| s.chars().count() as f32;
        assert_eq!(chip_label("travel|Lisbon", false, 40.0, chars), "travel › Lisbon");
        assert_eq!(chip_label("travel|Lisbon", true, 40.0, chars), "travel › Lisbon *");
        let cut = chip_label("Places|Portugal|Lisbon|Belém tower", true, 24.0, chars);
        assert_eq!(cut, "…Lisbon › Belém tower *");
        assert!(chars(&cut) <= 24.0);
        assert!(chip_label("Places|Lisbon", false, 4.0, chars).starts_with('…'), "even with hardly any room");
    }

    /// A photo listed twice in the selection counts once, and one that isn't in the library not at
    /// all: "On 1 of 2", not "On 2 of 4".
    #[test]
    fn the_counts_are_of_photos_not_ids() {
        let (c, ids) = library(&[&["beach"], &["sea"]]);
        let selection = [ids[0], ids[0], ids[1], PhotoId(999)];
        assert_eq!(shown(&chips(&c, &selection)), [("beach", 1, 2), ("sea", 1, 2)]);
        assert_eq!(shown(&will_export(&c, &selection)), [("beach", 1, 2), ("sea", 1, 2)]);
        let ticks = crate::panels::keyword_list::Ticks::of(&c, &selection);
        assert_eq!(ticks.tick("beach"), crate::panels::keyword_list::Tick::Some);
        assert_eq!(crate::panels::keyword_list::Ticks::of(&c, &[ids[0], ids[0]]).tick("beach"), crate::panels::keyword_list::Tick::All);
    }

    /// A keyword the Keyword List has is spelled as it spells it.
    #[test]
    fn a_listed_keyword_is_spelled_as_listed() {
        use lightcraft_catalog::keywords::KeywordInfo;
        let (mut c, ids) = library(&[&["beach"]]);
        c.apply(Op::SetKeyword { path: "Beach".into(), info: Some(KeywordInfo::default()) }).unwrap();
        assert_eq!(shown(&chips(&c, &ids)), [("Beach", 1, 1)]);
    }

    /// Screen readers hear a chip by its whole keyword (even when cut short on screen) and its ×
    /// as removing that keyword.
    #[test]
    fn chips_describe_themselves() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        let mut found = Vec::new();
        for _ in 0..3 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.set_max_width(120.0);
                let path = "Places|Portugal|Lisbon|Belém tower";
                let _ = chip(ui, path, "Belém tower", true, true, 120.0);
            });
            out.textures_delta.clear();
            if let Some(update) = out.platform_output.accesskit_update.take() {
                found = update.nodes.iter().map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string())).collect();
            }
        }
        let has = |role: &str, label: &str| found.iter().any(|(r, l)| r == role && l == label);
        assert!(has("Button", "Places › Portugal › Lisbon › Belém tower"), "{found:?}");
        assert!(has("Button", "Remove “Places › Portugal › Lisbon › Belém tower”"), "{found:?}");
    }

    fn chip_of(path: &str) -> Chip {
        Chip { path: path.into(), have: 1, of: 1 }
    }

    fn groups_of(paths: &[&str]) -> Vec<(String, Vec<String>)> {
        let chips: Vec<Chip> = paths.iter().map(|p| chip_of(p)).collect();
        grouped(&chips).into_iter().map(|g| (g.parent, g.chips.into_iter().map(|(name, _)| name).collect())).collect()
    }

    /// Given keywords inside the same keyword, when the box groups them, then they share a line
    /// naming that keyword and each says only what is below it; a keyword alone below its parent
    /// joins the nearest keyword it shares with another, and one that shares none comes first
    /// under no name.
    #[test]
    fn keywords_group_under_the_keyword_they_share() {
        let got = groups_of(&[
            "beach",
            "Places|Italy|Florence",
            "Places|Italy|Rome",
            "Places|Portugal|Lisbon",
            "Places|Portugal|Porto",
            "Transport|Ferry",
            "Transport|Rail|Night Train",
        ]);
        let want: Vec<(String, Vec<String>)> = [
            ("", vec!["beach"]),
            ("Places|Italy", vec!["Florence", "Rome"]),
            ("Places|Portugal", vec!["Lisbon", "Porto"]),
            ("Transport", vec!["Ferry", "Rail|Night Train"]),
        ]
        .into_iter()
        .map(|(p, c)| (p.to_string(), c.into_iter().map(str::to_string).collect()))
        .collect();
        assert_eq!(got, want);
    }

    /// A keyword and one inside it are both inside their shared parent; a hierarchy with one
    /// keyword stays whole, under no name; spelling differences of the parent don't split a group.
    #[test]
    fn grouping_holds_at_the_edges() {
        assert_eq!(groups_of(&["A|B", "A|B|C"]), [("A".to_string(), vec!["B".to_string(), "B|C".to_string()])]);
        assert_eq!(groups_of(&["Places|Portugal|Lisbon"]), [(String::new(), vec!["Places|Portugal|Lisbon".to_string()])]);
        assert_eq!(groups_of(&["Places|Rome", "places|Athens"]), [("Places".to_string(), vec!["Rome".to_string(), "Athens".to_string()])]);
        assert_eq!(groups_of(&["İstanbul|Köprü", "i̇stanbul|Kule"]).len(), 1, "a parent whose lower case is longer");
        assert!(groups_of(&[]).is_empty());
        // every chip is in exactly one group
        let paths = ["a", "a|b", "a|b|c", "a|d", "e|f", "g"];
        let n: usize = groups_of(&paths).iter().map(|(_, c)| c.len()).sum();
        assert_eq!(n, paths.len());
    }

    /// Chips fill a row until the next would not fit; one wider than the row has a row to itself,
    /// and widths that are not numbers don't break the layout.
    #[test]
    fn chips_fill_rows() {
        assert_eq!(rows_of(&[40.0, 40.0, 40.0], 100.0, 6.0), [0..2, 2..3]);
        assert_eq!(rows_of(&[40.0, 54.0], 100.0, 6.0).len(), 1, "exactly fits");
        assert_eq!(rows_of(&[300.0, 20.0], 100.0, 6.0), [0..1, 1..2]);
        assert_eq!(rows_of(&[20.0, 300.0, 20.0], 100.0, 6.0), [0..1, 1..2, 2..3]);
        assert!(rows_of(&[], 100.0, 6.0).is_empty());
        assert_eq!(rows_of(&[f32::NAN, f32::INFINITY, 10.0], 100.0, 6.0).iter().map(|r| r.len()).sum::<usize>(), 3);
        assert_eq!(rows_of(&[10.0, 10.0], 0.0, 6.0), [0..1, 1..2], "no room at all");
    }

    /// Given a list of more than LINES + SLACK lines, when it isn't asked for whole, then LINES
    /// lines show and the rest is counted; a heading is never the last line shown. Asked for
    /// whole, it ends in "Show fewer". A shorter list is shown as it is.
    #[test]
    fn a_long_list_is_shortened_and_says_how_much_is_left() {
        // a heading and 3 chips, a heading and 9 (one chip a row): 14 lines
        let groups = vec![(true, vec![90.0; 3]), (true, vec![90.0; 9])];
        let short = lines(&groups, 100.0, 6.0, false);
        assert_eq!(short.len(), LINES + 1, "{short:?}");
        assert_eq!(short.last(), Some(&Line::More(6)), "3 + 3 chips shown: {short:?}");
        let whole = lines(&groups, 100.0, 6.0, true);
        assert_eq!(whole.len(), 15);
        assert_eq!(whole.last(), Some(&Line::Fewer));
        // the eighth line would be the second group's heading: it goes too
        let cut = lines(&[(false, vec![90.0; 7]), (true, vec![90.0; 6])], 100.0, 6.0, false);
        assert_eq!(cut.len(), LINES, "{cut:?}");
        assert_eq!(cut.last(), Some(&Line::More(6)));
        assert!(cut.iter().all(|l| !matches!(l, Line::Heading(_))), "{cut:?}");
        // within the slack: whole, with neither button
        let fits = lines(&[(true, vec![90.0; 9])], 100.0, 6.0, false);
        assert_eq!(fits.len(), 10);
        assert!(fits.iter().all(|l| matches!(l, Line::Heading(_) | Line::Chips(..))));
        assert_eq!(lines(&[(true, vec![90.0; 9])], 100.0, 6.0, true), fits, "nothing to show fewer of");
        assert!(lines(&[], 100.0, 6.0, false).is_empty());
        assert!(lines(&[(true, vec![])], 100.0, 6.0, false).is_empty(), "a group without chips has no heading");
    }

    /// Given a library with Places › Lisbon, the box offers it for "lis" (not for photos that all
    /// have it), says a name the library lacks would be created (inside the default parent when
    /// there is one), and offers nothing for nothing.
    #[test]
    fn the_box_offers_what_matches_and_says_what_is_new() {
        use lightcraft_catalog::keywords::KeywordInfo;
        let (mut c, _) = library(&[&["Places|Lisbon", "sea"], &["Places|Porto"]]);
        let got = offers(&c, &[], "lis");
        assert_eq!(got.keywords, ["Places|Lisbon"]);
        assert_eq!(got.create.as_deref(), Some("lis"), "“lis” itself is no keyword");
        assert!(offers(&c, &["Places|Lisbon".into()], "lis").keywords.is_empty(), "they all have it");
        assert_eq!(offers(&c, &[], " SEA ").create, None, "the library has sea");
        assert_eq!(offers(&c, &[], "places").create, None, "and Places, which holds keywords");
        assert_eq!(offers(&c, &[], "  "), Offers::default());
        assert_eq!(offers(&c, &[], " | "), Offers::default());
        c.apply(Op::SetKeyword { path: "Inbox".into(), info: Some(KeywordInfo { new_keywords_inside: true, ..KeywordInfo::default() }) }).unwrap();
        assert_eq!(offers(&c, &[], "harbour").create.as_deref(), Some("Inbox|harbour"));
        assert_eq!(offers(&c, &[], "a|b").create.as_deref(), Some("a|b"), "a path typed whole goes where it says");
        assert!(offers(&c, &[], "p").keywords.len() <= OFFERS);
    }

    /// A keyword two photos spell differently is offered once.
    #[test]
    fn a_keyword_spelled_two_ways_is_one_offer() {
        let (c, _) = library(&[&["Lisbon"], &["lisbon"], &["LISBON", "Lisboa"]]);
        let got = offers(&c, &[], "lis").keywords;
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().any(|k| k.eq_ignore_ascii_case("lisbon")) && got.contains(&"Lisboa".to_string()));
    }

    /// Groups follow the tree: a keyword's groups come right after it, before a keyword whose name
    /// merely starts the same.
    #[test]
    fn groups_are_in_the_trees_order() {
        let parents: Vec<String> =
            groups_of(&["Portugal|a", "Portugal|b", "Port|Docks|j", "Port|Docks|k", "Port|x", "Port|y"]).into_iter().map(|(p, _)| p).collect();
        assert_eq!(parents, ["Port", "Port|Docks", "Portugal"]);
    }

    /// However wide the box, more chips are measured than a shortened list can show: the lines
    /// shown are planned with their real widths.
    #[test]
    fn enough_chips_are_measured_for_the_lines_shown() {
        for avail in [0.0, 60.0, 224.0, 474.0, 1500.0, 6000.0, f32::NAN, f32::INFINITY, -5.0] {
            let limit = measured(avail);
            assert!(limit > LINES + SLACK, "{avail}: {limit}");
            if avail.is_finite() && avail > 0.0 {
                // the narrowest chips there can be, a gap apart: those of the lines shown
                let narrowest = chip_edges(true) + 4.0;
                let fit = (LINES + SLACK + 1) * ((avail / (narrowest + GAP)) as usize + 1);
                assert!(limit >= fit, "{avail}: {limit} < {fit}");
            }
        }
        // a wide box: the lines shown are full rows, not one chip each past the 128th
        let widths = vec![40.0; 300];
        let limit = measured(1500.0);
        let planned: Vec<f32> = widths.iter().enumerate().map(|(i, w)| if i < limit { *w } else { 1500.0 }).collect();
        assert_eq!(lines(&[(false, planned)], 1500.0, GAP, false), lines(&[(false, widths)], 1500.0, GAP, false));
    }

    /// What is typed is what follows the last comma; what came before it goes along when an offer
    /// is taken.
    #[test]
    fn typing_is_what_follows_the_last_comma() {
        let (c, _) = library(&[&["sea"]]);
        assert_eq!(typing("sea, lis "), "lis");
        assert_eq!(typing("lis"), "lis");
        assert_eq!(typing("sea,"), "");
        assert_eq!(typing(""), "");
        assert_eq!(typed_before(&c, "sea, , beach ,lis"), ["sea", "beach"]);
        assert!(typed_before(&c, "lis").is_empty());
    }

    /// The grouped box's suggestions say a keyword's own name; two with the same name say their
    /// whole paths, so they can be told apart.
    #[test]
    fn suggestions_are_named_apart() {
        let paths: Vec<String> = ["Italy|Rome|Market", "Spain|Madrid|market", "Places|Lisbon", "sea"].iter().map(|s| s.to_string()).collect();
        assert_eq!(suggestion_names(&paths), ["Italy › Rome › Market", "Spain › Madrid › market", "Lisbon", "sea"]);
        assert!(suggestion_names(&[]).is_empty());
    }

    /// "Keywords & Containing Keywords": every level of the selection's keywords, flat, each
    /// counted once per photo.
    #[test]
    fn containing_keywords_are_every_level() {
        let (c, ids) = library(&[&["travel|Italy|Rome", "travel|Spain"], &["travel|Italy"]]);
        assert_eq!(shown(&with_containing(&c, &ids)), [("Italy", 2, 2), ("Rome", 1, 2), ("Spain", 1, 2), ("travel", 2, 2)]);
    }
}
