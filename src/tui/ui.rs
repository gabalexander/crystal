//! Drawing the TUI: the sidebar of sessions, the pane with the selected one
//! and the panes of the sessions split off, and a footer with the keys.
//! Drawing only reads the state; it never changes it.

use super::app::{App, Focus, Slot};
use super::groups::Row;
use super::pane::Pane;
use super::screen_widget::ScreenWidget;
use super::text_input::TextInput;
use crate::protocol::{Activity, SessionInfo, State};
use crate::shell;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

const SIDEBAR_WIDTH: u16 = 28;

/// Panes go side by side only while each is at least this wide, which fits
/// most agents' screens; narrower than that, they're stacked.
const MIN_PANE_WIDTH: u16 = 80;

/// Where each part of the TUI goes on a screen of a given size.
pub struct Areas {
    pub sidebar: Rect,
    /// One per pane, in the app's [`App::slots`] order: the selection's
    /// pane, then each split.
    pub panes: Vec<Rect>,
    pub footer: Rect,
}

impl Areas {
    /// Lays out a screen with `splits` sessions split off beside the
    /// selection's pane.
    pub fn new(screen: Rect, splits: usize) -> Areas {
        let [main, footer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(screen);
        let [sidebar, panes] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(main);
        Areas {
            sidebar,
            panes: pane_areas(panes, 1 + splits),
            footer,
        }
    }
}

/// Shares `area` out evenly between `count` panes: side by side when each
/// is still at least [`MIN_PANE_WIDTH`] wide, stacked otherwise.
pub fn pane_areas(area: Rect, count: usize) -> Vec<Rect> {
    let count = count.max(1);
    let constraints = vec![Constraint::Ratio(1, count as u32); count];
    let side_by_side = area.width / count as u16 >= MIN_PANE_WIDTH;
    let layout = if side_by_side {
        Layout::horizontal(constraints)
    } else {
        Layout::vertical(constraints)
    };
    layout.split(area).to_vec()
}

/// The inside of a pane's border, where its session's screen goes. The
/// session is sized to fit it exactly.
pub fn screen_area(pane: Rect) -> Rect {
    Block::bordered().inner(pane)
}

/// Draws the whole TUI. `panes` are the viewers of the sessions on screen.
pub fn draw(frame: &mut Frame, app: &App, panes: &[Pane]) {
    let areas = Areas::new(frame.area(), app.splits().len());
    draw_sidebar(frame, app, areas.sidebar);
    for (slot, area) in app.slots().into_iter().zip(&areas.panes) {
        draw_pane(frame, app, slot, *area, panes);
    }
    draw_footer(frame, app, areas.footer);
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(" sessions ")
        .border_style(border_style(app.focus() == Focus::Sidebar));
    let rows = app.rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| ListItem::new(sidebar_row(app, row)))
        .collect();
    // The selection is a session; find the row it's drawn on.
    let selected = app
        .selected_index()
        .and_then(|index| rows.iter().position(|row| *row == Row::Session(index)));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

/// One row of the sidebar. Headings name a project, then a worktree's
/// branch (`⌂` for the main worktree, `⎇` for a linked one); the sessions
/// sit indented under them.
fn sidebar_row<'a>(app: &'a App, row: &Row) -> Line<'a> {
    match row {
        Row::Project(name) => Line::from(name.clone()).bold(),
        Row::OutsideGit => Line::from("outside git").bold().dark_gray(),
        Row::Worktree { branch, main } => {
            let mark = if *main { "⌂ " } else { "⎇ " };
            let branch = branch.as_deref().unwrap_or("(detached)").to_string();
            Line::from(vec!["  ".into(), mark.dark_gray(), branch.into()])
        }
        Row::Directory(dir) => Line::from(format!("  {}", shell::home_relative(dir))).dark_gray(),
        Row::Session(index) => {
            let mut line = session_row(&app.sessions()[*index]);
            line.spans.insert(0, "    ".into());
            line
        }
    }
}

/// A session's row: a mark for what it's doing, its name, and a word on
/// it, unless it's simply running.
fn session_row(session: &SessionInfo) -> Line<'_> {
    let (mark, word) = match (&session.state, session.activity) {
        (State::Running, Some(Activity::Waiting)) => ("▲ ".yellow(), "waiting".yellow()),
        (State::Running, Some(Activity::Working)) => ("◐ ".cyan(), "working".dark_gray()),
        (State::Running, Some(Activity::Done)) => ("✓ ".green(), "done".dark_gray()),
        (State::Running, _) => ("▶ ".green(), "".into()),
        (State::Exited { code: 0 }, _) => ("■ ".dark_gray(), session.state.to_string().dark_gray()),
        (_, _) => ("■ ".red(), session.state.to_string().dark_gray()),
    };
    let mut row = Line::from(vec![mark, session.name.as_str().into()]);
    if !word.content.is_empty() {
        row.push_span(" ");
        row.push_span(word);
    }
    row
}

/// Draws the pane at `slot` in `area`: its session's screen, or a word on
/// why there's none to show. Only the focused pane shows the cursor.
fn draw_pane(frame: &mut Frame, app: &App, slot: Slot, area: Rect, panes: &[Pane]) {
    let focused = app.focus() == Focus::Pane(slot);
    let session = app.pane_session(slot);
    let mut block = Block::bordered().border_style(border_style(focused));
    if let Some(session) = session {
        block = block.title(pane_title(app, slot, session));
    }
    frame.render_widget(block, area);

    let screen = screen_area(area);
    let Some(session) = session else {
        if slot == Slot::Selected {
            draw_message(frame, "No sessions yet. Press n to start a shell.", screen);
        }
        return;
    };
    if slot == Slot::Selected && app.selected_is_own() {
        draw_message(frame, "This is the session crystal is running in.", screen);
        return;
    }
    if !app.shows_screen(slot) {
        // The selected session is split off: point at its pane rather than
        // draw it twice at two sizes.
        let message = format!("{} has a pane of its own", session.name);
        draw_message(frame, &message, screen);
        return;
    }
    // Until a viewer has attached to the session, there's nothing to show
    // yet.
    let Some(pane) = panes.iter().find(|pane| pane.session == session.name) else {
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

/// A pane's title: its session's name and, once it has ended, how. A split
/// showing the selected session says so, since the selection's own pane
/// points to it.
fn pane_title(app: &App, slot: Slot, session: &SessionInfo) -> String {
    let mut words = vec![session.name.clone()];
    if session.state != State::Running {
        words.push(session.state.to_string());
    }
    let selected = app.selected().is_some_and(|s| s.name == session.name);
    if slot != Slot::Selected && selected {
        words.push("selected".to_string());
    }
    format!(" {} ", words.join(" · "))
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
    if let Some(input) = app.branch_prompt() {
        draw_branch_prompt(frame, input, area);
        return;
    }
    let footer = if let Some(notice) = app.notice() {
        Line::from(notice.to_string()).red()
    } else if app.focus() == Focus::Sidebar {
        Line::from("j/k · enter type · tab pane · s split · n shell · w worktree · x kill · q quit")
            .dark_gray()
    } else {
        Line::from("typing into the session · ctrl+\\ back to the list").dark_gray()
    };
    frame.render_widget(footer, area);
}

/// Asks for the new worktree's branch, with the cursor in the answer.
fn draw_branch_prompt(frame: &mut Frame, input: &TextInput, area: Rect) {
    const QUESTION: &str = "branch for the new worktree: ";
    let line = Line::from(vec![QUESTION.cyan(), input.text().into()]);
    frame.render_widget(line, area);
    // The question is plain ASCII, so its length in bytes is its width.
    let column = area.x + (QUESTION.len() + input.cursor()) as u16;
    frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
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
    use crate::protocol::Worktree;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    /// Draws `app` on an 80 by 12 screen and returns it as lines of text.
    fn screen_text(app: &App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| draw(frame, app, &[])).unwrap();
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
            activity: None,
            worktree: None,
        }
    }

    /// The number of the first line that holds `text`.
    fn line_with(lines: &[String], text: &str) -> usize {
        let found = lines.iter().position(|line| line.contains(text));
        found.unwrap_or_else(|| panic!("{text:?} isn't on screen:\n{}", lines.join("\n")))
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
        assert!(line_with(&text, "▶ claude") < line_with(&text, "■ codex exited 1"));
        assert!(text[0].contains(" claude "), "the pane is titled after it");
    }

    #[test]
    fn the_sidebar_says_what_each_agent_is_doing() {
        let mut app = App::new(None);
        let mut sessions = Vec::new();
        for (name, activity) in [
            ("asks", Activity::Waiting),
            ("busy", Activity::Working),
            ("finished", Activity::Done),
            ("resting", Activity::Idle),
        ] {
            let mut session = session(name, State::Running);
            session.activity = Some(activity);
            sessions.push(session);
        }
        app.set_sessions(sessions);
        let text = screen_text(&app);
        line_with(&text, "▲ asks waiting");
        line_with(&text, "◐ busy working");
        line_with(&text, "✓ finished done");
        line_with(&text, "▶ resting ");
    }

    #[test]
    fn sessions_sit_under_their_project_and_worktree() {
        let in_worktree = |name: &str, branch: &str, main: bool| SessionInfo {
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: PathBuf::from("/code/app"),
                path: PathBuf::from(format!("/code/app/{branch}")),
                main,
                branch: Some(branch.into()),
            }),
            ..session(name, State::Running)
        };
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("fixer", "fix", false),
            in_worktree("planner", "main", true),
            session("shell", State::Running),
        ]);
        let text = screen_text(&app);
        let order = [
            line_with(&text, "│app"),
            line_with(&text, "⌂ main"),
            line_with(&text, "▶ planner"),
            line_with(&text, "⎇ fix"),
            line_with(&text, "▶ fixer"),
            line_with(&text, "outside git"),
            line_with(&text, "▶ shell"),
        ];
        assert!(
            order.is_sorted(),
            "out of order: {order:?}\n{}",
            text.join("\n")
        );
    }

    #[test]
    fn the_branch_prompt_takes_the_footer() {
        let mut app = App::new(None);
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let text = screen_text(&app);
        assert!(text[11].contains("branch for the new worktree: x"));
    }

    #[test]
    fn the_session_screen_sits_inside_the_panes_border() {
        let areas = Areas::new(Rect::new(0, 0, 80, 24), 0);
        assert_eq!(screen_area(areas.panes[0]), Rect::new(29, 1, 50, 21));
    }

    #[test]
    fn one_pane_takes_the_whole_area() {
        let area = Rect::new(28, 0, 52, 23);
        assert_eq!(pane_areas(area, 1), [area]);
    }

    #[test]
    fn panes_go_side_by_side_when_each_is_wide_enough() {
        let panes = pane_areas(Rect::new(0, 0, 240, 40), 3);
        assert_eq!(
            panes,
            [
                Rect::new(0, 0, 80, 40),
                Rect::new(80, 0, 80, 40),
                Rect::new(160, 0, 80, 40),
            ]
        );
    }

    #[test]
    fn panes_are_stacked_when_side_by_side_would_be_too_narrow() {
        let panes = pane_areas(Rect::new(0, 0, 159, 40), 2);
        assert_eq!(panes, [Rect::new(0, 0, 159, 20), Rect::new(0, 20, 159, 20)]);
    }

    #[test]
    fn a_selected_session_with_a_split_is_pointed_to_not_drawn_twice() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            session("left", State::Running),
            session("right", State::Running),
        ]);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let text = screen_text(&app).join("\n");
        assert!(text.contains("left has a pane of its own"));
        assert!(text.contains(" left · selected "));
    }
}
