//! The HUD toast's timing: how long a message stays (long enough to read), how it fades in and
//! out, and how it is replaced. Painting lives in `panels::toast`.

use lightcraft_catalog::ColorLabel;

/// Fade-in length (seconds) of a new toast.
pub const FADE_IN: f64 = 0.12;
/// Fade-out length (seconds): the last stretch before `until`.
pub const FADE_OUT: f64 = 0.25;
/// How long a toast stays after the pointer leaves it, fade-out included: 1.5 s of reading plus the fade.
pub const HOVER_HOLD: f64 = 1.5 + FADE_OUT;
/// Reading time never exceeds this, unless the caller's floor is higher.
const MAX_READING: f64 = 10.0;

/// The message on screen. `until` is when it is gone (its fade-out is the last [`FADE_OUT`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    /// App time the toast first appeared; a re-shown identical text keeps it (no fade-in flicker).
    pub shown_at: f64,
    pub until: f64,
    /// The pointer rested on it last frame (the painter holds `until` ahead meanwhile).
    pub hovered: bool,
    /// Colour-label styling.
    pub label: Option<ColorLabel>,
}

/// CJK ideographs, kana and hangul: scripts that don't separate words with spaces.
fn is_unspaced_script(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x3040..=0x30FF | 0x3130..=0x318F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF66..=0xFF9F | 0x20000..=0x2FA1F)
}

/// Seconds a message needs to be read: a beat to notice it plus about 180 words a minute.
/// Whitespace-separated tokens count one word; each unspaced-script character counts half a word
/// (a token containing any counts only those characters, never both). Capped at 10 s.
pub fn reading_seconds(text: &str) -> f64 {
    let words: f64 = text
        .split_whitespace()
        .map(|token| match token.chars().filter(|&c| is_unspaced_script(c)).count() {
            0 => 1.0,
            n => n as f64 * 0.5,
        })
        .sum();
    (1.0 + words / 3.0).min(MAX_READING)
}

/// Does the toast need another frame soon? Not while held under the pointer once faded in.
pub fn needs_repaint(hovered: bool, now: f64, shown_at: f64) -> bool {
    !hovered || now - shown_at < FADE_IN
}

/// Opacity (0–1) at `now`: the lower of the ease-out fade-in and the linear fade-out.
pub fn toast_alpha(now: f64, shown_at: f64, until: f64) -> f32 {
    let x = ((now - shown_at) / FADE_IN).clamp(0.0, 1.0);
    let fade_in = 1.0 - (1.0 - x).powi(3);
    let fade_out = ((until - now) / FADE_OUT).clamp(0.0, 1.0);
    fade_in.min(fade_out) as f32
}

impl Toast {
    /// How long a message stays: the reading time, never below the caller's `floor`.
    pub fn duration(text: &str, floor: f64) -> f64 {
        reading_seconds(text).max(floor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_latin_text_gets_a_beat_plus_its_words() {
        assert!((reading_seconds("Library saved") - (1.0 + 2.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn long_instruction_needs_over_five_seconds() {
        let t = reading_seconds("Smart albums update automatically: change their rules, or use Photo ▸ Delete Photo");
        assert!((t - (1.0 + 13.0 / 3.0)).abs() < 1e-9, "{t}");
    }

    #[test]
    fn cjk_characters_count_half_a_word_each() {
        // 6 ideographs/kana = 3 words
        assert!((reading_seconds("ライブラリを保存") - (1.0 + 4.0 / 3.0)).abs() < 1e-9);
        assert!((reading_seconds("라이브러리") - (1.0 + 2.5 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn a_token_with_cjk_is_not_counted_twice() {
        // one token with two ideographs: 1 word, not 1 + 1
        assert!((reading_seconds("保存済") - (1.0 + 1.5 / 3.0)).abs() < 1e-9);
        assert!((reading_seconds("a保存") - reading_seconds("保存")).abs() < 1e-9);
    }

    #[test]
    fn empty_or_blank_text_is_just_the_beat() {
        assert_eq!(reading_seconds(""), 1.0);
        assert_eq!(reading_seconds("  \n\t "), 1.0);
    }

    #[test]
    fn reading_time_is_capped_at_ten_seconds() {
        assert_eq!(reading_seconds(&"word ".repeat(500)), 10.0);
    }

    #[test]
    fn the_callers_floor_wins_over_a_short_reading_and_over_the_cap() {
        assert_eq!(Toast::duration("Hi", 1.4), 1.4);
        assert_eq!(Toast::duration("Hi", 6.0), 6.0);
        assert_eq!(Toast::duration(&"word ".repeat(500), 12.0), 12.0);
        assert_eq!(Toast::duration(&"word ".repeat(500), 1.4), 10.0);
    }

    #[test]
    fn a_hovered_toast_asks_for_frames_only_while_fading_in() {
        assert!(needs_repaint(true, 5.05, 5.0));
        assert!(!needs_repaint(true, 5.2, 5.0));
        assert!(needs_repaint(false, 9.0, 5.0));
    }

    #[test]
    fn alpha_fades_in_then_holds_then_fades_out() {
        assert_eq!(toast_alpha(5.0, 5.0, 7.0), 0.0);
        let mid = toast_alpha(5.06, 5.0, 7.0);
        assert!(mid > 0.5 && mid < 1.0, "ease-out is past linear halfway at half time: {mid}");
        assert_eq!(toast_alpha(6.0, 5.0, 7.0), 1.0);
        assert!((toast_alpha(6.875, 5.0, 7.0) - 0.5).abs() < 1e-6);
        assert_eq!(toast_alpha(7.0, 5.0, 7.0), 0.0);
    }

    #[test]
    fn a_toast_shorter_than_both_fades_takes_the_lower() {
        let a = toast_alpha(5.06, 5.0, 5.1);
        assert!(a <= ((5.1 - 5.06) / FADE_OUT) as f32 + 1e-6);
    }
}
