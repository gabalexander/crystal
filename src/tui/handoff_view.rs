//! The handoff view, `M` in the sidebar: what a session leaves for the
//! sessions after it, read in place. The notes its worktree keeps for
//! whoever works there next, `.crystal/handoff.md`, as they are now, and
//! the files its task kept as it closed (`crystal done --artifact`), the
//! notes as they were then among them. They're listed on the left, and the
//! one the bar is on is read on the right, a markdown file as its page;
//! Enter opens it in the user's `$EDITOR`, as the file finder does.
//!
//! The state is plain data, kept apart from I/O: the event loop looks for
//! the notes and reads the task's kept files from the database, handing
//! them in through [`HandoffView::found`], and reads each file for the
//! preview.

use super::app::{Action, Hit, Loading, Outcome};
use super::preview::{self, Content, Preview};
use super::sidebar::fit;
use super::ui::{self, Look, ViewAreas};
use crate::artifacts;
use crate::handoff;
use crate::protocol::{Artifact, ArtifactKind, task_label};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::{Path, PathBuf};

/// One file the view lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The directory it's in, where an editor opening it starts.
    pub dir: PathBuf,
    /// Its path from there, which the preview's title shows.
    pub path: String,
    /// What it is, beside its name: `notes now`, or a kept file's size.
    pub what: String,
}

impl File {
    /// The handoff file of the worktree at `worktree`, as it is now.
    pub fn notes(worktree: &Path) -> File {
        File {
            dir: worktree.to_path_buf(),
            path: format!("{}/{}", handoff::DIR, handoff::FILE),
            what: "notes now".to_string(),
        }
    }

    /// A file kept with task `task` as it closed.
    pub fn kept(artifact: &Artifact, task: u64) -> File {
        let dir = artifact.path.parent().unwrap_or(Path::new("/"));
        let size = artifacts::size(artifact.bytes);
        let what = match artifact.kind {
            ArtifactKind::Handoff => format!("notes as {} closed · {size}", task_label(Some(task))),
            ArtifactKind::File => format!("kept · {size}"),
        };
        File {
            dir: dir.to_path_buf(),
            path: artifact.name.clone(),
            what,
        }
    }
}

/// What the event loop found for the view: whether the worktree has notes,
/// and the files the task kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The worktree, when it has a handoff file with something in it.
    pub notes: Option<PathBuf>,
    /// The task's kept files, none for a session without a task.
    pub kept: Result<Vec<Artifact>, String>,
}

pub struct HandoffView {
    /// The session it's about, by name.
    pub session: String,
    /// Its project and branch, for the header.
    pub place: String,
    /// The worktree whose notes it reads, while the handoff file is on.
    pub worktree: Option<PathBuf>,
    /// The task whose kept files it lists, by number, while tasks are on.
    pub task: Option<u64>,
    /// The files: the worktree's notes first, then what the task kept.
    pub files: Loading<Vec<File>>,
    /// The file the bar is on, by its place in `files`.
    pub selected: usize,
    pub preview: Preview,
    /// Why the task's kept files couldn't be read, beside the notes that
    /// could.
    pub trouble: Option<String>,
}

impl HandoffView {
    pub fn new(
        session: String,
        place: String,
        worktree: Option<PathBuf>,
        task: Option<u64>,
    ) -> HandoffView {
        let dir = worktree.clone().unwrap_or_default();
        HandoffView {
            session,
            place,
            worktree,
            task,
            files: Loading::Reading,
            selected: 0,
            preview: Preview::new(dir),
            trouble: None,
        }
    }

    /// What finding its files takes, for the event loop to do.
    pub fn read(&self) -> Action {
        Action::ReadHandoff {
            session: self.session.clone(),
            worktree: self.worktree.clone(),
            task: self.task,
        }
    }

    /// Takes what was found for `session`, if it's this view's, and asks for
    /// the first file's preview.
    pub fn found(&mut self, session: &str, found: Found) -> Option<Action> {
        if session != self.session {
            return None;
        }
        let mut files: Vec<File> = found.notes.iter().map(|dir| File::notes(dir)).collect();
        match (found.kept, self.task) {
            (Ok(kept), Some(task)) => files.extend(kept.iter().map(|kept| File::kept(kept, task))),
            (Ok(_), None) => {}
            (Err(why), _) if files.is_empty() => {
                self.files = Loading::Failed(why);
                return None;
            }
            (Err(why), _) => self.trouble = Some(why),
        }
        self.files = Loading::Read(files);
        self.selected = 0;
        self.read_selected()
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
        match &self.files {
            Loading::Read(files) => files.get(self.selected),
            _ => None,
        }
    }

    /// ↑ and ↓ (or `k` and `j`, Ctrl+P and Ctrl+N) choose a file, the
    /// preview's keys scroll it, Ctrl+R flips a markdown file to its
    /// source, Enter edits the file, and Esc or `q` closes.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.preview.scroll_key(&key) {
            return Outcome::Stay;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Close,
            KeyCode::Enter => match self.selected() {
                Some(file) => Outcome::Edit {
                    line: self.preview.top_line(&file.path),
                    path: file.path.clone(),
                },
                None => Outcome::Stay,
            },
            KeyCode::Up | KeyCode::Char('k') => self.select_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select_by(1),
            KeyCode::Char('p') if ctrl => self.select_by(-1),
            KeyCode::Char('n') if ctrl => self.select_by(1),
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
                self.selected = index.min(self.count().saturating_sub(1));
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

    fn count(&self) -> usize {
        match &self.files {
            Loading::Read(files) => files.len(),
            _ => 0,
        }
    }

    fn select_by(&mut self, by: isize) -> Outcome {
        let last = self.count().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
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
/// as the file finder's.
pub fn list_width(width: u16) -> u16 {
    (width * 2 / 5).clamp(30, 64).min(width / 2)
}

/// The keys the footer shows while the view is open.
pub fn hints(view: &HandoffView) -> Vec<(&'static str, &'static str)> {
    let mut hints = vec![
        ("↑/↓", "select"),
        ("pgup/pgdn", "scroll"),
        ("enter", "edit"),
    ];
    hints.extend(preview::flip_hint(&view.preview));
    hints.push(("esc", "close"));
    hints
}

/// Which file's row is on screen `row`, in a list drawn in `area`.
pub fn list_hit(view: &HandoffView, area: Rect, row: u16) -> Hit {
    let index = usize::from(row - area.y);
    if index < view.count() {
        Hit::ViewList(index)
    } else {
        Hit::Elsewhere
    }
}

pub fn draw(frame: &mut Frame, view: &HandoffView, look: &Look, areas: &ViewAreas) {
    let theme = look.theme;
    frame.render_widget(header(view, look, areas.header.width), areas.header);
    ui::draw_rule(frame, look, areas.rule);
    let list = areas.list;
    match &view.files {
        Loading::Reading => ui::draw_message(frame, look, "looking…", list),
        Loading::Failed(why) => {
            let line = Line::styled(format!(" {why}"), Style::new().fg(theme.failed));
            frame.render_widget(line, Rect::new(list.x, list.y, list.width, 1));
        }
        Loading::Read(files) if files.is_empty() => {
            let message = match (&view.worktree, view.task) {
                (Some(_), Some(_)) => "no notes, and its task kept nothing",
                (Some(_), None) => "no notes in its worktree yet",
                (None, _) => "its task kept nothing",
            };
            ui::draw_message(frame, look, message, list);
        }
        Loading::Read(files) => {
            for (index, file) in files.iter().enumerate().take(usize::from(list.height)) {
                let row = Rect::new(list.x, list.y + index as u16, list.width, 1);
                if index == view.selected {
                    frame.buffer_mut().set_style(row, theme.selection);
                }
                frame.render_widget(file_line(file, look, row.width), row);
            }
            if let Some(trouble) = &view.trouble {
                let below = list.y + files.len() as u16 + 1;
                if below < list.bottom() {
                    let line = Line::styled(
                        format!(" {}", fit(trouble, usize::from(list.width) - 1)),
                        Style::new().fg(theme.failed),
                    );
                    frame.render_widget(line, Rect::new(list.x, below, list.width, 1));
                }
            }
        }
    }
    preview::draw(frame, &view.preview, look, areas.content);
}

/// "✎ handoff · fixer · t12", and where on the right.
fn header<'a>(view: &HandoffView, look: &Look, width: u16) -> Line<'a> {
    let mut notes = vec![view.session.clone()];
    if view.task.is_some() {
        notes.push(task_label(view.task));
    }
    ui::view_header("✎", "handoff", &notes, &view.place, look, width)
}

/// A file's name, and what it is muted on the right, in what room the
/// name leaves it.
fn file_line<'a>(file: &File, look: &Look, width: u16) -> Line<'a> {
    // What's kept for what it is, however long the name.
    const WHAT: usize = 9;
    let theme = look.theme;
    let width = usize::from(width);
    let name = fit(&file.path, width.saturating_sub(3 + WHAT));
    let what = fit(&file.what, width.saturating_sub(name.chars().count() + 3));
    let gap = width.saturating_sub(name.chars().count() + what.chars().count() + 2);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            name,
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
        Span::styled(what, Style::new().fg(theme.muted)),
        Span::raw(" "),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn kept(kind: ArtifactKind, name: &str, bytes: u64) -> Artifact {
        Artifact {
            kind,
            name: name.into(),
            path: PathBuf::from("/state/tasks/t12").join(name),
            bytes,
        }
    }

    fn view() -> HandoffView {
        HandoffView::new(
            "fixer".into(),
            "app ⎇ fix-login".into(),
            Some(PathBuf::from("/code/app-fix")),
            Some(12),
        )
    }

    fn found(view: &mut HandoffView) -> Option<Action> {
        let found = Found {
            notes: Some(PathBuf::from("/code/app-fix")),
            kept: Ok(vec![
                kept(ArtifactKind::File, "plan.md", 2048),
                kept(ArtifactKind::Handoff, "handoff.md", 512),
            ]),
        };
        view.found("fixer", found)
    }

    #[test]
    fn the_worktrees_notes_come_first_then_what_the_task_kept() {
        let mut view = view();
        assert_eq!(
            view.read(),
            Action::ReadHandoff {
                session: "fixer".into(),
                worktree: Some(PathBuf::from("/code/app-fix")),
                task: Some(12),
            }
        );
        assert_eq!(
            found(&mut view),
            Some(Action::ReadPreview {
                dir: PathBuf::from("/code/app-fix"),
                path: ".crystal/handoff.md".into(),
            })
        );
        let Loading::Read(files) = &view.files else {
            panic!("the files are found");
        };
        let listed: Vec<(&str, &str)> = files
            .iter()
            .map(|file| (file.path.as_str(), file.what.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                (".crystal/handoff.md", "notes now"),
                ("plan.md", "kept · 2 KiB"),
                ("handoff.md", "notes as t12 closed · 512 bytes"),
            ]
        );
        // Another session's answer, for a view since closed, is no answer.
        let other = Found {
            notes: None,
            kept: Ok(Vec::new()),
        };
        assert_eq!(view.found("other", other), None);
        assert_eq!(view.count(), 3);
    }

    #[test]
    fn the_preview_follows_the_bar_into_the_state_directory_and_enter_edits_it() {
        let mut view = view();
        found(&mut view);
        let read = view.on_key(key(KeyCode::Down));
        assert_eq!(
            read,
            Outcome::Do(Action::ReadPreview {
                dir: PathBuf::from("/state/tasks/t12"),
                path: "plan.md".into(),
            })
        );
        assert_eq!(view.preview.path(), Some("plan.md"));
        assert_eq!(
            view.on_key(key(KeyCode::Enter)),
            Outcome::Edit {
                path: "plan.md".into(),
                line: None,
            }
        );
        assert_eq!(view.selected().unwrap().dir, Path::new("/state/tasks/t12"));
        view.on_key(key(KeyCode::Down));
        view.on_key(key(KeyCode::Down));
        assert_eq!(view.selected().unwrap().path, "handoff.md");
        view.on_key(key(KeyCode::Up));
        view.on_key(key(KeyCode::Up));
        assert_eq!(
            view.on_key(key(KeyCode::Up)),
            Outcome::Stay,
            "the notes are shown already"
        );
        assert_eq!(view.preview.path(), Some(".crystal/handoff.md"));
        assert_eq!(view.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn kept_files_that_cant_be_read_leave_the_notes() {
        let mut view = view();
        let found = Found {
            notes: Some(PathBuf::from("/code/app-fix")),
            kept: Err("no database".into()),
        };
        view.found("fixer", found);
        assert_eq!(view.count(), 1);
        assert_eq!(view.trouble.as_deref(), Some("no database"));

        // With nothing else, it's what the list says.
        let mut alone = view_without_notes();
        let found = Found {
            notes: None,
            kept: Err("no database".into()),
        };
        assert_eq!(alone.found("fixer", found), None);
        assert_eq!(alone.files, Loading::Failed("no database".into()));
    }

    fn view_without_notes() -> HandoffView {
        HandoffView::new("fixer".into(), "app".into(), None, Some(12))
    }

    #[test]
    fn the_preview_reads_what_the_files_hold_and_flips_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join(".crystal");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("handoff.md"), "## 14:03 · fixer\nmake db\n").unwrap();
        let mut view = HandoffView::new(
            "fixer".into(),
            "app".into(),
            Some(dir.path().to_path_buf()),
            None,
        );
        let found = Found {
            notes: Some(dir.path().to_path_buf()),
            kept: Ok(Vec::new()),
        };
        let Some(Action::ReadPreview { dir: read_in, path }) = view.found("fixer", found) else {
            panic!("the notes are read");
        };
        let read = preview::read(&read_in, &path);
        view.preview_read(&read_in, &path, read);
        assert!(view.preview.is_markdown());
        assert!(hints(&view).contains(&("ctrl+r", "source")));
        view.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(view.preview.shows_source());
    }
}
