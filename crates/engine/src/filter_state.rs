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

/// What [`FilterState::carry`] does with the filter once its fields are carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Carried {
    InPlace,
    /// The filter put aside is dropped.
    Drop,
    /// The filter in use is put aside (if it was empty before and holds something now, or `even_if_empty`).
    PutAside {
        even_if_empty: bool,
    },
}

/// The filter being shown and the one View ▸ Enable Filters put aside (not persisted).
#[derive(Debug, Default)]
pub struct FilterState {
    current: Filter,
    aside: Option<Filter>,
    /// How many times the person's filter was thrown away (replaced, emptied, or a put-aside one forgotten): what
    /// tells a filter built afterwards from the one a step left ([`crate::undo_filters`]). Putting a filter aside
    /// and bringing it back, and editing it, do not count.
    replaced: u64,
    /// `toggle` brought back a filter and the source shown took its last criterion out: the filter ended there, and
    /// the next one set is a new filter, not an edit of it (`set` counts it then).
    ended: bool,
    /// A top-level command is running ([`FilterState::begin_recording`]): the person's filter as it was before the
    /// first change the command made, copied only then.
    recording: bool,
    before: Option<Snapshot>,
}

/// The person's filter and whether Enable Filters has it put aside.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) filter: Filter,
    pub(crate) aside: bool,
}

impl FilterState {
    pub fn current(&self) -> &Filter {
        &self.current
    }

    /// The filter Enable Filters put aside.
    pub fn aside(&self) -> Option<&Filter> {
        self.aside.as_ref()
    }

    /// A command starts: the first change to the filter from now to [`FilterState::end_recording`] keeps what it was
    /// (nothing is copied for a command that does not change the filter).
    pub(crate) fn begin_recording(&mut self) {
        self.recording = true;
        self.before = None;
    }

    /// The command ended: the person's filter as it was before the command's first change, if it made one.
    pub(crate) fn end_recording(&mut self) -> Option<Snapshot> {
        self.recording = false;
        self.before.take()
    }

    /// Called before every change: keeps the filter as it was while a command is recording.
    fn touch(&mut self) {
        if self.recording && self.before.is_none() {
            self.before = Some(self.snapshot());
        }
    }

    /// The person's filter now.
    pub(crate) fn snapshot(&self) -> Snapshot {
        let (filter, aside) = self.person();
        Snapshot { filter: filter.clone(), aside }
    }

    /// How many times the person's filter was thrown away.
    pub(crate) fn replaced(&self) -> u64 {
        self.replaced
    }

    /// The person's filter (the one put aside while filters are off, else the one in use) and whether it is put aside.
    pub(crate) fn person(&self) -> (&Filter, bool) {
        match &self.aside {
            Some(f) => (f, true),
            None => (&self.current, false),
        }
    }

    /// Show `f` as an edit of the filter, over `source`: what the source owns is left out and named in the answer.
    /// Setting a filter forgets the one put aside.
    pub fn set(&mut self, source: LibrarySource, mut f: Filter) -> Vec<SourceOwned> {
        self.touch();
        let ended = self.ended;
        let left_out = source.reconcile(&mut f);
        // an edit that empties the filter ends it, as forgetting the one put aside does; so does a filter built after
        // `toggle` found the one it brought back emptied by the source
        self.replaced +=
            u64::from(self.aside.is_some() || (f == Filter::default() && self.current != Filter::default()) || (ended && f != Filter::default()));
        // (an edit that leaves the filter empty changes nothing: the filter is still the one that ended)
        self.ended = ended && f == Filter::default();
        self.current = f;
        self.aside = None;
        left_out
    }

    /// Show `f` in place of the filter, over `source` (a saved preset applied): [`FilterState::set`] for a filter
    /// that is not an edit of the one before.
    pub fn replace(&mut self, source: LibrarySource, f: Filter) -> Vec<SourceOwned> {
        self.replaced += 1;
        self.set(source, f)
    }

    /// [`FilterState::set`] with `edit` applied to a copy of the current filter.
    pub fn change(&mut self, source: LibrarySource, edit: impl FnOnce(&mut Filter)) -> Vec<SourceOwned> {
        let mut f = self.current.clone();
        edit(&mut f);
        self.set(source, f)
    }

    /// No filter, none put aside.
    pub fn clear(&mut self) {
        self.touch();
        self.replaced += u64::from(self.aside.is_some() || self.current != Filter::default());
        self.current = Filter::default();
        self.aside = None;
    }

    /// `source` was shown: the filter lets go of what that source owns. The one put aside is
    /// reconciled when it comes back, against the source shown then.
    pub fn source_shown(&mut self, source: LibrarySource) {
        self.touch();
        let had_criteria = self.current != Filter::default();
        source.reconcile(&mut self.current);
        // a source that takes the last criterion out ends the filter, as taking it off by hand does
        self.replaced += u64::from(had_criteria && self.current == Filter::default());
    }

    /// Enable Filters: a filter set is put aside, one put aside comes back as it was except what the
    /// source shown now owns. Returns whether filters are on afterwards.
    pub fn toggle(&mut self, source: LibrarySource) -> bool {
        self.touch();
        if self.current != Filter::default() {
            self.ended = false;
            self.aside = Some(std::mem::take(&mut self.current));
        } else if let Some(mut f) = self.aside.take() {
            let had_criteria = f != Filter::default();
            source.reconcile(&mut f);
            // a source that owns the last criterion of the one brought back ends it: what is set next is not an edit of
            // it (counted then, see `set`, so that undo and redo leave it alone, but not before: the person has no
            // filter, and what a step dropped can still be put aside for them)
            self.ended = had_criteria && f == Filter::default();
            self.current = f;
        }
        self.aside.is_none()
    }

    /// An entity the filter names changed (a keyword or person renamed, a folder moved or removed): `edit` is
    /// applied to the filter and to the one put aside, and to nothing else. It must not set a field a source
    /// owns.
    pub fn follow(&mut self, mut edit: impl FnMut(&mut Filter)) {
        self.touch();
        edit(&mut self.current);
        if let Some(f) = self.aside.as_mut() {
            edit(f);
        }
    }

    /// A put-aside filter that [`FilterState::follow`] left empty has nothing to bring back.
    pub fn drop_aside_if_empty(&mut self) {
        self.touch();
        if self.aside.as_ref() == Some(&Filter::default()) {
            self.aside = None;
        }
    }

    /// Carry a change onto the person's filter: `edit` gets it, and whether it is put aside. A filter in use is
    /// then cleared of what the source owns (one put aside is, when it comes back). `edit` returns whether the
    /// filter goes aside again (a filter in use) or is dropped (one put aside): that is the step's own undo.
    pub(crate) fn carry(&mut self, source: LibrarySource, edit: impl FnOnce(&mut Filter, bool) -> Carried) {
        self.touch();
        let was_empty = self.current == Filter::default();
        let (aside, filter) = match self.aside.as_mut() {
            Some(f) => (true, f),
            None => (false, &mut self.current),
        };
        let verdict = edit(filter, aside);
        match verdict {
            Carried::InPlace => {}
            Carried::Drop if aside => self.aside = None,
            // put aside as the step had it: what the source shown owns is left out when it comes back (`toggle`), not
            // now, or a source that changes back first would find it gone (a filter that is empty has nothing to put
            // aside, unless it was the step's)
            Carried::PutAside { even_if_empty } if !aside && was_empty && (self.current != Filter::default() || even_if_empty) => {
                self.aside = Some(std::mem::take(&mut self.current));
            }
            Carried::Drop | Carried::PutAside { .. } => {}
        }
        if !aside {
            source.reconcile(&mut self.current);
        }
        // the filter that ended is still the person's only if nothing came back into it
        self.ended = self.ended && self.aside.is_none() && self.current == Filter::default();
    }

    /// Enable Filters is off (a filter is put aside).
    pub fn is_off(&self) -> bool {
        self.aside.is_some()
    }
}
