//! `crystal open`: files shown to the user in the TUI they used last, so
//! that an agent asked to show one puts it in front of them rather than
//! pasting it into its answer: a view of their own over the tabs, the one
//! the bar is on read beside the list, a markdown file as its page with its
//! mermaid diagrams drawn, and Enter opening it in their `$EDITOR`. The
//! order goes as the layout commands' do, through the daemon to the TUI
//! used last ([`crate::layout_relay`]); with no TUI open, nothing is shown,
//! and the command fails saying so, for the agent to name the paths
//! instead. Adapted from docket's file tabs.
//!
//! Text files only: a TUI draws text, so an image, a PDF or any other
//! binary is refused here, before anything is sent.

use crate::client;
use crate::git::Checkout;
use crate::layout::Command;
use crate::outln;
use crate::tui::preview;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Shows `files`, from the directory it runs in or absolute, in the TUI
/// used last, and says so to whoever ran it: most often an agent, which is
/// told not to paste them into its answer as well.
pub fn run(socket: &Path, files: &[PathBuf]) -> Result<()> {
    let cwd = std::env::current_dir().context("couldn't tell the current directory")?;
    let mut files: Vec<PathBuf> = (files.iter())
        .map(|file| checked(&cwd, file))
        .collect::<Result<_>>()?;
    // A file named twice is shown once.
    let mut named = HashSet::new();
    files.retain(|file| named.insert(file.clone()));
    let count = files.len();
    let dir = shown_from(&cwd);
    client::lay_out(socket, Command::Open { dir, files }).context("nothing was opened")?;
    outln!("{}", opened(count))?;
    Ok(())
}

/// The file at `file`, from `cwd` or absolute, made absolute, once it's
/// a text file a TUI can show.
fn checked(cwd: &Path, file: &Path) -> Result<PathBuf> {
    let shown = file.display();
    let path =
        std::fs::canonicalize(cwd.join(file)).with_context(|| format!("can't open {shown}"))?;
    if !path.is_file() {
        bail!("can't open {shown}: it isn't a file");
    }
    let mut head = Vec::new();
    std::fs::File::open(&path)
        .and_then(|opened| {
            opened
                .take(preview::BINARY_TEST_BYTES as u64)
                .read_to_end(&mut head)
        })
        .with_context(|| format!("can't open {shown}"))?;
    if preview::is_binary(&head) {
        bail!(
            "can't open {shown}: crystal shows text files only, not images, PDFs or other \
             binary files; name its path instead"
        );
    }
    Ok(path)
}

/// Where files opened from `cwd` are shown from, and where an editor
/// opening one starts: the top of the git worktree it's in, or itself.
fn shown_from(cwd: &Path) -> PathBuf {
    let top = match Checkout::find(cwd) {
        Some(checkout) => checkout.worktree().path,
        None => cwd.to_path_buf(),
    };
    // As the files are, so that theirs start with it.
    std::fs::canonicalize(&top).unwrap_or(top)
}

/// What the command says once `count` files are shown.
fn opened(count: usize) -> String {
    let (files, them) = match count {
        1 => ("1 file".to_string(), "it"),
        count => (format!("{count} files"), "them"),
    };
    format!(
        "showing {files} in crystal's TUI, where the user reads {them}: don't paste {them} into \
         your answer as well"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_file_is_made_absolute_and_anything_else_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(cwd.join("docs")).unwrap();
        std::fs::write(cwd.join("docs/plan.md"), "# Plan\n").unwrap();
        std::fs::write(cwd.join("empty.txt"), "").unwrap();
        std::fs::write(cwd.join("logo.png"), b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();

        let plan = checked(&cwd, Path::new("docs/plan.md")).unwrap();
        assert_eq!(plan, cwd.join("docs/plan.md"));
        assert_eq!(checked(&cwd, &plan).unwrap(), plan, "absolute as it is");
        assert!(
            checked(&cwd, Path::new("empty.txt")).is_ok(),
            "empty is text"
        );

        let said = |file: &str| format!("{:#}", checked(&cwd, Path::new(file)).unwrap_err());
        assert!(
            said("logo.png").contains("text files only"),
            "{}",
            said("logo.png")
        );
        assert!(said("docs").contains("it isn't a file"), "{}", said("docs"));
        assert!(
            said("gone.md").starts_with("can't open gone.md: "),
            "{}",
            said("gone.md")
        );
    }

    #[test]
    fn files_are_shown_from_the_top_of_their_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let top = std::fs::canonicalize(dir.path()).unwrap();
        let below = top.join("src/deep");
        std::fs::create_dir_all(&below).unwrap();
        assert_eq!(shown_from(&below), below, "outside git, where it runs");
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&top)
            .status();
        if init.is_ok_and(|status| status.success()) {
            assert_eq!(shown_from(&below), top);
        }
    }

    #[test]
    fn it_tells_the_agent_the_user_has_them() {
        assert_eq!(
            opened(1),
            "showing 1 file in crystal's TUI, where the user reads it: don't paste it into your \
             answer as well"
        );
        assert!(
            opened(3).starts_with("showing 3 files in crystal's TUI, where the user reads them")
        );
    }
}
