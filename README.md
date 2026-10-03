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
on the right. The bar along the top counts the sessions and how many wait on you, and the footer says where
you are and offers the keys that matter there.

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
| `/` | find a session by typing a little of its name, project, branch or command |
| `o` | open the pull request of the selected session's branch in your browser |
| `i` | list the open issues of the selected session's project, and start an agent on one |
| `?` | show every key, in the sidebar, in a pane, in a question and with the mouse |
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
| `◐` working | the agent is working on a turn; the mark turns while it does |
| `✓` done | the agent finished its turn, and you haven't looked yet |
| `▸` | running: an agent at its prompt, or any other program |
| `■` | ended: muted when it exited well, red when it failed; the pane's header says how |

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
issue's text under the list. Typing filters them by number, title, label or author. `Enter` on one asks for a
branch named after it, like `42-fix-login-redirect`, for a new worktree, then what to run there, starting out
with `claude Fix issue #42: <its title> (<its address>)`, so the agent knows which issue and can read it with
`gh issue view 42`. `Esc` closes the list.

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
crystal skill --install                     # teach Claude Code to drive crystal (see below)
crystal ssh box                             # crystal's TUI on another machine (see below)
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
directory, under the same name and in the same place in the list, with your environment. Claude Code and Codex
come back in the conversation they were in. Every session's program also gets `CRYSTAL_SESSION_ID`, which stays the same
when the session is renamed, while `CRYSTAL_SESSION` keeps the name the program started under.

The first `crystal new` starts the daemon. Sessions keep running after you detach or close the terminal, and
`crystal attach` picks up exactly where the screen was. With no name it attaches to the newest session; on a
session that has ended, it prints the last screen and how the program exited.

If the daemon dies without being asked to, because it crashed or the machine rebooted, the next `crystal` starts
the sessions that were running again, in the same directories. Claude Code and Codex come back in the
conversation they were in. `crystal kill-server` is asked to stop everything, so after it nothing comes back.
The list is kept in `~/.local/state/crystal/sessions.json`, without the sessions' environment variables, since
those can hold secrets; a session started again gets the environment of whoever started the daemon again.

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
    "status": "waiting"
  }
]
```

`state` is `"running"`, `{"exited": {"code": 3}}` or `{"signaled": {"signal": "Terminated"}}`; `activity` is
`null` for a program that doesn't report what it's doing; `worktree` is `null` outside a git repository. New
fields may appear; none goes away. With no daemon running, it prints `[]`.

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

### Settings

Settings live in `~/.config/crystal/config.toml` (or `$XDG_CONFIG_HOME/crystal/config.toml`). The file is
optional, and so is every setting in it. `crystal config` prints the settings in effect, ready to save as the
file and change. A setting crystal doesn't know is an error that names it, so a typo never goes unnoticed.

| Setting | Default | What it does |
|---|---|---|
| `notify` | `true` | tell you when a session needs you |
| `notify_command` | none | a shell command to run instead of the desktop notification |
| `new_session` | `"claude"` | what the TUI's new-session line starts out with |
| `theme` | `"dark"` | the TUI's colors: `"dark"`, `"light"`, or `"terminal"` |

`dark` and `light` paint their own background, so crystal looks the same in any terminal; `terminal` paints
nothing and uses your terminal's own colors. With `NO_COLOR` set, crystal uses no color at all.

`notify_command` is for telling you some other way, like a message to your phone. It runs with
`CRYSTAL_NOTICE` (the line a notification would show), `CRYSTAL_NOTICE_SESSION` (the session's name) and
`CRYSTAL_NOTICE_ACTIVITY` (`waiting` or `done`) in its environment:

```toml
notify_command = 'curl -s -d "$CRYSTAL_NOTICE" ntfy.sh/my-crystal'
```

The daemon reads the notification settings each time it tells you something, so a change counts straight
away; the TUI reads `new_session` and `theme` when it starts.

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
