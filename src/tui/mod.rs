//! The TUI, run as `crystal` with no command: a sidebar with every session,
//! the selected one live in a pane beside it, and any others split off into
//! panes of their own, split and sized as the user likes, in tabs that each
//! keep their own.
//!
//! Everything that happens arrives as an [`Event`] on one channel: a key,
//! the mouse, a resize, output from the session in the pane, a fresh
//! session list. The loop takes each event, updates the state, and draws.

mod app;
pub(crate) mod appearance;
mod archived_view;
mod away;
mod backlog_view;
mod command_line;
mod command_list;
mod compose;
mod copy_mode;
mod diff;
mod diff_tree;
mod diff_view;
mod editing;
mod finder;
mod fuzzy;
mod grep;
mod groups;
mod handoff_view;
mod help;
mod issues;
pub(crate) mod keymap;
pub(crate) mod launcher;
mod layout_link;
pub(crate) mod layouts;
mod listing;
mod memory_view;
mod menu;
pub(crate) mod mouse;
mod needs_you;
pub(crate) mod page;
mod pane;
mod plugins_view;
mod preview;
mod profiles;
mod pull_requests;
mod ram_view;
mod reply;
mod restarted;
mod review;
pub(crate) mod screen_widget;
mod scrollbar;
mod search;
mod settings_view;
pub(crate) mod sidebar;
pub(crate) mod split_tree;
mod status;
pub(crate) mod status_bar;
mod switcher;
mod tabs;
mod text_area;
mod text_input;
pub(crate) mod theme;
mod timeline;
mod tree_browser;
mod ui;
pub(crate) mod window;

use crate::bell::Ringer;
use crate::config::{self, Config};
use crate::db::{self, Db};
use crate::events::{Filter, Scope, Since};
use crate::flow_run::FlowRun;
use crate::forge::{
    Checkout, Forge, Issue, IssueDetail, PullRequest, PullRequestDetail, Repo, Topic,
};
use crate::layout::{self, Layout, Order, Relayed};
use crate::memory::{self, Listed, Memory};
use crate::plugin_manifest::Placement;
use crate::plugins::{self, Context, Id};
use crate::profile;
use crate::project_cli;
use crate::project_commands::{self, Commands, Verb};
use crate::protocol::{
    Backlog, NewSession, Request, Response, SessionInfo, Spending, State, Worktree,
};
use crate::{catalog, keys, links, names, project, shell, socket, typing, update};
use crate::{client, clipboard, drive, env, event_log, events, git, handoff};
use anyhow::{Context as _, Result, bail, ensure};
use app::{Action, App, Focus, Hit, Place, PluginKey, PluginPane, Popup, Slot};
use appearance::Appearance;
use backlog_view::BacklogChange;
use crossterm::event::{
    Event as TerminalEvent, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use diff_view::Against;
use keymap::{CommandKind, KeyCommand, Sequence, SplitWay};
use layouts::{Layouts, Which};
use page::Page;
use pane::Pane;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use settings_view::Setting;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use theme::Theme;

/// How often the session list is asked for. The daemon doesn't announce
/// changes, so this is how far behind the list can be.
const POLL_EVERY: Duration = Duration::from_millis(500);

/// How many polls go by between asking which projects crystal knows.
const PROJECTS_EVERY: u64 = 6;

/// How often the working mark turns a quarter, while an agent works. With
/// nothing working, the TUI waits for something to happen instead.
const SPIN_EVERY: Duration = Duration::from_millis(150);

/// How often its forge is asked again about a project's pull requests. A
/// project seen for the first time is asked about straight away.
const PULL_REQUESTS_EVERY: Duration = Duration::from_secs(60);

/// How often its forge is asked again about a project's issues, for the
/// tab bar's count: they change less, and the issues view asks for itself.
const ISSUES_EVERY: Duration = Duration::from_secs(300);

/// The least time between two fetches of a worktree's remotes the branch
/// switcher starts on its own, so opening and closing it never hammers a
/// remote. Ctrl+R fetches whenever it's asked.
const FETCH_EVERY: Duration = Duration::from_secs(60);

/// How long find in files waits after a key before it searches: the time
/// between keys of someone typing a word, so a search runs once it's typed.
const GREP_PAUSE: Duration = Duration::from_millis(150);

/// How often git is asked again about a project's worktrees, for those
/// made or removed outside crystal. A project seen for the first time is
/// asked about straight away, and so is every one when crystal has just
/// made or removed a worktree.
const WORKTREES_EVERY: Duration = Duration::from_secs(5);

/// How often git counts what each worktree the sidebar shows has changed,
/// when nothing says it has: a worktree is counted again straight away
/// once a session in it changes.
const STATS_EVERY: Duration = Duration::from_secs(10);

/// How often the daemon is asked what crystal's processes take, for the
/// footer's readout; and how often while the RAM view is open.
const RESOURCES_EVERY: Duration = Duration::from_secs(5);
const RESOURCES_OPEN_EVERY: Duration = Duration::from_secs(1);

/// How often the thread following the event log for the timeline looks up
/// from waiting, to see whether the timeline is still open.
const FOLLOW_CHECK: Duration = Duration::from_millis(250);

pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Text pasted into the terminal, whole.
    Paste(String),
    /// A layout command from the command line, passed on by the daemon.
    Layout(Relayed),
    /// The models Codex lets the user choose.
    CodexModels(Vec<String>),
    /// The terminal changed size. The next draw lays everything out again
    /// and resizes the pane's session to fit.
    Resize,
    /// The sessions, as the daemon listed them when asked at `asked`.
    Sessions {
        sessions: Vec<SessionInfo>,
        asked: Instant,
    },
    /// Every flow run, as the daemon listed them.
    Flows(Vec<FlowRun>),
    /// The projects crystal knows, by their main worktrees.
    Projects(Vec<Worktree>),
    /// Output from the session in the pane with this id.
    Output {
        pane: u64,
        bytes: Vec<u8>,
    },
    /// The session in the pane with this id has ended.
    OutputEnded {
        pane: u64,
    },
    /// What its forge said about the open pull requests of a project, and
    /// when it was asked.
    PullRequests {
        project: PathBuf,
        found: Result<(Forge, Vec<PullRequest>), String>,
        asked: Instant,
    },
    /// What git counted of the worktree at `path`, or `None` when it
    /// couldn't say.
    Stat {
        path: PathBuf,
        stat: Option<git::Stat>,
    },
    /// The linked worktrees of a project, as git listed them, and for
    /// those Claude Code made for itself, the subject of the commit each
    /// is at.
    Worktrees {
        project: PathBuf,
        worktrees: Vec<Worktree>,
        subjects: HashMap<PathBuf, String>,
        labels: HashMap<PathBuf, String>,
    },
    /// The worktrees the daemon is removing, whoever asked, as it listed
    /// them.
    Removals(Vec<PathBuf>),
    /// The daemon is done removing the worktree at `path`: it's gone, or
    /// why not.
    WorktreeRemoved {
        path: PathBuf,
        removed: Result<(), String>,
    },
    /// The worktree at `path`, on `branch`, has changes not committed, so
    /// git didn't remove it: only a forced removal would.
    WorktreeHasChanges {
        path: PathBuf,
        branch: String,
    },
    /// What its forge said about the open issues of a project, and when it
    /// was asked.
    Issues {
        project: PathBuf,
        found: Result<(Forge, Vec<Issue>), String>,
        asked: Instant,
    },
    /// One of a project's issues, read whole, and when it was asked for.
    IssueRead {
        project: PathBuf,
        number: u64,
        read: Result<IssueDetail, String>,
        asked: Instant,
    },
    /// One of a project's pull requests, read whole, and when it was asked
    /// for.
    PullRequestRead {
        project: PathBuf,
        number: u64,
        read: Result<PullRequestDetail, String>,
        asked: Instant,
    },
    /// A reply was sent to the session called `name`, or why it wasn't.
    Replied {
        name: String,
        sent: Result<(), String>,
    },
    /// A comment was posted on `topic`, or why it wasn't.
    Commented {
        project: PathBuf,
        topic: Topic,
        posted: Result<(), String>,
    },
    /// An issue was given a new title and text, or why it wasn't, and when
    /// the forge answered: what it's asked from then on has them.
    IssueEdited {
        project: PathBuf,
        number: u64,
        edit: (String, String),
        saved: Result<(), String>,
        at: Instant,
    },
    /// A pull request's worktree is there now: the start that waited on it
    /// can go on, in it.
    Fetched(Box<Action>),
    /// What the daemon found crystal's processes take.
    Resources(crate::resources::Resources),
    /// Something to tell the user, from work done off the loop.
    Notice(String),
    /// What's new in this crystal: its release's notes, asked for as its
    /// TUI first opened after an update.
    WhatsNew(String),
    /// A worktree's diff, read for the diff view.
    DiffRead {
        dir: PathBuf,
        against: Against,
        read: Result<diff_view::Read, String>,
    },
    /// A worktree's files, listed for the file finder or the tree browser.
    FilesRead {
        dir: PathBuf,
        files: Result<Vec<String>, String>,
    },
    /// A file read and highlighted for a preview.
    PreviewRead {
        dir: PathBuf,
        path: String,
        read: Result<preview::Content, String>,
    },
    /// A worktree's branches and changes, listed for the branch switcher.
    BranchesListed {
        dir: PathBuf,
        listed: Result<switcher::Listed, String>,
    },
    /// The remotes of the worktree at `dir` have been fetched, or why they
    /// couldn't.
    BranchesFetched {
        dir: PathBuf,
        fetched: Result<(), String>,
    },
    /// What switching the worktree at `dir` to another branch came to.
    BranchSwitched {
        dir: PathBuf,
        outcome: git::branches::Outcome,
    },
    /// What find in files' search for `query` found.
    Searched {
        dir: PathBuf,
        query: String,
        found: Result<git::Found, String>,
    },
    /// A file's lines, highlighted, for find in files' preview.
    MatchedFileRead {
        dir: PathBuf,
        path: String,
        lines: Result<Vec<crate::syntax::Runs>, String>,
    },
    /// A project's memory, read for the memory view.
    MemoryRead {
        dir: PathBuf,
        read: Result<Vec<Listed>, String>,
    },
    /// The backlog of the project `dir` is in, for the backlog view.
    Backlog {
        dir: PathBuf,
        found: Result<Backlog, String>,
    },
    /// How many backlog items each project has to do.
    BacklogCounts(HashMap<PathBuf, usize>),
    /// What background tasks have spent today.
    Spending(Spending),
    /// The settings as they are now, for the settings view: boxed, as the
    /// config is the biggest thing an event carries.
    Settings(Box<settings_view::Current>),
    /// The config file has changed: what it says now, or why it can't be
    /// read.
    ConfigFile(Box<Result<Config, String>>),
    /// The terminal has focus again, or has lost it.
    Focus(bool),
    /// A page of the event log, the newest first, read for the timeline
    /// following the log under the number `feed`.
    EventsRead {
        feed: u64,
        read: Result<Vec<events::Event>, String>,
    },
    /// An event that has just happened, for the timeline following the log
    /// under the number `feed`.
    Logged {
        feed: u64,
        event: Box<events::Event>,
    },
    /// What the event log gained while the user was away.
    Away(Result<away::Tally, String>),
    /// What the handoff view on `session` shows.
    HandoffFound {
        session: String,
        found: handoff_view::Found,
    },
    /// The system's appearance, light or dark, has changed.
    Appearance(Appearance),
    /// What each thing at the tab bar's right shows now.
    Status(Vec<String>),
}

/// Carries out a layout command with no TUI open, on the tabs as the TUIs
/// last kept them, `kept`: see [`App::obey_alone`].
pub(crate) fn obey_alone(
    sessions: Vec<SessionInfo>,
    flows: Vec<FlowRun>,
    kept: Option<&str>,
    order: Order,
) -> Result<app::Alone, String> {
    App::obey_alone(sessions, flows, tabs::read(kept), order)
}

pub fn run(socket: &Path) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("crystal needs a terminal; see crystal --help for the commands");
    }
    let config = Config::load()?;
    crate::vt::set_history_lines(config.scrollback_lines);
    crate::mermaid::set_ascii(config.mermaid_ascii);
    // Asking for the list starts the daemon if it isn't running.
    let sessions = list_sessions(socket, true)?;

    // The terminal is asked about its background, where it's asked, before
    // anything else reads what it sends.
    let appearance = config
        .appearance
        .auto_switch
        .then(appearance::at_start)
        .flatten();
    let (sender, events) = mpsc::channel();
    spawn_input_reader(sender.clone());
    let follow_appearance = Arc::new(AtomicBool::new(config.appearance.auto_switch));
    appearance::follow(sender.clone(), follow_appearance.clone(), appearance);
    let status = status_bar::watch(
        &config.tab_bar.right,
        std::env::current_dir().unwrap_or_default(),
        socket,
        sender.clone(),
    );
    let layout = layout_link::Link::open(socket, sender.clone());
    watch_config(sender.clone());
    let count_backlog = Arc::new(AtomicBool::new(crate::backlog::enabled(&config)));
    let poll_flows = Arc::new(AtomicBool::new(crate::flows::enabled(&config)));
    let poll_settings = Arc::new(AtomicBool::new(false));
    spawn_session_poller(
        socket.to_path_buf(),
        sender.clone(),
        Polled {
            backlog_counts: count_backlog.clone(),
            flows: poll_flows.clone(),
            settings: poll_settings.clone(),
        },
    );
    let projects = Arc::new(Mutex::new(Vec::new()));
    spawn_forge_poller(projects.clone(), sender.clone());
    let worktree_projects = Arc::new(Mutex::new(Vec::new()));
    let list_worktrees_now = Arc::new(AtomicBool::new(false));
    spawn_worktree_lister(
        worktree_projects.clone(),
        list_worktrees_now.clone(),
        sender.clone(),
    );
    let stat_worktrees = Arc::new(Mutex::new(Vec::new()));
    let stats_now = Arc::new(Mutex::new(HashSet::new()));
    spawn_stat_counter(stat_worktrees.clone(), stats_now.clone(), sender.clone());
    let ram_open = Arc::new(AtomicBool::new(false));
    spawn_resource_poller(socket.to_path_buf(), sender.clone(), ram_open.clone());

    // Losing the tabs is no reason not to start: without the database, the
    // TUI starts with one tab, and says why when asked for a layout.
    let db = Db::open(socket).map_err(|err| format!("{err:#}"));
    let mut tui = Tui {
        socket: socket.to_path_buf(),
        db,
        app: App::new(env::own_session_id(socket)),
        panes: Vec::new(),
        last_pane_id: 0,
        events: sender,
        screen: Rect::default(),
        projects,
        worktree_projects,
        list_worktrees_now,
        stat_worktrees,
        stats_now,
        theme: Theme::from_config(&config, appearance),
        appearance,
        follow_appearance,
        status,
        title: None,
        hostname: window::hostname(),
        started: Instant::now(),
        sessions_asked: Instant::now(),
        searches: Arc::new(AtomicU64::new(0)),
        link_clicked: false,
        clicks: pane::Clicks::default(),
        edge: None,
        kept_tabs: tabs::Tabs::default(),
        kept_sidebar: app::Shape::default(),
        kept_folded: BTreeSet::new(),
        quitting: false,
        overlay: None,
        count_backlog,
        poll_flows,
        poll_settings,
        ram_open,
        config: config.clone(),
        feed: Arc::new(AtomicU64::new(0)),
        presence: away::Presence::new(events::now_ms()),
        layout,
        ringer: Ringer::default(),
        fetched_remotes: HashMap::new(),
        fetching_remotes: HashSet::new(),
    };
    tui.app.set_agents(catalog::installed());
    let server = socket::server_of(socket).filter(|server| server != socket::DEFAULT);
    tui.app.set_server(server);
    tui.app.set_launch_settings(&config);
    tui.app.set_features(&config);
    tui.app.set_interface(&config);
    tui.app.set_start_dir(start_dir(&config));
    tui.app.set_plugin_keys(plugin_keys(&config));
    let sidebar = tui
        .ui(db::SIDEBAR)
        .and_then(|json| serde_json::from_str(&json).ok());
    tui.app.set_sidebar(sidebar, config.sidebar.folded);
    tui.kept_sidebar = tui.app.sidebar_shape();
    let folded = tui
        .ui(db::FOLDED)
        .and_then(|json| serde_json::from_str(&json).ok());
    tui.app.set_folded_projects(folded.unwrap_or_default());
    tui.kept_folded = tui.app.folded_projects().clone();
    tui.app
        .set_memory(launcher::read_memory(tui.ui(db::LAUNCHER).as_deref()));
    tui.app.set_diff_tree(tui.review().tree);
    if tui.app.shows_flows() {
        tui.app.set_flows(list_flows(socket));
    }
    tui.set_sessions(sessions);
    tui.app.set_tabs(tabs::read(tui.ui(db::TABS).as_deref()));
    tui.kept_tabs = tui.app.tabs_to_keep();
    tui.look_back_from_last_seen();
    if config.update.check {
        tui.look_for_update();
    }
    tui.show_whats_new(config.update.check);

    let mut terminal = ratatui::try_init()?;
    let result = tui.run_with_modes(&mut terminal, events);
    ratatui::restore();
    result
}

/// The terminal sending the TUI what the mouse does, pastes marked as
/// pastes, and keys it can tell apart, for as long as this lives. However
/// the TUI ends, by returning, failing or panicking, all of it goes back to
/// how the terminal had it: left on, a shell would fill with the sequences
/// the terminal sends for them.
struct TerminalModes;

impl TerminalModes {
    /// With `mouse` off, the terminal keeps the mouse: `[mouse] capture`.
    fn on(mouse: bool) -> Result<TerminalModes> {
        // A panic on any thread turns them off before the panic is shown.
        let shown_before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            modes_off();
            shown_before(info);
        }));
        // The mouse, unless the config leaves it to the terminal: see
        // `MOUSE_ON`. A move that changes nothing isn't drawn. Then
        // bracketed paste (2004): a paste comes whole, its lines kept, not
        // as typed keys. Then, pushed on the terminal's
        // stack, the Kitty keyboard protocol's flags to tell apart keys the
        // old way can't, like Esc or Shift+Enter, and to say which key a
        // shifted one is (1 and 4): a program in a pane that asked for the
        // protocol gets them. A terminal without it ignores the request.
        // Then focus (1004): the terminal says when it gains and loses it,
        // for "while you were away", and so that layout commands from the
        // command line go to the TUI the user is at. Last, the terminal's
        // title is saved on its stack, for the TUI's own to go over.
        let mut out = std::io::stdout();
        if mouse {
            out.write_all(MOUSE_ON)?;
        }
        out.write_all(b"\x1b[?2004h\x1b[>5u\x1b[?1004h")?;
        out.write_all(window::SAVE)?;
        out.flush()?;
        Ok(TerminalModes)
    }
}

impl Drop for TerminalModes {
    fn drop(&mut self) {
        modes_off();
    }
}

fn modes_off() {
    let mut out = std::io::stdout();
    let _ = out.write_all(window::RESTORE);
    let _ = out.write_all(b"\x1b[?1004l\x1b[<u\x1b[?2004l");
    let _ = out.write_all(MOUSE_OFF);
    let _ = out.flush();
}

/// Clicks and the wheel (1000), drags (1002), the mouse just moving
/// (1003), to underline the link under it while Ctrl is held, all written
/// the SGR way (1006).
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

/// Has the terminal send the TUI the mouse, or keep it for its own
/// selection: `[mouse] capture`.
fn capture_mouse(on: bool) {
    let mut out = std::io::stdout();
    let _ = out.write_all(if on { MOUSE_ON } else { MOUSE_OFF });
    let _ = out.flush();
}

/// A drag selecting in the pane at `slot`, held `past` rows beyond the top
/// of its screen (less than 0) or its bottom: its history scrolls again
/// at `next`.
#[derive(Debug, Clone, Copy)]
struct Edge {
    slot: Slot,
    past: i32,
    next: Instant,
}

struct Tui {
    socket: PathBuf,
    /// Where the tabs, the layouts and the new-session panel's memory are
    /// kept, or why it couldn't be opened.
    db: Result<Db, String>,
    app: App,
    /// A viewer of each session a pane shows: the selected one and the
    /// split ones. No session is shown twice, so its id finds its pane.
    panes: Vec<Pane>,
    last_pane_id: u64,
    /// Handed to each pane, for its output.
    events: Sender<Event>,
    /// The whole screen as it was last drawn, to find what the mouse is on.
    screen: Rect,
    /// The viewer of the session of a plugin's pane, while one is open.
    overlay: Option<Pane>,
    /// Whether the session poller asks how many backlog items each project
    /// has, which follows the backlog plugin being switched.
    count_backlog: Arc<AtomicBool>,
    /// Whether the session poller asks for the flow runs too: the flows
    /// plugin is on.
    poll_flows: Arc<AtomicBool>,
    /// Whether the session poller reads the settings too: the settings
    /// view is open.
    poll_settings: Arc<AtomicBool>,
    /// Whether the RAM view is open, which has the daemon asked what the
    /// sessions take more often.
    ram_open: Arc<AtomicBool>,
    /// The config as the TUI last took it in.
    config: Config,
    /// Where layout commands from the command line come from, and their
    /// answers go.
    layout: layout_link::Link,
    /// The projects the sessions are in, for the thread that asks GitHub
    /// about their pull requests.
    projects: Arc<Mutex<Vec<PathBuf>>>,
    /// The projects the sessions are in, for the thread that asks git
    /// about their worktrees: all of them, whatever the plugins.
    worktree_projects: Arc<Mutex<Vec<PathBuf>>>,
    /// Set when crystal has just made or removed a worktree, for that
    /// thread to ask git again straight away.
    list_worktrees_now: Arc<AtomicBool>,
    /// The worktrees the sidebar shows, for the thread that counts what
    /// each has changed.
    stat_worktrees: Arc<Mutex<Vec<PathBuf>>>,
    /// The worktrees for that thread to count again straight away.
    stats_now: Arc<Mutex<HashSet<PathBuf>>>,
    theme: Theme,
    /// The system's appearance, light or dark, as it was last told, which
    /// the theme follows when the settings say: see [`appearance`].
    appearance: Option<Appearance>,
    /// Whether the thread that asks the system about its appearance asks:
    /// `[appearance] auto_switch` is on.
    follow_appearance: Arc<AtomicBool>,
    /// The threads working out what the tab bar shows at its right, while
    /// it shows anything.
    status: Option<status_bar::Watch>,
    /// The title last given the terminal, `None` while the TUI has given it
    /// none.
    title: Option<String>,
    /// This machine's name, for the title.
    hostname: String,
    /// When the TUI started: the working mark turns with the time since.
    started: Instant,
    /// When the daemon was asked for the sessions the TUI shows: a list
    /// asked for earlier, which a slow poll can bring in late, is dropped.
    sessions_asked: Instant,
    /// The tabs as they were last kept.
    kept_tabs: tabs::Tabs,
    /// The sidebar's shape as it was last written down.
    kept_sidebar: app::Shape,
    /// The projects folded in the sidebar, as they were last written down.
    kept_folded: BTreeSet<PathBuf>,
    /// How many searches find in files has asked for: a search that isn't
    /// the last one asked for stops.
    searches: Arc<AtomicU64>,
    /// A Ctrl+click opened a link: the button coming up is that click's,
    /// not the program's under it.
    link_clicked: bool,
    /// The clicks on panes' screens, counted for double- and triple-clicks.
    clicks: pane::Clicks,
    /// A drag selecting in a pane, held past the top or bottom of its
    /// screen, which scrolls its history on until it comes back or lets go.
    edge: Option<Edge>,
    quitting: bool,
    /// The number the timeline follows the event log under, which goes up
    /// each time it starts or stops following: a thread following under an
    /// older number stops, and what was read under one is dropped.
    feed: Arc<AtomicU64>,
    /// Whether the user is there, for "while you were away".
    presence: away::Presence,
    /// Passes on to the user's terminal the bells of the sessions in panes,
    /// and of those out of sight the daemon marked as having rung.
    ringer: Ringer,
    /// When the branch switcher last fetched each worktree's remotes, and
    /// the worktrees whose fetch is still going: one at a time in each.
    fetched_remotes: HashMap<PathBuf, Instant>,
    fetching_remotes: HashSet<PathBuf>,
}

impl Tui {
    fn run_with_modes(
        &mut self,
        terminal: &mut DefaultTerminal,
        events: Receiver<Event>,
    ) -> Result<()> {
        // The mouse and pastes are the TUI's for as long as `_modes` lives:
        // to the end of this function, however it ends.
        let _modes = TerminalModes::on(self.config.mouse.capture)?;
        self.run(terminal, events)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal, events: Receiver<Event>) -> Result<()> {
        let mut changed = true;
        while !self.quitting {
            if changed {
                self.draw(terminal)?;
            }

            // Wait for something to happen, then take whatever else has
            // happened meanwhile, so a burst of output is drawn once.
            changed = match self.next_event(&events)? {
                Some(event) => self.take(event),
                None => true,
            };
            while let Ok(event) = events.try_recv() {
                changed |= self.take(event);
            }
            changed |= self.scroll_at_edge();
            self.read_topic();
            self.keep_tabs();
            self.count_worktrees();
        }
        self.keep_seen();
        Ok(())
    }

    /// Lays everything out for the terminal's size, and draws it.
    fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let size = terminal.size()?;
        self.screen = Rect::new(0, 0, size.width, size.height);
        // First, for what's laid out to go by it: one column or not.
        self.app.set_screen(self.screen);
        let areas = ui::Areas::of(&self.app, self.screen);
        self.app.set_tiles(areas.tiles);
        self.sync_panes(&areas);
        if let Some(overlay) = &mut self.overlay {
            let popup = self.app.plugin_pane().and_then(|pane| pane.popup.as_ref());
            let screen = ui::plugin_pane_screen(&areas, popup);
            let size = (screen.height.max(1), screen.width.max(1));
            if overlay.size() != size {
                overlay.resize(size.0, size.1);
            }
        }
        if let Some(view) = self.app.view() {
            let parts = ui::view_areas(view, areas.main);
            let size = |area: Rect| (area.height, area.width);
            self.app
                .set_view_size(size(parts.list), size(parts.content));
        }
        let look = ui::Look {
            theme: &self.theme,
            now: seconds_since_epoch(),
            spin: (self.started.elapsed().as_millis() / SPIN_EVERY.as_millis()) as usize,
        };
        let overlay = self.overlay.as_ref();
        terminal.draw(|frame| ui::draw(frame, &self.app, &self.panes, overlay, &look))?;
        self.give_title();
        Ok(())
    }

    /// Gives the terminal the title `crystal title set` gave, or the one
    /// the settings make, when it doesn't have it already. With neither,
    /// a title the TUI gave it is taken back.
    fn give_title(&mut self) {
        let template = &self.config.window.title;
        let title = match self.app.title_override() {
            Some(text) => text.to_string(),
            None if template.is_empty() => {
                if self.title.take().is_some() {
                    // The terminal's own, and saved again for the end.
                    self.write_out(&[window::RESTORE, window::SAVE].concat());
                }
                return;
            }
            None => window::fill(template, &self.title_values()),
        };
        if self.title.as_ref() != Some(&title) {
            self.write_out(&window::set(&title));
            self.title = Some(title);
        }
    }

    /// What the title's tokens are filled with.
    fn title_values(&self) -> window::Values {
        let selected = self.app.selected();
        let worktree = selected.and_then(|session| session.worktree.as_ref());
        let tab = self.app.tabs().current();
        let number = self.app.tabs().current_index() + 1;
        let title = selected.and_then(|session| {
            let pane = self.panes.iter().find(|pane| pane.session_id == session.id);
            pane.map(|pane| pane.screen.title())
        });
        window::Values {
            hostname: self.hostname.clone(),
            session: selected.map_or_else(String::new, |session| session.name.clone()),
            project: worktree.map_or_else(String::new, |worktree| worktree.project.clone()),
            branch: worktree
                .and_then(|worktree| worktree.branch.clone())
                .unwrap_or_default(),
            tab: match tab.name.is_empty() {
                true => number.to_string(),
                false => tab.name.clone(),
            },
            title: title.unwrap_or_default(),
        }
    }

    /// Writes `bytes` to the terminal, between draws.
    fn write_out(&self, bytes: &[u8]) {
        let mut out = std::io::stdout();
        let _ = out.write_all(bytes).and_then(|()| out.flush());
    }

    /// Takes in `event`, and says whether that may have changed what's
    /// drawn: the mouse just moving mostly doesn't.
    fn take(&mut self, event: Event) -> bool {
        if let Event::Mouse(mouse) = &event
            && mouse.kind == MouseEventKind::Moved
        {
            return self.mouse_moved(mouse);
        }
        self.handle(event);
        true
    }

    /// As the TUI starts, has it say what happened since it was last open,
    /// if it has been before.
    fn look_back_from_last_seen(&mut self) {
        match away::read_seen(self.ui(db::SEEN).as_deref()) {
            Some(seq) => self.look_back(Since::Seq(seq)),
            None => self.keep_seen(),
        }
    }

    /// Has what the event log gained since `since`, when the user went
    /// away, counted off the loop, for the footer to say. From now, they've
    /// seen it all.
    fn look_back(&self, since: Since) {
        self.keep_seen();
        let socket = self.socket.clone();
        self.read_in_background(move || {
            let read = event_log::read(&socket, &Filter::default(), since);
            let tally = read.map(|events| away::Tally::of(&events));
            Event::Away(tally.map_err(|err| format!("{err:#}")))
        });
    }

    /// Once a day, has whether a newer crystal is out looked up off the
    /// loop, and said when it is. The look is kept as it starts, so one that
    /// can't ask, offline say, waits a day too.
    fn look_for_update(&self) {
        let Ok(db) = &self.db else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        if !update::due(self.ui(db::UPDATE).as_deref(), now) {
            return;
        }
        let _ = db.keep_ui(db::UPDATE, &update::Looked { at: now });
        let events = self.events.clone();
        thread::spawn(move || {
            if let Some(notice) = update::newer_notice() {
                let _ = events.send(Event::Notice(notice));
            }
        });
    }

    /// Shows what's new in this crystal, once, as its TUI first opens after
    /// an update: the release's notes an update kept, or else, unless `ask`
    /// says not to, asked of where the releases are, off the loop.
    fn show_whats_new(&mut self, ask: bool) {
        let Ok(db) = &self.db else {
            return;
        };
        let kept = self.ui(db::OPENED);
        let opened_before = self.ui(db::TABS).is_some();
        let _ = db.keep_ui(db::OPENED, &update::Opened::now());
        if !update::has_news(kept.as_deref(), opened_before) {
            return;
        }
        if let Some(body) = update::this_crystals_kept_notes() {
            self.app.show_page(Page::new(&update::notes_title(), &body));
            return;
        }
        if !ask {
            return;
        }
        let events = self.events.clone();
        thread::spawn(move || {
            if let Some(body) = update::this_crystals_notes() {
                let _ = events.send(Event::WhatsNew(body));
            }
        });
    }

    /// Keeps the latest event in the log as the last the user has seen,
    /// where "while you were away" counts from the next time the TUI opens.
    fn keep_seen(&self) {
        let Ok(db) = &self.db else {
            return;
        };
        if let Ok(seq) = db.latest_event() {
            let _ = db.keep_ui(db::SEEN, &away::Seen { seq });
        }
    }

    /// A key, a click or a paste: when it ends a long while without one,
    /// the footer says what happened meanwhile.
    fn user_is_here(&mut self) {
        if let Some(went) = self.presence.input(events::now_ms()) {
            self.look_back(Since::At(went));
        }
    }

    /// Reads the newest page of the event log of `scope` for the timeline,
    /// then follows the log from there, on a thread of its own, until the
    /// timeline closes or is switched to another scope. Subscribing from
    /// the latest event the log had as the page was read leaves no gap
    /// between the two (an event in both is taken once), and the
    /// subscription picks up again after the last event it gave when a
    /// handover cuts it.
    fn follow_events(&self, scope: Scope) {
        let feed = self.feed.fetch_add(1, Ordering::Relaxed) + 1;
        let current = self.feed.clone();
        let socket = self.socket.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            let read = read_events(&socket, &scope, None);
            let since = match &read {
                Ok((latest, _)) => Some(Since::Seq(*latest)),
                Err(_) => None,
            };
            let read = read.map(|(_, page)| page);
            if events.send(Event::EventsRead { feed, read }).is_err() {
                return;
            }
            let mut subscription = match client::subscribe(&socket, Filter::default(), since) {
                Ok(subscription) => subscription,
                Err(err) => {
                    let notice = format!("couldn't follow the event log: {err:#}");
                    let _ = events.send(Event::Notice(notice));
                    return;
                }
            };
            while current.load(Ordering::Relaxed) == feed {
                let event = match subscription.next_before(Some(Instant::now() + FOLLOW_CHECK)) {
                    Ok(Some(event)) => Box::new(event),
                    Ok(None) => continue,
                    Err(err) => {
                        let notice = format!("stopped following the event log: {err:#}");
                        let _ = events.send(Event::Notice(notice));
                        return;
                    }
                };
                if events.send(Event::Logged { feed, event }).is_err() {
                    return;
                }
            }
        });
    }

    /// Whether the timeline still follows the log under the number `feed`.
    fn following(&self, feed: u64) -> bool {
        self.feed.load(Ordering::Relaxed) == feed
    }

    /// Starts again the sessions a layout being restored names that have
    /// gone, those it has `programs` for, each under its name, and says how
    /// many started. One that can't, its directory gone, say, stays gone.
    fn start_again(&self, programs: &layouts::Programs) -> usize {
        let gone = programs
            .iter()
            .filter(|(name, _)| !self.app.has_session(name));
        let started = gone.filter(|(name, program)| {
            let name = Some(name.to_string());
            let (cwd, command) = (program.cwd.clone(), program.command.clone());
            let purpose = client::Purpose::default();
            client::new_session_for(&self.socket, name, cwd, command, purpose).is_ok()
        });
        started.count()
    }

    /// Writes the tabs down when they've changed, so that they're there the
    /// next time the TUI opens, however this one ends.
    fn keep_tabs(&mut self) {
        let tabs = self.app.tabs_to_keep();
        if tabs != self.kept_tabs {
            if let Ok(db) = &self.db {
                let _ = db.keep_ui(db::TABS, &tabs);
            }
            self.kept_tabs = tabs;
        }
        // The sidebar's width goes with them; whether it's folded is the
        // config's to say at the start.
        let sidebar = self.app.sidebar_shape();
        let resized = sidebar.width != self.kept_sidebar.width
            || sidebar.from_config != self.kept_sidebar.from_config;
        if resized && let Ok(db) = &self.db {
            let _ = db.keep_ui(db::SIDEBAR, &sidebar);
        }
        self.kept_sidebar = sidebar;
        // And the projects folded in it.
        let folded = self.app.folded_projects();
        if *folded != self.kept_folded {
            if let Ok(db) = &self.db {
                let _ = db.keep_ui(db::FOLDED, folded);
            }
            self.kept_folded = folded.clone();
        }
    }

    /// Tells the thread that counts what worktrees have changed which the
    /// sidebar shows now, and which to count again straight away.
    fn count_worktrees(&mut self) {
        let shown = self.app.shown_worktrees();
        let mut wanted = self.stat_worktrees.lock().unwrap();
        if *wanted != shown {
            *wanted = shown;
        }
        drop(wanted);
        let due = self.app.take_stats_due();
        if !due.is_empty() {
            self.stats_now.lock().unwrap().extend(due);
        }
    }

    /// The document called `name` the TUI keeps, when there's one to read.
    fn ui(&self, name: &str) -> Option<String> {
        self.db.as_ref().ok()?.ui(name).ok()?
    }

    /// Keeps what the new-session panel remembers, if it can.
    fn keep_memory(&self) {
        if let Ok(db) = &self.db {
            let _ = db.keep_ui(db::LAUNCHER, self.app.memory());
        }
    }

    /// What the diff view keeps: the files marked reviewed, and whether it
    /// lists them as a tree.
    fn review(&self) -> review::Kept {
        review::read(self.ui(db::DIFF).as_deref())
    }

    fn keep_review(&self, kept: &review::Kept) -> Result<()> {
        let db = self.db.as_ref().map_err(|why| anyhow::anyhow!("{why}"))?;
        db.keep_ui(db::DIFF, kept)
    }

    /// The layouts saved with `S`, or why they can't be read.
    fn layouts(&self) -> Result<Layouts, String> {
        let db = self.db.as_ref().map_err(Clone::clone)?;
        let json = db
            .ui(db::LAYOUTS)
            .map_err(|err| format!("couldn't read the saved layouts: {err:#}"))?;
        layouts::read(json.as_deref())
    }

    fn keep_layouts(&self, layouts: &Layouts) -> Result<()> {
        let db = self.db.as_ref().map_err(|why| anyhow::anyhow!("{why}"))?;
        db.keep_ui(db::LAYOUTS, layouts)
    }

    /// The directory a new session at `place` starts in, as
    /// [`directory_for`] finds or makes it. A worktree made for it is
    /// listed straight away, so that it stays in the sidebar even if its
    /// session ends at once.
    fn start_dir(&self, place: Place) -> Result<PathBuf> {
        let makes_a_worktree = matches!(place, Place::NewWorktree { .. });
        let dir = directory_for(&self.socket, place)?;
        if makes_a_worktree {
            self.list_worktrees_again();
        }
        Ok(dir)
    }

    /// Has git asked about every project's worktrees again, straight away.
    fn list_worktrees_again(&self) {
        self.list_worktrees_now.store(true, Ordering::Relaxed);
    }

    /// Takes a fresh list of sessions, and tells the forge poller and the
    /// worktree lister which projects they're in.
    fn set_sessions(&mut self, sessions: Vec<SessionInfo>) {
        // A session the TUI knew of that has come to be marked as having
        // rung its bell, out of sight, rings the user's terminal.
        let known = self.app.sessions();
        let rang = sessions.iter().any(|session| {
            session.bell && known.iter().any(|old| old.id == session.id && !old.bell)
        });
        if rang {
            let _ = self.ringer.ring();
        }
        self.app.set_sessions(sessions);
        self.list_worktrees_of_projects();
        let projects = self.app.projects();
        // With the github plugin off, the poller has nothing to ask about.
        let asked = if self.app.github_on() {
            projects
        } else {
            Vec::new()
        };
        *self.projects.lock().unwrap() = asked;
    }

    /// Tells the worktree lister which projects to list the worktrees of:
    /// those the sessions are in, and those crystal knows.
    fn list_worktrees_of_projects(&mut self) {
        let mut projects = self.app.projects();
        projects.extend(self.app.known_projects().iter().map(|w| w.path.clone()));
        projects.sort();
        projects.dedup();
        *self.worktree_projects.lock().unwrap() = projects;
    }

    /// Takes the projects crystal knows, as the daemon listed them.
    fn set_known_projects(&mut self, projects: Vec<Worktree>) {
        self.app.set_known_projects(projects);
        self.list_worktrees_of_projects();
    }

    /// Asks the forge, off the loop, for the issue or pull request the
    /// open view's bar is on, read whole, the first time the bar is on it.
    fn read_topic(&mut self) {
        let Some((project, topic)) = self.app.topic_to_read() else {
            return;
        };
        self.read_in_background(move || {
            let asked = Instant::now();
            let repo = Repo::find(&project);
            match topic {
                Topic::Issue(number) => Event::IssueRead {
                    read: repo.and_then(|repo| repo.issue(number)),
                    project,
                    number,
                    asked,
                },
                Topic::PullRequest(number) => Event::PullRequestRead {
                    read: repo.and_then(|repo| repo.pull_request(number)),
                    project,
                    number,
                    asked,
                },
            }
        });
    }

    /// The next event. While an agent works, the wait is cut short in time
    /// to turn its mark, while a drag is held past the edge of a pane, to
    /// scroll it again, and while a key shows at the footer, to take it off,
    /// and there's no event: only a frame to draw.
    fn next_event(&self, events: &Receiver<Event>) -> Result<Option<Event>> {
        let spin = self.app.anything_working().then_some(SPIN_EVERY);
        let edge = self
            .edge
            .map(|edge| edge.next.saturating_duration_since(Instant::now()));
        // A key shown at the footer goes when its time is up.
        let shown_key = self
            .app
            .shown_key_goes()
            .map(|goes| goes.saturating_duration_since(Instant::now()));
        let Some(wait) = spin.into_iter().chain(edge).chain(shown_key).min() else {
            return Ok(Some(events.recv()?));
        };
        match events.recv_timeout(wait) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => bail!("the TUI's events stopped"),
        }
    }

    fn handle(&mut self, event: Event) {
        if matches!(
            event,
            Event::Key(_) | Event::Mouse(_) | Event::Paste(_) | Event::Focus(true)
        ) {
            self.layout.used();
        }
        if matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_)) {
            self.user_is_here();
        }
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Mouse(mouse) => self.on_mouse(mouse),
            Event::Paste(text) => {
                if let Some(action) = self.app.on_paste(text) {
                    self.carry_out(action);
                }
            }
            Event::Layout(relayed) => {
                let answer = self.obey(relayed.order);
                self.layout.answer(relayed.id, answer);
            }
            Event::CodexModels(models) => self.app.set_codex_models(models),
            Event::Resize => {}
            // A list asked for before the one the TUI has may lack a session
            // started since, which would leave the sidebar as it came.
            Event::Sessions { asked, .. } if asked < self.sessions_asked => {}
            Event::Sessions { sessions, asked } => {
                self.sessions_asked = asked;
                self.attach_again(&sessions);
                self.set_sessions(sessions);
            }
            Event::Flows(runs) => self.app.set_flows(runs),
            Event::Projects(projects) => self.set_known_projects(projects),
            Event::PullRequests {
                project,
                found,
                asked,
            } => self.app.set_pull_requests(project, found, asked),
            Event::Worktrees {
                project,
                worktrees,
                subjects,
                labels,
            } => {
                self.app.set_subjects(&project, subjects);
                self.app.set_labels(&project, labels);
                self.app.set_worktrees(project, worktrees);
            }
            Event::Stat { path, stat } => self.app.set_stat(path, stat),
            // One someone else asked for that's done with has git list the
            // worktrees again, for it to leave the sidebar if it's gone.
            Event::Removals(worktrees) => {
                if self.app.set_removals(worktrees) {
                    self.list_worktrees_again();
                }
            }
            Event::WorktreeRemoved { path, removed } => self.worktree_removed(&path, removed),
            Event::WorktreeHasChanges { path, branch } => {
                self.app.ask_to_force_removal(path, branch);
            }
            Event::Issues {
                project,
                found,
                asked,
            } => self.app.set_issues(&project, found, asked),
            Event::IssueRead {
                project,
                number,
                read,
                asked,
            } => self.app.set_issue(&project, number, read, asked),
            Event::PullRequestRead {
                project,
                number,
                read,
                asked,
            } => self.app.set_pull_request(&project, number, read, asked),
            Event::Replied { name, sent } => {
                self.app.replied(&name, sent);
                // A background task's follow-up has started a run.
                let _ = self.refresh_sessions();
            }
            Event::Commented {
                project,
                topic,
                posted,
            } => self.app.commented(&project, topic, posted),
            Event::IssueEdited {
                project,
                number,
                edit,
                saved,
                at,
            } => self.app.issue_edited(&project, number, edit, saved, at),
            Event::Fetched(start) => {
                self.list_worktrees_again();
                self.carry_out(*start);
            }
            Event::Notice(notice) => self.app.notify(notice),
            Event::WhatsNew(body) => {
                self.app.show_page(Page::new(&update::notes_title(), &body));
            }
            Event::Resources(taken) => self.app.set_resources(taken),
            Event::Output { pane, bytes } => {
                let Some(pane) = self.pane_with_id(pane) else {
                    return;
                };
                pane.screen.process(&bytes);
                let rang = pane.screen.take_bells() > 0;
                let copied = pane.screen.take_copied();
                let session = pane.session_id.clone();
                if rang {
                    let _ = self.ringer.ring();
                }
                if let Some(text) = copied {
                    self.copy_for_program(&session, &text);
                }
            }
            // The next list says whether the session has ended, or runs on
            // in a daemon handed over to a new crystal (`attach_again`).
            Event::OutputEnded { pane } => {
                if let Some(pane) = self.pane_with_id(pane) {
                    pane.ended = true;
                }
            }
            Event::DiffRead { dir, against, read } => {
                let kept = self.review();
                let read = read.map(|mut read| {
                    read.reviewed = kept.marks(&diff_view::scope(&dir, against, &read.head));
                    read
                });
                self.app.diff_read(&dir, against, read);
            }
            Event::FilesRead { dir, files } => {
                if let Some(action) = self.app.files_read(&dir, files) {
                    self.carry_out(action);
                }
            }
            Event::PreviewRead { dir, path, read } => self.app.preview_read(&dir, &path, read),
            Event::BranchesListed { dir, listed } => {
                if let Some(action) = self.app.branches_listed(&dir, listed) {
                    self.carry_out(action);
                }
            }
            Event::BranchesFetched { dir, fetched } => {
                self.fetching_remotes.remove(&dir);
                if fetched.is_ok() {
                    self.fetched_remotes.insert(dir.clone(), Instant::now());
                }
                if let Some(action) = self.app.branches_fetched(&dir, fetched) {
                    self.carry_out(action);
                }
            }
            Event::BranchSwitched { dir, outcome } => {
                if let Some(action) = self.app.branch_switched(&dir, outcome) {
                    self.carry_out(action);
                }
            }
            Event::Searched { dir, query, found } => {
                if let Some(action) = self.app.searched(&dir, &query, found) {
                    self.carry_out(action);
                }
            }
            Event::MatchedFileRead { dir, path, lines } => {
                self.app.matched_file_read(&dir, &path, lines);
            }
            Event::MemoryRead { dir, read } => self.app.memory_read(&dir, read),
            Event::Backlog { dir, found } => self.app.set_backlog(&dir, found),
            Event::BacklogCounts(counts) => self.app.set_backlog_counts(counts),
            Event::Spending(spending) => self.app.set_spending(spending),
            Event::Settings(current) => {
                // The file changed by hand, or by another crystal, counts
                // here too, straight away.
                if let Ok(config) = &current.config {
                    self.config_changed(config);
                }
                self.app.show_settings(*current);
            }
            Event::ConfigFile(read) => match *read {
                // A change the settings view made is the config already.
                Ok(config) if config != self.config => {
                    self.config_changed(&config);
                    self.app
                        .notify("the config file changed: the settings follow it".into());
                }
                Ok(_) => {}
                Err(why) => self.app.notify(format!(
                    "the config file can't be read, so the settings stay as they were: {why}"
                )),
            },
            Event::Focus(true) => {
                self.layout.focus(true);
                if let Some(went) = self.presence.focus_gained(events::now_ms()) {
                    self.look_back(Since::At(went));
                }
            }
            Event::Focus(false) => {
                self.layout.focus(false);
                self.presence.focus_lost(events::now_ms());
                self.keep_seen();
            }
            Event::EventsRead { feed, read } if self.following(feed) => {
                if let Some(action) = self.app.events_read(read) {
                    self.carry_out(action);
                }
            }
            Event::Logged { feed, event } if self.following(feed) => self.app.logged(*event),
            // Read for a timeline that has closed since.
            Event::EventsRead { .. } | Event::Logged { .. } => {}
            Event::Appearance(appearance) => {
                if Some(appearance) != self.appearance {
                    self.appearance = Some(appearance);
                    self.theme = Theme::from_config(&self.config, self.appearance);
                }
            }
            Event::Status(status) => self.app.set_status(status),
            Event::Away(Ok(tally)) => self.app.set_away(&tally),
            Event::HandoffFound { session, found } => {
                if let Some(action) = self.app.handoff_found(&session, found) {
                    self.carry_out(action);
                }
            }
            Event::Away(Err(reason)) => {
                self.app
                    .notify(format!("couldn't read the event log: {reason}"));
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if let Some(action) = self.app.on_key(key) {
            self.carry_out(action);
        }
    }

    /// Carries out a layout command from the command line, and what it
    /// needs done outside the state, and says what the tabs came to, or
    /// why it couldn't.
    fn obey(&mut self, order: Order) -> Result<Layout, String> {
        let failed = |err: anyhow::Error| format!("{err:#}");
        // A session it names may have started a moment ago, too lately for
        // the last list.
        self.refresh_sessions().map_err(failed)?;
        let raise = matches!(
            order.command,
            crate::layout::Command::Focus { raise: true, .. }
        );
        if let Some(action) = self.app.obey(order)? {
            self.perform(action).map_err(failed)?;
        }
        if raise {
            raise_terminal();
        }
        Ok(self.app.layout())
    }

    /// Performs `action`. One that fails, say because its session has just
    /// gone, says why at the bottom rather than closing the TUI.
    fn carry_out(&mut self, mut action: Action) {
        if let Some(Place::PullRequest(checkout)) = action.place_mut() {
            let checkout = checkout.clone();
            self.fetch_then_start(checkout, action);
            return;
        }
        let starts = action.place_mut().is_some();
        let done = self.perform(action);
        if starts {
            self.app.start_done(done.is_ok());
        }
        if let Err(err) = done {
            self.app.notify(format!("{err:#}"));
        }
    }

    /// Finds or makes the worktree `checkout` says, off the loop, since
    /// making one fetches over the network; then `start` goes on in it.
    fn fetch_then_start(&mut self, checkout: Checkout, mut start: Action) {
        self.app.notify(format!("fetching {}…", checkout.branch));
        let socket = self.socket.clone();
        self.read_in_background(
            move || match client::pull_request_worktree(&socket, &checkout) {
                Ok(dir) => {
                    if let Some(place) = start.place_mut() {
                        *place = Place::Directory(Some(dir));
                    }
                    Event::Fetched(Box::new(start))
                }
                Err(err) => Event::Notice(format!("{err:#}")),
            },
        );
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        // A plugin's pane takes the keyboard, not the mouse.
        if self.overlay.is_some() {
            return;
        }
        // An open menu has the mouse; a right click elsewhere closes it and
        // opens another there.
        let right_click = mouse.kind == MouseEventKind::Down(MouseButton::Right);
        if self.app.menu().is_some() {
            if let Some(action) = self.app.menu_mouse(mouse.kind, mouse.column, mouse.row) {
                self.carry_out(action);
            }
            if !right_click {
                return;
            }
        }
        let areas = ui::Areas::of(&self.app, self.screen);
        let mut hit = ui::hit(&areas, &self.app, mouse.column, mouse.row);
        if self.click_on_link(&mouse, hit) {
            return;
        }
        if self.app.dragging_sidebar() {
            // So is the sidebar's edge, wherever the mouse goes.
            hit = Hit::SidebarEdge(mouse.column);
        } else if let Some(split) = self.app.moving_border() {
            // A border taken by the mouse is crystal's until it's let go.
            hit = ui::border_hit(&areas, &self.app, split, mouse.column, mouse.row);
        } else if let Some(slot) = self.app.dragging() {
            // A selection being dragged is crystal's to the end, and keeps
            // to the edge of its pane when the mouse leaves it. Past its
            // top or bottom, the history scrolls under it, on and on while
            // the mouse stays there.
            let cell = ui::nearest_cell(&areas, &self.app, slot, mouse.column, mouse.row);
            hit = Hit::Pane { slot, cell };
            let past = ui::rows_past(&areas, &self.app, slot, mouse.row);
            self.edge = match mouse.kind {
                MouseEventKind::Drag(_) if past == 0 => None,
                MouseEventKind::Drag(_) => Some(Edge {
                    slot,
                    past,
                    next: self.edge.map_or_else(Instant::now, |edge| edge.next),
                }),
                MouseEventKind::Down(_) | MouseEventKind::Up(_) => None,
                _ => self.edge,
            };
        } else if let Some(slot) = self.app.holding_thumb() {
            // So is a scrollbar's thumb, which keeps to its track.
            hit = match ui::scrollbar_row(&areas, &self.app, slot, mouse.row) {
                Some(row) => Hit::Scrollbar { slot, row },
                None => Hit::Elsewhere,
            };
        } else if self.app.grabbed().is_none() && self.pass_to_program(&mouse, hit) {
            return;
        }
        // A right click the program in a pane didn't take opens a menu.
        if right_click {
            if let Some(action) = self.app.right_click(hit, (mouse.column, mouse.row)) {
                self.carry_out(action);
            }
            return;
        }
        if let Some(action) = self.app.on_mouse(mouse.kind, hit) {
            self.carry_out(action);
        }
    }

    /// The mouse moved: with Ctrl held, onto the link to underline.
    /// Returns whether that changes what's drawn.
    fn mouse_moved(&mut self, mouse: &MouseEvent) -> bool {
        if self.overlay.is_some() {
            return false;
        }
        // Over a menu, the bar follows the mouse.
        if let Some(was) = self.app.menu().map(|menu| menu.highlighted) {
            self.app.menu_mouse(mouse.kind, mouse.column, mouse.row);
            return self.app.menu().map(|menu| menu.highlighted) != Some(was);
        }
        let areas = ui::Areas::of(&self.app, self.screen);
        let hit = ui::hit(&areas, &self.app, mouse.column, mouse.row);
        let ctrl = mouse.modifiers.contains(KeyModifiers::CONTROL);
        self.app.mouse_moved(hit, ctrl)
    }

    /// Scrolls the history under a drag held past the edge of its pane's
    /// screen, when it's time to again. Returns whether that moved the
    /// view: at the end of the history it stays, and so does the timer
    /// until the mouse moves again.
    fn scroll_at_edge(&mut self) -> bool {
        let Some(edge) = self.edge else {
            return false;
        };
        let now = Instant::now();
        if now < edge.next {
            return false;
        }
        if self.app.dragging() != Some(edge.slot) {
            self.edge = None;
            return false;
        }
        self.edge = Some(Edge {
            next: now + pane::EDGE_SCROLL_EVERY,
            ..edge
        });
        let Some(pane) = self.pane_in(edge.slot) else {
            return false;
        };
        let back = pane.scrolled_back();
        pane.scroll_past_edge(edge.past);
        let moved = pane.scrolled_back() != back;
        if !moved {
            self.edge = None;
        }
        moved
    }

    /// Ctrl and a click on a link in a pane opens it, whoever has the
    /// mouse there, and the button coming up after is the click's too.
    /// Returns whether the mouse did that.
    fn click_on_link(&mut self, mouse: &MouseEvent, hit: Hit) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left)
                if mouse.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let Some((slot, cell)) = self.app.link_cell(hit) else {
                    return false;
                };
                let link = self
                    .pane_in(slot)
                    .and_then(|pane| pane.screen.link_at(cell));
                let Some(link) = link else {
                    return false;
                };
                self.link_clicked = true;
                let context = self.app.link_context(slot);
                if let Err(err) = self.open_link(link.url, context) {
                    self.app.notify(format!("{err:#}"));
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => std::mem::take(&mut self.link_clicked),
            _ => false,
        }
    }

    /// Opens `url`, a link a pane shows, about `context`: with the action
    /// of the first plugin that handles links like it, or in the browser.
    fn open_link(&mut self, url: String, context: Context) -> Result<()> {
        let config = Config::load()?;
        if let Some((plugin, action)) = plugins::link_handler(&config, &self.socket, &url) {
            let context = Context {
                link: Some(url),
                ..context
            };
            return self.run_plugin(&Id::own(&plugin), &action, context);
        }
        let said = links::open(&url)?;
        self.app.notify(said);
        Ok(())
    }

    /// Hands the mouse to the program in the pane that has the keyboard,
    /// if it asked for the mouse and the pane is showing it live (back in
    /// the history, the program's screen isn't what's under the mouse).
    /// Returns whether the program took it.
    fn pass_to_program(&mut self, mouse: &MouseEvent, hit: Hit) -> bool {
        let Hit::Pane {
            slot,
            cell: Some(cell),
        } = hit
        else {
            return false;
        };
        if self.app.focus() != Focus::Pane(slot) {
            return false;
        }
        let Some(pane) = self.pane_in(slot) else {
            return false;
        };
        let Some(protocol) = mouse::Protocol::of(&pane.screen) else {
            return false;
        };
        if pane.scrolled_back() > 0 {
            return false;
        }
        // An event the program didn't ask for, like a drag when it asked
        // only for clicks, is still the program's: it has the mouse here.
        if let Some(bytes) = mouse::encode(mouse.kind, mouse.modifiers, cell, protocol) {
            pane.send_keys(&bytes);
        }
        true
    }

    /// Puts the project in `dir` on the list, once it's one: a directory
    /// that isn't there, or isn't in a git repository, is asked about
    /// first. One in a project already, its root or not, puts that project
    /// on the list, if it isn't on it yet.
    fn add_project(&mut self, dir: PathBuf) -> Result<()> {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        if !dir.exists() {
            self.app.confirm_new_project(dir, true);
            return Ok(());
        }
        ensure!(
            dir.is_dir(),
            "{} isn't a directory",
            shell::home_relative(&dir)
        );
        if git::Checkout::find(&dir).is_none() {
            self.app.confirm_new_project(dir, false);
            return Ok(());
        }
        client::ask(
            &self.socket,
            &Request::AddProject { dir: dir.clone() },
            false,
        )?;
        if let Some(projects) = list_projects(&self.socket) {
            self.set_known_projects(projects);
        }
        self.app.project_added(&project::of(&dir).path);
        Ok(())
    }

    fn perform(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => self.quitting = true,
            Action::Start {
                place,
                command,
                purpose,
            } => {
                let cwd = self.start_dir(place)?;
                let name = client::new_session_for(&self.socket, None, cwd, command, purpose)?.name;
                self.show_new_session(&name)?;
                self.keep_memory();
            }
            Action::StartInBackground {
                place,
                spec,
                backlog,
                brief,
            } => {
                let cwd = self.start_dir(place)?;
                let name = client::new_task(&self.socket, None, cwd, spec, backlog, brief)?.name;
                // A background task takes no keys: the sidebar keeps them.
                self.refresh_sessions()?;
                self.app.select(&name);
                self.keep_memory();
            }
            Action::StartFlow { place, flow, goal } => {
                let cwd = self.start_dir(place)?;
                let run = client::start_flow(&self.socket, &flow, &goal, cwd)?;
                // Its first step takes no keys: the sidebar keeps them.
                self.refresh_sessions()?;
                let first = self.app.flows().iter().find(|found| found.name == run);
                let session = first.and_then(|run| run.steps.first()?.session.clone());
                if let Some(session) = session {
                    self.app.select(&session);
                }
                self.keep_memory();
            }
            Action::ApproveFlow(run) => {
                client::ask(&self.socket, &Request::ApproveFlow { run }, false)?;
                self.refresh_sessions()?;
            }
            Action::SendFlowBack { run, notes } => {
                client::ask(&self.socket, &Request::SendFlowBack { run, notes }, false)?;
                self.refresh_sessions()?;
            }
            Action::RetryFlow(run) => {
                client::ask(&self.socket, &Request::RetryFlow { run }, false)?;
                self.refresh_sessions()?;
            }
            Action::CloseTask {
                name,
                failed,
                summary,
            } => {
                client::close_task(&self.socket, &name, failed, &summary)?;
                self.refresh_sessions()?;
            }
            Action::Answer { name, answer } => {
                drive::answer(&self.socket, &name, answer, None)?;
                self.refresh_sessions()?;
            }
            Action::Interrupt(name) => {
                drive::interrupt(&self.socket, &name)?;
                self.refresh_sessions()?;
            }
            Action::Reply { name, text } => {
                // Typing into a terminal takes a moment, off the loop. From
                // the user, it goes as typed: no session is said to send it,
                // even when the TUI runs in one.
                let socket = self.socket.clone();
                self.read_in_background(move || {
                    let request = Request::Send {
                        name: name.clone(),
                        text,
                        enter: true,
                        from: None,
                        force: false,
                    };
                    let sent = client::ask(&socket, &request, false)
                        .and_then(|answer| answer.context("no daemon is running"))
                        .map(|_| ())
                        .map_err(|err| format!("{err:#}"));
                    Event::Replied { name, sent }
                });
            }
            Action::ListBacklog(dir) => self.list_backlog(dir),
            Action::ChangeBacklog { dir, change } => {
                let request = match change {
                    BacklogChange::Add(text) => Request::BacklogAdd {
                        dir: dir.clone(),
                        text,
                        body: String::new(),
                        tags: Vec::new(),
                    },
                    BacklogChange::Edit { number, text } => Request::BacklogEdit {
                        dir: dir.clone(),
                        number,
                        text: Some(text),
                        body: None,
                        tags: None,
                    },
                    BacklogChange::Mark { number, done } => Request::BacklogMark {
                        dir: dir.clone(),
                        number,
                        done,
                    },
                    BacklogChange::Remove(number) => Request::BacklogRemove {
                        dir: dir.clone(),
                        number,
                    },
                };
                client::ask(&self.socket, &request, false)?;
                self.list_backlog(dir);
            }
            Action::Paste { to, text } => {
                if let Some(pane) = self.pane_in(to) {
                    pane.send_keys(&pasted(&text, pane.wants_paste_marked()));
                }
            }
            Action::ReadCodexModels => {
                self.read_in_background(|| Event::CodexModels(read_codex_models()));
            }
            Action::ReadDiff { dir, against } => {
                self.read_in_background(move || {
                    let read = diff_view::read(&dir, against);
                    Event::DiffRead { dir, against, read }
                });
            }
            Action::KeepReviewed { scope, marks } => {
                let mut kept = self.review();
                kept.set_marks(scope, marks);
                self.keep_review(&kept)?;
            }
            Action::KeepTree(on) => {
                let mut kept = self.review();
                kept.tree = on;
                self.keep_review(&kept)?;
            }
            Action::ReadFiles(dir) => {
                self.read_in_background(move || {
                    let files = finder::read_files(&dir);
                    Event::FilesRead { dir, files }
                });
            }
            Action::ReadPreview { dir, path } => {
                self.read_in_background(move || {
                    let read = preview::read(&dir, &path);
                    Event::PreviewRead { dir, path, read }
                });
            }
            Action::CopyPath(path) => {
                clipboard::copy(&path).context("couldn't copy")?;
                self.app.notify(format!("copied {path}"));
            }
            Action::ReadMemory(dir) => {
                let socket = self.socket.clone();
                self.read_in_background(move || {
                    let read = read_memory(&socket, &dir);
                    Event::MemoryRead { dir, read }
                });
            }
            Action::ForgetMemory { dir, id } => {
                let socket = self.socket.clone();
                self.read_in_background(move || {
                    let project = memory::project_of(&dir);
                    match memory::remove(&socket, &project, id) {
                        Ok(entry) => {
                            let forgotten = events::Event::memory(
                                events::Kind::MemoryForgotten,
                                project,
                                entry,
                            );
                            // The entry is gone either way.
                            let _ = client::tell(&socket, forgotten);
                            Event::MemoryRead {
                                read: read_memory(&socket, &dir),
                                dir,
                            }
                        }
                        Err(err) => Event::Notice(format!("{err:#}")),
                    }
                });
            }
            Action::PromoteMemory { dir, id } => {
                let socket = self.socket.clone();
                self.read_in_background(move || match promote_memory(&socket, &dir, id) {
                    Ok(file) => {
                        let file = crate::shell::home_relative(&file);
                        Event::Notice(format!("added entry {id} to {file}"))
                    }
                    Err(err) => Event::Notice(format!("{err:#}")),
                });
            }
            Action::ListBranches(dir) => {
                self.read_in_background(move || {
                    let listed = switcher::read(&dir);
                    Event::BranchesListed { dir, listed }
                });
            }
            Action::FetchBranches { dir, now } => self.fetch_remotes(dir, now),
            Action::SwitchBranch { dir, target, carry } => {
                // git can take a while, over a commit's hooks say.
                self.read_in_background(move || {
                    let outcome = git::branches::switch(&dir, &target, &carry);
                    Event::BranchSwitched { dir, outcome }
                });
            }
            Action::Grep { dir, query } => self.search(dir, query),
            Action::ReadMatchedFile { dir, path } => {
                self.read_in_background(move || {
                    let lines = grep::read_file(&dir, &path);
                    Event::MatchedFileRead { dir, path, lines }
                });
            }
            Action::Edit {
                dir,
                path,
                line,
                name,
            } => {
                let command = editor_at(editor()?, path, line);
                self.start_session(Some(name), dir, command)?;
            }
            Action::EditHistory { slot, dir, name } => {
                let Some(pane) = self.pane_in(slot) else {
                    bail!("there's nothing on the pane yet");
                };
                let text = pane.screen.text();
                let path = history_path(&self.socket, &name);
                write_history(&path, &text)?;
                let mut command = editor()?;
                command.push(path.display().to_string());
                self.start_session(Some(name), dir, command)?;
            }
            Action::SaveProfile { replacing, profile } => {
                let saved = profile::save(&config::path(), replacing.as_deref(), &profile);
                self.profiles_changed(saved, Some(&profile.name));
            }
            Action::DeleteProfile(name) => {
                let deleted = profile::delete(&config::path(), &name);
                self.profiles_changed(deleted, None);
            }
            Action::OpenSettings => {
                self.poll_settings.store(true, Ordering::Relaxed);
                self.read_settings_now();
            }
            Action::CloseSettings => self.poll_settings.store(false, Ordering::Relaxed),
            Action::ChangeSetting(change) => {
                let mut edits = vec![change.edit()];
                // A theme picked by hand is the one wanted, whatever the
                // system's appearance.
                if change.setting == Setting::Theme && self.config.appearance.auto_switch {
                    edits.push(settings_view::Change::set(Setting::AutoSwitch, false).edit());
                }
                if self.write_settings(&edits)
                    && change == settings_view::Change::set(Setting::Embeddings, true)
                {
                    self.prepare_embeddings();
                }
            }
            Action::ChangeKeys(rebinding) => {
                self.write_settings(&settings_view::key_edits(&rebinding));
            }
            Action::PrepareEmbeddings => {
                self.prepare_embeddings();
                self.read_settings_now();
            }
            Action::Integrate { agent, install } => {
                let integrated = std::env::current_exe()
                    .map_err(anyhow::Error::from)
                    .and_then(|crystal| match install {
                        true => crate::integration::install(agent, &crystal),
                        false => crate::integration::uninstall(agent).map(|said| vec![said]),
                    });
                match integrated {
                    // The last line says what's left to do, like Codex's
                    // review of its hooks.
                    Ok(said) => self
                        .app
                        .setting_noted(said.last().cloned().unwrap_or_default()),
                    Err(err) => self.app.setting_failed(format!("{err:#}")),
                }
                self.read_settings_now();
            }
            Action::ListPlugins => {
                let config = Config::load()?;
                let listed = listed_plugins(&config, &self.socket, self.selected_project());
                self.app.show_plugins(listed);
            }
            Action::SwitchPlugin { name, project, on } => {
                let path = config::path();
                let id = Id { name, project };
                let switched = can_switch(&id, on)
                    .and_then(|()| plugins::switch(&path, &self.socket, &id, on))
                    .and_then(|()| Config::load());
                match switched {
                    Ok(config) => self.plugins_changed(&config),
                    Err(err) => self.app.plugin_failed(format!("{err:#}")),
                }
            }
            Action::RunPlugin {
                plugin,
                project,
                action,
                context,
            } => {
                let id = Id {
                    name: plugin,
                    project,
                };
                self.run_plugin(&id, &action, context)?
            }
            Action::OpenPluginPane {
                plugin,
                project,
                pane,
                context,
            } => {
                let id = Id {
                    name: plugin,
                    project,
                };
                self.open_plugin_pane(&id, &pane, context)?
            }
            Action::ShowPluginPane(pane) => self.show_over(pane)?,
            Action::TypeInPluginPane(key) => {
                if let Some(pane) = &mut self.overlay
                    && let Some(bytes) = keys::encode_for(&key, &pane.screen)
                {
                    pane.send_keys(&bytes);
                }
            }
            Action::PasteInPluginPane(text) => {
                if let Some(pane) = &mut self.overlay {
                    pane.send_keys(&pasted(&text, pane.wants_paste_marked()));
                }
            }
            Action::ClosePluginPane => self.close_plugin_pane(),
            Action::RunKeyCommand {
                command,
                dir,
                context,
            } => self.run_key_command(*command, dir, context)?,
            Action::ListLayouts => {
                let found = self.layouts();
                self.app.show_layouts(found, None);
            }
            Action::SaveLayout(name) => {
                let mut kept = self.layouts().map_err(anyhow::Error::msg)?;
                let tabs = self.app.tabs_to_keep();
                let programs = self.app.programs();
                let replaced = kept.save(&name, tabs, programs, seconds_since_epoch());
                self.keep_layouts(&kept)?;
                self.app
                    .show_layouts(Ok(kept), Some(&Which::Saved(name.clone())));
                let how = if replaced { "over" } else { "as" };
                self.app.notify(format!("saved your tabs {how} {name}"));
            }
            Action::RestoreLayout(which) => {
                let mut kept = self.layouts().map_err(anyhow::Error::msg)?;
                let current = self.app.tabs_to_keep();
                let name = match &which {
                    Which::Saved(name) => name.clone(),
                    Which::Before => "the tabs from before".to_string(),
                };
                let programs = self.app.programs();
                let now = seconds_since_epoch();
                let Some(restored) = kept.restore(&which, current, programs, now) else {
                    bail!("{name} can't be restored: it's from another crystal");
                };
                self.keep_layouts(&kept)?;
                let started = self.start_again(&restored.programs);
                if started > 0 {
                    self.refresh_sessions()?;
                }
                self.app.restore_layout(restored.tabs, &name, started);
            }
            Action::FollowEvents(scope) => self.follow_events(scope),
            Action::StopFollowing => {
                self.feed.fetch_add(1, Ordering::Relaxed);
            }
            Action::OpenRam => self.ram_open.store(true, Ordering::Relaxed),
            Action::CloseRam => self.ram_open.store(false, Ordering::Relaxed),
            Action::ReadOlderEvents { scope, before } => {
                let feed = self.feed.load(Ordering::Relaxed);
                let socket = self.socket.clone();
                self.read_in_background(move || Event::EventsRead {
                    feed,
                    read: read_events(&socket, &scope, Some(before)).map(|(_, page)| page),
                });
            }
            Action::ReadHandoff {
                session,
                worktree,
                task,
            } => {
                let socket = self.socket.clone();
                self.read_in_background(move || Event::HandoffFound {
                    found: find_handoff(&socket, worktree, task),
                    session,
                });
            }
            Action::RemoveLayout(which) => {
                let mut kept = self.layouts().map_err(anyhow::Error::msg)?;
                kept.remove(&which);
                self.keep_layouts(&kept)?;
                self.app.show_layouts(Ok(kept), None);
                if let Which::Saved(name) = which {
                    self.app.notify(format!("removed {name}"));
                }
            }
            Action::Kill(name) => {
                let emptied = self.app.take_emptied();
                client::ask(&self.socket, &Request::Kill { name }, false)?;
                self.refresh_sessions()?;
                self.ask_about_emptied(emptied)?;
            }
            Action::ForgetProject(path) => {
                client::ask(&self.socket, &Request::RemoveProject { dir: path }, false)?;
                if let Some(projects) = list_projects(&self.socket) {
                    self.set_known_projects(projects);
                }
            }
            Action::AddProject(dir) => self.add_project(dir)?,
            Action::NewProject { dir, create } => {
                if create {
                    std::fs::create_dir_all(&dir)
                        .with_context(|| format!("couldn't make {}", shell::home_relative(&dir)))?;
                }
                git::init(&dir)?;
                self.add_project(dir)?;
            }
            Action::CompleteDirectory(typed) => {
                let completed = shell::complete_dir(&typed, shell::subdirectories);
                self.app.complete_answer(&completed);
            }
            Action::Archive(name) => {
                client::ask(
                    &self.socket,
                    &Request::Archive { name: name.clone() },
                    false,
                )?;
                self.refresh_sessions()?;
                self.app
                    .notify(format!("archived {name}: Z starts it again where it was"));
            }
            Action::ListArchived => {
                let found = match client::ask(&self.socket, &Request::Archived, false) {
                    Ok(Some(Response::Archived { sessions })) => Ok(sessions),
                    Ok(_) => Ok(Vec::new()),
                    Err(err) => Err(format!("{err:#}")),
                };
                self.app.show_archived(found);
            }
            Action::Unarchive(id) => {
                let request = Request::Unarchive {
                    name: id,
                    env: env::current(),
                };
                let name = match client::ask(&self.socket, &request, true)? {
                    Some(Response::Created { name, .. }) => name,
                    _ => bail!("the daemon didn't start it"),
                };
                self.refresh_sessions()?;
                self.app.unarchived(&name);
            }
            Action::DeleteArchived(id) => {
                client::ask(&self.socket, &Request::DeleteArchived { name: id }, false)?;
                self.carry_out(Action::ListArchived);
            }
            Action::ProjectCommand { which, worktree } => {
                self.project_command(which, &worktree)?;
            }
            Action::KillAll(names) => {
                let emptied = self.app.take_emptied();
                // One that has gone already is no reason to spare the rest.
                let mut failed = None;
                for name in names {
                    if let Err(err) = client::ask(&self.socket, &Request::Kill { name }, false) {
                        failed.get_or_insert(err);
                    }
                }
                self.refresh_sessions()?;
                if let Some(err) = failed {
                    return Err(err);
                }
                self.ask_about_emptied(emptied)?;
            }
            Action::Rename { name, new_name } => {
                client::rename(&self.socket, &name, &new_name)?;
                self.app.renamed(&name, &new_name);
                self.refresh_sessions()?;
                self.app.select(&new_name);
            }
            Action::Respawn(name) => {
                client::respawn(&self.socket, &name)?;
                self.refresh_sessions()?;
                self.app.select(&name);
                self.app.type_into_selected();
            }
            Action::TaskToTerminal(name) => {
                let request = Request::TaskToTerminal {
                    task: name.clone(),
                    env: env::current(),
                };
                client::ask(&self.socket, &request, false)?;
                self.refresh_sessions()?;
                self.app.select(&name);
                self.app.type_into_selected();
            }
            Action::RemoveWorktree {
                path,
                branch,
                force,
            } => {
                // git looks for changes, then the daemon has it delete
                // every file in it, which can take a while: off the loop.
                // The daemon carries on if the TUI quits meanwhile.
                let socket = self.socket.clone();
                self.read_in_background(move || {
                    // git won't remove a worktree with changes not
                    // committed unless it's forced, so the user is asked
                    // again, this time about losing them.
                    if !force && git::has_changes(&path).unwrap_or(false) {
                        return Event::WorktreeHasChanges { path, branch };
                    }
                    let removed = client::remove_worktree(&socket, &path, force)
                        .map_err(|err| removal_refused(&path, &err));
                    Event::WorktreeRemoved { path, removed }
                });
            }
            Action::RemoveWorktrees(worktrees) => {
                for (path, branch) in worktrees {
                    self.perform(Action::RemoveWorktree {
                        path,
                        branch,
                        force: false,
                    })?;
                }
            }
            Action::Type { to, key } => {
                if let Some(pane) = self.pane_in(to)
                    && let Some(bytes) = keys::encode_for(&key, &pane.screen)
                {
                    pane.send_keys(&bytes);
                }
            }
            Action::PageBack(slot) => {
                if let Some(pane) = self.pane_in(slot) {
                    pane.page_back();
                }
            }
            Action::PageForward(slot) => {
                if let Some(pane) = self.pane_in(slot) {
                    pane.page_forward();
                }
            }
            Action::ScrollBack(slot) | Action::ScrollForward(slot) => {
                let back = matches!(action, Action::ScrollBack(_));
                let lines = self.config.mouse.scroll_lines;
                let selecting = self.app.dragging() == Some(slot);
                let typed_into = self.app.can_type_into(slot);
                if let Some(pane) = self.pane_in(slot) {
                    // A program on the alternate screen keeps no history to
                    // scroll: a pager is moved with the arrow keys, unless
                    // copy mode or a selection has the pane.
                    let arrows = mouse::alternate_scroll(&pane.screen, back, lines)
                        .filter(|_| typed_into && !selecting && pane.copy.is_none());
                    if let Some(arrows) = arrows {
                        pane.send_keys(&arrows);
                        return Ok(());
                    }
                    let lines = usize::from(lines);
                    if back {
                        pane.scroll_back(lines);
                    } else {
                        pane.scroll_forward(lines);
                    }
                    // A selection being dragged goes on to what's under the
                    // mouse now.
                    if selecting {
                        pane.follow_drag();
                    }
                }
            }
            Action::CopyKey { slot, key } => {
                let Some(pane) = self.pane_in(slot) else {
                    self.app.stop_copying();
                    return Ok(());
                };
                match pane.copy_key(key) {
                    copy_mode::Outcome::Stay => {}
                    copy_mode::Outcome::Say(said) => self.app.notify(said),
                    copy_mode::Outcome::Leave => self.app.stop_copying(),
                    copy_mode::Outcome::Copy(text) => {
                        self.app.stop_copying();
                        self.copy_to_clipboard(&text)?;
                    }
                    copy_mode::Outcome::Open(url) => {
                        self.app.stop_copying();
                        let context = self.app.link_context(slot);
                        self.open_link(url, context)?;
                    }
                }
            }
            Action::CopyPaste { slot, text } => {
                if let Some(copy) = self.pane_in(slot).and_then(|pane| pane.copy.as_mut()) {
                    copy.on_paste(&text);
                }
            }
            Action::SelectFrom { slot, cell } => {
                let clicks = self.clicks.click(slot, cell, Instant::now());
                if let Some(pane) = self.pane_in(slot) {
                    pane.select_from(cell, clicks);
                }
            }
            Action::SelectTo { slot, cell } => {
                self.clicks.dragged_to(cell);
                if let Some(pane) = self.pane_in(slot) {
                    pane.select_to(cell);
                }
            }
            Action::CopySelection(slot) => {
                let text = self
                    .pane_in(slot)
                    .and_then(|pane| pane.screen.selected_text());
                if let Some(text) = text {
                    self.copy_to_clipboard(&text)?;
                }
            }
            Action::HoldSelection(slot) => {
                let held = self.pane_in(slot).is_some_and(Pane::hold_selection);
                if held {
                    self.app.hold_selection(slot);
                }
            }
            Action::GrabThumb { slot, row } => {
                if let Some(pane) = self.pane_in(slot) {
                    pane.grab_thumb(row);
                }
            }
            Action::DragThumb { slot, row } => {
                if let Some(pane) = self.pane_in(slot) {
                    pane.drag_thumb(row);
                }
            }
            Action::OpenInBrowser { project, topic } => {
                // The forge's CLI goes over the network: off the loop,
                // saying only what went wrong.
                let events = self.events.clone();
                thread::spawn(move || {
                    let opened = Repo::find(&project).and_then(|repo| repo.open(topic));
                    if let Err(reason) = opened {
                        let _ = events.send(Event::Notice(reason));
                    }
                });
            }
            Action::ListIssues(project) => {
                self.read_in_background(move || {
                    let asked = Instant::now();
                    let found = list_issues(&project);
                    Event::Issues {
                        project,
                        found,
                        asked,
                    }
                });
            }
            Action::ListPullRequests(project) => {
                self.read_in_background(move || {
                    let asked = Instant::now();
                    let found = list_pull_requests(&project);
                    Event::PullRequests {
                        project,
                        found,
                        asked,
                    }
                });
            }
            Action::Find(to_find) => {
                for project in to_find.pull_requests {
                    self.read_in_background(move || {
                        let asked = Instant::now();
                        let found = list_pull_requests(&project);
                        Event::PullRequests {
                            project,
                            found,
                            asked,
                        }
                    });
                }
                for project in to_find.issues {
                    self.read_in_background(move || {
                        let asked = Instant::now();
                        let found = list_issues(&project);
                        Event::Issues {
                            project,
                            found,
                            asked,
                        }
                    });
                }
                self.find_backlogs(to_find.backlogs);
            }
            Action::Comment {
                project,
                topic,
                text,
            } => {
                self.read_in_background(move || {
                    let posted = Repo::find(&project).and_then(|repo| repo.comment(topic, &text));
                    Event::Commented {
                        project,
                        topic,
                        posted,
                    }
                });
            }
            Action::EditIssue {
                project,
                number,
                title,
                body,
            } => {
                self.read_in_background(move || {
                    let saved = Repo::find(&project)
                        .and_then(|repo| repo.edit_issue(number, &title, &body));
                    Event::IssueEdited {
                        project,
                        number,
                        edit: (title, body),
                        saved,
                        at: Instant::now(),
                    }
                });
            }
        }
        Ok(())
    }

    /// Puts `text` on the user's clipboard, and says how much went.
    /// Puts on the clipboard what the program in a pane asked its terminal
    /// to copy (OSC 52), as a terminal of its own would, unless the settings
    /// say not to. A background task's pane shows what crystal draws of
    /// Claude's work rather than a program's own output, and copies
    /// nothing.
    fn copy_for_program(&mut self, session_id: &str, text: &str) {
        if !self.config.clipboard.allow_programs {
            return;
        }
        let session = self.app.sessions().iter().find(|s| s.id == session_id);
        let task = session.and_then(|session| session.task.as_ref());
        if task.is_some_and(|task| task.background) {
            return;
        }
        if let Err(err) = self.copy_to_clipboard(text) {
            self.app.notify(format!("{err:#}"));
        }
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<()> {
        clipboard::copy(text).context("couldn't copy")?;
        let lines = text.lines().count().max(1);
        let noun = if lines == 1 { "line" } else { "lines" };
        self.app.notify(format!("copied {lines} {noun}"));
        Ok(())
    }

    /// Starts `command` in a new session in `cwd`, or the user's shell when
    /// it's empty, called `name` or after its program, then selects the
    /// session and hands it the keyboard.
    fn start_session(
        &mut self,
        name: Option<String>,
        cwd: PathBuf,
        command: Vec<String>,
    ) -> Result<()> {
        let name = client::new_session(&self.socket, name, cwd, command)?;
        self.show_new_session(&name)
    }

    /// Selects the session just started, called `name`, and hands it the
    /// keyboard.
    /// Runs the project's command for `which` in `worktree`. Run starts a
    /// session of its own there, or starts the one that ended again, or
    /// asks to stop the one running; open runs in the background.
    fn project_command(&mut self, which: Verb, worktree: &Worktree) -> Result<()> {
        let config = Config::load()?;
        let commands = Commands::of(&config, &worktree.path, &worktree.project_path)?;
        let line = commands.line(which, &worktree.project)?;
        let place = worktree.branch.as_deref().unwrap_or(&worktree.project);
        if which == Verb::Open {
            project_commands::open(line, &worktree.path)?;
            self.app.notify(format!("opened {place}: {line}"));
            return Ok(());
        }
        let found = self
            .app
            .sessions()
            .iter()
            .find(|session| project_commands::is_run_of(session, &worktree.path, line))
            .cloned();
        let name = match found {
            Some(session) if session.state == State::Running => {
                self.app.select(&session.name);
                self.app.confirm_kill(session.name);
                return Ok(());
            }
            Some(session) => {
                client::respawn(&self.socket, &session.name)?;
                session.name
            }
            None => {
                let base = project_commands::run_name(&worktree.path);
                let name = project_cli::free_name(&base, self.app.sessions());
                let command = project_commands::shell_command(line);
                client::new_session(&self.socket, Some(name), worktree.path.clone(), command)?
            }
        };
        // It runs on its own: the sidebar keeps the keys.
        self.refresh_sessions()?;
        self.app.select(&name);
        Ok(())
    }

    fn show_new_session(&mut self, name: &str) -> Result<()> {
        self.refresh_sessions()?;
        self.app.select(name);
        self.app.type_into_selected();
        Ok(())
    }

    /// After a profile was written to the config file or taken out of it:
    /// reads the file again, so the panel and the profiles view show what
    /// it now says, or has the view say why the file wasn't changed.
    fn profiles_changed(&mut self, changed: Result<()>, select: Option<&str>) {
        match changed.and_then(|()| Config::load()) {
            Ok(config) => self.app.profiles_saved(&config, select),
            Err(error) => self.app.profile_failed(format!("{error:#}")),
        }
    }

    /// After a plugin was switched on or off: everything that shows what
    /// the plugins add follows what the config file now says.
    fn plugins_changed(&mut self, config: &Config) {
        self.config_changed(config);
        let listed = listed_plugins(config, &self.socket, self.selected_project());
        self.app.show_plugins(listed);
    }

    /// The main worktree of the selected session's project, whose plugins
    /// the plugins view lists.
    fn selected_project(&self) -> Option<PathBuf> {
        self.app.selected_context().project
    }

    /// Takes in `config`, when it's not the one the TUI has: the theme, the
    /// new-session panel's settings and everything the plugins add follow
    /// what it says, straight away.
    fn config_changed(&mut self, config: &Config) {
        if *config == self.config {
            return;
        }
        if config.theme != self.config.theme
            || config.colors != self.config.colors
            || config.appearance != self.config.appearance
        {
            self.theme = Theme::from_config(config, self.appearance);
        }
        let follow = config.appearance.auto_switch;
        self.follow_appearance.store(follow, Ordering::Relaxed);
        if config.tab_bar.right != self.config.tab_bar.right {
            // The old threads stop as their watch goes.
            self.status = status_bar::watch(
                &config.tab_bar.right,
                std::env::current_dir().unwrap_or_default(),
                &self.socket,
                self.events.clone(),
            );
        }
        self.app.set_start_dir(start_dir(config));
        crate::vt::set_history_lines(config.scrollback_lines);
        crate::mermaid::set_ascii(config.mermaid_ascii);
        if config.mouse.capture != self.config.mouse.capture {
            capture_mouse(config.mouse.capture);
        }
        self.config = config.clone();
        self.app.set_launch_settings(config);
        self.app.set_features(config);
        self.app.set_interface(config);
        self.app.set_plugin_keys(plugin_keys(config));
        let backlog = crate::backlog::enabled(config);
        self.count_backlog.store(backlog, Ordering::Relaxed);
        let flows = crate::flows::enabled(config);
        self.poll_flows.store(flows, Ordering::Relaxed);
        self.set_sessions(self.app.sessions().to_vec());
    }

    /// Makes the settings view's `edits` to the config file and follows
    /// them, or says in the view why they couldn't be made; and reads the
    /// settings again for it. Whether they were made.
    fn write_settings(&mut self, edits: &[config::Edit]) -> bool {
        let changed = config::apply(&config::path(), edits).and_then(|()| Config::load());
        let written = match changed {
            Ok(config) => {
                self.config_changed(&config);
                true
            }
            Err(err) => {
                self.app.setting_failed(format!("{err:#}"));
                false
            }
        };
        self.read_settings_now();
        written
    }

    /// Reads the settings as they are now, off the loop, for the settings
    /// view.
    fn read_settings_now(&self) {
        let socket = self.socket.clone();
        self.read_in_background(move || Event::Settings(Box::new(read_settings(&socket))));
    }

    /// Fetches the remotes of the worktree at `dir` off the loop, for the
    /// branch switcher, which hears when it's done: `now`, or unless they
    /// were fetched less than [`FETCH_EVERY`] ago, when what that brought
    /// is as good. A fetch still going there answers for this one too.
    fn fetch_remotes(&mut self, dir: PathBuf, now: bool) {
        if self.fetching_remotes.contains(&dir) {
            return;
        }
        let lately = self
            .fetched_remotes
            .get(&dir)
            .is_some_and(|at| at.elapsed() < FETCH_EVERY);
        if lately && !now {
            if let Some(action) = self.app.branches_fetched(&dir, Ok(())) {
                self.carry_out(action);
            }
            return;
        }
        self.fetching_remotes.insert(dir.clone());
        self.read_in_background(move || {
            let fetched = git::branches::fetch(&dir);
            Event::BranchesFetched { dir, fetched }
        });
    }

    /// Asks the daemon to get the model that searches memory by meaning
    /// ready; the settings view follows how that goes.
    fn prepare_embeddings(&mut self) {
        if let Err(err) = client::ask(&self.socket, &Request::PrepareEmbeddings, false) {
            self.app.setting_failed(format!("{err:#}"));
        }
    }

    /// Runs one of a plugin's actions, off the loop, with what it prints in
    /// the plugin's log, and says how it went at the bottom.
    fn run_plugin(&mut self, plugin: &Id, action: &str, context: Context) -> Result<()> {
        let (dir, manifest) = installed_plugin(plugin)?;
        let label = plugin.label();
        let action = manifest
            .action(action)
            .cloned()
            .with_context(|| format!("{label} has no action {action}"))?;
        let context = placed(context)?;
        plugins::log(
            &self.socket,
            plugin,
            &format!("{}: {}", action.id, action.command.join(" ")),
        );
        let log = plugins::open_log(&self.socket, plugin)?;
        let mut child = plugins::command(plugin, &dir, &action.command, &self.socket, &context)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .with_context(|| format!("couldn't run {}", action.command.join(" ")))?;
        let what = format!("{label}: {}", action.title);
        let log = format!("crystal plugin log {}{}", plugin.name, plugin.flag());
        let events = self.events.clone();
        thread::spawn(move || {
            let notice = match child.wait() {
                Ok(status) if status.success() => format!("ran {what}"),
                Ok(status) => format!("{what} failed ({status}): `{log}`"),
                Err(err) => format!("{what}: {err}"),
            };
            let _ = events.send(Event::Notice(notice));
        });
        Ok(())
    }

    /// Runs one of the user's `[[keys.command]]`s in `dir`, about
    /// `context`: a popup's in a session of its own shown over everything,
    /// with the keyboard, until it ends; a pane's or a tab's in a session
    /// of its own, typed into where the app made room for it; and a
    /// shell's in the background.
    fn run_key_command(
        &mut self,
        command: KeyCommand,
        dir: Option<PathBuf>,
        context: Context,
    ) -> Result<()> {
        let context = placed(context)?;
        let dir = match dir.or_else(|| context.worktree.clone()) {
            Some(dir) => dir,
            None => std::env::current_dir()?,
        };
        let vars = key_command_env(&self.socket, &context);
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            command.command.clone(),
        ];
        if command.kind == CommandKind::Shell {
            return self.run_in_background(&command, argv, &dir, vars);
        }
        let taken = list_sessions(&self.socket, false)?;
        let base = names::from_prompt(command.label()).unwrap_or_else(|| "command".to_string());
        let mut environment = env::current();
        for (key, said) in vars {
            match said {
                Some(value) => environment.insert(key.to_string(), value),
                None => environment.remove(key),
            };
        }
        let request = Request::New(NewSession {
            name: Some(free_name(&base, &taken)),
            cwd: dir,
            command: argv,
            env: environment,
            task: None,
            backlog: None,
            brief: Default::default(),
        });
        let Some(Response::Created { name, .. }) = client::ask(&self.socket, &request, true)?
        else {
            bail!("the daemon didn't start {}", command.label());
        };
        if command.kind != CommandKind::Popup {
            return self.show_new_session(&name);
        }
        let popup = Popup {
            width: command.width.clone(),
            height: command.height.clone(),
        };
        self.show_over(PluginPane {
            plugin: String::new(),
            title: command.label().to_string(),
            session: name,
            popup: Some(popup),
        })?;
        self.refresh_sessions()
    }

    /// Shows `pane`'s session over the panes, or in its popup, with the
    /// keyboard.
    fn show_over(&mut self, pane: PluginPane) -> Result<()> {
        let areas = ui::Areas::of(&self.app, self.screen);
        let screen = ui::plugin_pane_screen(&areas, pane.popup.as_ref());
        self.last_pane_id += 1;
        let (id, events) = (self.last_pane_id, self.events.clone());
        let (rows, cols) = (screen.height.max(1), screen.width.max(1));
        let shown = Pane::open(&self.socket, &pane.session, rows, cols, id, events)?;
        self.overlay = Some(shown);
        self.app.plugin_pane_opened(pane);
        Ok(())
    }

    /// Runs `argv`, a `[[keys.command]]`'s, in `dir` with `vars` over the
    /// TUI's environment, on its own: nothing on screen, unless it fails,
    /// when the footer says so with the last line it wrote to its errors.
    fn run_in_background(
        &self,
        command: &KeyCommand,
        argv: Vec<String>,
        dir: &Path,
        vars: Vec<(&'static str, Option<String>)>,
    ) -> Result<()> {
        let mut shell = std::process::Command::new(&argv[0]);
        shell.args(&argv[1..]).current_dir(dir);
        for (key, said) in vars {
            match said {
                Some(value) => shell.env(key, value),
                None => shell.env_remove(key),
            };
        }
        let mut child = shell
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("couldn't run {}", command.command))?;
        let what = command.label().to_string();
        let events = self.events.clone();
        thread::spawn(move || {
            let stderr = child.stderr.take().map(std::io::BufReader::new);
            let lines = stderr.into_iter().flat_map(std::io::BufRead::lines);
            let last = lines
                .map_while(Result::ok)
                .filter(|line| !line.trim().is_empty())
                .last();
            let notice = match (child.wait(), last) {
                (Ok(status), _) if status.success() => return,
                (Ok(status), Some(last)) => format!("{what} failed ({status}): {}", last.trim()),
                (Ok(status), None) => format!("{what} failed ({status})"),
                (Err(err), _) => format!("{what}: {err}"),
            };
            let _ = events.send(Event::Notice(notice));
        });
        Ok(())
    }

    /// Starts one of a plugin's panes in a session of its own, and shows it
    /// where its manifest says: over the panes or in a popup, with the
    /// keyboard, or among them, split off the selected session's pane,
    /// zoomed, or in a tab of its own, the way the layout commands of
    /// `crystal plugin pane open` place it.
    fn open_plugin_pane(&mut self, plugin: &Id, pane: &str, context: Context) -> Result<()> {
        let (dir, manifest) = installed_plugin(plugin)?;
        let label = plugin.label();
        let spec = manifest
            .panes
            .into_iter()
            .find(|candidate| candidate.id == pane)
            .with_context(|| format!("{label} has no pane {pane}"))?;
        let context = placed(context)?;
        plugins::make_state_dir(&self.socket, plugin);
        let mut env = env::current();
        for (key, said) in plugins::env(&self.socket, plugin, &dir, &context) {
            match said {
                Some(value) => env.insert(key.to_string(), value),
                None => env.remove(key),
            };
        }
        let taken = list_sessions(&self.socket, false)?;
        let name = free_name(&format!("{}-{pane}", plugin.name), &taken);
        let request = Request::New(NewSession {
            name: Some(name),
            cwd: dir.clone(),
            command: plugins::argv(&dir, &spec.command),
            env,
            task: None,
            backlog: None,
            brief: Default::default(),
        });
        let Some(Response::Created { name, .. }) = client::ask(&self.socket, &request, true)?
        else {
            bail!("the daemon didn't start {label}'s pane");
        };
        if spec.placement.is_over() {
            let popup = (spec.placement == Placement::Popup).then(|| Popup {
                width: spec.width.clone(),
                height: spec.height.clone(),
            });
            self.show_over(PluginPane {
                plugin: plugin.name.clone(),
                title: spec.title,
                session: name,
                popup,
            })?;
            return self.refresh_sessions();
        }
        self.refresh_sessions()?;
        let order = |command| Order {
            command,
            caller: None,
        };
        let placed = (|| -> Result<(), String> {
            if spec.placement == Placement::Tab {
                let title = Some(spec.title.clone());
                self.app
                    .obey(order(layout::Command::NewTab { name: title }))?;
                let tab = self.app.layout().current().map(|tab| tab.number);
                let tab = tab.unwrap_or(1).to_string();
                let session = name.clone();
                self.app
                    .obey(order(layout::Command::MoveToTab { session, tab }))?;
                return Ok(());
            }
            let way = match spec.split {
                Some(SplitWay::Down) => split_tree::Way::Down,
                Some(SplitWay::Right) | None => split_tree::Way::Right,
            };
            let split = layout::Command::Split {
                session: name.clone(),
                beside: None,
                way,
                ratio: 0.5,
            };
            self.app.obey(order(split))?;
            if spec.placement == Placement::Zoomed {
                let session = Some(name.clone());
                self.app
                    .obey(order(layout::Command::Zoom { session, on: true }))?;
            }
            Ok(())
        })();
        if let Err(why) = placed {
            // It was only ever the pane's.
            let _ = client::ask(&self.socket, &Request::Kill { name }, false);
            bail!("{why}");
        }
        self.app.select(&name);
        self.app.type_into_selected();
        Ok(())
    }

    /// Attaches again to the sessions whose panes' output ended while they
    /// run on, as `sessions` says: a daemon handed over to a new crystal
    /// hangs up on every attach. A plugin's pane closes as its program
    /// ends.
    fn attach_again(&mut self, sessions: &[SessionInfo]) {
        let running = |id: &str| {
            sessions
                .iter()
                .any(|session| session.id == id && session.state == State::Running)
        };
        // Dropped, each attaches again as it's drawn.
        self.panes
            .retain(|pane| !pane.ended || !running(&pane.session_id));
        let Some(overlay) = self.overlay.as_ref().filter(|overlay| overlay.ended) else {
            return;
        };
        let (rows, cols) = overlay.size();
        let name = self.app.plugin_pane().map(|pane| pane.session.clone());
        let Some(name) = name.filter(|_| running(&overlay.session_id)) else {
            self.close_plugin_pane();
            return;
        };
        self.last_pane_id += 1;
        let (id, events) = (self.last_pane_id, self.events.clone());
        match Pane::open(&self.socket, &name, rows, cols, id, events) {
            Ok(pane) => self.overlay = Some(pane),
            Err(_) => self.close_plugin_pane(),
        }
    }

    /// Closes the plugin's pane that's open, and ends its session, which
    /// was only ever the pane's.
    fn close_plugin_pane(&mut self) {
        self.overlay = None;
        let Some(pane) = self.app.plugin_pane().cloned() else {
            return;
        };
        self.app.plugin_pane_closed();
        let kill = Request::Kill { name: pane.session };
        if let Err(err) = client::ask(&self.socket, &kill, false) {
            self.app.notify(format!("{err:#}"));
        }
        let _ = self.refresh_sessions();
    }

    /// Asks the daemon, off the loop, for the backlog of the project `dir`
    /// is in, done items too.
    fn list_backlog(&self, dir: PathBuf) {
        let socket = self.socket.clone();
        self.read_in_background(move || {
            let found =
                client::backlog(&socket, dir.clone(), true).map_err(|err| format!("{err:#}"));
            Event::Backlog { dir, found }
        });
    }

    /// Asks the daemon, off the loop, for the backlogs of the projects
    /// `dirs` are in, one after the other, done items too, as the backlog
    /// view does: for `/` to find their items to do.
    fn find_backlogs(&self, dirs: Vec<PathBuf>) {
        if dirs.is_empty() {
            return;
        }
        let socket = self.socket.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            for dir in dirs {
                let found =
                    client::backlog(&socket, dir.clone(), true).map_err(|err| format!("{err:#}"));
                if events.send(Event::Backlog { dir, found }).is_err() {
                    return;
                }
            }
        });
    }

    /// Searches the worktree at `dir` for `query` on a thread of its own, a
    /// moment after it's asked for, unless another search has been asked for
    /// by then: that one is what's wanted, and this one stops.
    fn search(&self, dir: PathBuf, query: String) {
        let this = self.searches.fetch_add(1, Ordering::Relaxed) + 1;
        let latest = self.searches.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            thread::sleep(GREP_PAUSE);
            let stale = || latest.load(Ordering::Relaxed) != this;
            if stale() {
                return;
            }
            if let Some(found) = grep::search(&dir, &query, &stale) {
                let _ = events.send(Event::Searched { dir, query, found });
            }
        });
    }

    /// Runs `read` on a thread of its own, since git and the disk can keep
    /// it a while, and hands what it read back to the loop as an event.
    fn read_in_background(&self, read: impl FnOnce() -> Event + Send + 'static) {
        let events = self.events.clone();
        thread::spawn(move || {
            let _ = events.send(read());
        });
    }

    /// The daemon is done removing the worktree at `path`. Once it's gone,
    /// it and the sessions that had ended in it leave the sidebar straight
    /// away.
    fn worktree_removed(&mut self, path: &Path, removed: Result<(), String>) {
        if let Err(reason) = removed {
            self.app.worktree_not_removed(path, reason);
            return;
        }
        self.app.worktree_removed(path);
        self.list_worktrees_again();
        if let Err(err) = self.refresh_sessions() {
            self.app.notify(format!("{err:#}"));
        }
    }

    /// Has the user asked whether the worktrees the sessions just killed
    /// left with nothing in them go too, or has them removed, as the
    /// settings say, once the daemon has said which archived sessions ran
    /// in them.
    fn ask_about_emptied(&mut self, emptied: Vec<Worktree>) -> Result<()> {
        if emptied.is_empty() {
            return Ok(());
        }
        let archived = match client::ask(&self.socket, &Request::Archived, false)? {
            Some(Response::Archived { sessions }) => sessions,
            _ => Vec::new(),
        };
        match self.app.ask_about_emptied(emptied, &archived) {
            Some(removal) => self.perform(removal),
            None => Ok(()),
        }
    }

    /// Asks for the list now, rather than waiting for the next poll, so a
    /// key's effect shows straight away.
    fn refresh_sessions(&mut self) -> Result<()> {
        if self.app.shows_flows() {
            self.app.set_flows(list_flows(&self.socket));
        }
        let asked = Instant::now();
        let sessions = list_sessions(&self.socket, false)?;
        self.sessions_asked = asked;
        self.set_sessions(sessions);
        Ok(())
    }

    /// Keeps a viewer on each session a pane shows, at the size it's drawn
    /// at: attaches to a session as it comes on screen, resizes when the
    /// layout changes, and lets go of sessions no pane shows any more.
    fn sync_panes(&mut self, areas: &ui::Areas) {
        let mut before = std::mem::take(&mut self.panes);
        for (slot, area) in self.app.slots().into_iter().zip(&areas.panes) {
            if !self.app.shows_screen(slot) {
                continue;
            }
            let Some(session) = self.app.pane_session(slot) else {
                continue;
            };
            let name = session.name.clone();
            let screen = ui::screen_area(*area, self.app.scrollbars());
            let (rows, cols) = (screen.height.max(1), screen.width.max(1));

            let kept = before.iter().position(|pane| pane.session_id == session.id);
            let pane = match kept {
                Some(index) => Some(before.swap_remove(index)),
                None => self.open_pane(&name, rows, cols),
            };
            if let Some(mut pane) = pane {
                if pane.size() != (rows, cols) {
                    pane.resize(rows, cols);
                }
                // Copy mode is on in a pane while the keyboard is in it.
                pane.set_copying(self.app.focus() == Focus::Copy(slot));
                self.panes.push(pane);
            }
        }
        // What's left in `before` is on no pane now. Dropping a viewer
        // hangs up.
    }

    /// Attaches a new pane to `session`. If that fails, the session has
    /// likely just gone, and the next list will catch up.
    fn open_pane(&mut self, session: &str, rows: u16, cols: u16) -> Option<Pane> {
        self.last_pane_id += 1;
        let id = self.last_pane_id;
        let events = self.events.clone();
        Pane::open(&self.socket, session, rows, cols, id, events).ok()
    }

    /// The viewer of the session the pane at `slot` shows.
    fn pane_in(&mut self, slot: Slot) -> Option<&mut Pane> {
        if !self.app.shows_screen(slot) {
            return None;
        }
        let session = self.app.pane_session(slot)?;
        self.panes
            .iter_mut()
            .find(|pane| pane.session_id == session.id)
    }

    fn pane_with_id(&mut self, id: u64) -> Option<&mut Pane> {
        self.panes
            .iter_mut()
            .chain(self.overlay.as_mut())
            .find(|pane| pane.id == id)
    }
}

/// The plugins as the plugins view lists them: crystal's own, then the
/// installed ones, then those the project whose main worktree is `project`
/// ships, each with whether it's on and what keeps it from running.
fn listed_plugins(
    config: &Config,
    socket: &Path,
    project: Option<PathBuf>,
) -> Vec<plugins_view::Listed> {
    let own = plugins::BUILT_IN.iter().map(|plugin| plugins_view::Listed {
        name: plugin.name.to_string(),
        description: plugin.description.to_string(),
        built_in: true,
        project: None,
        on: plugins::enabled(config, plugin.name),
        trouble: None,
        actions: Vec::new(),
        panes: Vec::new(),
        links: Vec::new(),
    });
    let shipped = project.as_deref().map(plugins::of_project);
    let installed = plugins::installed()
        .into_iter()
        .chain(shipped.into_iter().flatten());
    let installed = installed.map(|plugin| {
        let id = plugin.id();
        let on = plugins::is_on(config, &id);
        let paused = plugins::paused(socket, &id).filter(|_| on);
        let paused = paused.map(|_| "paused after failing: space off and on again".to_string());
        let trouble = plugin.blocked().or(paused);
        let item = |id: &str, title: &str, key: Option<String>| plugins_view::Item {
            id: id.to_string(),
            title: title.to_string(),
            key,
        };
        // A project's plugin takes no keys.
        let keyed = plugin.project.is_none();
        let key = |key: Option<&String>| key.filter(|_| keyed).cloned();
        match plugin.manifest {
            Ok(manifest) => plugins_view::Listed {
                name: plugin.name,
                built_in: false,
                project: plugin.project,
                on,
                trouble,
                actions: (manifest.actions.iter())
                    .map(|action| item(&action.id, &action.title, key(action.key.as_ref())))
                    .collect(),
                panes: (manifest.panes.iter())
                    .map(|pane| item(&pane.id, &pane.title, None))
                    .collect(),
                links: (manifest.link_handlers.iter())
                    .map(|handler| plugins_view::LinkItem {
                        pattern: handler.pattern.clone(),
                        action: manifest
                            .action(&handler.action)
                            .map_or(handler.action.clone(), |action| action.title.clone()),
                    })
                    .collect(),
                description: manifest.description,
            },
            Err(why) => plugins_view::Listed {
                name: plugin.name,
                description: String::new(),
                built_in: false,
                project: plugin.project,
                on,
                trouble: Some(why),
                actions: Vec::new(),
                panes: Vec::new(),
                links: Vec::new(),
            },
        }
    });
    own.chain(installed).collect()
}

/// The actions of the installed plugins that are on and can run here, each
/// with the sidebar key it took, if it took one. Installing or switching on
/// a plugin refuses a key another has, so where two plugins' files were
/// changed to share one, the first by name keeps it.
fn plugin_keys(config: &Config) -> Vec<PluginKey> {
    let mut keys: Vec<PluginKey> = Vec::new();
    let on = plugins::installed()
        .into_iter()
        .filter(|plugin| plugins::enabled(config, &plugin.name) && plugin.blocked().is_none());
    for plugin in on {
        let Ok(manifest) = plugin.manifest else {
            continue;
        };
        for action in manifest.actions {
            let key = action
                .key
                .as_deref()
                .and_then(|key| Sequence::parse(key).ok());
            let free = key.is_some_and(|key| {
                let clashes = |taken: &PluginKey| taken.key.is_some_and(|it| it.clashes(&key));
                !keys.iter().any(clashes)
            });
            keys.push(PluginKey {
                key: key.filter(|_| free),
                plugin: plugin.name.clone(),
                action: action.id,
                title: action.title,
            });
        }
    }
    keys
}

/// The installed plugin called `name`, when it can run here: its
/// directory and manifest.
fn installed_plugin(id: &Id) -> Result<(PathBuf, crate::plugin_manifest::Manifest)> {
    let label = id.label();
    if id.project.is_none() {
        plugins::ensure_enabled(&Config::load()?, &id.name)?;
    } else if !plugins::is_on(&Config::load()?, id) {
        bail!(
            "{label} is off: `crystal plugin enable {} --project` turns it on",
            id.name
        );
    }
    let plugin = plugins::find_id(id).with_context(|| format!("there's no plugin {label}"))?;
    if let Some(why) = plugin.blocked() {
        bail!("{label} can't run: {why}");
    }
    let manifest = plugin
        .manifest
        .map_err(|why| anyhow::anyhow!("{label}'s plugin.toml: {why}"))?;
    Ok((plugin.dir, manifest))
}

/// Refuses to switch on the plugin `id`, saying why, when it can't run
/// here or wants another's key, or is a project's, which the command line
/// turns on once it has shown what it runs. Any can be switched off.
fn can_switch(id: &Id, on: bool) -> Result<()> {
    if !on || plugins::is_built_in(&id.name) {
        return Ok(());
    }
    if id.project.is_some() {
        bail!(
            "`crystal plugin enable {} --project` shows what it runs, then turns it on",
            id.name
        );
    }
    let installed = plugins::installed();
    match installed.iter().find(|plugin| plugin.name == id.name) {
        Some(plugin) => plugins::check_can_enable(plugin, &installed),
        None => bail!("there's no plugin called {}", id.name),
    }
}

/// What a `[[keys.command]]`'s command finds in its environment, over the
/// TUI's own: the crystal running it and its daemon, and where it was run
/// from, as a plugin's action does. A session's `CRYSTAL_SESSION` is its
/// own.
fn key_command_env(socket: &Path, context: &Context) -> Vec<(&'static str, Option<String>)> {
    let crystal = std::env::current_exe().ok();
    let path = |path: PathBuf| Some(path.display().to_string());
    let crystal = [
        ("CRYSTAL_BIN", crystal.and_then(path)),
        ("CRYSTAL_SOCKET", path(socket.to_path_buf())),
    ];
    crystal.into_iter().chain(context.vars()).collect()
}

/// `context`, or, with no session selected to say where, the TUI's own
/// directory.
fn placed(context: Context) -> Result<Context> {
    if context.worktree.is_some() {
        return Ok(context);
    }
    Ok(Context::of_dir(&std::env::current_dir()?))
}

/// Where `config` has a new tab's shell start, and a session started with
/// none selected: `None` to follow the selection.
fn start_dir(config: &Config) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let current = std::env::current_dir().unwrap_or_default();
    config.terminal.start_dir(&home, &current)
}

/// `base`, or `base-2`, `base-3`, …, whichever no session has yet.
fn free_name(base: &str, sessions: &[SessionInfo]) -> String {
    let taken = |name: &str| sessions.iter().any(|session| session.name == name);
    let mut name = base.to_string();
    let mut number = 1;
    while taken(&name) {
        number += 1;
        name = format!("{base}-{number}");
    }
    name
}

/// The directory a new session at `place` starts in, making the worktree
/// first when it's a new one. Where a place says nothing, it's the TUI's
/// own directory.
fn directory_for(socket: &Path, place: Place) -> Result<PathBuf> {
    match place {
        Place::Directory(Some(dir)) => Ok(dir),
        Place::Directory(None) => Ok(std::env::current_dir()?),
        Place::NewWorktree {
            branch,
            base,
            made_up,
        } => {
            let base = match base {
                Some(base) => base,
                None => std::env::current_dir()?,
            };
            if made_up {
                client::add_new_worktree(socket, &base, &branch, None, None)
            } else {
                client::add_worktree(socket, &base, &branch, None, None)
            }
        }
        Place::PullRequest(checkout) => client::pull_request_worktree(socket, &checkout),
    }
}

/// Why the worktree at `path` wasn't removed, short enough for the footer:
/// git names a worktree by its whole path, which would push the reason
/// itself off the end, so it's named by its directory instead.
fn removal_refused(path: &Path, err: &anyhow::Error) -> String {
    let said = format!("{err:#}");
    let said = said.strip_prefix("fatal: ").unwrap_or(&said);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    said.replace(&path.display().to_string(), &name)
}

/// The bytes that hand `text`, pasted, to a program: marked as a paste
/// when the program asked for that. A program that didn't gets a newline
/// the way a terminal sends it for the Enter key.
fn pasted(text: &str, marked: bool) -> Vec<u8> {
    if marked {
        typing::keystrokes(text, true)
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// The memory of the project `dir` is in, for the memory view.
fn read_memory(socket: &Path, dir: &Path) -> Result<Vec<Listed>, String> {
    let project = memory::project_of(dir);
    match Memory::read(socket, &project) {
        Ok(memory) => Ok(memory.listed()),
        Err(err) => Err(format!("{err:#}")),
    }
}

/// Writes entry `id` of the memory of the project `dir` is in into its
/// CLAUDE.md or AGENTS.md, tells the daemon, and returns which.
fn promote_memory(socket: &Path, dir: &Path, id: u64) -> Result<PathBuf> {
    let project = memory::project_of(dir);
    let memory = Memory::read(socket, &project)?;
    let Some(entry) = memory.get(id) else {
        bail!("there's no entry {id}");
    };
    let file = memory::promote(&project, entry)?;
    // The file has it either way.
    let promoted = events::Event::promoted(project, entry.clone(), file.clone());
    let _ = client::tell(socket, promoted);
    Ok(file)
}

/// The models Codex lets the user choose, as `codex debug models` lists
/// them, or none when it can't say.
fn read_codex_models() -> Vec<String> {
    let output = std::process::Command::new("codex")
        .args(["debug", "models"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            catalog::codex_models(&String::from_utf8_lossy(&output.stdout))
        }
        _ => Vec::new(),
    }
}

/// Where the history a session called `name` opens in the editor is
/// written: beside the daemon's state, by the name of that session, which
/// no other running session has, so it never writes over a file an editor
/// still has open.
fn history_path(socket: &Path, name: &str) -> PathBuf {
    let file = format!("{}.txt", name.replace('/', "-"));
    crate::state::path(socket)
        .with_file_name("history")
        .join(file)
}

/// Writes `text` to `path`, making its directory first.
fn write_history(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("couldn't write {}", path.display()))
}

/// The user's editor, as a command line to put a file's path after:
/// `$EDITOR`, which may carry its own arguments, like `code --wait`, or
/// else `vi`.
fn editor() -> Result<Vec<String>> {
    let editor = std::env::var("EDITOR").unwrap_or_default();
    let editor = if editor.trim().is_empty() {
        "vi".to_string()
    } else {
        editor
    };
    let command = command_line::parse(&editor).map_err(|err| anyhow::anyhow!("$EDITOR: {err}"))?;
    if command.is_empty() {
        bail!("$EDITOR is empty");
    }
    Ok(command)
}

/// The command that opens `path` in `editor` at `line`: `+12 path`, the way
/// vi, Emacs, nano and most others take it, but for the editors that take
/// `path:12`, and VS Code and those made from it, which want `--goto` too.
fn editor_at(mut editor: Vec<String>, path: String, line: Option<usize>) -> Vec<String> {
    let Some(line) = line else {
        editor.push(path);
        return editor;
    };
    let program = editor
        .first()
        .and_then(|program| Path::new(program).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match program.as_str() {
        "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => {
            editor.extend(["--goto".to_string(), format!("{path}:{line}")]);
        }
        "hx" | "helix" | "zed" | "subl" => editor.push(format!("{path}:{line}")),
        _ => editor.extend([format!("+{line}"), path]),
    }
    editor
}

/// Brings the TUI's terminal to the front, as far as this machine lets it,
/// off the event loop: what a click on a notification asks for.
fn raise_terminal() {
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let commands = raise_commands(
        var,
        cfg!(target_os = "macos"),
        clipboard::remote(),
        crate::notify::on_path,
    );
    thread::spawn(move || {
        for argv in commands {
            let _ = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    });
}

/// The commands that bring the terminal to the front, by its environment
/// (`var`): tmux's window and pane this runs in, then the terminal's app on
/// macOS, or its window on X11 with `xdotool`. Over ssh, the terminal is
/// on another machine, out of reach but for tmux.
fn raise_commands(
    var: impl Fn(&str) -> Option<String>,
    macos: bool,
    remote: bool,
    installed: impl Fn(&str) -> bool,
) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    if let Some(pane) = var("TMUX_PANE").filter(|_| var("TMUX").is_some()) {
        commands.push(vec![
            "tmux".into(),
            "select-window".into(),
            "-t".into(),
            pane.clone(),
        ]);
        commands.push(vec!["tmux".into(), "select-pane".into(), "-t".into(), pane]);
    }
    if remote {
        return commands;
    }
    if macos {
        // The app the terminal is, which macOS gives every program it starts.
        if let Some(app) = var("__CFBundleIdentifier") {
            commands.push(vec!["open".into(), "-b".into(), app]);
        }
    } else if let Some(window) = var("WINDOWID").filter(|_| installed("xdotool")) {
        commands.push(vec!["xdotool".into(), "windowactivate".into(), window]);
    }
    commands
}

fn seconds_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

fn list_sessions(socket: &Path, start: bool) -> Result<Vec<SessionInfo>> {
    match client::ask(socket, &Request::List, start)? {
        Some(Response::Sessions { sessions }) => Ok(sessions),
        _ => Ok(Vec::new()),
    }
}

/// A page of the event log of the daemon at `socket` that `scope` takes,
/// the newest first: its end, or from before the event numbered `before`;
/// and the latest event the log had before it was read.
fn read_events(
    socket: &Path,
    scope: &Scope,
    before: Option<u64>,
) -> Result<(u64, Vec<events::Event>), String> {
    let read = || -> Result<_> {
        let db = Db::open(socket)?;
        let latest = db.latest_event()?;
        Ok((latest, db.events_before(scope, before, timeline::PAGE)?))
    };
    read().map_err(|err| format!("{err:#}"))
}

/// What the handoff view shows: whether the worktree at `worktree` has
/// notes, and the files the daemon at `socket` kept with task `task`.
fn find_handoff(
    socket: &Path,
    worktree: Option<PathBuf>,
    task: Option<u64>,
) -> handoff_view::Found {
    let notes = worktree.filter(|worktree| {
        let path = handoff::path(worktree);
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
    });
    let kept = match task {
        Some(task) => Db::open(socket)
            .and_then(|db| db.artifacts(task))
            .map_err(|err| format!("couldn't read what its task kept: {err:#}")),
        None => Ok(Vec::new()),
    };
    handoff_view::Found { notes, kept }
}

/// Every flow run, or none when the daemon can't say.
fn list_flows(socket: &Path) -> Vec<FlowRun> {
    match client::ask(socket, &Request::ListFlows, false) {
        Ok(Some(Response::Flows { runs })) => runs,
        _ => Vec::new(),
    }
}

/// Reads keys, the mouse and resizes off the terminal on a thread of its
/// own, since reading blocks.
fn spawn_input_reader(events: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            let event = match event {
                TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => Event::Key(key),
                TerminalEvent::Mouse(mouse) => Event::Mouse(mouse),
                TerminalEvent::Paste(text) => Event::Paste(text),
                TerminalEvent::Resize(..) => Event::Resize,
                TerminalEvent::FocusGained => Event::Focus(true),
                TerminalEvent::FocusLost => Event::Focus(false),
                _ => continue,
            };
            if events.send(event).is_err() {
                return;
            }
        }
    });
}

/// Asks git about the linked worktrees of each project the sessions are
/// in, on a thread of its own: a project as soon as it's seen, every one
/// again each [`WORKTREES_EVERY`], and every one straight away once `now`
/// is set. When git can't say, what was known stays.
fn spawn_worktree_lister(
    projects: Arc<Mutex<Vec<PathBuf>>>,
    now: Arc<AtomicBool>,
    events: Sender<Event>,
) {
    thread::spawn(move || {
        let mut listed: HashMap<PathBuf, Instant> = HashMap::new();
        loop {
            if now.swap(false, Ordering::Relaxed) {
                listed.clear();
            }
            let wanted = projects.lock().unwrap().clone();
            for project in wanted {
                let due = listed
                    .get(&project)
                    .is_none_or(|at| at.elapsed() >= WORKTREES_EVERY);
                if !due {
                    continue;
                }
                listed.insert(project.clone(), Instant::now());
                let Ok(linked) = git::linked_worktrees(&project) else {
                    continue;
                };
                let listed = Event::Worktrees {
                    project,
                    worktrees: linked.worktrees,
                    subjects: linked.subjects,
                    labels: linked.labels,
                };
                if events.send(listed).is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
}

/// Counts what git says of each worktree the sidebar shows, on a thread of
/// its own: one as soon as it's shown, again straight away once it's asked
/// for in `now`, and every [`STATS_EVERY`] meanwhile.
fn spawn_stat_counter(
    worktrees: Arc<Mutex<Vec<PathBuf>>>,
    now: Arc<Mutex<HashSet<PathBuf>>>,
    events: Sender<Event>,
) {
    thread::spawn(move || {
        let mut counted: HashMap<PathBuf, Instant> = HashMap::new();
        loop {
            let wanted = worktrees.lock().unwrap().clone();
            let due_now = std::mem::take(&mut *now.lock().unwrap());
            counted.retain(|path, _| wanted.contains(path));
            for path in wanted {
                let due = due_now.contains(&path)
                    || counted
                        .get(&path)
                        .is_none_or(|at| at.elapsed() >= STATS_EVERY);
                if !due {
                    continue;
                }
                counted.insert(path.clone(), Instant::now());
                let stat = git::stat(&path).ok();
                if events.send(Event::Stat { path, stat }).is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(200));
        }
    });
}

/// Asks the daemon what crystal's processes take, on a thread of its own:
/// every [`RESOURCES_EVERY`], and every [`RESOURCES_OPEN_EVERY`] while
/// `open` says the RAM view is, straight away as it opens.
fn spawn_resource_poller(socket: PathBuf, events: Sender<Event>, open: Arc<AtomicBool>) {
    thread::spawn(move || {
        let request = Request::Resources {
            client: Some(std::process::id()),
        };
        let mut asked: Option<Instant> = None;
        let mut was_open = false;
        loop {
            let is_open = open.load(Ordering::Relaxed);
            let every = match is_open {
                true => RESOURCES_OPEN_EVERY,
                false => RESOURCES_EVERY,
            };
            let due = asked.is_none_or(|at| at.elapsed() >= every) || (is_open && !was_open);
            was_open = is_open;
            if due {
                asked = Some(Instant::now());
                if let Ok(Some(Response::Resources(taken))) = client::ask(&socket, &request, false)
                    && events.send(Event::Resources(taken)).is_err()
                {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
}

/// The pull requests open on the project at `project`, and the forge
/// they're on.
fn list_pull_requests(project: &Path) -> Result<(Forge, Vec<PullRequest>), String> {
    let repo = Repo::find(project)?;
    Ok((repo.forge, repo.pull_requests()?))
}

/// The issues open on the project at `project`, and the forge they're on.
fn list_issues(project: &Path) -> Result<(Forge, Vec<Issue>), String> {
    let repo = Repo::find(project)?;
    Ok((repo.forge, repo.issues()?))
}

/// Asks their forge about the pull requests and the open issues of each
/// project the sessions are in, on a thread of its own, since its CLI can
/// take seconds to answer: a project as soon as it's seen, and every one
/// again each [`PULL_REQUESTS_EVERY`], and [`ISSUES_EVERY`] for its issues.
fn spawn_forge_poller(projects: Arc<Mutex<Vec<PathBuf>>>, events: Sender<Event>) {
    thread::spawn(move || {
        let mut asked: HashMap<PathBuf, Instant> = HashMap::new();
        let mut asked_issues: HashMap<PathBuf, Instant> = HashMap::new();
        let due = |asked: &HashMap<PathBuf, Instant>, project: &PathBuf, every: Duration| {
            asked.get(project).is_none_or(|at| at.elapsed() >= every)
        };
        loop {
            let wanted = projects.lock().unwrap().clone();
            for project in wanted {
                if due(&asked, &project, PULL_REQUESTS_EVERY) {
                    let now = Instant::now();
                    asked.insert(project.clone(), now);
                    let found = list_pull_requests(&project);
                    let project = project.clone();
                    let listed = Event::PullRequests {
                        project,
                        found,
                        asked: now,
                    };
                    if events.send(listed).is_err() {
                        return;
                    }
                }
                if due(&asked_issues, &project, ISSUES_EVERY) {
                    let now = Instant::now();
                    asked_issues.insert(project.clone(), now);
                    let found = list_issues(&project);
                    let listed = Event::Issues {
                        project,
                        found,
                        asked: now,
                    };
                    if events.send(listed).is_err() {
                        return;
                    }
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

/// Asks for the session list every [`POLL_EVERY`]: with `poll_flows`, the
/// flow runs first, which the sidebar groups the sessions under; and with
/// `count_backlog`, how many backlog items each of their projects has to
/// do.
/// What the session poller asks for beyond the sessions, each while its
/// flag is set.
struct Polled {
    backlog_counts: Arc<AtomicBool>,
    flows: Arc<AtomicBool>,
    settings: Arc<AtomicBool>,
}

/// How often the config file is looked at for a change.
const CONFIG_EVERY: Duration = Duration::from_secs(1);

/// Watches the config file, on a thread of its own, and reads it again
/// whenever it changes, so that a change made by hand, or by another
/// crystal, counts at once without starting the TUI again. Stops once the
/// event loop has gone.
fn watch_config(events: Sender<Event>) {
    thread::spawn(move || {
        let path = config::path();
        // What tells a change: when it was written, and how long it is.
        let stamp = || {
            let meta = std::fs::metadata(&path).ok()?;
            Some((meta.modified().ok()?, meta.len()))
        };
        let mut last = stamp();
        loop {
            thread::sleep(CONFIG_EVERY);
            let now = stamp();
            if now == last {
                continue;
            }
            last = now;
            let read = Config::load().map_err(|err| format!("{err:#}"));
            if events.send(Event::ConfigFile(Box::new(read))).is_err() {
                return;
            }
        }
    });
}

fn spawn_session_poller(socket: PathBuf, events: Sender<Event>, polled: Polled) {
    let Polled {
        backlog_counts: count_backlog,
        flows: poll_flows,
        settings: poll_settings,
    } = polled;
    thread::spawn(move || {
        // Why the list couldn't be had, last time, once it has been said.
        let mut said = None;
        let mut polls: u64 = 0;
        loop {
            // The projects change rarely: they're asked for every few polls.
            if polls.is_multiple_of(PROJECTS_EVERY)
                && let Some(projects) = list_projects(&socket)
                && events.send(Event::Projects(projects)).is_err()
            {
                return;
            }
            polls += 1;
            thread::sleep(POLL_EVERY);
            if poll_flows.load(Ordering::Relaxed)
                && events.send(Event::Flows(list_flows(&socket))).is_err()
            {
                return;
            }
            if poll_settings.load(Ordering::Relaxed)
                && events
                    .send(Event::Settings(Box::new(read_settings(&socket))))
                    .is_err()
            {
                return;
            }
            // A daemon that has gone away has no sessions left. One that
            // can't say, say because it's a newer crystal than this TUI,
            // leaves the list as it was, and says why, once.
            let asked = Instant::now();
            let sessions = match list_sessions(&socket, false) {
                Ok(sessions) => sessions,
                Err(err) => {
                    let why = format!("{err:#}");
                    if said.as_ref() != Some(&why)
                        && events.send(Event::Notice(why.clone())).is_err()
                    {
                        return;
                    }
                    said = Some(why);
                    continue;
                }
            };
            said = None;
            let projects = projects_of(&sessions);
            if events.send(Event::Sessions { sessions, asked }).is_err() {
                return;
            }
            if let Some(worktrees) = list_removals(&socket)
                && events.send(Event::Removals(worktrees)).is_err()
            {
                return;
            }
            if let Ok(Some(Response::Spending(spending))) =
                client::ask(&socket, &Request::Spending, false)
                && events.send(Event::Spending(spending)).is_err()
            {
                return;
            }
            if count_backlog.load(Ordering::Relaxed)
                && let Some(counts) = backlog_counts(&socket, projects)
                && events.send(Event::BacklogCounts(counts)).is_err()
            {
                return;
            }
        }
    });
}

/// The worktrees the daemon is removing, whoever asked, or `None` when it
/// can't say.
fn list_removals(socket: &Path) -> Option<Vec<PathBuf>> {
    match client::ask(socket, &Request::Removals, false) {
        Ok(Some(Response::Removals { worktrees })) => Some(worktrees),
        _ => None,
    }
}

/// The projects crystal knows, by their main worktrees, or `None` when the
/// daemon can't say.
fn list_projects(socket: &Path) -> Option<Vec<Worktree>> {
    match client::ask(socket, &Request::Projects, false) {
        Ok(Some(Response::Projects { projects })) => Some(projects),
        _ => None,
    }
}

/// The settings as they are now: the config file, and how the model that
/// searches memory by meaning stands, as the daemon says.
fn read_settings(socket: &Path) -> settings_view::Current {
    let model = match client::ask(socket, &Request::EmbeddingStatus, false) {
        Ok(Some(Response::EmbeddingStatus(status))) => Some(status),
        _ => None,
    };
    let integrations = std::env::current_exe()
        .map(|crystal| crate::integration::here(&crystal))
        .unwrap_or_default();
    settings_view::Current {
        path: config::path(),
        config: Config::load().map_err(|err| format!("{err:#}")),
        model,
        integrations,
    }
}

/// The projects `sessions` are in, by their main worktrees.
fn projects_of(sessions: &[SessionInfo]) -> Vec<PathBuf> {
    let mut projects: Vec<PathBuf> = sessions
        .iter()
        .filter_map(|session| session.worktree.as_ref())
        .map(|worktree| worktree.project_path.clone())
        .collect();
    projects.sort();
    projects.dedup();
    projects
}

/// How many backlog items each of `projects` has to do, or `None` when
/// the daemon can't say.
fn backlog_counts(socket: &Path, projects: Vec<PathBuf>) -> Option<HashMap<PathBuf, usize>> {
    match client::ask(socket, &Request::BacklogCounts { projects }, false) {
        Ok(Some(Response::BacklogCounts { open })) => Some(open.into_iter().collect()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn an_editor_is_told_the_line_the_way_it_takes_it() {
        let at = |editor: &[&str], line| editor_at(words(editor), "src/a.rs".into(), line);
        assert_eq!(at(&["nvim"], Some(12)), ["nvim", "+12", "src/a.rs"]);
        assert_eq!(at(&["/usr/bin/vi"], None), ["/usr/bin/vi", "src/a.rs"]);
        assert_eq!(at(&["hx"], Some(12)), ["hx", "src/a.rs:12"]);
        assert_eq!(
            at(&["code", "--wait"], Some(12)),
            ["code", "--wait", "--goto", "src/a.rs:12"]
        );
    }

    #[test]
    fn a_terminal_is_brought_to_the_front_the_way_its_machine_allows() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                let found = pairs.iter().find(|(key, _)| *key == name);
                found.map(|(_, value)| value.to_string())
            }
        };
        let iterm = env(&[("__CFBundleIdentifier", "com.googlecode.iterm2")]);
        assert_eq!(
            raise_commands(iterm, true, false, |_| true),
            [words(&["open", "-b", "com.googlecode.iterm2"])]
        );
        let x11 = env(&[("WINDOWID", "4194311")]);
        assert_eq!(
            raise_commands(x11, false, false, |_| true),
            [words(&["xdotool", "windowactivate", "4194311"])]
        );
        assert!(raise_commands(x11, false, false, |_| false).is_empty());
        let tmux_over_ssh = env(&[
            ("TMUX", "/tmp/tmux-1/default,1,0"),
            ("TMUX_PANE", "%3"),
            ("__CFBundleIdentifier", "com.apple.Terminal"),
        ]);
        assert_eq!(
            raise_commands(tmux_over_ssh, true, true, |_| true),
            [
                words(&["tmux", "select-window", "-t", "%3"]),
                words(&["tmux", "select-pane", "-t", "%3"]),
            ]
        );
    }
}
