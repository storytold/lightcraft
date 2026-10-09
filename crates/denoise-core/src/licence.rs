use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Commercial {
    Yes,
    /// Research or personal use only. Never bundled or redistributed by the app.
    No,
    /// The licence or the training data is unclear: the user is told so before installing.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Licence {
    /// SPDX id or a short name ("MIT", "Apache-2.0", "InsightFace non-commercial research").
    pub name: String,
    pub commercial: Commercial,
    pub url: Option<String>,
    /// Plain-language terms shown before the model is enabled.
    pub notice: String,
}

impl Default for Licence {
    fn default() -> Self {
        Self { name: String::new(), commercial: Commercial::Unknown, url: None, notice: String::new() }
    }
}

pub fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    let first_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    first_ok && id.len() <= 64 && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}
