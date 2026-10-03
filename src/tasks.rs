//! Tasks: sessions started with something to do. A task stays open until
//! its agent closes it with `crystal done`, the user closes it, or, for a
//! background task, its run ends. Once closed, it goes into its project's
//! history: what it was asked to do, how it went, and where it ran. The
//! daemon adds a line to the project's log as each task closes, in the
//! project's directory in the state dir, so the history outlives the
//! sessions it happened in.
//!
//! Everything tasks add to crystal goes through [`enabled`], so they can be
//! switched off as one.

use crate::config::Config;
use crate::plugins;
use crate::protocol::TaskRecord;
use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::Write;
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

/// The file a project's closed tasks are kept in: one JSON line each, so a
/// new one is added without reading or writing the others.
const FILE: &str = "tasks.jsonl";

pub fn record(dir: &Path, task: &TaskRecord) -> Result<()> {
    fs::create_dir_all(dir)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FILE))?;
    let mut line = serde_json::to_vec(task)?;
    line.push(b'\n');
    file.write_all(&line)?;
    Ok(())
}

/// The project's closed tasks, in the order they closed. A line that can't
/// be read, say one cut short by a crash, is left out.
pub fn load(dir: &Path) -> Vec<TaskRecord> {
    let Ok(text) = fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TaskOutcome;

    fn closed(goal: &str, at: u64) -> TaskRecord {
        TaskRecord {
            goal: goal.into(),
            session: "claude".into(),
            project: "payments".into(),
            branch: Some("main".into()),
            background: false,
            backlog: None,
            outcome: Some(TaskOutcome {
                failed: false,
                summary: "did it".into(),
                closed: at,
            }),
        }
    }

    #[test]
    fn closed_tasks_load_back_in_the_order_they_closed() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), &closed("first", 1)).unwrap();
        record(dir.path(), &closed("second", 2)).unwrap();
        let goals: Vec<String> = load(dir.path()).into_iter().map(|task| task.goal).collect();
        assert_eq!(goals, ["first", "second"]);
    }

    #[test]
    fn a_broken_line_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), &closed("kept", 1)).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE))
            .unwrap();
        file.write_all(b"{\"goal\": \"cut sh").unwrap();
        assert_eq!(load(dir.path()).len(), 1);
        assert!(load(&dir.path().join("none")).is_empty());
    }
}
