//! SentencePiece-style BPE from a Hugging Face `tokenizer.json` (the Gemma tokenizer SigLIP 2
//! uses), in pure Rust.
//!
//! The pipeline, as the reference library runs it for this file format:
//! 1. **Added tokens** (`<eos>`, `<unused0>`, `\n`, HTML tags, …) are split out of the raw text
//!    first, leftmost-longest match, and map straight to their ids.
//! 2. The other pieces are **normalised** (Gemma: every space becomes `▁`) and pre-split (Gemma's
//!    split on spaces is a no-op after that).
//! 3. **BPE**: each character becomes its vocabulary token, or its UTF-8 bytes as `<0xNN>`
//!    tokens (byte fallback), or `<unk>` (consecutive unknowns fused); then adjacent pairs are
//!    merged, lowest merge rank first, leftmost first on ties, until no listed pair remains.
//! 4. The **post-processor** template adds its special tokens (Gemma for SigLIP 2: `<eos>` after
//!    the text); [`SentencePiece::encode_padded`] truncates and pads to the context.
//!
//! Only the normaliser / pre-tokeniser / post-processor shapes listed below are accepted; any
//! other `tokenizer.json` is refused with an error rather than tokenized differently.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::{MAX_TEXT_BYTES, clip_text};
use crate::{Error, Result};

/// Largest `tokenizer.json` read.
pub const MAX_FILE_BYTES: u64 = 128 << 20;
pub const MAX_VOCAB: usize = 512 * 1024;
pub const MAX_MERGES: usize = 4 << 20;
pub const MAX_ADDED: usize = 8192;
const MAX_TOKEN_BYTES: usize = 1024;

#[derive(Deserialize)]
#[serde(untagged)]
enum RawMerge {
    Pair(String, String),
    Joined(String),
}

#[derive(Deserialize)]
struct RawModel {
    #[serde(rename = "type")]
    kind: String,
    vocab: HashMap<String, u32>,
    merges: Vec<RawMerge>,
    #[serde(default)]
    unk_token: Option<String>,
    #[serde(default)]
    fuse_unk: bool,
    #[serde(default)]
    byte_fallback: bool,
    #[serde(default)]
    continuing_subword_prefix: Option<String>,
    #[serde(default)]
    end_of_word_suffix: Option<String>,
    #[serde(default)]
    dropout: Option<f64>,
}

#[derive(Deserialize)]
struct RawAdded {
    id: u32,
    content: String,
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    added_tokens: Vec<RawAdded>,
    #[serde(default)]
    normalizer: Value,
    #[serde(default)]
    pre_tokenizer: Value,
    #[serde(default)]
    post_processor: Value,
    #[serde(default)]
    padding: Value,
    model: RawModel,
}

#[derive(Clone, Debug, PartialEq)]
enum Norm {
    Replace(String, String),
    Prepend(String),
}

/// A loaded tokenizer.
pub struct SentencePiece {
    vocab: HashMap<String, u32>,
    /// `(left, right)` → `(rank, merged id)`.
    merges: HashMap<(u32, u32), (u32, u32)>,
    bytes: [Option<u32>; 256],
    unk: Option<u32>,
    fuse_unk: bool,
    byte_fallback: bool,
    /// Added tokens by first character, longest first.
    added: HashMap<char, Vec<(String, u32)>>,
    norms: Vec<Norm>,
    /// Pre-split the normalised text after each occurrence of this (`MergedWithPrevious`).
    split: Option<String>,
    prefix: Vec<u32>,
    suffix: Vec<u32>,
    pad: u32,
    vocab_size: usize,
}

fn bad<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::Model(format!("tokenizer.json: {}", why.into())))
}

fn string_pattern(v: &Value) -> Option<&str> {
    v.get("pattern").and_then(|p| p.get("String")).and_then(Value::as_str)
}

fn parse_norms(v: &Value, out: &mut Vec<Norm>, depth: usize) -> Result<()> {
    if depth > 4 {
        return bad("normalizer nested too deeply");
    }
    match v.get("type").and_then(Value::as_str) {
        None if v.is_null() => Ok(()),
        Some("Replace") => {
            let (Some(from), Some(to)) = (string_pattern(v), v.get("content").and_then(Value::as_str)) else {
                return bad("Replace normalizer without a string pattern");
            };
            if from.is_empty() {
                return bad("Replace normalizer with an empty pattern");
            }
            out.push(Norm::Replace(from.into(), to.into()));
            Ok(())
        }
        Some("Prepend") => {
            out.push(Norm::Prepend(v.get("prepend").and_then(Value::as_str).unwrap_or_default().into()));
            Ok(())
        }
        Some("Sequence") => {
            for n in v.get("normalizers").and_then(Value::as_array).into_iter().flatten() {
                parse_norms(n, out, depth + 1)?;
            }
            Ok(())
        }
        other => bad(format!("unsupported normalizer {other:?}")),
    }
}

fn parse_split(v: &Value) -> Result<Option<String>> {
    match v.get("type").and_then(Value::as_str) {
        None if v.is_null() => Ok(None),
        Some("Split") => {
            let behaviour = v.get("behavior").and_then(Value::as_str);
            let invert = v.get("invert").and_then(Value::as_bool).unwrap_or(false);
            match (string_pattern(v), behaviour, invert) {
                (Some(p), Some("MergedWithPrevious"), false) if !p.is_empty() => Ok(Some(p.into())),
                _ => bad("unsupported Split pre-tokenizer"),
            }
        }
        other => bad(format!("unsupported pre-tokenizer {other:?}")),
    }
}

/// The ids the post-processor puts before and after the text.
fn parse_template(v: &Value, ids: &dyn Fn(&str) -> Option<Vec<u32>>) -> Result<(Vec<u32>, Vec<u32>)> {
    match v.get("type").and_then(Value::as_str) {
        None if v.is_null() => Ok((Vec::new(), Vec::new())),
        Some("TemplateProcessing") => {
            let (mut prefix, mut suffix, mut seen_seq) = (Vec::new(), Vec::new(), false);
            for item in v.get("single").and_then(Value::as_array).into_iter().flatten() {
                if item.get("Sequence").is_some() {
                    if seen_seq {
                        return bad("template with two sequences");
                    }
                    seen_seq = true;
                } else if let Some(name) = item.get("SpecialToken").and_then(|s| s.get("id")).and_then(Value::as_str) {
                    let special = v.get("special_tokens").and_then(|s| s.get(name)).and_then(|s| s.get("ids")).and_then(Value::as_array);
                    let found: Option<Vec<u32>> = match special {
                        Some(a) => a.iter().map(|x| x.as_u64().and_then(|x| u32::try_from(x).ok())).collect(),
                        None => ids(name),
                    };
                    let Some(found) = found else { return bad(format!("template token `{name}` is unknown")) };
                    if found.len() > 8 {
                        return bad("template token with too many ids");
                    }
                    if seen_seq { suffix.extend(found) } else { prefix.extend(found) }
                } else {
                    return bad("unsupported template item");
                }
            }
            if !seen_seq {
                return bad("template without the text sequence");
            }
            Ok((prefix, suffix))
        }
        other => bad(format!("unsupported post-processor {other:?}")),
    }
}

/// A symbol of the word being merged (a doubly linked list over the initial pieces).
#[derive(Clone, Copy)]
struct Sym {
    id: u32,
    prev: Option<usize>,
    next: Option<usize>,
    /// Zero once merged into its left neighbour.
    alive: bool,
}

impl SentencePiece {
    /// From `tokenizer.json` in `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("tokenizer.json");
        let len = std::fs::metadata(&path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?.len();
        if len > MAX_FILE_BYTES {
            return bad(format!("{len} bytes is more than the {MAX_FILE_BYTES} accepted"));
        }
        let bytes = std::fs::read(&path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        Self::from_json(&bytes)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return bad("too large");
        }
        let raw: RawFile = serde_json::from_slice(bytes).map_err(|e| Error::Model(format!("tokenizer.json: {e}")))?;
        let m = raw.model;
        if m.kind != "BPE" {
            return bad(format!("model type `{}` is not BPE", m.kind.chars().take(20).collect::<String>()));
        }
        if m.continuing_subword_prefix.as_deref().is_some_and(|s| !s.is_empty())
            || m.end_of_word_suffix.as_deref().is_some_and(|s| !s.is_empty())
            || m.dropout.is_some_and(|d| d > 0.0)
        {
            return bad("subword prefixes, word suffixes and dropout are not supported");
        }
        if m.vocab.is_empty() || m.vocab.len() > MAX_VOCAB || m.merges.len() > MAX_MERGES || raw.added_tokens.len() > MAX_ADDED {
            return bad("vocabulary, merges or added tokens out of bounds");
        }
        let mut vocab = m.vocab;
        let mut vocab_size = 0usize;
        for (tok, &id) in &vocab {
            if tok.len() > MAX_TOKEN_BYTES || id as usize >= MAX_VOCAB {
                return bad("a vocabulary entry is out of bounds");
            }
            vocab_size = vocab_size.max(id as usize + 1);
        }
        let mut added: HashMap<char, Vec<(String, u32)>> = HashMap::new();
        for a in raw.added_tokens {
            if a.content.is_empty() || a.content.len() > MAX_TOKEN_BYTES || a.id as usize >= MAX_VOCAB {
                return bad("an added token is out of bounds");
            }
            vocab_size = vocab_size.max(a.id as usize + 1);
            vocab.entry(a.content.clone()).or_insert(a.id);
            if let Some(c) = a.content.chars().next() {
                added.entry(c).or_default().push((a.content, a.id));
            }
        }
        for list in added.values_mut() {
            list.sort_by_key(|t| std::cmp::Reverse(t.0.len()));
        }
        let mut merges = HashMap::with_capacity(m.merges.len());
        for (rank, mg) in m.merges.into_iter().enumerate() {
            let (a, b) = match mg {
                RawMerge::Pair(a, b) => (a, b),
                RawMerge::Joined(s) => match s.split_once(' ') {
                    Some((a, b)) => (a.to_string(), b.to_string()),
                    None => return bad("a merge is not a pair"),
                },
            };
            let (Some(&ia), Some(&ib)) = (vocab.get(&a), vocab.get(&b)) else {
                return bad("a merge uses a token outside the vocabulary");
            };
            let Some(&merged) = vocab.get(&format!("{a}{b}")) else {
                return bad("a merge makes a token outside the vocabulary");
            };
            merges.entry((ia, ib)).or_insert((rank as u32, merged));
        }
        let mut bytes_ids = [None; 256];
        for (b, slot) in bytes_ids.iter_mut().enumerate() {
            *slot = vocab.get(&format!("<0x{b:02X}>")).copied();
        }
        let unk = match &m.unk_token {
            Some(u) => Some(*vocab.get(u).ok_or_else(|| Error::Model("tokenizer.json: unknown unk_token".into()))?),
            None => None,
        };
        let mut norms = Vec::new();
        parse_norms(&raw.normalizer, &mut norms, 0)?;
        let split = parse_split(&raw.pre_tokenizer)?;
        let lookup = |name: &str| vocab.get(name).map(|id| vec![*id]);
        let (prefix, suffix) = parse_template(&raw.post_processor, &lookup)?;
        let pad = raw
            .padding
            .get("pad_id")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
            .or_else(|| vocab.get("<pad>").copied())
            .unwrap_or(0);
        if prefix.iter().chain(&suffix).chain([&pad]).any(|&id| id as usize >= vocab_size.max(1)) {
            return bad("special token ids outside the vocabulary");
        }
        Ok(Self {
            vocab,
            merges,
            bytes: bytes_ids,
            unk,
            fuse_unk: m.fuse_unk,
            byte_fallback: m.byte_fallback,
            added,
            norms,
            split,
            prefix,
            suffix,
            pad,
            vocab_size,
        })
    }

    /// One more than the largest token id.
    pub fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    pub fn pad_id(&self) -> u32 {
        self.pad
    }

    /// The token ids of `text` (no special tokens, no padding). Text beyond
    /// [`MAX_TEXT_BYTES`] is ignored.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let text = clip_text(text, MAX_TEXT_BYTES);
        let mut out = Vec::new();
        let mut rest_start = 0;
        let mut i = 0;
        // added tokens: leftmost-longest match on the raw text
        while i < text.len() {
            let Some(c) = text.get(i..).and_then(|s| s.chars().next()) else { break };
            let hit = self.added.get(&c).and_then(|list| list.iter().find(|(tok, _)| text.get(i..).is_some_and(|s| s.starts_with(tok.as_str()))));
            if let Some((tok, id)) = hit {
                self.encode_piece(text.get(rest_start..i).unwrap_or(""), &mut out);
                out.push(*id);
                i += tok.len();
                rest_start = i;
            } else {
                i += c.len_utf8();
            }
        }
        self.encode_piece(text.get(rest_start..).unwrap_or(""), &mut out);
        out
    }

    /// Model input for `text`: the template's special tokens around the text, truncated so the
    /// whole fits `context`, then padded to `context` with the pad id.
    pub fn encode_padded(&self, text: &str, context: usize) -> Vec<u32> {
        let mut ids = self.encode(text);
        let room = context.saturating_sub(self.prefix.len() + self.suffix.len());
        ids.truncate(room);
        let mut out = Vec::with_capacity(context);
        out.extend(&self.prefix);
        out.extend(ids);
        out.extend(&self.suffix);
        out.truncate(context);
        out.resize(context, self.pad);
        out
    }

    fn encode_piece(&self, piece: &str, out: &mut Vec<u32>) {
        if piece.is_empty() {
            return;
        }
        let mut s = piece.to_string();
        for n in &self.norms {
            match n {
                Norm::Replace(from, to) => s = s.replace(from.as_str(), to),
                Norm::Prepend(p) => s.insert_str(0, p),
            }
        }
        match &self.split {
            Some(sep) => {
                for word in s.split_inclusive(sep.as_str()) {
                    self.bpe(word, out);
                }
            }
            None => self.bpe(&s, out),
        }
    }

    fn bpe(&self, word: &str, out: &mut Vec<u32>) {
        // initial symbols
        let mut syms: Vec<Sym> = Vec::with_capacity(word.len());
        let push = |syms: &mut Vec<Sym>, id: u32| {
            let n = syms.len();
            if let Some(last) = syms.last_mut() {
                last.next = Some(n);
            }
            syms.push(Sym { id, prev: n.checked_sub(1), next: None, alive: true });
        };
        let mut last_unk = false;
        let mut buf = [0u8; 4];
        for c in word.chars() {
            if let Some(&id) = self.vocab.get(c.encode_utf8(&mut buf) as &str) {
                push(&mut syms, id);
                last_unk = false;
                continue;
            }
            if self.byte_fallback {
                let ids: Option<Vec<u32>> = c.encode_utf8(&mut buf).bytes().map(|b| self.bytes.get(usize::from(b)).copied().flatten()).collect();
                if let Some(ids) = ids {
                    ids.into_iter().for_each(|id| push(&mut syms, id));
                    last_unk = false;
                    continue;
                }
            }
            if let Some(unk) = self.unk {
                if !(self.fuse_unk && last_unk) {
                    push(&mut syms, unk);
                }
                last_unk = true;
            }
        }
        // merges, lowest rank first, then leftmost
        let mut heap = BinaryHeap::new();
        for i in 0..syms.len().saturating_sub(1) {
            if let (Some(a), Some(b)) = (syms.get(i), syms.get(i + 1))
                && let Some(&(rank, new)) = self.merges.get(&(a.id, b.id))
            {
                heap.push(Reverse((rank, i, new)));
            }
        }
        while let Some(Reverse((_rank, pos, new))) = heap.pop() {
            let Some(&left) = syms.get(pos) else { continue };
            let Some(next) = left.next.filter(|_| left.alive) else { continue };
            let Some(&right) = syms.get(next) else { continue };
            if self.merges.get(&(left.id, right.id)).map(|m| m.1) != Some(new) {
                continue;
            }
            if let Some(s) = syms.get_mut(pos) {
                s.id = new;
                s.next = right.next;
            }
            if let Some(s) = syms.get_mut(next) {
                s.alive = false;
            }
            if let Some(after) = right.next
                && let Some(s) = syms.get_mut(after)
            {
                s.prev = Some(pos);
            }
            if let Some(p) = left.prev
                && let Some(ps) = syms.get(p)
                && let Some(&(r, n)) = self.merges.get(&(ps.id, new))
            {
                heap.push(Reverse((r, p, n)));
            }
            if let Some(after) = right.next
                && let Some(ns) = syms.get(after)
                && let Some(&(r, n)) = self.merges.get(&(new, ns.id))
            {
                heap.push(Reverse((r, pos, n)));
            }
        }
        out.extend(syms.iter().filter(|s| s.alive).map(|s| s.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny Gemma-shaped tokenizer.json.
    fn tiny() -> SentencePiece {
        let json = r#"{
          "added_tokens": [{"id": 0, "content": "<pad>"}, {"id": 1, "content": "<eos>"}, {"id": 2, "content": "<unk>"},
                           {"id": 3, "content": "\n"}, {"id": 4, "content": "\n\n"}],
          "normalizer": {"type": "Replace", "pattern": {"String": " "}, "content": "▁"},
          "pre_tokenizer": {"type": "Split", "pattern": {"String": " "}, "behavior": "MergedWithPrevious", "invert": false},
          "post_processor": {"type": "TemplateProcessing", "single": [{"Sequence": {"id": "A", "type_id": 0}}, {"SpecialToken": {"id": "<eos>", "type_id": 0}}],
                             "special_tokens": {"<eos>": {"id": "<eos>", "ids": [1], "tokens": ["<eos>"]}}},
          "padding": {"pad_id": 0},
          "model": {"type": "BPE", "unk_token": "<unk>", "fuse_unk": true, "byte_fallback": true,
            "vocab": {"<pad>": 0, "<eos>": 1, "<unk>": 2, "\n": 3, "\n\n": 4, "▁": 5, "a": 6, "b": 7, "▁a": 8, "ab": 9, "▁ab": 10, "aa": 11,
                      "<0xC3>": 12, "<0xA9>": 13},
            "merges": [["▁", "a"], ["a", "b"], ["▁a", "b"], "a a"]}
        }"#;
        SentencePiece::from_json(json.as_bytes()).unwrap()
    }

    #[test]
    fn merges_by_rank_then_leftmost() {
        let t = tiny();
        // "▁ab": (▁,a) has rank 0, then (▁a,b) rank 2
        assert_eq!(t.encode(" ab"), vec![10]);
        // "aaa": (a,a) twice at the same rank: the leftmost wins
        assert_eq!(t.encode("aaa"), vec![11, 6]);
        assert_eq!(t.encode("ab"), vec![9]);
    }

    #[test]
    fn byte_fallback_unknowns_and_added_tokens() {
        let t = tiny();
        // é is not in the vocabulary: its UTF-8 bytes C3 A9
        assert_eq!(t.encode("é"), vec![12, 13]);
        // x and y have no byte tokens: one fused <unk>
        assert_eq!(t.encode("xy"), vec![2]);
        assert_eq!(t.encode("a\n\nb<eos>"), vec![6, 4, 7, 1]);
        assert_eq!(t.encode(""), Vec::<u32>::new());
    }

    #[test]
    fn padding_and_truncation_keep_the_end_token() {
        let t = tiny();
        assert_eq!(t.encode_padded("ab", 4), vec![9, 1, 0, 0]);
        assert_eq!(t.encode_padded("", 3), vec![1, 0, 0]);
        let long = "a b ".repeat(100);
        let ids = t.encode_padded(&long, 6);
        assert_eq!(ids.len(), 6);
        assert_eq!(ids[5], 1);
        assert_eq!(t.encode_padded("ab", 0), Vec::<u32>::new());
        // huge input is clipped, not a crash
        assert!(t.encode(&"é".repeat(1 << 20)).len() <= MAX_TEXT_BYTES);
    }

    #[test]
    fn unsupported_or_broken_files_are_errors() {
        let base: Value = serde_json::from_str(r#"{"model": {"type": "BPE", "vocab": {"a": 0, "b": 1, "ab": 2}, "merges": [["a", "b"]]}}"#).unwrap();
        assert!(SentencePiece::from_json(base.to_string().as_bytes()).is_ok());
        let edits: Vec<Box<dyn Fn(&mut Value)>> = vec![
            Box::new(|v| v["model"]["type"] = "WordPiece".into()),
            Box::new(|v| v["model"]["merges"] = serde_json::json!([["a", "x"]])),
            Box::new(|v| v["model"]["merges"] = serde_json::json!(["ab"])),
            Box::new(|v| v["model"]["merges"] = serde_json::json!([["b", "a"]])),
            Box::new(|v| v["model"]["vocab"] = serde_json::json!({})),
            Box::new(|v| v["model"]["vocab"] = serde_json::json!({"a": 4000000000u64})),
            Box::new(|v| v["model"]["unk_token"] = "<nope>".into()),
            Box::new(|v| v["normalizer"] = serde_json::json!({"type": "NFKC"})),
            Box::new(|v| v["pre_tokenizer"] = serde_json::json!({"type": "ByteLevel"})),
            Box::new(|v| v["post_processor"] = serde_json::json!({"type": "TemplateProcessing", "single": [{"SpecialToken": {"id": "<x>"}}]})),
            Box::new(|v| v["model"]["continuing_subword_prefix"] = "##".into()),
        ];
        for (i, e) in edits.iter().enumerate() {
            let mut v = base.clone();
            e(&mut v);
            assert!(SentencePiece::from_json(v.to_string().as_bytes()).is_err(), "case {i}");
        }
        assert!(SentencePiece::from_json(b"{").is_err());
        assert!(SentencePiece::from_json(b"").is_err());
    }
}
