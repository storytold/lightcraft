//! Presets and profiles: see [`dac_engine_develop::presets`]; the session-side half lives here.

use dac_develop::Preset;
use serde_json::{Value, json};

pub use dac_engine_develop::presets::*;

use crate::Session;

impl Session {
    /// Add imported presets. Identical presets (same name, group and settings) already present are
    /// skipped; id clashes get a fresh id. Returns the ids added.
    pub fn add_presets(&mut self, presets: Vec<Preset>) -> Vec<String> {
        let mut added = Vec::new();
        for mut p in presets {
            if self.presets.iter().any(|q| q.name == p.name && q.group == p.group && q.settings == p.settings) {
                continue;
            }
            if p.id.trim().is_empty() || p.id.starts_with("lc.") || self.presets.iter().any(|q| q.id == p.id) {
                let base = format!("user.{}", slug(&format!("{}-{}", p.group, p.name)));
                let mut id = base.clone();
                let mut n = 2;
                while self.presets.iter().any(|q| q.id == id) {
                    id = format!("{base}-{n}");
                    n += 1;
                }
                p.id = id;
            }
            p.builtin = false;
            added.push(p.id.clone());
            self.presets.push(p);
        }
        added
    }
}

impl Session {
    /// A profile by id, looking up built-in profiles first and imported LUT profiles second.
    pub fn profile_info(&self, id: &str) -> Option<(&str, &str)> {
        if let Some(p) = profile(id) {
            return Some((p.name, p.group));
        }
        self.lut_profiles.iter().find(|p| p.id == id).map(|p| (p.name.as_str(), p.group.as_str()))
    }

    /// Remember `id` as the most recently applied profile.
    pub fn note_profile_used(&mut self, id: &str) {
        self.profile_recent.retain(|p| p != id);
        self.profile_recent.insert(0, id.to_string());
        self.profile_recent.truncate(RECENT_PROFILES);
    }

    /// The profile menu: favourites, recent, then every group (`profiles.menu`).
    pub fn profile_menu(&self) -> Value {
        let item =
            |p: &ProfileInfo| json!({"id": p.id, "name": p.name, "group": p.group, "favorite": self.profile_favorites.iter().any(|f| f == p.id)});
        let lut_item = |id: &str| {
            self.lut_profiles
                .iter()
                .find(|p| p.id == id)
                .map(|p| json!({"id": p.id, "name": p.name, "group": p.group, "favorite": self.profile_favorites.contains(&p.id), "imported": true}))
        };
        let list = |ids: &[String]| ids.iter().filter_map(|id| profile(id).map(item).or_else(|| lut_item(id))).collect::<Vec<_>>();
        let mut groups: Vec<Value> = profile_groups()
            .into_iter()
            .map(|g| json!({"name": g, "profiles": PROFILES.iter().filter(|p| p.group == g).map(item).collect::<Vec<_>>()}))
            .collect();
        // imported LUT profiles, by their groups
        let mut lut_groups: Vec<&str> = self.lut_profiles.iter().map(|p| p.group.as_str()).collect();
        lut_groups.sort_unstable();
        lut_groups.dedup();
        for g in lut_groups {
            let profiles: Vec<Value> = self
                .lut_profiles
                .iter()
                .filter(|p| p.group == g)
                .map(|p| json!({"id": p.id, "name": p.name, "group": p.group, "favorite": self.profile_favorites.contains(&p.id), "imported": true}))
                .collect();
            groups.push(json!({"name": g, "profiles": profiles, "imported": true}));
        }
        json!({
            "favorites": list(&self.profile_favorites),
            "recent": list(&self.profile_recent),
            "groups": groups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-presets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn creative_profiles_render_without_moving_the_sliders() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.4})).unwrap();
        let before = s.develop_of(id).unwrap();
        let plain = s.render_now(id, 96, 96).unwrap().image;
        s.execute("develop.profile", &json!({"id": "lc.cine.teal-amber", "amount": 150})).unwrap();
        let after = s.develop_of(id).unwrap();
        assert_eq!((after.profile.id.as_str(), after.profile.amount), ("lc.cine.teal-amber", 150.0));
        let mut same = (*after).clone();
        same.profile = before.profile.clone();
        assert_eq!(same, *before, "only the profile changed");
        let looked = s.render_now(id, 96, 96).unwrap().image;
        assert_ne!(plain.as_bytes(), looked.as_bytes());
    }

    #[test]
    fn lcpreset_export_import_roundtrip_keeps_groups() {
        let d = dir("rt");
        let mut s = Session::with_demo();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.6})).unwrap();
        s.execute("preset.create", &json!({"name": "Bright", "group": "Travel"})).unwrap();
        s.execute("develop.set", &json!({"control": "effects.clarity", "value": 30})).unwrap();
        s.execute("preset.create", &json!({"name": "Crisp", "group": "Street"})).unwrap();
        let mine: Vec<Preset> = s.presets.iter().filter(|p| !p.builtin).cloned().collect();
        assert_eq!(mine.len(), 2);
        let path = d.join("mine");
        let r = s.execute("preset.export", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(r["count"], 2);
        let file = d.join(format!("mine.{}", dac_brand::PRESET_EXT));
        assert!(file.is_file());
        let one = d.join("travel.lcpreset");
        s.execute("preset.export", &json!({"path": one.to_string_lossy(), "group": "Travel"})).unwrap();
        assert!(s.execute("preset.export", &json!({"path": one.to_string_lossy(), "group": "Nope"})).is_err());

        // a fresh session imports both files from the folder; the duplicate is skipped
        let mut t = Session::new();
        let r = t.execute("preset.import", &json!({"paths": [d.to_string_lossy()]})).unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 2, "{r}");
        assert_eq!(r["skipped"], 1, "{r}");
        for p in &mine {
            let q = t.presets.iter().find(|q| q.name == p.name).unwrap();
            assert_eq!((&q.group, &q.settings, q.builtin), (&p.group, &p.settings, false));
        }
        // importing into the session that has them: everything is skipped, nothing duplicated
        let n = s.presets.len();
        let r = s.execute("preset.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        assert_eq!((r["skipped"].as_u64(), s.presets.len()), (Some(2), n));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn id_clashes_get_fresh_ids_and_bad_files_are_reported() {
        let d = dir("clash");
        let builtin = builtin().remove(0);
        let mut clash = builtin.clone();
        clash.settings = json!({"light": {"exposure": 1.0}});
        std::fs::write(d.join("a.lcpreset"), serde_json::to_string(&clash).unwrap()).unwrap();
        std::fs::write(d.join("b.lcpreset"), "{not json").unwrap();
        std::fs::write(d.join("c.lcpreset"), r#"{"format": "other", "version": 1, "presets": []}"#).unwrap();
        std::fs::write(d.join("notes.txt"), "ignored inside folders").unwrap();
        let mut s = Session::new();
        let r = s.execute("preset.import", &json!({"paths": [d.to_string_lossy()]})).unwrap();
        assert_eq!(r["failed"].as_array().unwrap().len(), 2, "{r}");
        let id = r["imported"][0]["id"].as_str().unwrap();
        assert_ne!(id, builtin.id);
        assert!(id.starts_with("user."));
        assert!(s.presets.iter().find(|p| p.id == builtin.id).unwrap().builtin, "built-in untouched");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn xmp_preset_file_imports_and_applies() {
        let d = dir("xmp");
        // Hand-written for this test (not a third-party preset).
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
            crs:PresetType="Normal" crs:Exposure2012="+0.30" crs:Shadows2012="+20" crs:GrainAmount="12">
            <crs:Group><rdf:Alt><rdf:li xml:lang="x-default">Test Group</rdf:li></rdf:Alt></crs:Group>
          </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        std::fs::write(d.join("Soft Lift.xmp"), x).unwrap();
        let mut s = Session::with_demo();
        let r = s.execute("preset.import", &json!({"paths": [d.join("Soft Lift.xmp").to_string_lossy()]})).unwrap();
        let id = r["imported"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(r["imported"][0]["name"], "Soft Lift", "file name when crs:Name is missing");
        assert_eq!(r["imported"][0]["group"], "Test Group");
        s.execute("preset.apply", &json!({"id": id})).unwrap();
        let dv = s.develop_of(s.active().unwrap()).unwrap();
        assert_eq!((dv.light.exposure, dv.light.shadows, dv.grain.amount), (0.3, 20.0, 12.0));
        let _ = std::fs::remove_dir_all(&d);
    }
}
