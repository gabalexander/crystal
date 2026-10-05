//! What a session is doing, as the TUI shows it: one mark, in a color the
//! theme picks. The marks are shapes that tell apart at a glance even
//! without color: a warning triangle for a session waiting on you, a turning
//! circle while it works, a tick when it's done, a diamond for a task left
//! open that asks nothing of you.

use crate::protocol::{Activity, SessionInfo, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Its agent is asking the user something.
    Waiting,
    Working,
    /// Its agent finished a turn, and nobody has looked since.
    Done,
    /// Its agent ended its turn with its task still open, waiting on
    /// something other than the user, like its tests or CI: it needs
    /// nobody, so it isn't pinned, told of or gone to with `u`.
    Open,
    /// An agent at its prompt, or any program that doesn't say.
    Running,
    /// Its program ended well.
    Ended,
    /// Its program failed, or was killed, or it couldn't start again after
    /// a restart.
    Failed,
    /// It waits its turn to start again after a restart.
    Starting,
}

/// What a session needs of the user, the most pressing first: the sidebar
/// pins those that need anything, and `u` goes to them in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Need {
    /// Its agent is asking the user something.
    Answer,
    /// It couldn't start again after a restart: nothing runs there until
    /// the user puts that right, or kills it.
    Restart,
    /// Its agent finished a turn nobody has looked at.
    Look,
}

impl Need {
    pub fn of(session: &SessionInfo) -> Option<Need> {
        match Status::of(session) {
            Status::Waiting => Some(Need::Answer),
            Status::Done => Some(Need::Look),
            _ if matches!(session.state, State::Failed { .. }) => Some(Need::Restart),
            _ => None,
        }
    }
}

/// The working mark turns through these, a quarter at a time.
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

impl Status {
    pub fn of(session: &SessionInfo) -> Status {
        match (&session.state, session.activity) {
            (State::Running, Some(Activity::Waiting)) => Status::Waiting,
            (State::Running, Some(Activity::Working)) => Status::Working,
            (State::Running, Some(Activity::Done)) => Status::Done,
            (State::Running, Some(Activity::Idle))
                if session.task.as_ref().is_some_and(|task| task.is_open()) =>
            {
                Status::Open
            }
            (State::Running, _) => Status::Running,
            (State::Exited { code: 0 }, _) => Status::Ended,
            (State::Starting, _) => Status::Starting,
            // Stopped by crystal, which isn't the program failing.
            _ if session.stopped_idle => Status::Ended,
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
            Status::Open => "◇",
            Status::Running => "▸",
            Status::Ended | Status::Failed => "■",
            Status::Starting => "◌",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TaskInfo;
    use std::path::PathBuf;

    fn session(state: State, activity: Option<Activity>) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            front: None,
            name: "s".into(),
            id: "s".into(),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state,
            activity,
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
            row: Default::default(),
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
    fn a_session_waiting_to_start_again_is_marked_apart_from_one_that_couldn_t() {
        let starting = Status::of(&session(State::Starting, None));
        assert_eq!((starting, starting.mark(0)), (Status::Starting, "◌"));
        let why = "its directory, ~/code/app, isn't there".to_string();
        let failed = Status::of(&session(State::Failed { why }, None));
        assert_eq!(failed, Status::Failed);
    }

    #[test]
    fn a_session_that_couldn_t_start_again_needs_the_user_after_a_question() {
        let why = "command not found: claude".to_string();
        let failed = session(State::Failed { why }, None);
        let asking = session(State::Running, Some(Activity::Waiting));
        let done = session(State::Running, Some(Activity::Done));
        assert_eq!(Need::of(&failed), Some(Need::Restart));
        assert_eq!(Need::of(&asking), Some(Need::Answer));
        assert_eq!(Need::of(&done), Some(Need::Look));
        assert!(Need::Answer < Need::Restart && Need::Restart < Need::Look);
        // A program that failed, or one waiting its turn, needs nothing.
        assert_eq!(Need::of(&session(State::Exited { code: 1 }, None)), None);
        assert_eq!(Need::of(&session(State::Starting, None)), None);
        assert_eq!(Need::of(&session(State::Running, None)), None);
    }

    #[test]
    fn a_task_left_open_asking_nothing_is_marked_and_needs_nobody() {
        let goal = |waiting| TaskInfo {
            id: Some(1),
            goal: "fix it".into(),
            background: false,
            backlog: None,
            waiting,
            created: 0,
            outcome: None,
            brief: Default::default(),
        };
        let open = SessionInfo {
            task: Some(goal(false)),
            ..session(State::Running, Some(Activity::Idle))
        };
        assert_eq!(Status::of(&open), Status::Open);
        assert_eq!(Status::Open.mark(0), "◇");
        assert_eq!(Need::of(&open), None);
        // One asking the user something waits on them.
        let asking = SessionInfo {
            task: Some(goal(true)),
            ..session(State::Running, Some(Activity::Waiting))
        };
        assert_eq!(Need::of(&asking), Some(Need::Answer));
        // An agent at rest with no task is just that.
        let idle = session(State::Running, Some(Activity::Idle));
        assert_eq!(Status::of(&idle), Status::Running);
    }

    #[test]
    fn the_working_mark_turns() {
        let marks: Vec<&str> = (0..5).map(|spin| Status::Working.mark(spin)).collect();
        assert_eq!(marks, ["◐", "◓", "◑", "◒", "◐"]);
        assert_eq!(Status::Done.mark(3), "✓");
    }
}
