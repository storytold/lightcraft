use super::{Arc, DevelopSettings, HashMap, Hasher128, PhotoId, VecDeque, Weak};

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

/// Add the rendering process `s` renders with in this build to persistent render key `h`, from
/// process V3 on: a process number from a newer LightCraft renders with an older build's latest
/// process, so the same settings (and settings hash) render differently in a build that knows that
/// process, and the keys must tell those renders apart. Builds before V3 (which have no process in
/// their keys) resolve every number above V2 to V2: naming V3 and later keeps this build's renders
/// of those numbers apart from theirs, and V1 and V2 keep the keys they always had, so their
/// cached renders stay valid (`docs/process-versions.md`).
pub(super) fn with_process<'a>(h: &'a mut Hasher128, s: &DevelopSettings) -> &'a mut Hasher128 {
    let v = s.process.process().version().0;
    if v >= lightcraft_develop::Process::V3.version().0 {
        h.u64(u64::from(v));
    }
    h
}
