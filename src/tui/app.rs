//! The TUI's state and how keys, the mouse and session lists change it.
//! Nothing here talks to the daemon or draws: when a key needs the outside
//! world, it comes back as an [`Action`] for the event loop to carry out.
//! That keeps every state change testable on its own. [`commands`] carries
//! out the layout commands `crystal tab` and `crystal pane` send.

mod commands;

pub use commands::Alone;

use super::archived_view::{self, ArchivedView};
use super::away::{Away, Tally};
use super::backlog_view::{BacklogChange, BacklogView, Step};
use super::command_line;
use super::command_list::{self, CommandList, Pick, PluginAction};
use super::compose::Typed;
use super::diff_view::{self, Against, DiffView};
use super::finder::Finder;
use super::grep::Grep;
use super::groups::{self, Row};
use super::help;
use super::issues::{self, IssuesView};
use super::keymap::{
    Bound, Chord, Command, CommandKind, KeyCommand, Keymap, Mode, ModeKey, Sequence, SplitWay,
    Translated,
};
use super::launcher::{self, Launcher, Memory, Run, Setup, Target};
use super::layouts::{self, Layouts, LayoutsView, Program, Programs, Which};
use super::memory_view::MemoryView;
use super::menu::{self, Item, Menu};
use super::needs_you::{self, NeedsYouView};
use super::plugins_view::{self, PluginsView};
use super::preview::Content;
use super::profiles::{self, ProfilesView};
use super::pull_requests::{self, PullRequestsView};
use super::reply::ReplyBox;
use super::restarted::{Restarted, Restarts};
use super::review;
use super::search::{self, Around, StatusFilter};
use super::settings_view::{self, SettingsView};
use super::split_tree::{Direction, Pane, SplitTree, Way};
use super::status::Status;
use super::switcher::{self, Switcher};
use super::tabs::Tabs;
use super::text_input::TextInput;
use super::timeline::{self, TimelineView};
use super::tree_browser::TreeBrowser;
use crate::catalog::{self, Agent};
use crate::client::Purpose;
use crate::config::{BarPosition, Config, Fold, MouseSettings, SIDEBAR_WIDTHS, TabBarSettings};
use crate::events::Event;
use crate::flow_run::{FlowRun, RunState};
use crate::flows::{self, Flow};
use crate::forge::{self, Checkout, Forge, Issue, PullRequest, PullRequestDetail, Topic};
use crate::git;
use crate::profile::{self, Profile};
use crate::project_commands::Verb;
use crate::protocol::{
    Activity, Answer, ArchivedSession, Backlog, ForgeLink, Front, SessionInfo, Spending, State,
    TaskBrief, TaskSpec, Worktree,
};
use crate::shell;
use crate::{backlog, names, plugins, tasks};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Where a pane sits beside the sidebar: the one that follows the
/// selection, or one of the splits, counted in the order they're drawn
/// (down the tab's tree of panes, left to right and top to bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Selected,
    Split(usize),
    /// The pane floating over the others: see [`App::floating`].
    Float,
}

/// Where the keyboard goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Keys move through the list and act on sessions.
    Sidebar,
    /// Keys go to the session in this pane.
    Pane(Slot),
    /// Keys move copy mode's cursor over this pane's screen and history,
    /// select from it and search it.
    Copy(Slot),
}

/// What the mouse is over, worked out from the layout by `ui::hit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A tab in the top bar, by its place among them.
    Tab(usize),
    /// A row of the sidebar, by its place in [`App::rows`].
    SidebarRow(usize),
    /// The sidebar, but none of its rows: below the last.
    Sidebar,
    /// The rule between the sidebar and the panes, which the mouse drags
    /// to resize the sidebar: the column the mouse is at.
    SidebarEdge(u16),
    /// The pane at `slot`. `cell` is the `(row, column)` on its session's
    /// screen, counted from 0, when the mouse is inside the pane's border.
    Pane {
        slot: Slot,
        cell: Option<(u16, u16)>,
    },
    /// The scrollbar beside the screen of the pane at `slot`: `row` of it,
    /// counted from 0 at the screen's top row.
    Scrollbar { slot: Slot, row: u16 },
    /// The border between the two sides of a split of the tab's panes: the
    /// rule between panes side by side, or, beside the name on it, the
    /// header line of a pane below another. `split` counts the splits in
    /// the order [`SplitTree::borders`] lists them; `at` is the column of
    /// the screen the mouse is at for a rule, or the row for a header line.
    Border { split: usize, at: u16 },
    /// A row of an open view's list, by its place in the whole list.
    ViewList(usize),
    /// The rest of an open view: the diff, or the file's preview.
    ViewContent,
    /// The rule between an open view's list and the rest, where the tree
    /// browser's is dragged: the column the mouse is at, counted from the
    /// view's left edge.
    ViewBorder(u16),
    /// The footer, or anywhere else.
    Elsewhere,
}

/// A pane being moved with the mouse, by its header line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grab {
    /// The pane taken.
    pub from: Slot,
    /// The pane the mouse is over now, if it's over one.
    pub over: Option<Slot>,
}

/// Something that takes the place of the sidebar and the panes until it's
/// closed: the diff of a worktree, the file finder, the tree browser, find
/// in files, the branch switcher, or a project's memory.
pub enum View {
    Diff(DiffView),
    Files(Finder),
    Tree(TreeBrowser),
    Grep(Grep),
    Branches(Switcher),
    Memory(MemoryView),
}

/// What an open view's key asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing outside the view.
    Stay,
    Close,
    /// Something for the event loop to do, like reading a diff.
    Do(Action),
    /// Close the view, and open this file, by its path from the top of the
    /// view's worktree, in the user's editor: at `line`, when there's one.
    Edit {
        path: String,
        line: Option<usize>,
    },
}

/// Something read off the event loop: still being read, read, or what went
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loading<T> {
    Reading,
    Read(T),
    Failed(String),
}

/// Panes `s` puts side by side are each at least this wide, which fits most
/// agents' screens; narrower than that, it puts them one above the other.
const SIDE_BY_SIDE_WIDTH: u16 = 80;

/// How much of a pane's room a split leaves it: half.
const HALF: f32 = 0.5;

/// How far a key in resize mode moves a border: a few columns, or a row
/// for every two of those, rows being about twice as tall as columns are
/// wide.
const RESIZE_COLUMNS: u16 = 4;
const RESIZE_ROWS: u16 = 2;

/// Which way Tab goes round the panes: Tab forward, Shift+Tab back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Round {
    Forward,
    Back,
}

/// Where a new session starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// In this directory, or the TUI's own when it's `None`.
    Directory(Option<PathBuf>),
    /// In a new worktree on `branch`, made in the repository at `base`, or
    /// the TUI's own directory's when that's `None`. A branch that exists
    /// already is checked out there, unless crystal `made_up` its name:
    /// then it's always a new branch, `branch-2` or the next number that's
    /// free when `branch` is taken.
    NewWorktree {
        branch: String,
        base: Option<PathBuf>,
        made_up: bool,
    },
    /// In a pull request's worktree: the one its project has on its branch
    /// already, or a new one with its commits fetched.
    PullRequest(Checkout),
}

/// A question asked on the footer line, and the answer typed so far.
#[derive(Debug)]
pub struct Prompt {
    pub question: Question,
    pub input: TextInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// The command line for a new session, which starts at the place: what
    /// the new-session panel hands over to with Ctrl+E.
    Command(Place),
    /// A new name for the session now called this.
    Rename(String),
    /// A name for the tab in front.
    TabName,
    /// A line on how the task of the session called `name` went, closing
    /// it done, or `failed`.
    CloseTask { name: String, failed: bool },
    /// Notes on what the flow run called this is to do differently,
    /// sending it back from its gate.
    SendFlowBack(String),
}

/// A question on the footer line that `y` answers yes and any other key
/// no, asked before something that can't be taken back, or that starts a
/// program again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    Kill(String),
    /// Stop this session and keep it in the archive.
    Archive(String),
    /// Start this ended session's command again.
    Respawn(String),
    /// Remove the linked worktree at `path`, which is on `branch`: with
    /// `force`, though it has changes not committed, which go with it.
    RemoveWorktree {
        path: PathBuf,
        branch: String,
        force: bool,
    },
    /// Remove the linked worktree at `path`, called `name`, which the
    /// session just killed was the last in.
    RemoveEmptied {
        path: PathBuf,
        name: String,
    },
    /// Close the tab in front, tab `number`, and kill the sessions in it.
    CloseTab {
        number: usize,
        sessions: Vec<String>,
    },
    /// Take the project called `name`, whose main worktree is `path`, off
    /// the list of those crystal knows.
    ForgetProject {
        path: PathBuf,
        name: String,
    },
}

impl Confirm {
    /// The question, the way the footer asks it.
    pub fn question(&self) -> String {
        match self {
            Confirm::Kill(name) => format!("kill {name}? y/n"),
            Confirm::Archive(name) => {
                format!("archive {name}? It stops; Z starts it again where it was. y/n")
            }
            Confirm::Respawn(name) => format!("start {name} again? y/n"),
            Confirm::RemoveWorktree {
                branch,
                force: false,
                ..
            } => format!("remove worktree {branch}? y/n"),
            Confirm::RemoveWorktree {
                branch,
                force: true,
                ..
            } => format!("{branch} has uncommitted changes: remove it and lose them? y/n"),
            Confirm::RemoveEmptied { name, .. } => {
                format!("nothing else is in worktree {name}: remove it too? y/n")
            }
            Confirm::CloseTab { number, sessions } => {
                let count = sessions.len();
                let noun = if count == 1 { "session" } else { "sessions" };
                format!("close tab {number} and kill its {count} {noun}? y/n")
            }
            Confirm::ForgetProject { name, .. } => {
                format!("take {name} off the list? Nothing on disk changes. y/n")
            }
        }
    }

    /// What a yes asks for.
    fn action(self) -> Action {
        match self {
            Confirm::Kill(name) => Action::Kill(name),
            Confirm::Archive(name) => Action::Archive(name),
            Confirm::Respawn(name) => Action::Respawn(name),
            Confirm::RemoveWorktree {
                path,
                branch,
                force,
            } => Action::RemoveWorktree {
                path,
                branch,
                force,
            },
            Confirm::RemoveEmptied { path, name } => Action::RemoveWorktree {
                path,
                branch: name,
                force: false,
            },
            Confirm::CloseTab { sessions, .. } => Action::KillAll(sessions),
            Confirm::ForgetProject { path, .. } => Action::ForgetProject(path),
        }
    }
}

/// The menu for a tab, once it's in front.
fn tab_menu() -> Vec<Item> {
    vec![
        Item::new("new tab", Command::NewTab),
        Item::new("name it", Command::RenameTab),
        Item::new("saved layouts", Command::Layouts),
        Item::new("the archive", Command::Archived),
        Item::danger("close it", Command::CloseTab),
    ]
}

/// What a key asks the event loop to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Start `command` in a new session at `place`. An empty command starts
    /// the user's shell. A `purpose` with a task makes the session a task.
    Start {
        place: Place,
        command: Vec<String>,
        purpose: Purpose,
    },
    /// Start a background task at `place`: Claude Code runs `spec` without
    /// a terminal, for backlog item `backlog` if it's for one, about the
    /// pull request or the issue `brief` says, if it's about one.
    StartInBackground {
        place: Place,
        spec: TaskSpec,
        backlog: Option<u64>,
        brief: TaskBrief,
    },
    /// Start a run of the flow called `flow` at `place`, on `goal`.
    StartFlow {
        place: Place,
        flow: String,
        goal: String,
    },
    /// Go on past the gate of the flow run called this.
    ApproveFlow(String),
    /// Send the flow run called `run` back from its gate, with `notes`.
    SendFlowBack {
        run: String,
        notes: String,
    },
    /// Run the step that stopped the flow run called this again.
    RetryFlow(String),
    /// Close the task of the session called `name`, done or `failed`.
    CloseTask {
        name: String,
        failed: bool,
        summary: String,
    },
    /// Answer the permission the background task called `name` asks for.
    Answer {
        name: String,
        answer: Answer,
    },
    /// Stop the run the background task called this is in the middle of.
    Interrupt(String),
    /// Send `text` to the session called `name` as the user: typed in with
    /// Enter after it, or a background task's follow-up.
    Reply {
        name: String,
        text: String,
    },
    /// Ask the daemon for the backlog of the project `dir` is in, for the
    /// backlog view that's open.
    ListBacklog(PathBuf),
    /// Make `change` to the backlog of the project `dir` is in, then list
    /// it again.
    ChangeBacklog {
        dir: PathBuf,
        change: BacklogChange,
    },
    Kill(String),
    /// Stop this session and keep it in the archive.
    Archive(String),
    /// Ask the daemon for the archive, and open the archive on it.
    ListArchived,
    /// Start the archived session with this id again.
    Unarchive(String),
    /// Take the archived session with this id out of the archive for good.
    DeleteArchived(String),
    /// Kill each of these sessions: those of a tab that was closed.
    KillAll(Vec<String>),
    /// Take the project whose main worktree is this off the list of those
    /// crystal knows.
    ForgetProject(PathBuf),
    /// Run the project's run command in `worktree`, or stop it, or its open
    /// command there.
    ProjectCommand {
        which: Verb,
        worktree: Worktree,
    },
    Rename {
        name: String,
        new_name: String,
    },
    /// Start this ended session's command again.
    Respawn(String),
    /// Open the background task called this in a terminal.
    TaskToTerminal(String),
    /// Remove the linked worktree at `path`, which is on `branch`: with
    /// `force`, though it has changes not committed.
    RemoveWorktree {
        path: PathBuf,
        branch: String,
        force: bool,
    },
    /// Send the key to the session in the pane at `to`.
    Type {
        to: Slot,
        key: KeyEvent,
    },
    /// Hand pasted text to the session in the pane at `to`.
    Paste {
        to: Slot,
        text: String,
    },
    /// Ask Codex, off the event loop, which models it lets the user choose.
    ReadCodexModels,
    /// Show a page further back into the history of the pane at this slot.
    PageBack(Slot),
    /// Show a page further toward live in the pane at this slot.
    PageForward(Slot),
    /// Show a few lines further back into the history of the pane at this
    /// slot: a notch of the mouse wheel.
    ScrollBack(Slot),
    /// Show a few lines further toward live in the pane at this slot.
    ScrollForward(Slot),
    /// A key for copy mode in the pane at `slot`.
    CopyKey {
        slot: Slot,
        key: KeyEvent,
    },
    /// Text pasted while copy mode in the pane at this slot has the keyboard.
    CopyPaste {
        slot: Slot,
        text: String,
    },
    /// The mouse went down on `cell` of the screen of the pane at `slot`,
    /// where a selection starts if it drags.
    SelectFrom {
        slot: Slot,
        cell: (u16, u16),
    },
    /// The mouse dragged to `cell` of the screen of the pane at `slot`.
    SelectTo {
        slot: Slot,
        cell: (u16, u16),
    },
    /// The mouse let go: put what it selected in the pane at this slot on
    /// the clipboard.
    CopySelection(Slot),
    /// The mouse let go, and what it selected is copied with copy mode's
    /// keys: copy mode comes on in the pane at this slot, if anything was
    /// selected, the selection kept.
    HoldSelection(Slot),
    /// The mouse went down on `row` of the scrollbar of the pane at
    /// `slot`: it takes the thumb there, or the thumb jumps there.
    GrabThumb {
        slot: Slot,
        row: u16,
    },
    /// The mouse dragged the thumb it took to `row` of the scrollbar.
    DragThumb {
        slot: Slot,
        row: u16,
    },
    /// Open `topic`, of the project at `project`, in the browser.
    OpenInBrowser {
        project: PathBuf,
        topic: Topic,
    },
    /// Ask the forge for the open issues of the project at this path, for
    /// the issues view that's now open.
    ListIssues(PathBuf),
    /// Ask the forge for the open pull requests of the project at this
    /// path, for the pull requests view that's now open.
    ListPullRequests(PathBuf),
    /// Ask the forges of the projects at these paths for their open pull
    /// requests, for `/` to find: the projects no session is in, which
    /// nothing else asks about.
    FindPullRequests(Vec<PathBuf>),
    /// Post `text` on `topic`, of the project at `project`.
    Comment {
        project: PathBuf,
        topic: Topic,
        text: String,
    },
    /// Give issue `number` of the project at `project` this title and text.
    EditIssue {
        project: PathBuf,
        number: u64,
        title: String,
        body: String,
    },
    /// Read the diff of the worktree at `dir`, off the event loop.
    ReadDiff {
        dir: PathBuf,
        against: Against,
    },
    /// Keep `marks` as the files reviewed in the diff `scope` names.
    KeepReviewed {
        scope: review::Scope,
        marks: review::Marks,
    },
    /// Keep whether the diff view lists its files as a tree.
    KeepTree(bool),
    /// List the files of the worktree at this directory, off the event
    /// loop.
    ReadFiles(PathBuf),
    /// Read and highlight the file at `path`, from the top of the worktree
    /// at `dir`, for a preview, off the event loop.
    ReadPreview {
        dir: PathBuf,
        path: String,
    },
    /// List the branches of the worktree at this directory, and its
    /// changes, off the event loop.
    ListBranches(PathBuf),
    /// Fetch the remotes of the worktree at `dir`, off the event loop:
    /// `now`, or unless they were a moment ago.
    FetchBranches {
        dir: PathBuf,
        now: bool,
    },
    /// Move the worktree at `dir` onto `target`, its changes going as
    /// `carry` says, off the event loop.
    SwitchBranch {
        dir: PathBuf,
        target: crate::git::branches::Branch,
        carry: crate::git::branches::Carry,
    },
    /// Search the worktree at `dir` for `query`, off the event loop, once
    /// the typing stops.
    Grep {
        dir: PathBuf,
        query: String,
    },
    /// Read the file at `path`, from the top of the worktree at `dir`, off
    /// the event loop, for find in files' preview.
    ReadMatchedFile {
        dir: PathBuf,
        path: String,
    },
    /// Put this path, from the top of a worktree, on the clipboard.
    CopyPath(String),
    /// Open the file at `path`, from the top of the worktree at `dir`, in
    /// the user's editor, at `line` if there's one, as a new session called
    /// `name`.
    Edit {
        dir: PathBuf,
        path: String,
        line: Option<usize>,
        name: String,
    },
    /// Open the history and screen of the pane at `slot` in the user's
    /// editor, as a new session called `name` in `dir`.
    EditHistory {
        slot: Slot,
        dir: PathBuf,
        name: String,
    },
    /// Read the memory of the project at `dir`, off the event loop.
    ReadMemory(PathBuf),
    /// Forget entry `id` of the memory of the project at `dir`.
    ForgetMemory {
        dir: PathBuf,
        id: u64,
    },
    /// Write entry `id` of the memory of the project at `dir` into its
    /// CLAUDE.md or AGENTS.md.
    PromoteMemory {
        dir: PathBuf,
        id: u64,
    },
    /// Write `profile` to the config file, in place of the profile called
    /// `replacing`, or as a new one.
    SaveProfile {
        replacing: Option<String>,
        profile: Box<Profile>,
    },
    /// Take the profile with this name out of the config file.
    DeleteProfile(String),
    /// Read which plugins there are, and open the plugins view on them.
    ListPlugins,
    /// The settings view has opened: read the settings, and again and
    /// again while it's open.
    OpenSettings,
    /// The settings view has closed.
    CloseSettings,
    /// Write a change to a setting to the config file, and follow it.
    ChangeSetting(settings_view::Change),
    /// Have the daemon get the model that searches memory by meaning ready.
    PrepareEmbeddings,
    /// Turn the plugin called `name` on, or off, in the config file.
    SwitchPlugin {
        name: String,
        on: bool,
    },
    /// Run one of an installed plugin's actions, about `context`.
    RunPlugin {
        plugin: String,
        action: String,
        context: plugins::Context,
    },
    /// Start one of an installed plugin's panes, and show it over the
    /// panes.
    OpenPluginPane {
        plugin: String,
        pane: String,
        context: plugins::Context,
    },
    /// Run one of the user's `[[keys.command]]`s, about `context`: in a
    /// popup, in a session in `dir` (the pane or tab it's in made ready
    /// already), or in the background.
    RunKeyCommand {
        command: Box<KeyCommand>,
        dir: Option<PathBuf>,
        context: plugins::Context,
    },
    /// Type into the plugin's pane that's open.
    TypeInPluginPane(KeyEvent),
    PasteInPluginPane(String),
    /// Close the plugin's pane that's open, and end its session.
    ClosePluginPane,
    /// Read the saved layouts, and open the layouts view on them.
    ListLayouts,
    /// Save the tabs as they are as the layout with this name.
    SaveLayout(String),
    /// Put the tabs back the way this layout has them.
    RestoreLayout(Which),
    RemoveLayout(Which),
    /// The timeline has opened: read the newest page of the event log for
    /// it, then follow the log as it grows.
    FollowEvents,
    /// The timeline has closed: stop following the log.
    StopFollowing,
    /// Read the page of the event log before the event with this `seq`,
    /// for the timeline.
    ReadOlderEvents(u64),
}

impl Action {
    /// Where the session it starts goes, for one that starts a session.
    pub fn place_mut(&mut self) -> Option<&mut Place> {
        match self {
            Action::Start { place, .. }
            | Action::StartInBackground { place, .. }
            | Action::StartFlow { place, .. } => Some(place),
            _ => None,
        }
    }
}

/// An action of one of the installed plugins, and the sidebar key it
/// took, if it took one: the `:` list offers every one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginKey {
    pub key: Option<Sequence>,
    pub plugin: String,
    pub action: String,
    pub title: String,
}

/// A plugin's pane, open over the panes: a session of its own, which ends
/// when the pane closes. A `[[keys.command]]` popup is one too, with no
/// plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPane {
    pub plugin: String,
    pub title: String,
    /// The session's name.
    pub session: String,
    /// A popup's width and height: a plugin's pane takes all the room
    /// beside the sidebar.
    pub popup: Option<Popup>,
}

/// How big a `[[keys.command]]` popup is: see [`keymap::Extent`].
///
/// [`keymap::Extent`]: super::keymap::Extent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Popup {
    pub width: Option<super::keymap::Extent>,
    pub height: Option<super::keymap::Extent>,
}

impl PluginPane {
    /// What its frame says on top.
    pub fn heading(&self) -> String {
        match self.popup {
            Some(_) => format!(" {} ", self.title),
            None => format!(" {} · {} ", self.plugin, self.title),
        }
    }
}

/// The sidebar narrowed to what matches what's typed, while `/` is open:
/// the sessions, of every tab, and the projects, worktrees with no
/// sessions, flow runs and open pull requests: see [`search`]. The
/// selection stays where it was until Enter picks what the bar is on.
#[derive(Debug, Default)]
pub struct Filter {
    pub input: TextInput,
    /// The one status Tab has picked, when it has: only the sessions with
    /// it are found.
    pub status: Option<StatusFilter>,
    /// What the bar is on: the rows are laid out again with every fresh
    /// list, so a row's place wouldn't keep to it.
    highlighted: Option<Found>,
}

/// What `/`'s bar can be on, which Enter or a click picks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A session, by its id: picking it selects it.
    Session(String),
    /// A worktree with no sessions, a project's main one or a linked one,
    /// by its directory: picking it puts the selection on it, where Enter
    /// starts something.
    Worktree(PathBuf),
    /// A flow run, by its name: picking it selects the step it's at.
    Flow(String),
    /// An open pull request, by its project's main worktree and its
    /// number: picking it opens the pull requests view on it.
    PullRequest { project: PathBuf, number: u64 },
}

/// How many pull requests and issues are open on a project's forge, for
/// the tab bar: each `None` until its forge has listed them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenOnForge {
    pub forge: Forge,
    pub pull_requests: Option<Counted>,
    pub issues: Option<Counted>,
}

/// How many of something there are to show, and whether the forge listed
/// as many as it gives at once, so there may be more beyond them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counted {
    pub count: usize,
    pub more: bool,
}

impl Counted {
    /// `count` shown, of `listed` the forge listed.
    fn of(count: usize, listed: usize) -> Counted {
        Counted {
            count,
            more: listed >= forge::LIMIT,
        }
    }
}

/// The title and text an issue was given in the issues view, and when the
/// forge said it had saved them.
#[derive(Debug, Clone)]
struct IssueEdit {
    title: String,
    body: String,
    saved: Instant,
}

pub struct App {
    /// In the sidebar's order: see [`groups`].
    sessions: Vec<SessionInfo>,
    /// An index into `sessions`, kept in range while there are any.
    selected: usize,
    /// The worktree with no sessions the selection is on instead, by its
    /// directory, when it's on one: see [`Row::NoSessions`].
    on_worktree: Option<PathBuf>,
    /// Each project's linked worktrees, by its main worktree, as git last
    /// listed them. Those with no sessions stay in the sidebar.
    worktrees: HashMap<PathBuf, Vec<Worktree>>,
    /// For the worktrees Claude Code made for itself, the subject of the
    /// commit each is at, by its directory, as git last said: what the
    /// sidebar names one by.
    subjects: HashMap<PathBuf, String>,
    /// The labels each project's linked worktrees were given, by their
    /// directories, as git was last asked: what the sidebar names one by.
    labels: HashMap<PathBuf, HashMap<PathBuf, String>>,
    /// What git last counted of each worktree the sidebar shows, by its
    /// directory: its changes not committed, and how far its branch is from
    /// its upstream.
    stats: HashMap<PathBuf, git::Stat>,
    /// The worktrees whose sessions have changed since the event loop last
    /// asked: they're counted again straight away.
    stats_due: HashSet<PathBuf>,
    /// The projects folded down to their headings in the sidebar, by their
    /// main worktrees, which the event loop keeps in the database.
    folded: BTreeSet<PathBuf>,
    /// The main worktrees of the projects crystal knows, as the daemon last
    /// listed them: those with no sessions stay in the sidebar, and the
    /// new-session panel offers them all.
    known: Vec<Worktree>,
    /// The worktrees the daemon is removing for this TUI, by their
    /// directories: their lines say so, and `W` leaves them be until the
    /// daemon says it's done.
    removing: HashSet<PathBuf>,
    /// The worktrees the daemon said it was removing when last asked,
    /// whoever asked for them, another TUI or `crystal worktree rm`: their
    /// lines say so too, and `W` leaves them be as well.
    removals: HashSet<PathBuf>,
    /// The question on the footer line, while one is being answered.
    prompt: Option<Prompt>,
    /// The new-session panel, while it's open.
    launcher: Option<Launcher>,
    /// What the panel held when `n` or `w` last opened it and it was put
    /// away with a task in it, for the next to open on. Kept while this TUI
    /// runs, and gone once a session has started from it.
    launch_draft: Option<launcher::Draft>,
    /// Whether the session being started was started from `launch_draft`,
    /// which goes once it has.
    draft_starting: bool,
    /// The box a reply to a session is written in, while it's open.
    reply: Option<ReplyBox>,
    /// The agents installed on this machine, which the panel offers.
    agents: Vec<&'static Agent>,
    /// The profiles in the config file, which the panel offers first and
    /// the profiles view changes.
    profiles: Vec<Profile>,
    /// Whether profiles are offered at all: see [`profile::enabled`].
    profiles_on: bool,
    /// The profiles view, while it's open.
    profiles_view: Option<ProfilesView>,
    /// The config's `new_session` when it has arguments, like `codex
    /// --full-auto`: offered as a profile of its own, ahead of the others.
    new_session_profile: Option<Profile>,
    /// What the panel picks at first, until something has been started
    /// from it: the config's `new_session`, by [`Run::key`].
    first_run: Option<String>,
    /// What the panel remembers: earlier tasks, and what ran last.
    memory: Memory,
    /// The models Codex lets the user choose, once asked for; empty while
    /// the answer is on its way.
    codex_models: Option<Vec<String>>,
    /// A yes-or-no question on the footer line, until it's answered.
    confirm: Option<Confirm>,
    /// The tabs, each with its own sessions and its own panes, and which
    /// one is in front. The sidebar shows only the sessions of the tab in
    /// front. A split stays on its session while the selection moves.
    tabs: Tabs,
    /// The room beside the sidebar the panes share, as the event loop last
    /// laid them out: splits, resizes and going from pane to pane are
    /// worked out in it. Until then, an 80 by 24 terminal's.
    tiles: Rect,
    /// Whether the sidebar's keys move the selected session's pane's
    /// borders: resize mode, which `R` starts.
    resizing: bool,
    /// The border the mouse took, while its button is down: the border
    /// follows it.
    border: Option<usize>,
    /// The session `>` is moving to another tab, while the footer asks
    /// which.
    moving: Option<String>,
    focus: Focus,
    /// The pane the keyboard was in last, so that Tab in the sidebar goes
    /// on to the next one.
    last_pane: Option<Slot>,
    /// The pane a drag of the mouse started in, while the button is down:
    /// the drag is a selection in that pane to the end, wherever it goes.
    dragging: Option<Slot>,
    /// The pane whose scrollbar's thumb the mouse took, while the button
    /// is down: the thumb follows it, wherever it goes.
    holding_thumb: Option<Slot>,
    /// The pane the keyboard was typing into when the mouse put it in copy
    /// mode, to go back to when copy mode is over.
    copied_from: Option<Slot>,
    /// The pane taken by its header line, while the button is down, and
    /// the pane the mouse is over now: letting go there swaps the two.
    grabbed: Option<Grab>,
    /// Where the mouse is on a pane's screen while Ctrl is held: the link
    /// there, if there's one, is underlined, for a click to open.
    link_hover: Option<(Slot, (u16, u16))>,
    /// The id of the session this TUI runs in, if it runs in one. The pane
    /// never shows it: it would be showing itself.
    own_id: Option<String>,
    /// Something to tell the user, like why a key didn't work. It stays
    /// until the next key.
    notice: Option<String>,
    /// The page the overlay listing every key is open at, while it's
    /// open.
    keys_page: Option<usize>,
    /// The whole terminal, as the event loop last drew it: what the list of
    /// keys has to fit.
    screen: Rect,
    /// `/`'s filter on the sidebar, while it's open.
    filter: Option<Filter>,
    /// What its forge said about each project's open pull requests, and
    /// those merged lately, by the project's main worktree: the forge and
    /// the pull requests, or why there are none to show.
    pull_requests: HashMap<PathBuf, Result<(Forge, Vec<PullRequest>), String>>,
    /// What its forge said about each project's open issues, the same way.
    open_issues: HashMap<PathBuf, Result<(Forge, Vec<Issue>), String>>,
    /// When each of those lists was asked of the forge, by project: a list
    /// asked before lands after it only when the forge took longer over it,
    /// and it's dropped.
    pull_requests_asked: HashMap<PathBuf, Instant>,
    issues_asked: HashMap<PathBuf, Instant>,
    /// The issues given a new title and text in the issues view, by project
    /// and number: laid over what the forge answers to anything asked before
    /// it saved them, which may not have them yet. Kept while the TUI runs.
    issue_edits: HashMap<(PathBuf, u64), IssueEdit>,
    /// Whether draft pull requests are left out of the pull requests view,
    /// the tab bar's count and `/`, as the settings say.
    hide_draft_prs: bool,
    /// Where new worktrees go, when the settings say: `[worktrees]
    /// directory`.
    worktree_directory: Option<PathBuf>,
    /// The issues view, while it's open.
    issues: Option<IssuesView>,
    /// The pull requests view, while it's open.
    pull_requests_view: Option<PullRequestsView>,
    /// The view that takes the place of the sidebar and the panes, while
    /// one is open.
    view: Option<View>,
    /// Whether the diff view lists its files as a tree, as it last did.
    diff_tree: bool,
    /// The worktrees git is switching to another branch, off the loop: one
    /// switch at a time in each.
    switching: HashSet<PathBuf>,
    /// Whether memory is on, which is whether `m` opens it: see
    /// [`crate::memory::enabled`].
    memory_on: bool,
    /// Whether tasks are on: closing them, and showing how they stand.
    tasks_on: bool,
    /// Whether the backlog is on: its view, and its counts in the sidebar.
    backlog_on: bool,
    /// Whether the github plugin is on: pull requests on worktree lines,
    /// `o`, `O` and `i`, on GitHub or GitLab.
    github_on: bool,
    /// The plugins view, while it's open.
    plugins_view: Option<PluginsView>,
    /// The settings view, while it's open.
    settings: Option<SettingsView>,
    /// The sidebar keys the installed plugins that are on took.
    plugin_keys: Vec<PluginKey>,
    /// A plugin's pane, while one is open.
    plugin_pane: Option<PluginPane>,
    /// The session whose task `c` is closing, while the footer asks whether
    /// it was done or failed.
    closing: Option<String>,
    /// The backlog view, while it's open.
    backlog: Option<BacklogView>,
    /// The layouts view, while it's open.
    layouts: Option<LayoutsView>,
    /// The archive, while it's open.
    archived: Option<ArchivedView>,
    /// The menu a right click opened, while it's open.
    menu: Option<Menu>,
    /// How many backlog items each project has to do, by its main
    /// worktree.
    backlog_counts: HashMap<PathBuf, usize>,
    /// Every flow run, as the daemon last listed them: the sidebar groups
    /// their steps' sessions under them.
    flows: Vec<FlowRun>,
    /// The flows in the config file, which the panel offers.
    flow_defs: Vec<Flow>,
    /// Whether flows are on: offered, shown, and answered at their gates.
    flows_on: bool,
    /// What background tasks have spent today, as the daemon last said.
    spending: Option<Spending>,
    /// The server the TUI is on, when it isn't the default one: the top bar
    /// names it.
    server: Option<String>,
    /// The timeline, while it's open.
    timeline: Option<TimelineView>,
    /// The list of everything waiting on the user, while it's open.
    needs_you: Option<NeedsYouView>,
    /// What happened while the user was away, and since when, until the
    /// timeline is opened at that point.
    away: Option<Away>,
    /// Whether the footer says what happened while the user was away: until
    /// the next key.
    away_shown: bool,
    /// What the TUI has seen of sessions starting again after a restart.
    restarts: Restarts,
    /// The footer's line on what a restart brought back and what it
    /// couldn't, until the next key.
    restarted: Option<Restarted>,
    /// Which command each sidebar key runs, the prefix and the key that
    /// hands the keyboard back: see [`super::keymap`].
    keymap: Keymap,
    /// Whether the prefix was pressed in a pane: the next key is a
    /// command's.
    prefixed: bool,
    /// The first of a plugin's two keys, pressed: the next key is the
    /// second.
    pending: Option<Chord>,
    /// The `:` list, while it's open.
    command_list: Option<CommandList>,
    /// What was run from the `:` list lately, the latest first.
    recent_commands: Vec<Pick>,
    /// How wide the sidebar is and whether it's folded.
    sidebar: Shape,
    /// What a folded sidebar keeps.
    fold: Fold,
    /// Whether the sessions that need the user lead the sidebar, from every
    /// tab.
    pin_needs_you: bool,
    /// Whether the sidebar's edge is being dragged with the mouse.
    dragging_sidebar: bool,
    /// Where the tab bar goes and what it shows at its right.
    tab_bar: TabBar,
    /// What the terminal's title says in place of `[window] title`, as
    /// `crystal title set` gave it, until `crystal title clear`.
    title_override: Option<String>,
    /// Where a new tab's shell starts, and a session started with none
    /// selected: `None` follows the selection. See
    /// [`crate::config::NewCwd`].
    start_dir: Option<PathBuf>,
    /// What the mouse does, as the config has it.
    mouse: MouseSettings,
}

/// The tab bar as the settings have it, and what its right shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabBar {
    pub position: BarPosition,
    /// Left out while there's only one tab.
    pub hide_when_single: bool,
    /// What goes between two things at its right.
    pub separator: String,
    /// What each of `[tab_bar] right` shows, as it was last worked out:
    /// see [`super::status_bar`].
    pub status: Vec<String>,
}

/// How wide the sidebar is and whether it's folded, as the user left it,
/// which the event loop keeps in the database. A width from the config
/// holds until the user resizes the sidebar, and again once the config
/// gives another: `from_config` is the config's width it was resized from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Shape {
    pub width: u16,
    pub from_config: u16,
    #[serde(skip)]
    pub folded: bool,
}

impl Default for Shape {
    fn default() -> Shape {
        let width = crate::config::SidebarSettings::default().width;
        Shape {
            width,
            from_config: width,
            folded: false,
        }
    }
}

/// How many columns `(` and `)` take from the sidebar or give it.
const SIDEBAR_STEP: i32 = 4;

/// How wide a folded sidebar's rail of marks is.
pub const RAIL_WIDTH: u16 = 3;

impl App {
    /// A TUI with no sessions yet. `own_id` is the id of the session it
    /// runs in, if it runs in one.
    pub fn new(own_id: Option<String>) -> App {
        App {
            sessions: Vec::new(),
            selected: 0,
            on_worktree: None,
            worktrees: HashMap::new(),
            subjects: HashMap::new(),
            labels: HashMap::new(),
            stats: HashMap::new(),
            stats_due: HashSet::new(),
            folded: BTreeSet::new(),
            known: Vec::new(),
            removing: HashSet::new(),
            removals: HashSet::new(),
            prompt: None,
            launcher: None,
            launch_draft: None,
            draft_starting: false,
            reply: None,
            agents: Vec::new(),
            profiles: Vec::new(),
            profiles_on: profile::enabled(&Config::default()),
            profiles_view: None,
            new_session_profile: None,
            first_run: None,
            memory: Memory::default(),
            codex_models: None,
            confirm: None,
            tabs: Tabs::default(),
            tiles: Rect::new(29, 1, 51, 22),
            resizing: false,
            border: None,
            moving: None,
            focus: Focus::Sidebar,
            last_pane: None,
            dragging: None,
            holding_thumb: None,
            copied_from: None,
            grabbed: None,
            link_hover: None,
            own_id,
            notice: None,
            keys_page: None,
            screen: Rect::new(0, 0, 80, 24),
            filter: None,
            pull_requests: HashMap::new(),
            open_issues: HashMap::new(),
            pull_requests_asked: HashMap::new(),
            issues_asked: HashMap::new(),
            issue_edits: HashMap::new(),
            hide_draft_prs: false,
            worktree_directory: None,
            issues: None,
            pull_requests_view: None,
            view: None,
            diff_tree: false,
            switching: HashSet::new(),
            memory_on: true,
            tasks_on: true,
            backlog_on: true,
            github_on: true,
            plugins_view: None,
            settings: None,
            plugin_keys: Vec::new(),
            plugin_pane: None,
            closing: None,
            backlog: None,
            layouts: None,
            archived: None,
            menu: None,
            backlog_counts: HashMap::new(),
            flows: Vec::new(),
            flow_defs: Vec::new(),
            flows_on: true,
            spending: None,
            server: None,
            timeline: None,
            needs_you: None,
            away: None,
            away_shown: false,
            restarts: Restarts::default(),
            restarted: None,
            keymap: Keymap::default(),
            prefixed: false,
            pending: None,
            command_list: None,
            recent_commands: Vec::new(),
            sidebar: Shape::default(),
            fold: Fold::Marks,
            pin_needs_you: true,
            dragging_sidebar: false,
            tab_bar: TabBar {
                separator: TabBarSettings::default().separator,
                ..TabBar::default()
            },
            title_override: None,
            start_dir: None,
            mouse: MouseSettings::default(),
        }
    }

    /// Takes the keys and the sidebar's settings from the config. A config
    /// whose keys don't make sense never loads, so this keeps the defaults
    /// for it only in a config made up in a test.
    pub fn set_interface(&mut self, config: &Config) {
        self.keymap = Keymap::new(&config.keys).unwrap_or_default();
        let settings = &config.sidebar;
        if settings.width != self.sidebar.from_config {
            self.sidebar.width = settings.width;
            self.sidebar.from_config = settings.width;
        }
        self.fold = settings.fold;
        self.pin_needs_you = settings.needs_you;
        self.mouse = config.mouse.clone();
        let bar = &config.tab_bar;
        self.tab_bar.position = bar.position;
        self.tab_bar.hide_when_single = bar.hide_when_single;
        self.tab_bar.separator = bar.separator.clone();
        // What's shown follows the entries: a line for each, once it's
        // worked out.
        if bar.right.len() != self.tab_bar.status.len() {
            self.tab_bar.status = vec![String::new(); bar.right.len()];
        }
    }

    /// The tab bar's settings, and what its right shows.
    pub fn tab_bar(&self) -> &TabBar {
        &self.tab_bar
    }

    /// Whether the tab bar is drawn: unless it's left out while there's
    /// only one tab, and there is.
    pub fn tab_bar_shown(&self) -> bool {
        !(self.tab_bar.hide_when_single && self.tabs.all().len() == 1)
    }

    /// Takes what each thing at the tab bar's right shows now.
    pub fn set_status(&mut self, status: Vec<String>) {
        self.tab_bar.status = status;
    }

    /// What `crystal title set` gave the terminal's title, until `crystal
    /// title clear`.
    pub fn title_override(&self) -> Option<&str> {
        self.title_override.as_deref()
    }

    /// Takes where a new tab's shell starts, and a session started with
    /// none selected: `None` to follow the selection.
    pub fn set_start_dir(&mut self, dir: Option<PathBuf>) {
        self.start_dir = dir;
    }

    /// Whether each pane has a scrollbar beside its screen.
    pub fn scrollbars(&self) -> bool {
        self.mouse.scrollbars
    }

    /// Takes the sidebar's shape the TUI kept, and whether the config has it
    /// start folded. A kept width holds only while the config's width is
    /// the one it was resized from.
    pub fn set_sidebar(&mut self, kept: Option<Shape>, folded: bool) {
        if let Some(kept) = kept
            && kept.from_config == self.sidebar.from_config
            && SIDEBAR_WIDTHS.contains(&kept.width)
        {
            self.sidebar.width = kept.width;
        }
        self.sidebar.folded = folded;
    }

    /// The sidebar's shape, to keep.
    pub fn sidebar_shape(&self) -> Shape {
        self.sidebar
    }

    /// How many columns the sidebar takes: its width, or folded, a rail of
    /// marks or nothing; never so many that the panes have less than a
    /// third of `screen_width`.
    pub fn sidebar_columns(&self, screen_width: u16) -> u16 {
        if self.sidebar.folded {
            return match self.fold {
                Fold::Marks => RAIL_WIDTH.min(screen_width),
                Fold::Hidden => 0,
            };
        }
        let most = (screen_width * 2 / 3).max(*SIDEBAR_WIDTHS.start());
        self.sidebar.width.min(most).min(screen_width)
    }

    /// Whether the sidebar is folded.
    pub fn sidebar_folded(&self) -> bool {
        self.sidebar.folded
    }

    /// Whether the sidebar's edge is being dragged.
    pub fn dragging_sidebar(&self) -> bool {
        self.dragging_sidebar
    }

    /// `(` and `)`: the sidebar `by` columns wider, or narrower. Folded,
    /// wider unfolds it.
    fn resize_sidebar(&mut self, by: i32) {
        if self.sidebar.folded {
            if by > 0 {
                self.sidebar.folded = false;
            }
            return;
        }
        let width = (i32::from(self.sidebar.width) + by).clamp(
            i32::from(*SIDEBAR_WIDTHS.start()),
            i32::from(*SIDEBAR_WIDTHS.end()),
        );
        self.sidebar.width = width as u16;
    }

    /// Where the sidebar's edge has been dragged to: the sidebar takes the
    /// columns left of it, unfolding if it was folded.
    fn drag_sidebar_to(&mut self, column: u16) {
        let most = (self.screen.width * 2 / 3).max(*SIDEBAR_WIDTHS.start());
        let width = column.clamp(*SIDEBAR_WIDTHS.start(), *SIDEBAR_WIDTHS.end());
        self.sidebar.width = width.min(most);
        self.sidebar.folded = false;
    }

    /// `\`: folds the sidebar, or unfolds it.
    fn fold_sidebar(&mut self) {
        self.sidebar.folded = !self.sidebar.folded;
    }

    /// The keymap the TUI goes by.
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Whether the prefix was pressed, in a pane, and the next key is a
    /// command's.
    pub fn prefixed(&self) -> bool {
        self.prefixed
    }

    /// The `:` list, while it's open.
    pub fn command_list(&self) -> Option<&CommandList> {
        self.command_list.as_ref()
    }

    /// Takes which of crystal's plugins the config has on.
    pub fn set_features(&mut self, config: &Config) {
        self.tasks_on = tasks::enabled(config);
        self.backlog_on = backlog::enabled(config);
        self.memory_on = crate::memory::enabled(config);
        self.profiles_on = profile::enabled(config);
        self.github_on = forge::enabled(config);
        self.hide_draft_prs = config.forge.hide_draft_prs;
        self.flows_on = flows::enabled(config);
        self.worktree_directory = config.worktrees.directory();
    }

    /// Whether the TUI asks the forge about the sessions' projects.
    pub fn github_on(&self) -> bool {
        self.github_on
    }

    /// Whether the one of crystal's plugins called `name` is on, as the
    /// TUI last read the config.
    pub fn plugin_on(&self, name: &str) -> bool {
        match name {
            "tasks" => self.tasks_on,
            "backlog" => self.backlog_on,
            "memory" => self.memory_on,
            "profiles" => self.profiles_on,
            "github" => self.github_on,
            "flows" => self.flows_on,
            _ => true,
        }
    }

    /// Takes the sidebar keys the installed plugins that are on took.
    pub fn set_plugin_keys(&mut self, keys: Vec<PluginKey>) {
        self.plugin_keys = keys;
    }

    /// The plugins' keys as the `?` overlay lists them: the key, and what
    /// it does.
    pub fn plugin_key_rows(&self) -> Vec<(String, String)> {
        self.plugin_keys
            .iter()
            .filter_map(|taken| {
                let key = taken.key?;
                Some((key.label(), format!("{}: {}", taken.plugin, taken.title)))
            })
            .collect()
    }

    /// Opens the plugins view on `plugins`, or, when it's open, shows them
    /// as they are now.
    pub fn show_plugins(&mut self, plugins: Vec<plugins_view::Listed>) {
        match &mut self.plugins_view {
            Some(view) => view.set_plugins(plugins),
            None => self.plugins_view = Some(PluginsView::new(plugins)),
        }
    }

    /// Says, in the plugins view if it's open, why something asked of a
    /// plugin couldn't be done.
    pub fn plugin_failed(&mut self, problem: String) {
        match &mut self.plugins_view {
            Some(view) => view.set_problem(problem),
            None => self.notify(problem),
        }
    }

    pub fn plugins_view(&self) -> Option<&PluginsView> {
        self.plugins_view.as_ref()
    }

    pub fn settings_view(&self) -> Option<&SettingsView> {
        self.settings.as_ref()
    }

    /// Shows the settings as they are now, in the settings view, if it's
    /// still open.
    pub fn show_settings(&mut self, current: settings_view::Current) {
        if let Some(view) = &mut self.settings {
            view.set_current(current);
        }
    }

    /// Says, in the settings view if it's open, why a setting wasn't
    /// changed.
    pub fn setting_failed(&mut self, problem: String) {
        match &mut self.settings {
            Some(view) => view.set_problem(problem),
            None => self.notify(problem),
        }
    }

    /// A plugin's pane has opened, over the panes, with the keyboard.
    pub fn plugin_pane_opened(&mut self, pane: PluginPane) {
        self.plugins_view = None;
        self.plugin_pane = Some(pane);
    }

    pub fn plugin_pane_closed(&mut self) {
        self.plugin_pane = None;
    }

    pub fn plugin_pane(&self) -> Option<&PluginPane> {
        self.plugin_pane.as_ref()
    }

    /// Whether flows are on, which is whether the daemon is asked for them.
    pub fn shows_flows(&self) -> bool {
        self.flows_on
    }

    /// Whether the TUI shows tasks: under sessions, and in pane headers.
    pub fn shows_tasks(&self) -> bool {
        self.tasks_on
    }

    /// The agents installed on this machine, for the new-session panel.
    pub fn set_agents(&mut self, agents: Vec<&'static Agent>) {
        self.agents = agents;
    }

    /// Takes the server the TUI is on, when it isn't the default one.
    pub fn set_server(&mut self, server: Option<String>) {
        self.server = server;
    }

    /// The server the TUI is on, when it isn't the default one.
    pub fn server(&self) -> Option<&str> {
        self.server.as_deref()
    }

    /// Takes what the config file says about starting sessions: its
    /// profiles, and `new_session`, what the panel picks at first. A
    /// `new_session` with arguments, like `codex --full-auto`, is offered
    /// as a profile of its own.
    pub fn set_launch_settings(&mut self, config: &Config) {
        self.profiles = config.profiles.clone();
        self.flow_defs = config.flows.clone();
        self.new_session_profile = None;
        self.first_run = None;
        let words = command_line::parse(&config.new_session).unwrap_or_default();
        let Some((program, args)) = words.split_first() else {
            return;
        };
        if args.is_empty() {
            self.first_run = Some(program.clone());
        } else if catalog::find(program).is_some() {
            let profile = Profile {
                name: config.new_session.clone(),
                args: args.to_vec(),
                ..Profile::for_agent(program)
            };
            self.first_run = Some(Run::Profile(profile.clone()).key());
            self.new_session_profile = Some(profile);
        }
    }

    /// The profiles view, while it's open.
    pub fn profiles_view(&self) -> Option<&ProfilesView> {
        self.profiles_view.as_ref()
    }

    /// The config file as it is after a profile was saved or taken out:
    /// the panel offers what it now says, and the open view shows it, its
    /// bar on the profile called `select`.
    pub fn profiles_saved(&mut self, config: &Config, select: Option<&str>) {
        self.set_launch_settings(config);
        if let Some(view) = &mut self.profiles_view {
            view.saved(self.profiles.clone(), select);
        }
    }

    /// Why a profile couldn't be saved or taken out, for the open view to
    /// say.
    pub fn profile_failed(&mut self, problem: String) {
        if let Some(view) = &mut self.profiles_view {
            view.failed(problem);
        }
    }

    /// What the panel remembers, as kept on disk.
    pub fn set_memory(&mut self, memory: Memory) {
        self.memory = memory;
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    /// The new-session panel, while it's open.
    pub fn launcher(&self) -> Option<&Launcher> {
        self.launcher.as_ref()
    }

    /// The reply box, while it's open.
    pub fn reply(&self) -> Option<&ReplyBox> {
        self.reply.as_ref()
    }

    /// Space: opens the reply box for the session called `name`, to send
    /// it what to do next without going into its pane. Not for one that
    /// has ended, nor the session this TUI runs in, which would be typing
    /// into the TUI.
    fn open_reply(&mut self, name: &str) {
        let Some(session) = self.position(name).map(|at| &self.sessions[at]) else {
            return self.notify("there's no session selected".into());
        };
        if self.is_own(session) {
            return self.notify("crystal can't reply to the session it runs in".into());
        }
        if session.state != State::Running {
            return self.notify(format!("{name} has ended: Enter starts it again"));
        }
        let label = match &session.front {
            Some(Front::Task) => "a follow-up: its next run, which carries its conversation on",
            Some(front) if front.is_agent() => {
                "its agent's next prompt, typed in with Enter after it"
            }
            _ => "typed into it, with Enter after it",
        };
        self.reply = Some(ReplyBox::new(name, label));
    }

    /// A key in the reply box, which has every key while it's open.
    fn on_reply_key(&mut self, key: KeyEvent) -> Option<Action> {
        let reply = self.reply.as_mut()?;
        match reply.on_key(&key) {
            Typed::Stay => None,
            Typed::Cancel => {
                self.reply = None;
                None
            }
            Typed::Send(text) => Some(Action::Reply {
                name: reply.name.clone(),
                text,
            }),
        }
    }

    /// The reply to the session called `name` was sent, or why it wasn't:
    /// sent, the box goes; refused, it stays, what's written and all, and
    /// says why.
    pub fn replied(&mut self, name: &str, sent: Result<(), String>) {
        match sent {
            Ok(()) => {
                if self.reply.as_ref().is_some_and(|reply| reply.name == name) {
                    self.reply = None;
                }
                self.notify(format!("sent to {name}"));
            }
            Err(reason) => match self.reply.as_mut().filter(|reply| reply.name == name) {
                Some(reply) => reply.refused(reason),
                None => self.notify(reason),
            },
        }
    }

    /// Takes the models Codex lets the user choose.
    pub fn set_codex_models(&mut self, models: Vec<String>) {
        if let Some(launcher) = &mut self.launcher {
            launcher.set_codex_models(models.clone());
        }
        if let Some(view) = &mut self.profiles_view {
            view.set_codex_models(models.clone());
        }
        self.codex_models = Some(models);
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Whether the overlay listing every key is open.
    pub fn showing_keys(&self) -> bool {
        self.keys_page.is_some()
    }

    /// The page of the list of keys that's open.
    pub fn keys_page(&self) -> usize {
        self.keys_page.unwrap_or(0)
    }

    /// Takes the whole terminal's size, as the event loop lays it out.
    pub fn set_screen(&mut self, screen: Rect) {
        self.screen = screen;
    }

    /// How many pages the list of keys takes on the screen.
    fn keys_pages(&self) -> usize {
        let plugin_on = |plugin: &str| self.plugin_on(plugin);
        let plugin_keys = self.plugin_key_rows();
        let shown = help::Shown {
            plugin_on: &plugin_on,
            plugin_keys: &plugin_keys,
            keymap: &self.keymap,
        };
        help::page_count(&shown, self.screen)
    }

    /// The diff or the file finder, while one is open.
    pub fn view(&self) -> Option<&View> {
        self.view.as_ref()
    }

    /// Takes whether the diff view lists its files as a tree, as it last
    /// did.
    pub fn set_diff_tree(&mut self, on: bool) {
        self.diff_tree = on;
    }

    /// Takes a diff read for the diff view, if it's still the one it wants.
    pub fn diff_read(
        &mut self,
        dir: &Path,
        against: Against,
        read: Result<diff_view::Read, String>,
    ) {
        if let Some(View::Diff(diff)) = &mut self.view {
            diff.read_done(dir, against, read);
        }
    }

    /// Takes the files listed for the file finder or the tree browser, if
    /// it's still open on their worktree. The selected file's preview is to
    /// be read next.
    pub fn files_read(&mut self, dir: &Path, files: Result<Vec<String>, String>) -> Option<Action> {
        match &mut self.view {
            Some(View::Files(finder)) => finder.files_read(dir, files),
            Some(View::Tree(tree)) => tree.files_read(dir, files),
            _ => None,
        }
    }

    /// Takes a file read for the preview of the file finder or the tree
    /// browser, if it still shows it.
    pub fn preview_read(&mut self, dir: &Path, path: &str, read: Result<Content, String>) {
        match &mut self.view {
            Some(View::Files(finder)) => finder.preview_read(dir, path, read),
            Some(View::Tree(tree)) => tree.preview_read(dir, path, read),
            _ => {}
        }
    }

    /// Takes the branches listed for the branch switcher, if it's still
    /// open on their worktree, and what that asks for next: listed the
    /// first time, the remotes fetched.
    pub fn branches_listed(
        &mut self,
        dir: &Path,
        listed: Result<switcher::Listed, String>,
    ) -> Option<Action> {
        match &mut self.view {
            Some(View::Branches(switcher)) => switcher.listed_done(dir, listed),
            _ => None,
        }
    }

    /// The remotes of the worktree at `dir` have been fetched, or why they
    /// couldn't, for the switcher, if it's still open on it.
    pub fn branches_fetched(&mut self, dir: &Path, fetched: Result<(), String>) -> Option<Action> {
        match &mut self.view {
            Some(View::Branches(switcher)) => switcher.fetched(dir, fetched),
            _ => None,
        }
    }

    /// git is done switching the worktree at `dir`. The switcher that
    /// started it takes what it came to, if it's still open on it; or else
    /// the footer says.
    pub fn branch_switched(
        &mut self,
        dir: &Path,
        outcome: crate::git::branches::Outcome,
    ) -> Option<Action> {
        use crate::git::branches::Outcome as Switch;
        self.switching.remove(dir);
        if let Switch::Switched { branch, note } = &outcome {
            let notice = match note {
                Some(note) => format!("the worktree is on {branch} · {note}"),
                None => format!("the worktree is on {branch}"),
            };
            self.notify(notice);
        }
        if let Some(View::Branches(switcher)) = &mut self.view
            && switcher.dir == dir
            && switcher.working()
        {
            let next = switcher.switched(outcome);
            return self.follow(next);
        }
        match outcome {
            Switch::Switched { .. } => {}
            Switch::Dirty { changes, .. } => {
                let count = changes.len();
                let noun = if count == 1 { "change" } else { "changes" };
                self.notify(format!(
                    "not switched: {count} uncommitted {noun}; B to choose what becomes of them"
                ));
            }
            Switch::Failed(why) | Switch::Stopped(why) => {
                self.notify(format!("couldn't switch: {why}"));
            }
        }
        None
    }

    /// Takes what a search found, if find in files is still open on its
    /// worktree and query. The first hit's file is to be read next.
    pub fn searched(
        &mut self,
        dir: &Path,
        query: &str,
        found: Result<crate::git::Found, String>,
    ) -> Option<Action> {
        let Some(View::Grep(grep)) = &mut self.view else {
            return None;
        };
        grep.searched(dir, query, found)
    }

    /// Takes a file's lines, if find in files still has a hit in it
    /// selected.
    pub fn matched_file_read(
        &mut self,
        dir: &Path,
        path: &str,
        lines: Result<Vec<crate::syntax::Runs>, String>,
    ) {
        if let Some(View::Grep(grep)) = &mut self.view {
            grep.file_read(dir, path, lines);
        }
    }

    /// Takes a project's memory, read for the memory view, if it's still
    /// open on that project.
    pub fn memory_read(&mut self, dir: &Path, read: Result<Vec<crate::memory::Listed>, String>) {
        if let Some(View::Memory(memory)) = &mut self.view {
            memory.read_done(dir, read);
        }
    }

    /// Tells an open view how big its list and the rest of it are drawn,
    /// as `(rows, columns)`: what a page is, and whether side by side fits.
    pub fn set_view_size(&mut self, list: (u16, u16), content: (u16, u16)) {
        match &mut self.view {
            Some(View::Diff(diff)) => diff.set_size(content),
            Some(View::Files(finder)) => finder.set_size(list, content),
            Some(View::Tree(tree)) => tree.set_size(content),
            Some(View::Grep(grep)) => grep.set_size(list),
            Some(View::Branches(switcher)) => switcher.set_size(list),
            Some(View::Memory(memory)) => memory.set_size(list),
            None => {}
        }
    }

    pub fn notify(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    pub fn timeline_view(&self) -> Option<&TimelineView> {
        self.timeline.as_ref()
    }

    pub fn needs_you_view(&self) -> Option<&NeedsYouView> {
        self.needs_you.as_ref()
    }

    /// Everything waiting on the user now, in every tab: see
    /// [`needs_you::rows`].
    pub fn needs_you_rows(&self) -> Vec<needs_you::Row> {
        needs_you::rows(&self.sessions, self.shown_flows())
    }

    /// The footer's line on what happened while the user was away, until
    /// the next key.
    pub fn away_line(&self) -> Option<&str> {
        let away = self.away.as_ref().filter(|_| self.away_shown)?;
        Some(&away.line)
    }

    /// The footer's line on what a restart brought back and what it
    /// couldn't, until the next key.
    pub fn restarted(&self) -> Option<&Restarted> {
        self.restarted.as_ref()
    }

    /// Takes what the event log gained while the user was away. When
    /// something worth saying happened, the footer says it, and the
    /// timeline opened next marks what's new since.
    pub fn set_away(&mut self, tally: &Tally) {
        if let Some(line) = tally.line(&self.needs_you_rows()) {
            self.away = Some(Away {
                line,
                after: tally.after,
            });
            self.away_shown = true;
        }
    }

    /// Takes a page of the event log, for the timeline, and asks for the
    /// one before it when the timeline wants more to fill itself.
    pub fn events_read(&mut self, page: Result<Vec<Event>, String>) -> Option<Action> {
        let view = self.timeline.as_mut()?;
        // The timeline says why its first page couldn't be read; the footer
        // says why one further back couldn't.
        let failed = match (&page, view.list.items()) {
            (Err(reason), Some(Ok(_))) => Some(format!("couldn't read further back: {reason}")),
            _ => None,
        };
        view.read(page);
        let older = view.wants_older(false);
        if let Some(failed) = failed {
            self.notify(failed);
        }
        older.map(Action::ReadOlderEvents)
    }

    /// Takes an event that has just happened, for the timeline.
    pub fn logged(&mut self, event: Event) {
        if let Some(view) = &mut self.timeline {
            view.logged(event);
        }
    }

    pub fn sessions(&self) -> &[SessionInfo] {
        &self.sessions
    }

    /// Takes a fresh list of flow runs from the daemon, and puts the
    /// sessions in order again around them.
    pub fn set_flows(&mut self, runs: Vec<FlowRun>) {
        self.flows = runs;
        self.set_sessions(self.sessions.clone());
    }

    pub fn flows(&self) -> &[FlowRun] {
        &self.flows
    }

    /// Takes what the daemon says background tasks have spent today.
    pub fn set_spending(&mut self, spending: Spending) {
        self.spending = Some(spending);
    }

    /// What background tasks have spent today, once there's something
    /// spent to show.
    pub fn spending(&self) -> Option<Spending> {
        self.spending.filter(|spending| spending.today_usd > 0.0)
    }

    /// The flow run the session at `index` is a step of, and which step.
    pub fn flow_step_of(&self, index: usize) -> Option<(&FlowRun, usize)> {
        let session = self.sessions.get(index)?;
        let (run, step) = groups::flow_step(session, self.shown_flows())?;
        Some((&self.flows[run], step))
    }

    /// Whether any session's agent is working, which is when the TUI keeps
    /// drawing to turn its mark.
    pub fn anything_working(&self) -> bool {
        self.sessions.iter().any(|session| {
            session.state == State::Running && session.activity == Some(Activity::Working)
        })
    }

    /// The sidebar's rows: the sessions under their projects and worktrees,
    /// only those that match while `/`'s filter is open. The linked
    /// worktrees with no sessions come under their projects too, but while
    /// the filter is open, it adds those it found, with the rest it found:
    /// see [`App::add_found`]. A folded project is its heading alone.
    pub fn rows(&self) -> Vec<Row> {
        self.rows_folded(true)
    }

    /// The sidebar's rows with every project unfolded: where the selection
    /// can be, seen or not.
    fn all_rows(&self) -> Vec<Row> {
        self.rows_folded(false)
    }

    fn rows_folded(&self, fold: bool) -> Vec<Row> {
        let shown = self.matches();
        let empty = match self.filter {
            Some(_) => Vec::new(),
            None => self.empty_worktrees(),
        };
        let mut rows = groups::rows(&self.sessions, self.shown_flows(), &empty, |index| {
            shown.contains(&index)
        });
        if let Some(filter) = &self.filter {
            self.add_found(filter, &mut rows);
        }
        // The projects with no sessions go after those with some, ahead of
        // the sessions outside any repository.
        if self.filter.is_none() {
            let mut quiet = Vec::new();
            for project in self.quiet_projects() {
                quiet.push(Row::Project {
                    name: project.project.clone(),
                    path: project.path.clone(),
                });
                quiet.push(Row::Worktree {
                    project: project.path.clone(),
                    path: project.path.clone(),
                    branch: project.branch.clone(),
                    main: true,
                    in_progress: project.in_progress,
                });
                quiet.push(Row::NoSessions(project.path.clone()));
                quiet.extend(groups::empty_rows(&empty, &project.path));
            }
            let at = rows
                .iter()
                .position(|row| *row == Row::OutsideGit)
                .unwrap_or(rows.len());
            rows.splice(at..at, quiet);
        }
        if !self.tasks_on {
            rows.retain(|row| !matches!(row, Row::Task(_)));
        }
        if fold {
            rows = self.fold(rows);
        }
        let pinned = self.pinned();
        if pinned.is_empty() {
            return rows;
        }
        let mut with_pinned = vec![Row::NeedsYou(pinned.len())];
        with_pinned.extend(pinned.into_iter().map(Row::Pinned));
        with_pinned.extend(rows);
        with_pinned
    }

    /// `rows` with each folded project down to its heading.
    fn fold(&self, rows: Vec<Row>) -> Vec<Row> {
        if self.folded.is_empty() || self.filter.is_some() {
            return rows;
        }
        let mut folding = false;
        rows.into_iter()
            .filter(|row| match row {
                Row::Project { path, .. } => {
                    folding = self.folded.contains(path);
                    true
                }
                Row::OutsideGit => {
                    folding = false;
                    true
                }
                _ => !folding,
            })
            .collect()
    }

    /// Adds to the `rows` of the sessions `/`'s filter found the rest of
    /// what it found, each under its project: the projects with no
    /// sessions, the worktrees with none and the open pull requests that
    /// match. Only once something's typed, and while no status is picked:
    /// before that, they would be everything crystal knows, and they have
    /// no status.
    fn add_found(&self, filter: &Filter, rows: &mut Vec<Row>) {
        let query = filter.input.text();
        if filter.status.is_some() || query.trim().is_empty() {
            return;
        }
        // Each project's rows, in the order their headings would go in.
        let mut found: Vec<(PathBuf, Vec<Row>)> = Vec::new();
        let mut under = |project: &Path, more: Vec<Row>| match found
            .iter_mut()
            .find(|(path, _)| path == project)
        {
            Some((_, rows)) => rows.extend(more),
            None => found.push((project.to_path_buf(), more)),
        };
        for project in self.quiet_projects() {
            if search::worktree_match(query, project, None) {
                under(
                    &project.path,
                    vec![
                        Row::Worktree {
                            project: project.path.clone(),
                            path: project.path.clone(),
                            branch: project.branch.clone(),
                            main: true,
                            in_progress: project.in_progress,
                        },
                        Row::NoSessions(project.path.clone()),
                    ],
                );
            }
        }
        let mut empty = self.empty_worktrees();
        empty.retain(|worktree| {
            let path = &worktree.path;
            let also = self.subject_of(path).or_else(|| self.label_of(path));
            search::worktree_match(query, worktree, also)
        });
        let mut projects: Vec<&PathBuf> = empty.iter().map(|w| &w.project_path).collect();
        projects.extend(self.pull_requests.keys());
        projects.sort_by_key(|project| self.known_place(project));
        projects.dedup();
        for project in projects {
            under(project, groups::empty_rows(&empty, project));
            let name = self.project_name(project);
            under(project, self.found_pull_requests(query, project, &name));
        }
        found.retain(|(_, rows)| !rows.is_empty());
        let found = found
            .into_iter()
            .map(|(path, rows)| (self.project_name(&path), path, rows))
            .collect();
        search::place_under_projects(rows, found);
    }

    /// The rows of the open pull requests of the project at `project`,
    /// called `name`, that match `query`, as its forge listed them, but the
    /// drafts while the settings hide them: none while the github plugin is
    /// off, or before its forge has said.
    fn found_pull_requests(&self, query: &str, project: &Path, name: &str) -> Vec<Row> {
        if !self.github_on {
            return Vec::new();
        }
        let Some(Ok((_, pull_requests))) = self.pull_requests.get(project) else {
            return Vec::new();
        };
        pull_requests
            .iter()
            .filter(|pull_request| !pull_request.merged)
            .filter(|pull_request| !(self.hide_draft_prs && pull_request.draft))
            .filter(|pull_request| search::pull_request_match(query, pull_request, name).is_some())
            .map(|pull_request| Row::PullRequest {
                project: project.to_path_buf(),
                number: pull_request.number,
            })
            .collect()
    }

    /// Where the project at `project` comes among the projects crystal
    /// knows, for the order of the headings `/` adds: the ones it doesn't
    /// know after them, by their paths.
    fn known_place(&self, project: &Path) -> (usize, PathBuf) {
        let place = self.known.iter().position(|known| known.path == project);
        (place.unwrap_or(usize::MAX), project.to_path_buf())
    }

    /// The name of the project at `project`, by its main worktree: as its
    /// sessions or crystal's list of projects say, or else its directory's.
    fn project_name(&self, project: &Path) -> String {
        let named = self
            .sessions
            .iter()
            .filter_map(|session| session.worktree.as_ref())
            .chain(&self.known)
            .find(|worktree| worktree.project_path == project);
        match named {
            Some(worktree) => worktree.project.clone(),
            None => project
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        }
    }

    /// The sessions the sidebar pins at its top, from every tab: those
    /// waiting on the user, then those that finished a turn nobody has
    /// looked at, each tab's in its order, the tab in front's first. None
    /// while `/`'s filter is open, which finds sessions in every tab itself.
    fn pinned(&self) -> Vec<usize> {
        if !self.pin_needs_you || self.filter.is_some() {
            return Vec::new();
        }
        let count = self.tabs.all().len();
        let first = self.tabs.current_index();
        let in_order: Vec<usize> = (0..count)
            .flat_map(|step| self.sessions_in((first + step) % count))
            .collect();
        [Status::Waiting, Status::Done]
            .into_iter()
            .flat_map(|wanted| {
                in_order
                    .iter()
                    .copied()
                    .filter(move |&index| Status::of(&self.sessions[index]) == wanted)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The tab the session at `index` is in, when it isn't the tab in
    /// front: its name, or its number when it has none, as the top bar
    /// shows it.
    pub fn tab_elsewhere(&self, index: usize) -> Option<String> {
        let name = &self.sessions.get(index)?.name;
        let tab = self.tabs.tab_of(name)?;
        if tab == self.tabs.current_index() {
            return None;
        }
        let named = &self.tabs.all()[tab].name;
        Some(if named.is_empty() {
            (tab + 1).to_string()
        } else {
            named.clone()
        })
    }

    /// The projects crystal knows that no session is in, in any tab, by
    /// their main worktrees: they come last in the sidebar, in every tab.
    fn quiet_projects(&self) -> Vec<&Worktree> {
        self.known
            .iter()
            .filter(|project| {
                !self.sessions.iter().any(|session| {
                    let worktree = session.worktree.as_ref();
                    worktree.is_some_and(|w| w.project_path == project.path)
                })
            })
            .collect()
    }

    /// Takes the projects crystal knows, by their main worktrees, as the
    /// daemon listed them.
    pub fn set_known_projects(&mut self, projects: Vec<Worktree>) {
        self.known = projects;
        self.keep_selection_on_a_row();
    }

    /// The projects crystal knows, by their main worktrees.
    pub fn known_projects(&self) -> &[Worktree] {
        &self.known
    }

    /// The linked worktrees git listed that no session is in, in any tab.
    fn empty_worktrees(&self) -> Vec<Worktree> {
        let linked: Vec<Worktree> = self.worktrees.values().flatten().cloned().collect();
        groups::empty_worktrees(&linked, &self.sessions)
    }

    /// Adds the linked worktrees the sessions are in to those git listed.
    /// A worktree a session is in is there, without asking git; and when
    /// its last session goes, it stays known until git is next asked, so
    /// it doesn't leave the sidebar in between.
    fn know_sessions_worktrees(&mut self) {
        let linked = self
            .sessions
            .iter()
            .filter_map(|session| session.worktree.as_ref())
            .filter(|worktree| !worktree.main);
        for worktree in linked {
            let known = self
                .worktrees
                .entry(worktree.project_path.clone())
                .or_default();
            if !known.iter().any(|w| w.path == worktree.path) {
                known.push(worktree.clone());
            }
        }
    }

    /// Takes what git listed as the linked worktrees of `project`, by its
    /// main worktree.
    pub fn set_worktrees(&mut self, project: PathBuf, worktrees: Vec<Worktree>) {
        self.worktrees.insert(project, worktrees);
        self.know_sessions_worktrees();
        self.keep_selection_on_a_row();
    }

    /// Takes the subject of the commit each of the worktrees Claude Code
    /// made for itself in `project` is at, by its directory, in place of
    /// those known before.
    pub fn set_subjects(&mut self, project: &Path, subjects: HashMap<PathBuf, String>) {
        self.subjects.retain(|path, _| !path.starts_with(project));
        self.subjects.extend(subjects);
    }

    /// The subject of the commit the worktree at `path` is at, when it's
    /// one Claude Code made for itself and git has said.
    pub fn subject_of(&self, path: &Path) -> Option<&str> {
        self.subjects.get(path).map(String::as_str)
    }

    /// Takes the labels the linked worktrees of `project` were given, by
    /// their directories, in place of those known before.
    pub fn set_labels(&mut self, project: &Path, labels: HashMap<PathBuf, String>) {
        if labels.is_empty() {
            self.labels.remove(project);
        } else {
            self.labels.insert(project.to_path_buf(), labels);
        }
    }

    /// The label the worktree at `path` was given, if it has one.
    pub fn label_of(&self, path: &Path) -> Option<&str> {
        self.labels
            .values()
            .find_map(|labels| labels.get(path))
            .map(String::as_str)
    }

    /// Takes what git counted of the worktree at `path`, or forgets it when
    /// git couldn't say.
    pub fn set_stat(&mut self, path: PathBuf, stat: Option<git::Stat>) {
        match stat {
            Some(stat) => self.stats.insert(path, stat),
            None => self.stats.remove(&path),
        };
    }

    /// What git last counted of the worktree at `path`.
    pub fn stat_of(&self, path: &Path) -> Option<&git::Stat> {
        self.stats.get(path)
    }

    /// The worktrees whose lines the sidebar shows, by their directories:
    /// those to count. None while it's folded.
    pub fn shown_worktrees(&self) -> Vec<PathBuf> {
        if self.sidebar.folded {
            return Vec::new();
        }
        self.rows()
            .into_iter()
            .filter_map(|row| match row {
                Row::Worktree { path, .. } => Some(path),
                _ => None,
            })
            .collect()
    }

    /// The worktrees to count again now, since a session in one changed.
    pub fn take_stats_due(&mut self) -> HashSet<PathBuf> {
        std::mem::take(&mut self.stats_due)
    }

    /// The projects folded down to their headings, to keep.
    pub fn folded_projects(&self) -> &BTreeSet<PathBuf> {
        &self.folded
    }

    /// Takes the projects the TUI kept folded.
    pub fn set_folded_projects(&mut self, folded: BTreeSet<PathBuf>) {
        self.folded = folded;
    }

    /// Whether the project with its main worktree at `path` is folded down
    /// to its heading: never while `/`'s filter is open, which finds
    /// sessions wherever they are.
    pub fn is_folded(&self, path: &Path) -> bool {
        self.filter.is_none() && self.folded.contains(path)
    }

    /// The folded project the selection is in, out of sight, by its main
    /// worktree: its heading has the bar.
    pub fn folded_selection(&self) -> Option<&Path> {
        let project = &self.selection_worktree()?.project_path;
        self.is_folded(project).then_some(project.as_path())
    }

    /// `h`: folds the project the selection is in down to its heading. The
    /// selection stays where it was, out of sight, the heading taking the
    /// bar.
    fn fold_project(&mut self) {
        if self.filter.is_some() {
            return;
        }
        match self.selection_worktree() {
            Some(worktree) => {
                let project = worktree.project_path.clone();
                self.folded.insert(project);
            }
            None => self.notify("only a project folds".to_string()),
        }
    }

    /// `l`, or `Enter` on a folded project's heading: unfolds the project
    /// the selection is in.
    fn unfold_project(&mut self) {
        if let Some(worktree) = self.selection_worktree() {
            let project = worktree.project_path.clone();
            self.folded.remove(&project);
        }
    }

    /// A click on a project's heading folds the project, or unfolds it.
    fn toggle_fold(&mut self, project: &Path) {
        if !self.folded.remove(project) {
            self.folded.insert(project.to_path_buf());
        }
    }

    /// The sessions of the tab in front a folded project's heading stands
    /// for, by index.
    pub fn folded_sessions(&self, project: &Path) -> Vec<usize> {
        self.in_tab()
            .into_iter()
            .filter(|&index| {
                let worktree = self.sessions[index].worktree.as_ref();
                worktree.is_some_and(|worktree| worktree.project_path == project)
            })
            .collect()
    }

    /// Asks again before removing the worktree at `path`, on `branch`,
    /// which git found changes not committed in: a yes forces it, and they
    /// go with it. Until then, nothing is removing it.
    pub fn ask_to_force_removal(&mut self, path: PathBuf, branch: String) {
        self.removing.remove(&path);
        self.confirm = Some(Confirm::RemoveWorktree {
            path,
            branch,
            force: true,
        });
    }

    /// The worktree at `path` has been removed: it leaves the sidebar now,
    /// rather than when git is next asked.
    pub fn worktree_removed(&mut self, path: &Path) {
        self.removing.remove(path);
        self.removals.remove(path);
        for linked in self.worktrees.values_mut() {
            linked.retain(|worktree| worktree.path != path);
        }
        self.keep_selection_on_a_row();
    }

    /// The worktree at `path` wasn't removed, for `reason`: it stays, and
    /// can be asked about again.
    pub fn worktree_not_removed(&mut self, path: &Path, reason: String) {
        self.removing.remove(path);
        self.removals.remove(path);
        self.notify(reason);
    }

    /// Takes the worktrees the daemon is removing, whoever asked, and says
    /// whether one it was removing before is done with, gone or not, for
    /// the event loop to have git list the worktrees again.
    pub fn set_removals(&mut self, worktrees: Vec<PathBuf>) -> bool {
        let removals: HashSet<PathBuf> = worktrees.into_iter().collect();
        let done = self.removals.difference(&removals).next().is_some();
        self.removals = removals;
        done
    }

    /// Whether the daemon is removing the worktree at `path`, for this TUI
    /// or for anyone else.
    pub fn removing(&self, path: &Path) -> bool {
        self.removing.contains(path) || self.removals.contains(path)
    }

    /// The linked worktree with no sessions the selection is on, if it's
    /// on one rather than on a session.
    pub fn selected_empty_worktree(&self) -> Option<&Worktree> {
        let path = self.on_worktree.as_ref()?;
        self.worktrees
            .values()
            .flatten()
            .chain(&self.known)
            .find(|worktree| worktree.path == *path)
    }

    /// Moves the selection off an empty worktree's row once that row has
    /// gone. When it went because a session is in the worktree now, the
    /// selection goes to that session: it stays in the worktree. When the
    /// worktree was removed, or isn't in the tab in front, it goes back to
    /// the selected session.
    fn keep_selection_on_a_row(&mut self) {
        // While `/`'s filter is open, the rows are what it found: the
        // selection is looked at again once it closes.
        if self.filter.is_some() {
            return;
        }
        let Some(path) = self.on_worktree.clone() else {
            return;
        };
        if self.all_rows().contains(&Row::NoSessions(path.clone())) {
            return;
        }
        self.on_worktree = None;
        let in_it = self.in_tab().into_iter().find(|&index| {
            let worktree = self.sessions[index].worktree.as_ref();
            worktree.is_some_and(|worktree| worktree.path == path)
        });
        if let Some(index) = in_it {
            self.selected = index;
        }
    }

    /// `/`'s filter, while it's open.
    pub fn filter(&self) -> Option<&Filter> {
        self.filter.as_ref()
    }

    /// The sessions shown in the sidebar, by index: while `/`'s filter is
    /// open, those that match it in every tab, or else all of the tab in
    /// front's.
    pub fn matches(&self) -> Vec<usize> {
        let Some(filter) = &self.filter else {
            return self.in_tab();
        };
        (0..self.sessions.len())
            .filter(|&index| self.session_found(filter, index).is_some())
            .collect()
    }

    /// Whether `filter` finds the session at `index`, and if it does, which
    /// letters of its name it matched: one with the status it keeps to,
    /// when it keeps to one, that matches what's typed, by itself, the
    /// name of its tab or the flow run it's a step of.
    fn session_found(&self, filter: &Filter, index: usize) -> Option<Vec<usize>> {
        let session = &self.sessions[index];
        if let Some(status) = filter.status
            && !status.keeps(Status::of(session))
        {
            return None;
        }
        let tab = self.tabs.tab_of(&session.name);
        let tab = tab.map(|tab| self.tabs.all()[tab].name.as_str());
        let flows = self.shown_flows();
        let run = groups::flow_step(session, flows).map(|(run, _)| &flows[run]);
        let around = Around {
            tab: tab.filter(|name| !name.is_empty()),
            run,
        };
        search::session_match(filter.input.text(), session, around)
    }

    /// What the bar of `/`'s filter can be on, in the sidebar's order.
    pub fn found(&self) -> Vec<Found> {
        self.rows()
            .iter()
            .filter_map(|row| self.found_at(row))
            .collect()
    }

    /// What picking `row` picks, while `/`'s filter is open: `None` for a
    /// heading, or a line that goes with the row above it.
    fn found_at(&self, row: &Row) -> Option<Found> {
        Some(match row {
            Row::Session(index) => Found::Session(self.sessions[*index].id.clone()),
            Row::NoSessions(path) => Found::Worktree(path.clone()),
            Row::Flow(run) => Found::Flow(self.shown_flows().get(*run)?.name.clone()),
            Row::PullRequest { project, number } => Found::PullRequest {
                project: project.clone(),
                number: *number,
            },
            _ => return None,
        })
    }

    /// The row `/`'s bar is on, by its place in [`App::rows`], while the
    /// filter is open and the bar is on something it found.
    pub fn filter_row(&self) -> Option<usize> {
        let highlighted = self.filter.as_ref()?.highlighted.as_ref()?;
        self.rows()
            .iter()
            .position(|row| self.found_at(row).as_ref() == Some(highlighted))
    }

    /// The name of the tab the session at `index` is in, while `/`'s filter
    /// shows it from a tab other than the one in front.
    pub fn elsewhere(&self, index: usize) -> Option<String> {
        self.filter.as_ref()?;
        self.tab_elsewhere(index)
    }

    /// The sessions in the tab in front, by index, in the sidebar's order.
    fn in_tab(&self) -> Vec<usize> {
        self.sessions_in(self.tabs.current_index())
    }

    /// The sessions in the tab at `tab`, by index, in the sidebar's order.
    fn sessions_in(&self, tab: usize) -> Vec<usize> {
        let tab = &self.tabs.all()[tab];
        (0..self.sessions.len())
            .filter(|&index| tab.holds(&self.sessions[index].name))
            .collect()
    }

    /// What most needs the user in the tab at `index`, for the tab bar to
    /// show: an agent waiting on them, then a finished turn nobody has
    /// looked at, then an agent working. Nothing waiting in another tab
    /// goes unseen.
    pub fn tab_status(&self, index: usize) -> Option<Status> {
        let statuses: Vec<Status> = self
            .sessions_in(index)
            .into_iter()
            .map(|session| Status::of(&self.sessions[session]))
            .collect();
        [Status::Waiting, Status::Done, Status::Working]
            .into_iter()
            .find(|wanted| statuses.contains(wanted))
    }

    /// Which letters of a session's name to mark, while the filter is open:
    /// those the query matched.
    pub fn marked_letters(&self, index: usize) -> Vec<usize> {
        let Some(filter) = &self.filter else {
            return Vec::new();
        };
        self.session_found(filter, index).unwrap_or_default()
    }

    /// The open pull request numbered `number` of the project at
    /// `project`, as its forge last listed them, and while `/`'s filter is
    /// open, which letters of its title to mark.
    pub fn found_pull_request(
        &self,
        project: &Path,
        number: u64,
    ) -> Option<(&PullRequest, Vec<usize>)> {
        let Some(Ok((_, pull_requests))) = self.pull_requests.get(project) else {
            return None;
        };
        let pull_request = pull_requests.iter().find(|pr| pr.number == number)?;
        let marked = self.filter.as_ref().and_then(|filter| {
            let name = self.project_name(project);
            search::pull_request_match(filter.input.text(), pull_request, &name)
        });
        Some((pull_request, marked.unwrap_or_default()))
    }

    /// The session the sidebar's bar is on: the one the filter's bar is on
    /// while it's open, or else the selected one.
    pub fn sidebar_cursor(&self) -> Option<usize> {
        match &self.filter {
            Some(filter) => match filter.highlighted.as_ref()? {
                Found::Session(id) => self.sessions.iter().position(|session| session.id == *id),
                _ => None,
            },
            None => self.selected_index(),
        }
    }

    /// The projects the sessions are in, by their main worktrees: the ones
    /// to ask their forge about.
    pub fn projects(&self) -> Vec<PathBuf> {
        let mut projects: Vec<PathBuf> = self
            .sessions
            .iter()
            .filter_map(|session| session.worktree.as_ref())
            .map(|worktree| worktree.project_path.clone())
            .collect();
        projects.sort();
        projects.dedup();
        projects
    }

    /// Takes what its forge said about the open pull requests of the
    /// project at `project`, asked at `asked`, for the worktree lines and
    /// the pull requests view, if it's open on that project; unless the
    /// list the TUI has was asked later.
    pub fn set_pull_requests(
        &mut self,
        project: PathBuf,
        found: Result<(Forge, Vec<PullRequest>), String>,
        asked: Instant,
    ) {
        if !latest_asked(&mut self.pull_requests_asked, &project, asked) {
            return;
        }
        if let Some(view) = &mut self.pull_requests_view
            && view.project == project
        {
            view.set_pull_requests(found.clone());
        }
        self.pull_requests.insert(project, found);
        self.keep_filter_bar_on_a_match();
    }

    /// The pull request for `branch` in the project at `project`, if its
    /// forge knows of one: the open one, or else one merged lately.
    pub fn pull_request(&self, project: &Path, branch: &str) -> Option<&PullRequest> {
        if !self.github_on {
            return None;
        }
        let Some(Ok((_, pull_requests))) = self.pull_requests.get(project) else {
            return None;
        };
        pull_requests
            .iter()
            .find(|pull_request| pull_request.local_branch == branch)
    }

    /// The forge the project at `project` is on, as far as the TUI knows:
    /// GitHub until it's been asked.
    fn forge_of(&self, project: &Path) -> Forge {
        match self.pull_requests.get(project) {
            Some(Ok((forge, _))) => *forge,
            _ => Forge::GitHub,
        }
    }

    /// The issues view, while it's open.
    pub fn issues_view(&self) -> Option<&IssuesView> {
        self.issues.as_ref()
    }

    /// The pull requests view, while it's open.
    pub fn pull_requests_view(&self) -> Option<&PullRequestsView> {
        self.pull_requests_view.as_ref()
    }

    /// The layouts view, while it's open.
    pub fn layouts_view(&self) -> Option<&LayoutsView> {
        self.layouts.as_ref()
    }

    /// The archive, while it's open.
    pub fn archived_view(&self) -> Option<&ArchivedView> {
        self.archived.as_ref()
    }

    /// Takes the archive as the daemon gave it, or why it couldn't: opens
    /// the view on it, or shows it there if it's open.
    pub fn show_archived(&mut self, found: Result<Vec<ArchivedSession>, String>) {
        match &mut self.archived {
            Some(view) => view.set_archived(found),
            None => self.archived = Some(ArchivedView::new(found)),
        }
    }

    /// The archived session called `name` has started again: the archive
    /// closes, and the selection goes to it.
    pub fn unarchived(&mut self, name: &str) {
        self.archived = None;
        self.select(name);
    }

    /// Takes the layouts as they were read, or why they couldn't be: opens
    /// the layouts view on them, or shows them in it if it's open, with the
    /// bar on `on`, if that's given.
    pub fn show_layouts(&mut self, found: Result<Layouts, String>, on: Option<&Which>) {
        match &mut self.layouts {
            Some(view) => view.set_layouts(found, on),
            None => {
                let running = self.sessions.iter().map(|s| s.name.clone()).collect();
                self.layouts = Some(LayoutsView::new(found, running));
            }
        }
    }

    /// Puts the tabs back the way the layout called `name` had them, and
    /// closes the layouts view: see [`Self::set_tabs`]. Says how many of
    /// the layout's sessions have gone since, which it leaves out.
    pub fn restore_layout(&mut self, tabs: Tabs, name: &str, started: usize) {
        let gone = tabs
            .sessions()
            .filter(|name| self.position(name).is_none())
            .count();
        self.layouts = None;
        self.set_tabs(tabs);
        let started = match started {
            0 => None,
            1 => Some("one of its sessions started again".to_string()),
            started => Some(format!("{started} of its sessions started again")),
        };
        let gone = match gone {
            0 => None,
            1 => Some("one of its sessions has gone".to_string()),
            gone => Some(format!("{gone} of its sessions have gone")),
        };
        let said: Vec<String> = started.into_iter().chain(gone).collect();
        let notice = match said.is_empty() {
            true => format!("restored {name}"),
            false => format!("restored {name}: {}", said.join(", ")),
        };
        self.notify(notice);
    }

    /// What starts each session again, by name, but the one this TUI runs
    /// in: those a layout saved now names, should they go.
    pub fn programs(&self) -> Programs {
        (self.sessions.iter())
            .filter(|session| !self.is_own(session))
            .filter_map(|session| Some((session.name.clone(), Program::of(session)?)))
            .collect()
    }

    /// Whether there's a session called `name`.
    pub fn has_session(&self, name: &str) -> bool {
        self.position(name).is_some()
    }

    /// The backlog view, while it's open.
    pub fn backlog_view(&self) -> Option<&BacklogView> {
        self.backlog.as_ref()
    }

    /// Takes the backlog the daemon sent for the project `dir` is in.
    pub fn set_backlog(&mut self, dir: &Path, found: Result<Backlog, String>) {
        if let Some(view) = self.backlog.as_mut().filter(|view| view.dir == dir) {
            view.set_backlog(found);
        }
    }

    /// Takes how many backlog items each project has to do.
    pub fn set_backlog_counts(&mut self, counts: HashMap<PathBuf, usize>) {
        self.backlog_counts = counts;
    }

    /// How many backlog items the project at `project` has to do, when
    /// there are some and the backlog is on.
    pub fn backlog_open(&self, project: &Path) -> Option<usize> {
        let open = *self.backlog_counts.get(project)?;
        (self.backlog_on && open > 0).then_some(open)
    }

    /// The session whose task `c` is closing, while the footer asks how it
    /// went.
    pub fn closing(&self) -> Option<&str> {
        self.closing.as_deref()
    }

    /// Takes the open issues its forge listed for the project at
    /// `project`, asked at `asked`, for the tab bar's count and the issues
    /// view, if it's open on that project; unless the list the TUI has was
    /// asked later. The titles changed here since it was asked stay.
    pub fn set_issues(
        &mut self,
        project: &Path,
        mut found: Result<(Forge, Vec<Issue>), String>,
        asked: Instant,
    ) {
        if !latest_asked(&mut self.issues_asked, project, asked) {
            return;
        }
        if let Ok((_, issues)) = &mut found {
            for issue in issues {
                if let Some(edit) = self.edit_since(project, issue.number, asked) {
                    issue.title = edit.title.clone();
                }
            }
        }
        if let Some(view) = self.issues.as_mut().filter(|view| view.project == project) {
            view.set_issues(found.clone());
        }
        self.open_issues.insert(project.to_path_buf(), found);
    }

    /// How many pull requests and issues are open on the forge of the
    /// selected session's project, for the tab bar: the pull requests
    /// without the drafts while they're hidden, and each once its forge
    /// has listed them.
    pub fn open_on_forge(&self) -> Option<OpenOnForge> {
        if !self.github_on {
            return None;
        }
        let project = &self.selected()?.worktree.as_ref()?.project_path;
        let mut forge = None;
        let pull_requests = match self.pull_requests.get(project) {
            Some(Ok((on, pull_requests))) => {
                forge = Some(*on);
                let open = pull_requests.iter().filter(|pr| !pr.merged);
                let listed = open.clone().count();
                let shown = open.filter(|pr| !(self.hide_draft_prs && pr.draft));
                Some(Counted::of(shown.count(), listed))
            }
            _ => None,
        };
        let issues = match self.open_issues.get(project) {
            Some(Ok((on, issues))) => {
                forge = forge.or(Some(*on));
                Some(Counted::of(issues.len(), issues.len()))
            }
            _ => None,
        };
        Some(OpenOnForge {
            forge: forge?,
            pull_requests,
            issues,
        })
    }

    /// Takes issue `number` of the project at `project`, read whole as
    /// asked at `asked`, with the text it was given here since.
    pub fn set_issue(
        &mut self,
        project: &Path,
        number: u64,
        mut read: Result<forge::IssueDetail, String>,
        asked: Instant,
    ) {
        if let (Ok(detail), Some(edit)) = (&mut read, self.edit_since(project, number, asked)) {
            detail.body = edit.body.clone();
        }
        if let Some(view) = self.issues.as_mut().filter(|view| view.project == project) {
            view.list.set_detail(number, read, asked);
        }
    }

    /// Takes pull request `number` of the project at `project`, read whole
    /// as asked at `asked`.
    pub fn set_pull_request(
        &mut self,
        project: &Path,
        number: u64,
        read: Result<PullRequestDetail, String>,
        asked: Instant,
    ) {
        let view = self.pull_requests_view.as_mut();
        if let Some(view) = view.filter(|view| view.project == project) {
            view.list.set_detail(number, read, asked);
        }
    }

    /// The title and text issue `number` of the project at `project` was
    /// given here, if the forge saved them after `asked`: what an answer
    /// to something asked then may not have yet.
    fn edit_since(&self, project: &Path, number: u64, asked: Instant) -> Option<&IssueEdit> {
        let edit = self.issue_edits.get(&(project.to_path_buf(), number))?;
        (asked < edit.saved).then_some(edit)
    }

    /// The issue or pull request to read whole next, with its project: the
    /// one the open view's bar is on, once.
    pub fn topic_to_read(&mut self) -> Option<(PathBuf, Topic)> {
        if let Some(view) = &mut self.issues
            && let Some(number) = view.list.detail_to_fetch()
        {
            return Some((view.project.clone(), Topic::Issue(number)));
        }
        let view = self.pull_requests_view.as_mut()?;
        let number = view.list.detail_to_fetch()?;
        Some((view.project.clone(), Topic::PullRequest(number)))
    }

    /// The comment on `topic`, of the project at `project`, was posted, or
    /// why it wasn't.
    pub fn commented(&mut self, project: &Path, topic: Topic, posted: Result<(), String>) {
        let notice = posted.is_ok().then(|| match topic {
            Topic::Issue(number) => format!("commented on #{number}"),
            Topic::PullRequest(number) => {
                format!("commented on {}", self.forge_of(project).label(number))
            }
        });
        match topic {
            Topic::Issue(number) => {
                if let Some(view) = self.issues.as_mut().filter(|view| view.project == project) {
                    view.commented(number, posted);
                }
            }
            Topic::PullRequest(number) => {
                let view = self.pull_requests_view.as_mut();
                if let Some(view) = view.filter(|view| view.project == project) {
                    view.commented(number, posted);
                }
            }
        }
        if let Some(notice) = notice {
            self.notify(notice);
        }
    }

    /// Issue `number` of the project at `project` was given this title and
    /// text, or why it wasn't, as the forge said `at`. Saved, they stay
    /// over what the forge answers to anything asked before.
    pub fn issue_edited(
        &mut self,
        project: &Path,
        number: u64,
        (title, body): (String, String),
        saved: Result<(), String>,
        at: Instant,
    ) {
        if saved.is_ok() {
            self.notify(format!("updated issue #{number}"));
            // The list the tab bar counts, which the view opens on next.
            if let Some(Ok((_, issues))) = self.open_issues.get_mut(project)
                && let Some(issue) = issues.iter_mut().find(|issue| issue.number == number)
            {
                issue.title = title.clone();
            }
            let edit = IssueEdit {
                title: title.clone(),
                body: body.clone(),
                saved: at,
            };
            let key = (project.to_path_buf(), number);
            self.issue_edits.insert(key, edit);
        }
        if let Some(view) = self.issues.as_mut().filter(|view| view.project == project) {
            view.edited(number, title, body, saved);
        }
    }

    /// The question on the footer line and its answer so far, while one
    /// is being answered.
    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    /// The yes-or-no question waiting on its answer, if there is one.
    pub fn confirm(&self) -> Option<&Confirm> {
        self.confirm.as_ref()
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The tabs, and which one is in front.
    pub fn tabs(&self) -> &Tabs {
        &self.tabs
    }

    /// The tabs as they're to be kept for the next time the TUI opens: the
    /// one in front on the session selected now.
    pub fn tabs_to_keep(&self) -> Tabs {
        let mut tabs = self.tabs.clone();
        tabs.current_mut().selected = self.selected_name();
        tabs
    }

    /// Takes the tabs kept from the last time the TUI ran, and selects the
    /// session the one in front was on. Sessions that have gone since leave
    /// their tabs, and those started since join the one in front.
    pub fn set_tabs(&mut self, tabs: Tabs) {
        self.tabs = tabs;
        self.place_sessions();
        self.arrive_at_tab();
        self.remember_shown();
    }

    /// The panes of the tab in front.
    pub fn panes(&self) -> &SplitTree {
        &self.tabs.current().panes
    }

    /// Takes the room beside the sidebar the panes share, as the event loop
    /// lays them out.
    pub fn set_tiles(&mut self, tiles: Rect) {
        self.tiles = tiles;
    }

    /// The names of the sessions the tab in front splits off, in the order
    /// their panes are drawn.
    pub fn splits(&self) -> Vec<&str> {
        self.tabs.current().splits()
    }

    /// The panes on screen, in the order they're drawn: the tab's panes,
    /// left to right and top to bottom. Zoomed, only the pane that shows
    /// the selected session. Then, last, over the others, the float, if
    /// there is one.
    pub fn slots(&self) -> Vec<Slot> {
        let mut slots = if self.zoomed() {
            let zoomed = self.selected_slot().filter(|slot| *slot != Slot::Float);
            vec![zoomed.unwrap_or(Slot::Selected)]
        } else {
            self.tiled()
        };
        if self.floating().is_some() {
            slots.push(Slot::Float);
        }
        slots
    }

    /// The session floating over the panes of the tab in front, if one is.
    pub fn floating(&self) -> Option<&SessionInfo> {
        let name = self.tabs.current().floating.as_deref()?;
        self.sessions.iter().find(|session| session.name == name)
    }

    /// Whether the session called `name` floats over the panes.
    fn is_floating(&self, name: &str) -> bool {
        self.tabs.current().floating.as_deref() == Some(name)
    }

    /// Every pane of the tab in front, in the order they're drawn when it
    /// isn't zoomed.
    fn tiled(&self) -> Vec<Slot> {
        let mut splits = 0;
        let panes = self.panes().panes().into_iter();
        panes
            .map(|pane| match pane {
                Pane::Selection => Slot::Selected,
                Pane::Session(_) => {
                    splits += 1;
                    Slot::Split(splits - 1)
                }
            })
            .collect()
    }

    /// The pane at `slot` in the tab's tree: the float isn't in it.
    fn pane_of(&self, slot: Slot) -> Option<Pane> {
        match slot {
            Slot::Selected => Some(Pane::Selection),
            Slot::Split(index) => Some(Pane::Session(self.splits().get(index)?.to_string())),
            Slot::Float => None,
        }
    }

    /// Where `pane` of the tab's tree is drawn.
    fn slot_of(&self, pane: &Pane) -> Option<Slot> {
        match pane {
            Pane::Selection => Some(Slot::Selected),
            Pane::Session(name) => {
                let splits = self.splits();
                splits
                    .iter()
                    .position(|split| split == name)
                    .map(Slot::Split)
            }
        }
    }

    /// Whether the tab in front is zoomed: the selected session's pane
    /// takes the room of the sidebar and the other panes.
    pub fn zoomed(&self) -> bool {
        self.tabs.current().zoomed
    }

    /// Whether the sidebar's keys are moving the selected session's pane's
    /// borders, in resize mode.
    pub fn resizing(&self) -> bool {
        self.resizing
    }

    /// The border the mouse is moving, while it is: see [`Hit::Border`].
    pub fn moving_border(&self) -> Option<usize> {
        self.border
    }

    /// The pane a drag of the mouse is selecting in, while it lasts.
    pub fn dragging(&self) -> Option<Slot> {
        self.dragging
    }

    /// The pane whose scrollbar's thumb the mouse holds, while it does.
    pub fn holding_thumb(&self) -> Option<Slot> {
        self.holding_thumb
    }

    /// The pane being moved by its header line, while the button is down.
    pub fn grabbed(&self) -> Option<Grab> {
        self.grabbed
    }

    /// Where on a pane's screen the mouse is with Ctrl held, to underline
    /// the link there.
    pub fn link_hover(&self) -> Option<(Slot, (u16, u16))> {
        self.link_hover
    }

    /// The mouse moved onto `hit`, with Ctrl held or not: on a pane's
    /// screen with Ctrl, the link there is to be underlined, and otherwise
    /// none is. Returns whether that changes what's drawn.
    pub fn mouse_moved(&mut self, hit: Hit, ctrl: bool) -> bool {
        let over = if ctrl { self.link_cell(hit) } else { None };
        let changed = over != self.link_hover;
        self.link_hover = over;
        changed
    }

    /// Where a Ctrl+click on `hit` looks for a link to open: a cell of a
    /// pane's screen, while nothing over the panes waits on the keyboard.
    pub fn link_cell(&self, hit: Hit) -> Option<(Slot, (u16, u16))> {
        match hit {
            Hit::Pane {
                slot,
                cell: Some(cell),
            } if self.mouse_on_panes() && self.shows_screen(slot) => Some((slot, cell)),
            _ => None,
        }
    }

    /// What a plugin opening a link in the pane at `slot` is told: the
    /// session the pane shows.
    pub fn link_context(&self, slot: Slot) -> plugins::Context {
        self.pane_session(slot)
            .map(plugins::Context::of_session)
            .unwrap_or_default()
    }

    /// The session the pane at `slot` is about: the one split off there,
    /// the one floating, or, in the pane that follows the selection, the
    /// selected one. While the selected session has a pane of its own,
    /// that pane goes on showing the session it showed before, if it can;
    /// if not, it's still about the selected one, to say where that is.
    pub fn pane_session(&self, slot: Slot) -> Option<&SessionInfo> {
        match slot {
            Slot::Selected => {
                let selected = self.selected()?;
                if self.has_own_pane(&selected.name) {
                    return self.shown().or(Some(selected));
                }
                Some(selected)
            }
            Slot::Split(index) => {
                let name = *self.splits().get(index)?;
                self.sessions.iter().find(|session| session.name == name)
            }
            Slot::Float => self.floating(),
        }
    }

    /// The session the selection's pane went on showing when the selection
    /// moved to one with a pane of its own, while it's still in the tab
    /// and has none itself.
    fn shown(&self) -> Option<&SessionInfo> {
        let tab = self.tabs.current();
        let name = tab.shown.as_deref()?;
        if !tab.holds(name) || self.has_own_pane(name) {
            return None;
        }
        self.sessions.iter().find(|session| session.name == name)
    }

    /// Notes the selected session as the one the selection's pane shows,
    /// while it has no pane of its own, so that the pane goes on showing it
    /// once the selection moves to one that does.
    fn remember_shown(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        if !self.has_own_pane(&name) {
            self.tabs.current_mut().shown = Some(name);
        }
    }

    /// Whether the pane at `slot` shows its session's screen. The pane that
    /// follows the selection doesn't when its session has a pane of its
    /// own, split off or floating, so that no session is drawn twice at two
    /// sizes, nor when it's the session this TUI runs in. Zoomed, no other
    /// pane is on screen but the float.
    pub fn shows_screen(&self, slot: Slot) -> bool {
        let Some(session) = self.pane_session(slot) else {
            return false;
        };
        if self.zoomed() && slot != Slot::Float && Some(slot) != self.selected_slot() {
            return false;
        }
        match slot {
            Slot::Selected => !self.is_own(session) && !self.has_own_pane(&session.name),
            Slot::Split(_) | Slot::Float => true,
        }
    }

    /// Whether the session called `name` is split off into a pane of its
    /// own.
    pub fn is_split(&self, name: &str) -> bool {
        self.tabs.current().is_split(name)
    }

    /// Whether the session called `name` has a pane of its own: split off,
    /// or floating.
    fn has_own_pane(&self, name: &str) -> bool {
        self.is_split(name) || self.is_floating(name)
    }

    /// The index of the selected session, or `None` when the tab in front
    /// has none, or the selection is on a worktree with no sessions.
    pub fn selected_index(&self) -> Option<usize> {
        if self.on_worktree.is_some() {
            return None;
        }
        let session = self.sessions.get(self.selected)?;
        let in_tab = self.tabs.current().holds(&session.name);
        in_tab.then_some(self.selected)
    }

    pub fn selected(&self) -> Option<&SessionInfo> {
        self.sessions.get(self.selected_index()?)
    }

    fn selected_name(&self) -> Option<String> {
        self.selected().map(|session| session.name.clone())
    }

    /// Whether the selected session is the one this TUI runs in.
    pub fn selected_is_own(&self) -> bool {
        self.selected()
            .is_some_and(|selected| self.is_own(selected))
    }

    /// Whether `session` is the one this TUI runs in. Ids tell, since the
    /// session may have been renamed since the TUI started.
    pub fn is_own(&self, session: &SessionInfo) -> bool {
        self.own_id.as_ref() == Some(&session.id)
    }

    /// Takes a fresh list from the daemon and puts it in the sidebar's
    /// order, each session in its tab. The selected session stays selected
    /// wherever it moved to. If it's gone, and it was the last in its
    /// worktree, the selection goes to the row that worktree is left with;
    /// otherwise to the next session in the tab, or the last.
    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>) {
        let mut copied_unseen = None;
        // A session renamed from elsewhere, by `crystal rename` or from its
        // first prompt, keeps its tab, its pane and the selection.
        for now in &sessions {
            let was = self.sessions.iter().find(|was| was.id == now.id);
            if let Some(was) = was.filter(|was| was.name != now.name) {
                self.tabs.renamed(&was.name, &now.name);
            }
            // A session that started, or whose agent stopped or started
            // doing something, may have changed files: its worktree is
            // counted again.
            let changed = was.is_none_or(|was| was.changed != now.changed);
            if changed && let Some(worktree) = &now.worktree {
                self.stats_due.insert(worktree.path.clone());
            }
            // Its program copied something while nobody watched, which
            // nobody put on the clipboard: the user is told, to copy it
            // again where they can see it. A daemon handed over counts from
            // nothing again.
            let copied = was.is_some_and(|was| was.unseen_copies != now.unseen_copies);
            if copied && now.unseen_copies > 0 {
                copied_unseen = Some(now.name.clone());
            }
        }
        if let Some(name) = copied_unseen {
            self.notify(format!(
                "{name} copied out of sight: not put on your clipboard"
            ));
        }
        if let Some(restarted) = self.restarts.take(&sessions) {
            self.restarted = Some(restarted);
        }
        let before = self.sessions.get(self.selected).cloned();
        let on_a_session = self.on_worktree.is_none();
        self.sessions = groups::order(sessions, self.shown_flows());
        let still_there = before
            .as_ref()
            .and_then(|s| self.sessions.iter().position(|now| now.id == s.id));
        if let Some(index) = still_there {
            self.selected = index;
        }
        self.close_splits_of_gone_sessions();
        self.place_sessions();
        self.keep_selection_in_tab();
        self.know_sessions_worktrees();
        if on_a_session
            && still_there.is_none()
            && let Some(worktree) = before.and_then(|session| session.worktree)
            && self
                .all_rows()
                .contains(&Row::NoSessions(worktree.path.clone()))
        {
            self.on_worktree = Some(worktree.path);
        }
        self.keep_selection_on_a_row();
        let keeps_keyboard = match self.focus {
            Focus::Sidebar => true,
            Focus::Pane(slot) => self.can_type_into(slot),
            // An ended session's last screen can still be copied from.
            Focus::Copy(slot) => self.shows_screen(slot),
        };
        if !keeps_keyboard {
            self.focus = Focus::Sidebar;
        }
        if self.needs_you.is_some() {
            let rows = self.needs_you_rows();
            if let Some(view) = &mut self.needs_you {
                view.refresh(rows);
            }
        }
        self.keep_filter_bar_on_a_match();
        self.remember_shown();
    }

    /// Closes the panes of the tab in front whose sessions have gone, the
    /// keyboard moving with its pane. The other tabs' go as
    /// [`Self::place_sessions`] puts their sessions right.
    fn close_splits_of_gone_sessions(&mut self) {
        let there: Vec<String> = self.sessions.iter().map(|s| s.name.clone()).collect();
        self.change_panes(|panes| panes.retain(|name| there.iter().any(|held| held == name)));
    }

    /// Puts every session in a tab: those that have gone leave theirs, and
    /// those no tab holds yet join one. A step of a flow run joins the tab
    /// its run's other steps are in, so a run stays together; any other
    /// session joins the tab in front, which is where a session started
    /// from this TUI was started.
    fn place_sessions(&mut self) {
        let homes: HashMap<String, usize> = self
            .sessions
            .iter()
            .filter(|session| self.tabs.tab_of(&session.name).is_none())
            .filter_map(|session| Some((session.name.clone(), self.flow_tab(session)?)))
            .collect();
        let names: Vec<&str> = self.sessions.iter().map(|s| s.name.as_str()).collect();
        self.tabs.take_in(&names, |name| homes.get(name).copied());
    }

    /// The tab holding the other steps of the flow run `session` is a step
    /// of, if it's one and they're in a tab.
    fn flow_tab(&self, session: &SessionInfo) -> Option<usize> {
        let (run, _) = groups::flow_step(session, self.shown_flows())?;
        let steps = self.flows[run].steps.iter();
        steps
            .filter_map(|step| step.session.as_deref())
            .find_map(|name| self.tabs.tab_of(name))
    }

    /// Keeps the selection on a session in the tab in front: when the one
    /// it was on has gone, or left the tab, it goes to the next one down
    /// the sidebar, or else the last. In an empty tab, nothing is selected.
    fn keep_selection_in_tab(&mut self) {
        let in_tab = self.in_tab();
        if in_tab.contains(&self.selected) {
            return;
        }
        let next = in_tab.iter().find(|&&index| index > self.selected);
        if let Some(&index) = next.or(in_tab.last()) {
            self.selected = index;
        }
    }

    /// Selects the session called `name`, if there is one, bringing its tab
    /// to the front when it's in another.
    pub fn select(&mut self, name: &str) {
        let Some(index) = self.position(name) else {
            return;
        };
        if let Some(tab) = self.tabs.tab_of(name) {
            self.go_to_tab(tab);
        }
        self.selected = index;
        self.on_worktree = None;
        // Picked by name, it's shown: a folded project it's in unfolds.
        if let Some(worktree) = &self.sessions[index].worktree {
            self.folded.remove(&worktree.project_path);
        }
        self.remember_shown();
    }

    /// The session called `from` is called `to` now: a split of it stays
    /// open under its new name, and a tab that was on it stays on it.
    pub fn renamed(&mut self, from: &str, to: &str) {
        self.tabs.renamed(from, to);
    }

    /// Hands the keyboard to the selected session, in whichever pane shows
    /// it, if it can take keys.
    pub fn type_into_selected(&mut self) {
        let Some(slot) = self.selected_slot() else {
            return;
        };
        if self.can_type_into(slot) {
            self.focus_pane(slot);
        }
    }

    /// The pane that shows the selected session: the float, if it floats,
    /// its split, if it has one, or else the pane that follows the
    /// selection.
    fn selected_slot(&self) -> Option<Slot> {
        let selected = self.selected()?;
        if self.is_floating(&selected.name) {
            return Some(Slot::Float);
        }
        let split = self
            .splits()
            .iter()
            .position(|split| *split == selected.name);
        Some(split.map_or(Slot::Selected, Slot::Split))
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        let action = self.take_key(key);
        self.remember_shown();
        action
    }

    fn take_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.notice = None;
        self.away_shown = false;
        self.restarted = None;
        // The prefix is for the very next key, wherever it goes, and so is
        // a plugin's first key.
        let prefixed = std::mem::take(&mut self.prefixed);
        let pending = self.pending.take();
        // A plugin's pane is over everything, and has every key but the one
        // that closes it.
        if self.plugin_pane.is_some() {
            if self.keymap.is_hand_back(&key) {
                return Some(Action::ClosePluginPane);
            }
            return Some(Action::TypeInPluginPane(key));
        }
        // The keys the user gave the views stand for the views' own.
        let key = self.as_views_take(key)?;
        // An open view has every key until it's closed.
        if self.view.is_some() {
            return self.on_view_key(key);
        }
        if let Some(menu) = &mut self.menu {
            let step = menu.on_key(&key);
            return self.follow_menu(step);
        }
        // The arrows turn the list of keys' pages. Any other key closes it,
        // and does nothing else: the key that closes it may be one the user
        // was only reading about.
        if let Some(page) = self.keys_page {
            let pages = self.keys_pages();
            self.keys_page = match page_turn(key.code) {
                Some(by) if pages > 1 => Some((page + pages).wrapping_add_signed(by) % pages),
                _ => None,
            };
            return None;
        }
        // Only `y` says yes; any other key says no.
        if let Some(confirm) = self.confirm.take() {
            if key.code != KeyCode::Char('y') {
                return None;
            }
            // The tab is the TUI's to close; its sessions, the daemon's to
            // kill.
            if matches!(confirm, Confirm::CloseTab { .. }) {
                self.close_tab_in_front();
            }
            // The daemon removes a worktree; its line says so until the
            // daemon says it's done.
            if let Confirm::RemoveWorktree { path, .. } | Confirm::RemoveEmptied { path, .. } =
                &confirm
            {
                self.removing.insert(path.clone());
            }
            // Killing the last session in a worktree leaves it with nothing
            // in it: the next question is whether it goes too.
            if let Confirm::Kill(name) = &confirm {
                self.confirm = self.emptied_by_killing(name);
            }
            return Some(confirm.action());
        }
        // A digit or `t` says which tab the session goes to; any other key
        // leaves it where it is.
        if let Some(name) = self.moving.take() {
            self.move_to_tab(&name, key.code);
            return None;
        }
        // `d` says the task was done, `f` that it failed; any other key
        // leaves it open.
        if let Some(name) = self.closing.take() {
            let failed = match key.code {
                KeyCode::Char('d') => false,
                KeyCode::Char('f') => true,
                _ => return None,
            };
            self.ask(Question::CloseTask { name, failed }, "");
            return None;
        }
        if self.reply.is_some() {
            return self.on_reply_key(key);
        }
        if self.command_list.is_some() {
            return self.on_command_list_key(key);
        }
        if self.backlog.is_some() {
            return self.on_backlog_key(key);
        }
        if self.layouts.is_some() {
            return self.on_layouts_key(key);
        }
        if self.archived.is_some() {
            return self.on_archived_key(key);
        }
        if self.launcher.is_some() {
            return self.on_launcher_key(key);
        }
        if self.profiles_view.is_some() {
            return self.on_profiles_key(key);
        }
        if self.plugins_view.is_some() {
            return self.on_plugins_key(key);
        }
        if self.settings.is_some() {
            return self.on_settings_key(key);
        }
        if self.prompt.is_some() {
            return self.on_prompt_key(key);
        }
        if self.needs_you.is_some() {
            return self.on_needs_you_key(key);
        }
        if self.timeline.is_some() {
            return self.on_timeline_key(key);
        }
        if self.issues.is_some() {
            return self.on_issues_key(key);
        }
        if self.pull_requests_view.is_some() {
            return self.on_pull_requests_key(key);
        }
        if self.filter.is_some() {
            return self.on_filter_key(key);
        }
        if self.resizing {
            self.on_resize_key(key);
            return None;
        }
        if let Some(first) = pending {
            return self.second_key(first, key);
        }
        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(key),
            Focus::Pane(slot) if prefixed => self.after_prefix(slot, key),
            Focus::Pane(slot) => self.on_pane_key(slot, key),
            Focus::Copy(slot) => self.on_copy_key(slot, key),
        }
    }

    /// What the mouse does, when no program in a pane has taken it: a click
    /// selects a session or hands a pane the keyboard, a drag moves a
    /// border between panes, selects in a pane or moves its scrollbar's
    /// thumb, and the wheel moves the selection, or scrolls a pane through
    /// its history.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Option<Action> {
        let action = self.take_mouse(kind, hit);
        self.remember_shown();
        action
    }

    fn take_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Option<Action> {
        if let Some(view) = &mut self.view {
            let outcome = match view {
                View::Diff(diff) => diff.on_mouse(kind, hit),
                View::Files(finder) => finder.on_mouse(kind, hit),
                View::Tree(tree) => tree.on_mouse(kind, hit),
                View::Grep(grep) => grep.on_mouse(kind, hit),
                View::Branches(switcher) => switcher.on_mouse(kind, hit),
                View::Memory(memory) => memory.on_mouse(kind, hit),
            };
            return self.follow(outcome);
        }
        // A click closes the list of keys, like a key does.
        if self.showing_keys() {
            if kind == MouseEventKind::Down(MouseButton::Left) {
                self.keys_page = None;
            }
            return None;
        }
        let click = kind == MouseEventKind::Down(MouseButton::Left);
        // `/`'s filter waits on the keyboard, but a click on a match picks it.
        if self.filter.is_some()
            && click
            && let Hit::SidebarRow(row) = hit
        {
            return self.click_found(row);
        }
        if self.waiting_on_keyboard() {
            return None;
        }
        if click {
            self.notice = None;
            self.resizing = false;
        }
        // The sidebar's edge follows the mouse until the button comes up.
        if self.dragging_sidebar {
            match (kind, hit) {
                (MouseEventKind::Drag(_), Hit::SidebarEdge(column)) => {
                    self.drag_sidebar_to(column);
                    return None;
                }
                (MouseEventKind::Down(_), _) => self.dragging_sidebar = false,
                (MouseEventKind::Up(_), _) => {
                    self.dragging_sidebar = false;
                    return None;
                }
                _ => return None,
            }
        }
        // A border taken by the mouse follows it until the button comes up.
        if let Some(split) = self.border {
            match (kind, hit) {
                (MouseEventKind::Drag(_), Hit::Border { split: moved, at }) if moved == split => {
                    let tiles = self.tiles;
                    self.tabs.current_mut().panes.drag(split, at, tiles);
                    return None;
                }
                (MouseEventKind::Up(_), _) => {
                    self.border = None;
                    return None;
                }
                (MouseEventKind::Down(_), _) => self.border = None,
                _ => return None,
            }
        }
        // A drag selects in the pane it started in until the button comes
        // up, wherever it goes meanwhile.
        if let Some(slot) = self.dragging {
            match (kind, hit) {
                (MouseEventKind::Drag(MouseButton::Left), Hit::Pane { slot: at, cell })
                    if at == slot =>
                {
                    return cell.map(|cell| Action::SelectTo { slot, cell });
                }
                (MouseEventKind::Up(_), _) => {
                    self.dragging = None;
                    if self.mouse.copy_on_select {
                        return Some(Action::CopySelection(slot));
                    }
                    return Some(Action::HoldSelection(slot));
                }
                // The wheel scrolls the pane meanwhile, and the selection
                // goes on with it.
                (MouseEventKind::ScrollUp, _) => return Some(Action::ScrollBack(slot)),
                (MouseEventKind::ScrollDown, _) => return Some(Action::ScrollForward(slot)),
                // The button came up somewhere nothing heard it: this is a
                // new click.
                (MouseEventKind::Down(_), _) => self.dragging = None,
                _ => return None,
            }
        }
        // A scrollbar's thumb taken by the mouse follows it until the
        // button comes up.
        if let Some(slot) = self.holding_thumb {
            match (kind, hit) {
                (MouseEventKind::Drag(_), Hit::Scrollbar { slot: at, row }) if at == slot => {
                    return Some(Action::DragThumb { slot, row });
                }
                (MouseEventKind::Up(_), _) => {
                    self.holding_thumb = None;
                    return None;
                }
                (MouseEventKind::Down(_), _) => self.holding_thumb = None,
                _ => return None,
            }
        }
        // A pane taken by its header goes where the button comes up, over
        // another pane, and swaps places with it.
        if let Some(grab) = self.grabbed {
            match (kind, hit) {
                (MouseEventKind::Drag(_), Hit::Pane { slot, .. }) if slot != Slot::Float => {
                    self.grabbed = Some(Grab {
                        over: Some(slot),
                        ..grab
                    });
                    return None;
                }
                (MouseEventKind::Drag(_), _) => {
                    self.grabbed = Some(Grab { over: None, ..grab });
                    return None;
                }
                (MouseEventKind::Up(_), hit) => {
                    self.grabbed = None;
                    if let Hit::Pane { slot, .. } = hit
                        && slot != grab.from
                    {
                        self.swap_panes(grab.from, slot);
                    }
                    return None;
                }
                (MouseEventKind::Down(_), _) => self.grabbed = None,
                _ => return None,
            }
        }
        match (kind, hit) {
            (_, Hit::Tab(index)) if click => self.go_to_tab(index),
            (_, Hit::SidebarRow(row)) if click => self.click_row(row),
            // Anywhere else in the sidebar, the click only takes the keyboard.
            (_, Hit::Sidebar) if click => self.focus = Focus::Sidebar,
            (_, Hit::SidebarEdge(_)) if click => self.dragging_sidebar = true,
            (_, Hit::Border { split, .. }) if click && !self.zoomed() => self.border = Some(split),
            (_, Hit::Pane { slot, cell }) if click => {
                // A click hands a pane the keyboard, but copy mode keeps it.
                if self.focus != Focus::Copy(slot) && self.can_type_into(slot) {
                    self.focus_pane(slot);
                }
                // On its header line, it takes the pane, to move it.
                let tiled = slot != Slot::Float;
                if tiled && cell.is_none() && !self.zoomed() && self.tiled().len() > 1 {
                    self.grabbed = Some(Grab {
                        from: slot,
                        over: Some(slot),
                    });
                }
                if let Some(cell) = cell
                    && self.shows_screen(slot)
                {
                    self.dragging = Some(slot);
                    return Some(Action::SelectFrom { slot, cell });
                }
            }
            // A scrollbar takes the click as it is, the keyboard staying
            // where it was.
            (_, Hit::Scrollbar { slot, row }) if click && self.shows_screen(slot) => {
                self.holding_thumb = Some(slot);
                return Some(Action::GrabThumb { slot, row });
            }
            (MouseEventKind::ScrollUp, Hit::SidebarRow(_) | Hit::Sidebar) => {
                self.move_selection(-1);
            }
            (MouseEventKind::ScrollDown, Hit::SidebarRow(_) | Hit::Sidebar) => {
                self.move_selection(1);
            }
            (MouseEventKind::ScrollUp, Hit::Pane { slot, .. } | Hit::Scrollbar { slot, .. }) => {
                return Some(Action::ScrollBack(slot));
            }
            (MouseEventKind::ScrollDown, Hit::Pane { slot, .. } | Hit::Scrollbar { slot, .. }) => {
                return Some(Action::ScrollForward(slot));
            }
            _ => {}
        }
        None
    }

    /// The menu a right click opened, while it's open.
    pub fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }

    /// Opens the menu for what the right click at the screen's `at` was on:
    /// a session, a worktree or a project in the sidebar, which it selects
    /// first, a tab, which it goes to, or a pane, whose session it selects.
    /// Not while something else waits on the keyboard.
    pub fn right_click(&mut self, hit: Hit, at: (u16, u16)) -> Option<Action> {
        self.menu = None;
        if self.view.is_some() || self.showing_keys() || self.waiting_on_keyboard() {
            return None;
        }
        let items = match hit {
            Hit::SidebarRow(row) => self.sidebar_menu(row)?,
            Hit::Tab(index) => {
                self.go_to_tab(index);
                tab_menu()
            }
            Hit::Pane { slot, .. } | Hit::Scrollbar { slot, .. } => {
                let name = self.pane_session(slot)?.name.clone();
                self.select(&name);
                self.session_menu(true)?
            }
            _ => return None,
        };
        self.notice = None;
        self.resizing = false;
        if matches!(self.focus, Focus::Copy(_)) {
            self.stop_copying();
        }
        self.focus = Focus::Sidebar;
        self.menu = Some(Menu::new(at, items, &self.keymap));
        self.remember_shown();
        None
    }

    /// What the mouse does while the menu is open, at the screen's `column`
    /// and `row`: moving over an item puts the bar on it, a click on one
    /// chooses it, and a click anywhere else closes the menu.
    pub fn menu_mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) -> Option<Action> {
        let screen = self.screen;
        let menu = self.menu.as_mut()?;
        let on = menu.item_at(screen, column, row);
        match kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                if let Some(index) = on {
                    menu.highlight(index);
                }
                None
            }
            MouseEventKind::Down(MouseButton::Left) => match on {
                Some(index) => {
                    let step = menu.choose(index);
                    self.follow_menu(step)
                }
                // On its frame, the click does nothing.
                None if menu.covers(screen, column, row) => None,
                None => {
                    self.menu = None;
                    None
                }
            },
            MouseEventKind::Down(_) => {
                self.menu = None;
                None
            }
            _ => None,
        }
    }

    /// Carries out what the menu's key or click asked for.
    fn follow_menu(&mut self, step: menu::Step) -> Option<Action> {
        match step {
            menu::Step::Stay => None,
            menu::Step::Close => {
                self.menu = None;
                None
            }
            menu::Step::Run(command) => {
                self.menu = None;
                let action = self.run(command);
                self.remember_shown();
                action
            }
        }
    }

    /// The menu for the sidebar row at `row`, once the selection is on it:
    /// a session's, a worktree's with no sessions, or for a heading, its
    /// first session's or empty worktree's, with what the heading is for.
    fn sidebar_menu(&mut self, row: usize) -> Option<Vec<Item>> {
        let rows = self.rows();
        let clicked = rows.get(row)?.clone();
        // A heading selects the first row it heads that the selection can
        // be on.
        let first_under = |ends: fn(&Row) -> bool| {
            rows[row + 1..]
                .iter()
                .take_while(|row| !ends(row))
                .find(|row| matches!(row, Row::Session(_) | Row::NoSessions(_)))
                .cloned()
        };
        match &clicked {
            Row::Session(_) | Row::Task(_) | Row::Line(_) | Row::NoSessions(_) => {
                self.select_row(&clicked)
            }
            Row::Project { path, .. } if self.is_folded(path) => {
                self.select_row(&clicked);
                return Some(self.project_menu());
            }
            Row::Project { .. } => {
                self.select_row(&first_under(|row| {
                    matches!(row, Row::Project { .. } | Row::OutsideGit)
                })?);
                return Some(self.project_menu());
            }
            Row::Worktree { .. } | Row::Directory(_) => self.select_row(&first_under(|row| {
                matches!(
                    row,
                    Row::Project { .. }
                        | Row::OutsideGit
                        | Row::Worktree { .. }
                        | Row::Directory(_)
                        | Row::Flow(_)
                )
            })?),
            // A pinned session is selected where it is, in whichever tab.
            Row::Pinned(index) => {
                let name = self.sessions[*index].name.clone();
                self.select(&name);
            }
            Row::OutsideGit
            | Row::Terminals
            | Row::Flow(_)
            | Row::Step { .. }
            | Row::NeedsYou(_)
            | Row::PullRequest { .. } => return None,
        }
        self.focus = Focus::Sidebar;
        if self.on_worktree.is_some() {
            return Some(self.empty_worktree_menu());
        }
        self.session_menu(false)
    }

    /// The menu for the selected session; `on_pane` when it was opened on
    /// its pane, which offers the pane's keys too.
    fn session_menu(&self, on_pane: bool) -> Option<Vec<Item>> {
        let session = self.selected()?;
        let running = session.state == State::Running;
        let mut items = vec![if running {
            Item::new("type into it", Command::Open)
        } else {
            Item::new("start it again", Command::Open)
        }];
        items.push(if self.is_split(&session.name) {
            Item::new("close its split", Command::ToggleSplit)
        } else {
            Item::new("split it off", Command::ToggleSplit)
        });
        if on_pane {
            items.push(Item::new("split side by side", Command::SplitRight));
            items.push(Item::new("split below", Command::SplitDown));
        }
        items.push(if self.is_floating(&session.name) {
            Item::new("put it back", Command::Float)
        } else {
            Item::new("float it", Command::Float)
        });
        items.push(Item::new("zoom", Command::Zoom));
        if on_pane {
            items.push(Item::new("copy mode", Command::Copy));
            items.push(Item::new("edit its history", Command::EditHistory));
        }
        items.push(Item::new("rename", Command::Rename));
        items.push(Item::new("move to another tab", Command::MoveToTab));
        if session.worktree.is_some() {
            items.push(Item::new("what changed", Command::Diff));
            items.push(Item::new("find a file", Command::FindFile));
            items.push(Item::new("browse its files", Command::FileTree));
            items.push(Item::new(
                "run the project, or stop it",
                Command::RunProject,
            ));
            items.push(Item::new("open the project", Command::OpenProject));
            if self.github_on() {
                items.push(Item::new("its pull request", Command::PullRequest));
            }
        }
        if self.tasks_on && session.task.as_ref().is_some_and(|task| task.is_open()) {
            items.push(Item::new("close its task", Command::CloseTask));
        }
        if session.front == Some(Front::Task) {
            items.push(Item::new("open it in a terminal", Command::TaskToTerminal));
        }
        items.push(Item::new("archive it", Command::Archive));
        items.push(Item::danger("kill it", Command::Kill));
        Some(items)
    }

    /// The menu for the worktree with no sessions the selection is on.
    fn empty_worktree_menu(&self) -> Vec<Item> {
        let main = self.selected_empty_worktree().is_some_and(|w| w.main);
        let mut items = vec![
            Item::new("start a session here", Command::NewSession),
            Item::new("new worktree", Command::NewWorktree),
            Item::new("run the project", Command::RunProject),
            Item::new("open the project", Command::OpenProject),
            Item::new("what changed", Command::Diff),
            Item::new("find a file", Command::FindFile),
            Item::new("browse its files", Command::FileTree),
        ];
        items.push(if main {
            Item::danger("take the project off the list", Command::RemoveWorktree)
        } else {
            Item::danger("remove the worktree", Command::RemoveWorktree)
        });
        items
    }

    /// The menu for a project's heading.
    fn project_menu(&self) -> Vec<Item> {
        let mut items = vec![
            Item::new("new session", Command::NewSession),
            Item::new("new worktree", Command::NewWorktree),
            Item::new("run the project, or stop it", Command::RunProject),
            Item::new("open the project", Command::OpenProject),
        ];
        if self.github_on() {
            items.push(Item::new("pull requests", Command::PullRequests));
            items.push(Item::new("issues", Command::Issues));
        }
        if self.backlog_on {
            items.push(Item::new("the backlog", Command::Backlog));
        }
        if self.memory_on {
            items.push(Item::new("what it remembers", Command::Memory));
        }
        items.push(match self.folded_selection() {
            Some(_) => Item::new("unfold it", Command::UnfoldProject),
            None => Item::new("fold it to its heading", Command::FoldProject),
        });
        items
    }

    /// Whether something open waits for the keyboard, and the mouse does
    /// nothing meanwhile: a question on the footer waits for its answer,
    /// and so do the filter, the issues and backlog views, the new-session
    /// panel, the profiles view and the others over the panes.
    fn waiting_on_keyboard(&self) -> bool {
        let typing = self.filter.is_some()
            || self.command_list.is_some()
            || self.timeline.is_some()
            || self.needs_you.is_some()
            || self.issues.is_some()
            || self.pull_requests_view.is_some()
            || self.backlog.is_some()
            || self.layouts.is_some()
            || self.archived.is_some()
            || self.launcher.is_some()
            || self.reply.is_some()
            || self.profiles_view.is_some()
            || self.plugins_view.is_some()
            || self.settings.is_some()
            || self.plugin_pane.is_some();
        let asking = self.prompt.is_some()
            || self.confirm.is_some()
            || self.closing.is_some()
            || self.moving.is_some();
        typing || asking
    }

    /// Whether the mouse works on the panes: they're showing, and nothing
    /// over them waits on the keyboard.
    fn mouse_on_panes(&self) -> bool {
        self.view.is_none() && !self.showing_keys() && !self.waiting_on_keyboard()
    }

    /// A click on a sidebar row while `/`'s filter is open picks what's
    /// there, as Enter would, wherever it is; a heading, or any other row,
    /// does nothing.
    fn click_found(&mut self, row: usize) -> Option<Action> {
        self.focus = Focus::Sidebar;
        let found = match self.rows().get(row)? {
            Row::Task(index) | Row::Line(index) => self.found_at(&Row::Session(*index)),
            row => self.found_at(row),
        };
        self.pick(found?)
    }

    /// A click on a sidebar row gives the sidebar the keyboard, and on a
    /// session, or a worktree with none, selects it. A project's heading
    /// folds or unfolds the project; another heading leaves the selection
    /// where it was.
    fn click_row(&mut self, row: usize) {
        self.focus = Focus::Sidebar;
        let rows = self.rows();
        // A pinned session is selected where it is, in whichever tab.
        if let Some(Row::Pinned(index)) = rows.get(row) {
            let name = self.sessions[*index].name.clone();
            self.select(&name);
            return;
        }
        // A project's heading folds the project, or unfolds it.
        if let Some(Row::Project { path, .. }) = rows.get(row) {
            self.toggle_fold(path);
            return;
        }
        if let Some(row) = rows.get(row)
            && matches!(
                row,
                Row::Session(_) | Row::Task(_) | Row::Line(_) | Row::NoSessions(_)
            )
        {
            self.select_row(row);
        }
    }

    fn on_sidebar_key(&mut self, key: KeyEvent) -> Option<Action> {
        // The prefix does nothing here, where every key is a command's
        // already: the key after it does what it always does.
        if self.keymap.is_prefix(&key) {
            return None;
        }
        // On a background task asking for a permission, the answer keys,
        // `y`, `n` and `Y`, answer it; `n` is a new session again once it's
        // answered.
        let answer = self.keymap.mode_key(Mode::Answer, &key).map(answer_of);
        if let Some(answer) = answer
            && let Some(name) = self.selected().filter(|s| s.asking.is_some())
        {
            let name = name.name.clone();
            return Some(Action::Answer { name, answer });
        }
        if let Some(bound) = self.keymap.bound(&key) {
            return self.run_bound(bound);
        }
        if let Some(ran) = self.plugin_key(&key) {
            return ran;
        }
        if answer.is_some() {
            let not_asking = |s: &SessionInfo| format!("{} isn't asking for anything", s.name);
            let notice = self
                .selected()
                .map_or_else(|| "there's no session selected".into(), not_asking);
            self.notify(notice);
        }
        None
    }

    /// Runs what a key is bound to: one of crystal's commands, or one of
    /// the user's own.
    fn run_bound(&mut self, bound: Bound) -> Option<Action> {
        match bound {
            Bound::Command(command) => self.run(command),
            Bound::Custom(index) => {
                let command = self.keymap.custom().get(index)?.0.clone();
                self.run_key_command(command)
            }
        }
    }

    /// Runs one of the user's `[[keys.command]]`s, about the selected
    /// session: a plugin's action, as its own key would; for a pane or a
    /// tab, it's split off the selected session's pane or made first, for
    /// the session that runs the command to go to; a popup's and one in the
    /// background, the event loop starts as they are.
    fn run_key_command(&mut self, command: KeyCommand) -> Option<Action> {
        let context = self.selected_context();
        if command.kind == CommandKind::Plugin {
            let (plugin, action) = command.plugin_action()?;
            let (plugin, action) = (plugin.to_string(), action.to_string());
            return Some(Action::RunPlugin {
                plugin,
                action,
                context,
            });
        }
        let followed = self.selected().map(|session| session.cwd.clone());
        let dir = match command.kind {
            CommandKind::Tab => {
                let index = self.tabs.add();
                self.go_to_tab(index);
                self.start_dir.clone().or(followed)
            }
            CommandKind::Pane => {
                let way = match command.split {
                    Some(SplitWay::Right) => Way::Right,
                    Some(SplitWay::Down) => Way::Down,
                    None => self.way_to_split(),
                };
                self.split_pane(way, HALF);
                followed
            }
            _ => followed,
        };
        Some(Action::RunKeyCommand {
            command: Box::new(command),
            dir,
            context,
        })
    }

    /// Runs `command`, as its key does in the sidebar: from the sidebar,
    /// from the `:` list, or after the prefix in a pane.
    fn run(&mut self, command: Command) -> Option<Action> {
        match command {
            Command::Down => self.move_selection(1),
            Command::Up => self.move_selection(-1),
            // On a worktree with no sessions, there's nothing to type into:
            // Enter starts something there, as `n` does.
            Command::Open if self.folded_selection().is_some() => self.unfold_project(),
            Command::Open if self.on_worktree.is_some() => return self.open_launcher(false),
            Command::Open => self.enter(),
            Command::Reply if self.on_worktree.is_some() => {}
            Command::Reply => match self.selected_name() {
                Some(name) => self.open_reply(&name),
                None => self.notify("there's no session selected".into()),
            },
            Command::NextPane => self.move_to_pane(Round::Forward),
            Command::PreviousPane => self.move_to_pane(Round::Back),
            Command::PaneLeft => self.focus_toward(Direction::Left),
            Command::PaneDown => self.focus_toward(Direction::Down),
            Command::PaneUp => self.focus_toward(Direction::Up),
            Command::PaneRight => self.focus_toward(Direction::Right),
            Command::ToggleSplit => self.toggle_split(),
            Command::SplitRight => self.split_pane(Way::Right, HALF),
            Command::SplitDown => self.split_pane(Way::Down, HALF),
            Command::Zoom => self.toggle_zoom(),
            Command::Copy => self.start_copying(),
            Command::EditHistory => return self.edit_history(),
            Command::Float => self.toggle_float(),
            Command::Layouts => return Some(Action::ListLayouts),
            Command::SwapLeft => self.move_pane(Direction::Left),
            Command::SwapDown => self.move_pane(Direction::Down),
            Command::SwapUp => self.move_pane(Direction::Up),
            Command::SwapRight => self.move_pane(Direction::Right),
            Command::Resize => self.start_resizing(),
            Command::NewTab => return self.new_tab(),
            Command::RenameTab => self.ask_for_tab_name(),
            Command::CloseTab => self.close_tab(),
            Command::MoveToTab => self.ask_where_to_move(),
            Command::PreviousTab => self.go_to_tab(self.tabs.previous()),
            Command::NextTab => self.go_to_tab(self.tabs.next()),
            Command::MoveTabLeft => self.shift_tab(-1),
            Command::MoveTabRight => self.shift_tab(1),
            Command::Tab(number) => self.go_to_tab(usize::from(number.max(1)) - 1),
            Command::PageUp => return Some(Action::PageBack(self.selected_slot()?)),
            Command::PageDown => return Some(Action::PageForward(self.selected_slot()?)),
            Command::NewSession => return self.open_launcher(false),
            Command::NewWorktree => return self.open_launcher(true),
            Command::RemoveWorktree => self.ask_to_remove_worktree(),
            Command::Rename => self.ask_for_name(),
            Command::Kill => self.confirm = Some(Confirm::Kill(self.selected()?.name.clone())),
            Command::Archive => {
                self.confirm = Some(Confirm::Archive(self.selected()?.name.clone()));
            }
            Command::Archived => return Some(Action::ListArchived),
            Command::RunProject => return self.project_command(Verb::Run),
            Command::OpenProject => return self.project_command(Verb::Open),
            Command::NextNeedingYou => self.select_next_needing_user(),
            Command::NeedsYou => self.open_needs_you(),
            Command::Timeline => return Some(self.open_timeline()),
            Command::Diff => return self.open_diff(),
            Command::FindFile => return self.open_finder(),
            Command::FileTree => return self.open_tree_browser(),
            Command::Grep => self.open_grep(),
            Command::Branches => return self.open_switcher(),
            Command::Memory => return self.open_memory(),
            Command::Profiles => return self.open_profiles(),
            Command::Keys => self.keys_page = Some(0),
            Command::Search => return self.open_filter(),
            Command::Commands => self.open_command_list(),
            Command::PullRequest => return self.open_pull_request(),
            Command::PullRequests => return self.open_pull_requests(),
            Command::Issues => return self.open_issues(),
            Command::CloseTask if self.tasks_on => self.ask_how_the_task_went(),
            Command::CloseTask => self.notify(plugins::off("tasks")),
            Command::TaskToTerminal => return self.task_to_terminal(),
            Command::Backlog if self.backlog_on => return self.open_backlog(),
            Command::Backlog => self.notify(plugins::off("backlog")),
            Command::FlowGoOn if self.flows_on => return self.go_on_with_flow(),
            Command::FlowSendBack if self.flows_on => self.ask_to_send_flow_back(),
            Command::FlowGoOn | Command::FlowSendBack => self.notify(plugins::off("flows")),
            Command::NarrowerSidebar => self.resize_sidebar(-SIDEBAR_STEP),
            Command::WiderSidebar => self.resize_sidebar(SIDEBAR_STEP),
            Command::FoldSidebar => self.fold_sidebar(),
            Command::FoldProject => self.fold_project(),
            Command::UnfoldProject => self.unfold_project(),
            Command::Plugins => return Some(Action::ListPlugins),
            Command::Settings => {
                self.settings = Some(SettingsView::new());
                return Some(Action::OpenSettings);
            }
            Command::Quit => return Some(Action::Quit),
        }
        None
    }

    /// Asks for the project's run or open command in the worktree the
    /// selection is in.
    fn project_command(&mut self, which: Verb) -> Option<Action> {
        let Some(worktree) = self.selection_worktree().cloned() else {
            self.notify("select a session in a git worktree".into());
            return None;
        };
        Some(Action::ProjectCommand { which, worktree })
    }

    /// Asks before stopping the session called `name`, as `x` does.
    pub fn confirm_kill(&mut self, name: String) {
        self.confirm = Some(Confirm::Kill(name));
    }

    /// `:`: opens the list of every command.
    fn open_command_list(&mut self) {
        let plugin_on = |plugin: &str| self.plugin_on(plugin);
        let actions: Vec<PluginAction> = self
            .plugin_keys
            .iter()
            .map(|taken| PluginAction {
                plugin: &taken.plugin,
                action: &taken.action,
                title: &taken.title,
                key: taken.key,
            })
            .collect();
        let rows = command_list::rows(&self.keymap, &plugin_on, &actions);
        self.command_list = Some(CommandList::new(rows, &self.recent_commands));
    }

    /// A key while the `:` list is open: Enter runs the command the bar is
    /// on, as its key would where the keyboard is.
    fn on_command_list_key(&mut self, key: KeyEvent) -> Option<Action> {
        let list = self.command_list.as_mut()?;
        match list.on_key(&key) {
            command_list::Outcome::Stay => None,
            command_list::Outcome::Close => {
                self.command_list = None;
                None
            }
            command_list::Outcome::Run(pick) => {
                self.command_list = None;
                command_list::remember(&mut self.recent_commands, pick.clone());
                match pick {
                    Pick::Command(command) => self.run(command),
                    Pick::Plugin { plugin, action } => Some(Action::RunPlugin {
                        plugin,
                        action,
                        context: self.selected_context(),
                    }),
                    Pick::Custom(command) => self.run_key_command(command),
                }
            }
        }
    }

    /// The key after the prefix, in a pane: the prefix again goes to the
    /// program, Esc lets it be, the hand-back key hands the keyboard back,
    /// and any other is the sidebar's command, run with the keyboard
    /// staying in the pane, unless the command moves it.
    fn after_prefix(&mut self, slot: Slot, key: KeyEvent) -> Option<Action> {
        if self.keymap.is_prefix(&key) {
            if self.pane_shows_task(slot) {
                return None;
            }
            return Some(Action::Type { to: slot, key });
        }
        if key.code == KeyCode::Esc {
            return None;
        }
        if self.keymap.is_hand_back(&key) {
            self.focus = Focus::Sidebar;
            return None;
        }
        if let Some(bound) = self.keymap.bound(&key) {
            return self.run_bound(bound);
        }
        if let Some(ran) = self.plugin_key(&key) {
            return ran;
        }
        let prefix = self.keymap.prefix().map(|p| p.hint()).unwrap_or_default();
        let commands = self.keymap.hint(Command::Commands);
        let list = commands.map_or(String::new(), |key| format!(": {prefix} {key} lists them"));
        let written = Chord::of(&key).hint();
        self.notify(format!("{written} runs no command{list}"));
        None
    }

    /// What `key` does as an installed plugin's: runs the action that took
    /// it, about the selected session, or waits for the second of the two
    /// keys an action took. `None` when it's no plugin's.
    fn plugin_key(&mut self, key: &KeyEvent) -> Option<Option<Action>> {
        let chord = Chord::of(key);
        let taken = self
            .plugin_keys
            .iter()
            .find(|taken| taken.key.is_some_and(|key| key.first() == chord))?;
        if taken.key.and_then(|key| key.then()).is_some() {
            self.pending = Some(chord);
            return Some(None);
        }
        Some(self.run_plugin_action(taken))
    }

    /// The key after the first of a plugin's two: the second of an action's
    /// runs it; Esc lets the first go, and any other runs nothing.
    fn second_key(&mut self, first: Chord, key: KeyEvent) -> Option<Action> {
        if key.code == KeyCode::Esc {
            return None;
        }
        let second = Chord::of(&key);
        let taken = self.plugin_keys.iter().find(|taken| {
            taken
                .key
                .is_some_and(|key| key.first() == first && key.then() == Some(second))
        });
        match taken {
            Some(taken) => self.run_plugin_action(taken),
            None => {
                self.notify(format!("{first} {second} runs nothing"));
                None
            }
        }
    }

    /// Runs `taken`'s action, about the selected session.
    fn run_plugin_action(&self, taken: &PluginKey) -> Option<Action> {
        Some(Action::RunPlugin {
            plugin: taken.plugin.clone(),
            action: taken.action.clone(),
            context: self.selected_context(),
        })
    }

    /// The first of a plugin's two keys, while the next key is its second.
    pub fn pending(&self) -> Option<Chord> {
        self.pending
    }

    /// What a plugin's action or pane is told about where it was run from:
    /// the selected session. With none, the event loop says where.
    fn selected_context(&self) -> plugins::Context {
        self.selected()
            .map(plugins::Context::of_session)
            .unwrap_or_default()
    }

    /// Keys while the settings view is open: all of them are its.
    fn on_settings_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.settings.as_mut()?.on_key(key) {
            settings_view::Outcome::Stay => None,
            settings_view::Outcome::Close => {
                self.settings = None;
                Some(Action::CloseSettings)
            }
            settings_view::Outcome::Change(change) => Some(Action::ChangeSetting(change)),
            settings_view::Outcome::Prepare => Some(Action::PrepareEmbeddings),
        }
    }

    /// Keys while the plugins view is open: all of them are its.
    fn on_plugins_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.plugins_view.as_mut()?.on_key(key) {
            plugins_view::Outcome::Stay => None,
            plugins_view::Outcome::Close => {
                self.plugins_view = None;
                None
            }
            plugins_view::Outcome::Switch { name, on } => Some(Action::SwitchPlugin { name, on }),
            plugins_view::Outcome::Run { plugin, action } => Some(Action::RunPlugin {
                plugin,
                action,
                context: self.selected_context(),
            }),
            plugins_view::Outcome::Open { plugin, pane } => Some(Action::OpenPluginPane {
                plugin,
                pane,
                context: self.selected_context(),
            }),
        }
    }

    /// Opens the diff of the selected session's worktree, and asks for it
    /// to be read.
    fn open_diff(&mut self) -> Option<Action> {
        let (dir, place) = self.selected_worktree()?;
        let diff = DiffView::new(dir, place, self.diff_tree);
        let read = diff.read();
        self.view = Some(View::Diff(diff));
        Some(read)
    }

    /// Opens the file finder on the selected session's worktree, and asks
    /// for its files to be listed.
    fn open_finder(&mut self) -> Option<Action> {
        let (dir, place) = self.selected_worktree()?;
        let finder = Finder::new(dir, place);
        let read = finder.read();
        self.view = Some(View::Files(finder));
        Some(read)
    }

    /// `E`: opens the tree browser on the selected session's worktree, and
    /// asks for its files to be listed.
    fn open_tree_browser(&mut self) -> Option<Action> {
        let (dir, place) = self.selected_worktree()?;
        let tree = TreeBrowser::new(dir, place);
        let read = tree.read();
        self.view = Some(View::Tree(tree));
        Some(read)
    }

    /// `B`: opens the branch switcher on the selected session's worktree,
    /// and asks for its branches to be listed. Only a project's main
    /// worktree switches: a linked one is named after its branch.
    fn open_switcher(&mut self) -> Option<Action> {
        let worktree = match self.selected_empty_worktree() {
            Some(worktree) => worktree.clone(),
            None => {
                let selected = self.selected()?;
                let Some(worktree) = selected.worktree.clone() else {
                    let notice = format!("{} isn't in a git repository", selected.name);
                    self.notify(notice);
                    return None;
                };
                worktree
            }
        };
        if !worktree.main {
            self.notify(
                "B switches a project's main worktree: a linked one stays on the branch it was made for"
                    .to_string(),
            );
            return None;
        }
        if self.switching.contains(&worktree.path) {
            self.notify("git is still switching it: the footer will say how it went".to_string());
            return None;
        }
        let switcher = Switcher::new(worktree.path.clone(), worktree_label(&worktree));
        let read = switcher.read();
        self.view = Some(View::Branches(switcher));
        Some(read)
    }

    /// `G`: opens find in files on the selected session's worktree.
    fn open_grep(&mut self) {
        if let Some((dir, place)) = self.selected_worktree() {
            self.view = Some(View::Grep(Grep::new(dir, place)));
        }
    }

    /// `m`: opens the memory of the selected session's project, and asks for
    /// it to be read. A session outside git has its directory for a
    /// project. With memory off, the footer says so instead.
    fn open_memory(&mut self) -> Option<Action> {
        if !self.memory_on {
            self.notify(plugins::off("memory"));
            return None;
        }
        let selected = self.selected()?;
        let (dir, place) = match &selected.worktree {
            Some(worktree) => (worktree.project_path.clone(), worktree.project.clone()),
            None => (selected.cwd.clone(), shell::home_relative(&selected.cwd)),
        };
        let memory = MemoryView::new(dir, place);
        let read = memory.read();
        self.view = Some(View::Memory(memory));
        Some(read)
    }

    /// The selected session's worktree, or the worktree with no sessions
    /// the selection is on, and its project and branch the way a view's
    /// header names them: `payments ⎇ fix/login`. When a session isn't in
    /// one, the footer says so.
    fn selected_worktree(&mut self) -> Option<(PathBuf, String)> {
        if let Some(worktree) = self.selected_empty_worktree() {
            return Some((worktree.path.clone(), worktree_label(worktree)));
        }
        let selected = self.selected()?;
        let Some(worktree) = &selected.worktree else {
            let notice = format!("{} isn't in a git repository", selected.name);
            self.notify(notice);
            return None;
        };
        let mark = if worktree.main { "⌂" } else { "⎇" };
        let branch = worktree.branch.as_deref().unwrap_or("(detached)");
        let place = format!("{} {mark} {branch}", worktree.project);
        Some((worktree.path.clone(), place))
    }

    /// Keys while a view is open: they're all the view's.
    /// `key` as what has the keyboard takes it, by the keys the user gave
    /// the views and the needs-you view's answers: itself, or the key it
    /// stands for; `None` for a default the user took from them.
    fn as_views_take(&self, key: KeyEvent) -> Option<KeyEvent> {
        let Some((typing, answers)) = self.view_taking_keys() else {
            return Some(key);
        };
        if answers {
            match self.keymap.translate(Mode::Answer, &key, typing) {
                Translated::Same => {}
                Translated::As(own) => return Some(own),
                Translated::Nothing => return None,
            }
        }
        match self.keymap.translate(Mode::View, &key, typing) {
            Translated::Same => Some(key),
            Translated::As(own) => Some(own),
            Translated::Nothing => None,
        }
    }

    /// Whether what has the keyboard is a view that takes the views' keys,
    /// in the order [`App::take_key`] hands keys out; and if it is,
    /// whether a character is typed there just now, and whether it takes
    /// the answers too, as the needs-you view does. A form, a question or
    /// a menu takes none.
    fn view_taking_keys(&self) -> Option<(bool, bool)> {
        if let Some(view) = &self.view {
            let typing = match view {
                View::Diff(diff) => diff.typing(),
                View::Memory(memory) => memory.typing(),
                View::Files(_) | View::Tree(_) | View::Grep(_) | View::Branches(_) => true,
            };
            return Some((typing, false));
        }
        let asking = self.menu.is_some()
            || self.keys_page.is_some()
            || self.confirm.is_some()
            || self.moving.is_some()
            || self.closing.is_some()
            || self.reply.is_some();
        if asking {
            return None;
        }
        if self.command_list.is_some() {
            return Some((true, false));
        }
        if let Some(backlog) = &self.backlog {
            return Some((backlog.typing(), false));
        }
        if let Some(layouts) = &self.layouts {
            return Some((layouts.typing(), false));
        }
        if let Some(archived) = &self.archived {
            return Some((archived.typing(), false));
        }
        if self.launcher.is_some() || self.profiles_view.is_some() {
            return None;
        }
        if self.plugins_view.is_some() || self.settings.is_some() {
            return Some((false, false));
        }
        if self.prompt.is_some() {
            return None;
        }
        if self.needs_you.is_some() {
            return Some((false, true));
        }
        if self.timeline.is_some() {
            return Some((true, false));
        }
        if let Some(issues) = &self.issues {
            let writing = issues.comment.is_some() || issues.form.is_some();
            return (!writing).then_some((true, false));
        }
        if let Some(view) = &self.pull_requests_view {
            return view.comment.is_none().then_some((true, false));
        }
        self.filter.is_some().then_some((true, false))
    }

    fn on_view_key(&mut self, key: KeyEvent) -> Option<Action> {
        let outcome = match self.view.as_mut()? {
            View::Diff(diff) => diff.on_key(key),
            View::Files(finder) => finder.on_key(key),
            View::Tree(tree) => tree.on_key(key),
            View::Grep(grep) => grep.on_key(key),
            View::Branches(switcher) => switcher.on_key(key),
            View::Memory(memory) => memory.on_key(key),
        };
        self.follow(outcome)
    }

    /// Does what an open view asked for.
    fn follow(&mut self, outcome: Outcome) -> Option<Action> {
        match outcome {
            Outcome::Stay => None,
            Outcome::Close => {
                self.view = None;
                None
            }
            Outcome::Do(action) => {
                match &action {
                    Action::KeepTree(on) => self.diff_tree = *on,
                    Action::SwitchBranch { dir, .. } => {
                        self.switching.insert(dir.clone());
                    }
                    _ => {}
                }
                Some(action)
            }
            Outcome::Edit { path, line } => {
                let dir = match self.view.take()? {
                    View::Files(finder) => finder.dir,
                    View::Tree(tree) => tree.dir,
                    View::Grep(grep) => grep.dir,
                    View::Memory(memory) => memory.file_to_open()?.0,
                    _ => return None,
                };
                let name = self.free_name(&edit_name(&path));
                Some(Action::Edit {
                    dir,
                    path,
                    line,
                    name,
                })
            }
        }
    }

    /// `base`, or else `base-2`, `base-3`… whichever no session has yet.
    fn free_name(&self, base: &str) -> String {
        (1..)
            .map(|n| match n {
                1 => base.to_string(),
                n => format!("{base}-{n}"),
            })
            .find(|name| self.position(name).is_none())
            .unwrap()
    }

    /// Enter on a session: types into it while it runs, or, once it has
    /// ended, offers to start it again.
    fn enter(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        if selected.state == State::Running {
            self.type_into_selected();
            return;
        }
        let name = selected.name.clone();
        // A flow's step runs again as a step of its run, or the run
        // wouldn't know.
        let flow_step = self
            .selected_index()
            .and_then(|index| self.flow_step_of(index));
        if let Some((run, step)) = flow_step {
            let notice = format!(
                "g runs {} again, as a step of {}",
                run.step_name(step),
                run.name
            );
            self.notify(notice);
            return;
        }
        self.confirm = Some(Confirm::Respawn(name));
    }

    /// Asks for a new name for the selected session, starting from the
    /// one it has.
    fn ask_for_name(&mut self) {
        if let Some(selected) = self.selected() {
            let name = selected.name.clone();
            self.ask(Question::Rename(name.clone()), &name);
        }
    }

    /// Asks for a name for the tab in front, starting from the one it has.
    fn ask_for_tab_name(&mut self) {
        let name = self.tabs.current().name.clone();
        self.ask(Question::TabName, &name);
    }

    /// Asks before removing the selected session's worktree, or the
    /// worktree with no sessions the selection is on. Only a linked
    /// worktree goes, and only once nothing runs in it any more: removing
    /// it would pull the directory out from under them.
    fn ask_to_remove_worktree(&mut self) {
        // A project with no sessions is on its main worktree's row: what
        // goes is the project, off the list, never anything on disk.
        if let Some(project) = self.selected_empty_worktree().filter(|w| w.main) {
            self.confirm = Some(Confirm::ForgetProject {
                path: project.path.clone(),
                name: project.project.clone(),
            });
            return;
        }
        if let Some(worktree) = self.selected_empty_worktree() {
            let branch = worktree.branch.as_deref().unwrap_or("(detached)");
            let (path, branch) = (worktree.path.clone(), branch.to_string());
            self.confirm_removal(path, branch);
            return;
        }
        let Some(selected) = self.selected() else {
            return;
        };
        let name = selected.name.clone();
        let Some(worktree) = selected.worktree.clone() else {
            self.notify(format!("{name} isn't in a git worktree"));
            return;
        };
        if worktree.main {
            self.notify("the main worktree can't be removed".into());
            return;
        }
        let branch = worktree.branch.unwrap_or_else(|| "(detached)".into());
        let running: Vec<&str> = self
            .sessions
            .iter()
            .filter(|session| session.state == State::Running)
            .filter(|session| {
                let in_it = session.worktree.as_ref();
                in_it.is_some_and(|w| w.path == worktree.path)
            })
            .map(|session| session.name.as_str())
            .collect();
        if running.is_empty() {
            self.confirm_removal(worktree.path, branch);
        } else {
            let notice = format!("{} still running in {branch}", running.join(", "));
            self.notify(notice);
        }
    }

    /// What to ask about the worktree killing the session called `name`
    /// leaves with nothing in it, if it does: a linked worktree, not one
    /// Claude Code made for itself, with no other session in it, in any
    /// tab, running or not, that the daemon isn't removing already.
    fn emptied_by_killing(&self, name: &str) -> Option<Confirm> {
        let killed = self.sessions.iter().find(|session| session.name == name)?;
        let worktree = killed.worktree.as_ref()?;
        if worktree.main || worktree.claude_codes_own() || self.removing(&worktree.path) {
            return None;
        }
        let others = self.sessions.iter().any(|session| {
            session.name != name
                && session
                    .worktree
                    .as_ref()
                    .is_some_and(|w| w.path == worktree.path)
        });
        if others {
            return None;
        }
        let name = match self.label_of(&worktree.path) {
            Some(label) => label.to_string(),
            None => worktree
                .branch
                .clone()
                .unwrap_or_else(|| "(detached)".to_string()),
        };
        Some(Confirm::RemoveEmptied {
            path: worktree.path.clone(),
            name,
        })
    }

    /// Asks before removing the worktree at `path`, on `branch`, unless the
    /// daemon is removing it already.
    fn confirm_removal(&mut self, path: PathBuf, branch: String) {
        if self.removing(&path) {
            self.notify(format!("already removing {branch}"));
        } else {
            self.confirm = Some(Confirm::RemoveWorktree {
                path,
                branch,
                force: false,
            });
        }
    }

    /// Opens `/`'s filter, its bar on the selected session, and has the
    /// forges asked about the open pull requests of the projects no session
    /// is in, which nothing has asked about yet, for it to find.
    fn open_filter(&mut self) -> Option<Action> {
        let highlighted = self
            .selected()
            .map(|session| Found::Session(session.id.clone()));
        self.filter = Some(Filter {
            input: TextInput::default(),
            status: None,
            highlighted,
        });
        self.keep_filter_bar_on_a_match();
        if !self.github_on {
            return None;
        }
        let unasked: Vec<PathBuf> = self
            .quiet_projects()
            .into_iter()
            .map(|project| project.path.clone())
            .filter(|project| !self.pull_requests.contains_key(project))
            .collect();
        (!unasked.is_empty()).then_some(Action::FindPullRequests(unasked))
    }

    /// Keys while `/`'s filter is open: Enter picks what the bar is on, Esc
    /// leaves the selection where it was, ↑ and ↓ (or Ctrl+P and Ctrl+N)
    /// move the bar among what's found, Tab and Shift+Tab go round the
    /// statuses it keeps to, and every other key edits the filter. Letters
    /// type, so j and k don't move the bar here.
    fn on_filter_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.close_filter(),
            KeyCode::Enter => {
                let highlighted = self.filter.as_ref()?.highlighted.clone();
                match highlighted {
                    Some(found) => return self.pick(found),
                    None => self.close_filter(),
                }
            }
            KeyCode::Up => self.move_filter_bar(-1),
            KeyCode::Down => self.move_filter_bar(1),
            KeyCode::Char('p') if ctrl => self.move_filter_bar(-1),
            KeyCode::Char('n') if ctrl => self.move_filter_bar(1),
            KeyCode::Tab => self.step_filter_status(1),
            KeyCode::BackTab => self.step_filter_status(-1),
            _ => {
                if let Some(filter) = &mut self.filter {
                    filter.input.on_key(&key);
                }
                self.keep_filter_bar_on_a_match();
            }
        }
        None
    }

    /// Closes `/`'s filter, the selection where it was.
    fn close_filter(&mut self) {
        self.filter = None;
        self.keep_selection_on_a_row();
    }

    /// Closes `/`'s filter and does what picking `found` does: a session is
    /// selected, in whichever tab it's in; a worktree with no sessions has
    /// the selection put on it, where Enter starts something; a flow run's
    /// step it's at is selected; and a pull request is opened in the pull
    /// requests view.
    fn pick(&mut self, found: Found) -> Option<Action> {
        self.filter = None;
        match found {
            Found::Session(id) => {
                let session = self.sessions.iter().find(|session| session.id == id);
                if let Some(name) = session.map(|session| session.name.clone()) {
                    self.select(&name);
                }
            }
            Found::Worktree(path) => self.select_worktree(path),
            Found::Flow(name) => self.select_flow(&name),
            Found::PullRequest { project, number } => {
                return self.open_pull_requests_of(project, Some(number));
            }
        }
        self.keep_selection_on_a_row();
        None
    }

    /// Puts the selection on the worktree with no sessions at `path`. One
    /// in a project with sessions shows only in their tabs, so when the tab
    /// in front has none of them, the first tab that has one comes to the
    /// front; a project with no sessions shows in every tab.
    fn select_worktree(&mut self, path: PathBuf) {
        let project = self
            .worktrees
            .values()
            .flatten()
            .find(|worktree| worktree.path == path)
            .map(|worktree| worktree.project_path.clone());
        let in_project = |session: &SessionInfo| {
            let worktree = session.worktree.as_ref();
            project.is_some() && worktree.map(|w| &w.project_path) == project.as_ref()
        };
        let tabs: Vec<usize> = self
            .sessions
            .iter()
            .filter(|session| in_project(session))
            .filter_map(|session| self.tabs.tab_of(&session.name))
            .collect();
        if let Some(&tab) = tabs.iter().min()
            && !tabs.contains(&self.tabs.current_index())
        {
            self.go_to_tab(tab);
        }
        self.on_worktree = Some(path);
    }

    /// Selects the session of the step the flow run called `name` is at:
    /// the latest that has a session.
    fn select_flow(&mut self, name: &str) {
        let Some(run) = self.flows.iter().find(|run| run.name == name) else {
            return;
        };
        let at = run
            .steps
            .iter()
            .rev()
            .filter_map(|step| step.session.clone())
            .find(|session| self.position(session).is_some());
        match at {
            Some(session) => self.select(&session),
            None => self.notify(format!("no step of {name} has a session")),
        }
    }

    /// Goes round the statuses `/`'s filter keeps to, `by` one forward or
    /// back.
    fn step_filter_status(&mut self, by: isize) {
        if let Some(filter) = &mut self.filter {
            filter.status = StatusFilter::step(filter.status, by);
        }
        self.keep_filter_bar_on_a_match();
    }

    /// Moves the filter's bar `by` places among what's found, stopping at
    /// the ends.
    fn move_filter_bar(&mut self, by: isize) {
        let found = self.found();
        let Some(filter) = &mut self.filter else {
            return;
        };
        let at = filter
            .highlighted
            .as_ref()
            .and_then(|highlighted| found.iter().position(|each| each == highlighted));
        let Some(at) = at else {
            return;
        };
        let to = at.saturating_add_signed(by).min(found.len() - 1);
        filter.highlighted = Some(found[to].clone());
    }

    /// Puts the filter's bar on the first thing found when what it was on
    /// isn't found any more, while the filter is open.
    fn keep_filter_bar_on_a_match(&mut self) {
        if self.filter.is_none() {
            return;
        }
        let found = self.found();
        let Some(filter) = &mut self.filter else {
            return;
        };
        let on_a_match = filter
            .highlighted
            .as_ref()
            .is_some_and(|highlighted| found.contains(highlighted));
        if !on_a_match {
            filter.highlighted = found.into_iter().next();
        }
    }

    /// The worktree of the selected session, for a key that asks its
    /// project's forge something: `None`, the footer saying why, when the
    /// github plugin is off, when it isn't in a repository, or when what
    /// stopped the forge listing its pull requests would stop this too.
    fn forge_worktree(&mut self) -> Option<Worktree> {
        if !self.github_on {
            self.notify(plugins::off("github"));
            return None;
        }
        let selected = self.selected()?;
        let name = selected.name.clone();
        let Some(worktree) = selected.worktree.clone() else {
            self.notify(format!("{name} isn't in a git repository"));
            return None;
        };
        if let Some(Err(reason)) = self.pull_requests.get(&worktree.project_path) {
            let reason = reason.clone();
            self.notify(reason);
            return None;
        }
        Some(worktree)
    }

    /// `o`: opens the pull request of the selected session's branch, or
    /// says why there's none to open.
    fn open_pull_request(&mut self) -> Option<Action> {
        let worktree = self.forge_worktree()?;
        let Some(branch) = worktree.branch else {
            let name = self.selected()?.name.clone();
            self.notify(format!("{name} is on no branch"));
            return None;
        };
        let project = worktree.project_path;
        let Some(Ok((forge, _))) = self.pull_requests.get(&project) else {
            let name = worktree.project;
            self.notify(format!("still asking about {name}'s pull requests"));
            return None;
        };
        let forge = *forge;
        match self.pull_request(&project, &branch) {
            Some(pull_request) => Some(Action::OpenInBrowser {
                topic: Topic::PullRequest(pull_request.number),
                project,
            }),
            None => {
                self.notify(format!("no open {} for {branch}", forge.pull_request()));
                None
            }
        }
    }

    /// `O`: opens the pull requests view for the selected session's
    /// project, on the ones listed last until its forge lists them again,
    /// or says why it can't.
    fn open_pull_requests(&mut self) -> Option<Action> {
        let worktree = self.forge_worktree()?;
        self.open_pull_requests_of(worktree.project_path, None)
    }

    /// Opens the pull requests view for the project at `project`, on the
    /// ones listed last until its forge lists them again, with the bar on
    /// pull request `number` when there's one to put it on.
    fn open_pull_requests_of(&mut self, project: PathBuf, number: Option<u64>) -> Option<Action> {
        let known = match self.pull_requests.get(&project) {
            Some(Ok((_, pull_requests))) => Some(pull_requests.clone()),
            _ => None,
        };
        let forge = self.forge_of(&project);
        let name = self.project_name(&project);
        let hide_drafts = self.hide_draft_prs;
        let mut view = PullRequestsView::new(project.clone(), name, forge, known, hide_drafts);
        if let Some(number) = number {
            view.list.highlight(number);
        }
        self.pull_requests_view = Some(view);
        Some(Action::ListPullRequests(project))
    }

    /// `i`: opens the issues view for the selected session's project, on
    /// the ones listed last until its forge lists them again, or says why
    /// it can't.
    fn open_issues(&mut self) -> Option<Action> {
        let worktree = self.forge_worktree()?;
        let project = worktree.project_path;
        let known = match self.open_issues.get(&project) {
            Some(Ok((_, issues))) => Some(issues.clone()),
            _ => None,
        };
        let forge = self.forge_of(&project);
        let view = IssuesView::new(project.clone(), worktree.project, forge, known);
        self.issues = Some(view);
        Some(Action::ListIssues(project))
    }

    /// `c`: asks, on the footer, whether the selected session's task was
    /// done or failed, or says why there's no task to close.
    fn ask_how_the_task_went(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        let name = selected.name.clone();
        match &selected.task {
            None => self.notify(format!("{name} has no task to close")),
            Some(task) if task.outcome.is_some() => {
                self.notify(format!("{name}'s task is closed already"));
            }
            Some(_) => self.closing = Some(name),
        }
    }

    /// The flow runs the sidebar groups sessions under: none while flows
    /// are off.
    fn shown_flows(&self) -> &[FlowRun] {
        if self.flows_on { &self.flows } else { &[] }
    }

    /// The flow run the selected session is a step of, or `None`, the
    /// footer saying it isn't one.
    fn selected_flow(&mut self) -> Option<FlowRun> {
        let index = self.selected_index()?;
        let found = self.flow_step_of(index).map(|(run, _)| run.clone());
        if found.is_none() {
            let name = self.sessions[index].name.clone();
            self.notify(format!("{name} isn't a step of a flow"));
        }
        found
    }

    /// `g`: the selected step's flow goes on: past the gate it waits at, or,
    /// stopped at a step that failed or was cut short, with that step run
    /// again. Running or done, the footer says so.
    fn go_on_with_flow(&mut self) -> Option<Action> {
        let run = self.selected_flow()?;
        match run.state() {
            RunState::AtGate => Some(Action::ApproveFlow(run.name)),
            RunState::Failed | RunState::Interrupted => Some(Action::RetryFlow(run.name)),
            RunState::Running => {
                self.notify(format!("{} is still running", run.name));
                None
            }
            RunState::Done | RunState::Cancelled => {
                self.notify(format!("{} is {}", run.name, run.state().word()));
                None
            }
        }
    }

    /// `f`: asks, on the footer, for notes to send the selected step's flow
    /// back from its gate with.
    fn ask_to_send_flow_back(&mut self) {
        let Some(run) = self.selected_flow() else {
            return;
        };
        if run.state() == RunState::AtGate {
            self.ask(Question::SendFlowBack(run.name), "");
        } else {
            self.notify(format!("{} isn't waiting at a gate", run.name));
        }
    }

    /// `b`: opens the backlog of the selected session's project: its main
    /// worktree's, or, outside git, its directory's.
    fn open_backlog(&mut self) -> Option<Action> {
        let selected = self.selected()?;
        let (dir, name) = match &selected.worktree {
            Some(worktree) => (worktree.project_path.clone(), worktree.project.clone()),
            None => (selected.cwd.clone(), shell::home_relative(&selected.cwd)),
        };
        self.backlog = Some(BacklogView::new(dir.clone(), name));
        Some(Action::ListBacklog(dir))
    }

    /// Keys while the archive is open: all of them are its.
    fn on_archived_key(&mut self, key: KeyEvent) -> Option<Action> {
        let view = self.archived.as_mut()?;
        match view.on_key(&key) {
            archived_view::Step::Stay => None,
            archived_view::Step::Close => {
                self.archived = None;
                None
            }
            archived_view::Step::Restore(id) => Some(Action::Unarchive(id)),
            archived_view::Step::Delete(id) => Some(Action::DeleteArchived(id)),
        }
    }

    /// Keys while the layouts view is open: all of them are its.
    fn on_layouts_key(&mut self, key: KeyEvent) -> Option<Action> {
        let view = self.layouts.as_mut()?;
        match view.on_key(&key) {
            layouts::Step::Stay => None,
            layouts::Step::Close => {
                self.layouts = None;
                None
            }
            layouts::Step::Save(name) => Some(Action::SaveLayout(name)),
            layouts::Step::Restore(which) => Some(Action::RestoreLayout(which)),
            layouts::Step::Remove(which) => Some(Action::RemoveLayout(which)),
        }
    }

    /// Keys while the backlog view is open: all of them are its.
    fn on_backlog_key(&mut self, key: KeyEvent) -> Option<Action> {
        let view = self.backlog.as_mut()?;
        match view.on_key(&key) {
            Step::Stay => None,
            Step::Close => {
                self.backlog = None;
                None
            }
            Step::Change(change) => Some(Action::ChangeBacklog {
                dir: view.dir.clone(),
                change,
            }),
            Step::Start(item) => {
                self.backlog = None;
                let branch = forge::branch_for_issue(item.number, &item.text);
                let setup = self.launch_setup(false);
                let launcher = Launcher::new(setup)
                    .with_task(&crate::backlog::goal(&item))
                    .with_branch(&branch)
                    .for_backlog_item(item.number);
                self.launcher = Some(launcher);
                self.codex_models_wanted()
            }
        }
    }

    /// Keys while the issues view is open: all of them are its.
    fn on_issues_key(&mut self, key: KeyEvent) -> Option<Action> {
        let view = self.issues.as_mut()?;
        let project = view.project.clone();
        match view.on_key(&key) {
            issues::Step::Stay => None,
            issues::Step::Close => {
                self.issues = None;
                None
            }
            issues::Step::Start(issue) => self.start_on_issue(&issue),
            issues::Step::Open(number) => Some(Action::OpenInBrowser {
                project,
                topic: Topic::Issue(number),
            }),
            issues::Step::Comment { number, text } => Some(Action::Comment {
                project,
                topic: Topic::Issue(number),
                text,
            }),
            issues::Step::Edit {
                number,
                title,
                body,
            } => Some(Action::EditIssue {
                project,
                number,
                title,
                body,
            }),
            issues::Step::Refresh => Some(Action::ListIssues(project)),
            issues::Step::Say(said) => {
                self.notify(said);
                None
            }
        }
    }

    /// Keys while the pull requests view is open: all of them are its.
    fn on_pull_requests_key(&mut self, key: KeyEvent) -> Option<Action> {
        let view = self.pull_requests_view.as_mut()?;
        let project = view.project.clone();
        match view.on_key(&key) {
            pull_requests::Step::Stay => None,
            pull_requests::Step::Close => {
                self.pull_requests_view = None;
                None
            }
            pull_requests::Step::Start(pull_request) => self.start_on_pull_request(&pull_request),
            pull_requests::Step::Diff(number) => {
                let place = view.project_name.clone();
                let diff =
                    DiffView::of_pull_request(project, place, self.diff_tree, view.forge, number);
                let read = diff.read();
                // Over the view, which is there again when the diff closes.
                self.view = Some(View::Diff(diff));
                Some(read)
            }
            pull_requests::Step::Open(number) => Some(Action::OpenInBrowser {
                project,
                topic: Topic::PullRequest(number),
            }),
            pull_requests::Step::Comment { number, text } => Some(Action::Comment {
                project,
                topic: Topic::PullRequest(number),
                text,
            }),
            pull_requests::Step::Refresh => Some(Action::ListPullRequests(project)),
            pull_requests::Step::Say(said) => {
                self.notify(said);
                None
            }
        }
    }

    /// Closes the pull requests view and opens the new-session panel in
    /// `pull_request`'s worktree, with the task to work on it and its
    /// address, so the agent can read it.
    fn start_on_pull_request(&mut self, pull_request: &PullRequest) -> Option<Action> {
        let view = self.pull_requests_view.take()?;
        let forge = pull_request.forge;
        let task = format!(
            "Work on {} {}: {} ({})",
            forge.pull_request(),
            pull_request.label(),
            pull_request.title,
            pull_request.url
        );
        let mut setup = self.launch_setup(false);
        setup.targets = vec![Target::PullRequest {
            checkout: pull_request.checkout(&view.project),
            label: format!("{} ⎇ {}", view.project_name, pull_request.local_branch),
            choice: format!("{} {}", forge.pull_request(), pull_request.label()),
        }];
        setup.target = 0;
        let brief = TaskBrief {
            pull_request: Some(Box::new(ForgeLink {
                forge,
                number: pull_request.number,
                title: pull_request.title.clone(),
                url: pull_request.url.clone(),
                branch: Some(pull_request.local_branch.clone()),
            })),
            ..TaskBrief::default()
        };
        self.launcher = Some(Launcher::new(setup).with_task(&task).about(brief));
        self.codex_models_wanted()
    }

    /// Closes the issues view and opens the new-session panel for `issue`:
    /// a new worktree on a branch named after it, and the task to fix it,
    /// with the issue's address so the agent can read it.
    fn start_on_issue(&mut self, issue: &Issue) -> Option<Action> {
        let view = self.issues.take()?;
        let branch = forge::branch_for_issue(issue.number, &issue.title);
        let task = format!(
            "Fix issue #{}: {} ({})",
            issue.number, issue.title, issue.url
        );
        let mut setup = self.launch_setup(true);
        if let Some(Target::NewWorktree { base, .. }) = setup.targets.get_mut(1) {
            *base = Some(view.project.clone());
        }
        let brief = TaskBrief {
            issue: Some(Box::new(ForgeLink {
                forge: view.forge,
                number: issue.number,
                title: issue.title.clone(),
                url: issue.url.clone(),
                branch: None,
            })),
            ..TaskBrief::default()
        };
        let launcher = Launcher::new(setup)
            .with_task(&task)
            .with_branch(&branch)
            .about(brief);
        self.launcher = Some(launcher);
        self.codex_models_wanted()
    }

    /// Opens the new-session panel, set to start in a new worktree when
    /// `worktree` is set, or else where the selected session runs.
    /// It opens on the draft the last one put away left, if one did.
    fn open_launcher(&mut self, worktree: bool) -> Option<Action> {
        let setup = self.launch_setup(worktree);
        let draft = self.launch_draft.take();
        self.draft_starting = false;
        self.launcher = Some(Launcher::new(setup).with_draft(draft));
        self.codex_models_wanted()
    }

    /// What the panel opens with: what can run, with what to pick first,
    /// where it can start, and the tasks given before.
    fn launch_setup(&self, worktree: bool) -> Setup {
        let offered: &[Profile] = if self.profiles_on {
            &self.profiles
        } else {
            &[]
        };
        let mut runs: Vec<Run> = self
            .new_session_profile
            .iter()
            .chain(offered)
            .filter(|profile| {
                self.agents
                    .iter()
                    .any(|agent| agent.program == profile.agent)
            })
            .cloned()
            .map(Run::Profile)
            .collect();
        // A flow's steps run Claude Code unless their profiles say otherwise.
        let has_claude = self.agents.iter().any(|agent| agent.program == "claude");
        if self.flows_on && has_claude {
            runs.extend(self.flow_defs.iter().cloned().map(Run::Flow));
        }
        runs.extend(self.agents.iter().map(|agent| Run::Agent(agent)));
        runs.push(Run::Shell);
        let wanted = [self.memory.last_run.as_ref(), self.first_run.as_ref()];
        let picked = wanted
            .into_iter()
            .flatten()
            .find_map(|key| runs.iter().position(|run| run.key() == *key));
        let first_agent = runs.iter().position(|run| matches!(run, Run::Agent(_)));
        let run = picked.or(first_agent).unwrap_or(runs.len() - 1);
        Setup {
            runs,
            run,
            targets: self.launch_targets(),
            target: usize::from(worktree),
            history: self.memory.tasks.clone(),
            codex_models: self.codex_models.clone().unwrap_or_default(),
            background: self.tasks_on,
            branch: names::random(),
            worktree_directory: self.worktree_directory.clone(),
        }
    }

    /// Where a new session can start: where the selected session is, or
    /// the worktree with no sessions the selection is on; a new worktree of
    /// its project; or another project's main worktree.
    fn launch_targets(&self) -> Vec<Target> {
        let selected = self.selected();
        let worktree = self.selection_worktree();
        let here = match (worktree, selected) {
            (Some(worktree), _) => Target::Here {
                dir: Some(worktree.path.clone()),
                label: worktree_label(worktree),
            },
            (None, Some(session)) => Target::Here {
                dir: Some(session.cwd.clone()),
                label: shell::home_relative(&session.cwd),
            },
            (None, None) => match &self.start_dir {
                Some(dir) => Target::Here {
                    dir: Some(dir.clone()),
                    label: shell::home_relative(dir),
                },
                None => Target::Here {
                    dir: None,
                    label: "this directory".to_string(),
                },
            },
        };
        let mut targets = vec![
            here,
            Target::NewWorktree {
                base: self.worktree_base(),
                project: worktree.map(|worktree| worktree.project.clone()),
            },
        ];
        let current = worktree.map(|worktree| &worktree.project_path);
        let mut others: Vec<Target> = Vec::new();
        let sessions = self.sessions.iter().filter_map(|s| s.worktree.as_ref());
        for worktree in sessions.chain(&self.known) {
            let path = &worktree.project_path;
            let seen = others
                .iter()
                .any(|target| matches!(target, Target::Project { path: p, .. } if p == path));
            if Some(path) != current && !seen {
                others.push(Target::Project {
                    path: path.clone(),
                    label: worktree.project.clone(),
                });
            }
        }
        targets.extend(others);
        targets
    }

    /// Opens the profiles view, unless profiles are switched off.
    fn open_profiles(&mut self) -> Option<Action> {
        if !self.profiles_on {
            self.notify(plugins::off("profiles"));
            return None;
        }
        let models = self.codex_models.clone().unwrap_or_default();
        self.profiles_view = Some(ProfilesView::new(self.profiles.clone(), models));
        self.codex_models_wanted()
    }

    /// Keys while the profiles view is open: all of them are its.
    fn on_profiles_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.profiles_view.as_mut()?.on_key(key) {
            profiles::Outcome::Stay => None,
            profiles::Outcome::Close => {
                self.profiles_view = None;
                None
            }
            profiles::Outcome::Save { replacing, profile } => {
                Some(Action::SaveProfile { replacing, profile })
            }
            profiles::Outcome::Delete(name) => Some(Action::DeleteProfile(name)),
        }
    }

    /// Asks Codex for its models the first time the panel could show them.
    fn codex_models_wanted(&mut self) -> Option<Action> {
        let has_codex = self.agents.iter().any(|agent| agent.program == "codex");
        if !has_codex || self.codex_models.is_some() {
            return None;
        }
        self.codex_models = Some(Vec::new());
        Some(Action::ReadCodexModels)
    }

    /// Keys while the new-session panel is open: all of them are its.
    fn on_launcher_key(&mut self, key: KeyEvent) -> Option<Action> {
        let outcome = self.launcher.as_mut()?.on_key(key);
        match outcome {
            launcher::Outcome::Stay => None,
            launcher::Outcome::Cancel => {
                self.close_launcher(false);
                None
            }
            launcher::Outcome::Start {
                place,
                command,
                task,
                run,
                background,
                backlog,
                brief,
            } => {
                self.close_launcher(true);
                self.memory.remember(&task, &run);
                if background {
                    let spec = launcher::background_spec(&command)?;
                    return Some(Action::StartInBackground {
                        place,
                        spec,
                        backlog,
                        brief,
                    });
                }
                // Given something to do, the session is a task.
                let purpose = Purpose {
                    task: (!task.is_empty()).then_some(task),
                    backlog,
                    brief,
                };
                Some(Action::Start {
                    place,
                    command,
                    purpose,
                })
            }
            launcher::Outcome::StartFlow {
                place,
                flow,
                goal,
                run,
            } => {
                self.close_launcher(true);
                self.memory.remember(&goal, &run);
                Some(Action::StartFlow { place, flow, goal })
            }
            // The draft is kept until the command line starts a session.
            launcher::Outcome::CommandLine { place, line } => {
                self.close_launcher(true);
                self.ask(Question::Command(place), &line);
                None
            }
        }
    }

    /// Puts the new-session panel away. One `n` or `w` opened leaves what
    /// it holds as a draft for the next to open on, unless no task is
    /// written in it. `starting` says a session is being started from it,
    /// and the draft goes once that session has started.
    fn close_launcher(&mut self, starting: bool) {
        let Some(launcher) = self.launcher.take() else {
            return;
        };
        if launcher.keeps_draft() {
            self.launch_draft = launcher.draft();
            self.draft_starting = starting && self.launch_draft.is_some();
        }
    }

    /// A session the event loop was asked to start has started, or
    /// couldn't. Started from the new-session panel's draft, the draft goes;
    /// not started, it's kept for the next `n` to bring back.
    pub fn start_done(&mut self, started: bool) {
        if std::mem::take(&mut self.draft_starting) && started {
            self.launch_draft = None;
        }
    }

    /// Pasted text: into whichever text box has the keyboard, or else to
    /// the session in the pane that has it. Anywhere else, like the
    /// sidebar, where letters are commands, a paste does nothing.
    pub fn on_paste(&mut self, text: String) -> Option<Action> {
        if self.plugin_pane.is_some() {
            return Some(Action::PasteInPluginPane(text));
        }
        if let Some(View::Files(finder)) = &mut self.view {
            let outcome = finder.on_paste(&text);
            return self.follow(outcome);
        }
        if let Some(View::Tree(tree)) = &mut self.view {
            let outcome = tree.on_paste(&text);
            return self.follow(outcome);
        }
        if let Some(View::Grep(grep)) = &mut self.view {
            let outcome = grep.on_paste(&text);
            return self.follow(outcome);
        }
        if let Some(View::Branches(switcher)) = &mut self.view {
            switcher.on_paste(&text);
            return None;
        }
        if let Some(View::Diff(diff)) = &mut self.view {
            diff.on_paste(&text);
            return None;
        }
        if let Some(View::Memory(memory)) = &mut self.view {
            let outcome = memory.on_paste(&text);
            return self.follow(outcome);
        }
        if self.view.is_some() || self.showing_keys() || self.confirm.is_some() {
            return None;
        }
        if let Some(reply) = &mut self.reply {
            reply.on_paste(&text);
        } else if let Some(list) = &mut self.command_list {
            list.on_paste(&text);
        } else if let Some(launcher) = &mut self.launcher {
            launcher.on_paste(&text);
        } else if let Some(view) = &mut self.profiles_view {
            view.on_paste(&text);
        } else if let Some(prompt) = &mut self.prompt {
            prompt.input.insert_str(&text);
        } else if self.needs_you.is_some() {
            return None;
        } else if let Some(view) = &mut self.timeline {
            view.on_paste(&text);
        } else if let Some(issues) = &mut self.issues {
            issues.on_paste(&text);
        } else if let Some(view) = &mut self.pull_requests_view {
            view.on_paste(&text);
        } else if let Some(backlog) = &mut self.backlog {
            backlog.on_paste(&text);
        } else if let Some(view) = &mut self.layouts {
            view.on_paste(&text);
        } else if let Some(filter) = &mut self.filter {
            filter.input.insert_str(&text);
            self.keep_filter_bar_on_a_match();
        } else if let Focus::Pane(slot) = self.focus {
            return Some(Action::Paste { to: slot, text });
        } else if let Focus::Copy(slot) = self.focus {
            return Some(Action::CopyPaste { slot, text });
        }
        None
    }

    fn ask(&mut self, question: Question, answer: &str) {
        self.prompt = Some(Prompt {
            question,
            input: TextInput::with_text(answer),
        });
    }

    /// Keys while a question is asked: Enter answers it, Esc gives up, and
    /// every other key edits the answer.
    fn on_prompt_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                // Nothing starts from the new-session panel's command line.
                self.draft_starting = false;
                None
            }
            KeyCode::Enter => {
                let prompt = self.prompt.take()?;
                self.answer(prompt)
            }
            _ => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.input.on_key(&key);
                }
                None
            }
        }
    }

    /// What an answered question leads to: a command line, to a new
    /// session; a name, to a rename.
    fn answer(&mut self, prompt: Prompt) -> Option<Action> {
        let answer = prompt.input.text().trim().to_string();
        match prompt.question {
            Question::Command(place) => match command_line::parse(&answer) {
                Ok(command) => {
                    // As with `crystal new`, an agent given only a prompt is
                    // given a task.
                    let purpose = Purpose {
                        task: catalog::first_prompt_in(&command).filter(|_| self.tasks_on),
                        ..Purpose::default()
                    };
                    Some(Action::Start {
                        place,
                        command,
                        purpose,
                    })
                }
                Err(err) => {
                    self.draft_starting = false;
                    self.notify(err);
                    None
                }
            },
            Question::CloseTask { name, failed } => Some(Action::CloseTask {
                name,
                failed,
                summary: answer,
            }),
            Question::SendFlowBack(run) => Some(Action::SendFlowBack { run, notes: answer }),
            Question::TabName => {
                self.tabs.rename(&answer);
                None
            }
            // An empty answer, or the name it already has, changes nothing.
            Question::Rename(name) => {
                if answer.is_empty() || answer == name {
                    None
                } else {
                    Some(Action::Rename {
                        name,
                        new_name: answer,
                    })
                }
            }
        }
    }

    /// Where a new worktree is made from: the selected session's project,
    /// or `None` for the TUI's own directory when nothing in a repository
    /// is selected.
    fn worktree_base(&self) -> Option<PathBuf> {
        let worktree = self.selection_worktree()?;
        Some(worktree.project_path.clone())
    }

    /// The worktree the selection is in: the selected session's, or the
    /// worktree with no sessions it's on.
    fn selection_worktree(&self) -> Option<&Worktree> {
        match self.selected_empty_worktree() {
            Some(worktree) => Some(worktree),
            None => self.selected()?.worktree.as_ref(),
        }
    }

    /// Every key goes to the pane's session, Tab too, since shells and
    /// agents need it. Kept back are Ctrl+\, which returns to the sidebar,
    /// and Shift+PageUp and Shift+PageDown, how terminals have always
    /// scrolled back: they page through the pane's history. Unshifted, the
    /// page keys go to the session like any other.
    fn on_pane_key(&mut self, slot: Slot, key: KeyEvent) -> Option<Action> {
        // A key the user wrote `direct+` is theirs, not the program's.
        if let Some(bound) = self.keymap.direct(&key) {
            return self.run_bound(bound);
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            _ if self.keymap.is_hand_back(&key) => {
                self.focus = Focus::Sidebar;
                None
            }
            _ if self.keymap.is_prefix(&key) => {
                self.prefixed = true;
                None
            }
            KeyCode::PageUp if shift => Some(Action::PageBack(slot)),
            KeyCode::PageDown if shift => Some(Action::PageForward(slot)),
            _ if self.pane_shows_task(slot) => self.on_task_pane_key(slot, key),
            _ => Some(Action::Type { to: slot, key }),
        }
    }

    /// Opens the selected background task in a terminal: Claude Code picks
    /// its conversation up there, and its task goes on in it.
    fn task_to_terminal(&mut self) -> Option<Action> {
        let session = self.selected()?;
        if session.front != Some(Front::Task) {
            let name = session.name.clone();
            self.notify(format!("{name} isn't a background task"));
            return None;
        }
        Some(Action::TaskToTerminal(session.name.clone()))
    }

    /// Whether the pane at `slot` shows a background task, which takes no
    /// keys but its own.
    pub fn pane_shows_task(&self, slot: Slot) -> bool {
        self.pane_session(slot)
            .is_some_and(|session| session.front == Some(Front::Task))
    }

    /// A key in a background task's pane: `y`, `n` or `Y` answer what it
    /// asks for, Space opens the reply box for a follow-up, and Ctrl+C
    /// stops its run. It takes no others.
    fn on_task_pane_key(&mut self, slot: Slot, key: KeyEvent) -> Option<Action> {
        let session = self.pane_session(slot)?;
        let name = session.name.clone();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(Action::Interrupt(name));
        }
        if key.code == KeyCode::Char(' ') && !ctrl {
            self.open_reply(&name);
            return None;
        }
        let answer = self.keymap.mode_key(Mode::Answer, &key).map(answer_of);
        match answer {
            Some(answer) if session.asking.is_some() => Some(Action::Answer { name, answer }),
            Some(_) => {
                self.notify(format!("{name} isn't asking for anything"));
                None
            }
            None => {
                self.notify(format!(
                    "{name} takes no keys: space gives it a follow-up, ctrl+c stops its run"
                ));
                None
            }
        }
    }

    /// Every key goes to copy mode, but Ctrl+\, which leaves it for the
    /// sidebar.
    fn on_copy_key(&mut self, slot: Slot, key: KeyEvent) -> Option<Action> {
        if self.keymap.is_hand_back(&key) {
            self.focus = Focus::Sidebar;
            return None;
        }
        Some(Action::CopyKey { slot, key })
    }

    /// `v`: turns copy mode on in the pane that shows the selected session,
    /// and hands it the keyboard. A session that's ended can still be
    /// copied from.
    fn start_copying(&mut self) {
        let Some(slot) = self.selected_slot() else {
            return;
        };
        if self.shows_screen(slot) {
            self.copied_from = None;
            self.focus = Focus::Copy(slot);
        } else if self.selected_is_own() {
            self.notify("crystal can't show the session it runs in".into());
        }
    }

    /// `e`: opens what the selected session's pane shows, its history and
    /// all, in the user's editor, as a session of its own beside it. One
    /// that's ended can still be read.
    fn edit_history(&mut self) -> Option<Action> {
        let slot = self.selected_slot()?;
        if !self.shows_screen(slot) {
            if self.selected_is_own() {
                self.notify("crystal can't show the session it runs in".into());
            }
            return None;
        }
        let session = self.pane_session(slot)?;
        let dir = session.cwd.clone();
        let name = self.free_name(&format!("{}-history", session.name));
        Some(Action::EditHistory { slot, dir, name })
    }

    /// What the mouse selected in the pane at `slot` is kept to be copied
    /// with copy mode's keys: copy mode has the keyboard there, to give it
    /// back to the pane it was typing into, if it was, when it's over.
    pub fn hold_selection(&mut self, slot: Slot) {
        if !self.shows_screen(slot) || self.focus == Focus::Copy(slot) {
            return;
        }
        self.copied_from = (self.focus == Focus::Pane(slot)).then_some(slot);
        self.focus = Focus::Copy(slot);
    }

    /// Copy mode is over: the keyboard goes back to where it came from,
    /// the sidebar, or the pane the mouse took it from.
    pub fn stop_copying(&mut self) {
        if let Focus::Copy(_) = self.focus {
            self.focus = match self.copied_from.take() {
                Some(slot) if self.can_type_into(slot) => Focus::Pane(slot),
                _ => Focus::Sidebar,
            };
        }
    }

    /// `z`: zooms the selected session's pane, so it takes the room of the
    /// sidebar and the other panes, or puts them back. The keyboard stays in
    /// the sidebar, so `j` and `k` go on choosing the session it shows.
    fn toggle_zoom(&mut self) {
        let tab = self.tabs.current_mut();
        tab.zoomed = !tab.zoomed;
    }

    /// `s`: splits the selected session off into a pane of its own, beside
    /// its pane when there's room for both side by side or else below it,
    /// or closes its split if it has one.
    fn toggle_split(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        if self.is_split(&selected.name) {
            self.close_pane();
        } else {
            self.split_pane(self.way_to_split(), HALF);
        }
    }

    /// The way `s` splits the selection's pane: side by side while both
    /// sides would be at least [`SIDE_BY_SIDE_WIDTH`] wide, or else one
    /// above the other.
    fn way_to_split(&self) -> Way {
        let pane = self.panes().area_of(&Pane::Selection, self.tiles);
        let wide = pane.is_some_and(|pane| pane.width > 2 * SIDE_BY_SIDE_WIDTH);
        if wide { Way::Right } else { Way::Down }
    }

    /// `|` and `-`, and `s`: splits the selected session's pane in two,
    /// `way`, the first side keeping `ratio` of its room. The session
    /// stays where it is, in a pane of its own now, and the pane that
    /// follows the selection takes the other side, to show the next
    /// session selected; a session floating comes down where the
    /// selection's pane was. One split off already keeps its pane, and the
    /// selection's pane comes beside it, leaving what it showed where it
    /// was, split off.
    pub fn split_pane(&mut self, way: Way, ratio: f32) {
        let Some(selected) = self.selected() else {
            return;
        };
        if self.selected_is_own() {
            return self.notify("crystal can't show the session it runs in".into());
        }
        let name = selected.name.clone();
        let at = Pane::Session(name.clone());
        let split_off = self.is_split(&name);
        let splitting = if split_off { &at } else { &Pane::Selection };
        if !self.panes().has_room(splitting, way, self.tiles) {
            let place = match way {
                Way::Right => "beside",
                Way::Down => "below",
            };
            return self.notify(format!("no room for another pane {place} {name}"));
        }
        if self.is_floating(&name) {
            self.put_float_back();
        }
        let stays = if split_off {
            let shown = self.shown().filter(|shown| !self.is_own(shown));
            shown.map(|shown| Pane::Session(shown.name.clone()))
        } else {
            Some(at.clone())
        };
        self.change_panes(|panes| {
            match stays {
                Some(stays) => panes.replace(&Pane::Selection, stays),
                None => panes.close(&Pane::Selection),
            };
            panes.split(&at, way, ratio, Pane::Selection);
        });
    }

    /// Closes the selected session's split, when it has one: the pane
    /// beside it takes the room, and the selection's pane shows the
    /// session again. The selection's own pane stays.
    pub fn close_pane(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        let name = selected.name.clone();
        if !self.is_split(&name) {
            return self.notify("the selection's pane stays: it shows what you select".into());
        }
        self.change_panes(|panes| {
            panes.close(&Pane::Session(name));
        });
    }

    /// `F`: floats the selected session over the panes, in a pane of its
    /// own that takes the keyboard, or puts back the one that floats. A
    /// session split off comes up out of its split.
    fn toggle_float(&mut self) {
        if self.tabs.current().floating.is_some() {
            return self.put_float_back();
        }
        let Some(selected) = self.selected() else {
            return;
        };
        if self.selected_is_own() {
            return self.notify("crystal can't show the session it runs in".into());
        }
        let name = selected.name.clone();
        let split = Pane::Session(name.clone());
        self.change_panes(|panes| {
            panes.close(&split);
        });
        self.tabs.current_mut().floating = Some(name);
        if self.can_type_into(Slot::Float) {
            self.focus_pane(Slot::Float);
        }
    }

    /// Puts the session that floats back among the others. The keyboard
    /// goes back to the sidebar if it was in the float.
    fn put_float_back(&mut self) {
        self.tabs.current_mut().floating = None;
        if matches!(
            self.focus,
            Focus::Pane(Slot::Float) | Focus::Copy(Slot::Float)
        ) {
            self.focus = Focus::Sidebar;
        }
        if self.last_pane == Some(Slot::Float) {
            self.last_pane = None;
        }
    }

    /// The pane that shows the selected session, as `H`, `J`, `K`, `L`,
    /// Shift and the arrows, and `R` find it: in the tab's tree, so not the
    /// float, and not while zoomed, which hides the others. Without a
    /// selected session, the selection's pane. The footer says why not.
    fn selected_pane(&mut self) -> Option<Pane> {
        let slot = self.selected_slot().unwrap_or(Slot::Selected);
        if self.zoomed() {
            self.notify("zoomed: z puts the panes back first".into());
            return None;
        }
        if slot == Slot::Float {
            self.notify("it floats: F puts it back among the panes".into());
            return None;
        }
        self.pane_of(slot)
    }

    /// `H`, `J`, `K` and `L`: swaps the selected session's pane with the
    /// one beside it `toward`: left, down, up or right. At the edge it
    /// stays.
    fn move_pane(&mut self, toward: Direction) {
        let Some(pane) = self.selected_pane() else {
            return;
        };
        if self.tiled().len() == 1 {
            return self.notify("one pane: s splits a session off into another".into());
        }
        let Some(other) = self.panes().neighbour(&pane, toward, self.tiles).cloned() else {
            return;
        };
        self.change_panes(|panes| {
            panes.swap(&pane, &other);
        });
    }

    /// Swaps the panes at `a` and `b`, wherever they are.
    fn swap_panes(&mut self, a: Slot, b: Slot) {
        if let (Some(a), Some(b)) = (self.pane_of(a), self.pane_of(b)) {
            self.change_panes(|panes| {
                panes.swap(&a, &b);
            });
        }
    }

    /// Shift and an arrow: selects the session in the pane beside the
    /// selected session's `toward`, the way `j` and `k` would, so the keys
    /// and the panes that show the selection follow. The selection's pane
    /// is the session it shows. At the edge it stays.
    pub fn focus_toward(&mut self, toward: Direction) {
        let Some(pane) = self.selected_pane() else {
            return;
        };
        let Some(next) = self.panes().neighbour(&pane, toward, self.tiles) else {
            return;
        };
        let session = match next {
            Pane::Session(name) => Some(name.clone()),
            Pane::Selection => {
                let shown = self.pane_session(Slot::Selected);
                let shown = shown.filter(|shown| !self.has_own_pane(&shown.name));
                shown.map(|shown| shown.name.clone())
            }
        };
        match session {
            Some(name) => self.select(&name),
            None => {
                self.notify("the selection's pane is empty: j and k choose what it shows".into())
            }
        }
    }

    /// `R`: resize mode, until `Esc`: the keys move the selected session's
    /// pane's borders.
    fn start_resizing(&mut self) {
        if self.selected_pane().is_none() {
            return;
        }
        if self.tiled().len() == 1 {
            return self.notify("one pane: s splits a session off into another".into());
        }
        self.resizing = true;
    }

    /// Keys in resize mode: `h` `j` `k` `l` or the arrows move a border of
    /// the selected session's pane that way, the keys that select the pane
    /// that way (Shift and an arrow) go on to it, `=` evens the panes out,
    /// and `Esc`, `Enter`, `q` or resize's own key again are done, unless
    /// `[keys]` says other keys. Other keys do nothing.
    fn on_resize_key(&mut self, key: KeyEvent) {
        let toward = match self.keymap.mode_key(Mode::Resize, &key) {
            Some(ModeKey::ResizeLeft) => Direction::Left,
            Some(ModeKey::ResizeDown) => Direction::Down,
            Some(ModeKey::ResizeUp) => Direction::Up,
            Some(ModeKey::ResizeRight) => Direction::Right,
            Some(ModeKey::ResizeEven) => return self.equalize_panes(),
            Some(ModeKey::ResizeDone) => {
                self.resizing = false;
                return;
            }
            _ => {
                match self.keymap.command(&key) {
                    Some(Command::Resize) => self.resizing = false,
                    Some(Command::PaneLeft) => self.focus_toward(Direction::Left),
                    Some(Command::PaneDown) => self.focus_toward(Direction::Down),
                    Some(Command::PaneUp) => self.focus_toward(Direction::Up),
                    Some(Command::PaneRight) => self.focus_toward(Direction::Right),
                    _ => {}
                }
                return;
            }
        };
        self.resize_pane(toward, resize_step(toward));
    }

    /// Moves a border of the selected session's pane `cells` columns or
    /// rows `toward`: the one on that side of it, which it grows into, or
    /// else the one on its other side, which it shrinks from. No pane gets
    /// smaller than it can be drawn.
    pub fn resize_pane(&mut self, toward: Direction, cells: u16) {
        let Some(pane) = self.selected_pane() else {
            return;
        };
        let tiles = self.tiles;
        self.tabs
            .current_mut()
            .panes
            .resize(&pane, toward, cells, tiles);
    }

    /// `=` in resize mode: gives the panes in a line the same room each.
    pub fn equalize_panes(&mut self) {
        self.tabs.current_mut().panes.equalize();
    }

    /// Changes the panes of the tab in front. A split is counted by where
    /// it's drawn, so the keyboard, Tab, a drag and a pane taken by its
    /// header follow each split to its new place, and let go of one that
    /// closed.
    fn change_panes(&mut self, change: impl FnOnce(&mut SplitTree)) {
        let before: Vec<String> = self.splits().into_iter().map(String::from).collect();
        change(&mut self.tabs.current_mut().panes);
        let moved = |slot: Slot| match slot {
            Slot::Split(index) => self.slot_of(&Pane::Session(before.get(index)?.clone())),
            Slot::Selected | Slot::Float => Some(slot),
        };
        let focus = match self.focus {
            Focus::Pane(slot) => moved(slot).map_or(Focus::Sidebar, Focus::Pane),
            Focus::Copy(slot) => moved(slot).map_or(Focus::Sidebar, Focus::Copy),
            Focus::Sidebar => Focus::Sidebar,
        };
        let last_pane = self.last_pane.and_then(moved);
        let dragging = self.dragging.and_then(moved);
        let holding_thumb = self.holding_thumb.and_then(moved);
        let copied_from = self.copied_from.and_then(moved);
        let grabbed = self.grabbed.and_then(|grab| {
            let from = moved(grab.from)?;
            let over = grab.over.and_then(moved);
            Some(Grab { from, over })
        });
        self.focus = focus;
        self.last_pane = last_pane;
        self.dragging = dragging;
        self.holding_thumb = holding_thumb;
        self.copied_from = copied_from;
        self.grabbed = grabbed;
    }

    /// `t`: makes a new tab, brings it to the front, and starts a shell in
    /// it, in the selected session's directory: a new tab is somewhere to
    /// start new work, and a shell is where that starts.
    fn new_tab(&mut self) -> Option<Action> {
        let index = self.tabs.add();
        let followed = self.selected().map(|session| session.cwd.clone());
        let dir = self.start_dir.clone().or(followed);
        self.go_to_tab(index);
        Some(Action::Start {
            place: Place::Directory(dir),
            command: Vec::new(),
            purpose: Purpose::default(),
        })
    }

    /// `>`: asks, on the footer, which tab to move the selected session to.
    fn ask_where_to_move(&mut self) {
        if let Some(selected) = self.selected() {
            self.moving = Some(selected.name.clone());
        }
    }

    /// Moves the session called `name` to the tab `key` names: a digit for
    /// that tab, `t` for a new one. It leaves the sidebar, and the tab in
    /// front stays in front. Any other key leaves it where it is.
    fn move_to_tab(&mut self, name: &str, key: KeyCode) {
        let to = match key {
            KeyCode::Char(digit @ '1'..='9') => digit as usize - '1' as usize,
            KeyCode::Char('t') => self.tabs.add(),
            _ => return,
        };
        let number = to + 1;
        if to == self.tabs.current_index() {
            return self.notify(format!("{name} is in tab {number} already"));
        }
        if to >= self.tabs.all().len() {
            return self.notify(format!("there's no tab {number}"));
        }
        self.move_session(name, to);
        self.notify(format!("moved {name} to tab {number}"));
    }

    /// Moves the session called `name` to the tab at `to`, out of its pane
    /// in the tab it was in. Leaving the tab in front, the keyboard and the
    /// selection stay in that tab.
    fn move_session(&mut self, name: &str, to: usize) {
        if self.tabs.tab_of(name) == Some(to) {
            return;
        }
        if self.tabs.current().holds(name) {
            // Closing its split here first moves the keyboard with the
            // panes.
            let split = Pane::Session(name.to_string());
            self.change_panes(|panes| {
                panes.close(&split);
            });
            if self.is_floating(name) {
                self.put_float_back();
            }
        }
        self.tabs.put(name, to);
        self.keep_selection_in_tab();
    }

    /// The session `>` is moving to another tab, while the footer asks
    /// which.
    pub fn moving(&self) -> Option<&str> {
        self.moving.as_deref()
    }

    /// Brings the tab at `index` to the front.
    fn go_to_tab(&mut self, index: usize) {
        if index == self.tabs.current_index() {
            return;
        }
        self.leave_tab();
        if self.tabs.go_to(index) {
            self.arrive_at_tab();
        } else {
            self.notify(format!("there's no tab {}", index + 1));
        }
    }

    /// `{` and `}`: moves the tab in front one place to the left, `by` -1,
    /// or to the right, `by` 1, past its neighbor. It stays in front.
    fn shift_tab(&mut self, by: isize) {
        let from = self.tabs.current_index();
        let Some(to) = from.checked_add_signed(by) else {
            return self.notify("this tab is the first already".into());
        };
        if !self.tabs.move_tab(from, to) {
            self.notify("this tab is the last already".into());
        }
    }

    /// `&`: closes the tab in front. Its sessions go with it, so a tab with
    /// some asks first; an empty one closes at once. The session this TUI
    /// runs in is never killed with it: it joins the tab in front instead.
    fn close_tab(&mut self) {
        if self.tabs.all().len() == 1 {
            self.notify("this is the only tab".into());
            return;
        }
        let sessions: Vec<String> = self
            .in_tab()
            .into_iter()
            .map(|index| &self.sessions[index])
            .filter(|session| Some(&session.id) != self.own_id.as_ref())
            .map(|session| session.name.clone())
            .collect();
        if sessions.is_empty() {
            self.close_tab_in_front();
        } else {
            let number = self.tabs.current_index() + 1;
            self.confirm = Some(Confirm::CloseTab { number, sessions });
        }
    }

    /// Closes the tab in front, and brings the one that takes its place to
    /// the front.
    fn close_tab_in_front(&mut self) {
        if self.tabs.close() {
            self.place_sessions();
            self.arrive_at_tab();
        }
    }

    /// Notes where the tab in front was left: which session was selected.
    fn leave_tab(&mut self) {
        self.tabs.current_mut().selected = self.selected_name();
    }

    /// Selects the session the tab now in front was on, if it's still in
    /// it, or else its first. The keyboard goes back to the sidebar: the
    /// panes it could have been in have gone.
    fn arrive_at_tab(&mut self) {
        let in_tab = self.in_tab();
        let remembered = self.tabs.current().selected.as_deref();
        let was_on = remembered
            .and_then(|name| self.position(name))
            .filter(|index| in_tab.contains(index));
        if let Some(index) = was_on.or(in_tab.first().copied()) {
            self.selected = index;
        }
        self.on_worktree = None;
        self.focus = Focus::Sidebar;
        self.last_pane = None;
    }

    /// Moves the keyboard from the sidebar to the next pane that takes
    /// keys, going on from the one it was in last.
    fn move_to_pane(&mut self, round: Round) {
        if let Some(slot) = self.next_pane(round) {
            self.focus_pane(slot);
        }
    }

    /// The next pane that takes keys, going round the panes in the order
    /// they're drawn, from just past the one used last. With none used
    /// yet, going forward starts at the first pane, and going back at the
    /// last.
    fn next_pane(&self, round: Round) -> Option<Slot> {
        let slots = self.slots();
        let count = slots.len();
        let last = self
            .last_pane
            .and_then(|last| slots.iter().position(|slot| *slot == last));
        (1..=count)
            .map(|step| match (round, last) {
                (Round::Forward, Some(last)) => (last + step) % count,
                (Round::Back, Some(last)) => (last + count - step) % count,
                (Round::Forward, None) => step - 1,
                (Round::Back, None) => count - step,
            })
            .map(|index| slots[index])
            .find(|slot| self.can_type_into(*slot))
    }

    fn focus_pane(&mut self, slot: Slot) {
        self.focus = Focus::Pane(slot);
        self.last_pane = Some(slot);
    }

    /// Moves the selection `by` rows up or down the sidebar, over the rows
    /// it can be on: the sessions, the worktrees with none, and the
    /// headings of folded projects. It stops at the ends.
    fn move_selection(&mut self, by: isize) {
        let stops: Vec<Row> = self
            .rows()
            .into_iter()
            .filter(|row| match row {
                Row::Session(_) | Row::NoSessions(_) => true,
                Row::Project { path, .. } => self.is_folded(path),
                _ => false,
            })
            .collect();
        if stops.is_empty() {
            return;
        }
        // With nothing selected, as in a tab with only quiet projects, the
        // first move lands on the first row, or the last going up.
        let to = match stops.iter().position(|row| self.is_selected(row)) {
            Some(at) => at.saturating_add_signed(by).min(stops.len() - 1),
            None if by < 0 => stops.len() - 1,
            None => 0,
        };
        self.select_row(&stops[to]);
    }

    /// Whether the selection is on `row`: a session's, a worktree's with no
    /// sessions, or the heading of the folded project it's in.
    fn is_selected(&self, row: &Row) -> bool {
        match row {
            Row::Session(index) => self.selected_index() == Some(*index),
            Row::NoSessions(path) => self.on_worktree.as_ref() == Some(path),
            Row::Project { path, .. } => self.folded_selection() == Some(path.as_path()),
            _ => false,
        }
    }

    /// Puts the selection on `row`, if it's one it can be on. On a folded
    /// project's heading, it goes to the first row the heading stands for,
    /// out of sight.
    fn select_row(&mut self, row: &Row) {
        match row {
            Row::Session(index) | Row::Task(index) | Row::Line(index) => {
                self.selected = *index;
                self.on_worktree = None;
            }
            Row::NoSessions(path) => self.on_worktree = Some(path.clone()),
            Row::Project { path, .. } if self.is_folded(path) => {
                if let Some(first) = self.first_under(path) {
                    self.select_row(&first);
                }
            }
            _ => {}
        }
    }

    /// The first row under the heading of the project at `project`, folded
    /// or not, that the selection can be on.
    fn first_under(&self, project: &Path) -> Option<Row> {
        let rows = self.all_rows();
        let heading = rows
            .iter()
            .position(|row| matches!(row, Row::Project { path, .. } if path == project))?;
        rows[heading + 1..]
            .iter()
            .take_while(|row| !matches!(row, Row::Project { .. } | Row::OutsideGit))
            .find(|row| matches!(row, Row::Session(_) | Row::NoSessions(_)))
            .cloned()
    }

    /// Selects the next session that needs the user, bringing its tab to
    /// the front, or says there's none.
    fn select_next_needing_user(&mut self) {
        match self.next_needing_user() {
            Some(index) => {
                let name = self.sessions[index].name.clone();
                self.select(&name);
            }
            None => self.notify("nothing needs you".to_string()),
        }
    }

    /// `U`: opens the list of everything waiting on the user.
    fn open_needs_you(&mut self) {
        self.needs_you = Some(NeedsYouView::new(self.needs_you_rows()));
    }

    /// Keys while the needs-you view is open: all of them are its, but
    /// those of the question `f` asks.
    fn on_needs_you_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.needs_you.as_mut()?.on_key(&key) {
            needs_you::Step::Stay => None,
            needs_you::Step::Close => {
                self.needs_you = None;
                None
            }
            needs_you::Step::Go(name) => {
                self.needs_you = None;
                self.select(&name);
                None
            }
            needs_you::Step::Answer { name, answer } => Some(Action::Answer { name, answer }),
            needs_you::Step::GoOn(run) => Some(Action::ApproveFlow(run)),
            needs_you::Step::SendBack(run) => {
                self.ask(Question::SendFlowBack(run), "");
                None
            }
            needs_you::Step::Say(said) => {
                self.notify(said);
                None
            }
        }
    }

    /// `a`: opens the timeline, which marks what's new since the user was
    /// away when the footer has just said what that was, and has the log
    /// read for it.
    fn open_timeline(&mut self) -> Action {
        let after = self.away.take().map(|away| away.after);
        self.timeline = Some(TimelineView::new(after));
        Action::FollowEvents
    }

    /// Keys while the timeline is open: all of them are its.
    fn on_timeline_key(&mut self, key: KeyEvent) -> Option<Action> {
        let view = self.timeline.as_mut()?;
        match view.on_key(&key) {
            timeline::Step::Stay => view.wants_older(true).map(Action::ReadOlderEvents),
            timeline::Step::Close => self.close_timeline(),
            timeline::Step::Go(event) => self.go_to_event(&event),
        }
    }

    fn close_timeline(&mut self) -> Option<Action> {
        self.timeline = None;
        Some(Action::StopFollowing)
    }

    /// Enter on a line of the timeline: goes to the session it's about,
    /// closing the timeline, or says why there's none to go to.
    fn go_to_event(&mut self, event: &Event) -> Option<Action> {
        match self.session_of(event) {
            Some(name) => {
                self.select(&name);
                self.close_timeline()
            }
            None => {
                let notice = match &event.session {
                    Some(about) => format!("{} has gone", about.name),
                    None => format!("{} isn't about a session", event.kind.name()),
                };
                self.notify(notice);
                None
            }
        }
    }

    /// What the session `event` is about is called now: the session with
    /// its id, whatever it has been renamed since; or for a flow run's
    /// event, the session of its latest step that has one.
    fn session_of(&self, event: &Event) -> Option<String> {
        if let Some(about) = &event.session {
            let session = self.sessions.iter().find(|s| s.id == about.id)?;
            return Some(session.name.clone());
        }
        let run = event.flow.as_ref()?;
        let run = self.flows.iter().find(|found| found.name == run.run)?;
        let step = run.steps.iter().rev().find_map(|s| s.session.as_deref())?;
        self.position(step).map(|_| step.to_string())
    }

    /// The next session that needs the user, in any tab: one waiting on
    /// them comes before one that's done. See [`Self::sessions_in_turn`].
    fn next_needing_user(&self) -> Option<usize> {
        let in_turn = self.sessions_in_turn();
        for wanted in [Activity::Waiting, Activity::Done] {
            let found = in_turn.iter().copied().find(|&index| {
                let session = &self.sessions[index];
                session.state == State::Running && session.activity == Some(wanted)
            });
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Every session, by index, in the order `u` looks through them: down
    /// the tab in front from just after the selected one, on through the
    /// other tabs in their order, and round to the selected one last.
    /// Starting after the selection means that pressing the key again
    /// moves on to the next.
    fn sessions_in_turn(&self) -> Vec<usize> {
        let count = self.tabs.all().len();
        let first = self.tabs.current_index();
        let mut in_turn: Vec<usize> = (0..count)
            .flat_map(|step| self.sessions_in((first + step) % count))
            .collect();
        let selected = self.selected_index();
        let at = in_turn.iter().position(|&index| Some(index) == selected);
        if let Some(at) = at {
            in_turn.rotate_left(at + 1);
        }
        in_turn
    }

    /// Only a pane that shows a running session takes keys.
    pub fn can_type_into(&self, slot: Slot) -> bool {
        let running = self
            .pane_session(slot)
            .is_some_and(|session| session.state == State::Running);
        running && self.shows_screen(slot)
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.name == name)
    }
}

/// Whether what a forge answered about `project`, asked at `asked`, is to
/// be taken: unless what's kept was asked later, as `kept` says. Taken, it
/// is what's kept from then on.
fn latest_asked(kept: &mut HashMap<PathBuf, Instant>, project: &Path, asked: Instant) -> bool {
    if kept.get(project).is_some_and(|kept| asked < *kept) {
        return false;
    }
    kept.insert(project.to_path_buf(), asked);
    true
}

/// How far a key in resize mode moves a border `toward`.
fn resize_step(toward: Direction) -> u16 {
    match toward {
        Direction::Left | Direction::Right => RESIZE_COLUMNS,
        Direction::Up | Direction::Down => RESIZE_ROWS,
    }
}

/// The answer an answer key gives a permission a background task asks
/// for: `y` yes, `n` no, `Y` yes always, unless `[keys]` says other keys.
fn answer_of(key: ModeKey) -> Answer {
    match key {
        ModeKey::AnswerNo => Answer::Deny,
        ModeKey::AnswerAlways => Answer::Always,
        _ => Answer::Allow,
    }
}

/// Which way a key turns the list of keys' pages: on, or back.
fn page_turn(code: KeyCode) -> Option<isize> {
    match code {
        KeyCode::Right | KeyCode::PageDown | KeyCode::Char(' ' | 'l') => Some(1),
        KeyCode::Left | KeyCode::PageUp | KeyCode::Char('h') => Some(-1),
        _ => None,
    }
}

/// A worktree the way the panel names it: `payments ⌂ main` for a
/// project's main worktree, `payments ⎇ fix/login` for a linked one.
fn worktree_label(worktree: &crate::protocol::Worktree) -> String {
    let mark = if worktree.main { "⌂" } else { "⎇" };
    let branch = worktree.branch.as_deref().unwrap_or("(detached)");
    format!("{} {mark} {branch}", worktree.project)
}

/// What a session editing the file at `path` is called: the file's name,
/// with no spaces, which session names can't have.
fn edit_name(path: &str) -> String {
    let name = match Path::new(path).file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.to_string(),
    };
    name.replace(char::is_whitespace, "-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Activity, BacklogItem, TaskInfo, TaskOutcome, TaskState, Worktree};
    use crossterm::event::KeyModifiers;
    use std::time::Duration;

    fn session(name: &str) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            front: None,
            name: name.into(),
            id: name.into(),
            command: vec!["sh".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state: State::Running,
            activity: None,
            worktree: None,
            changed: 0,
            task: None,
            asking: None,
            reporter: None,
            subagents: 0,
            model: None,
            line: None,
            bell: false,
            unseen_copies: 0,
            context: None,
            output_waits: 0,
        }
    }

    fn ended(name: &str) -> SessionInfo {
        SessionInfo {
            front: None,
            state: State::Exited { code: 0 },
            ..session(name)
        }
    }

    fn app_with(names: &[&str]) -> App {
        let mut app = App::new(None);
        app.set_sessions(names.iter().map(|name| session(name)).collect());
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn selected_name(app: &App) -> Option<&str> {
        app.selected().map(|session| session.name.as_str())
    }

    fn doing(name: &str, activity: Activity) -> SessionInfo {
        SessionInfo {
            front: None,
            activity: Some(activity),
            ..session(name)
        }
    }

    #[test]
    fn something_is_working_only_while_an_agent_works() {
        let mut app = App::new(None);
        app.set_sessions(vec![session("quiet"), doing("finished", Activity::Done)]);
        assert!(!app.anything_working());
        app.set_sessions(vec![session("quiet"), doing("busy", Activity::Working)]);
        assert!(app.anything_working());
    }

    #[test]
    fn u_goes_to_a_session_waiting_on_the_user_before_one_thats_done() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            doing("finished", Activity::Done),
            session("quiet"),
            doing("asking", Activity::Waiting),
        ]);
        app.select("quiet");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(selected_name(&app), Some("asking"));
    }

    #[test]
    fn u_again_moves_on_to_the_next_and_round() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            doing("one", Activity::Waiting),
            session("rest"),
            doing("two", Activity::Waiting),
        ]);
        app.select("rest");
        let mut visited = Vec::new();
        for _ in 0..3 {
            press(&mut app, KeyCode::Char('u'));
            visited.push(selected_name(&app).unwrap().to_string());
        }
        assert_eq!(visited, ["one", "two", "one"]);
    }

    #[test]
    fn done_sessions_are_next_once_nothing_is_waiting() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            doing("a", Activity::Done),
            session("b"),
            doing("c", Activity::Done),
        ]);
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(selected_name(&app), Some("c"));
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn with_nothing_needing_the_user_u_says_so() {
        let mut app = App::new(None);
        let gone = SessionInfo {
            front: None,
            activity: Some(Activity::Done),
            ..ended("gone")
        };
        app.set_sessions(vec![session("a"), gone]);
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(selected_name(&app), Some("a"));
        assert_eq!(app.notice(), Some("nothing needs you"));
    }

    #[test]
    fn ctrl_or_alt_with_a_letter_does_nothing_in_the_sidebar() {
        let mut app = app_with(&["a", "b"]);
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        let alt_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        let ctrl_n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        for key in [ctrl_j, alt_x, ctrl_n] {
            assert_eq!(app.on_key(key), None);
        }
        assert_eq!(selected_name(&app), Some("a"));
        assert!(app.confirm().is_none());
        assert!(app.prompt().is_none());
    }

    #[test]
    fn j_and_k_move_the_selection_and_stop_at_the_ends() {
        let mut app = app_with(&["a", "b", "c"]);
        assert_eq!(selected_name(&app), Some("a"));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(selected_name(&app), Some("a"));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("c"));
        press(&mut app, KeyCode::Up);
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn the_selected_session_stays_selected_when_the_list_changes() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("b");
        app.set_sessions(vec![session("new"), session("a"), session("b")]);
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn when_the_selected_session_goes_the_selection_stays_in_range() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("c");
        app.set_sessions(vec![session("a"), session("b")]);
        assert_eq!(selected_name(&app), Some("b"));

        app.set_sessions(Vec::new());
        assert_eq!(app.selected_index(), None);
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
    }

    #[test]
    fn enter_hands_the_keyboard_to_the_pane_and_ctrl_backslash_takes_it_back() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));

        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let typed = Action::Type {
            to: Slot::Selected,
            key: q,
        };
        assert_eq!(app.on_key(q), Some(typed));

        let ctrl_backslash = KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL);
        assert_eq!(app.on_key(ctrl_backslash), None);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn an_ended_session_takes_no_keys() {
        let mut app = App::new(None);
        app.set_sessions(vec![ended("done")]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_keyboard_comes_back_when_the_session_being_typed_into_ends() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        app.set_sessions(vec![ended("a")]);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_tuis_own_session_is_never_typed_into() {
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![session("me")]);
        assert!(app.selected_is_own());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn a_notice_lasts_until_the_next_key() {
        let mut app = app_with(&["a"]);
        app.notify("no session named b".into());
        assert_eq!(app.notice(), Some("no session named b"));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.notice(), None);
    }

    #[test]
    fn what_a_restart_brought_back_is_said_until_the_next_key_and_kept_in_its_place() {
        let starting = |name: &str| SessionInfo {
            state: State::Starting,
            ..session(name)
        };
        let mut app = App::new(None);
        app.set_sessions(vec![session("api"), starting("docs"), starting("web")]);
        app.select("docs");
        assert_eq!(app.restarted(), None);
        let failed = SessionInfo {
            state: State::Failed {
                why: "its directory, ~/web, isn't there".into(),
            },
            ..session("web")
        };
        app.set_sessions(vec![session("api"), session("docs"), failed]);
        let said = app.restarted().unwrap();
        assert_eq!(
            said.line,
            "after the restart: 1 session back · 1 couldn't start: web"
        );
        assert!(said.failed);
        // Each kept its place, and the selection stayed on its session.
        assert_eq!(selected_name(&app), Some("docs"));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.restarted(), None);
        // Enter on the one that couldn't start offers to start it again.
        assert_eq!(selected_name(&app), Some("web"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.confirm(), Some(&Confirm::Respawn("web".into())));
    }

    #[test]
    fn sessions_waiting_on_the_user_come_first() {
        let mut waiting = session("asks");
        waiting.activity = Some(Activity::Waiting);
        let mut app = App::new(None);
        app.set_sessions(vec![session("a"), session("b"), waiting]);
        let names: Vec<&str> = app.sessions().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["asks", "a", "b"]);
    }

    #[test]
    fn q_asks_to_quit() {
        let mut app = app_with(&["a"]);
        assert_eq!(press(&mut app, KeyCode::Char('q')), Some(Action::Quit));
    }

    #[test]
    fn a_question_mark_shows_the_keys_and_any_key_puts_them_away() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('?'));
        assert!(app.showing_keys());

        // q is read about, not obeyed: the TUI doesn't quit.
        assert_eq!(press(&mut app, KeyCode::Char('q')), None);
        assert!(!app.showing_keys());
    }

    #[test]
    fn the_key_that_closes_the_keys_does_nothing_else() {
        let mut app = app_with(&["a", "b"]);
        for code in [KeyCode::Char('x'), KeyCode::Char('j'), KeyCode::Char('n')] {
            press(&mut app, KeyCode::Char('?'));
            assert_eq!(press(&mut app, code), None);
            assert!(app.confirm().is_none() && app.prompt().is_none());
            assert_eq!(selected_name(&app), Some("a"));
        }
    }

    fn in_project(name: &str, project: &str) -> SessionInfo {
        SessionInfo {
            front: None,
            worktree: Some(Worktree {
                project: project.into(),
                project_path: PathBuf::from(format!("/code/{project}")),
                path: PathBuf::from(format!("/code/{project}")),
                main: true,
                branch: Some("main".into()),
                in_progress: None,
            }),
            ..session(name)
        }
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// Clears the answer the prompt started out with, and types `text`.
    fn answer(app: &mut App, text: &str) {
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(app, text);
    }

    fn prompt_text(app: &App) -> Option<&str> {
        app.prompt().map(|prompt| prompt.input.text())
    }

    /// Starting `command` at `place`, a task when `task` isn't empty.
    fn start(place: Place, command: &[&str], task: &str) -> Option<Action> {
        let command = command.iter().map(|word| word.to_string()).collect();
        let purpose = Purpose {
            task: (!task.is_empty()).then(|| task.to_string()),
            ..Purpose::default()
        };
        Some(Action::Start {
            place,
            command,
            purpose,
        })
    }

    /// An app on a machine where these agents are installed.
    fn with_agents(programs: &[&str], sessions: Vec<SessionInfo>) -> App {
        let mut app = App::new(None);
        let agents = programs.iter().map(|p| catalog::find(p).unwrap()).collect();
        app.set_agents(agents);
        app.set_sessions(sessions);
        app
    }

    fn run_key(app: &App) -> String {
        app.launcher().unwrap().run().key()
    }

    #[test]
    fn n_opens_the_panel_and_enter_starts_where_the_selection_runs() {
        let mut app = with_agents(&["claude"], vec![in_project("agent", "app")]);
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert!(app.launcher().is_some());
        type_text(&mut app, "fix the login bug");
        let place = Place::Directory(Some(PathBuf::from("/code/app")));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(
                place,
                &["claude", "--", "fix the login bug"],
                "fix the login bug"
            )
        );
        assert!(app.launcher().is_none());
        assert_eq!(app.memory().tasks, ["fix the login bug"]);
        assert_eq!(app.memory().last_run.as_deref(), Some("claude"));
    }

    #[test]
    fn keys_go_to_the_panel_while_it_is_open() {
        let mut app = with_agents(&["claude"], vec![session("a"), session("b")]);
        press(&mut app, KeyCode::Char('n'));
        // q and x would quit and kill on the list; here they're the task.
        type_text(&mut app, "qx");
        assert_eq!(app.launcher().unwrap().task().text(), "qx");
        assert_eq!(selected_name(&app), Some("a"));
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert!(app.launcher().is_none());
    }

    #[test]
    fn the_panel_picks_what_ran_last_then_the_configs_choice() {
        let mut app = with_agents(&["claude", "codex"], vec![]);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(run_key(&app), "claude");
        press(&mut app, KeyCode::Esc);

        app.set_launch_settings(&Config {
            new_session: "codex".into(),
            ..Config::default()
        });
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(run_key(&app), "codex");
        press(&mut app, KeyCode::Esc);

        app.set_memory(Memory {
            tasks: Vec::new(),
            last_run: Some("shell".into()),
        });
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(run_key(&app), "shell");
    }

    #[test]
    fn a_new_session_setting_with_arguments_is_offered_as_a_profile() {
        let mut app = with_agents(&["codex"], vec![]);
        app.set_launch_settings(&Config {
            new_session: "codex --full-auto".into(),
            ..Config::default()
        });
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(run_key(&app), "profile:codex --full-auto");
        type_text(&mut app, "go");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(
                Place::Directory(None),
                &["codex", "--full-auto", "--", "go"],
                "go"
            )
        );
    }

    #[test]
    fn profiles_for_agents_not_installed_are_left_out() {
        let mut app = with_agents(&["claude"], vec![]);
        let profile = |name: &str, agent: &str| Profile {
            name: name.into(),
            ..Profile::for_agent(agent)
        };
        app.set_launch_settings(&Config {
            profiles: vec![profile("review", "claude"), profile("fast", "codex")],
            ..Config::default()
        });
        press(&mut app, KeyCode::Char('n'));
        let rows = app.launcher().unwrap().choice_rows();
        assert_eq!(rows[0].2, ["review", "Claude Code", "shell"]);
    }

    #[test]
    fn a_profile_changed_in_the_profiles_view_is_saved_then_offered() {
        let mut app = with_agents(&["claude"], vec![]);
        let review = Profile {
            name: "review".into(),
            ..Profile::for_agent("claude")
        };
        app.set_launch_settings(&Config {
            profiles: vec![review.clone()],
            ..Config::default()
        });
        press(&mut app, KeyCode::Char('P'));
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "er");
        let reviewer = Profile {
            name: "reviewer".into(),
            ..review
        };
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::SaveProfile {
                replacing: Some("review".into()),
                profile: Box::new(reviewer.clone())
            })
        );
        // The event loop wrote it, and read the file again.
        app.profiles_saved(
            &Config {
                profiles: vec![reviewer],
                ..Config::default()
            },
            Some("reviewer"),
        );
        let view = app.profiles_view().unwrap();
        assert!(view.form().is_none());
        assert_eq!(view.profiles()[0].name, "reviewer");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('n'));
        let rows = app.launcher().unwrap().choice_rows();
        assert_eq!(rows[0].2, ["reviewer", "Claude Code", "shell"]);
    }

    #[test]
    fn a_profile_that_couldnt_be_saved_says_why_in_the_view() {
        let mut app = with_agents(&["claude"], vec![]);
        press(&mut app, KeyCode::Char('P'));
        app.profile_failed("two profiles are called review".into());
        assert_eq!(
            app.profiles_view().unwrap().problem(),
            Some("two profiles are called review")
        );
    }

    #[test]
    fn with_profiles_switched_off_they_arent_offered() {
        let mut app = with_agents(&["claude"], vec![]);
        app.set_launch_settings(&Config {
            profiles: vec![Profile {
                name: "review".into(),
                ..Profile::for_agent("claude")
            }],
            ..Config::default()
        });
        app.profiles_on = false;
        press(&mut app, KeyCode::Char('P'));
        assert!(app.profiles_view().is_none());
        assert_eq!(app.notice(), Some(plugins::off("profiles").as_str()));
        press(&mut app, KeyCode::Char('n'));
        let rows = app.launcher().unwrap().choice_rows();
        assert_eq!(rows[0].2, ["Claude Code", "shell"]);
    }

    #[test]
    fn with_no_agent_installed_the_panel_starts_a_shell() {
        let mut app = App::new(None);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(run_key(&app), "shell");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(Place::Directory(None), &[], "")
        );
    }

    #[test]
    fn with_nothing_selected_the_panel_starts_where_the_settings_say() {
        let mut app = App::new(None);
        app.set_start_dir(Some(PathBuf::from("/home/ann/code")));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(
                Place::Directory(Some(PathBuf::from("/home/ann/code"))),
                &[],
                ""
            )
        );
        // A session selected is still where the panel starts.
        let mut app = with_agents(&[], vec![in_project("agent", "app")]);
        app.set_start_dir(Some(PathBuf::from("/home/ann/code")));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(Place::Directory(Some(PathBuf::from("/code/app"))), &[], "")
        );
    }

    #[test]
    fn w_opens_the_panel_on_a_new_worktree_with_a_made_up_name() {
        let mut app = with_agents(&["claude"], vec![in_project("agent", "app")]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "fix typo");
        let branch = app.launcher().unwrap().branch_name();
        assert!(
            matches!(branch.split_once('-'), Some((a, b)) if !a.is_empty() && !b.is_empty()),
            "{branch}"
        );
        let place = Place::NewWorktree {
            branch,
            base: Some(PathBuf::from("/code/app")),
            made_up: true,
        };
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(place, &["claude", "--", "fix typo"], "fix typo")
        );
    }

    #[test]
    fn outside_a_repository_the_worktree_is_made_from_the_tuis_directory() {
        let mut app = with_agents(&["claude"], vec![session("shell")]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "feat");
        let place = Place::NewWorktree {
            branch: app.launcher().unwrap().branch_name(),
            base: None,
            made_up: true,
        };
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(place, &["claude", "--", "feat"], "feat")
        );
    }

    #[test]
    fn other_projects_are_places_to_start_in() {
        let sessions = vec![in_project("a", "app"), in_project("b", "billing")];
        let mut app = with_agents(&["claude"], sessions);
        press(&mut app, KeyCode::Char('n'));
        let rows = app.launcher().unwrap().choice_rows();
        let places = &rows.last().unwrap().2;
        assert_eq!(places, &["here", "new worktree", "billing"]);
    }

    #[test]
    fn the_panel_keeps_a_task_put_away_until_a_session_starts_from_it() {
        let mut app = with_agents(&["claude"], vec![in_project("agent", "app")]);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "fix the login bug");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.launcher().unwrap().task().text(), "fix the login bug");
        assert!(app.launcher().unwrap().shows_draft());

        // A session that couldn't start leaves it for the next `n`.
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Start { .. })
        ));
        app.start_done(false);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.launcher().unwrap().task().text(), "fix the login bug");

        // One that started takes it.
        press(&mut app, KeyCode::Enter);
        app.start_done(true);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.launcher().unwrap().task().is_empty());
        assert!(!app.launcher().unwrap().shows_draft());

        // Emptied and put away, it's gone.
        type_text(&mut app, "x");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.launcher().unwrap().task().is_empty());
    }

    #[test]
    fn the_command_line_keeps_the_panel_s_draft_until_it_starts_a_session() {
        let mut app = with_agents(&["claude"], vec![]);
        let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "fix it");
        app.on_key(ctrl_e);
        press(&mut app, KeyCode::Esc);
        // Another session started meanwhile doesn't take it.
        app.start_done(true);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.launcher().unwrap().task().text(), "fix it");

        app.on_key(ctrl_e);
        assert!(press(&mut app, KeyCode::Enter).is_some());
        app.start_done(true);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.launcher().unwrap().task().is_empty());
    }

    #[test]
    fn ctrl_e_hands_the_command_to_the_command_line() {
        let mut app = with_agents(&["claude"], vec![]);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "fix it");
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(app.launcher().is_none());
        assert_eq!(prompt_text(&app), Some("claude -- 'fix it'"));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(
                Place::Directory(None),
                &["claude", "--", "fix it"],
                "fix it"
            )
        );
    }

    #[test]
    fn a_paste_goes_to_the_panel_or_else_to_the_pane_typed_into() {
        let mut app = with_agents(&["claude"], vec![session("a")]);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.on_paste("one\ntwo".into()), None);
        assert_eq!(app.launcher().unwrap().task().text(), "one\ntwo");
        press(&mut app, KeyCode::Esc);

        // In the sidebar, where its letters would be commands, it's dropped.
        assert_eq!(app.on_paste("xq".into()), None);
        assert!(app.confirm().is_none());

        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.on_paste("ls\n".into()),
            Some(Action::Paste {
                to: Slot::Selected,
                text: "ls\n".into()
            })
        );
    }

    #[test]
    fn codex_is_asked_for_its_models_once() {
        let mut app = with_agents(&["codex"], vec![]);
        assert_eq!(
            press(&mut app, KeyCode::Char('n')),
            Some(Action::ReadCodexModels)
        );
        app.set_codex_models(vec!["gpt-6-luna".into()]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        let rows = app.launcher().unwrap().choice_rows();
        assert_eq!(rows[1].2, ["default", "gpt-6-luna"]);
    }

    #[test]
    fn x_asks_first_and_only_y_kills() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert_eq!(app.confirm(), Some(&Confirm::Kill("b".into())));
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert_eq!(app.confirm(), None);
        assert!(app.prompt().is_none(), "the n answered the question");

        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Kill("b".into()))
        );
    }

    #[test]
    fn x_with_nothing_selected_asks_nothing() {
        let mut app = App::new(None);
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert_eq!(app.confirm(), None);
    }

    #[test]
    fn r_asks_for_a_new_name_starting_from_the_old_one() {
        let mut app = app_with(&["a", "fixer"]);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(prompt_text(&app), Some("fixer"));

        answer(&mut app, "login-fixer");
        let renamed = Action::Rename {
            name: "fixer".into(),
            new_name: "login-fixer".into(),
        };
        assert_eq!(press(&mut app, KeyCode::Enter), Some(renamed));
    }

    #[test]
    fn a_rename_to_nothing_or_the_same_name_changes_nothing() {
        let mut app = app_with(&["fixer"]);
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(press(&mut app, KeyCode::Enter), None);

        press(&mut app, KeyCode::Char('r'));
        answer(&mut app, "");
        assert_eq!(press(&mut app, KeyCode::Enter), None);
    }

    #[test]
    fn a_split_stays_open_when_its_session_is_renamed() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        app.renamed("a", "c");
        app.set_sessions(vec![session("c"), session("b")]);
        assert_eq!(app.splits(), ["c"]);
    }

    #[test]
    fn a_session_renamed_elsewhere_keeps_its_split_and_the_selection() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        let mut renamed = session("fix-login");
        renamed.id = "a".into();
        app.set_sessions(vec![renamed, session("b")]);
        assert_eq!(app.splits(), ["fix-login"]);
        assert_eq!(selected_name(&app), Some("fix-login"));
    }

    #[test]
    fn the_tuis_own_session_is_told_by_its_id_whatever_its_name() {
        let mut renamed = session("renamed");
        renamed.id = "me".into();
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![renamed]);
        assert!(app.selected_is_own());
    }

    #[test]
    fn enter_on_an_ended_session_offers_to_start_it_again() {
        let mut app = App::new(None);
        app.set_sessions(vec![ended("done")]);
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.confirm(), Some(&Confirm::Respawn("done".into())));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Respawn("done".into()))
        );
    }

    /// A session in the worktree of the `app` project on `branch`, the main
    /// one when `branch` is "main".
    fn in_worktree(name: &str, branch: &str, state: State) -> SessionInfo {
        SessionInfo {
            front: None,
            state,
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: PathBuf::from("/code/app"),
                path: PathBuf::from(format!("/code/app.worktrees/{branch}")),
                main: branch == "main",
                branch: Some(branch.into()),
                in_progress: None,
            }),
            ..session(name)
        }
    }

    #[test]
    fn capital_b_switches_a_projects_main_worktree_only() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("fixer", "fix", State::Running),
            in_worktree("other", "main", State::Running),
        ]);
        app.select("fixer");
        assert_eq!(press(&mut app, KeyCode::Char('B')), None);
        assert!(app.notice().unwrap().contains("a linked one stays"));
        app.select("other");
        let main = PathBuf::from("/code/app.worktrees/main");
        assert_eq!(
            press(&mut app, KeyCode::Char('B')),
            Some(Action::ListBranches(main))
        );
        assert!(matches!(app.view(), Some(View::Branches(_))));
    }

    #[test]
    fn a_switch_whose_switcher_has_closed_is_said_at_the_bottom() {
        use crate::git::branches::{Branch, Carry, Outcome as Switch};
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("other", "main", State::Running)]);
        let main = PathBuf::from("/code/app.worktrees/main");
        press(&mut app, KeyCode::Char('B'));
        let current = Branch {
            current: true,
            ..Branch::new("main")
        };
        let listed = switcher::Listed {
            branches: vec![current, Branch::new("feature")],
            changes: Vec::new(),
        };
        app.branches_listed(&main, Ok(listed));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::SwitchBranch {
                dir: main.clone(),
                target: Branch::new("feature"),
                carry: Carry::Ask,
            })
        );
        // Esc while git works closes it, and it won't open again until
        // git is done.
        press(&mut app, KeyCode::Esc);
        assert!(app.view().is_none());
        assert_eq!(press(&mut app, KeyCode::Char('B')), None);
        assert!(app.notice().unwrap().contains("still switching"));

        let done = Switch::Switched {
            branch: "feature".into(),
            note: None,
        };
        assert_eq!(app.branch_switched(&main, done), None);
        assert_eq!(app.notice(), Some("the worktree is on feature"));
        assert!(press(&mut app, KeyCode::Char('B')).is_some());
    }

    #[test]
    fn enter_in_find_in_files_edits_the_file_at_the_line_found() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("fixer", "fix", State::Running)]);
        let dir = PathBuf::from("/code/app.worktrees/fix");
        assert_eq!(press(&mut app, KeyCode::Char('G')), None);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(
            press(&mut app, KeyCode::Char('e')),
            Some(Action::Grep {
                dir: dir.clone(),
                query: "ne".into(),
            })
        );
        let hit = crate::git::Hit {
            path: "src/a.rs".into(),
            line: 4,
            text: "needle".into(),
        };
        let found = crate::git::Found {
            hits: vec![hit],
            more: false,
        };
        assert!(app.searched(&dir, "ne", Ok(found)).is_some());
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Edit {
                dir,
                path: "src/a.rs".into(),
                line: Some(4),
                name: "a.rs".into(),
            })
        );
        assert!(app.view().is_none());
    }

    #[test]
    fn shift_w_asks_before_removing_a_worktree_nothing_runs_in() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("fixer", "fix", State::Exited { code: 0 }),
            in_worktree("other", "main", State::Running),
        ]);
        app.select("fixer");
        press(&mut app, KeyCode::Char('W'));
        let removal = Confirm::RemoveWorktree {
            path: PathBuf::from("/code/app.worktrees/fix"),
            branch: "fix".into(),
            force: false,
        };
        assert_eq!(app.confirm(), Some(&removal));
        assert_eq!(
            app.confirm().unwrap().question(),
            "remove worktree fix? y/n"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(removal_of("fix", false))
        );
    }

    /// Removing the worktree of app on `branch`, forced or not.
    fn removal_of(branch: &str, force: bool) -> Action {
        Action::RemoveWorktree {
            path: PathBuf::from(format!("/code/app.worktrees/{branch}")),
            branch: branch.into(),
            force,
        }
    }

    #[test]
    fn a_worktree_with_changes_is_asked_about_again_before_it_is_forced() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("fixer", "fix", State::Exited { code: 0 })]);
        app.select("fixer");
        // The event loop found changes not committed after the first yes.
        app.ask_to_force_removal("/code/app.worktrees/fix".into(), "fix".into());
        assert_eq!(
            app.confirm().map(Confirm::question).as_deref(),
            Some("fix has uncommitted changes: remove it and lose them? y/n")
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(removal_of("fix", true))
        );

        app.ask_to_force_removal("/code/app.worktrees/fix".into(), "fix".into());
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert_eq!(app.confirm(), None);
    }

    #[test]
    fn killing_the_last_session_in_a_worktree_asks_whether_it_goes_too() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("planner", "main", State::Running),
            in_worktree("fixer", "fix", State::Running),
        ]);
        app.select("fixer");
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Kill("fixer".into()))
        );
        assert_eq!(
            app.confirm().map(Confirm::question).as_deref(),
            Some("nothing else is in worktree fix: remove it too? y/n")
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(removal_of("fix", false))
        );
        assert!(app.removing(Path::new("/code/app.worktrees/fix")));
    }

    #[test]
    fn the_emptied_worktree_stays_unless_it_s_a_yes() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("fixer", "fix", State::Running)]);
        app.select("fixer");
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert_eq!(app.confirm(), None);
        assert!(!app.removing(Path::new("/code/app.worktrees/fix")));
    }

    #[test]
    fn a_worktree_with_something_left_in_it_or_the_main_one_isn_t_asked_about() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("fixer", "fix", State::Running),
            in_worktree("tests", "fix", State::Exited { code: 0 }),
            in_worktree("planner", "main", State::Running),
        ]);
        for name in ["fixer", "planner"] {
            app.select(name);
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(
                press(&mut app, KeyCode::Char('y')),
                Some(Action::Kill(name.into()))
            );
            assert_eq!(app.confirm(), None, "{name}");
        }
    }

    #[test]
    fn shift_w_refuses_while_a_session_runs_in_the_worktree() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("fixer", "fix", State::Exited { code: 0 }),
            in_worktree("tests", "fix", State::Running),
        ]);
        app.select("fixer");
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.confirm(), None);
        assert_eq!(app.notice(), Some("tests still running in fix"));
    }

    #[test]
    fn shift_w_leaves_the_main_worktree_and_sessions_outside_git_alone() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_worktree("planner", "main", State::Running)]);
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.notice(), Some("the main worktree can't be removed"));

        app.set_sessions(vec![session("shell")]);
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.notice(), Some("shell isn't in a git worktree"));
        assert_eq!(app.confirm(), None);
    }

    /// A linked worktree of app on `branch`, as git lists it.
    fn linked(branch: &str) -> Worktree {
        Worktree {
            project: "app".into(),
            project_path: PathBuf::from("/code/app"),
            path: PathBuf::from(format!("/code/app.worktrees/{branch}")),
            main: false,
            branch: Some(branch.into()),
            in_progress: None,
        }
    }

    /// An app with a session in app's main worktree, and a worktree `old`
    /// git lists with no sessions in it.
    fn app_with_an_empty_worktree() -> App {
        let planner = in_worktree("planner", "main", State::Running);
        let mut app = with_agents(&["claude"], vec![planner]);
        app.set_worktrees(PathBuf::from("/code/app"), vec![linked("old")]);
        app
    }

    fn empty_branch(app: &App) -> Option<&str> {
        app.selected_empty_worktree()?.branch.as_deref()
    }

    fn shows_empty_worktree(app: &App) -> bool {
        app.rows()
            .iter()
            .any(|row| matches!(row, Row::NoSessions(_)))
    }

    #[test]
    fn a_worktree_with_no_sessions_stays_in_the_sidebar_and_j_and_k_reach_it() {
        let mut app = app_with_an_empty_worktree();
        assert!(shows_empty_worktree(&app));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(empty_branch(&app), Some("old"));
        assert_eq!(selected_name(&app), None, "no session is selected");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(empty_branch(&app), Some("old"), "it's the last row");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(selected_name(&app), Some("planner"));
        assert_eq!(empty_branch(&app), None);
    }

    #[test]
    fn a_click_on_a_worktree_with_no_sessions_selects_it() {
        let mut app = app_with_an_empty_worktree();
        let row = Row::NoSessions("/code/app.worktrees/old".into());
        let at = app.rows().iter().position(|shown| *shown == row).unwrap();
        app.on_mouse(CLICK, Hit::SidebarRow(at));
        assert_eq!(empty_branch(&app), Some("old"));
    }

    #[test]
    fn shift_w_on_a_worktree_with_no_sessions_asks_to_remove_it() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(
            app.confirm().map(Confirm::question).as_deref(),
            Some("remove worktree old? y/n")
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(removal_of("old", false))
        );
        app.worktree_removed(Path::new("/code/app.worktrees/old"));
        assert!(!shows_empty_worktree(&app));
        assert_eq!(selected_name(&app), Some("planner"));
    }

    #[test]
    fn a_worktree_stays_while_git_removes_it_and_isn_t_asked_about_twice() {
        let mut app = app_with_an_empty_worktree();
        let old = Path::new("/code/app.worktrees/old");
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('W'));
        assert!(!app.removing(old), "not until it's a yes");
        press(&mut app, KeyCode::Char('y'));
        assert!(app.removing(old));
        assert!(shows_empty_worktree(&app), "it's there until git is done");
        assert_eq!(empty_branch(&app), Some("old"));

        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.confirm(), None);
        assert_eq!(app.notice(), Some("already removing old"));

        app.worktree_removed(old);
        assert!(!app.removing(old));
        assert!(!shows_empty_worktree(&app));
    }

    #[test]
    fn a_worktree_git_wouldn_t_remove_can_be_asked_about_again() {
        let mut app = app_with_an_empty_worktree();
        let old = Path::new("/code/app.worktrees/old");
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('W'));
        press(&mut app, KeyCode::Char('y'));
        app.worktree_not_removed(old, "'old' contains modified or untracked files".into());
        assert!(!app.removing(old));
        assert!(shows_empty_worktree(&app));
        assert_eq!(
            app.notice(),
            Some("'old' contains modified or untracked files")
        );

        press(&mut app, KeyCode::Char('W'));
        assert_eq!(
            app.confirm().map(Confirm::question).as_deref(),
            Some("remove worktree old? y/n")
        );
    }

    #[test]
    fn a_worktree_found_with_changes_isn_t_being_removed_until_it_s_forced() {
        let mut app = app_with_an_empty_worktree();
        let old = Path::new("/code/app.worktrees/old");
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('W'));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.removing(old));
        // git found changes not committed, off the loop.
        app.ask_to_force_removal(old.into(), "old".into());
        assert!(!app.removing(old));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::RemoveWorktree {
                path: old.into(),
                branch: "old".into(),
                force: true,
            })
        );
        assert!(app.removing(old));
    }

    #[test]
    fn a_worktree_someone_else_is_removing_says_so_until_the_daemon_is_done() {
        let mut app = app_with_an_empty_worktree();
        let old = Path::new("/code/app.worktrees/old");
        // Asked for by another TUI, or `crystal worktree rm`.
        assert!(!app.set_removals(vec![old.into()]));
        assert!(app.removing(old));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.confirm(), None);
        assert_eq!(app.notice(), Some("already removing old"));

        assert!(!app.set_removals(vec![old.into()]), "still at it");
        assert!(app.set_removals(Vec::new()), "done: git lists them again");
        assert!(!app.removing(old));
        assert!(!app.set_removals(Vec::new()));
    }

    #[test]
    fn a_removal_this_tui_asked_for_says_so_before_the_daemon_lists_it() {
        let mut app = app_with_an_empty_worktree();
        let old = Path::new("/code/app.worktrees/old");
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('W'));
        press(&mut app, KeyCode::Char('y'));
        // Listed before the daemon was asked: git looks for changes first.
        app.set_removals(Vec::new());
        assert!(app.removing(old));

        app.set_removals(vec![old.into()]);
        app.worktree_removed(old);
        assert!(!app.removing(old), "done, though the daemon listed it");
    }

    #[test]
    fn no_to_removing_a_worktree_leaves_it_be() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert!(!app.removing(Path::new("/code/app.worktrees/old")));
    }

    #[test]
    fn n_on_a_worktree_with_no_sessions_starts_there() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "pick it up");
        let place = Place::Directory(Some(PathBuf::from("/code/app.worktrees/old")));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(place, &["claude", "--", "pick it up"], "pick it up")
        );
    }

    fn known(name: &str) -> Worktree {
        Worktree {
            project: name.into(),
            project_path: PathBuf::from(format!("/code/{name}")),
            path: PathBuf::from(format!("/code/{name}")),
            main: true,
            branch: Some("main".into()),
            in_progress: None,
        }
    }

    #[test]
    fn a_known_project_with_no_sessions_stays_in_the_sidebar_and_starts_there() {
        let mut app = app_with_an_empty_worktree();
        app.set_known_projects(vec![known("app"), known("api")]);
        let rows = app.rows();
        // app has a session, so only api is listed on its own, at the end.
        let api = Row::NoSessions(PathBuf::from("/code/api"));
        assert_eq!(rows.last(), Some(&api));
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, Row::Project { .. }))
                .count(),
            2
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            app.selected_empty_worktree().map(|w| w.project.as_str()),
            Some("api")
        );
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "look around");
        let place = Place::Directory(Some(PathBuf::from("/code/api")));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(place, &["claude", "--", "look around"], "look around")
        );
    }

    #[test]
    fn shift_w_on_a_known_project_with_no_sessions_takes_it_off_the_list() {
        let mut app = with_agents(&["claude"], Vec::new());
        app.set_known_projects(vec![known("api")]);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(
            app.confirm().map(Confirm::question).as_deref(),
            Some("take api off the list? Nothing on disk changes. y/n")
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::ForgetProject(PathBuf::from("/code/api")))
        );
    }

    #[test]
    fn the_new_session_panel_offers_every_known_project() {
        let mut app = app_with_an_empty_worktree();
        app.set_known_projects(vec![known("app"), known("api")]);
        let targets = app.launch_targets();
        let projects: Vec<&Path> = targets
            .iter()
            .filter_map(|target| match target {
                Target::Project { path, .. } => Some(path.as_path()),
                _ => None,
            })
            .collect();
        assert_eq!(projects, vec![Path::new("/code/api")]);
    }

    #[test]
    fn a_session_started_in_a_worktree_with_none_takes_its_row_and_the_selection() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('j'));
        app.set_sessions(vec![
            in_worktree("planner", "main", State::Running),
            in_worktree("picker", "old", State::Running),
        ]);
        assert!(!shows_empty_worktree(&app));
        assert_eq!(empty_branch(&app), None);
        assert_eq!(selected_name(&app), Some("picker"));
    }

    #[test]
    fn a_worktree_stays_when_its_last_session_goes_before_git_is_asked() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("planner", "main", State::Running),
            in_worktree("fixer", "fix", State::Running),
        ]);
        app.select("fixer");
        app.set_sessions(vec![in_worktree("planner", "main", State::Running)]);
        assert_eq!(empty_branch(&app), Some("fix"));
        // Git, asked at last, says it has gone.
        app.set_worktrees(PathBuf::from("/code/app"), Vec::new());
        assert!(!shows_empty_worktree(&app));
        assert_eq!(selected_name(&app), Some("planner"));
    }

    #[test]
    fn a_list_from_before_a_kill_doesn_t_lose_the_worktree_it_left() {
        let mut app = App::new(None);
        let before = vec![
            in_worktree("planner", "main", State::Running),
            in_worktree("fixer", "fix", State::Running),
        ];
        app.set_sessions(before.clone());
        app.set_worktrees(PathBuf::from("/code/app"), vec![linked("fix")]);
        app.select("fixer");
        let after = vec![in_worktree("planner", "main", State::Running)];
        app.set_sessions(after.clone());
        // A list asked for just before the kill comes in late.
        app.set_sessions(before);
        app.set_sessions(after);
        assert_eq!(empty_branch(&app), Some("fix"));
    }

    #[test]
    fn killing_the_last_session_in_a_worktree_leaves_the_selection_on_its_row() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_worktree("planner", "main", State::Running),
            in_worktree("fixer", "fix", State::Running),
        ]);
        app.set_worktrees(PathBuf::from("/code/app"), vec![linked("fix")]);
        assert!(!shows_empty_worktree(&app), "fixer is in it");
        app.select("fixer");
        app.set_sessions(vec![in_worktree("planner", "main", State::Running)]);
        assert_eq!(empty_branch(&app), Some("fix"));
    }

    #[test]
    fn a_worktree_with_no_sessions_shows_only_in_tabs_its_project_is_in() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('t'));
        app.set_sessions(vec![
            in_worktree("planner", "main", State::Running),
            session("shell"),
        ]);
        assert!(!shows_empty_worktree(&app), "tab 2 has only a shell");
        press(&mut app, KeyCode::Char('1'));
        assert!(shows_empty_worktree(&app));
    }

    #[test]
    fn the_filter_finds_sessions_and_leaves_worktrees_with_none_out() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('/'));
        assert!(!shows_empty_worktree(&app));
        press(&mut app, KeyCode::Esc);
        assert!(shows_empty_worktree(&app));
    }

    fn hand_back(app: &mut App) {
        app.on_key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL));
    }

    /// An app with `names`, the first `split` of them split off, and the
    /// selection on the last one. At the 80 by 24 terminal an app takes
    /// itself to be on until it's drawn, each is split off below the last,
    /// so they're stacked in order, the selection's pane at the bottom.
    fn app_with_splits(names: &[&str], split: usize) -> App {
        let mut app = app_with(names);
        for _ in 0..split {
            press(&mut app, KeyCode::Char('s'));
            press(&mut app, KeyCode::Char('j'));
        }
        app.select(names[names.len() - 1]);
        app
    }

    #[test]
    fn s_splits_the_selected_session_off_and_again_closes_it() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.splits(), ["a"]);
        // a stays where it was, and the selection's pane goes below it.
        assert_eq!(app.slots(), [Slot::Split(0), Slot::Selected]);

        press(&mut app, KeyCode::Char('s'));
        assert!(app.splits().is_empty());
        assert_eq!(app.panes(), &SplitTree::default());
    }

    #[test]
    fn a_split_stays_on_its_session_while_the_selection_moves() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        let shown = |slot| app.pane_session(slot).map(|session| session.name.as_str());
        assert_eq!(shown(Slot::Selected), Some("b"));
        assert_eq!(shown(Slot::Split(0)), Some("a"));
    }

    #[test]
    fn a_pane_with_no_room_for_another_says_so() {
        let mut app = app_with(&["a", "b"]);
        app.set_tiles(Rect::new(29, 1, 20, 5));
        press(&mut app, KeyCode::Char('s'));
        assert!(app.splits().is_empty());
        assert_eq!(app.notice(), Some("no room for another pane below a"));
        press(&mut app, KeyCode::Char('|'));
        assert_eq!(app.notice(), Some("no room for another pane beside a"));
    }

    /// Where the panes of the tab in front are, laid out in `tiles`, by
    /// the names of the sessions they show, `-` for the selection's pane
    /// while it shows none.
    fn placed(app: &App, tiles: Rect) -> Vec<(String, Rect)> {
        let slots = app.slots().into_iter();
        let names = slots.map(|slot| match app.pane_session(slot) {
            Some(session) if app.shows_screen(slot) => session.name.clone(),
            _ => "-".to_string(),
        });
        let areas = app.panes().layout(tiles).into_iter();
        names.zip(areas.map(|(_, area)| area)).collect()
    }

    const WIDE: Rect = Rect::new(0, 0, 101, 40);

    #[test]
    fn bar_and_dash_split_the_selected_sessions_pane_beside_or_below() {
        let mut app = app_with(&["a", "b", "c"]);
        app.set_tiles(WIDE);
        press(&mut app, KeyCode::Char('|'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            placed(&app, WIDE),
            [
                ("a".to_string(), Rect::new(0, 0, 50, 40)),
                ("b".to_string(), Rect::new(51, 0, 50, 40)),
            ]
        );
        // b, in the selection's pane, stays where it is, and the
        // selection's pane goes below it.
        press(&mut app, KeyCode::Char('-'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.splits(), ["a", "b"]);
        assert_eq!(
            placed(&app, WIDE)[2],
            ("c".to_string(), Rect::new(51, 20, 50, 20))
        );
        assert_eq!(app.notice(), None);
    }

    #[test]
    fn splitting_a_pane_of_its_own_brings_the_selections_pane_beside_it() {
        let mut app = app_with(&["a", "b", "c"]);
        app.set_tiles(WIDE);
        press(&mut app, KeyCode::Char('|'));
        press(&mut app, KeyCode::Char('j'));
        // On a again, the selection's pane goes on showing b. Splitting a's
        // pane below it leaves b where it was, split off, and the
        // selection's pane comes under a, to show the next one selected.
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('-'));
        assert_eq!(app.splits(), ["a", "b"]);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        let names: Vec<String> = (placed(&app, WIDE).into_iter())
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["a", "c", "b"]);
        assert_eq!(
            placed(&app, WIDE)[1].1,
            Rect::new(0, 20, 50, 20),
            "c is under a"
        );
    }

    #[test]
    fn the_selections_pane_goes_on_showing_what_it_showed_while_a_split_is_selected() {
        let mut app = app_with_splits(&["a", "b", "c"], 1);
        assert_eq!(drawn(&app), ["a", "c"]);
        app.select("a");
        assert_eq!(drawn(&app), ["a", "c"]);
        // Nor while a session floats: a comes up out of its split into
        // the float, over the selection's pane, which still shows b.
        app.select("b");
        app.select("a");
        press(&mut app, KeyCode::Char('F'));
        hand_back(&mut app);
        assert_eq!(drawn(&app), ["b", "a"]);
        // With nothing it showed left to show, it says where the selected
        // session is.
        app.set_sessions(vec![session("a"), session("c")]);
        assert_eq!(drawn(&app), ["-", "a"]);
    }

    #[test]
    fn shift_and_an_arrow_select_the_session_in_the_pane_that_way() {
        let mut app = app_with(&["a", "b", "c"]);
        app.set_tiles(WIDE);
        // a | (b over the selection's pane, showing c).
        press(&mut app, KeyCode::Char('|'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('-'));
        press(&mut app, KeyCode::Char('j'));
        let shift = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
        app.on_key(shift(KeyCode::Up));
        assert_eq!(selected_name(&app), Some("b"));
        app.on_key(shift(KeyCode::Left));
        assert_eq!(selected_name(&app), Some("a"));
        // Into the selection's pane is to the session it shows.
        app.on_key(shift(KeyCode::Right));
        assert_eq!(selected_name(&app), Some("b"));
        app.on_key(shift(KeyCode::Down));
        assert_eq!(selected_name(&app), Some("c"));
        assert_eq!(drawn(&app), ["a", "b", "c"]);
        // At the edge it stays.
        app.on_key(shift(KeyCode::Down));
        assert_eq!(selected_name(&app), Some("c"));
        assert_eq!(app.notice(), None);
    }

    #[test]
    fn going_into_the_selections_pane_with_nothing_in_it_says_so() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        let shift = KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT);
        app.on_key(shift);
        assert_eq!(selected_name(&app), Some("a"));
        assert!(app.notice().unwrap().contains("is empty"));
    }

    #[test]
    fn capital_r_moves_the_borders_with_the_keys_until_esc() {
        let mut app = app_with(&["a", "b"]);
        app.set_tiles(WIDE);
        press(&mut app, KeyCode::Char('|'));
        press(&mut app, KeyCode::Char('R'));
        assert!(app.resizing());
        let width = |app: &App| placed(app, WIDE)[0].1.width;
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(width(&app), 50 + RESIZE_COLUMNS);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(width(&app), 50 - RESIZE_COLUMNS);
        // Keys that mean something else in the sidebar do nothing here.
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.confirm(), None);
        press(&mut app, KeyCode::Char('='));
        assert_eq!(width(&app), 50);
        press(&mut app, KeyCode::Esc);
        assert!(!app.resizing());
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(width(&app), 50, "l isn't a sidebar key");
    }

    #[test]
    fn resize_mode_needs_panes_to_resize() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('R'));
        assert!(!app.resizing());
        assert!(app.notice().unwrap().contains("one pane"));
        let mut app = app_with_splits(&["a", "b"], 1);
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('R'));
        assert!(!app.resizing());
        assert!(app.notice().unwrap().contains("zoomed"));
    }

    #[test]
    fn a_border_taken_by_the_mouse_follows_it_until_let_go() {
        let mut app = app_with(&["a", "b"]);
        app.set_tiles(WIDE);
        press(&mut app, KeyCode::Char('|'));
        let down = MouseEventKind::Down(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let up = MouseEventKind::Up(MouseButton::Left);
        let border = |at| Hit::Border { split: 0, at };
        assert_eq!(app.on_mouse(down, border(50)), None);
        assert_eq!(app.moving_border(), Some(0));
        app.on_mouse(drag, border(70));
        assert_eq!(placed(&app, WIDE)[0].1.width, 70);
        // Anything else meanwhile is the drag's: nothing selects or moves.
        app.on_mouse(drag, Hit::Sidebar);
        assert_eq!(app.on_mouse(up, Hit::Elsewhere), None);
        assert_eq!(app.moving_border(), None);
        app.on_mouse(drag, border(20));
        assert_eq!(placed(&app, WIDE)[0].1.width, 70);
    }

    /// The sessions the panes show, in the order they're drawn: `-` for
    /// the selection's pane while its session has a split of its own.
    fn drawn(app: &App) -> Vec<String> {
        app.slots()
            .into_iter()
            .map(|slot| match app.pane_session(slot) {
                Some(_) if !app.shows_screen(slot) => "-".to_string(),
                Some(session) => session.name.clone(),
                None => "?".to_string(),
            })
            .collect()
    }

    #[test]
    fn capital_hjkl_swap_the_selected_sessions_pane_with_the_one_that_way() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        assert_eq!(drawn(&app), ["a", "b", "c"]);
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(drawn(&app), ["a", "c", "b"]);
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(drawn(&app), ["c", "a", "b"]);
        // At the edges it stays.
        press(&mut app, KeyCode::Char('K'));
        press(&mut app, KeyCode::Char('L'));
        assert_eq!(drawn(&app), ["c", "a", "b"]);
        assert_eq!(app.notice(), None);

        // A split moves the same way.
        app.select("a");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(drawn(&app), ["c", "b", "a"]);
        assert_eq!(app.splits(), ["b", "a"]);

        // Side by side, H and L.
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('|'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('H'));
        assert_eq!(drawn(&app), ["b", "a"]);
        press(&mut app, KeyCode::Char('H'));
        assert_eq!(drawn(&app), ["b", "a"]);
        press(&mut app, KeyCode::Char('L'));
        assert_eq!(drawn(&app), ["a", "b"]);
    }

    #[test]
    fn the_keyboard_and_tab_follow_a_pane_that_moved() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        // Type into a's pane, then b's, then come back to the sidebar.
        press(&mut app, KeyCode::Tab);
        hand_back(&mut app);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));
        hand_back(&mut app);
        app.select("b");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(app.splits(), ["b", "a"]);
        // Tab goes on from b's pane, wherever it is: to a's, after it.
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));
        let typing_into = app.pane_session(Slot::Split(1)).unwrap();
        assert_eq!(typing_into.name, "a");
    }

    #[test]
    fn a_pane_doesnt_move_zoomed_or_alone() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('L'));
        assert!(app.notice().unwrap().contains("one pane"));

        let mut app = app_with_splits(&["a", "b"], 1);
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('L'));
        assert!(app.notice().unwrap().contains("zoomed"));
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(drawn(&app), ["a", "b"]);
    }

    #[test]
    fn panes_keep_their_places_as_splits_close_and_come() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        assert_eq!(drawn(&app), ["a", "b", "c"]);
        // a's split closes, and b's pane takes its room.
        app.select("a");
        press(&mut app, KeyCode::Char('s'));
        app.select("c");
        assert_eq!(drawn(&app), ["b", "c"]);
        // A new split stays where the selection's pane showed it.
        app.select("a");
        press(&mut app, KeyCode::Char('s'));
        app.select("c");
        assert_eq!(drawn(&app), ["b", "a", "c"]);
        // A session that goes leaves its room to the pane beside it.
        app.set_sessions(vec![session("a"), session("c")]);
        assert_eq!(drawn(&app), ["a", "c"]);
        assert_eq!(app.tabs_to_keep().current().splits(), ["a"]);
    }

    #[test]
    fn a_pane_taken_by_its_header_swaps_with_the_one_it_is_let_go_over() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        let down = MouseEventKind::Down(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let up = MouseEventKind::Up(MouseButton::Left);
        let header = |slot| Hit::Pane { slot, cell: None };
        assert_eq!(app.on_mouse(down, header(Slot::Selected)), None);
        assert_eq!(
            app.grabbed(),
            Some(Grab {
                from: Slot::Selected,
                over: Some(Slot::Selected)
            })
        );
        app.on_mouse(drag, Hit::Sidebar);
        assert_eq!(app.grabbed().unwrap().over, None);
        let over_b = Hit::Pane {
            slot: Slot::Split(1),
            cell: Some((3, 4)),
        };
        assert_eq!(app.on_mouse(drag, over_b), None);
        assert_eq!(app.grabbed().unwrap().over, Some(Slot::Split(1)));
        assert_eq!(app.on_mouse(up, over_b), None);
        assert_eq!(app.grabbed(), None);
        assert_eq!(drawn(&app), ["a", "c", "b"]);

        // Let go anywhere but over another pane, and nothing moves.
        app.on_mouse(down, header(Slot::Split(0)));
        app.on_mouse(up, Hit::Sidebar);
        app.on_mouse(down, header(Slot::Split(0)));
        app.on_mouse(up, header(Slot::Split(0)));
        assert_eq!(drawn(&app), ["a", "c", "b"]);
    }

    #[test]
    fn a_header_alone_or_zoomed_takes_nothing() {
        let down = MouseEventKind::Down(MouseButton::Left);
        let header = Hit::Pane {
            slot: Slot::Selected,
            cell: None,
        };
        let mut app = app_with(&["a"]);
        app.on_mouse(down, header);
        assert_eq!(app.grabbed(), None);
        // The click still hands the pane the keyboard.
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));

        let mut app = app_with_splits(&["a", "b"], 1);
        press(&mut app, KeyCode::Char('z'));
        app.on_mouse(down, header);
        assert_eq!(app.grabbed(), None);
    }

    #[test]
    fn f_floats_the_selected_session_with_the_keyboard_and_again_puts_it_back() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.slots(), [Slot::Selected, Slot::Float]);
        assert_eq!(app.floating().unwrap().name, "a");
        assert_eq!(app.focus(), Focus::Pane(Slot::Float));
        // The selection's pane doesn't draw it a second time.
        assert!(app.shows_screen(Slot::Float));
        assert!(!app.shows_screen(Slot::Selected));

        // The float stays over the panes while the selection moves on.
        hand_back(&mut app);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(drawn(&app), ["b", "a"]);
        assert_eq!(app.tabs_to_keep().current().floating.as_deref(), Some("a"));

        // F puts it back, whatever is selected.
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.slots(), [Slot::Selected]);
        assert!(app.floating().is_none());
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn tab_goes_round_to_the_float_last_and_keys_go_to_it() {
        let mut app = app_with_splits(&["a", "b", "c"], 1);
        press(&mut app, KeyCode::Char('F'));
        hand_back(&mut app);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.slots(), [Slot::Split(0), Slot::Selected, Slot::Float]);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
        hand_back(&mut app);
        press(&mut app, KeyCode::Tab);
        hand_back(&mut app);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Float));
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(
            app.on_key(key),
            Some(Action::Type {
                to: Slot::Float,
                key
            })
        );
    }

    #[test]
    fn a_split_comes_up_into_the_float_and_s_puts_the_float_in_a_split() {
        let mut app = app_with_splits(&["a", "b"], 1);
        app.select("a");
        press(&mut app, KeyCode::Char('F'));
        assert!(app.splits().is_empty());
        assert_eq!(app.floating().unwrap().name, "a");
        hand_back(&mut app);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.splits(), ["a"]);
        assert!(app.floating().is_none());
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_float_floats_over_a_zoomed_tab_too() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('F'));
        hand_back(&mut app);
        press(&mut app, KeyCode::Char('z'));
        // a floats: the zoomed pane follows the selection, under it.
        assert_eq!(app.slots(), [Slot::Selected, Slot::Float]);
        assert!(app.shows_screen(Slot::Float));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(drawn(&app), ["b", "a"]);
    }

    #[test]
    fn the_float_goes_with_its_session() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('F'));
        app.set_sessions(vec![session("b")]);
        assert!(app.floating().is_none());
        assert_eq!(app.slots(), [Slot::Selected]);
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(app.tabs_to_keep().current().floating, None);
    }

    #[test]
    fn the_float_is_the_tabs_and_leaves_it_with_its_session() {
        let mut app = app_with_a_second_tab(&["a"]);
        press(&mut app, KeyCode::Char('F'));
        hand_back(&mut app);
        press(&mut app, KeyCode::Char('['));
        assert!(app.floating().is_none());
        press(&mut app, KeyCode::Char(']'));
        assert_eq!(app.floating().unwrap().name, "shell");
        press(&mut app, KeyCode::Char('>'));
        press(&mut app, KeyCode::Char('1'));
        assert!(app.floating().is_none());
    }

    #[test]
    fn the_tuis_own_session_doesnt_float_and_a_float_doesnt_move() {
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![session("me")]);
        press(&mut app, KeyCode::Char('F'));
        assert!(app.floating().is_none());
        assert!(app.notice().unwrap().contains("runs in"));

        let mut app = app_with_splits(&["a", "b"], 1);
        press(&mut app, KeyCode::Char('F'));
        hand_back(&mut app);
        press(&mut app, KeyCode::Char('L'));
        assert!(app.notice().unwrap().contains("F puts it back"));
    }

    #[test]
    fn s_capital_opens_the_layouts_and_keys_go_to_them_until_esc() {
        let mut app = app_with(&["a"]);
        assert_eq!(
            press(&mut app, KeyCode::Char('S')),
            Some(Action::ListLayouts)
        );
        app.show_layouts(Ok(Layouts::default()), None);
        assert!(app.layouts_view().is_some());
        // `s` saves rather than splits while the view is open.
        press(&mut app, KeyCode::Char('s'));
        assert!(app.splits().is_empty());
        type_text(&mut app, "work");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::SaveLayout("work".into()))
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.layouts_view().is_none());
    }

    #[test]
    fn enter_in_the_layouts_asks_for_the_one_the_bar_is_on() {
        let mut app = app_with(&["a"]);
        let mut layouts = Layouts::default();
        layouts.save("work", app.tabs_to_keep(), app.programs(), 10);
        app.show_layouts(Ok(layouts), None);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::RestoreLayout(Which::Saved("work".into())))
        );
    }

    #[test]
    fn a_layout_restored_puts_the_tabs_back_with_the_sessions_still_there() {
        let mut app = app_with(&["a", "b", "c"]);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('K'));
        press(&mut app, KeyCode::Char('T'));
        type_text(&mut app, "work");
        press(&mut app, KeyCode::Enter);
        let saved = app.tabs_to_keep();

        // Then the tabs change: a tab of its own for c, the split closed.
        app.select("c");
        press(&mut app, KeyCode::Char('>'));
        press(&mut app, KeyCode::Char('t'));
        app.select("a");
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.tabs().all().len(), 2);

        // a has gone since the layout was saved; d is new.
        app.set_sessions(vec![session("b"), session("c"), session("d")]);
        app.show_layouts(Ok(Layouts::default()), None);
        app.restore_layout(saved.clone(), "work", 0);
        assert!(app.layouts_view().is_none());
        assert_eq!(app.tabs().all().len(), 1);
        assert_eq!(app.tabs().current().name, "work");
        assert_eq!(drawn(&app), ["b"]);
        assert_eq!(selected_name(&app), Some("b"));
        let mut held = app.tabs().current().sessions.clone();
        held.sort();
        assert_eq!(held, ["b", "c", "d"]);
        assert_eq!(
            app.notice(),
            Some("restored work: one of its sessions has gone")
        );
        app.restore_layout(saved, "work", 2);
        assert_eq!(
            app.notice(),
            Some("restored work: 2 of its sessions started again, one of its sessions has gone")
        );
    }

    #[test]
    fn a_layout_saves_what_starts_each_session_but_the_tui_s_own() {
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![session("a"), session("me")]);
        let programs = app.programs();
        assert_eq!(programs.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(programs["a"].command, ["sh"]);
    }

    #[test]
    fn a_session_with_a_split_is_not_shown_again_in_the_selections_pane() {
        let mut app = app_with(&["a"]);
        assert!(app.shows_screen(Slot::Selected));
        press(&mut app, KeyCode::Char('s'));
        assert!(!app.shows_screen(Slot::Selected));
        assert!(app.shows_screen(Slot::Split(0)));
    }

    #[test]
    fn enter_on_a_split_session_types_into_its_split() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
    }

    #[test]
    fn the_tuis_own_session_is_never_split_off() {
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![session("me")]);
        press(&mut app, KeyCode::Char('s'));
        assert!(app.splits().is_empty());
        assert!(app.notice().is_some());
    }

    #[test]
    fn tab_in_the_sidebar_goes_on_to_the_next_pane_each_time() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        let mut visited = Vec::new();
        for _ in 0..4 {
            press(&mut app, KeyCode::Tab);
            visited.push(app.focus());
            hand_back(&mut app);
        }
        let pane = Focus::Pane;
        assert_eq!(
            visited,
            [
                pane(Slot::Split(0)),
                pane(Slot::Split(1)),
                pane(Slot::Selected),
                pane(Slot::Split(0)),
            ]
        );
    }

    #[test]
    fn shift_tab_goes_round_the_other_way_starting_at_the_last_pane() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        hand_back(&mut app);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));
    }

    #[test]
    fn tab_skips_panes_that_take_no_keys() {
        let mut app = App::new(None);
        app.set_sessions(vec![ended("done"), session("live")]);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('s'));
        // The selection is on `live`, which has a split of its own, so its
        // pane shows nothing; `done` has ended. Only `live`'s split is left.
        for _ in 0..3 {
            press(&mut app, KeyCode::Tab);
            assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));
            hand_back(&mut app);
        }
    }

    /// `app` with the config's `[keys]` set to `toml`'s.
    fn with_keys(mut app: App, toml: &str) -> App {
        let config = crate::config::from_text(&format!("[keys]\n{toml}")).unwrap();
        app.set_interface(&config);
        app
    }

    fn ctrl(app: &mut App, c: char) -> Option<Action> {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    #[test]
    fn a_key_the_config_gives_a_command_runs_it_and_its_old_key_doesnt() {
        let mut app = with_keys(
            app_with(&["a", "b"]),
            "kill = \"X\"\nnew-session = \"ctrl+n\"",
        );
        press(&mut app, KeyCode::Char('x'));
        assert!(app.confirm().is_none(), "x is free now");
        app.on_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
        assert!(matches!(app.confirm(), Some(Confirm::Kill(name)) if name == "a"));
        press(&mut app, KeyCode::Char('n'));
        assert!(app.launcher().is_none());
        ctrl(&mut app, 'n');
        assert!(app.launcher().is_some());
    }

    #[test]
    fn the_prefix_in_a_pane_runs_the_next_keys_command_and_keeps_the_keyboard_there() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        assert_eq!(ctrl(&mut app, 'b'), None);
        assert!(app.prefixed());
        // `j` selects the next session; the pane that follows the selection
        // keeps the keyboard, now on it.
        assert_eq!(press(&mut app, KeyCode::Char('j')), None);
        assert!(!app.prefixed());
        assert_eq!(selected_name(&app), Some("b"));
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        // Without the prefix, `j` is the program's.
        let typed = press(&mut app, KeyCode::Char('j'));
        assert!(matches!(typed, Some(Action::Type { .. })), "{typed:?}");
    }

    #[test]
    fn the_prefix_twice_sends_it_and_esc_after_it_does_nothing() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        ctrl(&mut app, 'b');
        let sent = ctrl(&mut app, 'b');
        assert!(
            matches!(sent, Some(Action::Type { key, .. }) if key.code == KeyCode::Char('b')),
            "{sent:?}"
        );
        ctrl(&mut app, 'b');
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        ctrl(&mut app, 'b');
        assert_eq!(press(&mut app, KeyCode::Char('%')), None);
        assert!(
            app.notice()
                .is_some_and(|notice| notice.contains("runs no command"))
        );
    }

    #[test]
    fn the_prefix_can_be_another_key_or_none() {
        let mut app = with_keys(app_with(&["a", "b"]), "prefix = \"none\"");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(ctrl(&mut app, 'b'), Some(Action::Type { .. })));
        let mut app = with_keys(
            app_with(&["a", "b"]),
            "prefix = \"ctrl+a\"\nhand-back = \"ctrl+g\"",
        );
        press(&mut app, KeyCode::Enter);
        ctrl(&mut app, 'a');
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("b"));
        ctrl(&mut app, '\\');
        assert_ne!(app.focus(), Focus::Sidebar, "ctrl+\\ is the program's now");
        ctrl(&mut app, 'g');
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn colon_lists_the_commands_and_enter_runs_one() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char(':'));
        assert!(app.command_list().is_some());
        type_text(&mut app, "zoom");
        press(&mut app, KeyCode::Enter);
        assert!(app.command_list().is_none());
        assert!(app.zoomed());
        // It leads the list the next time.
        press(&mut app, KeyCode::Char(':'));
        let first = app.command_list().unwrap().shown().next().unwrap().0;
        assert_eq!(first.name, "zoom");
        press(&mut app, KeyCode::Esc);
        assert!(app.command_list().is_none());
        assert!(app.zoomed(), "esc runs nothing");
    }

    #[test]
    fn a_plugin_action_with_no_key_runs_from_the_command_list() {
        let mut app = app_with(&["a"]);
        app.set_plugin_keys(vec![PluginKey {
            key: None,
            plugin: "notes".into(),
            action: "add".into(),
            title: "add a note".into(),
        }]);
        assert!(app.plugin_key_rows().is_empty(), "no key to list");
        press(&mut app, KeyCode::Char(':'));
        type_text(&mut app, "notes:add");
        let ran = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(&ran, Some(Action::RunPlugin { plugin, action, .. }) if plugin == "notes" && action == "add"),
            "{ran:?}"
        );
    }

    fn ctrl_alt(app: &mut App, c: char) -> Option<Action> {
        let both = KeyModifiers::CONTROL | KeyModifiers::ALT;
        app.on_key(KeyEvent::new(KeyCode::Char(c), both))
    }

    #[test]
    fn a_direct_key_runs_its_command_from_a_pane_and_other_chords_go_to_the_program() {
        let mut app = with_keys(
            app_with(&["a", "b"]),
            "down = [\"j\", \"direct+ctrl+alt+j\"]",
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        assert_eq!(ctrl_alt(&mut app, 'j'), None);
        assert_eq!(selected_name(&app), Some("b"));
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        // One the user didn't write direct+ is the program's.
        let typed = ctrl_alt(&mut app, 'k');
        assert!(matches!(typed, Some(Action::Type { .. })), "{typed:?}");
        // In the sidebar it's a key like any other.
        ctrl(&mut app, '\\');
        press(&mut app, KeyCode::Char('k'));
        ctrl_alt(&mut app, 'j');
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn any_of_the_prefixes_starts_a_command_from_a_pane() {
        let mut app = with_keys(app_with(&["a", "b"]), "prefix = [\"ctrl+b\", \"ctrl+a\"]");
        press(&mut app, KeyCode::Enter);
        ctrl(&mut app, 'a');
        assert!(app.prefixed());
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("b"));
        ctrl(&mut app, 'b');
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(selected_name(&app), Some("a"));
        // Either, twice, goes to the program as itself.
        ctrl(&mut app, 'a');
        let sent = ctrl(&mut app, 'a');
        assert!(
            matches!(sent, Some(Action::Type { key, .. }) if key.code == KeyCode::Char('a')),
            "{sent:?}"
        );
    }

    /// `app` with one `[[keys.command]]`, on Ctrl+G, of `kind`.
    fn with_command(app: App, kind: &str, command: &str) -> App {
        let toml = format!(
            "\n[[keys.command]]\nkey = \"direct+ctrl+g\"\ntype = \"{kind}\"\ncommand = \"{command}\"\n"
        );
        with_keys(app, &toml)
    }

    #[test]
    fn a_popup_or_a_shell_command_of_the_users_runs_about_the_selected_session() {
        let mut app = with_command(app_with(&["a", "b"]), "popup", "lazygit");
        press(&mut app, KeyCode::Char('j'));
        let ran = ctrl(&mut app, 'g');
        let Some(Action::RunKeyCommand {
            command,
            dir,
            context,
        }) = ran
        else {
            panic!("{ran:?}");
        };
        assert_eq!(command.kind, CommandKind::Popup);
        assert_eq!(command.command, "lazygit");
        let b = app
            .sessions
            .iter()
            .find(|session| session.name == "b")
            .unwrap();
        assert_eq!(dir.as_ref(), Some(&b.cwd));
        assert_eq!(context.session.as_deref(), Some("b"));
        // From a pane too, written direct+.
        let mut app = with_command(app_with(&["a"]), "shell", "make");
        press(&mut app, KeyCode::Enter);
        let ran = ctrl(&mut app, 'g');
        assert!(
            matches!(&ran, Some(Action::RunKeyCommand { command, .. }) if command.kind == CommandKind::Shell),
            "{ran:?}"
        );
    }

    #[test]
    fn a_pane_or_tab_command_of_the_users_makes_room_for_its_session_first() {
        let mut app = with_command(app_with(&["a", "b"]), "pane", "make test");
        app.set_tiles(WIDE);
        let ran = ctrl(&mut app, 'g');
        assert!(matches!(ran, Some(Action::RunKeyCommand { .. })), "{ran:?}");
        assert_eq!(app.splits(), vec!["a"], "a stays, beside the new one");
        let mut app = with_command(app_with(&["a"]), "tab", "htop");
        let a = app.sessions[0].cwd.clone();
        let ran = ctrl(&mut app, 'g');
        assert_eq!(app.tabs().all().len(), 2);
        assert_eq!(app.tabs().current_index(), 1);
        let Some(Action::RunKeyCommand { dir, .. }) = ran else {
            panic!("{ran:?}");
        };
        assert_eq!(dir, Some(a), "where a was");
    }

    #[test]
    fn a_plugin_action_of_the_users_runs_as_its_own_key_would_and_the_list_has_it() {
        let mut app = with_command(app_with(&["a"]), "plugin", "notes:add");
        let ran = ctrl(&mut app, 'g');
        assert!(
            matches!(&ran, Some(Action::RunPlugin { plugin, action, .. }) if plugin == "notes" && action == "add"),
            "{ran:?}"
        );
        let mut app = with_command(app_with(&["a"]), "shell", "make docs");
        press(&mut app, KeyCode::Char(':'));
        type_text(&mut app, "make docs");
        let ran = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(&ran, Some(Action::RunKeyCommand { command, .. }) if command.command == "make docs"),
            "{ran:?}"
        );
    }

    /// A background task called `name`, asking for a permission.
    fn asking(name: &str) -> SessionInfo {
        SessionInfo {
            front: Some(Front::Task),
            asking: Some(crate::protocol::Asking {
                tool: "Bash".into(),
                gist: "cargo test".into(),
            }),
            ..doing(name, Activity::Waiting)
        }
    }

    #[test]
    fn the_answer_keys_the_config_gives_answer_in_the_sidebar_its_pane_and_the_list() {
        let toml = "answer-yes = \"a\"\nanswer-no = \"d\"";
        let mut app = with_keys(App::new(None), toml);
        app.set_sessions(vec![asking("fixer")]);
        let answer = |answer| {
            Some(Action::Answer {
                name: "fixer".into(),
                answer,
            })
        };
        assert_eq!(press(&mut app, KeyCode::Char('a')), answer(Answer::Allow));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            None,
            "y answers no more"
        );
        // `n` is a new session's key again, and `d` says no.
        assert_eq!(press(&mut app, KeyCode::Char('d')), answer(Answer::Deny));
        // In its pane.
        press(&mut app, KeyCode::Enter);
        assert_eq!(press(&mut app, KeyCode::Char('a')), answer(Answer::Allow));
        let shifted = app.on_key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT));
        assert_eq!(shifted, answer(Answer::Always));
        // And in the list of what needs the user, `y` there is nothing.
        ctrl(&mut app, '\\');
        press(&mut app, KeyCode::Char('U'));
        assert_eq!(press(&mut app, KeyCode::Char('y')), None);
        assert_eq!(press(&mut app, KeyCode::Char('d')), answer(Answer::Deny));
    }

    #[test]
    fn resize_modes_keys_follow_the_config() {
        let toml = "resize-right = \"ctrl+l\"\nresize-done = \"ctrl+c\"";
        let mut app = with_keys(app_with(&["a", "b"]), toml);
        app.set_tiles(WIDE);
        press(&mut app, KeyCode::Char('|'));
        press(&mut app, KeyCode::Char('R'));
        let width = |app: &App| placed(app, WIDE)[0].1.width;
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(width(&app), 50, "l moves nothing now");
        ctrl(&mut app, 'l');
        assert_eq!(width(&app), 50 + RESIZE_COLUMNS);
        press(&mut app, KeyCode::Esc);
        assert!(app.resizing(), "esc isn't done now");
        ctrl(&mut app, 'c');
        assert!(!app.resizing());
        // Resize's own key again is done, whatever the config says.
        press(&mut app, KeyCode::Char('R'));
        press(&mut app, KeyCode::Char('R'));
        assert!(!app.resizing());
    }

    #[test]
    fn a_views_keys_the_config_gives_stand_for_its_own() {
        let toml = "view-down = \"ctrl+j\"\nview-close = \"ctrl+g\"";
        let mut app = with_keys(App::new(None), toml);
        app.set_sessions(vec![
            doing("first", Activity::Waiting),
            doing("second", Activity::Waiting),
        ]);
        press(&mut app, KeyCode::Char('U'));
        let highlighted = |app: &App| {
            app.needs_you_view()
                .unwrap()
                .highlighted()
                .unwrap()
                .name
                .clone()
        };
        let first = highlighted(&app);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(highlighted(&app), first, "j is taken away");
        ctrl(&mut app, 'j');
        assert_ne!(highlighted(&app), first);
        press(&mut app, KeyCode::Up);
        assert_eq!(highlighted(&app), first, "↑ is always the view's");
        press(&mut app, KeyCode::Char('q'));
        assert!(app.needs_you_view().is_some(), "q is taken away");
        ctrl(&mut app, 'g');
        assert!(app.needs_you_view().is_none());
        // Where a view takes typing, a letter is typed.
        let mut app = with_keys(app_with(&["a", "b"]), "view-down = \"m\"");
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.filter().unwrap().input.text(), "m");
    }

    #[test]
    fn a_plugins_two_keys_run_its_action_one_after_the_other() {
        let mut app = app_with(&["a"]);
        app.set_plugin_keys(vec![PluginKey {
            key: Some(Sequence::parse("N t").unwrap()),
            plugin: "notes".into(),
            action: "add".into(),
            title: "add a note".into(),
        }]);
        assert_eq!(app.plugin_key_rows()[0].0, "N t");
        let shift_n = KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT);
        assert_eq!(app.on_key(shift_n), None);
        assert!(app.pending().is_some());
        let ran = press(&mut app, KeyCode::Char('t'));
        assert!(
            matches!(&ran, Some(Action::RunPlugin { plugin, .. }) if plugin == "notes"),
            "{ran:?}"
        );
        assert!(app.pending().is_none());
        app.on_key(shift_n);
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert!(app.notice().unwrap().contains("N x runs nothing"));
        assert!(app.confirm().is_none(), "x killed nothing");
        app.on_key(shift_n);
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.notice(), None);
        // From a pane, after the prefix.
        press(&mut app, KeyCode::Enter);
        ctrl(&mut app, 'b');
        app.on_key(shift_n);
        let ran = press(&mut app, KeyCode::Char('t'));
        assert!(matches!(ran, Some(Action::RunPlugin { .. })), "{ran:?}");
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
    }

    #[test]
    fn a_plugins_chord_runs_its_action() {
        let mut app = app_with(&["a"]);
        app.set_plugin_keys(vec![PluginKey {
            key: Some(Sequence::parse("ctrl+alt+n").unwrap()),
            plugin: "notes".into(),
            action: "add".into(),
            title: "add a note".into(),
        }]);
        let ran = ctrl_alt(&mut app, 'n');
        assert!(matches!(ran, Some(Action::RunPlugin { .. })), "{ran:?}");
    }

    #[test]
    fn the_sidebar_resizes_within_its_bounds_and_folds() {
        let mut app = app_with(&["a"]);
        assert_eq!(app.sidebar_columns(120), 28);
        press(&mut app, KeyCode::Char(')'));
        assert_eq!(app.sidebar_columns(120), 32);
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('('));
        }
        assert_eq!(app.sidebar_columns(120), 16, "no narrower");
        assert_eq!(app.sidebar_columns(30), 16, "never past most of the screen");
        press(&mut app, KeyCode::Char('\\'));
        assert!(app.sidebar_folded());
        assert_eq!(app.sidebar_columns(120), RAIL_WIDTH);
        press(&mut app, KeyCode::Char(')'));
        assert!(!app.sidebar_folded(), "wider unfolds it");
        assert_eq!(app.sidebar_columns(120), 16);
    }

    #[test]
    fn a_folded_sidebar_can_take_no_columns() {
        let mut app = app_with(&["a"]);
        let config = crate::config::from_text("[sidebar]\nfold = \"hidden\"\nwidth = 40").unwrap();
        app.set_interface(&config);
        app.set_sidebar(None, true);
        assert_eq!(app.sidebar_columns(120), 0);
        press(&mut app, KeyCode::Char('\\'));
        assert_eq!(app.sidebar_columns(120), 40);
    }

    #[test]
    fn a_width_kept_holds_until_the_config_gives_another() {
        let mut app = app_with(&["a"]);
        let kept = Shape {
            width: 44,
            from_config: 28,
            folded: false,
        };
        app.set_sidebar(Some(kept), false);
        assert_eq!(app.sidebar_columns(200), 44);
        let config = crate::config::from_text("[sidebar]\nwidth = 30").unwrap();
        app.set_interface(&config);
        assert_eq!(app.sidebar_columns(200), 30);
        app.set_sidebar(Some(kept), false);
        assert_eq!(app.sidebar_columns(200), 30, "kept from another width");
    }

    #[test]
    fn the_sidebars_edge_drags_with_the_mouse() {
        let mut app = app_with(&["a"]);
        app.set_screen(Rect::new(0, 0, 120, 40));
        app.on_mouse(CLICK, Hit::SidebarEdge(28));
        assert!(app.dragging_sidebar());
        app.on_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            Hit::SidebarEdge(41),
        );
        assert_eq!(app.sidebar_columns(120), 41);
        app.on_mouse(MouseEventKind::Drag(MouseButton::Left), Hit::SidebarEdge(2));
        assert_eq!(app.sidebar_columns(120), 16);
        app.on_mouse(MouseEventKind::Up(MouseButton::Left), Hit::Elsewhere);
        assert!(!app.dragging_sidebar());
    }

    #[test]
    fn what_needs_you_is_pinned_at_the_top_from_every_tab() {
        let mut app = app_with_a_second_tab(&["a", "b"]);
        let waiting = doing("b", Activity::Waiting);
        let done = doing("a", Activity::Done);
        app.set_sessions(vec![done, waiting, session("shell")]);
        let rows = app.rows();
        let b = app.sessions().iter().position(|s| s.name == "b").unwrap();
        let a = app.sessions().iter().position(|s| s.name == "a").unwrap();
        assert_eq!(
            rows[..3],
            [Row::NeedsYou(2), Row::Pinned(b), Row::Pinned(a)]
        );
        assert_eq!(app.tab_elsewhere(b).as_deref(), Some("1"));
        // j and k pass the pinned rows by; a click on one goes to it.
        app.on_mouse(CLICK, Hit::SidebarRow(1));
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn the_config_can_leave_nothing_pinned() {
        let mut app = app_with(&["a"]);
        app.set_sessions(vec![doing("a", Activity::Waiting)]);
        assert!(matches!(app.rows()[0], Row::NeedsYou(1)));
        let config = crate::config::from_text("[sidebar]\nneeds_you = false").unwrap();
        app.set_interface(&config);
        assert!(!app.rows().iter().any(|row| matches!(row, Row::NeedsYou(_))));
    }

    #[test]
    fn tab_inside_a_pane_goes_to_its_session() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let typed = Action::Type {
            to: Slot::Selected,
            key: tab,
        };
        assert_eq!(app.on_key(tab), Some(typed));
    }

    #[test]
    fn a_split_closes_when_its_session_goes_and_the_keyboard_follows_its_pane() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        app.select("b");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));

        app.set_sessions(vec![session("b"), session("c")]);
        assert_eq!(app.splits(), ["b"]);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
    }

    #[test]
    fn page_keys_in_the_sidebar_page_the_selected_sessions_pane() {
        let mut app = app_with_splits(&["a", "b"], 1);
        app.select("b");
        assert_eq!(
            press(&mut app, KeyCode::PageUp),
            Some(Action::PageBack(Slot::Selected))
        );
        // A session split off is paged in its split.
        app.select("a");
        assert_eq!(
            press(&mut app, KeyCode::PageDown),
            Some(Action::PageForward(Slot::Split(0)))
        );
        assert_eq!(press(&mut App::new(None), KeyCode::PageUp), None);
    }

    #[test]
    fn in_a_pane_only_shifted_page_keys_page_its_history() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Enter);
        let shifted = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
        assert_eq!(
            app.on_key(shifted(KeyCode::PageUp)),
            Some(Action::PageBack(Slot::Selected))
        );
        assert_eq!(
            app.on_key(shifted(KeyCode::PageDown)),
            Some(Action::PageForward(Slot::Selected))
        );
        let page_up = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        let typed = Action::Type {
            to: Slot::Selected,
            key: page_up,
        };
        assert_eq!(app.on_key(page_up), Some(typed));
    }

    #[test]
    fn z_zooms_the_selected_sessions_pane_alone_and_again_puts_the_others_back() {
        let mut app = app_with_splits(&["a", "b", "c"], 1);
        app.select("b");
        assert_eq!(on_screen(&app), ["a", "b"]);
        press(&mut app, KeyCode::Char('z'));
        assert!(app.zoomed());
        assert_eq!(on_screen(&app), ["b"]);
        assert!(!app.shows_screen(Slot::Split(0)));
        assert_eq!(app.focus(), Focus::Sidebar);

        // Zoomed, j and k choose what the one pane shows: a split session
        // is shown in its own split's place.
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(selected_name(&app), Some("a"));
        assert_eq!(app.slots(), [Slot::Split(0)]);
        assert!(app.shows_screen(Slot::Split(0)));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(on_screen(&app), ["c"]);

        press(&mut app, KeyCode::Char('z'));
        assert!(!app.zoomed());
        assert_eq!(on_screen(&app), ["a", "c"]);
    }

    #[test]
    fn zoomed_tab_goes_to_the_one_pane_and_enter_types_into_it() {
        let mut app = app_with_splits(&["a", "b"], 1);
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        hand_back(&mut app);
        press(&mut app, KeyCode::Tab);
        assert_eq!(
            app.focus(),
            Focus::Pane(Slot::Selected),
            "the split is put away"
        );
        hand_back(&mut app);
        app.select("a");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
    }

    #[test]
    fn each_tab_is_zoomed_on_its_own() {
        let mut app = app_with_a_second_tab(&["a"]);
        press(&mut app, KeyCode::Char('z'));
        assert!(app.zoomed());
        press(&mut app, KeyCode::Char('['));
        assert!(!app.zoomed());
        press(&mut app, KeyCode::Char(']'));
        assert!(app.zoomed());
        assert!(app.tabs_to_keep().current().zoomed);
    }

    #[test]
    fn v_takes_the_keyboard_into_copy_mode_and_every_key_goes_there() {
        let mut app = app_with_splits(&["a", "b"], 1);
        app.select("a");
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.focus(), Focus::Copy(Slot::Split(0)));
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(
            app.on_key(q),
            Some(Action::CopyKey {
                slot: Slot::Split(0),
                key: q
            })
        );
        let paste = app.on_paste("text".into());
        let expected = Action::CopyPaste {
            slot: Slot::Split(0),
            text: "text".into(),
        };
        assert_eq!(paste, Some(expected));

        // The event loop says when copy mode is over.
        app.stop_copying();
        assert_eq!(app.focus(), Focus::Sidebar);
        press(&mut app, KeyCode::Char('v'));
        hand_back(&mut app);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn an_ended_session_can_be_copied_from_but_not_the_tuis_own() {
        let mut app = App::new(None);
        app.set_sessions(vec![ended("done")]);
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.focus(), Focus::Copy(Slot::Selected));
        // Copy mode stays while the ended session is still there to show.
        app.set_sessions(vec![ended("done")]);
        assert_eq!(app.focus(), Focus::Copy(Slot::Selected));
        app.set_sessions(Vec::new());
        assert_eq!(app.focus(), Focus::Sidebar);

        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![session("me")]);
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.focus(), Focus::Sidebar);
        assert!(app.notice().unwrap().contains("runs in"));
    }

    #[test]
    fn e_opens_the_history_of_the_selected_sessions_pane_in_the_editor() {
        let mut app = app_with_splits(&["a", "b", "a-history"], 1);
        app.select("a");
        // "a-history" is taken: the editor's session is called after it.
        assert_eq!(
            press(&mut app, KeyCode::Char('e')),
            Some(Action::EditHistory {
                slot: Slot::Split(0),
                dir: PathBuf::from("/"),
                name: "a-history-2".into(),
            })
        );

        // An ended session's last screen can be read too.
        let mut app = App::new(None);
        app.set_sessions(vec![ended("done")]);
        let edit = press(&mut app, KeyCode::Char('e'));
        assert!(matches!(
            edit,
            Some(Action::EditHistory {
                slot: Slot::Selected,
                ..
            })
        ));
    }

    #[test]
    fn e_has_nothing_to_open_for_the_tuis_own_session_or_none() {
        let mut app = App::new(Some("me".into()));
        assert_eq!(press(&mut app, KeyCode::Char('e')), None);
        app.set_sessions(vec![session("me")]);
        assert_eq!(press(&mut app, KeyCode::Char('e')), None);
        assert!(app.notice().unwrap().contains("runs in"));
    }

    #[test]
    fn a_drag_selects_in_the_pane_it_started_in_and_letting_go_copies() {
        let mut app = app_with_splits(&["a", "b"], 1);
        let down = MouseEventKind::Down(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let up = MouseEventKind::Up(MouseButton::Left);
        let in_split = |cell| Hit::Pane {
            slot: Slot::Split(0),
            cell,
        };
        assert_eq!(
            app.on_mouse(down, in_split(Some((2, 3)))),
            Some(Action::SelectFrom {
                slot: Slot::Split(0),
                cell: (2, 3)
            })
        );
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
        assert_eq!(app.dragging(), Some(Slot::Split(0)));
        assert_eq!(
            app.on_mouse(drag, in_split(Some((4, 0)))),
            Some(Action::SelectTo {
                slot: Slot::Split(0),
                cell: (4, 0)
            })
        );
        // Over anything else, the drag goes on but selects nothing new.
        assert_eq!(app.on_mouse(drag, Hit::Sidebar), None);
        assert_eq!(
            app.on_mouse(up, Hit::Elsewhere),
            Some(Action::CopySelection(Slot::Split(0)))
        );
        assert_eq!(app.dragging(), None);
    }

    #[test]
    fn without_copy_on_select_letting_go_holds_the_selection_in_copy_mode() {
        let mut app = app_with_splits(&["a", "b"], 1);
        let config = crate::config::from_text("[mouse]\ncopy_on_select = false").unwrap();
        app.set_interface(&config);
        let in_split = Hit::Pane {
            slot: Slot::Split(0),
            cell: Some((2, 3)),
        };
        app.on_mouse(MouseEventKind::Down(MouseButton::Left), in_split);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
        assert_eq!(
            app.on_mouse(MouseEventKind::Up(MouseButton::Left), in_split),
            Some(Action::HoldSelection(Slot::Split(0)))
        );
        // With something selected, copy mode takes the keyboard, and gives
        // it back to the pane it was typing into when it's over.
        app.hold_selection(Slot::Split(0));
        assert_eq!(app.focus(), Focus::Copy(Slot::Split(0)));
        app.stop_copying();
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
        // From the sidebar, it goes back there.
        app.focus = Focus::Sidebar;
        app.hold_selection(Slot::Split(0));
        app.stop_copying();
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_wheel_scrolls_the_pane_a_drag_is_selecting_in() {
        let mut app = app_with_splits(&["a", "b"], 1);
        let in_split = Hit::Pane {
            slot: Slot::Split(0),
            cell: Some((2, 3)),
        };
        app.on_mouse(MouseEventKind::Down(MouseButton::Left), in_split);
        // Wherever the mouse is, the wheel is the drag's pane's.
        assert_eq!(
            app.on_mouse(MouseEventKind::ScrollUp, Hit::Sidebar),
            Some(Action::ScrollBack(Slot::Split(0)))
        );
        assert_eq!(
            app.on_mouse(MouseEventKind::ScrollDown, in_split),
            Some(Action::ScrollForward(Slot::Split(0)))
        );
        assert_eq!(app.dragging(), Some(Slot::Split(0)));
    }

    #[test]
    fn a_scrollbars_thumb_follows_the_mouse_until_it_lets_go() {
        let mut app = app_with_splits(&["a", "b"], 1);
        let bar = |row| Hit::Scrollbar {
            slot: Slot::Split(0),
            row,
        };
        let slot = Slot::Split(0);
        assert_eq!(
            app.on_mouse(MouseEventKind::Down(MouseButton::Left), bar(4)),
            Some(Action::GrabThumb { slot, row: 4 })
        );
        // The keyboard stays where it was.
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(app.holding_thumb(), Some(slot));
        let drag = MouseEventKind::Drag(MouseButton::Left);
        assert_eq!(
            app.on_mouse(drag, bar(9)),
            Some(Action::DragThumb { slot, row: 9 })
        );
        assert_eq!(app.on_mouse(drag, Hit::Sidebar), None);
        assert_eq!(
            app.on_mouse(MouseEventKind::Up(MouseButton::Left), Hit::Sidebar),
            None
        );
        assert_eq!(app.holding_thumb(), None);
        // The wheel over it scrolls the pane, as over its screen, and a
        // right click opens the pane's menu.
        assert_eq!(
            app.on_mouse(MouseEventKind::ScrollUp, bar(0)),
            Some(Action::ScrollBack(slot))
        );
        app.right_click(bar(0), (79, 2));
        assert!(app.menu().is_some());
    }

    #[test]
    fn with_ctrl_held_the_mouse_over_a_pane_marks_where_to_look_for_a_link() {
        let mut app = app_with_splits(&["a", "b"], 1);
        let over = |cell| Hit::Pane {
            slot: Slot::Split(0),
            cell,
        };
        assert!(!app.mouse_moved(over(Some((2, 3))), false));
        assert!(app.mouse_moved(over(Some((2, 3))), true));
        assert_eq!(app.link_hover(), Some((Slot::Split(0), (2, 3))));
        // Staying on the cell changes nothing to draw.
        assert!(!app.mouse_moved(over(Some((2, 3))), true));
        // Its header line has no link, and nor has the sidebar.
        assert!(app.mouse_moved(over(None), true));
        assert_eq!(app.link_hover(), None);
        app.mouse_moved(over(Some((1, 1))), true);
        assert!(app.mouse_moved(Hit::Sidebar, true));
        // Letting go of Ctrl lets go of the link.
        app.mouse_moved(over(Some((1, 1))), true);
        assert!(app.mouse_moved(over(Some((1, 1))), false));
        assert_eq!(app.link_hover(), None);
        assert_eq!(
            app.link_context(Slot::Split(0)).session.as_deref(),
            Some("a")
        );
    }

    #[test]
    fn a_link_is_looked_for_only_while_nothing_waits_on_the_keyboard() {
        let mut app = app_with(&["a"]);
        let hit = Hit::Pane {
            slot: Slot::Selected,
            cell: Some((0, 0)),
        };
        assert_eq!(app.link_cell(hit), Some((Slot::Selected, (0, 0))));
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.link_cell(hit), None);
        assert!(!app.mouse_moved(hit, true));
    }

    #[test]
    fn a_click_in_the_pane_in_copy_mode_leaves_the_keyboard_there() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('v'));
        let down = MouseEventKind::Down(MouseButton::Left);
        let hit = Hit::Pane {
            slot: Slot::Selected,
            cell: Some((0, 0)),
        };
        app.on_mouse(down, hit);
        assert_eq!(app.focus(), Focus::Copy(Slot::Selected));
    }

    #[test]
    fn a_click_after_a_lost_release_starts_afresh() {
        let mut app = app_with_splits(&["a", "b"], 1);
        let down = MouseEventKind::Down(MouseButton::Left);
        let in_pane = |slot| Hit::Pane {
            slot,
            cell: Some((0, 0)),
        };
        app.on_mouse(down, in_pane(Slot::Split(0)));
        let again = app.on_mouse(down, in_pane(Slot::Selected));
        let expected = Action::SelectFrom {
            slot: Slot::Selected,
            cell: (0, 0),
        };
        assert_eq!(again, Some(expected));
        assert_eq!(app.dragging(), Some(Slot::Selected));
    }

    #[test]
    fn the_keyboard_leaves_a_split_whose_session_ends() {
        let mut app = app_with_splits(&["a", "b"], 1);
        app.select("a");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));

        app.set_sessions(vec![ended("a"), session("b")]);
        assert_eq!(app.splits(), ["a"], "it stays, to show how it ended");
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    /// The names of the sessions each pane shows, in the order they're
    /// drawn.
    fn on_screen(app: &App) -> Vec<&str> {
        let slots = app.slots().into_iter();
        let shown = slots.filter_map(|slot| app.pane_session(slot));
        shown.map(|session| session.name.as_str()).collect()
    }

    /// The names of the sessions the sidebar shows: the tab in front's.
    fn in_sidebar(app: &App) -> Vec<&str> {
        let shown = app.matches().into_iter();
        shown
            .map(|index| app.sessions()[index].name.as_str())
            .collect()
    }

    /// An app with `names` in the first tab, and a second tab in front
    /// holding only `shell`, started in it the way `t` starts one.
    fn app_with_a_second_tab(names: &[&str]) -> App {
        let mut app = app_with(names);
        press(&mut app, KeyCode::Char('t'));
        let mut sessions: Vec<SessionInfo> = names.iter().map(|name| session(name)).collect();
        sessions.push(session("shell"));
        app.set_sessions(sessions);
        app.select("shell");
        app
    }

    #[test]
    fn t_makes_an_empty_tab_and_starts_a_shell_where_the_selected_session_is() {
        let mut app = App::new(None);
        let b = SessionInfo {
            cwd: PathBuf::from("/code/b"),
            ..session("b")
        };
        app.set_sessions(vec![session("a"), b]);
        app.select("b");
        let action = press(&mut app, KeyCode::Char('t'));
        assert_eq!(
            action,
            Some(Action::Start {
                place: Place::Directory(Some(PathBuf::from("/code/b"))),
                command: Vec::new(),
                purpose: Purpose::default(),
            })
        );
        assert_eq!(app.tabs().all().len(), 2);
        assert_eq!(app.tabs().current_index(), 1);
        assert!(in_sidebar(&app).is_empty());
        assert!(app.selected().is_none());
    }

    #[test]
    fn t_starts_its_shell_where_the_settings_say_when_they_do() {
        let mut app = App::new(None);
        app.set_sessions(vec![SessionInfo {
            cwd: PathBuf::from("/code/b"),
            ..session("b")
        }]);
        app.select("b");
        app.set_start_dir(Some(PathBuf::from("/home/ann")));
        let Some(Action::Start { place, .. }) = press(&mut app, KeyCode::Char('t')) else {
            panic!("t starts a shell");
        };
        assert_eq!(place, Place::Directory(Some(PathBuf::from("/home/ann"))));
    }

    #[test]
    fn the_tab_bar_is_left_out_with_one_tab_only_when_told() {
        let mut app = App::new(None);
        assert!(app.tab_bar_shown());
        let config = crate::config::from_text("[tab_bar]\nhide_when_single = true\n").unwrap();
        app.set_interface(&config);
        assert!(!app.tab_bar_shown());
        press(&mut app, KeyCode::Char('t'));
        assert!(app.tab_bar_shown());
    }

    #[test]
    fn the_shell_t_starts_is_all_its_tab_has() {
        let app = app_with_a_second_tab(&["a", "b"]);
        assert_eq!(in_sidebar(&app), ["shell"]);
        assert_eq!(selected_name(&app), Some("shell"));
        assert_eq!(on_screen(&app), ["shell"]);
    }

    #[test]
    fn going_to_another_tab_changes_the_sidebar_and_the_panes() {
        let mut app = app_with_a_second_tab(&["a", "b"]);
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(in_sidebar(&app), ["a", "b"]);
        assert_eq!(on_screen(&app), ["a"]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("b"));

        press(&mut app, KeyCode::Char(']'));
        assert_eq!(in_sidebar(&app), ["shell"]);
        assert_eq!(on_screen(&app), ["shell"]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("shell"), "j stays in the tab");

        press(&mut app, KeyCode::Char('['));
        assert_eq!(selected_name(&app), Some("b"), "the tab kept its selection");
    }

    #[test]
    fn each_tab_keeps_its_own_splits() {
        let mut app = app_with_splits(&["a", "b", "c"], 1);
        assert_eq!(on_screen(&app), ["a", "c"]);
        press(&mut app, KeyCode::Char('t'));
        app.set_sessions(["a", "b", "c", "shell"].map(session).to_vec());
        assert!(app.splits().is_empty());
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(on_screen(&app), ["a", "c"]);
    }

    #[test]
    fn sessions_started_elsewhere_join_the_tab_in_front() {
        let mut app = app_with_a_second_tab(&["a"]);
        app.set_sessions(["a", "shell", "from-the-cli"].map(session).to_vec());
        assert_eq!(in_sidebar(&app), ["shell", "from-the-cli"]);
        press(&mut app, KeyCode::Char('1'));
        app.set_sessions(
            ["a", "shell", "from-the-cli", "later"]
                .map(session)
                .to_vec(),
        );
        assert_eq!(in_sidebar(&app), ["a", "later"]);
    }

    #[test]
    fn a_renamed_session_stays_in_its_tab() {
        let mut app = app_with_a_second_tab(&["a"]);
        app.renamed("a", "z");
        app.set_sessions(["z", "shell"].map(session).to_vec());
        assert_eq!(in_sidebar(&app), ["shell"]);
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(in_sidebar(&app), ["z"]);
    }

    #[test]
    fn a_copy_nobody_saw_is_said_once_as_it_comes() {
        let mut app = app_with(&["shown", "hidden"]);
        let copied = |count| SessionInfo {
            unseen_copies: count,
            ..session("hidden")
        };
        app.set_sessions(vec![session("shown"), copied(1)]);
        assert_eq!(
            app.notice(),
            Some("hidden copied out of sight: not put on your clipboard")
        );
        // The same count again says nothing new.
        app.notify("something else".into());
        app.set_sessions(vec![session("shown"), copied(1)]);
        assert_eq!(app.notice(), Some("something else"));
        // Nor does a daemon handed over, counting from nothing, until it
        // counts one.
        app.set_sessions(vec![session("shown"), copied(0)]);
        assert_eq!(app.notice(), Some("something else"));
        app.set_sessions(vec![session("shown"), copied(1)]);
        assert!(app.notice().unwrap().starts_with("hidden copied"));
    }

    #[test]
    fn a_session_renamed_elsewhere_stays_in_its_tab() {
        let mut app = app_with_a_second_tab(&["a"]);
        let mut renamed = session("z");
        renamed.id = "a".into();
        app.set_sessions(vec![renamed, session("shell")]);
        assert_eq!(in_sidebar(&app), ["shell"]);
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(in_sidebar(&app), ["z"]);
    }

    #[test]
    fn greater_than_moves_the_session_to_the_tab_named_next() {
        let mut app = app_with_a_second_tab(&["a", "b"]);
        press(&mut app, KeyCode::Char('1'));
        press(&mut app, KeyCode::Char('>'));
        assert_eq!(app.moving(), Some("a"));
        assert_eq!(press(&mut app, KeyCode::Char('2')), None);
        assert_eq!(app.moving(), None);
        assert_eq!(app.notice(), Some("moved a to tab 2"));
        assert_eq!(in_sidebar(&app), ["b"]);
        assert_eq!(selected_name(&app), Some("b"));
        assert_eq!(app.tabs().current_index(), 0, "the tab in front stays");

        press(&mut app, KeyCode::Char('2'));
        assert_eq!(in_sidebar(&app), ["a", "shell"]);
    }

    #[test]
    fn greater_than_then_t_moves_the_session_to_a_new_tab() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('>'));
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.tabs().all().len(), 2);
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(in_sidebar(&app), ["b"]);
        assert_eq!(app.tabs().all()[1].sessions, ["a"]);
    }

    #[test]
    fn moving_a_session_nowhere_new_says_so_and_other_keys_leave_it() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('>'));
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.notice(), Some("a is in tab 1 already"));
        press(&mut app, KeyCode::Char('>'));
        press(&mut app, KeyCode::Char('4'));
        assert_eq!(app.notice(), Some("there's no tab 4"));
        press(&mut app, KeyCode::Char('>'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.moving(), None);
        assert_eq!(in_sidebar(&app), ["a"]);
    }

    #[test]
    fn a_session_moved_away_takes_its_split_with_it() {
        let mut app = app_with_a_second_tab(&["a", "b"]);
        press(&mut app, KeyCode::Char('1'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.splits(), ["a"]);
        press(&mut app, KeyCode::Char('>'));
        press(&mut app, KeyCode::Char('2'));
        assert!(app.splits().is_empty());
        assert_eq!(on_screen(&app), ["b"]);
    }

    #[test]
    fn a_digit_goes_to_that_tab_and_one_not_there_says_so() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.tabs().current_index(), 0);
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.tabs().current_index(), 1);

        press(&mut app, KeyCode::Char('5'));
        assert_eq!(app.tabs().current_index(), 1);
        assert_eq!(app.notice(), Some("there's no tab 5"));
    }

    #[test]
    fn there_can_be_a_tenth_tab_and_more() {
        let mut app = app_with(&["a"]);
        for _ in 0..11 {
            press(&mut app, KeyCode::Char('t'));
        }
        assert_eq!(app.tabs().all().len(), 12);
        assert_eq!(app.tabs().current_index(), 11);
        press(&mut app, KeyCode::Char('9'));
        assert_eq!(app.tabs().current_index(), 8);
    }

    #[test]
    fn braces_move_the_tab_in_front_and_it_stays_in_front() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('T'));
        answer(&mut app, "one");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('t'));
        let names = |app: &App| -> Vec<String> {
            app.tabs()
                .all()
                .iter()
                .map(|tab| tab.name.clone())
                .collect()
        };
        press(&mut app, KeyCode::Char('{'));
        assert_eq!(names(&app), ["", "one"]);
        assert_eq!(app.tabs().current_index(), 0);
        press(&mut app, KeyCode::Char('{'));
        assert_eq!(app.notice(), Some("this tab is the first already"));
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(names(&app), ["one", ""]);
        assert_eq!(app.tabs().current_index(), 1);
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(app.notice(), Some("this tab is the last already"));
        // The session stays in its tab, which moved with it.
        assert_eq!(app.tabs().all()[0].sessions, ["a"]);
    }

    #[test]
    fn shift_t_names_the_tab_starting_from_the_name_it_has() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(prompt_text(&app), Some(""));
        answer(&mut app, "review");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tabs().current().name, "review");

        press(&mut app, KeyCode::Char('T'));
        assert_eq!(prompt_text(&app), Some("review"));
    }

    #[test]
    fn ampersand_closes_an_empty_tab_at_once() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(press(&mut app, KeyCode::Char('&')), None);
        assert!(app.confirm().is_none());
        assert_eq!(app.tabs().all().len(), 1);
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn ampersand_asks_before_closing_a_tab_and_killing_its_sessions() {
        let mut app = app_with_a_second_tab(&["a"]);
        press(&mut app, KeyCode::Char('&'));
        let question = app.confirm().map(Confirm::question);
        assert_eq!(
            question.as_deref(),
            Some("close tab 2 and kill its 1 session? y/n")
        );
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.tabs().all().len(), 2, "no keeps it");

        press(&mut app, KeyCode::Char('&'));
        let action = press(&mut app, KeyCode::Char('y'));
        assert_eq!(action, Some(Action::KillAll(vec!["shell".into()])));
        assert_eq!(app.tabs().all().len(), 1);
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn the_session_crystal_runs_in_is_never_killed_with_its_tab() {
        let mut app = App::new(Some("shell".into()));
        app.set_sessions(vec![session("a")]);
        press(&mut app, KeyCode::Char('t'));
        app.set_sessions(["a", "shell", "helper"].map(session).to_vec());
        press(&mut app, KeyCode::Char('&'));
        let action = press(&mut app, KeyCode::Char('y'));
        assert_eq!(action, Some(Action::KillAll(vec!["helper".into()])));
        assert!(in_sidebar(&app).contains(&"shell"), "it joined tab 1");
    }

    #[test]
    fn the_only_tab_stays_open() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('&'));
        assert_eq!(app.tabs().all().len(), 1);
        assert_eq!(app.notice(), Some("this is the only tab"));
    }

    #[test]
    fn a_split_of_a_session_that_goes_closes() {
        let mut app = app_with_splits(&["a", "b", "c"], 1);
        app.set_sessions(vec![session("b"), session("c")]);
        assert!(app.tabs().all().iter().all(|tab| tab.splits().is_empty()));
    }

    #[test]
    fn a_tab_whose_selected_session_has_gone_selects_its_first() {
        let mut app = app_with(&["a", "b", "c"]);
        app.select("b");
        press(&mut app, KeyCode::Char('t'));
        app.set_sessions(vec![session("a"), session("c")]);
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn u_goes_to_an_agent_waiting_in_another_tab() {
        let mut app = app_with_a_second_tab(&["a", "b"]);
        let waiting = doing("b", Activity::Waiting);
        app.set_sessions(vec![session("a"), waiting, session("shell")]);
        assert_eq!(app.tabs().current_index(), 1);
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn a_tab_shows_what_most_needs_the_user_in_it() {
        let mut app = app_with_a_second_tab(&["a", "b"]);
        assert_eq!(app.tab_status(0), None);
        let sessions = |a, b| vec![doing("a", a), doing("b", b), session("shell")];
        app.set_sessions(sessions(Activity::Working, Activity::Idle));
        assert_eq!(app.tab_status(0), Some(Status::Working));
        app.set_sessions(sessions(Activity::Working, Activity::Done));
        assert_eq!(app.tab_status(0), Some(Status::Done));
        app.set_sessions(sessions(Activity::Waiting, Activity::Done));
        assert_eq!(app.tab_status(0), Some(Status::Waiting));
        assert_eq!(app.tab_status(1), None);
    }

    #[test]
    fn a_flow_step_joins_the_tab_its_run_is_in() {
        let mut app = app_with_a_run(crate::flow_run::StepState::Running);
        press(&mut app, KeyCode::Char('t'));
        let mut run = app.flows()[0].clone();
        run.steps[1].session = Some("ship-1-review-2".into());
        app.set_flows(vec![run]);
        app.set_sessions(
            ["ship-1-review-2", "shell", "ship-1-plan", "elsewhere"]
                .map(session)
                .to_vec(),
        );
        assert_eq!(in_sidebar(&app), ["elsewhere"]);
        assert_eq!(app.tabs().tab_of("ship-1-review-2"), Some(0));
    }

    #[test]
    fn clicking_a_tab_goes_to_it_and_gives_the_sidebar_the_keyboard() {
        let mut app = app_with_a_second_tab(&["a"]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        app.on_mouse(CLICK, Hit::Tab(0));
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_tabs_kept_come_back_with_their_own_sessions_and_selections() {
        let mut app = app_with(&["a", "b"]);
        app.select("b");
        press(&mut app, KeyCode::Char('t'));
        app.set_sessions(["a", "b", "shell"].map(session).to_vec());
        let kept = app.tabs_to_keep();

        let mut reopened = app_with(&["a", "b", "shell"]);
        reopened.set_tabs(kept);
        assert_eq!(reopened.tabs().current_index(), 1);
        assert_eq!(in_sidebar(&reopened), ["shell"]);
        press(&mut reopened, KeyCode::Char('1'));
        assert_eq!(in_sidebar(&reopened), ["a", "b"]);
        assert_eq!(selected_name(&reopened), Some("b"));
    }

    #[test]
    fn tabs_kept_with_sessions_since_gone_drop_them() {
        let app = app_with_splits(&["a", "b"], 1);
        let kept = app.tabs_to_keep();
        let mut reopened = app_with(&["b", "new"]);
        reopened.set_tabs(kept);
        assert!(reopened.splits().is_empty());
        assert_eq!(in_sidebar(&reopened), ["b", "new"]);
    }

    const CLICK: MouseEventKind = MouseEventKind::Down(MouseButton::Left);

    /// The sidebar row `name` is drawn on.
    fn row_of(app: &App, name: &str) -> usize {
        let index = app.sessions().iter().position(|s| s.name == name).unwrap();
        app.rows()
            .iter()
            .position(|row| *row == Row::Session(index))
            .unwrap()
    }

    #[test]
    fn clicking_a_session_row_selects_it_and_a_heading_does_nothing() {
        let mut app = app_with(&["a", "b", "c"]);
        app.on_mouse(CLICK, Hit::SidebarRow(row_of(&app, "c")));
        assert_eq!(selected_name(&app), Some("c"));

        // Sessions outside git sit under two headings, on the first rows.
        assert!(matches!(app.rows()[0], Row::OutsideGit));
        app.on_mouse(CLICK, Hit::SidebarRow(0));
        assert_eq!(selected_name(&app), Some("c"));
    }

    #[test]
    fn clicking_a_row_gives_the_sidebar_the_keyboard() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Enter);
        app.on_mouse(CLICK, Hit::SidebarRow(row_of(&app, "b")));
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn clicking_anywhere_in_the_sidebar_gives_it_the_keyboard() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Enter);
        app.on_mouse(CLICK, Hit::Sidebar);
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(selected_name(&app), Some("a"));

        // A heading takes the keyboard too, the selection staying put.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));
        assert!(matches!(app.rows()[0], Row::OutsideGit));
        app.on_mouse(CLICK, Hit::SidebarRow(0));
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn clicking_a_pane_hands_it_the_keyboard_if_it_takes_keys() {
        let mut app = app_with(&["a"]);
        let pane = Hit::Pane {
            slot: Slot::Selected,
            cell: Some((2, 3)),
        };
        app.on_mouse(CLICK, pane);
        assert_eq!(app.focus(), Focus::Pane(Slot::Selected));

        let mut app = App::new(None);
        app.set_sessions(vec![ended("done")]);
        app.on_mouse(CLICK, pane);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn the_wheel_over_the_sidebar_moves_the_selection() {
        let mut app = app_with(&["a", "b", "c"]);
        app.on_mouse(MouseEventKind::ScrollDown, Hit::Sidebar);
        app.on_mouse(MouseEventKind::ScrollDown, Hit::SidebarRow(0));
        assert_eq!(selected_name(&app), Some("c"));
        app.on_mouse(MouseEventKind::ScrollUp, Hit::Sidebar);
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn the_wheel_over_a_pane_scrolls_its_history() {
        let mut app = app_with(&["a"]);
        let over = |slot| Hit::Pane { slot, cell: None };
        assert_eq!(
            app.on_mouse(MouseEventKind::ScrollUp, over(Slot::Selected)),
            Some(Action::ScrollBack(Slot::Selected))
        );
        assert_eq!(
            app.on_mouse(MouseEventKind::ScrollDown, over(Slot::Selected)),
            Some(Action::ScrollForward(Slot::Selected))
        );
    }

    #[test]
    fn the_mouse_waits_while_a_question_is_asked() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('x'));
        app.on_mouse(CLICK, Hit::SidebarRow(row_of(&app, "b")));
        assert_eq!(selected_name(&app), Some("a"));
        assert_eq!(
            app.confirm(),
            Some(&Confirm::Kill("a".into())),
            "the question is still asked"
        );
    }

    #[test]
    fn the_arrows_turn_the_keys_pages_round_and_round() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('?'));
        let pages = app.keys_pages();
        assert!(pages > 1, "80 by 24 takes more than a page");
        press(&mut app, KeyCode::Right);
        assert_eq!(app.keys_page(), 1);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        assert_eq!(
            app.keys_page(),
            pages - 1,
            "back from the first is the last"
        );
        assert!(app.showing_keys());
        assert_eq!(press(&mut app, KeyCode::Char('q')), None);
        assert!(!app.showing_keys());
    }

    #[test]
    fn a_click_puts_the_keys_away_without_selecting() {
        let mut app = app_with(&["a", "b"]);
        press(&mut app, KeyCode::Char('?'));
        app.on_mouse(MouseEventKind::ScrollDown, Hit::Sidebar);
        assert!(app.showing_keys(), "the wheel leaves it open");
        app.on_mouse(CLICK, Hit::SidebarRow(row_of(&app, "b")));
        assert!(!app.showing_keys());
        assert_eq!(selected_name(&app), Some("a"));
    }

    /// The name of the session the sidebar's bar is on.
    fn cursor_name(app: &App) -> Option<&str> {
        let index = app.sidebar_cursor()?;
        Some(app.sessions()[index].name.as_str())
    }

    #[test]
    fn slash_shows_only_the_matching_sessions_and_enter_selects_one() {
        let mut app = app_with(&["planner", "refund-fix", "reviewer"]);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "fix");
        let shown: Vec<&str> = app
            .matches()
            .iter()
            .map(|&index| app.sessions()[index].name.as_str())
            .collect();
        assert_eq!(shown, ["refund-fix"]);
        assert_eq!(cursor_name(&app), Some("refund-fix"));
        assert_eq!(selected_name(&app), Some("planner"), "not until Enter");

        press(&mut app, KeyCode::Enter);
        assert!(app.filter().is_none());
        assert_eq!(selected_name(&app), Some("refund-fix"));
    }

    #[test]
    fn esc_closes_the_filter_and_leaves_the_selection_where_it_was() {
        let mut app = app_with(&["planner", "refund-fix"]);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "fix");
        press(&mut app, KeyCode::Esc);
        assert!(app.filter().is_none());
        assert_eq!(selected_name(&app), Some("planner"));
        assert_eq!(app.matches().len(), 2, "every session shows again");
    }

    #[test]
    fn slash_finds_sessions_in_every_tab_and_enter_brings_one_to_the_front() {
        // "a" and "b" in the first tab, the new tab's shell in front.
        let mut app = app_with_a_second_tab(&["a", "b"]);
        assert_eq!(app.tabs().current_index(), 1);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "b");
        let shown: Vec<&str> = app
            .matches()
            .iter()
            .map(|&index| app.sessions()[index].name.as_str())
            .collect();
        assert_eq!(shown, ["b"]);
        let b = app.sidebar_cursor().unwrap();
        assert_eq!(app.elsewhere(b).as_deref(), Some("1"));

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(selected_name(&app), Some("b"));
        assert_eq!(app.elsewhere(b), None, "only while the filter is open");
    }

    #[test]
    fn a_click_on_a_match_in_another_tab_picks_it() {
        let mut app = app_with_a_second_tab(&["a", "b"]);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "a");
        let row = row_of(&app, "a");
        app.on_mouse(CLICK, Hit::SidebarRow(row));
        assert!(app.filter().is_none());
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(selected_name(&app), Some("a"));
    }

    #[test]
    fn letters_type_into_the_filter_and_the_arrows_move_its_bar() {
        let mut app = app_with(&["job-one", "job-two", "other"]);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "j");
        assert_eq!(app.filter().map(|f| f.input.text()), Some("j"));
        assert_eq!(cursor_name(&app), Some("job-one"));
        press(&mut app, KeyCode::Down);
        assert_eq!(cursor_name(&app), Some("job-two"));
        press(&mut app, KeyCode::Down);
        assert_eq!(
            cursor_name(&app),
            Some("job-two"),
            "the bar stops at the last"
        );
        assert_eq!(app.marked_letters(1), vec![0]);
    }

    #[test]
    fn slash_finds_a_session_by_the_name_of_its_tab() {
        let mut app = app_with_a_second_tab(&["a", "b"]);
        app.go_to_tab(0);
        press(&mut app, KeyCode::Char('T'));
        answer(&mut app, "review");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "review");
        let shown: Vec<&str> = app
            .matches()
            .iter()
            .map(|&index| app.sessions()[index].name.as_str())
            .collect();
        assert_eq!(shown, ["a", "b"]);
    }

    #[test]
    fn slash_finds_a_project_with_no_sessions_and_enter_selects_it() {
        let mut app = app_with_an_empty_worktree();
        app.set_known_projects(vec![known("app"), known("api")]);
        press(&mut app, KeyCode::Char('/'));
        assert!(
            app.found()
                .iter()
                .all(|found| matches!(found, Found::Session(_))),
            "before a word is typed, only sessions"
        );
        type_text(&mut app, "api");
        let api = PathBuf::from("/code/api");
        assert_eq!(app.found(), [Found::Worktree(api.clone())]);
        let rows = app.rows();
        let heading = Row::Project {
            name: "api".into(),
            path: api.clone(),
        };
        let at = rows.iter().position(|row| *row == heading).unwrap();
        assert!(matches!(rows[at + 1], Row::Worktree { main: true, .. }));
        assert_eq!(rows[at + 2], Row::NoSessions(api.clone()));
        assert_eq!(app.filter_row(), Some(at + 2));

        press(&mut app, KeyCode::Enter);
        assert!(app.filter().is_none());
        assert_eq!(
            app.selected_empty_worktree().map(|w| w.project.as_str()),
            Some("api")
        );
    }

    #[test]
    fn slash_finds_a_worktree_with_no_sessions_by_its_branch() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "old");
        let old = PathBuf::from("/code/app.worktrees/old");
        assert_eq!(app.found(), [Found::Worktree(old)]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(empty_branch(&app), Some("old"));
    }

    #[test]
    fn the_selection_on_a_worktree_with_no_sessions_outlasts_the_filter() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(empty_branch(&app), Some("old"));
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "planner");
        app.set_sessions(vec![in_worktree("planner", "main", State::Running)]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(empty_branch(&app), Some("old"));
    }

    #[test]
    fn slash_finds_an_open_pull_request_and_enter_opens_it_in_the_view() {
        let mut app = with_agents(&["claude"], vec![in_repo("planner", "main")]);
        let fix = PullRequest {
            title: "Fix the login redirect".into(),
            ..pull_request(57, "fix-login")
        };
        let dark = PullRequest {
            title: "Dark mode".into(),
            ..pull_request(58, "dark")
        };
        let app_path = PathBuf::from("/code/app");
        app.set_pull_requests(app_path.clone(), on_github(vec![fix, dark]), Instant::now());
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "dark");
        let found = Found::PullRequest {
            project: app_path.clone(),
            number: 58,
        };
        assert_eq!(app.found(), [found]);
        let (_, marked) = app.found_pull_request(&app_path, 58).unwrap();
        assert_eq!(marked, vec![0, 1, 2, 3]);

        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::ListPullRequests(app_path.clone()))
        );
        assert!(app.filter().is_none());
        let view = app.pull_requests_view().unwrap();
        assert_eq!(view.highlighted().map(|pr| pr.number), Some(58));

        // A click on one picks it too.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "#57");
        let rows = app.rows();
        let row = rows
            .iter()
            .position(|row| matches!(row, Row::PullRequest { number: 57, .. }))
            .unwrap();
        assert_eq!(
            app.on_mouse(CLICK, Hit::SidebarRow(row)),
            Some(Action::ListPullRequests(app_path))
        );
        let view = app.pull_requests_view().unwrap();
        assert_eq!(view.highlighted().map(|pr| pr.number), Some(57));
    }

    #[test]
    fn slash_finds_no_merged_pull_request_and_no_draft_while_they_re_hidden() {
        let mut app = with_agents(&["claude"], vec![in_repo("planner", "main")]);
        let merged = PullRequest {
            title: "Login, the old way".into(),
            merged: true,
            ..pull_request(41, "old-login")
        };
        let draft = PullRequest {
            title: "Login, a new way".into(),
            draft: true,
            ..pull_request(58, "new-login")
        };
        let app_path = PathBuf::from("/code/app");
        app.set_pull_requests(
            app_path.clone(),
            on_github(vec![merged, draft]),
            Instant::now(),
        );
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "login");
        let draft = Found::PullRequest {
            project: app_path,
            number: 58,
        };
        assert_eq!(app.found(), [draft]);
        let mut config = Config::default();
        config.forge.hide_draft_prs = true;
        app.set_features(&config);
        assert!(app.found().is_empty());
    }

    #[test]
    fn slash_asks_once_for_the_pull_requests_of_projects_no_session_is_in() {
        let mut app = with_agents(&["claude"], vec![in_repo("planner", "main")]);
        app.set_known_projects(vec![known("app"), known("api")]);
        let api = PathBuf::from("/code/api");
        assert_eq!(
            press(&mut app, KeyCode::Char('/')),
            Some(Action::FindPullRequests(vec![api.clone()]))
        );
        press(&mut app, KeyCode::Esc);
        let none = PullRequest {
            title: "Rate limits".into(),
            ..pull_request(3, "limits")
        };
        app.set_pull_requests(api.clone(), on_github(vec![none]), Instant::now());
        assert_eq!(press(&mut app, KeyCode::Char('/')), None);
        type_text(&mut app, "rate");
        assert_eq!(
            app.found(),
            [Found::PullRequest {
                project: api,
                number: 3
            }]
        );

        let mut config = Config::default();
        config.plugins.insert("github".into(), false);
        app.set_features(&config);
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Char('/')), None);
        type_text(&mut app, "rate");
        assert!(app.found().is_empty(), "nothing from a forge with it off");
    }

    #[test]
    fn tab_keeps_the_filter_to_one_status_and_shift_tab_goes_back() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            doing("asker", Activity::Waiting),
            doing("busy", Activity::Working),
            session("idler"),
            ended("gone"),
        ]);
        app.set_known_projects(vec![known("api")]);
        let names = |app: &App| -> Vec<String> {
            let mut names: Vec<String> = app
                .matches()
                .iter()
                .map(|&index| app.sessions()[index].name.clone())
                .collect();
            names.sort();
            names
        };
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Tab);
        assert_eq!(
            app.filter().and_then(|f| f.status),
            Some(StatusFilter::Waiting)
        );
        assert_eq!(names(&app), ["asker"]);
        assert_eq!(cursor_name(&app), Some("asker"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(names(&app), ["busy"]);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(names(&app), ["idler"]);
        press(&mut app, KeyCode::Tab);
        assert_eq!(names(&app), ["gone"]);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(names(&app), ["idler"]);

        // Only sessions have a status: a project that matches isn't found.
        type_text(&mut app, "i");
        assert_eq!(names(&app), ["idler"]);
        assert_eq!(app.found().len(), 1);

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.filter().and_then(|f| f.status), None);
        assert!(
            app.found()
                .contains(&Found::Worktree(PathBuf::from("/code/api")))
        );
    }

    #[test]
    fn slash_finds_a_flow_run_by_its_goal_and_enter_selects_the_step_it_is_at() {
        let mut app = app_with_a_run(crate::flow_run::StepState::Running);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "retries");
        assert_eq!(
            app.found(),
            [
                Found::Flow("ship-1".into()),
                Found::Session("ship-1-plan".into()),
                Found::Session("ship-1-review".into()),
            ]
        );
        assert_eq!(
            app.filter().and_then(|f| f.highlighted.clone()),
            Some(Found::Flow("ship-1".into()))
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(selected_name(&app), Some("ship-1-review"));
    }

    /// A session in the `app` project, on `branch`.
    fn in_repo(name: &str, branch: &str) -> SessionInfo {
        SessionInfo {
            front: None,
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: PathBuf::from("/code/app"),
                path: PathBuf::from(format!("/code/app/{branch}")),
                main: branch == "main",
                branch: Some(branch.into()),
                in_progress: None,
            }),
            ..session(name)
        }
    }

    fn pull_request(number: u64, branch: &str) -> PullRequest {
        PullRequest {
            forge: Forge::GitHub,
            number,
            title: "a change".into(),
            author: "ana".into(),
            branch: branch.into(),
            from_fork: false,
            local_branch: branch.into(),
            draft: false,
            conflicts: false,
            merged: false,
            checks: forge::Checks::None,
            review: forge::Review::None,
            updated_at: "2026-10-02T09:30:00Z".into(),
            url: format!("https://github.com/acme/app/pull/{number}"),
        }
    }

    fn on_github(pull_requests: Vec<PullRequest>) -> Result<(Forge, Vec<PullRequest>), String> {
        Ok((Forge::GitHub, pull_requests))
    }

    #[test]
    fn o_opens_the_pull_request_of_the_selected_sessions_branch() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("fixer", "fix-login")]);
        let found = on_github(vec![pull_request(57, "fix-login")]);
        app.set_pull_requests(PathBuf::from("/code/app"), found, Instant::now());
        assert_eq!(
            press(&mut app, KeyCode::Char('o')),
            Some(Action::OpenInBrowser {
                project: PathBuf::from("/code/app"),
                topic: Topic::PullRequest(57),
            })
        );
    }

    #[test]
    fn o_says_why_there_is_no_pull_request_to_open() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("fixer", "fix-login")]);
        assert_eq!(press(&mut app, KeyCode::Char('o')), None);
        assert_eq!(app.notice(), Some("still asking about app's pull requests"));

        let found = on_github(vec![pull_request(9, "other")]);
        app.set_pull_requests(PathBuf::from("/code/app"), found, Instant::now());
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.notice(), Some("no open pull request for fix-login"));

        let found = Ok((Forge::GitLab, Vec::new()));
        app.set_pull_requests(PathBuf::from("/code/app"), found, Instant::now());
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.notice(), Some("no open merge request for fix-login"));

        let not_on_one = "app's remote is on this machine, not GitHub or GitLab".to_string();
        app.set_pull_requests(
            PathBuf::from("/code/app"),
            Err(not_on_one.clone()),
            Instant::now(),
        );
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.notice(), Some(not_on_one.as_str()));
    }

    #[test]
    fn a_worktree_finds_a_forks_pull_request_by_its_own_branch() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        let fork = PullRequest {
            from_fork: true,
            local_branch: "ana/main".into(),
            ..pull_request(58, "main")
        };
        app.set_pull_requests(
            PathBuf::from("/code/app"),
            on_github(vec![fork]),
            Instant::now(),
        );
        assert!(app.pull_request(Path::new("/code/app"), "main").is_none());
        assert!(
            app.pull_request(Path::new("/code/app"), "ana/main")
                .is_some()
        );
    }

    #[test]
    fn capital_o_lists_the_pull_requests_and_enter_starts_in_one() {
        let mut app = with_agents(&["claude"], vec![in_repo("planner", "main")]);
        let found = on_github(vec![pull_request(57, "fix-login")]);
        app.set_pull_requests(PathBuf::from("/code/app"), found, Instant::now());
        assert_eq!(
            press(&mut app, KeyCode::Char('O')),
            Some(Action::ListPullRequests(PathBuf::from("/code/app")))
        );
        // What was listed last shows while the forge is asked again.
        let view = app.pull_requests_view().unwrap();
        assert_eq!(view.highlighted().map(|pr| pr.number), Some(57));

        press(&mut app, KeyCode::Enter);
        assert!(app.pull_requests_view().is_none());
        let panel = app.launcher().unwrap();
        assert_eq!(panel.title(), "New session · app ⎇ fix-login");
        assert_eq!(
            panel.task().text(),
            "Work on pull request #57: a change (https://github.com/acme/app/pull/57)"
        );
        let Some(Action::Start { place, purpose, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("Enter should start the session");
        };
        assert_eq!(
            place,
            Place::PullRequest(Checkout {
                project: PathBuf::from("/code/app"),
                branch: "fix-login".into(),
                fetch: "fix-login".into(),
            })
        );
        // Its agent is told of the pull request, as `crystal task --pr` is.
        let about = purpose.brief.pull_request.unwrap();
        assert_eq!(
            (about.number, about.branch.as_deref()),
            (57, Some("fix-login"))
        );
    }

    #[test]
    fn a_pull_requests_diff_opens_over_the_view_and_closes_back_to_it() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        let found = on_github(vec![pull_request(57, "fix-login")]);
        app.set_pull_requests(PathBuf::from("/code/app"), found, Instant::now());
        press(&mut app, KeyCode::Char('O'));
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(
            app.on_key(ctrl_d),
            Some(Action::ReadDiff {
                dir: PathBuf::from("/code/app"),
                against: Against::PullRequest {
                    forge: Forge::GitHub,
                    number: 57
                },
            })
        );
        assert!(matches!(app.view(), Some(View::Diff(_))));
        press(&mut app, KeyCode::Esc);
        assert!(app.view().is_none());
        assert!(app.pull_requests_view().is_some());
    }

    #[test]
    fn big_e_opens_the_tree_browser_and_ctrl_e_edits_a_file_in_a_session() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        assert_eq!(
            press(&mut app, KeyCode::Char('E')),
            Some(Action::ReadFiles(PathBuf::from("/code/app/main")))
        );
        assert!(matches!(app.view(), Some(View::Tree(_))));
        let files = vec!["src/main.rs".to_string(), "README.md".to_string()];
        // The directory comes first, and shows its names: nothing to read.
        assert_eq!(app.files_read(Path::new("/code/app/main"), Ok(files)), None);
        assert_eq!(
            press(&mut app, KeyCode::Down),
            Some(Action::ReadPreview {
                dir: PathBuf::from("/code/app/main"),
                path: "README.md".into(),
            })
        );
        let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(
            app.on_key(ctrl_e),
            Some(Action::Edit {
                dir: PathBuf::from("/code/app/main"),
                path: "README.md".into(),
                line: None,
                name: "README.md".into(),
            })
        );
        assert!(app.view().is_none());
    }

    #[test]
    fn a_comment_goes_to_the_forge_and_its_answer_comes_back_to_the_view() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        let found = on_github(vec![pull_request(57, "fix-login")]);
        app.set_pull_requests(PathBuf::from("/code/app"), found, Instant::now());
        press(&mut app, KeyCode::Char('O'));
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        type_text(&mut app, "LGTM");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Comment {
                project: PathBuf::from("/code/app"),
                topic: Topic::PullRequest(57),
                text: "LGTM".into(),
            })
        );
        app.commented(Path::new("/code/app"), Topic::PullRequest(57), Ok(()));
        assert_eq!(app.notice(), Some("commented on #57"));
        assert!(app.pull_requests_view().unwrap().comment.is_none());
        // It's read again, with the comment.
        assert_eq!(
            app.topic_to_read(),
            Some((PathBuf::from("/code/app"), Topic::PullRequest(57)))
        );
    }

    fn issue(number: u64, title: &str) -> Issue {
        Issue {
            number,
            title: title.into(),
            labels: Vec::new(),
            updated_at: "2026-10-02T09:30:00Z".into(),
            author: "ana".into(),
            url: format!("https://github.com/acme/app/issues/{number}"),
        }
    }

    #[test]
    fn enter_on_an_issue_opens_the_panel_ready_to_fix_it() {
        let mut app = with_agents(&["claude"], vec![in_repo("planner", "main")]);
        assert_eq!(
            press(&mut app, KeyCode::Char('i')),
            Some(Action::ListIssues(PathBuf::from("/code/app")))
        );
        app.set_issues(
            Path::new("/code/app"),
            Ok((Forge::GitHub, vec![issue(42, "Fix login redirect")])),
            Instant::now(),
        );
        press(&mut app, KeyCode::Enter);
        assert!(app.issues_view().is_none());
        let panel = app.launcher().unwrap();
        assert_eq!(panel.title(), "New session · app ⎇ 42-fix-login-redirect");
        assert_eq!(
            panel.task().text(),
            "Fix issue #42: Fix login redirect (https://github.com/acme/app/issues/42)"
        );
        let Some(Action::Start {
            place,
            command,
            purpose,
        }) = press(&mut app, KeyCode::Enter)
        else {
            panic!("Enter should start the session");
        };
        assert_eq!(
            place,
            Place::NewWorktree {
                branch: "42-fix-login-redirect".into(),
                base: Some(PathBuf::from("/code/app")),
                made_up: false,
            }
        );
        assert_eq!(command[0], "claude");
        assert!(command[2].starts_with("Fix issue #42"));
        assert_eq!(purpose.brief.issue.unwrap().number, 42);
    }

    #[test]
    fn ctrl_r_asks_the_forge_again_and_the_issues_open_on_those_listed_last() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        let project = PathBuf::from("/code/app");
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        let merged = PullRequest {
            merged: true,
            ..pull_request(41, "startup")
        };
        app.set_pull_requests(project.clone(), on_github(vec![merged]), Instant::now());
        press(&mut app, KeyCode::Char('O'));
        assert_eq!(
            app.on_key(ctrl_r),
            Some(Action::ListPullRequests(project.clone()))
        );
        // A merged one has nothing left to work on.
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(
            app.notice(),
            Some("#41 has merged: there's no work left on it")
        );
        press(&mut app, KeyCode::Esc);

        // The poller's issues are there as the view opens.
        let listed = Ok((Forge::GitHub, vec![issue(42, "Fix login redirect")]));
        app.set_issues(&project, listed, Instant::now());
        assert_eq!(
            press(&mut app, KeyCode::Char('i')),
            Some(Action::ListIssues(project.clone()))
        );
        let view = app.issues_view().unwrap();
        assert_eq!(view.highlighted().map(|issue| issue.number), Some(42));
        assert_eq!(app.on_key(ctrl_r), Some(Action::ListIssues(project)));
    }

    #[test]
    fn an_issue_edited_keeps_its_title_and_text_over_what_was_asked_before_it_was_saved() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        let project = PathBuf::from("/code/app");
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let (opened, refreshed, saved, after) = (at(0), at(1), at(2), at(3));
        let listed = |title: &str| Ok((Forge::GitHub, vec![issue(42, title)]));
        let read = |body: &str| {
            Ok(forge::IssueDetail {
                body: body.into(),
                comments: Vec::new(),
            })
        };
        let title = |app: &App| {
            app.issues_view()
                .unwrap()
                .highlighted()
                .unwrap()
                .title
                .clone()
        };
        let body = |app: &App| match app.issues_view().unwrap().list.detail(42) {
            Some(Ok(detail)) => detail.body.clone(),
            other => panic!("{other:?}"),
        };
        press(&mut app, KeyCode::Char('i'));
        app.set_issues(&project, listed("Fix login"), opened);
        app.set_issue(&project, 42, read("It loops."), opened);

        // The forge is asked again just before the edit is saved, and
        // answers after it: the edit stays.
        let edit = (
            "Fix login on Safari".to_string(),
            "It loops on Safari.".to_string(),
        );
        app.issue_edited(&project, 42, edit, Ok(()), saved);
        app.set_issues(&project, listed("Fix login"), refreshed);
        app.set_issue(&project, 42, read("It loops."), refreshed);
        assert_eq!(title(&app), "Fix login on Safari");
        assert_eq!(body(&app), "It loops on Safari.");

        // A list asked before the one kept is dropped.
        let more = Ok((
            Forge::GitHub,
            vec![issue(42, "Fix login"), issue(7, "Dark mode")],
        ));
        app.set_issues(&project, more, opened);
        let view = app.issues_view().unwrap();
        assert_eq!(view.list.shown().len(), 1);

        // The view opens again on the title given.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(title(&app), "Fix login on Safari");

        // Asked after it was saved, what the forge says goes: someone may
        // have changed it since.
        app.set_issues(&project, listed("Fix the login loop"), after);
        app.set_issue(&project, 42, read("Loops."), after);
        assert_eq!(title(&app), "Fix the login loop");
        assert_eq!(body(&app), "Loops.");
    }

    #[test]
    fn a_pull_request_list_asked_before_the_one_kept_is_dropped() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        let project = PathBuf::from("/code/app");
        let earlier = Instant::now();
        let later = earlier + Duration::from_secs(1);
        let open = pull_request(57, "fix-login");
        app.set_pull_requests(project.clone(), on_github(vec![open]), later);
        app.set_pull_requests(project.clone(), on_github(Vec::new()), earlier);
        assert!(app.pull_request(&project, "fix-login").is_some());
    }

    #[test]
    fn i_outside_a_repository_or_github_says_why() {
        let mut app = app_with(&["shell"]);
        assert_eq!(press(&mut app, KeyCode::Char('i')), None);
        assert_eq!(app.notice(), Some("shell isn't in a git repository"));

        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        let reason = "gh: not logged in".to_string();
        app.set_pull_requests(
            PathBuf::from("/code/app"),
            Err(reason.clone()),
            Instant::now(),
        );
        assert_eq!(press(&mut app, KeyCode::Char('i')), None);
        assert_eq!(app.notice(), Some(reason.as_str()));
        assert!(app.issues_view().is_none());
    }

    #[test]
    fn esc_closes_the_issues_view() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "q");
        assert!(app.issues_view().is_some(), "q types into its filter");
        press(&mut app, KeyCode::Esc);
        assert!(app.issues_view().is_none());
    }

    #[test]
    fn m_opens_the_memory_of_the_project_from_any_of_its_worktrees() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("fixer", "fix/ledger")]);
        assert_eq!(
            press(&mut app, KeyCode::Char('m')),
            Some(Action::ReadMemory(PathBuf::from("/code/app")))
        );
        assert!(matches!(app.view(), Some(View::Memory(_))));
    }

    #[test]
    fn enter_in_the_memory_view_edits_the_entry_s_file_in_a_session_of_its_own() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("fixer", "fix/ledger")]);
        press(&mut app, KeyCode::Char('m'));
        let entry = crate::memory::Entry {
            id: 3,
            kind: crate::memory::Kind::Gotcha,
            text: "the ledger needs redis".into(),
            files: vec!["src/ledger.rs".into()],
            source: crate::memory::Source::User,
            created: 0,
            seen: 1,
            last_seen: 0,
            anchors: Default::default(),
            checkout: None,
        };
        let listed = crate::memory::Listed {
            entry,
            freshness: crate::memory::Freshness::Fresh,
        };
        app.memory_read(Path::new("/code/app"), Ok(vec![listed]));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Edit {
                dir: PathBuf::from("/code/app"),
                path: "src/ledger.rs".into(),
                line: None,
                name: "ledger.rs".into(),
            })
        );
        assert!(app.view().is_none());
    }

    #[test]
    fn m_with_memory_off_says_so_and_opens_nothing() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("fixer", "main")]);
        app.memory_on = false;
        assert_eq!(press(&mut app, KeyCode::Char('m')), None);
        assert!(app.view().is_none());
        assert_eq!(app.notice(), Some(plugins::off("memory").as_str()));
    }

    /// A session given `goal` to do, closed with `outcome` if it's given:
    /// whether it failed, and how it went.
    fn with_task(name: &str, goal: &str, outcome: Option<(bool, &str)>) -> SessionInfo {
        let closed = |(failed, summary)| {
            let state = if failed {
                TaskState::Failed
            } else {
                TaskState::Done
            };
            TaskOutcome::new(state, summary, 1)
        };
        SessionInfo {
            task: Some(TaskInfo {
                id: Some(1),
                goal: goal.into(),
                background: false,
                backlog: None,
                waiting: false,
                created: 0,
                outcome: outcome.map(closed),
                brief: Default::default(),
            }),
            ..in_project(name, "shop")
        }
    }

    fn backlog_of(items: &[(u64, &str)]) -> Backlog {
        Backlog {
            project: "shop".into(),
            path: PathBuf::from("/code/shop"),
            items: items
                .iter()
                .map(|(number, text)| BacklogItem {
                    number: *number,
                    text: text.to_string(),
                    body: String::new(),
                    tags: Vec::new(),
                    done: false,
                    created: 0,
                    closed: None,
                })
                .collect(),
            tasks: Vec::new(),
        }
    }

    #[test]
    fn c_asks_how_the_task_went_then_for_a_line_on_it() {
        let mut app = App::new(None);
        app.set_sessions(vec![with_task("fixer", "fix the tests", None)]);
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.closing(), Some("fixer"));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.closing(), None);
        type_text(&mut app, "no network");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::CloseTask {
                name: "fixer".into(),
                failed: true,
                summary: "no network".into(),
            })
        );
    }

    #[test]
    fn c_leaves_the_task_open_on_any_other_key_and_says_when_there_is_none() {
        let mut app = App::new(None);
        app.set_sessions(vec![
            with_task("fixer", "fix the tests", None),
            session("plain"),
            with_task("finished", "ship it", Some((false, "shipped"))),
        ]);
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(
            press(&mut app, KeyCode::Char('q')),
            None,
            "q doesn't quit here"
        );
        assert_eq!(app.closing(), None);
        assert!(app.prompt().is_none());

        app.select("plain");
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.notice(), Some("plain has no task to close"));
        app.select("finished");
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.notice(), Some("finished's task is closed already"));
    }

    #[test]
    fn a_task_has_a_line_under_its_session_while_tasks_are_on() {
        let mut app = App::new(None);
        app.set_sessions(vec![with_task("fixer", "fix the tests", None)]);
        app.set_features(&Config::default());
        assert_eq!(app.rows().last(), Some(&Row::Task(0)));

        // With tasks off, the line goes, and so does `c`.
        app.tasks_on = false;
        assert!(!app.rows().contains(&Row::Task(0)));
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.closing(), None);
    }

    #[test]
    fn b_opens_the_projects_backlog_and_enter_starts_a_task_for_an_item() {
        let mut app = with_agents(&["claude"], vec![in_project("agent", "shop")]);
        assert_eq!(
            press(&mut app, KeyCode::Char('b')),
            Some(Action::ListBacklog(PathBuf::from("/code/shop")))
        );
        app.set_backlog(
            Path::new("/code/shop"),
            Ok(backlog_of(&[(3, "write the docs")])),
        );
        assert_eq!(app.backlog_view().unwrap().shown().len(), 1);

        // Letters are the view's: x asks, and only y removes.
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::ChangeBacklog {
                dir: PathBuf::from("/code/shop"),
                change: BacklogChange::Remove(3),
            })
        );

        press(&mut app, KeyCode::Enter);
        assert!(app.backlog_view().is_none());
        let panel = app.launcher().unwrap();
        assert_eq!(panel.task().text(), "write the docs");
        let Some(Action::Start { purpose, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("Enter should start the session");
        };
        assert_eq!(purpose.task.as_deref(), Some("write the docs"));
        assert_eq!(purpose.backlog, Some(3));

        // An item's body goes with it, under its line.
        press(&mut app, KeyCode::Char('b'));
        let mut backlog = backlog_of(&[(4, "ship it")]);
        backlog.items[0].body = "once it's green".into();
        app.set_backlog(Path::new("/code/shop"), Ok(backlog));
        press(&mut app, KeyCode::Enter);
        let panel = app.launcher().unwrap();
        assert_eq!(panel.task().text(), "ship it\n\nonce it's green");
    }

    #[test]
    fn the_panel_starts_claude_in_the_background_as_a_background_task() {
        let mut app = with_agents(&["claude"], vec![]);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "fix the tests");
        press(&mut app, KeyCode::Tab); // run
        press(&mut app, KeyCode::Tab); // how
        press(&mut app, KeyCode::Right);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::StartInBackground {
                place: Place::Directory(None),
                spec: TaskSpec {
                    prompt: "fix the tests".into(),
                    args: Vec::new(),
                },
                backlog: None,
                brief: TaskBrief::default(),
            })
        );
    }

    #[test]
    fn a_projects_heading_counts_its_backlog_only_when_there_is_something_to_do() {
        let mut app = App::new(None);
        let counts = HashMap::from([
            (PathBuf::from("/code/shop"), 3),
            (PathBuf::from("/code/blog"), 0),
        ]);
        app.set_backlog_counts(counts);
        assert_eq!(app.backlog_open(Path::new("/code/shop")), Some(3));
        assert_eq!(app.backlog_open(Path::new("/code/blog")), None);
        assert_eq!(app.backlog_open(Path::new("/code/else")), None);
    }

    /// The config file with a flow, `ship`: plan, then a review with a gate.
    fn config_with_a_flow() -> Config {
        crate::config::from_text(
            r#"
[[flow]]
name = "ship"
description = "Plan, then review"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"

[[flow.step]]
name = "review"
prompt = "Review it"
gate = true
"#,
        )
        .unwrap()
    }

    /// An app whose sessions `ship-1-plan` and `ship-1-review` are the steps
    /// of the run `ship-1`, which stands as `review` says, beside a session
    /// of its own.
    fn app_with_a_run(review: crate::flow_run::StepState) -> App {
        use crate::flow_run::{FlowRun, StepState};
        let config = config_with_a_flow();
        let mut run = FlowRun::new(
            "ship-1".into(),
            config.flows[0].clone(),
            &[],
            "add retries".into(),
            PathBuf::from("/"),
            Default::default(),
            0,
        );
        run.steps[0].state = StepState::Done;
        run.steps[0].session = Some("ship-1-plan".into());
        run.steps[1].state = review;
        run.steps[1].session = Some("ship-1-review".into());
        let mut app = App::new(None);
        app.set_flows(vec![run]);
        app.set_sessions(vec![
            session("ship-1-review"),
            session("shell"),
            session("ship-1-plan"),
        ]);
        app
    }

    #[test]
    fn a_runs_steps_follow_the_other_sessions_in_step_order() {
        let app = app_with_a_run(crate::flow_run::StepState::Running);
        let names: Vec<&str> = app.sessions().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["shell", "ship-1-plan", "ship-1-review"]);
        assert_eq!(
            app.flow_step_of(2)
                .map(|(run, step)| (run.name.as_str(), step)),
            Some(("ship-1", 1))
        );
        assert!(app.flow_step_of(0).is_none());
        assert!(app.rows().contains(&Row::Flow(0)));
    }

    #[test]
    fn g_goes_on_past_the_gate_of_the_selected_steps_run() {
        let mut app = app_with_a_run(crate::flow_run::StepState::AtGate);
        app.select("ship-1-plan");
        assert_eq!(
            press(&mut app, KeyCode::Char('g')),
            Some(Action::ApproveFlow("ship-1".into()))
        );
    }

    #[test]
    fn g_runs_a_step_that_failed_again() {
        let mut app = app_with_a_run(crate::flow_run::StepState::Failed);
        app.select("ship-1-review");
        assert_eq!(
            press(&mut app, KeyCode::Char('g')),
            Some(Action::RetryFlow("ship-1".into()))
        );
    }

    #[test]
    fn g_on_a_run_going_or_a_session_of_no_flow_says_why_not() {
        let mut app = app_with_a_run(crate::flow_run::StepState::Running);
        app.select("ship-1-review");
        assert_eq!(press(&mut app, KeyCode::Char('g')), None);
        assert_eq!(app.notice(), Some("ship-1 is still running"));
        app.select("shell");
        assert_eq!(press(&mut app, KeyCode::Char('g')), None);
        assert_eq!(app.notice(), Some("shell isn't a step of a flow"));
    }

    #[test]
    fn f_asks_for_notes_then_sends_the_run_back_with_them() {
        let mut app = app_with_a_run(crate::flow_run::StepState::AtGate);
        app.select("ship-1-review");
        assert_eq!(press(&mut app, KeyCode::Char('f')), None);
        assert_eq!(
            app.prompt().map(|prompt| &prompt.question),
            Some(&Question::SendFlowBack("ship-1".into()))
        );
        type_text(&mut app, "keep the old default");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::SendFlowBack {
                run: "ship-1".into(),
                notes: "keep the old default".into(),
            })
        );
    }

    #[test]
    fn f_on_a_run_not_at_a_gate_says_so() {
        let mut app = app_with_a_run(crate::flow_run::StepState::Running);
        app.select("ship-1-review");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.prompt().is_none());
        assert_eq!(app.notice(), Some("ship-1 isn't waiting at a gate"));
    }

    #[test]
    fn the_panel_offers_flows_and_starts_one_on_its_goal() {
        let mut app = with_agents(&["claude"], vec![]);
        app.set_launch_settings(&config_with_a_flow());
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Tab); // run
        while run_key(&app) != "flow:ship" {
            press(&mut app, KeyCode::Right);
        }
        let panel = app.launcher().unwrap();
        assert!(
            panel.title().starts_with("New flow run"),
            "{}",
            panel.title()
        );
        // A flow needs its goal.
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.launcher().unwrap().problem(),
            Some("say what the flow should do")
        );
        type_text(&mut app, "add retries");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::StartFlow {
                place: Place::Directory(None),
                flow: "ship".into(),
                goal: "add retries".into(),
            })
        );
        assert_eq!(app.memory().last_run.as_deref(), Some("flow:ship"));
    }

    #[test]
    fn without_claude_code_no_flow_is_offered() {
        let mut app = with_agents(&["codex"], vec![]);
        app.set_launch_settings(&config_with_a_flow());
        let setup = app.launch_setup(false);
        assert!(!setup.runs.iter().any(|run| matches!(run, Run::Flow(_))));
    }

    #[test]
    fn enter_on_a_failed_step_points_to_g_rather_than_running_it_alone() {
        let mut app = app_with_a_run(crate::flow_run::StepState::Failed);
        let mut sessions = app.sessions().to_vec();
        for session in &mut sessions {
            if session.name == "ship-1-review" {
                session.state = State::Exited { code: 1 };
            }
        }
        app.set_sessions(sessions);
        app.select("ship-1-review");
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(app.confirm().is_none());
        assert_eq!(
            app.notice(),
            Some("g runs review again, as a step of ship-1")
        );
    }

    #[test]
    fn with_the_flows_plugin_off_g_says_so_and_runs_arent_grouped() {
        let mut app = app_with_a_run(crate::flow_run::StepState::AtGate);
        let config = crate::config::from_text("[plugins]\nflows = false\n").unwrap();
        app.set_features(&config);
        app.select("ship-1-review");
        assert_eq!(press(&mut app, KeyCode::Char('g')), None);
        assert!(
            app.notice()
                .is_some_and(|notice| notice.starts_with("the flows plugin is off")),
            "{:?}",
            app.notice()
        );
        assert!(!app.rows().contains(&Row::Flow(0)));
    }

    #[test]
    fn comma_opens_the_settings_which_take_every_key_until_closed() {
        let mut app = app_with(&["agent"]);
        assert_eq!(
            press(&mut app, KeyCode::Char(',')),
            Some(Action::OpenSettings)
        );
        assert!(app.settings_view().is_some());
        // Nothing to change before the settings are read.
        assert_eq!(press(&mut app, KeyCode::Char(' ')), None);
        app.show_settings(settings_view::Current {
            path: PathBuf::from("/c"),
            config: Ok(Config::default()),
            model: None,
        });
        // `x` would kill the session from the sidebar; here it's nothing.
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        // The bar stops at the last.
        for _ in 0..30 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(
            press(&mut app, KeyCode::Char(' ')),
            Some(Action::ChangeSetting(settings_view::Change::HideDrafts(
                true
            )))
        );
        app.setting_failed("the file is read-only".into());
        assert_eq!(press(&mut app, KeyCode::Esc), Some(Action::CloseSettings));
        assert!(app.settings_view().is_none());
        assert_eq!(selected_name(&app), Some("agent"));
    }

    #[test]
    fn capital_u_lists_what_needs_you_in_every_tab_and_enter_goes_there() {
        let mut app = app_with_a_second_tab(&["a"]);
        app.set_sessions(vec![doing("a", Activity::Waiting), session("shell")]);
        press(&mut app, KeyCode::Char('U'));
        let row = app.needs_you_view().unwrap().highlighted().unwrap();
        assert_eq!(row.name, "a");
        // Its keys are the list's: in the sidebar, `x` would ask to kill.
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert!(app.confirm().is_none());
        press(&mut app, KeyCode::Enter);
        assert!(app.needs_you_view().is_none());
        assert_eq!(selected_name(&app), Some("a"));
        assert_eq!(app.tabs().current_index(), 0);
    }

    #[test]
    fn a_permission_is_answered_from_the_list_which_stays_open() {
        let mut app = App::new(None);
        let asking = SessionInfo {
            asking: Some(crate::protocol::Asking {
                tool: "Bash".into(),
                gist: "cargo test".into(),
            }),
            ..doing("fixer", Activity::Waiting)
        };
        app.set_sessions(vec![session("other"), asking]);
        press(&mut app, KeyCode::Char('U'));
        let answered = Action::Answer {
            name: "fixer".into(),
            answer: Answer::Always,
        };
        assert_eq!(press(&mut app, KeyCode::Char('Y')), Some(answered));
        // Answered, it leaves the list as the next list of sessions comes.
        app.set_sessions(vec![session("other"), doing("fixer", Activity::Working)]);
        assert!(app.needs_you_view().unwrap().highlighted().is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.needs_you_view().is_none());
    }

    /// An event about `session`, numbered `seq`.
    fn about(seq: u64, kind: crate::events::Kind, session: &SessionInfo) -> Event {
        Event {
            seq,
            session: Some(crate::events::SessionAbout::of(session)),
            ..Event::new(kind)
        }
    }

    #[test]
    fn the_timeline_follows_the_log_while_open_and_enter_goes_to_a_line_s_session() {
        let mut app = app_with(&["a"]);
        assert_eq!(
            press(&mut app, KeyCode::Char('a')),
            Some(Action::FollowEvents)
        );
        let worker = SessionInfo {
            id: "s1".into(),
            ..session("worker")
        };
        let gone = session("gone");
        let page = vec![
            about(2, crate::events::Kind::SessionEnded, &gone),
            about(1, crate::events::Kind::SessionStarted, &worker),
        ];
        assert_eq!(app.events_read(Ok(page)), None, "the log has no more");
        // Renamed since, it's still the session the line is about.
        let builder = SessionInfo {
            id: "s1".into(),
            ..session("builder")
        };
        app.set_sessions(vec![session("a"), builder]);
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.notice(), Some("gone has gone"));
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), Some(Action::StopFollowing));
        assert!(app.timeline_view().is_none());
        assert_eq!(selected_name(&app), Some("builder"));

        press(&mut app, KeyCode::Char('a'));
        assert_eq!(press(&mut app, KeyCode::Esc), Some(Action::StopFollowing));
    }

    #[test]
    fn what_happened_while_you_were_away_shows_until_a_key_and_marks_the_timeline() {
        let mut app = app_with(&["a"]);
        let finished = about(8, crate::events::Kind::SessionDone, &session("a"));
        app.set_away(&Tally::of(&[finished]));
        assert_eq!(
            app.away_line(),
            Some("while you were away: 1 session finished")
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.away_line(), None);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.timeline_view().unwrap().away_after, Some(7));
        press(&mut app, KeyCode::Esc);
        // Once it has been seen there, it's not new any more.
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.timeline_view().unwrap().away_after, None);
        // Nothing worth saying says nothing.
        app.set_away(&Tally::of(&[]));
        assert_eq!(app.away_line(), None);
    }

    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn space_opens_a_reply_box_and_enter_sends_it_as_the_user() {
        let mut app = app_with(&["a", "b"]);
        assert_eq!(press(&mut app, KeyCode::Char(' ')), None);
        let reply = app.reply().unwrap();
        assert_eq!(reply.name, "a");
        assert_eq!(reply.label, "typed into it, with Enter after it");
        // The box has every key: `q` is a letter, not quitting.
        type_in(&mut app, "quick");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        type_in(&mut app, "fix");
        let sent = Action::Reply {
            name: "a".into(),
            text: "quick\nfix".into(),
        };
        assert_eq!(press(&mut app, KeyCode::Enter), Some(sent));
        assert!(app.reply().unwrap().sending);

        app.replied("a", Ok(()));
        assert!(app.reply().is_none());
        assert_eq!(app.notice(), Some("sent to a"));
        // The sidebar has its keys back.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("b"));
    }

    #[test]
    fn a_refused_reply_keeps_what_was_written_and_says_why() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char(' '));
        type_in(&mut app, "go on");
        press(&mut app, KeyCode::Enter);
        let why = "agent_blocked: a is asking to use Bash: cargo test";
        app.replied("a", Err(why.into()));
        let reply = app.reply().unwrap();
        assert!(!reply.sending);
        assert_eq!(reply.problem.as_deref(), Some(why));
        assert_eq!(reply.text.text(), "go on");
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert!(app.reply().is_none());
        // Nothing written, Enter sends nothing.
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(app.reply().is_none());
    }

    #[test]
    fn space_takes_no_reply_for_an_ended_session_or_the_tui_s_own() {
        let mut app = App::new(Some("me".into()));
        app.set_sessions(vec![session("me"), ended("gone")]);
        press(&mut app, KeyCode::Char(' '));
        assert!(app.reply().is_none());
        assert_eq!(
            app.notice(),
            Some("crystal can't reply to the session it runs in")
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("gone"));
        press(&mut app, KeyCode::Char(' '));
        assert!(app.reply().is_none());
        assert!(app.notice().unwrap().contains("gone has ended"));
        // In a tab with nothing in it, there's nothing to reply to.
        let mut empty = App::new(None);
        press(&mut empty, KeyCode::Char(' '));
        assert!(empty.reply().is_none());
        assert_eq!(empty.notice(), Some("there's no session selected"));
    }

    #[test]
    fn space_in_a_task_s_pane_gives_it_a_follow_up() {
        let mut app = App::new(None);
        let task = SessionInfo {
            front: Some(Front::Task),
            ..doing("fixer", Activity::Done)
        };
        app.set_sessions(vec![task]);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.focus(), Focus::Pane(_)));
        press(&mut app, KeyCode::Char('x'));
        assert!(app.notice().unwrap().contains("space gives it a follow-up"));
        press(&mut app, KeyCode::Char(' '));
        let reply = app.reply().unwrap();
        assert_eq!(reply.name, "fixer");
        assert!(reply.label.starts_with("a follow-up"), "{}", reply.label);
        type_in(&mut app, "now the docs");
        let sent = Action::Reply {
            name: "fixer".into(),
            text: "now the docs".into(),
        };
        assert_eq!(press(&mut app, KeyCode::Enter), Some(sent));
    }

    #[test]
    fn a_reply_box_takes_a_paste_whole() {
        let mut app = App::new(None);
        let agent = SessionInfo {
            front: Some(Front::Agent {
                program: "claude".into(),
                name: "Claude Code".into(),
            }),
            ..session("claude")
        };
        app.set_sessions(vec![agent]);
        press(&mut app, KeyCode::Char(' '));
        assert!(
            app.reply()
                .unwrap()
                .label
                .starts_with("its agent's next prompt")
        );
        assert_eq!(app.on_paste("line one\nline two".into()), None);
        assert_eq!(app.reply().unwrap().text.text(), "line one\nline two");
    }

    fn labels(app: &App) -> Vec<&'static str> {
        app.menu()
            .unwrap()
            .items
            .iter()
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn a_right_click_on_a_session_selects_it_and_its_menu_does_what_its_keys_do() {
        let mut app = app_with(&["a", "b", "c"]);
        app.right_click(Hit::SidebarRow(row_of(&app, "b")), (3, 4));
        assert_eq!(selected_name(&app), Some("b"));
        let labels = labels(&app);
        assert_eq!(labels.first(), Some(&"type into it"));
        assert_eq!(labels.last(), Some(&"kill it"));
        // Outside git, there's no worktree to run or diff.
        assert!(!labels.contains(&"what changed"));
        // Its key chooses an item straight away: here, asking to kill it.
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert!(app.menu().is_none());
        assert_eq!(app.confirm(), Some(&Confirm::Kill("b".into())));
    }

    #[test]
    fn a_background_task_opens_in_a_terminal_from_its_menu_or_with_shift_c() {
        let mut app = App::new(None);
        let task = SessionInfo {
            front: Some(Front::Task),
            ..doing("fixer", Activity::Done)
        };
        app.set_sessions(vec![task, session("shell")]);
        app.right_click(Hit::SidebarRow(row_of(&app, "fixer")), (3, 4));
        assert!(labels(&app).contains(&"open it in a terminal"));
        assert_eq!(
            press(&mut app, KeyCode::Char('C')),
            Some(Action::TaskToTerminal("fixer".into()))
        );
        // A session in a terminal is in one already.
        app.right_click(Hit::SidebarRow(row_of(&app, "shell")), (3, 4));
        assert!(!labels(&app).contains(&"open it in a terminal"));
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Char('C')), None);
        assert_eq!(app.notice(), Some("shell isn't a background task"));
    }

    #[test]
    fn a_click_chooses_a_menu_item_and_one_outside_closes_it() {
        let mut app = app_with(&["a", "b"]);
        app.set_screen(Rect::new(0, 0, 100, 30));
        app.right_click(Hit::SidebarRow(row_of(&app, "a")), (5, 5));
        // The frame's top is row 5, so the first item is on row 6.
        let archive = labels(&app)
            .iter()
            .position(|l| *l == "archive it")
            .unwrap();
        app.menu_mouse(MouseEventKind::Moved, 8, 6 + archive as u16);
        assert_eq!(app.menu().unwrap().highlighted, archive);
        assert_eq!(app.menu_mouse(CLICK, 8, 6 + archive as u16), None);
        assert_eq!(app.confirm(), Some(&Confirm::Archive("a".into())));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Archive("a".into()))
        );

        app.right_click(Hit::SidebarRow(row_of(&app, "b")), (5, 5));
        assert_eq!(app.menu_mouse(CLICK, 90, 25), None);
        assert!(app.menu().is_none());
        assert_eq!(app.confirm(), None);
    }

    #[test]
    fn a_right_click_on_a_heading_offers_what_its_project_or_worktree_does() {
        let mut app = app_with_an_empty_worktree();
        app.right_click(Hit::SidebarRow(0), (1, 1));
        assert!(matches!(app.rows()[0], Row::Project { .. }));
        assert_eq!(selected_name(&app), Some("planner"));
        assert!(labels(&app).contains(&"new worktree"));
        press(&mut app, KeyCode::Esc);
        assert!(app.menu().is_none());

        let empty = app
            .rows()
            .iter()
            .position(|row| matches!(row, Row::NoSessions(_)))
            .unwrap();
        // The worktree's heading, above its row, selects it.
        app.right_click(Hit::SidebarRow(empty - 1), (1, 1));
        assert_eq!(empty_branch(&app), Some("old"));
        assert_eq!(labels(&app).last(), Some(&"remove the worktree"));
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(
            app.launcher().is_some(),
            "Enter on it starts a session there"
        );
    }

    #[test]
    fn a_right_click_on_a_tab_goes_to_it_and_offers_the_tabs_keys() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('t'));
        app.right_click(Hit::Tab(0), (1, 0));
        assert_eq!(app.tabs().current_index(), 0);
        assert_eq!(labels(&app).last(), Some(&"close it"));
        press(&mut app, KeyCode::Char('T'));
        assert!(app.prompt().is_some(), "it asks for the tab's name");
    }

    #[test]
    fn nothing_opens_a_menu_while_a_question_waits() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('x'));
        app.right_click(Hit::SidebarRow(row_of(&app, "a")), (1, 1));
        assert!(app.menu().is_none());
    }

    #[test]
    fn capital_a_asks_before_archiving_and_capital_z_lists_the_archive() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(
            app.confirm().map(Confirm::question).as_deref(),
            Some("archive a? It stops; Z starts it again where it was. y/n")
        );
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        assert_eq!(
            press(&mut app, KeyCode::Char('Z')),
            Some(Action::ListArchived)
        );
        app.show_archived(Ok(Vec::new()));
        assert!(app.archived_view().is_some());
        // The archive has the keys while it's open.
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert!(app.archived_view().is_none());
    }

    #[test]
    fn bang_and_dot_ask_for_the_projects_commands_in_the_selections_worktree() {
        let mut app = app_with_an_empty_worktree();
        let main = in_worktree("planner", "main", State::Running)
            .worktree
            .unwrap();
        assert_eq!(
            press(&mut app, KeyCode::Char('!')),
            Some(Action::ProjectCommand {
                which: Verb::Run,
                worktree: main,
            })
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            press(&mut app, KeyCode::Char('.')),
            Some(Action::ProjectCommand {
                which: Verb::Open,
                worktree: linked("old"),
            })
        );
        let mut outside = app_with(&["a"]);
        assert_eq!(press(&mut outside, KeyCode::Char('!')), None);
        assert!(outside.notice().unwrap().contains("git worktree"));
    }

    /// The row of the heading of the project at `project`.
    fn heading_of(app: &App, project: &str) -> usize {
        let project = Path::new(project);
        app.rows()
            .iter()
            .position(|row| matches!(row, Row::Project { path, .. } if path == project))
            .unwrap()
    }

    #[test]
    fn h_folds_the_selected_project_to_its_heading_and_l_unfolds_it() {
        let app_path = Path::new("/code/app");
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_project("planner", "app"),
            in_project("fixer", "app"),
            in_project("docs", "web"),
        ]);
        let unfolded = app.rows();
        press(&mut app, KeyCode::Char('h'));
        assert!(app.is_folded(app_path));
        // The project is its heading alone, the next project's right under
        // it; the selection stays, out of sight, its heading with the bar.
        let rows = app.rows();
        assert!(matches!(&rows[1], Row::Project { name, .. } if name == "web"));
        assert_eq!(rows.len(), unfolded.len() - 3);
        assert_eq!(selected_name(&app), Some("planner"));
        assert_eq!(app.folded_selection(), Some(app_path));
        // The folded project is one row to move over, and back onto.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected_name(&app), Some("docs"));
        assert_eq!(app.folded_selection(), None);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.folded_selection(), Some(app_path));
        // Enter on its heading unfolds it, as `l` does.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.rows(), unfolded);
        press(&mut app, KeyCode::Char('h'));
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.rows(), unfolded);
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn a_click_on_a_project_s_heading_folds_it_and_a_session_picked_unfolds_it() {
        let web = Path::new("/code/web");
        let mut app = App::new(None);
        app.set_sessions(vec![
            in_project("planner", "app"),
            in_project("docs", "web"),
        ]);
        app.on_mouse(CLICK, Hit::SidebarRow(heading_of(&app, "/code/web")));
        assert!(app.is_folded(web));
        assert_eq!(selected_name(&app), Some("planner"));
        assert_eq!(app.folded_projects().len(), 1);
        // `/` finds sessions wherever they are, folded or not.
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "docs");
        let docs = app
            .sessions()
            .iter()
            .position(|s| s.name == "docs")
            .unwrap();
        assert!(app.rows().contains(&Row::Session(docs)));
        press(&mut app, KeyCode::Esc);
        assert!(!app.rows().contains(&Row::Session(docs)));
        // A session picked by name is shown: its project unfolds.
        app.select("docs");
        assert!(!app.is_folded(web));
        // Clicked again, a folded heading unfolds.
        let mut app = App::new(None);
        app.set_sessions(vec![in_project("planner", "app")]);
        app.set_folded_projects(BTreeSet::from([PathBuf::from("/code/app")]));
        app.on_mouse(CLICK, Hit::SidebarRow(heading_of(&app, "/code/app")));
        assert!(app.folded_projects().is_empty());
    }

    #[test]
    fn folding_keeps_the_selection_on_an_empty_worktree_out_of_sight() {
        let mut app = app_with_an_empty_worktree();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(empty_branch(&app), Some("old"));
        press(&mut app, KeyCode::Char('h'));
        assert!(!shows_empty_worktree(&app));
        // git listing the worktrees again leaves the selection where it is.
        app.set_worktrees(PathBuf::from("/code/app"), vec![linked("old")]);
        assert_eq!(empty_branch(&app), Some("old"));
        assert_eq!(app.folded_selection(), Some(Path::new("/code/app")));
    }

    #[test]
    fn a_worktree_is_counted_again_once_a_session_in_it_changes() {
        let fix = PathBuf::from("/code/app.worktrees/fix");
        let mut fixer = in_worktree("fixer", "fix", State::Running);
        let mut app = App::new(None);
        app.set_sessions(vec![fixer.clone()]);
        assert_eq!(app.take_stats_due(), HashSet::from([fix.clone()]));
        app.set_sessions(vec![fixer.clone()]);
        assert!(app.take_stats_due().is_empty());
        fixer.changed = 5;
        app.set_sessions(vec![fixer]);
        assert_eq!(app.take_stats_due(), HashSet::from([fix.clone()]));
        // Shown, it's counted; folded away with its project, it isn't.
        assert_eq!(app.shown_worktrees(), [fix]);
        press(&mut app, KeyCode::Char('h'));
        assert!(app.shown_worktrees().is_empty());
    }
}
