//! The branch switcher, `B` in the sidebar: moves a project's main worktree
//! onto another of its branches without leaving crystal. Its local branches
//! and the remote ones no local branch has the name of are listed, the one
//! it's on marked, and filtered as you type; Enter switches, or, when
//! nothing matches, makes a branch with the name typed. A worktree with
//! changes not committed stops to ask what's to become of them first, the
//! way an editor does: stashed, brought along, committed, or thrown away.
//!
//! A linked worktree isn't switched: it's named after the branch it was
//! made for, in its directory, and moving it would make the name wrong.
//!
//! The state is plain data; the event loop lists the branches and runs the
//! switch off the loop, with [`crate::git::branches`].

use super::app::{Action, Hit, Loading, Outcome};
use super::fuzzy::{self, Match};
use super::sidebar::{ago, fit};
use super::text_input::TextInput;
use super::ui::{self, Look, ViewAreas};
use crate::git::branches::{self, Branch, Carry, Change};
use crate::shell;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::path::{Path, PathBuf};

/// How many branches are listed, at most.
const MOST_BRANCHES: usize = 500;

/// The rows above the list of branches: the query, and a rule under it.
const QUERY_ROWS: u16 = 2;

/// What's before the commit's message as it's typed.
const MESSAGE_LABEL: &str = "commit message: ";

/// A worktree's branches and its changes, as they were listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub branches: Vec<Branch>,
    pub changes: Vec<Change>,
}

/// Lists the branches of the worktree at `dir`, and what isn't committed
/// in it: run off the event loop.
pub fn read(dir: &Path) -> Result<Listed, String> {
    let listed = branches::list(dir).and_then(|branches| {
        Ok(Listed {
            branches,
            changes: branches::changes(dir)?,
        })
    });
    listed.map_err(|err| format!("{err:#}"))
}

/// The ways a worktree's changes can go with it to another branch, in the
/// order they're offered: the one that can be undone first, so Enter
/// stashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Stash,
    Bring,
    Commit,
    Discard,
}

pub const CHOICES: [Choice; 4] = [
    Choice::Stash,
    Choice::Bring,
    Choice::Commit,
    Choice::Discard,
];

impl Choice {
    pub fn key(self) -> char {
        match self {
            Choice::Stash => 's',
            Choice::Bring => 'b',
            Choice::Commit => 'c',
            Choice::Discard => 'd',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Choice::Stash => "stash them",
            Choice::Bring => "bring them along",
            Choice::Commit => "commit them",
            Choice::Discard => "throw them away",
        }
    }

    /// What it does, going `from` one branch `to` another.
    fn detail(self, from: &str, to: &str) -> String {
        match self {
            Choice::Stash => "kept in git's stash, to have back later".to_string(),
            Choice::Bring => format!("onto {to}, unless they're in files it changes"),
            Choice::Commit => format!("on {from} first, new files too"),
            Choice::Discard => "the changes to files git knows; new files stay".to_string(),
        }
    }

    /// What it was, when a switch it started comes back.
    fn of(carry: &Carry) -> Choice {
        match carry {
            Carry::Bring => Choice::Bring,
            Carry::Commit(_) => Choice::Commit,
            Carry::Discard(_) => Choice::Discard,
            Carry::Ask | Carry::Stash | Carry::Create => Choice::Stash,
        }
    }
}

/// Where the switcher is in a switch.
pub enum Stage {
    /// Choosing the branch.
    Pick,
    /// The worktree has changes, and the user is asked what's to become of
    /// them on the way to `target`. `armed` says `d` has been pressed once:
    /// a second throws them away.
    Dirty {
        target: Branch,
        changes: Vec<Change>,
        fingerprint: Vec<String>,
        choice: Choice,
        armed: bool,
    },
    /// Typing the message for the commit [`Choice::Commit`] makes.
    Commit {
        target: Branch,
        changes: Vec<Change>,
        fingerprint: Vec<String>,
        message: TextInput,
    },
    /// git is switching to `target`, with the changes as they were asked
    /// about, to ask again if it can't.
    Working {
        target: Branch,
        carry: Carry,
        changes: Vec<Change>,
        fingerprint: Vec<String>,
    },
}

pub struct Switcher {
    /// The worktree it switches.
    pub dir: PathBuf,
    /// The worktree's project and branch, for the header.
    pub place: String,
    pub query: TextInput,
    pub listed: Loading<Listed>,
    /// The branches that match the query, best first: their places in the
    /// list, with how they matched.
    matches: Vec<(usize, Match)>,
    /// The selected match, by its place in `matches`.
    pub selected: usize,
    /// Whether the user has moved the selection, or typed: from then on, a
    /// fresh list keeps the selection on its branch.
    touched: bool,
    pub stage: Stage,
    /// Why the last thing asked didn't happen, until the next key.
    pub problem: Option<String>,
    /// How many rows the list is drawn in, for paging.
    rows: u16,
}

impl Switcher {
    /// A switcher on the worktree at `dir`, until its branches are listed.
    pub fn new(dir: PathBuf, place: String) -> Switcher {
        Switcher {
            dir,
            place,
            query: TextInput::default(),
            listed: Loading::Reading,
            matches: Vec::new(),
            selected: 0,
            touched: false,
            stage: Stage::Pick,
            problem: None,
            rows: 20,
        }
    }

    /// What listing the branches takes, for the event loop to do.
    pub fn read(&self) -> Action {
        Action::ListBranches(self.dir.clone())
    }

    /// The size of the list's area, as `(rows, columns)`.
    pub fn set_size(&mut self, list: (u16, u16)) {
        self.rows = list.0;
    }

    /// Takes the branches listed, if they're this worktree's. Once the user
    /// has chosen one, the selection stays on it.
    pub fn listed_done(&mut self, dir: &Path, listed: Result<Listed, String>) {
        if dir != self.dir {
            return;
        }
        let kept = self
            .touched
            .then(|| self.selected_branch().map(|branch| branch.name.clone()))
            .flatten();
        self.listed = match listed {
            Ok(listed) => Loading::Read(listed),
            Err(err) => Loading::Failed(err),
        };
        self.filter();
        let at = kept.and_then(|name| {
            let branches = self.branches();
            let found = |(index, _): &(usize, Match)| branches[*index].name == name;
            self.matches.iter().position(found)
        });
        match at {
            Some(at) => self.selected = at,
            None => self.go_home(),
        }
    }

    pub fn branches(&self) -> &[Branch] {
        match &self.listed {
            Loading::Read(listed) => &listed.branches,
            _ => &[],
        }
    }

    pub fn changes(&self) -> &[Change] {
        match &self.listed {
            Loading::Read(listed) => &listed.changes,
            _ => &[],
        }
    }

    /// The branch the worktree is on: `None` when HEAD is detached.
    pub fn current(&self) -> Option<&Branch> {
        self.branches().iter().find(|branch| branch.current)
    }

    pub fn selected_branch(&self) -> Option<&Branch> {
        let (index, _) = self.matches.get(self.selected)?;
        self.branches().get(*index)
    }

    /// Whether a switch it started is under way.
    pub fn working(&self) -> bool {
        matches!(self.stage, Stage::Working { .. })
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        self.problem = None;
        match self.stage {
            Stage::Pick => self.on_pick_key(key),
            Stage::Dirty { .. } => self.on_dirty_key(key),
            Stage::Commit { .. } => self.on_commit_key(key),
            // git is at it already: Esc only closes the switcher, and how
            // it went is said at the bottom.
            Stage::Working { .. } if key.code == KeyCode::Esc => Outcome::Close,
            Stage::Working { .. } => Outcome::Stay,
        }
    }

    /// Pasted text goes into what's being typed: the query, or the commit's
    /// message.
    pub fn on_paste(&mut self, text: &str) {
        match &mut self.stage {
            Stage::Pick => {
                self.query.insert_str(text);
                self.query_changed();
            }
            Stage::Commit { message, .. } => message.insert_str(text),
            _ => {}
        }
    }

    /// A click on a branch selects it, and the wheel moves the selection,
    /// while one is being chosen.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        if !matches!(self.stage, Stage::Pick) {
            return Outcome::Stay;
        }
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(index)) => {
                self.touched = true;
                self.selected = index.min(self.matches.len().saturating_sub(1));
            }
            (MouseEventKind::ScrollUp, Hit::ViewList(_)) => self.select_by(-1),
            (MouseEventKind::ScrollDown, Hit::ViewList(_)) => self.select_by(1),
            _ => {}
        }
        Outcome::Stay
    }

    fn on_pick_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = usize::from(self.rows.saturating_sub(QUERY_ROWS + 1).max(1)) as isize;
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter => return self.switch_to_selected(),
            KeyCode::Up => self.select_by(-1),
            KeyCode::Down => self.select_by(1),
            KeyCode::Char('p') if ctrl => self.select_by(-1),
            KeyCode::Char('n') if ctrl => self.select_by(1),
            KeyCode::PageUp => self.select_by(-page),
            KeyCode::PageDown => self.select_by(page),
            _ => {
                let before = self.query.text().to_string();
                self.query.on_key(&key);
                if self.query.text() != before {
                    self.query_changed();
                }
            }
        }
        Outcome::Stay
    }

    /// Enter on the list: switches to the selected branch, or, when
    /// nothing matches, makes one with the name typed, from the commit the
    /// worktree is on.
    fn switch_to_selected(&mut self) -> Outcome {
        let Some(branch) = self.selected_branch().cloned() else {
            let name = self.query.text().trim().to_string();
            if matches!(self.listed, Loading::Read(_)) && !name.is_empty() {
                return self.start(Branch::new(&name), Carry::Create, Vec::new(), Vec::new());
            }
            return Outcome::Stay;
        };
        if branch.current {
            self.problem = Some(format!("the worktree is on {} already", branch.name));
            return Outcome::Stay;
        }
        if let Some(elsewhere) = &branch.elsewhere {
            self.problem = Some(format!(
                "{} is checked out in {}: git keeps a branch in one worktree",
                branch.name,
                shell::home_relative(elsewhere)
            ));
            return Outcome::Stay;
        }
        self.start(branch, Carry::Ask, Vec::new(), Vec::new())
    }

    fn on_dirty_key(&mut self, key: KeyEvent) -> Outcome {
        let Stage::Dirty { choice, armed, .. } = &mut self.stage else {
            return Outcome::Stay;
        };
        let at = CHOICES.iter().position(|each| each == choice).unwrap_or(0);
        match key.code {
            KeyCode::Esc => self.stage = Stage::Pick,
            KeyCode::Down | KeyCode::Char('j') => {
                *choice = CHOICES[(at + 1).min(CHOICES.len() - 1)];
                *armed = false;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *choice = CHOICES[at.saturating_sub(1)];
                *armed = false;
            }
            KeyCode::Enter => {
                let chosen = *choice;
                return self.choose(chosen);
            }
            KeyCode::Char(letter) => {
                if let Some(chosen) = CHOICES.iter().find(|each| each.key() == letter) {
                    return self.choose(*chosen);
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    /// Takes `chosen` for what's to become of the changes. Throwing them
    /// away takes it twice.
    fn choose(&mut self, chosen: Choice) -> Outcome {
        let detached = self.current().is_none();
        let Stage::Dirty {
            target,
            changes,
            fingerprint,
            choice,
            armed,
        } = &mut self.stage
        else {
            return Outcome::Stay;
        };
        let again = *armed && *choice == chosen;
        *choice = chosen;
        *armed = false;
        let (target, changes, fingerprint) = (target.clone(), changes.clone(), fingerprint.clone());
        match chosen {
            Choice::Stash => self.start(target, Carry::Stash, changes, fingerprint),
            Choice::Bring => self.start(target, Carry::Bring, changes, fingerprint),
            Choice::Commit if detached => {
                self.problem = Some(
                    "HEAD is detached, so a commit here would be on no branch: stash instead"
                        .to_string(),
                );
                Outcome::Stay
            }
            Choice::Commit => {
                self.stage = Stage::Commit {
                    target,
                    changes,
                    fingerprint,
                    message: TextInput::default(),
                };
                Outcome::Stay
            }
            Choice::Discard if again => {
                let carry = Carry::Discard(fingerprint.clone());
                self.start(target, carry, changes, fingerprint)
            }
            Choice::Discard => {
                if let Stage::Dirty { armed, .. } = &mut self.stage {
                    *armed = true;
                }
                Outcome::Stay
            }
        }
    }

    fn on_commit_key(&mut self, key: KeyEvent) -> Outcome {
        let Stage::Commit {
            target,
            changes,
            fingerprint,
            message,
        } = &mut self.stage
        else {
            return Outcome::Stay;
        };
        match key.code {
            KeyCode::Esc => {
                self.stage = Stage::Dirty {
                    target: target.clone(),
                    changes: std::mem::take(changes),
                    fingerprint: std::mem::take(fingerprint),
                    choice: Choice::Commit,
                    armed: false,
                };
            }
            KeyCode::Enter => {
                let text = message.text().trim().to_string();
                if text.is_empty() {
                    self.problem = Some("type the commit's message first".to_string());
                    return Outcome::Stay;
                }
                let (target, changes, fingerprint) = (
                    target.clone(),
                    std::mem::take(changes),
                    std::mem::take(fingerprint),
                );
                return self.start(target, Carry::Commit(text), changes, fingerprint);
            }
            _ => message.on_key(&key),
        }
        Outcome::Stay
    }

    /// Starts switching to `target`, the changes going as `carry` says.
    fn start(
        &mut self,
        target: Branch,
        carry: Carry,
        changes: Vec<Change>,
        fingerprint: Vec<String>,
    ) -> Outcome {
        let action = Action::SwitchBranch {
            dir: self.dir.clone(),
            target: target.clone(),
            carry: carry.clone(),
        };
        self.stage = Stage::Working {
            target,
            carry,
            changes,
            fingerprint,
        };
        Outcome::Do(action)
    }

    /// What the switch it started came to. Once it's on the branch, the
    /// switcher closes. Changes it didn't know about are asked about, and
    /// a switch that failed goes back to where it was started from, saying
    /// why; one that left the worktree changed goes back to the branches,
    /// listed again.
    pub fn switched(&mut self, outcome: branches::Outcome) -> Outcome {
        let Stage::Working {
            target,
            carry,
            changes,
            fingerprint,
        } = std::mem::replace(&mut self.stage, Stage::Pick)
        else {
            return Outcome::Stay;
        };
        match outcome {
            branches::Outcome::Switched { .. } => Outcome::Close,
            branches::Outcome::Dirty {
                changes,
                fingerprint,
            } => {
                if matches!(carry, Carry::Discard(_)) {
                    self.problem = Some(
                        "the changes are not as they were shown: look again before throwing them away"
                            .to_string(),
                    );
                }
                if let Loading::Read(listed) = &mut self.listed {
                    listed.changes = changes.clone();
                }
                self.stage = Stage::Dirty {
                    target,
                    changes,
                    fingerprint,
                    choice: Choice::Stash,
                    armed: false,
                };
                Outcome::Stay
            }
            branches::Outcome::Failed(why) => {
                if !changes.is_empty() {
                    self.stage = Stage::Dirty {
                        target,
                        changes,
                        fingerprint,
                        choice: Choice::of(&carry),
                        armed: false,
                    };
                }
                self.problem = Some(why);
                Outcome::Stay
            }
            branches::Outcome::Stopped(why) => {
                self.problem = Some(why);
                Outcome::Do(self.read())
            }
        }
    }

    fn query_changed(&mut self) {
        self.touched = true;
        self.filter();
        self.go_home();
    }

    fn select_by(&mut self, by: isize) {
        self.touched = true;
        let last = self.matches.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    /// Where the selection starts: the best match, or, with nothing typed,
    /// the first branch a switch can go to, so `B` then Enter goes
    /// somewhere.
    fn go_home(&mut self) {
        let branches = self.branches();
        let free = |(index, _): &(usize, Match)| {
            let branch = &branches[*index];
            !branch.current && branch.elsewhere.is_none()
        };
        self.selected = if self.query.text().trim().is_empty() {
            self.matches.iter().position(free).unwrap_or(0)
        } else {
            0
        };
    }

    fn filter(&mut self) {
        let names: Vec<String> = self
            .branches()
            .iter()
            .map(|branch| branch.name.clone())
            .collect();
        self.matches = fuzzy::filter(&names, self.query.text(), MOST_BRANCHES);
    }
}

/// How wide the list of branches is, for a view `width` columns wide.
pub fn list_width(width: u16) -> u16 {
    (width * 2 / 5).clamp(30.min(width), 64).min(width / 2)
}

/// The keys the footer shows while the switcher is open.
pub fn hints(switcher: &Switcher) -> Vec<(&'static str, &'static str)> {
    match switcher.stage {
        Stage::Pick => vec![("↑/↓", "select"), ("enter", "switch"), ("esc", "close")],
        Stage::Dirty { .. } => vec![
            ("s/b/c/d", "choose"),
            ("↑/↓", "move"),
            ("enter", "take it"),
            ("esc", "back"),
        ],
        Stage::Commit { .. } => vec![("enter", "commit and switch"), ("esc", "back")],
        Stage::Working { .. } => vec![("esc", "close; the footer says how it went")],
    }
}

/// Which branch's row is on screen `row`, in a list drawn in `area`.
pub fn list_hit(switcher: &Switcher, area: Rect, row: u16) -> Hit {
    let Some(row) = (row - area.y).checked_sub(QUERY_ROWS) else {
        return Hit::Elsewhere;
    };
    let height = area.height.saturating_sub(QUERY_ROWS);
    let index = list_offset(switcher.selected, height) + usize::from(row);
    if index < switcher.matches.len() {
        Hit::ViewList(index)
    } else {
        Hit::Elsewhere
    }
}

/// The first branch on screen: the list scrolls to keep the selection in
/// sight.
fn list_offset(selected: usize, height: u16) -> usize {
    let height = usize::from(height.max(1));
    (selected + 1).saturating_sub(height)
}

pub fn draw(frame: &mut Frame, switcher: &Switcher, look: &Look, areas: &ViewAreas) {
    let theme = look.theme;
    frame.render_widget(header(switcher, look, areas.header.width), areas.header);
    ui::draw_rule(frame, look, areas.rule);
    draw_query(frame, switcher, look, areas.list);
    let list = Rect::new(
        areas.list.x,
        areas.list.y + QUERY_ROWS,
        areas.list.width,
        areas.list.height.saturating_sub(QUERY_ROWS),
    );
    match &switcher.listed {
        Loading::Reading => ui::draw_message(frame, look, "listing branches…", list),
        Loading::Failed(err) => {
            let line = Line::styled(format!(" {err}"), Style::new().fg(theme.failed));
            frame.render_widget(line, Rect::new(list.x, list.y, list.width, 1));
        }
        Loading::Read(_) => draw_branches(frame, switcher, look, list),
    }
    let mut lines = match &switcher.stage {
        Stage::Pick => pick_lines(switcher, look),
        Stage::Dirty {
            target,
            choice,
            armed,
            changes,
            ..
        } => dirty_lines(switcher, target, *choice, *armed, changes, look),
        Stage::Commit {
            target, message, ..
        } => {
            let from = from_name(switcher);
            let lines = commit_lines(&from, target, message, look);
            // The message's line is the first.
            let area = inside(areas.content);
            let label = MESSAGE_LABEL.chars().count() as u16;
            let column = area.x + label + message.cursor() as u16;
            let right = area.right().saturating_sub(1);
            frame.set_cursor_position((column.min(right), area.y));
            lines
        }
        Stage::Working { target, .. } => vec![
            Line::raw(""),
            Line::styled(
                format!("switching to {}…", target.local_name()),
                Style::new().fg(theme.text),
            ),
        ],
    };
    if let Some(problem) = &switcher.problem {
        lines.push(Line::raw(""));
        lines.push(Line::styled(problem.clone(), Style::new().fg(theme.failed)));
    }
    let changes = match &switcher.stage {
        Stage::Dirty { changes, .. }
        | Stage::Commit { changes, .. }
        | Stage::Working { changes, .. }
            if !changes.is_empty() =>
        {
            changes.as_slice()
        }
        _ => switcher.changes(),
    };
    lines.extend(change_lines(changes, look));
    let content = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(content, inside(areas.content));
}

/// The content's area less a column each side, so wrapped lines keep in
/// line with the rest.
fn inside(content: Rect) -> Rect {
    Rect::new(
        content.x + 1,
        content.y,
        content.width.saturating_sub(2),
        content.height,
    )
}

/// "⎇ switch branch · 3 uncommitted changes", and where on the right.
fn header<'a>(switcher: &Switcher, look: &Look, width: u16) -> Line<'a> {
    let mut notes = Vec::new();
    if let Loading::Read(listed) = &switcher.listed {
        notes.push(changes_note(listed.changes.len()));
    }
    ui::view_header("⎇", "switch branch", &notes, &switcher.place, look, width)
}

fn changes_note(count: usize) -> String {
    match count {
        0 => "nothing uncommitted".to_string(),
        1 => "1 uncommitted change".to_string(),
        count => format!("{count} uncommitted changes"),
    }
}

/// The branch the worktree is on, as the switcher says it.
fn from_name(switcher: &Switcher) -> String {
    switcher
        .current()
        .map_or("the detached HEAD".to_string(), |branch| {
            branch.name.clone()
        })
}

/// The query, with the cursor in it while a branch is being chosen, and a
/// rule under it.
fn draw_query(frame: &mut Frame, switcher: &Switcher, look: &Look, list: Rect) {
    let theme = look.theme;
    let line = Line::from(vec![
        Span::styled(
            " › ",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            switcher.query.text().to_string(),
            Style::new().fg(theme.text),
        ),
    ]);
    frame.render_widget(line, Rect::new(list.x, list.y, list.width, 1));
    let rule = "─".repeat(usize::from(list.width));
    frame.render_widget(
        Line::styled(rule, Style::new().fg(theme.rule)),
        Rect::new(list.x, list.y + 1, list.width, 1),
    );
    if matches!(switcher.stage, Stage::Pick) {
        // The prompt is three columns wide.
        let column = list.x + 3 + switcher.query.cursor() as u16;
        frame.set_cursor_position((column.min(list.right().saturating_sub(1)), list.y));
    }
}

/// The branches that match, one a row: a mark for the one the worktree is
/// on, or one another worktree has, the name with the letters that matched
/// in the accent color and a remote's name muted, and how long ago its last
/// commit was on the right.
fn draw_branches(frame: &mut Frame, switcher: &Switcher, look: &Look, area: Rect) {
    let theme = look.theme;
    if switcher.matches.is_empty() {
        let name = switcher.query.text().trim();
        let message = if name.is_empty() {
            "no branches".to_string()
        } else {
            "no such branch".to_string()
        };
        ui::draw_message(frame, look, &message, area);
        return;
    }
    let first = list_offset(switcher.selected, area.height);
    let width = usize::from(area.width);
    let shown = switcher
        .matches
        .iter()
        .enumerate()
        .skip(first)
        .take(area.height.into());
    for (at, (index, found)) in shown {
        let branch = &switcher.branches()[*index];
        let row = Rect::new(area.x, area.y + (at - first) as u16, area.width, 1);
        if at == switcher.selected {
            frame.buffer_mut().set_style(row, theme.selection);
        }
        let (mark, mark_color) = if branch.current {
            ("●", theme.accent)
        } else if branch.elsewhere.is_some() {
            ("⎇", theme.muted)
        } else {
            (" ", theme.muted)
        };
        let when = ago(branch.committed, look.now);
        let name = fit(
            &branch.name,
            width.saturating_sub(4 + when.chars().count() + 2),
        );
        let remote_part = match branch.name.split_once('/') {
            Some((remote, _)) if branch.remote => remote.chars().count() + 1,
            _ => 0,
        };
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(mark, Style::new().fg(mark_color)),
            Span::raw(" "),
        ];
        for (place, letter) in name.chars().enumerate() {
            let style = if found.positions.contains(&place) {
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else if place < remote_part || branch.elsewhere.is_some() {
                Style::new().fg(theme.muted)
            } else {
                Style::new().fg(theme.text)
            };
            spans.push(Span::styled(letter.to_string(), style));
        }
        frame.render_widget(Line::from(spans), row);
        let when = Line::styled(format!("{when} "), Style::new().fg(theme.muted));
        frame.render_widget(when.right_aligned(), row);
    }
}

/// What the content says while a branch is being chosen: the selected
/// branch, its last commit, and what Enter does with it, or, when nothing
/// matches, the branch Enter makes.
fn pick_lines<'a>(switcher: &Switcher, look: &Look) -> Vec<Line<'a>> {
    let theme = look.theme;
    let bold = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
    let text = Style::new().fg(theme.text);
    let muted = Style::new().fg(theme.muted);
    if !matches!(switcher.listed, Loading::Read(_)) {
        return Vec::new();
    }
    let Some(branch) = switcher.selected_branch() else {
        let name = switcher.query.text().trim();
        if name.is_empty() {
            return Vec::new();
        }
        return vec![
            Line::styled(name.to_string(), bold),
            Line::raw(""),
            Line::styled(
                format!(
                    "Enter makes this branch from {}, and switches to it; the changes come along.",
                    from_name(switcher)
                ),
                text,
            ),
        ];
    };
    let what = if branch.current {
        "The worktree is on this branch.".to_string()
    } else if let Some(elsewhere) = &branch.elsewhere {
        format!(
            "Checked out in {}: git keeps a branch in one worktree.",
            shell::home_relative(elsewhere)
        )
    } else if branch.remote {
        format!(
            "Enter switches to it, as {}, a branch of your own that follows it.",
            branch.local_name()
        )
    } else {
        "Enter switches to it.".to_string()
    };
    vec![
        Line::styled(branch.name.clone(), bold),
        Line::from(vec![
            Span::styled(format!("{} · ", committed_when(branch, look.now)), muted),
            Span::styled(branch.subject.clone(), text),
        ]),
        Line::raw(""),
        Line::styled(what, text),
    ]
}

/// When a branch's last commit was made: "just now", or "3h ago".
fn committed_when(branch: &Branch, now: u64) -> String {
    match ago(branch.committed, now) {
        just if just == "now" => "just now".to_string(),
        ago => format!("{ago} ago"),
    }
}

/// The question about the changes, on the way to `target`, with the four
/// answers, the one the bar is on marked.
fn dirty_lines<'a>(
    switcher: &Switcher,
    target: &Branch,
    choice: Choice,
    armed: bool,
    changes: &[Change],
    look: &Look,
) -> Vec<Line<'a>> {
    let theme = look.theme;
    let from = from_name(switcher);
    let to = target.local_name();
    let mut lines = vec![
        Line::styled(
            format!(
                "{from} has {}. On the way to {to}, they're to be:",
                changes_note(changes.len())
            ),
            Style::new().fg(theme.text),
        ),
        Line::raw(""),
    ];
    let label_width = CHOICES
        .iter()
        .map(|each| each.label().len())
        .max()
        .unwrap_or(0);
    for each in CHOICES {
        let on = each == choice;
        let pointer = if on { "› " } else { "  " };
        let style = if on {
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text)
        };
        lines.push(Line::from(vec![
            Span::styled(pointer, Style::new().fg(theme.accent)),
            Span::styled(format!("{}  ", each.key()), Style::new().fg(theme.accent)),
            Span::styled(format!("{:<label_width$}  ", each.label()), style),
            Span::styled(each.detail(&from, to), Style::new().fg(theme.muted)),
        ]));
    }
    if armed {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "d again throws the changes away: they can't be had back",
            Style::new().fg(theme.failed),
        ));
    }
    lines
}

/// The box the commit's message is typed in.
fn commit_lines<'a>(
    from: &str,
    target: &Branch,
    message: &TextInput,
    look: &Look,
) -> Vec<Line<'a>> {
    let theme = look.theme;
    vec![
        Line::from(vec![
            Span::styled(
                MESSAGE_LABEL,
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(message.text().to_string(), Style::new().fg(theme.text)),
        ]),
        Line::raw(""),
        Line::styled(
            format!(
                "Everything is committed on {from}, new files too, then the worktree goes to {}.",
                target.local_name()
            ),
            Style::new().fg(theme.text),
        ),
    ]
}

/// The worktree's changes, a row each, under a blank line.
fn change_lines<'a>(changes: &[Change], look: &Look) -> Vec<Line<'a>> {
    let theme = look.theme;
    if changes.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        Line::raw(""),
        Line::styled(
            format!("{}:", changes_note(changes.len())),
            Style::new().fg(theme.muted),
        ),
    ];
    lines.extend(changes.iter().map(|change| {
        Line::from(vec![
            Span::styled(
                format!("  {} ", change.status),
                Style::new().fg(theme.working),
            ),
            Span::styled(change.path.clone(), Style::new().fg(theme.text)),
        ])
    }));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn branch(name: &str) -> Branch {
        Branch {
            remote: name.contains('/'),
            committed: 100,
            subject: format!("work on {name}"),
            ..Branch::new(name)
        }
    }

    fn change(path: &str) -> Change {
        Change {
            status: " M".into(),
            path: path.into(),
        }
    }

    fn switcher_with(branches: Vec<Branch>, changes: Vec<Change>) -> Switcher {
        let mut switcher = Switcher::new(PathBuf::from("/code/app"), "app ⌂ main".into());
        let listed = Listed { branches, changes };
        switcher.listed_done(Path::new("/code/app"), Ok(listed));
        switcher
    }

    fn three_branches() -> Vec<Branch> {
        let main = Branch {
            current: true,
            ..branch("main")
        };
        let fix = Branch {
            elsewhere: Some(PathBuf::from("/code/app.worktrees/fix")),
            ..branch("fix")
        };
        vec![main, fix, branch("feature"), branch("origin/theirs")]
    }

    fn type_text(switcher: &mut Switcher, text: &str) {
        for letter in text.chars() {
            switcher.on_key(key(KeyCode::Char(letter)));
        }
    }

    fn switch_to(target: &str, carry: Carry) -> Outcome {
        let target = match carry {
            Carry::Create => Branch::new(target),
            _ => branch(target),
        };
        Outcome::Do(Action::SwitchBranch {
            dir: PathBuf::from("/code/app"),
            target,
            carry,
        })
    }

    #[test]
    fn it_starts_on_the_first_branch_it_can_switch_to() {
        let switcher = switcher_with(three_branches(), Vec::new());
        assert_eq!(switcher.selected_branch().unwrap().name, "feature");
    }

    #[test]
    fn enter_switches_asking_about_changes_only_once_git_finds_them() {
        let mut switcher = switcher_with(three_branches(), Vec::new());
        assert_eq!(
            switcher.on_key(key(KeyCode::Enter)),
            switch_to("feature", Carry::Ask)
        );
        assert!(switcher.working());
        let dirty = branches::Outcome::Dirty {
            changes: vec![change("a.rs")],
            fingerprint: vec!["a".into()],
        };
        assert_eq!(switcher.switched(dirty), Outcome::Stay);
        assert!(matches!(switcher.stage, Stage::Dirty { .. }));
        assert_eq!(switcher.changes().len(), 1);
    }

    #[test]
    fn the_current_branch_and_one_checked_out_elsewhere_say_why_not() {
        let mut switcher = switcher_with(three_branches(), Vec::new());
        switcher.on_key(key(KeyCode::Up));
        assert_eq!(switcher.on_key(key(KeyCode::Enter)), Outcome::Stay);
        assert!(
            switcher
                .problem
                .as_ref()
                .unwrap()
                .contains("checked out in")
        );
        switcher.on_key(key(KeyCode::Up));
        switcher.on_key(key(KeyCode::Enter));
        assert_eq!(
            switcher.problem.as_deref(),
            Some("the worktree is on main already")
        );
    }

    #[test]
    fn a_name_nothing_matches_makes_a_branch() {
        let mut switcher = switcher_with(three_branches(), Vec::new());
        type_text(&mut switcher, "zzz-new");
        assert_eq!(switcher.selected_branch(), None);
        assert_eq!(
            switcher.on_key(key(KeyCode::Enter)),
            switch_to("zzz-new", Carry::Create)
        );
    }

    #[test]
    fn typing_filters_and_a_fresh_list_keeps_the_branch_chosen() {
        let mut switcher = switcher_with(three_branches(), Vec::new());
        type_text(&mut switcher, "theirs");
        assert_eq!(switcher.selected_branch().unwrap().name, "origin/theirs");
        let mut fresh = three_branches();
        fresh.insert(1, branch("newer"));
        let listed = Listed {
            branches: fresh,
            changes: Vec::new(),
        };
        switcher.listed_done(Path::new("/code/app"), Ok(listed));
        assert_eq!(switcher.selected_branch().unwrap().name, "origin/theirs");
    }

    /// A switcher asking what's to become of a change on the way to
    /// `feature`.
    fn asking() -> Switcher {
        let mut switcher = switcher_with(three_branches(), vec![change("a.rs")]);
        switcher.on_key(key(KeyCode::Enter));
        switcher.switched(branches::Outcome::Dirty {
            changes: vec![change("a.rs")],
            fingerprint: vec!["a".into()],
        });
        switcher
    }

    #[test]
    fn s_stashes_and_b_brings_the_changes_along() {
        let mut switcher = asking();
        assert_eq!(
            switcher.on_key(key(KeyCode::Char('s'))),
            switch_to("feature", Carry::Stash)
        );
        let mut switcher = asking();
        assert_eq!(
            switcher.on_key(key(KeyCode::Char('b'))),
            switch_to("feature", Carry::Bring)
        );
    }

    #[test]
    fn throwing_the_changes_away_takes_d_twice() {
        let mut switcher = asking();
        assert_eq!(switcher.on_key(key(KeyCode::Char('d'))), Outcome::Stay);
        assert_eq!(
            switcher.on_key(key(KeyCode::Char('d'))),
            switch_to("feature", Carry::Discard(vec!["a".into()]))
        );
    }

    #[test]
    fn committing_asks_for_a_message_first() {
        let mut switcher = asking();
        switcher.on_key(key(KeyCode::Char('c')));
        assert_eq!(switcher.on_key(key(KeyCode::Enter)), Outcome::Stay);
        assert!(switcher.problem.is_some());
        type_text(&mut switcher, "wip");
        assert_eq!(
            switcher.on_key(key(KeyCode::Enter)),
            switch_to("feature", Carry::Commit("wip".into()))
        );
    }

    #[test]
    fn a_failed_switch_goes_back_to_the_question_saying_why() {
        let mut switcher = asking();
        switcher.on_key(key(KeyCode::Char('b')));
        let failed = branches::Outcome::Failed("they collide".into());
        assert_eq!(switcher.switched(failed), Outcome::Stay);
        let Stage::Dirty { choice, .. } = switcher.stage else {
            panic!("it should ask again");
        };
        assert_eq!(choice, Choice::Bring);
        assert_eq!(switcher.problem.as_deref(), Some("they collide"));
    }

    #[test]
    fn a_switch_that_happened_closes_and_one_that_stopped_lists_again() {
        let mut switcher = switcher_with(three_branches(), Vec::new());
        switcher.on_key(key(KeyCode::Enter));
        let done = branches::Outcome::Switched {
            branch: "feature".into(),
            note: None,
        };
        assert_eq!(switcher.switched(done), Outcome::Close);

        let mut switcher = asking();
        switcher.on_key(key(KeyCode::Char('s')));
        let stopped = branches::Outcome::Stopped("left behind".into());
        assert_eq!(
            switcher.switched(stopped),
            Outcome::Do(Action::ListBranches(PathBuf::from("/code/app")))
        );
        assert!(matches!(switcher.stage, Stage::Pick));
    }
}
