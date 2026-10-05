//! The archive, `Z` in the sidebar: the sessions `A` stopped and kept out
//! of the list, the latest archived first. Enter starts the one the bar is
//! on again, where it was, and `x` deletes one for good once `y` says so.
//! The daemon keeps the archive; asking it is the event loop's, and nothing
//! here does any I/O.

use super::sidebar::{ago, fit};
use super::theme::Theme;
use crate::protocol::ArchivedSession;
use crate::shell;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear};

/// What a key in the view asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    /// Start the archived session with this id again.
    Restore(String),
    /// Take the archived session with this id out of the archive, for
    /// good.
    Delete(String),
}

pub struct ArchivedView {
    /// The archive as the daemon last gave it, or why it couldn't.
    archived: Result<Vec<ArchivedSession>, String>,
    /// The row the bar is on.
    highlighted: usize,
    /// The session `x` asks about deleting, by its id and name, until the
    /// next key answers.
    deleting: Option<(String, String)>,
}

impl ArchivedView {
    pub fn new(archived: Result<Vec<ArchivedSession>, String>) -> ArchivedView {
        ArchivedView {
            archived,
            highlighted: 0,
            deleting: None,
        }
    }

    /// Takes the archive as the daemon gives it now, the bar staying on
    /// its session if it's still there.
    pub fn set_archived(&mut self, archived: Result<Vec<ArchivedSession>, String>) {
        let was_on = self.highlighted_id();
        self.archived = archived;
        let rows = self.rows();
        let found = was_on.and_then(|id| rows.iter().position(|row| row.id == id));
        let at = found.unwrap_or(self.highlighted);
        self.highlighted = at.min(rows.len().saturating_sub(1));
    }

    fn rows(&self) -> &[ArchivedSession] {
        self.archived.as_deref().unwrap_or_default()
    }

    fn highlighted_id(&self) -> Option<String> {
        Some(self.rows().get(self.highlighted)?.id.clone())
    }

    /// The question `x` asks, while it waits for its answer.
    pub fn deleting(&self) -> Option<String> {
        let (_, name) = self.deleting.as_ref()?;
        Some(format!(
            "delete {name} for good? It can't be started again. y/n"
        ))
    }

    /// Whether a key typed is a character, not a move: while `x` asks.
    pub fn typing(&self) -> bool {
        self.deleting.is_some()
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        if let Some((id, _)) = self.deleting.take() {
            return match key.code {
                KeyCode::Char('y') => Step::Delete(id),
                _ => Step::Stay,
            };
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let highlighted = self.rows().get(self.highlighted);
        let highlighted = highlighted.map(|row| (row.id.clone(), row.name().to_string()));
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'Z') => return Step::Close,
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Char('x') => self.deleting = highlighted,
            KeyCode::Enter => {
                if let Some((id, _)) = highlighted {
                    return Step::Restore(id);
                }
            }
            _ => {}
        }
        Step::Stay
    }

    fn move_by(&mut self, by: isize) {
        let last = self.rows().len().saturating_sub(1);
        self.highlighted = self.highlighted.saturating_add_signed(by).min(last);
    }
}

/// Draws the view in `area`, over the sidebar and the panes: a heading,
/// then the archived sessions. `now` is in seconds since the Unix epoch.
pub fn draw(frame: &mut Frame, view: &ArchivedView, theme: &Theme, now: u64, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.base()), area);
    let [heading, _, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "archive",
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " · sessions stopped and kept, to start again where they were",
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(title, heading);
    let archived = match &view.archived {
        Ok(archived) => archived,
        Err(reason) => return draw_note(frame, theme, reason, list),
    };
    let count = Line::styled(
        format!("{} archived ", archived.len()),
        Style::new().fg(theme.muted),
    );
    frame.render_widget(count.right_aligned(), heading);
    if archived.is_empty() {
        let note = "nothing archived: A in the sidebar archives the selected session";
        return draw_note(frame, theme, note, list);
    }
    let height = usize::from(list.height.max(1));
    let first = (view.highlighted + 1).saturating_sub(height);
    for (row, session) in archived.iter().enumerate().skip(first).take(height) {
        let area = Rect::new(list.x, list.y + (row - first) as u16, list.width, 1);
        let highlighted = row == view.highlighted;
        if highlighted {
            frame.buffer_mut().set_style(area, theme.selection);
        }
        let line = archived_line(session, theme, now, highlighted, list.width);
        frame.render_widget(line, area);
    }
}

/// An archived session's row: its name, then where it ran, and on the
/// right whether it starts again where it was and how long ago it was
/// archived.
fn archived_line<'a>(
    session: &ArchivedSession,
    theme: &Theme,
    now: u64,
    highlighted: bool,
    width: u16,
) -> Line<'a> {
    let place = match &session.worktree {
        Some(worktree) => match &worktree.branch {
            Some(branch) => format!("{} · {branch}", worktree.project),
            None => worktree.project.clone(),
        },
        None => shell::home_relative(&session.session.cwd),
    };
    let resumes = if session.resumes() {
        "where it was"
    } else {
        "from the start"
    };
    let when = match ago(session.archived, now).as_str() {
        "now" => "just now".to_string(),
        long => format!("{long} ago"),
    };
    let right = format!("{resumes} · {when} ");
    let right_width = right.chars().count();
    let name_width = session.name().chars().count().min(30);
    let room = usize::from(width).saturating_sub(4 + name_width + right_width + 2);
    let name = fit(session.name(), 30);
    let place = fit(&place, room);
    let used = 3 + name.chars().count() + 2 + place.chars().count();
    let gap = usize::from(width).saturating_sub(used + right_width);
    let mut name_style = Style::new().fg(theme.text);
    if highlighted {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    Line::from(vec![
        Span::styled(" ▪ ", Style::new().fg(theme.muted)),
        Span::styled(name, name_style),
        Span::raw("  "),
        Span::styled(place, Style::new().fg(theme.muted)),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::new().fg(theme.muted)),
    ])
}

fn draw_note(frame: &mut Frame, theme: &Theme, note: &str, area: Rect) {
    let line = Line::styled(format!(" {note}"), Style::new().fg(theme.muted));
    frame.render_widget(line, area);
}

/// The keys while the view is open.
pub const HINTS: &[(&str, &str)] = &[
    ("enter", "start it again"),
    ("x", "delete"),
    ("esc", "close"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SavedSession;
    use std::path::PathBuf;

    fn press(view: &mut ArchivedView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn archived(id: &str, name: &str) -> ArchivedSession {
        ArchivedSession {
            id: id.into(),
            session: SavedSession {
                name: name.into(),
                command: vec!["claude".into()],
                cwd: PathBuf::from("/code/app"),
                conversation: None,
                task: None,
                goal: None,
                resume: None,
                about: Default::default(),
                name_given: false,
                moved: None,
            },
            worktree: None,
            archived: 10,
        }
    }

    #[test]
    fn enter_starts_the_one_the_bar_is_on_and_x_deletes_only_after_a_yes() {
        let mut view = ArchivedView::new(Ok(vec![archived("1", "a"), archived("2", "b")]));
        press(&mut view, KeyCode::Down);
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Restore("2".into()));
        press(&mut view, KeyCode::Char('x'));
        assert_eq!(
            view.deleting().as_deref(),
            Some("delete b for good? It can't be started again. y/n")
        );
        assert_eq!(press(&mut view, KeyCode::Char('n')), Step::Stay);
        assert_eq!(view.deleting(), None);
        press(&mut view, KeyCode::Char('x'));
        assert_eq!(
            press(&mut view, KeyCode::Char('y')),
            Step::Delete("2".into())
        );
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Close);
    }

    #[test]
    fn the_bar_stays_on_its_session_when_the_archive_comes_back() {
        let mut view = ArchivedView::new(Ok(vec![archived("1", "a"), archived("2", "b")]));
        press(&mut view, KeyCode::Down);
        view.set_archived(Ok(vec![
            archived("3", "c"),
            archived("1", "a"),
            archived("2", "b"),
        ]));
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Restore("2".into()));
        view.set_archived(Ok(vec![archived("3", "c")]));
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Restore("3".into()));
        view.set_archived(Ok(Vec::new()));
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Stay);
        assert_eq!(press(&mut view, KeyCode::Char('x')), Step::Stay);
        assert_eq!(view.deleting(), None);
    }
}
