//! `crystal remember` and `crystal memory`: adding to a project's memory,
//! and reading, searching and tidying it, from a shell or from an agent in
//! a session.

use crate::config::Config;
use crate::env;
use crate::git::Checkout;
use crate::memory::{self, Kind, Listed, Memory, Source};
use crate::tui::sidebar::ago;
use anyhow::{Result, bail};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Remembers `text` for the project `dir` is in (the current directory
/// without one), about `files`, and says which entry it is.
pub fn remember(
    socket: &Path,
    dir: Option<PathBuf>,
    kind: Kind,
    files: Vec<String>,
    text: &str,
) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let project = memory::project_of(&dir);
    let files = files
        .iter()
        .map(|file| from_worktree_top(file, &dir))
        .collect();
    let entry = memory::add(socket, &project, kind, text, files, source(socket))?;
    println!("remembered {}", entry.id);
    Ok(())
}

/// Prints the project's memory, newest first.
pub fn list(socket: &Path, dir: Option<PathBuf>) -> Result<()> {
    let memory = read(socket, dir)?;
    print_entries(&memory.listed().iter().collect::<Vec<_>>());
    Ok(())
}

/// Prints the entries that have to do with `words`, the best first.
pub fn search(socket: &Path, dir: Option<PathBuf>, words: &[String]) -> Result<()> {
    let memory = read(socket, dir)?;
    let listed = memory.listed();
    print_entries(&memory::search(&listed, &words.join(" "), now()));
    Ok(())
}

pub fn remove(socket: &Path, dir: Option<PathBuf>, id: u64) -> Result<()> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    let entry = memory::remove(socket, &project, id)?;
    println!("forgot {}: {}", entry.id, entry.text);
    Ok(())
}

/// Writes entry `id` into the project's CLAUDE.md or AGENTS.md, after the
/// user says yes, unless `yes` says it already.
pub fn promote(socket: &Path, dir: Option<PathBuf>, id: u64, yes: bool) -> Result<()> {
    let memory = read(socket, dir)?;
    let Some(entry) = memory.get(id) else {
        bail!("there's no entry {id}");
    };
    if !yes
        && !confirm(&format!(
            "Add entry {id} to the project's instructions file?"
        ))?
    {
        println!("left as it was");
        return Ok(());
    }
    let file = memory::promote(&memory.project, entry)?;
    println!("added entry {id} to {}", file.display());
    Ok(())
}

fn read(socket: &Path, dir: Option<PathBuf>) -> Result<Memory> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    Memory::read(socket, &project)
}

/// Every memory command goes through here first: with memory off, they
/// say so rather than doing anything.
fn check_on() -> Result<()> {
    crate::plugins::ensure_enabled(&Config::load()?, "memory")
}

fn dir_or_current(dir: Option<PathBuf>) -> Result<PathBuf> {
    match dir {
        Some(dir) => Ok(std::path::absolute(dir)?),
        None => Ok(std::env::current_dir()?),
    }
}

/// Who's remembering: the session this runs in, when it runs in one of
/// this daemon's, or else the user.
fn source(socket: &Path) -> Source {
    let session = std::env::var("CRYSTAL_SESSION").ok();
    match (env::own_session_id(socket), session) {
        (Some(_), Some(name)) => Source::Session(name),
        _ => Source::User,
    }
}

/// `file`, given from `dir`, from the top of its worktree: the same in
/// every worktree of the project. A file outside the worktree stays as it
/// was given.
fn from_worktree_top(file: &str, dir: &Path) -> String {
    let path = dir.join(file);
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let top = Checkout::find(dir).map_or_else(|| dir.to_path_buf(), |c| c.worktree().path);
    match path.strip_prefix(&top) {
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => file.to_string(),
    }
}

/// One line an entry: its id, kind and age, then its text, marked when
/// it's stale.
fn print_entries(entries: &[&Listed]) {
    let now = now();
    for item in entries {
        let entry = &item.entry;
        let stale = if item.stale { "  [stale]" } else { "" };
        let files = if entry.files.is_empty() {
            String::new()
        } else {
            format!("  ({})", entry.files.join(", "))
        };
        println!(
            "{:>4}  {:<8}  {:>4}  {}{files}{stale}",
            entry.id,
            entry.kind.to_string(),
            ago(entry.created, now),
            entry.text.split_whitespace().collect::<Vec<_>>().join(" "),
        );
    }
}

/// Asks `question` at the terminal, and says whether the answer was yes.
/// Away from a terminal there's nobody to answer, so it's an error.
pub fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("not at a terminal to ask: add --yes to go ahead");
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn now() -> u64 {
    memory::seconds_since_epoch(SystemTime::now())
}
