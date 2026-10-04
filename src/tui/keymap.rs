//! The TUI's keys and what they run. Every key the sidebar takes is a
//! [`Command`] here, with an id the config file names it by, what it does,
//! and the keys it has unless `[keys]` in the config says others:
//!
//! ```toml
//! [keys]
//! prefix = ["ctrl+b", "ctrl+a"]  # then a command's key, from inside a pane
//! hand-back = "ctrl+\\"          # from a pane back to the sidebar
//! new-session = ["n", "ctrl+n"]
//! pane-left = ["shift+left", "direct+ctrl+alt+h"]
//! kill = "X"
//! quit = "none"                  # no key: `:` still runs it
//! answer-yes = "a"               # a mode's key
//!
//! [[keys.command]]               # a key of the user's own
//! key = "direct+ctrl+alt+g"
//! type = "popup"
//! command = "lazygit"
//! ```
//!
//! A key the user gives a command is taken from the command that had it by
//! default, so moving a command onto another's key needs no second line;
//! two commands the user gives one key is an error, as is a command or a
//! key crystal doesn't know. A key written `direct+` works while a pane has
//! the keyboard too, without the prefix: only the keys the user writes so,
//! and only ones a program can spare, so programs keep every other key.
//!
//! Answering what a background task asks, resize mode and the views have
//! keys of their own, [`ModeKey`]s, each mode a table apart from the
//! sidebar's. A view's key the user gives stands for the key every view
//! takes for it (`↓`, `Esc`, …), which always works; the rest of a view's
//! keys, copy mode's and the questions' on the footer line are their own:
//! the grammar each of those shares, not commands.
//!
//! Terminals disagree about how some keys arrive: Shift+n comes as `N`
//! with or without the Shift bit, Ctrl+\ as Ctrl+4, Shift+Tab as BackTab.
//! [`Chord`] folds those into one form, both for what's written in the
//! config and for what the terminal sends, so a binding matches whatever
//! the terminal sends for it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

/// Everything the sidebar's keys can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Down,
    Up,
    Open,
    Reply,
    NextPane,
    PreviousPane,
    PaneLeft,
    PaneDown,
    PaneUp,
    PaneRight,
    NextNeedingYou,
    NeedsYou,
    Search,
    Commands,
    Keys,
    ToggleSplit,
    SplitRight,
    SplitDown,
    Zoom,
    Float,
    SwapLeft,
    SwapDown,
    SwapUp,
    SwapRight,
    Resize,
    Copy,
    PageUp,
    PageDown,
    EditHistory,
    NewTab,
    RenameTab,
    CloseTab,
    MoveToTab,
    PreviousTab,
    NextTab,
    MoveTabLeft,
    MoveTabRight,
    /// The tab with this number, 1 to 9.
    Tab(u8),
    Layouts,
    NewSession,
    NewWorktree,
    RemoveWorktree,
    Rename,
    Kill,
    Archive,
    Archived,
    RunProject,
    OpenProject,
    CloseTask,
    TaskToTerminal,
    FlowGoOn,
    FlowSendBack,
    Timeline,
    Diff,
    FindFile,
    FileTree,
    Grep,
    Branches,
    Memory,
    Profiles,
    PullRequest,
    PullRequests,
    Issues,
    Backlog,
    NarrowerSidebar,
    WiderSidebar,
    FoldSidebar,
    FoldProject,
    UnfoldProject,
    Plugins,
    Settings,
    Quit,
}

/// A command as the config file, the `:` list and the `?` overlay know it.
pub struct Spec {
    pub command: Command,
    /// What the config file's `[keys]` calls it. Never changes, since a
    /// user's keys are read by it.
    pub id: &'static str,
    /// What it does, for the `:` list.
    pub does: &'static str,
    /// The keys it has unless the config gives it others.
    pub keys: &'static [&'static str],
    /// The plugin of crystal's it's part of, which has to be on for it to
    /// do anything.
    pub plugin: Option<&'static str>,
}

const fn spec(
    command: Command,
    id: &'static str,
    does: &'static str,
    keys: &'static [&'static str],
) -> Spec {
    Spec {
        command,
        id,
        does,
        keys,
        plugin: None,
    }
}

const fn of_plugin(
    plugin: &'static str,
    command: Command,
    id: &'static str,
    does: &'static str,
    keys: &'static [&'static str],
) -> Spec {
    Spec {
        plugin: Some(plugin),
        ..spec(command, id, does, keys)
    }
}

/// Every command, in the order the `:` list shows them.
pub const COMMANDS: &[Spec] = &[
    spec(
        Command::Down,
        "down",
        "select the session below",
        &["j", "down"],
    ),
    spec(Command::Up, "up", "select the session above", &["k", "up"]),
    spec(
        Command::Open,
        "open",
        "type into the selected session, or rerun it",
        &["enter"],
    ),
    spec(
        Command::Reply,
        "reply",
        "reply to the selected session from right here",
        &["space"],
    ),
    spec(
        Command::Search,
        "search",
        "find a session, project, flow run or pull request",
        &["/"],
    ),
    spec(
        Command::Commands,
        "commands",
        "run a command by its name",
        &[":"],
    ),
    spec(
        Command::NextNeedingYou,
        "next-needing-you",
        "select the next session that needs you",
        &["u"],
    ),
    spec(
        Command::NeedsYou,
        "needs-you",
        "everything waiting on you, in every tab",
        &["U"],
    ),
    spec(
        Command::NewSession,
        "new-session",
        "start a session",
        &["n"],
    ),
    spec(
        Command::NewWorktree,
        "new-worktree",
        "start a session in a new worktree",
        &["w"],
    ),
    spec(
        Command::RemoveWorktree,
        "remove-worktree",
        "remove the selected worktree",
        &["W"],
    ),
    spec(
        Command::Rename,
        "rename",
        "rename the selected session",
        &["r"],
    ),
    spec(Command::Kill, "kill", "kill the selected session", &["x"]),
    spec(
        Command::Archive,
        "archive",
        "archive the selected session: stop it, keep it to start again",
        &["A"],
    ),
    spec(
        Command::Archived,
        "archived",
        "the archive of stopped sessions",
        &["Z"],
    ),
    spec(
        Command::RunProject,
        "run-project",
        "run the project in a terminal of its own, or stop it",
        &["!"],
    ),
    spec(
        Command::OpenProject,
        "open-project",
        "open the project, as its open command says",
        &["."],
    ),
    of_plugin(
        "tasks",
        Command::CloseTask,
        "close-task",
        "close the selected session's task",
        &["c"],
    ),
    spec(
        Command::TaskToTerminal,
        "task-terminal",
        "open the selected background task in a terminal",
        &["C"],
    ),
    of_plugin(
        "flows",
        Command::FlowGoOn,
        "flow-go-on",
        "flow run: go on past its gate, or run its step again",
        &["g"],
    ),
    of_plugin(
        "flows",
        Command::FlowSendBack,
        "flow-send-back",
        "flow run: send it back from its gate",
        &["f"],
    ),
    spec(Command::NextPane, "next-pane", "the next pane", &["tab"]),
    spec(
        Command::PreviousPane,
        "previous-pane",
        "the previous pane",
        &["shift+tab"],
    ),
    spec(
        Command::PaneLeft,
        "pane-left",
        "the pane to the left",
        &["shift+left"],
    ),
    spec(
        Command::PaneDown,
        "pane-down",
        "the pane below",
        &["shift+down"],
    ),
    spec(Command::PaneUp, "pane-up", "the pane above", &["shift+up"]),
    spec(
        Command::PaneRight,
        "pane-right",
        "the pane to the right",
        &["shift+right"],
    ),
    spec(
        Command::ToggleSplit,
        "split",
        "split the selected session off, or close its split",
        &["s"],
    ),
    spec(
        Command::SplitRight,
        "split-right",
        "split the pane, side by side",
        &["|"],
    ),
    spec(
        Command::SplitDown,
        "split-down",
        "split the pane, one below the other",
        &["-"],
    ),
    spec(
        Command::Zoom,
        "zoom",
        "zoom the selected session's pane, or unzoom",
        &["z"],
    ),
    spec(
        Command::Float,
        "float",
        "float the selected session over the panes, or put it back",
        &["F"],
    ),
    spec(
        Command::SwapLeft,
        "swap-left",
        "swap the pane with the one to its left",
        &["H"],
    ),
    spec(
        Command::SwapDown,
        "swap-down",
        "swap the pane with the one below",
        &["J"],
    ),
    spec(
        Command::SwapUp,
        "swap-up",
        "swap the pane with the one above",
        &["K"],
    ),
    spec(
        Command::SwapRight,
        "swap-right",
        "swap the pane with the one to its right",
        &["L"],
    ),
    spec(Command::Resize, "resize", "resize the panes", &["R"]),
    spec(
        Command::Copy,
        "copy",
        "copy mode in the selected session's pane",
        &["v"],
    ),
    spec(
        Command::PageUp,
        "page-up",
        "page back through its history",
        &["pageup"],
    ),
    spec(
        Command::PageDown,
        "page-down",
        "page forward through its history",
        &["pagedown"],
    ),
    spec(
        Command::EditHistory,
        "edit-history",
        "open its history in your editor",
        &["e"],
    ),
    spec(Command::NewTab, "new-tab", "a new tab", &["t"]),
    spec(Command::RenameTab, "rename-tab", "name the tab", &["T"]),
    spec(Command::CloseTab, "close-tab", "close the tab", &["&"]),
    spec(
        Command::MoveToTab,
        "move-to-tab",
        "move the selected session to another tab",
        &[">"],
    ),
    spec(
        Command::PreviousTab,
        "previous-tab",
        "the tab to the left",
        &["["],
    ),
    spec(Command::NextTab, "next-tab", "the tab to the right", &["]"]),
    spec(
        Command::MoveTabLeft,
        "move-tab-left",
        "move the tab in front one place left",
        &["{"],
    ),
    spec(
        Command::MoveTabRight,
        "move-tab-right",
        "move the tab in front one place right",
        &["}"],
    ),
    spec(Command::Tab(1), "tab-1", "tab 1", &["1"]),
    spec(Command::Tab(2), "tab-2", "tab 2", &["2"]),
    spec(Command::Tab(3), "tab-3", "tab 3", &["3"]),
    spec(Command::Tab(4), "tab-4", "tab 4", &["4"]),
    spec(Command::Tab(5), "tab-5", "tab 5", &["5"]),
    spec(Command::Tab(6), "tab-6", "tab 6", &["6"]),
    spec(Command::Tab(7), "tab-7", "tab 7", &["7"]),
    spec(Command::Tab(8), "tab-8", "tab 8", &["8"]),
    spec(Command::Tab(9), "tab-9", "tab 9", &["9"]),
    spec(Command::Layouts, "layouts", "saved layouts", &["S"]),
    spec(
        Command::Timeline,
        "timeline",
        "the timeline of what happened",
        &["a"],
    ),
    spec(
        Command::Diff,
        "diff",
        "what changed in the worktree: the diff",
        &["d"],
    ),
    spec(Command::FindFile, "find-file", "find a file", &["p"]),
    spec(
        Command::FileTree,
        "file-tree",
        "the worktree's files as a tree",
        &["E"],
    ),
    spec(Command::Grep, "grep", "find in files", &["G"]),
    spec(
        Command::Branches,
        "branches",
        "switch the worktree's branch",
        &["B"],
    ),
    of_plugin(
        "github",
        Command::PullRequest,
        "pull-request",
        "the worktree's pull request",
        &["o"],
    ),
    of_plugin(
        "github",
        Command::PullRequests,
        "pull-requests",
        "the project's pull requests",
        &["O"],
    ),
    of_plugin(
        "github",
        Command::Issues,
        "issues",
        "the project's issues",
        &["i"],
    ),
    of_plugin(
        "backlog",
        Command::Backlog,
        "backlog",
        "the project's backlog",
        &["b"],
    ),
    of_plugin(
        "memory",
        Command::Memory,
        "memory",
        "what the project's sessions remembered",
        &["m"],
    ),
    of_plugin(
        "profiles",
        Command::Profiles,
        "profiles",
        "your agent profiles",
        &["P"],
    ),
    spec(
        Command::NarrowerSidebar,
        "narrower-sidebar",
        "make the sidebar narrower",
        &["("],
    ),
    spec(
        Command::WiderSidebar,
        "wider-sidebar",
        "make the sidebar wider",
        &[")"],
    ),
    spec(
        Command::FoldSidebar,
        "fold-sidebar",
        "fold the sidebar to its marks, or unfold it",
        &["\\"],
    ),
    spec(
        Command::FoldProject,
        "fold-project",
        "fold the selected project down to its heading",
        &["h"],
    ),
    spec(
        Command::UnfoldProject,
        "unfold-project",
        "unfold the selected project",
        &["l"],
    ),
    spec(Command::Plugins, "plugins", "the plugins", &["X"]),
    spec(Command::Settings, "settings", "the settings", &[","]),
    spec(Command::Keys, "keys", "every key", &["?"]),
    spec(
        Command::Quit,
        "quit",
        "quit the TUI; the sessions carry on",
        &["q"],
    ),
];

/// The command `command`'s spec.
pub fn spec_of(command: Command) -> &'static Spec {
    COMMANDS
        .iter()
        .find(|spec| spec.command == command)
        .expect("every command has a spec")
}

/// What a sidebar key runs: one of crystal's commands, or one of the
/// user's `[[keys.command]]`s, by its place among them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bound {
    Command(Command),
    Custom(usize),
}

/// The keys of the modes the sidebar's commands aren't in: answering what
/// a background task asks, resize mode, and the views'.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeKey {
    AnswerYes,
    AnswerNo,
    AnswerAlways,
    ResizeLeft,
    ResizeDown,
    ResizeUp,
    ResizeRight,
    ResizeEven,
    ResizeDone,
    ViewDown,
    ViewUp,
    ViewPageDown,
    ViewPageUp,
    ViewOpen,
    ViewClose,
}

/// Where a [`ModeKey`] works. The keys of one mode are a table of their
/// own: two of them can't share a key, but one can have a key a sidebar
/// command or another mode has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// On a background task asking for a permission: in the sidebar, in
    /// its pane and in the needs-you view.
    Answer,
    Resize,
    /// In the views: the diff, the finder, the backlog and the rest.
    View,
}

/// A mode's key as the config file and `crystal keys` know it.
pub struct ModeSpec {
    pub key: ModeKey,
    pub mode: Mode,
    /// What the config file's `[keys]` calls it.
    pub id: &'static str,
    pub does: &'static str,
    /// The keys it has unless the config gives it others.
    pub keys: &'static [&'static str],
    /// The key the mode's own code takes for it, which a key the user gives
    /// it stands for. A view's always works, besides the keys it's given.
    /// None in resize mode, whose keys are all the keymap's.
    pub own: Option<&'static str>,
}

const fn mode_key(
    mode: Mode,
    key: ModeKey,
    id: &'static str,
    does: &'static str,
    keys: &'static [&'static str],
    own: Option<&'static str>,
) -> ModeSpec {
    ModeSpec {
        key,
        mode,
        id,
        does,
        keys,
        own,
    }
}

/// Every mode's keys, in the order `crystal keys` lists them.
pub const MODE_KEYS: &[ModeSpec] = &[
    mode_key(
        Mode::Answer,
        ModeKey::AnswerYes,
        "answer-yes",
        "a background task asking for a permission: allow it",
        &["y"],
        Some("y"),
    ),
    mode_key(
        Mode::Answer,
        ModeKey::AnswerNo,
        "answer-no",
        "a background task asking for a permission: deny it",
        &["n"],
        Some("n"),
    ),
    mode_key(
        Mode::Answer,
        ModeKey::AnswerAlways,
        "answer-always",
        "a background task asking for a permission: allow it always",
        &["Y"],
        Some("Y"),
    ),
    mode_key(
        Mode::Resize,
        ModeKey::ResizeLeft,
        "resize-left",
        "resize mode: move a border to the left",
        &["h", "left"],
        None,
    ),
    mode_key(
        Mode::Resize,
        ModeKey::ResizeDown,
        "resize-down",
        "resize mode: move a border down",
        &["j", "down"],
        None,
    ),
    mode_key(
        Mode::Resize,
        ModeKey::ResizeUp,
        "resize-up",
        "resize mode: move a border up",
        &["k", "up"],
        None,
    ),
    mode_key(
        Mode::Resize,
        ModeKey::ResizeRight,
        "resize-right",
        "resize mode: move a border to the right",
        &["l", "right"],
        None,
    ),
    mode_key(
        Mode::Resize,
        ModeKey::ResizeEven,
        "resize-even",
        "resize mode: even the panes out",
        &["="],
        None,
    ),
    mode_key(
        Mode::Resize,
        ModeKey::ResizeDone,
        "resize-done",
        "resize mode: done, as resize's own key is",
        &["esc", "enter", "q"],
        None,
    ),
    mode_key(
        Mode::View,
        ModeKey::ViewDown,
        "view-down",
        "in a view, as ↓ always does: the row below",
        &["j", "ctrl+n"],
        Some("down"),
    ),
    mode_key(
        Mode::View,
        ModeKey::ViewUp,
        "view-up",
        "in a view, as ↑ always does: the row above",
        &["k", "ctrl+p"],
        Some("up"),
    ),
    mode_key(
        Mode::View,
        ModeKey::ViewPageDown,
        "view-page-down",
        "in a view, as PgDn always does: a page on",
        &[],
        Some("pagedown"),
    ),
    mode_key(
        Mode::View,
        ModeKey::ViewPageUp,
        "view-page-up",
        "in a view, as PgUp always does: a page back",
        &[],
        Some("pageup"),
    ),
    mode_key(
        Mode::View,
        ModeKey::ViewOpen,
        "view-open",
        "in a view, as Enter always does: open or run what the bar is on",
        &[],
        Some("enter"),
    ),
    mode_key(
        Mode::View,
        ModeKey::ViewClose,
        "view-close",
        "in a view, as Esc always does: close it, or step back",
        &["q"],
        Some("esc"),
    ),
];

/// The mode key `key`'s spec.
pub fn mode_spec_of(key: ModeKey) -> &'static ModeSpec {
    MODE_KEYS
        .iter()
        .find(|spec| spec.key == key)
        .expect("every mode key has a spec")
}

/// The ids `[keys]` takes besides commands' and modes'.
pub const PREFIX: &str = "prefix";
pub const HAND_BACK: &str = "hand-back";

/// How a key in `[keys]` says it works while a pane has the keyboard,
/// without the prefix: `direct+ctrl+alt+h`.
pub const DIRECT: &str = "direct";

/// What the prefix is unless the config says: tmux's, and herdr's.
const DEFAULT_PREFIX: &str = "ctrl+b";

/// What hands the keyboard back from a pane unless the config says: the
/// key that detaches `crystal attach` too.
const DEFAULT_HAND_BACK: &str = "ctrl+\\";

/// A key with its modifiers, folded into one form whichever way the
/// terminal sent it: see the module's comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl Chord {
    fn new(code: KeyCode, modifiers: KeyModifiers) -> Chord {
        let mut modifiers =
            modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match code {
            // Shift+Tab, however it came.
            KeyCode::BackTab => {
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => {
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            KeyCode::Char(c) => {
                // A character is what Shift made of it already.
                let shifted = modifiers.contains(KeyModifiers::SHIFT);
                modifiers.remove(KeyModifiers::SHIFT);
                let ctrl = modifiers.contains(KeyModifiers::CONTROL);
                KeyCode::Char(match c {
                    // The legacy encoding has no Ctrl+\, Ctrl+], Ctrl+^ or
                    // Ctrl+_: it sends the bytes Ctrl+4 to Ctrl+7 send.
                    '4' if ctrl => '\\',
                    '5' if ctrl => ']',
                    '6' if ctrl => '^',
                    '7' if ctrl => '/',
                    '_' if ctrl => '/',
                    '@' | '2' if ctrl => ' ',
                    // Ctrl+Shift+B is Ctrl+B to most terminals.
                    c if ctrl => c.to_ascii_lowercase(),
                    c if shifted && c.is_ascii_lowercase() => c.to_ascii_uppercase(),
                    c => c,
                })
            }
            code => code,
        };
        Chord { code, modifiers }
    }

    /// The chord the terminal sent.
    pub fn of(key: &KeyEvent) -> Chord {
        Chord::new(key.code, key.modifiers)
    }

    /// The key event a terminal would send for it.
    pub fn event(&self) -> KeyEvent {
        KeyEvent::new(self.code, self.modifiers)
    }

    /// Reads a key as the config writes it: `n`, `N`, `shift+n`, `ctrl+b`,
    /// `alt+enter`, `shift+left`, `pageup`, `|`, `-`, `+`, `space`.
    pub fn parse(spec: &str) -> Result<Chord, String> {
        let mut rest = spec.trim();
        if rest.is_empty() {
            return Err("an empty key".to_string());
        }
        let mut modifiers = KeyModifiers::NONE;
        // A modifier's name and `+`, until what's left is the key: `+` on
        // its own, or whatever has no `+` after a modifier's name.
        while let Some((name, after)) = rest.split_once('+') {
            if after.is_empty() {
                break;
            }
            let modifier = match name.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
                "alt" | "meta" | "option" | "opt" | "m" => KeyModifiers::ALT,
                "shift" | "s" => KeyModifiers::SHIFT,
                _ => break,
            };
            modifiers |= modifier;
            rest = after;
        }
        let code = key_code(rest).ok_or_else(|| format!("{spec:?} isn't a key crystal knows"))?;
        Ok(Chord::new(code, modifiers))
    }

    /// Whether it types a character: one with neither Ctrl nor Alt.
    pub fn types(&self) -> bool {
        matches!(self.code, KeyCode::Char(_))
            && !self
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    }

    /// Whether a program in a pane can spare it: it has Ctrl or Alt, or
    /// it's an F key, which hardly any program needs.
    fn spared(&self) -> bool {
        self.modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            || matches!(self.code, KeyCode::F(_))
    }

    /// How the `?` overlay and the `:` list write it: `n`, `Ctrl+B`,
    /// `Shift+Tab`, `PgUp`, `↓`.
    pub fn label(&self) -> String {
        self.written(false)
    }

    /// How the footer writes it, in lower case but for the character
    /// itself: `n`, `N`, `ctrl+b`, `shift+tab`, `pgup`.
    pub fn hint(&self) -> String {
        self.written(true)
    }

    fn written(&self, lower: bool) -> String {
        let case = |text: &str| {
            if lower {
                text.to_ascii_lowercase()
            } else {
                text.to_string()
            }
        };
        let mut written = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            written.push_str(&case("Ctrl+"));
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            written.push_str(&case("Alt+"));
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            written.push_str(&case("Shift+"));
        }
        let key = match self.code {
            KeyCode::Char(' ') => case("Space"),
            KeyCode::Char(c) if self.modifiers.contains(KeyModifiers::CONTROL) && !lower => {
                c.to_ascii_uppercase().to_string()
            }
            KeyCode::Char(c) => c.to_string(),
            KeyCode::BackTab => case("Shift+Tab"),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::F(n) => format!("F{n}"),
            code => case(named(code).unwrap_or("?")),
        };
        written.push_str(&key);
        written
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hint())
    }
}

/// The keys with names, and how the overlay writes them. The config takes
/// these names in any case, and the others [`key_code`] knows.
const NAMED: &[(KeyCode, &str)] = &[
    (KeyCode::Enter, "Enter"),
    (KeyCode::Tab, "Tab"),
    (KeyCode::Esc, "Esc"),
    (KeyCode::Backspace, "Backspace"),
    (KeyCode::Delete, "Delete"),
    (KeyCode::Insert, "Insert"),
    (KeyCode::Home, "Home"),
    (KeyCode::End, "End"),
    (KeyCode::PageUp, "PgUp"),
    (KeyCode::PageDown, "PgDn"),
];

fn named(code: KeyCode) -> Option<&'static str> {
    NAMED
        .iter()
        .find(|(named, _)| *named == code)
        .map(|(_, name)| *name)
}

/// The key a name in the config is.
fn key_code(name: &str) -> Option<KeyCode> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(match c {
            '↑' => KeyCode::Up,
            '↓' => KeyCode::Down,
            '←' => KeyCode::Left,
            '→' => KeyCode::Right,
            c => KeyCode::Char(c),
        });
    }
    let lower = name.to_ascii_lowercase();
    let code = match lower.as_str() {
        "enter" | "return" | "ret" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "esc" | "escape" => KeyCode::Esc,
        "space" | "spc" => KeyCode::Char(' '),
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "plus" => KeyCode::Char('+'),
        "minus" => KeyCode::Char('-'),
        "pipe" | "bar" => KeyCode::Char('|'),
        "backslash" => KeyCode::Char('\\'),
        "colon" => KeyCode::Char(':'),
        "slash" => KeyCode::Char('/'),
        f if f.starts_with('f') => {
            let n: u8 = f[1..].parse().ok().filter(|n| (1..=24).contains(n))?;
            KeyCode::F(n)
        }
        _ => return None,
    };
    Some(code)
}

/// A key a plugin's action takes in the sidebar: one, which can be a
/// chord like `ctrl+alt+n`, or two pressed one after the other, written
/// with a space between them, like `N t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sequence {
    first: Chord,
    then: Option<Chord>,
}

impl Sequence {
    /// Reads a plugin's key as its `plugin.toml` writes it.
    pub fn parse(written: &str) -> Result<Sequence, String> {
        let mut keys = written.split_whitespace();
        let first = Chord::parse(keys.next().unwrap_or(""))?;
        let then = keys.next().map(Chord::parse).transpose()?;
        if keys.next().is_some() {
            return Err(format!("{written:?} is more than two keys"));
        }
        Ok(Sequence { first, then })
    }

    /// The key pressed first, and all of it when it's one.
    pub fn first(&self) -> Chord {
        self.first
    }

    /// The key pressed after the first, when it's two.
    pub fn then(&self) -> Option<Chord> {
        self.then
    }

    /// Whether `self` and `other` get in each other's way: they're the
    /// same, or one is the first key of the other.
    pub fn clashes(&self, other: &Sequence) -> bool {
        self.first == other.first
            && (self.then.is_none() || other.then.is_none() || self.then == other.then)
    }

    /// Why crystal keeps it from a plugin, if it does: its first key is
    /// one the sidebar has unless the config says otherwise, or Esc, which
    /// lets a key half pressed go; or its second is Esc.
    pub fn check(&self) -> Result<(), String> {
        let esc = Chord::new(KeyCode::Esc, KeyModifiers::NONE);
        if kept_from_plugins(self.first) || self.first == esc {
            return Err(format!("crystal uses `{}` itself", self.first.hint()));
        }
        if self.then == Some(esc) {
            return Err("esc can't be a second key: it lets the first go".to_string());
        }
        Ok(())
    }

    /// How the `?` overlay and the `:` list write it: `N t`, `Ctrl+Alt+N`.
    pub fn label(&self) -> String {
        match self.then {
            Some(then) => format!("{} {}", self.first.label(), then.label()),
            None => self.first.label(),
        }
    }
}

/// Whether `chord` is one the sidebar takes unless the config says
/// otherwise, which a plugin's key can't be: a command's default key, the
/// default prefix or hand-back key, or a character [`plugins::RESERVED_KEYS`]
/// names.
///
/// [`plugins::RESERVED_KEYS`]: crate::plugins::RESERVED_KEYS
fn kept_from_plugins(chord: Chord) -> bool {
    let default = |written: &&str| Chord::parse(written).ok() == Some(chord);
    let character = match chord.code {
        KeyCode::Char(c) if chord.types() => {
            c.is_whitespace() || crate::plugins::RESERVED_KEYS.contains(c)
        }
        _ => false,
    };
    character
        || [DEFAULT_PREFIX, DEFAULT_HAND_BACK].iter().any(default)
        || COMMANDS.iter().any(|spec| spec.keys.iter().any(default))
}

/// The keys `[keys]` gives one command, or the prefix: one, a list of
/// them, or `"none"` (or an empty list) for no key at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, expecting = "a key, a list of keys, or \"none\"")]
pub enum Binding {
    One(String),
    Many(Vec<String>),
}

/// A key `[keys]` gives, and whether it's written `direct+`, to work in a
/// pane too, without the prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Given {
    chord: Chord,
    direct: bool,
}

impl Binding {
    /// The keys, none for `"none"`, each read.
    fn keys(&self) -> Result<Vec<Given>, String> {
        let written: Vec<&str> = match self {
            Binding::One(key) => vec![key.as_str()],
            Binding::Many(keys) => keys.iter().map(String::as_str).collect(),
        };
        let mut keys: Vec<Given> = Vec::new();
        for key in written {
            let key = key.trim();
            if key.eq_ignore_ascii_case("none") {
                continue;
            }
            let (direct, rest) = match key.split_once('+') {
                Some((word, rest)) if word.eq_ignore_ascii_case(DIRECT) && !rest.is_empty() => {
                    (true, rest)
                }
                _ => (false, key),
            };
            let chord = Chord::parse(rest)?;
            match keys.iter_mut().find(|given| given.chord == chord) {
                Some(given) => given.direct |= direct,
                None => keys.push(Given { chord, direct }),
            }
        }
        Ok(keys)
    }

    /// The keys, none of them `direct+`, which only commands can be.
    fn chords(&self, id: &str) -> Result<Vec<Chord>, String> {
        let keys = self.keys().map_err(|why| format!("[keys] {id}: {why}"))?;
        if keys.iter().any(|given| given.direct) {
            return Err(format!(
                "[keys] {id}: only a command's key can be {DIRECT}+, to work in a pane"
            ));
        }
        Ok(keys.into_iter().map(|given| given.chord).collect())
    }
}

/// `[keys]` in the config file: each command's keys by its id, the
/// prefix, the hand-back key and the modes' keys, and the user's own keys
/// in `[[keys.command]]` tables.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySettings {
    #[serde(flatten)]
    pub bindings: BTreeMap<String, Binding>,
    #[serde(rename = "command", default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<KeyCommand>,
}

impl KeySettings {
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty() && self.commands.is_empty()
    }
}

impl FromIterator<(String, Binding)> for KeySettings {
    fn from_iter<I: IntoIterator<Item = (String, Binding)>>(bindings: I) -> KeySettings {
        KeySettings {
            bindings: bindings.into_iter().collect(),
            commands: Vec::new(),
        }
    }
}

/// A key of the user's own, `[[keys.command]]`: a command it runs, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyCommand {
    /// Its key, or keys, written as `[keys]` writes them: a sidebar key,
    /// and after the prefix in a pane, or `direct+` in a pane too.
    pub key: Binding,
    #[serde(rename = "type")]
    pub kind: CommandKind,
    /// A shell command line; for a plugin's action, `plugin:action`.
    pub command: String,
    /// What the `?` overlay and the `:` list say it does, in place of the
    /// command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A popup's size: so many cells, or a share of the screen, `"80%"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Extent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Extent>,
    /// Which way a pane splits off: as `s` would, unless it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitWay>,
}

/// How a `[[keys.command]]` runs its command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandKind {
    /// In a frame over everything, with the keyboard, closing when the
    /// command ends.
    Popup,
    /// In a session of its own, in a pane split off the selected session's.
    Pane,
    /// In a session of its own, in a new tab.
    Tab,
    /// In the background, with nothing on screen.
    Shell,
    /// One of an installed plugin's actions.
    Plugin,
}

impl CommandKind {
    pub fn name(self) -> &'static str {
        match self {
            CommandKind::Popup => "popup",
            CommandKind::Pane => "pane",
            CommandKind::Tab => "tab",
            CommandKind::Shell => "shell",
            CommandKind::Plugin => "plugin",
        }
    }
}

/// Which way a `[[keys.command]]`'s pane splits off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitWay {
    Right,
    Down,
}

/// A popup's width or height: so many cells, or a share of the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, expecting = "a number of cells, or a share like \"80%\"")]
pub enum Extent {
    Cells(u16),
    Share(String),
}

/// A popup's width and height unless its `[[keys.command]]` says.
const POPUP_SHARE: u16 = 80;

impl Extent {
    /// What's wrong with it, if anything: no cells, or a share that isn't
    /// one.
    pub fn check(&self) -> Result<(), String> {
        self.read().map(drop)
    }

    /// The share it is, from 1 to 100, or so many cells.
    fn read(&self) -> Result<Result<u16, u16>, String> {
        match self {
            Extent::Cells(0) => Err("a popup can't be 0 cells".to_string()),
            Extent::Cells(cells) => Ok(Err(*cells)),
            Extent::Share(share) => share
                .trim()
                .strip_suffix('%')
                .and_then(|number| number.trim().parse::<u16>().ok())
                .filter(|share| (1..=100).contains(share))
                .map(Ok)
                .ok_or_else(|| format!("{share:?} isn't a share of the screen, like \"80%\"")),
        }
    }

    /// How many of `room` cells `extent` takes, all of them at most; a
    /// share of them when it's left out.
    pub fn of(extent: Option<&Extent>, room: u16) -> u16 {
        let share = |share: u16| (u32::from(room) * u32::from(share) / 100) as u16;
        match extent.map(Extent::read) {
            Some(Ok(Err(cells))) => cells.min(room),
            Some(Ok(Ok(part))) => share(part),
            _ => share(POPUP_SHARE),
        }
    }
}

impl KeyCommand {
    /// What it's called on screen: its description, or else its command.
    pub fn label(&self) -> &str {
        self.description
            .as_deref()
            .filter(|description| !description.trim().is_empty())
            .unwrap_or(&self.command)
    }

    /// For a plugin's action, the plugin and the action.
    pub fn plugin_action(&self) -> Option<(&str, &str)> {
        let (plugin, action) = self.command.trim().split_once(':')?;
        let (plugin, action) = (plugin.trim(), action.trim());
        (!plugin.is_empty() && !action.is_empty()).then_some((plugin, action))
    }

    /// What's wrong with it, if anything: no command, a plugin's action
    /// not written `plugin:action`, or a size or a split where there's
    /// nothing to size or split.
    fn check(&self) -> Result<(), String> {
        let name = format!("[[keys.command]] {:?}", self.label());
        if self.command.trim().is_empty() {
            return Err(format!("{name} has no command"));
        }
        if self.kind == CommandKind::Plugin && self.plugin_action().is_none() {
            return Err(format!(
                "{name}: a plugin's action is written plugin:action, like notes:add"
            ));
        }
        for (field, extent) in [("width", &self.width), ("height", &self.height)] {
            let Some(extent) = extent else {
                continue;
            };
            if self.kind != CommandKind::Popup {
                return Err(format!("{name}: only a popup has a {field}"));
            }
            extent
                .read()
                .map(drop)
                .map_err(|why| format!("{name} {field}: {why}"))?;
        }
        if self.split.is_some() && self.kind != CommandKind::Pane {
            return Err(format!("{name}: only a pane splits"));
        }
        Ok(())
    }
}

/// What a key does in a view or the needs-you view, by the keys the user
/// gave the mode: what the view would make of it, or of the key it stands
/// for, or nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Translated {
    Same,
    As(KeyEvent),
    Nothing,
}

/// Which command each key runs, the prefixes, the key that hands the
/// keyboard back from a pane and the modes' keys: the defaults, with the
/// config's `[keys]` on top.
#[derive(Debug, Clone)]
pub struct Keymap {
    bound: HashMap<Chord, Bound>,
    keys: HashMap<Command, Vec<Chord>>,
    /// The sidebar keys that work while a pane has the keyboard too,
    /// without the prefix.
    direct: HashSet<Chord>,
    /// The user's own keys, each with what it runs.
    custom: Vec<(KeyCommand, Vec<Chord>)>,
    prefixes: Vec<Chord>,
    hand_back: Chord,
    modes: HashMap<(Mode, Chord), ModeKey>,
    mode_keys: HashMap<ModeKey, Vec<Chord>>,
    /// The keys the user gave a mode's keys that have one of their own,
    /// each standing for that one.
    standing: HashMap<(Mode, Chord), ModeKey>,
    /// Those modes' default keys the user gave none of them: nothing.
    dropped: HashSet<(Mode, Chord)>,
}

impl Default for Keymap {
    fn default() -> Keymap {
        Keymap::new(&KeySettings::default()).expect("the default keys make a keymap")
    }
}

/// Who has each key the user gave: a command, the prefix, the hand-back
/// key or a `[[keys.command]]`, to say so when two have the same.
struct Taken<K>(HashMap<K, String>);

impl<K: std::hash::Hash + Eq + Copy> Taken<K> {
    fn take(&mut self, key: K, by: &str, written: impl Fn() -> String) -> Result<(), String> {
        if let Some(other) = self.0.insert(key, by.to_string()) {
            return Err(format!(
                "[keys] gives {} to both {other} and {by}: one key, one command",
                written()
            ));
        }
        Ok(())
    }

    fn has(&self, key: &K) -> bool {
        self.0.contains_key(key)
    }
}

/// The keys `written` as the defaults write them.
fn defaults(written: &[&str]) -> Vec<Chord> {
    written
        .iter()
        .map(|key| Chord::parse(key).expect("the default keys are keys"))
        .collect()
}

impl Keymap {
    /// The keymap `[keys]` in the config asks for, or what's wrong with it:
    /// a command or a key crystal doesn't know, one key given to two
    /// things, or a key that can't do what it's written to.
    pub fn new(settings: &KeySettings) -> Result<Keymap, String> {
        let mut taken: Taken<Chord> = Taken(HashMap::new());
        // The prefixes and the hand-back key first, the user's or the
        // defaults, so that a command given one is told so.
        let mut prefixes = vec![Chord::parse(DEFAULT_PREFIX)?];
        let mut hand_back = Chord::parse(DEFAULT_HAND_BACK)?;
        for (id, binding) in &settings.bindings {
            match id.as_str() {
                PREFIX => prefixes = binding.chords(id)?,
                HAND_BACK => {
                    let [chord] = binding.chords(id)?[..] else {
                        return Err("[keys] hand-back: one key, which can't be none".to_string());
                    };
                    hand_back = chord;
                }
                _ => {}
            }
        }
        for &chord in &prefixes {
            taken.take(chord, PREFIX, || chord.hint())?;
        }
        taken.take(hand_back, HAND_BACK, || hand_back.hint())?;

        // What the user gave, by command and by mode key.
        let mut given: HashMap<Command, Vec<Chord>> = HashMap::new();
        let mut direct = HashSet::new();
        let mut mode_given: HashMap<ModeKey, Vec<Chord>> = HashMap::new();
        let mut mode_taken: Taken<(Mode, Chord)> = Taken(HashMap::new());
        for (id, binding) in &settings.bindings {
            if id == PREFIX || id == HAND_BACK {
                continue;
            }
            if let Some(spec) = COMMANDS.iter().find(|spec| spec.id == id) {
                let keys = binding
                    .keys()
                    .map_err(|why| format!("[keys] {id}: {why}"))?;
                for key in &keys {
                    taken.take(key.chord, spec.id, || key.chord.hint())?;
                    if key.direct {
                        check_direct(id, key.chord)?;
                        direct.insert(key.chord);
                    }
                }
                given.insert(spec.command, keys.iter().map(|key| key.chord).collect());
            } else if let Some(spec) = MODE_KEYS.iter().find(|spec| spec.id == id) {
                let chords = binding.chords(id)?;
                for &chord in &chords {
                    mode_taken.take((spec.mode, chord), spec.id, || chord.hint())?;
                    check_own(spec, chord)?;
                }
                mode_given.insert(spec.key, chords);
            } else {
                return Err(unknown_id(id));
            }
        }
        let mut custom: Vec<(KeyCommand, Vec<Chord>)> = Vec::new();
        for command in &settings.commands {
            command.check()?;
            let name = format!("[[keys.command]] {:?}", command.label());
            let keys = command.key.keys().map_err(|why| format!("{name}: {why}"))?;
            for key in &keys {
                taken.take(key.chord, &name, || key.chord.hint())?;
                if key.direct {
                    check_direct(&name, key.chord)?;
                    direct.insert(key.chord);
                }
            }
            custom.push((command.clone(), keys.iter().map(|key| key.chord).collect()));
        }

        // The defaults, but for commands the user gave keys and keys the
        // user gave to something else.
        let mut keys: HashMap<Command, Vec<Chord>> = HashMap::new();
        for spec in COMMANDS {
            let chords = match given.remove(&spec.command) {
                Some(chords) => chords,
                None => defaults(spec.keys)
                    .into_iter()
                    .filter(|chord| !taken.has(chord))
                    .collect(),
            };
            keys.insert(spec.command, chords);
        }
        let mut bound = HashMap::new();
        for spec in COMMANDS {
            for &chord in &keys[&spec.command] {
                bound.insert(chord, Bound::Command(spec.command));
            }
        }
        for (index, (_, chords)) in custom.iter().enumerate() {
            for &chord in chords {
                bound.insert(chord, Bound::Custom(index));
            }
        }

        // The same for the modes, each its own table.
        let mut mode_keys: HashMap<ModeKey, Vec<Chord>> = HashMap::new();
        let mut standing = HashMap::new();
        for spec in MODE_KEYS {
            let chords = match mode_given.remove(&spec.key) {
                Some(chords) => {
                    if spec.own.is_some() {
                        for &chord in &chords {
                            standing.insert((spec.mode, chord), spec.key);
                        }
                    }
                    chords
                }
                None => defaults(spec.keys)
                    .into_iter()
                    .filter(|chord| !mode_taken.has(&(spec.mode, *chord)))
                    .collect(),
            };
            mode_keys.insert(spec.key, chords);
        }
        let mut modes = HashMap::new();
        for spec in MODE_KEYS {
            for &chord in &mode_keys[&spec.key] {
                modes.insert((spec.mode, chord), spec.key);
            }
        }
        let mut dropped = HashSet::new();
        for spec in MODE_KEYS.iter().filter(|spec| spec.own.is_some()) {
            for chord in defaults(spec.keys) {
                if !modes.contains_key(&(spec.mode, chord)) {
                    dropped.insert((spec.mode, chord));
                }
            }
        }
        // The needs-you view takes both answers and the views' keys.
        for (&(mode, chord), by) in &mode_taken.0 {
            let other = match mode {
                Mode::Answer => Mode::View,
                Mode::View => Mode::Answer,
                Mode::Resize => continue,
            };
            if let Some(&key) = modes.get(&(other, chord)) {
                return Err(format!(
                    "[keys] gives {chord} to both {} and {by}, which the needs-you view both \
                     takes: one key, one command",
                    mode_spec_of(key).id
                ));
            }
        }
        Ok(Keymap {
            bound,
            keys,
            direct,
            custom,
            prefixes,
            hand_back,
            modes,
            mode_keys,
            standing,
            dropped,
        })
    }

    /// The command `key` runs, if any.
    pub fn command(&self, key: &KeyEvent) -> Option<Command> {
        match self.bound(key)? {
            Bound::Command(command) => Some(command),
            Bound::Custom(_) => None,
        }
    }

    /// What `key` runs in the sidebar, or after the prefix in a pane.
    pub fn bound(&self, key: &KeyEvent) -> Option<Bound> {
        self.bound.get(&Chord::of(key)).copied()
    }

    /// What `key` runs while a pane has the keyboard, without the prefix:
    /// only what the user wrote `direct+`.
    pub fn direct(&self, key: &KeyEvent) -> Option<Bound> {
        let chord = Chord::of(key);
        self.direct
            .contains(&chord)
            .then(|| self.bound.get(&chord).copied())
            .flatten()
    }

    /// Whether `chord` works in a pane without the prefix.
    pub fn is_direct(&self, chord: Chord) -> bool {
        self.direct.contains(&chord)
    }

    /// The keys that run `command`, the first the one shown.
    pub fn keys(&self, command: Command) -> &[Chord] {
        self.keys.get(&command).map_or(&[], Vec::as_slice)
    }

    /// The first key that runs `command`, as the footer writes it.
    pub fn hint(&self, command: Command) -> Option<String> {
        self.keys(command).first().map(Chord::hint)
    }

    /// The keys that run `command` as the `:` list writes them: `j ↓`.
    pub fn label(&self, command: Command) -> String {
        let keys: Vec<String> = self.keys(command).iter().map(Chord::label).collect();
        keys.join(" ")
    }

    /// The user's `[[keys.command]]`s, each with its keys.
    pub fn custom(&self) -> &[(KeyCommand, Vec<Chord>)] {
        &self.custom
    }

    /// Whether `key` is one of the prefixes.
    pub fn is_prefix(&self, key: &KeyEvent) -> bool {
        self.prefixes.contains(&Chord::of(key))
    }

    /// The prefix the footer and the overlay show: the first.
    pub fn prefix(&self) -> Option<Chord> {
        self.prefixes.first().copied()
    }

    pub fn prefixes(&self) -> &[Chord] {
        &self.prefixes
    }

    /// Whether `key` hands the keyboard back from a pane to the sidebar.
    pub fn is_hand_back(&self, key: &KeyEvent) -> bool {
        self.hand_back == Chord::of(key)
    }

    pub fn hand_back(&self) -> Chord {
        self.hand_back
    }

    /// What `key` does in `mode`, if anything.
    pub fn mode_key(&self, mode: Mode, key: &KeyEvent) -> Option<ModeKey> {
        self.modes.get(&(mode, Chord::of(key))).copied()
    }

    /// The keys of a mode's `key`, the first the one shown.
    pub fn mode_keys(&self, key: ModeKey) -> &[Chord] {
        self.mode_keys.get(&key).map_or(&[], Vec::as_slice)
    }

    /// What `key` is to the code of a mode with keys of its own, the views
    /// and the needs-you view's answers: the key it stands for, when the
    /// user gave it to one of the mode's keys; nothing, when it's a
    /// default the user gave none of them; or else itself. While the mode
    /// takes `typing`, a character is always typed.
    pub fn translate(&self, mode: Mode, key: &KeyEvent, typing: bool) -> Translated {
        let chord = Chord::of(key);
        if typing && chord.types() {
            return Translated::Same;
        }
        if let Some(&stands_for) = self.standing.get(&(mode, chord)) {
            let own = mode_spec_of(stands_for)
                .own
                .and_then(|own| Chord::parse(own).ok());
            return match own {
                Some(own) if own != chord => Translated::As(own.event()),
                _ => Translated::Same,
            };
        }
        if self.dropped.contains(&(mode, chord)) {
            return Translated::Nothing;
        }
        Translated::Same
    }

    /// Whether every command has the keys it has by default.
    fn is_default_for(&self, commands: &[Command]) -> bool {
        commands
            .iter()
            .all(|&command| self.keys(command) == defaults(spec_of(command).keys).as_slice())
    }

    /// Whether every mode key has the keys it has by default.
    fn modes_default_for(&self, keys: &[ModeKey]) -> bool {
        keys.iter()
            .all(|&key| self.mode_keys(key) == defaults(mode_spec_of(key).keys).as_slice())
    }

    /// How the `?` overlay writes the keys of `row`: as the row has it
    /// while its commands (or its modes' keys) have their own keys, or
    /// else each one's first key, `/` between them. `None` when none of
    /// them has a key.
    pub fn row_label(&self, row: &HelpRow) -> Option<String> {
        self.label_of(row.label, row.commands, row.modes, Chord::label)
    }

    /// [`Keymap::row_label`], as the footer writes keys.
    pub fn row_hint(&self, label: &str, commands: &[Command], modes: &[ModeKey]) -> Option<String> {
        self.label_of(label, commands, modes, Chord::hint)
    }

    fn label_of(
        &self,
        label: &str,
        commands: &[Command],
        modes: &[ModeKey],
        write: fn(&Chord) -> String,
    ) -> Option<String> {
        if self.is_default_for(commands) && self.modes_default_for(modes) {
            return Some(label.to_string());
        }
        let commands = commands.iter().map(|&command| self.keys(command));
        let modes = modes.iter().map(|&key| self.mode_keys(key));
        let keys: Vec<String> = commands
            .chain(modes)
            .filter_map(|keys| keys.first().map(write))
            .collect();
        (!keys.is_empty()).then(|| keys.join("/"))
    }
}

/// Refuses a `direct+` key a program in a pane couldn't spare.
fn check_direct(by: &str, chord: Chord) -> Result<(), String> {
    if chord.spared() {
        return Ok(());
    }
    Err(format!(
        "{by}: {DIRECT}+{chord} would take {chord} from every program in a pane: a {DIRECT} key \
         has ctrl or alt, like {DIRECT}+ctrl+alt+h, or is an F key"
    ))
}

/// Refuses to give a view's key the key another has of its own: that one
/// always does what it does.
fn check_own(spec: &ModeSpec, chord: Chord) -> Result<(), String> {
    if spec.mode != Mode::View {
        return Ok(());
    }
    let owner = MODE_KEYS.iter().find(|other| {
        other.mode == Mode::View
            && other.key != spec.key
            && other.own.and_then(|own| Chord::parse(own).ok()) == Some(chord)
    });
    match owner {
        Some(owner) => Err(format!(
            "[keys] {}: {chord} is {}'s in every view, always",
            spec.id, owner.id
        )),
        None => Ok(()),
    }
}

/// Every id `[keys]` takes.
fn ids() -> impl Iterator<Item = &'static str> {
    let commands = COMMANDS.iter().map(|spec| spec.id);
    let modes = MODE_KEYS.iter().map(|spec| spec.id);
    [PREFIX, HAND_BACK].into_iter().chain(commands).chain(modes)
}

fn unknown_id(id: &str) -> String {
    let near: Vec<&str> = ids().filter(|known| is_near(id, known)).collect();
    let hint = match near.as_slice() {
        [] => "`crystal keys` lists them".to_string(),
        near => format!("did you mean {}?", near.join(" or ")),
    };
    format!("[keys] has {id:?}, which isn't a command crystal knows: {hint}")
}

/// Whether `typed` is near enough `known` to be a slip for it: one has the
/// other in it, or they're two edits apart or less.
fn is_near(typed: &str, known: &str) -> bool {
    if typed.len() >= 3 && (known.contains(typed) || typed.contains(known)) {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (typed.chars().collect(), known.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut diagonal = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let above = row[j];
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row[j] = (above + 1).min(row[j - 1] + 1).min(diagonal + cost);
            diagonal = above;
        }
    }
    row[b.len()] <= 2
}

/// A row of the `?` overlay's sidebar keys: a few commands that go
/// together, how the row writes their keys while they're the defaults, and
/// what they do.
pub struct HelpRow {
    pub label: &'static str,
    pub does: &'static str,
    pub commands: &'static [Command],
    /// The keys of a mode the row is about, like answering's.
    pub modes: &'static [ModeKey],
    pub plugin: Option<&'static str>,
}

const fn row(label: &'static str, commands: &'static [Command], does: &'static str) -> HelpRow {
    HelpRow {
        label,
        does,
        commands,
        modes: &[],
        plugin: None,
    }
}

const fn plugin_row(
    plugin: &'static str,
    label: &'static str,
    commands: &'static [Command],
    does: &'static str,
) -> HelpRow {
    HelpRow {
        plugin: Some(plugin),
        ..row(label, commands, does)
    }
}

use Command as C;

/// The sidebar's keys in the `?` overlay, a few commands a row so that they
/// fit a small terminal.
pub const HELP: &[HelpRow] = &[
    row("j/k ↓/↑", &[C::Down, C::Up], "select a session"),
    row("/", &[C::Search], "find anything; Tab: status"),
    row(":", &[C::Commands], "every command by name"),
    row("Enter", &[C::Open], "type into it, or rerun"),
    row("Space", &[C::Reply], "reply, from right here"),
    row(
        "Tab/Shift+Tab",
        &[C::NextPane, C::PreviousPane],
        "next / previous pane",
    ),
    row(
        "Shift+arrows",
        &[C::PaneLeft, C::PaneDown, C::PaneUp, C::PaneRight],
        "the pane that way",
    ),
    row("s", &[C::ToggleSplit], "split it off, or close it"),
    row(
        "|/-",
        &[C::SplitRight, C::SplitDown],
        "split side by side / below",
    ),
    row("z", &[C::Zoom], "zoom its pane"),
    row("F", &[C::Float], "float it over the panes"),
    row(
        "H/J/K/L",
        &[C::SwapLeft, C::SwapDown, C::SwapUp, C::SwapRight],
        "swap its pane that way",
    ),
    row("R", &[C::Resize], "resize the panes"),
    row("v", &[C::Copy], "copy mode"),
    row(
        "PgUp/PgDn",
        &[C::PageUp, C::PageDown],
        "page through its history",
    ),
    row("e", &[C::EditHistory], "edit its history"),
    row(
        "t/T/&",
        &[C::NewTab, C::RenameTab, C::CloseTab],
        "tab: new / name / close",
    ),
    row(
        "[/] 1-9",
        &[
            C::PreviousTab,
            C::NextTab,
            C::Tab(1),
            C::Tab(2),
            C::Tab(3),
            C::Tab(4),
            C::Tab(5),
            C::Tab(6),
            C::Tab(7),
            C::Tab(8),
            C::Tab(9),
        ],
        "switch tabs",
    ),
    row(
        "{/}",
        &[C::MoveTabLeft, C::MoveTabRight],
        "move the tab left / right",
    ),
    row(">", &[C::MoveToTab], "move it to another tab"),
    row("S", &[C::Layouts], "saved layouts"),
    row(
        "n/w",
        &[C::NewSession, C::NewWorktree],
        "new, or in a worktree",
    ),
    row("W", &[C::RemoveWorktree], "remove the worktree"),
    row("r/x", &[C::Rename, C::Kill], "rename / kill it"),
    row(
        "A/Z",
        &[C::Archive, C::Archived],
        "archive it / the archive",
    ),
    row(
        "!/.",
        &[C::RunProject, C::OpenProject],
        "run / open the project",
    ),
    plugin_row(
        "tasks",
        "c/C",
        &[C::CloseTask, C::TaskToTerminal],
        "close task / in terminal",
    ),
    HelpRow {
        modes: &[ModeKey::AnswerYes, ModeKey::AnswerNo, ModeKey::AnswerAlways],
        ..plugin_row("tasks", "y/n/Y", &[], "answer what a task asks")
    },
    plugin_row(
        "flows",
        "g/f",
        &[C::FlowGoOn, C::FlowSendBack],
        "flow: go on / send back",
    ),
    row("u", &[C::NextNeedingYou], "next needing you"),
    row("U", &[C::NeedsYou], "all needing you"),
    row("a", &[C::Timeline], "the timeline"),
    plugin_row(
        "github",
        "o/O",
        &[C::PullRequest, C::PullRequests],
        "its PR / all PRs",
    ),
    plugin_row("github", "i", &[C::Issues], "the project's issues"),
    row("d", &[C::Diff], "what changed: the diff"),
    row("p", &[C::FindFile], "find a file"),
    row("E", &[C::FileTree], "the files as a tree"),
    row("G", &[C::Grep], "find in files"),
    row("B", &[C::Branches], "switch branches"),
    plugin_row("backlog", "b", &[C::Backlog], "the project's backlog"),
    plugin_row("memory", "m", &[C::Memory], "what it has remembered"),
    plugin_row("profiles", "P", &[C::Profiles], "your agent profiles"),
    row(
        "(/)/\\ h/l",
        &[
            C::NarrowerSidebar,
            C::WiderSidebar,
            C::FoldSidebar,
            C::FoldProject,
            C::UnfoldProject,
        ],
        "sidebar: size / folding",
    ),
    row("X/,", &[C::Plugins, C::Settings], "plugins / settings"),
    row("?/q", &[C::Keys, C::Quit], "keys / quit"),
];

/// Resize mode's keys in the `?` overlay.
pub const RESIZE_HELP: &[HelpRow] = &[
    HelpRow {
        modes: &[
            ModeKey::ResizeLeft,
            ModeKey::ResizeDown,
            ModeKey::ResizeUp,
            ModeKey::ResizeRight,
        ],
        ..row("h/j/k/l ←↓↑→", &[], "move a border that way")
    },
    row(
        "Shift+arrows",
        &[C::PaneLeft, C::PaneDown, C::PaneUp, C::PaneRight],
        "on to the pane that way",
    ),
    HelpRow {
        modes: &[ModeKey::ResizeEven],
        ..row("=", &[], "even the panes out")
    },
    HelpRow {
        modes: &[ModeKey::ResizeDone],
        ..row("Esc/Enter/q/R", &[C::Resize], "done")
    },
];

/// `crystal keys`: every command, its id and its keys, as the config has
/// them, for the user to find an id to put in `[keys]`; then the modes'
/// keys, and the user's own.
pub fn listing(keymap: &Keymap) -> String {
    let width = ids().map(str::len).max().unwrap_or(0);
    let line = |id: &str, keys: String, does: &str| {
        let keys = if keys.is_empty() { "-".into() } else { keys };
        format!("{id:width$}  {keys:12}  {does}\n")
    };
    let written = |chords: &[Chord]| {
        let keys: Vec<String> = chords
            .iter()
            .map(|chord| match keymap.is_direct(*chord) {
                true => format!("{DIRECT}+{chord}"),
                false => chord.hint(),
            })
            .collect();
        keys.join(" ")
    };
    let mut out = line(
        PREFIX,
        written(keymap.prefixes()),
        "from a pane: then a command's key",
    );
    out += &line(
        HAND_BACK,
        keymap.hand_back().hint(),
        "from a pane back to the sidebar",
    );
    for spec in COMMANDS {
        out += &line(spec.id, written(keymap.keys(spec.command)), spec.does);
    }
    out.push('\n');
    for spec in MODE_KEYS {
        out += &line(spec.id, written(keymap.mode_keys(spec.key)), spec.does);
    }
    if !keymap.custom().is_empty() {
        out.push_str("\n[[keys.command]]\n");
        for (command, chords) in keymap.custom() {
            let keys = match written(chords) {
                keys if keys.is_empty() => "-".to_string(),
                keys => keys,
            };
            out += &line(&keys, command.kind.name().into(), command.label());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn keymap(toml: &str) -> Result<Keymap, String> {
        let settings: KeySettings = toml::from_str(toml).unwrap();
        Keymap::new(&settings)
    }

    #[test]
    fn the_defaults_run_what_they_always_have() {
        let keymap = Keymap::default();
        let n = key(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(keymap.command(&n), Some(Command::NewSession));
        let shifted_n = key(KeyCode::Char('N'), KeyModifiers::SHIFT);
        assert_eq!(keymap.command(&shifted_n), None, "N is free");
        let j = key(KeyCode::Char('J'), KeyModifiers::SHIFT);
        assert_eq!(keymap.command(&j), Some(Command::SwapDown));
        let backtab = key(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(keymap.command(&backtab), Some(Command::PreviousPane));
        let pipe = key(KeyCode::Char('|'), KeyModifiers::SHIFT);
        assert_eq!(keymap.command(&pipe), Some(Command::SplitRight));
        let left = key(KeyCode::Left, KeyModifiers::SHIFT);
        assert_eq!(keymap.command(&left), Some(Command::PaneLeft));
        let seven = key(KeyCode::Char('7'), KeyModifiers::NONE);
        assert_eq!(keymap.command(&seven), Some(Command::Tab(7)));
    }

    #[test]
    fn ctrl_with_a_letter_isnt_the_letter() {
        let keymap = Keymap::default();
        let ctrl_j = key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(keymap.command(&ctrl_j), None);
    }

    #[test]
    fn every_default_key_is_one_commands_and_every_id_is_its_own() {
        let mut seen: HashMap<Chord, &str> = HashMap::new();
        let mut ids = std::collections::HashSet::new();
        for spec in COMMANDS {
            assert!(ids.insert(spec.id), "{} twice", spec.id);
            assert!(spec.id != PREFIX && spec.id != HAND_BACK);
            for written in spec.keys {
                let chord = Chord::parse(written).unwrap();
                if let Some(other) = seen.insert(chord, spec.id) {
                    panic!("{written} is both {other}'s and {}'s", spec.id);
                }
            }
        }
    }

    #[test]
    fn the_legacy_encoding_of_ctrl_backslash_is_the_hand_back_key() {
        let keymap = Keymap::default();
        assert!(keymap.is_hand_back(&key(KeyCode::Char('\\'), KeyModifiers::CONTROL)));
        assert!(keymap.is_hand_back(&key(KeyCode::Char('4'), KeyModifiers::CONTROL)));
        assert!(!keymap.is_hand_back(&key(KeyCode::Char('\\'), KeyModifiers::NONE)));
        assert!(keymap.is_prefix(&key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn keys_are_read_however_theyre_written() {
        let parse = |spec| Chord::parse(spec).unwrap();
        assert_eq!(parse("shift+n"), parse("N"));
        assert_eq!(parse("Ctrl+B"), parse("ctrl+b"));
        assert_eq!(parse("ctrl+shift+b"), parse("ctrl+b"));
        assert_eq!(parse("shift+tab"), parse("backtab"));
        assert_eq!(
            parse("+"),
            Chord::new(KeyCode::Char('+'), KeyModifiers::NONE)
        );
        assert_eq!(
            parse("ctrl++"),
            Chord::new(KeyCode::Char('+'), KeyModifiers::CONTROL)
        );
        assert_eq!(parse("pgdn"), parse("pagedown"));
        assert_eq!(parse("alt+enter").hint(), "alt+enter");
        assert_eq!(parse("ctrl+\\").label(), "Ctrl+\\");
        assert_eq!(parse("down").label(), "↓");
        assert_eq!(parse("space").hint(), "space");
        assert!(Chord::parse("hyper+x").is_err());
        assert!(Chord::parse("ctrl+nope").is_err());
        assert!(Chord::parse("").is_err());
    }

    #[test]
    fn a_key_the_user_gives_is_taken_from_the_default_that_had_it() {
        let keymap = keymap("kill = \"n\"").unwrap();
        let n = key(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(keymap.command(&n), Some(Command::Kill));
        assert!(keymap.keys(Command::NewSession).is_empty());
        let x = key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(keymap.command(&x), None, "kill's own key is free");
    }

    #[test]
    fn a_command_can_have_several_keys_or_none() {
        let keymap = keymap("new-session = [\"n\", \"ctrl+n\"]\nquit = \"none\"").unwrap();
        let ctrl_n = key(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(keymap.command(&ctrl_n), Some(Command::NewSession));
        assert_eq!(keymap.label(Command::NewSession), "n Ctrl+N");
        assert!(keymap.keys(Command::Quit).is_empty());
        let q = key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(keymap.command(&q), None);
    }

    #[test]
    fn what_the_keymap_cant_make_sense_of_is_an_error() {
        let both = keymap("kill = \"Q\"\nquit = \"Q\"").unwrap_err();
        assert!(both.contains("both kill and quit"), "{both}");
        let unknown = keymap("new-sesion = \"n\"").unwrap_err();
        assert!(unknown.contains("new-sesion"), "{unknown}");
        let bad_key = keymap("kill = \"ctrl+nope\"").unwrap_err();
        assert!(bad_key.contains("kill"), "{bad_key}");
        let no_hand_back = keymap("hand-back = \"none\"").unwrap_err();
        assert!(no_hand_back.contains("hand-back"), "{no_hand_back}");
        let prefix_taken = keymap("prefix = \"ctrl+a\"\nkill = \"ctrl+a\"").unwrap_err();
        assert!(prefix_taken.contains("prefix"), "{prefix_taken}");
        let default_prefix = keymap("kill = \"ctrl+b\"").unwrap_err();
        assert!(
            default_prefix.contains("both prefix and kill"),
            "{default_prefix}"
        );
        let freed = keymap("prefix = \"none\"\nkill = \"ctrl+b\"").unwrap();
        let ctrl_b = key(KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert_eq!(freed.command(&ctrl_b), Some(Command::Kill));
    }

    #[test]
    fn the_prefix_and_hand_back_key_can_be_changed_or_the_prefix_dropped() {
        let keymap = keymap("prefix = \"ctrl+a\"\nhand-back = \"ctrl+g\"").unwrap();
        assert!(keymap.is_prefix(&key(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        assert!(!keymap.is_prefix(&key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert!(keymap.is_hand_back(&key(KeyCode::Char('g'), KeyModifiers::CONTROL)));
        let none = self::keymap("prefix = \"none\"").unwrap();
        assert_eq!(none.prefix(), None);
    }

    #[test]
    fn a_help_row_follows_its_commands_keys() {
        let defaults = Keymap::default();
        let split = HELP.iter().find(|row| row.label == "|/-").unwrap();
        assert_eq!(defaults.row_label(split).as_deref(), Some("|/-"));
        let moved = keymap("split-right = \"V\"").unwrap();
        assert_eq!(moved.row_label(split).as_deref(), Some("V/-"));
        let gone = keymap("split-right = \"none\"\nsplit-down = \"none\"").unwrap();
        assert_eq!(gone.row_label(split), None);
    }

    #[test]
    fn every_command_is_in_the_help() {
        for spec in COMMANDS {
            let listed = HELP.iter().any(|row| row.commands.contains(&spec.command));
            assert!(listed, "{} isn't in the ? overlay", spec.id);
        }
    }

    #[test]
    fn the_listing_names_every_command() {
        let listing = listing(&Keymap::default());
        for spec in COMMANDS {
            assert!(listing.contains(spec.id), "{}", spec.id);
        }
        assert!(listing.starts_with("prefix"));
    }

    fn alt_ctrl(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::CONTROL | KeyModifiers::ALT)
    }

    #[test]
    fn every_prefix_the_config_gives_is_one_the_first_shown() {
        let keymap = keymap("prefix = [\"ctrl+b\", \"ctrl+a\"]").unwrap();
        assert!(keymap.is_prefix(&key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert!(keymap.is_prefix(&key(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        assert_eq!(keymap.prefix().map(|p| p.hint()).as_deref(), Some("ctrl+b"));
        let taken = self::keymap("prefix = [\"ctrl+b\", \"ctrl+a\"]\nkill = \"ctrl+a\"");
        assert!(taken.unwrap_err().contains("both prefix and kill"));
        let direct = self::keymap("prefix = \"direct+ctrl+a\"").unwrap_err();
        assert!(direct.contains("prefix"), "{direct}");
    }

    #[test]
    fn a_direct_key_works_in_a_pane_and_the_sidebar_and_the_others_dont() {
        let keymap = keymap("pane-left = [\"shift+left\", \"direct+ctrl+alt+h\"]").unwrap();
        let chord = alt_ctrl('h');
        assert_eq!(
            keymap.direct(&chord),
            Some(Bound::Command(Command::PaneLeft))
        );
        assert_eq!(keymap.command(&chord), Some(Command::PaneLeft));
        let shift_left = key(KeyCode::Left, KeyModifiers::SHIFT);
        assert_eq!(keymap.command(&shift_left), Some(Command::PaneLeft));
        assert_eq!(keymap.direct(&shift_left), None, "only what's direct+");
        let n = key(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(keymap.direct(&n), None);
        let listed = listing(&keymap);
        assert!(listed.contains("shift+← direct+ctrl+alt+h"), "{listed}");
    }

    #[test]
    fn a_direct_key_has_to_be_one_a_program_can_spare() {
        let plain = keymap("kill = \"direct+x\"").unwrap_err();
        assert!(plain.contains("direct+x would take x"), "{plain}");
        let shifted = keymap("kill = \"direct+shift+left\"").unwrap_err();
        assert!(shifted.contains("kill"), "{shifted}");
        assert!(keymap("kill = \"direct+f5\"").is_ok());
        assert!(keymap("kill = \"direct+alt+x\"").is_ok());
        let mode = keymap("view-down = \"direct+ctrl+j\"").unwrap_err();
        assert!(mode.contains("view-down"), "{mode}");
    }

    const LAZYGIT: &str = r#"
[[command]]
key = "direct+ctrl+alt+g"
type = "popup"
command = "lazygit"
width = "80%"
height = 30
"#;

    #[test]
    fn a_command_of_the_users_own_has_its_keys_like_any_other() {
        let keymap = keymap(LAZYGIT).unwrap();
        assert_eq!(keymap.bound(&alt_ctrl('g')), Some(Bound::Custom(0)));
        assert_eq!(keymap.direct(&alt_ctrl('g')), Some(Bound::Custom(0)));
        assert_eq!(keymap.command(&alt_ctrl('g')), None);
        let (command, keys) = &keymap.custom()[0];
        assert_eq!(command.kind, CommandKind::Popup);
        assert_eq!(command.label(), "lazygit");
        assert_eq!(keys.len(), 1);
        assert_eq!(Extent::of(command.width.as_ref(), 200), 160);
        assert_eq!(Extent::of(command.height.as_ref(), 20), 20);
        assert_eq!(Extent::of(None, 100), 80);
        let listed = listing(&keymap);
        assert!(listed.contains("[[keys.command]]"), "{listed}");
        assert!(listed.contains("popup"), "{listed}");
    }

    #[test]
    fn a_command_of_the_users_own_takes_a_default_key_but_not_one_the_user_gave() {
        let toml = "[[command]]\nkey = \"g\"\ntype = \"shell\"\ncommand = \"make\"";
        let keymap = keymap(toml).unwrap();
        assert!(keymap.keys(Command::FlowGoOn).is_empty());
        let g = key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(keymap.bound(&g), Some(Bound::Custom(0)));
        let both = self::keymap(&format!("kill = \"g\"\n{toml}")).unwrap_err();
        assert!(
            both.contains("both kill and [[keys.command]] \"make\""),
            "{both}"
        );
    }

    #[test]
    fn a_command_of_the_users_own_is_checked() {
        let command = |rest: &str| keymap(&format!("[[command]]\nkey = \"ctrl+g\"\n{rest}"));
        let empty = command("type = \"pane\"\ncommand = \" \"").unwrap_err();
        assert!(empty.contains("no command"), "{empty}");
        let plugin = command("type = \"plugin\"\ncommand = \"notes\"").unwrap_err();
        assert!(plugin.contains("plugin:action"), "{plugin}");
        let ok = command("type = \"plugin\"\ncommand = \"notes:add\"").unwrap();
        assert_eq!(ok.custom()[0].0.plugin_action(), Some(("notes", "add")));
        let sized = command("type = \"tab\"\ncommand = \"top\"\nwidth = 40").unwrap_err();
        assert!(sized.contains("only a popup"), "{sized}");
        let share = command("type = \"popup\"\ncommand = \"top\"\nwidth = \"150%\"");
        assert!(share.unwrap_err().contains("150%"));
        let split = command("type = \"popup\"\ncommand = \"top\"\nsplit = \"down\"");
        assert!(split.unwrap_err().contains("only a pane"));
        let bad_key = keymap("[[command]]\nkey = \"ctrl+nope\"\ntype = \"shell\"\ncommand = \"x\"");
        assert!(bad_key.unwrap_err().contains("[[keys.command]] \"x\""));
    }

    #[test]
    fn the_answer_keys_can_be_moved_and_the_old_ones_answer_nothing() {
        let keymap = keymap("answer-yes = \"a\"\nanswer-no = \"d\"").unwrap();
        let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
        let y = key(KeyCode::Char('y'), KeyModifiers::NONE);
        let shifted_y = key(KeyCode::Char('Y'), KeyModifiers::SHIFT);
        assert_eq!(keymap.mode_key(Mode::Answer, &a), Some(ModeKey::AnswerYes));
        assert_eq!(keymap.mode_key(Mode::Answer, &y), None);
        assert_eq!(
            keymap.mode_key(Mode::Answer, &shifted_y),
            Some(ModeKey::AnswerAlways)
        );
        assert_eq!(
            keymap.translate(Mode::Answer, &a, false),
            Translated::As(key(KeyCode::Char('y'), KeyModifiers::NONE))
        );
        assert_eq!(
            keymap.translate(Mode::Answer, &y, false),
            Translated::Nothing
        );
        assert_eq!(
            keymap.translate(Mode::Answer, &shifted_y, false),
            Translated::Same
        );
        // Two answers on one key is the same mistake as two commands.
        let both = self::keymap("answer-yes = \"a\"\nanswer-no = \"a\"").unwrap_err();
        assert!(both.contains("both answer-no and answer-yes"), "{both}");
        // An answer can have a sidebar command's key: they're apart.
        assert!(self::keymap("answer-yes = \"x\"").is_ok());
    }

    #[test]
    fn resize_modes_keys_can_be_moved() {
        let keymap = keymap("resize-left = \"a\"\nresize-done = [\"esc\", \"ctrl+c\"]").unwrap();
        let mode = |code, modifiers| keymap.mode_key(Mode::Resize, &key(code, modifiers));
        assert_eq!(
            mode(KeyCode::Char('a'), KeyModifiers::NONE),
            Some(ModeKey::ResizeLeft)
        );
        assert_eq!(mode(KeyCode::Char('h'), KeyModifiers::NONE), None);
        assert_eq!(mode(KeyCode::Left, KeyModifiers::NONE), None);
        assert_eq!(
            mode(KeyCode::Char('j'), KeyModifiers::NONE),
            Some(ModeKey::ResizeDown)
        );
        assert_eq!(
            mode(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(ModeKey::ResizeDone)
        );
        assert_eq!(mode(KeyCode::Char('q'), KeyModifiers::NONE), None);
    }

    #[test]
    fn a_views_keys_stand_for_its_own_and_its_defaults_taken_away_do_nothing() {
        let keymap = keymap("view-down = \"ctrl+j\"\nview-close = [\"q\", \"ctrl+g\"]").unwrap();
        let translate =
            |code, modifiers, typing| keymap.translate(Mode::View, &key(code, modifiers), typing);
        let down = Translated::As(key(KeyCode::Down, KeyModifiers::NONE));
        let esc = Translated::As(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            translate(KeyCode::Char('j'), KeyModifiers::CONTROL, false),
            down
        );
        assert_eq!(
            translate(KeyCode::Char('j'), KeyModifiers::CONTROL, true),
            down
        );
        assert_eq!(
            translate(KeyCode::Down, KeyModifiers::NONE, false),
            Translated::Same
        );
        // Taken away, `j` does nothing, but it's typed where a view takes
        // typing.
        assert_eq!(
            translate(KeyCode::Char('j'), KeyModifiers::NONE, false),
            Translated::Nothing
        );
        assert_eq!(
            translate(KeyCode::Char('j'), KeyModifiers::NONE, true),
            Translated::Same
        );
        assert_eq!(
            translate(KeyCode::Char('n'), KeyModifiers::CONTROL, true),
            Translated::Nothing
        );
        assert_eq!(
            translate(KeyCode::Char('q'), KeyModifiers::NONE, false),
            esc
        );
        assert_eq!(
            translate(KeyCode::Char('q'), KeyModifiers::NONE, true),
            Translated::Same
        );
        assert_eq!(
            translate(KeyCode::Char('g'), KeyModifiers::CONTROL, false),
            esc
        );
        // What the user left alone is the view's.
        assert_eq!(
            translate(KeyCode::Char('k'), KeyModifiers::NONE, false),
            Translated::Same
        );
        let defaults = Keymap::default();
        let j = key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(defaults.translate(Mode::View, &j, false), Translated::Same);
    }

    #[test]
    fn a_views_own_keys_stay_its_own() {
        let enter = keymap("view-close = \"enter\"").unwrap_err();
        assert!(enter.contains("view-open"), "{enter}");
        assert!(keymap("view-open = \"enter\"").is_ok());
    }

    #[test]
    fn the_needs_you_view_cant_take_one_key_for_an_answer_and_a_move() {
        let both = keymap("answer-yes = \"j\"").unwrap_err();
        assert!(both.contains("needs-you"), "{both}");
        assert!(
            both.contains("view-down") && both.contains("answer-yes"),
            "{both}"
        );
        assert!(keymap("answer-yes = \"j\"\nview-down = \"ctrl+j\"").is_ok());
    }

    #[test]
    fn a_slip_in_an_id_is_named_with_what_it_was_likely_meant_to_be() {
        let typo = keymap("new-sesion = \"n\"").unwrap_err();
        assert!(typo.contains("did you mean new-session?"), "{typo}");
        let mode = keymap("view-dwn = \"n\"").unwrap_err();
        assert!(mode.contains("view-down"), "{mode}");
        let far = keymap("teleport = \"n\"").unwrap_err();
        assert!(far.contains("crystal keys"), "{far}");
    }

    #[test]
    fn a_plugins_key_is_one_key_or_two_and_not_the_sidebars() {
        let parse = |written| Sequence::parse(written).unwrap();
        assert_eq!(parse("N t").label(), "N t");
        assert_eq!(parse("ctrl+alt+n").label(), "Ctrl+Alt+N");
        assert!(parse("N").check().is_ok());
        assert!(parse("N j").check().is_ok(), "a second key can be anything");
        assert!(parse("ctrl+alt+n").check().is_ok());
        assert!(parse("j").check().is_err());
        assert!(parse("j t").check().is_err());
        assert!(parse("enter").check().is_err());
        assert!(parse("ctrl+b").check().is_err());
        assert!(parse("shift+left").check().is_err());
        assert!(parse("N esc").check().is_err());
        assert!(Sequence::parse("N t u").is_err());
        assert!(Sequence::parse("").is_err());
        assert!(parse("N").clashes(&parse("N t")));
        assert!(parse("N t").clashes(&parse("N t")));
        assert!(!parse("N t").clashes(&parse("N u")));
        assert!(!parse("N").clashes(&parse("M")));
    }

    #[test]
    fn the_listing_names_every_mode_key() {
        let listing = listing(&Keymap::default());
        for spec in MODE_KEYS {
            assert!(listing.contains(spec.id), "{}", spec.id);
        }
        assert!(!listing.contains("[[keys.command]]"));
    }
}
