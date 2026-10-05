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
  daemon is handed over to it, its sessions carrying on, and `crystal skill --install` (not with
  `CRYSTAL_NO_SKILL=1`)

Run lint, format and tests before every commit.

Every type a request, a response or an event reaches derives `schemars::JsonSchema` in test builds alone
(`#[cfg_attr(test, derive(schemars::JsonSchema))]`, schemars being a dev-dependency), a new one too, and one
whose name another has, or says little alone, takes `#[cfg_attr(test, schemars(rename = "..."))]`. A test
keeps `docs/crystal-api.schema.json` what they make: after changing one, `CRYSTAL_UPDATE_API_SCHEMA=1 cargo test
api_schema` writes it again.

What a command prints on standard output goes through `out!` and `outln!` (`src/output.rs`) with a `?`, never
`print!` and `println!`, which `clippy.toml` refuses: a reader gone, like `head -1`'s, stops the command and
crystal exits 0, where `println!` panics. A line said along the way of work that must finish, like `crystal
update`'s, is printed with `let _ =`, the work going on unread.

## Releasing

Releases are built by `.github/workflows/release.yml`, for macOS (Apple silicon and Intel) and Linux (x86_64
and ARM, static with musl), and installed by `install.sh`.

1. Set the new version in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, and commit both:
   `chore: release 0.2.0`.
2. Optionally try the build first: `gh workflow run release.yml`, then check the run builds every target.
3. Tag the commit with the same version and push the tag: `git tag v0.2.0 && git push origin v0.2.0`. The
   workflow checks the tag against `Cargo.toml`, builds each target, and publishes the GitHub release with
   the archives and their checksums.

`install.sh` and `crystal update` (`src/update.rs`) both find a release's archive and checksum by the names the
workflow gives them, and so does the Homebrew formula (`packaging/homebrew/crystal.rb`): change one, change all
four (a test in `update.rs` checks the formula and the workflow). For a tap, `packaging/homebrew/formula.sh
0.2.0` prints the formula with that release's version and checksums filled in; `flake.nix` builds from source
and reads the version from `Cargo.toml`.

The daemon refuses requests from a crystal of another version (except a shutdown and a handover), so a user
who upgrades is told to run `crystal restart-server` rather than getting odd errors, and one left on an older
crystal to start it again. Keep `Request::Shutdown` and `Request::Handover` exactly as they are: they're the
requests every version must understand. `restart-server` hands the daemon over to the new binary (see
`src/handover.rs`), and a daemon only hands over to a crystal that reads its `handover::FORMAT`: bump it
whenever what's handed over changes in a way the crystal before couldn't read.

## Layout

- `src/main.rs`: the CLI (clap) and how it prints
- `src/client.rs`: connects to the daemon, starting it when needed; `tell` gives it an event from outside,
  `subscribe` a stream of its events, for the CLI and a TUI to read, which picks up again after a handover,
  and `lay_out` a layout command for the TUI; restarting the daemon, handed over or cold
- `src/layout.rs`: laying out the TUI from the command line: the commands `crystal tab`, `crystal pane`,
  `crystal title`, `crystal layout apply`, `crystal plugin pane open` and `crystal open` send (a session
  shown over the panes or in a popup, and files shown, only by a TUI), the order a TUI gets with the id of
  the session it was run in, what the TUI reports back, the layout it answers with, where the daemon says the
  user is, and how `crystal layout` prints it
- `src/layout_file.rs`: layout files, as herdr's `layout.export` and `layout.apply` take them: `crystal layout
  export` writing the tabs with what starts each session again, and `crystal layout apply` reading one (the
  shape `crystal layout --json` prints, with a session's `cwd`, `command` and `env` where it's named),
  checking it, starting what isn't there that it says how to, and the tabs it comes to, what couldn't start
  left out; pure but for reading the file and asking the daemon, so it's unit-tested
- `src/layout_relay.rs`: the daemon's side of those commands: the TUIs that take orders and which was used last
  (a key, a click, its terminal brought to the front), whether each one's terminal has the focus, which says
  where the user is for notifications, an order written to that one and its answer handed back to the command
  waiting, a few seconds at most, or `NoTui`, for the daemon to carry it out itself on the tabs the TUIs keep;
  nothing is handed over, as each TUI offers again after a handover saying when it was last used, and a
  command just after the daemon starts waits a moment for one to come back
- `src/open.rs`: `crystal open`: files shown to the user in the TUI used last, as a layout command, what an
  agent runs when they ask to see one: each checked to be a text file and made absolute, and shown from the
  top of the worktree it runs in; with no TUI open, nothing shown and the command failing, for the agent to
  name the paths; adapted from docket's file tabs
- `src/attach.rs`: `crystal attach`: draws a session in your terminal and sends it your keys, your terminal
  asked for what its program asked of them (the wheel's arrows only while it's on the alternate screen, your
  terminal's own put back after), attaching again after a handover, and passes its bell and what its program
  copies on; the keys it takes for itself, read out of what your terminal sends with the TUI's keymap: the
  prefix then copy mode's key or the page keys (the prefix twice the program's, and what can follow it said at
  the top right while it waits), `Shift+PageUp` and `Shift+PageDown`, a `direct+` one, and every key in copy
  mode, its search typed on the bottom row, and the releases of those keys kept from the program; with `[mouse]
  attach_capture`, the mouse taken: SGR reports picked out of the keys (one cut off held a moment for its end),
  handed to a program that asked, a drag selecting (a word on a double-click, a line on a triple-click, the
  history scrolling under one held on the top or bottom row), copied as it lets go or held in copy mode, the
  wheel as arrows for a pager or scrolling the session's history, and back to live as you type; what it does
  with the keys and the mouse kept apart from I/O, so it's unit-tested
- `src/bell.rs`: passing a session's terminal bell on to the user's own terminal, at most one every half a
  second
- `src/viewer.rs`: the client's side of an attach, shared by `crystal attach`, the TUI's pane and the streams,
  which attach as a program rather than the user
- `src/drive.rs`: `crystal send` (its text from standard input with `-`, a task's run stopped first with
  `--interrupt`), `wait`, `read` (`--ansi`, `--unwrap`, `--since`), `process-info`, `result`, `answer` and
  `interrupt`, for driving one session from another or a script; waits listen to the daemon's events about their
  session, and `wait --output` has the daemon look at its screen, asking again when a handover cuts it; `task
  --wait`'s run, done or failed; a wait that gives up is a `TimedOut`, which `crystal` exits 2 for
- `src/stream.rs`: `crystal observe` and `control`: a session's terminal as JSON lines, its output base64,
  attached again after a handover, and `control`'s commands on standard input (input, keys, resize, release)
- `src/api.rs`: `crystal api snapshot`: the sessions, layout, projects, open tasks, flow runs and archive in one
  JSON document, with the latest event's seq to follow on from
  - `api/schema.rs`: `crystal api schema`: the JSON Schema of the socket protocol, `docs/crystal-api.schema.json`
    bundled as it is, a line on each message it names, or the whole of it printed or written to a file; and the
    tests that make it from the types, keep the file in step and check messages crystal writes against it
- `src/messages.rs`: what `crystal send` carries from one session to another: the text tidied and cut to 8 KiB,
  the line ahead of it saying which session sent it, the guard that holds a session to 20 sends a minute, the
  refusal of a message that only acknowledges (`ok`, `thanks`, `👍`), and the `agent_blocked:` refusal for an
  agent asking the user something; adapted from docket's
- `src/keys.rs`: turning keys into the bytes a terminal sends: the TUI's keys, and the names `send-keys` takes;
  the old way, or in the Kitty keyboard protocol once a program has asked for it; and back, what a terminal
  sends read into keys, either way, pastes and the rest, each with its bytes, for `crystal attach`
- `src/remote.rs`: `crystal ssh`: finds (or installs) crystal on another machine, then runs it there over ssh
- `src/update.rs`: `crystal update`: the latest release (where GitHub's `releases/latest` redirects, or
  `CRYSTAL_RELEASES`), downloaded with `curl`, checked against its SHA-256, unpacked, tried, and renamed over
  this crystal, unless a package manager, cargo or a build from source put it there; then the new crystal
  restarts every running daemon and installs its skill, since only it reads its own handover; the TUI's
  look for a newer release, once a day, kept in the database; and a release's notes (its body from GitHub's
  API, or `release-notes.md` beside a mirror's files), kept in the state directory by the update for the new
  crystal's TUI to show once, or again when `release-notes` asks, which crystal it last opened as kept in the
  database, and `--notes`
- `src/completions.rs`: `crystal completions`: clap's script for each shell, without the hidden commands, and
  in bash, zsh and fish the running sessions' names (`crystal complete-sessions`, which never starts a daemon)
  where a command takes one, the arguments in `SESSION_ARGS`; keep that list in step with the commands
- `src/skill.rs`: `crystal skill`: prints or installs `skill/SKILL.md`, the Claude Code skill for driving
  crystal, and brings up to date a copy an earlier crystal installed that nobody has changed, which the daemon
  does as it starts; keep it in step with the commands it teaches, and add its SHA-256 to `SHIPPED` when it
  changes (a test says so)
- `src/config_bundle.rs`: `crystal config export` and `import`: the config file's text and the user's agent
  rule files as one JSON bundle, and a bundle, a config file or a directory of either merged in with
  `toml_edit`, a setting the bundle has replacing this one, profiles and flows by name, projects by path, keys'
  commands by key, nothing written unless the result makes sense; pure but for the files, so it's unit-tested
- `docs/guide.md`: the guide, one page on what to start, the keys that matter most, what agents call and
  where things live, which the `?` overlay's second tab shows and `crystal guide` prints
- `packaging/homebrew/`: the Homebrew formula for a tap, `crystal.rb`, installing a release's archive for the
  machine, and `formula.sh`, which fills in a release's version and checksums; `flake.nix` builds crystal
  from source with Nix
- `src/tui/`: the TUI (`crystal` with no command)
  - `mod.rs`: the event loop: one channel of events, then update and draw (not for a move of the mouse that
    changes nothing), opening the link a Ctrl+click or copy mode's `o` asks for, bringing the TUI's terminal to
    the front for `pane focus --raise`, ringing the user's terminal for a pane's bell or a session marked as
    having rung, putting on the clipboard what a pane's program copies (not a background task's); taking the
    mouse from the terminal or leaving it there (`[mouse] capture`), counting clicks for double- and
    triple-clicks, scrolling a pane's history on a timer while a drag selecting in it is held past its edge,
    and sending a pager the wheel as arrow keys; having git count the changes of the worktrees the sidebar
    shows, off the loop; asking the daemon what crystal's processes take, every few seconds and every second
    while the RAM view is open, off the loop; adding a project `+` asked for, made a git repository first
    once the user said so; running the user's own keys' commands: a popup over everything, a session in a
    pane or a tab, or a command in the background that says only when it fails; this crystal's release notes,
    off the loop, for `release-notes`; handing the settings view the mouse while it's open; and watching the
    config file, taking a change made by hand in at once, the panes' history following `scrollback_lines`, or
    saying why it can't be read
  - `app.rs`: the state and how keys and the mouse change it: a sidebar key looked up in the keymap and its
    command run, from the sidebar, the `:` list, after the prefix in a pane or in a pane without it for a key
    written `direct+`; a plugin's first key waiting for its second; a key in a view taken as the key the user
    gave it stands for; the user's own keys' commands made ready (a split, a tab); the sidebar's width, folded or
    not, what needs the user pinned at its top, and the projects folded down to their headings, the selection
    resting on one out of sight; the forge's lists, one asked before the one kept dropped, and the issues
    edited in the issues view laid over what was asked before the forge saved them; the worktrees being
    removed, for this TUI and for anyone else, as the daemon lists them; killing the last session in a linked
    worktree, or closing a tab with the last in some, asking whether they go too once the event loop has
    asked the daemon what's archived there, as `[worktrees] remove_emptied` says; the footer's questions
    waiting their turn; `q` asking first (`confirm_quit`); `D` opening the
    new-session panel like the selected session; `+`'s question, its `Tab` handed to the event loop to finish
    the directory; what crystal's processes take, and where the footer drew its readout of it, for a click;
    no I/O, so it's unit-tested
    - `app/commands.rs`: the layout commands carried out on the state, each on the tab holding the session it's
      about, in front or not, the files `crystal open` shows put in a view of their own, a layout applied (each
      of its tabs in place of the tab of its name or after the others, or in place of every tab), and the
      layout the TUI answers with; and carried out with no TUI open, on a state made for it from the tabs
      kept, the sessions and the flow runs, on a screen of an unseen session's size; a plugin's pane over the
      panes left to the event loop to show
  - `layout_link.rs`: the TUI's end of the layout commands: offering the daemon to take them, again at once
    after a handover or a restart, each one an event for the loop, and its answers, that it was used (a
    key, the mouse, a paste, focus gained) and when its terminal gains and loses the focus  sent back
  - `ui.rs`: the layout and drawing (the tab bar, on top or over the footer or left out, its tabs and what
    it shows at its right, the counts of what's open on the selected session's forge first, which a click
    lists, pane headers, footer and its readout of the memory crystal takes, the column each pane's
    scrollbar takes, the key pressed last while `show_keys` is on), one column on a terminal as narrow as a
    phone's (`[sidebar] phone_width`), the sidebar over the pane while it has the keyboard, and what's under
    the mouse
  - `scrollbar.rs`: a pane's scrollbar: where its thumb is for how far back the pane is, how far back a
    dragged thumb takes it, and drawing it; pure, so it's unit-tested
  - `tabs.rs`: tabs, as many as the user likes, each holding its own sessions (each session in exactly one)
    with its own selection, its tree of panes, the session the selection's pane last showed and the session
    floating over them, their order and which is in front; the sidebar shows only that tab's sessions. Kept apart from I/O; the event loop keeps
    them in the database, and tabs kept from when a tab's panes were a list are read as a tree
  - `split_tree.rs`: a tab's panes as a tree of splits, right or down at a ratio, at any depth, one pane
    following the selection: laying them out, borders, the pane beside another on screen, splitting, closing,
    swapping, resizing within each pane's least size, dragging a border, giving a pane's side of a split a share
    of its room, evening them out, folding the tree into a value and putting one together; pure, so it's
    unit-tested
  - `archived_view.rs`: the archive (`Z`): the sessions `A` archived, the latest first, one started again or
    deleted after a `y`; its state and keys, kept apart from I/O (the event loop asks the daemon), and its
    drawing
  - `menu.rs`: the menu a right click opens on a session, a worktree, a project, a tab or a pane: each item a
    sidebar key in words, chosen with a click, Enter or the key itself; its state, keys, where it's drawn and
    what's under the mouse. The App says what's in it, and carries an item out by pressing its key
  - `layouts.rs`: the layouts view (`S`): the tabs saved under a name, with what starts each of their
    terminals' programs again, and put back, and the tabs a restore replaced; its state and keys, kept apart
    from I/O (the event loop keeps them in the database and starts the sessions gone), and its drawing
  - `sidebar.rs`: the sidebar's rows: headings, a folded project's with what's in it, worktree lines with
    what git is in the middle of there, their changes not committed and how far they are from their upstream,
    Claude Code's own named by their commits and a labelled one by its label, sessions with their mark, their
    agent's model and how long ago, the line reported for one under it, terminals drawn apart from agents, the
    sessions that need the user pinned on top with the tab each is in, the pull requests, issues and backlog
    items `/` found, and the rail of marks a folded sidebar keeps; what doesn't fit left out
  - `keymap.rs`: the sidebar's commands, each with the id `[keys]` names it by, what it does and its default
    keys; keys as the config writes them and as terminals send them, folded into one form; the config's
    keys laid over the defaults, a key given to one command taken from the one that had it, and those written
    `direct+`, which work in a pane without the prefix; the prefixes and the key that hands the keyboard back;
    the modes' keys, each mode a table of its own (answering, resize mode and the views', a view's standing
    for the key every view takes for it); the user's own keys, `[[keys.command]]`, and what each runs (a
    popup, a pane, a tab, a command in the background or a plugin's action); a plugin's key, one or two
    pressed one after the other, and the keys kept from plugins; the `?` overlay's rows of sidebar keys and
    resize mode's; `crystal keys`'s list; and what a key the settings view gives a command, takes from
    another, leaves none or puts back does to `[keys]` (`rebind`, `unbind`, `reset`), checked as the config's
  - `command_list.rs`: the command list (`:`): every command, the user's own keys' commands and plugin action
    by name, with its keys, filtered as you type, the latest run first; its state and keys, kept apart from
    I/O, and its drawing
  - `status.rs`: a session's status as the TUI shows it, its mark, and what it needs of the user, which the
    sidebar's pinned rows and `u` go by
  - `theme.rs`: every color, named for what it's for, and `THEMES`, the one table of every theme by its names:
    crystal's own `dark`, `light` and `terminal`, and the well-known schemes (catppuccin, nord, …), each a
    palette of ten colors given their roles, its tints blended toward the background; the theme for a light
    or dark appearance, a scheme's other side; the user's `[colors]` over it, and none for `NO_COLOR`
  - `appearance.rs`: light or dark, which the theme follows with `[appearance] auto_switch`: the system's (a
    Mac's defaults, the desktop portal, GNOME's), asked every two seconds off the event loop, or over ssh the
    terminal's background, asked once as the TUI starts, before the input reader reads anything; not the
    terminal's mode 2031 reports, which crossterm 0.29 takes for an unfinished key and swallows the keys
    after
  - `window.rs`: the title the TUI gives its terminal: `[window] title`'s tokens filled in and checked, what
    an empty token leaves at either end taken off, and the title stack it's saved on and put back from
  - `status_bar.rs`: what the tab bar shows at its right (`[tab_bar] right`): the hostname, a clock through
    `strftime`, text, and a command's last line, run again on an interval with a timeout, its process group
    killed; worked out on threads that stop with their `Watch`, the event loop told only of a change
  - `mouse.rs`: writes mouse events the way a program in a pane, or `crystal attach`, asked for them, and the
    wheel as arrow keys for a program on the alternate screen that didn't ask (xterm's alternate scroll)
  - `help.rs`: the overlay `?` opens, a key a row: the sidebar's, the user's own, those that work in a pane
    without the prefix, resize mode's and the views' from the keymap, written as the user's `[keys]` has
    them, the rest from one table; its sections flowed into columns as tall as the terminal, two to a page,
    the pages turned with the arrows, `Tab` going to the guide; a test keeps the README's table of sidebar
    keys in step with the defaults
  - `page.rs`: a markdown page over everything, scrolled with the arrows and any other key closing it: the
    guide (`docs/guide.md`), the `?` overlay's second tab, and what's new in crystal after an update; its
    state and keys, and its drawing
  - `groups.rs`: the sidebar's order and headings: sessions by project, then worktree, agents before
    terminals, a session's task and the line reported for it under it, each flow run's steps under it, and
    linked worktrees with no sessions left at the end of their project, Claude Code's own last
  - `editing.rs`: the editing every text box shares: the text and the cursor, the motions (a character, a word
    as readline splits them, a line's ends, the text's), deleting to where one goes, and a shell's keys for
    them (`Ctrl+W`, `Alt+B`, `Ctrl+K`, …) looked up apart from what they do, for a vim mode to drive the same
    ones; pure, so it's unit-tested
  - `text_input.rs`: a one-line text box, for the questions asked on the bottom line, `/` and the filters
  - `text_area.rs`: a text box of several lines that wrap: the new-session panel's task, and the reply box
  - `reply.rs`: the reply box (`Space`): the next prompt for a session, or a background task's follow-up,
    sent without going into its pane; its state and keys, kept apart from I/O, what's typed kept until the
    daemon takes it, and its drawing
  - `launcher.rs`: the new-session panel (`n`, `w`): its state and keys, kept apart from I/O, the command
    it builds, a profile's `launch` setting its "how" row as it's chosen and whether what it starts is a
    task, what it remembers between runs, the draft it leaves when it's put away with a task in it and
    opens on again, the panel `D` opens like a session (the profile its command fits or its agent, its
    rows from its options, the rest of its arguments kept), and its drawing
  - `command_line.rs`: reads the line typed at `new session:` (the panel's `Ctrl+E`) into the command to run
  - `profiles.rs`: the profiles view (`P`): the list, the form that edits one, its keys and drawing; the
    event loop does the writing
  - `search.rs`: `/`'s matching, letters in order (a directory or a goal only whole): a session of any
    tab by its name, project, branch, command, agent, tab or flow run; a worktree with no sessions or a
    project nothing runs in; an open pull request or issue; an item to do on a backlog, its body only
    whole; the status Tab keeps to; and where what it finds goes under its project in the sidebar's rows
  - `issues.rs`: the issues view (`i`): its state and keys, kept apart from I/O, commenting on an issue and
    changing its title and text, and its drawing
  - `pull_requests.rs`: the pull requests view (`O`): its state and keys, kept apart from I/O, the open ones
    marked as drafts, conflicting and by their checks and reviews, then those merged lately, drafts left out
    while the settings hide them, reading one with its checks and conversation, its diff, commenting, asking
    the forge again (`Ctrl+R`), starting a session in its worktree, and its drawing
  - `listing.rs`: what the issues and pull requests views and the timeline share: a list filtered as you
    type, the bar kept on its item, each item read whole once, a read asked before the one kept dropped, the
    list asked for again and the heading saying so, the reading pane, and drawing them
  - `compose.rs`: writing back to the forge from those views: the comment box and the form that edits an
    issue, which keep what's typed until the forge takes it
  - `backlog_view.rs`: the backlog view `b` opens: its state and keys, kept apart from I/O, an item's line
    changed (`e`), the list kept to a tag (`t`), and its drawing, the item the bar is on under the list with
    its tags, the tasks started for it and its body
  - `pane.rs`: a viewer of a session on screen, the selected one or a split, and its screen, with copy mode
    over it while that's on; what the mouse selects on it, by characters, words or lines (`Clicks` counts
    them, for `crystal attach` too), following the history as a drag scrolls it, kept in copy mode without `copy_on_select`, and its
    scrollbar's thumb dragged
  - `copy_mode.rs`: copy mode (`v`, and `crystal attach`'s): vi's keys over a screen and its history,
    selecting, searching as the search is typed (each key from where it began, `Esc` going back there), the
    text to copy, and `o` for the link under the cursor; works on the screen, kept apart from I/O
  - `screen_widget.rs`: draws a session's screen into ratatui, for the panes and `crystal attach`, with the
    link under the mouse underlined
  - `diff.rs`: reads `git diff`'s patch into files, hunks and lines, marks the words that changed,
    and lays a file out in rows, unified or side by side; pure, so it's unit-tested
  - `diff_view.rs`: the diff view (`d`): its state, keys and drawing, files marked reviewed sinking to the
    bottom, the list filtered by a few letters of a path (`/`), and reading the diff off the event loop
  - `diff_tree.rs`: the diff view's files, or those its filter keeps, folded into a tree of directories
    (`t`), a chain of directories that hold only one another as one row; pure
  - `review.rs`: what the diff view keeps between runs, a document the event loop keeps in the database: the
    files marked reviewed, for each worktree's diff at its commit or each pull request's, with their hashes,
    and whether it lists files as a tree
  - `fuzzy.rs`: how a path matches a few typed letters, and how well
  - `finder.rs`: the file finder (`p`): its state, keys and drawing, and listing files off the event loop
  - `grep.rs`: find in files (`G`): `git grep` as you type, run off the event loop once the typing stops,
    the lines found under their files, the preview around one, and its drawing
  - `switcher.rs`: the branch switcher (`B`): the branches filtered as you type, the remotes fetched once
    they're listed (or on `Ctrl+R`) and the branches listed again, the question about the worktree's
    uncommitted changes and the commit message, kept apart from I/O, and its drawing
  - `tree_browser.rs`: the tree browser (`E`): a worktree's files as a tree, folded and opened, the filter
    that narrows it to the files that match and their directories, edited as a text box is but for the keys
    the tree and the preview take, the border dragged, its keys and drawing; kept apart from I/O, so it's
    unit-tested
  - `preview.rs`: the file finder's, the tree browser's, the handoff view's and the opened view's preview: a
    file read and highlighted off the event loop, a binary one refused by git's test, which `crystal open`
    makes too, a markdown file's page laid out for its width or its source, scrolling, and drawing them
  - `memory_view.rs`: the memory view (`m`): a project's entries by their titles, the filter, the entry's
    file opened in the editor (Enter), in the worktree it was said in while that's there, forgetting and
    promoting after a `y`, and its drawing
  - `plugins_view.rs`: the plugins view (`X`): every plugin, on or off, with installed ones' actions, panes and
    link handlers, and those the selected session's project ships under its name, which the view turns off
    but leaves the command line to turn on; its keys and drawing; the event loop does the switching, runs
    actions and opens plugins' panes where their manifests place them, through the layout commands for those
    among the panes
  - `timeline.rs`: the timeline (`a`, and `I` for the selected session's): the event log of everything or
    of one scope, a session, its task or its project, read back a page at a time, the newest first, and
    followed while it's open, `Ctrl+S` going through the selection's scopes, filtered as you type and by
    kind, what's new since the user was away marked, the line the bar is on read whole, and its drawing;
    kept apart from I/O, and events of kinds it doesn't know are listed by their name and what they say
  - `handoff_view.rs`: the handoff view (`M`): the selected session's worktree's handoff notes and the files
    its task kept, listed, the one the bar is on previewed, and opened in the editor; its state and keys,
    kept apart from I/O (the event loop looks for the notes and reads the kept files), and its drawing
  - `opened_view.rs`: the view `crystal open` brings up: the files listed by their paths from the worktree
    they were opened in, the one the bar is on previewed, a markdown file as its page with its mermaid
    diagrams drawn, `Tab` going round them, and Enter opening it in the editor; its state and keys, kept
    apart from I/O (the event loop reads each file), and its drawing
  - `needs_you.rs`: the needs-you view (`U`): everything waiting on the user, in every tab, the most urgent
    first, from the sessions and flow runs the TUI has, those that couldn't start again after a restart among
    them; answering a permission or a gate in place, or starting one of those again, the bar kept on its row
    as rows come and go, and its drawing
  - `ram_view.rs`: the RAM view (`#`): the memory each session's processes take, the biggest first, its
    share of all of it, and crystal's own; Enter goes to the session, the bar kept on its session as a new
    look reorders the rows; its state, keys and drawing, and the footer's readout
  - `away.rs`: "while you were away": when the user is gone (a quit, the terminal's focus lost for a while, or
    no key for a while where focus isn't told), what the event log gained meanwhile counted into the
    footer's line, and the latest event seen, which the event loop keeps in the database
  - `restarted.rs`: the footer's line on what a cold restart brought back and what couldn't start, worked out
    from the sessions the TUI sees waiting their turn, and those that failed, said once; pure
  - `settings_view.rs`: the settings view (`,`): the settings in tabs (general, look, sessions, mouse, tasks,
    memory), each under its heading, changed with a key, gone through with the arrows or typed in, or taken out
    of the file for its default, and how the models stand; crystal's hooks in each agent installed here, put
    in, brought up to date or taken out with a key; and the keys' tab, every key `[keys]` gives or those `/`'s
    filter finds, the key pressed next given one, one another has taken from it, or an installed plugin's
    action takes, once the user says so; a click on a tab or a row, and the wheel, where `hit` says the mouse
    is as `draw` lays the view out; kept apart from I/O, so it's unit-tested; the event loop writes the file
    (`config::apply`) or the agent's hooks (`integration`) and, while it's open, reads the settings, the hooks
    and the daemon's `EmbeddingStatus` again every half a second
- `src/daemon.rs`: the daemon: listens on the socket and owns the sessions, and emits an event wherever something
  happens to them, their tasks, flows, worktrees, memory or backlog, and for the entries of memory gone stale,
  looked for hourly and as each task closes; archives sessions, the distiller reading what one did, and starts
  them again,
  stops agents left idle past `[sessions] stop_idle_after`, has the sessions running keep the history
  `scrollback_lines` says, and keeps the list of projects sessions ran in;
  after a cold restart, puts the sessions written down back in their places and starts them again, agents
  `[sessions] restart_spacing_ms` apart on a thread of their own, those that can't start kept, failed, saying
  why, a name the user gave still theirs, and with `[sessions] restore_screens` a terminal below what it
  showed before, which it keeps in the database meanwhile; opens a background task in a terminal, in its
  place, its task carried on; hands itself over to a new crystal, and takes over from the daemon that handed
  over
  - `daemon/removal.rs`: removing a worktree, for `W` and `crystal worktree rm`: refused while a session runs
    there, git started and reaped while the removals are held, the sessions that had ended there killed,
    `worktree.removed`, and everyone who asked answered, a second ask waiting with the first; the worktrees
    being removed listed for anyone who asks, as each TUI does; one in flight handed over with its git still
    running, which the next daemon waits for, then has git try again if the worktree is still there
  - `daemon/moving.rs`: moving a session into another worktree of its project, for `crystal worktree move`:
    its program stopped once its agent's turn is over, or a background task's run (never reminded of its
    task meanwhile, which stays open), then started again there in its place, under its name and id, an
    agent in its conversation and told where it is now, a task told with a follow-up, or failed, saying
    why; the moves still to come handed over, and written down with the sessions as they'll be there, for a
    cold restart to start them there
- `src/worktree_hooks.rs`: the worktree hooks, `crystal.worktreeCreateHook` and `crystal.worktreeDeleteHook`
  in git config (adapted from docket's): run on `worktree.created` and `worktree.removed` from the daemon's
  bus, one at a time, as they're named with the main worktree and the worktree, their output in a log, stopped
  whole past a timeout, and a failure a `worktree.hook_failed` event
- `src/handover.rs`: handing the daemon over to a newly installed crystal by exec in its own process, the
  sessions carrying on: what's handed over and its `FORMAT`, the file it's written to and read from, keeping
  descriptors open across the exec, the readers it stops, the gate connections come in through, the helpers
  (hooks, the distiller) it waits for, and waiting for a child by its pid
- `src/events.rs`: what happens, as events: the one `Event` type, its kinds (a public contract plugins listen
  for: add one, never rename one) and when each happens, what each carries, how one reads in a line, the filter
  a reader gives, the scope a timeline shows (a session, a task or a project), and the made-up event `plugin
  run --event` tries hooks on; pure, so it's unit-tested
- `src/event_log.rs`: the event log, the `events` table in the database, read and pruned by age and count; and
  the daemon's `Bus`, which numbers each event (a `seq` that never goes back), writes it down and sends it to
  every subscriber: clients streaming over the socket, and the plugins' hooks
- `src/events_cli.rs`: `crystal events`: the log in a shell, filtered (by kind, session, project or task), the
  newest few with `--limit`, after a seq with `--after`, as lines or JSON, or followed, and `--since` read as a
  while back or a time on this machine's clock, which `read --since` takes too
- `src/agents.rs`: what crystal knows about particular agents: the hooks it adds to Claude Code, the events it
  listens to and what they mean (Claude Code's, which others copied adding a few, Cursor's spelled its own
  way, Letta's, and Codex's), subagents' among them, the variable that quiets the installed hooks for an agent
  crystal hooked itself, each agent's command that resumes a conversation, typed into a shell or run in place
  of the one it was started with, without its first prompt, the model a hook names, and where an agent hears
  crystal's notes: Claude Code's system prompt, or the top of another's first prompt
- `src/agent_rules.rs`: the rules agents' screens are read by: a file for each agent in `agents/` (adapted from
  herdr's), bundled, each rule a look, a priority, a region and tests; a file of the user's in the config's
  `agents/` directory in place of one, or adding an agent, read again when it changes, and a broken one said
  and passed over; reading a screen, and explaining a reading rule by rule
- `src/agent_hooks.rs`: crystal's hooks in the own settings of Cursor, Droid, Qoder, Qwen, Copilot, Devin, Kimi
  (TOML, with `toml_edit`), Letta, MastraCode, Grok and Antigravity, each in its shape and place, naming their
  events with `--event` where their input may not, for `crystal integration`: put there and taken out on the
  user's word, the user's own hooks left alone, and whether they're as this crystal would put them or out of
  date
- `src/agent_plugins.rs`: crystal's plugins for the agents that take plugins: Pi's extension, OpenCode's and
  Kilo's plugin and Hermes's, from `assets/integrations/` with crystal's path written in, each running `crystal
  hook <agent> --event`; written, out of date or not, and taken out, only files crystal wrote, and Hermes's
  switched on and off in its `config.yaml` by changing only its `plugins.enabled` list's lines; pure, so it's
  unit-tested
- `src/agent_cli.rs`: `crystal agent`: listing the agents with their rules and hooks, `explain` (a session's
  reading, from the daemon, or a saved screen's) and `rules`
- `src/catalog.rs`: the agents the new-session panel offers: their names, how each takes a first prompt and
  where it is on a command line, their options, and which are installed
- `src/codex.rs`: what crystal knows about Codex: finding a session's conversation in its rollouts, `codex
  resume`, and crystal's notes given as its developer instructions, after the ones it has already
- `src/hook.rs`: `crystal hook <agent>`: what those hooks run, any agent's, to tell the daemon, the prompt sent,
  the conversation, the agent and a subagent included (the daemon passes over an agent's that isn't the one in
  front), and to pass on its reminder to an agent ending a turn with its task open, and to Claude Code, as the
  user sends a prompt, the name the session was renamed to in crystal; with `--installed`, the hooks `crystal
  integration` installed, and with `--event`, the event a hook or plugin names itself
- `src/claude_title.rs`: keeping a session's name and Claude Code's name for its conversation (`/rename`) in
  step: the file Claude Code keeps it in beside the transcript, looked at with each check, a rename since the
  first look followed unless the user named the session; and a rename in crystal, given once as the prompt
  hook's `sessionTitle` (adapted from docket's)
- `src/integration.rs`: `crystal integration install|uninstall|status`: crystal's hooks put in Claude Code's
  `settings.json` and Codex's `hooks.json` (and `[features] hooks` in its `config.toml`, with `toml_edit`),
  beside the user's own, replacing those of a crystal at another path, taken out again alone, written in one
  go through symbolic links; pure edits on the JSON, so they're unit-tested; and the other agents' through
  `agent_hooks.rs` and `agent_plugins.rs`; how each stands, installed, out of date or not
  (`--outdated-only`), and the agents installed here, for the settings view
- `src/report.rs`: `crystal report`: any agent, or a script wrapped around one, saying what it's doing and the
  command that resumes it; checking that command, and what's typed into a shell to run it after a restart; and
  what `--line` and `--model` put on a session's row, for the sidebar alone: tidied, each with when it goes
  (`--ttl`); and a source's late reports (`--seq`) passed over, what's on the row and what the agent is doing
  numbered apart
- `src/model.rs`: the model a session's agent runs on: its command's `--model`, what its hooks say, then the
  newest switch in its conversation's transcript, Claude Code's `/model` or Codex's turn context, read a little
  at a time as it's written; and a model's name shortened for a row (adapted from docket's)
- `src/agent_screen.rs`: reading what an agent is doing off its screen, title and progress, by its rules, and
  the watch that counts a new look once it holds for two checks
- `src/front.rs`: what's in front in a session's terminal (agent, shell or program), from its foreground process:
  an agent by its program's name, the catalog's or one its rules give, or by the npm package its rules name,
  or by `CRYSTAL_AGENT` in its environment for a wrapper that hides it; and the processes in the foreground
  process group, each with its command and working directory, for `crystal process-info`
- `src/typing.rs`: typing into a session the way a person would: pastes marked, Enter on its own
- `src/session.rs`: one program in a PTY, or a task: spawn, exit status, stop, its screen (120 by 40 until a viewer
  sizes it), viewers (the user, or a program, which doesn't count as watching) and listeners, the output lately
  in an `OutputRing` and reading the screen as `crystal read` asks, the waits for output looking at it, the agent that says what it's doing
  itself while it holds the session, the pull request and the issue it's about apart from any task, the
  conversation its agent's hooks named, which counts once the agent has worked on a turn in it, an agent typed
  into its shell whose conversation a restart resumes while it's in front, its agent's subagents,
  whether its first prompt can name it, whether the user or a script gave its name, the name Claude Code gives
  its conversation, whether its agent is blocked on the user, how long its agent has sat
  idle (nobody watching or typing, its turn seen), the model its agent runs on and what was reported for its
  row, why its screen reads the way it does (`crystal agent explain`), and what has changed in it (its agent's
  activity, a task's runs, its bell rung or a copy its program made while nobody watched) for the daemon to
  tell; one written down before
  a restart, with no program, while it waits its turn to start again or once it couldn't, saying why on its
  screen; handing it over and adopting it, its PTY on a descriptor of crystal's own
- `src/output_ring.rs`: the last MiB a session's program wrote, with a mark of when at most every second, and
  what came since a time, or that it can't say, for `crystal read --since`; adapted from docket's
- `src/vt.rs`: a terminal's screen, through `alacritty_terminal`: what a program drew and its history, read as
  text a row or a line at a time, with its colors as SGR codes or not, the modes it set, its answers to the program's questions (the daemon's screen only), the output that catches a new viewer
  up (its hyperlinks included), the cells to draw, the input modes `crystal attach` asks your terminal for, and,
  for a viewer, copy mode's cursor, selection (of characters, words, lines or a block) and search, which are
  Alacritty's vi mode, where a search being typed began (`vt::Spot`, counted from the top of the history, so
  output meanwhile doesn't move it), to search from at each key and go back to, and the link on a cell: a hyperlink a program wrote (OSC 8), or a URL in the text across
  the rows it wrapped onto, as `vt::Link`; the
  times the program rang the bell; the text it last asked to copy (OSC 52), a read of the clipboard never
  answered; the progress a program reports (OSC 9;4), picked out of its output, which
  alacritty_terminal passes over; how much history a screen keeps, changed on a running one; a screen saved
  for a handover, both its screens and the history, and restored; and the main screen and its history kept to
  show again, without what its program set, above the program a cold restart starts in its place. The only
  module that uses
  `alacritty_terminal`
- `src/links.rs`: opening a link a pane shows: `open` or `xdg-open`, or over ssh (or with neither) the link put
  on the user's clipboard instead
- `src/clipboard.rs`: putting text on the user's clipboard: `pbcopy`, `wl-copy`, `xclip` or `xsel` on their own
  machine, or OSC 52 to their terminal over ssh or when none of those works
- `src/task.rs`: tasks: Claude Code run without a terminal (`claude -p`): one process taking the prompt and each
  follow-up over its standard input, the permissions it asks for and their answers, interrupts, each run's cost
  and budget, the permission mode and rules `[tasks]` gives each `claude`, letting an idle one go or one whose
  run failed, how full its conversation is (its context meter), its screen drawn again from Claude Code's
  transcript after a cold restart, the arguments it keeps opened in a terminal, and handing its `claude` over,
  pipes and all
- `src/claude_stream.rs`: Claude Code's stream-json protocol, as crystal speaks it with a background task's
  `claude -p`: prompts in, the control messages that carry a permission prompt out and its answer back, an
  interrupt, and the rule "always" keeps; adapted from docket's `docket-claude`
- `src/transcript.rs`: reading `claude -p`'s stream-json events, and drawing them as a task's transcript,
  Claude's answers as markdown pages, everything in it that isn't crystal's own made printable first; how
  many tokens each message took and each model takes; and the transcript Claude Code keeps of a
  conversation, its prompts too, read the same way
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
- `src/mermaid_cli.rs`: `crystal mermaid`: a diagram, or a markdown file's, drawn on standard output, in ASCII
  with `--ascii` or `mermaid_ascii`; or with `--open`, put on a page in the state directory, named by its hash,
  for mermaid to draw in the browser
- `src/spending.rs`: what background tasks have spent today, kept in the database by the day: the TUI footer's
  `$X today`, and what `daily_budget_usd` is held against
- `src/protocol.rs`: requests and responses, one JSON line each, and the frames an attached client sends; their
  JSON Schema is `docs/crystal-api.schema.json` (see `api/schema.rs`)
- `src/socket.rs`: where the socket lives: a server's, named after it in crystal's socket directory, or one
  given by its path; which a command is for (`-S`, `--server`, `CRYSTAL_SOCKET`, then `CRYSTAL_SERVER`); and
  which server a socket is, however it's spelled and whoever starts its daemon, so the same socket always gets
  the same state
- `src/state.rs`: where the daemon's state is: a server's directory in the state dir (the default server's is
  the state dir itself), or beside a socket given by its path; the database, the directory of the files each
  task kept, and the files kept before the database (the sessions, the flow runs, each project's directory); a
  running session as it's written down to start it again, and the worktree it was on its way into
- `src/server_cli.rs`: `crystal server`: every server with whether it's running and how many sessions it has,
  stopping one, and deleting a stopped one's state
- `src/db.rs`: the SQLite database the daemon and the TUI keep their state in (WAL, `synchronous=NORMAL`,
  migrations by `user_version`, as docket does): the sessions to start again, the archived sessions, flow runs,
  the projects on crystal's list, each project's backlog (each item's line, body and tags) and closed tasks, the files tasks kept, the tasks waiting to start and the last task number, what each
  task carried beside its goal (its acceptance criteria, pull request and issue), what background
  tasks spent each day, the event log, read from a point on or a page of a timeline's scope at a time back
  from its end, what each terminal written down showed (`[sessions] restore_screens`), and the TUI's
  tabs, layouts, the new-session panel's memory, the diff view's reviewed marks, the projects folded in the
  sidebar and the latest event the user had seen, each a JSON document; and bringing in the JSON files from before, a project's the first time it's
  asked for. Settings stay in the config file and memory in `memory.db`
- `src/project.rs`: the project a directory is in: its git main worktree, or the directory itself outside git
- `src/project_commands.rs`: a project's `run` and `open` commands, from the config's `[[project]]`, or else the
  worktree's `.crystal/project.toml`, or else the main worktree's; the command a run session runs and its name
  (`run-app`), which session is a worktree's run, and running `open` in the background
- `src/project_cli.rs`: `crystal project`: the projects crystal knows, adding and taking one off the list, and
  running or opening the project in a worktree
- `src/tasks.rs`: tasks, sessions started with something to do: the paragraph an agent is told about
  `crystal done`, the reminder for one that ends a turn with its task open, a task's acceptance criteria and
  where they go in its first prompt, the goal of a task on a pull request or an issue and what its agent is
  told of them, a session with no task too, reading a project's closed tasks from the file they were kept in before the database,
  numbering tasks (`t12`) and showing a task waiting to start, the most a prompt crystal puts together may be,
  and `enabled`, the one gate everything tasks add goes through
- `src/handoff.rs`: the handoff file, `.crystal/handoff.md` at the top of a git worktree: a note's heading and
  tidying, adding a section and letting the oldest go past the cap, the `.gitignore` beside it unless the config
  keeps a project's notes in git, the end of the file read for an agent starting there and the rule it's told,
  and `enabled`, the one gate everything it adds goes through; the daemon is its only writer
- `src/artifacts.rs`: the files a task keeps as it closes (`crystal done --artifact`): checking each is a small
  file in the task's worktree, copying them into the task's directory in the state directory under names of
  their own, and keeping the worktree's handoff file there too
- `src/backlog.rs`: a project's backlog, numbered items kept in the database by the daemon alone, each a line
  and a body, changed, imported (those whose line is there already passed over) and filtered by tag; what a
  task started for one is asked; its markdown export and reading a markdown list of checkboxes back, and
  `enabled`, the one gate everything the backlog adds goes through
- `src/worktree_cli.rs`: `crystal worktree list`, `create`, `open` (finding the worktree; `main.rs` starts the
  session), `label` and `move`, which makes the worktree a session moves into when there's none; and `crystal
  kill`, which asks at the terminal whether the worktree it empties goes too, says how to remove it with
  nobody there to ask, or does as `--remove-worktree`, `--keep-worktree` or the settings say
- `src/emptied.rs`: the linked worktrees killing sessions leaves with nothing in them, the archived sessions
  that ran in each, the question whether they go too, and what `[worktrees] remove_emptied` makes of it,
  for the TUI and `crystal kill`; pure, so it's unit-tested
- `src/work.rs`: `crystal done` (with `--artifact`), `handoff`, `tasks` and its commands (`new`, `start`, `show`,
  `cancel`, `log`, `terminal`), reading what a new task carries (`--accept`, and `--pr` and `--issue` from the
  forge, a pull request's worktree found or made), and `backlog` with its commands, an item's card with the
  tasks started for it, and starting a task for one with a profile, in a worktree, on a pull request or in the
  background
- `src/config.rs`: the settings in `~/.config/crystal/config.toml`, read and checked: a theme by any of its
  names, the colors `[colors]` takes, what the mouse does (`[mouse]`) and whether programs' copies go on the
  clipboard (`[clipboard]`); what background tasks may spend and do unasked (`[tasks]`); the shell a new terminal runs
  (`[terminal]`, `-l` for a login shell) and where the TUI starts one, the window's title, the tab bar and
  the appearance; where new worktrees start and go, and what's done with one emptied (`[worktrees]`); and the
  settings view's edits to the
  file (`apply`), several at once, a line set or taken out for its default, the rest of the file as the user
  wrote it, written only if what they come to is read
- `src/memory.rs`: what a project's sessions learned: the SQLite store in the state directory with its FTS5
  index (bm25, prefix and porter-stemmed words), each entry's vector and search by meaning merged with it by
  reciprocal rank fusion, then the reranker's read of the best (nothing when none answers), its migrations,
  the same said again seen again, forgotten entries the distiller can't add back, kept as they were to list,
  bringing in a project's JSON file from before, anchors (each file's SHA-256 when
  an entry was said) and whether an entry holds, fresh, drifting or stale, the entries gone stale the daemon
  hasn't told of, search (of a kind, about some files or directories, the stale left out or not), an entry's
  title, its first line, or one of its own, the paragraph every agent is
  shown at launch (entries about what its worktree changed first, in docket's 800 bytes, tasks' outcomes kept
  by an earlier crystal left out of it), promoting into
  CLAUDE.md, the markdown export, and `enabled`, the one gate everything memory adds goes through
- `src/distill.rs`: the distiller: after a task closes, or a session is archived that wasn't read as its task
  closed, one tool-less `claude -p` (Haiku by default, `[memory]`
  in the config) over the end of its transcript, told what the memory has already; its answer checked
  against the checkout before it's kept
- `src/mcp.rs`: `crystal mcp`: an MCP server over stdio with `memory_search` and `memory_show`, which every
  Claude Code session crystal starts, in a terminal or a task in the background, is given with `--mcp-config`
  and its tools allowed
- `src/embed.rs`: search by meaning: jina-embeddings-v5-text-small (its retrieval LoRA adapter folded into
  its weights as it loads) and jina-reranker-v3, both run through Candle on a Mac's GPU (Metal, bfloat16) or
  the CPU, one call at a time (Candle on Metal answers wrong to threads running models at once); `Embed` (the
  models, or a stand-in in tests), with the scores from which the reranker counts an entry; downloading both at pinned revisions with their SHA-256s checked, by the daemon as it starts unless
  `CRYSTAL_NO_MODEL_DOWNLOAD` is set; and the one copy each process loads when `[memory] embeddings` is on;
  memory.rs keeps the vectors and merges the rankings
- `src/qwen3.rs`: Qwen3, the transformer both models are, adapted from candle-transformers' to read texts whole:
  no cache, a batch padded at its end, causal attention through Candle's fused kernel on Metal (past 8 tokens,
  below which Candle's kernel isn't causal)
- `src/resources.rs`: the memory crystal's processes take, for the RAM view: every process on the machine read
  once (`/proc` on Linux, `ps` elsewhere), each session's program summed with every process under it, the
  daemon's own and the asking client's, and the machine's memory; the counting pure, so it's unit-tested
- `src/rerank.rs`: the reranker: every passage and the query in one prompt, each marked at its end, the
  projector over the model's state at the marks, and each passage's cosine with the query
- `src/secrets.rs`: taking credentials out of text before memory keeps it or the distiller reads it
- `src/memory_cli.rs`: `crystal remember` and `crystal memory`, `add`, `list` (by kind, or what was
  forgotten), `search` (by kind, files, the stale too, and how many), `show`, `export` and `distill` included,
  and an entry in full as `show` and the `memory_show` tool print it
- `src/profile.rs`: agent profiles: what one runs, its prompt and postfix around the task or alone with no
  task (`skip_task`), how it's meant to start (`launch`: a session, a task or a background task), checking it,
  and saving or removing one in the config file with `toml_edit`, so the user's comments and layout stay;
  `enabled` is the one switch for the feature
- `src/flows.rs`: flows, chains of tasks on one goal: the `[[flow]]` tables in the config file and in a project's
  `.crystal/flows.toml`, which take the place of the config's of the same name, checking them, the profile a
  step runs with, its own agent, model, effort and mode over it, and whether it runs in the background, where
  a step is placed, filling in a step's prompt and cutting it to fit, a goal's slug, the example `crystal flow
  example` prints, and `enabled`, the one gate everything flows add goes through
- `src/flow_run.rs`: a flow run and how it changes as its steps end, the user answers its gates (within their
  rounds) and cancels it; where each step runs, what it's asked, its acceptance criteria under it, and whether
  it runs in the background or in a terminal (an agent other than Claude Code, or Claude Code with `background
  = false`); kept apart from I/O, so it's unit-tested; the daemon starts the steps and writes the runs down in
  the database
- `src/flow_cli.rs`: `crystal flow` and its commands, `cancel` and `defs` among them
- `src/notify.rs`: telling the user when a session needs them, once it has for `[notifications] after_secs`
  and, with `unfocused_only`, while no TUI's terminal has the focus (where the user is, as the TUIs say, kept
  for the daemon): desktop notifications a click on takes them to the session, `notify-send`'s text escaped
  for a server that says it reads markup (asked once, with `gdbus` or `dbus-send`), or their own command;
  `crystal notify`'s too; and the sound at the same moments
- `src/sound.rs`: the sounds (`assets/sounds/`, herdr's): which plays for an agent asking or done, the user's own
  files and the agents they're off for (`[sound]`), and playing one with the system's player, off the thread
  that asked, stopped if it hangs
- `src/plugins.rs`: plugins: the registry of crystal's own, `enabled`, the gate every one of them goes through
  (each module's `enabled` asks it), finding installed plugins and those a project ships in its
  `.crystal/plugins/` (each known by its `Id`, its name and the project's), why one can't run here (it doesn't
  fit, or its build failed) or be switched on, switching one in the config's `[plugins]`, or a project's in
  its `[[project]]` table, with `toml_edit`, the plugins running, the context and environment their commands
  run with, each plugin's settings directory (shared, beside the config) and state directory (each server's),
  a project's kept apart under the project's name and hash, their logs, pausing one that fails, the plugin
  a link goes to, and starting an action in the background, its output in the plugin's log, for the TUI and
  `crystal attach`
- `src/plugin_manifest.rs`: a plugin's `plugin.toml` (build and startup commands, actions, events, panes and where
  each is placed, link handlers, `min_crystal_version`, `platforms` and `timeout_secs`), read and checked,
  whether it fits this crystal and this system, and how event patterns match
- `src/plugin_hooks.rs`: the daemon's side of plugins' `[[events]]` and `[[startup]]`: a subscriber of the bus,
  each plugin's hooks run one at a time on a thread of its own, with its timeout, a log, and a pause (and a
  `plugin.paused` event) after failures in a row, a project's hearing only its project's events, each given the
  event on its standard input, in `CRYSTAL_EVENT_JSON` and in words in `CRYSTAL_EVENT_TEXT`; startup commands
  queued the same way by `Hooks::start_up`, which the daemon calls once its sessions are back (and a daemon
  taking over must call too); and running a hook here, for `plugin run --event`
- `src/plugin_cli.rs`: `crystal plugin`: listing, the project's own too, and the events hooks hear, switching
  (a project's shown, asked about and built first), running an action, or trying hooks on an event made up,
  given as JSON or both, opening a pane where it's placed (a session started, then shown over the TUI's panes,
  or laid out with the layout commands), installing and building (`[[build]]`, its output in the plugin's log,
  a failure noted to keep it off), making and removing
- `examples/plugins/`: example plugins, each a `plugin.toml` and its scripts, which a unit test reads and
  `tests/cli.rs` installs and runs
- `src/env.rs`: the environment a session's program starts with
- `src/git.rs`: a directory's project, worktree and branch, a project's linked worktrees, and making and
  removing worktrees, a new branch from `origin`'s default (fetched, with a timeout), the settings' base or
  `--base`, beside the project in `<repo>.worktrees`, in the settings' directory or at `--path`, a pull
  request's with its commits fetched from `origin`, a linked worktree's label, kept in its own git directory,
  a path git config names, the patches the diff reads,
  the files a worktree changed since its branch left the default one, what git is in the middle of in a
  worktree (a merge, a rebase, a cherry-pick or a revert) and the branch a rebase keeps though HEAD is
  detached, the subject of the commit each of Claude Code's own worktrees (`.claude/worktrees`) is at, what a
  worktree's line in the sidebar counts (its changes not committed, files and lines, and how far its branch is
  ahead of its upstream and behind it, read without taking the index's lock), `git grep` stopped once it's
  no longer wanted, and making a directory a repository for a new project (runs `git`)
  - `git/branches.rs`: a worktree's branches, local and remote, fetching its remotes in a session of their own
    with a timeout, its uncommitted changes, and switching it to another branch or a new one, the changes
    stashed, brought along, committed or thrown away, and put back when git won't switch
- `src/names.rs`: made-up names for new worktrees' branches, like `brave-otter`, and a session's name from its
  first prompt, like `fix-login-redirect`, or from the name Claude Code gave its conversation
- `src/forge.rs`: pull requests (open, and merged lately) and issues from the forge a project's remote is on,
  GitHub or GitLab, told apart by its host and the hosts `gh` and `glab` know: the types both read into,
  `Repo`'s calls, one pull request or issue among them read on its own, and running the CLI with a timeout; tests use a fake `gh` and `glab`, never the real ones.
  Was `github.rs`
  - `forge/github.rs`: each call as a `gh` command, and reading its `--json`
  - `forge/gitlab.rs`: each call as a `glab` command, and reading its JSON, a merge request as a pull request
- `src/shell.rs`: quoting arguments and writing paths with `~`, the way a shell reads them, and finishing a
  directory's name as a shell's `Tab` does
- `src/output.rs`: what a command prints on standard output: `out!` and `outln!`, in place of `print!` and
  `println!`, and `Closed`, its reader gone, which stops the command and has `main` exit 0; SIGPIPE is left
  ignored, as the daemon, the TUI, `crystal attach`, `crystal mcp` and the hooks need it, and as a command's own
  writes to the daemon's socket and its programs' pipes do
- `src/printable.rs`: text crystal didn't write made fit for the user's terminal: control characters, the escape
  sequences they start and the explicit bidi controls taken out, on one line or keeping its lines; and the TUI's
  frame scrubbed of them last, since ratatui hands a zero-width one on to the terminal. A background task's
  transcript, the window's title, notifications, session names, reports, the backlog, memory and what the CLI
  prints for people go through it
- `tests/cli.rs`: end-to-end tests that drive the real binary against a private daemon (and read its database
  beside its socket to see what it wrote down), every command they run without the `CRYSTAL_*` variables of a
  crystal session they may be run in (`outside_crystal`), with a config of
  their own that turns notifications, sounds, the memory plugin, naming sessions from their prompts and panes'
  scrollbars off (a test of memory, naming or the scrollbar turns it back on, and `CRYSTAL_NO_SOUND` keeps
  sounds off even then; memory's models are kept out by a cache that can't hold them and
  `CRYSTAL_NO_MODEL_DOWNLOAD`), plugins
  of their own in its plugins directory, and a Claude Code config directory of their own (`CLAUDE_CONFIG_DIR`),
  since a daemon brings the skill there up to date as it starts; `crystal integration` and every TUI run with a
  home of their own (the settings view puts agents' hooks in), and none of the variables that move agents'
  settings elsewhere; a test that opens the new-session panel pins `PATH` to its fake
  agents, so no real agent is found or run, and a background task's `claude` is a fake that speaks stream-json,
  asks for permissions and takes interrupts. vt100 stands in for the user's own terminal: a second emulator,
  apart from crystal's. A test that copies, or opens a link, runs the TUI as over ssh (`SSH_TTY` set), so it
  asks the terminal with OSC 52 and never touches the machine's clipboard or opens a browser. A test of servers
  by name runs crystal without `--socket`, in a runtime dir and a state dir of its own, with `CRYSTAL_SOCKET`
  and `CRYSTAL_SERVER` taken out of its environment, so it never reaches the user's own daemon. A TUI looks for
  crystal's releases on a port nothing listens on (`CRYSTAL_RELEASES`), and a test of updating serves fake
  releases from a web server of its own on 127.0.0.1, its crystal a script that logs what it's asked, and
  updates a copy of the binary, never the one under test
