//! Tasks: sessions started with something to do. A task stays open until
//! its agent closes it with `crystal done`, the user closes it, or, for a
//! background task, its run ends. Once closed, it goes into its project's
//! history: what it was asked to do, how it went, and where it ran. The
//! daemon adds a row to the project's history in the database as each task
//! closes, so the history outlives the sessions it happened in.
//!
//! Everything tasks add to crystal goes through [`enabled`], so they can be
//! switched off as one.

use crate::config::Config;
use crate::plugins;
use crate::protocol::TaskRecord;
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
