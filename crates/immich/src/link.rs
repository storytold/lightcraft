//! IMM-LINK: which catalog photo each Immich asset is. Pure: takes the catalog and a page of
//! assets, returns the links to write.
//!
//! - **Checksum** first: the asset's SHA-1 equals a photo's SHA-1 (the same bytes). Several photos
//!   with the same bytes (duplicates, virtual copies) all point at the asset… but a remote asset
//!   maps back to one photo, so the master (not a virtual copy, lowest id) gets the link.
//! - **Path** for assets of an Immich *external library*: Immich does not hash those files (their
//!   checksum is the SHA-1 of `path:` + the container path), so the asset's path, translated by the
//!   account's path mapping ([`crate::extlib`]), is matched to the photo's file. Same file: linked.
//! - **Probable** otherwise: same file name (case-insensitive), same capture time to the second,
//!   and same file size. Written with [`SyncState::Probable`] for the user to confirm.

use std::collections::HashMap;

use dac_catalog::{Catalog, Op, Photo, PhotoId, RemoteIdentity, SyncState};

use crate::types::Asset;

/// The service name links are written under.
pub const SERVICE: &str = "immich";

/// How an asset was matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    Checksum,
    /// An external-library asset whose (mapped) path is the photo's file.
    Path,
    Probable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub photo: PhotoId,
    pub asset_id: String,
    pub kind: MatchKind,
}

/// Lookup tables over a catalog, built once per link pass.
pub struct Index {
    by_sha1: HashMap<String, PhotoId>,
    by_name: HashMap<(String, String, u64), Vec<PhotoId>>,
    by_path: HashMap<String, PhotoId>,
    maps: Vec<crate::extlib::PathMap>,
}

/// A local path in comparable form (separators unified, no trailing slash).
fn path_key(p: &str) -> String {
    p.trim().replace('\\', "/").trim_end_matches('/').to_string()
}

fn eligible(p: &Photo) -> bool {
    p.in_library() && p.copy_of.is_none()
}

impl Index {
    pub fn new(cat: &Catalog) -> Index {
        Index::with_path_maps(cat, &[])
    }

    /// An index that also links external-library assets by path through `maps`.
    pub fn with_path_maps(cat: &Catalog, maps: &[crate::extlib::PathMap]) -> Index {
        let mut by_path: HashMap<String, PhotoId> = HashMap::new();
        let mut by_sha1: HashMap<String, PhotoId> = HashMap::new();
        let mut by_name: HashMap<(String, String, u64), Vec<PhotoId>> = HashMap::new();
        for p in cat.photos().filter(|p| eligible(p)) {
            if let Some(h) = &p.sha1 {
                let e = by_sha1.entry(h.to_ascii_lowercase()).or_insert(p.id);
                if p.id < *e {
                    *e = p.id;
                }
            }
            if !maps.is_empty()
                && let dac_catalog::Source::File { path } = &p.source
            {
                let e = by_path.entry(path_key(path)).or_insert(p.id);
                if p.id < *e {
                    *e = p.id;
                }
            }
            if let Some(t) = &p.captured
                && p.file_size > 0
            {
                by_name.entry((p.file_name.to_lowercase(), t.chars().take(19).collect(), p.file_size)).or_default().push(p.id);
            }
        }
        Index { by_sha1, by_name, by_path, maps: maps.to_vec() }
    }

    /// The photo `a` is, if any.
    pub fn find(&self, a: &Asset) -> Option<Match> {
        if let Some(h) = a.sha1_hex()
            && let Some(id) = self.by_sha1.get(&h)
        {
            return Some(Match { photo: *id, asset_id: a.id.clone(), kind: MatchKind::Checksum });
        }
        if a.library_id.is_some()
            && let Some(local) = crate::extlib::to_local(&self.maps, &a.original_path)
            && let Some(id) = self.by_path.get(&path_key(&local))
        {
            return Some(Match { photo: *id, asset_id: a.id.clone(), kind: MatchKind::Path });
        }
        let key = (a.original_file_name.to_lowercase(), a.local_capture()?, a.file_size()?);
        match self.by_name.get(&key).map(Vec::as_slice) {
            // two photos with the same name, time and size: can't tell which, leave it
            Some([one]) => Some(Match { photo: *one, asset_id: a.id.clone(), kind: MatchKind::Probable }),
            _ => None,
        }
    }

    /// Whether a photo with this SHA-1 (hex or base64) is in the catalog.
    pub fn has_sha1(&self, checksum: &str) -> Option<PhotoId> {
        let h = dac_hash::Sha1Digest::parse(checksum)?.to_hex();
        self.by_sha1.get(&h).copied()
    }
}

/// The ops that link `assets` on `account` (only those that change something: a new link, a
/// different asset, a newer `updatedAt`, a probable link that became a checksum match). A
/// confirmed link is never downgraded to probable, and a photo the user already linked to
/// another asset by checksum is left alone.
pub fn link_ops(cat: &Catalog, index: &Index, account: &str, assets: &[Asset], now: &str) -> (Vec<Op>, Vec<Match>) {
    let mut ops = Vec::new();
    let mut found = Vec::new();
    let mut taken: HashMap<PhotoId, bool> = HashMap::new();
    for a in assets.iter().filter(|a| !a.is_trashed) {
        let Some(m) = index.find(a) else { continue };
        // another asset already claimed this photo in this batch (duplicates on the server)
        if taken.get(&m.photo).is_some_and(|checksum| *checksum || m.kind == MatchKind::Probable) {
            continue;
        }
        let existing = cat.remote_of(m.photo).find(|r| r.service == SERVICE && r.account_id == account).cloned();
        // the asset is linked to another photo already
        if cat.photo_of_remote(SERVICE, account, &a.id).is_some_and(|p| p != m.photo) {
            continue;
        }
        let state = match m.kind {
            MatchKind::Checksum | MatchKind::Path => SyncState::Synced,
            MatchKind::Probable => SyncState::Probable,
        };
        if let Some(old) = &existing {
            let same_asset = old.remote_id == a.id;
            if !same_asset && (old.sync_state != SyncState::Probable || m.kind == MatchKind::Probable) {
                continue;
            }
            if same_asset && old.remote_updated_at == a.updated_at && (old.sync_state != SyncState::Probable || m.kind == MatchKind::Probable) {
                taken.insert(m.photo, m.kind != MatchKind::Probable);
                found.push(m);
                continue;
            }
        }
        let keep_state = existing.as_ref().filter(|o| o.remote_id == a.id && o.sync_state != SyncState::Probable).map(|o| o.sync_state);
        let r = RemoteIdentity {
            photo_id: m.photo,
            service: SERVICE.to_string(),
            account_id: account.to_string(),
            remote_id: a.id.clone(),
            remote_checksum: (!a.checksum.is_empty()).then(|| a.checksum.clone()),
            remote_updated_at: a.updated_at.clone(),
            last_synced_at: Some(now.to_string()),
            sync_state: keep_state.unwrap_or(state),
        };
        ops.push(Catalog::link_remote_op(r));
        taken.insert(m.photo, m.kind != MatchKind::Probable);
        found.push(m);
    }
    (ops, found)
}

/// The op that confirms a probable link (`None`: no probable link on that account).
pub fn confirm_op(cat: &Catalog, photo: PhotoId, account: &str) -> Option<Op> {
    let mut r = cat.remote_of(photo).find(|r| r.service == SERVICE && r.account_id == account && r.sync_state == SyncState::Probable)?.clone();
    r.sync_state = SyncState::Synced;
    Some(Catalog::link_remote_op(r))
}

/// The link state of a photo across Immich accounts: `linked`, `probable` or `none`.
pub fn link_state(cat: &Catalog, photo: PhotoId) -> &'static str {
    cat.remote_links().link_state(photo, SERVICE)
}
