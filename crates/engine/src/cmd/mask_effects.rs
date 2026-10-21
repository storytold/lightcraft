//! Mask effect presets: named sets of local adjustment sliders picked from the Masking panel's
//! Effect menu (Lightroom's local adjustment / brush presets), saved with the library preferences.
//! Grouped (folders in the menu), favourites, partial presets (only some sliders, with or without
//! Amount) and optional brush settings. Files: [`super::mask_effect_files`].

use std::cmp::Ordering;

use lightcraft_catalog::PhotoId;
use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::mask_effect_files as files;
use super::{CommandSpec, always, bad, cmd, has_active, str_param};
use crate::{Result, Session};

/// The local sliders an effect can set (`LocalAdjustments` field names; Amount aside).
pub const FIELDS: &[&str] = &[
    "temp",
    "tint",
    "exposure",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
    "texture",
    "clarity",
    "dehaze",
    "hue",
    "saturation",
    "sharpness",
    "noise",
    "moire",
    "defringe",
    "color_hue",
    "color_sat",
];

/// Brush settings an effect can carry (Lightroom keeps these per brush, not in presets).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushSettings {
    /// Radius as a fraction of the image's long edge.
    pub size: f64,
    /// 0..100.
    pub feather: f64,
    pub flow: f64,
    pub density: f64,
    pub auto_mask: bool,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self { size: 0.04, feather: 50.0, flow: 100.0, density: 100.0, auto_mask: false }
    }
}

impl BrushSettings {
    fn clamped(self) -> Self {
        let c = |v: f64, lo: f64, hi: f64, d: f64| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let d = Self::default();
        Self {
            size: c(self.size, 0.002, 0.5, d.size),
            feather: c(self.feather, 0.0, 100.0, d.feather),
            flow: c(self.flow, 1.0, 100.0, d.flow),
            density: c(self.density, 1.0, 100.0, d.density),
            auto_mask: self.auto_mask,
        }
    }
}

/// A named set of local adjustment sliders. Applying one sets the sliders it names (all of them
/// when `fields` is empty, as Lightroom's presets do: those it doesn't change go to 0); the mask's
/// Amount stays unless `with_amount`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaskEffect {
    pub name: String,
    pub adjust: LocalAdjustments,
    /// The Effect menu folder ("" = none).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub favorite: bool,
    /// The sliders this effect sets ([`FIELDS`] names); empty = all of them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// Applying sets the mask's Amount to `adjust.amount` too.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub with_amount: bool,
    /// Brush settings the brush takes when the effect is applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brush: Option<BrushSettings>,
    /// Settings of an imported preset that we have no slider for (Lightroom key → value), kept so
    /// an export carries them.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Default for MaskEffect {
    fn default() -> Self {
        Self {
            name: String::new(),
            adjust: LocalAdjustments::default(),
            group: String::new(),
            favorite: false,
            fields: Vec::new(),
            with_amount: false,
            brush: None,
            extra: Map::new(),
        }
    }
}

/// Field `k` of `a` (0 for an unknown name).
pub fn get(a: &LocalAdjustments, k: &str) -> f64 {
    serde_json::to_value(a).ok().and_then(|v| v.get(k).and_then(Value::as_f64)).unwrap_or(0.0)
}

/// `a` with field `k` set to `v` (unchanged for an unknown name).
pub fn set(a: &LocalAdjustments, k: &str, v: f64) -> LocalAdjustments {
    let Ok(Value::Object(mut o)) = serde_json::to_value(a) else { return *a };
    if !o.contains_key(k) || !v.is_finite() {
        return *a;
    }
    o.insert(k.into(), json!(v));
    serde_json::from_value(Value::Object(o)).unwrap_or(*a)
}

/// The sliders of `a` away from 0 (the colour's hue only with its saturation).
pub fn changed_fields(a: &LocalAdjustments) -> Vec<String> {
    let mut out: Vec<String> = FIELDS.iter().filter(|k| get(a, k).abs() > 1e-9 && **k != "color_hue").map(|k| k.to_string()).collect();
    if a.color_sat.abs() > 1e-9 {
        out.push("color_hue".into());
    }
    out
}

impl MaskEffect {
    /// The sliders this effect sets.
    pub fn field_list(&self) -> Vec<&str> {
        if self.fields.is_empty() { FIELDS.to_vec() } else { FIELDS.iter().copied().filter(|k| self.fields.iter().any(|f| f == k)).collect() }
    }
    /// `a` with this preset's sliders (and Amount, if it carries it).
    pub fn apply(&self, a: &LocalAdjustments) -> LocalAdjustments {
        let mut out = if self.fields.is_empty() {
            LocalAdjustments { amount: a.amount, ..self.adjust }
        } else {
            self.field_list().iter().fold(*a, |acc, k| set(&acc, k, get(&self.adjust, k)))
        };
        if self.with_amount {
            out.amount = self.adjust.amount;
        }
        out
    }
    /// Does `a` carry exactly this preset's sliders? A tint's hue doesn't count without its
    /// saturation (it does nothing then).
    pub fn matches(&self, a: &LocalAdjustments) -> bool {
        let fields = self.field_list();
        let colorless = |x: &LocalAdjustments| x.color_sat.abs() < 1e-9;
        let same = |k: &str| (get(&self.adjust, k) - get(a, k)).abs() < 1e-6;
        fields.iter().all(|k| *k == "color_hue" && colorless(&self.adjust) && colorless(a) || same(k))
            && (!self.with_amount || (self.adjust.amount - a.amount).abs() < 1e-6)
    }
    /// Does the effect set nothing (all its sliders at 0, nothing kept from an import)?
    pub fn is_empty(&self) -> bool {
        self.extra.is_empty() && changed_fields(&self.adjust).iter().all(|k| !self.field_list().contains(&k.as_str()))
    }
    /// The same effect with its values in range and its field list normalised.
    pub fn normalized(mut self) -> Self {
        self.name = self.name.trim().to_string();
        self.group = self.group.trim().to_string();
        let amount = if self.with_amount { self.adjust.amount } else { 100.0 };
        self.adjust = super::masks::clamp_local(LocalAdjustments { amount, ..self.adjust });
        self.fields.retain(|f| FIELDS.contains(&f.as_str()));
        if self.fields.iter().any(|f| f == "color_sat") && !self.fields.iter().any(|f| f == "color_hue") {
            self.fields.push("color_hue".into());
        }
        self.fields.sort_by_key(|f| FIELDS.iter().position(|k| k == f));
        self.fields.dedup();
        if self.fields.len() == FIELDS.len() {
            self.fields.clear();
        }
        self.brush = self.brush.map(BrushSettings::clamped);
        self
    }
}

/// Compare names the way people number their presets: digit runs as numbers ("2 …" before
/// "10 …"), letters ignoring case.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let d = c.is_ascii_digit();
            match out.last_mut() {
                Some((kind, text)) if *kind == d => text.push(c),
                _ => out.push((d, c.to_string())),
            }
        }
        out
    }
    let (x, y) = (chunks(a), chunks(b));
    for (p, q) in x.iter().zip(y.iter()) {
        let o = match (p, q) {
            ((true, m), (true, n)) => {
                let (m, n) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                m.len().cmp(&n.len()).then_with(|| m.cmp(n))
            }
            ((_, m), (_, n)) => m.to_lowercase().cmp(&n.to_lowercase()),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    x.len().cmp(&y.len()).then_with(|| a.cmp(b))
}

/// The Effect menu's order: groups in the order they first appear (ungrouped first), each with
/// its effects (indices into `effects`) sorted by name.
pub fn menu_order(effects: &[MaskEffect]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    if effects.iter().any(|e| e.group.is_empty()) {
        groups.push((String::new(), Vec::new()));
    }
    for (i, e) in effects.iter().enumerate() {
        match groups.iter_mut().find(|(g, _)| g.eq_ignore_ascii_case(&e.group)) {
            Some((_, v)) => v.push(i),
            None => groups.push((e.group.clone(), vec![i])),
        }
    }
    for (_, v) in &mut groups {
        v.sort_by(|a, b| match (effects.get(*a), effects.get(*b)) {
            (Some(x), Some(y)) => natural_cmp(&x.name, &y.name),
            _ => Ordering::Equal,
        });
    }
    groups
}

/// Name of the effect the mask adjustments `a` match, if any (full presets before partial ones).
pub fn matching_effect(s: &Session, a: &LocalAdjustments) -> Option<String> {
    let full = s.mask_effects.iter().filter(|e| e.fields.is_empty()).find(|e| e.matches(a));
    full.or_else(|| s.mask_effects.iter().find(|e| e.matches(a))).map(|e| e.name.clone())
}

fn position(s: &Session, name: &str) -> Option<usize> {
    s.mask_effects.iter().position(|x| x.name.eq_ignore_ascii_case(name.trim()))
}

/// Add or replace (same name, ignoring case) an effect; a replaced one keeps its star.
pub(crate) fn upsert(s: &mut Session, e: MaskEffect) {
    match position(s, &e.name).and_then(|i| s.mask_effects.get_mut(i)) {
        Some(x) => {
            let favorite = x.favorite || e.favorite;
            *x = MaskEffect { favorite, ..e };
        }
        None => s.mask_effects.push(e),
    }
}

fn name_param(p: &Value, c: &str) -> Result<String> {
    name_of(p, "name", c)
}

fn name_of(p: &Value, key: &str, c: &str) -> Result<String> {
    str_param(p, key).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).ok_or_else(|| bad(c, format!("missing `{key}`")))
}

fn strings(p: &Value, key: &str) -> Option<Vec<String>> {
    p.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

/// The names a command acts on: `names: [...]` or `name`.
fn names_param(p: &Value, c: &str) -> Result<Vec<String>> {
    match strings(p, "names").filter(|v| !v.is_empty()) {
        Some(v) => Ok(v),
        None => name_param(p, c).map(|n| vec![n]),
    }
}

fn effect_index(s: &Session, name: &str, c: &str) -> Result<usize> {
    position(s, name).ok_or_else(|| bad(c, format!("no effect `{name}`")))
}

/// The selected mask's adjustments on the active photo.
fn selected_mask(s: &Session, p: &Value, c: &str) -> Result<Mask> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let mid = p
        .get("id")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .or(s.active_mask)
        .ok_or_else(|| bad(c, "no mask selected (give `id`)"))?;
    let d = s.develop_of(id).unwrap_or_default();
    d.masks.iter().find(|m| m.id == mid).cloned().ok_or_else(|| bad(c, format!("no mask {mid}")))
}

/// The kind of a mask's first component (`brush`, `linear`, `radial`, `subject`…): the tool the
/// effect is remembered for.
pub fn mask_tool(m: &Mask) -> Option<String> {
    m.components.first().and_then(|c| serde_json::to_value(&c.shape).ok()).and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_string))
}

/// The adjustments a new mask made with `tool` starts with: the effect last applied to a mask of
/// that kind (Lightroom carries the chosen effect over to the next mask), else all at 0.
pub fn new_mask_adjust(s: &Session, tool: &str) -> LocalAdjustments {
    s.mask_tool_effects
        .get(tool)
        .and_then(|name| position(s, name))
        .and_then(|i| s.mask_effects.get(i))
        .map(|e| super::masks::clamp_local(e.apply(&LocalAdjustments::default())))
        .unwrap_or_default()
}

fn list(s: &Session) -> Value {
    let current = s
        .active()
        .and_then(|id| s.develop_of(id))
        .and_then(|d| s.active_mask.and_then(|mid| d.masks.iter().find(|m| m.id == mid).map(|m| m.adjust)))
        .and_then(|a| matching_effect(s, &a));
    let groups: Vec<Value> = menu_order(&s.mask_effects)
        .into_iter()
        .map(|(g, ix)| json!({"name": g, "effects": ix.iter().filter_map(|i| s.mask_effects.get(*i)).map(|e| e.name.clone()).collect::<Vec<_>>()}))
        .collect();
    json!({"effects": s.mask_effects, "groups": groups, "current": current, "toolEffects": s.mask_tool_effects})
}

/// Masks on `d` named like one of `names` (ignoring case).
fn masks_named(d: &DevelopSettings, names: &[String]) -> Vec<u32> {
    d.masks.iter().filter(|m| names.iter().any(|n| n.eq_ignore_ascii_case(&m.name))).map(|m| m.id).collect()
}

fn apply_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.applyEffect";
    let name = name_param(p, C)?;
    let effect = s.mask_effects.get(effect_index(s, &name, C)?).cloned().ok_or_else(|| bad(C, format!("no effect `{name}`")))?;
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let d = s.develop_of(id).unwrap_or_default();
    // the masks: `ids`, `all`, else `id` or the selected one
    let ids: Vec<u32> = if p.get("all").and_then(Value::as_bool).unwrap_or(false) {
        d.masks.iter().map(|m| m.id).collect()
    } else if let Some(a) = p.get("ids").and_then(Value::as_array) {
        a.iter().filter_map(Value::as_u64).filter_map(|v| u32::try_from(v).ok()).collect()
    } else {
        vec![selected_mask(s, p, C)?.id]
    };
    if ids.is_empty() {
        return Err(bad(C, "no masks to apply the effect to"));
    }
    if let Some(missing) = ids.iter().find(|m| !d.masks.iter().any(|x| x.id == **m)) {
        return Err(bad(C, format!("no mask {missing}")));
    }
    let set_masks = |d: &DevelopSettings, ids: &[u32]| {
        let mut d = d.clone();
        for m in d.masks.iter_mut().filter(|m| ids.contains(&m.id)) {
            m.adjust = super::masks::clamp_local(effect.apply(&m.adjust));
        }
        d
    };
    let label = format!("Mask Effect: {}", effect.name);
    // with `photos: "selected"`, the other selected photos' masks of the same names too
    let others: Vec<(PhotoId, DevelopSettings)> = if str_param(p, "photos") == Some("selected") {
        let names: Vec<String> = d.masks.iter().filter(|m| ids.contains(&m.id)).map(|m| m.name.clone()).collect();
        s.selection
            .ids
            .iter()
            .filter(|x| **x != id)
            .filter_map(|x| s.develop_of(*x).map(|o| (*x, o)))
            .filter_map(|(x, o)| {
                let found = masks_named(&o, &names);
                (!found.is_empty()).then(|| (x, set_masks(&o, &found)))
            })
            .collect()
    } else {
        Vec::new()
    };
    let mine = set_masks(&d, &ids);
    let photos = 1 + others.len();
    if others.is_empty() {
        s.set_develop(id, mine, &label)?;
    } else {
        // one undo step for every photo
        let mut ops: Vec<_> = s.develop_op(id, mine, &label).into_iter().collect();
        ops.extend(others.into_iter().filter_map(|(x, o)| s.develop_op(x, o, &label)));
        s.commit(&label, lightcraft_catalog::Op::Batch { ops })?;
    }
    // the next mask made with this tool starts with the effect
    if p.get("remember").and_then(Value::as_bool).unwrap_or(true) {
        for m in d.masks.iter().filter(|m| ids.contains(&m.id)) {
            if let Some(tool) = mask_tool(m) {
                s.mask_tool_effects.insert(tool, effect.name.clone());
            }
        }
        s.save_prefs()?;
    }
    Ok(json!({"name": effect.name, "masks": ids, "photos": photos, "brush": effect.brush}))
}

/// The fields a save keeps: `fields: [..]` | `"changed"` | `"all"` (default).
fn fields_param(p: &Value, a: &LocalAdjustments, c: &str) -> Result<Vec<String>> {
    match p.get("fields") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(x)) if x == "all" => Ok(Vec::new()),
        Some(Value::String(x)) if x == "changed" => {
            let f = changed_fields(a);
            if f.is_empty() { Err(bad(c, "the mask changes no slider; nothing to save")) } else { Ok(f) }
        }
        Some(Value::Array(v)) => {
            let f: Vec<String> = v.iter().filter_map(Value::as_str).filter(|k| FIELDS.contains(k)).map(str::to_string).collect();
            if f.is_empty() { Err(bad(c, format!("`fields` names no slider (one of {})", FIELDS.join(", ")))) } else { Ok(f) }
        }
        Some(_) => Err(bad(c, "`fields` is \"all\", \"changed\" or a list of slider names")),
    }
}

fn brush_param(p: &Value, c: &str) -> Result<Option<BrushSettings>> {
    match p.get("brush") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(None),
        Some(v) => serde_json::from_value::<BrushSettings>(v.clone()).map(|b| Some(b.clamped())).map_err(|e| bad(c, format!("`brush`: {e}"))),
    }
}

fn save_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.saveEffect";
    let name = name_param(p, C)?;
    let adjust = selected_mask(s, p, C)?.adjust;
    let group = str_param(p, "group").map(|g| g.trim().to_string()).unwrap_or_default();
    let e = MaskEffect {
        name,
        adjust,
        group,
        fields: fields_param(p, &adjust, C)?,
        with_amount: p.get("amount").and_then(Value::as_bool).unwrap_or(false),
        brush: brush_param(p, C)?,
        ..Default::default()
    };
    upsert(s, e.normalized());
    s.save_prefs()?;
    Ok(list(s))
}

/// Overwrite an effect's sliders with the selected mask's (keeping which sliders and whether
/// Amount it carries, its group and star).
fn update_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.updateEffect";
    let name = name_param(p, C)?;
    let i = effect_index(s, &name, C)?;
    let adjust = selected_mask(s, p, C)?.adjust;
    let brush = brush_param(p, C)?;
    let e = s.mask_effects.get_mut(i).ok_or_else(|| bad(C, format!("no effect `{name}`")))?;
    let mut next = MaskEffect { adjust, extra: Map::new(), ..e.clone() };
    if let Some(f) = p.get("fields").filter(|v| !v.is_null()) {
        next.fields = fields_param(&json!({"fields": f}), &adjust, C)?;
    }
    if let Some(a) = p.get("amount").and_then(Value::as_bool) {
        next.with_amount = a;
    }
    if brush.is_some() {
        next.brush = brush;
    }
    *e = next.normalized();
    s.save_prefs()?;
    Ok(list(s))
}

fn rename_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.renameEffect";
    let name = name_param(p, C)?;
    let to = name_of(p, "to", C)?;
    let i = effect_index(s, &name, C)?;
    if position(s, &to).is_some_and(|j| j != i) {
        return Err(bad(C, format!("an effect named `{to}` exists already")));
    }
    if let Some(e) = s.mask_effects.get_mut(i) {
        e.name = to.clone();
    }
    for v in s.mask_tool_effects.values_mut() {
        if v.eq_ignore_ascii_case(&name) {
            *v = to.clone();
        }
    }
    s.save_prefs()?;
    Ok(list(s))
}

/// A name like `base` that no effect has: `base`, `base 2`, `base 3`…
fn free_name(s: &Session, base: &str) -> String {
    (1..10_000)
        .map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") })
        .find(|n| position(s, n).is_none())
        .unwrap_or_else(|| base.into())
}

fn duplicate_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.duplicateEffect";
    let name = name_param(p, C)?;
    let e = s.mask_effects.get(effect_index(s, &name, C)?).cloned().ok_or_else(|| bad(C, format!("no effect `{name}`")))?;
    let to = match str_param(p, "to").map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if position(s, t).is_some() => return Err(bad(C, format!("an effect named `{t}` exists already"))),
        Some(t) => t.to_string(),
        None => free_name(s, &format!("{} copy", e.name)),
    };
    s.mask_effects.push(MaskEffect { name: to.clone(), favorite: false, ..e });
    s.save_prefs()?;
    let mut out = list(s);
    out["name"] = json!(to);
    Ok(out)
}

fn delete_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.deleteEffect";
    let names = names_param(p, C)?;
    if let Some(missing) = names.iter().find(|n| position(s, n).is_none()) {
        return Err(bad(C, format!("no effect `{missing}`")));
    }
    s.mask_effects.retain(|x| !names.iter().any(|n| n.eq_ignore_ascii_case(&x.name)));
    s.mask_tool_effects.retain(|_, v| !names.iter().any(|n| n.eq_ignore_ascii_case(v)));
    s.save_prefs()?;
    Ok(list(s))
}

fn favorite_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.favoriteEffect";
    let names = names_param(p, C)?;
    let mut idx = Vec::new();
    for n in &names {
        idx.push(effect_index(s, n, C)?);
    }
    let want = p.get("favorite").and_then(Value::as_bool);
    for i in idx {
        if let Some(e) = s.mask_effects.get_mut(i) {
            e.favorite = want.unwrap_or(!e.favorite);
        }
    }
    s.save_prefs()?;
    Ok(list(s))
}

fn set_group(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.setEffectGroup";
    let names = names_param(p, C)?;
    let group = str_param(p, "group").map(|g| g.trim().to_string()).ok_or_else(|| bad(C, "missing `group` (\"\" = none)"))?;
    let mut idx = Vec::new();
    for n in &names {
        idx.push(effect_index(s, n, C)?);
    }
    // an existing group's spelling wins
    let group = s.mask_effects.iter().map(|e| &e.group).find(|g| g.eq_ignore_ascii_case(&group)).cloned().unwrap_or(group);
    for i in idx {
        if let Some(e) = s.mask_effects.get_mut(i) {
            e.group = group.clone();
        }
    }
    s.save_prefs()?;
    Ok(list(s))
}

fn rename_group(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.renameEffectGroup";
    let group = name_of(p, "group", C)?;
    let to = str_param(p, "to").map(|g| g.trim().to_string()).ok_or_else(|| bad(C, "missing `to` (\"\" = ungroup)"))?;
    let mut n = 0;
    for e in s.mask_effects.iter_mut().filter(|e| e.group.eq_ignore_ascii_case(&group)) {
        e.group = to.clone();
        n += 1;
    }
    if n == 0 {
        return Err(bad(C, format!("no group `{group}`")));
    }
    s.save_prefs()?;
    Ok(list(s))
}

fn tool_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.toolEffect";
    let tool = name_of(p, "tool", C)?;
    match str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => {
            let i = effect_index(s, n, C)?;
            let name = s.mask_effects.get(i).map(|e| e.name.clone()).unwrap_or_default();
            s.mask_tool_effects.insert(tool, name);
        }
        None => {
            s.mask_tool_effects.remove(&tool);
        }
    }
    s.save_prefs()?;
    Ok(json!({"toolEffects": s.mask_tool_effects}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "mask.effects",
            "Mask Effect Presets",
            [],
            None,
            "{} → {effects: [{name, adjust, group?, favorite?, fields?, with_amount?, brush?, extra?}], groups: [{name, effects: [name]}] (menu order), current: the effect the selected mask matches or null, toolEffects: {tool: effect a new mask of that kind starts with}}",
            always,
            |s, _| Ok(list(s))
        ),
        cmd!(
            "mask.applyEffect",
            "Apply Mask Effect",
            [],
            None,
            "{name, id? | ids?: [maskId] | all?: bool, photos?: \"selected\" (also the masks of the same names on the other selected photos, one undo step), remember?: bool (true: new masks of this kind start with it)} — sets the effect's sliders (all, or the ones it keeps; Amount only if it carries it) → {name, masks, photos, brush}",
            has_active,
            apply_effect
        ),
        cmd!(
            "mask.saveEffect",
            "Save Mask Effect",
            [],
            None,
            "{name, id?, group?, fields?: \"all\" (default) | \"changed\" | [slider], amount?: bool (carry Amount), brush?: {size, feather, flow, density, auto_mask}} — the selected mask's sliders as an effect preset (replaces one of that name), saved with the library",
            has_active,
            save_effect
        ),
        cmd!(
            "mask.updateEffect",
            "Update Mask Effect",
            [],
            None,
            "{name, id?, fields?, amount?, brush?} — overwrite the effect with the selected mask's sliders (keeps its group, star and which sliders it carries unless given)",
            has_active,
            update_effect
        ),
        cmd!("mask.renameEffect", "Rename Mask Effect", [], None, "{name, to}", always, rename_effect),
        cmd!(
            "mask.duplicateEffect",
            "Duplicate Mask Effect",
            [],
            None,
            "{name, to?} (default \"<name> copy\") → list + {name}",
            always,
            duplicate_effect
        ),
        cmd!("mask.deleteEffect", "Delete Mask Effect", [], None, "{name | names: [name]} — removes effect presets", always, delete_effect),
        cmd!("mask.favoriteEffect", "Favorite Mask Effect", [], None, "{name | names, favorite?: bool (default: toggle)}", always, favorite_effect),
        cmd!("mask.setEffectGroup", "Move Mask Effect to Group", [], None, "{name | names, group: name (\"\" = none)}", always, set_group),
        cmd!(
            "mask.renameEffectGroup",
            "Rename Mask Effect Group",
            [],
            None,
            "{group, to} (\"\" ungroups; an existing name merges)",
            always,
            rename_group
        ),
        cmd!(
            "mask.toolEffect",
            "Set Effect for New Masks",
            [],
            None,
            "{tool: brush|linear|radial|subject|…, name?} — the effect new masks of that kind start with (no name: none)",
            always,
            tool_effect
        ),
        cmd!(
            "mask.importEffects",
            "Import Mask Effects",
            [],
            None,
            "{path | paths: [file or folder], group?, dryRun?} — Lightroom Classic local adjustment presets (.lrtemplate), Camera Raw local correction presets (.xmp), .lceffect files and .zip bundles of these; folders and Lightroom's divider presets (\"10 ----- DETAIL ----- 10\") become groups; same-named effects are replaced → {imported: [{name, group, file, unmapped}], dividers: [{name, group, file}], failed: [[file, error]]}",
            always,
            files::import_effects
        ),
        cmd!(
            "mask.exportEffects",
            "Export Mask Effects",
            [],
            None,
            "{path, names?: [name], group?: name (default: all)} — writes an .lceffect JSON file (groups, stars and kept Lightroom settings included) → {path, count}",
            always,
            files::export_effects
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_keeps_amount_and_matches_ignore_it() {
        let e = MaskEffect { name: "E".into(), adjust: LocalAdjustments { exposure: 1.0, ..Default::default() }, ..Default::default() };
        let current = LocalAdjustments { shadows: 40.0, amount: 60.0, ..Default::default() };
        let out = e.apply(&current);
        assert_eq!((out.exposure, out.shadows, out.amount), (1.0, 0.0, 60.0));
        assert!(e.matches(&out));
        assert!(!e.matches(&current));
    }

    #[test]
    fn partial_effects_leave_other_sliders_and_can_carry_amount() {
        let e = MaskEffect {
            name: "Lift".into(),
            adjust: LocalAdjustments { shadows: 30.0, clarity: 99.0, amount: 70.0, ..Default::default() },
            fields: vec!["shadows".into()],
            with_amount: true,
            ..Default::default()
        };
        let current = LocalAdjustments { exposure: 0.5, clarity: 10.0, ..Default::default() };
        let out = e.apply(&current);
        assert_eq!((out.exposure, out.shadows, out.clarity, out.amount), (0.5, 30.0, 10.0, 70.0));
        assert!(e.matches(&out));
        assert!(!e.matches(&LocalAdjustments { amount: 100.0, ..out }), "Amount counts when carried");
    }

    #[test]
    fn hue_without_saturation_still_matches() {
        let e = MaskEffect {
            name: "E".into(),
            adjust: LocalAdjustments { color_hue: 240.0, exposure: 0.25, ..Default::default() },
            ..Default::default()
        };
        assert!(e.matches(&LocalAdjustments { exposure: 0.25, ..Default::default() }));
        assert!(!e.matches(&LocalAdjustments { exposure: 0.25, color_sat: 10.0, ..Default::default() }));
    }

    #[test]
    fn natural_order_and_groups() {
        let mut v = vec!["10 b", "2 a", "Burn", "01 x", "1 y"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["01 x", "1 y", "2 a", "10 b", "Burn"]);
        let e = |n: &str, g: &str| MaskEffect { name: n.into(), group: g.into(), ..Default::default() };
        let list = vec![e("12 b", "Detail"), e("3 a", ""), e("11 a", "detail"), e("20 r", "Retouch"), e("Burn", "")];
        let order = menu_order(&list);
        let names: Vec<(String, Vec<&str>)> = order.iter().map(|(g, ix)| (g.clone(), ix.iter().map(|i| list[*i].name.as_str()).collect())).collect();
        assert_eq!(
            names,
            vec![("".to_string(), vec!["3 a", "Burn"]), ("Detail".to_string(), vec!["11 a", "12 b"]), ("Retouch".to_string(), vec!["20 r"]),]
        );
    }

    #[test]
    fn normalized_clamps_and_tidies_fields() {
        let e = MaskEffect {
            name: "  X ".into(),
            adjust: LocalAdjustments { exposure: 99.0, amount: 150.0, ..Default::default() },
            fields: vec!["color_sat".into(), "bogus".into(), "exposure".into()],
            brush: Some(BrushSettings { size: f64::NAN, flow: 0.0, ..Default::default() }),
            ..Default::default()
        }
        .normalized();
        assert_eq!(e.name, "X");
        assert_eq!(e.adjust.exposure, 4.0);
        assert_eq!(e.adjust.amount, 100.0, "Amount isn't carried");
        assert_eq!(e.fields, vec!["exposure", "color_hue", "color_sat"]);
        let b = e.brush.unwrap();
        assert_eq!((b.size, b.flow), (0.04, 1.0));
    }
}
