//! Reading what an agent is doing off its screen. It covers agents that
//! don't report through hooks, Claude Code started by hand in a shell, and
//! what hooks never report: a turn cut short with Esc, or work going on
//! again once the user has said yes to a permission.
//!
//! Agents draw tell-tale text while they work or wait: a spinner at the
//! start of the terminal's title, "esc to interrupt" under the prompt, a
//! question with choices. That text changes between versions, so it's kept
//! in a file of rules for each agent, which the user can change: see
//! [`crate::agent_rules`].

use crate::agent_rules::{self, Input};
use crate::protocol::AgentEvent;
use serde::{Deserialize, Serialize};

/// What an agent's screen says it's doing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Looks {
    Working,
    Waiting,
    /// Neither: an agent at its prompt, or any other program.
    #[default]
    Settled,
}

/// Reads the screen of `agent`, the program in front: `rows`, what's on it
/// a row at a time, `title`, the title the program gave its terminal, and
/// `progress`, the progress it reports. `None` when the screen says nothing
/// either way, like a menu over the agent's prompt: the look stays as it
/// was.
pub fn read(agent: &str, rows: &[String], title: &str, progress: &str) -> Option<Looks> {
    let screen = Input {
        rows,
        title,
        progress,
    };
    agent_rules::current().for_program(agent).read(&screen)
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

    /// A different look seen once, which counts if it's seen again.
    pub fn candidate(&self) -> Option<Looks> {
        self.candidate
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

    fn looks(agent: &str, output: &str, title: &str) -> Option<Looks> {
        let mut screen = crate::vt::Screen::new(24, 80);
        screen.process(output.as_bytes());
        read(agent, &screen.rows(false), title, screen.progress())
    }

    #[test]
    fn a_prompt_with_nothing_going_on_is_settled() {
        assert_eq!(
            looks("claude", "> \r\n  ? for shortcuts", "✳ Claude Code"),
            Some(Looks::Settled)
        );
    }

    #[test]
    fn work_shows_in_the_status_line_or_the_title() {
        let status = "✻ Thinking… (3s)\r\n> \r\n  ⏵⏵ auto mode on · esc to interrupt";
        assert_eq!(looks("claude", status, ""), Some(Looks::Working));
        let working = Some(Looks::Working);
        assert_eq!(looks("claude", "> ", "⠋ Fixing the tests"), working);
        assert_eq!(looks("claude", "> ", "◐ Fixing the tests"), working);
    }

    #[test]
    fn a_question_is_waiting() {
        let question =
            "Bash command\r\n  echo hi\r\nDo you want to proceed?\r\n❯ 1. Yes\r\n  2. No";
        let title = "✳ Claude Code";
        assert_eq!(looks("claude", question, title), Some(Looks::Waiting));
        assert_eq!(
            looks("codex", "› ", "Action Required"),
            Some(Looks::Waiting)
        );
    }

    #[test]
    fn claude_s_spinner_outweighs_a_question_left_on_screen() {
        // Claude Code stops its spinner while it asks: one that turns says
        // it's at work, whatever the screen still shows.
        let question =
            "Bash command\r\n  echo hi\r\nDo you want to proceed?\r\n❯ 1. Yes\r\n  2. No";
        assert_eq!(looks("claude", question, "⠋ Running"), Some(Looks::Working));
    }

    // The Codex screens below are from its own TUI snapshot tests.

    #[test]
    fn codex_at_work_says_so_in_its_status_line() {
        let working = "• Working (0s • esc to interrupt)\r\n\r\n› Ask Codex to do anything\r\n\r\n  gpt-5 default · /tmp/project";
        assert_eq!(looks("codex", working, ""), Some(Looks::Working));
        let resting = "› Ask Codex to do anything\r\n\r\n  gpt-5 default · /tmp/project";
        assert_eq!(looks("codex", resting, ""), Some(Looks::Settled));
    }

    #[test]
    fn codex_asking_to_run_a_command_waits() {
        let question = "  Would you like to run the following command?\r\n\r\n  $ echo hello world\r\n\r\n› 1. Yes, proceed (y)\r\n  2. Yes, and don't ask again (p)\r\n  3. No, and tell Codex what to do differently (esc)\r\n\r\n  Press enter to confirm or esc to cancel";
        assert_eq!(looks("codex", question, ""), Some(Looks::Waiting));
        let edits = "  Would you like to make the following edits?\r\n\r\n› 1. Yes, proceed (y)";
        assert_eq!(looks("codex", edits, ""), Some(Looks::Waiting));
    }

    #[test]
    fn a_codex_question_at_startup_is_waiting() {
        let hooks = "  Hooks need review\r\n  12 hooks are new or changed.\r\n\
                     › 1. Review hooks\r\n  2. Trust all and continue\r\n\
                     \r\n  enter confirm · esc skip";
        assert_eq!(looks("codex", hooks, ""), Some(Looks::Waiting));
    }

    #[test]
    fn an_agent_with_no_rules_of_its_own_is_read_the_common_way() {
        let working = "> fix it\r\n  thinking · esc to interrupt";
        assert_eq!(looks("aider", working, ""), Some(Looks::Working));
        let asking = "Allow command?\r\n  rm -rf build";
        assert_eq!(looks("aider", asking, ""), Some(Looks::Waiting));
        assert_eq!(looks("aider", "> ", "⠋ aider"), Some(Looks::Working));
        assert_eq!(looks("aider", "> ", ""), Some(Looks::Settled));
    }

    #[test]
    fn only_the_last_rows_count() {
        let mut output = String::from("esc to interrupt\r\n");
        for line in 0..20 {
            output.push_str(&format!("line {line}\r\n"));
        }
        output.push_str("> ");
        assert_eq!(looks("aider", &output, ""), Some(Looks::Settled));
    }

    #[test]
    fn a_screen_that_says_nothing_leaves_the_look() {
        // Claude Code's transcript viewer, over its prompt.
        let viewer = "  ⎿ Read 20 lines\r\n\r\nShowing detailed transcript · ctrl+o to toggle";
        assert_eq!(looks("claude", viewer, ""), None);
    }

    #[test]
    fn progress_an_agent_reports_counts() {
        // Kiro says it's at work with OSC 9;4's state 3.
        assert_eq!(looks("kiro", "\x1b]9;4;3\x07> ", ""), Some(Looks::Working));
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
