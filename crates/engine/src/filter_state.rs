//! The library filter and the one Enable Filters put aside, behind one door.
//!
//! The filter is session state with owners besides the filter bar: the source shown supplies some of
//! its fields itself (the album shown, the library folder shown, Picks' flag). A copy left in the
//! filter would be hidden state: the filter bar has no chip for an album, and for a folder or a flag the
//! source shown replaces it on screen, so it would narrow the next source with nothing to explain why.
//! Every write to the filter therefore goes through [`FilterState`], whose fields stay private to this module, so
//! no writer can skip [`LibrarySource::reconcile`]. The source itself is shown through `library.source` (and a
//! library opening), which reconcile the filter against it: a new way to change `Session::source` does the same.
//!
//! That covers the three fields of [`SourceOwned`], not every field a source overwrites: `deleted`, `importedFrom`
//! and `folder`/`subfolders` (the Local folder shown, which `Session::browse` supplies) are replaced by their sources
//! too, but are not listed there and have no chip, so `library.filter` still accepts them.

use lightcraft_catalog::Filter;

use crate::LibrarySource;

/// A filter field its source owns (see [`LibrarySource::reconcile`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceOwned {
    Album,
    LibraryFolder,
    Flag,
}

impl SourceOwned {
    /// The field's name in commands (`album`, `libraryFolder`, `flag`).
    pub fn name(self) -> &'static str {
        match self {
            SourceOwned::Album => "album",
            SourceOwned::LibraryFolder => "libraryFolder",
            SourceOwned::Flag => "flag",
        }
    }

    /// Why a command that sets the field is refused.
    pub fn refusal(self) -> &'static str {
        match self {
            SourceOwned::Album => "the filter holds no album: show one with library.source {kind: album}",
            SourceOwned::LibraryFolder => "a folder is already shown (library.source): show another source first",
            SourceOwned::Flag => "Picks is already shown (library.source): show another source first",
        }
    }
}

impl LibrarySource {
    /// The fields of `f` that this source supplies itself: the ones [`LibrarySource::reconcile`] takes out.
    /// - album: the album shown replaces the filter's; under any other source it would narrow the view with
    ///   no chip to say so, so the filter never holds one.
    /// - library folder: the folder shown replaces the filter's.
    /// - flag: Picks shows the picked photos whatever flag the filter asks for.
    pub fn owned_fields_set_in(&self, f: &Filter) -> Vec<SourceOwned> {
        let mut set = Vec::new();
        if f.album.is_some() {
            set.push(SourceOwned::Album);
        }
        if f.library_folder.is_some() && *self == LibrarySource::LibraryFolder {
            set.push(SourceOwned::LibraryFolder);
        }
        if f.flag.is_some() && *self == LibrarySource::Picks {
            set.push(SourceOwned::Flag);
        }
        set
    }

    /// Take out of `f` what this source owns; returns what was taken out.
    pub fn reconcile(&self, f: &mut Filter) -> Vec<SourceOwned> {
        let owned = self.owned_fields_set_in(f);
        for field in &owned {
            match field {
                SourceOwned::Album => f.album = None,
                SourceOwned::LibraryFolder => f.library_folder = None,
                SourceOwned::Flag => f.flag = None,
            }
        }
        owned
    }
}

/// The filter being shown and the one View ▸ Enable Filters put aside (not persisted).
#[derive(Debug, Default)]
pub struct FilterState {
    current: Filter,
    aside: Option<Filter>,
}

impl FilterState {
    pub fn current(&self) -> &Filter {
        &self.current
    }

    /// The filter Enable Filters put aside.
    pub fn aside(&self) -> Option<&Filter> {
        self.aside.as_ref()
    }

    /// Show `f` as the filter, over `source`. What the source owns is left out and named in the answer.
    /// Setting a filter forgets the one put aside.
    pub fn set(&mut self, source: LibrarySource, mut f: Filter) -> Vec<SourceOwned> {
        let left_out = source.reconcile(&mut f);
        self.current = f;
        self.aside = None;
        left_out
    }

    /// [`FilterState::set`] with `edit` applied to a copy of the current filter.
    pub fn change(&mut self, source: LibrarySource, edit: impl FnOnce(&mut Filter)) -> Vec<SourceOwned> {
        let mut f = self.current.clone();
        edit(&mut f);
        self.set(source, f)
    }

    /// No filter, none put aside.
    pub fn clear(&mut self) {
        self.current = Filter::default();
        self.aside = None;
    }

    /// `source` was shown: the filter lets go of what that source owns. The one put aside is
    /// reconciled when it comes back, against the source shown then.
    pub fn source_shown(&mut self, source: LibrarySource) {
        source.reconcile(&mut self.current);
    }

    /// Enable Filters: a filter set is put aside, one put aside comes back as it was except what the
    /// source shown now owns. Returns whether filters are on afterwards.
    pub fn toggle(&mut self, source: LibrarySource) -> bool {
        if self.current != Filter::default() {
            self.aside = Some(std::mem::take(&mut self.current));
        } else if let Some(mut f) = self.aside.take() {
            source.reconcile(&mut f);
            self.current = f;
        }
        self.aside.is_none()
    }

    /// An entity the filter names changed (a keyword or person renamed, a folder moved or removed): `edit` is
    /// applied to the filter and to the one put aside, and to nothing else. It must not set a field a source
    /// owns.
    pub fn follow(&mut self, mut edit: impl FnMut(&mut Filter)) {
        edit(&mut self.current);
        if let Some(f) = self.aside.as_mut() {
            edit(f);
        }
    }

    /// A put-aside filter that [`FilterState::follow`] left empty has nothing to bring back.
    pub fn drop_aside_if_empty(&mut self) {
        if self.aside.as_ref() == Some(&Filter::default()) {
            self.aside = None;
        }
    }

    /// Enable Filters is off (a filter is put aside).
    pub fn is_off(&self) -> bool {
        self.aside.is_some()
    }
}
