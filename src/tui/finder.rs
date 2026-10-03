//! The file finder, `p` in the sidebar, like an editor's quick open: type a
//! few letters of a file's path and pick it from the list, with the
//! selected file previewed beside it, highlighted, or a markdown file as
//! its page. Enter opens the file in the user's `$EDITOR`, as a session of
//! its own in the worktree, so the editor sits in the sidebar like any
//! agent.

use super::app::{Action, Hit, Loading, Outcome};
use super::fuzzy::{self, Match};
use super::preview::{self, Content, Preview};
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
use std::path::{Path, PathBuf};

/// How many matches are listed, at most: more is past what anyone scrolls.
const MOST_MATCHES: usize = 500;

/// The rows above the list of matches: the query, and a rule under it.
pub const QUERY_ROWS: u16 = 2;

/// Lists the files of the worktree at `dir`: run off the event loop.
pub fn read_files(dir: &Path) -> Result<Vec<String>, String> {
    git::files(dir).map_err(|err| format!("{err:#}"))
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
    /// The selected file.
    pub preview: Preview,
    /// How many rows the list is drawn in, for paging.
    rows: u16,
}

impl Finder {
    /// A finder on the worktree at `dir`, until its files are listed.
    pub fn new(dir: PathBuf, place: String) -> Finder {
        Finder {
            preview: Preview::new(dir.clone()),
            dir,
            place,
            query: TextInput::default(),
            files: Loading::Reading,
            matches: Vec::new(),
            selected: 0,
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

    /// Takes a file that's been read for the preview, if it's still the
    /// selected one.
    pub fn preview_read(&mut self, dir: &Path, path: &str, read: Result<Content, String>) {
        self.preview.read_done(dir, path, read);
    }

    /// The sizes of the list's area and the preview's, as `(rows,
    /// columns)`.
    pub fn set_size(&mut self, list: (u16, u16), preview: (u16, u16)) {
        self.rows = list.0;
        self.preview.set_size(preview);
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
                    Some(path) => Outcome::Edit {
                        line: self.preview.top_line(path),
                        path: path.to_string(),
                    },
                    None => Outcome::Stay,
                };
            }
            KeyCode::Up => return self.select_by(-1),
            KeyCode::Down => return self.select_by(1),
            KeyCode::Char('p') if ctrl => return self.select_by(-1),
            KeyCode::Char('n') if ctrl => return self.select_by(1),
            KeyCode::Char('r') if ctrl => {
                self.preview.flip();
                return Outcome::Stay;
            }
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

    /// Matches the files again for a new query, from the best match.
    fn query_changed(&mut self) -> Outcome {
        self.filter();
        self.selected = 0;
        self.outcome_for_selection()
    }

    /// A click on a match selects it, and the wheel moves the selection,
    /// or scrolls the preview.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(index)) => {
                self.selected = index.min(self.matches.len().saturating_sub(1));
                self.outcome_for_selection()
            }
            (MouseEventKind::ScrollUp, Hit::ViewList(_)) => self.select_by(-1),
            (MouseEventKind::ScrollDown, Hit::ViewList(_)) => self.select_by(1),
            (MouseEventKind::ScrollUp, Hit::ViewContent) => {
                self.preview.wheel(true);
                Outcome::Stay
            }
            (MouseEventKind::ScrollDown, Hit::ViewContent) => {
                self.preview.wheel(false);
                Outcome::Stay
            }
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
        match self.selected_path().map(str::to_string) {
            Some(path) => self.preview.show(&path),
            None => {
                self.preview.clear();
                None
            }
        }
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
pub fn hints(finder: &Finder) -> Vec<(&'static str, &'static str)> {
    let mut hints = vec![("↑/↓", "select"), ("enter", "edit")];
    hints.extend(preview::flip_hint(&finder.preview));
    hints.push(("esc", "close"));
    hints
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
    draw_query(frame, &finder.query, look, areas.list);

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
    preview::draw(frame, &finder.preview, look, areas.content);
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

/// The query, with the cursor in it, and a rule under it, at the top of
/// `list`.
pub fn draw_query(frame: &mut Frame, query: &TextInput, look: &Look, list: Rect) {
    let theme = look.theme;
    let prompt = " › ";
    let line = Line::from(vec![
        Span::styled(
            prompt,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(query.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, Rect::new(list.x, list.y, list.width, 1));
    let rule = "─".repeat(usize::from(list.width));
    frame.render_widget(
        Line::styled(rule, Style::new().fg(theme.rule)),
        Rect::new(list.x, list.y + 1, list.width, 1),
    );
    // The prompt is three columns wide.
    let column = list.x + 3 + query.cursor() as u16;
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
        let outcome = type_text(&mut finder, "rf");
        assert_eq!(finder.selected_path(), Some("src/billing/refund.rs"));
        assert!(matches!(outcome, Outcome::Do(Action::ReadPreview { .. })));
        // The same best again: it's read already.
        assert_eq!(type_text(&mut finder, "nd"), Outcome::Stay);
        assert_eq!(finder.matches.len(), 1);
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
            Outcome::Edit {
                path: "src/main.rs".into(),
                line: None,
            }
        );
        assert_eq!(finder.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn the_preview_follows_the_selection() {
        let mut finder = finder_with(&["a.rs", "b.rs"]);
        assert_eq!(finder.preview.path(), Some("a.rs"));
        finder.on_key(key(KeyCode::Down));
        assert_eq!(finder.preview.path(), Some("b.rs"));
        type_text(&mut finder, "zzz");
        assert_eq!(finder.preview.path(), None, "nothing matches");
    }

    #[test]
    fn ctrl_r_flips_a_markdown_file_to_its_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# Title\n").unwrap();
        let mut finder = Finder::new(dir.path().to_path_buf(), "app".into());
        finder.files_read(dir.path(), Ok(vec!["README.md".into()]));
        let read = preview::read(dir.path(), "README.md");
        finder.preview_read(dir.path(), "README.md", read);
        assert_eq!(hints(&finder)[2], ("ctrl+r", "source"));
        finder.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(finder.preview.shows_source());
        assert_eq!(finder.query.text(), "", "not typed into the query");
    }
}
