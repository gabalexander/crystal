//! The diff view, `d` in the sidebar: what changed in a session's worktree,
//! the way VS Code and GitHub show it. The changed files are listed on the
//! left with how many lines each adds and removes; the selected file's
//! diff is on the right, added lines on green, removed lines on red, and
//! the words that changed inside a line marked stronger.
//!
//! It compares with the last commit, every change not committed yet, or,
//! after `b`, with where the branch started, everything committed on it:
//! how a branch an agent worked on reads as a pull request. Opened from the
//! pull requests view, it's a pull request's own diff, as its forge has it.
//!
//! A file read can be marked reviewed, `r`: it sinks to the bottom of the
//! list with a ✓, until it changes again (see [`review`]). `t` folds the
//! list into a tree of directories ([`Tree`]), and back.

use super::app::{Action, Hit, Loading, Outcome};
use super::diff::{self, Body, DiffLine, FileDiff, FileStatus, Layout, LineKind, Row};
use super::diff_tree::{Kind, Tree};
use super::review::{Marks, Scope};
use super::sidebar::fit;
use super::theme::Theme;
use super::ui::{self, Look, ViewAreas};
use crate::forge::{Forge, Repo};
use crate::git;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Side by side needs room for two files' lines: below this many columns,
/// the diff is unified whatever was asked.
pub const SIDE_BY_SIDE_WIDTH: u16 = 120;

/// How many rows a notch of the mouse wheel scrolls.
const WHEEL_ROWS: usize = 3;

/// How wide a tab is drawn.
const TAB: &str = "    ";

/// What the diff compares the worktree with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Against {
    /// The last commit: every change not committed yet, staged or not, and
    /// the new files git doesn't know about yet.
    Uncommitted,
    /// Where the branch started: everything committed on it since.
    Branch,
    /// Nothing in the worktree: what pull request `number` changes, as its
    /// forge has it.
    PullRequest { forge: Forge, number: u64 },
}

/// A diff as it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub files: Vec<FileDiff>,
    /// For a branch, the branch it started from.
    pub base: Option<String>,
    /// The commit the worktree is on; empty for a pull request.
    pub head: String,
    /// The files kept as reviewed in this diff, as the event loop read
    /// them, those that changed since included: the view sees to those.
    pub reviewed: Marks,
}

/// Reads the diff of the worktree at `dir`. It runs git, which can take a
/// while, so it's run off the event loop; the reviewed marks are the event
/// loop's to add.
pub fn read(dir: &Path, against: Against) -> Result<Read, String> {
    let read = match against {
        Against::Uncommitted => read_uncommitted(dir),
        Against::Branch => read_branch(dir),
        Against::PullRequest { number, .. } => return read_pull_request(dir, number),
    };
    read.map_err(|err| format!("{err:#}"))
}

/// Which diff a view's reviewed marks are kept for.
pub fn scope(dir: &Path, against: Against, head: &str) -> Scope {
    let diff = match against {
        Against::Uncommitted => "uncommitted".to_string(),
        Against::Branch => "branch".to_string(),
        Against::PullRequest { number, .. } => format!("pull request {number}"),
    };
    Scope {
        dir: dir.to_path_buf(),
        diff,
        head: head.to_string(),
    }
}

/// What pull request `number` of the project at `dir` changes, from its
/// forge, which goes over the network.
fn read_pull_request(dir: &Path, number: u64) -> Result<Read, String> {
    let patch = Repo::find(dir)?.diff(number)?;
    Ok(Read {
        files: diff::parse(&patch),
        base: None,
        head: String::new(),
        reviewed: Marks::new(),
    })
}

fn read_uncommitted(dir: &Path) -> anyhow::Result<Read> {
    let head = git::head(dir);
    let mut files = diff::parse(&git::uncommitted_patch(dir)?);
    for path in git::untracked_files(dir)? {
        // Read as much as the diff could ever show, and not much more.
        let content = read_start(&dir.join(&path), 1 << 20);
        files.push(diff::new_file(&path, &content));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Read {
        files,
        base: None,
        head,
        reviewed: Marks::new(),
    })
}

fn read_branch(dir: &Path) -> anyhow::Result<Read> {
    let head = git::head(dir);
    let (base, commit) = git::branch_start(dir)?;
    let files = diff::parse(&git::patch_since(dir, &commit)?);
    Ok(Read {
        files,
        base: Some(base),
        head,
        reviewed: Marks::new(),
    })
}

/// The first `most` bytes of the file at `path`, or none if it can't be
/// read.
fn read_start(path: &Path, most: u64) -> Vec<u8> {
    use std::io::Read as _;
    let mut start = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(most).read_to_end(&mut start);
    }
    start
}

pub struct DiffView {
    /// The worktree it's the diff of.
    pub dir: PathBuf,
    /// The worktree's project and branch, for the header.
    pub place: String,
    pub against: Against,
    /// The layout asked for with `v`; see [`DiffView::layout`].
    wanted: Layout,
    pub diff: Loading<Read>,
    /// The files marked reviewed, with their hashes then: only those
    /// that haven't changed since.
    reviewed: Marks,
    /// Whether the files are listed as a tree, which [`DiffView::tree`]
    /// is once they're read.
    as_tree: bool,
    tree: Option<Tree>,
    /// The selected row of the list.
    pub selected: usize,
    /// How many rows down the selected file's diff is scrolled.
    pub scroll: usize,
    /// The size of the area the diff is drawn in, as `(rows, columns)`.
    size: (u16, u16),
}

/// A row of the list: a file, by its place in the diff, or, in the tree, a
/// directory, by its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    File(usize),
    Dir(usize),
}

impl DiffView {
    /// The uncommitted changes of the worktree at `dir`, until they're read,
    /// its files listed as a tree if `as_tree`.
    pub fn new(dir: PathBuf, place: String, as_tree: bool) -> DiffView {
        DiffView {
            dir,
            place,
            against: Against::Uncommitted,
            wanted: Layout::Unified,
            diff: Loading::Reading,
            reviewed: Marks::new(),
            as_tree,
            tree: None,
            selected: 0,
            scroll: 0,
            size: (24, 80),
        }
    }

    /// What pull request `number` changes, the project at `dir` being on
    /// `forge`, until it's read.
    pub fn of_pull_request(
        dir: PathBuf,
        place: String,
        as_tree: bool,
        forge: Forge,
        number: u64,
    ) -> DiffView {
        DiffView {
            against: Against::PullRequest { forge, number },
            ..DiffView::new(dir, place, as_tree)
        }
    }

    /// What reading the diff takes, for the event loop to do.
    pub fn read(&self) -> Action {
        Action::ReadDiff {
            dir: self.dir.clone(),
            against: self.against,
        }
    }

    /// Takes a diff that's been read, unless it's an older one than the
    /// view now wants, and selects the first file not reviewed yet.
    pub fn read_done(&mut self, dir: &Path, against: Against, read: Result<Read, String>) {
        if dir != self.dir || against != self.against {
            return;
        }
        self.diff = match read {
            Ok(mut read) => {
                let files = &read.files;
                let unchanged = |path: &String, hash: &mut u64| {
                    files
                        .iter()
                        .any(|file| file.path == *path && file.hash == *hash)
                };
                read.reviewed.retain(unchanged);
                self.reviewed = std::mem::take(&mut read.reviewed);
                Loading::Read(read)
            }
            Err(err) => Loading::Failed(err),
        };
        self.tree = self.as_tree.then(|| Tree::new(&self.paths()));
        self.selected = self.first_unreviewed();
        self.scroll = 0;
    }

    /// The size of the area the diff is drawn in, as `(rows, columns)`.
    pub fn set_size(&mut self, size: (u16, u16)) {
        self.size = size;
    }

    /// The layout the diff is drawn in: side by side when it was asked
    /// for and there's room, unified otherwise.
    pub fn layout(&self) -> Layout {
        if self.wanted == Layout::SideBySide && self.size.1 >= SIDE_BY_SIDE_WIDTH {
            Layout::SideBySide
        } else {
            Layout::Unified
        }
    }

    /// Whether side by side was asked for but there's no room for it.
    pub fn too_narrow(&self) -> bool {
        self.wanted == Layout::SideBySide && self.layout() == Layout::Unified
    }

    pub fn files(&self) -> &[FileDiff] {
        match &self.diff {
            Loading::Read(read) => &read.files,
            _ => &[],
        }
    }

    fn paths(&self) -> Vec<&str> {
        self.files().iter().map(|file| file.path.as_str()).collect()
    }

    /// The rows of the list, top to bottom: the files, those reviewed
    /// last, or, as a tree, its rows.
    pub fn entries(&self) -> Vec<Entry> {
        if let Some(tree) = &self.tree {
            return tree
                .rows()
                .iter()
                .map(|&node| match tree.node(node).kind {
                    Kind::File(file) => Entry::File(file),
                    Kind::Dir { .. } => Entry::Dir(node),
                })
                .collect();
        }
        let mut files: Vec<usize> = (0..self.files().len()).collect();
        files.sort_by_key(|file| self.is_reviewed(*file));
        files.into_iter().map(Entry::File).collect()
    }

    /// The tree the files are listed as, when they are.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    pub fn selected_entry(&self) -> Option<Entry> {
        self.entries().get(self.selected).copied()
    }

    /// The selected file, unless a directory's row is selected.
    pub fn file(&self) -> Option<&FileDiff> {
        match self.selected_entry()? {
            Entry::File(file) => self.files().get(file),
            Entry::Dir(_) => None,
        }
    }

    /// Whether the file at `file` in the diff is marked reviewed.
    pub fn is_reviewed(&self, file: usize) -> bool {
        self.files()
            .get(file)
            .is_some_and(|file| self.reviewed.get(&file.path) == Some(&file.hash))
    }

    /// Whether every file under the tree's `node` is marked reviewed.
    pub fn all_reviewed(&self, node: usize) -> bool {
        let Some(tree) = &self.tree else {
            return false;
        };
        let files = tree.files_under(node);
        !files.is_empty() && files.iter().all(|file| self.is_reviewed(*file))
    }

    /// How many files are marked reviewed.
    pub fn reviewed_count(&self) -> usize {
        (0..self.files().len())
            .filter(|file| self.is_reviewed(*file))
            .count()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.page();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Char('j') | KeyCode::Down => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::Char(' ') if shift => self.scroll_up(page),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_down(page),
            KeyCode::PageUp => self.scroll_up(page),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = self.last_scroll(),
            KeyCode::Char(']') => self.next_hunk(),
            KeyCode::Char('[') => self.previous_hunk(),
            KeyCode::Char('v') => self.toggle_layout(),
            KeyCode::Char('b') => return self.toggle_against(),
            KeyCode::Char('r') => return self.toggle_reviewed(),
            KeyCode::Char('t') => return self.toggle_tree(),
            KeyCode::Enter => self.toggle_dir(self.selected),
            KeyCode::Char('l') | KeyCode::Right => self.open_dir(),
            KeyCode::Char('h') | KeyCode::Left => self.fold_dir(),
            _ => {}
        }
        Outcome::Stay
    }

    /// The wheel scrolls the diff, or, over the list, moves through the
    /// files; a click on the list selects a file, or folds or opens a
    /// directory.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(index)) => {
                self.select(index);
                self.toggle_dir(index);
            }
            (MouseEventKind::ScrollUp, Hit::ViewList(_)) => {
                self.select(self.selected.saturating_sub(1));
            }
            (MouseEventKind::ScrollDown, Hit::ViewList(_)) => self.select(self.selected + 1),
            (MouseEventKind::ScrollUp, Hit::ViewContent) => self.scroll_up(WHEEL_ROWS),
            (MouseEventKind::ScrollDown, Hit::ViewContent) => self.scroll_down(WHEEL_ROWS),
            _ => {}
        }
        Outcome::Stay
    }

    /// Selects the row at `index`, kept in range, from the top of its diff.
    fn select(&mut self, index: usize) {
        let last = self.entries().len().saturating_sub(1);
        let index = index.min(last);
        if index != self.selected {
            self.selected = index;
            self.scroll = 0;
        }
    }

    /// The row where reading starts: the first file not reviewed yet, or
    /// else the first file.
    fn first_unreviewed(&self) -> usize {
        let entries = self.entries();
        let file_row = |unreviewed: bool| {
            entries.iter().position(|entry| match entry {
                Entry::File(file) => !unreviewed || !self.is_reviewed(*file),
                Entry::Dir(_) => false,
            })
        };
        file_row(true).or_else(|| file_row(false)).unwrap_or(0)
    }

    /// Marks the selected file reviewed, or takes its mark off, and asks
    /// for the marks to be kept. In the list, the file sinks to the bottom
    /// or comes back up, and the selection stays in the same place, on the
    /// next file, to read on; it follows the file when that's all there
    /// is. In the tree, nothing moves, so the selection goes on to the
    /// next file still as this one was.
    fn toggle_reviewed(&mut self) -> Outcome {
        let Some(Entry::File(file)) = self.selected_entry() else {
            return Outcome::Stay;
        };
        let Some(scope) = self.scope() else {
            return Outcome::Stay;
        };
        let was_reviewed = self.is_reviewed(file);
        let (path, hash) = {
            let file = &self.files()[file];
            (file.path.clone(), file.hash)
        };
        if was_reviewed {
            self.reviewed.remove(&path);
        } else {
            self.reviewed.insert(path, hash);
        }
        let next = self.next_to_read(file, was_reviewed);
        self.select(next);
        Outcome::Do(Action::KeepReviewed {
            scope,
            marks: self.reviewed.clone(),
        })
    }

    /// The row to go on to once `file`, which `was_reviewed`, has been
    /// marked the other way: see [`DiffView::toggle_reviewed`].
    fn next_to_read(&self, file: usize, was_reviewed: bool) -> usize {
        let entries = self.entries();
        let as_it_was = |entry: &Entry| match entry {
            Entry::File(other) => self.is_reviewed(*other) == was_reviewed,
            Entry::Dir(_) => false,
        };
        if self.tree.is_some() {
            let below = entries.iter().skip(self.selected + 1).position(as_it_was);
            return below.map_or(self.selected, |at| self.selected + 1 + at);
        }
        if !entries.iter().any(as_it_was) {
            entries
                .iter()
                .position(|entry| *entry == Entry::File(file))
                .unwrap_or(self.selected)
        } else if was_reviewed {
            // The file left the reviewed files at the bottom: the next of
            // them is a row down.
            (self.selected + 1).min(entries.len() - 1)
        } else {
            self.selected
        }
    }

    /// Which diff the view's marks are kept for, once it's read.
    fn scope(&self) -> Option<Scope> {
        let Loading::Read(read) = &self.diff else {
            return None;
        };
        Some(scope(&self.dir, self.against, &read.head))
    }

    /// Lists the files as a tree, or flat again, keeping the selected file
    /// selected, and asks for the choice to be kept. Leaving the tree from
    /// a directory's row, its first file stands in for it.
    fn toggle_tree(&mut self) -> Outcome {
        let file = match self.selected_entry() {
            Some(Entry::File(file)) => Some(file),
            Some(Entry::Dir(node)) => self
                .tree
                .as_ref()
                .and_then(|tree| tree.files_under(node).first().copied()),
            None => None,
        };
        self.as_tree = !self.as_tree;
        self.tree = match &self.diff {
            Loading::Read(_) if self.as_tree => Some(Tree::new(&self.paths())),
            _ => None,
        };
        let row = match (&mut self.tree, file) {
            (Some(tree), Some(file)) => tree.reveal(file),
            (None, Some(file)) => self
                .entries()
                .iter()
                .position(|entry| *entry == Entry::File(file)),
            (_, None) => None,
        };
        self.selected = row.unwrap_or(0);
        Outcome::Do(Action::KeepTree(self.as_tree))
    }

    /// Folds or opens the directory on `row`, if it's a directory's.
    fn toggle_dir(&mut self, row: usize) {
        if let Some(tree) = &mut self.tree {
            self.selected = tree.toggle(row, self.selected);
        }
    }

    fn open_dir(&mut self) {
        if let Some(tree) = &mut self.tree {
            let row = tree.open(self.selected);
            self.select(row);
        }
    }

    fn fold_dir(&mut self) {
        if let Some(tree) = &mut self.tree {
            let row = tree.fold(self.selected);
            self.select(row);
        }
    }

    /// How many rows the selected file's diff has.
    fn row_count(&self) -> usize {
        self.file()
            .map_or(0, |file| diff::rows(file, self.layout()).len())
    }

    /// How many rows of the diff are on screen: all but the file's title.
    fn visible_rows(&self) -> usize {
        usize::from(self.size.0.saturating_sub(1)).max(1)
    }

    /// A page: what's on screen, less a row kept from the last.
    fn page(&self) -> usize {
        self.visible_rows().saturating_sub(1).max(1)
    }

    /// The furthest down the diff scrolls: its last row at the bottom.
    fn last_scroll(&self) -> usize {
        self.row_count().saturating_sub(self.visible_rows())
    }

    fn scroll_down(&mut self, rows: usize) {
        self.scroll = (self.scroll + rows).min(self.last_scroll());
    }

    fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    /// Where each hunk starts, among the selected file's rows.
    fn hunk_starts(&self) -> Vec<usize> {
        let Some(file) = self.file() else {
            return Vec::new();
        };
        diff::rows(file, self.layout())
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, Row::Hunk(_)))
            .map(|(index, _)| index)
            .collect()
    }

    fn next_hunk(&mut self) {
        let next = self.hunk_starts().into_iter().find(|&at| at > self.scroll);
        if let Some(at) = next {
            self.scroll = at.min(self.last_scroll());
        }
    }

    fn previous_hunk(&mut self) {
        let previous = self
            .hunk_starts()
            .into_iter()
            .rev()
            .find(|&at| at < self.scroll);
        if let Some(at) = previous {
            self.scroll = at;
        }
    }

    /// Switches between unified and side by side, starting the file over:
    /// the two have different rows.
    fn toggle_layout(&mut self) {
        self.wanted = match self.wanted {
            Layout::Unified => Layout::SideBySide,
            Layout::SideBySide => Layout::Unified,
        };
        self.scroll = 0;
    }

    /// Switches what the diff compares with, and asks for it to be read.
    /// A pull request's diff is only ever that.
    fn toggle_against(&mut self) -> Outcome {
        self.against = match self.against {
            Against::Uncommitted => Against::Branch,
            Against::Branch => Against::Uncommitted,
            Against::PullRequest { .. } => return Outcome::Stay,
        };
        self.diff = Loading::Reading;
        self.tree = None;
        self.selected = 0;
        self.scroll = 0;
        Outcome::Do(self.read())
    }
}

/// How wide the list of files is, for a view `width` columns wide.
pub fn list_width(width: u16) -> u16 {
    (width * 3 / 10).clamp(24, 44).min(width / 2)
}

/// The keys the footer shows while the diff is open.
pub fn hints(view: &DiffView) -> Vec<(&'static str, &'static str)> {
    let layout = match view.wanted {
        Layout::Unified => "side by side",
        Layout::SideBySide => "unified",
    };
    let mut hints = vec![
        ("j/k", "file"),
        ("space", "page"),
        ("]/[", "hunk"),
        ("v", layout),
    ];
    match view.against {
        Against::Uncommitted => hints.push(("b", "branch")),
        Against::Branch => hints.push(("b", "uncommitted")),
        Against::PullRequest { .. } => {}
    }
    hints.push(("r", "reviewed"));
    if view.as_tree {
        hints.push(("t", "flat"));
        hints.push(("←/→", "fold"));
    } else {
        hints.push(("t", "tree"));
    }
    hints.push(("esc", "close"));
    hints
}

/// Which row of the list is on screen `row`, in a list drawn in `area`.
pub fn list_hit(view: &DiffView, area: Rect, row: u16) -> Hit {
    let index = list_offset(view.selected, area.height) + usize::from(row - area.y);
    if index < view.entries().len() {
        Hit::ViewList(index)
    } else {
        Hit::ViewContent
    }
}

/// The first row on screen: the list scrolls to keep the selection in
/// sight. Drawing and clicking both go by this.
fn list_offset(selected: usize, height: u16) -> usize {
    let height = usize::from(height.max(1));
    (selected + 1).saturating_sub(height)
}

pub fn draw(frame: &mut Frame, view: &DiffView, look: &Look, areas: &ViewAreas) {
    let theme = look.theme;
    frame.render_widget(header(view, look, areas.header.width), areas.header);
    ui::draw_rule(frame, look, areas.rule);
    let files = match &view.diff {
        Loading::Reading => {
            ui::draw_message(frame, look, "reading the diff…", areas.content);
            return;
        }
        Loading::Failed(err) => {
            let line = Line::styled(format!(" {err}"), Style::new().fg(theme.failed));
            frame.render_widget(Paragraph::new(line), areas.content);
            return;
        }
        Loading::Read(read) => &read.files,
    };
    if files.is_empty() {
        let message = match view.against {
            Against::Uncommitted => "no changes since the last commit",
            Against::Branch => "nothing committed on this branch yet",
            Against::PullRequest { .. } => "it changes no files",
        };
        ui::draw_message(frame, look, message, areas.content);
        return;
    }
    draw_list(frame, view, look, areas.list);
    match view.selected_entry() {
        Some(Entry::File(file)) => draw_file(frame, view, &files[file], look, areas.content),
        Some(Entry::Dir(node)) => draw_dir(frame, view, node, look, areas.content),
        None => {}
    }
}

/// "± uncommitted changes · 3 files +41 −9 · 1 reviewed", and where on the
/// right.
fn header<'a>(view: &DiffView, look: &Look, width: u16) -> Line<'a> {
    let title = match (&view.against, &view.diff) {
        (Against::Uncommitted, _) => "uncommitted changes".to_string(),
        (
            Against::Branch,
            Loading::Read(Read {
                base: Some(base), ..
            }),
        ) => {
            format!("the branch since {base}")
        }
        (Against::Branch, _) => "the branch".to_string(),
        (Against::PullRequest { forge, number }, _) => {
            format!("{} {}", forge.pull_request(), forge.label(*number))
        }
    };
    let mut notes = Vec::new();
    let files = view.files();
    if !files.is_empty() {
        notes.push(count_line(files));
    }
    let reviewed = view.reviewed_count();
    if reviewed > 0 {
        notes.push(format!("{reviewed} reviewed"));
    }
    ui::view_header("±", &title, &notes, &view.place, look, width)
}

/// "3 files +41 −9".
fn count_line<'f>(files: impl IntoIterator<Item = &'f FileDiff>) -> String {
    let (count, added, removed) = files
        .into_iter()
        .fold((0, 0, 0), |(count, added, removed), file| {
            (count + 1, added + file.added, removed + file.removed)
        });
    let noun = if count == 1 { "file" } else { "files" };
    format!("{count} {noun} +{added} −{removed}")
}

/// The list's rows: the changed files, or, as a tree, its directories and
/// files.
fn draw_list(frame: &mut Frame, view: &DiffView, look: &Look, area: Rect) {
    let first = list_offset(view.selected, area.height);
    let entries = view.entries();
    let shown = entries
        .iter()
        .enumerate()
        .skip(first)
        .take(area.height.into());
    for (index, entry) in shown {
        let y = area.y + (index - first) as u16;
        let row = Rect::new(area.x, y, area.width, 1);
        let selected = index == view.selected;
        if selected {
            frame.buffer_mut().set_style(row, look.theme.selection);
        }
        let Some(mut list_row) = list_row(view, index, *entry, look.theme) else {
            continue;
        };
        if selected {
            list_row.name_style = list_row.name_style.add_modifier(Modifier::BOLD);
        }
        frame.render_widget(list_line(list_row, look.theme, area.width), row);
    }
}

/// What a row of the list shows.
struct ListRow {
    /// Columns in from the left: two a level, in the tree.
    indent: usize,
    /// A file's status letter, or ✓ once it's reviewed; a directory's
    /// fold.
    mark: Span<'static>,
    name: String,
    name_style: Style,
    /// After the name, muted: a file's directory, in the flat list.
    after: String,
    /// After that: a directory's ✓, once every file under it is reviewed.
    reviewed: bool,
    added: usize,
    removed: usize,
}

/// The row `entry`, on row `index` of the list, shows: a file's, flat or
/// in the tree, or a directory's.
fn list_row(view: &DiffView, index: usize, entry: Entry, theme: &Theme) -> Option<ListRow> {
    let tree = view.tree();
    let depth = tree.map(|tree| tree.node(tree.rows()[index]).depth);
    let row = match entry {
        Entry::Dir(node) => {
            let tree = tree?;
            let dir = tree.node(node);
            let open = matches!(dir.kind, Kind::Dir { open: true, .. });
            let (added, removed) =
                tree.files_under(node)
                    .iter()
                    .fold((0, 0), |(added, removed), file| {
                        let file = &view.files()[*file];
                        (added + file.added, removed + file.removed)
                    });
            ListRow {
                indent: dir.depth * 2,
                mark: Span::styled(if open { "▾" } else { "▸" }, Style::new().fg(theme.muted)),
                name: dir.name.clone(),
                name_style: Style::new().fg(theme.text),
                after: String::new(),
                reviewed: view.all_reviewed(node),
                added,
                removed,
            }
        }
        Entry::File(at) => {
            let file = &view.files()[at];
            let (directory, name) = match file.path.rsplit_once('/') {
                Some((directory, name)) => (directory, name),
                None => ("", file.path.as_str()),
            };
            let muted = view.is_reviewed(at);
            ListRow {
                indent: depth.unwrap_or(0) * 2,
                mark: file_mark(view, at, theme),
                // In the tree, the directory is the row above.
                name: name.to_string(),
                name_style: Style::new().fg(if muted { theme.muted } else { theme.text }),
                after: match depth {
                    Some(_) => String::new(),
                    None => directory.to_string(),
                },
                reviewed: false,
                added: file.added,
                removed: file.removed,
            }
        }
    };
    Some(row)
}

/// A file's status letter, or a ✓ once it's reviewed.
fn file_mark(view: &DiffView, file: usize, theme: &Theme) -> Span<'static> {
    let status = view.files()[file].status;
    if view.is_reviewed(file) {
        Span::styled("✓", Style::new().fg(theme.added))
    } else {
        Span::styled(
            status.letter().to_string(),
            Style::new().fg(status_color(status, theme)),
        )
    }
}

/// A row of the list, fitted to `width` columns: the mark, the name and
/// what's after it, and on the right how many lines it adds and removes.
fn list_line<'a>(row: ListRow, theme: &Theme, width: u16) -> Line<'a> {
    let counts = format!("+{} −{}", row.added, row.removed);
    let check = if row.reviewed { " ✓" } else { "" };
    // A space, the indent, the mark and a space before the name; a space
    // before and after the counts.
    let before = 1 + row.indent + 1 + 1;
    let room = usize::from(width)
        .saturating_sub(before + counts.chars().count() + 2 + check.chars().count());
    let name = fit(&row.name, room);
    let after = fit(&row.after, room.saturating_sub(name.chars().count() + 1));
    let used = name.chars().count() + usize::from(!after.is_empty()) + after.chars().count();
    let gap = room.saturating_sub(used);

    let mut spans = vec![
        Span::raw(" ".repeat(1 + row.indent)),
        row.mark,
        Span::raw(" "),
        Span::styled(name, row.name_style),
    ];
    if !after.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(after, Style::new().fg(theme.muted)));
    }
    if row.reviewed {
        spans.push(Span::styled(check, Style::new().fg(theme.added)));
    }
    spans.push(Span::raw(" ".repeat(gap + 1)));
    spans.push(Span::styled(
        format!("+{}", row.added),
        Style::new().fg(theme.added),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("−{}", row.removed),
        Style::new().fg(theme.removed),
    ));
    Line::from(spans)
}

/// The color of a file's status letter: green for what's new, red for
/// what's gone, and the theme's own for the rest.
fn status_color(status: FileStatus, theme: &Theme) -> ratatui::style::Color {
    match status {
        FileStatus::Added | FileStatus::Untracked => theme.added,
        FileStatus::Deleted => theme.removed,
        FileStatus::Renamed => theme.accent,
        FileStatus::Modified => theme.working,
    }
}

/// The selected file: its path on the first row, then its diff from where
/// it's scrolled to.
fn draw_file(frame: &mut Frame, view: &DiffView, file: &FileDiff, look: &Look, area: Rect) {
    let title = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(file_title(view, file, look.theme), title);
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    match &file.body {
        Body::Binary => ui::draw_message(frame, look, "a binary file: no lines to show", body),
        Body::TooBig => {
            let lines = file.added + file.removed;
            let message = format!("{lines} changed lines: too many to show here");
            ui::draw_message(frame, look, &message, body);
        }
        Body::Hunks(_) => draw_rows(frame, view, file, look, body),
    }
}

/// A directory's row in the tree: what changed under it, a file a row,
/// each with its status, or ✓, and how many lines it adds and removes.
fn draw_dir(frame: &mut Frame, view: &DiffView, node: usize, look: &Look, area: Rect) {
    let Some(tree) = view.tree() else {
        return;
    };
    let theme = look.theme;
    let dir = tree.node(node);
    let files = tree.files_under(node);
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{}/", dir.path),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " · {}",
                count_line(files.iter().map(|file| &view.files()[*file]))
            ),
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(title, Rect::new(area.x, area.y, area.width, 1));
    let inside = format!("{}/", dir.path);
    for (at, index) in files
        .iter()
        .take(area.height.saturating_sub(2).into())
        .enumerate()
    {
        let file = &view.files()[*index];
        let reviewed = view.is_reviewed(*index);
        let row = ListRow {
            indent: 2,
            mark: file_mark(view, *index, theme),
            name: file
                .path
                .strip_prefix(&inside)
                .unwrap_or(&file.path)
                .to_string(),
            name_style: Style::new().fg(if reviewed { theme.muted } else { theme.text }),
            after: String::new(),
            reviewed: false,
            added: file.added,
            removed: file.removed,
        };
        let line_area = Rect::new(area.x, area.y + 2 + at as u16, area.width, 1);
        frame.render_widget(list_line(row, theme, area.width), line_area);
    }
}

/// The file's path, where a renamed file came from, and a word when side
/// by side was asked for but doesn't fit.
fn file_title<'a>(view: &DiffView, file: &FileDiff, theme: &Theme) -> Line<'a> {
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            file.path.clone(),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(old) = &file.old_path {
        spans.push(Span::styled(
            format!(" ← {old}"),
            Style::new().fg(theme.muted),
        ));
    }
    if view.too_narrow() {
        spans.push(Span::styled(
            " · unified: too narrow for side by side",
            Style::new().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

fn draw_rows(frame: &mut Frame, view: &DiffView, file: &FileDiff, look: &Look, area: Rect) {
    let rows = diff::rows(file, view.layout());
    let numbers = diff::number_width(file);
    let shown = rows.iter().skip(view.scroll).take(area.height.into());
    for (index, row) in shown.enumerate() {
        let y = area.y + index as u16;
        let line_area = Rect::new(area.x, y, area.width, 1);
        match row {
            Row::Hunk(hunk) => {
                let style = Style::new().fg(look.theme.muted).bg(look.theme.panel);
                frame.buffer_mut().set_style(line_area, style);
                let text = format!(" @@ {} @@ {}", hunk.range, hunk.context);
                frame.render_widget(Line::styled(text, style), line_area);
            }
            Row::Line(line) => draw_unified(frame, line, numbers, look.theme, line_area),
            Row::Pair(old, new) => draw_pair(frame, *old, *new, numbers, look.theme, line_area),
        }
    }
}

/// A line of the unified layout: both its numbers, its sign, and its text,
/// on its tint.
fn draw_unified(frame: &mut Frame, line: &DiffLine, numbers: usize, theme: &Theme, area: Rect) {
    let tint = line_style(line.kind, theme);
    frame.buffer_mut().set_style(area, tint);
    let number = |number: Option<usize>| match number {
        Some(number) => format!("{number:>numbers$}"),
        None => " ".repeat(numbers),
    };
    let sign = match line.kind {
        LineKind::Context => ' ',
        LineKind::Removed => '-',
        LineKind::Added => '+',
    };
    let mut spans = vec![Span::styled(
        format!(" {} {} {sign} ", number(line.old), number(line.new)),
        tint.fg(theme.muted),
    )];
    spans.extend(text_spans(line, tint, words_style(line.kind, theme)));
    frame.render_widget(Line::from(spans), area);
}

/// A row of the side-by-side layout: the old file's line on the left and
/// the new file's on the right, with a rule between. A side with no line
/// is shaded.
fn draw_pair(
    frame: &mut Frame,
    old: Option<&DiffLine>,
    new: Option<&DiffLine>,
    numbers: usize,
    theme: &Theme,
    area: Rect,
) {
    let half = area.width.saturating_sub(1) / 2;
    let left = Rect::new(area.x, area.y, half, 1);
    let rule = Rect::new(area.x + half, area.y, 1, 1);
    let right = Rect::new(area.x + half + 1, area.y, area.width - half - 1, 1);
    draw_side(frame, old, |line| line.old, numbers, theme, left);
    frame.render_widget(Line::styled("│", Style::new().fg(theme.rule)), rule);
    draw_side(frame, new, |line| line.new, numbers, theme, right);
}

fn draw_side(
    frame: &mut Frame,
    line: Option<&DiffLine>,
    number: fn(&DiffLine) -> Option<usize>,
    numbers: usize,
    theme: &Theme,
    area: Rect,
) {
    let Some(line) = line else {
        frame
            .buffer_mut()
            .set_style(area, Style::new().bg(theme.panel));
        return;
    };
    let tint = line_style(line.kind, theme);
    frame.buffer_mut().set_style(area, tint);
    let number = number(line).map_or(String::new(), |n| n.to_string());
    let mut spans = vec![Span::styled(
        format!(" {number:>numbers$} "),
        tint.fg(theme.muted),
    )];
    spans.extend(text_spans(line, tint, words_style(line.kind, theme)));
    frame.render_widget(Line::from(spans), area);
}

/// The tint a line is drawn on, by whether it's added, removed or neither.
fn line_style(kind: LineKind, theme: &Theme) -> Style {
    match kind {
        LineKind::Context => Style::new().fg(theme.text),
        LineKind::Removed => Style::new().fg(theme.text).patch(theme.removed_line),
        LineKind::Added => Style::new().fg(theme.text).patch(theme.added_line),
    }
}

/// How the changed words in a line of `kind` are drawn.
fn words_style(kind: LineKind, theme: &Theme) -> Style {
    match kind {
        LineKind::Removed => theme.removed_words,
        _ => theme.added_words,
    }
}

/// A line's text as spans: the changed words in `words`, laid over the
/// line's `tint`, the rest in the tint alone. Tabs become spaces.
fn text_spans<'a>(line: &DiffLine, tint: Style, words: Style) -> Vec<Span<'a>> {
    pieces(&line.text, &line.changed)
        .into_iter()
        .map(|(text, changed)| {
            let style = if changed { tint.patch(words) } else { tint };
            Span::styled(text.replace('\t', TAB), style)
        })
        .collect()
}

/// `text` cut where the `changed` ranges start and end, each piece with
/// whether it's changed.
pub fn pieces<'t>(text: &'t str, changed: &[Range<usize>]) -> Vec<(&'t str, bool)> {
    let mut pieces = Vec::new();
    let mut at = 0;
    for range in changed {
        let (start, end) = (range.start.min(text.len()), range.end.min(text.len()));
        if start > at {
            pieces.push((&text[at..start], false));
        }
        if end > start {
            pieces.push((&text[start..end], true));
        }
        at = at.max(end);
    }
    if at < text.len() {
        pieces.push((&text[at..], false));
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::diff::Hunk;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn line(kind: LineKind, number: usize) -> DiffLine {
        DiffLine {
            kind,
            old: (kind != LineKind::Added).then_some(number),
            new: (kind != LineKind::Removed).then_some(number),
            text: format!("line {number}"),
            changed: Vec::new(),
        }
    }

    /// A file with `hunks` hunks of ten context lines each.
    fn file(path: &str, hunks: usize) -> FileDiff {
        let hunks = (0..hunks)
            .map(|hunk| Hunk {
                range: format!("-{0},10 +{0},10", hunk * 100),
                context: String::new(),
                lines: (0..10)
                    .map(|n| line(LineKind::Context, hunk * 100 + n))
                    .collect(),
            })
            .collect();
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            added: 0,
            removed: 0,
            body: Body::Hunks(hunks),
            hash: 0,
        }
    }

    fn read(files: Vec<FileDiff>, reviewed: Marks) -> Read {
        Read {
            files,
            base: None,
            head: "c1".into(),
            reviewed,
        }
    }

    fn view_with(files: Vec<FileDiff>) -> DiffView {
        view_reviewed(files, &[], false)
    }

    /// A view of `files`, those named in `reviewed` kept as reviewed, as a
    /// tree if `as_tree`.
    fn view_reviewed(files: Vec<FileDiff>, reviewed: &[&str], as_tree: bool) -> DiffView {
        let mut view = DiffView::new(PathBuf::from("/code/app"), "app ⌂ main".into(), as_tree);
        let marks = reviewed
            .iter()
            .map(|path| {
                (
                    path.to_string(),
                    files.iter().find(|file| file.path == *path).unwrap().hash,
                )
            })
            .collect();
        view.read_done(
            Path::new("/code/app"),
            Against::Uncommitted,
            Ok(read(files, marks)),
        );
        // Twelve rows: the title and eleven rows of diff.
        view.set_size((12, 100));
        view
    }

    /// The paths of the list's rows, top to bottom, a directory's ending
    /// in `/`.
    fn listed(view: &DiffView) -> Vec<String> {
        view.entries()
            .iter()
            .map(|entry| match entry {
                Entry::File(file) => view.files()[*file].path.clone(),
                Entry::Dir(node) => format!("{}/", view.tree().unwrap().node(*node).path),
            })
            .collect()
    }

    fn selected_path(view: &DiffView) -> Option<String> {
        view.file().map(|file| file.path.clone())
    }

    #[test]
    fn j_and_k_move_through_the_files_from_the_top_of_each() {
        let mut view = view_with(vec![file("a.rs", 3), file("b.rs", 1)]);
        view.on_key(key(KeyCode::Char(' ')));
        assert!(view.scroll > 0);
        view.on_key(key(KeyCode::Char('j')));
        assert_eq!((view.selected, view.scroll), (1, 0));
        view.on_key(key(KeyCode::Char('j')));
        assert_eq!(view.selected, 1, "the last file stays selected");
        view.on_key(key(KeyCode::Char('k')));
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn space_pages_down_and_stops_at_the_end() {
        // Three hunks: 33 rows, 11 on screen.
        let mut view = view_with(vec![file("a.rs", 3)]);
        view.on_key(key(KeyCode::Char(' ')));
        assert_eq!(view.scroll, 10);
        for _ in 0..5 {
            view.on_key(key(KeyCode::Char(' ')));
        }
        assert_eq!(view.scroll, 33 - 11);
        view.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::SHIFT));
        assert_eq!(view.scroll, 33 - 11 - 10);
    }

    #[test]
    fn brackets_go_from_hunk_to_hunk() {
        let mut view = view_with(vec![file("a.rs", 3)]);
        view.on_key(key(KeyCode::Char(']')));
        assert_eq!(view.scroll, 11);
        view.on_key(key(KeyCode::Char(']')));
        // The third hunk starts at 22, the furthest it can scroll.
        assert_eq!(view.scroll, 22);
        view.on_key(key(KeyCode::Char('[')));
        assert_eq!(view.scroll, 11);
    }

    #[test]
    fn side_by_side_needs_room() {
        let mut view = view_with(vec![file("a.rs", 1)]);
        view.on_key(key(KeyCode::Char('v')));
        assert_eq!(view.layout(), Layout::Unified);
        assert!(view.too_narrow());
        view.set_size((12, SIDE_BY_SIDE_WIDTH));
        assert_eq!(view.layout(), Layout::SideBySide);
        view.on_key(key(KeyCode::Char('v')));
        assert_eq!(view.layout(), Layout::Unified);
        assert!(!view.too_narrow());
    }

    #[test]
    fn b_asks_for_the_branch_and_drops_an_older_read() {
        let mut view = view_with(vec![file("a.rs", 1)]);
        let outcome = view.on_key(key(KeyCode::Char('b')));
        let wanted = Action::ReadDiff {
            dir: PathBuf::from("/code/app"),
            against: Against::Branch,
        };
        assert_eq!(outcome, Outcome::Do(wanted));
        assert_eq!(view.diff, Loading::Reading);

        // The uncommitted diff, read before the switch, comes too late.
        let late = read(vec![file("old.rs", 1)], Marks::new());
        view.read_done(Path::new("/code/app"), Against::Uncommitted, Ok(late));
        assert_eq!(view.diff, Loading::Reading);
    }

    #[test]
    fn esc_and_q_close_it() {
        let mut view = view_with(vec![]);
        assert_eq!(view.on_key(key(KeyCode::Esc)), Outcome::Close);
        assert_eq!(view.on_key(key(KeyCode::Char('q'))), Outcome::Close);
    }

    #[test]
    fn the_wheel_scrolls_the_diff_and_moves_through_the_list() {
        let mut view = view_with(vec![file("a.rs", 3), file("b.rs", 1)]);
        view.on_mouse(MouseEventKind::ScrollDown, Hit::ViewContent);
        assert_eq!(view.scroll, WHEEL_ROWS);
        view.on_mouse(MouseEventKind::ScrollDown, Hit::ViewList(0));
        assert_eq!(view.selected, 1);
        view.on_mouse(MouseEventKind::Down(MouseButton::Left), Hit::ViewList(0));
        assert_eq!(view.selected, 0);
    }

    /// A file whose hash is `hash`.
    fn changed(path: &str, hash: u64) -> FileDiff {
        FileDiff {
            hash,
            ..file(path, 1)
        }
    }

    #[test]
    fn r_marks_a_file_reviewed_and_sinks_it_reading_on() {
        let mut view = view_with(vec![file("a.rs", 1), file("b.rs", 1), file("c.rs", 1)]);
        let outcome = view.on_key(key(KeyCode::Char('r')));
        let Outcome::Do(Action::KeepReviewed { scope, marks }) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(
            scope,
            super::scope(Path::new("/code/app"), Against::Uncommitted, "c1")
        );
        assert_eq!(marks.keys().collect::<Vec<_>>(), ["a.rs"]);
        assert_eq!(listed(&view), ["b.rs", "c.rs", "a.rs"]);
        // The selection stays put, on the next file to read.
        assert_eq!(selected_path(&view).as_deref(), Some("b.rs"));
        view.on_key(key(KeyCode::Char('r')));
        view.on_key(key(KeyCode::Char('r')));
        // The last one marked stays selected: there's no other to read.
        assert_eq!(listed(&view), ["a.rs", "b.rs", "c.rs"]);
        assert_eq!(selected_path(&view).as_deref(), Some("c.rs"));
        assert_eq!(view.reviewed_count(), 3);

        // Taking marks off from the top of the reviewed goes on to the next.
        view.on_key(key(KeyCode::Char('k')));
        view.on_key(key(KeyCode::Char('k')));
        view.on_key(key(KeyCode::Char('r')));
        assert_eq!(listed(&view), ["a.rs", "b.rs", "c.rs"]);
        assert_eq!(selected_path(&view).as_deref(), Some("b.rs"));
        assert_eq!(view.reviewed_count(), 2);
    }

    #[test]
    fn a_mark_holds_only_while_its_file_is_unchanged() {
        let files = vec![changed("a.rs", 1), changed("b.rs", 2)];
        let mut view = DiffView::new(PathBuf::from("/code/app"), "app".into(), false);
        let kept = Marks::from([("a.rs".to_string(), 1), ("b.rs".to_string(), 99)]);
        view.read_done(
            Path::new("/code/app"),
            Against::Uncommitted,
            Ok(read(files, kept)),
        );
        assert!(view.is_reviewed(0));
        assert!(!view.is_reviewed(1), "b.rs changed again since");
        // Reading starts at the first file still to read.
        assert_eq!(selected_path(&view).as_deref(), Some("b.rs"));
    }

    #[test]
    fn t_folds_the_list_into_a_tree_keeping_the_selected_file() {
        let mut view = view_with(vec![
            file("README.md", 1),
            file("src/tui/app.rs", 1),
            file("src/tui/ui.rs", 1),
        ]);
        view.on_key(key(KeyCode::Char('j')));
        view.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            view.on_key(key(KeyCode::Char('t'))),
            Outcome::Do(Action::KeepTree(true))
        );
        assert_eq!(
            listed(&view),
            ["src/tui/", "src/tui/app.rs", "src/tui/ui.rs", "README.md"]
        );
        assert_eq!(selected_path(&view).as_deref(), Some("src/tui/ui.rs"));

        // ← goes up to the directory, which shows no file's diff, and
        // Enter folds it.
        view.on_key(key(KeyCode::Left));
        assert_eq!(view.selected_entry(), Some(Entry::Dir(0)));
        assert_eq!(view.file(), None);
        view.on_key(key(KeyCode::Enter));
        assert_eq!(listed(&view), ["src/tui/", "README.md"]);

        // Back to the list from a directory's row: its first file.
        assert_eq!(
            view.on_key(key(KeyCode::Char('t'))),
            Outcome::Do(Action::KeepTree(false))
        );
        assert_eq!(selected_path(&view).as_deref(), Some("src/tui/app.rs"));
    }

    #[test]
    fn in_the_tree_marking_goes_on_to_the_next_file_and_a_directory_follows_its_files() {
        let mut view = view_reviewed(
            vec![file("src/a.rs", 1), file("src/b.rs", 1), file("top.rs", 1)],
            &[],
            true,
        );
        assert_eq!(selected_path(&view).as_deref(), Some("src/a.rs"));
        view.on_key(key(KeyCode::Char('r')));
        // Nothing sinks in the tree.
        assert_eq!(listed(&view), ["src/", "src/a.rs", "src/b.rs", "top.rs"]);
        assert_eq!(selected_path(&view).as_deref(), Some("src/b.rs"));
        assert!(!view.all_reviewed(0));
        view.on_key(key(KeyCode::Char('r')));
        assert!(view.all_reviewed(0));
        assert_eq!(selected_path(&view).as_deref(), Some("top.rs"));
    }

    #[test]
    fn a_pull_requests_marks_are_its_own() {
        let forge = Forge::GitHub;
        let pull = super::scope(
            Path::new("/code/app"),
            Against::PullRequest { forge, number: 57 },
            "",
        );
        let worktree = super::scope(Path::new("/code/app"), Against::Uncommitted, "c1");
        assert_ne!(pull, worktree);
        assert_eq!(pull.diff, "pull request 57");
    }

    #[test]
    fn a_line_is_cut_where_its_words_changed() {
        let the_one = Range { start: 12, end: 13 };
        assert_eq!(
            pieces("let total = 1;", &[the_one]),
            [("let total = ", false), ("1", true), (";", false)]
        );
        assert_eq!(pieces("same", &[]), [("same", false)]);
    }
}
