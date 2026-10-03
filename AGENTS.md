# crystal

A terminal workspace for running many coding agents at once: a background daemon that owns each agent's PTY,
a status for every agent, and one list of projects, worktrees and sessions. README.md has the plan and the
roadmap.

## Commands

- Build: `make build` (`cargo build`)
- Test: `make test` (`cargo test`)
- Lint: `make lint` (`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`)
- Format: `cargo fmt`
- Install: `make install`: a release build into `~/.local/bin`, then `crystal restart-server` so a running
  daemon picks it up

Run lint, format and tests before every commit.

## Releasing

Releases are built by `.github/workflows/release.yml`, for macOS (Apple silicon and Intel) and Linux (x86_64
and ARM, static with musl), and installed by `install.sh`.

1. Set the new version in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, and commit both:
   `chore: release 0.2.0`.
2. Optionally try the build first: `gh workflow run release.yml`, then check the run builds every target.
3. Tag the commit with the same version and push the tag: `git tag v0.2.0 && git push origin v0.2.0`. The
   workflow checks the tag against `Cargo.toml`, builds each target, and publishes the GitHub release with
   the archives and their checksums.

The daemon refuses requests from a crystal of another version (except a shutdown), so a user who upgrades is
told to run `crystal restart-server` rather than getting odd errors. Keep `Request::Shutdown` exactly as it
is: it's the one request every version must understand.

## Layout

- `src/main.rs`: the CLI (clap) and how it prints
- `src/client.rs`: connects to the daemon, starting it when needed
- `src/attach.rs`: `crystal attach`: draws a session in your terminal and sends it your keys
- `src/viewer.rs`: the client's side of an attach, shared by `crystal attach` and the TUI's pane
- `src/drive.rs`: `crystal send`, `wait` and `read`, for driving one session from another or a script
- `src/keys.rs`: turning keys into the bytes a terminal sends: the TUI's keys, and the names `send-keys` takes
- `src/tui/`: the TUI (`crystal` with no command)
  - `mod.rs`: the event loop: one channel of events, then update and draw
  - `app.rs`: the state and how keys and the mouse change it; no I/O, so it's unit-tested
  - `ui.rs`: the layout and drawing, and what's under the mouse
  - `mouse.rs`: writes mouse events the way a program in a pane asked for them
  - `help.rs`: the overlay `?` opens, drawn from one table of every key; a test keeps the README's
    table of sidebar keys in step with it
  - `groups.rs`: the sidebar's order and headings: sessions by project, then worktree
  - `text_input.rs`: a one-line text box, for the questions asked on the bottom line
  - `command_line.rs`: reads the line typed at `new session:` into the command to run
  - `pane.rs`: a viewer of a session on screen, the selected one or a split, and its screen
  - `screen_widget.rs`: draws a vt100 screen into ratatui
- `src/daemon.rs`: the daemon: listens on the socket and owns the sessions
- `src/agents.rs`: what crystal knows about particular agents: the hooks it adds to Claude Code, and what they mean
- `src/hook.rs`: `crystal hook <agent>`: what those hooks run, to tell the daemon
- `src/agent_screen.rs`: reading what an agent is doing off its screen and title
- `src/typing.rs`: typing into a session the way a person would: pastes marked, Enter on its own
- `src/session.rs`: one program in a PTY: spawn, exit status, stop, and its screen (vt100) and viewers
- `src/history.rs`: the rows that scroll off a session's screen: keeping them (inline agents' too), reading
  them, and replaying them to a new viewer
- `src/protocol.rs`: requests and responses, one JSON line each, and the frames an attached client sends
- `src/socket.rs`: where the socket lives
- `src/state.rs`: the running sessions, written down to start them again after a restart
- `src/config.rs`: the settings in `~/.config/crystal/config.toml`
- `src/notify.rs`: telling the user when a session needs them: desktop notifications, or their own command
- `src/env.rs`: the environment a session's program starts with
- `src/git.rs`: a directory's project, worktree and branch, and making and removing worktrees (runs `git`)
- `src/shell.rs`: quoting arguments and writing paths with `~`, the way a shell reads them
- `tests/cli.rs`: end-to-end tests that drive the real binary against a private daemon, with a config of
  their own that turns notifications off
