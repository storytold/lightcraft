//! The static site: `index.html`, one page per photo under `content/`, `assets/gallery.css`,
//! `assets/gallery.js`, and the images the caller renders under `images/`.
//!
//! Every page works without JavaScript (thumbnails link to the photo pages, which link to each
//! other); the script adds the in-page viewer (Grid / Square), the stage swap (Track) and keyboard
//! navigation (← → Home End, Escape closes the viewer). Nothing is loaded from another host.

use std::collections::BTreeMap;

use crate::escape;
use crate::settings::{GallerySettings, Template};
use crate::tokens::expand;

/// Most photos in one gallery (a cap on what a request can make us write).
pub const MAX_PHOTOS: usize = 10_000;

/// One photo going into the gallery.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GalleryPhoto {
    /// Metadata fields for the caption tokens (`title`, `caption`, `filename`, …).
    pub fields: BTreeMap<String, String>,
    /// The photo's (cropped) aspect ratio, width / height; used for layout boxes before load.
    pub aspect: f32,
}

/// An image the caller must render and write at `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRequest {
    /// Index into the photos passed to [`generate`].
    pub photo: usize,
    /// Site-relative path, e.g. `images/large/0001.jpg`.
    pub path: String,
    /// Longest edge in pixels.
    pub long_edge: u32,
    /// The large image (watermark applies) rather than a thumbnail.
    pub large: bool,
}

/// One generated text file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// A generated site: the text files, plus the images still to render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub files: Vec<SiteFile>,
    pub images: Vec<ImageRequest>,
}

struct Entry {
    title: String,
    caption: String,
    alt: String,
    large: String,
    thumb: String,
    aspect: f32,
}

/// Generates the site for `photos` with `settings`.
pub fn generate(settings: &GallerySettings, photos: &[GalleryPhoto]) -> Result<Site, String> {
    if photos.is_empty() {
        return Err("the web gallery has no photos: select or filter some photos first".into());
    }
    if photos.len() > MAX_PHOTOS {
        return Err(format!("a web gallery holds at most {MAX_PHOTOS} photos ({} selected)", photos.len()));
    }
    let s = settings.sanitized();
    let mut images = Vec::with_capacity(photos.len() * 2);
    let entries: Vec<Entry> = photos
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let n = i.saturating_add(1);
            let large = format!("images/large/{n:04}.jpg");
            let thumb = format!("images/thumb/{n:04}.jpg");
            images.push(ImageRequest { photo: i, path: large.clone(), long_edge: s.output.large_size, large: true });
            images.push(ImageRequest { photo: i, path: thumb.clone(), long_edge: s.appearance.thumb_size.saturating_mul(2), large: false });
            let title = expand(&s.image_info.title, &p.fields);
            let caption = expand(&s.image_info.caption, &p.fields);
            let alt = [title.as_str(), caption.as_str()].iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join(". ");
            let alt = if alt.is_empty() { p.fields.get("filename").cloned().unwrap_or_else(|| format!("Photo {n}")) } else { alt };
            let aspect = if p.aspect.is_finite() && p.aspect > 0.01 { p.aspect.min(100.0) } else { 1.5 };
            Entry { title, caption, alt, large, thumb, aspect }
        })
        .collect();

    let mut files = vec![
        SiteFile { path: "assets/gallery.css".into(), bytes: css(&s).into_bytes() },
        SiteFile { path: "assets/gallery.js".into(), bytes: SCRIPT.as_bytes().to_vec() },
        SiteFile { path: "index.html".into(), bytes: index(&s, &entries).into_bytes() },
    ];
    for i in 0..entries.len() {
        files.push(SiteFile { path: page_path(i), bytes: photo_page(&s, &entries, i, "../").into_bytes() });
    }
    Ok(Site { files, images })
}

fn page_path(i: usize) -> String {
    format!("content/{:04}.html", i.saturating_add(1))
}

fn head(s: &GallerySettings, root: &str, title: &str) -> String {
    let t = if title.is_empty() { s.site.title.clone() } else { format!("{title} · {}", s.site.title) };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<link rel=\"stylesheet\" href=\"{root}assets/gallery.css\">\n</head>\n",
        escape(t.trim_end_matches(" · "))
    )
}

fn header(s: &GallerySettings, root: &str) -> String {
    let mut h = String::from("<header class=\"site\">\n");
    if !s.site.title.is_empty() {
        h.push_str(&format!("<h1><a href=\"{root}index.html\">{}</a></h1>\n", escape(&s.site.title)));
    }
    if !s.site.collection_title.is_empty() {
        h.push_str(&format!("<h2>{}</h2>\n", escape(&s.site.collection_title)));
    }
    if !s.site.description.is_empty() && root.is_empty() {
        h.push_str(&format!("<p class=\"description\">{}</p>\n", escape(&s.site.description)));
    }
    h.push_str("</header>\n");
    h
}

/// `https://…` stays, a bare mail address gets `mailto:`, anything else (e.g. `javascript:`) is dropped.
fn link_href(link: &str) -> Option<String> {
    let l = link.trim();
    let lower = l.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:") {
        Some(l.to_string())
    } else if l.contains('@') && !l.contains(':') && !l.contains(char::is_whitespace) {
        Some(format!("mailto:{l}"))
    } else if l.contains('.') && !l.contains(':') && !l.contains(char::is_whitespace) && !l.is_empty() {
        Some(format!("https://{l}"))
    } else {
        None
    }
}

fn footer(s: &GallerySettings) -> String {
    let contact = escape(&s.site.contact);
    let body = match (link_href(&s.site.link), contact.is_empty()) {
        (Some(h), false) => format!("<a href=\"{}\">{contact}</a>", escape(&h)),
        (Some(h), true) => format!("<a href=\"{}\">{}</a>", escape(&h), escape(s.site.link.trim())),
        (None, false) => contact,
        (None, true) => String::new(),
    };
    if body.is_empty() { String::new() } else { format!("<footer class=\"site\">{body}</footer>\n") }
}

fn data_json(entries: &[Entry]) -> String {
    let list: Vec<serde_json::Value> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| serde_json::json!({"src": e.large, "title": e.title, "caption": e.caption, "alt": e.alt, "page": page_path(i)}))
        .collect();
    // `<` escaped so a caption can't close the script element
    serde_json::Value::Array(list).to_string().replace('<', "\\u003c")
}

fn figure_caption(e: &Entry) -> String {
    let mut c = String::new();
    if !e.title.is_empty() {
        c.push_str(&format!("<span class=\"title\">{}</span>", escape(&e.title)));
    }
    if !e.caption.is_empty() {
        c.push_str(&format!("<span class=\"caption\">{}</span>", escape(&e.caption)));
    }
    c
}

fn index(s: &GallerySettings, entries: &[Entry]) -> String {
    if s.template == Template::Single {
        return photo_page(s, entries, 0, "");
    }
    let mut h = head(s, "", "");
    h.push_str(&format!("<body class=\"t-{}\">\n", s.template.key()));
    h.push_str(&header(s, ""));
    let a = &s.appearance;
    match s.template {
        Template::Track => {
            let first = entries.first();
            h.push_str("<main>\n<figure id=\"stage\" class=\"stage\">\n");
            if let Some(e) = first {
                h.push_str(&format!(
                    "<img id=\"stage-img\" src=\"{}\" alt=\"{}\">\n<figcaption id=\"stage-cap\">{}</figcaption>\n",
                    escape(&e.large),
                    escape(&e.alt),
                    figure_caption(e)
                ));
            }
            h.push_str("</figure>\n<nav class=\"track\" aria-label=\"Photos\">\n");
        }
        _ => h.push_str("<main>\n<ul class=\"grid\" aria-label=\"Photos\">\n"),
    }
    for (i, e) in entries.iter().enumerate() {
        let num = if a.cell_numbers { format!("<span class=\"num\">{}</span>", i.saturating_add(1)) } else { String::new() };
        let cap = if a.thumb_captions && s.template != Template::Track {
            format!("<span class=\"cell-cap\">{}</span>", figure_caption(e))
        } else {
            String::new()
        };
        let img = format!("<img src=\"{}\" alt=\"{}\" loading=\"lazy\" style=\"aspect-ratio:{:.4}\">", escape(&e.thumb), escape(&e.alt), e.aspect);
        let link = format!("<a class=\"cell\" href=\"{}\" data-i=\"{i}\">{num}{img}{cap}</a>", page_path(i));
        if s.template == Template::Track {
            h.push_str(&link);
            h.push('\n');
        } else {
            h.push_str(&format!("<li>{link}</li>\n"));
        }
    }
    h.push_str(if s.template == Template::Track { "</nav>\n</main>\n" } else { "</ul>\n</main>\n" });
    h.push_str(&footer(s));
    h.push_str(&format!("<script type=\"application/json\" id=\"gallery-data\">{}</script>\n", data_json(entries)));
    h.push_str("<script src=\"assets/gallery.js\"></script>\n</body>\n</html>\n");
    h
}

/// A photo's own page; `root` is the path back to the site root (`../` under `content/`).
fn photo_page(s: &GallerySettings, entries: &[Entry], i: usize, root: &str) -> String {
    let Some(e) = entries.get(i) else { return String::new() };
    let mut h = head(s, root, &e.title);
    h.push_str("<body class=\"t-page\">\n");
    h.push_str(&header(s, root));
    let here = if root.is_empty() { "content/" } else { "" };
    let rel = |j: usize| format!("{here}{:04}.html", j.saturating_add(1));
    let n = entries.len();
    let prev = if i > 0 { format!("<a rel=\"prev\" href=\"{}\">&larr; Previous</a>", rel(i - 1)) } else { "<span></span>".into() };
    let next = if i + 1 < n { format!("<a rel=\"next\" href=\"{}\">Next &rarr;</a>", rel(i + 1)) } else { "<span></span>".into() };
    let up =
        if s.template == Template::Single { "<span></span>".to_string() } else { format!("<a rel=\"index\" href=\"{root}index.html\">Index</a>") };
    h.push_str(&format!(
        "<main>\n<nav class=\"pager\">{prev}<span class=\"count\">{} / {n}</span>{up}{next}</nav>\n<figure class=\"photo\">\n<img src=\"{root}{}\" alt=\"{}\">\n<figcaption>{}</figcaption>\n</figure>\n</main>\n",
        i.saturating_add(1),
        escape(&e.large),
        escape(&e.alt),
        figure_caption(e)
    ));
    h.push_str(&footer(s));
    h.push_str(&format!("<script src=\"{root}assets/gallery.js\"></script>\n</body>\n</html>\n"));
    h
}

fn css(s: &GallerySettings) -> String {
    let p = &s.palette;
    let a = &s.appearance;
    let border = if a.photo_borders { format!("{}px solid var(--border)", a.border_width) } else { "none".into() };
    let grid_cols = match s.template {
        Template::Square => format!("repeat({}, minmax(0, 1fr))", a.columns),
        _ => format!("repeat(auto-fill, minmax({}px, 1fr))", a.thumb_size.saturating_mul(3) / 4),
    };
    let fit = if s.template == Template::Square { "aspect-ratio: 1 / 1 !important; object-fit: cover;" } else { "object-fit: contain;" };
    format!(
        r#":root {{
  --bg: {bg}; --text: {text}; --detail: {detail}; --cell: {cell}; --cell-hover: {hover};
  --border: {borderc}; --accent: {accent}; --thumb: {thumb}px; --page: {page}px;
}}
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; background: var(--bg); color: var(--text);
  font: 16px/1.45 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }}
a {{ color: var(--accent); }}
a:focus-visible, button:focus-visible {{ outline: 2px solid var(--accent); outline-offset: 2px; }}
header.site, footer.site {{ padding: 16px max(16px, 4vw); }}
header.site h1 {{ margin: 0; font-size: 1.6rem; font-weight: 600; }}
header.site h1 a {{ color: var(--text); text-decoration: none; }}
header.site h2 {{ margin: 4px 0 0; font-size: 1.05rem; font-weight: 400; color: var(--detail); }}
.description {{ max-width: 70ch; color: var(--detail); }}
footer.site {{ color: var(--detail); font-size: .9rem; }}
main {{ padding: 0 max(16px, 4vw) 24px; }}
ul.grid {{ list-style: none; margin: 0; padding: 0; display: grid; gap: 12px; grid-template-columns: {cols}; }}
.cell {{ position: relative; display: flex; flex-direction: column; align-items: center; justify-content: center;
  background: var(--cell); padding: 8px; text-decoration: none; color: var(--text); height: 100%; }}
.cell:hover, .cell:focus-visible {{ background: var(--cell-hover); }}
.cell img {{ display: block; width: 100%; height: auto; max-height: var(--thumb); border: {border}; {fit} }}
.num {{ position: absolute; top: 4px; left: 8px; font-size: .75rem; color: var(--detail); }}
.cell-cap {{ margin-top: 6px; font-size: .85rem; text-align: center; }}
.cell-cap span, figcaption span {{ display: block; }}
.title {{ font-weight: 600; }}
.caption {{ color: var(--detail); }}
.stage, .photo {{ margin: 0 auto 16px; text-align: center; }}
.stage img, .photo img {{ max-width: min(100%, var(--page)); max-height: min(80vh, var(--page)); width: auto; height: auto;
  border: {border}; }}
figcaption {{ margin-top: 8px; }}
nav.track {{ display: flex; gap: 8px; overflow-x: auto; padding-bottom: 8px; scroll-snap-type: x proximity; }}
nav.track .cell {{ flex: 0 0 auto; width: calc(var(--thumb) * .6); scroll-snap-align: start; }}
nav.track .cell[aria-current="true"] {{ outline: 2px solid var(--accent); }}
nav.pager {{ display: flex; justify-content: space-between; align-items: center; gap: 12px; max-width: var(--page);
  margin: 0 auto 12px; }}
.count {{ color: var(--detail); }}
.viewer {{ position: fixed; inset: 0; background: color-mix(in srgb, var(--bg) 94%, transparent); display: flex;
  flex-direction: column; align-items: center; justify-content: center; padding: 16px; z-index: 10; }}
.viewer[hidden] {{ display: none; }}
.viewer img {{ max-width: min(100%, var(--page)); max-height: 78vh; border: {border}; }}
.viewer button {{ background: var(--cell); color: var(--text); border: 1px solid var(--border); padding: 6px 14px;
  font: inherit; cursor: pointer; }}
.viewer .bar {{ display: flex; gap: 8px; margin-top: 12px; }}
@media (max-width: 600px) {{
  ul.grid {{ grid-template-columns: repeat({small_cols}, minmax(0, 1fr)); gap: 6px; }}
  .cell {{ padding: 4px; }}
}}
"#,
        bg = p.background,
        text = p.text,
        detail = p.detail_text,
        cell = p.cell,
        hover = p.cell_hover,
        borderc = p.border,
        accent = p.accent,
        thumb = a.thumb_size,
        page = a.image_page_size,
        cols = grid_cols,
        small_cols = a.columns.clamp(2, 3),
    )
}

/// The gallery script (own work): viewer, track stage, keyboard navigation.
const SCRIPT: &str = r#"(function () {
  "use strict";
  var dataEl = document.getElementById("gallery-data");
  if (!dataEl) {
    // photo page: arrow keys follow the pager links
    document.addEventListener("keydown", function (e) {
      if (e.altKey || e.ctrlKey || e.metaKey) return;
      var rel = { ArrowLeft: "prev", ArrowRight: "next", Escape: "index" }[e.key];
      var a = rel && document.querySelector('a[rel="' + rel + '"]');
      if (a) { location.href = a.href; }
    });
    return;
  }
  var photos = [];
  try { photos = JSON.parse(dataEl.textContent) || []; } catch (err) { return; }
  var cells = Array.prototype.slice.call(document.querySelectorAll("a.cell"));
  var current = 0;
  var track = document.body.classList.contains("t-track");
  function caption(p) {
    var f = document.createDocumentFragment();
    [["title", p.title], ["caption", p.caption]].forEach(function (x) {
      if (!x[1]) return;
      var s = document.createElement("span"); s.className = x[0]; s.textContent = x[1]; f.appendChild(s);
    });
    return f;
  }
  if (track) {
    var img = document.getElementById("stage-img"), cap = document.getElementById("stage-cap");
    var show = function (i) {
      if (!photos.length || !img) return;
      current = (i + photos.length) % photos.length;
      var p = photos[current];
      img.src = p.src; img.alt = p.alt;
      cap.textContent = ""; cap.appendChild(caption(p));
      cells.forEach(function (c, j) { c.setAttribute("aria-current", j === current ? "true" : "false"); });
      if (cells[current] && cells[current].scrollIntoView) cells[current].scrollIntoView({ block: "nearest", inline: "nearest" });
    };
    cells.forEach(function (c, j) { c.addEventListener("click", function (e) { e.preventDefault(); show(j); }); });
    document.addEventListener("keydown", function (e) {
      if (e.key === "ArrowRight") show(current + 1);
      else if (e.key === "ArrowLeft") show(current - 1);
      else if (e.key === "Home") show(0);
      else if (e.key === "End") show(photos.length - 1);
      else return;
      e.preventDefault();
    });
    show(0);
    return;
  }
  var v = document.createElement("div");
  v.className = "viewer"; v.hidden = true;
  v.setAttribute("role", "dialog"); v.setAttribute("aria-modal", "true"); v.setAttribute("aria-label", "Photo viewer");
  var vimg = document.createElement("img");
  var vcap = document.createElement("figcaption");
  var bar = document.createElement("div"); bar.className = "bar";
  function button(label, fn) {
    var b = document.createElement("button"); b.type = "button"; b.textContent = label;
    b.addEventListener("click", fn); bar.appendChild(b); return b;
  }
  button("← Previous", function () { open(current - 1); });
  var close = button("Close", function () { hide(); });
  button("Next →", function () { open(current + 1); });
  v.appendChild(vimg); v.appendChild(vcap); v.appendChild(bar);
  document.body.appendChild(v);
  function open(i) {
    if (!photos.length) return;
    current = (i + photos.length) % photos.length;
    var p = photos[current];
    vimg.src = p.src; vimg.alt = p.alt;
    vcap.textContent = ""; vcap.appendChild(caption(p));
    if (v.hidden) { v.hidden = false; close.focus(); }
  }
  function hide() { v.hidden = true; if (cells[current]) cells[current].focus(); }
  v.addEventListener("click", function (e) { if (e.target === v) hide(); });
  cells.forEach(function (c, j) { c.addEventListener("click", function (e) { e.preventDefault(); open(j); }); });
  document.addEventListener("keydown", function (e) {
    if (v.hidden) {
      var at = cells.indexOf(document.activeElement);
      if (at < 0) return;
      if (e.key === "ArrowRight" && cells[at + 1]) cells[at + 1].focus();
      else if (e.key === "ArrowLeft" && cells[at - 1]) cells[at - 1].focus();
      else if (e.key === "Home") cells[0].focus();
      else if (e.key === "End") cells[cells.length - 1].focus();
      else return;
      e.preventDefault();
      return;
    }
    if (e.key === "Escape") hide();
    else if (e.key === "ArrowRight") open(current + 1);
    else if (e.key === "ArrowLeft") open(current - 1);
    else if (e.key === "Home") open(0);
    else if (e.key === "End") open(photos.length - 1);
    else return;
    e.preventDefault();
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn photos(n: usize) -> Vec<GalleryPhoto> {
        (0..n)
            .map(|i| {
                let mut fields = BTreeMap::new();
                fields.insert("title".into(), format!("Photo <{i}>"));
                fields.insert("caption".into(), "a \"quoted\" </script> caption".into());
                fields.insert("filename".into(), format!("IMG_{i}.jpg"));
                GalleryPhoto { fields, aspect: 1.5 }
            })
            .collect()
    }

    fn file<'a>(site: &'a Site, path: &str) -> &'a str {
        std::str::from_utf8(&site.files.iter().find(|f| f.path == path).unwrap().bytes).unwrap()
    }

    #[test]
    fn every_template_makes_a_complete_site() {
        for t in Template::ALL {
            let s = GallerySettings { template: t, ..GallerySettings::default() };
            let site = generate(&s, &photos(3)).unwrap();
            let index = file(&site, "index.html");
            assert!(index.starts_with("<!doctype html>"), "{t:?}");
            assert!(file(&site, "content/0003.html").contains("rel=\"prev\""));
            assert_eq!(site.images.len(), 6);
            assert!(site.images.iter().any(|r| r.path == "images/large/0002.jpg" && r.large && r.long_edge == 1600));
            // every referenced local file exists or is an image request
            for f in &site.files {
                let text = std::str::from_utf8(&f.bytes).unwrap();
                assert!(!text.contains("http://") && !text.contains("https://"), "{} loads from another host", f.path);
            }
            assert!(!index.contains("<script>alert"), "no raw markup");
            assert!(!index.contains("Photo <0>"), "{t:?}: title not escaped");
        }
    }

    #[test]
    fn captions_cannot_break_out_of_the_data_script() {
        let site = generate(&GallerySettings::default(), &photos(2)).unwrap();
        let index = file(&site, "index.html");
        let data = index.split("id=\"gallery-data\">").nth(1).unwrap();
        let data = data.split("</script>").next().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(parsed[1]["title"], "Photo <1>");
        assert_eq!(parsed[0]["alt"], "Photo <0>. a \"quoted\" </script> caption");
    }

    #[test]
    fn site_info_palette_and_links() {
        let mut s = GallerySettings::default();
        s.site.title = "Iceland".into();
        s.site.contact = "Ann".into();
        s.site.link = "ann@example.org".into();
        s.palette.background = "#102030".into();
        let site = generate(&s, &photos(1)).unwrap();
        assert!(file(&site, "index.html").contains("<a href=\"mailto:ann@example.org\">Ann</a>"));
        assert!(file(&site, "assets/gallery.css").contains("--bg: #102030"));
        s.site.link = "javascript:alert(1)".into();
        let site = generate(&s, &photos(1)).unwrap();
        assert!(!file(&site, "index.html").contains("javascript:"));
    }

    #[test]
    fn empty_and_oversized_galleries_are_errors() {
        assert!(generate(&GallerySettings::default(), &[]).is_err());
        let many = vec![GalleryPhoto::default(); MAX_PHOTOS + 1];
        assert!(generate(&GallerySettings::default(), &many).is_err());
        // NaN aspect and no fields still produce alt text
        let site = generate(&GallerySettings::default(), &[GalleryPhoto { fields: BTreeMap::new(), aspect: f32::NAN }]).unwrap();
        assert!(file(&site, "index.html").contains("alt=\"Photo 1\""));
    }

    #[test]
    fn single_template_index_is_the_first_photo_page() {
        let s = GallerySettings { template: Template::Single, ..GallerySettings::default() };
        let site = generate(&s, &photos(2)).unwrap();
        let index = file(&site, "index.html");
        assert!(index.contains("href=\"content/0002.html\""));
        assert!(index.contains("src=\"images/large/0001.jpg\""));
    }
}
