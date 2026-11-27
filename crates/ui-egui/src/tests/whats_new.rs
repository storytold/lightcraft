//! Issue #4383: What's New showed each hard-wrapped line of `docs/whats-new.md` as its own paragraph, and its
//! Markdown links, bold and italics as raw text.

use crate::dialogs::{WHATS_NEW, WhatsNewBlock, whats_new_blocks};

#[test]
fn a_wrapped_entry_is_one_bullet_and_markdown_shows_as_text() {
    let src = "# What's New\n\n## Unreleased\n\n### Grain\n- New grain, first line\n  continued here.\n  See [film grain](grain.md).\n- **At launch**: *Ask me* or `--open`.\n\nA note\nover two lines.\n";
    assert_eq!(
        whats_new_blocks(src),
        vec![
            WhatsNewBlock::Section("Unreleased".into()),
            WhatsNewBlock::Heading("Grain".into()),
            WhatsNewBlock::Bullet("New grain, first line continued here. See film grain.".into()),
            WhatsNewBlock::Bullet("At launch: Ask me or --open.".into()),
            WhatsNewBlock::Paragraph("A note over two lines.".into()),
        ]
    );
}

#[test]
fn the_shipped_notes_have_no_split_sentences_or_raw_markdown() {
    let blocks = whats_new_blocks(WHATS_NEW);
    let continuation_lines = WHATS_NEW.lines().filter(|l| l.starts_with("  ")).count();
    let bullets = blocks.iter().filter(|b| matches!(b, WhatsNewBlock::Bullet(_))).count();
    assert!(continuation_lines > 0 && bullets > 0);
    for b in &blocks {
        let (WhatsNewBlock::Section(s) | WhatsNewBlock::Heading(s) | WhatsNewBlock::Bullet(s) | WhatsNewBlock::Paragraph(s)) = b;
        assert!(!s.contains("](") && !s.contains("**") && !s.contains('`'), "raw Markdown in {s:?}");
        assert!(s.chars().any(char::is_alphanumeric), "a block with no words: {s:?}");
    }
}

/// A code span keeps what it says (a file pattern's `*`), and a link's label is made plain too.
#[test]
fn code_spans_stay_literal_and_link_labels_lose_their_markers() {
    let blocks = whats_new_blocks("- Match `*.xmp` and `[a](b)`; see [**film grain**](grain.md) and 3 * 2.\n");
    assert_eq!(blocks, vec![WhatsNewBlock::Bullet("Match *.xmp and [a](b); see film grain and 3 * 2.".into())]);
}

/// Review of #4385: a note the catalog translates line by line is shown translated, as one paragraph. The German
/// catalog holds both lines of the Settings note on their own; the joined text is in no catalog.
#[test]
fn a_wrapped_note_keeps_its_line_by_line_translation() {
    use crate::dialogs::whats_new_blocks_in;
    let start = WHATS_NEW.lines().position(|l| l.starts_with("- The Settings window keeps its top edge")).expect("the Settings note");
    let note: Vec<&str> = WHATS_NEW.lines().skip(start).take_while(|l| !l.trim().is_empty()).collect();
    assert!(note.len() > 1, "a note wrapped over several lines: {note:?}");
    crate::i18n::set_language(crate::i18n::Locale::De);
    let blocks = whats_new_blocks_in(&note.join("\n"), &|l| crate::i18n::tr(l).to_string());
    let lines: Vec<String> = note.iter().map(|l| crate::i18n::tr(l.trim().trim_start_matches("- ")).to_string()).collect();
    crate::i18n::set_language(crate::i18n::Locale::En);
    let [WhatsNewBlock::Bullet(text)] = blocks.as_slice() else { panic!("one paragraph: {blocks:?}") };
    assert!(lines.iter().all(|l| !note.iter().any(|n| n.contains(l.as_str()))), "every line has a German translation: {lines:?}");
    assert_eq!(text, &lines.join(" "), "the German lines, joined");
}
