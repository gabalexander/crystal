//! The view `crystal open` brings up: files shown to the user, most often by
//! an agent they asked to see one, or the page it wrote them, like an
//! explanation with its diagrams. They're listed on the left, each by its
//! path from the worktree it was opened in, and the one the bar is on is
//! read on the right, a markdown file as its page with its mermaid diagrams
//! drawn; Enter opens it in the user's `$EDITOR`, as the file finder does.
//! Adapted from docket's file tabs.
//!
//! The state is plain data, kept apart from I/O: the event loop reads each
//! file for the preview.

use super::app::{Action, Hit, Outcome};
use super::preview::{self, Content, Preview};
use super::ui::{self, Look, ViewAreas};
use crate::shell;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::{Path, PathBuf};

/// One file the view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The worktree it was opened in, where an editor opening it starts.
    pub dir: PathBuf,
    /// Its path from there, or for a file outside it, its absolute path.
    pub path: String,
}

impl File {
    /// The file at `path`, absolute, opened in `dir`.
    pub fn at(dir: &Path, path: &Path) -> File {
        let path = match path.strip_prefix(dir) {
            Ok(inside) if !inside.as_os_str().is_empty() => inside,
            _ => path,
        };
        File {
            dir: dir.to_path_buf(),
            path: path.to_string_lossy().into_owned(),
        }
    }

    /// What the list calls it: its path from the worktree, or from home.
    fn name(&self) -> String {
        let path = Path::new(&self.path);
        if path.is_absolute() {
            shell::home_relative(path)
        } else {
            self.path.clone()
        }
    }
}

pub struct OpenedView {
    /// The session that opened them, by name, when it's one of the TUI's.
    pub by: Option<String>,
    /// Where they were opened, for the header: the session's project and
    /// branch, or the directory.
    pub place: String,
    pub files: Vec<File>,
    /// The file the bar is on, by its place in `files`.
    pub selected: usize,
    pub preview: Preview,
}

impl OpenedView {
    /// A view of `paths`, absolute, opened in `dir`, the bar on the first;
    /// and what reading it for the preview takes.
    pub fn new(
        by: Option<String>,
        place: String,
        dir: &Path,
        paths: &[PathBuf],
    ) -> (OpenedView, Option<Action>) {
        let mut view = OpenedView {
            by,
            place,
            files: paths.iter().map(|path| File::at(dir, path)).collect(),
            selected: 0,
            preview: Preview::new(dir.to_path_buf()),
        };
        let read = view.read_selected();
        (view, read)
    }

    /// Takes a file that's been read for the preview, if it's still the
    /// one shown.
    pub fn preview_read(&mut self, dir: &Path, path: &str, read: Result<Content, String>) {
        self.preview.read_done(dir, path, read);
    }

    /// The sizes of the preview's area, as `(rows, columns)`.
    pub fn set_size(&mut self, preview: (u16, u16)) {
        self.preview.set_size(preview);
    }

    /// The file the bar is on.
    pub fn selected(&self) -> Option<&File> {
        self.files.get(self.selected)
    }

    /// ↑ and ↓ choose a file, and Tab and Shift+Tab too, going round; the
    /// preview's keys scroll it, Ctrl+R flips a markdown file to its
    /// source, Enter edits the file, and Esc closes.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.preview.scroll_key(&key) {
            return Outcome::Stay;
        }
        match key.code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Enter => match self.selected() {
                Some(file) => Outcome::Edit {
                    line: self.preview.top_line(&file.path),
                    path: file.path.clone(),
                },
                None => Outcome::Stay,
            },
            KeyCode::Up => self.select_by(-1),
            KeyCode::Down => self.select_by(1),
            KeyCode::BackTab => self.go_round(-1),
            KeyCode::Tab => self.go_round(1),
            KeyCode::Char('r') if ctrl => {
                self.preview.flip();
                Outcome::Stay
            }
            _ => Outcome::Stay,
        }
    }

    /// A click on a file selects it, and the wheel moves the selection, or
    /// scrolls the preview.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(index)) => {
                self.selected = index.min(self.files.len().saturating_sub(1));
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

    fn select_by(&mut self, by: isize) -> Outcome {
        let last = self.files.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
        self.outcome_for_selection()
    }

    /// The file `by` from the one the bar is on, past the last back to the
    /// first and the other way.
    fn go_round(&mut self, by: isize) -> Outcome {
        let count = self.files.len().max(1) as isize;
        self.selected = (self.selected as isize + by).rem_euclid(count) as usize;
        self.outcome_for_selection()
    }

    fn outcome_for_selection(&mut self) -> Outcome {
        match self.read_selected() {
            Some(action) => Outcome::Do(action),
            None => Outcome::Stay,
        }
    }

    /// What reading the selected file's preview takes, if it isn't the one
    /// shown already.
    fn read_selected(&mut self) -> Option<Action> {
        match self.selected().cloned() {
            Some(file) => self.preview.show_in(&file.dir, &file.path),
            None => {
                self.preview.clear();
                None
            }
        }
    }
}

/// How wide the list of files is, for a view `width` columns wide: as wide
/// as the paths need, up to the file finder's, so a page has the room.
pub fn list_width(view: &OpenedView, width: u16) -> u16 {
    let longest = view.files.iter().map(|file| file.name().chars().count());
    let wanted = longest.max().unwrap_or(0) as u16 + 2;
    wanted.clamp(20, 64).min(width * 2 / 5).min(width / 2)
}

/// The keys the footer shows while the view is open.
pub fn hints(view: &OpenedView) -> Vec<(&'static str, &'static str)> {
    let mut hints = Vec::new();
    if view.files.len() > 1 {
        hints.push(("↑/↓", "select"));
    }
    hints.extend([("pgup/pgdn", "scroll"), ("enter", "edit")]);
    hints.extend(preview::flip_hint(&view.preview));
    hints.push(("esc", "close"));
    hints
}

/// Which file's row is on screen `row`, in a list drawn in `area`.
pub fn list_hit(view: &OpenedView, area: Rect, row: u16) -> Hit {
    let index = usize::from(row - area.y);
    if index < view.files.len() {
        Hit::ViewList(index)
    } else {
        Hit::Elsewhere
    }
}

pub fn draw(frame: &mut Frame, view: &OpenedView, look: &Look, areas: &ViewAreas) {
    let theme = look.theme;
    frame.render_widget(header(view, look, areas.header.width), areas.header);
    ui::draw_rule(frame, look, areas.rule);
    let list = areas.list;
    for (index, file) in view.files.iter().enumerate().take(usize::from(list.height)) {
        let row = Rect::new(list.x, list.y + index as u16, list.width, 1);
        if index == view.selected {
            frame.buffer_mut().set_style(row, theme.selection);
        }
        let name = fit_tail(&file.name(), usize::from(row.width).saturating_sub(2));
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                name,
                Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(line, row);
    }
    preview::draw(frame, &view.preview, look, areas.content);
}

/// `path` cut down to `width` characters from the front, starting with `…`
/// when it's cut: the end of a path is what tells `a/README.md` from
/// `b/README.md`.
fn fit_tail(path: &str, width: usize) -> String {
    let count = path.chars().count();
    if count <= width {
        return path.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let kept: String = path.chars().skip(count - (width - 1)).collect();
    format!("…{kept}")
}

/// "▤ opened · explainer · 2 files", and where on the right.
fn header<'a>(view: &OpenedView, look: &Look, width: u16) -> Line<'a> {
    let mut notes: Vec<String> = view.by.iter().cloned().collect();
    if view.files.len() > 1 {
        notes.push(format!("{} files", view.files.len()));
    }
    ui::view_header("▤", "opened", &notes, &view.place, look, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn view(paths: &[&str]) -> (OpenedView, Option<Action>) {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        OpenedView::new(
            Some("explainer".into()),
            "app ⎇ main".into(),
            Path::new("/code/app"),
            &paths,
        )
    }

    fn read(dir: &str, path: &str) -> Action {
        Action::ReadPreview {
            dir: PathBuf::from(dir),
            path: path.into(),
        }
    }

    #[test]
    fn files_go_by_their_path_from_the_worktree_or_else_their_whole_path() {
        let dir = Path::new("/code/app");
        let inside = File::at(dir, Path::new("/code/app/docs/explain/hooks.md"));
        assert_eq!(inside.path, "docs/explain/hooks.md");
        assert_eq!(inside.dir, dir);
        let outside = File::at(dir, Path::new("/tmp/notes.md"));
        assert_eq!(outside.path, "/tmp/notes.md");
        assert_eq!(outside.name(), "/tmp/notes.md");
        // An absolute path joined to the worktree is itself, for the
        // preview to read and the editor to open.
        assert_eq!(dir.join(&outside.path), Path::new("/tmp/notes.md"));
        if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
            let in_home = File::at(dir, &PathBuf::from(home).join("notes/plan.md"));
            assert_eq!(in_home.name(), "~/notes/plan.md");
        }
    }

    #[test]
    fn a_long_path_keeps_its_end() {
        assert_eq!(fit_tail("docs/plan.md", 20), "docs/plan.md");
        assert_eq!(fit_tail("crates/a/README.md", 12), "…a/README.md");
        assert_eq!(fit_tail("README.md", 0), "");
    }

    #[test]
    fn it_opens_on_the_first_file_and_the_bar_goes_round_with_tab() {
        let (mut view, first) = view(&["/code/app/README.md", "/code/app/src/main.rs"]);
        assert_eq!(first, Some(read("/code/app", "README.md")));
        assert_eq!(
            view.on_key(key(KeyCode::Tab)),
            Outcome::Do(read("/code/app", "src/main.rs"))
        );
        assert_eq!(
            view.on_key(key(KeyCode::Tab)),
            Outcome::Do(read("/code/app", "README.md")),
            "past the last, the first"
        );
        assert_eq!(
            view.on_key(key(KeyCode::BackTab)),
            Outcome::Do(read("/code/app", "src/main.rs"))
        );
        assert_eq!(
            view.on_key(key(KeyCode::Down)),
            Outcome::Stay,
            "↓ stops at the last"
        );
        assert_eq!(view.selected, 1);
        assert_eq!(
            view.on_key(key(KeyCode::Up)),
            Outcome::Do(read("/code/app", "README.md"))
        );
    }

    #[test]
    fn enter_edits_the_file_the_bar_is_on_and_esc_closes() {
        let (mut view, _) = view(&["/code/app/README.md", "/tmp/notes.md"]);
        view.on_key(key(KeyCode::Down));
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            Outcome::Edit {
                path: "/tmp/notes.md".into(),
                line: None,
            }
        );
        assert_eq!(view.selected().unwrap().dir, Path::new("/code/app"));
        assert_eq!(view.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn a_click_selects_a_file_and_the_wheel_scrolls_the_preview() {
        let (mut view, _) = view(&["/code/app/a.md", "/code/app/b.md"]);
        let click = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(
            view.on_mouse(click, Hit::ViewList(1)),
            Outcome::Do(read("/code/app", "b.md"))
        );
        assert_eq!(view.on_mouse(click, Hit::ViewList(9)), Outcome::Stay);
        assert_eq!(view.selected, 1, "past the last row, the last");
        assert_eq!(
            view.on_mouse(MouseEventKind::ScrollUp, Hit::ViewList(0)),
            Outcome::Do(read("/code/app", "a.md"))
        );
        assert_eq!(
            view.on_mouse(MouseEventKind::ScrollDown, Hit::ViewContent),
            Outcome::Stay
        );
    }

    #[test]
    fn a_markdown_file_is_read_as_its_page_and_flips_to_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let page = "# How a hook lands\n\n```mermaid\nsequenceDiagram\n  CLI->>daemon: hook\n```\n";
        std::fs::write(dir.path().join("explain.md"), page).unwrap();
        let (mut view, read) = OpenedView::new(
            None,
            "~/app".into(),
            dir.path(),
            &[dir.path().join("explain.md")],
        );
        let Some(Action::ReadPreview { dir: read_in, path }) = read else {
            panic!("the file is read");
        };
        assert_eq!(path, "explain.md");
        let content = preview::read(&read_in, &path);
        view.preview_read(&read_in, &path, content);
        assert!(view.preview.is_markdown());
        assert_eq!(
            hints(&view),
            [
                ("pgup/pgdn", "scroll"),
                ("enter", "edit"),
                ("ctrl+r", "source"),
                ("esc", "close"),
            ]
        );
        view.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(view.preview.shows_source());
    }

    #[test]
    fn the_list_is_as_wide_as_its_paths_within_bounds() {
        let (short, _) = view(&["/code/app/a.md"]);
        assert_eq!(list_width(&short, 200), 20);
        let long = format!("/code/app/{}.md", "a".repeat(100));
        let (long, _) = view(&[long.as_str()]);
        assert_eq!(list_width(&long, 200), 64);
        assert_eq!(list_width(&long, 100), 40);
    }
}
