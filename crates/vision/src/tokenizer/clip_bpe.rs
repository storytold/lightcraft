//! CLIP's byte-level byte-pair-encoding tokenizer (`vocab.json` + `merges.txt`): whitespace collapsed and lower-cased,
//! split into contractions, letter runs, single digits and runs of other symbols, each byte mapped to a printable
//! character (GPT-2's reversible map), words merged by rank with a `</w>` word end. Ids are `<|startoftext|>`, the
//! words' pieces, `<|endoftext|>`, then `<|endoftext|>` padding to the context; a text longer than the context is cut
//! to keep the two markers. A piece the vocabulary lacks becomes `<|endoftext|>` (the reference's unknown token).
//!
//! Used by the CLIP text tower ([`crate::clip`], 77 tokens) and by SAM 3's text encoder (`lightcraft-segment`, 32).
//! Not done: Unicode NFC normalisation (the reference composes accents first; typed text is NFC already), and letters
//! are Rust's `char::is_alphabetic` (the reference's `\p{L}` leaves out a few combining vowel signs it includes).
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from the Python CLIP
//! tokenizer of Hugging Face Transformers (`models/clip/tokenization_clip.py`, as used by `models/clip` and
//! `models/sam3`), Copyright The HuggingFace Team, OpenAI and Meta Platforms, Inc.; translated to Rust and
//! restructured. See NOTICE.

use std::collections::HashMap;
use std::path::Path;

use crate::{Error, Result};

pub const VOCAB_FILE: &str = "vocab.json";
pub const MERGES_FILE: &str = "merges.txt";
/// Largest `vocab.json` / `merges.txt` read (CLIP's are 0.9 MB and 0.5 MB).
pub const MAX_FILE_BYTES: u64 = 16 << 20;
/// Most vocabulary entries and merges accepted (CLIP has 49,408 and 48,894).
pub const MAX_ENTRIES: usize = 512 * 1024;
/// The start and end markers' ids in CLIP's vocabulary (used when a vocabulary does not list them).
pub const BOS: u32 = 49406;
pub const EOS: u32 = 49407;
const BOS_TEXT: &str = "<|startoftext|>";
const EOS_TEXT: &str = "<|endoftext|>";

pub struct ClipBpe {
    vocab: HashMap<String, u32>,
    ranks: HashMap<(String, String), usize>,
    byte_char: [char; 256],
    bos: u32,
    eos: u32,
    /// One more than the largest id.
    size: usize,
}

/// GPT-2's reversible byte → printable character map.
fn bytes_to_unicode() -> [char; 256] {
    let mut map = ['\0'; 256];
    let mut n = 0u32;
    for b in 0..=255u8 {
        let printable = (b'!'..=b'~').contains(&b) || (0xA1..=0xAC).contains(&b) || (0xAE..=0xFF).contains(&b);
        let c = if printable {
            u32::from(b)
        } else {
            n += 1;
            255 + n
        };
        if let Some(slot) = map.get_mut(usize::from(b)) {
            *slot = char::from_u32(c).unwrap_or('?');
        }
    }
    map
}

fn read(dir: &Path, name: &str) -> Result<String> {
    let path = dir.join(name);
    let len = std::fs::metadata(&path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?.len();
    if len > MAX_FILE_BYTES {
        return Err(Error::Model(format!("{name} is larger than {MAX_FILE_BYTES} bytes")));
    }
    std::fs::read_to_string(&path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))
}

impl ClipBpe {
    /// From `vocab.json` and `merges.txt` in `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        let vocab: HashMap<String, u32> = serde_json::from_str(&read(dir, VOCAB_FILE)?).map_err(|e| Error::Model(format!("{VOCAB_FILE}: {e}")))?;
        Self::new(vocab, &read(dir, MERGES_FILE)?)
    }

    pub fn new(vocab: HashMap<String, u32>, merges: &str) -> Result<Self> {
        if vocab.is_empty() || vocab.len() > MAX_ENTRIES {
            return Err(Error::Model(format!("{VOCAB_FILE}: {} entries (1 to {MAX_ENTRIES})", vocab.len())));
        }
        let mut ranks = HashMap::new();
        for (i, l) in merges.lines().filter(|l| !l.starts_with("#version") && !l.trim().is_empty()).enumerate() {
            if i >= MAX_ENTRIES {
                return Err(Error::Model(format!("{MERGES_FILE}: more than {MAX_ENTRIES} merges")));
            }
            if let Some((a, b)) = l.split_once(' ') {
                ranks.insert((a.to_string(), b.to_string()), i);
            }
        }
        let bos = vocab.get(BOS_TEXT).copied().unwrap_or(BOS);
        let eos = vocab.get(EOS_TEXT).copied().unwrap_or(EOS);
        let size = vocab.values().copied().chain([bos, eos]).max().map_or(0, |m| m as usize + 1);
        Ok(Self { vocab, ranks, byte_char: bytes_to_unicode(), bos, eos, size })
    }

    /// One more than the largest id it can give (the token table must have at least this many rows).
    pub fn vocab_size(&self) -> usize {
        self.size
    }

    pub fn eos(&self) -> u32 {
        self.eos
    }

    /// Token ids padded to `context` (at least 2), and how many are real (start and end markers included).
    pub fn encode(&self, text: &str, context: usize) -> (Vec<u32>, usize) {
        let context = context.max(2);
        let mut ids = vec![self.bos];
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        // the markers written out are the markers themselves (added tokens), the rest is split into words
        for (i, part) in text.split(EOS_TEXT).enumerate() {
            if i > 0 {
                ids.push(self.eos);
            }
            for (j, part) in part.split(BOS_TEXT).enumerate() {
                if j > 0 {
                    ids.push(self.bos);
                }
                for word in pre_tokenize(part) {
                    let mapped: String = word.bytes().map(|b| self.byte_char.get(usize::from(b)).copied().unwrap_or('?')).collect();
                    for piece in self.bpe(&mapped) {
                        ids.push(self.vocab.get(&piece).copied().unwrap_or(self.eos));
                    }
                }
            }
            if ids.len() >= context {
                break;
            }
        }
        ids.truncate(context - 1);
        ids.push(self.eos);
        let n = ids.len();
        ids.resize(context, self.eos);
        (ids, n)
    }

    fn bpe(&self, word: &str) -> Vec<String> {
        let mut syms: Vec<String> = word.chars().map(String::from).collect();
        if let Some(last) = syms.last_mut() {
            last.push_str("</w>");
        }
        while syms.len() > 1 {
            let best = syms
                .windows(2)
                .enumerate()
                .filter_map(|(i, w)| match w {
                    [a, b] => self.ranks.get(&(a.clone(), b.clone())).map(|r| (*r, i)),
                    _ => None,
                })
                .min();
            let Some((_, i)) = best else { break };
            let (Some(a), Some(b)) = (syms.get(i).cloned(), syms.get(i + 1).cloned()) else { break };
            let merged = a.clone() + &b;
            // merge every occurrence of the pair, left to right
            let mut out = Vec::with_capacity(syms.len());
            let mut j = 0;
            while j < syms.len() {
                if j + 1 < syms.len() && syms.get(j) == Some(&a) && syms.get(j + 1) == Some(&b) {
                    out.push(merged.clone());
                    j += 2;
                } else {
                    out.extend(syms.get(j).cloned());
                    j += 1;
                }
            }
            syms = out;
        }
        syms
    }
}

/// CLIP's pre-tokenizer: contractions, letter runs, single digits, and runs of other non-space characters.
fn pre_tokenize(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '\'' {
            let rest: String = chars.get(i + 1..(i + 3).min(chars.len())).map(|s| s.iter().collect()).unwrap_or_default();
            if let Some(k) = ["re", "ve", "ll"]
                .iter()
                .find(|k| rest.starts_with(**k))
                .map(|k| k.len())
                .or_else(|| rest.chars().next().filter(|c| matches!(c, 's' | 't' | 'm' | 'd')).map(|_| 1))
            {
                out.push(chars.get(i..i + 1 + k).map(|s| s.iter().collect()).unwrap_or_default());
                i += 1 + k;
                continue;
            }
        }
        let start = i;
        if c.is_alphabetic() {
            while chars.get(i).is_some_and(|c| c.is_alphabetic()) {
                i += 1;
            }
        } else if c.is_numeric() {
            i += 1;
        } else {
            while chars.get(i).is_some_and(|c| !c.is_whitespace() && !c.is_alphabetic() && !c.is_numeric()) {
                i += 1;
            }
        }
        out.push(chars.get(start..i).map(|s| s.iter().collect()).unwrap_or_default());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> ClipBpe {
        let vocab: HashMap<String, u32> = [("c", 1), ("a", 2), ("t</w>", 3), ("ca", 4), ("cat</w>", 5), (BOS_TEXT, 10), (EOS_TEXT, 11)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        ClipBpe::new(vocab, "#version: 0.2\nc a\nca t</w>\n").unwrap()
    }

    #[test]
    fn splits_like_clip() {
        assert_eq!(pre_tokenize("a dog's toy, 25 cats!"), ["a", "dog", "'s", "toy", ",", "2", "5", "cats", "!"]);
        assert_eq!(pre_tokenize("  "), Vec::<String>::new());
    }

    #[test]
    fn merges_by_rank_pads_and_truncates() {
        let t = tiny();
        let (ids, n) = t.encode("  CAT ", 8);
        assert_eq!(ids, [10, 5, 11, 11, 11, 11, 11, 11]);
        assert_eq!((n, t.vocab_size(), t.eos()), (3, 12, 11));
        // unknown pieces are the end marker; markers written out are markers
        assert_eq!(t.encode("x", 4).0, [10, 11, 11, 11]);
        assert_eq!(t.encode("cat <|endoftext|> cat", 6), (vec![10, 5, 11, 5, 11, 11], 5));
        // cut to keep both markers
        let (ids, n) = t.encode(&"cat ".repeat(100), 5);
        assert_eq!((ids, n), (vec![10, 5, 5, 5, 11], 5));
        assert_eq!(t.encode("", 2), (vec![10, 11], 2));
        assert_eq!(t.encode("cat", 0).0.len(), 2, "a context below 2 is 2");
    }

    #[test]
    fn odd_vocabularies_are_refused_or_bounded() {
        assert!(ClipBpe::new(HashMap::new(), "").is_err());
        let one: HashMap<String, u32> = [("a</w>".to_string(), 7)].into_iter().collect();
        let t = ClipBpe::new(one, "garbage line without a space\n\n").unwrap();
        // the markers it lacks are CLIP's ids
        assert_eq!(t.vocab_size(), EOS as usize + 1);
        assert_eq!(t.encode("a", 4).0, [BOS, 7, EOS, EOS]);
        let dir = std::env::temp_dir().join(format!("lc-vision-clipbpe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(ClipBpe::load(&dir).is_err(), "missing files");
        std::fs::write(dir.join(VOCAB_FILE), "[1, 2]").unwrap();
        std::fs::write(dir.join(MERGES_FILE), "").unwrap();
        assert!(ClipBpe::load(&dir).is_err(), "not a vocabulary");
        std::fs::write(dir.join(VOCAB_FILE), r#"{"a": 1}"#).unwrap();
        assert!(ClipBpe::load(&dir).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
