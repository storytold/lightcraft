//! Craft-fonts faces read from the package's files, for builds made with `CRAFT_FONTS_EMBED=0`
//! (see [`super`]). Native only: the web build has no files beside it and always embeds.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use super::{CraftFont, FontProblem};

/// A face read from the package's files: [`CraftFont`]'s fields, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedFace {
    pub family: String,
    pub style: String,
    pub scripts: Vec<String>,
    pub bytes: Vec<u8>,
}

impl LoadedFace {
    /// Kept for the rest of the process, as embedded fonts are: egui and the text renderers hold
    /// `&'static` faces. Called once per face per process ([`loaded`]).
    fn leak(self) -> CraftFont {
        let scripts: Vec<&'static str> = self.scripts.into_iter().map(|s| &*Box::leak(s.into_boxed_str())).collect();
        CraftFont {
            family: Box::leak(self.family.into_boxed_str()),
            style: Box::leak(self.style.into_boxed_str()),
            scripts: Vec::leak(scripts),
            bytes: Vec::leak(self.bytes),
        }
    }
}

pub(super) struct Loaded {
    pub(super) fonts: &'static [CraftFont],
    pub(super) problems: Vec<FontProblem>,
}

pub(super) fn loaded() -> &'static Loaded {
    static LOADED: OnceLock<Loaded> = OnceLock::new();
    LOADED.get_or_init(|| {
        let exe = std::env::current_exe().ok();
        let (faces, problems) = match exe.as_deref().and_then(package_root) {
            Some(root) => read_package(&root),
            None => (
                Vec::new(),
                vec![FontProblem {
                    what: "Contents/Resources/fonts".into(),
                    detail: "could not tell where this program is, so its fonts were not found".into(),
                }],
            ),
        };
        for problem in &problems {
            log::warn!("{problem}; text that needs it may show as empty boxes. Reinstall {} to restore it", lcc_brand::DISPLAY_NAME);
        }
        Loaded { fonts: Vec::leak(faces.into_iter().map(LoadedFace::leak).collect()), problems }
    })
}

/// The folder holding the package's `fonts/manifest.txt` for the program at `exe`: `Contents/Resources`
/// of the macOS bundle it is in (`Contents/MacOS/<program>`). Found from the program's real path, so
/// a symlink to the CLI elsewhere still finds the bundle.
fn package_root(exe: &Path) -> Option<PathBuf> {
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    Some(exe.parent()?.parent()?.join("Resources"))
}

/// Largest font list [`read_package`] reads, and the most faces it takes from it.
const MAX_MANIFEST: u64 = 1 << 20;
const MAX_FACES: usize = 256;
/// Largest font file [`read_package`] reads (the biggest craft-fonts face is ~16 MB).
const MAX_FONT_FILE: u64 = 64 << 20;

/// Reads the faces `root/fonts/manifest.txt` lists (craft-fonts' format:
/// `family | style | file | scripts | licence | licence file | sha256 | source`, the file relative
/// to `root`). A face whose file is missing, too large, outside `root`, not the file the list's
/// checksum names, or not a font is left out with a [`FontProblem`]; the others are returned.
pub fn read_package(root: &Path) -> (Vec<LoadedFace>, Vec<FontProblem>) {
    let manifest = root.join("fonts").join("manifest.txt");
    let text = match read_capped(&manifest, MAX_MANIFEST) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            return (
                Vec::new(),
                vec![FontProblem { what: manifest.display().to_string(), detail: format!("the font list could not be read ({e})") }],
            );
        }
    };
    let (mut faces, mut problems) = (Vec::new(), Vec::new());
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if faces.len() + problems.len() >= MAX_FACES {
            problems.push(FontProblem {
                what: manifest.display().to_string(),
                detail: format!("lists more than {MAX_FACES} fonts; the rest were skipped"),
            });
            break;
        }
        let fields: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let [family, style, file, scripts, _, _, sha256, ..] = fields.as_slice() else {
            problems.push(FontProblem { what: manifest.display().to_string(), detail: format!("malformed line: {line}") });
            continue;
        };
        let what = format!("{family} {style}");
        match read_face(root, file, sha256) {
            Ok(bytes) => faces.push(LoadedFace {
                family: family.to_string(),
                style: style.to_string(),
                scripts: scripts.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                bytes,
            }),
            Err(detail) => problems.push(FontProblem { what, detail }),
        }
    }
    (faces, problems)
}

/// One face's bytes, checked against the list's checksum and parsed.
fn read_face(root: &Path, file: &str, sha256: &str) -> Result<Vec<u8>, String> {
    use sha2::Digest as _;
    let rel = Path::new(file);
    // a file of the package: no absolute path, no `..` reaching out of it
    if file.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(format!("{file} is not a path inside the package"));
    }
    let path = root.join(rel);
    let bytes = read_capped(&path, MAX_FONT_FILE).map_err(|e| format!("{} could not be read ({e})", path.display()))?;
    let digest = sha2::Sha256::digest(&bytes);
    let actual: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    if !actual.eq_ignore_ascii_case(sha256) {
        return Err(format!("{} is damaged (its checksum doesn't match the font list)", path.display()));
    }
    // egui refuses (panics on) data it can't parse: never hand it any
    if ab_glyph::FontRef::try_from_slice(&bytes).is_err() {
        return Err(format!("{} is not a font this program can read", path.display()));
    }
    Ok(bytes)
}

/// A regular file's bytes, refused past `max` bytes.
fn read_capped(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a file"));
    }
    if meta.len() > max {
        return Err(std::io::Error::other(format!("{} bytes is more than a font file holds", meta.len())));
    }
    let mut bytes = Vec::new();
    // the size checked above can change before the read: never read past the cap either
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(std::io::Error::other("the file grew past the size a font file holds"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest as _;

    const INTER: &[u8] = include_bytes!("../../../../assets/fonts/Inter-Regular.ttf");

    fn sha256(bytes: &[u8]) -> String {
        sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A package folder whose `fonts/manifest.txt` lists `faces` (family, file, bytes written), each
    /// with its file's checksum.
    fn package(tag: &str, faces: &[(&str, &str, &[u8])]) -> lcc_testkit::TempDir {
        let dir = lcc_testkit::TempDir::new(&format!("lc-package-fonts-{tag}"));
        let mut manifest = String::from("# family | style | file | scripts | licence | licence file | sha256 | source\n\n");
        for (family, file, bytes) in faces {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            manifest.push_str(&format!(
                "{family} | Regular | {file} | Jpan, Latn | OFL-1.1 | fonts/x/OFL.txt | {} | https://example.invalid\n",
                sha256(bytes)
            ));
        }
        std::fs::create_dir_all(dir.join("fonts")).unwrap();
        std::fs::write(dir.join("fonts/manifest.txt"), manifest).unwrap();
        dir
    }

    fn named(problems: &[FontProblem]) -> Vec<&str> {
        problems.iter().map(|p| p.what.as_str()).collect()
    }

    #[test]
    fn a_package_s_fonts_load_with_their_scripts() {
        let dir = package("ok", &[("One", "fonts/one/One.ttf", INTER), ("Two", "fonts/two/Two.ttf", INTER)]);
        let (faces, problems) = read_package(&dir);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(faces.iter().map(|f| f.family.as_str()).collect::<Vec<_>>(), ["One", "Two"]);
        assert!(faces.iter().all(|f| f.style == "Regular" && f.scripts == ["Jpan", "Latn"] && f.bytes == INTER));
        // kept for the process as the renderers need it, with the same fields
        let font = faces[0].clone().leak();
        assert_eq!((font.family, font.style, font.scripts, font.bytes), ("One", "Regular", &["Jpan", "Latn"][..], INTER));
    }

    /// AC: a missing or damaged font never crashes; it is left out and named, and the others load.
    #[test]
    fn a_missing_or_damaged_font_is_left_out_and_named_and_the_rest_load() {
        let dir = package("cut", &[("Gone", "fonts/a/Gone.ttf", INTER), ("Cut", "fonts/b/Cut.ttf", INTER), ("Kept", "fonts/c/Kept.ttf", INTER)]);
        std::fs::remove_file(dir.join("fonts/a/Gone.ttf")).unwrap();
        std::fs::write(dir.join("fonts/b/Cut.ttf"), &INTER[..INTER.len() / 2]).unwrap();
        let (faces, problems) = read_package(&dir);
        assert_eq!(faces.iter().map(|f| f.family.as_str()).collect::<Vec<_>>(), ["Kept"]);
        assert_eq!(named(&problems), ["Gone Regular", "Cut Regular"]);
        assert!(problems[0].detail.contains("could not be read"), "{}", problems[0]);
        assert!(problems[1].detail.contains("checksum"), "{}", problems[1]);
    }

    /// Bytes that match their checksum but aren't a font would make egui panic: refused here.
    #[test]
    fn a_file_that_is_not_a_font_is_refused_even_with_the_right_checksum() {
        let junk: &[u8] = b"not a font at all, but listed with its own checksum";
        let dir = package("junk", &[("Junk", "fonts/j/Junk.ttf", junk)]);
        let (faces, problems) = read_package(&dir);
        assert!(faces.is_empty());
        assert_eq!(named(&problems), ["Junk Regular"]);
        assert!(problems[0].detail.contains("not a font"), "{}", problems[0]);
    }

    /// A list naming a file outside the package (`..`, an absolute path) never reads it.
    #[test]
    fn a_listed_path_outside_the_package_is_never_read() {
        let outside = lcc_testkit::TempDir::new("lc-package-fonts-outside");
        std::fs::write(outside.join("Out.ttf"), INTER).unwrap();
        let dir = package("escape", &[]);
        let abs = outside.join("Out.ttf");
        let lines = [
            format!("Up | Regular | ../{}/Out.ttf | Latn | OFL-1.1 | x | {} | s", outside.file_name().unwrap().to_string_lossy(), sha256(INTER)),
            format!("Abs | Regular | {} | Latn | OFL-1.1 | x | {} | s", abs.display(), sha256(INTER)),
            "Short | Regular | fonts/x.ttf".to_string(),
        ];
        std::fs::write(dir.join("fonts/manifest.txt"), lines.join("\n")).unwrap();
        let (faces, problems) = read_package(&dir);
        assert!(faces.is_empty(), "{faces:?}");
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems[0].detail.contains("not a path inside the package") && problems[1].detail.contains("not a path inside the package"),
            "{problems:?}"
        );
        assert!(problems[2].detail.contains("malformed line"), "{}", problems[2]);
    }

    /// AC: the CLI started through a symlink elsewhere (`~/.local/bin/<cli>`) reads the fonts of the
    /// bundle it is in, and a font removed from that bundle is named while the others load.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_cli_reads_its_bundle_s_fonts() {
        let bundle = package("bundle", &[("Kept", "fonts/k/Kept.ttf", INTER), ("Removed", "fonts/r/Removed.ttf", INTER)]);
        // the package folder plays Contents/Resources; the program sits in Contents/MacOS beside it
        let contents = bundle.join("X.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::create_dir_all(contents.join("Resources")).unwrap();
        std::fs::rename(bundle.join("fonts"), contents.join("Resources/fonts")).unwrap();
        let cli = contents.join("MacOS/cli");
        std::fs::write(&cli, b"").unwrap();
        let link = bundle.join("bin/cli");
        std::fs::create_dir_all(bundle.join("bin")).unwrap();
        std::os::unix::fs::symlink(&cli, &link).unwrap();
        let root = package_root(&link).expect("a linked bundle program has a package root");
        assert_eq!(Some(root.clone()), package_root(&cli), "the link leads to the bundle's fonts");
        std::fs::remove_file(root.join("fonts/r/Removed.ttf")).unwrap();
        let (faces, problems) = read_package(&root);
        assert_eq!(faces.iter().map(|f| f.family.as_str()).collect::<Vec<_>>(), ["Kept"]);
        assert_eq!(named(&problems), ["Removed Regular"]);
    }

    #[test]
    fn a_package_without_its_font_list_has_one_problem_and_no_fonts() {
        let dir = lcc_testkit::TempDir::new("lc-package-fonts-none");
        let (faces, problems) = read_package(&dir);
        assert!(faces.is_empty());
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].what.ends_with("manifest.txt") && problems[0].detail.contains("could not be read"), "{}", problems[0]);
    }

    /// AC: the package's copy of the craft-fonts checkout loads to exactly the faces the build embeds
    /// (same order, scripts and bytes), so every font test of the embedded faces holds for it.
    #[test]
    fn a_package_copy_of_the_checkout_loads_what_the_build_embeds() {
        let (Some(checkout), false) = (option_env!("CRAFT_FONTS_CHECKOUT"), super::super::EMBEDDED.is_empty()) else {
            lcc_testkit::skip!(Fonts, "built without CRAFT_FONTS_DIR (or with CRAFT_FONTS_EMBED=0): nothing embedded to compare with");
            return;
        };
        let (faces, problems) = read_package(Path::new(checkout));
        assert!(problems.is_empty(), "{problems:?}");
        let embedded = super::super::EMBEDDED;
        assert_eq!(faces.len(), embedded.len());
        for (face, font) in faces.iter().zip(embedded) {
            assert_eq!((face.family.as_str(), face.style.as_str()), (font.family, font.style));
            assert!(face.scripts.iter().map(String::as_str).eq(font.scripts.iter().copied()), "{} scripts", face.family);
            assert!(face.bytes == font.bytes, "{} {} bytes", face.family, face.style);
        }
    }
}
