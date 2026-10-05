//! Turning keys into the bytes a terminal would send for them, so that a
//! session gets what it would get if it ran in a terminal of its own: the
//! keys crossterm reports to the TUI, and the keys named to
//! `crystal send-keys`. And back: what your terminal sends `crystal
//! attach`, read into keys, for the few it takes for itself and for copy
//! mode.

use crate::vt;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::fmt::Write as _;
use std::ops::Range;

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

/// The bytes `key` sends the program on `screen`: in the Kitty keyboard
/// protocol, once the program has asked for it, and the old way otherwise.
pub fn encode_for(key: &KeyEvent, screen: &vt::Screen) -> Option<Vec<u8>> {
    let application_cursor = screen.application_cursor();
    kitty(key, screen.kitty_keyboard(), application_cursor)
        .or_else(|| encode(key, application_cursor))
}

/// The Kitty keyboard protocol's flags, which a program pushes with
/// `CSI > flags u`.
mod flag {
    /// Escapes for the keys the old way can't tell apart: Esc, and keys
    /// with Ctrl or Alt.
    pub const DISAMBIGUATE: u8 = 1;
    /// Saying whether a key was pressed, repeated or released.
    pub const EVENT_TYPES: u8 = 2;
    /// The shifted key too, like `A` for Shift+a.
    pub const ALTERNATE_KEYS: u8 = 4;
    /// Escapes for every key, text and Enter included.
    pub const ALL_KEYS: u8 = 8;
    /// The text a key types, after its escape.
    pub const ASSOCIATED_TEXT: u8 = 16;
}

/// The bytes for `key` in the Kitty keyboard protocol, with `flags` the
/// program pushed: `None` when the flags don't change how keys are
/// written, or for a key the protocol leaves the old way.
fn kitty(key: &KeyEvent, flags: u8, application_cursor: bool) -> Option<Vec<u8>> {
    if flags & (flag::DISAMBIGUATE | flag::ALL_KEYS) == 0 {
        return None;
    }
    let all_keys = flags & flag::ALL_KEYS != 0;
    let event = match key.kind {
        KeyEventKind::Press => None,
        _ if flags & flag::EVENT_TYPES == 0 => {
            // Only a program that asked hears keys repeat and come up.
            return (key.kind == KeyEventKind::Repeat)
                .then(|| {
                    kitty(
                        &KeyEvent::new(key.code, key.modifiers),
                        flags,
                        application_cursor,
                    )
                })
                .flatten();
        }
        KeyEventKind::Repeat => Some(2),
        KeyEventKind::Release => Some(3),
    };
    let mut modifiers = key.modifiers;
    // Shift+Tab is Tab with Shift held.
    if key.code == KeyCode::BackTab {
        modifiers |= KeyModifiers::SHIFT;
    }
    let held = kitty_modifiers(modifiers);
    let plain = held == 1 && event.is_none();
    // With Ctrl, Alt or the like held, a key doesn't type its text.
    let types_text = kitty_modifiers(modifiers - KeyModifiers::SHIFT) == 1;
    let ss3 = application_cursor && plain && !all_keys;
    let bytes = match key.code {
        KeyCode::Char(c) => {
            if types_text && !all_keys && key.kind != KeyEventKind::Release {
                return Some(c.to_string().into_bytes());
            }
            let base = unshifted(c);
            let mut code = u32::from(base).to_string();
            if flags & flag::ALTERNATE_KEYS != 0 && c != base {
                let _ = write!(code, ":{}", u32::from(c));
            }
            let text = (flags & flag::ASSOCIATED_TEXT != 0 && all_keys && types_text)
                .then(|| u32::from(c).to_string());
            csi_u(&code, held, event, text.as_deref())
        }
        KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab | KeyCode::Backspace => {
            let (legacy, code) = match key.code {
                KeyCode::Enter => ("\r", "13"),
                KeyCode::Backspace => ("\x7f", "127"),
                _ => ("\t", "9"),
            };
            // Unless every key is to be an escape, these stay what they
            // were, so a shell can still be typed into after a program
            // that left the protocol on.
            if plain && !all_keys {
                return Some(legacy.as_bytes().to_vec());
            }
            csi_u(code, held, event, None)
        }
        KeyCode::Esc => csi_u("27", held, event, None),
        KeyCode::Up => letter_key('A', held, event, ss3),
        KeyCode::Down => letter_key('B', held, event, ss3),
        KeyCode::Right => letter_key('C', held, event, ss3),
        KeyCode::Left => letter_key('D', held, event, ss3),
        KeyCode::Home => letter_key('H', held, event, ss3),
        KeyCode::End => letter_key('F', held, event, ss3),
        KeyCode::Insert => tilde_key(2, held, event),
        KeyCode::Delete => tilde_key(3, held, event),
        KeyCode::PageUp => tilde_key(5, held, event),
        KeyCode::PageDown => tilde_key(6, held, event),
        // F3 is a number of its own, since `CSI R` reports the cursor.
        KeyCode::F(3) => tilde_key(13, held, event),
        KeyCode::F(n @ 1..=4) => {
            let letter = char::from(b'P' + n - 1);
            letter_key(letter, held, event, plain && !all_keys)
        }
        KeyCode::F(n) => tilde_key(function_key_number(n)?, held, event),
        _ => return None,
    };
    Some(bytes.into_bytes())
}

/// The protocol's number for the modifiers held: 1, plus 1 for Shift, 2 for
/// Alt, 4 for Ctrl, 8 for Super, 16 for Hyper and 32 for Meta.
fn kitty_modifiers(modifiers: KeyModifiers) -> u8 {
    let bits = [
        (KeyModifiers::SHIFT, 1),
        (KeyModifiers::ALT, 2),
        (KeyModifiers::CONTROL, 4),
        (KeyModifiers::SUPER, 8),
        (KeyModifiers::HYPER, 16),
        (KeyModifiers::META, 32),
    ];
    bits.iter()
        .filter(|(modifier, _)| modifiers.contains(*modifier))
        .fold(1, |held, (_, bit)| held + bit)
}

/// `;modifiers:event`, or as little of it as says the same.
fn modifiers_field(held: u8, event: Option<u8>) -> String {
    match event {
        Some(event) => format!(";{held}:{event}"),
        None if held > 1 => format!(";{held}"),
        None => String::new(),
    }
}

/// `CSI code ; modifiers:event ; text u`.
fn csi_u(code: &str, held: u8, event: Option<u8>, text: Option<&str>) -> String {
    let mut fields = modifiers_field(held, event);
    if let Some(text) = text {
        if fields.is_empty() {
            fields.push_str(";1");
        }
        let _ = write!(fields, ";{text}");
    }
    format!("\x1b[{code}{fields}u")
}

/// A key written with a letter at its end, like the arrows: `CSI A`, or
/// `CSI 1 ; modifiers A` with modifiers, or `SS3 A` where the old way
/// would write that.
fn letter_key(letter: char, held: u8, event: Option<u8>, ss3: bool) -> String {
    let fields = modifiers_field(held, event);
    if ss3 {
        format!("\x1bO{letter}")
    } else if fields.is_empty() {
        format!("\x1b[{letter}")
    } else {
        format!("\x1b[1{fields}{letter}")
    }
}

/// A key written with a number and `~`, like Delete: `CSI 3 ~`.
fn tilde_key(number: u8, held: u8, event: Option<u8>) -> String {
    format!("\x1b[{number}{}~", modifiers_field(held, event))
}

/// The number F5 to F12 are written with.
fn function_key_number(n: u8) -> Option<u8> {
    let number = match n {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(number)
}

/// The character a key types without Shift, on a US keyboard: the key the
/// protocol names.
fn unshifted(c: char) -> char {
    match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        c => c.to_lowercase().next().unwrap_or(c),
    }
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

/// What pressing `key` sends the program on `screen`: the key it names, or
/// else the text as it is, the way typing it would send it.
pub fn keystrokes(key: &str, screen: &vt::Screen) -> Vec<u8> {
    named(key)
        .and_then(|named| encode_for(&named, screen))
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

/// What a terminal sent, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Key(KeyEvent),
    /// What was pasted, marked as a paste because the program asked.
    Paste(String),
    /// Not a key: a mouse report, the terminal saying it gained the focus,
    /// or a sequence crystal doesn't read.
    Other,
}

/// Reads what a terminal sent into what was typed, each with the bytes it
/// took: keys the old way or in the Kitty keyboard protocol, as
/// [`encode_for`] writes them, pastes, and the rest. A sequence cut off at
/// the end of `bytes` is taken as far as it goes.
pub fn decode(bytes: &[u8]) -> Vec<(Typed, Range<usize>)> {
    let mut decoded = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let (typed, len) = decode_one(&bytes[at..]);
        decoded.push((typed, at..at + len));
        at += len;
    }
    decoded
}

/// What `bytes`, which aren't empty, start with, and how many bytes it is.
fn decode_one(bytes: &[u8]) -> (Typed, usize) {
    let pressed = |code| Typed::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let ctrl = |c| Typed::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    let typed = match bytes[0] {
        0x1b => return escape(bytes),
        b'\r' => pressed(KeyCode::Enter),
        b'\t' => pressed(KeyCode::Tab),
        0x7f | 0x08 => pressed(KeyCode::Backspace),
        0x00 => ctrl(' '),
        byte @ 0x01..=0x1a => ctrl(char::from(byte - 1 + b'a')),
        // Ctrl+\, Ctrl+], Ctrl+^ and Ctrl+_, as crossterm names them.
        byte @ 0x1c..=0x1f => ctrl(char::from(byte - 0x1c + b'4')),
        _ => return character(bytes),
    };
    (typed, 1)
}

/// The character `bytes` start with, in UTF-8, as its key.
fn character(bytes: &[u8]) -> (Typed, usize) {
    let len = match bytes[0] {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    };
    let len = len.min(bytes.len());
    let c = std::str::from_utf8(&bytes[..len])
        .ok()
        .and_then(|text| text.chars().next());
    match c {
        Some(c) => (Typed::Key(char_key(c)), len),
        None => (Typed::Other, len),
    }
}

/// The key that types `c`, Shift held for a capital, as crossterm has it.
fn char_key(c: char) -> KeyEvent {
    let modifiers = if c.is_ascii_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    KeyEvent::new(KeyCode::Char(c), modifiers)
}

/// What `bytes`, which start with an escape, are: a CSI or SS3 sequence, a
/// key with Alt held, or Esc on its own.
fn escape(bytes: &[u8]) -> (Typed, usize) {
    let pressed = |code| Typed::Key(KeyEvent::new(code, KeyModifiers::NONE));
    match bytes.get(1) {
        None | Some(0x1b) => (pressed(KeyCode::Esc), 1),
        Some(b'[') => csi(bytes),
        Some(b'O') if bytes.len() > 2 => {
            let code = match bytes[2] {
                b'A' => KeyCode::Up,
                b'B' => KeyCode::Down,
                b'C' => KeyCode::Right,
                b'D' => KeyCode::Left,
                b'H' => KeyCode::Home,
                b'F' => KeyCode::End,
                letter @ b'P'..=b'S' => KeyCode::F(letter - b'P' + 1),
                _ => return (Typed::Other, 3),
            };
            (pressed(code), 3)
        }
        Some(_) => match decode_one(&bytes[1..]) {
            (Typed::Key(mut key), len) => {
                key.modifiers |= KeyModifiers::ALT;
                (Typed::Key(key), len + 1)
            }
            (_, len) => (Typed::Other, len + 1),
        },
    }
}

/// The end of a bracketed paste.
const PASTE_END: &[u8] = b"\x1b[201~";

/// What a CSI sequence at the start of `bytes` is, and how long it is: its
/// parameters, then any intermediate bytes, then the byte that ends it.
fn csi(bytes: &[u8]) -> (Typed, usize) {
    // A mouse report the old way: three bytes after `CSI M`.
    if bytes.get(2) == Some(&b'M') {
        return (Typed::Other, bytes.len().min(6));
    }
    let params_end = (2..bytes.len())
        .find(|&at| !(0x30..=0x3f).contains(&bytes[at]))
        .unwrap_or(bytes.len());
    let end = (params_end..bytes.len())
        .find(|&at| !(0x20..=0x2f).contains(&bytes[at]))
        .unwrap_or(bytes.len());
    let Some(&last) = bytes.get(end) else {
        return (Typed::Other, bytes.len());
    };
    let len = end + 1;
    let params = String::from_utf8_lossy(&bytes[2..params_end]);
    if params == "200" && last == b'~' {
        let rest = &bytes[len..];
        let ends = rest.windows(PASTE_END.len()).position(|w| w == PASTE_END);
        let (pasted, taken) = match ends {
            Some(at) => (&rest[..at], at + PASTE_END.len()),
            None => (rest, rest.len()),
        };
        let pasted = String::from_utf8_lossy(pasted).into_owned();
        return (Typed::Paste(pasted), len + taken);
    }
    // Private parameters, like an SGR mouse report's `<`, aren't keys.
    if end != params_end || params.starts_with(['<', '=', '>', '?']) {
        return (Typed::Other, len);
    }
    let key = match last {
        b'u' => kitty_key(&params),
        b'A' => csi_key(KeyCode::Up, &params),
        b'B' => csi_key(KeyCode::Down, &params),
        b'C' => csi_key(KeyCode::Right, &params),
        b'D' => csi_key(KeyCode::Left, &params),
        b'H' => csi_key(KeyCode::Home, &params),
        b'F' => csi_key(KeyCode::End, &params),
        b'P'..=b'S' => csi_key(KeyCode::F(last - b'P' + 1), &params),
        b'Z' => Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        b'~' => tilde_code(&params).and_then(|code| csi_key(code, &params)),
        _ => None,
    };
    (key.map_or(Typed::Other, Typed::Key), len)
}

/// A key written with a letter or `~` at its end, with the modifiers and
/// the event in its second parameter: `1;5A` is Ctrl+Up.
fn csi_key(code: KeyCode, params: &str) -> Option<KeyEvent> {
    let (modifiers, kind) = held(params.split(';').nth(1))?;
    Some(KeyEvent::new_with_kind(code, modifiers, kind))
}

/// The key a number before `~` stands for.
fn tilde_code(params: &str) -> Option<KeyCode> {
    let number: u8 = params.split(';').next()?.parse().ok()?;
    let code = match number {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F(number - 10),
        17..=21 => KeyCode::F(number - 11),
        23 | 24 => KeyCode::F(number - 12),
        _ => return None,
    };
    Some(code)
}

/// The modifiers and the event a field of `modifiers:event` says, as the
/// Kitty protocol and xterm write them: one more than the modifiers' bits,
/// Caps Lock and Num Lock not counted, and 1 for a press, 2 a repeat, 3 a
/// release. A field left out is a press with none held.
fn held(field: Option<&str>) -> Option<(KeyModifiers, KeyEventKind)> {
    let mut parts = field.unwrap_or("").split(':');
    let number = |part: Option<&str>| match part {
        None | Some("") => Some(1),
        Some(part) => part.parse::<u16>().ok(),
    };
    let bits = number(parts.next())?.checked_sub(1)?;
    let mut modifiers = KeyModifiers::NONE;
    for (bit, modifier) in [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
        (8, KeyModifiers::SUPER),
        (16, KeyModifiers::HYPER),
        (32, KeyModifiers::META),
    ] {
        if bits & bit != 0 {
            modifiers |= modifier;
        }
    }
    let kind = match number(parts.next())? {
        1 => KeyEventKind::Press,
        2 => KeyEventKind::Repeat,
        3 => KeyEventKind::Release,
        _ => return None,
    };
    Some((modifiers, kind))
}

/// A key in the Kitty protocol, `CSI code:shifted ; modifiers:event ; text
/// u`: the shifted key, or the text, says what Shift made of it where
/// they're given.
fn kitty_key(params: &str) -> Option<KeyEvent> {
    let mut fields = params.split(';');
    let mut codes = fields.next().unwrap_or("").split(':');
    let (modifiers, kind) = held(fields.next())?;
    let text = fields.next().and_then(|text| {
        let first = text.split(':').next()?;
        char::from_u32(first.parse().ok()?)
    });
    let number: u32 = codes.next()?.parse().ok()?;
    let shifted = codes
        .next()
        .and_then(|shifted| char::from_u32(shifted.parse().ok()?));
    let code = match number {
        13 | 57414 => KeyCode::Enter,
        9 => KeyCode::Tab,
        27 => KeyCode::Esc,
        127 => KeyCode::Backspace,
        // The protocol's own numbers, for keys with no character: the
        // keypad's, the modifiers' on their own, and the like.
        57344.. => return None,
        _ => {
            let c = char::from_u32(number)?;
            let c = match (shifted, text) {
                _ if !modifiers.contains(KeyModifiers::SHIFT) => c,
                (Some(shifted), _) => shifted,
                (None, Some(text)) => text,
                (None, None) => shifted_char(c),
            };
            KeyCode::Char(c)
        }
    };
    if code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT) {
        return Some(KeyEvent::new_with_kind(KeyCode::BackTab, modifiers, kind));
    }
    Some(KeyEvent::new_with_kind(code, modifiers, kind))
}

/// The character Shift makes of `c` on a US keyboard: what [`unshifted`]
/// undoes.
fn shifted_char(c: char) -> char {
    let pairs = "1!2@3#4$5%6^7&8*9(0)-_=+[{]}\\|;:'\",<.>/?`~";
    let chars: Vec<char> = pairs.chars().collect();
    match chars.chunks(2).find(|pair| pair[0] == c) {
        Some(pair) => pair[1],
        None => c.to_uppercase().next().unwrap_or(c),
    }
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

    /// A screen after the program on it wrote `output`.
    fn screen(output: &[u8]) -> vt::Screen {
        let mut screen = vt::Screen::new(2, 10);
        screen.process(output);
        screen
    }

    #[test]
    fn a_word_that_isnt_a_name_is_typed_as_it_is() {
        let plain = screen(b"");
        assert_eq!(keystrokes("1", &plain), b"1");
        assert_eq!(keystrokes("yes", &plain), b"yes");
        assert_eq!(keystrokes("Up", &screen(b"\x1b[?1h")), b"\x1bOA");
    }

    /// `key` in the Kitty protocol with `flags`, as text.
    fn kitty_text(code: KeyCode, modifiers: KeyModifiers, flags: u8) -> String {
        let bytes = kitty(&with(code, modifiers), flags, false).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn disambiguating_gives_escapes_to_the_keys_the_old_way_cant_tell_apart() {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        let ctrl = KeyModifiers::CONTROL;
        let kitty = |code, modifiers| kitty_text(code, modifiers, flag::DISAMBIGUATE);
        assert_eq!(kitty(KeyCode::Enter, shift), "\x1b[13;2u");
        assert_eq!(kitty(KeyCode::Esc, none), "\x1b[27u");
        assert_eq!(kitty(KeyCode::Char('c'), ctrl), "\x1b[99;5u");
        assert_eq!(kitty(KeyCode::Char('a'), KeyModifiers::ALT), "\x1b[97;3u");
        assert_eq!(kitty(KeyCode::Char('A'), ctrl | shift), "\x1b[97;6u");
        assert_eq!(kitty(KeyCode::Char('\\'), ctrl), "\x1b[92;5u");
        assert_eq!(kitty(KeyCode::BackTab, shift), "\x1b[9;2u");
        assert_eq!(kitty(KeyCode::Up, ctrl), "\x1b[1;5A");
        assert_eq!(kitty(KeyCode::Delete, shift), "\x1b[3;2~");
        assert_eq!(kitty(KeyCode::F(3), none), "\x1b[13~");
        // Text stays text, and so do the keys a shell needs.
        assert_eq!(kitty(KeyCode::Char('a'), none), "a");
        assert_eq!(kitty(KeyCode::Char('A'), shift), "A");
        assert_eq!(kitty(KeyCode::Char('!'), shift), "!");
        assert_eq!(kitty(KeyCode::Char('é'), none), "é");
        assert_eq!(kitty(KeyCode::Enter, none), "\r");
        assert_eq!(kitty(KeyCode::Backspace, none), "\x7f");
        assert_eq!(kitty(KeyCode::Up, none), "\x1b[A");
        assert_eq!(kitty(KeyCode::F(1), none), "\x1bOP");
    }

    #[test]
    fn every_key_can_be_an_escape_with_its_shifted_key_and_text() {
        let all = flag::DISAMBIGUATE | flag::ALL_KEYS;
        assert_eq!(
            kitty_text(KeyCode::Char('a'), KeyModifiers::NONE, all),
            "\x1b[97u"
        );
        assert_eq!(
            kitty_text(KeyCode::Enter, KeyModifiers::NONE, all),
            "\x1b[13u"
        );
        assert_eq!(
            kitty_text(KeyCode::Char('A'), KeyModifiers::SHIFT, all),
            "\x1b[97;2u"
        );
        assert_eq!(kitty_text(KeyCode::F(1), KeyModifiers::NONE, all), "\x1b[P");
        let alternate = all | flag::ALTERNATE_KEYS;
        assert_eq!(
            kitty_text(KeyCode::Char('A'), KeyModifiers::SHIFT, alternate),
            "\x1b[97:65;2u"
        );
        let text = all | flag::ASSOCIATED_TEXT;
        assert_eq!(
            kitty_text(KeyCode::Char('A'), KeyModifiers::SHIFT, text),
            "\x1b[97;2;65u"
        );
    }

    #[test]
    fn a_program_that_asked_hears_keys_repeat_and_come_up() {
        let repeat = KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
            KeyEventKind::Repeat,
        );
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            ..repeat
        };
        let types = flag::DISAMBIGUATE | flag::EVENT_TYPES;
        assert_eq!(kitty(&repeat, types, false).unwrap(), b"\x1b[97;5:2u");
        assert_eq!(kitty(&release, types, false).unwrap(), b"\x1b[97;5:3u");
        // Without asking, a repeat is a press and a release is nothing.
        assert_eq!(
            kitty(&repeat, flag::DISAMBIGUATE, false).unwrap(),
            b"\x1b[97;5u"
        );
        assert_eq!(kitty(&release, flag::DISAMBIGUATE, false), None);
    }

    #[test]
    fn the_application_cursor_mode_still_counts_for_a_plain_arrow() {
        let up = with(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(kitty(&up, flag::DISAMBIGUATE, true).unwrap(), b"\x1bOA");
    }

    #[test]
    fn a_program_that_asked_for_the_kitty_protocol_gets_its_keys_that_way() {
        let kitty = screen(b"\x1b[>1u");
        assert_eq!(keystrokes("Escape", &kitty), b"\x1b[27u");
        assert_eq!(keystrokes("C-c", &kitty), b"\x1b[99;5u");
        assert_eq!(keystrokes("Enter", &kitty), b"\r");
        assert_eq!(keystrokes("yes", &kitty), b"yes");
        let shift_enter = with(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(encode_for(&shift_enter, &kitty).unwrap(), b"\x1b[13;2u");
        assert_eq!(encode_for(&shift_enter, &screen(b"")).unwrap(), b"\r");
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

    /// What `bytes` read back as, with the bytes each took.
    fn decoded(bytes: &[u8]) -> Vec<(Typed, Range<usize>)> {
        decode(bytes)
    }

    /// The one thing `bytes` read back as, which takes them all.
    fn decoded_one(bytes: &[u8]) -> Typed {
        let mut decoded = decode(bytes);
        assert_eq!(decoded.len(), 1, "{decoded:?}");
        let (typed, range) = decoded.remove(0);
        assert_eq!(range, 0..bytes.len());
        typed
    }

    fn typed(code: KeyCode, modifiers: KeyModifiers) -> Typed {
        Typed::Key(with(code, modifiers))
    }

    #[test]
    fn keys_the_old_way_read_back() {
        use KeyModifiers as M;
        let cases: &[(&[u8], Typed)] = &[
            (b"a", typed(KeyCode::Char('a'), M::NONE)),
            (b"V", typed(KeyCode::Char('V'), M::SHIFT)),
            ("é".as_bytes(), typed(KeyCode::Char('é'), M::NONE)),
            (b"\r", typed(KeyCode::Enter, M::NONE)),
            (b"\x7f", typed(KeyCode::Backspace, M::NONE)),
            (b"\x02", typed(KeyCode::Char('b'), M::CONTROL)),
            (b"\x1c", typed(KeyCode::Char('4'), M::CONTROL)),
            (b"\x1b", typed(KeyCode::Esc, M::NONE)),
            (b"\x1bb", typed(KeyCode::Char('b'), M::ALT)),
            (b"\x1b[A", typed(KeyCode::Up, M::NONE)),
            (b"\x1bOA", typed(KeyCode::Up, M::NONE)),
            (b"\x1b[1;5A", typed(KeyCode::Up, M::CONTROL)),
            (b"\x1b[5~", typed(KeyCode::PageUp, M::NONE)),
            (b"\x1b[6;2~", typed(KeyCode::PageDown, M::SHIFT)),
            (b"\x1b[Z", typed(KeyCode::BackTab, M::SHIFT)),
        ];
        for (bytes, key) in cases {
            assert_eq!(
                &decoded_one(bytes),
                key,
                "{:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn keys_in_the_kitty_protocol_read_back() {
        use KeyModifiers as M;
        let release = |code, modifiers| {
            Typed::Key(KeyEvent::new_with_kind(
                code,
                modifiers,
                KeyEventKind::Release,
            ))
        };
        let cases: &[(&[u8], Typed)] = &[
            (b"\x1b[98;5u", typed(KeyCode::Char('b'), M::CONTROL)),
            (b"\x1b[27u", typed(KeyCode::Esc, M::NONE)),
            (b"\x1b[13;2u", typed(KeyCode::Enter, M::SHIFT)),
            (b"\x1b[92;5:3u", release(KeyCode::Char('\\'), M::CONTROL)),
            // Caps Lock on doesn't count.
            (b"\x1b[92;69u", typed(KeyCode::Char('\\'), M::CONTROL)),
            // Shift's key, from the shifted key, the text, or a US keyboard.
            (b"\x1b[118:86;2u", typed(KeyCode::Char('V'), M::SHIFT)),
            (b"\x1b[47;2;63u", typed(KeyCode::Char('?'), M::SHIFT)),
            (b"\x1b[47;2u", typed(KeyCode::Char('?'), M::SHIFT)),
            (b"\x1b[9;2u", typed(KeyCode::BackTab, M::SHIFT)),
            // Left Shift on its own.
            (b"\x1b[57441;2u", Typed::Other),
        ];
        for (bytes, key) in cases {
            assert_eq!(
                &decoded_one(bytes),
                key,
                "{:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn what_isnt_a_key_reads_back_whole_as_other() {
        for bytes in [
            &b"\x1b[<64;3;2M"[..],
            b"\x1b[M #\"",
            b"\x1b[I",
            b"\x1b[?1u",
            b"\x1b[92;5",
        ] {
            let what = String::from_utf8_lossy(bytes);
            assert_eq!(decoded_one(bytes), Typed::Other, "{what:?}");
        }
    }

    #[test]
    fn a_paste_reads_back_whole_and_each_key_with_its_bytes() {
        let bytes = b"a\x1b[A\x1b[200~hi\r\x1b[201~\x1bOBz";
        let read: Vec<(Typed, Range<usize>)> = decoded(bytes);
        assert_eq!(
            read,
            [
                (typed(KeyCode::Char('a'), KeyModifiers::NONE), 0..1),
                (typed(KeyCode::Up, KeyModifiers::NONE), 1..4),
                (Typed::Paste("hi\r".into()), 4..19),
                (typed(KeyCode::Down, KeyModifiers::NONE), 19..22),
                (typed(KeyCode::Char('z'), KeyModifiers::NONE), 22..23),
            ]
        );
    }
}
