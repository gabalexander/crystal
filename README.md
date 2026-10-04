<h1 align="center">crystal</h1>

<p align="center"><strong>One terminal for all your coding agents.</strong></p>

<p align="center">
  <a href="#install">install</a> · <a href="#usage">usage</a> · <a href="#how-it-works">how it works</a> · <a href="#roadmap">roadmap</a> · <a href="#development">development</a>
</p>

<p align="center">
  <a href="https://github.com/gabalexander/crystal/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/gabalexander/crystal/ci.yml?branch=master&label=ci&labelColor=333333&color=666666" alt="CI status" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-666666?labelColor=333333" alt="MIT license" /></a>
  <img src="https://img.shields.io/badge/built%20with-rust-666666?labelColor=333333&logo=rust" alt="built with Rust" />
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20linux-666666?labelColor=333333" alt="macOS and Linux" />
</p>

---

crystal runs Claude Code, Codex, Cursor and any other agent CLI side by side, across all your projects and git
worktrees. The agents keep working after you close it, and you can always see which one is waiting on you.

> [!NOTE]
> crystal is young. Everything below works, but expect rough edges and changes between versions. The screen
> reading in particular follows what today's agents draw, and will need keeping up with them.

- **Agents outlive the window** — a background daemon owns every terminal, the way tmux does. Close crystal or
  drop your SSH connection and the agents carry on; reattach later and the scrollback is all there.
- **See who's waiting at a glance** — each session shows whether its agent is working, waiting for input, done
  or idle. crystal reads that from the agent's hooks when it has them and from its screen when it doesn't, and
  waiting sessions float to the top.
- **Every project in one list** — projects, their git worktrees and the sessions in each, in one sidebar you
  drive from the keyboard, with the selected session live next to it.
- **One worktree per agent** — give an agent its own branch and `git worktree` in one key, so agents working in
  parallel never edit the same checkout.
- **Conversations survive restarts** — when crystal comes back up, each agent reopens its previous
  conversation. An upgrade doesn't even stop them: the daemon hands every running program over to the new
  crystal.
- **Agents can run agents** — from the CLI or the socket, one agent can start another, send it a task, wait for
  it to finish and read what it said.
- **Bring any agent** — Claude Code, Codex, Cursor, OpenCode, or any program that runs in a terminal, run
  exactly as you'd run it yourself. An agent crystal doesn't know can [tell it what it's
  doing](#teaching-crystal-about-your-agent), and how to pick its session up again.
- **A single Rust binary** — no Electron, no browser, nothing to host. It works in the terminal you already
  have.

## Install

On macOS or Linux, on Apple silicon, Intel or ARM:

```sh
curl -fsSL https://raw.githubusercontent.com/gabalexander/crystal/master/install.sh | sh
```

It downloads the latest release, checks it against its checksum, and puts `crystal` in `~/.local/bin`.
`CRYSTAL_VERSION=0.1.0` picks a release, and `CRYSTAL_INSTALL_DIR` another directory. With Claude Code on the
machine, it also installs [the skill](#a-skill-for-claude-code) that teaches Claude Code to drive crystal;
`CRYSTAL_NO_SKILL=1` leaves it out. The Linux builds are static, so they run on any distribution.

Or build it from source, with Rust 1.88 or newer:

```sh
git clone https://github.com/gabalexander/crystal
cd crystal
make install    # into ~/.local/bin; make install PREFIX=/usr/local for /usr/local/bin
```

or `cargo install --git https://github.com/gabalexander/crystal`.

There's one binary: crystal starts its daemon in the background, from the same binary, the first time it's
needed. A daemon that's already running goes on running the old crystal until it's restarted, so after
upgrading, run `crystal restart-server` (the install script and `make install` do it for you). It hands the
daemon over to the new crystal without stopping anything: programs go on running, background tasks go on
working, and every screen keeps what it showed, history and all. A TUI or `crystal attach` left open picks its
session up again by itself. `crystal restart-server --cold` stops the daemon and starts it again instead, as a
daemon from before handovers is restarted: running sessions come back, Claude Code in its conversation, other
programs from the start. A crystal that finds a daemon of another version says so, rather than misunderstanding
it; a TUI left open on an older crystal asks you to start it again.

## Usage

Run `crystal` on its own to open the TUI: every session in a sidebar on the left, and the selected one live in
a pane beside it.

There are no boxes. The sidebar lists each project with a thin rule after its name, its worktrees under it,
and their sessions under those, each with a mark for what it's doing and how long ago that changed. A thin
rule separates the sidebar from the panes; each pane has a header line naming its session, with where it runs
on the right. The bar along the top shows your [tabs](#tabs) and counts the sessions and how many wait on you,
and the footer says where you are and offers the keys that matter there, or, when you come back, what happened
[while you were away](#timeline).

| Key | In the sidebar |
|---|---|
| `j` / `k`, `↓` / `↑` | select a session, or a worktree with no sessions |
| `Enter` | type into the selected session, or start an ended one again, once you've said `y`; on a worktree with no sessions, start one there |
| `s` | split the selected session off into a pane of its own, beside its pane or below it, or close its split |
| `\|` / `-` | split the selected session's pane in two, side by side or one above the other |
| `Shift+arrows` | select the session in the pane to the left, right, above or below |
| `F` | float the selected session over the panes, and type into it; again, put it back |
| `H` / `J` / `K` / `L` | swap the selected session's pane with the one to its left, below, above or right |
| `R` | resize mode: move the borders of the selected session's pane with the keys |
| `z` | [zoom](#zoom-copy-mode-and-search) the selected session's pane to take the whole screen, or put it back |
| `Tab` / `Shift+Tab` | type into the next pane, or the one before |
| `PgUp` / `PgDn` | page the selected session's pane back through its history, or forward to live |
| `e` | open the selected session's [history](#zoom-copy-mode-and-search), and what's on its screen, in your `$EDITOR` |
| `v` | [copy mode](#zoom-copy-mode-and-search) in the selected session's pane: select, search its history, copy |
| `t` | make a new [tab](#tabs) with a shell in it, and go to it |
| `T` | name the tab you're in |
| `&` | close the tab you're in, and kill its sessions once you've said `y` |
| `[` / `]`, `1-9` | go to the tab before or after this one, or to the tab with that number |
| `>` | move the selected session to another tab: then a tab's number, or `t` for a new one |
| `S` | your saved [layouts](#layouts): save your tabs as one, or put them back the way one has them |
| `n` | start a new session from [the new-session panel](#starting-a-session), and type into it |
| `w` | the same, in a new worktree on a branch with a made-up name, like `brave-otter` |
| `W` | remove the selected worktree, once nothing runs in it and you've said `y` |
| `r` | rename the selected session |
| `x` | kill the selected session, once you've said `y` |
| `u` | select the next session that needs you: waiting on you first, then done |
| `U` | list everything that [needs you](#timeline), in every tab, and answer a permission or a gate where it stands |
| `a` | the [timeline](#timeline): what happened, the newest first, as it happens |
| `/` | find a session by typing a little of its name, project, branch or command |
| `o` | open the pull request of the selected session's branch in your browser |
| `O` | list the open [pull requests](#pull-requests-and-issues) of the selected session's project: read one, see its diff, comment, or start an agent in its worktree |
| `i` | list the open [issues](#pull-requests-and-issues) of the selected session's project: read one, comment, edit it, or start an agent on it |
| `b` | open the selected session's project's [backlog](#the-backlog) |
| `c` | close the selected session's [task](#tasks): done or failed, with a line on how it went |
| `y` / `n` / `Y` | on a [background task](#background-tasks) asking for a permission: allow it, deny it, or allow it always; elsewhere `n` is a new session |
| `g` | on a step of a [flow](#flows): go on past its gate, or run a step that failed or was cut short again |
| `f` | on a step of a flow waiting at its gate: send it back, with notes on what to do differently |
| `d` | show what changed in the selected session's worktree: [the diff](#the-diff) |
| `p` | find a file in the selected session's worktree and edit it: [the file finder](#the-file-finder-and-the-tree-browser) |
| `E` | browse the selected session's worktree as a tree of its files, each previewed beside it: [the tree browser](#the-file-finder-and-the-tree-browser) |
| `G` | search the files of the selected session's worktree as you type, and edit one where it's found: [find in files](#find-in-files) |
| `B` | move the selected session's worktree onto another branch, or a new one: [the branch switcher](#the-branch-switcher) |
| `m` | what the selected session's project has remembered: [memory](#memory) |
| `P` | list your [profiles](#profiles), and add, change, copy or remove one |
| `X` | list the [plugins](#plugins): switch them on and off, run their actions and open their panes |
| `,` | open the [settings](#the-settings-view): notifications, the theme, and how memory learns and searches, each changed as you go |
| `?` | show every key, in the sidebar, in a pane, in a question and with the mouse: a page at a time when they don't all fit, `→` and `←` (or `Space`, `PgDn` and `PgUp`) turning the pages |
| `q` | quit; the sessions keep running |

While you're typing into a session, every key goes to it, `Tab` included, except `Ctrl+\`, which takes you
back to the sidebar, and `Shift+PageUp` / `Shift+PageDown`, which page through the pane's history. Some
terminals keep `Shift+PageUp` for their own scrolling; `Ctrl+\` and then `PageUp` does the same.

A program that asks for the [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/), as
Codex does, gets its keys that way, in a pane, through `crystal attach` and from `crystal send-keys`: keys the
old way can't tell apart, like `Esc`, `Shift+Enter` or `Ctrl+I` and `Tab`, reach it as themselves. From your
keyboard that takes a terminal that speaks the protocol too, like Ghostty, kitty, foot or Alacritty; in any
other, keys arrive the old way.

Each session keeps the last 2,000 rows that scrolled off its screen, so a pane can page back through what an
agent wrote before you opened it. The title says how far back you are (`↑ 120 lines`), new output doesn't pull
you away while you read, and typing into the session brings you back to live. That includes agents that print
inline through a scroll region, like Codex. To search that history, or copy from it, there's
[copy mode](#zoom-copy-mode-and-search), and `e` opens it in your editor.

The mouse works too. Click a session in the sidebar to select it, or click a pane to type into it. The wheel
moves the selection over the sidebar, and scrolls a pane through its history. Drag across a pane to select
text: it goes to your clipboard as you let go, and stays marked until you click or type. A program that asks
for the mouse itself, like `vim` with `set mouse=a` or `htop`, gets the clicks, drags and the wheel in its pane
while that pane has the keyboard; there, your terminal's own selection still works with a key held: `Shift` in
most terminals, `Option` in iTerm2 and Terminal on macOS.

`Ctrl`+click opens a link in a pane, whoever has the mouse there: a URL written out in the text (`http://`,
`https://` or `file://`), whole across the rows it wrapped onto, or a hyperlink a program wrote (OSC 8), which
goes where it points rather than to the text it shows. Hold `Ctrl` and move the mouse over one to see it
underlined. It opens in your browser, with `open` on macOS or `xdg-open` on Linux, unless a
[plugin takes links like it](#link-handlers). Over ssh a browser opened on the other machine would be no use to
you, so the link goes on your clipboard instead, as copying does. Your terminal has to hand the click to
crystal: macOS's Terminal keeps `Ctrl`+click for its own menu, and iTerm2 does unless you turn that off in its
settings. Copy mode's `o` opens the link under its cursor, from the keyboard. In `crystal attach`, links are
your terminal's to open, as it finds them in the text.

A split keeps a session on screen while the selection moves on. `s` splits the selected session off into a
pane of its own: it stays where it is, and the pane that follows the selection takes the other half, to show
the next session you select, beside it when both halves can be at least 80 columns wide and below it when not.
`|` splits side by side and `-` one above the other, whatever the width. On a session that has a pane of its
own already, they split that pane: the selection's pane comes beside it, and what it showed stays where it was,
split off. Split any pane again, as often as there's room, to lay the panes out any way you like. `s` on a
session split off closes its split, and the pane beside it takes the room. Each pane's session is sized to its
pane.

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

The sidebar groups sessions by project, then by worktree: `⌂` marks a repository's main worktree and `⎇` a
linked one, each named by its branch. Sessions outside any repository come last, under their directory.
`w` makes its worktree in the selected session's project, or in the repository you started `crystal` in.

A linked worktree with no sessions left stays at the end of its project, with a `· no sessions` row under it,
until it's removed: it's still on disk, maybe with work in it. Select it, and `n` or `Enter` starts something
there, `d` and `p` show its changes and files, and `W` removes it. It shows in every tab its project has
sessions in. One made or removed outside crystal comes or goes within a few seconds.

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
| `■` | ended: muted when it exited well, red when it failed; the pane's header says how |

Each row also says what's in front in the session's terminal when its name doesn't already say it: `claude`,
`codex`, `vite`, `zsh`. crystal asks the terminal which program its keys go to, about once a second, so a
shell you typed `claude` into moves up among the agents, and back among the terminals when Claude exits. It
recognises the agents crystal can start, however they're installed: Claude Code's own binary, named after its
version, or an agent npm runs with `node`.

crystal knows what an agent is doing in two ways. When it starts Claude Code itself, it adds hooks with
`--settings`, so your settings files are left alone and your own hooks still run. And for every session it
reads the screen: the spinner an agent puts in its title, "esc to interrupt" while it works, the question
it asks before a command. The screen covers agents without hooks, like Codex or a Claude you started from a
shell, and what hooks never say: a turn you cut short with Esc, or work carrying on once you've said yes.
The screen only counts while an agent is in front: a shell or a build printing an agent's words never shows
as waiting, and when an agent exits back to its shell, what it was doing goes with it.

When a session comes to need you while you're looking elsewhere (its agent asks you something, or finishes a
turn nobody was watching), crystal shows a desktop notification, like "claude-2 is waiting on you · app
fix/login". It uses macOS's own notifications, or `notify-send` on Linux when it's installed. You're told once
each time a session comes to need you, and never about a session you're watching. `u` in the TUI takes you to
it, and `U` [lists everything](#timeline) that needs you.

`/` finds a session by typing a little of it. The sidebar shows only the sessions that match, under their
project and worktree, with the letters that matched marked in each name. The letters only have to turn up in
order (`rfx` finds `refund-fix`), and each word you type has to turn up in the name, project, branch or
command, so `pay fix` finds the fixer in the payments project. Letters type into the filter, so `↑` and `↓`
(or `Ctrl+P` and `Ctrl+N`) move among the matches; `Enter` selects one and `Esc` leaves the selection where it
was.

For a project on GitHub or GitLab, each worktree line shows its branch's open pull request, `#57` (a merge
request, `!57`, on GitLab), with a mark for what matters most about it; `o` opens it in your browser, `O` lists
the project's pull requests and `i` its issues: see [pull requests and issues](#pull-requests-and-issues).

Everything is also a command, for scripts and for agents:

```sh
crystal new claude                          # start Claude Code here and attach to it
crystal new -d -n review -c ~/code/app codex   # start one in the background, named, somewhere else
crystal new -w fix/login claude             # start one in a new worktree, on a new branch off origin's main
crystal new -w spike --base HEAD claude     # the same, its branch off the commit you're on
crystal task "update the docs"              # run Claude without a terminal, in the background (see below)
crystal result task                         # a task's answer
crystal answer task y                       # allow what a task asks for: y, n or always
crystal worktree rm fix/login               # remove that worktree, once nothing runs in it
crystal ls                                  # list sessions and how they're doing
crystal ls --json                           # the same, as JSON, for scripts and agents
crystal attach review                       # show a session; Ctrl+\ hands your terminal back
crystal send review "check the diff"        # type into a session and press Enter
crystal send-keys review 1                  # press keys: an answer, Enter, Escape, C-c, Up…
crystal wait review                         # block until its agent stops working; print how it ended
crystal read review --lines 20              # print the last 20 rows of its screen
crystal read review --history               # and what scrolled off it before
crystal rename review reviewer              # give a session another name
crystal respawn reviewer                    # run an ended session again; an agent in its conversation
crystal kill review                         # stop one session
crystal kill-server                         # stop every session, and the daemon
crystal restart-server                      # restart the daemon on this crystal, say after an upgrade
crystal restart-server --cold               # stop it and start it again: sessions start again too
crystal server                              # list the servers, daemons of their own (see below)
crystal config                              # where the config file is, and the settings in effect
crystal profile                             # list your agent profiles
crystal profile show review                 # what a profile runs, and where it starts
crystal pane split review                   # show a session in a pane beside yours in the TUI (see below)
crystal tab new review                      # a new tab in the TUI, in front
crystal layout                              # the TUI's tabs and how each splits its panes
crystal skill --install                     # teach Claude Code to drive crystal (see below)
crystal mermaid docs/flow.md                # draw a page's mermaid diagrams as text (see below)
crystal ssh box                             # crystal's TUI on another machine (see below)
```

```
$ crystal ls
NAME    STATE     PID    PROJECT  BRANCH     DIRECTORY                       PROGRAM  COMMAND
claude  waiting   41210  app      main       ~/code/app                      claude   claude
fixer   working   41377  app      fix/login  ~/code/app.worktrees/fix-login  claude   zsh
review  exited 0  41388  app      main       ~/code/app                      codex    codex
```

`crystal new -w <branch>` makes the worktree beside the repository, in `<repo>.worktrees/<branch>`, with any
`/` in the branch made a `-`. A branch that does exist is checked out as it is. One that doesn't yet starts
from `origin`'s default branch, fetched first, so it's what everyone else has as `main`, not whatever your
checkout last pulled; it follows no branch of `origin`'s, so its first `git push` makes a branch of its own.
`--base <ref>` starts it somewhere else: a branch (`origin`'s copy, fetched, when it has one), a tag, a commit,
or `HEAD` for the commit you're on. `base` under `[worktrees]` in the [settings](#settings) does the same for
every new worktree you don't give a base, the TUI's included, and a project without that branch starts from the
default. Offline, it's `origin`'s branch as your last fetch left it, and with no `origin`, the commit you're on.
A [flow](#flows) makes its worktree from the branch as last fetched, without fetching.

`crystal worktree rm` (or `W` in the TUI) takes the worktree's directory or its branch, refuses while a
session is still running in it, and leaves the rest to `git worktree remove`, which keeps a worktree with
changes you haven't committed. `crystal worktree rm --force` removes it anyway, and those changes with it; `W`
asks a second time, naming them, and a second `y` does the same. Sessions that had ended in it leave the list
with it: their directory is gone, so they could never start again.

To set a new worktree up, say install its dependencies or copy in an `.env`, have a [plugin](#plugins) run a
command on `worktree.created`, and on `worktree.removed` to tidy up after it.

`crystal rename` changes what a session is called; its program and its saved place after a restart follow the
new name. `crystal respawn`, or `Enter` on an ended session in the TUI, runs its command again in the same
directory, under the same name and in the same place in the list, with your environment. Claude Code and Codex
come back in the conversation they were in, without being asked their task again. Every session's program also gets `CRYSTAL_SESSION_ID`, which stays the same
when the session is renamed, while `CRYSTAL_SESSION` keeps the name the program started under.

The first `crystal new` starts the daemon. Sessions keep running after you detach or close the terminal, and
`crystal attach` picks up exactly where the screen was. With no name it attaches to the newest session; on a
session that has ended, it prints the last screen and how the program exited.

If the daemon dies without being asked to, because it crashed or the machine rebooted, the next `crystal` starts
the sessions that were running again, in the same directories. Claude Code and Codex come back in the
conversation they were in. `crystal kill-server` is asked to stop everything, so after it nothing comes back.
The list is kept in crystal's database, `~/.local/state/crystal/crystal.db`, without the sessions' environment
variables, since those can hold secrets; a session started again gets the environment of whoever started the
daemon again.

The database is SQLite, and holds everything crystal keeps but its settings and memory: the sessions to start
again, flow runs, each project's backlog and closed tasks, and the TUI's tabs, layouts, earlier tasks and the
files you've marked reviewed in the diff. Each
change is written whole or not at all, so a crash or a power cut never leaves half of one, and a database that
can't be read stops the daemon rather than being started over. What an older crystal kept in JSON files there is
brought in the first time, and each file is kept beside it, renamed `.imported` (or `.broken`, when it couldn't
be read).

### Tabs

A tab is a space of its own: it holds its own sessions, and the sidebar lists only the sessions of the tab
you're in, with that tab's panes beside them. Keep the agents on one feature in one tab, a dev server and its
logs in another, a review in a third, and switch between them.

The tabs sit in the bar along the top, numbered, the one you're in standing out. `t` makes a new one, starts
your shell in it, in the selected session's directory, and takes you there. `[` and `]` go to the tab before
and after, `1` to `9` straight to that one, and a click on a tab goes there too. Each tab keeps its own
selection and panes, split and sized its own way. `T` names the tab you're in, and the bar shows the name after its number; with too many
to fit, the bar shows only the numbers.

A tab with something going on in it shows that on its label, the way the sidebar marks a session: `▲` when an
agent in it is waiting on you, `✓` when one has finished a turn you haven't looked at, the turning `◐` while
one works. `u` looks through every tab for the next session that needs you, and takes you to its tab.

Every session is in exactly one tab. A session you start from the TUI goes in the tab you're in, and so does
one started any other way, from the command line or another TUI, unless it's a step of a [flow](#flows), which
goes in the tab with the rest of its run. `>` moves the selected session to another tab: press the tab's
number next, or `t` to make a new tab for it. `&` closes the tab you're in and kills the sessions in it, once
you've said `y`; an empty tab closes at once. The command line makes, names and closes tabs too: see
[laying out the TUI](#laying-out-the-tui). There's always one tab, and nine at most. They're kept in crystal's
database, so they're there when you open the TUI again.

### Layouts

A layout is your tabs saved under a name, to put back later: each tab's name and sessions, its panes with how
they're split and how big each is, its float, whether it's zoomed, and which tab was in front. `S` lists them, the one saved
last first, each with how many tabs and sessions it has and how many of those have gone since. `s` saves your
tabs as they are now under a name you type, in place of the layout of that name if there's one already;
`Enter` puts your tabs back the way the layout has them; `x` removes it, once you've said `y`; `Esc` closes the
list.

A layout names sessions; it doesn't start them. Restoring one arranges the sessions running now: those it
names that have gone since are left out, and those it doesn't name join the tab in front. The tabs a restore
replaces are kept, at the top of the list as `↶ before` the layout's name, so `Enter` on that takes you back,
once. Layouts are kept with your tabs, in crystal's database.

### Zoom, copy mode and search

`z` zooms the selected session's pane: it takes the whole screen between the top bar and the footer, and the
sidebar and the other panes step aside until `z` puts them back. The session is sized to the zoomed pane, as
any pane's is. The keyboard stays where it was, so `j` and `k` go on choosing the session the pane shows, and
`Enter` types into it. `/` brings the sidebar out over the pane while you look through it. Each tab is zoomed
or not on its own, and stays that way when you open the TUI again.

`v` puts the selected session's pane in copy mode: a cursor of its own that moves over the screen and back
through the history with vi's keys, while the program goes on running and its output goes on showing. It
works on a session that has ended too, on the last it showed. `Ctrl+\` leaves copy mode for the sidebar.

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
| `/` / `?` | search down, or up, for what you type next; `Enter` searches |
| `n` / `N` | the next match the same way, or the other way |
| `o` | open the link under the cursor, as `Ctrl`+click does, and leave copy mode |
| `Esc` | drop the selection, then the search, then leave copy mode |
| `q`, `Ctrl+C` | leave copy mode |

A search finds what you type as it's written, letter for letter, across lines that wrapped, and ignores case
unless you type a capital. Every match on screen is marked, the one the cursor is on most of all, and the
footer says which it is: `of: 3 of 12`, counted from the top of the history. A search goes round: down past the
last match, it starts again at the top.

What you copy goes to your clipboard. On your own machine crystal hands it to `pbcopy` on macOS, or to
`wl-copy`, `xclip` or `xsel` on Linux. Over ssh, or with none of those, it asks the terminal you're in to take
it, with OSC 52, which puts it on the clipboard of the machine your terminal runs on: Ghostty, kitty, WezTerm,
Alacritty, foot and Windows Terminal do; iTerm2 once you allow it in its settings; macOS's Terminal doesn't.

`e` opens the selected session's history in your `$EDITOR` (or `vi`): everything its pane can page back through,
then what's on its screen, as plain text, with a line that wrapped onto several rows made whole again. It opens
as a session of its own, in the session's directory, called after it (`claude-history`), and takes the keyboard,
so you can search, copy or save from it with the editor you know. It works on a session that has ended too. The
text is a copy, written beside the daemon's state in `~/.local/state/crystal/history/`: the session goes on as
before, and editing the file changes nothing in it.

### Starting a session

`n` opens the new-session panel over the panes, titled with where the session will start: `New session ·
payments ⌂ main`. Type what the agent should do and press `Enter`; the task is its first prompt, given as one
argument. `Alt+Enter` starts a new line, and a paste keeps its lines. An empty task starts the agent with no
prompt. `↑` on the first line and `↓` on the last bring back earlier tasks: the panel keeps the last 100, in
crystal's database.

The session is named for its task: its first few words that say what it's about, like `fix-refund-rounding`
for "Fix the refund rounding, please", with `-2`, `-3` added if that's taken. Started with no task, it's named
after its program, `claude`, until its first prompt names it: Claude Code tells crystal each prompt it's sent,
and the first that has words to go on names it, a slash command never. A name you give, or rename a session
to, stays, and so does one a script has typed into the session by with `crystal send` or `send-keys`.
`crystal new` and `crystal task` name a session the same way when you don't, and print the name.
`name_from_prompt = false` in the [settings](#settings) names sessions after their programs.

Under the task, `Tab` and `Shift+Tab` go from row to row and `←` / `→` change a row's choice:

- **run**: your [profiles](#profiles), then the agents installed on your `PATH` (Claude Code, Codex, Gemini
  CLI, OpenCode, Cursor, Aider), then your shell. What you started last is chosen the next time. A profile's
  description shows under the row, and choosing it sets the rows below from it; you can still change them.
- **how**, for Claude Code: **in a terminal**, or **in the background**, as a [background
  task](#background-tasks) that needs no terminal. Only Claude Code offers it: crystal reads `claude -p`'s
  events for a task's transcript, and Codex's `codex exec` writes another kind it doesn't read yet.
- Claude Code's **model** (fable, opus, sonnet, haiku), **effort** (low to max) and **permissions** (`--model`,
  `--effort`, `--permission-mode`), or Codex's **model** and **approvals** (`-m`, `-a`), its models the ones
  `codex debug models` lists. Left at `default`, no option is added.
- **start in**: here (the selected session's worktree, or where you started `crystal`), a new worktree, or
  another project's main worktree.

A new worktree's **branch** gets a made-up name, an adjective and an animal like `brave-otter`, whatever the
task says; type in that row to change it. A made-up name is always a new branch: if it's taken, the worktree
goes on `brave-otter-2`. One you type that's a branch already is checked out as it is. `w` opens the panel
with a new worktree chosen, and `Enter` on an issue opens it ready to fix that issue, on a branch named after
it.

The panel ends with the command it runs and, for a new worktree, where. `Ctrl+E` hands that command to the
bottom line, `new session:`, to change it or run anything else: `npm run dev` or `sh -c 'make && make test'`
work there, and an empty line starts your shell. Aider can't be given a task when it starts, so for it the
task box gives way to a note.

### The diff

`d` shows what changed in the selected session's worktree, the way VS Code and GitHub show it: the changed
files on the left, each with its status (`M`odified, `A`dded, `D`eleted, `R`enamed, `U`ntracked) and how many
lines it adds and removes, and the selected file's diff on the right, with both files' line numbers, added lines
on green, removed lines on red, and the words that changed inside a line marked stronger.

It starts with what isn't committed yet, staged or not, new files included. `b` switches to the whole branch
since it left its base (where it meets `origin`'s default branch, or `main` or `master`): everything an agent
committed, as its pull request would read. From [the pull requests](#pull-requests), `Ctrl+D` shows one's own
diff here, as GitHub or GitLab has it, with no worktree needed.

Once you've read a file, `r` marks it reviewed: it sinks to the bottom of the list with a `✓`, and the next file
is selected, so `r` after `r` reads through a change. A mark is crystal's own bookkeeping, nothing staged or
committed, and it keeps until the file changes again, or HEAD moves, a commit say, when the next diff is new work
to read. They're kept with your tabs, in crystal's database, apart for each worktree's uncommitted changes and
its branch, and for each pull request's diff, where only a file changing takes a mark off.

`t` folds the list into a tree of directories, and back, and crystal remembers which you like. Every directory
starts open, a directory that holds only another reads as one row with it, like `src/tui`, and a directory's
row shows how many lines change under it, and a `✓` once every file under it is reviewed; selected, it lists them.

| Key | In the diff |
|---|---|
| `j` / `k`, `↓` / `↑` | the next or previous file |
| `Space` / `Shift+Space`, `PageDown` / `PageUp` | page through the file's diff |
| `]` / `[` | the next or previous hunk |
| `v` | side by side, the old file beside the new one, or unified again; side by side needs 120 columns |
| `b` | the branch since its base, or the uncommitted changes again |
| `r` | mark the file reviewed, or take its mark off |
| `t` | the files as a tree of directories, or a list again |
| `←` / `→`, `h` / `l` | in the tree: fold a directory or go up to the one it's in; open one or go into it |
| `Enter` | in the tree: fold or open the directory |
| `Esc` / `q` | back to the sidebar |

The wheel scrolls the diff, and moves through the files over the list; a click on a directory folds or opens it.

### Pull requests and issues

For a project whose remote is on GitHub or GitLab, each worktree line shows its branch's open pull request when
there is one: `#57`, or on GitLab, where it's a merge request, `!57`, and a mark for what matters most about it,
in this order:

| Mark | Meaning |
|---|---|
| `✗` | a check failed |
| `±` | a reviewer asked for changes |
| `draft` | it's still a draft |
| `◌` | checks are still running |
| `✓` | approved |

A pull request that's simply ready shows its number alone. `o` opens the selected session's pull request in your
browser. crystal asks the forge's own command line tool, [`gh`](https://cli.github.com) for GitHub and
[`glab`](https://gitlab.com/gitlab-org/cli) for GitLab, as soon as it sees a project and then once a minute,
so your login works as it always does and crystal never sees a token. Which forge a project is on comes from
its `origin` remote (or its first, without one): `github.com`, or a host `gh` is logged in to, like a GitHub
Enterprise, is GitHub; `gitlab.com`, or a host in glab's config, like a GitLab of your own, is GitLab.
Without the tool, or logged out of it, or for a project on neither, nothing is shown; `o`, `O` and `i` say why.

#### Pull requests

`O` lists the open pull requests of the selected session's project, with each one's author and branch, whether
it's a draft, how its checks stand and what its reviewers decided. Under the list is the selected one, read
whole: who wants to merge which branch into which, each of its checks, its description, and then its
conversation, comments and reviews in the order they came. Typing filters the list by number, title, author or
branch; `↑` / `↓` pick another, and `PageUp` / `PageDown` scroll what's under the list.

| Key | In the pull requests |
|---|---|
| `Enter` | open the [new-session panel](#starting-a-session) in the pull request's worktree, with the task `Work on pull request #57: <its title> (<its address>)` |
| `Ctrl+D` | its whole diff, in [the diff](#the-diff); `Esc` comes back to the list |
| `Ctrl+C` | comment on it: `Enter` posts, `Alt+Enter` breaks a line, `Esc` puts the comment away |
| `Ctrl+O` | open it in your browser |
| `Esc` | close the list |

A pull request's worktree is the project's worktree on its branch, when there's one already; otherwise crystal
fetches the branch from `origin` and makes one beside the others, like `app.worktrees/fix-login`, its branch
following `origin`'s so `git pull` brings what's pushed later. A pull request from a fork isn't on a branch of
the project's, so it's fetched from where the forge keeps it (`refs/pull/57/head`, or
`refs/merge-requests/57/head` on GitLab) onto a branch named for its owner, like `ana/main`, the way `gh pr
checkout` names it, so that a fork's `main` is never taken for yours. GitLab doesn't name a fork's owner, so
there it's `mr-57/main`.

A comment is posted as you, the way `gh pr comment` or `glab mr note` would. While it's on its way the box
waits; once posted, the pull request is read again with it, and if the forge refuses it, it's all still in the
box, with why.

#### Issues

`i` lists the open issues of the selected session's project, the latest to change first, with the selected
issue under the list: its text, then what's been said on it. Typing filters them by number, title, label or
author.

| Key | In the issues |
|---|---|
| `Enter` | open the [new-session panel](#starting-a-session) on a new worktree with a branch named after the issue, like `42-fix-login-redirect`, and the task `Fix issue #42: <its title> (<its address>)`, so the agent knows which issue and can read it with `gh issue view 42` or `glab issue view 42` |
| `Ctrl+C` | comment on it: `Enter` posts, `Alt+Enter` breaks a line, `Esc` puts the comment away |
| `Ctrl+E` | change its title and text: `Tab` goes between them, `Enter` saves both, `Esc` keeps them as they were |
| `Ctrl+O` | open it in your browser |
| `Esc` | close the list |

#### On GitLab

Everything above works on a GitLab project, through `glab`, with merge requests where GitHub has pull requests.
GitLab's list of merge requests doesn't say how their checks or reviews stand, so their worktree lines and rows
show only `draft`; reading one shows its pipeline as its check, and an approval in its conversation. GitLab wants
a login to read comments, even on a project anyone can see: logged out, a merge request or an issue reads
without them.

### The file finder and the tree browser

`p` finds a file in the selected session's worktree, like an editor's quick open: type a few letters of its path,
in order (`rfnd` finds `src/billing/refund.rs`), and the best matches come first, with the selected one
previewed beside the list. Letters in a file's name, at the start of a word, or next to each other count for
more. `↑` / `↓` pick another, `Enter` opens it in your `$EDITOR` (or `vi`) as a session of its own in that
worktree, named after the file, and `Esc` closes the finder. Files git ignores aren't listed.

`E` shows the same files as a tree: directories first, each folded until `→` opens it, and the selected file
previewed on the right. Whatever you type filters the tree, as the finder matches, down to the files whose
paths match and the directories they're in, all open, with the best match selected. Drag the line between the
tree and the preview to make the tree wider or narrower.

| Key | In the tree browser |
|---|---|
| `↑` / `↓` | the file or directory above or below |
| `→` / `←` | open a directory, or go into one that's open; fold it, or go up to the directory a file is in |
| `Enter` | open or fold a directory; read a file into the preview again, with whatever an agent changed since |
| `Space` / `Shift+Space`, `PageDown` / `PageUp` | page through the preview |
| `Home` / `End`, `Shift+↑` / `Shift+↓` | the top or end of the preview; a line up or down |
| `Ctrl+E` | open the file in your `$EDITOR`, the way the finder's `Enter` does |
| `Ctrl+Y` | copy the path, from the top of the worktree |
| `Ctrl+R` | a markdown file's source, or its page again |
| `Esc` | clear what's typed; then close |

The wheel scrolls the preview, and moves through the tree over it.

Both preview a file highlighted, with its lines numbered: comments, strings, numbers and keywords, in Rust,
JavaScript and TypeScript, Python, Go, C and C++, Java, Kotlin and Swift, Ruby, shell, SQL, TOML, YAML, JSON,
CSS and Dockerfiles. A binary file says so; of a long one, the first megabyte or 10,000 lines are shown.

A markdown file shows as the page it makes: headings, lists, emphasis, quotes and GitHub's alerts, tables in
aligned columns, links with their address beside them, and fenced code highlighted. `Ctrl+R`, in the finder
too, flips it to its source and back, and that holds from one file to the next. A ```` ```mermaid ```` fence is
drawn as the diagram, in boxes and arrows, with a caption under it saying what kind: sequence diagrams,
flowcharts, and state, class and ER diagrams. Any other kind, or one too wide for the preview, stays its source,
and the caption says why. What Claude says in a [background task](#background-tasks)'s transcript is a page in
the same way.

`crystal mermaid` draws a diagram on the command line the same way, from a file or standard input: a diagram, or
each mermaid fence of a markdown file, as wide as the terminal (`--width` gives another width) and in box drawing
(`--ascii` in ASCII). One that can't be drawn is printed as it is, and the command fails saying why, so an agent
can check a diagram before it writes it into a page.

```
$ printf 'sequenceDiagram\n  Alice->>Bob: hello\n  Bob-->>Alice: hi\n' | crystal mermaid
┌───────┐  ┌─────┐
│ Alice │  │ Bob │
└───┬───┘  └──┬──┘
    │  hello  │
    ├────────▶│
    │   hi    │
    │◀┄┄┄┄┄┄┄┄┤
    │         │
```

### Find in files

`G` searches the files of the selected session's worktree for what you type, with `git grep`: the files git
tracks and the new ones it would, but not those it ignores, or binary files. It searches once you've typed two
letters and stopped for a moment, the case of letters counting only when what you type has a capital in it. The
lines it finds are listed under their files, the first 500 of them, and the lines around the selected one are
beside the list. `↑` / `↓` pick another, `Enter` opens the file in your `$EDITOR` at that line, as a session of
its own like the file finder's, and `Esc` closes it. The line goes to your editor the way it takes one: `+12` for
most, `file:12` for Helix, Zed and Sublime Text, and `--goto file:12` for VS Code, Cursor and their kind.

### The branch switcher

`B` moves the selected session's worktree onto another branch, without leaving crystal. It lists the project's
branches, the one the worktree is on first, marked `●`, then the others, the latest commit first, each with how
long ago that was, then the branches on its remotes that have no branch of yours by their name. Type to filter
them, the way the file finder does; beside the list is the selected branch's last commit and what `Enter` would
do with it. `Enter` switches to it; a remote's branch, like `origin/fix-login`, becomes a branch of your own,
`fix-login`, that follows it. When nothing matches what you typed, `Enter` makes a branch by that name, from the
commit the worktree is on, and switches to it, your changes coming along. The remotes' branches are as your last
`git fetch` left them: the switcher doesn't go over the network.

When the worktree has changes not committed, the switcher stops and asks what's to become of them:

| Key | The changes are |
|---|---|
| `s` | stashed, new files too, as `crystal: main before switching to fix-login`, where `git stash list` shows them; if git won't switch, they come straight back out |
| `b` | brought along, which git does unless they're in files the other branch changes |
| `c` | committed on the branch you're leaving, new files too, with the message you type; not on a detached HEAD, where the commit would be on no branch |
| `d` | thrown away, once you've pressed `d` a second time: the changes to files git knows, unstaged first, so new files stay; if they've changed since you were shown them, it asks again |

`Enter` takes the one the bar is on, stashing at first, and `Esc` goes back to the branches. Nothing switches in
the middle of a merge or a rebase, or with conflicts, and a branch another worktree has checked out is listed but
can't be switched to, since git keeps a branch in one worktree. `Esc` while git is at it, running a commit's
hooks say, closes the switcher, and the footer says how it went. Sessions in the worktree keep running, and see
its files change. Only a project's main worktree switches: a linked one is named after the branch it was made
for, and stays on it.

### Codex

crystal reads what Codex is doing off its screen: `Working (… esc to interrupt)` while it works, and its
approval questions ("Would you like to run the following command?") while it waits on you. Codex has hooks
too, but crystal can't add its own the way it does for Claude Code: Codex only reads hooks from its config
files, never from the command line, and skips any hook you haven't reviewed in `/hooks`. Its `notify` setting
can be given on the command line, but that would replace yours, so crystal leaves it alone.

To pick a conversation up again, crystal finds the file Codex records it in,
`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-…jsonl` (`~/.codex` without `CODEX_HOME`): the one for the session's
directory that Codex started closest to when the session did, within a minute. After a restart, or with
`crystal respawn`, the session runs `codex resume <id>` with the options it was started with, but not its first
prompt again. The limits:

- Codex writes that file once it has been sent a prompt, so a Codex that was never sent one starts afresh.
- A conversation you begin from inside Codex with `/new` isn't followed: crystal picks the first one up again.
- `codex exec` and Codex's other subcommands run as they were asked, without resuming.
- A Codex you start yourself in a shell session gets its status from the screen, but isn't resumed.

### Teaching crystal about your agent

crystal knows Claude Code by its hooks, and reads Codex and the other agents it knows off their screens. Any
other agent, or a script wrapped around one, can tell crystal what it's doing itself, and how to pick its
session up again, with `crystal report`: no change to crystal, and no waiting for a release of it. Once your
agent reports, its status shows in the sidebar and in `crystal ls`, the user is told when it's done with a turn
or waits on them, `crystal wait` and the [events](#events) follow it, and, once it says how, its session comes
back in the same conversation after crystal restarts.

Every program in a session has these in its environment:

| Variable | What it is |
|---|---|
| `CRYSTAL_SESSION` | the session's name when the program started |
| `CRYSTAL_SESSION_ID` | the session's id, which a rename never changes |
| `CRYSTAL_SOCKET` | the daemon's socket, which `crystal` finds it by |
| `CRYSTAL_SERVER` | the daemon's [server](#servers), when it isn't the default one |

Report only when `CRYSTAL_SESSION_ID` is set. Outside crystal there's no one to tell, and `crystal report`
fails, saying so.

#### What it's doing

```sh
crystal report working --agent my-agent         # a turn has started
crystal report waiting -m "approve the deploy"   # it needs the user to decide; blocked says the same
crystal report idle                              # at its prompt, ready for the next
crystal report done                              # it finished a turn
```

Report `working` as a turn starts, `idle` when your agent is ready for input, and `waiting` when it needs the
user; `-m` says what for, in the notification and the event log. `idle` after `working` ends a turn, the same
as `done`: the session shows `done` until someone looks at it, and the user is told. `--agent` is the name the
sidebar and `ls` show for it, one word; without it, it's the name given before, or what's in front in the
session. A report is about the session it's run in; `-n <session>` names another.

The first report takes the session over: from then on, its reports are the session's status, and crystal
reads neither its screen nor Claude Code's hooks for it, until your agent lets go.

#### How to resume it

Put the command that picks the current session up again after `--`, with the options that session needs, so
it comes back the same:

```sh
crystal report idle -- my-agent --resume "$SESSION_ID" --model my-model
crystal report --session-only -- my-agent --resume "$NEW_ID"     # only the command, when the session changes
```

After crystal restarts, from a crash, a reboot or `crystal restart-server --cold`, the session starts again in
its directory and runs that command: typed into the session's shell when the session runs one, the way you
started your agent, or else in place of the session's own command, which `ls` still shows. Then your agent
says what it's doing again, as it did the first time, command and all. `crystal respawn` does the same for a
session that ended while your agent held it. `crystal restart-server` hands the session over instead: your
agent goes on running, and still holds it. A command that breaks these rules is refused, and the report
with it:

- Its first word is a plain command name found on the `PATH`, like `my-agent`, not a path.
- No word holds a quote (`'`) or a control character, so every shell reads it the same.
- At most 64 words, and 8 KiB in all.

`--session-only` needs your agent to hold the session already: report what it's doing first, or along with
the command. `resume_reported_agents = false` in the [settings](#settings) starts sessions again with their
own commands instead.

#### Letting go

```sh
crystal report --release
```

When your agent quits, it lets go: crystal reads the session for itself again, and forgets the agent's name
and command. Let go only when the user quits; an agent that swaps one session for another reports the new
one instead. An agent that leaves without letting go is let go of once the shell is back in front, a moment
later: a safety net, not a way to leave. That's also why a report typed at the shell's own prompt doesn't
hold: report from your agent's process.

#### Keep it out of the way

- Don't let crystal hold your agent up: report with a short timeout, one report at a time, and ignore
  failures.
- `crystal ls --json` shows what crystal has: `reporter`, with the agent's name, its last `message` and the
  `resume` command, and `front`, the agent by its name.
- `crystal events -n <session>` shows each report that changed something, and `session.claimed` and
  `session.released` as your agent takes the session over and lets go.

### Agents driving agents

Every session knows how to reach its daemon, so an agent can run crystal commands too: start a second agent,
hand it work, wait for it, and read what it said. Here a Claude Code session gets a review of its change:

```sh
crystal new -d -n reviewer claude                         # a second Claude, in the background
crystal send reviewer "Review the diff on this branch" --wait   # prints done, or waiting if it asks something
crystal read reviewer --lines 40                          # the end of its answer, or its question
crystal send-keys reviewer 1 --wait                       # answer a question: the first choice
```

`send` types the way a person does: the text first, marked as a paste when the program asks for that, then
Enter on its own, so an agent takes it as a prompt and not as pasted text. `--wait` waits for the turn the
text starts, not one that ended before it. `wait` returns once the agent isn't working: `done`, `waiting` when
it asks something, `idle`, or how its program exited. It takes a `--timeout` in seconds, and fails when that
runs out. A program that doesn't say what it's doing counts as busy until it ends.

`wait` can wait for something else instead:

```sh
crystal wait reviewer --until waiting         # until it asks something; prints waiting
crystal wait reviewer --until done,idle       # any of them: working, waiting, done, idle, ended
crystal wait server --output 'listening on'   # until a line on its screen matches; prints the line
```

`--until` returns at once if the session is there already, and catches a state it's in only a moment, like
`working`. A turn that ends while someone watches it is `idle` at once, never `done`, so a script that doesn't
mind waits for `done,idle`. `ended` (or `exited`) waits for its program to end; ending first, when that isn't
what it waits for, is an error. `--output` takes a regular expression, matched a line at a time against the
screen and the 200 rows above it, so output already there counts. Both take `--timeout`. Waits listen to the
daemon's [events](#events) rather than asking it again and again, and `--output` looks each time the program
writes.

`send-keys` presses keys instead, the way tmux's does: key names like `Enter`, `Escape`, `Tab`, `Up`, `Down`,
`BSpace`, `C-c` or `M-x`, and any other word typed as keys. That's what answers an agent's question, since
agents don't act on a pasted answer. With `--wait`, it waits for the turn the answer lets carry on.

`crystal ls --json` prints the sessions as a JSON array. Each object holds what the daemon knows about the
session, plus `status`, the word the STATE column shows:

```json
[
  {
    "name": "reviewer",
    "id": "18daf82437703c98-0",
    "command": ["claude"],
    "cwd": "/code/app",
    "pid": 41210,
    "state": "running",
    "activity": "waiting",
    "worktree": {
      "project": "app",
      "project_path": "/code/app",
      "path": "/code/app",
      "main": true,
      "branch": "main"
    },
    "front": { "kind": "agent", "program": "claude", "name": "Claude Code" },
    "status": "waiting"
  }
]
```

`state` is `"running"`, `{"exited": {"code": 3}}` or `{"signaled": {"signal": "Terminated"}}`; `activity` is
`null` for a program that doesn't report what it's doing; `worktree` is `null` outside a git repository;
`front` is what's in front in the terminal: `{"kind": "agent", …}`, `{"kind": "shell", "name": "zsh"}`,
`{"kind": "program", "name": "vite"}` or `{"kind": "task"}`, and `null` until it's been looked at.
`task` is `null` for a session started with nothing to do, and otherwise holds its [task](#tasks): `id`,
`goal`, `waiting` while it waits on you, and once it's closed, `outcome` with `failed`, `cancelled` and
`summary`. `asking` holds the permission a [background task](#background-tasks) waits on you for, `tool` and
`gist`, and is `null` otherwise. `reporter` holds an agent that [says what it's doing
itself](#teaching-crystal-about-your-agent): its `agent` name, its last `message` and its `resume` command;
while it's there, `front` is that agent. New fields may appear; none goes away. With no daemon running, it
prints `[]`.

#### Laying out the TUI

The tabs and panes are the TUI's, and the command line lays them out too, so an agent can put the session it
started on screen beside its own, or a script can set up a tab for a review:

```sh
crystal new -d -n tests cargo test
crystal pane split tests                  # beside the pane of the session this runs in; --down below it
crystal pane split logs --beside server --down --ratio 0.7   # server keeps 70% of the room
crystal pane focus tests                  # select it, its tab in front, and type into it; or left, right, up, down
crystal pane resize left 8 -n tests       # move a border of its pane, as R does; 4 columns or 2 rows by default
crystal pane close tests                  # close its split, or put its float back
crystal pane zoom reviewer                # zoom its tab on it; --off puts the panes back
crystal pane float logs                   # float it over its tab's panes; --off puts it back
crystal pane equalize                     # even out the panes, as = does in resize mode
crystal tab new review                    # a tab after the others, in front: sessions started now go in it; prints 2
crystal tab select 1                      # a tab by its number or its name
crystal tab rename 2 checks               # an empty name takes it back to its number
crystal tab move reviewer review          # move a session to another tab, as > does
crystal tab close review --kill           # a tab with sessions closes only with --kill, which kills them
crystal layout                            # each tab's sessions and how its panes split the room; --json
```

A command about a session works on the tab that holds it, whether it's in front or not, and leaves the tab in
front where it is: an agent in another tab lays out its own without taking you there. Only going somewhere
moves you: `tab new`, `tab select` and `pane focus`. Without a session named, a command is about the session
it runs in, or, run outside crystal, the selected one. `pane split` splits the pane that session has of its
own, or else the selection's pane, which it selects if that pane shows something else; the session split off
moves into that tab, out of any pane it had. A command that can't be carried out says why, the way the footer
would: no room for another pane, a session that isn't on screen.

The command goes through the daemon to the TUI you used last, the one where you last pressed a key, clicked
or brought its terminal to the front, and waits for it to answer, a few seconds at most. With no TUI open,
it fails, saying so. When `restart-server` hands the daemon over, each TUI offers itself to the new one at
once, saying when you last used it, so commands carry on going to the same one. What it changes is kept like any change you make, so it's there when the TUI opens again.
`crystal layout --json` prints the tabs in their order, each with its `number`, `name`, whether it's
`current` and `zoomed`, its `sessions`, the one `selected`, the one `floating`, and its `panes`: either
`{"kind": "pane", "session": "tests"}`, with `"selection": true` for the pane that follows the selection, or
`{"kind": "split", "way": "right", "ratio": 0.5, "first": …, "second": …}`, `way` being `right` for side by
side and `down` for one above the other, and `ratio` the first side's share.

#### A skill for Claude Code

Claude Code learns all of this from a skill: when to hand work to another agent, the commands, what each
status means, and the traps, like answering a question with `send-keys` rather than `send`.

```sh
crystal skill --install   # into ~/.claude/skills/crystal, or $CLAUDE_CONFIG_DIR/skills/crystal
crystal skill             # or just print it
```

The install script installs it when it finds Claude Code (its `claude` command, or `~/.claude`), and so does
`make install`; `CRYSTAL_NO_SKILL=1` leaves it out of either.

`--install` won't write over a skill file you've changed; `--force` does. The skill lives in
[`skill/SKILL.md`](skill/SKILL.md), and each crystal carries its own copy. After an upgrade, the daemon brings
the skill up to date as it starts, when the copy installed is one an earlier crystal wrote and nobody has
changed since; it never installs the skill where it isn't, or writes over one you've changed.

### Events

The daemon writes down everything that happens in an event log, kept in crystal's database
(`~/.local/state/crystal/crystal.db`): sessions starting, working, waiting and ending, tasks opening and
closing, background runs, what they asked and what they cost, flows, worktrees, memory and the backlog.
`crystal events` prints it, one line each, the oldest first:

```sh
crystal events                        # all of it
crystal events --since 2h             # or 30m, 3d, 14:00, 2026-10-01T09:30
crystal events -n reviewer -k 'task.*'   # one session, through its renames; kinds or families, repeatable
crystal events -C ~/code/app --json   # one project's, as JSON lines
crystal events --follow               # new ones as they happen; with --since, catch up first
```

```
14:03:07     session.started     reviewer  claude 'Review the diff on this branch'
14:03:07     task.opened         reviewer  Review the diff on this branch
14:03:09     session.working     reviewer  idle → working
14:05:40     session.waiting     reviewer  working → waiting
14:05:52     session.working     reviewer  waiting → working
14:06:02     session.done        reviewer  working → done
14:06:15     task.closed         reviewer  done: two risks in the retry loop
```

The time is `14:03:07` today, `09-24 14:03` earlier in the year, and the date before that. In the TUI, `a`
shows the same lines as the [timeline](#timeline).

Each line of `--json` is one event, the same JSON the log keeps and plugins get: its `seq` (1, 2, 3…, never
going back), `at` (milliseconds since the Unix epoch), its name as `event`, the `project` it's about, the
`session` (its `name`, `id`, `command`, `cwd`, `project`, `worktree`, `branch`, `activity`, `task`, `status`,
as `ls` words it, and `reporter` while an agent that reports for itself holds it), and what its kind carries:
`from` (a renamed session's old name, what its agent was doing before, or the agent that let go), `task` (with
its `id`, `pending`, `waiting` and, once closed, its `outcome` and the `artifacts` kept with it), `run`
(`prompt`; `asking`, with its `tool` and `gist`, and the `decision`; then `failed`, `answer` and `cost_usd`),
`flow` (`run`, `flow`, `goal`, `step`, `state`, `said`, `cost_usd`), `worktree`, `handoff` (the file's `path`
and the `note`'s first line), `artifact` (a kept file's `kind`, `name`, `path` and `bytes`), `memory` (the
entry), `backlog` (the item) or `plugin` (`name` and `why`). New fields and events may appear; none goes away.
The events are listed under [plugins](#events-1).

A program can listen on the daemon's socket, as `--follow` does, with one line of JSON:

```json
{"type": "subscribe", "version": "0.3.0", "filter": {"kinds": ["session.waiting", "task.*"], "session": "reviewer"}, "since": {"seq": 41}}
```

`filter` takes `kinds` (names or patterns), a `session` by name or id and a `project` by the path of its main
worktree, all optional. `since` is `{"seq": N}` for the events after that one, or `{"at": ms}` for those from
that time; leave it out for new ones only. The daemon answers `{"type":"subscribed","seq":N}`, sends what the
log has from `since` up to that `seq`, then each new event as it happens, one line each, so a client that
reconnects with the last `seq` it saw misses nothing. One that falls more than 4096 events behind gets a last
`{"type":"error",…}` line and is let go. `version` is crystal's own: the daemon refuses another's.

The log keeps 30 days, or `keep_days` under `[events]` in the config (`0` keeps everything), and never more
than 50,000 events; the daemon prunes it as it starts and every hour after, and the numbers never go back,
however much is pruned. Events from outside the daemon, like `crystal remember`, reach the log through it,
so one done with no daemon running isn't written down.

### Timeline

`a` in the TUI opens the timeline: the event log read back, the newest first, a line an event, the way
`crystal events` prints it: when, what happened, in the color of how it went, the session or flow run it's
about, and what it says. It's live: while it's open, new events come in on top, and the bar stays on the line
it was on. Type to filter the lines by anything in them (`fixer`, `task.closed`, `failed`, a branch), and
`Tab` and `Shift+Tab` narrow them to one kind: sessions and worktrees, tasks with their runs, handoff notes
and the backlog, flows, memory, or the others. `↑` and `↓` move; the line the bar is on is read whole under
the list, with everything its event carries, and `PgUp` and `PgDn` scroll it. `Enter` goes to the session the
line is about, whatever it's called now (for a flow run, its latest step's); `Esc` clears the filter, then
closes. The timeline reads the log a page at a time, and further back as the bar reaches the end.

`U` lists everything that needs you now, in every tab, the most urgent first: background tasks asking for a
permission, flow runs at a gate, tasks whose agent ended its turn with the task still open, agents asking you
something, then agents that finished a turn you haven't seen. Within each, whatever has waited longest comes
first, and each thing has one row. `y`, `n` and `Y` answer a permission where it stands, and `g` and `f` a
gate (go on, or send it back with your notes), as on the session's own row; the list stays open, and an
answered row leaves it. `Enter` goes to the session, and `Esc` closes the list.

When you come back to crystal, the footer says what happened while you were away, in one line:

```
while you were away: 2 tasks done · 1 failed · 3 sessions finished · 1 needs you
```

Only what isn't nothing is said, a task or a session once however often, and of what came to need you only
what still does. You were away since you last quit the TUI (crystal keeps the latest event you had seen in its
database), while your terminal didn't have focus for five minutes or more, or, in a terminal that doesn't say
when it has focus, while you didn't type or click for that long. The line stays until your next key: `a` opens
the timeline with what's new since marked `•`, and `U` lists what needs you.

### Other machines

`crystal ssh` runs crystal on another machine through your own `ssh`, so your `~/.ssh/config`, keys and agent
work as they always do, and crystal never sees a password or a key:

```sh
crystal ssh box                                  # the TUI over there
crystal ssh me@box.example.com ls                # or any crystal command
crystal ssh box new -d -n fixer claude "fix the flaky test"
crystal ssh --install box                        # install or upgrade crystal there without asking
```

Everything after the machine goes to crystal over there as you typed it, quotes and all. That crystal has
its own daemon and sessions, which keep running when you disconnect. crystal looks for itself there on the
PATH, then in `~/.local/bin` and `~/.cargo/bin`. If it isn't there, crystal offers to install it with the
install script; if it's another version, crystal says so and offers to upgrade it. Away from a terminal it
never installs anything unless you pass `--install`. `CRYSTAL_SSH` names a command to use in place of `ssh`.

### Servers

A server is a daemon of its own, with its own sessions, tabs, layouts, backlog, tasks and memory: one for work
and one for a side project, say, or one to try something in without touching your own. `--server` (or `-L`, as
in tmux) names one for any command, the TUI included, and starts it the first time it's needed;
`CRYSTAL_SERVER` names one for every command run where it's set. Without either, crystal uses the default
server, the one it always has.

```sh
crystal --server side                       # the TUI on the server called side
crystal -L side new -d claude "try the new parser"
crystal server                              # every server: running or stopped, and its sessions
crystal server --json                       # the same, as JSON, with each one's socket and state
crystal server stop side                    # stop it and its sessions, as kill-server does
crystal server delete side                  # delete a stopped server: its sessions, tabs, backlog, memory…
```

```
$ crystal server
NAME     STATE    SESSIONS
default  running  4
side     stopped  0
work     running  2
```

A server's socket is beside the default one, named after it (`side.sock`), and its state is in
`~/.local/state/crystal/servers/side/`, so it outlives a reboot the way the default server's does. A stopped
server's sessions are those it starts again when it next starts: none after `stop`, those that were running
after a crash or a reboot. The config file, its profiles and flows, and plugins are shared by every server.

A session's program is told its server in `CRYSTAL_SERVER`, and crystal run in it reaches that server unless
`--server` names another, so an agent's `crystal send` or `crystal done` goes to its own server. The TUI on a
server other than the default names it in the top bar, before the count of its sessions. A name is letters,
digits, `-` and `_`. `crystal server delete` refuses while the server is running, and for the default server.
`crystal ssh box --server side` uses the server called side over there. `-S` (or `CRYSTAL_SOCKET`) still takes
a socket anywhere else, by its path; its state is kept beside it.

### Background tasks

A task is Claude Code without a terminal: `claude -p`, running a prompt in the background. It sits in the
session list like any session, with a transcript you can watch in the TUI, attach to, or `read`: the prompt,
what Claude says, laid out as [markdown](#the-file-finder-and-the-tree-browser), each tool it uses with the first
line of what came back, the permissions it asks for and how you answered, and how each run ended, with how long
it took and what it cost.

```sh
crystal task -n docs "Update the README for the new flags"           # prints the task's name
crystal task --wait -n tests "Run the tests and fix what fails" -- --permission-mode acceptEdits
crystal result tests                                                 # Claude's answer at the end of the run
crystal send docs "Now the changelog too" --wait                     # a follow-up, in the same conversation
crystal answer docs y                                                # allow what it asks for: y, n or always
crystal interrupt docs                                               # stop the run it's in the middle of
```

- One `claude` takes the task's prompt and each follow-up after it, a run each, over its standard input
  (`--input-format stream-json`). Arguments after `--` go to every `claude -p` the task starts.
- When Claude asks for a tool its permission mode and rules don't allow, the run waits on you: the session
  shows as `waiting`, its transcript and its pane's header say what it asks (`⚠ Bash cargo test`), and `ls
  --json` has it as `asking`. `y` in the TUI, on the task in the sidebar, in its pane or in the list `U` opens, or
  `crystal answer <task> y`, lets it run once. `n` says no: Claude is told so, or what `-m` says, and carries on. `Y`, or
  `always`, lets it run and keeps a rule for calls like it, so they aren't asked about again: for a shell
  command its first word, or its first two for `git`, `cargo`, `npm`, `go` and the like (`Bash(cargo
  test:*)`), and for any other tool the tool. Claude adds the rule to the checkout's
  `.claude/settings.local.json`, so later sessions there have it too. To be asked less to begin with, allow
  what it needs with `--allowedTools` or `--permission-mode` after `--`.
- `Ctrl+C` in a task's pane, or `crystal interrupt <task>`, stops the run it's in the middle of. Its task
  stays open, waiting on you, and a follow-up carries on.
- `crystal send` gives a task a follow-up: on the `claude` still there, or once that has gone, after five
  minutes with nothing to do or after a restart, on a new one that carries the conversation on with
  `--resume`. One run at a time: a follow-up sent while Claude is still working is refused. A task takes no
  keys, so `send-keys` is refused too.
- `crystal result <task>` prints the last answer; `--json` adds whether the run failed, the conversation's id,
  the cost so far and how many runs the task has had.
- Each task's `claude` is given `--max-budget-usd`: $5, unless `max_budget_usd` under `[tasks]` in the
  [settings](#settings) says otherwise, and `0` for none. A run that reaches it fails. What every task spends
  is added up by the day, and the TUI's footer shows it: `$4.12 today`. With `daily_budget_usd` set, past it
  the footer turns red (`$6.40 today · over $5.00`) and no new run starts until the next day, whether a new
  task, a follow-up or a flow's step: each is refused, saying why. Runs already going carry on.
- A run that fails, or crashes before saying anything, ends the task, which shows how it exited and why.
  `crystal respawn` runs its prompt again, in its conversation if it got that far.
- After a restart, a task comes back at rest rather than running its prompt again, and what it showed before
  is gone; `crystal send` carries its conversation on. `restart-server` hands it over as it is instead: its
  `claude` goes on with the run it's in, a permission it's asking for is still there to answer, and its
  screen keeps what it showed.

`<task>` is the task's session, or its [task](#tasks) number, like `t12`.

### Memory

A project keeps a short list of what its sessions have learned, so the next session doesn't learn it again: a
decision and why it was made, a gotcha, a command that works, a note, or how a task turned out. You or an
agent in a session add to it:

```sh
crystal remember "Fees are kept in cents; never store a float"
crystal remember -k gotcha -f tests/ledger.rs "The ledger tests need the database up: make db"
crystal remember -k command "make e2e runs the browser tests; they take about 4 minutes"
crystal memory                       # the list, newest first
crystal memory search ledger tests   # the entries that have most to do with those words
crystal memory show 3                # one in full: its files, where it came from, how often it was said
crystal memory export > MEMORY.md    # the whole list as markdown
crystal memory rm 3                  # forget one
crystal memory promote 2             # copy one into the project's CLAUDE.md, under "Notes"
crystal memory distill fixer         # have a model read what a session did, now
crystal memory embed                 # download the model that searches by meaning
```

- `-k` is `decision`, `gotcha`, `command`, `note` (the default) or `outcome`. Inside a session, an entry goes
  to the session's project and says which session added it; elsewhere it goes to the project of the current
  directory, or of `-C <dir>`.
- A project is its main worktree, so every worktree of it shares one list. Every project's list is kept in one
  SQLite database in crystal's state directory (`~/.local/state/crystal/memory/memory.db`), not in the
  repository. A project's list from before, a JSON file there, is brought in the first time it's read.
- `search` uses SQLite's full-text index (FTS5), ranked by bm25: any of the words matches, and so does a word
  they start or stem from (`deploying` finds "Deploys go out on Tuesdays"); the entries with more of the words,
  and rarer ones, come first, then drifting ones, and stale ones last. With
  [search by meaning](#search-by-meaning) on, entries that mean the same count too, whatever their words.
- The same thing remembered again (the same words, whatever the case or punctuation) is the one entry, seen
  again: `remembered 3 already`. Credentials in an entry, like `API_KEY=…` or a token, are taken out as it's
  kept.
- `-f` names a file an entry is about, and can be given more than once. crystal keeps a hash of each file as it
  is then (a file that isn't there isn't counted). Once some of them change, the entry is marked drifting: it
  may hold only in part, and it comes after the rest. Once all of them have changed, or gone, it's stale, and
  agents aren't shown it. `crystal memory rm` it, or remember it again, which takes its files as they are now.
  The files are looked at in the worktree the entry was remembered in while that's there, and in the main
  worktree after.
- `promote` asks first at a terminal; `--yes` doesn't. It writes to CLAUDE.md, or to AGENTS.md when that's the
  only one the project has.
- `m` in the sidebar opens the selected session's project's list, drifting and stale entries marked: the entry
  the bar is on is shown in full beside it, `/` filters, `x` forgets an entry and `p` promotes it, each after a
  `y`.

When an agent starts, crystal shows it the entries that have most to do with its launch: first those about files
its worktree has changed since its branch left the default one (`origin`'s, or `main` or `master`), committed
or not, then those that have most to do with its first prompt, or the newest when neither finds any. That's a
few at most, in 800 bytes, the least relevant left out first; none that's stale, and drifting ones marked and
after the rest. Each comes with its id, and a line on how to read the rest and add more:

- Claude Code gets them in its system prompt, and reads the rest with crystal's MCP tools (below).
- Codex gets them as its `developer_instructions` (`-c`), after the ones it has already, from a
  [profile](#profiles) or its own `config.toml`, and reads the rest with `crystal memory search` and `show`.
- Gemini CLI, OpenCode and Cursor get them at the top of their first prompt, when they're given one. A prompt
  that would pass 16 KiB with them loses what the memory has first. Aider, which takes no first prompt, isn't
  told.

`crystal plugin disable memory` turns it all off: see [plugins](#plugins).

Every Claude Code session crystal starts, in a terminal or as a task in the background (`claude -p`), gets
crystal's own MCP server, `crystal mcp`, with its two tools allowed: `memory_search`, which searches the
project's memory the way `crystal memory search` does, and `memory_show`, which reads one entry in full. These
are how it reads the rest of what was learned without a shell command, which a task has nobody to say yes to
and a session in a terminal would stop to ask about.

#### Search by meaning

Words only find words: "db" never finds "Postgres has to be running". With `embeddings = true` under
`[memory]`, crystal also searches by what entries mean, with a small model run on your machine,
[BAAI/bge-small-en-v1.5](https://huggingface.co/BAAI/bge-small-en-v1.5) through
[Candle](https://github.com/huggingface/candle): no API, no key, and nothing leaves the machine.

```sh
crystal memory embed   # once: downloads the model (134 MB), then gives every entry its vector
```

- The model isn't part of crystal. `crystal memory embed` downloads it with `curl`, at a pinned revision,
  checks each file against its SHA-256, and keeps it in `~/.cache/crystal/models/` (or `$XDG_CACHE_HOME`).
  Until it's there, searches go by words alone, and `crystal memory search` says so.
- Each entry's vector is kept beside it in `memory.db`. An entry without one, say one just remembered, gets it
  the first time a search needs it.
- A search ranks by words (bm25) and by meaning (the model), and merges the two by reciprocal rank fusion, so
  an entry high in both comes first. By meaning, only the entries close to the best match count: the model's
  scores sit close together, and one far behind the best is a match in name only.
- The daemon keeps the model loaded (about 150 MB), once for every client: `crystal memory search` and every
  task's `memory_search` ask it, and only search in their own process when no daemon is running. What a
  session is shown as it starts, and what the distiller is shown the memory has already, go by meaning too.
- It finds paraphrases and near-synonyms that words miss, but it's a small model: a one-word query can rank
  oddly, and a query about something the memory doesn't hold still brings back what's nearest.

#### The distiller

Agents don't always remember what they learned. So once a task closes, done or failed, a model reads what it
did and keeps what a later session would need to know and couldn't find in the code: decisions and why, dead
ends, commands that work, traps. It's one `claude -p` run on Haiku, in the background:

- It reads the end of what the task did: a task's transcript, as Claude Code keeps it (or, when it doesn't,
  what crystal read of its runs), or for Claude Code in a terminal, the transcript its hooks named. Codex leaves
  nothing it can read. Credentials are taken out before the model sees any of it.
- It's shown what the project's memory has already on the same subject, and told never to give that again.
- It has no tools, no MCP servers, none of the project's settings and none of your hooks, a budget (25 cents
  by default) and two turns. Its answer is checked before anything is kept: at most 8 entries, of the kinds
  `decision`, `gotcha`, `command` and `note`, each 400 characters at most, naming only files that are in the
  checkout.
- What passes is kept like anything else, `from the distiller, after task <name>`: what's there already is
  seen again rather than added twice, and what you forgot with `rm` it never adds back (you can, by
  remembering it yourself).
- How it went is a line in the daemon's log, `default.log` beside the socket. `crystal memory distill <session>`
  runs it now and says what came of it; it works on any Claude Code session or task, closed or not.

`[memory]` in the [settings](#settings) changes how it runs:

```toml
[memory]
distill = true                      # false to turn it off
distill_model = "claude-haiku-4-5"  # the model, as `claude --model` takes it
distill_budget_usd = 0.25           # the most one task's pass may spend
embeddings = false                  # true to search by meaning too: see above
```

### Tasks

A session started with something to do is a task: an agent given a task in the new-session panel, `crystal
new -t "<task>" claude` (or simply `crystal new claude "<task>"`), a [background task](#background-tasks), one
started from the [backlog](#the-backlog), or one made with `crystal tasks new`. Each task is numbered as it's
made, `t1`, `t2`…, and stays open until it's closed, done or failed, with a line on how it went:

- The agent closes it from inside its session: `crystal done "<what was done>"`, or `crystal done --failed
  "<why>"`. crystal tells Claude Code how, on top of its system prompt, Codex in its developer instructions,
  and other agents at the top of their first prompt, opening with a line on where that comes from, so the
  agent doesn't take it for a stranger's instructions. Agents don't always remember to, Haiku least of all, so
  the first time Claude Code ends a turn with its task still open, its Stop hook reminds it and it carries on:
  to close the task, or, if it isn't through, to leave it open and end its turn. `-n <session>` closes another
  session's task.
- You close it from the TUI: `c` on the session asks `d` done or `f` failed, then for a line on how it went,
  which can stay empty.
- A background task closes itself when its run ends: done with the first line of Claude's answer, or failed.
  A follow-up opens it again.

| State | Meaning |
|---|---|
| `pending` | made with `--no-launch`: nothing works on it yet |
| `running` | its session is working on it |
| `waiting` | its agent's turn ended with the task still open: it's asking you something |
| `done` | closed done |
| `failed` | closed failed, or its session ended while it was open, leaving nobody who could close it |
| `cancelled` | you cancelled it, or killed its session while it was open |

A turn that ends with the task still open, once the agent has been reminded, is a question for you: the session
waits on you (`▲`, and `u` finds it) until its agent works again, and its task line says so. So does a
background task you interrupted. A program that exits with its task open fails it, saying how it ended; `crystal
respawn` opens it again, under the same number.

The sidebar shows a task under its session: what it was asked to do while it's open, `▲` when it waits on you,
`⚠` and the permission a background task asks for, and `✓`, `✗` or `–` with how it went once it's closed. So
does the pane's header. `crystal ls` has a TASK column, and `ls --json` a `task` field.

```sh
crystal tasks                                       # the project's tasks: open, waiting to start, then closed
crystal tasks new "Fix the flaky test"              # the new-session panel's first agent on it; prints t12
crystal tasks new --background "Bump the deps" -- --model opus   # or Claude in the background
crystal tasks new --no-launch "Tidy the README"     # made now, started later: prints t13
crystal tasks start t13                             # start it; prints its session's name
crystal tasks show t12                              # how it stands, its session, what it asks for and costs
crystal tasks cancel t12                            # cancel it, and stop its session
crystal tasks log t12                               # how it stands, then its session's transcript
```

`tasks new` works from a shell or from inside a session, in the current directory (`-c <dir>`, or `-w
<branch>` for a new worktree, made at once), and `-n` names its session. A task is named by its number, with
or without the `t`, or by its session's name. `crystal task <prompt>` still starts a background task;
`crystal tasks` is about the tasks there are.

Closed tasks are kept in the project's history in crystal's database: what each was asked, when and how it
closed, and the session and branch it ran in. `crystal tasks` lists the project's tasks, open ones first, then
those waiting to start, then those closed, the latest first; `--all` lists every project's, `-C <dir>` another
project's, and `--json` prints them for scripts. `tasks log` shows a task's transcript while its session is
still in the list.

#### The handoff file

A task's summary is one line on what it did. What a worktree learned along the way, the next session there
would otherwise learn again, so each git worktree keeps notes for the sessions after: `.crystal/handoff.md`,
at its top. An agent adds one when it learns something the next session there should know:

```sh
crystal handoff "The fixtures live in tests/fixtures; cargo test codec runs just them"
```

```
## 2026-10-04T14:03:07+02:00 · porter · task "Port the codec"
The fixtures live in tests/fixtures; cargo test codec runs just them

## 2026-10-04T14:41:52+02:00 · porter · task "Port the codec" done
Ported the codec and its tests
```

- Each note is a section: a heading with the local time, the session's name and the task it has open, then the
  note, its spaces and blank lines tidied and cut at 8 KiB. `-n <session>` adds one for another session's
  worktree.
- A task that closes done or failed adds its summary, or why it failed, under a heading that ends in how it
  closed. A cancelled one adds nothing.
- Every agent crystal starts in a worktree whose file has notes is told to read it first and how to add to it,
  with the file's last 2 KiB, by the same road as its task: Claude Code on top of its system prompt, a
  background task too, Codex in its developer instructions, and Gemini CLI, OpenCode and Cursor at the top of
  their first prompt. When the file's end would make what it's asked and told more than 16 KiB, it's told
  where the file is without it; a first prompt too long even so loses what the memory has first, then the
  notes.
- crystal is the file's only writer. It keeps it to 256 KiB, letting the oldest notes go, with `[earlier
  notes trimmed]` on top.
- The notes stay out of git: the first note writes a `.gitignore` beside them that ignores everything in
  `.crystal/` but `flows.toml` (a `.gitignore` there already is left as it is). A project whose notes should
  travel with its branches lists its main worktree under `[handoff]` in the [settings](#settings), and the
  `.gitignore` isn't written:

  ```toml
  [handoff]
  in_git = ["~/code/app"]
  ```

- Notes are a git worktree's: a session outside git has none. `crystal plugin disable handoff` turns them off.

#### Kept files

`crystal done "<summary>" --artifact <path>` keeps a copy of a file with the task as it closes: a plan, a
report, a screenshot, whatever someone will want once the worktree is gone. `--artifact` can be given more
than once.

- Each must be a file in the session's worktree, not a link or a directory, of 1 MiB at most, and 8 MiB for all
  of them. A path is taken from the directory the command runs in. One that can't be kept refuses the whole
  close, saying why, and the task stays open, so the agent can fix the call and run it again.
- The daemon copies them itself, after checking each again, into its server's state directory, beside its
  database: `~/.local/state/crystal/tasks/t12/` for the default server. Two files with one name are kept as
  `plan.md` and `plan-2.md`.
- A task that closes done or failed keeps its worktree's handoff file too, as it is then, as `handoff.md`.
- `crystal tasks show t12` lists them, and `crystal tasks --json` gives each task's `artifacts`, with their
  `kind` (`file` or `handoff`), `name`, `path` and `bytes`. Each is a `task.artifact` in the [event
  log](#events).

### The backlog

Each project keeps a backlog: things worth doing later that aren't anyone's task yet. It's the project's, not
a worktree's, so every worktree of a repository shares it, and it's kept in crystal's database, out of the
repository. Items are numbered per project, `#1` on, and keep their number.

```sh
crystal backlog add "Retry the webhook on a timeout" -t payments   # prints #4
crystal backlog                     # what's still to do; --all for what's done too, --json for scripts
crystal backlog done 4              # or reopen 4, or rm 4
crystal backlog start 4 -w          # an agent on #4, in a new worktree named after it
crystal backlog export > TODO.md    # markdown checkboxes, done items ticked
```

`backlog start` starts the agent the new-session panel picks first (`new_session` in the
[settings](#settings)) with the item as its task. When that task closes done, the item is ticked off. Agents
are told to put what they notice along the way on the backlog with `crystal backlog add`, rather than into the
change at hand. Every command works on the current directory's project; `-C <dir>` names another.

In the TUI, `b` opens the selected session's project's backlog, and the sidebar counts what each project has
to do beside its name: `payments ──── 3 to do`. In the view, what's to do comes first and what's done after.
`a` adds an item, `Space` ticks one off or opens it again, `x` removes one once you've said `y`, and `/`
filters the list as you type. `Enter` opens the new-session panel with the item as its task, on a branch named
after it; that task ticks the item off when it closes done.

### Flows

A flow is a chain of [tasks](#tasks) on one goal: plan it, build it in a worktree, review it, open the pull
request. Each step runs with a [profile](#profiles) of its own and starts once the step before it is done,
given what that step answered. A step can stop the flow at a gate until you've looked at what it did, then
you go on, or send it back with notes.

Flows are written in the config file, a `[[flow]]` table each, with a `[[flow.step]]` table for each step.
`crystal flow example` prints this one, with the profiles it runs with, ready to copy in:

```toml
[[flow]]
name = "ship"
description = "Plan, build in a worktree, review, open a pull request"

[[flow.step]]
name = "plan"
profile = "planner"
prompt = "Plan how to do this: {goal}. Answer with the files to change and how, in order."

[[flow.step]]
name = "implement"
profile = "builder"
placement = "fresh"
prompt = """
Do this: {goal}

Follow this plan:
{plan.summary}

{feedback}"""

[[flow.step]]
name = "review"
profile = "reviewer"
placement = "same"
gate = true
back_to = "implement"
max_rounds = 3
prompt = "Review the changes on this branch against what was asked: {goal}"

[[flow.step]]
name = "pr"
profile = "shipper"
prompt = "Push this branch and open a pull request for it with `gh pr create --fill`."
```

| Step setting | What it does |
|---|---|
| `name` | what the step is called; its session is named after the run and it, like `ship-1-plan` |
| `profile` | optional: the [profile](#profiles) it runs with: agent, model, mode, arguments, instructions, prompt; left out, Claude Code as it's set up |
| `prompt` | what it's asked, with the names below filled in |
| `placement` | optional: where it runs, below; left out, where the step before it ran, and the first where the run started |
| `worktree` | optional: `true` is `placement = "fresh"`, as it was first written |
| `gate` | optional: `true` stops the flow after it until you go on, or send the flow back |
| `back_to` | optional, on a step with a gate: the step that sending the flow back runs again; left out, this one |
| `max_rounds` | optional, on a step with a gate: how many rounds the flow may take through it, 1 to 3, left out 3; in the last, it can't be sent back |

| Placement | The step runs |
|---|---|
| `root` | where the run was started |
| `fresh` | in a worktree the run makes for itself, the first time a step asks for it, on a branch named after the goal (`add-retries`, or `add-retries-2` when that's taken); every `fresh` step of the run after that runs there too |
| `same` | where the step before it ran |

A step in a worktree is told about the [handoff file](#the-handoff-file) there like any agent, so it hears what
the steps before it there noted, and how each of their tasks ended.

What a prompt can name, in braces:

| Name | What it's filled in with |
|---|---|
| `{goal}` | what you asked the flow to do |
| `{slug}` | the goal's first line as a branch would have it: `add-retries` |
| `{round}` | 1, and one more each time the flow is sent back |
| `{previous}` | what the step before this one answered |
| `{<step>.summary}` | what the step called `<step>`, before this one, answered last |
| `{<step>.artifacts}` | the paths of the files that step's task [kept](#kept-files), one after another |
| `{feedback}` | empty until you send the flow back; from then on your notes, and when it went back to an earlier step, what the step at the gate said |

The step the flow goes back to hears the feedback even if its prompt doesn't ask for it. A brace that names
none of these stays as it is, so a prompt can show code; a `{<step>.summary}` or `{<step>.artifacts}` naming no
step before it is an error. A prompt is kept to 16 KiB: past that, what steps answered is cut short, the oldest
first, each ending `[cut short]`, and a step whose own text is too long fails.

```sh
crystal flow run ship "Retry the webhook when it times out"    # prints the run's name: ship-1
crystal flow                         # every run: how it stands, its step, round and cost
crystal flow show ship-1             # each step: how it stands, its session, runs, cost and answer
crystal flow wait ship-1             # until it waits at a gate or is done; a failed step is an error
crystal flow approve ship-1          # go on past the gate
crystal flow back ship-1 "Keep the old timeout as the default"    # send it back, with notes
crystal flow retry ship-1            # run a step that failed, or that a restart cut short, again
crystal flow cancel ship-1           # cancel its step's task, and go no further
crystal flow defs                    # the flows a run started here finds, and where each is written
```

- A step whose profile is Claude Code's runs as a [background task](#background-tasks), so nobody is there to
  say yes to a permission: give it a profile that allows what it needs, with `mode` and `args`. A step on any
  other agent, like Codex, runs in a terminal, a session with the step as its [task](#tasks), and the flow goes
  on once that task closes: done with its summary as the step's answer, or failed. An agent that can't be given
  a prompt to start on, like Aider, can't be a step. Each step's task goes into the project's
  [history](#tasks) as `ship-1 plan: <goal>`.
- Sending the flow back runs the step it goes back to again, in a new round: a background step as a follow-up
  in its own conversation, a step in a terminal in a new session, with the old one left for you to read. Then
  the steps after it run again. In a gate's last round, `max_rounds`, it can't be sent back: approve it, or
  cancel the run. A step that fails stops the run until you run it again, and you're told, as you are when a run stops at
  a gate.
- `crystal flow cancel` cancels the task of the step the run is at, while it's open, stops its session, and
  the run goes no further: it's `cancelled`.
- A project can keep flows of its own in `.crystal/flows.toml` in its main worktree, `[[flow]]` tables like the
  config file's, run with the profiles in your config. They're found by any run started in the project, and
  one there takes the place of the config file's of the same name. `crystal flow defs` lists the flows a run
  started in the current directory (or `-C <dir>`) finds, each with its steps, or why it can't run, and the
  file it's written in.
- In the sidebar, a run sits under its project after its worktrees: `◇`, the flow's name and the goal, and the
  round once it's been sent back. Under it is a row for each step: `·` still to come, the working mark while it
  runs, `▲` at its gate, `✓` done, `✗` failed, `■` cut short and `–` cancelled. A step's row is its task's
  session, so selecting it shows the step's transcript.
- At a gate, the step's session waits on you the way an agent asking something does: you're told, `u` goes to
  it, and `ls` says `waiting`. `g` goes on, and `f` asks for your notes on the footer and sends the flow back,
  on the step's row or in the list `U` opens. On a step that failed or was cut short, `g` runs it again.
- The new-session panel offers the flows in your config file after your profiles, `flow: ship`; what you type
  as the task is the goal.
- Runs are kept with the sessions, in crystal's database. After a restart, a run waiting at a gate waits
  again, a step in a terminal whose session comes back carries on there, and a background step that was
  running is marked interrupted until you run it again, in its conversation; `restart-server` hands runs over
  as they are, every step running carrying on. A run's steps start from the environment of the `crystal flow
  run` that started it; after a restart, from the daemon's. The daemon reads the flow and its profiles as the
  run starts, so changing them never changes a run halfway.

### Plugins

Most of what crystal does beyond running sessions is a plugin you can switch off: tasks, handoff notes, the
backlog, memory, profiles, GitHub and GitLab, flows and notifications. Plugins of your own add actions, panes
over the TUI, hooks on what happens, commands to run as the daemon starts and links to open their own way, and
use crystal through its own command line, like any script would.

```sh
crystal plugin                     # every plugin, and whether it's on
crystal plugin disable github      # or enable; written under [plugins] in the config file
crystal plugin new notes           # a plugin to start from, in ~/.config/crystal/plugins/notes
crystal plugin install <git-url>   # or a directory; shows what it runs and asks first, then builds it
crystal plugin build notes         # run its build commands again
crystal plugin run notes hello     # run one of its actions
crystal plugin run notes --event session.waiting   # try its hooks on a made-up event
crystal plugin run notes --link https://…          # run what its link handlers do with a link
crystal plugin log notes           # what its commands printed, and how they failed
crystal plugin remove notes
```

| Plugin | What it adds |
|---|---|
| `tasks` | [tasks](#tasks): `c`, a task under its session, `crystal done` and `tasks`, and telling agents how to close theirs |
| `handoff` | [the handoff file](#the-handoff-file): `crystal handoff`, the note a closing task adds and the copy it keeps, and telling agents to read the notes |
| `backlog` | [the backlog](#the-backlog): `b`, the counts beside projects, `crystal backlog`, and telling agents to use it |
| `memory` | [memory](#memory): `m`, `crystal remember` and `memory`, and what Claude Code is shown as it starts |
| `profiles` | [profiles](#profiles): `P`, the profiles in the new-session panel, and `crystal profile` |
| `github` | [pull requests and issues](#pull-requests-and-issues), on GitHub or GitLab: their marks on worktree lines, `o`, `O` and `i`; switched off, crystal never runs `gh` or `glab` |
| `flows` | [flows](#flows): `g` and `f`, runs in the sidebar and the new-session panel, and `crystal flow` |
| `notifications` | telling you when a session needs you |

crystal's own plugins are on until you switch one off. Then everything it adds is gone: its keys (`?` stops
listing them), what it shows in the sidebar and the new-session panel, what it tells agents, and the work it
does in the background. Its commands still run, to say that it's off and how to turn it on. `X` in the TUI lists
every plugin, and `Space` switches the one the bar is on, straight away. Either way it's written to the config
file, keeping your comments:

```toml
[plugins]
github = false
notes = true
```

The `notify` setting came before the `notifications` plugin and still works: notifications are on only while
both are. `memory` used to be a setting of its own; crystal says where it went if it finds one.

#### Writing a plugin

A plugin is a directory in `~/.config/crystal/plugins/` (or `$XDG_CONFIG_HOME/crystal/plugins/`) named after
it, with a `plugin.toml`. `crystal plugin new <name>` makes one with one of everything to start from. A plugin
you add is off until you turn it on.

```toml
name = "notes"                # its directory's name: lowercase letters, digits and dashes
version = "0.1.0"
description = "Notes on sessions"
min_crystal_version = "0.3.0" # optional: the oldest crystal it works with
platforms = ["macos", "linux"] # optional: where it runs; anywhere, left out

[[build]]                     # run as it's installed, and by `crystal plugin build notes`
command = ["npm", "ci"]
platforms = ["linux"]         # optional, here and on a startup command: only there

[[startup]]                   # run by the daemon as it starts
command = ["sh", "restore.sh"]

[[actions]]                   # run from X, its key, or `crystal plugin run notes add`
id = "add"
title = "Add a note"
command = ["sh", "add.sh"]
key = "N"                     # optional: a key crystal and other plugins don't use

[[events]]                    # run by the daemon when something happens
on = "session.waiting"        # or a family of events, like "session.*", or "*" for all
command = ["./on-wait.sh"]

[[panes]]                     # a program shown over the panes
id = "board"
title = "The notes board"
command = ["sh", "board.sh"]

[[link_handlers]]             # links a Ctrl+click opens with one of its actions
pattern = "^https://github\\.com/[^/]+/[^/]+/issues/[0-9]+$"
action = "add"
```

A command is a list of words, run without a shell from the plugin's directory; a program given as a path is
found from there too. Every command but a build command finds crystal in its environment:

- `CRYSTAL_BIN`: the crystal running it, for crystal's own commands, like `"$CRYSTAL_BIN" send
  "$CRYSTAL_SESSION" "…"`
- `CRYSTAL_SOCKET`: that crystal's daemon
- `CRYSTAL_PLUGIN`, `CRYSTAL_PLUGIN_DIR`: the plugin's name and its directory
- `CRYSTAL_PLUGIN_CONFIG_DIR`: a directory for its settings, like a token, which you fill in:
  `~/.config/crystal/plugin-config/notes/`, made as the plugin is installed
- `CRYSTAL_PLUGIN_STATE_DIR`: a directory for what it keeps as it runs, made before each command:
  `~/.local/state/crystal/plugins/notes/`, the server's own (see [servers](#servers))
- `CRYSTAL_SESSION`, `CRYSTAL_SESSION_ID`: the session it's about, when there is one
- `CRYSTAL_PROJECT`, `CRYSTAL_WORKTREE`: the project's main worktree, and the worktree, it's about
- `CRYSTAL_LINK`: for an action a [link](#link-handlers) runs, the link

Its settings are every server's, like the config file, and stay when the plugin is removed, for when it's
installed again. What it keeps is each server's, like the sessions it's about, and goes with the server when
`crystal server delete` deletes it.

An action is about the session selected in the TUI; for `crystal plugin run`, the session `--session` names,
or else the one it's run in, or else the current directory. Run from the TUI, what it prints goes to the
plugin's log; `plugin run` prints it, and exits as the action did.

A pane is a session of its own, started in the plugin's directory and shown over the panes with the keyboard.
It's in `crystal ls` while it's open, and ends when its program does or when you press `Ctrl+\`. Its
`CRYSTAL_SESSION` is its own; `CRYSTAL_PROJECT` and `CRYSTAL_WORKTREE` are the selected session's.

A plugin that names a `min_crystal_version` newer than yours, or `platforms` without yours, won't install;
one already there is listed `unsupported`, saying why, and can't be turned on.

#### Building

`crystal plugin install` runs the plugin's build commands once you've said yes, in turn, from its directory,
as you'd run them, those with `platforms` only on the systems they name. What they print goes to the plugin's
log. A build that fails leaves the plugin installed but off, with the last of what the command printed, and
`crystal plugin` lists it `unbuilt` until `crystal plugin build <name>` works; that turns a plugin that's on off
too, when it fails. crystal runs the commands, not the tools they need: say in your plugin's README which it
needs, like `npm` or `cargo`.

#### Startup

Each startup command of each plugin that's on runs once as the daemon starts, after it has brought back the
sessions that were running, and again whenever a daemon starts in place of another, as `crystal
restart-server` does; not when the TUI opens, or a plugin is turned on. It's for restoring what the plugin
keeps and handing it to crystal, then ending: `CRYSTAL_EVENT` is `startup`, and it runs as a hook does, one at
a time with the plugin's hooks, logged, stopped after 30 seconds. One that fails doesn't stop the daemon.

#### Link handlers

A `Ctrl`+click on a link in a pane, or copy mode's `o`, goes to the first plugin that's on, by name, with a
link handler whose `pattern` matches the link, the plugin's handlers tried in their order. The handler's
`action` runs in place of your browser, about the session in that pane, with the link in `CRYSTAL_LINK`: open
an issue in a pane of the plugin's own, say, or have an agent look at it. The pattern is a regular expression,
matched anywhere in the link unless `^` and `$` pin it. `X` lists each plugin's handlers under it, and
`crystal plugin run <name> --link <url>` runs the action its handlers give a link, to try them.

#### Events

| Event | When |
|---|---|
| `session.started` | a session starts, or starts again |
| `session.renamed` | a session gets another name |
| `session.working` | a session's agent starts working on a turn |
| `session.waiting` | a session's agent comes to wait on you |
| `session.done` | a session's agent finishes a turn nobody was watching |
| `session.idle` | a session's agent is at its prompt, its turn seen |
| `session.ended` | a session's program ends, or the session is killed |
| `session.removed` | a session leaves the list: killed, or its worktree removed |
| `session.claimed` | an agent takes over saying what a session is doing, with [`crystal report`](#teaching-crystal-about-your-agent) |
| `session.released` | it lets go: `crystal report --release`, or it left and the shell is back in front |
| `task.opened` | a task is made: given to a session as it starts, made to start later, or opened again by a follow-up |
| `task.started` | a task made to start later starts, in a session of its own |
| `task.waiting` | a task's agent ends a turn with the task still open: it waits on you |
| `task.closed` | a task closes, done, failed or cancelled |
| `task.artifact` | a file is kept with a task as it closes: one `crystal done --artifact` named, or its worktree's handoff file |
| `run.started` | a background task starts a run of Claude: its prompt, or a follow-up |
| `run.asking` | a background task's Claude asks you for a permission |
| `run.answered` | you answer it: allowed, allowed always, or denied |
| `run.interrupted` | you stop a background task's run halfway |
| `run.ended` | that run ends, with what the task has cost |
| `flow.started` | a flow run starts |
| `flow.step_started` | a flow run starts a step |
| `flow.step_ended` | a step's run ends: done, at its gate, or failed |
| `flow.gate` | a flow run waits at a gate for you |
| `flow.gate_answered` | you approve a gate, or send the run back |
| `flow.ended` | a flow run ends: every step done, one failed, or you cancelled it |
| `worktree.created` | crystal makes a worktree |
| `worktree.removed` | crystal removes one |
| `handoff.added` | a note goes in a worktree's handoff file: `crystal handoff`, or a task closing there |
| `memory.added` | an entry is added to a project's memory: remembered, a task's outcome, or by the distiller |
| `memory.forgotten` | an entry is forgotten |
| `backlog.added` | an item goes on a project's backlog |
| `backlog.closed` | an item is marked done |
| `plugin.paused` | a plugin is paused for failing |
| `daemon.handed_over` | the daemon is handed over to another crystal, its sessions carrying on (see `restart-server`) |

A hook gets the event as a line of JSON on its standard input, the same as the [event log](#events) keeps it,
and its name in `CRYSTAL_EVENT`:

```json
{"seq":412,"at":1790949076244,"event":"session.waiting","project":"/code/app","session":{"name":"claude-2",
 "id":"k3x9…","command":["claude"],"cwd":"/code/app","project":"/code/app","worktree":"/code/app",
 "branch":"main","activity":"waiting","task":"Fix the login redirect","status":"waiting"},"from":"working"}
```

`task.closed` has a `task`, with its `goal`, `session`, `project`, `branch` and `outcome` (whether it `failed`,
its `summary`, and when it `closed`). The worktree events have a `worktree`, with its `path`, `branch` and,
once it's made, `project`. The rest are under [events](#events).

`crystal plugin run <name> --event <event>` runs the plugin's hooks on that event, here and now, whether the
plugin is on or not, on a made-up event with everything its kind carries, about the session `--session` names,
or the one it's run in, or a made-up one. What they print is printed, and it exits as the first that failed.

A plugin's hooks run one at a time, in the order things happened, and what they print goes to its log, kept in
crystal's state directory. A hook still running after 30 seconds is stopped. After 5 failures in a row, startup
commands included, the plugin is paused, with a notification, and `crystal plugin` shows it `paused` until
`crystal plugin enable <name>` turns it back on.

#### Security

A plugin is code that runs as you, with everything you can reach: your files, your keys, your logins. Its
build runs as it's installed, and its hooks and startup commands in the background, whenever something happens
or the daemon starts. Add only plugins you'd run as a script of your own. `crystal plugin install` shows every
command a plugin would run, its build and startup commands and the actions its link handlers run included, and
asks before it installs it, and installs it switched off.

### Settings

Settings live in `~/.config/crystal/config.toml` (or `$XDG_CONFIG_HOME/crystal/config.toml`). The file is
optional, and so is every setting in it. `crystal config` prints the settings in effect, ready to save as the
file and change. A setting crystal doesn't know is an error that names it, so a typo never goes unnoticed.

| Setting | Default | What it does |
|---|---|---|
| `notify` | `true` | tell you when a session needs you |
| `notify_command` | none | a shell command to run instead of the desktop notification |
| `new_session` | `"claude"` | what the new-session panel runs at first, until you start something from it |
| `theme` | `"dark"` | the TUI's colors: `"dark"`, `"light"`, or `"terminal"` |
| `name_from_prompt` | `true` | name a session you don't name for the [first thing it's asked](#starting-a-session) |
| `resume_reported_agents` | `true` | after a restart, run the command an agent [said resumes it](#teaching-crystal-about-your-agent) |
| `[plugins]` | | which plugins are on and off: [plugins](#plugins) |
| `[memory]` | | how memory's [distiller](#the-distiller) runs, and whether it [searches by meaning](#search-by-meaning) |
| `[tasks]` | | what [background tasks](#background-tasks) may spend: `max_budget_usd` each (`5`), `daily_budget_usd` all together (none) |
| `[events]` | | `keep_days`, how long the [event log](#events) keeps what happened: 30 days, or `0` for ever |
| `[handoff]` | | `in_git`, the projects, by their main worktree, whose [handoff notes](#the-handoff-file) go in git |
| `[worktrees]` | | `base`, the branch new worktrees' new branches [start from](#usage): `origin`'s default branch unless set |

`dark` and `light` paint their own background, so crystal looks the same in any terminal; `terminal` paints
nothing and uses your terminal's own colors. With `NO_COLOR` set, crystal uses no color at all.

`notify_command` is for telling you some other way, like a message to your phone. It runs with
`CRYSTAL_NOTICE` (the line a notification would show), `CRYSTAL_NOTICE_SESSION` (the session's name) and
`CRYSTAL_NOTICE_ACTIVITY` (`waiting` or `done`) in its environment:

```toml
notify_command = 'curl -s -d "$CRYSTAL_NOTICE" ntfy.sh/my-crystal'
```

`new_session` names an agent (`claude`, `codex`, …) or `shell`. With options, like `codex --full-auto`, it's
offered as a profile of its own.

The daemon reads the notification settings each time it tells you something, `[plugins]` each time it
does something a plugin adds, `[memory]` each time a task closes or a search runs, `[tasks]` each time a
background task's run starts, `[handoff]` each time a note is written, a flow each time one starts,
`name_from_prompt` each time it names a session and `resume_reported_agents` as it starts sessions again, so a
change counts straight away; the TUI reads `new_session`, `theme`, `[plugins]`, the profiles and
the flows when it starts, again when you save a profile or switch a plugin, and every half a second while the
settings view is open.

#### The settings view

`,` in the sidebar opens the settings you'd otherwise change in the file: notifications, the theme, and how
memory learns ([the distiller](#the-distiller)) and searches ([by meaning](#search-by-meaning)). `space`
changes the one the bar is on, and `←/→` go through the themes. Each change is written to the file at once,
keeping the rest of it as you wrote it, comments and all, and counts straight away: the TUI repaints in a new
theme, and the daemon reads the rest as it goes.

While it's open, the view reads the file and asks the daemon again every half a second, so it follows a
change made by hand in the file too, and shows how the model that searches by meaning stands: downloading
(`42 of 134 MB`), loaded in the daemon or not, and how many entries have their vector. Turning search by
meaning on has the daemon get the model ready: it downloads it if it isn't here, loads it and gives every
entry its vector, and `enter` on that row does it again. Turned off, the daemon lets the model go, and the
memory it took with it.

#### Profiles

A profile is a way of starting an agent you use often: which agent, how, with what standing instructions, and
where. The new-session panel offers your profiles first.

```toml
[[profile]]
name = "review"                            # how the panel shows it
description = "Reads the branch's diff"    # optional: shown under it in the panel
agent = "claude"                           # claude, codex, gemini, opencode, cursor-agent or aider
model = "opus"                             # optional: Claude Code's or Codex's model
effort = "high"                            # optional: Claude Code's effort: low, medium, high, xhigh or max
mode = "plan"                              # optional: Claude Code's permission mode, or Codex's approvals
args = ["--verbose"]                       # optional: more options, after those
prompt = "Review the diff on this branch." # optional: put before the task, a blank line between
instructions = """
You are reviewing, not writing. Point out risks before style,
and say which lines each comment is about.
"""                                        # optional: kept for the whole session, see below
where = "worktree"                         # optional: "here" or "worktree"; else as the panel is set
```

`instructions` stay with the agent for the whole session, on top of its own: Claude Code gets them with
`--append-system-prompt`, and Codex as its `developer_instructions` setting (`-c`), in place of any in its own
`config.toml`; what crystal adds, about a task or the memory, comes after them. The other agents can't be
given any, so a profile for them that has some is an error. `prompt`, unlike `instructions`, is only the start
of the first message.

`mode` is one of `acceptEdits`, `plan` or `bypassPermissions` for Claude Code, and `on-request` or `never` for
Codex. A profile is offered only when its agent is installed. One that can't start, for an agent crystal
doesn't know, with a mode that agent doesn't have, or with the name of another, is an error that says why.

`P` in the TUI lists your profiles: `Enter` changes the one the bar is on, `a` adds one, `c` copies one, and `x`
removes one once you've said `y`. Each setting is a row of the form; `Tab` goes from row to row, `←` / `→`
change a choice, and the form ends with the command the profile runs. `Enter` saves it to the config file,
changing only that profile's lines, so your comments and layout stay as they were. A change that would make
the file one crystal can't read isn't written, and the form says why.

`crystal profile` lists them, and `crystal profile show <name>` prints the command one runs, quoted the way a
shell reads it, with `<task>` where the task goes:

```
$ crystal profile show quick
quick
agent   Codex
starts  wherever the new-session panel is set
runs    codex -a never -c 'developer_instructions="Keep changes small."' -- '<task>'
```

## How it works

```
crystal (TUI) ─────────┐
                       ├── unix socket ──▶ crystal daemon ──┬── PTY ──▶ claude
crystal CLI ───────────┘                                    ├── PTY ──▶ codex
                                                            └── PTY ──▶ zsh
```

One binary is both the client and the daemon. The first `crystal` you run starts the daemon in the background.
The daemon owns the PTYs, tracks each session's status and saves its state to disk. The TUI and the CLI
commands talk to it over a unix socket, so closing the TUI never stops an agent.

`crystal restart-server` hands the daemon over to the crystal it's run from. The daemon finishes the requests
it's answering, gives plugins' hooks a few seconds to finish, then writes down what exec can't carry, each
session's state and screen, and runs the new crystal in its own process with `exec`. The pid stays the same,
so every program is still the daemon's child, and how it ends is still known; the PTYs, a background task's
pipes to its `claude` and the listening socket stay open across the exec, so a client that connects meanwhile
only waits. Attaches and event streams are cut, and come back by themselves: an event stream picks up after
the last event it had, with none missed, and the log has a `daemon.handed_over`. If the new crystal can't take
over, the daemon is restarted cold from the sessions it wrote down first.

## Roadmap

- [x] Project skeleton
- [x] Daemon that runs an agent in a PTY and keeps it alive
- [x] Attach and detach
- [x] Session list in a sidebar
- [x] Session status from Claude Code's hooks
- [x] Session status from the screen, for agents without hooks
- [x] Projects and worktrees
- [x] Resume after a restart
- [x] Split panes
- [x] Tabs
- [x] Agents that start, message, wait on and read other agents
- [x] Tasks that close done or failed, and a backlog per project
- [x] Plugins: crystal's own switched on and off, and your own actions, panes and hooks
- [x] Links in panes, opened with `Ctrl`+click or by a plugin; plugins' builds and startup commands
- [x] An event log, a stream of events on the socket, and waits on it
- [x] Any agent saying what it's doing and how to resume it, and sessions named from their first prompt
- [x] Restart the daemon on a new crystal without stopping its sessions

## Development

```sh
make build      # cargo build
make test       # cargo test
make lint       # cargo fmt --check, and clippy with warnings as errors
make install    # a release build into ~/.local/bin, and the daemon handed over to it
```

If you're an AI agent working on this repository, read [`AGENTS.md`](AGENTS.md) before making changes.

## Acknowledgements

crystal builds on ideas from [tmux](https://github.com/tmux/tmux) and [herdr](https://github.com/herdrdev/herdr).
Its terminal emulator is [Alacritty](https://github.com/alacritty/alacritty)'s, the `alacritty_terminal` crate.

## License

[MIT](LICENSE)
