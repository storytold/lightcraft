//! Hardware keyboards seen through USB HID usages (page 7), as iPadOS reports them (`UIKey`):
//! the shells that receive raw key codes instead of winit key events turn them into egui keys and
//! modifiers here. winit 0.30's iOS backend only passes typed text on, so without this an iPad's
//! keyboard couldn't reach a single shortcut.

use egui::{Key, Modifiers};

/// The egui key for HID usage `usage` (keyboard page), if egui has one.
pub fn key(usage: i64) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    // 1 … 9, then 0
    const DIGITS: [Key; 10] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9, Key::Num0];
    const FUNCTION: [Key; 12] = [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12];
    let at = |table: &[Key], first: i64| usize::try_from(usage - first).ok().and_then(|i| table.get(i).copied());
    Some(match usage {
        0x04..=0x1D => return at(&LETTERS, 0x04),
        0x1E..=0x27 => return at(&DIGITS, 0x1E),
        0x3A..=0x45 => return at(&FUNCTION, 0x3A),
        0x28 | 0x58 => Key::Enter, // Return, keypad Enter
        0x29 => Key::Escape,
        0x2A => Key::Backspace,
        0x2B => Key::Tab,
        0x2C => Key::Space,
        0x2D | 0x56 => Key::Minus,
        0x2E => Key::Equals,
        0x2F => Key::OpenBracket,
        0x30 => Key::CloseBracket,
        0x31 => Key::Backslash,
        0x33 => Key::Semicolon,
        0x34 => Key::Quote,
        0x35 => Key::Backtick,
        0x36 => Key::Comma,
        0x37 => Key::Period,
        0x38 | 0x54 => Key::Slash,
        0x49 => Key::Insert,
        0x4A => Key::Home,
        0x4B => Key::PageUp,
        0x4C => Key::Delete,
        0x4D => Key::End,
        0x4E => Key::PageDown,
        0x4F => Key::ArrowRight,
        0x50 => Key::ArrowLeft,
        0x51 => Key::ArrowDown,
        0x52 => Key::ArrowUp,
        0x57 => Key::Plus, // keypad +
        _ => return None,
    })
}

/// egui modifiers from Apple's `UIKeyModifierFlags` (⇧ 1<<17, ⌃ 1<<18, ⌥ 1<<19, ⌘ 1<<20): ⌘ is
/// the command key, as on a Mac.
pub fn apple_modifiers(flags: i64) -> Modifiers {
    let has = |bit: u32| flags & (1i64 << bit) != 0;
    let cmd = has(20);
    Modifiers { alt: has(19), ctrl: has(18), shift: has(17), mac_cmd: cmd, command: cmd }
}

/// While a text field is being edited: is this a key the app handles itself (⌘ / ⌃ combinations,
/// arrows, Escape, Tab, the navigation and function keys), rather than one that types (letters,
/// digits, Return, ⌫ — those go through the system's text input, with its dead keys)?
pub fn edits_by_key(key: Key, m: Modifiers) -> bool {
    m.command
        || m.ctrl
        || matches!(
            key,
            Key::Escape
                | Key::Tab
                | Key::ArrowUp
                | Key::ArrowDown
                | Key::ArrowLeft
                | Key::ArrowRight
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Delete
                | Key::Insert
                | Key::F1
                | Key::F2
                | Key::F3
                | Key::F4
                | Key::F5
                | Key::F6
                | Key::F7
                | Key::F8
                | Key::F9
                | Key::F10
                | Key::F11
                | Key::F12
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usages_map_to_keys() {
        assert_eq!(key(0x04), Some(Key::A));
        assert_eq!(key(0x1D), Some(Key::Z));
        assert_eq!(key(0x1E), Some(Key::Num1));
        assert_eq!(key(0x27), Some(Key::Num0));
        assert_eq!(key(0x2A), Some(Key::Backspace));
        assert_eq!(key(0x4C), Some(Key::Delete));
        assert_eq!(key(0x50), Some(Key::ArrowLeft));
        assert_eq!(key(0x3A), Some(Key::F1));
        assert_eq!(key(0x45), Some(Key::F12));
        assert_eq!(key(0xE3), None, "the modifier keys themselves come as flags");
        assert_eq!(key(-1), None);
        assert_eq!(key(i64::MAX), None);
    }

    #[test]
    fn in_a_text_field_shortcuts_and_navigation_are_keys_and_the_rest_types() {
        let cmd = apple_modifiers(1 << 20);
        assert!(edits_by_key(Key::C, cmd), "⌘C copies");
        assert!(edits_by_key(Key::A, apple_modifiers(1 << 18)), "⌃A");
        assert!(edits_by_key(Key::ArrowLeft, Modifiers::NONE));
        assert!(edits_by_key(Key::ArrowLeft, Modifiers::SHIFT), "⇧← selects");
        assert!(edits_by_key(Key::Escape, Modifiers::NONE));
        assert!(!edits_by_key(Key::C, Modifiers::NONE), "c types");
        assert!(!edits_by_key(Key::E, apple_modifiers(1 << 19)), "⌥e is a dead key");
        assert!(!edits_by_key(Key::Enter, Modifiers::NONE));
        assert!(!edits_by_key(Key::Backspace, Modifiers::NONE));
    }

    #[test]
    fn command_is_the_command_key() {
        let m = apple_modifiers((1 << 20) | (1 << 17));
        assert!(m.command && m.mac_cmd && m.shift && !m.ctrl && !m.alt);
        let m = apple_modifiers(1 << 18);
        assert!(m.ctrl && !m.command, "⌃ is not ⌘");
        assert_eq!(apple_modifiers(1 << 16), Modifiers::NONE, "Caps Lock is no modifier");
    }
}
