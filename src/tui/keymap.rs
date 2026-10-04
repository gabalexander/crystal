//! The sidebar's commands and the keys that run them. Every key the sidebar
//! takes is a [`Command`] here, with an id the config file names it by, what
//! it does, and the keys it has unless `[keys]` in the config says others:
//!
//! ```toml
//! [keys]
//! prefix = "ctrl+b"          # then a command's key, from inside a pane
//! hand-back = "ctrl+\\"      # from a pane back to the sidebar
//! new-session = ["n", "ctrl+n"]
//! kill = "X"
//! quit = "none"              # no key: `:` still runs it
//! ```
//!
//! A key the user gives a command is taken from the command that had it by
//! default, so moving a command onto another's key needs no second line;
//! two commands the user gives one key is an error, as is a command or a
//! key crystal doesn't know.
//!
//! The keys of the views (the diff, the finder and the rest), of copy mode
//! and of the questions on the footer line are their own and stay as they
//! are: they're the grammar each of those shares, not commands.
//!
//! Terminals disagree about how some keys arrive: Shift+n comes as `N`
//! with or without the Shift bit, Ctrl+\ as Ctrl+4, Shift+Tab as BackTab.
//! [`Chord`] folds those into one form, both for what's written in the
//! config and for what the terminal sends, so a binding matches whatever
//! the terminal sends for it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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

/// The ids `[keys]` takes besides commands'.
pub const PREFIX: &str = "prefix";
pub const HAND_BACK: &str = "hand-back";

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

/// The keys `[keys]` gives one command, or the prefix: one, a list of
/// them, or `"none"` (or an empty list) for no key at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Binding {
    One(String),
    Many(Vec<String>),
}

impl Binding {
    /// The keys, none for `"none"`, each read.
    fn chords(&self) -> Result<Vec<Chord>, String> {
        let written: Vec<&str> = match self {
            Binding::One(key) => vec![key.as_str()],
            Binding::Many(keys) => keys.iter().map(String::as_str).collect(),
        };
        let mut chords = Vec::new();
        for key in written {
            if key.trim().eq_ignore_ascii_case("none") {
                continue;
            }
            let chord = Chord::parse(key)?;
            if !chords.contains(&chord) {
                chords.push(chord);
            }
        }
        Ok(chords)
    }
}

/// Which command each key runs, the prefix, and the key that hands the
/// keyboard back from a pane: the defaults, with the config's `[keys]` on
/// top.
#[derive(Debug, Clone)]
pub struct Keymap {
    commands: HashMap<Chord, Command>,
    keys: HashMap<Command, Vec<Chord>>,
    prefix: Option<Chord>,
    hand_back: Chord,
}

impl Default for Keymap {
    fn default() -> Keymap {
        Keymap::new(&BTreeMap::new()).expect("the default keys make a keymap")
    }
}

impl Keymap {
    /// The keymap `[keys]` in the config asks for, or what's wrong with it:
    /// a command or a key crystal doesn't know, or one key given to two
    /// things.
    pub fn new(settings: &BTreeMap<String, Binding>) -> Result<Keymap, String> {
        // What the user gave, by command, and which of their lines gave
        // each key, to say so when two give the same.
        let mut given: HashMap<Command, Vec<Chord>> = HashMap::new();
        let mut taken: HashMap<Chord, &str> = HashMap::new();
        let mut take = |chord: Chord, id: &'static str, by: &str| -> Result<(), String> {
            if let Some(other) = taken.insert(chord, id) {
                return Err(format!(
                    "[keys] gives {chord} to both {other} and {by}: one key, one command"
                ));
            }
            Ok(())
        };
        // The prefix and the hand-back key first, the user's or the
        // defaults, so that a command given either is told so.
        let mut prefix = Some(Chord::parse(DEFAULT_PREFIX)?);
        let mut hand_back = Chord::parse(DEFAULT_HAND_BACK)?;
        for (id, binding) in settings {
            let chords = binding
                .chords()
                .map_err(|why| format!("[keys] {id}: {why}"))?;
            match id.as_str() {
                PREFIX => {
                    if chords.len() > 1 {
                        return Err("[keys] prefix: one key, or \"none\"".to_string());
                    }
                    prefix = chords.first().copied();
                }
                HAND_BACK => {
                    let [chord] = chords[..] else {
                        return Err("[keys] hand-back: one key, which can't be none".to_string());
                    };
                    hand_back = chord;
                }
                _ => {}
            }
        }
        if let Some(chord) = prefix {
            take(chord, PREFIX, PREFIX)?;
        }
        take(hand_back, HAND_BACK, HAND_BACK)?;
        for (id, binding) in settings {
            if id == PREFIX || id == HAND_BACK {
                continue;
            }
            let spec = COMMANDS
                .iter()
                .find(|spec| spec.id == id)
                .ok_or_else(|| unknown_command(id))?;
            let chords = binding
                .chords()
                .map_err(|why| format!("[keys] {id}: {why}"))?;
            for &chord in &chords {
                take(chord, spec.id, id)?;
            }
            given.insert(spec.command, chords);
        }
        // The defaults, but for commands the user gave keys and keys the
        // user gave to something else.
        let mut keys: HashMap<Command, Vec<Chord>> = HashMap::new();
        for spec in COMMANDS {
            let chords = match given.remove(&spec.command) {
                Some(chords) => chords,
                None => spec
                    .keys
                    .iter()
                    .map(|key| Chord::parse(key).expect("the default keys are keys"))
                    .filter(|chord| !taken.contains_key(chord))
                    .collect(),
            };
            keys.insert(spec.command, chords);
        }
        let mut commands = HashMap::new();
        for spec in COMMANDS {
            for &chord in &keys[&spec.command] {
                commands.insert(chord, spec.command);
            }
        }
        Ok(Keymap {
            commands,
            keys,
            prefix,
            hand_back,
        })
    }

    /// The command `key` runs, if any.
    pub fn command(&self, key: &KeyEvent) -> Option<Command> {
        self.commands.get(&Chord::of(key)).copied()
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

    /// Whether `key` is the prefix.
    pub fn is_prefix(&self, key: &KeyEvent) -> bool {
        self.prefix == Some(Chord::of(key))
    }

    pub fn prefix(&self) -> Option<Chord> {
        self.prefix
    }

    /// Whether `key` hands the keyboard back from a pane to the sidebar.
    pub fn is_hand_back(&self, key: &KeyEvent) -> bool {
        self.hand_back == Chord::of(key)
    }

    pub fn hand_back(&self) -> Chord {
        self.hand_back
    }

    /// Whether every command has the keys it has by default.
    fn is_default_for(&self, commands: &[Command]) -> bool {
        commands.iter().all(|&command| {
            let defaults: Vec<Chord> = spec_of(command)
                .keys
                .iter()
                .map(|key| Chord::parse(key).expect("the default keys are keys"))
                .collect();
            self.keys(command) == defaults.as_slice()
        })
    }

    /// How the `?` overlay writes the keys of `row`: as the row has it
    /// while its commands have their own keys, or else each command's
    /// first key, `/` between them. `None` when none of them has a key.
    pub fn row_label(&self, row: &HelpRow) -> Option<String> {
        if row.commands.is_empty() || self.is_default_for(row.commands) {
            return Some(row.label.to_string());
        }
        let keys: Vec<String> = row
            .commands
            .iter()
            .filter_map(|&command| self.keys(command).first().map(Chord::label))
            .collect();
        (!keys.is_empty()).then(|| keys.join("/"))
    }
}

fn unknown_command(id: &str) -> String {
    let near: Vec<&str> = COMMANDS
        .iter()
        .map(|spec| spec.id)
        .filter(|known| known.contains(id) || id.contains(known))
        .collect();
    let hint = match near.as_slice() {
        [] => "`crystal keys` lists them".to_string(),
        near => format!("did you mean {}?", near.join(" or ")),
    };
    format!("[keys] has {id:?}, which isn't a command crystal knows: {hint}")
}

/// A row of the `?` overlay's sidebar keys: a few commands that go
/// together, how the row writes their keys while they're the defaults, and
/// what they do.
pub struct HelpRow {
    pub label: &'static str,
    pub does: &'static str,
    /// No commands: keys that are the sidebar's own, like `y` answering.
    pub commands: &'static [Command],
    pub plugin: Option<&'static str>,
}

const fn row(label: &'static str, commands: &'static [Command], does: &'static str) -> HelpRow {
    HelpRow {
        label,
        does,
        commands,
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
    plugin_row("tasks", "c", &[C::CloseTask], "close its task"),
    plugin_row("tasks", "y/n/Y", &[], "answer what a task asks"),
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

/// `crystal keys`: every command, its id and its keys, as the config has
/// them, for the user to find an id to put in `[keys]`.
pub fn listing(keymap: &Keymap) -> String {
    let width = COMMANDS.iter().map(|spec| spec.id.len()).max().unwrap_or(0);
    let mut out = String::new();
    let mut line = |id: &str, keys: String, does: &str| {
        let keys = if keys.is_empty() {
            "-".to_string()
        } else {
            keys
        };
        out.push_str(&format!("{id:width$}  {keys:12}  {does}\n"));
    };
    let prefix = keymap
        .prefix()
        .map(|chord| chord.hint())
        .unwrap_or_default();
    line(PREFIX, prefix, "from a pane: then a command's key");
    line(
        HAND_BACK,
        keymap.hand_back().hint(),
        "from a pane back to the sidebar",
    );
    for spec in COMMANDS {
        let keys: Vec<String> = keymap.keys(spec.command).iter().map(Chord::hint).collect();
        line(spec.id, keys.join(" "), spec.does);
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
        let settings: BTreeMap<String, Binding> = toml::from_str(toml).unwrap();
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
}
