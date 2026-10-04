//! A one-line text box: the text typed so far, and where the cursor is.
//! Its editing is [`editing`]'s, which the box of several lines shares.

use super::editing::{self, Editor};
use crossterm::event::KeyEvent;

#[derive(Debug, Default)]
pub struct TextInput {
    editor: Editor,
}

impl TextInput {
    /// A box that starts out holding `text`, with the cursor at its end.
    pub fn with_text(text: &str) -> TextInput {
        TextInput {
            editor: Editor::with_text(text),
        }
    }

    pub fn text(&self) -> &str {
        self.editor.text()
    }

    /// The cursor's place, counted in characters from the start.
    pub fn cursor(&self) -> usize {
        self.editor.cursor()
    }

    /// Edits the text for `key` the way a shell's line does: a character
    /// goes in at the cursor, and the rest are [`editing::edit_for`]'s
    /// keys, Backspace, `Ctrl+W` and the arrows among them. Other keys do
    /// nothing.
    pub fn on_key(&mut self, key: &KeyEvent) {
        if let Some(edit) = editing::edit_for(key) {
            self.editor.apply(edit);
        }
    }

    /// Puts `text` in at the cursor, the way a paste does. The box holds
    /// one line, so the text's line breaks become spaces.
    pub fn insert_str(&mut self, text: &str) {
        let line = text
            .trim_end_matches(['\r', '\n'])
            .replace(['\r', '\n'], " ");
        self.editor.insert_str(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn press(input: &mut TextInput, code: KeyCode) {
        input.on_key(&KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn typed(text: &str) -> TextInput {
        let mut input = TextInput::default();
        for c in text.chars() {
            press(&mut input, KeyCode::Char(c));
        }
        input
    }

    #[test]
    fn typing_adds_at_the_cursor() {
        let mut input = typed("feat");
        assert_eq!((input.text(), input.cursor()), ("feat", 4));
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Char('/'));
        assert_eq!((input.text(), input.cursor()), ("/feat", 1));
    }

    #[test]
    fn backspace_and_delete_take_out_either_side() {
        let mut input = typed("abc");
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text(), input.cursor()), ("ac", 1));
        press(&mut input, KeyCode::Delete);
        assert_eq!((input.text(), input.cursor()), ("a", 1));
        press(&mut input, KeyCode::Delete);
        assert_eq!(input.text(), "a");
    }

    #[test]
    fn the_cursor_stays_inside_the_text() {
        let mut input = typed("ab");
        press(&mut input, KeyCode::Right);
        assert_eq!(input.cursor(), 2);
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text(), input.cursor()), ("ab", 0));
        press(&mut input, KeyCode::End);
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn characters_wider_than_a_byte_are_edited_whole() {
        let mut input = typed("café");
        press(&mut input, KeyCode::Backspace);
        press(&mut input, KeyCode::Char('e'));
        assert_eq!(input.text(), "cafe");
    }

    #[test]
    fn a_box_can_start_out_holding_text() {
        let mut input = TextInput::with_text("claude");
        assert_eq!((input.text(), input.cursor()), ("claude", 6));
        press(&mut input, KeyCode::Char('!'));
        assert_eq!(input.text(), "claude!");
    }

    #[test]
    fn ctrl_u_clears_everything_before_the_cursor() {
        let mut input = typed("claude fix");
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Left);
        input.on_key(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!((input.text(), input.cursor()), ("fix", 0));
    }

    #[test]
    fn words_are_deleted_and_crossed_as_in_a_shell() {
        let mut input = typed("feat/text-boxes");
        input.on_key(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!((input.text(), input.cursor()), ("feat/text-", 10));
        input.on_key(&KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
        input.on_key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!((input.text(), input.cursor()), ("feat/", 5));
        input.on_key(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        input.on_key(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
        assert_eq!((input.text(), input.cursor()), ("/", 0));
    }

    #[test]
    fn a_paste_is_one_line() {
        let mut input = typed("a");
        input.insert_str("b\nc\n");
        assert_eq!((input.text(), input.cursor()), ("ab c", 4));
    }

    #[test]
    fn ctrl_keys_type_nothing() {
        let mut input = typed("x");
        input.on_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(input.text(), "x");
    }
}
