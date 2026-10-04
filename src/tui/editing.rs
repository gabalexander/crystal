//! The editing every text box shares, the one-line box and the box of
//! several lines alike: the text and the cursor, where each motion takes
//! the cursor (a character, a word, the ends of the line it's on, the ends
//! of the text), deleting from the cursor to where a motion goes, and the
//! keys for each, as shells and terminals have them. The keys are looked
//! up apart from what they do, so another set of keys, like a vim mode's,
//! can drive the same motions and deletes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Where the cursor goes from where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// A character back, or on.
    Left,
    Right,
    /// To the start of the word before the cursor, or the end of the word
    /// after it: readline's `backward-word` and `forward-word`.
    WordLeft,
    WordRight,
    /// To the start, or the end, of the line the cursor is on.
    LineStart,
    LineEnd,
    /// To the start, or the end, of the whole text.
    Start,
    End,
}

/// What a key asks of a text box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Puts the character in at the cursor.
    Insert(char),
    /// Moves the cursor.
    Move(Motion),
    /// Deletes what's between the cursor and where the motion goes.
    Delete(Motion),
}

/// The edit `key` asks for, or `None` for a key that doesn't edit, which
/// is the box's owner's. The keys are a shell's: Backspace and Delete,
/// `Ctrl+W` and `Alt+Backspace` (or `Ctrl+Backspace`) the word before the
/// cursor, `Alt+D` (or `Ctrl+Delete`) the word after it, `Ctrl+U` back to
/// the start of the line and `Ctrl+K` on to its end; the arrows, `Alt+B`
/// and `Alt+F` or `Ctrl` and `Alt` with the arrows by a word, which is
/// what macOS's terminals send for `Option+←` and `Option+→`; `Ctrl+A` and
/// `Ctrl+E` or Home and End to the line's ends, and `Ctrl+Home` and
/// `Ctrl+End` to the text's. A character typed with `Ctrl` or `Alt` that
/// isn't one of those types nothing.
pub fn edit_for(key: &KeyEvent) -> Option<Edit> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let edit = match key.code {
        KeyCode::Left if ctrl || alt => Edit::Move(Motion::WordLeft),
        KeyCode::Right if ctrl || alt => Edit::Move(Motion::WordRight),
        KeyCode::Left => Edit::Move(Motion::Left),
        KeyCode::Right => Edit::Move(Motion::Right),
        KeyCode::Home if ctrl => Edit::Move(Motion::Start),
        KeyCode::End if ctrl => Edit::Move(Motion::End),
        KeyCode::Home => Edit::Move(Motion::LineStart),
        KeyCode::End => Edit::Move(Motion::LineEnd),
        KeyCode::Backspace if ctrl || alt => Edit::Delete(Motion::WordLeft),
        KeyCode::Backspace => Edit::Delete(Motion::Left),
        KeyCode::Delete if ctrl || alt => Edit::Delete(Motion::WordRight),
        KeyCode::Delete => Edit::Delete(Motion::Right),
        KeyCode::Char(_) if ctrl && alt => return None,
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            'a' => Edit::Move(Motion::LineStart),
            'e' => Edit::Move(Motion::LineEnd),
            'w' => Edit::Delete(Motion::WordLeft),
            'u' => Edit::Delete(Motion::LineStart),
            'k' => Edit::Delete(Motion::LineEnd),
            _ => return None,
        },
        KeyCode::Char(c) if alt => match c.to_ascii_lowercase() {
            'b' => Edit::Move(Motion::WordLeft),
            'f' => Edit::Move(Motion::WordRight),
            'd' => Edit::Delete(Motion::WordRight),
            _ => return None,
        },
        KeyCode::Char(c) => Edit::Insert(c),
        _ => return None,
    };
    Some(edit)
}

/// Whether `c` is part of a word: letters and digits, as readline has it,
/// so whitespace and punctuation (`/`, `-`, `.`, `_`) split words.
fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Text being edited, and the cursor in it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Editor {
    text: String,
    /// The cursor's place, counted in characters from the start. It sits
    /// before the character at that place, or at the end.
    cursor: usize,
}

impl Editor {
    /// Text to edit, with the cursor at its end.
    pub fn with_text(text: &str) -> Editor {
        Editor {
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

    /// Puts the cursor at `place`, or the end when that's past it.
    pub fn set_cursor(&mut self, place: usize) {
        self.cursor = place.min(self.len());
    }

    /// Puts `text` in place of what's there, with the cursor at its end.
    pub fn set_text(&mut self, text: &str) {
        *self = Editor::with_text(text);
    }

    /// Puts `text` in at the cursor, and the cursor after it.
    pub fn insert_str(&mut self, text: &str) {
        let at = self.byte_index(self.cursor);
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    /// Carries out `edit`. Returns whether the text changed.
    pub fn apply(&mut self, edit: Edit) -> bool {
        match edit {
            Edit::Insert(c) => {
                self.insert_str(c.encode_utf8(&mut [0; 4]));
                true
            }
            Edit::Move(motion) => {
                self.cursor = self.place(motion);
                false
            }
            Edit::Delete(motion) => self.delete(motion),
        }
    }

    /// Deletes what's between the cursor and where `motion` goes, which
    /// leaves the cursor where the deleting started. At the end of a line,
    /// deleting to its end takes the line break, as readline does, and at
    /// its start, deleting to its start takes the one before it, so `Ctrl+K`
    /// or `Ctrl+U` again goes on into the next line. Returns whether there
    /// was anything to delete.
    pub fn delete(&mut self, motion: Motion) -> bool {
        let to = match (self.place(motion), motion) {
            (to, Motion::LineEnd) if to == self.cursor => self.place(Motion::Right),
            (to, Motion::LineStart) if to == self.cursor => self.place(Motion::Left),
            (to, _) => to,
        };
        let (from, to) = (self.cursor.min(to), self.cursor.max(to));
        if from == to {
            return false;
        }
        let bytes = self.byte_index(from)..self.byte_index(to);
        self.text.replace_range(bytes, "");
        self.cursor = from;
        true
    }

    /// Where `motion` takes the cursor, in characters from the start.
    pub fn place(&self, motion: Motion) -> usize {
        match motion {
            Motion::Left => self.cursor.saturating_sub(1),
            Motion::Right => (self.cursor + 1).min(self.len()),
            Motion::WordLeft => self.word_left(),
            Motion::WordRight => self.word_right(),
            Motion::LineStart => self.start_of_line_at(self.cursor),
            Motion::LineEnd => self.end_of_line_at(self.cursor),
            Motion::Start => 0,
            Motion::End => self.len(),
        }
    }

    /// Where the line holding the character at `at` starts: just after the
    /// line break before it, or the start of the text.
    pub fn start_of_line_at(&self, at: usize) -> usize {
        let before: Vec<char> = self.text.chars().take(at).collect();
        before
            .iter()
            .rposition(|&c| c == '\n')
            .map_or(0, |newline| newline + 1)
    }

    /// Where the line holding the character at `at` ends: at the line
    /// break after it, or the end of the text.
    pub fn end_of_line_at(&self, at: usize) -> usize {
        let after = self.text.chars().skip(at).position(|c| c == '\n');
        after.map_or(self.len(), |newline| at + newline)
    }

    /// The length in characters, which is what the cursor counts in.
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The start of the word before the cursor: back over what isn't a
    /// word, then over the word.
    fn word_left(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut at = self.cursor;
        while at > 0 && !is_word(chars[at - 1]) {
            at -= 1;
        }
        while at > 0 && is_word(chars[at - 1]) {
            at -= 1;
        }
        at
    }

    /// The end of the word after the cursor: on over what isn't a word,
    /// then over the word.
    fn word_right(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut at = self.cursor;
        while at < chars.len() && !is_word(chars[at]) {
            at += 1;
        }
        while at < chars.len() && is_word(chars[at]) {
            at += 1;
        }
        at
    }

    /// Where the character at `place` starts, in bytes. A Rust string is
    /// UTF-8, where one character can take several bytes, so the two
    /// counts differ as soon as there's an `é` in the text.
    fn byte_index(&self, place: usize) -> usize {
        self.text
            .char_indices()
            .nth(place)
            .map_or(self.text.len(), |(index, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn ctrl(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// `text` with the cursor where the `|` in it is.
    fn at(text: &str) -> Editor {
        let cursor = text.find('|').expect("a | for the cursor");
        let mut editor = Editor::with_text(&text.replace('|', ""));
        editor.set_cursor(text[..cursor].chars().count());
        editor
    }

    /// The text, with a `|` where the cursor is.
    fn shown(editor: &Editor) -> String {
        let mut text = editor.text().to_string();
        let at = editor.byte_index(editor.cursor());
        text.insert(at, '|');
        text
    }

    fn after(text: &str, keys: &[KeyEvent]) -> String {
        let mut editor = at(text);
        for key in keys {
            if let Some(edit) = edit_for(key) {
                editor.apply(edit);
            }
        }
        shown(&editor)
    }

    #[test]
    fn words_split_on_whitespace_and_punctuation() {
        let back = key(KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(
            after("fix src/tui/text_area.rs|", &[back]),
            "fix src/tui/text_area.|rs"
        );
        assert_eq!(
            after("fix src/tui/text_area|", &[back, back]),
            "fix src/tui/|text_area"
        );
        assert_eq!(after("fix the  |bug", &[alt('b')]), "fix |the  bug");
        assert_eq!(
            after("|  fix-the bug", &[alt('f'), alt('f')]),
            "  fix-the| bug"
        );
        assert_eq!(after("ab|", &[back, back]), "|ab");
        assert_eq!(
            after("|ab", &[key(KeyCode::Right, KeyModifiers::ALT)]),
            "ab|"
        );
    }

    #[test]
    fn ctrl_w_and_alt_backspace_delete_the_word_before_the_cursor() {
        assert_eq!(after("fix the bug|", &[ctrl('w')]), "fix the |");
        assert_eq!(after("fix the bug  |", &[ctrl('w')]), "fix the |");
        assert_eq!(after("fix the| bug", &[ctrl('w'), ctrl('w')]), "| bug");
        let alt_backspace = key(KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(after("cd ~/code/crystal|", &[alt_backspace]), "cd ~/code/|");
        let ctrl_backspace = key(KeyCode::Backspace, KeyModifiers::CONTROL);
        assert_eq!(after("one two|", &[ctrl_backspace]), "one |");
        assert_eq!(after("|one", &[ctrl('w')]), "|one");
    }

    #[test]
    fn alt_d_deletes_the_word_after_the_cursor() {
        assert_eq!(after("fix |the bug", &[alt('d')]), "fix | bug");
        assert_eq!(after("fix| the bug", &[alt('d'), alt('d')]), "fix|");
        let ctrl_delete = key(KeyCode::Delete, KeyModifiers::CONTROL);
        assert_eq!(after("|a.b", &[ctrl_delete]), "|.b");
    }

    #[test]
    fn ctrl_a_ctrl_e_home_and_end_go_to_the_line_s_ends() {
        let home = key(KeyCode::Home, KeyModifiers::NONE);
        let end = key(KeyCode::End, KeyModifiers::NONE);
        assert_eq!(after("fix the| bug", &[ctrl('a')]), "|fix the bug");
        assert_eq!(after("fix the| bug", &[ctrl('e')]), "fix the bug|");
        assert_eq!(after("one\ntw|o\nthree", &[home]), "one\n|two\nthree");
        assert_eq!(after("one\ntw|o\nthree", &[end]), "one\ntwo|\nthree");
        let start = key(KeyCode::Home, KeyModifiers::CONTROL);
        let finish = key(KeyCode::End, KeyModifiers::CONTROL);
        assert_eq!(after("one\ntw|o\nthree", &[start]), "|one\ntwo\nthree");
        assert_eq!(after("one\ntw|o\nthree", &[finish]), "one\ntwo\nthree|");
    }

    #[test]
    fn ctrl_k_and_ctrl_u_delete_to_the_line_s_ends_then_go_on_past_them() {
        assert_eq!(after("fix |the bug", &[ctrl('k')]), "fix |");
        assert_eq!(after("fix |the bug", &[ctrl('u')]), "|the bug");
        assert_eq!(after("one|\ntwo", &[ctrl('k')]), "one|two");
        assert_eq!(after("one\ntwo|", &[ctrl('u'), ctrl('u')]), "one|");
        assert_eq!(after("one\ntwo|", &[ctrl('u'), ctrl('u'), ctrl('u')]), "|");
        assert_eq!(after("|", &[ctrl('u'), ctrl('k')]), "|");
    }

    #[test]
    fn letters_type_but_ctrl_and_alt_with_other_letters_do_nothing() {
        let shifted = key(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(
            after("|", &[key(KeyCode::Char('a'), KeyModifiers::NONE), shifted]),
            "aA|"
        );
        assert_eq!(after("x|", &[ctrl('c'), alt('x'), ctrl('j')]), "x|");
        let ctrl_alt_w = key(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(after("one two|", &[ctrl_alt_w]), "one two|");
        // Up and Down are for the box's owner.
        assert_eq!(edit_for(&key(KeyCode::Up, KeyModifiers::NONE)), None);
        assert_eq!(edit_for(&key(KeyCode::Enter, KeyModifiers::ALT)), None);
    }

    #[test]
    fn characters_wider_than_a_byte_are_edited_whole() {
        assert_eq!(after("café crème|", &[ctrl('w')]), "café |");
        assert_eq!(after("|café crème", &[alt('d')]), "| crème");
        let left = key(KeyCode::Left, KeyModifiers::NONE);
        let backspace = key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(after("naïve|", &[left, left, backspace]), "na|ve");
    }

    #[test]
    fn deleting_says_whether_the_text_changed() {
        let mut editor = at("|one");
        assert!(!editor.apply(Edit::Delete(Motion::WordLeft)));
        assert!(!editor.apply(Edit::Move(Motion::End)));
        assert!(editor.apply(Edit::Delete(Motion::WordLeft)));
        assert!(editor.apply(Edit::Insert('x')));
        assert_eq!(shown(&editor), "x|");
    }
}
