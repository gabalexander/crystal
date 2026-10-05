//! `crystal remember` and `crystal memory`: adding to a project's memory,
//! and reading, searching and tidying it, from a shell or from an agent in
//! a session.

use crate::client;
use crate::config::Config;
use crate::distill;
use crate::embed::{self, Models};
use crate::env;
use crate::events::{self, Event};
use crate::git::Checkout;
use crate::memory::{
    self, Added, Entry, Forgotten, Kind, Listed, Memory, Merge, New, Source, Store, Wanted,
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
    match added(socket, &project, new)? {
        Added::New(entry) => outln!("remembered {}", entry.id)?,
        Added::Again(entry) => outln!("remembered {} already", entry.id)?,
        Added::Alike(entry) => outln!(
            "remembered {} already, in other words: {}",
            entry.id,
            printable::line(&memory::title(&entry.text))
        )?,
        Added::Refused => unreachable!("only crystal is refused what was forgotten"),
    }
    Ok(())
}

/// Adds `new` to `project`'s memory: by the daemon, which keeps the models
/// that find it said already in other words loaded, and tells of a new
/// entry; or here, when there's no daemon to ask.
fn added(socket: &Path, project: &Path, new: New) -> Result<Added> {
    let request = Request::Remember {
        project: project.to_path_buf(),
        entry: new.clone(),
    };
    if let Ok(Some(Response::Remembered(added))) = client::ask(socket, &request, false) {
        return Ok(added);
    }
    let embedder = embed::shared_now();
    let added = Store::open(socket)?.add_with(project, new, embed::as_embed(&embedder))?;
    if let Added::New(entry) = &added {
        let event = Event::memory(
            events::Kind::MemoryAdded,
            project.to_path_buf(),
            entry.clone(),
        );
        tell(socket, event);
    }
    Ok(added)
}

/// Lists the entries of the project's memory that say what another does,
/// each group under the one it keeps, as `crystal memory dedupe` finds
/// them, and with `apply`, merges them; found and merged by the daemon,
/// which keeps the models loaded, or here, when there's no daemon to ask.
pub fn dedupe(socket: &Path, dir: Option<PathBuf>, apply: bool) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let request = Request::DedupeMemory {
        dir: dir.clone(),
        apply,
    };
    let merges = match client::ask(socket, &request, false) {
        Ok(Some(Response::Deduped { merges })) => merges,
        Ok(_) => {
            let project = memory::project_of(&dir);
            let embedder = embed::shared_now();
            let merges = memory::dedupe(socket, &project, embed::as_embed(&embedder), apply)?;
            if apply {
                for merge in &merges {
                    let ids: Vec<u64> = merge.merged.iter().map(|twin| twin.entry.id).collect();
                    tell(
                        socket,
                        Event::merged(project.clone(), merge.kept.clone(), &ids),
                    );
                }
            }
            merges
        }
        Err(err) => return Err(err),
    };
    out!("{}", merges_text(&merges, apply))?;
    Ok(())
}

/// What `crystal memory dedupe` prints of `merges`: each one kept, by its
/// id, kind and title, and under it each that goes into it, with how alike
/// they are; then how many went, or would go with `--apply`.
fn merges_text(merges: &[Merge], applied: bool) -> String {
    let line = |entry: &Entry| {
        printable::line(&format!(
            "{:>4}  {:<8}  {}",
            entry.id,
            entry.kind.to_string(),
            memory::title(&entry.text)
        ))
        .into_owned()
    };
    let mut text = String::new();
    for merge in merges {
        text.push_str(&format!("{}\n", line(&merge.kept)));
        for twin in &merge.merged {
            text.push_str(&format!("  ← {:.2}  {}\n", twin.alike, line(&twin.entry)));
        }
    }
    let merged: usize = merges.iter().map(|merge| merge.merged.len()).sum();
    let entries = if merged == 1 { "entry" } else { "entries" };
    let into = merges.len();
    text.push_str(&match (merged, applied) {
        (0, _) => "no two entries say the same thing\n".to_string(),
        (_, true) => format!("merged {merged} {entries} into {into}\n"),
        (_, false) => format!("{merged} {entries} would go into {into}: --apply merges them\n"),
    });
    text
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
        bail!("{}", no_entry(socket, &project, id));
    };
    let item = memory::checked(entry, &project);
    outln!("{}", in_full(&item, now()))?;
    Ok(())
}

/// Why there's no entry `id` in `project`'s memory to show: none was ever
/// that, or it was merged into another, which says the same thing.
pub fn no_entry(socket: &Path, project: &Path, id: u64) -> String {
    let merged = Store::open(socket).and_then(|mut store| store.merged_into(project, id));
    match merged {
        Ok(Some(kept)) => format!("entry {id} was merged into {kept}, which says the same thing"),
        _ => format!("there's no entry {id}"),
    }
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

/// What `crystal memory kind` changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kinding {
    /// The kind to give `ids`.
    pub kind: Option<Kind>,
    pub ids: Vec<u64>,
    /// Have the distiller's model read the notes for the lessons among
    /// them instead.
    pub notes: bool,
    /// With `notes`, give each the kind it says: without it, they're only
    /// listed, as what would change.
    pub yes: bool,
}

/// Changes the kind of the entries `kinding` says, and says which: each of
/// its ids, once every one is there, given its kind; or with `notes`, the
/// notes the distiller's model reads as lessons, listed with the kind it
/// gives each, and given it only once it says yes. What an entry says, its
/// vector and its place in the index stay as they are.
pub fn kind(socket: &Path, dir: Option<PathBuf>, kinding: Kinding) -> Result<()> {
    check_on()?;
    let dir = dir_or_current(dir)?;
    let project = memory::project_of(&dir);
    let mut store = Store::open(socket)?;
    let changes: Vec<(u64, Kind)> = if kinding.notes {
        // Those that read as status are no lessons: `list --status` has them.
        let (status, notes): (Vec<Entry>, Vec<Entry>) = store
            .entries(&project)?
            .into_iter()
            .filter(|entry| entry.kind == Kind::Note)
            .partition(|entry| memory::reads_as_status(&entry.text));
        if notes.is_empty() {
            outln!("there are no notes but those that read as status")?;
            return Ok(());
        }
        let settings = Config::load()?.memory;
        let count = notes.len();
        let model = &settings.distill_model;
        errln!(
            "asking {model} which of the {count} notes are lessons ({} that read as status left \
             out: `list --status` lists them)…",
            status.len()
        );
        let env = std::env::vars().collect();
        let read = distill::lessons_among(&notes, &settings, &top_of(&dir), &env)?;
        for why in &read.rejected {
            errln!("  rejected {why}");
        }
        errln!("(${:.4})", read.cost_usd);
        if !kinding.yes {
            for &(id, kind) in &read.lessons {
                let note = notes.iter().find(|note| note.id == id);
                let title = note
                    .map(|note| memory::title(&note.text))
                    .unwrap_or_default();
                let line = format!("{id:>4}  note → {:<8}  {title}", kind.to_string());
                outln!("{}", printable::line(&line))?;
            }
            match read.lessons.len() {
                0 => outln!("none of the notes reads as a lesson")?,
                1 => outln!("would make this note a lesson: add --yes to make it one")?,
                n => outln!("would make these {n} notes lessons: add --yes to make them")?,
            }
            return Ok(());
        }
        read.lessons
    } else {
        let kind = kinding.kind.expect("clap asks for a kind with ids");
        let mut missing = Vec::new();
        for &id in &kinding.ids {
            if store.get(&project, id)?.is_none() {
                missing.push(id.to_string());
            }
        }
        if !missing.is_empty() {
            bail!("there's no entry {}", missing.join(", "));
        }
        kinding.ids.iter().map(|&id| (id, kind)).collect()
    };
    for (id, kind) in changes {
        let entry = store.set_kind(&project, id, kind)?;
        let line = format!("{id} is a {kind} now: {}", memory::title(&entry.text));
        outln!("{}", printable::line(&line))?;
        let changed = Event::memory(events::Kind::MemoryChanged, project.clone(), entry);
        tell(socket, changed);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Twin;

    fn entry(id: u64, kind: Kind, text: &str) -> Entry {
        Entry {
            id,
            kind,
            text: text.into(),
            files: Vec::new(),
            source: Source::User,
            created: 0,
            seen: 1,
            last_seen: 0,
            anchors: Default::default(),
            checkout: None,
            used: None,
            names: Vec::new(),
            counted_from: None,
        }
    }

    #[test]
    fn dedupe_lists_each_one_kept_with_what_goes_into_it() {
        let merges = [Merge {
            kept: entry(3, Kind::Gotcha, "The ledger needs redis\n\nStart it first."),
            merged: vec![
                Twin {
                    entry: entry(7, Kind::Note, "redis has to run for the ledger"),
                    alike: 0.934,
                    reranked: Some(0.5),
                },
                Twin {
                    entry: entry(9, Kind::Command, "make redis \x1b[31mfirst"),
                    alike: 0.95,
                    reranked: None,
                },
            ],
        }];
        assert_eq!(
            merges_text(&merges, false),
            "   3  gotcha    The ledger needs redis\n\
             \x20 ← 0.93     7  note      redis has to run for the ledger\n\
             \x20 ← 0.95     9  command   make redis [31mfirst\n\
             2 entries would go into 1: --apply merges them\n"
        );
        assert!(merges_text(&merges, true).ends_with("\nmerged 2 entries into 1\n"));
        assert_eq!(
            merges_text(&[], false),
            "no two entries say the same thing\n"
        );
    }
}
