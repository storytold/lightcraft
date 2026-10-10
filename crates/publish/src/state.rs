//! Where each photo of a published collection stands.

use std::collections::BTreeSet;

use dac_catalog::{AlbumId, Catalog, Photo, PhotoId, RemoteIdentity};
use serde::Serialize;

/// A photo's publish state in one collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PhotoState {
    /// In the collection, never published.
    New,
    /// Published, changed since (develop settings or metadata).
    Modified,
    Published,
    /// Published, then taken out of the collection (or deleted): to remove from the service.
    ToRemove,
}

/// A collection's photos by state (each list in collection order; `to_remove` by photo id).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub new: Vec<PhotoId>,
    pub modified: Vec<PhotoId>,
    pub published: Vec<PhotoId>,
    pub to_remove: Vec<PhotoId>,
}

impl Status {
    /// Something to send or remove.
    pub fn pending(&self) -> usize {
        self.new.len() + self.modified.len() + self.to_remove.len()
    }
    pub fn state_of(&self, id: PhotoId) -> Option<PhotoState> {
        [
            (&self.new, PhotoState::New),
            (&self.modified, PhotoState::Modified),
            (&self.published, PhotoState::Published),
            (&self.to_remove, PhotoState::ToRemove),
        ]
        .into_iter()
        .find(|(l, _)| l.contains(&id))
        .map(|(_, s)| s)
    }
}

/// The `account_id` of a collection's remote links.
pub fn account_id(service: &str, album: AlbumId) -> String {
    format!("{service}/{}", album.0)
}

/// What a re-publish depends on: the develop settings and the metadata a render carries (rating,
/// flag, label, title, caption, keywords, …). Hex SHA-1 of their JSON.
pub fn fingerprint(p: &Photo) -> String {
    let v = serde_json::json!({
        "develop": &*p.develop,
        "meta": &p.meta,
        "rating": p.rating,
        "flag": p.flag,
        "label": p.label,
        "captured": p.captured,
        "file": p.file_name,
    });
    let bytes = serde_json::to_vec(&v).unwrap_or_default();
    dac_hash::sha1_bytes(&bytes).to_hex()
}

/// The links of one collection.
pub fn links<'a>(cat: &'a Catalog, service: &str, album: AlbumId) -> impl Iterator<Item = &'a RemoteIdentity> {
    let acct = account_id(service, album);
    cat.remote_links().iter().filter(move |r| r.service == crate::SERVICE && r.account_id == acct)
}

/// Sort a collection's photos by publish state. A collection whose album is gone has only photos
/// to remove.
pub fn status(cat: &Catalog, service: &str, album: AlbumId) -> Status {
    let acct = account_id(service, album);
    let mut st = Status::default();
    let members: Vec<PhotoId> = cat.album(album).map(|a| a.photos.clone()).unwrap_or_default();
    let mut seen = BTreeSet::new();
    for id in members {
        if !seen.insert(id) {
            continue;
        }
        let Some(p) = cat.photo(id) else { continue };
        let link = cat.remote_links().get(&(id, crate::SERVICE.to_string(), acct.clone()));
        match (p.deleted, link) {
            (true, Some(_)) => st.to_remove.push(id),
            (true, None) => {}
            (false, None) => st.new.push(id),
            (false, Some(l)) if l.remote_checksum.as_deref() == Some(fingerprint(p).as_str()) => st.published.push(id),
            (false, Some(_)) => st.modified.push(id),
        }
    }
    for l in links(cat, service, album) {
        if !seen.contains(&l.photo_id) {
            st.to_remove.push(l.photo_id);
        }
    }
    st
}
