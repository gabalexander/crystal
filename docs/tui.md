# The TUI

<sub>[← README](../README.md#documentation)</sub>

Run `crystal` on its own to open the TUI: every session in a sidebar on the left, and the selected one live in
a pane beside it.

- [The screen](#the-screen)
- [What each row says](#what-each-row-says)
- [The sidebar](#the-sidebar)
  - [The order](#the-order)
  - [Laying out its rows](#laying-out-its-rows)
  - [On a phone](#on-a-phone)
- [Finding with `/`](#finding-with-)
- [Splits and floats](#splits-and-floats)
- [Tabs](#tabs)
- [Layouts](#layouts)
- [A session's history](#a-sessions-history)
- [Zoom, copy mode and search](#zoom-copy-mode-and-search)
- [The mouse](#the-mouse)
- [Links](#links)

## The screen

There are no boxes. The sidebar lists each project with a thin rule after its name, its worktrees under it,
and their sessions under those, each with a mark for what it's doing and how long ago that changed. A thin
rule separates the sidebar from the panes; each pane has a header line naming its session, with where it runs
on the right. The bar along the top shows your [tabs](#tabs) and counts the sessions and how many wait on you,
and the footer says where you are and offers the keys that matter there, or, when you come back, what happened
[while you were away](events.md#timeline).

## What each row says

Each row says what its session is doing. Within a worktree the agents come first, then a `terminals` line and
the terminals: shells, and any other program that isn't an agent, drawn quieter. A project with a session
waiting on you moves to the top, and that session leads its worktree.

| Mark | Meaning |
|---|---|
| `▲` waiting | the agent is asking you something, like a permission |
| `◐` working | the agent is working on a turn; the mark turns while it does |
| `✓` done | the agent finished its turn, and you haven't looked yet |
| `▸` | running: an agent at its prompt |
| `❯` | a terminal: muted at a shell's prompt, brighter while a program runs in it |
| `■` | ended: muted when it exited well, red when it failed or [couldn't start again](sessions.md#after-a-restart); the pane's header says how |
| `◌` starting | waiting its turn to start again after crystal [restarted](sessions.md#after-a-restart) |

Each row also says what's in front in the session's terminal when its name doesn't already say it: `claude`,
`codex`, `vite`, `zsh`. crystal asks the terminal which program its keys go to, about once a second, so a
shell you typed `claude` into moves up among the agents, and back among the terminals when Claude exits. It
recognises the agents crystal can start, however they're installed: Claude Code's own binary, named after its
version, or an agent npm runs with `node`.

## The sidebar

The sidebar is 28 columns wide unless `[sidebar]` in the config says otherwise. `(` and `)` take four columns
from it or give it four, or drag the line between it and the panes with the mouse; the TUI keeps the width
you leave it at, until the config gives another. `\` folds it to a rail three columns wide, a session's mark
a row, so a session waiting on you still shows while the panes take the room; `\` again, `)`, or dragging its
edge, unfolds it. While it's folded, or the tab is zoomed, `/` brings it out over the panes to look through.

Whatever needs you, in every tab, is pinned at the top under **needs you**: the agents waiting on you, then
the sessions that couldn't start again after a [restart](sessions.md#after-a-restart), then those that finished a turn you haven't
looked at. One in another tab says which tab, and a click on it takes
you there. `u` goes to each in turn, and `U` lists them with what each waits for.

A row says what fits of what there is to say, and leaves the rest out as the sidebar narrows:

```
 app (2) ▲1 ───────────────
 web ────────────── 3 to do
   ⎇ fix-login    +3 ±42 ↑2
     ◐ fixer opus 5.5    4m
       indexing 40%
```

- **An agent's model**, after its name: the `--model` it was started with, then what its hooks say as it starts,
  then each switch it makes. Claude Code's `/model` fires no hook, so crystal reads the switch from its
  conversation's transcript as it's written, only what was added since it last looked; Codex's model is in each
  turn's context in its rollout. Claude's names are shortened, `claude-opus-5-5` to `opus 5.5`. The pane's header
  says it too, on the right.
- **A worktree's changes**, on its line: `+3` files changed and not committed, new ones included, `±42` lines
  changed in them, and `↑2 ↓1` commits ahead of its upstream and behind it. git counts them off to the side,
  without taking the index's lock from under an agent's own git, for the worktrees on screen: as one appears,
  as soon as a session in it starts or stops doing something, and every ten seconds meanwhile.
- **A line an agent or a script reported** with [`crystal report --line`](agents.md#a-line-on-its-row), under its row.
- **A folded project**: `h` folds the selected session's project down to its heading, `l` (or `Enter` on the
  heading) unfolds it, and a click on a project's heading does either. The heading says how many of the tab's
  sessions it holds, and a count of those waiting on you, working, done and failed, by their marks. The
  selection can rest on it, and `j` / `k` step over it as one row; a session picked by name, with `/`, `u` or a
  click on its pinned row, unfolds its project. Folds are kept across restarts, as the width is.

```toml
[sidebar]
width = 32             # 16 to 80 columns
folded = false         # start folded
fold = "marks"         # what folding keeps: "marks", or "hidden" for nothing
needs_you = true       # pin what needs you at the top
order = "attention"    # what waits on you goes first; "stable" moves nothing as statuses change
phone_width = 64       # one column at this width or narrower; 0 never
```

### The order

Projects come in the order their first sessions were made, and within a worktree, sessions in the order they
were made, its agents before its terminals. Ordered by attention, as they are unless `order = "stable"` under
`[sidebar]` says otherwise, a project with an agent waiting on you goes to the top for as long as it waits,
and that agent leads its worktree: what needs you is at the top, still beside the work it belongs to. Stable,
nothing moves as statuses change, and what waits on you shows in the pinned **needs you** rows instead. The
settings view's Look tab switches it.

Either way, the order is yours to change. `Alt+k` and `Alt+j` (or `Alt+↑` and `Alt+↓`) move the selected session
a place up or down among the sessions beside it in its worktree, agents among agents and terminals among
terminals; on a folded project's heading, they move the project. `Alt+K` and `Alt+J` (or `Alt+Shift+↑` and
`Alt+Shift+↓`) move the selected session's project among the projects. With the mouse, drag a session's row
onto another's in its worktree, or a project's heading onto any row of another project: it goes before the one
it's let go of, or after it when that's further down. A click on a heading still folds it; it folds as the
button comes up, where it went down. `crystal sidebar move` does the same from the command line. A project
with no sessions keeps the place it was put in, and a session renamed keeps its place; what you haven't placed
comes after what you have, in the order it was made. Ordered by attention, what waits on you still goes first,
and a move it holds back says so. The order is kept in crystal's database across restarts, cold ones too, and
a step of a [flow](flows.md) keeps its place in its run. On a Mac, `Alt` is `Option` once your terminal sends it
as `Meta` (Terminal's and iTerm2's profiles each have a setting for it).

### Laying out its rows

A session's row, a worktree's line and a project's heading can be laid out your own way, as herdr's sidebar
rows are: each line a list of tokens, written in the order they go. `rows` lays out a session's lines,
`rows_by_agent` a particular agent's sessions in place of `rows`, by the agent's program (`claude`, `codex`,
…), `worktree_row` a worktree's line and `project_row` a project's heading. Left out, crystal draws its own.

```toml
[sidebar]
rows = [
  ["mark", "name", "agent", "gap", "when"],
  [{ token = "$load", rules = [{ gt = 80, fg = "failed", bold = true }, { gt = 50, fg = "waiting" }] }, "state"],
  ["task"],
]
worktree_row = ["mark", "name", "about", "gap", "changes", "upstream", "pull_request", "$deploy"]
project_row = ["name", "$deploy", "gap", "to_do"]

[sidebar.rows_by_agent]
codex = [["mark", "title", "model", "gap", "when"], ["line"]]
```

Tokens on one line are separated by ` · `, but for a single space after `mark`, and what follows `gap` goes at
the line's right edge (on a heading, the rule fills the room between them). A token with nothing to say is left
out with the separator before it, and a line with nothing on it goes, but a session's first, which the
selection rests on. Short of room, what's before the gap is cut first, then what's after it goes. A session's
lines past its first take the place of its task's line and its reported line, under its name: `task` and
`line` put them where you like.

| Session token | What it says |
|---|---|
| `mark` | its status's mark, in its color |
| `name` | its name |
| `title` | the title reported for it with `--title`, or else its name |
| `agent` | what's in front in it, in a word, or the agent reported with `--display-agent` |
| `model` | the model its agent runs on |
| `subagents` | how many subagents its agent has running, `+2` |
| `state` | its status in a word (`waiting`, `working`, `done`, `idle`, `running`, `ended`, `failed`, `starting`), or the label reported for it with `--state-label` |
| `when` | how long ago it changed, or the tab it's in when it's shown from another |
| `bell` | `♪` while its bell rang out of sight |
| `task` | its task: what it was asked to do, or how that went |
| `line` | the line reported for it with `--line` |
| `branch`, `project`, `tab` | its worktree's branch, its project's name, the tab it's in |
| `context` | how full a background task's conversation is, `42%` |
| `$name` | the value reported for it with `crystal report --token name=…` |

| Worktree token | What it says |
|---|---|
| `mark` | `⌂` for the main worktree, `⎇` for a linked one |
| `name` | its label, its branch, or `claude` for one Claude Code made |
| `about` | what's said after its name: its branch, what git is in the middle of |
| `branch`, `label`, `doing` | its branch, the label it was given, what git is in the middle of there |
| `changes`, `upstream` | its changes not committed, `+3 ±42`, and how far it is from its upstream, `↑2 ↓1` |
| `pull_request` | its pull request and how it stands, `#57 ✓` |
| `path`, `project` | its directory, its project's name |
| `$name` | the value reported for its project with `crystal project report --token name=…` |

A project's heading takes `name`, `to_do` (how many backlog items are left, `3 to do`), `path` and `$name`, its
project's tokens. While crystal removes a worktree, what follows its line's gap says `removing…`.

A token can be a table that styles it: `fg`, a color as `[colors]` takes one or one of the theme's by what it's
for (`text`, `muted`, `accent`, `branch`, `waiting`, `working`, `done`, `running`, `ended`, `failed`, `added`,
`removed`), which follows the theme, and `bold`, `dim` and `italic`, `true` or `false`; what's left unsaid
keeps the token's own look. Its `rules`, up to 16, each have one condition, `equals`, `contains` and
`starts_with` (case and all, unless `ignore_case = true`) or `gt` and `lt`, for a value that reads whole as a
number: the first that holds styles the token as it says, or with `hide = true`, leaves it out. `mark`, `bell`,
`changes`, `upstream` and `pull_request` take a style but no rules. A token crystal doesn't know, a rule with
no condition or two, more than 16 lines or 16 tokens on one, is an error that says which.

The pinned **needs you** rows, the folded sidebar's rail, a folded project's heading and a flow's steps keep
crystal's own look.

### On a phone

crystal works on a phone without an app: ssh into the machine your agents run on, from any ssh client, and run
`crystal` there, or `crystal ssh box` from another machine. A terminal 64 columns wide or narrower, as a
phone's is, shows one column instead of the sidebar beside the panes. While the sidebar has the keyboard, it
takes all of the screen, over the pane; `Enter` on a session gives its pane all of it instead, as though it
were zoomed, and `Ctrl+\` brings the sidebar back. The pane keeps its size either way, so its program isn't
drawn again as you go between them. `phone_width` under `[sidebar]` moves the line, and `0` keeps the sidebar
beside the panes at any width.

## Finding with `/`

`/` finds anything in the sidebar, and more, by typing a little of it. The sidebar shows only what matches,
from every tab, under its project and worktree, with the letters that matched marked. The letters only have
to turn up in order (`rfx` finds `refund-fix`), and each word you type has to turn up somewhere in what it
finds, so `pay fix` finds the fixer in the payments project. Letters type into the filter, so `↑` and `↓` (or
`Ctrl+P` and `Ctrl+N`) move among the matches; `Enter` picks one, as does a click, and `Esc` leaves the
selection where it was. What it finds, and what picking it does:

| What | Found by | Picking it |
|---|---|---|
| a session, in any tab | its name, project, branch, command, the agent in front, its tab's name, its directory, or the name or goal of the flow run it's a step of | selects it, bringing its tab to the front |
| a flow run | its name, its flow's or its goal, which find its steps too | selects the step it's at |
| a project nothing runs in, or a worktree with no sessions | its project, its branch, or its directory | puts the selection on it, where `Enter` starts something |
| an open pull request (a merge request on GitLab) | its title, number (`57` or `#57`), branch, author or project | opens it in [the pull requests view](code.md#pull-requests-and-issues) |
| an open issue, marked `issue` | its title, number (`12` or `#12`), author, labels or project | opens it in [the issues view](code.md#pull-requests-and-issues) |
| an item to do on a project's [backlog](tasks.md#the-backlog), marked `to do` | its line, number (`3` or `#3`), tags or project, or a word of its body | opens the backlog view on it |

A directory, a goal or a backlog item's body only counts where a word turns up in it whole, or nearly anything
would find it. Before you type, only sessions show; the rest join them as you type, each under its project, a
project's pull requests, then its issues, then its backlog items. The pull requests and issues are those the
sidebar already asked the forge for, and `/` asks, in the background, about the projects with nothing running
the first time it opens, so typing never waits on the forge; it asks the daemon for every project's backlog
each time it opens.

`Tab` keeps to the sessions with one status, the footer saying which, round `waiting`, `working`, `done` (a
finished turn nobody has looked at), `idle` (at the prompt) and `ended`, then back to all of them; `Shift+Tab`
goes the other way. Typing narrows them further. Projects, pull requests, issues and backlog items have no
status, so they don't show while it keeps to one.

## Splits and floats

A split keeps a session on screen while the selection moves on. `s` splits the selected session off into a
pane of its own: it stays where it is, and the pane that follows the selection takes the other half, to show
the next session you select, beside it when both halves can be at least 80 columns wide and below it when not.
`|` splits side by side and `-` one above the other, whatever the width. On a session that has a pane of its
own already, they split that pane: the selection's pane comes beside it, and what it showed stays where it was,
split off. Split any pane again, as often as there's room, to lay the panes out any way you like. `s` on a
session split off closes its split, and the pane beside it takes the room. Each pane's session is sized to its
pane. A session nobody has looked at yet is 120 columns by 40 rows, so an agent started in the background lays
its output out for a real screen; once seen, it keeps the size of the last pane it was in.

While the selection is on a session with a pane of its own, the selection's pane goes on showing the last
session it showed, so no session is drawn twice and going from pane to pane changes none of them. Shift and an
arrow select the session in the pane that way, as `j` and `k` would, and `Enter` types into it. `Tab` from the
sidebar goes on to the pane after the one you typed into last, so `Tab`, then `Ctrl+\`, then `Tab` again walks
through them all.

`H`, `J`, `K` and `L` swap the selected session's pane with the one to its left, below, above or right; or take
a pane by its name, in its header line, with the mouse and let go over another, and the two swap places. To
change how big they are, drag the rule between two panes side by side, or the header line of a pane below
another, beside its name; or press `R` for resize mode, where the footer shows its keys until you're done:

| Key | In resize mode |
|---|---|
| `h` `j` `k` `l`, the arrows | move a border of the selected session's pane that way: the one on that side, which it grows into, or else the one on its other side, which it shrinks from |
| `Shift` and an arrow | go on to the pane that way |
| `=` | even the panes out: those in a line the same way get the same room each |
| `Esc`, `Enter`, `q`, `R` | done |

`[keys]` can change these too, as `resize-left` to `resize-done`: see [keys and commands](keys.md#modes-and-views).
No pane gets smaller than 12 columns or 3 rows. Each tab keeps its panes, how they're split and how big each
is, the next time you open the TUI too; panes kept by an older crystal, as a list, come back two side by side,
or more stacked.

`F` floats the selected session over the panes, in a frame of its own in the middle of them, and hands it the
keyboard: a shell to run something in, or an agent to answer, without changing the panes under it. The session
is sized to the frame. `Ctrl+\` takes you back to the sidebar and leaves it floating, over whatever the
selection shows; `Tab` or a click gets back into it, as with any pane. `F` again puts it back among the others,
whichever session is selected. A tab has one float at most, kept with the tab like its panes. A session split
off floats up out of its split, and `s` on the one floating puts it down into a split, where the selection's
pane was.

## Tabs

A tab is a space of its own: it holds its own sessions, and the sidebar lists only the sessions of the tab
you're in, with that tab's panes beside them. Keep the agents on one feature in one tab, a dev server and its
logs in another, a review in a third, and switch between them.

The tabs sit in the bar along the top, numbered, the one you're in standing out; `[tab_bar]` in the
[settings](configuration.md#terminals-the-window-and-the-tab-bar) puts the bar over the footer instead, leaves it out while
there's only one tab, and shows what you like at its right, after the count: the time, this machine's name,
a command's answer. `t` makes a new one, starts
your shell in it, in the selected session's directory, and takes you there. `[` and `]` go to the tab before
and after, `1` to `9` straight to the first nine, and a click on a tab goes there too. `{` and `}` move the
tab you're in one place to the left or right. Each tab keeps its own selection and panes, split and sized its
own way. `T` names the tab you're in, and the bar shows the name after its number; with too many to fit, the
bar shows only the numbers, and with more still, as many as fit around the tab you're in.

A tab with something going on in it shows that on its label, the way the sidebar marks a session: `▲` when an
agent in it is waiting on you, `✓` when one has finished a turn you haven't looked at, the turning `◐` while
one works. `u` looks through every tab for the next session that needs you, and takes you to its tab; the
sidebar [pins](#the-sidebar) whatever needs you, from every tab, at its top; and `/` finds a session in any tab.

Every session is in exactly one tab. A session you start from the TUI goes in the tab you're in, and so does
one started any other way, from the command line or another TUI, unless it's a step of a [flow](flows.md), which
goes in the tab with the rest of its run. `>` moves the selected session to another tab: press the tab's
number next, or `t` to make a new tab for it. `&` closes the tab you're in and kills the sessions in it, once
you've said `y`; an empty tab closes at once. The command line makes, names and closes tabs too: see
[laying out the TUI](driving.md#laying-out-the-tui). There's always one tab, and as many more as you like. They're kept
in crystal's database, so they're there when you open the TUI again.

## Layouts

A layout is your tabs saved under a name, to put back later: each tab's name and sessions, its panes with how
they're split and how big each is, its float, whether it's zoomed, and which tab was in front. `S` lists them, the one saved
last first, each with how many tabs and sessions it has and how many of those have gone since. `s` saves your
tabs as they are now under a name you type, in place of the layout of that name if there's one already;
`Enter` puts your tabs back the way the layout has them; `x` removes it, once you've said `y`; `Esc` closes the
list.

A layout keeps what starts each of its sessions again: the command it was started with and its directory.
Restoring one starts again the sessions it names that have gone since, each under its name, then arranges
the sessions that way; an agent given a first prompt starts without it, so it isn't asked that again, and a
background task, with no terminal of its own, isn't started. Those it can't start, their directory gone, say,
are left out, and those it doesn't name join the tab in front. The tabs a restore
replaces are kept, at the top of the list as `↶ before` the layout's name, so `Enter` on that takes you back,
once. Layouts are kept with your tabs, in crystal's database. To keep one in a file instead, to share or take
to another machine, `crystal layout export` writes one and `crystal layout apply` puts it back: see [laying out
the TUI](driving.md#laying-out-the-tui).

## A session's history

Each session keeps the last 10,000 rows that scrolled off its screen (`scrollback_lines` in the
[settings](configuration.md)), so a pane can page back through what an agent wrote before you opened it. The title says how far back you are (`↑ 120 lines`), new output doesn't pull
you away while you read, and typing into the session brings you back to live. That includes agents that print
inline through a scroll region, like Codex. To search that history, or copy from it, there's
[copy mode](#zoom-copy-mode-and-search), and `e` opens it in your editor.

## Zoom, copy mode and search

`z` zooms the selected session's pane: it takes the whole screen between the top bar and the footer, and the
sidebar and the other panes step aside until `z` puts them back. The session is sized to the zoomed pane, as
any pane's is. The keyboard stays where it was, so `j` and `k` go on choosing the session the pane shows, and
`Enter` types into it. `/` brings the sidebar out over the pane while you look through it. Each tab is zoomed
or not on its own, and stays that way when you open the TUI again.

`v` puts the selected session's pane in copy mode: a cursor of its own that moves over the screen and back
through the history with vi's keys, while the program goes on running and its output goes on showing. It
works on a session that has ended too, on the last it showed. `Ctrl+\` leaves copy mode for the sidebar.
`crystal attach` has copy mode too: `Ctrl+B` then `v`, and there `Ctrl+\` detaches.

| Key | In copy mode |
|---|---|
| `h` `j` `k` `l`, the arrows | move a character or a row; up past the top goes back through the history |
| `w` / `b` / `e` | to the next word, back a word, to the end of the word; `W` `B` `E` take only blanks to end one |
| `0` / `^` / `$` | to the start of the line, its first character, its end |
| `H` / `M` / `L` | to the top, middle or bottom row showing |
| `{` / `}`, `%` | to the blank line before or after the paragraph; to the bracket that pairs with this one |
| `gg` / `G` | to the top of the history, or the bottom of the screen |
| `Ctrl+U` / `Ctrl+D` | half a screen back, or forward |
| `Ctrl+B` / `Ctrl+F`, `PageUp` / `PageDown` | a screen back, or forward |
| `v` or `Space` / `V` / `Ctrl+V` | select from here as the cursor moves: characters, whole lines, or a block |
| `y` or `Enter` | copy the selection, and leave copy mode |
| `Y` | copy the line the cursor is on, and leave copy mode |
| `/` / `?` | search down, or up, as you type: each key goes to the nearest match; `Enter` keeps it, `Esc` goes back |
| `n` / `N` | the next match the same way, or the other way |
| `o` | open the link under the cursor, a URL or a file's path, as `Ctrl`+click does, and leave copy mode |
| `Esc` | drop the selection, then the search, then leave copy mode |
| `q`, `Ctrl+C` | leave copy mode |

A search finds what you type as it's written, letter for letter, across lines that wrapped, and ignores case
unless you type a capital. It searches as you type: at each key the cursor goes to the nearest match from where
it was when you pressed `/` or `?`, that way, or back there while nothing matches, and every match on screen is
marked, the one the cursor is on most of all. The search line says which it is, `3 of 12`, counted from the top
of the history, or `no match`. `Enter` keeps it, and the footer says it again (`of: 3 of 12`); `Esc` goes back
to where the search began, and to the search before, for `n`. A search goes round: down past the last match,
it starts again at the top.

What you copy goes to your clipboard. On your own machine crystal hands it to `pbcopy` on macOS, or to
`wl-copy`, `xclip` or `xsel` on Linux. Over ssh, or with none of those, it asks the terminal you're in to take
it, with OSC 52, which puts it on the clipboard of the machine your terminal runs on: Ghostty, kitty, WezTerm,
Alacritty, foot and Windows Terminal do; iTerm2 once you allow it in its settings; macOS's Terminal doesn't.

What a program in a session copies goes there too, as it would in a terminal of its own: Claude Code, vim with
an OSC 52 clipboard and tmux with `set-clipboard on` ask their terminal to copy with OSC 52, and the pane or the
`crystal attach` showing the session puts it on your clipboard the same way, on the machine you're at, over
[`crystal ssh`](servers.md#other-machines) too. A program can never read your clipboard. One out of sight, its session in
no pane and no attach, has its copy dropped: nobody saw what it was, and it would take the place of what you
copied meanwhile. The TUI's footer says so (`builder copied out of sight: not put on your clipboard`), and the
event log gets a `session.copy_dropped`; look at the session and copy again. `[clipboard] allow_programs = false`
in the [settings](configuration.md) keeps programs off your clipboard.

`e` opens the selected session's history in your `$EDITOR` (or `vi`): everything its pane can page back through,
then what's on its screen, as plain text, with a line that wrapped onto several rows made whole again. It opens
as a session of its own, in the session's directory, called after it (`claude-history`), and takes the keyboard,
so you can search, copy or save from it with the editor you know. It works on a session that has ended too. The
text is a copy, written beside the daemon's state in `~/.local/state/crystal/history/`: the session goes on as
before, and editing the file changes nothing in it.

`clear-pane` clears the selected session's screen and its history, but for the line its cursor is on, which goes
to the top: a shell's prompt and what's typed on it, the rows it wrapped onto too. Its program is sent nothing,
not even a `Ctrl+L`, so a command half typed stays as it was; every pane and `crystal attach` showing it is
cleared with it, and a program on the alternate screen, like an editor or `less`, is left alone, as it draws all
of it. What's cleared can't be had back, so it has no key until you give it one (`clear-pane = "ctrl+l"` under
`[keys]`); the [command list](keys.md#the-command-list) and a pane's right-click menu have it, and `crystal clear`
does the same from a shell, for the session it runs in or the one `-n` names.

## The mouse

The mouse works too. Click a session in the sidebar to select it, or click a pane to type into it. The wheel
moves the selection over the sidebar, and scrolls a pane through its history, three lines a notch. A right click
opens a menu of what you can do with what it's on: a session, a worktree or a project in the sidebar, a tab, or
a pane. Each item is a key from [the sidebar's table](keys.md#in-the-sidebar), shown beside it, and does just what that key would there;
choose one with a click, `Enter`, or its key, and `Esc` or a click elsewhere closes the menu. Drag across a pane
to select text: it goes to your clipboard as you let go, and stays marked until you click or type. A
double-click selects a word, where a path is one word and a blank, a comma, a quote, a bracket or a colon ends
one, and a triple-click the whole line, across the rows it wrapped onto; drag on from either and it takes in
whole words, or lines. Drag past the top or bottom of a pane and its history scrolls under the selection, faster
the further past, for as long as you hold it there; the wheel scrolls it too. A program that asks for the mouse
itself, like `vim` with `set mouse=a` or `htop`, gets the clicks, drags and the wheel in its pane while that pane
has the keyboard; there, your terminal's own selection still works with a key held: `Shift` in most terminals,
`Option` in iTerm2 and Terminal on macOS. A program on the alternate screen that doesn't ask, like `less`, a git
pager or `man`, keeps no history for the wheel to scroll, so a notch sends it the arrow keys instead, Up or Down
as many times as it would scroll lines, the way xterm's alternate scroll does; a program that turns that off
(`\e[?1007l`) gets nothing. Copy mode and a selection being dragged keep the wheel scrolling the pane.
`crystal attach` leaves the mouse to your terminal, and has it send the arrow keys for the wheel the same way,
only while the program is on the alternate screen: a shell at its prompt doesn't page through its history as
you scroll, though that leaves the wheel nothing to do there. With `attach_capture = true` under `[mouse]`, the
attach takes the mouse itself, as the TUI does, which works in a terminal that has no alternate scroll too: a
program that asks for the mouse gets it, a pager the arrow keys, and anywhere else the wheel scrolls the
session's history, from before you attached as well, the top right saying how far back (`↑ 120 lines`) until
you type. A drag selects as it does in a pane, a double-click a word and a triple-click a line, and held on the
top or bottom row it scrolls the history under the selection. What it selected goes to your clipboard as you
let go and stays marked until you type, or with `copy_on_select = false` waits in
[copy mode](#zoom-copy-mode-and-search) for `y`. Your terminal's own selection then takes a key held, as in the
TUI.

Beside each pane's screen, in a column of its own, a scrollbar shows where in its history the pane is, once it
has some: drag its thumb to scroll, or click the track and the thumb jumps there. The wheel over it scrolls the
pane too.

`[mouse]` in the [settings](configuration.md) changes all this. `copy_on_select = false` keeps what you select from
your clipboard as you let go: the pane goes into [copy mode](#zoom-copy-mode-and-search) with it still selected,
where `y` copies it, the keys change it first, and `Esc` drops it, and the keyboard goes back to where it was
after. `scroll_lines` is how far a notch of the wheel scrolls, and `scrollbars = false` gives the scrollbar's
column back to the pane. `capture = false` leaves the mouse to your terminal altogether: its own selection
works with no key held, but nothing in crystal answers a click, and no program in a pane gets one either.

A terminal can forget it was asked for the mouse: iTerm2's Session ▸ Reset does, and so does a stray reset
written to it, after which every click goes to the terminal and the wheel scrolls its own scrollback. The TUI
asks for the mouse again every two seconds, with bracketed paste, focus reports and the keys it tells apart,
though never in the middle of a drag, so the mouse comes back on its own; resizing the window goes back to the
TUI's screen too, should the terminal have left it, and draws all of it again.

## Links

`Ctrl`+click opens a link in a pane, whoever has the mouse there: a URL written out in the text (`http://`,
`https://` or `file://`), whole across the rows it wrapped onto, or a hyperlink a program wrote (OSC 8), which
goes where it points rather than to the text it shows. Hold `Ctrl` and move the mouse over one to see it
underlined. It opens in your browser, with `open` on macOS or `xdg-open` on Linux, unless a
[plugin takes links like it](plugins.md#link-handlers). Over ssh a browser opened on the other machine would be no use to
you, so the link goes on your clipboard instead, as copying does. Your terminal has to hand the click to
crystal: macOS's Terminal keeps `Ctrl`+click for its own menu, and iTerm2 does unless you turn that off in its
settings. Copy mode's `o` opens the link under its cursor, from the keyboard.

A file's path in a pane's text is a link too, like the `src/app.rs:42` an agent prints: `Ctrl`+click opens it
in your `$EDITOR` (or `vi`) at that line, as a session of its own beside the one that printed it, the way
[the file finder](code.md#the-file-finder-and-the-tree-browser) opens one, and `;` takes you back. The line can follow
the path as compilers and agents write it, `src/app.rs:42` or `src/app.rs:42:7`, as MSVC and TypeScript do,
`src/app.ts(42,7)`, or as GitHub does, `src/app.rs#L42`; a diff's `a/` or `b/` before it, and a bracket, a quote
or a full stop around it, are left out. A path is looked for where its session runs, then at the top of its
worktree, or where it says when it's absolute or starts with `~/`, and only one that's a file there is
underlined and opens.

In `crystal attach`, a click on a link is your terminal's to open, as it finds them in the text, and copy
mode's `o` opens the URL under its cursor as the TUI does; on a file's path, with no editor beside the session
to open it in, it puts the file's whole path and its line on your clipboard.
