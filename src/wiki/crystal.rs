//! Everything the wiki takes from the rest of crystal, in one place: its
//! settings and whether it's on, where the server keeps its state, which
//! project a directory is in and the forge it's on, taking credentials out
//! of text, reading a mermaid diagram, crystal's version, telling the
//! daemon what happened, and printing. The rest of `src/wiki/` reaches
//! crystal through here alone.

use super::About;
use crate::config::Config;

pub use crate::config::WikiSettings;
use crate::events::Event;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub(crate) use crate::output::{errln, outln};

/// Whether the wiki plugin is on: the one gate everything it adds goes
/// through.
pub fn enabled() -> Result<bool> {
    Ok(crate::plugins::enabled(&Config::load()?, "wiki"))
}

/// What a command says when the wiki plugin is off.
pub fn off() -> String {
    crate::plugins::off("wiki")
}

/// The wiki's settings, `[wiki]` in the config file.
pub fn settings() -> Result<WikiSettings> {
    Ok(Config::load()?.wiki)
}

/// Where the daemon at `socket` keeps every project's wiki.
pub fn wikis_dir(socket: &Path) -> PathBuf {
    crate::state::wikis_dir(socket)
}

/// The name a project's state is kept under: its directory's name and a
/// hash of its path.
pub fn project_key(project: &Path) -> String {
    crate::state::project_slug(project)
}

/// The main worktree of the git repository `dir` is in, or `None` outside
/// one.
pub fn project_of(dir: &Path) -> Option<PathBuf> {
    crate::git::Checkout::find(dir)?;
    Some(crate::project::of(dir).path)
}

/// The forges a wiki links into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    GitLab,
}

/// The forge the project at `project`'s remote is on, by its host: GitHub
/// and GitLab's own, and the hosts `gh` and `glab` are logged in to.
pub fn forge_of(project: &Path) -> Option<Forge> {
    match crate::forge::Repo::find(project).ok()?.forge {
        crate::forge::Forge::GitHub => Some(Forge::GitHub),
        crate::forge::Forge::GitLab => Some(Forge::GitLab),
    }
}

/// `text` with what looks like a credential taken out.
pub fn redact(text: &str) -> String {
    crate::secrets::redact(text)
}

/// Whether the mermaid diagram `source` reads, or why it doesn't, as
/// crystal's own reader reads it.
pub fn mermaid_reads(source: &str) -> Result<(), String> {
    // Wide enough that no diagram is refused for its width alone.
    match crate::mermaid::render(source, 4000, crate::mermaid::Glyphs::ASCII) {
        crate::mermaid::Rendered::Diagram { .. } => Ok(()),
        crate::mermaid::Rendered::Unsupported { reason } => Err(reason),
    }
}

/// How long ago `then` was, at `now`, both in seconds since the Unix
/// epoch, as crystal says it: `5m`, `2h`, `3d`.
pub fn ago(then: u64, now: u64) -> String {
    crate::tui::sidebar::ago(then, now)
}

/// The version of this crystal.
pub fn version() -> String {
    crate::protocol::version()
}

/// Tells the daemon at `socket` that a build of `project`'s wiki started,
/// or with `ended`, ended as `about` says: the events `wiki.started`,
/// `wiki.built` and `wiki.failed`.
pub fn tell(socket: &Path, project: &Path, about: &About, ended: bool) {
    let project = project.to_path_buf();
    let event = if ended {
        Event::wiki_ended(project, about.clone())
    } else {
        Event::wiki_started(project, about.clone())
    };
    // With no daemon running, there's nobody to tell.
    let _ = crate::client::tell(socket, event);
}
