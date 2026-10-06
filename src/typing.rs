//! Typing into a session the way a person would, for `crystal send`.
//!
//! A program can only tell typing from pasting by how fast the keys come.
//! Agents like Claude Code take a burst of text that ends in Enter for a
//! paste, and put the Enter into their prompt instead of acting on it. So
//! the Enter goes on its own, a moment after the text. A program that asks
//! for bracketed paste gets the text marked as a paste, so it takes the
//! text as one piece, newlines and all.
//!
//! An agent at its prompt is watched as it's typed into, by a
//! [`Delivery`]: the Enter goes once its screen shows the text, and again
//! when it didn't take it, until it starts on it or gives up. And an agent
//! crystal stopped idle and just started again is typed into only once it
//! takes keys, as [`Waking`] tells: Claude Code says it has started a
//! moment before its prompt reads the terminal, and what's typed meanwhile
//! comes to it in one piece, the Enter lost in the paste.

use std::time::{Duration, Instant};

/// How long to wait between the text and the Enter, so that the Enter
/// isn't taken as part of the text: after the text shows, for an agent
/// watched as it's typed into.
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

/// What a session shows: its screen's rows, and where its cursor is, as a
/// program reading lines moves it down on Enter and changes no row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shown {
    pub rows: Vec<String>,
    pub cursor: Option<(u16, u16)>,
}

/// How long the screen of an agent just started again must hold still, at
/// its prompt and taking keys, before it's typed into: Claude Code draws
/// the conversation it picks up once it's up.
pub const SETTLE: Duration = Duration::from_millis(500);

/// How long an agent just started again, at its prompt, is given to take
/// keys as they come before it's typed into all the same: one that reads
/// its terminal a line at a time, as a script does, never will, and takes
/// a line whenever it's typed.
pub const LINES_GRACE: Duration = Duration::from_secs(10);

/// How a session started again looks, for [`Waking`].
#[derive(Debug, Clone)]
pub struct Look {
    /// Its agent sits at its prompt, as its hooks or its screen say; a
    /// terminal's shell always does.
    pub at_prompt: bool,
    /// Its terminal hands its program keys as they come, out of the line
    /// at a time a terminal starts with, as a program reading keys asks.
    pub takes_keys: bool,
    /// What its screen shows.
    pub screen: Shown,
}

/// Whether a session crystal stopped idle, just started again, is ready to
/// be typed into: its agent at its prompt, taking keys as they come, and
/// its screen held still for [`SETTLE`], or past [`LINES_GRACE`] still
/// reading lines.
#[derive(Debug)]
pub struct Waking {
    started: Instant,
    screen: Shown,
    still_since: Instant,
}

impl Waking {
    pub fn new(now: Instant) -> Waking {
        Waking {
            started: now,
            screen: Shown::default(),
            still_since: now,
        }
    }

    /// Takes in how the session looks at `now`, and says whether it's
    /// ready.
    pub fn ready(&mut self, look: Look, now: Instant) -> bool {
        if look.screen != self.screen {
            self.screen = look.screen;
            self.still_since = now;
        }
        let still = now.saturating_duration_since(self.still_since) >= SETTLE;
        let reads_lines = now.saturating_duration_since(self.started) >= LINES_GRACE;
        look.at_prompt && still && (look.takes_keys || reads_lines)
    }
}

/// How long an agent's screen is given to show the text typed into it
/// before Enter is pressed all the same: one may not echo it.
pub const LANDING: Duration = Duration::from_secs(1);

/// How long an agent is given after Enter to start on what it was sent,
/// or to change its screen, before Enter is pressed again.
pub const TAKE: Duration = Duration::from_secs(2);

/// How many times Enter is pressed in all before what was sent is taken
/// to have stalled.
pub const ENTERS: u32 = 3;

/// What to do next as an agent at its prompt is typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Look again in a moment.
    Wait,
    /// Press Enter: the text has shown, or the last Enter wasn't taken.
    Enter,
    /// It has taken it: it started on it, or its screen changed after
    /// Enter.
    Taken,
    /// Enter was pressed [`ENTERS`] times, and it neither started on it
    /// nor changed its screen: it's still in its input, most likely.
    Stalled,
}

/// Typing into an agent at its prompt, watched until it has taken what it
/// was sent: the Enter goes once the text shows and has held still for
/// [`ENTER_PAUSE`], or [`LANDING`] on; then each Enter has [`TAKE`] to
/// start a turn or change the screen, or it's pressed again, [`ENTERS`]
/// times in all. Its agent starting on it at any point is enough.
#[derive(Debug)]
pub struct Delivery {
    /// How many turns its agent had begun as the text went in.
    turns: u64,
    typed: Instant,
    screen: Shown,
    /// Whether the screen has changed since the text went in.
    moved: bool,
    still_since: Instant,
    /// When Enter was last pressed, and how many times it has been.
    pressed: Option<(Instant, u32)>,
}

impl Delivery {
    /// The text has just gone into an agent showing `screen`, which had
    /// begun `turns` turns.
    pub fn typed(screen: Shown, turns: u64, now: Instant) -> Delivery {
        Delivery {
            turns,
            typed: now,
            screen,
            moved: false,
            still_since: now,
            pressed: None,
        }
    }

    /// How many times Enter has been pressed.
    pub fn enters(&self) -> u32 {
        self.pressed.map_or(0, |(_, times)| times)
    }

    /// Takes in the agent's screen and how many turns it has begun at
    /// `now`, and says what to do. [`Step::Enter`] counts as pressed.
    pub fn look(&mut self, screen: Shown, turns: u64, now: Instant) -> Step {
        if turns > self.turns {
            return Step::Taken;
        }
        let changed = screen != self.screen;
        if changed {
            self.screen = screen;
            self.still_since = now;
        }
        let Some((at, times)) = self.pressed else {
            self.moved |= changed;
            let shown =
                self.moved && now.saturating_duration_since(self.still_since) >= ENTER_PAUSE;
            if shown || now.saturating_duration_since(self.typed) >= LANDING {
                self.pressed = Some((now, 1));
                return Step::Enter;
            }
            return Step::Wait;
        };
        if changed {
            return Step::Taken;
        }
        if now.saturating_duration_since(at) < TAKE {
            return Step::Wait;
        }
        if times >= ENTERS {
            return Step::Stalled;
        }
        self.pressed = Some((now, times + 1));
        Step::Enter
    }
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

    fn screen(text: &str) -> Shown {
        Shown {
            rows: text.lines().map(String::from).collect(),
            cursor: None,
        }
    }

    fn look(at_prompt: bool, takes_keys: bool, text: &str) -> Look {
        Look {
            at_prompt,
            takes_keys,
            screen: screen(text),
        }
    }

    #[test]
    fn an_agent_started_again_is_ready_once_it_takes_keys_and_its_screen_holds_still() {
        let start = Instant::now();
        let mut waking = Waking::new(start);
        let ms = |ms| start + Duration::from_millis(ms);
        // It has said it's up, but its prompt doesn't read the terminal yet.
        assert!(!waking.ready(look(true, false, ""), ms(100)));
        assert!(!waking.ready(look(true, false, ""), ms(2000)));
        // It reads keys, and draws the conversation it picked up.
        assert!(!waking.ready(look(true, true, "earlier turns"), ms(2100)));
        assert!(!waking.ready(look(true, true, "earlier turns\n❯"), ms(2300)));
        assert!(!waking.ready(look(true, true, "earlier turns\n❯"), ms(2700)));
        assert!(waking.ready(look(true, true, "earlier turns\n❯"), ms(2800)));
    }

    #[test]
    fn an_agent_not_at_its_prompt_is_never_ready() {
        let start = Instant::now();
        let mut waking = Waking::new(start);
        let later = start + Duration::from_secs(30);
        waking.ready(look(false, true, "starting"), start);
        assert!(!waking.ready(look(false, true, "starting"), later));
    }

    #[test]
    fn an_agent_reading_lines_is_typed_into_after_a_while() {
        let start = Instant::now();
        let mut waking = Waking::new(start);
        waking.ready(look(true, false, "ready"), start);
        let before = start + LINES_GRACE - Duration::from_millis(1);
        assert!(!waking.ready(look(true, false, "ready"), before));
        assert!(waking.ready(look(true, false, "ready"), start + LINES_GRACE));
    }

    #[test]
    fn enter_goes_once_the_text_shows_and_holds_still() {
        let start = Instant::now();
        let ms = |ms| start + Duration::from_millis(ms);
        let mut delivery = Delivery::typed(screen("❯"), 4, start);
        assert_eq!(delivery.look(screen("❯"), 4, ms(50)), Step::Wait);
        assert_eq!(
            delivery.look(screen("❯ [Pasted text]"), 4, ms(100)),
            Step::Wait
        );
        assert_eq!(
            delivery.look(screen("❯ [Pasted text]"), 4, ms(200)),
            Step::Wait
        );
        assert_eq!(
            delivery.look(screen("❯ [Pasted text]"), 4, ms(250)),
            Step::Enter
        );
        assert_eq!(delivery.enters(), 1);
        // It starts on it.
        assert_eq!(
            delivery.look(screen("❯ [Pasted text]"), 4, ms(300)),
            Step::Wait
        );
        assert_eq!(delivery.look(screen("✻ Working…"), 5, ms(400)), Step::Taken);
    }

    #[test]
    fn enter_goes_all_the_same_when_the_text_never_shows() {
        let start = Instant::now();
        let mut delivery = Delivery::typed(screen("$"), 0, start);
        let before = start + LANDING - Duration::from_millis(1);
        assert_eq!(delivery.look(screen("$"), 0, before), Step::Wait);
        assert_eq!(delivery.look(screen("$"), 0, start + LANDING), Step::Enter);
    }

    #[test]
    fn a_screen_changed_by_enter_has_taken_it() {
        let start = Instant::now();
        let mut delivery = Delivery::typed(screen("$"), 0, start);
        assert_eq!(delivery.look(screen("$"), 0, start + LANDING), Step::Enter);
        let after = start + LANDING + Duration::from_millis(500);
        assert_eq!(delivery.look(screen("answer"), 0, after), Step::Taken);
    }

    #[test]
    fn an_enter_that_only_moves_the_cursor_has_taken_it() {
        let start = Instant::now();
        let at = |cursor| Shown {
            cursor: Some(cursor),
            ..screen("$ next")
        };
        let mut delivery = Delivery::typed(at((0, 2)), 0, start);
        assert_eq!(delivery.look(at((0, 6)), 0, start), Step::Wait);
        let now = start + ENTER_PAUSE;
        assert_eq!(delivery.look(at((0, 6)), 0, now), Step::Enter);
        assert_eq!(delivery.look(at((1, 0)), 0, now + TAKE / 2), Step::Taken);
    }

    #[test]
    fn a_turn_begun_before_enter_has_taken_it() {
        let start = Instant::now();
        let mut delivery = Delivery::typed(screen("❯"), 2, start);
        assert_eq!(delivery.look(screen("❯ go"), 3, start), Step::Taken);
        assert_eq!(delivery.enters(), 0);
    }

    #[test]
    fn enter_lost_is_pressed_again_then_the_prompt_has_stalled() {
        let start = Instant::now();
        let mut delivery = Delivery::typed(screen("❯"), 0, start);
        let pasted = || screen("❯ [Pasted text #1 +2 lines]");
        let mut now = start + Duration::from_millis(100);
        assert_eq!(delivery.look(pasted(), 0, now), Step::Wait);
        now += ENTER_PAUSE;
        assert_eq!(delivery.look(pasted(), 0, now), Step::Enter);
        for enters in 2..=ENTERS {
            assert_eq!(delivery.look(pasted(), 0, now + TAKE / 2), Step::Wait);
            now += TAKE;
            assert_eq!(delivery.look(pasted(), 0, now), Step::Enter);
            assert_eq!(delivery.enters(), enters);
        }
        now += TAKE;
        assert_eq!(delivery.look(pasted(), 0, now), Step::Stalled);
        assert_eq!(delivery.enters(), ENTERS);
    }

    #[test]
    fn an_enter_pressed_again_can_be_the_one_taken() {
        let start = Instant::now();
        let mut delivery = Delivery::typed(screen("❯"), 0, start);
        let pasted = || screen("❯ [Pasted text #1 +2 lines]");
        delivery.look(pasted(), 0, start);
        let now = start + ENTER_PAUSE;
        assert_eq!(delivery.look(pasted(), 0, now), Step::Enter);
        assert_eq!(delivery.look(pasted(), 0, now + TAKE), Step::Enter);
        assert_eq!(
            delivery.look(screen("❯"), 1, now + TAKE * 3 / 2),
            Step::Taken
        );
    }
}
