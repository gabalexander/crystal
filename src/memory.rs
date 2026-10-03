//! What a project's sessions have learned, kept for the sessions after
//! them: decisions made, gotchas hit, commands that work, notes, and how
//! tasks turned out.
//!
//! Each project keeps its own list, in a JSON file in crystal's state
//! directory, named after the project's main worktree. Anyone can add to
//! it: the user, an agent in a session (`crystal remember`), or a task as it
//! ends. A Claude Code session is shown the entries that have most to do
//! with what it was asked when it starts.
//!
//! An entry can name the files it's about. Once one of them changes, the
//! entry may no longer be true: it's marked stale, and agents aren't shown
//! it.
//!
//! The user can turn all of it off: [`enabled`] is the one place that
//! decides, and everything memory adds asks it first.

use crate::config::Config;
use crate::git::Checkout;
use crate::state;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many entries a session is shown when it starts. A few that matter
/// help; a long list is skimmed past.
const SHOWN_AT_LAUNCH: usize = 6;

/// The most of an entry's text a session is shown at launch.
const LAUNCH_TEXT_LENGTH: usize = 300;

/// How recent an entry has to be to count as recent when searching.
const RECENT: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Words too common to say what an entry is about.
const COMMON_WORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "are", "was", "but", "not", "you",
    "its", "has", "have", "when", "then", "than", "all", "any", "can", "use", "make",
];

/// What's said when something asks for memory while it's turned off.
pub const OFF: &str = "the memory plugin is off: set `memory = true` in the config to turn it on";

/// Whether memory is on: the one gate for everything it adds, from the
/// launch paragraph to the TUI's view and the commands.
pub fn enabled(config: &Config) -> bool {
    config.memory
}

/// Whether memory is on, by the config file as it is now. A file that
/// can't be read leaves memory at its default, on.
pub fn enabled_now() -> bool {
    Config::load().map_or(true, |config| enabled(&config))
}

/// What sort of thing an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A choice made, and why.
    Decision,
    /// Something that catches people out.
    Gotcha,
    /// A command that does something useful here.
    Command,
    Note,
    /// How a task turned out.
    Outcome,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            Kind::Decision => "decision",
            Kind::Gotcha => "gotcha",
            Kind::Command => "command",
            Kind::Note => "note",
            Kind::Outcome => "outcome",
        };
        f.write_str(name)
    }
}

/// Who an entry came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    User,
    /// A session, by its name when it added the entry.
    Session(String),
    /// A task, by its name, as it ended.
    Task(String),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Source::User => f.write_str("you"),
            Source::Session(name) => write!(f, "session {name}"),
            Source::Task(name) => write!(f, "task {name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Counts up from 1 in each project, so it's short enough to type.
    pub id: u64,
    pub kind: Kind,
    pub text: String,
    /// The files it's about, from the top of the project.
    #[serde(default)]
    pub files: Vec<String>,
    pub source: Source,
    /// When it was added, in seconds since the Unix epoch.
    pub created: u64,
}

/// A project's file of entries, as it's kept on disk.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    /// The id the next entry gets. Ids are never used twice, even once an
    /// entry is gone, so an id someone noted down never means another.
    next_id: u64,
    entries: Vec<Entry>,
}

/// A project's memory, as read at one moment.
#[derive(Debug)]
pub struct Memory {
    /// The project's main worktree, which the entries' files are under.
    pub project: PathBuf,
    entries: Vec<Entry>,
}

/// An entry, with whether it has gone stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub entry: Entry,
    pub stale: bool,
}

impl Memory {
    /// The memory of `project` kept for the daemon at `socket`. A project
    /// with nothing remembered yet has an empty one.
    pub fn read(socket: &Path, project: &Path) -> Result<Memory> {
        let file = file_for(socket, project);
        let store = load(&file)?;
        Ok(Memory {
            project: project.to_path_buf(),
            entries: store.entries,
        })
    }

    /// Every entry, newest first, with whether it's stale.
    pub fn listed(&self) -> Vec<Listed> {
        let mut listed: Vec<Listed> = self
            .entries
            .iter()
            .map(|entry| Listed {
                entry: entry.clone(),
                stale: is_stale(entry, &self.project),
            })
            .collect();
        listed.sort_by(|a, b| b.entry.id.cmp(&a.entry.id));
        listed
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

/// The project `dir` belongs to: its repository's main worktree, so that
/// every worktree of a project shares one memory, or else `dir` itself.
pub fn project_of(dir: &Path) -> PathBuf {
    match Checkout::find(dir) {
        Some(checkout) => checkout.worktree().project_path,
        None => fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()),
    }
}

/// Adds an entry to `project`'s memory, and returns it.
pub fn add(
    socket: &Path,
    project: &Path,
    kind: Kind,
    text: &str,
    files: Vec<String>,
    source: Source,
) -> Result<Entry> {
    let text = text.trim();
    if text.is_empty() {
        bail!("there's nothing to remember");
    }
    change(&file_for(socket, project), |store| {
        let entry = Entry {
            id: store.next_id.max(1),
            kind,
            text: text.to_string(),
            files,
            source,
            created: seconds_since_epoch(SystemTime::now()),
        };
        store.next_id = entry.id + 1;
        store.entries.push(entry.clone());
        Ok(entry)
    })
}

/// Keeps how a task turned out, for the sessions after it: what the tasks
/// part of crystal calls when a task ends with something to say. With
/// memory off it keeps nothing, and says so with `None`.
#[allow(
    dead_code,
    reason = "tasks record how they turned out through this once they close"
)]
pub fn record_outcome(
    config: &Config,
    socket: &Path,
    project: &Path,
    task: &str,
    summary: &str,
) -> Result<Option<Entry>> {
    if !enabled(config) {
        return Ok(None);
    }
    let source = Source::Task(task.to_string());
    add(socket, project, Kind::Outcome, summary, Vec::new(), source).map(Some)
}

/// Takes the entry `id` out of `project`'s memory, and returns it.
pub fn remove(socket: &Path, project: &Path, id: u64) -> Result<Entry> {
    change(&file_for(socket, project), |store| {
        let at = store
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .with_context(|| format!("there's no entry {id}"))?;
        Ok(store.entries.remove(at))
    })
}

/// The entries that have to do with `query`, the best first. Each word of
/// the query found in an entry's text or files counts ten; a decision or a
/// gotcha counts two more, since those are what save the most time; an
/// entry from the last month counts one more; and a stale one loses five.
/// An entry that shares no word with the query isn't a match at all.
pub fn search<'a>(listed: &'a [Listed], query: &str, now: u64) -> Vec<&'a Listed> {
    let words = words_of(query);
    let mut scored: Vec<(i64, &Listed)> = listed
        .iter()
        .filter_map(|item| {
            let found = shared_words(&words, &item.entry);
            (found > 0).then(|| (score(item, found, now), item))
        })
        .collect();
    // Best first; between equals, the newest.
    scored.sort_by(|(a, x), (b, y)| b.cmp(a).then(y.entry.id.cmp(&x.entry.id)));
    scored.into_iter().map(|(_, item)| item).collect()
}

/// What a Claude Code session is told of its project's memory as it
/// starts: the entries with most to do with what it was asked, `asked`, or
/// else the newest, leaving out stale ones; then how to add to it.
pub fn for_launch(memory: &Memory, asked: &str, now: u64) -> String {
    let listed: Vec<Listed> = memory
        .listed()
        .into_iter()
        .filter(|item| !item.stale)
        .collect();
    let mut shown: Vec<&Listed> = search(&listed, asked, now);
    if shown.is_empty() {
        shown = listed.iter().collect();
    }
    shown.truncate(SHOWN_AT_LAUNCH);

    let how_to_add = "When you learn something a later session here should know, like a \
                      decision, a gotcha or a command that works, keep it with \
                      `crystal remember \"<what>\"` (add `-k decision|gotcha|command|note`, and \
                      `-f <file>` for each file it's about).";
    if shown.is_empty() {
        return how_to_add.to_string();
    }
    let mut paragraph = String::from("What this project's earlier sessions learned:");
    for item in shown {
        paragraph.push_str("\n- ");
        paragraph.push_str(&launch_line(&item.entry));
    }
    paragraph.push_str("\n\n");
    paragraph.push_str(how_to_add);
    paragraph
}

/// Writes `entry` into the project's CLAUDE.md, or its AGENTS.md if that's
/// the one it has, under a "Notes" heading: for something every session
/// should know, kept with the code. Returns the file it went into.
pub fn promote(project: &Path, entry: &Entry) -> Result<PathBuf> {
    let file = instructions_file(project);
    let text = fs::read_to_string(&file).unwrap_or_default();
    let note = format!("- {}", one_line(&entry.text));
    fs::write(&file, with_note(&text, &note))
        .with_context(|| format!("couldn't write {}", file.display()))?;
    Ok(file)
}

/// The project's instructions file: CLAUDE.md, or AGENTS.md if that's the
/// one it has. With neither, a new CLAUDE.md.
fn instructions_file(project: &Path) -> PathBuf {
    let claude = project.join("CLAUDE.md");
    let agents = project.join("AGENTS.md");
    if !claude.exists() && agents.exists() {
        agents
    } else {
        claude
    }
}

/// `text` with `note` added at the end of its "Notes" section, which is
/// made at the end of the file if there isn't one.
fn with_note(text: &str, note: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    let heading = lines.iter().position(|line| line.trim() == "## Notes");
    let Some(heading) = heading else {
        let mut text = text.trim_end().to_string();
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        return format!("{text}## Notes\n\n{note}\n");
    };
    // The section ends at the next heading, or the end of the file; the
    // note goes after its last line that isn't blank.
    let end = lines[heading + 1..]
        .iter()
        .position(|line| line.starts_with('#'))
        .map_or(lines.len(), |at| heading + 1 + at);
    let mut at = end;
    while at > heading + 1 && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    if at == heading + 1 {
        // An empty section: a blank line under the heading first.
        lines.insert(at, "");
        at += 1;
    }
    lines.insert(at, note);
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// One entry as a session is shown it: its kind, its text on one line and
/// not too long, and the files it's about.
fn launch_line(entry: &Entry) -> String {
    let mut text = one_line(&entry.text);
    if text.chars().count() > LAUNCH_TEXT_LENGTH {
        text = text.chars().take(LAUNCH_TEXT_LENGTH).collect();
        text.push('…');
    }
    let mut line = format!("({}) {text}", entry.kind);
    if !entry.files.is_empty() {
        line.push_str(&format!(" [{}]", entry.files.join(", ")));
    }
    line
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a file `entry` is about has changed since it was added, or is
/// gone. Either way, the entry may no longer be true.
pub fn is_stale(entry: &Entry, project: &Path) -> bool {
    let added = UNIX_EPOCH + Duration::from_secs(entry.created);
    entry.files.iter().any(|file| {
        let changed = fs::metadata(project.join(file)).and_then(|meta| meta.modified());
        match changed {
            Ok(changed) => changed > added,
            Err(_) => true,
        }
    })
}

fn score(item: &Listed, found: usize, now: u64) -> i64 {
    let mut score = 10 * found as i64;
    if matches!(item.entry.kind, Kind::Decision | Kind::Gotcha) {
        score += 2;
    }
    if now.saturating_sub(item.entry.created) < RECENT.as_secs() {
        score += 1;
    }
    if item.stale {
        score -= 5;
    }
    score
}

/// How many of `words` are in the entry's text or the names of its files.
fn shared_words(words: &[String], entry: &Entry) -> usize {
    let mut own = words_of(&entry.text);
    for file in &entry.files {
        own.extend(words_of(file));
    }
    words.iter().filter(|word| own.contains(word)).count()
}

/// The words of `text` that say what it's about: lower case, without
/// punctuation, at least three letters, and not among the commonest. Each
/// word once.
fn words_of(text: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let word = word.to_lowercase();
        if word.chars().count() >= 3
            && !COMMON_WORDS.contains(&word.as_str())
            && !words.contains(&word)
        {
            words.push(word);
        }
    }
    words
}

/// Where the memories of the daemon at `socket` are kept: in crystal's
/// state directory, beside the list of sessions, so that a daemon of a
/// test's own keeps its own.
pub fn dir(socket: &Path) -> PathBuf {
    state::path(socket).with_file_name("memory")
}

/// The file `project`'s memory is kept in: named after its path, the way
/// Claude Code names its projects, `/code/app` as `-code-app.json`.
fn file_for(socket: &Path, project: &Path) -> PathBuf {
    let name = project.to_string_lossy().replace('/', "-");
    dir(socket).join(format!("{name}.json"))
}

fn load(file: &Path) -> Result<Store> {
    match fs::read_to_string(file) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("couldn't read {}", file.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(err) => Err(err).with_context(|| format!("couldn't read {}", file.display())),
    }
}

/// Reads `file`'s store, lets `edit` change it, and writes it back, holding
/// a lock all the while: two sessions remembering at once must both be
/// kept, not one written over the other.
fn change<T>(file: &Path, edit: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let _lock = Lock::take(&file.with_extension("lock"))?;
    let mut store = load(file)?;
    let answer = edit(&mut store)?;
    // Written beside it first, then moved into place in one step, so that
    // whoever reads it meanwhile reads all of it.
    let unfinished = file.with_extension("json.unfinished");
    fs::write(&unfinished, serde_json::to_string_pretty(&store)?)?;
    fs::rename(&unfinished, file)?;
    Ok(answer)
}

/// A lock on a file of its own, held until it's dropped. Closing the file
/// lets the lock go.
struct Lock {
    _file: File,
}

impl Lock {
    fn take(path: &Path) -> Result<Lock> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .with_context(|| format!("couldn't open {}", path.display()))?;
        // SAFETY: flock only takes a lock on a file descriptor, which `file`
        // keeps open for as long as the lock is held.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if locked != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("couldn't lock {}", path.display()));
        }
        Ok(Lock { _file: file })
    }
}

pub fn seconds_since_epoch(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, kind: Kind, text: &str) -> Entry {
        Entry {
            id,
            kind,
            text: text.into(),
            files: Vec::new(),
            source: Source::User,
            created: 1_000,
        }
    }

    fn listed(entry: Entry) -> Listed {
        Listed {
            entry,
            stale: false,
        }
    }

    /// A socket in a directory of the test's own, so its memory is too.
    fn socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        (dir, socket)
    }

    #[test]
    fn what_is_added_reads_back_with_ids_counting_up() {
        let (_dir, socket) = socket();
        let project = Path::new("/code/app");
        add(
            &socket,
            project,
            Kind::Gotcha,
            "the tests need the db",
            vec![],
            Source::User,
        )
        .unwrap();
        add(
            &socket,
            project,
            Kind::Note,
            "fees are in cents",
            vec![],
            Source::User,
        )
        .unwrap();

        let memory = Memory::read(&socket, project).unwrap();
        let ids: Vec<u64> = memory.listed().iter().map(|item| item.entry.id).collect();
        assert_eq!(ids, [2, 1], "newest first");
        assert_eq!(memory.get(1).unwrap().text, "the tests need the db");
    }

    #[test]
    fn a_removed_entry_s_id_is_never_used_again() {
        let (_dir, socket) = socket();
        let project = Path::new("/code/app");
        add(&socket, project, Kind::Note, "one", vec![], Source::User).unwrap();
        remove(&socket, project, 1).unwrap();
        let next = add(&socket, project, Kind::Note, "two", vec![], Source::User).unwrap();
        assert_eq!(next.id, 2);
        assert!(remove(&socket, project, 1).is_err());
    }

    #[test]
    fn each_project_has_a_memory_of_its_own() {
        let (_dir, socket) = socket();
        add(
            &socket,
            Path::new("/code/app"),
            Kind::Note,
            "app's",
            vec![],
            Source::User,
        )
        .unwrap();
        let other = Memory::read(&socket, Path::new("/code/other")).unwrap();
        assert!(other.listed().is_empty());
    }

    #[test]
    fn nothing_is_too_little_to_remember() {
        let (_dir, socket) = socket();
        assert!(
            add(
                &socket,
                Path::new("/code/app"),
                Kind::Note,
                "  ",
                vec![],
                Source::User
            )
            .is_err()
        );
    }

    #[test]
    fn a_task_s_outcome_is_kept_as_one() {
        let (_dir, socket) = socket();
        let project = Path::new("/code/app");
        let config = Config::default();
        let entry = record_outcome(&config, &socket, project, "fixer", "fixed the refund race")
            .unwrap()
            .unwrap();
        assert_eq!(entry.kind, Kind::Outcome);
        assert_eq!(entry.source, Source::Task("fixer".into()));
    }

    #[test]
    fn with_memory_off_an_outcome_is_not_kept() {
        let (_dir, socket) = socket();
        let project = Path::new("/code/app");
        let config = Config {
            memory: false,
            ..Config::default()
        };
        let kept = record_outcome(&config, &socket, project, "fixer", "done").unwrap();
        assert_eq!(kept, None);
        assert!(Memory::read(&socket, project).unwrap().listed().is_empty());
    }

    #[test]
    fn an_entry_goes_stale_when_its_file_changes_or_goes() {
        let project = tempfile::tempdir().unwrap();
        let file = project.path().join("refund.rs");
        fs::write(&file, "fn refund() {}").unwrap();
        let now = seconds_since_epoch(SystemTime::now());
        let mut note = entry(1, Kind::Gotcha, "refund waits for the ledger");
        note.files = vec!["refund.rs".into()];
        note.created = now + 60;
        assert!(!is_stale(&note, project.path()), "unchanged since");

        note.created = now - 3600;
        let file_handle = File::options().write(true).open(&file).unwrap();
        file_handle.set_modified(SystemTime::now()).unwrap();
        assert!(is_stale(&note, project.path()), "changed since");

        note.files = vec!["gone.rs".into()];
        note.created = now + 60;
        assert!(is_stale(&note, project.path()), "gone");
    }

    #[test]
    fn search_ranks_by_shared_words_then_kind_then_age() {
        let all = vec![
            listed(entry(1, Kind::Note, "the ledger is slow")),
            listed(entry(2, Kind::Gotcha, "the ledger test needs the database")),
            listed(entry(3, Kind::Note, "fees are rounded down")),
            listed(entry(4, Kind::Note, "ledger test flakes under load")),
        ];
        let found = search(&all, "flaky ledger test", 2_000);
        let ids: Vec<u64> = found.iter().map(|item| item.entry.id).collect();
        // Two shared words beat one; between two-word matches, a gotcha
        // beats a note; no shared word, no match.
        assert_eq!(ids, [2, 4, 1]);
    }

    #[test]
    fn a_stale_entry_sinks_in_the_search() {
        let mut stale = listed(entry(1, Kind::Note, "ledger timeout"));
        stale.stale = true;
        let fresh = listed(entry(2, Kind::Note, "ledger timeout"));
        let all = vec![stale, fresh];
        let ids: Vec<u64> = search(&all, "ledger", 2_000)
            .iter()
            .map(|item| item.entry.id)
            .collect();
        assert_eq!(ids, [2, 1]);
    }

    #[test]
    fn a_session_is_shown_what_it_was_asked_about_first() {
        let memory = Memory {
            project: PathBuf::from("/code/app"),
            entries: vec![
                entry(1, Kind::Gotcha, "the refund test needs the ledger running"),
                entry(2, Kind::Note, "the docs build with mdbook"),
            ],
        };
        let paragraph = for_launch(&memory, "fix the flaky refund test", 2_000);
        assert!(paragraph.starts_with("What this project's earlier sessions learned:"));
        assert!(paragraph.contains("- (gotcha) the refund test needs the ledger running"));
        assert!(
            !paragraph.contains("mdbook"),
            "only what has to do with the task"
        );
        assert!(paragraph.contains("crystal remember"));
    }

    #[test]
    fn with_nothing_relevant_a_session_is_shown_the_newest() {
        let memory = Memory {
            project: PathBuf::from("/code/app"),
            entries: vec![entry(1, Kind::Note, "the docs build with mdbook")],
        };
        let paragraph = for_launch(&memory, "", 2_000);
        assert!(paragraph.contains("- (note) the docs build with mdbook"));
    }

    #[test]
    fn with_nothing_remembered_a_session_is_told_how_to_add() {
        let memory = Memory {
            project: PathBuf::from("/code/app"),
            entries: Vec::new(),
        };
        let paragraph = for_launch(&memory, "anything", 2_000);
        assert!(paragraph.starts_with("When you learn something"));
    }

    #[test]
    fn a_stale_entry_is_never_shown_at_launch() {
        let project = tempfile::tempdir().unwrap();
        let mut note = entry(1, Kind::Gotcha, "refund waits for the ledger");
        note.files = vec!["gone.rs".into()];
        let memory = Memory {
            project: project.path().to_path_buf(),
            entries: vec![note],
        };
        assert!(!for_launch(&memory, "refund", 2_000).contains("refund waits"));
    }

    #[test]
    fn a_note_goes_under_a_notes_heading_made_if_missing() {
        assert_eq!(with_note("", "- one"), "## Notes\n\n- one\n");
        assert_eq!(
            with_note("# App\n\nUse cargo.\n", "- one"),
            "# App\n\nUse cargo.\n\n## Notes\n\n- one\n"
        );
    }

    #[test]
    fn a_note_joins_the_end_of_an_existing_notes_section() {
        let text = "# App\n\n## Notes\n\n- one\n\n## Build\n\ncargo build\n";
        assert_eq!(
            with_note(text, "- two"),
            "# App\n\n## Notes\n\n- one\n- two\n\n## Build\n\ncargo build\n"
        );
    }

    #[test]
    fn promote_writes_claude_md_or_the_agents_md_already_there() {
        let project = tempfile::tempdir().unwrap();
        let note = entry(1, Kind::Gotcha, "run the ledger first");
        assert_eq!(
            promote(project.path(), &note).unwrap(),
            project.path().join("CLAUDE.md")
        );

        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("AGENTS.md"), "# Agents\n").unwrap();
        let file = promote(other.path(), &note).unwrap();
        assert_eq!(file, other.path().join("AGENTS.md"));
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            "# Agents\n\n## Notes\n\n- run the ledger first\n"
        );
    }

    #[test]
    fn words_leave_out_punctuation_short_and_common_words() {
        assert_eq!(
            words_of("Fix the flaky refund-test, NOW!"),
            ["fix", "flaky", "refund", "test", "now"]
        );
    }
}
