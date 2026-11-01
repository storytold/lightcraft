//! Profiles named by imported presets (issue #1001).
//!
//! A Lightroom preset doesn't carry its profile's look: it names it (`crs:Look`: name, group,
//! UUID, amount), and many presets get most of their look from that profile. On import the
//! reference is kept in the preset's settings as `profile.look`, and resolved against the
//! profiles the user has imported (by name, the group breaking ties), so the preset applies the
//! profile like the other app does. The six basic profiles of the other app map to our own basic
//! profiles; its other built-in profiles can't be used here and the import says so.
//!
//! References are resolved again whenever profiles are imported or deleted, so a pack works in
//! either order. Only the user's own files are read; no profile content ships with LightCraft.

use serde_json::{Value, json};

use crate::Session;
use crate::cmd::lut_profiles::LutProfile;
use crate::crs::Props;

/// The profile named by a preset's `crs:Look`, as kept in its settings (`profile.look`):
/// `{name, group?, uuid?, amount}` (amount in percent, like [`lightcraft_develop::Profile`]).
pub fn look_ref(props: &Props) -> Option<Value> {
    let field = |k: &str| props.get(&format!("crs:Look/crs:{k}")).and_then(|v| v.first()).map(|s| s.trim()).filter(|s| !s.is_empty());
    let name = field("Name")?;
    // stored as a fraction (1 = 100 %), 0–2
    let amount = field("Amount").and_then(|a| a.parse::<f64>().ok()).filter(|a| a.is_finite()).map_or(100.0, |a| (a * 100.0).clamp(0.0, 200.0));
    let mut look = json!({"name": name, "amount": amount});
    if let Some(g) = field("Group") {
        look["group"] = json!(g);
    }
    if let Some(u) = field("UUID") {
        look["uuid"] = json!(u.to_ascii_lowercase());
    }
    Some(look)
}

/// What a preset's profile reference resolved to.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved {
    /// A profile the user imported (`lut:…`).
    Imported(String),
    /// One of our built-in profiles standing in for the other app's basic profile of that name.
    BuiltIn(String),
    /// One of the other app's own creative or camera profiles: not available here.
    OtherAppBuiltIn,
    /// A third-party profile the user hasn't imported (yet).
    NotImported,
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The other app's basic profiles and ours that stand in for them.
const BASIC: &[(&str, &str)] = &[
    ("adobe color", "lc.color"),
    ("adobe neutral", "lc.neutral"),
    ("adobe vivid", "lc.vivid"),
    ("adobe landscape", "lc.landscape"),
    ("adobe portrait", "lc.portrait"),
    ("adobe monochrome", "lc.mono"),
];

/// The other app's own profiles: everything named `Adobe …` or `Camera …`, and its numbered
/// creative profiles (`Artistic 01`, `B&W 05`, `Modern 10`, `Vintage 02`).
fn other_app_builtin(name: &str) -> bool {
    let n = norm(name);
    if n.starts_with("adobe ") || n.starts_with("camera ") {
        return true;
    }
    ["artistic ", "b&w ", "modern ", "vintage "]
        .iter()
        .any(|p| n.strip_prefix(p).is_some_and(|rest| !rest.is_empty() && rest.len() <= 2 && rest.chars().all(|c| c.is_ascii_digit())))
}

/// Resolve `look` (a `profile.look` value) against the imported profiles.
pub fn resolve(look: &Value, luts: &[LutProfile]) -> Resolved {
    let name = norm(look.get("name").and_then(Value::as_str).unwrap_or_default());
    let group = look.get("group").and_then(Value::as_str).map(norm);
    if !name.is_empty() {
        let same_name: Vec<&LutProfile> = luts.iter().filter(|p| norm(&p.name) == name).collect();
        let pick = same_name.iter().find(|p| group.as_deref() == Some(norm(&p.group).as_str())).or(same_name.first());
        if let Some(p) = pick {
            return Resolved::Imported(p.id.clone());
        }
    }
    if let Some((_, ours)) = BASIC.iter().find(|(theirs, _)| *theirs == name) {
        return Resolved::BuiltIn((*ours).to_string());
    }
    if other_app_builtin(&name) { Resolved::OtherAppBuiltIn } else { Resolved::NotImported }
}

/// Set (or clear) `profile.id` / `profile.amount` in a preset's partial settings from its
/// `profile.look`. Returns what the reference resolved to (`None`: the preset names no profile)
/// and whether `settings` changed.
pub fn apply(settings: &mut Value, luts: &[LutProfile]) -> Option<(Resolved, bool)> {
    let profile = settings.get_mut("profile")?.as_object_mut()?;
    let look = profile.get("look")?.clone();
    let r = resolve(&look, luts);
    let before = (profile.get("id").cloned(), profile.get("amount").cloned());
    match &r {
        Resolved::Imported(id) | Resolved::BuiltIn(id) => {
            profile.insert("id".into(), json!(id));
            profile.insert("amount".into(), look.get("amount").cloned().unwrap_or(json!(100.0)));
        }
        // the photo keeps its own profile
        Resolved::OtherAppBuiltIn | Resolved::NotImported => {
            profile.remove("id");
            profile.remove("amount");
        }
    }
    let changed = before != (profile.get("id").cloned(), profile.get("amount").cloned());
    Some((r, changed))
}

/// How the import reports a reference it couldn't resolve (one of the preset's `unmapped` names).
pub fn report(look: &Value, r: &Resolved) -> Option<String> {
    let name = look.get("name").and_then(Value::as_str).unwrap_or_default();
    match r {
        Resolved::Imported(_) | Resolved::BuiltIn(_) => None,
        Resolved::OtherAppBuiltIn => Some(format!("Profile “{name}” (Lightroom's own)")),
        Resolved::NotImported => Some(format!("Profile “{name}” (not imported yet)")),
    }
}

/// Resolve every user preset's profile reference again (after profiles were imported or deleted).
/// Only the presets that change are touched, so an unchanged list isn't saved again.
pub fn refresh(s: &mut Session) {
    let luts = s.lut_profiles.clone();
    let stale: Vec<usize> = s
        .presets
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.builtin)
        .filter(|(_, p)| {
            let mut settings = p.settings.clone();
            apply(&mut settings, &luts).is_some_and(|(_, changed)| changed)
        })
        .map(|(i, _)| i)
        .collect();
    if stale.is_empty() {
        return;
    }
    let presets = &mut *s.presets;
    for i in stale {
        if let Some(p) = presets.get_mut(i) {
            apply(&mut p.settings, &luts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lut(id: &str, name: &str, group: &str) -> LutProfile {
        LutProfile { id: id.into(), name: name.into(), group: group.into(), file: String::new() }
    }

    #[test]
    fn resolves_by_name_with_the_group_breaking_ties() {
        let luts = [lut("lut:a", "Kodak Portra 400", "Films A"), lut("lut:b", "Kodak  portra 400", "RNI Films")];
        let look = json!({"name": "Kodak Portra 400", "group": "RNI Films", "amount": 100.0});
        assert_eq!(resolve(&look, &luts), Resolved::Imported("lut:b".into()));
        let look = json!({"name": "kodak portra 400", "amount": 100.0});
        assert_eq!(resolve(&look, &luts), Resolved::Imported("lut:a".into()), "first by name without a group");
        assert_eq!(resolve(&json!({"name": "Fuji 400H"}), &luts), Resolved::NotImported);
    }

    #[test]
    fn other_app_profiles_map_to_ours_or_are_reported() {
        assert_eq!(resolve(&json!({"name": "Adobe Monochrome"}), &[]), Resolved::BuiltIn("lc.mono".into()));
        for n in ["Adobe Standard", "Camera Faithful", "Artistic 01", "B&W 12", "Vintage 3"] {
            assert_eq!(resolve(&json!({"name": n}), &[]), Resolved::OtherAppBuiltIn, "{n}");
        }
        assert_eq!(resolve(&json!({"name": "Vintage Film"}), &[]), Resolved::NotImported);
        // an imported profile wins over the mapping
        assert_eq!(resolve(&json!({"name": "Adobe Vivid"}), &[lut("lut:v", "Adobe Vivid", "Mine")]), Resolved::Imported("lut:v".into()));
    }

    #[test]
    fn apply_sets_and_clears_the_profile() {
        let mut s = json!({"profile": {"look": {"name": "mb1 profile", "amount": 80.0}}, "light": {"highlights": -6.0}});
        assert_eq!(apply(&mut s, &[]), Some((Resolved::NotImported, false)));
        let luts = [lut("lut:mb1", "mb1 profile", "Imported")];
        assert_eq!(apply(&mut s, &luts), Some((Resolved::Imported("lut:mb1".into()), true)));
        assert_eq!((s["profile"]["id"].clone(), s["profile"]["amount"].clone()), (json!("lut:mb1"), json!(80.0)));
        assert_eq!(apply(&mut s, &luts), Some((Resolved::Imported("lut:mb1".into()), false)), "already resolved");
        assert_eq!(apply(&mut s, &[]), Some((Resolved::NotImported, true)), "profile deleted");
        assert!(s["profile"].get("id").is_none());
        assert_eq!(apply(&mut json!({"light": {}}), &luts), None);
    }

    #[test]
    fn look_ref_reads_name_group_uuid_and_amount() {
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0">
           <crs:Look><rdf:Description crs:Name="mb1 profile" crs:Amount="0.75" crs:UUID="0A1B2C">
             <crs:Group><rdf:Alt><rdf:li xml:lang="x-default">Erich McVey B&amp;W Profiles</rdf:li></rdf:Alt></crs:Group>
           </rdf:Description></crs:Look>
          </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let props = lightcraft_meta::parse_xmp(x).unwrap().properties;
        assert_eq!(look_ref(&props), Some(json!({"name": "mb1 profile", "amount": 75.0, "group": "Erich McVey B&W Profiles", "uuid": "0a1b2c"})));
        assert_eq!(look_ref(&Props::new()), None);
    }
}
