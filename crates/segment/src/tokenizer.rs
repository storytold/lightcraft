//! The CLIP byte-pair-encoding tokenizer (lower-cased, `</w>` word ends), padded to the
//! 32-token context SAM 3's text encoder takes.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the project contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (the CLIP tokenizer used by `models/sam3`), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use std::collections::HashMap;
use std::path::Path;

use crate::{Error, Result};

pub const CONTEXT: usize = 32;
const BOS: u32 = 49406;
const EOS: u32 = 49407;

pub struct Tokenizer {
    vocab: HashMap<String, u32>,
    ranks: HashMap<(String, String), usize>,
    byte_char: [char; 256],
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

impl Tokenizer {
    /// From `vocab.json` and `merges.txt` in `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).map_err(|e| Error::Model(format!("{name}: {e}")));
        let vocab: HashMap<String, u32> = serde_json::from_str(&read("vocab.json")?).map_err(|e| Error::Model(format!("vocab.json: {e}")))?;
        let merges = read("merges.txt")?;
        Ok(Self::new(vocab, &merges))
    }

    pub fn new(vocab: HashMap<String, u32>, merges: &str) -> Self {
        let ranks = merges
            .lines()
            .filter(|l| !l.starts_with("#version") && !l.trim().is_empty())
            .filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())))
            .enumerate()
            .map(|(i, p)| (p, i))
            .collect();
        Self { vocab, ranks, byte_char: bytes_to_unicode() }
    }

    /// Token ids (start, words, end, padding) and which positions are real tokens.
    pub fn encode(&self, text: &str) -> (Vec<u32>, Vec<bool>) {
        let mut ids = vec![BOS];
        for word in pre_tokenize(&text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()) {
            let mapped: String = word.bytes().map(|b| self.byte_char.get(usize::from(b)).copied().unwrap_or('?')).collect();
            for piece in self.bpe(&mapped) {
                ids.push(self.vocab.get(&piece).copied().unwrap_or(EOS));
            }
        }
        ids.truncate(CONTEXT - 1);
        ids.push(EOS);
        let n = ids.len();
        ids.resize(CONTEXT, EOS);
        let valid = (0..CONTEXT).map(|i| i < n).collect();
        (ids, valid)
    }

    fn bpe(&self, word: &str) -> Vec<String> {
        let mut syms: Vec<String> = word.chars().map(String::from).collect();
        if let Some(last) = syms.last_mut() {
            last.push_str("</w>");
        }
        loop {
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
            if syms.len() == 1 {
                break;
            }
        }
        syms
    }
}

/// CLIP's pre-tokenizer: contractions, letter runs, single digits, and runs of other
/// non-space characters.
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

    #[test]
    fn splits_like_clip() {
        assert_eq!(pre_tokenize("a dog's toy, 25 cats!"), ["a", "dog", "'s", "toy", ",", "2", "5", "cats", "!"]);
    }

    #[test]
    fn merges_by_rank() {
        let vocab: HashMap<String, u32> =
            [("c", 1), ("a", 2), ("t</w>", 3), ("ca", 4), ("cat</w>", 5)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let t = Tokenizer::new(vocab, "#version: 0.2\nc a\nca t</w>\n");
        let (ids, valid) = t.encode("  CAT ");
        assert_eq!(&ids[..3], &[BOS, 5, EOS]);
        assert_eq!(ids.len(), CONTEXT);
        assert_eq!(valid.iter().filter(|v| **v).count(), 3);
    }
}
