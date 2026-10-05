<h1 align="center">crystal</h1>

<p align="center"><strong>One terminal for all your coding agents.</strong></p>

<p align="center">
  <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#documentation">docs</a> · <a href="#how-it-works">how it works</a> · <a href="#development">development</a>
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
  drive from the keyboard, with the selected session live next to it, in an order of your own if you like,
  each row laid out your way.
- **One worktree per agent** — give an agent its own branch and `git worktree` in one key, so agents working in
  parallel never edit the same checkout.
- **Conversations survive restarts** — when crystal comes back up, each agent reopens its previous
  conversation. An upgrade doesn't even stop them: the daemon hands every running program over to the new
  crystal.
- **Agents can run agents** — from the CLI or the socket, one agent can start another, send it a task, wait for
  it to finish and read what it said.
- **Bring any agent** — Claude Code, Codex, Cursor, OpenCode, or any program that runs in a terminal, run
  exactly as you'd run it yourself. An agent crystal doesn't know can [tell it what it's
  doing](docs/agents.md#teaching-crystal-about-your-agent), and how to pick its session up again.
- **A single Rust binary** — no Electron, no browser, nothing to host. It works in the terminal you already
  have.

## Install

On macOS or Linux, on Apple silicon, Intel or ARM:

```sh
curl -fsSL https://raw.githubusercontent.com/gabalexander/crystal/master/install.sh | sh
```

It downloads the latest release, checks it against its checksum, and puts `crystal` in `~/.local/bin`.
`CRYSTAL_VERSION=0.1.0` picks a release, and `CRYSTAL_INSTALL_DIR` another directory. With Claude Code on the
machine, it also installs [the skill](docs/driving.md#a-skill-for-claude-code) that teaches Claude Code to
drive crystal; `CRYSTAL_NO_SKILL=1` leaves it out. The Linux builds are static, so they run on any
distribution.

```sh
brew install gabalexander/crystal/crystal     # from a tap of crystal's own
nix run github:gabalexander/crystal           # or build it with Nix
cargo install --git https://github.com/gabalexander/crystal   # or from source, with Rust 1.88 or newer
```

`crystal update` puts the latest release in place of one the install script put there, and hands every running
daemon over to it, so your sessions carry on. [Installing crystal](docs/install.md) has the rest: the tap and
the flake, building from source, `crystal restart-server` after an upgrade, and shell completions.

## Quick start

Run `crystal` on its own to open the TUI: every session in a sidebar on the left, and the selected one live in
a pane beside it. The daemon starts by itself the first time it's needed, and the sessions keep running after
you quit.

`n` opens the new-session panel: type what the agent should do, choose what runs it with `←` and `→`, and
`Enter` starts it. `w` does the same in a new worktree, on a branch of its own. Then:

| Key | What it does |
|---|---|
| `j` / `k` | select a session |
| `Enter` | type into it; `Ctrl+\` hands the keyboard back to the sidebar |
| `Space` | reply to it without going into its pane |
| `u` | go to the next session that needs you |
| `d` | what changed in its worktree |
| `x` | kill it, once you've said `y` |
| `:` | every command by its name |
| `?` | every key, and the [guide](docs/guide.md) |
| `q` | quit; the sessions keep running |

Everything is also a command, for scripts and for agents:

```sh
crystal new -d -n fixer -w fix/login claude "fix the login redirect"   # Claude Code in a new worktree
crystal ls                                      # the sessions and how they're doing
crystal attach fixer                            # show one; Ctrl+\ hands your terminal back
crystal send fixer "add a test for it" --wait   # type into a session, and wait for its turn to end
crystal read fixer --lines 40                   # the end of its screen
crystal integration install                     # crystal's hooks in the agents you start yourself
```

[The guide](docs/guide.md), which `?` then `Tab` shows in the TUI and `crystal guide` prints, is a page on
what to start, the keys that matter most and what agents call.

## Documentation

| Page | What's in it |
|---|---|
| [The guide](docs/guide.md) | one page: what to start, fifteen keys, what agents call, and where things live |
| [Installing crystal](docs/install.md) | the install script, Homebrew and Nix, building from source, updating, shell completions |
| [The TUI](docs/tui.md) | the sidebar and what each row says, `/`, splits and floats, tabs, layouts, copy mode, the mouse and links |
| [Keys and commands](docs/keys.md) | every key, in the sidebar, a pane, a view and a text box; `[keys]`, keys of your own and the command list |
| [Sessions](docs/sessions.md) | the new-session panel, restarts, archiving and idle agents, notifications and sounds, RAM |
| [Projects and worktrees](docs/worktrees.md) | making, moving into and removing worktrees, their hooks, projects' commands, the branch switcher |
| [Diffs, files and pull requests](docs/code.md) | the diff, pull requests and issues on GitHub and GitLab, the file finder, find in files, `crystal open` |
| [Agents](docs/agents.md) | hooks and screen reading, `crystal integration`, Codex, agent rules, and `crystal report` for any agent |
| [Agents driving agents](docs/driving.md) | `send`, `wait`, `read`, streams, `ls --json`, laying out the TUI, and the skill for Claude Code |
| [Tasks and the backlog](docs/tasks.md) | tasks and `crystal done`, handoff notes and kept files, background tasks, the backlog |
| [Flows](docs/flows.md) | chains of tasks on one goal, with gates where you look before it goes on |
| [Memory](docs/memory.md) | what a project's sessions learned, search by meaning, and the distiller |
| [Events and the timeline](docs/events.md) | the event log, `crystal events`, listening on the socket, and the timeline |
| [Plugins](docs/plugins.md) | crystal's own and yours: actions, panes, hooks on events, link handlers, and the list of events |
| [Settings](docs/configuration.md) | every setting, backups, themes, the tab bar and the window, the settings view, profiles |
| [The command line](docs/cli.md) | every command at a glance, `crystal new` and `attach` |
| [Other machines and servers](docs/servers.md) | `crystal ssh`, and servers: daemons of their own, with `--server` |
| [How it works](docs/how-it-works.md) | the daemon, handing it over to a new crystal, and text crystal didn't write |
| [Roadmap](docs/roadmap.md) | what's done so far |

## How it works

```
crystal (TUI) ─────────┐
                       ├── unix socket ──▶ crystal daemon ──┬── PTY ──▶ claude
crystal CLI ───────────┘                                    ├── PTY ──▶ codex
                                                            └── PTY ──▶ zsh
```

One binary is both the client and the daemon. The first `crystal` you run starts the daemon in the background.
The daemon owns the PTYs, tracks each session's status and saves its state to disk. The TUI and the CLI
commands talk to it over a unix socket, so closing the TUI never stops an agent. `crystal restart-server`
hands the daemon over to a newly installed crystal without stopping a session: [how it
works](docs/how-it-works.md).

## Development

```sh
make build      # cargo build
make test       # cargo test
make lint       # cargo fmt --check, and clippy with warnings as errors
make install    # a release build into ~/.local/bin, and the daemon handed over to it
CRYSTAL_UPDATE_API_SCHEMA=1 cargo test api_schema  # write docs/crystal-api.schema.json again
```

A test keeps [`docs/crystal-api.schema.json`](docs/crystal-api.schema.json) what the protocol's types make, and
fails once one of them changes until the schema is written again.

If you're an AI agent working on this repository, read [`AGENTS.md`](AGENTS.md) before making changes.

## Acknowledgements

crystal builds on ideas from [tmux](https://github.com/tmux/tmux) and [herdr](https://github.com/herdrdev/herdr).
Its terminal emulator is [Alacritty](https://github.com/alacritty/alacritty)'s, the `alacritty_terminal` crate.
Its sounds are herdr's, under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
(`assets/sounds/NOTICE`).

## License

[MIT](LICENSE)
