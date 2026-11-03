//! A small, bounded XML tree reader for lensfun-format database files.

use quick_xml::events::Event;

use crate::Error;

const MAX_NODES: usize = 500_000;
const MAX_DEPTH: usize = 32;

#[derive(Debug, Default)]
pub(crate) struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    pub fn num(&self, k: &str) -> Option<f64> {
        self.attr(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())
    }
    pub fn kids<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    pub fn kid(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    pub fn kid_text(&self, name: &str) -> Option<&str> {
        self.kid(name).map(|n| n.text.trim()).filter(|t| !t.is_empty())
    }
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let n = name.strip_prefix('#')?;
            let v = match n.strip_prefix('x') {
                Some(h) if h.bytes().all(|b| b.is_ascii_hexdigit()) => u32::from_str_radix(h, 16).ok()?,
                Some(_) => return None,
                None if n.bytes().all(|b| b.is_ascii_digit()) => n.parse().ok()?,
                None => return None,
            };
            char::from_u32(v)?
        }
    })
}

/// Where the `;` of a reference starting at `rest` (its `&`) ends it, looked for only as far as
/// the longest reference reaches (`&#x10FFFF;`): a text of many `&` with a distant `;` is read
/// once, not once per `&`.
fn entity_end(rest: &str) -> Option<usize> {
    rest.char_indices().take(11).find(|&(_, c)| c == ';').map(|(i, _)| i)
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let tail = rest.get(i..).unwrap_or_default();
        match entity_end(tail).and_then(|j| entity(tail.get(1..j)?).map(|c| (c, j))) {
            Some((c, j)) => {
                out.push(c);
                rest = tail.get(j + 1..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = tail.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

pub(crate) fn parse(s: &str) -> Result<Node, Error> {
    let mut reader = quick_xml::Reader::from_str(s);
    let mut stack = vec![Node { name: "#root".into(), ..Default::default() }];
    let mut count = 0usize;
    loop {
        let ev = reader.read_event().map_err(|e| Error::Xml(e.to_string()))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                count += 1;
                if count > MAX_NODES || stack.len() > MAX_DEPTH {
                    return Err(Error::TooLarge);
                }
                let attrs = e
                    .attributes()
                    .with_checks(false)
                    .flatten()
                    .map(|a| (String::from_utf8_lossy(a.key.as_ref()).into_owned(), unescape(&String::from_utf8_lossy(&a.value))))
                    .collect();
                let node = Node { name: String::from_utf8_lossy(e.name().as_ref()).into_owned(), attrs, ..Default::default() };
                if matches!(ev, Event::Empty(_)) {
                    if let Some(p) = stack.last_mut() {
                        p.children.push(node);
                    }
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => close(&mut stack),
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.decode().map_err(|e| Error::Xml(e.to_string()))?);
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(r) => {
                if let (Some(top), Some(c)) = (stack.last_mut(), entity(&String::from_utf8_lossy(&r))) {
                    top.text.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    while stack.len() > 1 {
        close(&mut stack);
    }
    stack.pop().ok_or(Error::Xml("empty document".into()))
}

fn close(stack: &mut Vec<Node>) {
    if stack.len() > 1
        && let Some(n) = stack.pop()
        && let Some(p) = stack.last_mut()
    {
        p.children.push(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_ampersands_before_one_semicolon_are_read_once() {
        // 200 000 `&` then a `;`: each `&` once searched the whole rest for the `;`
        let hostile = format!("{};&amp;", "&".repeat(200_000));
        assert_eq!(unescape(&hostile), format!("{};&", "&".repeat(200_000)));
        assert_eq!(unescape("&lt;a&gt; &#x10FFFF; &unknownreference;"), "<a> \u{10FFFF} &unknownreference;");
    }

    #[test]
    fn entity_rejects_signed_numeric_references() {
        assert_eq!(entity("#+65"), None);
        assert_eq!(entity("#x+41"), None);
        assert_eq!(entity("#65"), Some('A'));
        assert_eq!(entity("#x41"), Some('A'));
    }
}
