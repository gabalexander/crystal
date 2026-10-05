//! What Claude Code is shown of its project's memory as it reads or edits a
//! file: the few entries about that file it hasn't been shown yet, once per
//! file. A lesson about a file is worth most just as an agent is about to
//! work on it, where the few it's shown as it starts, or what it thinks to
//! search for, may not have it.
//!
//! Claude Code's `PreToolUse` hook, which crystal gives each one it starts
//! for the tools that read and edit files (see [`crate::agents`]), asks the
//! daemon, and gives what it answers as the hook's `additionalContext`, which
//! Claude reads beside what the tool gave it. The daemon keeps, for each
//! session, what it has been shown, the entries at launch and since, and the
//! files it was told about, so nothing is said twice; it goes with the
//! session through a handover, a move and a stop for being idle, and ends with
//! it. Finding them is [`memory::Store::about_file`]'s, quick and with no model,
//! as a hook holds the tool up while it runs; a daemon slow to answer is
//! given up on, and nothing is added.
//!
//! Being shown an entry doesn't count as finding it again, as being said
//! again or read in full does: crystal chose to show it, nobody asked for
//! it. Counted, a note about a file every session reads would never expire,
//! however little use it is; an agent it helps reads it in full with
//! `memory_show`, which counts.

use crate::memory::{self, Freshness, Listed};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The most entries a session is shown as it reads one file.
const MOST_SHOWN: usize = 3;

/// The most bytes what a session is shown as it reads one file takes, all
/// of it: a few lines, no more than a short paragraph each time.
const RECALL_BYTES: usize = 600;

/// The most of an entry's title a session is shown as it reads a file:
/// about a sentence, for three to fit.
const TITLE_CHARS: usize = 140;

/// The most of the prompt a session was last sent that's kept, to rank
/// what it's shown by.
const ASKED_BYTES: usize = 2000;

/// How long the hook waits for the daemon's answer before it lets the tool
/// run with nothing added.
pub const WAIT: Duration = Duration::from_millis(300);

/// What a session has been shown of its project's memory: the entries, by
/// id, as it started and as it read files since; the files it was told
/// about, by their paths as its agent gave them; and the prompt it was last
/// sent, which what it's shown is ranked by.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recalled {
    #[serde(default)]
    shown: BTreeSet<u64>,
    #[serde(default)]
    files: BTreeSet<PathBuf>,
    #[serde(default)]
    asked: Option<String>,
}

/// Where to look for the entries about a file a session's agent reads: the
/// project, its worktree's top and the file from there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub project: PathBuf,
    pub top: PathBuf,
    pub file: String,
}

impl Recalled {
    /// Notes the entries the session was shown as it started, by id.
    pub fn launched(&mut self, ids: &[u64]) {
        self.shown.extend(ids);
    }

    /// Notes the prompt the session was just sent.
    pub fn asked(&mut self, prompt: &str) {
        let mut end = prompt.len().min(ASKED_BYTES);
        while !prompt.is_char_boundary(end) {
            end -= 1;
        }
        self.asked = Some(prompt[..end].to_string());
    }

    /// What the session's entries are ranked by: `first`, what it was
    /// started to do, and the prompt it was last sent.
    pub fn query(&self, first: &str) -> String {
        match &self.asked {
            Some(asked) => format!("{first} {asked}"),
            None => first.to_string(),
        }
    }

    /// Where to look for what to tell the session about `file` as its agent
    /// reads or edits it, the first time it does, which is noted: `None`
    /// after that, and for a file in neither `top`, the session's worktree,
    /// nor `project`, its main one.
    pub fn first_look(&mut self, file: &Path, top: &Path, project: &Path) -> Option<Lookup> {
        let lookup = lookup(file, top, project)?;
        self.files.insert(file.to_path_buf()).then_some(lookup)
    }

    /// What the session is told as its agent reads `file`, called from the
    /// top of its worktree, of `found`, the entries about it, the first to
    /// show first: those it hasn't been shown, [`MOST_SHOWN`] at most, in
    /// [`RECALL_BYTES`], which are noted as shown. `None` when there's none.
    pub fn tell(&mut self, file: &str, found: &[Listed]) -> Option<String> {
        let unseen: Vec<&Listed> = (found.iter())
            .filter(|item| !self.shown.contains(&item.entry.id))
            .collect();
        let (shown, text) = told(file, &unseen)?;
        self.shown.extend(shown);
        Some(text)
    }

    /// Takes on what `before` was shown: the session's, ended to start
    /// again, its agent in the same conversation.
    pub fn carry_on(&mut self, before: Recalled) {
        self.shown.extend(before.shown);
        self.files.extend(before.files);
        if self.asked.is_none() {
            self.asked = before.asked;
        }
    }
}

/// Where to look for the entries about `file`: in `top`, the session's
/// worktree, or else in `project`, its main one, with the file's path from
/// there, each as it's given or with the links in it followed, as a Mac's
/// `/var` is `/private/var`. `None` for a file in neither, or no file at
/// all.
fn lookup(file: &Path, top: &Path, project: &Path) -> Option<Lookup> {
    let files = [file.to_path_buf(), real(file)];
    let dirs = [top, project].map(|dir| [dir.to_path_buf(), real(dir)]);
    dirs.iter().find_map(|dirs| {
        let (dir, from) = (dirs.iter()).find_map(|dir| {
            let from = files.iter().find_map(|file| file.strip_prefix(dir).ok())?;
            Some((dir, from.to_str()?))
        })?;
        (!from.is_empty()).then(|| Lookup {
            project: project.to_path_buf(),
            top: dir.clone(),
            file: from.to_string(),
        })
    })
}

/// `path` with the links in it followed: its directory's, for a file yet
/// to be written. As it is, when it can't be.
fn real(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    let real_dir = path.parent().and_then(|dir| dir.canonicalize().ok());
    match (real_dir, path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

/// The ids of those of `unseen` a session reading `file` is shown, and
/// what it's told of them: the first that fit, one too long for the room
/// left passed over for a shorter one after it. `None` when none does.
fn told(file: &str, unseen: &[&Listed]) -> Option<(Vec<u64>, String)> {
    let heading = format!("What this project's earlier sessions learned about {file}:");
    let ending = "The memory_show tool reads one in full.";
    let mut room = RECALL_BYTES.saturating_sub(heading.len() + 1 + ending.len());
    let mut shown = Vec::new();
    let mut text = heading;
    for item in unseen {
        if shown.len() == MOST_SHOWN {
            break;
        }
        let line = format!("\n- {} {}", item.entry.id, line(item));
        if line.len() <= room {
            room -= line.len();
            shown.push(item.entry.id);
            text.push_str(&line);
        }
    }
    if shown.is_empty() {
        return None;
    }
    text.push('\n');
    text.push_str(ending);
    Some((shown, text))
}

/// An entry as a session reading a file is shown it: its kind, its title,
/// not too long, and whether it may no longer hold in full.
fn line(item: &Listed) -> String {
    let entry = &item.entry;
    let mut title = memory::title(&entry.text);
    if title.chars().count() > TITLE_CHARS {
        title = title.chars().take(TITLE_CHARS).collect();
        title.push('…');
    }
    let mut line = format!("({}) {title}", entry.kind);
    if item.freshness == Freshness::Drifting {
        line.push_str(" [may be out of date]");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Entry, Kind, Source};

    fn listed(id: u64, kind: Kind, text: &str, freshness: Freshness) -> Listed {
        Listed {
            entry: Entry {
                id,
                kind,
                text: text.to_string(),
                files: vec!["src/ledger.rs".to_string()],
                source: Source::User,
                created: 0,
                seen: 1,
                last_seen: 0,
                anchors: Default::default(),
                checkout: None,
                names: Vec::new(),
                used: None,
                counted_from: None,
            },
            freshness,
            gone: Vec::new(),
        }
    }

    #[test]
    fn a_session_is_told_of_a_file_once_and_never_of_an_entry_twice() {
        let mut recalled = Recalled::default();
        recalled.launched(&[1]);
        let found = [
            listed(
                1,
                Kind::Gotcha,
                "the ledger tests need the database up",
                Freshness::Fresh,
            ),
            listed(
                2,
                Kind::Decision,
                "fees are kept in cents",
                Freshness::Fresh,
            ),
            listed(3, Kind::Command, "make db starts it", Freshness::Drifting),
        ];
        let (top, project) = (Path::new("/code/app.wt/fix"), Path::new("/code/app"));
        let file = Path::new("/code/app.wt/fix/src/ledger.rs");

        let lookup = recalled.first_look(file, top, project).unwrap();
        assert_eq!(
            lookup,
            Lookup {
                project: project.to_path_buf(),
                top: top.to_path_buf(),
                file: "src/ledger.rs".to_string(),
            }
        );
        assert_eq!(
            recalled.tell(&lookup.file, &found).unwrap(),
            "What this project's earlier sessions learned about src/ledger.rs:\n\
             - 2 (decision) fees are kept in cents\n\
             - 3 (command) make db starts it [may be out of date]\n\
             The memory_show tool reads one in full."
        );
        assert_eq!(recalled.first_look(file, top, project), None);
        // Another file about the same entries has nothing new to say.
        let other = Path::new("/code/app.wt/fix/src/fees.rs");
        let lookup = recalled.first_look(other, top, project).unwrap();
        assert_eq!(recalled.tell(&lookup.file, &found), None);
    }

    #[test]
    fn a_file_outside_the_project_is_never_looked_up() {
        let mut recalled = Recalled::default();
        let (top, project) = (Path::new("/code/app.wt/fix"), Path::new("/code/app"));
        assert_eq!(
            recalled.first_look(Path::new("/home/me/.claude/CLAUDE.md"), top, project),
            None
        );
        assert_eq!(recalled.first_look(top, top, project), None);
        // The main worktree's files are the project's too.
        let lookup = recalled.first_look(Path::new("/code/app/README.md"), top, project);
        assert_eq!(lookup.unwrap().top, project);
    }

    #[test]
    fn a_file_is_found_in_its_worktree_by_a_path_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("app");
        std::fs::create_dir_all(top.join("src")).unwrap();
        std::fs::write(top.join("src/ledger.rs"), "").unwrap();
        let link = dir.path().join("linked");
        std::os::unix::fs::symlink(&top, &link).unwrap();
        let mut recalled = Recalled::default();
        let lookup = recalled.first_look(&link.join("src/ledger.rs"), &top, &top);
        assert_eq!(lookup.unwrap().file, "src/ledger.rs");
        // One yet to be written, by its directory.
        let lookup = recalled.first_look(&link.join("src/fees.rs"), &top, &top);
        assert_eq!(lookup.unwrap().file, "src/fees.rs");
    }

    #[test]
    fn what_a_session_is_told_of_a_file_keeps_to_its_budget() {
        // Two bytes a letter: two take most of the room.
        let long = "é ".repeat(100);
        let found: Vec<Listed> = (1..=6)
            .map(|id| listed(id, Kind::Gotcha, &long, Freshness::Fresh))
            .chain([listed(7, Kind::Gotcha, "short", Freshness::Fresh)])
            .collect();
        let mut recalled = Recalled::default();
        let text = recalled.tell("src/ledger.rs", &found).unwrap();
        assert!(text.len() <= RECALL_BYTES, "{} bytes: {text}", text.len());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2 + MOST_SHOWN, "{text}");
        assert!(lines[1].ends_with('…'), "titles are cut: {text}");
        // One too long for the room left gives way to a shorter one.
        assert_eq!(lines[3], "- 7 (gotcha) short");
        // Those not shown are there for the next file.
        let text = recalled.tell("src/fees.rs", &found).unwrap();
        assert!(text.contains("\n- 3 (gotcha) é"), "{text}");
    }

    #[test]
    fn what_a_session_was_shown_goes_on_as_it_starts_again() {
        let mut before = Recalled::default();
        before.launched(&[1, 2]);
        before.asked("fix the ledger");
        let (top, project) = (Path::new("/code/app"), Path::new("/code/app"));
        before.first_look(Path::new("/code/app/a.rs"), top, project);
        let mut after = Recalled::default();
        after.launched(&[3]);
        after.carry_on(before);
        assert_eq!(after.shown, BTreeSet::from([1, 2, 3]));
        assert_eq!(
            after.first_look(Path::new("/code/app/a.rs"), top, project),
            None
        );
        assert_eq!(after.query("claude"), "claude fix the ledger");
    }

    #[test]
    fn a_long_prompt_is_kept_cut_at_a_character() {
        let mut recalled = Recalled::default();
        recalled.asked(&"é".repeat(ASKED_BYTES));
        let asked = recalled.asked.unwrap();
        assert!(asked.len() <= ASKED_BYTES);
        assert!(asked.chars().all(|c| c == 'é'));
    }
}
