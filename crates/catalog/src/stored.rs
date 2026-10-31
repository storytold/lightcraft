//! The stored form of develop settings, and what a library's missing develop settings read as.
//!
//! A develop setting a record leaves out reads as its default (the types' `Deserialize`: a missing
//! field takes its default), so libraries whose records state every setting load as they are.
//! This module is the writing half of leaving them out: the settings of a photo, and those of its
//! history steps, versions and import look, stored as their **difference from
//! [`DevelopSettings::default`]** (`{}` for an unedited photo). Nothing writes it yet: catalog
//! format 10, in the next change, switches the records over (the `serialize_with` functions
//! [`settings`], [`step`], [`history`] and [`versions`] are what its ops name, and its photos are
//! stored with them), and until then records state settings as formats up to 9 do.
//!
//! **Settings are never stored in a form that reads back differently.** A value is left out only
//! where leaving it out of the defaults reads back as the defaults. That is not so for `process`,
//! whose missing value means the first process, not the default one; nor for single fields of a
//! value that is read all or nothing (the crop's rectangle), which is left out as a whole or not
//! at all. And each difference is read back and compared before it is written; settings that
//! would not survive are stored in full.
//!
//! **A library says what its missing values read as.** A value that is not in the file reads as
//! the default, so a default that changed in the code would change every photo relying on it. So
//! `catalog.snap` states them itself (`"defaults"`: [`base`], the settings a record with nothing
//! stated reads as in the build that wrote it), and they cover the log beside it. A build whose
//! own differ ([`reads_the_same`]) reads the library's settings over the library's defaults, not
//! over its own ([`reading_over`]), and then rewrites the library with its own. A build that only
//! has more settings than the library states reads it as it is: a setting the library never
//! heard of takes this build's default, until the library's next snapshot states it. Once records
//! leave settings out (the next change), such a library is rewritten to state them first
//! ([`states_every_setting`]), and until that is written its records state develop settings
//! whole ([`writing_full`]).
//!
//! That works where a setting is a value in a struct, which the settings are, and because the
//! library states every setting, also those the settings' own JSON leaves out at their default
//! ([`UNWRITTEN`]). It does not reach the entries of lists inside the settings (masks, spots):
//! an entry's missing value takes the default of the entry's own type. What a missing value reads
//! as in this build is pinned by a test as well (`develop_defaults.json`), so a changed default
//! is seen when it is made; a setting that is removed or renamed needs a loader of its own.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use lightcraft_develop::DevelopSettings;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{HistoryStep, Version};

// ---- develop settings

/// What the stored form of settings is measured against.
struct Defaults {
    settings: DevelopSettings,
    /// The default settings, section by section (their top-level fields).
    sections: BTreeMap<String, Section>,
    /// The stored form of the defaults themselves: the values that must always be stated.
    /// `None`: the defaults can't be told from their JSON at all, and settings are stored in full.
    stored: Option<Value>,
}

/// One top-level field of the default settings.
struct Section {
    /// As JSON text: a section that serialises to the same text is the default one.
    text: String,
    value: Value,
    rule: Rule,
}

/// Whether a value of the settings may be left out when it equals the default.
#[derive(Default)]
struct Rule {
    /// Leaving it out of the defaults (the whole value, when it is an object) reads back as the
    /// defaults.
    elide: bool,
    /// An object's fields.
    fields: BTreeMap<String, Rule>,
}

impl Rule {
    /// The rule for the value at `path` of `full`, the JSON of `settings`.
    fn of(full: &Value, settings: &DevelopSettings, path: &mut Vec<String>) -> Rule {
        let mut without = full.clone();
        let elide = !path.is_empty() && remove_at(&mut without, path) && reads_exactly_as(&without, settings);
        let mut fields = BTreeMap::new();
        if let Some(Value::Object(m)) = path.iter().try_fold(full, |v, k| v.get(k)) {
            for k in m.keys() {
                path.push(k.clone());
                fields.insert(k.clone(), Rule::of(full, settings, path));
                path.pop();
            }
        }
        Rule { elide, fields }
    }
}

fn defaults() -> &'static Defaults {
    static DEFAULTS: OnceLock<Defaults> = OnceLock::new();
    DEFAULTS.get_or_init(|| {
        let settings = DevelopSettings::default();
        let full = serde_json::to_value(&settings).unwrap_or(Value::Null);
        let Rule { mut fields, .. } = Rule::of(&full, &settings, &mut Vec::new());
        // each section's text as the settings serialise it (a `Value`'s text orders keys differently)
        let texts = settings.serialize(Sections { d: None, text: Vec::new() }).map(|c| c.texts).unwrap_or_default();
        let mut sections = BTreeMap::new();
        if let Value::Object(m) = full {
            for (key, value) in m {
                if let (Some(rule), Some(text)) = (fields.remove(&key), texts.get(&key)) {
                    sections.insert(key, Section { text: text.clone(), value, rule });
                }
            }
        }
        let mut d = Defaults { settings, sections, stored: None };
        d.stored = difference(&d.settings, &d).filter(|stored| reads_exactly_as(stored, &d.settings));
        if d.stored.is_none() {
            // the values depend on each other in a way this can't see: leave nothing out
            log::error!("catalog: develop defaults don't read back from their stored form; settings are stored in full");
            d.sections.clear();
        }
        d
    })
}

// ---- what a library's missing values read as

/// The settings a record with nothing stated (`{}`) reads as in this build, as `catalog.snap`
/// states them (`"defaults"`): the JSON text, and its value.
///
/// Every setting is stated, also the ones the settings' own JSON leaves out at their default
/// ([`UNWRITTEN`]): a record never states those at their default either, so a default of theirs
/// that changed would otherwise go unseen.
pub(crate) fn base() -> &'static (String, Value) {
    static BASE: OnceLock<(String, Value)> = OnceLock::new();
    BASE.get_or_init(|| {
        let settings = serde_json::from_str::<DevelopSettings>("{}").unwrap_or_default();
        let mut v = serde_json::to_value(&settings).unwrap_or(Value::Null);
        for (path, read) in UNWRITTEN {
            state(&mut v, path, read(&settings));
        }
        (v.to_string(), v)
    })
}

/// A setting's value in `settings`, as JSON.
type Stated = fn(&DevelopSettings) -> Value;

/// The settings whose JSON is left out at their default (`skip_serializing_if`), and what each
/// is: [`base`] states them all the same. A test holds that these are all of them, but for the
/// rendering process: a record without one is on the first process in every build (a missing
/// process never means "the default one"), so there is no default of it to state.
pub(crate) const UNWRITTEN: [(&[&str], Stated); 4] = [
    (&["wb", "scale"], |s| serde_json::to_value(s.wb.scale).unwrap_or(Value::Null)),
    (&["hdr"], |s| serde_json::to_value(s.hdr).unwrap_or(Value::Null)),
    (&["profile", "camera_local_tone"], |s| Value::Bool(s.profile.camera_local_tone)),
    (&["enhance", "denoise_on"], |s| serde_json::to_value(s.enhance.denoise_on).unwrap_or(Value::Null)),
];

/// Put `value` at `path` of `v` (through objects that are there).
fn state(v: &mut Value, path: &[&str], value: Value) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut at = v;
    for k in parents {
        let Some(next) = at.get_mut(*k) else { return };
        at = next;
    }
    if let Some(m) = at.as_object_mut() {
        m.insert((*last).to_string(), value);
    }
}

/// Whether a library that states `theirs` as its defaults reads the same over this build's:
/// every setting both know has the same default. A setting only this build knows takes this
/// build's default (the library never heard of it); one only the library knows is not read by
/// this build, whatever it is read over.
pub(crate) fn reads_the_same(theirs: &Value) -> bool {
    fn agree(theirs: &Value, ours: &Value) -> bool {
        match (theirs, ours) {
            (Value::Object(t), Value::Object(o)) => t.iter().all(|(k, x)| o.get(k).is_none_or(|y| agree(x, y))),
            _ => same(theirs, ours),
        }
    }
    agree(theirs, &base().1)
}

/// Whether a library that states `theirs` as its defaults states every setting this build has
/// (a build that adds a setting is the one this is for): a record written from now on leaves out
/// what equals this build's default of that setting, so the library must state it first.
#[allow(dead_code, reason = "called by the journal in the change that brings catalog format 10")]
pub(crate) fn states_every_setting(theirs: &Value) -> bool {
    fn has(theirs: &Value, ours: &Value) -> bool {
        match (theirs, ours) {
            (Value::Object(t), Value::Object(o)) => o.iter().all(|(k, y)| t.get(k).is_some_and(|x| has(x, y))),
            (Value::Object(_), _) | (_, Value::Object(_)) => false,
            _ => true,
        }
    }
    has(theirs, &base().1)
}

thread_local! {
    /// The defaults of the library being read on this thread, where they are not this build's.
    static READING_OVER: std::cell::RefCell<Option<Arc<Value>>> = const { std::cell::RefCell::new(None) };
}

/// While it lives, settings read on this thread are read over these defaults (see
/// [`reading_over`]).
pub(crate) struct ReadingOver(Option<Arc<Value>>);

impl Drop for ReadingOver {
    fn drop(&mut self) {
        let before = self.0.take();
        // (a thread shutting down has nothing left to read)
        let _ = READING_OVER.try_with(|over| *over.borrow_mut() = before);
    }
}

/// Read the settings of a library that states `defaults` as its own: until the returned guard
/// is dropped, a value a record leaves out on this thread reads as the library's, not as this
/// build's. `None`: as this build's (the usual case, and what reading costs nothing for).
pub(crate) fn reading_over(defaults: Option<Arc<Value>>) -> ReadingOver {
    ReadingOver(READING_OVER.try_with(|over| over.replace(defaults)).unwrap_or(None))
}

/// The defaults settings are being read over on this thread ([`reading_over`]), to read the
/// same library on other threads as well.
pub(crate) fn reading_now() -> Option<Arc<Value>> {
    READING_OVER.try_with(|over| over.borrow().clone()).unwrap_or(None)
}

/// Give `v` every value of `defaults` it leaves out, object by object: the reverse of [`strip`].
fn fill(v: &mut Value, defaults: &Value) {
    let (Value::Object(m), Value::Object(d)) = (v, defaults) else { return };
    for (k, x) in d {
        match m.get_mut(k) {
            Some(mine) => fill(mine, x),
            None => {
                m.insert(k.clone(), x.clone());
            }
        }
    }
}

/// `deserialize_with` for settings in a saved record: over the defaults of the library being
/// read, where those are not this build's ([`reading_over`]).
pub(crate) fn read_settings<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<DevelopSettings>, D::Error> {
    let Some(defaults) = reading_now() else { return Arc::<DevelopSettings>::deserialize(d) };
    let mut v = Value::deserialize(d)?;
    fill(&mut v, &defaults);
    DevelopSettings::deserialize(v).map(Arc::new).map_err(serde::de::Error::custom)
}

/// [`read_settings`] for settings that may not be there.
pub(crate) fn read_optional_settings<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<DevelopSettings>>, D::Error> {
    #[derive(Deserialize)]
    struct Read(#[serde(deserialize_with = "read_settings")] Arc<DevelopSettings>);
    Ok(Option::<Read>::deserialize(d)?.map(|r| r.0))
}

/// Remove the value at `path`; whether there was one.
fn remove_at(v: &mut Value, path: &[String]) -> bool {
    let Some((last, parents)) = path.split_last() else { return false };
    let mut at = v;
    for k in parents {
        let Some(next) = at.get_mut(k) else { return false };
        at = next;
    }
    at.as_object_mut().is_some_and(|m| m.remove(last).is_some())
}

/// The same JSON value, also telling `-0.0` from `0.0` (which `Value`'s own comparison doesn't).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(p), Some(q)) if x.is_f64() || y.is_f64() => x.is_f64() == y.is_f64() && p.to_bits() == q.to_bits(),
            _ => x == y,
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| same(p, q))),
        _ => a == b,
    }
}

/// Remove from `v` every value that is the one `base` (the defaults) has there, where `rule`
/// lets it be left out; objects are gone through field by field. An object that differs stays,
/// even when nothing of it is left.
fn strip(v: &mut Value, base: &Value, rule: &Rule) {
    let (Value::Object(m), Value::Object(b)) = (v, base) else { return };
    m.retain(|k, x| {
        let (Some(d), Some(r)) = (b.get(k), rule.fields.get(k)) else { return true };
        if r.elide && same(x, d) {
            return false;
        }
        strip(x, d, r);
        true
    });
}

/// `v` reads as `settings`. (`==` takes -0.0 for 0.0: what is left out is chosen by [`same`]
/// and by JSON text, which don't, so a zero's sign is never what differs here.)
fn reads_as(v: &Value, settings: &DevelopSettings) -> bool {
    DevelopSettings::deserialize(v).is_ok_and(|read| read == *settings)
}

/// `v` reads as `settings` down to their JSON text. Costs a serialisation each: for working out
/// the rules once, not for every write.
fn reads_exactly_as(v: &Value, settings: &DevelopSettings) -> bool {
    DevelopSettings::deserialize(v).is_ok_and(|read| read == *settings && serde_json::to_string(&read).ok() == serde_json::to_string(settings).ok())
}

/// Whether any number in `v` is -0.0.
fn has_negative_zero<T: Serialize + ?Sized>(v: &T) -> bool {
    let mut scan = NegativeZero(false);
    // (an error only says the walk stopped: when in doubt, it has one)
    v.serialize(&mut scan).is_err() || scan.0
}

/// Walks a value for [`has_negative_zero`] (no text, no allocation).
struct NegativeZero(bool);

impl NegativeZero {
    fn see(&mut self, x: f64) {
        self.0 |= x == 0.0 && x.is_sign_negative();
    }
}

impl Serializer for &mut NegativeZero {
    type Ok = ();
    type Error = std::fmt::Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_f32(self, v: f32) -> Result<(), Self::Error> {
        self.see(f64::from(v));
        Ok(())
    }
    fn serialize_f64(self, v: f64) -> Result<(), Self::Error> {
        self.see(v);
        Ok(())
    }
    fn serialize_bool(self, _: bool) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_char(self, _: char) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_str(self, _: &str) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<(), Self::Error> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, v: &T) -> Result<(), Self::Error> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(self, _: &'static str, _: u32, _: &'static str, v: &T) -> Result<(), Self::Error> {
        v.serialize(self)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self, Self::Error> {
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self, Self::Error> {
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }
}

impl serde::ser::SerializeSeq for &mut NegativeZero {
    type Ok = ();
    type Error = std::fmt::Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Self::Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeTuple for &mut NegativeZero {
    type Ok = ();
    type Error = std::fmt::Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Self::Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for &mut NegativeZero {
    type Ok = ();
    type Error = std::fmt::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Self::Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleVariant for &mut NegativeZero {
    type Ok = ();
    type Error = std::fmt::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Self::Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl SerializeMap for &mut NegativeZero {
    type Ok = ();
    type Error = std::fmt::Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, _: &T) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Self::Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeStruct for &mut NegativeZero {
    type Ok = ();
    type Error = std::fmt::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, v: &T) -> Result<(), Self::Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for &mut NegativeZero {
    type Ok = ();
    type Error = std::fmt::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, v: &T) -> Result<(), Self::Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// The stored form of `settings`: its difference from the defaults, or `None` when that would
/// not read back as `settings` (then they are stored in full).
fn difference(settings: &DevelopSettings, d: &Defaults) -> Option<Value> {
    let v = Value::Object(settings.serialize(Sections { d: Some(d), text: Vec::new() }).ok()?.differing);
    reads_as(&v, settings).then_some(v)
}

/// Collects the sections of settings that differ from the defaults, each as what differs in it.
/// Settings are a struct of sections; most sections of most photos are the default ones, and
/// those are recognised by their JSON text, without building a value for them.
///
/// Without defaults to compare with (`d` is `None`: while they are being worked out), collects
/// every section's JSON text instead.
struct Sections<'d> {
    d: Option<&'d Defaults>,
    /// Scratch space for a section's JSON text.
    text: Vec<u8>,
}

#[derive(Default)]
struct Collected {
    /// The sections that differ from the defaults, as what differs in them.
    differing: serde_json::Map<String, Value>,
    /// Every section's JSON text (only without defaults to compare with).
    texts: BTreeMap<String, String>,
}

struct SectionFields<'d> {
    d: Option<&'d Defaults>,
    text: Vec<u8>,
    out: Collected,
}

impl serde::ser::SerializeStruct for SectionFields<'_> {
    type Ok = Collected;
    type Error = serde_json::Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error> {
        let Some(d) = self.d else {
            self.out.texts.insert(key.to_owned(), serde_json::to_string(value)?);
            return Ok(());
        };
        let Some(section) = d.sections.get(key) else {
            // not among the defaults (left out of them: the rendering process, HDR…): as it is
            self.text.clear();
            serde_json::to_writer(&mut self.text, value)?;
            self.out.differing.insert(key.to_owned(), serde_json::from_slice(&self.text)?);
            return Ok(());
        };
        self.text.clear();
        serde_json::to_writer(&mut self.text, value)?;
        if section.rule.elide && self.text == section.text.as_bytes() {
            return Ok(());
        }
        // (from the text just written, not `to_value`: that widens an `f32` to the digits of its
        // `f64`, which reads back the same but is 17 digits long)
        let mut v: Value = serde_json::from_slice(&self.text)?;
        strip(&mut v, &section.value, &section.rule);
        self.out.differing.insert(key.to_owned(), v);
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.out)
    }
}

/// Only a struct has sections.
fn not_a_struct<T>() -> Result<T, serde_json::Error> {
    Err(serde::ser::Error::custom("settings are not a struct"))
}

type No = serde::ser::Impossible<Collected, serde_json::Error>;

impl<'d> Serializer for Sections<'d> {
    type Ok = Collected;
    type Error = serde_json::Error;
    type SerializeSeq = No;
    type SerializeTuple = No;
    type SerializeTupleStruct = No;
    type SerializeTupleVariant = No;
    type SerializeMap = No;
    type SerializeStruct = SectionFields<'d>;
    type SerializeStructVariant = No;

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(SectionFields { d: self.d, text: self.text, out: Collected::default() })
    }

    fn serialize_bool(self, _: bool) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_i8(self, _: i8) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_i16(self, _: i16) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_i32(self, _: i32) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_i64(self, _: i64) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_u8(self, _: u8) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_u16(self, _: u16) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_u32(self, _: u32) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_u64(self, _: u64) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_f32(self, _: f32) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_f64(self, _: f64) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_char(self, _: char) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_str(self, _: &str) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, _: &T) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(self, _: &'static str, _: u32, _: &'static str, _: &T) -> Result<Self::Ok, Self::Error> {
        not_a_struct()
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        not_a_struct()
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Self::Error> {
        not_a_struct()
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeTupleStruct, Self::Error> {
        not_a_struct()
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self::SerializeTupleVariant, Self::Error> {
        not_a_struct()
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        not_a_struct()
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self::SerializeStructVariant, Self::Error> {
        not_a_struct()
    }
}

thread_local! {
    /// Settings written on this thread are written whole, not as their difference from the
    /// defaults ([`writing_full`]).
    static WRITING_FULL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While it lives, develop settings written on this thread are stored whole, as formats before
/// 10 did, so a record reads back the same over whatever defaults its library states (a value the
/// settings' own JSON leaves out at its default is as in those formats): for a
/// library whose header doesn't state every setting this build has yet, where a difference from
/// this build's defaults would leave out what the header gives no default for.
pub(crate) struct WritingFull(bool);

impl Drop for WritingFull {
    fn drop(&mut self) {
        let before = self.0;
        let _ = WRITING_FULL.try_with(|w| w.set(before));
    }
}

#[allow(dead_code, reason = "called by the journal in the change that brings catalog format 10")]
pub(crate) fn writing_full() -> WritingFull {
    WritingFull(WRITING_FULL.try_with(|w| w.replace(true)).unwrap_or(false))
}

/// Develop settings in their stored form.
struct StoredSettings<'a>(&'a DevelopSettings);

impl Serialize for StoredSettings<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if WRITING_FULL.try_with(|w| w.get()).unwrap_or(false) {
            return self.0.serialize(s);
        }
        let d = defaults();
        // `==` takes -0.0 for 0.0, which their JSON (and so a photo's fingerprint and the preview
        // cache key made from it) tells apart: settings with a negative zero are never taken for
        // other settings equal to them
        let negative_zero = std::cell::OnceCell::new();
        let no_negative_zero = || !*negative_zero.get_or_init(|| has_negative_zero(self.0));
        let exactly = |other: &DevelopSettings| *self.0 == *other && no_negative_zero();
        if exactly(&d.settings)
            && let Some(stored) = &d.stored
        {
            return stored.serialize(s);
        }
        // Photos that were never edited often share their settings (a camera's import defaults):
        // the last difference worked out is kept for the next settings equal to those.
        thread_local! {
            static LAST: std::cell::RefCell<Option<(DevelopSettings, Value)>> = const { std::cell::RefCell::new(None) };
        }
        // (a serializer can only be used once: it goes in and out of the closure in a cell)
        let s = std::cell::Cell::new(Some(s));
        let stored = LAST.try_with(|last| {
            let mut last = last.try_borrow_mut().ok()?;
            if !last.as_ref().is_some_and(|(of, _)| exactly(of)) {
                let v = difference(self.0, d)?;
                if !no_negative_zero() {
                    // (not kept: it can't be taken for equal settings later)
                    return Some(v.serialize(s.take()?));
                }
                *last = Some((self.0.clone(), v));
            }
            let (_, v) = last.as_ref()?;
            Some(v.serialize(s.take()?))
        });
        match (stored, s.take()) {
            (Ok(Some(done)), _) => done,
            // no difference that reads back, or the cache can't be used (a thread shutting down)
            (_, Some(s)) => match difference(self.0, d) {
                Some(v) => v.serialize(s),
                None => self.0.serialize(s),
            },
            (_, None) => Err(serde::ser::Error::custom("develop settings: the serializer was lost")),
        }
    }
}

/// `serialize_with` for settings in an [`crate::Op`].
#[allow(dead_code, reason = "named by the ops and photos in the change that brings catalog format 10")]
pub(crate) fn settings<S: Serializer>(v: &Arc<DevelopSettings>, s: S) -> Result<S::Ok, S::Error> {
    StoredSettings(v).serialize(s)
}

#[derive(Serialize)]
struct StoredStep<'a> {
    label: &'a str,
    settings: StoredSettings<'a>,
}

impl<'a> StoredStep<'a> {
    fn of(step: &'a HistoryStep) -> Self {
        let HistoryStep { label, settings } = step;
        StoredStep { label, settings: StoredSettings(settings) }
    }
}

#[derive(Serialize)]
struct StoredVersion<'a> {
    name: &'a str,
    created: &'a str,
    settings: StoredSettings<'a>,
    auto: bool,
}

impl<'a> StoredVersion<'a> {
    fn of(version: &'a Version) -> Self {
        let Version { name, created, settings, auto } = version;
        StoredVersion { name, created, settings: StoredSettings(settings), auto: *auto }
    }
}

/// A list in its stored form.
struct StoredList<'a, T, R>(&'a [T], fn(&'a T) -> R);

impl<T, R: Serialize> Serialize for StoredList<'_, T, R> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            seq.serialize_element(&(self.1)(item))?;
        }
        seq.end()
    }
}

/// `serialize_with` for a history step in an [`crate::Op`].
#[allow(dead_code, reason = "named by the ops and photos in the change that brings catalog format 10")]
pub(crate) fn step<S: Serializer>(v: &HistoryStep, s: S) -> Result<S::Ok, S::Error> {
    StoredStep::of(v).serialize(s)
}

/// `serialize_with` for a history in an [`crate::Op`].
#[allow(dead_code, reason = "named by the ops and photos in the change that brings catalog format 10")]
pub(crate) fn history<S: Serializer>(v: &[HistoryStep], s: S) -> Result<S::Ok, S::Error> {
    StoredList(v, StoredStep::of).serialize(s)
}

/// `serialize_with` for versions in an [`crate::Op`].
#[allow(dead_code, reason = "named by the ops and photos in the change that brings catalog format 10")]
pub(crate) fn versions<S: Serializer>(v: &[Version], s: S) -> Result<S::Ok, S::Error> {
    StoredList(v, StoredVersion::of).serialize(s)
}
