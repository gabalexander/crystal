//! Keeping wikis up to date on their own, with `[wiki] auto_update`: the
//! daemon looks every few minutes at each project with a wiki, fetches its
//! default branch, and when that has moved past the wiki's commit, starts
//! `crystal wiki update` for it in the background: one build at a time,
//! and for each project at most one every [`AT_MOST_EVERY`]. The update is
//! a process of its own, which the daemon doesn't wait for, so a handover
//! never waits on it either.

use super::book;
use super::crystal;
use super::repo;
use super::{BUILD_FILE, LOG_FILE, Listed};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How often the daemon looks.
pub const LOOK_EVERY: Duration = Duration::from_secs(5 * 60);

/// The least time between two updates of one project it starts.
pub const AT_MOST_EVERY: Duration = Duration::from_secs(15 * 60);

/// Looks after the wikis of the daemon at `socket` on a thread of its own,
/// for as long as the daemon runs.
pub fn follow(socket: &Path) {
    let socket = socket.to_path_buf();
    thread::spawn(move || {
        let mut started: HashMap<PathBuf, Instant> = HashMap::new();
        loop {
            thread::sleep(LOOK_EVERY);
            if let Err(err) = look(&socket, &mut started) {
                crystal::errln!("crystal daemon: couldn't look at the wikis: {err:#}");
            }
        }
    });
}

/// One look at every wiki: starts the update of the first that's due.
fn look(socket: &Path, started: &mut HashMap<PathBuf, Instant>) -> Result<()> {
    let settings = crystal::settings()?;
    if !crystal::enabled()? || !settings.auto_update {
        return Ok(());
    }
    let wikis = super::list(socket);
    // One build at a time, whoever started it.
    if wikis.iter().any(|listed| book::building(&listed.dir)) {
        return Ok(());
    }
    for listed in wikis {
        if !listed.root.is_dir() {
            continue;
        }
        let since = started.get(&listed.root).map(Instant::elapsed);
        if since.is_some_and(|since| since < AT_MOST_EVERY) {
            continue;
        }
        let Ok(tip) = repo::default_tip(&listed.root, true) else {
            continue;
        };
        let missing = book::Book::read(&listed.dir.join(BUILD_FILE))
            .ok()
            .and_then(|book| book.built)
            .is_some_and(|built| !built.missing.is_empty());
        if due(&listed, &tip.commit, missing) {
            start_update(socket, &listed)?;
            started.insert(listed.root.clone(), Instant::now());
            return Ok(());
        }
    }
    Ok(())
}

/// Whether `listed`'s wiki is due an update with its default branch at
/// `tip`: it has moved, or the last build left subsections unwritten.
fn due(listed: &Listed, tip: &str, missing: bool) -> bool {
    listed.commit != tip || missing
}

/// Starts `crystal wiki update` for `listed` in the background, its errors
/// in the wiki's log: through a shell that leaves it running and ends, so
/// the daemon has no child to wait for.
fn start_update(socket: &Path, listed: &Listed) -> Result<()> {
    let crystal = std::env::current_exe().context("couldn't find crystal's own program")?;
    let status = Command::new("sh")
        .arg("-c")
        .arg(r#""$@" </dev/null >/dev/null 2>>"$CRYSTAL_WIKI_LOG" &"#)
        .arg("sh")
        .arg(crystal)
        .arg("--socket")
        .arg(socket)
        .args(["wiki", "update", "--auto", "-C"])
        .arg(&listed.root)
        .env("CRYSTAL_WIKI_LOG", listed.dir.join(LOG_FILE))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("couldn't start the update")?;
    if !status.success() {
        anyhow::bail!("the update couldn't start: {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wiki_is_due_once_its_branch_moves_or_it_has_gaps() {
        let listed = Listed {
            key: "app-1".into(),
            dir: "/state/wiki/app-1".into(),
            name: "app".into(),
            root: "/code/app".into(),
            commit: "abc".into(),
            updated: "2026-10-09T12:00:00Z".into(),
        };
        assert!(!due(&listed, "abc", false));
        assert!(due(&listed, "def", false));
        assert!(due(&listed, "abc", true));
    }
}
