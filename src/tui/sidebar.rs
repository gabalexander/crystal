//! The sidebar: every session under its project and its worktree, each
//! with a mark for what it's doing and how long ago that last changed.

use super::app::{App, Hit};
use super::groups::Row;
use super::status::Status;
use super::ui::Look;
use crate::protocol::SessionInfo;
use crate::shell;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// The room left at the sidebar's left edge, in line with the top bar and
/// the footer.
const MARGIN: &str = " ";

/// How far a worktree's line is indented: under its project's heading.
const WORKTREE_INDENT: &str = "   ";

/// How far a session row is indented: under its worktree's line.
const SESSION_INDENT: &str = "     ";

pub fn draw(frame: &mut Frame, app: &App, look: &Look, area: Rect) {
    let rows = app.rows();
    let first = offset(app, area.height);
    let selected = selected_row(app);
    let shown = rows.iter().enumerate().skip(first).take(area.height.into());
    for (index, row) in shown {
        let y = area.y + (index - first) as u16;
        let line_area = Rect::new(area.x, y, area.width, 1);
        let is_selected = selected == Some(index);
        if is_selected {
            frame
                .buffer_mut()
                .set_style(line_area, look.theme.selection);
        }
        let line = row_line(app, row, look, area.width, is_selected);
        frame.render_widget(line, line_area);
    }
}

/// Which sidebar row is on screen `row`, in a sidebar drawn in `area`.
pub fn hit(area: Rect, app: &App, row: u16) -> Hit {
    let index = offset(app, area.height) + usize::from(row - area.y);
    if index < app.rows().len() {
        Hit::SidebarRow(index)
    } else {
        Hit::Sidebar
    }
}

/// The first row on screen, when the rows don't all fit in `height`: the
/// list scrolls just far enough to keep the selection in sight. Drawing and
/// clicking both go by this, so a click lands on the row drawn there.
fn offset(app: &App, height: u16) -> usize {
    let height = usize::from(height.max(1));
    match selected_row(app) {
        Some(selected) if selected >= height => selected + 1 - height,
        _ => 0,
    }
}

/// The row the selected session is drawn on.
fn selected_row(app: &App) -> Option<usize> {
    let index = app.selected_index()?;
    app.rows()
        .iter()
        .position(|row| *row == Row::Session(index))
}

/// One row, fitted to `width` columns.
fn row_line<'a>(app: &'a App, row: &Row, look: &Look, width: u16, selected: bool) -> Line<'a> {
    let theme = look.theme;
    match row {
        Row::Project(name) => heading(name, Style::new().fg(theme.text), look, width),
        Row::OutsideGit => heading("outside git", Style::new().fg(theme.muted), look, width),
        Row::Worktree { branch, main } => {
            let mark = if *main { "⌂ " } else { "⎇ " };
            let branch = branch.as_deref().unwrap_or("(detached)");
            let room = usize::from(width).saturating_sub(WORKTREE_INDENT.len() + 2);
            Line::from(vec![
                Span::raw(WORKTREE_INDENT),
                Span::styled(mark, Style::new().fg(theme.muted)),
                Span::styled(fit(branch, room), Style::new().fg(theme.branch)),
            ])
        }
        Row::Directory(dir) => {
            let dir = shell::home_relative(dir);
            let room = usize::from(width).saturating_sub(WORKTREE_INDENT.len());
            Line::from(vec![
                Span::raw(WORKTREE_INDENT),
                Span::styled(fit(&dir, room), Style::new().fg(theme.muted)),
            ])
        }
        Row::Session(index) => session_line(&app.sessions()[*index], look, width, selected),
    }
}

/// A heading: the name in bold, then a thin rule nearly to the edge.
fn heading<'a>(name: &str, style: Style, look: &Look, width: u16) -> Line<'a> {
    let width = usize::from(width).saturating_sub(MARGIN.len());
    let name = fit(name, width);
    // A space before the rule, and one kept clear after it.
    let rule = width.saturating_sub(name.chars().count() + 2);
    Line::from(vec![
        Span::raw(MARGIN),
        Span::styled(name, style.add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::new().fg(look.theme.rule)),
    ])
}

/// A session's row: its mark, its name, and on the right how long ago it
/// changed. When the name and the time don't both fit, the time goes.
fn session_line<'a>(session: &SessionInfo, look: &Look, width: u16, selected: bool) -> Line<'a> {
    let theme = look.theme;
    let status = Status::of(session);
    let mut name_style = Style::new().fg(theme.text);
    if selected {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    // The indent, the mark and a space before the name; a space at the end.
    let room = usize::from(width).saturating_sub(SESSION_INDENT.len() + 2 + 1);
    let when = changed_ago(session, look.now);
    let name_width = session.name.chars().count();
    let fits_both = name_width + 1 + when.chars().count() <= room;

    let mut spans = vec![
        Span::raw(SESSION_INDENT),
        Span::styled(
            status.mark(look.spin),
            Style::new().fg(theme.status(status)),
        ),
        Span::raw(" "),
    ];
    if fits_both {
        let gap = room - name_width - when.chars().count();
        spans.push(Span::styled(session.name.clone(), name_style));
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(when, Style::new().fg(theme.muted)));
    } else {
        spans.push(Span::styled(fit(&session.name, room), name_style));
    }
    Line::from(spans)
}

/// How long ago the session changed, or nothing from a daemon that
/// doesn't say.
fn changed_ago(session: &SessionInfo, now: u64) -> String {
    if session.changed == 0 {
        return String::new();
    }
    ago(session.changed, now)
}

/// How long ago `then` was, at `now`, both in seconds since the Unix
/// epoch, in as few characters as it takes: "now", "45s", "12m", "3h",
/// "2d".
pub fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    let minute = 60;
    let hour = 60 * minute;
    let day = 24 * hour;
    if seconds < 10 {
        "now".to_string()
    } else if seconds < minute {
        format!("{seconds}s")
    } else if seconds < hour {
        format!("{}m", seconds / minute)
    } else if seconds < day {
        format!("{}h", seconds / hour)
    } else {
        format!("{}d", seconds / day)
    }
}

/// `text` cut down to `width` characters, ending in `…` when it's cut.
pub fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_in_the_last_ten_seconds_is_now() {
        assert_eq!(ago(1000, 1000), "now");
        assert_eq!(ago(1000, 1009), "now");
    }

    #[test]
    fn older_changes_are_counted_in_the_largest_whole_unit() {
        assert_eq!(ago(0, 45), "45s");
        assert_eq!(ago(0, 12 * 60 + 30), "12m");
        assert_eq!(ago(0, 3 * 3600 + 59 * 60), "3h");
        assert_eq!(ago(0, 2 * 86400 + 5), "2d");
    }

    #[test]
    fn a_clock_that_went_back_says_now() {
        assert_eq!(ago(2000, 1000), "now");
    }

    #[test]
    fn text_too_long_is_cut_with_an_ellipsis() {
        assert_eq!(fit("planner", 10), "planner");
        assert_eq!(fit("a-long-session-name", 8), "a-long-…");
        assert_eq!(fit("abc", 0), "");
    }
}
