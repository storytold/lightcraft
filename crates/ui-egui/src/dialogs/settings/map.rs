use super::widgets::{field, heading, hint, row};
use super::{LightcraftApp, RichText, Tokens, json, register};

// ----------------------------------------------------------------------------------------- Map

/// Where users get an ArcGIS Location Platform API key (free tier).
const ARCGIS_SIGN_UP: &str = "https://location.arcgis.com";

/// Settings ▸ Map: the ArcGIS API key the Esri layers need. The key typed here is applied with
/// Apply or Return (never while typing), then the field empties: the key is never shown again,
/// only whether one is set. It is kept outside `ui.json` and the library (`map.setEsriKey`).
pub(super) fn map_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Esri map layers"));
    hint(
        ui,
        t,
        crate::i18n::tr(
            "The Esri Streets and Esri Outdoor map styles need your own ArcGIS Location Platform API key. A free account gives a monthly tile allowance; tiles count against your key. Esri's server then sees which places you look at.",
        ),
    );
    let set = app.session.map.esri_key_set();
    row(ui, t, crate::i18n::tr("Status"), |ui| {
        let text = if set { crate::i18n::tr("A key is set") } else { crate::i18n::tr("No key: the Esri styles are not offered") };
        let r = ui.label(RichText::new(text).color(if set { t.text } else { t.text_dim }));
        register(ui, "settings:esriKeyStatus", r.rect);
    });
    // the draft lives in egui's temporary memory only (never saved with the window state)
    let draft_id = egui::Id::unique("settings:esriKeyDraft");
    let mut draft: String = ui.data_mut(|d| d.get_temp::<String>(draft_id)).unwrap_or_default();
    let mut apply = false;
    row(ui, t, crate::i18n::tr("ArcGIS API key"), |ui| {
        let shown = field(
            ui,
            crate::text_field::TextField::singleline("field:esriKey", &mut draft)
                .hint(crate::i18n::tr(if set { "Paste a new key to replace it" } else { "Paste your key" }))
                .width(260.0),
        );
        apply |= matches!(shown.ending, Some(crate::text_field::Ending::Return));
        let b = ui.add_enabled(!draft.trim().is_empty(), egui::Button::new(crate::i18n::tr("Apply")));
        register(ui, "button:esriKeyApply", b.rect);
        apply |= b.clicked();
        let c = ui.add_enabled(set, egui::Button::new(crate::i18n::tr("Remove Key")));
        register(ui, "button:esriKeyClear", c.rect);
        if c.clicked() {
            if let Err(e) = app.run("map.setEsriKey", json!({"key": ""})) {
                app.toast(ui.ctx(), e);
            }
            draft.clear();
        }
    });
    if apply && !draft.trim().is_empty() {
        match app.run("map.setEsriKey", json!({"key": draft.trim()})) {
            Ok(_) => draft.clear(),
            // a key of the wrong shape stays in the field to be corrected; nothing changed
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
    ui.data_mut(|d| d.insert_temp(draft_id, draft));
    row(ui, t, "", |ui| {
        let r = ui.link(crate::i18n::tr("Get a free key at location.arcgis.com"));
        register(ui, "link:arcgisSignUp", r.rect);
        if r.clicked()
            && let Err(e) = crate::links::open(app, ARCGIS_SIGN_UP)
        {
            app.toast(ui.ctx(), e);
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Give the key only the static basemap tiles privilege. It is kept in LightCraft's settings folder (map-keys.json), never in your library, and sent only to Esri's tile server. Choose the style in the Map view's Map Style list.",
        ),
    );
}
