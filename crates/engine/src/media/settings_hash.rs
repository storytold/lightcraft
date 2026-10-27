use super::*;

pub(super) const SETTINGS_HASH_ENTRIES: usize = 1024;

#[derive(Default)]
pub(super) struct SettingsHashes {
    pub(super) entries: HashMap<PhotoId, (Weak<DevelopSettings>, u64)>,
    pub(super) order: VecDeque<PhotoId>,
    #[cfg(test)]
    pub(super) computations: usize,
}

impl SettingsHashes {
    pub(super) fn get(&mut self, id: PhotoId, settings: &Arc<DevelopSettings>) -> u64 {
        if let Some((old, hash)) = self.entries.get(&id)
            && old.as_ptr() == Arc::as_ptr(settings)
        {
            return *hash;
        }
        let hash = settings.hash64();
        #[cfg(test)]
        {
            self.computations += 1;
        }
        if !self.entries.contains_key(&id) {
            if self.entries.len() >= SETTINGS_HASH_ENTRIES
                && let Some(oldest) = self.order.pop_front()
            {
                self.entries.remove(&oldest);
            }
            self.order.push_back(id);
        }
        self.entries.insert(id, (Arc::downgrade(settings), hash));
        hash
    }
}
