//! A diff, read from git's own patch format into files, hunks and lines,
//! then laid out in rows for drawing: unified, one line after another, or
//! side by side, the old file on the left and the new one on the right.
//!
//! Everything about a file is read from its part of the patch: whether it's
//! new, deleted or renamed, and how many lines it adds and removes. So one
//! `git diff` is all the diff view needs, besides the files git doesn't
//! know yet, which [`new_file`] turns into a diff of their own.

use similar::{Algorithm, DiffTag, capture_diff_slices};
use std::ops::Range;

/// A file with more changed lines than this shows a note instead: reading
/// thousands of lines here is no help, and drawing them costs.
pub const MAX_LINES: usize = 5_000;

/// How many bytes of a file are looked at for a NUL, which says a file is
/// binary, the way git itself decides.
const BINARY_CHECK: usize = 8_000;

/// How a file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    /// New, and not known to git yet.
    Untracked,
}

impl FileStatus {
    /// The letter the file list shows, the way git and editors do.
    pub fn letter(self) -> char {
        match self {
            FileStatus::Modified => 'M',
            FileStatus::Added => 'A',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
            FileStatus::Untracked => 'U',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Where the file is now, or was, if it's deleted.
    pub path: String,
    /// Where a renamed file came from.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub added: usize,
    pub removed: usize,
    pub body: Body,
    /// A hash of the file's part of the patch, or of a new file's content:
    /// it changes whenever the file changes again, which is what takes a
    /// reviewed mark off it. git's `index` line names the file's content,
    /// so a binary file's changes count too.
    pub hash: u64,
}

/// What there is to show of a file's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Hunks(Vec<Hunk>),
    /// git says the file is binary: there are no lines to show.
    Binary,
    /// More changed lines than [`MAX_LINES`].
    TooBig,
}

/// A stretch of a file where something changed, with a few lines of what
/// didn't around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Where it is in both files, as git writes it: `-12,7 +12,9`.
    pub range: String,
    /// What git says the stretch is in: the function or heading above it.
    pub context: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// In both files.
    Context,
    Removed,
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's number in the old file, if it's there.
    pub old: Option<usize>,
    /// The line's number in the new file, if it's there.
    pub new: Option<usize>,
    pub text: String,
    /// The parts of `text` that changed, when the line is one side of a
    /// changed line: the words that get the stronger color.
    pub changed: Changed,
}

/// Parts of a line, as byte ranges of its text, in order.
pub type Changed = Vec<Range<usize>>;

/// Reads a patch, as `git diff` prints it, into its files.
pub fn parse(patch: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut reading: Option<Reading> = None;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(reading.take().map(Reading::finish));
            reading = Some(Reading::new(rest));
        } else if let Some(file) = &mut reading {
            file.read(line);
        }
    }
    files.extend(reading.map(Reading::finish));
    files
}

/// A file git doesn't know yet, as a diff that adds every line of it.
/// `content` is the file as it is on disk.
pub fn new_file(path: &str, content: &[u8]) -> FileDiff {
    let mut file = FileDiff {
        path: path.to_string(),
        old_path: None,
        status: FileStatus::Untracked,
        added: 0,
        removed: 0,
        body: Body::Binary,
        hash: hash(FNV_OFFSET, content),
    };
    let start = &content[..content.len().min(BINARY_CHECK)];
    if start.contains(&0) {
        return file;
    }
    let text = String::from_utf8_lossy(content);
    let lines: Vec<DiffLine> = text
        .lines()
        .enumerate()
        .map(|(index, line)| DiffLine {
            kind: LineKind::Added,
            old: None,
            new: Some(index + 1),
            text: line.to_string(),
            changed: Vec::new(),
        })
        .collect();
    file.added = lines.len();
    file.body = if lines.len() > MAX_LINES {
        Body::TooBig
    } else {
        Body::Hunks(vec![Hunk {
            range: format!("-0,0 +1,{}", lines.len()),
            context: String::new(),
            lines,
        }])
    };
    file
}

/// Where an FNV-1a hash starts.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// `hash` taken on over `bytes`, and a line's end after them: FNV-1a,
/// written out here because reviewed marks are kept from one run to the
/// next, and std's hasher may change between Rust releases.
fn hash(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes.iter().chain(b"\n") {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// One file's part of the patch, while it's being read.
struct Reading {
    file: FileDiff,
    /// Every hunk read so far. Until the first, lines are the file's header.
    hunks: Vec<Hunk>,
    /// The next line's number in each file, inside a hunk.
    next_old: usize,
    next_new: usize,
}

impl Reading {
    /// Starts a file from its `diff --git a/<old> b/<new>` line, whose path
    /// is only a guess: a path may hold " b/" itself. The `+++` line, or a
    /// rename's, says for certain, when the patch has one.
    fn new(header: &str) -> Reading {
        let path = match header.rfind(" b/") {
            Some(at) => header[at + 3..].to_string(),
            None => header.to_string(),
        };
        Reading {
            file: FileDiff {
                path,
                old_path: None,
                status: FileStatus::Modified,
                added: 0,
                removed: 0,
                body: Body::Hunks(Vec::new()),
                hash: hash(FNV_OFFSET, header.as_bytes()),
            },
            hunks: Vec::new(),
            next_old: 0,
            next_new: 0,
        }
    }

    fn read(&mut self, line: &str) {
        self.file.hash = hash(self.file.hash, line.as_bytes());
        if let Some(header) = line.strip_prefix("@@ ") {
            self.start_hunk(header);
        } else if let Some(hunk) = self.hunks.last_mut() {
            // Inside the hunks, a line is what its first character says.
            let (kind, text) = match line.split_at_checked(1) {
                Some((" ", text)) => (LineKind::Context, text),
                Some(("-", text)) => (LineKind::Removed, text),
                Some(("+", text)) => (LineKind::Added, text),
                // `\ No newline at end of file`, and anything unexpected.
                _ => return,
            };
            let old = (kind != LineKind::Added).then_some(self.next_old);
            let new = (kind != LineKind::Removed).then_some(self.next_new);
            self.next_old += usize::from(old.is_some());
            self.next_new += usize::from(new.is_some());
            hunk.lines.push(DiffLine {
                kind,
                old,
                new,
                text: text.to_string(),
                changed: Vec::new(),
            });
        } else {
            self.read_header(line);
        }
    }

    /// A line of the file's header, before its first hunk.
    fn read_header(&mut self, line: &str) {
        let file = &mut self.file;
        if line.starts_with("new file mode") {
            file.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
        } else if let Some(from) = line.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = Some(from.to_string());
        } else if let Some(to) = line.strip_prefix("rename to ") {
            file.path = to.to_string();
        } else if line.starts_with("Binary files ") {
            file.body = Body::Binary;
        } else if let Some(new) = line.strip_prefix("+++ b/") {
            file.path = without_tab(new).to_string();
        } else if let Some(old) = line.strip_prefix("--- a/") {
            // A deleted file has no `+++ b/` line to say where it was.
            if file.status == FileStatus::Deleted {
                file.path = without_tab(old).to_string();
            }
        }
    }

    /// Starts a hunk from its header: `-12,7 +12,9 @@ fn refund()`.
    fn start_hunk(&mut self, header: &str) {
        let (range, context) = header.split_once(" @@").unwrap_or((header, ""));
        let mut starts = range.split(' ').map(|side| {
            let side = side.trim_start_matches(['-', '+']);
            let start = side.split(',').next().unwrap_or("0");
            start.parse::<usize>().unwrap_or(0)
        });
        self.next_old = starts.next().unwrap_or(0);
        self.next_new = starts.next().unwrap_or(0);
        self.hunks.push(Hunk {
            range: range.to_string(),
            context: context.trim().to_string(),
            lines: Vec::new(),
        });
    }

    fn finish(mut self) -> FileDiff {
        let lines = self.hunks.iter().flat_map(|hunk| &hunk.lines);
        for line in lines {
            match line.kind {
                LineKind::Added => self.file.added += 1,
                LineKind::Removed => self.file.removed += 1,
                LineKind::Context => {}
            }
        }
        if self.file.body == Body::Binary {
            return self.file;
        }
        self.file.body = if self.file.added + self.file.removed > MAX_LINES {
            Body::TooBig
        } else {
            for hunk in &mut self.hunks {
                mark_changed_words(hunk);
            }
            Body::Hunks(self.hunks)
        };
        self.file
    }
}

/// A path as git wrote it on a `---` or `+++` line, which ends in a tab
/// when the path has a space in it.
fn without_tab(path: &str) -> &str {
    path.strip_suffix('\t').unwrap_or(path)
}

/// Pairs each run of removed lines with the run of added lines right after
/// it, first with first, and marks the words that changed between each
/// pair: what turned a line into its new version.
fn mark_changed_words(hunk: &mut Hunk) {
    let lines = &mut hunk.lines;
    let mut at = 0;
    while at < lines.len() {
        let removed = run(lines, at, LineKind::Removed);
        let added = run(lines, at + removed, LineKind::Added);
        for pair in 0..removed.min(added) {
            let (old, new) = (at + pair, at + removed + pair);
            if let Some((old_changed, new_changed)) =
                changed_words(&lines[old].text, &lines[new].text)
            {
                lines[old].changed = old_changed;
                lines[new].changed = new_changed;
            }
        }
        at += (removed + added).max(1);
    }
}

/// How many lines of `kind` there are in a row from `start`.
fn run(lines: &[DiffLine], start: usize, kind: LineKind) -> usize {
    lines[start.min(lines.len())..]
        .iter()
        .take_while(|line| line.kind == kind)
        .count()
}

/// The parts of `old` and of `new` that differ, token by token, as byte
/// ranges: see [`tokens`]. `None` when the line was mostly rewritten: then
/// everything has changed, and the line's own color says so better than
/// marking it all.
pub fn changed_words(old: &str, new: &str) -> Option<(Changed, Changed)> {
    let old_tokens = tokens(old);
    let new_tokens = tokens(new);
    let old_texts: Vec<&str> = old_tokens.iter().map(|at| &old[at.clone()]).collect();
    let new_texts: Vec<&str> = new_tokens.iter().map(|at| &new[at.clone()]).collect();

    let (mut old_changed, mut new_changed) = (Changed::new(), Changed::new());
    for change in capture_diff_slices(Algorithm::Myers, &old_texts, &new_texts) {
        if change.tag() == DiffTag::Equal {
            continue;
        }
        if let Some(bytes) = covered(&old_tokens, change.old_range()) {
            add_range(&mut old_changed, bytes);
        }
        if let Some(bytes) = covered(&new_tokens, change.new_range()) {
            add_range(&mut new_changed, bytes);
        }
    }
    let changed: usize = old_changed.iter().chain(&new_changed).map(Range::len).sum();
    let total = old.len() + new.len();
    // Over three fifths changed reads as a rewrite.
    if total == 0 || changed * 5 > total * 3 {
        return None;
    }
    Some((old_changed, new_changed))
}

/// Where each token of `text` is, as byte ranges. A token is a run of
/// letters, digits and `_`, a run of whitespace, or any other character on
/// its own: code's words and its punctuation, so that `total;` and
/// `total - fee;` differ by ` - fee`, not by the whole of `total;`.
fn tokens(text: &str) -> Changed {
    let mut tokens = Changed::new();
    let mut previous = None;
    for (at, letter) in text.char_indices() {
        let kind = CharKind::of(letter);
        let end = at + letter.len_utf8();
        let joins_previous = kind != CharKind::Other && previous == Some(kind);
        match tokens.last_mut() {
            Some(last) if joins_previous => last.end = end,
            _ => tokens.push(at..end),
        }
        previous = Some(kind);
    }
    tokens
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Word,
    Space,
    Other,
}

impl CharKind {
    fn of(letter: char) -> CharKind {
        if letter.is_alphanumeric() || letter == '_' {
            CharKind::Word
        } else if letter.is_whitespace() {
            CharKind::Space
        } else {
            CharKind::Other
        }
    }
}

/// The bytes the tokens at `range` cover, if it covers any.
fn covered(tokens: &[Range<usize>], range: Range<usize>) -> Option<Range<usize>> {
    if range.is_empty() {
        return None;
    }
    Some(tokens[range.start].start..tokens[range.end - 1].end)
}

/// Adds `range` to `ranges`, joining it to the last one when they touch.
fn add_range(ranges: &mut Changed, range: Range<usize>) {
    match ranges.last_mut() {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}

/// How a diff is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Every line one after the other, removed lines before the lines that
    /// replace them, the way `git diff` and GitHub show it.
    Unified,
    /// The old file on the left and the new on the right, each changed line
    /// beside its new version, the way VS Code's split view shows it.
    SideBySide,
}

/// One row of a laid-out diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row<'a> {
    /// The divider at the start of a hunk.
    Hunk(&'a Hunk),
    /// A line, in the unified layout.
    Line(&'a DiffLine),
    /// The two sides of a row, side by side: either may be missing, where
    /// one file has lines the other hasn't.
    Pair(Option<&'a DiffLine>, Option<&'a DiffLine>),
}

/// The rows `file` is drawn in, laid out as `layout`. A file with nothing to
/// show, binary or too big, has none.
pub fn rows(file: &FileDiff, layout: Layout) -> Vec<Row<'_>> {
    let Body::Hunks(hunks) = &file.body else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for hunk in hunks {
        rows.push(Row::Hunk(hunk));
        match layout {
            Layout::Unified => rows.extend(hunk.lines.iter().map(Row::Line)),
            Layout::SideBySide => rows.extend(side_by_side(&hunk.lines)),
        }
    }
    rows
}

/// A hunk's lines side by side: a line both files have, on both sides; a
/// run of removed lines beside the run of added lines after it, first with
/// first; and a gap on the side that has fewer.
fn side_by_side(lines: &[DiffLine]) -> Vec<Row<'_>> {
    let mut rows = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if lines[at].kind == LineKind::Context {
            rows.push(Row::Pair(Some(&lines[at]), Some(&lines[at])));
            at += 1;
            continue;
        }
        let removed = run(lines, at, LineKind::Removed);
        let added = run(lines, at + removed, LineKind::Added);
        let old = &lines[at..at + removed];
        let new = &lines[at + removed..at + removed + added];
        for pair in 0..removed.max(added) {
            rows.push(Row::Pair(old.get(pair), new.get(pair)));
        }
        at += removed + added;
    }
    rows
}

/// How many columns the widest line number in `file` takes.
pub fn number_width(file: &FileDiff) -> usize {
    let Body::Hunks(hunks) = &file.body else {
        return 1;
    };
    let widest = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .flat_map(|line| [line.old, line.new])
        .flatten()
        .max()
        .unwrap_or(0);
    widest.to_string().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
diff --git a/src/refund.rs b/src/refund.rs
index 3b18e51..a4c2f03 100644
--- a/src/refund.rs
+++ b/src/refund.rs
@@ -10,4 +10,5 @@ fn refund(order: &Order) {
     let ledger = open();
-    let total = order.total;
+    let total = order.total - order.fee;
+    log(total);
     ledger.write(total);
@@ -40,2 +41,2 @@ fn cancel() {
-    old();
+    new();
diff --git a/notes.md b/notes.md
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/notes.md
@@ -0,0 +1,2 @@
+# Notes
+second line
\\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 9daeafb..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old name.rs b/new name.rs
similarity index 90%
rename from old name.rs
rename to new name.rs
index 1111111..2222222 100644
--- a/old name.rs\t
+++ b/new name.rs\t
@@ -1 +1 @@
-let a = 1;
+let a = 2;
diff --git a/logo.png b/logo.png
index 1111111..2222222 100644
Binary files a/logo.png and b/logo.png differ
";

    fn files() -> Vec<FileDiff> {
        parse(PATCH)
    }

    fn hunks(file: &FileDiff) -> &[Hunk] {
        match &file.body {
            Body::Hunks(hunks) => hunks,
            other => panic!("{} has no hunks: {other:?}", file.path),
        }
    }

    #[test]
    fn a_patch_reads_into_its_files_with_their_status() {
        let files = files();
        let found: Vec<(&str, char)> = files
            .iter()
            .map(|file| (file.path.as_str(), file.status.letter()))
            .collect();
        assert_eq!(
            found,
            [
                ("src/refund.rs", 'M'),
                ("notes.md", 'A'),
                ("gone.txt", 'D'),
                ("new name.rs", 'R'),
                ("logo.png", 'M'),
            ]
        );
        assert_eq!(files[3].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(files[4].body, Body::Binary);
    }

    #[test]
    fn each_file_counts_what_it_adds_and_removes() {
        let counts: Vec<(usize, usize)> = files()
            .iter()
            .map(|file| (file.added, file.removed))
            .collect();
        assert_eq!(counts, [(3, 2), (2, 0), (0, 1), (1, 1), (0, 0)]);
    }

    #[test]
    fn hunks_keep_their_range_and_what_they_are_in() {
        let files = files();
        let hunks = hunks(&files[0]);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].range, "-10,4 +10,5");
        assert_eq!(hunks[0].context, "fn refund(order: &Order) {");
        assert_eq!(hunks[1].context, "fn cancel() {");
    }

    #[test]
    fn lines_are_numbered_in_the_file_they_are_in() {
        let files = files();
        let numbers: Vec<(LineKind, Option<usize>, Option<usize>)> = hunks(&files[0])[0]
            .lines
            .iter()
            .map(|line| (line.kind, line.old, line.new))
            .collect();
        assert_eq!(
            numbers,
            [
                (LineKind::Context, Some(10), Some(10)),
                (LineKind::Removed, Some(11), None),
                (LineKind::Added, None, Some(11)),
                (LineKind::Added, None, Some(12)),
                (LineKind::Context, Some(12), Some(13)),
            ]
        );
    }

    #[test]
    fn the_no_newline_note_is_not_a_line() {
        let files = files();
        let lines = &hunks(&files[1])[0].lines;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "second line");
    }

    /// The parts of `text` that `changed` marks.
    fn marked<'t>(text: &'t str, changed: &Changed) -> Vec<&'t str> {
        changed.iter().map(|range| &text[range.clone()]).collect()
    }

    #[test]
    fn a_changed_line_marks_the_words_that_changed() {
        let files = files();
        let lines = &hunks(&files[0])[0].lines;
        let (old, new) = (&lines[1], &lines[2]);
        assert!(old.changed.is_empty(), "nothing was taken out");
        assert_eq!(marked(&new.text, &new.changed).concat(), " - order.fee");
        // The line added after the pair has nothing to pair with.
        assert!(lines[3].changed.is_empty());
    }

    #[test]
    fn code_reads_as_words_spaces_and_punctuation() {
        let text = "let ab_1 = f(x);";
        let found: Vec<&str> = tokens(text).into_iter().map(|at| &text[at]).collect();
        assert_eq!(
            found,
            ["let", " ", "ab_1", " ", "=", " ", "f", "(", "x", ")", ";"]
        );
    }

    #[test]
    fn a_rewritten_line_marks_nothing() {
        assert_eq!(changed_words("let a = 1;", "while true {}"), None);
        let (old, new) = changed_words("let a = 1;", "let a = 2;").unwrap();
        assert_eq!(marked("let a = 1;", &old), ["1"]);
        assert_eq!(marked("let a = 2;", &new), ["2"]);
    }

    #[test]
    fn side_by_side_puts_each_changed_line_beside_its_new_version() {
        let files = files();
        let rows = rows(&files[0], Layout::SideBySide);
        let shape: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Hunk(_) => "hunk".to_string(),
                Row::Pair(old, new) => {
                    let side = |line: &Option<&DiffLine>| match line {
                        Some(line) => format!("{:?}", line.kind),
                        None => "gap".to_string(),
                    };
                    format!("{} | {}", side(old), side(new))
                }
                Row::Line(_) => panic!("side by side has no single lines"),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "hunk",
                "Context | Context",
                "Removed | Added",
                "gap | Added",
                "Context | Context",
                "hunk",
                "Removed | Added",
            ]
        );
    }

    #[test]
    fn unified_rows_are_the_lines_in_order() {
        let files = files();
        let rows = rows(&files[0], Layout::Unified);
        // Two hunk dividers, and every line of both hunks.
        assert_eq!(rows.len(), 2 + 5 + 2);
        assert!(matches!(rows[0], Row::Hunk(_)));
        assert!(matches!(rows[2], Row::Line(line) if line.kind == LineKind::Removed));
    }

    #[test]
    fn a_file_with_nothing_to_show_has_no_rows() {
        let files = files();
        assert!(rows(&files[4], Layout::Unified).is_empty());
    }

    #[test]
    fn a_files_hash_changes_with_its_part_of_the_patch_alone() {
        let before = files();
        let binary_changed = PATCH.replace(
            "index 1111111..2222222 100644\nBinary",
            "index 1111111..3333333 100644\nBinary",
        );
        let after = parse(&binary_changed);
        assert_eq!(before[0].hash, after[0].hash);
        assert_ne!(before[4].hash, after[4].hash);
        // Kept from one run to the next, so it mustn't drift.
        assert_eq!(hash(FNV_OFFSET, b""), 0xaf63_c74c_8601_c8dd);
        assert_ne!(
            new_file("a.txt", b"one\n").hash,
            new_file("a.txt", b"two\n").hash
        );
    }

    #[test]
    fn a_new_file_adds_every_line() {
        let file = new_file("todo.txt", b"one\ntwo\n");
        assert_eq!(file.status, FileStatus::Untracked);
        assert_eq!((file.added, file.removed), (2, 0));
        let lines = &hunks(&file)[0].lines;
        assert_eq!(lines[1].new, Some(2));
        assert_eq!(new_file("blob.bin", b"\x89PNG\0\0").body, Body::Binary);
    }

    #[test]
    fn a_huge_file_shows_a_note_instead() {
        let content = "line\n".repeat(MAX_LINES + 1);
        assert_eq!(new_file("big.txt", content.as_bytes()).body, Body::TooBig);
    }

    #[test]
    fn numbers_are_as_wide_as_the_widest() {
        let files = files();
        assert_eq!(number_width(&files[0]), 2);
        assert_eq!(number_width(&files[4]), 1);
    }
}
