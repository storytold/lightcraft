//! What a library's missing develop settings read as: the reading half of the stored form of
//! catalog records ([`crate::journal`]).
//!
//! A record in `catalog.snap` or `catalog.log` may leave a develop setting out: the settings of
//! a photo, and those of its history steps, versions and import look, are read by the types'
//! `Deserialize`, where a missing field takes its default. Libraries whose records state every
//! setting load as they are.
//!
//! **A library says what its missing values read as.** A value that is not in the file reads as
//! the default, so a default that changed in the code would change every photo relying on it. So
//! `catalog.snap` states them itself (`"defaults"`: [`base`], the settings a record with nothing
//! stated reads as in the build that wrote it), and they cover the log beside it. A build whose
//! own differ ([`reads_the_same`]) reads the library's settings over the library's defaults, not
//! over its own ([`reading_over`]), and then rewrites the library with its own. A build that only
//! has more settings than the library states reads it as it is: a setting the library never
//! heard of takes this build's default, until the library's next snapshot states it (the build
//! that adds a setting, or changes a default it has, is the one to take care of that).
//!
//! That works where a setting is a value in a struct, which the settings are, and because the
//! library states every setting, also those the settings' own JSON leaves out at their default
//! ([`UNWRITTEN`]). It does not reach the entries of lists inside the settings (masks, spots):
//! an entry's missing value takes the default of the entry's own type. What a missing value reads
//! as in this build is pinned by a test as well (`develop_defaults.json`), so a changed default
//! is seen when it is made; a setting that is removed or renamed needs a loader of its own.

use std::sync::{Arc, OnceLock};

use lightcraft_develop::DevelopSettings;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

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

/// Give `v` every value of `defaults` it leaves out, object by object: what a record that leaves them out reads as.
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
