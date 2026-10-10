//! Book edits as JSON operations, shared by the UI, the control channel, MCP and the CLI.
//!
//! [`apply`] runs one operation on a book and returns a JSON reply. It is atomic: when the
//! result would be invalid, the book is left unchanged and the error says why. Pages are named
//! `"front"`, `"back"` or by 1-based number; cells by 0-based index.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::auto::{AutoPreset, auto_layout, builtin_presets, clear_layout};
use crate::{
    Background, Book, BookCell, BookError, BookKind, BookPage, BookSize, CellContent, CoverType, MAX_PAGES, Padding, PageRef, PageText, PhotoText,
    Result, TextPreset, TypeStyle, templates,
};

/// `(id, label, params)` of every operation.
pub const OPS: &[(&str, &str, &str)] = &[
    ("book.new", "New Book", "{name?, size?, kind?: pdf|jpeg, cover?: hardcover|softcover|none}"),
    ("book.get", "Book Document", "{} → the book JSON"),
    (
        "book.settings",
        "Book Settings",
        "{name?, kind?, size?: smallSquare|standardLandscape|standardPortrait|largeLandscape|letter|a4|custom, width?, height? (in, custom), cover?, paper?, jpegQuality?, colorProfile?, resolution?, sharpening?: off|low|standard|high, bleed? (pt)}",
    ),
    ("book.autoLayout", "Auto Layout", "{preset: name, photos: [id]} (the UI passes the filmstrip's photos)"),
    ("book.clearLayout", "Clear Layout", "{}"),
    ("book.addPage", "Add Page", "{template?: id (default 1-margin), after?: page, blank?: bool}"),
    ("book.removePage", "Remove Page", "{page}"),
    ("book.movePage", "Move Page", "{page, to}"),
    ("book.template", "Change Page Template", "{page, template} — the page's photos move to the new slots in order"),
    ("book.place", "Place Photo", "{page, cell, photo: id | null}"),
    ("book.swap", "Swap Photos", "{from: {page, cell}, to: {page, cell}}"),
    (
        "book.cell",
        "Cell Settings",
        "{page, cell, zoom? (1–10), pan? [x, y] (−1…1), fill?: bool, padding?: pt | {top, right, bottom, left}, linked?: bool}",
    ),
    ("book.text", "Set Cell Text", "{page, cell, text}"),
    (
        "book.photoText",
        "Photo Text",
        "{page, cell, enabled?: bool, text? (tokens like {Title}), position?: above|below|over, offset? (pt), style?: {…}}",
    ),
    ("book.pageText", "Page Text", "{page, enabled?: bool, text?, position?: top|bottom, offset? (pt), style?: {…}}"),
    (
        "book.type",
        "Type",
        "{page, cell?, target: cell|photoText|pageText|numbers, preset?: name, style?: {font, style, size, opacity, color, tracking, baseline, leading, kerning, columns, gutter, align, valign}}",
    ),
    ("book.textPreset", "Text Style Preset", "{name, style?: {…} (save), delete?: bool}"),
    (
        "book.background",
        "Background",
        "{page? (else the book), applyToAll?: bool, color?: [r,g,b], photo?: id|null, photoOpacity?, graphic?: none|frame|corners|band|lines, graphicColor?, graphicOpacity?, clear?: bool}",
    ),
    ("book.guides", "Guides", "{show?, bleed?, textSafe?, photoCells?, fillerText?}"),
    (
        "book.pageNumbers",
        "Page Numbers",
        "{show?, position?: topCorner|topCenter|bottomCorner|bottomCenter|side, offset?, style?, page?, hide?: bool, applyToAll?: bool}",
    ),
    ("book.favorite", "Favorite Template", "{template, on?: bool}"),
    ("book.templates", "Page Templates", "{} → [{id, name, group, photos, text, favorite}]"),
    ("book.presets", "Auto Layout Presets", "{} → [name]"),
];

fn bad(s: impl Into<String>) -> BookError {
    BookError::Bad(s.into())
}

fn merge<T: Serialize + DeserializeOwned>(t: &T, patch: &Value) -> Result<T> {
    let mut v = serde_json::to_value(t).map_err(|e| BookError::Json(e.to_string()))?;
    let (Some(o), Some(p)) = (v.as_object_mut(), patch.as_object()) else { return Err(bad("expected an object")) };
    for (k, x) in p {
        o.insert(k.clone(), x.clone());
    }
    serde_json::from_value(v).map_err(|e| bad(e.to_string()))
}

fn enum_of<T: DeserializeOwned>(v: &Value, what: &str) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| bad(format!("invalid {what}: {v}")))
}

fn page_ref(p: &Value, key: &str) -> Result<PageRef> {
    let v = p.get(key).ok_or_else(|| bad(format!("missing {key}")))?;
    PageRef::parse(v).ok_or_else(|| bad(format!("invalid page: {v}")))
}

fn index(p: &Value, key: &str) -> Result<usize> {
    p.get(key).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(format!("missing or invalid {key}")))
}

fn f32_of(p: &Value, key: &str) -> Option<f32> {
    p.get(key).and_then(Value::as_f64).map(|v| v as f32)
}

fn str_of<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}

fn bool_of(p: &Value, key: &str) -> Option<bool> {
    p.get(key).and_then(Value::as_bool)
}

fn color_of(v: &Value) -> Result<[u8; 3]> {
    if let Some(s) = v.as_str() {
        let h = s.trim().trim_start_matches('#');
        let c = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
        return match (h.len(), c(0), c(2), c(4)) {
            (6, Some(r), Some(g), Some(b)) => Ok([r, g, b]),
            _ => Err(bad(format!("invalid colour: {s}"))),
        };
    }
    enum_of(v, "colour")
}

fn page_mut(book: &mut Book, r: PageRef) -> Result<&mut BookPage> {
    book.page_mut(r).ok_or_else(|| bad(format!("no {}", r.label())))
}

fn cell_mut<'a>(book: &'a mut Book, p: &Value) -> Result<&'a mut BookCell> {
    let r = page_ref(p, "page")?;
    let i = index(p, "cell")?;
    page_mut(book, r)?.cells.get_mut(i).ok_or_else(|| bad(format!("no cell {i} on {}", r.label())))
}

/// The photo at a `{page, cell}` slot.
fn slot_photo<'a>(book: &'a mut Book, p: &Value) -> Result<&'a mut Option<String>> {
    match &mut cell_mut(book, p)?.content {
        CellContent::Photo { photo, .. } => Ok(photo),
        CellContent::Text { .. } => Err(bad("that cell holds text")),
    }
}

/// Runs one operation (`book.*`) on `book`. Unknown ids are errors.
pub fn apply(book: &mut Book, id: &str, p: &Value) -> Result<Value> {
    let before = book.clone();
    let r = run(book, id, p).and_then(|v| book.validate().map(|_| v));
    if r.is_err() {
        *book = before;
    }
    r
}

fn run(book: &mut Book, id: &str, p: &Value) -> Result<Value> {
    match id {
        "book.new" => {
            *book = Book::default();
            if let Some(n) = str_of(p, "name") {
                book.name = n.into();
            }
            if let Some(s) = str_of(p, "size") {
                book.settings.size = BookSize::parse(s).ok_or_else(|| bad(format!("unknown size: {s}")))?;
            }
            if let Some(k) = p.get("kind") {
                book.settings.kind = enum_of::<BookKind>(k, "kind")?;
            }
            if let Some(c) = p.get("cover") {
                book.settings.cover = enum_of::<CoverType>(c, "cover")?;
            }
            Ok(json!({"name": book.name}))
        }
        "book.get" => serde_json::to_value(&*book).map_err(|e| BookError::Json(e.to_string())),
        "book.settings" => {
            let s = &mut book.settings;
            if let Some(n) = str_of(p, "name") {
                book.name = n.into();
            }
            if let Some(v) = p.get("kind") {
                s.kind = enum_of(v, "kind")?;
            }
            if let Some(v) = str_of(p, "size") {
                s.size = if v.eq_ignore_ascii_case("custom") {
                    let cur = s.size.points();
                    BookSize::Custom { w: f32_of(p, "width").unwrap_or(cur.w / 72.0), h: f32_of(p, "height").unwrap_or(cur.h / 72.0) }
                } else {
                    BookSize::parse(v).ok_or_else(|| bad(format!("unknown size: {v}")))?
                };
            } else if let BookSize::Custom { w, h } = s.size {
                s.size = BookSize::Custom { w: f32_of(p, "width").unwrap_or(w), h: f32_of(p, "height").unwrap_or(h) };
            }
            if let Some(v) = p.get("cover") {
                s.cover = enum_of(v, "cover")?;
            }
            if let Some(v) = str_of(p, "paper") {
                s.paper = v.into();
            }
            if let Some(v) = p.get("jpegQuality") {
                s.jpeg_quality = v.as_u64().and_then(|q| u8::try_from(q).ok()).ok_or_else(|| bad("jpegQuality must be 1–100"))?;
            }
            if let Some(v) = str_of(p, "colorProfile") {
                s.color_profile = v.into();
            }
            if let Some(v) = p.get("resolution") {
                s.resolution = v.as_u64().and_then(|q| u32::try_from(q).ok()).ok_or_else(|| bad("resolution must be 72–600"))?;
            }
            if let Some(v) = p.get("sharpening") {
                s.sharpening = enum_of(v, "sharpening")?;
            }
            if let Some(v) = f32_of(p, "bleed") {
                s.bleed = v;
            }
            serde_json::to_value(&book.settings).map_err(|e| BookError::Json(e.to_string()))
        }
        "book.autoLayout" => {
            let name = str_of(p, "preset").unwrap_or("One Photo Per Page");
            let preset: AutoPreset = match p.get("custom") {
                Some(c) => enum_of(c, "custom preset")?,
                None => crate::auto::preset(name).ok_or_else(|| bad(format!("unknown preset: {name}")))?,
            };
            let photos: Vec<String> = p
                .get("photos")
                .and_then(Value::as_array)
                .ok_or_else(|| bad("missing photos"))?
                .iter()
                .map(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))
                .collect::<Option<_>>()
                .ok_or_else(|| bad("photos must be ids"))?;
            let n = auto_layout(book, &photos, &preset)?;
            Ok(json!({"pages": n, "photos": photos.len()}))
        }
        "book.clearLayout" => {
            clear_layout(book);
            Ok(json!({"pages": 0}))
        }
        "book.addPage" => {
            if book.pages.len() >= MAX_PAGES {
                return Err(bad(format!("a book has at most {MAX_PAGES} pages")));
            }
            let tid = if bool_of(p, "blank") == Some(true) { "blank" } else { str_of(p, "template").unwrap_or("1-margin") };
            let page = templates::page(tid).ok_or_else(|| bad(format!("unknown template: {tid}")))?;
            let at = match p.get("after").map(PageRef::parse) {
                None => book.pages.len(),
                Some(Some(PageRef::Page(i))) if i < book.pages.len() => i + 1,
                Some(Some(PageRef::Front)) => 0,
                Some(_) => return Err(bad("after must be an existing page")),
            };
            book.pages.insert(at, page);
            Ok(json!({"page": at + 1, "pages": book.pages.len()}))
        }
        "book.removePage" => match page_ref(p, "page")? {
            PageRef::Page(i) if i < book.pages.len() => {
                book.pages.remove(i);
                Ok(json!({"pages": book.pages.len()}))
            }
            r => Err(bad(format!("can't remove {}", r.label()))),
        },
        "book.movePage" => match (page_ref(p, "page")?, page_ref(p, "to")?) {
            (PageRef::Page(a), PageRef::Page(b)) if a < book.pages.len() && b < book.pages.len() => {
                let pg = book.pages.remove(a);
                book.pages.insert(b, pg);
                Ok(json!({"page": b + 1}))
            }
            _ => Err(bad("page and to must be existing pages")),
        },
        "book.template" => {
            let r = page_ref(p, "page")?;
            let tid = str_of(p, "template").ok_or_else(|| bad("missing template"))?;
            let mut fresh = templates::page(tid).ok_or_else(|| bad(format!("unknown template: {tid}")))?;
            let old = page_mut(book, r)?;
            let mut photos = old.photos().map(str::to_string).collect::<Vec<_>>().into_iter();
            for c in &mut fresh.cells {
                if let CellContent::Photo { photo, .. } = &mut c.content {
                    *photo = photos.next();
                }
            }
            fresh.text = old.text.take();
            fresh.background = old.background.take();
            fresh.hide_number = old.hide_number;
            let left: Vec<String> = photos.collect();
            *old = fresh;
            Ok(json!({"template": tid, "unplaced": left}))
        }
        "book.place" => {
            let photo = match p.get("photo") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(v) => Some(v.as_u64().ok_or_else(|| bad("photo must be an id"))?.to_string()),
            };
            *slot_photo(book, p)? = photo.clone();
            Ok(json!({"photo": photo}))
        }
        "book.swap" => {
            let (a, b) = (p.get("from").ok_or_else(|| bad("missing from"))?, p.get("to").ok_or_else(|| bad("missing to"))?);
            let pa = slot_photo(book, a)?.take();
            let pb = std::mem::replace(slot_photo(book, b)?, pa.clone());
            *slot_photo(book, a)? = pb.clone();
            Ok(json!({"from": pb, "to": pa}))
        }
        "book.cell" => {
            let c = cell_mut(book, p)?;
            if let Some(v) = p.get("padding") {
                c.padding = match v.as_f64() {
                    Some(x) => Padding::all(x as f32),
                    None => Padding { linked: false, ..merge(&c.padding, v)? },
                };
            }
            if let Some(l) = bool_of(p, "linked") {
                c.padding.linked = l;
                if l {
                    c.padding = Padding::all(c.padding.top);
                }
            }
            if let CellContent::Photo { zoom, pan, fill, .. } = &mut c.content {
                if let Some(z) = f32_of(p, "zoom") {
                    *zoom = z;
                }
                if let Some(v) = p.get("pan") {
                    *pan = enum_of(v, "pan")?;
                }
                if let Some(f) = bool_of(p, "fill") {
                    *fill = f;
                }
            } else if p.get("zoom").is_some() || p.get("pan").is_some() {
                return Err(bad("zoom and pan apply to photo cells"));
            }
            serde_json::to_value(&*c).map_err(|e| BookError::Json(e.to_string()))
        }
        "book.text" => {
            let t = str_of(p, "text").ok_or_else(|| bad("missing text"))?.to_string();
            match &mut cell_mut(book, p)?.content {
                CellContent::Text { text, .. } => *text = t,
                CellContent::Photo { .. } => return Err(bad("that cell holds a photo (use book.photoText)")),
            }
            Ok(json!({}))
        }
        "book.photoText" => {
            let c = cell_mut(book, p)?;
            let CellContent::Photo { text, .. } = &mut c.content else { return Err(bad("that cell holds text")) };
            if bool_of(p, "enabled") == Some(false) {
                *text = None;
                return Ok(json!({"enabled": false}));
            }
            let mut t = text.take().unwrap_or_default();
            let mut patch = p.clone();
            if let Some(o) = patch.as_object_mut() {
                for k in ["page", "cell", "enabled"] {
                    o.remove(k);
                }
                if let Some(s) = o.remove("style") {
                    t.style = merge(&t.style, &s)?;
                }
            }
            let t: PhotoText = merge(&t, &patch)?;
            *text = Some(t.clone());
            serde_json::to_value(t).map_err(|e| BookError::Json(e.to_string()))
        }
        "book.pageText" => {
            let pg = page_mut(book, page_ref(p, "page")?)?;
            if bool_of(p, "enabled") == Some(false) {
                pg.text = None;
                return Ok(json!({"enabled": false}));
            }
            let mut t = pg.text.take().unwrap_or_default();
            let mut patch = p.clone();
            if let Some(o) = patch.as_object_mut() {
                for k in ["page", "enabled"] {
                    o.remove(k);
                }
                if let Some(s) = o.remove("style") {
                    t.style = merge(&t.style, &s)?;
                }
            }
            let t: PageText = merge(&t, &patch)?;
            pg.text = Some(t.clone());
            serde_json::to_value(t).map_err(|e| BookError::Json(e.to_string()))
        }
        "book.type" => {
            let base = match str_of(p, "preset") {
                Some(n) => Some(crate::text::find_preset(book, n).ok_or_else(|| bad(format!("unknown text preset: {n}")))?),
                None => None,
            };
            let target = str_of(p, "target").unwrap_or("cell");
            let style: &mut TypeStyle = match target {
                "numbers" => &mut book.numbers.style,
                "pageText" => &mut page_mut(book, page_ref(p, "page")?)?.text.get_or_insert_with(PageText::default).style,
                "photoText" => match &mut cell_mut(book, p)?.content {
                    CellContent::Photo { text, .. } => &mut text.get_or_insert_with(PhotoText::default).style,
                    CellContent::Text { .. } => return Err(bad("that cell holds text (target cell)")),
                },
                "cell" => match &mut cell_mut(book, p)?.content {
                    CellContent::Text { style, .. } => style,
                    CellContent::Photo { text, .. } => &mut text.get_or_insert_with(PhotoText::default).style,
                },
                t => return Err(bad(format!("unknown target: {t}"))),
            };
            if let Some(b) = base {
                *style = b;
            }
            if let Some(s) = p.get("style") {
                *style = merge(style, s)?;
            }
            style.validate()?;
            serde_json::to_value(&*style).map_err(|e| BookError::Json(e.to_string()))
        }
        "book.textPreset" => {
            let name = str_of(p, "name").filter(|n| !n.trim().is_empty()).ok_or_else(|| bad("missing name"))?.to_string();
            book.text_presets.retain(|x| !x.name.eq_ignore_ascii_case(&name));
            if bool_of(p, "delete") == Some(true) {
                return Ok(json!({"deleted": name}));
            }
            let style: TypeStyle = match p.get("style") {
                Some(s) => merge(&TypeStyle::default(), s)?,
                None => return Err(bad("missing style")),
            };
            style.validate()?;
            book.text_presets.push(TextPreset { name: name.clone(), style });
            Ok(json!({"saved": name}))
        }
        "book.background" => {
            let page = p.get("page").map(|v| PageRef::parse(v).ok_or_else(|| bad(format!("invalid page: {v}")))).transpose()?;
            if bool_of(p, "clear") == Some(true) {
                if let Some(r) = page {
                    page_mut(book, r)?.background = None;
                } else {
                    book.background = Background::default();
                }
                return Ok(json!({"cleared": true}));
            }
            let mut bg = match page {
                Some(r) => book.page(r).ok_or_else(|| bad(format!("no {}", r.label())))?.background.clone().unwrap_or_else(|| book.background.clone()),
                None => book.background.clone(),
            };
            if let Some(v) = p.get("color") {
                bg.color = color_of(v)?;
            }
            if let Some(v) = p.get("photo") {
                bg.photo = match v {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    v => Some(v.as_u64().ok_or_else(|| bad("photo must be an id"))?.to_string()),
                };
            }
            if let Some(v) = f32_of(p, "photoOpacity") {
                bg.photo_opacity = v;
            }
            if let Some(v) = p.get("graphic") {
                bg.graphic = enum_of(v, "graphic")?;
            }
            if let Some(v) = p.get("graphicColor") {
                bg.graphic_color = color_of(v)?;
            }
            if let Some(v) = f32_of(p, "graphicOpacity") {
                bg.graphic_opacity = v;
            }
            if bool_of(p, "applyToAll") == Some(true) || page.is_none() {
                book.background = bg.clone();
                for pg in book.pages.iter_mut().chain([&mut book.front, &mut book.back]) {
                    if bool_of(p, "applyToAll") == Some(true) {
                        pg.background = None;
                    }
                }
            } else if let Some(r) = page {
                page_mut(book, r)?.background = Some(bg.clone());
            }
            serde_json::to_value(bg).map_err(|e| BookError::Json(e.to_string()))
        }
        "book.guides" => {
            let g = &mut book.guides;
            for (k, f) in [("show", &mut g.show), ("bleed", &mut g.bleed), ("textSafe", &mut g.text_safe), ("photoCells", &mut g.photo_cells), ("fillerText", &mut g.filler_text)]
            {
                if let Some(v) = bool_of(p, k) {
                    *f = v;
                }
            }
            serde_json::to_value(*g).map_err(|e| BookError::Json(e.to_string()))
        }
        "book.pageNumbers" => {
            let n = &mut book.numbers;
            if let Some(v) = bool_of(p, "show") {
                n.show = v;
            }
            if let Some(v) = p.get("position") {
                n.position = enum_of(v, "position")?;
            }
            if let Some(v) = f32_of(p, "offset") {
                n.offset = v;
            }
            if let Some(s) = p.get("style") {
                n.style = merge(&n.style, s)?;
            }
            if let Some(h) = bool_of(p, "hide") {
                page_mut(book, page_ref(p, "page")?)?.hide_number = h;
            }
            if bool_of(p, "applyToAll") == Some(true) {
                book.numbers.show = true;
                book.pages.iter_mut().for_each(|pg| pg.hide_number = false);
            }
            serde_json::to_value(&book.numbers).map_err(|e| BookError::Json(e.to_string()))
        }
        "book.favorite" => {
            let t = str_of(p, "template").ok_or_else(|| bad("missing template"))?;
            if templates::find(t).is_none() {
                return Err(bad(format!("unknown template: {t}")));
            }
            let on = bool_of(p, "on").unwrap_or(!book.favorites.iter().any(|f| f == t));
            book.favorites.retain(|f| f != t);
            if on {
                book.favorites.push(t.into());
            }
            Ok(json!({"template": t, "favorite": on}))
        }
        "book.templates" => Ok(Value::Array(
            templates::builtin()
                .iter()
                .map(|t| json!({"id": t.id, "name": t.name, "group": t.group.label(), "photos": t.photos(), "text": t.text, "favorite": book.favorites.iter().any(|f| f == t.id)}))
                .collect(),
        )),
        "book.presets" => Ok(json!(builtin_presets().into_iter().map(|p| p.name).collect::<Vec<_>>())),
        _ => Err(bad(format!("unknown book operation: {id}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Book {
        let mut b = Book::default();
        apply(&mut b, "book.autoLayout", &json!({"preset": "Two Per Page", "photos": ["1", "2", "3", 4]})).unwrap();
        b
    }

    #[test]
    fn every_op_is_listed_and_runs() {
        let mut book = b();
        assert_eq!(book.pages.len(), 2);
        let calls = [
            (
                "book.settings",
                json!({"size": "a4", "cover": "softcover", "paper": "Lustre", "jpegQuality": 80, "resolution": 300, "sharpening": "high"}),
            ),
            ("book.settings", json!({"size": "custom", "width": 9, "height": 6})),
            ("book.addPage", json!({"template": "4-grid", "after": 1})),
            ("book.addPage", json!({"blank": true})),
            ("book.movePage", json!({"page": 4, "to": 1})),
            ("book.removePage", json!({"page": 1})),
            ("book.template", json!({"page": 1, "template": "1-caption"})),
            ("book.place", json!({"page": 2, "cell": 0, "photo": 9})),
            ("book.swap", json!({"from": {"page": 1, "cell": 0}, "to": {"page": 2, "cell": 0}})),
            ("book.cell", json!({"page": 1, "cell": 0, "zoom": 2, "pan": [0.5, -0.5], "padding": 6, "fill": false})),
            ("book.cell", json!({"page": 1, "cell": 0, "padding": {"top": 2}})),
            ("book.text", json!({"page": 1, "cell": 1, "text": "Hello {Title}"})),
            ("book.photoText", json!({"page": 1, "cell": 0, "text": "{Filename}", "position": "above", "style": {"size": 8}})),
            ("book.pageText", json!({"page": 1, "text": "Chapter one", "position": "top"})),
            ("book.type", json!({"page": 1, "cell": 1, "style": {"columns": 2, "tracking": 50, "color": [10, 20, 30], "align": "justify"}})),
            ("book.type", json!({"page": 1, "cell": 1, "preset": "Title"})),
            ("book.type", json!({"target": "numbers", "style": {"size": 7}})),
            ("book.textPreset", json!({"name": "Mine", "style": {"size": 14}})),
            ("book.type", json!({"target": "pageText", "page": 1, "preset": "mine"})),
            ("book.background", json!({"color": "#f0e8d8", "graphic": "frame"})),
            ("book.background", json!({"page": 2, "photo": "1", "photoOpacity": 0.2})),
            ("book.background", json!({"page": 2, "clear": true})),
            ("book.guides", json!({"bleed": false, "fillerText": false})),
            ("book.pageNumbers", json!({"show": true, "position": "bottomCenter"})),
            ("book.pageNumbers", json!({"page": 2, "hide": true})),
            ("book.favorite", json!({"template": "4-grid"})),
            ("book.templates", json!({})),
            ("book.presets", json!({})),
            ("book.get", json!({})),
            ("book.clearLayout", json!({})),
            ("book.new", json!({"size": "smallSquare", "kind": "jpeg", "cover": "none"})),
        ];
        for (id, p) in calls {
            assert!(OPS.iter().any(|o| o.0 == id), "{id} not listed");
            apply(&mut book, id, &p).unwrap_or_else(|e| panic!("{id}: {e}"));
        }
        assert_eq!(book.settings.size, BookSize::SmallSquare);
        let json = book.to_json().unwrap();
        assert_eq!(Book::from_json(&json).unwrap(), book);
    }

    #[test]
    fn bad_input_is_an_error_and_changes_nothing() {
        let mut book = b();
        let snapshot = book.clone();
        for (id, p) in [
            ("book.nope", json!({})),
            ("book.settings", json!({"jpegQuality": 0})),
            ("book.settings", json!({"resolution": 100000})),
            ("book.settings", json!({"size": "custom", "width": f64::MAX})),
            ("book.settings", json!({"bleed": -1})),
            ("book.cell", json!({"page": 1, "cell": 0, "zoom": 1e9})),
            ("book.cell", json!({"page": 1, "cell": 0, "pan": [9, 9]})),
            ("book.cell", json!({"page": 99, "cell": 0})),
            ("book.cell", json!({"page": 1, "cell": 99})),
            ("book.cell", json!({"page": 0, "cell": 0})),
            ("book.type", json!({"page": 1, "cell": 0, "style": {"size": -3}})),
            ("book.type", json!({"page": 1, "cell": 0, "style": {"columns": 99}})),
            ("book.type", json!({"page": 1, "cell": 0, "target": "x"})),
            ("book.background", json!({"color": "#zzzzzz"})),
            ("book.background", json!({"photoOpacity": 5})),
            ("book.removePage", json!({"page": "front"})),
            ("book.template", json!({"page": 1, "template": "nope"})),
            ("book.autoLayout", json!({"preset": "nope", "photos": ["1"]})),
            ("book.autoLayout", json!({"photos": [{"x": 1}]})),
            ("book.swap", json!({"from": {"page": 1, "cell": 0}})),
            ("book.text", json!({"page": 1, "cell": 0, "text": "x"})),
            ("book.textPreset", json!({"name": " "})),
            ("book.favorite", json!({"template": "nope"})),
            ("book.pageNumbers", json!({"position": "middle"})),
        ] {
            assert!(apply(&mut book, id, &p).is_err(), "{id} {p}");
            assert_eq!(book, snapshot, "{id} changed the book");
        }
    }
}
