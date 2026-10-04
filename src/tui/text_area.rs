//! A text box of several lines: the task in the new-session panel, and the
//! reply box. The text breaks only where a newline was typed or pasted; to
//! fit the box, each of its lines is wrapped into rows when it's drawn, at a
//! space where there's one. Its editing is [`editing`]'s, which the one-line
//! box shares.

use super::editing::{self, Editor};
use crossterm::event::KeyEvent;

#[derive(Debug, Default)]
pub struct TextArea {
    editor: Editor,
}

/// One row of the box as drawn: the characters from `start` up to `end`,
/// counted from the start of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

impl TextArea {
    pub fn text(&self) -> &str {
        self.editor.text()
    }

    pub fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    /// Puts `text` in the box in place of what's there, with the cursor at
    /// its end.
    pub fn set_text(&mut self, text: &str) {
        self.editor.set_text(text);
    }

    /// Edits the text for `key` the way a shell's line does, on the line
    /// the cursor is on: a character goes in at the cursor, and the rest
    /// are [`editing::edit_for`]'s keys, `Ctrl+U` back to the start of the
    /// line, Home and End to its ends and `Ctrl+Home` and `Ctrl+End` to the
    /// text's among them. Up and Down are for the box's owner: see
    /// [`TextArea::line_up`]. Returns whether the text changed.
    pub fn on_key(&mut self, key: &KeyEvent) -> bool {
        match editing::edit_for(key) {
            Some(edit) => self.editor.apply(edit),
            None => false,
        }
    }

    /// Puts `text` in at the cursor, the way a paste does. A carriage
    /// return, which terminals send for a newline, is a newline here.
    pub fn insert_str(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.editor.insert_str(&text);
    }

    /// Breaks the line at the cursor.
    pub fn newline(&mut self) {
        self.insert_str("\n");
    }

    /// Whether the cursor is on the text's first line, where Up has
    /// nowhere further up to go.
    pub fn on_first_line(&self) -> bool {
        self.line_start() == 0
    }

    /// Whether the cursor is on the text's last line.
    pub fn on_last_line(&self) -> bool {
        self.line_end() == self.editor.len()
    }

    /// Moves the cursor to the line above, as near its column as that
    /// line allows.
    pub fn line_up(&mut self) {
        if self.on_first_line() {
            return;
        }
        let column = self.cursor() - self.line_start();
        let above_end = self.line_start() - 1;
        let above_start = self.editor.start_of_line_at(above_end);
        self.editor
            .set_cursor((above_start + column).min(above_end));
    }

    /// Moves the cursor to the line below, as near its column as that
    /// line allows.
    pub fn line_down(&mut self) {
        if self.on_last_line() {
            return;
        }
        let column = self.cursor() - self.line_start();
        let below_start = self.line_end() + 1;
        let below_end = self.editor.end_of_line_at(below_start);
        self.editor
            .set_cursor((below_start + column).min(below_end));
    }

    /// The rows the text takes in a box `width` characters wide: each
    /// line, wrapped after the last space that fits, or cut at the width
    /// where a word is longer than that.
    pub fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(1);
        let chars: Vec<char> = self.text().chars().collect();
        let mut rows = Vec::new();
        let mut line_start = 0;
        loop {
            let line_end = chars[line_start..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |at| line_start + at);
            let mut start = line_start;
            while line_end - start > width {
                let fits = start + width;
                // A space just past the edge ends the row: it's clipped
                // when drawn, and the next row starts with the next word.
                let end = if chars[fits] == ' ' {
                    fits + 1
                } else {
                    let after_space = (start + 1..=fits).rev().find(|&at| chars[at - 1] == ' ');
                    after_space.unwrap_or(fits)
                };
                rows.push(Row { start, end });
                start = end;
            }
            rows.push(Row {
                start,
                end: line_end,
            });
            if line_end == chars.len() {
                return rows;
            }
            line_start = line_end + 1;
        }
    }

    /// The row and column the cursor is at, in a box `width` wide.
    pub fn cursor_at(&self, width: usize) -> (usize, usize) {
        let rows = self.rows(width);
        for (index, row) in rows.iter().enumerate() {
            // At the very end of a row that wrapped, the cursor is at the
            // start of the next one, where what's typed will go.
            let last_of_its_line = rows.get(index + 1).is_none_or(|next| next.start != row.end);
            let cursor = self.cursor();
            let on_it = cursor >= row.start
                && (cursor < row.end || (cursor == row.end && last_of_its_line));
            if on_it {
                return (index, cursor - row.start);
            }
        }
        (rows.len() - 1, 0)
    }

    /// The characters of `row`, as a string to draw.
    pub fn row_text(&self, row: Row) -> String {
        self.text()
            .chars()
            .skip(row.start)
            .take(row.end - row.start)
            .collect()
    }

    fn cursor(&self) -> usize {
        self.editor.cursor()
    }

    /// Where the cursor's line starts, in characters.
    fn line_start(&self) -> usize {
        self.editor.start_of_line_at(self.cursor())
    }

    /// Where the cursor's line ends: the newline after it, or the end.
    fn line_end(&self) -> usize {
        self.editor.end_of_line_at(self.cursor())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn press(area: &mut TextArea, code: KeyCode) {
        area.on_key(&KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn with(text: &str) -> TextArea {
        let mut area = TextArea::default();
        area.set_text(text);
        area
    }

    fn row_texts(area: &TextArea, width: usize) -> Vec<String> {
        let rows = area.rows(width);
        rows.into_iter().map(|row| area.row_text(row)).collect()
    }

    #[test]
    fn lines_wrap_after_the_last_space_that_fits() {
        let area = with("fix the flaky refund test");
        assert_eq!(row_texts(&area, 12), ["fix the ", "flaky refund ", "test"]);
    }

    #[test]
    fn a_word_longer_than_the_box_is_cut() {
        let area = with("abcdefghij");
        assert_eq!(row_texts(&area, 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn newlines_start_rows_of_their_own() {
        let area = with("one\n\ntwo");
        assert_eq!(row_texts(&area, 20), ["one", "", "two"]);
        assert_eq!(row_texts(&TextArea::default(), 20), [""]);
    }

    #[test]
    fn the_cursor_is_found_on_its_row() {
        let area = with("one\ntwo");
        assert_eq!(area.cursor_at(20), (1, 3));
        let mut wrapped = with("abcdefgh");
        assert_eq!(wrapped.cursor_at(4), (1, 4));
        press(&mut wrapped, KeyCode::Home);
        assert_eq!(wrapped.cursor_at(4), (0, 0));
    }

    #[test]
    fn up_and_down_keep_to_the_column() {
        let mut area = with("a long line\nshort\nanother line");
        area.line_up();
        assert_eq!(area.cursor_at(40), (1, 5));
        area.line_up();
        assert_eq!(area.cursor_at(40), (0, 5));
        area.line_down();
        area.line_down();
        assert_eq!(area.cursor_at(40), (2, 5));
        assert!(area.on_last_line());
    }

    #[test]
    fn a_paste_keeps_its_lines() {
        let mut area = with("do ");
        area.insert_str("this\r\nthen that");
        assert_eq!(area.text(), "do this\nthen that");
        assert!(!area.on_first_line());
    }

    #[test]
    fn home_end_and_ctrl_u_keep_to_the_line() {
        let mut area = with("first\nsecond");
        press(&mut area, KeyCode::Home);
        assert_eq!(area.cursor_at(40), (1, 0));
        press(&mut area, KeyCode::End);
        area.on_key(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(area.text(), "first\n");
    }

    #[test]
    fn words_and_lines_are_edited_as_in_a_shell() {
        let mut area = with("fix the bug\nin the panel");
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert!(area.on_key(&ctrl('w')));
        assert_eq!(area.text(), "fix the bug\nin the ");
        area.on_key(&KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
        area.on_key(&ctrl('a'));
        assert_eq!(area.cursor_at(40), (1, 0));
        assert!(!area.on_key(&ctrl('a')), "moving changes no text");
        // Ctrl+U at the start of a line takes the line break before it.
        assert!(area.on_key(&ctrl('u')));
        assert_eq!(area.text(), "fix the bugin the ");
        area.on_key(&KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(area.cursor_at(40), (0, 0));
        area.on_key(&KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert!(area.on_last_line());
    }

    #[test]
    fn backspace_joins_lines() {
        let mut area = with("a\nb");
        press(&mut area, KeyCode::Left);
        press(&mut area, KeyCode::Backspace);
        assert_eq!(area.text(), "ab");
    }
}
