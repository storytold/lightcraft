//! Caption templates: text with `{field}` tokens replaced by a photo's metadata.
//!
//! Known fields are whatever the caller puts in the photo's field map (the engine fills `title`,
//! `caption`, `filename`, `date`, `camera`, `lens`, `copyright`, `creator`, `keywords`, `rating`,
//! `iso`, `aperture`, `shutter`, `focal`). An unknown token expands to nothing; `{{` and `}}` are
//! literal braces. A template that expands to only whitespace is empty.

use std::collections::BTreeMap;

/// The tokens offered in the Image Info panel's menu.
pub const KNOWN: &[&str] =
    &["title", "caption", "filename", "date", "camera", "lens", "copyright", "creator", "keywords", "rating", "iso", "aperture", "shutter", "focal"];

/// Expands `{field}` tokens in `template` from `fields`.
pub fn expand(template: &str, fields: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                let mut closed = false;
                for n in chars.by_ref() {
                    if n == '}' {
                        closed = true;
                        break;
                    }
                    name.push(n);
                }
                if closed {
                    if let Some(v) = fields.get(name.trim()) {
                        out.push_str(v);
                    }
                } else {
                    out.push('{');
                    out.push_str(&name);
                }
            }
            _ => out.push(c),
        }
    }
    if out.trim().is_empty() { String::new() } else { out.trim().to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tokens() {
        let mut f = BTreeMap::new();
        f.insert("title".to_string(), "Dunes".to_string());
        f.insert("camera".to_string(), "X100".to_string());
        assert_eq!(expand("{title} — {camera}", &f), "Dunes — X100");
        assert_eq!(expand("{nope}", &f), "");
        assert_eq!(expand("{{literal}} {title", &f), "{literal} {title");
        assert_eq!(expand("  {missing}  ", &f), "");
    }
}
