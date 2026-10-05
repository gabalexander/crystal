# Keys and commands

<sub>[← README](../README.md#documentation)</sub>

Every key the TUI takes, in the sidebar, in a pane, in its modes and views and in a text box, and how to
change them or add keys of your own. In the TUI, `?` shows the same keys as your config has them, and `crystal
keys` lists them.

- [In the sidebar](#in-the-sidebar)
- [In a pane](#in-a-pane)
- [Commands and `[keys]`](#commands-and-keys)
- [Modes and views](#modes-and-views)
- [Keys of your own](#keys-of-your-own)
- [Text boxes](#text-boxes)
- [The command list](#the-command-list)

## In the sidebar

| Key | In the sidebar |
|---|---|
| `j` / `k`, `↓` / `↑` | select a session, or a worktree with no sessions |
| `;` | go back to the session you were on before, in whichever [tab](tui.md#tabs) it is, as tmux's `last-pane` does, and `;` again comes back; one only passed over with `j` or `k` doesn't count. After the prefix in a pane, you type into it |
| `Enter` | type into the selected session, or start an ended one again, once you've said `y`; on a worktree with no sessions, start one there |
| `Space` | reply to the selected session without going into its pane: a box takes what to say, and `Enter` sends it, typed in with `Enter` after it, or as a [background task](tasks.md#background-tasks)'s follow-up (`Alt+Enter` or `Ctrl+J` for a new line, `Esc` to cancel) |
| `s` | split the selected session off into a pane of its own, beside its pane or below it, or close its split |
| `\|` / `-` | split the selected session's pane in two, side by side or one above the other |
| `Shift+arrows` | select the session in the pane to the left, right, above or below |
| `F` | float the selected session over the panes, and type into it; again, put it back |
| `H` / `J` / `K` / `L` | swap the selected session's pane with the one to its left, below, above or right |
| `R` | resize mode: move the borders of the selected session's pane with the keys |
| `z` | [zoom](tui.md#zoom-copy-mode-and-search) the selected session's pane to take the whole screen, or put it back |
| `Tab` / `Shift+Tab` | type into the next pane, or the one before |
| `PgUp` / `PgDn` | page the selected session's pane back through its history, or forward to live |
| `e` | open the selected session's [history](tui.md#zoom-copy-mode-and-search), and what's on its screen, in your `$EDITOR` |
| `v` | [copy mode](tui.md#zoom-copy-mode-and-search) in the selected session's pane: select, search its history, copy |
| `t` | make a new [tab](tui.md#tabs) with a shell in it, and go to it |
| `T` | name the tab you're in |
| `&` | close the tab you're in, and kill its sessions once you've said `y` |
| `[` / `]`, `1-9` | go to the tab before or after this one, or to the tab with that number |
| `{` / `}` | move the tab you're in one place to the left or right |
| `>` | move the selected session to another tab: then a tab's number, or `t` for a new one |
| `S` | your saved [layouts](tui.md#layouts): save your tabs as one, or put them back the way one has them |
| `n` | start a new session from [the new-session panel](sessions.md#starting-a-session), and type into it |
| `w` | the same, in a new worktree on a branch with a made-up name, like `brave-otter` |
| `D` | start a session [like the selected one](sessions.md#starting-a-session): the new-session panel, set to what it runs and where |
| `W` | remove the selected worktree, once nothing runs in it and you've said `y` |
| `+` | add a [project](worktrees.md#projects): a directory, made a git repository first if it isn't one, once you've said `y` |
| `r` | rename the selected session |
| `x` | kill the selected session, once you've said `y`; the last in a linked worktree asks whether the worktree goes too |
| `A` | [archive](sessions.md#archiving-and-idle-agents) the selected session, once you've said `y`: it stops and leaves the list, to start again where it was |
| `Z` | the archive: start an archived session again, in its conversation, or delete it |
| `!` | run the selected worktree's [project](worktrees.md#projects), with its `run` command, in a terminal of its own; again, stop it |
| `.` | open the selected worktree with its project's `open` command, like `code .` |
| `u` | select the next session that needs you: waiting on you first, then one that couldn't start again after a restart, then done |
| `U` | list everything that [needs you](events.md#timeline), in every tab, and answer a permission or a gate where it stands |
| `a` | the [timeline](events.md#timeline): what happened, the newest first, as it happens |
| `I` | the [timeline](events.md#timeline) of the selected session; `Ctrl+S` there goes on to its task's, its project's and everything |
| `M` | what the selected session leaves for the next: its worktree's [handoff notes](tasks.md#the-handoff-file) and the files its task [kept](tasks.md#kept-files), each read beside the list |
| `/` | find a session in any tab, a project or worktree with nothing running, a flow run, an open pull request or issue, or an item on a backlog, by typing a little of it; `Tab` keeps to one status; picking a session in another tab takes you there: [finding with `/`](tui.md#finding-with-) |
| `:` | the [command list](#the-command-list): every command by its name, with its key, the latest you ran first; `Enter` runs one |
| `(` / `)` | make the [sidebar](tui.md#the-sidebar) narrower or wider; its edge drags with the mouse too |
| `\` | fold the [sidebar](tui.md#the-sidebar) down to a rail of marks, or unfold it |
| `h` / `l` | fold the selected session's project down to its heading in the [sidebar](tui.md#the-sidebar), or unfold it; a click on a project's heading does the same |
| `Alt+k` / `Alt+j` | move the selected session up or down among those beside it in its worktree, or a folded project among the projects; dragging its row does the same: [the order](tui.md#the-order) |
| `Alt+K` / `Alt+J` | move the selected session's project up or down among the projects; dragging its heading does the same |
| `o` | open the pull request of the selected session's branch in your browser |
| `O` | list the open [pull requests](code.md#pull-requests-and-issues) of the selected session's project: read one, see its diff, comment, or start an agent in its worktree |
| `i` | list the open [issues](code.md#pull-requests-and-issues) of the selected session's project: read one, comment, edit it, or start an agent on it |
| `b` | open the selected session's project's [backlog](tasks.md#the-backlog) |
| `c` | close the selected session's [task](tasks.md#tasks): done or failed, with a line on how it went |
| `C` | open the selected [background task](tasks.md#background-tasks) in a terminal: Claude Code picks its conversation up there |
| `y` / `n` / `Y` | on a [background task](tasks.md#background-tasks) asking for a permission: allow it, deny it, or allow it always; elsewhere `n` is a new session |
| `g` | on a step of a [flow](flows.md): go on past its gate, or run a step that failed or was cut short again |
| `f` | on a step of a flow waiting at its gate: send it back, with notes on what to do differently |
| `d` | show what changed in the selected session's worktree: [the diff](code.md#the-diff) |
| `p` | find a file in the selected session's worktree and edit it: [the file finder](code.md#the-file-finder-and-the-tree-browser) |
| `E` | browse the selected session's worktree as a tree of its files, each previewed beside it: [the tree browser](code.md#the-file-finder-and-the-tree-browser) |
| `G` | search the files of the selected session's worktree as you type, and edit one where it's found: [find in files](code.md#find-in-files) |
| `B` | move the selected session's worktree onto another branch, or a new one: [the branch switcher](worktrees.md#the-branch-switcher) |
| `m` | what the selected session's project has remembered: [memory](memory.md) |
| `P` | list your [profiles](configuration.md#profiles), and add, change, copy or remove one |
| `X` | list the [plugins](plugins.md): switch them on and off, run their actions and open their panes |
| `,` | open the [settings](configuration.md#the-settings-view): notifications, sounds, the theme, and how memory learns and searches, each changed as you go |
| `#` | the memory each session's processes take, and crystal's own: [RAM](sessions.md#ram) |
| `?` | show every key, in the sidebar, in a pane, in resize mode, in a view, in a question and with the mouse, your own included: a page at a time when they don't all fit, `→` and `←` (or `Space`, `PgDn` and `PgUp`) turning the pages; `Tab` goes to the [guide](guide.md), a page on what to start, the keys that matter most and what agents call, and back |
| `q` | quit, once you've said `y`; the sessions keep running |

## In a pane

While you're typing into a session, every key goes to it, `Tab` included, except `Ctrl+\`, which takes you
back to the sidebar, `Shift+PageUp` / `Shift+PageDown`, which page through the pane's history, and the prefix,
`Ctrl+B`: press it, then any key in the table above, and that key's command runs without the keyboard leaving
the pane, as in tmux. `Ctrl+B` twice sends `Ctrl+B` to the program, and `Esc` after it does nothing. Some
terminals keep `Shift+PageUp` for their own scrolling; `Ctrl+B` and then `PageUp` does the same. Every key in
the table, the prefix and `Ctrl+\` included, can be changed, a command can have a key that works in a pane with
no prefix at all, and keys of your own can open a popup or run a command: see
[keys and commands](#commands-and-keys).

A program that asks for the [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/), as
Codex does, gets its keys that way, in a pane, through `crystal attach` and from `crystal send-keys`: keys the
old way can't tell apart, like `Esc`, `Shift+Enter` or `Ctrl+I` and `Tab`, reach it as themselves. From your
keyboard that takes a terminal that speaks the protocol too, like Ghostty, kitty, foot or Alacritty; in any
other, keys arrive the old way.

## Commands and `[keys]`

Every key in the sidebar's table runs a command with a name: `n` is `new-session`, `|` is `split-right`, `q` is
`quit`. `crystal keys` lists them all, with the keys your config gives them. `[keys]` in the
[config file](configuration.md) changes them, a command's name to one key, a list of them, or `"none"`:

```toml
[keys]
prefix = ["ctrl+b", "ctrl+a"]  # the prefixes, from inside a pane; "none" for no prefix
hand-back = "ctrl+g"           # from a pane back to the sidebar
new-session = ["n", "ctrl+n"]
kill = "X"                     # x is free now
split-right = "v"              # v was copy mode's: copy mode has no key now
quit = "none"                  # the command list still runs it
pane-left = ["shift+left", "direct+ctrl+alt+h"]   # in a pane too, without the prefix
```

A key you give one command is taken from the command that had it, which is left with its other keys, or none.
Two commands given the same key, a command or a key crystal doesn't know, are errors that name them, and a
name that's nearly one crystal knows says which, so a typo never goes unnoticed. Keys are written as `n`, `N`
(or `shift+n`), `ctrl+b`, `alt+enter`, `ctrl+alt+h`, `shift+left`, `pageup`, `space`, `f5`, or the character
itself, like `|`, `(` or `:`. The `?` overlay, the footer and the command list all say the keys you chose. The
[settings view](configuration.md#the-settings-view)'s keys tab gives a command the key you press, with the same checks.

Every prefix in the list starts the same thing: the first is the one the footer shows. Inside a pane every key
goes to the program but the prefixes and the hand-back key, so a command's key works there only after a prefix,
unless you write it `direct+`: then it works in a pane straight away, as well as in the sidebar. Only the keys
you write that way are taken from the programs, and only ones a program can spare: a `direct+` key has `ctrl` or
`alt`, or is an `F` key. `ctrl+alt` is the family terminals and desktops leave alone the most; some of it is
taken all the same, like `ctrl+alt+arrows` by GNOME's workspaces and `ctrl+alt+t` by Ubuntu's terminal, and on
a Mac plain `alt` makes characters unless the terminal sends `Option` as `Meta`. If a `direct+` key does
nothing, your terminal or your desktop kept it.

## Modes and views

Answering a background task, resize mode and the views have keys of their own, each set apart from the
sidebar's, so one of them can have a key a command has, as `n` answers no on a task asking and starts a session
everywhere else. `[keys]` names them the same way:

| Name | Keys | What it does |
|---|---|---|
| `answer-yes`, `answer-no`, `answer-always` | `y`, `n`, `Y` | on a [background task](tasks.md#background-tasks) asking for a permission, in the sidebar, its pane and the needs-you view |
| `resize-left`, `resize-down`, `resize-up`, `resize-right` | `h` `←`, `j` `↓`, `k` `↑`, `l` `→` | resize mode: move a border that way |
| `resize-even` | `=` | resize mode: even the panes out |
| `resize-done` | `Esc`, `Enter`, `q` | resize mode: done; `resize`'s own key again is done too |
| `view-down`, `view-up` | `j` `Ctrl+N`, `k` `Ctrl+P` | in a view: the row below, or above |
| `view-page-down`, `view-page-up` | none | in a view: a page on, or back |
| `view-open` | none | in a view: open or run what the bar is on |
| `view-close` | `q` | in a view: close it, or step back out of what's open in it |

A view is any of the lists that take the keyboard: the diff, the file finder, the tree browser, find in files,
the branch switcher, memory, the handoff notes, the files `crystal open` shows, the backlog, layouts, the
archive, the plugins, the settings, what needs you, RAM, the timeline, the issues, the pull requests, `/` and the command list. A key you give a view's name stands in every
one of them for the key they all take for it, `↓`, `↑`, `PgDn`, `PgUp`, `Enter` or `Esc`, which go on working
whatever you give; the defaults you leave it without do nothing. While a view is taking what you type, like
the file finder's query or a filter, a letter is typed rather than standing for anything, so a key there is
best given with `ctrl` or `alt`. The rest of each view's keys, copy mode's and the questions' on the footer line
are their own.

## Keys of your own

Keys of your own run commands, in `[[keys.command]]` tables: a key, written as `[keys]` writes one, `direct+`
or not, and what it runs.

```toml
[[keys.command]]
key = "direct+ctrl+alt+g"
type = "popup"               # over everything, with the keyboard, until it ends
command = "lazygit"
description = "lazygit"      # what ? and : call it, in place of the command
width = "80%"                # or so many cells; 80% of the screen each way, left out
height = "80%"

[[keys.command]]
key = "ctrl+t"
type = "pane"                # a session of its own, split off the selected session's pane
command = "make test"
split = "down"               # or "right"; as s would split, left out

[[keys.command]]
key = "T"
type = "tab"                 # a session of its own, in a new tab
command = "htop"

[[keys.command]]
key = "direct+ctrl+alt+s"
type = "shell"               # in the background; the footer says only if it fails
command = "git fetch --all"

[[keys.command]]
key = "N"
type = "plugin"              # one of an installed plugin's actions
command = "notes:add"
```

A command is a line for `/bin/sh -c`, run in the selected session's directory, or the TUI's with none
selected. It finds `CRYSTAL_BIN` and `CRYSTAL_SOCKET` in its environment, and the selected session's
`CRYSTAL_SESSION`, `CRYSTAL_SESSION_ID`, `CRYSTAL_PROJECT` and `CRYSTAL_WORKTREE`, as a [plugin](plugins.md)'s
action does; a popup, a pane and a tab are sessions of their own, so their `CRYSTAL_SESSION` is their own. A
popup is in `crystal ls` while it's open, ends when its program does or when you press `Ctrl+\`, and takes
every other key, `Esc` included. A pane's or a tab's session stays when its command ends, to read what it said
or run it again with `Enter`, like any other. Your commands are in the command list and the `?` overlay, under
what they're called.

## Text boxes

Every text box edits the way a shell's line does: the new-session panel's task and branch, the reply box, the
questions on the footer line, `/`, the command list, the views' filters, comments and forms.

| Key | In a text box |
| --- | --- |
| `←` / `→` | a character back, or on |
| `Alt+B` / `Alt+F`, `Ctrl+←` / `Ctrl+→`, `Alt+←` / `Alt+→` | a word back, or on |
| `Ctrl+A` / `Ctrl+E`, `Home` / `End` | to the start, or the end, of the line |
| `Ctrl+Home` / `Ctrl+End` | to the start, or the end, of all the text |
| `Backspace` / `Delete` | delete the character before the cursor, or after it |
| `Ctrl+W`, `Alt+Backspace`, `Ctrl+Backspace` | delete the word before the cursor |
| `Alt+D`, `Ctrl+Delete` | delete the word after the cursor |
| `Ctrl+U` / `Ctrl+K` | delete back to the start of the line, or on to its end; at the start or the end already, the line break |

A word is letters and digits, as readline has it: spaces and punctuation, `/`, `-`, `.` and `_` among them, come
between words. macOS's terminals send `Alt+B` and `Alt+F` for `Option+←` and `Option+→`, once `Option` is set
to act as `Meta` (`Alt`). A view keeps the keys it had: `Ctrl+E` is the new-session panel's command line, the
issues view's edit and the tree browser's editor, so `End` goes to the end there, and the tree browser's arrows,
`Home` and `End` are its tree's and its preview's, so its filter takes the arrows with `Ctrl` or `Alt` for a word,
and `Ctrl+Home` and `Ctrl+End` for its ends.

## The command list

`:` opens the command list: every command by its name, with what it does and its key, and your plugins'
actions after them. Type a little of a name, or of what it does, and `Enter` runs the one the bar is on, as
its key would. Before you type, the five you ran from it last come first, so it's also a quick way back to
what you just did. A command with no key, or whose key you don't remember, is always there: `guide` opens the
[guide](guide.md) and `release-notes` what's new in this crystal, neither with a key until you give it one.
