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
  daemon picks it up, and `crystal skill --install` (not with `CRYSTAL_NO_SKILL=1`)

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
- `src/client.rs`: connects to the daemon, starting it when needed; `tell` gives it an event from outside, and
  `subscribe` a stream of its events, for the CLI and a TUI to read
- `src/attach.rs`: `crystal attach`: draws a session in your terminal and sends it your keys
- `src/viewer.rs`: the client's side of an attach, shared by `crystal attach` and the TUI's pane
- `src/drive.rs`: `crystal send`, `wait`, `read`, `result`, `answer` and `interrupt`, for driving one session from
  another or a script; waits listen to the daemon's events about their session, and `wait --output` has the
  daemon look at its screen
- `src/keys.rs`: turning keys into the bytes a terminal sends: the TUI's keys, and the names `send-keys` takes;
  the old way, or in the Kitty keyboard protocol once a program has asked for it
- `src/remote.rs`: `crystal ssh`: finds (or installs) crystal on another machine, then runs it there over ssh
- `src/skill.rs`: `crystal skill`: prints or installs `skill/SKILL.md`, the Claude Code skill for driving
  crystal, and brings up to date a copy an earlier crystal installed that nobody has changed, which the daemon
  does as it starts; keep it in step with the commands it teaches, and add its SHA-256 to `SHIPPED` when it
  changes (a test says so)
- `src/tui/`: the TUI (`crystal` with no command)
  - `mod.rs`: the event loop: one channel of events, then update and draw
  - `app.rs`: the state and how keys and the mouse change it; no I/O, so it's unit-tested
  - `ui.rs`: the layout and drawing (top bar and its tabs, pane headers, footer), and what's under the mouse
  - `tabs.rs`: tabs, each holding its own sessions (each session in exactly one) with its own selection,
    its tree of panes, the session the selection's pane last showed and the session floating over them, and
    which is in front; the sidebar shows only that tab's sessions. Kept apart from I/O; the event loop keeps
    them in the database, and tabs kept from when a tab's panes were a list are read as a tree
  - `split_tree.rs`: a tab's panes as a tree of splits, right or down at a ratio, at any depth, one pane
    following the selection: laying them out, borders, the pane beside another on screen, splitting, closing,
    swapping, resizing within each pane's least size, dragging a border and evening them out; pure, so it's
    unit-tested, and the API a command driving the layout would call
  - `layouts.rs`: the layouts view (`S`): the tabs saved under a name and put back, and the tabs a restore
    replaced; its state and keys, kept apart from I/O (the event loop keeps them in the database), and its
    drawing
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
  - `issues.rs`: the issues view (`i`): its state and keys, kept apart from I/O, commenting on an issue and
    changing its title and text, and its drawing
  - `pull_requests.rs`: the pull requests view (`O`): its state and keys, kept apart from I/O, reading one with
    its checks and conversation, its diff, commenting, starting a session in its worktree, and its drawing
  - `listing.rs`: what the issues and pull requests views share: the forge's list filtered as you type, the
    bar kept on its item, each item read whole once, the reading pane, and drawing them
  - `compose.rs`: writing back to the forge from those views: the comment box and the form that edits an
    issue, which keep what's typed until the forge takes it
  - `backlog_view.rs`: the backlog view `b` opens: its state and keys, kept apart from I/O, and its
    drawing
  - `pane.rs`: a viewer of a session on screen, the selected one or a split, and its screen, with copy mode
    over it while that's on
  - `copy_mode.rs`: copy mode (`v`): vi's keys over a pane's screen and history, selecting, searching, and
    the text to copy; works on the screen, kept apart from I/O
  - `screen_widget.rs`: draws a session's screen into ratatui, for the panes and `crystal attach`
  - `diff.rs`: reads `git diff`'s patch into files, hunks and lines, marks the words that changed,
    and lays a file out in rows, unified or side by side; pure, so it's unit-tested
  - `diff_view.rs`: the diff view (`d`): its state, keys and drawing, files marked reviewed sinking to the
    bottom, and reading the diff off the event loop
  - `diff_tree.rs`: the diff view's files folded into a tree of directories (`t`), a chain of directories
    that hold only one another as one row; pure
  - `review.rs`: what the diff view keeps between runs, a document the event loop keeps in the database: the
    files marked reviewed, for each worktree's diff at its commit or each pull request's, with their hashes,
    and whether it lists files as a tree
  - `fuzzy.rs`: how a path matches a few typed letters, and how well
  - `finder.rs`: the file finder (`p`): its state, keys and drawing, and listing files off the event loop
  - `grep.rs`: find in files (`G`): `git grep` as you type, run off the event loop once the typing stops,
    the lines found under their files, the preview around one, and its drawing
  - `switcher.rs`: the branch switcher (`B`): the branches filtered as you type, the question about the
    worktree's uncommitted changes and the commit message, kept apart from I/O, and its drawing
  - `tree_browser.rs`: the tree browser (`E`): a worktree's files as a tree, folded and opened, the filter
    that narrows it to the files that match and their directories, the border dragged, its keys and drawing;
    kept apart from I/O, so it's unit-tested
  - `preview.rs`: the file finder's and the tree browser's preview: a file read and highlighted off the
    event loop, a markdown file's page laid out for its width or its source, scrolling, and drawing them
  - `memory_view.rs`: the memory view (`m`): a project's entries, the filter, forgetting and
    promoting after a `y`, and its drawing
  - `plugins_view.rs`: the plugins view (`X`): every plugin, on or off, with installed ones' actions and panes,
    its keys and drawing; the event loop does the switching, runs actions and opens plugins' panes over the
    others
  - `settings_view.rs`: the settings view (`,`): notifications, the theme, the distiller and search by meaning,
    each changed with a key, and how the model stands; the event loop writes the file (`config::set`) and,
    while it's open, reads the settings and the daemon's `EmbeddingStatus` again every half a second
- `src/daemon.rs`: the daemon: listens on the socket and owns the sessions, and emits an event wherever something
  happens to them, their tasks, flows, worktrees, memory or backlog
- `src/events.rs`: what happens, as events: the one `Event` type, its kinds (a public contract plugins listen
  for), what each carries, how one reads in a line, the filter a reader gives, and the made-up event `plugin run
  --event` tries hooks on; pure, so it's unit-tested
- `src/event_log.rs`: the event log, the `events` table in the database, read and pruned by age and count; and
  the daemon's `Bus`, which numbers each event (a `seq` that never goes back), writes it down and sends it to
  every subscriber: clients streaming over the socket, and the plugins' hooks
- `src/events_cli.rs`: `crystal events`: the log in a shell, filtered, as lines or JSON, or followed, and `--since`
  read as a while back or a time on this machine's clock
- `src/agents.rs`: what crystal knows about particular agents: the hooks it adds to Claude Code, and what they
  mean, and where an agent hears crystal's notes: Claude Code's system prompt, or the top of another's first
  prompt
- `src/catalog.rs`: the agents the new-session panel offers: their names, how each takes a first prompt and
  where it is on a command line, their options, and which are installed
- `src/codex.rs`: what crystal knows about Codex: finding a session's conversation in its rollouts, `codex
  resume`, and crystal's notes given as its developer instructions, after the ones it has already
- `src/hook.rs`: `crystal hook <agent>`: what those hooks run, to tell the daemon, and to pass on its reminder
  to an agent ending a turn with its task open
- `src/agent_screen.rs`: reading what an agent is doing off its screen and title
- `src/front.rs`: what's in front in a session's terminal (agent, shell or program), from its foreground process
- `src/typing.rs`: typing into a session the way a person would: pastes marked, Enter on its own
- `src/session.rs`: one program in a PTY, or a task: spawn, exit status, stop, its screen, viewers and listeners,
  and what has changed in it (its agent's activity, a task's runs) for the daemon to tell
- `src/vt.rs`: a terminal's screen, through `alacritty_terminal`: what a program drew and its history, the modes
  it set, its answers to the program's questions (the daemon's screen only), the output that catches a new viewer
  up, the cells to draw, the input modes `crystal attach` asks your terminal for, and, for a viewer, copy mode's
  cursor, selection and search, which are Alacritty's vi mode. The only module that uses `alacritty_terminal`
- `src/clipboard.rs`: putting text on the user's clipboard: `pbcopy`, `wl-copy`, `xclip` or `xsel` on their own
  machine, or OSC 52 to their terminal over ssh or when none of those works
- `src/task.rs`: tasks: Claude Code run without a terminal (`claude -p`): one process taking the prompt and each
  follow-up over its standard input, the permissions it asks for and their answers, interrupts, each run's cost
  and budget, and letting an idle one go
- `src/claude_stream.rs`: Claude Code's stream-json protocol, as crystal speaks it with a background task's
  `claude -p`: prompts in, the control messages that carry a permission prompt out and its answer back, an
  interrupt, and the rule "always" keeps; adapted from docket's `docket-claude`
- `src/transcript.rs`: reading `claude -p`'s stream-json events, and drawing them as a task's transcript,
  Claude's answers as markdown pages
- `src/markdown.rs`: markdown laid out as a page for one width (pulldown-cmark), each piece marked with what
  it is for the TUI's theme or a transcript's colors to draw, mermaid fences drawn as diagrams; adapted from
  docket's
- `src/syntax.rs`: highlighting code a line at a time, by a file's name or a fence's language: comments,
  strings, numbers and keywords, without a highlighter crate
- `src/mermaid/`: mermaid diagrams drawn as box-drawing text, pure and never panicking; adapted from docket's
  `docket-mermaid`, its drawings checked against `tests/mermaid/`
  - `mod.rs`: `render`, the kinds drawn, and telling which kind a diagram is
  - `canvas.rs`: the grid of cells a diagram is drawn into, and the glyphs, box drawing or ASCII
  - `graph.rs`: the layered layout every kind but the sequence goes through
  - `sequence.rs`: sequence diagrams, read and laid out on their own
  - `flowchart.rs`, `state.rs`, `class.rs`, `er.rs`: each kind read into a graph
  - `width.rs`: how many columns text takes, and cutting it to fit
- `src/mermaid_cli.rs`: `crystal mermaid`: a diagram, or a markdown file's, drawn on standard output
- `src/spending.rs`: what background tasks have spent today, kept in the database by the day: the TUI footer's
  `$X today`, and what `daily_budget_usd` is held against
- `src/protocol.rs`: requests and responses, one JSON line each, and the frames an attached client sends
- `src/socket.rs`: where the socket lives: a server's, named after it in crystal's socket directory, or one
  given by its path; which a command is for (`-S`, `--server`, `CRYSTAL_SOCKET`, then `CRYSTAL_SERVER`); and
  which server a socket is, however it's spelled and whoever starts its daemon, so the same socket always gets
  the same state
- `src/state.rs`: where the daemon's state is: a server's directory in the state dir (the default server's is
  the state dir itself), or beside a socket given by its path; the database, and the files kept before it (the
  sessions, the flow runs, each project's directory); a running session as it's written down to start it again
- `src/server_cli.rs`: `crystal server`: every server with whether it's running and how many sessions it has,
  stopping one, and deleting a stopped one's state
- `src/db.rs`: the SQLite database the daemon and the TUI keep their state in (WAL, `synchronous=NORMAL`,
  migrations by `user_version`, as docket does): the sessions to start again, flow runs, each project's backlog
  and closed tasks, the tasks waiting to start and the last task number, what background tasks spent each day,
  the event log, and the TUI's tabs, layouts, the new-session panel's memory and the diff view's reviewed
  marks, each a JSON document; and
  bringing in the JSON files from before, a project's the first time it's asked for. Settings stay in the
  config file and memory in `memory.db`
- `src/project.rs`: the project a directory is in: its git main worktree, or the directory itself outside git
- `src/tasks.rs`: tasks, sessions started with something to do: the paragraph an agent is told about
  `crystal done`, the reminder for one that ends a turn with its task open, reading a project's closed tasks
  from the file they were kept in before the database, numbering tasks (`t12`) and showing a task waiting to
  start, and `enabled`, the one gate everything tasks add goes through
- `src/backlog.rs`: a project's backlog, numbered items kept in the database by the daemon alone, its
  markdown export, and `enabled`, the one gate everything the backlog adds goes through
- `src/work.rs`: `crystal done`, `tasks` and its commands (`new`, `start`, `show`, `cancel`, `log`), and `backlog`
- `src/config.rs`: the settings in `~/.config/crystal/config.toml`, read and checked
- `src/memory.rs`: what a project's sessions learned: the SQLite store in the state directory with its FTS5
  index (bm25, prefix and porter-stemmed words), each entry's vector and search by meaning merged with it by
  reciprocal rank fusion, its migrations, the same said again seen again, forgotten entries the
  distiller can't add back, bringing in a project's JSON file from before, anchors (each file's SHA-256 when
  an entry was said) and whether an entry holds, fresh, drifting or stale, search, the paragraph every agent is
  shown at launch (entries about what its worktree changed first, in docket's 800 bytes), promoting into
  CLAUDE.md, the markdown export, and `enabled`, the one gate everything memory adds goes through
- `src/distill.rs`: the distiller: after a task closes, one tool-less `claude -p` (Haiku by default, `[memory]`
  in the config) over the end of its transcript, told what the memory has already; its answer checked
  against the checkout before it's kept
- `src/mcp.rs`: `crystal mcp`: an MCP server over stdio with `memory_search` and `memory_show`, which every
  Claude Code session crystal starts, in a terminal or a task in the background, is given with `--mcp-config`
  and its tools allowed
- `src/embed.rs`: search by meaning: bge-small-en-v1.5 run through Candle, `Embed` (the model, or a stand-in in
  tests), downloading it at a pinned revision with its SHA-256s checked, and the one copy each process loads
  when `[memory] embeddings` is on; memory.rs keeps the vectors and merges the rankings
- `src/secrets.rs`: taking credentials out of text before memory keeps it or the distiller reads it
- `src/memory_cli.rs`: `crystal remember` and `crystal memory`, `show`, `export` and `distill` included, and an
  entry in full as `show` and the `memory_show` tool print it
- `src/profile.rs`: agent profiles: what one runs, checking it, and saving or removing one in the config file
  with `toml_edit`, so the user's comments and layout stay; `enabled` is the one switch for the feature
- `src/flows.rs`: flows, chains of background tasks on one goal: the `[[flow]]` tables in the config file,
  checking them, filling in a step's prompt, the example `crystal flow example` prints, and `enabled`, the one
  gate everything flows add goes through
- `src/flow_run.rs`: a flow run and how it changes as its steps end and the user answers its gates, kept apart
  from I/O, so it's unit-tested; the daemon starts the steps and writes the runs down in the database
- `src/flow_cli.rs`: `crystal flow` and its commands
- `src/notify.rs`: telling the user when a session needs them: desktop notifications, or their own command
- `src/plugins.rs`: plugins: the registry of crystal's own, `enabled`, the gate every one of them goes through
  (each module's `enabled` asks it), finding installed plugins, switching one in the config's `[plugins]` with
  `toml_edit`, the context and environment their commands run with, their logs, and pausing one that fails
- `src/plugin_manifest.rs`: an installed plugin's `plugin.toml` (actions, events, panes), read and checked, and
  how event patterns match
- `src/plugin_hooks.rs`: the daemon's side of plugins' `[[events]]`: a subscriber of the bus, each plugin's hooks
  run one at a time on a thread of its own, with a timeout, a log, and a pause (and a `plugin.paused` event)
  after failures in a row; and running a hook here, for `plugin run --event`
- `src/plugin_cli.rs`: `crystal plugin`: listing, switching, running an action or trying hooks on a made-up event,
  installing, making and removing
- `src/env.rs`: the environment a session's program starts with
- `src/git.rs`: a directory's project, worktree and branch, a project's linked worktrees, and making and
  removing worktrees, a pull request's with its commits fetched from `origin`, the patches the diff reads,
  the files a worktree changed since its branch left the default one, and `git grep` stopped once it's no
  longer wanted (runs `git`)
  - `git/branches.rs`: a worktree's branches, local and remote, its uncommitted changes, and switching it to
    another branch or a new one, the changes stashed, brought along, committed or thrown away, and put back
    when git won't switch
- `src/names.rs`: made-up names for new worktrees' branches, like `brave-otter`
- `src/forge.rs`: pull requests and issues from the forge a project's remote is on, GitHub or GitLab, told
  apart by its host and the hosts `gh` and `glab` know: the types both read into, `Repo`'s calls, and running
  the CLI with a timeout; tests use a fake `gh` and `glab`, never the real ones. Was `github.rs`
  - `forge/github.rs`: each call as a `gh` command, and reading its `--json`
  - `forge/gitlab.rs`: each call as a `glab` command, and reading its JSON, a merge request as a pull request
- `src/shell.rs`: quoting arguments and writing paths with `~`, the way a shell reads them
- `tests/cli.rs`: end-to-end tests that drive the real binary against a private daemon (and read its database
  beside its socket to see what it wrote down), with a config of
  their own that turns notifications and the memory plugin off (a memory test turns it back on), plugins
  of their own in its plugins directory, and a Claude Code config directory of their own (`CLAUDE_CONFIG_DIR`),
  since a daemon brings the skill there up to date as it starts; a test that opens the new-session panel pins `PATH` to its fake
  agents, so no real agent is found or run, and a background task's `claude` is a fake that speaks stream-json,
  asks for permissions and takes interrupts. vt100 stands in for the user's own terminal: a second emulator,
  apart from crystal's. A test that copies runs the TUI as over ssh (`SSH_TTY` set), so it asks the terminal
  with OSC 52 and never touches the machine's clipboard. A test of servers by name runs crystal without
  `--socket`, in a runtime dir and a state dir of its own, with `CRYSTAL_SOCKET` and `CRYSTAL_SERVER` taken
  out of its environment, so it never reaches the user's own daemon
