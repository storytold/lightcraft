//! Additive route evidence; the original fourteen behavioral checks remain unchanged.
use super::*;
use crate::{Services, headless::HeadlessView};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::{Selection, Session};
use serde_json::json;
struct Locale(crate::i18n::Locale);
impl Drop for Locale {
    fn drop(&mut self) {
        crate::i18n::set_language(self.0);
    }
}
#[test]
fn create_album_actual_parent_publishes_sdk_name_and_checkbox_owners() {
    let _locale = Locale(crate::i18n::language());
    crate::i18n::set_language(crate::i18n::Locale::En);
    let mut session = Session::new();
    session.clock = Box::new(|| "2026-10-01T12:00:00Z".into());
    session.xmp.auto_write = false;
    session
        .commit(
            "Fixture",
            Op::AddPhoto {
                photo: Box::new(Photo::new(PhotoId(101), Source::Demo { scene: 0 }, "Protected route photo", "JPEG", 8, 6, "2026-10-01T12:00:00Z")),
            },
        )
        .unwrap();
    session.selection = Selection::single(PhotoId(101));
    let mut app = LightcraftApp::new(session, Services { png: None, local_places: Some(vec![]), ..Default::default() });
    app.ui.settings.gpu = false;
    app.run("dialog.newAlbum", json!({"name":"SDK route"})).unwrap();
    let document = json!({"catalog":app.session.catalog,"selection":app.session.selection,"journal":app.session.journal,"settings":app.ui.settings,"commits":app.session.commits()});
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    app.install_framework(&ctx);
    let mut elements = vec![];
    for tick in 1..=8 {
        assert!(!super::settings::native_controls_active(&ctx), "actual unscoped generic parent defaults to SDK");
        let raw = HeadlessView::raw_input(egui::vec2(1400.0, 1000.0), 1.0, f64::from(tick) / 60.0, vec![]);
        let mut out = ctx.run_ui(raw, |root| {
            crate::widgets::take_registries(&ctx);
            let (_, recorded) = craft::ui::runtime::inspect::record(root, egui::Id::unique("create-album-sdk-route"), |_| {
                crate::dialogs::show(&mut app, &ctx, false)
            });
            elements = recorded.map(|recording| recording.elements.clone()).unwrap_or_default();
        });
        out.textures_delta.clear();
        out.platform_output.commands.clear();
        out.viewport_output.clear();
        out.shapes.clear();
        crate::widgets::take_registries(&ctx);
    }
    for (component, name) in [("TextField", "Name"), ("Checkbox", "Include selected photos")] {
        let owners = elements
            .iter()
            .filter(|element| {
                element.part.is_none()
                    && element.component.starts_with(component)
                    && element.facts.as_ref().is_some_and(|facts| facts.name.as_deref() == Some(name) && !facts.disabled)
            })
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 1, "one actual SDK owner for {name}; records={elements:#?}");
    }
    assert_eq!(
        json!({"catalog":app.session.catalog,"selection":app.session.selection,"journal":app.session.journal,"settings":app.ui.settings,"commits":app.session.commits()}),
        document
    );
    assert!(app.session.library.is_none() && app.session.library_folder.is_none() && app.session.media.rendered.disk().is_none());
    assert_eq!(app.renderer.live_workers(), 0);
    assert_eq!(app.renderer.in_flight(), 0);
}
