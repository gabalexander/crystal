//! The settings view, `,`: the settings crystal reads as it goes, in tabs,
//! each changed with a key or typed in and written to the config file at
//! once; how the models that search memory by meaning stand, as the daemon
//! says they do; crystal's hooks in each agent installed here, put there,
//! brought up to date or taken out with a key; and the keys `[keys]` gives,
//! each given by pressing it. While the view is open, the event loop reads
//! them again every half a second, so what it shows follows the file, a
//! download or the daemon, whoever changed them. The event loop does the
//! writing (see [`crate::config::apply`]).
//!
//! A key pressed for a command goes through the checks the config file's
//! keys do ([`keymap::rebind`]): one another command has is taken from it
//! only once the user says so, and one the hand-back key or a key of the
//! user's own has, never. One an installed plugin's action takes asks
//! first too, as the command would have it before the action. `/` filters
//! the keys' tab by a command's name, what it does or its keys.
//!
//! The mouse picks a tab, or a row, a click on the row the bar is on being
//! Enter on it, and the wheel moves the bar.
//!
//! The view is state and logic only, apart from [`draw`] and [`hit`] at
//! the end.

use super::app::PluginKey;
use super::appearance::Appearance;
use super::keymap::{self, Binding, Chord, KeyId, Keymap, Mode, Rebinding};
use super::search::letters_in;
use super::text_input::TextInput;
use super::theme::{self, Theme};
use crate::config::{
    self, BarPosition, Config, EmptiedWorktree, Fold, NewCwd, SessionSettings, ShellMode,
    SidebarOrder, TaskSettings, ThemeName,
};
use crate::embed::Status;
use crate::integration::{self, Standing};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use std::path::PathBuf;

/// The settings as they are, read off the loop.
#[derive(Debug, Clone)]
pub struct Current {
    /// The config file, and what it says, or why it can't be read.
    pub path: PathBuf,
    pub config: Result<Config, String>,
    /// How the model stands, as the daemon said; `None` when it couldn't.
    pub model: Option<Status>,
    /// The agents installed here that crystal can hook, and how its hooks
    /// in each stand.
    pub integrations: Vec<(integration::Agent, Standing)>,
}

/// A setting the view changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Notify,
    NotifyAfter,
    UnfocusedOnly,
    NotifyCommand,
    Sound,
    ConfirmQuit,
    UpdateCheck,
    KeepEvents,
    HideDrafts,
    Theme,
    AutoSwitch,
    LightTheme,
    DarkTheme,
    TabBar,
    HideSingleTab,
    Separator,
    WindowTitle,
    SidebarWidth,
    SidebarFolded,
    Fold,
    PinNeedsYou,
    SidebarOrder,
    PhoneWidth,
    ShowKeys,
    MermaidAscii,
    NewSession,
    NameFromPrompt,
    StopIdle,
    RestartSpacing,
    ResumeReported,
    Shell,
    ShellMode,
    NewCwd,
    Scrollback,
    RestoreScreens,
    WorktreeBase,
    WorktreeDirectory,
    RemoveEmptied,
    MouseCapture,
    CopyOnSelect,
    ScrollLines,
    Scrollbars,
    AttachCapture,
    ProgramsCopy,
    TaskPermissions,
    TaskBudget,
    DailyBudget,
    Distill,
    DistillModel,
    DistillBudget,
    Embeddings,
    Rerank,
}

impl Setting {
    /// Where the setting is in the config file.
    pub fn keys(self) -> &'static [&'static str] {
        match self {
            Setting::Notify => &["notify"],
            Setting::NotifyAfter => &["notifications", "after_secs"],
            Setting::UnfocusedOnly => &["notifications", "unfocused_only"],
            Setting::NotifyCommand => &["notify_command"],
            Setting::Sound => &["sound", "enabled"],
            Setting::ConfirmQuit => &["confirm_quit"],
            Setting::UpdateCheck => &["update", "check"],
            Setting::KeepEvents => &["events", "keep_days"],
            Setting::HideDrafts => &["forge", "hide_draft_prs"],
            Setting::Theme => &["theme"],
            Setting::AutoSwitch => &["appearance", "auto_switch"],
            Setting::LightTheme => &["appearance", "light_theme"],
            Setting::DarkTheme => &["appearance", "dark_theme"],
            Setting::TabBar => &["tab_bar", "position"],
            Setting::HideSingleTab => &["tab_bar", "hide_when_single"],
            Setting::Separator => &["tab_bar", "separator"],
            Setting::WindowTitle => &["window", "title"],
            Setting::SidebarWidth => &["sidebar", "width"],
            Setting::SidebarFolded => &["sidebar", "folded"],
            Setting::Fold => &["sidebar", "fold"],
            Setting::PinNeedsYou => &["sidebar", "needs_you"],
            Setting::SidebarOrder => &["sidebar", "order"],
            Setting::PhoneWidth => &["sidebar", "phone_width"],
            Setting::ShowKeys => &["show_keys"],
            Setting::MermaidAscii => &["mermaid_ascii"],
            Setting::NewSession => &["new_session"],
            Setting::NameFromPrompt => &["name_from_prompt"],
            Setting::StopIdle => &["sessions", "stop_idle_after"],
            Setting::RestartSpacing => &["sessions", "restart_spacing_ms"],
            Setting::ResumeReported => &["resume_reported_agents"],
            Setting::Shell => &["terminal", "default_shell"],
            Setting::ShellMode => &["terminal", "shell_mode"],
            Setting::NewCwd => &["terminal", "new_cwd"],
            Setting::Scrollback => &["scrollback_lines"],
            Setting::RestoreScreens => &["sessions", "restore_screens"],
            Setting::WorktreeBase => &["worktrees", "base"],
            Setting::WorktreeDirectory => &["worktrees", "directory"],
            Setting::RemoveEmptied => &["worktrees", "remove_emptied"],
            Setting::MouseCapture => &["mouse", "capture"],
            Setting::CopyOnSelect => &["mouse", "copy_on_select"],
            Setting::ScrollLines => &["mouse", "scroll_lines"],
            Setting::Scrollbars => &["mouse", "scrollbars"],
            Setting::AttachCapture => &["mouse", "attach_capture"],
            Setting::ProgramsCopy => &["clipboard", "allow_programs"],
            Setting::TaskPermissions => &["tasks", "permission_mode"],
            Setting::TaskBudget => &["tasks", "max_budget_usd"],
            Setting::DailyBudget => &["tasks", "daily_budget_usd"],
            Setting::Distill => &["memory", "distill"],
            Setting::DistillModel => &["memory", "distill_model"],
            Setting::DistillBudget => &["memory", "distill_budget_usd"],
            Setting::Embeddings => &["memory", "embeddings"],
            Setting::Rerank => &["memory", "rerank"],
        }
    }

    /// What its row calls it.
    fn name(self) -> &'static str {
        match self {
            Setting::Notify => "notifications",
            Setting::NotifyAfter => "  after",
            Setting::UnfocusedOnly => "  only when away",
            Setting::NotifyCommand => "  command",
            Setting::Sound => "sounds",
            Setting::ConfirmQuit => "ask before quitting",
            Setting::UpdateCheck => "look for updates",
            Setting::KeepEvents => "keep events",
            Setting::HideDrafts => "hide drafts",
            Setting::Theme => "theme",
            Setting::AutoSwitch => "  follow the system",
            Setting::LightTheme => "  when it's light",
            Setting::DarkTheme => "  when it's dark",
            Setting::TabBar => "tab bar",
            Setting::HideSingleTab => "  hide with one tab",
            Setting::Separator => "  separator",
            Setting::WindowTitle => "window title",
            Setting::SidebarWidth => "sidebar width",
            Setting::SidebarFolded => "  starts folded",
            Setting::Fold => "  folded, keeps",
            Setting::PinNeedsYou => "  pin what needs you",
            Setting::SidebarOrder => "  waiting goes first",
            Setting::PhoneWidth => "  one column at",
            Setting::ShowKeys => "show keys pressed",
            Setting::MermaidAscii => "diagrams in ASCII",
            Setting::NewSession => "new session runs",
            Setting::NameFromPrompt => "name from the prompt",
            Setting::StopIdle => "stop idle agents",
            Setting::RestartSpacing => "space out restarts",
            Setting::ResumeReported => "resume as reported",
            Setting::Shell => "shell",
            Setting::ShellMode => "  login shell",
            Setting::NewCwd => "new terminals in",
            Setting::Scrollback => "scrollback",
            Setting::RestoreScreens => "restore screens",
            Setting::WorktreeBase => "base branch",
            Setting::WorktreeDirectory => "directory",
            Setting::RemoveEmptied => "remove once emptied",
            Setting::MouseCapture => "take the mouse",
            Setting::CopyOnSelect => "copy on select",
            Setting::ScrollLines => "wheel scrolls",
            Setting::Scrollbars => "scrollbars",
            Setting::AttachCapture => "take it in attach",
            Setting::ProgramsCopy => "programs copy",
            Setting::TaskPermissions => "permission mode",
            Setting::TaskBudget => "a run's budget",
            Setting::DailyBudget => "a day's budget",
            Setting::Distill => "distill finished work",
            Setting::DistillModel => "  model",
            Setting::DistillBudget => "  budget",
            Setting::Embeddings => "search by meaning",
            Setting::Rerank => "  rerank",
        }
    }

    /// Whether it's typed in, rather than gone through with `←/→`.
    fn typed(self) -> bool {
        matches!(
            self,
            Setting::NotifyCommand
                | Setting::Separator
                | Setting::WindowTitle
                | Setting::NewSession
                | Setting::Shell
                | Setting::WorktreeBase
                | Setting::WorktreeDirectory
                | Setting::DistillModel
        )
    }

    /// Whether, typed in, its spaces at either end count.
    fn keeps_spaces(self) -> bool {
        matches!(self, Setting::Separator | Setting::WindowTitle)
    }

    /// Whether typed in empty, it's taken out of the file, for its default:
    /// one empty means nothing, or something crystal can't run.
    fn empty_is_default(self) -> bool {
        matches!(
            self,
            Setting::NotifyCommand
                | Setting::NewSession
                | Setting::WorktreeBase
                | Setting::WorktreeDirectory
                | Setting::DistillModel
        )
    }
}

/// What a setting is set to, as the config file writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    On(bool),
    Number(i64),
    /// An amount in US dollars, in cents.
    Cents(i64),
    Text(String),
}

impl Value {
    pub fn toml(&self) -> toml_edit::Value {
        match self {
            Value::On(on) => (*on).into(),
            Value::Number(number) => (*number).into(),
            Value::Cents(cents) => (*cents as f64 / 100.0).into(),
            Value::Text(text) => text.as_str().into(),
        }
    }
}

impl From<bool> for Value {
    fn from(on: bool) -> Value {
        Value::On(on)
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Value {
        Value::Text(text.to_string())
    }
}

/// A change to one setting, to write to the config file: set to `to`, or,
/// with none, its line taken out, for it to have its default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub setting: Setting,
    pub to: Option<Value>,
}

impl Change {
    pub fn set(setting: Setting, to: impl Into<Value>) -> Change {
        Change {
            setting,
            to: Some(to.into()),
        }
    }

    /// Its line taken out of the file.
    pub fn default(setting: Setting) -> Change {
        Change { setting, to: None }
    }

    /// The edit it makes to the config file.
    pub fn edit(&self) -> config::Edit {
        match &self.to {
            Some(to) => config::Edit::set(self.setting.keys(), to.toml()),
            None => config::Edit::remove(self.setting.keys()),
        }
    }
}

/// The edits a rebinding makes to the config file's `[keys]`.
pub fn key_edits(rebinding: &Rebinding) -> Vec<config::Edit> {
    let edit = |(key, binding): &(KeyId, Option<Binding>)| {
        let keys = ["keys", key.id()];
        match binding {
            Some(Binding::One(written)) => config::Edit::set(&keys, written.as_str().into()),
            Some(Binding::Many(written)) => {
                let list = written.iter().map(String::as_str).collect();
                config::Edit::set(&keys, toml_edit::Value::Array(list))
            }
            None => config::Edit::remove(&keys),
        }
    };
    rebinding.lines.iter().map(edit).collect()
}

/// The waits `←/→` go through for how long a session needs the user
/// before they're told, in seconds.
const NOTIFY_AFTER: [u64; 6] = [0, 10, 30, 60, 120, 300];

/// The lines `←/→` go through for how far a notch of the wheel scrolls.
const SCROLL_LINES: [u16; 5] = [1, 2, 3, 5, 10];

/// How many days the event log keeps, `0` for ever.
const KEEP_DAYS: [u32; 5] = [0, 7, 30, 90, 365];

/// The sidebar's widths, in columns.
const WIDTHS: [u16; 8] = [20, 24, 28, 32, 36, 40, 48, 60];

/// How narrow a terminal shows one column, in columns, `0` never.
const PHONE_WIDTHS: [u16; 6] = [0, 48, 56, 64, 72, 80];

/// How many rows that scrolled off a session's screen it keeps.
const SCROLLBACK: [usize; 5] = [1_000, 5_000, 10_000, 50_000, 100_000];

/// What a background task's run may spend, in cents, `0` without a limit.
const TASK_BUDGETS: [i64; 6] = [0, 100, 200, 500, 1_000, 2_000];

/// What background tasks may spend in a day, in cents, `0` without one.
const DAILY_BUDGETS: [i64; 6] = [0, 500, 1_000, 2_000, 5_000, 10_000];

/// What the distiller may spend on a task, in cents.
const DISTILL_BUDGETS: [i64; 4] = [10, 25, 50, 100];

/// The shell modes and the places a new terminal starts, as the file
/// writes them.
const SHELL_MODES: [&str; 3] = ["auto", "login", "non_login"];
const NEW_CWDS: [&str; 3] = ["follow", "home", "current"];

/// The choice after `now` among `choices`, in order, or before it, going
/// round: one set by hand between two goes to the next, or the one before.
fn next_of<T: PartialOrd + Copy>(choices: &[T], now: T, forward: bool) -> T {
    if forward {
        let next = choices.iter().find(|&&choice| choice > now);
        *next.unwrap_or(&choices[0])
    } else {
        let before = choices.iter().rev().find(|&&choice| choice < now);
        *before.unwrap_or(&choices[choices.len() - 1])
    }
}

/// The place after `at` among `count` choices, or before it, going round;
/// one written by hand that isn't among them, `None`, goes to the first.
fn round(at: Option<usize>, count: usize, forward: bool) -> usize {
    match (at, forward) {
        (Some(at), true) => (at + 1) % count,
        (Some(at), false) => (at + count - 1) % count,
        (None, _) => 0,
    }
}

/// The choice after `now` among `choices`, or before it, going round.
fn next_named<'a>(choices: &[&'a str], now: &str, forward: bool) -> &'a str {
    let at = choices.iter().position(|choice| *choice == now);
    choices[round(at, choices.len(), forward)]
}

/// A wait as the view shows it.
fn wait_text(secs: u64) -> String {
    match secs {
        0 => "at once".to_string(),
        secs if secs % 60 == 0 => format!("{}m", secs / 60),
        secs => format!("{secs}s"),
    }
}

/// An amount in US dollars, in cents.
fn cents(usd: f64) -> i64 {
    (usd * 100.0).round() as i64
}

/// Cents as the view shows them: `$5`, `$0.25`.
fn dollars(cents: i64) -> String {
    match cents % 100 {
        0 => format!("${}", cents / 100),
        rest => format!("${}.{rest:02}", cents / 100),
    }
}

/// A row the bar can be on: a setting, crystal's hooks in an agent, or
/// what `[keys]` gives keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Setting(Setting),
    Hooks(integration::Agent),
    Key(KeyId),
}

/// A tab of the view: its settings under their headings, or, with none,
/// the agents' hooks or every key.
struct Tab {
    name: &'static str,
    sections: &'static [(&'static str, &'static [Setting])],
}

use Setting as S;

/// The tabs, in order, the agents' hooks and the keys last.
const TABS: [Tab; 8] = [
    Tab {
        name: "General",
        sections: &[
            (
                "Notifications",
                &[
                    S::Notify,
                    S::NotifyAfter,
                    S::UnfocusedOnly,
                    S::NotifyCommand,
                    S::Sound,
                ],
            ),
            ("Quitting and updates", &[S::ConfirmQuit, S::UpdateCheck]),
            ("Events", &[S::KeepEvents]),
            ("Pull requests", &[S::HideDrafts]),
        ],
    },
    Tab {
        name: "Look",
        sections: &[
            (
                "Theme",
                &[S::Theme, S::AutoSwitch, S::LightTheme, S::DarkTheme],
            ),
            (
                "Tab bar and window",
                &[S::TabBar, S::HideSingleTab, S::Separator, S::WindowTitle],
            ),
            (
                "Sidebar",
                &[
                    S::SidebarWidth,
                    S::SidebarFolded,
                    S::Fold,
                    S::PinNeedsYou,
                    S::SidebarOrder,
                    S::PhoneWidth,
                ],
            ),
            ("Keys and diagrams", &[S::ShowKeys, S::MermaidAscii]),
        ],
    },
    Tab {
        name: "Sessions",
        sections: &[
            (
                "Agents",
                &[
                    S::NewSession,
                    S::NameFromPrompt,
                    S::StopIdle,
                    S::RestartSpacing,
                    S::ResumeReported,
                ],
            ),
            (
                "Terminals",
                &[
                    S::Shell,
                    S::ShellMode,
                    S::NewCwd,
                    S::Scrollback,
                    S::RestoreScreens,
                ],
            ),
            (
                "Worktrees",
                &[S::WorktreeBase, S::WorktreeDirectory, S::RemoveEmptied],
            ),
        ],
    },
    Tab {
        name: "Mouse",
        sections: &[
            (
                "Mouse",
                &[
                    S::MouseCapture,
                    S::CopyOnSelect,
                    S::ScrollLines,
                    S::Scrollbars,
                    S::AttachCapture,
                ],
            ),
            ("Clipboard", &[S::ProgramsCopy]),
        ],
    },
    Tab {
        name: "Tasks",
        sections: &[(
            "Background tasks",
            &[S::TaskPermissions, S::TaskBudget, S::DailyBudget],
        )],
    },
    Tab {
        name: "Memory",
        sections: &[(
            "Memory",
            &[
                S::Distill,
                S::DistillModel,
                S::DistillBudget,
                S::Embeddings,
                S::Rerank,
            ],
        )],
    },
    Tab {
        name: "Integrations",
        sections: &[],
    },
    Tab {
        name: "Keys",
        sections: &[],
    },
];

/// The agents' hooks' tab, and the keys'.
const HOOKS_TAB: usize = TABS.len() - 2;
const KEYS_TAB: usize = TABS.len() - 1;

/// The rows of tab `tab`, in the order they're listed, with `hooked` the
/// agents installed here.
fn rows(tab: usize, hooked: &[(integration::Agent, Standing)]) -> Vec<Row> {
    if tab == HOOKS_TAB {
        return hooked.iter().map(|&(agent, _)| Row::Hooks(agent)).collect();
    }
    if tab == KEYS_TAB {
        return KeyId::all().map(Row::Key).collect();
    }
    let sections = TABS[tab].sections.iter();
    let settings = sections.flat_map(|(_, settings)| settings.iter());
    settings.map(|setting| Row::Setting(*setting)).collect()
}

/// The heading a key's row is listed under.
fn key_heading(key: KeyId) -> &'static str {
    match key.mode() {
        None if matches!(key, KeyId::Prefix | KeyId::HandBack) => "From a pane",
        None => "Sidebar",
        Some(Mode::Answer) => "Answering a background task",
        Some(Mode::Resize) => "Resize mode",
        Some(Mode::View) => "Views",
    }
}

/// What a key in the view leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Change(Change),
    /// Write the keys, as the user gave them.
    Keys(Rebinding),
    /// Have the daemon download the model if it isn't here, and give every
    /// entry its vector.
    Prepare,
    /// Put crystal's hooks in the agent's settings, or bring them up to
    /// date, or with `install` false, take them out.
    Integrate {
        agent: integration::Agent,
        install: bool,
    },
}

/// What the row the bar is on is in the middle of.
#[derive(Debug)]
enum Editing {
    /// Its setting being typed in.
    Text(TextInput),
    /// Waiting for the key to give it, in place of its keys or, with
    /// `add`, beside them.
    Capture { add: bool },
    /// A key pressed that something else has, or that an installed
    /// plugin's `action` takes, which the command would have first: waiting
    /// for Enter to give it, or any other key to leave it.
    Taking {
        chord: Chord,
        rebinding: Rebinding,
        action: Option<String>,
    },
    /// The keys' tab's filter being typed.
    Filter,
}

/// What the mouse is on in the view: see [`hit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// A tab's name, by its place among [`TABS`].
    Tab(usize),
    /// A row, by its place among the tab's.
    Row(usize),
    /// Anywhere else, in the view or not.
    Elsewhere,
}

pub struct SettingsView {
    /// `None` until the settings have been read.
    current: Option<Current>,
    /// The tab in front, among [`TABS`].
    tab: usize,
    /// The row the bar is on, among the tab's.
    selected: usize,
    editing: Option<Editing>,
    /// Why the last thing asked for couldn't be done.
    problem: Option<String>,
    /// What the last thing asked for came to, when there's something to
    /// say, like what's next for Codex's hooks.
    note: Option<String>,
    /// The keys' tab's filter, typed after `/`, while it narrows the rows.
    filter: Option<TextInput>,
    /// The keys the keys' tab lists: those the filter finds, in order.
    keys: Vec<KeyId>,
    /// The sidebar keys the installed plugins' actions took.
    plugin_keys: Vec<PluginKey>,
}

impl SettingsView {
    pub fn new() -> SettingsView {
        SettingsView {
            current: None,
            tab: 0,
            selected: 0,
            editing: None,
            problem: None,
            note: None,
            filter: None,
            keys: KeyId::all().collect(),
            plugin_keys: Vec::new(),
        }
    }

    /// Takes the sidebar keys the installed plugins' actions took, to say
    /// so of one pressed for a command.
    pub fn set_plugin_keys(&mut self, keys: Vec<PluginKey>) {
        self.plugin_keys = keys;
    }

    /// Says what came of the last thing asked for.
    pub fn set_note(&mut self, note: String) {
        self.note = Some(note);
    }

    /// Takes the settings as they are now.
    pub fn set_current(&mut self, current: Current) {
        self.current = Some(current);
        // A key given since may be one the filter finds now, or no longer.
        self.refilter();
    }

    pub fn set_problem(&mut self, problem: String) {
        self.problem = Some(problem);
    }

    #[cfg(test)]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Whether every key is the view's as it comes, with no key of the
    /// views' standing for another: while a setting is typed in, or a key
    /// is waited for.
    pub fn takes_keys_as_they_come(&self) -> bool {
        self.editing.is_some()
    }

    fn config(&self) -> Option<&Config> {
        self.current.as_ref()?.config.as_ref().ok()
    }

    fn model(&self) -> Option<&Status> {
        self.current.as_ref()?.model.as_ref()
    }

    /// The agents installed here crystal can hook, as last read.
    fn hooked(&self) -> &[(integration::Agent, Standing)] {
        self.current
            .as_ref()
            .map_or(&[], |current| current.integrations.as_slice())
    }

    /// The rows of the tab in front: in the keys' tab, those the filter
    /// finds.
    fn rows(&self) -> Vec<Row> {
        match self.tab {
            KEYS_TAB => self.keys.iter().map(|&key| Row::Key(key)).collect(),
            tab => rows(tab, self.hooked()),
        }
    }

    /// The place of the row the bar is on: the tab's last, after an agent
    /// has gone between two reads; none in a tab with none.
    fn at(&self) -> Option<usize> {
        let last = self.rows().len().checked_sub(1)?;
        Some(self.selected.min(last))
    }

    /// The row the bar is on.
    fn row(&self) -> Option<Row> {
        Some(self.rows()[self.at()?])
    }

    /// Moves the bar `by` rows, down or up, stopping at either end.
    fn move_bar(&mut self, by: isize) {
        let last = self.rows().len().saturating_sub(1);
        self.selected = self.selected.min(last).saturating_add_signed(by).min(last);
    }

    /// Lists the keys the filter finds, every key without one.
    fn refilter(&mut self) {
        let query = self.filter.as_ref().map_or("", |filter| filter.text());
        let keymap = (self.config())
            .and_then(|config| Keymap::new(&config.keys).ok())
            .unwrap_or_default();
        let keys = KeyId::all().filter(|&key| {
            let chords = keymap.keys_of(key);
            finds(query, key.id(), key.does(), &chords)
        });
        self.keys = keys.collect();
    }

    /// Takes the filter away, the bar staying on the key it was on.
    fn drop_filter(&mut self) {
        let was = self.row();
        self.filter = None;
        self.refilter();
        let rows = self.rows();
        self.selected = (was.and_then(|was| rows.iter().position(|row| *row == was))).unwrap_or(0);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        self.problem = None;
        self.note = None;
        match self.editing.take() {
            Some(Editing::Text(input)) => return self.on_text_key(input, key),
            Some(Editing::Capture { add }) => return self.on_captured(key, add),
            Some(Editing::Taking { rebinding, .. }) => {
                return match key.code {
                    KeyCode::Enter => Outcome::Keys(rebinding),
                    _ => Outcome::Stay,
                };
            }
            Some(Editing::Filter) => return self.on_filter_key(key),
            None => {}
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Stay;
        }
        let last = self.rows().len().saturating_sub(1);
        match key.code {
            // Esc takes the keys' filter away before it closes the view.
            KeyCode::Esc if self.filter.is_some() => self.drop_filter(),
            KeyCode::Esc | KeyCode::Char('q' | ',') => return Outcome::Close,
            KeyCode::Char('/') if self.tab == KEYS_TAB => {
                self.filter.get_or_insert_with(TextInput::default);
                self.editing = Some(Editing::Filter);
            }
            KeyCode::Tab | KeyCode::Char(']') => self.show_tab((self.tab + 1) % TABS.len()),
            KeyCode::BackTab | KeyCode::Char('[') => {
                self.show_tab((self.tab + TABS.len() - 1) % TABS.len());
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let tab = digit as usize - '1' as usize;
                if tab < TABS.len() {
                    self.show_tab(tab);
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_bar(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_bar(-1),
            KeyCode::PageDown => self.move_bar(PAGE),
            KeyCode::PageUp => self.move_bar(-PAGE),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = last,
            _ => {
                return match self.row() {
                    Some(Row::Setting(setting)) => self.on_setting_key(setting, key.code),
                    Some(Row::Hooks(agent)) => self.on_hooks_key(agent, key.code),
                    Some(Row::Key(id)) => self.on_key_row_key(id, key.code),
                    None => Outcome::Stay,
                };
            }
        }
        Outcome::Stay
    }

    fn show_tab(&mut self, tab: usize) {
        self.tab = tab;
        self.selected = 0;
        if self.filter.take().is_some() {
            self.refilter();
        }
    }

    /// A key while the keys' filter is typed: the arrows move the bar
    /// through what it finds, Enter keeps it and goes back to the rows, Esc
    /// takes it away, and the rest edit it.
    fn on_filter_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => {
                self.drop_filter();
                return Outcome::Stay;
            }
            KeyCode::Enter => {
                if self
                    .filter
                    .as_ref()
                    .is_some_and(|f| f.text().trim().is_empty())
                {
                    self.drop_filter();
                }
                return Outcome::Stay;
            }
            KeyCode::Down => self.move_bar(1),
            KeyCode::Up => self.move_bar(-1),
            KeyCode::PageDown => self.move_bar(PAGE),
            KeyCode::PageUp => self.move_bar(-PAGE),
            _ => {
                if let Some(filter) = &mut self.filter {
                    filter.on_key(&key);
                }
                self.refilter();
                self.selected = 0;
            }
        }
        self.editing = Some(Editing::Filter);
        Outcome::Stay
    }

    /// What the mouse does over the view, on `spot`: a click on a tab shows
    /// it, one on a row puts the bar there, or on the row the bar is on is
    /// Enter there; the wheel moves the bar. Not while a setting is typed
    /// in or a key is waited for, which a stray click mustn't answer.
    pub fn on_mouse(&mut self, kind: MouseEventKind, spot: Spot) -> Outcome {
        let typing_filter = matches!(self.editing, Some(Editing::Filter));
        if self.editing.is_some() && !typing_filter {
            return Outcome::Stay;
        }
        match kind {
            MouseEventKind::ScrollDown => self.move_bar(1),
            MouseEventKind::ScrollUp => self.move_bar(-1),
            MouseEventKind::Down(MouseButton::Left) => {
                self.problem = None;
                self.note = None;
                match spot {
                    Spot::Tab(tab) => {
                        self.editing = None;
                        self.show_tab(tab);
                    }
                    Spot::Row(row) => {
                        self.editing = None;
                        if self.at() == Some(row) {
                            return self.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                        }
                        self.selected = row;
                    }
                    Spot::Elsewhere => {}
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    /// A key on a setting's row: a switch turns over, a choice goes to the
    /// next, or the one before; a setting typed in is opened to type.
    fn on_setting_key(&mut self, setting: Setting, code: KeyCode) -> Outcome {
        let choices = !setting.typed();
        match code {
            KeyCode::Char(' ') => self.change(setting, true),
            KeyCode::Char('l') | KeyCode::Right if choices => self.change(setting, true),
            KeyCode::Char('h') | KeyCode::Left if choices => self.change(setting, false),
            KeyCode::Enter if setting == Setting::Embeddings => self.prepare(),
            KeyCode::Enter => self.change(setting, true),
            KeyCode::Backspace | KeyCode::Delete => match self.readable() {
                Some(_) => Outcome::Change(Change::default(setting)),
                None => Outcome::Stay,
            },
            _ => Outcome::Stay,
        }
    }

    /// A key on an agent's row: space or Enter puts crystal's hooks in it,
    /// or brings them up to date, or takes them out once they're in.
    fn on_hooks_key(&mut self, agent: integration::Agent, code: KeyCode) -> Outcome {
        if !matches!(code, KeyCode::Char(' ') | KeyCode::Enter) {
            return Outcome::Stay;
        }
        let standing = self.hooked().iter().find(|(hooked, _)| *hooked == agent);
        let install = standing.is_none_or(|(_, standing)| *standing != Standing::Installed);
        Outcome::Integrate { agent, install }
    }

    /// A key on a key's row: Enter or space waits for the key to give it,
    /// `a` for one to add, `x` leaves it none and Delete puts its own back.
    fn on_key_row_key(&mut self, id: KeyId, code: KeyCode) -> Outcome {
        let Some(keys) = self.readable().map(|config| config.keys.clone()) else {
            return Outcome::Stay;
        };
        let rebinding = match code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.editing = Some(Editing::Capture { add: false });
                return Outcome::Stay;
            }
            KeyCode::Char('a') => {
                self.editing = Some(Editing::Capture { add: true });
                return Outcome::Stay;
            }
            KeyCode::Char('x') => keymap::unbind(&keys, id),
            KeyCode::Backspace | KeyCode::Delete => keymap::reset(&keys, id),
            _ => return Outcome::Stay,
        };
        self.keys_or_why(rebinding)
    }

    /// The key pressed for the row's, given it, or once the user says so,
    /// taken from what has it.
    fn on_captured(&mut self, key: KeyEvent, add: bool) -> Outcome {
        if key.code == KeyCode::Esc {
            return Outcome::Stay;
        }
        // A modifier on its own, which a terminal speaking the Kitty
        // protocol may send, is the start of a key, not one.
        if matches!(key.code, KeyCode::Modifier(_)) {
            self.editing = Some(Editing::Capture { add });
            return Outcome::Stay;
        }
        let chord = Chord::of(&key);
        if Chord::parse(&chord.config()) != Ok(chord) {
            self.problem = Some("crystal can't write that key down: press another".to_string());
            return Outcome::Stay;
        }
        let (Some(Row::Key(id)), Some(config)) = (self.row(), self.config()) else {
            return Outcome::Stay;
        };
        let rebinding = keymap::rebind(&config.keys, id, chord, add);
        let action = self.action_with(id, chord);
        match rebinding {
            Ok(rebinding) if rebinding.taken_from.is_some() || action.is_some() => {
                self.editing = Some(Editing::Taking {
                    chord,
                    rebinding,
                    action,
                });
                Outcome::Stay
            }
            rebinding => self.keys_or_why(rebinding),
        }
    }

    /// The installed plugin's action whose key, or first of two, is
    /// `chord`, as the view names it, when `id` given it would run in the
    /// action's place: a command's key in the sidebar, or the prefix, which
    /// does nothing there.
    fn action_with(&self, id: KeyId, chord: Chord) -> Option<String> {
        if !matches!(id, KeyId::Command(_) | KeyId::Prefix) {
            return None;
        }
        let taken = (self.plugin_keys.iter())
            .find(|taken| taken.key.is_some_and(|key| key.first() == chord))?;
        Some(format!("the {} plugin's {:?}", taken.plugin, taken.title))
    }

    fn keys_or_why(&mut self, rebinding: Result<Rebinding, String>) -> Outcome {
        match rebinding {
            Ok(rebinding) => Outcome::Keys(rebinding),
            Err(why) => {
                self.problem = Some(why);
                Outcome::Stay
            }
        }
    }

    /// A key while a setting is typed in: Enter writes it, Esc leaves it
    /// as it was, and the rest edit it.
    fn on_text_key(&mut self, mut input: TextInput, key: KeyEvent) -> Outcome {
        let Some(Row::Setting(setting)) = self.row() else {
            return Outcome::Stay;
        };
        match key.code {
            KeyCode::Esc => Outcome::Stay,
            KeyCode::Enter => {
                let typed = match setting.keeps_spaces() {
                    true => input.text(),
                    false => input.text().trim(),
                };
                let change = match typed.is_empty() && setting.empty_is_default() {
                    true => Change::default(setting),
                    false => Change::set(setting, typed),
                };
                Outcome::Change(change)
            }
            _ => {
                input.on_key(&key);
                self.editing = Some(Editing::Text(input));
                Outcome::Stay
            }
        }
    }

    /// Puts pasted text in the setting being typed, or the keys' filter,
    /// if one is.
    pub fn on_paste(&mut self, text: &str) {
        match &mut self.editing {
            Some(Editing::Text(input)) => input.insert_str(text),
            Some(Editing::Filter) => {
                if let Some(filter) = &mut self.filter {
                    filter.insert_str(text);
                }
                self.refilter();
                self.selected = 0;
            }
            _ => {}
        }
    }

    /// The settings, once they're read and make sense; or else nothing,
    /// saying why when they don't.
    fn readable(&mut self) -> Option<&Config> {
        match &self.current {
            None => None,
            Some(Current {
                config: Err(why), ..
            }) => {
                self.problem = Some(format!("the config file can't be read: {why}"));
                None
            }
            Some(Current { config: Ok(c), .. }) => Some(c),
        }
    }

    /// The change a key asks for on `setting`: a switch turns over, a
    /// choice goes to the next, or `forward` false, the one before; one
    /// typed in opens to type.
    fn change(&mut self, setting: Setting, forward: bool) -> Outcome {
        let Some(config) = self.readable().cloned() else {
            return Outcome::Stay;
        };
        let config = &config;
        if setting.typed() {
            let text = typed_text(setting, config);
            self.editing = Some(Editing::Text(TextInput::with_text(&text)));
            return Outcome::Stay;
        }
        let on = |on: bool| Change::set(setting, on);
        let number = |number: i64| Change::set(setting, Value::Number(number));
        let money = |now: f64, choices: &[i64]| {
            Change::set(setting, Value::Cents(next_of(choices, cents(now), forward)))
        };
        let change = match setting {
            S::Notify => on(!config.notify),
            S::NotifyAfter => {
                let wait = next_of(&NOTIFY_AFTER, config.notifications.after_secs, forward);
                number(i64::try_from(wait).unwrap_or(i64::MAX))
            }
            S::UnfocusedOnly => on(!config.notifications.unfocused_only),
            S::Sound => on(!config.sound.enabled),
            S::ConfirmQuit => on(!config.confirm_quit),
            S::UpdateCheck => on(!config.update.check),
            S::KeepEvents => number(next_of(&KEEP_DAYS, config.events.keep_days, forward).into()),
            S::HideDrafts => on(!config.forge.hide_draft_prs),
            S::Theme if forward => Change::set(setting, config.theme.next().name()),
            S::Theme => Change::set(setting, config.theme.previous().name()),
            S::AutoSwitch => on(!config.appearance.auto_switch),
            S::LightTheme => side_theme(setting, config.appearance.light_theme, forward),
            S::DarkTheme => side_theme(setting, config.appearance.dark_theme, forward),
            S::TabBar => Change::set(
                setting,
                match config.tab_bar.position {
                    BarPosition::Top => "bottom",
                    BarPosition::Bottom => "top",
                },
            ),
            S::HideSingleTab => on(!config.tab_bar.hide_when_single),
            S::SidebarWidth => number(next_of(&WIDTHS, config.sidebar.width, forward).into()),
            S::SidebarFolded => on(!config.sidebar.folded),
            S::Fold => Change::set(
                setting,
                match config.sidebar.fold {
                    Fold::Marks => "hidden",
                    Fold::Hidden => "marks",
                },
            ),
            S::PinNeedsYou => on(!config.sidebar.needs_you),
            S::SidebarOrder => Change::set(
                setting,
                match config.sidebar.order {
                    SidebarOrder::Attention => "stable",
                    SidebarOrder::Stable => "attention",
                },
            ),
            S::PhoneWidth => {
                number(next_of(&PHONE_WIDTHS, config.sidebar.phone_width, forward).into())
            }
            S::ShowKeys => on(!config.show_keys),
            S::MermaidAscii => on(!config.mermaid_ascii),
            S::NameFromPrompt => on(!config.name_from_prompt),
            S::StopIdle => {
                let now = &config.sessions.stop_idle_after;
                Change::set(setting, next_named(&SessionSettings::CHOICES, now, forward))
            }
            S::RestartSpacing => {
                let now = config.sessions.restart_spacing_ms;
                let spacing = next_of(&SessionSettings::SPACINGS, now, forward);
                number(i64::try_from(spacing).unwrap_or(i64::MAX))
            }
            S::ResumeReported => on(!config.resume_reported_agents),
            S::ShellMode => {
                let now = shell_mode(config.terminal.shell_mode);
                Change::set(setting, next_named(&SHELL_MODES, now, forward))
            }
            S::NewCwd => {
                // A directory of the user's goes on to the first.
                let now = String::from(config.terminal.new_cwd.clone());
                Change::set(setting, next_named(&NEW_CWDS, &now, forward))
            }
            S::Scrollback => {
                let lines = next_of(&SCROLLBACK, config.scrollback_lines, forward);
                number(i64::try_from(lines).unwrap_or(i64::MAX))
            }
            S::RestoreScreens => on(!config.sessions.restore_screens),
            S::RemoveEmptied => {
                let now = config.worktrees.remove_emptied.name();
                Change::set(setting, next_named(&EmptiedWorktree::CHOICES, now, forward))
            }
            S::MouseCapture => on(!config.mouse.capture),
            S::CopyOnSelect => on(!config.mouse.copy_on_select),
            S::ScrollLines => {
                number(next_of(&SCROLL_LINES, config.mouse.scroll_lines, forward).into())
            }
            S::Scrollbars => on(!config.mouse.scrollbars),
            S::AttachCapture => on(!config.mouse.attach_capture),
            S::ProgramsCopy => on(!config.clipboard.allow_programs),
            // One the list doesn't have, like bypassing the checks, goes on
            // to the first.
            S::TaskPermissions => {
                let now = &config.tasks.permission_mode;
                let modes = TaskSettings::PERMISSION_MODES;
                Change::set(setting, next_named(&modes, now, forward))
            }
            S::TaskBudget => money(config.tasks.max_budget_usd, &TASK_BUDGETS),
            S::DailyBudget => money(config.tasks.daily_budget_usd, &DAILY_BUDGETS),
            S::Distill => on(!config.memory.distill),
            S::DistillBudget => money(config.memory.distill_budget_usd, &DISTILL_BUDGETS),
            S::Embeddings => on(!config.memory.embeddings),
            S::Rerank => on(!config.memory.rerank),
            S::NotifyCommand
            | S::Separator
            | S::WindowTitle
            | S::NewSession
            | S::Shell
            | S::WorktreeBase
            | S::WorktreeDirectory
            | S::DistillModel => return Outcome::Stay,
        };
        Outcome::Change(change)
    }

    /// Enter on search by meaning: get the model ready, once it's on.
    fn prepare(&mut self) -> Outcome {
        match self.config() {
            Some(config) if config.memory.embeddings => Outcome::Prepare,
            Some(_) => {
                self.problem = Some("search by meaning is off: space turns it on".to_string());
                Outcome::Stay
            }
            None => Outcome::Stay,
        }
    }
}

impl Default for SettingsView {
    fn default() -> SettingsView {
        SettingsView::new()
    }
}

/// How far PgUp and PgDn move the bar.
const PAGE: isize = 10;

/// The theme for one side of the appearance after `now`, or before it,
/// going round from the one the theme gives that side, which takes the
/// line out, through every theme.
fn side_theme(setting: Setting, now: Option<ThemeName>, forward: bool) -> Change {
    let themes: Vec<ThemeName> = ThemeName::all().collect();
    let at = match now {
        None => 0,
        Some(theme) => theme.position() + 1,
    };
    match round(Some(at), themes.len() + 1, forward) {
        0 => Change::default(setting),
        at => Change::set(setting, themes[at - 1].name()),
    }
}

/// The shell mode as the file writes it.
fn shell_mode(mode: ShellMode) -> &'static str {
    match mode {
        ShellMode::Auto => SHELL_MODES[0],
        ShellMode::Login => SHELL_MODES[1],
        ShellMode::NonLogin => SHELL_MODES[2],
    }
}

/// What a setting typed in says now, for its box to start with.
fn typed_text(setting: Setting, config: &Config) -> String {
    match setting {
        S::NotifyCommand => config.notify_command.clone().unwrap_or_default(),
        S::Separator => config.tab_bar.separator.clone(),
        S::WindowTitle => config.window.title.clone(),
        S::NewSession => config.new_session.clone(),
        S::Shell => config.terminal.default_shell.clone(),
        S::WorktreeBase => config.worktrees.base.clone().unwrap_or_default(),
        S::WorktreeDirectory => (config.worktrees.directory.as_ref())
            .map(|directory| directory.display().to_string())
            .unwrap_or_default(),
        S::DistillModel => config.memory.distill_model.clone(),
        _ => String::new(),
    }
}

/// Whether `query` finds a key, or a key of the user's own, by its `name`,
/// what it `does` or its `chords`: each word of it the name's letters, in
/// order, or in what it does, or one of the keys as the view or the file
/// writes it. Nothing typed finds every one.
fn finds(query: &str, name: &str, does: &str, chords: &[Chord]) -> bool {
    query.split_whitespace().all(|word| {
        letters_in(word, name).is_some()
            || does.to_lowercase().contains(&word.to_lowercase())
            || (chords.iter()).any(|chord| chord.label() == word || chord.config() == word)
    })
}

/// The keys while the view is open, for what the bar is on and what it's
/// in the middle of.
pub fn hints(view: &SettingsView) -> &'static [(&'static str, &'static str)] {
    match &view.editing {
        Some(Editing::Text(_)) => &[("enter", "save"), ("esc", "leave it")],
        Some(Editing::Capture { .. }) => &[("a key", "give it"), ("esc", "leave it")],
        Some(Editing::Taking { .. }) => &[("enter", "take it"), ("esc", "leave it")],
        Some(Editing::Filter) => &[("enter", "keep it"), ("↑/↓", "move"), ("esc", "show all")],
        None => match view.row() {
            None if view.filter.is_some() => &[("/", "filter"), ("esc", "show all")],
            None => &[("tab", "next tab"), ("esc", "close")],
            Some(Row::Hooks(_)) => &[
                ("space", "put in, update or take out"),
                ("tab", "next tab"),
                ("j/k", "move"),
                ("esc", "close"),
            ],
            Some(Row::Key(_)) if view.filter.is_some() => &[
                ("enter", "press its key"),
                ("a", "add one"),
                ("x", "none"),
                ("del", "default"),
                ("/", "filter"),
                ("esc", "show all"),
            ],
            // Without `tab`, which the other tabs say, to fit 80 columns.
            Some(Row::Key(_)) => &[
                ("enter", "press its key"),
                ("a", "add one"),
                ("x", "none"),
                ("del", "default"),
                ("/", "filter"),
                ("esc", "close"),
            ],
            Some(Row::Setting(Setting::Embeddings)) => &[
                ("space", "change"),
                ("enter", "get the model"),
                ("del", "default"),
                ("tab", "next tab"),
                ("esc", "close"),
            ],
            Some(Row::Setting(setting)) if setting.typed() => &[
                ("enter", "type it"),
                ("del", "default"),
                ("tab", "next tab"),
                ("j/k", "move"),
                ("esc", "close"),
            ],
            Some(Row::Setting(_)) => &[
                ("space", "change"),
                ("del", "default"),
                ("tab", "next tab"),
                ("j/k", "move"),
                ("esc", "close"),
            ],
        },
    }
}

/// What the models line says of how the models stand.
fn model_says(status: Option<&Status>, on: bool) -> (String, Option<bool>) {
    let Some(status) = status else {
        return ("the daemon didn't say".to_string(), None);
    };
    let mb = |bytes: u64| bytes / 1_000_000;
    if let Some(doing) = &status.preparing {
        if !status.is_downloaded() {
            let got = format!("{} of {} MB", mb(status.on_disk), mb(status.size));
            return (format!("{doing} · {got}"), None);
        }
        return (format!("{doing}…"), None);
    }
    if let Some(failed) = &status.failed {
        return (format!("couldn't get them ready: {failed}"), Some(false));
    }
    if !status.is_downloaded() {
        let size = mb(status.size);
        return match on {
            true => (
                format!("not downloaded: enter gets them ({size} MB)"),
                Some(false),
            ),
            false => (format!("not downloaded ({size} MB)"), None),
        };
    }
    match (on, status.loaded) {
        (_, true) => ("downloaded, loaded in the daemon".to_string(), Some(true)),
        (true, false) => (
            "downloaded; loads at the next search".to_string(),
            Some(true),
        ),
        (false, false) => ("downloaded, not loaded".to_string(), None),
    }
}

/// How wide the mark and the name are, before a row's value.
const NAME_WIDTH: usize = 24;

/// The lines of a tab's rows and what's around them, with the place of
/// the row each line is among the tab's, if it's one: what a click on it
/// picks.
#[derive(Default)]
struct Body {
    lines: Vec<Line<'static>>,
    rows: Vec<Option<usize>>,
}

impl Body {
    /// A line that's no row: a heading, a blank or what's said.
    fn say(&mut self, line: Line<'static>) {
        self.lines.push(line);
        self.rows.push(None);
    }

    /// The line of the tab's row at `row`.
    fn row(&mut self, row: usize, line: Line<'static>) {
        self.lines.push(line);
        self.rows.push(Some(row));
    }
}

/// Where [`draw`] puts the view, and what goes there.
struct Laid {
    panel: Rect,
    head: Vec<Line<'static>>,
    head_area: Rect,
    body: Body,
    body_area: Rect,
    /// How many of the body's lines are scrolled off its top.
    scroll: usize,
    /// The body's line the bar is on.
    selected: Option<usize>,
    foot: Vec<Line<'static>>,
    foot_area: Rect,
}

/// The head's line of tabs, and the one the keys' filter is on.
const TABS_LINE: u16 = 1;
const FILTER_LINE: u16 = 2;

/// What's between two tabs' names.
const TAB_GAP: &str = "  ";

/// The view's frame, and whether it's drawn: in a theme with no panel
/// color, a border sets the view apart.
fn frame_of(theme: &Theme) -> (Block<'static>, bool) {
    let framed = theme.panel == Color::Reset;
    let block = if framed {
        Block::bordered().border_style(Style::new().fg(theme.rule))
    } else {
        Block::new()
    };
    (
        block.style(Style::new().bg(theme.panel).fg(theme.text)),
        framed,
    )
}

/// Lays the view out over the middle of `area`: the file and the tabs at
/// its top, what's said of the last thing asked at its bottom, and the
/// tab's rows between, scrolled to keep the bar in sight.
fn lay_out(view: &SettingsView, theme: &Theme, area: Rect) -> Laid {
    let width = area.width.saturating_sub(4).clamp(40.min(area.width), 110);
    let head = head(view, theme);
    let body = body(view, theme);
    // Inside its frame and margins, or its margins alone.
    let foot = foot(view, theme, usize::from(width.saturating_sub(4)));
    let selected = view
        .at()
        .and_then(|at| body.rows.iter().position(|row| *row == Some(at)));
    let tall = head.len() + body.lines.len() + foot.len() + 2;
    let height = u16::try_from(tall).unwrap_or(u16::MAX).min(area.height);
    let top = if area.height > height { 1 } else { 0 };
    let panel = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + top,
        width,
        height,
    );
    let (block, framed) = frame_of(theme);
    let inside = block
        .inner(panel)
        .inner(Margin::new(if framed { 1 } else { 2 }, 1));
    let head_height = (head.len() as u16).min(inside.height);
    let foot_height = (foot.len() as u16).min(inside.height - head_height);
    let room = inside.height - head_height - foot_height;
    let at = |y: u16, height: u16| Rect::new(inside.x, y, inside.width, height);
    // Taller than the room, the rows scroll to keep the one the bar is on
    // in sight, and what's said under it.
    let room_rows = usize::from(room);
    let scroll = selected
        .map_or(0, |at| (at + 3).saturating_sub(room_rows))
        .min(body.lines.len().saturating_sub(room_rows));
    Laid {
        panel,
        head,
        head_area: at(inside.y, head_height),
        body,
        body_area: at(inside.y + head_height, room),
        scroll,
        selected,
        foot,
        foot_area: at(inside.y + inside.height - foot_height, foot_height),
    }
}

/// Draws the view over the middle of `area`, the rest dimmed behind it.
pub fn draw(frame: &mut Frame, view: &SettingsView, theme: &Theme, area: Rect) {
    frame
        .buffer_mut()
        .set_style(area, Style::new().add_modifier(Modifier::DIM));
    let laid = lay_out(view, theme, area);
    frame.render_widget(Clear, laid.panel);
    frame.render_widget(frame_of(theme).0, laid.panel);
    frame.render_widget(Paragraph::new(laid.head), laid.head_area);
    frame.render_widget(Paragraph::new(laid.foot), laid.foot_area);
    let scroll = u16::try_from(laid.scroll).unwrap_or(u16::MAX);
    let body = Paragraph::new(laid.body.lines).scroll((scroll, 0));
    frame.render_widget(body, laid.body_area);
    // The cursor, in a setting being typed, or in the keys' filter.
    let cursor = match &view.editing {
        Some(Editing::Text(input)) => laid.selected.and_then(|line| {
            let row = u16::try_from(line.checked_sub(laid.scroll)?).ok()?;
            Some((laid.body_area, row, NAME_WIDTH + input.cursor()))
        }),
        Some(Editing::Filter) => (view.filter.as_ref()).map(|filter| {
            (
                laid.head_area,
                FILTER_LINE,
                FILTER_PROMPT.len() + filter.cursor(),
            )
        }),
        _ => None,
    };
    if let Some((area, row, column)) = cursor
        && let Ok(column) = u16::try_from(column)
        && row < area.height
        && column < area.width
    {
        frame.set_cursor_position(Position::new(area.x + column, area.y + row));
    }
}

/// What's at `(column, row)` on the screen, with the view drawn over the
/// middle of `area` as [`draw`] draws it: a tab's name, a row, or
/// anything else.
pub fn hit(view: &SettingsView, theme: &Theme, area: Rect, column: u16, row: u16) -> Spot {
    let laid = lay_out(view, theme, area);
    let on = Position::new(column, row);
    if laid.head_area.contains(on) && row - laid.head_area.y == TABS_LINE {
        let offset = usize::from(column - laid.head_area.x);
        return tab_at(offset).map_or(Spot::Elsewhere, Spot::Tab);
    }
    if laid.body_area.contains(on) {
        let line = laid.scroll + usize::from(row - laid.body_area.y);
        if let Some(Some(at)) = laid.body.rows.get(line) {
            return Spot::Row(*at);
        }
    }
    Spot::Elsewhere
}

/// The tab whose name is `offset` columns into the line of tabs.
fn tab_at(offset: usize) -> Option<usize> {
    let mut start = 0;
    for (at, tab) in TABS.iter().enumerate() {
        let end = start + tab.name.chars().count();
        if (start..end).contains(&offset) {
            return Some(at);
        }
        start = end + TAB_GAP.len();
    }
    None
}

/// What the keys' filter is written after.
const FILTER_PROMPT: &str = "/ ";

/// The view's top: where the file is, and the tabs, the one in front
/// marked.
fn head(view: &SettingsView, theme: &Theme) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let path = view.current.as_ref().map_or(String::new(), |current| {
        crate::shell::home_relative(&current.path)
    });
    let mut tabs = Vec::new();
    for (at, tab) in TABS.iter().enumerate() {
        if at > 0 {
            tabs.push(Span::styled(TAB_GAP, muted));
        }
        let style = match at == view.tab {
            true => Style::new()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            false => muted,
        };
        tabs.push(Span::styled(tab.name, style));
    }
    let mut lines = vec![
        Line::from(vec![
            Span::styled("settings", bold),
            Span::styled(format!("  {path}"), muted),
        ]),
        Line::from(tabs),
    ];
    // The keys' filter, and how many keys it finds.
    if let Some(filter) = view.filter.as_ref().filter(|_| view.tab == KEYS_TAB) {
        let found = view.keys.len();
        let every = KeyId::all().count();
        lines.push(Line::from(vec![
            Span::styled(
                FILTER_PROMPT,
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(filter.text().to_string(), Style::new().fg(theme.text)),
            Span::styled(format!("  {found} of {every}"), muted),
        ]));
    }
    lines
}

/// The view's bottom: what's said of the last thing asked, or what came of
/// it, or of the key pressed that something else has, in lines `width`
/// wide.
fn foot(view: &SettingsView, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let (said, color) = match (&view.editing, &view.problem) {
        (
            Some(Editing::Taking {
                chord,
                rebinding,
                action,
            }),
            _,
        ) => {
            let given = view.row().map_or("", |row| match row {
                Row::Key(key) => key.id(),
                _ => "",
            });
            let said = match (rebinding.taken_from, action) {
                (Some(from), None) => format!(
                    "{chord} is {}'s: enter takes it from it, esc leaves it",
                    from.id()
                ),
                (Some(from), Some(action)) => format!(
                    "{chord} is {}'s, and runs {action}, which would run from : alone: \
                     enter takes it, esc leaves it",
                    from.id()
                ),
                (None, Some(action)) => format!(
                    "{chord} runs {action}, which would run from : alone once {given} has it: \
                     enter gives it all the same, esc leaves it"
                ),
                (None, None) => String::new(),
            };
            (said, theme.waiting)
        }
        (_, Some(problem)) => (problem.clone(), theme.failed),
        _ => match &view.note {
            Some(note) => (note.clone(), theme.muted),
            None => return Vec::new(),
        },
    };
    let lines = wrapped(&said, width).into_iter();
    let lines = lines.map(|line| Line::styled(line, Style::new().fg(color)));
    std::iter::once(Line::from("")).chain(lines).collect()
}

/// `text` in lines at most `width` characters long, broken between words,
/// or in a word longer than that.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let fits = line.chars().count() + 1 + word.chars().count() <= width;
        if !line.is_empty() && !fits {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        while line.chars().count() > width {
            let rest: String = line.chars().skip(width).collect();
            line = line.chars().take(width).collect();
            lines.push(std::mem::replace(&mut line, rest));
        }
    }
    lines.push(line);
    lines
}

/// What a setting's row shows: whether it's on, if it's a switch; its
/// value; what it does or how it stands; and whether it does nothing as
/// the rest are set.
struct Shown {
    on: Option<bool>,
    value: String,
    about: String,
    dim: bool,
}

/// What `setting`'s row shows, with `config`.
fn shown(setting: Setting, config: &Config) -> Shown {
    let switch = |on: bool, about: &str| Shown {
        on: Some(on),
        value: if on { "on" } else { "off" }.to_string(),
        about: about.to_string(),
        dim: false,
    };
    let choice = |value: String, about: &str| Shown {
        on: None,
        value,
        about: about.to_string(),
        dim: false,
    };
    let or_none = |text: &str, none: &str| match text.is_empty() {
        true => none.to_string(),
        false => text.to_string(),
    };
    let memory_on = crate::memory::enabled(config);
    let shown = match setting {
        S::Notify => switch(config.notify, "tell you when a session needs you"),
        S::NotifyAfter => choice(
            wait_text(config.notifications.after_secs),
            "once it has needed you this long: ←/→",
        ),
        S::UnfocusedOnly => switch(
            config.notifications.unfocused_only,
            "only while crystal's terminal hasn't the focus",
        ),
        S::NotifyCommand => choice(
            or_none(config.notify_command.as_deref().unwrap_or(""), "none"),
            "runs in place of the desktop's, as for a phone",
        ),
        S::Sound => switch(config.sound.enabled, "a chime at the same moments"),
        S::ConfirmQuit => switch(
            config.confirm_quit,
            "q asks first; the sessions keep running either way",
        ),
        S::UpdateCheck => switch(
            config.update.check,
            "once a day, as the TUI opens, for a newer crystal",
        ),
        S::KeepEvents => choice(
            match config.events.keep_days {
                0 => "for ever".to_string(),
                1 => "a day".to_string(),
                days => format!("{days} days"),
            },
            "what the event log and the timeline keep: ←/→",
        ),
        S::HideDrafts => Shown {
            dim: !crate::forge::enabled(config),
            ..switch(
                config.forge.hide_draft_prs,
                "leave drafts out of O, / and the tab bar's count",
            )
        },
        S::Theme => {
            let whose = if config.colors.is_empty() {
                "the TUI's colors"
            } else {
                "under your [colors]"
            };
            let themes = ThemeName::all().count();
            let at = config.theme.position() + 1;
            choice(
                config.theme.name().to_string(),
                &format!("{whose}: ←/→ ({at} of {themes})"),
            )
        }
        S::AutoSwitch => switch(
            config.appearance.auto_switch,
            &format!(
                "{} when it's light, {} when dark",
                theme::for_appearance(config, Appearance::Light).name(),
                theme::for_appearance(config, Appearance::Dark).name()
            ),
        ),
        S::LightTheme | S::DarkTheme => {
            let (named, appearance) = match setting {
                S::LightTheme => (config.appearance.light_theme, Appearance::Light),
                _ => (config.appearance.dark_theme, Appearance::Dark),
            };
            let about = match named {
                Some(_) => "←/→; del gives it the theme's".to_string(),
                None => format!(
                    "the theme's: {}: ←/→",
                    theme::for_appearance(config, appearance).name()
                ),
            };
            Shown {
                dim: !config.appearance.auto_switch,
                ..choice(
                    named.map_or("auto".to_string(), |theme| theme.name().into()),
                    &about,
                )
            }
        }
        S::TabBar => choice(
            match config.tab_bar.position {
                BarPosition::Top => "top",
                BarPosition::Bottom => "bottom",
            }
            .to_string(),
            "above the panes, or over the footer: ←/→",
        ),
        S::HideSingleTab => switch(
            config.tab_bar.hide_when_single,
            "while there's only the one tab",
        ),
        S::Separator => Shown {
            dim: config.tab_bar.right.is_empty(),
            ..choice(
                format!("{:?}", config.tab_bar.separator),
                "between what [tab_bar] right shows: enter",
            )
        },
        S::WindowTitle => choice(
            or_none(&config.window.title, "left alone"),
            "what your terminal is titled: enter",
        ),
        S::SidebarWidth => choice(
            format!("{} columns", config.sidebar.width),
            "until you resize it: ←/→",
        ),
        S::SidebarFolded => switch(config.sidebar.folded, "as the TUI starts, until \\"),
        S::Fold => choice(
            match config.sidebar.fold {
                Fold::Marks => "marks",
                Fold::Hidden => "nothing",
            }
            .to_string(),
            "each session's mark, or nothing: ←/→",
        ),
        S::PinNeedsYou => switch(
            config.sidebar.needs_you,
            "what needs you, from every tab, at its top",
        ),
        S::SidebarOrder => switch(
            config.sidebar.order == SidebarOrder::Attention,
            "its project to the top, it first in its worktree",
        ),
        S::PhoneWidth => choice(
            match config.sidebar.phone_width {
                0 => "never".to_string(),
                columns => format!("{columns} columns"),
            },
            "or narrower, as on a phone: the sidebar or the pane: ←/→",
        ),
        S::ShowKeys => switch(
            config.show_keys,
            "each key's command at the footer's right, for a screen share",
        ),
        S::MermaidAscii => switch(
            config.mermaid_ascii,
            "+ - | > v rather than box drawing, for fonts without it",
        ),
        S::NewSession => choice(
            config.new_session.clone(),
            "what the new-session panel offers first: enter",
        ),
        S::NameFromPrompt => switch(
            config.name_from_prompt,
            "a session you didn't name, for what it's asked",
        ),
        S::StopIdle => {
            let limit = config.sessions.idle_limit();
            Shown {
                on: Some(limit.is_some()),
                value: match limit {
                    Some(_) => format!("after {}", config.sessions.stop_idle_after),
                    None => "off".to_string(),
                },
                about: "at their prompt, unwatched: they start again where they were".to_string(),
                dim: false,
            }
        }
        S::RestartSpacing => {
            let spacing = config.sessions.restart_spacing_ms;
            Shown {
                on: Some(spacing > 0),
                ..choice(
                    match spacing {
                        0 => "all at once".to_string(),
                        ms => format!("{ms}ms apart"),
                    },
                    "the agents a crash or a reboot starts again: ←/→",
                )
            }
        }
        S::ResumeReported => switch(
            config.resume_reported_agents,
            "after a restart, as an agent said, or its hooks named",
        ),
        S::Shell => choice(
            or_none(&config.terminal.default_shell, "$SHELL"),
            "a new terminal's: a program, not a command line: enter",
        ),
        S::ShellMode => choice(
            shell_mode(config.terminal.shell_mode).to_string(),
            match config.terminal.shell_mode {
                ShellMode::Auto => "with -l on a Mac, not elsewhere: ←/→",
                ShellMode::Login => "always with -l, which reads your profile: ←/→",
                ShellMode::NonLogin => "never with -l: ←/→",
            },
        ),
        S::NewCwd => choice(
            String::from(config.terminal.new_cwd.clone()),
            match &config.terminal.new_cwd {
                NewCwd::Follow => "the selected session's directory: ←/→",
                NewCwd::Home => "your home directory: ←/→",
                NewCwd::Current => "where you started crystal: ←/→",
                NewCwd::Path(_) => "a directory of yours, from the file: ←/→",
            },
        ),
        S::Scrollback => choice(
            format!("{} lines", config.scrollback_lines),
            "kept as they scroll off, by running sessions too: ←/→",
        ),
        S::RestoreScreens => switch(
            config.sessions.restore_screens,
            "after a crash or a reboot, kept in the database: it may hold secrets",
        ),
        S::WorktreeBase => choice(
            or_none(
                config.worktrees.base.as_deref().unwrap_or(""),
                "origin's default",
            ),
            "where new worktrees' branches start: enter",
        ),
        S::WorktreeDirectory => choice(
            (config.worktrees.directory.as_ref())
                .map_or("beside the project".to_string(), |directory| {
                    directory.display().to_string()
                }),
            "where new worktrees go, from / or ~: enter",
        ),
        S::RemoveEmptied => choice(
            config.worktrees.remove_emptied.name().to_string(),
            match config.worktrees.remove_emptied {
                EmptiedWorktree::Ask => "a linked one its last session is killed from: ←/→",
                EmptiedWorktree::Always => {
                    "without asking, unless archived sessions ran there: ←/→"
                }
                EmptiedWorktree::Never => "keep it, without asking: ←/→",
            },
        ),
        S::MouseCapture => switch(
            config.mouse.capture,
            "off, your terminal selects as it would without crystal",
        ),
        S::CopyOnSelect => Shown {
            // What crystal attach selects too, while it takes the mouse.
            dim: !config.mouse.capture && !config.mouse.attach_capture,
            ..switch(
                config.mouse.copy_on_select,
                "as you let go; off, it waits in copy mode for y",
            )
        },
        S::ScrollLines => {
            let lines = config.mouse.scroll_lines;
            let noun = if lines == 1 { "line" } else { "lines" };
            Shown {
                // The wheel scrolls the attach's history too, while it
                // takes the mouse.
                dim: !config.mouse.capture && !config.mouse.attach_capture,
                ..choice(
                    format!("{lines} {noun}"),
                    "a notch of the wheel, through a pane's history: ←/→",
                )
            }
        }
        S::Scrollbars => Shown {
            dim: !config.mouse.capture,
            ..switch(
                config.mouse.scrollbars,
                "beside each pane, a column of its own: drag one to scroll",
            )
        },
        S::AttachCapture => switch(
            config.mouse.attach_capture,
            "crystal attach's wheel scrolls its history, a drag selects",
        ),
        S::ProgramsCopy => switch(
            config.clipboard.allow_programs,
            "what Claude Code, vim or tmux copy (OSC 52)",
        ),
        S::TaskPermissions => {
            let tasks = &config.tasks;
            let about = match tasks.allowed_tools.len() {
                0 => "what they may do without asking: ←/→".to_string(),
                1 => "and the rule in [tasks] allowed_tools: ←/→".to_string(),
                rules => format!("and the {rules} rules in [tasks] allowed_tools: ←/→"),
            };
            choice(tasks.permission_mode.clone(), &about)
        }
        S::TaskBudget | S::DailyBudget => {
            let (usd, about) = match setting {
                S::TaskBudget => (
                    config.tasks.max_budget_usd,
                    "the most one run spends; one that reaches it fails: ←/→",
                ),
                _ => (
                    config.tasks.daily_budget_usd,
                    "every task's together; past it, none runs till tomorrow: ←/→",
                ),
            };
            let value = match cents(usd) {
                0 => "no limit".to_string(),
                cents => dollars(cents),
            };
            choice(value, about)
        }
        S::Distill => Shown {
            dim: !memory_on,
            ..switch(
                config.memory.distill,
                "a model reads what a closed task or an archived session did",
            )
        },
        S::DistillModel => Shown {
            dim: !memory_on || !config.memory.distill,
            ..choice(
                config.memory.distill_model.clone(),
                "as claude --model takes it: enter",
            )
        },
        S::DistillBudget => Shown {
            dim: !memory_on || !config.memory.distill,
            ..choice(
                dollars(cents(config.memory.distill_budget_usd)),
                "the most it spends on a task: ←/→",
            )
        },
        S::Embeddings => Shown {
            dim: !memory_on,
            ..switch(config.memory.embeddings, "jina v5, on this machine")
        },
        S::Rerank => Shown {
            dim: !memory_on || !config.memory.embeddings,
            ..switch(
                config.memory.rerank,
                "jina's reranker reads the best of a search again",
            )
        },
    };
    let quiet = match setting {
        S::NotifyAfter | S::UnfocusedOnly | S::NotifyCommand => !config.notify,
        _ => false,
    };
    Shown {
        dim: shown.dim || quiet,
        ..shown
    }
}

/// A row: its mark, its name, its value and what it says, as the bar
/// being on it and its doing nothing would have it.
fn row_line(
    theme: &Theme,
    mark: (&'static str, Color),
    name: &str,
    value: Span<'static>,
    about: String,
    (selected, dim): (bool, bool),
) -> Line<'static> {
    let text = if dim { theme.muted } else { theme.text };
    let name_width = NAME_WIDTH - 2;
    let line = Line::from(vec![
        Span::styled(mark.0, Style::new().fg(mark.1)),
        Span::styled(format!("{name:<name_width$}"), Style::new().fg(text)),
        value,
        Span::styled(about, Style::new().fg(theme.muted)),
    ]);
    if selected {
        line.style(theme.selection)
    } else {
        line
    }
}

/// The value column: as wide as the longest theme's name, and a space
/// after a value longer than that.
fn value_span(value: &str, theme: &Theme) -> Span<'static> {
    Span::styled(format!("{value:<16} "), Style::new().fg(theme.accent))
}

/// The tab's rows, under their headings, each with what it does or how
/// it stands.
fn body(view: &SettingsView, theme: &Theme) -> Body {
    let muted = Style::new().fg(theme.muted);
    let mut body = Body::default();
    let Some(current) = &view.current else {
        body.say(Line::from(""));
        body.say(Line::styled("reading the settings…", muted));
        return body;
    };
    // The agents' own settings, whatever crystal's file says.
    if view.tab == HOOKS_TAB {
        hooks_lines(view, theme, &mut body);
        return body;
    }
    let config = match &current.config {
        Ok(config) => config,
        Err(why) => {
            let said = format!("the file can't be read: {why}");
            body.say(Line::from(""));
            body.say(Line::styled(said, Style::new().fg(theme.failed)));
            return body;
        }
    };
    if view.tab == KEYS_TAB {
        key_lines(view, config, theme, &mut body);
        return body;
    }
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let at = view.at();
    let mut row = 0;
    for (heading, settings) in TABS[view.tab].sections {
        body.say(Line::from(""));
        body.say(Line::styled(*heading, bold));
        for &setting in *settings {
            let selected = at == Some(row);
            body.row(row, setting_line(view, setting, config, theme, selected));
            row += 1;
        }
        for line in after_section(view, settings, config, theme) {
            body.say(line);
        }
    }
    body
}

/// The line of `setting`'s row, with the bar on it or not.
fn setting_line(
    view: &SettingsView,
    setting: Setting,
    config: &Config,
    theme: &Theme,
    selected: bool,
) -> Line<'static> {
    let shown = shown(setting, config);
    let mark = match shown.on {
        Some(true) => ("● ", theme.done),
        Some(false) => ("○ ", theme.muted),
        None => ("  ", theme.muted),
    };
    let (value, about) = match &view.editing {
        Some(Editing::Text(input)) if selected => (
            Span::styled(input.text().to_string(), Style::new().fg(theme.text)),
            String::new(),
        ),
        _ => (value_span(&shown.value, theme), shown.about),
    };
    row_line(
        theme,
        mark,
        setting.name(),
        value,
        about,
        (selected, shown.dim),
    )
}

/// What's said after a section's rows: how the models stand, and the
/// plugins it needs that are off.
fn after_section(
    view: &SettingsView,
    settings: &[Setting],
    config: &Config,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let mut lines = Vec::new();
    let detail = |label: &str, said: String, good: Option<bool>| {
        let color = match good {
            Some(false) => theme.failed,
            _ => theme.muted,
        };
        Line::from(vec![
            Span::raw(format!("    {label:<20}")),
            Span::styled(said, Style::new().fg(color)),
        ])
    };
    if settings.contains(&Setting::Embeddings) {
        let memory = &config.memory;
        let (said, good) = model_says(view.model(), memory.embeddings);
        lines.push(detail("models", said, good));
        if let Some(status) = view.model()
            && (memory.embeddings || status.is_downloaded())
        {
            let said = format!(
                "{} of {} have their vector",
                status.embedded, status.entries
            );
            let good = (status.entries > 0).then_some(status.embedded == status.entries);
            lines.push(detail("entries", said, good.filter(|all| *all)));
        }
        if !crate::memory::enabled(config) {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                "the memory plugin is off, so these do nothing: X switches it on",
                muted,
            ));
        }
    }
    if settings.contains(&Setting::HideDrafts) && !crate::forge::enabled(config) {
        lines.push(Line::styled(
            "the github plugin is off, so this does nothing: X switches it on",
            muted,
        ));
    }
    if settings.contains(&Setting::TaskPermissions) {
        let said = match config.tasks.allow_bypass {
            true => "[tasks] allow_bypass lets the file say bypassPermissions",
            false => "bypassPermissions is the file's to say, with [tasks] allow_bypass",
        };
        lines.push(Line::from(""));
        lines.push(Line::styled(said, muted));
    }
    lines
}

/// The agents' hooks' tab: each agent installed here that crystal can hook,
/// with how its hooks stand.
fn hooks_lines(view: &SettingsView, theme: &Theme, body: &mut Body) {
    let muted = Style::new().fg(theme.muted);
    body.say(Line::from(""));
    body.say(Line::styled(
        "crystal's hooks in each agent's own settings, for it to say what it's doing",
        muted,
    ));
    body.say(Line::from(""));
    if view.hooked().is_empty() {
        body.say(Line::styled(
            "none of the agents crystal can hook is installed here",
            muted,
        ));
    }
    let at = view.at();
    for (row, &(agent, standing)) in view.hooked().iter().enumerate() {
        let (mark, about) = match standing {
            Standing::Installed => (
                ("● ", theme.done),
                "it says what it's doing: space takes them out",
            ),
            Standing::OutOfDate => (
                ("○ ", theme.muted),
                "another crystal's, or older: space brings them up to date",
            ),
            Standing::NotInstalled => (
                ("○ ", theme.muted),
                "space puts crystal's hooks in its settings",
            ),
        };
        let value = value_span(standing.word(), theme);
        let flags = (at == Some(row), false);
        let line = row_line(theme, mark, agent.name(), value, about.into(), flags);
        body.row(row, line);
    }
}

/// The keys' tab: every key `[keys]` gives, or those the filter finds,
/// under the heading of where it works, then the user's own, which the
/// file changes.
fn key_lines(view: &SettingsView, config: &Config, theme: &Theme, body: &mut Body) {
    let muted = Style::new().fg(theme.muted);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let keymap = Keymap::new(&config.keys).unwrap_or_default();
    let written = |chords: &[Chord], direct: bool| {
        let keys: Vec<String> = chords
            .iter()
            .map(|chord| match direct && keymap.is_direct(*chord) {
                true => format!("direct+{}", chord.label()),
                false => chord.label(),
            })
            .collect();
        match keys.is_empty() {
            true => "none".to_string(),
            false => keys.join(" "),
        }
    };
    body.say(Line::from(""));
    body.say(Line::styled(
        "a key you press for a command is checked as the file's are; • the file gives it",
        muted,
    ));
    if view.keys.is_empty() {
        body.say(Line::from(""));
        body.say(Line::styled(
            "the filter finds no key's command: esc shows them all",
            muted,
        ));
    }
    let at = view.at();
    let mut heading = "";
    for (row, &key) in view.keys.iter().enumerate() {
        if key_heading(key) != heading {
            heading = key_heading(key);
            body.say(Line::from(""));
            body.say(Line::styled(heading, bold));
        }
        let selected = at == Some(row);
        let given = config.keys.bindings.contains_key(key.id());
        let mark = match given {
            true => ("• ", theme.accent),
            false => ("  ", theme.muted),
        };
        let plugin_off = key
            .plugin()
            .is_some_and(|plugin| !crate::plugins::enabled(config, plugin));
        let value = match &view.editing {
            Some(Editing::Capture { add }) if selected => Span::styled(
                match add {
                    true => "press one to add…".to_string(),
                    false => "press a key…".to_string(),
                },
                Style::new().fg(theme.waiting),
            ),
            Some(Editing::Taking { chord, .. }) if selected => Span::styled(
                format!("{:<16} ", chord.label()),
                Style::new().fg(theme.waiting),
            ),
            _ => {
                let direct = matches!(key, KeyId::Command(_));
                value_span(&written(&keymap.keys_of(key), direct), theme)
            }
        };
        let about = match plugin_off {
            true => format!("{} (its plugin is off)", key.does()),
            false => key.does().to_string(),
        };
        let flags = (selected, plugin_off);
        body.row(row, row_line(theme, mark, key.id(), value, about, flags));
    }
    let query = view.filter.as_ref().map_or("", |filter| filter.text());
    let own: Vec<_> = (keymap.custom().iter())
        .filter(|(command, chords)| finds(query, command.label(), &command.command, chords))
        .collect();
    if !own.is_empty() {
        body.say(Line::from(""));
        body.say(Line::styled("Your own, [[keys.command]] in the file", bold));
        for (command, chords) in own {
            body.say(Line::from(vec![
                Span::raw(format!("  {:<22}", command.label())),
                value_span(&written(chords, true), theme),
                Span::styled(command.kind.name(), muted),
            ]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::keymap::Command;

    fn press(view: &mut SettingsView, code: KeyCode) -> Outcome {
        view.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn view_of(config: Config, model: Option<Status>) -> SettingsView {
        let mut view = SettingsView::new();
        view.set_current(Current {
            path: PathBuf::from("/home/ann/.config/crystal/config.toml"),
            config: Ok(config),
            model,
            integrations: Vec::new(),
        });
        view
    }

    fn status() -> Status {
        Status {
            on_disk: 0,
            size: 134_000_000,
            ..Status::default()
        }
    }

    fn text(view: &SettingsView) -> String {
        let theme = Theme::new(ThemeName::DARK, true);
        let lines = head(view, &theme)
            .into_iter()
            .chain(body(view, &theme).lines)
            .chain(foot(view, &theme, 200));
        let lines: Vec<String> = lines.map(|line| line.to_string()).collect();
        lines.join("\n")
    }

    fn change(setting: Setting, to: impl Into<Value>) -> Outcome {
        Outcome::Change(Change::set(setting, to))
    }

    fn number(setting: Setting, to: i64) -> Outcome {
        Outcome::Change(Change::set(setting, Value::Number(to)))
    }

    /// Puts the bar on `setting`, in its tab.
    fn go_to(view: &mut SettingsView, setting: Setting) {
        let tab = (0..TABS.len())
            .find(|&tab| rows(tab, &[]).contains(&Row::Setting(setting)))
            .unwrap();
        press(view, KeyCode::Char(char::from(b'1' + tab as u8)));
        while view.row() != Some(Row::Setting(setting)) {
            press(view, KeyCode::Down);
        }
    }

    #[test]
    fn space_turns_the_setting_the_bar_is_on_over() {
        let mut view = view_of(Config::default(), Some(status()));
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            change(S::Notify, false)
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(press(&mut view, KeyCode::Right), number(S::NotifyAfter, 10));
        assert_eq!(press(&mut view, KeyCode::Left), number(S::NotifyAfter, 300));
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            change(S::UnfocusedOnly, true)
        );
        let switches = [
            (S::Sound, false),
            (S::ConfirmQuit, false),
            (S::UpdateCheck, false),
            (S::HideSingleTab, true),
            (S::AutoSwitch, true),
            (S::SidebarFolded, true),
            (S::PinNeedsYou, false),
            (S::ShowKeys, true),
            (S::MermaidAscii, true),
            (S::NameFromPrompt, false),
            (S::ResumeReported, false),
            (S::RestoreScreens, true),
            (S::MouseCapture, false),
            (S::CopyOnSelect, false),
            (S::Scrollbars, false),
            (S::AttachCapture, true),
            (S::ProgramsCopy, false),
            (S::Distill, false),
            (S::Embeddings, false),
            (S::Rerank, false),
            (S::HideDrafts, true),
        ];
        for (setting, to) in switches {
            go_to(&mut view, setting);
            assert_eq!(press(&mut view, KeyCode::Char(' ')), change(setting, to));
        }
        assert_eq!(press(&mut view, KeyCode::Char(',')), Outcome::Close);
    }

    #[test]
    fn left_and_right_go_through_a_settings_choices() {
        let mut view = view_of(Config::default(), None);
        let both = |view: &mut SettingsView, setting, forward: Outcome, back: Outcome| {
            go_to(view, setting);
            assert_eq!(press(view, KeyCode::Right), forward, "{setting:?}");
            assert_eq!(press(view, KeyCode::Left), back, "{setting:?}");
        };
        both(&mut view, S::Theme, change(S::Theme, "light"), {
            // Back from the first is the last.
            change(S::Theme, ThemeName::all().last().unwrap().name())
        });
        both(
            &mut view,
            S::LightTheme,
            change(S::LightTheme, "dark"),
            change(S::LightTheme, ThemeName::all().last().unwrap().name()),
        );
        both(
            &mut view,
            S::TabBar,
            change(S::TabBar, "bottom"),
            change(S::TabBar, "bottom"),
        );
        both(
            &mut view,
            S::SidebarWidth,
            number(S::SidebarWidth, 32),
            number(S::SidebarWidth, 24),
        );
        both(
            &mut view,
            S::PhoneWidth,
            number(S::PhoneWidth, 72),
            number(S::PhoneWidth, 56),
        );
        both(
            &mut view,
            S::Fold,
            change(S::Fold, "hidden"),
            change(S::Fold, "hidden"),
        );
        both(
            &mut view,
            S::SidebarOrder,
            change(S::SidebarOrder, "stable"),
            change(S::SidebarOrder, "stable"),
        );
        both(
            &mut view,
            S::KeepEvents,
            number(S::KeepEvents, 90),
            number(S::KeepEvents, 7),
        );
        both(
            &mut view,
            S::StopIdle,
            change(S::StopIdle, "15m"),
            change(S::StopIdle, "8h"),
        );
        both(
            &mut view,
            S::RestartSpacing,
            number(S::RestartSpacing, 500),
            number(S::RestartSpacing, 100),
        );
        both(
            &mut view,
            S::ShellMode,
            change(S::ShellMode, "login"),
            change(S::ShellMode, "non_login"),
        );
        both(
            &mut view,
            S::NewCwd,
            change(S::NewCwd, "home"),
            change(S::NewCwd, "current"),
        );
        both(
            &mut view,
            S::Scrollback,
            number(S::Scrollback, 50_000),
            number(S::Scrollback, 5_000),
        );
        both(
            &mut view,
            S::RemoveEmptied,
            change(S::RemoveEmptied, "always"),
            change(S::RemoveEmptied, "never"),
        );
        both(
            &mut view,
            S::ScrollLines,
            number(S::ScrollLines, 5),
            number(S::ScrollLines, 2),
        );
        both(
            &mut view,
            S::TaskPermissions,
            change(S::TaskPermissions, "acceptEdits"),
            change(S::TaskPermissions, "plan"),
        );
        let cents = |setting, cents| Outcome::Change(Change::set(setting, Value::Cents(cents)));
        both(
            &mut view,
            S::TaskBudget,
            cents(S::TaskBudget, 1_000),
            cents(S::TaskBudget, 200),
        );
        both(
            &mut view,
            S::DailyBudget,
            cents(S::DailyBudget, 500),
            cents(S::DailyBudget, 10_000),
        );
        both(
            &mut view,
            S::DistillBudget,
            cents(S::DistillBudget, 50),
            cents(S::DistillBudget, 10),
        );
    }

    #[test]
    fn whats_said_at_the_bottom_is_broken_between_words() {
        assert_eq!(wrapped("one two three", 7), ["one two", "three"]);
        assert_eq!(wrapped("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrapped("", 4), [""]);
    }

    #[test]
    fn a_choice_goes_through_its_steps_and_round() {
        assert_eq!(next_of(&NOTIFY_AFTER, 0, true), 10);
        assert_eq!(next_of(&NOTIFY_AFTER, 300, true), 0);
        assert_eq!(next_of(&NOTIFY_AFTER, 45, true), 60);
        assert_eq!(next_of(&NOTIFY_AFTER, 45, false), 30);
        assert_eq!(next_of(&NOTIFY_AFTER, 0, false), 300);
        assert_eq!(next_of(&SCROLL_LINES, 40, true), 1);
        assert_eq!(next_of(&SessionSettings::SPACINGS, 2000, true), 0);
        assert_eq!(next_named(&NEW_CWDS, "/srv", true), "follow");
        assert_eq!(next_named(&NEW_CWDS, "follow", false), "current");
        assert_eq!(wait_text(0), "at once");
        assert_eq!(wait_text(120), "2m");
        assert_eq!(wait_text(45), "45s");
        assert_eq!(
            (dollars(25), dollars(500), dollars(1_050)),
            ("$0.25".into(), "$5".into(), "$10.50".into())
        );
        assert_eq!(cents(0.25), 25);
    }

    #[test]
    fn a_change_says_where_it_goes_in_the_file() {
        let edit = Change::set(S::AutoSwitch, true).edit();
        assert_eq!(edit.keys, ["appearance", "auto_switch"]);
        assert_eq!(edit.value.unwrap().as_bool(), Some(true));
        let edit = Change::set(S::TaskBudget, Value::Cents(250)).edit();
        assert_eq!(edit.keys, ["tasks", "max_budget_usd"]);
        assert_eq!(edit.value.unwrap().as_float(), Some(2.5));
        let edit = Change::default(S::LightTheme).edit();
        assert_eq!(edit.keys, ["appearance", "light_theme"]);
        assert!(edit.value.is_none());
        assert_eq!(
            Change::set(S::NotifyAfter, Value::Number(30))
                .edit()
                .value
                .unwrap()
                .as_integer(),
            Some(30)
        );
        // Every setting is somewhere in the file, and in a tab.
        for tab in 0..HOOKS_TAB {
            for row in rows(tab, &[]) {
                let Row::Setting(setting) = row else { panic!() };
                assert!(!setting.keys().is_empty());
            }
        }
        let settings: usize = (0..KEYS_TAB).map(|tab| rows(tab, &[]).len()).sum();
        assert_eq!(settings, 52);
    }

    /// Writes `change` to a config file made of `text`, and reads it back.
    fn written(text: &str, change: &Change) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        config::apply(&path, &[change.edit()]).unwrap();
        config::from_text(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn what_each_choice_writes_the_config_reads() {
        let mut view = view_of(Config::default(), None);
        for tab in 0..HOOKS_TAB {
            for row in rows(tab, &[]) {
                let Row::Setting(setting) = row else { continue };
                if setting.typed() {
                    continue;
                }
                go_to(&mut view, setting);
                for code in [KeyCode::Right, KeyCode::Left] {
                    let Outcome::Change(change) = press(&mut view, code) else {
                        continue;
                    };
                    written("", &change);
                }
            }
        }
        let config = written("", &Change::set(S::TaskBudget, Value::Cents(1_000)));
        assert_eq!(config.tasks.max_budget_usd, 10.0);
        let config = written("", &Change::set(S::NewCwd, "home"));
        assert_eq!(config.terminal.new_cwd, NewCwd::Home);
        let config = written("", &Change::set(S::RemoveEmptied, "never"));
        assert_eq!(config.worktrees.remove_emptied, EmptiedWorktree::Never);
        let config = written(
            "[appearance]\nlight_theme = \"nord\"\n",
            &Change::default(S::LightTheme),
        );
        assert_eq!(config.appearance.light_theme, None);
    }

    #[test]
    fn a_setting_typed_in_is_written_as_typed() {
        let mut view = view_of(Config::default(), None);
        go_to(&mut view, S::WindowTitle);
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert!(view.takes_keys_as_they_come());
        assert!(
            text(&view).contains("crystal · {session}"),
            "{}",
            text(&view)
        );
        for _ in 0.."{session}".len() {
            press(&mut view, KeyCode::Backspace);
        }
        view.on_paste("{branch} ");
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            change(S::WindowTitle, "crystal · {branch} ")
        );
        assert!(!view.takes_keys_as_they_come());

        // Empty, a setting that has to say something has its default.
        go_to(&mut view, S::WorktreeBase);
        press(&mut view, KeyCode::Char(' '));
        press(&mut view, KeyCode::Char('m'));
        press(&mut view, KeyCode::Backspace);
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Outcome::Change(Change::default(S::WorktreeBase))
        );
        go_to(&mut view, S::WorktreeDirectory);
        press(&mut view, KeyCode::Enter);
        view.on_paste("~/trees");
        let change = Change::set(S::WorktreeDirectory, "~/trees");
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Outcome::Change(change.clone())
        );
        let config = written("", &change);
        assert_eq!(config.worktrees.directory, Some(PathBuf::from("~/trees")));
        // Esc leaves it as it was.
        go_to(&mut view, S::Shell);
        press(&mut view, KeyCode::Enter);
        press(&mut view, KeyCode::Char('q'));
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Stay);
        assert!(!view.takes_keys_as_they_come());
        // ←/→ don't open it.
        assert_eq!(press(&mut view, KeyCode::Left), Outcome::Stay);
        assert!(!view.takes_keys_as_they_come());
    }

    #[test]
    fn delete_puts_a_settings_default_back() {
        let mut view = view_of(Config::default(), None);
        go_to(&mut view, S::ScrollLines);
        assert_eq!(
            press(&mut view, KeyCode::Delete),
            Outcome::Change(Change::default(S::ScrollLines))
        );
    }

    #[test]
    fn the_tabs_go_round_and_each_starts_at_its_top() {
        let mut view = view_of(Config::default(), None);
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Tab);
        assert_eq!(view.row(), Some(Row::Setting(S::Theme)));
        assert!(
            text(&view).contains("Tab bar and window"),
            "{}",
            text(&view)
        );
        press(&mut view, KeyCode::BackTab);
        press(&mut view, KeyCode::BackTab);
        assert_eq!(view.row(), Some(Row::Key(KeyId::Prefix)));
        press(&mut view, KeyCode::Char(']'));
        assert_eq!(view.row(), Some(Row::Setting(S::Notify)));
        press(&mut view, KeyCode::Char('9'));
        assert_eq!(view.row(), Some(Row::Setting(S::Notify)));
        press(&mut view, KeyCode::End);
        assert_eq!(view.row(), Some(Row::Setting(S::HideDrafts)));
        press(&mut view, KeyCode::Char('8'));
        press(&mut view, KeyCode::PageDown);
        assert_eq!(view.row(), Some(rows(KEYS_TAB, &[])[PAGE as usize]));
        // A tab with no rows has no bar.
        press(&mut view, KeyCode::Char('7'));
        assert_eq!(view.row(), None);
        assert_eq!(press(&mut view, KeyCode::Char(' ')), Outcome::Stay);
        assert!(text(&view).contains("none of the agents crystal can hook"));
    }

    /// The keys' tab, the bar on `key`.
    fn on_key_row(config: Config, key: KeyId) -> SettingsView {
        let mut view = view_of(config, None);
        press(&mut view, KeyCode::Char('8'));
        while view.row() != Some(Row::Key(key)) {
            press(&mut view, KeyCode::Down);
        }
        view
    }

    #[test]
    fn a_key_pressed_for_a_command_is_given_it() {
        let new_session = KeyId::Command(Command::NewSession);
        let mut view = on_key_row(Config::default(), new_session);
        assert!(text(&view).contains("new-session"), "{}", text(&view));
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert!(view.takes_keys_as_they_come());
        assert!(text(&view).contains("press a key…"), "{}", text(&view));
        // A Ctrl key too, which the view otherwise passes over.
        let ctrl_n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        let Outcome::Keys(rebinding) = view.on_key(ctrl_n) else {
            panic!("no keys")
        };
        assert_eq!(
            rebinding.lines,
            [(new_session, Some(Binding::One("ctrl+n".into())))]
        );
        let edits = key_edits(&rebinding);
        assert_eq!(edits[0].keys, ["keys", "new-session"]);
        assert_eq!(edits[0].value.as_ref().unwrap().as_str(), Some("ctrl+n"));

        // `a` adds one beside them.
        press(&mut view, KeyCode::Char('a'));
        assert!(text(&view).contains("press one to add…"));
        let Outcome::Keys(rebinding) = press(&mut view, KeyCode::F(2)) else {
            panic!("no keys")
        };
        let both = Binding::Many(vec!["n".into(), "f2".into()]);
        assert_eq!(rebinding.lines, [(new_session, Some(both))]);
        let edits = key_edits(&rebinding);
        assert_eq!(
            edits[0].value.as_ref().unwrap().as_array().unwrap().len(),
            2
        );
        // Esc waits for no key.
        press(&mut view, KeyCode::Enter);
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Stay);
        assert!(!view.takes_keys_as_they_come());
    }

    #[test]
    fn a_key_another_command_has_is_taken_only_once_the_user_says() {
        let archive = KeyId::Command(Command::Archive);
        let mut view = on_key_row(Config::default(), archive);
        press(&mut view, KeyCode::Enter);
        assert_eq!(press(&mut view, KeyCode::Char('x')), Outcome::Stay);
        assert!(
            text(&view).contains("x is kill's: enter takes it from it, esc leaves it"),
            "{}",
            text(&view)
        );
        let Outcome::Keys(rebinding) = press(&mut view, KeyCode::Enter) else {
            panic!("not taken")
        };
        assert_eq!(rebinding.taken_from, Some(KeyId::Command(Command::Kill)));
        // Any other key leaves it.
        press(&mut view, KeyCode::Enter);
        press(&mut view, KeyCode::Char('x'));
        assert_eq!(press(&mut view, KeyCode::Char('j')), Outcome::Stay);
        assert!(!view.takes_keys_as_they_come());
        assert!(!text(&view).contains("kill's"));
    }

    #[test]
    fn a_key_that_cant_be_given_says_why_and_x_and_delete_take_keys_back() {
        let config = Config {
            keys: toml::from_str(
                "kill = \"K\"\n[[command]]\nkey = \"g\"\ntype = \"popup\"\ncommand = \"lazygit\"\n",
            )
            .unwrap(),
            ..Config::default()
        };
        let kill = KeyId::Command(Command::Kill);
        let mut view = on_key_row(config, kill);
        assert!(text(&view).contains("• kill"), "{}", text(&view));
        assert!(text(&view).contains("lazygit"), "{}", text(&view));
        press(&mut view, KeyCode::Enter);
        assert_eq!(press(&mut view, KeyCode::Char('g')), Outcome::Stay);
        assert!(view.problem().unwrap().contains("lazygit"));
        assert!(text(&view).contains("change it in the config file"));
        assert_eq!(
            press(&mut view, KeyCode::Delete),
            Outcome::Keys(Rebinding {
                lines: vec![(kill, None)],
                taken_from: None,
            })
        );
        assert_eq!(
            press(&mut view, KeyCode::Char('x')),
            Outcome::Keys(Rebinding {
                lines: vec![(kill, Some(Binding::One("none".into())))],
                taken_from: None,
            })
        );
    }

    #[test]
    fn slash_filters_the_keys_and_esc_shows_them_all_again() {
        let zoom = KeyId::Command(Command::Zoom);
        let mut view = view_of(Config::default(), None);
        // Only in the keys' tab.
        press(&mut view, KeyCode::Char('/'));
        assert!(!view.takes_keys_as_they_come());
        press(&mut view, KeyCode::Char('8'));
        let every = view.rows().len();
        press(&mut view, KeyCode::Char('/'));
        assert!(view.takes_keys_as_they_come());
        for c in "zoom".chars() {
            press(&mut view, KeyCode::Char(c));
        }
        assert_eq!(view.row(), Some(Row::Key(zoom)));
        assert!(view.rows().len() < every);
        assert!(!view.rows().contains(&Row::Key(KeyId::Prefix)));
        let shown = text(&view);
        let count = format!("/ zoom  {} of {every}", view.rows().len());
        assert!(shown.contains(&count), "{shown}");
        assert!(!shown.contains("From a pane"), "{shown}");
        // Enter keeps it, and the rows' keys work again.
        press(&mut view, KeyCode::Enter);
        assert!(!view.takes_keys_as_they_come());
        assert_eq!(hints(&view)[4], ("/", "filter"));
        press(&mut view, KeyCode::Enter);
        assert!(text(&view).contains("press a key…"));
        press(&mut view, KeyCode::Esc);
        // Esc shows every key, the bar staying where it was, then closes.
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Stay);
        assert_eq!(view.rows().len(), every);
        assert_eq!(view.row(), Some(Row::Key(zoom)));
        assert!(!text(&view).contains("/ zoom"));
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn the_filter_finds_a_key_by_what_it_does_and_its_keys() {
        let mut view = view_of(Config::default(), None);
        press(&mut view, KeyCode::Char('8'));
        press(&mut view, KeyCode::Char('/'));
        view.on_paste("x");
        // `x` is kill's key; the names with an x in them are found too.
        assert!(
            view.rows()
                .contains(&Row::Key(KeyId::Command(Command::Kill)))
        );
        press(&mut view, KeyCode::Backspace);
        view.on_paste("editor");
        assert_eq!(
            view.rows(),
            [Row::Key(KeyId::Command(Command::EditHistory))]
        );
        view.on_paste("nothing");
        assert_eq!(view.row(), None);
        assert!(text(&view).contains("the filter finds no key's command"));
        // Another tab drops it.
        press(&mut view, KeyCode::Enter);
        assert_eq!(hints(&view), [("/", "filter"), ("esc", "show all")]);
        press(&mut view, KeyCode::Tab);
        press(&mut view, KeyCode::Char('8'));
        assert_eq!(view.rows().len(), KeyId::all().count());
    }

    /// The keys' tab of a view that knows of the notes plugin's action on
    /// `written`, the bar on `key`.
    fn with_a_plugins_key(written: &str, key: KeyId) -> SettingsView {
        let mut view = on_key_row(Config::default(), key);
        view.set_plugin_keys(vec![PluginKey {
            key: Some(keymap::Sequence::parse(written).unwrap()),
            plugin: "notes".into(),
            action: "add".into(),
            title: "add a note".into(),
        }]);
        view
    }

    #[test]
    fn a_key_a_plugins_action_takes_is_given_a_command_only_once_the_user_says() {
        let new_session = KeyId::Command(Command::NewSession);
        let mut view = with_a_plugins_key("N", new_session);
        press(&mut view, KeyCode::Enter);
        assert_eq!(press(&mut view, KeyCode::Char('N')), Outcome::Stay);
        let said = "N runs the notes plugin's \"add a note\", which would run from : alone \
                    once new-session has it: enter gives it all the same, esc leaves it";
        assert!(text(&view).contains(said), "{}", text(&view));
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Outcome::Keys(Rebinding {
                lines: vec![(new_session, Some(Binding::One("N".into())))],
                taken_from: None,
            })
        );
        // The first of two keys is taken from the action as much.
        let mut view = with_a_plugins_key("N t", new_session);
        press(&mut view, KeyCode::Char('a'));
        press(&mut view, KeyCode::Char('N'));
        assert!(text(&view).contains("N runs the notes plugin's"));
        assert_eq!(press(&mut view, KeyCode::Esc), Outcome::Stay);
        assert!(!view.takes_keys_as_they_come());
        // A view's key is no sidebar key: the action keeps working.
        let down = KeyId::Mode(keymap::ModeKey::ViewDown);
        let mut view = with_a_plugins_key("N", down);
        press(&mut view, KeyCode::Enter);
        assert!(matches!(
            press(&mut view, KeyCode::Char('N')),
            Outcome::Keys(_)
        ));
    }

    /// The rows of the screen with `view` drawn on an 80 by 24 one.
    fn screen(view: &SettingsView, theme: &Theme) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, view, theme, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..24)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// What the mouse is on where `text` is drawn, at its first character.
    fn spot_of(view: &SettingsView, theme: &Theme, text: &str) -> Spot {
        let rows = screen(view, theme);
        let (row, column) = (rows.iter().enumerate())
            .find_map(|(row, line)| Some((row, line.find(text)?)))
            .unwrap_or_else(|| panic!("no {text:?} in {rows:#?}"));
        let column = rows[row][..column].chars().count();
        let area = Rect::new(0, 0, 80, 24);
        hit(view, theme, area, column as u16, row as u16)
    }

    #[test]
    fn a_click_picks_a_tab_or_a_row_and_on_the_bars_row_is_enter() {
        let click = MouseEventKind::Down(MouseButton::Left);
        for framed in [false, true] {
            let theme = match framed {
                true => Theme::new(ThemeName::TERMINAL, false),
                false => Theme::new(ThemeName::DARK, false),
            };
            assert_eq!(theme.panel == Color::Reset, framed);
            let mut view = view_of(Config::default(), None);
            assert_eq!(spot_of(&view, &theme, "Mouse"), Spot::Tab(3));
            assert_eq!(spot_of(&view, &theme, "settings"), Spot::Elsewhere);
            view.on_mouse(click, spot_of(&view, &theme, "Keys"));
            let spot = spot_of(&view, &theme, "new-session");
            let Spot::Row(row) = spot else {
                panic!("{spot:?}")
            };
            assert_eq!(
                view.rows()[row],
                Row::Key(KeyId::Command(Command::NewSession))
            );
            assert_eq!(view.on_mouse(click, spot), Outcome::Stay);
            assert_eq!(
                view.row(),
                Some(Row::Key(KeyId::Command(Command::NewSession)))
            );
            assert!(!view.takes_keys_as_they_come());
            // Again, it waits for a key, which a click doesn't answer.
            view.on_mouse(click, spot);
            assert!(text(&view).contains("press a key…"));
            view.on_mouse(click, spot_of(&view, &theme, "General"));
            assert!(view.takes_keys_as_they_come());
            press(&mut view, KeyCode::Esc);
            // A heading is no row.
            assert_eq!(spot_of(&view, &theme, "Sidebar"), Spot::Elsewhere);
            // The wheel moves the bar.
            view.on_mouse(MouseEventKind::ScrollDown, Spot::Elsewhere);
            view.on_mouse(MouseEventKind::ScrollDown, Spot::Elsewhere);
            view.on_mouse(MouseEventKind::ScrollUp, Spot::Elsewhere);
            let after = view.rows()[row + 1];
            assert_eq!(view.row(), Some(after));
            // On a switch, a second click turns it over.
            view.on_mouse(click, spot_of(&view, &theme, "General"));
            let sounds = spot_of(&view, &theme, "sounds");
            assert_eq!(view.on_mouse(click, sounds), Outcome::Stay);
            assert_eq!(view.on_mouse(click, sounds), change(S::Sound, false));
        }
    }

    #[test]
    fn an_agents_hooks_go_in_come_up_to_date_and_come_out_with_space() {
        use integration::Agent;
        let mut view = view_of(Config::default(), None);
        let hooked = |integrations| Current {
            path: PathBuf::from("/c"),
            config: Ok(Config::default()),
            model: None,
            integrations,
        };
        view.set_current(hooked(vec![
            (Agent::Claude, Standing::Installed),
            (Agent::Codex, Standing::OutOfDate),
            (Agent::Pi, Standing::NotInstalled),
        ]));
        press(&mut view, KeyCode::Char('7'));
        let shown = text(&view);
        assert!(shown.contains("Claude Code"), "{shown}");
        assert!(shown.contains("out of date"), "{shown}");
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Integrate {
                agent: Agent::Claude,
                install: false
            }
        );
        press(&mut view, KeyCode::Char('j'));
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Outcome::Integrate {
                agent: Agent::Codex,
                install: true
            }
        );
        // The bar stops at the last.
        press(&mut view, KeyCode::Char('j'));
        press(&mut view, KeyCode::Char('j'));
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Integrate {
                agent: Agent::Pi,
                install: true
            }
        );
        view.set_note("codex: review them in its /hooks".into());
        assert!(text(&view).contains("review them in its /hooks"));
        // An agent gone between two reads leaves the bar on a row there is.
        view.set_current(hooked(vec![(Agent::Claude, Standing::NotInstalled)]));
        assert_eq!(
            press(&mut view, KeyCode::Char(' ')),
            Outcome::Integrate {
                agent: Agent::Claude,
                install: true
            }
        );
    }

    #[test]
    fn a_view_taller_than_the_screen_scrolls_to_the_bar() {
        let theme = Theme::new(ThemeName::DARK, false);
        let shown = |view: &SettingsView| {
            let backend = ratatui::backend::TestBackend::new(80, 16);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| draw(frame, view, &theme, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let cells = buffer.content().iter().map(|cell| cell.symbol());
            cells.collect::<String>()
        };
        let mut view = view_of(Config::default(), Some(status()));
        press(&mut view, KeyCode::Char('2'));
        let top = shown(&view);
        assert!(top.contains("theme"), "{top}");
        assert!(!top.contains("pin what needs you"), "{top}");
        press(&mut view, KeyCode::End);
        let bottom = shown(&view);
        assert!(bottom.contains("pin what needs you"), "{bottom}");
        assert!(!bottom.contains("follow the system"), "{bottom}");
        // The tabs stay at the top.
        assert!(bottom.contains("General  Look"), "{bottom}");
    }

    #[test]
    fn the_mouse_rows_say_how_it_is_and_dim_without_the_mouse() {
        let mut config = Config::default();
        let mut view = view_of(config.clone(), None);
        press(&mut view, KeyCode::Char('4'));
        let shown = text(&view);
        assert!(shown.contains("take the mouse"), "{shown}");
        assert!(shown.contains("3 lines"), "{shown}");
        config.mouse.capture = false;
        config.mouse.scroll_lines = 1;
        let mut view = view_of(config, None);
        press(&mut view, KeyCode::Char('4'));
        let shown = text(&view);
        assert!(shown.contains("1 line "), "{shown}");
        assert!(shown.contains("take it in attach     off"), "{shown}");
    }

    #[test]
    fn the_theme_rows_say_which_theme_each_is() {
        let mut config = Config {
            theme: ThemeName::find("catppuccin-latte").unwrap(),
            ..Config::default()
        };
        let mut view = view_of(config.clone(), None);
        press(&mut view, KeyCode::Char('2'));
        let shown = text(&view);
        let at = config.theme.position() + 1;
        let row = format!("catppuccin-latte the TUI's colors: ←/→ ({at} of 20)");
        assert!(shown.contains(&row), "{shown}");

        config.colors.insert(
            crate::config::ColorToken::Accent,
            crate::config::ColorValue(Color::Red),
        );
        let mut view = view_of(config, None);
        press(&mut view, KeyCode::Char('2'));
        assert!(text(&view).contains("under your [colors]: ←/→"));

        let config = Config {
            theme: ThemeName::find("kanagawa").unwrap(),
            ..Config::default()
        };
        let mut view = view_of(config.clone(), None);
        press(&mut view, KeyCode::Char('2'));
        let shown = text(&view);
        let row = "○   follow the system   off              \
                   kanagawa-lotus when it's light, kanagawa when dark";
        assert!(shown.contains(row), "{shown}");
        let light = format!(
            "{:<24}{:<17}the theme's: kanagawa-lotus",
            "    when it's light", "auto"
        );
        assert!(shown.contains(&light), "{shown}");
        assert!(shown.contains("  tab bar               top"), "{shown}");
        let mut config = config;
        config.appearance.auto_switch = true;
        config.appearance.dark_theme = ThemeName::find("dracula");
        let mut view = view_of(config, None);
        press(&mut view, KeyCode::Char('2'));
        let shown = text(&view);
        assert!(shown.contains("●   follow the system   on "), "{shown}");
        assert!(
            shown.contains("when it's light, dracula when dark"),
            "{shown}"
        );
        assert!(
            shown.contains(&format!("{:<24}dracula", "    when it's dark")),
            "{shown}"
        );
    }

    #[test]
    fn the_sessions_rows_say_what_they_are() {
        let mut view = view_of(Config::default(), None);
        press(&mut view, KeyCode::Char('3'));
        let row = |name: &str, value: &str| format!("{name:<22}{value}");
        let shown = text(&view);
        assert!(
            shown.contains(&row("space out restarts", "250ms apart")),
            "{shown}"
        );
        assert!(shown.contains(&row("scrollback", "10000 lines")), "{shown}");
        assert!(shown.contains(&row("shell", "$SHELL")), "{shown}");
        assert!(
            shown.contains(&row("base branch", "origin's default")),
            "{shown}"
        );
        press(&mut view, KeyCode::Char('5'));
        let shown = text(&view);
        assert!(shown.contains(&row("a run's budget", "$5 ")), "{shown}");
        assert!(
            shown.contains(&row("a day's budget", "no limit")),
            "{shown}"
        );
    }

    #[test]
    fn enter_gets_the_model_only_once_search_by_meaning_is_on() {
        let mut off = Config::default();
        off.memory.embeddings = false;
        let mut view = view_of(off, Some(status()));
        go_to(&mut view, S::Embeddings);
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert_eq!(
            view.problem(),
            Some("search by meaning is off: space turns it on")
        );
        let mut config = Config::default();
        config.memory.embeddings = true;
        view.set_current(Current {
            path: PathBuf::from("/c"),
            config: Ok(config),
            model: Some(status()),
            integrations: Vec::new(),
        });
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Prepare);
    }

    #[test]
    fn a_file_that_can_t_be_read_is_said_and_never_written() {
        let mut view = SettingsView::new();
        assert_eq!(press(&mut view, KeyCode::Char(' ')), Outcome::Stay);
        assert!(text(&view).contains("reading the settings"));
        view.set_current(Current {
            path: PathBuf::from("/c"),
            config: Err("`notfy` is not a setting".into()),
            model: None,
            integrations: Vec::new(),
        });
        assert_eq!(press(&mut view, KeyCode::Char(' ')), Outcome::Stay);
        assert!(view.problem().unwrap().contains("can't be read"));
        assert!(text(&view).contains("`notfy` is not a setting"));
        press(&mut view, KeyCode::Char('8'));
        assert_eq!(press(&mut view, KeyCode::Enter), Outcome::Stay);
        assert!(!view.takes_keys_as_they_come());
    }

    #[test]
    fn the_model_line_follows_the_download_and_the_daemon() {
        let config = Config::default();
        let memory = |config: &Config, model| {
            let mut view = view_of(config.clone(), model);
            press(&mut view, KeyCode::Char('6'));
            text(&view)
        };
        let downloading = Status {
            on_disk: 42_000_000,
            preparing: Some("downloading the models".into()),
            ..status()
        };
        let shown = memory(&config, Some(downloading));
        assert!(
            shown.contains("downloading the models · 42 of 134 MB"),
            "{shown}"
        );
        let ready = Status {
            on_disk: 134_000_000,
            loaded: true,
            entries: 40,
            embedded: 37,
            ..status()
        };
        let shown = memory(&config, Some(ready.clone()));
        assert!(shown.contains("downloaded, loaded in the daemon"));
        assert!(shown.contains("37 of 40 have their vector"));

        assert!(
            memory(&config, Some(status())).contains("not downloaded: enter gets them (134 MB)")
        );
        let mut off = Config::default();
        off.memory.embeddings = false;
        let shown = memory(&off, Some(status()));
        assert!(shown.contains("not downloaded (134 MB)"));
        assert!(!shown.contains("have their vector"));
        let failed = Status {
            failed: Some("couldn't download it".into()),
            ..ready
        };
        assert!(memory(&config, Some(failed)).contains("couldn't get them ready"));
    }

    #[test]
    fn with_memory_off_its_settings_say_they_do_nothing() {
        let mut config = Config::default();
        config.plugins.insert("memory".into(), false);
        let mut view = view_of(config, None);
        press(&mut view, KeyCode::Char('6'));
        assert!(text(&view).contains("the memory plugin is off"));
        assert!(text(&view).contains("the daemon didn't say"));
    }
}
