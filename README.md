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

### Updating

```sh
crystal update            # install the latest release, if it's newer
crystal update --check    # only say whether a newer one is out
crystal update 0.2.0      # install that release instead, even an older one
```

`crystal update` does what the install script does, in place: it downloads the release for this machine,
checks it against its checksum, runs it once to see that it runs here, and only then puts it in place of the
crystal you ran. Then the new crystal restarts every daemon that's running, each server's, handed over as
`crystal restart-server` does, so your sessions carry on, and brings the skill up to date where Claude Code is
(`CRYSTAL_NO_SKILL=1` leaves it). A crystal installed by Homebrew, mise, Nix or cargo, or one built from
source, is left alone, and the command says what updates it instead. `CRYSTAL_RELEASES` names another place to
download releases from, as it does for the install script.

Once a day, as it opens, the TUI looks for a newer release and says so on its bottom line when there is one.
`check = false` under `[update]` in the [settings](#settings) turns that off.

### Shell completions

`crystal completions <shell>` prints the script that completes crystal's commands and options in bash, zsh,
fish, elvish or PowerShell. In bash, zsh and fish, a command that takes a session's name, like `attach`, `send`
or `kill`, completes the names of the sessions running now; with no daemon running there are none, and none is
started.

```sh
# bash: in ~/.bashrc
eval "$(crystal completions bash)"
# zsh: in a directory on your $fpath, then start a new shell (compinit must run in ~/.zshrc)
crystal completions zsh > ~/.zfunc/_crystal
# fish
crystal completions fish > ~/.config/fish/completions/crystal.fish
# elvish: in ~/.config/elvish/rc.elv
eval (crystal completions elvish | slurp)
# PowerShell: in your $PROFILE
crystal completions powershell | Out-String | Invoke-Expression
```

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
| `Space` | reply to the selected session without going into its pane: a box takes what to say, and `Enter` sends it, typed in with `Enter` after it, or as a [background task](#background-tasks)'s follow-up (`Alt+Enter` or `Ctrl+J` for a new line, `Esc` to cancel) |
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
| `{` / `}` | move the tab you're in one place to the left or right |
| `>` | move the selected session to another tab: then a tab's number, or `t` for a new one |
| `S` | your saved [layouts](#layouts): save your tabs as one, or put them back the way one has them |
| `n` | start a new session from [the new-session panel](#starting-a-session), and type into it |
| `w` | the same, in a new worktree on a branch with a made-up name, like `brave-otter` |
| `W` | remove the selected worktree, once nothing runs in it and you've said `y` |
| `r` | rename the selected session |
| `x` | kill the selected session, once you've said `y` |
| `A` | [archive](#archiving-and-idle-agents) the selected session, once you've said `y`: it stops and leaves the list, to start again where it was |
| `Z` | the archive: start an archived session again, in its conversation, or delete it |
| `!` | run the selected worktree's [project](#projects), with its `run` command, in a terminal of its own; again, stop it |
| `.` | open the selected worktree with its project's `open` command, like `code .` |
| `u` | select the next session that needs you: waiting on you first, then done |
| `U` | list everything that [needs you](#timeline), in every tab, and answer a permission or a gate where it stands |
| `a` | the [timeline](#timeline): what happened, the newest first, as it happens |
| `/` | find a session in any tab, a project or worktree with nothing running, a flow run or an open pull request, by typing a little of it; `Tab` keeps to one status; picking a session in another tab takes you there: [finding with `/`](#finding-with-) |
| `:` | the [command list](#keys-and-commands): every command by its name, with its key, the latest you ran first; `Enter` runs one |
| `(` / `)` | make the [sidebar](#the-sidebar) narrower or wider; its edge drags with the mouse too |
| `\` | fold the [sidebar](#the-sidebar) down to a rail of marks, or unfold it |
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
| `,` | open the [settings](#the-settings-view): notifications, sounds, the theme, and how memory learns and searches, each changed as you go |
| `?` | show every key, in the sidebar, in a pane, in a question and with the mouse: a page at a time when they don't all fit, `→` and `←` (or `Space`, `PgDn` and `PgUp`) turning the pages |
| `q` | quit; the sessions keep running |

While you're typing into a session, every key goes to it, `Tab` included, except `Ctrl+\`, which takes you
back to the sidebar, `Shift+PageUp` / `Shift+PageDown`, which page through the pane's history, and the prefix,
`Ctrl+B`: press it, then any key in the table above, and that key's command runs without the keyboard leaving
the pane, as in tmux. `Ctrl+B` twice sends `Ctrl+B` to the program, and `Esc` after it does nothing. Some
terminals keep `Shift+PageUp` for their own scrolling; `Ctrl+B` and then `PageUp` does the same. Every key in
the table, the prefix and `Ctrl+\` included, can be changed: see [keys and commands](#keys-and-commands).

A program that asks for the [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/), as
Codex does, gets its keys that way, in a pane, through `crystal attach` and from `crystal send-keys`: keys the
old way can't tell apart, like `Esc`, `Shift+Enter` or `Ctrl+I` and `Tab`, reach it as themselves. From your
keyboard that takes a terminal that speaks the protocol too, like Ghostty, kitty, foot or Alacritty; in any
other, keys arrive the old way.

Each session keeps the last 10,000 rows that scrolled off its screen (`scrollback_lines` in the
[settings](#settings)), so a pane can page back through what an agent wrote before you opened it. The title says how far back you are (`↑ 120 lines`), new output doesn't pull
you away while you read, and typing into the session brings you back to live. That includes agents that print
inline through a scroll region, like Codex. To search that history, or copy from it, there's
[copy mode](#zoom-copy-mode-and-search), and `e` opens it in your editor.

The mouse works too. Click a session in the sidebar to select it, or click a pane to type into it. The wheel
moves the selection over the sidebar, and scrolls a pane through its history, three lines a notch. A right click
opens a menu of what you can do with what it's on: a session, a worktree or a project in the sidebar, a tab, or
a pane. Each item is a key from the table above, shown beside it, and does just what that key would there;
choose one with a click, `Enter`, or its key, and `Esc` or a click elsewhere closes the menu. Drag across a pane
to select text: it goes to your clipboard as you let go, and stays marked until you click or type. A
double-click selects a word, where a path is one word and a blank, a comma, a quote, a bracket or a colon ends
one, and a triple-click the whole line, across the rows it wrapped onto; drag on from either and it takes in
whole words, or lines. Drag past the top or bottom of a pane and its history scrolls under the selection, faster
the further past, for as long as you hold it there; the wheel scrolls it too. A program that asks for the mouse
itself, like `vim` with `set mouse=a` or `htop`, gets the clicks, drags and the wheel in its pane while that pane
has the keyboard; there, your terminal's own selection still works with a key held: `Shift` in most terminals,
`Option` in iTerm2 and Terminal on macOS.

Beside each pane's screen, in a column of its own, a scrollbar shows where in its history the pane is, once it
has some: drag its thumb to scroll, or click the track and the thumb jumps there. The wheel over it scrolls the
pane too.

`[mouse]` in the [settings](#settings) changes all this. `copy_on_select = false` keeps what you select from
your clipboard as you let go: the pane goes into [copy mode](#zoom-copy-mode-and-search) with it still selected,
where `y` copies it, the keys change it first, and `Esc` drops it, and the keyboard goes back to where it was
after. `scroll_lines` is how far a notch of the wheel scrolls, and `scrollbars = false` gives the scrollbar's
column back to the pane. `capture = false` leaves the mouse to your terminal altogether: its own selection
works with no key held, but nothing in crystal answers a click, and no program in a pane gets one either.

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

A worktree in the middle of a rebase, a merge, a cherry-pick or a revert, stopped on conflicts say, says so
after its branch, `⎇ fix-login · rebasing`, until it's finished or aborted. A rebase detaches HEAD meanwhile,
but the line keeps the branch being rebased, and its pull request with it. Claude Code makes worktrees of its
own for its subagents, under `.claude/worktrees` in the project, and leaves one behind once it holds a change.
Those come last in their project, named `claude` and the subject of the commit each is at, `⎇ claude · feat:
add the thing`, since their branches are hashes. `Enter` starts a session in one and `W` removes it, as with
any worktree. Better, crystal tells Claude Code to run several fixes as sessions of crystal's instead: see
[agents driving agents](#agents-driving-agents).

A [project](#projects) with no sessions at all stays too: after those with sessions, in every tab, its main
worktree with a `· no sessions` row under it, and its linked worktrees after. `n` or `Enter` there starts
something in it, and `W` takes it off the list, which changes nothing on disk.

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
| `■` | ended: muted when it exited well, red when it failed or [couldn't start again](#usage); the pane's header says how |
| `◌` starting | waiting its turn to start again after crystal [restarted](#usage) |

Each row also says what's in front in the session's terminal when its name doesn't already say it: `claude`,
`codex`, `vite`, `zsh`. crystal asks the terminal which program its keys go to, about once a second, so a
shell you typed `claude` into moves up among the agents, and back among the terminals when Claude exits. It
recognises the agents crystal can start, however they're installed: Claude Code's own binary, named after its
version, or an agent npm runs with `node`.

crystal knows what an agent is doing in two ways. When it starts Claude Code itself, it adds hooks with
`--settings`, so your settings files are left alone and your own hooks still run. And for every session it
reads the screen, by [rules for each agent](#how-crystal-reads-an-agent): the spinner an agent puts in its
title, "esc to interrupt" while it works, the question it asks before a command. The screen covers agents
without hooks, like Codex or a Claude you started from a shell, and what hooks never say: a turn you cut short
with Esc, or work carrying on once you've said yes. The screen only counts while an agent is in front: a shell
or a build printing an agent's words never shows as waiting, and when an agent exits back to its shell, what
it was doing goes with it. `crystal agent explain <session>` shows why crystal reads a session the way it
does. While Claude Code's agent has subagents running, its row says how many after
what's in front: `claude +2`.

A Claude Code or Codex you start yourself, typed into a session's shell, has no hooks of crystal's: crystal
doesn't start it. `crystal integration install` puts crystal's hooks in their own settings, beside yours:

```sh
crystal integration install          # each agent crystal can hook that's installed here
crystal integration install claude   # into $CLAUDE_CONFIG_DIR/settings.json, or ~/.claude/settings.json
crystal integration install codex    # into $CODEX_HOME/hooks.json, or ~/.codex/hooks.json
crystal integration install cursor   # and droid, qodercli, qwen, copilot: see below
crystal integration status           # whether they're there, for this crystal
crystal integration uninstall        # take them out again, and only them
```

Then the agent you typed says what it's doing through its hooks, the same as one crystal starts, and which
conversation it's in: after a restart, the session's shell starts again and `claude --resume <id>` (or `codex
resume <id>`) is typed into it, so you're back where you were. Only while the agent is in front, though: quit
it, and the shell comes back on its own. `resume_reported_agents = false` in the [settings](#settings) turns
that off. Each hook runs `crystal hook <agent> --installed`, crystal by its path, so run `install` again if
you move crystal; `status` says when the hooks are out of date. Outside crystal, and for an agent crystal
started with hooks of its own, they do nothing.

When a session comes to need you while you're looking elsewhere (its agent asks you something, or finishes a
turn nobody was watching), crystal shows a desktop notification, like "claude-2 is waiting on you · app
fix/login". You're told once each time a session comes to need you, and never about a session you're watching:
one shown in the TUI counts as watched only while the TUI's terminal has the focus, as most terminals say. `u`
in the TUI takes you to it, and `U` [lists everything](#timeline) that needs you.

On macOS, crystal uses [`terminal-notifier`](https://github.com/julienXX/terminal-notifier) when it's
installed (`brew install terminal-notifier`), and macOS's own notifications otherwise; on Linux, `notify-send`.
Clicking a notification from `terminal-notifier`, or from a `notify-send` that takes actions (libnotify 0.7.10
on), takes you to the session: the TUI you used last selects it, hands it the keyboard and brings its terminal
to the front (on macOS the terminal's app, on X11 its window with `xdotool`, and inside tmux its window and
pane). The click runs `crystal pane focus --raise <session>`, which you can run yourself.

Two settings, in `[notifications]`, say when to tell you; the [settings view](#the-settings-view) changes both:

```toml
[notifications]
after_secs = 30         # only once a session has needed you this long; one answered sooner is never told
unfocused_only = true   # only while no crystal TUI's terminal has the focus
```

`crystal notify` sends a notification of your own, through the same settings: a script's `crystal notify
"deploy finished"`, or an agent's, which a click takes you back to its session (`-n <name>` names another).

A sound plays at the same moments: one for an agent asking you something, another for one that's done.
crystal plays them with `afplay` on macOS, and on Linux with the first of `paplay`, `pw-play`, `ffplay`,
`mpg123` and `mpv` that's installed; with none, there's no sound. `[sound]` in the
[settings](#settings) switches them off, for every agent or some, or plays files of your own:

```toml
[sound]
enabled = true              # sounds for every agent
request = "sounds/ask.mp3"  # your own, for an agent that asks you something; from the config's directory
done = "~/sounds/done.wav"  # and for one that's done

[sound.agents]
codex = false               # not for Codex, say because it plays its own
```

`CRYSTAL_NO_SOUND=1` keeps crystal quiet whatever the config says.

A program in a session that rings the terminal's bell (`printf '\a'`, `tput bel`, a build that's done) rings
yours: `crystal attach` and the TUI's panes pass the bell of the session they show on to your terminal, which
beeps, flashes or marks its tab, the way you set it up to. A session out of sight that rings gets a `♪` before
its time in the sidebar until you look at it, rings your terminal from the TUI too, and the event log gets a
`session.bell`. A program ringing over and over rings yours at most twice a second.

`/` finds a session by typing a little of it, in any tab, and a project or worktree with nothing running, a
flow run or an open pull request too; `Tab` keeps it to the sessions with one status: see [finding with
`/`](#finding-with-).

For a project on GitHub or GitLab, each worktree line shows its branch's open pull request, `#57` (a merge
request, `!57`, on GitLab), with a mark for what matters most about it; `o` opens it in your browser, `O` lists
the project's pull requests and `i` its issues: see [pull requests and issues](#pull-requests-and-issues).

Everything is also a command, for scripts and for agents:

```sh
crystal new claude                          # start Claude Code here and attach to it
crystal new -d -n review -c ~/code/app codex   # start one in the background, named, somewhere else
crystal new -w fix/login claude             # start one in a new worktree, on a new branch off origin's main
crystal new -w spike --base HEAD claude     # the same, its branch off the commit you're on
crystal new -d -e PORT=4000 npm run dev     # with a variable set in its environment, over yours
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
crystal archive review                      # stop it and keep it in the archive, out of the list
crystal unarchive review                    # start it again where it was, in its conversation
crystal ls --archived                       # the archived sessions
crystal project                             # the projects crystal knows, running or not
crystal project run                         # run this worktree's project in a session of its own
crystal kill-server                         # stop every session, and the daemon
crystal restart-server                      # restart the daemon on this crystal, say after an upgrade
crystal restart-server --cold               # stop it and start it again: sessions start again too
crystal update                              # install the latest release, the daemons restarted on it
crystal completions zsh                     # complete crystal's commands in your shell (see above)
crystal server                              # list the servers, daemons of their own (see below)
crystal config                              # where the config file is, and the settings in effect
crystal profile                             # list your agent profiles
crystal profile show review                 # what a profile runs, and where it starts
crystal pane split review                   # show a session in a pane beside yours in the TUI (see below)
crystal tab new review                      # a new tab in the TUI, in front
crystal title set "deploying"               # the title of the TUI's terminal, until `crystal title clear`
crystal layout                              # the TUI's tabs and how each splits its panes
crystal skill --install                     # teach Claude Code to drive crystal (see below)
crystal integration install                 # hooks for a claude or codex you start in a shell (see above)
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

`crystal new` with no command starts your shell: the one `default_shell` under `[terminal]` in the
[settings](#terminals-the-window-and-the-tab-bar) names, or else `$SHELL`, as a login shell on a Mac. `--env
KEY=VALUE` (`-e`), as many times as you like, sets a variable in the session's environment over the one it
would have from yours: `-e PORT=4000`, `-e DEBUG=` for an empty one. crystal's own, like `TERM` and
`CRYSTAL_SESSION`, can't be changed.

`crystal rename` changes what a session is called; its program and its saved place after a restart follow the
new name. `crystal respawn`, or `Enter` on an ended session in the TUI, runs its command again in the same
directory, under the same name and in the same place in the list, with your environment; on one that couldn't
start again after a restart, it tries again. Claude Code and Codex
come back in the conversation they were in, without being asked their task again. Every session's program also gets `CRYSTAL_SESSION_ID`, which stays the same
when the session is renamed, while `CRYSTAL_SESSION` keeps the name the program started under.

The first `crystal new` starts the daemon. Sessions keep running after you detach or close the terminal, and
`crystal attach` picks up exactly where the screen was. With no name it attaches to the newest session; on a
session that has ended, it prints the last screen and how the program exited.

If the daemon dies without being asked to, because it crashed or the machine rebooted, the next `crystal` starts
the sessions that were running again, in the same directories and in their places in the list. Claude Code and
Codex come back in the conversation they were in. Shells and other programs start straight away, and so does
the first agent, but the agents after it start a quarter of a second apart (`restart_spacing_ms` under
`[sessions]`, `0` for all at once), so a dozen of them don't all load at once; until its turn, an agent's row
says `starting`. A session that can't start again, because its directory has gone or its command isn't there
any more, isn't dropped, and never starts somewhere else instead: it stays in its place, its row says
`couldn't start`, its screen and `crystal ls` say why, and it stays written down, to try again with the next
restart. Put it right and `Enter` on it (or `crystal respawn`) starts it, or kill it. Once they've all started
or failed, the TUI's footer says how it went, like `after the restart: 6 sessions back · 1 couldn't start:
docs`, and the [event log](#events) has a `session.start_failed` for each that couldn't and a
`daemon.restarted` for the lot. `crystal kill-server` is asked to stop everything, so after it nothing comes
back.
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

### Keys and commands

Every key in the sidebar's table runs a command with a name: `n` is `new-session`, `|` is `split-right`, `q` is
`quit`. `crystal keys` lists them all, with the keys your config gives them. `[keys]` in the
[config file](#settings) changes them, a command's name to one key, a list of them, or `"none"`:

```toml
[keys]
prefix = "ctrl+a"          # the prefix, from inside a pane; "none" for no prefix
hand-back = "ctrl+g"       # from a pane back to the sidebar
new-session = ["n", "ctrl+n"]
kill = "X"                 # x is free now
split-right = "v"          # v was copy mode's: copy mode has no key now
quit = "none"              # the command list still runs it
```

A key you give one command is taken from the command that had it, which is left with its other keys, or none.
Two commands given the same key, a command or a key crystal doesn't know, are errors that name them, so a
typo never goes unnoticed. Keys are written as `n`, `N` (or `shift+n`), `ctrl+b`, `alt+enter`, `shift+left`,
`pageup`, `space`, `f5`, or the character itself, like `|`, `(` or `:`. The `?` overlay, the footer and the
command list all say the keys you chose. The keys inside the views (the diff, the file finder and the rest),
copy mode's and the questions' on the footer line stay as they are.

`:` opens the command list: every command by its name, with what it does and its key, and your plugins'
actions after them. Type a little of a name, or of what it does, and `Enter` runs the one the bar is on, as
its key would. Before you type, the five you ran from it last come first, so it's also a quick way back to
what you just did. A command with no key, or whose key you don't remember, is always there.

### The sidebar

The sidebar is 28 columns wide unless `[sidebar]` in the config says otherwise. `(` and `)` take four columns
from it or give it four, or drag the line between it and the panes with the mouse; the TUI keeps the width
you leave it at, until the config gives another. `\` folds it to a rail three columns wide, a session's mark
a row, so a session waiting on you still shows while the panes take the room; `\` again, `)`, or dragging its
edge, unfolds it. While it's folded, or the tab is zoomed, `/` brings it out over the panes to look through.

Whatever needs you, in every tab, is pinned at the top under **needs you**: the agents waiting on you, then
those that finished a turn you haven't looked at. One in another tab says which tab, and a click on it takes
you there. `u` goes to each in turn, and `U` lists them with what each waits for.

```toml
[sidebar]
width = 32             # 16 to 80 columns
folded = false         # start folded
fold = "marks"         # what folding keeps: "marks", or "hidden" for nothing
needs_you = true       # pin what needs you at the top
```

### Finding with `/`

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
| an open pull request (a merge request on GitLab) | its title, number (`57` or `#57`), branch, author or project | opens it in [the pull requests view](#pull-requests-and-issues) |

A directory or a goal only counts where a word turns up in it whole, or nearly anything would find it. Before
you type, only sessions show; projects, worktrees and pull requests join them as you type. The pull requests are
those the sidebar already asked the forge for, and `/` asks, in the background, about the projects with nothing
running the first time it opens, so typing never waits on the forge.

`Tab` keeps to the sessions with one status, the footer saying which, round `waiting`, `working`, `done` (a
finished turn nobody has looked at), `idle` (at the prompt) and `ended`, then back to all of them; `Shift+Tab`
goes the other way. Typing narrows them further. Projects and pull requests have no status, so they don't show
while it keeps to one.

### Tabs

A tab is a space of its own: it holds its own sessions, and the sidebar lists only the sessions of the tab
you're in, with that tab's panes beside them. Keep the agents on one feature in one tab, a dev server and its
logs in another, a review in a third, and switch between them.

The tabs sit in the bar along the top, numbered, the one you're in standing out; `[tab_bar]` in the
[settings](#terminals-the-window-and-the-tab-bar) puts the bar over the footer instead, leaves it out while
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
one started any other way, from the command line or another TUI, unless it's a step of a [flow](#flows), which
goes in the tab with the rest of its run. `>` moves the selected session to another tab: press the tab's
number next, or `t` to make a new tab for it. `&` closes the tab you're in and kills the sessions in it, once
you've said `y`; an empty tab closes at once. The command line makes, names and closes tabs too: see
[laying out the TUI](#laying-out-the-tui). There's always one tab, and as many more as you like. They're kept
in crystal's database, so they're there when you open the TUI again.

### Layouts

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
  CLI, OpenCode, Cursor, Qwen Code, Pi, GitHub Copilot, Amp, Droid, Kimi Code, Kiro, Cline, Kilo Code,
  Devin, Grok, Qoder CLI, Letta Code, Hermes Agent, Antigravity, Aider), then your shell. What you started
  last is chosen the next time. A profile's
  description shows under the row, and choosing it sets the rows below from it; you can still change them.
- **how**, for Claude Code: **in a terminal**, or **in the background**, as a [background
  task](#background-tasks) that needs no terminal. Only Claude Code offers it: crystal reads `claude -p`'s
  events for a task's transcript, and Codex's `codex exec` writes another kind it doesn't read yet.
- Claude Code's **model** (fable, opus, sonnet, haiku), **effort** (low to max) and **permissions** (`--model`,
  `--effort`, `--permission-mode`), or Codex's **model** and **approvals** (`-m`, `-a`), its models the ones
  `codex debug models` lists. Left at `default`, no option is added.
- **start in**: here (the selected session's worktree, or where you started `crystal`), a new worktree, or
  the main worktree of another of the [projects](#projects) crystal knows, sessions running there or not.

A new worktree's **branch** gets a made-up name, an adjective and an animal like `brave-otter`, whatever the
task says; type in that row to change it. A made-up name is always a new branch: if it's taken, the worktree
goes on `brave-otter-2`. One you type that's a branch already is checked out as it is. `w` opens the panel
with a new worktree chosen, and `Enter` on an issue opens it ready to fix that issue, on a branch named after
it.

The panel ends with the command it runs and, for a new worktree, where. `Ctrl+E` hands that command to the
bottom line, `new session:`, to change it or run anything else: `npm run dev` or `sh -c 'make && make test'`
work there, and an empty line starts your shell. Claude Code, Codex, Gemini CLI, OpenCode, Cursor, Qwen Code
and Pi are given the task as they start, each the way it takes one. For the others crystal knows of no way to,
checked against their own code or documentation, so for them the task box gives way to a note.

### Archiving and idle agents

`A` archives the selected session, once you've said `y`: it stops, as `x` would, and leaves the list, but
crystal keeps what it takes to start it again, in the archive. `Z` opens the archive, the latest archived
first, each with where it ran and how long ago: `Enter` starts the one the bar is on again, under its name (or
the next one free, if that's been taken since), and `x` deletes it for good once you've said `y`. Claude Code
and Codex come back in the conversation they were in, as after a restart, and so does an agent that
[said how to resume it](#teaching-crystal-about-your-agent); anything else starts its command again from the
top, and the archive says which. An archived session's open task is cancelled, and open again when it comes
back. From the command line, `crystal archive <name>`, `crystal unarchive <name>` and `crystal ls --archived`
do the same, and `crystal kill` on an archived name deletes it.

An agent you've left alone can be stopped for you, to free what it holds. With `stop_idle_after` under
`[sessions]` in the [settings](#settings), say `"30m"`, crystal stops an agent that has sat at its prompt that
long, its turn seen, with nobody watching it or typing into it. It stays in the list, its row saying
`stopped idle`, and `Enter` (or `crystal respawn`) starts it again in its conversation. Only an agent that can
come back where it was is stopped: Claude Code or Codex once crystal knows its conversation, or an agent that
said how to resume it. A turn that ended while you were away waits for you (`✓`) however long it takes, and so
does one asking you something; terminals, background tasks and sessions with their task open are never
stopped. It's off until you set it, from the file or the [settings view](#the-settings-view).

### Projects

crystal keeps a list of the projects you work in: every git repository a session has run in, and those you
add. A project with no sessions stays in the sidebar and in the new-session panel's "start in", so you can
start something there without a terminal of your own in it. `crystal project` lists them with how many
sessions each has (`--json` for scripts), `crystal project add [dir]` adds the repository a directory is in, and
`crystal project rm [dir]` (or `W` on its row in the sidebar) takes one off the list. Its backlog, tasks and
memory stay, and it's back as soon as a session runs there again.

A project can say how it's run and how it's opened, in `.crystal/project.toml` at the top of a worktree:

```toml
run = "npm run dev"     # runs the project, in a terminal of its own
open = "code ."         # opens the worktree, say in your editor
```

`!` in the sidebar runs the selected session's worktree's `run` command in a session of its own there, called
`run-` and the worktree's directory (`run-app`, `run-fix-login`), and leaves the keyboard where it was; `!`
again asks to stop it, and on one that has ended starts it again. `.` runs the `open` command in the worktree,
in the background, its output thrown away. Both are shell lines, run by your `$SHELL` in the worktree's
directory. A linked worktree without a file of its own uses the main worktree's, so commit the file and every
worktree has it. To keep it out of the repository, or to say otherwise for yourself, a `[[project]]` table in the
[settings](#settings) takes its place:

```toml
[[project]]
path = "~/code/app"     # the project's main worktree
run = "npm run dev -- --port 3001"
open = "cursor ."
```

`crystal project run` and `crystal project open` do the same from the command line, in the worktree you're in
or the one `-C` names; `crystal project run --stop` stops it. The command opens on the machine crystal runs on,
so over [ssh](#other-machines) `open` runs on the other machine.

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
too, but crystal can't add its own as it starts Codex, the way it does for Claude Code: Codex only reads hooks
from its config files, never from the command line, and skips any hook you haven't reviewed. Its `notify`
setting can be given on the command line, but that would replace yours, so crystal leaves it alone.

`crystal integration install codex` puts crystal's hooks in Codex's `hooks.json` instead, and turns on
`[features] hooks` in its `config.toml`, keeping the rest as it was. Codex asks you to review new hooks as it
starts, or in `/hooks`; once you've trusted them, every Codex reports through them, the ones crystal starts and
the ones you type into a shell: a turn starting and ending, the permissions it asks for, its subagents and its
conversation. A turn you cut short with Esc ends it too. Codex runs hooks in the background server its
sessions share, so they can't tell crystal which session they're in: crystal goes by the conversation they
name, and for a conversation it doesn't know yet, by the session Codex was just started in. Codex sessions
started at the same moment may not be told apart until each has been sent a prompt; `codex --no-daemon` keeps
a Codex's hooks to itself.

To pick a conversation up again, crystal finds the file Codex records it in,
`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-…jsonl` (`~/.codex` without `CODEX_HOME`): the one for the session's
directory that Codex started closest to when the session did, within a minute, unless its hooks have named
it. After a restart, or with `crystal respawn`, the session runs `codex resume <id>` with the options it was
started with, but not its first prompt again. The limits:

- Codex writes that file once it has been sent a prompt, so a Codex that was never sent one starts afresh.
- A conversation you begin from inside Codex with `/new` isn't followed, unless its hooks are installed:
  crystal picks the first one up again.
- `codex exec` and Codex's other subcommands run as they were asked, without resuming.
- A Codex you start yourself in a shell session gets its status from the screen, and is resumed only with
  crystal's hooks installed.

### How crystal reads an agent

crystal reads each agent's screen by a file of rules for that agent. It comes with one for each of Claude
Code, Codex, Gemini CLI, OpenCode, Cursor, Qwen Code, Pi, GitHub Copilot, Amp, Droid, Kimi Code, Kiro, Cline,
Kilo Code, Devin, Grok, Qoder CLI, Letta Code, Hermes Agent, Antigravity, Maki and Muse, adapted from
[herdr](https://github.com/herdrdev/herdr)'s, and a common one for any other agent in front, like Aider. An
agent changes what it draws from one version to the next, so when crystal reads one wrong you can mend its
rules yourself without waiting for a release:

```sh
crystal agent list                        # the agents, where their rules come from, installed, hooks
crystal agent explain fix-login           # why crystal reads that session the way it does
crystal agent explain fix-login --agent codex -v   # Codex's rules on its screen, with what each looked at
crystal agent explain --file screen.txt --agent codex --title "⠋ codex"   # rules on a saved screen
crystal agent rules codex > ~/.config/crystal/agents/codex.toml           # start from crystal's own
```

A file in `~/.config/crystal/agents/` takes the place of crystal's rules for the agent its `id` (or one of its
`aliases`) names, and a file for an agent crystal has none for adds it: it's then taken for an agent when it's
in front, and read by its rules. The daemon reads the files again within a couple of seconds of a change. A
file that can't be used is said in `crystal agent list`, `explain` and the daemon's log, and crystal's own
rules stand in for it, so a typo never stops the reading.

```toml
# A file's fields, and every test a rule can make.
id = "codex"                     # the agent, by its program's name
name = "Codex"                   # how crystal shows it
aliases = ["codex-cli"]          # other names its program goes by
packages = ["@openai/codex"]     # npm packages it runs from, as `node …/node_modules/<package>/…`

[[rules]]
id = "approval_question"         # what explain calls it
looks = "waiting"                # working, waiting, settled, or skip
priority = 890                   # of the rules that match, the highest wins; the first in the file on a tie
region = "last_rows(15)"         # where it looks
contains = ["would you like to"] # all of these, in any case
regex = ['\(y\)']                # all of these patterns match
line_regex = ['^› ']             # each of these matches a line
any = [{ contains = ["yes"] }, { contains = ["❯"] }]   # one of these passes
all = [{ contains = ["proceed"] }]                     # every one of these passes
not = [{ contains = ["esc to interrupt"] }]            # none of these passes
```

`skip` is for a screen that says nothing either way, like a menu or a transcript viewer over the prompt: the
status stays as it was. When no rule matches, the agent is settled. A rule looks in one region:

| Region | What it is |
|---|---|
| `screen` | the whole screen (the default) |
| `title` | the title the agent gave its terminal |
| `progress` | the progress it reports (OSC 9;4), as `4;1;-1`: a state, then a percentage |
| `last_rows(N)`, `first_rows(N)` | the last or first N rows with something on them |
| `after_last_rule` | the rows after the last horizontal rule (`───`) |
| `prompt_box`, `above_prompt_box`, `last_row_above_prompt_box` | inside the box between the last two rules, what's above it, and its last row |
| `after_last_prompt`, `before_current_prompt`, `without_current_prompt` | around Codex's prompt line, `›`: the rows after the last one, the rows before the one the user is at, or the whole screen unless the user is at one |

A new look counts once two checks in a row see it, so a screen caught halfway through a redraw doesn't.

#### Hooks in other agents' own settings

Beyond Claude Code and Codex, some agents take hooks only in their own settings files, never on the command
line. crystal leaves those files alone unless you ask, with the same [`crystal
integration`](#usage) command, as for those two:

```sh
crystal integration install cursor     # ~/.cursor/hooks.json, or $CURSOR_CONFIG_DIR's
crystal integration uninstall cursor   # takes crystal's out, and leaves yours
```

It can for Cursor, Droid (`~/.factory/settings.json`), Qoder CLI (`qodercli`), Qwen Code and GitHub Copilot,
with the events each has that say what it's doing or which conversation it's in. The hook runs `crystal hook
<agent>` inside a crystal session only, so the agent anywhere else runs as before, and it never fails the
agent. What a hook says counts only while that agent is in front: an agent that Claude Code runs in the
session doesn't speak for the session. `crystal agent list` says whose hooks are in. The settings file is
written again as formatted JSON, its keys in order.

### Teaching crystal about your agent

crystal knows Claude Code by its hooks, and reads Codex and the other agents it has
[rules](#how-crystal-reads-an-agent) for off their screens. Any other agent, or a script wrapped around one,
can tell crystal what it's doing itself, and how to pick its session up again, with `crystal report`: no
change to crystal, and no waiting for a release of it. Once your
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

An agent asking you something takes nothing `send` types, since the text would land in its question: `send`
refuses with an error that starts `agent_blocked:`, saying what it asks and how to answer it, with `crystal
answer` for a background task or in its pane or with `send-keys` for one in a terminal. `--force` types it
anyway. An agent waiting only because it ended a turn with its [task](#tasks) open is at its prompt, and takes
it. `send-keys` is never refused, since that's how a question is answered.

Sent from another session, a message says so on a line ahead of it, `[crystal] Message from session "scout",
working on task "Port the codec":`, so the agent knows who asks, and that `crystal send` answers. Such a
message loses its control characters but for line breaks, and is cut at 8 KiB. A session may send 20
messages a minute, the most crystal allows: the next is refused, since two agents answering each other are
most likely in a loop. A session can't send to itself. From you, a script or the TUI's `Space`, the text goes
as it is, with no line ahead of it and no limit. Each message is a `session.message` [event](#events).

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
`null` for a program that doesn't report what it's doing; `worktree` is `null` outside a git repository, its
`branch` `null` on a detached HEAD, but for a rebase under way the branch being rebased, and it carries
`in_progress` only while git is in the middle of something there, `merge`, `rebase`, `cherry-pick` or `revert`;
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

The command line lays out the tabs and panes too, so an agent can put the session it started on screen
beside its own, or a script can set up a tab for a review, with the TUI open or not:

```sh
crystal new -d -n tests cargo test
crystal pane split tests                  # beside the pane of the session this runs in; --down below it
crystal pane split logs --beside server --down --ratio 0.7   # server keeps 70% of the room
crystal pane split -e PORT=4000           # no session named: a new shell, split off; prints its name
crystal pane focus tests                  # select it, its tab in front, and type into it; or left, right, up, down
crystal pane focus tests --raise          # the same, and bring the TUI's terminal to the front, as a notification's click does
crystal pane resize left 8 -n tests       # move a border of its pane, as R does; 4 columns or 2 rows by default
crystal pane close tests                  # close its split, or put its float back
crystal pane zoom reviewer                # zoom its tab on it; --off puts the panes back
crystal pane float logs                   # float it over its tab's panes; --off puts it back
crystal pane equalize                     # even out the panes, as = does in resize mode
crystal tab new review                    # a tab after the others, in front: sessions started now go in it; prints 2
crystal tab select 1                      # a tab by its number or its name
crystal tab rename 2 checks               # an empty name takes it back to its number
crystal tab move reviewer review          # move a session to another tab, as > does
crystal tab reorder review 1              # move a tab to be the first, as { and } do a place at a time
crystal tab close review --kill           # a tab with sessions closes only with --kill, which kills them
crystal layout                            # each tab's sessions and how its panes split the room; --json
crystal title set "deploying"             # the title of the TUI's terminal, in place of the settings' one
crystal title clear                       # back to the settings' one
```

A command about a session works on the tab that holds it, whether it's in front or not, and leaves the tab in
front where it is: an agent in another tab lays out its own without taking you there. Only going somewhere
moves you: `tab new`, `tab select` and `pane focus`. Without a session named, a command is about the session
it runs in, or, run outside crystal, the selected one. `pane split` splits the pane that session has of its
own, or else the selection's pane, which it selects if that pane shows something else; the session split off
moves into that tab, out of any pane it had. With no session named, `pane split` starts a new shell to split
off, in the directory it's run in or `--cwd`, with any `--env` variables, as `crystal new` does, and prints its
name. A command that can't be carried out says why, the way the footer would: no room for another pane, a
session that isn't on screen. `title` needs a TUI open: with none, there's no terminal to give the title.

The command goes through the daemon to the TUI you used last, the one where you last pressed a key, clicked
or brought its terminal to the front, and waits for it to answer, a few seconds at most. When `restart-server`
hands the daemon over, each TUI offers itself to the new one at once, saying when you last used it, so
commands carry on going to the same one. What it changes is kept like any change you make, so it's there when
the TUI opens again. With no TUI open, the daemon carries the command out itself, the way the TUI would, on
the tabs as the TUI last kept them, starting if it isn't running; a tab closed with `--kill` has its sessions
killed. The next TUI to open shows the tabs as the commands left them.
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

Every Claude Code session crystal starts is also told, on top of its system prompt, to work on several things
at once as sessions of crystal's, one `crystal new -d -w <branch>` each, rather than in worktrees or subagents
of its own: a session shows in the sidebar with its status, its diff and its screen, where you can step in,
while a worktree Claude Code makes for itself shows only once it's left behind.

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
- Every Claude Code session crystal starts, a task or in a terminal, may run the crystal commands it's told
  to without asking: `crystal done`, `crystal backlog add` and reading the backlog, `crystal handoff`,
  `crystal remember`, and `crystal memory search` and `show`, each only while its plugin is on. Anything
  that changes or removes what's there, like `crystal backlog rm` or `crystal memory rm`, still asks.
- `Ctrl+C` in a task's pane, or `crystal interrupt <task>`, stops the run it's in the middle of. Its task
  stays open, waiting on you, and a follow-up carries on.
- `crystal send`, or `Space` in the TUI, gives a task a follow-up: on the `claude` still there, or once that
  has gone, after five minutes with nothing to do or after a restart, on a new one that carries the
  conversation on with `--resume`. One run at a time: a follow-up sent while Claude is still working is
  refused, and so is one sent while it asks for a permission. A task takes no keys, so `send-keys` is refused
  too.
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
  and rarer ones, come first, drifting and stale ones marked where they rank. With
  [search by meaning](#search-by-meaning), on unless you turn it off, entries that mean the same count too,
  whatever their words, and what doesn't answer the search is left out.
- The same thing remembered again (the same words, whatever the case or punctuation) is the one entry, seen
  again: `remembered 3 already`. Credentials in an entry, like `API_KEY=…` or a token, are taken out as it's
  kept.
- `-f` names a file an entry is about, and can be given more than once. crystal keeps a hash of each file as it
  is then (a file that isn't there isn't counted). Once some of them change, the entry is marked drifting: it
  may hold only in part. Once all of them have changed, or gone, it's stale: a search marks it, and agents
  starting aren't shown it. `crystal memory rm` it, or remember it again, which takes its files as they are now.
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
few at most, in 800 bytes, the least relevant left out first; none that's stale, and drifting ones marked where
they rank. Each comes with its id, and a line on how to read the rest and add more:

- Claude Code gets them in its system prompt, and reads the rest with crystal's MCP tools (below).
- Codex gets them as its `developer_instructions` (`-c`), after the ones it has already, from a
  [profile](#profiles) or its own `config.toml`, and reads the rest with `crystal memory search` and `show`.
- Gemini CLI, OpenCode, Cursor, Qwen Code and Pi get them at the top of their first prompt, when they're given
  one. A prompt that would pass 16 KiB with them loses what the memory has first. An agent that takes no first
  prompt, like Aider, isn't told.

`crystal plugin disable memory` turns it all off: see [plugins](#plugins).

Every Claude Code session crystal starts, in a terminal or as a task in the background (`claude -p`), gets
crystal's own MCP server, `crystal mcp`, with its two tools allowed: `memory_search`, which searches the
project's memory the way `crystal memory search` does, and `memory_show`, which reads one entry in full. These
are how it reads the rest of what was learned without a shell command, which a task has nobody to say yes to
and a session in a terminal would stop to ask about.

#### Search by meaning

Words only find words: "db" never finds "Postgres has to be running". So crystal also searches by what entries
mean, with two models run on your machine through [Candle](https://github.com/huggingface/candle): no API, no
key, and nothing leaves the machine.

- [jinaai/jina-embeddings-v5-text-small](https://huggingface.co/jinaai/jina-embeddings-v5-text-small) turns
  each entry, and each query, into a vector, so an entry that means what a query asks is found whatever its
  words.
- [jinaai/jina-reranker-v3](https://huggingface.co/jinaai/jina-reranker-v3) then reads the query with the 20
  entries found best, by words and meaning together, and scores how well each answers it: what answers comes
  first, what doesn't is left out, and a search about something the memory doesn't hold finds nothing, rather
  than whatever is nearest.

Asked 97 questions about crystal's own memory, it had the right entry among the first five for 92% of them,
against 75% by words alone, and found nothing for most questions the memory couldn't answer.

```sh
crystal memory embed   # downloads both models now (2.4 GB), and gives every entry its vector
```

- The models aren't part of crystal. The daemon downloads them in the background as it starts, when they
  aren't there yet, or `crystal memory embed` does now: with `curl`, at pinned revisions, each file checked
  against its SHA-256, kept in `~/.cache/crystal/models/` (or `$XDG_CACHE_HOME`). Until they're there,
  searches go by words alone, and `crystal memory search` says so.
- They run on a Mac's GPU (Metal), or on the CPU elsewhere; `CRYSTAL_MODELS_ON_CPU=1` keeps them on the CPU
  on a Mac too. The daemon keeps them loaded, about 2.5 GB, once for every client: `crystal memory search`
  and every task's `memory_search` ask it, and only search in their own process when no daemon is running. A
  search takes about half a second on an Apple silicon Mac, most of it the reranker's; on a CPU, a few
  seconds.
- Each entry's vector is kept beside it in `memory.db`. An entry without one, say one just remembered, gets it
  the first time a search needs it, and vectors from a model crystal no longer uses are let go.
- A search ranks by words (bm25) and by meaning, and merges the two by reciprocal rank fusion, so an entry
  high in both comes first; by meaning, only the entries close to the best match count. Then the reranker
  reads the first 20: when not even the best answers the query, the search finds nothing; otherwise the ones
  it rules out are left out, and its ranking is merged in too. What a session is shown as it starts, and what
  the distiller is shown the memory has already, go the same way.
- Both models are licensed [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/): yours to use,
  but not commercially. `embeddings = false` under `[memory]` turns search by meaning off, and `rerank =
  false` leaves the reranker out: faster on a CPU, but a search then always brings back what's nearest.

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
embeddings = true                   # false to search by words alone: see above
rerank = true                       # false to leave the reranker out
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
  session's task. A task isn't done while its worktree is in the middle of a rebase or a merge, stopped on
  conflicts say: `crystal done` refuses then and says so, until the agent finishes or aborts it; `--failed`
  closes it anyway.
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
  background task too, Codex in its developer instructions, and Gemini CLI, OpenCode, Cursor, Qwen Code and
  Pi at the top of their first prompt. When the file's end would make what it's asked and told more than 16
  KiB, it's told where the file is without it; a first prompt too long even so loses what the memory has
  first, then the notes.
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
| `notifications` | telling you when a session needs you, with a notification and a sound |

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
both are. Sounds go with the plugin too, but not with `notify`: `[sound]` has a switch of its own. `memory` used to be a setting of its own; crystal says where it went if it finds one.

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
| `session.start_failed` | a session can't start again after a [restart](#usage): its directory or its command has gone; its `status` says why |
| `session.removed` | a session leaves the list: killed, or its worktree removed |
| `session.archived` | a session is stopped and kept in the archive: `A`, or `crystal archive` |
| `session.claimed` | an agent takes over saying what a session is doing, with [`crystal report`](#teaching-crystal-about-your-agent) |
| `session.released` | it lets go: `crystal report --release`, or it left and the shell is back in front |
| `subagent.started` | a session's agent starts a subagent, as its hooks say: its `subagent`, with its `id` and `agent_type` |
| `subagent.stopped` | that subagent finishes |
| `session.message` | a session is sent a message: by another session with `crystal send`, which its `message` names, or by you |
| `session.bell` | a session's program rings the terminal's bell while nobody's watching it |
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
| `daemon.restarted` | the daemon, restarted cold, has started the sessions that were running again: its `daemon` says how many came back (`sessions`) and which couldn't (`failed`) |

A hook gets the event as a line of JSON on its standard input, the same as the [event log](#events) keeps it,
and its name in `CRYSTAL_EVENT`:

```json
{"seq":412,"at":1790949076244,"event":"session.waiting","project":"/code/app","session":{"name":"claude-2",
 "id":"k3x9…","command":["claude"],"cwd":"/code/app","project":"/code/app","worktree":"/code/app",
 "branch":"main","activity":"waiting","task":"Fix the login redirect","status":"waiting"},"from":"working"}
```

`task.closed` has a `task`, with its `goal`, `session`, `project`, `branch` and `outcome` (whether it `failed`,
its `summary`, and when it `closed`). The worktree events have a `worktree`, with its `path`, `branch` and,
once it's made, `project`. `session.message` has a `message`, with its first `line` and, when another session
sent it, that session's name (`from`) and id (`from_id`). The rest are under [events](#events).

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
| `[notifications]` | | when to tell you: `after_secs`, how long a session must need you first (`0`), and `unfocused_only`, only while crystal's terminal hasn't the focus (`false`): [notifications](#usage) |
| `[sound]` | | the [sounds](#usage) played at the same moments: `enabled` (`true`), your own `done` and `request` files, and `[sound.agents]` to switch them for an agent by its program |
| `new_session` | `"claude"` | what the new-session panel runs at first, until you start something from it |
| `theme` | `"dark"` | the TUI's colors: one of the [themes](#themes) |
| `[colors]` | | colors of your own over the theme's: [themes](#themes) |
| `name_from_prompt` | `true` | name a session you don't name for the [first thing it's asked](#starting-a-session) |
| `resume_reported_agents` | `true` | after a restart, run the command an agent [said resumes it](#teaching-crystal-about-your-agent), or the one that resumes a Claude Code or Codex [typed into a shell](#usage) |
| `scrollback_lines` | `10000` | how many rows that scrolled off a session's screen it keeps, up to 1,000,000, for scrolling back, copy mode, `e` and `crystal read --history` |
| `[plugins]` | | which plugins are on and off: [plugins](#plugins) |
| `[memory]` | | how memory's [distiller](#the-distiller) runs, and whether it [searches by meaning](#search-by-meaning) |
| `[tasks]` | | what [background tasks](#background-tasks) may spend: `max_budget_usd` each (`5`), `daily_budget_usd` all together (none) |
| `[events]` | | `keep_days`, how long the [event log](#events) keeps what happened: 30 days, or `0` for ever |
| `[handoff]` | | `in_git`, the projects, by their main worktree, whose [handoff notes](#the-handoff-file) go in git |
| `[worktrees]` | | `base`, the branch new worktrees' new branches [start from](#usage): `origin`'s default branch unless set |
| `[sessions]` | | `stop_idle_after`, how long an agent may sit [idle](#archiving-and-idle-agents) before crystal stops it, like `"30m"`: `"off"`; `restart_spacing_ms`, how far apart the agents a [crash or a reboot](#usage) starts again start (`250`, or `0` for all at once) |
| `[[project]]` | | a project's [run and open commands](#projects), by its main worktree's `path`, in place of its own file's |
| `[keys]` | | the TUI's keys, by command, its prefix and the key back to the sidebar: [keys and commands](#keys-and-commands) |
| `[sidebar]` | | the sidebar's `width`, whether it starts `folded`, what folding keeps, and whether what needs you is pinned: [the sidebar](#the-sidebar) |
| `[terminal]` | | the shell a new terminal runs, `default_shell`, whether it's a login shell, `shell_mode`, and where `t` starts one, `new_cwd`: [terminals](#terminals-the-window-and-the-tab-bar) |
| `[window]` | | `title`, what the TUI titles its terminal: [the window](#terminals-the-window-and-the-tab-bar) |
| `[tab_bar]` | | where the tab bar goes, whether it's left out with one tab, and what it shows at its right: [the tab bar](#terminals-the-window-and-the-tab-bar) |
| `[appearance]` | | `auto_switch`, the theme following your system's light or dark, and the theme for each: [themes](#themes) |
| `[mouse]` | | `capture`, whether the TUI takes the mouse from your terminal (`true`); `copy_on_select`, whether a selection is copied as you let go or waits in copy mode for `y` (`true`); `scroll_lines`, how far a notch of the wheel scrolls a pane, up to 100 (`3`); `scrollbars`, a scrollbar beside each pane (`true`): [the mouse](#usage) |
| `[update]` | | `check`, whether the TUI looks once a day for a [newer crystal](#updating) (`true`) |

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
starts sessions again, a flow
each time one starts, `[[project]]` each time a project's commands run,
`name_from_prompt` each time it names a session, `resume_reported_agents` as it starts sessions again and
`scrollback_lines` as each session starts, so a change counts straight away (a session already running keeps
what it had); `crystal new` and the TUI read `[terminal]` each time they start a shell; the TUI reads
`new_session`, `theme`, `[colors]`, `[appearance]`, `[window]`, `[tab_bar]`, `scrollback_lines`, `[plugins]`,
`[update]`, `[mouse]`, the profiles and the flows when it starts, again when you save a profile or switch a
plugin, and every half a second while the settings view is open.

#### Themes

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
changes, but crystal's keyboard reader would take that for the start of a key it waits to see the end of.)
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

#### Terminals, the window and the tab bar

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
as it is. A project's [run command](#projects) runs with `$SHELL -c` all the same.

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

`right` lists what the tab bar shows after the count, in order, `separator` between them: `hostname`, this
machine's name; `clock`, the time as `strftime` writes `format` (`%H:%M` unless given); `text`; and
`command`, the last line a shell command prints, run in the directory you started `crystal` in with
`CRYSTAL_SOCKET` set, again `every` while (`10s` unless given), stopped once it takes `timeout` (`2s`), its
colors and other escape sequences taken out, and nothing when it fails. Something with nothing to say is left
out, and on a bar too narrow for it and the tabs, all of it is, the tabs coming first.

#### The settings view

`,` in the sidebar opens the settings you'd otherwise change in the file: notifications and when they come,
sounds, the theme and whether it follows your system's [appearance](#themes) (the row says which theme each
side is), whether the [tab bar](#terminals-the-window-and-the-tab-bar) goes on top or over the footer and is
left out with one tab, how long an agent may sit [idle](#archiving-and-idle-agents), how far apart agents
start again after a [crash or a reboot](#usage), [the mouse](#usage), and how memory learns ([the
distiller](#the-distiller)) and searches ([by meaning](#search-by-meaning)). `space` changes the one the bar
is on, and `←/→` go through the [themes](#themes), forward and back (the row says which of the twenty it's
on), the waits before a notification, the times an agent may sit idle: off, 15 minutes, 30, an hour, two or
eight, the spacing of restarts: all at once, 100 milliseconds, 250, 500, a second or two, and how far a notch
of the wheel scrolls: 1, 2, 3, 5 or 10 lines. Each change is written to the file at once,
keeping the rest of it as you wrote it, comments and all, and counts straight away: the TUI repaints in a new
theme, and the daemon reads the rest as it goes. On a screen too short for every row, the view scrolls to keep
the one the bar is on in sight.

While it's open, the view reads the file and asks the daemon again every half a second, so it follows a
change made by hand in the file too, and shows how the models that search by meaning stand: downloading
(`42 of 2449 MB`), loaded in the daemon or not, and how many entries have their vector. Turning search by
meaning on has the daemon get the models ready: it downloads them if they aren't here, loads them and gives
every entry its vector, and `enter` on that row does it again. Turned off, the daemon lets the models go, and
the memory they took with them.

#### Profiles

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
- [x] Rules for reading each agent's screen in files you can change, and why a session reads as it does
- [x] Projects and worktrees
- [x] Resume after a restart
- [x] Restarts after a crash that start agents a moment apart and keep what can't start, saying why
- [x] Split panes
- [x] Tabs
- [x] Agents that start, message, wait on and read other agents
- [x] Tasks that close done or failed, and a backlog per project
- [x] Plugins: crystal's own switched on and off, and your own actions, panes and hooks
- [x] Links in panes, opened with `Ctrl`+click or by a plugin; plugins' builds and startup commands
- [x] An event log, a stream of events on the socket, and waits on it
- [x] Any agent saying what it's doing and how to resume it, and sessions named from their first prompt
- [x] Restart the daemon on a new crystal without stopping its sessions
- [x] Hooks for a Claude Code or Codex typed into a shell, resumed after a restart, and subagents counted
- [x] Archived sessions, idle agents stopped, right-click menus, and projects kept with their run and open commands

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
Its sounds are herdr's, under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
(`assets/sounds/NOTICE`).

## License

[MIT](LICENSE)
