//! What the TUI says of the sessions a restart starts again: once none of
//! them waits its turn any more, a line on the footer until the next key,
//! like `after the restart: 6 sessions back · 1 couldn't start: docs`. A
//! session that couldn't start is said once, also when the TUI opens after
//! the restart is over, and stays marked in the sidebar until it's started
//! again or killed.
//!
//! It's worked out from the list of sessions as the TUI reads it, so a TUI
//! that opens once every session is back has nothing to say.

use crate::protocol::{SessionInfo, State};
use std::collections::HashSet;

/// What the TUI has seen of sessions starting again.
#[derive(Debug, Default)]
pub struct Restarts {
    /// The sessions seen waiting their turn, by id, while some still do. A
    /// session keeps its id once it has started.
    starting: HashSet<String>,
    /// The sessions said to have failed to start, by id, while they're in
    /// the list.
    told: HashSet<String>,
}

/// The line, and whether it says some couldn't start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restarted {
    pub line: String,
    pub failed: bool,
}

impl Restarts {
    /// Takes the list as it is now. Gives the line to say when there's
    /// something new: the sessions seen waiting their turn have all
    /// started, or failed to, or there are sessions that couldn't start
    /// that haven't been said.
    pub fn take(&mut self, sessions: &[SessionInfo]) -> Option<Restarted> {
        let starting: Vec<&SessionInfo> = sessions
            .iter()
            .filter(|session| session.state == State::Starting)
            .collect();
        if !starting.is_empty() {
            self.starting
                .extend(starting.iter().map(|session| session.id.clone()));
            return None;
        }
        let failed: Vec<&SessionInfo> = sessions
            .iter()
            .filter(|session| matches!(session.state, State::Failed { .. }))
            .collect();
        let new: Vec<&str> = failed
            .iter()
            .filter(|session| !self.told.contains(&session.id))
            .map(|session| session.name.as_str())
            .collect();
        self.told = failed.iter().map(|session| session.id.clone()).collect();
        let seen = std::mem::take(&mut self.starting);
        let back = sessions
            .iter()
            .filter(|session| seen.contains(&session.id) && !session.state.is_unstarted())
            .count();
        let parts: Vec<String> = [
            count(back, "session back", "sessions back"),
            (!new.is_empty()).then(|| format!("{} couldn't start: {}", new.len(), new.join(", "))),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| Restarted {
            line: format!("after the restart: {}", parts.join(" · ")),
            failed: !new.is_empty(),
        })
    }
}

/// `2 sessions back`, or nothing at all for none.
fn count(n: usize, one: &str, many: &str) -> Option<String> {
    match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(name: &str, state: State) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            front: None,
            name: name.into(),
            id: format!("id-{name}"),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/"),
            pid: None,
            state,
            activity: None,
            worktree: None,
            changed: 0,
            task: None,
            asking: None,
            reporter: None,
            subagents: 0,
            model: None,
            line: None,
            bell: false,
            unseen_copies: 0,
            context: None,
            output_waits: 0,
        }
    }

    fn failed(name: &str) -> SessionInfo {
        let why = "command not found: claude".to_string();
        session(name, State::Failed { why })
    }

    #[test]
    fn once_every_session_waiting_has_started_or_failed_the_line_says_how_many() {
        let mut restarts = Restarts::default();
        let waiting = vec![
            session("api", State::Running),
            session("docs", State::Starting),
            session("web", State::Starting),
        ];
        assert_eq!(restarts.take(&waiting), None);
        let one_left = vec![
            session("api", State::Running),
            session("docs", State::Running),
            session("web", State::Starting),
        ];
        assert_eq!(restarts.take(&one_left), None);
        let over = vec![
            session("api", State::Running),
            session("docs", State::Running),
            failed("web"),
        ];
        let said = restarts.take(&over).unwrap();
        assert_eq!(
            said.line,
            "after the restart: 1 session back · 1 couldn't start: web"
        );
        assert!(said.failed);
        // Said once.
        assert_eq!(restarts.take(&over), None);
    }

    #[test]
    fn every_session_back_is_said_without_a_failure() {
        let mut restarts = Restarts::default();
        restarts.take(&[session("a", State::Starting), session("b", State::Starting)]);
        let said = restarts
            .take(&[session("a", State::Running), session("b", State::Running)])
            .unwrap();
        assert_eq!(said.line, "after the restart: 2 sessions back");
        assert!(!said.failed);
    }

    #[test]
    fn a_tui_opened_after_the_restart_says_only_what_couldn_t_start() {
        let mut restarts = Restarts::default();
        let list = [
            session("api", State::Running),
            failed("docs"),
            failed("web"),
        ];
        let said = restarts.take(&list).unwrap();
        assert_eq!(said.line, "after the restart: 2 couldn't start: docs, web");
        assert_eq!(restarts.take(&list), None);
        // Nothing waited and nothing failed: nothing to say.
        let mut restarts = Restarts::default();
        assert_eq!(restarts.take(&[session("api", State::Running)]), None);
    }

    #[test]
    fn a_session_failing_again_after_it_was_started_again_is_said_again() {
        let mut restarts = Restarts::default();
        assert!(restarts.take(&[failed("docs")]).is_some());
        // Started again, it's gone from the failed; failing again, under
        // an id of its own, it's new.
        restarts.take(&[session("docs", State::Running)]);
        let mut again = failed("docs");
        again.id = "id-docs-2".into();
        assert!(restarts.take(&[again]).is_some());
    }
}
