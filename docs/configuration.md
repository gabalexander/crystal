# Settings

<sub>[← README](../README.md#documentation)</sub>

Settings live in `~/.config/crystal/config.toml` (or `$XDG_CONFIG_HOME/crystal/config.toml`). The file is
optional, and so is every setting in it. `crystal config` prints the settings in effect, ready to save as the
file and change. A setting crystal doesn't know is an error that names it, so a typo never goes unnoticed.

- [Every setting](#every-setting)
- [Backups and other machines](#backups-and-other-machines)
- [Themes](#themes)
- [Terminals, the window and the tab bar](#terminals-the-window-and-the-tab-bar)
- [The settings view](#the-settings-view)
- [Profiles](#profiles)

## Every setting

A change counts without a restart: the daemon reads the file each time it needs a setting, and the TUI watches
it, takes a change in at once (its theme, its keys, the tab bar, the mouse…) and says so on its footer. A file
that makes no sense is said there too, and the settings stay as they were until it's mended.

| Setting | Default | What it does |
|---|---|---|
| `notify` | `true` | tell you when a session needs you |
| `notify_command` | none | a shell command to run instead of the desktop notification |
| `[notifications]` | | when to tell you: `after_secs`, how long a session must need you first (`0`), and `unfocused_only`, only while crystal's terminal hasn't the focus (`false`): [notifications](sessions.md#notifications) |
| `[sound]` | | the [sounds](sessions.md#sounds-and-the-bell) played at the same moments: `enabled` (`true`), your own `done` and `request` files, and `[sound.agents]` to switch them for an agent by its program |
| `new_session` | `"claude"` | what the new-session panel runs at first, until you start something from it |
| `theme` | `"dark"` | the TUI's colors: one of the [themes](#themes) |
| `[colors]` | | colors of your own over the theme's: [themes](#themes) |
| `name_from_prompt` | `true` | name a session you don't name for the [first thing it's asked](sessions.md#starting-a-session) |
| `name_by_agent` | `true` | have Claude Code name a session you don't name in [a few words of its own](sessions.md#starting-a-session), as it's sent its first prompt |
| `resume_reported_agents` | `true` | after a restart, run the command an agent [said resumes it](agents.md#teaching-crystal-about-your-agent), or the one that resumes an agent [typed into a shell](agents.md#agents-you-start-yourself) whose hooks named its conversation |
| `confirm_quit` | `true` | `q` asks before it quits the TUI, since a key meant for an agent can land on the sidebar; the sessions keep running either way |
| `show_keys` | `false` | show each key that runs a command at the right of the footer, with the command, for a few seconds: for whoever watches your screen shared or recorded |
| `mermaid_ascii` | `false` | draw [mermaid diagrams](code.md#the-file-finder-and-the-tree-browser) with ASCII rather than box drawing |
| `scrollback_lines` | `10000` | how many rows that scrolled off a session's screen it keeps, up to 1,000,000, for scrolling back, copy mode, `e` and `crystal read --history`; a change counts for the sessions running too, which let their oldest rows go when it's fewer |
| `[plugins]` | | which plugins are on and off: [plugins](plugins.md) |
| `[memory]` | | how memory's [distiller](memory.md#the-distiller) runs, whether it [searches by meaning](memory.md#search-by-meaning), and with what: the model here or [Gemini](memory.md#gemini-instead-of-the-model-here); and whether Claude Code is shown the entries about each file it reads |
| `[tasks]` | | what [background tasks](tasks.md#background-tasks) may spend: `max_budget_usd` each (`5`), `daily_budget_usd` all together (none); and what they may do without asking: `permission_mode` (`"default"`), `allowed_tools` (none) and `allow_bypass` (`false`) |
| `[events]` | | `keep_days`, how long the [event log](events.md) keeps what happened: 30 days, or `0` for ever |
| `[handoff]` | | `in_git`, the projects, by their main worktree, whose [handoff notes](tasks.md#the-handoff-file) go in git |
| `[worktrees]` | | `base`, the branch new worktrees' new branches [start from](worktrees.md#making-one): `origin`'s default branch unless set; `directory`, where new worktrees [go](worktrees.md#making-one), each project's in a directory of its own, from `/` or `~`: beside the project, in `<repo>.worktrees`, unless set; `remove_emptied`, what's done with a linked worktree once [its last session is killed](worktrees.md#in-the-sidebar): `"ask"` (the default), `"always"` removes it without asking, unless archived sessions ran there, and `"never"` keeps it |
| `[forge]` | | `hide_draft_prs`, leave draft pull requests out of [the pull requests](code.md#pull-requests), the tab bar's count and `/` (`false`) |
| `[sessions]` | | `stop_idle_after`, how long an agent may sit [idle](sessions.md#archiving-and-idle-agents) before crystal stops it, like `"2h"`, or `"off"`: `"30m"`; `stop_idle_terminals`, whether a terminal whose shell sits idle is stopped too (`false`); `warm_agent`, whether a Claude Code is kept [started and waiting](sessions.md#archiving-and-idle-agents) where the TUI's selection is, for a new session to take over (`false`); `restart_spacing_ms`, how far apart the agents a [crash or a reboot](sessions.md#after-a-restart) starts again start (`250`, or `0` for all at once); `restore_screens`, whether a terminal a crash or a reboot starts again shows what it showed before, kept in crystal's database (`false`: a screen can hold secrets) |
| `[[project]]` | | a project's [run and open commands](worktrees.md#projects), by its main worktree's `path`, in place of its own file's, and the [plugins it ships](plugins.md#a-projects-own-plugins) that are on for it, `plugins` |
| `[keys]` | | the TUI's keys, by command, its prefixes, the key back to the sidebar, answering's, resize mode's and the views', and `[[keys.command]]`, keys of your own that run commands: [keys and commands](keys.md) |
| `[sidebar]` | | the sidebar's `width`, whether it starts `folded`, what folding keeps, whether what needs you is pinned, its [`order`](tui.md#the-order), how narrow a terminal shows [one column](tui.md#on-a-phone), and its rows [laid out your own way](tui.md#laying-out-its-rows): [the sidebar](tui.md#the-sidebar) |
| `[terminal]` | | the shell a new terminal runs, `default_shell`, whether it's a login shell, `shell_mode`, and where `t` starts one, `new_cwd`: [terminals](#terminals-the-window-and-the-tab-bar) |
| `[window]` | | `title`, what the TUI titles its terminal: [the window](#terminals-the-window-and-the-tab-bar) |
| `[tab_bar]` | | where the tab bar goes, whether it's left out with one tab, and what it shows at its right: [the tab bar](#terminals-the-window-and-the-tab-bar) |
| `[appearance]` | | `auto_switch`, the theme following your system's light or dark, and the theme for each: [themes](#themes) |
| `[mouse]` | | `capture`, whether the TUI takes the mouse from your terminal (`true`); `copy_on_select`, whether a selection is copied as you let go or waits in copy mode for `y` (`true`); `scroll_lines`, how far a notch of the wheel scrolls a pane, up to 100, or how many arrow keys it sends a pager (`3`); `scrollbars`, a scrollbar beside each pane (`true`); `attach_capture`, whether `crystal attach` takes the mouse, for the wheel to scroll a session's history and a drag to select (`false`): [the mouse](tui.md#the-mouse) |
| `[clipboard]` | | `allow_programs`, whether what a program in a session copies goes on your clipboard (`true`): [copying](tui.md#zoom-copy-mode-and-search) |
| `[update]` | | `check`, whether the TUI looks once a day for a [newer crystal](install.md#updating) (`true`) |

`notify_command` is for telling you some other way, like a message to your phone. It runs with
`CRYSTAL_NOTICE` (the line a notification would show), `CRYSTAL_NOTICE_SESSION` (the session's name),
`CRYSTAL_NOTICE_ACTIVITY` (`waiting` or `done`) and `CRYSTAL_NOTICE_JUMP` (a shell command that takes you to the
session, for a notifier that runs one when it's clicked) in its environment:

```toml
notify_command = 'curl -s -d "$CRYSTAL_NOTICE" ntfy.sh/my-crystal'
```

`new_session` names an agent (`claude`, `codex`, …) or `shell`. With options, like `codex --full-auto`, it's
offered as a profile of its own.

The daemon reads the notification and sound settings each time it tells you something, `[plugins]` each time it
does something a plugin adds, `[memory]` each time a task closes or a search runs, `[tasks]` each time a
background task's run starts, `[handoff]` each time a note is written, `[sessions]` every 15 seconds and as it
starts sessions again (`restore_screens` every second), a flow
each time one starts, `[[project]]` each time a project's commands run,
`name_from_prompt` and `name_by_agent` each time they name a session, `resume_reported_agents` as it starts
sessions again,
`[clipboard]` each time a program copies out of sight,
`scrollback_lines` as each session starts and every 15 seconds for the sessions running, and `mermaid_ascii` as
each session starts and each background task's run does, so a change counts straight away (but for
`mermaid_ascii`, a session already running keeps what it had); `crystal new` and the TUI read `[terminal]` each
time they start a shell, and `crystal attach` reads `[clipboard]`, `[mouse]`, `[keys]` and `scrollback_lines` as
it attaches; the TUI reads
`new_session`, `theme`, `[colors]`, `[appearance]`, `[window]`, `[tab_bar]`, `scrollback_lines`, `mermaid_ascii`,
`[plugins]`, `[update]`, `[mouse]`, `[clipboard]`, `[forge]`, `confirm_quit`, `show_keys`, the profiles and the
flows when it starts, again when you save a profile or switch a plugin, and every half a second while the
settings view is open.

## Backups and other machines

```sh
crystal config export ~/backups             # write ~/backups/crystal-settings.json
crystal config export > settings.json       # or on standard output
crystal config import ~/backups             # merge it in, here or on another machine
crystal config import ~/dotfiles/crystal    # a config.toml, its agents/ beside it, or both
```

An export is one JSON file holding the config file as it is, comments and all, and your own [agent rule
files](agents.md#how-crystal-reads-an-agent) from `agents/` beside it. It never holds your plugins, nor what they keep
beside the config (`plugin-config/`, where a plugin's token goes), nor anything a server keeps: sessions,
layouts, the backlog, memory. An import takes an export, a config file on its own, a directory holding
either, the way a copy of `~/.config/crystal` does, or `-` for standard input. It merges rather than
replaces: a setting it has takes the place of yours, one it doesn't have stays as it is, profiles and flows
merge by their names, projects by their paths and your own keys' commands by their keys, and a rules file
replaces yours of the same name. Your config file keeps its comments and its order, nothing is written unless
all of it, merged, makes sense, and the import lists what it changed. The daemon goes by the new settings as
it next reads them; a TUI that's open shows all of them once it starts again.

## Themes

`theme` picks one of twenty:

| Theme | Also called | |
|---|---|---|
| `dark` | | crystal's own: deep ink, with a violet accent |
| `light` | | crystal's own: warm paper |
| `terminal` | | paints nothing, and uses your terminal's own sixteen colors |
| `catppuccin` | `catppuccin-mocha`, `mocha` | |
| `catppuccin-latte` | `latte` | light |
| `tokyo-night` | `tokyonight` | |
| `tokyo-night-day` | `tokyo-day`, `tokyonight-day` | light |
| `dracula` | | |
| `nord` | | |
| `gruvbox` | `gruvbox-dark` | |
| `gruvbox-light` | | light |
| `one-dark` | `onedark` | |
| `one-light` | `onelight` | light |
| `solarized` | `solarized-dark` | |
| `solarized-light` | | light |
| `kanagawa` | | |
| `kanagawa-lotus` | `lotus` | light |
| `rose-pine` | `rosepine` | |
| `rose-pine-dawn` | `rosepine-dawn`, `dawn` | light |
| `vesper` | | |

A name can be written in any case, with spaces or underscores for its dashes: `"Tokyo Night"` is
`tokyo-night`. Every theme but `terminal` paints its own background, so crystal looks the same in any
terminal. The schemes' colors are [herdr](https://github.com/herdrdev/herdr)'s, each a palette of ten
that crystal gives their roles and blends the tints it needs from, toward the background: behind a diff's
lines, what a search found, the selection and blocks of code.

`[appearance]` has the theme follow your system's light or dark, switching as the system does, without a
restart:

```toml
theme = "catppuccin"

[appearance]
auto_switch = true
light_theme = "catppuccin-latte"   # unless given: theme's light side, else crystal's own light
dark_theme = "catppuccin"          # unless given: theme's dark side, else crystal's own dark
```

A theme that has two sides (crystal's own, catppuccin, tokyo night, gruvbox, one, solarized, kanagawa and rose
pine) goes to its other side, so `theme = "gruvbox"` alone is `gruvbox-light` while it's light; one with
one side goes to crystal's `light` or `dark`; and `terminal` stays, since your terminal's own colors follow
your terminal. On a Mac the TUI asks the system's appearance every two seconds, and on Linux the desktop's
settings portal (GNOME's and KDE's), or else GNOME's own setting. Over ssh the system isn't yours, and some
systems can't say: there the TUI asks your terminal what its background is as it starts, and goes by that,
which follows a change only the next time it starts. (A terminal can tell a program each time its appearance
changes, mode 2031, but crystal's keyboard reader, crossterm's, would take that report for the start of a key
it waits to see the end of, and swallow the keys after it, until crossterm reads it: see crossterm's
[#1104](https://github.com/crossterm-rs/crossterm/issues/1104).)
Picking a theme in the [settings view](#the-settings-view) stops the following, as that's the theme you want.
`[appearance.light_colors]` and `[appearance.dark_colors]` take colors of your own, as `[colors]` below does,
for while it's light or dark, over `[colors]`.

`[colors]` puts colors of your own over the theme's, each named for what it's for:

```toml
theme = "catppuccin"

[colors]
accent = "#f5c2e7"      # what has the keyboard, and crystal's name
waiting = "bright-red"  # an agent waiting on you
selection = "#313244"   # behind the selected row
background = "reset"    # your terminal's own background
```

A color is `"#rrggbb"`, `"#rgb"`, one of your terminal's sixteen (`black`, `red`, `green`, `yellow`, `blue`,
`magenta`, `cyan`, `white`, and each with `bright-` in front), a number from its 256 (`"238"`), or `"reset"`
for your terminal's own. What each paints:

| Name | What it paints |
|---|---|
| `background`, `text`, `muted`, `accent` | behind everything; text; hints and times; what has the keyboard |
| `rule`, `panel`, `branch` | the lines between the parts; behind what's drawn over the rest; branch names |
| `waiting`, `working`, `done` | an agent waiting on you, at work, done with a turn you haven't seen |
| `running`, `ended`, `failed` | a program running, one that ended well, one that failed, and errors |
| `selection`, `copy_selection` | behind the selected row; behind text selected to copy |
| `found`, `found_current` | behind what a search found; behind the match copy mode's cursor is on |
| `added`, `removed` | a diff's counts and the letters of files it adds or deletes |
| `added_line`, `removed_line`, `added_words`, `removed_words` | behind a diff's lines, and the words that changed in them |
| `keyword`, `string`, `number`, `code_block` | highlighted code; behind a block of code |

A name or a color crystal doesn't know is an error that names it. With `NO_COLOR` set, crystal uses no color at
all, whatever the theme or `[colors]` say.

## Terminals, the window and the tab bar

```toml
[terminal]
default_shell = "fish"   # the shell a new terminal runs: a program, not a command line; $SHELL unless given
shell_mode = "auto"      # a login shell on a Mac, not elsewhere; or "login", "non_login"
new_cwd = "follow"       # where t starts its shell: "follow", "home", "current" or a directory

[window]
title = "crystal · {session}"

[tab_bar]
position = "top"         # or "bottom", over the footer
hide_when_single = false # leave it out while there's only one tab
separator = " · "
right = [
  { type = "hostname" },
  { type = "clock", format = "%a %H:%M" },
  { type = "text", text = "prod" },
  { type = "command", command = "~/bin/status.sh", every = "10s", timeout = "2s" },
]
```

A new terminal, whether `crystal new` with no command, `t`, the new-session panel's shell or `crystal pane
split` with no session, runs `default_shell`, or else your `$SHELL`, or else `/bin/sh`. `shell_mode = "auto"`
starts it as a login shell on a Mac, as Terminal and iTerm do, so the profile that puts Homebrew and
`path_helper`'s directories on the `PATH` runs; elsewhere it doesn't. A login shell is started with `-l`, which
sh, bash, zsh, fish, ksh, dash, tcsh, nu, xonsh and pwsh take; a shell with no such thing, like elvish, starts
as it is. A project's [run command](worktrees.md#projects) runs with `$SHELL -c` all the same.

`new_cwd` says where `t` starts its new tab's shell, and where a session starts when none is selected:
`follow`, the selected session's directory, or where you started `crystal` with none selected; `home`;
`current`, where you started `crystal`; or a directory of your own, from `/` or `~`. `crystal new` starts where
it's run, or `--cwd`.

`title` is what the TUI titles the terminal it runs in, which the terminal's tabs, its window and your window
manager show; a session's own title stops at crystal, which plays its terminal. Its tokens: `{session}`, the
selected session's name; `{project}` and `{branch}`, where it runs; `{title}`, the title its program gave its
terminal; `{tab}`, the tab in front's name, or its number; `{hostname}`, this machine's name up to its first
dot. `{{` and `}}` are braces. A token with nothing to say is empty, and so is what that leaves at either end,
like the ` · ` of `crystal · {session}` with no session. An empty `title` leaves your terminal's own alone.
`crystal title set` gives the terminal a title of its own in place of it, say while a script deploys, until
`crystal title clear`. The terminal's title from before is saved as the TUI starts and put back as it ends, by
a terminal that keeps a stack of them (xterm's, kitty, WezTerm, iTerm2…).

`right` lists what the tab bar shows after the count (and after the count of [pull requests and
issues](code.md#pull-requests-and-issues) open on the selected session's project), in order, `separator` between
them: `hostname`, this
machine's name; `clock`, the time as `strftime` writes `format` (`%H:%M` unless given); `text`; and
`command`, the last line a shell command prints, run in the directory you started `crystal` in with
`CRYSTAL_SOCKET` set, again `every` while (`10s` unless given), stopped once it takes `timeout` (`2s`), its
colors and other escape sequences taken out, and nothing when it fails. Something with nothing to say is left
out, and on a bar too narrow for it and the tabs, all of it is, the tabs coming first; the pull requests and
issues give way before it.

## The settings view

`,` in the sidebar opens the settings you'd otherwise change in the file, in tabs: `Tab` and `Shift+Tab` (or `]`
and `[`) go from one to the next, and `1` to `8` straight to one. With the mouse, a click on a tab shows it and a
click on a row puts the bar there; a click on the row the bar is on is `enter` there, and the wheel moves the
bar.

| Tab | What's in it |
|---|---|
| General | [notifications](sessions.md#notifications): whether, after how long, only while you're away, and a command of your own in place of them; sounds; whether `q` asks before it quits; looking for a [newer crystal](install.md#updating); how long the [event log](events.md) keeps what happened; and whether [draft pull requests](code.md#pull-requests) are hidden |
| Look | the [theme](#themes), whether it follows your system's appearance (the row says which theme each side is) and the theme for each side; the [tab bar](#terminals-the-window-and-the-tab-bar)'s place, whether it's left out with one tab, and its separator; the window's title; the [sidebar](tui.md#the-sidebar)'s width, whether it starts folded, what folding keeps, whether what needs you is pinned, whether what waits goes first ([the order](tui.md#the-order)), and how narrow a terminal shows [one column](tui.md#on-a-phone); whether keys pressed show at the footer (`show_keys`), and whether [mermaid diagrams](code.md#the-file-finder-and-the-tree-browser) are drawn in ASCII |
| Sessions | what the [new-session panel](sessions.md#starting-a-session) offers first, naming sessions for their prompt, how long an agent may sit [idle](sessions.md#archiving-and-idle-agents) and whether a terminal is stopped too, whether an agent is kept warm for the next session, how far apart agents start again after a [crash or a reboot](sessions.md#after-a-restart) and whether one resumes as it said; a new terminal's shell, whether it's a login shell and where it starts; how much each session's history keeps, the running ones' too, and whether a terminal shows it again after a crash or a reboot; and the branch new worktrees start from, where they go and whether one its last session is killed from is removed |
| Mouse | [the mouse](tui.md#the-mouse), and whether programs' copies go on [your clipboard](tui.md#zoom-copy-mode-and-search) |
| Tasks | the permission mode [background tasks](tasks.md#background-tasks) start in, and what a run and a day may spend |
| Memory | how memory learns ([the distiller](memory.md#the-distiller), its model and what it may spend), whether it searches [by meaning](memory.md#search-by-meaning), with the model here or [Gemini](memory.md#gemini-instead-of-the-model-here) and at how many dimensions, and reranks, and whether Claude Code is shown what's known about a file as it reads it |
| Integrations | the agents installed here that crystal can [hook](agents.md#hooks-in-other-agents-own-settings): below |
| Keys | every key `[keys]` gives, each given by pressing it: below |

`space` changes the setting the bar is on: a switch turns over, and a choice goes to the next, `←/→` going
through them forward and back: the [themes](#themes) (the row says which of the twenty it's on), the waits before
a notification, the times an agent may sit idle (off, 15 minutes, 30, an hour, two or eight), the spacing of
restarts, the permission modes (`default`, `acceptEdits`, `auto`, `dontAsk` and `plan`), the budgets, and the
rest. A setting that's text, like the window's title, the shell or the base branch, is typed in: `enter` opens
it, `enter` again writes it, and `esc` leaves it as it was; emptied, a command, the agent, the base branch or the
distiller's model goes back to its default. `del` on any row takes its line out of the file, for its default.
Each change is written to the file at once, keeping the rest of it as you wrote it, comments and all, and counts
straight away: the TUI repaints in a new theme, and the daemon reads the rest as it goes. A change crystal
couldn't read isn't written, and the view says why. On a screen too short for a tab's rows, the view scrolls to
keep the one the bar is on in sight. `[[profile]]`s have their own view (`P`), plugins theirs (`X`), and the
settings that are lists or tables, like `[colors]` and `[tab_bar] right`, are the file's.

The keys' tab lists the prefixes, the key back from a pane, every command and each mode's keys, with the keys
each has and a `•` beside those the file gives. `enter` on one waits for the key you press next and gives it that
key in place of its own, and `a` gives it the key beside them; `x` leaves it with none, and `del` puts its own
back. A key is checked as the file's keys are: one another command has asks first, saying whose it is, and
`enter` takes it from that one, which keeps its other keys, while `esc` leaves it; a key a view always has, or
the key back from a pane, isn't given, and the view says why. A key an installed [plugin](plugins.md)'s action
takes asks first too, since the command would have it before the action, which would run from the command list
alone; `enter` gives it all the same. Your own `[[keys.command]]` keys are listed under the rest, for the file to
change. `esc` can't be given this way, since it stops the waiting; the file can give it.

`/` filters the keys' tab as you type: a key stays when each word is in its command's name, letters in order,
or in what it does, or is one of its keys, like `x` or `ctrl+n`. The arrows move the bar meanwhile, `enter` keeps
the filter and gives the rows their keys back, and `esc` takes it away, the bar staying on the key it was on;
`esc` again closes the view.

While it's open, the view reads the file and asks the daemon again every half a second, so it follows a
change made by hand in the file too, and shows how the models that search by meaning stand: downloading
(`42 of 2449 MB`), loaded in the daemon or not, and how many entries have their vector; with Gemini, where
its key is and how many tokens it was sent, or why its last request failed and what searches go by meanwhile. Turning search by
meaning on has the daemon get the models ready: it downloads them if they aren't here, loads them and gives
every entry its vector, and `enter` on that row does it again. Turned off, the daemon lets the models go, and
the memory they took with them.

The integrations' tab lists the agents installed here that crystal can [hook](agents.md#hooks-in-other-agents-own-settings),
each with how its hooks stand, as `crystal integration status` says: `installed`, `out of date` or `not
installed`. `space` or `enter` puts crystal's hooks in the agent's own settings, or brings them up to date, and on
one installed, takes them out again; the view says what's left to do, like reviewing Codex's in its `/hooks`.

## Profiles

A profile is a way of starting an agent you use often: which agent, how, with what standing instructions, and
where. The new-session panel offers your profiles first.

```toml
[[profile]]
name = "review"                            # how the panel shows it
description = "Reads the branch's diff"    # optional: shown under it in the panel
agent = "claude"                           # an agent the new-session panel knows: claude, codex, qwen, …
model = "opus"                             # optional: Claude Code's or Codex's model
effort = "high"                            # optional: Claude Code's effort: low, medium, high, xhigh or max
mode = "plan"                              # optional: Claude Code's permission mode, or Codex's approvals
args = ["--verbose"]                       # optional: more options, after those
prompt = "Review the diff on this branch." # optional: put before the task, a blank line between
postfix = "End with the risks you found."  # optional: put after the task, the same way
instructions = """
You are reviewing, not writing. Point out risks before style,
and say which lines each comment is about.
"""                                        # optional: kept for the whole session, see below
where = "worktree"                         # optional: "here" or "worktree"; else as the panel is set
launch = "background"                      # optional: "session", "task" or "background"; else as the panel is set
skip_task = false                          # optional: true starts it without asking for a task
```

`instructions` stay with the agent for the whole session, on top of its own: Claude Code gets them with
`--append-system-prompt`, and Codex as its `developer_instructions` setting (`-c`), in place of any in its own
`config.toml`; what crystal adds, about a task or the memory, comes after them. The other agents can't be
given any, so a profile for them that has some is an error. `prompt`, unlike `instructions`, is only the start
of the first message.

`launch` says how the profile is meant to start, and choosing it in the panel sets the panel's "how" row to
match, which you can still change: `session` starts the agent in a terminal with what you typed as its first
prompt only, a session rather than a [task](tasks.md#tasks), with nothing to close; `task` starts it in a terminal as a
task, which `crystal done` closes; and `background` as a [background task](tasks.md#background-tasks), which only Claude
Code can be, so another agent starts as a task in a terminal. Left out, the panel decides as it's set: what's
given something to do is a task. `skip_task = true` starts it at once, without asking for a task: its `prompt`
and `postfix` alone are what it's asked, and as a task, what it's to do, like a profile that commits the
working tree in small commits.

`mode` is one of `acceptEdits`, `plan` or `bypassPermissions` for Claude Code, and `on-request` or `never` for
Codex. A profile is offered only when its agent is installed. One that can't start, for an agent crystal
doesn't know, with a mode that agent doesn't have, or with the name of another, is an error that says why.

`P` in the TUI lists your profiles: `Enter` changes the one the bar is on, `a` adds one, `c` copies one, and `x`
removes one once you've said `y`. Each setting is a row of the form; `Tab` goes from row to row, `←` / `→`
change a choice, and the form ends with the command the profile runs. `Enter` saves it to the config file,
changing only that profile's lines, so your comments and layout stay as they were. A change that would make
the file one crystal can't read isn't written, and the form says why.

`crystal profile` lists them, and `crystal profile show <name>` prints the command one runs, quoted the way a
shell reads it, with `<task>` where the task goes, and how it starts when it says:

```
$ crystal profile show quick
quick
agent   Codex
starts  wherever the new-session panel is set
runs    codex -a never -c 'developer_instructions="Keep changes small."' -- '<task>'
```
