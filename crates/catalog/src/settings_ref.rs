//! Develop settings inside photo records: inline JSON, or a reference into the v4 store's
//! content-addressed settings table.
//!
//! Photos' `develop`, `import_look`, History steps and Versions all hold `Arc<DevelopSettings>`.
//! The op log and JSON snapshots always write them inline (this module is a no-op there). While a
//! v4 checkpoint encodes photos ([`encode_scope`]), each one is written as the hex hash of its
//! JSON instead and collected once; while a v4 load decodes them ([`decode_scope`]), a hash
//! resolves to an `Arc` shared by every photo with the same settings (unedited photos all share
//! one), which is most of the memory a large library saves.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hasher;
use std::sync::Arc;

use dac_develop::DevelopSettings;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};

/// Settings met while encoding: pointer → hash (valid while the encoded photos are borrowed),
/// and the JSON of each hash not stored yet.
#[derive(Default)]
pub(crate) struct Encoder {
    by_ptr: HashMap<usize, u128>,
    /// The last few distinct settings hashed (kept alive, so their pointers stay unique).
    recent: Vec<(Arc<DevelopSettings>, u128)>,
    pub(crate) new: HashMap<u128, Vec<u8>>,
}

/// Settings a decode resolves references against.
pub(crate) type Resolver = Arc<HashMap<u128, Arc<DevelopSettings>>>;

thread_local! {
    static ENCODE: RefCell<Option<(Encoder, Arc<dyn Fn(u128) -> bool + Send + Sync>)>> = const { RefCell::new(None) };
    static DECODE: RefCell<Option<Resolver>> = const { RefCell::new(None) };
}

/// The content hash of settings JSON.
pub(crate) fn hash(json: &[u8]) -> u128 {
    let mut h = siphasher::sip128::SipHasher13::new_with_keys(0x5eed_0f5e_7715_9a51, 0x0dd5_e771_e65f_00d4);
    h.write(json);
    siphasher::sip128::Hasher128::finish128(&h).as_u128()
}

/// Run `f` with references on: returns its result and the settings it met that `known` doesn't
/// have (hash → JSON).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn encode_scope<T>(known: Arc<dyn Fn(u128) -> bool + Send + Sync>, f: impl FnOnce() -> T) -> (T, HashMap<u128, Vec<u8>>) {
    ENCODE.with(|e| *e.borrow_mut() = Some((Encoder::default(), known)));
    let out = f();
    let enc = ENCODE.with(|e| e.borrow_mut().take());
    (out, enc.map(|(e, _)| e.new).unwrap_or_default())
}

/// Run `f` with references resolved against `r`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn decode_scope<T>(r: Resolver, f: impl FnOnce() -> T) -> T {
    DECODE.with(|d| *d.borrow_mut() = Some(r));
    let out = f();
    DECODE.with(|d| *d.borrow_mut() = None);
    out
}

/// The reference for `v` in the active encode scope (`None`: no scope, write inline).
fn reference(v: &Arc<DevelopSettings>) -> Option<Result<u128, String>> {
    ENCODE.with(|e| {
        let mut e = e.borrow_mut();
        let (enc, known) = e.as_mut()?;
        let ptr = Arc::as_ptr(v) as usize;
        if let Some(h) = enc.by_ptr.get(&ptr) {
            return Some(Ok(*h));
        }
        // equal to settings met recently (unedited photos all equal the defaults, each in an
        // `Arc` of its own): comparing is far cheaper than serialising and hashing
        if let Some((_, h)) = enc.recent.iter().find(|(s, _)| **s == **v) {
            let h = *h;
            enc.by_ptr.insert(ptr, h);
            return Some(Ok(h));
        }
        let json = match serde_json::to_vec(v.as_ref()) {
            Ok(j) => j,
            Err(err) => return Some(Err(err.to_string())),
        };
        let h = hash(&json);
        enc.by_ptr.insert(ptr, h);
        if enc.recent.len() >= 4 {
            enc.recent.remove(0);
        }
        enc.recent.push((v.clone(), h));
        if !known(h) {
            enc.new.entry(h).or_insert(json);
        }
        Some(Ok(h))
    })
}

pub fn serialize<S: Serializer>(v: &Arc<DevelopSettings>, s: S) -> Result<S::Ok, S::Error> {
    match reference(v) {
        Some(Ok(h)) => s.serialize_str(&format!("{h:032x}")),
        Some(Err(e)) => Err(serde::ser::Error::custom(e)),
        None => v.as_ref().serialize(s),
    }
}

struct V;

impl<'de> Visitor<'de> for V {
    type Value = Arc<DevelopSettings>;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("develop settings or a settings reference")
    }
    fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
        let h = u128::from_str_radix(s, 16).map_err(|_| E::custom(format!("bad settings reference {s:?}")))?;
        DECODE.with(|d| d.borrow().as_ref().and_then(|r| r.get(&h).cloned())).ok_or_else(|| E::custom(format!("unknown settings reference {s}")))
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        DevelopSettings::deserialize(de::value::MapAccessDeserializer::new(map)).map(Arc::new)
    }
}

pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<DevelopSettings>, D::Error> {
    d.deserialize_any(V)
}

/// The same for `Option<Arc<DevelopSettings>>`.
pub mod opt {
    use super::*;

    pub fn serialize<S: Serializer>(v: &Option<Arc<DevelopSettings>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => super::serialize(v, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<DevelopSettings>>, D::Error> {
        #[derive(Deserialize)]
        struct W(#[serde(deserialize_with = "super::deserialize")] Arc<DevelopSettings>);
        Ok(Option::<W>::deserialize(d)?.map(|w| w.0))
    }
}
