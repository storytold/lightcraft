//! Before / After: what the "before" side shows. By default a photo's import state (camera
//! defaults plus the import preset, see [`Photo::import_defaults`]; without an import preset, Remove
//! Hot Pixels as the photo has it now, see `import_state`); it can be set from the
//! current settings, a history step or a version, and copied or swapped with the current
//! settings. The before side always uses the current crop and orientation so the two line up.

use std::sync::Arc;

use lightcraft_catalog::Photo;
use lightcraft_develop::DevelopSettings;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_active};
use crate::{Result, Session};

impl Session {
    /// The settings the "before" side of photo `p` renders with.
    pub fn before_settings(&self, p: &Photo) -> DevelopSettings {
        let base = self.before.get(&p.id).map(|b| (**b).clone()).unwrap_or_else(|| import_state(p));
        DevelopSettings { crop: p.develop.crop, orientation: p.develop.orientation, ..base }
    }
}

/// The before side when none was chosen: [`Photo::import_defaults`]. Without an import look the photo doesn't record
/// which Remove Hot Pixels default it was imported with (off before #1350, on since), so the box is taken from its
/// current settings: it never differs between the two sides, and an untouched raw is decoded once for both (#4256).
fn import_state(p: &Photo) -> DevelopSettings {
    let mut d = p.import_defaults();
    if p.import_look.is_none() {
        d.detail.nr_outliers = p.develop.detail.nr_outliers;
    }
    d
}

fn active(s: &Session, c: &str) -> Result<lightcraft_catalog::PhotoId> {
    s.active().ok_or_else(|| bad(c, "no active photo"))
}

fn state(s: &Session) -> Result<Value> {
    let custom = s.active().is_some_and(|id| s.before.contains_key(&id));
    Ok(json!({"before": if custom { "custom" } else { "import" }}))
}

/// `{source: import | current | history | version, index?, name?}`
fn set_before(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "beforeAfter.setBefore";
    let id = active(s, C)?;
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "no photo"))?;
    let settings: Option<Arc<DevelopSettings>> = match p.get("source").and_then(Value::as_str).unwrap_or("current") {
        "import" | "original" => None,
        "current" => Some(photo.develop.clone()),
        "history" => {
            let i = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(C, "history needs `index`"))? as usize;
            Some(photo.history.get(i).ok_or_else(|| bad(C, format!("no history step {i}")))?.settings.clone())
        }
        "version" => {
            let name = p.get("name").and_then(Value::as_str).ok_or_else(|| bad(C, "version needs `name`"))?;
            Some(photo.versions.iter().find(|v| v.name == name).ok_or_else(|| bad(C, format!("no version `{name}`")))?.settings.clone())
        }
        other => return Err(bad(C, format!("unknown source `{other}` (import, current, history, version)"))),
    };
    match settings {
        Some(b) => s.before.insert(id, b),
        None => s.before.remove(&id),
    };
    state(s)
}

/// Copy the before settings to the photo (an edit, undoable); `swap` also makes the old
/// current settings the new before.
fn before_to_after(s: &mut Session, swap: bool) -> Result<Value> {
    let c = if swap { "beforeAfter.swap" } else { "beforeAfter.copyBeforeToAfter" };
    let id = active(s, c)?;
    let photo = s.catalog.photo(id).ok_or_else(|| bad(c, "no photo"))?;
    let after = photo.develop.clone();
    let before = s.before.get(&id).map(|b| (**b).clone()).unwrap_or_else(|| import_state(photo));
    s.set_develop(id, before, if swap { "Swap Before and After" } else { "Copy Before to After" })?;
    if swap {
        s.before.insert(id, after);
    }
    state(s)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "beforeAfter.setBefore",
            "Set Before",
            [],
            None,
            "{source: import|current|history|version, index? (history step), name? (version)} — what the before side shows (this session)",
            has_active,
            set_before
        ),
        cmd!(
            "beforeAfter.copyAfterToBefore",
            "Copy After's Settings to Before",
            ["View", "Before/After Settings"],
            None,
            "{}",
            has_active,
            |s, _| { set_before(s, &json!({"source": "current"})) }
        ),
        cmd!(
            "beforeAfter.copyBeforeToAfter",
            "Copy Before's Settings to After",
            ["View", "Before/After Settings"],
            None,
            "{}",
            has_active,
            |s, _| { before_to_after(s, false) }
        ),
        cmd!("beforeAfter.swap", "Swap Before and After Settings", ["View", "Before/After Settings"], None, "{}", has_active, |s, _| {
            before_to_after(s, true)
        }),
        cmd!("beforeAfter.resetBefore", "Reset Before to Import State", ["View", "Before/After Settings"], None, "{}", has_active, |s, _| {
            set_before(s, &json!({"source": "import"}))
        }),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
    use lightcraft_pipeline::SourceInfo;
    use lightcraft_raster::Rgb32f;
    use serde_json::json;

    use crate::Session;

    /// An untouched raw whose Remove Hot Pixels box is `on`, imported with an import look whose box is `look` when
    /// that is `Some`, and a log of how its file was decoded.
    fn untouched_raw(on: bool, look: Option<bool>) -> (Session, Arc<Mutex<Vec<bool>>>) {
        let mut s = Session::new();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        s.media.file_loader = Some(Arc::new(move |_, _, with: crate::media::RawDecode<'_>| {
            log.lock().unwrap().push(with.outliers);
            Ok((Rgb32f::new(8, 8), SourceInfo { raw: true, ..Default::default() }))
        }));
        let mut p = Photo::new(PhotoId(1), Source::File { path: "a.cr3".into() }, "a.cr3", "CR3", 6000, 4000, "2026-01-01");
        p.kind = MediaKind::Raw;
        // a CR3 of a camera without measured colour matrices has a relative white balance, so its camera defaults
        // are `DevelopSettings::for_raw`'s, with Remove Hot Pixels on
        assert!(p.relative_wb() && p.camera_defaults().detail.nr_outliers);
        let mut d = p.camera_defaults();
        d.detail.nr_outliers = on;
        p.develop = Arc::new(d);
        if let Some(look_on) = look {
            let mut l = p.camera_defaults();
            l.detail.nr_outliers = look_on;
            p.import_look = Some(Arc::new(l));
        }
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        s.execute("library.select", &json!({"ids": [1], "active": 1})).unwrap();
        (s, asked)
    }

    /// Issue #4256: a raw imported before Remove Hot Pixels was on by default (#1350) has the box off. Its default
    /// Before agrees with the photo on the box, so the two sides differ only by edits and the raw is decoded once
    /// for both; Copy Before to After and Swap leave the box as it is. A raw imported since has it on, on both sides.
    #[test]
    fn the_default_before_takes_remove_hot_pixels_from_the_photo() {
        for on in [false, true] {
            let what = if on { "imported with the box on" } else { "imported before the box" };
            let (mut s, asked) = untouched_raw(on, None);
            let photo = s.catalog.photo(PhotoId(1)).unwrap().clone();
            let before = s.before_settings(&photo);
            assert_eq!(before.detail.nr_outliers, on, "{what}");
            assert_eq!(before, *photo.develop, "{what}: an untouched raw looks the same on both sides");
            for side in [false, true] {
                let r = s.render_job(PhotoId(1), 64, 64, side, true).unwrap().run();
                s.accept(&r);
            }
            assert_eq!(*asked.lock().unwrap(), [on], "{what}: decoded once for both sides");
            s.execute("develop.set", &json!({"values": {"light.exposure": 1.0}})).unwrap();
            s.execute("beforeAfter.swap", &json!({})).unwrap();
            let d = s.develop_of(PhotoId(1)).unwrap();
            assert_eq!((d.light.exposure, d.detail.nr_outliers), (0.0, on), "{what}: swapped");
            s.execute("beforeAfter.resetBefore", &json!({})).unwrap();
            s.execute("develop.set", &json!({"values": {"light.exposure": 1.0}})).unwrap();
            s.execute("beforeAfter.copyBeforeToAfter", &json!({})).unwrap();
            let d = s.develop_of(PhotoId(1)).unwrap();
            assert_eq!((d.light.exposure, d.detail.nr_outliers), (0.0, on), "{what}: copied");
        }
    }

    /// A photo imported with an import look (a default preset) records the box it was imported with: the default
    /// Before keeps the look's box, not the photo's current one.
    #[test]
    fn an_import_look_keeps_its_own_remove_hot_pixels() {
        for look in [false, true] {
            let (s, _) = untouched_raw(!look, Some(look));
            let photo = s.catalog.photo(PhotoId(1)).unwrap().clone();
            assert_eq!(s.before_settings(&photo).detail.nr_outliers, look, "look with the box {look}");
        }
    }
}
