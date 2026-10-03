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
- `src/drive.rs`: `crystal send`, `wait`, `read` and `result`, for driving one session from another or a script
- `src/keys.rs`: turning keys into the bytes a terminal sends: the TUI's keys, and the names `send-keys` takes;
  the old way, or in the Kitty keyboard protocol once a program has asked for it
- `src/remote.rs`: `crystal ssh`: finds (or installs) crystal on another machine, then runs it there over ssh
- `src/skill.rs`: `crystal skill`: prints or installs `skill/SKILL.md`, the Claude Code skill for driving
  crystal; keep it in step with the commands it teaches
- `src/tui/`: the TUI (`crystal` with no command)
  - `mod.rs`: the event loop: one channel of events, then update and draw
  - `app.rs`: the state and how keys and the mouse change it; no I/O, so it's unit-tested
  - `ui.rs`: the layout and drawing (top bar and its tabs, pane headers, footer), and what's under the mouse
  - `tabs.rs`: tabs, each holding its own sessions (each session in exactly one) with its own selection,
    splits and the order of its panes, and which is in front; the sidebar shows only that tab's sessions.
    Kept apart from I/O but for keeping them in a file beside the daemon's state
  - `sidebar.rs`: the sidebar's rows: headings, worktree lines, sessions with their mark and how long ago,
    terminals drawn apart from agents
  - `status.rs`: a session's status as the TUI shows it, and its mark
  - `theme.rs`: every color, named for what it's for: `dark`, `light`, `terminal`, and none for `NO_COLOR`
  - `mouse.rs`: writes mouse events the way a program in a pane asked for them
  - `help.rs`: the overlay `?` opens, drawn from one table of every key, its sections put in two columns to
    fit the terminal; a test keeps the README's table of sidebar keys in step with it
  - `groups.rs`: the sidebar's order and headings: sessions by project, then worktree, agents before
    terminals, each flow run's steps under it, and linked worktrees with no sessions left at the end of
    their project
  - `text_input.rs`: a one-line text box, for the questions asked on the bottom line
  - `text_area.rs`: a text box of several lines that wrap: the new-session panel's task
  - `launcher.rs`: the new-session panel (`n`, `w`): its state and keys, kept apart from I/O, the command
    it builds, what it remembers between runs, and its drawing
  - `command_line.rs`: reads the line typed at `new session:` (the panel's `Ctrl+E`) into the command to run
  - `profiles.rs`: the profiles view (`P`): the list, the form that edits one, its keys and drawing; the
    event loop does the writing
  - `search.rs`: `/`'s matching: a session's name, project, branch or command, letters in order
  - `issues.rs`: the issues view `i` opens: its state, kept apart from I/O, and its drawing
  - `backlog_view.rs`: the backlog view `b` opens: its state and keys, kept apart from I/O, and its
    drawing
  - `pane.rs`: a viewer of a session on screen, the selected one or a split, and its screen, with copy mode
    over it while that's on
  - `copy_mode.rs`: copy mode (`v`): vi's keys over a pane's screen and history, selecting, searching, and
    the text to copy; works on the screen, kept apart from I/O
  - `screen_widget.rs`: draws a session's screen into ratatui, for the panes and `crystal attach`
  - `diff.rs`: reads `git diff`'s patch into files, hunks and lines, marks the words that changed,
    and lays a file out in rows, unified or side by side; pure, so it's unit-tested
  - `diff_view.rs`: the diff view (`d`): its state, keys and drawing, and reading the diff off the
    event loop
  - `fuzzy.rs`: how a path matches a few typed letters, and how well
  - `finder.rs`: the file finder (`p`): its state, keys and drawing, listing files and reading the
    preview off the event loop
  - `memory_view.rs`: the memory view (`m`): a project's entries, the filter, forgetting and
    promoting after a `y`, and its drawing
  - `plugins_view.rs`: the plugins view (`X`): every plugin, on or off, with installed ones' actions and panes,
    its keys and drawing; the event loop does the switching, runs actions and opens plugins' panes over the
    others
- `src/daemon.rs`: the daemon: listens on the socket and owns the sessions
- `src/agents.rs`: what crystal knows about particular agents: the hooks it adds to Claude Code, and what they mean
- `src/catalog.rs`: the agents the new-session panel offers: their names, how each takes a first prompt, their
  options, and which are installed
- `src/codex.rs`: what crystal knows about Codex: finding a session's conversation in its rollouts, and
  `codex resume`
- `src/hook.rs`: `crystal hook <agent>`: what those hooks run, to tell the daemon, and to pass on its reminder
  to an agent ending a turn with its task open
- `src/agent_screen.rs`: reading what an agent is doing off its screen and title
- `src/front.rs`: what's in front in a session's terminal (agent, shell or program), from its foreground process
- `src/typing.rs`: typing into a session the way a person would: pastes marked, Enter on its own
- `src/session.rs`: one program in a PTY, or a task: spawn, exit status, stop, and its screen and viewers
- `src/vt.rs`: a terminal's screen, through `alacritty_terminal`: what a program drew and its history, the modes
  it set, its answers to the program's questions (the daemon's screen only), the output that catches a new viewer
  up, the cells to draw, the input modes `crystal attach` asks your terminal for, and, for a viewer, copy mode's
  cursor, selection and search, which are Alacritty's vi mode. The only module that uses `alacritty_terminal`
- `src/clipboard.rs`: putting text on the user's clipboard: `pbcopy`, `wl-copy`, `xclip` or `xsel` on their own
  machine, or OSC 52 to their terminal over ssh or when none of those works
- `src/task.rs`: tasks: Claude Code run without a terminal (`claude -p`), one run per prompt or follow-up
- `src/transcript.rs`: reading `claude -p`'s stream-json events, and drawing them as a task's transcript
- `src/protocol.rs`: requests and responses, one JSON line each, and the frames an attached client sends
- `src/socket.rs`: where the socket lives
- `src/state.rs`: the running sessions, written down to start them again after a restart, and each
  project's directory in the state dir
- `src/project.rs`: the project a directory is in: its git main worktree, or the directory itself outside git
- `src/tasks.rs`: tasks, sessions started with something to do: the paragraph an agent is told about
  `crystal done`, the reminder for one that ends a turn with its task open, each project's history of closed
  tasks, and `enabled`, the one gate everything tasks add goes through
- `src/backlog.rs`: a project's backlog, numbered items kept in the state dir by the daemon alone, its
  markdown export, and `enabled`, the one gate everything the backlog adds goes through
- `src/work.rs`: `crystal done`, `tasks` and `backlog`
- `src/config.rs`: the settings in `~/.config/crystal/config.toml`, read and checked
- `src/memory.rs`: what a project's sessions learned: the store in the state directory, staleness, search,
  the paragraph Claude Code is shown at launch, promoting into CLAUDE.md, and `enabled`, the one gate
  everything memory adds goes through
- `src/memory_cli.rs`: `crystal remember` and `crystal memory`
- `src/profile.rs`: agent profiles: what one runs, checking it, and saving or removing one in the config file
  with `toml_edit`, so the user's comments and layout stay; `enabled` is the one switch for the feature
- `src/flows.rs`: flows, chains of background tasks on one goal: the `[[flow]]` tables in the config file,
  checking them, filling in a step's prompt, the example `crystal flow example` prints, and `enabled`, the one
  gate everything flows add goes through
- `src/flow_run.rs`: a flow run and how it changes as its steps end and the user answers its gates, kept apart
  from I/O, so it's unit-tested; the daemon starts the steps and writes the runs down
- `src/flow_cli.rs`: `crystal flow` and its commands
- `src/notify.rs`: telling the user when a session needs them: desktop notifications, or their own command
- `src/plugins.rs`: plugins: the registry of crystal's own, `enabled`, the gate every one of them goes through
  (each module's `enabled` asks it), finding installed plugins, switching one in the config's `[plugins]` with
  `toml_edit`, the context and environment their commands run with, their logs, and pausing one that fails
- `src/plugin_manifest.rs`: an installed plugin's `plugin.toml` (actions, events, panes), read and checked, and
  how event patterns match
- `src/plugin_hooks.rs`: the daemon's side of plugins' `[[events]]`: the events, and each plugin's hooks run one
  at a time on a thread of its own, with a timeout, a log, and a pause after failures in a row
- `src/plugin_cli.rs`: `crystal plugin`: listing, switching, running an action, installing, making and
  removing
- `src/env.rs`: the environment a session's program starts with
- `src/git.rs`: a directory's project, worktree and branch, a project's linked worktrees, and making and
  removing worktrees (runs `git`)
- `src/github.rs`: pull requests and issues from GitHub, through `gh` with a timeout; tests use a fake `gh`,
  never the real one
- `src/shell.rs`: quoting arguments and writing paths with `~`, the way a shell reads them
- `tests/cli.rs`: end-to-end tests that drive the real binary against a private daemon, with a config of
  their own that turns notifications and the memory plugin off (a memory test turns it back on), and plugins
  of their own in its plugins directory; a test that opens the new-session panel pins `PATH` to its fake
  agents, so no real agent is found or run. vt100 stands in for the user's own terminal: a second emulator,
  apart from crystal's. A test that copies runs the TUI as over ssh (`SSH_TTY` set), so it asks the terminal
  with OSC 52 and never touches the machine's clipboard
