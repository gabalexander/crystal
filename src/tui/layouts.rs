//! Layouts, `S` in the sidebar: the tabs saved under a name, to put back
//! later. A layout is the tabs as they were: their names, which sessions
//! each holds, its panes with how they're split and how big each is, its
//! float, whether it's zoomed, and which tab was in front; and what starts
//! each of its terminals' programs again, its command and directory.
//! Restoring one starts again those it names that have gone since, then
//! arranges the sessions that way; those it can't start are left out, and
//! those it doesn't name join the tab in front. The tabs a restore replaces
//! are kept, to go back to.
//!
//! The view's keys: Enter restores the layout the bar is on, `s` saves the
//! tabs as one under a name typed on the footer, and `x` removes one once
//! `y` says so. The layouts are kept in a file beside the tabs; reading and
//! writing it is the event loop's, and nothing else here does any I/O.

use super::sidebar::{ago, fit};
use super::tabs::Tabs;
use super::text_input::TextInput;
use super::theme::Theme;
use crate::catalog;
use crate::protocol::{Front, SessionInfo};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Which shape of file [`save`] writes. A file of another shape is shown
/// as one that can't be read, and isn't written over.
const VERSION: u32 = 1;

/// The tabs as they were when they were saved. Saved by the crystal before
/// this one, a tab's panes were a list: they're read as a tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub name: String,
    /// When it was saved, in seconds since the Unix epoch.
    pub saved: u64,
    pub tabs: Tabs,
    /// What starts the sessions it names again, by name, when they've gone.
    /// None for a layout saved before layouts started them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub programs: Programs,
}

/// What starts sessions again, by their names.
pub type Programs = BTreeMap<String, Program>;

/// What starts a session's program again: its command, as it was started
/// but for an agent's first prompt, and its directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    pub command: Vec<String>,
    pub cwd: PathBuf,
}

impl Program {
    /// What starts `session` again, for one running in a terminal: a
    /// background task has no program of its own. An agent given a first
    /// prompt starts without it, as it would be asked that again.
    pub fn of(session: &SessionInfo) -> Option<Program> {
        let background = session.task.as_ref().is_some_and(|task| task.background);
        if background || session.front == Some(Front::Task) || session.command.is_empty() {
            return None;
        }
        Some(Program {
            command: catalog::without_first_prompt(&session.command),
            cwd: session.cwd.clone(),
        })
    }
}

/// A layout put back: its tabs, and what starts the sessions they name
/// again.
#[derive(Debug, Clone, PartialEq)]
pub struct Restored {
    pub tabs: Tabs,
    pub programs: Programs,
}

impl Layout {
    /// How many tabs it has, and sessions in them.
    fn counts(&self) -> (usize, usize) {
        (self.tabs.all().len(), self.tabs.sessions().count())
    }
}

/// Every layout saved, and the tabs a restore last replaced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layouts {
    /// [`VERSION`] when written by [`save`]; 0 for a file without one.
    version: u32,
    /// In the order they were first saved.
    pub saved: Vec<Layout>,
    /// The tabs as they were just before the last restore, under the name
    /// of the layout that replaced them, to go back to once.
    pub before: Option<Layout>,
}

impl Default for Layouts {
    fn default() -> Layouts {
        Layouts {
            version: VERSION,
            saved: Vec::new(),
            before: None,
        }
    }
}

/// One of the layouts: one saved under its name, or the tabs before the
/// last restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Which {
    Saved(String),
    Before,
}

impl Layouts {
    pub fn get(&self, which: &Which) -> Option<&Layout> {
        match which {
            Which::Saved(name) => self.saved.iter().find(|layout| layout.name == *name),
            Which::Before => self.before.as_ref(),
        }
    }

    /// Saves `tabs` as the layout called `name`, with `programs` to start
    /// their sessions again, in place of the one called that, if there is
    /// one. Says whether there was.
    pub fn save(&mut self, name: &str, tabs: Tabs, programs: Programs, now: u64) -> bool {
        let layout = Layout {
            name: name.to_string(),
            saved: now,
            tabs,
            programs,
        };
        match self.saved.iter_mut().find(|saved| saved.name == name) {
            Some(saved) => {
                *saved = layout;
                true
            }
            None => {
                self.saved.push(layout);
                false
            }
        }
    }

    pub fn remove(&mut self, which: &Which) {
        match which {
            Which::Saved(name) => self.saved.retain(|layout| layout.name != *name),
            Which::Before => self.before = None,
        }
    }

    /// Restores `which` in place of the tabs there are now, `current`, with
    /// `programs` to start their sessions again: gives back the tabs it
    /// saved, put right, and what starts theirs, and keeps `current` to go
    /// back to. Going back is once only: the tabs it replaces aren't kept.
    /// `None`, and nothing changes, when there's no such layout, or it was
    /// saved by a crystal that kept tabs another way.
    pub fn restore(
        &mut self,
        which: &Which,
        current: Tabs,
        programs: Programs,
        now: u64,
    ) -> Option<Restored> {
        let layout = self.get(which)?.clone();
        let tabs = layout.tabs.kept()?;
        self.before = match which {
            Which::Saved(name) => Some(Layout {
                name: name.clone(),
                saved: now,
                tabs: current,
                programs,
            }),
            Which::Before => None,
        };
        Some(Restored {
            tabs,
            programs: layout.programs,
        })
    }
}

/// The layouts kept as `json`: none when nothing has been kept yet, or why
/// what was kept couldn't be read.
pub fn read(json: Option<&str>) -> Result<Layouts, String> {
    let Some(json) = json else {
        return Ok(Layouts::default());
    };
    let layouts: Layouts = serde_json::from_str(json)
        .map_err(|err| format!("couldn't read the saved layouts: {err}"))?;
    if layouts.version != VERSION {
        return Err("the saved layouts were written by another crystal".to_string());
    }
    Ok(layouts)
}

/// What a key in the view asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    /// Save the tabs as the layout with this name.
    Save(String),
    Restore(Which),
    Remove(Which),
}

pub struct LayoutsView {
    /// What the file held when it was last read, or why it couldn't be.
    layouts: Result<Layouts, String>,
    /// The sessions there are now, by name, to say how many of a layout's
    /// have gone.
    running: Vec<String>,
    /// The row the bar is on: see [`LayoutsView::rows`].
    highlighted: usize,
    /// While `s` is saving the tabs: the name typed so far.
    pub naming: Option<TextInput>,
    /// The layout `x` asks about removing, until the next key answers.
    removing: Option<Which>,
}

impl LayoutsView {
    pub fn new(layouts: Result<Layouts, String>, running: Vec<String>) -> LayoutsView {
        LayoutsView {
            layouts,
            running,
            highlighted: 0,
            naming: None,
            removing: None,
        }
    }

    /// Takes the layouts as the file holds them now, with the bar where it
    /// was, or on the layout called `on`.
    pub fn set_layouts(&mut self, layouts: Result<Layouts, String>, on: Option<&Which>) {
        let was_on = on.cloned().or_else(|| self.highlighted_row());
        self.layouts = layouts;
        let rows = self.rows();
        let found = was_on.and_then(|which| rows.iter().position(|row| *row == which));
        self.highlighted = found.unwrap_or(0).min(rows.len().saturating_sub(1));
    }

    /// The rows, in order: the tabs before the last restore, if they were
    /// kept, then every layout saved, the one saved last first.
    pub fn rows(&self) -> Vec<Which> {
        let Ok(layouts) = &self.layouts else {
            return Vec::new();
        };
        // Saved in the same second, the one saved later is still first.
        let mut saved: Vec<&Layout> = layouts.saved.iter().rev().collect();
        saved.sort_by_key(|layout| std::cmp::Reverse(layout.saved));
        let before = layouts.before.as_ref().map(|_| Which::Before);
        let saved = saved
            .into_iter()
            .map(|layout| Which::Saved(layout.name.clone()));
        before.into_iter().chain(saved).collect()
    }

    fn highlighted_row(&self) -> Option<Which> {
        self.rows().get(self.highlighted).cloned()
    }

    /// The question `x` asks, while it waits for its answer.
    pub fn removing(&self) -> Option<String> {
        let name = match self.removing.as_ref()? {
            Which::Saved(name) => name.as_str(),
            Which::Before => "the tabs from before the restore",
        };
        Some(format!("remove {name}? y/n"))
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        if let Some(which) = self.removing.take() {
            return match key.code {
                KeyCode::Char('y') => Step::Remove(which),
                _ => Step::Stay,
            };
        }
        if self.naming.is_some() {
            return self.on_naming_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Step::Close,
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            // There's nothing to save into a file that can't be read.
            KeyCode::Char('s' | 'a') if self.layouts.is_ok() => {
                self.naming = Some(TextInput::default());
            }
            KeyCode::Char('x') => self.removing = self.highlighted_row(),
            KeyCode::Enter => {
                if let Some(which) = self.highlighted_row() {
                    return Step::Restore(which);
                }
            }
            _ => {}
        }
        Step::Stay
    }

    /// A paste goes into the name being typed, on one line.
    pub fn on_paste(&mut self, text: &str) {
        if let Some(naming) = &mut self.naming {
            naming.insert_str(&text.replace(['\r', '\n'], " "));
        }
    }

    /// Enter saves under what's typed; Esc gives up.
    fn on_naming_key(&mut self, key: &KeyEvent) -> Step {
        match key.code {
            KeyCode::Esc => self.naming = None,
            KeyCode::Enter => {
                let name = self
                    .naming
                    .take()
                    .map(|input| input.text().trim().to_string());
                if let Some(name) = name.filter(|name| !name.is_empty()) {
                    return Step::Save(name);
                }
            }
            _ => {
                if let Some(naming) = &mut self.naming {
                    naming.on_key(key);
                }
            }
        }
        Step::Stay
    }

    fn move_by(&mut self, by: isize) {
        let last = self.rows().len().saturating_sub(1);
        self.highlighted = self.highlighted.saturating_add_signed(by).min(last);
    }
}

/// Draws the view in `area`, over the sidebar and the panes: a heading,
/// then the layouts. `now` is in seconds since the Unix epoch, to say how
/// long ago each was saved.
pub fn draw(frame: &mut Frame, view: &LayoutsView, theme: &Theme, now: u64, area: Rect) {
    // A style alone would leave the characters drawn there before, the
    // sidebar and the panes, showing through.
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.base()), area);
    let [heading, _, list] = Split::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "layouts",
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " · your tabs, saved to put back",
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(title, heading);

    let layouts = match &view.layouts {
        Ok(layouts) => layouts,
        Err(reason) => return draw_note(frame, theme, reason, list),
    };
    let count = format!("{} saved ", layouts.saved.len());
    let count = Line::styled(count, Style::new().fg(theme.muted));
    frame.render_widget(count.right_aligned(), heading);
    let rows = view.rows();
    if rows.is_empty() {
        let note = "no layouts yet: s saves your tabs as one";
        return draw_note(frame, theme, note, list);
    }
    let height = usize::from(list.height.max(1));
    let first = (view.highlighted + 1).saturating_sub(height);
    for (row, which) in rows.iter().enumerate().skip(first).take(height) {
        let Some(layout) = layouts.get(which) else {
            continue;
        };
        let line_area = Rect::new(list.x, list.y + (row - first) as u16, list.width, 1);
        let highlighted = row == view.highlighted;
        if highlighted {
            frame.buffer_mut().set_style(line_area, theme.selection);
        }
        let line = layout_line(view, which, layout, theme, now, highlighted, list.width);
        frame.render_widget(line, line_area);
    }
}

/// A layout's row: its name, and on the right how many tabs and sessions
/// it has, how many of those have gone, and how long ago it was saved.
fn layout_line<'a>(
    view: &LayoutsView,
    which: &Which,
    layout: &Layout,
    theme: &Theme,
    now: u64,
    highlighted: bool,
    width: u16,
) -> Line<'a> {
    let (tabs, sessions) = layout.counts();
    let gone = (layout.tabs.sessions())
        .filter(|name| !view.running.iter().any(|running| running == name))
        .count();
    let plural = |count: usize, noun: &str| match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    };
    let mut right = format!("{} · {}", plural(tabs, "tab"), plural(sessions, "session"));
    if gone > 0 {
        right.push_str(&format!(" ({gone} gone)"));
    }
    match ago(layout.saved, now).as_str() {
        "now" => right.push_str(" · just now "),
        long => right.push_str(&format!(" · {long} ago ")),
    }
    let (mark, name) = match which {
        Which::Saved(name) => ("  ", name.clone()),
        Which::Before => ("↶ ", format!("before {}", layout.name)),
    };
    let right_width = right.chars().count();
    let room = usize::from(width).saturating_sub(1 + mark.chars().count() + right_width + 2);
    let name = fit(&name, room);
    let used = 1 + mark.chars().count() + name.chars().count();
    let gap = usize::from(width).saturating_sub(used + right_width);
    let mut name_style = Style::new().fg(theme.text);
    if highlighted {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    Line::from(vec![
        Span::raw(" "),
        Span::styled(mark, Style::new().fg(theme.accent)),
        Span::styled(name, name_style),
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
    ("enter", "restore"),
    ("s", "save the tabs as"),
    ("x", "remove"),
    ("esc", "close"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::split_tree::Pane;

    fn press(view: &mut LayoutsView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(view: &mut LayoutsView, text: &str) {
        for c in text.chars() {
            press(view, KeyCode::Char(c));
        }
    }

    /// What starts each session named, with its program, in /tmp.
    fn programs(named: &[(&str, &str)]) -> Programs {
        let program = |command: &str| Program {
            command: vec![command.to_string()],
            cwd: PathBuf::from("/tmp"),
        };
        (named.iter())
            .map(|(name, command)| (name.to_string(), program(command)))
            .collect()
    }

    #[test]
    fn a_terminal_s_program_starts_again_and_an_agent_without_its_prompt() {
        let session = |command: &[&str]| SessionInfo {
            name: "x".into(),
            id: "x".into(),
            command: command.iter().map(|word| word.to_string()).collect(),
            cwd: PathBuf::from("/work"),
            pid: Some(1),
            state: crate::protocol::State::Running,
            activity: None,
            worktree: None,
            changed: 0,
            front: None,
            task: None,
            asking: None,
            reporter: None,
        };
        let claude = Program::of(&session(&["claude", "--model", "opus", "--", "fix it"]));
        assert_eq!(claude.unwrap().command, ["claude", "--model", "opus"]);
        let shell = Program::of(&session(&["zsh"])).unwrap();
        assert_eq!(shell.cwd, PathBuf::from("/work"));
        let mut task = session(&["claude", "-p"]);
        task.front = Some(Front::Task);
        assert_eq!(Program::of(&task), None);
    }

    /// Tabs with one tab, named `name`, holding `sessions`.
    fn tabs_of(name: &str, sessions: &[&str]) -> Tabs {
        let mut tabs = Tabs::default();
        tabs.rename(name);
        for session in sessions {
            tabs.put(session, 0);
        }
        tabs
    }

    #[test]
    fn saving_under_a_name_there_is_already_replaces_it() {
        let mut layouts = Layouts::default();
        assert!(!layouts.save("work", tabs_of("one", &["a"]), Programs::new(), 10));
        assert!(!layouts.save("review", tabs_of("two", &["b"]), Programs::new(), 20));
        assert!(layouts.save("work", tabs_of("three", &["c"]), Programs::new(), 30));
        let names: Vec<&str> = layouts.saved.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["work", "review"]);
        let work = layouts.get(&Which::Saved("work".into())).unwrap();
        assert_eq!(work.tabs.current().name, "three");
        assert_eq!(work.saved, 30);
    }

    #[test]
    fn a_restore_keeps_the_tabs_it_replaces_to_go_back_to_once() {
        let mut layouts = Layouts::default();
        let saved = programs(&[("a", "vim")]);
        layouts.save("work", tabs_of("saved", &["a"]), saved.clone(), 10);
        let now = tabs_of("now", &["b"]);
        let running = programs(&[("b", "htop")]);
        let work = Which::Saved("work".into());
        let restored = layouts.restore(&work, now.clone(), running.clone(), 20);
        let restored = restored.unwrap();
        assert_eq!(restored.tabs.current().name, "saved");
        assert_eq!(restored.programs, saved);
        let before = layouts.before.clone().unwrap();
        assert_eq!((before.name.as_str(), before.saved), ("work", 20));
        assert_eq!(before.tabs, now);

        let back = layouts.restore(&Which::Before, restored.tabs, saved, 30);
        let back = back.unwrap();
        assert_eq!(back.tabs, now);
        assert_eq!(back.programs, running);
        assert_eq!(layouts.before, None);
        let again = layouts.restore(&Which::Before, back.tabs, running, 40);
        assert_eq!(again, None);
    }

    #[test]
    fn layouts_kept_come_back_as_they_were() {
        assert_eq!(read(None), Ok(Layouts::default()));
        let mut layouts = Layouts::default();
        layouts.save(
            "work",
            tabs_of("one", &["a", "b"]),
            programs(&[("a", "vim")]),
            10,
        );
        let json = serde_json::to_string(&layouts).unwrap();
        assert_eq!(read(Some(&json)), Ok(layouts));
    }

    #[test]
    fn a_layout_saved_when_panes_were_a_list_restores_them_as_a_tree() {
        let old = r#"{"version": 1, "saved": [{"name": "work", "saved": 10, "tabs": {
            "version": 2, "tabs": [{"sessions": ["a", "b"], "splits": ["a"],
            "selection_at": 1}], "current": 0}}]}"#;
        let mut layouts = read(Some(old)).unwrap();
        let work = Which::Saved("work".into());
        let restored = layouts.restore(&work, Tabs::default(), Programs::new(), 20);
        let tabs = restored.unwrap().tabs;
        assert_eq!(tabs.current().splits(), ["a"]);
        let panes = tabs.current().panes.panes();
        assert_eq!(panes.last(), Some(&&Pane::Selection));
    }

    #[test]
    fn layouts_that_cant_be_read_say_so_and_nothing_can_be_saved_over_them() {
        assert!(
            read(Some("not json"))
                .unwrap_err()
                .contains("couldn't read")
        );
        let other = r#"{"version": 9, "saved": []}"#;
        let reason = read(Some(other)).unwrap_err();
        assert!(reason.contains("another crystal"), "{reason}");

        let mut view = LayoutsView::new(read(Some(other)), Vec::new());
        type_text(&mut view, "s");
        assert!(view.naming.is_none());
    }

    fn view_with(names: &[(&str, u64)], before: bool) -> LayoutsView {
        let mut layouts = Layouts::default();
        for (name, saved) in names {
            layouts.save(name, tabs_of(name, &["a"]), Programs::new(), *saved);
        }
        if before {
            let first = Which::Saved(names[0].0.into());
            layouts.restore(&first, tabs_of("old", &[]), Programs::new(), 100);
        }
        LayoutsView::new(Ok(layouts), vec!["a".into()])
    }

    #[test]
    fn the_rows_are_the_tabs_before_a_restore_then_the_newest_first() {
        let view = view_with(&[("old", 10), ("new", 20)], true);
        let rows = view.rows();
        let saved = |name: &str| Which::Saved(name.into());
        assert_eq!(rows, [Which::Before, saved("new"), saved("old")]);

        let view = view_with(&[("first", 10), ("second", 10)], false);
        assert_eq!(view.rows(), [saved("second"), saved("first")]);
    }

    #[test]
    fn enter_restores_the_row_the_bar_is_on() {
        let mut view = view_with(&[("old", 10), ("new", 20)], false);
        press(&mut view, KeyCode::Down);
        let old = Which::Saved("old".into());
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Restore(old));
        press(&mut view, KeyCode::Down);
        assert_eq!(press(&mut view, KeyCode::Char('q')), Step::Close);
    }

    #[test]
    fn s_saves_under_the_name_typed_and_x_removes_only_after_a_yes() {
        let mut view = view_with(&[("work", 10)], false);
        type_text(&mut view, "s review ");
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Step::Save("review".into())
        );
        assert!(view.naming.is_none());
        // Nothing typed saves nothing.
        type_text(&mut view, "s");
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Stay);

        press(&mut view, KeyCode::Char('x'));
        assert_eq!(view.removing().as_deref(), Some("remove work? y/n"));
        assert_eq!(press(&mut view, KeyCode::Char('n')), Step::Stay);
        assert_eq!(view.removing(), None);
        press(&mut view, KeyCode::Char('x'));
        let work = Which::Saved("work".into());
        assert_eq!(press(&mut view, KeyCode::Char('y')), Step::Remove(work));
    }

    #[test]
    fn the_bar_follows_its_layout_when_the_list_comes_back() {
        let mut view = view_with(&[("a", 10), ("b", 20)], false);
        press(&mut view, KeyCode::Down);
        let mut layouts = Layouts::default();
        layouts.save("a", tabs_of("a", &[]), Programs::new(), 10);
        layouts.save("b", tabs_of("b", &[]), Programs::new(), 20);
        layouts.save("c", tabs_of("c", &[]), Programs::new(), 30);
        view.set_layouts(Ok(layouts.clone()), None);
        assert_eq!(view.rows()[view.highlighted], Which::Saved("a".into()));
        let c = Which::Saved("c".into());
        view.set_layouts(Ok(layouts), Some(&c));
        assert_eq!(view.rows()[view.highlighted], c);
    }
}
