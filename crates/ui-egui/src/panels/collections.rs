//! The Classic Collections panel's own parts: the target collection line under the tree and
//! collection definition export / import. The tree itself (collection sets, collections, smart
//! collections, drag and drop) is `left::albums_body`.

use egui::{Align2, Sense, pos2, vec2};
use serde_json::json;

use crate::DacApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// Below the collection tree: which collection B adds to (`collectionsTarget`); a click on × goes
/// back to the Quick Collection.
pub fn footer(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let target = app.session.target_album.and_then(|a| app.session.catalog.album(a)).map(|a| a.name.clone());
    let name = target.clone().unwrap_or_else(|| crate::i18n::tr("Quick Collection").to_string());
    ui.add_space(4.0);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    register(ui.ctx(), "collectionsTarget", r);
    let edge = ui.clip_rect().right().min(r.right());
    ui.painter().text(
        pos2(r.left() + 18.0, r.center().y),
        Align2::LEFT_CENTER,
        crate::i18n::tr_format!("Target: {name}", name = name),
        t.font(11.5),
        t.text_dim,
    );
    resp.on_hover_text(crate::i18n::tr("B adds the selected photos to the target collection (set it from a collection's menu)"));
    if target.is_some() {
        let x = egui::Rect::from_center_size(pos2(edge - 20.0, r.center().y), vec2(18.0, 18.0));
        register(ui.ctx(), "collectionsTargetClear", x);
        let xr = ui.interact(x, egui::Id::new("collections-target-clear"), Sense::click());
        ui.painter().text(x.center(), Align2::CENTER_CENTER, "×", t.font(13.0), if xr.hovered() { t.text } else { t.text_dim });
        if xr.on_hover_text(crate::i18n::tr("Use the Quick Collection as the target again")).clicked() {
            let _ = app.run("album.setTarget", json!({"id": null}));
        }
    }
}

/// The + menu's import entry.
pub fn import_item(app: &mut DacApp, ui: &mut egui::Ui, parent: Option<u64>) {
    if app.services.pick_list_file.is_some() && ui.button(crate::i18n::tr("Import Collection Definition…")).clicked() {
        let picked = app.services.pick_list_file.as_mut().map(|f| f()).unwrap_or_default();
        if let Some(path) = picked.first() {
            match app.run("album.importDefinition", json!({"path": path, "parent": parent})) {
                Ok(r) => {
                    let (n, m) = (r["photos"].as_u64().unwrap_or(0), r["unmatched"].as_u64().unwrap_or(0));
                    app.toast(ui.ctx(), crate::i18n::tr_format!("Collection imported: {n} photos found, {m} not in this library", n = n, m = m));
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
        ui.close();
    }
}

/// A collection row's export entry.
pub fn export_item(app: &mut DacApp, ui: &mut egui::Ui, id: u64, name: &str) {
    if app.services.save_list_file.is_some() && ui.button(crate::i18n::tr("Export Collection Definition…")).clicked() {
        let suggested = format!("{}.json", name.replace(['/', '\\', ':'], "-"));
        let path = app.services.save_list_file.as_mut().and_then(|f| f(&suggested));
        if let Some(path) = path
            && let Err(e) = app.run("album.exportDefinition", json!({"id": id, "path": path}))
        {
            app.toast(ui.ctx(), e);
        }
        ui.close();
    }
}
