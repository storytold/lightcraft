//! Saved creations in the Collections panel (P3.7): Saved Prints, Books, Slideshows and Web
//! Galleries are collections carrying a layout or module settings (`dac_engine::creations`).
//! The panel marks each kind with its own icon; a double-click (or `creation.open {id}`) shows
//! the collection and opens the creation in its module.
//!
//! Sources: own design.

use dac_catalog::{Album, AlbumId};
use dac_layout::CreationKind;
use serde_json::{Value, json};

use crate::DacApp;
use crate::icons::Icon;

/// UI commands: (id, label, shortcut, menu).
pub const COMMANDS: &[crate::menus::UiCommand] = &[("creation.open", "Open Saved Creation", None, "")];

/// The kind of saved creation `a` is, if it is one.
pub fn kind(a: &Album) -> Option<CreationKind> {
    a.creation.as_ref().and_then(|c| dac_engine::creations::kind_of(&c.kind))
}

/// The Collections panel icon of a saved creation.
pub fn icon(a: &Album) -> Option<Icon> {
    Some(match kind(a)? {
        CreationKind::Print => Icon::Printer,
        CreationKind::Book => Icon::Book,
        CreationKind::Slideshow => Icon::Slideshow,
        CreationKind::Web => Icon::Globe,
    })
}

/// The module a creation kind opens in.
fn module_of(k: CreationKind) -> &'static str {
    match k {
        CreationKind::Print => "print",
        CreationKind::Book => "book",
        CreationKind::Slideshow => "slideshow",
        CreationKind::Web => "web",
    }
}

/// Is `id` one of ours, and is it enabled?
pub fn enabled(_app: &DacApp, id: &str) -> Option<bool> {
    COMMANDS.iter().any(|c| c.0 == id).then_some(true)
}

/// Run one of our commands; `None`: not one.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    (id == "creation.open").then(|| open(app, p))
}

/// `creation.open {id}`: shows the creation's collection (its photos fill the filmstrip) and
/// opens it in its module.
fn open(app: &mut DacApp, p: &Value) -> Result<Value, String> {
    let id = p.get("id").and_then(Value::as_u64).ok_or("missing id")?;
    let k = app.session.catalog.album(AlbumId(id)).and_then(kind).ok_or("not a saved creation")?;
    app.run("library.source", json!({"kind": "album", "id": id}))?;
    let module = module_of(k);
    app.run("module.switch", json!({"module": module}))?;
    let r = match k {
        CreationKind::Print => app.run("printui.openCreation", json!({"id": id}))?,
        CreationKind::Book => app.run("book.open", json!({"id": id}))?,
        CreationKind::Slideshow => app.run("slideshow.openSaved", json!({"id": id}))?,
        CreationKind::Web => crate::web_module::open_creation(app, id)?,
    };
    Ok(json!({"id": id, "kind": dac_engine::creations::kind_name(k), "module": module, "result": r}))
}

/// A double-click on a creation's row in the Collections panel.
pub fn double_clicked(app: &mut DacApp, ctx: &egui::Context, a: &Album) {
    if kind(a).is_some()
        && let Err(e) = app.run("creation.open", json!({"id": a.id.0}))
    {
        app.toast_error(ctx, e);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use crate::headless::Headless;
    use crate::{DacApp, Services};

    const T: Duration = Duration::from_secs(60);

    pub(crate) fn demo() -> Headless {
        let services = Services { png: None, ..Default::default() };
        let app = DacApp::new(dac_engine::Session::with_demo(), services);
        let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
        h.settle(Duration::from_secs(120));
        h
    }

    fn ok(h: &mut Headless, cmd: &str, p: Value) -> Value {
        let r = h.request("engine.execute", json!({"command": cmd, "params": p}), T);
        h.step();
        assert_eq!(r["ok"], true, "{cmd}: {r}");
        r["result"].clone()
    }

    #[test]
    fn every_kind_of_creation_opens_in_its_module_with_its_photos() {
        let mut h = demo();
        let ids: Vec<u64> = h.app.session.catalog.photos().take(3).map(|p| p.id.0).collect();
        let print = ok(&mut h, "creation.save", json!({"kind": "print", "name": "Sheet", "template": "2×2 Cells, A4", "ids": ids}))["id"].clone();
        let slides =
            ok(&mut h, "creation.save", json!({"kind": "slideshow", "name": "Trip", "settings": {"playback": {"slideSecs": 7}}, "ids": ids}))["id"]
                .clone();
        let web = ok(&mut h, "creation.save", json!({"kind": "web", "name": "Site", "settings": {"site": {"title": "Site"}}, "ids": [ids[0]]}))["id"]
            .clone();
        for (id, module) in [(&print, "print"), (&slides, "slideshow"), (&web, "web")] {
            let r = ok(&mut h, "creation.open", json!({"id": id}));
            assert_eq!(r["module"], module, "{r}");
            h.settle(T);
            assert_eq!(serde_json::to_value(h.app.ui.module).unwrap(), json!(module));
            assert_eq!(h.app.session.source, dac_engine::LibrarySource::Album(dac_catalog::AlbumId(id.as_u64().unwrap())));
        }
        assert_eq!(h.app.session.visible_cloned().len(), 1, "the web gallery's photos");
        assert_eq!(crate::web_module::state(&h.view.ctx).gallery_name, "Site");
        ok(&mut h, "creation.open", json!({"id": slides}));
        assert_eq!(h.app.ui.slides.settings.playback.slide_secs, 7.0);
        assert_eq!(h.app.ui.slides.open_id, slides.as_u64());
        // a Saved Print made in the module keeps its settings
        ok(&mut h, "creation.open", json!({"id": print}));
        ok(&mut h, "printui.template", json!({"name": dac_print::job::builtin_templates()[1].0}));
        let want = serde_json::to_value(&h.app.print.settings).unwrap();
        let mine = ok(&mut h, "printui.saveCreation", json!({"name": "Mine"}))["creation"].clone();
        ok(&mut h, "printui.template", json!({"name": dac_print::job::builtin_templates()[0].0}));
        ok(&mut h, "creation.open", json!({"id": mine}));
        assert_eq!(serde_json::to_value(&h.app.print.settings).unwrap(), want);
        let r = h.request("engine.execute", json!({"command": "creation.open", "params": {"id": 999_999}}), T);
        assert_eq!(r["ok"], false);
        // the panel marks creations with their own icons
        let a = h.app.session.catalog.album(dac_catalog::AlbumId(print.as_u64().unwrap())).unwrap().clone();
        assert_eq!(super::icon(&a), Some(crate::icons::Icon::Printer));
    }
}
