//! Drawing the TUI: the sidebar of sessions, the pane with the selected one,
//! and a footer with the keys. Drawing only reads the state; it never
//! changes it.

use super::app::{App, Focus};
use super::pane::Pane;
use super::screen_widget::ScreenWidget;
use crate::protocol::{SessionInfo, State};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

const SIDEBAR_WIDTH: u16 = 28;

/// Where each part of the TUI goes on a screen of a given size.
pub struct Areas {
    pub sidebar: Rect,
    pub pane: Rect,
    pub footer: Rect,
}

impl Areas {
    pub fn new(screen: Rect) -> Areas {
        let [main, footer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(screen);
        let [sidebar, pane] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(main);
        Areas {
            sidebar,
            pane,
            footer,
        }
    }

    /// The inside of the pane's border, where the session's screen goes.
    /// The session is sized to fit it exactly.
    pub fn session_screen(&self) -> Rect {
        Block::bordered().inner(self.pane)
    }
}

pub fn draw(frame: &mut Frame, app: &App, pane: Option<&Pane>) {
    let areas = Areas::new(frame.area());
    draw_sidebar(frame, app, areas.sidebar);
    draw_pane(frame, app, pane, &areas);
    draw_footer(frame, app, areas.footer);
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(" sessions ")
        .border_style(border_style(app.focus() == Focus::Sidebar));
    let rows: Vec<ListItem> = app
        .sessions()
        .iter()
        .map(|session| ListItem::new(session_row(session)))
        .collect();
    let list = List::new(rows)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(app.selected_index());
    frame.render_stateful_widget(list, area, &mut state);
}

/// A session's row: a mark that says whether it's running, its name, and
/// how it ended if it has.
fn session_row(session: &SessionInfo) -> Line<'_> {
    let mark = match session.state {
        State::Running => "▶ ".green(),
        State::Exited { code: 0 } => "■ ".dark_gray(),
        _ => "■ ".red(),
    };
    let mut row = Line::from(vec![mark, session.name.as_str().into()]);
    if session.state != State::Running {
        row.push_span(format!(" {}", session.state).dark_gray());
    }
    row
}

fn draw_pane(frame: &mut Frame, app: &App, pane: Option<&Pane>, areas: &Areas) {
    let focused = app.focus() == Focus::Pane;
    let mut block = Block::bordered().border_style(border_style(focused));
    if let Some(session) = app.selected() {
        block = block.title(pane_title(session));
    }
    frame.render_widget(block, areas.pane);

    let screen = areas.session_screen();
    let Some(session) = app.selected() else {
        draw_message(frame, "No sessions yet. Press n to start a shell.", screen);
        return;
    };
    if app.selected_is_own() {
        draw_message(frame, "This is the session crystal is running in.", screen);
        return;
    }
    // Until the pane has attached to the selected session, there's
    // nothing to show yet.
    let Some(pane) = pane.filter(|pane| pane.session == session.name) else {
        return;
    };
    let session_screen = pane.screen.screen();
    frame.render_widget(ScreenWidget::new(session_screen), screen);
    if focused && !session_screen.hide_cursor() {
        let (row, col) = session_screen.cursor_position();
        if row < screen.height && col < screen.width {
            frame.set_cursor_position((screen.x + col, screen.y + row));
        }
    }
}

fn pane_title(session: &SessionInfo) -> String {
    if session.state == State::Running {
        format!(" {} ", session.name)
    } else {
        format!(" {} · {} ", session.name, session.state)
    }
}

/// One line of text across the middle of `area`.
fn draw_message(frame: &mut Frame, message: &str, area: Rect) {
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(message).centered().dark_gray(), middle);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let footer = if let Some(notice) = app.notice() {
        Line::from(notice.to_string()).red()
    } else if app.focus() == Focus::Pane {
        Line::from("typing into the session · ctrl+\\ back to the list").dark_gray()
    } else {
        Line::from("j/k select · enter type into it · n new shell · x kill · q quit").dark_gray()
    };
    frame.render_widget(footer, area);
}

/// The focused part stands out; the other fades.
fn border_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    /// Draws `app` on an 80 by 12 screen and returns it as lines of text.
    fn screen_text(app: &App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| draw(frame, app, None)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    fn session(name: &str, state: State) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state,
        }
    }

    #[test]
    fn with_no_sessions_the_pane_says_how_to_start_one() {
        let app = App::new(None);
        let text = screen_text(&app).join("\n");
        assert!(text.contains("No sessions yet. Press n to start a shell."));
        assert!(text.contains("q quit"));
    }

    #[test]
    fn the_sidebar_lists_sessions_with_how_they_ended() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            session("claude", State::Running),
            session("codex", State::Exited { code: 1 }),
        ]);
        let text = screen_text(&app);
        assert!(text[1].contains("▶ claude"));
        assert!(text[2].contains("■ codex exited 1"));
        assert!(text[0].contains(" claude "), "the pane is titled after it");
    }

    #[test]
    fn the_session_screen_sits_inside_the_panes_border() {
        let areas = Areas::new(Rect::new(0, 0, 80, 24));
        assert_eq!(areas.session_screen(), Rect::new(29, 1, 50, 21));
    }
}
