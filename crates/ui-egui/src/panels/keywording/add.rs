//! SDK add-keyword field over the existing selection-scoped metadata transaction.

use craft::ui::contract::{Bind, Props as _, Show as _, Sizing};
use craft::ui::runtime::focus::FocusTarget;
use craft::ui::text::TextField;
use egui::Rect;
use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::LightcraftApp;

#[derive(Clone, Default)]
struct Owner {
    library: u64,
    photos: Vec<PhotoId>,
    pass: Option<u64>,
    epoch: u64,
}

impl Owner {
    fn prepare(&mut self, library: u64, photos: &[PhotoId], pass: u64) -> (bool, bool) {
        let mut photos = photos.to_vec();
        photos.sort_unstable();
        photos.dedup();
        let changed = self.library != library || self.photos != photos;
        let interrupted = self.pass.is_none_or(|p| p != pass && p.checked_add(1) != Some(pass));
        let restart = changed || interrupted;
        if restart {
            self.epoch = self.epoch.wrapping_add(1);
        }
        self.library = library;
        self.photos = photos;
        self.pass = Some(pass);
        (changed, restart)
    }

    fn field(&self) -> egui::Id {
        crate::text_field::id("field:keyword").with(self.epoch)
    }
}

/// Whether the currently drawn SDK add-keyword field owns keyboard focus.
pub(crate) fn field_focused(ctx: &egui::Context) -> bool {
    let owner = ctx.data(|d| d.get_temp::<Owner>(egui::Id::unique("kw-input").with("owner")));
    owner.is_some_and(|owner| focused(ctx, owner.field()))
}

fn focused(ctx: &egui::Context, field: egui::Id) -> bool {
    craft::ui::runtime::focus::focused_rec(ctx).is_some_and(|rec| rec.stop.id == field)
}

/// The painter is a real sibling SDK stop, so Tab can leave the add field's typing run.
/// Its text remains an app preference; only Enter asks the existing Paint command to run.
pub(crate) fn painter_field(ui: &mut egui::Ui, text: &mut String) -> bool {
    let id = crate::text_field::id("field:keywordPainter");
    let (rect, _) = ui.allocate_exact_size(egui::vec2(130.0, 24.0), egui::Sense::hover());
    let (shown, record) = craft::ui::runtime::inspect::record(ui, id.with("inspect"), |ui| {
        TextField::new(crate::i18n::tr("Keyword to paint"), Bind::bound(text))
            .id(id)
            .placeholder(crate::i18n::tr("Keyword to paint"))
            .limit(None)
            .select_on_focus()
            .submits()
            .show_at(ui, rect)
    });
    crate::text_field::register_field(ui, "field:keywordPainter", &shown.response, record.as_deref());
    if (shown.submitted || shown.escaped)
        && let Some(stop) = craft::ui::runtime::focus::focused_rec(ui.ctx()).filter(|rec| rec.stop.id == id)
    {
        ui.memory_mut(|m| m.surrender_focus(stop.stop.root));
    }
    shown.submitted
}

/// Returns the selection's common keywords, also used by the unchanged suggestions below.
pub(crate) fn add_field(app: &mut LightcraftApp, ui: &mut egui::Ui, selection: &[PhotoId]) -> Vec<String> {
    let have: Vec<String> = app.caches.keyword_chips(&app.session.catalog, selection).iter().filter(|c| c.on_all()).map(|c| c.path.clone()).collect();
    if craft::ui::runtime::measure::active(ui.ctx()) {
        // A sizing run paints only the SDK field; it must not consume focus requests or drafts.
        let text: String = ui.data(|d| d.get_temp(egui::Id::unique("kw-input"))).unwrap_or_default();
        TextField::new(crate::i18n::tr("Add keyword"), Bind::given(text))
            .placeholder(crate::i18n::tr("Add keyword"))
            .limit(None)
            .submits()
            .sizing(Sizing::FillWidth)
            .show(ui);
        return have;
    }
    let kid = egui::Id::unique("kw-input");
    let mut text = ui.data_mut(|d| d.get_temp::<String>(kid).unwrap_or_default());
    let owner_key = kid.with("owner");
    let mut owner: Owner = ui.data(|d| d.get_temp(owner_key)).unwrap_or_default();
    let (changed, restart) = owner.prepare(app.session.library_generation(), selection, ui.ctx().cumulative_pass_nr());
    if changed {
        // Never retarget an unfinished draft to newly selected photos or a different library.
        text.clear();
    }
    if restart {
        // Same-owner reopening keeps the typed text, but never revives an old offer or focus.
        ui.data_mut(|d| {
            d.remove::<(String, Option<String>, Rect, u64, bool)>(egui::Id::unique("kw-offers"));
            d.remove::<u64>(egui::Id::unique("kw-begin"));
        });
    }
    ui.data_mut(|d| d.insert_temp(owner_key, owner.clone()));
    // while the box has the cursor it offers the library's keywords for what is being typed,
    // right under itself; it stays for a click on one of them, which takes the cursor away
    let field = owner.field();
    let oid = egui::Id::unique("kw-offers");
    let pass = ui.ctx().cumulative_pass_nr();
    // (remembered from the last frame: the text the highlight was for and the keyword it was
    // on, where the offers were drawn, and whether the box was in sight)
    let (typed_then, at_then, drawn, drawn_pass, hidden): (String, Option<String>, Rect, u64, bool) =
        ui.data(|d| d.get_temp(oid)).unwrap_or((String::new(), None, Rect::NOTHING, 0, false));
    let last_frame = drawn_pass.checked_add(1) == Some(pass);
    let clicking =
        last_frame && ui.input(|i| (i.pointer.any_down() || i.pointer.any_released()) && i.pointer.interact_pos().is_some_and(|p| drawn.contains(p)));
    let typed = crate::panels::keywording::typing(&text).to_string();
    // (none while the box is scrolled out of the panel: they would float over what is there)
    let offers = ((focused(ui.ctx(), field) || clicking) && !(last_frame && hidden))
        .then(|| app.caches.completions(&app.session.catalog, &have, &typed))
        .filter(|o| !o.keywords.is_empty() || o.create.is_some());
    // ↓ / ↑ move through the offers (none is highlighted at first, and ↑ from the first goes
    // back to none: Return then gives what was typed, as without offers); Return gives the
    // highlighted one. The highlight is on a keyword, not a line: it stays with it when the
    // library changes under it, and goes when the keyword does. With Shift or Alt the keys
    // are the text's own.
    let mut at = at_then.filter(|k| typed_then == typed && offers.as_ref().is_some_and(|o| o.keywords.contains(k)));
    let mut picked = None;
    if let Some(o) = offers.as_ref().filter(|o| !o.keywords.is_empty()) {
        let pressed = |ui: &mut egui::Ui, key: egui::Key| {
            ui.input_mut(|i| {
                let plain =
                    |e: &egui::Event| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && modifiers.is_none());
                let found = i.events.iter().any(plain);
                i.events.retain(|e| !plain(e));
                found
            })
        };
        let line = at.as_ref().and_then(|k| o.keywords.iter().position(|x| x == k));
        if pressed(ui, egui::Key::ArrowDown) {
            let next = line.map_or(0, |a| a.saturating_add(1)).min(o.keywords.len().saturating_sub(1));
            at = o.keywords.get(next).cloned();
        } else if pressed(ui, egui::Key::ArrowUp) {
            at = line.and_then(|a| a.checked_sub(1)).and_then(|a| o.keywords.get(a).cloned());
        }
        if let Some(k) = &at
            && pressed(ui, egui::Key::Enter)
        {
            picked = Some(crate::panels::keywording::Picked::Keyword(k.clone()));
        }
    }
    // (after an offer was taken the box starts a new edit, empty: Esc has nothing to bring back)
    // (asked for on the frame before this one only: a box that wasn't drawn then, its panel
    // switched away, doesn't claim the cursor whenever it is next drawn)
    let bid = egui::Id::unique("kw-begin");
    let begin = ui.data_mut(|d| d.remove_temp::<u64>(bid)).is_some_and(|asked| asked.checked_add(1) == Some(pass));
    // Add Keywords (⌘K) asks for the cursor too, and for the box to be scrolled into sight
    let asked = app.ui.focus_keywords.take().is_some_and(|at| pass <= at.saturating_add(crate::state::FOCUS_KEYWORDS_PASSES));
    if begin || asked {
        craft::ui::runtime::focus::request(ui.ctx(), FocusTarget::Id(field));
    }
    let (r, record) = craft::ui::runtime::inspect::record(ui, field.with("inspect"), |ui| {
        TextField::new(crate::i18n::tr("Add keyword"), Bind::bound(&mut text))
            .id(field)
            .placeholder(crate::i18n::tr("Add keyword"))
            .limit(None)
            .submits()
            .sizing(Sizing::FillWidth)
            .show(ui)
    });
    let field_rect = crate::text_field::register_field(ui, "field:keyword", &r.response, record.as_deref());
    if begin || asked {
        craft::ui::runtime::focus::request(ui.ctx(), FocusTarget::Id(field));
    }
    if asked {
        ui.scroll_to_rect(field_rect, None);
    }
    let in_sight = field_rect.is_positive() && ui.clip_rect().contains(field_rect.center());
    let mut drawn = Rect::NOTHING;
    // (what isn't drawn can't be taken: Return on the frame the box scrolls out gives nothing)
    if !in_sight {
        picked = None;
    }
    if offers.is_none() {
        // (not over the offers)
        r.response.clone().on_hover_text(crate::i18n::tr("Separate several keywords with commas. Return gives them to the selected photos."));
    }
    if let Some(o) = offers.as_ref().filter(|_| in_sight) {
        let (clicked, rect) = crate::panels::keywording::offers_below(ui, field_rect, o, at.as_deref());
        drawn = rect;
        picked = picked.or(clicked);
    }
    ui.data_mut(|d| d.insert_temp(oid, (typed.clone(), at, drawn, pass, !in_sight)));
    // Return gives the keywords to the selected photos; Esc gives back what was there before
    // this edit (the shared field does)
    let returned = r.submitted && !text.trim().is_empty();
    // a new name goes inside the default parent (Put New Keywords Inside This Keyword)
    // (`>` and `<` mark a hierarchy as `|` does)
    let as_typed = |app: &LightcraftApp, text: &str| -> Vec<String> { crate::panels::keywording::typed_names(&app.session.catalog, text) };
    let adding: Option<Vec<String>> = match picked {
        // an offer: it, and the keywords typed before the last comma
        Some(crate::panels::keywording::Picked::Keyword(k)) => {
            let mut kws = crate::panels::keywording::typed_before(&app.session.catalog, &text);
            kws.push(k);
            Some(kws)
        }
        Some(crate::panels::keywording::Picked::Typed) => Some(as_typed(app, &text)),
        None if returned => Some(as_typed(app, &text)),
        None => None,
    };
    if let Some(kws) = adding {
        // (refused, the text stays to be put right, and the refusal is said)
        match app.run("photo.setMeta", json!({"ids": selection, "addKeywords": kws})) {
            Ok(_) => {
                text.clear();
                // an offer taken leaves the cursor in the box, for the next keyword
                if !returned {
                    ui.data_mut(|d| d.insert_temp(bid, pass));
                }
            }
            Err(e) => app.toast_error(ui.ctx(), e),
        }
    }
    if (r.submitted || r.escaped)
        && let Some(stop) = craft::ui::runtime::focus::focused_rec(ui.ctx()).filter(|rec| rec.stop.id == field)
    {
        ui.memory_mut(|m| m.surrender_focus(stop.stop.root));
    }
    ui.data_mut(|d| d.insert_temp(kid, text));
    have
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_survives_reordered_selection_but_changes_for_another_photo_library_or_gap() {
        let mut owner = Owner::default();
        assert_eq!(owner.prepare(7, &[PhotoId(1), PhotoId(2)], 10), (true, true));
        let first = owner.field();
        assert_eq!(owner.prepare(7, &[PhotoId(2), PhotoId(1), PhotoId(2)], 11), (false, false));
        assert_eq!(owner.field(), first);
        assert_eq!(owner.prepare(7, &[PhotoId(1), PhotoId(2)], 11), (false, false), "a second draw in the same pass is not stale");
        assert_eq!(owner.field(), first);
        assert_eq!(owner.prepare(7, &[PhotoId(3)], 12), (true, true));
        assert_ne!(owner.field(), first);
        let second = owner.field();
        assert_eq!(owner.prepare(8, &[PhotoId(3)], 13), (true, true));
        assert_ne!(owner.field(), second);
        let third = owner.field();
        assert_eq!(owner.prepare(8, &[PhotoId(3)], 15), (false, true), "same owner keeps text but restarts SDK focus/popup state");
        assert_ne!(owner.field(), third);
    }
}
