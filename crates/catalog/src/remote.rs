//! Remote identities: which asset on which remote service (an Immich server, …) a photo is.
//!
//! One record per (photo, service, account), indexed both ways: from a photo to its remote
//! assets, and from a remote asset (service, account, remote id) back to the photo. Changed
//! through [`crate::Op::SetRemote`], so links are undoable, logged (the op log doubles as the
//! sync change feed) and persisted in both catalog formats (v4: the `remote_identity` table).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::PhotoId;

/// Where a link stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Both sides match as of `last_synced_at`.
    #[default]
    Synced,
    /// Changed locally since the last sync: to upload.
    LocalChanged,
    /// Changed remotely: to download.
    RemoteChanged,
    /// Both changed: needs a decision.
    Conflict,
    /// The remote asset is gone (deleted or no longer visible to the account).
    RemoteMissing,
    /// Matched without a checksum (file name, capture time and size agree): the user should
    /// confirm the link before anything is synced through it.
    Probable,
}

/// One link between a photo and an asset on a remote service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteIdentity {
    pub photo_id: PhotoId,
    /// The service kind, e.g. `immich`.
    pub service: String,
    /// The account on it (server URL + user id, …: whatever makes remote ids unique).
    pub account_id: String,
    pub remote_id: String,
    /// The remote's checksum of the asset (Immich: SHA-1, base64), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_checksum: Option<String>,
    /// The remote's modification time (ISO 8601), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_updated_at: Option<String>,
    /// When this link was last synced (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,
    #[serde(default)]
    pub sync_state: SyncState,
}

/// The forward key: a photo's link on one account.
pub type RemoteKey = (PhotoId, String, String);

impl RemoteIdentity {
    pub fn key(&self) -> RemoteKey {
        (self.photo_id, self.service.clone(), self.account_id.clone())
    }
    fn reverse_key(&self) -> (String, String, String) {
        (self.service.clone(), self.account_id.clone(), self.remote_id.clone())
    }
}

/// All links, indexed both ways. Serialized as a plain list (the reverse index is rebuilt).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteTable {
    by_photo: BTreeMap<RemoteKey, RemoteIdentity>,
    by_remote: BTreeMap<(String, String, String), PhotoId>,
}

impl RemoteTable {
    /// How photo `id` is linked to `service` on any account: `linked` (a confirmed link),
    /// `probable` (only links waiting for confirmation) or `none`.
    pub fn link_state(&self, id: PhotoId, service: &str) -> &'static str {
        let mut probable = false;
        for r in self.of_photo(id).filter(|r| r.service == service) {
            if r.sync_state == SyncState::Probable {
                probable = true;
            } else {
                return "linked";
            }
        }
        if probable { "probable" } else { "none" }
    }

    pub fn is_empty(&self) -> bool {
        self.by_photo.is_empty()
    }
    pub fn len(&self) -> usize {
        self.by_photo.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = &RemoteIdentity> {
        self.by_photo.values()
    }
    pub fn get(&self, key: &RemoteKey) -> Option<&RemoteIdentity> {
        self.by_photo.get(key)
    }
    /// Every link of one photo.
    pub fn of_photo(&self, id: PhotoId) -> impl Iterator<Item = &RemoteIdentity> {
        let lo = (id, String::new(), String::new());
        self.by_photo.range(lo..).take_while(move |(k, _)| k.0 == id).map(|(_, v)| v)
    }
    /// The photo linked to a remote asset.
    pub fn photo_of(&self, service: &str, account_id: &str, remote_id: &str) -> Option<PhotoId> {
        self.by_remote.get(&(service.to_string(), account_id.to_string(), remote_id.to_string())).copied()
    }
    /// Insert or replace (by forward key); returns the previous record. A remote asset links to
    /// one photo only: a record whose remote id another photo already holds is refused.
    pub(crate) fn upsert(&mut self, r: RemoteIdentity) -> std::result::Result<Option<RemoteIdentity>, String> {
        if let Some(other) = self.by_remote.get(&r.reverse_key())
            && *other != r.photo_id
        {
            return Err(format!("{} asset {} is already linked to photo {}", r.service, r.remote_id, other.0));
        }
        let old = self.by_photo.insert(r.key(), r.clone());
        if let Some(o) = &old {
            self.by_remote.remove(&o.reverse_key());
        }
        self.by_remote.insert(r.reverse_key(), r.photo_id);
        Ok(old)
    }
    pub(crate) fn remove(&mut self, key: &RemoteKey) -> Option<RemoteIdentity> {
        let old = self.by_photo.remove(key)?;
        self.by_remote.remove(&old.reverse_key());
        Some(old)
    }
    /// The forward keys of a photo's links (to drop them with the photo).
    pub fn keys_of(&self, id: PhotoId) -> Vec<RemoteKey> {
        self.of_photo(id).map(RemoteIdentity::key).collect()
    }
}

impl FromIterator<RemoteIdentity> for RemoteTable {
    fn from_iter<I: IntoIterator<Item = RemoteIdentity>>(it: I) -> Self {
        let mut t = RemoteTable::default();
        for r in it {
            // a duplicate remote id in stored data: the first link wins
            let _ = t.upsert(r);
        }
        t
    }
}

impl Serialize for RemoteTable {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_seq(self.by_photo.values())
    }
}

impl<'de> Deserialize<'de> for RemoteTable {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Vec::<RemoteIdentity>::deserialize(d)?.into_iter().collect())
    }
}

/// The previews index: what preview the cache holds for a photo (so it can be rebuilt,
/// discarded or checked without scanning the cache).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewEntry {
    /// Long edge of the standard preview (pixels); 0 = none.
    pub size: u32,
    /// A 1:1 preview exists.
    #[serde(default)]
    pub one_to_one: bool,
    /// Hash of the develop settings it was rendered with.
    pub settings_hash: u64,
    /// When it was built (ISO 8601).
    pub built_at: String,
}
