//! Derived ignored-region keys. Never serialized; changes are followed photo by photo.
use std::collections::BTreeMap;

use lightcraft_catalog::{Aspects, Catalog, PhotoId};

#[derive(Default)]
pub(crate) struct Index {
    stamp: Option<(u64, u64)>,
    photos: BTreeMap<PhotoId, Vec<usize>>,
    total: usize,
    #[cfg(test)]
    pub scanned: usize,
}

impl Index {
    fn update(&mut self, catalog: &Catalog, id: PhotoId) {
        #[cfg(test)]
        {
            self.scanned += 1;
        }
        if let Some(old) = self.photos.remove(&id) {
            self.total = self.total.saturating_sub(old.len());
        }
        let Some(photo) = catalog.photo(id).filter(|photo| photo.in_library()) else { return };
        let keys: Vec<_> = photo
            .meta
            .regions
            .iter()
            .enumerate()
            .filter(|(_, region)| region.kind == lightcraft_meta::RegionKind::IgnoredFace)
            .map(|(index, _)| index)
            .collect();
        self.total = self.total.saturating_add(keys.len());
        if !keys.is_empty() {
            self.photos.insert(id, keys);
        }
    }

    pub fn page(&mut self, catalog: &Catalog, generation: u64, offset: usize, limit: usize) -> (usize, Vec<(PhotoId, usize)>) {
        let stamp = (generation, catalog.revision);
        if self.stamp != Some(stamp) {
            let changes = self.stamp.filter(|(g, _)| *g == generation).and_then(|(_, revision)| {
                let aspects = catalog.aspects_since(revision)?;
                if aspects.any_of(Aspects::META.with(Aspects::PRESENCE)) { catalog.touched_since(revision) } else { Some(Vec::new()) }
            });
            match changes {
                Some(ids) => {
                    for id in ids {
                        self.update(catalog, id);
                    }
                }
                None => {
                    self.photos.clear();
                    self.total = 0;
                    for photo in catalog.photos() {
                        self.update(catalog, photo.id);
                    }
                }
            }
            self.stamp = Some(stamp);
        }
        // Skip whole photos (and slice within one), not every preceding face on a late page.
        let mut skip = offset;
        let mut keys = Vec::with_capacity(limit);
        for (id, indices) in &self.photos {
            if skip >= indices.len() {
                skip -= indices.len();
                continue;
            }
            keys.extend(indices.get(skip..).unwrap_or_default().iter().take(limit.saturating_sub(keys.len())).map(|index| (*id, *index)));
            skip = 0;
            if keys.len() == limit {
                break;
            }
        }
        (self.total, keys)
    }
}
