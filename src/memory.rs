//! What a project's sessions have learned, kept for the sessions after
//! them: decisions made, gotchas hit, commands that work, notes, and how
//! tasks turned out.
//!
//! Every project's entries are kept in one SQLite database in crystal's
//! state directory, with a full-text index over them (SQLite's FTS5, ranked
//! by bm25), so a search finds an entry by any of its words, or a word they
//! start or stem from. Anyone can add to it: the user, an agent in a
//! session (`crystal remember`), a task as it ends, and the distiller
//! ([`crate::distill`]), from what a closed task did. The same thing said
//! again is the one entry seen again, not a second one, and what the user
//! forgot the distiller can't bring back.
//!
//! Every agent crystal starts is shown, as it starts, the entries that have
//! most to do with its launch: first those about files its worktree has
//! changed, then those about what it was asked. Claude Code searches the
//! rest through crystal's MCP server ([`crate::mcp`]); other agents with
//! crystal's commands.
//!
//! An entry can name the files it's about, and keeps a hash of each as it
//! was when the entry was said: its anchors. Once some of them change, the
//! entry is drifting, and may hold only in part; once all of them have, it's
//! stale, and agents aren't shown it as they start. A search marks both
//! where they rank.
//!
//! The user can turn all of it off: [`enabled`] is the one place that
//! decides, and everything memory adds asks it first.

use crate::config::Config;
use crate::embed::Embed;
use crate::git::Checkout;
use crate::printable;
use crate::secrets;
use crate::state;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many entries a session is shown when it starts. A few that matter
/// help; a long list is skimmed past.
const SHOWN_AT_LAUNCH: usize = 6;

/// The most of an entry's text a session is shown at launch.
const LAUNCH_TEXT_LENGTH: usize = 300;

/// The most bytes the entries a session is shown at launch take, their
/// lines together: docket's budget for its index of the same. The least
/// relevant are left out first.
const LAUNCH_BYTES: usize = 800;

/// The biggest file an anchor hashes. A file an entry is about is source,
/// far smaller; one bigger is data, not worth reading through each time.
const MAX_ANCHORED_BYTES: u64 = 8 * 1024 * 1024;

/// The most entries a search gives back.
pub const SEARCH_LIMIT: usize = 50;

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

/// bm25's weight for each indexed column, the text and the files: a word
/// in a file's name says more about what an entry is about than one in a
/// sentence.
const RANK: &str = "bm25(entries_fts, 1.0, 2.0)";

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

/// What makes the database as it is now, a step for each version: a
/// database at version `v`, kept in its `user_version`, takes the steps
/// after the first `v`.
const MIGRATIONS: &[fn(&Connection) -> Result<()>] = &[
    |conn| Ok(conn.execute_batch(TABLES)?),
    |conn| Ok(conn.execute_batch(VECTORS)?),
    add_anchors,
];

/// The columns [`entry_of`] reads, in its order.
const COLUMNS: &str = "e.id, e.kind, e.text, e.files, e.source, e.created, e.seen, e.last_seen, \
                       e.anchors, e.checkout";

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
}

fn once() -> u32 {
    1
}

/// An entry about to be added.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    /// A new entry.
    New(Entry),
    /// The project had it already: it was seen again, with any new files
    /// it named added to its own.
    Again(Entry),
    /// The user forgot it, and crystal can't add it back.
    Refused,
}

#[cfg(test)]
impl Added {
    pub fn entry(&self) -> Option<&Entry> {
        match self {
            Added::New(entry) | Added::Again(entry) => Some(entry),
            Added::Refused => None,
        }
    }
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
    /// it. Either way, it's anchored to its files as they are now.
    pub fn add(&mut self, project: &Path, new: New) -> Result<Added> {
        let text = clean(&secrets::redact(new.text.trim()));
        if text.trim().is_empty() {
            bail!("there's nothing to remember");
        }
        let key = key_of(&text);
        let name = self.ready(project)?;
        let now = seconds_since_epoch(SystemTime::now());
        let checkout = new.checkout.as_deref().unwrap_or(project);
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
            let files = joined(&said.files, &new.files);
            // Said again, it holds for its files as they are now.
            let anchors = anchors_in(checkout, &files);
            tx.execute(
                "UPDATE entries SET seen = seen + 1, last_seen = ?3, files = ?4, anchors = ?5, \
                 checkout = ?6 WHERE project = ?1 AND id = ?2",
                params![
                    name,
                    said.id,
                    now,
                    serde_json::to_string(&files)?,
                    serde_json::to_string(&anchors)?,
                    path_text(&new.checkout),
                ],
            )?;
            let entry = get(&tx, &name, said.id)?.context("the entry just seen is gone")?;
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
            text,
            anchors: anchors_in(checkout, &new.files),
            files: new.files,
            source: new.source,
            created: now,
            seen: 1,
            last_seen: now,
            checkout: new.checkout,
        };
        insert(&tx, &name, &entry)?;
        tx.commit()?;
        Ok(Added::New(entry))
    }

    /// Takes entry `id` out of `project`'s memory, and gives it back. What
    /// it said is kept only as a hash, for the distiller to know not to add
    /// it again.
    pub fn remove(&mut self, project: &Path, id: u64) -> Result<Entry> {
        let name = self.ready(project)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry = get(&tx, &name, id)?.with_context(|| format!("there's no entry {id}"))?;
        tx.execute(
            "DELETE FROM entries WHERE project = ?1 AND id = ?2",
            params![name, id],
        )?;
        let key = key_of(&entry.text);
        if !key.is_empty() {
            tx.execute(
                "INSERT OR IGNORE INTO forgotten (project, key) VALUES (?1, ?2)",
                params![name, hash(&key)],
            )?;
        }
        tx.commit()?;
        Ok(entry)
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

    /// The entries of `project` about any of `files`, said most recently
    /// first.
    fn about_files(&mut self, project: &Path, files: &[String]) -> Result<Vec<Entry>> {
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
        let about = about.query_map(params![name, files, SEARCH_LIMIT], entry_of)?;
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
    pub fn search(
        &mut self,
        project: &Path,
        text: &str,
        kind: Option<Kind>,
        limit: usize,
        embedder: Option<&dyn Embed>,
    ) -> Result<Vec<Entry>> {
        let name = self.ready(project)?;
        let kind = kind.map(|kind| kind.to_string());
        let Some(query) = fts_query(text) else {
            return self.newest(&name, kind.as_deref(), limit);
        };
        let pool = limit.max(POOL);
        let mut by_words = self.by_words(&name, &query, kind.as_deref(), pool)?;
        let Some(embedder) = embedder else {
            by_words.truncate(limit);
            return Ok(by_words);
        };
        match self.by_meaning(&name, text, kind.as_deref(), pool, embedder) {
            Ok(by_meaning) => Ok(reranked(
                fused(&[by_words, by_meaning], pool),
                text,
                embedder,
                limit,
            )),
            // The model failing leaves the search to the words.
            Err(err) => {
                eprintln!("crystal: couldn't search by meaning: {err:#}");
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
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Entry>> {
        let mut found = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM entries_fts JOIN entries e ON e.n = entries_fts.rowid \
             WHERE entries_fts MATCH ?2 AND e.project = ?1 AND (?3 IS NULL OR e.kind = ?3) \
             ORDER BY {RANK}, e.last_seen DESC, e.id DESC LIMIT ?4"
        ))?;
        let found = found.query_map(params![project, query, kind, limit], entry_of)?;
        Ok(found.collect::<rusqlite::Result<_>>()?)
    }

    /// The entries of the project called `project` that mean much the same
    /// as `text`, as alike as the model counts a match, the most alike
    /// first. Its entries with no vector yet get one first.
    fn by_meaning(
        &mut self,
        project: &str,
        text: &str,
        kind: Option<&str>,
        limit: usize,
        embedder: &dyn Embed,
    ) -> Result<Vec<Entry>> {
        self.embed_missing_in(Some(project), embedder)?;
        let asked = embedder.embed_query(text)?;
        let asked = &asked;
        let mut rows = self.conn.prepare(&format!(
            "SELECT {COLUMNS}, v.vector FROM entries e JOIN vectors v ON v.n = e.n \
             WHERE e.project = ?1 AND v.model = ?2 AND (?3 IS NULL OR e.kind = ?3)"
        ))?;
        let rows = rows.query_map(params![project, embedder.model(), kind], |row| {
            Ok((entry_of(row)?, row.get::<_, Vec<u8>>("vector")?))
        })?;
        let mut alike = Vec::new();
        for row in rows {
            let (entry, vector) = row?;
            let score = dot(asked, &vector_of(&vector));
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

    /// The entries of the project called `project`, of `kind` if it's
    /// given, said most recently first.
    fn newest(&self, project: &str, kind: Option<&str>, limit: usize) -> Result<Vec<Entry>> {
        let mut newest = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM entries e WHERE e.project = ?1 \
             AND (?2 IS NULL OR e.kind = ?2) ORDER BY e.last_seen DESC, e.id DESC LIMIT ?3"
        ))?;
        let newest = newest.query_map(params![project, kind, limit], entry_of)?;
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
        Ok(gone)
    }

    /// Gives every entry, in every project, that has no vector from
    /// `embedder` one, and says how many that was.
    pub fn embed_missing(&mut self, embedder: &dyn Embed) -> Result<usize> {
        self.embed_missing_in(None, embedder)
    }

    /// Gives the entries of the project called `project`, or of every
    /// project, that have no vector from `embedder` one.
    fn embed_missing_in(&mut self, project: Option<&str>, embedder: &dyn Embed) -> Result<usize> {
        let missing: Vec<(i64, String)> = {
            let mut missing = self.conn.prepare(
                "SELECT e.n, e.text FROM entries e \
                 LEFT JOIN vectors v ON v.n = e.n AND v.model = ?2 \
                 WHERE v.n IS NULL AND (?1 IS NULL OR e.project = ?1)",
            )?;
            let missing = missing.query_map(params![project, embedder.model()], |row| {
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
                // Only for the entry as it was read: it may have gone since.
                tx.execute(
                    "INSERT OR REPLACE INTO vectors (n, model, vector) \
                     SELECT ?1, ?2, ?3 WHERE EXISTS \
                     (SELECT 1 FROM entries WHERE n = ?1 AND text = ?4)",
                    params![n, embedder.model(), bytes_of(vector), text],
                )?;
            }
            tx.commit()?;
        }
        Ok(missing.len())
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
            for entry in entries {
                let mut entry = entry.clone();
                entry.seen = entry.seen.max(1);
                if entry.last_seen == 0 {
                    entry.last_seen = entry.created;
                }
                let said = entry.last_seen.max(entry.created);
                entry.anchors = anchors_from_before(&entry.files, said, project);
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
         last_seen, anchors, checkout) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
        ],
    )?;
    Ok(())
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
    })
}

/// `rankings` merged into one, by reciprocal rank fusion: an entry gets
/// `1 / (K + its place)` from each ranking it's in, so one high in both
/// comes before one first in only one. Between equals, the one said most
/// recently comes first.
fn fused(rankings: &[Vec<Entry>], limit: usize) -> Vec<Entry> {
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
/// out, in the order of the two rankings merged. Without a reranker, or
/// with it failing, `found` as it is.
fn reranked(mut found: Vec<Entry>, text: &str, embedder: &dyn Embed, limit: usize) -> Vec<Entry> {
    found.truncate(RERANK_POOL.max(limit));
    let read = &found[..found.len().min(RERANK_POOL)];
    let passages: Vec<&str> = read.iter().map(|entry| entry.text.as_str()).collect();
    let scores = match embedder.rerank(text, &passages) {
        Ok(Some(scores)) if scores.len() == read.len() => scores,
        Ok(Some(_)) => {
            eprintln!("crystal: the reranker didn't score every entry");
            found.truncate(limit);
            return found;
        }
        Ok(None) => {
            found.truncate(limit);
            return found;
        }
        Err(err) => {
            eprintln!("crystal: couldn't rerank: {err:#}");
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

/// Whether an entry still holds, by the files it's about. The fresher
/// sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Freshness {
    /// None of its files has changed since it was said, or it names none.
    Fresh,
    /// Some of its files have changed since: it may hold only in part.
    Drifting,
    /// Every one of its files has changed since, or gone: it may no
    /// longer hold at all.
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
pub struct Listed {
    pub entry: Entry,
    pub freshness: Freshness,
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
        let mut hashes = Hashes::default();
        self.entries
            .iter()
            .map(|entry| hashes.listed(entry.clone(), &self.project))
            .collect()
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
pub fn add(socket: &Path, project: &Path, new: New) -> Result<Added> {
    Store::open(socket)?.add(project, new)
}

/// Keeps how a task turned out, for the sessions after it: what the tasks
/// part of crystal calls when a task ends with something to say. With
/// memory off it keeps nothing, and says so with `None`.
pub fn record_outcome(
    config: &Config,
    socket: &Path,
    project: &Path,
    task: &str,
    summary: &str,
) -> Result<Option<Added>> {
    if !enabled(config) {
        return Ok(None);
    }
    let new = New {
        kind: Kind::Outcome,
        text: summary.to_string(),
        files: Vec::new(),
        source: Source::Task(task.to_string()),
        checkout: None,
    };
    Ok(Some(add(socket, project, new)?))
}

/// Takes the entry `id` out of `project`'s memory, and returns it.
pub fn remove(socket: &Path, project: &Path, id: u64) -> Result<Entry> {
    Store::open(socket)?.remove(project, id)
}

/// `entries` with whether each still holds, in the order they came: a
/// search's ranking stays as it is, the drifting and the stale marked where
/// they rank rather than moved below the rest.
pub fn marked(entries: Vec<Entry>, project: &Path) -> Vec<Listed> {
    let mut hashes = Hashes::default();
    entries
        .into_iter()
        .map(|entry| hashes.listed(entry, project))
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
/// newest, and how to search and add to it. Stale entries are left out,
/// and the drifting are marked where they rank. A task in the
/// background isn't told how to add, so it has nothing to be told when
/// nothing is remembered. With an `embedder`, what has to do with what it
/// was asked goes by meaning as well as by words.
pub fn for_launch(
    socket: &Path,
    project: &Path,
    asked: &str,
    changed: &[String],
    reader: Reader,
    embedder: Option<&dyn Embed>,
) -> Result<Option<String>> {
    let mut store = Store::open(socket)?;
    let mut hashes = Hashes::default();
    let about_changes = store.about_files(project, changed)?;
    let found = store.search(project, asked, None, SEARCH_LIMIT, embedder)?;
    let mut shown = launch_order(vec![about_changes, found], project, &mut hashes);
    if shown.is_empty() {
        shown = launch_order(vec![store.entries(project)?], project, &mut hashes);
    }
    Ok(launch_paragraph(&shown, reader))
}

/// The entries a session may be shown as it starts, from `parts`, the most
/// relevant part first: each once, none that's stale, and in each part in
/// the order it ranked, the drifting marked where they rank.
fn launch_order(parts: Vec<Vec<Entry>>, project: &Path, hashes: &mut Hashes) -> Vec<Listed> {
    let mut shown: Vec<Listed> = Vec::new();
    for part in parts {
        let part: Vec<Listed> = part
            .into_iter()
            .filter(|entry| !shown.iter().any(|item| item.entry.id == entry.id))
            .map(|entry| hashes.listed(entry, project))
            .filter(|item| item.freshness != Freshness::Stale)
            .collect();
        shown.extend(part);
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

/// One entry as a session is shown it: its kind, its text on one line and
/// not too long, the files it's about, and whether some have changed.
fn launch_line(item: &Listed) -> String {
    let entry = &item.entry;
    let mut text = one_line(&entry.text);
    if text.chars().count() > LAUNCH_TEXT_LENGTH {
        text = text.chars().take(LAUNCH_TEXT_LENGTH).collect();
        text.push('…');
    }
    let mut line = format!("({}) {text}", entry.kind);
    if !entry.files.is_empty() {
        line.push_str(&format!(" [{}]", entry.files.join(", ")));
    }
    if item.freshness == Freshness::Drifting {
        line.push_str(" [drifting: some of its files changed since]");
    }
    line
}

pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `entry` still holds, by its files as they are now.
pub fn freshness(entry: &Entry, project: &Path) -> Freshness {
    Hashes::default().freshness(entry, project)
}

/// The hash of each file looked at, read once however many entries are
/// about it.
#[derive(Default)]
struct Hashes(HashMap<PathBuf, Option<String>>);

impl Hashes {
    fn of(&mut self, path: PathBuf) -> Option<&String> {
        self.0
            .entry(path)
            .or_insert_with_key(|path| hash_of(path))
            .as_ref()
    }

    /// Whether `entry` still holds: its anchored files looked at in the
    /// worktree they were hashed in, while it's there, or else in
    /// `project`, and each that's changed or gone counted.
    fn freshness(&mut self, entry: &Entry, project: &Path) -> Freshness {
        let checkout = entry
            .checkout
            .as_deref()
            .filter(|checkout| checkout.is_dir())
            .unwrap_or(project);
        let changed = entry
            .anchors
            .iter()
            .filter(|(file, hash)| self.of(checkout.join(file)) != Some(*hash))
            .count();
        match changed {
            0 => Freshness::Fresh,
            all if all == entry.anchors.len() => Freshness::Stale,
            _ => Freshness::Drifting,
        }
    }

    fn listed(&mut self, entry: Entry, project: &Path) -> Listed {
        Listed {
            freshness: self.freshness(&entry, project),
            entry,
        }
    }
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
    fn a_task_s_outcome_is_kept_as_one() {
        let (_dir, socket) = socket();
        let config = Config::default();
        let added = record_outcome(&config, &socket, Path::new(APP), "fixer", "fixed the race")
            .unwrap()
            .unwrap();
        let Added::New(entry) = added else {
            panic!("{added:?}");
        };
        assert_eq!(entry.kind, Kind::Outcome);
        assert_eq!(entry.source, Source::Task("fixer".into()));
    }

    #[test]
    fn with_memory_off_an_outcome_is_not_kept() {
        let (_dir, socket) = socket();
        let project = Path::new(APP);
        let config = Config {
            plugins: [("memory".to_string(), false)].into(),
            ..Config::default()
        };
        let kept = record_outcome(&config, &socket, project, "fixer", "done").unwrap();
        assert_eq!(kept, None);
        assert!(Memory::read(&socket, project).unwrap().listed().is_empty());
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

    #[test]
    fn an_entry_drifts_as_some_of_its_files_change_and_goes_stale_once_all_have() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs", "b.rs"]);
        let at = project.path();
        add(&socket, at, about(&["a.rs", "b.rs"], at)).unwrap();
        let holds = || freshness(Memory::read(&socket, at).unwrap().get(1).unwrap(), at);
        assert_eq!(holds(), Freshness::Fresh);

        fs::write(at.join("a.rs"), "changed").unwrap();
        assert_eq!(holds(), Freshness::Drifting);
        fs::remove_file(at.join("b.rs")).unwrap();
        assert_eq!(holds(), Freshness::Stale, "gone counts as changed");

        // Said again, it holds for its files as they are now.
        fs::write(at.join("b.rs"), "back").unwrap();
        add(&socket, at, about(&["a.rs"], at)).unwrap();
        assert_eq!(holds(), Freshness::Fresh);
        // A file changed back is the file it was.
        fs::write(at.join("a.rs"), "changed again").unwrap();
        assert_eq!(holds(), Freshness::Drifting);
        fs::write(at.join("a.rs"), "changed").unwrap();
        assert_eq!(holds(), Freshness::Fresh);
    }

    #[test]
    fn a_file_that_isn_t_there_isn_t_anchored() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs"]);
        let at = project.path();
        let added = add(&socket, at, about(&["a.rs", "gone.rs"], at)).unwrap();
        let entry = added.entry().unwrap();
        assert_eq!(entry.anchors.keys().collect::<Vec<_>>(), ["a.rs"]);
        assert_eq!(freshness(entry, at), Freshness::Fresh);

        let nothing = add(
            &socket,
            at,
            New {
                text: "no file of it is there".into(),
                ..about(&["gone.rs"], at)
            },
        );
        assert_eq!(
            freshness(nothing.unwrap().entry().unwrap(), at),
            Freshness::Fresh
        );
    }

    #[test]
    fn files_are_looked_at_in_the_worktree_they_were_said_in_while_it_s_there() {
        let (_dir, socket) = socket();
        let project = project_with(&["a.rs"]);
        let worktree = tempfile::tempdir().unwrap();
        fs::write(worktree.path().join("a.rs"), "the worktree's own").unwrap();
        let added = add(&socket, project.path(), about(&["a.rs"], worktree.path())).unwrap();
        let entry = added.entry().unwrap().clone();
        assert_eq!(freshness(&entry, project.path()), Freshness::Fresh);

        // Once the worktree is gone, the project's file is the one.
        drop(worktree);
        assert_eq!(freshness(&entry, project.path()), Freshness::Stale);
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
        store.remove(project, 1).unwrap();
        assert_eq!(vectors(&store), 2);
        assert_eq!(store.forget_vectors_but("meanings").unwrap(), 0);
        assert_eq!(store.forget_vectors_but("another").unwrap(), 2);
        assert_eq!(vectors(&store), 0);

        // Another model's vectors can't be compared: it makes its own.
        let other = Meanings { model: "other" };
        assert_eq!(store.embed_missing(&other).unwrap(), 2);
        assert_eq!(store.embed_missing(&MEANINGS).unwrap(), 2);
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

    #[test]
    fn the_drifting_and_the_stale_are_marked_where_they_rank() {
        let project = project_with(&["a.rs", "b.rs"]);
        let anchored = |id, anchors: &[(&str, &str)]| Entry {
            anchors: anchors
                .iter()
                .map(|(file, hash)| (file.to_string(), hash.to_string()))
                .collect(),
            ..entry(id, Kind::Note, "ledger timeout")
        };
        let a = hash_of(&project.path().join("a.rs")).unwrap();
        let stale = anchored(1, &[("a.rs", "")]);
        let drifting = anchored(2, &[("a.rs", &a), ("b.rs", "")]);
        let fresh = anchored(3, &[("a.rs", &a)]);
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
        add(
            &socket,
            project.path(),
            about(&["refund.rs"], project.path()),
        )
        .unwrap();
        fs::write(project.path().join("refund.rs"), "changed").unwrap();
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
        };
        let short = |id| Listed {
            entry: entry(id, Kind::Note, "ledger"),
            freshness: Freshness::Fresh,
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
            [Freshness::Fresh, Freshness::Stale, Freshness::Fresh],
            "newest first"
        );
        // Anchored to what the file holds, it goes on from there.
        fs::write(project.path().join("a.rs"), "changed").unwrap();
        let holds = freshness_of(&memory, 1);
        assert_eq!(holds, Freshness::Stale);
    }

    fn freshness_of(memory: &Memory, id: u64) -> Freshness {
        freshness(memory.get(id).unwrap(), &memory.project)
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
            },
            Listed {
                entry: entry(1, Kind::Note, "Fees are in cents."),
                freshness: Freshness::Fresh,
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
}
