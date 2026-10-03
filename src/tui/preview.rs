//! A file's preview, as the file finder and the tree browser show it: its
//! lines highlighted and numbered, or, for markdown, the page it makes,
//! which `Ctrl+R` flips to its source and back. The file is read and
//! highlighted off the event loop ([`read`]); a page is laid out again
//! whenever the width it's drawn at changes, before it's drawn, so drawing
//! only reads.

use super::app::{Action, Loading};
use super::ui::{self, Look};
use crate::markdown::{self, PageLine};
use crate::syntax::{Highlighter, Runs, TokenKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// How much of a file a preview reads, and how many of its lines it keeps.
const MOST_BYTES: u64 = 1 << 20;
const MOST_LINES: usize = 10_000;

/// git's test for a binary file: a zero byte in its first 8 KiB.
const BINARY_TEST_BYTES: usize = 8 * 1024;

/// How many lines a notch of the mouse wheel scrolls.
const WHEEL_LINES: usize = 3;

/// What a preview shows: a file's lines, or the names in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    /// The file's text, for a page to be laid out from.
    text: String,
    lines: Vec<Runs>,
    /// Whether these are a file's lines, which are numbered, rather than a
    /// directory's names.
    numbered: bool,
    markdown: bool,
    /// Only the start of a long file was read.
    cut_short: bool,
}

impl Content {
    /// The names in a directory, a line each.
    pub fn listing(names: Vec<String>) -> Content {
        Content {
            text: String::new(),
            lines: names
                .into_iter()
                .map(|name| vec![(TokenKind::Text, name)])
                .collect(),
            numbered: false,
            markdown: false,
            cut_short: false,
        }
    }
}

/// Reads the file at `path` in the worktree at `dir`, as much of it as a
/// preview shows, and highlights it: run off the event loop. A file that
/// can't be shown says why.
pub fn read(dir: &Path, path: &str) -> Result<Content, String> {
    let file = std::fs::File::open(dir.join(path)).map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    file.take(MOST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if bytes.iter().take(BINARY_TEST_BYTES).any(|byte| *byte == 0) {
        return Err("a binary file".to_string());
    }
    let mut cut_short = bytes.len() as u64 > MOST_BYTES;
    bytes.truncate(MOST_BYTES as usize);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.trim().is_empty() {
        return Err("an empty file".to_string());
    }
    let mut lines: Vec<Runs> = highlight(path, &text).take(MOST_LINES + 1).collect();
    if lines.len() > MOST_LINES {
        lines.truncate(MOST_LINES);
        cut_short = true;
    }
    let markdown = markdown::is_markdown_path(path);
    Ok(Content {
        // Only a page is laid out from the text.
        text: if markdown { text } else { String::new() },
        lines,
        numbered: true,
        markdown,
        cut_short,
    })
}

/// `text`'s lines highlighted as the file at `path` would be, tabs as
/// spaces.
pub fn highlight<'a>(path: &str, text: &'a str) -> impl Iterator<Item = Runs> + 'a {
    let mut highlighter = Highlighter::for_path(path);
    text.lines()
        .map(move |line| highlighter.line(&line.replace('\t', "    ")))
}

pub struct Preview {
    /// The worktree the files are in.
    dir: PathBuf,
    /// What's shown, by its path from the top of the worktree.
    path: Option<String>,
    content: Loading<Content>,
    /// Whether a markdown file shows its source rather than its page. It
    /// stays as it is from one file to the next.
    source: bool,
    /// The page laid out, and the width it was laid out for.
    page: Option<(usize, Vec<PageLine>)>,
    /// How many lines down it's scrolled.
    pub scroll: usize,
    /// The size of the area it's drawn in, as `(rows, columns)`.
    size: (u16, u16),
}

impl Preview {
    /// A preview of files in the worktree at `dir`, with nothing to show
    /// yet.
    pub fn new(dir: PathBuf) -> Preview {
        Preview {
            dir,
            path: None,
            content: Loading::Reading,
            source: false,
            page: None,
            scroll: 0,
            size: (24, 80),
        }
    }

    /// The path of what's shown.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Shows the file at `path`, from its top, once it's read: what reading
    /// it takes, unless it's the one shown already.
    pub fn show(&mut self, path: &str) -> Option<Action> {
        if self.path.as_deref() == Some(path) {
            return None;
        }
        self.start(path, Loading::Reading);
        Some(self.read_again())
    }

    /// What reading the file shown again takes, to show what's changed in
    /// it. What's on screen stays until it's read.
    pub fn read_again(&self) -> Action {
        Action::ReadPreview {
            dir: self.dir.clone(),
            path: self.path.clone().unwrap_or_default(),
        }
    }

    /// Shows the names in the directory at `path`.
    pub fn show_listing(&mut self, path: &str, names: Vec<String>) {
        if self.path.as_deref() != Some(path) {
            self.start(path, Loading::Read(Content::listing(names)));
        }
    }

    /// Shows nothing at all.
    pub fn clear(&mut self) {
        self.path = None;
        self.content = Loading::Reading;
        self.page = None;
        self.scroll = 0;
    }

    fn start(&mut self, path: &str, content: Loading<Content>) {
        self.path = Some(path.to_string());
        self.content = content;
        self.page = None;
        self.scroll = 0;
        self.lay_out();
    }

    /// Takes a file that's been read, if it's still the one shown. Read
    /// again, it keeps its place.
    pub fn read_done(&mut self, dir: &Path, path: &str, read: Result<Content, String>) {
        if dir != self.dir || self.path.as_deref() != Some(path) {
            return;
        }
        self.content = match read {
            Ok(content) => Loading::Read(content),
            Err(why) => Loading::Failed(why),
        };
        self.page = None;
        self.lay_out();
        self.scroll = self.scroll.min(self.last_scroll());
    }

    /// The size of the area it's drawn in, as `(rows, columns)`.
    pub fn set_size(&mut self, size: (u16, u16)) {
        self.size = size;
        self.lay_out();
        self.scroll = self.scroll.min(self.last_scroll());
    }

    /// The line of the file at `path` at the top of the preview, when it
    /// shows that file's lines scrolled down: where an editor opening it
    /// starts.
    pub fn top_line(&self, path: &str) -> Option<usize> {
        let shown = self.path.as_deref() == Some(path) && self.page().is_none();
        let numbered = matches!(&self.content, Loading::Read(content) if content.numbered);
        (shown && numbered && self.scroll > 0).then_some(self.scroll + 1)
    }

    /// Whether what's shown is a markdown file, which `Ctrl+R` flips
    /// between its page and its source.
    pub fn is_markdown(&self) -> bool {
        matches!(&self.content, Loading::Read(content) if content.markdown)
    }

    /// Whether a markdown file's source is shown rather than its page.
    pub fn shows_source(&self) -> bool {
        self.source
    }

    /// Flips a markdown file between its page and its source, from the
    /// top.
    pub fn flip(&mut self) {
        if self.is_markdown() {
            self.source = !self.source;
            self.scroll = 0;
            self.lay_out();
        }
    }

    /// Lays the page out for the width it's drawn at, if it's a page that's
    /// shown and it hasn't been laid out for that width yet.
    fn lay_out(&mut self) {
        let Some(text) = self.page_text() else {
            return;
        };
        let width = page_width(self.size.1);
        if self.page.as_ref().is_none_or(|(laid, _)| *laid != width) {
            self.page = Some((width, markdown::render(text, width)));
        }
    }

    /// The text of the markdown page that's shown, if one is.
    fn page_text(&self) -> Option<&str> {
        match &self.content {
            Loading::Read(content) if content.markdown && !self.source => Some(&content.text),
            _ => None,
        }
    }

    /// The page as it's laid out, when a page is shown.
    fn page(&self) -> Option<&[PageLine]> {
        self.page_text()?;
        self.page.as_ref().map(|(_, lines)| lines.as_slice())
    }

    /// How many lines there are to scroll through.
    fn line_count(&self) -> usize {
        match (&self.content, self.page()) {
            (_, Some(page)) => page.len(),
            (Loading::Read(content), None) => content.lines.len(),
            _ => 0,
        }
    }

    /// How many lines are on screen: all but the title's.
    fn visible_lines(&self) -> usize {
        usize::from(self.size.0.saturating_sub(1)).max(1)
    }

    /// A page: what's on screen, less a line kept from the last.
    fn page_size(&self) -> usize {
        self.visible_lines().saturating_sub(1).max(1)
    }

    /// The furthest down it scrolls: its last line at the bottom.
    fn last_scroll(&self) -> usize {
        self.line_count().saturating_sub(self.visible_lines())
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll = (self.scroll + lines).min(self.last_scroll());
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    /// A notch of the mouse wheel, up or down.
    pub fn wheel(&mut self, up: bool) {
        if up {
            self.scroll_up(WHEEL_LINES);
        } else {
            self.scroll_down(WHEEL_LINES);
        }
    }

    /// Scrolls for `key` the way the diff view does: `Space`, `PageDown`
    /// and `PageUp` page, `Home` and `End` go to the ends, and
    /// `Shift+↓`/`Shift+↑` go a line. Returns whether it was one of those.
    pub fn scroll_key(&mut self, key: &KeyEvent) -> bool {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.page_size();
        match key.code {
            KeyCode::Char(' ') if shift => self.scroll_up(page),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_down(page),
            KeyCode::PageUp => self.scroll_up(page),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = self.last_scroll(),
            KeyCode::Down if shift => self.scroll_down(1),
            KeyCode::Up if shift => self.scroll_up(1),
            _ => return false,
        }
        true
    }
}

/// How wide a page is laid out in an area `columns` wide: a column is kept
/// clear on each side.
fn page_width(columns: u16) -> usize {
    usize::from(columns.saturating_sub(2)).max(1)
}

/// The footer's word for what `Ctrl+R` flips a markdown file to, when one
/// is shown.
pub fn flip_hint(preview: &Preview) -> Option<(&'static str, &'static str)> {
    if !preview.is_markdown() {
        return None;
    }
    let to = if preview.shows_source() {
        "page"
    } else {
        "source"
    };
    Some(("ctrl+r", to))
}

/// The path on the first row, then the lines from where it's scrolled to.
pub fn draw(frame: &mut Frame, preview: &Preview, look: &Look, area: Rect) {
    let theme = look.theme;
    let Some(path) = preview.path() else {
        return;
    };
    let mut title = vec![
        Span::raw(" "),
        Span::styled(
            path.to_string(),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Loading::Read(content) = &preview.content
        && content.cut_short
    {
        title.push(Span::styled(
            " · too long: only its start",
            Style::new().fg(theme.muted),
        ));
    }
    frame.render_widget(Line::from(title), Rect::new(area.x, area.y, area.width, 1));
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    match &preview.content {
        Loading::Reading => {}
        Loading::Failed(why) => ui::draw_message(frame, look, why, body),
        Loading::Read(content) => {
            let shown = usize::from(body.height);
            let lines: Vec<Line> = match preview.page() {
                Some(page) => page
                    .iter()
                    .skip(preview.scroll)
                    .take(shown)
                    .map(|line| page_line(line, look))
                    .collect(),
                None => source_lines(content, preview.scroll, shown, look),
            };
            frame.render_widget(Paragraph::new(lines), body);
        }
    }
}

/// A line of a page, a column in from the left.
fn page_line<'a>(line: &PageLine, look: &Look) -> Line<'a> {
    let pieces = line
        .iter()
        .map(|piece| Span::styled(piece.text.clone(), look.theme.mark(piece.mark)));
    Line::from_iter(std::iter::once(Span::raw(" ")).chain(pieces))
}

/// `shown` of the content's lines from `first`, highlighted, a file's with
/// their numbers.
fn source_lines<'a>(content: &Content, first: usize, shown: usize, look: &Look) -> Vec<Line<'a>> {
    let theme = look.theme;
    let numbers = content.lines.len().to_string().len();
    let lines = content.lines.iter().enumerate().skip(first).take(shown);
    lines
        .map(|(index, runs)| {
            let margin = if content.numbered {
                format!(" {:>numbers$}  ", index + 1)
            } else {
                " ".to_string()
            };
            let runs = runs_spans(runs, look);
            Line::from_iter(
                std::iter::once(Span::styled(margin, Style::new().fg(theme.muted))).chain(runs),
            )
        })
        .collect()
}

/// A highlighted line's runs, in the theme's colors.
pub fn runs_spans<'a>(runs: &Runs, look: &Look) -> Vec<Span<'a>> {
    runs.iter()
        .map(|(kind, text)| Span::styled(text.clone(), look.theme.token(*kind)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A file of `count` numbered lines, read.
    fn lines(count: usize) -> Content {
        Content {
            text: String::new(),
            lines: (1..=count)
                .map(|n| vec![(TokenKind::Text, format!("line {n}"))])
                .collect(),
            numbered: true,
            markdown: false,
            cut_short: false,
        }
    }

    fn page(text: &str) -> Content {
        Content {
            text: text.to_string(),
            lines: text
                .lines()
                .map(|line| vec![(TokenKind::Text, line.to_string())])
                .collect(),
            numbered: true,
            markdown: true,
            cut_short: false,
        }
    }

    /// A preview showing `content` as `path`, in an area with ten lines
    /// under its title.
    fn showing(path: &str, content: Content) -> Preview {
        let mut preview = Preview::new(PathBuf::from("/code/app"));
        preview.show(path);
        preview.read_done(Path::new("/code/app"), path, Ok(content));
        preview.set_size((11, 40));
        preview
    }

    #[test]
    fn a_file_is_read_once_and_only_its_own_read_is_taken() {
        let mut preview = Preview::new(PathBuf::from("/code/app"));
        let read = preview.show("a.rs");
        assert_eq!(
            read,
            Some(Action::ReadPreview {
                dir: PathBuf::from("/code/app"),
                path: "a.rs".into(),
            })
        );
        assert_eq!(preview.show("a.rs"), None, "already shown");
        preview.show("b.rs");
        preview.read_done(Path::new("/code/app"), "a.rs", Ok(lines(3)));
        assert_eq!(preview.content, Loading::Reading, "a read too late");
        preview.read_done(Path::new("/code/app"), "b.rs", Ok(lines(3)));
        assert_eq!(preview.line_count(), 3);
    }

    #[test]
    fn read_again_a_file_keeps_its_place() {
        let mut preview = showing("a.rs", lines(30));
        preview.scroll_down(12);
        preview.read_done(Path::new("/code/app"), "a.rs", Ok(lines(30)));
        assert_eq!(preview.scroll, 12);
        // Shorter now: as far down as it goes.
        preview.read_done(Path::new("/code/app"), "a.rs", Ok(lines(15)));
        assert_eq!(preview.scroll, 5);
    }

    #[test]
    fn the_keys_scroll_it_the_way_the_diff_view_does() {
        // Thirty lines, ten on screen.
        let mut preview = showing("a.rs", lines(30));
        assert!(preview.scroll_key(&key(KeyCode::Char(' '))));
        assert_eq!(preview.scroll, 9);
        preview.scroll_key(&key(KeyCode::PageDown));
        preview.scroll_key(&key(KeyCode::PageDown));
        assert_eq!(preview.scroll, 20, "no further than the last page");
        preview.scroll_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::SHIFT));
        assert_eq!(preview.scroll, 11);
        preview.scroll_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(preview.scroll, 10);
        preview.scroll_key(&key(KeyCode::Home));
        assert_eq!(preview.scroll, 0);
        preview.scroll_key(&key(KeyCode::End));
        assert_eq!(preview.scroll, 20);
        assert!(!preview.scroll_key(&key(KeyCode::Char('j'))));
        preview.wheel(true);
        assert_eq!(preview.scroll, 20 - WHEEL_LINES);
    }

    #[test]
    fn the_top_line_is_where_its_scrolled_to_in_a_files_own_lines() {
        let mut preview = showing("a.rs", lines(30));
        assert_eq!(preview.top_line("a.rs"), None, "at the top");
        preview.scroll_down(5);
        assert_eq!(preview.top_line("a.rs"), Some(6));
        assert_eq!(preview.top_line("b.rs"), None, "another file");
        let mut page = showing("notes.md", page(&"words\n\n".repeat(30)));
        page.scroll_down(5);
        assert_eq!(
            page.top_line("notes.md"),
            None,
            "a page isn't the file's lines"
        );
    }

    #[test]
    fn markdown_is_a_page_until_its_flipped_to_its_source() {
        let mut preview = showing("README.md", page("# Title\n\nSome words."));
        assert!(preview.is_markdown() && !preview.shows_source());
        // The heading, its rule, a blank line and the words.
        assert_eq!(preview.line_count(), 4);
        assert_eq!(flip_hint(&preview), Some(("ctrl+r", "source")));
        preview.flip();
        assert_eq!(preview.line_count(), 3, "the source's three lines");
        assert_eq!(flip_hint(&preview), Some(("ctrl+r", "page")));
        // The next markdown file shows its source too.
        preview.show("other.md");
        preview.read_done(Path::new("/code/app"), "other.md", Ok(page("# Other")));
        assert!(preview.shows_source());
    }

    #[test]
    fn a_page_is_laid_out_again_for_a_new_width() {
        let words = "word ".repeat(20);
        let mut preview = showing("notes.md", page(&words));
        let at_40 = preview.line_count();
        preview.set_size((11, 20));
        assert!(preview.line_count() > at_40);
        assert_eq!(preview.page.as_ref().unwrap().0, 18);
    }

    #[test]
    fn flipping_does_nothing_to_a_file_that_isnt_markdown() {
        let mut preview = showing("a.rs", lines(3));
        preview.flip();
        assert!(!preview.shows_source());
        assert_eq!(flip_hint(&preview), None);
    }

    #[test]
    fn a_directory_shows_its_names() {
        let mut preview = Preview::new(PathBuf::from("/code/app"));
        preview.show_listing("src", vec!["tui/".into(), "main.rs".into()]);
        assert_eq!(preview.path(), Some("src"));
        assert_eq!(preview.line_count(), 2);
    }

    #[test]
    fn reading_a_file_highlights_it_and_says_why_one_cant_be_shown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {\n\tlet x = 1;\n}\n").unwrap();
        std::fs::write(dir.path().join("logo.png"), b"\x89PNG\0\0").unwrap();
        std::fs::write(dir.path().join("empty.txt"), "\n\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Hi\n").unwrap();
        let read = read(dir.path(), "main.rs").unwrap();
        assert_eq!(read.lines.len(), 3);
        assert_eq!(read.lines[0][0], (TokenKind::Keyword, "fn".to_string()));
        assert_eq!(read.lines[1][0], (TokenKind::Text, "    ".to_string()));
        assert!(read.numbered && !read.markdown && !read.cut_short);
        assert!(super::read(dir.path(), "README.md").unwrap().markdown);
        assert_eq!(
            super::read(dir.path(), "logo.png").unwrap_err(),
            "a binary file"
        );
        assert_eq!(
            super::read(dir.path(), "empty.txt").unwrap_err(),
            "an empty file"
        );
        assert!(super::read(dir.path(), "gone.rs").is_err());
    }

    #[test]
    fn only_the_start_of_a_long_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let long = "x\n".repeat(MOST_LINES + 5);
        std::fs::write(dir.path().join("long.txt"), long).unwrap();
        let read = read(dir.path(), "long.txt").unwrap();
        assert_eq!(read.lines.len(), MOST_LINES);
        assert!(read.cut_short);
    }
}
