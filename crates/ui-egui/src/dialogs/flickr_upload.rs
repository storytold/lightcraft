//! File ▸ Share ▸ Flickr…: size, quality, who can see the photos, which album, and what goes with
//! them. OK runs `flickr.upload` in the background ([`crate::flickr`]).

use serde_json::json;

use super::Tokens;
use super::frame::field;
use crate::LightcraftApp;

/// File → Share → Flickr…: size, quality, who can see the photos, and what goes with them.
pub(super) fn flickr_upload_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, opts: &mut crate::flickr::UploadDialog) {
    use crate::flickr::{PRIVACY, SIZES};
    let n = app.session.targets(&json!({})).map_or(0, |ids| ids.len());
    let status = app.session.execute("flickr.status", &json!({})).unwrap_or_default();
    let name = status["fullname"].as_str().filter(|s| !s.trim().is_empty()).or(status["username"].as_str()).unwrap_or_default().to_string();
    ui.label(
        egui::RichText::new(crate::i18n::tr_format!("Upload {n} photo{} to Flickr as {name}", if n == 1 { "" } else { "s" }, n = n, name = name))
            .color(t.text_dim),
    );
    field(ui, "Size", |ui| {
        let current = SIZES.iter().find(|(v, _)| *v == opts.long_edge).map_or("2048 px", |(_, l)| l);
        // (pixel sizes read the same in every language)
        let label = |v: u32, l: &str| if v == 0 { crate::i18n::tr(l).to_string() } else { l.to_string() };
        let c = crate::widgets::combo_box("flickrSize").selected_text(label(opts.long_edge, current)).show_ui(ui, |ui| {
            for (v, l) in SIZES {
                ui.selectable_value(&mut opts.long_edge, *v, label(*v, l));
            }
        });
        crate::widgets::name_combo(&c.response, crate::i18n::tr("Size"));
        crate::widgets::register(ui, "combo:flickrSize", c.response.rect);
    });
    field(ui, "Quality", |ui| {
        ui.add(egui::DragValue::new(&mut opts.quality).range(1..=100));
    });
    field(ui, "Visibility", |ui| {
        let current = PRIVACY.iter().find(|(v, _)| *v == opts.privacy).map_or("Public", |(_, l)| l);
        let c = crate::widgets::combo_box("flickrPrivacy").selected_text(crate::i18n::tr(current)).show_ui(ui, |ui| {
            for (v, l) in PRIVACY {
                if ui.selectable_label(opts.privacy == *v, crate::i18n::tr(l)).clicked() {
                    opts.privacy = v.to_string();
                }
            }
        });
        crate::widgets::name_combo(&c.response, crate::i18n::tr("Visibility"));
        crate::widgets::register(ui, "combo:flickrPrivacy", c.response.rect);
    });
    field(ui, "Album", |ui| {
        let albums = app.flickr.albums.clone().unwrap_or_default();
        let current = albums.iter().find(|(id, _, _)| *id == opts.album).map(|(_, title, _)| title.clone());
        // an album that is gone since it was chosen: none
        if current.is_none() && !opts.album.is_empty() && app.flickr.albums.is_some() {
            opts.album.clear();
        }
        let none = crate::i18n::tr("None").to_string();
        let new = crate::i18n::tr("New Album…").to_string();
        let shown = if opts.new_album { new.clone() } else { current.unwrap_or(none.clone()) };
        let c = crate::widgets::combo_box("flickrAlbum").width(220.0).selected_text(shown).show_ui(ui, |ui| {
            if ui.selectable_label(!opts.new_album && opts.album.is_empty(), none).clicked() {
                opts.album.clear();
                opts.new_album = false;
            }
            let r = ui.selectable_label(opts.new_album, new);
            crate::widgets::register(ui, "item:flickrNewAlbum", r.rect);
            if r.clicked() && !opts.new_album {
                opts.album.clear();
                opts.new_album = true;
                // the name field takes the focus
                ui.memory_mut(|m| m.request_focus(crate::text_field::id("field:flickrNewAlbum")));
            }
            if !albums.is_empty() {
                ui.separator();
            }
            for (id, title, photos) in &albums {
                let title = if title.trim().is_empty() { id.clone() } else { title.clone() };
                if ui.selectable_label(!opts.new_album && opts.album == *id, title).on_hover_text(photos.to_string()).clicked() {
                    opts.album = id.clone();
                    opts.new_album = false;
                }
            }
            if app.flickr.albums_loading && albums.is_empty() {
                ui.add_enabled(false, egui::Label::new(crate::i18n::tr("Loading albums…")));
            }
        });
        crate::widgets::name_combo(&c.response, crate::i18n::tr("Album"));
        crate::widgets::register(ui, "combo:flickrAlbum", c.response.rect);
        if app.flickr.albums_loading {
            ui.spinner();
        }
    });
    if opts.new_album {
        field(ui, "Album name", |ui| {
            crate::text_field::TextField::singleline("field:flickrNewAlbum", &mut opts.new_album_title)
                .hint(crate::i18n::tr("Name of the new album"))
                .width(220.0)
                .show(ui);
        });
        ui.label(
            egui::RichText::new(crate::i18n::tr("Flickr makes the album with the first photo that goes up as its cover."))
                .size(11.0)
                .color(t.text_dim),
        );
    }
    if let Some(e) = &app.flickr.albums_error {
        ui.label(egui::RichText::new(format!("{} {e}", crate::i18n::tr("Couldn't read your Flickr albums:"))).size(11.0).color(t.caution));
    }
    for (value, id, label) in [
        (&mut opts.tags, "flickrTags", "Keywords as tags"),
        (&mut opts.description, "flickrDescription", "Caption as description"),
        (&mut opts.remove_location, "flickrRemoveLocation", "Remove location info"),
        (&mut opts.hidden, "flickrHidden", "Hide from public searches"),
    ] {
        let r = ui.checkbox(value, crate::i18n::tr(label));
        crate::widgets::register(ui, format!("check:{id}"), r.rect);
    }
    ui.label(
        egui::RichText::new(crate::i18n::tr("Each photo goes up with its edits as a JPEG, titled with its title or else its file name."))
            .size(11.0)
            .color(t.text_dim),
    );
}
