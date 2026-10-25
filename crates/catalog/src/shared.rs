//! One develop settings value for all the photos whose settings are equal.
//!
//! A photo holds its settings behind an `Arc`, and so do its history steps, versions and import
//! look. Most photos of a library were never edited: without sharing, each holds a copy of the
//! same defaults (over a third of the memory an unedited JPEG's record takes), and so does every
//! raw with the same import look. Here every
//! settings value that enters a catalog, read from a saved library or given by a change, is
//! replaced by the one already held when an equal one is.
//!
//! Sharing never lets one photo's edit reach another: settings behind an `Arc` are not changed
//! in place (the catalog replaces them; `Arc::make_mut` copies a value that is shared).
//!
//! **Equal means the same down to the JSON.** `==` takes -0.0 for 0.0, which the saved form, a
//! photo's fingerprint and the preview-cache key tell apart. Two values are shared only when
//! they are `==` and neither holds a negative zero. Settings holding a negative zero are left
//! alone: never shared, never what others share.
//!
//! What is held to compare with: the defaults, and on each thread the last few values seen (a
//! camera's import defaults, a photo's own history). The thread's list keeps nothing alive: a
//! value no photo holds any more is gone from it too.

use std::cell::{OnceCell, RefCell};
use std::sync::{Arc, OnceLock, Weak};

use lightcraft_develop::DevelopSettings;
use serde::de::{Deserialize, Deserializer};

use crate::{HistoryStep, Photo, Version};

/// How many values a thread remembers besides the defaults and the current photo's.
const RECENT: usize = 4;

/// Whether a value holds a -0.0 anywhere (a float the saved JSON writes as `-0.0`): found by
/// walking it as `Serialize` does, so no text is built (writing every float out costs as much as
/// reading the library did).
fn has_negative_zero<T: serde::Serialize + ?Sized>(value: &T) -> bool {
    value.serialize(Walk).is_err()
}

/// Walks a value; stops with an error at the first -0.0.
struct Walk;

#[derive(Debug)]
struct Found;

impl std::fmt::Display for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("negative zero")
    }
}

impl std::error::Error for Found {}

impl serde::ser::Error for Found {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Found
    }
}

type Walked = Result<(), Found>;

macro_rules! nothing {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(fn $method(self, _: $ty) -> Walked { Ok(()) })*
    };
}

impl serde::Serializer for Walk {
    type Ok = ();
    type Error = Found;
    type SerializeSeq = Walk;
    type SerializeTuple = Walk;
    type SerializeTupleStruct = Walk;
    type SerializeTupleVariant = Walk;
    type SerializeMap = Walk;
    type SerializeStruct = Walk;
    type SerializeStructVariant = Walk;

    nothing!(serialize_bool(bool), serialize_i8(i8), serialize_i16(i16), serialize_i32(i32), serialize_i64(i64), serialize_i128(i128));
    nothing!(serialize_u8(u8), serialize_u16(u16), serialize_u32(u32), serialize_u64(u64), serialize_u128(u128));
    nothing!(serialize_char(char), serialize_str(&str), serialize_bytes(&[u8]), serialize_unit_struct(&'static str));

    fn serialize_f32(self, v: f32) -> Walked {
        if v == 0.0 && v.is_sign_negative() { Err(Found) } else { Ok(()) }
    }

    fn serialize_f64(self, v: f64) -> Walked {
        if v == 0.0 && v.is_sign_negative() { Err(Found) } else { Ok(()) }
    }

    fn serialize_none(self) -> Walked {
        Ok(())
    }

    fn serialize_some<T: serde::Serialize + ?Sized>(self, v: &T) -> Walked {
        v.serialize(Walk)
    }

    fn serialize_unit(self) -> Walked {
        Ok(())
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Walked {
        Ok(())
    }

    fn serialize_newtype_struct<T: serde::Serialize + ?Sized>(self, _: &'static str, v: &T) -> Walked {
        v.serialize(Walk)
    }

    fn serialize_newtype_variant<T: serde::Serialize + ?Sized>(self, _: &'static str, _: u32, _: &'static str, v: &T) -> Walked {
        v.serialize(Walk)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Walk, Found> {
        Ok(Walk)
    }

    fn serialize_tuple(self, _: usize) -> Result<Walk, Found> {
        Ok(Walk)
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Walk, Found> {
        Ok(Walk)
    }

    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Walk, Found> {
        Ok(Walk)
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Walk, Found> {
        Ok(Walk)
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Walk, Found> {
        Ok(Walk)
    }

    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Walk, Found> {
        Ok(Walk)
    }
}

macro_rules! walk_elements {
    ($($trait:ident::$method:ident),* $(,)?) => {
        $(impl serde::ser::$trait for Walk {
            type Ok = ();
            type Error = Found;

            fn $method<T: serde::Serialize + ?Sized>(&mut self, v: &T) -> Walked {
                v.serialize(Walk)
            }

            fn end(self) -> Walked {
                Ok(())
            }
        })*
    };
}

walk_elements!(
    SerializeSeq::serialize_element,
    SerializeTuple::serialize_element,
    SerializeTupleStruct::serialize_field,
    SerializeTupleVariant::serialize_field
);

impl serde::ser::SerializeMap for Walk {
    type Ok = ();
    type Error = Found;

    fn serialize_key<T: serde::Serialize + ?Sized>(&mut self, k: &T) -> Walked {
        k.serialize(Walk)
    }

    fn serialize_value<T: serde::Serialize + ?Sized>(&mut self, v: &T) -> Walked {
        v.serialize(Walk)
    }

    fn end(self) -> Walked {
        Ok(())
    }
}

impl serde::ser::SerializeStruct for Walk {
    type Ok = ();
    type Error = Found;

    fn serialize_field<T: serde::Serialize + ?Sized>(&mut self, _: &'static str, v: &T) -> Walked {
        v.serialize(Walk)
    }

    fn end(self) -> Walked {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for Walk {
    type Ok = ();
    type Error = Found;

    fn serialize_field<T: serde::Serialize + ?Sized>(&mut self, _: &'static str, v: &T) -> Walked {
        v.serialize(Walk)
    }

    fn end(self) -> Walked {
        Ok(())
    }
}

fn held_default() -> &'static Arc<DevelopSettings> {
    static DEFAULT: OnceLock<Arc<DevelopSettings>> = OnceLock::new();
    DEFAULT.get_or_init(|| Arc::new(DevelopSettings::default()))
}

/// The default settings, held once.
pub fn default_settings() -> Arc<DevelopSettings> {
    held_default().clone()
}

/// What a thread has seen: pointers that keep nothing alive (a value the catalog let go of is
/// gone from here too). None of the values holds a negative zero.
#[derive(Default)]
struct Seen {
    /// The settings of the photo being read or added: its history (up to 200 steps, each seen
    /// after it) usually ends with them.
    current: Option<Weak<DevelopSettings>>,
    /// The last few values, the latest first.
    recent: Vec<Weak<DevelopSettings>>,
}

thread_local! {
    static SEEN: RefCell<Seen> = RefCell::new(Seen::default());
}

/// How many values this thread remembers now (tests).
#[cfg(test)]
pub(crate) fn remembered() -> usize {
    SEEN.with(|seen| seen.borrow().recent.len())
}

/// `settings`, or the value already held that is the same.
pub fn settings(settings: Arc<DevelopSettings>) -> Arc<DevelopSettings> {
    share(settings, false)
}

/// [`settings`] for a photo's own settings (`current`): what its history is compared with first.
fn share(settings: Arc<DevelopSettings>, current: bool) -> Arc<DevelopSettings> {
    // (looked for at most once, and only when an equal value turned up or this one is kept)
    let exact = OnceCell::new();
    let exact = || *exact.get_or_init(|| !has_negative_zero(settings.as_ref()));
    let same = |held: &Arc<DevelopSettings>| Arc::ptr_eq(held, &settings) || (**held == *settings && exact());
    let default = held_default();
    if same(default) {
        // (the held default is compared with anyway)
        if current {
            let _ = SEEN.try_with(|seen| seen.try_borrow_mut().map(|mut s| s.current = None));
        }
        return default.clone();
    }
    let found = SEEN.try_with(|seen| {
        let mut seen = seen.try_borrow_mut().ok()?;
        let found = seen.current.as_ref().and_then(Weak::upgrade).filter(|s| same(s)).or_else(|| {
            // (values that are gone drop out as they are met)
            let mut found = None;
            seen.recent.retain(|w| match w.upgrade() {
                Some(s) if found.is_none() && same(&s) => {
                    found = Some(s);
                    false
                }
                Some(_) => true,
                None => false,
            });
            if let Some(f) = &found {
                seen.recent.insert(0, Arc::downgrade(f));
            }
            found
        });
        if found.is_none() && exact() {
            seen.recent.truncate(RECENT - 1);
            seen.recent.insert(0, Arc::downgrade(&settings));
        }
        if current {
            seen.current = match &found {
                Some(f) => Some(Arc::downgrade(f)),
                None => exact().then(|| Arc::downgrade(&settings)),
            };
        }
        found
    });
    // (a thread shutting down: as it came)
    found.ok().flatten().unwrap_or(settings)
}

/// Share the settings of a history step.
pub(crate) fn step(step: &mut HistoryStep) {
    step.settings = settings(step.settings.clone());
}

/// What a change's settings are compared with first: the photo's own settings.
pub(crate) struct Anchor {
    own: Arc<DevelopSettings>,
    /// Whether `own` holds no negative zero (looked for once, and only when an equal value turned up).
    exact: OnceCell<bool>,
}

impl Anchor {
    pub(crate) fn new(own: Arc<DevelopSettings>) -> Self {
        Self { own, exact: OnceCell::new() }
    }

    /// `s`, or the photo's own settings when equal (and neither holds a negative zero), or else what [`settings`] says.
    fn settings(&self, s: Arc<DevelopSettings>) -> Arc<DevelopSettings> {
        if Arc::ptr_eq(&self.own, &s) {
            return s;
        }
        if *self.own == *s && *self.exact.get_or_init(|| !has_negative_zero(self.own.as_ref())) && !has_negative_zero(s.as_ref()) {
            return self.own.clone();
        }
        settings(s)
    }

    pub(crate) fn step(&self, step: &mut HistoryStep) {
        step.settings = self.settings(step.settings.clone());
    }

    pub(crate) fn versions(&self, versions: &mut [Version]) {
        for v in versions {
            v.settings = self.settings(v.settings.clone());
        }
    }
}

/// Share the settings of versions.
pub(crate) fn versions(versions: &mut [Version]) {
    for v in versions {
        v.settings = settings(v.settings.clone());
    }
}

/// Share every settings value a photo holds.
pub(crate) fn photo(p: &mut Photo) {
    p.develop = share(p.develop.clone(), true);
    p.history.iter_mut().for_each(step);
    versions(&mut p.versions);
    p.import_look = p.import_look.take().map(settings);
}

/// Settings read from a saved library (`deserialize_with`): shared.
pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<DevelopSettings>, D::Error> {
    DevelopSettings::deserialize(d).map(|s| share(Arc::new(s), false))
}

/// [`deserialize`] for a photo's own settings: what its history, read after them, is compared
/// with first.
pub(crate) fn deserialize_current<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<DevelopSettings>, D::Error> {
    DevelopSettings::deserialize(d).map(|s| share(Arc::new(s), true))
}

/// [`deserialize`] for settings a photo may not have (its import look).
pub(crate) fn deserialize_optional<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<DevelopSettings>>, D::Error> {
    Ok(Option::<DevelopSettings>::deserialize(d)?.map(|s| share(Arc::new(s), false)))
}
