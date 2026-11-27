//! Reading lensfun-format database files with `craft-xml`, within this crate's limits.

pub(crate) use craft_xml::El as Node;

use crate::Error;

/// Most nodes (elements and their text) one document may hold.
const MAX_NODES: usize = 1_000_000;
/// Deepest element nesting (lensfun files nest four deep).
const MAX_DEPTH: usize = 32;

/// The document's root element. Malformed markup is an error (strict); an undefined entity is kept as the text
/// `&name;`.
pub(crate) fn parse(s: &str) -> Result<Node, Error> {
    let limits = craft_xml::Limits { max_depth: MAX_DEPTH, max_nodes: MAX_NODES, ..craft_xml::Limits::DEFAULT };
    let options = craft_xml::Options {
        strict: true,
        unknown_entity: craft_xml::UnknownEntity::Literal,
        drop_blank_text: true,
        ..craft_xml::Options::default()
    }
    .with_limits(limits);
    options.parse(s.as_bytes()).map_err(|e| if e.is_limit() { Error::TooLarge } else { Error::Xml(e.to_string()) })
}

/// A finite number in attribute `k`.
pub(crate) fn num(n: &Node, k: &str) -> Option<f64> {
    n.attr(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())
}

/// The trimmed text of the first child `name`, when not empty.
pub(crate) fn kid_text(n: &Node, name: &str) -> Option<String> {
    n.child(name).map(|c| c.text().trim().to_string()).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_and_padding_decode_and_unknown_entities_stay() {
        let root = parse("<lens maker='C&#0000000097;non &amp;'>&#x0000041; &unknownreference;</lens>").unwrap();
        assert_eq!(root.attr("maker"), Some("Canon &"));
        assert_eq!(root.text(), "A &unknownreference;");
    }

    #[test]
    fn limits_are_too_large() {
        assert!(matches!(parse(&"<a>".repeat(MAX_DEPTH + 1)), Err(Error::TooLarge)));
        assert!(matches!(parse(&format!("<a>{}</a>", "<b/>".repeat(MAX_NODES))), Err(Error::TooLarge)));
    }
}
