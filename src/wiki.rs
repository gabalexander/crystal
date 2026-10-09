//! A project's wiki: one page about its code, written by `crystal wiki
//! build` into a directory of the project's own in the server's state,
//! which `crystal wiki serve` serves and `crystal wiki export` writes out
//! as a site (see [`crate::wiki_server`] and [`crate::wiki_site`]).
//!
//! In each project's directory: `wiki.json`, the page's text and diagrams
//! at a commit; `build.json`, how the build that writes it is going; and
//! `build.log`.

use crate::state;
use std::path::{Path, PathBuf};

/// The wiki's file, in its directory.
pub const WIKI_FILE: &str = "wiki.json";

/// The build's own bookkeeping, in its directory.
pub const BUILD_FILE: &str = "build.json";

/// Where the daemon at `socket` keeps the wiki of the project whose main
/// worktree is `project`: a directory named as the project's backlog's
/// was, its name and a hash of its path.
pub fn dir(socket: &Path, project: &Path) -> PathBuf {
    state::wikis_dir(socket).join(state::project_slug(project))
}
