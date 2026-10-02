//! A one-line text box: the text typed so far, and where the cursor is.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default)]
pub struct TextInput {
    text: String,
    /// The cursor's place, counted in characters from the start. It sits
    /// before the character at that place, or at the end.
    cursor: usize,
}

impl TextInput {
    /// A box that starts out holding `text`, with the cursor at its end.
    pub fn with_text(text: &str) -> TextInput {
        TextInput {
            text: text.to_string(),
            cursor: text.chars().count(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Edits the text for `key`: a character goes in at the cursor,
    /// Backspace and Delete take out the character before or after it,
    /// Ctrl+U everything before it, as in a shell, and the arrows, Home and
    /// End move it. Other keys do nothing.
    pub fn on_key(&mut self, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if ctrl => self.clear_before_cursor(),
            KeyCode::Char(c) if !ctrl => self.insert(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.len(),
            _ => {}
        }
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_index(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        let at = self.byte_index(self.cursor);
        self.text.remove(at);
    }

    fn clear_before_cursor(&mut self) {
        let at = self.byte_index(self.cursor);
        self.text.replace_range(..at, "");
        self.cursor = 0;
    }

    fn delete(&mut self) {
        if self.cursor < self.len() {
            let at = self.byte_index(self.cursor);
            self.text.remove(at);
        }
    }

    /// The length in characters, which is what the cursor counts in.
    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Where the character at `place` starts, in bytes. A Rust string is
    /// UTF-8, where one character can take several bytes, so the two
    /// counts differ as soon as there's an `é` in the text.
    fn byte_index(&self, place: usize) -> usize {
        match self.text.char_indices().nth(place) {
            Some((index, _)) => index,
            None => self.text.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ctrl_keys_type_nothing() {
        let mut input = typed("x");
        input.on_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(input.text(), "x");
    }
}
