//! A project's wiki: one page about its code, in the manner of Google's
//! Code Wiki, written by Claude through Claude Code with `crystal wiki
//! build` into a directory of the project's own in the server's state,
//! kept up to date as the default branch moves, which `crystal wiki serve`
//! serves and `crystal wiki export` writes out as a site (see
//! [`crate::wiki_server`] and [`crate::wiki_site`]).
//!
//! In each project's directory ([`dir`]): `wiki.json`, the page's text and
//! diagrams at a commit (see [`model`]); `build.json`, how the build that
//! writes it is going, and what it was written from (see [`book`]);
//! `build.log`; and `checkout/`, the clean checkout it's written from.
//! `crystal wiki build` writes it ([`build`]), `crystal wiki update` writes
//! again what changed, and with `[wiki] auto_update`, the daemon has it
//! updated as the default branch moves ([`auto`]).
//!
//! The generator reaches the rest of crystal through [`crystal`] alone.

pub mod auto;
pub mod book;
mod build;
mod check;
mod claude;
pub mod cli;
pub mod crystal;
mod files;
pub mod index;
pub mod model;
mod plan;
mod prose;
mod repo;
mod write;

use model::Wiki;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The wiki's file, in its directory.
pub const WIKI_FILE: &str = "wiki.json";

/// The build's own bookkeeping, in its directory.
pub const BUILD_FILE: &str = "build.json";

/// What the builds did, a line a step.
pub const LOG_FILE: &str = "build.log";

/// Where the daemon at `socket` keeps every project's wiki.
pub fn root(socket: &Path) -> PathBuf {
    crystal::wikis_dir(socket)
}

/// Where the daemon at `socket` keeps the wiki of the project whose main
/// worktree is `project`: a directory named as the project's backlog's
/// was, its name and a hash of its path.
pub fn dir(socket: &Path, project: &Path) -> PathBuf {
    root(socket).join(crystal::project_key(project))
}

/// The clean checkout a wiki's directory `dir` writes from, which a chat
/// about the wiki reads too.
pub fn checkout_dir(dir: &Path) -> PathBuf {
    dir.join("checkout")
}

/// A project with a wiki, for a list of them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listed {
    pub key: String,
    pub dir: PathBuf,
    pub name: String,
    pub root: PathBuf,
    pub commit: String,
    /// When it was last written, in UTC.
    pub updated: String,
}

/// Every project the daemon at `socket` has a wiki of, by name.
pub fn list(socket: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(root(socket)) else {
        return Vec::new();
    };
    let mut listed: Vec<Listed> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let dir = entry.path();
            let wiki = Wiki::read(&dir.join(WIKI_FILE)).ok()?;
            Some(Listed {
                key: entry.file_name().to_string_lossy().into_owned(),
                name: wiki.repo.name,
                root: wiki.repo.root,
                commit: wiki.repo.commit,
                updated: wiki.generated.at,
                dir,
            })
        })
        .collect();
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    listed
}

/// How a project's wiki stands, for `crystal wiki status` and the page's
/// `/api/status`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Status {
    /// Whether a build is writing it now.
    pub building: bool,
    /// What that build is doing, or where one that stopped before it ended
    /// stopped: `writing 12/64 subsections`.
    pub progress: Option<String>,
    /// When it was last written, in UTC; `None` before its first build.
    pub updated: Option<String>,
    /// Whether the default branch has moved since.
    pub stale: bool,
    /// The commit it was written from.
    pub commit: Option<String>,
    /// The default branch, and where it is now.
    pub branch: Option<String>,
    pub head: Option<String>,
    /// How many commits it's behind the default branch.
    pub behind: Option<u64>,
    /// Whether a build stopped before it ended, which `crystal wiki build`
    /// carries on.
    pub stopped: bool,
    /// What every build of it has cost, in US dollars.
    pub cost_usd: f64,
    /// The subsections the last build couldn't write.
    pub missing: usize,
}

/// How the wiki of the project whose main worktree is `project` stands,
/// for the daemon at `socket`.
pub fn status(socket: &Path, project: &Path) -> Status {
    let dir = dir(socket, project);
    let building = book::building(&dir);
    let book = book::Book::read(&dir.join(BUILD_FILE)).unwrap_or_default();
    let tip = repo::default_tip(project, false).ok();
    let built = book.built.as_ref();
    let stale = match (built, &tip) {
        (Some(built), Some(tip)) => built.commit != tip.commit,
        _ => false,
    };
    let behind = match (built, &tip) {
        (Some(built), Some(tip)) if stale => {
            repo::commits_between(project, &built.commit, &tip.commit)
        }
        (Some(_), Some(_)) => Some(0),
        _ => None,
    };
    Status {
        building,
        progress: book.run.as_ref().map(|run| run.progress.clone()),
        updated: built.map(|built| built.at.clone()),
        stale,
        commit: built.map(|built| built.commit.clone()),
        branch: tip.as_ref().map(|tip| tip.branch.clone()),
        head: tip.map(|tip| tip.commit),
        behind,
        stopped: !building && book.run.is_some(),
        cost_usd: built.map_or(0.0, |built| built.cost_usd),
        missing: built.map_or(0, |built| built.missing.len()),
    }
}

/// A build or an update of a project's wiki, as the events about it tell
/// it: the commit it writes from, and once it has ended, how many sections
/// and subsections the wiki has, how many it wrote, what it cost and how
/// long it took; or why it failed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "WikiAbout"))]
pub struct About {
    pub commit: String,
    /// Whether it writes only what changed since the wiki's commit.
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub sections: usize,
    #[serde(default)]
    pub subsections: usize,
    /// How many subsections it wrote.
    #[serde(default)]
    pub written: usize,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

impl About {
    /// How it went, in a line: `an update at 22ee18c: 3 of 64 subsections
    /// written in 12 sections ($0.84, 95s)`; with `started`, what started.
    pub fn line(&self, started: bool) -> String {
        let what = if self.update { "an update" } else { "a build" };
        let commit = &self.commit[..self.commit.len().min(7)];
        if started {
            return format!("{what} at {commit}");
        }
        match &self.failed {
            Some(why) => format!("{what} at {commit} stopped: {why} (${:.2})", self.cost_usd),
            None => format!(
                "{what} at {commit}: {} of {} subsections written in {} sections (${:.2}, {}s)",
                self.written, self.subsections, self.sections, self.cost_usd, self.seconds
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_s_wiki_is_kept_by_its_name_and_a_hash_of_its_path() {
        let socket = Path::new("/tmp/test/crystal.sock");
        let app = dir(socket, Path::new("/code/app"));
        assert!(
            app.starts_with("/tmp/test/crystal.wiki"),
            "{}",
            app.display()
        );
        let name = app.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("app-"), "{name}");
        assert_ne!(app, dir(socket, Path::new("/elsewhere/app")));
    }

    #[test]
    fn what_a_build_did_reads_in_a_line() {
        let about = About {
            commit: "22ee18c82b0956c4".into(),
            update: true,
            sections: 12,
            subsections: 64,
            written: 3,
            cost_usd: 0.84,
            seconds: 95,
            failed: None,
        };
        assert_eq!(about.line(true), "an update at 22ee18c");
        assert_eq!(
            about.line(false),
            "an update at 22ee18c: 3 of 64 subsections written in 12 sections ($0.84, 95s)"
        );
        let failed = About {
            failed: Some("the build reached its budget of $30.00".into()),
            update: false,
            ..about
        };
        assert_eq!(
            failed.line(false),
            "a build at 22ee18c stopped: the build reached its budget of $30.00 ($0.84)"
        );
    }
}
