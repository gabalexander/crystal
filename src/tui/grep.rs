//! Find in files, `G` in the sidebar: `git grep` over a session's worktree
//! as you type, through the files git tracks and the new ones it would,
//! with the lines found listed under their files and the lines around the
//! selected one beside them. Enter opens the file in the user's `$EDITOR`
//! at that line, as a session of its own, the way the file finder does.
//!
//! The search runs off the event loop, a moment after the typing stops,
//! and stops early when the query has moved on, or once it has found
//! [`MOST_HITS`].

use super::app::{Action, Hit, Outcome};
use super::diff_view::pieces;
use super::preview;
use super::sidebar::fit;
use super::text_input::TextInput;
use super::ui::{self, Look, ViewAreas};
use crate::git::{self, Found};
use crate::syntax::Runs;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// A query shorter than this isn't searched for: a letter is in half the
/// lines of any project.
pub const SHORTEST: usize = 2;

/// How many lines a search finds, at most: more is past what anyone reads.
pub const MOST_HITS: usize = 500;

/// How much of a file the preview reads.
const FILE_BYTES: u64 = 4 << 20;

/// The rows above the list: the query, and a rule under it.
const QUERY_ROWS: u16 = 2;

/// Searches the worktree at `dir` for `query`: run off the event loop.
/// `None` when `stale` stopped it.
pub fn search(dir: &Path, query: &str, stale: &dyn Fn() -> bool) -> Option<Result<Found, String>> {
    match git::grep(dir, query, MOST_HITS, stale) {
        Ok(found) => found.map(Ok),
        Err(err) => Some(Err(format!("{err:#}"))),
    }
}

/// The lines of the file at `path` in the worktree at `dir`, highlighted,
/// for the preview: run off the event loop.
pub fn read_file(dir: &Path, path: &str) -> Result<Vec<Runs>, String> {
    let file = std::fs::File::open(dir.join(path)).map_err(|err| err.to_string())?;
    let mut text = Vec::new();
    file.take(FILE_BYTES)
        .read_to_end(&mut text)
        .map_err(|err| err.to_string())?;
    Ok(preview::highlight(path, &String::from_utf8_lossy(&text)).collect())
}

pub struct Grep {
    /// The worktree it searches.
    pub dir: PathBuf,
    /// The worktree's project and branch, for the header.
    pub place: String,
    pub query: TextInput,
    /// What the last search found, or why it failed: for the query as it
    /// was then, until a search for the new one comes back.
    found: Option<Result<Found, String>>,
    /// Whether a search for the query as it is now is on its way.
    pub searching: bool,
    /// The selected hit, by its place among them.
    pub selected: usize,
    /// The selected hit's file, once it's read.
    pub file: Option<FileText>,
    /// How many rows the list is drawn in, for paging.
    rows: u16,
}

/// A file's lines, highlighted, or why there are none.
pub struct FileText {
    pub path: String,
    pub lines: Result<Vec<Runs>, String>,
}

/// A row of the list: a file, with how many lines were found in it, or one
/// of those lines, by its place among the hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row<'a> {
    File { path: &'a str, hits: usize },
    Hit(usize),
}

impl Grep {
    pub fn new(dir: PathBuf, place: String) -> Grep {
        Grep {
            dir,
            place,
            query: TextInput::default(),
            found: None,
            searching: false,
            selected: 0,
            file: None,
            rows: 20,
        }
    }

    /// The size of the list's area, as `(rows, columns)`.
    pub fn set_size(&mut self, list: (u16, u16)) {
        self.rows = list.0;
    }

    fn hits(&self) -> &[git::Hit] {
        match &self.found {
            Some(Ok(found)) => &found.hits,
            _ => &[],
        }
    }

    pub fn selected_hit(&self) -> Option<&git::Hit> {
        self.hits().get(self.selected)
    }

    /// Takes what a search found, if it's for the query as it is now and in
    /// this worktree, and asks for the first hit's file.
    pub fn searched(
        &mut self,
        dir: &Path,
        query: &str,
        found: Result<Found, String>,
    ) -> Option<Action> {
        if dir != self.dir || query != self.query.text() {
            return None;
        }
        self.found = Some(found);
        self.searching = false;
        self.selected = 0;
        self.read_selected()
    }

    /// Takes a file's lines, if it's still the selected hit's.
    pub fn file_read(&mut self, dir: &Path, path: &str, lines: Result<Vec<Runs>, String>) {
        let wanted = self.selected_hit().map(|hit| hit.path.as_str());
        if dir == self.dir && wanted == Some(path) {
            self.file = Some(FileText {
                path: path.to_string(),
                lines,
            });
        }
    }

    /// The list's rows: each file the hits are in, then its hits.
    pub fn rows(&self) -> Vec<Row<'_>> {
        let hits = self.hits();
        let mut rows = Vec::new();
        let mut at = 0;
        while at < hits.len() {
            let path = hits[at].path.as_str();
            let count = hits[at..].iter().take_while(|hit| hit.path == path).count();
            rows.push(Row::File { path, hits: count });
            rows.extend((at..at + count).map(Row::Hit));
            at += count;
        }
        rows
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = usize::from(self.rows.saturating_sub(QUERY_ROWS + 1).max(1));
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter => {
                return match self.selected_hit() {
                    Some(hit) => Outcome::Edit {
                        path: hit.path.clone(),
                        line: Some(hit.line),
                    },
                    None => Outcome::Stay,
                };
            }
            KeyCode::Up => return self.select_by(-1),
            KeyCode::Down => return self.select_by(1),
            KeyCode::Char('p') if ctrl => return self.select_by(-1),
            KeyCode::Char('n') if ctrl => return self.select_by(1),
            KeyCode::PageUp => return self.select_by(-(page as isize)),
            KeyCode::PageDown => return self.select_by(page as isize),
            _ => {}
        }
        let before = self.query.text().to_string();
        self.query.on_key(&key);
        if self.query.text() == before {
            return Outcome::Stay;
        }
        self.query_changed()
    }

    /// Pasted text goes into the query, as if typed.
    pub fn on_paste(&mut self, text: &str) -> Outcome {
        self.query.insert_str(text);
        self.query_changed()
    }

    /// A click on a hit selects it, and the wheel moves the selection.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(row)) => {
                match self.rows().get(row) {
                    Some(Row::Hit(index)) => {
                        self.selected = *index;
                        self.outcome_for_selection()
                    }
                    _ => Outcome::Stay,
                }
            }
            (MouseEventKind::ScrollUp, Hit::ViewList(_)) => self.select_by(-1),
            (MouseEventKind::ScrollDown, Hit::ViewList(_)) => self.select_by(1),
            _ => Outcome::Stay,
        }
    }

    /// Asks for a search for the new query, unless it's too short to be
    /// worth one; then what was found before goes.
    fn query_changed(&mut self) -> Outcome {
        let query = self.query.text().to_string();
        if query.trim().chars().count() < SHORTEST {
            self.found = None;
            self.searching = false;
            self.selected = 0;
            return Outcome::Stay;
        }
        self.searching = true;
        Outcome::Do(Action::Grep {
            dir: self.dir.clone(),
            query,
        })
    }

    /// Moves the selection `by` hits, kept in range.
    fn select_by(&mut self, by: isize) -> Outcome {
        let last = self.hits().len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
        self.outcome_for_selection()
    }

    fn outcome_for_selection(&mut self) -> Outcome {
        match self.read_selected() {
            Some(action) => Outcome::Do(action),
            None => Outcome::Stay,
        }
    }

    /// What reading the selected hit's file takes, unless it's the one read
    /// already.
    fn read_selected(&mut self) -> Option<Action> {
        let path = self.selected_hit()?.path.clone();
        if self.file.as_ref().map(|file| file.path.as_str()) == Some(path.as_str()) {
            return None;
        }
        self.file = None;
        Some(Action::ReadMatchedFile {
            dir: self.dir.clone(),
            path,
        })
    }

    /// The row the selected hit is on.
    fn selected_row(&self) -> usize {
        self.rows()
            .iter()
            .position(|row| *row == Row::Hit(self.selected))
            .unwrap_or(0)
    }
}

/// Where `query` is in `text`, as byte ranges, minding the case of letters
/// only when the query has a capital in it, as the search did.
pub fn occurrences(text: &str, query: &str) -> Vec<Range<usize>> {
    let ignore_case = !query.chars().any(char::is_uppercase);
    let fold = |letter: char| match ignore_case {
        true => letter.to_lowercase().next().unwrap_or(letter),
        false => letter,
    };
    let wanted: Vec<char> = query.chars().map(fold).collect();
    let letters: Vec<(usize, char)> = text.char_indices().collect();
    let mut found = Vec::new();
    let mut at = 0;
    while !wanted.is_empty() && at + wanted.len() <= letters.len() {
        let here = &letters[at..at + wanted.len()];
        if here
            .iter()
            .zip(&wanted)
            .all(|((_, letter), want)| fold(*letter) == *want)
        {
            let end = letters
                .get(at + wanted.len())
                .map_or(text.len(), |(index, _)| *index);
            found.push(letters[at].0..end);
            at += wanted.len();
        } else {
            at += 1;
        }
    }
    found
}

/// How wide the list is, for a view `width` columns wide: the lines found
/// are what's read here, so it's wide.
pub fn list_width(width: u16) -> u16 {
    (width / 2).clamp(30.min(width), 80)
}

/// The keys the footer shows while find in files is open.
pub fn hints() -> Vec<(&'static str, &'static str)> {
    vec![("↑/↓", "select"), ("enter", "edit there"), ("esc", "close")]
}

/// Which row of the list is on screen `row`, in a list drawn in `area`.
pub fn list_hit(grep: &Grep, area: Rect, row: u16) -> Hit {
    let Some(row) = (row - area.y).checked_sub(QUERY_ROWS) else {
        return Hit::Elsewhere;
    };
    let height = area.height.saturating_sub(QUERY_ROWS);
    let index = list_offset(grep.selected_row(), height) + usize::from(row);
    if index < grep.rows().len() {
        Hit::ViewList(index)
    } else {
        Hit::Elsewhere
    }
}

/// The first row on screen: the list scrolls to keep the selection in
/// sight.
fn list_offset(selected: usize, height: u16) -> usize {
    let height = usize::from(height.max(1));
    (selected + 1).saturating_sub(height)
}

pub fn draw(frame: &mut Frame, grep: &Grep, look: &Look, areas: &ViewAreas) {
    let theme = look.theme;
    frame.render_widget(header(grep, look, areas.header.width), areas.header);
    ui::draw_rule(frame, look, areas.rule);
    draw_query(frame, grep, look, areas.list);
    let list = Rect::new(
        areas.list.x,
        areas.list.y + QUERY_ROWS,
        areas.list.width,
        areas.list.height.saturating_sub(QUERY_ROWS),
    );
    match &grep.found {
        None if grep.searching => ui::draw_message(frame, look, "searching…", list),
        None => {
            let message = format!("type {SHORTEST} letters or more");
            ui::draw_message(frame, look, &message, list);
        }
        Some(Err(err)) => {
            let line = Line::styled(format!(" {err}"), Style::new().fg(theme.failed));
            frame.render_widget(Paragraph::new(line), list);
        }
        Some(Ok(found)) if found.hits.is_empty() => {
            ui::draw_message(frame, look, "nothing found", list);
        }
        Some(Ok(_)) => draw_rows(frame, grep, look, list),
    }
    draw_preview(frame, grep, look, areas.content);
}

/// "⌕ find in files · 12 lines in 3 files", and where on the right.
fn header<'a>(grep: &Grep, look: &Look, width: u16) -> Line<'a> {
    let mut notes = Vec::new();
    let lines = grep.hits().len();
    if lines > 0 {
        let files = grep
            .rows()
            .iter()
            .filter(|row| matches!(row, Row::File { .. }))
            .count();
        let more = match &grep.found {
            Some(Ok(found)) if found.more => "+",
            _ => "",
        };
        let line_noun = if lines == 1 { "line" } else { "lines" };
        let file_noun = if files == 1 { "file" } else { "files" };
        notes.push(format!("{lines}{more} {line_noun} in {files} {file_noun}"));
    }
    if grep.searching {
        notes.push("searching…".to_string());
    }
    ui::view_header("⌕", "find in files", &notes, &grep.place, look, width)
}

/// The query, with the cursor in it, and a rule under it.
fn draw_query(frame: &mut Frame, grep: &Grep, look: &Look, list: Rect) {
    let theme = look.theme;
    let line = Line::from(vec![
        Span::styled(
            " › ",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(grep.query.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, Rect::new(list.x, list.y, list.width, 1));
    let rule = "─".repeat(usize::from(list.width));
    frame.render_widget(
        Line::styled(rule, Style::new().fg(theme.rule)),
        Rect::new(list.x, list.y + 1, list.width, 1),
    );
    // The prompt is three columns wide.
    let column = list.x + 3 + grep.query.cursor() as u16;
    frame.set_cursor_position((column.min(list.right().saturating_sub(1)), list.y));
}

/// The rows: each file's path, with how many lines were found in it, then
/// those lines, each with its number and what was found in it marked.
fn draw_rows(frame: &mut Frame, grep: &Grep, look: &Look, area: Rect) {
    let theme = look.theme;
    let rows = grep.rows();
    let first = list_offset(grep.selected_row(), area.height);
    let width = usize::from(area.width);
    let hits = grep.hits();
    let numbers = hits
        .iter()
        .map(|hit| hit.line)
        .max()
        .unwrap_or(0)
        .to_string()
        .len();
    for (at, row) in rows.iter().enumerate().skip(first).take(area.height.into()) {
        let y = area.y + (at - first) as u16;
        let line_area = Rect::new(area.x, y, area.width, 1);
        let line = match row {
            Row::File { path, hits } => {
                let count = format!(" {hits}");
                let path = fit(path, width.saturating_sub(count.len() + 2));
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        path,
                        Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(count, Style::new().fg(theme.muted)),
                ])
            }
            Row::Hit(index) => {
                let hit = &hits[*index];
                if *index == grep.selected {
                    frame.buffer_mut().set_style(line_area, theme.selection);
                }
                let number = format!("   {:>numbers$}  ", hit.line);
                let text = fit(&hit.text, width.saturating_sub(number.len()));
                let mut spans = vec![Span::styled(number, Style::new().fg(theme.muted))];
                spans.extend(found_spans(&text, grep.query.text(), look));
                Line::from(spans)
            }
        };
        frame.render_widget(line, line_area);
    }
}

/// `text` as spans, with where `query` is in it in the accent color.
fn found_spans<'a>(text: &str, query: &str, look: &Look) -> Vec<Span<'a>> {
    let theme = look.theme;
    let found = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    pieces(text, &occurrences(text, query.trim()))
        .into_iter()
        .map(|(piece, is_found)| {
            let style = if is_found {
                found
            } else {
                Style::new().fg(theme.text)
            };
            Span::styled(piece.to_string(), style)
        })
        .collect()
}

/// The selected hit's file around its line: the path and line number,
/// then the lines, numbered and highlighted, the hit's a third of the way
/// down, with what was found in it marked.
fn draw_preview(frame: &mut Frame, grep: &Grep, look: &Look, area: Rect) {
    let theme = look.theme;
    let Some(hit) = grep.selected_hit() else {
        return;
    };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            hit.path.clone(),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(":{}", hit.line), Style::new().fg(theme.muted)),
    ]);
    frame.render_widget(title, Rect::new(area.x, area.y, area.width, 1));
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let Some(file) = grep.file.as_ref().filter(|file| file.path == hit.path) else {
        return;
    };
    let lines = match &file.lines {
        Ok(lines) => lines,
        Err(why) => {
            ui::draw_message(frame, look, why, body);
            return;
        }
    };
    let numbers = lines.len().to_string().len();
    let first = hit.line.saturating_sub(1 + usize::from(body.height / 3));
    let shown = lines
        .iter()
        .enumerate()
        .skip(first)
        .take(body.height.into());
    for (row, (index, runs)) in shown.enumerate() {
        let line_area = Rect::new(body.x, body.y + row as u16, body.width, 1);
        let number = Span::styled(
            format!(" {:>numbers$}  ", index + 1),
            Style::new().fg(theme.muted),
        );
        let mut spans = vec![number];
        if index + 1 == hit.line {
            frame.buffer_mut().set_style(line_area, theme.selection);
            let text: String = runs.iter().map(|(_, text)| text.as_str()).collect();
            spans.extend(found_spans(&text, grep.query.text(), look));
        } else {
            spans.extend(preview::runs_spans(runs, look));
        }
        frame.render_widget(Line::from(spans), line_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::TokenKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn hit(path: &str, line: usize) -> git::Hit {
        git::Hit {
            path: path.into(),
            line,
            text: format!("needle at {line}"),
        }
    }

    fn grep_with(hits: Vec<git::Hit>) -> Grep {
        let mut grep = Grep::new(PathBuf::from("/code/app"), "app ⌂ main".into());
        for letter in "needle".chars() {
            grep.on_key(key(KeyCode::Char(letter)));
        }
        let found = Found { hits, more: false };
        grep.searched(Path::new("/code/app"), "needle", Ok(found));
        grep
    }

    #[test]
    fn typing_asks_for_a_search_once_the_query_is_long_enough() {
        let mut grep = Grep::new(PathBuf::from("/code/app"), "app".into());
        assert_eq!(grep.on_key(key(KeyCode::Char('n'))), Outcome::Stay);
        assert_eq!(
            grep.on_key(key(KeyCode::Char('e'))),
            Outcome::Do(Action::Grep {
                dir: PathBuf::from("/code/app"),
                query: "ne".into(),
            })
        );
        assert!(grep.searching);
    }

    #[test]
    fn what_a_search_for_an_older_query_found_is_dropped() {
        let mut grep = grep_with(vec![hit("a.rs", 1)]);
        grep.on_key(key(KeyCode::Char('s')));
        let late = Found {
            hits: vec![hit("old.rs", 3)],
            more: false,
        };
        assert_eq!(
            grep.searched(Path::new("/code/app"), "needle", Ok(late)),
            None
        );
        assert!(grep.searching);
        assert_eq!(grep.selected_hit().unwrap().path, "a.rs");
    }

    #[test]
    fn hits_are_listed_under_their_files_and_the_arrows_skip_the_files() {
        let mut grep = grep_with(vec![hit("a.rs", 1), hit("a.rs", 9), hit("b.rs", 4)]);
        assert_eq!(
            grep.rows(),
            [
                Row::File {
                    path: "a.rs",
                    hits: 2
                },
                Row::Hit(0),
                Row::Hit(1),
                Row::File {
                    path: "b.rs",
                    hits: 1
                },
                Row::Hit(2),
            ]
        );
        grep.on_key(key(KeyCode::Down));
        let outcome = grep.on_key(key(KeyCode::Down));
        assert_eq!(grep.selected_row(), 4);
        assert_eq!(
            outcome,
            Outcome::Do(Action::ReadMatchedFile {
                dir: PathBuf::from("/code/app"),
                path: "b.rs".into(),
            })
        );
        assert_eq!(
            grep.on_key(key(KeyCode::Enter)),
            Outcome::Edit {
                path: "b.rs".into(),
                line: Some(4),
            }
        );
    }

    #[test]
    fn a_hit_in_the_file_already_read_needs_no_reading() {
        let mut grep = grep_with(vec![hit("a.rs", 1), hit("a.rs", 9)]);
        let line = vec![(TokenKind::Text, "needle".to_string())];
        grep.file_read(Path::new("/code/app"), "a.rs", Ok(vec![line]));
        assert_eq!(grep.on_key(key(KeyCode::Down)), Outcome::Stay);
        assert!(grep.file.is_some());
    }

    #[test]
    fn found_text_is_marked_minding_case_only_for_capitals() {
        let text = "Needle, needle, NEEDLE";
        assert_eq!(occurrences(text, "needle"), [0..6, 8..14, 16..22]);
        assert_eq!(occurrences(text, "Needle"), vec![0..6]);
        assert_eq!(occurrences("héllo héllo", "llo"), [3..6, 10..13]);
        assert!(occurrences(text, "").is_empty());
    }

    #[test]
    fn a_files_lines_are_read_highlighted_with_tabs_as_spaces() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "one\n\tfn\n").unwrap();
        let read = read_file(dir.path(), "a.rs").unwrap();
        assert_eq!(read[0], [(TokenKind::Text, "one".to_string())]);
        assert_eq!(
            read[1],
            [
                (TokenKind::Text, "    ".to_string()),
                (TokenKind::Keyword, "fn".to_string())
            ]
        );
        assert!(read_file(dir.path(), "gone.rs").is_err());
    }
}
