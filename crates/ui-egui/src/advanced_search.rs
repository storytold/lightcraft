//! The Advanced Search panel (issue #846): a dedicated dialog with granular structured field
//! filtering, organised in collapsible categories. It edits a draft ([`Search`]); Search runs
//! `library.advancedSearch`, whose results the grid shows as a filtered view. Categories the
//! catalog cannot support yet (audio/video codec and frame rate) show present but disabled, and
//! a refused query (a typo in a number, an unsupported condition) is an error toast, never a
//! crash and never a silently empty grid.

use egui::RichText;

use crate::dialog_frame;
use crate::i18n::tr;
use crate::{LccApp, text_field};

/// One numeric range of the panel: its two ends as the user is typing them. Numbers stay text
/// until Search, so a half-typed value survives the frame; they are parsed and named at Search
/// time ([`draft_query`]).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NumEdits {
    pub width: [String; 2],
    pub height: [String; 2],
    pub megapixels: [String; 2],
    pub focal: [String; 2],
    pub iso: [String; 2],
    pub aperture: [String; 2],
    pub duration: [String; 2],
    /// Shutter speed in camera notation (`1/250`) or seconds — the query keeps it as text too.
    pub shutter: [String; 2],
    /// Keywords, comma separated, split into the list at Search time.
    pub keywords: String,
    /// People, comma separated, split into the list at Search time.
    pub people: String,
    /// The face-count condition as typed (a count, `3+`, `<=4`, `1-5`, `none`, `any`).
    pub face_count: String,
}

/// The Advanced Search panel's draft: the structured query's text and choice fields, plus the
/// numeric ranges and comma lists as they are being typed.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Search {
    pub query: Box<lcc_catalog::advanced_search::AdvancedSearch>,
    pub num: NumEdits,
    /// Which collection the File category names (`None` = any).
    pub album: Option<u64>,
}

/// A list written as one comma-separated line, split into its entries.
fn split_list(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// One numeric range end as typed (`""` = no bound), or an error naming the field.
fn range_end(field: &str, text: &str) -> Result<Option<f64>, String> {
    let s = text.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite()).map(Some).ok_or_else(|| format!("{}: {s:?} is not a number", tr(field)))
}

/// One numeric range, or an error naming the field; a reversed range is an error too.
fn num_range(field: &str, edits: &[String; 2]) -> Result<lcc_catalog::advanced_search::Range, String> {
    let (min, max) = (range_end(field, &edits[0])?, range_end(field, &edits[1])?);
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(format!("{}: the minimum is above the maximum", tr(field)));
    }
    Ok(lcc_catalog::advanced_search::Range { min, max })
}

/// Search clicked: the draft as the query `library.advancedSearch` runs, or the first problem
/// with it (a half-typed number, a reversed range) as an error naming the field.
pub fn draft_query(search: &Search) -> Result<lcc_catalog::advanced_search::AdvancedSearch, String> {
    let mut q = (*search.query).clone();
    let num = &search.num;
    if let Some(file) = q.file.as_mut() {
        file.album = search.album.map(lcc_catalog::AlbumId);
        file.keywords = split_list(&num.keywords);
    }
    let props = q.properties.get_or_insert_with(Default::default);
    props.width = num_range("Width (pixels)", &num.width)?;
    props.height = num_range("Height (pixels)", &num.height)?;
    props.megapixels = num_range("Megapixels", &num.megapixels)?;
    let photo = q.photograph.get_or_insert_with(Default::default);
    photo.focal_mm = num_range("Focal length (mm)", &num.focal)?;
    photo.iso = num_range("ISO speed", &num.iso)?;
    photo.aperture = num_range("Aperture (f-number)", &num.aperture)?;
    let trim = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
    photo.shutter = lcc_catalog::advanced_search::ShutterRange { min: trim(&num.shutter[0]), max: trim(&num.shutter[1]) };
    let media = q.media.get_or_insert_with(Default::default);
    media.duration_s = num_range("Duration (seconds)", &num.duration)?;
    if let Some(faces) = q.faces.as_mut() {
        faces.people = split_list(&num.people);
        faces.count = (!num.face_count.trim().is_empty()).then(|| num.face_count.trim().to_string());
    }
    // validated here (not only in the command), so the panel stays open on the problem
    q.to_filter().map(|_| q)
}

/// Where a category's open/closed state lives: a global id, not one derived from the parent
/// `Ui`. The dialog body switches between a plain scope and a scroll area as its height crosses
/// the room left (`ModalLayout::body`), which changes every parent-derived id; egui's own
/// `CollapsingHeader` state was lost on each switch and fell back to "open because the category
/// exists", so an expanded section could not be collapsed again (#1134).
fn section_state_id(id: &str) -> egui::Id {
    egui::Id::new(("advanced-search-section", id))
}

/// Whether category `id` is expanded; `default` until the user toggles it.
fn section_open(ctx: &egui::Context, id: &str, default: bool) -> bool {
    ctx.data(|d| d.get_temp::<bool>(section_state_id(id))).unwrap_or(default)
}

fn set_section_open(ctx: &egui::Context, id: &str, open: bool) {
    ctx.data_mut(|d| d.insert_temp(section_state_id(id), open));
}

/// A collapsible rule category: a `CollapsingHeader` whose content creates the category on first
/// open (an empty category constrains nothing, so leaving one expanded is harmless). Its open
/// state is kept under [`section_state_id`] so it survives the body's layout switches.
fn section<T: Default>(ui: &mut egui::Ui, id: &'static str, title: &str, value: &mut Option<T>, add: impl FnOnce(&mut egui::Ui, &mut T)) {
    let open = section_open(ui.ctx(), id, value.is_some());
    let shown = egui::CollapsingHeader::new(RichText::new(tr(title)).strong()).id_salt(id).open(Some(open)).show(ui, |ui| {
        ui.add_space(2.0);
        add(ui, value.get_or_insert_with(T::default));
    });
    let open = if shown.header_response.clicked() { !open } else { open };
    set_section_open(ui.ctx(), id, open);
}

/// The dialog's body: the global words field and one collapsible category per rule group.
pub fn body(ui: &mut egui::Ui, search: &mut Search, app: &LccApp) {
    let q = &mut *search.query;
    let num = &mut search.num;
    let album = &mut search.album;

    ui.label(RichText::new(tr("Find items that have associated all these words:")));
    text_row(ui, "field:asWords", "", &mut q.words, "a word per topic; every word must match");
    ui.add_space(4.0);

    section(ui, "asFile", "File, Collection & Keywords", &mut q.file, |ui, f| {
        choice(
            ui,
            "field:asKind",
            "File type",
            &mut f.kind,
            &[(None, "Any type"), (Some("image"), "Image"), (Some("raw"), "Raw"), (Some("video"), "Video")],
        );
        text_row(ui, "field:asName", "File name contains", &mut f.name, "e.g. img_");
        text_row(ui, "field:asFolder", "Folder path contains", &mut f.folder, "e.g. /photos/2026");
        album_row(ui, album, app);
        op_row(ui, "Keywords", &mut f.keyword_op);
        text_row(ui, "field:asKeywords", "Keywords (comma separated)", &mut num.keywords, "e.g. travel, italy");
    });

    section(ui, "asProps", "Item Properties", &mut q.properties, |ui, p| {
        range_row(ui, "asWidth", "Width (pixels)", &mut num.width);
        range_row(ui, "asHeight", "Height (pixels)", &mut num.height);
        range_row(ui, "asMegapixels", "Megapixels", &mut num.megapixels);
        rating_row(ui, p);
        choice(
            ui,
            "field:asLabel",
            "Color label",
            &mut p.label,
            &[
                (None, "Any label"),
                (Some("red"), "Red"),
                (Some("yellow"), "Yellow"),
                (Some("green"), "Green"),
                (Some("blue"), "Blue"),
                (Some("purple"), "Purple"),
                (Some("none"), "No Label"),
            ],
        );
        choice(
            ui,
            "field:asAspect",
            "Aspect",
            &mut p.aspect,
            &[(None, "Any aspect"), (Some("landscape"), "Landscape (wide)"), (Some("portrait"), "Portrait (tall)"), (Some("square"), "Square")],
        );
    });

    section(ui, "asDesc", "Caption & Title", &mut q.description, |ui, d| {
        text_row(ui, "field:asTitle", "Title contains", &mut d.title, "");
        text_row(ui, "field:asCaption", "Caption contains", &mut d.caption, "");
    });

    section(ui, "asPhoto", "Photograph Information", &mut q.photograph, |ui, p| {
        text_row(ui, "field:asCamera", "Camera make/model contains", &mut p.camera, "e.g. canon");
        text_row(ui, "field:asLens", "Lens contains", &mut p.lens, "e.g. 85mm");
        range_row(ui, "asFocal", "Focal length (mm)", &mut num.focal);
        range_row(ui, "asAperture", "Aperture (f-number)", &mut num.aperture);
        range_row(ui, "asShutter", "Shutter speed (e.g. 1/250)", &mut num.shutter);
        range_row(ui, "asIso", "ISO speed", &mut num.iso);
    });

    section(ui, "asLocation", "Location", &mut q.location, |ui, l| {
        text_row(ui, "field:asLocation", "Location contains", &mut l.location, "");
        text_row(ui, "field:asSublocation", "Sublocation contains", &mut l.sublocation, "");
        text_row(ui, "field:asCity", "City contains", &mut l.city, "");
        text_row(ui, "field:asState", "State / Province contains", &mut l.state, "");
        text_row(ui, "field:asCountry", "Country contains", &mut l.country, "");
        gps_row(ui, l);
    });

    section(ui, "asMedia", "Audio/Video Properties", &mut q.media, |ui, _m| {
        range_row(ui, "asDuration", "Duration (seconds)", &mut num.duration);
        unsupported_row(ui, "Codec");
        unsupported_row(ui, "Frame rate");
        dialog_frame::help(ui, tr("Filtering by codec or frame rate is not supported yet."));
    });

    section(ui, "asFaces", "Faces", &mut q.faces, |ui, f| {
        text_row(ui, "field:asFaceCount", "Face count", &mut num.face_count, "e.g. 2, 3+, <=4, 1-5, none, any");
        op_row(ui, "People", &mut f.people_op);
        text_row(ui, "field:asPeople", "People (comma separated)", &mut num.people, "e.g. Jane, John");
    });
}

/// A text row: the label in a form's label column, the field in the control column.
fn text_row(ui: &mut egui::Ui, id: &str, label: &str, value: &mut String, hint: &str) {
    dialog_frame::form_row(ui, tr(label), |ui| {
        text_field::TextField::singleline(id, value).hint(tr(hint)).width(f32::INFINITY).show(ui);
    });
}

/// A numeric range: two small fields, from and to, either left blank for "no bound".
fn range_row(ui: &mut egui::Ui, id: &str, label: &str, edits: &mut [String; 2]) {
    dialog_frame::form_row(ui, tr(label), |ui| {
        ui.horizontal(|ui| {
            text_field::TextField::singleline(&format!("{id}Min"), &mut edits[0]).hint(tr("from")).width(90.0).show(ui);
            ui.label(tr("–"));
            text_field::TextField::singleline(&format!("{id}Max"), &mut edits[1]).hint(tr("to")).width(90.0).show(ui);
        });
    });
}

fn rating_row(ui: &mut egui::Ui, p: &mut lcc_catalog::advanced_search::PropertiesCategory) {
    dialog_frame::form_row(ui, tr("Rating"), |ui| {
        ui.horizontal(|ui| {
            let mut on = p.rating.is_some();
            if dialog_frame::checkbox(ui, &mut on, tr("Rated")).changed() {
                p.rating = if on { Some(3) } else { None };
            }
            if p.rating.is_some() {
                let mut r = p.rating.unwrap_or(3);
                if ui.add(egui::Slider::new(&mut r, 0..=5)).changed() {
                    p.rating = Some(r);
                }
                ui.radio_value(&mut p.rating_op, lcc_catalog::RatingOp::AtLeast, tr("at least"));
                ui.radio_value(&mut p.rating_op, lcc_catalog::RatingOp::Exactly, tr("exactly"));
                ui.radio_value(&mut p.rating_op, lcc_catalog::RatingOp::AtMost, tr("at most"));
            }
        });
    });
}

fn gps_row(ui: &mut egui::Ui, l: &mut lcc_catalog::advanced_search::LocationCategory) {
    dialog_frame::form_row(ui, tr("GPS coordinates"), |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut l.has_gps, None, tr("Any"));
            ui.radio_value(&mut l.has_gps, Some(true), tr("Has GPS"));
            ui.radio_value(&mut l.has_gps, Some(false), tr("No GPS"));
        });
    });
}

/// A choice drawn as a combo box of `(stored value, shown label)` pairs; the `None` row clears
/// the choice.
fn choice(ui: &mut egui::Ui, id: &str, label: &str, value: &mut Option<String>, options: &[(Option<&'static str>, &'static str)]) {
    dialog_frame::form_row(ui, tr(label), |ui| {
        let shown = value
            .as_deref()
            .and_then(|v| options.iter().find(|(k, _)| Some(v) == *k).map(|(_, l)| tr(l)))
            .unwrap_or_else(|| options.first().map(|(_, l)| tr(l)).unwrap_or(""));
        egui::ComboBox::from_id_salt(id).width(180.0).selected_text(shown).show_ui(ui, |ui| {
            for (k, l) in options {
                ui.selectable_value(value, k.map(str::to_string), tr(l));
            }
        });
    });
}

fn album_row(ui: &mut egui::Ui, album: &mut Option<u64>, app: &LccApp) {
    let mut albums: Vec<(String, u64)> = app.session.catalog.albums().filter(|a| !a.folder).map(|a| (a.name.clone(), a.id.0)).collect();
    albums.sort_by_key(|a| a.0.to_lowercase());
    dialog_frame::form_row(ui, tr("Collection"), |ui| {
        let shown =
            album.and_then(|id| albums.iter().find(|(_, a)| *a == id).map(|(n, _)| n.clone())).unwrap_or_else(|| tr("Any collection").to_string());
        egui::ComboBox::from_id_salt("field:asAlbum").width(220.0).selected_text(shown).show_ui(ui, |ui| {
            ui.selectable_value(album, None, tr("Any collection"));
            for (name, id) in &albums {
                ui.selectable_value(album, Some(*id), name.clone());
            }
        });
    });
}

/// How a category's list condition combines: `any` (at least one) or `all` (every one).
fn op_row(ui: &mut egui::Ui, label: &str, value: &mut lcc_catalog::MatchOp) {
    dialog_frame::form_row(ui, tr(label), |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(value, lcc_catalog::MatchOp::Any, tr("any of"));
            ui.radio_value(value, lcc_catalog::MatchOp::All, tr("all of"));
        });
    });
}

/// A condition the catalog cannot support yet: present, with its input greyed out.
fn unsupported_row(ui: &mut egui::Ui, label: &str) {
    dialog_frame::form_row(ui, tr(label), |ui| {
        ui.label(RichText::new(tr("not supported yet")).weak());
    });
}

#[cfg(test)]
mod section_tests {
    use super::*;

    /// Draws one category, either directly or inside a scroll area (the two layouts the dialog
    /// body switches between), and reports whether its content was shown.
    fn frame(ctx: &egui::Context, scrolled: bool, value: &mut Option<u8>) -> bool {
        let mut shown = false;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut draw = |ui: &mut egui::Ui| section(ui, "asTest", "Test", value, |_, _| shown = true);
            if scrolled {
                egui::ScrollArea::vertical().id_salt("body").show(ui, |ui| draw(ui));
            } else {
                ui.scope(|ui| draw(ui));
            }
        });
        out.textures_delta.clear();
        shown
    }

    #[test]
    fn collapsed_section_stays_collapsed_across_layout_switches() {
        let ctx = egui::Context::default();
        let mut value = None;
        assert!(!frame(&ctx, false, &mut value), "a missing category starts collapsed");
        set_section_open(&ctx, "asTest", true);
        assert!(frame(&ctx, false, &mut value), "expanding shows the category");
        assert!(value.is_some(), "the category exists once expanded");
        // the user collapses it; the body then flips between a scope and a scroll area
        set_section_open(&ctx, "asTest", false);
        for scrolled in [true, false, true, false] {
            assert!(!frame(&ctx, scrolled, &mut value), "a collapsed category reopened after a layout switch");
        }
        set_section_open(&ctx, "asTest", true);
        assert!(frame(&ctx, true, &mut value), "re-expanding still works");
    }
}
