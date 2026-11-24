//! What the named faces suggest for every unnamed face in the library (issue #1365).
//!
//! `faces.unnamed` lists a page of unnamed faces, and the look-alike order it shows them in is a display choice. A
//! suggestion must not depend on it: a face that looks like a confirmed person is suggested as them whether it comes
//! first on the page, six-hundredth, or after the faces the view stops ordering. So the judging is kept here,
//! separately from the listing that draws it: every unnamed face with an embedding is scored against the named faces
//! once, a chunk at a time (the work is resumable, so a large library does not stall a frame), and the page and the
//! Suggested category read the answer from it.
//!
//! What an answer depends on is the model and the bars a suggestion passes (they are one number), and the named faces
//! it is made from (another). A different model drops everything and starts over; different named faces have every
//! face judged again, over the answer the last ones gave, since that answer is still the best one there is until a face
//! has its new one, and a view that emptied on every name would be no use on a large library. The cache file keeps
//! embeddings, not suggestions: these are rebuilt in a session, and never written to a photo.

use std::collections::{HashMap, HashSet};

/// One face's key: its photo and the index of its region in that photo.
pub(crate) type Key = (u64, usize);

/// The library-wide judging, as it stands.
#[derive(Default)]
pub(crate) struct Scan {
    /// The model and the bars the suggestions were judged with, and which library it is (made by
    /// [`crate::cmd::face_recognize`]): a different one starts over, since nothing judged still answers.
    pub model: u64,
    /// The named faces the suggestions are made from. A different one has every face judged again, over the answer the
    /// last one gave rather than from nothing, so naming a face does not empty the Suggested faces while the library is
    /// looked at again.
    pub gallery: u64,
    /// Every face judged with these.
    pub judged: HashSet<Key>,
    /// The faces that look like a named person enough to be suggested as them.
    pub suggestions: HashMap<Key, (String, f32)>,
    /// Every judged face's closest named person, even below the suggestion bars (what the hover captions show).
    pub similarities: HashMap<Key, (String, f32)>,
    /// Whether every unnamed face with an embedding was judged when the scan last looked.
    pub complete: bool,
    /// How many faces the index held when the scan last found nothing left to do: the same count means nothing was
    /// embedded since, so the walk over the library's faces can be skipped.
    pub indexed: usize,
}

impl Scan {
    /// Start over: the model, the bars or the library changed, so what was judged says nothing about these faces.
    pub fn reset(&mut self, model: u64, gallery: u64) {
        self.model = model;
        self.gallery = gallery;
        self.judged.clear();
        self.suggestions.clear();
        self.similarities.clear();
        self.complete = false;
        self.indexed = 0;
    }

    /// The named faces changed: every face is judged again, keeping what the last answer said until each face has its
    /// new one. A suggestion naming someone renamed or removed since is dropped when the answer is read, so it does not
    /// wait for the judging to reach the face that carried it.
    pub fn named_again(&mut self, gallery: u64) {
        self.gallery = gallery;
        self.judged.clear();
        self.complete = false;
        self.indexed = 0;
    }

    /// A face's embedding was dropped (its file was replaced): judge it again, and do not trust the walk's shortcut
    /// until every face is judged again.
    pub fn forget_photo(&mut self, photo: u64) {
        let gone = |k: &Key| k.0 == photo;
        self.judged.retain(|k| !gone(k));
        self.suggestions.retain(|k, _| !gone(k));
        self.similarities.retain(|k, _| !gone(k));
        self.complete = false;
        self.indexed = usize::MAX;
    }

    /// Keep what one face is: its closest named person, and the person suggested for it, if any.
    pub fn judge(&mut self, key: Key, similarity: Option<(String, f32)>, suggestion: Option<(String, f32)>) {
        self.judged.insert(key);
        match similarity {
            Some(v) => {
                self.similarities.insert(key, v);
            }
            None => {
                self.similarities.remove(&key);
            }
        }
        match suggestion {
            Some(v) => {
                self.suggestions.insert(key, v);
            }
            None => {
                self.suggestions.remove(&key);
            }
        }
    }
}
