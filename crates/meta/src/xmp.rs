#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
//! XMP packets (ISO 16684-1 / Adobe XMP Specification Part 1–2): read and write.
//!
//! Reading builds a small namespace-resolved tree with `quick-xml`, then collects every property of every
//! `rdf:Description` (attribute form, element form, `rdf:Seq`/`rdf:Bag`/`rdf:Alt` arrays, `rdf:parseType="Resource"`
//! and nested-description structs, flattened as `parent/field`). Prefixes are canonicalised by namespace URI, so
//! a packet that binds `http://purl.org/dc/elements/1.1/` to `foo:` still yields `dc:title`.
//!
//! Writing emits the interchange subset in standard namespaces (dc, xmp, photoshop, exif, exifEX, tiff, lr) and
//! The app's full develop settings as an opaque JSON string in `lc:settings`.

use crate::{DateTime, Flash, Gps, Metadata, Orientation, Region, RegionKind, parse_number};
use lcc_geom::{Point, Rect};
use quick_xml::escape::{escape as xml_escape, partial_escape as xml_partial_escape};

/// Whether `c` may appear in an XML 1.0 document (`Char` production): tab, LF, CR, U+0020..U+D7FF,
/// U+E000..U+FFFD and the supplementary planes. Other control characters make the packet unparsable.
fn xml_char(c: char) -> bool {
    matches!(c, '\u{9}' | '\u{A}' | '\u{D}' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

/// Drops characters XML 1.0 forbids (they cannot even be written as references).
fn xml_legal(s: &str) -> std::borrow::Cow<'_, str> {
    if s.chars().all(xml_char) { std::borrow::Cow::Borrowed(s) } else { std::borrow::Cow::Owned(s.chars().filter(|c| xml_char(*c)).collect()) }
}

fn escape(s: &str) -> String {
    xml_escape(xml_legal(s)).into_owned()
}

fn partial_escape(s: &str) -> String {
    xml_partial_escape(xml_legal(s)).into_owned()
}
use quick_xml::events::Event;
use std::collections::BTreeMap;

/// The app's XMP namespace URI (prefix `lc`).
pub use crate::legacy::LC_NS;

const NAMESPACES: &[(&str, &str)] = &[
    ("x", "adobe:ns:meta/"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("xmp", "http://ns.adobe.com/xap/1.0/"),
    ("xmpRights", "http://ns.adobe.com/xap/1.0/rights/"),
    ("photoshop", "http://ns.adobe.com/photoshop/1.0/"),
    ("exif", "http://ns.adobe.com/exif/1.0/"),
    ("exifEX", "http://cipa.jp/exif/1.0/"),
    ("tiff", "http://ns.adobe.com/tiff/1.0/"),
    ("aux", "http://ns.adobe.com/exif/1.0/aux/"),
    ("lr", "http://ns.adobe.com/lightroom/1.0/"),
    ("Iptc4xmpCore", "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"),
    ("lc", LC_NS),
    // Read-only: develop settings written by other raw developers (interchange; see docs/xmp-interop.md).
    ("crs", CRS_NS),
    // Read-only: face/pet/focus regions written by Lightroom, digiKam, Picasa and others (MWG Region
    // Guidelines v2.0). Canonicalising these three lets a packet use any prefix for them.
    ("mwg-rs", "http://www.metadataworkinggroup.com/schemas/regions/"),
    ("stArea", "http://ns.adobe.com/xmp/sType/Area#"),
    ("stDim", "http://ns.adobe.com/xap/1.0/sType/Dimensions#"),
];

/// The camera-raw-settings namespace URI (prefix `crs`), read for interchange only.
pub const CRS_NS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";

/// XMP read failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmpError {
    Xml(String),
    /// Nesting / size limits exceeded.
    TooComplex,
}

impl std::fmt::Display for XmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XmpError::Xml(e) => write!(f, "XMP is not well-formed XML: {e}"),
            XmpError::TooComplex => write!(f, "XMP packet too deeply nested or too large"),
        }
    }
}

impl std::error::Error for XmpError {}

/// Result of [`parse_xmp`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XmpData {
    pub metadata: Metadata,
    /// The opaque `lc:settings` JSON string, if present.
    pub lc_settings: Option<String>,
    /// Every property found, keyed `prefix:name` (struct fields as `prefix:name/prefix:field`); arrays keep
    /// item order, language alternatives put `x-default` first. Foreign namespaces that reuse a recognized
    /// prefix use `Q{hex-encoded namespace URI}local` instead, so they cannot impersonate known properties.
    pub properties: BTreeMap<String, Vec<String>>,
    /// True when flattened properties were omitted after reaching the cumulative key-size limit.
    pub properties_truncated: bool,
    /// Every top-level property with its full structure (arrays of structs such as local
    /// corrections, which [`XmpData::properties`] flattens).
    pub values: BTreeMap<String, XmpValue>,
}

impl XmpData {
    /// The pixel size the packet's regions were drawn on (`mwg-rs:AppliedToDimensions`), unless one of them says how
    /// the photo was turned (`mwg-rs:Rotation`: Lightroom's boxes, which the reader already turns upright). It tells
    /// boxes drawn on the photo as stored from boxes drawn on it upright: see [`crate::upright_regions`].
    pub fn regions_drawn_on(&self) -> Option<(f64, f64)> {
        let regions = self.values.get("mwg-rs:Regions")?;
        let turned = regions.field("mwg-rs:RegionList").is_some_and(|l| l.items().iter().any(|r| r.field("mwg-rs:Rotation").is_some()));
        if turned { None } else { applied_to(regions) }
    }
}

/// A structured XMP value.
#[derive(Clone, Debug, PartialEq)]
pub enum XmpValue {
    Text(String),
    Array(Vec<XmpValue>),
    Struct(BTreeMap<String, XmpValue>),
}

impl XmpValue {
    /// A struct field (`prefix:name`).
    pub fn field(&self, k: &str) -> Option<&XmpValue> {
        match self {
            XmpValue::Struct(m) => m.get(k),
            _ => None,
        }
    }
    pub fn text(&self) -> Option<&str> {
        match self {
            XmpValue::Text(s) => Some(s.trim()),
            XmpValue::Array(a) => a.first().and_then(XmpValue::text),
            XmpValue::Struct(_) => None,
        }
    }
    /// Array items (a lone value is a one-item array).
    pub fn items(&self) -> &[XmpValue] {
        match self {
            XmpValue::Array(a) => a,
            other => std::slice::from_ref(other),
        }
    }
}

fn node_struct(n: &Node) -> XmpValue {
    let mut m = BTreeMap::new();
    for (k, v) in &n.attrs {
        if !is_rdf_meta_attr(k) {
            m.insert(k.clone(), XmpValue::Text(v.clone()));
        }
    }
    for c in &n.children {
        m.insert(c.name.clone(), node_value(c));
    }
    XmpValue::Struct(m)
}

fn node_value(n: &Node) -> XmpValue {
    if let Some(arr) = n.children.iter().find(|c| matches!(c.name.as_str(), "rdf:Seq" | "rdf:Bag" | "rdf:Alt")) {
        return XmpValue::Array(arr.children.iter().filter(|li| li.name == "rdf:li").map(node_value).collect());
    }
    if let Some(d) = n.children.iter().find(|c| c.name == "rdf:Description") {
        return node_struct(d);
    }
    if !n.children.is_empty() || n.attrs.iter().any(|(k, _)| !is_rdf_meta_attr(k)) {
        return node_struct(n);
    }
    XmpValue::Text(n.attr("rdf:resource").map(str::to_string).unwrap_or_else(|| n.text.trim().to_string()))
}

#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
}

impl Node {
    fn attr(&self, n: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str())
    }
}

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 200_000;
// Expanding a long namespace URI on many short names must not amplify a small packet without bound.
pub(crate) const MAX_OPAQUE_NAME_BYTES: usize = 8 * 1024 * 1024;

/// Most text a converter's flattened property keys may add up to. Keys repeat their whole ancestor
/// path (`parent/field`), so a document with a long nested name and a wide child fan-out otherwise
/// retains its prefix once per property: node limits alone cannot bound that duplication.
const MAX_KEY_BYTES: usize = 1 << 20;

/// Most namespace declarations in scope at once. Prefix lookup scans them, so declarations times the names of a
/// start tag is the work; real packets declare a few dozen.
const MAX_NS_DECLS: usize = 512;

/// Refuses a scope stack holding more than [`MAX_NS_DECLS`] declarations (one count per start tag over a stack at most [`MAX_DEPTH`] deep).
pub(crate) fn check_scopes(scopes: &[Vec<(String, String)>]) -> Result<(), XmpError> {
    if scopes.iter().map(Vec::len).sum::<usize>() > MAX_NS_DECLS {
        return Err(XmpError::TooComplex);
    }
    Ok(())
}

/// Names canonicalised by namespace URI (see [`NAMESPACES`]); conflicting foreign prefixes are opaque.
pub(crate) fn canonical_name(qname: &str, scopes: &[Vec<(String, String)>], opaque_budget: &mut usize) -> Result<String, XmpError> {
    canonical(qname, scopes, opaque_budget)
}

fn canonical(qname: &str, scopes: &[Vec<(String, String)>], opaque_budget: &mut usize) -> Result<String, XmpError> {
    let (prefix, local) = qname.split_once(':').unwrap_or(("", qname));
    if prefix == "xml" || prefix == "xmlns" {
        return Ok(qname.to_string());
    }
    let uri = scopes.iter().rev().flat_map(|s| s.iter().rev()).find(|(p, _)| p == prefix).map(|(_, u)| u.as_str());
    Ok(match uri {
        Some(u) => match NAMESPACES.iter().find(|(_, nu)| *nu == u) {
            Some((p, _)) => format!("{p}:{local}"),
            None if u == video_ns() => format!("{VIDEO_PREFIX}:{local}"),
            None if u == plugin_fields_ns() => format!("lcpf:{local}"),
            None if u == plugin_filters_ns() => format!("{FILTERS_PREFIX}:{local}"),
            None if NAMESPACES.iter().any(|(p, _)| *p == prefix) => {
                // A foreign `lc:settings` must never match our `lc:*` ownership rule (nor may a
                // foreign `xmp:Rating` become a rating). Braces cannot occur in an XML QName;
                // hex keeps URI slashes out of the flattened property's `/` path separators.
                let size =
                    u.len().checked_mul(2).and_then(|n| n.checked_add(3)).and_then(|n| n.checked_add(local.len())).ok_or(XmpError::TooComplex)?;
                *opaque_budget = opaque_budget.checked_sub(size).ok_or(XmpError::TooComplex)?;
                let mut key = String::with_capacity(size);
                key.push_str("Q{");
                for byte in u.bytes() {
                    for nibble in [byte >> 4, byte & 15] {
                        key.push(char::from(b"0123456789abcdef".get(usize::from(nibble)).copied().unwrap_or(b'0')));
                    }
                }
                key.push('}');
                key.push_str(local);
                key
            }
            None if prefix.is_empty() => local.to_string(),
            None => qname.to_string(),
        },
        None => qname.to_string(),
    })
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let n = name.strip_prefix('#')?;
            let v = match n.strip_prefix(['x', 'X']) {
                Some(h) if h.bytes().all(|b| b.is_ascii_hexdigit()) => u32::from_str_radix(h, 16).ok()?,
                Some(_) => return None,
                None if n.bytes().all(|b| b.is_ascii_digit()) => n.parse().ok()?,
                None => return None,
            };
            char::from_u32(v).filter(|&c| is_xml_char(c))?
        }
    })
}

/// The XML 1.0 `Char` production (the surrogate range can't occur in a `char`).
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || (c >= '\u{20}' && c != '\u{FFFE}' && c != '\u{FFFF}')
}

/// Unescape attribute values (predefined + numeric references; unknown references kept verbatim).
pub(crate) fn unescape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // the longest reference is `&#x10FFFF;` plus leading zeros: skip the zeros (each byte once, since a
        // miss resumes after the `&`), then search a short window, not the rest of the value per `&`
        let b = rest.as_bytes();
        let digits = match (b.get(1), b.get(2)) {
            (Some(b'#'), Some(b'x' | b'X')) => 3,
            (Some(b'#'), _) => 2,
            _ => 1,
        };
        let zeros = b.iter().skip(digits).take_while(|&&c| c == b'0').count();
        let skip = digits.saturating_add(zeros.saturating_sub(1));
        let end = b.iter().skip(skip).take(12).position(|&c| c == b';').map(|j| j.saturating_add(skip));
        match end.and_then(|j| entity(&rest[1..j]).map(|c| (c, j))) {
            Some((c, j)) => {
                out.push(c);
                rest = rest.get(j.saturating_add(1)..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_tree(s: &str) -> Result<Node, XmpError> {
    let mut reader = quick_xml::Reader::from_str(s);
    reader.config_mut().check_end_names = false;
    let mut stack: Vec<Node> = vec![Node { name: "#root".into(), ..Default::default() }];
    let mut scopes: Vec<Vec<(String, String)>> = vec![vec![("xml".into(), "http://www.w3.org/XML/1998/namespace".into())]];
    let mut count = 0usize;
    let mut opaque_budget = MAX_OPAQUE_NAME_BYTES;
    loop {
        let ev = reader.read_event().map_err(|e| XmpError::Xml(e.to_string()))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                count = count.saturating_add(1);
                if count > MAX_NODES || stack.len() > MAX_DEPTH {
                    return Err(XmpError::TooComplex);
                }
                let mut scope = Vec::new();
                let mut raw_attrs = Vec::new();
                for a in e.attributes().with_checks(false).flatten() {
                    let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
                    let v = unescape_attr(&String::from_utf8_lossy(&a.value));
                    if k == "xmlns" {
                        scope.push((String::new(), v));
                    } else if let Some(p) = k.strip_prefix("xmlns:") {
                        scope.push((p.to_string(), v));
                    } else {
                        raw_attrs.push((k, v));
                    }
                }
                scopes.push(scope);
                check_scopes(&scopes)?;
                let name = canonical(&String::from_utf8_lossy(e.name().as_ref()), &scopes, &mut opaque_budget)?;
                let attrs =
                    raw_attrs.into_iter().map(|(k, v)| Ok((canonical(&k, &scopes, &mut opaque_budget)?, v))).collect::<Result<_, XmpError>>()?;
                let node = Node { name, attrs, ..Default::default() };
                if matches!(ev, Event::Empty(_)) {
                    scopes.pop();
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    }
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) if stack.len() > 1 => {
                scopes.pop();
                if let Some(n) = stack.pop()
                    && let Some(parent) = stack.last_mut()
                {
                    parent.children.push(n);
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.decode().map_err(|e| XmpError::Xml(e.to_string()))?);
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(r) => {
                let name = String::from_utf8_lossy(&r).into_owned();
                if let Some(top) = stack.last_mut() {
                    match entity(&name) {
                        Some(c) => top.text.push(c),
                        None => {
                            top.text.push('&');
                            top.text.push_str(&name);
                            top.text.push(';');
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // close unterminated elements leniently
    while stack.len() > 1 {
        let Some(n) = stack.pop() else { break };
        if let Some(parent) = stack.last_mut() {
            parent.children.push(n);
        }
    }
    stack.pop().ok_or_else(|| XmpError::Xml("no root element".into()))
}

fn is_rdf_meta_attr(k: &str) -> bool {
    matches!(k, "rdf:about" | "rdf:ID" | "rdf:nodeID" | "xml:lang" | "rdf:parseType" | "rdf:resource" | "rdf:datatype") || k.starts_with("xmlns")
}

fn collect_description(d: &Node, prefix: &str, props: &mut BTreeMap<String, Vec<String>>, budget: &mut KeyBudget) {
    for (k, v) in &d.attrs {
        // the budget is checked before the path is built, not after
        if is_rdf_meta_attr(k) {
            continue;
        }
        if budget.left < prefix.len().saturating_add(k.len()) {
            budget.truncated = true;
            continue;
        }
        let key = format!("{prefix}{k}");
        if take_key_bytes(&key, budget) {
            props.entry(key).or_default().push(v.clone());
        }
    }
    for c in &d.children {
        collect_property(c, prefix, props, budget);
    }
}

/// What is left of the flattened-key budget, and whether any property was left out for lack of it.
struct KeyBudget {
    left: usize,
    truncated: bool,
}

/// Charge one flattened key's length to the key budget; `false` when the budget is spent (the
/// property is then left out, like other elements a limit has cut).
fn take_key_bytes(key: &str, budget: &mut KeyBudget) -> bool {
    let n = key.len();
    if budget.left < n {
        budget.truncated = true;
        return false;
    }
    budget.left = budget.left.saturating_sub(n);
    true
}

fn collect_property(p: &Node, prefix: &str, props: &mut BTreeMap<String, Vec<String>>, budget: &mut KeyBudget) {
    // a path that can't fit the budget isn't built (a long ancestor path shared by many children
    // would otherwise be copied for each of them after the budget is spent)
    if budget.left < prefix.len().saturating_add(p.name.len()) {
        budget.truncated = true;
        return;
    }
    let key = format!("{prefix}{}", p.name);
    if let Some(arr) = p.children.iter().find(|c| matches!(c.name.as_str(), "rdf:Seq" | "rdf:Bag" | "rdf:Alt")) {
        let mut items: Vec<(bool, String)> = arr
            .children
            .iter()
            .filter(|li| li.name == "rdf:li")
            .map(|li| {
                (li.attr("xml:lang") == Some("x-default"), li.attr("rdf:resource").map(str::to_string).unwrap_or_else(|| li.text.trim().to_string()))
            })
            .collect();
        if arr.name == "rdf:Alt" {
            items.sort_by_key(|(default, _)| !*default);
        }
        if take_key_bytes(&key, budget) {
            props.entry(key).or_default().extend(items.into_iter().map(|(_, v)| v));
        }
        return;
    }
    let struct_prefix = format!("{key}/");
    // a struct's path is charged like a key before its fields copy it: that work too is bounded
    // by the budget, kept fields or not
    let is_struct = p.attr("rdf:parseType") == Some("Resource") || p.children.iter().any(|c| c.name == "rdf:Description");
    if is_struct && !take_key_bytes(&key, budget) {
        return;
    }
    if p.attr("rdf:parseType") == Some("Resource") {
        for c in &p.children {
            collect_property(c, &struct_prefix, props, budget);
        }
        return;
    }
    if let Some(d) = p.children.iter().find(|c| c.name == "rdf:Description") {
        collect_description(d, &struct_prefix, props, budget);
        return;
    }
    let field_attrs: Vec<&(String, String)> = p.attrs.iter().filter(|(k, _)| !is_rdf_meta_attr(k)).collect();
    if !field_attrs.is_empty() && p.children.is_empty() && p.text.trim().is_empty() {
        for (k, v) in field_attrs {
            let full = format!("{struct_prefix}{k}");
            if take_key_bytes(&full, budget) {
                props.entry(full).or_default().push(v.clone());
            }
        }
        return;
    }
    let v = p.attr("rdf:resource").map(str::to_string).unwrap_or_else(|| p.text.trim().to_string());
    if take_key_bytes(&key, budget) {
        props.entry(key).or_default().push(v);
    }
}

fn find_descriptions<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    if n.name == "rdf:Description" {
        out.push(n);
        return;
    }
    for c in &n.children {
        find_descriptions(c, out);
    }
}

/// XMP GPS coordinate `DDD,MM,SSk` or `DDD,MM.mmk` (k = N/S/E/W) → signed degrees.
fn parse_gps_coord(s: &str) -> Option<f64> {
    let s = s.trim();
    let dir = s.chars().last()?;
    let body = s.get(..s.len().saturating_sub(dir.len_utf8()))?;
    let parts: Vec<f64> = body.split(',').map(|p| p.trim().parse::<f64>().ok()).collect::<Option<_>>()?;
    let v = match parts.as_slice() {
        [d] => *d,
        [d, m] if (0.0..60.0).contains(m) => d + m / 60.0,
        [d, m, sec] if (0.0..60.0).contains(m) && (0.0..60.0).contains(sec) => d + m / 60.0 + sec / 3600.0,
        _ => return None,
    };
    let v = match dir.to_ascii_uppercase() {
        'N' | 'E' => v,
        'S' | 'W' => -v,
        _ => return None,
    };
    v.is_finite().then_some(v)
}

/// `mwg-rs:Regions` → every region in its `RegionList`, converted to the full-image normalized frame.
/// Malformed or out-of-range entries are skipped rather than rejecting the whole list: one bad region
/// (from Lightroom, digiKam, Picasa, or a hand-edited file) shouldn't hide the rest.
fn parse_regions(regions: &XmpValue) -> Vec<Region> {
    // `AppliedToDimensions` documents the pixel size regions were authored against. Here it's only consulted
    // for the rare region whose own Area is in pixel units instead of MWG's normalized default below.
    let px_dims = applied_to(regions);
    let Some(list) = regions.field("mwg-rs:RegionList") else { return Vec::new() };
    list.items().iter().filter_map(|item| parse_one_region(item, px_dims)).collect()
}

/// `mwg-rs:AppliedToDimensions` of a `mwg-rs:Regions` struct: (width, height) in pixels, both positive.
fn applied_to(regions: &XmpValue) -> Option<(f64, f64)> {
    regions.field("mwg-rs:AppliedToDimensions").and_then(|d| {
        let w = d.field("stDim:w").and_then(XmpValue::text).and_then(parse_number)?;
        let h = d.field("stDim:h").and_then(XmpValue::text).and_then(parse_number)?;
        (w > 0.0 && h > 0.0).then_some((w, h))
    })
}

fn parse_one_region(item: &XmpValue, px_dims: Option<(f64, f64)>) -> Option<Region> {
    let area = item.field("mwg-rs:Area")?;
    let num_attr = |k: &str| area.field(k).and_then(XmpValue::text).and_then(parse_number);
    // stArea:x/y are the region's CENTER, not its top-left corner (MWG Region Guidelines §Area).
    let (cx, cy, w, h) = (num_attr("stArea:x")?, num_attr("stArea:y")?, num_attr("stArea:w")?, num_attr("stArea:h")?);
    let finite_and_sane = [cx, cy, w, h].iter().all(|v| v.is_finite() && v.abs() <= 1.0e6);
    if !finite_and_sane || w <= 0.0 || h <= 0.0 {
        return None;
    }
    // `stArea:unit` is "normalized" (a fraction of AppliedToDimensions) in every writer we know of, and
    // that needs no cross-referencing at all. Only a region that explicitly says "pixel" gets divided
    // down, and only if AppliedToDimensions is there to divide by; otherwise it's dropped rather than
    // stored as a nonsense fraction.
    let is_pixels = area.field("stArea:unit").and_then(XmpValue::text).is_some_and(|u| u.eq_ignore_ascii_case("pixel"));
    let (cx, cy, w, h) = match (is_pixels, px_dims) {
        (false, _) => (cx, cy, w, h),
        (true, Some((dw, dh))) => (cx / dw, cy / dh, w / dw, h / dh),
        (true, None) => return None,
    };
    // Lightroom Classic stores the box of a photo with Exif orientation 3 / 6 / 8 in the sensor's own
    // frame and writes the orientation as the region's `mwg-rs:Rotation` (π for 3, −π/2 for 6, +π/2 for
    // 8; 0 for 1) while `AppliedToDimensions` names the upright size. Turn it into the upright frame.
    // MWG has no way to say "mirrored", so orientations 2/4/5/7 can't be told apart from their unmirrored
    // twins here; boxes on mirrored photos are taken as written (see LR-LIB-PEOPLE in docs/parity-checklist.md).
    let rotation = item.field("mwg-rs:Rotation").and_then(XmpValue::text).and_then(parse_number).filter(|r| r.is_finite());
    let (quarter, half) = (std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    let (cx, cy, w, h) = match rotation {
        Some(r) if (r + quarter).abs() < 0.05 => (1.0 - cy, cx, h, w),
        Some(r) if (r - quarter).abs() < 0.05 => (cy, 1.0 - cx, h, w),
        Some(r) if (r.abs() - half).abs() < 0.05 => (1.0 - cx, 1.0 - cy, w, h),
        _ => (cx, cy, w, h),
    };
    // Keep only the part of the box inside the photo: a region hanging off an edge (common for faces
    // cut by the frame) is clipped, and one entirely outside it (hostile or stale) is dropped, so every
    // stored rect is within 0..1 and non-empty.
    let rect = Rect::from_center(Point::new(cx, cy), w, h).intersect(&Rect::UNIT);
    if rect.is_empty() {
        return None;
    }
    let name = item.field("mwg-rs:Name").and_then(XmpValue::text).map(str::to_string).filter(|s| !s.is_empty());
    let description = item.field("mwg-rs:Description").and_then(XmpValue::text).map(str::to_string).filter(|s| !s.is_empty());
    let kind = match item.field("mwg-rs:Type").and_then(XmpValue::text).unwrap_or("") {
        t if t.eq_ignore_ascii_case("face") => RegionKind::Face,
        t if t.eq_ignore_ascii_case("pet") => RegionKind::Pet,
        t if t.eq_ignore_ascii_case("focus") => RegionKind::Focus,
        t if t.eq_ignore_ascii_case("barcode") => RegionKind::BarCode,
        t => RegionKind::Other(t.to_string()),
    };
    Some(Region { rect, kind, name, description })
}

/// A finite, whole, non-negative number that fits `u32`; anything else (fractional, huge, NaN) is not a
/// usable integer metadata value and is dropped rather than truncated or saturated.
fn whole_u32(v: f64) -> Option<u32> {
    (v.is_finite() && v >= 0.0 && v <= f64::from(u32::MAX) && v.fract() == 0.0).then_some(v as u32)
}

fn fmt_gps_coord(v: f64, pos: char, neg: char) -> String {
    let a = v.abs();
    let mut d = a.trunc();
    let mut m = ((a - d) * 60.0 * 1e6).round() / 1e6;
    if m >= 60.0 {
        d += 1.0;
        m = 0.0;
    }
    format!("{},{:.6}{}", d as i64, m, if v < 0.0 { neg } else { pos })
}

/// Parse an XMP packet (with or without the `<?xpacket?>` wrapper / `x:xmpmeta` element).
pub fn parse_xmp(s: &str) -> Result<XmpData, XmpError> {
    let s = s.trim_start_matches('\u{feff}');
    let tree = parse_tree(s)?;
    let mut descs = Vec::new();
    find_descriptions(&tree, &mut descs);
    let mut props = BTreeMap::new();
    let mut values = BTreeMap::new();
    // One budget for the whole document: several descriptions cannot each spend a fresh one.
    let mut key_budget = KeyBudget { left: MAX_KEY_BYTES, truncated: false };
    for d in descs {
        collect_description(d, "", &mut props, &mut key_budget);
        if let XmpValue::Struct(m) = node_struct(d) {
            values.extend(m);
        }
    }
    let first = |k: &str| props.get(k).and_then(|v| v.first()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let num = |k: &str| first(k).and_then(|s| parse_number(&s));
    let mut m = Metadata {
        make: first("tiff:Make"),
        model: first("tiff:Model"),
        serial_number: first("exifEX:BodySerialNumber").or_else(|| first("aux:SerialNumber")),
        software: first("xmp:CreatorTool"),
        lens_make: first("exifEX:LensMake"),
        lens_model: first("exifEX:LensModel").or_else(|| first("aux:Lens")),
        lens_serial_number: first("exifEX:LensSerialNumber").or_else(|| first("aux:LensSerialNumber")),
        exposure_time: num("exif:ExposureTime").filter(|v| *v > 0.0),
        f_number: num("exif:FNumber").filter(|v| *v > 0.0),
        focal_length: num("exif:FocalLength").filter(|v| *v > 0.0),
        focal_length_35mm: num("exif:FocalLengthIn35mmFilm").filter(|v| *v > 0.0),
        exposure_bias: num("exif:ExposureBiasValue"),
        exposure_program: num("exif:ExposureProgram").map(|v| v as u16),
        metering_mode: num("exif:MeteringMode").map(|v| v as u16),
        white_balance: num("exif:WhiteBalance").map(|v| v as u16),
        orientation: num("tiff:Orientation").filter(|v| (1.0..=8.0).contains(v) && v.fract() == 0.0).map(|v| Orientation::from_exif(v as u16)),
        width: num("tiff:ImageWidth").and_then(whole_u32).or_else(|| num("exif:PixelXDimension").and_then(whole_u32)),
        height: num("tiff:ImageLength").and_then(whole_u32).or_else(|| num("exif:PixelYDimension").and_then(whole_u32)),
        title: first("dc:title"),
        headline: first("photoshop:Headline"),
        caption: first("dc:description"),
        description_writer: first("photoshop:CaptionWriter"),
        category: first("photoshop:Category"),
        alt_text: first("Iptc4xmpCore:AltTextAccessibility"),
        extended_description: first("Iptc4xmpCore:ExtDescrAccessibility"),
        sublocation: first("Iptc4xmpCore:Location"),
        city: first("photoshop:City"),
        state: first("photoshop:State"),
        country: first("photoshop:Country"),
        country_code: first("Iptc4xmpCore:CountryCode"),
        copyright: first("dc:rights"),
        copyright_marked: first("xmpRights:Marked").and_then(|s| match s.to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }),
        usage_terms: first("xmpRights:UsageTerms"),
        copyright_url: first("xmpRights:WebStatement"),
        rating: num("xmp:Rating").map(|v| v.round().clamp(-1.0, 5.0) as i8),
        label: first("xmp:Label"),
        ..Default::default()
    };
    m.iso = [num("exif:ISOSpeedRatings"), num("exifEX:PhotographicSensitivity")].into_iter().flatten().filter(|v| *v > 0.0).find_map(whole_u32);
    if let Some(spec) = props.get("exifEX:LensSpecification").filter(|v| v.len() == 4) {
        let v: Vec<f64> = spec.iter().filter_map(|s| parse_number(s)).collect();
        if let &[a, b, c, d] = v.as_slice() {
            m.lens_spec = Some([a, b, c, d]);
        }
    }
    if let Some(creators) = props.get("dc:creator").filter(|c| !c.is_empty()) {
        m.artist = Some(creators.join("; "));
    }
    let ci = |k: &str| first(&format!("Iptc4xmpCore:CreatorContactInfo/Iptc4xmpCore:{k}")).unwrap_or_default();
    m.contact = crate::CreatorContact {
        job_title: first("photoshop:AuthorsPosition").unwrap_or_default(),
        address: ci("CiAdrExtadr"),
        city: ci("CiAdrCity"),
        state: ci("CiAdrRegion"),
        postal_code: ci("CiAdrPcode"),
        country: ci("CiAdrCtry"),
        phone: ci("CiTelWork"),
        email: ci("CiEmailWork"),
        website: ci("CiUrlWork"),
    };
    m.keywords = props.get("dc:subject").cloned().unwrap_or_default().into_iter().filter(|k| !k.is_empty()).collect();
    m.supplemental_categories =
        props.get("photoshop:SupplementalCategories").cloned().unwrap_or_default().into_iter().filter(|k| !k.is_empty()).collect();
    m.subject_code = props.get("Iptc4xmpCore:SubjectCode").cloned().unwrap_or_default().into_iter().filter(|k| !k.is_empty()).collect();
    m.hierarchical_keywords = props.get("lr:hierarchicalSubject").cloned().unwrap_or_default().into_iter().filter(|k| !k.is_empty()).collect();
    m.capture_time =
        ["exif:DateTimeOriginal", "photoshop:DateCreated", "xmp:CreateDate"].iter().find_map(|k| first(k).and_then(|s| DateTime::parse_iso(&s)));
    if let Some(fired) = first("exif:Flash/exif:Fired") {
        let b = |k: &str| first(k).is_some_and(|s| s.eq_ignore_ascii_case("true"));
        let n = |k: &str| num(k).unwrap_or(0.0) as u16;
        let fired = fired.eq_ignore_ascii_case("true");
        let raw = fired as u16
            | (n("exif:Flash/exif:Return") & 3) << 1
            | (n("exif:Flash/exif:Mode") & 3) << 3
            | (b("exif:Flash/exif:Function") as u16) << 5
            | (b("exif:Flash/exif:RedEyeMode") as u16) << 6;
        m.flash = Some(Flash { fired, raw });
    }
    if let (Some(lat), Some(lon)) =
        (first("exif:GPSLatitude").and_then(|s| parse_gps_coord(&s)), first("exif:GPSLongitude").and_then(|s| parse_gps_coord(&s)))
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
    {
        let altitude = num("exif:GPSAltitude").map(|a| if first("exif:GPSAltitudeRef").as_deref() == Some("1") { -a } else { a });
        m.gps = Some(Gps { latitude: lat, longitude: lon, altitude });
    }
    m.regions = values.get("mwg-rs:Regions").map(parse_regions).unwrap_or_default();
    let lc_settings = props.get("lc:settings").and_then(|v| v.first()).cloned();
    Ok(XmpData { metadata: m, lc_settings, properties: props, properties_truncated: key_budget.truncated, values })
}

fn frac(v: f64) -> String {
    if v > 0.0 && v < 1.0 {
        let inv = 1.0 / v;
        if (inv - inv.round()).abs() < 1e-6 {
            return format!("1/{}", inv.round() as u64);
        }
    }
    let (n, d) = lcc_tiff::writer::rational(v);
    format!("{n}/{d}")
}

fn sfrac(v: f64) -> String {
    let (n, d) = lcc_tiff::writer::srational(v);
    format!("{n}/{d}")
}

/// Serialise the interchange subset of `meta` (plus `lc_settings`, an opaque JSON string, in `lc:settings`)
/// as a complete XMP packet.
pub fn write_xmp(meta: &Metadata, lc_settings: Option<&str>) -> String {
    let lc: Vec<(&str, &str)> = lc_settings.map(|s| ("settings", s)).into_iter().collect();
    write_xmp_lc(meta, &lc)
}

/// Namespace of plug-in custom metadata fields in sidecars (prefix `lcpf`): `lcpf:fields`, a bag of
/// `{lcpf:plugin, lcpf:key, lcpf:value}`. Under the app's homepage (`lcc_brand::HOMEPAGE`):
/// `<homepage>/ns/plugin-fields/1.0/`.
pub fn plugin_fields_ns() -> String {
    format!("{}/ns/plugin-fields/1.0/", lcc_brand::HOMEPAGE.trim_end_matches('/'))
}

/// Prefix of the pixel-filter stack namespace ([`plugin_filters_ns`]) as [`parse_xmp`] reports it.
pub const FILTERS_PREFIX: &str = "lcpx";

/// Namespace of a photo's pixel-filter plug-in stack in sidecars (prefix [`FILTERS_PREFIX`]): `lcpx:filters`, a
/// seq of `{lcpx:plugin, lcpx:moduleHash, lcpx:name, lcpx:enabled, lcpx:stage, lcpx:params, lcpx:analysis}`
/// (`params` and `analysis` as JSON text). Under the app's homepage: `<homepage>/ns/plugin-filters/1.0/`.
pub fn plugin_filters_ns() -> String {
    format!("{}/ns/plugin-filters/1.0/", lcc_brand::HOMEPAGE.trim_end_matches('/'))
}

/// One entry of a sidecar's pixel-filter stack, as text (checking the values is the caller's business).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XmpPluginFilter {
    pub plugin: String,
    pub module_hash: String,
    pub name: String,
    /// `"True"` / `"False"`.
    pub enabled: String,
    /// `"source"` / `"display"`.
    pub stage: String,
    /// JSON object text.
    pub params: String,
    /// JSON text; empty when the filter has no analysis.
    pub analysis: String,
}

/// Longest text one stack entry part may have when read ([`plugin_filters`]); longer entries are skipped.
pub const MAX_FILTER_PART: usize = 128 * 1024;

/// The pixel-filter stack a packet carries (`lcpx:filters`, see [`plugin_filters_ns`]), in order: `None` when the
/// packet has none, entries without a plug-in id or with an over-long part skipped, at most `max` entries.
pub fn plugin_filters(d: &XmpData, max: usize) -> Option<Vec<XmpPluginFilter>> {
    let v = d.values.get(&format!("{FILTERS_PREFIX}:filters"))?;
    let out = v
        .items()
        .iter()
        .filter_map(|it| {
            let part = |k: &str| -> Option<String> {
                match it.field(&format!("{FILTERS_PREFIX}:{k}")) {
                    None => Some(String::new()),
                    Some(x) => x.text().filter(|s| s.len() <= MAX_FILTER_PART).map(str::to_string),
                }
            };
            let e = XmpPluginFilter {
                plugin: part("plugin")?,
                module_hash: part("moduleHash")?,
                name: part("name")?,
                enabled: part("enabled")?,
                stage: part("stage")?,
                params: part("params")?,
                analysis: part("analysis")?,
            };
            (!e.plugin.is_empty()).then_some(e)
        })
        .take(max)
        .collect();
    Some(out)
}

/// Prefix of the video clip namespace ([`video_ns`]) as [`parse_xmp`] reports it, whatever prefix a packet uses.
pub const VIDEO_PREFIX: &str = "lcvid";

/// Namespace of video clip settings in sidecars (prefix [`VIDEO_PREFIX`]): `lcvid:trimIn`, `lcvid:trimOut`,
/// `lcvid:posterTime` (seconds). Under the app's homepage (`lcc_brand::HOMEPAGE`): `<homepage>/ns/video/1.0/`.
pub fn video_ns() -> String {
    format!("{}/ns/video/1.0/", lcc_brand::HOMEPAGE.trim_end_matches('/'))
}

/// Like [`write_xmp`], with any number of simple `lc:<name>` properties (e.g. `settings`, `flag`).
pub fn write_xmp_lc(meta: &Metadata, lc: &[(&str, &str)]) -> String {
    write_xmp_ext(meta, lc, &[])
}

/// The plug-in custom metadata fields a packet carries (`lcpf:fields`, see [`plugin_fields_ns`]):
/// `(plug-in id, key, value)`, items missing a part skipped, at most 4096 items.
pub fn plugin_fields(d: &XmpData) -> Vec<(String, String, String)> {
    let Some(XmpValue::Array(items)) = d.values.get("lcpf:fields") else { return Vec::new() };
    items
        .iter()
        .filter_map(|it| {
            let part = |k: &str| it.field(k).and_then(XmpValue::text).map(str::to_string);
            let plugin = part("lcpf:plugin").filter(|s| !s.is_empty())?;
            let key = part("lcpf:key").filter(|s| !s.is_empty())?;
            Some((plugin, key, part("lcpf:value").unwrap_or_default()))
        })
        .take(4096)
        .collect()
}

/// [`write_xmp_lc`], plus plug-in custom metadata fields `(plug-in id, field key, value)` in
/// [`plugin_fields_ns`].
pub fn write_xmp_ext(meta: &Metadata, lc: &[(&str, &str)], plugin_fields: &[(&str, &str, &str)]) -> String {
    write_xmp_video(meta, lc, plugin_fields, &[])
}

/// [`write_xmp_ext`], plus simple video clip properties `(name, value)` in [`video_ns`] (`lcvid:<name>`).
pub fn write_xmp_video(meta: &Metadata, lc: &[(&str, &str)], plugin_fields: &[(&str, &str, &str)], video: &[(&str, &str)]) -> String {
    write_xmp_filters(meta, lc, plugin_fields, video, &[])
}

/// [`write_xmp_video`], plus a pixel-filter stack in [`plugin_filters_ns`] (`lcpx:filters`; nothing when empty).
pub fn write_xmp_filters(
    meta: &Metadata,
    lc: &[(&str, &str)],
    plugin_fields: &[(&str, &str, &str)],
    video: &[(&str, &str)],
    filters: &[XmpPluginFilter],
) -> String {
    let mut simple: Vec<(&str, String)> = Vec::new();
    let mut push = |k: &'static str, v: Option<String>| {
        if let Some(v) = v {
            simple.push((k, v));
        }
    };
    push("tiff:Make", meta.make.clone());
    push("tiff:Model", meta.model.clone());
    push("tiff:Orientation", meta.orientation.map(|o| o.to_exif().to_string()));
    push("tiff:ImageWidth", meta.width.map(|v| v.to_string()));
    push("tiff:ImageLength", meta.height.map(|v| v.to_string()));
    push("xmp:CreatorTool", meta.software.clone());
    push("xmp:Rating", meta.rating.map(|v| v.to_string()));
    push("xmp:Label", meta.label.clone());
    // a year or a month alone goes in `photoshop:DateCreated` only, as Lightroom writes it: Exif's
    // date can't say it
    push("exif:DateTimeOriginal", meta.capture_time.filter(DateTime::is_full).map(|d| d.to_iso()));
    push("photoshop:DateCreated", meta.capture_time.map(|d| d.to_iso()));
    push("exif:ExposureTime", meta.exposure_time.map(frac));
    push("exif:FNumber", meta.f_number.map(frac));
    push("exif:FocalLength", meta.focal_length.map(frac));
    push("exif:FocalLengthIn35mmFilm", meta.focal_length_35mm.map(|v| format!("{}", v.round() as i64)));
    push("exif:ExposureBiasValue", meta.exposure_bias.map(sfrac));
    push("exif:ExposureProgram", meta.exposure_program.map(|v| v.to_string()));
    push("exif:MeteringMode", meta.metering_mode.map(|v| v.to_string()));
    push("exif:WhiteBalance", meta.white_balance.map(|v| v.to_string()));
    push("exifEX:BodySerialNumber", meta.serial_number.clone());
    push("exifEX:LensMake", meta.lens_make.clone());
    push("exifEX:LensModel", meta.lens_model.clone());
    push("exifEX:LensSerialNumber", meta.lens_serial_number.clone());
    push("Iptc4xmpCore:Location", meta.sublocation.clone());
    push("photoshop:Headline", meta.headline.clone());
    push("photoshop:CaptionWriter", meta.description_writer.clone());
    push("photoshop:Category", meta.category.clone());
    push("photoshop:City", meta.city.clone());
    push("photoshop:State", meta.state.clone());
    push("photoshop:Country", meta.country.clone());
    push("Iptc4xmpCore:CountryCode", meta.country_code.clone());
    push("xmpRights:Marked", meta.copyright_marked.map(|m| if m { "True" } else { "False" }.to_string()));
    push("xmpRights:WebStatement", meta.copyright_url.clone());
    push("photoshop:AuthorsPosition", Some(meta.contact.job_title.trim().to_string()).filter(|v| !v.is_empty()));
    if let Some(g) = meta.gps {
        push("exif:GPSLatitude", Some(fmt_gps_coord(g.latitude, 'N', 'S')));
        push("exif:GPSLongitude", Some(fmt_gps_coord(g.longitude, 'E', 'W')));
        if let Some(a) = g.altitude {
            push("exif:GPSAltitude", Some(frac(a.abs())));
            push("exif:GPSAltitudeRef", Some(if a < 0.0 { "1" } else { "0" }.into()));
        }
    }

    let mut x = String::new();
    x.push_str("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    x.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"");
    x.push_str(lcc_brand::DISPLAY_NAME);
    x.push_str("\">\n");
    x.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    x.push_str("  <rdf:Description rdf:about=\"\"");
    for (p, u) in NAMESPACES.iter().skip(2).filter(|(p, _)| *p != "crs") {
        x.push_str(&format!("\n    xmlns:{p}=\"{u}\""));
    }
    if !plugin_fields.is_empty() {
        x.push_str(&format!("\n    xmlns:lcpf=\"{}\"", plugin_fields_ns()));
    }
    if !filters.is_empty() {
        x.push_str(&format!("\n    xmlns:{FILTERS_PREFIX}=\"{}\"", plugin_filters_ns()));
    }
    if !video.is_empty() {
        x.push_str(&format!("\n    xmlns:{VIDEO_PREFIX}=\"{}\"", video_ns()));
    }
    x.push_str(">\n");
    for (k, v) in &simple {
        x.push_str(&format!("   <{k}>{}</{k}>\n", escape(v)));
    }
    for (k, v) in lc {
        if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            // element content: quotes needn't be escaped, which keeps the JSON readable
            x.push_str(&format!("   <lc:{k}>{}</lc:{k}>\n", partial_escape(v)));
        }
    }
    for (k, v) in video {
        if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric()) {
            x.push_str(&format!("   <{VIDEO_PREFIX}:{k}>{}</{VIDEO_PREFIX}:{k}>\n", escape(v)));
        }
    }
    let array = |x: &mut String, k: &str, kind: &str, items: &[String], lang: bool| {
        if items.is_empty() {
            return;
        }
        x.push_str(&format!("   <{k}>\n    <rdf:{kind}>\n"));
        for it in items {
            if lang {
                x.push_str(&format!("     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n", escape(it)));
            } else {
                x.push_str(&format!("     <rdf:li>{}</rdf:li>\n", escape(it)));
            }
        }
        x.push_str(&format!("    </rdf:{kind}>\n   </{k}>\n"));
    };
    let one = |v: &Option<String>| v.iter().cloned().collect::<Vec<_>>();
    array(&mut x, "dc:title", "Alt", &one(&meta.title), true);
    array(&mut x, "dc:description", "Alt", &one(&meta.caption), true);
    array(&mut x, "Iptc4xmpCore:AltTextAccessibility", "Alt", &one(&meta.alt_text), true);
    array(&mut x, "Iptc4xmpCore:ExtDescrAccessibility", "Alt", &one(&meta.extended_description), true);
    array(&mut x, "dc:rights", "Alt", &one(&meta.copyright), true);
    array(&mut x, "xmpRights:UsageTerms", "Alt", &one(&meta.usage_terms), true);
    array(&mut x, "dc:creator", "Seq", &one(&meta.artist), false);
    array(&mut x, "dc:subject", "Bag", &meta.keywords, false);
    array(&mut x, "photoshop:SupplementalCategories", "Bag", &meta.supplemental_categories, false);
    array(&mut x, "Iptc4xmpCore:SubjectCode", "Bag", &meta.subject_code, false);
    array(&mut x, "lr:hierarchicalSubject", "Bag", &meta.hierarchical_keywords, false);
    array(&mut x, "exif:ISOSpeedRatings", "Seq", &meta.iso.map(|v| vec![v.to_string()]).unwrap_or_default(), false);
    if let Some(s) = meta.lens_spec {
        array(&mut x, "exifEX:LensSpecification", "Seq", &s.iter().map(|&v| frac(v)).collect::<Vec<_>>(), false);
    }
    let c = &meta.contact;
    let ci: Vec<(&str, &str)> = [
        ("CiAdrExtadr", &c.address),
        ("CiAdrCity", &c.city),
        ("CiAdrRegion", &c.state),
        ("CiAdrPcode", &c.postal_code),
        ("CiAdrCtry", &c.country),
        ("CiTelWork", &c.phone),
        ("CiEmailWork", &c.email),
        ("CiUrlWork", &c.website),
    ]
    .into_iter()
    .map(|(k, v)| (k, v.trim()))
    .filter(|(_, v)| !v.is_empty())
    .collect();
    if !ci.is_empty() {
        x.push_str(
            "   <Iptc4xmpCore:CreatorContactInfo rdf:parseType=\"Resource\">
",
        );
        for (k, v) in ci {
            x.push_str(&format!(
                "    <Iptc4xmpCore:{k}>{}</Iptc4xmpCore:{k}>
",
                escape(v)
            ));
        }
        x.push_str(
            "   </Iptc4xmpCore:CreatorContactInfo>
",
        );
    }
    if let Some(f) = meta.flash {
        let r = f.raw;
        let tf = |b: bool| if b { "True" } else { "False" };
        x.push_str(&format!(
            "   <exif:Flash rdf:parseType=\"Resource\">\n    <exif:Fired>{}</exif:Fired>\n    <exif:Return>{}</exif:Return>\n    <exif:Mode>{}</exif:Mode>\n    <exif:Function>{}</exif:Function>\n    <exif:RedEyeMode>{}</exif:RedEyeMode>\n   </exif:Flash>\n",
            tf(f.fired),
            (r >> 1) & 3,
            (r >> 3) & 3,
            tf(r & 0x20 != 0),
            tf(r & 0x40 != 0)
        ));
    }
    if !plugin_fields.is_empty() {
        x.push_str("   <lcpf:fields>\n    <rdf:Bag>\n");
        for (plugin, key, value) in plugin_fields {
            x.push_str(&format!(
                "     <rdf:li rdf:parseType=\"Resource\">\n      <lcpf:plugin>{}</lcpf:plugin>\n      <lcpf:key>{}</lcpf:key>\n      <lcpf:value>{}</lcpf:value>\n     </rdf:li>\n",
                escape(plugin),
                escape(key),
                escape(value)
            ));
        }
        x.push_str("    </rdf:Bag>\n   </lcpf:fields>\n");
    }
    if !filters.is_empty() {
        let p = FILTERS_PREFIX;
        x.push_str(&format!("   <{p}:filters>\n    <rdf:Seq>\n"));
        for f in filters {
            x.push_str("     <rdf:li rdf:parseType=\"Resource\">\n");
            for (k, v) in [
                ("plugin", &f.plugin),
                ("moduleHash", &f.module_hash),
                ("name", &f.name),
                ("enabled", &f.enabled),
                ("stage", &f.stage),
                ("params", &f.params),
                ("analysis", &f.analysis),
            ] {
                if !v.is_empty() {
                    x.push_str(&format!("      <{p}:{k}>{}</{p}:{k}>\n", partial_escape(v.as_str())));
                }
            }
            x.push_str("     </rdf:li>\n");
        }
        x.push_str(&format!("    </rdf:Seq>\n   </{p}:filters>\n"));
    }
    x.push_str("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>");
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// References resolve, anything else (unknown, unterminated, or a `;` too far away to end one) stays as
    /// written, and a value that is nothing but `&` is handled in one pass.
    #[test]
    fn attribute_references_resolve_and_hostile_ones_stay_verbatim() {
        assert_eq!(unescape_attr("a&amp;b&#65;&#x42;&lt;&gt;&quot;&apos;"), "a&bAB<>\"'");
        assert_eq!(unescape_attr("&bogus; & &#xZZ; tail&"), "&bogus; & &#xZZ; tail&");
        let far = format!("&{};", "x".repeat(40));
        assert_eq!(unescape_attr(&far), far);
        let many = "&".repeat(1 << 20);
        assert_eq!(unescape_attr(&many), many);
        // multi-byte text around a reference is never cut inside a character
        assert_eq!(unescape_attr("é&amp;日&é;"), "é&日&é;");
        // leading zeros are valid in numeric references, however many there are
        assert_eq!(unescape_attr("&#00000000000000000065;&#x000000000000000042;"), "AB");
        let zeros = format!("&#{};", "0".repeat(1 << 20));
        assert_eq!(unescape_attr(&zeros), zeros);
    }

    fn full() -> Metadata {
        Metadata {
            // write_xmp doesn't emit regions yet (read-only interchange; see docs/xmp-interop.md), so the
            // roundtrip fixture must leave this empty or `roundtrip_all_fields` can't round-trip it.
            regions: Vec::new(),
            focus: None,
            fuji_recipe: None,
            make: Some("Maker & Sons".into()),
            model: Some("X <1>".into()),
            serial_number: Some("SN1".into()),
            software: Some(lcc_brand::DISPLAY_NAME.into()),
            lens_make: Some("L".into()),
            lens_model: Some("24-70mm".into()),
            lens_serial_number: Some("LS".into()),
            lens_spec: Some([24.0, 70.0, 2.8, 2.8]),
            // (read from Nikon maker notes only; XMP carries no VR state)
            stabilization: None,
            capture_time: DateTime::parse_iso("2022-05-06T07:08:09.123-04:00"),
            exposure_time: Some(1.0 / 125.0),
            f_number: Some(5.6),
            iso: Some(800),
            focal_length: Some(35.0),
            focal_length_35mm: Some(52.0),
            flash: Some(Flash { fired: true, raw: 0x59 }),
            exposure_bias: Some(-1.0 / 3.0),
            exposure_program: Some(2),
            metering_mode: Some(3),
            white_balance: Some(1),
            orientation: Some(Orientation::Rotate270),
            gps: Some(Gps { latitude: 48.858222, longitude: -2.2945, altitude: Some(-3.5) }),
            width: Some(8000),
            height: Some(6000),
            artist: Some("Ann \"Photo\" Lee".into()),
            copyright: Some("© 2022".into()),
            copyright_marked: Some(true),
            usage_terms: Some("Editorial use only; no <resale>".into()),
            copyright_url: Some("https://example.com/licence?a=1&b=2".into()),
            title: Some("Tïtle".into()),
            headline: Some("A headline".into()),
            caption: Some("Line one\nline two".into()),
            description_writer: Some("Jane Doe".into()),
            category: Some("CAT".into()),
            supplemental_categories: vec!["Sports".into(), "Nature".into()],
            subject_code: vec!["01001000".into(), "02002000".into()],
            alt_text: Some("A tree by the sea".into()),
            extended_description: Some("Long <description>".into()),
            sublocation: Some("Champ de Mars".into()),
            city: Some("Paris".into()),
            state: Some("Île-de-France".into()),
            country: Some("France".into()),
            country_code: Some("FR".into()),
            contact: crate::CreatorContact { job_title: "Lead".into(), email: "a@b.c".into(), city: "Lyon & co".into(), ..Default::default() },
            keywords: vec!["tree".into(), "sky & sea".into()],
            hierarchical_keywords: vec!["Places|France|Paris".into()],
            rating: Some(-1),
            label: Some("Green".into()),
        }
    }

    /// Control characters XML 1.0 forbids (from a camera's maker note, a pasted caption) used to be written
    /// verbatim, leaving a sidecar no XML reader accepts. They are dropped; the rest of the text survives.
    #[test]
    fn xml_illegal_control_characters_are_not_written() {
        let m = Metadata { title: Some("a\u{1}b\u{0}c\u{1b}d\u{FFFE}e\tf".into()), make: Some("Can\u{7}on".into()), ..Default::default() };
        let x = write_xmp_lc(&m, &[("note", "x\u{2}y")]);
        assert!(!x.chars().any(|c| matches!(c, '\u{0}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{FFFE}')));
        let d = parse_xmp(&x).unwrap();
        assert_eq!(d.metadata.title.as_deref(), Some("abcde\tf"));
        assert_eq!(d.metadata.make.as_deref(), Some("Canon"));
    }

    #[test]
    fn roundtrip_all_fields() {
        let m = full();
        let json = r#"{"version":1,"exposure":0.5,"note":"<&>"}"#;
        let x = write_xmp(&m, Some(json));
        let d = parse_xmp(&x).unwrap();
        assert_eq!(d.lc_settings.as_deref(), Some(json));
        let mut got = d.metadata.clone();
        // floats go through rationals
        assert!((got.exposure_bias.unwrap() + 1.0 / 3.0).abs() < 1e-5);
        got.exposure_bias = m.exposure_bias;
        let g = got.gps.unwrap();
        assert!((g.latitude - 48.858222).abs() < 1e-6 && (g.longitude + 2.2945).abs() < 1e-6, "{g:?}");
        got.gps = m.gps;
        assert_eq!(got, m);
    }

    #[test]
    fn empty_metadata_roundtrip() {
        let x = write_xmp(&Metadata::default(), None);
        let d = parse_xmp(&x).unwrap();
        assert_eq!(d.metadata, Metadata::default());
        assert_eq!(d.lc_settings, None);
    }

    #[test]
    fn foreign_namespace_cannot_impersonate_known_metadata() {
        for body in [
            r#"xmp:Rating="2" lc:settings="foreign settings"/>"#,
            r#"><xmp:Rating>2</xmp:Rating><lc:settings>foreign settings</lc:settings></rdf:Description>"#,
        ] {
            let packet = format!(
                r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description
                xmlns:xmp="https://example.org/vendor/" xmlns:lc="https://example.org/vendor/" {body}</rdf:RDF>"#
            );
            let d = parse_xmp(&packet).unwrap();
            assert_eq!(d.metadata.rating, None, "foreign rating: {packet}");
            assert_eq!(d.lc_settings, None, "foreign settings: {packet}");
            assert!(!d.properties.contains_key("xmp:Rating"));
            assert!(!d.values.contains_key("lc:settings"));
            assert!(d.properties.values().any(|v| v == &["foreign settings"]));
            assert!(d.properties.keys().all(|key| !key.contains('/')), "opaque names must not create struct paths");
        }
    }

    #[test]
    fn foreign_namespace_names_are_distinct_and_scoped() {
        let packet = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/">
                <xmp:Rating xmlns:xmp="https://example.org/one/">1</xmp:Rating>
                <xmp:Rating xmlns:xmp="https://example.org/two/">2</xmp:Rating>
                <xmp:Rating>4</xmp:Rating>
            </rdf:Description></rdf:RDF>"#;
        let d = parse_xmp(packet).unwrap();
        assert_eq!(d.metadata.rating, Some(4), "a local rebinding must not escape its scope");
        assert_eq!(d.properties.len(), 3, "different foreign URIs must not collide");
        assert_eq!(d.values.len(), 3);
        assert_eq!(d.properties["xmp:Rating"], vec!["4"]);
        assert!(d.properties.values().any(|v| v == &["1"]));
        assert!(d.properties.values().any(|v| v == &["2"]));
    }

    #[test]
    fn attribute_form_foreign_prefixes_and_structs() {
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about="" xmlns:a="http://ns.adobe.com/xap/1.0/" xmlns:d="http://purl.org/dc/elements/1.1/"
               xmlns:e="http://ns.adobe.com/exif/1.0/" a:Rating="4" a:Label="Blue" e:FNumber="40/10" e:GPSLatitude="10,30N" e:GPSLongitude="20,15,36W">
              <d:title><rdf:Alt><rdf:li xml:lang="de">Titel</rdf:li><rdf:li xml:lang="x-default">Title &amp; more &#233;</rdf:li></rdf:Alt></d:title>
              <e:Flash><rdf:Description e:Fired="False" e:Mode="2"/></e:Flash>
              <d:subject><rdf:Bag><rdf:li>k1</rdf:li><rdf:li>k2</rdf:li></rdf:Bag></d:subject>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let d = parse_xmp(x).unwrap();
        let m = d.metadata;
        assert_eq!(m.rating, Some(4));
        assert_eq!(m.label.as_deref(), Some("Blue"));
        assert_eq!(m.f_number, Some(4.0));
        assert_eq!(m.title.as_deref(), Some("Title & more é"));
        assert_eq!(m.keywords, vec!["k1".to_string(), "k2".to_string()]);
        assert_eq!(m.flash, Some(Flash { fired: false, raw: 0x10 }));
        let g = m.gps.unwrap();
        assert!((g.latitude - 10.5).abs() < 1e-9);
        assert!((g.longitude + 20.26).abs() < 1e-9);
        assert_eq!(d.properties["dc:title"], vec!["Title & more é".to_string(), "Titel".to_string()]);
    }

    /// The IPTC Content fields as Lightroom Classic and DxO PhotoLab write them (issue #687):
    /// headline, caption writer, legacy category and supplemental categories, and the IPTC
    /// Extension subject codes.
    #[test]
    fn iptc_content_fields_parse_and_roundtrip() {
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about="" xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"
               xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">
              <photoshop:Headline>Sunrise over the harbour</photoshop:Headline>
              <photoshop:CaptionWriter>Jane Doe</photoshop:CaptionWriter>
              <photoshop:Category>CAT</photoshop:Category>
              <photoshop:SupplementalCategories><rdf:Bag><rdf:li>Sports</rdf:li><rdf:li>Nature</rdf:li></rdf:Bag></photoshop:SupplementalCategories>
              <Iptc4xmpCore:SubjectCode><rdf:Bag><rdf:li>01001000</rdf:li><rdf:li>02002000</rdf:li></rdf:Bag></Iptc4xmpCore:SubjectCode>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let m = parse_xmp(x).unwrap().metadata;
        assert_eq!(m.headline.as_deref(), Some("Sunrise over the harbour"));
        assert_eq!(m.description_writer.as_deref(), Some("Jane Doe"));
        assert_eq!(m.category.as_deref(), Some("CAT"));
        assert_eq!(m.supplemental_categories, vec!["Sports".to_string(), "Nature".to_string()]);
        assert_eq!(m.subject_code, vec!["01001000".to_string(), "02002000".to_string()]);
        // a packet without them keeps them empty
        assert_eq!(parse_xmp(&write_xmp(&Metadata::default(), None)).unwrap().metadata.supplemental_categories, Vec::<String>::new());
        // written back into another packet and read again
        let back = parse_xmp(&write_xmp(&m, None)).unwrap().metadata;
        assert_eq!(back.headline, m.headline);
        assert_eq!(back.description_writer, m.description_writer);
        assert_eq!(back.category, m.category);
        assert_eq!(back.supplemental_categories, m.supplemental_categories);
        assert_eq!(back.subject_code, m.subject_code);
    }

    /// The creator's contact details travel as IPTC Core `CreatorContactInfo` (a struct) and the
    /// job title as `photoshop:AuthorsPosition`, and read back, also from a nested-Description form.
    #[test]
    fn creator_contact_round_trips() {
        let m = Metadata {
            contact: crate::CreatorContact {
                job_title: "Photographer".into(),
                address: "1 Harbour <Road>".into(),
                city: "Hobart".into(),
                postal_code: "7000".into(),
                email: "ann@example.org".into(),
                website: "https://example.org".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let x = write_xmp(&m, None);
        assert!(x.contains("Iptc4xmpCore:CreatorContactInfo"), "{x}");
        assert!(!x.contains("CiTelWork"), "unset fields are left out: {x}");
        assert_eq!(parse_xmp(&x).unwrap().metadata, m);
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about="" xmlns:I="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/">
            <I:CreatorContactInfo><rdf:Description I:CiEmailWork="a@b.c" I:CiTelWork="+61 3"/></I:CreatorContactInfo>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let c = parse_xmp(x).unwrap().metadata.contact;
        assert_eq!((c.email.as_str(), c.phone.as_str()), ("a@b.c", "+61 3"));
        assert!(Metadata::default().contact.is_empty());
    }

    /// Copyright status, usage terms and info URL (XMP Rights Management schema): written as
    /// `xmpRights:Marked` (Boolean), `xmpRights:UsageTerms` (Lang Alt) and `xmpRights:WebStatement` (URI).
    #[test]
    fn rights_management_fields() {
        let m = Metadata { copyright_marked: Some(false), usage_terms: Some("CC0".into()), ..Default::default() };
        let x = write_xmp(&m, None);
        assert!(x.contains("<xmpRights:Marked>False</xmpRights:Marked>"), "{x}");
        assert!(x.contains("<xmpRights:UsageTerms>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">CC0</rdf:li>"), "{x}");
        assert!(!x.contains("WebStatement"), "unset fields are left out");
        assert_eq!(parse_xmp(&x).unwrap().metadata, m);
        // attribute form, other prefixes; anything but True / False is "unknown"
        let read = |marked: &str| {
            let x = format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                <rdf:Description rdf:about="" xmlns:r="http://ns.adobe.com/xap/1.0/rights/" r:Marked="{marked}" r:WebStatement="https://example.org/c"/>
                </rdf:RDF></x:xmpmeta>"#
            );
            parse_xmp(&x).unwrap().metadata
        };
        assert_eq!(read("True").copyright_marked, Some(true));
        assert_eq!(read("false").copyright_marked, Some(false));
        assert_eq!(read("").copyright_marked, None);
        assert_eq!(read("maybe").copyright_marked, None);
        assert_eq!(read("True").copyright_url.as_deref(), Some("https://example.org/c"));
    }

    /// Fractional, oversized or non-finite ISO values are dropped (never truncated or saturated) and the
    /// alternate property is still considered.
    #[test]
    fn invalid_iso_candidates_are_rejected() {
        let read = |attrs: &str| {
            let x = format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                <rdf:Description rdf:about="" xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:exifEX="http://cipa.jp/exif/1.0/" {attrs}/>
                </rdf:RDF></x:xmpmeta>"#
            );
            parse_xmp(&x).map(|d| d.metadata.iso).unwrap_or(None)
        };
        assert_eq!(read(r#"exif:ISOSpeedRatings="100.5""#), None);
        assert_eq!(read(r#"exif:ISOSpeedRatings="4294967296""#), None);
        assert_eq!(read(r#"exif:ISOSpeedRatings="1e100""#), None);
        assert_eq!(read(r#"exif:ISOSpeedRatings="NaN""#), None);
        assert_eq!(read(r#"exif:ISOSpeedRatings="100.5" exifEX:PhotographicSensitivity="400""#), Some(400));
        assert_eq!(read(r#"exif:ISOSpeedRatings="200.0""#), Some(200));
        // non positive iso is ignored
        // A zero or negative `exif:ISOSpeedRatings` is "no ISO" (as in the EXIF reader), not `Some(0)`, and
        // must not hide a valid `exifEX:PhotographicSensitivity`.
        {
            let read = |attrs: &str| {
                let x = format!(
                    r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                    <rdf:Description rdf:about="" xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:exifEX="http://cipa.jp/exif/1.0/" {attrs}/>
                    </rdf:RDF></x:xmpmeta>"#
                );
                parse_xmp(&x).unwrap().metadata.iso
            };
            assert_eq!(read(r#"exif:ISOSpeedRatings="0" exifEX:PhotographicSensitivity="400""#), Some(400));
            assert_eq!(read(r#"exif:ISOSpeedRatings="-100""#), None);
            assert_eq!(read(r#"exif:ISOSpeedRatings="0""#), None);
            assert_eq!(read(r#"exif:ISOSpeedRatings="200""#), Some(200));
        }
    }

    /// Fractional, oversized or negative pixel dimensions are dropped, not truncated or saturated.
    #[test]
    fn invalid_pixel_dimensions_are_rejected() {
        let read = |attrs: &str| {
            let x = format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                <rdf:Description rdf:about="" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:exif="http://ns.adobe.com/exif/1.0/" {attrs}/>
                </rdf:RDF></x:xmpmeta>"#
            );
            parse_xmp(&x).map(|d| d.metadata.width).unwrap_or(None)
        };
        for bad in ["640.9", "4294967296", "-1", "1e100000", "NaN", "Infinity", "nope"] {
            assert_eq!(read(&format!(r#"tiff:ImageWidth="{bad}""#)), None, "{bad}");
            assert_eq!(read(&format!(r#"exif:PixelXDimension="{bad}""#)), None, "{bad}");
        }
        assert_eq!(read(r#"tiff:ImageWidth="640.5" exif:PixelXDimension="800""#), Some(800));
        assert_eq!(read(r#"tiff:ImageWidth="640""#), Some(640));
        // negative xmp pixel width is ignored
        {
            let read = |attrs: &str| {
                let x = format!(
                    r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                    <rdf:Description xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:exif="http://ns.adobe.com/exif/1.0/" {attrs}/>
                </rdf:RDF>"#
                );
                parse_xmp(&x).unwrap().metadata.width
            };

            assert_eq!(read(r#"tiff:ImageWidth="-1""#), None);
            assert_eq!(read(r#"tiff:ImageWidth="-1" exif:PixelXDimension="640""#), Some(640));
            assert_eq!(read(r#"exif:PixelXDimension="-1""#), None);
        }
    }

    #[test]
    fn malformed_capture_time_falls_back_and_keeps_raw_property() {
        let packet = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" exif:DateTimeOriginal="2026-01-01T" xmp:CreateDate="2026-01-02T10:00:00Z"/></rdf:RDF></x:xmpmeta>"#;
        let parsed = parse_xmp(packet).unwrap();
        assert_eq!(
            parsed.metadata.capture_time.map(|d| (d.year, d.month, d.day, d.hour, d.minute, d.second, d.offset_minutes)),
            Some((2026, 1, 2, 10, 0, 0, Some(0)))
        );
        assert_eq!(parsed.properties["exif:DateTimeOriginal"], vec!["2026-01-01T".to_string()]);
    }

    #[test]
    fn malformed_capture_time_without_fallback_is_not_projected() {
        let packet = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:exif="http://ns.adobe.com/exif/1.0/" exif:DateTimeOriginal="2026-01-01T"/></rdf:RDF></x:xmpmeta>"#;
        let parsed = parse_xmp(packet).unwrap();
        assert_eq!(parsed.metadata.capture_time, None);
        assert_eq!(parsed.properties["exif:DateTimeOriginal"], vec!["2026-01-01T".to_string()]);
    }

    fn orientation_packet(value: &str) -> String {
        format!(
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:tiff="http://ns.adobe.com/tiff/1.0/" tiff:Orientation="{value}"/></rdf:RDF>"#
        )
    }

    #[test]
    fn xmp_orientation_rejects_fractional_values_and_keeps_raw_properties() {
        for value in ["6.9", "1.5", "8.9", "13/2"] {
            let parsed = parse_xmp(&orientation_packet(value)).unwrap();
            assert_eq!(parsed.metadata.orientation, None, "{value}");
            assert_eq!(parsed.properties["tiff:Orientation"], vec![value.to_string()], "{value}");
            assert!(!write_xmp(&parsed.metadata, None).contains("tiff:Orientation"), "rejected {value} must not become a written orientation");
        }
        // xmp orientation rejects out of range values
        {
            for value in ["0", "9", "-1", "nan", "inf"] {
                let parsed = parse_xmp(&orientation_packet(value)).unwrap();
                assert_eq!(parsed.metadata.orientation, None, "{value}");
            }
        }
    }

    #[test]
    fn xmp_orientation_integral_forms_keep_their_meaning() {
        for code in 1..=8 {
            let parsed = parse_xmp(&orientation_packet(&code.to_string())).unwrap();
            assert_eq!(parsed.metadata.orientation.map(|orientation| orientation.to_exif()), Some(code));
        }
        for value in ["6.0", "6/1", " 6.0 "] {
            let parsed = parse_xmp(&orientation_packet(value)).unwrap();
            assert_eq!(parsed.metadata.orientation.map(|orientation| orientation.to_exif()), Some(6), "{value}");
        }
    }

    #[test]
    fn malformed_inputs() {
        assert!(parse_xmp("<a><b></a>").is_ok() || parse_xmp("<a><b></a>").is_err());
        assert_eq!(parse_xmp("").unwrap().metadata, Metadata::default());
        assert_eq!(parse_xmp("not xml at all").unwrap().metadata, Metadata::default());
        let deep = "<a>".repeat(1000);
        assert_eq!(parse_xmp(&deep), Err(XmpError::TooComplex));
        let x = write_xmp(&full(), Some("{}"));
        for n in (0..x.len()).step_by(7) {
            if x.is_char_boundary(n) {
                let _ = parse_xmp(&x[..n]);
            }
        }
    }

    #[test]
    fn lc_extras_and_crs_prefix() {
        let x = write_xmp_lc(&Metadata::default(), &[("settings", "{\"a\":1}"), ("flag", "pick"), ("bad name", "x")]);
        assert!(!x.contains("xmlns:crs"), "we never write crs:");
        let d = parse_xmp(&x).unwrap();
        assert_eq!(d.lc_settings.as_deref(), Some("{\"a\":1}"));
        assert_eq!(d.properties["lc:flag"], vec!["pick".to_string()]);
        assert!(!x.contains("bad name"));
        let foreign = r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
            xmlns:cr="http://ns.adobe.com/camera-raw-settings/1.0/" cr:Exposure2012="+0.5"/>"#;
        assert_eq!(parse_xmp(foreign).unwrap().properties["crs:Exposure2012"], vec!["+0.5".to_string()]);
    }

    #[test]
    fn gps_coords() {
        assert_eq!(parse_gps_coord("45,30N"), Some(45.5));
        assert_eq!(parse_gps_coord("45,30,36S"), Some(-45.51));
        assert_eq!(parse_gps_coord("45X"), None);
        assert_eq!(parse_gps_coord(""), None);
        assert_eq!(parse_gps_coord("é"), None);
        assert_eq!(fmt_gps_coord(-45.51, 'N', 'S'), "45,30.600000S");
        assert_eq!(fmt_gps_coord(10.9999999999, 'N', 'S'), "11,0.000000N");
        assert_eq!(fmt_gps_coord(-0.9999999999, 'E', 'W'), "1,0.000000W");
    }

    #[test]
    fn xmp_gps_component_bounds() {
        let parse = |lat: &str, lon: &str| {
            let x = format!(
                r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
                xmlns:e="http://ns.adobe.com/exif/1.0/" e:GPSLatitude="{lat}" e:GPSLongitude="{lon}"/>"#
            );
            parse_xmp(&x).unwrap()
        };

        for (lat, lon) in [
            ("45,60N", "7,15E"),
            ("45,99N", "7,15E"),
            ("45,0,60N", "7,15E"),
            ("45,0,99N", "7,15E"),
            ("45,15N", "7,60E"),
            ("45,15N", "7,99E"),
            ("45,15N", "7,0,60E"),
            ("45,15N", "7,0,99E"),
        ] {
            let xmp = parse(lat, lon);
            assert_eq!(xmp.metadata.gps, None, "{lat}, {lon}");
            assert_eq!(xmp.properties.get("exif:GPSLatitude").and_then(|v| v.first()).map(String::as_str), Some(lat));
            assert_eq!(xmp.properties.get("exif:GPSLongitude").and_then(|v| v.first()).map(String::as_str), Some(lon));
        }

        for (lat, lon, expected) in [
            ("45,59.5N", "7,59.5E", (45.0 + 59.5 / 60.0, 7.0 + 59.5 / 60.0)),
            ("45,59,59S", "7,59,59W", (-(45.0 + 59.0 / 60.0 + 59.0 / 3600.0), -(7.0 + 59.0 / 60.0 + 59.0 / 3600.0))),
            ("0,0N", "0,0E", (0.0, 0.0)),
            ("90,0N", "180,0W", (90.0, -180.0)),
            (" 45,30n ", " 7,15e ", (45.5, 7.25)),
        ] {
            let xmp = parse(lat, lon);
            let gps = xmp.metadata.gps.expect("valid XMP GPS components");
            assert!((gps.latitude - expected.0).abs() < 1e-9, "{lat}: {}", gps.latitude);
            assert!((gps.longitude - expected.1).abs() < 1e-9, "{lon}: {}", gps.longitude);
        }

        assert_eq!(parse("95,0N", "7,0E").metadata.gps, None, "combined latitude range remains checked");
        assert_eq!(parse("45,0N", "200,0E").metadata.gps, None, "combined longitude range remains checked");
        // xmp gps out of range rejected
        // Like the EXIF and GPX readers, XMP drops a position outside latitude ±90° / longitude ±180°.
        {
            let gps = |lat: &str, lon: &str| {
                let x = format!(
                    r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
                    xmlns:e="http://ns.adobe.com/exif/1.0/" e:GPSLatitude="{lat}" e:GPSLongitude="{lon}"/>"#
                );
                parse_xmp(&x).unwrap().metadata.gps
            };
            assert_eq!(gps("95,0N", "200,30E"), None);
            assert_eq!(gps("95,0N", "10,0E"), None);
            assert_eq!(gps("10,0S", "200,30W"), None);
            assert_eq!(gps("90,0,1N", "0,0E"), None);
            assert_eq!(gps("90,0N", "180,0E"), Some(Gps { latitude: 90.0, longitude: 180.0, altitude: None }));
            assert_eq!(gps("90,0S", "180,0W"), Some(Gps { latitude: -90.0, longitude: -180.0, altitude: None }));
        }
    }

    /// MWG Region Guidelines v2.0, element form (`rdf:parseType="Resource"`, self-closing `Area`) — how
    /// Lightroom typically writes it. One named face, one untyped/unnamed region (still usable), one
    /// region with a description. `AppliedToDimensions` is present but irrelevant: normalized areas
    /// (the default, and the only unit real files use) don't need it.
    #[test]
    fn mwg_regions_element_form() {
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about=""
                xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
                xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#"
                xmlns:stDim="http://ns.adobe.com/xap/1.0/sType/Dimensions#">
              <mwg-rs:Regions rdf:parseType="Resource">
                <mwg-rs:AppliedToDimensions rdf:parseType="Resource" stDim:w="4000" stDim:h="3000" stDim:unit="pixel"/>
                <mwg-rs:RegionList>
                  <rdf:Bag>
                    <rdf:li rdf:parseType="Resource">
                      <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.5" stArea:y="0.4" stArea:w="0.2" stArea:h="0.3" stArea:unit="normalized"/>
                      <mwg-rs:Name>Jane Doe</mwg-rs:Name>
                      <mwg-rs:Type>Face</mwg-rs:Type>
                    </rdf:li>
                    <rdf:li rdf:parseType="Resource">
                      <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.1" stArea:y="0.1" stArea:w="0.05" stArea:h="0.05" stArea:unit="normalized"/>
                    </rdf:li>
                    <rdf:li rdf:parseType="Resource">
                      <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.8" stArea:y="0.2" stArea:w="0.1" stArea:h="0.1" stArea:unit="normalized"/>
                      <mwg-rs:Type>Pet</mwg-rs:Type>
                      <mwg-rs:Name>Rex</mwg-rs:Name>
                      <mwg-rs:Description>Good boy</mwg-rs:Description>
                    </rdf:li>
                  </rdf:Bag>
                </mwg-rs:RegionList>
              </mwg-rs:Regions>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let regions = parse_xmp(x).unwrap().metadata.regions;
        assert_eq!(regions.len(), 3, "{regions:?}");

        assert_eq!(regions[0].kind, RegionKind::Face);
        assert_eq!(regions[0].name.as_deref(), Some("Jane Doe"));
        assert_eq!(regions[0].description, None);
        let r = regions[0].rect;
        assert!((r.x0 - 0.4).abs() < 1e-9 && (r.x1 - 0.6).abs() < 1e-9, "{r:?}");
        assert!((r.y0 - 0.25).abs() < 1e-9 && (r.y1 - 0.55).abs() < 1e-9, "{r:?}");

        // No mwg-rs:Type or mwg-rs:Name at all: still a usable region, kind falls back to `Other("")`.
        assert_eq!(regions[1].kind, RegionKind::Other(String::new()));
        assert_eq!(regions[1].name, None);

        assert_eq!(regions[2].kind, RegionKind::Pet);
        assert_eq!(regions[2].name.as_deref(), Some("Rex"));
        assert_eq!(regions[2].description.as_deref(), Some("Good boy"));
    }

    /// Lightroom Classic writes a rotated photo's (Exif orientation 3 / 6 / 8) box in the sensor's frame
    /// and records the orientation as `mwg-rs:Rotation` (±π / −π/2 / +π/2); the reader returns the
    /// upright frame.
    #[test]
    fn mwg_regions_quarter_turn_rotation_is_mapped_to_the_upright_frame() {
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about=""
                xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
                xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
              <mwg-rs:Regions rdf:parseType="Resource">
                <mwg-rs:RegionList>
                  <rdf:Bag>
                    <rdf:li rdf:parseType="Resource" mwg-rs:Rotation="-1.57080" mwg-rs:Type="Face">
                      <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.3" stArea:y="0.4" stArea:w="0.2" stArea:h="0.3"/>
                    </rdf:li>
                    <rdf:li rdf:parseType="Resource" mwg-rs:Rotation="1.57080" mwg-rs:Type="Face">
                      <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.3" stArea:y="0.4" stArea:w="0.2" stArea:h="0.3"/>
                    </rdf:li>
                    <rdf:li rdf:parseType="Resource" mwg-rs:Rotation="0.00000" mwg-rs:Type="Face">
                      <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.3" stArea:y="0.4" stArea:w="0.2" stArea:h="0.3"/>
                    </rdf:li>
                    <rdf:li rdf:parseType="Resource" mwg-rs:Rotation="3.14159" mwg-rs:Type="Face">
                      <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.3" stArea:y="0.4" stArea:w="0.2" stArea:h="0.3"/>
                    </rdf:li>
                    <rdf:li rdf:parseType="Resource" mwg-rs:Rotation="-3.14159" mwg-rs:Type="Face">
                      <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.3" stArea:y="0.4" stArea:w="0.2" stArea:h="0.3"/>
                    </rdf:li>
                  </rdf:Bag>
                </mwg-rs:RegionList>
              </mwg-rs:Regions>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let regions = parse_xmp(x).unwrap().metadata.regions;
        assert_eq!(regions.len(), 5, "{regions:?}");
        let near = |r: Rect, b: [f64; 4]| [r.x0 - b[0], r.y0 - b[1], r.x1 - b[2], r.y1 - b[3]].iter().all(|d| d.abs() < 1e-9);
        assert!(near(regions[0].rect, [0.45, 0.2, 0.75, 0.4]), "−π/2 (orientation 6): {:?}", regions[0].rect);
        assert!(near(regions[1].rect, [0.25, 0.6, 0.55, 0.8]), "+π/2 (orientation 8): {:?}", regions[1].rect);
        assert!(near(regions[2].rect, [0.2, 0.25, 0.4, 0.55]), "0 is untouched: {:?}", regions[2].rect);
        assert!(near(regions[3].rect, [0.6, 0.45, 0.8, 0.75]), "π (orientation 3): {:?}", regions[3].rect);
        assert!(near(regions[4].rect, [0.6, 0.45, 0.8, 0.75]), "−π (orientation 3): {:?}", regions[4].rect);
    }

    /// The size a packet's boxes were drawn on is its `AppliedToDimensions`, unless a box says how the photo was turned
    /// (Lightroom's `mwg-rs:Rotation`: those are upright already) or there is no size to go by.
    #[test]
    fn regions_drawn_on_is_the_applied_to_size_unless_a_box_says_how_it_was_turned() {
        let packet = |dims: &str, rotation: &str| {
            format!(
                r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                <rdf:Description rdf:about="" xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
                    xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#" xmlns:stDim="http://ns.adobe.com/xap/1.0/sType/Dimensions#">
                  <mwg-rs:Regions rdf:parseType="Resource">{dims}
                    <mwg-rs:RegionList><rdf:Bag>
                      <rdf:li rdf:parseType="Resource" mwg-rs:Type="Face">
                        <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.3" stArea:y="0.4" stArea:w="0.2" stArea:h="0.3"/>
                      </rdf:li>
                      <rdf:li rdf:parseType="Resource" mwg-rs:Type="Face"{rotation}>
                        <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.6" stArea:y="0.4" stArea:w="0.2" stArea:h="0.3"/>
                      </rdf:li>
                    </rdf:Bag></mwg-rs:RegionList>
                  </mwg-rs:Regions>
                </rdf:Description></rdf:RDF>"#
            )
        };
        let dims = r#"<mwg-rs:AppliedToDimensions rdf:parseType="Resource" stDim:w="6000" stDim:h="4000" stDim:unit="pixel"/>"#;
        let drawn_on = |x: &str| parse_xmp(x).unwrap().regions_drawn_on();
        assert_eq!(drawn_on(&packet(dims, "")), Some((6000.0, 4000.0)));
        assert_eq!(drawn_on(&packet(dims, r#" mwg-rs:Rotation="-1.57080""#)), None, "one box turned by Lightroom");
        assert_eq!(drawn_on(&packet("", "")), None, "no AppliedToDimensions");
        assert_eq!(drawn_on(r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about=""/></rdf:RDF>"#), None);
    }

    /// A second, differently-shaped encoding: each region is its own `rdf:Description` (rather than an
    /// `rdf:li rdf:parseType="Resource"`), `mwg-rs:Type` as an attribute instead of an element, and an
    /// unrecognised `mwg-rs:Type` value kept verbatim.
    #[test]
    fn mwg_regions_attribute_form_and_unknown_type() {
        let x = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about=""
                xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
                xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
              <mwg-rs:Regions rdf:parseType="Resource">
                <mwg-rs:RegionList>
                  <rdf:Bag>
                    <rdf:li>
                      <rdf:Description mwg-rs:Type="BarCode">
                        <mwg-rs:Area stArea:x="0.3" stArea:y="0.3" stArea:w="0.1" stArea:h="0.1"/>
                      </rdf:Description>
                    </rdf:li>
                    <rdf:li>
                      <rdf:Description mwg-rs:Type="Sticker">
                        <mwg-rs:Area stArea:x="0.6" stArea:y="0.6" stArea:w="0.1" stArea:h="0.1"/>
                      </rdf:Description>
                    </rdf:li>
                  </rdf:Bag>
                </mwg-rs:RegionList>
              </mwg-rs:Regions>
            </rdf:Description></rdf:RDF>"#;
        let regions = parse_xmp(x).unwrap().metadata.regions;
        assert_eq!(regions.len(), 2, "{regions:?}");
        assert_eq!(regions[0].kind, RegionKind::BarCode);
        assert_eq!(regions[1].kind, RegionKind::Other("Sticker".to_string()));
    }

    /// Hostile/malformed regions are dropped individually, never panic, and never poison the rest of
    /// the list: missing Area, non-finite or absurd numbers, zero/negative size, and a pixel-unit area
    /// with no `AppliedToDimensions` to convert it (so it can't be stored as a nonsense fraction).
    #[test]
    fn mwg_regions_hostile_inputs_are_dropped_not_panicking() {
        let x = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about=""
                xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
                xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
              <mwg-rs:Regions rdf:parseType="Resource">
                <mwg-rs:RegionList>
                  <rdf:Bag>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Name>No Area</mwg-rs:Name></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="NaN" stArea:y="0.1" stArea:w="0.1" stArea:h="0.1"/></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="1e300" stArea:y="0.1" stArea:w="0.1" stArea:h="0.1"/></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="0.5" stArea:y="0.5" stArea:w="0" stArea:h="0.1"/></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="0.5" stArea:y="0.5" stArea:w="-0.1" stArea:h="0.1"/></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="100" stArea:y="100" stArea:w="50" stArea:h="50" stArea:unit="pixel"/></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="0.5" stArea:y="0.5" stArea:w="0.2" stArea:h="0.2"/><mwg-rs:Type>Face</mwg-rs:Type></rdf:li>
                  </rdf:Bag>
                </mwg-rs:RegionList>
              </mwg-rs:Regions>
            </rdf:Description></rdf:RDF>"#;
        let regions = parse_xmp(x).unwrap().metadata.regions;
        assert_eq!(regions.len(), 1, "only the last, well-formed region should survive: {regions:?}");
        assert_eq!(regions[0].kind, RegionKind::Face);
    }

    /// Regions are clipped to the photo frame, and ones that miss the photo entirely are dropped: a
    /// hostile sidecar can't park a box at x=1e5 (or a 1e5-wide one) that the loupe or the People view
    /// would then draw or crop far outside the image.
    #[test]
    fn mwg_regions_are_clipped_to_the_frame_and_off_photo_ones_dropped() {
        let x = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about=""
                xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
                xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
              <mwg-rs:Regions rdf:parseType="Resource">
                <mwg-rs:RegionList>
                  <rdf:Bag>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="1e5" stArea:y="0.5" stArea:w="0.1" stArea:h="0.1"/><mwg-rs:Name>Far right</mwg-rs:Name></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="-3" stArea:y="-3" stArea:w="0.5" stArea:h="0.5"/><mwg-rs:Name>Far left</mwg-rs:Name></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="1.2" stArea:y="0.5" stArea:w="0.2" stArea:h="0.2"/><mwg-rs:Name>Touching edge</mwg-rs:Name></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="0.5" stArea:y="0.5" stArea:w="1e5" stArea:h="1e5"/><mwg-rs:Name>Huge</mwg-rs:Name></rdf:li>
                    <rdf:li rdf:parseType="Resource"><mwg-rs:Area rdf:parseType="Resource" stArea:x="0.95" stArea:y="0.05" stArea:w="0.2" stArea:h="0.2"/><mwg-rs:Name>Corner</mwg-rs:Name></rdf:li>
                  </rdf:Bag>
                </mwg-rs:RegionList>
              </mwg-rs:Regions>
            </rdf:Description></rdf:RDF>"#;
        let regions = parse_xmp(x).unwrap().metadata.regions;
        let names: Vec<_> = regions.iter().map(|r| r.name.as_deref().unwrap_or("")).collect();
        assert_eq!(names, ["Huge", "Corner"], "{regions:?}");
        for r in &regions {
            let q = r.rect;
            assert!(q.x0 >= 0.0 && q.y0 >= 0.0 && q.x1 <= 1.0 && q.y1 <= 1.0 && !q.is_empty(), "{q:?}");
        }
        assert_eq!(regions[0].rect, Rect::UNIT);
        let c = regions[1].rect;
        assert!((c.x0 - 0.85).abs() < 1e-9 && c.x1 == 1.0 && c.y0 == 0.0 && (c.y1 - 0.15).abs() < 1e-9, "{c:?}");
    }

    /// `extract()`'s precedence (EXIF/IPTC never carry regions; XMP is read and filled in like keywords).
    #[test]
    fn regions_merge_like_keywords() {
        let with_region = Region { rect: Rect::from_center(Point::new(0.5, 0.5), 0.2, 0.2), kind: RegionKind::Face, name: None, description: None };
        let mut a = Metadata::default();
        let b = Metadata { regions: vec![with_region.clone()], ..Default::default() };
        a.fill_missing(&b);
        assert_eq!(a.regions, vec![with_region.clone()]);

        let mut a = Metadata::default();
        a.overlay_user_fields(&b);
        assert_eq!(a.regions, vec![with_region]);
    }

    #[test]
    fn plugin_fields_read_back_under_any_prefix() {
        let x = write_xmp_ext(&Metadata::default(), &[], &[("com.ex.jobs", "job", "J-1 <&>"), ("com.ex.jobs", "ok", "true")]);
        let d = parse_xmp(&x).unwrap();
        assert_eq!(
            plugin_fields(&d),
            vec![("com.ex.jobs".into(), "job".into(), "J-1 <&>".into()), ("com.ex.jobs".into(), "ok".into(), "true".into())]
        );
        // another prefix for the same namespace; an item without a key is skipped
        let ns = plugin_fields_ns();
        let y = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:pf="{ns}"><pf:fields><rdf:Bag><rdf:li rdf:parseType="Resource"><pf:plugin>a</pf:plugin><pf:key>k</pf:key><pf:value>v</pf:value></rdf:li><rdf:li rdf:parseType="Resource"><pf:plugin>a</pf:plugin></rdf:li></rdf:Bag></pf:fields></rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        assert_eq!(plugin_fields(&parse_xmp(&y).unwrap()), vec![("a".into(), "k".into(), "v".into())]);
        assert!(plugin_fields(&parse_xmp(&write_xmp_ext(&Metadata::default(), &[], &[])).unwrap()).is_empty());
    }

    #[test]
    fn writer_drops_characters_xml_1_0_forbids() {
        let bad = "a\u{1}b\u{0}c\u{FFFE}d\u{FFFF}e\u{1F}\tf\ng\rh";
        let m = Metadata { make: Some(bad.into()), caption: Some(bad.into()), keywords: vec![bad.into()], ..Default::default() };
        let x = write_xmp_lc(&m, &[("settings", bad)]);
        let illegal: Vec<char> =
            x.chars().filter(|&c| (c < '\u{20}' && !matches!(c, '\t' | '\n' | '\r')) || c == '\u{FFFE}' || c == '\u{FFFF}').collect();
        assert!(illegal.is_empty(), "illegal XML chars in output: {illegal:?}");
        assert!(x.contains(">abcde\tf\ng\rh<"), "allowed characters must survive: {x}");
    }

    #[test]
    fn numeric_references_to_forbidden_characters_are_not_decoded() {
        assert_eq!(entity("#1"), None);
        assert_eq!(entity("#0"), None);
        assert_eq!(entity("#xFFFE"), None);
        assert_eq!(entity("#x41"), Some('A'));
        assert_eq!(entity("#9"), Some('\t'));
    }

    #[test]
    fn entity_rejects_signed_numeric_references() {
        assert_eq!(entity("#+65"), None);
        assert_eq!(entity("#x+41"), None);
        assert_eq!(entity("#65"), Some('A'));
        assert_eq!(entity("#x41"), Some('A'));
    }

    /// Flattened keys repeat their whole ancestor path, so a long nested name with a wide child
    /// fan-out would otherwise retain its prefix once per leaf: the cumulative key budget stops it
    /// (a 65 KB document with 3000 leaves must not retain ~2.4 MB of key text).
    #[test]
    fn flattened_keys_stay_within_the_cumulative_budget() {
        let level = "n".repeat(200);
        let mut doc = String::from(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:lc=\"https://example.invalid/lc/\"><rdf:RDF><rdf:Description><lc:root rdf:parseType=\"Resource\">",
        );
        for _ in 0..4 {
            doc.push_str(&format!("<lc:{level} rdf:parseType=\"Resource\">"));
        }
        // each leaf has a name of its own, so each one adds a key
        for i in 0..3000 {
            doc.push_str(&format!("<lc:leaf{i} lc:att=\"v\"/>"));
        }
        doc.push_str("</lc:root>");
        for _ in 0..4 {
            doc.push_str(&format!("</lc:{level}>"));
        }
        doc.push_str("</rdf:Description></rdf:RDF></x:xmpmeta>");
        let d = parse_xmp(&doc).unwrap();
        let total: usize = d.properties.keys().map(String::len).sum();
        assert!(total <= MAX_KEY_BYTES, "flattened-key text {total} is over the {MAX_KEY_BYTES} budget");
        assert!(d.properties.len() < 3000, "keys stop being added once the budget is spent");
        assert!(!d.properties.is_empty(), "properties parsed before the budget are kept");
        assert!(d.properties_truncated, "omitted flattened keys are reported to callers");
    }

    #[test]
    fn a_start_tag_with_thousands_of_namespace_declarations_is_refused() {
        let decls: String = (0..2000).map(|i| format!(" xmlns:p{i}=\"urn:x{i}\"")).collect();
        let xmp = format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description{decls} p1:a=\"1\"/></rdf:RDF></x:xmpmeta>"
        );
        assert!(matches!(parse_xmp(&xmp), Err(XmpError::TooComplex)));
    }
}
