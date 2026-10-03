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

| Key | In the sidebar |
|---|---|
| `j` / `k`, `↓` / `↑` | select a session |
| `Enter` | type into the selected session, or start an ended one again, once you've said `y` |
| `s` | split the selected session off into a pane of its own, or close its split |
| `Tab` / `Shift+Tab` | type into the next pane, or the one before |
| `PageUp` / `PageDown` | page the selected session's pane back through its history, or forward to live |
| `n` | ask what to run, then start it in a new session beside the selected one, and type into it |
| `w` | ask for a branch, then what to run in a new worktree on it, and type into it |
| `W` | remove the selected session's worktree, once nothing runs in it and you've said `y` |
| `r` | rename the selected session |
| `x` | kill the selected session, once you've said `y` |
| `u` | select the next session that needs you: waiting on you first, then done |
| `q` | quit; the sessions keep running |

While you're typing into a session, every key goes to it, `Tab` included, except `Ctrl+\`, which takes you
back to the sidebar, and `Shift+PageUp` / `Shift+PageDown`, which page through the pane's history. Some
terminals keep `Shift+PageUp` for their own scrolling; `Ctrl+\` and then `PageUp` does the same.

Each session keeps the last 2,000 rows that scrolled off its screen, so a pane can page back through what an
agent wrote before you opened it. The title says how far back you are (`↑ 120 lines`), new output doesn't pull
you away while you read, and typing into the session brings you back to live. That includes agents that print
inline through a scroll region, like Codex, whose rows a plain terminal emulator would lose.

The mouse works too. Click a session in the sidebar to select it, or click a pane to type into it. The wheel
moves the selection over the sidebar, and scrolls a pane through its history. A program that asks for the
mouse itself, like `vim` with `set mouse=a` or `htop`, gets the clicks, drags and the wheel in its pane while
that pane has the keyboard. Since crystal takes the mouse, your terminal's own text selection needs a key held:
`Shift` in most terminals, `Option` in iTerm2 and Terminal on macOS.

`n` asks on the bottom line, `new session:`, starting out with the command you used last, at first `claude`.
For an agent, the rest of the line is its first prompt: `claude fix the login bug` starts Claude on that.
Anything else runs as a shell would run it, so `npm run dev` or `sh -c 'make && make test'` work too, and an
empty line (`Ctrl+U` clears it) starts your shell.

A split keeps a session on screen while the selection moves on: up to two of them, beside the selected
session's pane when each pane can be at least 80 columns wide, and stacked below it when not. Each pane's
session is sized to its pane. `Tab` from the sidebar goes on to the pane after the one you typed into last, so
`Tab`, then `Ctrl+\`, then `Tab` again walks through them all.

The sidebar groups sessions by project, then by worktree: `⌂` marks a repository's main worktree and `⎇` a
linked one, each named by its branch. Sessions outside any repository come last, under their directory.
`w` makes its worktree in the selected session's project, or in the repository you started `crystal` in.

Each row says what its session is doing. A project with a session waiting on you moves to the top, and that
session leads its worktree.

| Mark | Meaning |
|---|---|
| `▲` waiting | the agent is asking you something, like a permission |
| `◐` working | the agent is working on a turn |
| `✓` done | the agent finished its turn, and you haven't looked yet |
| `▶` | running: an agent at its prompt, or any other program |
| `■` | ended, with how it exited |

crystal knows what an agent is doing in two ways. When it starts Claude Code itself, it adds hooks with
`--settings`, so your settings files are left alone and your own hooks still run. And for every session it
reads the screen: the spinner an agent puts in its title, "esc to interrupt" while it works, the question
it asks before a command. The screen covers agents without hooks, like Codex or a Claude you started from a
shell, and what hooks never say: a turn you cut short with Esc, or work carrying on once you've said yes.

When a session comes to need you while you're looking elsewhere (its agent asks you something, or finishes a
turn nobody was watching), crystal shows a desktop notification, like "claude-2 is waiting on you · app
fix/login". It uses macOS's own notifications, or `notify-send` on Linux when it's installed. You're told once
each time a session comes to need you, and never about a session you're watching. `u` in the TUI takes you to
it.

Everything is also a command, for scripts and for agents:

```sh
crystal new claude                          # start Claude Code here and attach to it
crystal new -d -n review -c ~/code/app codex   # start one in the background, named, somewhere else
crystal new -w fix/login claude             # start one in a new worktree, on a new branch
crystal worktree rm fix/login               # remove that worktree, once nothing runs in it
crystal ls                                  # list sessions and how they're doing
crystal attach review                       # show a session; Ctrl+\ hands your terminal back
crystal send review "check the diff"        # type into a session and press Enter
crystal wait review                         # block until its agent stops working; print how it ended
crystal read review --lines 20              # print the last 20 rows of its screen
crystal read review --history               # and what scrolled off it before
crystal rename review reviewer              # give a session another name
crystal respawn reviewer                    # run an ended session again; Claude Code in its conversation
crystal kill review                         # stop one session
crystal kill-server                         # stop every session, and the daemon
crystal restart-server                      # restart the daemon, say after an upgrade; sessions come back
crystal config                              # where the config file is, and the settings in effect
```

```
$ crystal ls
NAME    STATE     PID    PROJECT  BRANCH     DIRECTORY                       COMMAND
claude  waiting   41210  app      main       ~/code/app                      claude
fixer   working   41377  app      fix/login  ~/code/app.worktrees/fix-login  claude
review  exited 0  41388  app      main       ~/code/app                      codex
```

`crystal new -w <branch>` makes the worktree beside the repository, in `<repo>.worktrees/<branch>`, with any
`/` in the branch made a `-`. A branch that doesn't exist yet starts from the commit you're on; one that does
is checked out as it is. `crystal worktree rm` (or `W` in the TUI) takes the worktree's directory or its
branch, refuses while a session is still running in it, and leaves the rest to `git worktree remove`, which
keeps a worktree with changes you haven't committed. Sessions that had ended in it leave the list with it:
their directory is gone, so they could never start again.

`crystal rename` changes what a session is called; its program and its saved place after a restart follow the
new name. `crystal respawn`, or `Enter` on an ended session in the TUI, runs its command again in the same
directory, under the same name and in the same place in the list, with your environment. Claude Code comes back
in the conversation it was in. Every session's program also gets `CRYSTAL_SESSION_ID`, which stays the same
when the session is renamed, while `CRYSTAL_SESSION` keeps the name the program started under.

The first `crystal new` starts the daemon. Sessions keep running after you detach or close the terminal, and
`crystal attach` picks up exactly where the screen was. With no name it attaches to the newest session; on a
session that has ended, it prints the last screen and how the program exited.

If the daemon dies without being asked to, because it crashed or the machine rebooted, the next `crystal` starts
the sessions that were running again, in the same directories. Claude Code comes back in the conversation it
was in. `crystal kill-server` is asked to stop everything, so after it nothing comes back. The list is kept in
`~/.local/state/crystal/sessions.json`, without the sessions' environment variables, since those can hold
secrets; a session started again gets the environment of whoever started the daemon again.

### Agents driving agents

Every session knows how to reach its daemon, so an agent can run crystal commands too: start a second agent,
hand it work, wait for it, and read what it said. Here a Claude Code session gets a review of its change:

```sh
crystal new -d -n reviewer claude                         # a second Claude, in the background
crystal send reviewer "Review the diff on this branch" --wait   # prints done, or waiting if it asks something
crystal read reviewer --lines 40                          # the end of its answer
```

`send` types the way a person does: the text first, marked as a paste when the program asks for that, then
Enter on its own, so an agent takes it as a prompt and not as pasted text. `--wait` waits for the turn the
text starts, not one that ended before it. `wait` returns once the agent isn't working: `done`, `waiting` when
it asks something, `idle`, or how its program exited. It takes a `--timeout` in seconds, and fails when that
runs out. A program that doesn't say what it's doing counts as busy until it ends.

### Settings

Settings live in `~/.config/crystal/config.toml` (or `$XDG_CONFIG_HOME/crystal/config.toml`). The file is
optional, and so is every setting in it. `crystal config` prints the settings in effect, ready to save as the
file and change. A setting crystal doesn't know is an error that names it, so a typo never goes unnoticed.

| Setting | Default | What it does |
|---|---|---|
| `notify` | `true` | tell you when a session needs you |
| `notify_command` | none | a shell command to run instead of the desktop notification |
| `new_session` | `"claude"` | what the TUI's new-session line starts out with |

`notify_command` is for telling you some other way, like a message to your phone. It runs with
`CRYSTAL_NOTICE` (the line a notification would show), `CRYSTAL_NOTICE_SESSION` (the session's name) and
`CRYSTAL_NOTICE_ACTIVITY` (`waiting` or `done`) in its environment:

```toml
notify_command = 'curl -s -d "$CRYSTAL_NOTICE" ntfy.sh/my-crystal'
```

The daemon reads the notification settings each time it tells you something, so a change counts straight
away; the TUI reads `new_session` when it starts.

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
- [x] Agents that start, message, wait on and read other agents

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

## License

[MIT](LICENSE)
