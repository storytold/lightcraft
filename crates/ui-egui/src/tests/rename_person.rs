//! Rename Person from the People overview and from a person's page.

use std::time::Duration;

use lightcraft_catalog::Op;
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

fn face(x: f64, name: &str) -> lightcraft_meta::Region {
    lightcraft_meta::Region {
        rect: lightcraft_geom::Rect { x0: x, y0: 0.2, x1: x + 0.25, y1: 0.6 },
        kind: lightcraft_meta::RegionKind::Face,
        name: Some(name.to_string()),
        description: None,
    }
}

fn name_faces(h: &mut Headless, id: lightcraft_catalog::PhotoId, regions: Vec<lightcraft_meta::Region>) {
    let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
    meta.regions = regions;
    h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

fn right_click(h: &mut Headless, id: &str) {
    let c = widget(h, id).center();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

fn open_people(h: &mut Headless) {
    let r = h.request("engine.execute", json!({"command": "view.people"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    h.step();
}

fn stored(h: &Headless, id: lightcraft_catalog::PhotoId) -> Vec<Option<String>> {
    h.app.session.catalog.photo(id).unwrap().meta.regions.iter().map(|r| r.name.clone()).collect()
}

/// Both entry points open the dialog. Cancel and an empty name change nothing; a real rename updates
/// the person page, the way back to it, and a person filter.
#[test]
fn rename_person_from_the_card_and_the_page() {
    let mut h = demo();
    let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(2).collect();
    name_faces(&mut h, ids[0], vec![face(0.1, "Jane Doe"), face(0.5, "jane doe")]);
    name_faces(&mut h, ids[1], vec![face(0.3, "Jane Doe")]);
    open_people(&mut h);
    right_click(&mut h, "person:Jane Doe");
    click(&mut h, "personMenu:rename");
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::RenamePerson { from, to, .. }) if from == "Jane Doe" && to == "Jane Doe"));
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(stored(&h, ids[0]), vec![Some("Jane Doe".into()), Some("jane doe".into())]);

    h.request("engine.execute", json!({"command": "view.person", "params": {"name": "Jane Doe"}}), T);
    h.step();
    click(&mut h, "person:rename");
    let Some(Dialog::RenamePerson { to, .. }) = &mut h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    to.clear();
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::RenamePerson { .. })), "an empty name does not rename");
    assert_eq!(stored(&h, ids[0])[0].as_deref(), Some("Jane Doe"));

    let Some(Dialog::RenamePerson { to, .. }) = &mut h.app.ui.dialog else { panic!("dialog closed") };
    *to = "Ada Lovelace".into();
    h.app.ui.last_person = Some("Jane Doe".into());
    h.app.ui.person_from = Some((Some("Jane Doe".into()), ids[0].0));
    h.app.session.execute("library.filter", &json!({"person": "jane doe"})).unwrap();
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
    assert!(stored(&h, ids[0]).iter().all(|n| n.as_deref() == Some("Ada Lovelace")));
    assert_eq!(stored(&h, ids[1])[0].as_deref(), Some("Ada Lovelace"));
    assert_eq!(h.app.ui.person_page.as_deref(), Some("Ada Lovelace"));
    assert_eq!(h.app.ui.last_person.as_deref(), Some("Ada Lovelace"));
    assert_eq!(h.app.ui.person_from.as_ref().and_then(|(n, _)| n.as_deref()), Some("Ada Lovelace"));
    assert_eq!(h.app.session.filter.person.as_deref(), Some("Ada Lovelace"));
    h.app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(stored(&h, ids[0]), vec![Some("Jane Doe".into()), Some("jane doe".into())]);
}

/// Renaming onto someone else waits for Combine, then one undo step restores both people.
#[test]
fn rename_person_asks_before_merging() {
    let mut h = demo();
    let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(2).collect();
    name_faces(&mut h, ids[0], vec![face(0.1, "Jane Doe")]);
    name_faces(&mut h, ids[1], vec![face(0.3, "John Roe")]);
    open_people(&mut h);
    h.request("engine.execute", json!({"command": "view.person", "params": {"name": "Jane Doe"}}), T);
    h.step();
    click(&mut h, "person:rename");
    let Some(Dialog::RenamePerson { to, .. }) = &mut h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    *to = "John Roe".into();
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::RenamePerson { .. })), "a merge is not silent");
    assert_eq!(stored(&h, ids[0])[0].as_deref(), Some("Jane Doe"));
    click(&mut h, "check:personMerge");
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(stored(&h, ids[0])[0].as_deref(), Some("John Roe"));
    assert_eq!(h.app.ui.person_page.as_deref(), Some("John Roe"));
    let names: Vec<String> = h.app.session.catalog.people().into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["John Roe".to_string()]);
    h.app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(stored(&h, ids[0])[0].as_deref(), Some("Jane Doe"));
    assert_eq!(stored(&h, ids[1])[0].as_deref(), Some("John Roe"));
}
