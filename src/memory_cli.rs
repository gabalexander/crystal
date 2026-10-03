//! `crystal remember` and `crystal memory`: adding to a project's memory,
//! and reading, searching and tidying it, from a shell or from an agent in
//! a session.

use crate::client;
use crate::config::Config;
use crate::embed::{self, Embedder};
use crate::env;
use crate::events::{self, Event};
use crate::git::Checkout;
use crate::memory::{self, Added, Kind, Listed, Memory, New, Source, Store};
use crate::protocol::{Request, Response};
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
    let new = New {
        kind,
        text: text.to_string(),
        files,
        source: source(socket),
    };
    match memory::add(socket, &project, new)? {
        Added::New(entry) => {
            println!("remembered {}", entry.id);
            tell(
                socket,
                Event::memory(events::Kind::MemoryAdded, project, entry),
            );
        }
        Added::Again(entry) => println!("remembered {} already", entry.id),
        Added::Refused => unreachable!("only crystal is refused what was forgotten"),
    }
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
    check_on()?;
    let dir = dir_or_current(dir)?;
    let settings = Config::load()?.memory;
    let downloaded = embed::model_dir().is_some_and(|dir| embed::is_downloaded(&dir));
    if settings.embeddings && !downloaded {
        eprintln!(
            "{} isn't downloaded, so this goes by words alone: `crystal memory embed` gets it",
            embed::MODEL
        );
    }
    let found = found(socket, &dir, &words.join(" "), None, memory::SEARCH_LIMIT)?;
    print_entries(&found.iter().collect::<Vec<_>>());
    Ok(())
}

/// The entries of the memory of the project `dir` is in that have to do
/// with `query`, of `kind` if it's given, the best first: searched by the
/// daemon, which keeps the model that searches by meaning loaded, or here,
/// when there's no daemon to ask.
pub fn found(
    socket: &Path,
    dir: &Path,
    query: &str,
    kind: Option<Kind>,
    limit: usize,
) -> Result<Vec<Listed>> {
    let request = Request::SearchMemory {
        dir: dir.to_path_buf(),
        query: query.to_string(),
        kind,
        limit,
    };
    if let Ok(Some(Response::Memory { entries })) = client::ask(socket, &request, false) {
        return Ok(entries);
    }
    let project = memory::project_of(dir);
    let embedder = embed::shared_now();
    let mut store = Store::open(socket)?;
    let found = store.search(&project, query, kind, limit, embed::as_embed(&embedder))?;
    Ok(memory::stale_last(found, &project))
}

pub fn remove(socket: &Path, dir: Option<PathBuf>, id: u64) -> Result<()> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    let entry = memory::remove(socket, &project, id)?;
    println!("forgot {}: {}", entry.id, entry.text);
    tell(
        socket,
        Event::memory(events::Kind::MemoryForgotten, project, entry),
    );
    Ok(())
}

/// Tells the daemon what was done to the memory, for its event log. The
/// memory has it either way, so a daemon that can't be told is no reason
/// to fail.
fn tell(socket: &Path, event: Event) {
    if let Err(err) = client::tell(socket, event) {
        eprintln!("crystal: couldn't tell the daemon: {err:#}");
    }
}

/// Downloads the embedding model if it isn't here yet, then gives every
/// entry of every project its vector, and says how many that was.
pub fn embed(socket: &Path) -> Result<()> {
    check_on()?;
    let dir = match embed::model_dir().filter(|dir| embed::is_downloaded(dir)) {
        Some(dir) => dir,
        None => {
            eprintln!("downloading {} ({} MB)", embed::MODEL, embed::size_mb());
            embed::download(std::io::stderr().is_terminal())?
        }
    };
    let embedder = Embedder::load(&dir)?;
    let count = Store::open(socket)?.embed_missing(&embedder)?;
    println!("embedded {count} entries with {}", embed::MODEL);
    if !Config::load()?.memory.embeddings {
        println!(
            "searches use it once `embeddings = true` is under `[memory]` in {}",
            crate::config::path().display()
        );
    }
    Ok(())
}

/// Has the daemon run the distiller over what the session `name` did, and
/// says what came of it.
pub fn distill(socket: &Path, name: &str) -> Result<()> {
    check_on()?;
    let request = Request::Distill {
        name: name.to_string(),
    };
    let Some(Response::Distilled(report)) = client::ask(socket, &request, false)? else {
        bail!("there's no session called {name}: no daemon is running");
    };
    println!("distilled {name}: {}", report.line());
    for why in &report.rejected {
        println!("  rejected {why}");
    }
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
