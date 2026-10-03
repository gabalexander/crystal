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
  conversation.
- **Agents can run agents** — from the CLI or the socket, one agent can start another, send it a task, wait for
  it to finish and read what it said.
- **Bring any agent** — Claude Code, Codex, Cursor, OpenCode, or any program that runs in a terminal, run
  exactly as you'd run it yourself.
- **A single Rust binary** — no Electron, no browser, nothing to host. It works in the terminal you already
  have.

## Install

On macOS or Linux, on Apple silicon, Intel or ARM:

```sh
curl -fsSL https://raw.githubusercontent.com/gabalexander/crystal/master/install.sh | sh
```

It downloads the latest release, checks it against its checksum, and puts `crystal` in `~/.local/bin`.
`CRYSTAL_VERSION=0.1.0` picks a release, and `CRYSTAL_INSTALL_DIR` another directory. The Linux builds are
static, so they run on any distribution.

Or build it from source, with Rust 1.88 or newer:

```sh
git clone https://github.com/gabalexander/crystal
cd crystal
make install    # into ~/.local/bin; make install PREFIX=/usr/local for /usr/local/bin
```

or `cargo install --git https://github.com/gabalexander/crystal`.

There's one binary: crystal starts its daemon in the background, from the same binary, the first time it's
needed. A daemon that's already running goes on running the old crystal until it's restarted, so after
upgrading, run `crystal restart-server` (the install script and `make install` do it for you). Running
sessions come back: Claude Code in its conversation, other programs from the start. A crystal that finds a
daemon of another version says so, rather than misunderstanding it.

## Usage

Run `crystal` on its own to open the TUI: every session in a sidebar on the left, and the selected one live in
a pane beside it.

There are no boxes. The sidebar lists each project with a thin rule after its name, its worktrees under it,
and their sessions under those, each with a mark for what it's doing and how long ago that changed. A thin
rule separates the sidebar from the panes; each pane has a header line naming its session, with where it runs
on the right. The bar along the top shows your [tabs](#tabs) and counts the sessions and how many wait on you,
and the footer says where you are and offers the keys that matter there.

| Key | In the sidebar |
|---|---|
| `j` / `k`, `↓` / `↑` | select a session, or a worktree with no sessions |
| `Enter` | type into the selected session, or start an ended one again, once you've said `y`; on a worktree with no sessions, start one there |
| `s` | split the selected session off into a pane of its own, or close its split |
| `H` / `L` | move the selected session's pane a place left or right among the panes (up or down, stacked), swapping it with the pane there |
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
| `n` | start a new session from [the new-session panel](#starting-a-session), and type into it |
| `w` | the same, in a new worktree on a branch named after the task |
| `W` | remove the selected worktree, once nothing runs in it and you've said `y` |
| `r` | rename the selected session |
| `x` | kill the selected session, once you've said `y` |
| `u` | select the next session that needs you: waiting on you first, then done |
| `/` | find a session by typing a little of its name, project, branch or command |
| `o` | open the pull request of the selected session's branch in your browser |
| `i` | list the open issues of the selected session's project, and start an agent on one |
| `b` | open the selected session's project's [backlog](#the-backlog) |
| `c` | close the selected session's [task](#tasks): done or failed, with a line on how it went |
| `g` | on a step of a [flow](#flows): go on past its gate, or run a step that failed or was cut short again |
| `f` | on a step of a flow waiting at its gate: send it back, with notes on what to do differently |
| `d` | show what changed in the selected session's worktree: [the diff](#the-diff) |
| `p` | find a file in the selected session's worktree and edit it: [the file finder](#the-file-finder) |
| `m` | what the selected session's project has remembered: [memory](#memory) |
| `P` | list your [profiles](#profiles), and add, change, copy or remove one |
| `X` | list the [plugins](#plugins): switch them on and off, run their actions and open their panes |
| `?` | show every key, in the sidebar, in a pane, in a question and with the mouse |
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

A split keeps a session on screen while the selection moves on: up to two of them, beside the selected
session's pane when each pane can be at least 80 columns wide, and stacked below it when not. Each pane's
session is sized to its pane. `Tab` from the sidebar goes on to the pane after the one you typed into last, so
`Tab`, then `Ctrl+\`, then `Tab` again walks through them all.

A new split goes after the other panes. To put them in another order, `H` moves the selected session's pane a
place to the left and `L` a place to the right (up and down when they're stacked), swapping it with the pane
that was there; or take a pane by its header line with the mouse and let go over another, and the two swap
places. Each tab keeps its panes in their order, the next time you open the TUI too.

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
it.

`/` finds a session by typing a little of it. The sidebar shows only the sessions that match, under their
project and worktree, with the letters that matched marked in each name. The letters only have to turn up in
order (`rfx` finds `refund-fix`), and each word you type has to turn up in the name, project, branch or
command, so `pay fix` finds the fixer in the payments project. Letters type into the filter, so `↑` and `↓`
(or `Ctrl+P` and `Ctrl+N`) move among the matches; `Enter` selects one and `Esc` leaves the selection where it
was.

For a project whose `origin` is on GitHub, each worktree line shows its branch's open pull request when there
is one: `#57`, and a mark for what matters most about it, in this order:

| Mark | Meaning |
|---|---|
| `✗` | a check failed |
| `±` | a reviewer asked for changes |
| `draft` | it's still a draft |
| `◌` | checks are still running |
| `✓` | approved |

A pull request that's simply ready shows its number alone. `o` opens the selected session's pull request in your
browser. crystal asks [`gh`](https://cli.github.com), GitHub's own command line tool, as soon as it sees a
project and then once a minute, so your login works as it always does and crystal never sees a token. Without
`gh`, or logged out of it, or for a project that isn't on GitHub, nothing is shown; `o` and `i` say why.

`i` lists the open issues of the selected session's project, the latest to change first, with the selected
issue's text under the list. Typing filters them by number, title, label or author. `Enter` on one opens the
[new-session panel](#starting-a-session) on a new worktree with a branch named after it, like
`42-fix-login-redirect`, and the task `Fix issue #42: <its title> (<its address>)`, so the agent knows which
issue and can read it with `gh issue view 42`. `Esc` closes the list.

Everything is also a command, for scripts and for agents:

```sh
crystal new claude                          # start Claude Code here and attach to it
crystal new -d -n review -c ~/code/app codex   # start one in the background, named, somewhere else
crystal new -w fix/login claude             # start one in a new worktree, on a new branch
crystal task "update the docs"              # run Claude without a terminal, in the background (see below)
crystal result task                         # a task's answer
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
crystal restart-server                      # restart the daemon, say after an upgrade; sessions come back
crystal config                              # where the config file is, and the settings in effect
crystal profile                             # list your agent profiles
crystal profile show review                 # what a profile runs, and where it starts
crystal skill --install                     # teach Claude Code to drive crystal (see below)
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
`/` in the branch made a `-`. A branch that doesn't exist yet starts from the commit you're on; one that does
is checked out as it is. `crystal worktree rm` (or `W` in the TUI) takes the worktree's directory or its
branch, refuses while a session is still running in it, and leaves the rest to `git worktree remove`, which
keeps a worktree with changes you haven't committed. Sessions that had ended in it leave the list with it:
their directory is gone, so they could never start again.

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
The list is kept in `~/.local/state/crystal/sessions.json`, without the sessions' environment variables, since
those can hold secrets; a session started again gets the environment of whoever started the daemon again.

### Tabs

A tab is a space of its own: it holds its own sessions, and the sidebar lists only the sessions of the tab
you're in, with that tab's panes beside them. Keep the agents on one feature in one tab, a dev server and its
logs in another, a review in a third, and switch between them.

The tabs sit in the bar along the top, numbered, the one you're in standing out. `t` makes a new one, starts
your shell in it, in the selected session's directory, and takes you there. `[` and `]` go to the tab before
and after, `1` to `9` straight to that one, and a click on a tab goes there too. Each tab keeps its own
selection and splits. `T` names the tab you're in, and the bar shows the name after its number; with too many
to fit, the bar shows only the numbers.

A tab with something going on in it shows that on its label, the way the sidebar marks a session: `▲` when an
agent in it is waiting on you, `✓` when one has finished a turn you haven't looked at, the turning `◐` while
one works. `u` looks through every tab for the next session that needs you, and takes you to its tab.

Every session is in exactly one tab. A session you start from the TUI goes in the tab you're in, and so does
one started any other way, from the command line or another TUI, unless it's a step of a [flow](#flows), which
goes in the tab with the rest of its run. `>` moves the selected session to another tab: press the tab's
number next, or `t` to make a new tab for it. `&` closes the tab you're in and kills the sessions in it, once
you've said `y`; an empty tab closes at once. There's always one tab, and nine at most. They're kept beside the
daemon's state, in `~/.local/state/crystal/`, so they're there when you open the TUI again.

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
`launcher.json` beside `sessions.json`.

Under the task, `Tab` and `Shift+Tab` go from row to row and `←` / `→` change a row's choice:

- **run**: your [profiles](#profiles), then the agents installed on your `PATH` (Claude Code, Codex, Gemini
  CLI, OpenCode, Cursor, Aider), then your shell. What you started last is chosen the next time. A profile's
  description shows under the row, and choosing it sets the rows below from it; you can still change them.
- **how**, for Claude Code: **in a terminal**, or **in the background**, as a [background
  task](#background-tasks) that needs no terminal. Only Claude Code offers it: crystal reads `claude -p`'s
  events for a task's transcript, and Codex's `codex exec` writes another kind it doesn't read yet.
- Claude Code's **model** and **permissions** (`--model`, `--permission-mode`), or Codex's **model** and
  **approvals** (`-m`, `-a`), its models the ones `codex debug models` lists. Left at `default`, no option is
  added.
- **start in**: here (the selected session's worktree, or where you started `crystal`), a new worktree, or
  another project's main worktree.

A new worktree's **branch** is named after the task, its words in lowercase joined by `-`; type in that row to
change it. With no task, `Enter` asks you to name it. `w` opens the panel with a new worktree chosen, and
`Enter` on an issue opens it ready to fix that issue, on a branch named after it.

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
committed, as its pull request would read.

| Key | In the diff |
|---|---|
| `j` / `k`, `↓` / `↑` | the next or previous file |
| `Space` / `Shift+Space`, `PageDown` / `PageUp` | page through the file's diff |
| `]` / `[` | the next or previous hunk |
| `v` | side by side, the old file beside the new one, or unified again; side by side needs 120 columns |
| `b` | the branch since its base, or the uncommitted changes again |
| `Esc` / `q` | back to the sidebar |

The wheel scrolls the diff, and moves through the files over the list.

### The file finder

`p` finds a file in the selected session's worktree, like an editor's quick open: type a few letters of its path,
in order (`rfnd` finds `src/billing/refund.rs`), and the best matches come first, with the start of the
selected one beside the list. Letters in a file's name, at the start of a word, or next to each other count for
more. `↑` / `↓` pick another, `Enter` opens it in your `$EDITOR` (or `vi`) as a session of its own in that
worktree, named after the file, and `Esc` closes the finder. Files git ignores aren't listed.

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
`task` is `null` for a session started with nothing to do, and otherwise holds its [task](#tasks): `goal`,
and once it's closed, `outcome` with `failed` and `summary`. New fields may appear; none goes away. With no
daemon running, it prints `[]`.

#### A skill for Claude Code

Claude Code learns all of this from a skill: when to hand work to another agent, the commands, what each
status means, and the traps, like answering a question with `send-keys` rather than `send`.

```sh
crystal skill --install   # into ~/.claude/skills/crystal, or $CLAUDE_CONFIG_DIR/skills/crystal
crystal skill             # or just print it
```

`--install` won't write over a skill file you've changed; `--force` does. The skill lives in
[`skill/SKILL.md`](skill/SKILL.md), and each crystal carries its own copy, so installing again after an
upgrade brings it up to date.

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

### Background tasks

A task is Claude Code without a terminal: `claude -p`, running a prompt in the background. It sits in the
session list like any session, with a transcript you can watch in the TUI, attach to, or `read`: the prompt,
what Claude says, each tool it uses with the first line of what came back, and how the run ended, with how
long it took and what it cost.

```sh
crystal task -n docs "Update the README for the new flags"           # prints the task's name
crystal task --wait -n tests "Run the tests and fix what fails" -- --permission-mode acceptEdits
crystal result tests                                                 # Claude's answer at the end of the run
crystal send docs "Now the changelog too" --wait                     # a follow-up, in the same conversation
```

- Arguments after `--` go to every `claude -p` the task runs. Nobody is there to say yes to a permission, so
  Claude is refused what isn't allowed; say what is with `--allowedTools` or `--permission-mode`. The
  transcript lists what was refused.
- `crystal send` gives a finished task a follow-up: another run that carries the conversation on with
  `--resume`. One run at a time: a follow-up sent while Claude is still working is refused. A task takes no
  keys, so `send-keys` is refused too.
- `crystal result <task>` prints the last answer; `--json` adds whether the run failed, the conversation's id,
  the cost so far and how many runs the task has had.
- A run that fails, or crashes before saying anything, ends the task, which shows how it exited and why.
  `crystal respawn` runs its prompt again, in its conversation if it got that far.
- After a restart, a task comes back at rest rather than running its prompt again, and what it showed before
  is gone; `crystal send` carries its conversation on.

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
crystal memory rm 3                  # forget one
crystal memory promote 2             # copy one into the project's CLAUDE.md, under "Notes"
```

- `-k` is `decision`, `gotcha`, `command`, `note` (the default) or `outcome`. Inside a session, an entry goes
  to the session's project and says which session added it; elsewhere it goes to the project of the current
  directory, or of `-C <dir>`.
- A project is its main worktree, so every worktree of it shares one list. The list is a JSON file in
  crystal's state directory (`~/.local/state/crystal/memory/`), not in the repository.
- `-f` names a file an entry is about, and can be given more than once. Once that file changes, the entry may
  no longer hold: it's marked stale, and agents aren't shown it. `crystal memory rm` it, or remember it again.
- `promote` asks first at a terminal; `--yes` doesn't. It writes to CLAUDE.md, or to AGENTS.md when that's the
  only one the project has.
- `m` in the sidebar opens the selected session's project's list: the entry the bar is on is shown in full
  beside it, `/` filters, `x` forgets an entry and `p` promotes it, each after a `y`.

When a Claude Code session starts, crystal adds the entries that have most to do with its first prompt (the
newest, without one) to its system prompt, a few at most and none that's stale, with a line on how to add
more. Codex is told nothing: it has no option for a system prompt, and anything crystal typed in would read as
your first message. `crystal plugin disable memory` turns it all off: see [plugins](#plugins).

### Tasks

A session started with something to do is a task: an agent given a task in the new-session panel, `crystal
new -t "<task>" claude` (or simply `crystal new claude "<task>"`), a [background task](#background-tasks), or
one started from the [backlog](#the-backlog). The task stays open until it's closed, done or failed, with a
line on how it went:

- The agent closes it from inside its session: `crystal done "<what was done>"`, or `crystal done --failed
  "<why>"`. crystal tells Claude Code how, on top of its system prompt, and tells Codex at the end of its first
  prompt, opening with a line on where that comes from, so the agent doesn't take it for a stranger's
  instructions. Agents don't always remember to, Haiku least of all, so the first time Claude Code ends a turn
  with its task still open, its Stop hook reminds it and it carries on: to close the task, or, if it isn't
  through, to leave it open and end its turn. `-n <session>` closes another session's task.
- You close it from the TUI: `c` on the session asks `d` done or `f` failed, then for a line on how it went,
  which can stay empty.
- A background task closes itself when its run ends: done with the first line of Claude's answer, or failed.
  A follow-up opens it again.

The sidebar shows a task under its session, what it was asked to do while it's open and `✓` or `✗` with how it
went once it's closed, and so does the pane's header. `crystal ls` has a TASK column, and `ls --json` a `task`
field.

Closed tasks are kept in the project's history in the state directory: what each was asked, when and how it
closed, and the session and branch it ran in. `crystal tasks` lists the project's tasks, open ones first, then
those closed, the latest first; `--all` lists every project's, `-C <dir>` another project's, and `--json`
prints them for scripts.

### The backlog

Each project keeps a backlog: things worth doing later that aren't anyone's task yet. It's the project's, not
a worktree's, so every worktree of a repository shares it, and it's kept in the state directory, out of the
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

A flow is a chain of [background tasks](#background-tasks) on one goal: plan it, build it in a worktree,
review it, open the pull request. Each step runs with a [profile](#profiles) of its own and starts once the
step before it is done, given what that step answered. A step can stop the flow at a gate until you've looked
at what it did, then you go on, or send it back with notes.

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
worktree = true
prompt = """
Do this: {goal}

Follow this plan:
{previous}

{feedback}"""

[[flow.step]]
name = "review"
profile = "reviewer"
gate = true
back_to = "implement"
prompt = "Review the changes on this branch against what was asked: {goal}"

[[flow.step]]
name = "pr"
profile = "shipper"
prompt = "Push this branch and open a pull request for it with `gh pr create --fill`."
```

| Step setting | What it does |
|---|---|
| `name` | what the step is called; its session is named after the run and it, like `ship-1-plan` |
| `profile` | optional: the Claude Code [profile](#profiles) it runs with: model, mode, arguments, instructions, prompt |
| `prompt` | what it's asked, with `{goal}`, `{previous}` and `{feedback}` filled in |
| `worktree` | optional: `true` runs it in a worktree the flow makes, on a branch named after the goal; the steps after it run there too |
| `gate` | optional: `true` stops the flow after it until you go on, or send the flow back |
| `back_to` | optional, on a step with a gate: the step that sending the flow back runs again; left out, this one |

`{goal}` is what you asked the flow to do, and `{previous}` what the step before answered. `{feedback}` is
empty until you send the flow back; from then on it holds your notes and, when it went back to an earlier step,
what the step at the gate said. The step it goes back to hears that even if its prompt doesn't ask for it.

```sh
crystal flow run ship "Retry the webhook when it times out"    # prints the run's name: ship-1
crystal flow                         # every run: how it stands, its step, round and cost
crystal flow show ship-1             # each step: how it stands, its session, runs, cost and answer
crystal flow wait ship-1             # until it waits at a gate or is done; a failed step is an error
crystal flow approve ship-1          # go on past the gate
crystal flow back ship-1 "Keep the old timeout as the default"    # send it back, with notes
crystal flow retry ship-1            # run a step that failed, or that a restart cut short, again
```

- Each step is a background task, so nobody is there to say yes to a permission: give each step a profile that
  allows what it needs, with `mode` and `args`. Only Claude Code runs as a background task, so a step's profile
  is a Claude Code one. Each step's task goes into the project's [history](#tasks) as `ship-1 plan: <goal>`.
- Sending the flow back runs the step it goes back to again, as a follow-up in that step's own conversation,
  then the steps after it again, in a new round. A step that fails stops the run until you run it again, and
  you're told, as you are when a run stops at a gate.
- In the sidebar, a run sits under its project after its worktrees: `◇`, the flow's name and the goal, and the
  round once it's been sent back. Under it is a row for each step: `·` still to come, the working mark while it
  runs, `▲` at its gate, `✓` done, `✗` failed and `■` cut short. A step's row is its task's session, so
  selecting it shows the step's transcript.
- At a gate, the step's session waits on you the way an agent asking something does: you're told, `u` goes to
  it, and `ls` says `waiting`. `g` goes on, and `f` asks for your notes on the footer and sends the flow back.
  On a step that failed or was cut short, `g` runs it again.
- The new-session panel offers your flows after your profiles, `flow: ship`; what you type as the task is the
  goal.
- Runs are kept beside the sessions in the state directory. After a restart, a run waiting at a gate waits
  again, and a step that was running is marked interrupted until you run it again, in its conversation. A
  run's steps start from the environment of the `crystal flow run` that started it; after a restart, from the
  daemon's. The daemon reads the flow and its profiles from the config file as the run starts, so changing
  them never changes a run halfway.

### Plugins

Most of what crystal does beyond running sessions is a plugin you can switch off: tasks, the backlog, memory,
profiles, GitHub, flows and notifications. Plugins of your own add actions, panes over the TUI and hooks on what
happens, and use crystal through its own command line, like any script would.

```sh
crystal plugin                     # every plugin, and whether it's on
crystal plugin disable github      # or enable; written under [plugins] in the config file
crystal plugin new notes           # a plugin to start from, in ~/.config/crystal/plugins/notes
crystal plugin install <git-url>   # or a directory; shows what it runs and asks first
crystal plugin run notes hello     # run one of its actions
crystal plugin log notes           # what its commands printed, and how they failed
crystal plugin remove notes
```

| Plugin | What it adds |
|---|---|
| `tasks` | [tasks](#tasks): `c`, a task under its session, `crystal done` and `tasks`, and telling agents how to close theirs |
| `backlog` | [the backlog](#the-backlog): `b`, the counts beside projects, `crystal backlog`, and telling agents to use it |
| `memory` | [memory](#memory): `m`, `crystal remember` and `memory`, and what Claude Code is shown as it starts |
| `profiles` | [profiles](#profiles): `P`, the profiles in the new-session panel, and `crystal profile` |
| `github` | pull requests on worktree lines, `o` and `i`; switched off, crystal never runs `gh` |
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
```

A command is a list of words, run without a shell from the plugin's directory; a program given as a path is
found from there too. Every command finds crystal in its environment:

- `CRYSTAL_BIN`: the crystal running it, for crystal's own commands, like `"$CRYSTAL_BIN" send
  "$CRYSTAL_SESSION" "…"`
- `CRYSTAL_SOCKET`: that crystal's daemon
- `CRYSTAL_SESSION`, `CRYSTAL_SESSION_ID`: the session it's about, when there is one
- `CRYSTAL_PROJECT`, `CRYSTAL_WORKTREE`: the project's main worktree, and the worktree, it's about

An action is about the session selected in the TUI; for `crystal plugin run`, the session `--session` names,
or else the one it's run in, or else the current directory. Run from the TUI, what it prints goes to the
plugin's log; `plugin run` prints it, and exits as the action did.

A pane is a session of its own, started in the plugin's directory and shown over the panes with the keyboard.
It's in `crystal ls` while it's open, and ends when its program does or when you press `Ctrl+\`. Its
`CRYSTAL_SESSION` is its own; `CRYSTAL_PROJECT` and `CRYSTAL_WORKTREE` are the selected session's.

#### Events

| Event | When |
|---|---|
| `session.started` | a session starts, or starts again |
| `session.waiting` | a session's agent comes to wait on you |
| `session.done` | a session's agent finishes a turn |
| `session.ended` | a session's program ends, or the session is killed |
| `task.closed` | a task closes, done or failed |
| `worktree.created` | crystal makes a worktree |
| `worktree.removed` | crystal removes one |

A hook gets the event as a line of JSON on its standard input, and its name in `CRYSTAL_EVENT`:

```json
{"event":"session.waiting","session":{"name":"claude-2","id":"k3x9…","command":["claude"],"cwd":"/code/app",
 "project":"/code/app","worktree":"/code/app","branch":"main","activity":"waiting","task":"Fix the login redirect"}}
```

`task.closed` has a `task`, with its `goal`, `session`, `project`, `branch` and `outcome` (whether it `failed`,
its `summary`, and when it `closed`). The worktree events have a `worktree`, with its `path`, `branch` and,
once it's made, `project`.

A plugin's hooks run one at a time, in the order things happened, and what they print goes to its log, kept in
crystal's state directory. A hook still running after 30 seconds is stopped. After 5 failures in a row the
plugin is paused, with a notification, and `crystal plugin` shows it `paused` until `crystal plugin enable
<name>` turns it back on.

#### Security

A plugin is code that runs as you, with everything you can reach: your files, your keys, your logins. Its
hooks run in the background, whenever something happens. Add only plugins you'd run as a script of your own.
`crystal plugin install` shows every command a plugin would run and asks before it installs it, and installs
it switched off.

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
| `[plugins]` | | which plugins are on and off: [plugins](#plugins) |

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
does something a plugin adds, and a flow each time one starts, so a change counts straight away; the TUI
reads `new_session`, `theme`, `[plugins]`, the profiles and the flows when it starts, and again when you save
a profile or switch a plugin.

#### Profiles

A profile is a way of starting an agent you use often: which agent, how, with what standing instructions, and
where. The new-session panel offers your profiles first.

```toml
[[profile]]
name = "review"                            # how the panel shows it
description = "Reads the branch's diff"    # optional: shown under it in the panel
agent = "claude"                           # claude, codex, gemini, opencode, cursor-agent or aider
model = "opus"                             # optional: Claude Code's or Codex's model
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
`--append-system-prompt`, and Codex as its `developer_instructions` setting (`-c`). The other agents can't be
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

## Development

```sh
make build      # cargo build
make test       # cargo test
make lint       # cargo fmt --check, and clippy with warnings as errors
make install    # a release build into ~/.local/bin, and the daemon restarted on it
```

If you're an AI agent working on this repository, read [`AGENTS.md`](AGENTS.md) before making changes.

## Acknowledgements

crystal builds on ideas from [tmux](https://github.com/tmux/tmux) and [herdr](https://github.com/herdrdev/herdr).
Its terminal emulator is [Alacritty](https://github.com/alacritty/alacritty)'s, the `alacritty_terminal` crate.

## License

[MIT](LICENSE)
