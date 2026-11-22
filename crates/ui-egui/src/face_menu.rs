//! Photo ▸ People: the items that act on "the chosen face" (fork-owned). One accessor,
//! [`chosen_face`], says which face that is, so every People item agrees:
//! - in the People view, the one unnamed face selected;
//! - in the loupe, the face whose name box is open, else the face box the pointer last rested on.
//!
//! Without a chosen face the items are disabled.

use crate::json_ext::JsonExt as _;
use serde_json::{Value, json};

use crate::LccApp;
use crate::menus::UiCommand;
use crate::module::ModuleId;
use crate::state::{NameEdit, ViewMode};

pub const MOVE_TO_UNNAMED: &str = "face.moveToUnnamed";
pub const REMOVE_REGION: &str = "face.removeRegion";
pub const EDIT_NAME: &str = "face.editName";
pub const FIND_SIMILAR: &str = "face.findSimilar";
pub const DRAW_REGIONS: &str = "face.drawRegions";

pub const COMMANDS: &[UiCommand] = &[
    (MOVE_TO_UNNAMED, "Move to Unnamed People", None, ""),
    (REMOVE_REGION, "Remove Face Region", None, ""),
    (EDIT_NAME, "Edit Name", Some("Shift+O"), ""),
    (FIND_SIMILAR, "Find Similar Faces", None, ""),
    (DRAW_REGIONS, "Draw Face Regions", None, ""),
];

/// The face the People items act on: (photo, region index), when it is a face region that exists.
pub fn chosen_face(app: &LccApp) -> Option<(u64, usize)> {
    let key = match app.ui.view {
        ViewMode::People => {
            let mut it = app.ui.unnamed_selected.iter();
            match (it.next(), it.next()) {
                (Some(k), None) => *k,
                _ => return None,
            }
        }
        ViewMode::Detail if app.ui.module == ModuleId::Library => {
            let active = app.session.active()?.0;
            let key = app.ui.name_edit.as_ref().map(|e| (e.photo, e.index)).or(app.ui.face_focus)?;
            if key.0 != active {
                return None;
            }
            key
        }
        _ => return None,
    };
    let photo = app.session.catalog.photo(lcc_catalog::PhotoId(key.0))?;
    photo.meta.regions.get(key.1).filter(|r| r.kind == lcc_meta::RegionKind::Face).map(|_| key)
}

fn region_name(app: &LccApp, (photo, index): (u64, usize)) -> Option<String> {
    app.session.catalog.photo(lcc_catalog::PhotoId(photo))?.meta.regions.get(index)?.name.clone()
}

/// Opens the page of the person the chosen face belongs to (a named face), or looks most like (an unnamed one, by the
/// faces already embedded: nothing is scanned here). That page lists their faces and the unnamed faces that look like them.
fn find_similar(app: &mut LccApp, (photo, index): (u64, usize)) -> Result<Value, String> {
    let name = match region_name(app, (photo, index)) {
        Some(n) => n,
        None => {
            let v = app.session.execute("faces.suggest", &json!({"ids": [photo], "budgetMs": 0})).map_err(|e| e.to_string())?;
            let best = v
                .field("photos")
                .as_array()
                .and_then(|a| a.iter().find(|p| p["id"].as_u64() == Some(photo)))
                .and_then(|p| p["faces"].as_array())
                .and_then(|a| a.iter().find(|f| f["index"].as_u64().and_then(|i| usize::try_from(i).ok()) == Some(index)))
                .and_then(|f| f["candidates"].as_array())
                .and_then(|c| c.first())
                .and_then(|c| c["name"].as_str());
            match best {
                Some(n) => n.to_string(),
                None => return Err("no named person looks like this face yet (name some faces first, and let the scan reach this photo)".to_string()),
            }
        }
    };
    app.run("view.person", json!({"name": name}))
}

pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    if id == DRAW_REGIONS {
        // the mode can always be left; it starts on a photo open in the Library's loupe
        return Some(app.ui.draw_faces || (app.ui.view == ViewMode::Detail && app.ui.module == ModuleId::Library && app.session.active().is_some()));
    }
    let face = chosen_face(app);
    match id {
        MOVE_TO_UNNAMED => Some(face.and_then(|f| region_name(app, f)).is_some()),
        REMOVE_REGION | EDIT_NAME | FIND_SIMILAR => Some(face.is_some()),
        _ => None,
    }
}

pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id == DRAW_REGIONS {
        // {on?}: no params toggles
        app.ui.draw_faces = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.draw_faces);
        if !app.ui.draw_faces {
            return Some(Ok(json!({"on": false})));
        }
        if app.ui.view != ViewMode::Detail || app.ui.module != ModuleId::Library || app.session.active().is_none() {
            app.ui.draw_faces = false;
            return Some(Err("open a photo in the Library's loupe to draw a face region".to_string()));
        }
        return Some(Ok(json!({"on": true})));
    }
    if !matches!(id, MOVE_TO_UNNAMED | REMOVE_REGION | EDIT_NAME | FIND_SIMILAR) {
        return None;
    }
    let Some((photo, index)) = chosen_face(app) else {
        return Some(Err("no face is chosen (select one unnamed face, or point at a face box in the loupe)".to_string()));
    };
    Some(match id {
        MOVE_TO_UNNAMED => app.run("faces.setName", json!({"id": photo, "index": index, "name": null})),
        REMOVE_REGION => {
            app.ui.unnamed_selected.remove(&(photo, index));
            app.ui.face_focus = None;
            app.ui.name_edit = None;
            app.run("photo.removeRegion", json!({"id": photo, "index": index}))
        }
        FIND_SIMILAR => find_similar(app, (photo, index)),
        _ => {
            if app.ui.view == ViewMode::People {
                // the naming bar's box takes the keyboard
                app.ui.unnamed_focus = true;
            } else {
                let text = region_name(app, (photo, index)).unwrap_or_default();
                app.ui.name_edit = Some(NameEdit { photo, index, text, fresh: true });
            }
            Ok(json!({"photo": photo, "index": index}))
        }
    })
}
