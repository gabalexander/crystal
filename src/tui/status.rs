//! What a session is doing, as the TUI shows it: one mark, in a color the
//! theme picks. The marks are shapes that tell apart at a glance even
//! without color: a warning triangle for a session waiting on you, a turning
//! circle while it works, a tick when it's done.

use crate::protocol::{Activity, SessionInfo, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Its agent is asking the user something.
    Waiting,
    Working,
    /// Its agent finished a turn, and nobody has looked since.
    Done,
    /// An agent at its prompt, or any program that doesn't say.
    Running,
    /// Its program ended well.
    Ended,
    /// Its program failed, or was killed.
    Failed,
}

/// The working mark turns through these, a quarter at a time.
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

impl Status {
    pub fn of(session: &SessionInfo) -> Status {
        match (&session.state, session.activity) {
            (State::Running, Some(Activity::Waiting)) => Status::Waiting,
            (State::Running, Some(Activity::Working)) => Status::Working,
            (State::Running, Some(Activity::Done)) => Status::Done,
            (State::Running, _) => Status::Running,
            (State::Exited { code: 0 }, _) => Status::Ended,
            _ => Status::Failed,
        }
    }

    /// The mark for the status. `spin` turns the working mark: the drawing
    /// passes a number that goes up with time.
    pub fn mark(self, spin: usize) -> &'static str {
        match self {
            Status::Waiting => "▲",
            Status::Working => SPINNER[spin % SPINNER.len()],
            Status::Done => "✓",
            Status::Running => "▸",
            Status::Ended | Status::Failed => "■",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(state: State, activity: Option<Activity>) -> SessionInfo {
        SessionInfo {
            name: "s".into(),
            id: "s".into(),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state,
            activity,
            worktree: None,
            changed: 0,
        }
    }

    #[test]
    fn what_an_agent_says_counts_only_while_it_runs() {
        let running = |activity| Status::of(&session(State::Running, activity));
        assert_eq!(running(Some(Activity::Waiting)), Status::Waiting);
        assert_eq!(running(Some(Activity::Idle)), Status::Running);
        assert_eq!(running(None), Status::Running);
        let ended = session(State::Exited { code: 2 }, Some(Activity::Working));
        assert_eq!(Status::of(&ended), Status::Failed);
    }

    #[test]
    fn the_working_mark_turns() {
        let marks: Vec<&str> = (0..5).map(|spin| Status::Working.mark(spin)).collect();
        assert_eq!(marks, ["◐", "◓", "◑", "◒", "◐"]);
        assert_eq!(Status::Done.mark(3), "✓");
    }
}
