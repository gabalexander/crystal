//! Writing back to the forge from its views: the box a comment is written
//! in, and the form an issue's title and text are changed in. Each keeps
//! what's typed until the forge has taken it: while it's on its way the
//! keys wait, and if the forge refuses it, it's all still there, with why.
//! Their state and keys, kept apart from I/O, and their drawing.

use super::text_area::TextArea;
use super::text_input::TextInput;
use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// What a key in the box or the form asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed<T> {
    Stay,
    /// Put it away, sending nothing.
    Cancel,
    /// Send this to the forge, and wait for its answer.
    Send(T),
}

/// The box a comment is written in, on an issue or a pull request.
#[derive(Debug)]
pub struct CommentBox {
    /// The issue's or pull request's number: it stays on that one, even if
    /// the list changes under it.
    pub number: u64,
    pub text: TextArea,
    /// Whether it's on its way to the forge.
    pub sending: bool,
    /// Why the forge didn't take it.
    pub problem: Option<String>,
}

impl CommentBox {
    /// A box for a comment on issue or pull request `number`.
    pub fn new(number: u64) -> CommentBox {
        CommentBox {
            number,
            text: TextArea::default(),
            sending: false,
            problem: None,
        }
    }

    /// Enter sends what's written, or, with nothing written, puts the box
    /// away; Alt+Enter breaks the line; Esc puts it away.
    pub fn on_key(&mut self, key: &KeyEvent) -> Typed<String> {
        if self.sending {
            return Typed::Stay;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return Typed::Cancel,
            KeyCode::Enter if alt => self.text.newline(),
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

    /// The forge didn't take it: it's back to be changed or sent again.
    pub fn refused(&mut self, reason: String) {
        self.sending = false;
        self.problem = Some(reason);
    }
}

/// Which of the form's fields has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Body,
}

/// The form an issue's title and text are changed in, starting from what
/// they are.
#[derive(Debug)]
pub struct IssueForm {
    pub number: u64,
    pub title: TextInput,
    pub body: TextArea,
    pub field: Field,
    /// The title and text it started from: sending them back unchanged
    /// would do nothing.
    was: (String, String),
    pub sending: bool,
    pub problem: Option<String>,
}

impl IssueForm {
    pub fn new(number: u64, title: &str, body: &str) -> IssueForm {
        let mut text = TextArea::default();
        text.set_text(body);
        IssueForm {
            number,
            title: TextInput::with_text(title),
            body: text,
            field: Field::Title,
            was: (title.to_string(), body.to_string()),
            sending: false,
            problem: None,
        }
    }

    /// Tab, or Up and Down at the edges, go between the title and the
    /// text; Enter saves both, and Alt+Enter breaks a line of the text;
    /// Esc puts the form away. Unchanged, Enter puts it away too.
    pub fn on_key(&mut self, key: &KeyEvent) -> Typed<(String, String)> {
        if self.sending {
            return Typed::Stay;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match (key.code, self.field) {
            (KeyCode::Esc, _) => return Typed::Cancel,
            (KeyCode::Tab | KeyCode::BackTab, Field::Title) => self.field = Field::Body,
            (KeyCode::Tab | KeyCode::BackTab, Field::Body) => self.field = Field::Title,
            (KeyCode::Enter, Field::Body) if alt => self.body.newline(),
            (KeyCode::Enter, _) => return self.save(),
            (KeyCode::Down, Field::Title) => self.field = Field::Body,
            (KeyCode::Up, Field::Body) if self.body.on_first_line() => self.field = Field::Title,
            (KeyCode::Up, Field::Body) => self.body.line_up(),
            (KeyCode::Down, Field::Body) => self.body.line_down(),
            (_, Field::Title) => self.title.on_key(key),
            (_, Field::Body) => {
                self.body.on_key(key);
            }
        }
        Typed::Stay
    }

    pub fn on_paste(&mut self, text: &str) {
        if self.sending {
            return;
        }
        match self.field {
            // A title is one line.
            Field::Title => self.title.insert_str(&text.replace(['\r', '\n'], " ")),
            Field::Body => self.body.insert_str(text),
        }
    }

    /// The forge didn't take it: it's back to be changed or saved again.
    pub fn refused(&mut self, reason: String) {
        self.sending = false;
        self.problem = Some(reason);
    }

    fn save(&mut self) -> Typed<(String, String)> {
        let title = self.title.text().trim().to_string();
        let body = self.body.text().trim_end().to_string();
        if title.is_empty() {
            self.problem = Some("an issue needs a title".to_string());
            self.field = Field::Title;
            return Typed::Stay;
        }
        if (title.as_str(), body.as_str()) == (self.was.0.trim(), self.was.1.trim_end()) {
            return Typed::Cancel;
        }
        self.sending = true;
        self.problem = None;
        Typed::Send((title, body))
    }
}

/// Draws the comment box in `area`: `heading`, what's written, and below
/// it whether it's on its way or why it was refused.
pub fn draw_comment_box(
    frame: &mut Frame,
    theme: &Theme,
    comment: &CommentBox,
    heading: &str,
    area: Rect,
) {
    let mut lines = vec![heading_line(heading, theme)];
    let rows = area.height.saturating_sub(3);
    let (text, cursor) = text_lines(&comment.text, theme, area.width, rows);
    lines.extend(text);
    lines.push(Line::default());
    lines.push(status_line(
        comment.sending,
        comment.problem.as_deref(),
        "posting…",
        theme,
    ));
    frame.render_widget(Paragraph::new(lines), area);
    if !comment.sending {
        let (row, column) = cursor;
        frame.set_cursor_position((area.x + 1 + column, area.y + 1 + row));
    }
}

/// Draws the issue form in `area`: the title on a line of its own, the
/// text under it, and below them whether it's being saved or why it
/// wasn't.
pub fn draw_issue_form(frame: &mut Frame, theme: &Theme, form: &IssueForm, area: Rect) {
    let label = |name: &str, field: Field| {
        let color = if form.field == field {
            theme.accent
        } else {
            theme.muted
        };
        Span::styled(format!(" {name:<7}"), Style::new().fg(color))
    };
    let title = Line::from(vec![
        label("title", Field::Title),
        Span::styled(form.title.text().to_string(), Style::new().fg(theme.text)),
    ]);
    let mut lines = vec![
        heading_line(&format!("edit issue #{}", form.number), theme),
        title,
        Line::from(label("text", Field::Body)),
    ];
    let rows = area.height.saturating_sub(5);
    let (text, cursor) = text_lines(&form.body, theme, area.width, rows);
    lines.extend(text);
    lines.push(Line::default());
    lines.push(status_line(
        form.sending,
        form.problem.as_deref(),
        "saving…",
        theme,
    ));
    frame.render_widget(Paragraph::new(lines), area);
    if form.sending {
        return;
    }
    let at = match form.field {
        Field::Title => (area.x + 8 + form.title.cursor() as u16, area.y + 1),
        Field::Body => (area.x + 1 + cursor.1, area.y + 3 + cursor.0),
    };
    frame.set_cursor_position((at.0.min(area.right().saturating_sub(1)), at.1));
}

fn heading_line<'a>(heading: &str, theme: &Theme) -> Line<'a> {
    Line::styled(
        format!(" {heading}"),
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    )
}

/// What's on its way, or why it was refused, or nothing.
fn status_line<'a>(
    sending: bool,
    problem: Option<&str>,
    on_its_way: &str,
    theme: &Theme,
) -> Line<'a> {
    match (sending, problem) {
        (true, _) => Line::styled(format!(" {on_its_way}"), Style::new().fg(theme.muted)),
        (false, Some(problem)) => {
            Line::styled(format!(" {problem}"), Style::new().fg(theme.failed))
        }
        (false, None) => Line::default(),
    }
}

/// The text of `area`'s rows, a column in, at most `rows` of them and
/// scrolled to keep the cursor in sight, and the cursor's row and column
/// among them.
fn text_lines<'a>(
    text: &TextArea,
    theme: &Theme,
    width: u16,
    rows: u16,
) -> (Vec<Line<'a>>, (u16, u16)) {
    let width = usize::from(width.saturating_sub(2)).max(1);
    let rows = usize::from(rows.max(1));
    let all = text.rows(width);
    let (row, column) = text.cursor_at(width);
    let first = (row + 1).saturating_sub(rows);
    let lines = all
        .iter()
        .skip(first)
        .take(rows)
        .map(|&row| {
            Line::styled(
                format!(" {}", text.row_text(row)),
                Style::new().fg(theme.text),
            )
        })
        .collect();
    (lines, ((row - first) as u16, column as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_into<T>(on_key: &mut impl FnMut(&KeyEvent) -> Typed<T>, text: &str) {
        for c in text.chars() {
            on_key(&key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_comment_is_sent_whole_and_waits_for_the_forge() {
        let mut comment = CommentBox::new(57);
        type_into(&mut |key| comment.on_key(key), "Looks right.");
        comment.on_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        type_into(&mut |key| comment.on_key(key), "Ship it");
        assert_eq!(
            comment.on_key(&key(KeyCode::Enter)),
            Typed::Send("Looks right.\nShip it".to_string())
        );
        assert!(comment.sending);
        // Keys wait while it's on its way.
        assert_eq!(comment.on_key(&key(KeyCode::Esc)), Typed::Stay);
        comment.refused("gh: not logged in".to_string());
        assert_eq!(comment.text.text(), "Looks right.\nShip it");
        assert_eq!(comment.problem.as_deref(), Some("gh: not logged in"));
        assert_eq!(comment.on_key(&key(KeyCode::Esc)), Typed::Cancel);
    }

    #[test]
    fn enter_on_an_empty_comment_puts_the_box_away() {
        let mut comment = CommentBox::new(57);
        comment.on_key(&key(KeyCode::Char(' ')));
        assert_eq!(comment.on_key(&key(KeyCode::Enter)), Typed::Cancel);
    }

    #[test]
    fn the_form_saves_a_changed_title_and_text() {
        let mut form = IssueForm::new(15, "Login loops", "It loops.");
        assert_eq!(form.field, Field::Title);
        type_into(&mut |key| form.on_key(key), " forever");
        form.on_key(&key(KeyCode::Tab));
        assert_eq!(form.field, Field::Body);
        form.on_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        type_into(&mut |key| form.on_key(key), "Every time.");
        assert_eq!(
            form.on_key(&key(KeyCode::Enter)),
            Typed::Send((
                "Login loops forever".to_string(),
                "It loops.\nEvery time.".to_string()
            ))
        );
    }

    #[test]
    fn the_form_wont_save_without_a_title_and_unchanged_just_closes() {
        let mut form = IssueForm::new(15, "Login loops", "It loops.");
        assert_eq!(form.on_key(&key(KeyCode::Enter)), Typed::Cancel);
        form.title = TextInput::with_text("  ");
        form.field = Field::Body;
        assert_eq!(form.on_key(&key(KeyCode::Enter)), Typed::Stay);
        assert_eq!(form.problem.as_deref(), Some("an issue needs a title"));
        assert_eq!(form.field, Field::Title);
    }

    #[test]
    fn up_and_down_go_between_the_title_and_the_text() {
        let mut form = IssueForm::new(15, "Login loops", "one\ntwo");
        form.on_key(&key(KeyCode::Down));
        assert_eq!(form.field, Field::Body);
        // The cursor starts at the end of the text, on its last line.
        form.on_key(&key(KeyCode::Up));
        assert_eq!(form.field, Field::Body);
        form.on_key(&key(KeyCode::Up));
        assert_eq!(form.field, Field::Title);
    }
}
