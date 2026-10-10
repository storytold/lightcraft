//! The Keywording box (the top of the right panel's Keywords): the keywords of the selected photos,
//! not only the active one's, as Lightroom Classic shows them. A keyword only some of the selected
//! photos have is marked as such.

use dac_catalog::{Catalog, PhotoId};
use serde_json::json;

use crate::DacApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// The Keywording box's view: the keywords (chips), them with the keywords containing them, or
/// what exported files will carry.
pub fn view_switch(app: &mut DacApp, ui: &mut egui::Ui) {
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
pub fn names_row(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let selection = app.session.targets(&serde_json::Value::Null);
    let export = app.ui.keywording_view == crate::state::KeywordingView::WillExport;
    let names = app.caches.keyword_names(&app.session.catalog, &selection, export);
    if names.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr(if export { "No keywords are exported" } else { "No keywords yet" })).color(t.text_dim));
        return;
    }
    let prefix = if export { "keywordExport" } else { "keywordContaining" };
    ui.horizontal_wrapped(|ui| {
        for n in names.iter() {
            let (body, _) = chip(ui, &n.path, !n.on_all(), false);
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

/// One chip, measured before it is placed so that a wrapping row moves it whole to the next line:
/// the name (cut short to fit the row) and, with `cross`, a × at its end. A `partial` one (only
/// some of the selected photos) is dimmed and marked with an asterisk. Returns the name's response
/// and the ×'s.
fn chip(ui: &mut egui::Ui, path: &str, partial: bool, cross: bool) -> (egui::Response, Option<egui::Response>) {
    use egui::{Align2, Rect, Sense, pos2, vec2};
    let t = Tokens::get(ui.ctx());
    let font = t.font(13.0);
    let color = if partial { t.text_dim } else { t.text_label };
    let cross_w = if cross { 18.0 } else { 0.0 };
    // at most the row's width
    let room = (ui.max_rect().width() - 16.0 - cross_w).max(24.0);
    let measure = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), color).size().x;
    let shown = chip_label(path, partial, room, measure);
    let w = measure(&shown) + 16.0 + cross_w;
    // the row places it; its name and × answer under ids of their own keyword, so an open menu
    // stays with its keyword when the chips change
    let (rect, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
    let id = egui::Id::new(("keyword-chip", path.to_lowercase(), cross));
    let body = Rect::from_min_max(rect.min, pos2(rect.right() - cross_w, rect.bottom()));
    let full = path.replace('|', " › ");
    let resp = ui.interact(body, id, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &full));
    let x = cross.then(|| {
        let xr = Rect::from_center_size(pos2(rect.right() - 11.0, rect.center().y), vec2(14.0, 14.0));
        let xresp = ui.interact(xr, id.with("remove"), Sense::click());
        let said = crate::i18n::tr_format!("Remove “{name}”", name = full);
        xresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &said));
        xresp
    });
    ui.painter().rect_stroke(rect, 10.0, egui::Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
    if resp.hovered() || x.as_ref().is_some_and(egui::Response::hovered) {
        ui.painter().rect_filled(rect.shrink(1.0), 10.0, t.hover.gamma_multiply(0.5));
    }
    ui.painter().text(pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, &shown, font.clone(), color);
    if let Some(x) = &x {
        ui.painter().text(x.rect.center(), Align2::CENTER_CENTER, "×", t.font(13.0), if x.hovered() { t.text } else { t.text_dim });
    }
    (resp, x)
}

/// A chip's menu: it acts on the selected photos (deleting a keyword from the whole library is the
/// Keyword List's).
fn menu(app: &mut DacApp, ui: &mut egui::Ui, chip: &Chip) {
    let item = |ui: &mut egui::Ui, id: &str, label: &str| {
        let r = ui.button(label);
        register(ui.ctx(), format!("keywordChipMenu:{id}"), r.rect);
        r.clicked()
    };
    if !chip.on_all() && item(ui, "add", crate::i18n::tr("Add to All Selected Photos")) {
        let _ = app.run("photo.setMeta", json!({"addKeywords": [chip.path]}));
    }
    if item(ui, "remove", crate::i18n::tr("Remove from Selected Photos")) {
        let _ = app.run("photo.setMeta", json!({"removeKeywords": [chip.path]}));
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

/// The selection's keywords as chips: the name (right-click for its menu), and × to take it off
/// every selected photo. One only some of them have is marked with an asterisk.
pub fn chip_row(app: &mut DacApp, ui: &mut egui::Ui) {
    let selection = app.session.targets(&serde_json::Value::Null);
    let chips = app.caches.keyword_chips(&app.session.catalog, &selection);
    ui.horizontal_wrapped(|ui| {
        for c in chips.iter() {
            let (body, x) = chip(ui, &c.path, !c.on_all(), true);
            register(ui.ctx(), format!("keywordChip:{}", c.path), body.rect);
            if let Some(x) = x {
                register(ui.ctx(), format!("keywordChipRemove:{}", c.path), x.rect);
                if x.on_hover_text(crate::i18n::tr("Remove from Selected Photos")).clicked() {
                    let _ = app.run("photo.setMeta", json!({"removeKeywords": [c.path]}));
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
            body.context_menu(|ui| menu(app, ui, &c));
        }
    });
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
    use dac_catalog::keywords::{SEP, clean};
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
pub(crate) fn distinct<'a>(catalog: &'a Catalog, photos: &[PhotoId]) -> Vec<&'a dac_catalog::Photo> {
    let mut seen = std::collections::HashSet::new();
    photos.iter().filter(|id| seen.insert(**id)).filter_map(|id| catalog.photo(*id).map(|p| &**p)).collect()
}

/// The keywords of the selected `photos`, each once whatever its case, with how many of them have
/// it; by name.
pub(crate) fn chips(catalog: &Catalog, photos: &[PhotoId]) -> Vec<Chip> {
    use dac_catalog::keywords::clean;
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
    use dac_catalog::{Op, Photo, Source};

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
        use dac_catalog::keywords::KeywordInfo;
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
        use dac_catalog::keywords::KeywordInfo;
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
                let _ = chip(ui, "Places|Portugal|Lisbon|Belém tower", true, true);
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

    /// "Keywords & Containing Keywords": every level of the selection's keywords, flat, each
    /// counted once per photo.
    #[test]
    fn containing_keywords_are_every_level() {
        let (c, ids) = library(&[&["travel|Italy|Rome", "travel|Spain"], &["travel|Italy"]]);
        assert_eq!(shown(&with_containing(&c, &ids)), [("Italy", 2, 2), ("Rome", 1, 2), ("Spain", 1, 2), ("travel", 2, 2)]);
    }
}
