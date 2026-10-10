//! Auto Layout: fill the book from a list of photos with a preset, and Clear Layout.
//!
//! A preset says what the left (even) and right (odd) pages get: a fixed template, or nothing
//! (left blank). Page 1 is a right-hand page, as in a bound book. With a cover, the first photo
//! also goes on the front cover.

use serde::{Deserialize, Serialize};

use crate::{Book, BookError, CellContent, MAX_PAGES, PhotoText, Result, templates};

/// What one side of a spread gets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "template")]
pub enum SideRule {
    /// A blank page.
    Blank,
    /// A page from this template (its photo slots take the next photos).
    Template(String),
}

/// An Auto Layout preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoPreset {
    pub name: String,
    pub left: SideRule,
    pub right: SideRule,
    /// Zoom photos to fill their cells (else fit).
    #[serde(default = "yes")]
    pub fill: bool,
    /// Add photo text (`{Title}`) under every photo.
    #[serde(default)]
    pub photo_text: bool,
}

fn yes() -> bool {
    true
}

/// The built-in presets.
pub fn builtin_presets() -> Vec<AutoPreset> {
    let tpl = |s: &str| SideRule::Template(s.into());
    vec![
        AutoPreset { name: "One Photo Per Page".into(), left: tpl("1-margin"), right: tpl("1-margin"), fill: true, photo_text: false },
        AutoPreset { name: "Left Blank, Right One Photo".into(), left: SideRule::Blank, right: tpl("1-margin"), fill: true, photo_text: false },
        AutoPreset { name: "One Photo Per Page, with Text".into(), left: tpl("1-caption"), right: tpl("1-caption"), fill: false, photo_text: true },
        AutoPreset { name: "Fill Pages".into(), left: tpl("1-full"), right: tpl("1-full"), fill: true, photo_text: false },
        AutoPreset { name: "Two Per Page".into(), left: tpl("2-stack"), right: tpl("2-side"), fill: true, photo_text: false },
        AutoPreset { name: "Grid of Four".into(), left: tpl("4-grid"), right: tpl("4-grid"), fill: true, photo_text: false },
    ]
}

/// The built-in preset with this name (case-insensitive).
pub fn preset(name: &str) -> Option<AutoPreset> {
    builtin_presets().into_iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

fn template_page(rule: &SideRule) -> Result<Option<crate::BookPage>> {
    match rule {
        SideRule::Blank => Ok(None),
        SideRule::Template(id) => templates::page(id).map(Some).ok_or_else(|| BookError::Bad(format!("unknown template: {id}"))),
    }
}

/// Lays `photos` out over new pages (replacing the pages, keeping settings, covers' templates
/// and text). Returns the number of pages made.
pub fn auto_layout(book: &mut Book, photos: &[String], preset: &AutoPreset) -> Result<usize> {
    if photos.is_empty() {
        return Err(BookError::Bad("no photos to lay out".into()));
    }
    let left = template_page(&preset.left)?;
    let right = template_page(&preset.right)?;
    let slots = |p: &Option<crate::BookPage>| p.as_ref().map_or(0, crate::BookPage::photo_slots);
    if slots(&left) + slots(&right) == 0 {
        return Err(BookError::Bad("the preset places no photos".into()));
    }
    let mut pages = Vec::new();
    let mut next = photos.iter();
    let mut remaining = photos.len();
    while remaining > 0 {
        if pages.len() >= MAX_PAGES {
            return Err(BookError::Bad(format!("the photos need more than {MAX_PAGES} pages")));
        }
        // page index 0 is page 1, a right-hand page
        let rule = if pages.len() % 2 == 0 { &right } else { &left };
        let mut page = rule.clone().unwrap_or_else(|| templates::page("blank").unwrap_or_default());
        for c in &mut page.cells {
            if let CellContent::Photo { photo, fill, text, .. } = &mut c.content {
                let Some(id) = next.next() else { break };
                *photo = Some(id.clone());
                *fill = preset.fill;
                if preset.photo_text {
                    *text = Some(PhotoText::default());
                }
                remaining -= 1;
            }
        }
        // unfilled slots on the last page stay empty
        pages.push(page);
    }
    book.pages = pages;
    if book.has_cover()
        && let Some(first) = photos.first()
        && let Some(c) = book.front.cells.iter_mut().find(|c| c.is_photo())
        && let CellContent::Photo { photo, .. } = &mut c.content
    {
        *photo = Some(first.clone());
    }
    Ok(book.pages.len())
}

/// Removes every page (the covers' photos too).
pub fn clear_layout(book: &mut Book) {
    book.pages.clear();
    for c in book.front.cells.iter_mut().chain(book.back.cells.iter_mut()) {
        if let CellContent::Photo { photo, .. } = &mut c.content {
            *photo = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        (1..=n).map(|i| i.to_string()).collect()
    }

    #[test]
    fn presets_lay_out_every_photo() {
        for p in builtin_presets() {
            let mut b = Book::default();
            let n = auto_layout(&mut b, &ids(7), &p).unwrap();
            assert!(n > 0, "{}", p.name);
            let placed: Vec<_> = b.pages.iter().flat_map(|p| p.photos()).collect();
            assert_eq!(placed, ids(7).iter().map(String::as_str).collect::<Vec<_>>(), "{}", p.name);
            assert_eq!(b.front.photos().next(), Some("1"));
            b.validate().unwrap();
        }
        let mut b = Book::default();
        let p = preset("left blank, right one photo").unwrap();
        auto_layout(&mut b, &ids(3), &p).unwrap();
        // page 1 right (photo), page 2 left (blank), page 3 photo, page 4 blank, page 5 photo
        assert_eq!(b.pages.len(), 5);
        assert_eq!(b.pages[1].photo_slots(), 0);
        clear_layout(&mut b);
        assert!(b.pages.is_empty() && b.front.photos().next().is_none());
    }

    #[test]
    fn bad_presets_are_errors() {
        let mut b = Book::default();
        let blank = AutoPreset { name: "x".into(), left: SideRule::Blank, right: SideRule::Blank, fill: true, photo_text: false };
        assert!(auto_layout(&mut b, &ids(2), &blank).is_err());
        let unknown = AutoPreset { left: SideRule::Template("nope".into()), ..blank };
        assert!(auto_layout(&mut b, &ids(2), &unknown).is_err());
        assert!(auto_layout(&mut b, &[], &builtin_presets()[0]).is_err());
        assert!(auto_layout(&mut b, &ids(5000), &builtin_presets()[0]).is_err());
    }
}
