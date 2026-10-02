# crystal

A terminal workspace for running many coding agents at once: a background daemon that owns each agent's PTY,
a status for every agent, and one list of projects, worktrees and sessions. README.md has the plan and the
roadmap.

## Commands

- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Format: `cargo fmt`

Run lint, format and tests before every commit.

## Layout

- `src/main.rs`: the CLI (clap) and how it prints
- `src/client.rs`: connects to the daemon, starting it when needed
- `src/attach.rs`: `crystal attach`: draws a session in your terminal and sends it your keys
- `src/daemon.rs`: the daemon: listens on the socket and owns the sessions
- `src/session.rs`: one program in a PTY: spawn, exit status, stop, and its screen (vt100) and viewers
- `src/protocol.rs`: requests and responses, one JSON line each, and the frames an attached client sends
- `src/socket.rs`: where the socket lives
- `tests/cli.rs`: end-to-end tests that drive the real binary against a private daemon
