//! Copy mode: vi's keys move a cursor over a pane's screen and back
//! through its history, select from it, and search it, and what's selected
//! goes to the clipboard. The program in the pane goes on running, and its
//! output on showing. Kept apart from I/O: the keys work on the pane's
//! screen, and say what's to be copied.

use super::text_input::TextInput;
use crate::vt::{self, Motion, SelectionKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default)]
pub struct CopyMode {
    /// The search being typed on the footer, after `/` or `?`.
    pub prompt: Option<SearchPrompt>,
    /// Which way the last search went, for `n` to go on that way.
    forward: bool,
    /// A `g`, waiting for the second `g` of `gg`.
    g: bool,
}

#[derive(Debug, Default)]
pub struct SearchPrompt {
    pub input: TextInput,
    /// `/` searches down, `?` up.
    pub forward: bool,
}

/// What a key in copy mode asks for, beyond what it did to the screen.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    /// Copy mode is over.
    Leave,
    /// Put this on the clipboard; copy mode is over.
    Copy(String),
    /// Tell the user this.
    Say(String),
}

impl CopyMode {
    pub fn on_key(&mut self, screen: &mut vt::Screen, key: KeyEvent) -> Outcome {
        if self.prompt.is_some() {
            return self.on_prompt_key(screen, key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let g = std::mem::take(&mut self.g);
        let (rows, _) = screen.size();
        let page = i32::from(rows.saturating_sub(1).max(1));
        let half = i32::from((rows / 2).max(1));
        match key.code {
            KeyCode::Char('c') if ctrl => return Outcome::Leave,
            KeyCode::Char('u') if ctrl => screen.page_copy_cursor(half),
            KeyCode::Char('d') if ctrl => screen.page_copy_cursor(-half),
            KeyCode::Char('b') if ctrl => screen.page_copy_cursor(page),
            KeyCode::Char('f') if ctrl => screen.page_copy_cursor(-page),
            KeyCode::Char('y') if ctrl => screen.scroll_back(1),
            KeyCode::Char('e') if ctrl => screen.scroll_back(-1),
            KeyCode::Char('v') if ctrl => screen.toggle_selection(SelectionKind::Block),
            _ if ctrl => {}
            KeyCode::PageUp => screen.page_copy_cursor(page),
            KeyCode::PageDown => screen.page_copy_cursor(-page),
            KeyCode::Char('g') if g => screen.move_copy_cursor(Motion::HistoryTop),
            KeyCode::Char('g') => self.g = true,
            KeyCode::Char('v' | ' ') => screen.toggle_selection(SelectionKind::Chars),
            KeyCode::Char('V') => screen.toggle_selection(SelectionKind::Lines),
            KeyCode::Char('y') | KeyCode::Enter => {
                return match screen.selected_text() {
                    Some(text) => Outcome::Copy(text),
                    None => Outcome::Say("nothing is selected: v starts a selection".into()),
                };
            }
            KeyCode::Char('Y') => {
                let line = screen.copy_cursor_line();
                if line.is_empty() {
                    return Outcome::Say("this line is empty".into());
                }
                return Outcome::Copy(line);
            }
            KeyCode::Char('/') => self.open_prompt(true),
            KeyCode::Char('?') => self.open_prompt(false),
            KeyCode::Char('n') => return self.search_again(screen, self.forward),
            KeyCode::Char('N') => return self.search_again(screen, !self.forward),
            KeyCode::Char('q') => return Outcome::Leave,
            // Esc takes back what's marked first, a mark at a time.
            KeyCode::Esc if screen.selecting() => screen.clear_selection(),
            KeyCode::Esc if screen.searched() => screen.clear_search(),
            KeyCode::Esc => return Outcome::Leave,
            code => {
                if let Some(motion) = motion(code) {
                    screen.move_copy_cursor(motion);
                }
            }
        }
        Outcome::Stay
    }

    /// Text pasted while copy mode has the keyboard: into the search being
    /// typed, if there is one.
    pub fn on_paste(&mut self, text: &str) {
        if let Some(prompt) = &mut self.prompt {
            prompt.input.insert_str(text);
        }
    }

    fn open_prompt(&mut self, forward: bool) {
        self.prompt = Some(SearchPrompt {
            input: TextInput::default(),
            forward,
        });
    }

    /// Keys while a search is being typed: Enter searches, Esc doesn't, and
    /// the rest edit it.
    fn on_prompt_key(&mut self, screen: &mut vt::Screen, key: KeyEvent) -> Outcome {
        let Some(prompt) = &mut self.prompt else {
            return Outcome::Stay;
        };
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let SearchPrompt { input, forward } = self.prompt.take().unwrap_or_default();
                let text = input.text();
                if text.is_empty() {
                    return Outcome::Stay;
                }
                self.forward = forward;
                return said(screen.search(text, forward), text);
            }
            _ => prompt.input.on_key(&key),
        }
        Outcome::Stay
    }

    fn search_again(&mut self, screen: &mut vt::Screen, forward: bool) -> Outcome {
        if !screen.searched() {
            return Outcome::Say("no search yet: / searches down, ? up".into());
        }
        match screen.search_again(forward) {
            Some(found) => Outcome::Say(place(found)),
            None => Outcome::Say("no match".into()),
        }
    }
}

/// What to tell the user about a search for `text`.
fn said(found: Option<vt::Found>, text: &str) -> Outcome {
    match found {
        Some(found) => Outcome::Say(format!("{text}: {}", place(found))),
        None => Outcome::Say(format!("no match for {text}")),
    }
}

/// Which match the cursor is on: `3 of 12`, counted from the top.
fn place(found: vt::Found) -> String {
    format!("{} of {}", found.number, found.of)
}

/// The move a key makes, as vi has it.
fn motion(code: KeyCode) -> Option<Motion> {
    let motion = match code {
        KeyCode::Char('h') | KeyCode::Left => Motion::Left,
        KeyCode::Char('j') | KeyCode::Down => Motion::Down,
        KeyCode::Char('k') | KeyCode::Up => Motion::Up,
        KeyCode::Char('l') | KeyCode::Right => Motion::Right,
        KeyCode::Char('0') | KeyCode::Home => Motion::LineStart,
        KeyCode::Char('^') => Motion::LineText,
        KeyCode::Char('$') | KeyCode::End => Motion::LineEnd,
        KeyCode::Char('H') => Motion::ViewTop,
        KeyCode::Char('M') => Motion::ViewMiddle,
        KeyCode::Char('L') => Motion::ViewBottom,
        KeyCode::Char('w') => Motion::WordNext,
        KeyCode::Char('b') => Motion::WordBack,
        KeyCode::Char('e') => Motion::WordEnd,
        KeyCode::Char('W') => Motion::BigWordNext,
        KeyCode::Char('B') => Motion::BigWordBack,
        KeyCode::Char('E') => Motion::BigWordEnd,
        KeyCode::Char('{') => Motion::ParagraphBack,
        KeyCode::Char('}') => Motion::ParagraphNext,
        KeyCode::Char('%') => Motion::Bracket,
        KeyCode::Char('G') => Motion::HistoryBottom,
        _ => return None,
    };
    Some(motion)
}

/// The keys the footer offers in copy mode, most needed first.
pub fn hints(screen: &vt::Screen) -> &'static [(&'static str, &'static str)] {
    if screen.selecting() {
        &[
            ("y", "copy"),
            ("hjkl w b", "move"),
            ("V", "lines"),
            ("esc", "unselect"),
        ]
    } else {
        &[
            ("v", "select"),
            ("/ ?", "search"),
            ("n/N", "next"),
            ("Y", "copy line"),
            ("q", "leave"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4-row screen with numbered lines behind it, in copy mode, the
    /// cursor on the empty row below `line 9`.
    fn screen() -> vt::Screen {
        let mut screen = vt::Screen::new(4, 30);
        for line in 0..10 {
            screen.process(format!("line {line} of ten\r\n").as_bytes());
        }
        screen.start_copying();
        screen
    }

    fn press(copy: &mut CopyMode, screen: &mut vt::Screen, keys: &str) -> Outcome {
        let mut outcome = Outcome::Stay;
        for c in keys.chars() {
            let code = match c {
                '\r' => KeyCode::Enter,
                '\x1b' => KeyCode::Esc,
                c => KeyCode::Char(c),
            };
            outcome = copy.on_key(screen, KeyEvent::new(code, KeyModifiers::NONE));
        }
        outcome
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn v_selects_as_the_cursor_moves_and_y_copies_it() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let outcome = press(&mut copy, &mut screen, "kkwvjy");
        assert_eq!(outcome, Outcome::Copy("8 of ten\nline 9".into()));
    }

    #[test]
    fn y_with_nothing_selected_says_so_and_stays() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let Outcome::Say(said) = press(&mut copy, &mut screen, "y") else {
            panic!("y copied nothing, yet said nothing");
        };
        assert!(said.contains("v starts a selection"), "{said}");
    }

    #[test]
    fn shift_y_copies_the_line_the_cursor_is_on() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let outcome = press(&mut copy, &mut screen, "kkY");
        assert_eq!(outcome, Outcome::Copy("line 8 of ten".into()));
    }

    #[test]
    fn shift_y_on_an_empty_line_copies_nothing() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let outcome = press(&mut copy, &mut screen, "Y");
        assert_eq!(outcome, Outcome::Say("this line is empty".into()));
    }

    #[test]
    fn shift_v_selects_whole_lines() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let outcome = press(&mut copy, &mut screen, "kwVk\r");
        assert_eq!(
            outcome,
            Outcome::Copy("line 8 of ten\nline 9 of ten".into())
        );
    }

    #[test]
    fn gg_goes_to_the_top_of_the_history_and_g_alone_does_nothing_yet() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        press(&mut copy, &mut screen, "g");
        assert_eq!(screen.scrolled_back(), 0);
        press(&mut copy, &mut screen, "g");
        assert_eq!(screen.copy_cursor_line(), "line 0 of ten");
        press(&mut copy, &mut screen, "G");
        assert_eq!(screen.scrolled_back(), 0);
    }

    #[test]
    fn ctrl_u_and_ctrl_d_go_half_a_screen_back_and_forth() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        copy.on_key(&mut screen, ctrl('u'));
        assert_eq!(screen.scrolled_back(), 2);
        copy.on_key(&mut screen, ctrl('d'));
        assert_eq!(screen.scrolled_back(), 0);
    }

    #[test]
    fn a_search_is_typed_then_n_and_shift_n_go_on_and_back() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        press(&mut copy, &mut screen, "?of");
        assert_eq!(copy.prompt.as_ref().unwrap().input.text(), "of");
        let outcome = press(&mut copy, &mut screen, "\r");
        assert_eq!(outcome, Outcome::Say("of: 10 of 10".into()));
        assert!(copy.prompt.is_none());
        assert_eq!(
            press(&mut copy, &mut screen, "n"),
            Outcome::Say("9 of 10".into())
        );
        assert_eq!(
            press(&mut copy, &mut screen, "N"),
            Outcome::Say("10 of 10".into())
        );
    }

    #[test]
    fn a_search_with_no_match_says_so() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let outcome = press(&mut copy, &mut screen, "/eleven\r");
        assert_eq!(outcome, Outcome::Say("no match for eleven".into()));
        let Outcome::Say(said) = press(&mut CopyMode::default(), &mut screen, "n") else {
            panic!("n said nothing");
        };
        assert_eq!(said, "no match");
    }

    #[test]
    fn esc_in_a_search_being_typed_drops_only_the_search() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let outcome = press(&mut copy, &mut screen, "/of\x1b");
        assert_eq!(outcome, Outcome::Stay);
        assert!(copy.prompt.is_none());
        assert!(!screen.searched());
    }

    #[test]
    fn esc_unselects_then_drops_the_search_then_leaves() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        press(&mut copy, &mut screen, "?ten\rv");
        assert!(screen.selecting());
        assert_eq!(press(&mut copy, &mut screen, "\x1b"), Outcome::Stay);
        assert!(!screen.selecting());
        assert!(screen.searched());
        assert_eq!(press(&mut copy, &mut screen, "\x1b"), Outcome::Stay);
        assert!(!screen.searched());
        assert_eq!(press(&mut copy, &mut screen, "\x1b"), Outcome::Leave);
    }

    #[test]
    fn q_and_ctrl_c_leave() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        assert_eq!(press(&mut copy, &mut screen, "q"), Outcome::Leave);
        assert_eq!(copy.on_key(&mut screen, ctrl('c')), Outcome::Leave);
    }

    #[test]
    fn a_paste_goes_into_the_search_being_typed() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        copy.on_paste("ignored");
        assert!(copy.prompt.is_none());
        press(&mut copy, &mut screen, "/");
        copy.on_paste("line 4\n");
        let outcome = press(&mut copy, &mut screen, "\r");
        assert_eq!(outcome, Outcome::Say("line 4: 1 of 1".into()));
    }
}
