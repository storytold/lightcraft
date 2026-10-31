//! Library-wide keyword operations: the keyword tree (hierarchical keywords are written
//! `parent|child|grandchild`), rename / delete / merge across every photo, and suggestions.
//!
//! Rename, delete and merge produce one [`Op::Batch`] of [`Op::SetMeta`]s — one per photo that
//! changes — so they are a single undo step and replay from the op log like any other edit.
//! Keyword names compare case-insensitively; renaming a keyword renames its children too
//! (`travel|italy` → `trips|italy`).
//!
//! The **keyword list** holds keywords on their own ([`Op::SetKeyword`]): created before any
//! photo has them, or given attributes ([`KeywordInfo`]: synonyms, export options, person). The
//! tree is the keyword list together with the keywords photos carry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Catalog, CatalogError, Meta, Op, Result};

/// Separator of hierarchical keyword levels.
pub const SEP: char = '|';

/// Normalize a keyword: trim every level, drop empty levels.
pub fn clean(k: &str) -> String {
    k.split(SEP).map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("|")
}

/// The same name, whatever the case of any of its letters (not only ASCII ones).
pub fn same(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// `k` is `parent` or one of its descendants (`parent|…`), ignoring case.
pub fn is_under(k: &str, parent: &str) -> bool {
    let (k, p) = (k.to_lowercase(), parent.to_lowercase());
    k == p || k.strip_prefix(&p).is_some_and(|rest| rest.starts_with(SEP))
}

/// Replace the `from` prefix of `k` (which [`is_under`] `from`) with `to`. By levels, not bytes:
/// `from` may be written in another case, which can change a letter's length (ẞ / ß).
pub fn reparent(k: &str, from: &str, to: &str) -> String {
    let skip = from.split(SEP).filter(|s| !s.trim().is_empty()).count();
    let rest: Vec<&str> = k.split(SEP).filter(|s| !s.trim().is_empty()).skip(skip).collect();
    std::iter::once(to).filter(|t| !t.is_empty()).chain(rest).collect::<Vec<_>>().join("|")
}

/// A file's keywords as a photo carries them: the paths of `lr:hierarchicalSubject`, then the
/// names of `dc:subject` that aren't levels of those paths (Lightroom Classic writes each keyword's
/// name, its parents' and synonyms flat besides its path). Each once, cleaned.
pub fn from_file(flat: &[String], hierarchical: &[String]) -> Vec<String> {
    let mut out: Vec<String> = hierarchical.iter().map(|k| clean(k)).collect();
    // One lowered set of every hierarchical level, not a search of `hierarchical` for each flat
    // keyword: disjoint lists from sidecars made the old approach quadratic in both (CWE-407), and
    // `same` allocates two lower-cased strings per comparison.
    let levels: std::collections::HashSet<String> = hierarchical.iter().flat_map(|h| h.split(SEP).map(|l| l.trim().to_lowercase())).collect();
    out.extend(flat.iter().map(|k| clean(k)).filter(|k| !levels.contains(&k.to_lowercase())));
    dedupe(&mut out);
    out
}

/// The deepest a keyword list file may nest keywords: a file is input, so its nesting is bounded.
pub const MAX_LEVELS: usize = 64;

/// A keyword list as written by [`Catalog::keyword_list_text`]: the file's text, the keywords in
/// it (paths), and what the format can't hold and so is left out (a keyword, with those inside it,
/// as its path; a synonym as `path {synonym}`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeywordListFile {
    pub text: String,
    pub written: Vec<String>,
    pub unwritable: Vec<String>,
}

/// Read a keyword list file (Lightroom Classic's format): a keyword a line, those inside it
/// indented one tab more, a synonym in braces on a line of its own under its keyword, a keyword
/// left out of export in square brackets. Gives each keyword's path and attributes, in the order of
/// the file; a keyword twice is one. A line it can't read is an error that says which.
pub fn parse_keyword_list(text: &str) -> Result<Vec<(String, KeywordInfo)>> {
    let bad = |n: usize, why: &str| CatalogError::Invalid(format!("line {n}: {why}"));
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Windows, Unix and classic Mac line ends alike
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out: Vec<(String, KeywordInfo)> = Vec::new();
    // where each keyword is in `out`, by lower-case path (a search of `out` per line was quadratic)
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // synonyms already accepted, by keyword (a search of each keyword's growing synonym list per
    // line was quadratic and re-allocated both spellings every comparison — CWE-407)
    let mut synonyms: std::collections::HashMap<usize, std::collections::HashSet<String>> = std::collections::HashMap::new();
    // the keyword at each level above the line: its path and where it is in `out`
    let mut above: Vec<(String, usize)> = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        let n = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let depth = line.chars().take_while(|c| *c == '\t').count();
        let rest = line.trim_start_matches('\t');
        // levels are tabs: spaces would flatten the hierarchy without a word
        if rest.starts_with(' ') {
            return Err(bad(n, "keywords are indented with tabs, one per level, not spaces"));
        }
        if rest.chars().any(char::is_control) {
            return Err(bad(n, "a control character in a keyword"));
        }
        let body = rest.trim();
        if let Some(synonym) = body.strip_prefix('{').and_then(|b| b.strip_suffix('}')) {
            let Some(at) = depth.checked_sub(1).and_then(|d| above.get(d)).map(|(_, at)| *at) else {
                return Err(bad(n, "a synonym (in braces) goes one tab under its keyword"));
            };
            let synonym = synonym.trim();
            if synonym.contains(SEP) {
                return Err(bad(n, "“|” separates a keyword's levels: a synonym can't hold one"));
            }
            if !synonym.is_empty()
                && synonyms.entry(at).or_default().insert(synonym.to_lowercase())
                && let Some((_, info)) = out.get_mut(at)
            {
                info.synonyms.push(synonym.to_string());
            }
            continue;
        }
        if depth > above.len() {
            return Err(bad(n, "indented more than one tab below the keyword above it"));
        }
        if depth >= MAX_LEVELS {
            return Err(bad(n, &format!("keywords nest {MAX_LEVELS} levels deep at most")));
        }
        let (name, left_out) = match body.strip_prefix('[').and_then(|b| b.strip_suffix(']')) {
            Some(name) => (name.trim(), true),
            None => (body, false),
        };
        if name.is_empty() {
            return Err(bad(n, "a keyword needs a name"));
        }
        if name.contains(SEP) {
            return Err(bad(n, "“|” separates a keyword's levels: a name can't hold one"));
        }
        above.truncate(depth);
        let path = match above.last() {
            Some((parent, _)) => format!("{parent}{SEP}{name}"),
            None => name.to_string(),
        };
        let at = match index.get(&path.to_lowercase()) {
            Some(at) => *at,
            None => {
                out.push((path.clone(), KeywordInfo::default()));
                index.insert(path.to_lowercase(), out.len() - 1);
                out.len() - 1
            }
        };
        if left_out && let Some((_, info)) = out.get_mut(at) {
            info.include_on_export = false;
        }
        // as first spelled: the keywords inside a repeat take that spelling
        let spelled = out.get(at).map_or(path, |(p, _)| p.clone());
        above.push((spelled, at));
    }
    Ok(out)
}

/// The keyword of `from` that `k` is under, the closest one when several are (`a|b` before `a`).
pub fn closest<'a>(k: &str, from: &'a [String]) -> Option<&'a String> {
    from.iter().filter(|f| is_under(k, f)).max_by_key(|f| f.split(SEP).count())
}

/// Keep the first of case-insensitively equal keywords.
fn dedupe(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|k| !k.is_empty() && seen.insert(k.to_lowercase()));
}

/// What the keyword list knows about a keyword besides the photos that carry it: Lightroom
/// Classic's keyword tag options. A keyword is listed when it was created on its own or given
/// attributes; one that only photos carry has the defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordInfo {
    /// Other words for it, exported with it (when `export_synonyms`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synonyms: Vec<String>,
    /// Written into exported files at all.
    #[serde(default = "yes")]
    pub include_on_export: bool,
    /// The keywords containing it are exported with it (`travel|italy` also gives `travel`).
    #[serde(default = "yes")]
    pub export_containing: bool,
    #[serde(default = "yes")]
    pub export_synonyms: bool,
    /// The keyword names a person.
    #[serde(default)]
    pub person: bool,
    /// New keywords go inside this one (Put New Keywords Inside This Keyword): one keyword at
    /// most, see [`Catalog::default_keyword_parent`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new_keywords_inside: bool,
    /// Part of the keyword shortcut (Set Keyword Shortcut), which ⇧K adds to or removes from the
    /// selected photos: see [`Catalog::keyword_shortcut`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keyword_shortcut: bool,
}

fn yes() -> bool {
    true
}

impl Default for KeywordInfo {
    fn default() -> Self {
        KeywordInfo {
            synonyms: Vec::new(),
            include_on_export: true,
            export_containing: true,
            export_synonyms: true,
            person: false,
            new_keywords_inside: false,
            keyword_shortcut: false,
        }
    }
}

/// A keyword in the library's keyword list: its path as written and its attributes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedKeyword {
    pub path: String,
    pub info: KeywordInfo,
}

/// A photo's keywords as an exported file carries them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportKeywords {
    /// Names, flat (`dc:subject`): each keyword's own, the keywords containing it and synonyms, as
    /// their options say.
    pub flat: Vec<String>,
    /// Full paths (`lr:hierarchicalSubject`).
    pub hierarchical: Vec<String>,
}

/// One node of the keyword tree.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KeywordNode {
    /// This level's name (`italy`).
    pub name: String,
    /// The full keyword (`travel|italy`).
    pub path: String,
    /// Photos (not deleted) with this keyword or one below it.
    pub count: usize,
    pub children: Vec<KeywordNode>,
}

impl Catalog {
    /// The keyword tree: every keyword level with photo counts, sorted by name (case-insensitive).
    pub fn keyword_tree(&self) -> Vec<KeywordNode> {
        use std::collections::HashMap;
        // every keyword level (`travel`, `travel|italy`…), one per lower-case path: the name and
        // path as first written, and the photos counted (once per photo, even when two of its
        // keywords share a parent: `seen` is the last photo counted); `sort` is the name in lower case
        struct Level {
            name: String,
            sort: String,
            path: String,
            count: usize,
            seen: usize,
        }
        let mut levels: Vec<Level> = Vec::new();
        let mut by_lower: HashMap<String, usize> = HashMap::new();
        // each level's children (by index), and the top level
        let mut kids: Vec<Vec<usize>> = Vec::new();
        let mut roots: Vec<usize> = Vec::new();
        // the keyword list first (no photos counted): a listed keyword is there without photos
        let listed: Vec<String> = self.listed_keywords().map(|k| k.path.clone()).collect();
        // each keyword as written → its levels (its own and those above it), worked out once: a
        // large library repeats a few thousand keywords tens of thousands of times (issue #1011)
        let mut chains: HashMap<&str, Vec<usize>> = HashMap::new();
        let photos = self.photos().filter(|p| p.in_library()).map(|p| (true, p.meta.keywords.as_slice()));
        for (photo, (counts, keywords)) in std::iter::once((false, listed.as_slice())).chain(photos).enumerate() {
            for k in keywords {
                let chain = chains.entry(k.as_str()).or_insert_with(|| {
                    let mut chain: Vec<usize> = Vec::new();
                    let mut path = String::new();
                    let mut lower = String::new();
                    // Level paths hold their every prefix (`path` and `lower` both), so a photo's XMP
                    // keyword split far more levels than a list file accepts (`MAX_LEVELS`) made the
                    // stored prefixes quadratic in the level count: a keyword nests [MAX_LEVELS]
                    // levels here, as a list file would, and what lies below is left out (CWE-400).
                    for (part_n, part) in (1..).zip(k.split(SEP).map(str::trim).filter(|s| !s.is_empty())) {
                        if part_n > MAX_LEVELS {
                            break;
                        }
                        if !path.is_empty() {
                            path.push(SEP);
                            lower.push(SEP);
                        }
                        path.push_str(part);
                        let sort = part.to_lowercase();
                        lower.push_str(&sort);
                        let i = match by_lower.get(&lower) {
                            Some(&i) => i,
                            None => {
                                let i = levels.len();
                                match chain.last().and_then(|&p| kids.get_mut(p)) {
                                    Some(k) => k.push(i),
                                    None => roots.push(i),
                                }
                                levels.push(Level { name: part.to_string(), sort, path: path.clone(), count: 0, seen: 0 });
                                kids.push(Vec::new());
                                by_lower.insert(lower.clone(), i);
                                i
                            }
                        };
                        chain.push(i);
                    }
                    chain
                });
                if counts {
                    for &i in chain.iter() {
                        let Some(l) = levels.get_mut(i) else { continue };
                        // photo 0 is the keyword list, which counts nothing
                        if l.seen != photo {
                            l.seen = photo;
                            l.count += 1;
                        }
                    }
                }
            }
        }
        fn build(mut list: Vec<usize>, levels: &[Level], kids: &mut [Vec<usize>]) -> Vec<KeywordNode> {
            list.sort_by(|&a, &b| levels.get(a).map(|l| &l.sort).cmp(&levels.get(b).map(|l| &l.sort)));
            list.into_iter()
                .filter_map(|i| {
                    let lv = levels.get(i)?;
                    let below = kids.get_mut(i).map(std::mem::take).unwrap_or_default();
                    let children = build(below, levels, kids);
                    Some(KeywordNode { name: lv.name.clone(), path: lv.path.clone(), count: lv.count, children })
                })
                .collect()
        }
        build(roots, &levels, &mut kids)
    }

    /// `SetMeta` ops for every photo whose keywords `f` changes.
    fn keyword_ops(&self, f: impl Fn(&[String]) -> Vec<String>) -> Vec<Op> {
        self.photos()
            .filter_map(|p| {
                let mut kws = f(&p.meta.keywords);
                dedupe(&mut kws);
                (kws != p.meta.keywords).then(|| Op::SetMeta { id: p.id, meta: Box::new(Meta { keywords: kws, ..p.meta.clone() }) })
            })
            .collect()
    }

    /// `SetKeyword` ops turning the keyword list into what `f` makes of it (lower-case path →
    /// listing): what it no longer has is taken off first, then what is new or changed is set.
    fn list_ops(&self, f: impl FnOnce(&mut BTreeMap<String, ListedKeyword>)) -> Vec<Op> {
        let before = &self.keyword_list;
        let mut after = before.clone();
        f(&mut after);
        let gone = before.iter().filter(|(k, _)| !after.contains_key(*k)).map(|(_, l)| Op::SetKeyword { path: l.path.clone(), info: None });
        let set = after
            .iter()
            .filter(|(k, l)| before.get(*k) != Some(*l))
            .map(|(_, l)| Op::SetKeyword { path: l.path.clone(), info: Some(l.info.clone()) });
        gone.chain(set).collect()
    }

    /// The keyword list once the keywords under each of `from` move to `to`: a listing landing on
    /// one already there merges into it (that one's attributes stay, the synonyms gather).
    fn list_move_ops(&self, from: &[String], to: &str) -> Vec<Op> {
        self.list_ops(|list| {
            // every moving listing leaves first, then each lands: one landing where another is
            // about to leave isn't merged into it
            let moving: Vec<String> = list.iter().filter(|(_, l)| closest(&l.path, from).is_some()).map(|(k, _)| k.clone()).collect();
            let left: Vec<ListedKeyword> = moving.iter().filter_map(|key| list.remove(key)).collect();
            for l in left {
                let Some(f) = closest(&l.path, from) else { continue };
                let path = reparent(&l.path, f, to);
                match list.get_mut(&path.to_lowercase()) {
                    Some(there) => {
                        there.info.new_keywords_inside |= l.info.new_keywords_inside;
                        there.info.keyword_shortcut |= l.info.keyword_shortcut;
                        for s in l.info.synonyms {
                            if !there.info.synonyms.iter().any(|x| same(x, &s)) {
                                there.info.synonyms.push(s);
                            }
                        }
                    }
                    None => {
                        list.insert(path.to_lowercase(), ListedKeyword { path, info: l.info });
                    }
                }
            }
        })
    }

    /// Rename `from` (and the keywords below it) to `to` on every photo and in the keyword list —
    /// one batch. Renaming onto an existing keyword merges the two.
    pub fn rename_keyword_ops(&self, from: &str, to: &str) -> Result<Op> {
        let (from, to) = (clean(from), clean(to));
        if from.is_empty() || to.is_empty() {
            return Err(CatalogError::Invalid("keyword names can't be empty".into()));
        }
        if is_under(&to, &from) && !same(&to, &from) {
            return Err(CatalogError::Invalid("can't move a keyword below itself".into()));
        }
        let mut ops = self.keyword_ops(|kws| kws.iter().map(|k| if is_under(k, &from) { reparent(k, &from, &to) } else { k.clone() }).collect());
        ops.extend(self.list_move_ops(std::slice::from_ref(&from), &to));
        Ok(Op::Batch { ops })
    }

    /// Remove `keyword` and the keywords below it from every photo and from the keyword list.
    pub fn delete_keyword_ops(&self, keyword: &str) -> Result<Op> {
        let k = clean(keyword);
        if k.is_empty() {
            return Err(CatalogError::Invalid("empty keyword".into()));
        }
        let mut ops = self.keyword_ops(|kws| kws.iter().filter(|x| !is_under(x, &k)).cloned().collect());
        ops.extend(self.list_ops(|list| list.retain(|_, l| !is_under(&l.path, &k))));
        Ok(Op::Batch { ops })
    }

    /// Merge several keywords (with their children) into `into`.
    pub fn merge_keywords_ops(&self, from: &[String], into: &str) -> Result<Op> {
        let into = clean(into);
        let from: Vec<String> = from.iter().map(|f| clean(f)).filter(|f| !f.is_empty() && !same(f, &into)).collect();
        if into.is_empty() || from.is_empty() {
            return Err(CatalogError::Invalid("merge needs keywords and a target".into()));
        }
        if from.iter().any(|f| is_under(&into, f)) {
            return Err(CatalogError::Invalid("can't merge a keyword into one below it".into()));
        }
        let mut ops =
            self.keyword_ops(|kws| kws.iter().map(|k| closest(k, &from).map(|f| reparent(k, f, &into)).unwrap_or_else(|| k.clone())).collect());
        ops.extend(self.list_move_ops(&from, &into));
        Ok(Op::Batch { ops })
    }

    /// What exported files carry for a photo with these keywords (Lightroom Classic's keyword tag
    /// options, [`KeywordInfo`]): a keyword left out of export goes nowhere; otherwise its name
    /// and full path, the keywords containing it that are exported themselves (unless "export
    /// containing keywords" is off) and synonyms (unless "export synonyms" is off). Each name once,
    /// whatever its case.
    pub fn export_keywords(&self, keywords: &[String]) -> ExportKeywords {
        let defaults = KeywordInfo::default();
        let info = |path: &str| self.keyword_info(path).unwrap_or(&defaults);
        let mut out = ExportKeywords::default();
        let push = |v: &mut Vec<String>, s: &str| {
            if !s.is_empty() && !v.iter().any(|x| same(x, s)) {
                v.push(s.to_string());
            }
        };
        for k in keywords {
            let path = clean(k);
            let me = info(&path);
            if path.is_empty() || !me.include_on_export {
                continue;
            }
            let levels: Vec<&str> = path.split(SEP).collect();
            let mut names = |at: usize, i: &KeywordInfo| {
                if let Some(name) = levels.get(at) {
                    push(&mut out.flat, name);
                }
                if i.export_synonyms {
                    for s in &i.synonyms {
                        push(&mut out.flat, s.trim());
                    }
                }
            };
            names(levels.len().saturating_sub(1), me);
            if me.export_containing {
                for at in (0..levels.len().saturating_sub(1)).rev() {
                    let parent = levels.get(..=at).map(|l| l.join("|")).unwrap_or_default();
                    let pi = info(&parent);
                    if pi.include_on_export {
                        names(at, pi);
                    }
                }
            }
            push(&mut out.hierarchical, &path);
        }
        out
    }

    /// The keyword list as a file (Lightroom Classic's format, [`parse_keyword_list`]): every
    /// keyword of the tree, those inside one indented a tab more, synonyms in braces under their
    /// keyword, a keyword left out of export in square brackets; by name. What the format can't hold
    /// is left out and named, so the file always reads back.
    pub fn keyword_list_text(&self) -> KeywordListFile {
        // a name the list would read as something else, or not at all
        fn unwritable(name: &str) -> bool {
            name.trim().is_empty()
                || name != name.trim()
                || name.chars().any(char::is_control)
                // a byte-order mark: on the first line the reader takes it for the file's own
                || name.starts_with('\u{feff}')
                || (name.starts_with('[') && name.ends_with(']'))
                || (name.starts_with('{') && name.ends_with('}'))
        }
        fn write(c: &Catalog, nodes: &[KeywordNode], depth: usize, file: &mut KeywordListFile) {
            let tabs = "\t".repeat(depth);
            for n in nodes {
                // the keywords inside go with it: under its parent they'd be other keywords
                if depth >= MAX_LEVELS || unwritable(&n.name) {
                    file.unwritable.push(n.path.clone());
                    continue;
                }
                let info = c.keyword_info(&n.path).cloned().unwrap_or_default();
                if info.include_on_export {
                    file.text.push_str(&format!("{tabs}{}\n", n.name));
                } else {
                    file.text.push_str(&format!("{tabs}[{}]\n", n.name));
                }
                file.written.push(n.path.clone());
                // the reader trims synonyms and takes one per spelling: an empty one, or another's
                // in a different case, is no loss; a line break or a “|” is
                let mut synonyms: Vec<&str> = Vec::new();
                for s in info.synonyms.iter().map(|s| s.trim()) {
                    if s.is_empty() || synonyms.iter().any(|x| same(x, s)) {
                        continue;
                    }
                    if s.chars().any(char::is_control) || s.contains(SEP) {
                        file.unwritable.push(format!("{} {{{s}}}", n.path));
                    } else {
                        synonyms.push(s);
                        file.text.push_str(&format!("{tabs}\t{{{s}}}\n"));
                    }
                }
                write(c, &n.children, depth + 1, file);
            }
        }
        let mut file = KeywordListFile::default();
        // The tree is capped at [`MAX_LEVELS`] (its level rows hold every level's prefix twice, so a
        // synthetic keyword past that depth made the stored bytes quadratic); whatever lies deeper
        // comes from the photos' and list's own spellings instead, named unwritable as before.
        let mut over_deep: std::collections::HashSet<String> = Default::default();
        for k in self.listed_keywords().map(|k| &k.path) {
            if k.split(SEP).count() > MAX_LEVELS {
                over_deep.insert(k.clone());
            }
        }
        for p in self.photos().filter(|p| p.in_library()) {
            for k in &p.meta.keywords {
                if clean(k).split(SEP).count() > MAX_LEVELS {
                    over_deep.insert(k.clone());
                }
            }
        }
        write(self, &self.keyword_tree(), 0, &mut file);
        file.unwritable.extend(over_deep);
        file
    }

    /// Import a keyword list (read with [`parse_keyword_list`]) — one batch: the keywords the
    /// library doesn't have are added with the list's attributes, in the library's spelling of the
    /// levels it has; those it has keep their attributes and gain the list's synonyms. Also says how
    /// many keywords were added and how many gained synonyms.
    pub fn import_keywords_ops(&self, entries: &[(String, KeywordInfo)]) -> (Op, usize, usize) {
        // every keyword the library has, by lower-case path, as it spells it: learnt once from the
        // tree (looking each line up among all the photos' keywords took minutes for big lists)
        let mut known: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        fn learn(nodes: &[KeywordNode], known: &mut std::collections::HashMap<String, String>) {
            for n in nodes {
                known.insert(n.path.to_lowercase(), n.path.clone());
                learn(&n.children, known);
            }
        }
        learn(&self.keyword_tree(), &mut known);
        // a path in the library's spelling of each level it has
        let spell = |path: &str, known: &std::collections::HashMap<String, String>| -> String {
            let levels: Vec<&str> = path.split(SEP).collect();
            let mut out = String::new();
            for (i, level) in levels.iter().enumerate() {
                if !out.is_empty() {
                    out.push(SEP);
                }
                let prefix = levels.get(..=i).map(|l| l.join("|").to_lowercase()).unwrap_or_default();
                match known.get(&prefix).and_then(|p| p.rsplit(SEP).next()) {
                    Some(name) => out.push_str(name),
                    None => out.push_str(level),
                }
            }
            out
        };
        let (mut added, mut updated) = (0, 0);
        let ops = self.list_ops(|list| {
            for (path, info) in entries {
                let path = spell(&clean(path), &known);
                let key = path.to_lowercase();
                if path.is_empty() {
                    continue;
                }
                if !known.contains_key(&key) {
                    list.insert(
                        key.clone(),
                        ListedKeyword {
                            path: path.clone(),
                            info: KeywordInfo { new_keywords_inside: false, keyword_shortcut: false, ..info.clone() },
                        },
                    );
                    // it and its parents are the library's now: what follows spells them so
                    let levels: Vec<&str> = path.split(SEP).collect();
                    for i in 0..levels.len() {
                        let prefix = levels.get(..=i).map(|l| l.join("|")).unwrap_or_default();
                        known.entry(prefix.to_lowercase()).or_insert(prefix);
                    }
                    added += 1;
                    continue;
                }
                // one the library has: the list's synonyms join its own (listed only if it gains some)
                let mut info_now = list.get(&key).map(|l| l.info.clone()).unwrap_or_default();
                let before = info_now.synonyms.len();
                for s in &info.synonyms {
                    if !info_now.synonyms.iter().any(|x| same(x, s)) {
                        info_now.synonyms.push(s.clone());
                    }
                }
                if info_now.synonyms.len() > before {
                    let path = list.get(&key).map_or(path, |l| l.path.clone());
                    list.insert(key, ListedKeyword { path, info: info_now });
                    updated += 1;
                }
            }
        });
        (Op::Batch { ops }, added, updated)
    }

    /// The keyword is in the tree: listed or on a library photo (not one in Recently Deleted or
    /// only browsed), itself or one below it.
    pub fn has_keyword(&self, path: &str) -> bool {
        self.keyword_path(path).is_some()
    }

    /// Where a keyword typed for a photo goes: inside the default parent when it is a new name (Put
    /// New Keywords Inside This Keyword); a keyword the library has, or a path typed whole, as
    /// typed (cleaned).
    pub fn typed_keyword(&self, typed: &str) -> String {
        let k = clean(typed);
        match self.default_keyword_parent() {
            Some(parent) if !k.is_empty() && !k.contains(SEP) && !self.has_keyword(&k) => format!("{parent}{SEP}{k}"),
            _ => k,
        }
    }

    /// `path` with each level the library has spelled as the library spells it (`TRAVEL|Rome` next
    /// to `travel|italy` is `travel|Rome`).
    fn spelled(&self, path: &str) -> String {
        let levels: Vec<&str> = path.split(SEP).collect();
        let mut out = String::new();
        for (i, level) in levels.iter().enumerate() {
            let prefix = levels.get(..=i).map(|l| l.join("|")).unwrap_or_default();
            let name = self.keyword_path(&prefix).and_then(|p| p.rsplit(SEP).next().map(str::to_string)).unwrap_or_else(|| level.to_string());
            if !out.is_empty() {
                out.push(SEP);
            }
            out.push_str(&name);
        }
        out
    }

    /// The keyword new keywords go inside (Put New Keywords Inside This Keyword), if any.
    pub fn default_keyword_parent(&self) -> Option<String> {
        self.keyword_list.values().find(|l| l.info.new_keywords_inside).map(|l| l.path.clone())
    }

    /// Make `keyword` the one new keywords go inside (`None`: none) — one batch. It is listed if it
    /// wasn't, keeping the attributes it has.
    pub fn set_default_parent_ops(&self, keyword: Option<&str>) -> Result<Op> {
        let path = match keyword {
            Some(k) => Some(self.keyword_path(k).ok_or_else(|| CatalogError::Invalid(format!("no keyword “{}”", clean(k))))?),
            None => None,
        };
        Ok(Op::Batch {
            ops: self.list_ops(|list| {
                for l in list.values_mut() {
                    l.info.new_keywords_inside = false;
                }
                if let Some(path) = path {
                    let l = list.entry(path.to_lowercase()).or_insert_with(|| ListedKeyword { path, info: KeywordInfo::default() });
                    l.info.new_keywords_inside = true;
                }
            }),
        })
    }

    /// The keyword shortcut's keywords (Set Keyword Shortcut), in the keyword list's order.
    pub fn keyword_shortcut(&self) -> Vec<String> {
        self.keyword_list.values().filter(|l| l.info.keyword_shortcut).map(|l| l.path.clone()).collect()
    }

    /// Make `keywords` the keyword shortcut, in one batch. A keyword the library doesn't have yet is
    /// listed, and the others keep the attributes they have.
    pub fn set_keyword_shortcut_ops(&self, keywords: &[String]) -> Op {
        let paths: Vec<String> =
            keywords.iter().map(|k| self.keyword_path(k).unwrap_or_else(|| self.spelled(&clean(k)))).filter(|k| !k.is_empty()).collect();
        Op::Batch {
            ops: self.list_ops(|list| {
                for l in list.values_mut() {
                    l.info.keyword_shortcut = false;
                }
                for path in paths {
                    let l = list.entry(path.to_lowercase()).or_insert_with(|| ListedKeyword { path, info: KeywordInfo::default() });
                    l.info.keyword_shortcut = true;
                }
            }),
        }
    }

    /// The keyword as written in the library (whatever the case of `path`): its listing, or the
    /// levels a photo's keyword spells it with. `None` when there is no such keyword.
    pub fn keyword_path(&self, path: &str) -> Option<String> {
        let k = clean(path);
        if k.is_empty() {
            return None;
        }
        if let Some(l) = self.keyword_list.get(&k.to_lowercase()) {
            return Some(l.path.clone());
        }
        // the levels of a listed keyword below it, or of a photo's
        let levels = k.split(SEP).count();
        let listed = self.keyword_list.values().map(|l| &l.path);
        listed
            .chain(self.photos().filter(|p| p.in_library()).flat_map(|p| p.meta.keywords.iter()))
            .find(|x| is_under(x, &k))
            .map(|x| x.split(SEP).map(str::trim).filter(|s| !s.is_empty()).take(levels).collect::<Vec<_>>().join("|"))
    }

    /// Create a keyword with these attributes, and give it to `photos` — one batch. A keyword that
    /// exists already is refused.
    pub fn create_keyword_ops(&self, path: &str, info: KeywordInfo, photos: &[crate::PhotoId]) -> Result<Op> {
        let path = clean(path);
        if path.is_empty() {
            return Err(CatalogError::Invalid("keyword names can't be empty".into()));
        }
        if self.has_keyword(&path) {
            return Err(CatalogError::KeywordExists(path));
        }
        let path = self.spelled(&path);
        let mut ops = vec![Op::SetKeyword { path: path.clone(), info: Some(info) }];
        for id in photos {
            let p = self.photo(*id).ok_or(CatalogError::NoPhoto(*id))?;
            let mut meta = p.meta.clone();
            meta.keywords.push(path.clone());
            dedupe(&mut meta.keywords);
            ops.push(Op::SetMeta { id: *id, meta: Box::new(meta) });
        }
        Ok(Op::Batch { ops })
    }

    /// Move `keyword` (with the keywords below it) inside `parent`, or to the top level (`None`).
    /// It keeps its name. Onto a keyword that is there already the two merge, which only happens
    /// with `merge`; otherwise [`CatalogError::KeywordExists`].
    pub fn move_keyword_ops(&self, keyword: &str, parent: Option<&str>, merge: bool) -> Result<Op> {
        let Some(from) = self.keyword_path(keyword) else {
            return Err(CatalogError::Invalid(format!("no keyword “{}”", clean(keyword))));
        };
        let leaf = from.rsplit(SEP).next().unwrap_or(&from);
        let to = match parent.map(clean).filter(|p| !p.is_empty()) {
            Some(p) if is_under(&p, &from) => return Err(CatalogError::Invalid("can't move a keyword inside itself".into())),
            Some(p) => format!("{}{SEP}{leaf}", self.keyword_path(&p).unwrap_or(p)),
            None => leaf.to_string(),
        };
        if same(&to, &from) {
            return Ok(Op::Batch { ops: vec![] });
        }
        if !merge && self.has_keyword(&to) {
            return Err(CatalogError::KeywordExists(to));
        }
        self.rename_keyword_ops(&from, &to)
    }

    /// Rename `keyword`'s last level to `name` (it stays where it is) and set its attributes — one
    /// batch. A name that is taken is refused ([`CatalogError::KeywordExists`]): merging is its own
    /// action.
    pub fn edit_keyword_ops(&self, keyword: &str, name: &str, info: KeywordInfo) -> Result<Op> {
        let Some(from) = self.keyword_path(keyword) else {
            return Err(CatalogError::Invalid(format!("no keyword “{}”", clean(keyword))));
        };
        let name = name.trim();
        if name.is_empty() || name.contains(SEP) {
            return Err(CatalogError::Invalid("a keyword's name is one level, without “|”".into()));
        }
        let to = match from.rsplit_once(SEP) {
            Some((parent, _)) => format!("{parent}{SEP}{name}"),
            None => name.to_string(),
        };
        let mut ops = Vec::new();
        if to != from {
            if !same(&to, &from) && self.has_keyword(&to) {
                return Err(CatalogError::KeywordExists(to));
            }
            if let Op::Batch { ops: renamed } = self.rename_keyword_ops(&from, &to)? {
                ops = renamed;
            }
        }
        // set the attributes only when they change something: an unlisted keyword with the default
        // ones stays unlisted
        let now = self.keyword_info(&from).cloned();
        let changed = match &now {
            Some(now) => *now != info || to != from,
            None => info != KeywordInfo::default(),
        };
        if changed {
            ops.push(Op::SetKeyword { path: to, info: Some(info) });
        }
        Ok(Op::Batch { ops })
    }

    /// Take off the keyword list the keywords no library photo has, nor any keyword below them — one
    /// batch.
    pub fn purge_unused_keywords_ops(&self) -> Op {
        let used: Vec<&String> = self.photos().filter(|p| p.in_library()).flat_map(|p| p.meta.keywords.iter()).collect();
        Op::Batch { ops: self.list_ops(|list| list.retain(|_, l| used.iter().any(|k| is_under(k, &l.path)))) }
    }

    /// Keyword suggestions for a photo that has `current` keywords: with a typed `prefix`, the
    /// library's keywords containing it (those starting with it first); otherwise keywords that
    /// appear together with `current` on other photos, then the most used ones. Most frequent
    /// first, at most `n`, never one of `current`.
    pub fn keyword_suggestions(&self, current: &[String], prefix: &str, n: usize) -> Vec<String> {
        // the photos' keywords with their counts, and those only the list has (none)
        let mut all = self.keywords();
        for l in self.keyword_list.values() {
            if !all.iter().any(|(k, _)| same(k, &l.path)) {
                all.push((l.path.clone(), 0));
            }
        }
        let has = |k: &str| current.iter().any(|c| same(c, k));
        let q = prefix.trim().to_lowercase();
        let mut scored: Vec<(i64, String)> = if !q.is_empty() {
            all.into_iter()
                .filter(|(k, _)| !has(k))
                .filter_map(|(k, c)| {
                    let l = k.to_lowercase();
                    let leaf = l.rsplit(SEP).next().unwrap_or(&l).to_string();
                    let rank = if l.starts_with(&q) || leaf.starts_with(&q) {
                        2
                    } else if l.contains(&q) {
                        1
                    } else {
                        return None;
                    };
                    Some((rank * 1_000_000 + c as i64, k))
                })
                .collect()
        } else {
            let mut co: std::collections::HashMap<String, i64> = Default::default();
            if !current.is_empty() {
                for p in self.photos().filter(|p| p.in_library()) {
                    if p.meta.keywords.iter().any(|k| has(k)) {
                        for k in p.meta.keywords.iter().filter(|k| !has(k)) {
                            *co.entry(k.clone()).or_default() += 1;
                        }
                    }
                }
            }
            all.into_iter().filter(|(k, _)| !has(k)).map(|(k, c)| (co.get(&k).copied().unwrap_or(0) * 1_000_000 + c as i64, k)).collect()
        };
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        scored.into_iter().take(n).map(|(_, k)| k).collect()
    }
}

#[cfg(test)]
mod tests;
