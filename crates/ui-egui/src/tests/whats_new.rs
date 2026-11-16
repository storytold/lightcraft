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
