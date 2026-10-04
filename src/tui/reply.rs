//! The reply box, Space in the sidebar or in a background task's pane: the
//! next thing to say to a session, sent without going into its pane, as
//! docket's Space does. A program in a terminal has it typed in and Enter
//! pressed after it, the way `crystal send` does; a background task takes
//! it as a follow-up, another run that carries its conversation on. It
//! comes from the user, so it goes as typed: no line says who sent it, and
//! it isn't counted against the most a session may send in a minute.
//!
//! The box keeps what's typed until the daemon has taken it: refused, say
//! because the agent is asking the user something, it's all still there,
//! with why. Its state and keys, kept apart from I/O, and its drawing.

use super::compose::Typed;
use super::text_area::TextArea;
use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

/// How big the box is, columns by rows, when there's room: docket's.
const SIZE: (u16, u16) = (76, 9);

/// The box a reply to a session is written in.
#[derive(Debug)]
pub struct ReplyBox {
    /// The session it's for, by name.
    pub name: String,
    /// What sending it does to that session, in a few words.
    pub label: String,
    pub text: TextArea,
    /// Whether it's on its way to the session.
    pub sending: bool,
    /// Why the daemon didn't take it.
    pub problem: Option<String>,
}

impl ReplyBox {
    /// A box for a reply to the session called `name`; `label` says what
    /// sending it does.
    pub fn new(name: &str, label: &str) -> ReplyBox {
        ReplyBox {
            name: name.to_string(),
            label: label.to_string(),
            text: TextArea::default(),
            sending: false,
            problem: None,
        }
    }

    /// Enter sends what's written, or, with nothing written, puts the box
    /// away; Alt+Enter, Shift+Enter and Ctrl+J break the line; Esc puts it
    /// away. While it's on its way, the keys wait.
    pub fn on_key(&mut self, key: &KeyEvent) -> Typed<String> {
        if self.sending {
            return Typed::Stay;
        }
        let newline = key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Typed::Cancel,
            KeyCode::Enter if newline => self.text.newline(),
            KeyCode::Char('j') if ctrl => self.text.newline(),
            KeyCode::Enter if self.text.text().trim().is_empty() => return Typed::Cancel,
            KeyCode::Enter => {
                self.sending = true;
                self.problem = None;
                return Typed::Send(self.text.text().trim_end().to_string());
            }
            KeyCode::Up => self.text.line_up(),
            KeyCode::Down => self.text.line_down(),
            _ => {
                self.text.on_key(key);
            }
        }
        Typed::Stay
    }

    pub fn on_paste(&mut self, text: &str) {
        if !self.sending {
            self.text.insert_str(text);
        }
    }

    /// The daemon didn't take it: it's back to be changed or sent again.
    pub fn refused(&mut self, reason: String) {
        self.sending = false;
        self.problem = Some(reason);
    }
}

/// Draws the box in the middle of `area`, what's behind it dimmed: its
/// title on the top border, the label, what's written, whether it's on its
/// way or why it was refused, and its keys on the bottom border.
pub fn draw(frame: &mut Frame, reply: &ReplyBox, theme: &Theme, area: Rect) {
    frame
        .buffer_mut()
        .set_style(area, Style::new().add_modifier(Modifier::DIM));
    let place = place(area);
    frame.render_widget(Clear, place);
    let title = Line::styled(
        format!(" Reply · {} ", reply.name),
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.rule))
        .title(title)
        .title_bottom(Line::styled(
            hint(place.width),
            Style::new().fg(theme.muted),
        ))
        .style(Style::new().bg(theme.panel).fg(theme.text));
    let inside = block.inner(place);
    frame.render_widget(block, place);

    let width = usize::from(inside.width.saturating_sub(2)).max(1);
    let rows = usize::from(inside.height.saturating_sub(2)).max(1);
    let mut lines = vec![Line::styled(
        format!(" {}", reply.label),
        Style::new().fg(theme.muted).add_modifier(Modifier::DIM),
    )];
    let all = reply.text.rows(width);
    let (row, column) = reply.text.cursor_at(width);
    let first = (row + 1).saturating_sub(rows);
    for &shown in all.iter().skip(first).take(rows) {
        let text = format!(" {}", reply.text.row_text(shown));
        lines.push(Line::styled(text, Style::new().fg(theme.text)));
    }
    while lines.len() < rows + 1 {
        lines.push(Line::default());
    }
    lines.push(match (reply.sending, &reply.problem) {
        (true, _) => Line::styled(" sending…", Style::new().fg(theme.muted)),
        (false, Some(problem)) => Line::from(Span::styled(
            format!(" {problem}"),
            Style::new().fg(theme.failed),
        )),
        (false, None) => Line::default(),
    });
    frame.render_widget(Paragraph::new(lines), inside);
    if !reply.sending {
        let x = inside.x + 1 + (column as u16).min(inside.width.saturating_sub(2));
        let y = inside.y + 1 + (row - first) as u16;
        frame.set_cursor_position((x, y));
    }
}

/// Where the box goes in `area`: in the middle, as big as [`SIZE`] or as
/// `area` allows.
fn place(area: Rect) -> Rect {
    let width = SIZE.0.min(area.width);
    let height = SIZE.1.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// The keys on the bottom border, as many words as fit in `width`.
fn hint(width: u16) -> &'static str {
    if width >= 55 {
        " enter send · alt+enter newline · esc cancel "
    } else if width >= 40 {
        " enter send · ^J newline · esc cancel "
    } else {
        " esc · ^J · enter "
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_into(reply: &mut ReplyBox, text: &str) {
        for c in text.chars() {
            reply.on_key(&key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_reply_keeps_its_lines_and_waits_for_the_daemon() {
        let mut reply = ReplyBox::new("fixer", "a follow-up");
        type_into(&mut reply, "now the docs");
        reply.on_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        type_into(&mut reply, "and the");
        reply.on_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        type_into(&mut reply, "changelog");
        assert_eq!(
            reply.on_key(&key(KeyCode::Enter)),
            Typed::Send("now the docs\nand the\nchangelog".to_string())
        );
        assert!(reply.sending);
        // On its way, the keys wait.
        assert_eq!(reply.on_key(&key(KeyCode::Esc)), Typed::Stay);
        type_into(&mut reply, "x");
        assert_eq!(reply.text.text(), "now the docs\nand the\nchangelog");

        reply.refused("agent_blocked: fixer is asking to use Bash".into());
        assert!(!reply.sending);
        assert_eq!(reply.text.text(), "now the docs\nand the\nchangelog");
        assert_eq!(reply.on_key(&key(KeyCode::Esc)), Typed::Cancel);
    }

    #[test]
    fn enter_with_nothing_written_sends_nothing() {
        let mut reply = ReplyBox::new("fixer", "a follow-up");
        assert_eq!(reply.on_key(&key(KeyCode::Enter)), Typed::Cancel);
        type_into(&mut reply, "  ");
        assert_eq!(reply.on_key(&key(KeyCode::Enter)), Typed::Cancel);
        assert!(!reply.sending);
    }

    #[test]
    fn the_box_says_who_it_s_for_what_sending_does_and_why_it_was_refused() {
        let theme = Theme::new(ThemeName::Dark, false);
        let mut reply = ReplyBox::new("fixer", "a follow-up: its next run");
        type_into(&mut reply, "carry on");
        reply.on_key(&key(KeyCode::Enter));
        reply.refused("fixer has ended".into());
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| draw(frame, &reply, &theme, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..20)
            .map(|y| (0..100).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        let text = lines.join("\n");
        assert!(text.contains("Reply · fixer"), "{text}");
        assert!(text.contains("a follow-up: its next run"), "{text}");
        assert!(text.contains("carry on"), "{text}");
        assert!(text.contains("fixer has ended"), "{text}");
        assert!(text.contains("enter send · alt+enter newline · esc cancel"));
        // In the middle, 76 columns by 9 rows.
        let top = lines
            .iter()
            .position(|line| line.contains("Reply"))
            .unwrap();
        assert_eq!(top, 5);
        assert_eq!(lines[top].find('╭'), Some(12));
    }

    #[test]
    fn the_box_fits_a_small_screen() {
        let small = place(Rect::new(0, 0, 40, 6));
        assert_eq!(small, Rect::new(0, 0, 40, 6));
        assert_eq!(hint(40), " enter send · ^J newline · esc cancel ");
        assert_eq!(hint(30), " esc · ^J · enter ");
    }
}
