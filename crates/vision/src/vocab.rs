//! The vocabulary of auto-tags: the things a photo can be tagged with, each a path (`animal|bird|owl`) and the
//! phrases that describe it (its synonyms: one tag, their embeddings averaged).
//!
//! The built-in English list ([`DEFAULT_EN`], `vocab/en.txt`) is original work written for LightCraft, not derived from
//! any published label set. A user's own list has the same form ([`parse`]):
//!
//! ```text
//! # a comment
//! animal|bird|owl                  described by "owl" (the last level)
//! animal|dog = dog, puppy          described by "dog" and "puppy"
//! scene|beach = beach @0.03        its own cut-off instead of the model's
//! ```
//!
//! Levels above a tag need no line of their own: a photo tagged `animal|bird|owl` counts under `animal|bird` and
//! `animal` too ([`ancestors`]). Everything read is bounded ([`MAX_TERMS`], [`MAX_PATH_CHARS`], [`MAX_LEVELS`],
//! [`MAX_PROMPTS`], [`MAX_PROMPT_CHARS`]); a line that breaks a bound, repeats a tag or can't be read is skipped with a
//! note, never an error.

/// The built-in English vocabulary.
pub const DEFAULT_EN: &str = include_str!("../vocab/en.txt");

/// Most tags in a vocabulary.
pub const MAX_TERMS: usize = 5000;
/// Longest tag path, in characters.
pub const MAX_PATH_CHARS: usize = 120;
/// Most levels in a tag path.
pub const MAX_LEVELS: usize = 6;
/// Most phrases describing one tag.
pub const MAX_PROMPTS: usize = 8;
/// Longest phrase, in characters.
pub const MAX_PROMPT_CHARS: usize = 100;
/// Largest vocabulary text read, in bytes.
pub const MAX_BYTES: usize = 1 << 20;
/// Most notes kept about skipped lines.
const MAX_NOTES: usize = 20;

/// The sentences each phrase is put in before it is embedded; a tag's embedding is the mean of all of them (over all
/// its phrases). Chosen on a labelled set (see `docs/content-search.md`).
pub const TEMPLATES: &[&str] = &["this is a photo of {}."];

/// One tag.
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    /// `animal|bird|owl`: levels joined by `|`, trimmed, one space between words.
    pub path: String,
    /// The phrases describing it (at least one: the last level when the line names none).
    pub prompts: Vec<String>,
    /// Its own cut-off (a probability), instead of the model's.
    pub threshold: Option<f32>,
}

impl Term {
    /// The last level: what the tag is called (`owl`).
    pub fn label(&self) -> &str {
        label_of(&self.path)
    }

    /// Its line in a vocabulary text.
    pub fn line(&self) -> String {
        let mut s = self.path.clone();
        if self.prompts.len() != 1 || self.prompts.first().map(String::as_str) != Some(self.label()) {
            s.push_str(" = ");
            s.push_str(&self.prompts.join(", "));
        }
        if let Some(t) = self.threshold {
            s.push_str(&format!(" @{t}"));
        }
        s
    }

    /// What identifies its embedding: its phrases (with the templates; a changed phrase is embedded again).
    pub fn key(&self) -> String {
        self.prompts.join("\n")
    }

    /// The sentences that are embedded for it ([`TEMPLATES`] × its phrases).
    pub fn sentences(&self) -> Vec<String> {
        self.prompts.iter().flat_map(|p| TEMPLATES.iter().map(move |t| t.replace("{}", p))).collect()
    }
}

/// A vocabulary: tags in the order written, and what was skipped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vocabulary {
    pub terms: Vec<Term>,
    /// Lines that were skipped and why ("line 12: a tag can have at most 6 levels"), at most a few.
    pub notes: Vec<String>,
}

/// The last level of a path.
pub fn label_of(path: &str) -> &str {
    path.rsplit('|').next().unwrap_or(path)
}

/// The levels above `path`, nearest first (`animal|bird|owl` → `animal|bird`, `animal`).
pub fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.char_indices().rev().filter(|(_, c)| *c == '|').filter_map(move |(i, _)| path.get(..i))
}

/// Whether `path` is `under` or a level below it (case-insensitive): `animal|bird|owl` is under `animal`.
pub fn is_under(path: &str, under: &str) -> bool {
    let (p, u) = (path.to_lowercase(), under.to_lowercase());
    p == u || p.strip_prefix(&u).is_some_and(|rest| rest.starts_with('|'))
}

/// One space between words, no space at the ends.
fn tidy(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A tag path as written, tidied: levels trimmed (empty ones dropped), `None` when nothing is left.
pub fn clean_path(s: &str) -> Option<String> {
    let levels: Vec<String> = s.split('|').map(tidy).filter(|l| !l.is_empty()).collect();
    (!levels.is_empty()).then(|| levels.join("|"))
}

/// Read a vocabulary text (see the module). Never fails: what can't be read is skipped and noted.
pub fn parse(text: &str) -> Vocabulary {
    let mut v = Vocabulary::default();
    let mut seen = std::collections::HashSet::new();
    let note = |notes: &mut Vec<String>, n: usize, why: &str| {
        if notes.len() < MAX_NOTES {
            notes.push(format!("line {n}: {why}"));
        }
    };
    // (a text larger than the cap is read up to it, at a character boundary)
    let mut end = text.len().min(MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = text.get(..end).unwrap_or("");
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if v.terms.len() >= MAX_TERMS {
            note(&mut v.notes, n, &format!("a vocabulary has at most {MAX_TERMS} tags; the rest are left out"));
            break;
        }
        // `… @0.05`: the tag's own cut-off
        let (line, threshold) = match line.rsplit_once('@') {
            Some((rest, t)) => match t.trim().parse::<f32>() {
                Ok(t) if t.is_finite() && t > 0.0 && t < 1.0 => (rest.trim(), Some(t)),
                _ => {
                    note(&mut v.notes, n, "the cut-off after @ is a number between 0 and 1");
                    (rest.trim(), None)
                }
            },
            None => (line, None),
        };
        let (path, prompts) = match line.split_once('=') {
            Some((p, rest)) => (p, Some(rest)),
            None => (line, None),
        };
        let Some(path) = clean_path(path) else {
            note(&mut v.notes, n, "no tag name");
            continue;
        };
        if path.chars().count() > MAX_PATH_CHARS {
            note(&mut v.notes, n, &format!("a tag is at most {MAX_PATH_CHARS} characters"));
            continue;
        }
        if path.split('|').count() > MAX_LEVELS {
            note(&mut v.notes, n, &format!("a tag can have at most {MAX_LEVELS} levels"));
            continue;
        }
        if !seen.insert(path.to_lowercase()) {
            note(&mut v.notes, n, &format!("`{path}` is already in the list"));
            continue;
        }
        let mut list: Vec<String> = Vec::new();
        for p in prompts.unwrap_or("").split(',').map(tidy).filter(|p| !p.is_empty()) {
            if p.chars().count() > MAX_PROMPT_CHARS {
                note(&mut v.notes, n, &format!("a phrase is at most {MAX_PROMPT_CHARS} characters"));
                continue;
            }
            if list.len() >= MAX_PROMPTS {
                note(&mut v.notes, n, &format!("a tag has at most {MAX_PROMPTS} phrases"));
                break;
            }
            if !list.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
                list.push(p);
            }
        }
        if list.is_empty() {
            list.push(label_of(&path).to_string());
        }
        v.terms.push(Term { path, prompts: list, threshold });
    }
    v
}

impl Vocabulary {
    /// The built-in English vocabulary.
    pub fn default_en() -> Vocabulary {
        parse(DEFAULT_EN)
    }

    /// The vocabulary as text ([`parse`] reads it back the same).
    pub fn to_text(&self) -> String {
        let mut s: String = self.terms.iter().map(|t| t.line() + "\n").collect();
        if s.is_empty() {
            s.push('\n');
        }
        s
    }

    /// A digest of everything that decides the tags' embeddings and cut-offs (paths, phrases, cut-offs, templates):
    /// two vocabularies with the same hash tag photos the same.
    pub fn hash(&self) -> u64 {
        let mut h = Fnv::new();
        for t in TEMPLATES {
            h.write(t.as_bytes());
            h.write(&[0]);
        }
        for t in &self.terms {
            h.write(t.line().as_bytes());
            h.write(&[0]);
        }
        h.finish()
    }

    /// The tag `name` names: its path, its last level, or one of its phrases (case-insensitive; the first that fits).
    pub fn find(&self, name: &str) -> Option<usize> {
        let name = clean_path(name)?.to_lowercase();
        let by = |f: &dyn Fn(&Term) -> bool| self.terms.iter().position(f);
        by(&|t| t.path.to_lowercase() == name)
            .or_else(|| by(&|t| t.label().to_lowercase() == name))
            .or_else(|| by(&|t| t.prompts.iter().any(|p| p.to_lowercase() == name)))
    }
}

/// FNV-1a, 64 bits: a stable digest (the same on every machine and version).
pub struct Fnv(u64);

impl Fnv {
    pub fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for Fnv {
    fn default() -> Self {
        Fnv::new()
    }
}

/// A stable digest of a tag path, case-insensitive (what a dismissed suggestion is remembered by).
pub fn path_hash(path: &str) -> u64 {
    let mut h = Fnv::new();
    h.write(path.to_lowercase().as_bytes());
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_list_is_large_hierarchical_and_has_one_tag_per_name() {
        let v = Vocabulary::default_en();
        assert!(v.notes.is_empty(), "{:?}", v.notes);
        assert!((1000..=MAX_TERMS).contains(&v.terms.len()), "{}", v.terms.len());
        for want in
            ["scene|beach", "nature|forest", "nature|rocks", "plant|flower", "people|person", "people|crowd", "animal|bird", "animal|mammal|dog"]
        {
            assert!(v.find(want).is_some(), "{want}");
        }
        assert!(v.find("animal|arachnid|scorpion").is_some());
        assert!(v.find("vehicle|car").is_some() && v.find("event|wedding").is_some() && v.find("time|sunset").is_some());
        assert!(v.find("weather|snow").is_some());
        // every tag's own name is unique: accepted tags are keywords named after it
        let mut labels: Vec<String> = v.terms.iter().map(|t| t.label().to_lowercase()).collect();
        labels.sort();
        let n = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), n, "two tags share a name");
        // synonyms find their tag
        assert_eq!(v.terms[v.find("puppy").unwrap()].path, "animal|mammal|dog");
        assert_eq!(v.terms[v.find("Dog").unwrap()].path, "animal|mammal|dog");
    }

    #[test]
    fn lines_are_read_with_their_phrases_cut_offs_and_bounds() {
        let long = "x".repeat(MAX_PATH_CHARS + 1);
        let text = format!(
            "# comment\n\n a | b |  c d \nanimal|dog = dog,  puppy , Dog # trailing\nscene|beach = beach @0.03\n{long}\n1|2|3|4|5|6|7\nanimal|DOG\n = x\nbad @2\n"
        );
        let v = parse(&text);
        let paths: Vec<&str> = v.terms.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(paths, ["a|b|c d", "animal|dog", "scene|beach", "bad"]);
        assert_eq!(v.terms[0].prompts, ["c d"]);
        assert_eq!(v.terms[1].prompts, ["dog", "puppy"]);
        assert_eq!(v.terms[2].threshold, Some(0.03));
        assert_eq!(v.terms[3].threshold, None);
        assert_eq!(v.notes.len(), 5, "{:?}", v.notes);
        // round trip
        assert_eq!(parse(&v.to_text()).terms, v.terms);
        assert_eq!(parse(&v.to_text()).hash(), v.hash());
        assert_ne!(parse("a\nb").hash(), parse("a\nc").hash());
        assert_eq!(v.terms[1].sentences().len(), 2 * TEMPLATES.len());
    }

    #[test]
    fn hostile_vocabularies_are_bounded() {
        assert!(parse("").terms.is_empty());
        assert!(parse("\n\n|||\n=\n@\n").terms.is_empty());
        let many: String = (0..MAX_TERMS + 50).map(|i| format!("t{i}\n")).collect();
        let v = parse(&many);
        assert_eq!(v.terms.len(), MAX_TERMS);
        let prompts: Vec<String> = (0..50).map(|i| format!("p{i}")).collect();
        let v = parse(&format!("x = {}", prompts.join(",")));
        assert_eq!(v.terms[0].prompts.len(), MAX_PROMPTS);
        // multi-byte text cut at the byte cap stays whole characters
        let big = "é".repeat(MAX_BYTES);
        let v = parse(&big);
        assert!(v.terms.is_empty() || v.terms[0].path.chars().all(|c| c == 'é'));
    }

    #[test]
    fn levels_and_digests() {
        assert_eq!(ancestors("animal|bird|owl").collect::<Vec<_>>(), ["animal|bird", "animal"]);
        assert_eq!(ancestors("owl").count(), 0);
        assert!(is_under("animal|bird|owl", "Animal") && is_under("animal", "animal") && !is_under("animals", "animal"));
        assert_eq!(path_hash("A|b"), path_hash("a|B"));
        assert_eq!(label_of("a|b c"), "b c");
    }
}
