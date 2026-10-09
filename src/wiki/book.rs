//! `build.json`: the generator's own bookkeeping beside a wiki. What the
//! wiki was written from (its commit, its outline, the hash of every file
//! each subsection covers, which says what an update must write again),
//! the build under way or stopped before it ended, with everything written
//! so far (which a build carries on from), and the builds before.
//!
//! One build at a time writes a wiki: it holds `build.lock`, which the
//! system lets go of when the build ends however it ends, so a build that
//! crashed never keeps the next from starting.

use super::check::Tally;
use super::model::{Diagram, write_whole};
use super::plan::Plan;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// What `version` says of `build.json`'s shape.
pub const VERSION: u32 = 1;

/// How many builds the book remembers.
const HISTORY: usize = 50;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    pub version: u32,
    /// Whether a build is writing the wiki: as the build last wrote it
    /// down, which [`building`] tells for sure.
    #[serde(default)]
    pub building: bool,
    /// What the build under way is doing, or where one that stopped before
    /// it ended stopped: `writing 12/64 subsections`.
    #[serde(default)]
    pub progress: Option<String>,
    /// What `wiki.json` was written from.
    #[serde(default)]
    pub built: Option<Built>,
    /// The build under way, or stopped before it ended.
    #[serde(default)]
    pub run: Option<Run>,
    /// The builds that ended, the latest last.
    #[serde(default)]
    pub history: Vec<Done>,
}

/// What a wiki was written from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Built {
    pub commit: String,
    pub branch: String,
    /// When, in UTC.
    pub at: String,
    pub plan: Plan,
    /// The files each subsection covered, by its id, each with the hash of
    /// its content.
    pub blobs: BTreeMap<String, BTreeMap<String, String>>,
    /// The subsections that couldn't be written, which the next update
    /// writes.
    #[serde(default)]
    pub missing: BTreeSet<String>,
    /// What every build of it has cost, in US dollars.
    pub cost_usd: f64,
    /// The model that wrote the most of it, as Claude Code names it.
    pub model: String,
}

/// Whether a build writes all of a wiki, or what changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Build,
    Update,
}

/// What a build is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Planning,
    Writing,
    Summarizing,
    Overview,
    Finishing,
}

/// A build under way, or stopped before it ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub kind: Kind,
    pub commit: String,
    pub branch: String,
    /// When it started, in UTC.
    pub started: String,
    /// The process building, while one is.
    pub pid: u32,
    /// The outline: made by its planner, or the wiki's own.
    #[serde(default)]
    pub plan: Option<Plan>,
    /// Which subsections it writes; every one for a build.
    #[serde(default)]
    pub todo: BTreeSet<String>,
    /// Whether the outline changed from the wiki's, which has the overview
    /// written again.
    #[serde(default)]
    pub replanned: bool,
    /// The subsections written, by id.
    #[serde(default)]
    pub pages: BTreeMap<String, Page>,
    /// The subsections that couldn't be written, with why.
    #[serde(default)]
    pub failed: BTreeMap<String, String>,
    /// The sections' summaries written, by id.
    #[serde(default)]
    pub sections: BTreeMap<String, Summary>,
    #[serde(default)]
    pub overview: Option<Summary>,
    pub phase: Phase,
    /// What it's doing, in a few words: `writing 12/64 subsections`.
    pub progress: String,
    /// What it has cost so far, every run of Claude, in US dollars.
    pub cost_usd: f64,
    /// What checking and linking found and did.
    #[serde(default)]
    pub tally: Tally,
    /// The cost of each model that answered, by its name.
    #[serde(default)]
    pub models: BTreeMap<String, f64>,
}

/// A subsection written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub body_md: String,
    pub diagram: Option<Diagram>,
    /// The files it covered, each with its hash.
    pub blobs: BTreeMap<String, String>,
    /// For an update, whether what it says changed in meaning.
    pub meaning_changed: bool,
    pub cost_usd: f64,
}

/// A section's summary, or the overview, written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub summary_md: String,
    pub diagram: Option<Diagram>,
    pub meaning_changed: bool,
    pub cost_usd: f64,
}

/// A build that ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Done {
    pub kind: Kind,
    pub commit: String,
    pub started: String,
    pub ended: String,
    pub seconds: u64,
    pub cost_usd: f64,
    /// The subsections it wrote, of all of them.
    pub written: usize,
    pub subsections: usize,
    #[serde(default)]
    pub tally: Tally,
    /// Why it failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

impl Book {
    /// The book in `path`, or an empty one when there's none.
    pub fn read(path: &Path) -> Result<Book> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("couldn't read {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
            Err(err) => Err(err).with_context(|| format!("couldn't read {}", path.display())),
        }
    }

    /// Writes it to `path`, `progress` from its run.
    pub fn write(&self, path: &Path) -> Result<()> {
        let book = Book {
            version: VERSION,
            building: self.building && self.run.is_some(),
            progress: self.run.as_ref().map(|run| run.progress.clone()),
            ..self.clone()
        };
        write_whole(
            path,
            &serde_json::to_string_pretty(&book).expect("a book makes JSON"),
        )
    }

    /// Remembers `done`, forgetting the oldest past [`HISTORY`].
    pub fn remember(&mut self, done: Done) {
        self.history.push(done);
        let extra = self.history.len().saturating_sub(HISTORY);
        self.history.drain(..extra);
    }
}

/// The lock a build holds on a wiki's directory while it runs.
pub struct Lock {
    _file: File,
}

/// Takes the lock in `dir`, or fails saying a build is running there.
pub fn lock(dir: &Path) -> Result<Lock> {
    fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("build.lock"))
        .context("couldn't open the build's lock")?;
    // SAFETY: flock only reads the descriptor, which `file` keeps open.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("a build of this wiki is running already");
    }
    Ok(Lock { _file: file })
}

/// Whether a build holds the lock in `dir`.
pub fn building(dir: &Path) -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(dir.join("build.lock")) else {
        return false;
    };
    // SAFETY: as in `lock`; the lock taken here is let go of at once.
    unsafe {
        if libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 {
            return true;
        }
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
    false
}

/// Now, in UTC: `2026-10-09T12:00:00Z`.
pub fn utc_now() -> String {
    utc(now_seconds())
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `seconds` since the Unix epoch, in UTC.
pub fn utc(seconds: u64) -> String {
    let Ok(time) = libc::time_t::try_from(seconds) else {
        return format!("@{seconds}");
    };
    // SAFETY: an all-zero tm is a valid value for gmtime_r to fill in.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: gmtime_r reads `time` and writes only `tm`, both of which
    // live for the whole call.
    if unsafe { libc::gmtime_r(&time, &mut tm) }.is_null() {
        return format!("@{seconds}");
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The seconds since the Unix epoch a UTC time like [`utc`] writes stands
/// for.
pub fn seconds_of(text: &str) -> Option<u64> {
    let number = |at: std::ops::Range<usize>| -> Option<i32> { text.get(at)?.parse().ok() };
    // SAFETY: an all-zero tm is a valid value to fill in.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = number(0..4)? - 1900;
    tm.tm_mon = number(5..7)? - 1;
    tm.tm_mday = number(8..10)?;
    tm.tm_hour = number(11..13)?;
    tm.tm_min = number(14..16)?;
    tm.tm_sec = number(17..19)?;
    // SAFETY: timegm reads and normalizes only `tm`, which lives for the
    // whole call.
    let seconds = unsafe { libc::timegm(&mut tm) };
    u64::try_from(seconds).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_reads_back_as_it_was_written() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_791_547_200), "2026-10-09T12:00:00Z");
        assert_eq!(seconds_of("2026-10-09T12:00:00Z"), Some(1_791_547_200));
        assert_eq!(seconds_of("soon"), None);
    }

    #[test]
    fn one_build_at_a_time_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!building(dir.path()));
        let held = lock(dir.path()).unwrap();
        assert!(building(dir.path()));
        assert!(lock(dir.path()).is_err());
        drop(held);
        assert!(!building(dir.path()));
        assert!(lock(dir.path()).is_ok());
    }

    #[test]
    fn a_book_remembers_its_latest_builds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build.json");
        let mut book = Book::read(&path).unwrap();
        assert!(book.built.is_none());
        for n in 0..(HISTORY + 3) {
            book.remember(Done {
                kind: Kind::Update,
                commit: format!("c{n}"),
                started: utc(0),
                ended: utc(1),
                seconds: 1,
                cost_usd: 0.1,
                written: 1,
                subsections: 2,
                tally: Tally::default(),
                failed: None,
            });
        }
        book.write(&path).unwrap();
        let book = Book::read(&path).unwrap();
        assert_eq!(book.version, VERSION);
        assert_eq!(book.history.len(), HISTORY);
        assert_eq!(book.history[0].commit, "c3");
    }
}
