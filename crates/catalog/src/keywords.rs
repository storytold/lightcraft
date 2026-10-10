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
    out.extend(flat.iter().map(|k| clean(k)).filter(|k| !hierarchical.iter().any(|h| h.split(SEP).any(|level| same(level.trim(), k)))));
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
            if let Some((_, info)) = out.get_mut(at)
                && !synonym.is_empty()
                && !info.synonyms.iter().any(|s| same(s, synonym))
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
        use std::collections::{HashMap, HashSet};
        // every keyword level (`travel`, `travel|italy`…) by its lower-case path: the name and
        // path as first written, and the photos counted (once per photo, even when two of its
        // keywords share a parent)
        struct Level {
            name: String,
            path: String,
            count: usize,
            parent: Option<String>,
        }
        let mut levels: HashMap<String, Level> = HashMap::new();
        let mut this_photo: HashSet<String> = HashSet::new();
        // the keyword list first (no photos counted): a listed keyword is there without photos
        let listed: Vec<String> = self.listed_keywords().map(|k| k.path.clone()).collect();
        let photos = self.photos().filter(|p| p.in_library()).map(|p| (true, p.meta.keywords.as_slice()));
        for (counts, keywords) in std::iter::once((false, listed.as_slice())).chain(photos) {
            this_photo.clear();
            for k in keywords {
                let mut path = String::new();
                let mut lower = String::new();
                let mut parent: Option<String> = None;
                for part in k.split(SEP).map(str::trim).filter(|s| !s.is_empty()) {
                    if !path.is_empty() {
                        path.push(SEP);
                        lower.push(SEP);
                    }
                    path.push_str(part);
                    lower.push_str(&part.to_lowercase());
                    let l = levels.entry(lower.clone()).or_insert_with(|| Level {
                        name: part.to_string(),
                        path: path.clone(),
                        count: 0,
                        parent: parent.clone(),
                    });
                    if counts && this_photo.insert(lower.clone()) {
                        l.count += 1;
                    }
                    parent = Some(lower.clone());
                }
            }
        }
        let mut kids: HashMap<Option<String>, Vec<String>> = HashMap::new();
        for (lower, l) in &levels {
            kids.entry(l.parent.clone()).or_default().push(lower.clone());
        }
        fn build(at: Option<String>, levels: &HashMap<String, Level>, kids: &mut HashMap<Option<String>, Vec<String>>) -> Vec<KeywordNode> {
            let Some(mut list) = kids.remove(&at) else { return Vec::new() };
            list.sort_by_key(|l| levels[l].name.to_lowercase());
            list.into_iter()
                .map(|l| {
                    let lv = &levels[&l];
                    let children = build(Some(l.clone()), levels, kids);
                    KeywordNode { name: lv.name.clone(), path: lv.path.clone(), count: lv.count, children }
                })
                .collect()
        }
        build(None, &levels, &mut kids)
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
        write(self, &self.keyword_tree(), 0, &mut file);
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
                    list.insert(key.clone(), ListedKeyword { path: path.clone(), info: KeywordInfo { new_keywords_inside: false, ..info.clone() } });
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
mod tests {
    use super::*;
    use crate::{Filter, Photo, PhotoId, Source};

    fn lib(kws: &[&[&str]]) -> (Catalog, Vec<PhotoId>) {
        let mut c = Catalog::new();
        let mut ids = Vec::new();
        for k in kws {
            let id = c.alloc_photo_id();
            let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a.jpg", "JPEG", 3, 2, "2026-01-01");
            p.meta.keywords = k.iter().map(|s| s.to_string()).collect();
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            ids.push(id);
        }
        (c, ids)
    }
    fn kws(c: &Catalog, id: PhotoId) -> Vec<String> {
        c.photo(id).unwrap().meta.keywords.clone()
    }

    #[test]
    fn tree_counts_photos_per_level() {
        let (c, ids) = lib(&[&["travel|Italy|Rome", "beach"], &["Travel|italy"], &["travel|France", "travel|italy|rome"], &["beach"]]);
        // Levels keep the spelling of the lowest-id photo that has them, and ids are random.
        let first_of = |idx: &[usize]| *idx.iter().min_by_key(|&&i| ids[i]).unwrap();
        let travel = ["travel", "Travel", "travel"][first_of(&[0, 1, 2])];
        let italy = ["travel|Italy", "Travel|italy", "travel|italy"][first_of(&[0, 1, 2])];
        let rome = ["travel|Italy|Rome", "", "travel|italy|rome"][first_of(&[0, 2])];
        let t = c.keyword_tree();
        assert_eq!(t.iter().map(|n| (n.name.as_str(), n.count)).collect::<Vec<_>>(), vec![("beach", 2), (travel, 3)]);
        let travel = &t[1];
        assert_eq!(travel.children.iter().map(|n| (n.path.as_str(), n.count)).collect::<Vec<_>>(), vec![("travel|France", 1), (italy, 3)]);
        assert_eq!(travel.children[1].children[0].path, rome);
        assert_eq!(travel.children[1].children[0].count, 2);
        // filtering by a parent finds its children
        let f = Filter { keyword: Some("travel|italy".into()), ..Default::default() };
        assert_eq!(c.query(&f, &Default::default()).len(), 3);
    }

    #[test]
    fn rename_delete_merge_are_single_undoable_batches() {
        let (mut c, ids) = lib(&[&["travel|italy|rome", "beach"], &["Travel|Italy"], &["italia", "travel|italy"], &["sea"]]);
        let before = c.to_snapshot();
        let op = c.rename_keyword_ops("travel|italy", "Europe|Italy").unwrap();
        let Op::Batch { ops } = &op else { panic!() };
        assert_eq!(ops.len(), 3, "only photos that change");
        let inv = c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["Europe|Italy|rome", "beach"]);
        assert_eq!(kws(&c, ids[1]), ["Europe|Italy"]);
        c.apply(inv).unwrap();
        assert_eq!(c.to_snapshot(), before);
        // merge `italia` into the existing hierarchical keyword: duplicates collapse
        let op = c.merge_keywords_ops(&["italia".into()], "travel|italy").unwrap();
        c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[2]), ["travel|italy"]);
        // delete removes the keyword and its children everywhere
        let op = c.delete_keyword_ops("TRAVEL").unwrap();
        c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["beach"]);
        assert!(kws(&c, ids[1]).is_empty());
        assert_eq!(c.keywords(), vec![("beach".to_string(), 1), ("sea".to_string(), 1)]);
        // invalid requests
        assert!(c.rename_keyword_ops("beach", " ").is_err());
        assert!(c.rename_keyword_ops("a", "a|b").is_err());
        assert!(c.merge_keywords_ops(&["a".into()], "a|b").is_err());
        assert_eq!(clean(" a | |b "), "a|b");
    }

    fn tree_paths(nodes: &[KeywordNode]) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        for n in nodes {
            out.push((n.path.clone(), n.count));
            out.extend(tree_paths(&n.children));
        }
        out
    }

    /// A keyword can be listed before any photo has it (Lightroom Classic's Create Keyword Tag):
    /// it is in the tree with no photos, its parents too, and undo takes it away.
    #[test]
    fn a_keyword_listed_with_no_photos_is_in_the_tree() {
        let (mut c, _) = lib(&[&["travel|France"]]);
        let undo = c.apply(Op::SetKeyword { path: " travel | Italy ".into(), info: Some(KeywordInfo::default()) }).unwrap();
        assert_eq!(tree_paths(&c.keyword_tree()), [("travel".to_string(), 1), ("travel|France".to_string(), 1), ("travel|Italy".to_string(), 0)]);
        assert_eq!(c.keyword_info("TRAVEL|italy"), Some(&KeywordInfo::default()), "found whatever the case");
        c.apply(undo).unwrap();
        assert_eq!(tree_paths(&c.keyword_tree()), [("travel".to_string(), 1), ("travel|France".to_string(), 1)]);
        assert_eq!(c.keyword_info("travel|Italy"), None);
    }

    /// A listed keyword needs a name, and its attributes start as Lightroom Classic's do: exported,
    /// with the keywords containing it and its synonyms; not a person.
    #[test]
    fn a_listed_keyword_needs_a_name_and_starts_exported() {
        let mut c = Catalog::new();
        assert!(c.apply(Op::SetKeyword { path: " | ".into(), info: Some(KeywordInfo::default()) }).is_err());
        let info = KeywordInfo::default();
        assert!(info.include_on_export && info.export_containing && info.export_synonyms && !info.person && info.synonyms.is_empty());
    }

    /// Changing a listed keyword's attributes is undone to the attributes it had, under the name as
    /// it was written.
    #[test]
    fn changing_a_listed_keyword_is_undone_to_what_it_was() {
        let mut c = Catalog::new();
        c.apply(Op::SetKeyword { path: "Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
        let person = KeywordInfo { person: true, synonyms: vec!["Marriage".into()], ..KeywordInfo::default() };
        let undo = c.apply(Op::SetKeyword { path: "weddings".into(), info: Some(person.clone()) }).unwrap();
        assert_eq!(c.keyword_info("Weddings"), Some(&person));
        assert_eq!(undo, Op::SetKeyword { path: "Weddings".into(), info: Some(KeywordInfo::default()) });
        c.apply(undo).unwrap();
        assert_eq!(c.keyword_info("weddings"), Some(&KeywordInfo::default()));
    }

    fn listed(c: &Catalog) -> Vec<(String, KeywordInfo)> {
        let mut v: Vec<(String, KeywordInfo)> = c.listed_keywords().map(|k| (k.path.clone(), k.info.clone())).collect();
        v.sort_by_key(|(p, _)| p.to_lowercase());
        v
    }

    fn with_synonyms(words: &[&str]) -> KeywordInfo {
        KeywordInfo { synonyms: words.iter().map(|w| w.to_string()).collect(), ..KeywordInfo::default() }
    }

    /// Renaming (or moving) a keyword takes its attributes and its listed children along, photos
    /// or not, in the same undo step.
    #[test]
    fn renaming_a_keyword_carries_its_listing_along() {
        let (mut c, ids) = lib(&[&["travel|italy"]]);
        c.apply(Op::SetKeyword { path: "travel".into(), info: Some(with_synonyms(&["trip"])) }).unwrap();
        c.apply(Op::SetKeyword { path: "travel|Spain".into(), info: Some(KeywordInfo::default()) }).unwrap();
        let before = c.to_snapshot();
        let op = c.rename_keyword_ops("Travel", "Places|Europe").unwrap();
        let undo = c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["Places|Europe|italy"]);
        assert_eq!(
            listed(&c),
            [("Places|Europe".to_string(), with_synonyms(&["trip"])), ("Places|Europe|Spain".to_string(), KeywordInfo::default())]
        );
        c.apply(undo).unwrap();
        assert_eq!(c.to_snapshot(), before);
    }

    /// A keyword only the list holds (no photos) can be renamed too.
    #[test]
    fn a_keyword_without_photos_can_be_renamed() {
        let mut c = Catalog::new();
        c.apply(Op::SetKeyword { path: "weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
        let op = c.rename_keyword_ops("weddings", "Events|Weddings").unwrap();
        c.apply(op).unwrap();
        assert_eq!(listed(&c), [("Events|Weddings".to_string(), KeywordInfo::default())]);
    }

    /// Merging keeps the target's attributes and adds the merged keyword's synonyms to them; a target
    /// not listed yet takes the merged keyword's attributes.
    #[test]
    fn merging_keeps_the_targets_attributes_and_gathers_synonyms() {
        let (mut c, _) = lib(&[&["holiday"], &["travel"]]);
        let person = KeywordInfo { person: true, ..with_synonyms(&["trip"]) };
        c.apply(Op::SetKeyword { path: "travel".into(), info: Some(person.clone()) }).unwrap();
        c.apply(Op::SetKeyword {
            path: "holiday".into(),
            info: Some(KeywordInfo { include_on_export: false, ..with_synonyms(&["vacation", "Trip"]) }),
        })
        .unwrap();
        let op = c.merge_keywords_ops(&["holiday".into()], "travel").unwrap();
        c.apply(op).unwrap();
        assert_eq!(listed(&c), [("travel".to_string(), KeywordInfo { person: true, ..with_synonyms(&["trip", "vacation"]) })]);
        // onto a keyword that isn't listed: it takes the merged one's attributes
        let mut c = Catalog::new();
        c.apply(Op::SetKeyword { path: "holiday".into(), info: Some(with_synonyms(&["vacation"])) }).unwrap();
        let op = c.merge_keywords_ops(&["holiday".into()], "Travel").unwrap();
        c.apply(op).unwrap();
        assert_eq!(listed(&c), [("Travel".to_string(), with_synonyms(&["vacation"]))]);
    }

    /// Deleting a keyword takes it, and the keywords below it, off the list as well as off photos.
    #[test]
    fn deleting_a_keyword_takes_it_off_the_list() {
        let (mut c, _) = lib(&[&["travel|italy"], &["beach"]]);
        for path in ["travel", "travel|spain", "beach"] {
            c.apply(Op::SetKeyword { path: path.into(), info: Some(KeywordInfo::default()) }).unwrap();
        }
        let before = c.to_snapshot();
        let op = c.delete_keyword_ops("travel").unwrap();
        let undo = c.apply(op).unwrap();
        assert_eq!(listed(&c), [("beach".to_string(), KeywordInfo::default())]);
        assert_eq!(tree_paths(&c.keyword_tree()), [("beach".to_string(), 1)]);
        c.apply(undo).unwrap();
        assert_eq!(c.to_snapshot(), before);
    }

    /// Creating a keyword lists it with its attributes, and can tag photos with it in the same undo
    /// step. A keyword that exists already (listed, or on a photo) isn't created again.
    #[test]
    fn creating_a_keyword_lists_it_and_can_tag_photos() {
        let (mut c, ids) = lib(&[&["beach"], &[]]);
        let before = c.to_snapshot();
        let op = c.create_keyword_ops("Events | Weddings", with_synonyms(&["marriage"]), &[ids[1]]).unwrap();
        let undo = c.apply(op).unwrap();
        assert_eq!(listed(&c), [("Events|Weddings".to_string(), with_synonyms(&["marriage"]))]);
        assert_eq!(kws(&c, ids[1]), ["Events|Weddings"]);
        c.apply(undo).unwrap();
        assert_eq!(c.to_snapshot(), before);
        for exists in ["BEACH", "events|weddings"] {
            if exists == "events|weddings" {
                let op = c.create_keyword_ops("Events|Weddings", KeywordInfo::default(), &[]).unwrap();
                c.apply(op).unwrap();
            }
            assert!(c.create_keyword_ops(exists, KeywordInfo::default(), &[]).is_err(), "{exists}");
        }
        assert!(c.create_keyword_ops(" | ", KeywordInfo::default(), &[]).is_err());
    }

    /// Moving a keyword nests it inside another (or takes it to the top level), with its children,
    /// on photos and in the list. It can't go inside itself or one of its children, and moving it
    /// where it already is changes nothing.
    #[test]
    fn moving_a_keyword_nests_it_inside_another() {
        let (mut c, ids) = lib(&[&["Italy|Rome"], &["Europe"]]);
        // typed in any case, the keywords keep the spelling the library has
        let op = c.move_keyword_ops("italy", Some("EUROPE"), false).unwrap();
        c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["Europe|Italy|Rome"]);
        let op = c.move_keyword_ops("europe|italy", None, false).unwrap();
        c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["Italy|Rome"], "back at the top level");
        assert!(c.move_keyword_ops("italy", Some("italy|rome"), false).is_err(), "inside its own child");
        assert!(c.move_keyword_ops("italy", Some("Italy"), false).is_err(), "inside itself");
        assert_eq!(c.move_keyword_ops("italy|rome", Some("italy"), false).unwrap(), Op::Batch { ops: vec![] }, "already there");
        assert!(c.move_keyword_ops("lisbon", Some("europe"), false).is_err(), "no such keyword");
    }

    /// Moving onto a name that is taken (Europe already has a Rome) merges the two, so the move
    /// asks first: refused unless merging is allowed.
    #[test]
    fn moving_onto_a_taken_name_merges_only_when_allowed() {
        let (mut c, ids) = lib(&[&["rome"], &["europe|rome"]]);
        let err = c.move_keyword_ops("rome", Some("europe"), false).unwrap_err();
        assert!(matches!(err, CatalogError::KeywordExists(ref k) if k == "europe|rome"), "{err:?}");
        let op = c.move_keyword_ops("rome", Some("europe"), true).unwrap();
        c.apply(op).unwrap();
        assert_eq!((kws(&c, ids[0]), kws(&c, ids[1])), (vec!["europe|rome".to_string()], vec!["europe|rome".to_string()]));
    }

    /// Editing a keyword renames it (its last level: it stays where it is) and sets its attributes,
    /// in one undo step. A name that is taken is refused: merging is its own action.
    #[test]
    fn editing_a_keyword_renames_it_and_sets_its_attributes() {
        let (mut c, ids) = lib(&[&["travel|italy"], &["travel|spain"]]);
        let before = c.to_snapshot();
        let person = KeywordInfo { person: true, ..KeywordInfo::default() };
        let op = c.edit_keyword_ops("travel|italy", "Italia", person.clone()).unwrap();
        let undo = c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["travel|Italia"]);
        assert_eq!(c.keyword_info("travel|italia"), Some(&person));
        c.apply(undo).unwrap();
        assert_eq!(c.to_snapshot(), before);
        // attributes alone
        let op = c.edit_keyword_ops("travel", "travel", with_synonyms(&["trip"])).unwrap();
        c.apply(op).unwrap();
        assert_eq!(listed(&c), [("travel".to_string(), with_synonyms(&["trip"]))]);
        assert!(matches!(c.edit_keyword_ops("travel|italy", "Spain", KeywordInfo::default()), Err(CatalogError::KeywordExists(_))));
        assert!(c.edit_keyword_ops("travel|italy", "a|b", KeywordInfo::default()).is_err(), "a name, not a path");
        assert!(c.edit_keyword_ops("travel|italy", "  ", KeywordInfo::default()).is_err());
    }

    /// Purging takes off the list the keywords no photo has (nor any keyword below them), in one
    /// undo step; keywords photos carry stay.
    #[test]
    fn purging_takes_unused_keywords_off_the_list() {
        let (mut c, _) = lib(&[&["travel|italy"]]);
        for path in ["travel", "travel|italy", "travel|spain", "weddings"] {
            c.apply(Op::SetKeyword { path: path.into(), info: Some(KeywordInfo::default()) }).unwrap();
        }
        let op = c.purge_unused_keywords_ops();
        c.apply(op).unwrap();
        assert_eq!(listed(&c).iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["travel", "travel|italy"]);
        assert_eq!(tree_paths(&c.keyword_tree()), [("travel".to_string(), 1), ("travel|italy".to_string(), 1)]);
    }

    /// Renaming matches names whatever their case, also where a letter's case changes its length
    /// (ẞ is three bytes, ß two): the levels below are kept whole, never cut mid-letter.
    #[test]
    fn renaming_keeps_the_levels_below_whatever_the_case() {
        let (mut c, ids) = lib(&[&["straße|nord"], &["ßa|ü"]]);
        let op = c.rename_keyword_ops("STRAẞE", "Road").unwrap();
        c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["Road|nord"]);
        let op = c.rename_keyword_ops("ẞA", "Weg").unwrap();
        c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[1]), ["Weg|ü"]);
    }

    fn exported(c: &Catalog, keywords: &[&str]) -> (Vec<String>, Vec<String>) {
        let e = c.export_keywords(&keywords.iter().map(|k| k.to_string()).collect::<Vec<_>>());
        (e.flat, e.hierarchical)
    }

    /// By default a keyword is exported as its name together with the keywords containing it (flat,
    /// `dc:subject`), and as its full path (`lr:hierarchicalSubject`); each name once.
    #[test]
    fn a_keyword_exports_its_name_its_parents_and_its_path() {
        let c = Catalog::new();
        let (flat, paths) = exported(&c, &["travel|Italy|Rome", "Travel|Spain", "beach"]);
        assert_eq!(flat, ["Rome", "Italy", "travel", "Spain", "beach"]);
        assert_eq!(paths, ["travel|Italy|Rome", "Travel|Spain", "beach"]);
    }

    /// A keyword not included on export is left out, path and all; a parent left out (a keyword that
    /// only organizes others) isn't exported as a containing keyword, but its children are.
    #[test]
    fn keywords_left_out_of_export_stay_out() {
        let mut c = Catalog::new();
        let out = KeywordInfo { include_on_export: false, ..KeywordInfo::default() };
        c.apply(Op::SetKeyword { path: "Places".into(), info: Some(out.clone()) }).unwrap();
        c.apply(Op::SetKeyword { path: "draft".into(), info: Some(out) }).unwrap();
        let (flat, paths) = exported(&c, &["Places|Lisbon", "draft"]);
        assert_eq!(flat, ["Lisbon"]);
        assert_eq!(paths, ["Places|Lisbon"]);
    }

    /// Without "export containing keywords" only the keyword's own name goes out flat; synonyms go
    /// out with it unless "export synonyms" is off.
    #[test]
    fn containing_keywords_and_synonyms_follow_their_options() {
        let mut c = Catalog::new();
        let alone = KeywordInfo { export_containing: false, ..with_synonyms(&["Lisboa"]) };
        c.apply(Op::SetKeyword { path: "Places|Lisbon".into(), info: Some(alone) }).unwrap();
        let quiet = KeywordInfo { export_synonyms: false, ..with_synonyms(&["marriage"]) };
        c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(quiet) }).unwrap();
        let (flat, _) = exported(&c, &["Places|Lisbon", "Events|Weddings"]);
        assert_eq!(flat, ["Lisbon", "Lisboa", "Weddings", "Events"]);
    }

    /// Names match whatever the case of any letter, not only ASCII ones: changing only the case of
    /// "Ärzte" is a rename, not a clash with itself, and moving it where it is changes nothing.
    #[test]
    fn names_match_whatever_the_case_of_any_letter() {
        let (mut c, ids) = lib(&[&["Ärzte|Wien"]]);
        let op = c.edit_keyword_ops("ärzte", "ÄRZTE", KeywordInfo::default()).unwrap();
        c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["ÄRZTE|Wien"]);
        assert_eq!(c.move_keyword_ops("ärzte|wien", Some("ärzte"), false).unwrap(), Op::Batch { ops: vec![] });
        let op = c.rename_keyword_ops("ärzte", "Ärzte").unwrap();
        c.apply(op).unwrap();
        assert_eq!(kws(&c, ids[0]), ["Ärzte|Wien"]);
        assert!(c.create_keyword_ops("ärzte", KeywordInfo::default(), &[]).is_err(), "exists");
    }

    /// A keyword that is only the parent of a listed one ("Events" of "Events|Weddings") is a
    /// keyword like any other: it is in the tree, so it can be found, moved and renamed.
    #[test]
    fn the_parent_of_a_listed_keyword_is_a_keyword() {
        let mut c = Catalog::new();
        c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
        assert!(c.has_keyword("events"));
        assert_eq!(c.keyword_path("EVENTS").as_deref(), Some("Events"));
        let op = c.edit_keyword_ops("events", "Occasions", KeywordInfo::default()).unwrap();
        c.apply(op).unwrap();
        assert_eq!(tree_paths(&c.keyword_tree()), [("Occasions".to_string(), 0), ("Occasions|Weddings".to_string(), 0)]);
    }

    /// Photos in Recently Deleted don't count: a keyword only they have isn't in the tree, so it
    /// can be created again and is purged as unused, and its spelling there isn't the library's.
    #[test]
    fn deleted_photos_dont_hold_keywords() {
        let (mut c, ids) = lib(&[&["Weddings"], &["travel"]]);
        c.apply(Op::SetDeleted { id: ids[0], deleted: true }).unwrap();
        assert!(!c.has_keyword("weddings"));
        assert!(c.create_keyword_ops("weddings", KeywordInfo::default(), &[]).is_ok());
        c.apply(Op::SetKeyword { path: "Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
        let op = c.purge_unused_keywords_ops();
        c.apply(op).unwrap();
        assert!(listed(&c).is_empty(), "only a deleted photo has it: unused");
    }

    /// Merging several keywords moves each keyword under them by the closest one, whatever the
    /// order they are given in, and every listing lands where its keyword does: one landing where
    /// another is about to leave isn't merged into it.
    #[test]
    fn merging_overlapping_keywords_keeps_every_listing() {
        let (mut c, ids) = lib(&[&["a|b|c"], &["a|c"]]);
        let person = KeywordInfo { person: true, ..KeywordInfo::default() };
        c.apply(Op::SetKeyword { path: "a|b|c".into(), info: Some(person.clone()) }).unwrap();
        c.apply(Op::SetKeyword { path: "a|c".into(), info: Some(with_synonyms(&["sea"])) }).unwrap();
        let op = c.merge_keywords_ops(&["a|b".into(), "a|c".into()], "a").unwrap();
        c.apply(op).unwrap();
        assert_eq!((kws(&c, ids[0]), kws(&c, ids[1])), (vec!["a|c".to_string()], vec!["a".to_string()]));
        assert_eq!(listed(&c), [("a".to_string(), with_synonyms(&["sea"])), ("a|c".to_string(), person)]);
        // the order of the keywords merged doesn't matter
        let merged = |from: [&str; 2]| {
            let (mut c, ids) = lib(&[&["a|b|c", "a|d"]]);
            let op = c.merge_keywords_ops(&from.map(String::from), "x").unwrap();
            c.apply(op).unwrap();
            kws(&c, ids[0])
        };
        assert_eq!(merged(["a", "a|b"]), merged(["a|b", "a"]));
        assert_eq!(merged(["a", "a|b"]), ["x|c", "x|d"]);
    }

    /// One keyword at most is where new keywords go (Put New Keywords Inside This Keyword). Making
    /// another one so is one undo step; the mark moves with its keyword (also into a merge) and
    /// goes with it.
    #[test]
    fn one_keyword_is_where_new_keywords_go() {
        let (mut c, _) = lib(&[&["Events|Weddings"], &["Travel"]]);
        let op = c.set_default_parent_ops(Some("events")).unwrap();
        let undo = c.apply(op).unwrap();
        assert_eq!(c.default_keyword_parent().as_deref(), Some("Events"));
        let op = c.set_default_parent_ops(Some("travel")).unwrap();
        c.apply(op).unwrap();
        assert_eq!(c.default_keyword_parent().as_deref(), Some("Travel"), "only one");
        assert!(c.set_default_parent_ops(Some("lisbon")).is_err());
        let op = c.rename_keyword_ops("travel", "Trips").unwrap();
        c.apply(op).unwrap();
        assert_eq!(c.default_keyword_parent().as_deref(), Some("Trips"));
        let op = c.merge_keywords_ops(&["trips".into()], "Events").unwrap();
        c.apply(op).unwrap();
        assert_eq!(c.default_keyword_parent().as_deref(), Some("Events"), "into the merge");
        let op = c.delete_keyword_ops("events").unwrap();
        c.apply(op).unwrap();
        assert_eq!(c.default_keyword_parent(), None);
        let op = c.set_default_parent_ops(None).unwrap();
        assert_eq!(op, Op::Batch { ops: vec![] }, "none to clear");
        c.apply(undo).unwrap();
        assert_eq!(c.default_keyword_parent(), None, "undo of the first: none was");
    }

    /// A file's keywords as a photo carries them: the paths of `lr:hierarchicalSubject`, and the
    /// names of `dc:subject` that aren't levels of those paths (an exported `travel|Italy` comes
    /// back as one keyword, not three). Without paths, the names as they are.
    #[test]
    fn a_files_keywords_keep_their_hierarchy() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(from_file(&v(&["Rome", "Italy", "travel", "beach"]), &v(&["travel|Italy|Rome"])), ["travel|Italy|Rome", "beach"]);
        assert_eq!(from_file(&v(&["ROME"]), &v(&["travel|Rome", " travel | rome "])), ["travel|Rome"], "each once, cleaned");
        assert_eq!(from_file(&v(&["beach", " sea "]), &[]), ["beach", "sea"]);
        assert_eq!(from_file(&v(&["a|b"]), &[]), ["a|b"], "LightCraft's own sidecars write paths flat");
    }

    /// A new keyword inside one the library has takes the library's spelling of it: creating
    /// "TRAVEL|rome" next to "travel|italy" doesn't respell "travel".
    #[test]
    fn a_new_keyword_keeps_the_spelling_of_its_parents() {
        let (mut c, _) = lib(&[&["travel|italy"]]);
        let op = c.create_keyword_ops("TRAVEL|Rome", KeywordInfo::default(), &[]).unwrap();
        c.apply(op).unwrap();
        assert_eq!(listed(&c).iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["travel|Rome"]);
        assert_eq!(tree_paths(&c.keyword_tree())[0].0, "travel");
    }

    /// An edit that changes nothing is nothing: no listing, no undo step.
    #[test]
    fn an_edit_that_changes_nothing_is_nothing() {
        let (mut c, _) = lib(&[&["beach"]]);
        assert_eq!(c.edit_keyword_ops("beach", "beach", KeywordInfo::default()).unwrap(), Op::Batch { ops: vec![] });
        c.apply(Op::SetKeyword { path: "beach".into(), info: Some(with_synonyms(&["shore"])) }).unwrap();
        assert_eq!(c.edit_keyword_ops("BEACH", "beach", with_synonyms(&["shore"])).unwrap(), Op::Batch { ops: vec![] });
        // renamed with the default attributes: the photos change, nothing is listed
        let (mut c, _) = lib(&[&["beach"]]);
        let op = c.edit_keyword_ops("beach", "Shore", KeywordInfo::default()).unwrap();
        c.apply(op).unwrap();
        assert!(listed(&c).is_empty());
    }

    /// Typing suggests keywords no photo has yet too (created in the Keyword List).
    #[test]
    fn suggestions_include_keywords_without_photos() {
        let (mut c, _) = lib(&[&["beach"]]);
        c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
        assert_eq!(c.keyword_suggestions(&[], "wed", 5), ["Events|Weddings"]);
        assert!(!c.keyword_suggestions(&["events|weddings".into()], "wed", 5).contains(&"Events|Weddings".to_string()), "not one it has");
    }

    /// A new name typed for a photo goes inside the default parent; a keyword the library has, and
    /// a path typed whole, don't.
    #[test]
    fn typed_new_keywords_go_inside_the_default_parent() {
        let (mut c, _) = lib(&[&["beach"], &["Events"]]);
        assert_eq!(c.typed_keyword(" Weddings "), "Weddings", "no default parent");
        let op = c.set_default_parent_ops(Some("events")).unwrap();
        c.apply(op).unwrap();
        assert_eq!(c.typed_keyword("Weddings"), "Events|Weddings");
        assert_eq!(c.typed_keyword("BEACH"), "BEACH", "the library has it");
        assert_eq!(c.typed_keyword("Places|Lisbon"), "Places|Lisbon");
    }

    /// The keyword list as a text file, as Lightroom Classic writes one: a keyword a line, the
    /// keywords inside it indented one tab more, a synonym in braces on a line of its own under its
    /// keyword, a keyword left out of export in square brackets; by name.
    #[test]
    fn the_keyword_list_is_written_as_tab_indented_text() {
        let (mut c, _) = lib(&[&["Places|Lisbon", "beach"]]);
        c.apply(Op::SetKeyword { path: "Places".into(), info: Some(KeywordInfo { include_on_export: false, ..KeywordInfo::default() }) }).unwrap();
        c.apply(Op::SetKeyword { path: "Places|Lisbon".into(), info: Some(with_synonyms(&["Lisboa"])) }).unwrap();
        c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(KeywordInfo::default()) }).unwrap();
        assert_eq!(c.keyword_list_text().text, "beach\nEvents\n\tWeddings\n[Places]\n\tLisbon\n\t\t{Lisboa}\n");
    }

    /// Reading a list gives each keyword's path and attributes: what was written comes back, and a
    /// file from elsewhere (Windows line ends, a byte-order mark, blank lines) reads too.
    #[test]
    fn a_keyword_list_reads_back() {
        let read = parse_keyword_list("\u{feff}beach\r\n\r\nEvents\r\n\tWeddings\r\n\t\t{marriage}\r\n[Places]\r\n\tLisbon\r\n").unwrap();
        let out = KeywordInfo { include_on_export: false, ..KeywordInfo::default() };
        assert_eq!(
            read,
            [
                ("beach".to_string(), KeywordInfo::default()),
                ("Events".to_string(), KeywordInfo::default()),
                ("Events|Weddings".to_string(), with_synonyms(&["marriage"])),
                ("Places".to_string(), out),
                ("Places|Lisbon".to_string(), KeywordInfo::default()),
            ]
        );
    }

    /// Photo Supreme's Formatted Vocabulary File is the same format: plain text with Windows line
    /// ends and no byte-order mark, a top-level category in square brackets (an organizing keyword,
    /// left out of export), keywords a tab further in at each level.
    #[test]
    fn a_photo_supreme_vocabulary_file_reads() {
        let file = "[Places]\r\n\tPortugal\r\n\t\tLisbon\r\n\t\t\tAlfama\r\n\t\tPorto\r\n\tSpain\r\n\t\tSeville\r\n";
        let read = parse_keyword_list(file).unwrap();
        let paths: Vec<&str> = read.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            ["Places", "Portugal", "Portugal|Lisbon", "Portugal|Lisbon|Alfama", "Portugal|Porto", "Spain", "Spain|Seville"]
                .map(|p| if p == "Places" { p.to_string() } else { format!("Places|{p}") })
        );
        assert!(!read[0].1.include_on_export, "the category organizes: it isn't exported");
        assert!(read[1..].iter().all(|(_, i)| i.include_on_export));
    }

    /// A list that can't be read says which line and why, and nothing of it is taken.
    #[test]
    fn a_bad_keyword_list_says_which_line() {
        let err = |text: &str| parse_keyword_list(text).unwrap_err().to_string();
        assert!(err("Events\n\t\tWeddings\n").contains("line 2"), "two levels down at once");
        assert!(err("{marriage}\n").contains("line 1"), "a synonym of nothing");
        assert!(err("Events\nTravel|Lisbon\n").contains("line 2"), "“|” separates levels here");
        let deep: String = (0..70).map(|i| format!("{}k{i}\n", "\t".repeat(i))).collect();
        assert!(err(&deep).contains("line 65"), "a file is input: 64 levels at most");
    }

    /// Files from elsewhere read as they were meant or say why not: classic Mac line ends (a lone
    /// carriage return) are lines; indenting with spaces, which would flatten the hierarchy, is
    /// refused at its line; so are control characters in a name and "|" in a synonym.
    #[test]
    fn a_keyword_list_from_elsewhere_reads_or_says_why_not() {
        let paths = |text: &str| parse_keyword_list(text).unwrap().into_iter().map(|(p, _)| p).collect::<Vec<_>>();
        assert_eq!(paths("Events\r\tWeddings\rPlaces\r"), ["Events", "Events|Weddings", "Places"]);
        let err = |text: &str| parse_keyword_list(text).unwrap_err().to_string();
        assert!(err("Events\n    Weddings\n").contains("line 2"), "space indentation");
        assert!(err("Events\n\t Weddings\n").contains("line 2"), "a space after the tabs");
        assert!(err("Events\nx\u{0}y\n").contains("line 2"), "a control character");
        assert!(err("Events\n\t{sea|shore}\n").contains("line 2"), "“|” in a synonym");
    }

    /// A keyword repeated in another case is one keyword, and the keywords inside either take the
    /// first spelling.
    #[test]
    fn a_repeated_keyword_keeps_its_first_spelling() {
        let read = parse_keyword_list("Events\n\tWeddings\nevents\n\tBirthdays\n").unwrap();
        assert_eq!(read.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["Events", "Events|Weddings", "Events|Birthdays"]);
    }

    /// What the list can't hold is left out and named, so the file always reads back: a name the
    /// format would read as something else (in brackets or braces), one with a line break, a
    /// synonym with one, a keyword nested deeper than a list goes; and the keywords inside one left
    /// out go with it.
    #[test]
    fn an_exported_keyword_list_always_reads_back() {
        let deep = (0..=MAX_LEVELS).map(|i| format!("level {i}")).collect::<Vec<_>>().join("|");
        let (mut c, _) = lib(&[&["Travel|[draft]", "{todo}|Lisbon", "Weddings", "Line\nbreak", deep.as_str()]]);
        c.apply(Op::SetKeyword { path: "Weddings".into(), info: Some(with_synonyms(&["marriage", "two\nlines"])) }).unwrap();
        let KeywordListFile { text, unwritable, .. } = c.keyword_list_text();
        let read = parse_keyword_list(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert!(read.iter().any(|(p, _)| p == "Travel"));
        assert!(read.iter().all(|(p, _)| !p.contains("draft") && !p.contains("todo") && !p.contains("Line")), "{text}");
        let weddings = read.iter().find(|(p, _)| p == "Weddings").map(|(_, i)| i.synonyms.clone());
        assert_eq!(weddings, Some(vec!["marriage".to_string()]));
        let deepest = read.iter().map(|(p, _)| p.split(SEP).count()).max();
        assert_eq!(deepest, Some(MAX_LEVELS));
        assert!(unwritable.contains(&"Travel|[draft]".to_string()), "{unwritable:?}");
        assert!(unwritable.contains(&"{todo}".to_string()), "{unwritable:?}");
        assert!(unwritable.contains(&"Line\nbreak".to_string()), "{unwritable:?}");
        assert!(unwritable.contains(&"Weddings {two\nlines}".to_string()), "{unwritable:?}");
        assert!(unwritable.iter().any(|u| u.ends_with(&format!("level {MAX_LEVELS}"))), "{unwritable:?}");
    }

    /// The list leaves out only what it must, and names only what's lost: a name starting with a
    /// byte-order mark would lose it, but a synonym in braces reads back, and a synonym that's empty
    /// or another's in a different case is no loss. It also says which keywords it wrote.
    #[test]
    fn an_exported_keyword_list_names_only_what_it_loses() {
        let (mut c, _) = lib(&[&["\u{feff}Albums", "{draft", "Weddings"]]);
        c.apply(Op::SetKeyword { path: "Weddings".into(), info: Some(with_synonyms(&["Marriage", "marriage", " ", "{vows}"])) }).unwrap();
        let out = c.keyword_list_text();
        let read = parse_keyword_list(&out.text).unwrap_or_else(|e| panic!("{e}\n{}", out.text));
        assert_eq!(out.unwritable, ["\u{feff}Albums"]);
        assert_eq!(out.written, ["Weddings", "{draft"]);
        assert_eq!(read.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["Weddings", "{draft"]);
        let weddings = read.iter().find(|(p, _)| p == "Weddings").map(|(_, i)| i.synonyms.clone());
        assert_eq!(weddings, Some(vec!["Marriage".to_string(), "{vows}".to_string()]));
    }

    /// A big list reads and imports at once, against a big library too: 40,000 keywords used to take
    /// half a minute to read, and checking 5,000 new ones against 20,000 photos over a minute, with
    /// the app frozen.
    #[test]
    fn a_big_keyword_list_imports_quickly() {
        let mut text = String::new();
        for g in 0..400 {
            text.push_str(&format!("group {g}\n"));
            for k in 0..99 {
                text.push_str(&format!("\tkeyword {g}-{k}\n"));
            }
        }
        let t0 = std::time::Instant::now();
        let read = parse_keyword_list(&text).unwrap();
        assert_eq!(read.len(), 40_000);
        let mut c = Catalog::new();
        let mut ops = Vec::new();
        for i in 0..20_000u32 {
            let id = c.alloc_photo_id();
            let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a.jpg", "JPEG", 3, 2, "2026-01-01");
            p.meta.keywords = (0..5).map(|k| format!("library {}|tag {}", k, (i + k) % 3000)).collect();
            ops.push(Op::AddPhoto { photo: Box::new(p) });
        }
        c.apply(Op::Batch { ops }).unwrap();
        let (op, added, _) = c.import_keywords_ops(&read[..5_000]);
        c.apply(op).unwrap();
        assert_eq!(added, 5_000);
        let took = t0.elapsed();
        assert!(took < std::time::Duration::from_secs(10), "{took:?}");
    }

    /// Importing a list adds the keywords the library doesn't have, with their attributes, and gives
    /// those it has the list's synonyms (their own attributes stay), in one undo step.
    #[test]
    fn importing_a_keyword_list_adds_what_is_missing() {
        let (mut c, _) = lib(&[&["Events|Weddings"]]);
        c.apply(Op::SetKeyword { path: "Events|Weddings".into(), info: Some(KeywordInfo { person: true, ..with_synonyms(&["marriage"]) }) }).unwrap();
        let before = c.to_snapshot();
        let read = parse_keyword_list("events\n\tweddings\n\t\t{nuptials}\n\tBirthdays\n[Drafts]\n").unwrap();
        let (op, added, updated) = c.import_keywords_ops(&read);
        assert_eq!((added, updated), (2, 1));
        let undo = c.apply(op).unwrap();
        assert_eq!(c.keyword_info("Events|Weddings"), Some(&KeywordInfo { person: true, ..with_synonyms(&["marriage", "nuptials"]) }));
        assert!(c.has_keyword("Events|Birthdays"));
        assert_eq!(
            listed(&c).iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            ["Drafts", "Events|Birthdays", "Events|Weddings"],
            "in the library's spelling"
        );
        assert!(!c.keyword_info("Drafts").unwrap().include_on_export);
        c.apply(undo).unwrap();
        assert_eq!(c.to_snapshot(), before);
    }

    #[test]
    fn suggestions_rank_co_occurrence_and_prefixes() {
        let (c, _) = lib(&[&["dog", "park"], &["dog", "park", "ball"], &["dog", "beach"], &["cat"], &["cat"], &["cat"], &["parade"], &["eagle"]]);
        // co-occurring with `dog` first (park twice), then the most used
        let s = c.keyword_suggestions(&["dog".into()], "", 3);
        assert_eq!(s, ["park", "ball", "beach"]);
        let s = c.keyword_suggestions(&[], "", 2);
        assert_eq!(s, ["cat", "dog"]);
        let s = c.keyword_suggestions(&["park".into()], "pa", 5);
        assert_eq!(s, ["parade"]);
        let s = c.keyword_suggestions(&[], "E", 5);
        assert_eq!(s, ["eagle", "beach", "parade"], "prefix matches before substring matches");
    }
}
