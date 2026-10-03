//! Reading what an agent is doing off its screen. It covers agents that
//! don't report through hooks, Claude Code started by hand in a shell, and
//! what hooks never report: a turn cut short with Esc, or work going on
//! again once the user has said yes to a permission.
//!
//! Agents draw tell-tale text while they work or wait: a spinner at the
//! start of the terminal's title, "esc to interrupt" under the prompt, a
//! question with choices. That text changes between versions, so these
//! lists need keeping up with the agents.

use crate::protocol::AgentEvent;
use serde::{Deserialize, Serialize};

/// How many of the last rows with something on them to read: where agents
/// draw their prompt, their status line and their questions. Agents draw
/// inline, so on a fresh screen that's near the top, not at the bottom.
const LAST_ROWS: usize = 15;

/// Text, in lower case, that agents show only while they wait for the user
/// to answer them.
const WAITING_TEXT: &[&str] = &[
    // Claude Code
    "do you want to proceed?",
    "waiting for permission",
    // Codex: each of its approvals asks "would you like to …?", and ends
    // with how to answer. "Allow command?" is from older versions.
    "would you like to run the following command?",
    "would you like to make the following edits?",
    "would you like to grant these permissions?",
    "press enter to confirm or esc to cancel",
    "allow command?",
    // Codex's other questions, like reviewing new hooks when it starts,
    // end with their keys in this shorter form.
    "enter confirm · esc",
];

/// Text, in lower case, that agents show only while they work on a turn.
const WORKING_TEXT: &[&str] = &["esc to interrupt"];

/// What an agent's screen says it's doing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Looks {
    Working,
    Waiting,
    /// Neither: an agent at its prompt, or any other program.
    #[default]
    Settled,
}

/// Reads `rows`, what's on the screen a row at a time, and `title`, the
/// title the program gave its terminal.
pub fn read(rows: &[String], title: &str) -> Looks {
    let bottom = last_rows(rows);
    // Codex says so in its title while it waits on the user.
    if WAITING_TEXT.iter().any(|text| bottom.contains(text)) || title.contains("Action Required") {
        return Looks::Waiting;
    }
    if WORKING_TEXT.iter().any(|text| bottom.contains(text)) || starts_with_spinner(title) {
        return Looks::Working;
    }
    Looks::Settled
}

/// The last [`LAST_ROWS`] rows that aren't blank, as one lower-case string.
fn last_rows(rows: &[String]) -> String {
    let rows: Vec<&str> = rows
        .iter()
        .map(String::as_str)
        .filter(|row| !row.trim().is_empty())
        .collect();
    let first = rows.len().saturating_sub(LAST_ROWS);
    rows[first..].join("\n").to_lowercase()
}

/// Claude Code and Codex start their title with a spinner while they work:
/// a braille pattern, or a circle a quarter of which is filled in.
fn starts_with_spinner(title: &str) -> bool {
    let Some(first) = title.chars().next() else {
        return false;
    };
    let braille = ('\u{2800}'..='\u{28FF}').contains(&first);
    let circle = ('\u{25D0}'..='\u{25D3}').contains(&first);
    braille || circle
}

/// Turns how a session's screen looks, read again at every check, into the
/// events an agent's hooks would send. A new look counts once it has held
/// for two checks in a row, so a screen caught halfway through a redraw
/// doesn't.
#[derive(Debug, Default)]
pub struct ScreenWatch {
    current: Looks,
    /// A different look seen at the last check, waiting to be seen again.
    candidate: Option<Looks>,
}

impl ScreenWatch {
    /// A watch that has seen the screen look `looks`: handed over, so that
    /// how it looked already isn't news.
    pub fn seeing(looks: Looks) -> ScreenWatch {
        ScreenWatch {
            current: looks,
            candidate: None,
        }
    }

    /// How the screen has been seen to look.
    pub fn looks(&self) -> Looks {
        self.current
    }

    pub fn update(&mut self, looks: Looks) -> Option<AgentEvent> {
        if looks == self.current {
            self.candidate = None;
            return None;
        }
        if self.candidate != Some(looks) {
            self.candidate = Some(looks);
            return None;
        }
        self.current = looks;
        self.candidate = None;
        let event = match looks {
            // Back at work, whether a turn has started or the user has
            // answered a question.
            Looks::Working => AgentEvent::TurnStarted,
            Looks::Waiting => AgentEvent::Asking,
            Looks::Settled => AgentEvent::TurnEnded,
        };
        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looks(output: &str, title: &str) -> Looks {
        let mut screen = crate::vt::Screen::new(24, 80);
        screen.process(output.as_bytes());
        read(&screen.rows(false), title)
    }

    #[test]
    fn a_prompt_with_nothing_going_on_is_settled() {
        assert_eq!(
            looks("> \r\n  ? for shortcuts", "✳ Claude Code"),
            Looks::Settled
        );
    }

    #[test]
    fn work_shows_in_the_status_line_or_the_title() {
        let status = "✻ Thinking… (3s)\r\n> \r\n  ⏵⏵ auto mode on · esc to interrupt";
        assert_eq!(looks(status, ""), Looks::Working);
        assert_eq!(looks("> ", "⠋ Fixing the tests"), Looks::Working);
        assert_eq!(looks("> ", "◐ Fixing the tests"), Looks::Working);
    }

    #[test]
    fn a_question_outweighs_the_signs_of_work() {
        let question =
            "Bash command\r\n  echo hi\r\nDo you want to proceed?\r\n❯ 1. Yes\r\n  2. No";
        assert_eq!(looks(question, "⠋ Running"), Looks::Waiting);
        assert_eq!(looks("> ", "Action Required"), Looks::Waiting);
    }

    // The Codex screens below are from its own TUI snapshot tests.

    #[test]
    fn codex_at_work_says_so_in_its_status_line() {
        let working = "• Working (0s • esc to interrupt)\r\n\r\n› Ask Codex to do anything\r\n\r\n  gpt-5 default · /tmp/project";
        assert_eq!(looks(working, ""), Looks::Working);
        let resting = "› Ask Codex to do anything\r\n\r\n  gpt-5 default · /tmp/project";
        assert_eq!(looks(resting, ""), Looks::Settled);
    }

    #[test]
    fn codex_asking_to_run_a_command_waits() {
        let question = "  Would you like to run the following command?\r\n\r\n  $ echo hello world\r\n\r\n› 1. Yes, proceed (y)\r\n  2. Yes, and don't ask again (p)\r\n  3. No, and tell Codex what to do differently (esc)\r\n\r\n  Press enter to confirm or esc to cancel";
        assert_eq!(looks(question, ""), Looks::Waiting);
        let edits = "  Would you like to make the following edits?\r\n\r\n› 1. Yes, proceed (y)";
        assert_eq!(looks(edits, ""), Looks::Waiting);
    }

    #[test]
    fn a_codex_question_at_startup_is_waiting() {
        let hooks = "  Hooks need review\r\n  12 hooks are new or changed.\r\n\
                     › 1. Review hooks\r\n  2. Trust all and continue\r\n\
                     \r\n  enter confirm · esc skip";
        assert_eq!(looks(hooks, ""), Looks::Waiting);
    }

    #[test]
    fn only_the_last_rows_count() {
        let mut output = String::from("esc to interrupt\r\n");
        for line in 0..20 {
            output.push_str(&format!("line {line}\r\n"));
        }
        output.push_str("> ");
        assert_eq!(looks(&output, ""), Looks::Settled);
    }

    #[test]
    fn a_new_look_counts_once_it_holds() {
        let mut watch = ScreenWatch::default();
        assert_eq!(watch.update(Looks::Settled), None);
        assert_eq!(watch.update(Looks::Working), None);
        assert_eq!(watch.update(Looks::Working), Some(AgentEvent::TurnStarted));
        assert_eq!(watch.update(Looks::Working), None);
    }

    #[test]
    fn a_look_that_flickers_never_counts() {
        let mut watch = ScreenWatch::default();
        assert_eq!(watch.update(Looks::Working), None);
        assert_eq!(watch.update(Looks::Settled), None);
        assert_eq!(watch.update(Looks::Working), None);
        assert_eq!(watch.update(Looks::Settled), None);
    }

    #[test]
    fn each_look_means_its_event() {
        let mut watch = ScreenWatch::default();
        let mut settle_on = |looks| {
            watch.update(looks);
            watch.update(looks)
        };
        assert_eq!(settle_on(Looks::Working), Some(AgentEvent::TurnStarted));
        assert_eq!(settle_on(Looks::Waiting), Some(AgentEvent::Asking));
        assert_eq!(settle_on(Looks::Working), Some(AgentEvent::TurnStarted));
        assert_eq!(settle_on(Looks::Settled), Some(AgentEvent::TurnEnded));
    }
}
