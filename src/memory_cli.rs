//! `crystal remember` and `crystal memory`: adding to a project's memory,
//! and reading, searching and tidying it, from a shell or from an agent in
//! a session.

use crate::client;
use crate::config::Config;
use crate::embed::{self, Models};
use crate::env;
use crate::events::{self, Event};
use crate::git::Checkout;
use crate::memory::{
    self, Added, Entry, Forgotten, Kind, Listed, Memory, New, Source, Store, Wanted,
};
use crate::output::{err, errln, out, outln};
use crate::printable;
use crate::protocol::{Request, Response};
use crate::tui::sidebar::ago;
use anyhow::{Result, bail};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Remembers `text` for the project `dir` is in (the current directory
/// without one), about `files`, under `title` if it's given, and says which
/// entry it is.
pub fn remember(
    socket: &Path,
    dir: Option<PathBuf>,
    kind: Kind,
    files: Vec<String>,
    title: Option<String>,
    text: &str,
) -> Result<()> {
    check_on()?;
    let text = match title {
        Some(title) => memory::titled(&title, text)?,
        None => text.to_string(),
    };
    let dir = dir_or_current(dir)?;
    let project = memory::project_of(&dir);
    let top = top_of(&dir);
    let files = files
        .iter()
        .map(|file| from_top(file, &dir, &top))
        .collect();
    let new = New {
        kind,
        text,
        files,
        source: source(socket),
        checkout: Some(top),
    };
    match memory::add(socket, &project, new)? {
        Added::New(entry) => {
            outln!("remembered {}", entry.id)?;
            tell(
                socket,
                Event::memory(events::Kind::MemoryAdded, project, entry),
            );
        }
        Added::Again(entry) => outln!("remembered {} already", entry.id)?,
        Added::Refused => unreachable!("only crystal is refused what was forgotten"),
    }
    Ok(())
}

/// Which of a project's entries `crystal memory list` lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Listing {
    /// Of this kind alone.
    pub kind: Option<Kind>,
    /// What was forgotten instead, the latest forgotten first.
    pub forgotten: bool,
    /// Those that read as status rather than lessons: see [`is_status`].
    pub status: bool,
    /// The expired alone.
    pub expired: bool,
}

/// Prints the project's memory, newest first, as `listing` says.
pub fn list(socket: &Path, dir: Option<PathBuf>, listing: Listing) -> Result<()> {
    let kind = listing.kind;
    if listing.forgotten {
        check_on()?;
        let project = memory::project_of(&dir_or_current(dir)?);
        let forgotten = Store::open(socket)?.forgotten(&project)?;
        let forgotten = forgotten
            .iter()
            .filter(|entry| kind.is_none_or(|kind| entry.kind == kind));
        for entry in forgotten {
            outln!("{}", printable::line(&forgotten_line(entry, now())))?;
        }
        return Ok(());
    }
    let memory = read(socket, dir)?;
    let listed = memory.listed();
    let now = now();
    let listed: Vec<&Listed> = listed
        .iter()
        .filter(|item| kind.is_none_or(|kind| item.entry.kind == kind))
        .filter(|item| !listing.status || is_status(&item.entry))
        .filter(|item| !listing.expired || item.entry.expired(now))
        .collect();
    print_entries(&listed)
}

/// Whether `entry` reads as progress or status rather than a lesson, by
/// [`memory::reads_as_status`]: tasks' outcomes, all status, have a kind
/// of their own to list them by, and expire, so they aren't counted.
fn is_status(entry: &Entry) -> bool {
    entry.kind != Kind::Outcome && memory::reads_as_status(&entry.text)
}

/// Prints entry `id` in full. An agent in a session reading it uses it,
/// which keeps it from expiring.
pub fn show(socket: &Path, dir: Option<PathBuf>, id: u64) -> Result<()> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    let mut store = Store::open(socket)?;
    let entry = match env::own_session_id(socket) {
        Some(_) => store.used(&project, id)?,
        None => store.get(&project, id)?,
    };
    let Some(entry) = entry else {
        bail!("there's no entry {id}");
    };
    let item = memory::checked(entry, &project);
    outln!("{}", in_full(&item, now()))?;
    Ok(())
}

/// Prints the project's memory as markdown, newest first.
pub fn export(socket: &Path, dir: Option<PathBuf>) -> Result<()> {
    let memory = read(socket, dir)?;
    let name = memory.project.file_name().unwrap_or_default();
    out!(
        "{}",
        memory::markdown(&name.to_string_lossy(), &memory.listed())
    )?;
    Ok(())
}

/// What `crystal memory search` keeps to besides its words.
#[derive(Debug, Default)]
pub struct SearchArgs {
    pub kind: Option<Kind>,
    /// Files or directories, as given from the current directory.
    pub files: Vec<String>,
    /// Leave the stale out.
    pub fresh: bool,
    /// The expired too.
    pub all: bool,
    pub limit: Option<usize>,
}

/// Prints the entries that have to do with `words`, the best first, as
/// `args` says: those that hold before the stale, or with `fresh`, the
/// stale left out; the expired left out unless it says all.
pub fn search(
    socket: &Path,
    dir: Option<PathBuf>,
    words: &[String],
    args: SearchArgs,
) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let settings = Config::load()?.memory;
    let downloaded = embed::models_dir().is_some_and(|dir| embed::is_downloaded(&dir));
    if settings.embeddings && !downloaded {
        errln!(
            "the models that search by meaning aren't downloaded yet, so this goes by words \
             alone: the daemon gets them as it starts, or `crystal memory embed` does now"
        );
    }
    let top = top_of(&dir);
    let wanted = Wanted {
        kind: args.kind,
        files: args
            .files
            .iter()
            .map(|file| from_top(file, &dir, &top))
            .collect(),
        fresh: args.fresh,
        expired: args.all,
        limit: args.limit.unwrap_or(memory::SEARCH_LIMIT).max(1),
    };
    let found = found(socket, &dir, &words.join(" "), &wanted)?;
    print_entries(&found.iter().collect::<Vec<_>>())
}

/// The entries of the memory of the project `dir` is in that have to do
/// with `query`, as `wanted` says, the best first: searched by the daemon,
/// which keeps the model that searches by meaning loaded, or here, when
/// there's no daemon to ask.
pub fn found(socket: &Path, dir: &Path, query: &str, wanted: &Wanted) -> Result<Vec<Listed>> {
    let request = Request::SearchMemory {
        dir: dir.to_path_buf(),
        query: query.to_string(),
        wanted: wanted.clone(),
    };
    if let Ok(Some(Response::Memory { entries })) = client::ask(socket, &request, false) {
        return Ok(entries);
    }
    let project = memory::project_of(dir);
    let embedder = embed::shared_now();
    let mut store = Store::open(socket)?;
    store.find(&project, query, wanted, embed::as_embed(&embedder))
}

/// Which entries `crystal memory rm` forgets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forgetting {
    /// These, by their ids.
    pub ids: Vec<u64>,
    /// Every one that reads as status, as `list --status` lists them, of
    /// `kind` if it's given.
    pub status: bool,
    pub kind: Option<Kind>,
    /// With `status`, forget them: without it, they're only listed, as what
    /// would be forgotten.
    pub yes: bool,
}

/// Forgets the entries `forgetting` says, and says which: each of its ids,
/// once every one is there, or with `status`, every entry that reads as
/// status, listed first and forgotten only once it says yes.
pub fn remove(socket: &Path, dir: Option<PathBuf>, forgetting: Forgetting) -> Result<()> {
    check_on()?;
    let project = memory::project_of(&dir_or_current(dir)?);
    let mut store = Store::open(socket)?;
    let ids = if forgetting.status {
        let listed = memory::marked(store.entries(&project)?, &project);
        let listed: Vec<&Listed> = listed
            .iter()
            .filter(|item| forgetting.kind.is_none_or(|kind| item.entry.kind == kind))
            .filter(|item| is_status(&item.entry))
            .collect();
        if !forgetting.yes {
            print_entries(&listed)?;
            match listed.len() {
                0 => outln!("nothing reads as status")?,
                n => outln!("would forget these {n}: add --yes to forget them")?,
            }
            return Ok(());
        }
        listed.iter().map(|item| item.entry.id).collect()
    } else {
        let mut missing = Vec::new();
        for &id in &forgetting.ids {
            if store.get(&project, id)?.is_none() {
                missing.push(id.to_string());
            }
        }
        if !missing.is_empty() {
            bail!("there's no entry {}", missing.join(", "));
        }
        forgetting.ids
    };
    for id in ids {
        let entry = store.remove(&project, id)?;
        outln!("forgot {}: {}", entry.id, entry.text)?;
        let forgotten = Event::memory(events::Kind::MemoryForgotten, project.clone(), entry);
        tell(socket, forgotten);
    }
    Ok(())
}

/// Tells the daemon what was done to the memory, for its event log. The
/// memory has it either way, so a daemon that can't be told is no reason
/// to fail.
fn tell(socket: &Path, event: Event) {
    if let Err(err) = client::tell(socket, event) {
        errln!("crystal: couldn't tell the daemon: {err:#}");
    }
}

/// Downloads the models that search by meaning if they aren't here yet,
/// then gives every entry of every project its vector, and says how many
/// that was.
pub fn embed(socket: &Path) -> Result<()> {
    check_on()?;
    let root = match embed::models_dir().filter(|root| embed::is_downloaded(root)) {
        Some(root) => root,
        None => {
            errln!("downloading {} ({} MB)", embed::names(), embed::size_mb());
            embed::download(std::io::stderr().is_terminal())?
        }
    };
    // Embedding needs only the one model.
    let models = Models::load(&root, false)?;
    let count = Store::open(socket)?.embed_missing(&models)?;
    outln!("embedded {count} entries with {}", embed::MODEL)?;
    if !Config::load()?.memory.embeddings {
        outln!(
            "searches use it once `embeddings = true` is under `[memory]` in {}",
            crate::config::path().display()
        )?;
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
    outln!("distilled {name}: {}", report.line())?;
    for why in &report.rejected {
        outln!("  rejected {why}")?;
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
        outln!("left as it was")?;
        return Ok(());
    }
    let file = memory::promote(&memory.project, entry)?;
    outln!("added entry {id} to {}", file.display())?;
    let promoted = Event::promoted(memory.project.clone(), entry.clone(), file);
    tell(socket, promoted);
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

/// The top of the worktree `dir` is in, or `dir` itself outside git.
fn top_of(dir: &Path) -> PathBuf {
    Checkout::find(dir).map_or_else(|| dir.to_path_buf(), |c| c.worktree().path)
}

/// `file`, given from `dir`, from `top`, the top of its worktree: the same
/// in every worktree of the project. A file outside the worktree stays as
/// it was given.
fn from_top(file: &str, dir: &Path, top: &Path) -> String {
    let path = dir.join(file);
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    match path.strip_prefix(top) {
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => file.to_string(),
    }
}

/// One line an entry: its id, kind and age, then its text, marked when
/// it's drifting, stale or expired.
fn print_entries(entries: &[&Listed]) -> Result<()> {
    let now = now();
    for item in entries {
        let entry = &item.entry;
        let expired = entry.expired(now).then_some("expired");
        let marks = item.freshness.mark().into_iter().chain(expired);
        let mark: String = marks.map(|mark| format!("  [{mark}]")).collect();
        let files = if entry.files.is_empty() {
            String::new()
        } else {
            format!("  ({})", entry.files.join(", "))
        };
        let line = format!(
            "{:>4}  {:<8}  {:>4}  {}{files}{mark}",
            entry.id,
            entry.kind.to_string(),
            ago(entry.created, now),
            memory::title(&entry.text),
        );
        // Its text is kept clean, but not its files, nor what was kept
        // before that.
        outln!("{}", printable::line(&line))?;
    }
    Ok(())
}

/// A forgotten entry on one line: the id it had, its kind, how long ago it
/// was forgotten, its title and its files.
fn forgotten_line(entry: &Forgotten, now: u64) -> String {
    let files = if entry.files.is_empty() {
        String::new()
    } else {
        format!("  ({})", entry.files.join(", "))
    };
    format!(
        "{:>4}  {:<8}  {:>4}  {}{files}",
        entry.id,
        entry.kind.to_string(),
        ago(entry.forgotten, now),
        memory::title(&entry.text),
    )
}

/// An entry in full, as `crystal memory show` and the `memory_show` tool
/// give it: its id and kind, how it holds when it's drifting or stale, and
/// what's gone, whether it's expired, its text, its files, where it came
/// from, how often and how lately it was said, and when an agent last read
/// it in full.
pub fn in_full(item: &Listed, now: u64) -> String {
    let entry = &item.entry;
    let mut text = format!("{} · {}", entry.id, entry.kind);
    if let Some(holds) = item.how_it_holds() {
        text.push_str(&format!(" · {holds}"));
    }
    if entry.expired(now) {
        text.push_str(" · expired: nobody has found it again, so searches leave it out");
    }
    text.push_str(&format!("\n\n{}\n", entry.text));
    if !entry.files.is_empty() {
        text.push_str(&format!("\nfiles: {}", entry.files.join(", ")));
    }
    text.push_str(&format!("\nfrom: {}", entry.source));
    let times = if entry.seen == 1 {
        "once".to_string()
    } else {
        format!("{} times", entry.seen)
    };
    text.push_str(&format!(
        "\nadded {} ago; said {times}, last {} ago",
        ago(entry.created, now),
        ago(entry.last_seen, now)
    ));
    if let Some(used) = entry.used {
        let when = match ago(used, now).as_str() {
            "now" => "just now".to_string(),
            age => format!("{age} ago"),
        };
        text.push_str(&format!("; an agent read it in full {when}"));
    }
    printable::text(&text).into_owned()
}

/// Asks `question` at the terminal, and says whether the answer was yes.
/// Away from a terminal there's nobody to answer, so it's an error.
pub fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("not at a terminal to ask: add --yes to go ahead");
    }
    err!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn now() -> u64 {
    memory::seconds_since_epoch(SystemTime::now())
}
