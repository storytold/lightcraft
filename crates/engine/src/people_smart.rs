//! The library's people store ([`crate::people_store`]) on the session: loaded when first
//! needed, saved on every change, and who a mask's `who` (a name, or a role in this shoot) is on
//! a photo. A damaged `people.json` is left as it is and never overwritten.
//!
//! Ported from OneSlips' fork (own design there).

use lcc_catalog::PhotoId;
use lcc_develop::recipe::Who;

use crate::Session;
use crate::people_store::{PeopleStore, PhotoContext};

#[derive(Default)]
pub(crate) struct PeopleState {
    /// The library directory the store was loaded for (`Some(None)`: no library).
    loaded_for: Option<Option<std::path::PathBuf>>,
    store: PeopleStore,
    /// The file couldn't be read: it isn't overwritten.
    blocked: bool,
    /// What went wrong loading it, for the user.
    pub(crate) messages: Vec<String>,
}

/// Who a `who` names on one photo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// This person (a name the photo's Face regions use).
    Person(String),
    /// Nobody is bound to the role here: the selection covers everyone, and why.
    Everyone(String),
}

impl Session {
    /// What a photo's scope (for roles) is judged by.
    pub fn photo_context(&self, id: PhotoId) -> PhotoContext {
        let Some(p) = self.catalog.photo(id) else { return PhotoContext { id: id.0, ..Default::default() } };
        let folder = match &p.source {
            lcc_catalog::Source::File { path } => {
                let n = crate::people_store::norm_folder(path);
                n.rsplit_once('/').map(|(dir, _)| dir.to_string())
            }
            _ => None,
        };
        PhotoContext { id: id.0, folder, albums: self.catalog.albums_of(id).into_iter().map(|a| a.0).collect(), date: p.date().to_string() }
    }

    fn people_load(&mut self) {
        let dir = self.library.as_ref().map(|l| l.dir.clone());
        if self.people.loaded_for.as_ref() == Some(&dir) {
            return;
        }
        self.people = PeopleState { loaded_for: Some(dir), ..Default::default() };
        let Some(lib) = self.library.as_mut() else { return };
        match lib.read_file(crate::people_store::FILE) {
            Ok(None) => {}
            Ok(Some(bytes)) => match PeopleStore::from_bytes(&bytes) {
                Ok(s) => self.people.store = s,
                Err(e) => {
                    log::error!("{e}");
                    self.people.messages.push(format!("{e}. It was left as it is; the list starts empty."));
                    self.people.blocked = true;
                }
            },
            Err(e) => {
                log::error!("{}: {e}", crate::people_store::FILE);
                self.people.blocked = true;
            }
        }
    }

    /// The people store (roles, appearance examples).
    pub fn people_store(&mut self) -> &PeopleStore {
        self.people_load();
        &self.people.store
    }

    /// Change the people store and save it.
    pub fn with_people<T>(&mut self, f: impl FnOnce(&mut PeopleStore) -> T) -> Result<T, String> {
        self.people_load();
        if self.people.blocked {
            return Err(format!("{} couldn't be read, so it isn't overwritten; move it aside to start a new one", crate::people_store::FILE));
        }
        let out = f(&mut self.people.store);
        let bytes = self.people.store.to_bytes();
        if let Some(lib) = self.library.as_mut() {
            lib.write_file(crate::people_store::FILE, &bytes).map_err(|e| format!("couldn't save {}: {e}", crate::people_store::FILE))?;
        }
        Ok(out)
    }

    /// The automatic name of mask `m` on photo `id`: what it selects, People with their role and
    /// the name it has in this shoot ("Groom Jamie - Clothes, Hair").
    pub fn auto_mask_name(&mut self, id: PhotoId, m: &lcc_develop::Mask) -> String {
        let mut m = m.clone();
        if m.components.iter().any(|c| matches!(c.shape, lcc_develop::MaskShape::People { who: Some(Who::Role(_)), .. })) {
            let ctx = self.photo_context(id);
            for c in &mut m.components {
                if let lcc_develop::MaskShape::People { who: Some(Who::Role(r)), .. } = &mut c.shape
                    && let Some(n) = self.people_store().role_for(r, &ctx).map(str::to_string)
                {
                    *r = format!("{} {n}", r.trim());
                }
            }
        }
        lcc_develop::recipe::content_name(&m)
    }

    /// Give the masks of `after` that still carry an automatic name (a placeholder such as
    /// "Mask 3", or the name they had automatically in `before`, numbered or not) the name of what
    /// they select now; a name the photographer gave is kept. A name another mask of the photo
    /// has already gets a number ("Brush 2", issue #1268); a mask whose name doesn't change keeps
    /// it, so masks that shared a name before (imported ones too) keep theirs.
    pub(crate) fn refresh_mask_names(&mut self, id: PhotoId, before: &[lcc_develop::Mask], after: &mut [lcc_develop::Mask]) {
        use lcc_develop::recipe::{generic_name, numbered_name, unique_name};
        for i in 0..after.len() {
            let Some(m) = after.get(i).filter(|m| !m.components.is_empty()).cloned() else { continue };
            let was = before.iter().find(|b| b.id == m.id).filter(|b| !b.components.is_empty()).map(|b| self.auto_mask_name(id, b));
            let placeholder = generic_name(&m.name);
            if !placeholder && !was.as_deref().is_some_and(|w| numbered_name(&m.name, w)) {
                continue;
            }
            let base = self.auto_mask_name(id, &m);
            if !placeholder && was.as_deref() == Some(base.as_str()) {
                // selects what it selected: its name (and number) stays
                continue;
            }
            let name = {
                let taken: Vec<&str> = after.iter().enumerate().filter(|(k, _)| *k != i).map(|(_, x)| x.name.as_str()).collect();
                unique_name(&base, &taken)
            };
            if let Some(m) = after.get_mut(i) {
                m.name = name;
            }
        }
    }

    /// Who `who` is on photo `id`.
    pub fn resolve_who(&mut self, id: PhotoId, who: &Who) -> Resolved {
        match who {
            Who::Name(n) => Resolved::Person(n.trim().to_string()),
            Who::Role(r) => {
                let ctx = self.photo_context(id);
                match self.people_store().role_for(r, &ctx) {
                    Some(n) => Resolved::Person(n.to_string()),
                    None => Resolved::Everyone(format!("No {} is set for this shoot, so the mask covers everyone (people.setRole)", r.trim())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lcc_develop::recipe::Who;
    use serde_json::json;

    use crate::Session;

    #[test]
    fn roles_resolve_per_photo_and_survive_hostile_arguments() {
        let mut s = Session::with_demo();
        let id = s.active().expect("demo photo");
        let set = s.execute("people.setRole", &json!({"role": "Bride", "person": "Sam", "scope": {"ids": [id.0]}})).expect("set");
        assert_eq!(set["person"], "Sam");
        assert_eq!(s.resolve_who(id, &Who::Role("bride".into())), super::Resolved::Person("Sam".into()));
        assert!(matches!(s.resolve_who(id, &Who::Role("Groom".into())), super::Resolved::Everyone(_)));
        let roles = s.execute("people.roles", &json!({})).expect("roles");
        assert_eq!(roles["here"][0]["person"], "Sam");
        for bad in [
            json!({}),
            json!({"role": "", "person": "x"}),
            json!({"role": "x", "person": "y", "scope": 7}),
            json!({"role": "x".repeat(300), "person": "y"}),
        ] {
            assert!(s.execute("people.setRole", &bad).is_err(), "{bad}");
        }
        assert_eq!(s.execute("people.clearRole", &json!({"role": "BRIDE"})).expect("clear")["removed"], 1);
        assert!(s.execute("people.forgetAll", &json!({})).is_ok());
        assert!(s.people_store().roles.is_empty());
    }
}
