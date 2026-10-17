//! One develop settings value for all the photos whose settings are equal.
//!
//! A photo holds its settings behind an `Arc`, and so do its history steps, versions and import
//! look. Most photos of a library were never edited: without sharing, each holds a copy of the
//! same defaults (1.5 kB of the 2.9 kB an unedited JPEG's record takes in memory). Here every
//! settings value that enters a catalog, read from a saved library or given by a change, is
//! replaced by the one already held when an equal one is.
//!
//! Sharing never lets one photo's edit reach another: settings behind an `Arc` are not changed
//! in place (the catalog replaces them; `Arc::make_mut` copies a value that is shared).
//!
//! **Equal means the same down to the JSON.** `==` takes -0.0 for 0.0, which the saved form, a
//! photo's fingerprint and the preview-cache key tell apart. Settings holding a negative zero
//! are left alone: never shared, never what others share.
//!
//! What is held to compare with: the defaults, what settings with nothing stated read as (`{}`
//! in a saved library: recognised without reading them, see [`deserialize`]), and on each thread
//! the last few values seen (a camera's import defaults, a photo's own history). The thread's
//! list keeps nothing alive: a value no photo holds any more is gone from it too.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock, Weak};

use lightcraft_develop::DevelopSettings;
use serde::de::{Deserialize, Deserializer, IntoDeserializer, MapAccess, SeqAccess, Visitor};

use crate::{HistoryStep, Photo, Version};

/// How many values a thread remembers besides the defaults and the current photo's.
const RECENT: usize = 4;

fn held_default() -> &'static Arc<DevelopSettings> {
    static DEFAULT: OnceLock<Arc<DevelopSettings>> = OnceLock::new();
    DEFAULT.get_or_init(|| Arc::new(DevelopSettings::default()))
}

/// The default settings, held once.
pub fn default_settings() -> Arc<DevelopSettings> {
    held_default().clone()
}

/// What settings with nothing stated read as (the defaults, on the first rendering process),
/// held once; `None` if `{}` should ever stop being settings.
fn nothing_stated() -> Option<&'static Arc<DevelopSettings>> {
    static NOTHING: OnceLock<Option<Arc<DevelopSettings>>> = OnceLock::new();
    NOTHING
        .get_or_init(|| {
            let read: DevelopSettings = serde_json::from_str("{}").ok()?;
            // (one value where that is the defaults)
            Some(if read == **held_default() && !crate::stored::has_negative_zero(&read) { default_settings() } else { Arc::new(read) })
        })
        .as_ref()
}

/// What a thread has seen: pointers that keep nothing alive (a value the catalog let go of is
/// gone from here too). None of the values holds a negative zero.
struct Seen {
    /// The settings of the photo being read or added: its history (up to 200 steps, each seen
    /// after it) usually ends with them.
    current: Option<Weak<DevelopSettings>>,
    /// The last few values, the latest first.
    recent: Vec<Weak<DevelopSettings>>,
}

thread_local! {
    static SEEN: RefCell<Seen> = const { RefCell::new(Seen { current: None, recent: Vec::new() }) };
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
    let negative_zero = std::cell::OnceCell::new();
    let exact = || !*negative_zero.get_or_init(|| crate::stored::has_negative_zero(settings.as_ref()));
    let same = |held: &Arc<DevelopSettings>| Arc::ptr_eq(held, &settings) || (**held == *settings && exact());
    let held = [Some(held_default()), nothing_stated()].into_iter().flatten().find(|h| same(h)).cloned();
    let found = SEEN.try_with(|seen| {
        let mut seen = seen.try_borrow_mut().ok()?;
        let found = held.clone().or_else(|| seen.current.as_ref().and_then(Weak::upgrade).filter(|s| same(s))).or_else(|| {
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
            seen.current = match (&held, &found) {
                // (the held values are compared with anyway)
                (Some(_), _) => None,
                (None, Some(f)) => Some(Arc::downgrade(f)),
                (None, None) => exact().then(|| Arc::downgrade(&settings)),
            };
        }
        found
    });
    // (a thread shutting down: the held value if it is one, else as it came)
    found.ok().flatten().or(held).unwrap_or(settings)
}

/// Share the settings of a history step.
pub(crate) fn step(step: &mut HistoryStep) {
    step.settings = settings(step.settings.clone());
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

/// Settings read from a saved library (`deserialize_with`): shared. Settings with nothing
/// stated, which is what an unedited photo's are saved as, are not read at all: they are the
/// one value held for them.
pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<DevelopSettings>, D::Error> {
    read(d, false)
}

/// [`deserialize`], the settings being a photo's own (`current`) or not.
fn read<'de, D: Deserializer<'de>>(d: D, current: bool) -> Result<Arc<DevelopSettings>, D::Error> {
    struct Shared(bool);

    impl<'de> Visitor<'de> for Shared {
        type Value = Arc<DevelopSettings>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("develop settings")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let Some(first) = map.next_key::<String>()? else {
                return match nothing_stated() {
                    // (through `share` only for a photo's own: to say they are the current ones)
                    Some(held) if self.0 => Ok(share(held.clone(), true)),
                    Some(held) => Ok(held.clone()),
                    None => DevelopSettings::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(|s| share(Arc::new(s), self.0)),
                };
            };
            let rest = Rest { first: Some(first), map };
            DevelopSettings::deserialize(serde::de::value::MapAccessDeserializer::new(rest)).map(|s| share(Arc::new(s), self.0))
        }

        // (a struct can be saved as a list of its fields: nothing here writes that, and it reads as it did)
        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            DevelopSettings::deserialize(serde::de::value::SeqAccessDeserializer::new(seq)).map(|s| share(Arc::new(s), self.0))
        }
    }

    /// A map whose first key was already taken off it.
    struct Rest<A> {
        first: Option<String>,
        map: A,
    }

    impl<'de, A: MapAccess<'de>> MapAccess<'de> for Rest<A> {
        type Error = A::Error;

        fn next_key_seed<K: serde::de::DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error> {
            match self.first.take() {
                Some(first) => seed.deserialize(first.into_deserializer()).map(Some),
                None => self.map.next_key_seed(seed),
            }
        }

        fn next_value_seed<V: serde::de::DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Self::Error> {
            self.map.next_value_seed(seed)
        }

        fn size_hint(&self) -> Option<usize> {
            self.map.size_hint().map(|n| n + usize::from(self.first.is_some()))
        }
    }

    // A library that states other defaults than this build's (`stored::reading_over`): its
    // settings are read over its own, so nothing stated is not this build's nothing stated.
    if crate::stored::reading_now().is_some() {
        return crate::stored::read_settings(d).map(|s| share(s, current));
    }
    d.deserialize_any(Shared(current))
}

/// [`deserialize`] for a photo's own settings: what its history, read after them, is compared
/// with first.
pub(crate) fn deserialize_current<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<DevelopSettings>, D::Error> {
    read(d, true)
}

/// [`deserialize`] for settings a photo may not have (its import look).
pub(crate) fn deserialize_optional<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<DevelopSettings>>, D::Error> {
    #[derive(serde::Deserialize)]
    struct Held(#[serde(deserialize_with = "deserialize")] Arc<DevelopSettings>);
    Ok(Option::<Held>::deserialize(d)?.map(|h| h.0))
}
