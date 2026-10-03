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

use super::app::{Action, Hit, Loading, Outcome};
use super::diff::{self, Body, DiffLine, FileDiff, FileStatus, Layout, LineKind, Row};
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
}

/// Reads the diff of the worktree at `dir`. It runs git, which can take a
/// while, so it's run off the event loop.
pub fn read(dir: &Path, against: Against) -> Result<Read, String> {
    let read = match against {
        Against::Uncommitted => read_uncommitted(dir),
        Against::Branch => read_branch(dir),
        Against::PullRequest { number, .. } => return read_pull_request(dir, number),
    };
    read.map_err(|err| format!("{err:#}"))
}

/// What pull request `number` of the project at `dir` changes, from its
/// forge, which goes over the network.
fn read_pull_request(dir: &Path, number: u64) -> Result<Read, String> {
    let patch = Repo::find(dir)?.diff(number)?;
    Ok(Read {
        files: diff::parse(&patch),
        base: None,
    })
}

fn read_uncommitted(dir: &Path) -> anyhow::Result<Read> {
    let mut files = diff::parse(&git::uncommitted_patch(dir)?);
    for path in git::untracked_files(dir)? {
        // Read as much as the diff could ever show, and not much more.
        let content = read_start(&dir.join(&path), 1 << 20);
        files.push(diff::new_file(&path, &content));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Read { files, base: None })
}

fn read_branch(dir: &Path) -> anyhow::Result<Read> {
    let (base, commit) = git::branch_start(dir)?;
    let files = diff::parse(&git::patch_since(dir, &commit)?);
    Ok(Read {
        files,
        base: Some(base),
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
    /// The selected file, by its place in the list.
    pub selected: usize,
    /// How many rows down the selected file's diff is scrolled.
    pub scroll: usize,
    /// The size of the area the diff is drawn in, as `(rows, columns)`.
    size: (u16, u16),
}

impl DiffView {
    /// The uncommitted changes of the worktree at `dir`, until they're read.
    pub fn new(dir: PathBuf, place: String) -> DiffView {
        DiffView {
            dir,
            place,
            against: Against::Uncommitted,
            wanted: Layout::Unified,
            diff: Loading::Reading,
            selected: 0,
            scroll: 0,
            size: (24, 80),
        }
    }

    /// What pull request `number` changes, the project at `dir` being on
    /// `forge`, until it's read.
    pub fn of_pull_request(dir: PathBuf, place: String, forge: Forge, number: u64) -> DiffView {
        DiffView {
            against: Against::PullRequest { forge, number },
            ..DiffView::new(dir, place)
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
    /// view now wants.
    pub fn read_done(&mut self, dir: &Path, against: Against, read: Result<Read, String>) {
        if dir != self.dir || against != self.against {
            return;
        }
        self.diff = match read {
            Ok(read) => Loading::Read(read),
            Err(err) => Loading::Failed(err),
        };
        self.selected = 0;
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

    pub fn file(&self) -> Option<&FileDiff> {
        self.files().get(self.selected)
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
            _ => {}
        }
        Outcome::Stay
    }

    /// The wheel scrolls the diff, or, over the list, moves through the
    /// files; a click on the list selects a file.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(index)) => self.select(index),
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

    /// Selects the file at `index`, kept in range, from the top of its diff.
    fn select(&mut self, index: usize) {
        let last = self.files().len().saturating_sub(1);
        let index = index.min(last);
        if index != self.selected {
            self.selected = index;
            self.scroll = 0;
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
    hints.push(("esc", "close"));
    hints
}

/// Which file's row is on screen `row`, in a list drawn in `area`.
pub fn list_hit(view: &DiffView, area: Rect, row: u16) -> Hit {
    let index = list_offset(view.selected, area.height) + usize::from(row - area.y);
    if index < view.files().len() {
        Hit::ViewList(index)
    } else {
        Hit::ViewContent
    }
}

/// The first file on screen: the list scrolls to keep the selection in
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
    draw_list(frame, view, files, look, areas.list);
    if let Some(file) = files.get(view.selected) {
        draw_file(frame, view, file, look, areas.content);
    }
}

/// "± uncommitted changes · 3 files +41 −9", and where on the right.
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
        let count = files.len();
        let noun = if count == 1 { "file" } else { "files" };
        let added: usize = files.iter().map(|file| file.added).sum();
        let removed: usize = files.iter().map(|file| file.removed).sum();
        notes.push(format!("{count} {noun} +{added} −{removed}"));
    }
    ui::view_header("±", &title, &notes, &view.place, look, width)
}

/// The changed files, one a row: the status letter, the file's name, its
/// directory muted after it, and on the right how many lines it adds and
/// removes.
fn draw_list(frame: &mut Frame, view: &DiffView, files: &[FileDiff], look: &Look, area: Rect) {
    let first = list_offset(view.selected, area.height);
    let shown = files
        .iter()
        .enumerate()
        .skip(first)
        .take(area.height.into());
    for (index, file) in shown {
        let y = area.y + (index - first) as u16;
        let row = Rect::new(area.x, y, area.width, 1);
        let selected = index == view.selected;
        if selected {
            frame.buffer_mut().set_style(row, look.theme.selection);
        }
        frame.render_widget(file_line(file, look.theme, area.width, selected), row);
    }
}

/// One file's row in the list, fitted to `width` columns.
fn file_line<'a>(file: &FileDiff, theme: &Theme, width: u16, selected: bool) -> Line<'a> {
    let counts = format!("+{} −{}", file.added, file.removed);
    let (directory, name) = match file.path.rsplit_once('/') {
        Some((directory, name)) => (directory, name),
        None => ("", file.path.as_str()),
    };
    // A space, the letter and a space before the name; a space before and
    // after the counts.
    let room = usize::from(width).saturating_sub(3 + counts.chars().count() + 2);
    let name = fit(name, room);
    let directory = fit(directory, room.saturating_sub(name.chars().count() + 1));
    let used =
        name.chars().count() + usize::from(!directory.is_empty()) + directory.chars().count();
    let gap = room.saturating_sub(used);

    let mut name_style = Style::new().fg(theme.text);
    if selected {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    let (added, removed) = (file.added.to_string(), file.removed.to_string());
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            file.status.letter().to_string(),
            Style::new().fg(status_color(file.status, theme)),
        ),
        Span::raw(" "),
        Span::styled(name, name_style),
    ];
    if !directory.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(directory, Style::new().fg(theme.muted)));
    }
    spans.push(Span::raw(" ".repeat(gap + 1)));
    spans.push(Span::styled(
        format!("+{added}"),
        Style::new().fg(theme.added),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("−{removed}"),
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
        }
    }

    fn view_with(files: Vec<FileDiff>) -> DiffView {
        let mut view = DiffView::new(PathBuf::from("/code/app"), "app ⌂ main".into());
        let read = Read { files, base: None };
        view.read_done(Path::new("/code/app"), Against::Uncommitted, Ok(read));
        // Twelve rows: the title and eleven rows of diff.
        view.set_size((12, 100));
        view
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
        let late = Read {
            files: vec![file("old.rs", 1)],
            base: None,
        };
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
