use super::*;

/// Each language through its menu command and the control channel's locale tags, saved by its
/// code: built-in labels follow it, user values and ids don't, the window paints its text and
/// the bundled fonts have its letters.
#[test]
fn languages_switch_persist_and_keep_user_values() {
    use crate::control::{ControlRequest, Outcome};
    let languages: [(Locale, &str, &[&str], &str); 8] = [
        (Locale::Cs, "czech", &["cs", "cs-CZ", "cs_CZ.UTF-8", "cs-Latn-CZ"], "áčďéěíňóřšťúůýžÁČĎÉĚÍŇÓŘŠŤÚŮÝŽ„“"),
        (Locale::Id, "indonesian", &["id", "id-ID", "id_ID.UTF-8", "id-Latn-ID"], ""),
        (Locale::Sv, "swedish", &["sv", "sv-SE", "sv_SE.UTF-8", "sv-FI"], "åäöÅÄÖé”’"),
        (Locale::Uk, "ukrainian", &["uk", "uk-UA", "uk_UA.UTF-8", "uk-Cyrl-UA"], ""),
        (Locale::Ka, "georgian", &["ka", "ka-GE", "ka_GE.UTF-8"], ""),
        (Locale::It, "italian", &["it", "it-IT", "it_IT.UTF-8", "it-Latn-IT", "it-CH"], ""),
        (Locale::Fi, "finnish", &["fi", "fi-FI", "fi_FI.UTF-8", "fi-Latn-FI"], ""),
        (Locale::Vi, "vietnamese", &["vi", "vi-VN", "vi_VN", "vi_VN.UTF-8"], ""),
    ];
    for (locale, command, tags, letters) in languages {
        let command = format!("app.language.{command}");
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        app.run(&command, serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, locale);
        assert_eq!(language(), locale);
        assert_eq!(crate::menubar::checked(&app, &command), Some(true));
        assert_eq!(builtin_label("Warm Glow", false), "Warm Glow");
        assert_eq!(builtin_label("Warm Glow", true), locale.tr("Warm Glow"));
        assert_eq!(tr("photo-{title}.jpg"), "photo-{title}.jpg");
        assert_eq!(tr("develop.set"), "develop.set");
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert!(saved.contains(&format!(r#""language":"{}""#, locale.code())), "{saved}");
        assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, locale);
        let ctx = egui::Context::default();
        for tag in tags {
            assert_eq!(Locale::parse_tag(tag), Some(locale), "{tag}");
            let (request, _) = ControlRequest::new("ui.set", serde_json::json!({"language": tag}));
            let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &request) else { panic!("expected reply") };
            assert_eq!(reply["ok"], true);
            assert_eq!(language(), locale);
        }
        let text = painted_text(&ctx, &mut app, locale);
        // (not the menu titles: a language whose titles don't fit shows one Menu button)
        assert!(text.contains(locale.tr("Exposure")), "{locale:?}: {text}");
        ctx.fonts_mut(|fonts| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                let font = egui::FontId::new(13.0, family);
                for ch in letters.chars() {
                    assert!(fonts.has_glyph(&font, ch), "{locale:?}: missing glyph {ch}");
                }
            }
        });
    }
    set_language(Locale::En);
}

#[test]
fn sentence_case_keeps_georgian_lowercase() {
    assert_eq!(upper_first("its input is not an image"), "Its input is not an image");
    assert_eq!(upper_first("ფაილი ვერ იკითხება"), "ფაილი ვერ იკითხება");
    assert_eq!(upper_first("ᲥᲐᲠᲗᲣᲚᲘ"), "ᲥᲐᲠᲗᲣᲚᲘ");
    assert_eq!(upper_first(""), "");
    assert!(capitalizes('a') && capitalizes('ж') && !capitalizes('ქ') && !capitalizes('Ⴀ') && !capitalizes('ⴀ'));
}

#[test]
fn every_language_has_a_name_a_script_and_its_own_code() {
    assert!(crate::i18n::Locale::En.catalog().is_empty(), "the source language needs no catalog");
    for language in Locale::ALL {
        assert!(!language.name().is_empty(), "{language:?} has a name");
        assert!(!language.script().is_empty(), "{language:?} has a script");
        assert_eq!(Locale::parse(language.code()), Some(*language), "{language:?} parses its own code");
    }
}

/// Every catalog file is a JSON object of strings, each key once: a malformed file would load as
/// no catalog, and a repeated key silently drops one of its translations.
#[test]
fn catalogs_load() {
    let files = catalog_files();
    assert_eq!(files.len(), 2 * (Locale::ALL.len() - 1), "a plain and a formats catalog per translation");
    for path in files {
        let repeated = repeated_keys(&path);
        assert_eq!(repeated, Ok(Vec::new()), "{}", path.display());
    }
}

/// `locales/*.json`, sorted.
fn catalog_files() -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let mut files: Vec<std::path::PathBuf> =
        std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.extension().is_some_and(|ext| ext == "json")).collect();
    files.sort();
    files
}

/// The keys a catalog file names more than once, or why it isn't an object of strings.
fn repeated_keys(path: &std::path::Path) -> Result<Vec<String>, String> {
    struct Repeated;
    impl<'de> serde::de::Visitor<'de> for Repeated {
        type Value = Vec<String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an object of strings")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Vec<String>, A::Error> {
            let (mut seen, mut repeated) = (std::collections::BTreeSet::new(), Vec::new());
            while let Some((key, _)) = map.next_entry::<String, String>()? {
                if !seen.insert(key.clone()) {
                    repeated.push(key);
                }
            }
            Ok(repeated)
        }
    }
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde::Deserializer::deserialize_map(&mut serde_json::Deserializer::from_str(&text), Repeated).map_err(|error| error.to_string())
}

/// A translation carries the placeholders of the message it translates.
#[test]
fn catalogs_agree_on_placeholders() {
    let fields = |text: &str| {
        let mut out: Vec<String> = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let Some(end) = rest[start..].find('}') else { break };
            out.push(rest[start + 1..start + end].split(':').next().unwrap_or("").to_string());
            rest = &rest[start + end + 1..];
        }
        out.sort();
        out
    };
    for language in Locale::ALL {
        for (key, value) in language.catalog() {
            assert_eq!(fields(key), fields(value), "{language:?}: {key:?} -> {value:?}");
        }
    }
}

/// Locale tags are matched leniently, so a system locale reaches a shipped language.
/// The message box for a desktop start that failed (issue #260) names the error and the log
/// file, in the UI language.
#[test]
fn startup_failure_message_names_the_error_and_the_log() {
    let (title, text) = startup_failed_message("no adapter", Some("/home/a/.config/lightcraft/logs/lightcraft.log"));
    assert_eq!(title, "LightCraft could not start");
    assert!(text.contains("no adapter") && text.ends_with("lightcraft.log"), "{text}");
    assert!(!startup_failed_message("no adapter", None).1.contains("log file"));
}

#[test]
fn locale_tags_normalize() {
    assert_eq!(Locale::parse_tag("en"), Some(Locale::En));
    assert_eq!(Locale::parse_tag("en-US"), Some(Locale::En));
    assert_eq!(Locale::parse_tag("zh-hans"), Some(Locale::ZhHans));
    assert_eq!(Locale::parse_tag("zh"), Some(Locale::ZhHans));
    assert_eq!(Locale::parse_tag("zh_CN"), Some(Locale::ZhHans));
    assert_eq!(Locale::parse_tag("zh-hant"), Some(Locale::ZhHant));
    assert_eq!(Locale::parse_tag("zh-Hant"), Some(Locale::ZhHant));
    assert_eq!(Locale::parse_tag("zh-TW"), Some(Locale::ZhHant));
    assert_eq!(Locale::parse_tag("zh_TW.UTF-8"), Some(Locale::ZhHant));
    assert_eq!(Locale::parse_tag("zh-HK"), Some(Locale::ZhHant));
    assert_eq!(Locale::parse_tag("zh-Hant-TW"), Some(Locale::ZhHant));
    assert_eq!(Locale::parse_tag("zh-CN"), Some(Locale::ZhHans));
    assert_eq!(Locale::parse_tag("ja_JP.UTF-8"), Some(Locale::Ja));
    assert_eq!(Locale::parse_tag("pt-br"), Some(Locale::PtBr));
    assert_eq!(Locale::parse_tag("pt-BR"), Some(Locale::PtBr));
    assert_eq!(Locale::parse_tag("pt_BR.UTF-8"), Some(Locale::PtBr));
    assert_eq!(Locale::parse_tag("pt"), Some(Locale::PtBr));
    assert_eq!(Locale::parse_tag("fr"), Some(Locale::Fr));
    assert_eq!(Locale::parse_tag("fr-FR"), Some(Locale::Fr));
    assert_eq!(Locale::parse_tag("fr_FR.UTF-8"), Some(Locale::Fr));
    for tag in ["de", "de-DE", "de_AT.UTF-8", "de-CH"] {
        assert_eq!(Locale::parse_tag(tag), Some(Locale::De), "{tag}");
    }
    for tag in ["ru", "ru-RU", "ru_RU.UTF-8", "ru-UA"] {
        assert_eq!(Locale::parse_tag(tag), Some(Locale::Ru), "{tag}");
    }
    for tag in ["es", "es-ES", "es_MX.UTF-8", "es-419"] {
        assert_eq!(Locale::parse_tag(tag), Some(Locale::Es), "{tag}");
    }
    for tag in ["vi", "vi-VN", "vi_VN", "vi_VN.UTF-8"] {
        assert_eq!(Locale::parse_tag(tag), Some(Locale::Vi), "{tag}");
    }
    assert_eq!(Locale::parse_tag("xx"), None);
}

/// The web host's "Auto" (issue #2085): the browser's `navigator.languages` in order, first
/// shipped language wins, unsupported tags are skipped, an empty or foreign list stays English
/// ([`default_language`] fallback).
#[test]
fn locale_from_tags_takes_the_first_shipped_language() {
    assert_eq!(locale_from_tags(&[]), None);
    assert_eq!(locale_from_tags(&["xx-XX".into()]), None);
    assert_eq!(locale_from_tags(&["it-IT".into()]), Some(Locale::It));
    assert_eq!(locale_from_tags(&["xx".into(), "de-DE".into(), "fr".into()]), Some(Locale::De));
    assert_eq!(locale_from_tags(&["zh-TW".into(), "en".into()]), Some(Locale::ZhHant));
    assert_eq!(locale_from_tags(&["ja_JP.UTF-8".into(), "en".into()]), Some(Locale::Ja));
    assert_eq!(locale_from_tags(&["pt".into(), "pt-BR".into()]), Some(Locale::PtBr));
}

#[test]
fn switching_language_translates_and_restores() {
    for language in Locale::ALL {
        set_language(*language);
        for source in ["Exposure", "Settings"] {
            assert_eq!(tr(source), language.catalog().get(source).map_or(source, String::as_str), "{language:?}");
        }
        // Untranslated and data-like text passes through untouched.
        assert_eq!(tr("my-photo.jpg"), "my-photo.jpg");
        assert_eq!(tr("develop.set"), "develop.set");
        assert_eq!(tr("A string no catalog has"), "A string no catalog has");
    }
    set_language(Locale::En);
    assert_eq!(tr("Exposure"), "Exposure");
}

fn assert_format_specs(language: &Locale) {
    let text = tr_format!("{count} smart preview{} · {:.1} MB", "s", 12.345, count = 3);
    assert!(text.contains("12.3") && !text.contains("12.34"), "{language:?}: {text}");
    let merging = tr_format!("Merging… {stage} {:.0}%", 42.4, stage = "Aligning");
    assert!(merging.contains("42%"), "{language:?}: {merging}");
    let delta = tr_format!("{label} {d:+} on every selected photo", label = "Exposure", d = 5);
    assert!(delta.contains("+5"), "{language:?}: {delta}");
    let reading = tr_format!("Reading photos… {} of {}", 3, 10);
    let (three, ten) = (reading.find('3'), reading.find("10"));
    assert!(three.is_some() && ten.is_some(), "{language:?}: {reading}");
}

fn assert_format_values_kept(language: &Locale) {
    let name = "Color 1234.56 {title} 日本語";
    let added = tr_format!("Added {n} photo{} to “{}”", "s", name, n = 22);
    assert!(added.contains(name) && added.contains("22"), "{language:?}: {added}");
    for n in [0, 1, 2, 5, 22] {
        let imported = tr_format!("Imported {} photo{}", n, if n == 1 { "" } else { "s" });
        assert!(imported.contains(&n.to_string()), "{language:?}: {imported}");
    }
    // The trailing argument is queue progress, not an English plural suffix.
    let queued = tr_format!(" · {queued} waiting", queued = 5);
    let denoising = tr_format!("Denoising {name}: {done} of {total} tiles{}", queued, name = "IMG.CR3", done = 1, total = 4);
    assert!(denoising.contains("IMG.CR3") && denoising.contains('5'), "{language:?}: {denoising}");
}

/// Format strings carry their values in every language, and a language without a translation
/// falls back to the English format rather than dropping the values.
#[test]
fn formats_render_in_every_language() {
    // Every language's format string is checked by `format!` against the call site's values.
    set_language(Locale::En);
    assert_eq!(tr_format!("Imported {} photo{}", 12, "s"), "Imported 12 photos");
    assert_eq!(tr_format!("Exported {ok} of {total} photo{}", "s", ok = 4, total = 12), "Exported 4 of 12 photos");
    // A message no catalog has formats its English source.
    assert_eq!(tr_format!("{} of {} (no catalog has this)", 1, 2), "1 of 2 (no catalog has this)");
    // Format specs survive translation: precision, sign, and values a language reorders; user
    // text (an album name with braces and CJK) and counts are never dropped.
    for language in Locale::ALL {
        set_language(*language);
        assert_format_specs(language);
        assert_format_values_kept(language);
    }
    set_language(Locale::En);
    assert_eq!(tr_format!("Reading photos… {} of {}", 3, 10), "Reading photos… 3 of 10");
}

/// Every character a catalog paints has a glyph in the fonts the UI installs, so a translation
/// never shows as boxes. Without craft-fonts there are no CJK faces at all, and the CJK
/// catalogs are skipped (the same degradation the Japanese UI has always had).
#[test]
fn catalog_characters_are_paintable() {
    let ctx = egui::Context::default();
    ctx.set_fonts(crate::theme::font_definitions(lightcraft_engine::CRAFT_FONTS));
    let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
    out.textures_delta.clear();
    for language in Locale::ALL {
        let messages = language.catalog();
        if messages.is_empty() {
            continue;
        }
        // The faces this language's script needs; a language the craft-fonts input does not
        // cover (a translation added ahead of its font) is reported, not failed.
        let mut faces: Vec<&str> =
            lightcraft_engine::CRAFT_FONTS.iter().filter(|font| font.serves(language.script())).map(|font| font.family).collect();
        faces.dedup();
        if faces.is_empty() {
            lightcraft_testkit::skip!(Fonts, "{}: built without a craft-fonts face for {}", language.code(), language.script());
            continue;
        }
        ctx.fonts_mut(|fonts| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
                let font = egui::FontId::new(13.0, family.clone());
                let mut missing: Vec<char> =
                    messages.values().flat_map(|message| message.chars()).filter(|ch| !ch.is_whitespace() && !fonts.has_glyph(&font, *ch)).collect();
                missing.sort_unstable();
                missing.dedup();
                assert!(
                    missing.is_empty(),
                    "{} in {family:?} (faces: {faces:?}) lack glyphs: {}",
                    language.code(),
                    missing.iter().collect::<String>()
                );
            }
        });
    }
}

/// The interface's own symbols paint in every language: the CJK faces stop at Han and kana, so a
/// translation must not carry a symbol no installed font has (egui's defaults cover the rest).
#[test]
fn interface_symbols_are_paintable() {
    let ctx = egui::Context::default();
    ctx.set_fonts(crate::theme::font_definitions(lightcraft_engine::CRAFT_FONTS));
    let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
    out.textures_delta.clear();
    // ⌥ and ⌫ come from egui's bundled fonts; ▸ and ▾ are in neither (measured 2026-10-07).
    ctx.fonts_mut(|fonts| {
        for symbol in ['⌥', '⌫', '⇧', '⌘', '→', '…', '—', '“', '”', '·', '»', '⌄', '★'] {
            let font = egui::FontId::new(13.0, egui::FontFamily::Proportional);
            assert!(fonts.has_glyph(&font, symbol), "{symbol:?} has no glyph in any installed font");
        }
    });
}

/// Symbols a translation must not carry, because the craft-fonts faces of at least one shipped
/// script have no glyph for them and egui's bundled fonts do not either: the menu triangles,
/// and the heavy multiplication X (U+2715) that Noto Sans Georgian lacks — `×` (U+00D7) reads
/// the same and every face has it.
const UNPAINTABLE: [char; 3] = ['▸', '▾', '✕'];

#[test]
fn translated_catalogs_do_not_use_missing_menu_triangles() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let messages: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (source, text) in messages {
            for bad in UNPAINTABLE {
                assert!(!text.contains(bad), "{}: {source:?} uses the unpaintable {bad:?}", path.display());
            }
        }
    }
}

/// The string literals of non-test code in `src`, with the line each starts on. Code under
/// `#[cfg(test)]` and comments are left out; escapes are decoded where they matter here (`\u{…}`,
/// line continuations).
fn string_literals(src: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = src.chars().collect();
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    // where the inside of a `#[cfg(…)]` or `#![cfg(…)]` attribute starts; other attributes are read
    // as code, so the strings in `#[error("…")]` and `#[cfg_attr(…, error("…"))]` are checked
    let cfg_start = |i: usize| {
        let open = if at(i) == '#' && at(i + 1) == '[' {
            i + 2
        } else if at(i) == '#' && at(i + 1) == '!' && at(i + 2) == '[' {
            i + 3
        } else {
            return None;
        };
        let name = (open..chars.len()).find(|&j| !chars[j].is_whitespace())?;
        let paren = (name + 3..chars.len()).find(|&j| !chars[j].is_whitespace())?;
        (chars[name..].starts_with(&['c', 'f', 'g']) && chars[paren] == '(').then_some(open)
    };
    // whether `test` is one of the top-level conditions of an `all(…)`: `all(unix, any(test, …))`
    // can ship without it
    let all_has_test = |args: &str| {
        let (mut level, mut from) = (0, 0);
        for (k, ch) in args.char_indices().chain([(args.len(), ',')]) {
            match ch {
                '(' => level += 1,
                ')' => level -= 1,
                ',' if level == 0 => {
                    if &args[from..k] == "test" {
                        return true;
                    }
                    from = k + 1;
                }
                _ => {}
            }
        }
        false
    };
    let (mut out, mut i, mut line) = (Vec::new(), 0, 1);
    let (mut depth, mut test_depth, mut pending_test) = (0usize, None, false);
    while i < chars.len() {
        let c = chars[i];
        let raw_start = if c == 'r' {
            Some(i + 1)
        } else if c == 'b' && at(i + 1) == 'r' {
            Some(i + 2)
        } else {
            None
        };
        let raw_hashes = raw_start.filter(|_| i == 0 || !ident(at(i - 1))).and_then(|start| {
            let hashes = chars[start..].iter().take_while(|&&h| h == '#').count();
            (at(start + hashes) == '"').then_some((start, hashes))
        });
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == '*' {
            skip_block_comment(&chars, &mut i, &mut line);
        } else if let Some(start) = cfg_start(i) {
            // `#[cfg(test)]` (spaces allowed) and `#[cfg(all(test, …))]` mark the next item as test
            // code up to its `;` or through its body, `#![cfg(test)]` the rest of the file;
            // `cfg(any(test, …))` code can ship, so it stays
            let mut end = start;
            let mut nested = 1;
            while end < chars.len() && nested > 0 {
                nested = match chars[end] {
                    '[' => nested + 1,
                    ']' => nested - 1,
                    _ => nested,
                };
                line += usize::from(chars[end] == '\n');
                end += 1;
            }
            let attr: String = chars[start..end.saturating_sub(1)].iter().filter(|ch| !ch.is_whitespace()).collect();
            let all_test = attr.strip_prefix("cfg(all(").and_then(|rest| rest.strip_suffix("))")).is_some_and(all_has_test);
            if attr == "cfg(test)" || all_test {
                if c == '#' && at(i + 1) == '!' {
                    break;
                }
                pending_test = true;
            }
            i = end;
        } else if let Some((start, hashes)) = raw_hashes {
            let (first, mut text) = (line, String::new());
            i = start + hashes + 1;
            while i < chars.len() && !(chars[i] == '"' && chars[i + 1..].iter().take(hashes).filter(|&&h| h == '#').count() == hashes) {
                line += usize::from(chars[i] == '\n');
                text.push(chars[i]);
                i += 1;
            }
            i += 1 + hashes;
            if test_depth.is_none() && !pending_test {
                out.push((first, text));
            }
        } else if c == '"' {
            let first = line;
            let text = quoted_literal(&chars, &mut i, &mut line);
            if test_depth.is_none() && !pending_test {
                out.push((first, text));
            }
        } else if c == '\'' {
            // a char literal ('"', '\'', '{') or a lifetime ('a)
            i += if at(i + 1) == '\\' {
                chars[i + 3..].iter().position(|&q| q == '\'').map_or(1, |end| end + 4)
            } else if at(i + 2) == '\'' {
                3
            } else {
                1
            };
        } else {
            match c {
                '{' => {
                    depth += 1;
                    if pending_test && test_depth.is_none() {
                        test_depth = Some(depth);
                    }
                    pending_test = false;
                }
                '}' => {
                    if test_depth == Some(depth) {
                        test_depth = None;
                    }
                    depth = depth.saturating_sub(1);
                }
                ';' => pending_test = false,
                _ => {}
            }
            i += 1;
        }
    }
    out
}

/// Skips the block comment (nested ones too) that starts at `chars[*i]`, counting its lines.
fn skip_block_comment(chars: &[char], i: &mut usize, line: &mut usize) {
    let at = |k: usize| chars.get(k).copied().unwrap_or('\0');
    let mut nested = 0;
    while *i < chars.len() {
        if chars[*i] == '/' && at(*i + 1) == '*' {
            nested += 1;
            *i += 2;
        } else if chars[*i] == '*' && at(*i + 1) == '/' {
            nested -= 1;
            *i += 2;
            if nested == 0 {
                break;
            }
        } else {
            *line += usize::from(chars[*i] == '\n');
            *i += 1;
        }
    }
}

/// The text of the `"…"` literal that starts at `chars[*i]`, with `\u{…}`, line continuations and
/// other escapes decoded; `*i` ends past its closing quote.
fn quoted_literal(chars: &[char], i: &mut usize, line: &mut usize) -> String {
    let at = |k: usize| chars.get(k).copied().unwrap_or('\0');
    let mut text = String::new();
    *i += 1;
    while *i < chars.len() && chars[*i] != '"' {
        if chars[*i] != '\\' {
            *line += usize::from(chars[*i] == '\n');
            text.push(chars[*i]);
            *i += 1;
            continue;
        }
        match at(*i + 1) {
            '\n' => {
                *line += 1;
                *i += 2;
                while at(*i).is_whitespace() {
                    *line += usize::from(at(*i) == '\n');
                    *i += 1;
                }
            }
            'u' => {
                let hex: String = chars[*i + 3..].iter().take_while(|&&h| h != '}').collect();
                text.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                *i += 4 + hex.len();
            }
            escaped => {
                text.push(escaped);
                *i += 2;
            }
        }
    }
    *i += 1;
    text
}

#[test]
fn string_literals_skip_comments_tests_and_char_literals() {
    let src = r##"// "a"
fn f() { let q = '"'; let e = '\''; let s = "one ▸ \"two\""; }
/* "b" */ const R: &str = r#"raw "x""#;
fn g<'a>(x: &'a str) -> &'a str { "long \
  line \u{2192}" }
#[cfg(test)]
mod tests { fn t() { "test"; } }
const Z: &str = "after";
#[ cfg( test ) ] fn spaced() { "test"; }
#[cfg(all(unix, test))] fn both() { "test"; }
#[cfg(any(test, feature = "x"))] fn either() { "either"; }
#[error("shown")] struct E;
#[cfg(all(unix, any(test, feature = "y")))] fn nested() { "nested"; }
#[cfg(all(test, not(feature = "y")))] fn negated() { "test"; }
#[cfg(test)] const SYMBOL: &str = "test";
const KEPT: &str = "kept";
#[cfg_attr(feature = "y", error("conditional"))] struct C;
#![cfg(test)]
fn rest() { "test"; }"##;
    let found = string_literals(src);
    let found: Vec<(usize, &str)> = found.iter().map(|(line, text)| (*line, text.as_str())).collect();
    let expected = [
        (2, "one ▸ \"two\""),
        (3, "raw \"x\""),
        (4, "long line →"),
        (8, "after"),
        (11, "either"),
        (12, "shown"),
        (13, "nested"),
        (16, "kept"),
        (17, "y"),
        (17, "conditional"),
    ];
    assert_eq!(found, expected);
}

/// Every symbol in the interface's own text has a glyph (issue #1950: ▸ showed as a box in the
/// English strings, which untranslated messages fall back to). Checked on the string literals of
/// the code, not just the catalogs, with the fonts the build installs: a build without craft-fonts
/// checks Inter and egui's own fonts alone, so a symbol can't lean on a CJK face. Letters and the
/// full-width forms only CJK text uses are `catalog_characters_are_paintable`'s business.
/// Test code is left out: `tests/` directories, files named `tests.rs`, `tests_*.rs` or
/// `*_tests.rs`, and items under `#[cfg(test)]` or `#[cfg(all(test, …))]`. A string only a test
/// uses belongs in one of those, or it is checked like interface text.
#[test]
fn source_strings_use_paintable_symbols() {
    let ctx = egui::Context::default();
    ctx.set_fonts(crate::theme::font_definitions(lightcraft_engine::CRAFT_FONTS));
    let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
    out.textures_delta.clear();
    let cjk_only = |ch: char| matches!(ch, '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ffef}');
    // noncharacters are sentinels (an IndexedDB key range's end), never shown
    let text = |ch: char| u32::from(ch) & 0xfffe != 0xfffe && !matches!(ch, '\u{fdd0}'..='\u{fdef}') && !ch.is_control();
    let checked = |ch: &char| !ch.is_ascii() && !ch.is_whitespace() && !ch.is_alphanumeric() && !cjk_only(*ch) && text(*ch);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for group in ["crates", "apps"] {
        for krate in std::fs::read_dir(root.join(group)).unwrap() {
            let mut dirs = vec![krate.unwrap().path().join("src")];
            while let Some(dir) = dirs.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else { continue };
                for entry in entries {
                    let path = entry.unwrap().path();
                    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_string();
                    if path.is_dir() {
                        if name != "tests" {
                            dirs.push(path);
                        }
                    } else if name.ends_with(".rs") && !name.starts_with("tests_") && !name.ends_with("_tests.rs") && name != "tests.rs" {
                        files.push(path);
                    }
                }
            }
        }
    }
    assert!(files.len() > 100, "found the workspace's sources ({} files)", files.len());
    let mut texts: Vec<(String, String)> = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let shown = path.strip_prefix(&root).unwrap_or(path).display().to_string();
        texts.extend(string_literals(&src).into_iter().map(|(line, text)| (format!("{shown}:{line}"), text)));
    }
    // What's New as the dialog shows it
    for line in crate::panels::dialogs::WHATS_NEW.lines().map(str::trim) {
        let line = line.strip_prefix("### ").or_else(|| line.strip_prefix("## ")).or_else(|| line.strip_prefix("- ")).unwrap_or(line);
        texts.push(("docs/whats-new.md".into(), crate::panels::dialogs::whats_new_line(line)));
    }
    let font = egui::FontId::new(13.0, egui::FontFamily::Proportional);
    let mut missing: Vec<String> = Vec::new();
    ctx.fonts_mut(|fonts| {
        for (place, text) in &texts {
            for ch in text.chars().filter(checked) {
                if !fonts.has_glyph(&font, ch) {
                    missing.push(format!("{place}: {ch:?} (U+{:04X})", u32::from(ch)));
                }
            }
        }
    });
    missing.dedup();
    assert!(missing.is_empty(), "symbols no UI font has (they paint as boxes):\n{}", missing.join("\n"));
}

/// The language menu covers every language, and a language's own command selects it.
#[test]
fn language_commands_cover_every_language() {
    let commands = [
        ("app.language.traditionalChinese", Locale::ZhHant),
        ("app.language.english", Locale::En),
        ("app.language.simplifiedChinese", Locale::ZhHans),
        ("app.language.japanese", Locale::Ja),
        ("app.language.portuguese", Locale::PtBr),
        ("app.language.french", Locale::Fr),
        ("app.language.spanish", Locale::Es),
        ("app.language.german", Locale::De),
        ("app.language.russian", Locale::Ru),
        ("app.language.ukrainian", Locale::Uk),
        ("app.language.polish", Locale::Pl),
        ("app.language.czech", Locale::Cs),
        ("app.language.italian", Locale::It),
        ("app.language.georgian", Locale::Ka),
        ("app.language.swedish", Locale::Sv),
        ("app.language.finnish", Locale::Fi),
        ("app.language.indonesian", Locale::Id),
        ("app.language.vietnamese", Locale::Vi),
    ];
    // One command per language, and every command reachable from the menu table.
    assert_eq!(commands.len(), Locale::ALL.len());
    for (id, language) in commands {
        assert_eq!(crate::menus::language_from_command(id), Some(language), "{id}");
        assert!(crate::menus::ui_commands().any(|command| command.0 == id), "{id} is not in the menu");
    }
    assert_eq!(crate::menus::language_from_command("app.language.klingon"), None);
    assert_eq!(crate::menus::language_from_command("view.detail"), None);
}

#[test]
fn preferences_round_trip_and_old_settings_remain_readable() {
    let old: crate::state::UiState = serde_json::from_str("{}").unwrap();
    assert_eq!(old.language, Locale::En);
    // Settings written before the language list grew still load.
    let legacy: crate::state::UiState = serde_json::from_str(r#"{"language":"ja"}"#).unwrap();
    assert_eq!(legacy.language, Locale::Ja);
    let settings = crate::state::UiState { language: Locale::ZhHans, ..old };
    let saved = serde_json::to_string(&settings).unwrap();
    let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.language, Locale::ZhHans);
}

/// Smart-album choice values read as words in every language (English "Public Domain"), never as
/// rule ids like `publicDomain`; an id the field doesn't know shows as written. A yes/no value
/// reads Yes / No, or the language's contextual entry for it where it has one.
#[test]
fn choice_values_read_as_words_in_every_language() {
    use lightcraft_catalog::rules::{FIELDS, Kind, bool_label};
    assert_eq!(choice_text_in(Locale::En, "copyrightStatus", "publicDomain"), "Public Domain");
    assert_eq!((bool_text_in(Locale::En, true), bool_text_in(Locale::En, false)), ("Yes", "No"));
    assert_eq!(tr_ctx_in(Locale::En, BOOL_CONTEXT, "Yes"), "Yes");
    for language in Locale::ALL {
        assert_eq!(choice_text_in(*language, "copyrightStatus", "someday"), "someday");
        assert_eq!(tr_ctx_in(*language, "nope", "Yes"), language.tr("Yes"), "no contextual entry: the plain message");
        for b in [true, false] {
            let contextual = language.catalog().get(&format!("{BOOL_CONTEXT}|{}", bool_label(b)));
            assert_eq!(bool_text_in(*language, b), contextual.map_or(language.tr(bool_label(b)), String::as_str));
        }
        for (field, _, kind) in FIELDS {
            let Kind::Choice(choices) = kind else { continue };
            for (id, _) in *choices {
                assert_ne!(choice_text_in(*language, field, id), *id, "{}: {field} {id}", language.code());
            }
        }
    }
    // a contextual entry names a real value (a typo would silently fall back to the plain Yes / No)
    let values = [bool_label(true), bool_label(false)];
    for language in Locale::ALL {
        for key in language.catalog().keys() {
            if let Some(value) = key.strip_prefix(BOOL_CONTEXT).and_then(|k| k.strip_prefix('|')) {
                assert!(values.contains(&value), "{}: {key:?} isn't a yes/no value", language.code());
            }
        }
    }
}

/// A rule's problem reads as the field's name and the issue, in the editor's language, joined by
/// a full-width colon in Japanese and Chinese; the sidebar's list puts the rule's position first.
#[test]
fn problems_read_in_every_language() {
    use lightcraft_catalog::rules::Issue;
    let cat = lightcraft_catalog::Catalog::new();
    let problems =
        |rules: serde_json::Value| serde_json::from_value::<lightcraft_catalog::RuleSet>(serde_json::json!({"rules": rules})).unwrap().check(&cat);
    let p = problems(serde_json::json!([{"field": "rating", "op": "gte", "value": 3}, {"field": "title", "op": "contains", "value": ""}]));
    assert_eq!(problem_text_in(Locale::En, &p[0]), "Title: needs something to look for (or use “is empty”)");
    assert_eq!(problem_line_in(Locale::En, &p[0]), "#2 Title: needs something to look for (or use “is empty”)");
    for language in Locale::ALL {
        let colon = if matches!(language.script(), "Jpan" | "Hans" | "Hant") { "：" } else { ": " };
        let text = format!("{}{colon}{}", language.tr("Title"), language.tr(p[0].issue.text()));
        assert_eq!(problem_text_in(*language, &p[0]), text);
        assert_eq!(problem_line_in(*language, &p[0]), format!("#2 {text}"));
    }
    // a problem with the album filter itself has no rule position to show
    let filter_loop =
        lightcraft_catalog::rules::Problem { path: Vec::new(), field: Some("album".into()), issue: Issue::AlbumLoop, message: String::new() };
    assert!(problem_line_in(Locale::En, &filter_loop).starts_with("Album: "), "{}", problem_line_in(Locale::En, &filter_loop));
    let g = problems(serde_json::json!([{"group": {"rules": []}}]));
    assert_eq!(problem_text_in(Locale::En, &g[0]), "This group is empty: add a rule or remove it.");
}

#[test]
fn ukrainian_panels_dialogs_and_letters_paint_with_bundled_fonts() {
    let ctx = fonts_ctx(&[]);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.ui.left_panel = true;
    let text = painted_text(&ctx, &mut app, Locale::Uk);
    assert!(text.contains(Locale::Uk.tr("My Photos")) && text.contains(Locale::Uk.tr("All Photos")), "{text}");
    for (command, title) in [("app.about", "About LightCraft"), ("app.shortcuts", "Keyboard Shortcuts"), ("app.settings", "Settings")] {
        app.ui.dialog = None;
        app.run(command, serde_json::json!({})).unwrap();
        let text = painted_text(&ctx, &mut app, Locale::Uk);
        assert!(text.contains(Locale::Uk.tr(title)), "{command}: {text}");
    }
    ctx.fonts_mut(|fonts| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
            let font = egui::FontId::new(13.0, family);
            for ch in "ҐґЄєІіЇї’«»"
                .chars()
                .chain(Locale::Uk.catalog().values().flat_map(|text| text.chars()).filter(|ch| ('\u{0400}'..='\u{04ff}').contains(ch)))
            {
                assert!(fonts.has_glyph(&font, ch), "missing Ukrainian glyph {ch}");
            }
        }
    });
    set_language(Locale::En);
}

#[test]
fn italian_panels_dialogs_and_letters_paint_with_bundled_fonts() {
    let ctx = fonts_ctx(&[]);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.ui.left_panel = true;
    let text = painted_text(&ctx, &mut app, Locale::It);
    assert!(text.contains(Locale::It.tr("My Photos")) && text.contains(Locale::It.tr("All Photos")), "{text}");
    for (command, title) in [("app.about", "About LightCraft"), ("app.shortcuts", "Keyboard Shortcuts"), ("app.settings", "Settings")] {
        app.ui.dialog = None;
        app.run(command, serde_json::json!({})).unwrap();
        let text = painted_text(&ctx, &mut app, Locale::It);
        assert!(text.contains(Locale::It.tr(title)), "{command}: {text}");
    }
    ctx.fonts_mut(|fonts| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
            let font = egui::FontId::new(13.0, family);
            for ch in "àèéìòùÀÈÉÌÒÙ’“”«»"
                .chars()
                .chain(Locale::It.catalog().values().flat_map(|text| text.chars()).filter(|ch| ('\u{00c0}'..='\u{017f}').contains(ch)))
            {
                assert!(fonts.has_glyph(&font, ch), "missing Italian glyph {ch}");
            }
        }
    });
    set_language(Locale::En);
}

#[test]
fn finnish_panels_dialogs_and_letters_paint_with_bundled_fonts() {
    let ctx = fonts_ctx(&[]);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.ui.left_panel = true;
    let text = painted_text(&ctx, &mut app, Locale::Fi);
    assert!(text.contains(Locale::Fi.tr("My Photos")) && text.contains(Locale::Fi.tr("All Photos")), "{text}");
    for (command, title) in [("app.about", "About LightCraft"), ("app.shortcuts", "Keyboard Shortcuts"), ("app.settings", "Settings")] {
        app.ui.dialog = None;
        app.run(command, serde_json::json!({})).unwrap();
        let text = painted_text(&ctx, &mut app, Locale::Fi);
        assert!(text.contains(Locale::Fi.tr(title)), "{command}: {text}");
    }
    ctx.fonts_mut(|fonts| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
            let font = egui::FontId::new(13.0, family);
            for ch in "äöåÄÖÅ”–"
                .chars()
                .chain(Locale::Fi.catalog().values().flat_map(|text| text.chars()).filter(|ch| ('\u{00c0}'..='\u{017f}').contains(ch)))
            {
                assert!(fonts.has_glyph(&font, ch), "missing Finnish glyph {ch}");
            }
        }
    });
    set_language(Locale::En);
}

#[test]
fn german_switches_via_menu_and_control_and_persists() {
    use crate::control::{ControlRequest, Outcome};
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.run("app.language.german", serde_json::json!({})).unwrap();
    assert_eq!(app.ui.language, Locale::De);
    assert_eq!(language(), Locale::De);
    assert_eq!(crate::menubar::checked(&app, "app.language.german"), Some(true));
    let saved = serde_json::to_string(&app.ui).unwrap();
    assert!(saved.contains(r#""language":"de""#));
    assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, Locale::De);
    let ctx = egui::Context::default();
    for code in ["de-DE", "de_AT.UTF-8", "de-CH"] {
        let (req, _) = ControlRequest::new("ui.set", serde_json::json!({"language": code}));
        let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &req) else { panic!("expected reply") };
        assert_eq!(reply["ok"], true);
        assert_eq!(language(), Locale::De);
    }
    assert_eq!(builtin_label("Warm Glow", false), "Warm Glow");
    assert_eq!(builtin_label("Warm Glow", true), Locale::De.tr("Warm Glow"));
    assert_eq!(tr("my-photo.jpg"), "my-photo.jpg");
    let rules = lightcraft_catalog::RuleSet {
        mode: lightcraft_catalog::Match::All,
        rules: vec![lightcraft_catalog::Rule::Field { field: "keywords".into(), op: "contains".into(), value: serde_json::json!("Color") }],
    };
    let mut catalog = lightcraft_catalog::Catalog::new();
    catalog
        .apply(lightcraft_catalog::Op::AddAlbum { album: lightcraft_catalog::Album::new(lightcraft_catalog::AlbumId(4), "Ausgeschlossen") })
        .unwrap();
    // "Color" is a message too, but a rule's free-text value stays as written
    let keywords = lightcraft_catalog::rules::field_label("keywords").unwrap_or("keywords");
    assert_eq!(rules_label(&rules, &catalog), format!("{} {} Color", Locale::De.tr(keywords), Locale::De.tr("contains")));
    let album = lightcraft_catalog::RuleSet {
        mode: lightcraft_catalog::Match::All,
        rules: vec![lightcraft_catalog::Rule::Field { field: "album".into(), op: "isNot".into(), value: serde_json::json!(4) }],
    };
    assert!(rules_label(&album, &catalog).ends_with(" “Ausgeschlossen”"), "{}", rules_label(&album, &catalog));
    let rules = lightcraft_catalog::RuleSet {
        mode: lightcraft_catalog::Match::All,
        rules: vec![lightcraft_catalog::Rule::Field { field: "copyrightStatus".into(), op: "is".into(), value: serde_json::json!("publicDomain") }],
    };
    let public_domain = format!(" {}", choice_text_in(Locale::De, "copyrightStatus", "publicDomain"));
    assert!(rules_label(&rules, &catalog).ends_with(&public_domain), "{}", rules_label(&rules, &catalog));
    for value in [serde_json::json!(false), serde_json::json!("false")] {
        let rules = lightcraft_catalog::RuleSet {
            mode: lightcraft_catalog::Match::All,
            rules: vec![lightcraft_catalog::Rule::Field { field: "edited".into(), op: "is".into(), value }],
        };
        let no = format!(" {}", bool_text_in(Locale::De, false));
        assert!(rules_label(&rules, &catalog).ends_with(&no), "{}", rules_label(&rules, &catalog));
    }
    let both = lightcraft_catalog::Filter {
        label: Some(lightcraft_catalog::ColorLabel::Red),
        labels: vec![lightcraft_catalog::ColorLabel::Blue, lightcraft_catalog::ColorLabel::Red],
        ..Default::default()
    };
    let name = |label| color_label(&app.session.catalog, label);
    let (red, blue, green) =
        (name(lightcraft_catalog::ColorLabel::Red), name(lightcraft_catalog::ColorLabel::Blue), name(lightcraft_catalog::ColorLabel::Green));
    let (label, and, or) = (tr("Label"), tr("and"), tr("or"));
    assert_eq!(filter_label(&both, &app.session.catalog), format!("{label}: {red}"));
    let both = lightcraft_catalog::Filter { labels: vec![lightcraft_catalog::ColorLabel::Blue], ..both };
    assert_eq!(filter_label(&both, &app.session.catalog), format!("{label}: {red} {and} {blue}"));
    let both = lightcraft_catalog::Filter { labels: vec![lightcraft_catalog::ColorLabel::Blue, lightcraft_catalog::ColorLabel::Green], ..both };
    assert_eq!(filter_label(&both, &app.session.catalog), format!("{label}: {red} {and} ({blue} {or} {green})"));
    let filter =
        lightcraft_catalog::Filter { labels: vec![lightcraft_catalog::ColorLabel::Red, lightcraft_catalog::ColorLabel::Blue], ..Default::default() };
    let blue = color_label(&app.session.catalog, lightcraft_catalog::ColorLabel::Blue);
    let red = color_label(&app.session.catalog, lightcraft_catalog::ColorLabel::Red);
    let label = filter_label(&filter, &app.session.catalog);
    assert!(label.contains(&red) && label.contains(&blue), "{label}");
    // a label set's own name is the user's
    app.run("label.setNames", serde_json::json!({"names": {"red": "Color"}})).unwrap();
    let label = filter_label(&filter, &app.session.catalog);
    assert!(label.contains("Color") && !label.contains(&red) && label.contains(&blue), "{label}");
    let filter = lightcraft_catalog::Filter { flags: vec![lightcraft_catalog::Flag::Pick, lightcraft_catalog::Flag::None], ..Default::default() };
    let (flag, pick, reject, none) = (tr("Flag"), tr("pick"), tr("reject"), tr("none"));
    assert_eq!(filter_label(&filter, &app.session.catalog), format!("{flag}: {pick} {or} {none}"));
    let filter = lightcraft_catalog::Filter { flag: Some(lightcraft_catalog::Flag::Pick), ..filter };
    assert_eq!(filter_label(&filter, &app.session.catalog), format!("{flag}: {pick}"));
    let filter = lightcraft_catalog::Filter { flags: vec![lightcraft_catalog::Flag::Reject, lightcraft_catalog::Flag::None], ..filter };
    assert_eq!(filter_label(&filter, &app.session.catalog), format!("{flag}: {pick} {and} ({reject} {or} {none})"));
    set_language(Locale::En);
}

#[test]
fn german_panels_dialogs_and_glyphs_are_painted_without_extra_fonts() {
    let ctx = fonts_ctx(&[]);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.ui.left_panel = true;
    let text = painted_text(&ctx, &mut app, Locale::De);
    assert!(text.contains(Locale::De.tr("My Photos")) && text.contains(Locale::De.tr("All Photos")), "{text}");
    app.ui.view = crate::state::ViewMode::People;
    let text = painted_text(&ctx, &mut app, Locale::De);
    assert!(text.contains(Locale::De.tr("Named People")), "{text}");
    for (command, title) in [("app.about", "About LightCraft"), ("app.shortcuts", "Keyboard Shortcuts"), ("app.settings", "Settings")] {
        app.ui.dialog = None;
        app.run(command, serde_json::json!({})).unwrap();
        let text = painted_text(&ctx, &mut app, Locale::De);
        assert!(text.contains(Locale::De.tr(title)), "{command}: {text}");
    }
    ctx.fonts_mut(|fonts| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
            let font = egui::FontId::new(13.0, family);
            for ch in "ÄÖÜäöüß„“".chars() {
                assert!(fonts.has_glyph(&font, ch), "missing German glyph {ch}");
            }
        }
    });
    set_language(Locale::En);
}

#[test]
fn german_ukrainian_italian_and_finnish_buttons_fit_the_bottom_bar_and_export_dialog() {
    fn collect(shape: &egui::epaint::Shape, bounds: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::epaint::Shape::Text(shape) => {
                bounds.push((shape.galley.job.text.clone(), egui::Rect::from_min_size(shape.pos, shape.galley.size())))
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect(shape, bounds)),
            _ => {}
        }
    }
    for language in [Locale::De, Locale::Uk, Locale::It, Locale::Fi] {
        let ctx = fonts_ctx(&[]);
        let services = crate::Services { pick_folder: Some(Box::new(|| None)), ..Default::default() };
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        app.ui.language = language;
        let mut bounds = Vec::new();
        for export in [false, true] {
            if export {
                app.run("dialog.export", serde_json::json!({})).unwrap();
            }
            for frame in 0..4 {
                let input = crate::headless::HeadlessView::raw_input(egui::vec2(1600.0, 1000.0), 1.0, frame as f64 / 60.0, vec![]);
                let mut out = ctx.run_ui(input, |ui| {
                    app.logic(ui.ctx());
                    app.ui(ui);
                });
                out.textures_delta.clear();
                bounds.clear();
                for shape in out.shapes {
                    collect(&shape.shape, &mut bounds);
                }
            }
            let rect = |id: &str| app.widgets.iter().find(|entry| entry.0 == id).unwrap().1;
            if export {
                let dialog = rect("dialog:window");
                for id in ["button:exportNamingTags", "button:exportChooseFolder", "button:exportSavePreset"] {
                    assert!(dialog.contains_rect(rect(id)), "{id} spills outside {dialog:?}: {:?}", rect(id));
                }
            } else {
                let button = rect("button:copySettings");
                let (_, text) = bounds.iter().find(|entry| entry.0 == language.tr("Copy Edit Settings")).unwrap();
                assert!(button.contains_rect(*text), "{language:?} copy caption overflows its button: {text:?} vs {button:?}");
                assert!(!button.intersects(rect("icon:copyGear")));
            }
        }
    }
    set_language(Locale::En);
}

/// User-named menu items (presets, albums, label sets) are never translated; built-in labels are.
#[test]
fn menu_labels_translate_but_user_names_survive() {
    for language in Locale::ALL {
        set_language(*language);
        assert_eq!(crate::menubar::display_item_label("album.addPhotos", &serde_json::json!({"id": 1}), "Color"), "Color");
        assert_eq!(crate::menubar::display_item_label("app.export", &serde_json::json!({"preset": "Color"}), "Color"), "Color");
        assert_eq!(crate::menubar::display_item_label("view.photoGrid", &serde_json::Value::Null, "Color"), language.tr("Color"));
    }
    set_language(Locale::En);
}

/// The text the whole window paints over a few frames in `language`.
fn painted_text(ctx: &egui::Context, app: &mut crate::LightcraftApp, language: Locale) -> String {
    fn collect(shape: &egui::epaint::Shape, text: &mut String) {
        match shape {
            egui::epaint::Shape::Text(shape) => {
                text.push_str(&shape.galley.job.text);
                text.push('\n');
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect(shape, text)),
            _ => {}
        }
    }
    app.ui.language = language;
    let mut text = String::new();
    for frame in 0..4 {
        let input = crate::headless::HeadlessView::raw_input(egui::vec2(1600.0, 1000.0), 1.0, frame as f64 / 60.0, vec![]);
        let mut out = ctx.run_ui(input, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        // This inspects shapes without a renderer; discard texture uploads explicitly.
        out.textures_delta.clear();
        text.clear();
        for shape in out.shapes {
            collect(&shape.shape, &mut text);
        }
    }
    text
}

/// Every language is painted by the real window, switching at runtime reinstalls the fonts
/// (their CJK fallback order follows the language), and command ids never change with it.
#[test]
fn every_language_is_painted_and_menu_ids_stay_the_same() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.ui.left_panel = true;
    let ids = |app: &crate::LightcraftApp| crate::menus::menu_entries(app).into_iter().map(|entry| entry.id).collect::<Vec<_>>();
    let english_ids = ids(&app);
    for language in Locale::ALL {
        let text = painted_text(&ctx, &mut app, *language);
        for label in ["My Photos", "All Photos"] {
            assert!(text.contains(language.tr(label)), "{language:?}: {label} -> {:?} not in\n{text}", language.tr(label));
        }
        assert_eq!(app.font_language, *language, "the fonts follow the language");
        assert_eq!(ids(&app), english_ids, "{language:?}: command ids are presentation-independent");
    }
    set_language(Locale::En);
}

/// The UI families, after one frame so the font definitions are loaded.
fn fonts_ctx(craft: &'static [lightcraft_engine::CraftFont]) -> egui::Context {
    let ctx = egui::Context::default();
    ctx.set_fonts(crate::theme::font_definitions(craft));
    let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
    out.textures_delta.clear();
    ctx
}

/// Built with craft-fonts, every CJK language renders with real glyphs (no tofu) in every UI
/// family, Monospace included.
#[test]
fn craft_fonts_render_cjk_in_the_ui() {
    let samples = [(Locale::Ja, "日本語の文字"), (Locale::ZhHans, "简体中文字"), (Locale::ZhHant, "繁體中文字")];
    for (language, sample) in samples {
        if !lightcraft_engine::CRAFT_FONTS.iter().any(|font| font.serves(language.script())) {
            lightcraft_testkit::skip!(Fonts, "{}: built without a craft-fonts face for {}", language.code(), language.script());
            continue;
        }
        set_language(language);
        let ctx = fonts_ctx(lightcraft_engine::CRAFT_FONTS);
        ctx.fonts_mut(|fonts| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into()), egui::FontFamily::Monospace] {
                let font = egui::FontId::new(13.0, family);
                for ch in sample.chars() {
                    assert!(fonts.has_glyph(&font, ch), "{language:?}: {ch} in {font:?}");
                }
                let galley = fonts.layout_no_wrap(sample.into(), font.clone(), egui::Color32::WHITE);
                let wide = 13.0 * (sample.chars().count() as f32 - 1.0);
                assert!(galley.size().x > wide, "{language:?} {font:?}: full-width glyphs, {:?}", galley.size());
            }
        });
    }
    set_language(Locale::En);
}

/// Built without craft-fonts, the UI (in every language) still installs its fonts and runs;
/// Latin text keeps Inter.
#[test]
fn the_ui_works_without_craft_fonts() {
    let ctx = fonts_ctx(&[]);
    ctx.fonts_mut(|fonts| {
        // (Not Monospace: egui's `has_glyph` reports false for glyphs of the family's
        // replacement-glyph face, which there is Hack, the face that draws Latin.)
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
            let font = egui::FontId::new(13.0, family);
            assert!("LightCraft".chars().all(|ch| fonts.has_glyph(&font, ch)), "{font:?}");
        }
    });
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    for language in Locale::ALL {
        app.ui.language = *language;
        for frame in 0..3 {
            let input = crate::headless::HeadlessView::raw_input(egui::vec2(1200.0, 800.0), 1.0, frame as f64 / 60.0, vec![]);
            let mut out = ctx.run_ui(input, |ui| {
                app.logic(ui.ctx());
                app.ui(ui);
            });
            out.textures_delta.clear();
        }
    }
    set_language(Locale::En);
}

/// Traditional Chinese through the menu command and the control channel, persisted, with
/// localised headings; ids and user text are untouched.
#[test]
fn traditional_chinese_switches_through_menu_and_control_and_persists() {
    use crate::control::{ControlRequest, Outcome};
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.run("app.language.traditionalChinese", serde_json::json!({})).unwrap();
    assert_eq!(app.ui.language, Locale::ZhHant);
    // Applied at once, not on the next frame.
    assert_eq!(language(), Locale::ZhHant);
    assert_eq!(source_label(lightcraft_engine::LibrarySource::All, &app.session.catalog), Locale::ZhHant.tr("All Photos"));
    assert_eq!(date_group_label("", false), Locale::ZhHant.tr("Unknown Date"));
    assert_eq!(display_time("a-user-value"), "a-user-value");
    assert_eq!(crate::menubar::checked(&app, "app.language.traditionalChinese"), Some(true));
    assert_eq!(crate::menubar::checked(&app, "app.language.english"), Some(false));
    assert_eq!(tr("my-photo.jpg"), "my-photo.jpg");
    for id in ["album.addPhotos", "metadata.applyPreset", "label.applySet"] {
        assert_eq!(crate::menubar::display_item_label(id, &serde_json::json!({"id": 1}), "Color"), "Color");
    }
    assert_eq!(crate::menubar::display_item_label("view.photoGrid", &serde_json::Value::Null, "Color"), Locale::ZhHant.tr("Color"));
    let saved = serde_json::to_string(&app.ui).unwrap();
    assert!(saved.contains(r#""language":"zh-hant""#), "{saved}");
    assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, Locale::ZhHant);
    let ctx = egui::Context::default();
    app.ui.language = Locale::En;
    for code in ["zh-hant", "zh-Hant", "zh-TW", "zh_TW", "zh-HK"] {
        let (req, _) = ControlRequest::new("ui.set", serde_json::json!({"language": code}));
        let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &req) else { panic!("expected a reply") };
        assert_eq!(reply["ok"], true, "{code}");
        assert_eq!(app.ui.language, Locale::ZhHant, "{code}");
        assert_eq!(language(), Locale::ZhHant, "{code} applies at once");
    }
    let (req, _) = ControlRequest::new("ui.set", serde_json::json!({"language": "xx"}));
    let Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &req) else { panic!("expected a reply") };
    assert_eq!(reply["ok"], false);
    assert_eq!(app.ui.language, Locale::ZhHant);
    let ids = |app: &crate::LightcraftApp| crate::menus::menu_entries(app).into_iter().map(|entry| entry.id).collect::<Vec<_>>();
    let chinese_ids = ids(&app);
    app.run("app.language.english", serde_json::json!({})).unwrap();
    assert_eq!(tr("File"), "File");
    assert_eq!(chinese_ids, ids(&app));
}

/// Dates follow every language's own patterns (English keeps the catalog's wording); a key that
/// is not a date never panics and never shows a raw pattern.
#[test]
fn date_headings_in_every_language() {
    set_language(Locale::En);
    assert_eq!(date_group_label("2026-09-20", false), "Sunday, 20 September 2026");
    assert_eq!(date_group_label("2026-09-20", true), "Sunday, 20");
    assert_eq!(date_group_label("2026-09", true), "September");
    assert_eq!(display_time("2026-09-20T16:04:05"), lightcraft_catalog::dates::display_time("2026-09-20T16:04:05"));
    for language in Locale::ALL {
        set_language(*language);
        if *language != Locale::En {
            let day = date_group_label("2026-09-20", false);
            assert!(day.contains("20") && day.contains("2026") && day.contains(tr("Sunday")), "{language:?}: {day}");
            assert!(date_group_label("2026-09-20", true).contains("20"), "{language:?}");
            assert!(date_group_label("2026-09", false).contains("2026"), "{language:?}");
            assert!(date_group_label("2026", false).contains("2026"), "{language:?}");
            assert_eq!(display_time("2026-09-20T16:04:05"), format!("{day} 16:04:05"), "{language:?}");
        }
        for key in ["", "abcd", "2026-xx", "2026-13", "2026-09-xx", "２０２６-09"] {
            assert!(!date_group_label(key, false).contains('{'), "{language:?}: {key:?}");
            assert!(!date_group_label(key, true).contains('{'), "{language:?}: {key:?}");
        }
    }
    set_language(Locale::En);
}

/// Stock presets and panel headers are translated; a user preset keeps its name, also when it
/// shares a stock preset's name.
#[test]
fn traditional_chinese_presets_and_panel_headers_are_painted() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.ui.presets = true;
    let builtin = app.session.presets.iter().find(|p| p.name == "Warm Glow").unwrap().clone();
    app.session.presets.push(lightcraft_develop::Preset { id: "user.test".into(), builtin: false, ..builtin });
    let zh = Locale::ZhHant;
    for (panel, title) in [(crate::state::RightPanel::Activity, "History"), (crate::state::RightPanel::Versions, "Versions")] {
        app.ui.right = panel;
        let text = painted_text(&ctx, &mut app, zh);
        assert!(text.contains(zh.tr(title)), "{text}");
        assert!(text.contains(zh.tr("Warm Glow")), "stock preset: {text}");
        assert!(text.contains("Warm Glow"), "user preset: {text}");
        assert!(text.contains(zh.tr("Color")), "stock group: {text}");
    }
    assert_eq!(history_label("Exposure", &app.session.presets), zh.tr("Exposure"));
    assert!(history_label("Preset: Warm Film", &app.session.presets).ends_with(zh.tr("Warm Film")));
    assert!(history_label("Preset: Warm Glow", &app.session.presets).ends_with("Warm Glow"), "the user's preset shares the name");
    set_language(Locale::En);
}

#[test]
fn vietnamese_panels_dialogs_and_diacritics_paint_with_bundled_fonts() {
    let ctx = fonts_ctx(&[]);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.ui.left_panel = true;
    let text = painted_text(&ctx, &mut app, Locale::Vi);
    assert!(text.contains(Locale::Vi.tr("My Photos")) && text.contains(Locale::Vi.tr("All Photos")), "{text}");
    for (command, title) in [("app.about", "About LightCraft"), ("app.shortcuts", "Keyboard Shortcuts"), ("app.settings", "Settings")] {
        app.ui.dialog = None;
        app.run(command, serde_json::json!({})).unwrap();
        let text = painted_text(&ctx, &mut app, Locale::Vi);
        assert!(text.contains(Locale::Vi.tr(title)), "{command}: {text}");
    }
    ctx.fonts_mut(|fonts| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
            let font = egui::FontId::new(13.0, family);
            for ch in "ĂăÂâÊêÔôƠơƯưĐđáàảãạéèẻẽẹíìỉĩịóòỏõọúùủũụýỳỷỹỵ".chars().chain(
                Locale::Vi
                    .catalog()
                    .values()
                    .flat_map(|text| text.chars())
                    .filter(|ch| ('\u{00c0}'..='\u{024f}').contains(ch) || ('\u{1ea0}'..='\u{1ef9}').contains(ch)),
            ) {
                assert!(fonts.has_glyph(&font, ch), "missing Vietnamese glyph {ch}");
            }
        }
    });
    set_language(Locale::En);
}
/// The messages each language lacks, which it shows in English. Never fails: `cargo xtask ci`
/// runs it with `--nocapture`, so the list lands in the gate log. Plain messages are every key
/// some catalog has, every `tr("…")` literal in the app's source (`gaps.rs`, wrapped calls and
/// `if`/`match` branches included, so a string no language has yet is listed too) and the
/// labels the UI passes to `tr` from tables (commands, controls, rule fields…); formats are
/// every key some `*-formats.json` has plus every `tr_format!` message in the app's source. An
/// empty translation counts as missing.
#[test]
fn translation_gaps_report() {
    let notes = whats_new_lines();
    let messages: std::collections::BTreeSet<String> = plain_messages().into_iter().filter(|m| !notes.contains(&m.as_str())).collect();
    println!("{}", gaps_text(&messages, &format_messages(), &notes));
}

/// A string only the source parsing finds (a call rustfmt wrapped, the branches of an `if` or a
/// `match`) is in no catalog, and the report still lists it for every language.
#[test]
fn translation_gaps_report_lists_strings_no_catalog_has() {
    let found = gaps::messages_in(
        "fn f(m: u8) {\n    ui.label(crate::i18n::tr(\n        \"Only wrapped here\",\n    ));\n    \
             ui.label(tr(if m == 0 { \"Only in an if\" } else { \"Only in an else\" }));\n    \
             ui.label(tr(match m { 1 => \"Only in a match\", _ => \"Only in a last arm\" }));\n}\n",
    );
    assert_eq!(found.len(), 5, "{found:?}");
    let text = gaps_text(&found, &Default::default(), &[]);
    for language in Locale::ALL.iter().filter(|language| **language != Locale::En) {
        let line = text.lines().find(|line| line.starts_with(&format!("{}: ", language.code()))).expect("a line per language");
        assert!(line.contains("5 messages"), "{line}");
    }
    for key in &found {
        assert!(text.contains(&format!("{key:?}")), "{key}");
    }
}

/// The report: the totals, then per language its counts and the lists.
fn gaps_text(messages: &std::collections::BTreeSet<String>, formats: &std::collections::BTreeSet<String>, notes: &[&str]) -> String {
    let quoted = |list: &[&str]| list.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(", ");
    let mut out =
        format!("Translation gaps, shown in English ({} messages, {} formats, {} What's New lines):\n", messages.len(), formats.len(), notes.len());
    for language in Locale::ALL.iter().filter(|language| **language != Locale::En) {
        let catalog = language.catalog();
        let own_formats = formats_catalog(language.code());
        let missing: Vec<&str> = messages.iter().map(String::as_str).filter(|m| !catalog.contains_key(*m)).collect();
        let missing_formats: Vec<&str> = formats.iter().map(String::as_str).filter(|m| !own_formats.contains_key(*m)).collect();
        let missing_notes = notes.iter().filter(|line| !catalog.contains_key(**line)).count();
        out.push_str(&format!(
            "{}: {} messages, {} formats, {} What's New lines\n",
            language.code(),
            missing.len(),
            missing_formats.len(),
            missing_notes
        ));
        if !missing.is_empty() {
            out.push_str(&format!("  messages: {}\n", quoted(&missing)));
        }
        if !missing_formats.is_empty() {
            out.push_str(&format!("  formats: {}\n", quoted(&missing_formats)));
        }
    }
    out
}

/// Every plain message: the keys of every catalog (but contextual entries, which a language adds
/// only where it needs one), the `tr("…")` literals in the source and the labels the UI
/// translates from tables.
fn plain_messages() -> std::collections::BTreeSet<String> {
    use lightcraft_catalog::rules;
    let mut labels: Vec<&str> = lightcraft_engine::command_specs().iter().map(|spec| spec.label).collect();
    labels.extend(crate::menus::ui_commands().map(|command| command.1).filter(|label| !Locale::ALL.iter().any(|locale| locale.name() == *label)));
    labels.extend(lightcraft_develop::CONTROLS.iter().map(|control| control.label));
    labels.extend(crate::panels::settings::TABS.iter().map(|(_, label)| *label));
    for (_, label, kind) in rules::FIELDS {
        labels.push(label);
        labels.extend(rules::ops_for(*kind).iter().map(|(_, label)| *label));
        if let rules::Kind::Choice(choices) = kind {
            labels.extend(choices.iter().map(|(_, label)| *label));
        }
    }
    labels.extend(rules::FIELD_GROUPS.iter().map(|group| group.0));
    labels.extend(rules::Issue::ALL.iter().map(|issue| issue.text()));
    labels.extend([rules::bool_label(true), rules::bool_label(false)]);
    labels.extend(lightcraft_engine::rename::TOKENS.iter().map(|token| token.meaning));
    labels.extend(lightcraft_engine::rename::TEMPLATE_NOTES);
    labels.extend(crate::date_picker::WEEKDAY_SHORT);
    labels.extend(["Pick a date", "Year", "Month", "Day", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]);
    // the album pickers
    labels.extend([
        "Choose an album…",
        "No albums yet",
        "Search albums",
        "No albums match",
        "This is the album you're editing",
        "It tests this album, so testing it back would loop",
        "Fix the marked rules to save this album.",
        "None",
        "New album",
        "New album…",
        "Imported Photos",
    ]);
    labels.extend(crate::panels::keyword_list::VARIABLE_LABELS);
    let contextual = format!("{BOOL_CONTEXT}|");
    Locale::ALL
        .iter()
        .flat_map(|language| language.catalog().keys())
        .filter(|key| !key.starts_with(&contextual))
        .cloned()
        .chain(gaps::source_messages())
        .chain(labels.into_iter().map(str::to_string))
        .collect()
}

/// Help ▸ What's New, line by line as the dialog looks each one up.
fn whats_new_lines() -> Vec<&'static str> {
    crate::panels::dialogs::WHATS_NEW
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("# "))
        .map(|line| line.strip_prefix("### ").or_else(|| line.strip_prefix("## ")).or_else(|| line.strip_prefix("- ")).unwrap_or(line))
        .collect()
}

/// `locales/<code>-formats.json` (the catalog `build.rs` compiles into `tr_format!`), without
/// empty translations.
fn formats_catalog(code: &str) -> BTreeMap<String, String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("locales/{code}-formats.json"));
    let mut messages: BTreeMap<String, String> =
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    messages.retain(|_, text| !text.is_empty());
    messages
}

/// Every format message: the keys of every formats catalog and the message of every
/// `tr_format!` call in `src/` outside its test modules.
fn format_messages() -> std::collections::BTreeSet<String> {
    let mut all: std::collections::BTreeSet<String> =
        Locale::ALL.iter().filter(|language| **language != Locale::En).flat_map(|language| formats_catalog(language.code()).into_keys()).collect();
    let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = dirs.pop() {
        for path in std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path()) {
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                let app = test_module_start(&source).map_or(source.as_str(), |at| &source[..at]);
                all.extend(tr_format_messages(app));
            }
        }
    }
    all
}

/// Where the first inline `#[cfg(test)] mod x { … }` starts. A `#[cfg(test)] mod x;` line only
/// declares a file, and app code can follow it.
fn test_module_start(source: &str) -> Option<usize> {
    let head = "#[cfg(test)]\nmod ";
    source
        .match_indices(head)
        .map(|(at, _)| at)
        .find(|at| source[at + head.len()..].trim_start_matches(|c: char| c.is_alphanumeric() || c == '_').starts_with(" {"))
}

/// The first argument of each `tr_format!` call in `source`, when it is a string literal.
fn tr_format_messages(source: &str) -> Vec<String> {
    // built from parts, so this file's own source holds no call to find
    let call = concat!("tr_format", "!(");
    let mut messages = Vec::new();
    for (at, _) in source.match_indices(call) {
        let Some(body) = source[at + call.len()..].trim_start().strip_prefix('"') else { continue };
        let mut message = String::new();
        let mut chars = body.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => message.push('\n'),
                    Some('t') => message.push('\t'),
                    Some('u') => {
                        let hex: String = chars.by_ref().skip(1).take_while(|ch| *ch != '}').collect();
                        message.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                    }
                    Some(other) => message.push(other),
                    None => break,
                },
                _ => message.push(ch),
            }
        }
        messages.push(message);
    }
    messages
}

#[test]
fn tr_format_messages_are_read_from_source() {
    let call = concat!("tr_format", "!(");
    let source = format!("{call}\"Imported {{}} photo{{}}\", n, s); crate::i18n::{call}\n    \"Say \\\"hi\\\" \\u{{2192}}\", x); {call}name)");
    assert_eq!(tr_format_messages(&source), ["Imported {} photo{}", "Say \"hi\" →"]);
}

/// A drop's notice comes from what `library.import` reported: a photo added, one brought back from Recently
/// Deleted and a file that could not be read are each named; a drop of duplicates only says nothing.
#[test]
fn a_dropped_photos_notice_names_every_outcome_of_the_import_and_is_silent_for_duplicates_only() {
    set_language(Locale::En);
    let dir = std::env::temp_dir().join(format!("lc-drop-notice-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let png = |name: &str, seed: u8| {
        let data: Vec<[u8; 4]> = (0..48 * 32).map(|i| [(i % 48 * 5) as u8, (i / 48 * 7) as u8, seed, 255]).collect();
        let img = lightcraft_raster::Rgba8 { width: 48, height: 32, data };
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
        dir.join(name).to_string_lossy().into_owned()
    };
    let (trashed, added) = (png("trashed.png", 1), png("new.png", 2));
    let broken = dir.join("broken.jpg");
    std::fs::write(&broken, b"not a jpeg at all").unwrap();
    let mut s = lightcraft_engine::Session::new().with_fs();
    let r = s.execute("library.import", &serde_json::json!({"paths": [trashed]})).unwrap();
    s.execute("photo.delete", &serde_json::json!({"ids": [r["imported"][0]]})).unwrap();
    let report = s.execute("library.import", &serde_json::json!({"paths": [trashed, added, broken]})).unwrap();
    assert_eq!(
        dropped_photos_outcome(&report).as_deref(),
        Some("Added 1 photo · Restored 1 photo from Recently Deleted · 1 photo not readable"),
        "{report}"
    );
    let again = s.execute("library.import", &serde_json::json!({"paths": [added]})).unwrap();
    assert_eq!(dropped_photos_outcome(&again), None, "a duplicate only: {again}");
    let _ = std::fs::remove_dir_all(dir);
}

fn assert_polish_counts() {
    for n in [0, 1, 2, 3, 4, 5, 12, 21, 22, 25, 111] {
        let s = if n == 1 { "" } else { "s" };
        assert_eq!(tr_format!("Imported {} photo{}", n, s), format!("Zaimportowane zdjęcia: {n}"));
        for (one, many, noun) in [
            ("photo waiting", "photos waiting", "Oczekujące zdjęcia"),
            ("cached picture", "cached pictures", "Obrazy w pamięci podręcznej"),
            ("photo has its denoised picture.", "photos have their denoised picture.", "Zdjęcia z odszumioną wersją"),
        ] {
            assert_eq!(tr_format!("{n} {noun}", n = n, noun = tr(if n == 1 { one } else { many })), format!("{noun}: {n}"));
        }
        assert_eq!(tr_format!("Delete {n} Photos", n = n), format!("Usuń zdjęcia ({n})"));
        assert_eq!(tr_format!("{n} preset{}", s, n = n), format!("Ustawienia: {n}"));
    }
}

fn assert_polish_selected_by_tags(app: &mut crate::LightcraftApp, ctx: &egui::Context) {
    use crate::control::{ControlRequest, Outcome};
    for tag in ["pl", "pl-PL", "pl_PL.UTF-8", "PL-pl"] {
        assert_eq!(Locale::parse_tag(tag), Some(Locale::Pl), "{tag}");
        let (request, _) = ControlRequest::new("ui.set", serde_json::json!({"language": tag}));
        let Outcome::Done(reply) = crate::control::handle(app, ctx, &request) else { panic!("expected reply") };
        assert_eq!(reply["ok"], true);
        assert_eq!(language(), Locale::Pl);
    }
}

fn assert_polish_glyphs(ctx: &egui::Context) {
    ctx.fonts_mut(|fonts| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Name(crate::theme::FONT_SEMIBOLD.into())] {
            let font = egui::FontId::new(13.0, family);
            for ch in "ąćęłńóśźżĄĆĘŁŃÓŚŹŻ„”".chars() {
                assert!(fonts.has_glyph(&font, ch), "missing Polish glyph {ch}");
            }
        }
    });
}

/// Polish: the menu and control channel select it, dates read day.month.year, and every count
/// reads right for any number. Polish has three plural forms (1 zdjęcie, 2 zdjęcia, 5 zdjęć), so
/// counts follow their noun ("Zaimportowane zdjęcia: 22") instead of a form that fits only some.
#[test]
fn polish_switches_formats_dates_and_reads_every_count() {
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    app.run("app.language.polish", serde_json::json!({})).unwrap();
    assert_eq!(app.ui.language, Locale::Pl);
    assert_eq!(Locale::Pl.name(), "Polski");
    assert_eq!(crate::menubar::checked(&app, "app.language.polish"), Some(true));
    assert_eq!(tr("File"), "Plik");
    assert_eq!(tr("Exposure"), "Ekspozycja");
    assert_eq!(date_group_label("2026-09-20", false), "Niedziela, 20.09.2026");
    assert_eq!(date_group_label("2026-09-20", true), "Niedziela, 20");
    assert_eq!(date_group_label("2026-09", false), "09.2026");
    assert_eq!(date_group_label("2026", false), "2026");
    assert_eq!(display_time("2026-09-20T16:04:05"), "Niedziela, 20.09.2026 16:04:05");
    assert_polish_counts();
    let name = String::from("Color 1234.56 {title} 日本語");
    assert_eq!(tr_format!("Added {n} photo{} to “{}”", "s", name, n = 22), format!("Dodano do „{name}”: 22"));
    let saved = serde_json::to_string(&app.ui).unwrap();
    assert_eq!(serde_json::from_str::<crate::state::UiState>(&saved).unwrap().language, Locale::Pl);
    let ctx = egui::Context::default();
    assert_polish_selected_by_tags(&mut app, &ctx);
    let text = painted_text(&ctx, &mut app, Locale::Pl);
    assert!(text.contains("Plik") && text.contains("Ekspozycja"), "{text}");
    assert_polish_glyphs(&ctx);
    set_language(Locale::En);
}
