//! Font database: bundled fonts (always available, also on the web), the craft-fonts faces
//! installed with [`crate::craft_fonts::install`], optional system fonts found by scanning the
//! platform font directories (no fontconfig), user-registered font data, and PostScript-name
//! lookup.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parley::FontContext;
use parley::fontique::{Blob, Collection, CollectionOptions, FontInfoOverride, FontStyle, FontWeight, FontWidth, GenericFamily, SourceCache};
use skrifa::raw::{FileRef, TableProvider, types::Tag};
use skrifa::{MetadataProvider, string::StringId};

/// Family used when a style names no family or an unknown one.
pub const DEFAULT_FAMILY: &str = "Inter";

// The app's bundled Inter (assets/fonts, OFL).
pub static INTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
pub static INTER_MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/Inter-Medium.ttf");
pub static INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");

/// Fonts shipped with the app (OFL; licences in `assets/fonts`).
pub static BUNDLED: &[(&str, &[u8])] =
    &[("Inter-Regular.ttf", INTER_REGULAR), ("Inter-Medium.ttf", INTER_MEDIUM), ("Inter-SemiBold.ttf", INTER_SEMIBOLD)];

/// Families tried (if installed) after the requested one, for missing glyphs. The Thai and CJK
/// families ([`THAI_FAMILIES`], [`crate::cjk::families`]) go between these two lists, in the UI locale's script order.
const FALLBACK_CANDIDATES: &[&str] = &["Noto Sans", "Segoe UI", "DejaVu Sans", "Geeza Pro", "Arial Hebrew", "Noto Sans Arabic", "Noto Sans Hebrew"];
/// Thai-capable families: Windows (Leelawadee UI since Windows 8; Tahoma and Leelawadee on
/// older systems), macOS (Thonburi, Sukhumvit Set) and Linux (Noto). Text in a font without Thai
/// glyphs falls back to these, as with Photoshop's missing glyph protection (#1909).
pub const THAI_FAMILIES: &[&str] =
    &["Leelawadee UI", "Leelawadee", "Tahoma", "Thonburi", "Sukhumvit Set", "Noto Sans Thai", "Noto Sans Thai Looped", "Noto Sans Thai UI"];
/// Broad-coverage and emoji fonts, tried after the CJK script fonts so shared Han characters get
/// the locale's forms instead of Arial Unicode's.
const FALLBACK_LAST: &[&str] = &["Arial Unicode MS", "Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji"];

/// Every fallback family candidate, CJK ordered for `order` (see [`crate::cjk::script_order`]).
pub fn fallback_candidates(order: &[crate::cjk::CjkScript; 4]) -> Vec<String> {
    let mut v: Vec<String> = FALLBACK_CANDIDATES.iter().map(|s| s.to_string()).collect();
    v.extend(THAI_FAMILIES.iter().map(|s| s.to_string()));
    for s in order {
        // craft-fonts' CJK fonts (if installed) go ahead of the installed system fonts of their
        // script, in that script's slot of the locale order, so shared Han keeps the locale's forms.
        match s {
            crate::cjk::CjkScript::Japanese => v.extend(crate::craft_fonts::japanese_families()),
            crate::cjk::CjkScript::SimplifiedChinese => v.extend(crate::craft_fonts::chinese_families()),
            _ => {}
        }
        v.extend(crate::cjk::families(*s).iter().map(|s| s.to_string()));
    }
    v.extend(FALLBACK_LAST.iter().map(|s| s.to_string()));
    let mut seen = std::collections::HashSet::new();
    v.retain(|f| seen.insert(f.clone()));
    v
}

/// Bumped whenever font data is registered: fonts can arrive after start (the web build fetches
/// served fonts on demand, [`crate::served`]), so lists derived from the database, such as font
/// menus, compare it to know when to refresh.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Changes whenever font data is registered in any [`FontDb`].
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// One face in the database (for font menus).
#[derive(Clone, Debug, PartialEq)]
pub struct FaceInfo {
    pub family: String,
    /// OpenType typographic subfamily (name 17), or legacy subfamily (name 2).
    pub style: String,
    /// Stable face identity, including faces that share the same CSS attributes.
    pub postscript_name: Option<String>,
    /// CSS weight (100–900); variable fonts report their default.
    pub weight: f32,
    pub italic: bool,
    /// Variation axes: (tag, min, default, max).
    pub axes: Vec<(String, f32, f32, f32)>,
}

/// Result of a PostScript-name lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFont {
    pub family: String,
    pub weight: u16,
    pub italic: bool,
    /// True when the exact face was found; false for a heuristic family guess.
    pub exact: bool,
}

pub struct FontDb {
    pub(crate) fcx: FontContext,
    system_loaded: bool,
    /// PostScript name → (family, weight, italic), filled lazily.
    ps_cache: HashMap<String, Option<ResolvedFont>>,
    fallbacks: Vec<String>,
    face_cache: HashMap<String, Vec<FaceInfo>>,
    face_aliases: HashMap<(String, usize), String>,
}

impl Default for FontDb {
    fn default() -> Self {
        Self::new()
    }
}

impl FontDb {
    /// Bundled fonts only (deterministic: used by tests and the web build).
    pub fn new() -> Self {
        let collection = Collection::new(CollectionOptions { shared: false, system_fonts: false });
        let mut db = FontDb {
            fcx: FontContext { collection, source_cache: SourceCache::default() },
            system_loaded: false,
            ps_cache: HashMap::new(),
            fallbacks: Vec::new(),
            face_cache: HashMap::new(),
            face_aliases: HashMap::new(),
        };
        for (_, bytes) in BUNDLED {
            db.register_font_data(bytes.to_vec());
        }
        // The installed craft-fonts (empty unless the host installed them), before any system font.
        for f in crate::craft_fonts::installed() {
            db.register_blob(Blob::new(Arc::new(f.bytes)));
        }
        db.refresh_generics();
        db
    }

    /// Bundled fonts plus the fonts installed on this machine (no-op on the web).
    pub fn with_system_fonts() -> Self {
        let mut db = Self::new();
        db.load_system_fonts();
        db
    }

    /// Scans the platform font directories (once). Font files are memory-mapped lazily by
    /// fontique when a face is first used.
    pub fn load_system_fonts(&mut self) {
        if self.system_loaded {
            return;
        }
        self.system_loaded = true;
        #[cfg(not(target_arch = "wasm32"))]
        {
            // One call per file: fontique's directory scan is much slower on large folders.
            let mut files = Vec::new();
            for d in system_font_dirs() {
                collect_font_files(&d, 0, &mut files);
            }
            // PingFang (the macOS Chinese UI font) lives outside the font folders.
            if cfg!(target_os = "macos") {
                files.extend(crate::cjk::mac_pingfang());
            }
            for f in files {
                self.fcx.collection.load_fonts_from_paths([f]);
            }
            self.ps_cache.clear();
            self.face_cache.clear();
            self.refresh_generics();
        }
    }

    /// Registers font data (TTF/OTF, or every face of a TTC/OTC). Returns the family names added.
    pub fn register_font_data(&mut self, bytes: Vec<u8>) -> Vec<String> {
        self.register_blob(Blob::new(Arc::new(bytes)))
    }

    /// Registers embedded font data without copying it.
    pub fn register_static_font(&mut self, bytes: &'static [u8]) -> Vec<String> {
        self.register_blob(Blob::new(Arc::new(bytes)))
    }

    fn register_blob(&mut self, blob: Blob<u8>) -> Vec<String> {
        let added = self.fcx.collection.register_fonts(blob, None);
        let mut names = Vec::new();
        for (id, _) in added {
            if let Some(n) = self.fcx.collection.family_name(id)
                && !names.iter().any(|x: &String| x == n)
            {
                names.push(n.to_string());
            }
        }
        // Deterministic order, user-facing families first: a collection's registration order isn't
        // stable, and macOS ships private UI faces (".Hiragino Kaku Gothic Interface") whose
        // metrics differ from the public family's.
        names.sort_by_key(|n| (is_hidden_family(n), n.to_lowercase()));
        self.ps_cache.clear();
        self.face_cache.clear();
        self.refresh_generics();
        GENERATION.fetch_add(1, Ordering::Relaxed);
        names
    }

    fn refresh_generics(&mut self) {
        let c = &mut self.fcx.collection;
        if let Some(inter) = c.family_id(DEFAULT_FAMILY) {
            for g in [GenericFamily::SansSerif, GenericFamily::Serif, GenericFamily::SystemUi, GenericFamily::UiSansSerif] {
                if c.generic_families(g).next().is_none() {
                    c.set_generic_families(g, std::iter::once(inter));
                }
            }
        }
        let order = crate::cjk::ui_script_order();
        self.fallbacks = fallback_candidates(&order).into_iter().filter(|f| c.family_id(f).is_some()).collect();
    }

    /// Families available after the requested one (bundled default + installed coverage fonts).
    pub(crate) fn fallback_stack(&self) -> impl Iterator<Item = &str> {
        std::iter::once(DEFAULT_FAMILY).chain(self.fallbacks.iter().map(String::as_str))
    }

    /// All family names, sorted.
    pub fn families(&mut self) -> Vec<String> {
        // Private system faces (a leading '.', e.g. macOS ".SF NS") are hidden, as in Photoshop.
        let mut v: Vec<String> = self.fcx.collection.family_names().filter(|n| !is_hidden_family(n)).map(str::to_string).collect();
        v.sort_by_key(|s| s.to_lowercase());
        v.dedup();
        v
    }

    pub fn has_family(&mut self, name: &str) -> bool {
        self.fcx.collection.family_id(name).is_some()
    }

    /// Whether the face selected for a character style exposes an OpenType GSUB feature.
    ///
    /// This deliberately checks the matched face, rather than every face in the family: a
    /// regular face can lack a feature that its italic or bold sibling has (or vice versa).
    pub(crate) fn selected_face_has_feature(&mut self, family: &str, weight: u16, italic: bool, feature: Tag) -> bool {
        let family = if family.is_empty() || !self.has_family(family) { DEFAULT_FAMILY } else { family };
        let Some(info) = self.fcx.collection.family_by_name(family) else {
            return false;
        };
        let style = if italic { FontStyle::Italic } else { FontStyle::Normal };
        let Some(font) = info.match_font(FontWidth::default(), style, FontWeight::new(weight.clamp(1, 1000) as f32), true) else {
            return false;
        };
        let Some(blob) = font.load(Some(&mut self.fcx.source_cache)) else {
            return false;
        };
        let Ok(font) = skrifa::FontRef::from_index(blob.as_ref(), font.index()) else {
            return false;
        };
        font.gsub()
            .ok()
            .and_then(|gsub| gsub.feature_list().ok())
            .is_some_and(|features| features.feature_records().iter().any(|record| record.feature_tag() == feature))
    }

    /// Faces of a family, preserving names even when multiple faces share CSS attributes.
    pub fn faces(&mut self, family: &str) -> Vec<FaceInfo> {
        if let Some(faces) = self.face_cache.get(family) {
            return faces.clone();
        }
        let Some(info) = self.fcx.collection.family_by_name(family) else {
            return Vec::new();
        };
        let faces: Vec<_> = info
            .fonts()
            .iter()
            .map(|f| {
                let blob = f.load(Some(&mut self.fcx.source_cache));
                let font = blob.as_ref().and_then(|b| skrifa::FontRef::from_index(b.as_ref(), f.index()).ok());
                let name =
                    |id| font.as_ref().and_then(|f| f.localized_strings(id).english_or_first()).map(|s| s.to_string()).filter(|s| !s.is_empty());
                let italic = !matches!(f.style(), FontStyle::Normal);
                FaceInfo {
                    family: info.name().to_string(),
                    style: name(StringId::TYPOGRAPHIC_SUBFAMILY_NAME)
                        .or_else(|| name(StringId::SUBFAMILY_NAME))
                        .unwrap_or_else(|| fallback_style(f.weight().value(), italic)),
                    postscript_name: name(StringId::POSTSCRIPT_NAME),
                    weight: f.weight().value(),
                    italic,
                    axes: f.axes().iter().map(|a| (a.tag.to_string(), a.min, a.default, a.max)).collect(),
                }
            })
            .collect();
        self.face_cache.insert(family.to_string(), faces.clone());
        faces
    }

    /// Resolve a menu style to its actual metadata instead of guessing from its spelling.
    pub fn named_face(&mut self, family: &str, style: &str) -> Option<FaceInfo> {
        self.faces(family).into_iter().find(|f| f.style.eq_ignore_ascii_case(style))
    }

    /// Select an exact face for shaping. Private aliases are runtime-only: the document keeps
    /// its public family and PostScript name. CSS matching alone cannot distinguish e.g. the
    /// numeric subfamilies of Yoon fonts, all of which declare weight 400 / normal.
    pub(crate) fn select_named_face(&mut self, style: &mut crate::style::CharStyle) {
        let family = style.font_family.clone();
        let faces = self.faces(&family);
        let index = style.postscript_name.as_ref().and_then(|ps| faces.iter().position(|f| f.postscript_name.as_ref() == Some(ps))).or_else(|| {
            faces.iter().position(|f| {
                !style.font_style.is_empty()
                    && f.style.eq_ignore_ascii_case(&style.font_style)
                    && f.weight.round() as u16 == style.weight
                    && f.italic == style.italic
            })
        });
        let Some(index) = index else { return };
        let Some(face) = faces.get(index) else { return };
        let key = (family.clone(), index);
        let alias = if let Some(alias) = self.face_aliases.get(&key) {
            alias.clone()
        } else {
            let Some(info) = self.fcx.collection.family_by_name(&family) else { return };
            let Some(font) = info.fonts().get(index) else { return };
            let Some(blob) = font.load(Some(&mut self.fcx.source_cache)) else { return };
            let Some(blob) = isolated_face(blob, font.index()) else { return };
            let alias = format!(".dac-face-{}-{index}", info.id().to_u64());
            self.fcx.collection.register_fonts(blob, Some(FontInfoOverride { family_name: Some(&alias), ..Default::default() }));
            self.face_aliases.insert(key, alias.clone());
            alias
        };
        style.font_family = alias;
        style.weight = face.weight.round() as u16;
        style.italic = face.italic;
    }

    /// Finds a face by PostScript name (as stored in PSD files): exact match by reading the
    /// `name` table of candidate faces, else a heuristic split (`Arial-BoldMT` → Arial, 700).
    pub fn resolve_postscript(&mut self, ps: &str) -> ResolvedFont {
        if let Some(Some(r)) = self.ps_cache.get(ps) {
            return r.clone();
        }
        let guess = guess_from_postscript(ps);
        let exact = self.find_exact(ps, &guess.family);
        let r = exact.unwrap_or_else(|| {
            // Keep the guessed family only if we have it; otherwise let fallback pick.
            let mut g = guess.clone();
            if !self.has_family(&g.family)
                && let Some(f) = self.families().into_iter().find(|f| f.replace(' ', "").eq_ignore_ascii_case(&g.family.replace(' ', "")))
            {
                g.family = f;
            }
            g
        });
        self.ps_cache.insert(ps.to_string(), Some(r.clone()));
        r
    }

    fn find_exact(&mut self, ps: &str, family_guess: &str) -> Option<ResolvedFont> {
        let first_word = family_guess.split(' ').next().unwrap_or(family_guess).to_lowercase();
        let mut candidates = self.families();
        candidates.sort_by_key(|f| !f.to_lowercase().replace(' ', "").starts_with(&first_word));
        for fam in candidates {
            for face in self.faces(&fam) {
                let Some(name) = face.postscript_name else { continue };
                let resolved = ResolvedFont { family: face.family, weight: face.weight.round() as u16, italic: face.italic, exact: true };
                self.ps_cache.insert(name.clone(), Some(resolved.clone()));
                if name == ps {
                    return Some(resolved);
                }
            }
        }
        None
    }
}

/// Make a collection expose only the requested face without rewriting any font tables.
/// TTC table offsets are absolute; changing its face directory preserves them verbatim.
fn isolated_face(blob: Blob<u8>, index: u32) -> Option<Blob<u8>> {
    match FileRef::new(blob.as_ref()).ok()? {
        FileRef::Font(_) => (index == 0).then_some(blob),
        FileRef::Collection(collection) => {
            collection.get(index).ok()?;
            // Bound a copy of untrusted font data. Ordinary single-face fonts need no copy.
            if blob.as_ref().len() > 256 * 1024 * 1024 {
                return None;
            }
            let offset = 12usize.checked_add(usize::try_from(index).ok()?.checked_mul(4)?)?;
            let face_offset: [u8; 4] = blob.as_ref().get(offset..offset.checked_add(4)?)?.try_into().ok()?;
            let mut bytes = blob.as_ref().to_vec();
            bytes.get_mut(8..12)?.copy_from_slice(&1u32.to_be_bytes());
            bytes.get_mut(12..16)?.copy_from_slice(&face_offset);
            Some(Blob::new(Arc::new(bytes)))
        }
    }
}

fn fallback_style(weight: f32, italic: bool) -> String {
    let name = match weight.round() as i32 {
        ..=150 => "Thin",
        151..=250 => "ExtraLight",
        251..=350 => "Light",
        351..=450 => "Regular",
        451..=550 => "Medium",
        551..=650 => "SemiBold",
        651..=750 => "Bold",
        751..=850 => "ExtraBold",
        _ => "Black",
    };
    match (name, italic) {
        ("Regular", true) => "Italic".into(),
        (_, true) => format!("{name} Italic"),
        (_, false) => name.into(),
    }
}

/// Number of faces in a font file (1 for TTF/OTF, n for TTC/OTC, 0 if unreadable).
pub fn face_count(bytes: &[u8]) -> usize {
    match FileRef::new(bytes) {
        Ok(FileRef::Font(_)) => 1,
        Ok(FileRef::Collection(c)) => c.len() as usize,
        Err(_) => 0,
    }
}

/// Heuristic PostScript-name split: `MyriadPro-BoldIt` → ("Myriad Pro", 700, italic).
pub fn guess_from_postscript(ps: &str) -> ResolvedFont {
    let (fam, style) = ps.split_once('-').unwrap_or((ps, ""));
    let fam = fam.trim_end_matches("MT").trim_end_matches("PS").trim_end_matches("Std").trim_end_matches("Pro");
    let pro = ps.split_once('-').map_or(ps, |p| p.0);
    let suffix = if pro.ends_with("Pro") {
        " Pro"
    } else if pro.ends_with("Std") {
        " Std"
    } else {
        ""
    };
    // Split camel case: "TimesNewRoman" → "Times New Roman".
    let mut family = String::new();
    let chars: Vec<char> = fam.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0
            && c.is_uppercase()
            && (chars[i - 1].is_lowercase() || chars.get(i + 1).is_some_and(|n| n.is_lowercase()) && chars[i - 1].is_uppercase())
        {
            family.push(' ');
        }
        family.push(c);
    }
    family.push_str(suffix);
    let s = style.to_lowercase();
    let weight = if s.contains("thin") || s.contains("hairline") {
        100
    } else if s.contains("extralight") || s.contains("ultralight") {
        200
    } else if s.contains("light") {
        300
    } else if s.contains("medium") {
        500
    } else if s.contains("semibold") || s.contains("demibold") || s.contains("demi") {
        600
    } else if s.contains("extrabold") || s.contains("ultrabold") || s.contains("heavy") {
        800
    } else if s.contains("black") {
        900
    } else if s.contains("bold") {
        700
    } else {
        400
    };
    let italic = s.contains("italic") || s.ends_with("it") || s.contains("oblique");
    ResolvedFont { family: family.trim().to_string(), weight, italic, exact: false }
}

#[cfg(not(target_arch = "wasm32"))]
fn collect_font_files(dir: &std::path::Path, depth: u32, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 8 {
                collect_font_files(&p, depth + 1, out);
            }
        } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc")) {
            out.push(p);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn system_font_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        v.push(PathBuf::from("/System/Library/Fonts"));
        v.push(PathBuf::from("/Library/Fonts"));
        if let Some(h) = &home {
            v.push(h.join("Library/Fonts"));
        }
    } else if cfg!(target_os = "windows") {
        let windir = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        v.push(windir.join("Fonts"));
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(PathBuf::from(l).join("Microsoft\\Windows\\Fonts"));
        }
    } else {
        v.push(PathBuf::from("/usr/share/fonts"));
        v.push(PathBuf::from("/usr/local/share/fonts"));
        // Inside a Flatpak sandbox the host's system and per-user fonts are mounted here
        // (`/usr/share/fonts` is the runtime's own small set). Missing dirs are skipped.
        v.push(PathBuf::from("/run/host/fonts"));
        v.push(PathBuf::from("/run/host/user-fonts"));
        if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
            v.push(PathBuf::from(d).join("fonts"));
        }
        if let Some(h) = &home {
            v.push(h.join(".local/share/fonts"));
            v.push(h.join(".fonts"));
        }
    }
    v
}

/// A platform-private family (macOS names its UI faces with a leading '.'): never listed or
/// picked by default.
pub fn is_hidden_family(name: &str) -> bool {
    name.starts_with('.')
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #[test]
    fn hidden_families_sort_last_and_are_not_listed() {
        assert!(super::is_hidden_family(".Hiragino Kaku Gothic Interface"));
        assert!(!super::is_hidden_family("Hiragino Sans"));
        let mut v = [".Hiragino Kaku Gothic Interface".to_string(), "Hiragino Kaku Gothic ProN".to_string()];
        v.sort_by_key(|n| (super::is_hidden_family(n), n.to_lowercase()));
        assert_eq!(v[0], "Hiragino Kaku Gothic ProN");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn scans_flatpak_host_fonts() {
        let dirs = super::system_font_dirs();
        assert!(dirs.iter().any(|d| d.ends_with("run/host/fonts")));
        assert!(dirs.iter().any(|d| d.ends_with("run/host/user-fonts")));
    }

    /// The bundled UI fonts cover Czech (the `cs` UI language) on their own: every letter with a
    /// diacritic and the Czech quotation marks, so no system fallback font is needed.
    #[test]
    fn bundled_fonts_cover_czech() {
        use skrifa::MetadataProvider;
        let czech = "aábcčdďeéěfghiíjklmnňoópqrřsštťuúůvwxyýzž„“";
        let letters: String = czech.chars().chain(czech.chars().flat_map(char::to_uppercase)).collect();
        for (name, bytes) in super::BUNDLED {
            let font = skrifa::FontRef::new(bytes).expect("bundled font parses");
            let cmap = font.charmap();
            let missing: String = letters.chars().filter(|c| cmap.map(*c).is_none_or(|g| g.to_u32() == 0)).collect();
            assert!(missing.is_empty(), "{name} lacks Czech glyphs: {missing}");
        }
    }

    /// The bundled UI fonts cover French (the `fr` UI language) on their own: every accented
    /// letter, the ligatures, the guillemets and the no-break space used before `: ; ? !`.
    #[test]
    fn bundled_fonts_cover_french() {
        use skrifa::MetadataProvider;
        let french = "àâæçéèêëîïôœùûüÿ";
        let letters: String = french.chars().chain(french.chars().flat_map(char::to_uppercase)).chain("«»\u{a0}’".chars()).collect();
        for (name, bytes) in super::BUNDLED {
            let font = skrifa::FontRef::new(bytes).expect("bundled font parses");
            let cmap = font.charmap();
            let missing: String = letters.chars().filter(|c| cmap.map(*c).is_none_or(|g| g.to_u32() == 0)).collect();
            assert!(missing.is_empty(), "{name} lacks French glyphs: {missing:?}");
        }
    }

    #[test]
    fn missing_font_dirs_are_skipped() {
        let mut files = Vec::new();
        super::collect_font_files(std::path::Path::new("/nonexistent/app/fonts"), 0, &mut files);
        assert!(files.is_empty());
    }
}

#[cfg(test)]
#[path = "font_names_tests.rs"]
mod font_names_tests;
