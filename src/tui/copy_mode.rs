//! Copy mode: vi's keys move a cursor over a pane's screen and back
//! through its history, select from it, and search it as the search is
//! typed, and what's selected goes to the clipboard; `o` opens the link
//! under the cursor, a URL or a file's path. The program in the pane goes on running, and its
//! output on showing. The TUI's panes and `crystal attach` both have it.
//! Kept apart from I/O: the keys work on the screen, and say what's to be
//! copied.

use super::text_input::TextInput;
use crate::links::Target;
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

#[derive(Debug)]
pub struct SearchPrompt {
    input: TextInput,
    /// `/` searches down, `?` up.
    forward: bool,
    /// Where copy mode was as the search began: each key searches from
    /// there, and `Esc` goes back.
    from: vt::Spot,
    /// The match the cursor is on, for what's typed so far.
    found: Option<vt::Found>,
}

impl SearchPrompt {
    /// What leads the search on the line it's typed on.
    pub fn label(&self) -> &'static str {
        if self.forward {
            " search down: "
        } else {
            " search up: "
        }
    }

    pub fn text(&self) -> &str {
        self.input.text()
    }

    /// The cursor's place in the text, counted in characters.
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }

    /// Whether what's typed so far matches anything.
    pub fn matches(&self) -> bool {
        self.found.is_some()
    }

    /// What what's typed so far found: which match the cursor is on, `3 of
    /// 12`, or that nothing matches. Nothing before anything's typed.
    pub fn count(&self) -> Option<String> {
        if self.input.text().is_empty() {
            return None;
        }
        Some(self.found.map_or_else(|| NO_MATCH.to_string(), place))
    }

    /// Searches for what's typed, from where the search began.
    fn search(&mut self, screen: &mut vt::Screen) {
        self.found = screen.search_from(&self.from, self.input.text(), self.forward);
    }
}

/// What a search says when nothing matches it.
const NO_MATCH: &str = "no match";

/// What a key in copy mode asks for, beyond what it did to the screen.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    /// Copy mode is over.
    Leave,
    /// Put this on the clipboard; copy mode is over.
    Copy(String),
    /// Open this link; copy mode is over.
    Open(Target),
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
            KeyCode::Char('o') => {
                let link = screen.copy_cursor().and_then(|cell| screen.link_at(cell));
                return match link {
                    Some(link) => Outcome::Open(link.target),
                    None => Outcome::Say("there's no link under the cursor".into()),
                };
            }
            KeyCode::Char('/') => self.open_prompt(screen, true),
            KeyCode::Char('?') => self.open_prompt(screen, false),
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
    /// typed, if there is one, which it searches for.
    pub fn on_paste(&mut self, screen: &mut vt::Screen, text: &str) {
        if let Some(prompt) = &mut self.prompt {
            prompt.input.insert_str(text);
            prompt.search(screen);
        }
    }

    fn open_prompt(&mut self, screen: &vt::Screen, forward: bool) {
        self.prompt = Some(SearchPrompt {
            input: TextInput::default(),
            forward,
            from: screen.spot(),
            found: None,
        });
    }

    /// Keys while a search is being typed: each one that changes it
    /// searches again from where it began, the cursor going to the nearest
    /// match; Enter keeps it, and Esc (or Ctrl+C) goes back to where it
    /// began, and to the search before.
    fn on_prompt_key(&mut self, screen: &mut vt::Screen, key: KeyEvent) -> Outcome {
        let Some(prompt) = &mut self.prompt else {
            return Outcome::Stay;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let cancel = key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c'));
        match key.code {
            _ if cancel => {
                screen.go_back(&prompt.from);
                self.prompt = None;
            }
            KeyCode::Enter => {
                let Some(SearchPrompt {
                    input,
                    forward,
                    found,
                    ..
                }) = self.prompt.take()
                else {
                    return Outcome::Stay;
                };
                if input.text().is_empty() {
                    return Outcome::Stay;
                }
                self.forward = forward;
                return said(found, input.text());
            }
            _ => {
                let before = prompt.input.text().to_string();
                prompt.input.on_key(&key);
                if prompt.input.text() != before {
                    prompt.search(screen);
                }
            }
        }
        Outcome::Stay
    }

    fn search_again(&mut self, screen: &mut vt::Screen, forward: bool) -> Outcome {
        if !screen.searched() {
            return Outcome::Say("no search yet: / searches down, ? up".into());
        }
        match screen.search_again(forward) {
            Some(found) => Outcome::Say(place(found)),
            None => Outcome::Say(NO_MATCH.into()),
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
            ("o", "open link"),
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
    fn a_search_goes_to_the_nearest_match_as_it_is_typed() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        press(&mut copy, &mut screen, "?line 4");
        let prompt = copy.prompt.as_ref().unwrap();
        assert_eq!(screen.copy_cursor_line(), "line 4 of ten");
        assert_eq!(prompt.count().as_deref(), Some("1 of 1"));
        // A letter taken back searches again from where it began: up from
        // the bottom, line 9 is nearest.
        copy.on_key(
            &mut screen,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(screen.copy_cursor_line(), "line 9 of ten");
        let prompt = copy.prompt.as_ref().unwrap();
        assert_eq!(prompt.count().as_deref(), Some("10 of 10"));

        press(&mut copy, &mut screen, "x");
        assert_eq!(screen.copy_cursor_line(), "", "back where it began");
        let prompt = copy.prompt.as_ref().unwrap();
        assert_eq!(prompt.count().as_deref(), Some("no match"));
    }

    #[test]
    fn enter_keeps_the_match_the_search_went_to() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let outcome = press(&mut copy, &mut screen, "?line 4\r");
        assert_eq!(outcome, Outcome::Say("line 4: 1 of 1".into()));
        assert!(copy.prompt.is_none());
        assert_eq!(screen.copy_cursor_line(), "line 4 of ten");
        assert!(screen.searched());
    }

    #[test]
    fn esc_in_a_search_being_typed_goes_back_to_where_it_began() {
        let (mut copy, mut screen) = (CopyMode::default(), screen());
        let outcome = press(&mut copy, &mut screen, "/of\x1b");
        assert_eq!(outcome, Outcome::Stay);
        assert!(copy.prompt.is_none());
        assert!(!screen.searched());

        // With a search before, the cursor goes back, and n goes on with
        // the search before.
        press(&mut copy, &mut screen, "?ten\rk");
        assert_eq!(screen.copy_cursor_line(), "line 8 of ten");
        press(&mut copy, &mut screen, "?line 2");
        assert_eq!(screen.copy_cursor_line(), "line 2 of ten");
        assert_ne!(screen.scrolled_back(), 0);
        press(&mut copy, &mut screen, "\x1b");
        assert_eq!(screen.copy_cursor_line(), "line 8 of ten");
        assert_eq!(screen.scrolled_back(), 0);
        assert_eq!(
            press(&mut copy, &mut screen, "n"),
            Outcome::Say("8 of 10".into())
        );

        // Ctrl+C goes back too.
        press(&mut copy, &mut screen, "/line 0");
        assert_eq!(copy.on_key(&mut screen, ctrl('c')), Outcome::Stay);
        assert!(copy.prompt.is_none());
        assert_eq!(screen.copy_cursor_line(), "line 7 of ten");
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
    fn o_opens_the_link_under_the_cursor() {
        let mut screen = vt::Screen::new(3, 40);
        screen.process(b"docs at https://example.com/docs\r\n");
        screen.start_copying();
        let mut copy = CopyMode::default();
        assert_eq!(
            press(&mut copy, &mut screen, "o"),
            Outcome::Say("there's no link under the cursor".into())
        );
        let outcome = press(&mut copy, &mut screen, "kWWo");
        let url = Target::Url("https://example.com/docs".into());
        assert_eq!(outcome, Outcome::Open(url));
    }

    #[test]
    fn o_opens_the_path_under_the_cursor_with_its_line() {
        let mut screen = vt::Screen::new(3, 40);
        screen.process(b"see src/app.rs:42 now\r\n");
        screen.start_copying();
        let mut copy = CopyMode::default();
        let file = Target::File {
            path: "src/app.rs".into(),
            line: Some(42),
        };
        assert_eq!(press(&mut copy, &mut screen, "kWo"), Outcome::Open(file));
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
        copy.on_paste(&mut screen, "ignored");
        assert!(copy.prompt.is_none());
        press(&mut copy, &mut screen, "/");
        copy.on_paste(&mut screen, "line 4\n");
        assert_eq!(screen.copy_cursor_line(), "line 4 of ten");
        let outcome = press(&mut copy, &mut screen, "\r");
        assert_eq!(outcome, Outcome::Say("line 4: 1 of 1".into()));
    }
}
