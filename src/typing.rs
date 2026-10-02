//! Typing into a session the way a person would, for `crystal send`.
//!
//! A program can only tell typing from pasting by how fast the keys come.
//! Agents like Claude Code take a burst of text that ends in Enter for a
//! paste, and put the Enter into their prompt instead of acting on it. So
//! the Enter goes on its own, a moment after the text. A program that asks
//! for bracketed paste gets the text marked as a paste, so it takes the
//! text as one piece, newlines and all.

use std::time::Duration;

/// How long to wait between the text and the Enter, so that the Enter
/// isn't taken as part of the text.
pub const ENTER_PAUSE: Duration = Duration::from_millis(150);

pub const ENTER: &[u8] = b"\r";

/// What a terminal sends before and after pasted text, once the program
/// has asked for bracketed paste.
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// The bytes that type `text`: marked as a paste when the program has
/// asked for that.
pub fn keystrokes(text: &str, bracketed_paste: bool) -> Vec<u8> {
    if !bracketed_paste {
        return text.as_bytes().to_vec();
    }
    // An end marker inside the text would end the paste early, and the
    // rest would be taken as keys typed.
    let text = text.replace(PASTE_END, "");
    format!("{PASTE_START}{text}{PASTE_END}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_typed_as_it_is() {
        assert_eq!(keystrokes("review the diff", false), b"review the diff");
    }

    #[test]
    fn a_program_that_asks_gets_the_text_marked_as_a_paste() {
        assert_eq!(
            keystrokes("line one\nline two", true),
            b"\x1b[200~line one\nline two\x1b[201~"
        );
    }

    #[test]
    fn the_text_cannot_end_its_own_paste() {
        assert_eq!(keystrokes("a\x1b[201~b", true), b"\x1b[200~ab\x1b[201~");
    }
}
