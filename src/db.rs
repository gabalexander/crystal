//! The database the daemon and the TUI keep their state in: one SQLite file
//! in the state directory holding the sessions to start again after a
//! restart, the flow runs, each project's backlog and closed tasks, the
//! tasks waiting to start and the number the next task gets, what
//! background tasks have spent each day, the event log, and the TUI's tabs,
//! layouts and what the new-session panel remembers. The
//! settings stay in the config file, which people edit by hand, and memory
//! in a database of its own.
//!
//! Each write is a transaction, so one cut short by a crash or a power cut
//! leaves what was there before, never half of it. A database that can't be
//! read is an error, not a fresh start that would write over it.
//!
//! What crystal kept in JSON files before the database is brought in the
//! first time it's opened, and a project's backlog and tasks the first time
//! that project is asked for, since their directories are named by a hash
//! of the project's path. Each file is kept, renamed `.imported`, should
//! anyone want it; one that can't be read is renamed `.broken` instead.

use crate::backlog;
use crate::events::{Event, Scope, Since};
use crate::flow_run::FlowRun;
use crate::protocol::{
    ArchivedSession, Artifact, ArtifactKind, BacklogItem, PendingTask, TaskBrief, TaskOutcome,
    TaskRecord,
};
use crate::state::{self, SavedSession};
use crate::tasks;
use anyhow::{Context, Result};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, Row, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The tabs, under the name the TUI keeps them by.
pub const TABS: &str = "tabs";
/// What the new-session panel remembers between runs.
pub const LAUNCHER: &str = "launcher";
/// The layouts saved with `S`.
pub const LAYOUTS: &str = "layouts";
/// What the diff view keeps: the files marked reviewed, and whether it
/// lists them as a tree.
pub const DIFF: &str = "diff";
/// The latest event the user had seen when the TUI last knew they were
/// looking: where "while you were away" counts from.
pub const SEEN: &str = "seen";
/// How wide the sidebar was left.
pub const SIDEBAR: &str = "sidebar";
/// The projects folded in the sidebar, by their main worktrees.
pub const FOLDED: &str = "folded";
/// When the TUI last looked for a newer crystal.
pub const UPDATE: &str = "update";

/// How long a write waits for another to finish: the daemon and every TUI
/// share the one database.
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// The tables as they were first. A session's and a flow run's place in
/// their list is `position`, since the daemon keeps both lists in order and
/// writes each down whole. A project's row is its main worktree's path, with
/// the number its last backlog item got, which is never given again; that
/// it has a row says its files from before were brought in. Lists and
/// whole values, like a run's flow, are JSON. The TUI's state is a JSON
/// document under each name.
const TABLES: &str = "
CREATE TABLE sessions (
  position     INTEGER PRIMARY KEY,
  name         TEXT NOT NULL,
  command      TEXT NOT NULL,
  cwd          TEXT NOT NULL,
  conversation TEXT,
  task         TEXT,
  goal         TEXT
);
CREATE TABLE flow_runs (
  position INTEGER PRIMARY KEY,
  name     TEXT NOT NULL UNIQUE,
  flow     TEXT NOT NULL,
  profiles TEXT NOT NULL,
  goal     TEXT NOT NULL,
  cwd      TEXT NOT NULL,
  worktree TEXT,
  round    INTEGER NOT NULL,
  feedback TEXT,
  steps    TEXT NOT NULL,
  started  INTEGER NOT NULL
);
CREATE TABLE projects (
  path         TEXT PRIMARY KEY,
  last_backlog INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE backlog (
  project TEXT NOT NULL REFERENCES projects(path) ON DELETE CASCADE,
  number  INTEGER NOT NULL,
  text    TEXT NOT NULL,
  tags    TEXT NOT NULL DEFAULT '[]',
  done    INTEGER NOT NULL DEFAULT 0,
  created INTEGER NOT NULL,
  closed  INTEGER,
  PRIMARY KEY (project, number)
);
CREATE TABLE tasks (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  project      TEXT NOT NULL REFERENCES projects(path) ON DELETE CASCADE,
  project_name TEXT NOT NULL,
  goal         TEXT NOT NULL,
  session      TEXT NOT NULL,
  branch       TEXT,
  background   INTEGER NOT NULL DEFAULT 0,
  backlog      INTEGER,
  failed       INTEGER,
  summary      TEXT,
  closed       INTEGER
);
CREATE INDEX tasks_project ON tasks(project, id);
CREATE TABLE ui (
  name TEXT PRIMARY KEY,
  json TEXT NOT NULL
);
";

/// What tasks added: a closed task's number (`t12`), when it was made and
/// whether it was cancelled; the number the last task got, which is never
/// given again; the tasks waiting to start, each under its number, with how
/// it starts as JSON; and what background tasks spent, by the day on this
/// machine's clock.
const TASK_STATES: &str = "
ALTER TABLE tasks ADD COLUMN number INTEGER;
ALTER TABLE tasks ADD COLUMN created INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN cancelled INTEGER NOT NULL DEFAULT 0;
CREATE TABLE task_numbers (
  last INTEGER NOT NULL
);
INSERT INTO task_numbers (last) VALUES (0);
CREATE TABLE pending_tasks (
  number  INTEGER PRIMARY KEY,
  goal    TEXT NOT NULL,
  cwd     TEXT NOT NULL,
  name    TEXT,
  start   TEXT NOT NULL,
  backlog INTEGER,
  created INTEGER NOT NULL
);
CREATE TABLE spending (
  day TEXT PRIMARY KEY,
  usd REAL NOT NULL
);
";

/// The event log (see [`crate::event_log`]): each event under its `seq`,
/// which AUTOINCREMENT never gives again, not even once the events before
/// it are pruned; when it happened, what it was, the session (by id) and
/// project it's about, and the whole of it as JSON.
const EVENTS: &str = "
CREATE TABLE events (
  seq     INTEGER PRIMARY KEY AUTOINCREMENT,
  at      INTEGER NOT NULL,
  kind    TEXT NOT NULL,
  session TEXT,
  project TEXT,
  json    TEXT NOT NULL
);
CREATE INDEX events_at ON events(at);
";

/// The files kept with tasks as they closed: each under its task's number
/// and the name of its copy, which is unique in the task's directory; what
/// it is (`file` or `handoff`), where the copy is, how big, and when it was
/// kept. A task that closes again, a background task given a follow-up,
/// keeps its handoff file again in the same row.
const ARTIFACTS: &str = "
CREATE TABLE task_artifacts (
  task  INTEGER NOT NULL,
  name  TEXT NOT NULL,
  kind  TEXT NOT NULL,
  path  TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  kept  INTEGER NOT NULL,
  PRIMARY KEY (task, name)
);
";

/// The command an agent that says what it's doing itself said picks a
/// session up again after a restart, as JSON.
const RESUME: &str = "
ALTER TABLE sessions ADD COLUMN resume TEXT;
";

/// Whether a project is in the list of those crystal knows, which the
/// sidebar and the new-session panel offer with no session running there:
/// every project a session has run in, or that was added, until it's
/// taken off the list. Those with a row already had a backlog or tasks, so
/// they're on it.
const PROJECTS: &str = "
ALTER TABLE projects ADD COLUMN listed INTEGER NOT NULL DEFAULT 1;
";

/// The sessions in the archive, out of the list until they're started
/// again: each by the id it had, with what it takes to start it again as
/// the sessions table keeps it, the worktree it ran in as JSON, and when it
/// was archived.
const ARCHIVED: &str = "
CREATE TABLE archived (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  command      TEXT NOT NULL,
  cwd          TEXT NOT NULL,
  conversation TEXT,
  task         TEXT,
  goal         TEXT,
  resume       TEXT,
  worktree     TEXT,
  archived     INTEGER NOT NULL
);
CREATE INDEX archived_name ON archived(name, archived);
";

/// What a task carries beside its goal, as JSON (see
/// [`crate::protocol::TaskBrief`]): its acceptance criteria, and the pull
/// request and issue it's about. `NULL` for a task with none.
const BRIEFS: &str = "
ALTER TABLE tasks ADD COLUMN brief TEXT;
ALTER TABLE pending_tasks ADD COLUMN brief TEXT;
";

/// What makes the database as it is now, a step for each version: a
/// database at version `v`, kept in its `user_version`, takes the steps
/// after the first `v`.
const MIGRATIONS: &[&str] = &[
    TABLES,
    TASK_STATES,
    EVENTS,
    ARTIFACTS,
    RESUME,
    PROJECTS,
    ARCHIVED,
    BRIEFS,
];

/// The file each project kept its backlog in before the database.
const OLD_BACKLOG: &str = "backlog.json";

const SESSION_COLUMNS: &str = "name, command, cwd, conversation, task, goal, resume";
const RUN_COLUMNS: &str =
    "name, flow, profiles, goal, cwd, worktree, round, feedback, steps, started";
const TASK_COLUMNS: &str = "project_name, goal, session, branch, background, backlog, failed, \
                            summary, closed, number, created, cancelled, brief";
const PENDING_COLUMNS: &str = "number, goal, cwd, name, start, backlog, created, brief";
const ITEM_COLUMNS: &str = "number, text, tags, done, created, closed";
const ARTIFACT_COLUMNS: &str = "kind, name, path, bytes";

/// The database of the daemon at `socket`, open.
pub struct Db {
    conn: Connection,
    socket: PathBuf,
}

impl Db {
    /// The database of the daemon at `socket`, made if it isn't there, with
    /// the files kept before it brought in.
    pub fn open(socket: &Path) -> Result<Db> {
        let file = state::db_path(socket);
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
        }
        let mut conn =
            Connection::open(&file).with_context(|| format!("couldn't open {}", file.display()))?;
        conn.busy_timeout(BUSY_WAIT)?;
        // Readers then never wait on a writer, nor a writer on them, and a
        // commit is written once, to the log.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn).with_context(|| format!("couldn't set up {}", file.display()))?;
        let mut db = Db {
            conn,
            socket: socket.to_path_buf(),
        };
        db.bring_in_old();
        Ok(db)
    }

    /// The sessions written down, in their order. One that can't be read,
    /// written by another crystal say, is left out.
    pub fn sessions(&self) -> Result<Vec<SavedSession>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions ORDER BY position"
        ))?;
        let rows = statement.query_map([], |row| Ok(session_of(row)))?;
        readable(rows, "a session")
    }

    /// Writes `sessions` down in place of those that were.
    pub fn save_sessions(&mut self, sessions: &[SavedSession]) -> Result<()> {
        let tx = self.write()?;
        write_sessions(&tx, sessions)?;
        tx.commit()?;
        Ok(())
    }

    /// Keeps `archived` in the archive.
    pub fn archive(&self, archived: &ArchivedSession) -> Result<()> {
        let session = &archived.session;
        self.conn.execute(
            &format!(
                "INSERT OR REPLACE INTO archived (id, {SESSION_COLUMNS}, worktree, archived) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
            ),
            params![
                archived.id,
                session.name,
                json(&session.command)?,
                session.cwd.to_string_lossy(),
                json_or_null(&session.conversation)?,
                json_or_null(&session.task)?,
                json_or_null(&session.goal)?,
                json_or_null(&session.resume)?,
                json_or_null(&archived.worktree)?,
                archived.archived as i64,
            ],
        )?;
        Ok(())
    }

    /// The sessions in the archive, the latest archived first. One that
    /// can't be read is left out.
    pub fn archived(&self) -> Result<Vec<ArchivedSession>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS}, id, worktree, archived FROM archived \
             ORDER BY archived DESC, rowid DESC"
        ))?;
        let rows = statement.query_map([], |row| Ok(archived_of(row)))?;
        readable(rows, "an archived session")
    }

    /// Takes the session archived with the id `which`, or else under the
    /// name `which`, the latest archived when there are more, out of the
    /// archive, and gives it back.
    pub fn unarchive(&self, which: &str) -> Result<Option<ArchivedSession>> {
        let archived = self.archived()?;
        let found = match archived.iter().position(|a| a.id == which) {
            Some(at) => Some(archived[at].clone()),
            None => archived.into_iter().find(|a| a.name() == which),
        };
        if let Some(found) = &found {
            self.conn
                .execute("DELETE FROM archived WHERE id = ?1", params![found.id])?;
        }
        Ok(found)
    }

    /// The flow runs written down, the oldest first.
    pub fn flow_runs(&self) -> Result<Vec<FlowRun>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM flow_runs ORDER BY position"
        ))?;
        let rows = statement.query_map([], |row| Ok(run_of(row)))?;
        readable(rows, "a flow run")
    }

    /// Writes `runs` down in place of those that were.
    pub fn save_flow_runs(&mut self, runs: &[FlowRun]) -> Result<()> {
        let tx = self.write()?;
        write_runs(&tx, runs)?;
        tx.commit()?;
        Ok(())
    }

    /// The backlog of the project whose main worktree is `project`.
    pub fn backlog(&mut self, project: &Path) -> Result<backlog::Store> {
        let key = self.ready(project)?;
        read_backlog(&self.conn, &key)
    }

    /// Changes `project`'s backlog with `change`, and writes it back when
    /// that worked, all in one transaction.
    pub fn change_backlog<T>(
        &mut self,
        project: &Path,
        change: impl FnOnce(&mut backlog::Store) -> Result<T>,
    ) -> Result<T> {
        let key = self.ready(project)?;
        let tx = self.write()?;
        let mut store = read_backlog(&tx, &key)?;
        let changed = change(&mut store)?;
        write_backlog(&tx, &key, &store)?;
        tx.commit()?;
        Ok(changed)
    }

    /// Adds a task that has closed to `project`'s history.
    pub fn record_task(&mut self, project: &Path, task: &TaskRecord) -> Result<()> {
        let key = self.ready(project)?;
        insert_task(&self.conn, &key, task)
    }

    /// The closed tasks of `project`, or of every project, in the order
    /// they closed. Every project's takes in those of projects whose files
    /// haven't been brought in yet, read from the files.
    pub fn closed_tasks(&mut self, project: Option<&Path>) -> Result<Vec<TaskRecord>> {
        let mut found = match project {
            Some(project) => {
                let key = self.ready(project)?;
                let mut statement = self.conn.prepare(&format!(
                    "SELECT {TASK_COLUMNS} FROM tasks WHERE project = ?1 ORDER BY id"
                ))?;
                let rows = statement.query_map(params![key], |row| Ok(task_of(row)))?;
                readable(rows, "a closed task")?
            }
            None => {
                let mut statement = self
                    .conn
                    .prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY id"))?;
                let rows = statement.query_map([], |row| Ok(task_of(row)))?;
                readable(rows, "a closed task")?
            }
        };
        if project.is_none() {
            for dir in state::project_dirs(&self.socket) {
                found.extend(tasks::load_old(&dir));
            }
        }
        Ok(found)
    }

    /// The number for a new task, which is never given again.
    pub fn new_task_number(&mut self) -> Result<u64> {
        let tx = self.write()?;
        let number = next_task_number(&tx)?;
        tx.commit()?;
        Ok(number)
    }

    /// Keeps `task` until it's started, under a new number, which it gives
    /// back.
    pub fn add_pending_task(&mut self, task: &PendingTask) -> Result<u64> {
        let tx = self.write()?;
        let number = next_task_number(&tx)?;
        tx.execute(
            &format!(
                "INSERT INTO pending_tasks ({PENDING_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            ),
            params![
                number,
                task.goal,
                task.cwd.to_string_lossy(),
                task.name,
                json(&task.start)?,
                task.backlog,
                task.created,
                brief_json(&task.brief)?,
            ],
        )?;
        tx.commit()?;
        Ok(number)
    }

    /// The tasks waiting to start, the oldest first.
    pub fn pending_tasks(&self) -> Result<Vec<PendingTask>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {PENDING_COLUMNS} FROM pending_tasks ORDER BY number"
        ))?;
        let rows = statement.query_map([], |row| Ok(pending_of(row)))?;
        readable(rows, "a task waiting to start")
    }

    /// The task numbered `number`, if it's waiting to start.
    pub fn pending_task(&self, number: u64) -> Result<Option<PendingTask>> {
        let tasks = self.pending_tasks()?;
        Ok(tasks.into_iter().find(|task| task.id == number))
    }

    /// Forgets the task numbered `number`, which was waiting to start: it
    /// has, or it was cancelled. Says whether it was there.
    pub fn remove_pending_task(&self, number: u64) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM pending_tasks WHERE number = ?1",
            params![number],
        )?;
        Ok(removed > 0)
    }

    /// Keeps `artifact` with the task numbered `task`, at `kept`, in place
    /// of one of the same name.
    pub fn add_artifact(&self, task: u64, artifact: &Artifact, kept: u64) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT OR REPLACE INTO task_artifacts (task, {ARTIFACT_COLUMNS}, kept) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            ),
            params![
                task,
                artifact.kind.word(),
                artifact.name,
                artifact.path.to_string_lossy(),
                artifact.bytes,
                kept,
            ],
        )?;
        Ok(())
    }

    /// The files kept with the task numbered `task`, in the order they
    /// were kept.
    pub fn artifacts(&self, task: u64) -> Result<Vec<Artifact>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {ARTIFACT_COLUMNS} FROM task_artifacts WHERE task = ?1 ORDER BY kept, rowid"
        ))?;
        let rows = statement.query_map(params![task], |row| Ok(artifact_of(row)))?;
        readable(rows, "a kept file")
    }

    /// What background tasks spent on `day`, like `2026-10-03`.
    pub fn spent_on(&self, day: &str) -> Result<f64> {
        let mut statement = self
            .conn
            .prepare("SELECT usd FROM spending WHERE day = ?1")?;
        let mut rows = statement.query(params![day])?;
        Ok(match rows.next()? {
            Some(row) => row.get(0)?,
            None => 0.0,
        })
    }

    /// Adds `usd` to what background tasks spent on `day`.
    pub fn add_spending(&self, day: &str, usd: f64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO spending (day, usd) VALUES (?1, ?2) \
             ON CONFLICT (day) DO UPDATE SET usd = usd + excluded.usd",
            params![day, usd],
        )?;
        Ok(())
    }

    /// Writes `event` down, under the `seq` it has been given.
    pub fn add_event(&self, event: &Event) -> Result<()> {
        let session = event.session.as_ref().map(|session| &session.id);
        let project = event
            .project
            .as_ref()
            .map(|project| project.to_string_lossy());
        self.conn.execute(
            "INSERT INTO events (seq, at, kind, session, project, json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.seq,
                event.at,
                event.kind.name(),
                session,
                project,
                json(event)?
            ],
        )?;
        Ok(())
    }

    /// The `seq` of the latest event ever written down, pruned since or
    /// not: 0 before the first.
    pub fn latest_event(&self) -> Result<u64> {
        // AUTOINCREMENT keeps the highest it has seen in sqlite_sequence.
        Ok(self.conn.query_row(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'events'), 0)",
            [],
            |row| row.get(0),
        )?)
    }

    /// The events from `since` on, the oldest first. One that can't be
    /// read, written by a newer crystal say, is left out.
    pub fn events(&self, since: Since) -> Result<Vec<Event>> {
        let (from, value) = match since {
            Since::Seq(seq) => ("seq >", seq),
            Since::At(at) => ("at >=", at),
        };
        let mut statement = self.conn.prepare(&format!(
            "SELECT json FROM events WHERE {from} ?1 ORDER BY seq"
        ))?;
        let rows = statement.query_map(params![value], |row| {
            Ok(from_json::<Event>(&row.get::<_, String>(0)?))
        })?;
        readable(rows, "an event")
    }

    /// The newest `count` events `scope` takes from before the one numbered
    /// `before`, or from the end of the log without it, the newest first: a
    /// page of the TUI's timeline. What it takes is what
    /// [`Scope::matches`] does.
    pub fn events_before(
        &self,
        scope: &Scope,
        before: Option<u64>,
        count: usize,
    ) -> Result<Vec<Event>> {
        let (taken, about) = match scope {
            Scope::All => ("?3 IS NULL", Value::Null),
            Scope::Session(id) => (
                "(session = ?3 OR json_extract(json, '$.message.from_id') = ?3)",
                Value::Text(id.clone()),
            ),
            Scope::Task(id) => (
                "(json_extract(json, '$.task.id') = ?3 \
                 OR json_extract(json, '$.session.task_id') = ?3)",
                Value::Integer(i64::try_from(*id).unwrap_or(i64::MAX)),
            ),
            Scope::Project(path) => (
                "project = ?3",
                Value::Text(path.to_string_lossy().into_owned()),
            ),
        };
        let mut statement = self.conn.prepare(&format!(
            "SELECT json FROM events WHERE (?1 IS NULL OR seq < ?1) AND {taken} \
             ORDER BY seq DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![before, count, about], |row| {
            Ok(from_json::<Event>(&row.get::<_, String>(0)?))
        })?;
        readable(rows, "an event")
    }

    /// Takes the events from before `at`, in milliseconds since the Unix
    /// epoch, out of the log.
    pub fn delete_events_before(&self, at: u64) -> Result<()> {
        self.conn
            .execute("DELETE FROM events WHERE at < ?1", params![at])?;
        Ok(())
    }

    /// How many events the log holds.
    pub fn event_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?)
    }

    /// Takes every event out of the log but the newest `count`.
    pub fn keep_newest_events(&self, count: u64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM events WHERE seq <= \
             (SELECT seq FROM events ORDER BY seq DESC LIMIT 1 OFFSET ?1)",
            params![count],
        )?;
        Ok(())
    }

    /// The TUI's document called `name`, when it has kept one.
    pub fn ui(&self, name: &str) -> Result<Option<String>> {
        let mut statement = self.conn.prepare("SELECT json FROM ui WHERE name = ?1")?;
        let mut rows = statement.query(params![name])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }

    /// Keeps `value` as the TUI's document called `name`.
    pub fn keep_ui<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        put_ui(&self.conn, name, &serde_json::to_string(value)?)
    }

    /// A transaction that writes: it takes the database's one write lock
    /// at its start, so what it reads stays as it was until it commits.
    fn write(&mut self) -> Result<Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// The projects in the list of those crystal knows, by their main
    /// worktrees, in the order of their paths.
    pub fn listed_projects(&self) -> Result<Vec<PathBuf>> {
        let mut statement = self
            .conn
            .prepare("SELECT path FROM projects WHERE listed = 1 ORDER BY path")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<String>>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }

    /// Puts `project`, by its main worktree, in the list of those crystal
    /// knows, or takes it off with `listed` false. Its backlog and tasks
    /// stay either way.
    pub fn list_project(&mut self, project: &Path, listed: bool) -> Result<()> {
        let key = self.ready(project)?;
        self.conn.execute(
            "UPDATE projects SET listed = ?2 WHERE path = ?1",
            params![key, listed],
        )?;
        Ok(())
    }

    /// `project`'s key in the database, once it has its row there: the
    /// first time, with the backlog and closed tasks it kept in files
    /// before the database brought in.
    fn ready(&mut self, project: &Path) -> Result<String> {
        let key = project.to_string_lossy().into_owned();
        let known = |conn: &Connection| -> rusqlite::Result<bool> {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM projects WHERE path = ?1)",
                params![key],
                |row| row.get(0),
            )
        };
        if known(&self.conn)? {
            return Ok(key);
        }
        let dir = state::project_dir(&self.socket, project);
        let old_backlog = dir.join(OLD_BACKLOG);
        let old_tasks = dir.join(tasks::OLD_FILE);
        let tx = self.write()?;
        // Another process may have got here first.
        if known(&tx)? {
            return Ok(key);
        }
        tx.execute("INSERT INTO projects (path) VALUES (?1)", params![key])?;
        let backlog = match fs::read_to_string(&old_backlog) {
            Ok(text) => match serde_json::from_str::<backlog::Store>(&text) {
                Ok(store) => Some(store),
                Err(err) => {
                    set_aside(&old_backlog, "broken", &err.into());
                    None
                }
            },
            Err(_) => None,
        };
        if let Some(store) = &backlog {
            write_backlog(&tx, &key, store)?;
        }
        let closed = tasks::load_old(&dir);
        for task in &closed {
            insert_task(&tx, &key, task)?;
        }
        tx.commit()?;
        // Kept, should anyone want them, but never read again.
        if backlog.is_some() {
            let _ = fs::rename(&old_backlog, with_suffix(&old_backlog, "imported"));
        }
        if old_tasks.exists() {
            let _ = fs::rename(&old_tasks, with_suffix(&old_tasks, "imported"));
        }
        Ok(key)
    }

    /// Brings in what was kept in files before the database: the sessions,
    /// the flow runs and the TUI's documents.
    fn bring_in_old(&mut self) {
        let sessions = state::path(&self.socket);
        self.bring_in(&sessions, |tx, text| {
            let sessions: Vec<SavedSession> = serde_json::from_str(text)?;
            write_sessions(tx, &sessions)
        });
        let runs = state::flows_path(&self.socket);
        self.bring_in(&runs, |tx, text| {
            let runs: Vec<FlowRun> = serde_json::from_str(text)?;
            write_runs(tx, &runs)
        });
        for name in [TABS, LAUNCHER, LAYOUTS] {
            let file = sessions.with_file_name(format!("{name}.json"));
            self.bring_in(&file, |tx, text| {
                serde_json::from_str::<serde_json::Value>(text)?;
                put_ui(tx, name, text)
            });
        }
    }

    /// Brings in `file` with `put`, when it's there.
    fn bring_in(&mut self, file: &Path, put: impl FnOnce(&Transaction, &str) -> Result<()>) {
        if !file.exists() {
            return;
        }
        if let Err(err) = self.try_bring_in(file, put) {
            eprintln!("crystal: couldn't bring in {}: {err:#}", file.display());
        }
    }

    fn try_bring_in(
        &mut self,
        file: &Path,
        put: impl FnOnce(&Transaction, &str) -> Result<()>,
    ) -> Result<()> {
        let tx = self.write()?;
        // Read under the write lock: another process bringing it in first
        // has renamed it by the time this one has the lock.
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        if let Err(err) = put(&tx, &text) {
            drop(tx);
            set_aside(file, "broken", &err);
            return Ok(());
        }
        let imported = with_suffix(file, "imported");
        fs::rename(file, &imported)?;
        if let Err(err) = tx.commit() {
            let _ = fs::rename(&imported, file);
            return Err(err.into());
        }
        Ok(())
    }
}

/// How many sessions the daemon at `socket` has written down to start
/// again, for a server that isn't running: read only, since it's only
/// being looked at, so a database is neither made, brought up to date nor
/// given what was kept before it. No database yet is none.
pub fn saved_session_count(socket: &Path) -> Result<usize> {
    let file = state::db_path(socket);
    if !file.exists() {
        return Ok(0);
    }
    let conn = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("couldn't open {}", file.display()))?;
    let count = conn.query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))?;
    Ok(count)
}

/// Brings the database up to date: makes its tables, or adds what a newer
/// crystal keeps.
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
        tx.execute_batch(step)?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len()))?;
    tx.commit()?;
    Ok(())
}

fn write_sessions(conn: &Connection, sessions: &[SavedSession]) -> Result<()> {
    conn.execute("DELETE FROM sessions", [])?;
    for (position, session) in sessions.iter().enumerate() {
        conn.execute(
            &format!(
                "INSERT INTO sessions (position, {SESSION_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            ),
            params![
                position as i64,
                session.name,
                json(&session.command)?,
                session.cwd.to_string_lossy(),
                json_or_null(&session.conversation)?,
                json_or_null(&session.task)?,
                json_or_null(&session.goal)?,
                json_or_null(&session.resume)?,
            ],
        )?;
    }
    Ok(())
}

fn session_of(row: &Row) -> Result<SavedSession> {
    Ok(SavedSession {
        name: row.get(0)?,
        command: from_json(&row.get::<_, String>(1)?)?,
        cwd: PathBuf::from(row.get::<_, String>(2)?),
        conversation: from_json_or_null(row.get(3)?)?,
        task: from_json_or_null(row.get(4)?)?,
        goal: from_json_or_null(row.get(5)?)?,
        resume: from_json_or_null(row.get(6)?)?,
    })
}

fn archived_of(row: &Row) -> Result<ArchivedSession> {
    Ok(ArchivedSession {
        session: session_of(row)?,
        id: row.get(7)?,
        worktree: from_json_or_null(row.get(8)?)?,
        archived: row.get::<_, i64>(9)? as u64,
    })
}

fn write_runs(conn: &Connection, runs: &[FlowRun]) -> Result<()> {
    conn.execute("DELETE FROM flow_runs", [])?;
    for (position, run) in runs.iter().enumerate() {
        conn.execute(
            &format!(
                "INSERT INTO flow_runs (position, {RUN_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
            ),
            params![
                position as i64,
                run.name,
                json(&run.flow)?,
                json(&run.profiles)?,
                run.goal,
                run.cwd.to_string_lossy(),
                run.worktree.as_ref().map(|path| path.to_string_lossy()),
                run.round,
                run.feedback,
                json(&run.steps)?,
                run.started,
            ],
        )?;
    }
    Ok(())
}

/// A flow run as it was written down, without the environment, which never
/// is: the daemon gives it its own.
fn run_of(row: &Row) -> Result<FlowRun> {
    Ok(FlowRun {
        name: row.get(0)?,
        flow: from_json(&row.get::<_, String>(1)?)?,
        profiles: from_json(&row.get::<_, String>(2)?)?,
        goal: row.get(3)?,
        cwd: PathBuf::from(row.get::<_, String>(4)?),
        worktree: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
        round: row.get(6)?,
        feedback: row.get(7)?,
        steps: from_json(&row.get::<_, String>(8)?)?,
        started: row.get(9)?,
        env: Default::default(),
    })
}

fn read_backlog(conn: &Connection, project: &str) -> Result<backlog::Store> {
    let last: u64 = conn.query_row(
        "SELECT last_backlog FROM projects WHERE path = ?1",
        params![project],
        |row| row.get(0),
    )?;
    let mut statement = conn.prepare(&format!(
        "SELECT {ITEM_COLUMNS} FROM backlog WHERE project = ?1 ORDER BY number"
    ))?;
    let rows = statement.query_map(params![project], |row| Ok(item_of(row)))?;
    Ok(backlog::Store {
        next: last,
        items: readable(rows, "a backlog item")?,
    })
}

fn write_backlog(conn: &Connection, project: &str, store: &backlog::Store) -> Result<()> {
    conn.execute(
        "UPDATE projects SET last_backlog = ?2 WHERE path = ?1",
        params![project, store.next],
    )?;
    conn.execute("DELETE FROM backlog WHERE project = ?1", params![project])?;
    for item in &store.items {
        conn.execute(
            &format!(
                "INSERT INTO backlog (project, {ITEM_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
            ),
            params![
                project,
                item.number,
                item.text,
                json(&item.tags)?,
                item.done,
                item.created,
                item.closed,
            ],
        )?;
    }
    Ok(())
}

fn item_of(row: &Row) -> Result<BacklogItem> {
    Ok(BacklogItem {
        number: row.get(0)?,
        text: row.get(1)?,
        tags: from_json(&row.get::<_, String>(2)?)?,
        done: row.get(3)?,
        created: row.get(4)?,
        closed: row.get(5)?,
    })
}

fn insert_task(conn: &Connection, project: &str, task: &TaskRecord) -> Result<()> {
    let outcome = task.outcome.as_ref();
    conn.execute(
        &format!(
            "INSERT INTO tasks (project, {TASK_COLUMNS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
        ),
        params![
            project,
            task.project,
            task.goal,
            task.session,
            task.branch,
            task.background,
            task.backlog,
            outcome.map(|outcome| outcome.failed),
            outcome.map(|outcome| &outcome.summary),
            outcome.map(|outcome| outcome.closed),
            task.id,
            task.created,
            outcome.is_some_and(|outcome| outcome.cancelled),
            brief_json(&task.brief)?,
        ],
    )?;
    Ok(())
}

/// What a task carries beside its goal, as its row keeps it: `NULL` for
/// nothing.
fn brief_json(brief: &TaskBrief) -> Result<Option<String>> {
    (!brief.is_empty()).then(|| json(brief)).transpose()
}

/// A task from a project's history. It closed, or it's from a file before
/// the database that kept it open, so it was never pending or waiting.
fn task_of(row: &Row) -> Result<TaskRecord> {
    let closed: Option<u64> = row.get(8)?;
    let outcome = match closed {
        Some(closed) => Some(TaskOutcome {
            failed: row.get::<_, Option<bool>>(6)?.unwrap_or(false),
            cancelled: row.get(11)?,
            summary: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            closed,
        }),
        None => None,
    };
    Ok(TaskRecord {
        id: row.get(9)?,
        project: row.get(0)?,
        goal: row.get(1)?,
        session: row.get(2)?,
        branch: row.get(3)?,
        background: row.get(4)?,
        backlog: row.get(5)?,
        pending: false,
        waiting: false,
        created: row.get(10)?,
        outcome,
        artifacts: Vec::new(),
        brief: from_json_or_null(row.get(12)?)?.unwrap_or_default(),
    })
}

fn artifact_of(row: &Row) -> Result<Artifact> {
    let kind: String = row.get(0)?;
    Ok(Artifact {
        kind: ArtifactKind::named(&kind).with_context(|| format!("no kind of file is {kind}"))?,
        name: row.get(1)?,
        path: PathBuf::from(row.get::<_, String>(2)?),
        bytes: row.get(3)?,
    })
}

/// Takes the next task number, inside the transaction `tx` writes in.
fn next_task_number(tx: &Transaction) -> Result<u64> {
    tx.execute("UPDATE task_numbers SET last = last + 1", [])?;
    Ok(tx.query_row("SELECT last FROM task_numbers", [], |row| row.get(0))?)
}

fn pending_of(row: &Row) -> Result<PendingTask> {
    Ok(PendingTask {
        id: row.get(0)?,
        goal: row.get(1)?,
        cwd: PathBuf::from(row.get::<_, String>(2)?),
        name: row.get(3)?,
        start: from_json(&row.get::<_, String>(4)?)?,
        backlog: row.get(5)?,
        created: row.get(6)?,
        brief: from_json_or_null(row.get(7)?)?.unwrap_or_default(),
    })
}

fn put_ui(conn: &Connection, name: &str, json: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO ui (name, json) VALUES (?1, ?2) \
         ON CONFLICT (name) DO UPDATE SET json = excluded.json",
        params![name, json],
    )?;
    Ok(())
}

/// The rows that could be read. One that couldn't is told about and left
/// out: the rest are still worth having.
fn readable<T>(
    rows: impl Iterator<Item = rusqlite::Result<Result<T>>>,
    what: &str,
) -> Result<Vec<T>> {
    let mut found = Vec::new();
    for row in rows {
        match row? {
            Ok(value) => found.push(value),
            Err(err) => eprintln!("crystal: couldn't read {what}: {err:#}"),
        }
    }
    Ok(found)
}

/// Moves `file` aside, `backlog.json` to `backlog.json.broken`, and says
/// why: it's kept for the user, never read again, and never written over.
fn set_aside(file: &Path, suffix: &str, why: &anyhow::Error) {
    let aside = with_suffix(file, suffix);
    if fs::rename(file, &aside).is_ok() {
        eprintln!(
            "crystal: couldn't read {}, kept as {}: {why:#}",
            file.display(),
            aside.display()
        );
    }
}

fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn json_or_null<T: Serialize>(value: &Option<T>) -> Result<Option<String>> {
    value.as_ref().map(json).transpose()
}

fn from_json<T: DeserializeOwned>(text: &str) -> Result<T> {
    Ok(serde_json::from_str(text)?)
}

fn from_json_or_null<T: DeserializeOwned>(text: Option<String>) -> Result<Option<T>> {
    text.as_deref().map(from_json).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::{Flow, Step};
    use crate::protocol::{Conversation, TaskInfo, TaskSpec, TaskStart, TaskState};
    use std::collections::BTreeMap;

    /// A socket of a test's own, in `dir`, so its database is too.
    fn socket_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("crystal.sock")
    }

    fn saved(name: &str) -> SavedSession {
        SavedSession {
            name: name.into(),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/code/app"),
            conversation: Some(Conversation {
                id: "abc".into(),
                transcript: None,
                prompted: false,
            }),
            task: None,
            goal: None,
            resume: None,
        }
    }

    fn run(name: &str) -> FlowRun {
        let flow = Flow {
            name: "ship".into(),
            description: None,
            steps: vec![Step {
                name: "plan".into(),
                profile: None,
                prompt: "Plan {goal}".into(),
                placement: None,
                worktree: false,
                gate: true,
                back_to: None,
                max_rounds: None,
            }],
        };
        let env = BTreeMap::from([("TOKEN".to_string(), "secret".to_string())]);
        let mut run = FlowRun::new(
            name.into(),
            flow,
            &[],
            "add retries".into(),
            PathBuf::from("/code/app"),
            env,
            7,
        );
        run.start();
        run
    }

    fn closed(goal: &str, at: u64) -> TaskRecord {
        TaskRecord {
            id: Some(at),
            goal: goal.into(),
            session: "claude".into(),
            project: "app".into(),
            branch: Some("main".into()),
            background: false,
            backlog: Some(3),
            pending: false,
            waiting: false,
            created: 1,
            outcome: Some(TaskOutcome::new(TaskState::Done, "did it", at)),
            artifacts: Vec::new(),
            brief: Default::default(),
        }
    }

    fn pending(goal: &str) -> PendingTask {
        PendingTask {
            id: 0,
            goal: goal.into(),
            cwd: PathBuf::from("/code/app"),
            name: Some("later".into()),
            start: TaskStart::Background {
                args: vec!["--model".into(), "opus".into()],
            },
            backlog: Some(4),
            created: 9,
            brief: Default::default(),
        }
    }

    #[test]
    fn sessions_saved_load_back_the_same_in_their_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        assert!(db.sessions().unwrap().is_empty());
        let mut task = saved("b");
        task.task = Some(TaskSpec {
            prompt: "fix the tests".into(),
            args: vec!["--permission-mode".into(), "acceptEdits".into()],
        });
        task.goal = Some(TaskInfo {
            id: Some(5),
            goal: "fix the tests".into(),
            background: true,
            backlog: Some(2),
            waiting: true,
            created: 3,
            outcome: None,
            brief: Default::default(),
        });
        let mut reported = saved("c");
        reported.resume = Some(vec!["pi".into(), "--session".into(), "s1".into()]);
        let sessions = vec![reported, task, saved("a")];
        db.save_sessions(&sessions).unwrap();
        assert_eq!(db.sessions().unwrap(), sessions);

        // Saving again replaces them, and another process reads the same.
        db.save_sessions(&sessions[..1]).unwrap();
        let other = Db::open(&socket_in(&dir)).unwrap();
        assert_eq!(other.sessions().unwrap(), &sessions[..1]);
    }

    #[test]
    fn a_session_that_cant_be_read_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        db.save_sessions(&[saved("a"), saved("b")]).unwrap();
        db.conn
            .execute(
                "UPDATE sessions SET command = 'not json' WHERE name = 'a'",
                [],
            )
            .unwrap();
        assert_eq!(db.sessions().unwrap(), [saved("b")]);
    }

    #[test]
    fn flow_runs_are_written_down_without_their_environment() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        let runs = vec![run("ship-1"), run("ship-2")];
        db.save_flow_runs(&runs).unwrap();
        let kept: String = db
            .conn
            .query_row(
                "SELECT group_concat(steps || flow) FROM flow_runs",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!kept.contains("secret"));
        let loaded = db.flow_runs().unwrap();
        let without_env: Vec<FlowRun> = runs
            .into_iter()
            .map(|mut run| {
                run.env.clear();
                run
            })
            .collect();
        assert_eq!(loaded, without_env);
    }

    #[test]
    fn a_backlog_change_is_kept_and_one_that_fails_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        let app = Path::new("/code/app");
        let first = db
            .change_backlog(app, |store| {
                store.add("write the docs", vec!["docs".into()], 5)
            })
            .unwrap();
        let second = db
            .change_backlog(app, |store| store.add("fix the cart", Vec::new(), 6))
            .unwrap();
        assert_eq!((first, second), (1, 2));
        db.change_backlog(app, |store| store.mark(1, true, 9))
            .unwrap();
        db.change_backlog(app, |store| store.remove(2)).unwrap();
        assert!(
            db.change_backlog(app, |store| store.mark(7, true, 9))
                .is_err()
        );

        let store = Db::open(&socket_in(&dir)).unwrap().backlog(app).unwrap();
        assert_eq!(store.items.len(), 1);
        let item = &store.items[0];
        assert_eq!((item.number, item.done, item.closed), (1, true, Some(9)));
        assert_eq!(item.tags, ["docs"]);
        let third = db
            .change_backlog(app, |store| store.add("third", Vec::new(), 10))
            .unwrap();
        assert_eq!(third, 3, "a removed item's number isn't given again");
        assert!(
            db.backlog(Path::new("/code/other"))
                .unwrap()
                .items
                .is_empty()
        );
    }

    #[test]
    fn the_sessions_a_stopped_server_will_start_again_are_counted_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        assert_eq!(saved_session_count(&socket).unwrap(), 0);
        assert!(!state::db_path(&socket).exists());
        Db::open(&socket)
            .unwrap()
            .save_sessions(&[saved("a"), saved("b")])
            .unwrap();
        assert_eq!(saved_session_count(&socket).unwrap(), 2);
    }

    #[test]
    fn closed_tasks_come_back_by_project_in_the_order_they_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        let app = Path::new("/code/app");
        db.record_task(app, &closed("first", 1)).unwrap();
        db.record_task(Path::new("/code/other"), &closed("elsewhere", 2))
            .unwrap();
        let mut open = closed("open", 3);
        open.outcome = None;
        db.record_task(app, &open).unwrap();

        let mine = db.closed_tasks(Some(app)).unwrap();
        assert_eq!(mine, [closed("first", 1), open]);
        let goals: Vec<String> = db
            .closed_tasks(None)
            .unwrap()
            .into_iter()
            .map(|task| task.goal)
            .collect();
        assert_eq!(goals, ["first", "elsewhere", "open"]);
    }

    #[test]
    fn a_closed_task_keeps_its_number_and_whether_it_was_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        let app = Path::new("/code/app");
        let mut cancelled = closed("dropped", 7);
        cancelled.outcome = Some(TaskOutcome::new(TaskState::Cancelled, "no", 7));
        db.record_task(app, &cancelled).unwrap();
        let back = db.closed_tasks(Some(app)).unwrap();
        assert_eq!(back, [cancelled]);
        assert_eq!(back[0].state(), TaskState::Cancelled);
    }

    #[test]
    fn a_task_keeps_its_criteria_and_what_it_was_about_closed_or_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        let brief = crate::protocol::TaskBrief {
            accept: vec!["the tests pass".into()],
            issue: Some(Box::new(crate::protocol::ForgeLink {
                forge: crate::forge::Forge::GitHub,
                number: 7,
                title: "Login loops".into(),
                url: "https://github.com/o/r/issues/7".into(),
                branch: None,
            })),
            ..Default::default()
        };
        let app = Path::new("/code/app");
        let task = TaskRecord {
            brief: brief.clone(),
            ..closed("fix it", 4)
        };
        db.record_task(app, &task).unwrap();
        db.record_task(app, &closed("plain", 5)).unwrap();
        assert_eq!(
            db.closed_tasks(Some(app)).unwrap(),
            [task, closed("plain", 5)]
        );
        // One with none keeps nothing.
        let kept: Option<String> = db
            .conn
            .query_row("SELECT brief FROM tasks WHERE goal = 'plain'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(kept, None);

        let waiting = PendingTask {
            brief,
            ..pending("later")
        };
        let id = db.add_pending_task(&waiting).unwrap();
        assert_eq!(
            db.pending_task(id).unwrap().unwrap(),
            PendingTask { id, ..waiting }
        );
    }

    #[test]
    fn tasks_waiting_to_start_share_the_numbers_no_task_gets_twice() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        assert_eq!(db.new_task_number().unwrap(), 1);
        assert_eq!(db.add_pending_task(&pending("later")).unwrap(), 2);
        assert_eq!(db.add_pending_task(&pending("after")).unwrap(), 3);

        let other = Db::open(&socket_in(&dir)).unwrap();
        let waiting = other.pending_tasks().unwrap();
        let wanted = PendingTask {
            id: 2,
            ..pending("later")
        };
        assert_eq!(waiting[0], wanted);
        assert_eq!(waiting.len(), 2);
        assert_eq!(other.pending_task(3).unwrap().unwrap().goal, "after");
        assert!(other.remove_pending_task(2).unwrap());
        assert!(!other.remove_pending_task(2).unwrap());
        assert_eq!(db.pending_task(2).unwrap(), None);
        assert_eq!(db.new_task_number().unwrap(), 4);
    }

    #[test]
    fn a_tasks_kept_files_come_back_in_the_order_they_were_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&socket_in(&dir)).unwrap();
        assert!(db.artifacts(12).unwrap().is_empty());
        let artifact = |kind, name: &str, bytes| Artifact {
            kind,
            name: name.into(),
            path: PathBuf::from("/state/tasks/t12").join(name),
            bytes,
        };
        db.add_artifact(12, &artifact(ArtifactKind::File, "plan.md", 10), 1)
            .unwrap();
        db.add_artifact(12, &artifact(ArtifactKind::Handoff, "handoff.md", 20), 2)
            .unwrap();
        db.add_artifact(13, &artifact(ArtifactKind::File, "other.md", 30), 2)
            .unwrap();
        // Closing again keeps the handoff file again, in its place.
        db.add_artifact(12, &artifact(ArtifactKind::Handoff, "handoff.md", 25), 3)
            .unwrap();
        let kept = db.artifacts(12).unwrap();
        assert_eq!(
            kept,
            [
                artifact(ArtifactKind::File, "plan.md", 10),
                artifact(ArtifactKind::Handoff, "handoff.md", 25)
            ]
        );
    }

    #[test]
    fn spending_adds_up_by_the_day() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&socket_in(&dir)).unwrap();
        assert_eq!(db.spent_on("2026-10-03").unwrap(), 0.0);
        db.add_spending("2026-10-03", 1.25).unwrap();
        db.add_spending("2026-10-03", 0.5).unwrap();
        db.add_spending("2026-10-04", 2.0).unwrap();
        assert_eq!(db.spent_on("2026-10-03").unwrap(), 1.75);
        assert_eq!(db.spent_on("2026-10-04").unwrap(), 2.0);
    }

    #[test]
    fn a_database_from_before_tasks_had_numbers_takes_the_next_step() {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        {
            let conn = Connection::open(state::db_path(&socket)).unwrap();
            conn.execute_batch(TABLES).unwrap();
            conn.execute_batch("PRAGMA user_version = 1").unwrap();
            conn.execute("INSERT INTO projects (path) VALUES ('/code/app')", [])
                .unwrap();
            conn.execute(
                "INSERT INTO tasks (project, project_name, goal, session, failed, summary, closed) \
                 VALUES ('/code/app', 'app', 'old', 'claude', 1, 'no', 5)",
                [],
            )
            .unwrap();
        }
        let mut db = Db::open(&socket).unwrap();
        let old = db.closed_tasks(Some(Path::new("/code/app"))).unwrap();
        assert_eq!(old[0].id, None);
        assert_eq!(old[0].state(), TaskState::Failed);
        assert_eq!(db.new_task_number().unwrap(), 1);
    }

    #[test]
    fn an_archived_session_comes_back_the_latest_first_and_leaves_once_taken() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&socket_in(&dir)).unwrap();
        let archived = |id: &str, name: &str, at: u64| ArchivedSession {
            id: id.into(),
            session: saved(name),
            worktree: None,
            archived: at,
        };
        db.archive(&archived("1", "claude", 10)).unwrap();
        db.archive(&archived("2", "codex", 20)).unwrap();
        db.archive(&archived("3", "claude", 30)).unwrap();
        let ids: Vec<String> = db.archived().unwrap().into_iter().map(|a| a.id).collect();
        assert_eq!(ids, ["3", "2", "1"]);
        assert_eq!(
            db.unarchive("claude").unwrap(),
            Some(archived("3", "claude", 30))
        );
        assert_eq!(
            db.unarchive("claude").unwrap().map(|a| a.id),
            Some("1".into())
        );
        assert_eq!(db.unarchive("claude").unwrap(), None);
        assert_eq!(db.archived().unwrap().len(), 1);
        // By its id, whatever it's called.
        assert_eq!(db.unarchive("2").unwrap().map(|a| a.id), Some("2".into()));
        assert!(db.archived().unwrap().is_empty());
    }

    #[test]
    fn projects_stay_listed_until_taken_off_and_keep_their_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&socket_in(&dir)).unwrap();
        let (app, api) = (Path::new("/code/app"), Path::new("/code/api"));
        db.list_project(app, true).unwrap();
        db.list_project(api, true).unwrap();
        db.change_backlog(api, |store| store.add("tidy up", Vec::new(), 1))
            .unwrap();
        assert_eq!(db.listed_projects().unwrap(), vec![api, app]);
        db.list_project(api, false).unwrap();
        assert_eq!(db.listed_projects().unwrap(), vec![app]);
        assert_eq!(db.backlog(api).unwrap().items.len(), 1);
        db.list_project(api, true).unwrap();
        assert_eq!(db.listed_projects().unwrap(), vec![api, app]);
    }

    #[test]
    fn the_tuis_documents_are_kept_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&socket_in(&dir)).unwrap();
        assert_eq!(db.ui(TABS).unwrap(), None);
        db.keep_ui(TABS, &vec!["one"]).unwrap();
        db.keep_ui(TABS, &vec!["two"]).unwrap();
        db.keep_ui(LAYOUTS, &0).unwrap();
        assert_eq!(db.ui(TABS).unwrap().as_deref(), Some(r#"["two"]"#));
        assert_eq!(db.ui(LAYOUTS).unwrap().as_deref(), Some("0"));
    }

    #[test]
    fn the_files_from_before_are_brought_in_once_and_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        let sessions = state::path(&socket);
        let text = serde_json::to_string(&[saved("a"), saved("b")]).unwrap();
        fs::write(&sessions, text).unwrap();
        let runs = state::flows_path(&socket);
        fs::write(&runs, serde_json::to_string(&[run("ship-1")]).unwrap()).unwrap();
        let tabs = sessions.with_file_name("tabs.json");
        fs::write(&tabs, r#"{"version": 2, "tabs": []}"#).unwrap();
        let layouts = sessions.with_file_name("layouts.json");
        fs::write(&layouts, "not json").unwrap();

        let mut db = Db::open(&socket).unwrap();
        assert_eq!(db.sessions().unwrap(), [saved("a"), saved("b")]);
        assert_eq!(db.flow_runs().unwrap()[0].name, "ship-1");
        assert_eq!(
            db.ui(TABS).unwrap().as_deref(),
            Some(r#"{"version": 2, "tabs": []}"#)
        );
        assert!(!sessions.exists() && !runs.exists() && !tabs.exists());
        assert!(with_suffix(&sessions, "imported").exists());
        assert!(with_suffix(&tabs, "imported").exists());
        // What can't be read is kept for the user, and never read again.
        assert_eq!(db.ui(LAYOUTS).unwrap(), None);
        assert!(!layouts.exists());
        assert_eq!(
            fs::read_to_string(with_suffix(&layouts, "broken")).unwrap(),
            "not json"
        );

        // A database that has them doesn't take them again.
        db.save_sessions(&[saved("c")]).unwrap();
        drop(db);
        let db = Db::open(&socket).unwrap();
        assert_eq!(db.sessions().unwrap(), [saved("c")]);
    }

    #[test]
    fn a_projects_files_from_before_are_brought_in_when_its_first_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        let app = Path::new("/code/app");
        let old = state::project_dir(&socket, app);
        fs::create_dir_all(&old).unwrap();
        let mut store = backlog::Store::default();
        store.add("from before", Vec::new(), 1).unwrap();
        store.add("removed", Vec::new(), 2).unwrap();
        store.remove(2).unwrap();
        fs::write(
            old.join(OLD_BACKLOG),
            serde_json::to_string(&store).unwrap(),
        )
        .unwrap();
        let line = serde_json::to_string(&closed("before", 1)).unwrap();
        fs::write(old.join(tasks::OLD_FILE), format!("{line}\n")).unwrap();

        let mut db = Db::open(&socket).unwrap();
        // Every project's tasks take in those not brought in yet.
        assert_eq!(db.closed_tasks(None).unwrap(), [closed("before", 1)]);

        assert_eq!(db.backlog(app).unwrap(), store);
        assert!(!old.join(OLD_BACKLOG).exists());
        assert!(old.join("backlog.json.imported").exists());
        assert!(old.join("tasks.jsonl.imported").exists());
        let next = db
            .change_backlog(app, |store| store.add("new", Vec::new(), 3))
            .unwrap();
        assert_eq!(next, 3);
        db.record_task(app, &closed("after", 4)).unwrap();
        let goals: Vec<String> = db
            .closed_tasks(None)
            .unwrap()
            .into_iter()
            .map(|task| task.goal)
            .collect();
        assert_eq!(goals, ["before", "after"]);
    }

    #[test]
    fn a_broken_backlog_from_before_is_kept_aside_and_the_project_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        let app = Path::new("/code/app");
        let old = state::project_dir(&socket, app);
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join(OLD_BACKLOG), "{\"next\": 4, \"items\": [").unwrap();

        let mut db = Db::open(&socket).unwrap();
        assert!(db.backlog(app).unwrap().items.is_empty());
        assert!(old.join("backlog.json.broken").exists());
    }

    #[test]
    fn the_timeline_reads_the_log_back_a_page_at_a_time_the_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&socket_in(&dir)).unwrap();
        for seq in 1..=5 {
            let event = Event {
                seq,
                at: seq * 1000,
                ..Event::about_project(crate::events::Kind::BacklogAdded, "/code/app".into())
            };
            db.add_event(&event).unwrap();
        }
        let seqs = |events: Vec<Event>| -> Vec<u64> { events.iter().map(|e| e.seq).collect() };
        let all = Scope::All;
        assert_eq!(seqs(db.events_before(&all, None, 2).unwrap()), [5, 4]);
        assert_eq!(seqs(db.events_before(&all, Some(4), 2).unwrap()), [3, 2]);
        assert_eq!(seqs(db.events_before(&all, Some(2), 2).unwrap()), [1]);
    }

    #[test]
    fn a_timeline_of_one_session_task_or_project_reads_what_its_scope_takes() {
        use crate::events::{Kind, MessageAbout, SessionAbout};
        use crate::protocol::TaskRecord;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&socket_in(&dir)).unwrap();
        let about = |id: &str, task_id: Option<u64>| SessionAbout {
            name: id.into(),
            id: id.into(),
            command: Vec::new(),
            cwd: "/code/app".into(),
            project: None,
            worktree: None,
            branch: None,
            activity: None,
            task: None,
            task_id,
            status: String::new(),
            reporter: None,
        };
        let task = |id: u64| TaskRecord {
            id: Some(id),
            goal: "Fix it".into(),
            session: String::new(),
            project: "app".into(),
            branch: None,
            background: false,
            backlog: None,
            pending: true,
            waiting: false,
            created: 0,
            outcome: None,
            artifacts: Vec::new(),
            brief: Default::default(),
        };
        let in_app = |kind| Event::about_project(kind, "/code/app".into());
        let events = [
            // 1: a task made to start later, in the project.
            Event {
                task: Some(task(12)),
                ..in_app(Kind::TaskOpened)
            },
            // 2: its session working on it.
            Event {
                session: Some(about("s1", Some(12))),
                ..in_app(Kind::SessionWorking)
            },
            // 3: a message s1 sent s2.
            Event {
                session: Some(about("s2", None)),
                message: Some(MessageAbout {
                    from: Some("s1".into()),
                    from_id: Some("s1".into()),
                    line: "look".into(),
                }),
                ..in_app(Kind::SessionMessage)
            },
            // 4: another project's.
            Event::about_project(Kind::BacklogAdded, "/code/web".into()),
        ];
        for (seq, event) in (1..).zip(events) {
            db.add_event(&Event { seq, ..event }).unwrap();
        }
        let read = |scope: Scope| -> Vec<u64> {
            let page = db.events_before(&scope, None, 10).unwrap();
            assert!(page.iter().all(|event| scope.matches(event)));
            page.iter().map(|event| event.seq).collect()
        };
        assert_eq!(read(Scope::Session("s1".into())), [3, 2]);
        assert_eq!(read(Scope::Session("s2".into())), [3]);
        assert_eq!(read(Scope::Task(12)), [2, 1]);
        assert_eq!(read(Scope::Task(13)), Vec::<u64>::new());
        assert_eq!(read(Scope::Project("/code/web".into())), [4]);
        assert_eq!(read(Scope::All), [4, 3, 2, 1]);
    }

    #[test]
    fn a_database_that_cant_be_read_is_an_error_and_is_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        let file = state::db_path(&socket);
        fs::write(
            &file,
            "this is not a database, and is long enough to say so",
        )
        .unwrap();
        assert!(Db::open(&socket).is_err());
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "this is not a database, and is long enough to say so"
        );
    }
}
