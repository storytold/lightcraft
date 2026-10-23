//! Measured camera looks built into LightCraft (`assets/camera-looks/`, see `assets/ATTRIBUTION.md`): a camera's own
//! renderings of its picture styles, measured by having the camera convert charts of known colour, each a 3D lookup
//! from white-balanced camera RGB to the camera's output (`lightcraft_pipeline::looks`,
//! `docs/camera-preview-colour.md`). A camera model's looks are profiles (`look:<model>/<look>`) that the profile
//! browser lists for that camera's raws. One JSON file per model holds its looks; they are parsed and registered with
//! the pipeline once per process, and a damaged file is skipped with a warning. They hold measured colour only.
use std::sync::{Arc, OnceLock};

use lightcraft_pipeline::looks::{self, Axis, Look};
use serde::Deserialize;

/// Format version of the look files.
const VERSION: u32 = 1;

/// Looks built into LightCraft: `(model, JSON)`.
pub const BUNDLED: &[(&str, &str)] = &[("X-T4", include_str!("../../../assets/camera-looks/X-T4.json"))];

#[derive(Deserialize)]
struct LookFile {
    version: u32,
    /// The camera model as its files report it (Exif `Model`).
    model: String,
    /// The profile browser group the looks are listed under.
    group: String,
    input: Input,
    /// A node value is `n · node_step + node_offset` for its stored 16-bit `n`.
    node_offset: f64,
    node_step: f64,
    looks: Vec<Entry>,
}

#[derive(Deserialize)]
struct Input {
    /// White-balanced camera RGB → CIE XYZ, rows.
    forward_matrix: [[f64; 3]; 3],
    /// The XYZ white CIELAB is taken against.
    white: [f64; 3],
    /// Lightness and a*/b* grids: first node, last node, node count.
    lightness: (f64, f64, usize),
    ab: (f64, f64, usize),
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    name: String,
    description: String,
    /// Every node's three channels as 16-bit numbers, 12 hexadecimal digits per node.
    nodes: String,
}

/// The profile id of look `look` of camera `model`.
pub fn profile_id(model: &str, look: &str) -> String {
    format!("{}{}/{}", looks::PREFIX, model.trim().to_ascii_lowercase(), look)
}

/// The grid nodes of a look file entry.
fn decode_nodes(hex: &str, offset: f64, step: f64) -> Result<Vec<[f32; 3]>, String> {
    if !(offset.is_finite() && step.is_finite() && step > 0.0) {
        return Err("bad node encoding".into());
    }
    let b = hex.as_bytes();
    if !b.len().is_multiple_of(12) {
        return Err(format!("{} hexadecimal digits is not a whole number of nodes", b.len()));
    }
    let value = |d: &[u8]| -> Option<f32> {
        let n = u16::from_str_radix(std::str::from_utf8(d).ok()?, 16).ok()?;
        Some((f64::from(n) * step + offset) as f32)
    };
    b.as_chunks::<12>()
        .0
        .iter()
        .map(|node| {
            let at = |k: usize| node.get(4 * k..4 * k + 4).and_then(value);
            match (at(0), at(1), at(2)) {
                (Some(r), Some(g), Some(b)) => Ok([r, g, b]),
                _ => Err("a node is not hexadecimal".to_string()),
            }
        })
        .collect()
}

/// Parse and validate a look file (`origin` names it in errors).
fn parse(json: &str, origin: &str) -> Result<Vec<Look>, String> {
    let f: LookFile = serde_json::from_str(json).map_err(|e| format!("{origin}: {e}"))?;
    if f.version != VERSION {
        return Err(format!("{origin}: version {} (this build reads {VERSION})", f.version));
    }
    let axis = |(min, max, n): (f64, f64, usize)| Axis { min: min as f32, max: max as f32, n };
    f.looks
        .iter()
        .map(|e| {
            let nodes = decode_nodes(&e.nodes, f.node_offset, f.node_step).map_err(|why| format!("{origin}, {}: {why}", e.id))?;
            Look::new(
                &profile_id(&f.model, &e.id),
                &f.model,
                &e.name,
                &f.group,
                &e.description,
                f.input.forward_matrix,
                f.input.white,
                axis(f.input.lightness),
                axis(f.input.ab),
                nodes,
            )
            .map_err(|why| format!("{origin}: {why}"))
        })
        .collect()
}

/// Every built-in look, parsed and registered with the pipeline on first use.
pub fn all() -> &'static [Arc<Look>] {
    static ALL: OnceLock<Vec<Arc<Look>>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut out = Vec::new();
        for (model, json) in BUNDLED {
            match parse(json, &format!("built-in looks {model}")) {
                Ok(list) => {
                    for look in list {
                        looks::register(look.clone());
                        out.push(Arc::new(look));
                    }
                }
                Err(e) => eprintln!("lightcraft: ignoring camera looks {e}"),
            }
        }
        out
    })
}

/// The built-in looks of camera `model` (as its files report it), in the order the file lists them.
pub fn for_model(model: &str) -> Vec<&'static Look> {
    all().iter().map(|l| l.as_ref()).filter(|l| l.fits(model)).collect()
}

/// The built-in looks of a photo shot on `camera` (Exif make and model, as the catalog keeps them: "FUJIFILM X-T4").
pub fn for_camera(camera: &str) -> Vec<&'static Look> {
    let camera = camera.trim();
    all()
        .iter()
        .map(|l| l.as_ref())
        .filter(|l| {
            let tail = camera.len().checked_sub(l.model.len()).and_then(|i| camera.get(i..));
            let before = camera.len().checked_sub(l.model.len() + 1).and_then(|i| camera.get(i..=i));
            tail.is_some_and(|t| l.fits(t)) && before.is_none_or(|c| c == " ")
        })
        .collect()
}

/// The built-in looks photo `p` can render: none unless it is a raw developed from its sensor data.
pub fn for_photo(p: &lightcraft_catalog::Photo) -> Vec<&'static Look> {
    if !p.develops_raw() {
        return Vec::new();
    }
    for_camera(&p.meta.camera)
}

/// Whether photo `p` can render the built-in look with profile id `id`.
pub fn renders(p: &lightcraft_catalog::Photo, id: &str) -> bool {
    for_photo(p).iter().any(|l| l.id == id)
}

/// The built-in look with profile id `id`.
pub fn find(id: &str) -> Option<&'static Look> {
    if !looks::is_look_profile(id) {
        return None;
    }
    all().iter().map(|l| l.as_ref()).find(|l| l.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nine bundled X-T4 looks parse, register, match the camera and render white and grey sensibly.
    #[test]
    fn the_bundled_x_t4_looks_parse_and_register() {
        let list = for_model("X-T4");
        assert_eq!(list.len(), 9, "the nine colour film simulations");
        let mut ids = std::collections::HashSet::new();
        for l in &list {
            assert!(ids.insert(l.id.clone()), "{} twice", l.id);
            assert!(l.id.starts_with("look:x-t4/"), "{}", l.id);
            assert!(lightcraft_pipeline::looks::get(&l.id).is_some(), "{} registered", l.id);
            assert!(find(&l.id).is_some());
            // camera white renders as white, and a grey of 18 % of the sensor range as a light grey (the looks lift it; Classic Neg. tints it)
            let w = l.apply([1.0, 1.0, 1.0]);
            assert!(w.iter().all(|v| (0.85..=1.01).contains(v)), "{}: white {w:?}", l.id);
            let g = l.apply([0.18, 0.18, 0.18]);
            assert!(g.iter().all(|v| (0.1..=0.6).contains(v)), "{}: grey {g:?}", l.id);
        }
        assert_eq!(find("look:x-t4/velvia").map(|l| l.name.as_str()), Some("Vivid (Velvia)"));
        assert!(for_model("X-T3").is_empty());
        assert_eq!(for_camera("FUJIFILM X-T4").len(), 9);
        assert_eq!(for_camera("X-T4").len(), 9);
        assert!(for_camera("FUJIFILM X-T40").is_empty() && for_camera("FUJIFILM XX-T4").is_empty() && for_camera("").is_empty());
        assert!(find("lc.vivid").is_none());
    }

    /// A public X-T4 raw (raw.pixls.us 3914, DR100) records its camera colour for the looks: with its starting colour's
    /// table applied when decoded plainly, left out when decoded for a look, and the two render alike without a look.
    #[test]
    fn corpus_x_t4_raw_records_its_camera_colour() {
        let path = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw/raf-fuji-xt4-3914.raf");
        let Ok(bytes) = std::fs::read(&path) else {
            lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
            return;
        };
        assert_eq!(lightcraft_raw::fujifilm_dynamic_range(&bytes), Some(100));
        let (plain, plain_info) = crate::files::load_vec(bytes.clone(), 400).unwrap();
        let (for_look, look_info) = crate::files::load_vec_for_look(bytes, 400).unwrap();
        let (a, b) = (plain_info.camera_rgb.clone().unwrap(), look_info.camera_rgb.clone().unwrap());
        assert_eq!((a.model.as_str(), a.exposure, b.exposure), ("X-T4", 1.0, 1.0));
        assert!(a.hue_sat.is_some() && !a.deferred && b.deferred, "the bundled X-T4 profile's table");
        assert_eq!(a.matrix, b.matrix);
        let req = lightcraft_pipeline::RenderRequest::fit(400, 400);
        let mut s = lightcraft_develop::DevelopSettings::default();
        let one = lightcraft_pipeline::render(&plain, &plain_info, &s, &req).image;
        let two = lightcraft_pipeline::render(&for_look, &look_info, &s, &req).image;
        let diff: f64 = one.data.iter().flatten().zip(two.data.iter().flatten()).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>()
            / (one.data.len() * 4) as f64;
        assert!(diff < 0.5, "starting look from the two decodes: mean difference {diff} levels");
        s.profile.id = "look:x-t4/velvia".into();
        let velvia = lightcraft_pipeline::render(&for_look, &look_info, &s, &req).image;
        assert_ne!(velvia.data, two.data, "the look renders");
    }

    /// `develop.profile` sets a look on a raw of its camera and refuses it for any other photo.
    #[test]
    fn a_look_is_set_only_on_a_raw_of_its_camera() {
        use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
        let mut s = crate::Session::new();
        for (id, file, format, kind, camera) in [
            (1, "a.RAF", "RAF", MediaKind::Raw, "FUJIFILM X-T4"),
            (2, "b.JPG", "JPEG", MediaKind::Image, "FUJIFILM X-T4"),
            (3, "c.RAF", "RAF", MediaKind::Raw, "FUJIFILM X-T3"),
        ] {
            let mut p = Photo::new(PhotoId(id), Source::File { path: file.into() }, file, format, 6240, 4160, "2026-10-21");
            p.kind = kind;
            p.meta.camera = camera.into();
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        let set = |s: &mut crate::Session, id: u64| {
            s.execute("library.select", &serde_json::json!({"ids": [id]})).unwrap();
            s.execute("develop.profile", &serde_json::json!({"id": "look:x-t4/velvia"}))
        };
        assert!(set(&mut s, 1).is_ok());
        assert_eq!(s.develop_of(PhotoId(1)).unwrap().profile.id, "look:x-t4/velvia");
        let e = set(&mut s, 2).unwrap_err().to_string();
        assert!(e.contains("X-T4 raws only"), "{e}");
        assert!(set(&mut s, 3).is_err(), "another camera");
        assert_eq!(for_photo(s.catalog.photo(PhotoId(1)).unwrap()).len(), 9);
        assert!(for_photo(s.catalog.photo(PhotoId(2)).unwrap()).is_empty());
    }

    /// Paste, Paste from Previous, Sync, presets and Auto Sync carry a look only to the photos that can render it:
    /// another camera's raw or a JPEG keeps its own profile and still takes the other settings.
    #[test]
    fn copied_looks_reach_only_photos_that_render_them() {
        use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
        use serde_json::json;
        let mut s = crate::Session::new();
        for (id, file, format, kind, camera) in [
            (1, "a.RAF", "RAF", MediaKind::Raw, "FUJIFILM X-T4"),
            (2, "b.RAF", "RAF", MediaKind::Raw, "FUJIFILM X-T4"),
            (3, "c.RAF", "RAF", MediaKind::Raw, "FUJIFILM X-T3"),
            (4, "d.JPG", "JPEG", MediaKind::Image, "FUJIFILM X-T4"),
        ] {
            let mut p = Photo::new(PhotoId(id), Source::File { path: file.into() }, file, format, 6240, 4160, "2026-10-21");
            p.kind = kind;
            p.meta.camera = camera.into();
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        let select = |s: &mut crate::Session, ids: &[u64]| {
            s.execute("library.select", &json!({"ids": ids})).unwrap();
            s.selection.active = Some(PhotoId(ids[0]));
        };
        // the X-T3 raw has a profile of its own; the first X-T4 raw a look and +0.5 EV
        select(&mut s, &[3]);
        s.execute("develop.profile", &json!({"id": "lc.vivid"})).unwrap();
        select(&mut s, &[1]);
        s.execute("develop.profile", &json!({"id": "look:x-t4/velvia"})).unwrap();
        s.execute("develop.set", &json!({"values": {"light.exposure": 0.5}})).unwrap();
        let jpeg_profile = s.develop_of(PhotoId(4)).unwrap().profile.id.clone();
        let check = |s: &crate::Session, path: &str, ev: f64| {
            let got = |id| {
                let d = s.develop_of(PhotoId(id)).unwrap();
                (d.profile.id.clone(), d.light.exposure)
            };
            assert_eq!(got(2), ("look:x-t4/velvia".to_string(), ev), "{path}: an X-T4 raw takes the look");
            assert_eq!(got(3), ("lc.vivid".to_string(), ev), "{path}: another camera's raw keeps its profile");
            assert_eq!(got(4), (jpeg_profile.clone(), ev), "{path}: a JPEG keeps its profile");
        };

        s.execute("develop.copy", &json!({})).unwrap();
        s.execute("develop.paste", &json!({"ids": [2, 3, 4]})).unwrap();
        check(&s, "paste", 0.5);
        s.execute("edit.undo", &json!({})).unwrap();

        select(&mut s, &[2, 3, 4]);
        assert_eq!(s.execute("develop.pastePrevious", &json!({"ids": [2, 3, 4]})).unwrap()["from"], 1);
        check(&s, "paste from previous", 0.5);
        s.execute("edit.undo", &json!({})).unwrap();

        select(&mut s, &[1, 2, 3, 4]);
        s.execute("develop.sync", &json!({})).unwrap();
        check(&s, "sync", 0.5);
        s.execute("edit.undo", &json!({})).unwrap();

        select(&mut s, &[1]);
        let preset = s.execute("preset.create", &json!({"name": "Velvia", "groups": ["profile", "light"]})).unwrap()["id"].clone();
        select(&mut s, &[2, 3, 4]);
        s.execute("preset.apply", &json!({"id": preset, "amount": 100})).unwrap();
        check(&s, "preset", 0.5);
        s.execute("edit.undo", &json!({})).unwrap();

        // Auto Sync carries only the change, the look, and leaves the photos it doesn't reach without a history step
        select(&mut s, &[1]);
        s.execute("develop.profile", &json!({"id": "lc.color"})).unwrap();
        select(&mut s, &[1, 2, 3, 4]);
        let steps = |s: &crate::Session, id| s.catalog.photo(PhotoId(id)).unwrap().history.len();
        let before = (steps(&s, 3), steps(&s, 4));
        s.execute("develop.autoSync", &json!({"on": true})).unwrap();
        s.execute("develop.profile", &json!({"id": "look:x-t4/velvia"})).unwrap();
        check(&s, "auto sync", 0.0);
        assert_eq!((steps(&s, 3), steps(&s, 4)), before, "no step where nothing changed");
    }

    /// `profiles.menu` lists a look, in favourites, recent and its group, only while the active photo renders it.
    #[test]
    fn the_profile_menu_lists_looks_only_for_photos_that_render_them() {
        use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
        use serde_json::{Value, json};
        let mut s = crate::Session::new();
        for (id, file, camera) in [(1, "a.RAF", "FUJIFILM X-T4"), (2, "b.RAF", "FUJIFILM X-T3")] {
            let mut p = Photo::new(PhotoId(id), Source::File { path: file.into() }, file, "RAF", 6240, 4160, "2026-10-21");
            p.kind = MediaKind::Raw;
            p.meta.camera = camera.into();
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        let velvia = find("look:x-t4/velvia").unwrap();
        s.execute("library.select", &json!({"ids": [1]})).unwrap();
        s.execute("develop.profile", &json!({"id": velvia.id})).unwrap();
        s.execute("profile.favorite", &json!({"id": velvia.id, "favorite": true})).unwrap();
        let has = |v: &Value| v.as_array().unwrap().iter().any(|p| p["id"] == velvia.id.as_str());
        let looks_group = |m: &Value| m["groups"].as_array().unwrap().iter().find(|g| g["name"] == velvia.group.as_str()).cloned();

        let m = s.execute("profiles.menu", &json!({})).unwrap();
        assert!(has(&m["favorites"]) && has(&m["recent"]), "{m}");
        let group = looks_group(&m).expect("the looks' group for an X-T4 raw");
        assert_eq!(group["profiles"].as_array().unwrap().len(), 9);
        assert!(has(&group["profiles"]));

        s.execute("library.select", &json!({"ids": [2]})).unwrap();
        let m = s.execute("profiles.menu", &json!({})).unwrap();
        assert!(!has(&m["favorites"]) && !has(&m["recent"]), "another camera's raw: {m}");
        assert!(looks_group(&m).is_none());
    }

    /// A newer format, a node short, a non-hexadecimal node or no step is refused.
    #[test]
    fn damaged_look_files_are_refused() {
        let (_, json) = BUNDLED[0];
        assert!(parse(json, "test").is_ok());
        assert!(parse("{}", "test").is_err());
        assert!(parse(&json.replacen("\"version\": 1", "\"version\": 2", 1), "test").is_err(), "a newer format");
        let first_nodes = json.find("\"nodes\": \"").unwrap() + 10;
        let mut cut = json.to_string();
        cut.replace_range(first_nodes..first_nodes + 4, "");
        assert!(parse(&cut, "test").is_err(), "a node short");
        let mut garbled = json.to_string();
        garbled.replace_range(first_nodes..first_nodes + 4, "zzzz");
        assert!(parse(&garbled, "test").is_err(), "not hexadecimal");
        assert_eq!(decode_nodes("0000ffff8000", -4.0, 1.0 / 8192.0).unwrap(), vec![[-4.0, 4.0 - 1.0 / 8192.0, 0.0]]);
        assert!(decode_nodes("0000ffff8000", 0.0, 0.0).is_err(), "no step");
    }
}
