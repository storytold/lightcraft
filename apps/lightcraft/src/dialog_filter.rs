//! File-dialog filters that match extensions whatever their letter case.
//!
//! On Linux, `rfd` turns each extension into a `*.ext` glob for the GTK dialog and for the
//! desktop portal (`xdg-desktop-portal` FileChooser), and both match globs case-sensitively, so
//! a filter of `nef` hid `DSC_1234.NEF` (issue #342). Windows and macOS ignore case. Every
//! dialog filter of the app goes through [`FileDialogExt::add_filter_nocase`] (a test checks
//! that nothing else calls `add_filter`), so no extension can hit this.

use rfd::FileDialog;

/// The patterns to hand to `rfd` for `exts`, which are plain extensions without the dot.
///
/// Windows and macOS take the extension literally and ignore case: lower case, de-duplicated.
/// Linux (`linux`): `rfd` formats each entry as `*.{entry}`, which GTK 3 and the portal match
/// as a glob, so an entry may carry glob syntax. Each extension gives the lower-case form, the
/// UPPER-case form (kept in case a portal backend does not do `[...]` classes) and a bracket
/// form such as `[nN][eE][fF]` that matches every mix (`.Nef`, `.nEf`, ...).
pub fn dialog_extensions_for(exts: &[&str], linux: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    for ext in exts {
        let ext = ext.trim_start_matches('.');
        let lower = ext.to_lowercase();
        push(lower.clone());
        if !linux {
            continue;
        }
        push(ext.to_uppercase());
        // `[` `]` `*` `?` in an extension would already be glob syntax: leave those alone
        if lower.chars().any(|c| c.is_alphabetic()) && !lower.contains(['[', ']', '*', '?', '\\']) {
            push(
                lower
                    .chars()
                    .map(|c| {
                        let up: String = c.to_uppercase().collect();
                        if up.chars().count() == 1 && up != c.to_string() { format!("[{c}{up}]") } else { c.to_string() }
                    })
                    .collect(),
            );
        }
    }
    out
}

/// [`dialog_extensions_for`] for the platform this is built for.
pub fn dialog_extensions(exts: &[&str]) -> Vec<String> {
    dialog_extensions_for(exts, cfg!(all(unix, not(target_os = "macos"))))
}

/// `add_filter` that matches the extensions in any letter case.
pub trait FileDialogExt {
    fn add_filter_nocase(self, name: impl Into<String>, exts: &[&str]) -> Self;
}

impl FileDialogExt for FileDialog {
    fn add_filter_nocase(self, name: impl Into<String>, exts: &[&str]) -> Self {
        self.add_filter(name, &dialog_extensions(exts))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn other_platforms_get_lower_case_once() {
        assert_eq!(dialog_extensions_for(&["NEF", "nef", ".Jpg", "jpg", ""], false), ["nef", "jpg"]);
    }

    #[test]
    fn linux_gets_lower_upper_and_any_mix() {
        assert_eq!(dialog_extensions_for(&["nef"], true), ["nef", "NEF", "[nN][eE][fF]"]);
        // digits have no case; a duplicate adds nothing
        assert_eq!(dialog_extensions_for(&["mp4", "MP4"], true), ["mp4", "MP4", "[mM][pP]4"]);
        assert_eq!(dialog_extensions_for(&["7z"], true), ["7z", "7Z", "7[zZ]"]);
        assert_eq!(dialog_extensions_for(&["123"], true), ["123"]);
    }

    #[test]
    fn linux_glob_syntax_is_not_wrapped_again() {
        assert_eq!(dialog_extensions_for(&["[nN]ef"], true), ["[nn]ef", "[NN]EF"]);
    }

    /// The Linux patterns match like GTK's glob (`*.{entry}`): checked against a tiny matcher of
    /// the subset used here (verified against real GTK 3.24 `GtkFileFilter` in the PR notes).
    fn glob_ext(entry: &str, ext: &str) -> bool {
        let mut pat = entry.chars().peekable();
        let mut s = ext.chars();
        while let Some(p) = pat.next() {
            let Some(c) = s.next() else { return false };
            if p == '[' {
                let mut hit = false;
                for q in pat.by_ref() {
                    if q == ']' {
                        break;
                    }
                    hit |= q == c;
                }
                if !hit {
                    return false;
                }
            } else if p != c {
                return false;
            }
        }
        s.next().is_none()
    }

    #[test]
    fn linux_patterns_match_every_case_mix() {
        let pats = dialog_extensions_for(&["nef", "cr2", "xmp"], true);
        let matches = |name: &str| pats.iter().any(|p| glob_ext(p, name));
        for ok in ["nef", "NEF", "Nef", "nEF", "CR2", "Cr2", "XMP", "xMp"] {
            assert!(matches(ok), "{ok}");
        }
        for bad in ["nex", "ne", "nefs", "jpg", "cr3"] {
            assert!(!matches(bad), "{bad}");
        }
    }

    /// The app's own extension list (the one the folder scan uses) is covered whole.
    #[test]
    fn import_extensions_are_all_covered_in_both_cases() {
        let pats = dialog_extensions_for(lightcraft_engine::import::EXTENSIONS, true);
        for e in lightcraft_engine::import::EXTENSIONS {
            assert!(pats.iter().any(|p| glob_ext(p, &e.to_uppercase())), "{e}");
            assert!(pats.iter().any(|p| p == e), "{e}");
        }
    }

    /// Every dialog filter goes through the helper: `main.rs` must not call rfd's own
    /// `add_filter`.
    #[test]
    fn no_dialog_bypasses_the_helper() {
        for (file, src) in [("main.rs", include_str!("main.rs")), ("control_server.rs", include_str!("control_server.rs"))] {
            assert!(!src.contains(".add_filter("), "{file} calls rfd's add_filter; use add_filter_nocase");
        }
        assert!(include_str!("main.rs").contains(".add_filter_nocase("));
    }
}
