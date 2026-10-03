//! Tasks: sessions started with something to do. A task is numbered as
//! it's made, `t12`, and stays open until its agent closes it with `crystal
//! done`, the user closes or cancels it, or, for a background task, its run
//! ends. Its session ending while it's open fails it, and killing the
//! session cancels it. Once closed, it goes into its project's history:
//! what it was asked to do, how it went, and where it ran. The daemon adds
//! a row to the project's history in the database as each task closes, so
//! the history outlives the sessions it happened in.
//!
//! A task can also be made to start later (`--no-launch`): the daemon keeps
//! it in the database until it's started, with nothing working on it.
//!
//! Everything tasks add to crystal goes through [`enabled`], so they can be
//! switched off as one.

use crate::config::Config;
use crate::git::Checkout;
use crate::plugins;
use crate::project;
use crate::protocol::{PendingTask, TaskRecord, TaskStart};
use anyhow::Result;
use std::fs;
use std::path::Path;

/// Whether tasks are on: the `tasks` plugin.
pub fn enabled(config: &Config) -> bool {
    plugins::enabled(config, "tasks")
}

/// Refuses a command that's only about tasks while they're off.
pub fn ensure_enabled(config: &Config) -> Result<()> {
    plugins::ensure_enabled(config, "tasks")
}

/// The most a prompt crystal puts together to start an agent may be: a flow
/// step's, or what an agent is told on top of its task. It may go on the
/// agent's command line, so it's kept well within what one can hold.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;

/// What an agent given a task is told about it: how to close it, and, with
/// `backlog` on, where to put what it notices for later.
pub fn instructions(backlog: bool) -> String {
    let mut text = "What you were asked to do is a task, and crystal shows the user \
                    whether it's still open. Once you've done it, the last step is to \
                    close it: run `crystal done \"<one line on what you did>\"`. If you \
                    can't do it, run `crystal done --failed \"<why>\"` instead."
        .to_string();
    if backlog {
        text.push_str(
            " Anything you notice that's worth doing later but isn't part of this \
             task, put on the project's backlog with `crystal backlog add \"<what>\"` \
             rather than doing it now.",
        );
    }
    text
}

/// What an agent is told when it ends a turn with its task still open, once
/// a task: agents don't always remember to close theirs, Haiku least of all.
/// One that isn't through, say because it's waiting on the user, is told to
/// leave it open.
pub const REMINDER: &str = "Your crystal task is still open. If you've done it, close it now: \
                            run `crystal done \"<one line on what you did>\"`, or `crystal \
                            done --failed \"<why>\"` if you couldn't do it. If you aren't \
                            through, say you're waiting on the user, leave it open and end \
                            your turn.";

/// A task waiting to start, as `crystal tasks` lists it.
pub fn pending_record(task: &PendingTask) -> TaskRecord {
    let worktree = Checkout::find(&task.cwd).map(|checkout| checkout.worktree());
    let project = match &worktree {
        Some(worktree) => worktree.project.clone(),
        None => project::of(&task.cwd).name,
    };
    TaskRecord {
        id: Some(task.id),
        goal: task.goal.clone(),
        session: String::new(),
        project,
        branch: worktree.and_then(|worktree| worktree.branch),
        background: matches!(task.start, TaskStart::Background { .. }),
        backlog: task.backlog,
        pending: true,
        waiting: false,
        created: task.created,
        outcome: None,
        artifacts: Vec::new(),
    }
}

/// The number a task is given as on the command line: `t12`, or just `12`.
pub fn parse_id(text: &str) -> Option<u64> {
    text.strip_prefix('t').unwrap_or(text).parse().ok()
}

/// The file a project's closed tasks were kept in before the database, in
/// its directory: one JSON line each.
pub const OLD_FILE: &str = "tasks.jsonl";

/// The closed tasks kept in the project directory `dir` before the
/// database, in the order they closed. A line that can't be read, say one
/// cut short by a crash, is left out.
pub fn load_old(dir: &Path) -> Vec<TaskRecord> {
    let Ok(text) = fs::read_to_string(dir.join(OLD_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TaskState;

    #[test]
    fn a_task_waiting_to_start_is_listed_as_pending() {
        let task = PendingTask {
            id: 4,
            goal: "later".into(),
            cwd: "/nowhere".into(),
            name: None,
            start: TaskStart::Agent {
                command: vec!["claude".into(), "later".into()],
            },
            backlog: None,
            created: 1,
        };
        let record = pending_record(&task);
        assert_eq!(record.state(), TaskState::Pending);
        assert_eq!(record.id, Some(4));
        assert_eq!(record.project, "nowhere");
        assert!(record.session.is_empty());
    }

    #[test]
    fn an_id_is_given_with_its_t_or_without() {
        assert_eq!(parse_id("t12"), Some(12));
        assert_eq!(parse_id("12"), Some(12));
        assert_eq!(parse_id("fixer"), None);
        assert_eq!(parse_id("t"), None);
    }

    #[test]
    fn a_record_from_before_tasks_had_numbers_still_reads() {
        let old = r#"{"goal":"x","session":"s","project":"p","outcome":{"failed":true,"summary":"no","closed":3}}"#;
        let record: TaskRecord = serde_json::from_str(old).unwrap();
        assert_eq!(record.id, None);
        assert_eq!(record.state(), TaskState::Failed);
    }

    #[test]
    fn a_broken_line_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let kept = r#"{"goal": "kept", "session": "claude", "project": "payments"}"#;
        let text = format!("{kept}\n{{\"goal\": \"cut sh");
        fs::write(dir.path().join(OLD_FILE), text).unwrap();
        let goals: Vec<String> = load_old(dir.path())
            .into_iter()
            .map(|task| task.goal)
            .collect();
        assert_eq!(goals, ["kept"]);
        assert!(load_old(&dir.path().join("none")).is_empty());
    }
}
