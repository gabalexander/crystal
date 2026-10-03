//! The file finder, `p` in the sidebar, like an editor's quick open: type a
//! few letters of a file's path and pick it from the list, with the start
//! of the selected file shown beside it. Enter opens the file in the user's
//! `$EDITOR`, as a session of its own in the worktree, so the editor sits
//! in the sidebar like any agent.

use super::app::{Action, Hit, Loading, Outcome};
use super::fuzzy::{self, Match};
use super::sidebar::fit;
use super::text_input::TextInput;
use super::ui::{self, Look, ViewAreas};
use crate::git;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::io::Read;
use std::path::{Path, PathBuf};

/// How many matches are listed, at most: more is past what anyone scrolls.
const MOST_MATCHES: usize = 500;

/// How much of a file the preview reads, and how many of its lines it keeps.
const PREVIEW_BYTES: u64 = 64 * 1024;
const PREVIEW_LINES: usize = 500;

/// The rows above the list of matches: the query, and a rule under it.
const QUERY_ROWS: u16 = 2;

/// Lists the files of the worktree at `dir`: run off the event loop.
pub fn read_files(dir: &Path) -> Result<Vec<String>, String> {
    git::files(dir).map_err(|err| format!("{err:#}"))
}

/// The first lines of the file at `path` in the worktree at `dir`: run off
/// the event loop. A binary file has none to show.
pub fn read_preview(dir: &Path, path: &str) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(dir.join(path)).map_err(|err| err.to_string())?;
    let mut start = Vec::new();
    file.take(PREVIEW_BYTES)
        .read_to_end(&mut start)
        .map_err(|err| err.to_string())?;
    if start.contains(&0) {
        return Err("a binary file".to_string());
    }
    let text = String::from_utf8_lossy(&start);
    Ok(text
        .lines()
        .take(PREVIEW_LINES)
        .map(|line| line.replace('\t', "    "))
        .collect())
}

pub struct Finder {
    /// The worktree it finds files in.
    pub dir: PathBuf,
    /// The worktree's project and branch, for the header.
    pub place: String,
    pub query: TextInput,
    pub files: Loading<Vec<String>>,
    /// The files that match the query, best first: their places in the
    /// list of files, with how they matched.
    pub matches: Vec<(usize, Match)>,
    /// The selected match, by its place in `matches`.
    pub selected: usize,
    /// The selected file's first lines, once they're read.
    pub preview: Option<Preview>,
    /// How many rows the list is drawn in, for paging.
    rows: u16,
}

/// A file's first lines, or why there are none.
pub struct Preview {
    pub path: String,
    pub lines: Result<Vec<String>, String>,
}

impl Finder {
    /// A finder on the worktree at `dir`, until its files are listed.
    pub fn new(dir: PathBuf, place: String) -> Finder {
        Finder {
            dir,
            place,
            query: TextInput::default(),
            files: Loading::Reading,
            matches: Vec::new(),
            selected: 0,
            preview: None,
            rows: 20,
        }
    }

    /// What listing the files takes, for the event loop to do.
    pub fn read(&self) -> Action {
        Action::ReadFiles(self.dir.clone())
    }

    /// Takes the listed files, if they're this worktree's, and asks for the
    /// first one's preview.
    pub fn files_read(&mut self, dir: &Path, files: Result<Vec<String>, String>) -> Option<Action> {
        if dir != self.dir {
            return None;
        }
        self.files = match files {
            Ok(files) => Loading::Read(files),
            Err(err) => Loading::Failed(err),
        };
        self.filter();
        self.read_selected()
    }

    /// Takes a file's first lines, if it's still the selected one.
    pub fn preview_read(&mut self, dir: &Path, path: &str, lines: Result<Vec<String>, String>) {
        if dir == self.dir && self.selected_path() == Some(path) {
            self.preview = Some(Preview {
                path: path.to_string(),
                lines,
            });
        }
    }

    /// The size of the list's area, as `(rows, columns)`.
    pub fn set_size(&mut self, list: (u16, u16)) {
        self.rows = list.0;
    }

    /// The path of the selected match.
    pub fn selected_path(&self) -> Option<&str> {
        let Loading::Read(files) = &self.files else {
            return None;
        };
        let (index, _) = self.matches.get(self.selected)?;
        Some(&files[*index])
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = usize::from(self.rows.saturating_sub(QUERY_ROWS + 1).max(1));
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter => {
                return match self.selected_path() {
                    Some(path) => Outcome::Edit(path.to_string()),
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
        self.filter();
        self.selected = 0;
        self.outcome_for_selection()
    }

    /// A click on a match selects it, and the wheel moves the selection.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(index)) => {
                self.selected = index.min(self.matches.len().saturating_sub(1));
                self.outcome_for_selection()
            }
            (MouseEventKind::ScrollUp, Hit::ViewList(_)) => self.select_by(-1),
            (MouseEventKind::ScrollDown, Hit::ViewList(_)) => self.select_by(1),
            _ => Outcome::Stay,
        }
    }

    /// Moves the selection `by` matches, kept in range.
    fn select_by(&mut self, by: isize) -> Outcome {
        let last = self.matches.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
        self.outcome_for_selection()
    }

    /// Asks for the selected file's preview, unless it's already shown.
    fn outcome_for_selection(&mut self) -> Outcome {
        match self.read_selected() {
            Some(action) => Outcome::Do(action),
            None => Outcome::Stay,
        }
    }

    /// What reading the selected file's preview takes, if it isn't the one
    /// shown already.
    fn read_selected(&mut self) -> Option<Action> {
        let path = self.selected_path()?.to_string();
        let shown = self.preview.as_ref().map(|preview| preview.path.as_str());
        if shown == Some(path.as_str()) {
            return None;
        }
        self.preview = None;
        Some(Action::ReadPreview {
            dir: self.dir.clone(),
            path,
        })
    }

    /// Finds the matches for the query, best first.
    fn filter(&mut self) {
        self.matches = match &self.files {
            Loading::Read(files) => fuzzy::filter(files, self.query.text(), MOST_MATCHES),
            _ => Vec::new(),
        };
    }
}

/// How wide the list of files is, for a view `width` columns wide: wider
/// than the diff's, since paths are what's read here.
pub fn list_width(width: u16) -> u16 {
    (width * 2 / 5).clamp(30, 64).min(width / 2)
}

/// The keys the footer shows while the finder is open.
pub fn hints() -> Vec<(&'static str, &'static str)> {
    vec![("↑/↓", "select"), ("enter", "edit"), ("esc", "close")]
}

/// Which match's row is on screen `row`, in a list drawn in `area`.
pub fn list_hit(finder: &Finder, area: Rect, row: u16) -> Hit {
    let Some(row) = (row - area.y).checked_sub(QUERY_ROWS) else {
        return Hit::Elsewhere;
    };
    let index = list_offset(finder.selected, matches_height(area)) + usize::from(row);
    if index < finder.matches.len() {
        Hit::ViewList(index)
    } else {
        Hit::Elsewhere
    }
}

fn matches_height(list: Rect) -> u16 {
    list.height.saturating_sub(QUERY_ROWS)
}

/// The first match on screen: the list scrolls to keep the selection in
/// sight.
fn list_offset(selected: usize, height: u16) -> usize {
    let height = usize::from(height.max(1));
    (selected + 1).saturating_sub(height)
}

pub fn draw(frame: &mut Frame, finder: &Finder, look: &Look, areas: &ViewAreas) {
    let theme = look.theme;
    frame.render_widget(header(finder, look, areas.header.width), areas.header);
    ui::draw_rule(frame, look, areas.rule);
    draw_query(frame, finder, look, areas.list);

    let list = Rect::new(
        areas.list.x,
        areas.list.y + QUERY_ROWS,
        areas.list.width,
        matches_height(areas.list),
    );
    match &finder.files {
        Loading::Reading => ui::draw_message(frame, look, "listing files…", list),
        Loading::Failed(err) => {
            let line = Line::styled(format!(" {err}"), Style::new().fg(theme.failed));
            frame.render_widget(Paragraph::new(line), list);
        }
        Loading::Read(files) if finder.matches.is_empty() => {
            let message = if files.is_empty() {
                "no files"
            } else {
                "nothing matches"
            };
            ui::draw_message(frame, look, message, list);
        }
        Loading::Read(files) => draw_matches(frame, finder, files, look, list),
    }
    draw_preview(frame, finder, look, areas.content);
}

/// "⌕ find a file · 1204 files", and where on the right.
fn header<'a>(finder: &Finder, look: &Look, width: u16) -> Line<'a> {
    let mut notes = Vec::new();
    if let Loading::Read(files) = &finder.files {
        let count = files.len();
        notes.push(if count == 1 {
            "1 file".to_string()
        } else {
            format!("{count} files")
        });
    }
    ui::view_header("⌕", "find a file", &notes, &finder.place, look, width)
}

/// The query, with the cursor in it, and a rule under it.
fn draw_query(frame: &mut Frame, finder: &Finder, look: &Look, list: Rect) {
    let theme = look.theme;
    let prompt = " › ";
    let line = Line::from(vec![
        Span::styled(
            prompt,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(finder.query.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, Rect::new(list.x, list.y, list.width, 1));
    let rule = "─".repeat(usize::from(list.width));
    frame.render_widget(
        Line::styled(rule, Style::new().fg(theme.rule)),
        Rect::new(list.x, list.y + 1, list.width, 1),
    );
    // The prompt is three columns wide.
    let column = list.x + 3 + finder.query.cursor() as u16;
    frame.set_cursor_position((column.min(list.right().saturating_sub(1)), list.y));
}

/// The matches, one a row: each path with its directory muted, its name
/// brighter, and the letters that matched in the accent color.
fn draw_matches(frame: &mut Frame, finder: &Finder, files: &[String], look: &Look, area: Rect) {
    let first = list_offset(finder.selected, area.height);
    let shown = finder
        .matches
        .iter()
        .enumerate()
        .skip(first)
        .take(area.height.into());
    for (index, (file, found)) in shown {
        let y = area.y + (index - first) as u16;
        let row = Rect::new(area.x, y, area.width, 1);
        if index == finder.selected {
            frame.buffer_mut().set_style(row, look.theme.selection);
        }
        let path = fit(&files[*file], usize::from(area.width).saturating_sub(2));
        frame.render_widget(path_line(&path, &found.positions, look), row);
    }
}

/// A path with its matched letters marked.
fn path_line<'a>(path: &str, matched: &[usize], look: &Look) -> Line<'a> {
    let theme = look.theme;
    let name_starts = path.rfind('/').map_or(0, |at| path[..=at].chars().count());
    let mut spans = vec![Span::raw(" ")];
    for (at, letter) in path.chars().enumerate() {
        let style = if matched.contains(&at) {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else if at >= name_starts {
            Style::new().fg(theme.text)
        } else {
            Style::new().fg(theme.muted)
        };
        spans.push(Span::styled(letter.to_string(), style));
    }
    Line::from(spans)
}

/// The selected file's path, then its first lines, numbered.
fn draw_preview(frame: &mut Frame, finder: &Finder, look: &Look, area: Rect) {
    let theme = look.theme;
    let Some(path) = finder.selected_path() else {
        return;
    };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            path.to_string(),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(title, Rect::new(area.x, area.y, area.width, 1));
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let Some(preview) = &finder.preview else {
        return;
    };
    let lines = match &preview.lines {
        Ok(lines) => lines,
        Err(why) => {
            ui::draw_message(frame, look, why, body);
            return;
        }
    };
    let numbers = lines.len().to_string().len();
    let shown: Vec<Line> = lines
        .iter()
        .take(body.height.into())
        .enumerate()
        .map(|(index, line)| {
            Line::from(vec![
                Span::styled(
                    format!(" {:>numbers$}  ", index + 1),
                    Style::new().fg(theme.muted),
                ),
                Span::styled(line.clone(), Style::new().fg(theme.text)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn finder_with(files: &[&str]) -> Finder {
        let mut finder = Finder::new(PathBuf::from("/code/app"), "app ⌂ main".into());
        let files = files.iter().map(|file| file.to_string()).collect();
        finder.files_read(Path::new("/code/app"), Ok(files));
        finder
    }

    fn type_text(finder: &mut Finder, text: &str) -> Outcome {
        let mut outcome = Outcome::Stay;
        for letter in text.chars() {
            outcome = finder.on_key(key(KeyCode::Char(letter)));
        }
        outcome
    }

    #[test]
    fn the_files_list_asks_for_the_first_preview() {
        let mut finder = Finder::new(PathBuf::from("/code/app"), "app".into());
        let files = vec!["README.md".to_string(), "src/main.rs".to_string()];
        let read = finder.files_read(Path::new("/code/app"), Ok(files));
        assert_eq!(
            read,
            Some(Action::ReadPreview {
                dir: PathBuf::from("/code/app"),
                path: "README.md".into(),
            })
        );
    }

    #[test]
    fn typing_narrows_the_list_and_asks_for_the_new_best() {
        let mut finder = finder_with(&["README.md", "src/billing/refund.rs", "src/main.rs"]);
        let outcome = type_text(&mut finder, "rfnd");
        assert_eq!(finder.matches.len(), 1);
        assert_eq!(finder.selected_path(), Some("src/billing/refund.rs"));
        assert!(matches!(outcome, Outcome::Do(Action::ReadPreview { .. })));
    }

    #[test]
    fn arrows_move_the_selection_within_the_matches() {
        let mut finder = finder_with(&["a.rs", "b.rs", "c.rs"]);
        finder.on_key(key(KeyCode::Down));
        finder.on_key(key(KeyCode::Down));
        finder.on_key(key(KeyCode::Down));
        assert_eq!(finder.selected_path(), Some("c.rs"));
        finder.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(finder.selected_path(), Some("b.rs"));
    }

    #[test]
    fn enter_edits_the_selected_file_and_esc_closes() {
        let mut finder = finder_with(&["README.md", "src/main.rs"]);
        type_text(&mut finder, "main");
        assert_eq!(
            finder.on_key(key(KeyCode::Enter)),
            Outcome::Edit("src/main.rs".into())
        );
        assert_eq!(finder.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn a_preview_for_a_file_no_longer_selected_is_dropped() {
        let mut finder = finder_with(&["a.rs", "b.rs"]);
        finder.on_key(key(KeyCode::Down));
        finder.preview_read(Path::new("/code/app"), "a.rs", Ok(vec!["old".into()]));
        assert!(finder.preview.is_none());
        finder.preview_read(Path::new("/code/app"), "b.rs", Ok(vec!["fn b() {}".into()]));
        assert_eq!(finder.preview.as_ref().unwrap().path, "b.rs");
    }

    #[test]
    fn a_preview_reads_the_start_of_a_text_file_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "one\n\ttwo\n").unwrap();
        std::fs::write(dir.path().join("logo.png"), b"\x89PNG\0\0").unwrap();
        assert_eq!(
            read_preview(dir.path(), "notes.txt").unwrap(),
            ["one", "    two"]
        );
        assert!(read_preview(dir.path(), "logo.png").is_err());
    }
}
