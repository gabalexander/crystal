//! The handoff file: `.crystal/handoff.md` at the top of a worktree, what
//! the worktree remembers across the sessions that work in it. A plain
//! file, because every agent can read a file in its worktree already: all
//! it has to be told is where to look, which [`rule`] does as it starts.
//!
//! The daemon is the file's only writer, and each write is one section: a
//! heading with the local time, the session's name and, while it has one
//! open, its task, then the note, then a blank line. A session adds one
//! with `crystal handoff`, and a task closing adds its summary. One writer
//! is what keeps the file a list of dated sections that trims cleanly:
//! past [`MAX_FILE_BYTES`], the oldest go and [`TRIMMED`] stays on top.
//!
//! The notes stay out of git: the directory gets a `.gitignore` that
//! ignores everything in it but `flows.toml`, the project's own flows,
//! unless the config file says a project keeps its notes in git. A
//! `.gitignore` there already is left as it is.
//!
//! Everything the handoff file adds to crystal goes through [`enabled`].

use crate::config::Config;
use crate::git::Checkout;
use crate::plugins;
use crate::tasks::MAX_PROMPT_BYTES;
use anyhow::{Context, Result};
use std::fs;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The directory at the top of a worktree that crystal writes in.
pub const DIR: &str = ".crystal";

/// The handoff file, in [`DIR`].
pub const FILE: &str = "handoff.md";

/// The file never grows past this: the oldest notes go to make room.
pub const MAX_FILE_BYTES: usize = 256 * 1024;

/// One note is kept to this.
pub const MAX_NOTE_BYTES: usize = 8 * 1024;

/// How much of the file's end an agent starting is shown: enough that a
/// short file is all there, never so much that a long one crowds its prompt.
pub const EXCERPT_BYTES: usize = 2 * 1024;

/// What heads a file whose oldest notes have gone.
pub const TRIMMED: &str = "[earlier notes trimmed]";

/// The `.gitignore` beside the notes: everything there but the project's
/// flows, which are meant to be committed.
const GITIGNORE: &str = "*\n!flows.toml\n";

/// The longest a task's goal is in a heading.
const TITLE_CHARS: usize = 80;

/// Whether the handoff file is on: the `handoff` plugin.
pub fn enabled(config: &Config) -> bool {
    plugins::enabled(config, "handoff")
}

/// Refuses a command that's only about the handoff file while it's off.
pub fn ensure_enabled(config: &Config) -> Result<()> {
    plugins::ensure_enabled(config, "handoff")
}

/// The top of the git worktree `dir` is in, where its handoff file goes.
/// Outside git there's none: notes are a worktree's.
pub fn worktree_of(dir: &Path) -> Option<PathBuf> {
    Checkout::find(dir).map(|checkout| checkout.worktree().path)
}

/// The handoff file of the worktree at `worktree`.
pub fn path(worktree: &Path) -> PathBuf {
    worktree.join(DIR).join(FILE)
}

/// A section's heading: when, the session, and the task it has open with
/// `outcome`, how it just closed, for the section a closing task adds.
pub fn heading(when: &str, session: &str, task: Option<&str>, outcome: Option<&str>) -> String {
    let mut line = format!("## {when} · {session}");
    if let Some(task) = task {
        line.push_str(&format!(" · task \"{}\"", title(task)));
        if let Some(outcome) = outcome {
            line.push_str(&format!(" {outcome}"));
        }
    }
    line
}

/// A section as it goes in the file: its heading, the note, a blank line.
pub fn section(heading: &str, note: &str) -> String {
    format!("{heading}\n{note}\n\n")
}

/// A note as it's kept: control characters out, runs of spaces one space,
/// its lines kept but never two blank ones together, and cut to
/// [`MAX_NOTE_BYTES`]. `None` when nothing is left.
pub fn tidy(note: &str) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in note.lines() {
        let line: String = line
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() && lines.last().is_none_or(String::is_empty) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let note = lines.join("\n");
    (!note.is_empty()).then(|| cut(&note, MAX_NOTE_BYTES))
}

/// The file after `section` is added to `existing`, kept within `cap`
/// bytes: past it, whole sections go from the top, the oldest first, and
/// [`TRIMMED`] heads what's left. The new section always stays.
pub fn appended(existing: &str, section: &str, cap: usize) -> String {
    let (trimmed_before, body) = match existing.strip_prefix(TRIMMED) {
        Some(rest) => (true, rest.trim_start_matches('\n')),
        None => (false, existing),
    };
    let mut body = body.to_string();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    let whole = format!("{body}{section}");
    let marker = format!("{TRIMMED}\n\n");
    let head = if trimmed_before { marker.as_str() } else { "" };
    if head.len() + whole.len() <= cap {
        return format!("{head}{whole}");
    }
    // Where each section starts, the new one's included: the file keeps
    // the sections from the first start that leaves room for the marker.
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    for line in whole.split_inclusive('\n') {
        if at > 0 && is_heading(line) {
            starts.push(at);
        }
        at += line.len();
    }
    starts.push(body.len());
    let keep_from = starts
        .into_iter()
        .filter(|&start| start <= body.len())
        .find(|&start| marker.len() + whole.len() - start <= cap)
        .unwrap_or(body.len());
    format!("{marker}{}", &whole[keep_from..])
}

/// Adds `section` to the handoff file of the worktree at `worktree`,
/// making the file and its directory the first time, with the
/// `.gitignore` unless the notes are to be kept in git (`in_git`). Written
/// whole to another file, then moved over the old one, so nobody reads
/// half of it. The caller makes sure only one write happens at a time.
pub fn append(worktree: &Path, section: &str, in_git: bool) -> Result<PathBuf> {
    let dir = worktree.join(DIR);
    fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let ignore = dir.join(".gitignore");
    if !in_git && !ignore.exists() {
        fs::write(&ignore, GITIGNORE)
            .with_context(|| format!("couldn't write {}", ignore.display()))?;
    }
    let file = dir.join(FILE);
    let existing = match fs::read(&file) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("couldn't read {}", file.display())),
    };
    let text = appended(&existing, section, MAX_FILE_BYTES);
    let unfinished = dir.join(format!(".{FILE}.saving"));
    fs::write(&unfinished, text)
        .with_context(|| format!("couldn't write {}", unfinished.display()))?;
    fs::rename(&unfinished, &file).with_context(|| format!("couldn't write {}", file.display()))?;
    Ok(file)
}

/// What an agent starting in a worktree is shown of its handoff file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notes {
    pub path: PathBuf,
    /// The file's last [`EXCERPT_BYTES`], from the start of a line.
    pub excerpt: String,
    /// Whether that's the whole file.
    pub whole: bool,
}

/// The notes of the worktree at `worktree`, or `None` when it has no
/// handoff file or an empty one. Reads no more than the file's end.
pub fn read(worktree: &Path) -> Option<Notes> {
    let path = path(worktree);
    let mut file = fs::File::open(&path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(EXCERPT_BYTES as u64);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let whole = start == 0;
    if !whole {
        // Cut in the middle of a line: start at the next one.
        text = match text.find('\n') {
            Some(at) => text[at + 1..].to_string(),
            None => String::new(),
        };
    }
    let excerpt = text.trim().to_string();
    if whole && excerpt.is_empty() {
        return None;
    }
    Some(Notes {
        path,
        excerpt,
        whole,
    })
}

/// What an agent starting in a worktree with notes is told: where they
/// are, to read them first, and how to add to them; with `inline`, the end
/// of the file too, so a short one is simply there.
pub fn rule(notes: &Notes, inline: bool) -> String {
    let mut text = format!(
        "This worktree has notes left by the sessions before you in .crystal/handoff.md \
         ({}). Read that file before you start: it's what they learned here, the decisions, \
         the dead ends, the commands that work and what was left undone. When you learn \
         something the next session in this worktree should know, add it by running \
         `crystal handoff \"<note>\"`. Keep each note short and whole on its own; crystal adds \
         the time and this session's name. Don't edit the file yourself: crystal writes it, \
         and lets the oldest notes go as it grows.",
        notes.path.display()
    );
    if inline && !notes.excerpt.is_empty() {
        let lead = if notes.whole {
            "The file as it is now:"
        } else {
            "Its latest notes (the file has the rest):"
        };
        text.push_str(&format!("\n\n{lead}\n\n{}", notes.excerpt));
    }
    text
}

/// The rule for an agent starting in the worktree at `worktree`, when it
/// has notes. `said` is how much else its prompt holds: the file's end
/// comes along only while the prompt stays within [`MAX_PROMPT_BYTES`],
/// and the rule is the pointer alone past that.
pub fn launch_note(worktree: &Path, said: usize) -> Option<String> {
    read(worktree).map(|notes| fitting(&notes, said))
}

fn fitting(notes: &Notes, said: usize) -> String {
    let inline = rule(notes, true);
    if said + inline.len() <= MAX_PROMPT_BYTES {
        inline
    } else {
        rule(notes, false)
    }
}

/// Whether the config says the project whose main worktree is `project`
/// keeps its notes in git.
pub fn in_git(config: &Config, project: &Path) -> bool {
    let real = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let same = |listed: &PathBuf| real(&expand_home(listed)) == real(project);
    config.handoff.in_git.iter().any(same)
}

/// Now, the way a heading says it: local time with its offset from UTC,
/// like `2026-09-25T14:03:07+02:00`.
pub fn now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    local_time(seconds)
}

/// `seconds` since the Unix epoch, as [`now`] says it: the local time is
/// for the person who reads the file.
fn local_time(seconds: u64) -> String {
    let Ok(time) = libc::time_t::try_from(seconds) else {
        return format!("@{seconds}");
    };
    // SAFETY: an all-zero tm is a valid value for localtime_r to fill in.
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r reads `time` and writes only `local`, both of
    // which live for the whole call.
    if unsafe { libc::localtime_r(&time, &mut local) }.is_null() {
        return format!("@{seconds}");
    }
    let offset = local.tm_gmtoff;
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec,
        offset / 3600,
        offset % 3600 / 60,
    )
}

/// Whether `line` heads a section crystal wrote: `## ` and a year. A note
/// with a `## Plan` line of its own doesn't count, so a trim never cuts a
/// note in half.
fn is_heading(line: &str) -> bool {
    line.strip_prefix("## ").is_some_and(|rest| {
        let bytes = rest.as_bytes();
        bytes.len() > 5 && bytes[..4].iter().all(u8::is_ascii_digit) && bytes[4] == b'-'
    })
}

/// A task's goal as a heading shows it: its first line, cut short.
fn title(goal: &str) -> String {
    let line = goal.lines().map(str::trim).find(|line| !line.is_empty());
    let line = line.unwrap_or("");
    match line.char_indices().nth(TITLE_CHARS) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_string(),
    }
}

/// `text` cut to at most `max` bytes, at a character, with `…` where it
/// was cut.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// `path` with a leading `~` made the home directory.
fn expand_home(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(rest)
        }
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(second: usize) -> String {
        let heading = heading(
            &format!("2026-09-25T00:00:{second:02}+00:00"),
            "a",
            None,
            None,
        );
        section(&heading, &"x".repeat(40))
    }

    #[test]
    fn a_section_is_its_heading_the_note_and_a_blank_line() {
        let plain = heading("2026-09-25T14:03:07+02:00", "fixer", None, None);
        assert_eq!(plain, "## 2026-09-25T14:03:07+02:00 · fixer");
        assert_eq!(
            section(&plain, "found it"),
            format!("{plain}\nfound it\n\n")
        );
        let closed = heading(
            "2026-09-25T14:03:07+02:00",
            "fixer",
            Some("Fix the login\nall of it"),
            Some("done"),
        );
        assert_eq!(
            closed,
            "## 2026-09-25T14:03:07+02:00 · fixer · task \"Fix the login\" done"
        );
        assert!(is_heading(&plain));
        assert!(!is_heading("## Plan"), "a note's own heading");
        assert!(!is_heading("# 2026-09-25"));
    }

    #[test]
    fn the_time_is_local_with_its_offset() {
        let stamp = local_time(1_790_000_000);
        // 2026-09-21 in every time zone there is.
        assert!(stamp.starts_with("2026-09-2"), "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'T', "{stamp}");
        assert!(matches!(stamp.as_bytes()[19], b'+' | b'-'), "{stamp}");
        assert_eq!(stamp.len(), "2026-09-21T12:53:20+00:00".len(), "{stamp}");
        assert!(is_heading(&heading(&stamp, "s", None, None)));
    }

    #[test]
    fn a_note_is_tidied_and_cut() {
        assert_eq!(
            tidy("  found\tit  \n\n\n\nthe  fix\u{7}\n\n").as_deref(),
            Some("found it\n\nthe fix")
        );
        assert_eq!(tidy(" \n\t\n"), None);
        let long = tidy(&"é".repeat(MAX_NOTE_BYTES)).unwrap();
        assert!(long.len() <= MAX_NOTE_BYTES && long.ends_with('…'));
    }

    #[test]
    fn the_file_lets_its_oldest_sections_go_at_the_cap() {
        let one = appended("", &note(1), 1000);
        assert_eq!(one, note(1));
        assert_eq!(
            appended(&one, &note(2), 1000),
            format!("{}{}", note(1), note(2))
        );

        let cap = note(1).len() * 3;
        let mut file = String::new();
        for second in 1..=5 {
            file = appended(&file, &note(second), cap);
            assert!(file.len() <= cap, "{second}: {} > {cap}", file.len());
        }
        assert!(file.starts_with(&format!("{TRIMMED}\n\n")), "{file}");
        assert!(file.ends_with(&note(5)), "the newest stays");
        assert!(!file.contains(":01+"), "the oldest went");
        assert_eq!(file.matches(TRIMMED).count(), 1);
        // Whole sections only: every heading still has its note.
        for line in file.lines().filter(|line| is_heading(line)) {
            assert!(file.contains(&format!("{line}\n{}", "x".repeat(40))));
        }
        // A file made one long blob by hand still makes room for the note.
        let blob = "y".repeat(cap * 2);
        let fixed = appended(&blob, &note(9), cap);
        assert!(fixed.len() <= cap && fixed.ends_with(&note(9)));
    }

    #[test]
    fn the_directory_is_kept_out_of_git_unless_the_notes_go_in_it() {
        let worktree = tempfile::tempdir().unwrap();
        let file = append(
            worktree.path(),
            &section("## 2026-09-25T00:00:00+00:00 · a", "x"),
            false,
        )
        .unwrap();
        assert_eq!(file, path(worktree.path()));
        let ignore = worktree.path().join(".crystal/.gitignore");
        assert_eq!(fs::read_to_string(&ignore).unwrap(), "*\n!flows.toml\n");
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "## 2026-09-25T00:00:00+00:00 · a\nx\n\n"
        );
        // A .gitignore of the user's own is left as it is.
        fs::write(&ignore, "handoff.md\n").unwrap();
        append(worktree.path(), &note(1), false).unwrap();
        assert_eq!(fs::read_to_string(&ignore).unwrap(), "handoff.md\n");

        let kept = tempfile::tempdir().unwrap();
        append(kept.path(), &note(1), true).unwrap();
        assert!(path(kept.path()).is_file());
        assert!(!kept.path().join(".crystal/.gitignore").exists());
    }

    #[test]
    fn notes_are_the_files_end_and_none_without_any() {
        let worktree = tempfile::tempdir().unwrap();
        assert_eq!(read(worktree.path()), None);
        fs::create_dir(worktree.path().join(DIR)).unwrap();
        fs::write(path(worktree.path()), "").unwrap();
        assert_eq!(read(worktree.path()), None, "empty");

        fs::write(path(worktree.path()), "## 2026 · a\nshort\n\n").unwrap();
        let short = read(worktree.path()).unwrap();
        assert!(short.whole);
        assert_eq!(short.excerpt, "## 2026 · a\nshort");

        let long: String = (0..400).map(|line| format!("line {line:04}\n")).collect();
        fs::write(path(worktree.path()), &long).unwrap();
        let end = read(worktree.path()).unwrap();
        assert!(!end.whole);
        assert!(end.excerpt.len() <= EXCERPT_BYTES);
        assert!(end.excerpt.starts_with("line "), "from a whole line");
        assert!(end.excerpt.ends_with("line 0399"));
    }

    #[test]
    fn the_rule_points_at_the_file_and_shows_its_end_while_there_is_room() {
        let notes = Notes {
            path: PathBuf::from("/code/app/.crystal/handoff.md"),
            excerpt: "## 2026 · a\nfound it".into(),
            whole: true,
        };
        let inline = fitting(&notes, 0);
        assert!(inline.contains("/code/app/.crystal/handoff.md"), "{inline}");
        assert!(inline.contains("crystal handoff \"<note>\""), "{inline}");
        assert!(inline.ends_with("The file as it is now:\n\n## 2026 · a\nfound it"));
        let partial = Notes {
            whole: false,
            ..notes.clone()
        };
        assert!(fitting(&partial, 0).contains("Its latest notes"));

        // A prompt that's nearly full gets the pointer alone.
        let pointer = fitting(&notes, MAX_PROMPT_BYTES - 100);
        assert!(pointer.contains("crystal handoff") && !pointer.contains("found it"));
    }

    #[test]
    fn a_project_keeps_its_notes_in_git_when_the_config_says() {
        let mut config = Config::default();
        assert!(!in_git(&config, Path::new("/code/app")));
        config.handoff.in_git = vec![PathBuf::from("/code/app")];
        assert!(in_git(&config, Path::new("/code/app")));
        assert!(!in_git(&config, Path::new("/code/other")));
        let home = std::env::var("HOME").unwrap();
        config.handoff.in_git = vec![PathBuf::from("~/code/app")];
        assert!(in_git(&config, &Path::new(&home).join("code/app")));
    }
}
