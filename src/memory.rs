//! What a project's sessions have learned, kept for the sessions after
//! them: decisions made, gotchas hit, commands that work, and notes.
//!
//! Every project's entries are kept in one SQLite database in crystal's
//! state directory, with a full-text index over them (SQLite's FTS5, ranked
//! by bm25), so a search finds an entry by any of its words, or a word they
//! start or stem from. Anyone can add to it: the user, an agent in a
//! session (`crystal remember`), and the distiller ([`crate::distill`]),
//! from what a closed task did. The same thing said again is the one entry
//! seen again, not a second one: in the same words, or, with the models
//! that search by meaning, in others; and what the user forgot the distiller
//! can't bring back, in its words or others. Entries kept twice before that
//! check, `crystal memory dedupe` merges into one: what it merged counts as
//! the entry it went into said again, and a search finds that entry by its
//! words and meaning too.
//!
//! Every agent crystal starts is shown, as it starts, the entries that have
//! most to do with its launch: first those about files its worktree has
//! changed, then those about what it was asked. Claude Code searches the
//! rest through crystal's MCP server ([`crate::mcp`]); other agents with
//! crystal's commands.
//!
//! How each task turned out is kept with the task, in its project's
//! history (`crystal tasks`). The outcome entries an earlier crystal kept
//! for each one are still found by a search, but not shown at launch: true
//! only when they were written, and one for each `crystal done`, they
//! crowded out what still holds. Each keeps its task's goal in a sentence
//! and what `crystal done` said, not the whole brief the task was given.
//!
//! Lessons (decisions, gotchas and commands) rank above notes and
//! outcomes, in a search and at launch, unless what's asked is about what
//! was done. A note or an outcome nobody finds again, by saying it again
//! or by an agent reading it in full, expires after a while: searches and
//! agents starting leave it out, and the list marks it.
//!
//! Whether an entry still holds goes by what it names: the identifiers,
//! paths, commands and flags in its text (`local_origin`,
//! `Request::Shutdown`, `src/agent_rules.rs`, `--test-threads`) that were
//! in its worktree's code when it was said. While they're all still there,
//! it holds, however much its files have changed; once some are gone, it's
//! drifting, and may hold only in part; once all of them are, it's stale.
//! An entry that names nothing to look for goes by the files it's about
//! instead, a hash of each kept as it was when it was said: drifting once
//! some have changed, and stale once every one is gone. Those, and the
//! names, are its anchors. Agents starting aren't shown the stale; a search
//! gives them after the rest, marked, and the distiller is asked whether
//! those about the files a task touched still hold.
//!
//! The user can turn all of it off: [`enabled`] is the one place that
//! decides, and everything memory adds asks it first.

use crate::config::Config;
use crate::embed::{self, Embed};
use crate::git::Checkout;
use crate::output::errln;
use crate::printable;
use crate::secrets;
use crate::state;
use anyhow::{Context, Result, bail};
use regex::RegexSet;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::hash::{DefaultHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many entries a session is shown when it starts. A few that matter
/// help; a long list is skimmed past.
const SHOWN_AT_LAUNCH: usize = 6;

/// The most of an entry's text a session is shown at launch.
const LAUNCH_TEXT_LENGTH: usize = 300;

/// The most bytes the entries a session is shown at launch take, their
/// lines together: docket's budget for its index of the same. The least
/// relevant are left out first.
const LAUNCH_BYTES: usize = 800;

/// The biggest file an anchor hashes, or whose words are looked through
/// for an entry's names. A file an entry is about is source, far smaller;
/// one bigger is data, not worth reading through each time.
const MAX_ANCHORED_BYTES: u64 = 8 * 1024 * 1024;

/// The most of a worktree read for the names entries give, in bytes, its
/// files in the order git lists them: crystal's own is under 8 MiB.
const MAX_WORDS_READ: u64 = 64 * 1024 * 1024;

/// The most files of a directory outside git read for the names entries
/// give.
const MAX_WALKED: usize = 5_000;

/// The directories of what's built or fetched, never read for names
/// outside git, where nothing says to ignore them.
const NOT_WALKED: &[&str] = &["node_modules", "target"];

/// How long after a file last changed its stamp, when that was and its
/// size, tells a change from none: changed twice within one tick of a
/// coarse clock, to the same size, it would look unchanged, so one read
/// sooner after it changed is read again the next time, as git does with
/// what its index keeps.
const SETTLED: Duration = Duration::from_secs(2);

/// How long the words of a worktree nobody looks at are kept.
const WORDS_KEPT: Duration = Duration::from_secs(30 * 60);

/// The most names an entry is checked by: the first it gives.
const MAX_NAMES: usize = 16;

/// The extensions that make a word a file's name, like `memory.db`.
const EXTENSIONS: &[&str] = &[
    "c", "cpp", "css", "db", "go", "h", "html", "java", "js", "json", "jsonl", "jsx", "kt", "lock",
    "log", "lua", "md", "nix", "py", "rb", "rs", "sh", "sock", "sql", "swift", "toml", "ts", "tsx",
    "txt", "yaml", "yml",
];

/// Words shaped like code that name nothing an entry could lose: Rust's
/// keywords and commonest types, and products' names.
const NOT_NAMES: &[&str] = &[
    "Err",
    "GitHub",
    "GitLab",
    "JavaScript",
    "LaTeX",
    "MySQL",
    "NeoVim",
    "None",
    "Ok",
    "OpenAI",
    "Option",
    "PostgreSQL",
    "PowerShell",
    "Result",
    "SQLite",
    "Self",
    "Some",
    "String",
    "TypeScript",
    "VSCode",
    "Vec",
    "WezTerm",
    "YouTube",
    "async",
    "await",
    "bool",
    "crate",
    "false",
    "iOS",
    "iTerm",
    "iTerm2",
    "impl",
    "let",
    "macOS",
    "mod",
    "mut",
    "pub",
    "self",
    "str",
    "super",
    "true",
    "use",
    "usize",
];

/// The most entries a search gives back.
pub const SEARCH_LIMIT: usize = 50;

/// The most entries said before that the reranker reads with one being
/// added, to find whether it says what one of them does: the most alike.
const TWIN_POOL: usize = 20;

/// The longest a title of an entry's own may be, in characters: a line.
const MAX_TITLE: usize = 120;

/// The most words of a query that reach the index: a session's first
/// prompt can be pages long, and its first few dozen words say what it's
/// about.
const MAX_QUERY_TERMS: usize = 24;

/// Words that say nothing about what an entry is about. They're left out
/// of a query that has other words.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "do", "does", "for", "from",
    "has", "have", "how", "i", "if", "in", "into", "is", "it", "its", "me", "my", "no", "not",
    "of", "on", "or", "our", "please", "so", "that", "the", "their", "then", "there", "these",
    "this", "to", "up", "us", "was", "we", "what", "when", "where", "which", "who", "why", "will",
    "with", "you", "your",
];

/// bm25's weight for each indexed column, the text, the files and the
/// words of the entries merged into it: a word in a file's name says more
/// about what an entry is about than one in a sentence, and one in what was
/// merged into it, less than one in what it says itself. On crystal's
/// notes, 0.5 for those merged found the most.
const RANK: &str = "bm25(entries_fts, 1.0, 2.0, 0.5)";

/// How long a write waits for another to finish: crystal's commands, the
/// daemon, the TUI and the MCP servers all share the one database.
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// How many of the best entries each ranking, by words and by meaning,
/// gives before the two are merged.
const POOL: usize = 50;

/// How many of the best entries, by words and meaning merged, the reranker
/// reads again: 20 ranked crystal's notes best. Those past it it never
/// reads, so they aren't found.
const RERANK_POOL: usize = 20;

/// Reciprocal rank fusion's constant: an entry gets `1 / (K + its place)`
/// from each ranking it's in. 60, as in the paper that brought it in, keeps
/// a first place in one ranking from outweighing good places in both.
const FUSION_K: f32 = 60.0;

/// How many entries go through the model at once, embedding those that
/// have no vector yet.
const EMBED_BATCH: usize = 32;

/// How many places lower a note or a task's outcome ranks than what it
/// says would put it, below the lessons near it: what was learned before
/// what was done, but a note that answers well still ahead of a lesson
/// that hardly does. Asked 97 questions about crystal's own memory, 3 put
/// the lessons that answer as high as more places did, and left the notes
/// that answer the most room.
const NOTES_BEHIND: usize = 3;

/// Words that ask about what was done rather than what was learned: a
/// search with one of them ranks notes and outcomes where they fall.
const WHAT_WAS_DONE: &[&str] = &[
    "already",
    "did",
    "done",
    "finished",
    "happened",
    "history",
    "merged",
    "outcome",
    "outcomes",
    "previously",
    "progress",
    "shipped",
    "status",
];

/// The longest a task's goal stays in its outcome entry, in characters: a
/// title's line.
const OUTCOME_GOAL: usize = MAX_TITLE;

/// The database's tables as they were first. `n` is the entry's
/// rowid, which the full-text index is kept by; `id` is the number people
/// see, counting up in each project from its `next_id`, and never used
/// twice. `key` is the entry's words, lower case, to find the same thing
/// said again. `forgotten` keeps a hash of each forgotten entry's key, so
/// the distiller can't add it back. The triggers keep the index in step
/// with every change.
const TABLES: &str = "
CREATE TABLE projects (
  path    TEXT PRIMARY KEY,
  next_id INTEGER NOT NULL
);
CREATE TABLE entries (
  n         INTEGER PRIMARY KEY,
  project   TEXT NOT NULL,
  id        INTEGER NOT NULL,
  kind      TEXT NOT NULL,
  text      TEXT NOT NULL,
  key       TEXT NOT NULL,
  files     TEXT NOT NULL DEFAULT '[]',
  source    TEXT NOT NULL,
  created   INTEGER NOT NULL,
  seen      INTEGER NOT NULL DEFAULT 1,
  last_seen INTEGER NOT NULL,
  UNIQUE (project, id)
);
CREATE INDEX entries_key ON entries (project, key);
CREATE TABLE forgotten (
  project TEXT NOT NULL,
  key     TEXT NOT NULL,
  PRIMARY KEY (project, key)
);
CREATE VIRTUAL TABLE entries_fts USING fts5(
  text, files, content = 'entries', content_rowid = 'n', tokenize = 'porter unicode61'
);
CREATE TRIGGER entries_fts_insert AFTER INSERT ON entries BEGIN
  INSERT INTO entries_fts (rowid, text, files) VALUES (new.n, new.text, new.files);
END;
CREATE TRIGGER entries_fts_delete AFTER DELETE ON entries BEGIN
  INSERT INTO entries_fts (entries_fts, rowid, text, files)
    VALUES ('delete', old.n, old.text, old.files);
END;
CREATE TRIGGER entries_fts_update AFTER UPDATE OF text, files ON entries BEGIN
  INSERT INTO entries_fts (entries_fts, rowid, text, files)
    VALUES ('delete', old.n, old.text, old.files);
  INSERT INTO entries_fts (rowid, text, files) VALUES (new.n, new.text, new.files);
END;
";

/// Each entry's vector, from the model [`crate::embed`] runs, kept under
/// the model's name: a vector from another model can't be compared. An
/// entry whose text changes, or that goes, loses its vector; so does a new
/// entry given the rowid of one gone, which SQLite can do.
const VECTORS: &str = "
CREATE TABLE vectors (
  n      INTEGER PRIMARY KEY,
  model  TEXT NOT NULL,
  vector BLOB NOT NULL
);
CREATE TRIGGER vectors_entry_added AFTER INSERT ON entries BEGIN
  DELETE FROM vectors WHERE n = new.n;
END;
CREATE TRIGGER vectors_entry_gone AFTER DELETE ON entries BEGIN
  DELETE FROM vectors WHERE n = old.n;
END;
CREATE TRIGGER vectors_text_changed AFTER UPDATE OF text ON entries BEGIN
  DELETE FROM vectors WHERE n = old.n;
END;
";

/// Each entry's anchors, the hash of each of its files as it was when the
/// entry was last said, by file; and the worktree they were hashed in,
/// which the files are looked at in while it's there. With none, they're
/// looked at in the project's main worktree.
const ANCHORS: &str = "
ALTER TABLE entries ADD COLUMN anchors TEXT NOT NULL DEFAULT '{}';
ALTER TABLE entries ADD COLUMN checkout TEXT;
";

/// Whether the daemon has told of each entry going stale, which it does
/// once, until the entry holds again: see [`newly_stale`].
const TOLD_STALE: &str = "
ALTER TABLE entries ADD COLUMN told_stale INTEGER NOT NULL DEFAULT 0;
";

/// The names each entry's text gives that were in its worktree's code when
/// it was last said, which it's checked by: see [`add_names`].
const NAMES: &str = "
ALTER TABLE entries ADD COLUMN names TEXT NOT NULL DEFAULT '[]';
";

/// What each forgotten entry said, beside the hash of its words, for
/// `crystal memory list --forgotten` to show: its id, kind, text and files,
/// where it came from, and when it was forgotten. Those forgotten before
/// have only their hash.
const FORGOTTEN_ENTRIES: &str = "
ALTER TABLE forgotten ADD COLUMN id INTEGER;
ALTER TABLE forgotten ADD COLUMN kind TEXT;
ALTER TABLE forgotten ADD COLUMN text TEXT;
ALTER TABLE forgotten ADD COLUMN files TEXT NOT NULL DEFAULT '[]';
ALTER TABLE forgotten ADD COLUMN source TEXT;
ALTER TABLE forgotten ADD COLUMN forgot INTEGER;
";

/// When an agent last read each entry in full, which finds it again, so
/// it doesn't expire: see [`Store::used`].
const USED: &str = "
ALTER TABLE entries ADD COLUMN used INTEGER;
";

/// When each entry's time to expire counts from, when that's later than
/// when it was last said: see [`count_from_now`].
const COUNTED_FROM: &str = "
ALTER TABLE entries ADD COLUMN counted_from INTEGER;
";

/// Each entry merged into another that says the same thing, as it was: by
/// a hash of its key, as [`FORGOTTEN_ENTRIES`] keeps the forgotten, so that
/// its words said again count as `kept`, the entry it went into, said again.
const MERGED: &str = "
CREATE TABLE merged (
  project TEXT NOT NULL,
  key     TEXT NOT NULL,
  kept    INTEGER NOT NULL,
  id      INTEGER NOT NULL,
  kind    TEXT NOT NULL,
  text    TEXT NOT NULL,
  files   TEXT NOT NULL DEFAULT '[]',
  source  TEXT NOT NULL,
  merged  INTEGER NOT NULL,
  PRIMARY KEY (project, key)
);
";

/// What the entries merged into each entry said, a line each, in its own
/// column of the full-text index, so their words find the entry they went
/// into; and the vector of each entry merged or forgotten, by the hash of
/// its key as `merged` and `forgotten` keep it, so its meaning does too,
/// and what was forgotten is told by its meaning as well as its words. The
/// index is made again with the column, from what's merged so far.
const APART: &str = "
DROP TRIGGER entries_fts_insert;
DROP TRIGGER entries_fts_delete;
DROP TRIGGER entries_fts_update;
DROP TABLE entries_fts;
ALTER TABLE entries ADD COLUMN merged_words TEXT NOT NULL DEFAULT '';
UPDATE entries SET merged_words = coalesce((SELECT group_concat(m.text, char(10)) FROM merged m
  WHERE m.project = entries.project AND m.kept = entries.id), '');
CREATE VIRTUAL TABLE entries_fts USING fts5(
  text, files, merged_words, content = 'entries', content_rowid = 'n',
  tokenize = 'porter unicode61'
);
CREATE TRIGGER entries_fts_insert AFTER INSERT ON entries BEGIN
  INSERT INTO entries_fts (rowid, text, files, merged_words)
    VALUES (new.n, new.text, new.files, new.merged_words);
END;
CREATE TRIGGER entries_fts_delete AFTER DELETE ON entries BEGIN
  INSERT INTO entries_fts (entries_fts, rowid, text, files, merged_words)
    VALUES ('delete', old.n, old.text, old.files, old.merged_words);
END;
CREATE TRIGGER entries_fts_update AFTER UPDATE OF text, files, merged_words ON entries BEGIN
  INSERT INTO entries_fts (entries_fts, rowid, text, files, merged_words)
    VALUES ('delete', old.n, old.text, old.files, old.merged_words);
  INSERT INTO entries_fts (rowid, text, files, merged_words)
    VALUES (new.n, new.text, new.files, new.merged_words);
END;
INSERT INTO entries_fts (entries_fts) VALUES ('rebuild');
CREATE TABLE apart_vectors (
  project TEXT NOT NULL,
  key     TEXT NOT NULL,
  model   TEXT NOT NULL,
  vector  BLOB NOT NULL,
  PRIMARY KEY (project, key)
);
";

/// What makes the database as it is now, a step for each version: a
/// database at version `v`, kept in its `user_version`, takes the steps
/// after the first `v`.
const MIGRATIONS: &[fn(&Connection) -> Result<()>] = &[
    |conn| Ok(conn.execute_batch(TABLES)?),
    |conn| Ok(conn.execute_batch(VECTORS)?),
    add_anchors,
    |conn| Ok(conn.execute_batch(TOLD_STALE)?),
    |conn| Ok(conn.execute_batch(FORGOTTEN_ENTRIES)?),
    shorten_outcomes,
    |conn| Ok(conn.execute_batch(USED)?),
    add_names,
    count_from_now,
    |conn| Ok(conn.execute_batch(MERGED)?),
    |conn| Ok(conn.execute_batch(APART)?),
];

/// The columns [`entry_of`] reads, in its order.
const COLUMNS: &str = "e.id, e.kind, e.text, e.files, e.source, e.created, e.seen, e.last_seen, \
                       e.anchors, e.checkout, e.used, e.names, e.counted_from";

/// Whether memory is on, the `memory` plugin: the one gate for everything
/// it adds, from the launch paragraph to the TUI's view and the commands.
pub fn enabled(config: &Config) -> bool {
    crate::plugins::enabled(config, "memory")
}

/// Whether memory is on, by the config file as it is now. A file that
/// can't be read leaves memory at its default, on.
pub fn enabled_now() -> bool {
    Config::load().map_or(true, |config| enabled(&config))
}

/// What sort of thing an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "MemoryKind"))]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A choice made, and why.
    Decision,
    /// Something that catches people out.
    Gotcha,
    /// A command that does something useful here.
    Command,
    Note,
    /// How a task turned out, as an earlier crystal kept it as each task
    /// closed: found by a search, never shown at launch, and not something
    /// to add any more.
    #[value(skip)]
    Outcome,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Decision,
        Kind::Gotcha,
        Kind::Command,
        Kind::Note,
        Kind::Outcome,
    ];

    /// The kind called `name`, as [`Kind`]'s `Display` writes it.
    pub fn parse(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.to_string() == name)
    }

    /// Whether it's a lesson, something learned that holds: a decision, a
    /// gotcha or a command. They rank above notes and outcomes, and never
    /// expire.
    pub fn is_lesson(self) -> bool {
        matches!(self, Kind::Decision | Kind::Gotcha | Kind::Command)
    }

    /// How long an entry of this kind lasts once nobody finds it again: a
    /// month for a note, two weeks for a task's outcome, and a lesson for
    /// good.
    pub fn expires_after(self) -> Option<Duration> {
        const DAY: u64 = 24 * 60 * 60;
        match self {
            Kind::Note => Some(Duration::from_secs(30 * DAY)),
            Kind::Outcome => Some(Duration::from_secs(14 * DAY)),
            Kind::Decision | Kind::Gotcha | Kind::Command => None,
        }
    }
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

/// The kinds of entry a search looks among.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kinds {
    All,
    Only(Kind),
    /// Every kind but tasks' outcomes, for what a session is shown as it
    /// starts.
    Lasting,
}

/// The entries a search looks among: those of `kinds`, and with `files`,
/// only those about one of them or about a file under one of them; the
/// expired too only with `expired`.
#[derive(Debug, Clone, Copy)]
struct Among<'a> {
    kinds: Kinds,
    files: &'a [String],
    expired: bool,
}

impl Among<'_> {
    /// The kind it keeps to, the kind it leaves out, the files it keeps
    /// to, as JSON, and the time the expired are told by, as the SQL's
    /// parameters, `NULL` for none.
    fn params(self) -> (Option<String>, Option<String>, Option<String>, Option<u64>) {
        let (only, but) = match self.kinds {
            Kinds::All => (None, None),
            Kinds::Only(kind) => (Some(kind.to_string()), None),
            Kinds::Lasting => (None, Some(Kind::Outcome.to_string())),
        };
        let files = (!self.files.is_empty())
            .then(|| serde_json::to_string(self.files).expect("a list of strings is JSON"));
        let now = (!self.expired).then(|| seconds_since_epoch(SystemTime::now()));
        (only, but, files, now)
    }
}

/// The SQL that keeps a search to the entries about the files, as JSON, in
/// parameter `at`, or a file under one of them: all of them while it's
/// `NULL`.
fn about_files_sql(at: usize) -> String {
    format!(
        "(?{at} IS NULL OR EXISTS (SELECT 1 FROM json_each(e.files) f, json_each(?{at}) w \
         WHERE f.value = w.value OR substr(f.value, 1, length(w.value) + 1) = w.value || '/'))"
    )
}

/// The SQL that leaves out the entries expired at the time in parameter
/// `at`, as [`Entry::expired`] tells them: none while it's `NULL`.
fn unexpired_sql(at: usize) -> String {
    let ages: String = Kind::ALL
        .iter()
        .filter_map(|kind| {
            let age = kind.expires_after()?.as_secs();
            Some(format!(" WHEN '{kind}' THEN {age}"))
        })
        .collect();
    format!(
        "(?{at} IS NULL OR e.seen > 1 OR e.used IS NOT NULL \
         OR coalesce(max(e.created, e.last_seen, coalesce(e.counted_from, 0)) \
         + CASE e.kind{ages} END > ?{at}, 1))"
    )
}

/// Who an entry came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "MemorySource"))]
#[serde(rename_all = "lowercase")]
pub enum Source {
    User,
    /// A session, by its name when it added the entry.
    Session(String),
    /// A task, by its name, as it ended.
    Task(String),
    /// The distiller, from what the task of this name did.
    Distilled(String),
}

impl Source {
    /// Whether crystal wrote it itself, rather than a person or an agent
    /// asked to: what was forgotten only they can bring back.
    fn is_crystal(&self) -> bool {
        matches!(self, Source::Task(_) | Source::Distilled(_))
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Source::User => f.write_str("you"),
            Source::Session(name) => write!(f, "session {name}"),
            Source::Task(name) => write!(f, "task {name}"),
            Source::Distilled(name) => write!(f, "the distiller, after task {name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "MemoryEntry"))]
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
    /// How many times it has been said: once, then once more each time
    /// someone says the same again.
    #[serde(default = "once")]
    pub seen: u32,
    /// When it was last said, in seconds since the Unix epoch.
    #[serde(default)]
    pub last_seen: u64,
    /// The SHA-256 of each of its files that was there when it was last
    /// said, by file, to tell whether they've changed since.
    #[serde(default)]
    pub anchors: BTreeMap<String, String>,
    /// The worktree its files were hashed in; the project's main worktree
    /// when it's `None`.
    #[serde(default)]
    pub checkout: Option<PathBuf>,
    /// What its text names that was in the code when it was last said: the
    /// identifiers, paths, commands and flags in it, to tell whether
    /// they're still there.
    #[serde(default)]
    pub names: Vec<String>,
    /// When an agent last read it in full, in seconds since the Unix epoch.
    #[serde(default)]
    pub used: Option<u64>,
    /// When its time to expire counts from, when that's later than when it
    /// was last said: when crystal started telling whether an entry is
    /// found again, for one from before then, or when it was made a note.
    #[serde(default)]
    pub counted_from: Option<u64>,
}

impl Entry {
    /// Whether it's expired at `now`: a note or a task's outcome that
    /// nobody has found again, said again or read in full by an agent,
    /// since it was said, or since its time began to count, longer ago than
    /// its kind lasts. Searches and agents starting leave it out; it's
    /// kept, and the list marks it.
    pub fn expired(&self, now: u64) -> bool {
        let Some(age) = self.kind.expires_after() else {
            return false;
        };
        let said = self.created.max(self.last_seen);
        let said = said.max(self.counted_from.unwrap_or(0));
        self.seen <= 1 && self.used.is_none() && now >= said.saturating_add(age.as_secs())
    }
}

fn once() -> u32 {
    1
}

/// An entry about to be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "MemoryNew"))]
pub struct New {
    pub kind: Kind,
    pub text: String,
    pub files: Vec<String>,
    pub source: Source,
    /// The worktree its files are in, from its top; the project's main
    /// worktree when it's `None`.
    pub checkout: Option<PathBuf>,
}

/// What adding an entry came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "MemoryAdded"))]
#[serde(rename_all = "lowercase")]
pub enum Added {
    /// A new entry.
    New(Entry),
    /// The project had it already, in the same words: it was seen again,
    /// with any new files it named added to its own.
    Again(Entry),
    /// The project had it already in other words: this entry, which says
    /// the same thing, or had one merged into it in those words, was seen
    /// again, as with [`Added::Again`].
    Alike(Entry),
    /// The user forgot it, and crystal can't add it back.
    Refused,
}

#[cfg(test)]
impl Added {
    pub fn entry(&self) -> Option<&Entry> {
        match self {
            Added::New(entry) | Added::Again(entry) | Added::Alike(entry) => Some(entry),
            Added::Refused => None,
        }
    }
}

/// An entry said before, and how alike it is to another, which says the
/// same thing: by the model's vectors, from -1 to 1, and by the reranker,
/// reading the other as the query, when it was asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "MemoryTwin"))]
pub struct Twin {
    pub entry: Entry,
    pub alike: f32,
    #[serde(default)]
    pub reranked: Option<f32>,
}

/// Entries of a project that say the same thing, as `crystal memory
/// dedupe` finds them: the one kept, and those that go into it, each as
/// alike to it as it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "MemoryMerge"))]
pub struct Merge {
    pub kept: Entry,
    pub merged: Vec<Twin>,
}

/// What an entry being added comes to by its meaning: its vector, the
/// entry already there that says what it does, if one does, and whether it
/// says what was forgotten.
struct Meant {
    vector: Vec<f32>,
    twin: Option<u64>,
    forgotten: bool,
}

/// What a project keeps apart, with vectors: each entry merged into one
/// still there, by that one's id, with what it said; and what each
/// forgotten entry said.
struct Apart {
    merged: Vec<(u64, String, Vec<f32>)>,
    forgotten: Vec<(String, Vec<f32>)>,
}

/// What a search keeps to besides its words, and the most it gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Wanted {
    /// Only entries of this kind.
    pub kind: Option<Kind>,
    /// Only entries about one of these files, or a file under one of them,
    /// each from the top of the project.
    pub files: Vec<String>,
    /// Leave the stale out, rather than give them after the rest.
    pub fresh: bool,
    /// The expired too, which are left out otherwise.
    #[serde(default)]
    pub expired: bool,
    pub limit: usize,
}

impl Wanted {
    /// The `limit` best of every kind, about any file, stale or not, but
    /// none expired.
    pub fn best(limit: usize) -> Wanted {
        Wanted {
            kind: None,
            files: Vec::new(),
            fresh: false,
            expired: false,
            limit,
        }
    }
}

/// An entry that was forgotten, as it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forgotten {
    /// The id it had.
    pub id: u64,
    pub kind: Kind,
    pub text: String,
    pub files: Vec<String>,
    pub source: Source,
    /// When it was forgotten, in seconds since the Unix epoch.
    pub forgotten: u64,
}

/// The database every project's memory is kept in, open.
pub struct Store {
    conn: Connection,
    /// Where each project's entries were kept, in a file of its own,
    /// before the database: a project's are brought in from there the
    /// first time it's opened.
    dir: PathBuf,
}

impl Store {
    /// The database of the daemon at `socket`, made if it isn't there.
    pub fn open(socket: &Path) -> Result<Store> {
        let dir = dir(socket);
        fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
        let file = dir.join("memory.db");
        let mut conn =
            Connection::open(&file).with_context(|| format!("couldn't open {}", file.display()))?;
        conn.busy_timeout(BUSY_WAIT)?;
        // Readers then never wait on a writer, nor a writer on them.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        migrate(&mut conn).with_context(|| format!("couldn't set up {}", file.display()))?;
        Ok(Store { conn, dir })
    }

    /// Adds `new` to `project`'s memory: a new entry, or one it has already
    /// seen again. Credentials in its text are taken out first, and so are
    /// control characters, which would drive the terminal of whoever reads
    /// it. Either way, it's anchored to its files and what it names as they
    /// are now.
    #[cfg(test)]
    pub fn add(&mut self, project: &Path, new: New) -> Result<Added> {
        self.add_with(project, new, None)
    }

    /// [`Store::add`], and with an `embedder`, an entry that says what one
    /// already there does in other words is that one seen again too: one
    /// as alike as [`Embed::same_from`], or as [`Embed::alike_from`] that
    /// the reranker, reading the new one as the query, scores
    /// [`Embed::same_reranked_from`]; held against the words of the
    /// entries merged into another too, which count as that one. So is one
    /// in the words of an entry merged into another. What says what was
    /// forgotten, by the same rule, crystal can't add, as it can't in the
    /// same words. A new entry keeps the vector it was compared by.
    pub fn add_with(
        &mut self,
        project: &Path,
        new: New,
        embedder: Option<&dyn Embed>,
    ) -> Result<Added> {
        let text = clean(&secrets::redact(new.text.trim()));
        if text.trim().is_empty() {
            bail!("there's nothing to remember");
        }
        let key = key_of(&text);
        let name = self.ready(project)?;
        // By meaning before the write, which holds up every other while
        // the models take their time.
        let meant = embedder.and_then(|embedder| {
            self.meaning(&name, &text, embedder, new.source.is_crystal())
                .map(|meant| (meant, embedder.model()))
                .inspect_err(|err| errln!("crystal: couldn't compare it by meaning: {err:#}"))
                .ok()
        });
        let now = seconds_since_epoch(SystemTime::now());
        let checkout = new.checkout.as_deref().unwrap_or(project);
        // Read before the database is held: the worktree's words are kept
        // for the same said again in other words, below.
        let mut code = Code::default();
        let names = code.names_in(checkout, &text, &new.files);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let said: Option<Entry> = if key.is_empty() {
            None
        } else {
            tx.query_row(
                &format!(
                    "SELECT {COLUMNS} FROM entries e WHERE e.project = ?1 AND e.key = ?2 \
                     ORDER BY e.id LIMIT 1"
                ),
                params![name, key],
                entry_of,
            )
            .optional()?
        };
        if let Some(said) = said {
            let entry = seen_again(&tx, &name, &said, &new, checkout, now, &mut code)?;
            tx.commit()?;
            return Ok(Added::Again(entry));
        }
        let forgotten = hash(&key);
        let was_forgotten: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM forgotten WHERE project = ?1 AND key = ?2)",
            params![name, forgotten],
            |row| row.get(0),
        )?;
        if was_forgotten {
            if new.source.is_crystal() {
                return Ok(Added::Refused);
            }
            tx.execute(
                "DELETE FROM forgotten WHERE project = ?1 AND key = ?2",
                params![name, forgotten],
            )?;
            tx.execute(
                "DELETE FROM apart_vectors WHERE project = ?1 AND key = ?2",
                params![name, forgotten],
            )?;
        }
        let merged_into: Option<u64> = tx
            .query_row(
                "SELECT kept FROM merged WHERE project = ?1 AND key = ?2",
                params![name, forgotten],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(kept) = merged_into {
            match get(&tx, &name, kept)? {
                Some(kept) => {
                    let entry = seen_again(&tx, &name, &kept, &new, checkout, now, &mut code)?;
                    tx.commit()?;
                    return Ok(Added::Alike(entry));
                }
                // The entry it went into was forgotten since, and with it
                // what it said.
                None if new.source.is_crystal() => return Ok(Added::Refused),
                None => {
                    tx.execute(
                        "DELETE FROM merged WHERE project = ?1 AND key = ?2",
                        params![name, forgotten],
                    )?;
                    tx.execute(
                        "DELETE FROM apart_vectors WHERE project = ?1 AND key = ?2",
                        params![name, forgotten],
                    )?;
                }
            }
        }
        let twin = meant.as_ref().and_then(|(meant, _)| meant.twin);
        // Only while it's there: it may have gone as the models ran.
        if let Some(twin) = twin.map(|id| get(&tx, &name, id)).transpose()?.flatten() {
            let entry = seen_again(&tx, &name, &twin, &new, checkout, now, &mut code)?;
            tx.commit()?;
            return Ok(Added::Alike(entry));
        }
        // What the user forgot crystal can't add back in other words either.
        if meant.as_ref().is_some_and(|(meant, _)| meant.forgotten) {
            return Ok(Added::Refused);
        }
        let id: u64 = tx.query_row(
            "SELECT next_id FROM projects WHERE path = ?1",
            params![name],
            |row| row.get(0),
        )?;
        tx.execute(
            "UPDATE projects SET next_id = ?2 WHERE path = ?1",
            params![name, id + 1],
        )?;
        let entry = Entry {
            id,
            kind: new.kind,
            anchors: anchors_in(checkout, &new.files),
            names,
            text,
            files: new.files,
            source: new.source,
            created: now,
            seen: 1,
            last_seen: now,
            checkout: new.checkout,
            used: None,
            counted_from: None,
        };
        insert(&tx, &name, &entry)?;
        if let Some((meant, model)) = &meant {
            tx.execute(
                "INSERT OR REPLACE INTO vectors (n, model, vector) VALUES (?1, ?2, ?3)",
                params![tx.last_insert_rowid(), model, bytes_of(&meant.vector)],
            )?;
        }
        tx.commit()?;
        Ok(Added::New(entry))
    }

    /// Anchors entry `id` of `project` again to its files and what it names
    /// as they are in `checkout` now: it holds as it is, someone who knows
    /// says, so it's fresh, and can be told of going stale again. Gives the
    /// entry back.
    pub fn reanchor(&mut self, project: &Path, id: u64, checkout: &Path) -> Result<Entry> {
        let name = self.ready(project)?;
        let entry =
            get(&self.conn, &name, id)?.with_context(|| format!("there's no entry {id}"))?;
        let mut code = Code::default();
        self.conn.execute(
            "UPDATE entries SET anchors = ?3, names = ?4, checkout = ?5, told_stale = 0 \
             WHERE project = ?1 AND id = ?2",
            params![
                name,
                id,
                serde_json::to_string(&anchors_in(checkout, &entry.files))?,
                serde_json::to_string(&code.names_in(checkout, &entry.text, &entry.files))?,
                checkout.to_string_lossy(),
            ],
        )?;
        get(&self.conn, &name, id)?.context("the entry just anchored is gone")
    }

    /// Puts `text` in place of what entry `id` of `project` says, anchored
    /// to its files and what `text` names as they are in `checkout` now,
    /// under the same id. Cleaned as [`Store::add`] cleans what it keeps;
    /// refused when it says what another entry says, or what was forgotten.
    /// Gives the entry back.
    pub fn reword(
        &mut self,
        project: &Path,
        id: u64,
        text: &str,
        checkout: &Path,
    ) -> Result<Entry> {
        let text = clean(&secrets::redact(text.trim()));
        let key = key_of(&text);
        if key.is_empty() {
            bail!("it says nothing");
        }
        let name = self.ready(project)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry = get(&tx, &name, id)?.with_context(|| format!("there's no entry {id}"))?;
        let other: Option<u64> = tx
            .query_row(
                "SELECT id FROM entries WHERE project = ?1 AND key = ?2 AND id != ?3",
                params![name, key, id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(other) = other {
            bail!("it says what entry {other} says");
        }
        let forgotten: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM forgotten WHERE project = ?1 AND key = ?2)",
            params![name, hash(&key)],
            |row| row.get(0),
        )?;
        if forgotten {
            bail!("it says what was forgotten");
        }
        let mut code = Code::default();
        tx.execute(
            "UPDATE entries SET text = ?3, key = ?4, anchors = ?5, names = ?6, checkout = ?7, \
             told_stale = 0 WHERE project = ?1 AND id = ?2",
            params![
                name,
                id,
                text,
                key,
                serde_json::to_string(&anchors_in(checkout, &entry.files))?,
                serde_json::to_string(&code.names_in(checkout, &text, &entry.files))?,
                checkout.to_string_lossy(),
            ],
        )?;
        let entry = get(&tx, &name, id)?.context("the entry just reworded is gone")?;
        tx.commit()?;
        Ok(entry)
    }

    /// What `text`, being added to the project called `project`, comes to
    /// by its meaning: see [`Store::add_with`]. Whether it says what one
    /// forgotten said is only looked for with `forgotten`, and while no
    /// entry there says it.
    fn meaning(
        &mut self,
        project: &str,
        text: &str,
        embedder: &dyn Embed,
        forgotten: bool,
    ) -> Result<Meant> {
        let entries = self.with_vectors(project, embedder)?;
        let apart = self.apart(project, embedder.model())?;
        let vector = embedder
            .embed_passages(&[text])?
            .pop()
            .context("the model gave no vector")?;
        if !embed::is_numbers(&vector) {
            bail!("the model gave a vector that isn't numbers");
        }
        // What each entry says, in its words and those merged into it.
        let own = entries
            .iter()
            .map(|(entry, other)| (entry.id, &entry.text, other));
        let merged = (apart.merged.iter()).map(|(kept, said, other)| (*kept, said, other));
        let mut said: Vec<(f32, &str, u64)> = own
            .chain(merged)
            .map(|(id, said, other)| (dot(&vector, other), said.as_str(), id))
            .filter(|(score, ..)| *score >= embedder.alike_from())
            .collect();
        said.sort_by(|a, b| b.0.total_cmp(&a.0));
        let alike: Vec<(f32, &str)> = said
            .iter()
            .map(|(score, said, _)| (*score, *said))
            .collect();
        let twin = saying_the_same(text, &alike, embedder)
            .first()
            .map(|(at, _)| said[*at].2);
        let forgotten = forgotten && twin.is_none() && {
            let mut said: Vec<(f32, &str)> = (apart.forgotten.iter())
                .map(|(said, other)| (dot(&vector, other), said.as_str()))
                .filter(|(score, _)| *score >= embedder.alike_from())
                .collect();
            said.sort_by(|a, b| b.0.total_cmp(&a.0));
            !saying_the_same(text, &said, embedder).is_empty()
        };
        Ok(Meant {
            vector,
            twin,
            forgotten,
        })
    }

    /// What the project called `project` keeps apart, with its vectors from
    /// the model called `model`: each entry merged into one still there,
    /// with that one's id and what it said, and what each forgotten entry
    /// said. Those with no vector yet ([`Store::embed_missing_in`] gives
    /// them one) are left out.
    fn apart(&self, project: &str, model: &str) -> Result<Apart> {
        let vector = |row: &rusqlite::Row, at: usize| -> rusqlite::Result<Vec<f32>> {
            Ok(vector_of(&row.get::<_, Vec<u8>>(at)?))
        };
        let mut merged = self.conn.prepare(
            "SELECT m.kept, m.text, a.vector FROM merged m \
             JOIN apart_vectors a ON a.project = m.project AND a.key = m.key AND a.model = ?2 \
             JOIN entries e ON e.project = m.project AND e.id = m.kept \
             WHERE m.project = ?1 ORDER BY m.id",
        )?;
        let merged = merged.query_map(params![project, model], |row| {
            Ok((row.get(0)?, row.get(1)?, vector(row, 2)?))
        })?;
        let merged = merged.collect::<rusqlite::Result<_>>()?;
        let mut forgotten = self.conn.prepare(
            "SELECT f.text, a.vector FROM forgotten f \
             JOIN apart_vectors a ON a.project = f.project AND a.key = f.key AND a.model = ?2 \
             WHERE f.project = ?1 AND f.text IS NOT NULL ORDER BY f.id",
        )?;
        let forgotten = forgotten.query_map(params![project, model], |row| {
            Ok((row.get(0)?, vector(row, 1)?))
        })?;
        let forgotten = forgotten.collect::<rusqlite::Result<_>>()?;
        Ok(Apart { merged, forgotten })
    }

    /// Every entry of the project called `project` but tasks' outcomes,
    /// with its vector from `embedder`, in the order they were added: those
    /// with none yet get one first.
    fn with_vectors(
        &mut self,
        project: &str,
        embedder: &dyn Embed,
    ) -> Result<Vec<(Entry, Vec<f32>)>> {
        self.embed_missing_in(Some(project), embedder)?;
        let mut rows = self.conn.prepare(&format!(
            "SELECT {COLUMNS}, v.vector FROM entries e JOIN vectors v ON v.n = e.n \
             WHERE e.project = ?1 AND v.model = ?2 AND e.kind != ?3 ORDER BY e.id"
        ))?;
        let outcome = Kind::Outcome.to_string();
        let rows = rows.query_map(params![project, embedder.model(), outcome], |row| {
            Ok((entry_of(row)?, vector_of(&row.get::<_, Vec<u8>>("vector")?)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The entries of `project` but tasks' outcomes nearest in meaning to
    /// any of `texts`, the nearest first, as many as `limit`: each as near
    /// as it is to the nearest of them. For the distiller, to be shown what
    /// the memory has already on what it's about to say.
    pub fn nearest(
        &mut self,
        project: &Path,
        texts: &[&str],
        limit: usize,
        embedder: &dyn Embed,
    ) -> Result<Vec<Entry>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let name = self.ready(project)?;
        let entries = self.with_vectors(&name, embedder)?;
        let asked = embedder.embed_passages(texts)?;
        let mut near: Vec<(f32, Entry)> = entries
            .into_iter()
            .filter_map(|(entry, vector)| {
                let nearest = asked
                    .iter()
                    .map(|text| dot(text, &vector))
                    .filter(|score| score.is_finite())
                    .max_by(f32::total_cmp)?;
                Some((nearest, entry))
            })
            .collect();
        near.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(near.into_iter().take(limit).map(|(_, e)| e).collect())
    }

    /// The entries of `project` that say what another does, grouped about
    /// the one each group keeps, for `crystal memory dedupe`: each entry
    /// read as though it were being added now, by [`Store::add_with`]'s
    /// check, against every other, and the groups as [`grouped`] makes
    /// them, the one kept earliest first.
    pub fn twins(&mut self, project: &Path, embedder: &dyn Embed) -> Result<Vec<Merge>> {
        let name = self.ready(project)?;
        let entries = self.with_vectors(&name, embedder)?;
        let same: Vec<Vec<Twin>> = entries
            .iter()
            .map(|(entry, vector)| {
                let mut alike = alike_to(vector, &entries, embedder.alike_from());
                alike.retain(|(_, other)| other.id != entry.id);
                same_as(&entry.text, alike, embedder)
            })
            .collect();
        let entries: Vec<Entry> = entries.into_iter().map(|(entry, _)| entry).collect();
        let mut merges = grouped(&entries, &same);
        merges.sort_by_key(|merge| merge.kept.id);
        Ok(merges)
    }

    /// Merges each of `merges` into the entry it keeps, as it is now, those
    /// changed or gone since left out: the one kept counts every time each
    /// was said, was last said when the latest of them was, and is about
    /// every file each was, and was used when the latest of them was, so it
    /// doesn't expire. It holds as well as the freshest of them: when one
    /// still holds, it's anchored to its files and what it names as they
    /// are now, as it would be said again; otherwise each file as the
    /// latest said of them that names it was, and what it names as it was.
    /// Those merged leave the list, kept apart as they were with their
    /// vectors, so their words said again, or what they mean, count as the
    /// one kept said again, and their words and meaning find it in a
    /// search. Gives back what it merged, each one kept as it is now.
    pub fn merge(&mut self, project: &Path, merges: &[Merge]) -> Result<Vec<Merge>> {
        let name = self.ready(project)?;
        let now = seconds_since_epoch(SystemTime::now());
        // Read before the database is held, as adding reads it: whether
        // each still holds, by the code its worktree has, and that code,
        // for what the one kept names.
        let mut code = Code::default();
        let mut holds: HashMap<u64, bool> = HashMap::new();
        for merge in merges {
            let merged = merge.merged.iter().map(|twin| &twin.entry);
            for entry in std::iter::once(&merge.kept).chain(merged) {
                let fresh = code.holds(entry, project).0 == Freshness::Fresh;
                holds.insert(entry.id, fresh);
                if fresh {
                    let checkout = entry.checkout.as_deref().filter(|dir| dir.is_dir());
                    code.names_in(checkout.unwrap_or(project), &merge.kept.text, &[]);
                }
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut done = Vec::new();
        for merge in merges {
            let Some(kept) = get(&tx, &name, merge.kept.id)? else {
                continue;
            };
            let mut merged = Vec::new();
            for twin in &merge.merged {
                match get(&tx, &name, twin.entry.id)? {
                    Some(entry) if entry.text == twin.entry.text => merged.push(Twin {
                        entry,
                        ..twin.clone()
                    }),
                    _ => {}
                }
            }
            if merged.is_empty() {
                continue;
            }
            let mut said: Vec<&Entry> = merged.iter().map(|twin| &twin.entry).collect();
            let files = said.iter().fold(kept.files.clone(), |files, entry| {
                joined(&files, &entry.files)
            });
            said.push(&kept);
            said.sort_by_key(|entry| (entry.last_seen, entry.id));
            let latest = said.last().expect("the one kept at least");
            let holding = said
                .iter()
                .find(|entry| holds.get(&entry.id).copied().unwrap_or(false));
            // One of them holds still, so all of it does, as it does said
            // again; otherwise each file as the latest said of them that
            // names it was, and what the one kept names as it was.
            let (anchors, names, checkout) = match holding {
                Some(entry) => {
                    let checkout = entry.checkout.as_deref().filter(|dir| dir.is_dir());
                    let top = checkout.unwrap_or(project);
                    let names = code.names_in(top, &kept.text, &files);
                    (
                        anchors_in(top, &files),
                        names,
                        checkout.map(Path::to_path_buf),
                    )
                }
                None => {
                    let mut anchors = BTreeMap::new();
                    for entry in &said {
                        anchors.extend(entry.anchors.clone());
                    }
                    (anchors, kept.names.clone(), latest.checkout.clone())
                }
            };
            let seen: u32 = said.iter().map(|entry| entry.seen).sum();
            let used = said.iter().filter_map(|entry| entry.used).max();
            tx.execute(
                "UPDATE entries SET seen = ?3, last_seen = ?4, files = ?5, anchors = ?6, \
                 checkout = ?7, told_stale = told_stale AND ?8, used = ?9, names = ?10 \
                 WHERE project = ?1 AND id = ?2",
                params![
                    name,
                    kept.id,
                    seen,
                    latest.last_seen,
                    serde_json::to_string(&files)?,
                    serde_json::to_string(&anchors)?,
                    path_text(&checkout),
                    holding.is_none(),
                    used,
                    serde_json::to_string(&names)?,
                ],
            )?;
            for Twin { entry, .. } in &merged {
                let key = hash(&key_of(&entry.text));
                keep_apart_vector(&tx, &name, entry.id, &key)?;
                tx.execute(
                    "INSERT OR REPLACE INTO merged (project, key, kept, id, kind, text, files, \
                     source, merged) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        name,
                        key,
                        kept.id,
                        entry.id,
                        entry.kind.to_string(),
                        entry.text,
                        serde_json::to_string(&entry.files)?,
                        serde_json::to_string(&entry.source)?,
                        now,
                    ],
                )?;
                // What went into it before goes with it.
                tx.execute(
                    "UPDATE merged SET kept = ?3 WHERE project = ?1 AND kept = ?2",
                    params![name, entry.id, kept.id],
                )?;
                tx.execute(
                    "DELETE FROM entries WHERE project = ?1 AND id = ?2",
                    params![name, entry.id],
                )?;
            }
            write_merged_words(&tx, &name, kept.id)?;
            let kept = get(&tx, &name, kept.id)?.context("the entry kept is gone")?;
            done.push(Merge { kept, merged });
        }
        tx.commit()?;
        Ok(done)
    }

    /// The entry that entry `id` of `project` was merged into, when it was.
    pub fn merged_into(&mut self, project: &Path, id: u64) -> Result<Option<u64>> {
        let name = self.ready(project)?;
        Ok(self
            .conn
            .query_row(
                "SELECT kept FROM merged WHERE project = ?1 AND id = ?2 LIMIT 1",
                params![name, id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Takes entry `id` out of `project`'s memory, and gives it back. What
    /// it said is kept apart, with a hash of its words and its vector for
    /// the distiller to know not to add it again, in those words or others,
    /// and for [`Store::forgotten`] to list.
    pub fn remove(&mut self, project: &Path, id: u64) -> Result<Entry> {
        let name = self.ready(project)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry = get(&tx, &name, id)?.with_context(|| format!("there's no entry {id}"))?;
        let key = key_of(&entry.text);
        if !key.is_empty() {
            keep_apart_vector(&tx, &name, id, &hash(&key))?;
        }
        tx.execute(
            "DELETE FROM entries WHERE project = ?1 AND id = ?2",
            params![name, id],
        )?;
        if !key.is_empty() {
            tx.execute(
                "INSERT OR REPLACE INTO forgotten (project, key, id, kind, text, files, source, \
                 forgot) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    name,
                    hash(&key),
                    entry.id,
                    entry.kind.to_string(),
                    entry.text,
                    serde_json::to_string(&entry.files)?,
                    serde_json::to_string(&entry.source)?,
                    seconds_since_epoch(SystemTime::now()),
                ],
            )?;
        }
        tx.commit()?;
        Ok(entry)
    }

    /// The entries forgotten in `project`, the latest forgotten first: each
    /// as it was, until it's remembered again. Those forgotten before
    /// crystal kept what they said aren't listed.
    pub fn forgotten(&mut self, project: &Path) -> Result<Vec<Forgotten>> {
        let name = self.ready(project)?;
        let mut query = self.conn.prepare(
            "SELECT id, kind, text, files, source, forgot FROM forgotten \
             WHERE project = ?1 AND text IS NOT NULL ORDER BY forgot DESC, id DESC",
        )?;
        let rows = query.query_map(params![name], |row| {
            let kind: String = row.get(1)?;
            let files: String = row.get(3)?;
            let source: Option<String> = row.get(4)?;
            Ok(Forgotten {
                id: row.get(0)?,
                kind: Kind::parse(&kind).unwrap_or(Kind::Note),
                text: row.get(2)?,
                files: serde_json::from_str(&files).unwrap_or_default(),
                source: source
                    .and_then(|source| serde_json::from_str(&source).ok())
                    .unwrap_or(Source::User),
                forgotten: row.get::<_, Option<u64>>(5)?.unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every entry of `project`, newest first.
    pub fn entries(&mut self, project: &Path) -> Result<Vec<Entry>> {
        let name = self.ready(project)?;
        let mut query = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM entries e WHERE e.project = ?1 ORDER BY e.id DESC"
        ))?;
        let entries = query.query_map(params![name], entry_of)?;
        Ok(entries.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&mut self, project: &Path, id: u64) -> Result<Option<Entry>> {
        let name = self.ready(project)?;
        get(&self.conn, &name, id)
    }

    /// Entry `id` of `project`, read in full by an agent, which notes that
    /// it was: found again, it doesn't expire.
    pub fn used(&mut self, project: &Path, id: u64) -> Result<Option<Entry>> {
        let name = self.ready(project)?;
        self.conn.execute(
            "UPDATE entries SET used = ?3 WHERE project = ?1 AND id = ?2",
            params![name, id, seconds_since_epoch(SystemTime::now())],
        )?;
        get(&self.conn, &name, id)
    }

    /// Makes entry `id` of `project` one of `kind`, and gives it back: what
    /// it says stays as it is, and so do its vector and its place in the
    /// index. Made a note, its time to expire counts from now.
    pub fn set_kind(&mut self, project: &Path, id: u64, kind: Kind) -> Result<Entry> {
        let name = self.ready(project)?;
        let now = seconds_since_epoch(SystemTime::now());
        let from = kind.expires_after().map(|_| now);
        let changed = self.conn.execute(
            "UPDATE entries SET kind = ?3, counted_from = coalesce(?4, counted_from) \
             WHERE project = ?1 AND id = ?2",
            params![name, id, kind.to_string(), from],
        )?;
        if changed == 0 {
            bail!("there's no entry {id}");
        }
        get(&self.conn, &name, id)?.context("the entry just changed is gone")
    }

    /// Every project's entries that are anchored to names or files, each
    /// with its project's main worktree and whether the daemon has told of
    /// it going stale: those that can go stale.
    fn anchored(&mut self) -> Result<Vec<(PathBuf, Entry, bool)>> {
        let mut query = self.conn.prepare(&format!(
            "SELECT {COLUMNS}, e.project, e.told_stale FROM entries e \
             WHERE e.anchors != '{{}}' OR e.names != '[]' ORDER BY e.project, e.id"
        ))?;
        let anchored = query.query_map([], |row| {
            let project: String = row.get("project")?;
            Ok((
                PathBuf::from(project),
                entry_of(row)?,
                row.get("told_stale")?,
            ))
        })?;
        Ok(anchored.collect::<rusqlite::Result<_>>()?)
    }

    /// The entries of `project` about any of `files` that have gone stale,
    /// said most recently first, `limit` at most, each with what's gone:
    /// those to ask about again.
    pub fn stale_about(
        &mut self,
        project: &Path,
        files: &[String],
        limit: usize,
    ) -> Result<Vec<Listed>> {
        // Every one about them: a file nearly every task changes has many.
        let about = self.about_files(project, files, i64::MAX as usize)?;
        let mut stale = marked(about, project);
        stale.retain(|item| item.freshness == Freshness::Stale);
        stale.truncate(limit);
        Ok(stale)
    }

    /// Notes whether the daemon has told of entry `id` of `project` going
    /// stale.
    fn set_told_stale(&mut self, project: &Path, id: u64, told: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET told_stale = ?3 WHERE project = ?1 AND id = ?2",
            params![project.to_string_lossy(), id, told],
        )?;
        Ok(())
    }

    /// The entries of `project` about any of `files`, said most recently
    /// first, `limit` at most.
    fn about_files(
        &mut self,
        project: &Path,
        files: &[String],
        limit: usize,
    ) -> Result<Vec<Entry>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let name = self.ready(project)?;
        let mut about = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM entries e WHERE e.project = ?1 AND EXISTS \
             (SELECT 1 FROM json_each(e.files) WHERE value IN (SELECT value FROM json_each(?2))) \
             ORDER BY e.last_seen DESC, e.id DESC LIMIT ?3"
        ))?;
        let files = serde_json::to_string(files)?;
        let about = about.query_map(params![name, files, limit], entry_of)?;
        Ok(about.collect::<rusqlite::Result<_>>()?)
    }

    /// The entries of `project` that have to do with `text`, of `kind` if
    /// it's given, the best first. By its words, bm25 ranks those with more
    /// of them, and rarer ones, first, then the ones said most recently. Any
    /// word counts, and so does a word it starts or stems from: "test" finds
    /// "tests" and "testing". With an `embedder`, entries that mean much the
    /// same count too, whatever their words, and the two rankings are
    /// merged; then, with its reranker, the best are read again with `text`,
    /// those that don't answer it are left out, and its ranking is merged
    /// in too. A text with no words gives the entries said most recently.
    /// Either way, lessons come before notes and outcomes near them, unless
    /// `text` asks about what was done, and the expired are left out.
    pub fn search(
        &mut self,
        project: &Path,
        text: &str,
        kind: Option<Kind>,
        limit: usize,
        embedder: Option<&dyn Embed>,
    ) -> Result<Vec<Entry>> {
        self.search_about(project, text, kind, &[], limit, embedder)
    }

    /// The entries of `project` that have to do with `text`, as `wanted`
    /// says, each with whether it still holds: the best that
    /// [`Store::search_about`] finds, the expired too if it says so, those
    /// that hold first, as they rank, lessons before the notes near them and
    /// the drifting marked, then the stale, marked; or with `fresh`, the
    /// stale left out.
    pub fn find(
        &mut self,
        project: &Path,
        text: &str,
        wanted: &Wanted,
        embedder: Option<&dyn Embed>,
    ) -> Result<Vec<Listed>> {
        // The stale left out leave room for as many after them, and the
        // notes put after lessons for the lessons below the limit.
        let limit = if wanted.fresh {
            wanted.limit.max(SEARCH_LIMIT)
        } else {
            wanted.limit
        } + NOTES_BEHIND;
        let files: Vec<String> = wanted
            .files
            .iter()
            .map(|file| file.trim_end_matches('/').to_string())
            .filter(|file| !file.is_empty() && file != ".")
            .collect();
        let among = Among {
            kinds: wanted.kind.map_or(Kinds::All, Kinds::Only),
            files: &files,
            expired: wanted.expired,
        };
        let found = self.search_among(project, text, among, limit, embedder)?;
        let (found, stale): (Vec<Listed>, Vec<Listed>) = marked(found, project)
            .into_iter()
            .partition(|item| item.freshness != Freshness::Stale);
        let mut found = lessons_first(found, text, |item| item.entry.kind);
        if !wanted.fresh {
            found.extend(lessons_first(stale, text, |item| item.entry.kind));
        }
        found.truncate(wanted.limit);
        Ok(found)
    }

    /// [`Store::search`], with `files`, among the entries about one of them
    /// alone, or about a file under one of them: each a path from the top
    /// of the project, a file's or a directory's.
    pub fn search_about(
        &mut self,
        project: &Path,
        text: &str,
        kind: Option<Kind>,
        files: &[String],
        limit: usize,
        embedder: Option<&dyn Embed>,
    ) -> Result<Vec<Entry>> {
        let among = Among {
            kinds: kind.map_or(Kinds::All, Kinds::Only),
            files,
            expired: false,
        };
        // Deeper than the limit, so that lessons below it can come up past
        // the notes ahead of them.
        let deeper = limit + NOTES_BEHIND;
        let found = self.search_among(project, text, among, deeper, embedder)?;
        let mut found = lessons_first(found, text, |entry| entry.kind);
        found.truncate(limit);
        Ok(found)
    }

    /// [`Store::search`], among the entries `among` keeps to alone, so that
    /// those left out take no place in either ranking, nor among those the
    /// reranker reads; ranked as they are found, before lessons are put
    /// first.
    fn search_among(
        &mut self,
        project: &Path,
        text: &str,
        among: Among,
        limit: usize,
        embedder: Option<&dyn Embed>,
    ) -> Result<Vec<Entry>> {
        let name = self.ready(project)?;
        let Some(query) = fts_query(text) else {
            return self.newest(&name, among, limit);
        };
        let pool = limit.max(POOL);
        let mut by_words = self.by_words(&name, &query, among, pool)?;
        let Some(embedder) = embedder else {
            by_words.truncate(limit);
            return Ok(by_words);
        };
        match self.by_meaning(&name, text, among, pool, embedder) {
            Ok(by_meaning) => {
                let found = fused(&[by_words, by_meaning], pool);
                let read: Vec<u64> = found.iter().take(RERANK_POOL).map(|e| e.id).collect();
                let merged_words = self.merged_words(&name, &read)?;
                Ok(reranked(found, text, embedder, limit, &merged_words))
            }
            // The model failing leaves the search to the words.
            Err(err) => {
                errln!("crystal: couldn't search by meaning: {err:#}");
                by_words.truncate(limit);
                Ok(by_words)
            }
        }
    }

    /// The entries of the project called `project` whose words match the
    /// FTS5 `query`, the best first.
    fn by_words(
        &self,
        project: &str,
        query: &str,
        among: Among,
        limit: usize,
    ) -> Result<Vec<Entry>> {
        let (only, but, files, now) = among.params();
        let mut found = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM entries_fts JOIN entries e ON e.n = entries_fts.rowid \
             WHERE entries_fts MATCH ?2 AND e.project = ?1 AND (?3 IS NULL OR e.kind = ?3) \
             AND e.kind IS NOT ?5 AND {} AND {} \
             ORDER BY {RANK}, e.last_seen DESC, e.id DESC LIMIT ?4",
            about_files_sql(6),
            unexpired_sql(7)
        ))?;
        let found = found.query_map(
            params![project, query, only, limit, but, files, now],
            entry_of,
        )?;
        Ok(found.collect::<rusqlite::Result<_>>()?)
    }

    /// The entries of the project called `project` that mean much the same
    /// as `text`, as alike as the model counts a match, the most alike
    /// first: each as alike as the most alike of what it says and what the
    /// entries merged into it said. Its entries with no vector yet get one
    /// first.
    fn by_meaning(
        &mut self,
        project: &str,
        text: &str,
        among: Among,
        limit: usize,
        embedder: &dyn Embed,
    ) -> Result<Vec<Entry>> {
        self.embed_missing_in(Some(project), embedder)?;
        let asked = embedder.embed_query(text)?;
        let asked = &asked;
        let (only, but, files, now) = among.params();
        let mut rows = self.conn.prepare(&format!(
            "SELECT {COLUMNS}, v.vector FROM entries e JOIN vectors v ON v.n = e.n \
             WHERE e.project = ?1 AND v.model = ?2 AND (?3 IS NULL OR e.kind = ?3) \
             AND e.kind IS NOT ?4 AND {} AND {}",
            about_files_sql(5),
            unexpired_sql(6)
        ))?;
        let model = embedder.model();
        let rows = rows.query_map(params![project, model, only, but, files, now], |row| {
            Ok((entry_of(row)?, row.get::<_, Vec<u8>>("vector")?))
        })?;
        let mut merged: HashMap<u64, Vec<Vec<f32>>> = HashMap::new();
        for (kept, _, vector) in self.apart(project, model)?.merged {
            merged.entry(kept).or_default().push(vector);
        }
        let mut alike = Vec::new();
        for row in rows {
            let (entry, vector) = row?;
            let merged = merged.get(&entry.id).into_iter().flatten();
            let score = std::iter::once(&vector_of(&vector))
                .chain(merged)
                .map(|vector| dot(asked, vector))
                .filter(|score| score.is_finite())
                .fold(f32::NEG_INFINITY, f32::max);
            if score >= embedder.min_similarity() {
                alike.push((score, entry));
            }
        }
        alike.sort_by(|a, b| b.0.total_cmp(&a.0));
        let best = alike.first().map_or(0.0, |(score, _)| *score);
        let near = best - embedder.near_best();
        Ok(alike
            .into_iter()
            .take_while(|(score, _)| *score >= near)
            .take(limit)
            .map(|(_, e)| e)
            .collect())
    }

    /// What the entries merged into each of `ids`, entries of the project
    /// called `project`, said, by its id: those with nothing merged into
    /// them left out.
    fn merged_words(&self, project: &str, ids: &[u64]) -> Result<HashMap<u64, String>> {
        let mut rows = self.conn.prepare(
            "SELECT id, merged_words FROM entries WHERE project = ?1 AND merged_words != '' \
             AND id IN (SELECT value FROM json_each(?2))",
        )?;
        let ids = serde_json::to_string(ids)?;
        let rows = rows.query_map(params![project, ids], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The entries of the project called `project` that `among` keeps to,
    /// said most recently first.
    fn newest(&self, project: &str, among: Among, limit: usize) -> Result<Vec<Entry>> {
        let (only, but, files, now) = among.params();
        let mut newest = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM entries e WHERE e.project = ?1 \
             AND (?2 IS NULL OR e.kind = ?2) AND e.kind IS NOT ?4 AND {} AND {} \
             ORDER BY e.last_seen DESC, e.id DESC LIMIT ?3",
            about_files_sql(5),
            unexpired_sql(6)
        ))?;
        let newest = newest.query_map(params![project, only, limit, but, files, now], entry_of)?;
        Ok(newest.collect::<rusqlite::Result<_>>()?)
    }

    /// How many entries every project has, and how many of them have their
    /// vector from the model called `model`.
    pub fn counts(&self, model: &str) -> Result<(usize, usize)> {
        let entries = self
            .conn
            .query_row("SELECT count(*) FROM entries", [], |row| row.get(0))?;
        let embedded = self.conn.query_row(
            "SELECT count(*) FROM vectors v JOIN entries e ON e.n = v.n WHERE v.model = ?1",
            params![model],
            |row| row.get(0),
        )?;
        Ok((entries, embedded))
    }

    /// Lets go of the vectors of every model but `model`: crystal searches
    /// with one, and another's can't be compared with it.
    pub fn forget_vectors_but(&mut self, model: &str) -> Result<usize> {
        let gone = self
            .conn
            .execute("DELETE FROM vectors WHERE model != ?1", params![model])?;
        let apart = self.conn.execute(
            "DELETE FROM apart_vectors WHERE model != ?1",
            params![model],
        )?;
        Ok(gone + apart)
    }

    /// Gives every entry, in every project, that has no vector from
    /// `embedder` one, and what was merged and forgotten too, a vector
    /// that isn't numbers ([`embed::is_numbers`]) let go first, to be made
    /// again; and says how many that was.
    pub fn embed_missing(&mut self, embedder: &dyn Embed) -> Result<usize> {
        let broken = self.forget_broken_vectors()?;
        if broken > 0 {
            errln!("crystal: {broken} of memory's vectors weren't numbers; making them again");
        }
        self.embed_missing_in(None, embedder)
    }

    /// Lets go of every vector kept that isn't numbers, and says how many.
    fn forget_broken_vectors(&mut self) -> Result<usize> {
        let mut broken = 0;
        for (table, key) in [("vectors", "n"), ("apart_vectors", "rowid")] {
            let ids: Vec<i64> = {
                let mut rows = self
                    .conn
                    .prepare(&format!("SELECT {key}, vector FROM {table}"))?;
                let rows = rows.query_map([], |row| {
                    Ok((row.get(0)?, vector_of(&row.get::<_, Vec<u8>>(1)?)))
                })?;
                let rows: Vec<(i64, Vec<f32>)> = rows.collect::<rusqlite::Result<_>>()?;
                rows.into_iter()
                    .filter(|(_, vector)| !embed::is_numbers(vector))
                    .map(|(id, _)| id)
                    .collect()
            };
            for id in &ids {
                self.conn.execute(
                    &format!("DELETE FROM {table} WHERE {key} = ?1"),
                    params![id],
                )?;
            }
            broken += ids.len();
        }
        Ok(broken)
    }

    /// Gives the entries of the project called `project`, or of every
    /// project, that have no vector from `embedder` one, and what was
    /// merged or forgotten there too (but for what was forgotten before
    /// crystal kept what it said). A vector the model gives that isn't
    /// numbers, even once made again, isn't kept: what has none is tried
    /// again next time.
    fn embed_missing_in(&mut self, project: Option<&str>, embedder: &dyn Embed) -> Result<usize> {
        let model = embedder.model();
        let missing: Vec<(i64, String)> = {
            let mut missing = self.conn.prepare(
                "SELECT e.n, e.text FROM entries e \
                 LEFT JOIN vectors v ON v.n = e.n AND v.model = ?2 \
                 WHERE v.n IS NULL AND (?1 IS NULL OR e.project = ?1)",
            )?;
            let missing = missing.query_map(params![project, model], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            missing.collect::<rusqlite::Result<_>>()?
        };
        for batch in missing.chunks(EMBED_BATCH) {
            let texts: Vec<&str> = batch.iter().map(|(_, text)| text.as_str()).collect();
            let vectors = embedder.embed_passages(&texts)?;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for ((n, text), vector) in batch.iter().zip(&vectors) {
                if !embed::is_numbers(vector) {
                    continue;
                }
                // Only for the entry as it was read: it may have gone since.
                tx.execute(
                    "INSERT OR REPLACE INTO vectors (n, model, vector) \
                     SELECT ?1, ?2, ?3 WHERE EXISTS \
                     (SELECT 1 FROM entries WHERE n = ?1 AND text = ?4)",
                    params![n, model, bytes_of(vector), text],
                )?;
            }
            tx.commit()?;
        }
        let apart: Vec<(String, String, String)> = {
            let mut apart = self.conn.prepare(
                "SELECT m.project, m.key, m.text FROM merged m \
                 WHERE (?1 IS NULL OR m.project = ?1) AND NOT EXISTS (SELECT 1 FROM apart_vectors a \
                   WHERE a.project = m.project AND a.key = m.key AND a.model = ?2) \
                 UNION ALL SELECT f.project, f.key, f.text FROM forgotten f \
                 WHERE f.text IS NOT NULL AND (?1 IS NULL OR f.project = ?1) \
                   AND NOT EXISTS (SELECT 1 FROM apart_vectors a \
                   WHERE a.project = f.project AND a.key = f.key AND a.model = ?2)",
            )?;
            let apart = apart.query_map(params![project, model], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            apart.collect::<rusqlite::Result<_>>()?
        };
        for batch in apart.chunks(EMBED_BATCH) {
            let texts: Vec<&str> = batch.iter().map(|(_, _, text)| text.as_str()).collect();
            let vectors = embedder.embed_passages(&texts)?;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for ((project, key, _), vector) in batch.iter().zip(&vectors) {
                if embed::is_numbers(vector) {
                    tx.execute(
                        "INSERT OR REPLACE INTO apart_vectors (project, key, model, vector) \
                         VALUES (?1, ?2, ?3, ?4)",
                        params![project, key, model, bytes_of(vector)],
                    )?;
                }
            }
            tx.commit()?;
        }
        Ok(missing.len() + apart.len())
    }

    /// `project`'s name in the database, once it has its row there: the
    /// first time, with the entries it kept in its own file before the
    /// database brought in, ids and all.
    fn ready(&mut self, project: &Path) -> Result<String> {
        let name = project.to_string_lossy().into_owned();
        let known = |conn: &Connection| -> rusqlite::Result<bool> {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM projects WHERE path = ?1)",
                params![name],
                |row| row.get(0),
            )
        };
        if known(&self.conn)? {
            return Ok(name);
        }
        let old_file = old_file_for(&self.dir, project);
        let old = load_old(&old_file)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Another process may have got here first.
        if !known(&tx)? {
            let entries = old.as_ref().map_or(&[][..], |old| &old.entries[..]);
            let past = entries.iter().map(|entry| entry.id + 1).max().unwrap_or(1);
            let next_id = old.as_ref().map_or(1, |old| old.next_id).max(past);
            tx.execute(
                "INSERT INTO projects (path, next_id) VALUES (?1, ?2)",
                params![name, next_id],
            )?;
            let mut code = Code::default();
            for entry in entries {
                let mut entry = entry.clone();
                entry.seen = entry.seen.max(1);
                if entry.last_seen == 0 {
                    entry.last_seen = entry.created;
                }
                let said = entry.last_seen.max(entry.created);
                entry.anchors = anchors_from_before(&entry.files, said, project);
                entry.names = code.names_in(project, &entry.text, &entry.files);
                // Brought in now, it has as long as any other to be found.
                entry.counted_from = Some(seconds_since_epoch(SystemTime::now()));
                insert(&tx, &name, &entry)?;
            }
        }
        tx.commit()?;
        if old.is_some() {
            // Kept, should anyone want it, but never read again.
            let _ = fs::rename(&old_file, old_file.with_extension("json.imported"));
        }
        Ok(name)
    }
}

/// Brings the database up to date: makes its tables, or adds what a
/// newer crystal keeps.
fn migrate(conn: &mut Connection) -> Result<()> {
    let version = |conn: &Connection| -> rusqlite::Result<usize> {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
    };
    if version(conn)? >= MIGRATIONS.len() {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Another process may have got here first.
    let done = version(&tx)?;
    for step in MIGRATIONS.iter().skip(done) {
        step(&tx)?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len()))?;
    tx.commit()?;
    Ok(())
}

/// Adds [`ANCHORS`], and anchors the entries already there, once, the
/// way [`anchors_from_before`] does.
fn add_anchors(conn: &Connection) -> Result<()> {
    conn.execute_batch(ANCHORS)?;
    let old: Vec<(i64, String, String, u64)> = {
        let mut old = conn.prepare(
            "SELECT n, project, files, max(created, last_seen) FROM entries WHERE files != '[]'",
        )?;
        let old = old.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        old.collect::<rusqlite::Result<_>>()?
    };
    for (n, project, files, said) in old {
        let files: Vec<String> = serde_json::from_str(&files).unwrap_or_default();
        let anchors = anchors_from_before(&files, said, Path::new(&project));
        conn.execute(
            "UPDATE entries SET anchors = ?2 WHERE n = ?1",
            params![n, serde_json::to_string(&anchors)?],
        )?;
    }
    Ok(())
}

/// Adds [`NAMES`], and anchors each entry already there to what it names
/// that's in its worktree's code now, while that's there, or else its
/// project's: what it names that's gone already says nothing either way,
/// as a file that isn't there isn't anchored.
fn add_names(conn: &Connection) -> Result<()> {
    conn.execute_batch(NAMES)?;
    let old: Vec<(i64, String, String, String, Option<String>)> = {
        let mut old = conn.prepare("SELECT n, project, text, files, checkout FROM entries")?;
        let old = old.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        old.collect::<rusqlite::Result<_>>()?
    };
    let mut code = Code::default();
    for (n, project, text, files, checkout) in old {
        let checkout = checkout
            .map(PathBuf::from)
            .filter(|checkout| checkout.is_dir())
            .unwrap_or_else(|| PathBuf::from(project));
        let files: Vec<String> = serde_json::from_str(&files).unwrap_or_default();
        let names = code.names_in(&checkout, &text, &files);
        if !names.is_empty() {
            conn.execute(
                "UPDATE entries SET names = ?2 WHERE n = ?1",
                params![n, serde_json::to_string(&names)?],
            )?;
        }
    }
    Ok(())
}

/// Shortens each task's outcome entry an earlier crystal kept to its goal
/// in a sentence and what `crystal done` said, as [`short_outcome`] does:
/// each kept the whole brief its task was given, pages of it at times. The
/// brief is still in the task's record, in its project's history.
fn shorten_outcomes(conn: &Connection) -> Result<()> {
    let outcomes: Vec<(i64, String)> = {
        let mut outcomes = conn.prepare("SELECT n, text FROM entries WHERE kind = ?1")?;
        let outcomes = outcomes.query_map(params![Kind::Outcome.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        outcomes.collect::<rusqlite::Result<_>>()?
    };
    for (n, text) in outcomes {
        if let Some(short) = short_outcome(&text) {
            conn.execute(
                "UPDATE entries SET text = ?2, key = ?3 WHERE n = ?1",
                params![n, short, key_of(&short)],
            )?;
        }
    }
    Ok(())
}

/// An outcome entry as an earlier crystal kept it, `<goal>: <summary>` or
/// `<goal> (failed): <summary>`, with its goal cut to its first sentence:
/// `None` when that leaves it no shorter. What `crystal done` said is its
/// last line, and the goal everything before it.
fn short_outcome(text: &str) -> Option<String> {
    const FAILED: &str = " (failed): ";
    let text = text.trim_end();
    let last = text.rfind('\n').map_or(0, |at| at + 1);
    let line = &text[last..];
    let (goal_ends, how, said) = match line.find(FAILED) {
        Some(at) => (at, " (failed)", &line[at + FAILED.len()..]),
        None => {
            let at = goal_end(line)?;
            (at, "", &line[at + 2..])
        }
    };
    let goal = goal_sentence(&text[..last + goal_ends]);
    if goal.is_empty() || said.trim().is_empty() {
        return None;
    }
    let short = format!("{goal}{how}: {}", said.trim());
    (short.len() < text.len()).then_some(short)
}

/// Where, in the last line of an outcome entry, its goal ends and what
/// `crystal done` said begins: at the first `: ` that follows the end of a
/// sentence and comes before what could start one, or else at the first,
/// which may keep some of the goal but never loses any of what was said.
fn goal_end(line: &str) -> Option<usize> {
    let colons: Vec<usize> = line.match_indices(": ").map(|(at, _)| at).collect();
    let after_a_sentence = colons.iter().copied().find(|&at| {
        let before = line[..at].chars().next_back();
        let after = line[at + 2..].chars().next();
        before.is_some_and(|c| ".?!)`\"'".contains(c)) && after.is_some_and(|c| !c.is_lowercase())
    });
    after_a_sentence.or(colons.first().copied())
}

/// A task's goal as its outcome keeps it: its first line that says
/// something, without a heading's or a list item's marks, to the end of
/// its first sentence, its last full stop left off, and at most
/// [`OUTCOME_GOAL`] characters, cut after a word.
fn goal_sentence(goal: &str) -> String {
    let line = goal
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let line = line.trim_start_matches('#').trim_start();
    let line = ["- ", "* "]
        .iter()
        .find_map(|mark| line.strip_prefix(mark))
        .unwrap_or(line);
    let sentence = &line[..sentence_end(line).unwrap_or(line.len())];
    let sentence = one_line(sentence.strip_suffix('.').unwrap_or(sentence));
    if sentence.chars().count() <= OUTCOME_GOAL {
        return sentence;
    }
    let mut cut: String = sentence.chars().take(OUTCOME_GOAL).collect();
    if let Some(space) = cut.rfind(' ') {
        cut.truncate(space);
    }
    cut.push('…');
    cut
}

/// Where the first sentence of `line` ends, just after its `.`, `?` or `!`
/// with a space or nothing after it: not a number's full stop (`1. `), nor
/// that of a letter alone (`e.g. `).
fn sentence_end(line: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    chars.iter().enumerate().find_map(|(i, &(at, c))| {
        if !matches!(c, '.' | '?' | '!') {
            return None;
        }
        if chars
            .get(i + 1)
            .is_some_and(|(_, next)| !next.is_whitespace())
        {
            return None;
        }
        if c == '.' {
            let before = &chars[..i];
            let letters = before.iter().rev().take_while(|(_, c)| c.is_alphabetic());
            let closing = before.last().is_some_and(|(_, c)| ")`\"'".contains(*c));
            if letters.count() < 2 && !closing {
                return None;
            }
        }
        Some(at + c.len_utf8())
    })
}

/// Adds [`COUNTED_FROM`], and starts the time of every entry there to
/// expire now: before, crystal didn't tell whether an entry was found
/// again, so one said a month ago would expire the day it upgraded,
/// however much it was read.
fn count_from_now(conn: &Connection) -> Result<()> {
    conn.execute_batch(COUNTED_FROM)?;
    conn.execute(
        "UPDATE entries SET counted_from = ?1",
        params![seconds_since_epoch(SystemTime::now())],
    )?;
    Ok(())
}

fn get(conn: &Connection, project: &str, id: u64) -> Result<Option<Entry>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM entries e WHERE e.project = ?1 AND e.id = ?2"),
            params![project, id],
            entry_of,
        )
        .optional()?)
}

fn insert(conn: &Connection, project: &str, entry: &Entry) -> Result<()> {
    conn.execute(
        "INSERT INTO entries (project, id, kind, text, key, files, source, created, seen, \
         last_seen, anchors, checkout, names, counted_from) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            project,
            entry.id,
            entry.kind.to_string(),
            entry.text,
            key_of(&entry.text),
            serde_json::to_string(&entry.files)?,
            serde_json::to_string(&entry.source)?,
            entry.created,
            entry.seen,
            entry.last_seen,
            serde_json::to_string(&entry.anchors)?,
            path_text(&entry.checkout),
            serde_json::to_string(&entry.names)?,
            entry.counted_from,
        ],
    )?;
    Ok(())
}

/// Counts `said`, an entry of the project called `project`, said again at
/// `now` as `new`, which says the same thing: with any new files `new`
/// names added to its own, and, said again, holding for its files and what
/// it names as they are now in `checkout`, looked at in `code`. Gives it
/// back as it is then.
fn seen_again(
    conn: &Connection,
    project: &str,
    said: &Entry,
    new: &New,
    checkout: &Path,
    now: u64,
    code: &mut Code,
) -> Result<Entry> {
    let files = joined(&said.files, &new.files);
    let anchors = anchors_in(checkout, &files);
    conn.execute(
        "UPDATE entries SET seen = seen + 1, last_seen = ?3, files = ?4, anchors = ?5, \
         checkout = ?6, names = ?7, told_stale = 0 WHERE project = ?1 AND id = ?2",
        params![
            project,
            said.id,
            now,
            serde_json::to_string(&files)?,
            serde_json::to_string(&anchors)?,
            path_text(&new.checkout),
            serde_json::to_string(&code.names_in(checkout, &said.text, &files))?,
        ],
    )?;
    get(conn, project, said.id)?.context("the entry just seen is gone")
}

/// Keeps the vector of entry `id` of the project called `project` as it
/// goes, merged or forgotten, under `key`, the hash of its key, so what it
/// said is still told by its meaning: see [`APART`].
fn keep_apart_vector(conn: &Connection, project: &str, id: u64, key: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO apart_vectors (project, key, model, vector) \
         SELECT ?1, ?3, v.model, v.vector FROM vectors v JOIN entries e ON e.n = v.n \
         WHERE e.project = ?1 AND e.id = ?2",
        params![project, id, key],
    )?;
    Ok(())
}

/// Writes down, beside entry `id` of the project called `project`, what
/// the entries merged into it said, a line each, for their words to find
/// it: see [`APART`].
fn write_merged_words(conn: &Connection, project: &str, id: u64) -> Result<()> {
    conn.execute(
        "UPDATE entries SET merged_words = coalesce((SELECT group_concat(m.text, char(10)) \
         FROM merged m WHERE m.project = ?1 AND m.kept = ?2), '') \
         WHERE project = ?1 AND id = ?2",
        params![project, id],
    )?;
    Ok(())
}

/// Of `entries`, those at least `from` alike to `vector`, the most alike
/// first, each with how alike. A vector the model made nothing of, which
/// it can, is alike to none.
fn alike_to<'a>(
    vector: &[f32],
    entries: &'a [(Entry, Vec<f32>)],
    from: f32,
) -> Vec<(f32, &'a Entry)> {
    let mut alike: Vec<(f32, &Entry)> = entries
        .iter()
        .map(|(entry, other)| (dot(vector, other), entry))
        .filter(|(score, _)| *score >= from)
        .collect();
    alike.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    alike
}

/// Of `alike`, the entries said before at least [`Embed::alike_from`]
/// alike to `text`, the most alike first, those that say what `text` does,
/// as [`saying_the_same`] finds them.
fn same_as(text: &str, alike: Vec<(f32, &Entry)>, embedder: &dyn Embed) -> Vec<Twin> {
    let texts: Vec<(f32, &str)> = alike
        .iter()
        .map(|(score, entry)| (*score, entry.text.as_str()))
        .collect();
    saying_the_same(text, &texts, embedder)
        .into_iter()
        .map(|(at, reranked)| Twin {
            entry: alike[at].1.clone(),
            alike: alike[at].0,
            reranked,
        })
        .collect()
}

/// Of `alike`, what was said before, each with how alike it is to `text`,
/// the most alike first, those that say what `text` does, by their places
/// in it, each with what the reranker scored it when it was asked: every
/// one at least [`Embed::same_from`] alike, and of those at least
/// [`Embed::alike_from`] alike, those the reranker, reading `text` as the
/// query with the first [`TWIN_POOL`] of them, scores at least
/// [`Embed::same_reranked_from`]. Without a reranker, or with it failing,
/// only the first.
fn saying_the_same(
    text: &str,
    alike: &[(f32, &str)],
    embedder: &dyn Embed,
) -> Vec<(usize, Option<f32>)> {
    let pool: Vec<(usize, f32, &str)> = (alike.iter().enumerate())
        .filter(|(_, (score, _))| *score >= embedder.alike_from())
        .take(TWIN_POOL)
        .map(|(at, (score, said))| (at, *score, *said))
        .collect();
    let unsure = pool
        .iter()
        .any(|(_, score, _)| *score < embedder.same_from());
    let scores = if unsure {
        let passages: Vec<&str> = pool.iter().map(|(_, _, said)| *said).collect();
        match embedder.rerank(text, &passages) {
            Ok(Some(scores)) if scores.len() == pool.len() => Some(scores),
            Ok(Some(_)) => {
                errln!("crystal: the reranker didn't score every entry");
                None
            }
            Ok(None) => None,
            Err(err) => {
                errln!("crystal: couldn't rerank: {err:#}");
                None
            }
        }
    } else {
        None
    };
    pool.into_iter()
        .enumerate()
        .filter_map(|(read, (at, score, _))| {
            let reranked = scores.as_ref().map(|scores| scores[read]);
            let same = score >= embedder.same_from()
                || reranked.is_some_and(|reranked| reranked >= embedder.same_reranked_from());
            same.then_some((at, reranked))
        })
        .collect()
}

/// `entries` grouped about the ones kept, by `same`, the entries each of
/// them says what they do (in its order). The one kept is, first, one
/// someone remembered rather than one the distiller said; then the one most
/// of the rest say the same as, the clearest statement of what they all say;
/// then the earliest. Those that say what it does go into it, each straight
/// into one kept that it says the same as itself, never through another.
/// Those remembered are all kept or gone before any the distiller said is
/// kept, so one remembered never goes into one the distiller said. Groups
/// of one are left out.
fn grouped(entries: &[Entry], same: &[Vec<Twin>]) -> Vec<Merge> {
    let at: HashMap<u64, usize> = entries
        .iter()
        .enumerate()
        .map(|(n, entry)| (entry.id, n))
        .collect();
    // The entries that say what each does, by their places, and how alike.
    let mut saying: Vec<Vec<(usize, &Twin)>> = vec![Vec::new(); entries.len()];
    for (n, twins) in same.iter().enumerate() {
        for twin in twins {
            if let Some(&kept) = at.get(&twin.entry.id) {
                saying[kept].push((n, twin));
            }
        }
    }
    let mut left = vec![true; entries.len()];
    let mut merges = Vec::new();
    loop {
        let into = |kept: usize| -> Vec<(usize, &Twin)> {
            saying[kept]
                .iter()
                .copied()
                .filter(|(n, _)| left[*n])
                .collect()
        };
        let next = (0..entries.len()).filter(|&n| left[n]).min_by_key(|&n| {
            let entry = &entries[n];
            (
                entry.source.is_crystal(),
                std::cmp::Reverse(into(n).len()),
                entry.id,
            )
        });
        let Some(kept) = next else {
            break;
        };
        let merged = into(kept);
        left[kept] = false;
        for (n, _) in &merged {
            left[*n] = false;
        }
        if !merged.is_empty() {
            merges.push(Merge {
                kept: entries[kept].clone(),
                merged: merged
                    .into_iter()
                    .map(|(n, twin)| Twin {
                        entry: entries[n].clone(),
                        ..twin.clone()
                    })
                    .collect(),
            });
        }
    }
    merges
}

/// A path as the database keeps it.
fn path_text(path: &Option<PathBuf>) -> Option<String> {
    path.as_ref()
        .map(|path| path.to_string_lossy().into_owned())
}

/// An entry from a row of [`COLUMNS`]. What doesn't read, say a row
/// changed by hand, falls back to what fits best rather than failing the
/// whole read.
fn entry_of(row: &rusqlite::Row) -> rusqlite::Result<Entry> {
    let kind: String = row.get(1)?;
    let files: String = row.get(3)?;
    let source: String = row.get(4)?;
    let anchors: String = row.get(8)?;
    let checkout: Option<String> = row.get(9)?;
    let names: String = row.get(11)?;
    Ok(Entry {
        id: row.get(0)?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Note),
        text: row.get(2)?,
        files: serde_json::from_str(&files).unwrap_or_default(),
        source: serde_json::from_str(&source).unwrap_or(Source::User),
        created: row.get(5)?,
        seen: row.get(6)?,
        last_seen: row.get(7)?,
        anchors: serde_json::from_str(&anchors).unwrap_or_default(),
        checkout: checkout.map(PathBuf::from),
        used: row.get(10)?,
        names: serde_json::from_str(&names).unwrap_or_default(),
        counted_from: row.get(12)?,
    })
}

/// `rankings` merged into one, by reciprocal rank fusion: an entry gets
/// `1 / (K + its place)` from each ranking it's in, so one high in both
/// comes before one first in only one. Between equals, the one said most
/// recently comes first.
pub fn fused(rankings: &[Vec<Entry>], limit: usize) -> Vec<Entry> {
    let mut scored: Vec<(f32, &Entry)> = Vec::new();
    for ranking in rankings {
        for (at, entry) in ranking.iter().enumerate() {
            let score = 1.0 / (FUSION_K + at as f32 + 1.0);
            match scored.iter_mut().find(|(_, seen)| seen.id == entry.id) {
                Some((total, _)) => *total += score,
                None => scored.push((score, entry)),
            }
        }
    }
    scored.sort_by(|(a, x), (b, y)| {
        b.total_cmp(a)
            .then(y.last_seen.cmp(&x.last_seen))
            .then(y.id.cmp(&x.id))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry.clone())
        .collect()
}

/// `found`, the best ranked first, read again with `text` by the
/// embedder's reranker: nothing, when not even the best of the first
/// [`RERANK_POOL`] answers `text`; otherwise those of them it doesn't rule
/// out, in the order of the two rankings merged. An entry is read with the
/// words of those merged into it, `merged_words`, by its id, after its own.
/// Without a reranker, or with it failing, `found` as it is.
fn reranked(
    mut found: Vec<Entry>,
    text: &str,
    embedder: &dyn Embed,
    limit: usize,
    merged_words: &HashMap<u64, String>,
) -> Vec<Entry> {
    found.truncate(RERANK_POOL.max(limit));
    let read = &found[..found.len().min(RERANK_POOL)];
    let passages: Vec<String> = read
        .iter()
        .map(|entry| match merged_words.get(&entry.id) {
            Some(words) => format!("{}\n{words}", entry.text),
            None => entry.text.clone(),
        })
        .collect();
    let passages: Vec<&str> = passages.iter().map(String::as_str).collect();
    let scores = match embedder.rerank(text, &passages) {
        Ok(Some(scores)) if scores.len() == read.len() => scores,
        Ok(Some(_)) => {
            errln!("crystal: the reranker didn't score every entry");
            found.truncate(limit);
            return found;
        }
        Ok(None) => {
            found.truncate(limit);
            return found;
        }
        Err(err) => {
            errln!("crystal: couldn't rerank: {err:#}");
            found.truncate(limit);
            return found;
        }
    };
    if scores.iter().all(|score| *score < embedder.answers_from()) {
        return Vec::new();
    }
    let mut answering: Vec<(f32, &Entry)> = scores
        .into_iter()
        .zip(read)
        .filter(|(score, _)| *score >= embedder.kept_from())
        .collect();
    let first: Vec<Entry> = answering
        .iter()
        .map(|(_, entry)| (*entry).clone())
        .collect();
    answering.sort_by(|a, b| b.0.total_cmp(&a.0));
    let second: Vec<Entry> = answering
        .into_iter()
        .map(|(_, entry)| entry.clone())
        .collect();
    fused(&[first, second], limit)
}

/// `found`, the best first, with each note and task's outcome, by its
/// `kind`, ranked [`NOTES_BEHIND`] places lower than it was, a lesson
/// coming first where they meet: unless `asked` is about what was done, as
/// it is. Put after what leaves entries out, so that it counts the places
/// of those shown.
fn lessons_first<T>(found: Vec<T>, asked: &str, kind: impl Fn(&T) -> Kind) -> Vec<T> {
    if about_what_was_done(asked) {
        return found;
    }
    let mut ranked: Vec<(usize, bool, T)> = found
        .into_iter()
        .enumerate()
        .map(|(at, item)| {
            let lesson = kind(&item).is_lesson();
            let at = if lesson { at } else { at + NOTES_BEHIND };
            (at, !lesson, item)
        })
        .collect();
    ranked.sort_by_key(|(at, not_a_lesson, _)| (*at, *not_a_lesson));
    ranked.into_iter().map(|(_, _, item)| item).collect()
}

/// Whether `asked` is about what was done, by one of its words: see
/// [`WHAT_WAS_DONE`].
fn about_what_was_done(asked: &str) -> bool {
    asked
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| WHAT_WAS_DONE.contains(&word.to_lowercase().as_str()))
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// A vector as it's kept: its numbers, four little-endian bytes each.
fn bytes_of(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn vector_of(bytes: &[u8]) -> Vec<f32> {
    let (fours, _) = bytes.as_chunks::<4>();
    fours.iter().map(|four| f32::from_le_bytes(*four)).collect()
}

/// A text as an FTS5 query: its words (letters, digits and `_`), lower
/// case, each once, the commonest left out unless there's nothing else,
/// each quoted as a word to match the start of, joined with OR. So any word
/// matches, and bm25 ranks the entries with more and rarer ones first.
/// Quoted, nothing a person or a prompt types is taken for FTS5's own
/// syntax. `None` for a text with no words.
fn fts_query(text: &str) -> Option<String> {
    let mut words: Vec<String> = Vec::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        let word = word.to_lowercase();
        if !word.is_empty() && !words.contains(&word) {
            words.push(word);
        }
    }
    let telling: Vec<&String> = words
        .iter()
        .filter(|word| !STOP_WORDS.contains(&word.as_str()) && word.chars().count() > 1)
        .collect();
    let picked = if telling.is_empty() {
        words.iter().collect()
    } else {
        telling
    };
    if picked.is_empty() {
        return None;
    }
    let terms: Vec<String> = picked
        .into_iter()
        .take(MAX_QUERY_TERMS)
        .map(|word| format!("\"{word}\"*"))
        .collect();
    Some(terms.join(" OR "))
}

/// What an entry says, in a form that finds the same said again: its
/// words, lower case, one space apart.
fn key_of(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A forgotten entry's key as it's kept: FNV-1a, which is the same on
/// every machine and every version of Rust, unlike the standard hasher.
fn hash(key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// `text` without control characters, which would drive the terminal of
/// whoever reads it, but for line breaks and tabs: see [`printable`].
fn clean(text: &str) -> String {
    printable::text(text).into_owned()
}

/// `a`, then whatever of `b` isn't in it.
fn joined(a: &[String], b: &[String]) -> Vec<String> {
    let mut files = a.to_vec();
    for file in b {
        if !files.contains(file) {
            files.push(file.clone());
        }
    }
    files
}

/// A project's memory, as read at one moment.
#[derive(Debug)]
pub struct Memory {
    /// The project's main worktree, which the entries' files are under.
    pub project: PathBuf,
    entries: Vec<Entry>,
}

/// Whether an entry still holds, by what it names, or with nothing to look
/// for, the files it's about. The fresher sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Freshness {
    /// Everything it names is in the code still; or naming nothing to look
    /// for, none of its files has changed since it was said, or it has none.
    Fresh,
    /// Some of what it names is gone from the code; or naming nothing to
    /// look for, some of its files have changed since: it may hold only in
    /// part.
    Drifting,
    /// Everything it names is gone from the code; or naming nothing to
    /// look for, every one of its files is gone: it may no longer hold at
    /// all.
    Stale,
}

impl Freshness {
    /// What an entry that may not hold as it did is marked with.
    pub fn mark(self) -> Option<&'static str> {
        match self {
            Freshness::Fresh => None,
            Freshness::Drifting => Some("drifting"),
            Freshness::Stale => Some("stale"),
        }
    }
}

/// An entry, with whether it still holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "MemoryListed"))]
pub struct Listed {
    pub entry: Entry,
    pub freshness: Freshness,
    /// What it names that's gone from the code since it was said; or,
    /// naming nothing to look for, the files it's about that are gone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gone: Vec<String>,
}

impl Listed {
    /// How it holds, in words, when it may not: drifting or stale, and
    /// why. `None` for one that's fresh.
    pub fn how_it_holds(&self) -> Option<String> {
        let mark = self.freshness.mark()?;
        let gone = || {
            let gone: Vec<&str> = self.gone.iter().map(String::as_str).collect();
            gone.join(", ")
        };
        let why = match (self.entry.names.is_empty(), self.freshness) {
            (false, Freshness::Stale) => format!("what it names is gone from the code: {}", gone()),
            (false, _) => format!("some of what it names is gone from the code: {}", gone()),
            (true, Freshness::Stale) => "every file it's about is gone".to_string(),
            (true, _) => "some of its files have changed since".to_string(),
        };
        Some(format!("{mark}: {why}"))
    }
}

impl Memory {
    /// The memory of `project` kept for the daemon at `socket`. A project
    /// with nothing remembered yet has an empty one.
    pub fn read(socket: &Path, project: &Path) -> Result<Memory> {
        let entries = Store::open(socket)?.entries(project)?;
        Ok(Memory {
            project: project.to_path_buf(),
            entries,
        })
    }

    /// Every entry, newest first, with whether it still holds.
    pub fn listed(&self) -> Vec<Listed> {
        marked(self.entries.clone(), &self.project)
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

/// Adds an entry to `project`'s memory.
#[cfg(test)]
pub fn add(socket: &Path, project: &Path, new: New) -> Result<Added> {
    Store::open(socket)?.add(project, new)
}

/// Takes the entry `id` out of `project`'s memory, and returns it.
pub fn remove(socket: &Path, project: &Path, id: u64) -> Result<Entry> {
    Store::open(socket)?.remove(project, id)
}

/// The entries of `project`'s memory that say what another does, grouped
/// about the one each group keeps ([`Store::twins`]), and with `apply`,
/// merged into it ([`Store::merge`]). Only the models can tell, so without
/// an `embedder` it's refused.
pub fn dedupe(
    socket: &Path,
    project: &Path,
    embedder: Option<&dyn Embed>,
    apply: bool,
) -> Result<Vec<Merge>> {
    let Some(embedder) = embedder else {
        bail!(
            "only the models that search by meaning can tell which entries say the same thing: \
             `crystal memory embed` downloads them, and `embeddings = true` under `[memory]` in \
             the config turns them on"
        );
    };
    let mut store = Store::open(socket)?;
    let merges = store.twins(project, embedder)?;
    if apply {
        store.merge(project, &merges)
    } else {
        Ok(merges)
    }
}

/// `entries` with whether each still holds, in the order they came.
pub fn marked(entries: Vec<Entry>, project: &Path) -> Vec<Listed> {
    let mut code = Code::default();
    entries
        .into_iter()
        .map(|entry| code.listed(entry, project))
        .collect()
}

/// Who [`for_launch`] tells, which says how it reads the rest of the
/// memory, and whether it's told how to add to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    /// Claude Code in a terminal: it has crystal's MCP tools.
    Claude,
    /// Claude Code as a task in the background: the tools, but it isn't
    /// told how to add.
    Task,
    /// Any other agent: crystal's commands, to read and to add.
    Agent,
}

/// What a session is told of its project's memory as it starts: the
/// entries about files its worktree has changed since its base, `changed`,
/// then those with most to do with what it was asked, `asked`, or else the
/// newest, and how to search and add to it. In each, lessons come before
/// the notes near them, unless it was asked about what was done. Stale and
/// expired entries and tasks' outcomes are left out, and the drifting are
/// marked where they rank. A task in the background isn't told how to add,
/// so it has nothing to be told when nothing is remembered. With an
/// `embedder`, what has to do with what it was asked goes by meaning as
/// well as by words.
pub fn for_launch(
    socket: &Path,
    project: &Path,
    asked: &str,
    changed: &[String],
    reader: Reader,
    embedder: Option<&dyn Embed>,
) -> Result<Option<String>> {
    let mut store = Store::open(socket)?;
    let mut code = Code::default();
    let about_changes = store.about_files(project, changed, SEARCH_LIMIT)?;
    let lasting = Among {
        kinds: Kinds::Lasting,
        files: &[],
        expired: false,
    };
    let found = store.search_among(project, asked, lasting, SEARCH_LIMIT, embedder)?;
    let mut shown = launch_order(vec![about_changes, found], asked, project, &mut code);
    if shown.is_empty() {
        shown = launch_order(vec![store.entries(project)?], asked, project, &mut code);
    }
    Ok(launch_paragraph(&shown, reader))
}

/// The entries a session may be shown as it starts, from `parts`, the most
/// relevant part first: each once, none that's stale, expired or a task's
/// outcome, and in each part in the order it ranked, lessons put before the
/// notes near them unless it was `asked` about what was done, the drifting
/// marked where they rank.
fn launch_order(
    parts: Vec<Vec<Entry>>,
    asked: &str,
    project: &Path,
    code: &mut Code,
) -> Vec<Listed> {
    let now = seconds_since_epoch(SystemTime::now());
    let mut shown: Vec<Listed> = Vec::new();
    for part in parts {
        let part: Vec<Listed> = part
            .into_iter()
            .filter(|entry| entry.kind != Kind::Outcome && !entry.expired(now))
            .filter(|entry| !shown.iter().any(|item| item.entry.id == entry.id))
            .map(|entry| code.listed(entry, project))
            .filter(|item| item.freshness != Freshness::Stale)
            .collect();
        shown.extend(lessons_first(part, asked, |item| item.entry.kind));
    }
    shown
}

/// The paragraph [`for_launch`] tells a session, showing it the first of
/// `shown` that fit, each by its id, to read in full.
fn launch_paragraph(shown: &[Listed], reader: Reader) -> Option<String> {
    let how_to_add = "When you learn something a later session here should know, like a \
                      decision, a gotcha or a command that works, keep it with \
                      `crystal remember \"<what>\"` (add `-k decision|gotcha|command|note`, and \
                      `-f <file>` for each file it's about).";
    let lines = fitted(shown);
    if lines.is_empty() {
        return (reader != Reader::Task).then(|| how_to_add.to_string());
    }
    let mut paragraph = String::from("What this project's earlier sessions learned:");
    for line in lines {
        paragraph.push_str("\n- ");
        paragraph.push_str(&line);
    }
    paragraph.push_str("\n\n");
    paragraph.push_str(match reader {
        Reader::Claude | Reader::Task => {
            "Search the rest of this project's memory with the memory_search tool, and read an \
             entry in full with the memory_show tool, by its id."
        }
        Reader::Agent => {
            "Search the rest of this project's memory with `crystal memory search <words>`, and \
             read an entry in full with `crystal memory show <id>`."
        }
    });
    if reader != Reader::Task {
        paragraph.push(' ');
        paragraph.push_str(how_to_add);
    }
    Some(paragraph)
}

/// The lines of the first of `shown` that fit at launch: at most
/// [`SHOWN_AT_LAUNCH`] of them, in [`LAUNCH_BYTES`]. One too long for the
/// room left is passed over for a shorter one after it.
fn fitted(shown: &[Listed]) -> Vec<String> {
    let mut room = LAUNCH_BYTES;
    let mut lines = Vec::new();
    for item in shown {
        if lines.len() == SHOWN_AT_LAUNCH {
            break;
        }
        let line = format!("{} {}", item.entry.id, launch_line(item));
        // With its "\n- " in front.
        let size = line.len() + 3;
        if size <= room {
            room -= size;
            lines.push(line);
        }
    }
    lines
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

/// The memory of the project called `name` as markdown, for `crystal
/// memory export`: each of `listed` under a heading of its id and kind,
/// marked when it's drifting or stale, then its text, the files it's about
/// and where it came from.
pub fn markdown(name: &str, listed: &[Listed]) -> String {
    let mut text = format!("# {name} memory\n");
    for item in listed {
        let entry = &item.entry;
        let mark = item.freshness.mark().map(|mark| format!(" ({mark})"));
        let mark = mark.unwrap_or_default();
        text.push_str(&format!(
            "\n## {} · {}{mark}\n\n{}\n\n",
            entry.id,
            entry.kind,
            entry.text.trim()
        ));
        if !entry.files.is_empty() {
            let files: Vec<String> = entry.files.iter().map(|file| format!("`{file}`")).collect();
            text.push_str(&format!("About {}. ", files.join(", ")));
        }
        text.push_str(&format!("From {}", entry.source));
        if entry.seen > 1 {
            text.push_str(&format!(", said {} times", entry.seen));
        }
        text.push_str(".\n");
    }
    text
}

/// One entry as a session is shown it: its kind, its title, not too long,
/// the files it's about, and whether it's drifting.
fn launch_line(item: &Listed) -> String {
    let entry = &item.entry;
    let mut text = title(&entry.text);
    if text.chars().count() > LAUNCH_TEXT_LENGTH {
        text = text.chars().take(LAUNCH_TEXT_LENGTH).collect();
        text.push('…');
    }
    let mut line = format!("({}) {text}", entry.kind);
    if !entry.files.is_empty() {
        line.push_str(&format!(" [{}]", entry.files.join(", ")));
    }
    if item.freshness == Freshness::Drifting {
        line.push_str(if entry.names.is_empty() {
            " [drifting: some of its files changed since]"
        } else {
            " [drifting: some of what it names is gone]"
        });
    }
    line
}

pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An entry's title: the first line of its text, which is the whole of an
/// entry said in one line. A list and an agent starting are shown it, and
/// `crystal memory show` the rest.
pub fn title(text: &str) -> String {
    one_line(text.trim().lines().next().unwrap_or_default())
}

/// The text of an entry with a title of its own (`--title`): the title, on
/// one line, then `text` under it. A title is the claim itself, so one too
/// long is refused rather than cut.
pub fn titled(title: &str, text: &str) -> Result<String> {
    let title = one_line(&printable::line(title));
    if title.is_empty() {
        bail!("a title has to say something");
    }
    let length = title.chars().count();
    if length > MAX_TITLE {
        bail!("a title is {MAX_TITLE} characters at most; this one is {length}");
    }
    Ok(match text.trim() {
        "" => title,
        text => format!("{title}\n\n{text}"),
    })
}

/// What says an entry is about progress or status rather than a lesson, by
/// the distiller's rules ([`crate::distill::SYSTEM_PROMPT`]): what was
/// merged, pushed, committed or installed, CI passing, a pull request
/// opened, the backlog item that tracks something, or what holds only in
/// this pull request or session. A commit's hash says so too, which
/// [`has_commit_hash`] finds.
const STATUS: &[&str] = &[
    r"(?i)\bsquash-merged (?:into|as)\b",
    r"(?i)\bmerged (?:in|into|to) (?:master|main)\b",
    r"(?i)\bmerged (?:as|at) [0-9a-f]{7}",
    r"(?i)\bmerged with CI\b",
    r"(?i)\ball merged\b",
    r"(?i)(?:#\d+|\bPRs?) (?:\S+ )?merged\b",
    r"(?i)\bnot (?:yet )?(?:pushed|committed|installed|merged)\b",
    r"(?i)\(uncommitted|\buncommitted\)",
    r"(?i)\bcommitted (?:as )?[0-9a-f]{7}",
    r"(?i)\brun `?make install`? to\b",
    r"(?i)\bCI (?:is )?(?:green|passed)\b|\bgreen CI\b",
    r"(?i)\bopened (?:a |the )?(?:stacked )?(?:draft )?PRs? #?\d",
    r"(?i)\bbacklog #\d+(?:/#?\d+)? (?:tracks|waits)\b",
    r"(?i)\b(?:tracked|saved|archived|deferred|left|noted|backlogged) (?:in|as|to|on) (?:the )?backlog\b",
    r"(?i)\bbacklogged as #\d+",
    r"(?i)\bin this (?:PR|session)\b",
];

/// Whether `text` reads as progress or status rather than a lesson, by
/// [`STATUS`]: for `crystal memory list --status` to list, for the user to
/// look through and forget. Never acted on by itself.
pub fn reads_as_status(text: &str) -> bool {
    static RULES: OnceLock<RegexSet> = OnceLock::new();
    let rules = RULES.get_or_init(|| RegexSet::new(STATUS).expect("the status rules are regexes"));
    rules.is_match(text) || has_commit_hash(text)
}

/// Whether `text` names a commit by its hash: a word of 7 to 12 hex
/// digits, numbers and letters both, and not a piece of a path, a color or
/// a longer name, which stay whole as words here.
fn has_commit_hash(text: &str) -> bool {
    let words = text.split(|c: char| !(c.is_alphanumeric() || "_/.#-".contains(c)));
    words.map(|word| word.trim_end_matches('.')).any(|word| {
        (7..=12).contains(&word.len())
            && word.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
            && word.contains(|c: char| c.is_ascii_digit())
            && word.contains(|c: char| c.is_ascii_alphabetic())
    })
}

/// What of `text` [`names_in`] finds that's in the code of the worktree at
/// `top` now, but for the paths of `files`.
pub fn found_in(top: &Path, text: &str, files: &[String]) -> Vec<String> {
    Code::default().names_in(top, text, files)
}

/// `entry`, with whether it still holds, by what it names and its files as
/// they are now.
pub fn checked(entry: Entry, project: &Path) -> Listed {
    Code::default().listed(entry, project)
}

/// The entries of the memory kept for the daemon at `socket`, with their
/// projects, that have gone stale, all they name gone from the code, or
/// naming nothing to look for, every file they're about, and that the
/// daemon hasn't told of yet: each is told of once, until it holds again,
/// what it names back or it said again.
pub fn newly_stale(socket: &Path) -> Result<Vec<(PathBuf, Entry)>> {
    let mut store = Store::open(socket)?;
    let mut code = Code::default();
    let mut gone_stale = Vec::new();
    for (project, entry, told) in store.anchored()? {
        let stale = code.holds(&entry, &project).0 == Freshness::Stale;
        if stale != told {
            store.set_told_stale(&project, entry.id, stale)?;
        }
        if stale && !told {
            gone_stale.push((project, entry));
        }
    }
    Ok(gone_stale)
}

/// The code entries are checked against: the hash of each file, and the
/// words of each worktree, each read once however many entries look at
/// it.
#[derive(Default)]
struct Code {
    hashes: HashMap<PathBuf, Option<String>>,
    words: HashMap<PathBuf, Words>,
}

impl Code {
    fn hash(&mut self, path: PathBuf) -> Option<&String> {
        self.hashes
            .entry(path)
            .or_insert_with_key(|path| hash_of(path))
            .as_ref()
    }

    /// What of `text` [`names_in`] finds that's in the code of the
    /// worktree at `top` now, but for the paths of `files`: what an entry
    /// saying it about them is anchored to. Its own files say only where it
    /// is, and they're anchored by their hashes.
    fn names_in(&mut self, top: &Path, text: &str, files: &[String]) -> Vec<String> {
        let names = names_beside(text, files);
        if names.is_empty() {
            return names;
        }
        let words = self.words_of(top);
        names.into_iter().filter(|name| words.has(name)).collect()
    }

    fn words_of(&mut self, top: &Path) -> &Words {
        self.words
            .entry(top.to_path_buf())
            .or_insert_with_key(|top| Words::of(top))
    }

    /// Whether `entry` still holds, and what's gone: looked at in the
    /// worktree it was said in, while it's there, or else in `project`.
    /// What it names, while it names anything that was there, each looked
    /// for in the code; or else its anchored files, each changed or gone
    /// counted, a change making it drifting, and every one gone, stale.
    fn holds(&mut self, entry: &Entry, project: &Path) -> (Freshness, Vec<String>) {
        let checkout = entry
            .checkout
            .as_deref()
            .filter(|checkout| checkout.is_dir())
            .unwrap_or(project);
        if !entry.names.is_empty() {
            let words = self.words_of(checkout);
            let gone: Vec<String> = (entry.names.iter())
                .filter(|name| !words.has(name))
                .cloned()
                .collect();
            let freshness = match gone.len() {
                0 => Freshness::Fresh,
                all if all == entry.names.len() => Freshness::Stale,
                _ => Freshness::Drifting,
            };
            return (freshness, gone);
        }
        let mut changed = 0;
        let mut gone = Vec::new();
        for (file, hash) in &entry.anchors {
            let path = checkout.join(file);
            if !path.exists() {
                gone.push(file.clone());
                changed += 1;
            } else if self.hash(path) != Some(hash) {
                changed += 1;
            }
        }
        let freshness = if changed == 0 {
            Freshness::Fresh
        } else if gone.len() == entry.anchors.len() {
            Freshness::Stale
        } else {
            Freshness::Drifting
        };
        (freshness, gone)
    }

    fn listed(&mut self, entry: Entry, project: &Path) -> Listed {
        let (freshness, gone) = self.holds(&entry, project);
        Listed {
            entry,
            freshness,
            gone,
        }
    }
}

/// The words of a worktree's code, to look the names entries give up in:
/// those of every file git lists there, those it tracks and the new ones it
/// doesn't ignore, or outside git, every file under it but the hidden; the
/// files themselves by their paths; and the worktree, for what's in it by
/// a path. Each word is kept by a hash of it, and with how many files have
/// it.
struct Words {
    top: PathBuf,
    words: Arc<HashMap<u64, u32>>,
}

impl Words {
    /// The words of the worktree at `top` as it is now, as much of it as
    /// [`MAX_WORDS_READ`] reads: none when it isn't there. What this
    /// process kept of it from its last look is brought up to date, only
    /// what changed since read again: see [`Kept`].
    fn of(top: &Path) -> Words {
        let files = crate::git::files(top).unwrap_or_else(|_| walked(top));
        let kept = Kept::of(top);
        let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.update(top, &files);
        Words {
            top: top.to_path_buf(),
            words: kept.counts.clone(),
        }
    }

    /// Whether `name`, as [`names_in`] gives it, is in the code: a long
    /// flag by its name with dashes or underscores, as a parser derives it
    /// from a field; a path, or a file's name, as a file or a directory
    /// there, or a path its files' words end with; anything else as one of
    /// their words.
    fn has(&self, name: &str) -> bool {
        if let Some(flag) = name.strip_prefix("--") {
            return self.contains(flag) || self.contains(&flag.replace('-', "_"));
        }
        if name.contains(['/', '.']) {
            let path = name.trim_end_matches('/');
            return self.contains(path) || self.top.join(path).exists();
        }
        self.contains(name)
    }

    fn contains(&self, word: &str) -> bool {
        self.words.contains_key(&word_key(word.as_bytes()))
    }
}

/// What a process keeps of a worktree's words from one look to the next:
/// each file's, as it was read, by its stamp then, and how many of its
/// files have each word. A look reads again only the files whose stamps
/// changed, those that hadn't settled as they were read, and those new
/// since; and lets go of those gone. The daemon, which searches, starts
/// agents and looks for what's gone stale, reads a worktree whole once.
#[derive(Default)]
struct Kept {
    files: HashMap<String, KeptFile>,
    counts: Arc<HashMap<u64, u32>>,
}

/// A file's words as a look read them.
struct KeptFile {
    /// Its stamp as it was looked at, or `None` when it couldn't be.
    stamp: Option<Stamp>,
    /// Whether its stamp had [`SETTLED`] as it was read.
    settled: bool,
    /// Whether what it holds was read, rather than only its path counted:
    /// not when it's too big, or past what a look reads.
    read: bool,
    /// The hash of each of its words, each once.
    words: Box<[u64]>,
}

/// When a file last changed and its size: unchanged, the file is taken to
/// be as it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    changed: SystemTime,
    len: u64,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Option<Stamp> {
        Some(Stamp {
            changed: meta.modified().ok()?,
            len: meta.len(),
        })
    }

    /// Whether, read at `now`, it had settled: it changed at least
    /// [`SETTLED`] before.
    fn settled(self, now: SystemTime) -> bool {
        now.duration_since(self.changed)
            .is_ok_and(|since| since >= SETTLED)
    }
}

impl Kept {
    /// What this process keeps of the worktree at `top`, empty the first
    /// time. Those of worktrees gone, or nobody has looked at for
    /// [`WORDS_KEPT`], are let go.
    fn of(top: &Path) -> Arc<Mutex<Kept>> {
        type Worktrees = HashMap<PathBuf, (Instant, Arc<Mutex<Kept>>)>;
        static KEPT: OnceLock<Mutex<Worktrees>> = OnceLock::new();
        let mut kept = KEPT
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        kept.retain(|dir, (looked, _)| looked.elapsed() < WORDS_KEPT && dir.is_dir());
        let (looked, words) =
            (kept.entry(top.to_path_buf())).or_insert_with(|| (Instant::now(), Arc::default()));
        *looked = Instant::now();
        words.clone()
    }

    /// Brings what's kept up to date with `files`, the worktree's files as
    /// they're listed now, in the order they're read.
    fn update(&mut self, top: &Path, files: &[String]) {
        let counts = Arc::make_mut(&mut self.counts);
        let now = SystemTime::now();
        let mut room = MAX_WORDS_READ;
        let mut listed = HashSet::with_capacity(files.len());
        for file in files {
            listed.insert(file.as_str());
            let path = top.join(file);
            let meta = fs::metadata(&path).ok();
            let stamp = meta.as_ref().and_then(Stamp::of);
            let size = meta
                .as_ref()
                .filter(|meta| meta.is_file())
                .map(fs::Metadata::len);
            let read = size.is_some_and(|size| size <= MAX_ANCHORED_BYTES && size <= room);
            if read {
                room -= size.unwrap_or_default();
            }
            let unchanged = (self.files.get(file))
                .is_some_and(|kept| kept.settled && kept.stamp == stamp && kept.read == read);
            if unchanged {
                continue;
            }
            let kept = KeptFile {
                stamp,
                settled: stamp.is_some_and(|stamp| stamp.settled(now)),
                read,
                words: file_words(file, read.then_some(&path)),
            };
            count(counts, &kept.words, true);
            if let Some(was) = self.files.insert(file.clone(), kept) {
                count(counts, &was.words, false);
            }
        }
        self.files.retain(|file, kept| {
            let there = listed.contains(file.as_str());
            if !there {
                count(counts, &kept.words, false);
            }
            there
        });
    }
}

/// The words of the file at `file`, from the top of its worktree: those of
/// its path, and with `path`, those of what it holds, unless it's binary.
fn file_words(file: &str, path: Option<&PathBuf>) -> Box<[u64]> {
    let mut words = HashSet::new();
    add_path(&mut words, file.as_bytes());
    if let Some(bytes) = path.and_then(|path| fs::read(path).ok())
        // A NUL near the start is git's own test for a binary file.
        && !bytes[..bytes.len().min(8000)].contains(&0)
    {
        add_words(&mut words, &bytes);
    }
    words.into_iter().collect()
}

/// Counts `words` in `counts` as a file that has them comes, `added`, or
/// goes.
fn count(counts: &mut HashMap<u64, u32>, words: &[u64], added: bool) {
    for word in words {
        if added {
            *counts.entry(*word).or_default() += 1;
        } else if let Some(files) = counts.get_mut(word) {
            *files -= 1;
            if *files == 0 {
                counts.remove(word);
            }
        }
    }
}

/// A word as [`Words`] keeps it: the same hash for the same bytes in every
/// look of one process.
fn word_key(word: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(word);
    hasher.finish()
}

/// What `text` names, as [`names_in`] finds it, but for the paths of
/// `files`, the files an entry saying it is about.
pub fn names_beside(text: &str, files: &[String]) -> Vec<String> {
    let mut names = names_in(text);
    names.retain(|name| !is_one_of(name, files));
    names
}

/// Whether the path `name` is one of `files`, or the end of one's path, as
/// `cli.rs` is of `tests/cli.rs`.
fn is_one_of(name: &str, files: &[String]) -> bool {
    let name = name.trim_end_matches('/');
    files.iter().any(|file| {
        let file = file.trim_end_matches('/');
        file == name
            || file
                .strip_suffix(name)
                .is_some_and(|dir| dir.ends_with('/'))
    })
}

/// Every file under `dir`, by its path from it, but those under a hidden
/// directory or one of [`NOT_WALKED`], [`MAX_WALKED`] at most: the files of
/// a project outside git.
fn walked(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(at) = dirs.pop() {
        let Ok(entries) = fs::read_dir(dir.join(&at)) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            if name_text.starts_with('.') {
                continue;
            }
            let path = at.join(&name);
            match entry.file_type() {
                Ok(kind) if kind.is_dir() && !NOT_WALKED.contains(&&*name_text) => dirs.push(path),
                Ok(kind) if kind.is_file() => files.push(path.to_string_lossy().into_owned()),
                _ => {}
            }
            if files.len() >= MAX_WALKED {
                return files;
            }
        }
    }
    files
}

/// Adds the words of `bytes` to `words`: each identifier, each word joined
/// by dashes (`kill-server`, `--test-threads` as `test-threads`), and each
/// path (`src/memory.rs`, `memory.db`, `memory.stale`) with every path it
/// ends with.
fn add_words(words: &mut HashSet<u64>, bytes: &[u8]) {
    let in_path = |byte: &u8| byte.is_ascii_alphanumeric() || b"_-./~".contains(byte);
    for run in bytes.split(|byte| !in_path(byte)) {
        if run.is_empty() {
            continue;
        }
        for identifier in run.split(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_')) {
            add_word(words, identifier);
        }
        for dashed in run.split(|byte| b"./~".contains(byte)) {
            let dashed = dashed.trim_ascii_start().trim_ascii_end();
            let dashed = trim_bytes(dashed, b'-');
            if dashed.contains(&b'-') {
                add_word(words, dashed);
            }
        }
        add_path(words, run);
    }
}

/// Adds the path `path` to `words`, without the dots and slashes it ends
/// with, with every path it ends with after a slash, and every directory
/// it's in.
fn add_path(words: &mut HashSet<u64>, path: &[u8]) {
    let mut path = path;
    while let [rest @ .., b'.' | b'/'] = path {
        path = rest;
    }
    if !path.contains(&b'/') && !path.contains(&b'.') {
        return;
    }
    add_word(words, path);
    for (at, byte) in path.iter().enumerate() {
        if *byte == b'/' {
            add_word(words, &path[at + 1..]);
            add_word(words, &path[..at]);
        }
    }
}

fn add_word(words: &mut HashSet<u64>, word: &[u8]) {
    if !word.is_empty() {
        words.insert(word_key(word));
    }
}

/// `bytes` without `byte` at either end.
fn trim_bytes(mut bytes: &[u8], byte: u8) -> &[u8] {
    while let [first, rest @ ..] = bytes
        && *first == byte
    {
        bytes = rest;
    }
    while let [rest @ .., last] = bytes
        && *last == byte
    {
        bytes = rest;
    }
    bytes
}

/// What `text` names that looks like code, which can be looked for in it:
/// what's in backticks, and outside them, identifiers with an underscore or
/// a hump (`local_origin`, `TaskRecord`), the last part of a path in code
/// (`Crystal::listening` as `listening`), what's called (`listening()`),
/// long flags (`--test-threads`), files and directories (`src/memory.rs`,
/// `memory.db`, `agents/`) and words joined by dots (`memory.stale`). A
/// command in backticks gives those of its words, and those joined by
/// dashes (`kill-server`). Words split by a slash are each looked at alone,
/// unless they're a path. Each once, in the order they come,
/// [`MAX_NAMES`] at most. Nothing outside the project, like `~/.config` or
/// `/tmp`, and nothing that names no particular thing, like `true` or
/// `macOS`.
pub fn names_in(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let pieces: Vec<&str> = text.split('`').collect();
    for (at, piece) in pieces.iter().enumerate() {
        // Between two backticks on one line, it's quoted.
        let quoted = at % 2 == 1 && at + 1 < pieces.len() && !piece.contains('\n');
        let one_word = !piece.trim().contains(char::is_whitespace);
        for word in piece.split_whitespace() {
            for part in parts_of(word) {
                let name = if quoted && one_word {
                    name_of(part, true)
                } else if quoted {
                    name_of(part, false).or_else(|| name_of(part, part.contains('-')))
                } else {
                    name_of(part, false)
                };
                if let Some(name) = name
                    && !names.contains(&name)
                {
                    names.push(name);
                }
            }
        }
    }
    names.truncate(MAX_NAMES);
    names
}

/// The parts of `word` that may each be a name: the word without the
/// punctuation around it, whole when it's a path, or else split at each
/// slash, each part without what a call takes (`listening()` as
/// `listening`).
fn parts_of(word: &str) -> Vec<&str> {
    let word = bare(word);
    // Outside the project: from the root, a home, or another machine.
    if word.starts_with(['/', '~']) || word.contains("://") {
        return Vec::new();
    }
    if is_path(word) {
        return vec![word];
    }
    word.split('/')
        .map(|part| {
            let part = bare(part);
            let called = part.find('(').filter(|&at| {
                at > 0
                    && part[..at]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_:.".contains(c))
            });
            bare(called.map_or(part, |at| &part[..at]))
        })
        .filter(|part| !part.is_empty())
        .collect()
}

/// `word` without the punctuation around it.
fn bare(word: &str) -> &str {
    word.trim_start_matches(|c| "([{\"'<*$".contains(c))
        .trim_end_matches(|c| ")]}\"'>,.;:!?*".contains(c))
}

/// The name `word` gives, if it looks like code: always so in a backtick of
/// its own, `quoted`, any word of three letters or more, or words joined by
/// dashes.
fn name_of(word: &str, quoted: bool) -> Option<String> {
    if !word.bytes().any(|byte| byte.is_ascii_alphabetic()) {
        return None;
    }
    if let Some(flag) = word.strip_prefix("--") {
        let flag = flag.split('=').next().unwrap_or_default();
        let shaped = flag.len() > 1
            && !flag.starts_with('-')
            && !flag.ends_with('-')
            && flag
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        return shaped.then(|| format!("--{flag}"));
    }
    if word.starts_with(['/', '~', '-']) {
        return None;
    }
    if is_path(word) || is_file_name(word) {
        return Some(word.to_string());
    }
    if let Some((_, last)) = word.rsplit_once("::") {
        // A type, a variant or a constant by its own name; a function or a
        // field only when its name looks like code.
        let typed = last.starts_with(|c: char| c.is_ascii_uppercase());
        return (last.len() > 1 && is_identifier(last))
            .then(|| name_of(last, quoted || typed))
            .flatten();
    }
    if is_identifier(word) {
        let underscored = word.trim_matches('_').contains('_');
        let humped = word
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase());
        let named = underscored || humped || (quoted && word.len() >= 3);
        return (named && !NOT_NAMES.contains(&word)).then(|| word.to_string());
    }
    let dashed = word.split('-').count() > 1
        && word.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    if quoted && dashed {
        return Some(word.to_string());
    }
    let dotted = word.split('.').count() > 1
        && word.split('.').all(|part| {
            part.len() > 1
                && is_identifier(part)
                && part.bytes().any(|byte| byte.is_ascii_lowercase())
        });
    dotted.then(|| word.to_string())
}

fn is_identifier(word: &str) -> bool {
    let mut chars = word.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `word` is a path in the project: parts joined by slashes, a
/// directory's ending with one (`agents/`), a file's with its name
/// (`src/memory.rs`); not words a slash sets side by side, like `add/list`
/// or `CLAUDE.md/AGENTS.md`.
fn is_path(word: &str) -> bool {
    let shaped = word.contains('/')
        && !word.contains("://")
        && word.bytes().any(|byte| byte.is_ascii_alphabetic())
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-./~".contains(&byte));
    if !shaped {
        return false;
    }
    let mut parts: Vec<&str> = word.trim_end_matches('/').split('/').collect();
    let last = if word.ends_with('/') {
        None
    } else {
        parts.pop()
    };
    let directories = parts
        .iter()
        .all(|part| !part.is_empty() && *part != "." && *part != ".." && !part[1..].contains('.'));
    directories && last.is_none_or(is_file_name) && word.len() > 1
}

/// Whether `word` is a file's name: a name, then an extension a file of
/// code or data has, like `memory.db` or `.gitignore.md`.
fn is_file_name(word: &str) -> bool {
    let Some((stem, extension)) = word.rsplit_once('.') else {
        return false;
    };
    let stem = stem.strip_prefix('.').unwrap_or(stem);
    EXTENSIONS.contains(&extension)
        && stem.bytes().any(|byte| byte.is_ascii_alphabetic())
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte))
}

/// An anchor for each of `files` that's a file in `checkout` now: its
/// hash, to tell later whether it has changed. A file that isn't there
/// says nothing either way, so it isn't anchored.
fn anchors_in(checkout: &Path, files: &[String]) -> BTreeMap<String, String> {
    files
        .iter()
        .filter_map(|file| Some((file.clone(), hash_of(&checkout.join(file))?)))
        .collect()
}

/// Anchors for an entry from before anchors, last said at `said`, from
/// its `files` in `project` as they are now. A file that hasn't changed
/// since, by its time, is anchored to what it holds; one changed since, or
/// gone, to nothing, which no file matches, so an entry stale before is
/// stale still.
fn anchors_from_before(files: &[String], said: u64, project: &Path) -> BTreeMap<String, String> {
    let said = UNIX_EPOCH + Duration::from_secs(said);
    files
        .iter()
        .map(|file| {
            let path = project.join(file);
            let hash = match fs::metadata(&path).and_then(|meta| meta.modified()) {
                Ok(changed) if changed <= said => hash_of(&path).unwrap_or_default(),
                _ => String::new(),
            };
            (file.clone(), hash)
        })
        .collect()
}

/// The SHA-256 of what the file at `path` holds, in hex, when it's a file
/// and not too big to hash.
fn hash_of(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ANCHORED_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    Some(
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// Where the memory of the daemon at `socket` is kept: in crystal's state
/// directory, beside the list of sessions, so that a daemon of a test's own
/// keeps its own.
pub fn dir(socket: &Path) -> PathBuf {
    state::path(socket).with_file_name("memory")
}

/// A project's file of entries from before the database: named after its
/// path, the way Claude Code names its projects, `/code/app` as
/// `-code-app.json`.
fn old_file_for(dir: &Path, project: &Path) -> PathBuf {
    let name = project.to_string_lossy().replace('/', "-");
    dir.join(format!("{name}.json"))
}

/// A project's file of entries from before the database, as it was kept.
#[derive(Debug, Default, Deserialize)]
struct OldStore {
    next_id: u64,
    entries: Vec<Entry>,
}

/// What `file` kept, when it's there.
fn load_old(file: &Path) -> Result<Option<OldStore>> {
    match fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("couldn't read {}", file.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("couldn't read {}", file.display())),
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
            seen: 1,
            last_seen: 1_000,
            anchors: BTreeMap::new(),
            checkout: None,
            used: None,
            names: Vec::new(),
            counted_from: None,
        }
    }

    fn note(text: &str) -> New {
        New {
            kind: Kind::Note,
            text: text.into(),
            files: Vec::new(),
            source: Source::User,
            checkout: None,
        }
    }

    /// A socket in a directory of the test's own, so its memory is too.
    fn socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        (dir, socket)
    }

    fn ids(entries: &[Entry]) -> Vec<u64> {
        entries.iter().map(|entry| entry.id).collect()
    }

    const APP: &str = "/code/app";

    #[test]
    fn the_bundled_sqlite_has_fts5() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIRTUAL TABLE probe USING fts5(a, tokenize = 'porter');")
            .unwrap();
    }

    #[test]
    fn what_is_added_reads_back_with_ids_counting_up() {
        let (_dir, socket) = socket();
        let project = Path::new(APP);
        let gotcha = New {
            kind: Kind::Gotcha,
            ..note("the tests need the db")
        };
        add(&socket, project, gotcha).unwrap();
        add(&socket, project, note("fees are in cents")).unwrap();

        let memory = Memory::read(&socket, project).unwrap();
        let ids: Vec<u64> = memory.listed().iter().map(|item| item.entry.id).collect();
        assert_eq!(ids, [2, 1], "newest first");
        assert_eq!(memory.get(1).unwrap().text, "the tests need the db");
        assert_eq!(memory.get(1).unwrap().kind, Kind::Gotcha);
    }

    #[test]
    fn a_removed_entry_s_id_is_never_used_again() {
        let (_dir, socket) = socket();
        let project = Path::new(APP);
        add(&socket, project, note("one")).unwrap();
        remove(&socket, project, 1).unwrap();
        let next = add(&socket, project, note("two")).unwrap();
        assert_eq!(next.entry().unwrap().id, 2);
        assert!(remove(&socket, project, 1).is_err());
    }

    #[test]
    fn each_project_has_a_memory_of_its_own() {
        let (_dir, socket) = socket();
        add(&socket, Path::new(APP), note("app's")).unwrap();
        let other = Memory::read(&socket, Path::new("/code/other")).unwrap();
        assert!(other.listed().is_empty());
        let mut store = Store::open(&socket).unwrap();
        let found = store.search(Path::new("/code/other"), "app", None, 10, None);
        assert!(found.unwrap().is_empty());
    }

    #[test]
    fn nothing_is_too_little_to_remember() {
        let (_dir, socket) = socket();
        assert!(add(&socket, Path::new(APP), note("  ")).is_err());
    }

    #[test]
    fn the_same_said_again_is_the_one_entry_seen_again() {
        let (_dir, socket) = socket();
        let project = Path::new(APP);
        add(&socket, project, note("Fees are kept in cents.")).unwrap();
        let again = New {
            files: vec!["fees.rs".into()],
            ..note("fees are kept in CENTS")
        };
        let Added::Again(entry) = add(&socket, project, again).unwrap() else {
            panic!("a second entry");
        };
        assert_eq!(entry.id, 1);
        assert_eq!(entry.seen, 2);
        assert_eq!(
            entry.text, "Fees are kept in cents.",
            "the first wording stays"
        );
        assert_eq!(entry.files, ["fees.rs"]);
        assert_eq!(Memory::read(&socket, project).unwrap().listed().len(), 1);
    }

    #[test]
    fn what_the_user_forgot_crystal_can_t_add_back_but_they_can() {
        let (_dir, socket) = socket();
        let project = Path::new(APP);
        add(&socket, project, note("the ledger needs redis")).unwrap();
        remove(&socket, project, 1).unwrap();

        let distilled = New {
            source: Source::Distilled("fixer".into()),
            ..note("The ledger needs Redis")
        };
        assert_eq!(
            add(&socket, project, distilled.clone()).unwrap(),
            Added::Refused
        );
        let Added::New(entry) = add(&socket, project, note("the ledger needs redis")).unwrap()
        else {
            panic!("not added back");
        };
        assert_eq!(entry.id, 2);
        // Remembered again, it's the distiller's to see again too.
        assert!(matches!(
            add(&socket, project, distilled).unwrap(),
            Added::Again(_)
        ));
    }

    #[test]
    fn what_was_forgotten_is_listed_as_it_was_until_it_s_remembered_again() {
        let (_dir, socket) = socket();
        let project = Path::new(APP);
        let mut store = Store::open(&socket).unwrap();
        let about = New {
            kind: Kind::Gotcha,
            files: vec!["ledger.rs".into()],
            ..note("the ledger needs redis")
        };
        store.add(project, about).unwrap();
        store.add(project, note("fees are kept in cents")).unwrap();
        store.remove(project, 1).unwrap();
        store.remove(project, 2).unwrap();
        let forgotten = store.forgotten(project).unwrap();
        let said: Vec<(u64, Kind, &str)> = forgotten
            .iter()
            .map(|entry| (entry.id, entry.kind, &entry.text[..]))
            .collect();
        assert_eq!(
            said,
            [
                (2, Kind::Note, "fees are kept in cents"),
                (1, Kind::Gotcha, "the ledger needs redis"),
            ]
        );
        assert_eq!(forgotten[1].files, ["ledger.rs"]);
        assert!(forgotten[0].forgotten > 0);
        assert!(
            store
                .forgotten(Path::new("/code/other"))
                .unwrap()
                .is_empty()
        );

        store.add(project, note("The ledger needs Redis!")).unwrap();
        let forgotten = store.forgotten(project).unwrap();
        assert_eq!(forgotten.len(), 1, "{forgotten:?}");
    }

    #[test]
    fn a_title_is_an_entry_s_first_line_and_one_of_its_own_goes_over_it() {
        assert_eq!(
            title("  Fees are in cents\nnever floats "),
            "Fees are in cents"
        );
        assert_eq!(title("one line,  spaced"), "one line, spaced");
        assert_eq!(
            titled("Fees are in cents", "Never store a float.").unwrap(),
            "Fees are in cents\n\nNever store a float."
        );
        assert_eq!(titled(" two\nlines ", "").unwrap(), "two lines");
        assert!(titled("  ", "x").is_err());
        let long = "word ".repeat(30);
        let refused = titled(&long, "").unwrap_err().to_string();
        assert!(refused.contains("120 characters at most"), "{refused}");
    }

    #[test]
    fn a_search_keeps_to_the_files_it_names_and_the_directories_they_re_in() {
        let (_dir, socket) = socket();
        let project = Path::new(APP);
        let mut store = Store::open(&socket).unwrap();
        for files in [
            &["src/ledger.rs"][..],
            &["src/ledger/mod.rs"],
            &["src/ledgers.rs"],
            &[],
        ] {
            let new = New {
                files: files.iter().map(|file| file.to_string()).collect(),
                ..note(&format!("ledger note about {files:?}"))
            };
            store.add(project, new).unwrap();
        }
        let mut about = |files: &[&str]| {
            let files: Vec<String> = files.iter().map(|file| file.to_string()).collect();
            let found = store
                .search_about(project, "ledger", None, &files, 10, None)
                .unwrap();
            let mut found = ids(&found);
            found.sort();
            found
        };
        assert_eq!(about(&["src/ledger.rs"]), [1]);
        assert_eq!(
            about(&["src/ledger"]),
            [2],
            "a directory, not a name it starts"
        );
        assert_eq!(about(&["src"]), [1, 2, 3]);
        assert_eq!(about(&["src/ledger.rs", "src/ledgers.rs"]), [1, 3]);
        assert_eq!(about(&[]), [1, 2, 3, 4]);
        let newest = store
            .search_about(project, "", None, &["src/ledger".into()], 10, None)
            .unwrap();
        assert_eq!(ids(&newest), [2]);
    }

    #[test]
    fn a_search_gives_the_stale_after_the_rest_or_leaves_them_out() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs", "b.rs"]);
        fs::write(project.path().join("a.rs"), "fn refund_ledger() {}").unwrap();
        let mut store = Store::open(&socket).unwrap();
        let named = New {
            text: "the refund ledger is in `refund_ledger`".into(),
            ..about(&["a.rs"], project.path())
        };
        store.add(project.path(), named).unwrap();
        store
            .add(project.path(), note("refund waits for the ledger, always"))
            .unwrap();
        store
            .add(project.path(), note("the refund ledger is slow"))
            .unwrap();
        fs::write(project.path().join("a.rs"), "changed").unwrap();

        let wanted = |fresh, limit| Wanted {
            fresh,
            ..Wanted::best(limit)
        };
        let mut found = |wanted: Wanted| {
            let found = store
                .find(project.path(), "refund ledger", &wanted, None)
                .unwrap();
            let said: Vec<(u64, Freshness)> = found
                .iter()
                .map(|item| (item.entry.id, item.freshness))
                .collect();
            said
        };
        // First by its words, it comes after those that hold.
        let all = found(wanted(false, 10));
        assert_eq!(all.len(), 3);
        assert_eq!(all[2], (1, Freshness::Stale), "{all:?}");
        // Among the few best, those that hold come first.
        let best = found(wanted(false, 1));
        assert_eq!(best.len(), 1);
        assert_eq!(best[0].1, Freshness::Fresh, "{best:?}");
        let mut fresh = found(wanted(true, 10));
        fresh.sort();
        assert_eq!(fresh, [(2, Freshness::Fresh), (3, Freshness::Fresh)]);
        assert_eq!(found(wanted(true, 1)).len(), 1);
        let gotchas = Wanted {
            kind: Some(Kind::Gotcha),
            ..wanted(false, 10)
        };
        assert!(found(gotchas).is_empty());
    }

    #[test]
    fn credentials_and_escapes_never_reach_the_memory() {
        let (_dir, socket) = socket();
        let added = add(
            &socket,
            Path::new(APP),
            note("deploy with DEPLOY_TOKEN=abc123def456ghi789 \x1b[31mfast\x1b[0m"),
        )
        .unwrap();
        assert_eq!(
            added.entry().unwrap().text,
            "deploy with DEPLOY_TOKEN=[redacted] [31mfast[0m"
        );
    }

    #[test]
    fn a_project_s_file_from_before_the_database_is_brought_in_once() {
        let (_dir, socket) = socket();
        let project = Path::new(APP);
        let old = dir(&socket).join("-code-app.json");
        fs::create_dir_all(dir(&socket)).unwrap();
        let text = serde_json::json!({
            "next_id": 4,
            "entries": [
                {"id": 2, "kind": "gotcha", "text": "the ledger needs redis",
                 "source": {"session": "fixer"}, "created": 1000},
                {"id": 3, "kind": "note", "text": "fees are in cents", "files": ["fees.rs"],
                 "source": "user", "created": 2000},
            ],
        });
        fs::write(&old, text.to_string()).unwrap();

        let memory = Memory::read(&socket, project).unwrap();
        let listed = memory.listed();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            memory.get(2).unwrap().source,
            Source::Session("fixer".into())
        );
        assert_eq!(memory.get(2).unwrap().last_seen, 1000);
        assert_eq!(memory.get(3).unwrap().files, ["fees.rs"]);
        assert!(!old.exists() && old.with_extension("json.imported").exists());
        // Ids go on from where the file left off.
        let next = add(&socket, project, note("deploys on tuesdays")).unwrap();
        assert_eq!(next.entry().unwrap().id, 4);
        // And it's searchable like any other.
        let mut store = Store::open(&socket).unwrap();
        let found = store.search(project, "redis", None, 10, None).unwrap();
        assert_eq!(found[0].id, 2);
    }

    /// A project of the test's own, with `files` in it, each holding its
    /// own name.
    fn project_with(files: &[&str]) -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        for file in files {
            fs::write(project.path().join(file), file).unwrap();
        }
        project
    }

    /// `files` of the project as a note is about them, as said in `checkout`.
    fn about(files: &[&str], checkout: &Path) -> New {
        New {
            files: files.iter().map(|file| file.to_string()).collect(),
            checkout: Some(checkout.to_path_buf()),
            ..note("refund waits for the ledger")
        }
    }

    /// How entry `id` of the memory of `at` holds now.
    fn holds(socket: &Path, at: &Path, id: u64) -> Freshness {
        let entry = Memory::read(socket, at).unwrap().get(id).unwrap().clone();
        checked(entry, at).freshness
    }

    #[test]
    fn an_entry_holds_while_what_it_names_is_there_however_its_files_change() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs", "b.rs"]);
        let at = project.path();
        fs::write(
            at.join("a.rs"),
            "fn local_origin() {}\nconst LEDGER_DB: &str = \"\";",
        )
        .unwrap();
        let named = New {
            text: "Tests use `local_origin()`, never github.com; LEDGER_DB is set".into(),
            ..about(&["a.rs", "b.rs"], at)
        };
        let added = add(&socket, at, named).unwrap();
        assert_eq!(added.entry().unwrap().names, ["local_origin", "LEDGER_DB"]);
        assert_eq!(holds(&socket, at, 1), Freshness::Fresh);

        fs::write(at.join("b.rs"), "changed").unwrap();
        fs::write(
            at.join("a.rs"),
            "// moved\nfn local_origin() {} const LEDGER_DB: u8 = 1;",
        )
        .unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Fresh, "its files changed");
        // Moved to another file, it's there still.
        fs::write(at.join("a.rs"), "const LEDGER_DB: u8 = 1;").unwrap();
        fs::write(at.join("c.rs"), "pub fn local_origin() {}").unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Fresh);

        fs::remove_file(at.join("c.rs")).unwrap();
        let entry = Memory::read(&socket, at).unwrap().get(1).unwrap().clone();
        let item = checked(entry, at);
        assert_eq!(item.freshness, Freshness::Drifting);
        assert_eq!(item.gone, ["local_origin"]);
        assert_eq!(
            item.how_it_holds().unwrap(),
            "drifting: some of what it names is gone from the code: local_origin"
        );
        fs::write(at.join("a.rs"), "const LEDGER: u8 = 1;").unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Stale);
        // Back, it holds again.
        fs::write(
            at.join("a.rs"),
            "fn local_origin() {}\nconst LEDGER_DB: u8 = 1;",
        )
        .unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Fresh);
    }

    #[test]
    fn what_wasn_t_in_the_code_as_it_was_said_isn_t_looked_for() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs"]);
        let at = project.path();
        fs::write(at.join("a.rs"), "fn uncapped_tabs() {}").unwrap();
        let removed = New {
            text: "Tabs are uncapped now: there's no MAX_TABS; see uncapped_tabs".into(),
            ..about(&["a.rs"], at)
        };
        let added = add(&socket, at, removed).unwrap();
        assert_eq!(added.entry().unwrap().names, ["uncapped_tabs"]);
        // Naming nothing that was there, it goes by its files.
        let nothing = New {
            text: "There's no MAX_TABS any more".into(),
            ..about(&["a.rs"], at)
        };
        assert!(
            add(&socket, at, nothing)
                .unwrap()
                .entry()
                .unwrap()
                .names
                .is_empty()
        );
    }

    #[test]
    fn an_entry_naming_nothing_drifts_as_its_files_change_and_goes_stale_once_all_are_gone() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs", "b.rs"]);
        let at = project.path();
        add(&socket, at, about(&["a.rs", "b.rs"], at)).unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Fresh);

        fs::write(at.join("a.rs"), "changed").unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Drifting);
        fs::write(at.join("b.rs"), "changed").unwrap();
        assert_eq!(
            holds(&socket, at, 1),
            Freshness::Drifting,
            "changed, not gone"
        );
        fs::remove_file(at.join("b.rs")).unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Drifting);
        fs::remove_file(at.join("a.rs")).unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Stale, "every one gone");
        let entry = Memory::read(&socket, at).unwrap().get(1).unwrap().clone();
        assert_eq!(
            checked(entry, at).how_it_holds().unwrap(),
            "stale: every file it's about is gone"
        );

        // Said again, it holds for its files as they are now.
        fs::write(at.join("a.rs"), "back").unwrap();
        add(&socket, at, about(&["a.rs"], at)).unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Fresh);
        // A file changed back is the file it was.
        fs::write(at.join("a.rs"), "changed again").unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Drifting);
        fs::write(at.join("a.rs"), "back").unwrap();
        assert_eq!(holds(&socket, at, 1), Freshness::Fresh);
    }

    #[test]
    fn an_entry_is_told_of_once_as_it_goes_stale_until_it_holds_again() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs", "b.rs"]);
        let at = project.path();
        fs::write(at.join("a.rs"), "fn nightly_ledger() {}").unwrap();
        let named = New {
            text: "the ledger runs nightly: `nightly_ledger`".into(),
            ..about(&["a.rs"], at)
        };
        add(&socket, at, named).unwrap();
        add(&socket, at, note("about no file")).unwrap();
        add(&socket, at, about(&["b.rs"], at)).unwrap();
        let swept = || -> Vec<u64> {
            let stale = newly_stale(&socket).unwrap();
            assert!(stale.iter().all(|(project, _)| project == at));
            stale.into_iter().map(|(_, entry)| entry.id).collect()
        };
        assert_eq!(swept(), Vec::<u64>::new());
        fs::write(at.join("b.rs"), "changed").unwrap();
        assert_eq!(swept(), Vec::<u64>::new(), "a change only drifts");
        fs::write(at.join("a.rs"), "fn daily_ledger() {}").unwrap();
        assert_eq!(swept(), [1]);
        assert_eq!(swept(), Vec::<u64>::new(), "once is enough");

        // Holding again, what it names back, it can go stale again.
        fs::write(at.join("a.rs"), "fn nightly_ledger() {}").unwrap();
        assert_eq!(swept(), Vec::<u64>::new());
        fs::remove_file(at.join("a.rs")).unwrap();
        assert_eq!(swept(), [1]);
        fs::remove_file(at.join("b.rs")).unwrap();
        assert_eq!(swept(), [3]);

        // Said again, it holds for what it names as it is now.
        fs::write(at.join("b.rs"), "pub fn nightly_ledger() {}").unwrap();
        let again = New {
            text: "the ledger runs nightly: `nightly_ledger`".into(),
            ..about(&["b.rs"], at)
        };
        add(&socket, at, again).unwrap();
        fs::write(at.join("b.rs"), "changed again").unwrap();
        assert_eq!(swept(), [1]);
    }

    #[test]
    fn a_file_that_isn_t_there_isn_t_anchored() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs"]);
        let at = project.path();
        let added = add(&socket, at, about(&["a.rs", "gone.rs"], at)).unwrap();
        let entry = added.entry().unwrap();
        assert_eq!(entry.anchors.keys().collect::<Vec<_>>(), ["a.rs"]);
        assert_eq!(checked(entry.clone(), at).freshness, Freshness::Fresh);

        let nothing = add(
            &socket,
            at,
            New {
                text: "no file of it is there".into(),
                ..about(&["gone.rs"], at)
            },
        );
        assert_eq!(
            checked(nothing.unwrap().entry().unwrap().clone(), at).freshness,
            Freshness::Fresh
        );
    }

    #[test]
    fn code_is_looked_at_in_the_worktree_it_was_said_in_while_it_s_there() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs"]);
        let worktree = tempfile::tempdir().unwrap();
        fs::write(worktree.path().join("a.rs"), "fn worktree_only() {}").unwrap();
        let named = New {
            text: "worktree_only is the worktree's own".into(),
            ..about(&["a.rs"], worktree.path())
        };
        let added = add(&socket, project.path(), named).unwrap();
        let entry = added.entry().unwrap().clone();
        assert_eq!(entry.names, ["worktree_only"]);
        let holds = |entry: &Entry| checked(entry.clone(), project.path()).freshness;
        assert_eq!(holds(&entry), Freshness::Fresh);
        let files = added.entry().unwrap().clone();
        let files = Entry {
            names: Vec::new(),
            ..files
        };
        assert_eq!(holds(&files), Freshness::Fresh);

        // Once the worktree is gone, the project's code is the one.
        drop(worktree);
        assert_eq!(holds(&entry), Freshness::Stale);
        assert_eq!(holds(&files), Freshness::Drifting);
    }

    fn remembering(texts: &[(Kind, &str)]) -> (tempfile::TempDir, PathBuf, Store) {
        let (dir, socket) = socket();
        let mut store = Store::open(&socket).unwrap();
        for (kind, text) in texts {
            let new = New {
                kind: *kind,
                ..note(text)
            };
            store.add(Path::new(APP), new).unwrap();
        }
        (dir, socket, store)
    }

    #[test]
    fn search_ranks_by_how_many_words_and_how_rare() {
        let (_dir, _socket, mut store) = remembering(&[
            (Kind::Note, "the ledger is slow"),
            (Kind::Gotcha, "the ledger test needs the database"),
            (Kind::Note, "fees are rounded down"),
            (Kind::Note, "ledger tests flake under load"),
        ]);
        let found = store
            .search(Path::new(APP), "flaky ledger test", None, 10, None)
            .unwrap();
        // Both words beat one; an entry with neither isn't found.
        let mut both = ids(&found)[..2].to_vec();
        both.sort_unstable();
        assert_eq!(both, [2, 4]);
        assert_eq!(ids(&found)[2..], [1]);
    }

    #[test]
    fn search_matches_the_start_of_a_word_and_its_stem() {
        let (_dir, _socket, mut store) = remembering(&[
            (Kind::Note, "Running the migrations takes a minute"),
            (Kind::Note, "the docs build with mdbook"),
        ]);
        let project = Path::new(APP);
        assert_eq!(
            ids(&store.search(project, "migrat", None, 10, None).unwrap()),
            [1]
        );
        assert_eq!(
            ids(&store.search(project, "run", None, 10, None).unwrap()),
            [1]
        );
        assert_eq!(
            ids(&store.search(project, "builds", None, 10, None).unwrap()),
            [2]
        );
    }

    #[test]
    fn search_finds_an_entry_by_its_files_and_keeps_to_a_kind() {
        let (_dir, _socket, mut store) = remembering(&[(Kind::Decision, "fees are in cents")]);
        let project = Path::new(APP);
        let about = New {
            files: vec!["src/ledger.rs".into()],
            ..note("rounding happens once, at the end")
        };
        store.add(project, about).unwrap();
        assert_eq!(
            ids(&store.search(project, "ledger", None, 10, None).unwrap()),
            [2]
        );
        let decisions = store
            .search(project, "", Some(Kind::Decision), 10, None)
            .unwrap();
        assert_eq!(ids(&decisions), [1]);
    }

    #[test]
    fn what_a_person_types_is_never_taken_for_fts_syntax() {
        let (_dir, _socket, mut store) = remembering(&[(Kind::Note, "use NEAR not AND")]);
        let project = Path::new(APP);
        for query in [
            "\"",
            "AND OR NOT",
            "a* (b",
            "near(x y)",
            "col:umn",
            "^start",
        ] {
            store.search(project, query, None, 10, None).unwrap();
        }
        // Only common words: they're all there is to go on.
        assert_eq!(fts_query("the AND"), Some("\"the\"* OR \"and\"*".into()));
        assert_eq!(fts_query("!!!"), None);
    }

    /// A stand-in for the model: each word stands for a meaning, and words
    /// that mean the same stand for the same one, so "db" is "postgres".
    struct Meanings {
        model: &'static str,
    }

    const SAME: [&[&str]; 3] = [
        &["db", "database", "postgres"],
        &["deploy", "deploys", "release", "ship"],
        &["money", "fees", "cents"],
    ];

    impl Meanings {
        fn vectors(&self, texts: &[&str]) -> Vec<Vec<f32>> {
            texts
                .iter()
                .map(|text| {
                    let mut vector = vec![0.0_f32; 16];
                    for word in text.split(|c: char| !c.is_alphanumeric()) {
                        let word = word.to_lowercase();
                        if word.is_empty() || STOP_WORDS.contains(&word.as_str()) {
                            continue;
                        }
                        let dim = match SAME.iter().position(|same| same.contains(&&*word)) {
                            Some(meaning) => meaning,
                            None => 3 + word.bytes().map(usize::from).sum::<usize>() % 13,
                        };
                        vector[dim] += 1.0;
                    }
                    let length = dot(&vector, &vector).sqrt().max(f32::EPSILON);
                    vector.iter().map(|x| x / length).collect()
                })
                .collect()
        }
    }

    impl Embed for Meanings {
        fn model(&self) -> &str {
            self.model
        }

        fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(self.vectors(texts))
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(self.vectors(&[text]).remove(0))
        }

        fn min_similarity(&self) -> f32 {
            0.3
        }

        fn near_best(&self) -> f32 {
            1.0
        }
    }

    const MEANINGS: Meanings = Meanings { model: "meanings" };

    /// A model that always fails.
    struct Broken;

    impl Embed for Broken {
        fn model(&self) -> &str {
            "broken"
        }

        fn embed_passages(&self, _: &[&str]) -> Result<Vec<Vec<f32>>> {
            bail!("out of memory")
        }

        fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
            bail!("out of memory")
        }

        fn min_similarity(&self) -> f32 {
            0.0
        }

        fn near_best(&self) -> f32 {
            1.0
        }
    }

    fn vectors(store: &Store) -> i64 {
        store
            .conn
            .query_row("SELECT count(*) FROM vectors", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn with_a_model_an_entry_is_found_by_what_it_means() {
        let (_dir, _socket, mut store) = remembering(&[
            (Kind::Gotcha, "Postgres has to be up for the ledger tests"),
            (Kind::Note, "Deploys go out on Tuesdays"),
        ]);
        let project = Path::new(APP);
        assert!(
            store
                .search(project, "db", None, 10, None)
                .unwrap()
                .is_empty()
        );
        let found = store
            .search(project, "db", None, 10, Some(&MEANINGS))
            .unwrap();
        assert_eq!(ids(&found), [1]);
        let found = store.search(project, "when do we ship", None, 10, Some(&MEANINGS));
        assert_eq!(ids(&found.unwrap()), [2]);
    }

    #[test]
    fn an_entry_found_by_its_words_and_its_meaning_comes_first() {
        let (_dir, _socket, mut store) = remembering(&[
            (Kind::Note, "the database migrations are slow"),
            (Kind::Note, "postgres needs a restart each night"),
            (Kind::Note, "the nightly build restarts"),
        ]);
        let found = store
            .search(Path::new(APP), "postgres db", None, 10, Some(&MEANINGS))
            .unwrap();
        // 2 by both, 1 by its meaning alone; 3 by neither.
        assert_eq!(ids(&found), [2, 1]);
    }

    #[test]
    fn a_match_by_meaning_well_behind_the_best_doesn_t_count() {
        let (_dir, _socket, mut store) = remembering(&[
            (Kind::Note, "fees are kept in cents"),
            (Kind::Note, "fees and the ledger, cents and float rounding"),
        ]);
        let picky = Meanings { model: "picky" };
        struct Picky(Meanings);
        impl Embed for Picky {
            fn model(&self) -> &str {
                self.0.model()
            }
            fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                self.0.embed_passages(texts)
            }
            fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
                self.0.embed_query(text)
            }
            fn min_similarity(&self) -> f32 {
                0.3
            }
            fn near_best(&self) -> f32 {
                0.1
            }
        }
        let found = store
            .search(Path::new(APP), "money", None, 10, Some(&Picky(picky)))
            .unwrap();
        assert_eq!(ids(&found), [1]);
    }

    /// A stand-in with a reranker: an entry answers a query by how much of
    /// what it means the query asks about, and a reranker that fails, or
    /// leaves an entry unscored, when `broken` says so.
    struct Reading {
        broken: Option<&'static str>,
    }

    impl Embed for Reading {
        fn model(&self) -> &str {
            "meanings"
        }

        fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            MEANINGS.embed_passages(texts)
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            MEANINGS.embed_query(text)
        }

        fn min_similarity(&self) -> f32 {
            0.1
        }

        fn near_best(&self) -> f32 {
            1.0
        }

        fn rerank(&self, query: &str, passages: &[&str]) -> Result<Option<Vec<f32>>> {
            match self.broken {
                Some("fails") => bail!("the GPU went away"),
                Some("short") => return Ok(Some(vec![1.0])),
                _ => {}
            }
            let asked = MEANINGS.embed_query(query)?;
            Ok(Some(
                MEANINGS
                    .embed_passages(passages)?
                    .iter()
                    .map(|passage| dot(&asked, passage))
                    .collect(),
            ))
        }

        fn answers_from(&self) -> f32 {
            0.9
        }

        fn kept_from(&self) -> f32 {
            0.85
        }
    }

    #[test]
    fn the_reranker_puts_what_answers_first_and_leaves_out_what_doesn_t() {
        let (_dir, _socket, mut store) = remembering(&[
            (
                Kind::Note,
                "fees in the ledger, cents, floats, rounding and the money tests",
            ),
            (Kind::Note, "fees are kept in cents"),
            (Kind::Note, "cents, fees and money"),
        ]);
        let project = Path::new(APP);
        let reading = Reading { broken: None };
        let found = store
            .search(project, "fees cents", None, 10, Some(&reading))
            .unwrap();
        // All three have the words, and the third answers; the first, about
        // much else, scores too little to keep.
        assert_eq!(ids(&found), [3, 2]);
        let found = store
            .search(project, "ledger floats", None, 10, Some(&reading))
            .unwrap();
        assert!(found.is_empty(), "nothing answers it: {found:?}");
    }

    #[test]
    fn a_reranker_that_fails_leaves_the_search_as_it_was() {
        let (_dir, _socket, mut store) = remembering(&[
            (Kind::Note, "fees are kept in cents"),
            (Kind::Note, "the deploy checklist"),
        ]);
        let project = Path::new(APP);
        for broken in ["fails", "short"] {
            let reading = Reading {
                broken: Some(broken),
            };
            let found = store
                .search(project, "fees", None, 10, Some(&reading))
                .unwrap();
            assert_eq!(ids(&found), [1], "{broken}");
        }
    }

    #[test]
    fn the_rankings_merge_by_reciprocal_rank() {
        let entries: Vec<Entry> = (1..=4).map(|id| entry(id, Kind::Note, "x")).collect();
        let ranking = |ids: &[usize]| -> Vec<Entry> {
            ids.iter().map(|id| entries[id - 1].clone()).collect()
        };
        let merged = fused(&[ranking(&[1, 2, 3]), ranking(&[3, 4])], 10);
        // 3 is in both; 1 and 4 lead one each, then 2 and 4 are second in
        // one each, and between equals the higher id is newer.
        assert_eq!(ids(&merged), [3, 1, 4, 2]);
        assert_eq!(
            ids(&fused(&[ranking(&[1, 2, 3]), ranking(&[3, 4])], 2)),
            [3, 1]
        );
    }

    #[test]
    fn each_entry_gets_one_vector_a_model_which_goes_with_it() {
        let (_dir, _socket, mut store) = remembering(&[
            (Kind::Note, "fees are kept in cents"),
            (Kind::Note, "deploys"),
        ]);
        let project = Path::new(APP);
        store
            .search(project, "money", None, 10, Some(&MEANINGS))
            .unwrap();
        assert_eq!(vectors(&store), 2);
        assert_eq!(store.embed_missing(&MEANINGS).unwrap(), 0, "none left");

        store
            .add(project, note("the release is on tuesday"))
            .unwrap();
        assert_eq!(store.counts("meanings").unwrap(), (3, 2));
        assert_eq!(store.embed_missing(&MEANINGS).unwrap(), 1);
        assert_eq!(store.counts("meanings").unwrap(), (3, 3));
        // The one forgotten keeps its vector apart.
        store.remove(project, 1).unwrap();
        assert_eq!(vectors(&store), 2);
        assert_eq!(store.forget_vectors_but("meanings").unwrap(), 0);
        assert_eq!(store.forget_vectors_but("another").unwrap(), 3);
        assert_eq!(vectors(&store), 0);

        // Another model's vectors can't be compared: it makes its own.
        let other = Meanings { model: "other" };
        assert_eq!(store.embed_missing(&other).unwrap(), 3);
        assert_eq!(store.embed_missing(&MEANINGS).unwrap(), 3);
    }

    #[test]
    fn a_model_that_fails_leaves_the_search_to_the_words() {
        let (_dir, _socket, mut store) = remembering(&[(Kind::Note, "fees are kept in cents")]);
        let found = store
            .search(Path::new(APP), "fees", None, 10, Some(&Broken))
            .unwrap();
        assert_eq!(ids(&found), [1]);
    }

    #[test]
    fn a_database_from_before_vectors_gets_them() {
        let (_dir, socket) = socket();
        fs::create_dir_all(dir(&socket)).unwrap();
        let conn = Connection::open(dir(&socket).join("memory.db")).unwrap();
        conn.execute_batch(TABLES).unwrap();
        conn.execute_batch("PRAGMA user_version = 1").unwrap();
        drop(conn);

        let mut store = Store::open(&socket).unwrap();
        store
            .add(Path::new(APP), note("fees are kept in cents"))
            .unwrap();
        assert_eq!(store.embed_missing(&MEANINGS).unwrap(), 1);
        let version: usize = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len());
    }

    /// A stand-in whose vectors are at the angle a text starts with, `@10`
    /// for 10 degrees (90 without one), so two texts are as alike as the
    /// cosine of the angle between them: 10 degrees apart is 0.98, past
    /// [`Embed::same_from`]; 25 is 0.91, alike enough for the reranker to be
    /// asked; 40 is 0.77, apart. With `reranker`, it holds a query that says
    /// "unlike" to say something else, and any other the same.
    struct Angled {
        reranker: bool,
    }

    impl Angled {
        fn vector(text: &str) -> Vec<f32> {
            let degrees: f32 = text
                .strip_prefix('@')
                .and_then(|text| text.split_whitespace().next()?.parse().ok())
                .unwrap_or(90.0);
            let (sin, cos) = degrees.to_radians().sin_cos();
            vec![cos, sin]
        }
    }

    impl Embed for Angled {
        fn model(&self) -> &str {
            "angled"
        }

        fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| Angled::vector(text)).collect())
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(Angled::vector(text))
        }

        fn min_similarity(&self) -> f32 {
            0.0
        }

        fn near_best(&self) -> f32 {
            1.0
        }

        fn rerank(&self, query: &str, passages: &[&str]) -> Result<Option<Vec<f32>>> {
            if !self.reranker {
                return Ok(None);
            }
            let score = if query.contains("unlike") { 0.1 } else { 0.6 };
            Ok(Some(vec![score; passages.len()]))
        }
    }

    const ANGLED: Angled = Angled { reranker: true };

    fn adding(store: &mut Store, text: &str, embedder: &dyn Embed) -> Added {
        store
            .add_with(Path::new(APP), note(text), Some(embedder))
            .unwrap()
    }

    #[test]
    fn what_says_the_same_in_other_words_is_the_one_entry_seen_again() {
        let (_dir, _socket, mut store) = remembering(&[(
            Kind::Gotcha,
            "@0 Postgres has to be up for the ledger tests",
        )]);
        let project = Path::new(APP);
        let again = New {
            files: vec!["ledger.rs".into()],
            source: Source::Distilled("fixer".into()),
            ..note("@10 The ledger tests need the database running")
        };
        let Added::Alike(entry) = store.add_with(project, again, Some(&ANGLED)).unwrap() else {
            panic!("a second entry");
        };
        assert_eq!(entry.id, 1);
        assert_eq!(entry.seen, 2);
        assert_eq!(entry.text, "@0 Postgres has to be up for the ledger tests");
        assert_eq!(entry.files, ["ledger.rs"]);
        assert_eq!(store.entries(project).unwrap().len(), 1);

        // Without the models, only the same words are the same.
        let words = store.add(project, note("@10 The ledger tests need the database up"));
        assert!(matches!(words.unwrap(), Added::New(_)));
    }

    #[test]
    fn the_reranker_says_whether_what_is_only_alike_says_the_same() {
        let (_dir, _socket, mut store) = remembering(&[(Kind::Note, "@0 the refund ledger")]);
        assert!(matches!(
            adding(&mut store, "@25 refunds wait for the ledger", &ANGLED),
            Added::Alike(Entry { id: 1, .. })
        ));
        assert!(matches!(
            adding(&mut store, "@25 unlike: refunds skip the ledger", &ANGLED),
            Added::New(Entry { id: 2, .. })
        ));
        let alone = Angled { reranker: false };
        assert!(matches!(
            adding(&mut store, "@-26 refunds go to the ledger", &alone),
            Added::New(Entry { id: 3, .. })
        ));
        assert!(matches!(
            adding(&mut store, "@-70 the ledger's own tests", &ANGLED),
            Added::New(Entry { id: 4, .. })
        ));
        // A new entry keeps the vector it was compared by.
        assert_eq!(store.counts("angled").unwrap(), (4, 4));
    }

    #[test]
    fn a_vector_the_model_made_nothing_of_is_alike_to_none() {
        let entries = vec![
            (entry(1, Kind::Note, "one"), vec![f32::NAN, f32::NAN]),
            (entry(2, Kind::Note, "two"), vec![1.0, 0.0]),
        ];
        let alike = alike_to(&[1.0, 0.0], &entries, 0.5);
        let ids: Vec<u64> = alike.iter().map(|(_, entry)| entry.id).collect();
        assert_eq!(ids, [2]);
    }

    /// What `texts` come to in the project's memory, each said by the user,
    /// or with "(distilled)" in it, by the distiller.
    fn said(texts: &[&str]) -> (tempfile::TempDir, Store) {
        let (dir, socket) = socket();
        let mut store = Store::open(&socket).unwrap();
        for text in texts {
            let source = if text.contains("(distilled)") {
                Source::Distilled("fixer".into())
            } else {
                Source::User
            };
            let new = New {
                source,
                files: vec![format!("{}.rs", text.split_whitespace().nth(1).unwrap())],
                ..note(text)
            };
            store.add(Path::new(APP), new).unwrap();
        }
        (dir, store)
    }

    fn groups(merges: &[Merge]) -> Vec<(u64, Vec<u64>)> {
        merges
            .iter()
            .map(|merge| {
                let merged = merge.merged.iter().map(|twin| twin.entry.id).collect();
                (merge.kept.id, merged)
            })
            .collect()
    }

    #[test]
    fn dedupe_keeps_what_was_remembered_and_what_most_say_the_same_as() {
        // Thirty degrees apart is too far to say the same.
        let (_dir, mut store) = said(&[
            "@0 one (distilled)",
            "@14 two (distilled)",
            "@30 three",
            "@80 four (distilled)",
            "@96 five (distilled)",
            "@112 six (distilled)",
        ]);
        let merges = store.twins(Path::new(APP), &ANGLED).unwrap();
        // Three, remembered, is kept over two, which more say the same as,
        // and one doesn't go into it through two. Five is the one four and
        // six both say the same as.
        assert_eq!(groups(&merges), [(3, vec![2]), (5, vec![4, 6])]);
        assert!((merges[0].merged[0].alike - 16_f32.to_radians().cos()).abs() < 1e-6);
    }

    #[test]
    fn dedupe_merges_straight_into_the_one_kept_never_through_another() {
        let (_dir, mut store) = said(&["@0 one", "@25 two (distilled)", "@50 three (distilled)"]);
        let merges = store.twins(Path::new(APP), &ANGLED).unwrap();
        assert_eq!(groups(&merges), [(1, vec![2])]);
        let alone = Angled { reranker: false };
        assert!(store.twins(Path::new(APP), &alone).unwrap().is_empty());
        let (_dir, socket) = socket();
        let refused = dedupe(&socket, Path::new(APP), None, false).unwrap_err();
        assert!(
            refused.to_string().contains("crystal memory embed"),
            "{refused}"
        );
    }

    #[test]
    fn a_merge_counts_each_merged_as_said_again_and_takes_its_files() {
        let (_dir, mut store) = said(&["@0 one", "@5 two (distilled)", "@8 three (distilled)"]);
        let project = Path::new(APP);
        // None holds still, and two, said last, is anchored as it was then.
        store
            .conn
            .execute_batch(
                "UPDATE entries SET anchors = '{\"two.rs\":\"beef\"}' WHERE id = 1;
                 UPDATE entries SET seen = 3, last_seen = 9999999999,
                   anchors = '{\"two.rs\":\"cafe\"}' WHERE id = 2;
                 UPDATE entries SET anchors = '{\"three.rs\":\"dead\"}', used = 1234
                   WHERE id = 3;",
            )
            .unwrap();
        let merges = store.twins(project, &ANGLED).unwrap();
        let done = store.merge(project, &merges).unwrap();
        assert_eq!(groups(&done), [(1, vec![2, 3])]);
        let kept = store.get(project, 1).unwrap().unwrap();
        assert_eq!((kept.seen, kept.last_seen), (5, 9_999_999_999));
        assert_eq!(kept.used, Some(1234));
        assert!(!kept.expired(u64::MAX), "found again, it lasts");
        assert_eq!(kept.files, ["one.rs", "two.rs", "three.rs"]);
        let anchors: Vec<(&str, &str)> = kept
            .anchors
            .iter()
            .map(|(file, hash)| (file.as_str(), hash.as_str()))
            .collect();
        assert_eq!(anchors, [("three.rs", "dead"), ("two.rs", "cafe")]);
        assert_eq!(done[0].kept, kept);
        let ids: Vec<u64> = store
            .entries(project)
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, [1]);
        assert_eq!(store.merged_into(project, 2).unwrap(), Some(1));
        assert_eq!(store.merged_into(project, 1).unwrap(), None);
        // Merging again finds nothing, and what's gone isn't merged twice.
        assert!(store.twins(project, &ANGLED).unwrap().is_empty());
        assert!(store.merge(project, &merges).unwrap().is_empty());
    }

    #[test]
    fn the_one_kept_holds_as_well_as_the_freshest_that_went_into_it() {
        let project = project_with(&["a.rs", "b.rs"]);
        let (_dir, socket) = socket();
        let mut store = Store::open(&socket).unwrap();
        let one = New {
            files: vec!["a.rs".into()],
            ..note("@0 one")
        };
        store.add(project.path(), one).unwrap();
        fs::remove_file(project.path().join("a.rs")).unwrap();
        let stale = |store: &mut Store| {
            let kept = store.get(project.path(), 1).unwrap().unwrap();
            checked(kept, project.path()).freshness
        };
        assert_eq!(stale(&mut store), Freshness::Stale);
        // Two's file hasn't changed, so it holds still, and so does what it
        // says.
        let two = New {
            files: vec!["b.rs".into(), "gone.rs".into()],
            ..note("@5 two")
        };
        store.add(project.path(), two).unwrap();
        let merges = store.twins(project.path(), &ANGLED).unwrap();
        store.merge(project.path(), &merges).unwrap();
        assert_eq!(stale(&mut store), Freshness::Fresh);
        let kept = store.get(project.path(), 1).unwrap().unwrap();
        assert_eq!(kept.files, ["a.rs", "b.rs", "gone.rs"]);
        let anchored: Vec<&String> = kept.anchors.keys().collect();
        assert_eq!(anchored, ["b.rs"], "a file that isn't there isn't anchored");
    }

    #[test]
    fn what_was_merged_said_again_is_the_one_kept_said_again() {
        let (_dir, mut store) = said(&["@0 one", "@5 two (distilled)"]);
        let project = Path::new(APP);
        let merges = store.twins(project, &ANGLED).unwrap();
        store.merge(project, &merges).unwrap();
        let distilled = New {
            source: Source::Distilled("fixer".into()),
            ..note("@5 TWO (distilled)!")
        };
        // By its words, without the models.
        let Added::Alike(kept) = store.add(project, distilled.clone()).unwrap() else {
            panic!("added back");
        };
        assert_eq!((kept.id, kept.seen), (1, 3));

        // Once the one kept is forgotten, the distiller can't bring back
        // what went into it either; the user can.
        store.remove(project, 1).unwrap();
        assert_eq!(store.add(project, distilled).unwrap(), Added::Refused);
        let Added::New(entry) = store.add(project, note("@5 two (distilled)")).unwrap() else {
            panic!("not added back");
        };
        assert_eq!(entry.id, 3);
        assert_eq!(store.merged_into(project, 2).unwrap(), None);
    }

    #[test]
    fn what_went_into_an_entry_merged_since_goes_with_it() {
        let (_dir, mut store) = said(&["@0 one (distilled)", "@15 two (distilled)"]);
        let project = Path::new(APP);
        let merges = store.twins(project, &ANGLED).unwrap();
        assert_eq!(
            groups(&store.merge(project, &merges).unwrap()),
            [(1, vec![2])]
        );
        store.add(project, note("@20 three")).unwrap();
        let merges = store.twins(project, &ANGLED).unwrap();
        assert_eq!(
            groups(&store.merge(project, &merges).unwrap()),
            [(3, vec![1])]
        );
        assert_eq!(store.merged_into(project, 2).unwrap(), Some(3));
        let kept = store.get(project, 3).unwrap().unwrap();
        assert_eq!(kept.seen, 3);
    }

    #[test]
    fn the_nearest_to_what_was_said_are_nearest_to_any_of_it() {
        let (_dir, mut store) = said(&["@0 one", "@30 two", "@60 three", "@90 four"]);
        let project = Path::new(APP);
        store
            .conn
            .execute("UPDATE entries SET kind = 'outcome' WHERE id = 2", [])
            .unwrap();
        let near = store
            .nearest(project, &["@88 what was said", "@10 and more"], 2, &ANGLED)
            .unwrap();
        assert_eq!(ids(&near), [4, 1], "outcomes left out");
        assert!(store.nearest(project, &[], 2, &ANGLED).unwrap().is_empty());
    }

    /// Merges what [`Store::twins`] finds in `store`'s project.
    fn deduped(store: &mut Store) -> Vec<(u64, Vec<u64>)> {
        let merges = store.twins(Path::new(APP), &ANGLED).unwrap();
        groups(&store.merge(Path::new(APP), &merges).unwrap())
    }

    #[test]
    fn a_merged_entry_s_words_still_find_the_one_it_went_into() {
        let (_dir, mut store) = said(&[
            "@0 fees are kept in cents",
            "@5 money is stored as integers (distilled)",
        ]);
        assert_eq!(deduped(&mut store), [(1, vec![2])]);
        let found = store.search(Path::new(APP), "integers", None, 10, None);
        assert_eq!(ids(&found.unwrap()), [1]);
        // Forgotten, its words go with it.
        store.remove(Path::new(APP), 1).unwrap();
        let found = store.search(Path::new(APP), "integers", None, 10, None);
        assert!(found.unwrap().is_empty());
    }

    #[test]
    fn an_entry_means_what_those_merged_into_it_meant_too() {
        let (_dir, mut store) = said(&["@0 one", "@20 two (distilled)", "@35 three"]);
        assert_eq!(deduped(&mut store), [(1, vec![2])]);
        // One is 23 degrees from the query and three 12, but two, merged
        // into one, is 3.
        let found = store.search(Path::new(APP), "@23 query", None, 10, Some(&ANGLED));
        assert_eq!(ids(&found.unwrap()), [1, 3]);
        // And what says what two said is one said again.
        let again = store.add_with(Path::new(APP), note("@21 two again"), Some(&ANGLED));
        assert!(matches!(again.unwrap(), Added::Alike(Entry { id: 1, .. })));
    }

    /// A stand-in whose reranker scores a passage by whether it has the
    /// query's last word.
    struct Wording;

    impl Embed for Wording {
        fn model(&self) -> &str {
            "angled"
        }

        fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            ANGLED.embed_passages(texts)
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            ANGLED.embed_query(text)
        }

        fn min_similarity(&self) -> f32 {
            0.0
        }

        fn near_best(&self) -> f32 {
            1.0
        }

        fn rerank(&self, query: &str, passages: &[&str]) -> Result<Option<Vec<f32>>> {
            let word = query.split_whitespace().last().unwrap_or_default();
            let scores = passages
                .iter()
                .map(|passage| if passage.contains(word) { 0.9 } else { -1.0 })
                .collect();
            Ok(Some(scores))
        }
    }

    #[test]
    fn the_reranker_reads_an_entry_with_the_words_merged_into_it() {
        let found = vec![
            entry(1, Kind::Note, "fees in cents"),
            entry(2, Kind::Note, "deploys"),
        ];
        let merged = HashMap::from([(1, "money as integers".to_string())]);
        let read = reranked(found.clone(), "@0 integers", &Wording, 10, &merged);
        assert_eq!(ids(&read), [1]);
        let unmerged = reranked(found, "@0 integers", &Wording, 10, &HashMap::new());
        assert!(unmerged.is_empty(), "nothing answers it: {unmerged:?}");
    }

    #[test]
    fn what_was_forgotten_crystal_can_t_add_back_in_other_words() {
        let (_dir, _socket, mut store) =
            remembering(&[(Kind::Gotcha, "@0 the ledger tests need redis up")]);
        let project = Path::new(APP);
        // Forgotten with no vector yet: it gets one as it's needed.
        store.remove(project, 1).unwrap();
        let distilled = |text: &str| New {
            source: Source::Distilled("fixer".into()),
            ..note(text)
        };
        let again = store.add_with(
            project,
            distilled("@10 redis has to run for the ledger"),
            Some(&ANGLED),
        );
        assert_eq!(again.unwrap(), Added::Refused);
        let unlike = store.add_with(
            project,
            distilled("@-25 unlike: the ledger skips redis"),
            Some(&ANGLED),
        );
        assert!(matches!(unlike.unwrap(), Added::New(Entry { id: 2, .. })));
        let apart = store.add_with(
            project,
            distilled("@60 deploys go out on tuesdays"),
            Some(&ANGLED),
        );
        assert!(matches!(apart.unwrap(), Added::New(Entry { id: 3, .. })));
        // The user can, in any words.
        let theirs = store.add_with(
            project,
            note("@10 redis has to run for the ledger"),
            Some(&ANGLED),
        );
        assert!(matches!(theirs.unwrap(), Added::New(Entry { id: 4, .. })));
        // Forgotten with its vector, kept apart as it goes.
        store.remove(project, 3).unwrap();
        let again = store.add_with(project, distilled("@58 deploys on tuesdays"), Some(&ANGLED));
        assert_eq!(again.unwrap(), Added::Refused);
    }

    /// A stand-in that makes nothing of a text with "nan" in it, as the
    /// model once did of one, every number of its vector NaN.
    struct Faulty;

    impl Embed for Faulty {
        fn model(&self) -> &str {
            "angled"
        }

        fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            let vectors = ANGLED.embed_passages(texts)?;
            Ok(texts
                .iter()
                .zip(vectors)
                .map(|(text, vector)| match text.contains("nan") {
                    true => vec![f32::NAN; vector.len()],
                    false => vector,
                })
                .collect())
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            ANGLED.embed_query(text)
        }

        fn min_similarity(&self) -> f32 {
            0.0
        }

        fn near_best(&self) -> f32 {
            1.0
        }
    }

    #[test]
    fn a_vector_that_isn_t_numbers_is_never_kept_and_one_kept_is_made_again() {
        let (_dir, _socket, mut store) = remembering(&[(Kind::Note, "@0 fees are kept in cents")]);
        let project = Path::new(APP);
        // Added, it's kept with no vector, and tried again each time.
        let added = store.add_with(project, note("@1 a nan of a note"), Some(&Faulty));
        assert!(matches!(added.unwrap(), Added::New(Entry { id: 2, .. })));
        assert_eq!(store.counts("angled").unwrap(), (2, 1));
        assert_eq!(store.embed_missing(&Faulty).unwrap(), 1);
        assert_eq!(store.counts("angled").unwrap(), (2, 1));
        // One kept from before, a vector of NaN, is let go and made again.
        store
            .conn
            .execute(
                "UPDATE vectors SET vector = ?1",
                params![bytes_of(&[f32::NAN, f32::NAN])],
            )
            .unwrap();
        assert_eq!(store.embed_missing(&ANGLED).unwrap(), 2);
        assert_eq!(store.counts("angled").unwrap(), (2, 2));
        let found = store
            .search(project, "@0 cents", None, 10, Some(&ANGLED))
            .unwrap();
        assert_eq!(ids(&found), [1, 2]);
    }

    #[test]
    fn a_database_from_before_merged_words_finds_the_kept_by_what_went_into_it() {
        let (_dir, socket) = socket();
        fs::create_dir_all(dir(&socket)).unwrap();
        let conn = Connection::open(dir(&socket).join("memory.db")).unwrap();
        // The steps before the words merged into an entry were searched.
        for step in &MIGRATIONS[..10] {
            step(&conn).unwrap();
        }
        conn.execute_batch(
            "PRAGMA user_version = 10;
             INSERT INTO projects (path, next_id) VALUES ('/code/app', 3);
             INSERT INTO entries (project, id, kind, text, key, source, created, last_seen)
               VALUES ('/code/app', 1, 'decision', 'fees are kept in cents', 'k', '\"user\"', 1, 1);
             INSERT INTO merged (project, key, kept, id, kind, text, source, merged)
               VALUES ('/code/app', 'h', 1, 2, 'note', 'money is stored as integers',
                 '\"user\"', 1);",
        )
        .unwrap();
        drop(conn);
        let mut store = Store::open(&socket).unwrap();
        let found = store.search(Path::new(APP), "integers", None, 10, None);
        assert_eq!(ids(&found.unwrap()), [1]);
        let found = store.search(Path::new(APP), "cents", None, 10, None);
        assert_eq!(ids(&found.unwrap()), [1]);
    }

    #[test]
    fn the_drifting_and_the_stale_are_marked_where_they_rank() {
        let project = project_with(&["a.rs", "b.rs"]);
        fs::write(project.path().join("a.rs"), "fn ledger_timeout() {}").unwrap();
        let naming = |id, names: &[&str]| Entry {
            names: names.iter().map(|name| name.to_string()).collect(),
            ..entry(id, Kind::Note, "ledger timeout")
        };
        let stale = naming(1, &["ledger_retries"]);
        let drifting = naming(2, &["ledger_timeout", "ledger_retries"]);
        let fresh = naming(3, &["ledger_timeout", "a.rs"]);
        let listed = marked(vec![stale, drifting, fresh], project.path());
        let ids: Vec<u64> = listed.iter().map(|item| item.entry.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        let freshness: Vec<Freshness> = listed.iter().map(|item| item.freshness).collect();
        assert_eq!(
            freshness,
            [Freshness::Stale, Freshness::Drifting, Freshness::Fresh]
        );
    }

    /// What a session asked `asked` in a worktree that changed nothing is
    /// told as it starts.
    fn launch(
        socket: &Path,
        project: &Path,
        asked: &str,
        reader: Reader,
    ) -> Result<Option<String>> {
        for_launch(socket, project, asked, &[], reader, None)
    }

    #[test]
    fn a_session_is_shown_what_it_was_asked_about_first() {
        let (_dir, socket, _store) = remembering(&[
            (Kind::Gotcha, "the refund test needs the ledger running"),
            (Kind::Note, "the docs build with mdbook"),
        ]);
        let paragraph = launch(
            &socket,
            Path::new(APP),
            "fix the flaky refund test",
            Reader::Claude,
        )
        .unwrap()
        .unwrap();
        assert!(paragraph.starts_with("What this project's earlier sessions learned:"));
        assert!(
            paragraph.contains("\n- 1 (gotcha) the refund test needs the ledger running"),
            "{paragraph}"
        );
        assert!(
            !paragraph.contains("mdbook"),
            "only what has to do with the task"
        );
        assert!(paragraph.contains("memory_search tool"));
        assert!(paragraph.contains("memory_show tool"));
        assert!(paragraph.contains("crystal remember"));
    }

    #[test]
    fn with_nothing_relevant_a_session_is_shown_the_newest() {
        let (_dir, socket, _store) = remembering(&[(Kind::Note, "the docs build with mdbook")]);
        let paragraph = launch(&socket, Path::new(APP), "", Reader::Claude)
            .unwrap()
            .unwrap();
        assert!(paragraph.contains("- 1 (note) the docs build with mdbook"));
        let paragraph = launch(&socket, Path::new(APP), "deploy friday", Reader::Claude)
            .unwrap()
            .unwrap();
        assert!(paragraph.contains("- 1 (note) the docs build with mdbook"));
    }

    #[test]
    fn with_nothing_remembered_a_session_is_told_how_to_add() {
        let (_dir, socket) = socket();
        let paragraph = launch(&socket, Path::new(APP), "anything", Reader::Claude)
            .unwrap()
            .unwrap();
        assert!(paragraph.starts_with("When you learn something"));
        assert_eq!(
            launch(&socket, Path::new(APP), "anything", Reader::Task).unwrap(),
            None,
            "a task in the background has nothing to be told"
        );
    }

    #[test]
    fn a_task_s_outcome_is_found_by_a_search_but_never_shown_at_launch() {
        let (_dir, socket, mut store) = remembering(&[
            (Kind::Gotcha, "the refund test needs the ledger running"),
            (
                Kind::Outcome,
                "fix the flaky refund test: fixed it, uncommitted",
            ),
        ]);
        let project = Path::new(APP);
        for asked in ["fix the flaky refund test", ""] {
            let paragraph = launch(&socket, project, asked, Reader::Claude)
                .unwrap()
                .unwrap();
            assert!(
                paragraph.contains("\n- 1 (gotcha) the refund test needs the ledger running"),
                "{paragraph}"
            );
            assert!(!paragraph.contains("uncommitted"), "{paragraph}");
        }
        assert_eq!(
            ids(&store
                .search(project, "uncommitted", None, 10, None)
                .unwrap()),
            [2]
        );
        assert_eq!(
            ids(&store
                .search(project, "", Some(Kind::Outcome), 10, None)
                .unwrap()),
            [2]
        );

        // With only outcomes, there's nothing to show.
        let (_dir, socket, _store) = remembering(&[(Kind::Outcome, "fix it: fixed it")]);
        let paragraph = launch(&socket, project, "fix it", Reader::Claude)
            .unwrap()
            .unwrap();
        assert!(
            paragraph.starts_with("When you learn something"),
            "{paragraph}"
        );
    }

    #[test]
    fn outcomes_take_no_place_from_what_a_session_is_shown() {
        // More outcomes than the reranker reads, each ahead of the gotcha by
        // its words and by its meaning.
        let mut outcomes = Vec::new();
        for money in 0..5 {
            for fees in [[0, 1], [0, 2], [0, 3], [1, 2], [1, 3], [2, 3]] {
                let mut words = vec!["cents"; 4];
                words[fees[0]] = "fees";
                words[fees[1]] = "fees";
                words.insert(money, "money");
                outcomes.push(words.join(" "));
            }
        }
        assert!(outcomes.len() > RERANK_POOL);
        let mut texts = vec![(Kind::Gotcha, "fees are kept in cents")];
        texts.extend(outcomes.iter().map(|text| (Kind::Outcome, text.as_str())));
        texts.push((Kind::Note, "the deploy checklist"));
        let (_dir, socket, _store) = remembering(&texts);

        /// A reranker every passage answers.
        struct Answering;
        impl Embed for Answering {
            fn model(&self) -> &str {
                MEANINGS.model()
            }
            fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                MEANINGS.embed_passages(texts)
            }
            fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
                MEANINGS.embed_query(text)
            }
            fn min_similarity(&self) -> f32 {
                0.1
            }
            fn near_best(&self) -> f32 {
                1.0
            }
            fn rerank(&self, _: &str, passages: &[&str]) -> Result<Option<Vec<f32>>> {
                Ok(Some(vec![1.0; passages.len()]))
            }
        }
        let project = Path::new(APP);
        let answering: Option<&dyn Embed> = Some(&Answering);
        let paragraph = for_launch(
            &socket,
            project,
            "fees cents",
            &[],
            Reader::Claude,
            answering,
        )
        .unwrap()
        .unwrap();
        assert!(
            paragraph.contains("\n- 1 (gotcha) fees are kept in cents"),
            "{paragraph}"
        );
        assert!(!paragraph.contains("(outcome)"), "{paragraph}");
        // Found, so the newest aren't shown in its place.
        assert!(!paragraph.contains("deploy checklist"), "{paragraph}");
    }

    #[test]
    fn a_task_in_the_background_is_shown_ids_and_told_of_its_tools() {
        let (_dir, socket, _store) = remembering(&[(Kind::Gotcha, "the ledger needs redis")]);
        let paragraph = launch(&socket, Path::new(APP), "fix the ledger", Reader::Task)
            .unwrap()
            .unwrap();
        assert!(
            paragraph.contains("\n- 1 (gotcha) the ledger needs redis"),
            "{paragraph}"
        );
        assert!(paragraph.contains("memory_search tool"));
        assert!(paragraph.contains("memory_show tool"));
        assert!(!paragraph.contains("crystal remember"));
    }

    #[test]
    fn a_stale_entry_is_never_shown_at_launch() {
        let (_dir, socket) = socket();
        let project = project_with(&["refund.rs"]);
        fs::write(project.path().join("refund.rs"), "fn refund_waits() {}").unwrap();
        let named = New {
            text: "refund waits for the ledger: refund_waits".into(),
            ..about(&["refund.rs"], project.path())
        };
        add(&socket, project.path(), named).unwrap();
        fs::write(project.path().join("refund.rs"), "fn refund_now() {}").unwrap();
        let paragraph = launch(&socket, project.path(), "refund", Reader::Claude)
            .unwrap()
            .unwrap();
        assert!(!paragraph.contains("refund waits"), "{paragraph}");
    }

    #[test]
    fn a_drifting_entry_is_shown_marked_where_it_ranks() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs", "b.rs"]);
        let at = project.path();
        add(&socket, at, about(&["a.rs", "b.rs"], at)).unwrap();
        add(&socket, at, note("the ledger is slow to start")).unwrap();
        fs::write(at.join("a.rs"), "changed").unwrap();
        let paragraph = launch(&socket, at, "refund ledger", Reader::Claude)
            .unwrap()
            .unwrap();
        let lines: Vec<&str> = paragraph.lines().collect();
        // It has both words, so it comes first, marked.
        assert_eq!(
            lines[1..3],
            [
                "- 1 (note) refund waits for the ledger [a.rs, b.rs] \
                 [drifting: some of its files changed since]",
                "- 2 (note) the ledger is slow to start",
            ]
        );
    }

    #[test]
    fn entries_about_what_the_worktree_changed_come_first() {
        let (_dir, socket) = socket();
        let project = project_with(&["fees.rs"]);
        let at = project.path();
        add(
            &socket,
            at,
            note("the refund test needs the ledger running"),
        )
        .unwrap();
        let fees = New {
            text: "fees are kept in cents".into(),
            ..about(&["fees.rs"], at)
        };
        add(&socket, at, fees).unwrap();
        add(&socket, at, note("the docs build with mdbook")).unwrap();
        let changed = ["fees.rs".to_string(), "other.rs".to_string()];
        let paragraph = for_launch(
            &socket,
            at,
            "fix the refund test",
            &changed,
            Reader::Claude,
            None,
        )
        .unwrap()
        .unwrap();
        let lines: Vec<&str> = paragraph.lines().collect();
        assert_eq!(
            lines[1..3],
            [
                "- 2 (note) fees are kept in cents [fees.rs]",
                "- 1 (note) the refund test needs the ledger running",
            ]
        );
        assert!(!paragraph.contains("mdbook"), "{paragraph}");
    }

    #[test]
    fn what_a_session_is_shown_keeps_to_its_budget_the_least_relevant_left_out() {
        let long = |id| Listed {
            entry: entry(id, Kind::Note, &"ledger ".repeat(40)),
            freshness: Freshness::Fresh,
            gone: Vec::new(),
        };
        let short = |id| Listed {
            entry: entry(id, Kind::Note, "ledger"),
            freshness: Freshness::Fresh,
            gone: Vec::new(),
        };
        let shown = [long(1), long(2), long(3), short(4)];
        let lines = fitted(&shown);
        // The third doesn't fit after two; the shorter one after it does.
        let ids: Vec<&str> = lines.iter().map(|line| &line[..1]).collect();
        assert_eq!(ids, ["1", "2", "4"]);
        let size: usize = lines.iter().map(|line| line.len() + 3).sum();
        assert!(size <= LAUNCH_BYTES, "{size}");

        let many: Vec<Listed> = (1..=9).map(short).collect();
        assert_eq!(fitted(&many).len(), SHOWN_AT_LAUNCH);
    }

    #[test]
    fn another_agent_is_told_crystal_s_commands_rather_than_its_tools() {
        let (_dir, socket, _store) = remembering(&[(Kind::Gotcha, "the ledger needs redis")]);
        let paragraph = launch(&socket, Path::new(APP), "fix the ledger", Reader::Agent)
            .unwrap()
            .unwrap();
        assert!(paragraph.contains("\n- 1 (gotcha) the ledger needs redis"));
        assert!(
            paragraph.contains("`crystal memory search <words>`"),
            "{paragraph}"
        );
        assert!(
            paragraph.contains("`crystal memory show <id>`"),
            "{paragraph}"
        );
        assert!(paragraph.contains("crystal remember"), "{paragraph}");
        assert!(!paragraph.contains("memory_search tool"), "{paragraph}");
    }

    #[test]
    fn a_database_from_before_anchors_anchors_its_entries_once() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs", "b.rs"]);
        fs::create_dir_all(dir(&socket)).unwrap();
        let conn = Connection::open(dir(&socket).join("memory.db")).unwrap();
        conn.execute_batch(TABLES).unwrap();
        conn.execute_batch(VECTORS).unwrap();
        conn.execute_batch("PRAGMA user_version = 2").unwrap();
        let name = project.path().to_string_lossy();
        conn.execute(
            "INSERT INTO projects (path, next_id) VALUES (?1, 4)",
            params![name],
        )
        .unwrap();
        // Said after its file last changed; said before; about no file.
        let later = seconds_since_epoch(SystemTime::now()) + 60;
        for (id, files, said) in [
            (1, r#"["a.rs"]"#, later),
            (2, r#"["a.rs", "b.rs"]"#, 1_000),
            (3, "[]", 1_000),
        ] {
            conn.execute(
                "INSERT INTO entries (project, id, kind, text, key, files, source, created, \
                 last_seen) VALUES (?1, ?2, 'note', ?3, ?3, ?4, '\"user\"', ?5, ?5)",
                params![name, id, format!("entry {id}"), files, said],
            )
            .unwrap();
        }
        drop(conn);

        let memory = Memory::read(&socket, project.path()).unwrap();
        let freshness: Vec<Freshness> = memory.listed().iter().map(|item| item.freshness).collect();
        assert_eq!(
            freshness,
            [Freshness::Fresh, Freshness::Drifting, Freshness::Fresh],
            "newest first"
        );
        // Anchored to what the file holds, it goes on from there.
        fs::write(project.path().join("a.rs"), "changed").unwrap();
        let holds = checked(memory.get(1).unwrap().clone(), &memory.project);
        assert_eq!(holds.freshness, Freshness::Drifting);
    }

    #[test]
    fn an_entry_names_what_looks_like_code_in_it() {
        let names = |text| names_in(text);
        assert_eq!(
            names(
                "In tests/cli.rs, wait with Crystal::listening()/start_daemon_with, never \
                 socket.exists(); every test goes through `outside_crystal()`."
            ),
            [
                "tests/cli.rs",
                "start_daemon_with",
                "socket.exists",
                "outside_crystal"
            ]
        );
        // Of a path in code, a type, a variant or a constant by its name,
        // and anything else when it looks like code or is in backticks.
        assert_eq!(
            names("Request::Shutdown, Profile::check, Store::open_db and `Crystal::listening()`"),
            ["Shutdown", "open_db", "listening"]
        );
        assert_eq!(
            names("Set CRYSTAL_AGENT_HOOKS, then TaskRecord and insteadOf; memory.stale fires."),
            [
                "CRYSTAL_AGENT_HOOKS",
                "TaskRecord",
                "insteadOf",
                "memory.stale"
            ]
        );
        assert_eq!(
            names("Run `crystal kill-server --socket /tmp/x` and `cargo test -- --test-threads=4`"),
            ["kill-server", "--socket", "--test-threads"]
        );
        assert_eq!(
            names("`wait` and `agents/` and src/agent_rules.rs, `memory.db`; Cargo.toml too"),
            [
                "wait",
                "agents/",
                "src/agent_rules.rs",
                "memory.db",
                "Cargo.toml"
            ]
        );
        // Words a slash sets side by side are each looked at alone.
        assert_eq!(
            names("add/list/export, CLAUDE.md/AGENTS.md and run_hook/run_task"),
            ["CLAUDE.md", "AGENTS.md", "run_hook", "run_task"]
        );
        // Prose, what's outside the project and what names nothing.
        assert!(names("Agent rule files compile lazily on first use, e.g. now.").is_empty());
        assert!(names("On macOS `true` is None; ~/.config/x, /tmp/y.rs, a/b, -k, #62").is_empty());
        assert!(names("see https://example.com/a.md and v0.2.0").is_empty());
        // The paths of the files an entry is about say only where it is.
        let files = ["tests/cli.rs".to_string(), "agents/".to_string()];
        assert_eq!(
            names_beside(
                "cli.rs, tests/cli.rs, `agents/` and src/x.rs: run_hook",
                &files
            ),
            ["src/x.rs", "run_hook"]
        );
        let many: String = (0..20).map(|n| format!("name_{n} ")).collect();
        assert_eq!(names(&many).len(), MAX_NAMES);
        assert_eq!(names("`same_one` and same_one")[..], ["same_one"]);
    }

    #[test]
    fn a_name_is_looked_for_among_the_words_and_paths_of_the_code() {
        let project = project_with(&[]);
        let at = project.path();
        fs::create_dir_all(at.join("src/tui")).unwrap();
        fs::create_dir(at.join(".hidden")).unwrap();
        fs::write(
            at.join("src/tui/app.rs"),
            "#[arg(long)]\nremove_worktree: bool,\n// crystal kill-server; see docs/guide.md.\n",
        )
        .unwrap();
        fs::write(
            at.join("README.md"),
            "Run `make e2e` with --test-threads=4.",
        )
        .unwrap();
        fs::write(at.join(".hidden/secret.rs"), "fn hidden_away() {}").unwrap();
        fs::write(at.join("data.bin"), b"fn in_binary() {}\0\0").unwrap();
        let words = Words::of(at);
        for there in [
            "remove_worktree",
            "--remove-worktree",
            "--test-threads",
            "kill-server",
            "docs/guide.md",
            "guide.md",
            "src/tui/app.rs",
            "tui/app.rs",
            "app.rs",
            "src/tui/",
            "src/",
            "README.md",
            "e2e",
        ] {
            assert!(words.has(there), "{there}");
        }
        for gone in [
            "remove_work",
            "--kill-all",
            "server-kill",
            "src/app.rs",
            "hidden_away",
            "in_binary",
            "docs/manual.md",
        ] {
            assert!(!words.has(gone), "{gone}");
        }
        assert!(!Words::of(&at.join("nowhere")).has("app.rs"));
    }

    /// Writes `text` into `file`, changed when the clock read `changed`.
    fn write_changed(file: &Path, text: &str, changed: SystemTime) {
        fs::write(file, text).unwrap();
        let file = fs::File::options().write(true).open(file).unwrap();
        file.set_modified(changed).unwrap();
    }

    #[test]
    fn a_worktree_s_words_are_read_again_only_where_it_changed() {
        let project = project_with(&[]);
        let at = project.path();
        let long_ago = SystemTime::now() - Duration::from_secs(60);
        write_changed(&at.join("a.rs"), "fn ledger_retry() {}", long_ago);
        write_changed(&at.join("b.rs"), "fn fees_in_cents() {}", long_ago);
        let words = Words::of(at);
        assert!(words.has("ledger_retry") && words.has("fees_in_cents"));

        // Its stamp the same, a file is taken to be as it was read: what
        // shows the kept words are used.
        write_changed(&at.join("a.rs"), "fn ledger_redo_() {}", long_ago);
        assert!(Words::of(at).has("ledger_retry"));
        // Its size or its time changed, it's read again.
        write_changed(&at.join("a.rs"), "fn ledger_redone() {}", long_ago);
        let words = Words::of(at);
        assert!(words.has("ledger_redone") && !words.has("ledger_retry"));
        write_changed(&at.join("a.rs"), "fn ledger_again() {}", SystemTime::now());
        assert!(Words::of(at).has("ledger_again"));
        // Read so soon after it changed, it's read again next time, though
        // its stamp is the same.
        let now = fs::metadata(at.join("a.rs")).unwrap().modified().unwrap();
        write_changed(&at.join("a.rs"), "fn ledger_later() {}", now);
        assert!(Words::of(at).has("ledger_later"));

        // A file gone takes its words with it, but not those another has.
        fs::write(at.join("c.rs"), "fn fees_in_cents() {} fn only_in_c() {}").unwrap();
        assert!(Words::of(at).has("only_in_c"));
        fs::remove_file(at.join("c.rs")).unwrap();
        let words = Words::of(at);
        assert!(!words.has("only_in_c") && !words.has("c.rs"));
        assert!(words.has("fees_in_cents"));
        // What a look took is its own, whatever comes after.
        fs::remove_file(at.join("b.rs")).unwrap();
        assert!(words.has("fees_in_cents"));
        assert!(!Words::of(at).has("fees_in_cents"));
    }

    #[test]
    fn a_database_from_before_names_anchors_its_entries_to_those_in_the_code() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs"]);
        fs::write(project.path().join("a.rs"), "fn ledger_retry() {}").unwrap();
        fs::create_dir_all(dir(&socket)).unwrap();
        let conn = Connection::open(dir(&socket).join("memory.db")).unwrap();
        // The database as it was before names, the eighth step.
        let before_names = 7;
        for step in &MIGRATIONS[..before_names] {
            step(&conn).unwrap();
        }
        conn.execute_batch(&format!("PRAGMA user_version = {before_names}"))
            .unwrap();
        let name = project.path().to_string_lossy();
        conn.execute(
            "INSERT INTO projects (path, next_id) VALUES (?1, 4)",
            params![name],
        )
        .unwrap();
        // Said in a worktree that's gone; in none; about something gone.
        for (id, text, checkout) in [
            (
                1,
                "use `ledger_retry` and LEDGER_GONE",
                Some("/gone/worktree"),
            ),
            (2, "nothing like code", None),
            (3, "MAX_TABS is gone", None),
        ] {
            conn.execute(
                "INSERT INTO entries (project, id, kind, text, key, files, source, created, \
                 last_seen, checkout) VALUES (?1, ?2, 'note', ?3, ?3, '[]', '\"user\"', 1, 1, ?4)",
                params![name, id, text, checkout],
            )
            .unwrap();
        }
        drop(conn);

        let memory = Memory::read(&socket, project.path()).unwrap();
        let names: Vec<Vec<String>> = (1..=3)
            .map(|id| memory.get(id).unwrap().names.clone())
            .collect();
        assert_eq!(names, [vec!["ledger_retry".to_string()], vec![], vec![]]);
        let freshness: Vec<Freshness> = memory.listed().iter().map(|item| item.freshness).collect();
        assert_eq!(freshness, [Freshness::Fresh; 3]);
        fs::write(project.path().join("a.rs"), "fn ledger_retries() {}").unwrap();
        assert_eq!(memory.listed()[2].freshness, Freshness::Stale);
    }

    #[test]
    fn a_stale_entry_anchored_again_or_reworded_holds_again() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs", "b.rs"]);
        let at = project.path();
        fs::write(at.join("a.rs"), "fn ledger_retry() {}").unwrap();
        let mut store = Store::open(&socket).unwrap();
        let named = |text: &str| New {
            text: text.into(),
            ..about(&["a.rs"], at)
        };
        store
            .add(at, named("Flaky calls go through `ledger_retry`"))
            .unwrap();
        store
            .add(at, named("`ledger_retry` waits a second"))
            .unwrap();
        store.add(at, named("Fees are kept in cents")).unwrap();
        store.add(at, about(&["b.rs"], at)).unwrap();
        fs::write(at.join("a.rs"), "fn ledger_retry_twice() {}").unwrap();
        fs::remove_file(at.join("b.rs")).unwrap();
        let stale = |store: &mut Store| -> Vec<u64> {
            let files = ["a.rs".to_string(), "b.rs".to_string()];
            let stale = store.stale_about(at, &files, 10).unwrap();
            stale.iter().map(|item| item.entry.id).collect()
        };
        assert_eq!(stale(&mut store), [4, 2, 1], "said most recently first");
        let a_rs = ["a.rs".to_string()];
        assert_eq!(store.stale_about(at, &a_rs, 1).unwrap().len(), 1);

        let reworded = store
            .reword(at, 1, "Flaky calls go through `ledger_retry_twice`", at)
            .unwrap();
        assert_eq!(reworded.id, 1);
        assert_eq!(reworded.names, ["ledger_retry_twice"]);
        assert_eq!(reworded.kind, Kind::Note);
        let kept = store.reanchor(at, 4, at).unwrap();
        assert!(kept.anchors.is_empty(), "{kept:?}");
        assert_eq!(stale(&mut store), [2]);
        let found = store.search(at, "twice", None, 10, None).unwrap();
        assert_eq!(ids(&found), [1], "the index has the new words");

        let said = store.reword(at, 2, "Fees are kept in CENTS", at);
        assert!(said.unwrap_err().to_string().contains("entry 3 says"));
        store.remove(at, 3).unwrap();
        let forgotten = store.reword(at, 2, "fees are kept in cents", at);
        assert!(forgotten.unwrap_err().to_string().contains("forgotten"));
        assert!(store.reword(at, 2, " ", at).is_err());
        assert!(store.reanchor(at, 9, at).is_err());
    }

    #[test]
    fn the_export_is_markdown_with_where_each_entry_came_from() {
        let mut gotcha = entry(2, Kind::Gotcha, "The ledger tests need redis.\n");
        gotcha.files = vec!["tests/ledger.rs".into()];
        gotcha.source = Source::Session("fixer".into());
        gotcha.seen = 3;
        let listed = [
            Listed {
                entry: gotcha,
                freshness: Freshness::Drifting,
                gone: Vec::new(),
            },
            Listed {
                entry: entry(1, Kind::Note, "Fees are in cents."),
                freshness: Freshness::Fresh,
                gone: Vec::new(),
            },
        ];
        assert_eq!(
            markdown("app", &listed),
            "# app memory\n\
             \n## 2 · gotcha (drifting)\n\nThe ledger tests need redis.\n\n\
             About `tests/ledger.rs`. From session fixer, said 3 times.\n\
             \n## 1 · note\n\nFees are in cents.\n\nFrom you.\n"
        );
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
    fn a_query_keeps_the_telling_words_each_once() {
        assert_eq!(
            fts_query("Fix the flaky refund-test, fix NOW!"),
            Some("\"fix\"* OR \"flaky\"* OR \"refund\"* OR \"test\"* OR \"now\"*".into())
        );
        assert_eq!(
            key_of("Fix the  flaky refund-test!"),
            "fix the flaky refund test"
        );
    }

    #[test]
    fn an_outcome_keeps_its_goal_in_a_sentence_and_what_crystal_done_said() {
        let short = |text: &str| short_outcome(text);
        // A question, and an answer with a colon of its own.
        assert_eq!(
            short(
                "do we have memory like docket? - Memory. Crystal has none.: Checked memory \
                 against docket: no distiller yet"
            )
            .as_deref(),
            Some("do we have memory like docket?: Checked memory against docket: no distiller yet")
        );
        // A brief of a page, under a heading.
        let brief = "# Backlog #41: `/` search beyond sessions\n\nYou are one of six workers.\n\
                     Keep to your item.: / finds projects too: https://example.com/pull/62";
        assert_eq!(
            short(brief).as_deref(),
            Some(
                "Backlog #41: `/` search beyond sessions: / finds projects too: \
                 https://example.com/pull/62"
            )
        );
        assert_eq!(
            short("merge them. Both, in order. (failed): auto mode blocked the merge").as_deref(),
            Some("merge them (failed): auto mode blocked the merge")
        );
        // Neither a list's number nor an abbreviation ends a sentence.
        assert_eq!(
            short(
                "we need to handle this: 1. Claude asks first, e.g. for crystal done. Then \
                 more.\n2. And this.: Allowed crystal's own commands"
            )
            .as_deref(),
            Some(
                "we need to handle this: 1. Claude asks first, e.g. for crystal done: Allowed \
                 crystal's own commands"
            )
        );
        // A long sentence is cut after a word.
        let long = format!("{}: done it", "word ".repeat(40).trim());
        let cut = short(&long).unwrap();
        assert!(cut.ends_with("word…: done it"), "{cut}");
        assert!(cut.chars().count() <= OUTCOME_GOAL + ": done it".len() + 1);
        // No shorter, or nothing to tell the goal from: as it was.
        assert_eq!(
            short("Fix issue #17: Remind it (https://x/17): Stop hook reminds it"),
            None
        );
        assert_eq!(short("fixed it"), None);
    }

    #[test]
    fn a_database_from_before_shortens_its_outcomes_once() {
        let (_dir, socket) = socket();
        fs::create_dir_all(dir(&socket)).unwrap();
        let conn = Connection::open(dir(&socket).join("memory.db")).unwrap();
        for step in &MIGRATIONS[..5] {
            step(&conn).unwrap();
        }
        conn.execute_batch("PRAGMA user_version = 5").unwrap();
        conn.execute(
            "INSERT INTO projects (path, next_id) VALUES (?1, 3)",
            params![APP],
        )
        .unwrap();
        let now = seconds_since_epoch(SystemTime::now());
        let brief = "# Fix the ledger\n\nThe ledger drops a cent on refunds. Find out why.\n\
                     Keep to it.: Fees are kept in cents now";
        let note = "the ledger needs redis: start it first. Always.";
        for (id, kind, text) in [(1, "outcome", brief), (2, "note", note)] {
            conn.execute(
                "INSERT INTO entries (project, id, kind, text, key, source, created, last_seen) \
                 VALUES (?1, ?2, ?3, ?4, ?5, '\"user\"', ?6, ?6)",
                params![APP, id, kind, text, key_of(text), now],
            )
            .unwrap();
        }
        drop(conn);

        let mut store = Store::open(&socket).unwrap();
        let project = Path::new(APP);
        let text = |store: &mut Store, id| store.get(project, id).unwrap().unwrap().text;
        assert_eq!(
            text(&mut store, 1),
            "Fix the ledger: Fees are kept in cents now"
        );
        assert_eq!(text(&mut store, 2), note, "only outcomes");
        // The index follows: the brief's words are gone.
        assert!(
            store
                .search(project, "refunds", None, 10, None)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            ids(&store.search(project, "cents", None, 10, None).unwrap()),
            [1]
        );
        assert_eq!(store.used(project, 2).unwrap().unwrap().id, 2);
    }

    #[test]
    fn lessons_rank_above_the_notes_near_them_unless_asked_what_was_done() {
        let kinds = [
            Kind::Note,
            Kind::Outcome,
            Kind::Gotcha,
            Kind::Note,
            Kind::Decision,
            Kind::Command,
            Kind::Command,
            Kind::Command,
            Kind::Gotcha,
        ];
        let found: Vec<Entry> = kinds
            .iter()
            .enumerate()
            .map(|(at, kind)| entry(at as u64 + 1, *kind, "x"))
            .collect();
        // Each note three places lower, a lesson first where they meet.
        assert_eq!(
            ids(&lessons_first(found.clone(), "the ledger", |entry| entry.kind)),
            [3, 1, 5, 2, 6, 7, 4, 8, 9]
        );
        assert_eq!(
            ids(&lessons_first(
                found,
                "what did we do about the ledger?",
                |entry| entry.kind
            )),
            [1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
        assert!(about_what_was_done("Was the ledger fix MERGED?"));
        assert!(!about_what_was_done("fix the flaky ledger test"));
    }

    #[test]
    fn a_search_finds_lessons_before_the_notes_that_match_as_well() {
        let (_dir, socket, mut store) = remembering(&[
            (Kind::Note, "the ledger flakes on refunds"),
            (Kind::Gotcha, "the ledger needs redis"),
        ]);
        let project = Path::new(APP);
        let found =
            |store: &mut Store, asked| ids(&store.search(project, asked, None, 10, None).unwrap());
        assert_eq!(found(&mut store, "ledger flakes refunds"), [2, 1]);
        assert_eq!(
            found(&mut store, "what happened to the ledger refunds"),
            [1, 2]
        );
        let paragraph = launch(&socket, project, "ledger refunds flakes", Reader::Claude)
            .unwrap()
            .unwrap();
        let lines: Vec<&str> = paragraph.lines().collect();
        assert_eq!(
            lines[1..3],
            [
                "- 2 (gotcha) the ledger needs redis",
                "- 1 (note) the ledger flakes on refunds"
            ]
        );
    }

    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn a_note_or_an_outcome_nobody_finds_again_expires_and_a_lesson_never_does() {
        let now = 100 * DAY;
        let said = |kind, days: u64| Entry {
            created: now - days * DAY,
            last_seen: now - days * DAY,
            ..entry(1, kind, "x")
        };
        assert!(!said(Kind::Note, 29).expired(now));
        assert!(said(Kind::Note, 30).expired(now));
        assert!(!said(Kind::Outcome, 13).expired(now));
        assert!(said(Kind::Outcome, 14).expired(now));
        for kind in [Kind::Decision, Kind::Gotcha, Kind::Command] {
            assert!(!said(kind, 99).expired(now), "{kind}");
        }
        // Found again: said again, or read in full by an agent.
        let again = Entry {
            seen: 2,
            ..said(Kind::Note, 99)
        };
        assert!(!again.expired(now));
        let used = Entry {
            used: Some(now - 60 * DAY),
            ..said(Kind::Note, 99)
        };
        assert!(!used.expired(now));
    }

    #[test]
    fn the_expired_are_left_out_of_searches_and_launch_until_found_again() {
        let (_dir, socket, mut store) = remembering(&[
            (Kind::Note, "the ledger needs redis"),
            (Kind::Gotcha, "the ledger tests are slow"),
        ]);
        let project = Path::new(APP);
        store
            .conn
            .execute(
                "UPDATE entries SET created = created - ?1, last_seen = last_seen - ?1",
                params![40 * DAY],
            )
            .unwrap();
        let found =
            |store: &mut Store, asked| ids(&store.search(project, asked, None, 10, None).unwrap());
        assert_eq!(found(&mut store, "ledger redis"), [2]);
        assert_eq!(found(&mut store, ""), [2], "nor among the newest");
        let all = Wanted {
            expired: true,
            ..Wanted::best(10)
        };
        let listed = store.find(project, "ledger redis", &all, None).unwrap();
        let listed: Vec<u64> = listed.iter().map(|item| item.entry.id).collect();
        assert_eq!(listed, [2, 1], "found when asked for");
        let shown = |asked| {
            launch(&socket, project, asked, Reader::Claude)
                .unwrap()
                .unwrap()
        };
        assert!(!shown("ledger redis").contains("redis"));
        assert!(!shown("").contains("redis"));

        // An agent reading it in full finds it again.
        store.used(project, 1).unwrap();
        assert_eq!(found(&mut store, "ledger redis"), [2, 1]);
        assert!(shown("ledger redis").contains("1 (note) the ledger needs redis"));
    }

    #[test]
    fn a_database_from_before_counts_its_entries_time_from_the_upgrade() {
        let (_dir, socket) = socket();
        fs::create_dir_all(dir(&socket)).unwrap();
        let conn = Connection::open(dir(&socket).join("memory.db")).unwrap();
        // The database as it was before its time was counted, the ninth step.
        let before = 8;
        for step in &MIGRATIONS[..before] {
            step(&conn).unwrap();
        }
        conn.execute_batch(&format!("PRAGMA user_version = {before}"))
            .unwrap();
        conn.execute(
            "INSERT INTO projects (path, next_id) VALUES (?1, 2)",
            params![APP],
        )
        .unwrap();
        let long_ago = seconds_since_epoch(SystemTime::now()) - 100 * DAY;
        conn.execute(
            "INSERT INTO entries (project, id, kind, text, key, source, created, last_seen) \
             VALUES (?1, 1, 'note', 'the ledger needs redis', 'the ledger needs redis', \
             '\"user\"', ?2, ?2)",
            params![APP, long_ago],
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&socket).unwrap();
        let project = Path::new(APP);
        let entry = store.get(project, 1).unwrap().unwrap();
        let now = seconds_since_epoch(SystemTime::now());
        assert!(entry.counted_from.is_some_and(|from| from + 60 >= now));
        assert!(!entry.expired(now), "it has its month from the upgrade");
        assert!(entry.expired(now + 30 * DAY), "and no more");
        let found = store.search(project, "ledger redis", None, 10, None);
        assert_eq!(ids(&found.unwrap()), [1]);
    }

    #[test]
    fn an_entry_s_kind_changes_in_place_and_a_note_made_counts_from_then() {
        let (_dir, _socket, mut store) =
            remembering(&[(Kind::Note, "the ledger tests need redis")]);
        let project = Path::new(APP);
        assert_eq!(store.embed_missing(&MEANINGS).unwrap(), 1);
        // Said long ago and never found again, it's expired as a note.
        store
            .conn
            .execute(
                "UPDATE entries SET created = created - ?1, last_seen = last_seen - ?1",
                params![40 * DAY],
            )
            .unwrap();
        let found =
            |store: &mut Store| ids(&store.search(project, "redis", None, 10, None).unwrap());
        assert!(found(&mut store).is_empty());

        let made = store.set_kind(project, 1, Kind::Gotcha).unwrap();
        assert_eq!(made.kind, Kind::Gotcha);
        assert_eq!(made.text, "the ledger tests need redis");
        // Its vector and its words stay: nothing is embedded again.
        assert_eq!(store.embed_missing(&MEANINGS).unwrap(), 0);
        assert_eq!(found(&mut store), [1], "a lesson never expires");
        // Made a note again, its month starts now.
        let note = store.set_kind(project, 1, Kind::Note).unwrap();
        assert!(!note.expired(seconds_since_epoch(SystemTime::now())));
        assert_eq!(found(&mut store), [1]);
        let missing = store.set_kind(project, 9, Kind::Gotcha).unwrap_err();
        assert_eq!(missing.to_string(), "there's no entry 9");
    }

    #[test]
    fn what_reads_as_status_is_told_by_its_words() {
        for status in [
            "Notifications PR #56 squash-merged into master as 8012b6c on 2026-10-04",
            "PRs #99–#104 all merged at da801ed but crystal binary not yet installed; run `make \
             install` to pick up all 13 items",
            "Backlog #81 tracks showing removal progress when another client requests it",
            "Flaky plugin test archived to backlog #135 during batch merge",
            "Only Claude Code naming implemented in this PR; other agents need their own",
            "The rebase onto master (dd40e97) had many overlapping conflicts",
            "Added an effort row; tests and README updated (uncommitted)",
            "All 14 items (PRs #81–#91) merged with CI green",
            "Fixed between 1234567 and abc1234def",
        ] {
            assert!(reads_as_status(status), "{status}");
        }
        for lesson in [
            "Merging a stack of squash-merged PRs here: after each squash, merge master into the \
             next PR's branch",
            "So no prefix, a 0.5 floor, and only matches near the best, merged with bm25 by RRF",
            "Merging conflicting PRs: have each merge master locally, wait for CI to pass, then \
             merge to master in sequence (used for #62-#67)",
            "Run the e2e tests with env -u CRYSTAL_AGENT_HOOKS (backlog #73)",
            "The skill's SHA-256 goes in SHIPPED: \
             3f2a9c0d1e4b5a6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c",
            "Its files are in src/tui/a1b2c3d/ and target/0123abc.d, its color #1e66f5a",
        ] {
            assert!(!reads_as_status(lesson), "{lesson}");
        }
    }
}
