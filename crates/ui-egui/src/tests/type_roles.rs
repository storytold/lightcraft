//! The type roles (`theme::TextSize` and `Tokens::caption()`, `Tokens::body_strong()` and the
//! like): seven sizes, each with a job, in a regular and a semibold face. This is the model; the
//! panels move over to it as their text is touched.

use egui::FontFamily;

use crate::theme::{FONT_SEMIBOLD, TextSize, ThemeKind, Tokens};

fn tokens() -> Tokens {
    Tokens::for_kind(ThemeKind::Charcoal)
}

#[test]
fn the_sizes_are_those_of_the_scale_smallest_first() {
    // Given the seven roles
    let points: Vec<f32> = TextSize::ALL.iter().map(|size| size.points()).collect();
    // Then their sizes are 10, 11, 12, 13, 14, 15 and 18 pt, each larger than the one before
    assert_eq!(points, [10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 18.0]);
}

#[test]
fn the_sizes_of_the_scale_are_a_point_or_more_apart() {
    // Two sizes closer than a point cannot be told apart, so they would not carry different jobs
    for pair in TextSize::ALL.windows(2) {
        assert!(pair[1].points() - pair[0].points() >= 1.0, "{pair:?}");
    }
}

#[test]
fn each_role_gives_its_size_in_both_faces() {
    let t = tokens();
    // Given each role's regular and semibold font
    let roles = [
        (TextSize::Micro, t.micro(), t.micro_strong()),
        (TextSize::Caption, t.caption(), t.caption_strong()),
        (TextSize::Label, t.label(), t.label_strong()),
        (TextSize::Body, t.body(), t.body_strong()),
        (TextSize::Title, t.title(), t.title_strong()),
        (TextSize::PanelTitle, t.panel_title(), t.panel_title_strong()),
        (TextSize::Display, t.display(), t.display_strong()),
    ];
    assert_eq!(roles.len(), TextSize::ALL.len());
    for (size, regular, strong) in roles {
        // Then both are the role's size, the regular in the proportional family and the strong in the semibold
        assert_eq!((regular.size, strong.size), (size.points(), size.points()), "{size:?}");
        assert_eq!(regular.family, FontFamily::Proportional, "{size:?}");
        assert_eq!(strong.family, FontFamily::Name(FONT_SEMIBOLD.into()), "{size:?}");
        // And asking the theme by role is the same font
        assert_eq!((t.regular(size), t.strong(size)), (regular, strong), "{size:?}");
    }
}

#[test]
fn the_text_styles_a_control_takes_are_roles() {
    // Given the theme applied to a context
    let t = tokens();
    let ctx = egui::Context::default();
    crate::theme::apply_kind(&ctx, ThemeKind::Charcoal);
    // When a label, a button, small text and a window's title take their style's font
    let style = ctx.global_style();
    let font = |s: egui::TextStyle| style.text_styles.get(&s).cloned();
    // Then each is a named size, in the regular face
    assert_eq!(font(egui::TextStyle::Body), Some(t.body()));
    assert_eq!(font(egui::TextStyle::Button), Some(t.body()));
    assert_eq!(font(egui::TextStyle::Small), Some(t.caption()));
    // And a window's title is the panel's name, in semibold
    assert_eq!(font(egui::TextStyle::Heading), Some(t.panel_title_strong()));
}
