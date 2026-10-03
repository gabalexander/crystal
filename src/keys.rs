//! Turning keys into the bytes a terminal would send for them, so that a
//! session gets what it would get if it ran in a terminal of its own: the
//! keys crossterm reports to the TUI, and the keys named to
//! `crystal send-keys`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Whether `key` is Ctrl+\, which hands the keyboard back from the pane to
/// the sidebar, as it detaches `crystal attach`. Terminals send it as the
/// byte 0x1c, which crossterm reports as Ctrl+4.
pub fn is_hand_back(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    ctrl && matches!(key.code, KeyCode::Char('\\') | KeyCode::Char('4'))
}

/// The bytes a terminal sends for `key`, or `None` for a key it sends
/// nothing for. `application_cursor` is the mode a program can ask for, in
/// which the arrows and Home/End send `ESC O` instead of `ESC [`.
pub fn encode(key: &KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let bytes = match key.code {
        KeyCode::Char(c) if ctrl => vec![control_byte(c)?],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => b"\x7f".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Up => cursor_key(b'A', key.modifiers, application_cursor),
        KeyCode::Down => cursor_key(b'B', key.modifiers, application_cursor),
        KeyCode::Right => cursor_key(b'C', key.modifiers, application_cursor),
        KeyCode::Left => cursor_key(b'D', key.modifiers, application_cursor),
        KeyCode::Home => cursor_key(b'H', key.modifiers, application_cursor),
        KeyCode::End => cursor_key(b'F', key.modifiers, application_cursor),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(n) => function_key(n)?,
        _ => return None,
    };
    // Alt is sent as an escape in front of the key: what most terminals do
    // when Alt is used as Meta.
    if alt && matches!(key.code, KeyCode::Char(_)) {
        let mut with_escape = vec![0x1b];
        with_escape.extend(bytes);
        return Some(with_escape);
    }
    Some(bytes)
}

/// The key a name stands for, in the names tmux's `send-keys` uses:
/// `Enter`, `Escape`, `Tab`, `BTab` (Shift+Tab), `BSpace`, `Space`, the
/// arrows `Up` `Down` `Left` `Right`, `Home`, `End`, `PageUp` (`PPage`),
/// `PageDown` (`NPage`), `Delete`, `F1`…`F12`, and `C-x` or `M-x` for a key
/// with Ctrl or Alt held. Names aren't case-sensitive. Anything else isn't
/// a name, and `None` says so.
pub fn named(name: &str) -> Option<KeyEvent> {
    if let Some(key) = name.strip_prefix("C-").or_else(|| name.strip_prefix("c-")) {
        return with_modifier(key, KeyModifiers::CONTROL);
    }
    if let Some(key) = name.strip_prefix("M-").or_else(|| name.strip_prefix("m-")) {
        return with_modifier(key, KeyModifiers::ALT);
    }
    let code = match name.to_ascii_lowercase().as_str() {
        "enter" => KeyCode::Enter,
        "escape" | "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "btab" => KeyCode::BackTab,
        "bspace" => KeyCode::Backspace,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "ppage" => KeyCode::PageUp,
        "pagedown" | "npage" => KeyCode::PageDown,
        "delete" | "dc" => KeyCode::Delete,
        lower => match lower.strip_prefix('f').map(str::parse::<u8>) {
            Some(Ok(n)) if (1..=12).contains(&n) => KeyCode::F(n),
            _ => return None,
        },
    };
    Some(KeyEvent::new(code, KeyModifiers::NONE))
}

/// What pressing `key` sends: the key it names, or else the text as it
/// is, the way typing it would send it.
pub fn keystrokes(key: &str, application_cursor: bool) -> Vec<u8> {
    named(key)
        .and_then(|named| encode(&named, application_cursor))
        .unwrap_or_else(|| key.as_bytes().to_vec())
}

/// `key`, a single character or a name, with `modifier` held too.
fn with_modifier(key: &str, modifier: KeyModifiers) -> Option<KeyEvent> {
    let mut chars = key.chars();
    let mut event = match (chars.next(), chars.next()) {
        (Some(c), None) => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        _ => named(key)?,
    };
    event.modifiers |= modifier;
    Some(event)
}

/// Ctrl+letter is the letter's position in the alphabet: Ctrl+A is 1. A few
/// punctuation keys fill the rest of the 32 control bytes.
fn control_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        letter @ 'a'..='z' => Some(letter as u8 - b'a' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' => Some(0x1f),
        _ => None,
    }
}

/// An arrow, Home or End. With Shift, Alt or Ctrl held it carries them as
/// a number, the way xterm does: `ESC [ 1 ; 5 A` is Ctrl+Up.
fn cursor_key(final_byte: u8, modifiers: KeyModifiers, application_cursor: bool) -> Vec<u8> {
    let held = modifier_number(modifiers);
    if held > 1 {
        let mut bytes = format!("\x1b[1;{held}").into_bytes();
        bytes.push(final_byte);
        return bytes;
    }
    if application_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// xterm's number for the modifiers held: 1, plus 1 for Shift, 2 for Alt
/// and 4 for Ctrl.
fn modifier_number(modifiers: KeyModifiers) -> u8 {
    let mut number = 1;
    if modifiers.contains(KeyModifiers::SHIFT) {
        number += 1;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        number += 2;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        number += 4;
    }
    number
}

fn function_key(n: u8) -> Option<Vec<u8>> {
    let sequence = match n {
        1 => "\x1bOP",
        2 => "\x1bOQ",
        3 => "\x1bOR",
        4 => "\x1bOS",
        5 => "\x1b[15~",
        6 => "\x1b[17~",
        7 => "\x1b[18~",
        8 => "\x1b[19~",
        9 => "\x1b[20~",
        10 => "\x1b[21~",
        11 => "\x1b[23~",
        12 => "\x1b[24~",
        _ => return None,
    };
    Some(sequence.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn encoded(key: KeyEvent) -> Vec<u8> {
        encode(&key, false).unwrap()
    }

    #[test]
    fn names_stand_for_their_keys() {
        let bytes = |name: &str| encode(&named(name).unwrap(), false).unwrap();
        assert_eq!(bytes("Enter"), b"\r");
        assert_eq!(bytes("escape"), b"\x1b");
        assert_eq!(bytes("Up"), b"\x1b[A");
        assert_eq!(bytes("BTab"), b"\x1b[Z");
        assert_eq!(bytes("C-c"), [3]);
        assert_eq!(bytes("M-b"), b"\x1bb");
        assert_eq!(bytes("F5"), b"\x1b[15~");
    }

    #[test]
    fn a_word_that_isnt_a_name_is_typed_as_it_is() {
        assert_eq!(keystrokes("1", false), b"1");
        assert_eq!(keystrokes("yes", false), b"yes");
        assert_eq!(keystrokes("Up", true), b"\x1bOA");
    }

    #[test]
    fn a_word_that_isnt_a_name_is_none() {
        assert_eq!(named("hello"), None);
        assert_eq!(named("1"), None);
        assert_eq!(named("F13"), None);
    }

    #[test]
    fn printable_keys_are_their_utf8() {
        assert_eq!(encoded(key(KeyCode::Char('a'))), b"a");
        assert_eq!(encoded(with(KeyCode::Char('A'), KeyModifiers::SHIFT)), b"A");
        assert_eq!(encoded(key(KeyCode::Char('é'))), "é".as_bytes());
    }

    #[test]
    fn editing_keys_are_what_a_terminal_sends() {
        assert_eq!(encoded(key(KeyCode::Enter)), b"\r");
        assert_eq!(encoded(key(KeyCode::Backspace)), b"\x7f");
        assert_eq!(encoded(key(KeyCode::Tab)), b"\t");
        assert_eq!(encoded(key(KeyCode::BackTab)), b"\x1b[Z");
        assert_eq!(encoded(key(KeyCode::Esc)), b"\x1b");
        assert_eq!(encoded(key(KeyCode::Delete)), b"\x1b[3~");
        assert_eq!(encoded(key(KeyCode::PageUp)), b"\x1b[5~");
        assert_eq!(encoded(key(KeyCode::PageDown)), b"\x1b[6~");
    }

    #[test]
    fn ctrl_letters_are_control_bytes() {
        assert_eq!(
            encoded(with(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            [3]
        );
        assert_eq!(
            encoded(with(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            [1]
        );
        assert_eq!(
            encoded(with(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            [26]
        );
    }

    #[test]
    fn alt_puts_an_escape_in_front() {
        assert_eq!(
            encoded(with(KeyCode::Char('b'), KeyModifiers::ALT)),
            b"\x1bb"
        );
    }

    #[test]
    fn arrows_follow_the_programs_cursor_mode() {
        assert_eq!(encode(&key(KeyCode::Up), false).unwrap(), b"\x1b[A");
        assert_eq!(encode(&key(KeyCode::Up), true).unwrap(), b"\x1bOA");
        assert_eq!(encode(&key(KeyCode::Home), false).unwrap(), b"\x1b[H");
        assert_eq!(encode(&key(KeyCode::End), true).unwrap(), b"\x1bOF");
    }

    #[test]
    fn modified_arrows_carry_the_modifiers() {
        assert_eq!(
            encoded(with(KeyCode::Right, KeyModifiers::CONTROL)),
            b"\x1b[1;5C"
        );
        assert_eq!(
            encoded(with(KeyCode::Left, KeyModifiers::SHIFT)),
            b"\x1b[1;2D"
        );
    }

    #[test]
    fn function_keys_are_xterms() {
        assert_eq!(encoded(key(KeyCode::F(1))), b"\x1bOP");
        assert_eq!(encoded(key(KeyCode::F(12))), b"\x1b[24~");
        assert_eq!(encode(&key(KeyCode::F(13)), false), None);
    }

    #[test]
    fn ctrl_backslash_hands_the_keyboard_back_however_it_is_reported() {
        assert!(is_hand_back(&with(
            KeyCode::Char('\\'),
            KeyModifiers::CONTROL
        )));
        assert!(is_hand_back(&with(
            KeyCode::Char('4'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_hand_back(&key(KeyCode::Char('\\'))));
    }
}
