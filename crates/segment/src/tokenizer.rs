//! The CLIP byte-pair-encoding tokenizer padded to the 32-token context SAM 3's text encoder takes: the shared CLIP
//! tokenizer of `lightcraft-vision` ([`lightcraft_vision::tokenizer::ClipBpe`], also used by the CLIP content model
//! at 77 tokens), plus which positions are real tokens (the encoder masks the padding).

use std::collections::HashMap;
use std::path::Path;

use lightcraft_vision::tokenizer::ClipBpe;

use crate::{Error, Result};

pub const CONTEXT: usize = 32;

pub struct Tokenizer(ClipBpe);

impl Tokenizer {
    /// From `vocab.json` and `merges.txt` in `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        ClipBpe::load(dir).map(Self).map_err(|e| Error::Model(e.to_string()))
    }

    pub fn new(vocab: HashMap<String, u32>, merges: &str) -> Result<Self> {
        ClipBpe::new(vocab, merges).map(Self).map_err(|e| Error::Model(e.to_string()))
    }

    /// Token ids (start, words, end, padding) and which positions are real tokens.
    pub fn encode(&self, text: &str) -> (Vec<u32>, Vec<bool>) {
        let (ids, n) = self.0.encode(text, CONTEXT);
        let valid = (0..CONTEXT).map(|i| i < n).collect();
        (ids, valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_vision::tokenizer::clip_bpe::{BOS, EOS};

    #[test]
    fn merges_by_rank() {
        let vocab: HashMap<String, u32> =
            [("c", 1), ("a", 2), ("t</w>", 3), ("ca", 4), ("cat</w>", 5)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let t = Tokenizer::new(vocab, "#version: 0.2\nc a\nca t</w>\n").unwrap();
        let (ids, valid) = t.encode("  CAT ");
        assert_eq!(&ids[..3], &[BOS, 5, EOS]);
        assert_eq!(ids.len(), CONTEXT);
        assert_eq!(valid.iter().filter(|v| **v).count(), 3);
        // a long prompt keeps its end marker
        let (ids, valid) = t.encode(&"cat ".repeat(50));
        assert_eq!((ids.len(), ids[CONTEXT - 1], valid.iter().all(|v| *v)), (CONTEXT, EOS, true));
    }
}
