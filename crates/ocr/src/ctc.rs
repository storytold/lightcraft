//! The recogniser's alphabet and greedy CTC decoding (PaddleOCR's `CTCLabelDecode`).

use std::io::Read;
use std::path::Path;

use crate::{Error, Result};

/// Largest alphabet file read (PP-OCRv6's is 206 kB).
const MAX_BYTES: u64 = 8 << 20;
/// Most symbols an alphabet may have.
const MAX_SYMBOLS: usize = 100_000;

/// The recogniser's symbols: index 0 is the CTC blank, the rest characters (one or more `char`s each).
#[derive(Clone, Debug, PartialEq)]
pub struct Alphabet {
    symbols: Vec<String>,
}

impl Alphabet {
    /// `symbols[0]` is the blank.
    pub fn new(symbols: Vec<String>) -> Result<Self> {
        if symbols.len() < 2 || symbols.len() > MAX_SYMBOLS {
            return Err(Error::Model(format!("an alphabet of {} symbols can't be used", symbols.len())));
        }
        Ok(Self { symbols })
    }

    /// The `character_list` of a Hugging Face `preprocessor_config.json` (PaddlePaddle's safetensors exports).
    pub fn from_preprocessor_config(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        let mut buf = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut buf).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        if buf.len() as u64 > MAX_BYTES {
            return Err(Error::Model(format!("{}: too large for an alphabet", path.display())));
        }
        let v: serde_json::Value = serde_json::from_slice(&buf).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        let list =
            v.get("character_list").and_then(|l| l.as_array()).ok_or_else(|| Error::Model(format!("{}: no character_list", path.display())))?;
        let symbols = list
            .iter()
            .map(|s| s.as_str().map(str::to_string).ok_or_else(|| Error::Model(format!("{}: a character is not a string", path.display()))))
            .collect::<Result<Vec<_>>>()?;
        Self::new(symbols)
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Greedy decoding of `t × classes` probabilities: the best class per step, repeats merged, blanks dropped. The text
    /// and its confidence (the mean probability of the kept steps; 0 when nothing was read).
    pub fn decode(&self, probs: &[f32], classes: usize) -> (String, f32) {
        if classes == 0 {
            return (String::new(), 0.0);
        }
        let (mut text, mut sum, mut n, mut prev) = (String::new(), 0f64, 0usize, usize::MAX);
        for step in probs.chunks_exact(classes) {
            let (best, p) = step.iter().copied().enumerate().fold((0, f32::MIN), |(bi, bp), (i, p)| if p > bp { (i, p) } else { (bi, bp) });
            if best != 0
                && best != prev
                && let Some(s) = self.symbols.get(best)
            {
                text.push_str(s);
                sum += f64::from(p);
                n += 1;
            }
            prev = best;
        }
        (text, if n == 0 { 0.0 } else { (sum / n as f64) as f32 })
    }
}

#[cfg(test)]
mod tests {
    use super::Alphabet;

    #[test]
    fn decodes_merging_repeats_and_dropping_blanks() {
        let a = Alphabet::new(["blank", "a", "b", "日"].map(String::from).to_vec()).unwrap();
        // steps: a a blank a b 日 → "aab日" (the blank separates the two a's)
        let one = |i: usize, p: f32| {
            let mut v = vec![0.0; 4];
            v[i] = p;
            v
        };
        let probs: Vec<f32> = [one(1, 0.9), one(1, 0.8), one(0, 0.9), one(1, 0.7), one(2, 0.6), one(3, 1.0)].concat();
        let (text, score) = a.decode(&probs, 4);
        assert_eq!(text, "aab日");
        assert!((score - (0.9 + 0.7 + 0.6 + 1.0) / 4.0).abs() < 1e-6);
        assert_eq!(a.decode(&one(0, 1.0), 4), (String::new(), 0.0));
        assert_eq!(a.decode(&probs, 0), (String::new(), 0.0));
        assert!(Alphabet::new(vec!["blank".into()]).is_err());
    }

    #[test]
    fn reads_the_preprocessor_config() {
        let d = std::env::temp_dir().join(format!("lightcraft-ocr-alpha-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("preprocessor_config.json");
        std::fs::write(&p, r#"{"character_list": ["blank", "x", " "]}"#).unwrap();
        assert_eq!(Alphabet::from_preprocessor_config(&p).unwrap().len(), 3);
        std::fs::write(&p, r#"{"character_list": ["blank", 3]}"#).unwrap();
        assert!(Alphabet::from_preprocessor_config(&p).is_err());
        std::fs::write(&p, "not json").unwrap();
        assert!(Alphabet::from_preprocessor_config(&p).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
