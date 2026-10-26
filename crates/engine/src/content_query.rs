//! A search by photo contents in the library view: what is asked (a Contents-mode text, `contents:` / `shows:` words
//! in the search text, or Find Similar Contents), the ranking it gives ([`lightcraft_catalog::Filter::ranked`], which
//! the catalog ANDs with every other filter) and what the view says about it (the chip, More / Fewer, hints).
//!
//! The ranking is worked out by the engine because the catalog cannot see the content index: the text is embedded by
//! the model's text tower on a thread of its own ([`crate::content_worker::TextWorker`]), every indexed photo is scored
//! against it, and the photos at or above the model's cut-off (else the best few hundred, "best matches") go to the
//! catalog as ids with scores. [`Session::visible`] asks for it before each query, so a ranking follows the index as it
//! grows, the model in use, and the photos deleted since.

use lightcraft_catalog::{PhotoId, Ranked, Sort, SortKey};
use serde_json::{Value, json};

use crate::Session;

/// The longest content query (longer ones are cut: the model reads 64 tokens at most).
pub const MAX_QUERY_CHARS: usize = 500;
/// How far More and Fewer go from the model's cut-off (each step halves or doubles it).
pub const MAX_STEP: i32 = 8;
pub const MIN_STEP: i32 = -5;

/// What is asked.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentAsk {
    /// Photos showing this text.
    Text(String),
    /// Photos whose contents are like this photo's.
    Similar(PhotoId),
}

/// Where the search is.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentState {
    /// The search model is loading, or the text is being read: the ranking follows.
    Preparing,
    Ready,
    /// It cannot run (no model, the photo is not indexed, the model failed…): nothing is shown, and this says why.
    Problem(String),
}

/// A note beside the chip.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentHint {
    /// Indexing is off or no model is installed: Settings ▸ Search.
    TurnOnIndexing,
    /// Indexing is under way: the answers come from this share of the library.
    Partial { percent: u32 },
}

/// What the view shows about a search by contents (the chip and its hints).
#[derive(Clone, Debug, PartialEq)]
pub struct ContentView {
    pub ask: ContentAsk,
    /// "Contents: red car", "Similar to IMG_0001.JPG".
    pub label: String,
    pub state: ContentState,
    /// Photos in the ranking (before the other filters).
    pub matches: usize,
    /// Nothing reached the cut-off: these are the best matches there are.
    pub best_matches: bool,
    /// More (+) / Fewer (−) steps from the default cut-off.
    pub step: i32,
    pub can_more: bool,
    pub can_fewer: bool,
    /// The cut-off in use (a text search that has photos passing it).
    pub min: Option<f32>,
    pub indexed: usize,
    pub total: usize,
    pub hint: Option<ContentHint>,
}

impl ContentView {
    pub fn to_json(&self) -> Value {
        let (kind, text, id) = match &self.ask {
            ContentAsk::Text(t) => ("text", Some(t.clone()), None),
            ContentAsk::Similar(p) => ("similar", None, Some(p.0)),
        };
        let (state, problem) = match &self.state {
            ContentState::Preparing => ("preparing", None),
            ContentState::Ready => ("ready", None),
            ContentState::Problem(p) => ("problem", Some(p.clone())),
        };
        let hint = match &self.hint {
            Some(ContentHint::TurnOnIndexing) => json!({"kind": "turnOnIndexing"}),
            Some(ContentHint::Partial { percent }) => json!({"kind": "partial", "percent": percent}),
            None => Value::Null,
        };
        json!({
            "kind": kind, "text": text, "id": id, "label": self.label, "state": state, "problem": problem,
            "matches": self.matches, "bestMatches": self.best_matches, "step": self.step, "canMore": self.can_more,
            "canFewer": self.can_fewer, "min": self.min.map(|m| (f64::from(m) * 1e5).round() / 1e5),
            "indexed": self.indexed, "total": self.total, "hint": hint,
        })
    }
}

/// The search by contents of the library view (`Session::content_query`).
#[derive(Default)]
pub struct ContentQuery {
    /// The Contents-mode text (`library.filter {contents}`).
    pub contents: Option<String>,
    /// Find Similar Contents (`content.similar {show: true}`).
    pub similar: Option<PhotoId>,
    /// More / Fewer steps.
    pub step: i32,
    /// The ask at the last refresh, and the sort from before Relevance took over.
    pub(crate) last: Option<ContentAsk>,
    pub(crate) saved_sort: Option<Sort>,
    pub(crate) ranked: Option<Ranked>,
    pub(crate) scores: std::collections::HashMap<PhotoId, f32>,
    /// What the ranking was made from (model, ask, step, index version).
    pub(crate) key: Option<String>,
    pub(crate) view: Option<ContentView>,
    pub(crate) generation: u64,
}

impl ContentQuery {
    /// Drop the ranking (the view shows nothing for it until it is worked out again).
    pub(crate) fn set_ranked(&mut self, ranked: Option<Ranked>, key: Option<String>) {
        self.scores = ranked.as_ref().map(Ranked::scores).unwrap_or_default();
        self.ranked = ranked;
        self.key = key;
        self.generation = self.generation.wrapping_add(1);
    }
}

/// A content query as typed: trimmed, at most [`MAX_QUERY_CHARS`] characters, `None` when blank.
pub fn clean_query(text: &str) -> Option<String> {
    let t: String = text.trim().chars().take(MAX_QUERY_CHARS).collect();
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

impl Session {
    /// What the view's search by contents asks, if anything: Find Similar Contents, else the Contents-mode text, else
    /// the `contents:` / `shows:` words of the search text.
    pub fn content_ask(&self) -> Option<ContentAsk> {
        if let Some(id) = self.content_query.similar {
            return Some(ContentAsk::Similar(id));
        }
        if let Some(t) = self.content_query.contents.as_deref().and_then(clean_query) {
            return Some(ContentAsk::Text(t));
        }
        let (asked, _) = lightcraft_catalog::query::split_contents(&self.filter.text);
        asked.as_deref().and_then(clean_query).map(ContentAsk::Text)
    }

    /// The search by contents the view shows, worked out as far as it can be now (`None`: there is none).
    pub fn content_view(&mut self) -> Option<ContentView> {
        self.content_refresh();
        self.content_query.view.clone()
    }

    /// Whether searching by contents works here and now (for the search bar's Contents mode before anything is asked):
    /// [`ContentHint::TurnOnIndexing`] without a model or with indexing off and the library not all indexed,
    /// [`ContentHint::Partial`] while indexing; `None` when the index is complete (or this build has no content search).
    pub fn content_hint(&mut self) -> Option<ContentHint> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::cmd::content_scan::hint(self)
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    }

    /// How well photo `id` matched the search by contents in view (`None`: no such search, or not in its ranking).
    pub fn content_score(&self, id: PhotoId) -> Option<f32> {
        self.content_query.scores.get(&id).copied()
    }

    /// The best score of the search by contents in view (a match bar is drawn against it).
    pub fn content_top_score(&self) -> Option<f32> {
        self.content_query.ranked.as_ref().and_then(|r| r.ids.iter().map(|(_, s)| *s).filter(|s| s.is_finite()).reduce(f32::max))
    }

    /// Stop the search by contents: the Contents text, Find Similar Contents and the `contents:` words of the search
    /// text. The sort goes back to what it was before Relevance.
    pub fn clear_content_query(&mut self) {
        self.content_query.contents = None;
        self.content_query.similar = None;
        let (asked, rest) = lightcraft_catalog::query::split_contents(&self.filter.text);
        if asked.is_some() {
            self.filter.text = rest;
        }
        self.content_refresh();
    }

    /// End the search by contents without ranking anything (another library is opening): its words leave the search
    /// text, and Relevance gives way to the sort it replaced.
    pub(crate) fn end_content_query_for_new_library(&mut self) {
        if self.sort.key == SortKey::Relevance {
            self.sort = self.content_query.saved_sort.filter(|s| s.key != SortKey::Relevance).unwrap_or_default();
        }
        self.content_query = ContentQuery::default();
        let (asked, rest) = lightcraft_catalog::query::split_contents(&self.filter.text);
        if asked.is_some() {
            self.filter.text = rest;
        }
    }

    /// Bring the ranking up to date with what is asked (cheap when nothing changed). Returns its generation, which
    /// changes whenever the ranking does.
    pub(crate) fn content_refresh(&mut self) -> u64 {
        let ask = self.content_ask();
        let q = &mut self.content_query;
        if ask != q.last {
            // a new question starts at the model's own cut-off
            q.step = 0;
            match (&q.last, &ask) {
                (None, Some(_)) => {
                    // a search by contents is best read best-first
                    q.saved_sort = Some(self.sort);
                    self.sort = Sort { key: SortKey::Relevance, ascending: false, ..self.sort };
                }
                (Some(_), None) => {
                    if self.sort.key == SortKey::Relevance {
                        self.sort = q.saved_sort.filter(|s| s.key != SortKey::Relevance).unwrap_or_default();
                    }
                    q.saved_sort = None;
                }
                _ => {}
            }
            q.last = ask.clone();
        }
        let Some(ask) = ask else {
            if self.content_query.ranked.is_some() || self.content_query.view.is_some() {
                self.content_query.view = None;
                self.content_query.set_ranked(None, None);
            }
            return self.content_query.generation;
        };
        self.rank_contents(ask);
        self.content_query.generation
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn rank_contents(&mut self, ask: ContentAsk) {
        crate::cmd::content_scan::rank(self, ask);
    }

    #[cfg(target_arch = "wasm32")]
    fn rank_contents(&mut self, ask: ContentAsk) {
        let label = self.content_label(&ask);
        let q = &mut self.content_query;
        if q.key.as_deref() != Some("web") {
            q.set_ranked(Some(Ranked::default()), Some("web".into()));
        }
        q.view = Some(ContentView {
            ask,
            label,
            state: ContentState::Problem("Search by contents is not available in the web version".into()),
            matches: 0,
            best_matches: false,
            step: 0,
            can_more: false,
            can_fewer: false,
            min: None,
            indexed: 0,
            total: 0,
            hint: None,
        });
    }

    /// The chip's text for `ask`.
    pub(crate) fn content_label(&self, ask: &ContentAsk) -> String {
        match ask {
            ContentAsk::Text(t) => format!("Contents: {t}"),
            ContentAsk::Similar(id) => {
                let name = self.catalog.photo(*id).map(|p| p.file_name.clone()).unwrap_or_else(|| format!("photo {}", id.0));
                format!("Similar to {name}")
            }
        }
    }

    /// Wait (at most `timeout`) until the search by contents is no longer being prepared: for commands that answer at
    /// once (the CLI, MCP, `library.filter {wait: true}`).
    pub(crate) fn content_wait(&mut self, timeout: std::time::Duration) {
        let started = std::time::Instant::now();
        while self.content_view().is_some_and(|v| v.state == ContentState::Preparing) && started.elapsed() < timeout {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
