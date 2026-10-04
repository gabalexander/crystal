//! The settings in `~/.config/crystal/config.toml`. The file is optional,
//! and so is every setting in it; a setting left out has its default.
//!
//! A key crystal doesn't know is an error, not something to skip: a
//! setting spelled wrong would otherwise do nothing, without a word.

use crate::flows::Flow;
use crate::plugins;
use crate::profile::Profile;
use crate::tui::keymap::{Binding, Keymap};
use crate::vt;
use anyhow::{Context, Result, bail};
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Tell the user when a session needs them: its agent is asking them
    /// something, or has finished a turn nobody was watching.
    pub notify: bool,
    /// A shell command to run in place of the desktop notification, say to
    /// send it to a phone. It finds what happened in its environment: see
    /// [`crate::notify`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_command: Option<String>,
    /// The agent the new-session panel picks at first, until one has been
    /// started from it: an agent's program, like `codex`, maybe with
    /// arguments for it, like `codex --full-auto`.
    pub new_session: String,
    /// Name a session crystal would name after its program from the first
    /// thing it's asked instead: see [`crate::names::from_prompt`].
    pub name_from_prompt: bool,
    /// After a restart, start an agent that said how to resume it, with
    /// `crystal report`, with that command: see [`crate::report`]. And one
    /// typed into a shell, whose installed hooks named its conversation,
    /// with the command that resumes it: see [`crate::integration`].
    pub resume_reported_agents: bool,
    /// The TUI's colors.
    pub theme: ThemeName,
    /// How many rows that scrolled off a session's screen are kept, for
    /// copy mode, `crystal read --history` and the editor `e` opens: in the
    /// daemon, and again in each pane showing the session. A session keeps
    /// what the settings said as it started; a pane, as it opened.
    pub scrollback_lines: usize,
    /// Colors of the user's own, over the theme's: `[colors]` in the file.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<ColorToken, ColorValue>,
    /// Which plugins are on and off, by name: crystal's own, which are on
    /// unless switched off here, and ones the user installed, which are off
    /// until switched on. See [`crate::plugins`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, bool>,
    /// When to tell: `[notifications]` in the file.
    pub notifications: NotifySettings,
    /// The sounds played when a session needs the user: `[sound]` in the
    /// file. See [`crate::sound`].
    pub sound: SoundSettings,
    /// How the memory plugin learns: `[memory]` in the file.
    pub memory: MemorySettings,
    /// What background tasks may spend: `[tasks]` in the file.
    pub tasks: TaskSettings,
    /// How long the event log keeps what happened: `[events]` in the file.
    pub events: EventSettings,
    /// Where worktrees' handoff notes go: `[handoff]` in the file.
    pub handoff: HandoffSettings,
    /// How new worktrees are made: `[worktrees]` in the file.
    pub worktrees: WorktreeSettings,
    /// What crystal does with sessions left alone: `[sessions]` in the
    /// file.
    pub sessions: SessionSettings,
    /// Whether the TUI says when a newer crystal is out: `[update]` in the
    /// file.
    pub update: UpdateSettings,
    /// Saved ways to start an agent, offered first in the new-session
    /// panel: `[[profile]]` tables in the file. See [`crate::profile`].
    #[serde(rename = "profile", skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<Profile>,
    /// Chains of steps run one after another on one goal: `[[flow]]`
    /// tables in the file. See [`crate::flows`].
    #[serde(rename = "flow", skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<Flow>,
    /// What runs a project and what opens it, for projects that don't say
    /// in their own `.crystal/project.toml`, or to say otherwise:
    /// `[[project]]` tables in the file. See [`crate::project_commands`].
    #[serde(rename = "project", skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectSettings>,
    /// The TUI's keys, by command, and its prefix: `[keys]` in the file.
    /// See [`crate::tui::keymap`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, Binding>,
    /// How the TUI's sidebar is laid out: `[sidebar]` in the file.
    pub sidebar: SidebarSettings,
    /// The shell a new terminal runs, and where the TUI starts one:
    /// `[terminal]` in the file.
    pub terminal: TerminalSettings,
    /// The title the TUI gives the terminal it runs in: `[window]` in the
    /// file. See [`crate::tui::window`].
    pub window: WindowSettings,
    /// Where the TUI's tab bar goes, and what it shows at its right:
    /// `[tab_bar]` in the file.
    pub tab_bar: TabBarSettings,
    /// The theme following the system's light or dark: `[appearance]` in
    /// the file. See [`crate::tui::appearance`].
    pub appearance: AppearanceSettings,
}

/// The shell a new terminal runs, and where the TUI starts one when
/// nothing says where.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
    /// The shell's program, a name on the `PATH` or a path, not a command
    /// line. Empty is `$SHELL`, or else `/bin/sh`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub default_shell: String,
    /// Whether the shell starts as a login shell.
    pub shell_mode: ShellMode,
    /// Where a terminal the TUI starts goes when nothing says where.
    pub new_cwd: NewCwd,
}

/// Whether a new terminal's shell starts as a login shell, which reads the
/// profile that sets the `PATH` up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellMode {
    /// A login shell on a Mac, where a terminal's shell always is one and
    /// the profile is what puts Homebrew and `path_helper`'s directories
    /// on the `PATH`; not elsewhere.
    #[default]
    Auto,
    Login,
    NonLogin,
}

/// Where a terminal the TUI starts goes when nothing says where: a new
/// tab's shell, and the new-session panel's first place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum NewCwd {
    /// The selected session's directory, or the TUI's own with none.
    #[default]
    Follow,
    /// The home directory.
    Home,
    /// The directory the TUI was started in.
    Current,
    /// A directory of the user's, which may start with `~`.
    Path(PathBuf),
}

/// Shells that start as login shells with `-l`. One not listed, like
/// elvish, has no such thing, and starts as it is.
const LOGIN_SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "ksh", "mksh", "oksh", "dash", "ash", "yash", "tcsh", "csh", "nu",
    "xonsh", "pwsh",
];

impl TerminalSettings {
    /// The command a new terminal runs: the shell, with `-l` when it
    /// starts as a login shell.
    pub fn shell(&self) -> Vec<String> {
        let from_env = std::env::var("SHELL").ok();
        self.shell_on(from_env.as_deref(), cfg!(target_os = "macos"))
    }

    /// [`TerminalSettings::shell`], with `$SHELL` as `from_env`, on a Mac
    /// when `mac` is set.
    fn shell_on(&self, from_env: Option<&str>, mac: bool) -> Vec<String> {
        let shell = [Some(self.default_shell.trim()), from_env]
            .into_iter()
            .flatten()
            .find(|shell| !shell.is_empty())
            .unwrap_or("/bin/sh")
            .to_string();
        let login = match self.shell_mode {
            ShellMode::Auto => mac,
            ShellMode::Login => true,
            ShellMode::NonLogin => false,
        };
        let name = Path::new(&shell)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut command = vec![shell];
        if login && LOGIN_SHELLS.contains(&name.as_str()) {
            command.push("-l".to_string());
        }
        command
    }

    /// The directory a terminal the TUI starts goes in when nothing says
    /// where, with `home` the home directory and `current` the TUI's own;
    /// `None` to follow the selection.
    pub fn start_dir(&self, home: &Path, current: &Path) -> Option<PathBuf> {
        match &self.new_cwd {
            NewCwd::Follow => None,
            NewCwd::Home => Some(home.to_path_buf()),
            NewCwd::Current => Some(current.to_path_buf()),
            NewCwd::Path(path) => Some(match path.strip_prefix("~") {
                Ok(rest) => home.join(rest),
                Err(_) => path.clone(),
            }),
        }
    }
}

impl TryFrom<String> for NewCwd {
    type Error = String;

    fn try_from(text: String) -> Result<NewCwd, String> {
        match text.trim() {
            "follow" => Ok(NewCwd::Follow),
            "home" => Ok(NewCwd::Home),
            "current" => Ok(NewCwd::Current),
            path if path.starts_with('/') || path == "~" || path.starts_with("~/") => {
                Ok(NewCwd::Path(PathBuf::from(path)))
            }
            other => Err(format!(
                "new_cwd is `follow`, `home`, `current` or a directory from / or ~, not `{other}`"
            )),
        }
    }
}

impl From<NewCwd> for String {
    fn from(new_cwd: NewCwd) -> String {
        match new_cwd {
            NewCwd::Follow => "follow".to_string(),
            NewCwd::Home => "home".to_string(),
            NewCwd::Current => "current".to_string(),
            NewCwd::Path(path) => path.display().to_string(),
        }
    }
}

/// The title the TUI gives the terminal it runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowSettings {
    /// What the title says, its tokens filled in: see
    /// [`crate::tui::window`]. Empty leaves the terminal's title alone.
    pub title: String,
}

impl Default for WindowSettings {
    fn default() -> WindowSettings {
        WindowSettings {
            title: "crystal · {session}".to_string(),
        }
    }
}

/// Where the TUI's tab bar goes, and what it shows at its right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TabBarSettings {
    /// Above the panes, or below them, over the footer.
    pub position: BarPosition,
    /// Leave the bar out while there's only one tab, for the panes to have
    /// its row.
    pub hide_when_single: bool,
    /// What the bar shows at its right, after the sessions' count, in
    /// order: see [`crate::tui::status_bar`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub right: Vec<StatusEntry>,
    /// What goes between two of them.
    pub separator: String,
}

impl Default for TabBarSettings {
    fn default() -> TabBarSettings {
        TabBarSettings {
            position: BarPosition::Top,
            hide_when_single: false,
            right: Vec::new(),
            separator: " · ".to_string(),
        }
    }
}

/// Where the tab bar goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BarPosition {
    #[default]
    Top,
    Bottom,
}

/// One thing the tab bar shows at its right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum StatusEntry {
    /// This machine's name.
    Hostname {},
    /// The time, as `strftime` writes `format`.
    Clock {
        #[serde(default = "StatusEntry::default_clock")]
        format: String,
    },
    /// Text that says what it says.
    Text { text: String },
    /// The last line a shell command prints, run again `every` while, and
    /// stopped once it has taken `timeout`.
    Command {
        command: String,
        #[serde(default = "StatusEntry::default_every")]
        every: String,
        #[serde(default = "StatusEntry::default_timeout")]
        timeout: String,
    },
}

impl StatusEntry {
    fn default_clock() -> String {
        "%H:%M".to_string()
    }

    fn default_every() -> String {
        "10s".to_string()
    }

    fn default_timeout() -> String {
        "2s".to_string()
    }

    /// For a command, how often it runs and how long it may take.
    pub fn command_times(&self) -> Option<(Duration, Duration)> {
        let StatusEntry::Command { every, timeout, .. } = self else {
            return None;
        };
        let every = duration(every).ok().flatten()?;
        let timeout = duration(timeout).ok().flatten()?;
        Some((every, timeout))
    }

    /// Says what doesn't make sense in it.
    fn check(&self) -> Result<()> {
        match self {
            StatusEntry::Clock { format } if format.trim().is_empty() => {
                bail!("a clock's format is empty: write it like \"%H:%M\"")
            }
            StatusEntry::Command { command, .. } if command.trim().is_empty() => {
                bail!("a command entry runs nothing: give it a command")
            }
            StatusEntry::Command { every, timeout, .. } => {
                for (name, when) in [("every", every), ("timeout", timeout)] {
                    duration(when)
                        .with_context(|| format!("in a command entry's {name}"))?
                        .with_context(|| format!("a command entry's {name} can't be off"))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// The theme following the system's appearance, light or dark.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppearanceSettings {
    /// Switch between `light_theme` and `dark_theme` as the system's
    /// appearance changes, or over ssh, where the system isn't the user's,
    /// the terminal's background says: see [`crate::tui::appearance`].
    pub auto_switch: bool,
    /// The theme while it's light: unless given, `theme` when it's a light
    /// one, else its light side, else crystal's own `light`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light_theme: Option<ThemeName>,
    /// The theme while it's dark, chosen the same way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark_theme: Option<ThemeName>,
    /// Colors of the user's own while it's light, over `[colors]`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub light_colors: BTreeMap<ColorToken, ColorValue>,
    /// Colors of the user's own while it's dark, over `[colors]`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub dark_colors: BTreeMap<ColorToken, ColorValue>,
}

/// One project's commands, which take the place of those in its own
/// `.crystal/project.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSettings {
    /// The project's main worktree, which may start with `~`.
    pub path: PathBuf,
    /// A shell command that runs the project, like `npm run dev`, in a
    /// terminal of its own in the worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// A shell command that opens the worktree, like `code .`, run once
    /// there with its output thrown away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
}

/// When the user is told a session needs them: see [`crate::notify`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifySettings {
    /// How many seconds a session has to go on needing the user before
    /// they're told: a question answered, or a turn looked at, in that
    /// while is never told of. 0 tells at once.
    pub after_secs: u64,
    /// Tell only while no crystal TUI's terminal has the focus: the user
    /// looking at crystal sees what needs them in its sidebar.
    pub unfocused_only: bool,
}

/// How the TUI's sidebar is laid out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarSettings {
    /// How many columns wide it is until it's resized, with the mouse on
    /// its edge or with `{` and `}`; the TUI keeps a width it was resized
    /// to.
    pub width: u16,
    /// Whether it starts folded, until `\` unfolds it.
    pub folded: bool,
    /// What it keeps while it's folded.
    pub fold: Fold,
    /// The sessions that need the user, from every tab, in a group of their
    /// own at its top.
    pub needs_you: bool,
}

/// The narrowest and widest the sidebar can be, unfolded.
pub const SIDEBAR_WIDTHS: std::ops::RangeInclusive<u16> = 16..=80;

impl Default for SidebarSettings {
    fn default() -> SidebarSettings {
        SidebarSettings {
            width: 28,
            folded: false,
            fold: Fold::Marks,
            needs_you: true,
        }
    }
}

/// What a folded sidebar keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fold {
    /// A narrow rail: each session's mark, so what needs the user still
    /// shows.
    Marks,
    /// Nothing: the panes take every column.
    Hidden,
}

/// The sounds played when a session needs the user, at the same moments
/// a notification tells them: see [`crate::sound`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SoundSettings {
    pub enabled: bool,
    /// A sound file of the user's own for an agent that's done with a
    /// turn nobody watched, in place of crystal's: a path relative to the
    /// config file's directory, or `~/` for the home directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<PathBuf>,
    /// The same for an agent that comes to ask the user something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<PathBuf>,
    /// Agents switched on or off by their program's name, like `codex =
    /// false`, for one that plays its own sounds. An agent left out
    /// follows `enabled`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, bool>,
}

impl Default for SoundSettings {
    fn default() -> SoundSettings {
        SoundSettings {
            enabled: true,
            done: None,
            request: None,
            agents: BTreeMap::new(),
        }
    }
}

/// How the memory plugin learns, beyond what it's told.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    /// Once a task has closed, have a model read what it did and keep what
    /// a later session would need to know: see [`crate::distill`].
    pub distill: bool,
    /// The model that does it, as `claude --model` takes it.
    pub distill_model: String,
    /// The most it may spend on one task, in US dollars.
    pub distill_budget_usd: f64,
    /// Search by what entries mean as well as by their words, with models
    /// run on this machine: see [`crate::embed`].
    pub embeddings: bool,
    /// Have the reranker read the best of a search again, putting what
    /// answers it first and leaving out what doesn't.
    pub rerank: bool,
}

impl Default for MemorySettings {
    fn default() -> MemorySettings {
        MemorySettings {
            distill: true,
            distill_model: "claude-haiku-4-5".to_string(),
            distill_budget_usd: 0.25,
            embeddings: true,
            rerank: true,
        }
    }
}

/// What background tasks may spend, in US dollars, by Claude's own count.
/// 0 is no limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskSettings {
    /// The most one task's `claude -p` may spend, as `--max-budget-usd`: a
    /// run that reaches it fails.
    pub max_budget_usd: f64,
    /// The most every background task together may spend in a day: past
    /// it, no new run starts until tomorrow.
    pub daily_budget_usd: f64,
}

impl Default for TaskSettings {
    fn default() -> TaskSettings {
        TaskSettings {
            max_budget_usd: 5.0,
            daily_budget_usd: 0.0,
        }
    }
}

/// How long the event log keeps what happened: see [`crate::event_log`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventSettings {
    /// How many days an event is kept; 0 keeps every one.
    pub keep_days: u32,
}

impl Default for EventSettings {
    fn default() -> EventSettings {
        EventSettings { keep_days: 30 }
    }
}

/// Where the handoff notes go: see [`crate::handoff`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HandoffSettings {
    /// The projects, by their main worktree's directory, whose notes are
    /// kept in git with their branches, rather than kept out of it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub in_git: Vec<PathBuf>,
}

/// How new worktrees are made: see [`crate::git::add_worktree`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorktreeSettings {
    /// The branch a new worktree's new branch starts from, as `origin` has
    /// it when it has one, in place of `origin`'s default branch. A project
    /// without a branch of that name starts from the default all the same.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// Whether the TUI says when a newer crystal is out: see [`crate::update`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateSettings {
    /// Once a day, as the TUI opens, look for a newer release.
    pub check: bool,
}

impl Default for UpdateSettings {
    fn default() -> UpdateSettings {
        UpdateSettings { check: true }
    }
}

/// What crystal does with sessions left alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionSettings {
    /// How long an agent may sit idle at its prompt, with nobody watching
    /// or typing, before crystal stops it, to start again in its
    /// conversation when it's wanted: like `30m`, `2h` or `90s`, or `off`.
    pub stop_idle_after: String,
}

impl Default for SessionSettings {
    fn default() -> SessionSettings {
        SessionSettings {
            stop_idle_after: "off".to_string(),
        }
    }
}

impl SessionSettings {
    /// The choices the settings view goes round.
    pub const CHOICES: [&str; 6] = ["off", "15m", "30m", "1h", "2h", "8h"];

    /// How long an agent may sit idle, or `None` when it may for good.
    pub fn idle_limit(&self) -> Option<Duration> {
        duration(&self.stop_idle_after).ok().flatten()
    }
}

/// A while, as the settings write it: a number, then `s`, `m` or `h`; or
/// `off`, or `0`, for none.
pub fn duration(text: &str) -> Result<Option<Duration>> {
    let text = text.trim();
    if text == "off" || text == "0" {
        return Ok(None);
    }
    let bad = || format!("`{text}` isn't a while: write it like `30m`, `2h` or `90s`, or `off`");
    let split = text.len().checked_sub(1).with_context(bad)?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number.trim().parse().ok().with_context(bad)?;
    let seconds = match unit {
        "s" => number,
        "m" => number * 60,
        "h" => number * 3600,
        _ => bail!(bad()),
    };
    Ok((seconds > 0).then(|| Duration::from_secs(seconds)))
}

/// One of the TUI's themes, by the name the config file gives it: crystal's
/// own `dark`, `light` and `terminal`, or a well-known scheme like
/// `catppuccin`. The themes, and the other names each is known by, are
/// [`crate::tui::theme::THEMES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(into = "String")]
pub struct ThemeName(&'static str);

/// What a color in `[colors]` paints: one of the theme's own names for
/// what its colors are for. See [`crate::tui::theme::Theme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorToken {
    Background,
    Text,
    Muted,
    Accent,
    Rule,
    Branch,
    Selection,
    Panel,
    Waiting,
    Working,
    Done,
    Running,
    Ended,
    Failed,
    Added,
    Removed,
    AddedLine,
    RemovedLine,
    AddedWords,
    RemovedWords,
    CopySelection,
    Found,
    FoundCurrent,
    Keyword,
    String,
    Number,
    CodeBlock,
}

/// A color as the config file says it: `"#rrggbb"`, `"#rgb"`, one of the
/// terminal's sixteen by name (`"red"`, `"bright-blue"`), a number from
/// its 256, or `"reset"` for the terminal's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ColorValue(pub Color);

/// The terminal's sixteen colors, by the names `[colors]` takes.
const NAMED_COLORS: [(&str, Color); 16] = [
    ("black", Color::Black),
    ("red", Color::Red),
    ("green", Color::Green),
    ("yellow", Color::Yellow),
    ("blue", Color::Blue),
    ("magenta", Color::Magenta),
    ("cyan", Color::Cyan),
    ("white", Color::Gray),
    ("bright-black", Color::DarkGray),
    ("bright-red", Color::LightRed),
    ("bright-green", Color::LightGreen),
    ("bright-yellow", Color::LightYellow),
    ("bright-blue", Color::LightBlue),
    ("bright-magenta", Color::LightMagenta),
    ("bright-cyan", Color::LightCyan),
    ("bright-white", Color::White),
];

impl Default for Config {
    fn default() -> Config {
        Config {
            notify: true,
            notify_command: None,
            new_session: "claude".to_string(),
            name_from_prompt: true,
            resume_reported_agents: true,
            theme: ThemeName::DARK,
            colors: BTreeMap::new(),
            scrollback_lines: vt::DEFAULT_HISTORY_LINES,
            plugins: BTreeMap::new(),
            notifications: NotifySettings::default(),
            sound: SoundSettings::default(),
            memory: MemorySettings::default(),
            tasks: TaskSettings::default(),
            events: EventSettings::default(),
            handoff: HandoffSettings::default(),
            worktrees: WorktreeSettings::default(),
            sessions: SessionSettings::default(),
            update: UpdateSettings::default(),
            profiles: Vec::new(),
            flows: Vec::new(),
            projects: Vec::new(),
            keys: BTreeMap::new(),
            sidebar: SidebarSettings::default(),
            terminal: TerminalSettings::default(),
            window: WindowSettings::default(),
            tab_bar: TabBarSettings::default(),
            appearance: AppearanceSettings::default(),
        }
    }
}

impl Config {
    /// The settings in the config file, or the defaults when there's no
    /// file. A file that can't be read, or doesn't make sense, is an error
    /// that names it.
    pub fn load() -> Result<Config> {
        let path = path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("couldn't read {}", path.display()));
            }
        };
        let config = from_text(&text).with_context(|| format!("in {}", path.display()))?;
        check_plugins(&config, &plugins::installed_names())
            .with_context(|| format!("in {}", path.display()))?;
        Ok(config)
    }

    /// The settings written as TOML, the way the file would hold them.
    pub fn to_toml(&self) -> String {
        // Our own plain struct always makes valid TOML.
        toml::to_string(self).expect("settings make TOML")
    }
}

/// Sets the setting at `keys`, like `["memory", "embeddings"]`, to `value`
/// in the config file at `path`, making the table it's in if there isn't
/// one, and keeping the rest of the file as the user wrote it, comments and
/// all: what the settings view writes. A file the change would leave
/// meaning nothing crystal knows is left as it was, with an error.
pub fn set(path: &Path, keys: &[&str], value: toml_edit::Value) -> Result<()> {
    let (last, tables) = keys.split_last().context("say which setting")?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("couldn't read {}", path.display())),
    };
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("couldn't read {}", path.display()))?;
    let mut table = document.as_table_mut();
    for key in tables {
        table = table
            .entry(key)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .with_context(|| format!("`{key}` in the config file isn't a [{key}] table"))?;
    }
    // A line that's there already keeps its comment.
    match table.get_mut(last).and_then(toml_edit::Item::as_value_mut) {
        Some(said) => {
            let decor = said.decor().clone();
            *said = value;
            *said.decor_mut() = decor;
        }
        None => {
            table.insert(last, toml_edit::Item::Value(value));
        }
    }
    let new_text = document.to_string();
    from_text(&new_text).with_context(|| format!("in {}", path.display()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let unfinished = path.with_extension("toml.saving");
    std::fs::write(&unfinished, &new_text)?;
    std::fs::rename(&unfinished, path)?;
    Ok(())
}

impl ThemeName {
    pub const DARK: ThemeName = ThemeName("dark");
    pub const LIGHT: ThemeName = ThemeName("light");
    pub const TERMINAL: ThemeName = ThemeName("terminal");

    /// The theme `text` names, by its name or another it's known by, in
    /// any case and with spaces or underscores for dashes:
    /// `"Tokyo Night"` is `tokyo-night`.
    pub fn find(text: &str) -> Option<ThemeName> {
        let text = text.trim().to_lowercase().replace([' ', '_'], "-");
        crate::tui::theme::THEMES
            .iter()
            .find(|theme| theme.name == text || theme.aliases.contains(&text.as_str()))
            .map(|theme| ThemeName(theme.name))
    }

    /// Every theme, in the order the settings view goes through them.
    pub fn all() -> impl Iterator<Item = ThemeName> {
        crate::tui::theme::THEMES
            .iter()
            .map(|theme| ThemeName(theme.name))
    }

    /// Its name, as the config file has it.
    pub fn name(self) -> &'static str {
        self.0
    }

    /// Where it is among [`ThemeName::all`], counted from 0.
    pub fn position(self) -> usize {
        ThemeName::all()
            .position(|theme| theme == self)
            .unwrap_or(0)
    }

    /// The one after it, back to the first after the last.
    pub fn next(self) -> ThemeName {
        self.step(1)
    }

    /// The one before it, back to the last before the first.
    pub fn previous(self) -> ThemeName {
        self.step(-1)
    }

    fn step(self, by: isize) -> ThemeName {
        let themes: Vec<ThemeName> = ThemeName::all().collect();
        let at = self.position() as isize + by;
        themes[at.rem_euclid(themes.len() as isize) as usize]
    }
}

impl TryFrom<String> for ThemeName {
    type Error = String;

    fn try_from(text: String) -> Result<ThemeName, String> {
        ThemeName::find(&text).ok_or_else(|| {
            let names: Vec<&str> = ThemeName::all().map(ThemeName::name).collect();
            format!("`{text}` isn't a theme: say one of {}", names.join(", "))
        })
    }
}

impl From<ThemeName> for String {
    fn from(theme: ThemeName) -> String {
        theme.name().to_string()
    }
}

// By hand, since serde's derive would take the `&'static str` for one
// borrowed from the file.
impl<'de> Deserialize<'de> for ThemeName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<ThemeName, D::Error> {
        let text = String::deserialize(deserializer)?;
        ThemeName::try_from(text).map_err(serde::de::Error::custom)
    }
}

impl ColorValue {
    /// The color `text` says, or `None` when it says none crystal knows.
    pub fn parse(text: &str) -> Option<Color> {
        let text = text.trim().to_lowercase().replace([' ', '_'], "-");
        if let Some(hex) = text.strip_prefix('#') {
            return hex_color(hex);
        }
        if let Ok(index) = text.parse::<u8>() {
            return Some(Color::Indexed(index));
        }
        if matches!(text.as_str(), "reset" | "none") {
            return Some(Color::Reset);
        }
        NAMED_COLORS
            .iter()
            .find(|(name, _)| *name == text)
            .map(|(_, color)| *color)
    }
}

/// `rrggbb` or `rgb` as a color, each digit of `rgb` doubled, as CSS reads
/// it.
fn hex_color(hex: &str) -> Option<Color> {
    // `from_str_radix` would take a `+` too.
    if !hex.chars().all(|digit| digit.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |digits: &str| u8::from_str_radix(digits, 16).ok();
    match hex.len() {
        6 => Some(Color::Rgb(
            channel(&hex[0..2])?,
            channel(&hex[2..4])?,
            channel(&hex[4..6])?,
        )),
        3 => {
            let doubled = |at: usize| channel(&hex[at..=at]).map(|digit| digit * 17);
            Some(Color::Rgb(doubled(0)?, doubled(1)?, doubled(2)?))
        }
        _ => None,
    }
}

impl TryFrom<String> for ColorValue {
    type Error = String;

    fn try_from(text: String) -> Result<ColorValue, String> {
        ColorValue::parse(&text).map(ColorValue).ok_or_else(|| {
            let names = NAMED_COLORS.map(|(name, _)| name).join(", ");
            format!(
                "`{text}` isn't a color: say \"#rrggbb\", \"#rgb\", a number up to 255, \
                 \"reset\", or one of {names}"
            )
        })
    }
}

impl From<ColorValue> for String {
    fn from(ColorValue(color): ColorValue) -> String {
        if let Some((name, _)) = NAMED_COLORS.iter().find(|(_, named)| *named == color) {
            return (*name).to_string();
        }
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Indexed(index) => index.to_string(),
            _ => "reset".to_string(),
        }
    }
}

/// `$XDG_CONFIG_HOME/crystal/config.toml`, or else
/// `~/.config/crystal/config.toml`.
pub fn path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config")
        }
    };
    base.join("crystal").join("config.toml")
}

/// Checks that every name in `[plugins]` is a plugin: one of crystal's
/// own, or one of those `installed`. A name spelled wrong would otherwise
/// switch nothing, without a word.
pub fn check_plugins(config: &Config, installed: &[String]) -> Result<()> {
    for name in config.plugins.keys() {
        let known = plugins::is_built_in(name) || installed.iter().any(|plugin| plugin == name);
        if !known {
            let built_in: Vec<&str> = plugins::BUILT_IN.iter().map(|plugin| plugin.name).collect();
            bail!(
                "`[plugins]` names `{name}`, which is neither one of crystal's plugins ({}) \
                 nor one installed in {}",
                built_in.join(", "),
                plugins::plugins_dir().display()
            );
        }
    }
    Ok(())
}

/// The settings `text`, a config file's contents, holds; or an error that
/// says what doesn't make sense in it.
pub fn from_text(text: &str) -> Result<Config> {
    let table: toml::Table = toml::from_str(text)?;
    // Profiles were called presets for a while; say so rather than only
    // that `preset` is a key crystal doesn't know.
    if table.contains_key("preset") {
        bail!("`[[preset]]` tables are now `[[profile]]`: rename them in the file");
    }
    // Memory became a plugin; say where its switch went. `[memory]` is
    // how it learns.
    if matches!(table.get("memory"), Some(toml::Value::Boolean(_))) {
        bail!("`memory` is now a plugin: put `memory = …` under a `[plugins]` line instead");
    }
    let config: Config = table.try_into()?;
    if config.scrollback_lines > vt::MAX_HISTORY_LINES {
        bail!(
            "scrollback_lines is at most {}, not {}",
            vt::MAX_HISTORY_LINES,
            config.scrollback_lines
        );
    }
    duration(&config.sessions.stop_idle_after).context("in [sessions], stop_idle_after")?;
    Keymap::new(&config.keys).map_err(anyhow::Error::msg)?;
    if !SIDEBAR_WIDTHS.contains(&config.sidebar.width) {
        bail!(
            "[sidebar] width is {}: it's from {} to {} columns",
            config.sidebar.width,
            SIDEBAR_WIDTHS.start(),
            SIDEBAR_WIDTHS.end()
        );
    }
    for entry in &config.tab_bar.right {
        entry.check().context("in [tab_bar] right")?;
    }
    crate::tui::window::check(&config.window.title)
        .map_err(|err| anyhow::anyhow!("in [window] title, {err}"))?;
    for profile in &config.profiles {
        profile.check()?;
    }
    let mut names: Vec<&str> = config.profiles.iter().map(|p| p.name.as_str()).collect();
    names.sort_unstable();
    if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
        bail!("two profiles are called {}", pair[0]);
    }
    for (index, flow) in config.flows.iter().enumerate() {
        flow.check(&config.profiles)?;
        if config.flows[..index]
            .iter()
            .any(|before| before.name == flow.name)
        {
            bail!("two flows are called {}", flow.name);
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::{Placement, Step};
    use crate::profile::StartIn;

    fn parse(text: &str) -> Result<Config> {
        from_text(text)
    }

    #[test]
    fn how_long_an_agent_may_sit_idle_is_read_as_a_while() {
        assert_eq!(Config::default().sessions.idle_limit(), None);
        let config = parse("[sessions]\nstop_idle_after = \"30m\"\n").unwrap();
        assert_eq!(
            config.sessions.idle_limit(),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(duration("2h").unwrap(), Some(Duration::from_secs(7200)));
        assert_eq!(duration("90s").unwrap(), Some(Duration::from_secs(90)));
        assert_eq!(duration("0").unwrap(), None);
        for choice in SessionSettings::CHOICES {
            duration(choice).unwrap();
        }
        let err = parse("[sessions]\nstop_idle_after = \"soon\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("isn't a while"), "{err:#}");
        assert!(duration("m").is_err());
        assert!(duration("").is_err());
    }

    #[test]
    fn an_empty_file_is_all_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn a_setting_given_replaces_its_default_only() {
        let config = parse("new_session = \"codex\"\n").unwrap();
        assert_eq!(config.new_session, "codex");
        assert!(config.notify);
        assert_eq!(config.notify_command, None);
    }

    #[test]
    fn a_key_spelled_wrong_is_an_error_that_names_it() {
        let err = parse("notfy = false\n").unwrap_err();
        assert!(format!("{err:#}").contains("notfy"), "{err:#}");
    }

    #[test]
    fn a_setting_of_the_wrong_kind_is_an_error() {
        assert!(parse("notify = \"yes\"\n").is_err());
    }

    #[test]
    fn scrollback_has_a_limit() {
        assert_eq!(
            parse("scrollback_lines = 50000\n")
                .unwrap()
                .scrollback_lines,
            50_000
        );
        let err = parse("scrollback_lines = 5000000\n").unwrap_err();
        assert!(format!("{err:#}").contains("at most"), "{err:#}");
    }

    #[test]
    fn a_theme_is_chosen_by_name() {
        assert_eq!(
            parse("theme = \"light\"\n").unwrap().theme,
            ThemeName::LIGHT
        );
        assert_eq!(Config::default().theme, ThemeName::DARK);
        let config = parse("theme = \"rose-pine-dawn\"\n").unwrap();
        assert_eq!(config.theme.name(), "rose-pine-dawn");
    }

    #[test]
    fn a_theme_answers_to_its_other_names_in_any_case() {
        let named = |text: &str| ThemeName::find(text).map(ThemeName::name);
        assert_eq!(named("Tokyo Night"), Some("tokyo-night"));
        assert_eq!(named("tokyonight"), Some("tokyo-night"));
        assert_eq!(named("catppuccin_mocha"), Some("catppuccin"));
        assert_eq!(named("latte"), Some("catppuccin-latte"));
        assert_eq!(named("dawn"), Some("rose-pine-dawn"));
        assert_eq!(named(" GRUVBOX-DARK "), Some("gruvbox"));
        // crystal's own light theme keeps its name: herdr's `light` alias
        // for catppuccin-latte isn't taken.
        assert_eq!(named("light"), Some("light"));
        assert_eq!(named("neon"), None);
        // Written back, a theme has its own name.
        let config = parse("theme = \"Solarized Dark\"\n").unwrap();
        assert!(config.to_toml().contains("theme = \"solarized\""));
    }

    #[test]
    fn a_theme_crystal_doesnt_have_is_an_error_that_names_it_and_the_themes() {
        let err = format!("{:#}", parse("theme = \"neon\"\n").unwrap_err());
        assert!(err.contains("neon"), "{err}");
        assert!(err.contains("dark, light, terminal, catppuccin"), "{err}");
    }

    #[test]
    fn colors_of_ones_own_are_read_by_what_they_paint() {
        let config = parse(
            "[colors]\naccent = \"#F5C2E7\"\nbackground = \"reset\"\nwaiting = \"bright-red\"\n\
             rule = \"#abc\"\nselection = \"238\"\ncode_block = \"none\"\n",
        )
        .unwrap();
        let color = |token| config.colors.get(&token).map(|value| value.0);
        assert_eq!(color(ColorToken::Accent), Some(Color::Rgb(245, 194, 231)));
        assert_eq!(color(ColorToken::Background), Some(Color::Reset));
        assert_eq!(color(ColorToken::Waiting), Some(Color::LightRed));
        assert_eq!(color(ColorToken::Rule), Some(Color::Rgb(170, 187, 204)));
        assert_eq!(color(ColorToken::Selection), Some(Color::Indexed(238)));
        assert_eq!(color(ColorToken::CodeBlock), Some(Color::Reset));
        assert_eq!(color(ColorToken::Text), None);
    }

    #[test]
    fn a_color_that_isnt_one_or_paints_nothing_is_an_error_that_names_it() {
        let cases = [
            ("accent = \"#ggg\"", "`#ggg` isn't a color"),
            ("accent = \"#+f+f+f\"", "`#+f+f+f` isn't a color"),
            ("accent = \"#12345\"", "`#12345` isn't a color"),
            ("accent = \"256\"", "`256` isn't a color"),
            ("accent = \"pink\"", "bright-blue"),
            ("acent = \"red\"", "acent"),
            ("accent = 3", "accent"),
        ];
        for (line, expected) in cases {
            let err = parse(&format!("[colors]\n{line}\n")).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{line}: {err:#}");
        }
    }

    #[test]
    fn colors_written_out_read_back_the_same() {
        for text in ["#0a0b0c", "red", "bright-white", "white", "17", "reset"] {
            let value = ColorValue::try_from(text.to_string()).unwrap();
            assert_eq!(String::from(value), text);
        }
        assert_eq!(ColorValue::parse("Bright Blue"), Some(Color::LightBlue));
    }

    #[test]
    fn profiles_are_read_in_order() {
        let config = parse(
            r#"
[[profile]]
name = "review"
description = "A second pair of eyes"
agent = "claude"
mode = "plan"
prompt = "Review the diff on this branch."
instructions = "Point out risks before style."
where = "here"

[[profile]]
name = "fast"
agent = "codex"
model = "gpt-6-luna"
args = ["--search"]
where = "worktree"
"#,
        )
        .unwrap();
        let names: Vec<&str> = config.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["review", "fast"]);
        assert_eq!(config.profiles[0].mode.as_deref(), Some("plan"));
        assert_eq!(config.profiles[0].start_in, Some(StartIn::Here));
        assert_eq!(config.profiles[1].args, ["--search"]);
        assert_eq!(config.profiles[1].start_in, Some(StartIn::Worktree));
    }

    #[test]
    fn a_profile_that_cant_start_is_an_error_that_says_why() {
        let cases = [
            (
                "name = \"x\"\nagent = \"vim\"",
                "doesn't know the agent vim",
            ),
            (
                "name = \"x\"\nagent = \"claude\"\nmode = \"yolo\"",
                "isn't a mode of Claude Code",
            ),
            (
                "name = \"x\"\nagent = \"claude\"\nflavor = \"mint\"",
                "flavor",
            ),
            ("name = \"x\"\nagent = \"claude\"\nwhere = \"moon\"", "moon"),
            (
                "name = \"x\"\nagent = \"claude\"\n[[profile]]\nname = \"x\"\nagent = \"codex\"",
                "two profiles are called x",
            ),
        ];
        for (profile, expected) in cases {
            let err = parse(&format!("[[profile]]\n{profile}\n")).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
    }

    #[test]
    fn plugins_are_switched_by_name() {
        let config = parse("[plugins]\nmemory = false\ngithub = true\n").unwrap();
        assert_eq!(config.plugins.get("memory"), Some(&false));
        assert_eq!(config.plugins.get("github"), Some(&true));
    }

    #[test]
    fn a_plugin_that_doesnt_exist_is_an_error_that_names_it() {
        let config = parse("[plugins]\nmemroy = false\n").unwrap();
        let err = check_plugins(&config, &[]).unwrap_err();
        assert!(format!("{err:#}").contains("memroy"), "{err:#}");
        assert!(check_plugins(&config, &["memroy".to_string()]).is_ok());
    }

    #[test]
    fn a_leftover_memory_setting_says_where_it_went() {
        let err = parse("memory = false\n").unwrap_err();
        assert!(format!("{err:#}").contains("`[plugins]`"), "{err:#}");
    }

    #[test]
    fn flows_and_their_steps_are_read_in_order() {
        let config = parse(
            r#"
[[profile]]
name = "reviewer"
agent = "claude"

[[flow]]
name = "ship"
description = "Plan, build, review"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"

[[flow.step]]
name = "build"
prompt = "Build {goal} like so: {previous}"
worktree = true

[[flow.step]]
name = "review"
profile = "reviewer"
prompt = "Review it. {feedback}"
gate = true
back_to = "build"
"#,
        )
        .unwrap();
        let ship = &config.flows[0];
        assert_eq!(ship.chain(), "plan → build → review");
        assert!(ship.steps[1].worktree && !ship.steps[1].gate);
        assert_eq!(ship.steps[2].profile.as_deref(), Some("reviewer"));
        assert_eq!(ship.steps[2].back_to.as_deref(), Some("build"));
    }

    #[test]
    fn a_flow_that_cant_run_is_an_error_that_says_why() {
        let cases = [
            ("[[flow]]\nname = \"ship\"\n", "has no steps"),
            (
                "[[flow]]\nname = \"ship\"\n[[flow.step]]\nname = \"plan\"\nprompt = \"x\"\nwait = true\n",
                "wait",
            ),
            (
                "[[flow]]\nname = \"ship\"\n[[flow.step]]\nname = \"plan\"\nprompt = \"x\"\nprofile = \"nope\"\n",
                "profile nope, which isn't there",
            ),
            (
                "[[flow]]\nname = \"x\"\n[[flow.step]]\nname = \"a\"\nprompt = \"x\"\n\
                 [[flow]]\nname = \"x\"\n[[flow.step]]\nname = \"a\"\nprompt = \"x\"\n",
                "two flows are called x",
            ),
        ];
        for (flow, expected) in cases {
            let err = parse(flow).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
    }

    #[test]
    fn a_setting_set_keeps_the_rest_of_the_file_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# mine\nnotify = true # loud\n\n[plugins]\nmemory = true\n",
        )
        .unwrap();
        set(&path, &["notify"], false.into()).unwrap();
        set(&path, &["memory", "embeddings"], false.into()).unwrap();
        set(&path, &["theme"], "light".into()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine\nnotify = false # loud\n"),
            "{text}"
        );
        assert!(text.contains("[memory]\nembeddings = false\n"), "{text}");
        let config = from_text(&text).unwrap();
        assert!(!config.notify && !config.memory.embeddings);
        assert_eq!(config.theme, ThemeName::LIGHT);

        // What crystal wouldn't take is never written.
        assert!(set(&path, &["theme"], "pink".into()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn the_themes_go_round_both_ways() {
        assert_eq!(ThemeName::DARK.next(), ThemeName::LIGHT);
        assert_eq!(ThemeName::LIGHT.previous(), ThemeName::DARK);
        assert_eq!(ThemeName::LIGHT.name(), "light");
        let last = ThemeName::all().last().unwrap();
        assert_eq!(last.next(), ThemeName::DARK);
        assert_eq!(ThemeName::DARK.previous(), last);
        let mut theme = ThemeName::DARK;
        let mut seen = Vec::new();
        loop {
            seen.push(theme.name());
            theme = theme.next();
            if theme == ThemeName::DARK {
                break;
            }
        }
        assert_eq!(seen.len(), ThemeName::all().count());
        assert_eq!(seen.len(), 20);
    }

    #[test]
    fn memory_learns_by_its_own_table() {
        let config = parse("[memory]\ndistill = false\n").unwrap();
        assert!(!config.memory.distill);
        assert_eq!(config.memory.distill_model, "claude-haiku-4-5");
        assert_eq!(config.memory.distill_budget_usd, 0.25);
        assert!(config.memory.embeddings && config.memory.rerank);
        let config = parse("[memory]\ndistill_model = \"sonnet\"\ndistill_budget_usd = 1\n");
        let config = config.unwrap();
        assert_eq!(config.memory.distill_model, "sonnet");
        assert_eq!(config.memory.distill_budget_usd, 1.0);
        assert!(parse("[memory]\ndistil = false\n").is_err());
    }

    #[test]
    fn background_tasks_have_a_budget_each_and_none_a_day_unless_given() {
        let config = parse("").unwrap();
        assert_eq!(config.tasks.max_budget_usd, 5.0);
        assert_eq!(config.tasks.daily_budget_usd, 0.0);
        let config = parse("[tasks]\ndaily_budget_usd = 10\n").unwrap();
        assert_eq!(config.tasks.daily_budget_usd, 10.0);
        assert!(parse("[tasks]\nbudget = 1\n").is_err());
    }

    #[test]
    fn notifications_come_at_once_whatever_has_the_focus_unless_told() {
        let config = Config::default();
        assert_eq!(config.notifications, NotifySettings::default());
        assert_eq!(config.notifications.after_secs, 0);
        assert!(!config.notifications.unfocused_only);
        let config = parse("[notifications]\nafter_secs = 20\nunfocused_only = true\n").unwrap();
        assert_eq!(config.notifications.after_secs, 20);
        assert!(config.notifications.unfocused_only);
        assert!(parse("[notifications]\ndelay = 3\n").is_err());
    }

    #[test]
    fn the_event_log_keeps_a_month_unless_told() {
        assert_eq!(Config::default().events.keep_days, 30);
        let config = parse("[events]\nkeep_days = 0\n").unwrap();
        assert_eq!(config.events.keep_days, 0);
        assert!(parse("[events]\nkeep = 3\n").is_err());
    }

    #[test]
    fn sounds_play_unless_switched_off_for_all_or_an_agent() {
        let sound = Config::default().sound;
        assert!(sound.enabled);
        assert_eq!((sound.done, sound.request), (None, None));
        let config = parse(
            "[sound]\nenabled = false\nrequest = \"ask.mp3\"\n\n[sound.agents]\ncodex = false\n",
        )
        .unwrap();
        assert!(!config.sound.enabled);
        assert_eq!(config.sound.request, Some(PathBuf::from("ask.mp3")));
        assert_eq!(config.sound.agents.get("codex"), Some(&false));
        assert!(parse("[sound]\nvolume = 3\n").is_err());
    }

    #[test]
    fn the_tui_looks_for_a_newer_crystal_unless_told_not_to() {
        assert!(Config::default().update.check);
        let config = parse("[update]\ncheck = false\n").unwrap();
        assert!(!config.update.check);
        assert!(parse("[update]\nauto = true\n").is_err());
    }

    #[test]
    fn new_worktrees_start_from_origins_default_unless_told() {
        assert_eq!(Config::default().worktrees.base, None);
        let config = parse("[worktrees]\nbase = \"develop\"\n").unwrap();
        assert_eq!(config.worktrees.base.as_deref(), Some("develop"));
        assert!(parse("[worktrees]\nbranch = \"develop\"\n").is_err());
    }

    #[test]
    fn a_leftover_preset_says_its_now_a_profile() {
        let err = parse("[[preset]]\nname = \"x\"\nagent = \"claude\"\n").unwrap_err();
        assert!(
            format!("{err:#}").contains("`[[preset]]` tables are now `[[profile]]`"),
            "{err:#}"
        );
    }

    #[test]
    fn keys_and_the_sidebar_are_checked() {
        let keys = parse("[keys]\nkill = \"n\"\nnew-session = \"N\"").unwrap();
        assert_eq!(keys.keys.len(), 2);
        let unknown = parse("[keys]\nkil = \"n\"").unwrap_err();
        assert!(format!("{unknown:#}").contains("kil"), "{unknown:#}");
        let twice = parse("[keys]\nkill = \"q\"\nquit = \"q\"").unwrap_err();
        assert!(format!("{twice:#}").contains("both"), "{twice:#}");
        let narrow = parse("[sidebar]\nwidth = 4").unwrap_err();
        assert!(format!("{narrow:#}").contains("width"), "{narrow:#}");
        let folded = parse("[sidebar]\nfolded = true\nfold = \"hidden\"").unwrap();
        assert!(folded.sidebar.folded);
        assert_eq!(folded.sidebar.fold, Fold::Hidden);
    }

    #[test]
    fn a_new_terminal_runs_the_users_shell_a_login_one_on_a_mac() {
        let terminal = TerminalSettings::default();
        assert_eq!(
            terminal.shell_on(Some("/bin/zsh"), true),
            ["/bin/zsh", "-l"]
        );
        assert_eq!(terminal.shell_on(Some("/bin/zsh"), false), ["/bin/zsh"]);
        assert_eq!(terminal.shell_on(None, false), ["/bin/sh"]);
        assert_eq!(terminal.shell_on(Some(""), true), ["/bin/sh", "-l"]);
        // A shell with no login of its own starts as it is.
        assert_eq!(terminal.shell_on(Some("elvish"), true), ["elvish"]);

        let config = parse("[terminal]\ndefault_shell = \"nu\"\nshell_mode = \"login\"\n").unwrap();
        assert_eq!(
            config.terminal.shell_on(Some("/bin/zsh"), false),
            ["nu", "-l"]
        );
        let config = parse("[terminal]\nshell_mode = \"non_login\"\n").unwrap();
        assert_eq!(
            config.terminal.shell_on(Some("/bin/zsh"), true),
            ["/bin/zsh"]
        );
        let err = parse("[terminal]\nshell_mode = \"sometimes\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("sometimes"), "{err:#}");
    }

    #[test]
    fn a_new_terminal_follows_the_selection_unless_told_where() {
        let (home, current) = (Path::new("/home/ann"), Path::new("/code/app"));
        let start_dir = |text: &str| {
            let config = parse(&format!("[terminal]\nnew_cwd = \"{text}\"\n")).unwrap();
            config.terminal.start_dir(home, current)
        };
        assert_eq!(TerminalSettings::default().start_dir(home, current), None);
        assert_eq!(start_dir("follow"), None);
        assert_eq!(start_dir("home"), Some(PathBuf::from("/home/ann")));
        assert_eq!(start_dir("current"), Some(PathBuf::from("/code/app")));
        assert_eq!(start_dir("~/code"), Some(PathBuf::from("/home/ann/code")));
        assert_eq!(start_dir("/srv/work"), Some(PathBuf::from("/srv/work")));
        let err = parse("[terminal]\nnew_cwd = \"code\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("`code`"), "{err:#}");
    }

    #[test]
    fn the_tab_bar_is_on_top_with_nothing_at_its_right_unless_told() {
        assert_eq!(Config::default().tab_bar.position, BarPosition::Top);
        assert!(Config::default().tab_bar.right.is_empty());
        let config = parse(
            "[tab_bar]\nposition = \"bottom\"\nhide_when_single = true\nright = [\n\
             { type = \"hostname\" },\n{ type = \"clock\" },\n\
             { type = \"command\", command = \"date\" },\n]\n",
        )
        .unwrap();
        assert_eq!(config.tab_bar.position, BarPosition::Bottom);
        assert!(config.tab_bar.hide_when_single);
        assert_eq!(
            config.tab_bar.right[1],
            StatusEntry::Clock {
                format: "%H:%M".into()
            }
        );
        assert_eq!(
            config.tab_bar.right[2].command_times(),
            Some((Duration::from_secs(10), Duration::from_secs(2)))
        );
        let cases = [
            ("{ type = \"weather\" }", "weather"),
            ("{ type = \"text\" }", "text"),
            ("{ type = \"clock\", format = \"\" }", "format is empty"),
            ("{ type = \"command\", command = \" \" }", "runs nothing"),
            (
                "{ type = \"command\", command = \"date\", every = \"soon\" }",
                "isn't a while",
            ),
            (
                "{ type = \"command\", command = \"date\", timeout = \"off\" }",
                "can't be off",
            ),
            ("{ type = \"hostname\", name = \"x\" }", "name"),
        ];
        for (entry, expected) in cases {
            let err = parse(&format!("[tab_bar]\nright = [{entry}]\n")).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{entry}: {err:#}");
        }
    }

    #[test]
    fn the_theme_stays_put_unless_told_to_follow_the_appearance() {
        assert!(!Config::default().appearance.auto_switch);
        let config = parse(
            "[appearance]\nauto_switch = true\nlight_theme = \"Rose Pine Dawn\"\n\
             dark_theme = \"rose-pine\"\n",
        )
        .unwrap();
        assert!(config.appearance.auto_switch);
        assert_eq!(config.appearance.light_theme, ThemeName::find("dawn"));
        assert_eq!(config.appearance.dark_theme, ThemeName::find("rosepine"));
        let err = parse("[appearance]\ndark_theme = \"neon\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("neon"), "{err:#}");
        let colored = parse("[appearance.dark_colors]\naccent = \"#89b4fa\"\n").unwrap();
        assert_eq!(
            colored.appearance.dark_colors[&ColorToken::Accent],
            ColorValue(Color::Rgb(137, 180, 250))
        );
        let err = parse("[appearance.light_colors]\nacent = \"red\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("acent"), "{err:#}");
    }

    #[test]
    fn the_window_is_titled_after_the_session_unless_told() {
        assert_eq!(Config::default().window.title, "crystal · {session}");
        let config = parse("[window]\ntitle = \"\"\n").unwrap();
        assert_eq!(config.window.title, "");
        let err = parse("[window]\ntitle = \"{workspace}\"\n").unwrap_err();
        assert!(
            format!("{err:#}").contains("`{workspace}` isn't a token"),
            "{err:#}"
        );
    }

    #[test]
    fn the_settings_written_out_read_back_the_same() {
        let config = Config {
            notify: false,
            notify_command: Some("say \"$CRYSTAL_NOTICE\"".into()),
            new_session: "codex --model o3".into(),
            name_from_prompt: false,
            resume_reported_agents: false,
            theme: ThemeName::find("nord").unwrap(),
            scrollback_lines: 50_000,
            colors: BTreeMap::from([
                (ColorToken::Accent, ColorValue(Color::Rgb(245, 194, 231))),
                (ColorToken::FoundCurrent, ColorValue(Color::LightRed)),
                (ColorToken::Background, ColorValue(Color::Reset)),
            ]),
            plugins: BTreeMap::from([("memory".to_string(), false)]),
            notifications: NotifySettings {
                after_secs: 30,
                unfocused_only: true,
            },
            sound: SoundSettings {
                enabled: false,
                done: Some(PathBuf::from("sounds/done.wav")),
                request: None,
                agents: BTreeMap::from([("codex".to_string(), false)]),
            },
            memory: MemorySettings {
                distill: false,
                distill_model: "claude-sonnet-5-5".into(),
                distill_budget_usd: 0.5,
                embeddings: true,
                rerank: false,
            },
            tasks: TaskSettings {
                max_budget_usd: 2.5,
                daily_budget_usd: 20.0,
            },
            events: EventSettings { keep_days: 7 },
            handoff: HandoffSettings {
                in_git: vec![PathBuf::from("~/code/app")],
            },
            worktrees: WorktreeSettings {
                base: Some("develop".into()),
            },
            sessions: SessionSettings {
                stop_idle_after: "45m".into(),
            },
            update: UpdateSettings { check: false },
            profiles: vec![Profile {
                name: "review".into(),
                description: Some("A second pair of eyes".into()),
                agent: "claude".into(),
                model: Some("opus".into()),
                effort: Some("high".into()),
                mode: Some("plan".into()),
                args: vec!["--verbose".into()],
                prompt: Some("Review it.".into()),
                instructions: Some("Be brief.".into()),
                start_in: Some(StartIn::Worktree),
            }],
            flows: vec![Flow {
                name: "ship".into(),
                description: Some("Plan, then build".into()),
                steps: vec![
                    Step {
                        name: "plan".into(),
                        profile: Some("review".into()),
                        prompt: "Plan {goal}".into(),
                        placement: None,
                        worktree: false,
                        gate: true,
                        back_to: None,
                        max_rounds: Some(2),
                    },
                    Step {
                        name: "build".into(),
                        profile: None,
                        prompt: "Build it:\n{previous}".into(),
                        placement: Some(Placement::Fresh),
                        worktree: false,
                        gate: false,
                        back_to: None,
                        max_rounds: None,
                    },
                ],
            }],
            projects: vec![ProjectSettings {
                path: PathBuf::from("~/code/app"),
                run: Some("npm run dev".into()),
                open: Some("code .".into()),
            }],
            keys: BTreeMap::from([
                ("prefix".to_string(), Binding::One("ctrl+a".into())),
                (
                    "new-session".to_string(),
                    Binding::Many(vec!["n".into(), "ctrl+n".into()]),
                ),
            ]),
            sidebar: SidebarSettings {
                width: 36,
                folded: true,
                fold: Fold::Hidden,
                needs_you: false,
            },
            terminal: TerminalSettings {
                default_shell: "fish".into(),
                shell_mode: ShellMode::NonLogin,
                new_cwd: NewCwd::Path(PathBuf::from("~/code")),
            },
            window: WindowSettings {
                title: "{hostname}: {session}".into(),
            },
            tab_bar: TabBarSettings {
                position: BarPosition::Bottom,
                hide_when_single: true,
                right: vec![
                    StatusEntry::Hostname {},
                    StatusEntry::Clock {
                        format: "%a %H:%M".into(),
                    },
                    StatusEntry::Text {
                        text: "prod".into(),
                    },
                    StatusEntry::Command {
                        command: "uptime".into(),
                        every: "30s".into(),
                        timeout: "1s".into(),
                    },
                ],
                separator: " | ".into(),
            },
            appearance: AppearanceSettings {
                auto_switch: true,
                light_theme: ThemeName::find("latte"),
                dark_theme: None,
                light_colors: BTreeMap::from([(
                    ColorToken::Panel,
                    ColorValue(Color::Rgb(239, 241, 245)),
                )]),
                dark_colors: BTreeMap::new(),
            },
        };
        assert_eq!(parse(&config.to_toml()).unwrap(), config);
        assert_eq!(
            parse(&Config::default().to_toml()).unwrap(),
            Config::default()
        );
    }
}
