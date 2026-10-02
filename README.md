<h1 align="center">crystal</h1>

<p align="center"><strong>One terminal for all your coding agents.</strong></p>

<p align="center">
  <a href="#install">install</a> · <a href="#usage">usage</a> · <a href="#how-it-works">how it works</a> · <a href="#roadmap">roadmap</a> · <a href="#development">development</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-666666?labelColor=333333" alt="MIT license" /></a>
  <img src="https://img.shields.io/badge/built%20with-rust-666666?labelColor=333333&logo=rust" alt="built with Rust" />
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20linux-666666?labelColor=333333" alt="macOS and Linux" />
</p>

---

crystal runs Claude Code, Codex, Cursor and any other agent CLI side by side, across all your projects and git
worktrees. The agents keep working after you close it, and you can always see which one is waiting on you.

> [!WARNING]
> crystal is pre-alpha and not usable yet. This README describes what it's being built to do. The
> [roadmap](#roadmap) shows what works today.

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

There are no releases yet. Build from source with Rust 1.88 or newer:

```sh
cargo install --git https://github.com/gabalexander/crystal
```

## Usage

Run `crystal` on its own to open the TUI: every session in a sidebar on the left, and the selected one live in
a pane beside it.

| Key | In the sidebar |
|---|---|
| `j` / `k`, `↓` / `↑` | select a session |
| `Enter` | type into the selected session |
| `n` | start a shell in a new session, and type into it |
| `x` | kill the selected session |
| `q` | quit; the sessions keep running |

While you're typing into a session, every key goes to it except `Ctrl+\`, which takes you back to the
sidebar.

Each row says what its session is doing. Sessions waiting on you move to the top.

| Mark | Meaning |
|---|---|
| `▲` waiting | the agent is asking you something, like a permission |
| `◐` working | the agent is working on a turn |
| `✓` done | the agent finished its turn, and you haven't looked yet |
| `▶` | running: an agent at its prompt, or any other program |
| `■` | ended, with how it exited |

crystal knows what Claude Code is doing from its hooks. It adds them with `--settings` when it starts
`claude`, so your settings files are left alone and your own hooks still run. Other agents show as running
for now.

Everything is also a command, for scripts and for agents:

```sh
crystal new claude                          # start Claude Code here and attach to it
crystal new -d -n review -c ~/code/app codex   # start one in the background, named, somewhere else
crystal ls                                  # list sessions and how they're doing
crystal attach review                       # show a session; Ctrl+\ hands your terminal back
crystal kill review                         # stop one session
crystal kill-server                         # stop every session, and the daemon
```

```
$ crystal ls
NAME    STATE     PID    DIRECTORY   COMMAND
claude  waiting   41210  ~/code/app  claude
docs    done      41377  ~/code/app  claude
review  exited 0  41388  ~/code/app  codex
```

The first `crystal new` starts the daemon. Sessions keep running after you detach or close the terminal, and
`crystal attach` picks up exactly where the screen was. With no name it attaches to the newest session; on a
session that has ended, it prints the last screen and how the program exited.

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
- [ ] Session status from the screen, for agents without hooks
- [ ] Projects and worktrees
- [ ] Resume after a restart
- [ ] Split panes
- [ ] Agents that start, message, wait on and read other agents

## Development

```sh
git clone https://github.com/gabalexander/crystal
cd crystal
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

If you're an AI agent working on this repository, read [`AGENTS.md`](AGENTS.md) before making changes.

## Acknowledgements

crystal builds on ideas from [tmux](https://github.com/tmux/tmux) and [herdr](https://github.com/herdrdev/herdr).

## License

[MIT](LICENSE)
