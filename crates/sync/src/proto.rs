//! What two devices say to each other over the encrypted channel.
//!
//! A session: both send [`Msg::Hello`]; then rounds in which each sends a [`Msg::Batch`] (what
//! changed, and how far it has taken in what the other sent); then the device that started the
//! session asks for files with [`Msg::Gets`], each answered by [`Msg::Blob`] and the bytes, or
//! [`Msg::NotHere`]; then roles swap; [`Msg::Bye`] ends it.

use serde::{Deserialize, Serialize};

use crate::clock::MAX_SKEW_MS;
use crate::model::Group;
use crate::pill::Depth;
use crate::replica::Item;

/// The protocol's version: devices with different versions don't exchange.
pub const VERSION: u32 = 2;
/// The most items in one batch.
pub const MAX_ITEMS: usize = 400;
/// File bytes travel in frames of this size.
pub const CHUNK: usize = 1 << 20;
/// The largest file sent (a hostile size is refused before any byte is read).
pub const MAX_BLOB: u64 = 64 << 30;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub proto: u32,
    /// The app's version: records only merge between the same version (a different one could
    /// drop settings it doesn't know).
    pub app: String,
    /// This device's name.
    pub name: String,
    /// Its wall clock (ms since 1970).
    pub now_ms: u64,
    /// The group of its open catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The open catalogue's name.
    #[serde(default)]
    pub catalogue: String,
    /// It asks the other device to join this group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<Group>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Batch {
    #[serde(default)]
    pub items: Vec<Item>,
    /// Confirming this number confirms the items (see `Replica::outgoing`).
    #[serde(default)]
    pub upto: u64,
    /// More waits than fitted.
    #[serde(default)]
    pub more: bool,
    /// The sender took in the other side's batches up to this number of theirs.
    #[serde(default)]
    pub ack: Option<u64>,
    /// Immich connections the sender shares with the group, key and all. Only ever sent over
    /// the encrypted, authenticated peer channel, to a device of the group; never stored in the
    /// replica.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<SharedAccount>,
}

/// The most shared Immich connections one batch carries, and the longest field taken.
pub const MAX_SHARED: usize = 8;
pub const MAX_SHARED_FIELD: usize = 2048;

/// An Immich connection one device shares with the others of its group.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedAccount {
    pub url: String,
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    /// The API key.
    pub key: String,
    /// A certificate fingerprint the sharing device confirmed (self-signed servers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
}

impl SharedAccount {
    /// Whether every field is within bounds (a hostile peer's are dropped).
    pub fn sane(&self) -> bool {
        let ok = |s: &str| s.len() <= MAX_SHARED_FIELD && !s.chars().any(char::is_control);
        !self.url.is_empty()
            && !self.user_id.is_empty()
            && self.user_id.len() <= 128
            && !self.key.is_empty()
            && ok(&self.url)
            && ok(&self.user_id)
            && ok(&self.user_name)
            && ok(&self.key)
            && self.pinned.as_deref().is_none_or(ok)
    }
}

/// Never prints the key.
impl std::fmt::Debug for SharedAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedAccount").field("url", &self.url).field("user_id", &self.user_id).field("key", &"<hidden>").finish()
    }
}

/// One file asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Get {
    pub key: String,
    pub depth: Depth,
    #[serde(default)]
    pub offset: u64,
}

/// The most files asked for in one message.
pub const MAX_GETS: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "t")]
pub enum Msg {
    Hello(Hello),
    Batch(Batch),
    /// Send me these photos' files, each from its offset on; they are answered in order.
    Gets {
        wants: Vec<Get>,
    },
    /// The file follows, `size` bytes in all, from `offset`, in raw frames.
    Blob {
        key: String,
        depth: Depth,
        size: u64,
        offset: u64,
    },
    /// Not sent: it isn't here (`later`: it is being made, ask again), or it isn't allowed.
    NotHere {
        key: String,
        depth: Depth,
        later: bool,
    },
    /// Nothing more to ask; the other side may ask now.
    Turn,
    Bye {
        reason: String,
    },
}

/// Why two devices that reached each other don't exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Protocol,
    /// The other device runs this app version.
    Version(String),
    /// The clocks differ by this many minutes.
    Clock(u64),
}

/// Whether a session with the device that sent `theirs` can go on.
pub fn check(mine: &Hello, theirs: &Hello) -> Result<(), Refusal> {
    if theirs.proto != mine.proto {
        return Err(Refusal::Protocol);
    }
    if theirs.app != mine.app {
        return Err(Refusal::Version(theirs.app.chars().take(32).collect()));
    }
    let skew = mine.now_ms.abs_diff(theirs.now_ms);
    if skew > MAX_SKEW_MS {
        return Err(Refusal::Clock(skew / 60_000));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(app: &str, now_ms: u64) -> Hello {
        Hello { proto: VERSION, app: app.into(), name: "d".into(), now_ms, group: None, catalogue: String::new(), invite: None }
    }

    #[test]
    fn devices_with_other_versions_or_clocks_are_refused_with_the_reason() {
        let me = hello("26.47.0", 1_000_000_000);
        assert_eq!(check(&me, &hello("26.47.0", 1_000_060_000)), Ok(()));
        assert_eq!(check(&me, &hello("26.46.2", 1_000_000_000)), Err(Refusal::Version("26.46.2".into())));
        assert_eq!(check(&me, &hello("26.47.0", 1_000_000_000 + 3_600_000)), Err(Refusal::Clock(60)));
        assert_eq!(check(&me, &Hello { proto: 1, ..hello("26.47.0", 1_000_000_000) }), Err(Refusal::Protocol));
    }

    #[test]
    fn hostile_messages_are_errors_not_panics() {
        for bad in
            ["", "{}", "[]", r#"{"t":"gets"}"#, r#"{"t":"blob","key":1}"#, r#"{"t":"batch","items":[{"photo":{}}]}"#, r#"{"t":"hello","proto":"x"}"#]
        {
            assert!(serde_json::from_str::<Msg>(bad).is_err(), "{bad}");
        }
        // unknown extra items in a batch fail the whole message (the session ends with an error)
        assert!(serde_json::from_str::<Msg>(r#"{"t":"batch","items":[{"later":{}}]}"#).is_err());
        assert!(serde_json::from_str::<Msg>(r#"{"t":"batch"}"#).is_ok());
    }
}
