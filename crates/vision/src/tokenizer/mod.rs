//! Text tokenizers for the text towers.
//!
//! - [`sentencepiece`]: SentencePiece-style BPE read from a Hugging Face `tokenizer.json`
//!   (Gemma's, which SigLIP 2 uses): byte fallback, unknown-token fusing, added tokens,
//!   end-of-sequence suffix and padding to the model's context.
//! - [`clip_bpe`]: CLIP's byte-level BPE from `vocab.json` + `merges.txt` (CLIP's text tower, and SAM 3's text
//!   encoder in `lightcraft-segment`).

pub mod clip_bpe;
pub mod sentencepiece;

pub use clip_bpe::ClipBpe;
pub use sentencepiece::SentencePiece;

/// Longest text tokenized (bytes); longer input is cut at a character boundary first. Far more
/// than the 64-token context needs.
pub const MAX_TEXT_BYTES: usize = 4096;

/// `s` cut to at most `max` bytes, at a character boundary.
pub fn clip_text(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_respects_character_boundaries() {
        assert_eq!(clip_text("héllo", 2), "h");
        assert_eq!(clip_text("héllo", 3), "hé");
        assert_eq!(clip_text("abc", 10), "abc");
        assert_eq!(clip_text("🦂", 3), "");
    }
}
