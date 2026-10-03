//! The TUI's state and how keys, the mouse and session lists change it.
//! Nothing here talks to the daemon or draws: when a key needs the outside
//! world, it comes back as an [`Action`] for the event loop to carry out.
//! That keeps every state change testable on its own.

use super::backlog_view::{BacklogChange, BacklogView, Step};
use super::command_line;
use super::diff_view::{self, Against, DiffView};
use super::finder::Finder;
use super::groups::{self, Row};
use super::issues::IssuesView;
use super::launcher::{self, Launcher, Memory, Run, Setup, Target};
use super::memory_view::MemoryView;
use super::plugins_view::{self, PluginsView};
use super::profiles::{self, ProfilesView};
use super::search;
use super::text_input::TextInput;
use crate::catalog::{self, Agent};
use crate::client::Purpose;
use crate::config::Config;
use crate::flow_run::{FlowRun, RunState};
use crate::flows::{self, Flow};
use crate::github::{self, PullRequest};
use crate::keys;
use crate::profile::{self, Profile};
use crate::protocol::{Activity, Backlog, SessionInfo, State, TaskSpec};
use crate::shell;
use crate::{backlog, plugins, tasks};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How many sessions can be split off into panes of their own at once.
pub const MAX_SPLITS: usize = 2;

/// Where a pane sits beside the sidebar: the one that follows the
/// selection, or one of the splits, counted in the order they were made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Selected,
    Split(usize),
}

/// Where the keyboard goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Keys move through the list and act on sessions.
    Sidebar,
    /// Keys go to the session in this pane.
    Pane(Slot),
}

/// What the mouse is over, worked out from the layout by `ui::hit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A row of the sidebar, by its place in [`App::rows`].
    SidebarRow(usize),
    /// The sidebar, but none of its rows: its border, or below the last.
    Sidebar,
    /// The pane at `slot`. `cell` is the `(row, column)` on its session's
    /// screen, counted from 0, when the mouse is inside the pane's border.
    Pane {
        slot: Slot,
        cell: Option<(u16, u16)>,
    },
    /// A row of an open view's list, by its place in the whole list.
    ViewList(usize),
    /// The rest of an open view: the diff, or the file's preview.
    ViewContent,
    /// The footer, or anywhere else.
    Elsewhere,
}

/// Something that takes the place of the sidebar and the panes until it's
/// closed: the diff of a worktree, the file finder, or a project's memory.
pub enum View {
    Diff(DiffView),
    Files(Finder),
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
    /// view's worktree, in the user's editor.
    Edit(String),
}

/// Something read off the event loop: still being read, read, or what went
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loading<T> {
    Reading,
    Read(T),
    Failed(String),
}

/// Which way Tab goes round the panes: Tab forward, Shift+Tab back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Forward,
    Back,
}

/// Where a new session starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// In this directory, or the TUI's own when it's `None`.
    Directory(Option<PathBuf>),
    /// In a new worktree on `branch`, made in the repository at `base`, or
    /// the TUI's own directory's when that's `None`.
    NewWorktree {
        branch: String,
        base: Option<PathBuf>,
    },
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
    /// Start this ended session's command again.
    Respawn(String),
    /// Remove the linked worktree at `path`, which is on `branch`.
    RemoveWorktree {
        path: PathBuf,
        branch: String,
    },
}

impl Confirm {
    /// The question, the way the footer asks it.
    pub fn question(&self) -> String {
        match self {
            Confirm::Kill(name) => format!("kill {name}? y/n"),
            Confirm::Respawn(name) => format!("start {name} again? y/n"),
            Confirm::RemoveWorktree { branch, .. } => format!("remove worktree {branch}? y/n"),
        }
    }

    /// What a yes asks for.
    fn action(self) -> Action {
        match self {
            Confirm::Kill(name) => Action::Kill(name),
            Confirm::Respawn(name) => Action::Respawn(name),
            Confirm::RemoveWorktree { path, .. } => Action::RemoveWorktree(path),
        }
    }
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
    /// a terminal, for backlog item `backlog` if it's for one.
    StartInBackground {
        place: Place,
        spec: TaskSpec,
        backlog: Option<u64>,
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
    Rename {
        name: String,
        new_name: String,
    },
    /// Start this ended session's command again.
    Respawn(String),
    /// Remove the linked worktree at this path.
    RemoveWorktree(PathBuf),
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
    /// Open pull request `number` of the project at `project` in the
    /// browser.
    OpenPullRequest {
        project: PathBuf,
        number: u64,
    },
    /// Ask GitHub for the open issues of the project at this path, for the
    /// issues view that's now open.
    ListIssues(PathBuf),
    /// Read the diff of the worktree at `dir`, off the event loop.
    ReadDiff {
        dir: PathBuf,
        against: Against,
    },
    /// List the files of the worktree at this directory, off the event
    /// loop.
    ReadFiles(PathBuf),
    /// Read the first lines of the file at `path`, from the top of the
    /// worktree at `dir`, off the event loop.
    ReadPreview {
        dir: PathBuf,
        path: String,
    },
    /// Open the file at `path`, from the top of the worktree at `dir`, in
    /// the user's editor, as a new session called `name`.
    Edit {
        dir: PathBuf,
        path: String,
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
        profile: Profile,
    },
    /// Take the profile with this name out of the config file.
    DeleteProfile(String),
    /// Read which plugins there are, and open the plugins view on them.
    ListPlugins,
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
    /// Type into the plugin's pane that's open.
    TypeInPluginPane(KeyEvent),
    PasteInPluginPane(String),
    /// Close the plugin's pane that's open, and end its session.
    ClosePluginPane,
}

/// A sidebar key one of the installed plugins' actions took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginKey {
    pub key: char,
    pub plugin: String,
    pub action: String,
    pub title: String,
}

/// A plugin's pane, open over the panes: a session of its own, which ends
/// when the pane closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPane {
    pub plugin: String,
    pub title: String,
    /// The session's name.
    pub session: String,
}

/// The sidebar narrowed to the sessions that match what's typed, while `/`
/// is open. The selection stays where it was until Enter moves it to the
/// session the bar is on.
#[derive(Debug, Default)]
pub struct Filter {
    pub input: TextInput,
    /// The session the bar is on, by its id: the sessions are put in order
    /// again with every fresh list, so an index wouldn't keep to it.
    highlighted: Option<String>,
}

pub struct App {
    /// In the sidebar's order: see [`groups`].
    sessions: Vec<SessionInfo>,
    /// An index into `sessions`, kept in range while there are any.
    selected: usize,
    /// The question on the footer line, while one is being answered.
    prompt: Option<Prompt>,
    /// The new-session panel, while it's open.
    launcher: Option<Launcher>,
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
    /// Sessions split off into panes of their own, by name, in the order
    /// they were split off. A split stays on its session while the
    /// selection moves.
    splits: Vec<String>,
    focus: Focus,
    /// The pane the keyboard was in last, so that Tab in the sidebar goes
    /// on to the next one.
    last_pane: Option<Slot>,
    /// The id of the session this TUI runs in, if it runs in one. The pane
    /// never shows it: it would be showing itself.
    own_id: Option<String>,
    /// Something to tell the user, like why a key didn't work. It stays
    /// until the next key.
    notice: Option<String>,
    /// Whether the overlay listing every key is open.
    showing_keys: bool,
    /// `/`'s filter on the sidebar, while it's open.
    filter: Option<Filter>,
    /// What GitHub said about each project's open pull requests, by the
    /// project's main worktree: the pull requests, or why there are none to
    /// show.
    pull_requests: HashMap<PathBuf, Result<Vec<PullRequest>, String>>,
    /// The issues view, while it's open.
    issues: Option<IssuesView>,
    /// The diff, the file finder or a project's memory, while one is open.
    view: Option<View>,
    /// Whether memory is on, which is whether `m` opens it: see
    /// [`crate::memory::enabled`].
    memory_on: bool,
    /// Whether tasks are on: closing them, and showing how they stand.
    tasks_on: bool,
    /// Whether the backlog is on: its view, and its counts in the sidebar.
    backlog_on: bool,
    /// Whether the github plugin is on: pull requests on worktree lines,
    /// `o` and `i`.
    github_on: bool,
    /// The plugins view, while it's open.
    plugins_view: Option<PluginsView>,
    /// The sidebar keys the installed plugins that are on took.
    plugin_keys: Vec<PluginKey>,
    /// A plugin's pane, while one is open.
    plugin_pane: Option<PluginPane>,
    /// The session whose task `c` is closing, while the footer asks whether
    /// it was done or failed.
    closing: Option<String>,
    /// The backlog view, while it's open.
    backlog: Option<BacklogView>,
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
}

impl App {
    /// A TUI with no sessions yet. `own_id` is the id of the session it
    /// runs in, if it runs in one.
    pub fn new(own_id: Option<String>) -> App {
        App {
            sessions: Vec::new(),
            selected: 0,
            prompt: None,
            launcher: None,
            agents: Vec::new(),
            profiles: Vec::new(),
            profiles_on: profile::enabled(&Config::default()),
            profiles_view: None,
            new_session_profile: None,
            first_run: None,
            memory: Memory::default(),
            codex_models: None,
            confirm: None,
            splits: Vec::new(),
            focus: Focus::Sidebar,
            last_pane: None,
            own_id,
            notice: None,
            showing_keys: false,
            filter: None,
            pull_requests: HashMap::new(),
            issues: None,
            view: None,
            memory_on: true,
            tasks_on: true,
            backlog_on: true,
            github_on: true,
            plugins_view: None,
            plugin_keys: Vec::new(),
            plugin_pane: None,
            closing: None,
            backlog: None,
            backlog_counts: HashMap::new(),
            flows: Vec::new(),
            flow_defs: Vec::new(),
            flows_on: true,
        }
    }

    /// Takes which of crystal's plugins the config has on.
    pub fn set_features(&mut self, config: &Config) {
        self.tasks_on = tasks::enabled(config);
        self.backlog_on = backlog::enabled(config);
        self.memory_on = crate::memory::enabled(config);
        self.profiles_on = profile::enabled(config);
        self.github_on = github::enabled(config);
        self.flows_on = flows::enabled(config);
    }

    /// Whether the TUI asks GitHub about the sessions' projects.
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
            .map(|key| {
                (
                    key.key.to_string(),
                    format!("{}: {}", key.plugin, key.title),
                )
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
        self.showing_keys
    }

    /// The diff or the file finder, while one is open.
    pub fn view(&self) -> Option<&View> {
        self.view.as_ref()
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

    /// Takes the files listed for the file finder, if it's still open on
    /// their worktree. Its first file's preview is to be read next.
    pub fn files_read(&mut self, dir: &Path, files: Result<Vec<String>, String>) -> Option<Action> {
        let Some(View::Files(finder)) = &mut self.view else {
            return None;
        };
        finder.files_read(dir, files)
    }

    /// Takes a file's first lines, if the file finder still has it selected.
    pub fn preview_read(&mut self, dir: &Path, path: &str, lines: Result<Vec<String>, String>) {
        if let Some(View::Files(finder)) = &mut self.view {
            finder.preview_read(dir, path, lines);
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
            Some(View::Files(finder)) => finder.set_size(list),
            Some(View::Memory(memory)) => memory.set_size(list),
            None => {}
        }
    }

    pub fn notify(&mut self, notice: String) {
        self.notice = Some(notice);
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
    /// only those that match while `/`'s filter is open.
    pub fn rows(&self) -> Vec<Row> {
        let shown = self.matches();
        let mut rows = groups::rows(&self.sessions, self.shown_flows(), |index| {
            shown.contains(&index)
        });
        if !self.tasks_on {
            rows.retain(|row| !matches!(row, Row::Task(_)));
        }
        rows
    }

    /// `/`'s filter, while it's open.
    pub fn filter(&self) -> Option<&Filter> {
        self.filter.as_ref()
    }

    /// The sessions shown in the sidebar, by index: those that match the
    /// filter while it's open, or else all of them.
    pub fn matches(&self) -> Vec<usize> {
        let query = self
            .filter
            .as_ref()
            .map_or("", |filter| filter.input.text());
        (0..self.sessions.len())
            .filter(|&index| search::session_match(query, &self.sessions[index]).is_some())
            .collect()
    }

    /// Which letters of a session's name to mark, while the filter is open:
    /// those the query matched.
    pub fn marked_letters(&self, index: usize) -> Vec<usize> {
        let Some(filter) = &self.filter else {
            return Vec::new();
        };
        let session = &self.sessions[index];
        search::session_match(filter.input.text(), session).unwrap_or_default()
    }

    /// The session the sidebar's bar is on: the one the filter's bar is on
    /// while it's open, or else the selected one.
    pub fn sidebar_cursor(&self) -> Option<usize> {
        match &self.filter {
            Some(filter) => {
                let id = filter.highlighted.as_ref()?;
                self.sessions.iter().position(|session| session.id == *id)
            }
            None => self.selected_index(),
        }
    }

    /// The projects the sessions are in, by their main worktrees: the ones
    /// to ask GitHub about.
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

    /// Takes what GitHub said about the open pull requests of the project
    /// at `project`.
    pub fn set_pull_requests(&mut self, project: PathBuf, found: Result<Vec<PullRequest>, String>) {
        self.pull_requests.insert(project, found);
    }

    /// The open pull request for `branch` in the project at `project`, if
    /// GitHub knows of one.
    pub fn pull_request(&self, project: &Path, branch: &str) -> Option<&PullRequest> {
        if !self.github_on {
            return None;
        }
        let Some(Ok(pull_requests)) = self.pull_requests.get(project) else {
            return None;
        };
        pull_requests
            .iter()
            .find(|pull_request| pull_request.head_ref_name == branch)
    }

    /// The issues view, while it's open.
    pub fn issues_view(&self) -> Option<&IssuesView> {
        self.issues.as_ref()
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

    /// Takes the open issues GitHub listed for the project at `project`.
    pub fn set_issues(&mut self, project: &Path, found: Result<Vec<github::Issue>, String>) {
        if let Some(view) = self.issues.as_mut().filter(|view| view.project == project) {
            view.set_issues(found);
        }
    }

    /// Takes the text of issue `number` of the project at `project`.
    pub fn set_issue_body(&mut self, project: &Path, number: u64, body: Result<String, String>) {
        if let Some(view) = self.issues.as_mut().filter(|view| view.project == project) {
            view.set_body(number, body);
        }
    }

    /// The issue whose text to fetch next, with its project: the one the
    /// issues view's bar is on, once.
    pub fn issue_body_to_fetch(&mut self) -> Option<(PathBuf, u64)> {
        let view = self.issues.as_mut()?;
        let number = view.body_to_fetch()?;
        Some((view.project.clone(), number))
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

    /// The names of the sessions split off, in the order they were.
    pub fn splits(&self) -> &[String] {
        &self.splits
    }

    /// The panes beside the sidebar, in the order they're drawn: the one
    /// that follows the selection, then each split.
    pub fn slots(&self) -> Vec<Slot> {
        let splits = (0..self.splits.len()).map(Slot::Split);
        std::iter::once(Slot::Selected).chain(splits).collect()
    }

    /// The session the pane at `slot` is about: the selected one, or the
    /// one split off there.
    pub fn pane_session(&self, slot: Slot) -> Option<&SessionInfo> {
        match slot {
            Slot::Selected => self.selected(),
            Slot::Split(index) => {
                let name = self.splits.get(index)?;
                self.sessions.iter().find(|session| session.name == *name)
            }
        }
    }

    /// Whether the pane at `slot` shows its session's screen. The pane that
    /// follows the selection doesn't when the selected session is split
    /// off, so that no session is drawn twice at two sizes, nor when it's
    /// the session this TUI runs in.
    pub fn shows_screen(&self, slot: Slot) -> bool {
        let Some(session) = self.pane_session(slot) else {
            return false;
        };
        match slot {
            Slot::Selected => !self.selected_is_own() && !self.is_split(&session.name),
            Slot::Split(_) => true,
        }
    }

    /// Whether the session called `name` has a pane of its own.
    pub fn is_split(&self, name: &str) -> bool {
        self.splits.iter().any(|split| split == name)
    }

    /// The index of the selected session, or `None` when there are none.
    pub fn selected_index(&self) -> Option<usize> {
        if self.sessions.is_empty() {
            None
        } else {
            Some(self.selected)
        }
    }

    pub fn selected(&self) -> Option<&SessionInfo> {
        self.sessions.get(self.selected)
    }

    /// Whether the selected session is the one this TUI runs in. Ids tell,
    /// since the session may have been renamed since the TUI started.
    pub fn selected_is_own(&self) -> bool {
        match (&self.own_id, self.selected()) {
            (Some(own), Some(selected)) => selected.id == *own,
            _ => false,
        }
    }

    /// Takes a fresh list from the daemon and puts it in the sidebar's
    /// order. The selected session stays selected wherever it moved to; if
    /// it's gone, the selection stays at the same place in the list, or the
    /// end of it.
    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>) {
        let selected_name = self.selected().map(|session| session.name.clone());
        self.sessions = groups::order(sessions, self.shown_flows());
        let still_there = selected_name.and_then(|name| self.position(&name));
        if let Some(index) = still_there {
            self.selected = index;
        }
        self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
        // A split whose session has gone closes. Going from the end keeps
        // the splits still to check where they were.
        for index in (0..self.splits.len()).rev() {
            if self.position(&self.splits[index]).is_none() {
                self.close_split(index);
            }
        }
        if let Focus::Pane(slot) = self.focus
            && !self.can_type_into(slot)
        {
            self.focus = Focus::Sidebar;
        }
    }

    /// Selects the session called `name`, if there is one.
    pub fn select(&mut self, name: &str) {
        if let Some(index) = self.position(name) {
            self.selected = index;
        }
    }

    /// The session called `from` is called `to` now: a split of it stays
    /// open under its new name.
    pub fn renamed(&mut self, from: &str, to: &str) {
        for split in &mut self.splits {
            if split == from {
                *split = to.to_string();
            }
        }
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

    /// The pane that shows the selected session: its split, if it has one,
    /// or else the pane that follows the selection.
    fn selected_slot(&self) -> Option<Slot> {
        let selected = self.selected()?;
        let slot = match self.splits.iter().position(|split| *split == selected.name) {
            Some(index) => Slot::Split(index),
            None => Slot::Selected,
        };
        Some(slot)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.notice = None;
        // A plugin's pane is over everything, and has every key but the one
        // that closes it.
        if self.plugin_pane.is_some() {
            if keys::is_hand_back(&key) {
                return Some(Action::ClosePluginPane);
            }
            return Some(Action::TypeInPluginPane(key));
        }
        // An open view has every key until it's closed.
        if self.view.is_some() {
            return self.on_view_key(key);
        }
        // Any key closes the list of keys, and does nothing else: the key
        // that closes it may be one the user was only reading about.
        if self.showing_keys {
            self.showing_keys = false;
            return None;
        }
        // Only `y` says yes; any other key says no.
        if let Some(confirm) = self.confirm.take() {
            if key.code == KeyCode::Char('y') {
                return Some(confirm.action());
            }
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
        if self.backlog.is_some() {
            return self.on_backlog_key(key);
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
        if self.prompt.is_some() {
            return self.on_prompt_key(key);
        }
        if self.issues.is_some() {
            return self.on_issues_key(key);
        }
        if self.filter.is_some() {
            self.on_filter_key(key);
            return None;
        }
        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(key),
            Focus::Pane(slot) => self.on_pane_key(slot, key),
        }
    }

    /// What the mouse does, when no program in a pane has taken it: a click
    /// selects a session or hands a pane the keyboard, and the wheel moves
    /// the selection, or scrolls a pane through its history.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Option<Action> {
        if let Some(view) = &mut self.view {
            let outcome = match view {
                View::Diff(diff) => diff.on_mouse(kind, hit),
                View::Files(finder) => finder.on_mouse(kind, hit),
                View::Memory(memory) => memory.on_mouse(kind, hit),
            };
            return self.follow(outcome);
        }
        // A click closes the list of keys, like a key does.
        if self.showing_keys {
            if kind == MouseEventKind::Down(MouseButton::Left) {
                self.showing_keys = false;
            }
            return None;
        }
        // A question on the footer waits for its answer from the keyboard,
        // and so do the filter, the issues and backlog views, the new-session
        // panel and the profiles view.
        let typing = self.filter.is_some()
            || self.issues.is_some()
            || self.backlog.is_some()
            || self.launcher.is_some()
            || self.profiles_view.is_some()
            || self.plugins_view.is_some()
            || self.plugin_pane.is_some();
        let asking = self.prompt.is_some() || self.confirm.is_some() || self.closing.is_some();
        if asking || typing {
            return None;
        }
        let click = kind == MouseEventKind::Down(MouseButton::Left);
        if click {
            self.notice = None;
        }
        match (kind, hit) {
            (_, Hit::SidebarRow(row)) if click => self.click_row(row),
            (_, Hit::Pane { slot, .. }) if click => {
                if self.can_type_into(slot) {
                    self.focus_pane(slot);
                }
            }
            (MouseEventKind::ScrollUp, Hit::SidebarRow(_) | Hit::Sidebar) => {
                self.move_selection(-1);
            }
            (MouseEventKind::ScrollDown, Hit::SidebarRow(_) | Hit::Sidebar) => {
                self.move_selection(1);
            }
            (MouseEventKind::ScrollUp, Hit::Pane { slot, .. }) => {
                return Some(Action::ScrollBack(slot));
            }
            (MouseEventKind::ScrollDown, Hit::Pane { slot, .. }) => {
                return Some(Action::ScrollForward(slot));
            }
            _ => {}
        }
        None
    }

    /// A click on a sidebar row: on a session, selects it and gives the
    /// sidebar the keyboard. Headings don't do anything.
    fn click_row(&mut self, row: usize) {
        if let Some(Row::Session(index) | Row::Task(index)) = self.rows().get(row) {
            self.selected = *index;
            self.focus = Focus::Sidebar;
        }
    }

    fn on_sidebar_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Ctrl or Alt with a letter isn't that letter. Ctrl+J is a line feed,
        // which a terminal sends as it closes, and it mustn't move the
        // selection: showing a session counts as having seen it.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Enter => self.enter(),
            KeyCode::Tab => self.move_to_pane(Direction::Forward),
            KeyCode::BackTab => self.move_to_pane(Direction::Back),
            KeyCode::Char('s') => self.toggle_split(),
            KeyCode::PageUp => return Some(Action::PageBack(self.selected_slot()?)),
            KeyCode::PageDown => return Some(Action::PageForward(self.selected_slot()?)),
            KeyCode::Char('n') => return self.open_launcher(false),
            KeyCode::Char('w') => return self.open_launcher(true),
            KeyCode::Char('W') => self.ask_to_remove_worktree(),
            KeyCode::Char('r') => self.ask_for_name(),
            KeyCode::Char('x') => self.confirm = Some(Confirm::Kill(self.selected()?.name.clone())),
            KeyCode::Char('u') => self.select_next_needing_user(),
            KeyCode::Char('d') => return self.open_diff(),
            KeyCode::Char('p') => return self.open_finder(),
            KeyCode::Char('m') => return self.open_memory(),
            KeyCode::Char('P') => return self.open_profiles(),
            KeyCode::Char('?') => self.showing_keys = true,
            KeyCode::Char('/') => self.open_filter(),
            KeyCode::Char('o') => return self.open_pull_request(),
            KeyCode::Char('i') => return self.open_issues(),
            KeyCode::Char('c') if self.tasks_on => self.ask_how_the_task_went(),
            KeyCode::Char('b') if self.backlog_on => return self.open_backlog(),
            KeyCode::Char('g') if self.flows_on => return self.go_on_with_flow(),
            KeyCode::Char('f') if self.flows_on => self.ask_to_send_flow_back(),
            KeyCode::Char('c') => self.notify(plugins::off("tasks")),
            KeyCode::Char('b') => self.notify(plugins::off("backlog")),
            KeyCode::Char('g' | 'f') => self.notify(plugins::off("flows")),
            KeyCode::Char('X') => return Some(Action::ListPlugins),
            KeyCode::Char('q') => return Some(Action::Quit),
            KeyCode::Char(c) => return self.run_plugin_key(c),
            _ => {}
        }
        None
    }

    /// Runs the plugin action that took `key`, about the selected session,
    /// if one did.
    fn run_plugin_key(&mut self, key: char) -> Option<Action> {
        let taken = self.plugin_keys.iter().find(|taken| taken.key == key)?;
        let (plugin, action) = (taken.plugin.clone(), taken.action.clone());
        Some(Action::RunPlugin {
            plugin,
            action,
            context: self.selected_context(),
        })
    }

    /// What a plugin's action or pane is told about where it was run from:
    /// the selected session. With none, the event loop says where.
    fn selected_context(&self) -> plugins::Context {
        self.selected()
            .map(plugins::Context::of_session)
            .unwrap_or_default()
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
        let diff = DiffView::new(dir, place);
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

    /// The selected session's worktree, and its project and branch the way
    /// a view's header names them: `payments ⎇ fix/login`. When it isn't in
    /// one, the footer says so.
    fn selected_worktree(&mut self) -> Option<(PathBuf, String)> {
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
    fn on_view_key(&mut self, key: KeyEvent) -> Option<Action> {
        let outcome = match self.view.as_mut()? {
            View::Diff(diff) => diff.on_key(key),
            View::Files(finder) => finder.on_key(key),
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
            Outcome::Do(action) => Some(action),
            Outcome::Edit(path) => {
                let Some(View::Files(finder)) = self.view.take() else {
                    return None;
                };
                let name = self.free_name(&edit_name(&path));
                Some(Action::Edit {
                    dir: finder.dir,
                    path,
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

    /// Asks before removing the selected session's worktree. Only a linked
    /// worktree goes, and only once nothing runs in it any more: removing
    /// it would pull the directory out from under them.
    fn ask_to_remove_worktree(&mut self) {
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
            self.confirm = Some(Confirm::RemoveWorktree {
                path: worktree.path,
                branch,
            });
        } else {
            let notice = format!("{} still running in {branch}", running.join(", "));
            self.notify(notice);
        }
    }

    /// Opens `/`'s filter, its bar on the selected session.
    fn open_filter(&mut self) {
        let highlighted = self.selected().map(|session| session.id.clone());
        self.filter = Some(Filter {
            input: TextInput::default(),
            highlighted,
        });
    }

    /// Keys while `/`'s filter is open: Enter selects the session the bar
    /// is on, Esc leaves the selection where it was, ↑ and ↓ (or Ctrl+P
    /// and Ctrl+N) move the bar among the matches, and every other key
    /// edits the filter. Letters type, so j and k don't move the bar here.
    fn on_filter_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.filter = None,
            KeyCode::Enter => {
                if let Some(index) = self.sidebar_cursor() {
                    self.selected = index;
                }
                self.filter = None;
            }
            KeyCode::Up => self.move_filter_bar(-1),
            KeyCode::Down => self.move_filter_bar(1),
            KeyCode::Char('p') if ctrl => self.move_filter_bar(-1),
            KeyCode::Char('n') if ctrl => self.move_filter_bar(1),
            _ => {
                if let Some(filter) = &mut self.filter {
                    filter.input.on_key(&key);
                }
                self.keep_filter_bar_on_a_match();
            }
        }
    }

    /// Moves the filter's bar `by` matches, stopping at the ends.
    fn move_filter_bar(&mut self, by: isize) {
        let matches = self.matches();
        let at = self
            .sidebar_cursor()
            .and_then(|cursor| matches.iter().position(|&index| index == cursor));
        let Some(at) = at else {
            return;
        };
        let to = matches[at.saturating_add_signed(by).min(matches.len() - 1)];
        let id = self.sessions[to].id.clone();
        if let Some(filter) = &mut self.filter {
            filter.highlighted = Some(id);
        }
    }

    /// Puts the filter's bar on the first match when the session it was on
    /// doesn't match any more.
    fn keep_filter_bar_on_a_match(&mut self) {
        let matches = self.matches();
        let on_a_match = self
            .sidebar_cursor()
            .is_some_and(|cursor| matches.contains(&cursor));
        if !on_a_match {
            let first = matches
                .first()
                .map(|&index| self.sessions[index].id.clone());
            if let Some(filter) = &mut self.filter {
                filter.highlighted = first;
            }
        }
    }

    /// `o`: opens the pull request of the selected session's branch, or
    /// says why there's none to open.
    fn open_pull_request(&mut self) -> Option<Action> {
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
        let Some(branch) = worktree.branch else {
            self.notify(format!("{name} is on no branch"));
            return None;
        };
        let project = worktree.project_path;
        let number = match self.pull_requests.get(&project) {
            None => {
                self.notify(format!("still asking GitHub about {}", worktree.project));
                return None;
            }
            Some(Err(reason)) => {
                let reason = reason.clone();
                self.notify(reason);
                return None;
            }
            Some(Ok(_)) => self.pull_request(&project, &branch).map(|pr| pr.number),
        };
        match number {
            Some(number) => Some(Action::OpenPullRequest { project, number }),
            None => {
                self.notify(format!("no open pull request for {branch}"));
                None
            }
        }
    }

    /// `i`: opens the issues view for the selected session's project, or
    /// says why it can't.
    fn open_issues(&mut self) -> Option<Action> {
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
        // What stopped GitHub listing pull requests would stop it listing
        // issues too.
        if let Some(Err(reason)) = self.pull_requests.get(&worktree.project_path) {
            let reason = reason.clone();
            self.notify(reason);
            return None;
        }
        let project = worktree.project_path;
        self.issues = Some(IssuesView::new(project.clone(), worktree.project));
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
            RunState::Done => {
                self.notify(format!("{} is done", run.name));
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
                let branch = github::branch_for_issue(item.number, &item.text);
                let setup = self.launch_setup(false);
                let launcher = Launcher::new(setup)
                    .with_task(&item.text, &branch)
                    .for_backlog_item(item.number);
                self.launcher = Some(launcher);
                self.codex_models_wanted()
            }
        }
    }

    /// Keys while the issues view is open: Esc closes it, Enter goes on to
    /// start a session for the issue the bar is on, and the view takes the
    /// rest.
    fn on_issues_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => self.issues = None,
            KeyCode::Enter => return self.start_on_issue(),
            _ => {
                if let Some(view) = &mut self.issues {
                    view.on_key(&key);
                }
            }
        }
        None
    }

    /// Closes the issues view and opens the new-session panel for the issue
    /// the bar was on: a new worktree on a branch named after it, and the
    /// task to fix it, with the issue's address so the agent can read it.
    fn start_on_issue(&mut self) -> Option<Action> {
        let view = self.issues.take()?;
        let Some(issue) = view.highlighted() else {
            self.issues = Some(view);
            return None;
        };
        let branch = github::branch_for_issue(issue.number, &issue.title);
        let task = format!(
            "Fix issue #{}: {} ({})",
            issue.number, issue.title, issue.url
        );
        let mut setup = self.launch_setup(true);
        if let Some(Target::NewWorktree { base, .. }) = setup.targets.get_mut(1) {
            *base = Some(view.project.clone());
        }
        self.launcher = Some(Launcher::new(setup).with_task(&task, &branch));
        self.codex_models_wanted()
    }

    /// Opens the new-session panel, set to start in a new worktree when
    /// `worktree` is set, or else where the selected session runs.
    fn open_launcher(&mut self, worktree: bool) -> Option<Action> {
        let setup = self.launch_setup(worktree);
        self.launcher = Some(Launcher::new(setup));
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
        // A flow's steps are Claude Code's background tasks.
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
        }
    }

    /// Where a new session can start: where the selected session is, a new
    /// worktree of its project, or another project's main worktree.
    fn launch_targets(&self) -> Vec<Target> {
        let selected = self.selected();
        let worktree = selected.and_then(|session| session.worktree.as_ref());
        let here = match (selected, worktree) {
            (Some(_), Some(worktree)) => Target::Here {
                dir: Some(worktree.path.clone()),
                label: worktree_label(worktree),
            },
            (Some(session), None) => Target::Here {
                dir: Some(session.cwd.clone()),
                label: shell::home_relative(&session.cwd),
            },
            (None, _) => Target::Here {
                dir: None,
                label: "this directory".to_string(),
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
        for worktree in self.sessions.iter().filter_map(|s| s.worktree.as_ref()) {
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
                self.launcher = None;
                None
            }
            launcher::Outcome::Start {
                place,
                command,
                task,
                run,
                background,
                backlog,
            } => {
                self.launcher = None;
                self.memory.remember(&task, &run);
                if background {
                    let spec = launcher::background_spec(&command)?;
                    return Some(Action::StartInBackground {
                        place,
                        spec,
                        backlog,
                    });
                }
                // Given something to do, the session is a task.
                let purpose = Purpose {
                    task: (!task.is_empty()).then_some(task),
                    backlog,
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
                self.launcher = None;
                self.memory.remember(&goal, &run);
                Some(Action::StartFlow { place, flow, goal })
            }
            launcher::Outcome::CommandLine { place, line } => {
                self.launcher = None;
                self.ask(Question::Command(place), &line);
                None
            }
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
        if let Some(View::Memory(memory)) = &mut self.view {
            let outcome = memory.on_paste(&text);
            return self.follow(outcome);
        }
        if self.view.is_some() || self.showing_keys || self.confirm.is_some() {
            return None;
        }
        if let Some(launcher) = &mut self.launcher {
            launcher.on_paste(&text);
        } else if let Some(view) = &mut self.profiles_view {
            view.on_paste(&text);
        } else if let Some(prompt) = &mut self.prompt {
            prompt.input.insert_str(&text);
        } else if let Some(issues) = &mut self.issues {
            issues.on_paste(&text);
        } else if let Some(backlog) = &mut self.backlog {
            backlog.on_paste(&text);
        } else if let Some(filter) = &mut self.filter {
            filter.input.insert_str(&text);
            self.keep_filter_bar_on_a_match();
        } else if let Focus::Pane(slot) = self.focus {
            return Some(Action::Paste { to: slot, text });
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
                        backlog: None,
                    };
                    Some(Action::Start {
                        place,
                        command,
                        purpose,
                    })
                }
                Err(err) => {
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
        let worktree = self.selected()?.worktree.as_ref()?;
        Some(worktree.project_path.clone())
    }

    /// Every key goes to the pane's session, Tab too, since shells and
    /// agents need it. Kept back are Ctrl+\, which returns to the sidebar,
    /// and Shift+PageUp and Shift+PageDown, how terminals have always
    /// scrolled back: they page through the pane's history. Unshifted, the
    /// page keys go to the session like any other.
    fn on_pane_key(&mut self, slot: Slot, key: KeyEvent) -> Option<Action> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            _ if keys::is_hand_back(&key) => {
                self.focus = Focus::Sidebar;
                None
            }
            KeyCode::PageUp if shift => Some(Action::PageBack(slot)),
            KeyCode::PageDown if shift => Some(Action::PageForward(slot)),
            _ => Some(Action::Type { to: slot, key }),
        }
    }

    /// Splits the selected session off into a pane of its own, or closes
    /// its split if it has one.
    fn toggle_split(&mut self) {
        let Some(selected) = self.selected() else {
            return;
        };
        let name = selected.name.clone();
        if let Some(index) = self.splits.iter().position(|split| *split == name) {
            self.close_split(index);
        } else if self.selected_is_own() {
            self.notify("crystal can't show the session it runs in".into());
        } else if self.splits.len() >= MAX_SPLITS {
            let notice = format!("{MAX_SPLITS} splits at most: press s on one to close it");
            self.notify(notice);
        } else {
            self.splits.push(name);
        }
    }

    /// Closes the split at `index`. The splits after it move up a place,
    /// and the keyboard moves with its pane, or goes back to the sidebar
    /// if its pane is the one that closed.
    fn close_split(&mut self, index: usize) {
        self.splits.remove(index);
        self.focus = match self.focus {
            Focus::Pane(Slot::Split(at)) if at == index => Focus::Sidebar,
            Focus::Pane(Slot::Split(at)) if at > index => Focus::Pane(Slot::Split(at - 1)),
            focus => focus,
        };
    }

    /// Moves the keyboard from the sidebar to the next pane that takes
    /// keys, going on from the one it was in last.
    fn move_to_pane(&mut self, direction: Direction) {
        if let Some(slot) = self.next_pane(direction) {
            self.focus_pane(slot);
        }
    }

    /// The next pane that takes keys, going round the panes in the order
    /// they're drawn, from just past the one used last. With none used
    /// yet, going forward starts at the first pane, and going back at the
    /// last.
    fn next_pane(&self, direction: Direction) -> Option<Slot> {
        let slots = self.slots();
        let count = slots.len();
        let last = self
            .last_pane
            .and_then(|last| slots.iter().position(|slot| *slot == last));
        (1..=count)
            .map(|step| match (direction, last) {
                (Direction::Forward, Some(last)) => (last + step) % count,
                (Direction::Back, Some(last)) => (last + count - step) % count,
                (Direction::Forward, None) => step - 1,
                (Direction::Back, None) => count - step,
            })
            .map(|index| slots[index])
            .find(|slot| self.can_type_into(*slot))
    }

    fn focus_pane(&mut self, slot: Slot) {
        self.focus = Focus::Pane(slot);
        self.last_pane = Some(slot);
    }

    fn move_selection(&mut self, by: isize) {
        let last = self.sessions.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    /// Selects the next session that needs the user, or says there's none.
    fn select_next_needing_user(&mut self) {
        match self.next_needing_user() {
            Some(index) => self.selected = index,
            None => self.notify("nothing needs you".to_string()),
        }
    }

    /// The next session that needs the user, going down the sidebar from
    /// the selected one and round again: one waiting on them comes before
    /// one that's done. Starting after the selection means that pressing
    /// the key again moves on to the next.
    fn next_needing_user(&self) -> Option<usize> {
        let count = self.sessions.len();
        let in_turn: Vec<usize> = (1..=count)
            .map(|step| (self.selected + step) % count)
            .collect();
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

    /// Only a pane that shows a running session takes keys.
    fn can_type_into(&self, slot: Slot) -> bool {
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
    use crate::protocol::{Activity, BacklogItem, TaskInfo, TaskOutcome, Worktree};
    use crossterm::event::KeyModifiers;

    fn session(name: &str) -> SessionInfo {
        SessionInfo {
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
            backlog: None,
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
            start(place, &["claude", "fix the login bug"], "fix the login bug")
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
                &["codex", "--full-auto", "go"],
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
                profile: reviewer.clone()
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
    fn w_opens_the_panel_on_a_new_worktree_named_after_the_task() {
        let mut app = with_agents(&["claude"], vec![in_project("agent", "app")]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "fix typo");
        let place = Place::NewWorktree {
            branch: "fix-typo".into(),
            base: Some(PathBuf::from("/code/app")),
        };
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(place, &["claude", "fix typo"], "fix typo")
        );
    }

    #[test]
    fn outside_a_repository_the_worktree_is_made_from_the_tuis_directory() {
        let mut app = with_agents(&["claude"], vec![session("shell")]);
        press(&mut app, KeyCode::Char('w'));
        type_text(&mut app, "feat");
        let place = Place::NewWorktree {
            branch: "feat".into(),
            base: None,
        };
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(place, &["claude", "feat"], "feat")
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
    fn ctrl_e_hands_the_command_to_the_command_line() {
        let mut app = with_agents(&["claude"], vec![]);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "fix it");
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(app.launcher().is_none());
        assert_eq!(prompt_text(&app), Some("claude 'fix it'"));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            start(Place::Directory(None), &["claude", "fix it"], "fix it")
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
            }),
            ..session(name)
        }
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
        };
        assert_eq!(app.confirm(), Some(&removal));
        assert_eq!(
            app.confirm().unwrap().question(),
            "remove worktree fix? y/n"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::RemoveWorktree("/code/app.worktrees/fix".into()))
        );
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

    fn hand_back(app: &mut App) {
        app.on_key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL));
    }

    /// An app with `names`, the first `split` of them split off, and the
    /// selection on the last one.
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
        assert_eq!(app.slots(), [Slot::Selected, Slot::Split(0)]);

        press(&mut app, KeyCode::Char('s'));
        assert!(app.splits().is_empty());
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
    fn two_splits_at_most_and_a_third_says_so() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.splits(), ["a", "b"]);
        assert!(app.notice().unwrap().contains("2 splits at most"));
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
                pane(Slot::Selected),
                pane(Slot::Split(0)),
                pane(Slot::Split(1)),
                pane(Slot::Selected),
            ]
        );
    }

    #[test]
    fn shift_tab_goes_round_the_other_way_starting_at_the_last_pane() {
        let mut app = app_with_splits(&["a", "b", "c"], 2);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(1)));
        hand_back(&mut app);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));
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
    fn the_keyboard_leaves_a_split_whose_session_ends() {
        let mut app = app_with_splits(&["a", "b"], 1);
        app.select("a");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus(), Focus::Pane(Slot::Split(0)));

        app.set_sessions(vec![ended("a"), session("b")]);
        assert_eq!(app.splits(), ["a"], "it stays, to show how it ended");
        assert_eq!(app.focus(), Focus::Sidebar);
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
            }),
            ..session(name)
        }
    }

    fn pull_request(number: u64, branch: &str) -> PullRequest {
        serde_json::from_value(serde_json::json!({
            "number": number,
            "title": "a change",
            "headRefName": branch,
            "isDraft": false,
            "reviewDecision": "",
            "statusCheckRollup": [],
            "url": format!("https://github.com/acme/app/pull/{number}"),
        }))
        .unwrap()
    }

    #[test]
    fn o_opens_the_pull_request_of_the_selected_sessions_branch() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("fixer", "fix-login")]);
        app.set_pull_requests(
            PathBuf::from("/code/app"),
            Ok(vec![pull_request(57, "fix-login")]),
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('o')),
            Some(Action::OpenPullRequest {
                project: PathBuf::from("/code/app"),
                number: 57
            })
        );
    }

    #[test]
    fn o_says_why_there_is_no_pull_request_to_open() {
        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("fixer", "fix-login")]);
        assert_eq!(press(&mut app, KeyCode::Char('o')), None);
        assert_eq!(app.notice(), Some("still asking GitHub about app"));

        app.set_pull_requests(
            PathBuf::from("/code/app"),
            Ok(vec![pull_request(9, "other")]),
        );
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.notice(), Some("no open pull request for fix-login"));

        let not_github = "app's origin isn't on GitHub".to_string();
        app.set_pull_requests(PathBuf::from("/code/app"), Err(not_github.clone()));
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.notice(), Some(not_github.as_str()));
    }

    fn issue(number: u64, title: &str) -> github::Issue {
        serde_json::from_value(serde_json::json!({
            "number": number,
            "title": title,
            "labels": [],
            "updatedAt": "2026-10-02T09:30:00Z",
            "author": {"login": "ana"},
            "url": format!("https://github.com/acme/app/issues/{number}"),
        }))
        .unwrap()
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
            Ok(vec![issue(42, "Fix login redirect")]),
        );
        press(&mut app, KeyCode::Enter);
        assert!(app.issues_view().is_none());
        let panel = app.launcher().unwrap();
        assert_eq!(panel.title(), "New session · app ⎇ 42-fix-login-redirect");
        assert_eq!(
            panel.task().text(),
            "Fix issue #42: Fix login redirect (https://github.com/acme/app/issues/42)"
        );
        let Some(Action::Start { place, command, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("Enter should start the session");
        };
        assert_eq!(
            place,
            Place::NewWorktree {
                branch: "42-fix-login-redirect".into(),
                base: Some(PathBuf::from("/code/app")),
            }
        );
        assert_eq!(command[0], "claude");
        assert!(command[1].starts_with("Fix issue #42"));
    }

    #[test]
    fn i_outside_a_repository_or_github_says_why() {
        let mut app = app_with(&["shell"]);
        assert_eq!(press(&mut app, KeyCode::Char('i')), None);
        assert_eq!(app.notice(), Some("shell isn't in a git repository"));

        let mut app = App::new(None);
        app.set_sessions(vec![in_repo("planner", "main")]);
        let reason = "gh: not logged in".to_string();
        app.set_pull_requests(PathBuf::from("/code/app"), Err(reason.clone()));
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
        SessionInfo {
            task: Some(TaskInfo {
                goal: goal.into(),
                background: false,
                backlog: None,
                outcome: outcome.map(|(failed, summary)| TaskOutcome {
                    failed,
                    summary: summary.into(),
                    closed: 1,
                }),
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
                    tags: Vec::new(),
                    done: false,
                    created: 0,
                    closed: None,
                })
                .collect(),
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
}
