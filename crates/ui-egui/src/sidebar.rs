//! The left sidebar's sections and how the user arranged them: their order, which are hidden and
//! which are folded shut. No drawing here (that is `panels/left.rs`); this is the arrangement
//! itself, saved with the UI state and changed by the `view.sidebar*` commands.
//!
//! My Photos (All Photos, Recently Added, Picks, Missing Photos, Recently Deleted) is not a
//! section: it stays at the top and can't be hidden, moved or folded.

use serde::{Deserialize, Serialize};

/// A section of the left sidebar below My Photos.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SidebarSection {
    Albums,
    Local,
    ByDate,
    Folders,
    Keywords,
}

impl SidebarSection {
    /// Every section, in the order a new library shows them.
    pub const ALL: [SidebarSection; 5] =
        [SidebarSection::Albums, SidebarSection::Local, SidebarSection::ByDate, SidebarSection::Folders, SidebarSection::Keywords];

    /// The name commands, the saved UI state and widget ids (`sidebarSection:<id>`) use.
    pub const fn id(self) -> &'static str {
        match self {
            SidebarSection::Albums => "albums",
            SidebarSection::Local => "local",
            SidebarSection::ByDate => "byDate",
            SidebarSection::Folders => "folders",
            SidebarSection::Keywords => "keywords",
        }
    }

    /// The heading, in English (translated where it is shown).
    pub const fn title(self) -> &'static str {
        match self {
            SidebarSection::Albums => "Albums",
            SidebarSection::Local => "Local",
            SidebarSection::ByDate => "By Date",
            SidebarSection::Folders => "Folders",
            SidebarSection::Keywords => "Keywords",
        }
    }

    /// What the section says while it has nothing to list, in English.
    pub const fn empty_hint(self) -> &'static str {
        match self {
            SidebarSection::Albums => "No albums yet",
            SidebarSection::Local => "No folders to browse",
            SidebarSection::ByDate => "No photos yet",
            SidebarSection::Folders => "No imported folders yet",
            SidebarSection::Keywords => "No keywords yet",
        }
    }

    pub fn from_id(id: &str) -> Option<SidebarSection> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Whether this build has the section at all. Local browses folders on disk: not on the web,
    /// nor on iOS / Android, where the app sees only its own sandbox and what the user picks.
    /// Folders lists where on disk photos were imported from: on a phone or tablet those are the
    /// app's own storage and the pickers' temporary copies, nothing the user would browse by.
    pub const fn available(self) -> bool {
        self.available_on(cfg!(target_arch = "wasm32"), cfg!(any(target_os = "ios", target_os = "android")))
    }

    /// [`Self::available`] for a given platform (split out so tests on the desktop cover every one).
    const fn available_on(self, web: bool, mobile: bool) -> bool {
        match self {
            SidebarSection::Local => !web && !mobile,
            SidebarSection::Folders => !mobile,
            SidebarSection::Albums | SidebarSection::ByDate | SidebarSection::Keywords => true,
        }
    }

    /// The ids of every section, for error messages (`albums|local|…`).
    pub fn ids() -> String {
        Self::ALL.map(SidebarSection::id).join("|")
    }
}

/// Where one section stands: shown or hidden, open or folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SectionState {
    pub id: SidebarSection,
    pub hidden: bool,
    pub collapsed: bool,
}

impl SectionState {
    const fn new(id: SidebarSection) -> Self {
        Self { id, hidden: false, collapsed: false }
    }
}

/// The arrangement of the sidebar's sections, top to bottom. Every section is in it exactly once,
/// whatever was read from disk or sent by an agent: unknown and repeated entries are dropped and
/// sections not named come last, in their usual order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "serde_json::Value", into = "Vec<SectionState>")]
pub struct SidebarLayout(Vec<SectionState>);

impl Default for SidebarLayout {
    fn default() -> Self {
        Self(SidebarSection::ALL.map(SectionState::new).to_vec())
    }
}

impl From<SidebarLayout> for Vec<SectionState> {
    fn from(layout: SidebarLayout) -> Self {
        layout.0
    }
}

/// Lenient on purpose: a settings file edited by hand, written by a newer version, or damaged
/// must still open, so whatever can't be understood is left out instead of failing the load.
impl From<serde_json::Value> for SidebarLayout {
    fn from(saved: serde_json::Value) -> Self {
        let entries = saved.as_array().map(Vec::as_slice).unwrap_or_default();
        let flag = |entry: &serde_json::Value, key: &str| entry.get(key).and_then(serde_json::Value::as_bool).unwrap_or(false);
        let read = entries.iter().filter_map(|entry| {
            // an entry is `{id, hidden?, collapsed?}`, or just the id
            let id = entry.as_str().or_else(|| entry.get("id").and_then(serde_json::Value::as_str))?;
            Some(SectionState { id: SidebarSection::from_id(id)?, hidden: flag(entry, "hidden"), collapsed: flag(entry, "collapsed") })
        });
        Self::from_states(read)
    }
}

impl SidebarLayout {
    /// The arrangement an agent or script asks for (`ui.set` `sidebar`), read strictly, unlike a
    /// settings file: a list whose entries are a section id or `{id, hidden?, collapsed?}`, each
    /// section at most once. Sections left out follow in the usual order, shown and open. What is
    /// wrong is said, so a typo doesn't quietly rearrange the sidebar.
    pub fn parse(asked: &serde_json::Value) -> Result<Self, String> {
        let entries = asked.as_array().ok_or_else(|| format!("`sidebar` must be a list of sections ({}), not {asked}", SidebarSection::ids()))?;
        let mut states: Vec<SectionState> = Vec::new();
        for entry in entries {
            let name = entry
                .as_str()
                .or_else(|| entry.get("id").and_then(serde_json::Value::as_str))
                .ok_or_else(|| format!("`sidebar`: {entry} names no section (an id, or {{id, hidden, collapsed}})"))?;
            let id = SidebarSection::from_id(name).ok_or_else(|| format!("`sidebar`: unknown section `{name}` ({})", SidebarSection::ids()))?;
            if states.iter().any(|s| s.id == id) {
                return Err(format!("`sidebar`: `{name}` is listed twice"));
            }
            let flag = |key: &str| match entry.get(key) {
                None | Some(serde_json::Value::Null) => Ok(false),
                Some(serde_json::Value::Bool(b)) => Ok(*b),
                Some(other) => Err(format!("`sidebar`: `{key}` of `{name}` must be true or false, not {other}")),
            };
            states.push(SectionState { id, hidden: flag("hidden")?, collapsed: flag("collapsed")? });
        }
        Ok(Self::from_states(states))
    }

    /// The layout these states describe, made whole (see the type).
    fn from_states(states: impl IntoIterator<Item = SectionState>) -> Self {
        let mut all: Vec<SectionState> = Vec::with_capacity(SidebarSection::ALL.len());
        for state in states.into_iter().chain(SidebarSection::ALL.map(SectionState::new)) {
            if !all.iter().any(|s| s.id == state.id) {
                all.push(state);
            }
        }
        Self(all)
    }

    /// Every section top to bottom, hidden ones included.
    pub fn sections(&self) -> &[SectionState] {
        &self.0
    }

    /// The sections that are shown, top to bottom.
    pub fn shown(&self) -> impl Iterator<Item = SidebarSection> + '_ {
        self.0.iter().filter(|s| !s.hidden && s.id.available()).map(|s| s.id)
    }

    fn state(&self, section: SidebarSection) -> SectionState {
        self.0.iter().copied().find(|s| s.id == section).unwrap_or(SectionState::new(section))
    }

    fn state_mut(&mut self, section: SidebarSection) -> Option<&mut SectionState> {
        self.0.iter_mut().find(|s| s.id == section)
    }

    pub fn is_hidden(&self, section: SidebarSection) -> bool {
        self.state(section).hidden
    }

    pub fn is_collapsed(&self, section: SidebarSection) -> bool {
        self.state(section).collapsed
    }

    /// Show or hide a section. It keeps its place and whether it was folded.
    pub fn set_hidden(&mut self, section: SidebarSection, hidden: bool) {
        if let Some(s) = self.state_mut(section) {
            s.hidden = hidden;
        }
    }

    /// Fold a section shut or open it.
    pub fn set_collapsed(&mut self, section: SidebarSection, collapsed: bool) {
        if let Some(s) = self.state_mut(section) {
            s.collapsed = collapsed;
        }
    }

    pub fn toggle_collapsed(&mut self, section: SidebarSection) {
        self.set_collapsed(section, !self.is_collapsed(section));
    }

    /// Make sure a section can be seen: shown and open (something in it is being pointed out).
    pub fn reveal(&mut self, section: SidebarSection) {
        self.set_hidden(section, false);
        self.set_collapsed(section, false);
    }

    /// Put `section` just above `before`, or last when `before` is `None`. Returns whether it
    /// moved. Moving a section above itself or to where it already is changes nothing, and neither
    /// does a move of a shown section that would only take it past hidden ones: the sidebar would
    /// look the same, so the arrangement stays the same too.
    pub fn move_before(&mut self, section: SidebarSection, before: Option<SidebarSection>) -> bool {
        if before == Some(section) {
            return false;
        }
        let was = self.clone();
        let moved = self.state(section);
        self.0.retain(|s| s.id != section);
        let at = before.and_then(|b| self.0.iter().position(|s| s.id == b)).unwrap_or(self.0.len());
        self.0.insert(at, moved);
        let seen = !moved.hidden && section.available();
        if seen && self.shown().eq(was.shown()) {
            *self = was;
            return false;
        }
        *self != was
    }

    /// Move `section` one place up or down among the sections that are shown (hidden ones are
    /// stepped over, so every move is one the user can see). Returns whether it moved: the first
    /// can't go up, the last can't go down, and a hidden section stays where it is.
    pub fn move_by(&mut self, section: SidebarSection, up: bool) -> bool {
        let shown: Vec<SidebarSection> = self.shown().collect();
        let Some(at) = shown.iter().position(|s| *s == section) else { return false };
        if up {
            let Some(above) = at.checked_sub(1).and_then(|i| shown.get(i)) else { return false };
            self.move_before(section, Some(*above))
        } else {
            if at + 1 >= shown.len() {
                return false;
            }
            // below the next one: above the one after it, or last
            self.move_before(section, shown.get(at + 2).copied())
        }
    }

    /// Whether `section` can move up (or down) among the shown sections.
    pub fn can_move(&self, section: SidebarSection, up: bool) -> bool {
        let shown: Vec<SidebarSection> = self.shown().collect();
        match shown.iter().position(|s| *s == section) {
            Some(at) if up => at > 0,
            Some(at) => at + 1 < shown.len(),
            None => false,
        }
    }

    /// Whether this is the arrangement a new library starts with.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Fold shut the sections an older settings file listed as collapsed (`collapsedSidebar`).
    pub fn adopt_collapsed(&mut self, ids: &[String]) {
        for section in ids.iter().filter_map(|id| SidebarSection::from_id(id)) {
            self.set_collapsed(section, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SidebarSection::{Albums, ByDate, Folders, Keywords, Local};
    use super::*;
    use serde_json::json;

    fn order(layout: &SidebarLayout) -> Vec<SidebarSection> {
        layout.sections().iter().map(|s| s.id).collect()
    }

    fn read(saved: serde_json::Value) -> SidebarLayout {
        serde_json::from_value(saved).unwrap()
    }

    /// Given a new library, the sidebar lists every section, shown and open, in the usual order.
    #[test]
    fn a_new_sidebar_shows_every_section_open_in_the_usual_order() {
        let layout = SidebarLayout::default();
        assert_eq!(order(&layout), [Albums, Local, ByDate, Folders, Keywords]);
        assert!(layout.sections().iter().all(|s| !s.hidden && !s.collapsed));
        assert!(layout.is_default());
    }

    /// Given an arrangement the user made, when it is saved and read back, then it is the same.
    #[test]
    fn an_arrangement_survives_saving() {
        let mut layout = SidebarLayout::default();
        layout.move_before(Keywords, Some(Albums));
        layout.set_hidden(Local, true);
        layout.set_collapsed(ByDate, true);
        let saved = serde_json::to_value(&layout).unwrap();
        assert_eq!(saved[0], json!({"id": "keywords", "hidden": false, "collapsed": false}));
        assert_eq!(read(saved), layout);
    }

    /// Given a saved arrangement that is incomplete, repeats itself or names sections this version
    /// doesn't know, when it is read, then every section is there exactly once: the readable
    /// entries first, in their order, and the rest after them.
    #[test]
    fn a_damaged_arrangement_is_made_whole() {
        let layout = read(json!([
            {"id": "keywords", "collapsed": true},
            {"id": "map"},
            "folders",
            {"id": "keywords", "hidden": true},
            {"id": 7},
            null,
            {"id": "albums", "hidden": "yes", "collapsed": 1}
        ]));
        assert_eq!(order(&layout), [Keywords, Folders, Albums, Local, ByDate]);
        assert!(layout.is_collapsed(Keywords) && !layout.is_hidden(Keywords), "the first entry for a section counts");
        assert!(!layout.is_hidden(Albums) && !layout.is_collapsed(Albums), "a flag that isn't true or false is off");
    }

    /// Given a saved arrangement that is not a list at all, when it is read, then the sidebar is
    /// the usual one (and the rest of the settings still load).
    #[test]
    fn an_unreadable_arrangement_is_the_usual_one() {
        for saved in [json!(null), json!("albums"), json!(3), json!({"albums": true}), json!([])] {
            assert!(read(saved.clone()).is_default(), "{saved}");
        }
    }

    /// Hiding a section takes it off the sidebar and nothing else: it keeps its place and stays
    /// folded or open, so showing it again puts it back as it was.
    #[test]
    fn a_hidden_section_comes_back_where_and_how_it_was() {
        let mut layout = SidebarLayout::default();
        layout.set_collapsed(ByDate, true);
        layout.set_hidden(ByDate, true);
        assert_eq!(layout.shown().collect::<Vec<_>>(), [Albums, Local, Folders, Keywords]);
        assert!(!layout.is_default());
        layout.set_hidden(ByDate, false);
        assert_eq!(layout.shown().collect::<Vec<_>>(), [Albums, Local, ByDate, Folders, Keywords]);
        assert!(layout.is_collapsed(ByDate));
    }

    /// Revealing a section shows it and opens it.
    #[test]
    fn revealing_a_section_shows_and_opens_it() {
        let mut layout = SidebarLayout::default();
        layout.set_collapsed(Albums, true);
        layout.set_hidden(Albums, true);
        layout.reveal(Albums);
        assert!(!layout.is_hidden(Albums) && !layout.is_collapsed(Albums));
    }

    /// A section can be put above any other, or last; the others keep their order.
    #[test]
    fn a_section_moves_above_another_or_to_the_end() {
        let mut layout = SidebarLayout::default();
        assert!(layout.move_before(Keywords, Some(Local)));
        assert_eq!(order(&layout), [Albums, Keywords, Local, ByDate, Folders]);
        assert!(layout.move_before(Albums, None));
        assert_eq!(order(&layout), [Keywords, Local, ByDate, Folders, Albums]);
        // nothing to do: above itself, where it already is
        assert!(!layout.move_before(Local, Some(Local)));
        assert!(!layout.move_before(Local, Some(ByDate)));
        assert!(!layout.move_before(Albums, None));
        assert_eq!(order(&layout), [Keywords, Local, ByDate, Folders, Albums]);
    }

    /// Given a hidden section between two shown ones, when the upper one is dropped right where
    /// it already appears (above the lower one), then nothing changes: no move that can't be seen.
    #[test]
    fn a_move_that_cannot_be_seen_is_no_move() {
        let mut layout = SidebarLayout::default();
        layout.set_hidden(Local, true);
        let was = layout.clone();
        assert!(!layout.move_before(Albums, Some(ByDate)), "Albums is already right above By Date, as far as anyone can see");
        assert_eq!(layout, was);
        // a hidden section can still be put elsewhere (it shows there when it is shown again)
        assert!(layout.move_before(Local, None));
        assert_eq!(order(&layout), [Albums, ByDate, Folders, Keywords, Local]);
    }

    /// Moving a section keeps whether it is hidden or folded.
    #[test]
    fn a_moved_section_stays_folded() {
        let mut layout = SidebarLayout::default();
        layout.set_collapsed(Folders, true);
        layout.move_before(Folders, Some(Albums));
        assert!(layout.is_collapsed(Folders));
    }

    /// Move Up and Move Down go one shown section at a time and stop at the ends.
    #[test]
    fn a_section_moves_one_place_among_the_shown_ones() {
        let mut layout = SidebarLayout::default();
        layout.set_hidden(ByDate, true);
        // down past the hidden By Date in one step
        assert!(layout.can_move(Local, false));
        assert!(layout.move_by(Local, false));
        assert_eq!(layout.shown().collect::<Vec<_>>(), [Albums, Folders, Local, Keywords]);
        assert!(layout.move_by(Local, false));
        assert_eq!(layout.shown().collect::<Vec<_>>(), [Albums, Folders, Keywords, Local]);
        assert!(!layout.can_move(Local, false) && !layout.move_by(Local, false), "the last can't go down");
        assert!(layout.move_by(Folders, true));
        assert_eq!(layout.shown().collect::<Vec<_>>(), [Folders, Albums, Keywords, Local]);
        assert!(!layout.can_move(Folders, true) && !layout.move_by(Folders, true), "the first can't go up");
        assert!(!layout.can_move(ByDate, true) && !layout.move_by(ByDate, true), "a hidden section doesn't move");
    }

    /// Sections an older settings file listed as collapsed are folded; names it got wrong are ignored.
    #[test]
    fn collapsed_sections_of_an_older_settings_file_stay_folded() {
        let mut layout = SidebarLayout::default();
        layout.adopt_collapsed(&["byDate".into(), "nonsense".into(), "keywords".into()]);
        assert!(layout.is_collapsed(ByDate) && layout.is_collapsed(Keywords) && !layout.is_collapsed(Albums));
    }

    /// Given an arrangement asked for by an agent, when it is right, then it is taken (sections it
    /// leaves out follow, shown and open); when anything in it is wrong, then it is refused with
    /// what is wrong, instead of being made whole as a settings file is.
    #[test]
    fn an_arrangement_that_is_asked_for_is_read_strictly() {
        let layout = SidebarLayout::parse(&json!(["keywords", {"id": "albums", "hidden": true, "collapsed": null}])).unwrap();
        assert_eq!(order(&layout), [Keywords, Albums, Local, ByDate, Folders]);
        assert!(layout.is_hidden(Albums) && !layout.is_collapsed(Albums));
        assert!(SidebarLayout::parse(&json!([])).unwrap().is_default());
        for (asked, says) in [
            (json!(null), "must be a list"),
            (json!({"albums": 1}), "must be a list"),
            (json!([{"id": "keywrods", "hidden": true}]), "unknown section `keywrods`"),
            (json!([7]), "names no section"),
            (json!(["albums", {"id": "albums"}]), "`albums` is listed twice"),
            (json!([{"id": "albums", "hidden": "yes"}]), "`hidden` of `albums` must be true or false"),
            (json!([{"id": "albums", "collapsed": 1}]), "`collapsed` of `albums` must be true or false"),
        ] {
            let e = SidebarLayout::parse(&asked).unwrap_err();
            assert!(e.contains(says), "{asked}: {e}");
        }
    }

    #[test]
    fn sections_are_named_by_their_ids() {
        for s in SidebarSection::ALL {
            assert_eq!(SidebarSection::from_id(s.id()), Some(s));
            assert_eq!(serde_json::to_value(s).unwrap(), json!(s.id()));
        }
        assert_eq!(SidebarSection::from_id("Albums"), None);
        assert_eq!(SidebarSection::ids(), "albums|local|byDate|folders|keywords");
    }

    /// The phone and tablet apps have no Local or Folders (no folders of their own to browse);
    /// the web has Folders but no Local; the desktop has everything.
    #[test]
    fn local_and_folders_are_desktop_sections() {
        let on = |web, mobile| SidebarSection::ALL.into_iter().filter(|s| s.available_on(web, mobile)).collect::<Vec<_>>();
        assert_eq!(on(false, false), SidebarSection::ALL.to_vec());
        assert_eq!(on(true, false), [Albums, ByDate, Folders, Keywords]);
        assert_eq!(on(false, true), [Albums, ByDate, Keywords]);
        assert!(SidebarSection::ALL.into_iter().all(SidebarSection::available), "this desktop build has every section");
    }
}
