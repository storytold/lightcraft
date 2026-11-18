//! People: the faces of the library in the three categories the view sorts them into (issue #1050), picked in the row
//! over it:
//!
//! - **Unknown**: the raw detected faces nobody has named and no name is suggested for, as cropped pictures, to be
//!   named a group at a time (`unnamed.rs`) or ignored.
//! - **Suggested**: the unnamed faces recognition has matched to a person, the faces waiting under that person's own
//!   heading, to be confirmed, rejected or ignored person by person (`suggested.rs`).
//! - **Confirmed**: the people the library knows, a card per person named on faces (read from XMP): a close-up of their
//!   face and how many photos they are in (a small number on the picture). A click opens that person's page
//!   (`person.rs`): only their cropped faces, and the faces that look like them.
//!
//! The thumbnail-size slider in the bottom bar sizes the faces here too (with its own smaller and larger limits), and
//! every face is cut from the same kind of box (the detector's, once the scan has looked at it), so faces are shown
//! equally close. Only the rows on screen ask for a face render (the engine caches them, memory and disk).

use std::sync::Arc;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use lcc_catalog::Person;
use serde_json::json;

use crate::LccApp;
use crate::state::PeopleCategory;
use crate::theme::Tokens;
use crate::widgets::register;

use super::unnamed::{SuggestionGroup, Unnamed, UnnamedFace};

const GAP: f32 = 14.0;
pub(super) const PAD: f32 = 20.0;
const HEADER_H: f32 = 44.0;
/// The row of categories over the view.
const CATEGORY_H: f32 = 40.0;
/// The name under a card's picture.
const NAME_H: f32 = 26.0;
/// A category's heading: its title and count, the line under them, and the buttons beside them.
pub(super) const SECTION_H: f32 = 70.0;
/// The search box at the right of the header (points wide).
const SEARCH_W: f32 = 240.0;

/// `thumb` (the thumbnail slider, 90 to 480) as a position between `low` and `high`.
fn between(thumb: f32, low: f32, high: f32) -> f32 {
    let f = (thumb - 90.0) / (480.0 - 90.0);
    low + if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.4 } * (high - low)
}

/// A person card's face picture (points) for the thumbnail slider's value.
pub fn card_edge(thumb: f32) -> f32 {
    between(thumb, 84.0, 260.0)
}

/// A face tile's picture (points), for the faces no one is named, a person's page and the suggestions.
pub fn tile_edge(thumb: f32) -> f32 {
    between(thumb, 56.0, 180.0)
}

/// How many `edge`-wide cells (with the gap between them) fit across `width`.
pub(super) fn columns(width: f32, edge: f32) -> usize {
    (((width - PAD * 2.0 + GAP) / (edge + GAP)).floor() as usize).max(1)
}

pub(super) const TILE_GAP: f32 = GAP;

/// `text` shortened with "…" until it is no wider than `max_w` in `font` (measured, not guessed).
pub(super) fn fit(painter: &egui::Painter, text: &str, font: egui::FontId, max_w: f32) -> String {
    let width = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x;
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    (1..chars.len()).rev().map(|n| chars[..n].iter().collect::<String>() + "…").find(|s| width(s) <= max_w).unwrap_or_else(|| "…".to_string())
}

/// The People view: its categories' picker, then whichever category is on screen (issue #1050).
pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    if let Some(name) = app.ui.person_page.clone() {
        return super::person::show(app, ui, &name);
    }
    category_header(app, ui);
    match app.ui.people_category {
        PeopleCategory::Unknown => unknown(app, ui),
        PeopleCategory::Suggested => super::suggested::show(app, ui),
        PeopleCategory::Confirmed => confirmed(app, ui),
    }
}

/// The categories' picker over the view: the three of them, how many faces each holds, and which is on screen. A click
/// shows that category (each a `button:peopleCategory-<id>`).
fn category_header(app: &mut LccApp, ui: &mut egui::Ui) {
    let people = app.caches.people(&app.session.catalog, &app.session.people_filter());
    let counts = category_counts(&categories(app, ui.input(|i| i.time)), people.len());
    let labels: Vec<(String, String)> =
        [(PeopleCategory::Unknown, counts[0]), (PeopleCategory::Suggested, counts[1]), (PeopleCategory::Confirmed, counts[2])]
            .into_iter()
            .map(|(category, count)| (format!("{} ({count})", crate::i18n::tr(category.title())), category.id().to_string()))
            .collect();
    let items: Vec<(&str, &str)> = labels.iter().map(|(label, key)| (label.as_str(), key.as_str())).collect();
    let active = Some(match app.ui.people_category {
        PeopleCategory::Unknown => 0,
        PeopleCategory::Suggested => 1,
        PeopleCategory::Confirmed => 2,
    });
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), CATEGORY_H), Sense::hover());
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(row.shrink2(vec2(PAD, 6.0))));
    if let Some(i) = crate::widgets::segmented(&mut child, "peopleCategory", &items, active, 3) {
        app.ui.people_category = [PeopleCategory::Unknown, PeopleCategory::Suggested, PeopleCategory::Confirmed][i.min(2)];
        app.ui.person_page = None;
    }
}

/// The three categories' counts, for the picker: the unknown faces, the suggested faces (not the people they suggest),
/// and the people named (a person is one card in Confirmed, however many photos they are in).
pub fn category_counts(faces: &CategoryFaces, people: usize) -> [usize; 3] {
    [faces.unknown.len(), faces.suggested_faces().count(), people]
}

/// The unnamed faces of each category (issue #1050): one listing (`faces.unnamed {bySuggestion: true}`) split by what
/// the named faces suggest for each face, with the session's own decisions applied on top, so a confirmation or a
/// rejection shows at once rather than when the engine's next answer arrives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CategoryFaces {
    /// Faces with no name and no suggestion.
    pub unknown: Vec<UnnamedFace>,
    /// The suggested faces under the person suggested, the most faces first.
    pub suggested: Vec<SuggestionGroup>,
    /// The listing itself (how many unnamed faces there are, and whether recognition is on).
    pub list: Arc<Unnamed>,
}

impl CategoryFaces {
    /// Every suggested face, groups in order.
    pub fn suggested_faces(&self) -> impl Iterator<Item = &UnnamedFace> {
        self.suggested.iter().flat_map(|g| g.faces.iter())
    }
}

/// Split a `faces.unnamed {bySuggestion: true}` answer into the categories: a face confirmed in this session is named
/// now and gone from it, and a face whose suggestion was rejected is unknown again.
pub fn category_faces(
    list: &Unnamed,
    rejected: &std::collections::HashSet<(u64, usize)>,
    confirmed: &std::collections::HashSet<(u64, usize)>,
) -> CategoryFaces {
    let mut unknown = Vec::new();
    let mut suggested: Vec<SuggestionGroup> = Vec::new();
    for face in &list.faces {
        let key = (face.photo, face.index);
        if confirmed.contains(&key) {
            continue;
        }
        let name = face.suggestion.as_deref().map(str::trim).filter(|n| !n.is_empty()).filter(|_| !rejected.contains(&key));
        match name {
            Some(name) => match suggested.iter_mut().find(|g| g.name.eq_ignore_ascii_case(name)) {
                Some(group) => {
                    group.faces.push(face.clone());
                    group.total = group.faces.len();
                }
                None => suggested.push(SuggestionGroup { name: name.to_string(), total: 1, faces: vec![face.clone()] }),
            },
            None => unknown.push(face.clone()),
        }
    }
    // the most suggested faces first, then by name, as the engine orders the groups (a listing cached before a decision
    // must not move about)
    suggested.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    CategoryFaces { unknown, suggested, list: Arc::new(list.clone()) }
}

/// The view's categories now: the unnamed faces (asked for again only when they change) split up.
pub fn categories(app: &mut LccApp, now: f64) -> CategoryFaces {
    let list = super::unnamed::faces(app, now);
    category_faces(&list, &app.ui.rejected_suggestions, &app.ui.confirmed_suggestions)
}

/// A category's heading: its title and how many faces it holds, a line saying what it is for (or why it has none), and
/// a button at the right when it offers one.
pub(super) struct Heading<'a> {
    pub title: &'a str,
    pub count: usize,
    /// The line under the title.
    pub note: String,
    /// The button at the right of the heading: (label, widget id).
    pub action: Option<(String, String)>,
}

/// The width of a heading's button.
pub(super) const HEADING_BUTTON_W: f32 = 130.0;

/// Draws `h` at the top of a row `SECTION_H` tall: the title and, right of it, the button, on one line, with the count to
/// the button's left and the note under the title. `Some(id)` of the button when it was clicked.
pub(super) fn heading(ui: &mut egui::Ui, r: Rect, h: &Heading<'_>) -> Option<String> {
    let t = Tokens::get(ui.ctx());
    let at = pos2(r.left() + PAD, r.top() + 22.0);
    let right = r.right() - PAD;
    // the count stays clear of the button
    let count_x = if h.action.is_some() { right - HEADING_BUTTON_W - 12.0 } else { right };
    let p = ui.painter();
    p.text(at, Align2::LEFT_CENTER, crate::i18n::tr(h.title), t.semibold(15.0), t.text);
    p.text(pos2(count_x, at.y), Align2::RIGHT_CENTER, h.count.to_string(), t.font(13.0), t.text_dim);
    p.text(pos2(at.x, at.y + 24.0), Align2::LEFT_CENTER, &h.note, t.font(12.0), t.text_dim);
    let (label, id) = h.action.as_ref()?;
    let br = Rect::from_min_size(pos2(right - HEADING_BUTTON_W, at.y - 11.0), vec2(HEADING_BUTTON_W, 22.0));
    let b = ui.put(br, egui::Button::new(label.clone()));
    register(ui.ctx(), id.clone(), b.rect);
    b.clicked().then(|| id.clone())
}

/// The space a category's heading takes (`SECTION_H`).
pub(super) fn heading_row(ui: &mut egui::Ui) -> Rect {
    ui.allocate_exact_size(vec2(ui.available_width(), SECTION_H), Sense::hover()).0
}

/// The faces a grid draws, in the order it draws them: the tiles only (their pictures, a click selecting them), the way
/// the Unnamed faces section has always drawn them.
pub(super) fn face_grid(app: &mut LccApp, ui: &mut egui::Ui, list: &Unnamed, visible: &[UnnamedFace], top: f32, viewport: egui::Rect, ppp: f32) {
    let tile = tile_edge(app.ui.thumb_size);
    let width = ui.available_width();
    let cols = columns(width, tile);
    let rows = visible.len().div_ceil(cols);
    let row_h = tile + GAP;
    let (area, _) = ui.allocate_exact_size(vec2(width, top + rows as f32 * row_h + PAD), Sense::hover());
    let first = ((viewport.top() - top) / row_h).floor().max(0.0) as usize;
    let last = ((((viewport.bottom() - top) / row_h).ceil().max(0.0) as usize).min(rows)).max(first.min(rows));
    for row in first..last {
        for col in 0..cols {
            let Some(i) = row.checked_mul(cols).and_then(|n| n.checked_add(col)) else { break };
            if i >= visible.len() {
                break;
            }
            let min = area.min + vec2(PAD + col as f32 * row_h, top + row as f32 * row_h);
            super::unnamed::tile_at(app, ui, list, visible, i, Rect::from_min_size(min, vec2(tile, tile)), ppp);
        }
    }
    app.renderer.want_variants(visible.len().min(rows * cols));
}

/// The Confirmed category: a card per named person (their face, the number of photos they are in, their name), with the
/// search box at the right of the header narrowing them by name. A click opens that person's page.
fn confirmed(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let thumb = app.ui.thumb_size;
    // what the view shows: its source (folder, collection, Missing Photos…) and the filter bar (issue #1131)
    let scope = app.session.people_filter();
    let people = app.caches.people(&app.session.catalog, &scope);
    // the search box, at the right of the header: the cards below keep only the names that hold what is typed
    // (case aside); emptied, every person is back
    let query = app.ui.people_search.trim().to_lowercase();
    let listed: Vec<&Person> = people.iter().filter(|p| query.is_empty() || p.name.to_lowercase().contains(&query)).collect();
    let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::hover());
    let search = Rect::from_min_size(pos2(head.right() - PAD - SEARCH_W, head.center().y - 13.0), vec2(SEARCH_W, 26.0));
    let title_x = head.left() + PAD;
    let title = ui.painter().text(pos2(title_x, head.center().y), Align2::LEFT_CENTER, crate::i18n::tr("Named People"), t.semibold(15.0), t.text);
    // the person page last left (a face opened from it, or its back button): one click returns to it, as long as it
    // stays on the header's side of the search box
    if let Some(last) = app.ui.last_person.clone().filter(|n| people.iter().any(|p| &p.name == n)) {
        // its label plus the button's arrow and margins stay clear of the search box
        let room = (search.left() - 66.0 - (title.right() + 14.0)).clamp(48.0, 224.0);
        let label = fit(ui.painter(), &last, t.font(13.0), room);
        let width = ui.painter().layout_no_wrap(label.clone(), t.font(13.0), Color32::WHITE).size().x + 44.0;
        let r = Rect::from_min_size(pos2(title.right() + 14.0, head.center().y - 12.0), vec2(width, 24.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
        let b = crate::widgets::arrow_button(&mut child, RichText::new(label).font(t.font(13.0)), crate::icons::Icon::ChevronRight, false);
        register(ui.ctx(), "people:last", b.rect);
        if b.on_hover_text(crate::i18n::tr("Back to this person")).clicked() {
            let _ = app.run("view.person", json!({"name": last}));
        }
    }
    // how many cards the search lets through (of all of them, while it narrows them)
    let count_text = if query.is_empty() { people.len().to_string() } else { format!("{} / {}", listed.len(), people.len()) };
    let count = ui.painter().text(pos2(search.left() - 16.0, head.center().y), Align2::RIGHT_CENTER, count_text, t.font(13.0), t.text_dim);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(search));
    crate::text_field::TextField::singleline("field:peopleSearch", &mut app.ui.people_search)
        .hint(crate::i18n::tr("Search People"))
        .label(crate::i18n::tr("Search People"))
        .width(SEARCH_W - 12.0)
        .font(t.font(13.0))
        .margin(egui::Margin::symmetric(8, 4))
        .show(&mut child);
    // faces the user ignored: one click brings them all back (one undo step)
    let ignored = app.caches.ignored_faces(&app.session.catalog);
    if !ignored.is_empty() {
        let label = if ignored.len() == 1 {
            crate::i18n::tr_format!("Restore {} ignored face", ignored.len()).to_string()
        } else {
            crate::i18n::tr_format!("Restore {} ignored faces", ignored.len()).to_string()
        };
        let w = ui.painter().layout_no_wrap(label.clone(), t.font(12.5), Color32::WHITE).size().x + 20.0;
        let r = Rect::from_min_size(pos2(count.left() - 16.0 - w, head.center().y - 12.0), vec2(w, 24.0));
        let b = ui.put(r, egui::Button::new(RichText::new(label).font(t.font(12.5))));
        register(ui.ctx(), "people:restoreIgnored", b.rect);
        if b.clicked() {
            let list: Vec<serde_json::Value> = ignored.iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
            let _ = app.run("faces.ignore", json!({"faces": list, "ignored": false}));
        }
    }
    // until face recognition is set up: what it takes, and a button that does the next step
    super::faces::setup_banner(app, ui);
    // the filters narrowing the list (a date, a keyword…), removable here
    let chips = lcc_engine::filter_chips(&app.session.filter, &app.session.catalog);
    super::chips::show(app, ui, &chips);
    if people.is_empty() {
        // nothing to show: why, said in the UI's language
        if chips.is_empty() {
            super::empty_message(
                ui,
                ui.available_rect_before_wrap(),
                "No people yet",
                "Face names written to XMP by Lightroom and other apps show up here, and faces found in your photos can be named.",
            );
        } else {
            super::empty_message(
                ui,
                ui.available_rect_before_wrap(),
                "No named people in these photos",
                "Remove a filter above, or choose Clear all",
            );
        }
        return;
    }
    let ppp = ui.ctx().pixels_per_point();
    let active = app.session.filter.person.clone();
    let edge = card_edge(thumb);
    egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, viewport| {
        let width = ui.available_width();
        let cols = columns(width, edge);
        let row_h = edge + NAME_H + GAP;
        let rows = listed.len().div_ceil(cols);
        let (area, _) = ui.allocate_exact_size(vec2(width, PAD + rows as f32 * row_h + PAD), Sense::hover());
        let first = ((viewport.top() - PAD) / row_h).floor().max(0.0) as usize;
        let last = ((((viewport.bottom() - PAD) / row_h).ceil().max(0.0) as usize).min(rows)).max(first.min(rows));
        let mut shown = 0;
        for row in first..last {
            for col in 0..cols {
                let Some(&person) = listed.get(row * cols + col) else { break };
                shown += 1;
                let min = area.min + vec2(PAD + col as f32 * (edge + GAP), PAD + row as f32 * row_h);
                let selected = active.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&person.name));
                card(app, ui, person, Rect::from_min_size(min, vec2(edge, edge + NAME_H)), edge, ppp, selected);
            }
        }
        if listed.is_empty() {
            // the search box holds a name no one has
            ui.painter().text(
                pos2(area.left() + PAD, area.top() + 24.0),
                Align2::LEFT_CENTER,
                crate::i18n::tr("No people match. Clear the search box to see everyone again.").to_string(),
                t.font(13.0),
                t.text_dim,
            );
        }
        // the picture cache keeps all of them (a fixed budget would evict and re-request the same few every frame)
        app.renderer.want_variants(shown);
    });
}

/// The Unknown category: the faces nobody has named, which no name is suggested for, as cropped pictures, to be named a
/// group at a time (`unnamed::naming_bar`) or ignored. Once recognition matches a face to someone it moves to Suggested.
fn unknown(app: &mut LccApp, ui: &mut egui::Ui) {
    let now = ui.input(|i| i.time);
    let ppp = ui.ctx().pixels_per_point();
    // until face recognition is set up: what it takes, and a button that does the next step
    super::faces::setup_banner(app, ui);
    // the filters narrowing the list (a date, a keyword…), removable here
    let chips = lcc_engine::filter_chips(&app.session.filter, &app.session.catalog);
    super::chips::show(app, ui, &chips);
    // what is selected below, and the box to name it with
    super::unnamed::naming_bar(app, ui);
    let cats = categories(app, now);
    let faces = cats.unknown.clone();
    // what this category is (and why it is empty), in the UI's language
    let tr = crate::i18n::tr;
    let note = if !cats.list.ready && cats.list.faces.is_empty() {
        tr("Turn on face recognition in Settings ▸ Faces to find the faces in your photos.").to_string()
    } else if faces.is_empty() && cats.list.total > 0 {
        tr("No face here is unknown: the rest are named or suggested as someone (see Suggested).").to_string()
    } else if faces.is_empty() {
        tr("No unknown faces. Faces found in your photos appear here until you name them.").to_string()
    } else if cats.list.ordered {
        tr("No name and no suggestion. Faces that look alike are next to each other: click to select, Shift-click for a range, then type a name.")
            .to_string()
    } else {
        tr("No name and no suggestion. Click faces to select them, Shift-click for a range, then type a name.").to_string()
    };
    let row = heading_row(ui);
    if faces.is_empty() {
        heading(ui, row, &Heading { title: "Unknown faces", count: 0, note, action: None });
        super::empty_message(
            ui,
            ui.available_rect_before_wrap(),
            "No unknown faces",
            "The faces recognition matches to someone wait under Suggested, and the faces of the people you have named under Confirmed.",
        );
        return;
    }
    // Select all / Select none, for the faces this category lists
    let all = faces.iter().all(|f| app.ui.unnamed_selected.contains(&(f.photo, f.index)));
    let select = crate::i18n::tr(if all { "Select none" } else { "Select all" }).to_string();
    let clicked =
        heading(ui, row, &Heading { title: "Unknown faces", count: faces.len(), note, action: Some((select, "unnamed:selectAll".to_string())) });
    if clicked.is_some() {
        let keys: Vec<(u64, usize)> = faces.iter().map(|f| (f.photo, f.index)).collect();
        if all {
            for key in &keys {
                app.ui.unnamed_selected.remove(key);
            }
        } else {
            app.ui.unnamed_selected.extend(keys);
            app.ui.unnamed_focus = true;
        }
    }
    egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, viewport| {
        face_grid(app, ui, &cats.list, &faces, 0.0, viewport, ppp);
    });
}

/// A person's number of photos as the card shows it on their picture: in full (a library from Lightroom passes 1 000
/// quickly), so no line under the name has to repeat it.
fn badge(count: usize) -> String {
    count.to_string()
}

/// A face card: a picture, the count on it, the name under it, and the context menu and click that go with it.
#[allow(clippy::too_many_arguments)]
fn face_card(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    r: Rect,
    edge: f32,
    ppp: f32,
    photo: u64,
    face_rect: lcc_geom::Rect,
    name: &str,
    count: usize,
    tooltip: String,
    id: (&'static str, egui::Id),
    selected: bool,
) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let face = Rect::from_min_size(r.min, vec2(edge, edge));
    let resp = ui.interact(r, id.1, Sense::click());
    crate::widgets::announce(&resp, name.to_string());
    register(ui.ctx(), format!("{}:{}", id.0, name), r);
    let view = app.session.face_view(lcc_catalog::PhotoId(photo), face_rect);
    let p = ui.painter();
    p.rect_filled(face, 3.0, t.canvas);
    if let Some(job) = app.session.face_job(lcc_catalog::PhotoId(photo), view, (edge * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.id(), face, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // how many photos they are in, on the picture
    let g = p.layout_no_wrap(badge(count), t.font(11.5), Color32::WHITE);
    let pill = Rect::from_min_size(pos2(face.right() - g.size().x - 14.0, face.bottom() - g.size().y - 11.0), g.size() + vec2(10.0, 6.0));
    p.rect_filled(pill, 4.0, Color32::from_black_alpha(175));
    p.galley(pill.min + vec2(5.0, 3.0), g, Color32::WHITE);
    if selected {
        super::grid::selection_ring(p, &t, face, lcc_engine::SelectionState::Active);
    } else if resp.hovered() {
        p.rect_stroke(face, 3.0, Stroke::new(1.0, t.text_dim), StrokeKind::Outside);
    }
    // a long name must not run past the card
    let shown = fit(p, name, t.semibold(13.0), edge);
    p.text(pos2(r.left() + 2.0, face.bottom() + 14.0), Align2::LEFT_CENTER, shown, t.semibold(13.0), t.text);
    resp.on_hover_text(tooltip)
}

/// A person's card in the Confirmed category: their face, the number of photos they are in, their name. A click opens
/// their page; the context menu renames them.
pub(super) fn card(app: &mut LccApp, ui: &mut egui::Ui, person: &Person, r: Rect, edge: f32, ppp: f32, selected: bool) {
    let tooltip = crate::i18n::tr_format!("{}: show their photos", person.name);
    let resp = face_card(
        app,
        ui,
        r,
        edge,
        ppp,
        person.photo.0,
        person.face,
        &person.name,
        person.count,
        tooltip,
        ("person", egui::Id::new(("person-card", &person.name))),
        selected,
    );
    let full = person.name.clone();
    crate::widgets::context_menu(&resp, |ui| {
        let r = ui.button(crate::i18n::tr("Rename Person…"));
        register(ui.ctx(), "personMenu:rename", r.rect);
        if r.clicked() {
            open_rename(app, &full);
        }
    });
    if resp.clicked() {
        let _ = app.run("view.person", json!({"name": person.name}));
    }
}

/// The Rename Person dialog, prefilled with the name the card shows.
pub(crate) fn open_rename(app: &mut LccApp, name: &str) {
    app.ui.dialog = Some(crate::state::Dialog::RenamePerson { from: name.to_string(), to: name.to_string(), merge: false });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(photo: u64, index: usize, suggestion: Option<&str>) -> UnnamedFace {
        UnnamedFace {
            photo,
            index,
            rect: lcc_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.3, y1: 0.4 },
            view: lcc_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.3, y1: 0.4 },
            suggestion: suggestion.map(str::to_string),
            group: None,
        }
    }

    fn listing(faces: Vec<UnnamedFace>) -> Unnamed {
        Unnamed { total: faces.len(), faces, ordered: true, ready: true, groups: Vec::new(), rejected: 0 }
    }

    #[test]
    fn faces_follow_the_thumbnail_slider_within_their_own_limits() {
        assert_eq!((card_edge(90.0), card_edge(480.0)), (84.0, 260.0));
        assert_eq!((tile_edge(90.0), tile_edge(480.0)), (56.0, 180.0));
        // between, growing with the slider; beyond its ends, held at the limits; nonsense, a middle size
        assert!(card_edge(220.0) > card_edge(150.0) && tile_edge(300.0) > tile_edge(220.0));
        assert_eq!((card_edge(-5.0), card_edge(10_000.0)), (84.0, 260.0));
        assert!(card_edge(f32::NAN) > 84.0 && card_edge(f32::NAN) < 260.0);
        // there is always at least one column, whatever the width
        assert_eq!((columns(0.0, 260.0), columns(-50.0, 260.0), columns(f32::NAN, 100.0)), (1, 1, 1));
        assert!(columns(1400.0, 84.0) > columns(1400.0, 260.0));
    }

    /// #3251: the number on a card's picture is the person's full count, past 999 too ("999+" said less than the line
    /// under the name, now gone).
    #[test]
    fn a_cards_badge_shows_the_full_count() {
        assert_eq!(
            (badge(1), badge(999), badge(1000), badge(6110), badge(123_456)),
            ("1".into(), "999".into(), "1000".into(), "6110".into(), "123456".into())
        );
    }

    /// Issue #1050: the unnamed faces split into the categories the named faces suggest, the most suggested faces
    /// first; a face with no suggestion is unknown, and what was decided this session is applied at once.
    #[test]
    fn unnamed_faces_split_into_unknown_and_the_people_suggested_for_them() {
        let mut l = listing(vec![
            face(1, 0, None),
            face(1, 1, Some("Jane Doe")),
            face(2, 0, Some("Jane Doe")),
            face(3, 0, Some("john roe")),
            face(4, 0, Some("  ")),
        ]);
        l.total = 9;
        let cats = category_faces(&l, &Default::default(), &Default::default());
        assert_eq!((cats.unknown.len(), cats.suggested.len(), cats.list.total), (2, 2, 9), "a blank suggestion is no suggestion");
        assert_eq!(cats.suggested[0].name, "Jane Doe");
        assert_eq!((cats.suggested[0].total, cats.suggested[0].faces.len()), (2, 2), "a group says how many faces it holds");
        assert_eq!(cats.suggested[1].name, "john roe");
        assert_eq!(cats.suggested_faces().count(), 3);
        // a confirmation this session leaves the list at once, whichever group it was in
        let confirmed: std::collections::HashSet<(u64, usize)> = [(2, 0)].into_iter().collect();
        let cats = category_faces(&l, &Default::default(), &confirmed);
        assert_eq!((cats.unknown.len(), cats.suggested.len(), cats.suggested_faces().count()), (2, 2, 2));
        assert!(cats.suggested.iter().all(|g| !g.faces.iter().any(|f| (f.photo, f.index) == (2, 0))));
        // a rejection this session takes the face out of the suggestion straight away (the engine's next answer says so
        // too, once `people.json` has it)
        let rejected: std::collections::HashSet<(u64, usize)> = [(3, 0)].into_iter().collect();
        let cats = category_faces(&l, &rejected, &Default::default());
        assert_eq!((cats.unknown.len(), cats.suggested.len(), cats.suggested_faces().count()), (3, 1, 2), "{cats:?}");
        assert!(cats.unknown.iter().any(|f| (f.photo, f.index) == (3, 0)));
        // every face gone: nothing left to show
        let all: std::collections::HashSet<(u64, usize)> = l.faces.iter().map(|f| (f.photo, f.index)).collect();
        let cats = category_faces(&l, &Default::default(), &all);
        assert!(cats.unknown.is_empty() && cats.suggested.is_empty());
        // counts are the categories' faces and the people named (a person is a card, not a face)
        assert_eq!(category_counts(&cats, 12), [0, 0, 12]);
    }

    /// Issue #1050: the category row names the three categories, in the UI's language, with their counts; the ids the
    /// control channel and the widget names use are the ones `PeopleCategory` parses.
    #[test]
    fn every_category_has_a_name_a_count_and_an_id_a_script_can_ask_for() {
        let names = [PeopleCategory::Unknown, PeopleCategory::Suggested, PeopleCategory::Confirmed];
        for category in names {
            assert_eq!(PeopleCategory::parse(category.id()), Some(category), "{}", category.id());
            assert!(!crate::i18n::tr(category.title()).is_empty(), "{}", category.id());
            for language in crate::i18n::Locale::ALL.iter().filter(|l| **l != crate::i18n::Locale::En) {
                assert_ne!(language.tr(category.title()), category.title(), "{} in {}", category.id(), language.code());
            }
        }
        // a name a script makes up is no category (it would show nothing and leave the view where it was)
        assert_eq!(PeopleCategory::parse(""), None);
        assert_eq!(PeopleCategory::parse("Unknown"), None, "the ids, not the titles");
        assert_eq!(PeopleCategory::parse("nobody"), None);
        // the counts are one number per category, whatever the listing holds
        let l = listing(vec![face(1, 0, None), face(1, 1, Some("Jane Doe"))]);
        assert_eq!(category_counts(&category_faces(&l, &Default::default(), &Default::default()), 7), [1, 1, 7]);
        let confirmed: std::collections::HashSet<(u64, usize)> = [(1, 0)].into_iter().collect();
        assert_eq!(category_counts(&category_faces(&l, &Default::default(), &confirmed), 0), [0, 1, 0], "the confirmed face is named by now");
    }

    /// Issue #1050: a grid puts every face in its own cell, whatever the width or the count, and never runs outside it.
    #[test]
    fn a_grid_puts_every_face_in_its_own_cell() {
        for width in [0.0, 300.0, 1400.0, f32::NAN] {
            let cols = columns(width, 100.0);
            assert!(cols >= 1);
            for n in [0usize, 1, 7, 50] {
                let rows = n.div_ceil(cols);
                assert!(rows * cols >= n, "every face has a cell: {n} faces, {cols} x {rows}");
            }
        }
    }
}
