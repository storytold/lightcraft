//! Token text for text cells, captions, page info and identity plates.
//!
//! The tokens are the export/rename naming tokens (`{name}`, `{date}`, `{date:%Y-%m-%d}`,
//! `{camera}`, `{lens}`, `{iso}`, `{rating}`, `{title}`, `{creator}`, `{ext}`, `{folder}`,
//! `{seq}`) plus layout ones: `{filename}` (with extension), `{caption}`, `{headline}`,
//! `{exposure}` (`1/250 s at f/8`), `{shutter}`, `{aperture}`, `{focal}`, `{copyright}`,
//! `{keywords}`, `{dimensions}`, `{page}`, `{pages}`. Names are case-insensitive (`{Title}` and
//! `{title}` are the same); unknown tokens stay as typed, a missing value expands to nothing.
//! `{{` and `}}` write literal braces.
//!
//! The engine fills a [`PhotoInfo`] from the catalog; this crate sits below the engine, so it has
//! its own expander rather than calling the rename module.

use serde::{Deserialize, Serialize};

/// What tokens can say about a photo and its page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotoInfo {
    /// File name with extension.
    pub filename: String,
    pub folder: String,
    /// Capture date-time, ISO 8601 (`2024-05-01T13:45:10`); may be just a date.
    pub date: String,
    pub title: String,
    pub caption: String,
    pub headline: String,
    pub creator: String,
    pub copyright: String,
    pub camera: String,
    pub lens: String,
    pub iso: Option<u32>,
    /// Exposure time in seconds.
    pub shutter: Option<f64>,
    pub aperture: Option<f64>,
    /// Focal length in mm.
    pub focal: Option<f64>,
    pub rating: Option<u8>,
    pub keywords: Vec<String>,
    /// Pixel dimensions.
    pub dimensions: Option<(u32, u32)>,
    /// Sequence number in the job (1-based).
    pub seq: usize,
    /// Page number (1-based) and page count.
    pub page: usize,
    pub pages: usize,
}

/// `1/250 s`, `2 s`, `0.5 s`.
pub fn format_shutter(t: f64) -> String {
    if !(t.is_finite() && t > 0.0) {
        return String::new();
    }
    if t < 0.5 {
        format!("1/{} s", (1.0 / t).round())
    } else if (t - t.round()).abs() < 1e-6 {
        format!("{} s", t.round())
    } else {
        format!("{t:.1} s")
    }
}

fn format_f(v: f64) -> String {
    if (v - v.round()).abs() < 1e-6 { format!("{}", v.round()) } else { format!("{v:.1}") }
}

fn stem_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (name.get(..i).unwrap_or(name), name.get(i + 1..).unwrap_or("")),
        _ => (name, ""),
    }
}

/// `{date:%Y-%m-%d}` on an ISO date (`%Y %y %m %d %H %M %S`; missing parts are empty).
fn format_date(iso: &str, fmt: &str) -> String {
    let digits = |a: usize, b: usize| iso.get(a..b).filter(|s| s.bytes().all(|c| c.is_ascii_digit())).unwrap_or("");
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(digits(0, 4)),
            Some('y') => out.push_str(digits(2, 4)),
            Some('m') => out.push_str(digits(5, 7)),
            Some('d') => out.push_str(digits(8, 10)),
            Some('H') => out.push_str(digits(11, 13)),
            Some('M') => out.push_str(digits(14, 16)),
            Some('S') => out.push_str(digits(17, 19)),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

/// The value of one token (`None` = unknown token).
fn value(token: &str, p: &PhotoInfo) -> Option<String> {
    let (name, arg) = match token.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (token, None),
    };
    let (stem, ext) = stem_ext(&p.filename);
    let num = |v: Option<u32>| v.map(|v| v.to_string()).unwrap_or_default();
    Some(match name.to_ascii_lowercase().as_str() {
        "name" => stem.to_string(),
        "filename" => p.filename.clone(),
        "ext" => ext.to_string(),
        "folder" => p.folder.clone(),
        "date" => match arg {
            Some(f) => format_date(&p.date, f),
            None => format_date(&p.date, "%Y-%m-%d"),
        },
        "title" => p.title.clone(),
        "caption" => p.caption.clone(),
        "headline" => p.headline.clone(),
        "creator" => p.creator.clone(),
        "copyright" => p.copyright.clone(),
        "camera" => p.camera.clone(),
        "lens" => p.lens.clone(),
        "iso" => p.iso.map(|v| format!("ISO {v}")).unwrap_or_default(),
        "shutter" => p.shutter.map(format_shutter).unwrap_or_default(),
        "aperture" => p.aperture.filter(|v| v.is_finite() && *v > 0.0).map(|v| format!("f/{}", format_f(v))).unwrap_or_default(),
        "focal" => p.focal.filter(|v| v.is_finite() && *v > 0.0).map(|v| format!("{} mm", format_f(v))).unwrap_or_default(),
        "exposure" => {
            let s = p.shutter.map(format_shutter).unwrap_or_default();
            let a = p.aperture.filter(|v| v.is_finite() && *v > 0.0).map(|v| format!("f/{}", format_f(v)));
            match (s.is_empty(), a) {
                (false, Some(a)) => format!("{s} at {a}"),
                (false, None) => s,
                (true, Some(a)) => a,
                (true, None) => String::new(),
            }
        }
        "rating" => p.rating.map(|r| "★".repeat(r.min(5) as usize)).unwrap_or_default(),
        "keywords" => p.keywords.join(", "),
        "dimensions" => p.dimensions.map(|(w, h)| format!("{w} × {h}")).unwrap_or_default(),
        "seq" => {
            let width = arg.and_then(|a| a.parse::<usize>().ok()).unwrap_or(3).min(12);
            format!("{:0width$}", p.seq)
        }
        "page" => num(u32::try_from(p.page).ok()),
        "pages" => num(u32::try_from(p.pages).ok()),
        _ => return None,
    })
}

/// Expands every token of `template` against `p`.
pub fn expand(template: &str, p: &PhotoInfo) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("{{") {
            out.push('{');
            rest = r;
        } else if let Some(r) = rest.strip_prefix("}}") {
            out.push('}');
            rest = r;
        } else if let Some(r) = rest.strip_prefix('{')
            && let Some(end) = r.find('}')
            && let Some(tok) = r.get(..end)
            && !tok.contains('{')
        {
            match value(tok, p) {
                Some(v) => out.push_str(&v),
                None => {
                    out.push('{');
                    out.push_str(tok);
                    out.push('}');
                }
            }
            rest = r.get(end + 1..).unwrap_or("");
        } else {
            let mut it = rest.chars();
            if let Some(c) = it.next() {
                out.push(c);
            }
            rest = it.as_str();
        }
    }
    out
}

/// The token names a UI offers, as `{Name}`.
pub const TOKENS: &[&str] = &[
    "{Title}",
    "{Caption}",
    "{Headline}",
    "{Filename}",
    "{Name}",
    "{Folder}",
    "{Date}",
    "{Exposure}",
    "{Shutter}",
    "{Aperture}",
    "{Focal}",
    "{ISO}",
    "{Camera}",
    "{Lens}",
    "{Creator}",
    "{Copyright}",
    "{Rating}",
    "{Keywords}",
    "{Dimensions}",
    "{Seq}",
    "{Page}",
    "{Pages}",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> PhotoInfo {
        PhotoInfo {
            filename: "IMG_0042.CR3".into(),
            date: "2024-05-01T13:45:10".into(),
            title: "Harbour".into(),
            iso: Some(200),
            shutter: Some(1.0 / 250.0),
            aperture: Some(8.0),
            focal: Some(35.0),
            rating: Some(3),
            seq: 7,
            page: 2,
            pages: 9,
            ..Default::default()
        }
    }

    #[test]
    fn expands_layout_and_naming_tokens() {
        let p = info();
        assert_eq!(expand("{Title} — {Date}", &p), "Harbour — 2024-05-01");
        assert_eq!(expand("{Exposure}, {ISO}, {Focal}", &p), "1/250 s at f/8, ISO 200, 35 mm");
        assert_eq!(expand("{name}.{ext} {date:%Y%m%d} {seq} {seq:5}", &p), "IMG_0042.CR3 20240501 007 00007");
        assert_eq!(expand("{Filename} {Rating} {Page}/{Pages}", &p), "IMG_0042.CR3 ★★★ 2/9");
    }

    #[test]
    fn unknown_missing_and_literal() {
        let p = PhotoInfo::default();
        assert_eq!(expand("{Nope} {Title}{{x}} {", &p), "{Nope} {x} {");
        assert_eq!(expand("{date:%Y-%Q}", &PhotoInfo { date: "20".into(), ..p.clone() }), "-%Q");
        assert_eq!(expand("é{Title}ü{", &p), "éü{");
        assert_eq!(format_shutter(2.0), "2 s");
        assert_eq!(format_shutter(f64::NAN), "");
        assert_eq!(expand("{seq:99999999999999999999}", &p), "000");
    }
}
